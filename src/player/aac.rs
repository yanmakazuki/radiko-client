//! 状態を保持する容量制限付きADTSデコーダー。PCMは明示的に要求された場合だけ保持する。
use rusty_aac::{config::StreamConfig, decode::Decoder, parse_adts};
use std::{
    fmt,
    time::{Duration, Instant},
};

pub const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACKETS: usize = 4096;
const MAX_PCM_SAMPLES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AacError {
    Cancelled,
    TooLarge,
    InvalidMetadata,
    InvalidHeader,
    TruncatedFrame,
    Empty,
    TooManyPackets,
    UnsupportedTransport,
    UnsupportedCodec,
    DecodeFailed,
    FormatChanged,
    InvalidPcm,
    DeclaredSbrMissing,
    SbrNotReconstructed,
}
impl fmt::Display for AacError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "AACデコードを取り消しました",
            Self::TooLarge => "AACセグメントのサイズ上限を超えました",
            Self::InvalidMetadata => "AACに付加されたID3の形式が不正です",
            Self::InvalidHeader => "ADTSヘッダーが不正です",
            Self::TruncatedFrame => "ADTSフレームが途中で切れています",
            Self::Empty => "AAC音声フレームがありません",
            Self::TooManyPackets => "AACセグメントのパケット数上限を超えました",
            Self::UnsupportedTransport => "CRC付きまたは複数raw blockのADTSは検証対象外です",
            Self::UnsupportedCodec => "AACの構成がデコーダーで未対応です",
            Self::DecodeFailed => "AACパケットのデコードに失敗しました",
            Self::FormatChanged => "AACの構成が変わりました。新しいデコーダーで再開してください",
            Self::InvalidPcm => "デコードしたPCMの形式またはサンプル値が不正です",
            Self::DeclaredSbrMissing => {
                "HLSはHE-AACを宣言していますが、このサンプルではSBR再構成を確認できません"
            }
            Self::SbrNotReconstructed => "SBRデータを検出しましたが出力がコアレートのままです",
        })
    }
}
impl std::error::Error for AacError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacSummary {
    pub core_sample_rate: u32,
    pub core_channels: u16,
    pub sample_rate: u32,
    pub channels: u16,
    pub packets: usize,
    pub frames: usize,
    pub sbr_detected: bool,
    /// Output expansion is observable, but does not independently prove PS signalling.
    pub stereo_expansion: bool,
    pub media_duration: Duration,
    pub decode_duration: Duration,
    pub nonzero_samples: usize,
}

/// Validated PCM for one short segment. Debug deliberately omits audio samples.
pub struct DecodedPcm {
    pub summary: AacSummary,
    pub samples: Vec<f32>,
}
impl fmt::Debug for DecodedPcm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedPcm")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Format {
    object_type: u8,
    sample_rate: u32,
    channels: u16,
}
struct Frame<'a> {
    format: Format,
    payload: &'a [u8],
}

fn frames<'a>(
    bytes: &'a [u8],
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<Vec<Frame<'a>>, AacError> {
    if cancelled() {
        return Err(AacError::Cancelled);
    }
    if bytes.len() > MAX_SEGMENT_BYTES {
        return Err(AacError::TooLarge);
    }
    let offset = super::audio_offset(bytes).ok_or(AacError::InvalidMetadata)?;
    let mut remaining = &bytes[offset..];
    let mut frames = Vec::new();
    while !remaining.is_empty() {
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        if frames.len() >= MAX_PACKETS {
            return Err(AacError::TooManyPackets);
        }
        if remaining.len() < 7 {
            return Err(AacError::TruncatedFrame);
        }
        let header = parse_adts(remaining).map_err(|_| AacError::InvalidHeader)?;
        if header.frame_length <= header.header_len {
            return Err(AacError::InvalidHeader);
        }
        if header.frame_length > remaining.len() {
            return Err(AacError::TruncatedFrame);
        }
        // Keep unsupported transport arrangements explicit rather than discarding CRCs.
        if header.header_len != 7 || remaining[6] & 3 != 0 {
            return Err(AacError::UnsupportedTransport);
        }
        let format = Format {
            object_type: header.object_type,
            sample_rate: header.sample_rate,
            channels: header.channels,
        };
        if format.channels == 0 || format.channels > 7 || format.object_type != 2 {
            return Err(AacError::UnsupportedCodec);
        }
        frames.push(Frame {
            format,
            payload: &remaining[header.header_len..header.frame_length],
        });
        remaining = &remaining[header.frame_length..];
    }
    if frames.is_empty() {
        return Err(AacError::Empty);
    }
    Ok(frames)
}

/// Keeps overlap, SBR and PS decoder state across adjacent segments.
/// Create a fresh instance for a seek, station switch or discontinuity.
/// A failed segment discards decoder state and must not be treated as playable PCM.
#[derive(Default)]
pub struct AdtsDecoder {
    decoder: Option<Decoder>,
    format: Option<Format>,
}
impl AdtsDecoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn decode_segment(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
    ) -> Result<AacSummary, AacError> {
        let result = self.decode(bytes, declared_codec, None, &mut || false);
        if result.is_err() {
            self.decoder = None;
            self.format = None;
        }
        result
    }
    /// Retains at most 4 MiB; no PCM escapes if any packet or final check fails.
    pub fn decode_segment_pcm(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
    ) -> Result<DecodedPcm, AacError> {
        self.decode_segment_pcm_controlled(bytes, declared_codec, || false)
    }
    /// Cancellation is checked between ADTS headers and before/after each packet.
    /// One decoder call is not interrupted; cancelled state and partial PCM are discarded.
    pub fn decode_segment_pcm_controlled(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<DecodedPcm, AacError> {
        if cancelled() {
            self.decoder = None;
            self.format = None;
            return Err(AacError::Cancelled);
        }
        let mut samples = Vec::with_capacity(MAX_PCM_SAMPLES);
        match self.decode(bytes, declared_codec, Some(&mut samples), &mut cancelled) {
            Ok(summary) => Ok(DecodedPcm { summary, samples }),
            Err(error) => {
                self.decoder = None;
                self.format = None;
                Err(error)
            }
        }
    }
    fn decode(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
        mut pcm: Option<&mut Vec<f32>>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<AacSummary, AacError> {
        let started = Instant::now();
        let frames = frames(bytes, cancelled)?;
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        let format = frames[0].format;
        if frames.iter().any(|f| f.format != format)
            || self.format.is_some_and(|previous| previous != format)
        {
            return Err(AacError::FormatChanged);
        }
        if self.decoder.is_none() {
            let config = StreamConfig::lc(
                format.object_type,
                format.sample_rate,
                format.channels as u8,
            );
            self.decoder = Some(Decoder::with_stream_config(config).map_err(map_error)?);
            self.format = Some(format);
        }
        let decoder = self.decoder.as_mut().ok_or(AacError::DecodeFailed)?;
        let mut summary = AacSummary {
            core_sample_rate: format.sample_rate,
            core_channels: rusty_aac::config::channels_for_config(format.channels as u8) as u16,
            sample_rate: 0,
            channels: 0,
            packets: 0,
            frames: 0,
            sbr_detected: false,
            stereo_expansion: false,
            media_duration: Duration::ZERO,
            decode_duration: Duration::ZERO,
            nonzero_samples: 0,
        };
        for frame in frames {
            if cancelled() {
                return Err(AacError::Cancelled);
            }
            let audio = decoder.decode(frame.payload, None).map_err(map_error)?;
            if cancelled() {
                return Err(AacError::Cancelled);
            }
            if audio.sample_rate == 0
                || audio.sample_rate > 192_000
                || audio.channels == 0
                || audio.channels > 8
                || audio.samples.is_empty()
                || audio.samples.len() % usize::from(audio.channels) != 0
                || audio.samples.iter().any(|s| !s.is_finite())
            {
                return Err(AacError::InvalidPcm);
            }
            let output_frames = audio.frames();
            if output_frames > 2048 {
                return Err(AacError::InvalidPcm);
            }
            if summary.packets > 0
                && (summary.sample_rate != audio.sample_rate || summary.channels != audio.channels)
            {
                return Err(AacError::FormatChanged);
            }
            summary.sample_rate = audio.sample_rate;
            summary.channels = audio.channels;
            summary.packets += 1;
            summary.frames += output_frames;
            summary.nonzero_samples += audio.samples.iter().filter(|s| s.abs() > 0.000_001).count();
            summary.sbr_detected |= decoder.saw_sbr();
            if let Some(samples) = pcm.as_mut() {
                if samples.len() + audio.samples.len() > MAX_PCM_SAMPLES {
                    return Err(AacError::TooLarge);
                }
                samples.extend_from_slice(&audio.samples);
            }
            // Normal decode probes discard PCM; explicit output retains one bounded segment.
        }
        summary.decode_duration = started.elapsed();
        summary.stereo_expansion = summary.core_channels == 1 && summary.channels == 2;
        summary.media_duration =
            Duration::from_secs_f64(summary.frames as f64 / f64::from(summary.sample_rate));
        let source_seconds = summary.packets as f64 * 1024.0 / f64::from(summary.core_sample_rate);
        if (summary.media_duration.as_secs_f64() - source_seconds).abs()
            > 1.0 / f64::from(summary.sample_rate)
        {
            return Err(AacError::InvalidPcm);
        }
        if summary.sbr_detected && summary.sample_rate <= summary.core_sample_rate {
            return Err(AacError::SbrNotReconstructed);
        }
        let declared_he = declared_codec.is_some_and(|codecs| {
            codecs
                .split(',')
                .any(|c| matches!(c.trim(), "mp4a.40.5" | "mp4a.40.29"))
        });
        if declared_he && !summary.sbr_detected {
            return Err(AacError::DeclaredSbrMissing);
        }
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        Ok(summary)
    }
}
fn map_error(error: rusty_aac::Error) -> AacError {
    match error {
        rusty_aac::Error::Unsupported(_) | rusty_aac::Error::Unimplemented(_) => {
            AacError::UnsupportedCodec
        }
        _ => AacError::DecodeFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_aac::{AacEncoder, AacEncoderConfig, AdtsHeader, write_adts_header};

    fn generated_adts(rate: u32, mpeg2: bool) -> Vec<Vec<u8>> {
        let mut encoder = AacEncoder::new(AacEncoderConfig::default());
        let pcm: Vec<f32> = (0..3072)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.2)
            .collect();
        encoder.push_pcm(&pcm, 1, rate).unwrap();
        encoder.finish();
        let mut result = Vec::new();
        while let Ok(packet) = encoder.next_packet() {
            let mut frame = write_adts_header(&AdtsHeader {
                object_type: 2,
                sample_rate: rate,
                channels: 1,
                frame_length: 7 + packet.data.len(),
                header_len: 7,
            });
            if mpeg2 {
                frame[1] |= 8;
            }
            frame.extend_from_slice(&packet.data);
            result.push(frame);
        }
        assert!(result.len() >= 3);
        result
    }
    #[test]
    fn cancellation_at_every_checkpoint_discards_partial_pcm_and_resets_decoder_state() {
        let bytes = generated_adts(24000, false).concat();
        let mut checkpoints = 0;
        let reference = AdtsDecoder::new()
            .decode_segment_pcm_controlled(&bytes, None, || {
                checkpoints += 1;
                false
            })
            .unwrap();
        assert!(checkpoints > 3);
        for stop_at in 1..=checkpoints {
            let mut decoder = AdtsDecoder::new();
            // Include previously valid overlap state: cancellation must discard it too.
            decoder.decode_segment(&bytes, None).unwrap();
            let mut seen = 0;
            let result = decoder.decode_segment_pcm_controlled(&bytes, None, || {
                seen += 1;
                seen >= stop_at
            });
            assert_eq!(result.unwrap_err(), AacError::Cancelled);
            assert!(decoder.decoder.is_none());
            assert!(decoder.format.is_none());
            let recovered = decoder.decode_segment_pcm(&bytes, None).unwrap();
            assert_eq!(recovered.samples, reference.samples);
            assert_eq!(recovered.summary.frames, reference.summary.frames);
        }
        let mut decoder = AdtsDecoder::new();
        assert_eq!(
            decoder
                .decode_segment_pcm_controlled(b"invalid", None, || true)
                .unwrap_err(),
            AacError::Cancelled
        );
    }
    #[test]
    fn retained_pcm_is_complete_validated_and_bounded_before_output() {
        let bytes = generated_adts(24000, false).concat();
        let mut decoder = AdtsDecoder::new();
        let pcm = decoder.decode_segment_pcm(&bytes, None).unwrap();
        assert_eq!(
            pcm.samples.len(),
            pcm.summary.frames * usize::from(pcm.summary.channels)
        );
        assert!(pcm.samples.capacity() <= MAX_PCM_SAMPLES);
        assert!(pcm.samples.iter().all(|s| s.is_finite()));
        assert!(pcm.samples.iter().any(|s| s.abs() > 0.000001));
        assert!(!format!("{pcm:?}").contains("samples: ["));
        assert_eq!(
            decoder
                .decode_segment_pcm(&bytes, Some("mp4a.40.5"))
                .unwrap_err(),
            AacError::DeclaredSbrMissing
        );
        assert!(decoder.decode_segment_pcm(&bytes, None).is_ok());
        let packet = generated_adts(24000, false).remove(0);
        let too_much = packet.repeat(MAX_PCM_SAMPLES / 1024 + 1);
        assert_eq!(
            decoder.decode_segment_pcm(&too_much, None).unwrap_err(),
            AacError::TooLarge
        );
        assert!(decoder.decode_segment_pcm(&bytes, None).is_ok());
    }
    #[test]
    fn generated_mpeg2_adts_with_id3_decodes_every_packet_at_the_correct_rate() {
        let packets = generated_adts(24000, true);
        let mut bytes = b"ID3\x04\0\0\0\0\0\0".to_vec();
        for packet in &packets {
            bytes.extend_from_slice(packet);
        }
        let summary = AdtsDecoder::new()
            .decode_segment(&bytes, Some("mp4a.40.2"))
            .unwrap();
        assert_eq!(summary.packets, packets.len());
        assert_eq!(summary.frames, packets.len() * 1024);
        assert!(
            (summary.media_duration.as_secs_f64() - summary.frames as f64 / 24000.0).abs()
                < 0.000_001
        );
        assert_eq!(
            (
                summary.core_sample_rate,
                summary.sample_rate,
                summary.channels
            ),
            (24000, 24000, 1)
        );
        assert!(!summary.sbr_detected);
        assert!(!summary.stereo_expansion);
        assert!(summary.nonzero_samples > 0);
    }
    #[test]
    fn decoder_keeps_state_between_segments_and_resets_after_a_format_change() {
        let packets = generated_adts(24000, false);
        let mut decoder = AdtsDecoder::new();
        let a = decoder.decode_segment(&packets[0], None).unwrap();
        let b = decoder
            .decode_segment(&packets[1..].concat(), None)
            .unwrap();
        assert_eq!(a.frames + b.frames, packets.len() * 1024);
        let changed = generated_adts(48000, false).concat();
        assert_eq!(
            decoder.decode_segment(&changed, None).unwrap_err(),
            AacError::FormatChanged
        );
        let restarted = decoder.decode_segment(&changed, None).unwrap();
        assert_eq!(restarted.sample_rate, 48000);
    }
    #[test]
    fn he_aac_declaration_cannot_turn_a_core_only_stream_into_a_success() {
        let bytes = generated_adts(24000, false).concat();
        for declaration in ["mp4a.40.5", "mp4a.40.29"] {
            assert_eq!(
                AdtsDecoder::new()
                    .decode_segment(&bytes, Some(declaration))
                    .unwrap_err(),
                AacError::DeclaredSbrMissing
            );
        }
    }
    #[test]
    fn truncation_and_transport_errors_never_return_partial_success() {
        let packet = generated_adts(24000, false).remove(0);
        for end in [0, 1, 6, packet.len() - 1] {
            assert!(
                AdtsDecoder::new()
                    .decode_segment(&packet[..end], None)
                    .is_err()
            );
        }
        let mut bytes = packet.clone();
        bytes.extend_from_slice(&packet[..packet.len() - 1]);
        assert_eq!(
            AdtsDecoder::new().decode_segment(&bytes, None).unwrap_err(),
            AacError::TruncatedFrame
        );
        let mut multi = packet.clone();
        multi[6] |= 1;
        assert_eq!(
            AdtsDecoder::new().decode_segment(&multi, None).unwrap_err(),
            AacError::UnsupportedTransport
        );
        let mut crc = packet;
        crc[1] &= !1;
        assert_eq!(
            AdtsDecoder::new().decode_segment(&crc, None).unwrap_err(),
            AacError::UnsupportedTransport
        );
    }
    #[test]
    fn malformed_metadata_and_segment_limits_are_checked_before_decoding() {
        assert_eq!(
            AdtsDecoder::new().decode_segment(b"ID3", None).unwrap_err(),
            AacError::InvalidMetadata
        );
        assert_eq!(
            AdtsDecoder::new()
                .decode_segment(&vec![0; MAX_SEGMENT_BYTES + 1], None)
                .unwrap_err(),
            AacError::TooLarge
        );
        assert_eq!(
            AdtsDecoder::new()
                .decode_segment(b"not adts", None)
                .unwrap_err(),
            AacError::InvalidHeader
        );
    }
    #[test]
    fn packet_limit_is_checked_before_decoding_tiny_frames() {
        let mut frame = write_adts_header(&AdtsHeader {
            object_type: 2,
            sample_rate: 24000,
            channels: 1,
            frame_length: 8,
            header_len: 7,
        });
        frame.push(0);
        let segment = frame.repeat(MAX_PACKETS + 1);
        assert_eq!(
            AdtsDecoder::new()
                .decode_segment(&segment, None)
                .unwrap_err(),
            AacError::TooManyPackets
        );
    }
}

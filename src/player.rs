//! メモリ内でのデコード検証と容量制限付き音声出力。録音ファイルは保存しない。
pub mod aac;
pub mod container;
pub mod diagnostics;
#[cfg(test)]
pub(crate) mod fixtures;
mod mp4_limits;
pub mod output;
pub mod playback;
pub mod seek;
pub mod state;
use std::io::Cursor;
use symphonia::core::{
    codecs::{CODEC_TYPE_AAC, CODEC_TYPE_PCM_S16LE, DecoderOptions},
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeProbe {
    Decoded {
        codec: &'static str,
        sample_rate: u32,
        channels: usize,
        frames: usize,
    },
    UnsupportedFormat,
    UnsupportedCodec,
    UnsupportedDeclaredCodec,
    AacDecoded(aac::AacSummary),
    RawAacDecoded(aac::AacSummary),
    AacFailed(aac::AacError),
    DecodeFailed,
    NoAudio,
    Encrypted,
}

pub fn probe_declared_sample(bytes: Vec<u8>, declared_codec: Option<&str>) -> DecodeProbe {
    if audio_offset(&bytes)
        .and_then(|offset| bytes.get(offset..))
        .is_some_and(rusty_aac::is_adts)
    {
        let mut decoder = aac::AdtsDecoder::new();
        return match decoder.decode_segment(&bytes, declared_codec) {
            Ok(summary) => DecodeProbe::AacDecoded(summary),
            Err(error) => DecodeProbe::AacFailed(error),
        };
    }
    probe_symphonia(bytes, declared_codec)
}

pub fn probe_sample(bytes: Vec<u8>) -> DecodeProbe {
    probe_declared_sample(bytes, None)
}

pub(crate) fn audio_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    for _ in 0..8 {
        if bytes.get(offset..offset.checked_add(3)?) != Some(b"ID3") {
            return Some(offset);
        }
        let tag = bytes.get(offset..offset.checked_add(10)?)?;
        if !(2..=4).contains(&tag[3]) || tag[6..10].iter().any(|b| b & 0x80 != 0) {
            return None;
        }
        let size = tag[6..10]
            .iter()
            .fold(0usize, |size, b| (size << 7) | usize::from(*b));
        let footer = if tag[3] == 4 && tag[5] & 0x10 != 0 {
            10
        } else {
            0
        };
        offset = offset
            .checked_add(10)?
            .checked_add(size)?
            .checked_add(footer)?;
        if offset > bytes.len() {
            return None;
        }
    }
    None
}

fn probe_symphonia(bytes: Vec<u8>, declared_codec: Option<&str>) -> DecodeProbe {
    // The pinned container parser can panic on a missing ES descriptor. Keep this
    // third-party parsing boundary from unwinding through the probe caller.
    std::panic::catch_unwind(|| probe_symphonia_inner(bytes, declared_codec))
        .unwrap_or(DecodeProbe::DecodeFailed)
}

fn probe_symphonia_inner(mut bytes: Vec<u8>, declared_codec: Option<&str>) -> DecodeProbe {
    let declared_he = declared_codec.is_some_and(|codecs| {
        codecs
            .split(',')
            .any(|codec| matches!(codec.trim(), "mp4a.40.5" | "mp4a.40.29"))
    });
    let unsupported = || {
        if declared_he {
            DecodeProbe::UnsupportedDeclaredCodec
        } else {
            DecodeProbe::UnsupportedFormat
        }
    };
    if bytes.len() > aac::MAX_SEGMENT_BYTES + 1024 * 1024 {
        return DecodeProbe::AacFailed(aac::AacError::TooLarge);
    }
    let Some(offset) = audio_offset(&bytes) else {
        return unsupported();
    };
    bytes.drain(..offset);
    if let Err(error) = mp4_limits::validate(&bytes) {
        return DecodeProbe::AacFailed(error);
    }
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let Ok(probed) = symphonia::default::get_probe().format(
        &Hint::new(),
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return unsupported();
    };
    let mut format = probed.format;
    let Some(track) = format.default_track() else {
        return DecodeProbe::NoAudio;
    };
    let id = track.id;
    if track.codec_params.codec == CODEC_TYPE_AAC {
        let Some(asc) = track.codec_params.extra_data.as_deref() else {
            return DecodeProbe::UnsupportedCodec;
        };
        let mut decoder = match aac::RawAacDecoder::new(asc) {
            Ok(decoder) => decoder,
            Err(error) => return DecodeProbe::AacFailed(error),
        };
        for _ in 0..512 {
            let Ok(packet) = format.next_packet() else {
                return DecodeProbe::NoAudio;
            };
            if packet.track_id() != id {
                continue;
            }
            return match decoder.decode_packets(&[packet.data.as_ref()], declared_codec) {
                Ok(summary) => DecodeProbe::RawAacDecoded(summary),
                Err(error) => DecodeProbe::AacFailed(error),
            };
        }
        return DecodeProbe::NoAudio;
    }
    if declared_he {
        return DecodeProbe::UnsupportedDeclaredCodec;
    }
    let codec = match track.codec_params.codec {
        CODEC_TYPE_AAC => "AAC",
        CODEC_TYPE_PCM_S16LE => "PCM S16LE",
        _ => "other",
    };
    let Ok(mut decoder) =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
    else {
        return DecodeProbe::UnsupportedCodec;
    };
    for _ in 0..512 {
        let Ok(packet) = format.next_packet() else {
            return DecodeProbe::NoAudio;
        };
        if packet.track_id() != id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) if audio.frames() > 0 => {
                return DecodeProbe::Decoded {
                    codec,
                    sample_rate: audio.spec().rate,
                    channels: audio.spec().channels.count(),
                    frames: audio.frames(),
                };
            }
            Ok(_) => (),
            Err(_) => return DecodeProbe::DecodeFailed,
        }
    }
    DecodeProbe::NoAudio
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_fragmented_mp4_is_a_first_packet_probe_only() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        for timestamp in [0, 24000 * 1000] {
            let (init, fragment) =
                fixtures::fragmented_aac(&[0x13, 0x08], &[&media[8..]; 3], timestamp);
            let bytes = [init, fragment].concat();
            assert!(
                matches!(probe_sample(bytes.clone()), DecodeProbe::RawAacDecoded(summary) if summary.frames == 1024)
            );
            assert_eq!(
                container::decode_aac_pcm_controlled(bytes, None, || false).unwrap_err(),
                aac::AacError::InvalidMetadata
            );
        }
    }
    #[test]
    fn generated_mp4_aac_uses_asc_and_does_not_report_he_aac_core_as_success() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let bytes = [init, media].concat();
        let DecodeProbe::RawAacDecoded(summary) =
            probe_declared_sample(bytes.clone(), Some("mp4a.40.2"))
        else {
            panic!(
                "generated MP4 was not decoded: {:?}",
                probe_sample(bytes.clone())
            );
        };
        assert_eq!(
            (
                summary.sample_rate,
                summary.channels,
                summary.frames,
                summary.packets
            ),
            (24000, 1, 1024, 1)
        );
        assert!(!summary.sbr_detected);
        for declaration in ["mp4a.40.5", "mp4a.40.29"] {
            assert_eq!(
                probe_declared_sample(bytes.clone(), Some(declaration)),
                DecodeProbe::AacFailed(aac::AacError::DeclaredSbrMissing)
            );
        }
    }
    #[test]
    fn incomplete_mp4_samples_and_oversized_input_never_report_decoded_audio() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let init_len = init.len();
        let bytes = [init, media].concat();
        for end in [
            0,
            8,
            init_len - 1,
            init_len,
            bytes.len() - 2,
            bytes.len() - 1,
        ] {
            assert!(!matches!(
                probe_sample(bytes[..end].to_vec()),
                DecodeProbe::RawAacDecoded(_) | DecodeProbe::Decoded { .. }
            ));
        }
        assert_eq!(
            probe_sample(vec![0; aac::MAX_SEGMENT_BYTES + 1024 * 1024 + 1]),
            DecodeProbe::AacFailed(aac::AacError::TooLarge)
        );
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x0c]); // Unsupported 960-frame ASC.
        assert_eq!(
            probe_sample([init, media].concat()),
            DecodeProbe::AacFailed(aac::AacError::InvalidConfig)
        );
    }
    #[test]
    fn malformed_mp4_descriptors_are_reported_without_unwinding() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let mut bytes = [init, media].concat();
        let esds = bytes.windows(4).position(|kind| kind == b"esds").unwrap();
        bytes[esds + 8] = 0; // Replace the required ES descriptor with an unknown descriptor.
        assert_eq!(
            probe_declared_sample(bytes, Some("mp4a.40.5")),
            DecodeProbe::DecodeFailed
        );
    }
    #[test]
    fn declared_he_aac_never_reports_core_only_decoding_as_full_success() {
        for codec in ["mp4a.40.5", "mp4a.40.29", "avc1.42, mp4a.40.5"] {
            assert_eq!(
                probe_declared_sample(vec![0; 8], Some(codec)),
                DecodeProbe::UnsupportedDeclaredCodec
            );
        }
        assert_eq!(
            probe_declared_sample(vec![0; 8], Some("mp4a.40.2")),
            DecodeProbe::UnsupportedFormat
        );
    }
    #[test]
    fn id3_size_is_checked_before_skipping_metadata() {
        assert_eq!(audio_offset(b"ID3\x04\0\0\0\0\0\0audio"), Some(10));
        assert_eq!(audio_offset(b"ID3\x04\0\0\0\0\0\x01Xaudio"), Some(11));
        assert_eq!(audio_offset(b"ID3\x04\0\0\0\0\0\x7f"), None);
        assert_eq!(audio_offset(b"ID3\x04\0\0\x80\0\0\0"), None);
        assert_eq!(audio_offset(b"ID3"), None);
    }
    #[test]
    fn generated_pcm_sample_is_decoded_without_output() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&44u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(
            probe_sample(bytes),
            DecodeProbe::Decoded {
                codec: "PCM S16LE",
                sample_rate: 8000,
                channels: 1,
                frames: 4
            }
        );
    }
    #[test]
    fn malformed_sample_is_not_reported_as_decoded() {
        assert_eq!(probe_sample(vec![0; 128]), DecodeProbe::UnsupportedFormat);
    }
}

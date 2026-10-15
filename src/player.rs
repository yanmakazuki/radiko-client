//! One in-memory sample, with no audio output and no persistent recording.
pub mod aac;
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
    if declared_codec.is_some_and(|codecs| {
        codecs
            .split(',')
            .any(|codec| matches!(codec.trim(), "mp4a.40.5" | "mp4a.40.29"))
    }) {
        // HE-AAC outside ADTS is not wired to the new decoder yet.
        // Keep the Symphonia fallback from reporting AAC core-only success.
        return DecodeProbe::UnsupportedDeclaredCodec;
    }
    probe_symphonia(bytes)
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

fn probe_symphonia(mut bytes: Vec<u8>) -> DecodeProbe {
    let Some(offset) = audio_offset(&bytes) else {
        return DecodeProbe::UnsupportedFormat;
    };
    bytes.drain(..offset);
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let Ok(probed) = symphonia::default::get_probe().format(
        &Hint::new(),
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return DecodeProbe::UnsupportedFormat;
    };
    let mut format = probed.format;
    let Some(track) = format.default_track() else {
        return DecodeProbe::NoAudio;
    };
    let id = track.id;
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

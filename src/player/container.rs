//! Bounded, explicit PCM extraction from a complete AAC container in memory.
use super::{
    aac::{AacError, DecodedPcm, MAX_SEGMENT_BYTES, RawAacDecoder},
    audio_offset,
};
use std::io::{Cursor, ErrorKind};
use symphonia::core::{
    codecs::CODEC_TYPE_AAC, errors::Error, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};

/// Requires a finite track duration and contiguous packets covering that duration.
/// Fragmented streams without a complete duration are deliberately rejected.
/// No audio device is opened; cancellation discards all intermediate data.
pub fn decode_aac_pcm_controlled(
    bytes: Vec<u8>,
    declared_codec: Option<&str>,
    cancelled: impl FnMut() -> bool,
) -> Result<DecodedPcm, AacError> {
    ContainerAacDecoder::default().decode_pcm_controlled(bytes, declared_codec, cancelled)
}

/// Retains overlap and SBR state between complete containers with identical ASC.
/// A discontinuity requires a new instance. Any failure resets the instance.
#[derive(Default)]
pub struct ContainerAacDecoder {
    asc: Vec<u8>,
    decoder: Option<RawAacDecoder>,
}
impl ContainerAacDecoder {
    pub fn decode_pcm_controlled(
        &mut self,
        bytes: Vec<u8>,
        declared_codec: Option<&str>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<DecodedPcm, AacError> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_inner(self, bytes, declared_codec, &mut cancelled)
        }))
        .unwrap_or(Err(AacError::DecodeFailed));
        if result.is_err() {
            self.asc.clear();
            self.decoder = None;
        }
        result
    }
}

fn decode_inner(
    state: &mut ContainerAacDecoder,
    mut bytes: Vec<u8>,
    declared_codec: Option<&str>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<DecodedPcm, AacError> {
    if cancelled() {
        return Err(AacError::Cancelled);
    }
    if bytes.len() > MAX_SEGMENT_BYTES + 1024 * 1024 {
        return Err(AacError::TooLarge);
    }
    let offset = audio_offset(&bytes).ok_or(AacError::InvalidMetadata)?;
    bytes.drain(..offset);
    super::mp4_limits::validate(&bytes)?;
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut format = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| AacError::UnsupportedTransport)?
        .format;
    if cancelled() {
        return Err(AacError::Cancelled);
    }
    let track = format.default_track().ok_or(AacError::Empty)?;
    if track.codec_params.codec != CODEC_TYPE_AAC {
        return Err(AacError::UnsupportedCodec);
    }
    let id = track.id;
    let duration = track
        .codec_params
        .n_frames
        .filter(|&n| n > 0)
        .ok_or(AacError::InvalidMetadata)?;
    let time_base = track
        .codec_params
        .time_base
        .ok_or(AacError::InvalidMetadata)?;
    if time_base.numer == 0 || time_base.denom == 0 {
        return Err(AacError::InvalidMetadata);
    }
    let asc = track
        .codec_params
        .extra_data
        .as_deref()
        .ok_or(AacError::InvalidConfig)?;
    if state.decoder.is_some() && state.asc != asc {
        return Err(AacError::FormatChanged);
    }
    if state.decoder.is_none() {
        state.decoder = Some(RawAacDecoder::new(asc)?);
        state.asc = asc.to_vec();
    }
    let decoder = state.decoder.as_mut().ok_or(AacError::DecodeFailed)?;
    let mut packets: Vec<Box<[u8]>> = Vec::new();
    let mut total_bytes = 0usize;
    let mut end = 0u64;
    // Includes packets on other tracks; the EOF read also consumes one iteration.
    for _ in 0..=512 {
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                if end != duration {
                    return Err(AacError::TruncatedFrame);
                }
                let refs: Vec<_> = packets.iter().map(|data| data.as_ref()).collect();
                let pcm = decoder.decode_packets_pcm_controlled(
                    &refs,
                    declared_codec,
                    &mut *cancelled,
                )?;
                // Container duration must agree with reconstructed PCM, including SBR.
                if pcm.summary.frames as u128 * u128::from(time_base.denom)
                    != u128::from(duration)
                        * u128::from(time_base.numer)
                        * u128::from(pcm.summary.sample_rate)
                {
                    return Err(AacError::InvalidMetadata);
                }
                if cancelled() {
                    return Err(AacError::Cancelled);
                }
                return Ok(pcm);
            }
            Err(_) => return Err(AacError::DecodeFailed),
        };
        if packet.track_id() != id {
            continue;
        }
        if packets.len() == 512 {
            return Err(AacError::TooManyPackets);
        }
        if packet.ts() != end || packet.dur() == 0 {
            return Err(AacError::InvalidMetadata);
        }
        end = end
            .checked_add(packet.dur())
            .ok_or(AacError::InvalidMetadata)?;
        if end > duration {
            return Err(AacError::InvalidMetadata);
        }
        total_bytes = total_bytes
            .checked_add(packet.data.len())
            .ok_or(AacError::TooLarge)?;
        if total_bytes > MAX_SEGMENT_BYTES {
            return Err(AacError::TooLarge);
        }
        packets.push(packet.data);
    }
    Err(AacError::TooManyPackets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::fixtures;

    #[test]
    fn adjacent_containers_keep_overlap_and_failure_discards_it() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let mut raw = RawAacDecoder::new(&[0x13, 0x08]).unwrap();
        let bytes = [init, media.clone()].concat();
        let mut decoder = ContainerAacDecoder::default();
        let baseline = decode_aac_pcm_controlled(bytes.clone(), None, || false).unwrap();
        for _ in 0..3 {
            let expected = raw.decode_packets_pcm(&[&media[8..]], None).unwrap();
            let actual = decoder
                .decode_pcm_controlled(bytes.clone(), None, || false)
                .unwrap();
            assert_eq!(actual.samples, expected.samples);
        }
        assert_eq!(
            decoder
                .decode_pcm_controlled(bytes.clone(), None, || true)
                .unwrap_err(),
            AacError::Cancelled
        );
        assert!(decoder.decoder.is_none());
        assert!(decoder.asc.is_empty());
        assert_eq!(
            decoder
                .decode_pcm_controlled(bytes.clone(), None, || false)
                .unwrap()
                .samples,
            baseline.samples
        );
        let (init, other) = fixtures::mp4_aac(&[0x13, 0x10]);
        assert_eq!(
            decoder
                .decode_pcm_controlled([init, other].concat(), None, || false)
                .unwrap_err(),
            AacError::FormatChanged
        );
        assert!(decoder.decoder.is_none());
        assert_eq!(
            decoder
                .decode_pcm_controlled(bytes.clone(), None, || false)
                .unwrap()
                .samples,
            baseline.samples
        );
        assert!(
            decoder
                .decode_pcm_controlled(bytes[..bytes.len() - 1].to_vec(), None, || false)
                .is_err()
        );
        assert!(decoder.decoder.is_none());
        assert_eq!(
            decoder
                .decode_pcm_controlled(bytes, None, || false)
                .unwrap()
                .samples,
            baseline.samples
        );
    }

    #[test]
    fn multiple_packets_are_all_decoded_and_late_corruption_returns_no_pcm() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let packets = vec![raw; 12];
        let (init, media) = fixtures::mp4_access_units(&[0x13, 0x08], &packets, 24000, 1);
        let bytes = [init, media].concat();
        let actual = decode_aac_pcm_controlled(bytes.clone(), None, || false).unwrap();
        let reference = RawAacDecoder::new(&[0x13, 0x08])
            .unwrap()
            .decode_packets_pcm(&packets, None)
            .unwrap();
        assert_eq!(actual.samples, reference.samples);
        assert_eq!(actual.summary.packets, 12);
        for missing in [1, raw.len(), raw.len() * 2] {
            assert!(
                decode_aac_pcm_controlled(bytes[..bytes.len() - missing].to_vec(), None, || false)
                    .is_err()
            );
        }
        let mut corrupt = packets;
        corrupt[11] = &[0];
        let (init, media) = fixtures::mp4_access_units(&[0x13, 0x08], &corrupt, 24000, 1);
        assert!(decode_aac_pcm_controlled([init, media].concat(), None, || false).is_err());
    }

    #[test]
    fn packet_pcm_input_and_metadata_limits_fail_closed() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        for count in [513, 512] {
            let (init, media) =
                fixtures::mp4_access_units(&[0x13, 0x08], &vec![&media[8..]; count], 24000, 1);
            let result = decode_aac_pcm_controlled([init, media].concat(), None, || false);
            if count == 513 {
                assert_eq!(result.unwrap_err(), AacError::TooManyPackets);
            } else {
                assert_eq!(result.unwrap().summary.packets, 512);
            }
        }
        assert_eq!(
            decode_aac_pcm_controlled(vec![0; MAX_SEGMENT_BYTES + 1024 * 1024 + 1], None, || false)
                .unwrap_err(),
            AacError::TooLarge
        );
        let (mut init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let mdhd = init.windows(4).position(|w| w == b"mdhd").unwrap();
        init[mdhd + 20..mdhd + 24].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            decode_aac_pcm_controlled([init, media].concat(), None, || false).unwrap_err(),
            AacError::InvalidMetadata
        );
    }

    #[test]
    fn complete_container_matches_raw_pcm_and_rejects_truncation_and_he_declaration() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let reference = RawAacDecoder::new(&[0x13, 0x08])
            .unwrap()
            .decode_packets_pcm(&[&media[8..]], None)
            .unwrap();
        let bytes = [init, media].concat();
        let actual = decode_aac_pcm_controlled(bytes.clone(), None, || false).unwrap();
        assert_eq!(actual.samples, reference.samples);
        assert_eq!(actual.summary.frames, 1024);
        assert!(
            decode_aac_pcm_controlled(bytes[..bytes.len() - 1].to_vec(), None, || false).is_err()
        );
        assert_eq!(
            decode_aac_pcm_controlled(bytes, Some("mp4a.40.5"), || false).unwrap_err(),
            AacError::DeclaredSbrMissing
        );
    }

    #[test]
    fn every_cancellation_checkpoint_discards_pcm() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let bytes = [init, media].concat();
        let mut calls = 0;
        decode_aac_pcm_controlled(bytes.clone(), None, || {
            calls += 1;
            false
        })
        .unwrap();
        for stop in 1..=calls {
            let mut call = 0;
            let mut decoder = ContainerAacDecoder::default();
            decoder
                .decode_pcm_controlled(bytes.clone(), None, || false)
                .unwrap();
            assert_eq!(
                decoder
                    .decode_pcm_controlled(bytes.clone(), None, || {
                        call += 1;
                        call == stop
                    })
                    .unwrap_err(),
                AacError::Cancelled
            );
            assert!(decoder.decoder.is_none());
            assert!(decoder.asc.is_empty());
        }
    }

    #[test]
    fn parser_panics_and_inconsistent_media_clock_are_errors() {
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let mut bytes = [init, media].concat();
        let mdhd = bytes.windows(4).position(|w| w == b"mdhd").unwrap();
        // Duration and sample table agree, but the media clock lies about PCM time.
        bytes[mdhd + 16..mdhd + 20].copy_from_slice(&48000u32.to_be_bytes());
        assert_eq!(
            decode_aac_pcm_controlled(bytes, None, || false).unwrap_err(),
            AacError::InvalidMetadata
        );
        let (init, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let mut bytes = [init, media].concat();
        let esds = bytes.windows(4).position(|w| w == b"esds").unwrap();
        bytes[esds + 8] = 0;
        assert_eq!(
            decode_aac_pcm_controlled(bytes, None, || false).unwrap_err(),
            AacError::DecodeFailed
        );
    }
}

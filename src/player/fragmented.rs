//! Explicit decoding of a bounded single-track AAC movie fragment.
//! Absolute decode time comes from tfdt, independently of Symphonia packet timestamps.
use super::aac::{AacError, DecodedPcm, MAX_SEGMENT_BYTES, RawAacDecoder};
use std::io::Cursor;
use symphonia::core::{
    codecs::CODEC_TYPE_AAC, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions,
    probe::Hint,
};

#[derive(Debug)]
pub struct FragmentPcm {
    pub decode_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub pcm: DecodedPcm,
}

/// Restricted to one AAC track, one traf/trun/mdat, relative addressing and no edits.
/// Initialization changes and discontinuities require a new instance.
pub struct FragmentedAacDecoder {
    track: u32,
    timescale: u32,
    default_duration: u32,
    default_size: u32,
    asc: Vec<u8>,
    decoder: Option<RawAacDecoder>,
    next_time: Option<u64>,
    next_sequence: Option<u32>,
}

impl FragmentedAacDecoder {
    pub fn new(init: Vec<u8>) -> Result<Self, AacError> {
        std::panic::catch_unwind(|| Self::initialize(init)).unwrap_or(Err(AacError::DecodeFailed))
    }

    fn initialize(init: Vec<u8>) -> Result<Self, AacError> {
        if init.len() > 1024 * 1024 {
            return Err(AacError::TooLarge);
        }
        super::mp4_limits::validate(&init)?;
        let top = atoms(&init)?;
        if top.len() != 2 || top[0].kind != b"ftyp" || top[1].kind != b"moov" {
            return Err(AacError::UnsupportedContainer);
        }
        let moov = atoms(top[1].data)?;
        let trak = atoms(one(&moov, b"trak")?.data)?;
        if trak.iter().any(|a| a.kind == b"edts") {
            return Err(AacError::UnsupportedContainer);
        }
        let mdia = atoms(one(&trak, b"mdia")?.data)?;
        let minf = atoms(one(&mdia, b"minf")?.data)?;
        let stbl = atoms(one(&minf, b"stbl")?.data)?;
        for kind in [b"stts", b"stsc", b"stco"] {
            let table = one(&stbl, kind)?.data;
            if table.len() != 8 || word(table, 0)? != 0 || word(table, 4)? != 0 {
                return Err(AacError::InvalidContainer);
            }
        }
        let stsz = one(&stbl, b"stsz")?.data;
        if stsz.len() != 12 || word(stsz, 0)? != 0 || word(stsz, 8)? != 0 {
            return Err(AacError::InvalidContainer);
        }
        let stsd = one(&stbl, b"stsd")?.data;
        if word(stsd, 0)? != 0 || word(stsd, 4)? != 1 {
            return Err(AacError::UnsupportedContainer);
        }
        let entries = atoms(stsd.get(8..).ok_or(AacError::InvalidContainer)?)?;
        if entries.len() != 1 || entries[0].kind != b"mp4a" {
            return Err(AacError::UnsupportedCodec);
        }
        let entry = entries[0].data;
        if entry.get(6..8) != Some(&[0, 1]) || entry.get(8..16) != Some(&[0; 8]) {
            return Err(AacError::UnsupportedContainer);
        }
        let description = atoms(entry.get(28..).ok_or(AacError::InvalidContainer)?)?;
        if description
            .iter()
            .any(|a| a.kind != b"esds" && a.kind != b"btrt")
        {
            return Err(AacError::UnsupportedContainer);
        }
        one(&description, b"esds")?;
        if minf.iter().any(|a| a.kind == b"dinf") {
            let dinf = one(&minf, b"dinf")?;
            let refs = atoms(dinf.data)?;
            let dref = one(&refs, b"dref")?.data;
            if word(dref, 0)? != 0 || word(dref, 4)? != 1 {
                return Err(AacError::UnsupportedContainer);
            }
            let urls = atoms(dref.get(8..).ok_or(AacError::InvalidContainer)?)?;
            if urls.len() != 1 || urls[0].kind != b"url " || urls[0].data != [0, 0, 0, 1] {
                return Err(AacError::UnsupportedContainer);
            }
        }
        let tkhd = one(&trak, b"tkhd")?.data;
        let track = match tkhd.first() {
            Some(0) => word(tkhd, 12)?,
            Some(1) => word(tkhd, 20)?,
            _ => return Err(AacError::InvalidContainer),
        };
        if track == 0 {
            return Err(AacError::InvalidContainer);
        }
        let mvex = atoms(one(&moov, b"mvex")?.data)?;
        let trex = one(&mvex, b"trex")?.data;
        if trex.len() != 24 || word(trex, 0)? != 0 || word(trex, 4)? != track || word(trex, 8)? != 1
        {
            return Err(AacError::InvalidContainer);
        }
        let default_duration = word(trex, 12)?;
        let default_size = word(trex, 16)?;
        // Only metadata is read by Symphonia; media packet timestamps are never used.
        let stream = MediaSourceStream::new(Box::new(Cursor::new(init)), Default::default());
        let format = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|_| AacError::InvalidContainer)?
            .format;
        if format.tracks().len() != 1 {
            return Err(AacError::UnsupportedContainer);
        }
        let params = &format.tracks()[0].codec_params;
        if params.codec != CODEC_TYPE_AAC {
            return Err(AacError::UnsupportedCodec);
        }
        if params.n_frames != Some(0) {
            return Err(AacError::InvalidContainer);
        }
        let time = params.time_base.ok_or(AacError::InvalidContainer)?;
        if time.numer != 1 || time.denom == 0 {
            return Err(AacError::InvalidContainer);
        }
        let asc = params
            .extra_data
            .as_deref()
            .ok_or(AacError::InvalidConfig)?
            .to_vec();
        let decoder = RawAacDecoder::new(&asc)?;
        Ok(Self {
            track,
            timescale: time.denom,
            default_duration,
            default_size,
            asc,
            decoder: Some(decoder),
            next_time: None,
            next_sequence: None,
        })
    }

    /// Success advances the timeline. Every failure discards overlap and timeline state.
    pub fn decode_pcm_controlled(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<FragmentPcm, AacError> {
        let result = self.decode(bytes, declared_codec, &mut cancelled);
        if result.is_err() {
            self.decoder = None;
            self.next_time = None;
            self.next_sequence = None;
        }
        result
    }

    fn decode(
        &mut self,
        bytes: &[u8],
        declared_codec: Option<&str>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<FragmentPcm, AacError> {
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        if bytes.len() > MAX_SEGMENT_BYTES {
            return Err(AacError::TooLarge);
        }
        super::mp4_limits::validate(bytes)?;
        let top = atoms(bytes)?;
        let media = match top.as_slice() {
            [moof, mdat] if moof.kind == b"moof" && mdat.kind == b"mdat" => (moof, mdat),
            [styp, moof, mdat]
                if styp.kind == b"styp" && moof.kind == b"moof" && mdat.kind == b"mdat" =>
            {
                (moof, mdat)
            }
            _ => return Err(AacError::UnsupportedContainer),
        };
        let (moof, mdat) = media;
        let children = atoms(moof.data)?;
        if children.len() != 2 {
            return Err(AacError::UnsupportedContainer);
        }
        let mfhd = one(&children, b"mfhd")?.data;
        if mfhd.len() != 8 || word(mfhd, 0)? != 0 {
            return Err(AacError::InvalidContainer);
        }
        let sequence = word(mfhd, 4)?;
        if self.next_sequence.is_some_and(|next| next != sequence) {
            return Err(AacError::InvalidContainer);
        }
        let traf = atoms(one(&children, b"traf")?.data)?;
        if traf.len() != 3 {
            return Err(AacError::UnsupportedContainer);
        }
        let tfhd = one(&traf, b"tfhd")?.data;
        let flags = word(tfhd, 0)?;
        // No external/base offsets, alternate sample descriptions or empty fragments.
        if flags & !0x2003a != 0 || flags & 0x20000 == 0 || word(tfhd, 4)? != self.track {
            return Err(AacError::UnsupportedContainer);
        }
        let mut at = 8;
        if flags & 2 != 0 && take_word(tfhd, &mut at)? != 1 {
            return Err(AacError::UnsupportedContainer);
        }
        let default_duration = if flags & 8 != 0 {
            take_word(tfhd, &mut at)?
        } else {
            self.default_duration
        };
        let default_size = if flags & 16 != 0 {
            take_word(tfhd, &mut at)?
        } else {
            self.default_size
        };
        if flags & 32 != 0 {
            take_word(tfhd, &mut at)?;
        }
        if at != tfhd.len() {
            return Err(AacError::InvalidContainer);
        }
        let tfdt = one(&traf, b"tfdt")?.data;
        let decode_time = match (word(tfdt, 0)?, tfdt.len()) {
            (0, 8) => u64::from(word(tfdt, 4)?),
            (0x1000000, 12) => (u64::from(word(tfdt, 4)?) << 32) | u64::from(word(tfdt, 8)?),
            _ => return Err(AacError::InvalidContainer),
        };
        if self.next_time.is_some_and(|next| next != decode_time) {
            return Err(AacError::InvalidContainer);
        }
        let trun = one(&traf, b"trun")?.data;
        let flags = word(trun, 0)?;
        if flags & !0x705 != 0 || flags & 1 == 0 || flags & 0x404 == 0x404 {
            return Err(AacError::UnsupportedContainer);
        }
        let count = word(trun, 4)? as usize;
        if count == 0 {
            return Err(AacError::Empty);
        }
        if count > 512 {
            return Err(AacError::TooManyPackets);
        }
        let offset = word(trun, 8)? as i32;
        if offset < 0 || moof.offset.checked_add(offset as usize) != Some(mdat.offset + mdat.header)
        {
            return Err(AacError::InvalidContainer);
        }
        let mut at = 12;
        if flags & 4 != 0 {
            take_word(trun, &mut at)?;
        }
        let mut cursor = 0usize;
        let mut duration = 0u64;
        let mut packets = Vec::with_capacity(count);
        for _ in 0..count {
            if cancelled() {
                return Err(AacError::Cancelled);
            }
            let dur = if flags & 0x100 != 0 {
                take_word(trun, &mut at)?
            } else {
                default_duration
            };
            let size = if flags & 0x200 != 0 {
                take_word(trun, &mut at)?
            } else {
                default_size
            } as usize;
            if flags & 0x400 != 0 {
                take_word(trun, &mut at)?;
            }
            if dur == 0 || size == 0 {
                return Err(AacError::InvalidContainer);
            }
            duration = duration
                .checked_add(u64::from(dur))
                .ok_or(AacError::InvalidContainer)?;
            let end = cursor.checked_add(size).ok_or(AacError::TooLarge)?;
            packets.push(
                mdat.data
                    .get(cursor..end)
                    .ok_or(AacError::InvalidContainer)?,
            );
            cursor = end;
        }
        if at != trun.len() || cursor != mdat.data.len() {
            return Err(AacError::InvalidContainer);
        }
        let next_time = decode_time
            .checked_add(duration)
            .ok_or(AacError::InvalidContainer)?;
        let next_sequence = sequence.checked_add(1).ok_or(AacError::InvalidContainer)?;
        if self.decoder.is_none() {
            self.decoder = Some(RawAacDecoder::new(&self.asc)?);
        }
        let pcm = self
            .decoder
            .as_mut()
            .ok_or(AacError::DecodeFailed)?
            .decode_packets_pcm_controlled(&packets, declared_codec, &mut *cancelled)?;
        if pcm.summary.frames as u128 * u128::from(self.timescale)
            != u128::from(duration) * u128::from(pcm.summary.sample_rate)
        {
            return Err(AacError::InvalidContainer);
        }
        if cancelled() {
            return Err(AacError::Cancelled);
        }
        self.next_time = Some(next_time);
        self.next_sequence = Some(next_sequence);
        Ok(FragmentPcm {
            decode_time,
            timescale: self.timescale,
            duration,
            pcm,
        })
    }
}

struct Atom<'a> {
    kind: &'a [u8],
    data: &'a [u8],
    offset: usize,
    header: usize,
}
fn atoms(bytes: &[u8]) -> Result<Vec<Atom<'_>>, AacError> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if result.len() == 8192 {
            return Err(AacError::TooManyPackets);
        }
        let rest = &bytes[at..];
        let size = word(rest, 0)?;
        let kind = rest.get(4..8).ok_or(AacError::InvalidContainer)?;
        let (size, header) = match size {
            0 => (rest.len(), 8),
            1 => {
                let size = (u64::from(word(rest, 8)?) << 32) | u64::from(word(rest, 12)?);
                (usize::try_from(size).map_err(|_| AacError::TooLarge)?, 16)
            }
            n => (n as usize, 8),
        };
        if size < header || size > rest.len() {
            return Err(AacError::InvalidContainer);
        }
        result.push(Atom {
            kind,
            data: &rest[header..size],
            offset: at,
            header,
        });
        at += size;
    }
    Ok(result)
}
fn one<'a, 'b>(atoms: &'b [Atom<'a>], kind: &[u8]) -> Result<&'b Atom<'a>, AacError> {
    let mut found = atoms.iter().filter(|atom| atom.kind == kind);
    let first = found.next().ok_or(AacError::InvalidContainer)?;
    if found.next().is_some() {
        return Err(AacError::UnsupportedContainer);
    }
    Ok(first)
}
fn word(bytes: &[u8], at: usize) -> Result<u32, AacError> {
    let end = at.checked_add(4).ok_or(AacError::InvalidContainer)?;
    let data = bytes.get(at..end).ok_or(AacError::InvalidContainer)?;
    Ok(u32::from_be_bytes(
        data.try_into().map_err(|_| AacError::InvalidContainer)?,
    ))
}
fn take_word(bytes: &[u8], at: &mut usize) -> Result<u32, AacError> {
    let value = word(bytes, *at)?;
    *at += 4;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::fixtures;

    fn atom(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [
            ((data.len() + 8) as u32).to_be_bytes().as_slice(),
            kind,
            data,
        ]
        .concat()
    }
    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    #[test]
    fn sample_defaults_follow_trex_and_tfhd_overrides() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        for override_defaults in [false, true] {
            let (mut init, _) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw], 0);
            let trex = init.windows(4).position(|w| w == b"trex").unwrap();
            init[trex + 20..trex + 24].copy_from_slice(&(raw.len() as u32).to_be_bytes());
            let tfhd = if override_defaults {
                init[trex + 16..trex + 20].copy_from_slice(&1u32.to_be_bytes());
                init[trex + 20..trex + 24].copy_from_slice(&1u32.to_be_bytes());
                words(&[0x20018, 1, 1024, raw.len() as u32])
            } else {
                words(&[0x20000, 1])
            };
            let make_moof = |offset| {
                atom(
                    b"moof",
                    &[
                        atom(b"mfhd", &words(&[0, 1])),
                        atom(
                            b"traf",
                            &[
                                atom(b"tfhd", &tfhd),
                                atom(b"tfdt", &words(&[0, 7000])),
                                atom(b"trun", &words(&[1, 3, offset])),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                )
            };
            let offset = make_moof(0).len() as u32 + 8;
            let bytes = [make_moof(offset), atom(b"mdat", &raw.repeat(3))].concat();
            let actual = FragmentedAacDecoder::new(init)
                .unwrap()
                .decode_pcm_controlled(&bytes, None, || false)
                .unwrap();
            assert_eq!((actual.decode_time, actual.duration), (7000, 3072));
            let expected = RawAacDecoder::new(&[0x13, 0x08])
                .unwrap()
                .decode_packets_pcm(&[raw; 3], None)
                .unwrap();
            assert_eq!(actual.pcm.samples, expected.samples);
        }
    }

    fn fragment(raw: &[u8], time: u64, sequence: u32) -> Vec<u8> {
        let (_, mut bytes) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw; 3], time);
        let at = bytes.windows(4).position(|w| w == b"mfhd").unwrap();
        bytes[at + 8..at + 12].copy_from_slice(&sequence.to_be_bytes());
        bytes
    }

    #[test]
    fn absolute_decode_time_and_overlap_survive_adjacent_fragments() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let start = u64::from(u32::MAX) + 1000;
        let (init, _) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw], start);
        let mut decoder = FragmentedAacDecoder::new(init).unwrap();
        let mut reference = RawAacDecoder::new(&[0x13, 0x08]).unwrap();
        for i in 0..3 {
            let time = start + u64::from(i) * 3072;
            let actual = decoder
                .decode_pcm_controlled(&fragment(raw, time, i + 1), None, || false)
                .unwrap();
            assert_eq!(
                (actual.decode_time, actual.timescale, actual.duration),
                (time, 24000, 3072)
            );
            assert_eq!(
                actual.pcm.samples,
                reference
                    .decode_packets_pcm(&[raw; 3], None)
                    .unwrap()
                    .samples
            );
        }
    }

    #[test]
    fn duplicate_gap_bad_offsets_and_truncated_audio_discard_state() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let (init, _) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw], 1000);
        let first = fragment(raw, 1000, 1);
        let mut decoder = FragmentedAacDecoder::new(init).unwrap();
        let mut wrong_offset = fragment(raw, 4072, 2);
        let at = wrong_offset.windows(4).position(|w| w == b"trun").unwrap();
        wrong_offset[at + 12..at + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        let next = fragment(raw, 4072, 2);
        for invalid in [
            first.clone(),
            fragment(raw, 4073, 2),
            fragment(raw, 4072, 3),
            wrong_offset,
            next[..next.len() - 1].to_vec(),
        ] {
            decoder
                .decode_pcm_controlled(&first, None, || false)
                .unwrap();
            assert!(
                decoder
                    .decode_pcm_controlled(&invalid, None, || false)
                    .is_err()
            );
            assert!(decoder.decoder.is_none());
            assert!(decoder.next_time.is_none());
            assert!(decoder.next_sequence.is_none());
        }
        decoder
            .decode_pcm_controlled(&first, None, || false)
            .unwrap();
    }

    #[test]
    fn every_cancellation_checkpoint_discards_old_state_and_partial_pcm() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let (init, _) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw], 0);
        let first = fragment(raw, 0, 1);
        let next = fragment(raw, 3072, 2);
        let mut decoder = FragmentedAacDecoder::new(init).unwrap();
        let mut calls = 0;
        decoder
            .decode_pcm_controlled(&first, None, || {
                calls += 1;
                false
            })
            .unwrap();
        decoder.decoder = None;
        decoder.next_time = None;
        decoder.next_sequence = None;
        for stop in 1..=calls {
            decoder
                .decode_pcm_controlled(&first, None, || false)
                .unwrap();
            let mut call = 0;
            assert_eq!(
                decoder
                    .decode_pcm_controlled(&next, None, || {
                        call += 1;
                        call == stop
                    })
                    .unwrap_err(),
                AacError::Cancelled
            );
            assert!(decoder.decoder.is_none());
            assert!(decoder.next_time.is_none());
        }
    }

    #[test]
    fn inconsistent_headers_and_media_clock_never_return_pcm() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let raw = &media[8..];
        let (init, good) = fixtures::fragmented_aac(&[0x13, 0x08], &[raw; 3], 1000);
        let mut decoder = FragmentedAacDecoder::new(init).unwrap();
        for (kind, relative, value) in [
            (b"tfhd", 8, 2u32),    // Wrong track.
            (b"tfhd", 4, 0x20001), // Absolute external offset.
            (b"trun", 4, 0xb01),   // Composition offsets unsupported.
            (b"trun", 8, 513),     // Packet limit.
            (b"trun", 8, 0),
            (b"trun", 16, 1025),     // Metadata duration differs from PCM.
            (b"tfdt", 4, 0x1000000), // 64-bit version with 32-bit payload.
            (b"mfhd", 4, 0x1000000),
        ] {
            let mut bad = good.clone();
            let at = bad.windows(4).position(|w| w == kind).unwrap() + relative;
            bad[at..at + 4].copy_from_slice(&value.to_be_bytes());
            assert!(
                decoder.decode_pcm_controlled(&bad, None, || false).is_err(),
                "accepted {kind:?}/{relative}"
            );
            assert!(decoder.decoder.is_none());
        }
        let overflow = fragment(raw, u64::MAX - 1, 1);
        assert_eq!(
            decoder
                .decode_pcm_controlled(&overflow, None, || false)
                .unwrap_err(),
            AacError::InvalidContainer
        );
        assert_eq!(
            decoder
                .decode_pcm_controlled(&good, None, || false)
                .unwrap()
                .decode_time,
            1000
        );
    }

    #[test]
    fn initialization_requires_an_empty_single_aac_track_and_rejects_external_references() {
        let (_, media) = fixtures::mp4_aac(&[0x13, 0x08]);
        let (init, _) = fixtures::fragmented_aac(&[0x13, 0x08], &[&media[8..]], 0);
        for (kind, relative, bytes) in [
            (b"trex", 8, vec![0, 0, 0, 2]),
            (b"stts", 8, vec![0, 0, 0, 1]),
            (b"mp4a", 10, vec![0, 2]),
            (b"mdhd", 16, vec![0, 0, 0, 0]),
            (b"esds", 8, vec![0]),
        ] {
            let mut bad = init.clone();
            let at = bad.windows(4).position(|w| w == kind).unwrap() + relative;
            bad[at..at + bytes.len()].copy_from_slice(&bytes);
            assert!(FragmentedAacDecoder::new(bad).is_err());
        }
        assert!(matches!(
            FragmentedAacDecoder::new(vec![0; 1024 * 1024 + 1]),
            Err(AacError::TooLarge)
        ));
    }
}

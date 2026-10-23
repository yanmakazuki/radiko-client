//! Bounds declared MP4 table sizes before invoking the third-party demuxer.
use super::aac::AacError;

const MAX_ENTRIES: u32 = 4096;
const MAX_BOXES: usize = 8192;

pub(super) fn validate(bytes: &[u8]) -> Result<(), AacError> {
    if !matches!(
        bytes.get(4..8),
        Some(b"ftyp" | b"styp" | b"moov" | b"moof" | b"mdat" | b"free" | b"skip" | b"wide")
    ) {
        // The probe searches up to 1 MiB for ftyp and may discard a prefix.
        // Validate the same MP4 suffix so prefix junk cannot bypass the limits.
        if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
            return Ok(());
        }
        if let Some(marker) = bytes[..bytes.len().min(1024 * 1024 + 16)]
            .windows(4)
            .position(|window| window == b"ftyp")
        {
            let start = marker.checked_sub(4).ok_or(AacError::InvalidContainer)?;
            return walk(&bytes[start..], 0, &mut 0, &mut 0);
        }
        return Ok(());
    }
    walk(bytes, 0, &mut 0, &mut 0)
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, AacError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(AacError::InvalidContainer)?;
    Ok(u32::from_be_bytes(
        value.try_into().map_err(|_| AacError::InvalidContainer)?,
    ))
}

fn walk(
    mut bytes: &[u8],
    depth: usize,
    boxes: &mut usize,
    samples: &mut u32,
) -> Result<(), AacError> {
    if depth > 16 {
        return Err(AacError::InvalidContainer);
    }
    while !bytes.is_empty() {
        *boxes += 1;
        if *boxes > MAX_BOXES {
            return Err(AacError::TooManyPackets);
        }
        let size = word(bytes, 0)?;
        let kind = bytes.get(4..8).ok_or(AacError::InvalidContainer)?;
        let (size, header) = match size {
            0 => (bytes.len(), 8),
            1 => {
                let value = bytes.get(8..16).ok_or(AacError::InvalidContainer)?;
                let size =
                    u64::from_be_bytes(value.try_into().map_err(|_| AacError::InvalidContainer)?);
                (usize::try_from(size).map_err(|_| AacError::TooLarge)?, 16)
            }
            n => (n as usize, 8),
        };
        if size < header || size > bytes.len() {
            return Err(AacError::InvalidContainer);
        }
        let payload = &bytes[header..size];
        match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"mvex" | b"moof" | b"traf"
            | b"edts" => {
                walk(payload, depth + 1, boxes, samples)?;
            }
            b"trun" | b"stsz" | b"stz2" => {
                let offset = if kind == b"trun" { 4 } else { 8 };
                let count = word(payload, offset)?;
                *samples = samples.checked_add(count).ok_or(AacError::TooManyPackets)?;
                if *samples > MAX_ENTRIES {
                    return Err(AacError::TooManyPackets);
                }
            }
            b"stts" | b"ctts" | b"stsc" | b"stco" | b"co64" | b"stss" | b"elst" => {
                let count = word(payload, 4)?;
                if count > MAX_ENTRIES {
                    return Err(AacError::TooManyPackets);
                }
                if kind == b"stts" {
                    let mut expanded = 0u32;
                    for i in 0..count as usize {
                        expanded = expanded
                            .checked_add(word(payload, 8 + i * 8)?)
                            .ok_or(AacError::TooManyPackets)?;
                        if expanded > MAX_ENTRIES {
                            return Err(AacError::TooManyPackets);
                        }
                    }
                }
            }
            _ => (),
        }
        bytes = &bytes[size..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            ((payload.len() + 8) as u32).to_be_bytes().as_slice(),
            kind,
            payload,
        ]
        .concat()
    }
    #[test]
    fn tiny_fragment_with_huge_sample_count_is_rejected_before_demuxing() {
        let trun = atom(b"trun", &[0, 0, 0, 0, 255, 255, 255, 255]);
        let bytes = atom(b"moof", &atom(b"traf", &trun));
        assert_eq!(validate(&bytes), Err(AacError::TooManyPackets));
        assert_eq!(
            crate::player::probe_sample(bytes),
            crate::player::DecodeProbe::AacFailed(AacError::TooManyPackets)
        );
        let prefixed = [
            b"unstructured-prefix".as_slice(),
            &atom(b"ftyp", b"isom0000"),
            &atom(b"moof", &atom(b"traf", &trun)),
        ]
        .concat();
        assert_eq!(validate(&prefixed), Err(AacError::TooManyPackets));
    }
    #[test]
    fn nested_boxes_truncation_and_expanded_table_counts_are_bounded() {
        let mut bytes = atom(b"free", &[]);
        for _ in 0..18 {
            bytes = atom(b"moov", &bytes);
        }
        assert_eq!(validate(&bytes), Err(AacError::InvalidContainer));
        let table = atom(
            b"stts",
            &[0, 0, 0, 0, 0, 0, 0, 1, 255, 255, 255, 255, 0, 0, 4, 0],
        );
        assert_eq!(
            validate(&atom(b"moov", &table)),
            Err(AacError::TooManyPackets)
        );
        let bytes = atom(b"moov", &atom(b"free", &[]));
        for end in 8..bytes.len() {
            assert!(validate(&bytes[..end]).is_err());
        }
        assert_eq!(
            validate(&atom(b"moov", &[0; 4])),
            Err(AacError::InvalidContainer)
        );
        let many = atom(b"moov", &atom(b"free", &[]).repeat(MAX_BOXES));
        assert_eq!(validate(&many), Err(AacError::TooManyPackets));
    }

    #[test]
    fn extended_sizes_zero_sizes_and_aggregate_sample_counts_are_checked() {
        let valid = atom(b"free", &[]);
        let extended = [
            1u32.to_be_bytes().as_slice(),
            b"moov",
            &24u64.to_be_bytes(),
            &valid,
        ]
        .concat();
        assert_eq!(validate(&extended), Ok(()));
        for end in 8..extended.len() {
            assert!(validate(&extended[..end]).is_err());
        }
        assert_eq!(
            validate(&[0u32.to_be_bytes().as_slice(), b"moov", &valid].concat()),
            Ok(())
        );
        let trun = atom(b"trun", &[0, 0, 0, 0, 0, 0, 16, 0]);
        assert_eq!(
            validate(&atom(b"moof", &atom(b"traf", &trun.repeat(2)))),
            Err(AacError::TooManyPackets)
        );
    }
}

//! Synthetic AAC/MP4 used only by offline tests; no broadcast recordings.
use rusty_aac::{AacEncoder, AacEncoderConfig};

fn atom(kind: &[u8; 4], data: Vec<u8>) -> Vec<u8> {
    let mut result = ((data.len() + 8) as u32).to_be_bytes().to_vec();
    result.extend(kind);
    result.extend(data);
    result
}
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}
fn descriptor(tag: u8, bytes: Vec<u8>) -> Vec<u8> {
    assert!(bytes.len() < 128);
    [vec![tag, bytes.len() as u8], bytes].concat()
}

pub(crate) fn mp4_aac(asc: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut encoder = AacEncoder::new(AacEncoderConfig::default());
    let samples: Vec<_> = (0..2048).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
    encoder.push_pcm(&samples, 1, 24000).unwrap();
    encoder.finish();
    let packet = encoder.next_packet().unwrap().data;
    mp4_access_unit(asc, &packet, 24000, 1)
}

pub(crate) fn mp4_access_unit(
    asc: &[u8],
    packet: &[u8],
    core_rate: u32,
    channels: u16,
) -> (Vec<u8>, Vec<u8>) {
    mp4_access_units(asc, &[packet], core_rate, channels)
}

pub(crate) fn mp4_access_units(
    asc: &[u8],
    packets: &[&[u8]],
    core_rate: u32,
    channels: u16,
) -> (Vec<u8>, Vec<u8>) {
    assert!(core_rate <= 65535);
    let count = packets.len() as u32;
    let duration = count * 1024;
    let sizes = [
        words(&[0, 0, count]),
        words(&packets.iter().map(|p| p.len() as u32).collect::<Vec<_>>()),
    ]
    .concat();
    let ftyp = atom(b"ftyp", [b"M4A \0\0\0\0".as_slice(), b"M4A isom"].concat());
    let make_moov = |offset| {
        let mut mvhd = vec![0; 100];
        mvhd[12..16].copy_from_slice(&core_rate.to_be_bytes());
        mvhd[16..20].copy_from_slice(&duration.to_be_bytes());
        mvhd[20..24].copy_from_slice(&65536u32.to_be_bytes());
        mvhd[24..26].copy_from_slice(&256u16.to_be_bytes());
        let mut tkhd = vec![0; 84];
        tkhd[3] = 7;
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        tkhd[20..24].copy_from_slice(&duration.to_be_bytes());
        tkhd[36..38].copy_from_slice(&256u16.to_be_bytes());
        let mut mdhd = words(&[0, 0, 0, core_rate, duration]);
        mdhd.extend([0; 4]);
        let mut hdlr = words(&[0, 0]);
        hdlr.extend(b"soun");
        hdlr.extend([0; 13]);
        let config = descriptor(
            4,
            [vec![0x40, 0x15], vec![0; 11], descriptor(5, asc.to_vec())].concat(),
        );
        let es = descriptor(3, [vec![0, 1, 0], config, descriptor(6, vec![2])].concat());
        let esds = atom(b"esds", [vec![0; 4], es].concat());
        let mut entry = vec![0; 28];
        entry[6..8].copy_from_slice(&1u16.to_be_bytes());
        entry[16..18].copy_from_slice(&channels.to_be_bytes());
        entry[18..20].copy_from_slice(&16u16.to_be_bytes());
        entry[24..28].copy_from_slice(&(core_rate << 16).to_be_bytes());
        entry.extend(esds);
        let stsd = atom(b"stsd", [words(&[0, 1]), atom(b"mp4a", entry)].concat());
        let stbl = atom(
            b"stbl",
            [
                stsd,
                atom(
                    b"stts",
                    if count == 0 {
                        words(&[0, 0])
                    } else {
                        words(&[0, 1, count, 1024])
                    },
                ),
                atom(
                    b"stsc",
                    if count == 0 {
                        words(&[0, 0])
                    } else {
                        words(&[0, 1, 1, count, 1])
                    },
                ),
                atom(b"stsz", sizes.clone()),
                atom(
                    b"stco",
                    if count == 0 {
                        words(&[0, 0])
                    } else {
                        words(&[0, 1, offset])
                    },
                ),
            ]
            .concat(),
        );
        let mdia = atom(
            b"mdia",
            [
                atom(b"mdhd", mdhd),
                atom(b"hdlr", hdlr),
                atom(b"minf", stbl),
            ]
            .concat(),
        );
        atom(
            b"moov",
            [
                atom(b"mvhd", mvhd),
                atom(b"trak", [atom(b"tkhd", tkhd), mdia].concat()),
            ]
            .concat(),
        )
    };
    let size = make_moov(0).len();
    let moov = make_moov((ftyp.len() + size + 8) as u32);
    ([ftyp, moov].concat(), atom(b"mdat", packets.concat()))
}

pub(crate) fn fragmented_aac(asc: &[u8], packets: &[&[u8]], timestamp: u64) -> (Vec<u8>, Vec<u8>) {
    let (mut init, _) = mp4_access_units(asc, &[], 24000, 1);
    let moov = init.windows(4).position(|w| w == b"moov").unwrap() - 4;
    let mvex = atom(b"mvex", atom(b"trex", words(&[0, 1, 1, 1024, 0, 0])));
    let size = (init.len() - moov + mvex.len()) as u32;
    init[moov..moov + 4].copy_from_slice(&size.to_be_bytes());
    init.extend(mvex);
    let make_moof = |offset| {
        atom(
            b"moof",
            [
                atom(b"mfhd", words(&[0, 1])),
                atom(
                    b"traf",
                    [
                        atom(b"tfhd", words(&[0x20000, 1])),
                        atom(
                            b"tfdt",
                            [words(&[0x1000000]), timestamp.to_be_bytes().to_vec()].concat(),
                        ),
                        atom(
                            b"trun",
                            [
                                words(&[0x301, packets.len() as u32, offset]),
                                packets
                                    .iter()
                                    .flat_map(|p| words(&[1024, p.len() as u32]))
                                    .collect(),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )
    };
    let offset = make_moof(0).len() as u32 + 8;
    (
        init,
        [make_moof(offset), atom(b"mdat", packets.concat())].concat(),
    )
}

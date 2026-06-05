use crate::frame::{Timecode, DATA_TYPE_AUDIO};
use crate::iso_reader::SECTOR_SIZE;
use std::fs::File;
use std::io::Write;

pub fn write_iso(sectors: &[Vec<u8>]) -> tempfile::TempDir {
    let td = tempfile::tempdir().expect("tempdir");
    let path = td.path().join("test.iso");
    let mut f = File::create(&path).expect("create test iso");
    for sector in sectors {
        assert_eq!(sector.len(), SECTOR_SIZE as usize, "sector length");
        f.write_all(sector).expect("write sector");
    }
    td
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

pub fn tc_at(frame_count: u32) -> Timecode {
    let minutes = frame_count / (60 * 75);
    let rem = frame_count % (60 * 75);
    let seconds = rem / 75;
    let frames = rem % 75;
    Timecode {
        minutes: minutes as u8,
        seconds: seconds as u8,
        frames: frames as u8,
    }
}

pub fn synth_audio_sector(frame_start: bool, payload: &[u8], tc: Timecode) -> Vec<u8> {
    assert!(payload.len() <= 2045);
    let frame_info_count = if frame_start { 1 } else { 0 };
    let packet_info_count = 1u8;
    let mut s = Vec::with_capacity(SECTOR_SIZE as usize);
    s.push((frame_info_count << 2) | (packet_info_count << 5));
    s.extend_from_slice(&packet_info(frame_start, DATA_TYPE_AUDIO, payload.len() as u16));
    if frame_start {
        s.extend_from_slice(&[tc.minutes, tc.seconds, tc.frames]);
    }
    s.extend_from_slice(payload);
    s.resize(SECTOR_SIZE as usize, 0);
    s
}

pub fn synth_continuation_sector(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 2045);
    let mut s = Vec::with_capacity(SECTOR_SIZE as usize);
    s.push(1 << 5); // one packet, no frame-info records
    s.extend_from_slice(&packet_info(false, DATA_TYPE_AUDIO, payload.len() as u16));
    s.extend_from_slice(payload);
    s.resize(SECTOR_SIZE as usize, 0);
    s
}

pub fn synth_dst_sector(
    payload: &[u8],
    channel_count: u8,
    sector_count: u8,
    tc: Timecode,
) -> Vec<u8> {
    assert!(payload.len() <= 2041);
    let mut s = Vec::with_capacity(SECTOR_SIZE as usize);
    let dst_encoded = 1u8;
    let frame_info_count = 1u8;
    let packet_info_count = 1u8;
    s.push(dst_encoded | (frame_info_count << 2) | (packet_info_count << 5));
    s.extend_from_slice(&packet_info(true, DATA_TYPE_AUDIO, payload.len() as u16));
    let channel_bits = match channel_count {
        6 => 0b0000_0010,
        5 => 0b0000_0001,
        _ => 0,
    };
    let b3 = ((sector_count & 0x1f) << 2) | channel_bits;
    s.extend_from_slice(&[tc.minutes, tc.seconds, tc.frames, b3]);
    s.extend_from_slice(payload);
    s.resize(SECTOR_SIZE as usize, 0);
    s
}

fn packet_info(frame_start: bool, data_type: u8, len: u16) -> [u8; 2] {
    assert!(len <= 0x07ff);
    [
        ((frame_start as u8) << 7) | ((data_type & 0x07) << 3) | (((len >> 8) as u8) & 0x07),
        (len & 0xff) as u8,
    ]
}

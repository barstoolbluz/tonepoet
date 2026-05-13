//! Shared test-only helpers for synthesizing SACD sectors. Used by
//! both `frame::tests` and `extract::tests`.

#![cfg(test)]

use crate::frame::Timecode;
use crate::iso_reader::SECTOR_SIZE;
use std::fs::File;
use std::io::Write;

/// Packet data_type for audio packets (per ScarletBook spec).
pub(crate) const DATA_TYPE_AUDIO: u8 = 2;

/// Build a synthetic *uncompressed* DSD audio sector with a single
/// packet of `payload`. `frame_start=true` emits a frame_info entry
/// with `tc`; `frame_start=false` produces a continuation packet (no
/// frame_info, no timecode). Pads the rest of the 2048-byte sector
/// with zeros.
pub(crate) fn synth_audio_sector(
    frame_start: bool,
    payload: &[u8],
    tc: Timecode,
) -> Vec<u8> {
    let mut s = vec![0u8; SECTOR_SIZE as usize];
    let frame_info_count: u8 = if frame_start { 1 } else { 0 };
    // dst=0, reserved=0, frame_info_count, packet_info_count=1.
    s[0] = (frame_info_count << 2) | (1 << 5);

    let plen = payload.len() as u16;
    let fs_bit = if frame_start { 1u8 << 7 } else { 0 };
    s[1] = fs_bit | (DATA_TYPE_AUDIO << 3) | ((plen >> 8) as u8 & 0x07);
    s[2] = (plen & 0xFF) as u8;
    let mut off = 3usize;

    if frame_start {
        s[off] = tc.minutes;
        s[off + 1] = tc.seconds;
        s[off + 2] = tc.frames;
        off += 3;
    }

    s[off..off + payload.len()].copy_from_slice(payload);
    s
}

/// Build a synthetic *DST-encoded* DSD audio sector with a single
/// frame_start packet. `sector_count` ∈ [0, 31] is what the
/// frame_info byte 3 encodes — when the audio packet is processed
/// the reader decrements this to 0 (frame complete).
pub(crate) fn synth_dst_sector(
    payload: &[u8],
    channel_count: u8,
    sector_count: u8,
    tc: Timecode,
) -> Vec<u8> {
    assert!(sector_count <= 31, "sector_count is a 5-bit field");
    let mut s = vec![0u8; SECTOR_SIZE as usize];
    // dst=1, reserved=0, frame_info_count=1, packet_info_count=1.
    s[0] = 1 | (1 << 2) | (1 << 5);

    let plen = payload.len() as u16;
    s[1] = (1 << 7) | (DATA_TYPE_AUDIO << 3) | ((plen >> 8) as u8 & 0x07);
    s[2] = (plen & 0xFF) as u8;

    // frame_info (4 bytes for DST): timecode + channel/sector byte.
    s[3] = tc.minutes;
    s[4] = tc.seconds;
    s[5] = tc.frames;
    // Byte 3 bit layout per the C reference:
    //   bit 0 = channel_bit_3 (set for 5ch)
    //   bit 1 = channel_bit_2 (set for 6ch)
    //   bits 2..6 = sector_count
    //   bit 7 = channel_bit_1 (unused in count derivation)
    let (cb3, cb2) = match channel_count {
        5 => (1u8, 0u8),
        6 => (0u8, 1u8),
        _ => (0u8, 0u8),
    };
    s[6] = cb3 | (cb2 << 1) | (sector_count << 2);

    s[7..7 + payload.len()].copy_from_slice(payload);
    s
}

/// Build a *continuation* sector (no frame_start, no frame_info)
/// carrying a single audio packet of `payload`. Used for frames that
/// span multiple sectors after the initial frame_start sector.
pub(crate) fn synth_continuation_sector(payload: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; SECTOR_SIZE as usize];
    // dst=0, frame_info_count=0, packet_info_count=1.
    s[0] = 1 << 5;
    let plen = payload.len() as u16;
    s[1] = (DATA_TYPE_AUDIO << 3) | ((plen >> 8) as u8 & 0x07);
    s[2] = (plen & 0xFF) as u8;
    s[3..3 + payload.len()].copy_from_slice(payload);
    s
}

/// Build a [`Timecode`] from a total 75fps frame count. Useful for
/// readable test setup when frame counts are the natural unit.
///
/// Formula matches sacd_extract's `TIME_FRAMECOUNT` macro (defined
/// in `libsacd/scarletbook.h`):
/// `total = minutes * 60 * 75 + seconds * 75 + frames`.
pub(crate) fn tc_at(frame_count: u32) -> Timecode {
    let m = (frame_count / (60 * 75)) as u8;
    let rem = frame_count % (60 * 75);
    let s = (rem / 75) as u8;
    let f = (rem % 75) as u8;
    Timecode { minutes: m, seconds: s, frames: f }
}

/// Write `sectors` to a temp file and return the TempDir (kept alive
/// by the caller to prevent cleanup before the test finishes).
pub(crate) fn write_iso(sectors: &[Vec<u8>]) -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("test.iso");
    let mut f = File::create(&path).unwrap();
    for s in sectors {
        assert_eq!(s.len(), SECTOR_SIZE as usize);
        f.write_all(s).unwrap();
    }
    td
}

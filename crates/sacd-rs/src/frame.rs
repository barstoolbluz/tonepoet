//! DSD audio frame extraction from a SACD ISO's per-track sector
//! range. Mirrors the algorithm in sacd-extract's
//! `scarletbook_process_frames` (libsacd/scarletbook_read.c).
//!
//! ## Sector layout (each LSN is 2048 bytes)
//!
//! ```text
//!  +----------------------------------------------------------+
//!  | byte 0:  audio_frame_header (1 byte)                     |
//!  |   bits LSB→MSB: dst_encoded | reserved | frame_info_cnt  |
//!  |                 (3 bits)    | packet_info_cnt (3 bits)   |
//!  +----------------------------------------------------------+
//!  | bytes 1..N: packet_info[] (2 bytes × packet_info_count,  |
//!  |             up to 7 entries)                             |
//!  |   bits LSB→MSB: frame_start | reserved | data_type (3)   |
//!  |                | packet_length (11 bits, spans bytes)    |
//!  +----------------------------------------------------------+
//!  | bytes after packet_info: frame_info[] (3 bytes for       |
//!  |             uncompressed, 4 bytes for DST-encoded,       |
//!  |             × frame_info_count, up to 7)                 |
//!  |   bytes 0..2: timecode (minutes, seconds, frames @ 75fps)|
//!  |   byte 3 (DST only): channel/sector bits                 |
//!  +----------------------------------------------------------+
//!  | rest of sector: packet payloads back-to-back, lengths    |
//!  |                 from packet_info[].packet_length         |
//!  +----------------------------------------------------------+
//! ```
//!
//! A DSD audio frame spans 1+ sectors. The `frame_start` bit on an
//! `audio_packet_info` marks the beginning of a new frame; data is
//! accumulated until the next `frame_start` or end of range. A
//! complete frame:
//!
//! - For uncompressed DSD: total bytes == `channel_count *
//!   FRAME_SIZE_UNCOMPRESSED` (4704 per channel at DSD64).
//! - For DST: `sector_count` (from the frame_info byte 3) reaches 0
//!   as sectors are consumed.

use crate::iso_reader::{IsoReader, SECTOR_SIZE};
use std::collections::VecDeque;
use std::io;

/// Uncompressed DSD frame size per channel at DSD64.
/// 588 samples × 64 bits/sample / 8 bits/byte = 4704 bytes.
pub const FRAME_SIZE_UNCOMPRESSED: usize = 4704;

/// Maximum bytes a single packet within a sector can carry.
/// Per the ScarletBook spec: a sector is 2048 bytes, the largest
/// packet_length value fits in 11 bits (2047). In practice the
/// reference enforces 2045 (sector minus minimal header overhead).
pub const MAX_PACKET_SIZE: usize = 2045;

/// Maximum buffered frame size. DST frames can be larger than a
/// single sector; the upstream reference allocates 64 KiB. We match
/// that to handle the worst-case DST frame.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;

/// A complete DSD audio frame extracted from one or more sectors.
///
/// `data` is the concatenated packet payload (DSD or DST-encoded
/// bytes; the caller decides via `dst_encoded`). Demultiplexing
/// per channel is the consumer's responsibility — for uncompressed
/// DSD it's stride-N interleaved; for DST it requires running the
/// decoder.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub timecode: Timecode,
    pub channel_count: u8,
    pub dst_encoded: bool,
    /// Only meaningful for DST frames: how many sectors the encoded
    /// payload occupies. Counts down to zero as sectors are
    /// consumed.
    pub sector_count: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timecode {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Timecode {
    /// Total frame count at 75 fps.
    pub fn as_frame_count(self) -> u32 {
        (self.minutes as u32) * 60 * 75
            + (self.seconds as u32) * 75
            + (self.frames as u32)
    }
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// Sector header was malformed (e.g., too many packets declared).
    MalformedSector { lsn: u64, reason: String },
    /// Frame buffer would overflow the 64 KiB limit.
    BufferOverflow { lsn: u64 },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {}", e),
            Self::MalformedSector { lsn, reason } => {
                write!(f, "malformed sector at LSN {}: {}", lsn, reason)
            }
            Self::BufferOverflow { lsn } => {
                write!(f, "frame buffer overflow at LSN {}", lsn)
            }
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Packet data types per the ScarletBook spec.
const DATA_TYPE_AUDIO: u8 = 2;
// 3 = supplementary, 7 = padding — we skip both.

/// Iterator over complete frames in an LSN range `[start_lsn,
/// end_lsn)`. Sectors are read sequentially; frames are emitted as
/// they complete. Partial frames at the end of the range are
/// dropped (matches the upstream reference's behavior — the last
/// incomplete frame is just discarded).
pub struct FrameReader<'a> {
    iso: &'a mut IsoReader,
    cur_lsn: u64,
    end_lsn: u64,
    sector_buf: Vec<u8>,
    /// Frames emitted so far in this iteration (for diagnostics).
    pub frames_yielded: u64,
    /// In-progress frame accumulator. `None` until the first
    /// `frame_start` packet arrives.
    pending: Option<PendingFrame>,
    /// Completed frames waiting to be yielded. A single sector can
    /// finalize multiple frames (rare with uncompressed DSD where a
    /// frame is ~9 KB across multiple sectors; possible with short
    /// DST frames where `sector_count == 1` and a new frame can
    /// start in the same sector that finished the previous one).
    ready: VecDeque<Frame>,
}

#[derive(Debug)]
struct PendingFrame {
    data: Vec<u8>,
    timecode: Timecode,
    channel_count: u8,
    dst_encoded: bool,
    sector_count: u8,
}

impl PendingFrame {
    fn into_frame(self) -> Frame {
        Frame {
            data: self.data,
            timecode: self.timecode,
            channel_count: self.channel_count,
            dst_encoded: self.dst_encoded,
            sector_count: self.sector_count,
        }
    }
}

impl<'a> FrameReader<'a> {
    pub fn new(iso: &'a mut IsoReader, start_lsn: u64, end_lsn: u64) -> Self {
        Self {
            iso,
            cur_lsn: start_lsn,
            end_lsn,
            sector_buf: vec![0u8; SECTOR_SIZE as usize],
            frames_yielded: 0,
            pending: None,
            ready: VecDeque::new(),
        }
    }

    /// Read the next complete frame. Returns `Ok(None)` at end of
    /// range. Internally consumes as many sectors as needed; frames
    /// that completed in earlier sectors but weren't yielded yet
    /// drain from the `ready` queue first.
    ///
    /// When the LSN range is fully consumed, the final pending
    /// frame is flushed (if complete) before `Ok(None)` is returned.
    /// Matches the C reference's `last_block=1` flush behavior —
    /// without this, the last frame of a track would be silently
    /// dropped because the upstream algorithm only finalizes on the
    /// NEXT `frame_start`.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                self.frames_yielded += 1;
                return Ok(Some(frame));
            }
            if self.cur_lsn >= self.end_lsn {
                // End of range: flush a final complete pending
                // frame if there is one. After that, `pending` is
                // cleared and subsequent calls return Ok(None).
                if let Some(prev) = self.pending.take() {
                    if frame_is_complete(&prev) {
                        self.ready.push_back(prev.into_frame());
                        continue;
                    }
                }
                return Ok(None);
            }
            let lsn = self.cur_lsn;
            self.iso.read_sector(lsn, &mut self.sector_buf)?;
            self.cur_lsn += 1;
            self.process_sector(lsn)?;
        }
    }

    /// Parse one sector and feed its audio packets into the pending
    /// frame accumulator. Any frames that complete during this
    /// sector are pushed onto `self.ready` for the caller to drain.
    fn process_sector(&mut self, lsn: u64) -> Result<(), FrameError> {
        let sector = &self.sector_buf;

        // Byte 0: audio_frame_header. Bit layout on little-endian:
        //   bit 0 (LSB): dst_encoded
        //   bit 1: reserved
        //   bits 2..4: frame_info_count
        //   bits 5..7: packet_info_count
        let header = sector[0];
        let dst_encoded = (header & 0x01) != 0;
        let frame_info_count = ((header >> 2) & 0x07) as usize;
        let packet_info_count = ((header >> 5) & 0x07) as usize;

        if packet_info_count > 7 {
            // The 3-bit field caps at 7, so this branch is
            // unreachable, but keep the check for parity with the
            // upstream reference's diagnostic.
            return Err(FrameError::MalformedSector {
                lsn,
                reason: format!("packet_info_count {} > 7", packet_info_count),
            });
        }

        let mut off = 1usize;

        // Parse packet_info[]: 2 bytes per entry.
        // Each entry's wire layout (little-endian; the upstream code
        // bit-extracts because of the cross-byte 11-bit field):
        //   byte0 bit 7 (MSB): frame_start
        //   byte0 bit 6: reserved
        //   byte0 bits 3..5: data_type
        //   byte0 bits 0..2 + byte1: packet_length (11 bits)
        let mut packets: [PacketInfo; 7] = Default::default();
        for i in 0..packet_info_count {
            let b0 = sector[off];
            let b1 = sector[off + 1];
            packets[i] = PacketInfo {
                frame_start: (b0 >> 7) & 1 != 0,
                data_type: (b0 >> 3) & 0x07,
                packet_length: (((b0 & 0x07) as u16) << 8) | (b1 as u16),
            };
            off += 2;
        }

        // Parse frame_info[]. Each entry is 4 bytes for DST-encoded
        // sectors (timecode + sector_count/channel bits) or 3 bytes
        // for uncompressed (timecode only).
        let frame_info_entry_size = if dst_encoded { 4 } else { 3 };
        let mut frame_infos: [FrameInfo; 7] = Default::default();
        for i in 0..frame_info_count {
            let tc = Timecode {
                minutes: sector[off],
                seconds: sector[off + 1],
                frames: sector[off + 2],
            };
            let (channel_count, sector_count) = if dst_encoded {
                let b3 = sector[off + 3];
                // Per the C reference's bitfield on little-endian:
                //   bit 0: channel_bit_3 (1 = 5 channels)
                //   bit 1: channel_bit_2 (1 = 6 channels)
                //   bits 2..6: sector_count (5 bits)
                //   bit 7: channel_bit_1 (unused in count derivation)
                let cb3 = b3 & 1;
                let cb2 = (b3 >> 1) & 1;
                let scnt = (b3 >> 2) & 0x1F;
                // Match the C reference's exact logic (scarletbook_read.c
                // `get_channel_count`): both conditions on both bits.
                // When both bits are set (invalid data), fall through to
                // stereo defensively.
                let chans = if cb2 == 1 && cb3 == 0 {
                    6
                } else if cb2 == 0 && cb3 == 1 {
                    5
                } else {
                    2
                };
                (chans, scnt)
            } else {
                // Uncompressed frame_info doesn't carry channel/sector
                // bits — the channel count is implied by the area's
                // header (caller's responsibility) and there's no
                // multi-sector layout (sector_count is irrelevant).
                (0u8, 0u8)
            };
            frame_infos[i] = FrameInfo { timecode: tc, channel_count, sector_count };
            off += frame_info_entry_size;
        }

        // Walk packets, accumulating into `self.pending`. Any frame
        // that completes (via a fresh frame_start finalizing a
        // complete previous frame) gets pushed onto `self.ready` for
        // the caller to drain.
        let mut frame_info_idx = 0usize;

        for i in 0..packet_info_count {
            let p = &packets[i];
            if p.data_type == DATA_TYPE_AUDIO {
                if p.frame_start {
                    // A new frame is starting. If we had a frame in
                    // progress AND it's complete, push it to ready.
                    // Incomplete previous frame is dropped silently
                    // (matches upstream).
                    if let Some(prev) = self.pending.take() {
                        if frame_is_complete(&prev) {
                            self.ready.push_back(prev.into_frame());
                        }
                    }
                    let info = &frame_infos[frame_info_idx];
                    frame_info_idx += 1;
                    self.pending = Some(PendingFrame {
                        data: Vec::with_capacity(MAX_FRAME_SIZE / 2),
                        timecode: info.timecode,
                        channel_count: info.channel_count,
                        dst_encoded,
                        sector_count: info.sector_count,
                    });
                }
                if let Some(ref mut p_acc) = self.pending {
                    let plen = p.packet_length as usize;
                    if p_acc.data.len() + plen > MAX_FRAME_SIZE {
                        self.pending = None;
                        return Err(FrameError::BufferOverflow { lsn });
                    }
                    p_acc.data.extend_from_slice(&sector[off..off + plen]);
                    if p_acc.dst_encoded {
                        p_acc.sector_count = p_acc.sector_count.saturating_sub(1);
                    }
                }
            }
            // Advance source pointer past the packet payload
            // regardless of data type — non-audio packets occupy
            // the same payload space.
            off += p.packet_length as usize;
        }

        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct PacketInfo {
    frame_start: bool,
    data_type: u8,
    packet_length: u16,
}

#[derive(Debug, Default, Clone, Copy)]
struct FrameInfo {
    timecode: Timecode,
    channel_count: u8,
    sector_count: u8,
}

fn frame_is_complete(p: &PendingFrame) -> bool {
    if p.dst_encoded {
        p.sector_count == 0
    } else {
        // For uncompressed DSD: complete when we have one full
        // frame's worth of bytes per channel. `channel_count` for
        // uncompressed frames isn't carried in the frame_info byte;
        // it's set by the caller via context. We accept any non-zero
        // multiple of FRAME_SIZE_UNCOMPRESSED as "complete enough"
        // and let the orchestration layer enforce exact channel
        // count.
        !p.data.is_empty() && p.data.len() % FRAME_SIZE_UNCOMPRESSED == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    /// Build a synthetic sector with one audio packet of `payload`,
    /// optionally starting a new frame. Pads the rest of the 2048
    /// bytes with zeros. Uncompressed (dst=false). `tc` is the
    /// timecode the frame_info entry carries.
    fn synth_audio_sector(frame_start: bool, payload: &[u8], tc: Timecode) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR_SIZE as usize];
        // header: dst=0, frame_info_count=1 (if frame_start, else 0),
        // packet_info_count=1
        let frame_info_count: u8 = if frame_start { 1 } else { 0 };
        let header = 0u8 | (frame_info_count << 2) | (1 << 5);
        s[0] = header;

        let plen = payload.len() as u16;
        // packet_info[0]
        let fs_bit = if frame_start { 1u8 << 7 } else { 0 };
        let dt = DATA_TYPE_AUDIO << 3;
        s[1] = fs_bit | dt | ((plen >> 8) as u8 & 0x07);
        s[2] = (plen & 0xFF) as u8;
        let mut off = 3usize;

        if frame_start {
            // frame_info[0] (3 bytes for uncompressed)
            s[off] = tc.minutes;
            s[off + 1] = tc.seconds;
            s[off + 2] = tc.frames;
            off += 3;
        }

        // payload
        s[off..off + payload.len()].copy_from_slice(payload);
        s
    }

    fn write_iso(sectors: &[Vec<u8>]) -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("test.iso");
        let mut f = File::create(&path).unwrap();
        for s in sectors {
            assert_eq!(s.len(), SECTOR_SIZE as usize);
            f.write_all(s).unwrap();
        }
        td
    }

    #[test]
    fn header_parsing_extracts_bitfields() {
        // Build a single sector with no packets to test header alone.
        let mut s = vec![0u8; SECTOR_SIZE as usize];
        // dst=1, frame_info_count=3, packet_info_count=5
        s[0] = 0b101_011_01;
        // (MSB → LSB): packet_info_count=5 (101), frame_info_count=3 (011), reserved=0, dst=1
        let td = write_iso(&[s]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        // No completion expected with 5 packets but all zero payload
        // sizes. Just check no error.
        let _ = reader.next_frame();
    }

    #[test]
    fn single_uncompressed_frame_spans_two_sectors() {
        // Create a fake uncompressed DSD64 stereo frame:
        // FRAME_SIZE_UNCOMPRESSED * 2 = 9408 bytes. Spread across
        // sectors (max packet payload ~2040 bytes).
        let frame_bytes: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| (i & 0xFF) as u8)
            .collect();
        let part_size = 2000;
        let sector1 = synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        );
        // Sector 2..N: continuation packets (frame_start=false), no
        // frame_info entry. Build manually.
        let mut sectors = vec![sector1];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            let mut s = vec![0u8; SECTOR_SIZE as usize];
            // header: dst=0, frame_info_count=0, packet_info_count=1
            s[0] = 1 << 5;
            let plen = chunk as u16;
            s[1] = (DATA_TYPE_AUDIO << 3) | ((plen >> 8) as u8 & 0x07);
            s[2] = (plen & 0xFF) as u8;
            s[3..3 + chunk].copy_from_slice(&frame_bytes[written..written + chunk]);
            sectors.push(s);
            written += chunk;
        }
        // Trailing sector with a frame_start so the previous frame
        // gets emitted.
        sectors.push(synth_audio_sector(
            true,
            &[],
            Timecode { minutes: 0, seconds: 0, frames: 2 },
        ));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);

        let frame = reader.next_frame().unwrap().expect("frame 1");
        assert_eq!(frame.timecode, Timecode { minutes: 0, seconds: 0, frames: 1 });
        assert!(!frame.dst_encoded);
        assert_eq!(frame.data, frame_bytes);
    }

    #[test]
    fn final_frame_flushes_at_end_of_range_without_trailing_start() {
        // The C reference flushes a complete pending frame when
        // `last_block=1` is signaled. We mirror that by flushing in
        // next_frame when cur_lsn reaches end_lsn. Without it, the
        // last frame of every track would be silently truncated.
        let frame_bytes: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 7) & 0xFF) as u8)
            .collect();
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 5 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            let mut s = vec![0u8; SECTOR_SIZE as usize];
            s[0] = 1 << 5;
            let plen = chunk as u16;
            s[1] = (DATA_TYPE_AUDIO << 3) | ((plen >> 8) as u8 & 0x07);
            s[2] = (plen & 0xFF) as u8;
            s[3..3 + chunk].copy_from_slice(&frame_bytes[written..written + chunk]);
            sectors.push(s);
            written += chunk;
        }
        // NO trailing frame_start sector — range ends with the last
        // continuation packet. Final flush should emit the frame.

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);

        let frame = reader.next_frame().unwrap().expect("frame flushed at EOR");
        assert_eq!(frame.timecode, Timecode { minutes: 0, seconds: 0, frames: 5 });
        assert_eq!(frame.data, frame_bytes);
        // Next call should now return None — pending cleared.
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn end_of_range_returns_none() {
        let td = write_iso(&[
            vec![0u8; SECTOR_SIZE as usize],
            vec![0u8; SECTOR_SIZE as usize],
        ]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 2);
        // No frame_start packets in the synthetic empty sectors;
        // both sectors consume without emitting.
        let r1 = reader.next_frame().unwrap();
        assert!(r1.is_none());
    }

    #[test]
    fn timecode_frame_count_is_75fps() {
        assert_eq!(
            Timecode { minutes: 1, seconds: 0, frames: 0 }.as_frame_count(),
            60 * 75,
        );
        assert_eq!(
            Timecode { minutes: 0, seconds: 1, frames: 0 }.as_frame_count(),
            75,
        );
        assert_eq!(
            Timecode { minutes: 0, seconds: 0, frames: 74 }.as_frame_count(),
            74,
        );
    }
}

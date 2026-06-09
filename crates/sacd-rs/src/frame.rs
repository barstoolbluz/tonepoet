//! Structured DSD/DST audio frame extraction from SACD ISO sectors.
//!
//! This module implements the ScarletBook audio-sector grammar as a
//! small typed parser plus an explicit frame-assembly state machine.
//! It deliberately keeps the byte stream handed to callers unchanged:
//! uncompressed frames are concatenated packet payloads and DST frames
//! are raw DST payloads ready for `dst::decode_frame`.
//!
//! Formal spec audit index: `docs/scarlet_book_audit_map.md`, anchors
//! `SB-AUDIO-001` through `SB-AUDIO-015`.

use crate::iso_reader::{IsoReader, SECTOR_SIZE};
use std::collections::VecDeque;
use std::io;

/// Uncompressed DSD frame size per channel at DSD64.
/// 588 samples × 64 bits/sample / 8 bits/byte = 4704 bytes.
pub const FRAME_SIZE_UNCOMPRESSED: usize = 4704;

/// Maximum bytes a single packet within a sector can carry.
pub const MAX_PACKET_SIZE: usize = 2045;

/// Maximum assembled encoded frame size. Matches the long-standing
/// 64 KiB allocation used by the C implementations and avoids
/// unbounded growth on corrupt packet headers.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;

/// ScarletBook packet data type: audio payload.
pub const DATA_TYPE_AUDIO: u8 = 2;
/// ScarletBook packet data type: supplementary metadata payload.
pub const DATA_TYPE_SUPPLEMENTARY: u8 = 3;
/// ScarletBook packet data type: padding payload.
pub const DATA_TYPE_PADDING: u8 = 7;

/// Area frame-format nibble from the area TOC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-012
pub enum FrameFormat {
    /// DST-coded DSD.
    Dst,
    /// Reserved ScarletBook value 1.
    Reserved,
    /// Plain DSD, three frames in fourteen sectors.
    Dsd3In14,
    /// Plain DSD, three frames in sixteen sectors.
    Dsd3In16,
    /// Undocumented values observed on some pressings. Treated as
    /// plain DSD by extraction code, but kept distinct for reporting.
    Dsd4,
    Dsd5,
    Dsd6,
    Dsd7,
    /// Any value outside the known low-nibble range.
    Unknown(u8),
}

impl FrameFormat {
    pub fn from_nibble(n: u8) -> Self {
        match n & 0x0f {
            0 => Self::Dst,
            1 => Self::Reserved,
            2 => Self::Dsd3In14,
            3 => Self::Dsd3In16,
            4 => Self::Dsd4,
            5 => Self::Dsd5,
            6 => Self::Dsd6,
            7 => Self::Dsd7,
            other => Self::Unknown(other),
        }
    }

    pub fn is_dst_encoded(self) -> bool {
        matches!(self, Self::Dst)
    }

    pub fn as_nibble(self) -> u8 {
        match self {
            Self::Dst => 0,
            Self::Reserved => 1,
            Self::Dsd3In14 => 2,
            Self::Dsd3In16 => 3,
            Self::Dsd4 => 4,
            Self::Dsd5 => 5,
            Self::Dsd6 => 6,
            Self::Dsd7 => 7,
            Self::Unknown(n) => n & 0x0f,
        }
    }

    /// Sectors per uncompressed frame group as declared by the area
    /// format. DST is variable-rate; `None` is intentional.
    pub fn sectors_per_frame(self) -> Option<u32> {
        match self {
            Self::Dsd3In14 | Self::Dsd4 | Self::Dsd5 | Self::Dsd6 | Self::Dsd7 => Some(14),
            Self::Dsd3In16 => Some(16),
            Self::Dst | Self::Reserved | Self::Unknown(_) => None,
        }
    }
}

impl From<u8> for FrameFormat {
    fn from(value: u8) -> Self {
        Self::from_nibble(value)
    }
}

impl std::fmt::Display for FrameFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dst => f.write_str("DST"),
            Self::Reserved => f.write_str("reserved"),
            Self::Dsd3In14 => f.write_str("DSD-3-in-14"),
            Self::Dsd3In16 => f.write_str("DSD-3-in-16"),
            Self::Dsd4 => f.write_str("DSD-format-4"),
            Self::Dsd5 => f.write_str("DSD-format-5"),
            Self::Dsd6 => f.write_str("DSD-format-6"),
            Self::Dsd7 => f.write_str("DSD-format-7"),
            Self::Unknown(n) => write!(f, "unknown({})", n),
        }
    }
}

/// A complete DSD or DST audio frame extracted from one or more sectors.
#[derive(Debug, Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub timecode: Timecode,
    /// For DST this is derived from the frame-info channel bits unless
    /// the caller supplied an area-TOC count, in which case the area
    /// count wins after mismatch diagnostics. For uncompressed DSD it
    /// is the expected channel count when supplied, otherwise 0.
    pub channel_count: u8,
    pub dst_encoded: bool,
    /// Meaningful only for DST frames.
    pub sector_count: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-009, SB-TRL2-004
pub struct Timecode {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Timecode {
    /// Total SACD frame count at 75 fps.
    pub fn as_frame_count(self) -> u32 {
        (self.minutes as u32) * 60 * 75 + (self.seconds as u32) * 75 + self.frames as u32
    }

    pub fn is_normalized(self) -> bool {
        // Match sacd_extract's TIME_FRAMECOUNT treatment for raw SACD
        // time fields: the seconds byte contributes 75-frame units even
        // when it is greater than 59. The frame byte remains a sub-second
        // index in the 75 fps SACD clock.
        self.frames < 75
    }
}

/// First byte of each ScarletBook audio sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-001..SB-AUDIO-004
pub struct AudioFrameHeader {
    pub dst_encoded: bool,
    pub frame_info_count: u8,
    pub packet_info_count: u8,
}

impl AudioFrameHeader {
    pub fn from_byte(byte: u8) -> Self {
        Self {
            dst_encoded: (byte & 0x01) != 0,
            frame_info_count: (byte >> 2) & 0x07,
            packet_info_count: (byte >> 5) & 0x07,
        }
    }
}

/// Two-byte packet-info record preceding sector payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-005..SB-AUDIO-008
pub struct AudioPacketInfo {
    pub frame_start: bool,
    pub data_type: u8,
    pub packet_length: u16,
}

impl AudioPacketInfo {
    /// Fallible parse for callers that receive arbitrary slices.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 {
            return None;
        }
        Some(Self {
            frame_start: (bytes[0] & 0x80) != 0,
            data_type: (bytes[0] >> 3) & 0x07,
            packet_length: (((bytes[0] & 0x07) as u16) << 8) | bytes[1] as u16,
        })
    }
}

/// Frame-start metadata record. DST sectors carry a fourth byte with
/// sector-count and channel-hint bits; plain DSD records carry only
/// the timecode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-009..SB-AUDIO-011
pub struct AudioFrameInfo {
    pub timecode: Timecode,
    pub sector_count: u8,
    pub channel_bits: u8,
}

impl AudioFrameInfo {
    pub fn from_bytes(bytes: &[u8], dst_encoded: bool) -> Option<Self> {
        let need = if dst_encoded { 4 } else { 3 };
        if bytes.len() < need {
            return None;
        }
        Some(Self {
            timecode: Timecode {
                minutes: bytes[0],
                seconds: bytes[1],
                frames: bytes[2],
            },
            sector_count: if dst_encoded { (bytes[3] >> 2) & 0x1f } else { 0 },
            // Preserve the three documented channel bits, including
            // bit 7, even though channel-count derivation uses only
            // bits 0 and 1. This makes diagnostics auditable.
            channel_bits: if dst_encoded { bytes[3] & 0x83 } else { 0 },
        })
    }

    pub fn to_frame_count(self) -> u32 {
        self.timecode.as_frame_count()
    }

    /// Match sacd_extract's `get_channel_count`: bit2=6ch,
    /// bit3=5ch, everything else falls back to stereo.
    pub fn derived_channel_count(self) -> u8 {
        let channel_bit_3 = self.channel_bits & 0x01;
        let channel_bit_2 = (self.channel_bits >> 1) & 0x01;
        match (channel_bit_2, channel_bit_3) {
            (1, 0) => 6,
            (0, 1) => 5,
            _ => 2,
        }
    }
}

/// Inclusive/exclusive SACD timecode filter in 75-fps frame units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-013
pub struct FrameTimeFilter {
    pub start_frame: u32,
    pub end_frame: u32,
}

impl FrameTimeFilter {
    pub fn new(start_frame: u32, duration_frames: u32) -> Self {
        Self {
            start_frame,
            end_frame: start_frame.saturating_add(duration_frames),
        }
    }

    pub fn includes(self, timecode: Timecode) -> bool {
        let tc = timecode.as_frame_count();
        tc >= self.start_frame && tc < self.end_frame
    }
}

/// Kind of sector-level problem recovered by [`FrameReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryEventKind {
    /// Sector bytes were readable but did not satisfy the audio-sector grammar.
    MalformedSector,
    /// The ISO reader failed to read the requested sector.
    IoError,
}

impl std::fmt::Display for RecoveryEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedSector => f.write_str("malformed-sector"),
            Self::IoError => f.write_str("io-error"),
        }
    }
}

/// A durable per-sector recovery log entry.
///
/// Recovery mode is for damaged-disc salvage, so counters alone are
/// insufficient: callers need the exact LSN and parser/read error to
/// decide whether to trust, quarantine, or retry the resulting file.
#[derive(Debug, Clone, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-014
pub struct RecoveryEvent {
    /// Logical sector number skipped by recovery.
    pub lsn: u64,
    /// Whether the sector failed during ISO read or audio-sector parsing.
    pub kind: RecoveryEventKind,
    /// Human-readable error text, including the parser's sector context.
    pub error: String,
}

impl RecoveryEvent {
    pub fn new(lsn: u64, kind: RecoveryEventKind, error: impl Into<String>) -> Self {
        Self {
            lsn,
            kind,
            error: error.into(),
        }
    }
}

impl std::fmt::Display for RecoveryEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LSN {} {}: {}", self.lsn, self.kind, self.error)
    }
}


/// A dropped partial frame in recovery mode.
#[derive(Debug, Clone, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-015
pub struct DroppedFrameEvent {
    /// LSN at which the incomplete frame was detected: either the frame-start
    /// sector that superseded it, a damaged sector that forced recovery, or
    /// the exclusive end LSN for end-of-range flush drops.
    pub lsn: u64,
    pub timecode: Timecode,
    pub dst_encoded: bool,
    pub bytes: usize,
    pub expected_bytes: Option<usize>,
    pub reason: &'static str,
}

impl std::fmt::Display for DroppedFrameEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.expected_bytes {
            Some(expected) => write!(
                f,
                "LSN {} dropped incomplete {} frame {:02}:{:02}:{:02}: {} bytes, expected {}; {}",
                self.lsn,
                if self.dst_encoded { "DST" } else { "DSD" },
                self.timecode.minutes,
                self.timecode.seconds,
                self.timecode.frames,
                self.bytes,
                expected,
                self.reason,
            ),
            None => write!(
                f,
                "LSN {} dropped incomplete {} frame {:02}:{:02}:{:02}: {} bytes; {}",
                self.lsn,
                if self.dst_encoded { "DST" } else { "DSD" },
                self.timecode.minutes,
                self.timecode.seconds,
                self.timecode.frames,
                self.bytes,
                self.reason,
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// SB-AUDIT: SB-AUDIO-014..SB-AUDIO-015
/// SB-AUDIT: SB-AUDIO-001..SB-AUDIO-015
pub struct FrameReaderStats {
    pub sectors_read: u64,
    pub sectors_skipped: u64,
    pub malformed_sectors: u64,
    pub io_errors: u64,
    pub frames_emitted: u64,
    pub frames_filtered: u64,
    pub frames_dropped_incomplete: u64,
    pub channel_mismatches: u64,
    /// Number of sectors whose area-TOC frame format was unusable for
    /// DSD/DST classification in recovery mode.
    pub frame_format_mismatches: u64,
    pub invalid_timecodes: u64,
    pub bytes_emitted: u64,
    /// Per-sector recovery details. Length should match `sectors_skipped`.
    pub recovery_events: Vec<RecoveryEvent>,
    /// Incomplete frames dropped while recovery was enabled, including
    /// frame-buffer overflow drops. Length should match
    /// `frames_dropped_incomplete` whenever detailed drop context was
    /// available.
    pub dropped_frame_events: Vec<DroppedFrameEvent>,
}

impl FrameReaderStats {
    pub fn recovery_events(&self) -> &[RecoveryEvent] {
        &self.recovery_events
    }

    pub fn dropped_frame_events(&self) -> &[DroppedFrameEvent] {
        &self.dropped_frame_events
    }
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    MalformedSector { lsn: u64, reason: String },
    BufferOverflow { lsn: u64, attempted: usize, limit: usize },
    /// A frame was started but never completed. In normal extraction this
    /// is fatal; in recovery mode it is counted and reported in stats.
    IncompleteFrame {
        lsn: u64,
        timecode: Timecode,
        dst_encoded: bool,
        bytes: usize,
        expected_bytes: Option<usize>,
        reason: &'static str,
    },
    ChannelCountMismatch {
        lsn: u64,
        expected: u8,
        derived: u8,
        timecode: Timecode,
    },
    /// Frame-info timecode used non-normalized SACD values. In the
    /// strict/default path this is fatal; in recovery mode it is retained
    /// in integrity stats and the caller receives a salvaged report.
    InvalidTimecode {
        lsn: u64,
        timecode: Timecode,
    },
    /// Sector header compression state disagreed with the area TOC frame
    /// format. The area TOC is the authoritative structural type.
    FrameFormatMismatch {
        lsn: u64,
        area_format: FrameFormat,
        sector_dst_encoded: bool,
    },
    /// The area TOC declared a reserved or unknown frame format, so the
    /// reader cannot safely choose DST versus plain-DSD sector layout.
    UnsupportedFrameFormat {
        lsn: u64,
        area_format: FrameFormat,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {}", e),
            Self::MalformedSector { lsn, reason } => {
                write!(f, "malformed audio sector at LSN {}: {}", lsn, reason)
            }
            Self::BufferOverflow { lsn, attempted, limit } => write!(
                f,
                "frame buffer overflow at LSN {}: attempted {} bytes, limit {} bytes",
                lsn, attempted, limit
            ),
            Self::IncompleteFrame { lsn, timecode, dst_encoded, bytes, expected_bytes, reason } => {
                match expected_bytes {
                    Some(expected) => write!(
                        f,
                        "incomplete {} frame at LSN {} timecode {:02}:{:02}:{:02}: {} bytes, expected {}; {}",
                        if *dst_encoded { "DST" } else { "DSD" },
                        lsn,
                        timecode.minutes,
                        timecode.seconds,
                        timecode.frames,
                        bytes,
                        expected,
                        reason,
                    ),
                    None => write!(
                        f,
                        "incomplete {} frame at LSN {} timecode {:02}:{:02}:{:02}: {} bytes; {}",
                        if *dst_encoded { "DST" } else { "DSD" },
                        lsn,
                        timecode.minutes,
                        timecode.seconds,
                        timecode.frames,
                        bytes,
                        reason,
                    ),
                }
            }
            Self::ChannelCountMismatch { lsn, expected, derived, timecode } => write!(
                f,
                "channel-count mismatch at LSN {} timecode {:02}:{:02}:{:02}: area TOC={}, frame hint={}",
                lsn, timecode.minutes, timecode.seconds, timecode.frames, expected, derived
            ),
            Self::InvalidTimecode { lsn, timecode } => write!(
                f,
                "invalid SACD timecode at LSN {}: {:02}:{:02}:{:02} (seconds must be < 60 and frames < 75)",
                lsn, timecode.minutes, timecode.seconds, timecode.frames
            ),
            Self::FrameFormatMismatch { lsn, area_format, sector_dst_encoded } => write!(
                f,
                "frame-format mismatch at LSN {}: area TOC declares {}, sector header declares {}",
                lsn,
                area_format,
                if *sector_dst_encoded { "DST" } else { "plain DSD" },
            ),
            Self::UnsupportedFrameFormat { lsn, area_format } => write!(
                f,
                "unsupported area TOC frame format at LSN {}: {}",
                lsn, area_format,
            ),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// SB-AUDIT: SB-AUDIO-001..SB-AUDIO-015
/// Iterator over complete frames in an LSN range `[start_lsn, end_lsn)`.
pub struct FrameReader<'a> {
    iso: &'a mut IsoReader,
    cur_lsn: u64,
    end_lsn: u64,
    sector_buf: Vec<u8>,
    pending: Option<PendingFrame>,
    ready: VecDeque<Frame>,
    expected_channel_count: Option<u8>,
    expected_frame_format: Option<FrameFormat>,
    strict_channel_count: bool,
    time_filter: Option<FrameTimeFilter>,
    past_time_filter_end: bool,
    recover_sector_errors: bool,
    stats: FrameReaderStats,
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

#[derive(Debug)]
struct ParsedSector {
    dst_encoded: bool,
    packets: Vec<AudioPacketInfo>,
    frame_infos: Vec<AudioFrameInfo>,
    payload_offset: usize,
}

impl<'a> FrameReader<'a> {
    pub fn new(iso: &'a mut IsoReader, start_lsn: u64, end_lsn: u64) -> Self {
        Self {
            iso,
            cur_lsn: start_lsn,
            end_lsn,
            sector_buf: vec![0u8; SECTOR_SIZE as usize],
            pending: None,
            ready: VecDeque::new(),
            expected_channel_count: None,
            expected_frame_format: None,
            strict_channel_count: false,
            time_filter: None,
            past_time_filter_end: false,
            recover_sector_errors: false,
            stats: FrameReaderStats::default(),
        }
    }

    /// Area-TOC channel-count cross-check. In non-strict mode,
    /// mismatches are counted and the area value is used for downstream
    /// decode/writer compatibility. In strict mode they become errors.
    pub fn set_expected_channel_count(&mut self, channel_count: u8) {
        if channel_count > 0 {
            self.expected_channel_count = Some(channel_count);
        }
    }

    /// Area-TOC frame-format routing. When supplied, this value decides
    /// whether completed frames are plain DSD or DST. The sector header
    /// compression bit still controls only that sector's frame-info entry
    /// width.
    pub fn set_expected_frame_format(&mut self, frame_format: FrameFormat) {
        self.expected_frame_format = Some(frame_format);
    }

    pub fn set_strict_channel_count(&mut self, strict: bool) {
        self.strict_channel_count = strict;
    }

    pub fn set_timecode_filter(&mut self, start_frame: u32, duration_frames: u32) {
        self.time_filter = Some(FrameTimeFilter::new(start_frame, duration_frames));
        self.past_time_filter_end = false;
    }

    /// Continue after malformed sectors or read failures. The current
    /// partial frame is discarded on each skipped sector so bytes from
    /// different frames are never stitched together silently.
    pub fn set_recover_sector_errors(&mut self, recover: bool) {
        self.recover_sector_errors = recover;
    }

    pub fn stats(&self) -> FrameReaderStats {
        self.stats.clone()
    }

    /// Explicit end-of-range flush. Returns a complete pending frame if
    /// one is buffered, applies the time filter, and clears pending state.
    /// A trailing incomplete frame at the track boundary is normal — the
    /// TOC sector range rarely aligns to DSD frame boundaries. Record it
    /// in stats but never treat it as a hard error.
    pub fn flush(&mut self) -> Result<Option<Frame>, FrameError> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        if !frame_is_complete(&pending, self.expected_channel_count) {
            self.record_dropped_frame(
                self.end_lsn,
                &pending,
                "end of extraction range before frame completed",
            );
            return Ok(None);
        }
        let frame = pending.into_frame();
        if self.frame_selected(&frame) {
            self.stats.frames_emitted += 1;
            self.stats.bytes_emitted += frame.data.len() as u64;
            Ok(Some(frame))
        } else {
            self.stats.frames_filtered += 1;
            Ok(None)
        }
    }

    /// Read the next complete frame. Returns `Ok(None)` at end of range.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                self.stats.frames_emitted += 1;
                self.stats.bytes_emitted += frame.data.len() as u64;
                return Ok(Some(frame));
            }

            if self.cur_lsn >= self.end_lsn {
                return self.flush();
            }

            let lsn = self.cur_lsn;
            self.cur_lsn += 1;

            // Move the reusable sector buffer out of `self` before parsing.
            // Packet payload assembly may read ahead through `self.iso`, while
            // the main loop still returns here to process each LSN in order.
            let mut sector_buf = std::mem::take(&mut self.sector_buf);
            let read_result = self.iso.read_sector(lsn, &mut sector_buf);
            match read_result {
                Ok(()) => {
                    self.stats.sectors_read += 1;
                    let sector_result = self.process_sector_bytes(lsn, &sector_buf);
                    self.sector_buf = sector_buf;
                    match sector_result {
                        Ok(()) => {}
                        Err(err) if self.recover_sector_errors => {
                            if matches!(err, FrameError::FrameFormatMismatch { .. }) {
                                self.stats.frame_format_mismatches += 1;
                            }
                            self.stats.malformed_sectors += 1;
                            self.stats.sectors_skipped += 1;
                            self.stats.recovery_events.push(RecoveryEvent::new(
                                lsn,
                                RecoveryEventKind::MalformedSector,
                                err.to_string(),
                            ));
                            self.discard_recovery_state();
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(e) if self.recover_sector_errors => {
                    self.sector_buf = sector_buf;
                    self.stats.io_errors += 1;
                    self.stats.sectors_skipped += 1;
                    self.stats.recovery_events.push(RecoveryEvent::new(
                        lsn,
                        RecoveryEventKind::IoError,
                        e.to_string(),
                    ));
                    self.discard_recovery_state();
                }
                Err(e) => {
                    self.sector_buf = sector_buf;
                    return Err(FrameError::Io(e));
                }
            }
        }
    }

    fn discard_recovery_state(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.record_dropped_frame(
                self.cur_lsn.saturating_sub(1),
                &pending,
                "sector recovery discarded the in-progress frame",
            );
        }
        self.ready.clear();
    }

    fn process_sector_bytes(&mut self, lsn: u64, sector: &[u8]) -> Result<(), FrameError> {
        let parsed = parse_sector_header(sector, lsn, self.expected_frame_format)?;
        let mut payload_offset = parsed.payload_offset;
        let mut frame_info_idx = 0usize;

        for packet in parsed.packets.iter().copied() {
            let packet_len = packet.packet_length as usize;
            if packet_len > MAX_PACKET_SIZE {
                if self.can_skip_malformed_packet_after_time_window(&parsed.frame_infos) {
                    self.stats.frames_filtered += 1;
                    return Ok(());
                }
                return Err(FrameError::MalformedSector {
                    lsn,
                    reason: format!("packet length {} exceeds {}", packet_len, MAX_PACKET_SIZE),
                });
            }
            let packet_end = payload_offset.checked_add(packet_len).ok_or_else(|| {
                FrameError::MalformedSector {
                    lsn,
                    reason: "packet offset overflow".into(),
                }
            })?;

            match packet.data_type {
                DATA_TYPE_AUDIO => {
                    let needs_payload = self.prepare_audio_packet(
                        lsn,
                        parsed.dst_encoded,
                        packet,
                        &parsed.frame_infos,
                        &mut frame_info_idx,
                    )?;
                    if needs_payload {
                        let packet_data = self.read_packet_payload(lsn, sector, payload_offset, packet_len)?;
                        self.append_audio_packet_payload(lsn, &packet_data)?;
                    }
                }
                DATA_TYPE_SUPPLEMENTARY | DATA_TYPE_PADDING => {}
                _ => {}
            }

            payload_offset = packet_end;
        }

        Ok(())
    }

    fn read_packet_payload(
        &mut self,
        lsn: u64,
        sector: &[u8],
        payload_offset: usize,
        packet_len: usize,
    ) -> Result<Vec<u8>, FrameError> {
        if packet_len == 0 {
            return Ok(Vec::new());
        }

        let sector_size = SECTOR_SIZE as usize;
        let mut payload = Vec::with_capacity(packet_len);
        let mut absolute_offset = payload_offset;
        let mut remaining = packet_len;

        while remaining > 0 {
            let sector_delta = absolute_offset / sector_size;
            let offset = absolute_offset % sector_size;
            let take = remaining.min(sector_size - offset);
            let sector_lsn = lsn.checked_add(sector_delta as u64).ok_or_else(|| {
                FrameError::MalformedSector {
                    lsn,
                    reason: "packet lookahead LSN overflow".into(),
                }
            })?;

            if sector_lsn >= self.end_lsn {
                return Err(FrameError::MalformedSector {
                    lsn,
                    reason: format!(
                        "packet payload extends past extraction range: end LSN {}",
                        self.end_lsn
                    ),
                });
            }

            if sector_delta == 0 {
                let end = offset.checked_add(take).ok_or_else(|| FrameError::MalformedSector {
                    lsn,
                    reason: "packet slice overflow".into(),
                })?;
                if end > sector.len() {
                    return Err(FrameError::MalformedSector {
                        lsn,
                        reason: "packet payload starts beyond current sector".into(),
                    });
                }
                payload.extend_from_slice(&sector[offset..end]);
            } else {
                let mut lookahead = vec![0u8; sector_size];
                self.iso.read_sector(sector_lsn, &mut lookahead)?;
                payload.extend_from_slice(&lookahead[offset..offset + take]);
            }

            absolute_offset = absolute_offset.checked_add(take).ok_or_else(|| {
                FrameError::MalformedSector {
                    lsn,
                    reason: "packet offset overflow".into(),
                }
            })?;
            remaining -= take;
        }

        Ok(payload)
    }

    fn prepare_audio_packet(
        &mut self,
        lsn: u64,
        dst_encoded: bool,
        packet: AudioPacketInfo,
        frame_infos: &[AudioFrameInfo],
        frame_info_idx: &mut usize,
    ) -> Result<bool, FrameError> {
        if !packet.frame_start {
            return Ok(self.pending.is_some());
        }

        if let Some(prev) = self.pending.take() {
            self.finish_or_drop(
                lsn,
                prev,
                "new frame_start encountered before previous frame completed",
            )?;
        }

        let Some(info) = frame_infos.get(*frame_info_idx).copied() else {
            if self.can_skip_missing_frame_info_after_time_window() {
                self.stats.frames_filtered += 1;
                return Ok(false);
            }
            return Err(FrameError::MalformedSector {
                lsn,
                reason: format!(
                    "audio frame_start without frame_info (idx {}, count {})",
                    *frame_info_idx,
                    frame_infos.len()
                ),
            });
        };
        *frame_info_idx += 1;

        // Match sacd_extract's output trimming order: compute the absolute
        // frame count from the frame-info timecode first and discard
        // out-of-window frames before validating the BCD-like fields. This
        // lets garbage/lead-out frames outside the selected track interval be
        // filtered without forcing payload assembly or reporting integrity
        // loss for data that will never be emitted. Record when the stream has
        // advanced past the selected interval so later frame-start garbage that
        // carries no frame_info can be skipped without weakening in-window
        // integrity checks.
        let selected = self.timecode_selected(info.timecode);
        self.observe_timecode_for_filter_end(info.timecode);
        if !selected {
            self.stats.frames_filtered += 1;
            return Ok(false);
        }

        if !info.timecode.is_normalized() {
            if self.can_skip_after_selected_time_window() {
                self.stats.frames_filtered += 1;
                return Ok(false);
            }
            self.stats.invalid_timecodes += 1;
            if !self.recover_sector_errors {
                return Err(FrameError::InvalidTimecode {
                    lsn,
                    timecode: info.timecode,
                });
            }
        }

        let channel_count = if dst_encoded {
            let derived = info.derived_channel_count();
            if let Some(expected) = self.expected_channel_count {
                if derived != expected {
                    self.stats.channel_mismatches += 1;
                    if self.strict_channel_count {
                        return Err(FrameError::ChannelCountMismatch {
                            lsn,
                            expected,
                            derived,
                            timecode: info.timecode,
                        });
                    }
                }
                expected
            } else {
                derived
            }
        } else {
            self.expected_channel_count.unwrap_or(0)
        };

        self.pending = Some(PendingFrame {
            data: Vec::with_capacity(frame_initial_capacity(dst_encoded, channel_count)),
            timecode: info.timecode,
            channel_count,
            dst_encoded,
            sector_count: info.sector_count,
        });

        Ok(true)
    }

    fn append_audio_packet_payload(
        &mut self,
        lsn: u64,
        packet_data: &[u8],
    ) -> Result<(), FrameError> {
        if let Some(pending) = self.pending.as_ref() {
            let attempted = pending.data.len().saturating_add(packet_data.len());
            if attempted > MAX_FRAME_SIZE {
                if let Some(dropped) = self.pending.take() {
                    self.record_dropped_frame(lsn, &dropped, "frame buffer overflow");
                }
                return Err(FrameError::BufferOverflow {
                    lsn,
                    attempted,
                    limit: MAX_FRAME_SIZE,
                });
            }
        }

        if let Some(pending) = self.pending.as_mut() {
            pending.data.extend_from_slice(packet_data);
            if pending.dst_encoded {
                pending.sector_count = pending.sector_count.saturating_sub(1);
            }
        }

        if self
            .pending
            .as_ref()
            .is_some_and(|p| frame_is_complete(p, self.expected_channel_count))
        {
            if let Some(done) = self.pending.take() {
                self.finish_or_drop(lsn, done, "frame completed")?;
            }
        }

        Ok(())
    }

    fn finish_or_drop(
        &mut self,
        lsn: u64,
        pending: PendingFrame,
        reason: &'static str,
    ) -> Result<(), FrameError> {
        if !frame_is_complete(&pending, self.expected_channel_count) {
            return self.handle_incomplete_frame(lsn, pending, reason);
        }
        let frame = pending.into_frame();
        if self.frame_selected(&frame) {
            self.ready.push_back(frame);
        } else {
            self.stats.frames_filtered += 1;
        }
        Ok(())
    }

    fn handle_incomplete_frame(
        &mut self,
        lsn: u64,
        pending: PendingFrame,
        reason: &'static str,
    ) -> Result<(), FrameError> {
        if self.can_skip_after_selected_time_window()
            || self.pending_frame_outside_time_filter(&pending)
            || self.pending_frame_reaches_dynamic_time_tail(&pending)
        {
            self.stats.frames_filtered += 1;
            self.past_time_filter_end = true;
            return Ok(());
        }

        let error = FrameError::IncompleteFrame {
            lsn,
            timecode: pending.timecode,
            dst_encoded: pending.dst_encoded,
            bytes: pending.data.len(),
            expected_bytes: expected_complete_bytes(&pending, self.expected_channel_count),
            reason,
        };
        self.record_dropped_frame(lsn, &pending, reason);
        if self.recover_sector_errors {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn pending_frame_outside_time_filter(&mut self, pending: &PendingFrame) -> bool {
        let Some(filter) = self.time_filter else {
            return false;
        };
        self.observe_timecode_for_filter_end(pending.timecode);
        !filter.includes(pending.timecode)
    }

    fn pending_frame_reaches_dynamic_time_tail(&self, pending: &PendingFrame) -> bool {
        let Some(filter) = self.time_filter else {
            return false;
        };
        let emitted_or_ready = self
            .stats
            .frames_emitted
            .saturating_add(self.ready.len() as u64);
        if emitted_or_ready == 0 {
            return false;
        }
        let dynamic_end = filter.start_frame.saturating_add(emitted_or_ready as u32);
        pending.timecode.as_frame_count() >= dynamic_end
    }

    fn record_dropped_frame(&mut self, lsn: u64, pending: &PendingFrame, reason: &'static str) {
        self.stats.frames_dropped_incomplete += 1;
        self.stats.dropped_frame_events.push(DroppedFrameEvent {
            lsn,
            timecode: pending.timecode,
            dst_encoded: pending.dst_encoded,
            bytes: pending.data.len(),
            expected_bytes: expected_complete_bytes(pending, self.expected_channel_count),
            reason,
        });
    }

    fn observe_timecode_for_filter_end(&mut self, timecode: Timecode) {
        if let Some(filter) = self.time_filter {
            if timecode.as_frame_count() >= filter.end_frame {
                self.past_time_filter_end = true;
            }
        }
    }

    fn can_skip_missing_frame_info_after_time_window(&self) -> bool {
        self.can_skip_after_selected_time_window()
    }

    fn can_skip_after_selected_time_window(&self) -> bool {
        if self.time_filter.is_none() {
            return false;
        }
        if self.past_time_filter_end {
            return true;
        }
        let Some(filter) = self.time_filter else {
            return false;
        };
        let duration = filter.end_frame.saturating_sub(filter.start_frame) as u64;
        duration > 0
            && self
                .stats
                .frames_emitted
                .saturating_add(self.ready.len() as u64)
                >= duration
    }

    fn can_skip_malformed_packet_after_time_window(
        &mut self,
        frame_infos: &[AudioFrameInfo],
    ) -> bool {
        if self.can_skip_after_selected_time_window() {
            return true;
        }

        let Some(filter) = self.time_filter else {
            return false;
        };
        let Some(first_info) = frame_infos.first().copied() else {
            return false;
        };
        if first_info.timecode.as_frame_count() >= filter.end_frame {
            self.past_time_filter_end = true;
            return true;
        }
        false
    }

    fn frame_selected(&self, frame: &Frame) -> bool {
        self.timecode_selected(frame.timecode)
    }

    fn timecode_selected(&self, timecode: Timecode) -> bool {
        if self.past_time_filter_end {
            return false;
        }
        self.time_filter
            .map(|f| f.includes(timecode))
            .unwrap_or(true)
    }
}

fn parse_sector_header(
    sector: &[u8],
    lsn: u64,
    expected_frame_format: Option<FrameFormat>,
) -> Result<ParsedSector, FrameError> {
    if sector.len() < SECTOR_SIZE as usize {
        return Err(FrameError::MalformedSector {
            lsn,
            reason: format!("sector too short: {} bytes", sector.len()),
        });
    }

    let header = AudioFrameHeader::from_byte(sector[0]);
    let dst_encoded = frame_dst_routing(lsn, expected_frame_format, header.dst_encoded)?;
    let frame_info_uses_dst_width = header.dst_encoded;
    let mut offset = 1usize;

    let packet_count = header.packet_info_count as usize;
    let packet_info_bytes = packet_count.checked_mul(2).ok_or_else(|| FrameError::MalformedSector {
        lsn,
        reason: "packet_info size overflow".into(),
    })?;
    if offset + packet_info_bytes > sector.len() {
        return Err(FrameError::MalformedSector {
            lsn,
            reason: "packet_info table extends beyond sector".into(),
        });
    }

    let mut packets = Vec::with_capacity(packet_count);
    for _ in 0..packet_count {
        let packet = AudioPacketInfo::from_bytes(&sector[offset..offset + 2]).ok_or_else(|| {
            FrameError::MalformedSector {
                lsn,
                reason: "short packet_info".into(),
            }
        })?;
        packets.push(packet);
        offset += 2;
    }

    let frame_count = header.frame_info_count as usize;
    let frame_info_size = if frame_info_uses_dst_width { 4 } else { 3 };
    let frame_info_bytes = frame_count.checked_mul(frame_info_size).ok_or_else(|| {
        FrameError::MalformedSector {
            lsn,
            reason: "frame_info size overflow".into(),
        }
    })?;
    if offset + frame_info_bytes > sector.len() {
        return Err(FrameError::MalformedSector {
            lsn,
            reason: "frame_info table extends beyond sector".into(),
        });
    }

    let mut frame_infos = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let info = AudioFrameInfo::from_bytes(
            &sector[offset..offset + frame_info_size],
            frame_info_uses_dst_width,
        )
        .ok_or_else(|| FrameError::MalformedSector {
            lsn,
            reason: "short frame_info".into(),
        })?;
        frame_infos.push(info);
        offset += frame_info_size;
    }

    Ok(ParsedSector { dst_encoded, packets, frame_infos, payload_offset: offset })
}

fn frame_dst_routing(
    lsn: u64,
    expected_frame_format: Option<FrameFormat>,
    sector_dst_encoded: bool,
) -> Result<bool, FrameError> {
    let Some(format) = expected_frame_format else {
        return Ok(sector_dst_encoded);
    };

    match format {
        FrameFormat::Dst => Ok(true),
        FrameFormat::Dsd3In14
        | FrameFormat::Dsd3In16
        | FrameFormat::Dsd4
        | FrameFormat::Dsd5
        | FrameFormat::Dsd6
        | FrameFormat::Dsd7 => Ok(false),
        FrameFormat::Reserved | FrameFormat::Unknown(_) => {
            Err(FrameError::UnsupportedFrameFormat { lsn, area_format: format })
        }
    }
}

fn frame_initial_capacity(dst_encoded: bool, channel_count: u8) -> usize {
    if dst_encoded {
        4096
    } else if channel_count > 0 {
        FRAME_SIZE_UNCOMPRESSED * channel_count as usize
    } else {
        FRAME_SIZE_UNCOMPRESSED * 2
    }
}

fn expected_complete_bytes(p: &PendingFrame, expected_channel_count: Option<u8>) -> Option<usize> {
    if p.dst_encoded {
        None
    } else if let Some(channels) = expected_channel_count.filter(|&n| n > 0) {
        Some(FRAME_SIZE_UNCOMPRESSED * channels as usize)
    } else {
        None
    }
}

fn frame_is_complete(p: &PendingFrame, expected_channel_count: Option<u8>) -> bool {
    if p.dst_encoded {
        p.sector_count == 0 && !p.data.is_empty()
    } else if let Some(channels) = expected_channel_count.filter(|&n| n > 0) {
        p.data.len() == FRAME_SIZE_UNCOMPRESSED * channels as usize
    } else {
        !p.data.is_empty() && p.data.len() % FRAME_SIZE_UNCOMPRESSED == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{synth_audio_sector, synth_continuation_sector, write_iso};


    fn packet_info_bytes(frame_start: bool, data_type: u8, packet_length: u16) -> [u8; 2] {
        assert!(packet_length <= 0x07ff);
        [
            (if frame_start { 0x80 } else { 0 })
                | ((data_type & 0x07) << 3)
                | (((packet_length >> 8) as u8) & 0x07),
            packet_length as u8,
        ]
    }

    fn packet_start_sector(dst_bit: bool, frame_info: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];
        sector[0] = (if dst_bit { 1 } else { 0 }) | (1 << 2) | (1 << 5);
        sector[1..3].copy_from_slice(&packet_info_bytes(true, DATA_TYPE_AUDIO, payload.len() as u16));
        let payload_offset = 3 + frame_info.len();
        sector[3..payload_offset].copy_from_slice(frame_info);
        sector[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        sector
    }

    #[test]
    fn header_parsing_extracts_bitfields() {
        let header = AudioFrameHeader::from_byte(0b101_011_01);
        assert!(header.dst_encoded);
        assert_eq!(header.frame_info_count, 3);
        assert_eq!(header.packet_info_count, 5);
    }

    #[test]
    fn packet_info_parsing_extracts_bitfields() {
        let p = AudioPacketInfo::from_bytes(&[0b1001_0011, 0x7f]).unwrap();
        assert!(p.frame_start);
        assert_eq!(p.data_type, 2);
        assert_eq!(p.packet_length, 0x037f);
    }

    #[test]
    fn frame_format_covers_known_nibbles() {
        assert_eq!(FrameFormat::from_nibble(0), FrameFormat::Dst);
        assert_eq!(FrameFormat::from_nibble(1), FrameFormat::Reserved);
        assert_eq!(FrameFormat::from_nibble(2), FrameFormat::Dsd3In14);
        assert_eq!(FrameFormat::from_nibble(3), FrameFormat::Dsd3In16);
        assert_eq!(FrameFormat::from_nibble(7), FrameFormat::Dsd7);
        assert_eq!(FrameFormat::from_nibble(15), FrameFormat::Unknown(15));
        assert_eq!(FrameFormat::Dsd3In14.sectors_per_frame(), Some(14));
        assert_eq!(FrameFormat::Dsd3In16.sectors_per_frame(), Some(16));
        assert_eq!(FrameFormat::Dst.sectors_per_frame(), None);
    }

    #[test]
    fn single_uncompressed_frame_spans_multiple_sectors() {
        let frame_bytes: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| (i & 0xff) as u8)
            .collect();
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);

        let frame = reader.next_frame().unwrap().expect("frame");
        assert_eq!(frame.timecode, Timecode { minutes: 0, seconds: 0, frames: 1 });
        assert!(!frame.dst_encoded);
        assert_eq!(frame.channel_count, 2);
        assert_eq!(frame.data, frame_bytes);
        assert!(reader.next_frame().unwrap().is_none());
    }


    #[test]
    fn cross_sector_packet_payload_reads_following_sector_bytes() {
        let first_payload_len = SECTOR_SIZE as usize - 6;
        let mut expected = Vec::new();
        expected.extend((0..first_payload_len).map(|idx| 0x40_u8.wrapping_add(idx as u8)));
        expected.extend([0xa1, 0xb2, 0xc3]);

        let mut first_sector = packet_start_sector(
            false,
            &[0, 0, 1],
            &expected[..first_payload_len],
        );
        first_sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            expected.len() as u16,
        ));
        let mut second_sector = vec![0u8; SECTOR_SIZE as usize];
        second_sector[..3].copy_from_slice(&expected[first_payload_len..]);

        let td = write_iso(&[first_sector.clone(), second_sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 2);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);

        reader.process_sector_bytes(0, &first_sector).unwrap();

        let pending = reader.pending.as_ref().expect("pending frame");
        assert!(!pending.dst_encoded);
        assert_eq!(pending.data, expected);
    }


    #[test]
    fn cross_sector_lookahead_does_not_skip_next_sector_scan() {
        let first_payload_len = SECTOR_SIZE as usize - 6;
        let mut expected = vec![0x5a; first_payload_len];
        expected.extend([0xa1, 0xb2, 0xc3]);

        let mut first_sector = packet_start_sector(false, &[0, 0, 1], &expected[..first_payload_len]);
        first_sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            expected.len() as u16,
        ));
        let mut second_sector = vec![0u8; SECTOR_SIZE as usize];
        second_sector[..3].copy_from_slice(&expected[first_payload_len..]);

        let td = write_iso(&[first_sector, second_sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 2);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_recover_sector_errors(true);

        assert!(reader.next_frame().unwrap().is_none());
        assert_eq!(reader.stats().sectors_read, 2);
    }

    #[test]
    fn dsd_area_uses_sector_dst_bit_only_for_frame_info_width() {
        let payload = [0x11, 0x22, 0x33, 0x44];
        let sector = packet_start_sector(true, &[0, 0, 1, 0xa5], &payload);

        let td = write_iso(&[sector.clone()]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);

        reader.process_sector_bytes(0, &sector).unwrap();

        let pending = reader.pending.as_ref().expect("pending frame");
        assert!(!pending.dst_encoded);
        assert_eq!(pending.data, payload);
        assert_eq!(pending.timecode, Timecode { minutes: 0, seconds: 0, frames: 1 });
    }

    #[test]
    fn time_filter_drops_out_of_range_frame_before_yield() {
        let frame_bytes: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| (i & 0xff) as u8)
            .collect();
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 10 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);
        reader.set_timecode_filter(20, 10);

        assert!(reader.next_frame().unwrap().is_none());
        assert_eq!(reader.stats().frames_filtered, 1);
    }

    #[test]
    fn out_of_filter_frame_start_does_not_read_cross_sector_payload_past_end_lsn() {
        let current_sector_payload_len = SECTOR_SIZE as usize - 6;
        let advertised_packet_len = current_sector_payload_len + 3;
        let current_sector_payload = vec![0x7c; current_sector_payload_len];
        let mut sector = packet_start_sector(
            false,
            &[0, 0, 10],
            &current_sector_payload,
        );
        sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            advertised_packet_len as u16,
        ));

        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(20, 10);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.sectors_read, 1);
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.malformed_sectors, 0);
        assert_eq!(stats.io_errors, 0);
    }


    #[test]
    fn out_of_filter_invalid_timecode_is_filtered_before_strict_validation() {
        let current_sector_payload_len = SECTOR_SIZE as usize - 6;
        let advertised_packet_len = current_sector_payload_len + 3;
        let current_sector_payload = vec![0x8d; current_sector_payload_len];
        let mut sector = packet_start_sector(
            false,
            &[77, 240, 241],
            &current_sector_payload,
        );
        sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            advertised_packet_len as u16,
        ));

        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 100);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.invalid_timecodes, 0);
        assert_eq!(stats.malformed_sectors, 0);
        assert_eq!(stats.frames_dropped_incomplete, 0);
    }

    #[test]
    fn missing_frame_info_after_time_filter_end_is_skipped() {
        let after_end = packet_start_sector(
            false,
            &[0, 0, 20],
            &[0x11, 0x22, 0x33, 0x44],
        );

        let mut no_info = vec![0u8; SECTOR_SIZE as usize];
        no_info[0] = 1 << 5; // one packet, zero frame_info entries
        no_info[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            4,
        ));
        no_info[3..7].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);

        let td = write_iso(&[after_end, no_info]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 2);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 10);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 2);
        assert_eq!(stats.malformed_sectors, 0);
        assert_eq!(stats.sectors_skipped, 0);
        assert_eq!(stats.frames_dropped_incomplete, 0);
    }


    #[test]
    fn missing_frame_info_after_emitting_filter_duration_is_skipped() {
        let frame_bytes: Vec<u8> = vec![0x42; FRAME_SIZE_UNCOMPRESSED * 2];
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 0 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let mut no_info = vec![0u8; SECTOR_SIZE as usize];
        no_info[0] = 1 << 5; // one packet, zero frame_info entries
        no_info[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            4,
        ));
        no_info[3..7].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
        sectors.push(no_info);

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 1);

        let frame = reader.next_frame().unwrap().expect("selected frame");
        assert_eq!(frame.data, frame_bytes);
        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.frames_emitted, 1);
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.malformed_sectors, 0);
        assert_eq!(stats.frames_dropped_incomplete, 0);
    }

    #[test]
    fn missing_frame_info_before_time_filter_end_still_fails() {
        let mut no_info = vec![0u8; SECTOR_SIZE as usize];
        no_info[0] = 1 << 5; // one packet, zero frame_info entries
        no_info[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            4,
        ));
        no_info[3..7].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);

        let td = write_iso(&[no_info]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 10);

        let err = reader.next_frame().expect_err("missing frame_info must fail in window");
        assert!(matches!(err, FrameError::MalformedSector { .. }));
    }

    #[test]
    fn malformed_packet_bounds_error_not_panic() {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];
        sector[0] = 1 << 5; // one packet, no frame info, uncompressed
        sector[1] = (DATA_TYPE_AUDIO << 3) | 0x07; // length high bits all set
        sector[2] = 0xff; // len 2047, larger than allowed/payload
        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        let err = reader.next_frame().expect_err("malformed sector");
        assert!(matches!(err, FrameError::MalformedSector { .. }));
    }

    #[test]
    fn malformed_packet_after_time_filter_end_is_skipped() {
        let mut sector = packet_start_sector(
            false,
            &[82, 41, 65],
            &[0xaa, 0xbb, 0xcc, 0xdd],
        );
        sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            2047,
        ));

        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 100);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.malformed_sectors, 0);
        assert_eq!(stats.sectors_skipped, 0);
        assert_eq!(stats.frames_dropped_incomplete, 0);
    }

    #[test]
    fn malformed_packet_before_time_filter_end_still_fails() {
        let mut sector = packet_start_sector(
            false,
            &[0, 0, 5],
            &[0xaa, 0xbb, 0xcc, 0xdd],
        );
        sector[1..3].copy_from_slice(&packet_info_bytes(
            true,
            DATA_TYPE_AUDIO,
            2047,
        ));

        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(0, 10);

        let err = reader.next_frame().expect_err("in-window malformed packet must fail");
        assert!(matches!(err, FrameError::MalformedSector { .. }));
    }

    #[test]
    fn incomplete_frame_after_time_filter_end_is_filtered_not_dropped() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 0);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(191_550, 9_375);

        let pending = PendingFrame {
            data: vec![0xaa, 0xbb, 0xcc, 0xdd],
            timecode: Timecode { minutes: 82, seconds: 41, frames: 65 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };

        reader
            .handle_incomplete_frame(
                1_737_218,
                pending,
                "new frame_start encountered before previous frame completed",
            )
            .unwrap();

        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.frames_dropped_incomplete, 0);
        assert_eq!(stats.dropped_frame_events.len(), 0);
    }

    #[test]
    fn incomplete_frame_inside_time_filter_still_fails() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 0);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(191_550, 9_375);

        let pending = PendingFrame {
            data: vec![0xaa, 0xbb, 0xcc, 0xdd],
            timecode: Timecode { minutes: 42, seconds: 34, frames: 0 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };

        let err = reader
            .handle_incomplete_frame(
                1_464_418,
                pending,
                "new frame_start encountered before previous frame completed",
            )
            .expect_err("in-window incomplete frame must fail");
        assert!(matches!(err, FrameError::IncompleteFrame { .. }));
        assert_eq!(reader.stats().frames_dropped_incomplete, 1);
    }

    #[test]
    fn incomplete_frame_at_dynamic_tail_is_filtered_not_dropped() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 0);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(313_684, 99_155);
        reader.stats.frames_emitted = 58_456;

        let pending = PendingFrame {
            data: vec![0xaa; 5_376],
            timecode: Timecode { minutes: 82, seconds: 41, frames: 65 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };

        reader
            .handle_incomplete_frame(
                1_737_218,
                pending,
                "new frame_start encountered before previous frame completed",
            )
            .unwrap();

        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 1);
        assert_eq!(stats.frames_dropped_incomplete, 0);
        assert_eq!(stats.dropped_frame_events.len(), 0);
    }

    #[test]
    fn incomplete_frame_after_dynamic_tail_is_filtered_even_with_lower_garbage_timecode() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 0);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(313_684, 99_155);
        reader.stats.frames_emitted = 58_456;

        let first_tail = PendingFrame {
            data: vec![0xaa; 5_376],
            timecode: Timecode { minutes: 82, seconds: 41, frames: 65 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };
        reader
            .handle_incomplete_frame(
                1_737_218,
                first_tail,
                "new frame_start encountered before previous frame completed",
            )
            .unwrap();

        let later_garbage = PendingFrame {
            data: vec![0xbb; 938],
            timecode: Timecode { minutes: 79, seconds: 98, frames: 43 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };
        reader
            .handle_incomplete_frame(
                1_737_661,
                later_garbage,
                "new frame_start encountered before previous frame completed",
            )
            .unwrap();

        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 2);
        assert_eq!(stats.frames_dropped_incomplete, 0);
        assert_eq!(stats.dropped_frame_events.len(), 0);
    }

    #[test]
    fn complete_frame_after_dynamic_tail_is_filtered_even_with_lower_garbage_timecode() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 0);
        reader.set_expected_channel_count(2);
        reader.set_expected_frame_format(FrameFormat::Dsd3In14);
        reader.set_timecode_filter(313_684, 99_155);
        reader.stats.frames_emitted = 58_456;

        let first_tail = PendingFrame {
            data: vec![0xaa; 5_376],
            timecode: Timecode { minutes: 82, seconds: 41, frames: 65 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };
        reader
            .handle_incomplete_frame(
                1_737_218,
                first_tail,
                "new frame_start encountered before previous frame completed",
            )
            .unwrap();

        let later_complete = PendingFrame {
            data: vec![0xcc; FRAME_SIZE_UNCOMPRESSED * 2],
            timecode: Timecode { minutes: 79, seconds: 98, frames: 43 },
            channel_count: 2,
            dst_encoded: false,
            sector_count: 0,
        };
        reader
            .finish_or_drop(1_737_662, later_complete, "frame completed")
            .unwrap();

        let stats = reader.stats();
        assert_eq!(stats.frames_filtered, 2);
        assert_eq!(stats.frames_emitted, 58_456);
        assert!(reader.ready.is_empty());
    }

    #[test]
    fn invalid_timecode_after_emitting_filter_duration_is_skipped() {
        let complete: Vec<u8> = vec![0x44; FRAME_SIZE_UNCOMPRESSED * 2];
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &complete[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 0 },
        )];
        let mut written = part_size;
        while written < complete.len() {
            let chunk = (complete.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&complete[written..written + chunk]));
            written += chunk;
        }
        sectors.push(synth_audio_sector(
            true,
            &[0x55; 2000],
            Timecode { minutes: 0, seconds: 0, frames: 186 },
        ));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);
        reader.set_timecode_filter(0, 1);

        let frame = reader.next_frame().unwrap().expect("selected frame");
        assert_eq!(frame.timecode, Timecode { minutes: 0, seconds: 0, frames: 0 });
        assert!(reader.next_frame().unwrap().is_none());
        assert_eq!(reader.stats().frames_filtered, 1);
        assert_eq!(reader.stats().invalid_timecodes, 0);
        assert_eq!(reader.stats().frames_dropped_incomplete, 0);
    }

    #[test]
    fn invalid_timecode_fails_in_strict_mode() {
        let frame_bytes: Vec<u8> = vec![0x55; FRAME_SIZE_UNCOMPRESSED * 2];
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 75 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);

        let err = reader.next_frame().expect_err("invalid frame component must fail strict mode");
        assert!(matches!(err, FrameError::InvalidTimecode { .. }));
        assert_eq!(reader.stats().invalid_timecodes, 1);
    }

    #[test]
    fn timecode_seconds_above_59_are_counted_not_rejected() {
        let frame_bytes: Vec<u8> = vec![0x77; FRAME_SIZE_UNCOMPRESSED * 2];
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 1, seconds: 74, frames: 0 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);
        reader.set_timecode_filter(0, 20_000);

        let frame = reader.next_frame().unwrap().expect("frame with raw seconds > 59");
        assert_eq!(frame.timecode.as_frame_count(), 1 * 60 * 75 + 74 * 75);
        assert_eq!(frame.data, frame_bytes);
        assert_eq!(reader.stats().invalid_timecodes, 0);
    }

    #[test]
    fn invalid_timecode_is_reported_in_salvage_mode() {
        let frame_bytes: Vec<u8> = vec![0x66; FRAME_SIZE_UNCOMPRESSED * 2];
        let part_size = 2000;
        let mut sectors = vec![synth_audio_sector(
            true,
            &frame_bytes[..part_size],
            Timecode { minutes: 0, seconds: 0, frames: 75 },
        )];
        let mut written = part_size;
        while written < frame_bytes.len() {
            let chunk = (frame_bytes.len() - written).min(part_size);
            sectors.push(synth_continuation_sector(&frame_bytes[written..written + chunk]));
            written += chunk;
        }

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_expected_channel_count(2);
        reader.set_recover_sector_errors(true);

        let frame = reader.next_frame().unwrap().expect("salvaged frame");
        assert_eq!(frame.data, frame_bytes);
        let stats = reader.stats();
        assert_eq!(stats.invalid_timecodes, 1);
        assert_eq!(stats.sectors_skipped, 0);
    }

    #[test]
    fn recovery_records_malformed_sector_lsn_and_error() {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];
        sector[0] = 1 << 5; // one packet, no frame info, uncompressed
        sector[1] = (DATA_TYPE_AUDIO << 3) | 0x07;
        sector[2] = 0xff; // len 2047, larger than allowed/payload

        let td = write_iso(&[sector]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_recover_sector_errors(true);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.sectors_skipped, 1);
        assert_eq!(stats.malformed_sectors, 1);
        assert_eq!(stats.recovery_events.len(), 1);
        assert_eq!(stats.recovery_events[0].lsn, 0);
        assert_eq!(stats.recovery_events[0].kind, RecoveryEventKind::MalformedSector);
        assert!(stats.recovery_events[0].error.contains("LSN 0"));
        assert!(stats.recovery_events[0].error.contains("packet length"));
    }

    #[test]
    fn recovery_records_dropped_frame_event_on_buffer_overflow() {
        // Frame-start sector overhead: 1 (header) + 2 (packet info) + 3 (frame info) = 6 bytes.
        // Continuation sector overhead: 1 (header) + 2 (packet info) = 3 bytes.
        // Sector size: 2048. Max payloads: 2042 (start), 2045 (continuation).
        let start_chunk = vec![0xaa; 2042];
        let cont_chunk = vec![0xaa; 2045];
        let mut sectors = vec![synth_audio_sector(
            true,
            &start_chunk,
            Timecode { minutes: 0, seconds: 0, frames: 1 },
        )];
        let mut total_payload = start_chunk.len();
        while total_payload <= MAX_FRAME_SIZE {
            sectors.push(synth_continuation_sector(&cont_chunk));
            total_payload += cont_chunk.len();
        }

        let overflow_lsn = (sectors.len() - 1) as u64;
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, sectors.len() as u64);
        reader.set_recover_sector_errors(true);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.sectors_skipped, 1);
        assert_eq!(stats.malformed_sectors, 1);
        assert_eq!(stats.frames_dropped_incomplete, 1);
        assert_eq!(stats.dropped_frame_events.len(), 1);
        assert_eq!(stats.dropped_frame_events[0].lsn, overflow_lsn);
        assert_eq!(stats.dropped_frame_events[0].reason, "frame buffer overflow");
        assert!(stats.dropped_frame_events[0].bytes <= MAX_FRAME_SIZE);
        assert_eq!(stats.recovery_events.len(), 1);
        assert_eq!(stats.recovery_events[0].lsn, overflow_lsn);
        assert!(stats.recovery_events[0].error.contains("frame buffer overflow"));
    }

    #[test]
    fn recovery_records_io_error_lsn_and_error() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut reader = FrameReader::new(&mut iso, 0, 1);
        reader.set_recover_sector_errors(true);

        assert!(reader.next_frame().unwrap().is_none());
        let stats = reader.stats();
        assert_eq!(stats.sectors_skipped, 1);
        assert_eq!(stats.io_errors, 1);
        assert_eq!(stats.recovery_events.len(), 1);
        assert_eq!(stats.recovery_events[0].lsn, 0);
        assert_eq!(stats.recovery_events[0].kind, RecoveryEventKind::IoError);
        assert!(!stats.recovery_events[0].error.is_empty());
    }

    #[test]
    fn timecode_frame_count_is_75fps() {
        assert_eq!(Timecode { minutes: 1, seconds: 0, frames: 0 }.as_frame_count(), 60 * 75);
        assert_eq!(Timecode { minutes: 0, seconds: 1, frames: 0 }.as_frame_count(), 75);
        assert_eq!(Timecode { minutes: 0, seconds: 0, frames: 74 }.as_frame_count(), 74);
    }
}

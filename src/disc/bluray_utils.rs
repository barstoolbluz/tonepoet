//! Blu-ray helper parsing that does not depend on libbluray.

use std::collections::{HashMap, HashSet};

const TS_PACKET_SIZE: usize = 188;
const MAX_PES_PREFIX_BYTES: usize = 9 + 255 + 4;

/// Parsed BD-ROM LPCM four-byte PES payload header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurayLpcmPesHeader {
    pub channel_assignment_code: u8,
    pub sample_rate_code: u8,
    pub bit_depth_code: u8,
    pub channels: u8,
    pub coded_channels: u8,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub channel_layout: &'static str,
}

/// Parser-level reason a target PID did not produce a valid LPCM PES payload
/// header during a probe window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlurayLpcmPesProbeFailureReason {
    PesStartNotFound,
    LpcmSubheaderIncomplete,
    InvalidPesPrefix,
    InvalidLpcmHeader { message: String },
}

/// Stateful MPEG-TS/PES probe for BD-ROM LPCM stream headers.
///
/// The probe accepts arbitrary byte chunks, preserves incomplete TS packets
/// between feeds, assembles PES prefix bytes per PID, and parses LPCM only
/// after the PES optional header plus the four-byte Blu-ray LPCM payload header
/// has arrived. It does not require the LPCM subheader to fit in a single TS
/// packet.
pub struct BlurayLpcmPesProbe {
    pids: HashSet<u16>,
    found: HashMap<u16, BlurayLpcmPesHeader>,
    pes_by_pid: HashMap<u16, PesPrefixAssembly>,
    failure_by_pid: HashMap<u16, BlurayLpcmPesProbeFailureReason>,
    pending_packet_bytes: Vec<u8>,
}

impl BlurayLpcmPesProbe {
    #[must_use]
    pub fn new<I>(pids: I) -> Self
    where
        I: IntoIterator<Item = u16>,
    {
        Self {
            pids: pids.into_iter().collect(),
            found: HashMap::new(),
            pes_by_pid: HashMap::new(),
            failure_by_pid: HashMap::new(),
            pending_packet_bytes: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.found.len() >= self.pids.len()
    }

    #[must_use]
    pub fn found(&self) -> &HashMap<u16, BlurayLpcmPesHeader> {
        &self.found
    }

    #[must_use]
    pub fn into_found(self) -> HashMap<u16, BlurayLpcmPesHeader> {
        self.found
    }

    #[must_use]
    pub fn failure_reason(&self, pid: u16) -> BlurayLpcmPesProbeFailureReason {
        if self.found.contains_key(&pid) {
            return BlurayLpcmPesProbeFailureReason::LpcmSubheaderIncomplete;
        }
        self.failure_by_pid
            .get(&pid)
            .cloned()
            .unwrap_or(BlurayLpcmPesProbeFailureReason::PesStartNotFound)
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || self.is_complete() {
            return;
        }

        let owned;
        let input: &[u8] = if self.pending_packet_bytes.is_empty() {
            bytes
        } else {
            owned = {
                let mut pending = std::mem::take(&mut self.pending_packet_bytes);
                pending.extend_from_slice(bytes);
                pending
            };
            &owned
        };

        let mut offset = 0usize;
        while input.len().saturating_sub(offset) >= TS_PACKET_SIZE && !self.is_complete() {
            if input[offset] != 0x47 {
                match find_next_ts_sync(&input[offset..]) {
                    Some(sync_offset) => {
                        offset += sync_offset;
                    }
                    None => {
                        offset = input.len().saturating_sub(TS_PACKET_SIZE - 1);
                        break;
                    }
                }
                continue;
            }

            self.process_packet(&input[offset..offset + TS_PACKET_SIZE]);
            offset += TS_PACKET_SIZE;
        }

        if offset < input.len() && !self.is_complete() {
            self.pending_packet_bytes.extend_from_slice(&input[offset..]);
            if self.pending_packet_bytes.len() > TS_PACKET_SIZE - 1 {
                let keep_from = self.pending_packet_bytes.len() - (TS_PACKET_SIZE - 1);
                self.pending_packet_bytes.drain(..keep_from);
            }
        }
    }

    fn process_packet(&mut self, packet: &[u8]) {
        debug_assert_eq!(packet.len(), TS_PACKET_SIZE);
        if packet.first().copied() != Some(0x47) {
            return;
        }

        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        if !self.pids.contains(&pid) || self.found.contains_key(&pid) {
            return;
        }

        let Some(payload_offset) = ts_payload_offset(packet) else {
            return;
        };
        let payload = &packet[payload_offset..];
        if payload.is_empty() {
            return;
        }

        let payload_unit_start = (packet[1] & 0x40) != 0;
        if payload_unit_start {
            let mut assembly = PesPrefixAssembly::new();
            assembly.push(payload);
            self.failure_by_pid
                .insert(pid, BlurayLpcmPesProbeFailureReason::LpcmSubheaderIncomplete);
            self.pes_by_pid.insert(pid, assembly);
        } else if let Some(assembly) = self.pes_by_pid.get_mut(&pid) {
            assembly.push(payload);
        } else {
            return;
        }

        match self.pes_by_pid.get(&pid).and_then(PesPrefixAssembly::try_parse_lpcm) {
            Some(Ok(header)) => {
                self.found.insert(pid, header);
                self.failure_by_pid.remove(&pid);
                self.pes_by_pid.remove(&pid);
            }
            Some(Err(reason)) => {
                self.failure_by_pid.insert(pid, reason);
                self.pes_by_pid.remove(&pid);
            }
            None => {}
        }
    }
}

struct PesPrefixAssembly {
    bytes: Vec<u8>,
}

impl PesPrefixAssembly {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(MAX_PES_PREFIX_BYTES),
        }
    }

    fn push(&mut self, payload: &[u8]) {
        if payload.is_empty() || self.bytes.len() >= MAX_PES_PREFIX_BYTES {
            return;
        }

        let remaining = MAX_PES_PREFIX_BYTES - self.bytes.len();
        self.bytes.extend_from_slice(&payload[..payload.len().min(remaining)]);
    }

    fn try_parse_lpcm(
        &self,
    ) -> Option<Result<BlurayLpcmPesHeader, BlurayLpcmPesProbeFailureReason>> {
        match parse_lpcm_header_from_pes_prefix(&self.bytes) {
            Some(result) => Some(result),
            None if self.bytes.len() >= MAX_PES_PREFIX_BYTES => Some(Err(
                BlurayLpcmPesProbeFailureReason::LpcmSubheaderIncomplete,
            )),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PesPrefixParseState {
    NeedMore,
    Failure(BlurayLpcmPesProbeFailureReason),
    Header(BlurayLpcmPesHeader),
}

/// Parse the first four bytes of a Blu-ray LPCM PES payload.
///
/// The coding follows FFmpeg's pcm_bluray parser: channel assignment lives in
/// the high nibble of byte 2, sample-rate code in the low nibble of byte 2,
/// and sample depth in the two high bits of byte 3.
pub fn parse_bluray_lpcm_pes_header(header: [u8; 4]) -> Result<BlurayLpcmPesHeader, String> {
    let channel_assignment_code = header[2] >> 4;
    let sample_rate_code = header[2] & 0x0f;
    let bit_depth_code = header[3] >> 6;

    let bit_depth = match bit_depth_code {
        1 => 16,
        2 => 20,
        3 => 24,
        other => return Err(format!("reserved Blu-ray LPCM bit-depth code {other}")),
    };

    let sample_rate = match sample_rate_code {
        1 => 48_000,
        4 => 96_000,
        5 => 192_000,
        other => return Err(format!("reserved Blu-ray LPCM sample-rate code {other}")),
    };

    let (channels, channel_layout) = match channel_assignment_code {
        1 => (1, "mono"),
        3 => (2, "stereo"),
        4 => (3, "3.0"),
        5 => (3, "2.1"),
        6 => (4, "4.0"),
        7 => (4, "2.2"),
        8 => (5, "5.0"),
        9 => (6, "5.1"),
        10 => (7, "7.0"),
        11 => (8, "7.1"),
        other => return Err(format!("reserved Blu-ray LPCM channel code {other}")),
    };

    Ok(BlurayLpcmPesHeader {
        channel_assignment_code,
        sample_rate_code,
        bit_depth_code,
        channels,
        coded_channels: align_to_even(channels),
        sample_rate,
        bit_depth,
        channel_layout,
    })
}

#[must_use]
pub fn bluray_lpcm_channel_layout_from_code(code: u8) -> Option<(&'static str, u8)> {
    match code {
        1 => Some(("mono", 1)),
        3 => Some(("stereo", 2)),
        4 => Some(("3.0", 3)),
        5 => Some(("2.1", 3)),
        6 => Some(("4.0", 4)),
        7 => Some(("2.2", 4)),
        8 => Some(("5.0", 5)),
        9 => Some(("5.1", 6)),
        10 => Some(("7.0", 7)),
        11 => Some(("7.1", 8)),
        _ => None,
    }
}

#[must_use]
pub const fn bluray_audio_rate_from_libbluray_code(rate: u8) -> Option<u32> {
    match rate {
        1 => Some(48_000),
        4 => Some(96_000),
        5 => Some(192_000),
        12 => Some(192_000),
        14 => Some(96_000),
        _ => None,
    }
}

#[must_use]
pub fn bluray_audio_layout_from_libbluray_code(format: u8) -> (Option<u8>, Option<String>) {
    match format {
        1 => (Some(1), Some("mono".to_string())),
        3 => (Some(2), Some("stereo".to_string())),
        6 => (None, Some("multichannel".to_string())),
        12 => (None, Some("combo".to_string())),
        _ => (None, None),
    }
}

/// Probe an MPEG-TS byte window for LPCM PES headers on selected PIDs.
///
/// This convenience wrapper uses the same stateful PES reassembly as the live
/// libbluray backend. It can parse LPCM headers fragmented across several TS
/// packets contained in `bytes`.
pub fn probe_bluray_lpcm_pes_headers_from_ts(
    bytes: &[u8],
    pids: &HashSet<u16>,
) -> HashMap<u16, BlurayLpcmPesHeader> {
    let mut probe = BlurayLpcmPesProbe::new(pids.iter().copied());
    probe.feed(bytes);
    probe.into_found()
}

fn parse_lpcm_header_from_pes_prefix(
    payload: &[u8],
) -> Option<Result<BlurayLpcmPesHeader, BlurayLpcmPesProbeFailureReason>> {
    match parse_lpcm_prefix_state(payload) {
        PesPrefixParseState::NeedMore => None,
        PesPrefixParseState::Failure(reason) => Some(Err(reason)),
        PesPrefixParseState::Header(header) => Some(Ok(header)),
    }
}

fn parse_lpcm_prefix_state(payload: &[u8]) -> PesPrefixParseState {
    if payload.len() < 9 {
        if [0x00, 0x00, 0x01].starts_with(payload) {
            return PesPrefixParseState::NeedMore;
        }
        return PesPrefixParseState::Failure(BlurayLpcmPesProbeFailureReason::InvalidPesPrefix);
    }

    if payload[0..3] != [0x00, 0x00, 0x01] {
        return PesPrefixParseState::Failure(BlurayLpcmPesProbeFailureReason::InvalidPesPrefix);
    }

    let pes_header_data_len = usize::from(payload[8]);
    let lpcm_header_offset = match 9usize.checked_add(pes_header_data_len) {
        Some(offset) => offset,
        None => return PesPrefixParseState::Failure(BlurayLpcmPesProbeFailureReason::InvalidPesPrefix),
    };
    if lpcm_header_offset + 4 > payload.len() {
        return PesPrefixParseState::NeedMore;
    }

    let header = [
        payload[lpcm_header_offset],
        payload[lpcm_header_offset + 1],
        payload[lpcm_header_offset + 2],
        payload[lpcm_header_offset + 3],
    ];

    match parse_bluray_lpcm_pes_header(header) {
        Ok(header) => PesPrefixParseState::Header(header),
        Err(message) => {
            PesPrefixParseState::Failure(BlurayLpcmPesProbeFailureReason::InvalidLpcmHeader {
                message,
            })
        }
    }
}

fn ts_payload_offset(packet: &[u8]) -> Option<usize> {
    if packet.len() != TS_PACKET_SIZE {
        return None;
    }
    let adaptation_field_control = (packet[3] >> 4) & 0x03;
    match adaptation_field_control {
        0 | 2 => None,
        1 => Some(4),
        3 => {
            let adaptation_len = usize::from(packet[4]);
            let payload = 5usize.checked_add(adaptation_len)?;
            (payload < TS_PACKET_SIZE).then_some(payload)
        }
        _ => None,
    }
}

fn find_next_ts_sync(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .position(|byte| *byte == 0x47)
        .filter(|offset| *offset > 0)
}

const fn align_to_even(channels: u8) -> u8 {
    if channels % 2 == 0 {
        channels
    } else {
        channels + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stereo_48khz_16_bit_header() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.coded_channels, 2);
        assert_eq!(parsed.channel_layout, "stereo");
        assert_eq!(parsed.sample_rate, 48_000);
        assert_eq!(parsed.bit_depth, 16);
    }

    #[test]
    fn parses_5_1_96khz_24_bit_header() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (9 << 4) | 4, 3 << 6]).unwrap();
        assert_eq!(parsed.channels, 6);
        assert_eq!(parsed.channel_layout, "5.1");
        assert_eq!(parsed.sample_rate, 96_000);
        assert_eq!(parsed.bit_depth, 24);
    }

    #[test]
    fn parses_20_bit_header_even_if_decode_path_may_not_accept_it() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 5, 2 << 6]).unwrap();
        assert_eq!(parsed.sample_rate, 192_000);
        assert_eq!(parsed.bit_depth, 20);
    }

    #[test]
    fn rejects_reserved_values() {
        assert!(parse_bluray_lpcm_pes_header([0, 0, (2 << 4) | 1, 1 << 6]).is_err());
        assert!(parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 2, 1 << 6]).is_err());
        assert!(parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 1, 0]).is_err());
    }

    #[test]
    fn scans_ts_packet_for_lpcm_pes_header() {
        let pid = 0x1100;
        let packet = ts_packet(pid, true, 0, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));

        let mut pids = HashSet::new();
        pids.insert(pid);
        let found = probe_bluray_lpcm_pes_headers_from_ts(&packet, &pids);
        assert_eq!(found.get(&pid).unwrap().bit_depth, 16);
    }

    #[test]
    fn reassembles_lpcm_pes_header_split_across_ts_packets() {
        let pid = 0x1100;
        let pes = pes_prefix([0, 0, (9 << 4) | 4, 3 << 6]);
        let packet_a = ts_packet(pid, true, 0, &pes[..10]);
        let packet_b = ts_packet(pid, false, 1, &pes[10..14]);
        let packet_c = ts_packet(pid, false, 2, &pes[14..]);
        let bytes = [packet_a.as_slice(), packet_b.as_slice(), packet_c.as_slice()].concat();

        let mut pids = HashSet::new();
        pids.insert(pid);
        let found = probe_bluray_lpcm_pes_headers_from_ts(&bytes, &pids);
        let header = found.get(&pid).unwrap();
        assert_eq!(header.channel_layout, "5.1");
        assert_eq!(header.sample_rate, 96_000);
        assert_eq!(header.bit_depth, 24);
    }

    #[test]
    fn stateful_probe_recovers_from_prefix_junk_before_sync() {
        let pid = 0x1100;
        let packet = ts_packet(pid, true, 0, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));
        let bytes = [b"junk".as_slice(), packet.as_slice()].concat();
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&bytes);

        assert_eq!(probe.found().get(&pid).unwrap().bit_depth, 16);
    }

    #[test]
    fn stateful_probe_handles_read_chunks_that_split_ts_packets() {
        let pid = 0x1100;
        let packet = ts_packet(pid, true, 0, &pes_prefix([0, 0, (3 << 4) | 5, 2 << 6]));
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&packet[..71]);
        assert!(probe.found().is_empty());
        probe.feed(&packet[71..]);

        let header = probe.found().get(&pid).unwrap();
        assert_eq!(header.sample_rate, 192_000);
        assert_eq!(header.bit_depth, 20);
    }

    #[test]
    fn payload_unit_start_replaces_prior_incomplete_pes_for_same_pid() {
        let pid = 0x1100;
        let bad_start = ts_packet(pid, true, 0, &[0x00, 0x00, 0x01, 0xbd, 0x00]);
        let good = ts_packet(pid, true, 1, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&bad_start);
        probe.feed(&good);

        assert_eq!(probe.found().get(&pid).unwrap().bit_depth, 16);
    }

    #[test]
    fn ignores_invalid_pes_prefix_and_waits_for_next_payload_start() {
        let pid = 0x1100;
        let invalid = ts_packet(pid, true, 0, &[0x12, 0x34, 0x56, 0x78]);
        let good = ts_packet(pid, true, 1, &pes_prefix([0, 0, (1 << 4) | 1, 1 << 6]));
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&invalid);
        assert!(probe.found().is_empty());
        probe.feed(&good);

        assert_eq!(probe.found().get(&pid).unwrap().channel_layout, "mono");
    }

    #[test]
    fn reports_parser_reason_when_pes_start_never_appears() {
        let pid = 0x1100;
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&ts_packet(0x1101, true, 0, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6])));

        assert_eq!(
            probe.failure_reason(pid),
            BlurayLpcmPesProbeFailureReason::PesStartNotFound
        );
    }

    #[test]
    fn reports_parser_reason_for_incomplete_lpcm_subheader() {
        let pid = 0x1100;
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&ts_packet(pid, true, 0, &[0x00, 0x00, 0x01, 0xbd, 0x00]));

        assert_eq!(
            probe.failure_reason(pid),
            BlurayLpcmPesProbeFailureReason::LpcmSubheaderIncomplete
        );
    }

    #[test]
    fn reports_parser_reason_for_invalid_lpcm_header() {
        let pid = 0x1100;
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&ts_packet(pid, true, 0, &pes_prefix([0, 0, (2 << 4) | 1, 1 << 6])));

        match probe.failure_reason(pid) {
            BlurayLpcmPesProbeFailureReason::InvalidLpcmHeader { message } => {
                assert!(message.contains("reserved Blu-ray LPCM channel code"));
            }
            other => panic!("expected invalid LPCM header, got {other:?}"),
        }
    }

    fn pes_prefix(lpcm_header: [u8; 4]) -> Vec<u8> {
        let mut pes = vec![
            0x00, 0x00, 0x01, 0xbd, // PES start code + private stream id
            0x00, 0x00, // unspecified PES packet length for probing purposes
            0x80, 0x80, 0x05, // marker flags + five-byte optional header
            0x21, 0x00, 0x01, 0x00, 0x01, // placeholder PTS bytes
        ];
        pes.extend_from_slice(&lpcm_header);
        pes
    }

    fn ts_packet(
        pid: u16,
        payload_unit_start: bool,
        continuity_counter: u8,
        payload: &[u8],
    ) -> [u8; TS_PACKET_SIZE] {
        assert!(payload.len() <= 184);
        let mut packet = [0xff; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        if payload_unit_start {
            packet[1] |= 0x40;
        }
        packet[2] = pid as u8;
        packet[3] = continuity_counter & 0x0f;

        if payload.len() == 184 {
            packet[3] |= 0x10;
            packet[4..].copy_from_slice(payload);
        } else {
            packet[3] |= 0x30;
            let adaptation_len = 183 - payload.len();
            packet[4] = adaptation_len as u8;
            let payload_offset = 5 + adaptation_len;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        }

        packet
    }
}

//! Lightweight single-PID Blu-ray MPEG-TS/PES demuxer.
//!
//! The realizer only needs one selected audio PID, so this module implements
//! strict 188-byte TS parsing, selected-PID continuity validation, and PES
//! reassembly without introducing a broad MPEG-TS dependency.

use std::collections::VecDeque;

use super::errors::ConvertError;

pub(crate) const TS_PACKET_SIZE: usize = 188;
pub(crate) const M2TS_PACKET_SIZE: usize = 192;
pub(crate) const M2TS_TP_EXTRA_SIZE: usize = 4;
pub(crate) const TS_RESYNC_CONFIRMATION_PACKETS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TsPacketFormat {
    /// Standard 188-byte TS packets with 0x47 at offset 0.
    StandardTs,
    /// 192-byte M2TS packets: 4-byte TP_extra_header, then a 188-byte TS packet.
    M2ts,
}

impl TsPacketFormat {
    pub(crate) fn packet_size(self) -> usize {
        match self {
            Self::StandardTs => TS_PACKET_SIZE,
            Self::M2ts => M2TS_PACKET_SIZE,
        }
    }

    pub(crate) fn sync_byte_offset(self) -> usize {
        match self {
            Self::StandardTs => 0,
            Self::M2ts => M2TS_TP_EXTRA_SIZE,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedPes<'a> {
    pub(crate) pts_90k: Option<u64>,
    pub(crate) payload: &'a [u8],
}

pub(crate) fn parse_lpcm_pes_packet(pes: &[u8]) -> Result<ParsedPes<'_>, ConvertError> {
    if pes.len() < 9 {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray selected audio PES is too short: {} byte(s)",
            pes.len()
        )));
    }
    if pes[0..3] != [0x00, 0x00, 0x01] {
        return Err(ConvertError::TrackValidation(
            "Blu-ray selected audio PES has an invalid start-code prefix".to_string(),
        ));
    }
    let stream_id = pes[3];
    if is_unstructured_pes_stream_id(stream_id) {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray selected LPCM PID carried unsupported PES stream id 0x{stream_id:02x}"
        )));
    }
    let declared_len = u16::from_be_bytes([pes[4], pes[5]]) as usize;
    let pes_len = if declared_len == 0 {
        pes.len()
    } else {
        let total = declared_len.checked_add(6).ok_or_else(|| {
            ConvertError::Realize("Blu-ray PES packet length overflow".to_string())
        })?;
        if total > pes.len() {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray selected audio PES declared {total} byte(s) but only {} byte(s) were reassembled",
                pes.len()
            )));
        }
        total
    };
    let pes = &pes[..pes_len];
    if pes.len() < 9 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray selected audio PES does not contain a full optional header".to_string(),
        ));
    }
    if (pes[6] & 0x30) != 0 {
        let scrambling = (pes[6] & 0x30) >> 4;
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray selected audio PES is scrambled (PES_scrambling_control={scrambling}); provide a decrypted/unprotected source"
        )));
    }

    let pts_dts_flags = (pes[7] >> 6) & 0x03;
    if pts_dts_flags == 0b01 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray selected audio PES has forbidden PTS_DTS_flags value 01".to_string(),
        ));
    }
    let header_data_len = usize::from(pes[8]);
    let optional_header_end = 9usize.checked_add(header_data_len).ok_or_else(|| {
        ConvertError::Realize("Blu-ray PES optional header offset overflow".to_string())
    })?;
    if optional_header_end > pes.len() {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray selected audio PES optional header extends past packet: header end {}, packet {} byte(s)",
            optional_header_end,
            pes.len()
        )));
    }
    let pts_90k = if matches!(pts_dts_flags, 0b10 | 0b11) {
        let required_header_bytes = if pts_dts_flags == 0b11 { 10 } else { 5 };
        if header_data_len < required_header_bytes || pes.len() < 9 + required_header_bytes {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray selected audio PES declares PTS_DTS_flags {:02b} but optional header contains only {} byte(s)",
                pts_dts_flags,
                header_data_len
            )));
        }
        Some(parse_pts_90k(&pes[9..14])?)
    } else {
        None
    };

    Ok(ParsedPes {
        pts_90k,
        payload: &pes[optional_header_end..],
    })
}

fn parse_pts_90k(bytes: &[u8]) -> Result<u64, ConvertError> {
    if bytes.len() != 5 {
        return Err(ConvertError::Realize(
            "internal PTS parser called with non-five-byte slice".to_string(),
        ));
    }
    if (bytes[0] & 0x01) == 0 || (bytes[2] & 0x01) == 0 || (bytes[4] & 0x01) == 0 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray selected audio PES has invalid PTS marker bits".to_string(),
        ));
    }
    let high = u64::from((bytes[0] >> 1) & 0x07) << 30;
    let mid = (u64::from(bytes[1]) << 22) | (u64::from((bytes[2] >> 1) & 0x7f) << 15);
    let low = (u64::from(bytes[3]) << 7) | u64::from((bytes[4] >> 1) & 0x7f);
    Ok(high | mid | low)
}

fn is_unstructured_pes_stream_id(stream_id: u8) -> bool {
    matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    )
}


#[derive(Debug, Clone)]
pub(crate) struct SelectedPesPacket {
    pub(crate) payload: Vec<u8>,
    pub(crate) continuity_start: u8,
    pub(crate) continuity_end: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuityDecision {
    Accept,
    DuplicateRetransmission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PesLengthState {
    NeedHeader,
    UnknownLength,
    Known(usize),
}

pub(crate) struct SelectedPidPesDemuxer {
    pid: u16,
    current_pes: Vec<u8>,
    started: bool,
    current_start_continuity_counter: Option<u8>,
    current_end_continuity_counter: Option<u8>,
    current_pes_length: PesLengthState,
    last_continuity_counter: Option<u8>,
    last_payload: Option<Vec<u8>>,
    completed: VecDeque<SelectedPesPacket>,
}

impl SelectedPidPesDemuxer {
    pub(crate) fn new(pid: u16) -> Self {
        Self {
            pid,
            current_pes: Vec::new(),
            started: false,
            current_start_continuity_counter: None,
            current_end_continuity_counter: None,
            current_pes_length: PesLengthState::NeedHeader,
            last_continuity_counter: None,
            last_payload: None,
            completed: VecDeque::new(),
        }
    }

    pub(crate) fn push_ts_packet(&mut self, packet: &[u8]) -> Result<Option<SelectedPesPacket>, ConvertError> {
        let parsed = parse_ts_packet(packet)?;
        if parsed.pid != self.pid {
            return Ok(self.completed.pop_front());
        }

        if parsed.discontinuity_indicator {
            self.reset_continuity_expectation();
            self.discard_current();
        }

        let Some(payload) = parsed.payload else {
            return Ok(self.completed.pop_front());
        };
        if payload.is_empty() {
            return Ok(self.completed.pop_front());
        }

        match self.validate_selected_payload_continuity(parsed.continuity_counter, payload)? {
            ContinuityDecision::DuplicateRetransmission => {
                return Ok(self.completed.pop_front());
            }
            ContinuityDecision::Accept => {}
        }

        if parsed.payload_unit_start {
            self.finalize_current_for_new_pusi()?;
            self.start_new_pes(parsed.continuity_counter);
        } else if !self.started {
            self.mark_selected_payload_accepted(parsed.continuity_counter, payload);
            return Ok(self.completed.pop_front());
        }

        self.current_end_continuity_counter = Some(parsed.continuity_counter);
        self.current_pes.extend_from_slice(payload);
        self.mark_selected_payload_accepted(parsed.continuity_counter, payload);
        self.refresh_current_pes_length_state()?;
        if self.current_pes_is_complete()? {
            let complete = self.take_current_pes()?;
            self.completed.push_back(complete);
        }

        Ok(self.completed.pop_front())
    }

    pub(crate) fn finish(&mut self) -> Result<Option<SelectedPesPacket>, ConvertError> {
        if let Some(complete) = self.completed.pop_front() {
            return Ok(Some(complete));
        }
        if self.started && !self.current_pes.is_empty() {
            if let PesLengthState::Known(expected) = self.current_pes_length {
                if self.current_pes.len() != expected {
                    let got = self.current_pes.len();
                    self.discard_current();
                    return Err(ConvertError::TrackValidation(format!(
                        "Blu-ray selected audio PES PID 0x{:04x} ended before declared length: expected {} byte(s), got {}",
                        self.pid,
                        expected,
                        got
                    )));
                }
            }
            Ok(Some(self.take_current_pes()?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn discard_current(&mut self) {
        self.current_pes.clear();
        self.started = false;
        self.current_start_continuity_counter = None;
        self.current_end_continuity_counter = None;
        self.current_pes_length = PesLengthState::NeedHeader;
    }

    fn reset_continuity_expectation(&mut self) {
        self.last_continuity_counter = None;
        self.last_payload = None;
    }

    fn start_new_pes(&mut self, continuity_counter: u8) {
        self.current_pes.clear();
        self.started = true;
        self.current_start_continuity_counter = Some(continuity_counter);
        self.current_end_continuity_counter = Some(continuity_counter);
        self.current_pes_length = PesLengthState::NeedHeader;
    }

    fn finalize_current_for_new_pusi(&mut self) -> Result<(), ConvertError> {
        if !self.started || self.current_pes.is_empty() {
            self.discard_current();
            return Ok(());
        }
        self.refresh_current_pes_length_state()?;
        if let PesLengthState::Known(expected) = self.current_pes_length {
            if self.current_pes.len() != expected {
                let got = self.current_pes.len();
                self.discard_current();
                return Err(ConvertError::TrackValidation(format!(
                    "Blu-ray selected audio PES PID 0x{:04x} was interrupted by a new payload start before its declared length: expected {} byte(s), got {}",
                    self.pid,
                    expected,
                    got
                )));
            }
        }
        let complete = self.take_current_pes()?;
        self.completed.push_back(complete);
        Ok(())
    }

    fn refresh_current_pes_length_state(&mut self) -> Result<(), ConvertError> {
        if self.current_pes_length != PesLengthState::NeedHeader {
            return Ok(());
        }
        if self.current_pes.len() < 6 {
            return Ok(());
        }
        if self.current_pes[0..3] != [0x00, 0x00, 0x01] {
            self.discard_current();
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray selected audio PID 0x{:04x} payload-unit-start does not begin with a PES start-code prefix",
                self.pid
            )));
        }
        let declared = u16::from_be_bytes([self.current_pes[4], self.current_pes[5]]) as usize;
        if declared == 0 {
            self.current_pes_length = PesLengthState::UnknownLength;
            return Ok(());
        }
        let total = declared.checked_add(6).ok_or_else(|| {
            ConvertError::Realize("Blu-ray selected audio PES declared length overflow".to_string())
        })?;
        if total < 9 {
            self.discard_current();
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray selected audio PES PID 0x{:04x} declares impossible length {} byte(s)",
                self.pid, total
            )));
        }
        self.current_pes_length = PesLengthState::Known(total);
        Ok(())
    }

    fn current_pes_is_complete(&mut self) -> Result<bool, ConvertError> {
        match self.current_pes_length {
            PesLengthState::Known(expected) => {
                if self.current_pes.len() > expected {
                    let got = self.current_pes.len();
                    self.discard_current();
                    Err(ConvertError::TrackValidation(format!(
                        "Blu-ray selected audio PES PID 0x{:04x} exceeded declared length: expected exactly {} byte(s), got at least {}",
                        self.pid, expected, got
                    )))
                } else {
                    Ok(self.current_pes.len() == expected)
                }
            }
            PesLengthState::NeedHeader | PesLengthState::UnknownLength => Ok(false),
        }
    }

    fn take_current_pes(&mut self) -> Result<SelectedPesPacket, ConvertError> {
        let continuity_start = self.current_start_continuity_counter.take().ok_or_else(|| {
            ConvertError::Realize("Blu-ray PES demuxer lost current continuity start".to_string())
        })?;
        let continuity_end = self.current_end_continuity_counter.take().unwrap_or(continuity_start);
        self.started = false;
        self.current_pes_length = PesLengthState::NeedHeader;
        Ok(SelectedPesPacket {
            payload: std::mem::take(&mut self.current_pes),
            continuity_start,
            continuity_end,
        })
    }

    fn validate_selected_payload_continuity(
        &mut self,
        continuity_counter: u8,
        payload: &[u8],
    ) -> Result<ContinuityDecision, ConvertError> {
        if let Some(last) = self.last_continuity_counter {
            let expected = (last + 1) & 0x0f;
            if continuity_counter == last {
                if self.last_payload.as_deref() == Some(payload) {
                    return Ok(ContinuityDecision::DuplicateRetransmission);
                }
                self.discard_current();
                return Err(ConvertError::TrackValidation(format!(
                    "Blu-ray TS duplicate continuity counter on PID 0x{:04x} carried different payload bytes: {}",
                    self.pid, continuity_counter
                )));
            }
            if continuity_counter != expected {
                self.discard_current();
                return Err(ConvertError::TrackValidation(format!(
                    "Blu-ray TS continuity counter gap on PID 0x{:04x}: expected {}, got {}",
                    self.pid, expected, continuity_counter
                )));
            }
        }
        Ok(ContinuityDecision::Accept)
    }

    fn mark_selected_payload_accepted(&mut self, continuity_counter: u8, payload: &[u8]) {
        self.last_continuity_counter = Some(continuity_counter);
        self.last_payload = Some(payload.to_vec());
    }
}

struct ParsedTsPacket<'a> {
    pid: u16,
    payload_unit_start: bool,
    discontinuity_indicator: bool,
    continuity_counter: u8,
    payload: Option<&'a [u8]>,
}

fn parse_ts_packet(packet: &[u8]) -> Result<ParsedTsPacket<'_>, ConvertError> {
    if packet.len() != TS_PACKET_SIZE {
        return Err(ConvertError::Realize(format!(
            "internal TS parser called with {} byte packet",
            packet.len()
        )));
    }
    if packet[0] != 0x47 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray TS packet sync byte mismatch".to_string(),
        ));
    }
    if (packet[1] & 0x80) != 0 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray TS packet has transport_error_indicator set".to_string(),
        ));
    }

    let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
    let payload_unit_start = (packet[1] & 0x40) != 0;
    let transport_scrambling_control = (packet[3] >> 6) & 0x03;
    if transport_scrambling_control != 0 {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray TS packet PID 0x{pid:04x} is scrambled/encrypted (transport_scrambling_control={transport_scrambling_control}); provide a decrypted/unprotected source"
        )));
    }

    let adaptation_field_control = (packet[3] >> 4) & 0x03;
    let continuity_counter = packet[3] & 0x0f;
    if adaptation_field_control == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray TS packet PID 0x{pid:04x} uses reserved adaptation_field_control=0"
        )));
    }

    let has_adaptation = matches!(adaptation_field_control, 2 | 3);
    let has_payload = matches!(adaptation_field_control, 1 | 3);
    let mut discontinuity_indicator = false;
    let payload_offset = if has_adaptation {
        let adaptation_len = usize::from(packet[4]);
        let payload_offset = 5usize.checked_add(adaptation_len).ok_or_else(|| {
            ConvertError::Realize("Blu-ray TS adaptation field offset overflow".to_string())
        })?;
        if payload_offset > TS_PACKET_SIZE {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray TS packet PID 0x{pid:04x} has adaptation field length {} past packet end",
                adaptation_len
            )));
        }
        if adaptation_len > 0 {
            discontinuity_indicator = (packet[5] & 0x80) != 0;
        }
        payload_offset
    } else {
        4
    };

    let payload = if has_payload && payload_offset < TS_PACKET_SIZE {
        Some(&packet[payload_offset..])
    } else {
        None
    };

    Ok(ParsedTsPacket {
        pid,
        payload_unit_start,
        discontinuity_indicator,
        continuity_counter,
        payload,
    })
}


#[allow(dead_code)]
pub(crate) fn find_next_ts_sync_at_cadence(bytes: &[u8]) -> Option<usize> {
    find_next_ts_sync_at_cadence_with_format(bytes, TsPacketFormat::StandardTs)
}

pub(crate) fn find_next_ts_sync_at_cadence_with_format(
    bytes: &[u8],
    format: TsPacketFormat,
) -> Option<usize> {
    let packet_size = format.packet_size();
    let sync_offset = format.sync_byte_offset();

    bytes.iter().enumerate().find_map(|(offset, _)| {
        let first_sync = offset.checked_add(sync_offset)?;
        if first_sync >= bytes.len() || bytes[first_sync] != 0x47 {
            return None;
        }
        for step in 1..TS_RESYNC_CONFIRMATION_PACKETS {
            let next = offset
                .checked_add(step * packet_size)?
                .checked_add(sync_offset)?;
            if next >= bytes.len() || bytes[next] != 0x47 {
                return None;
            }
        }
        Some(offset)
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    fn encode_pts(pts: u64) -> [u8; 5] {
        let pts = pts & ((1u64 << 33) - 1);
        [
            0x20 | (((pts >> 30) as u8 & 0x07) << 1) | 1,
            (pts >> 22) as u8,
            (((pts >> 15) as u8 & 0x7f) << 1) | 1,
            (pts >> 7) as u8,
            ((pts as u8 & 0x7f) << 1) | 1,
        ]
    }

    fn pes_packet(pts: u64, payload: &[u8]) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xBD];
        let packet_len = 3 + 5 + payload.len();
        pes.extend_from_slice(&(packet_len as u16).to_be_bytes());
        pes.extend_from_slice(&[0x80, 0x80, 0x05]);
        pes.extend_from_slice(&encode_pts(pts));
        pes.extend_from_slice(payload);
        pes
    }

    fn pes_packet_unknown_length(pts: u64, payload: &[u8]) -> Vec<u8> {
        let mut pes = pes_packet(pts, payload);
        pes[4] = 0;
        pes[5] = 0;
        pes
    }

    fn ts_packet_with_payload_opts(
        pid: u16,
        payload_unit_start: bool,
        continuity_counter: u8,
        payload: &[u8],
        discontinuity: bool,
        scrambling_control: u8,
        transport_error: bool,
    ) -> [u8; TS_PACKET_SIZE] {
        assert!(payload.len() <= TS_PACKET_SIZE - 4);
        let mut packet = [0xffu8; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        if payload_unit_start {
            packet[1] |= 0x40;
        }
        if transport_error {
            packet[1] |= 0x80;
        }
        packet[2] = pid as u8;
        let adaptation_field_control = if payload.len() == TS_PACKET_SIZE - 4 && !discontinuity {
            1u8
        } else {
            3u8
        };
        packet[3] = ((scrambling_control & 0x03) << 6)
            | (adaptation_field_control << 4)
            | (continuity_counter & 0x0f);
        if adaptation_field_control == 1 {
            packet[4..].copy_from_slice(payload);
        } else {
            let adaptation_len = (TS_PACKET_SIZE - 5) - payload.len();
            packet[4] = adaptation_len as u8;
            if adaptation_len > 0 {
                packet[5] = if discontinuity { 0x80 } else { 0x00 };
                for byte in &mut packet[6..5 + adaptation_len] {
                    *byte = 0xff;
                }
            }
            let payload_offset = 5 + adaptation_len;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        }
        packet
    }

    fn ts_packet_with_payload(
        pid: u16,
        payload_unit_start: bool,
        continuity_counter: u8,
        payload: &[u8],
    ) -> [u8; TS_PACKET_SIZE] {
        ts_packet_with_payload_opts(
            pid,
            payload_unit_start,
            continuity_counter,
            payload,
            false,
            0,
            false,
        )
    }

    fn ts_packet_adaptation_only(
        pid: u16,
        continuity_counter: u8,
        discontinuity: bool,
    ) -> [u8; TS_PACKET_SIZE] {
        let mut packet = [0xffu8; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        packet[2] = pid as u8;
        packet[3] = (2 << 4) | (continuity_counter & 0x0f);
        packet[4] = 183;
        packet[5] = if discontinuity { 0x80 } else { 0x00 };
        packet
    }

    fn m2ts_packet(
        ts_packet: &[u8; TS_PACKET_SIZE],
        arrival_time_stamp: u32,
    ) -> [u8; M2TS_PACKET_SIZE] {
        let mut packet = [0u8; M2TS_PACKET_SIZE];
        let tp_extra = arrival_time_stamp & 0x3fff_ffff;
        packet[..M2TS_TP_EXTRA_SIZE].copy_from_slice(&tp_extra.to_be_bytes());
        packet[M2TS_TP_EXTRA_SIZE..].copy_from_slice(ts_packet);
        packet
    }

    fn packetize_pes(
        pid: u16,
        start_cc: u8,
        pes: &[u8],
        chunk_sizes: &[usize],
    ) -> Vec<[u8; TS_PACKET_SIZE]> {
        let mut packets = Vec::new();
        let mut offset = 0usize;
        let mut cc = start_cc;
        for (index, chunk_size) in chunk_sizes.iter().enumerate() {
            let end = (offset + *chunk_size).min(pes.len());
            packets.push(ts_packet_with_payload(pid, index == 0, cc, &pes[offset..end]));
            offset = end;
            cc = (cc + 1) & 0x0f;
            if offset == pes.len() {
                break;
            }
        }
        if offset < pes.len() {
            packets.push(ts_packet_with_payload(pid, packets.is_empty(), cc, &pes[offset..]));
        }
        packets
    }

    fn collect_demuxed_pes(
        demuxer: &mut SelectedPidPesDemuxer,
        packets: &[[u8; TS_PACKET_SIZE]],
    ) -> Result<Vec<Vec<u8>>, ConvertError> {
        let mut out = Vec::new();
        for packet in packets {
            if let Some(pes) = demuxer.push_ts_packet(packet)? {
                out.push(pes.payload);
            }
        }
        if let Some(pes) = demuxer.finish()? {
            out.push(pes.payload);
        }
        Ok(out)
    }

    #[test]
    fn pts_parser_round_trips_33_bit_value() {
        let pts = 0x1abc_def0u64;
        assert_eq!(parse_pts_90k(&encode_pts(pts)).unwrap(), pts);
    }

    #[test]
    fn ts_demuxes_clean_selected_pid_stream_across_many_packets() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0x55; 700]);
        let packets = packetize_pes(pid, 0, &pes, &[80, 120, 184, 60, 184, 184]);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let out = collect_demuxed_pes(&mut demuxer, &packets).unwrap();
        assert_eq!(out, vec![pes]);
    }

    #[test]
    fn ts_selected_pid_adaptation_only_does_not_advance_continuity() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0; 64]);
        let first = ts_packet_with_payload(pid, true, 0, &pes[..12]);
        let adaptation_only = ts_packet_adaptation_only(pid, 9, false);
        let second = ts_packet_with_payload(pid, false, 1, &pes[12..]);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let out = collect_demuxed_pes(&mut demuxer, &[first, adaptation_only, second]).unwrap();
        assert_eq!(out, vec![pes]);
    }

    #[test]
    fn ts_discontinuity_indicator_resets_continuity_expectation() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0; 32]);
        let discontinuity = ts_packet_adaptation_only(pid, 3, true);
        let payload = ts_packet_with_payload(pid, true, 12, &pes);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let out = collect_demuxed_pes(&mut demuxer, &[discontinuity, payload]).unwrap();
        assert_eq!(out, vec![pes]);
    }

    #[test]
    fn ts_duplicate_identical_payload_does_not_duplicate_audio() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0x11; 128]);
        let first = ts_packet_with_payload(pid, true, 0, &pes[..50]);
        let duplicate = first;
        let second = ts_packet_with_payload(pid, false, 1, &pes[50..]);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let out = collect_demuxed_pes(&mut demuxer, &[first, duplicate, second]).unwrap();
        assert_eq!(out, vec![pes]);
    }

    #[test]
    fn ts_duplicate_different_payload_errors() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0x11; 64]);
        let first = ts_packet_with_payload(pid, true, 0, &pes[..30]);
        let mut different_payload = pes[..30].to_vec();
        different_payload[10] ^= 0xff;
        let duplicate_different = ts_packet_with_payload(pid, true, 0, &different_payload);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        demuxer.push_ts_packet(&first).unwrap();
        let err = demuxer.push_ts_packet(&duplicate_different).unwrap_err();
        assert!(err.to_string().contains("duplicate continuity counter"));
        assert!(err.to_string().contains("different payload"));
    }

    #[test]
    fn ts_scrambled_selected_pid_packet_errors() {
        let pid = 0x1100;
        let packet =
            ts_packet_with_payload_opts(pid, true, 0, &[0, 0, 1, 0xbd], false, 2, false);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let err = demuxer.push_ts_packet(&packet).unwrap_err();
        assert!(err.to_string().contains("scrambled/encrypted"));
    }

    #[test]
    fn ts_malformed_adaptation_length_errors() {
        let pid = 0x1100;
        let mut packet = [0xffu8; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        packet[2] = pid as u8;
        packet[3] = (3 << 4) | 0;
        packet[4] = 184;
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let err = demuxer.push_ts_packet(&packet).unwrap_err();
        assert!(err.to_string().contains("adaptation field length"));
    }

    #[test]
    fn ts_sync_loss_requires_repeated_188_byte_cadence_to_resync() {
        let pid = 0x1100;
        let mut bytes = vec![0u8; 32];
        bytes[5] = 0x47;
        let candidate_offset = bytes.len();
        let p0 = ts_packet_adaptation_only(pid, 0, false);
        let p1 = ts_packet_adaptation_only(pid, 1, false);
        let p2 = ts_packet_adaptation_only(pid, 2, false);
        bytes.extend_from_slice(&p0);
        bytes.extend_from_slice(&p1);
        bytes.extend_from_slice(&p2);

        assert_eq!(find_next_ts_sync_at_cadence(&bytes), Some(candidate_offset));
        assert_ne!(find_next_ts_sync_at_cadence(&bytes), Some(5));
    }

    #[test]
    fn m2ts_sync_loss_requires_repeated_192_byte_cadence_to_resync() {
        let pid = 0x1100;
        let mut bytes = vec![0u8; 19];
        bytes[0] = 0x47;
        bytes[7] = 0x47;
        let candidate_offset = bytes.len();
        let p0 = m2ts_packet(&ts_packet_adaptation_only(pid, 0, false), 0);
        let p1 = m2ts_packet(&ts_packet_adaptation_only(pid, 1, false), 1);
        let p2 = m2ts_packet(&ts_packet_adaptation_only(pid, 2, false), 2);
        bytes.extend_from_slice(&p0);
        bytes.extend_from_slice(&p1);
        bytes.extend_from_slice(&p2);

        assert_eq!(
            find_next_ts_sync_at_cadence_with_format(&bytes, TsPacketFormat::M2ts),
            Some(candidate_offset)
        );
        assert_ne!(
            find_next_ts_sync_at_cadence_with_format(&bytes, TsPacketFormat::M2ts),
            Some(0)
        );
        assert_ne!(
            find_next_ts_sync_at_cadence_with_format(&bytes, TsPacketFormat::M2ts),
            Some(7)
        );
    }

    #[test]
    fn m2ts_sync_loss_and_recovery_finds_valid_cadence_after_garbage() {
        let pid = 0x1100;
        let first = m2ts_packet(&ts_packet_adaptation_only(pid, 0, false), 0);
        let mut bytes = first.to_vec();
        bytes.extend_from_slice(&[0x00, 0x47, 0x11, 0x22, 0x33, 0x47, 0x44]);
        let candidate_offset = bytes.len();
        for cc in 1..=3 {
            let ts = ts_packet_adaptation_only(pid, cc, false);
            let m2ts = m2ts_packet(&ts, cc as u32);
            bytes.extend_from_slice(&m2ts);
        }

        let search = &bytes[M2TS_PACKET_SIZE + 1..];
        assert_eq!(
            find_next_ts_sync_at_cadence_with_format(search, TsPacketFormat::M2ts),
            Some(candidate_offset - M2TS_PACKET_SIZE - 1)
        );
    }

    #[test]
    fn pes_known_length_completes_exactly() {
        let pid = 0x1100;
        let pes = pes_packet(0, &[0x33; 64]);
        let packet = ts_packet_with_payload(pid, true, 0, &pes);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        let out = collect_demuxed_pes(&mut demuxer, &[packet]).unwrap();
        assert_eq!(out, vec![pes]);
    }

    #[test]
    fn pes_unknown_length_finalizes_on_next_pusi() {
        let pid = 0x1100;
        let unknown = pes_packet_unknown_length(0, &[0x44; 64]);
        let next = pes_packet(90, &[0x55; 16]);
        let first = ts_packet_with_payload(pid, true, 0, &unknown);
        let second = ts_packet_with_payload(pid, true, 1, &next);
        let mut demuxer = SelectedPidPesDemuxer::new(pid);

        assert!(demuxer.push_ts_packet(&first).unwrap().is_none());
        let completed = demuxer.push_ts_packet(&second).unwrap().unwrap();
        assert_eq!(completed.payload, unknown);
        let final_pes = demuxer.finish().unwrap().unwrap();
        assert_eq!(final_pes.payload, next);
    }
}

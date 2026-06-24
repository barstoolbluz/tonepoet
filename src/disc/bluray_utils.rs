//! Blu-ray source detection, mapping glue, and LPCM PES helper parsing.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::bluray_backend::BlurayBackend;
use super::bluray_backend_libbluray::BlurayBackendLibbluray;

const TS_PACKET_SIZE: usize = 188;
const MAX_PES_PREFIX_BYTES: usize = 9 + 255 + 4;

const MIN_BLURAY_ISO_BYTES: u64 = 64 * 1024;
const BLURAY_ISO_SCAN_BYTES: u64 = 1024 * 1024;

/// Check whether a path is any supported Blu-ray source.
#[must_use]
pub fn is_bluray_source(path: &Path) -> bool {
    if path.is_file() {
        is_bluray_iso(path)
    } else if path.is_dir() {
        is_bluray_directory(path)
    } else {
        false
    }
}

/// Check whether an ISO file is a Blu-ray disc image.
///
/// This intentionally performs cheap structural checks before calling
/// libbluray, so random ISO-like media files do not trigger expensive Blu-ray
/// probing.
#[must_use]
pub fn is_bluray_iso(path: &Path) -> bool {
    if !path.is_file() || !bluray_iso_has_bounded_candidate_markers(path) {
        return false;
    }

    BlurayBackendLibbluray::open(path).is_ok()
}

/// Check whether a directory contains a Blu-ray disc root.
///
/// Accepts either the disc root (`.../DISC/BDMV`) or the `BDMV` directory
/// itself, resolving the latter to its parent for backend calls.
#[must_use]
pub fn is_bluray_directory(path: &Path) -> bool {
    let Some(bdmv) = bluray_bdmv_directory_path(path) else {
        return false;
    };
    bdmv_has_expected_layout(&bdmv)
}

/// Return the resolved `BDMV` directory for a Blu-ray directory source.
///
/// Accepts either the disc root or the `BDMV` directory itself, and resolves
/// the `BDMV` component case-insensitively to match Blu-ray source detection.
#[must_use]
pub fn bluray_bdmv_directory_path(path: &Path) -> Option<PathBuf> {
    let root = bluray_directory_root(path)?;
    resolve_child_case_insensitive(&root, "BDMV").filter(|bdmv| bdmv.is_dir())
}

/// Resolved Blu-ray directory paths that participate in source detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlurayDirectoryLayoutPaths {
    pub bdmv: PathBuf,
    pub index: Option<PathBuf>,
    pub movie_object: Option<PathBuf>,
    pub playlist_dir: Option<PathBuf>,
    pub stream_dir: Option<PathBuf>,
    pub first_playlist: Option<PathBuf>,
    pub first_stream: Option<PathBuf>,
}

/// Return a marker file inside the resolved `BDMV` directory.
///
/// This uses the same root normalization and case-insensitive child lookup as
/// `is_bluray_directory()`, so callers get the same path semantics for cache
/// fingerprints and probe invalidation.
#[must_use]
pub fn bluray_directory_marker_path(path: &Path, marker_name: &str) -> Option<PathBuf> {
    let bdmv = bluray_bdmv_directory_path(path)?;
    resolve_child_case_insensitive(&bdmv, marker_name).filter(|marker| marker.is_file())
}

/// Return all detection-relevant Blu-ray directory paths.
///
/// The result accepts either the disc root or the `BDMV` directory itself and
/// resolves required files and directories case-insensitively, matching
/// `is_bluray_directory()`. Optional fields are deliberately present when a
/// component is missing so callers can fingerprint negative classifications
/// against the full predicate rather than only `index.bdmv`.
#[must_use]
pub fn bluray_directory_layout_paths(path: &Path) -> Option<BlurayDirectoryLayoutPaths> {
    let bdmv = bluray_bdmv_directory_path(path)?;
    let playlist_dir = child_dir_case_insensitive(&bdmv, "PLAYLIST");
    let stream_dir = child_dir_case_insensitive(&bdmv, "STREAM");
    let first_playlist = playlist_dir
        .as_ref()
        .and_then(|playlist| first_file_with_extension_case_insensitive(playlist, "mpls"));
    let first_stream = stream_dir
        .as_ref()
        .and_then(|stream| first_file_with_extension_case_insensitive(stream, "m2ts"));

    Some(BlurayDirectoryLayoutPaths {
        bdmv,
        index: bluray_directory_marker_path(path, "index.bdmv"),
        movie_object: bluray_directory_marker_path(path, "MovieObject.bdmv"),
        playlist_dir,
        stream_dir,
        first_playlist,
        first_stream,
    })
}

/// Return the disc root for a Blu-ray directory source.
#[must_use]
pub fn bluray_directory_root(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return None;
    }

    if path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("BDMV"))
    {
        return path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| Some(PathBuf::from(".")));
    }

    let bdmv = resolve_child_case_insensitive(path, "BDMV")?;
    bdmv.is_dir().then(|| path.to_path_buf())
}

/// Open and map a Blu-ray source into the unified disc browsing model.
pub fn map_bluray_source(path: &Path) -> Result<crate::disc::DiscContents, String> {
    let probe = crate::disc::bluray_mapper::FfprobeBlurayAudioProbe::default();
    let control = crate::disc::bluray_mapper::BlurayProbeControl::bounded_default();
    map_bluray_source_with_audio_probe(path, &probe, &control)
}

/// Open and map a Blu-ray source using an explicit compressed-stream probe.
///
/// This keeps the normal TUI browse path bounded by default while allowing tests
/// and callers with configured tool paths to inject their own probe runner.
pub(crate) fn map_bluray_source_with_audio_probe<P>(
    path: &Path,
    compressed_audio_probe: &P,
    probe_control: &crate::disc::bluray_mapper::BlurayProbeControl<'_>,
) -> Result<crate::disc::DiscContents, String>
where
    P: crate::disc::bluray_mapper::BlurayCompressedAudioProbe + ?Sized,
{
    let source = bluray_source_path_for_backend(path)?;
    let disc = BlurayBackendLibbluray::open(&source)
        .map_err(|err| format!("Blu-ray open failed for '{}': {err}", source.display()))?;
    let mut contents = crate::disc::bluray_mapper::map_bluray_disc_with_audio_probe::<
        BlurayBackendLibbluray,
        P,
    >(&disc, &source, compressed_audio_probe, probe_control)?;
    overlay_bluray_sidecar_metadata(path, &mut contents);
    crate::disc::bluray_mapper::refresh_bluray_presentation_labels(&mut contents);
    Ok(contents)
}

pub fn bluray_source_path_for_backend(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        if bluray_iso_has_bounded_candidate_markers(path) {
            return Ok(path.to_path_buf());
        }
        return Err(format!("Not a Blu-ray ISO: {}", path.display()));
    }

    if path.is_dir() {
        let root = bluray_directory_root(path).ok_or_else(|| {
            format!("Not a Blu-ray directory source: {}", path.display())
        })?;
        if is_bluray_directory(&root) {
            return Ok(root);
        }
        return Err(format!("Not a Blu-ray directory source: {}", path.display()));
    }

    Err(format!("Not a Blu-ray source: {}", path.display()))
}

fn overlay_bluray_sidecar_metadata(_source: &Path, _contents: &mut crate::disc::DiscContents) {
    // Phase 5: load and overlay a TOML sidecar, then refresh presentation labels.
}

fn bluray_iso_has_bounded_candidate_markers(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() < MIN_BLURAY_ISO_BYTES {
        return false;
    }

    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buf = Vec::new();
    if file
        .by_ref()
        .take(BLURAY_ISO_SCAN_BYTES.min(meta.len()))
        .read_to_end(&mut buf)
        .is_err()
    {
        return false;
    }

    contains_bytes(&buf, b"NSR02") || contains_bytes(&buf, b"NSR03")
}

fn bdmv_has_expected_layout(bdmv: &Path) -> bool {
    // A single BDMV marker is too weak for source routing: unrelated folders,
    // partial copies, or work directories can contain one of these names. Require
    // the authored navigation files plus at least one playlist and transport
    // stream before the probe path claims Blu-ray ownership.
    let Some(paths) = bluray_directory_layout_paths(bdmv) else {
        return false;
    };

    paths.index.is_some()
        && paths.movie_object.is_some()
        && paths.playlist_dir.is_some()
        && paths.stream_dir.is_some()
        && paths.first_playlist.is_some()
        && paths.first_stream.is_some()
}

fn child_dir_case_insensitive(parent: &Path, wanted: &str) -> Option<PathBuf> {
    resolve_child_case_insensitive(parent, wanted).filter(|path| path.is_dir())
}

fn first_file_with_extension_case_insensitive(dir: &Path, extension: &str) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(extension))
        })
        .collect();

    matches.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let right_name = right
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        left_name
            .cmp(&right_name)
            .then_with(|| left.to_string_lossy().cmp(&right.to_string_lossy()))
    });
    matches.into_iter().next()
}

fn resolve_child_case_insensitive(parent: &Path, wanted: &str) -> Option<PathBuf> {
    let exact = parent.join(wanted);
    if exact.exists() {
        return Some(exact);
    }

    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(wanted))
        {
            return Some(entry.path());
        }
    }
    None
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}


/// Logical channel labels used by Blu-ray LPCM channel-assignment tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    FrontLeft,
    FrontRight,
    FrontCenter,
    LowFrequency,
    BackLeft,
    BackRight,
    BackCenter,
    SideLeft,
    SideRight,
}

const SPEAKER_FRONT_LEFT: u32 = 0x0000_0001;
const SPEAKER_FRONT_RIGHT: u32 = 0x0000_0002;
const SPEAKER_FRONT_CENTER: u32 = 0x0000_0004;
const SPEAKER_LOW_FREQUENCY: u32 = 0x0000_0008;
const SPEAKER_BACK_LEFT: u32 = 0x0000_0010;
const SPEAKER_BACK_RIGHT: u32 = 0x0000_0020;
const SPEAKER_BACK_CENTER: u32 = 0x0000_0100;
const SPEAKER_SIDE_LEFT: u32 = 0x0000_0200;
const SPEAKER_SIDE_RIGHT: u32 = 0x0000_0400;

const CH_MONO_BD: &[Channel] = &[Channel::FrontCenter];
const CH_MONO_WAV: &[Channel] = &[Channel::FrontCenter];
const CH_STEREO_BD: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight];
const CH_STEREO_WAV: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight];
const CH_3_0_BD: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight, Channel::FrontCenter];
const CH_3_0_WAV: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight, Channel::FrontCenter];
const CH_2_1_BD: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight, Channel::BackCenter];
const CH_2_1_WAV: &[Channel] = &[Channel::FrontLeft, Channel::FrontRight, Channel::BackCenter];
const CH_4_0_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::BackCenter,
];
const CH_4_0_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::BackCenter,
];
const CH_2_2_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_2_2_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_5_0_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_5_0_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_5_1_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::LowFrequency,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_5_1_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::LowFrequency,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_7_0_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::SideLeft,
    Channel::SideRight,
    Channel::BackLeft,
    Channel::BackRight,
];
const CH_7_0_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::BackLeft,
    Channel::BackRight,
    Channel::SideLeft,
    Channel::SideRight,
];
const CH_7_1_BD: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::LowFrequency,
    Channel::SideLeft,
    Channel::SideRight,
    Channel::BackLeft,
    Channel::BackRight,
];
const CH_7_1_WAV: &[Channel] = &[
    Channel::FrontLeft,
    Channel::FrontRight,
    Channel::FrontCenter,
    Channel::LowFrequency,
    Channel::BackLeft,
    Channel::BackRight,
    Channel::SideLeft,
    Channel::SideRight,
];

/// Parsed BD-ROM LPCM four-byte PES payload header.
///
/// Source of truth: this parser deliberately follows the BD-ROM LPCM layout
/// used by FFmpeg's pcm_bluray decoder and libavformat Blu-ray fixtures: byte 2
/// stores the audio-presentation/channel-assignment code in its high nibble and
/// the sampling-frequency code in its low nibble; byte 3 stores the bit-depth
/// code in its two high bits. Bytes 0 and 1 are not stream-format selectors and
/// are intentionally ignored by this stream-format parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurayLpcmHeader {
    pub audio_presentation_type: u8,
    pub sample_rate_code: u8,
    pub bits_per_sample_code: u8,
    pub channel_assignment: u8,

    pub sample_rate: u32,
    pub container_bits: u16,
    pub valid_bits: u16,
    pub channels: u8,
    pub wav_channel_mask: Option<u32>,
    pub bd_channel_order: &'static [Channel],
    pub wav_channel_order: &'static [Channel],
}

impl BlurayLpcmHeader {
    #[must_use]
    pub const fn coded_channels(self) -> u8 {
        align_to_even(self.channels)
    }

    #[must_use]
    pub const fn channel_layout_label(self) -> &'static str {
        match self.channel_assignment {
            1 => "mono",
            3 => "stereo",
            4 => "3.0",
            5 => "2.1",
            6 => "4.0",
            7 => "2.2",
            8 => "5.0",
            9 => "5.1",
            10 => "7.0",
            11 => "7.1",
            _ => "unknown",
        }
    }
}

pub type BlurayLpcmPesHeader = BlurayLpcmHeader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlurayLpcmError {
    ReservedSampleRateCode(u8),
    ReservedBitsPerSampleCode(u8),
    UnsupportedChannelAssignment(u8),
}

impl std::fmt::Display for BlurayLpcmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedSampleRateCode(code) => write!(
                f,
                "reserved Blu-ray LPCM sample-rate code {code}; supported codes are 1=48000 Hz, 4=96000 Hz, 5=192000 Hz"
            ),
            Self::ReservedBitsPerSampleCode(code) => write!(
                f,
                "reserved Blu-ray LPCM bit-depth code {code}; supported codes are 1=16-bit, 2=20-in-24-bit, 3=24-bit"
            ),
            Self::UnsupportedChannelAssignment(code) => write!(
                f,
                "unsupported Blu-ray LPCM channel assignment code {code}; supported assignments are 1 mono, 3 stereo, 4 3.0, 5 2.1, 6 4.0, 7 2.2, 8 5.0, 9 5.1, 10 7.0, and 11 7.1"
            ),
        }
    }
}

impl std::error::Error for BlurayLpcmError {}

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
#[must_use]
pub fn parse_bluray_lpcm_pes_header(header: [u8; 4]) -> Result<BlurayLpcmPesHeader, String> {
    parse_bluray_lpcm_header(header).map_err(|err| err.to_string())
}

pub fn parse_bluray_lpcm_header(bytes: [u8; 4]) -> Result<BlurayLpcmHeader, BlurayLpcmError> {
    let audio_presentation_type = bytes[2] >> 4;
    let channel_assignment = audio_presentation_type;
    let sample_rate_code = bytes[2] & 0x0f;
    let bits_per_sample_code = bytes[3] >> 6;

    let (container_bits, valid_bits) = match bits_per_sample_code {
        1 => (16, 16),
        2 => (24, 20),
        3 => (24, 24),
        other => return Err(BlurayLpcmError::ReservedBitsPerSampleCode(other)),
    };

    let sample_rate = match sample_rate_code {
        1 => 48_000,
        4 => 96_000,
        5 => 192_000,
        other => return Err(BlurayLpcmError::ReservedSampleRateCode(other)),
    };

    let (channels, wav_channel_mask, bd_channel_order, wav_channel_order) = match channel_assignment {
        1 => (1, Some(SPEAKER_FRONT_CENTER), CH_MONO_BD, CH_MONO_WAV),
        3 => (2, Some(SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT), CH_STEREO_BD, CH_STEREO_WAV),
        4 => (
            3,
            Some(SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_CENTER),
            CH_3_0_BD,
            CH_3_0_WAV,
        ),
        5 => (
            3,
            Some(SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_BACK_CENTER),
            CH_2_1_BD,
            CH_2_1_WAV,
        ),
        6 => (
            4,
            Some(
                SPEAKER_FRONT_LEFT
                    | SPEAKER_FRONT_RIGHT
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_BACK_CENTER,
            ),
            CH_4_0_BD,
            CH_4_0_WAV,
        ),
        7 => (
            4,
            Some(SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT),
            CH_2_2_BD,
            CH_2_2_WAV,
        ),
        8 => (
            5,
            Some(
                SPEAKER_FRONT_LEFT
                    | SPEAKER_FRONT_RIGHT
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_SIDE_LEFT
                    | SPEAKER_SIDE_RIGHT,
            ),
            CH_5_0_BD,
            CH_5_0_WAV,
        ),
        9 => (
            6,
            Some(
                SPEAKER_FRONT_LEFT
                    | SPEAKER_FRONT_RIGHT
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_LOW_FREQUENCY
                    | SPEAKER_SIDE_LEFT
                    | SPEAKER_SIDE_RIGHT,
            ),
            CH_5_1_BD,
            CH_5_1_WAV,
        ),
        10 => (
            7,
            Some(
                SPEAKER_FRONT_LEFT
                    | SPEAKER_FRONT_RIGHT
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_BACK_LEFT
                    | SPEAKER_BACK_RIGHT
                    | SPEAKER_SIDE_LEFT
                    | SPEAKER_SIDE_RIGHT,
            ),
            CH_7_0_BD,
            CH_7_0_WAV,
        ),
        11 => (
            8,
            Some(
                SPEAKER_FRONT_LEFT
                    | SPEAKER_FRONT_RIGHT
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_LOW_FREQUENCY
                    | SPEAKER_BACK_LEFT
                    | SPEAKER_BACK_RIGHT
                    | SPEAKER_SIDE_LEFT
                    | SPEAKER_SIDE_RIGHT,
            ),
            CH_7_1_BD,
            CH_7_1_WAV,
        ),
        other => return Err(BlurayLpcmError::UnsupportedChannelAssignment(other)),
    };

    Ok(BlurayLpcmHeader {
        audio_presentation_type,
        sample_rate_code,
        bits_per_sample_code,
        channel_assignment,
        sample_rate,
        container_bits,
        valid_bits,
        channels,
        wav_channel_mask,
        bd_channel_order,
        wav_channel_order,
    })
}

#[must_use]
pub fn bluray_lpcm_channel_layout_from_code(code: u8) -> Option<(&'static str, u8)> {
    parse_bluray_lpcm_header([0, 0, (code << 4) | 1, 1 << 6])
        .ok()
        .map(|header| (header.channel_layout_label(), header.channels))
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bluray-utils-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn write_fixture_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, b"fixture").expect("write fixture file");
    }

    #[test]
    fn directory_detection_rejects_single_marker_bdmv_trees() {
        let root = unique_dir("single-marker");
        let bdmv = root.join("BDMV");
        fs::create_dir_all(&bdmv).expect("create BDMV dir");

        assert!(!is_bluray_directory(&root));

        write_fixture_file(&bdmv.join("index.bdmv"));
        assert!(!is_bluray_directory(&root));

        fs::create_dir_all(bdmv.join("PLAYLIST")).expect("create PLAYLIST dir");
        assert!(!is_bluray_directory(&root));

        fs::create_dir_all(bdmv.join("STREAM")).expect("create STREAM dir");
        assert!(!is_bluray_directory(&root));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_detection_requires_navigation_and_media_assets() {
        let root = unique_dir("full-layout");
        let bdmv = root.join("bdmv");
        let playlist = bdmv.join("playlist");
        let stream = bdmv.join("stream");
        fs::create_dir_all(&playlist).expect("create playlist dir");
        fs::create_dir_all(&stream).expect("create stream dir");
        write_fixture_file(&bdmv.join("INDEX.BDMV"));
        write_fixture_file(&bdmv.join("MovieObject.bdmv"));

        assert!(!is_bluray_directory(&root));

        write_fixture_file(&playlist.join("00012.MPLS"));
        assert!(!is_bluray_directory(&root));

        write_fixture_file(&stream.join("00012.m2ts"));
        assert!(is_bluray_directory(&root));
        assert!(is_bluray_directory(&bdmv));
        assert_eq!(bluray_directory_root(&bdmv), Some(root.clone()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_stereo_48khz_16_bit_header() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.coded_channels(), 2);
        assert_eq!(parsed.channel_layout_label(), "stereo");
        assert_eq!(parsed.sample_rate, 48_000);
        assert_eq!(parsed.valid_bits, 16);
    }

    #[test]
    fn parses_5_1_96khz_24_bit_header() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (9 << 4) | 4, 3 << 6]).unwrap();
        assert_eq!(parsed.channels, 6);
        assert_eq!(parsed.channel_layout_label(), "5.1");
        assert_eq!(parsed.sample_rate, 96_000);
        assert_eq!(parsed.valid_bits, 24);
    }

    #[test]
    fn parses_20_bit_header_even_if_decode_path_may_not_accept_it() {
        let parsed = parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 5, 2 << 6]).unwrap();
        assert_eq!(parsed.sample_rate, 192_000);
        assert_eq!(parsed.valid_bits, 20);
    }

    #[test]
    fn rejects_reserved_values() {
        assert!(parse_bluray_lpcm_pes_header([0, 0, (2 << 4) | 1, 1 << 6]).is_err());
        assert!(parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 2, 1 << 6]).is_err());
        assert!(parse_bluray_lpcm_pes_header([0, 0, (3 << 4) | 1, 0]).is_err());
    }

    #[test]
    fn parses_common_channel_assignments_from_raw_headers() {
        let mono = parse_bluray_lpcm_header([0, 0, (1 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(mono.channels, 1);
        assert_eq!(mono.channel_layout_label(), "mono");
        assert_eq!(mono.wav_channel_order, &[Channel::FrontCenter][..]);

        let stereo = parse_bluray_lpcm_header([0, 0, (3 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(stereo.channels, 2);
        assert_eq!(stereo.channel_layout_label(), "stereo");
        assert_eq!(stereo.wav_channel_mask, Some(SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT));

        let surround_5_1 = parse_bluray_lpcm_header([0, 0, (9 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(surround_5_1.channels, 6);
        assert_eq!(surround_5_1.channel_layout_label(), "5.1");
        assert_eq!(
            surround_5_1.bd_channel_order,
            &[
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::FrontCenter,
                Channel::LowFrequency,
                Channel::SideLeft,
                Channel::SideRight,
            ][..]
        );

        let surround_7_1 = parse_bluray_lpcm_header([0, 0, (11 << 4) | 1, 1 << 6]).unwrap();
        assert_eq!(surround_7_1.channels, 8);
        assert_eq!(surround_7_1.channel_layout_label(), "7.1");
        assert_eq!(
            surround_7_1.wav_channel_order,
            &[
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::FrontCenter,
                Channel::LowFrequency,
                Channel::BackLeft,
                Channel::BackRight,
                Channel::SideLeft,
                Channel::SideRight,
            ][..]
        );
    }

    #[test]
    fn parses_each_supported_sample_rate_code() {
        let cases = [(1, 48_000), (4, 96_000), (5, 192_000)];
        for (code, expected_rate) in cases {
            let parsed = parse_bluray_lpcm_header([0, 0, (3 << 4) | code, 1 << 6]).unwrap();
            assert_eq!(parsed.sample_rate_code, code);
            assert_eq!(parsed.sample_rate, expected_rate);
        }
    }

    #[test]
    fn parses_each_supported_bit_depth_code() {
        let cases = [(1, 16, 16), (2, 24, 20), (3, 24, 24)];
        for (code, expected_container, expected_valid) in cases {
            let parsed = parse_bluray_lpcm_header([0, 0, (3 << 4) | 1, code << 6]).unwrap();
            assert_eq!(parsed.bits_per_sample_code, code);
            assert_eq!(parsed.container_bits, expected_container);
            assert_eq!(parsed.valid_bits, expected_valid);
        }
    }

    #[test]
    fn parser_rejects_reserved_codes_with_actionable_errors() {
        let err = parse_bluray_lpcm_header([0, 0, (2 << 4) | 1, 1 << 6]).unwrap_err();
        assert!(err.to_string().contains("unsupported Blu-ray LPCM channel assignment code 2"));

        let err = parse_bluray_lpcm_header([0, 0, (3 << 4) | 2, 1 << 6]).unwrap_err();
        assert!(err.to_string().contains("reserved Blu-ray LPCM sample-rate code 2"));

        let err = parse_bluray_lpcm_header([0, 0, (3 << 4) | 1, 0]).unwrap_err();
        assert!(err.to_string().contains("reserved Blu-ray LPCM bit-depth code 0"));
    }

    #[test]
    fn scans_ts_packet_for_lpcm_pes_header() {
        let pid = 0x1100;
        let packet = ts_packet(pid, true, 0, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));

        let mut pids = HashSet::new();
        pids.insert(pid);
        let found = probe_bluray_lpcm_pes_headers_from_ts(&packet, &pids);
        assert_eq!(found.get(&pid).unwrap().valid_bits, 16);
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
        assert_eq!(header.channel_layout_label(), "5.1");
        assert_eq!(header.sample_rate, 96_000);
        assert_eq!(header.valid_bits, 24);
    }

    #[test]
    fn stateful_probe_recovers_from_prefix_junk_before_sync() {
        let pid = 0x1100;
        let packet = ts_packet(pid, true, 0, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));
        let bytes = [b"junk".as_slice(), packet.as_slice()].concat();
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&bytes);

        assert_eq!(probe.found().get(&pid).unwrap().valid_bits, 16);
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
        assert_eq!(header.valid_bits, 20);
    }

    #[test]
    fn payload_unit_start_replaces_prior_incomplete_pes_for_same_pid() {
        let pid = 0x1100;
        let bad_start = ts_packet(pid, true, 0, &[0x00, 0x00, 0x01, 0xbd, 0x00]);
        let good = ts_packet(pid, true, 1, &pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]));
        let mut probe = BlurayLpcmPesProbe::new([pid]);

        probe.feed(&bad_start);
        probe.feed(&good);

        assert_eq!(probe.found().get(&pid).unwrap().valid_bits, 16);
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

        assert_eq!(probe.found().get(&pid).unwrap().channel_layout_label(), "mono");
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
                assert!(message.contains("unsupported Blu-ray LPCM channel assignment code"));
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

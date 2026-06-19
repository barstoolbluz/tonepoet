#![forbid(unsafe_code)]

//! DVD-Audio Phase 2 materializer.
//!
//! This module wires the Phase 1 IFO parser into the conversion pipeline. It
//! describes DVD-Audio tracks as `TrackSourceRef::DvdaTrack` values and leaves
//! AOB packet extraction/demuxing to Phase 3.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[path = "materializer_dvda_metabase.rs"]
mod materializer_dvda_metabase;

use self::materializer_dvda_metabase::DvdaTrackMetadataKeys;
#[cfg(test)]
use super::dvda_demux::DVD_SECTOR_SIZE;
use super::errors::{MaterializeError, SourceDetectError};
use super::reporter::PipelineReporter;
use super::stages::Materializer;
use super::tool::ToolRunner;
use super::types::*;
use crate::disc::dvda_utils::probe_group_aob_format_with_path;
use crate::tui::dvda::{
    parse_dvda_volume, refine_copy_protection_from_aob_probe, AobFileEntry, AudioAttributes,
    AudioChapter, AudioTitle, ChannelAssignment, ChannelFormat, CopyProtectionSource,
    DirectoryDvdaVolume, DvdaDisc, DvdaError, DvdaFile, DvdaGroup, DvdaVolume, GroupCorrelation,
    Iso9660DvdaVolume, IsoUdfDvdaVolume, SamgTrack, SamgTrackRef, SamgZone, TitleRef, TitleRefKind,
    TitleSet,
};
use crate::tui::dvda_metabase::{self, DvdaMetabase, LoadedDvdaMetabase};

const DVDA_AMG_MAGIC: &[u8] = b"DVDAUDIO-AMG";
const PTS_PER_SECOND: u64 = 90_000;
// ATS track-type bit 3 marks an alternate presentation in observed discs.
// Its low three bits may still point at the primary IFO audio-format entry,
// so do not trust IFO-derived audio expectations when this bit is set.
const DVDA_TRACK_TYPE_ALTERNATE_PRESENTATION_BIT: u8 = 0x08;
#[allow(dead_code)]
const DVD_SECTOR_SIZE_U64: u64 = 2048;
#[cfg(test)]
const DVD_SECTOR_SIZE_USIZE: usize = DVD_SECTOR_SIZE;
const RAW_DVDA_MAGIC_SCAN_CHUNK: usize = 1024 * 1024;

pub struct DvdaAudioMaterializer;

#[async_trait]
impl Materializer for DvdaAudioMaterializer {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        _staging: &StagingDir,
        _runner: &dyn ToolRunner,
        _reporter: Option<&dyn PipelineReporter>,
        _tool_paths: &HashMap<String, PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError> {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let volume = open_dvda_volume_for_request(req)?;
        materialize_prepared_source(req, volume.source_ref(), &volume, cancel)
    }
}

/// Route DVD-Audio sources before generic `.iso` archive handling.
///
/// Detection uses confidence layers rather than an unconditional raw byte scan:
///
/// 1. Directory path lookup for `AUDIO_TS/AUDIO_TS.IFO`, with the AMG identifier
///    checked at byte offset 0.
/// 2. UDF path lookup inside an ISO, using the read-only UDF backend.
/// 3. ISO9660 bridge path lookup inside an ISO, using the same backend used
///    for materialization when that evidence path wins.
/// 4. Raw magic scanning only when the caller has explicitly requested DVD-Audio
///    handling through DVD-Audio-specific source options. Raw magic is diagnostic
///    evidence only until a filesystem-backed backend can open the volume.
///    With explicit intent, raw evidence still routes to the DVD-Audio
///    materializer so the user gets a DVD-Audio parse/evidence error rather
///    than a generic archive fallback.
///
/// Auto-detection returns true only for filesystem-backed `AUDIO_TS/AUDIO_TS.IFO`
/// evidence. Explicit DVD-Audio intent also routes raw AMG evidence to the
/// DVD-Audio materializer so it can return a DVD-Audio-specific diagnostic
/// instead of falling through to generic archive handling.
pub(crate) fn is_dvda_candidate(req: &PipelineRequest) -> Result<bool, SourceDetectError> {
    Ok(detect_dvda_source(req)?.routes_to_dvd_audio())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DvdaDetection {
    DirectoryPath,
    UdfPath,
    Iso9660BridgePath,
    ExplicitRawMagicFallback,
    NotDetected,
}

impl DvdaDetection {
    fn routes_to_dvd_audio(self) -> bool {
        matches!(
            self,
            DvdaDetection::DirectoryPath
                | DvdaDetection::UdfPath
                | DvdaDetection::Iso9660BridgePath
                | DvdaDetection::ExplicitRawMagicFallback
        )
    }
}

fn detect_dvda_source(req: &PipelineRequest) -> Result<DvdaDetection, SourceDetectError> {
    let explicit = explicit_dvda_requested(req);

    if req.container.is_dir() {
        if directory_has_dvda_magic(&req.container)? {
            return Ok(DvdaDetection::DirectoryPath);
        }
        return Ok(DvdaDetection::NotDetected);
    }

    if !has_extension(&req.container, "iso") {
        return Ok(DvdaDetection::NotDetected);
    }

    if iso_udf_path_has_dvda_magic(&req.container).unwrap_or(false) {
        return Ok(DvdaDetection::UdfPath);
    }

    if iso9660_bridge_has_dvda_magic(&req.container).unwrap_or(false) {
        return Ok(DvdaDetection::Iso9660BridgePath);
    }

    if explicit {
        if raw_iso_scan_has_dvda_magic(&req.container)? {
            return Ok(DvdaDetection::ExplicitRawMagicFallback);
        }
        return Ok(DvdaDetection::NotDetected);
    }

    Ok(DvdaDetection::NotDetected)
}

fn explicit_dvda_requested(req: &PipelineRequest) -> bool {
    req.source.explicit_dvda_requested()
}

fn materialize_prepared_source(
    req: &PipelineRequest,
    volume_source: &DvdaVolumeSourceRef,
    volume: &dyn DvdaVolume,
    cancel: &CancellationToken,
) -> Result<PreparedSource, MaterializeError> {
    if cancel.is_cancelled() {
        return Err(MaterializeError::Cancelled);
    }

    let mut disc = parse_dvda_volume(volume).map_err(dvda_error_to_materialize)?;
    refine_copy_protection_from_aob_probe(volume, &mut disc, req.source.dvda_assume_decrypted)
        .map_err(dvda_error_to_materialize)?;

    let loaded_metabase =
        materializer_dvda_metabase::load_for_materializer(volume, &req.container)?;
    let metabase = loaded_metabase.as_ref().map(|loaded| &loaded.metabase);

    let group_selection = req.source.effective_dvda_group_selection();
    let groups = select_groups(&disc, group_selection)?;
    let mut tracks = prepared_tracks_for_groups(
        volume_source,
        volume,
        &disc,
        &groups,
        req.source.dvda_downmix_policy,
        metabase,
        cancel,
    )?;
    tracks = apply_track_selection(tracks, &req.source.track_selection)?;

    if disc.copy_protection.cppm_detected {
        let block = dvda_copy_protection_block(&disc);
        mark_tracks_blocked_for_copy_protection(&mut tracks, &block);
        let source = prepared_source(
            req,
            &disc,
            &groups,
            group_selection,
            tracks,
            metabase,
            loaded_metabase.as_ref(),
        );
        let message = format!("DVD-Audio source is encrypted: {}", block.log_label());
        return Err(MaterializeError::BlockedSource {
            message: message.clone(),
            blocked: Box::new(BlockedSource {
                source,
                reason: SourceBlockReason::DvdaCppm(block),
                message,
            }),
        });
    }

    Ok(prepared_source(
        req,
        &disc,
        &groups,
        group_selection,
        tracks,
        metabase,
        loaded_metabase.as_ref(),
    ))
}

fn prepared_source(
    req: &PipelineRequest,
    disc: &DvdaDisc,
    groups: &[&DvdaGroup],
    group_selection: DvdaGroupSelection,
    tracks: Vec<PreparedTrack>,
    metabase: Option<&DvdaMetabase>,
    loaded_metabase: Option<&LoadedDvdaMetabase>,
) -> PreparedSource {
    let album_metadata = album_metadata(
        disc,
        groups,
        group_selection,
        tracks.len() as u32,
        &tracks,
        metabase,
        loaded_metabase,
    );
    let mut tool_versions = BTreeMap::new();
    tool_versions.insert("dvda-demuxer".to_string(), "in-process".to_string());
    if loaded_metabase.is_some() {
        tool_versions.insert(
            "dvda-metabase".to_string(),
            "in-process".to_string(),
        );
    }

    PreparedSource {
        container: req.container.clone(),
        kind: SourceKind::DvdAudio,
        tracks,
        album_metadata,
        provenance: ExtractionProvenance {
            source_kind: SourceKind::DvdAudio,
            source_sha256: None,
            tool_versions,
            extracted_at: chrono::Utc::now(),
        },
    }
}

fn dvda_copy_protection_block(disc: &DvdaDisc) -> DvdaCopyProtectionBlock {
    let evidence_source = match disc.copy_protection.source {
        CopyProtectionSource::MkbPresence => DvdaCopyProtectionEvidenceSource::DvdaudioMkb,
        CopyProtectionSource::MkbPresentAobProbeReadable => {
            DvdaCopyProtectionEvidenceSource::AobMpegPsProbe
        }
        CopyProtectionSource::AobProbeNoMpegPs => DvdaCopyProtectionEvidenceSource::AobMpegPsProbe,
        CopyProtectionSource::AssumeDecryptedOverride => {
            DvdaCopyProtectionEvidenceSource::UserOverride
        }
        CopyProtectionSource::NotDetected => DvdaCopyProtectionEvidenceSource::Unknown,
    };
    let evidence_filename = if disc.copy_protection.mkb_present {
        Some("DVDAUDIO.MKB".to_string())
    } else {
        None
    };
    let diagnostics = disc
        .diagnostics
        .iter()
        .map(|diag| format!("{}:{:?}: {}", diag.code, diag.severity, diag.message))
        .collect();

    DvdaCopyProtectionBlock {
        scheme: DvdaCopyProtectionScheme::Cppm,
        evidence_source,
        evidence_filename,
        mkb_present: disc.copy_protection.mkb_present,
        cppm_detected: disc.copy_protection.cppm_detected,
        handling_policy: DvdaCopyProtectionHandlingPolicy::DetectExplainSkip,
        decryption_supported: false,
        skip_reason: "CPPM-protected DVD-Audio source; this build detects, explains, and skips protected AOB realization instead of attempting decryption".to_string(),
        user_explanation: "This DVD-Audio source appears to use CPPM copy protection. tonepoet can report the DVD-Audio structure and track metadata, but this build will not decrypt CPPM-protected audio sectors. Use an unencrypted disc image or another source that has already been made accessible by a legally authorized workflow.".to_string(),
        diagnostics,
    }
}

fn mark_tracks_blocked_for_copy_protection(
    tracks: &mut [PreparedTrack],
    block: &DvdaCopyProtectionBlock,
) {
    for track in tracks {
        track
            .metadata
            .extra
            .insert("tonepoet_blocked".to_string(), "true".to_string());
        track.metadata.extra.insert(
            "tonepoet_block_reason".to_string(),
            "copy_protection".to_string(),
        );
        track.metadata.extra.insert(
            "dvda_copy_protection_scheme".to_string(),
            "CPPM".to_string(),
        );
        track.metadata.extra.insert(
            "dvda_copy_protection_source".to_string(),
            format!("{:?}", block.evidence_source),
        );
        track.metadata.extra.insert(
            "dvda_copy_protection_policy".to_string(),
            format!("{:?}", block.handling_policy),
        );
        track.metadata.extra.insert(
            "dvda_cppm_decryption_supported".to_string(),
            block.decryption_supported.to_string(),
        );
        track.metadata.extra.insert(
            "dvda_copy_protection_skip_reason".to_string(),
            block.skip_reason.clone(),
        );
        if let Some(filename) = &block.evidence_filename {
            track
                .metadata
                .extra
                .insert("dvda_copy_protection_file".to_string(), filename.clone());
        }
    }
}

fn open_dvda_volume_for_request(
    req: &PipelineRequest,
) -> Result<PreparedDvdaVolume, MaterializeError> {
    let detection = detect_dvda_source(req).map_err(|err| {
        MaterializeError::Parse(format!(
            "DVD-Audio source detection failed before materialization: {err}"
        ))
    })?;
    open_dvda_volume_with_detection(&req.container, detection)
}

fn open_dvda_volume_with_detection(
    container: &Path,
    detection: DvdaDetection,
) -> Result<PreparedDvdaVolume, MaterializeError> {
    match detection {
        DvdaDetection::DirectoryPath => open_dvda_volume(container),
        DvdaDetection::UdfPath => open_dvda_iso_volume_with_backend(container, DvdaIsoBackend::Udf),
        DvdaDetection::Iso9660BridgePath => open_dvda_iso_volume_with_backend(container, DvdaIsoBackend::Iso9660Bridge),
        DvdaDetection::ExplicitRawMagicFallback => Err(MaterializeError::Parse(format!(
            "explicit DVD-Audio raw magic was found in {}, but no AUDIO_TS filesystem path was available for materialization",
            container.display()
        ))),
        DvdaDetection::NotDetected => Err(MaterializeError::Parse(format!(
            "DVD-Audio source was not detected through a materializable AUDIO_TS path: {}",
            container.display()
        ))),
    }
}

fn open_dvda_volume(container: &Path) -> Result<PreparedDvdaVolume, MaterializeError> {
    if container.is_dir() {
        return Ok(PreparedDvdaVolume {
            source_ref: DvdaVolumeSourceRef::Directory {
                root: container.to_path_buf(),
            },
            backend: PreparedDvdaVolumeBackend::Directory(DirectoryDvdaVolume::new(
                container.to_path_buf(),
            )),
        });
    }
    if has_extension(container, "iso") {
        return open_dvda_iso_volume(container);
    }

    Err(MaterializeError::Parse(format!(
        "DVD-Audio container is not a directory or ISO: {}",
        container.display()
    )))
}

fn open_dvda_iso_volume(path: &Path) -> Result<PreparedDvdaVolume, MaterializeError> {
    open_dvda_iso_volume_with_backend(path, DvdaIsoBackend::Udf)
        .or_else(|_| open_dvda_iso_volume_with_backend(path, DvdaIsoBackend::Iso9660Bridge))
}

fn open_dvda_iso_volume_with_backend(
    path: &Path,
    backend: DvdaIsoBackend,
) -> Result<PreparedDvdaVolume, MaterializeError> {
    match backend {
        DvdaIsoBackend::Udf => {
            let volume = IsoUdfDvdaVolume::open(path).map_err(dvda_error_to_materialize)?;
            if !file_in_volume_starts_with_magic(&volume, "AUDIO_TS.IFO", DVDA_AMG_MAGIC)
                .map_err(dvda_error_to_materialize)?
            {
                return Err(MaterializeError::Parse(format!(
                    "UDF AUDIO_TS/AUDIO_TS.IFO does not start with DVDAUDIO-AMG: {}",
                    path.display()
                )));
            }
            Ok(PreparedDvdaVolume {
                source_ref: DvdaVolumeSourceRef::Iso {
                    path: path.to_path_buf(),
                    backend: DvdaIsoBackend::Udf,
                },
                backend: PreparedDvdaVolumeBackend::IsoUdf(volume),
            })
        }
        DvdaIsoBackend::Iso9660Bridge => {
            let volume = Iso9660DvdaVolume::open(path).map_err(dvda_error_to_materialize)?;
            if !file_in_volume_starts_with_magic(&volume, "AUDIO_TS.IFO", DVDA_AMG_MAGIC)
                .map_err(dvda_error_to_materialize)?
            {
                return Err(MaterializeError::Parse(format!(
                    "ISO9660 AUDIO_TS/AUDIO_TS.IFO does not start with DVDAUDIO-AMG: {}",
                    path.display()
                )));
            }
            Ok(PreparedDvdaVolume {
                source_ref: DvdaVolumeSourceRef::Iso {
                    path: path.to_path_buf(),
                    backend: DvdaIsoBackend::Iso9660Bridge,
                },
                backend: PreparedDvdaVolumeBackend::Iso9660(volume),
            })
        }
        DvdaIsoBackend::ExplicitRawMagicOnly => Err(MaterializeError::Parse(format!(
            "raw DVD-Audio magic is not a materializable ISO filesystem backend: {}",
            path.display()
        ))),
    }
}

#[derive(Debug)]
struct PreparedDvdaVolume {
    source_ref: DvdaVolumeSourceRef,
    backend: PreparedDvdaVolumeBackend,
}

impl PreparedDvdaVolume {
    fn source_ref(&self) -> &DvdaVolumeSourceRef {
        &self.source_ref
    }
}

#[derive(Debug)]
enum PreparedDvdaVolumeBackend {
    Directory(DirectoryDvdaVolume),
    IsoUdf(IsoUdfDvdaVolume),
    Iso9660(Iso9660DvdaVolume),
}

impl DvdaVolume for PreparedDvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> crate::tui::dvda::Result<Box<dyn DvdaFile>> {
        match &self.backend {
            PreparedDvdaVolumeBackend::Directory(volume) => volume.open_audio_ts_file(name),
            PreparedDvdaVolumeBackend::IsoUdf(volume) => volume.open_audio_ts_file(name),
            PreparedDvdaVolumeBackend::Iso9660(volume) => volume.open_audio_ts_file(name),
        }
    }

    fn file_len(&self, name: &str) -> crate::tui::dvda::Result<Option<u64>> {
        match &self.backend {
            PreparedDvdaVolumeBackend::Directory(volume) => volume.file_len(name),
            PreparedDvdaVolumeBackend::IsoUdf(volume) => volume.file_len(name),
            PreparedDvdaVolumeBackend::Iso9660(volume) => volume.file_len(name),
        }
    }
}

fn iso_udf_path_has_dvda_magic(path: &Path) -> Result<bool, DvdaError> {
    let volume = IsoUdfDvdaVolume::open(path)?;
    file_in_volume_starts_with_magic(&volume, "AUDIO_TS.IFO", DVDA_AMG_MAGIC)
}

fn directory_has_dvda_magic(root: &Path) -> std::io::Result<bool> {
    for candidate in audio_ts_ifo_candidates(root) {
        if file_starts_with_magic(&candidate, DVDA_AMG_MAGIC)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn file_in_volume_starts_with_magic<V: DvdaVolume + ?Sized>(
    volume: &V,
    name: &str,
    magic: &[u8],
) -> Result<bool, DvdaError> {
    let mut file = match volume.open_audio_ts_file(name) {
        Ok(file) => file,
        Err(DvdaError::MissingFile { .. }) => return Ok(false),
        Err(err) => return Err(err),
    };
    let mut buf = vec![0_u8; magic.len()];
    let read = file
        .read(&mut buf)
        .map_err(|source| DvdaError::io(name, source))?;
    Ok(read == magic.len() && buf == magic)
}

fn audio_ts_ifo_candidates(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("AUDIO_TS").join("AUDIO_TS.IFO"),
        root.join("audio_ts").join("audio_ts.ifo"),
        root.join("AUDIO_TS.IFO"),
        root.join("audio_ts.ifo"),
    ]
}

fn file_starts_with_magic(path: &Path, magic: &[u8]) -> std::io::Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let mut buf = vec![0_u8; magic.len()];
    let read = file.read(&mut buf)?;
    Ok(read == magic.len() && buf == magic)
}

fn iso9660_bridge_has_dvda_magic(path: &Path) -> Result<bool, DvdaError> {
    let volume = match Iso9660DvdaVolume::open(path) {
        Ok(volume) => volume,
        Err(DvdaError::MissingFile { .. }) => return Ok(false),
        Err(err) => return Err(err),
    };
    file_in_volume_starts_with_magic(&volume, "AUDIO_TS.IFO", DVDA_AMG_MAGIC)
}

fn raw_iso_scan_has_dvda_magic(path: &Path) -> std::io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut carry = Vec::<u8>::new();
    let mut chunk = vec![0_u8; RAW_DVDA_MAGIC_SCAN_CHUNK];

    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            return Ok(false);
        }

        let mut window = carry.clone();
        window.extend_from_slice(&chunk[..read]);
        if contains_subslice(&window, DVDA_AMG_MAGIC) {
            return Ok(true);
        }

        let keep = DVDA_AMG_MAGIC.len().saturating_sub(1);
        carry.clear();
        let start = window.len().saturating_sub(keep);
        carry.extend_from_slice(&window[start..]);
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

#[allow(dead_code)]
fn select_group<'a>(
    disc: &'a DvdaDisc,
    requested: Option<u8>,
) -> Result<&'a DvdaGroup, MaterializeError> {
    let selection = requested
        .map(DvdaGroupSelection::Group)
        .unwrap_or(DvdaGroupSelection::Default);
    select_groups(disc, selection).map(|groups| groups[0])
}

fn select_groups<'a>(
    disc: &'a DvdaDisc,
    selection: DvdaGroupSelection,
) -> Result<Vec<&'a DvdaGroup>, MaterializeError> {
    if disc.groups.is_empty() {
        return Err(MaterializeError::Parse(
            "DVD-Audio disc exposes no audio groups".to_string(),
        ));
    }

    match selection {
        DvdaGroupSelection::Default => Ok(vec![default_group(disc)?]),
        DvdaGroupSelection::Group(group_nr) => {
            if group_nr == 0 {
                return Err(MaterializeError::InvalidTrackSelection(
                    "DVD-Audio group numbers are 1-based".to_string(),
                ));
            }
            disc.groups
                .iter()
                .find(|group| group.group_nr == group_nr)
                .map(|group| vec![group])
                .ok_or_else(|| {
                    MaterializeError::InvalidTrackSelection(format!(
                        "requested DVD-Audio group {group_nr} is not present"
                    ))
                })
        }
        DvdaGroupSelection::All => Ok(disc.groups.iter().collect()),
        DvdaGroupSelection::PreferStereo => Ok(vec![
            preferred_stereo_group(disc).unwrap_or(default_group(disc)?)
        ]),
        DvdaGroupSelection::PreferMultichannel => Ok(vec![
            preferred_multichannel_group(disc).unwrap_or(default_group(disc)?)
        ]),
        DvdaGroupSelection::PreferHighestResolution => Ok(vec![
            preferred_highest_resolution_group(disc).unwrap_or(default_group(disc)?),
        ]),
    }
}

fn default_group(disc: &DvdaDisc) -> Result<&DvdaGroup, MaterializeError> {
    disc.groups
        .iter()
        .find(|group| group.group_nr == 1)
        .or_else(|| disc.groups.first())
        .ok_or_else(|| {
            MaterializeError::Parse("DVD-Audio disc exposes no audio groups".to_string())
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GroupAudioProfile {
    max_channels: Option<u8>,
    primary_sample_rate: Option<u32>,
    bit_depth: Option<u8>,
}

impl GroupAudioProfile {
    fn observe_format(&mut self, format: &ChannelFormat, assignment: Option<&ChannelAssignment>) {
        self.max_channels = max_option(
            self.max_channels,
            assignment.and_then(|value| format.total_channels(Some(value))),
        );
        self.primary_sample_rate =
            max_option(self.primary_sample_rate, primary_sample_rate(format));
        self.bit_depth = max_option(self.bit_depth, primary_bit_depth(format));
    }

    fn ranking_key(self) -> (u32, u32, u32) {
        (
            self.primary_sample_rate.unwrap_or(0),
            u32::from(self.bit_depth.unwrap_or(0)),
            u32::from(self.max_channels.unwrap_or(0)),
        )
    }
}

fn max_option<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(std::cmp::max(left, right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn preferred_stereo_group<'a>(disc: &'a DvdaDisc) -> Option<&'a DvdaGroup> {
    disc.groups
        .iter()
        .filter_map(|group| {
            let profile = group_audio_profile(disc, group);
            let is_stereo_presentation =
                profile.max_channels == Some(2) || group_is_authored_stereo_downmix(disc, group);
            is_stereo_presentation.then_some((group, profile.ranking_key()))
        })
        .max_by_key(|(_, key)| *key)
        .map(|(group, _)| group)
}

fn preferred_multichannel_group<'a>(disc: &'a DvdaDisc) -> Option<&'a DvdaGroup> {
    disc.groups
        .iter()
        .filter_map(|group| {
            let profile = group_audio_profile(disc, group);
            profile
                .max_channels
                .filter(|channels| *channels > 2)
                .map(|channels| {
                    let key = (
                        u32::from(channels),
                        profile.primary_sample_rate.unwrap_or(0),
                        u32::from(profile.bit_depth.unwrap_or(0)),
                    );
                    (group, key)
                })
        })
        .max_by_key(|(_, key)| *key)
        .map(|(group, _)| group)
}

fn preferred_highest_resolution_group<'a>(disc: &'a DvdaDisc) -> Option<&'a DvdaGroup> {
    disc.groups
        .iter()
        .filter_map(|group| {
            let profile = group_audio_profile(disc, group);
            (profile.primary_sample_rate.is_some() || profile.bit_depth.is_some())
                .then_some((group, profile.ranking_key()))
        })
        .max_by_key(|(_, key)| *key)
        .map(|(group, _)| group)
}

fn group_audio_profile(disc: &DvdaDisc, group: &DvdaGroup) -> GroupAudioProfile {
    let mut profile = GroupAudioProfile::default();

    for title_ref in &group.title_refs {
        if let Ok(title_set) = find_title_set(disc, title_ref.title_set_nr) {
            let present: Vec<&AudioAttributes> = title_set
                .audio_formats
                .iter()
                .filter(|attr| attr.present)
                .collect();
            if let [attr] = present.as_slice() {
                profile.observe_format(&attr.channel_format, attr.channel_assignment.as_ref());
            }
        }
    }

    for samg_ref in &group.samg_tracks {
        if let Ok(track) = find_samg_track(disc, samg_ref) {
            profile.observe_format(&track.channel_format, track.channel_assignment.as_ref());
        }
    }

    profile
}

fn resolve_non_auto_downmix_policy(policy: DvdaDownmixPolicy) -> DvdaDownmixPolicy {
    match policy {
        DvdaDownmixPolicy::Auto => DvdaDownmixPolicy::None,
        policy => policy,
    }
}

fn resolve_dvda_track_downmix_policy(
    requested_policy: DvdaDownmixPolicy,
    disc_info_stereo_downmix_source_label: Option<&str>,
) -> DvdaDownmixPolicy {
    match requested_policy {
        DvdaDownmixPolicy::Auto => {
            if disc_info_stereo_downmix_source_label.is_some() {
                DvdaDownmixPolicy::FooInputDvdaCompatible
            } else {
                DvdaDownmixPolicy::None
            }
        }
        policy => policy,
    }
}

fn group_is_authored_stereo_downmix(disc: &DvdaDisc, group: &DvdaGroup) -> bool {
    // Group selection runs before AOB probing in the materializer. Keep this
    // as a structural fallback for `PreferStereo`, but do not use it for the
    // extraction downmix policy. `resolve_dvda_track_downmix_policy()` uses the
    // disc-info probe signal instead.
    for title_ref in &group.title_refs {
        let Ok(title_set) = find_title_set(disc, title_ref.title_set_nr) else {
            continue;
        };
        if authored_stereo_downmix_source_label(disc, group, title_set).is_some() {
            return true;
        }
    }
    false
}

fn authored_stereo_downmix_source_label(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_set: &TitleSet,
) -> Option<String> {
    if title_set_has_existing_aobs(title_set) {
        return None;
    }

    let my_tracks = materializer_group_track_count(disc, group);
    if my_tracks == 0 {
        return None;
    }
    let my_duration = materializer_group_duration_secs(disc, group);

    // Do not reject the candidate when this AOB-less title set's own IFO or
    // SAMG-facing facts say "2 channels". On discs such as the Brothers in
    // Arms DVD-Audio, that stereo value describes the authored presentation,
    // while the reused/cross-ATS MLP carrier is still multichannel. This
    // fallback is only for group ranking; extraction policy uses the probed MLP
    // carrier result from disc-info.
    for sibling in &disc.groups {
        if sibling.group_nr == group.group_nr {
            continue;
        }
        if !group_has_existing_aobs(disc, sibling) {
            continue;
        }
        if materializer_group_track_count(disc, sibling) != my_tracks {
            continue;
        }
        if !durations_near_match(my_duration, materializer_group_duration_secs(disc, sibling)) {
            continue;
        }

        let sibling_profile = group_audio_profile(disc, sibling);
        if matches!(sibling_profile.max_channels, Some(channels) if channels > 2) {
            return Some(multichannel_group_label(
                disc,
                sibling,
                sibling_profile.max_channels,
            ));
        }
    }

    None
}

fn group_has_existing_aobs(disc: &DvdaDisc, group: &DvdaGroup) -> bool {
    group.title_refs.iter().any(|title_ref| {
        find_title_set(disc, title_ref.title_set_nr)
            .map(|title_set| title_set.aobs.iter().any(|aob| aob.exists))
            .unwrap_or(false)
    })
}

fn title_set_has_existing_aobs(title_set: &TitleSet) -> bool {
    title_set.aobs.iter().any(|aob| aob.exists)
}

fn group_uses_disc_absolute_addressing(disc: &DvdaDisc, group: &DvdaGroup) -> bool {
    group.title_refs.iter().any(|title_ref| {
        find_title_set(disc, title_ref.title_set_nr)
            .map(|title_set| !title_set_has_existing_aobs(title_set))
            .unwrap_or(false)
    })
}

fn elementary_stream_kind_hint_from_codec(codec: &str) -> Option<DvdaElementaryStreamKind> {
    if codec.eq_ignore_ascii_case("MLP") {
        Some(DvdaElementaryStreamKind::Mlp)
    } else if codec.eq_ignore_ascii_case("LPCM") {
        Some(DvdaElementaryStreamKind::Lpcm)
    } else {
        None
    }
}

fn existing_aob_file_refs(title_set: &TitleSet) -> Vec<DvdaAobFileRef> {
    title_set
        .aobs
        .iter()
        .filter(|aob| aob.exists)
        .map(aob_file_ref)
        .collect()
}

#[derive(Debug, Clone)]
struct SamgSectorCorrelation<'a> {
    disc_absolute_base: u32,
    tracks: Vec<&'a SamgTrack>,
}

impl<'a> SamgSectorCorrelation<'a> {
    fn track_for_chapter_index(&self, chapter_index: usize) -> Option<&'a SamgTrack> {
        self.tracks.get(chapter_index).copied()
    }

    fn elementary_stream_kind_hint(&self) -> Option<DvdaElementaryStreamKind> {
        if self
            .tracks
            .iter()
            .all(|track| matches!(track.zone, SamgZone::Vob))
        {
            Some(DvdaElementaryStreamKind::DvdVideoLpcm)
        } else {
            None
        }
    }
}

fn title_chapter_sector_span(chapter: &AudioChapter) -> Option<(u32, u32, u64)> {
    let first = chapter
        .sector_ranges
        .iter()
        .map(|range| range.first)
        .min()?;
    let last = chapter.sector_ranges.iter().map(|range| range.last).max()?;
    Some((first, last, sector_block_count(chapter)))
}

fn find_samg_sector_correlation<'a>(
    disc: &'a DvdaDisc,
    title: &AudioTitle,
) -> Option<SamgSectorCorrelation<'a>> {
    let samg = disc.samg.as_ref()?;
    if title.chapters.is_empty() {
        return None;
    }

    let chapter_spans: Option<Vec<_>> = title
        .chapters
        .iter()
        .map(title_chapter_sector_span)
        .collect();
    let chapter_spans = chapter_spans?;

    let mut tracks_by_group: BTreeMap<u8, Vec<&SamgTrack>> = BTreeMap::new();
    for track in samg
        .tracks
        .iter()
        .filter(|track| matches!(track.zone, SamgZone::Vob))
    {
        tracks_by_group
            .entry(track.group_nr)
            .or_default()
            .push(track);
    }

    for tracks in tracks_by_group.values_mut() {
        tracks.sort_by_key(|track| (track.track_nr, track.ordinal));
        if tracks.len() != chapter_spans.len() {
            continue;
        }

        let mut base: Option<u32> = None;
        let mut matched = true;
        for (track, (chapter_first, chapter_last, chapter_blocks)) in
            tracks.iter().zip(chapter_spans.iter().copied())
        {
            if samg_sector_block_count(track) != chapter_blocks {
                matched = false;
                break;
            }
            let Some(candidate_base) = track.abs_first_sector.checked_sub(chapter_first) else {
                matched = false;
                break;
            };
            let Some(expected_last) = candidate_base.checked_add(chapter_last) else {
                matched = false;
                break;
            };
            if track.abs_last_sector != expected_last {
                matched = false;
                break;
            }
            if let Some(existing_base) = base {
                if existing_base != candidate_base {
                    matched = false;
                    break;
                }
            } else {
                base = Some(candidate_base);
            }
        }

        if matched {
            return Some(SamgSectorCorrelation {
                disc_absolute_base: base?,
                tracks: tracks.clone(),
            });
        }
    }

    None
}

fn title_disc_absolute_sector_base(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
    title: &AudioTitle,
    samg_sector_correlation: Option<&SamgSectorCorrelation<'_>>,
) -> Option<u32> {
    if let Some(correlation) = samg_sector_correlation {
        log::info!(
            "DVD-Audio ATS {} group {} title {} uses SAMG-derived disc-absolute base {} for AOB-less VOB sharing",
            title_set.number,
            group.group_nr,
            title.title_ordinal,
            correlation.disc_absolute_base
        );
        return Some(correlation.disc_absolute_base);
    }

    let aott_entry = disc
        .amg
        .audio_title_table
        .iter()
        .find(|entry| {
            entry.ordinal == u16::from(group.group_nr)
                && entry.title_set_nr == title_ref.title_set_nr
        })
        .or_else(|| {
            disc.amg
                .audio_title_table
                .iter()
                .find(|entry| entry.title_set_nr == title_ref.title_set_nr)
        })?;

    let base = u64::from(aott_entry.atsi_mat_sector)
        .checked_add(u64::from(title_set.header.atstt_vobs))?;
    u32::try_from(base).ok()
}

fn sector_ranges_for_address_space(
    chapter: &AudioChapter,
    sector_base: Option<u32>,
) -> Result<Vec<DvdaSectorRangeRef>, MaterializeError> {
    chapter
        .sector_ranges
        .iter()
        .map(|range| {
            let (first, last) = if let Some(base) = sector_base {
                (
                    base.checked_add(range.first).ok_or_else(|| {
                        MaterializeError::Parse(format!(
                            "DVD-Audio disc-absolute first sector overflowed for track {} range {}",
                            chapter.track_nr, range.index_nr
                        ))
                    })?,
                    base.checked_add(range.last).ok_or_else(|| {
                        MaterializeError::Parse(format!(
                            "DVD-Audio disc-absolute last sector overflowed for track {} range {}",
                            chapter.track_nr, range.index_nr
                        ))
                    })?,
                )
            } else {
                (range.first, range.last)
            };
            Ok(DvdaSectorRangeRef {
                index_nr: range.index_nr,
                first,
                last,
            })
        })
        .collect()
}

fn materializer_group_track_count(disc: &DvdaDisc, group: &DvdaGroup) -> usize {
    if !group.title_refs.is_empty() {
        let mut count = 0usize;
        for title_ref in &group.title_refs {
            let Ok(title_set) = find_title_set(disc, title_ref.title_set_nr) else {
                continue;
            };
            if let Some(title) = find_title(title_set, title_ref).ok() {
                count += title.chapters.len();
            }
        }
        if count > 0 {
            return count;
        }
    }

    if !group.samg_tracks.is_empty() {
        return group.samg_tracks.len();
    }

    disc.amg
        .audio_title_table
        .iter()
        .find(|entry| entry.ordinal == u16::from(group.group_nr))
        .map(|entry| usize::from(entry.track_count))
        .unwrap_or(0)
}

fn materializer_group_duration_secs(disc: &DvdaDisc, group: &DvdaGroup) -> f64 {
    let mut total_pts: u64 = 0;

    if !group.title_refs.is_empty() {
        for title_ref in &group.title_refs {
            let Ok(title_set) = find_title_set(disc, title_ref.title_set_nr) else {
                continue;
            };
            if let Some(title) = find_title(title_set, title_ref).ok() {
                total_pts += title
                    .chapters
                    .iter()
                    .map(|chapter| u64::from(chapter.len_in_pts))
                    .sum::<u64>();
            }
        }
        if total_pts > 0 {
            return total_pts as f64 / PTS_PER_SECOND as f64;
        }
    }

    if let Some(samg) = disc.samg.as_ref() {
        for samg_ref in &group.samg_tracks {
            if let Some(track) = samg.tracks.iter().find(|track| {
                track.ordinal == samg_ref.samg_ordinal
                    && track.group_nr == samg_ref.group_nr
                    && track.track_nr == samg_ref.track_nr
            }) {
                total_pts += u64::from(track.len_in_pts);
            }
        }
    }

    total_pts as f64 / PTS_PER_SECOND as f64
}

fn durations_near_match(a: f64, b: f64) -> bool {
    let diff = (a - b).abs();
    let max_dur = a.max(b);
    diff <= max_dur * 0.01 || diff <= 30.0
}

fn multichannel_group_label(
    disc: &DvdaDisc,
    group: &DvdaGroup,
    fallback_channels: Option<u8>,
) -> String {
    for title_ref in &group.title_refs {
        let Ok(title_set) = find_title_set(disc, title_ref.title_set_nr) else {
            continue;
        };
        let present: Vec<&AudioAttributes> = title_set
            .audio_formats
            .iter()
            .filter(|attr| attr.present)
            .collect();
        if let [attr] = present.as_slice() {
            if let Some(assignment) = attr.channel_assignment.as_ref() {
                let total = assignment.group1_channels + assignment.group2_channels;
                if total > 2 {
                    return format!("{}ch", total);
                }
            }
        }
    }

    fallback_channels
        .map(|channels| format!("{channels}ch"))
        .unwrap_or_else(|| "multichannel".to_string())
}

fn prepared_tracks_for_groups(
    volume_source: &DvdaVolumeSourceRef,
    volume: &dyn DvdaVolume,
    disc: &DvdaDisc,
    groups: &[&DvdaGroup],
    requested_downmix_policy: DvdaDownmixPolicy,
    metabase: Option<&DvdaMetabase>,
    cancel: &CancellationToken,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    let mut tracks = Vec::new();
    for group in groups {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }
        let group_uses_disc_absolute = group_uses_disc_absolute_addressing(disc, group);
        let disc_info_probe = if group_uses_disc_absolute
            || matches!(requested_downmix_policy, DvdaDownmixPolicy::Auto)
        {
            let source_path = volume_source.original_container();
            let source_path = source_path.is_file().then_some(source_path.as_path());
            probe_group_aob_format_with_path(volume, disc, group, source_path)
        } else {
            None
        };
        let disc_info_downmix_source_label =
            if matches!(requested_downmix_policy, DvdaDownmixPolicy::Auto) {
                disc_info_probe.as_ref().and_then(|probe| {
                    if probe.codec.eq_ignore_ascii_case("MLP") && probe.channels > 2 {
                        probe.stereo_downmix_source_label.clone()
                    } else {
                        None
                    }
                })
            } else {
                None
            };
        let disc_absolute_stream_kind_hint = group_uses_disc_absolute
            .then(|| {
                disc_info_probe
                    .as_ref()
                    .and_then(|probe| elementary_stream_kind_hint_from_codec(probe.codec.as_ref()))
            })
            .flatten();
        append_tracks_for_group(
            volume_source,
            disc,
            group,
            requested_downmix_policy,
            disc_info_downmix_source_label.as_deref(),
            disc_absolute_stream_kind_hint,
            metabase,
            &mut tracks,
            cancel,
        )?;
    }
    if tracks.is_empty() {
        return Err(MaterializeError::Parse(
            "selected DVD-Audio groups contain no materializable ATS or SAMG tracks".to_string(),
        ));
    }
    Ok(tracks)
}

fn append_tracks_for_group(
    volume_source: &DvdaVolumeSourceRef,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    requested_downmix_policy: DvdaDownmixPolicy,
    disc_info_downmix_source_label: Option<&str>,
    disc_absolute_stream_kind_hint: Option<DvdaElementaryStreamKind>,
    metabase: Option<&DvdaMetabase>,
    tracks: &mut Vec<PreparedTrack>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    let start_len = tracks.len();

    if !group.title_refs.is_empty() {
        for title_ref in &group.title_refs {
            if cancel.is_cancelled() {
                return Err(MaterializeError::Cancelled);
            }
            let title_set = find_title_set(disc, title_ref.title_set_nr)?;
            let title = find_title(title_set, title_ref)?;
            append_title_tracks(
                volume_source,
                disc,
                group,
                title_ref,
                title_set,
                title,
                requested_downmix_policy,
                disc_info_downmix_source_label,
                disc_absolute_stream_kind_hint,
                metabase,
                tracks,
                start_len,
            )?;
        }
    } else if !group.samg_tracks.is_empty() {
        append_samg_only_tracks(
            volume_source,
            disc,
            group,
            requested_downmix_policy,
            tracks,
            cancel,
        )?;
    }

    if tracks.len() == start_len {
        return Err(MaterializeError::Parse(format!(
            "DVD-Audio group {} contains no materializable ATS or SAMG tracks",
            group.group_nr
        )));
    }

    Ok(())
}

fn append_title_tracks(
    volume_source: &DvdaVolumeSourceRef,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    title_ref: &TitleRef,
    title_set: &TitleSet,
    title: &AudioTitle,
    requested_downmix_policy: DvdaDownmixPolicy,
    disc_info_downmix_source_label: Option<&str>,
    disc_absolute_stream_kind_hint: Option<DvdaElementaryStreamKind>,
    metabase: Option<&DvdaMetabase>,
    tracks: &mut Vec<PreparedTrack>,
    group_start_len: usize,
) -> Result<(), MaterializeError> {
    let title_set_has_existing_aobs = title_set_has_existing_aobs(title_set);
    let relative_aob_files = existing_aob_file_refs(title_set);
    let samg_sector_correlation = if title_set_has_existing_aobs {
        None
    } else {
        find_samg_sector_correlation(disc, title)
    };
    let disc_absolute_base = if title_set_has_existing_aobs {
        None
    } else {
        Some(title_disc_absolute_sector_base(
            disc,
            group,
            title_ref,
            title_set,
            title,
            samg_sector_correlation.as_ref(),
        ).ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Audio ATS {} has no existing AOB files and no AMG/AOTT or SAMG sector base for group {} title {}",
                title_set.number, group.group_nr, title.title_ordinal
            ))
        })?)
    };
    let sector_address_space = if title_set_has_existing_aobs {
        DvdaSectorAddressSpace::AtsAobRelative {
            title_set_nr: title_set.number,
        }
    } else {
        DvdaSectorAddressSpace::DiscAbsolute {
            title_set_nr: title_set.number,
        }
    };
    let aob_files = if title_set_has_existing_aobs {
        relative_aob_files
    } else {
        Vec::new()
    };
    let track_stream_kind_hint = if matches!(
        sector_address_space,
        DvdaSectorAddressSpace::DiscAbsolute { .. }
    ) {
        samg_sector_correlation
            .as_ref()
            .and_then(SamgSectorCorrelation::elementary_stream_kind_hint)
            .or(disc_absolute_stream_kind_hint)
    } else {
        None
    };
    let group_total_tracks = ats_group_track_count(disc, group);

    for (chapter_index, chapter) in title.chapters.iter().enumerate() {
        if chapter.sector_ranges.is_empty() {
            return Err(MaterializeError::Parse(format!(
                "DVD-Audio ATS {} title {} track {} has no sector ranges",
                title_set.number, title.title_ordinal, chapter.track_nr
            )));
        }
        validate_sector_ranges_are_well_formed(chapter, title_set.number, title.title_ordinal)?;

        let source_ordinal = tracks.len() as u32 + 1;
        let group_track_ordinal = group_track_ordinal_from_lengths(tracks.len(), group_start_len)?;
        let correlated_samg_track = samg_sector_correlation
            .as_ref()
            .and_then(|correlation| correlation.track_for_chapter_index(chapter_index));
        let audio_facts = correlated_samg_track
            .map(audio_facts_for_samg_track)
            .unwrap_or_else(|| audio_facts_for_title_chapter(title_set, chapter));
        // Orphan PGC titles share a title set with another title whose IFO
        // audio format entry may describe a completely different presentation
        // (e.g., 5.1/96kHz vs stereo/88.2kHz). Clear all IFO-derived format
        // expectations so MLP/LPCM stream self-describes during realization.
        let audio_facts = if matches!(group.correlation, GroupCorrelation::OrphanPgcTitle) {
            unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats)
        } else {
            audio_facts
        };
        let source_audio = source_audio_descriptor_for_facts(audio_facts);
        let aob_inventory_covers_track =
            title_set_has_existing_aobs && sector_ranges_are_covered(chapter, &aob_files);
        let sector_ranges = sector_ranges_for_address_space(chapter, disc_absolute_base)?;
        let dvda_downmix_policy = resolve_dvda_track_downmix_policy(
            requested_downmix_policy,
            disc_info_downmix_source_label,
        );

        tracks.push(PreparedTrack {
            id: TrackId {
                source_ordinal,
                disc_number: disc_number(disc),
                track_number: source_ordinal,
            },
            source_ref: ats_track_source_ref(
                volume_source,
                group,
                title_set,
                title,
                chapter,
                group_track_ordinal,
                audio_facts,
                sector_address_space,
                sector_ranges,
                dvda_downmix_policy,
                track_stream_kind_hint,
                correlated_samg_track,
                aob_files.clone(),
            ),
            metadata: track_metadata(
                group,
                title_set,
                title,
                chapter,
                group_track_ordinal,
                audio_facts,
                aob_inventory_covers_track,
                source_ordinal,
                group_total_tracks,
                metabase,
            ),
            expected_samples: audio_facts
                .sample_rate
                .and_then(|rate| expected_samples_from_pts(chapter, rate)),
            sample_rate: audio_facts.sample_rate,
            source_audio,
            bit_depth: audio_facts.bit_depth.map(u32::from),
        });
    }
    Ok(())
}

fn append_samg_only_tracks(
    volume_source: &DvdaVolumeSourceRef,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    requested_downmix_policy: DvdaDownmixPolicy,
    tracks: &mut Vec<PreparedTrack>,
    cancel: &CancellationToken,
) -> Result<(), MaterializeError> {
    for samg_ref in &group.samg_tracks {
        if cancel.is_cancelled() {
            return Err(MaterializeError::Cancelled);
        }

        let samg_track = find_samg_track(disc, samg_ref)?;
        let source_ordinal = tracks.len() as u32 + 1;
        tracks.push(prepared_track_from_samg_track(
            volume_source,
            disc,
            group,
            samg_track,
            source_ordinal,
            u32::from(samg_track.track_nr),
            resolve_non_auto_downmix_policy(requested_downmix_policy),
        )?);
    }

    Ok(())
}

fn ats_track_source_ref(
    volume_source: &DvdaVolumeSourceRef,
    group: &DvdaGroup,
    title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
    group_track_ordinal: u32,
    audio_facts: AudioFacts<'_>,
    sector_address_space: DvdaSectorAddressSpace,
    sector_ranges: Vec<DvdaSectorRangeRef>,
    dvda_downmix_policy: DvdaDownmixPolicy,
    elementary_stream_kind_hint: Option<DvdaElementaryStreamKind>,
    correlated_samg_track: Option<&SamgTrack>,
    aob_files: Vec<DvdaAobFileRef>,
) -> TrackSourceRef {
    TrackSourceRef::DvdaTrack {
        volume_source: volume_source.clone(),
        group_nr: group.group_nr,
        title_set_nr: Some(title_set.number),
        title_nr: Some(title.title_nr),
        title_ordinal: Some(title.title_ordinal),
        group_track_ordinal,
        ats_track_nr: Some(chapter.track_nr),
        samg_track_nr: correlated_samg_track
            .map(|track| track.track_nr)
            .or_else(|| matching_samg_track_nr(group, group_track_ordinal)),
        samg_ordinal: correlated_samg_track
            .map(|track| track.ordinal)
            .or_else(|| matching_samg_ordinal(group, group_track_ordinal)),
        sector_address_space,
        elementary_stream_kind_hint,
        first_pts: chapter.first_pts,
        len_in_pts: chapter.len_in_pts,
        track_type: Some(chapter.track_type),
        index_start: Some(chapter.index_start),
        downmix_matrix: chapter.downmix_matrix,
        dvda_downmix_policy,
        title_table_offset: Some(title.title_table_offset),
        title_len_in_pts: Some(title.len_in_pts),
        title_track_count_declared: Some(title.track_count_declared),
        title_index_count_declared: Some(title.index_count_declared),
        audio_format_index: audio_facts.format_index,
        expected_sample_rate: audio_facts.sample_rate,
        expected_channel_count: expected_channel_count_for_facts(audio_facts),
        expected_bit_depth: audio_facts.bit_depth.map(u32::from),
        expected_channel_assignment_code: expected_channel_assignment_code_for_facts(audio_facts),
        expected_group1_sample_rate: expected_group1_sample_rate_for_facts(audio_facts),
        expected_group2_sample_rate: expected_group2_sample_rate_for_facts(audio_facts),
        expected_group1_bit_depth: expected_group1_bit_depth_for_facts(audio_facts),
        expected_group2_bit_depth: expected_group2_bit_depth_for_facts(audio_facts),
        expected_group1_channel_count: expected_group1_channel_count_for_facts(audio_facts),
        expected_group2_channel_count: expected_group2_channel_count_for_facts(audio_facts),
        sector_ranges,
        aob_files,
    }
}

fn prepared_track_from_samg_track(
    volume_source: &DvdaVolumeSourceRef,
    disc: &DvdaDisc,
    group: &DvdaGroup,
    samg_track: &SamgTrack,
    source_ordinal: u32,
    group_track_ordinal: u32,
    dvda_downmix_policy: DvdaDownmixPolicy,
) -> Result<PreparedTrack, MaterializeError> {
    if samg_track.abs_last_sector < samg_track.abs_first_sector {
        return Err(MaterializeError::Parse(format!(
            "DVD-Audio SAMG group {} track {} has an inverted sector range {}-{}",
            samg_track.group_nr,
            samg_track.track_nr,
            samg_track.abs_first_sector,
            samg_track.abs_last_sector
        )));
    }

    let audio_facts = audio_facts_for_samg_track(samg_track);
    let source_audio = source_audio_descriptor_for_facts(audio_facts);

    Ok(PreparedTrack {
        id: TrackId {
            source_ordinal,
            disc_number: disc_number(disc),
            track_number: source_ordinal,
        },
        source_ref: TrackSourceRef::DvdaTrack {
            volume_source: volume_source.clone(),
            group_nr: group.group_nr,
            title_set_nr: None,
            title_nr: None,
            title_ordinal: None,
            group_track_ordinal,
            ats_track_nr: None,
            samg_track_nr: Some(samg_track.track_nr),
            samg_ordinal: Some(samg_track.ordinal),
            sector_address_space: DvdaSectorAddressSpace::SamgAbsolute,
            elementary_stream_kind_hint: None,
            first_pts: samg_track.first_pts,
            len_in_pts: samg_track.len_in_pts,
            track_type: None,
            index_start: None,
            downmix_matrix: None,
            dvda_downmix_policy,
            title_table_offset: None,
            title_len_in_pts: None,
            title_track_count_declared: None,
            title_index_count_declared: None,
            audio_format_index: None,
            expected_sample_rate: audio_facts.sample_rate,
            expected_channel_count: expected_channel_count_for_facts(audio_facts),
            expected_bit_depth: audio_facts.bit_depth.map(u32::from),
            expected_channel_assignment_code: expected_channel_assignment_code_for_facts(
                audio_facts,
            ),
            expected_group1_sample_rate: expected_group1_sample_rate_for_facts(audio_facts),
            expected_group2_sample_rate: expected_group2_sample_rate_for_facts(audio_facts),
            expected_group1_bit_depth: expected_group1_bit_depth_for_facts(audio_facts),
            expected_group2_bit_depth: expected_group2_bit_depth_for_facts(audio_facts),
            expected_group1_channel_count: expected_group1_channel_count_for_facts(audio_facts),
            expected_group2_channel_count: expected_group2_channel_count_for_facts(audio_facts),
            sector_ranges: vec![DvdaSectorRangeRef {
                index_nr: 1,
                first: samg_track.abs_first_sector,
                last: samg_track.abs_last_sector,
            }],
            aob_files: Vec::new(),
        },
        metadata: samg_track_metadata(
            group,
            samg_track,
            audio_facts,
            source_ordinal,
            group_track_ordinal,
        ),
        expected_samples: audio_facts
            .sample_rate
            .and_then(|rate| expected_samples_from_pts_len(samg_track.len_in_pts, rate)),
        sample_rate: audio_facts.sample_rate,
        source_audio,
        bit_depth: audio_facts.bit_depth.map(u32::from),
    })
}

fn find_samg_track<'a>(
    disc: &'a DvdaDisc,
    samg_ref: &SamgTrackRef,
) -> Result<&'a SamgTrack, MaterializeError> {
    let samg = disc.samg.as_ref().ok_or_else(|| {
        MaterializeError::Parse(format!(
            "DVD-Audio group {} references SAMG track {}, but AUDIO_PP.IFO was not parsed",
            samg_ref.group_nr, samg_ref.track_nr
        ))
    })?;

    samg.tracks
        .iter()
        .find(|track| {
            track.ordinal == samg_ref.samg_ordinal
                && track.group_nr == samg_ref.group_nr
                && track.track_nr == samg_ref.track_nr
        })
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Audio group {} references missing SAMG ordinal {} track {}",
                samg_ref.group_nr, samg_ref.samg_ordinal, samg_ref.track_nr
            ))
        })
}

fn group_track_ordinal_from_lengths(
    current_len: usize,
    group_start_len: usize,
) -> Result<u32, MaterializeError> {
    let local_index = current_len.checked_sub(group_start_len).ok_or_else(|| {
        MaterializeError::Parse("DVD-Audio group-track ordinal underflow".to_string())
    })?;
    u32::try_from(local_index + 1).map_err(|_| {
        MaterializeError::Parse("DVD-Audio group-track ordinal exceeds u32".to_string())
    })
}

fn matching_samg_track_ref(group: &DvdaGroup, group_track_ordinal: u32) -> Option<&SamgTrackRef> {
    group
        .samg_tracks
        .iter()
        .find(|samg| u32::from(samg.track_nr) == group_track_ordinal)
}

fn matching_samg_track_nr(group: &DvdaGroup, group_track_ordinal: u32) -> Option<u8> {
    matching_samg_track_ref(group, group_track_ordinal).map(|samg| samg.track_nr)
}

fn matching_samg_ordinal(group: &DvdaGroup, group_track_ordinal: u32) -> Option<u16> {
    matching_samg_track_ref(group, group_track_ordinal).map(|samg| samg.samg_ordinal)
}

fn find_title_set(disc: &DvdaDisc, title_set_nr: u8) -> Result<&TitleSet, MaterializeError> {
    disc.title_sets
        .iter()
        .find(|title_set| title_set.number == title_set_nr)
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Audio group references missing ATS {title_set_nr}"
            ))
        })
}

fn find_title<'a>(
    title_set: &'a TitleSet,
    title_ref: &TitleRef,
) -> Result<&'a AudioTitle, MaterializeError> {
    title_set
        .titles
        .iter()
        .find(|title| match title_ref.kind {
            TitleRefKind::AottTitleOrdinal => title.title_ordinal == title_ref.title_nr,
            TitleRefKind::AtsPgcTitleNr => title.title_nr == title_ref.title_nr,
        })
        .ok_or_else(|| {
            MaterializeError::Parse(format!(
                "DVD-Audio group references missing ATS {} {:?} title {}",
                title_set.number, title_ref.kind, title_ref.title_nr
            ))
        })
}

fn validate_sector_ranges_are_well_formed(
    chapter: &AudioChapter,
    title_set_nr: u8,
    title_ordinal: u8,
) -> Result<(), MaterializeError> {
    for range in &chapter.sector_ranges {
        if range.block_count() == 0 {
            return Err(MaterializeError::Parse(format!(
                "DVD-Audio ATS {title_set_nr} title {title_ordinal} track {} has an empty sector range",
                chapter.track_nr
            )));
        }
    }
    Ok(())
}

fn sector_ranges_are_covered(chapter: &AudioChapter, aob_files: &[DvdaAobFileRef]) -> bool {
    chapter
        .sector_ranges
        .iter()
        .all(|range| sector_range_is_covered(range.first, range.last, aob_files))
}

fn sector_range_is_covered(first: u32, last: u32, aob_files: &[DvdaAobFileRef]) -> bool {
    if last < first {
        return false;
    }
    let mut cursor = first;
    loop {
        let Some(aob) = aob_files
            .iter()
            .filter(|aob| aob.exists)
            .find(|aob| aob.contains(cursor))
        else {
            return false;
        };
        if aob.block_last >= last {
            return true;
        }
        if aob.block_last == u32::MAX {
            return false;
        }
        cursor = aob.block_last + 1;
    }
}

#[derive(Clone, Copy)]
struct AudioFacts<'a> {
    format_index: Option<u8>,
    attr: Option<&'a AudioAttributes>,
    channel_format: Option<&'a ChannelFormat>,
    channel_assignment: Option<&'a ChannelAssignment>,
    sample_rate: Option<u32>,
    bit_depth: Option<u8>,
    resolution: AudioFormatResolution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioFormatResolution {
    NoPresentFormat,
    SinglePresentFormat,
    TrackTypeAudioFormatIndex,
    MultiplePresentFormats,
    SamgTrackRecord,
}

#[allow(dead_code)]
fn audio_facts_for_title_set(title_set: &TitleSet) -> AudioFacts<'_> {
    let present: Vec<&AudioAttributes> = present_audio_formats(title_set);

    match present.as_slice() {
        [] => unknown_audio_facts(AudioFormatResolution::NoPresentFormat),
        [attr] => audio_facts_from_attr(*attr, AudioFormatResolution::SinglePresentFormat),
        _ => unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats),
    }
}

fn audio_facts_for_title_chapter<'a>(
    title_set: &'a TitleSet,
    chapter: &AudioChapter,
) -> AudioFacts<'a> {
    if track_type_signals_alternate_presentation(chapter.track_type) {
        return unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats);
    }

    let present: Vec<&'a AudioAttributes> = present_audio_formats(title_set);

    match present.as_slice() {
        [] => unknown_audio_facts(AudioFormatResolution::NoPresentFormat),
        [attr] => audio_facts_from_attr(*attr, AudioFormatResolution::SinglePresentFormat),
        _ => {
            let candidate = chapter.track_type_low_bits_candidate;
            present
                .iter()
                .copied()
                .find(|attr| attr.format_index == candidate)
                .map(|attr| {
                    audio_facts_from_attr(attr, AudioFormatResolution::TrackTypeAudioFormatIndex)
                })
                .unwrap_or_else(|| {
                    unknown_audio_facts(AudioFormatResolution::MultiplePresentFormats)
                })
        }
    }
}

fn track_type_signals_alternate_presentation(track_type: u8) -> bool {
    (track_type & DVDA_TRACK_TYPE_ALTERNATE_PRESENTATION_BIT) != 0
}

fn present_audio_formats(title_set: &TitleSet) -> Vec<&AudioAttributes> {
    title_set
        .audio_formats
        .iter()
        .filter(|attr| attr.present)
        .collect()
}

fn audio_facts_from_attr(
    attr: &AudioAttributes,
    resolution: AudioFormatResolution,
) -> AudioFacts<'_> {
    AudioFacts {
        format_index: Some(attr.format_index),
        attr: Some(attr),
        channel_format: Some(&attr.channel_format),
        channel_assignment: attr.channel_assignment.as_ref(),
        sample_rate: primary_sample_rate(&attr.channel_format),
        bit_depth: primary_bit_depth(&attr.channel_format),
        resolution,
    }
}

fn unknown_audio_facts<'a>(resolution: AudioFormatResolution) -> AudioFacts<'a> {
    AudioFacts {
        format_index: None,
        attr: None,
        channel_format: None,
        channel_assignment: None,
        sample_rate: None,
        bit_depth: None,
        resolution,
    }
}

fn audio_facts_for_samg_track(track: &SamgTrack) -> AudioFacts<'_> {
    AudioFacts {
        format_index: None,
        attr: None,
        channel_format: Some(&track.channel_format),
        channel_assignment: track.channel_assignment.as_ref(),
        sample_rate: primary_sample_rate(&track.channel_format),
        bit_depth: primary_bit_depth(&track.channel_format),
        resolution: AudioFormatResolution::SamgTrackRecord,
    }
}

fn expected_channel_count_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts.channel_assignment.and_then(|assignment| {
        let channels = channel_count(assignment);
        (channels > 0).then_some(u32::from(channels))
    })
}

fn expected_channel_assignment_code_for_facts(audio_facts: AudioFacts<'_>) -> Option<u8> {
    audio_facts
        .channel_format
        .map(|format| format.assignment_code)
}

fn expected_group1_sample_rate_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts
        .channel_format
        .and_then(|format| format.group1_sample_rate)
}

fn expected_group2_sample_rate_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts
        .channel_format
        .and_then(|format| format.group2_sample_rate)
}

fn expected_group1_bit_depth_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts
        .channel_format
        .and_then(|format| format.group1_bits)
        .map(u32::from)
}

fn expected_group2_bit_depth_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts
        .channel_format
        .and_then(|format| format.group2_bits)
        .map(u32::from)
}

fn expected_group1_channel_count_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts.channel_assignment.and_then(|assignment| {
        (assignment.group1_channels > 0).then_some(u32::from(assignment.group1_channels))
    })
}

fn expected_group2_channel_count_for_facts(audio_facts: AudioFacts<'_>) -> Option<u32> {
    audio_facts.channel_assignment.and_then(|assignment| {
        (assignment.group2_channels > 0).then_some(u32::from(assignment.group2_channels))
    })
}

fn source_audio_descriptor_for_facts(audio_facts: AudioFacts<'_>) -> SourceAudioDescriptor {
    let channel_groups = audio_facts
        .channel_format
        .map(|format| channel_group_descriptors(format, audio_facts.channel_assignment))
        .unwrap_or_default();

    SourceAudioDescriptor {
        coding: Some(SourceAudioCoding::DvdaUnknown),
        channel_groups,
        primary_sample_rate: audio_facts.sample_rate,
        bit_depth: audio_facts.bit_depth.map(u32::from),
    }
}

fn channel_group_descriptors(
    format: &ChannelFormat,
    assignment: Option<&ChannelAssignment>,
) -> Vec<ChannelGroupDescriptor> {
    let mut groups = Vec::new();
    maybe_push_channel_group(
        &mut groups,
        1,
        format.group1_sample_rate,
        format.group1_bits,
        assignment.and_then(|value| (value.group1_channels > 0).then_some(value.group1_channels)),
        assignment.and_then(|value| channel_assignment_label(value.group1)),
    );
    maybe_push_channel_group(
        &mut groups,
        2,
        format.group2_sample_rate,
        format.group2_bits,
        assignment.and_then(|value| (value.group2_channels > 0).then_some(value.group2_channels)),
        assignment.and_then(|value| channel_assignment_label(value.group2)),
    );
    groups
}

fn maybe_push_channel_group(
    groups: &mut Vec<ChannelGroupDescriptor>,
    group_nr: u8,
    sample_rate: Option<u32>,
    bit_depth: Option<u8>,
    channels: Option<u8>,
    assignment: Option<String>,
) {
    if sample_rate.is_none() && bit_depth.is_none() && channels.is_none() && assignment.is_none() {
        return;
    }

    groups.push(ChannelGroupDescriptor {
        group_nr,
        channels,
        assignment,
        sample_rate,
        bit_depth: bit_depth.map(u32::from),
    });
}

fn channel_assignment_label(channels: &[&str]) -> Option<String> {
    if channels.is_empty() {
        None
    } else {
        Some(channels.join(","))
    }
}

fn primary_sample_rate(format: &ChannelFormat) -> Option<u32> {
    match (format.group1_sample_rate, format.group2_sample_rate) {
        (Some(group1), Some(group2)) if group1 != group2 => None,
        (Some(group1), _) => Some(group1),
        (None, Some(group2)) => Some(group2),
        (None, None) => None,
    }
}

fn primary_bit_depth(format: &ChannelFormat) -> Option<u8> {
    match (format.group1_bits, format.group2_bits) {
        (Some(group1), Some(group2)) if group1 != group2 => None,
        (Some(group1), _) => Some(group1),
        (None, Some(group2)) => Some(group2),
        (None, None) => None,
    }
}

fn audio_format_resolution_label(resolution: AudioFormatResolution) -> &'static str {
    match resolution {
        AudioFormatResolution::NoPresentFormat => "no_present_format",
        AudioFormatResolution::SinglePresentFormat => "single_present_format",
        AudioFormatResolution::TrackTypeAudioFormatIndex => "track_type_audio_format_index",
        AudioFormatResolution::MultiplePresentFormats => {
            "multiple_present_formats_unknown_until_aob_demux"
        }
        AudioFormatResolution::SamgTrackRecord => "samg_track_record",
    }
}

fn expected_samples_from_pts(chapter: &AudioChapter, sample_rate: u32) -> Option<u64> {
    expected_samples_from_pts_len(chapter.len_in_pts, sample_rate)
}

fn expected_samples_from_pts_len(len_in_pts: u32, sample_rate: u32) -> Option<u64> {
    let numerator = u64::from(len_in_pts) * u64::from(sample_rate);
    if numerator % PTS_PER_SECOND == 0 {
        Some(numerator / PTS_PER_SECOND)
    } else {
        None
    }
}

#[allow(dead_code)]
fn sector_range_ref(range: &crate::tui::dvda::SectorRange) -> DvdaSectorRangeRef {
    DvdaSectorRangeRef {
        index_nr: range.index_nr,
        first: range.first,
        last: range.last,
    }
}

fn aob_file_ref(entry: &AobFileEntry) -> DvdaAobFileRef {
    DvdaAobFileRef {
        title_set_nr: entry.title_set_nr,
        part_nr: entry.part_nr,
        file_name: entry.file_name.clone(),
        exists: entry.exists,
        byte_len: entry.byte_len,
        block_first: entry.block_first,
        block_last: entry.block_last,
    }
}

fn ats_group_track_count(disc: &DvdaDisc, group: &DvdaGroup) -> u32 {
    let mut count = 0_u32;
    for title_ref in &group.title_refs {
        let Some(title_set) = disc
            .title_sets
            .iter()
            .find(|title_set| title_set.number == title_ref.title_set_nr)
        else {
            continue;
        };
        let title = match title_ref.kind {
            TitleRefKind::AottTitleOrdinal => title_set
                .titles
                .iter()
                .find(|title| title.title_ordinal == title_ref.title_nr),
            TitleRefKind::AtsPgcTitleNr => title_set
                .titles
                .iter()
                .find(|title| title.title_nr == title_ref.title_nr),
        };
        if let Some(title) = title {
            count = count.saturating_add(title.chapters.len() as u32);
        }
    }
    count
}

fn album_metadata(
    disc: &DvdaDisc,
    groups: &[&DvdaGroup],
    group_selection: DvdaGroupSelection,
    total_tracks: u32,
    tracks: &[PreparedTrack],
    metabase: Option<&DvdaMetabase>,
    loaded_metabase: Option<&LoadedDvdaMetabase>,
) -> AlbumMetadata {
    let mut extra = BTreeMap::new();
    let selected_group_numbers = groups
        .iter()
        .map(|group| group.group_nr.to_string())
        .collect::<Vec<_>>()
        .join(",");
    insert_nonempty(&mut extra, "dvda_group_selection", group_selection.label());
    insert_nonempty(&mut extra, "dvda_selected_groups", selected_group_numbers);
    insert_nonempty(
        &mut extra,
        "dvda_selected_group_count",
        groups.len().to_string(),
    );
    if let [group] = groups {
        insert_nonempty(&mut extra, "dvda_group", group.group_nr.to_string());
        insert_nonempty(
            &mut extra,
            "dvda_group_correlation",
            group_correlation_label(&group.correlation).to_string(),
        );
    }
    insert_nonempty(
        &mut extra,
        "dvda_group_count",
        disc.groups.len().to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_audio_title_sets",
        disc.amg.audio_title_sets.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_video_title_sets",
        disc.amg.video_title_sets.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_provider_identifier",
        disc.amg.provider_identifier.clone(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_specification_version",
        format!("0x{:02x}", disc.amg.specification_version),
    );
    insert_nonempty(
        &mut extra,
        "dvda_mkb_present",
        disc.copy_protection.mkb_present.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_cppm_detected",
        disc.copy_protection.cppm_detected.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_copy_protection_source",
        format!("{:?}", disc.copy_protection.source),
    );
    if disc.copy_protection.cppm_detected {
        insert_nonempty(
            &mut extra,
            "dvda_copy_protection_policy",
            "DetectExplainSkip".to_string(),
        );
        insert_nonempty(
            &mut extra,
            "dvda_cppm_decryption_supported",
            "false".to_string(),
        );
        insert_nonempty(
            &mut extra,
            "dvda_copy_protection_skip_reason",
            "CPPM-protected DVD-Audio source; this build detects, explains, and skips protected AOB realization instead of attempting decryption".to_string(),
        );
    }
    if disc.copy_protection.mkb_present {
        insert_nonempty(
            &mut extra,
            "dvda_copy_protection_file",
            "DVDAUDIO.MKB".to_string(),
        );
    }
    insert_nonempty(
        &mut extra,
        "dvda_supplemental_video_ifo_present",
        disc.supplemental_video_ifo_present.to_string(),
    );

    let base = AlbumMetadata {
        album: None,
        album_artist: None,
        genre: None,
        date: None,
        total_tracks,
        total_discs: None,
        disc_number: disc_number(disc),
        extra,
    };
    let mut album =
        materializer_dvda_metabase::overlay_album_metadata(base, metabase, loaded_metabase);

    // foo_input_dvda stores album-like values per track, and one XML can contain
    // multiple DVD-Audio presentations. Scope album values to the selected
    // PreparedTracks so the standard ALBUM tag and folder template reflect the
    // stream being converted instead of leaking a sibling group or falling back
    // to the ISO/file stem.
    overlay_selected_track_album_values(&mut album, metabase, tracks);

    album
}

fn overlay_selected_track_album_values(
    album: &mut AlbumMetadata,
    metabase: Option<&DvdaMetabase>,
    tracks: &[PreparedTrack],
) {
    let track_ids = selected_metabase_track_ids(tracks);

    if let Some(value) = dvda_metabase::album_value_for_track_ids(metabase, &track_ids, &["ALBUM"])
        .or_else(|| common_selected_track_extra_value(tracks, "dvda_metabase_album"))
    {
        album.album = Some(value);
    }
    if let Some(value) = dvda_metabase::album_value_for_track_ids(
        metabase,
        &track_ids,
        &["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"],
    ) {
        album.album_artist = Some(value);
    }
    if let Some(value) = dvda_metabase::album_value_for_track_ids(metabase, &track_ids, &["GENRE"])
    {
        album.genre = Some(value);
    }
    if let Some(value) =
        dvda_metabase::album_value_for_track_ids(metabase, &track_ids, &["DATE", "YEAR"])
    {
        album.date = Some(value);
    }
}

fn selected_metabase_track_ids(tracks: &[PreparedTrack]) -> Vec<String> {
    tracks
        .iter()
        .filter_map(|track| track.metadata.extra.get("dvda_track_id"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn common_selected_track_extra_value(tracks: &[PreparedTrack], key: &str) -> Option<String> {
    let mut values = tracks
        .iter()
        .filter_map(|track| track.metadata.extra.get(key))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());

    let first = values.next()?.to_string();
    if values.all(|value| value == first) {
        Some(first)
    } else {
        None
    }
}

fn track_metadata(
    group: &DvdaGroup,
    title_set: &TitleSet,
    title: &AudioTitle,
    chapter: &AudioChapter,
    group_track_ordinal: u32,
    audio_facts: AudioFacts<'_>,
    aob_inventory_covers_track: bool,
    source_ordinal: u32,
    total_tracks: u32,
    metabase: Option<&DvdaMetabase>,
) -> TrackMetadata {
    let mut extra = BTreeMap::new();
    insert_nonempty(&mut extra, "dvda_group", group.group_nr.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_group_correlation",
        group_correlation_label(&group.correlation).to_string(),
    );
    insert_nonempty(&mut extra, "dvda_origin", "atsi".to_string());
    insert_nonempty(
        &mut extra,
        "dvda_sector_address_space",
        "ats_aob_relative".to_string(),
    );
    insert_nonempty(&mut extra, "dvda_title_set", title_set.number.to_string());
    insert_nonempty(&mut extra, "dvda_title_nr", title.title_nr.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_title_ordinal",
        title.title_ordinal.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_group_track_ordinal",
        group_track_ordinal.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_ats_track_nr",
        chapter.track_nr.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_track_type",
        format!("0x{:02x}", chapter.track_type),
    );
    insert_nonempty(
        &mut extra,
        "dvda_index_start",
        chapter.index_start.to_string(),
    );
    insert_nonempty(&mut extra, "dvda_first_pts", chapter.first_pts.to_string());
    insert_nonempty(&mut extra, "dvda_len_pts", chapter.len_in_pts.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_sector_range_count",
        chapter.sector_ranges.len().to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_sector_blocks",
        sector_block_count(chapter).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_aob_inventory_covers_track",
        aob_inventory_covers_track.to_string(),
    );
    if let Some(first) = chapter.first_sector() {
        insert_nonempty(&mut extra, "dvda_first_sector", first.to_string());
    }
    if let Some(last) = chapter.last_sector() {
        insert_nonempty(&mut extra, "dvda_last_sector", last.to_string());
    }
    if let Some(index) = audio_facts.format_index {
        insert_nonempty(&mut extra, "dvda_audio_format_index", index.to_string());
    }
    insert_nonempty(
        &mut extra,
        "dvda_audio_format_known",
        audio_facts.format_index.is_some().to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_audio_format_resolution",
        audio_format_resolution_label(audio_facts.resolution).to_string(),
    );
    if let Some(attr) = audio_facts.attr {
        insert_audio_attributes(&mut extra, attr);
    }

    let base = TrackMetadata {
        title: None,
        artist: None,
        album_artist: None,
        composer: None,
        performer: None,
        genre: None,
        date: None,
        track_number: Some(source_ordinal),
        disc_number: None,
        isrc: None,
        publisher: None,
        copyright: None,
        comment: None,
        pre_emphasis: false,
        extra,
    };

    materializer_dvda_metabase::overlay_track_metadata(
        base,
        &DvdaTrackMetadataKeys {
            group_nr: group.group_nr,
            titleset: title_set.number,
            title: title.title_ordinal,
            track: chapter.track_nr,
            source_ordinal,
            track_number: source_ordinal,
            total_tracks,
            sample_rate: audio_facts.sample_rate,
            bit_depth: audio_facts.bit_depth.map(u32::from),
            channel_count: expected_channel_count_for_facts(audio_facts).map(|c| c as u8),
            codec: audio_facts.attr.map(|_| "DVD-Audio".to_string()),
            channel_layout: audio_facts.channel_assignment.map(channel_layout),
        },
        metabase,
    )
}

fn samg_track_metadata(
    group: &DvdaGroup,
    track: &SamgTrack,
    audio_facts: AudioFacts<'_>,
    source_ordinal: u32,
    group_track_ordinal: u32,
) -> TrackMetadata {
    let mut extra = BTreeMap::new();
    insert_nonempty(&mut extra, "dvda_group", group.group_nr.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_group_correlation",
        group_correlation_label(&group.correlation).to_string(),
    );
    insert_nonempty(&mut extra, "dvda_origin", "samg".to_string());
    insert_nonempty(
        &mut extra,
        "dvda_sector_address_space",
        "samg_absolute".to_string(),
    );
    insert_nonempty(&mut extra, "dvda_samg_ordinal", track.ordinal.to_string());
    insert_nonempty(&mut extra, "dvda_samg_group", track.group_nr.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_group_track_ordinal",
        group_track_ordinal.to_string(),
    );
    insert_nonempty(&mut extra, "dvda_samg_track_nr", track.track_nr.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_samg_zone",
        samg_zone_label(&track.zone).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_samg_flags",
        format!("0x{:02x}", track.flags),
    );
    insert_nonempty(&mut extra, "dvda_first_pts", track.first_pts.to_string());
    insert_nonempty(&mut extra, "dvda_len_pts", track.len_in_pts.to_string());
    insert_nonempty(
        &mut extra,
        "dvda_first_sector",
        track.abs_first_sector.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_last_sector",
        track.abs_last_sector.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_samg_abs_first_sector_dup",
        track.abs_first_sector_dup.to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_sector_blocks",
        samg_sector_block_count(track).to_string(),
    );
    insert_nonempty(
        &mut extra,
        "dvda_aob_inventory_covers_track",
        "false".to_string(),
    );
    insert_nonempty(&mut extra, "dvda_audio_format_known", "false".to_string());
    insert_nonempty(
        &mut extra,
        "dvda_audio_format_resolution",
        audio_format_resolution_label(audio_facts.resolution).to_string(),
    );
    insert_channel_format(
        &mut extra,
        &track.channel_format,
        track.channel_assignment.as_ref(),
    );

    TrackMetadata {
        title: None,
        artist: None,
        album_artist: None,
        composer: None,
        performer: None,
        genre: None,
        date: None,
        track_number: Some(source_ordinal),
        disc_number: None,
        isrc: None,
        publisher: None,
        copyright: None,
        comment: None,
        pre_emphasis: false,
        extra,
    }
}

fn insert_audio_attributes(extra: &mut BTreeMap<String, String>, attr: &AudioAttributes) {
    insert_nonempty(
        extra,
        "dvda_audio_type_raw",
        format!("0x{:04x}", attr.audio_type_raw),
    );
    insert_channel_format(
        extra,
        &attr.channel_format,
        attr.channel_assignment.as_ref(),
    );
}

fn insert_channel_format(
    extra: &mut BTreeMap<String, String>,
    format: &ChannelFormat,
    assignment: Option<&ChannelAssignment>,
) {
    if let Some(rate) = format.group1_sample_rate {
        insert_nonempty(extra, "dvda_group1_sample_rate", rate.to_string());
    }
    if let Some(rate) = format.group2_sample_rate {
        insert_nonempty(extra, "dvda_group2_sample_rate", rate.to_string());
    }
    if let Some(bits) = format.group1_bits {
        insert_nonempty(extra, "dvda_group1_bit_depth", bits.to_string());
    }
    if let Some(bits) = format.group2_bits {
        insert_nonempty(extra, "dvda_group2_bit_depth", bits.to_string());
    }
    insert_nonempty(
        extra,
        "dvda_channel_assignment_code",
        format.assignment_code.to_string(),
    );
    if let Some(assignment) = assignment {
        insert_nonempty(
            extra,
            "dvda_channel_count",
            channel_count(assignment).to_string(),
        );
        insert_nonempty(extra, "dvda_channel_layout", channel_layout(assignment));
    }
}

fn samg_zone_label(zone: &SamgZone) -> &'static str {
    match zone {
        SamgZone::Aob => "aob",
        SamgZone::Vob => "vob",
    }
}

fn samg_sector_block_count(track: &SamgTrack) -> u64 {
    if track.abs_last_sector < track.abs_first_sector {
        0
    } else {
        u64::from(track.abs_last_sector) - u64::from(track.abs_first_sector) + 1
    }
}

fn sector_block_count(chapter: &AudioChapter) -> u64 {
    chapter
        .sector_ranges
        .iter()
        .map(|range| u64::from(range.block_count()))
        .sum()
}

fn channel_count(assignment: &ChannelAssignment) -> u8 {
    assignment.group1_channels + assignment.group2_channels
}

fn channel_layout(assignment: &ChannelAssignment) -> String {
    let group1 = assignment.group1.join(",");
    let group2 = assignment.group2.join(",");
    if group2.is_empty() {
        group1
    } else if group1.is_empty() {
        group2
    } else {
        format!("{group1}+{group2}")
    }
}

fn group_correlation_label(correlation: &GroupCorrelation) -> &'static str {
    match correlation {
        GroupCorrelation::FromAmgAott => "amg_aott",
        GroupCorrelation::FromAtsiFallback => "atsi_fallback",
        GroupCorrelation::OrphanPgcTitle => "orphan_pgc_title",
        GroupCorrelation::SamgOnly => "samg_only",
        GroupCorrelation::MixedAmgAndSamg => "mixed_amg_samg",
    }
}

fn disc_number(disc: &DvdaDisc) -> Option<u32> {
    if disc.amg.nr_of_volumes > 1 {
        Some(u32::from(disc.amg.this_volume_nr))
    } else {
        None
    }
}

fn apply_track_selection(
    tracks: Vec<PreparedTrack>,
    selection: &TrackSelection,
) -> Result<Vec<PreparedTrack>, MaterializeError> {
    match selection {
        TrackSelection::All => Ok(tracks),
        TrackSelection::Range { start, end } => {
            if *start == 0 || *end == 0 || start > end {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "invalid range {start}-{end}"
                )));
            }
            let max_ordinal = tracks.len() as u32;
            if *start > max_ordinal {
                return Err(MaterializeError::InvalidTrackSelection(format!(
                    "range start {start} exceeds track count {max_ordinal}"
                )));
            }
            Ok(tracks
                .into_iter()
                .filter(|track| {
                    track.id.source_ordinal >= *start && track.id.source_ordinal <= *end
                })
                .collect())
        }
        TrackSelection::Set(indices) => {
            if indices.is_empty() {
                return Err(MaterializeError::InvalidTrackSelection(
                    "empty track set".to_string(),
                ));
            }
            let max_ordinal = tracks.len() as u32;
            for &idx in indices {
                if idx == 0 || idx > max_ordinal {
                    return Err(MaterializeError::InvalidTrackSelection(format!(
                        "track {idx} outside valid range 1-{max_ordinal}"
                    )));
                }
            }
            Ok(tracks
                .into_iter()
                .filter(|track| indices.contains(&track.id.source_ordinal))
                .collect())
        }
    }
}

fn insert_nonempty(extra: &mut BTreeMap<String, String>, key: &str, value: String) {
    if !value.trim().is_empty() {
        extra.insert(key.to_string(), value);
    }
}

fn dvda_error_to_materialize(err: DvdaError) -> MaterializeError {
    let message = err.to_string();
    match err {
        DvdaError::Io { .. } => MaterializeError::Extraction(message),
        DvdaError::MissingFile { .. }
        | DvdaError::InvalidIdentifier { .. }
        | DvdaError::ShortRead { .. }
        | DvdaError::OutOfBounds { .. }
        | DvdaError::Parse { .. }
        | DvdaError::Unsupported { .. }
        | DvdaError::Iso { .. } => MaterializeError::Parse(message),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn contains_magic_for_test(bytes: &[u8]) -> bool {
        contains_subslice(bytes, DVDA_AMG_MAGIC)
    }

    pub(crate) fn default_group_for_test(disc: &DvdaDisc) -> Result<u8, MaterializeError> {
        select_group(disc, None).map(|group| group.group_nr)
    }

    pub(crate) fn selected_ordinals_for_test(
        tracks: Vec<PreparedTrack>,
        selection: &TrackSelection,
    ) -> Result<Vec<u32>, MaterializeError> {
        apply_track_selection(tracks, selection).map(|tracks| {
            tracks
                .into_iter()
                .map(|track| track.id.source_ordinal)
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dvda::{
        AmgInfo, AmgPointers, AtsiHeader, AudioCoding, CopyProtectionInfo, CopyProtectionSource,
        SamgCopyValidation, SamgInfo, TitleSetKind,
    };
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    const NO_CHANNELS: &[&str] = &[];
    const STEREO_CHANNELS: &[&str] = &["L", "R"];
    const SURROUND_CHANNELS: &[&str] = &["L", "R", "C", "LFE", "Ls", "Rs"];

    fn audio_attr(index: u8, sample_rate: Option<u32>, bit_depth: Option<u8>) -> AudioAttributes {
        audio_attr_with_channels(index, sample_rate, bit_depth, None)
    }

    fn audio_attr_with_channels(
        index: u8,
        sample_rate: Option<u32>,
        bit_depth: Option<u8>,
        channels: Option<u8>,
    ) -> AudioAttributes {
        let channel_assignment = channels.map(|channels| {
            if channels == 2 {
                ChannelAssignment {
                    code: 1,
                    group1: STEREO_CHANNELS,
                    group2: NO_CHANNELS,
                    group1_channels: 2,
                    group2_channels: 0,
                }
            } else {
                ChannelAssignment {
                    code: 2,
                    group1: SURROUND_CHANNELS,
                    group2: NO_CHANNELS,
                    group1_channels: channels,
                    group2_channels: 0,
                }
            }
        });
        AudioAttributes {
            format_index: index,
            present: true,
            audio_type_raw: 0,
            channel_format: ChannelFormat {
                group1_bits: bit_depth,
                group2_bits: bit_depth,
                group1_sample_rate: sample_rate,
                group2_sample_rate: sample_rate,
                assignment_code: 1,
                raw: [0, 0, 1],
            },
            channel_assignment,
            coding: AudioCoding::Unknown,
        }
    }

    fn title_set_with_audio_formats(audio_formats: Vec<AudioAttributes>) -> TitleSet {
        title_set_with_number_and_audio_formats(1, audio_formats)
    }

    fn title_set_with_number_and_audio_formats(
        number: u8,
        audio_formats: Vec<AudioAttributes>,
    ) -> TitleSet {
        TitleSet {
            number,
            source_file: "ATS_01_0.IFO".to_string(),
            kind: TitleSetKind::Audio,
            header: AtsiHeader {
                ats_last_sector: 0,
                atsi_last_sector: 0,
                specification_version: 0,
                category: 0,
                atsm_vobs: 0,
                atstt_vobs: 0,
                ats_ptt_srpt: 0,
                ats_pgcit: 0,
                ats_c_adt: 0,
                ats_vobu_admap: 0,
            },
            audio_pgcit_offset: 0,
            audio_formats,
            downmix_matrices: Vec::new(),
            aobs: Vec::new(),
            aobs_last_sector: None,
            titles: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn find_title_matches_declared_title_ref_semantics_not_either_identifier() {
        let mut title_set = title_set_with_number_and_audio_formats(1, Vec::new());
        title_set.titles = vec![
            AudioTitle {
                title_set_nr: 1,
                title_nr: 0x82,
                title_ordinal: 1,
                title_table_offset: 0,
                uniform_track_type_low_bits_candidate: None,
                track_type_low_bits_candidates: Vec::new(),
                track_count_declared: 0,
                index_count_declared: 0,
                len_in_pts: 0,
                chapters: Vec::new(),
            },
            AudioTitle {
                title_set_nr: 1,
                title_nr: 1,
                title_ordinal: 2,
                title_table_offset: 0,
                uniform_track_type_low_bits_candidate: None,
                track_type_low_bits_candidates: Vec::new(),
                track_count_declared: 0,
                index_count_declared: 0,
                len_in_pts: 0,
                chapters: Vec::new(),
            },
        ];

        let aott_ref = TitleRef {
            title_set_nr: 1,
            title_nr: 1,
            kind: TitleRefKind::AottTitleOrdinal,
        };
        let fallback_ref = TitleRef {
            title_set_nr: 1,
            title_nr: 1,
            kind: TitleRefKind::AtsPgcTitleNr,
        };

        let aott_title = find_title(&title_set, &aott_ref).unwrap();
        assert_eq!(aott_title.title_ordinal, 1);
        assert_eq!(aott_title.title_nr, 0x82);

        let fallback_title = find_title(&title_set, &fallback_ref).unwrap();
        assert_eq!(fallback_title.title_ordinal, 2);
        assert_eq!(fallback_title.title_nr, 1);
    }

    fn dummy_track(ordinal: u32) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: ordinal,
                disc_number: None,
                track_number: ordinal,
            },
            source_ref: TrackSourceRef::DvdaTrack {
                volume_source: DvdaVolumeSourceRef::Directory {
                    root: PathBuf::from("/tmp/dvda"),
                },
                group_nr: 1,
                title_set_nr: Some(1),
                title_nr: Some(0x81),
                title_ordinal: Some(1),
                group_track_ordinal: ordinal,
                ats_track_nr: Some(ordinal as u8),
                samg_track_nr: None,
                samg_ordinal: None,
                sector_address_space: DvdaSectorAddressSpace::AtsAobRelative { title_set_nr: 1 },
                elementary_stream_kind_hint: None,
                first_pts: 0,
                len_in_pts: 90_000,
                track_type: Some(0),
                index_start: Some(1),
                downmix_matrix: None,
                dvda_downmix_policy: DvdaDownmixPolicy::None,
                title_table_offset: Some(0),
                title_len_in_pts: Some(90_000),
                title_track_count_declared: Some(1),
                title_index_count_declared: Some(1),
                audio_format_index: None,
                expected_sample_rate: Some(48_000),
                expected_channel_count: Some(2),
                expected_bit_depth: Some(24),
                expected_channel_assignment_code: Some(1),
                expected_group1_sample_rate: Some(48_000),
                expected_group2_sample_rate: None,
                expected_group1_bit_depth: Some(24),
                expected_group2_bit_depth: None,
                expected_group1_channel_count: Some(2),
                expected_group2_channel_count: None,
                sector_ranges: vec![DvdaSectorRangeRef {
                    index_nr: 1,
                    first: 0,
                    last: 1,
                }],
                aob_files: vec![DvdaAobFileRef {
                    title_set_nr: 1,
                    part_nr: 1,
                    file_name: "ATS_01_1.AOB".to_string(),
                    exists: true,
                    byte_len: 4096,
                    block_first: 0,
                    block_last: 1,
                }],
            },
            metadata: TrackMetadata::default(),
            expected_samples: None,
            sample_rate: Some(48_000),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(48_000),
                Some(24),
                Some(SourceAudioCoding::DvdaUnknown),
            ),
            bit_depth: Some(24),
        }
    }

    fn chapter_with_sector_range(first: u32, last: u32) -> AudioChapter {
        AudioChapter {
            track_nr: 1,
            track_type: 0,
            track_type_low_bits_candidate: 0,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 1,
                first,
                last,
            }],
        }
    }

    fn aob_file_ref_for_test(exists: bool, block_first: u32, block_last: u32) -> DvdaAobFileRef {
        DvdaAobFileRef {
            title_set_nr: 1,
            part_nr: 1,
            file_name: "ATS_01_1.AOB".to_string(),
            exists,
            byte_len: if exists { 4096 } else { 0 },
            block_first,
            block_last,
        }
    }

    fn aob_entry_for_test(
        title_set_nr: u8,
        exists: bool,
        block_first: u32,
        block_last: u32,
    ) -> AobFileEntry {
        AobFileEntry {
            title_set_nr,
            part_nr: 1,
            file_name: format!("ATS_{title_set_nr:02}_1.AOB"),
            exists,
            byte_len: if exists { 4096 } else { 0 },
            block_first,
            block_last,
        }
    }

    fn audio_title_for_test(title_set_nr: u8, title_nr: u8, len_in_pts: u32) -> AudioTitle {
        let mut chapter = chapter_with_sector_range(10, 20);
        chapter.len_in_pts = len_in_pts;
        AudioTitle {
            title_set_nr,
            title_nr,
            title_ordinal: 1,
            title_table_offset: 0,
            uniform_track_type_low_bits_candidate: Some(1),
            track_type_low_bits_candidates: vec![1],
            track_count_declared: 1,
            index_count_declared: 1,
            len_in_pts,
            chapters: vec![chapter],
        }
    }

    fn samg_track_for_test() -> SamgTrack {
        SamgTrack {
            ordinal: 7,
            group_nr: 3,
            track_nr: 2,
            first_pts: 0,
            len_in_pts: 90_000,
            zone: SamgZone::Aob,
            flags: 0,
            channel_format: ChannelFormat {
                group1_bits: Some(24),
                group2_bits: Some(24),
                group1_sample_rate: Some(48_000),
                group2_sample_rate: Some(48_000),
                assignment_code: 1,
                raw: [0, 0, 1],
            },
            channel_assignment: None,
            abs_first_sector: 100,
            abs_first_sector_dup: 100,
            abs_last_sector: 199,
        }
    }

    fn samg_only_group_for_test() -> DvdaGroup {
        DvdaGroup {
            group_nr: 3,
            title_refs: Vec::new(),
            samg_tracks: vec![SamgTrackRef {
                samg_ordinal: 7,
                group_nr: 3,
                track_nr: 2,
            }],
            correlation: GroupCorrelation::SamgOnly,
        }
    }

    fn disc_with_samg_track_for_test(track: SamgTrack) -> DvdaDisc {
        DvdaDisc {
            amg: AmgInfo {
                source_file: "AUDIO_TS.IFO".to_string(),
                last_sector: 0,
                ifo_last_sector: 0,
                specification_version: 0,
                category: 0,
                nr_of_volumes: 1,
                this_volume_nr: 1,
                disc_side: 1,
                audio_title_sets: 0,
                video_title_sets: 0,
                provider_identifier: String::new(),
                position_code: 0,
                ifo_last_byte: 0,
                first_play_pgc: 0,
                pointers: AmgPointers::default(),
                audio_title_table: Vec::new(),
            },
            title_sets: Vec::new(),
            samg: Some(SamgInfo {
                source_file: "AUDIO_PP.IFO".to_string(),
                specification_version: 0,
                track_count_declared: 1,
                tracks: vec![track],
                raw_len: 0,
                expected_len: 0,
                copy_size: 0,
                copy_count: 0,
                repeated_copies_valid: false,
                copy_validations: Vec::<SamgCopyValidation>::new(),
                diagnostics: Vec::new(),
            }),
            groups: Vec::new(),
            copy_protection: CopyProtectionInfo {
                mkb_present: false,
                cppm_detected: false,
                source: CopyProtectionSource::NotDetected,
            },
            supplemental_video_ifo_present: false,
            diagnostics: Vec::new(),
        }
    }

    fn disc_with_group_profiles_for_test() -> DvdaDisc {
        let mut disc = disc_with_samg_track_for_test(samg_track_for_test());
        disc.samg = None;
        disc.title_sets = vec![
            title_set_with_number_and_audio_formats(
                1,
                vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(6))],
            ),
            title_set_with_number_and_audio_formats(
                2,
                vec![audio_attr_with_channels(
                    1,
                    Some(192_000),
                    Some(24),
                    Some(2),
                )],
            ),
            title_set_with_number_and_audio_formats(
                3,
                vec![audio_attr_with_channels(
                    1,
                    Some(176_400),
                    Some(24),
                    Some(6),
                )],
            ),
        ];
        disc.groups = vec![
            DvdaGroup {
                group_nr: 1,
                title_refs: vec![TitleRef {
                    title_set_nr: 1,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
            DvdaGroup {
                group_nr: 2,
                title_refs: vec![TitleRef {
                    title_set_nr: 2,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
            DvdaGroup {
                group_nr: 3,
                title_refs: vec![TitleRef {
                    title_set_nr: 3,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
        ];
        disc
    }

    #[test]
    fn group_selection_supports_default_group_exact_group_and_all_groups() {
        let disc = disc_with_group_profiles_for_test();

        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::Default).unwrap()[0].group_nr,
            1
        );
        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::Group(2)).unwrap()[0].group_nr,
            2
        );
        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::All)
                .unwrap()
                .iter()
                .map(|group| group.group_nr)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(matches!(
            select_groups(&disc, DvdaGroupSelection::Group(0)),
            Err(MaterializeError::InvalidTrackSelection(_))
        ));
    }

    #[test]
    fn group_selection_prefers_stereo_multichannel_and_highest_resolution() {
        let disc = disc_with_group_profiles_for_test();

        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::PreferStereo).unwrap()[0].group_nr,
            2
        );
        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::PreferMultichannel).unwrap()[0].group_nr,
            3,
            "multichannel preference should choose highest-channel groups, then rate/depth"
        );
        assert_eq!(
            select_groups(&disc, DvdaGroupSelection::PreferHighestResolution).unwrap()[0].group_nr,
            2,
            "highest-resolution preference should rank scalar rate/depth before channel count"
        );
    }

    #[test]
    fn automatic_downmix_policy_uses_disc_info_detector_signal() {
        assert_eq!(
            resolve_dvda_track_downmix_policy(DvdaDownmixPolicy::Auto, Some("5.1")),
            DvdaDownmixPolicy::FooInputDvdaCompatible,
            "auto must follow the same stereo-derived-from signal that disc-info renders"
        );
        assert_eq!(
            resolve_dvda_track_downmix_policy(DvdaDownmixPolicy::Auto, None),
            DvdaDownmixPolicy::None,
            "auto must preserve native extraction when disc-info did not identify a derived stereo presentation"
        );
        assert_eq!(
            resolve_dvda_track_downmix_policy(DvdaDownmixPolicy::None, Some("5.1")),
            DvdaDownmixPolicy::None,
            "an explicit native/raw override must not be replaced by the automatic detector"
        );
        assert_eq!(
            resolve_dvda_track_downmix_policy(DvdaDownmixPolicy::FfmpegDefault, Some("5.1")),
            DvdaDownmixPolicy::FfmpegDefault,
            "an explicit ffmpeg override must not be replaced by the automatic detector"
        );
    }

    #[test]
    fn authored_stereo_downmix_detection_allows_stereo_presentation_facts() {
        let mut carrier_title_set = title_set_with_number_and_audio_formats(
            1,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(6))],
        );
        carrier_title_set.aobs = vec![aob_entry_for_test(1, true, 10, 20)];
        carrier_title_set.titles = vec![audio_title_for_test(1, 1, 90_000)];

        let mut stereo_title_set = title_set_with_number_and_audio_formats(
            2,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(2))],
        );
        stereo_title_set.titles = vec![audio_title_for_test(2, 1, 90_000)];

        let mut disc = disc_with_samg_track_for_test(samg_track_for_test());
        disc.samg = None;
        disc.title_sets = vec![carrier_title_set, stereo_title_set];
        disc.groups = vec![
            DvdaGroup {
                group_nr: 1,
                title_refs: vec![TitleRef {
                    title_set_nr: 1,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
            DvdaGroup {
                group_nr: 2,
                title_refs: vec![TitleRef {
                    title_set_nr: 2,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
        ];

        let stereo_group = &disc.groups[1];
        let stereo_title_set = &disc.title_sets[1];
        assert_eq!(
            expected_channel_count_for_facts(audio_facts_for_title_set(stereo_title_set)),
            Some(2),
            "fixture must exercise the target case: presentation-facing facts say stereo"
        );
        assert_eq!(
            authored_stereo_downmix_source_label(&disc, stereo_group, stereo_title_set).as_deref(),
            Some("6ch"),
            "carrier evidence must come from the matching AOB-owning sibling, not from this group's IFO-facing channel count"
        );
        assert_eq!(
            resolve_dvda_track_downmix_policy(
                DvdaDownmixPolicy::Auto,
                authored_stereo_downmix_source_label(&disc, stereo_group, stereo_title_set)
                    .as_deref(),
            ),
            DvdaDownmixPolicy::FooInputDvdaCompatible
        );
    }

    #[test]
    fn authored_stereo_downmix_detection_treats_missing_aob_entries_as_aobless() {
        let mut carrier_title_set = title_set_with_number_and_audio_formats(
            1,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(6))],
        );
        carrier_title_set.aobs = vec![aob_entry_for_test(1, true, 10, 20)];
        carrier_title_set.titles = vec![audio_title_for_test(1, 1, 90_000)];

        let mut stereo_title_set = title_set_with_number_and_audio_formats(
            2,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(2))],
        );
        stereo_title_set.aobs = vec![aob_entry_for_test(2, false, 10, 20)];
        stereo_title_set.titles = vec![audio_title_for_test(2, 1, 90_000)];

        let mut disc = disc_with_samg_track_for_test(samg_track_for_test());
        disc.samg = None;
        disc.title_sets = vec![carrier_title_set, stereo_title_set];
        disc.groups = vec![
            DvdaGroup {
                group_nr: 1,
                title_refs: vec![TitleRef {
                    title_set_nr: 1,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
            DvdaGroup {
                group_nr: 2,
                title_refs: vec![TitleRef {
                    title_set_nr: 2,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
        ];

        let stereo_group = &disc.groups[1];
        let stereo_title_set = &disc.title_sets[1];
        assert!(!title_set_has_existing_aobs(stereo_title_set));
        assert_eq!(existing_aob_file_refs(stereo_title_set), Vec::new());
        assert_eq!(
            authored_stereo_downmix_source_label(&disc, stereo_group, stereo_title_set).as_deref(),
            Some("6ch"),
            "AOB entries with exists=false must behave like an AOB-less title set for authored-stereo detection"
        );
    }

    #[test]
    fn authored_stereo_downmix_detection_rejects_stereo_carrier_sibling() {
        let mut carrier_title_set = title_set_with_number_and_audio_formats(
            1,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(2))],
        );
        carrier_title_set.aobs = vec![aob_entry_for_test(1, true, 10, 20)];
        carrier_title_set.titles = vec![audio_title_for_test(1, 1, 90_000)];

        let mut stereo_title_set = title_set_with_number_and_audio_formats(
            2,
            vec![audio_attr_with_channels(1, Some(96_000), Some(24), Some(2))],
        );
        stereo_title_set.titles = vec![audio_title_for_test(2, 1, 90_000)];

        let mut disc = disc_with_samg_track_for_test(samg_track_for_test());
        disc.samg = None;
        disc.title_sets = vec![carrier_title_set, stereo_title_set];
        disc.groups = vec![
            DvdaGroup {
                group_nr: 1,
                title_refs: vec![TitleRef {
                    title_set_nr: 1,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
            DvdaGroup {
                group_nr: 2,
                title_refs: vec![TitleRef {
                    title_set_nr: 2,
                    title_nr: 1,
                    kind: TitleRefKind::AottTitleOrdinal,
                }],
                samg_tracks: Vec::new(),
                correlation: GroupCorrelation::FromAmgAott,
            },
        ];

        assert_eq!(
            authored_stereo_downmix_source_label(&disc, &disc.groups[1], &disc.title_sets[1]),
            None,
            "an AOB-less stereo presentation is not enough; the payload-owning sibling must be multichannel"
        );
    }

    #[test]
    fn legacy_dvda_group_field_maps_to_exact_group_selection() {
        let options = SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_group: Some(4),
            dvda_assume_decrypted: false,
            dvda_downmix_policy: DvdaDownmixPolicy::Auto,
            cue_sidecar: CueSidecarPolicy::PreferSidecar,
            track_selection: TrackSelection::All,
        };
        assert_eq!(
            options.effective_dvda_group_selection(),
            DvdaGroupSelection::Group(4)
        );
        assert!(options.explicit_dvda_requested());
    }

    #[test]
    fn magic_search_detects_amg_identifier_across_arbitrary_bytes() {
        let mut bytes = vec![0_u8; 31];
        bytes.extend_from_slice(DVDA_AMG_MAGIC);
        bytes.extend_from_slice(&[1, 2, 3]);
        assert!(contains_subslice(&bytes, DVDA_AMG_MAGIC));
        assert!(!contains_subslice(b"DVDAUDIO-VMG", DVDA_AMG_MAGIC));
    }

    #[test]
    fn iso9660_bridge_detector_validates_audio_ts_ifo_path() {
        let path = temp_test_path("dvda_iso9660_bridge.iso");
        let bytes = minimal_iso9660_dvda_image(true);
        fs::write(&path, bytes).expect("write ISO fixture");

        assert!(iso9660_bridge_has_dvda_magic(&path).expect("ISO9660 detection"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn iso9660_bridge_detector_rejects_stray_magic_without_path() {
        let path = temp_test_path("dvda_iso9660_stray_magic.iso");
        let mut bytes = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];
        bytes[DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()]
            .copy_from_slice(DVDA_AMG_MAGIC);
        fs::write(&path, bytes).expect("write ISO fixture");

        assert!(!iso9660_bridge_has_dvda_magic(&path).expect("ISO9660 detection"));
        assert!(raw_iso_scan_has_dvda_magic(&path).expect("raw scan"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn auto_detection_rejects_stray_raw_magic_without_audio_ts_path() {
        let path = temp_test_path("dvda_auto_rejects_stray_magic.iso");
        let mut bytes = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];
        bytes[DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()]
            .copy_from_slice(DVDA_AMG_MAGIC);
        fs::write(&path, bytes).expect("write ISO fixture");

        let req = dvda_detection_request(path.clone(), None);
        assert!(!is_dvda_candidate(&req).expect("DVD-Audio auto-detection"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn explicit_dvda_request_routes_raw_magic_to_dvd_audio_diagnostic() {
        let path = temp_test_path("dvda_explicit_raw_fallback.iso");
        let mut bytes = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];
        bytes[DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()]
            .copy_from_slice(DVDA_AMG_MAGIC);
        fs::write(&path, bytes).expect("write ISO fixture");

        let req = dvda_detection_request(path.clone(), Some(1));
        assert_eq!(
            detect_dvda_source(&req).expect("explicit DVD-Audio detection"),
            DvdaDetection::ExplicitRawMagicFallback
        );
        assert!(is_dvda_candidate(&req).expect("explicit DVD-Audio candidate"));
        assert_eq!(
            super::super::stages::detect_source_kind(&req).expect("explicit DVD-Audio source kind"),
            SourceKind::DvdAudio
        );
        match open_dvda_volume_with_detection(&path, DvdaDetection::ExplicitRawMagicFallback) {
            Err(MaterializeError::Parse(message)) => {
                assert!(message.contains("explicit DVD-Audio raw magic"));
                assert!(message.contains("no AUDIO_TS filesystem path"));
            }
            other => panic!("expected DVD-Audio-specific parse error, got {other:?}"),
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn iso9660_bridge_path_routes_without_raw_fallback_or_toolrunner_extraction() {
        let path = temp_test_path("dvda_iso9660_bridge_routes.iso");
        let bytes = minimal_iso9660_dvda_image(true);
        fs::write(&path, bytes).expect("write ISO fixture");

        let req = dvda_detection_request(path.clone(), None);
        assert_eq!(
            detect_dvda_source(&req).expect("DVD-Audio detection"),
            DvdaDetection::Iso9660BridgePath
        );
        assert!(is_dvda_candidate(&req).expect("DVD-Audio candidate detection"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn iso9660_detection_path_opens_iso9660_materialization_backend() {
        let path = temp_test_path("dvda_iso9660_backend_routes.iso");
        fs::write(&path, minimal_iso9660_dvda_image(true)).expect("write ISO fixture");

        let volume = open_dvda_volume_with_detection(&path, DvdaDetection::Iso9660BridgePath)
            .expect("ISO9660 materialization backend");
        assert_eq!(
            volume.source_ref(),
            &DvdaVolumeSourceRef::Iso {
                path: path.clone(),
                backend: DvdaIsoBackend::Iso9660Bridge,
            }
        );
        assert!(
            file_in_volume_starts_with_magic(&volume, "AUDIO_TS.IFO", DVDA_AMG_MAGIC)
                .expect("IFO magic through materialization backend")
        );

        let _ = fs::remove_file(path);
    }

    fn temp_test_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("tonepoet_{nonce}_{name}"))
    }

    fn dvda_detection_request(path: PathBuf, explicit_group: Option<u8>) -> PipelineRequest {
        let root = std::env::temp_dir().join("tonepoet-dvda-detect-tests");
        let group_selection = explicit_group
            .map(DvdaGroupSelection::Group)
            .unwrap_or(DvdaGroupSelection::Default);
        PipelineRequest {
            job_id: "dvda-detect-test".to_string(),
            item_id: "dvda-detect-test".to_string(),
            container: path,
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group_selection: group_selection,
                dvda_group: None,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: Some(1),
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: true,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Disabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn minimal_iso9660_dvda_image(include_path: bool) -> Vec<u8> {
        let mut image = vec![0_u8; DVD_SECTOR_SIZE_USIZE * 24];

        let root_record = iso9660_test_record(&[0], 18, DVD_SECTOR_SIZE_U64 as u32, 0x02);
        let pvd = &mut image[DVD_SECTOR_SIZE_USIZE * 16..DVD_SECTOR_SIZE_USIZE * 17];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[156..156 + root_record.len()].copy_from_slice(&root_record);

        let vdst = &mut image[DVD_SECTOR_SIZE_USIZE * 17..DVD_SECTOR_SIZE_USIZE * 18];
        vdst[0] = 255;
        vdst[1..6].copy_from_slice(b"CD001");
        vdst[6] = 1;

        if include_path {
            let root_dir = &mut image[DVD_SECTOR_SIZE_USIZE * 18..DVD_SECTOR_SIZE_USIZE * 19];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 18, DVD_SECTOR_SIZE_U64 as u32, 0x02),
                iso9660_test_record(&[1], 18, DVD_SECTOR_SIZE_U64 as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS", 19, DVD_SECTOR_SIZE_U64 as u32, 0x02),
            ] {
                root_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }

            let audio_dir = &mut image[DVD_SECTOR_SIZE_USIZE * 19..DVD_SECTOR_SIZE_USIZE * 20];
            let mut offset = 0usize;
            for record in [
                iso9660_test_record(&[0], 19, DVD_SECTOR_SIZE_U64 as u32, 0x02),
                iso9660_test_record(&[1], 18, DVD_SECTOR_SIZE_U64 as u32, 0x02),
                iso9660_test_record(b"AUDIO_TS.IFO;1", 20, DVDA_AMG_MAGIC.len() as u32, 0x00),
            ] {
                audio_dir[offset..offset + record.len()].copy_from_slice(&record);
                offset += record.len();
            }
        }

        let ifo = &mut image
            [DVD_SECTOR_SIZE_USIZE * 20..DVD_SECTOR_SIZE_USIZE * 20 + DVDA_AMG_MAGIC.len()];
        ifo.copy_from_slice(DVDA_AMG_MAGIC);
        image
    }

    fn iso9660_test_record(name: &[u8], extent_lba: u32, data_len: u32, file_flags: u8) -> Vec<u8> {
        let len_without_padding = 33 + name.len();
        let record_len = len_without_padding + (len_without_padding % 2);
        let mut record = vec![0_u8; record_len];
        record[0] = record_len as u8;
        record[2..6].copy_from_slice(&extent_lba.to_le_bytes());
        record[6..10].copy_from_slice(&extent_lba.to_be_bytes());
        record[10..14].copy_from_slice(&data_len.to_le_bytes());
        record[14..18].copy_from_slice(&data_len.to_be_bytes());
        record[25] = file_flags;
        record[28..30].copy_from_slice(&1_u16.to_le_bytes());
        record[30..32].copy_from_slice(&1_u16.to_be_bytes());
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name);
        record
    }

    #[test]
    fn track_selection_filters_by_source_ordinal() {
        let tracks = vec![
            dummy_track(1),
            dummy_track(2),
            dummy_track(3),
            dummy_track(4),
        ];
        let selected = apply_track_selection(tracks, &TrackSelection::Range { start: 2, end: 3 })
            .expect("valid range");
        assert_eq!(
            selected
                .iter()
                .map(|track| track.id.source_ordinal)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn track_selection_rejects_out_of_range_set_member() {
        let mut set = BTreeSet::new();
        set.insert(1);
        set.insert(4);
        let err = apply_track_selection(
            vec![dummy_track(1), dummy_track(2)],
            &TrackSelection::Set(set),
        )
        .expect_err("selection should reject out-of-range ordinal");
        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
    }

    #[test]
    fn sector_validation_accepts_ifo_structure_without_aob_payloads() {
        let chapter = chapter_with_sector_range(10, 20);

        validate_sector_ranges_are_well_formed(&chapter, 1, 1)
            .expect("Phase 2 structure materialization should not require AOB payloads");
        assert!(!sector_ranges_are_covered(&chapter, &[]));
    }

    #[test]
    fn sector_validation_rejects_malformed_ranges() {
        let chapter = chapter_with_sector_range(20, 10);
        let err = validate_sector_ranges_are_well_formed(&chapter, 1, 1)
            .expect_err("inverted range should remain a structural parse error");
        assert!(matches!(err, MaterializeError::Parse(_)));
    }

    #[test]
    fn aob_coverage_ignores_inventory_entries_for_missing_payloads() {
        let chapter = chapter_with_sector_range(10, 20);
        let missing_aob = aob_file_ref_for_test(false, 10, 20);
        let present_aob = aob_file_ref_for_test(true, 10, 20);

        assert!(!sector_ranges_are_covered(&chapter, &[missing_aob]));
        assert!(sector_ranges_are_covered(&chapter, &[present_aob]));
    }

    #[test]
    fn atsi_track_source_ref_carries_typed_decode_boundary_fields() {
        let title_set = title_set_with_audio_formats(vec![audio_attr_with_channels(
            2,
            Some(96_000),
            Some(24),
            Some(2),
        )]);
        let title = AudioTitle {
            title_set_nr: 1,
            title_nr: 0x83,
            title_ordinal: 3,
            title_table_offset: 0x120,
            uniform_track_type_low_bits_candidate: Some(5),
            track_type_low_bits_candidates: vec![5],
            track_count_declared: 4,
            index_count_declared: 9,
            len_in_pts: 360_000,
            chapters: Vec::new(),
        };
        let chapter = AudioChapter {
            track_nr: 2,
            track_type: 0xa5,
            track_type_low_bits_candidate: 5,
            downmix_matrix: Some(3),
            index_start: 7,
            first_pts: 12_345,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 7,
                first: 100,
                last: 200,
            }],
        };
        let group = DvdaGroup {
            group_nr: 1,
            title_refs: vec![TitleRef {
                title_set_nr: 1,
                title_nr: 3,
                kind: TitleRefKind::AottTitleOrdinal,
            }],
            samg_tracks: vec![SamgTrackRef {
                samg_ordinal: 12,
                group_nr: 1,
                track_nr: 2,
            }],
            correlation: GroupCorrelation::MixedAmgAndSamg,
        };
        let aob_files = vec![aob_file_ref_for_test(true, 100, 200)];

        let volume_source = DvdaVolumeSourceRef::Iso {
            path: PathBuf::from("/tmp/dvda.iso"),
            backend: DvdaIsoBackend::Udf,
        };
        let source_ref = ats_track_source_ref(
            &volume_source,
            &group,
            &title_set,
            &title,
            &chapter,
            2,
            audio_facts_for_title_set(&title_set),
            DvdaSectorAddressSpace::AtsAobRelative { title_set_nr: 1 },
            chapter.sector_ranges.iter().map(sector_range_ref).collect(),
            DvdaDownmixPolicy::None,
            None,
            None,
            aob_files,
        );

        match source_ref {
            TrackSourceRef::DvdaTrack {
                volume_source,
                first_pts,
                len_in_pts,
                track_type,
                index_start,
                downmix_matrix,
                title_table_offset,
                title_len_in_pts,
                title_track_count_declared,
                title_index_count_declared,
                group_track_ordinal,
                ats_track_nr,
                samg_track_nr,
                audio_format_index,
                expected_sample_rate,
                expected_channel_count,
                expected_bit_depth,
                expected_channel_assignment_code,
                expected_group1_sample_rate,
                expected_group2_sample_rate,
                expected_group1_bit_depth,
                expected_group2_bit_depth,
                expected_group1_channel_count,
                expected_group2_channel_count,
                samg_ordinal,
                ..
            } => {
                assert_eq!(
                    volume_source,
                    DvdaVolumeSourceRef::Iso {
                        path: PathBuf::from("/tmp/dvda.iso"),
                        backend: DvdaIsoBackend::Udf,
                    }
                );
                assert_eq!(first_pts, 12_345);
                assert_eq!(len_in_pts, 90_000);
                assert_eq!(track_type, Some(0xa5));
                assert_eq!(expected_sample_rate, Some(96_000));
                assert_eq!(expected_channel_count, Some(2));
                assert_eq!(expected_bit_depth, Some(24));
                assert_eq!(expected_channel_assignment_code, Some(1));
                assert_eq!(expected_group1_sample_rate, Some(96_000));
                assert_eq!(expected_group1_bit_depth, Some(24));
                assert_eq!(expected_group1_channel_count, Some(2));
                assert_eq!(index_start, Some(7));
                assert_eq!(downmix_matrix, Some(3));
                assert_eq!(title_table_offset, Some(0x120));
                assert_eq!(title_len_in_pts, Some(360_000));
                assert_eq!(title_track_count_declared, Some(4));
                assert_eq!(title_index_count_declared, Some(9));
                assert_eq!(group_track_ordinal, 2);
                assert_eq!(ats_track_nr, Some(2));
                assert_eq!(samg_track_nr, Some(2));
                assert_eq!(audio_format_index, Some(2));
                assert_eq!(samg_ordinal, Some(12));
            }
            _ => panic!("expected DVD-Audio source ref"),
        }
    }

    #[test]
    fn samg_correlation_uses_group_track_ordinal_not_ats_chapter_number() {
        let title_set = title_set_with_audio_formats(vec![audio_attr(0, Some(96_000), Some(24))]);
        let title = AudioTitle {
            title_set_nr: 1,
            title_nr: 0x84,
            title_ordinal: 4,
            title_table_offset: 0x240,
            uniform_track_type_low_bits_candidate: None,
            track_type_low_bits_candidates: Vec::new(),
            track_count_declared: 1,
            index_count_declared: 1,
            len_in_pts: 90_000,
            chapters: Vec::new(),
        };
        let chapter = AudioChapter {
            track_nr: 1,
            track_type: 0,
            track_type_low_bits_candidate: 0,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 1,
                first: 10,
                last: 20,
            }],
        };
        let group = DvdaGroup {
            group_nr: 1,
            title_refs: vec![TitleRef {
                title_set_nr: 1,
                title_nr: 4,
                kind: TitleRefKind::AottTitleOrdinal,
            }],
            samg_tracks: vec![SamgTrackRef {
                samg_ordinal: 30,
                group_nr: 1,
                track_nr: 3,
            }],
            correlation: GroupCorrelation::MixedAmgAndSamg,
        };
        let source_ref = ats_track_source_ref(
            &DvdaVolumeSourceRef::Directory {
                root: PathBuf::from("/tmp/dvda"),
            },
            &group,
            &title_set,
            &title,
            &chapter,
            3,
            audio_facts_for_title_set(&title_set),
            DvdaSectorAddressSpace::AtsAobRelative { title_set_nr: 1 },
            chapter.sector_ranges.iter().map(sector_range_ref).collect(),
            DvdaDownmixPolicy::None,
            None,
            None,
            vec![aob_file_ref_for_test(true, 10, 20)],
        );

        match source_ref {
            TrackSourceRef::DvdaTrack {
                group_track_ordinal,
                ats_track_nr,
                samg_track_nr,
                samg_ordinal,
                ..
            } => {
                assert_eq!(group_track_ordinal, 3);
                assert_eq!(ats_track_nr, Some(1));
                assert_eq!(samg_track_nr, Some(3));
                assert_eq!(samg_ordinal, Some(30));
            }
            _ => panic!("expected DVD-Audio source ref"),
        }
    }

    #[test]
    fn samg_sector_correlation_derives_base_and_lpcm_hint_for_vob_tracks() {
        let mut title = audio_title_for_test(2, 1, 180_000);
        title.chapters = vec![
            chapter_with_sector_range(0, 45_280),
            chapter_with_sector_range(45_281, 119_152),
        ];
        title.chapters[1].track_nr = 2;

        let mut track1 = samg_track_for_test();
        track1.ordinal = 1;
        track1.group_nr = 2;
        track1.track_nr = 1;
        track1.zone = SamgZone::Vob;
        track1.channel_assignment = Some(ChannelAssignment {
            code: 1,
            group1: STEREO_CHANNELS,
            group2: NO_CHANNELS,
            group1_channels: 2,
            group2_channels: 0,
        });
        track1.abs_first_sector = 1_703_445;
        track1.abs_first_sector_dup = 1_703_445;
        track1.abs_last_sector = 1_748_725;

        let mut track2 = track1.clone();
        track2.ordinal = 2;
        track2.track_nr = 2;
        track2.abs_first_sector = 1_748_726;
        track2.abs_first_sector_dup = 1_748_726;
        track2.abs_last_sector = 1_822_597;

        let mut disc = disc_with_samg_track_for_test(track1);
        disc.samg
            .as_mut()
            .expect("SAMG fixture")
            .tracks
            .push(track2);

        let correlation = find_samg_sector_correlation(&disc, &title)
            .expect("SAMG VOB sectors should correlate with ATS chapter sectors");
        assert_eq!(correlation.disc_absolute_base, 1_703_445);
        assert_eq!(
            correlation.elementary_stream_kind_hint(),
            Some(DvdaElementaryStreamKind::DvdVideoLpcm)
        );
        assert_eq!(
            correlation
                .track_for_chapter_index(0)
                .expect("track 1")
                .track_nr,
            1
        );
        assert_eq!(
            correlation
                .track_for_chapter_index(1)
                .expect("track 2")
                .track_nr,
            2
        );
    }

    #[test]
    fn single_format_ats_reports_structural_audio_facts() {
        let title_set = title_set_with_audio_formats(vec![audio_attr(2, Some(96_000), Some(24))]);
        let facts = audio_facts_for_title_set(&title_set);
        assert_eq!(facts.format_index, Some(2));
        assert_eq!(facts.sample_rate, Some(96_000));
        assert_eq!(facts.bit_depth, Some(24));
        assert_eq!(facts.resolution, AudioFormatResolution::SinglePresentFormat);
        assert!(facts.attr.is_some());
    }

    #[test]
    fn multi_format_ats_keeps_audio_facts_unknown_until_aob_demux() {
        let title_set = title_set_with_audio_formats(vec![
            audio_attr(0, Some(48_000), Some(24)),
            audio_attr(2, Some(192_000), Some(24)),
        ]);
        let facts = audio_facts_for_title_set(&title_set);
        assert_eq!(facts.format_index, None);
        assert_eq!(facts.sample_rate, None);
        assert_eq!(facts.bit_depth, None);
        assert_eq!(
            facts.resolution,
            AudioFormatResolution::MultiplePresentFormats
        );
        assert!(facts.attr.is_none());
    }

    #[test]
    fn alternate_presentation_track_type_keeps_single_format_audio_facts_unknown() {
        let title_set = title_set_with_audio_formats(vec![audio_attr(0, Some(96_000), Some(24))]);
        let chapter = AudioChapter {
            track_nr: 3,
            track_type: 0x08,
            track_type_low_bits_candidate: 0,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 1,
                first: 10,
                last: 20,
            }],
        };

        let facts = audio_facts_for_title_chapter(&title_set, &chapter);

        assert_eq!(facts.format_index, None);
        assert_eq!(facts.sample_rate, None);
        assert_eq!(facts.bit_depth, None);
        assert_eq!(
            facts.resolution,
            AudioFormatResolution::MultiplePresentFormats
        );
        assert!(facts.attr.is_none());
    }

    #[test]
    fn alternate_presentation_track_type_does_not_match_primary_format_index() {
        let title_set = title_set_with_audio_formats(vec![
            audio_attr(0, Some(96_000), Some(24)),
            audio_attr(2, Some(192_000), Some(24)),
        ]);
        let chapter = AudioChapter {
            track_nr: 2,
            track_type: 0x08,
            track_type_low_bits_candidate: 0,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 1,
                first: 10,
                last: 20,
            }],
        };

        let facts = audio_facts_for_title_chapter(&title_set, &chapter);

        assert_eq!(facts.format_index, None);
        assert_eq!(facts.sample_rate, None);
        assert_eq!(facts.bit_depth, None);
        assert_eq!(
            facts.resolution,
            AudioFormatResolution::MultiplePresentFormats
        );
        assert!(facts.attr.is_none());
    }

    #[test]
    fn multi_format_ats_resolves_active_format_from_track_type_candidate() {
        let title_set = title_set_with_audio_formats(vec![
            audio_attr(0, Some(48_000), Some(20)),
            audio_attr(2, Some(192_000), Some(24)),
        ]);
        let chapter = AudioChapter {
            track_nr: 1,
            track_type: 0xa2,
            track_type_low_bits_candidate: 2,
            downmix_matrix: None,
            index_start: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            sector_ranges: vec![crate::tui::dvda::SectorRange {
                index_nr: 1,
                first: 10,
                last: 20,
            }],
        };

        let facts = audio_facts_for_title_chapter(&title_set, &chapter);

        assert_eq!(facts.format_index, Some(2));
        assert_eq!(facts.sample_rate, Some(192_000));
        assert_eq!(facts.bit_depth, Some(24));
        assert_eq!(
            facts.resolution,
            AudioFormatResolution::TrackTypeAudioFormatIndex
        );
    }

    #[test]
    fn mixed_group_rates_do_not_collapse_to_one_reported_rate() {
        let title_set = title_set_with_audio_formats(vec![AudioAttributes {
            format_index: 0,
            present: true,
            audio_type_raw: 0,
            channel_format: ChannelFormat {
                group1_bits: Some(24),
                group2_bits: Some(24),
                group1_sample_rate: Some(96_000),
                group2_sample_rate: Some(48_000),
                assignment_code: 12,
                raw: [0, 0, 12],
            },
            channel_assignment: None,
            coding: AudioCoding::Unknown,
        }]);
        let facts = audio_facts_for_title_set(&title_set);
        assert_eq!(facts.format_index, Some(0));
        assert_eq!(facts.sample_rate, None);
        assert_eq!(facts.bit_depth, Some(24));
        assert_eq!(facts.resolution, AudioFormatResolution::SinglePresentFormat);
    }

    #[test]
    fn samg_only_track_materializes_without_ats_title_reference() {
        let samg_track = samg_track_for_test();
        let disc = disc_with_samg_track_for_test(samg_track.clone());
        let group = samg_only_group_for_test();

        let volume_source = DvdaVolumeSourceRef::Directory {
            root: PathBuf::from("/tmp/dvda"),
        };
        let prepared = prepared_track_from_samg_track(
            &volume_source,
            &disc,
            &group,
            &samg_track,
            1,
            2,
            DvdaDownmixPolicy::None,
        )
        .expect("SAMG-only groups should produce a structure-only PreparedTrack");

        assert_eq!(prepared.scalar_sample_rate(), Some(48_000));
        assert_eq!(prepared.bit_depth, Some(24));
        assert_eq!(prepared.expected_samples, Some(48_000));
        assert_eq!(
            prepared
                .metadata
                .extra
                .get("dvda_origin")
                .map(String::as_str),
            Some("samg")
        );
        assert_eq!(
            prepared
                .metadata
                .extra
                .get("dvda_sector_address_space")
                .map(String::as_str),
            Some("samg_absolute")
        );

        match prepared.source_ref {
            TrackSourceRef::DvdaTrack {
                volume_source,
                title_set_nr,
                title_nr,
                title_ordinal,
                group_track_ordinal,
                ats_track_nr,
                samg_track_nr,
                samg_ordinal,
                sector_address_space,
                first_pts,
                len_in_pts,
                track_type,
                index_start,
                downmix_matrix,
                title_table_offset,
                title_len_in_pts,
                title_track_count_declared,
                title_index_count_declared,
                expected_sample_rate,
                expected_channel_count,
                expected_bit_depth,
                expected_channel_assignment_code,
                expected_group1_sample_rate,
                expected_group2_sample_rate,
                expected_group1_bit_depth,
                expected_group2_bit_depth,
                expected_group1_channel_count,
                expected_group2_channel_count,
                sector_ranges,
                aob_files,
                ..
            } => {
                assert_eq!(
                    volume_source,
                    DvdaVolumeSourceRef::Directory {
                        root: PathBuf::from("/tmp/dvda"),
                    }
                );
                assert_eq!(title_set_nr, None);
                assert_eq!(title_nr, None);
                assert_eq!(title_ordinal, None);
                assert_eq!(group_track_ordinal, 2);
                assert_eq!(ats_track_nr, None);
                assert_eq!(samg_track_nr, Some(2));
                assert_eq!(samg_ordinal, Some(7));
                assert_eq!(first_pts, 0);
                assert_eq!(len_in_pts, 90_000);
                assert_eq!(track_type, None);
                assert_eq!(index_start, None);
                assert_eq!(downmix_matrix, None);
                assert_eq!(title_table_offset, None);
                assert_eq!(title_len_in_pts, None);
                assert_eq!(title_track_count_declared, None);
                assert_eq!(title_index_count_declared, None);
                assert_eq!(expected_sample_rate, Some(48_000));
                assert_eq!(expected_channel_count, None);
                assert_eq!(expected_bit_depth, Some(24));
                assert_eq!(expected_channel_assignment_code, Some(12));
                assert_eq!(expected_group1_sample_rate, Some(48_000));
                assert_eq!(expected_group1_bit_depth, Some(24));
                assert_eq!(expected_group1_channel_count, None);
                assert_eq!(sector_address_space, DvdaSectorAddressSpace::SamgAbsolute);
                assert_eq!(sector_ranges.len(), 1);
                assert_eq!(sector_ranges[0].first, 100);
                assert_eq!(sector_ranges[0].last, 199);
                assert!(aob_files.is_empty());
            }
            _ => panic!("expected DVD-Audio source ref"),
        }
    }

    #[test]
    fn samg_audio_facts_come_from_samg_channel_format() {
        let samg_track = samg_track_for_test();
        let facts = audio_facts_for_samg_track(&samg_track);

        assert_eq!(facts.format_index, None);
        assert_eq!(facts.sample_rate, Some(48_000));
        assert_eq!(facts.bit_depth, Some(24));
        assert_eq!(facts.resolution, AudioFormatResolution::SamgTrackRecord);
    }

    #[test]
    fn mixed_group_rates_are_carried_as_channel_groups_not_scalar_sentinel() {
        let title_set = title_set_with_audio_formats(vec![AudioAttributes {
            format_index: 0,
            present: true,
            audio_type_raw: 0,
            channel_format: ChannelFormat {
                group1_bits: Some(24),
                group2_bits: Some(16),
                group1_sample_rate: Some(96_000),
                group2_sample_rate: Some(48_000),
                assignment_code: 12,
                raw: [0, 0, 12],
            },
            channel_assignment: None,
            coding: AudioCoding::Unknown,
        }]);
        let facts = audio_facts_for_title_set(&title_set);
        let descriptor = source_audio_descriptor_for_facts(facts);

        assert_eq!(facts.sample_rate, None);
        assert_eq!(descriptor.primary_sample_rate, None);
        assert_eq!(descriptor.bit_depth, None);
        assert_eq!(descriptor.coding, Some(SourceAudioCoding::DvdaUnknown));
        assert_eq!(descriptor.channel_groups.len(), 2);
        assert_eq!(descriptor.channel_groups[0].sample_rate, Some(96_000));
        assert_eq!(descriptor.channel_groups[0].bit_depth, Some(24));
        assert_eq!(descriptor.channel_groups[1].sample_rate, Some(48_000));
        assert_eq!(descriptor.channel_groups[1].bit_depth, Some(16));
    }

    #[test]
    fn multi_format_ats_has_no_scalar_or_channel_group_audio_descriptor() {
        let title_set = title_set_with_audio_formats(vec![
            audio_attr(0, Some(48_000), Some(24)),
            audio_attr(2, Some(192_000), Some(24)),
        ]);
        let facts = audio_facts_for_title_set(&title_set);
        let descriptor = source_audio_descriptor_for_facts(facts);

        assert_eq!(facts.sample_rate, None);
        assert_eq!(descriptor.primary_sample_rate, None);
        assert_eq!(descriptor.bit_depth, None);
        assert!(descriptor.channel_groups.is_empty());
    }

    fn prepared_track_with_extra_album(source_ordinal: u32, album: Option<&str>) -> PreparedTrack {
        let mut extra = BTreeMap::new();
        extra.insert("dvda_track_id".to_string(), format!("1.1.{source_ordinal}"));
        if let Some(album) = album {
            extra.insert("dvda_metabase_album".to_string(), album.to_string());
        }

        PreparedTrack {
            id: TrackId {
                source_ordinal,
                disc_number: None,
                track_number: source_ordinal,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!(
                "track-{source_ordinal}.wav"
            ))),
            metadata: TrackMetadata {
                extra,
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: None,
            source_audio: SourceAudioDescriptor::default(),
            bit_depth: None,
        }
    }

    #[test]
    fn selected_track_album_overlay_scopes_metabase_album_to_selected_ids() {
        let mut selected_meta = BTreeMap::new();
        selected_meta.insert(
            "ALBUM".to_string(),
            "Brothers in Arms (DVD-A) [Multichannel ISO]".to_string(),
        );
        selected_meta.insert("ARTIST".to_string(), "Dire Straits".to_string());

        let mut sibling_meta = BTreeMap::new();
        sibling_meta.insert("ALBUM".to_string(), "Wrong sibling group".to_string());
        sibling_meta.insert("ARTIST".to_string(), "Other Artist".to_string());

        let metabase = DvdaMetabase {
            store_id: "0123456789ABCDEF0123456789ABCDEF".to_string(),
            tracks: vec![
                crate::tui::dvda_metabase::DvdaMetabaseTrack {
                    id: "1.1.1".to_string(),
                    meta: selected_meta.clone(),
                },
                crate::tui::dvda_metabase::DvdaMetabaseTrack {
                    id: "1.1.2".to_string(),
                    meta: selected_meta,
                },
                crate::tui::dvda_metabase::DvdaMetabaseTrack {
                    id: "9.9.1".to_string(),
                    meta: sibling_meta,
                },
            ],
        };
        let tracks = vec![
            prepared_track_with_extra_album(1, None),
            prepared_track_with_extra_album(2, None),
        ];
        let mut album = AlbumMetadata::default();

        overlay_selected_track_album_values(&mut album, Some(&metabase), &tracks);

        assert_eq!(
            album.album.as_deref(),
            Some("Brothers in Arms (DVD-A) [Multichannel ISO]")
        );
        assert_eq!(album.album_artist.as_deref(), Some("Dire Straits"));
    }

    #[test]
    fn common_selected_track_extra_value_uses_selected_group_album_when_consistent() {
        let tracks = vec![
            prepared_track_with_extra_album(1, Some("Brothers in Arms (DVD-A) [Multichannel ISO]")),
            prepared_track_with_extra_album(2, Some("Brothers in Arms (DVD-A) [Multichannel ISO]")),
            prepared_track_with_extra_album(3, None),
        ];

        assert_eq!(
            common_selected_track_extra_value(&tracks, "dvda_metabase_album").as_deref(),
            Some("Brothers in Arms (DVD-A) [Multichannel ISO]")
        );
    }

    #[test]
    fn common_selected_track_extra_value_refuses_conflicting_album_values() {
        let tracks = vec![
            prepared_track_with_extra_album(1, Some("Album A")),
            prepared_track_with_extra_album(2, Some("Album B")),
        ];

        assert_eq!(
            common_selected_track_extra_value(&tracks, "dvda_metabase_album"),
            None
        );
    }
}

#[cfg(test)]
#[path = "materializer_dvda_fixture_tests.rs"]
mod fixture_corpus_tests;

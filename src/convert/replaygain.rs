//! Native production ReplayGain/loudness integration.
//!
//! Phase 4 deliberately keeps four boundaries separate:
//! * PCM reading/completion is application-owned;
//! * loudness and reporting-peak arithmetic are owned by `tonepoet-true-peak`;
//! * ReplayGain projection is pure policy;
//! * serialization is owned by Tonepoet's format-specific metadata writer.
//!
//! `LoudnessProfile::NativeEbu2023` is the only production profile used here.
//! The compatibility profiles in the numerical crate remain differential tools
//! and are never accepted as native observation identity.

use std::collections::BTreeSet;
use std::fs::File;
use std::fmt;
use std::io::{self, Read};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;
use lofty::tag::ItemKey;
use tonepoet_true_peak::loudness::{
    AlbumLoudnessBuilder, AlbumLoudnessSummary, ChannelRole, IntegratedLoudness,
    LoudnessMeasurement, LoudnessMetricCoverage, LoudnessMetricDemand, LoudnessProfile,
    LoudnessSummary, LoudnessMeter,
};
use tonepoet_true_peak::replaygain::{
    calculate_replaygain, gain_db_to_opus_q78, ReplayGainCalculation, ReplayGainOptions,
};
use tonepoet_true_peak::{PeakLevel, ReportingPeakMeter, TruePeakResult};

const PRODUCTION_PROFILE: LoudnessProfile = LoudnessProfile::NativeEbu2023;
const METER_STORAGE_ALLOWANCE_BYTES: usize = 256 * 1024 * 1024;
const ALBUM_STORAGE_ALLOWANCE_BYTES: usize = 256 * 1024 * 1024;
// Source-pass CUE observers are simultaneously live. Split one application-
// level allowance across the group so track count cannot multiply a per-meter
// allowance without bound. Album reduction has its own independent allowance;
// the worst overlap is therefore explicitly bounded by these two budgets.
const CUE_ACTIVE_METER_STORAGE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const ORDINARY_REPLAYGAIN_REFERENCE_LUFS: f64 = -18.0;
const OPUS_R128_REFERENCE_LUFS: f64 = -23.0;
const PREVENT_CLIPPING_CEILING_DBTP: f64 = -1.0;
const OPUS_HEAD_SCAN_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetricDemand {
    IntegratedOnly,
    IntegratedAndRange,
}

impl MetricDemand {
    pub(crate) const fn from_requires_lra(requires_lra: bool) -> Self {
        if requires_lra {
            Self::IntegratedAndRange
        } else {
            Self::IntegratedOnly
        }
    }

    pub(crate) const fn native(self) -> LoudnessMetricDemand {
        match self {
            Self::IntegratedOnly => LoudnessMetricDemand::IntegratedOnly,
            Self::IntegratedAndRange => LoudnessMetricDemand::IntegratedAndRange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReaderAuthority {
    FfmpegDecodedArtifact,
    CueExactPcmMirror,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgrammeSubject {
    /// Exact track/group participant this programme observation belongs to.
    /// This is distinct from the artifact identity so album reduction can
    /// reject reordered, missing, or wrong-member observations before owned
    /// statistics are consumed.
    pub participant: String,
    pub identity: String,
    pub reader: ReaderAuthority,
    pub sample_rate_hz: u32,
    pub roles: Vec<ChannelRole>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeObservationSummary {
    pub subject: ProgrammeSubject,
    pub profile: LoudnessProfile,
    pub metric_coverage: LoudnessMetricCoverage,
    pub real_frames: u64,
    pub loudness: LoudnessSummary,
    pub reporting_peak: TruePeakResult,
    pub completion_evidence: String,
}

#[derive(Debug)]
pub struct OwnedTrackObservation {
    summary: NativeObservationSummary,
    measurement: LoudnessMeasurement,
    reporting_peak_linear: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MathematicalUnavailability {
    TooShort { frames: u64, required_frames: u64 },
    BelowAbsoluteGate,
    BelowRelativeGate,
    NoEligibleBlocks,
}

impl fmt::Display for MathematicalUnavailability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { frames, required_frames } => {
                write!(f, "too short ({frames} frames; {required_frames} required)")
            }
            Self::BelowAbsoluteGate => f.write_str("below absolute loudness gate"),
            Self::BelowRelativeGate => f.write_str("below relative loudness gate"),
            Self::NoEligibleBlocks => f.write_str("no eligible loudness blocks"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectedGain {
    pub calculation: Option<ReplayGainCalculation>,
    pub unavailable: Option<MathematicalUnavailability>,
    pub reporting_peak_linear: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayGainFileWriteReport {
    pub path: PathBuf,
    pub member_id: String,
    pub track: ProjectedGain,
    pub album: Option<ProjectedGain>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayGainWriteReport {
    pub files: Vec<ReplayGainFileWriteReport>,
}

impl ReplayGainWriteReport {
    pub(crate) fn has_unavailable_requested_gain(&self) -> bool {
        self.files.iter().any(|file| {
            file.track.unavailable.is_some()
                || file.album.as_ref().is_some_and(|album| album.unavailable.is_some())
        })
    }

    pub(crate) fn status_summary(&self) -> String {
        let mut unavailable = Vec::new();
        for file in &self.files {
            let label = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.member_id.clone());
            if let Some(reason) = file.track.unavailable.as_ref() {
                unavailable.push(format!("{label} Track gain unavailable: {reason}"));
            }
            if let Some(reason) = file.album.as_ref().and_then(|album| album.unavailable.as_ref()) {
                unavailable.push(format!("{label} Album gain unavailable: {reason}"));
            }
        }
        if unavailable.is_empty() {
            format!(
                "ReplayGain tags written ({} file{})",
                self.files.len(),
                if self.files.len() == 1 { "" } else { "s" }
            )
        } else {
            format!("ReplayGain metadata completed; {}", unavailable.join("; "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayGainManifestBinding {
    pub ordered_members: Vec<String>,
}

impl ReplayGainManifestBinding {
    pub(crate) fn new(ordered_members: Vec<String>) -> io::Result<Self> {
        if ordered_members.is_empty() {
            return Err(invalid("ReplayGain manifest is empty"));
        }
        let unique = ordered_members.iter().collect::<BTreeSet<_>>();
        if unique.len() != ordered_members.len() {
            return Err(invalid("ReplayGain manifest contains duplicate members"));
        }
        Ok(Self { ordered_members })
    }

    pub(crate) fn matches(&self, members: &[String]) -> bool {
        self.ordered_members == members
    }
}

/// Cloneable source-pass result. Owned sufficient statistics are consumed by
/// album reduction before this value is formed; they are never cloned or
/// serialized. The scalar album result is valid only for this exact manifest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayGainSourceScan {
    /// Members whose per-track observations are carried by this value and may
    /// be written by `apply_source_scan`.
    pub manifest: ReplayGainManifestBinding,
    /// Full cohort that contributed to `album`. For ordinary/CUE scans this is
    /// identical to `manifest`; an independent-file album batch may retain one
    /// member's track observation while binding the shared album reduction to
    /// the complete dispatcher-authored cohort.
    pub album_manifest: ReplayGainManifestBinding,
    pub demand: MetricDemand,
    pub tracks: Vec<NativeObservationSummary>,
    pub album: Option<AlbumLoudnessSummary>,
}

impl ReplayGainSourceScan {
    pub(crate) fn slice_for_members(&self, member_ids: &[String]) -> io::Result<Self> {
        let manifest = ReplayGainManifestBinding::new(member_ids.to_vec())?;
        let mut tracks = Vec::with_capacity(member_ids.len());
        for member_id in member_ids {
            let Some(track) = self
                .tracks
                .iter()
                .find(|track| track.subject.participant == *member_id)
            else {
                return Err(invalid(format!(
                    "ReplayGain batch scan has no observation for manifest member {member_id:?}"
                )));
            };
            tracks.push(track.clone());
        }
        Ok(Self {
            manifest,
            album_manifest: self.album_manifest.clone(),
            demand: self.demand,
            tracks,
            album: self.album.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PcmMirrorEncoding {
    S32Le,
    F32Le,
    F64Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterCapability {
    Supported(crate::metadata_persistence::MetadataPersistenceBackend),
    Unsupported(crate::metadata_persistence::MetadataPersistenceBackend),
}

pub(crate) fn writer_capability(path: &Path) -> io::Result<WriterCapability> {
    use crate::metadata_persistence::MetadataPersistenceBackend as B;
    let backend = crate::metadata_persistence::metadata_backend_for_path(path)
        .map_err(|error| invalid(format!("metadata backend for '{}': {error}", path.display())))?;
    let capability = match backend {
        B::NativeFlacVorbis
        | B::NativeDsfId3
        | B::NativeWavPackApe
        | B::LoftyVorbisComments
        | B::LoftyId3v2
        | B::LoftyApe
        | B::LoftyMp4Ilst => WriterCapability::Supported(backend),
        B::ReadOnlyApeFamily | B::UnsupportedDff | B::UnclassifiedLofty => {
            WriterCapability::Unsupported(backend)
        }
    };
    Ok(capability)
}

pub(crate) fn all_writers_supported(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        if let WriterCapability::Unsupported(backend) = writer_capability(path)? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("no admitted ReplayGain metadata writer for '{}' ({backend:?})", path.display()),
            ));
        }
    }
    Ok(())
}

pub(crate) struct NativeReplayGainObserver {
    subject: ProgrammeSubject,
    demand: MetricDemand,
    loudness: LoudnessMeter,
    reporting_peak: ReportingPeakMeter,
}

impl NativeReplayGainObserver {
    pub(crate) fn new(subject: ProgrammeSubject, demand: MetricDemand) -> io::Result<Self> {
        Self::new_with_storage_limit(subject, demand, METER_STORAGE_ALLOWANCE_BYTES)
    }

    fn new_with_storage_limit(
        subject: ProgrammeSubject,
        demand: MetricDemand,
        storage_limit_bytes: usize,
    ) -> io::Result<Self> {
        validate_roles(&subject.roles)?;
        let loudness = LoudnessMeter::with_roles_and_metric_demand_and_limit(
            subject.sample_rate_hz,
            &subject.roles,
            PRODUCTION_PROFILE,
            demand.native(),
            storage_limit_bytes,
        )
        .map_err(|error| invalid(format!("native loudness meter admission failed: {error}")))?;
        let reporting_peak = ReportingPeakMeter::new(subject.sample_rate_hz, subject.roles.len())
            .map_err(|error| invalid(format!("reporting peak meter admission failed: {error}")))?;
        Ok(Self {
            subject,
            demand,
            loudness,
            reporting_peak,
        })
    }

    pub(crate) fn push_interleaved(&mut self, samples: &[f64]) -> io::Result<()> {
        self.push_interleaved_profiled(samples, None)
    }

    fn push_interleaved_profiled(
        &mut self,
        samples: &[f64],
        mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
    ) -> io::Result<()> {
        use crate::convert::pipeline::rg_profile::ObserverStage;
        // The same meter methods and order are used with diagnostics on/off.
        // A failure terminates the entire grouped observation, as before.
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let loudness_result = self.loudness.push_interleaved(samples);
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::LoudnessPush, started.elapsed());
        }
        loudness_result
            .map_err(|error| invalid(format!("native loudness observation failed: {error}")))?;
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let peak_result = self.reporting_peak.push_interleaved(samples);
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::PeakPush, started.elapsed());
        }
        peak_result
            .map_err(|error| invalid(format!("reporting peak observation failed: {error}")))?;
        Ok(())
    }

    fn finish(self, completion_evidence: String) -> io::Result<OwnedTrackObservation> {
        self.finish_profiled(completion_evidence, None)
    }

    fn finish_profiled(
        self,
        completion_evidence: String,
        mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
    ) -> io::Result<OwnedTrackObservation> {
        use crate::convert::pipeline::rg_profile::ObserverStage;
        let expected_subject = self.subject.clone();
        let expected_demand = self.demand;
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let measurement_result = self.loudness.finalize();
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::LoudnessFinalize, started.elapsed());
        }
        let measurement = measurement_result
            .map_err(|error| invalid(format!("native loudness finalization failed: {error}")))?;
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let peak_result = self.reporting_peak.finalize();
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::PeakFinalize, started.elapsed());
        }
        let reporting_peak = peak_result
            .map_err(|error| invalid(format!("reporting peak finalization failed: {error}")))?;
        validate_completed_measurement(
            &expected_subject,
            expected_demand,
            &measurement.summary,
            &reporting_peak,
        )?;
        let reporting_peak_linear = reporting_peak_linear(&reporting_peak)?;
        let summary = NativeObservationSummary {
            subject: expected_subject,
            profile: PRODUCTION_PROFILE,
            metric_coverage: measurement.summary.metric_coverage,
            real_frames: measurement.summary.real_frames,
            loudness: measurement.summary.clone(),
            reporting_peak,
            completion_evidence,
        };
        Ok(OwnedTrackObservation {
            summary,
            measurement,
            reporting_peak_linear,
        })
    }
}

fn validate_roles(roles: &[ChannelRole]) -> io::Result<()> {
    if roles.is_empty() {
        return Err(invalid("decoded programme has no channel roles"));
    }
    if roles.iter().any(|role| *role == ChannelRole::Unused) {
        return Err(invalid("compatibility/unused channel roles are not valid for NativeEbu2023"));
    }
    Ok(())
}

fn validate_completed_measurement(
    subject: &ProgrammeSubject,
    demand: MetricDemand,
    summary: &LoudnessSummary,
    reporting_peak: &TruePeakResult,
) -> io::Result<()> {
    if summary.profile != PRODUCTION_PROFILE {
        return Err(invalid("production ReplayGain observation is not NativeEbu2023"));
    }
    if summary.sample_rate_hz != subject.sample_rate_hz || summary.roles != subject.roles {
        return Err(invalid("completed loudness result does not match bound rate/roles"));
    }
    if !summary.metric_coverage.satisfies(demand.native()) {
        return Err(invalid("completed loudness result has insufficient metric coverage"));
    }
    if summary.real_frames == 0 || reporting_peak.frames == 0 {
        return Err(invalid("empty decoder output is an operational observation failure"));
    }
    if summary.real_frames != reporting_peak.frames {
        return Err(invalid(format!(
            "loudness/reporting-peak frame mismatch: {} versus {}",
            summary.real_frames, reporting_peak.frames
        )));
    }
    let _ = reporting_peak_linear(reporting_peak)?;
    match summary.integrated {
        IntegratedLoudness::NoInput | IntegratedLoudness::NumericalRange => {
            return Err(invalid("native loudness result is operationally invalid"));
        }
        _ => {}
    }
    Ok(())
}

fn validate_retained_summary(
    summary: &NativeObservationSummary,
    demand: MetricDemand,
) -> io::Result<()> {
    if summary.profile != PRODUCTION_PROFILE
        || summary.metric_coverage != summary.loudness.metric_coverage
        || summary.real_frames != summary.loudness.real_frames
    {
        return Err(invalid(
            "retained ReplayGain observation envelope disagrees with its native loudness result",
        ));
    }
    validate_completed_measurement(
        &summary.subject,
        demand,
        &summary.loudness,
        &summary.reporting_peak,
    )
}

fn reporting_peak_linear(result: &TruePeakResult) -> io::Result<f64> {
    let linear = match result.overall {
        PeakLevel::Silence => 0.0,
        PeakLevel::Finite { linear, .. } => linear,
    };
    if !linear.is_finite() || linear < 0.0 {
        return Err(invalid("reporting peak is non-finite or negative"));
    }
    if result.channel_linear_peaks.iter().any(|peak| !peak.is_finite() || *peak < 0.0) {
        return Err(invalid("reporting channel peak is non-finite or negative"));
    }
    Ok(linear)
}

pub(crate) fn project_summary(
    summary: &LoudnessSummary,
    reporting_peak_linear: f64,
    prevent_clipping: bool,
    reference_lufs: f64,
) -> io::Result<ProjectedGain> {
    if summary.profile != PRODUCTION_PROFILE {
        return Err(invalid("refusing to project a non-NativeEbu2023 production observation"));
    }
    project_integrated(
        &summary.integrated,
        reporting_peak_linear,
        prevent_clipping,
        reference_lufs,
    )
}

fn project_integrated(
    integrated: &IntegratedLoudness,
    reporting_peak_linear: f64,
    prevent_clipping: bool,
    reference_lufs: f64,
) -> io::Result<ProjectedGain> {
    if !reporting_peak_linear.is_finite() || reporting_peak_linear < 0.0 {
        return Err(invalid("invalid ReplayGain reporting peak"));
    }
    let unavailable = mathematical_unavailability(integrated)?;
    let calculation = match integrated.finite_lufs() {
        Some(lufs) => Some(
            calculate_replaygain(
                lufs,
                reporting_peak_linear,
                ReplayGainOptions {
                    reference_lufs,
                    prevention_ceiling_dbtp: if prevent_clipping {
                        Some(PREVENT_CLIPPING_CEILING_DBTP)
                    } else {
                        None
                    },
                },
            )
            .map_err(|error| invalid(format!("ReplayGain projection failed: {error}")))?,
        ),
        None => None,
    };
    Ok(ProjectedGain {
        calculation,
        unavailable,
        reporting_peak_linear,
    })
}

fn mathematical_unavailability(value: &IntegratedLoudness) -> io::Result<Option<MathematicalUnavailability>> {
    Ok(match value {
        IntegratedLoudness::Finite { .. } => None,
        IntegratedLoudness::InsufficientFrames { frames, required_frames } => {
            Some(MathematicalUnavailability::TooShort {
                frames: *frames,
                required_frames: *required_frames,
            })
        }
        IntegratedLoudness::BelowAbsoluteGate => Some(MathematicalUnavailability::BelowAbsoluteGate),
        IntegratedLoudness::BelowRelativeGate => Some(MathematicalUnavailability::BelowRelativeGate),
        IntegratedLoudness::NoEligibleBlocks => Some(MathematicalUnavailability::NoEligibleBlocks),
        IntegratedLoudness::NoInput => return Err(invalid("empty programme has no valid ReplayGain observation")),
        IntegratedLoudness::NumericalRange => return Err(invalid("loudness numerical-range failure")),
    })
}

fn project_album(
    album: &AlbumLoudnessSummary,
    prevent_clipping: bool,
    reference_lufs: f64,
) -> io::Result<ProjectedGain> {
    if album.profile != PRODUCTION_PROFILE {
        return Err(invalid("album result is not NativeEbu2023"));
    }
    project_integrated(
        &album.integrated,
        album.reporting_peak_linear,
        prevent_clipping,
        reference_lufs,
    )
}

fn ordinary_options_reference() -> f64 {
    ORDINARY_REPLAYGAIN_REFERENCE_LUFS
}

pub(crate) fn measure_paths(
    paths: &[PathBuf],
    member_ids: &[String],
    grouping: tonepoet_pipeline::ReplayGainMode,
    demand: MetricDemand,
) -> io::Result<ReplayGainSourceScan> {
    if paths.len() != member_ids.len() {
        return Err(invalid("ReplayGain path/member count mismatch"));
    }
    let manifest = ReplayGainManifestBinding::new(member_ids.to_vec())?;
    let needs_album = matches!(
        grouping,
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both
    );
    let mut album = needs_album.then(|| AlbumLoudnessBuilder::with_metric_demand(
        ALBUM_STORAGE_ALLOWANCE_BYTES,
        demand.native(),
    ));
    let mut summaries = Vec::with_capacity(paths.len());
    for (path, member_id) in paths.iter().zip(member_ids) {
        let observation = observe_file(path, member_id.clone(), demand)?;
        if observation.summary.subject.participant != *member_id {
            return Err(invalid(format!(
                "ReplayGain observation participant mismatch: expected {member_id:?}, observed {:?}",
                observation.summary.subject.participant
            )));
        }
        if let Some(builder) = album.as_mut() {
            builder
                .push_track(observation.measurement.statistics, observation.reporting_peak_linear)
                .map_err(|error| invalid(format!("album statistics append failed: {error}")))?;
        }
        summaries.push(observation.summary);
    }
    let album = match album {
        Some(builder) => Some(
            builder
                .finalize()
                .map_err(|error| invalid(format!("album loudness finalization failed: {error}")))?,
        ),
        None => None,
    };
    Ok(ReplayGainSourceScan {
        album_manifest: manifest.clone(),
        manifest,
        demand,
        tracks: summaries,
        album,
    })
}

fn observe_file(path: &Path, member_id: String, demand: MetricDemand) -> io::Result<OwnedTrackObservation> {
    crate::tui::probe::ensure_ffmpeg_init_pub();
    let before = std::fs::metadata(path)?;
    if !before.is_file() {
        return Err(invalid(format!("ReplayGain reader requires a regular file: '{}'", path.display())));
    }
    let before_len = before.len();
    let wrapped_flac = crate::flac_envelope::WrappedFlacDecodeGuard::for_path(path, true)
        .map_err(|error| invalid(format!("inspect FLAC wrapper '{}': {error}", path.display())))?;
    let mut baseline_observation =
        crate::convert::pipeline::baseline::replaygain_observation_started(path, before_len);
    let subject_identity = artifact_subject_identity(path, &member_id, &before);

    let mut ictx = ffmpeg::format::input(path)
        .map_err(|error| invalid(format!("open ReplayGain input '{}': {error}", path.display())))?;
    let audio_stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| invalid(format!("no audio stream in '{}'", path.display())))?;
    let stream_index = audio_stream.index();
    let context = ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())
        .map_err(|error| invalid(format!("read audio parameters '{}': {error}", path.display())))?;
    let mut decoder = context
        .decoder()
        .audio()
        .map_err(|error| invalid(format!("open audio decoder '{}': {error}", path.display())))?;
    let rate = decoder.rate();
    let channels = usize::from(decoder.channels());
    let roles = ordered_roles_from_layout(decoder.channel_layout(), channels)?;
    let subject = ProgrammeSubject {
        participant: member_id.clone(),
        identity: subject_identity,
        reader: ReaderAuthority::FfmpegDecodedArtifact,
        sample_rate_hz: rate,
        roles: roles.clone(),
    };
    let mut observer = NativeReplayGainObserver::new(subject, demand)?;
    let mut decoded = ffmpeg::util::frame::Audio::empty();
    let mut decoded_frames = 0_u64;
    let mut frame_allocations = 0_u64;
    let mut frame_allocation_bytes = 0_u64;

    let mut accepted_wrapped_eof = false;
    loop {
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut ictx) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => break,
            Err(error) if wrapped_flac.accepts_post_extent_error(decoded_frames) => {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC demux EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    decoded_frames,
                );
                break;
            }
            Err(error) => {
                return Err(invalid(format!(
                    "demux ReplayGain input '{}': {error}",
                    path.display()
                )))
            }
        }
        if packet.stream() != stream_index {
            continue;
        }
        if let Err(error) = decoder.send_packet(&packet) {
            if wrapped_flac.accepts_post_extent_error(decoded_frames) {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC packet EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    decoded_frames,
                );
                break;
            }
            return Err(invalid(format!("send audio packet '{}': {error}", path.display())));
        }
        match drain_decoder_frames(
            &mut decoder,
            &mut decoded,
            rate,
            channels,
            &roles,
            &mut observer,
            &mut decoded_frames,
            &mut frame_allocations,
            &mut frame_allocation_bytes,
            false,
            wrapped_flac,
        )? {
            DecoderDrainOutcome::ReadyForInput | DecoderDrainOutcome::CleanEof => {}
            DecoderDrainOutcome::DeclaredWrappedFlacEof => {
                accepted_wrapped_eof = true;
                break;
            }
        }
    }
    if !accepted_wrapped_eof {
        if let Err(error) = decoder.send_eof() {
            if wrapped_flac.accepts_post_extent_error(decoded_frames) {
                accepted_wrapped_eof = true;
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC flush EOF after declared extent: path={}, frames={}, error={error}",
                    path.display(),
                    decoded_frames,
                );
            } else {
                return Err(invalid(format!("flush audio decoder '{}': {error}", path.display())));
            }
        }
    }
    if !accepted_wrapped_eof {
        match drain_decoder_frames(
            &mut decoder,
            &mut decoded,
            rate,
            channels,
            &roles,
            &mut observer,
            &mut decoded_frames,
            &mut frame_allocations,
            &mut frame_allocation_bytes,
            true,
            wrapped_flac,
        )? {
            DecoderDrainOutcome::CleanEof => {}
            DecoderDrainOutcome::DeclaredWrappedFlacEof => accepted_wrapped_eof = true,
            DecoderDrainOutcome::ReadyForInput => {
                return Err(invalid("decoder flush ended without clean EOF"));
            }
        }
    }

    wrapped_flac.validate_complete(decoded_frames).map_err(|error| {
        invalid(format!("{error} for '{}'", path.display()))
    })?;

    if decoded_frames == 0 {
        return Err(invalid(format!("decoder produced no audio for '{}'", path.display())));
    }
    let after = std::fs::metadata(path)?;
    if !same_artifact_generation(&before, &after) {
        return Err(invalid(format!("audio artifact changed while it was being observed: '{}'", path.display())));
    }
    let decode_eof_detail = if wrapped_flac.declared_sample_frames().is_some() && accepted_wrapped_eof {
        "verified ID3-wrapped FLAC declared-sample EOF"
    } else {
        "clean decoder EOF"
    };
    let observation = observer.finish(format!(
        "ffmpeg complete decode; {decode_eof_detail}; stable extent={} bytes",
        before_len
    ))?;
    if let Some(baseline) = baseline_observation.as_mut() {
        let decoded_pcm_bytes = decoded_frames
            .saturating_mul(u64::try_from(channels).unwrap_or(u64::MAX))
            .saturating_mul(std::mem::size_of::<f64>() as u64);
        baseline.mark_complete(
            decoded_pcm_bytes,
            frame_allocations,
            frame_allocation_bytes,
        );
    }
    Ok(observation)
}

fn artifact_subject_identity(
    path: &Path,
    member_id: &str,
    metadata: &std::fs::Metadata,
) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return format!(
            "artifact:{member_id}:{}:dev={}:ino={}:len={}:mtime={}.{}:ctime={}.{}",
            path.display(),
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        );
    }
    #[cfg(not(unix))]
    {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        format!(
            "artifact:{member_id}:{}:len={}:mtime_ns={modified:?}",
            path.display(),
            metadata.len(),
        )
    }
}

fn same_artifact_generation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderDrainOutcome {
    ReadyForInput,
    CleanEof,
    DeclaredWrappedFlacEof,
}

fn drain_decoder_frames(
    decoder: &mut ffmpeg::decoder::Audio,
    frame: &mut ffmpeg::util::frame::Audio,
    expected_rate: u32,
    expected_channels: usize,
    expected_roles: &[ChannelRole],
    observer: &mut NativeReplayGainObserver,
    frames: &mut u64,
    frame_allocations: &mut u64,
    frame_allocation_bytes: &mut u64,
    flushing: bool,
    wrapped_flac: crate::flac_envelope::WrappedFlacDecodeGuard,
) -> io::Result<DecoderDrainOutcome> {
    loop {
        match decoder.receive_frame(frame) {
            Ok(()) => {
                let rate = frame.rate();
                let channels = usize::from(frame.channels());
                if rate != expected_rate || channels != expected_channels {
                    return Err(invalid("decoder changed sample rate or channel count mid-programme"));
                }
                let roles = ordered_roles_from_layout(frame.channel_layout(), channels)?;
                if roles != expected_roles {
                    return Err(invalid("decoder changed ordered channel roles mid-programme"));
                }
                let frame_samples = u64::try_from(frame.samples())
                    .map_err(|_| invalid("frame sample count overflow"))?;
                let next_frames = wrapped_flac
                    .checked_advance(*frames, frame_samples)
                    .map_err(|error| invalid(error.to_string()))?;
                let interleaved = frame_to_interleaved_f64(frame, channels)?;
                *frame_allocations = frame_allocations.saturating_add(1);
                *frame_allocation_bytes = frame_allocation_bytes.saturating_add(
                    u64::try_from(interleaved.capacity())
                        .unwrap_or(u64::MAX)
                        .saturating_mul(std::mem::size_of::<f64>() as u64),
                );
                observer.push_interleaved(&interleaved)?;
                *frames = next_frames;
            }
            Err(ffmpeg::Error::Eof) => return Ok(DecoderDrainOutcome::CleanEof),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                if flushing {
                    return Err(invalid("decoder flush ended without clean EOF"));
                }
                return Ok(DecoderDrainOutcome::ReadyForInput);
            }
            Err(error) if wrapped_flac.accepts_post_extent_error(*frames) => {
                log::debug!(
                    "accepted legacy ID3-wrapped FLAC decoder EOF after declared extent: frames={}, error={error}",
                    *frames,
                );
                return Ok(DecoderDrainOutcome::DeclaredWrappedFlacEof);
            }
            Err(error) => {
                return Err(invalid(format!(
                    "audio decoder failed after {} decoded frames (declared wrapped extent {:?}): {error}",
                    *frames,
                    wrapped_flac.declared_sample_frames(),
                )))
            }
        }
    }
}

pub(crate) fn ordered_roles_from_layout(
    layout: ffmpeg::ChannelLayout,
    channels: usize,
) -> io::Result<Vec<ChannelRole>> {
    use ffmpeg::ChannelLayout as L;
    if channels == 0 {
        return Err(invalid("decoded audio has zero channels"));
    }
    if layout.is_empty() {
        return match channels {
            1 => Ok(vec![ChannelRole::Mono]),
            2 => Ok(vec![ChannelRole::Left, ChannelRole::Right]),
            _ => Err(invalid(format!(
                "{channels}-channel programme has no authoritative channel layout"
            ))),
        };
    }
    if layout.channels() as usize != channels {
        return Err(invalid("decoder channel layout/count mismatch"));
    }
    let roles = if layout == L::MONO {
        vec![ChannelRole::Mono]
    } else if layout == L::STEREO {
        vec![ChannelRole::Left, ChannelRole::Right]
    } else if layout == L::SURROUND {
        vec![ChannelRole::Left, ChannelRole::Right, ChannelRole::Center]
    } else if layout == L::_3POINT1 {
        vec![ChannelRole::Left, ChannelRole::Right, ChannelRole::Center, ChannelRole::Lfe]
    } else if layout == L::QUAD {
        vec![ChannelRole::Left, ChannelRole::Right, ChannelRole::LeftBack, ChannelRole::RightBack]
    } else if layout == L::_5POINT0 {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ]
    } else if layout == L::_5POINT1 {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ]
    } else if layout == L::_5POINT0_BACK {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::LeftBack,
            ChannelRole::RightBack,
        ]
    } else if layout == L::_5POINT1_BACK {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftBack,
            ChannelRole::RightBack,
        ]
    } else if layout == L::_7POINT0 {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::LeftBack,
            ChannelRole::RightBack,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ]
    } else if layout == L::_7POINT1 {
        vec![
            ChannelRole::Left,
            ChannelRole::Right,
            ChannelRole::Center,
            ChannelRole::Lfe,
            ChannelRole::LeftBack,
            ChannelRole::RightBack,
            ChannelRole::LeftSurround,
            ChannelRole::RightSurround,
        ]
    } else {
        return Err(invalid(format!(
            "unsupported NativeEbu2023 ordered channel layout: {layout:?}"
        )));
    };
    if roles.len() != channels {
        return Err(invalid("resolved ordered roles do not match channel count"));
    }
    Ok(roles)
}

fn frame_to_interleaved_f64(frame: &ffmpeg::util::frame::Audio, channels: usize) -> io::Result<Vec<f64>> {
    use ffmpeg::util::format::sample::{Sample, Type};
    let samples = frame.samples();
    let total = samples
        .checked_mul(channels)
        .ok_or_else(|| invalid("decoded frame geometry overflow"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(total)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "PCM observer allocation failed"))?;

    macro_rules! planar {
        ($ty:ty, $convert:expr) => {{
            let planes = (0..channels)
                .map(|ch| frame.plane::<$ty>(ch))
                .collect::<Vec<_>>();
            for index in 0..samples {
                for plane in &planes {
                    output.push(($convert)(plane[index]));
                }
            }
        }};
    }
    macro_rules! packed {
        ($width:expr, $decode:expr, $convert:expr) => {{
            let raw = frame.data(0);
            let expected_bytes = total
                .checked_mul($width)
                .ok_or_else(|| invalid("decoded packed PCM geometry overflow"))?;
            if raw.len() < expected_bytes {
                return Err(invalid("decoded packed PCM plane is truncated"));
            }
            for bytes in raw[..expected_bytes].chunks_exact($width) {
                output.push(($convert)(($decode)(bytes)));
            }
        }};
    }

    match frame.format() {
        Sample::U8(Type::Planar) => planar!(u8, |v: u8| (f64::from(v) - 128.0) / 128.0),
        Sample::U8(Type::Packed) => packed!(1, |b: &[u8]| b[0], |v: u8| (f64::from(v) - 128.0) / 128.0),
        Sample::I16(Type::Planar) => planar!(i16, |v: i16| f64::from(v) / 32768.0),
        Sample::I16(Type::Packed) => packed!(2, |b: &[u8]| i16::from_ne_bytes([b[0], b[1]]), |v: i16| f64::from(v) / 32768.0),
        Sample::I32(Type::Planar) => planar!(i32, |v: i32| f64::from(v) / 2147483648.0),
        Sample::I32(Type::Packed) => packed!(4, |b: &[u8]| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]), |v: i32| f64::from(v) / 2147483648.0),
        // ffmpeg-next exposes AV_SAMPLE_FMT_S64/P but does not implement its
        // typed frame::audio::Sample trait for i64. Decode S64P from the raw
        // per-channel planes instead of using frame.plane::<i64>().
        Sample::I64(Type::Planar) => {
            let expected_bytes = samples
                .checked_mul(8)
                .ok_or_else(|| invalid("decoded planar s64 geometry overflow"))?;
            let planes = (0..channels)
                .map(|channel| {
                    let raw = frame.data(channel);
                    if raw.len() < expected_bytes {
                        return Err(invalid("decoded planar s64 plane is truncated"));
                    }
                    Ok(&raw[..expected_bytes])
                })
                .collect::<io::Result<Vec<_>>>()?;
            for index in 0..samples {
                let offset = index * 8;
                for plane in &planes {
                    let bytes: [u8; 8] = plane[offset..offset + 8]
                        .try_into()
                        .expect("eight-byte s64 sample");
                    output.push((i64::from_ne_bytes(bytes) as f64) / 9223372036854775808.0);
                }
            }
        }
        Sample::I64(Type::Packed) => packed!(8, |b: &[u8]| i64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]), |v: i64| (v as f64) / 9223372036854775808.0),
        Sample::F32(Type::Planar) => planar!(f32, |v: f32| f64::from(v)),
        Sample::F32(Type::Packed) => packed!(4, |b: &[u8]| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]), |v: f32| f64::from(v)),
        Sample::F64(Type::Planar) => planar!(f64, |v: f64| v),
        Sample::F64(Type::Packed) => packed!(8, |b: &[u8]| f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]), |v: f64| v),
        Sample::None => return Err(invalid("decoder returned no PCM sample format")),
    }
    if output.len() != total {
        return Err(invalid("decoded PCM extraction returned the wrong sample count"));
    }
    if output.iter().any(|sample| !sample.is_finite()) {
        return Err(invalid("decoded PCM contains a non-finite sample"));
    }
    Ok(output)
}

pub(crate) fn cue_observer_storage_limit(observer_count: usize) -> io::Result<usize> {
    if observer_count == 0 {
        return Err(invalid("CUE ReplayGain observer set is empty"));
    }
    Ok(CUE_ACTIVE_METER_STORAGE_BUDGET_BYTES / observer_count)
}

pub(crate) fn preflight_cue_observer(
    sample_rate_hz: u32,
    channels: u16,
    demand: MetricDemand,
    storage_limit_bytes: usize,
) -> io::Result<()> {
    let roles = match channels {
        1 => vec![ChannelRole::Mono],
        2 => vec![ChannelRole::Left, ChannelRole::Right],
        _ => return Err(invalid("CUE source-pass loudness admits only mono/stereo mirror roles")),
    };
    let subject = ProgrammeSubject {
        participant: "admission-probe".to_string(),
        identity: "cue-source-pass:admission-probe".to_string(),
        reader: ReaderAuthority::CueExactPcmMirror,
        sample_rate_hz,
        roles,
    };
    drop(NativeReplayGainObserver::new_with_storage_limit(
        subject,
        demand,
        storage_limit_bytes,
    )?);
    Ok(())
}

/// One qualified native source-range meter. The owned result is valid only
/// after the process supervisor has ALSO proved a clean decoder exit. The
/// caller binds it to the immutable IP-02 range, album identity and source
/// generation; this reader neither owns a child nor writes ReplayGain tags.
#[derive(Debug, Clone)]
pub struct CueSourceRangeObservationSpec {
    pub member_id: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frames: u64,
    pub(crate) demand: MetricDemand,
    pub(crate) storage_limit_bytes: usize,
}

/// Raw s32le pipe, not a WAV FIFO: use the exact native S1 normalization and
/// 16,384-frame push boundaries. A connected anonymous pipe has ordinary
/// blocking read/EOF semantics; there is no no-writer FIFO polling loop.
/// The supervisor owns the writer and closes it on success, failure and
/// cancellation. No statistic may be admitted before EOF *and* child success.
pub(crate) fn observe_cue_source_range_pcm<R: Read>(
    mut reader: R,
    spec: &CueSourceRangeObservationSpec,
    mut extent: crate::convert::pipeline::cue_flac_range::ExactPcmExtent,
    cancel: &CancellationToken,
) -> io::Result<OwnedTrackObservation> {
    let roles = match spec.channels {
        1 => vec![ChannelRole::Mono],
        2 => vec![ChannelRole::Left, ChannelRole::Right],
        _ => return Err(invalid("source-range meter admits only mono/stereo")),
    };
    let frame_bytes = u64::from(spec.channels)
        .checked_mul(4)
        .ok_or_else(|| invalid("source-range PCM frame-size overflow"))?;
    let required_bytes = spec.frames
        .checked_mul(frame_bytes)
        .ok_or_else(|| invalid("source-range PCM byte extent overflow"))?;
    if spec.frames == 0 || required_bytes != extent.expected_bytes() {
        return Err(invalid("source-range meter frame geometry and qualified extent disagree"));
    }
    let subject = ProgrammeSubject {
        participant: spec.member_id.clone(),
        identity: format!("cue-source-pass:{}", spec.member_id),
        reader: ReaderAuthority::CueExactPcmMirror,
        sample_rate_hz: spec.sample_rate_hz,
        roles,
    };
    let mut meter = NativeReplayGainObserver::new_with_storage_limit(
        subject, spec.demand, spec.storage_limit_bytes,
    )?;
    let chunk_bytes = usize::try_from(16_384_u64 * frame_bytes)
        .map_err(|_| invalid("source-range chunk does not fit in usize"))?;
    let mut raw = vec![0u8; chunk_bytes];
    let mut remaining = required_bytes;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "source-range meter cancelled"));
        }
        let wanted = usize::try_from(remaining.min(chunk_bytes as u64))
            .map_err(|_| invalid("source-range read extent overflow"))?;
        reader.read_exact(&mut raw[..wanted])?;
        extent.observe_bytes(wanted).map_err(|error| invalid(error.to_string()))?;
        let samples = decode_pcm_bytes(&raw[..wanted], PcmMirrorEncoding::S32Le)?;
        if samples.len() % usize::from(spec.channels) != 0 {
            return Err(invalid("source-range meter received an incomplete PCM frame"));
        }
        meter.push_interleaved(&samples)?;
        remaining -= wanted as u64;
    }
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "source-range meter cancelled"));
    }
    let mut surplus = [0u8; 1];
    if reader.read(&mut surplus)? != 0 {
        return Err(invalid("source-range producer emitted surplus PCM after exact extent"));
    }
    extent.finish().map_err(|error| invalid(error.to_string()))?;
    let result = meter.finish(format!(
        "supervised source-range s32le pipe: {required_bytes} bytes, exact EOF; decoder exit checked by runner"
    ))?;
    if result.summary.real_frames != spec.frames {
        return Err(invalid("source-range native observer frame count differs from qualified source"));
    }
    Ok(result)
}

/// Exact CUE source-pass reader. It validates the exact header, exact payload,
/// frame alignment, and EOF *after* the declared payload, so a later surplus
/// FIFO write cannot be mistaken for a complete observation.
pub(crate) fn observe_cue_mirror(
    path: &Path,
    member_id: String,
    expected_header: &[u8],
    sample_rate_hz: u32,
    channels: u16,
    frames: u64,
    encoding: PcmMirrorEncoding,
    demand: MetricDemand,
    storage_limit_bytes: usize,
    cancel: &CancellationToken,
    mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
) -> io::Result<OwnedTrackObservation> {
    use crate::convert::pipeline::rg_profile::ObserverStage;
    let roles = match channels {
        1 => vec![ChannelRole::Mono],
        2 => vec![ChannelRole::Left, ChannelRole::Right],
        _ => return Err(invalid("CUE source-pass loudness admits only mono/stereo mirror roles")),
    };
    let subject = ProgrammeSubject {
        participant: member_id.clone(),
        identity: format!("cue-source-pass:{member_id}"),
        reader: ReaderAuthority::CueExactPcmMirror,
        sample_rate_hz,
        roles,
    };
    let mut observer = NativeReplayGainObserver::new_with_storage_limit(subject, demand, storage_limit_bytes)?;
    let t = profile.as_ref().map(|_| std::time::Instant::now());
    let opened = open_fifo_reader_nonblocking(path);
    if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
        stats.add(ObserverStage::FifoOpen, started.elapsed());
    }
    let mut reader = opened?;
    let mut saw_data = false;
    let mut header = vec![0_u8; expected_header.len()];
    let t = profile.as_ref().map(|_| std::time::Instant::now());
    let header_result = read_fifo_exact(&mut reader, &mut header, cancel, &mut saw_data, profile.as_deref_mut());
    if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
        stats.add(ObserverStage::HeaderRead, started.elapsed());
    }
    header_result?;
    if header != expected_header {
        return Err(invalid("CUE source-pass mirror header does not match planned geometry"));
    }
    let bytes_per_sample = match encoding {
        PcmMirrorEncoding::S32Le | PcmMirrorEncoding::F32Le => 4_u64,
        PcmMirrorEncoding::F64Le => 8_u64,
    };
    let frame_bytes = bytes_per_sample
        .checked_mul(u64::from(channels))
        .ok_or_else(|| invalid("CUE mirror frame-size overflow"))?;
    let payload_bytes = frames
        .checked_mul(frame_bytes)
        .ok_or_else(|| invalid("CUE mirror payload-size overflow"))?;
    let mut remaining = payload_bytes;
    let chunk_frames = 16_384_u64;
    let mut raw = Vec::new();
    while remaining > 0 {
        let wanted = remaining.min(chunk_frames * frame_bytes) as usize;
        raw.resize(wanted, 0);
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let read_result = read_fifo_exact(&mut reader, &mut raw, cancel, &mut saw_data, profile.as_deref_mut());
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::PcmRead, started.elapsed());
        }
        read_result?;
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        let decoded = decode_pcm_bytes(&raw, encoding);
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.add(ObserverStage::PcmDecode, started.elapsed());
        }
        let interleaved = decoded?;
        if interleaved.len() % usize::from(channels) != 0 {
            return Err(invalid("CUE mirror ended on a partial PCM frame"));
        }
        observer.push_interleaved_profiled(&interleaved, profile.as_deref_mut())?;
        if let Some(stats) = profile.as_deref_mut() {
            stats.blocks = stats.blocks.saturating_add(1);
            stats.pcm_bytes = stats.pcm_bytes.saturating_add(wanted as u64);
        }
        remaining -= wanted as u64;
    }
    let mut surplus = [0_u8; 1];
    let eof_start = profile.as_ref().map(|_| std::time::Instant::now());
    let eof_result = (|| -> io::Result<()> {
        loop {
            if cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "CUE source-pass observation cancelled"));
            }
            match reader.read(&mut surplus) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(invalid("CUE mirror contains unexpected PCM after declared payload")),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(stats) = profile.as_deref_mut() {
                        stats.would_block_poll_count = stats.would_block_poll_count.saturating_add(1);
                    }
                    wait_fifo_readable(&reader, cancel, profile.as_deref_mut())?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    })();
    if let (Some(stats), Some(started)) = (profile.as_deref_mut(), eof_start) {
        stats.add(ObserverStage::EofWait, started.elapsed());
    }
    eof_result?;
    let result = observer.finish_profiled(format!(
        "exact CUE FIFO header/payload/EOF; {payload_bytes} payload bytes"
    ), profile.as_deref_mut())?;
    if result.summary.real_frames != frames {
        return Err(invalid("CUE mirror frame count does not match declared programme extent"));
    }
    Ok(result)
}


#[cfg(unix)]
fn open_fifo_reader_nonblocking(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_fifo_reader_nonblocking(path: &Path) -> io::Result<File> {
    File::open(path)
}

// A connected-but-empty FIFO returns EAGAIN; the producer's next write or
// close must wake the observer promptly. Do not use poll for the *initial*
// no-writer state: a FIFO after writer closure can continuously report POLLHUP,
// and polling it would spin. That state retains the bounded setup backoff.
#[cfg(unix)]
fn wait_fifo_readable(
    reader: &File,
    cancel: &CancellationToken,
    profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    wait_fifo_fd_readable(reader.as_raw_fd(), cancel, profile)
}

#[cfg(unix)]
fn wait_fifo_fd_readable(
    fd: std::os::fd::RawFd,
    cancel: &CancellationToken,
    mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
) -> io::Result<()> {
    use crate::convert::pipeline::rg_profile::ObserverStage;
    let mut ready_fd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "CUE source-pass observation cancelled"));
        }
        ready_fd.revents = 0;
        let t = profile.as_ref().map(|_| std::time::Instant::now());
        // Same bounded descriptor-readiness pattern as script_supervisor.
        // Periodic cancellation checks also cover a writer that never resumes.
        let ready = unsafe { libc::poll(&mut ready_fd, 1, 50) };
        if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
            stats.readiness_wait_count = stats.readiness_wait_count.saturating_add(1);
            stats.add(ObserverStage::ReadinessWait, started.elapsed());
            if ready == 0 {
                stats.readiness_timeout_count = stats.readiness_timeout_count.saturating_add(1);
            }
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        // POLLHUP is not an error: the FIFO can still contain unread PCM.
        // Retry read first, and let its EOF/expected-length checks decide.
        if ready_fd.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if ready_fd.revents & libc::POLLERR != 0 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        return Ok(());
    }
}

// The source-pass mirror is Unix-only. Retain the legacy cross-platform
// fallback rather than changing unrelated builds or transport paths.
#[cfg(not(unix))]
fn wait_fifo_readable(
    _reader: &File,
    _cancel: &CancellationToken,
    mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
) -> io::Result<()> {
    use crate::convert::pipeline::rg_profile::ObserverStage;
    let t = profile.as_ref().map(|_| std::time::Instant::now());
    std::thread::sleep(Duration::from_millis(5));
    if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
        stats.add(ObserverStage::PollSleep, started.elapsed());
    }
    Ok(())
}

fn read_fifo_exact(
    reader: &mut File,
    output: &mut [u8],
    cancel: &CancellationToken,
    saw_data: &mut bool,
    mut profile: Option<&mut crate::convert::pipeline::rg_profile::ObserverMetrics>,
) -> io::Result<()> {
    use crate::convert::pipeline::rg_profile::ObserverStage;
    let mut offset = 0usize;
    // EAGAIN on a nonblocking FIFO proves a writer was connected even if
    // no payload has arrived yet. A later EOF must be reported as truncation,
    // not mistaken for a writer that has never connected.
    let mut saw_connected_writer = *saw_data;
    while offset < output.len() {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "CUE source-pass observation cancelled",
            ));
        }
        match reader.read(&mut output[offset..]) {
            Ok(0) if !*saw_data && !saw_connected_writer => {
                // Nonblocking FIFO readers report EOF until the writer first
                // connects. This is setup wait, not programme EOF.
                let t = profile.as_ref().map(|_| std::time::Instant::now());
                std::thread::sleep(Duration::from_millis(5));
                if let (Some(stats), Some(started)) = (profile.as_deref_mut(), t) {
                    stats.no_writer_poll_count = stats.no_writer_poll_count.saturating_add(1);
                    stats.add(ObserverStage::PollSleep, started.elapsed());
                }
            }
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("CUE source-pass FIFO ended with {} bytes remaining", output.len() - offset),
                ));
            }
            Ok(read) => {
                *saw_data = true;
                offset += read;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                saw_connected_writer = true;
                if let Some(stats) = profile.as_deref_mut() {
                    stats.would_block_poll_count = stats.would_block_poll_count.saturating_add(1);
                }
                wait_fifo_readable(reader, cancel, profile.as_deref_mut())?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn decode_pcm_bytes(raw: &[u8], encoding: PcmMirrorEncoding) -> io::Result<Vec<f64>> {
    let width = match encoding {
        PcmMirrorEncoding::S32Le | PcmMirrorEncoding::F32Le => 4,
        PcmMirrorEncoding::F64Le => 8,
    };
    if raw.len() % width != 0 {
        return Err(invalid("PCM mirror chunk is not sample-aligned"));
    }
    let mut output = Vec::with_capacity(raw.len() / width);
    for bytes in raw.chunks_exact(width) {
        let value = match encoding {
            PcmMirrorEncoding::S32Le => {
                f64::from(i32::from_le_bytes(bytes.try_into().expect("width checked"))) / 2147483648.0
            }
            PcmMirrorEncoding::F32Le => {
                f64::from(f32::from_le_bytes(bytes.try_into().expect("width checked")))
            }
            PcmMirrorEncoding::F64Le => {
                f64::from_le_bytes(bytes.try_into().expect("width checked"))
            }
        };
        if !value.is_finite() {
            return Err(invalid("PCM mirror contains a non-finite sample"));
        }
        output.push(value);
    }
    Ok(output)
}

pub(crate) fn reduce_cue_observations(
    manifest: ReplayGainManifestBinding,
    demand: MetricDemand,
    observations: Vec<OwnedTrackObservation>,
    needs_album: bool,
) -> io::Result<ReplayGainSourceScan> {
    if observations.len() != manifest.ordered_members.len() {
        return Err(invalid("CUE source-pass observation count does not match manifest"));
    }
    for (expected, observation) in manifest.ordered_members.iter().zip(&observations) {
        if observation.summary.subject.participant != *expected {
            return Err(invalid(format!(
                "CUE source-pass observation belongs to {:?}, expected manifest member {expected:?}",
                observation.summary.subject.participant
            )));
        }
    }
    let mut builder = needs_album.then(|| AlbumLoudnessBuilder::with_metric_demand(
        ALBUM_STORAGE_ALLOWANCE_BYTES,
        demand.native(),
    ));
    let mut summaries = Vec::with_capacity(observations.len());
    for observation in observations {
        if let Some(album) = builder.as_mut() {
            album
                .push_track(observation.measurement.statistics, observation.reporting_peak_linear)
                .map_err(|error| invalid(format!("CUE album statistics append failed: {error}")))?;
        }
        summaries.push(observation.summary);
    }
    let album = builder
        .map(|builder| builder.finalize().map_err(|error| invalid(format!("CUE album finalization failed: {error}"))))
        .transpose()?;
    Ok(ReplayGainSourceScan {
        album_manifest: manifest.clone(),
        manifest,
        demand,
        tracks: summaries,
        album,
    })
}

pub(crate) fn apply_source_scan(
    paths: &[PathBuf],
    member_ids: &[String],
    mode: tonepoet_pipeline::ReplayGainMode,
    prevent_clipping: bool,
    scan: &ReplayGainSourceScan,
) -> io::Result<ReplayGainWriteReport> {
    if paths.len() != scan.tracks.len() || !scan.manifest.matches(member_ids) {
        return Err(invalid("source-pass ReplayGain result is not bound to the current manifest"));
    }
    if scan
        .tracks
        .iter()
        .zip(member_ids)
        .any(|(track, member)| track.subject.participant != *member)
    {
        return Err(invalid(
            "source-pass ReplayGain observation order/participant binding does not match the current manifest",
        ));
    }
    for track in &scan.tracks {
        validate_retained_summary(track, scan.demand)?;
    }
    if !scan
        .manifest
        .ordered_members
        .iter()
        .all(|member| scan.album_manifest.ordered_members.contains(member))
    {
        return Err(invalid(
            "retained ReplayGain member manifest is not a subset of its album cohort",
        ));
    }
    if let Some(album) = scan.album.as_ref() {
        if album.profile != PRODUCTION_PROFILE
            || !album.metric_coverage.satisfies(scan.demand.native())
            || album.track_count != scan.album_manifest.ordered_members.len()
            || !album.reporting_peak_linear.is_finite()
            || album.reporting_peak_linear < 0.0
        {
            return Err(invalid(
                "retained ReplayGain album envelope does not match the bound album cohort/coverage",
            ));
        }
    } else if matches!(
        mode,
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both
    ) {
        return Err(invalid(
            "retained Album/Both ReplayGain scan is missing its current-group reduction",
        ));
    }
    all_writers_supported(paths)?;
    let mut files = Vec::with_capacity(paths.len());
    for ((path, member_id), track) in paths.iter().zip(member_ids).zip(&scan.tracks) {
        let projection = write_projected_metadata(path, track, mode, prevent_clipping, scan.album.as_ref())?;
        files.push(ReplayGainFileWriteReport {
            path: path.clone(),
            member_id: member_id.clone(),
            track: projection.track,
            album: projection.album,
        });
    }
    Ok(ReplayGainWriteReport { files })
}

pub(crate) fn measure_and_write_paths(
    paths: &[PathBuf],
    member_ids: &[String],
    mode: tonepoet_pipeline::ReplayGainMode,
    prevent_clipping: bool,
    requires_lra: bool,
) -> io::Result<ReplayGainWriteReport> {
    all_writers_supported(paths)?;
    let demand = MetricDemand::from_requires_lra(requires_lra);
    let measure_scope = crate::convert::pipeline::baseline::scoped_event(
        "replaygain_measure",
        serde_json::json!({
            "path_count": paths.len(),
            "mode": format!("{mode:?}"),
            "requires_lra": requires_lra,
        }),
    );
    let scan = measure_paths(paths, member_ids, mode, demand)?;
    drop(measure_scope);
    let mut files = Vec::with_capacity(paths.len());
    for ((path, member_id), track) in paths.iter().zip(member_ids).zip(&scan.tracks) {
        let write_scope = crate::convert::pipeline::baseline::scoped_event(
            "replaygain_metadata_write",
            serde_json::json!({
                "path": path.display().to_string(),
                "member_id": member_id,
            }),
        );
        let projection = write_projected_metadata(path, track, mode, prevent_clipping, scan.album.as_ref())?;
        drop(write_scope);
        files.push(ReplayGainFileWriteReport {
            path: path.clone(),
            member_id: member_id.clone(),
            track: projection.track,
            album: projection.album,
        });
    }
    Ok(ReplayGainWriteReport { files })
}

#[derive(Debug, Clone, PartialEq)]
struct MetadataProjectionReport {
    track: ProjectedGain,
    album: Option<ProjectedGain>,
}

fn write_projected_metadata(
    path: &Path,
    track: &NativeObservationSummary,
    mode: tonepoet_pipeline::ReplayGainMode,
    prevent_clipping: bool,
    album: Option<&AlbumLoudnessSummary>,
) -> io::Result<MetadataProjectionReport> {
    let is_opus = opus_header_gain_q78(path)?.is_some();
    let reference = if is_opus {
        OPUS_R128_REFERENCE_LUFS
    } else {
        ordinary_options_reference()
    };
    let track_peak = reporting_peak_linear(&track.reporting_peak)?;
    let track_projection = project_summary(&track.loudness, track_peak, prevent_clipping, reference)?;
    let album_projection = match mode {
        tonepoet_pipeline::ReplayGainMode::Track => None,
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both => {
            Some(project_album(
                album.ok_or_else(|| invalid("Album/Both ReplayGain is missing current-group reduction"))?,
                prevent_clipping,
                reference,
            )?)
        }
    };
    if is_opus {
        write_opus_r128(path, mode, &track_projection, album_projection.as_ref())?;
    } else {
        write_ordinary_replaygain(path, mode, &track_projection, album_projection.as_ref())?;
    }
    Ok(MetadataProjectionReport {
        track: track_projection,
        album: album_projection,
    })
}

fn gain_string(projection: &ProjectedGain) -> Vec<String> {
    projection
        .calculation
        .as_ref()
        .map(|value| vec![format!("{:.2} dB", value.applied_gain_db)])
        .unwrap_or_default()
}

fn peak_string(linear: f64) -> Vec<String> {
    vec![format!("{linear:.6}")]
}

fn write_ordinary_replaygain(
    path: &Path,
    mode: tonepoet_pipeline::ReplayGainMode,
    track: &ProjectedGain,
    album: Option<&ProjectedGain>,
) -> io::Result<()> {
    let mut changes = vec![
        (ItemKey::ReplayGainTrackGain, gain_string(track)),
        (ItemKey::ReplayGainTrackPeak, peak_string(track.reporting_peak_linear)),
    ];
    match mode {
        tonepoet_pipeline::ReplayGainMode::Track => {
            changes.push((ItemKey::ReplayGainAlbumGain, Vec::new()));
            changes.push((ItemKey::ReplayGainAlbumPeak, Vec::new()));
        }
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both => {
            let album = album.ok_or_else(|| invalid("Album/Both ReplayGain has no album projection"))?;
            changes.push((ItemKey::ReplayGainAlbumGain, gain_string(album)));
            changes.push((ItemKey::ReplayGainAlbumPeak, peak_string(album.reporting_peak_linear)));
        }
    }
    // Ordinary output must not retain an incompatible Opus R128 family copied
    // from an input container.
    changes.push((ItemKey::Unknown("R128_TRACK_GAIN".to_string()), Vec::new()));
    changes.push((ItemKey::Unknown("R128_ALBUM_GAIN".to_string()), Vec::new()));
    write_desired_state(path, &changes)
}

fn write_opus_r128(
    path: &Path,
    mode: tonepoet_pipeline::ReplayGainMode,
    track: &ProjectedGain,
    album: Option<&ProjectedGain>,
) -> io::Result<()> {
    let header_before = opus_header_gain_q78(path)?
        .ok_or_else(|| invalid("Opus metadata projection requires an OpusHead"))?;
    let track_value = projection_to_q78(track)?;
    let album_value = match mode {
        tonepoet_pipeline::ReplayGainMode::Track => None,
        tonepoet_pipeline::ReplayGainMode::Album | tonepoet_pipeline::ReplayGainMode::Both => {
            projection_to_q78(album.ok_or_else(|| invalid("Opus Album/Both projection is missing"))?)?
        }
    };
    let changes = vec![
        (ItemKey::ReplayGainTrackGain, Vec::new()),
        (ItemKey::ReplayGainTrackPeak, Vec::new()),
        (ItemKey::ReplayGainAlbumGain, Vec::new()),
        (ItemKey::ReplayGainAlbumPeak, Vec::new()),
        (
            ItemKey::Unknown("R128_TRACK_GAIN".to_string()),
            track_value.map(|value| vec![value.to_string()]).unwrap_or_default(),
        ),
        (
            ItemKey::Unknown("R128_ALBUM_GAIN".to_string()),
            album_value.map(|value| vec![value.to_string()]).unwrap_or_default(),
        ),
    ];
    write_desired_state(path, &changes)?;
    let header_after = opus_header_gain_q78(path)?
        .ok_or_else(|| invalid("OpusHead disappeared after metadata mutation"))?;
    if header_before != header_after {
        return Err(invalid(format!(
            "Opus mandatory header gain changed during ReplayGain metadata mutation: {header_before} -> {header_after}"
        )));
    }
    Ok(())
}

fn projection_to_q78(projection: &ProjectedGain) -> io::Result<Option<i16>> {
    projection
        .calculation
        .as_ref()
        .map(|calculation| {
            gain_db_to_opus_q78(calculation.applied_gain_db)
                .map_err(|error| invalid(format!("Opus R128 Q7.8 projection failed: {error}")))
        })
        .transpose()
}

fn write_desired_state(path: &Path, changes: &[(ItemKey, Vec<String>)]) -> io::Result<()> {
    if let WriterCapability::Unsupported(backend) = writer_capability(path)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("no admitted metadata writer for '{}' ({backend:?})", path.display()),
        ));
    }
    let report = crate::tui::probe::write_all_tag_value_lists(path, changes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("write ReplayGain desired metadata state to '{}': {error}", path.display()),
        )
    })?;
    for warning in report.durability_warnings {
        log::warn!("ReplayGain metadata write warning for '{}': {warning}", path.display());
    }
    Ok(())
}

/// Remove/suppress inherited ReplayGain and R128 fields without starting any
/// replacement meter. This is used independently of the scan enablement bit.
pub(crate) fn remove_inherited_measurement_fields(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        // The Phase 4 lifecycle contract permits a separate cleanup mutation
        // only when an actual inherited measurement field exists. Presence is
        // key-based rather than value-based so empty, binary, or otherwise
        // malformed fields still require disposition.
        if !measurement_fields_present(path)? {
            continue;
        }
        if let WriterCapability::Unsupported(backend) = writer_capability(path)? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "stale ReplayGain/R128 metadata is present on '{}' but backend {backend:?} has no admitted writer to remove it",
                    path.display()
                ),
            ));
        }
        write_desired_state(
            path,
            &[
                (ItemKey::ReplayGainTrackGain, Vec::new()),
                (ItemKey::ReplayGainTrackPeak, Vec::new()),
                (ItemKey::ReplayGainAlbumGain, Vec::new()),
                (ItemKey::ReplayGainAlbumPeak, Vec::new()),
                (ItemKey::Unknown("R128_TRACK_GAIN".to_string()), Vec::new()),
                (ItemKey::Unknown("R128_ALBUM_GAIN".to_string()), Vec::new()),
            ],
        )?;
    }
    Ok(())
}

fn measurement_key_name(key: &ItemKey) -> Option<&'static str> {
    match key {
        ItemKey::ReplayGainTrackGain => Some("REPLAYGAIN_TRACK_GAIN"),
        ItemKey::ReplayGainTrackPeak => Some("REPLAYGAIN_TRACK_PEAK"),
        ItemKey::ReplayGainAlbumGain => Some("REPLAYGAIN_ALBUM_GAIN"),
        ItemKey::ReplayGainAlbumPeak => Some("REPLAYGAIN_ALBUM_PEAK"),
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("REPLAYGAIN_TRACK_GAIN") => {
            Some("REPLAYGAIN_TRACK_GAIN")
        }
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("REPLAYGAIN_TRACK_PEAK") => {
            Some("REPLAYGAIN_TRACK_PEAK")
        }
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_GAIN") => {
            Some("REPLAYGAIN_ALBUM_GAIN")
        }
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_PEAK") => {
            Some("REPLAYGAIN_ALBUM_PEAK")
        }
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("R128_TRACK_GAIN") => {
            Some("R128_TRACK_GAIN")
        }
        ItemKey::Unknown(value) if value.eq_ignore_ascii_case("R128_ALBUM_GAIN") => {
            Some("R128_ALBUM_GAIN")
        }
        _ => None,
    }
}

fn is_measurement_key(key: &ItemKey) -> bool {
    measurement_key_name(key).is_some()
}

fn same_measurement_key(actual: &ItemKey, expected: &ItemKey) -> bool {
    match (measurement_key_name(actual), measurement_key_name(expected)) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => actual == expected,
    }
}

fn tags_contain_any_measurement_key(tags: &[&lofty::tag::Tag], keys: &[ItemKey]) -> bool {
    tags.iter().any(|tag| {
        tag.items().any(|item| {
            keys.iter()
                .any(|key| same_measurement_key(item.key(), key))
        })
    })
}

fn tags_contain_measurement_field(tags: &[&lofty::tag::Tag]) -> bool {
    tags.iter()
        .any(|tag| tag.items().any(|item| is_measurement_key(item.key())))
}

fn measurement_keys_present(path: &Path, keys: &[ItemKey]) -> io::Result<bool> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).map_err(|error| {
        invalid(format!(
            "inspect inherited ReplayGain/R128 metadata on '{}': {error}",
            path.display()
        ))
    })?;
    let tags = tagged.tags().iter().collect::<Vec<_>>();
    Ok(tags_contain_any_measurement_key(&tags, keys))
}

fn measurement_fields_present(path: &Path) -> io::Result<bool> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).map_err(|error| {
        invalid(format!(
            "inspect inherited ReplayGain/R128 metadata on '{}': {error}",
            path.display()
        ))
    })?;
    let tags = tagged.tags().iter().collect::<Vec<_>>();
    Ok(tags_contain_measurement_field(&tags))
}


fn exactly_one_valid_value(
    tags: &[&lofty::tag::Tag],
    key: &ItemKey,
    valid: impl Fn(&str) -> bool,
) -> bool {
    use lofty::tag::ItemValue;

    // Completeness is about the physical field set, not merely the subset of
    // values Lofty can expose as text. A valid text value plus a second binary
    // or malformed instance is still a duplicate and cannot be skip evidence.
    let mut items = tags
        .iter()
        .flat_map(|tag| tag.items())
        .filter(|item| same_measurement_key(item.key(), key));
    let Some(item) = items.next() else {
        return false;
    };
    if items.next().is_some() {
        return false;
    }
    match item.value() {
        ItemValue::Text(value) | ItemValue::Locator(value) => valid(value.trim()),
        ItemValue::Binary(_) => false,
    }
}

fn valid_gain_text(value: &str) -> bool {
    let numeric = value
        .strip_suffix("dB")
        .or_else(|| value.strip_suffix("DB"))
        .map(str::trim)
        .unwrap_or(value);
    numeric.parse::<f64>().is_ok_and(f64::is_finite)
}

fn valid_peak_text(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|peak| peak.is_finite() && peak >= 0.0)
}

fn valid_opus_q78_text(value: &str) -> bool {
    value.parse::<i16>().is_ok()
}

/// Whether the final artifact contains exactly the retained field set required
/// by Tonepoet's current writer contract. Serialized Album/Both fields still do
/// not establish current-group identity; callers may use this predicate only
/// for field-shape validation, never as album manifest proof.
pub(crate) fn replaygain_metadata_fields_complete(
    path: &Path,
    mode: tonepoet_pipeline::ReplayGainMode,
) -> io::Result<bool> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).map_err(|error| {
        invalid(format!(
            "inspect ReplayGain completeness on '{}': {error}",
            path.display()
        ))
    })?;
    let tags = tagged.tags().iter().collect::<Vec<_>>();
    if tags.is_empty() {
        return Ok(false);
    }

    if opus_header_gain_q78(path)?.is_some() {
        let track = exactly_one_valid_value(
            &tags,
            &ItemKey::Unknown("R128_TRACK_GAIN".to_string()),
            valid_opus_q78_text,
        );
        let album = exactly_one_valid_value(
            &tags,
            &ItemKey::Unknown("R128_ALBUM_GAIN".to_string()),
            valid_opus_q78_text,
        );
        return Ok(match mode {
            tonepoet_pipeline::ReplayGainMode::Track => track,
            tonepoet_pipeline::ReplayGainMode::Album
            | tonepoet_pipeline::ReplayGainMode::Both => track && album,
        });
    }

    let track_gain = exactly_one_valid_value(&tags, &ItemKey::ReplayGainTrackGain, valid_gain_text);
    let track_peak = exactly_one_valid_value(&tags, &ItemKey::ReplayGainTrackPeak, valid_peak_text);
    let album_gain = exactly_one_valid_value(&tags, &ItemKey::ReplayGainAlbumGain, valid_gain_text);
    let album_peak = exactly_one_valid_value(&tags, &ItemKey::ReplayGainAlbumPeak, valid_peak_text);
    let track = track_gain && track_peak;
    let album = album_gain && album_peak;
    Ok(match mode {
        tonepoet_pipeline::ReplayGainMode::Track => track,
        tonepoet_pipeline::ReplayGainMode::Album
        | tonepoet_pipeline::ReplayGainMode::Both => track && album,
    })
}

fn track_skip_normalization_changes(is_opus: bool) -> Vec<(ItemKey, Vec<String>)> {
    if is_opus {
        vec![
            (ItemKey::ReplayGainTrackGain, Vec::new()),
            (ItemKey::ReplayGainTrackPeak, Vec::new()),
            (ItemKey::ReplayGainAlbumGain, Vec::new()),
            (ItemKey::ReplayGainAlbumPeak, Vec::new()),
            (ItemKey::Unknown("R128_ALBUM_GAIN".to_string()), Vec::new()),
        ]
    } else {
        vec![
            (ItemKey::ReplayGainAlbumGain, Vec::new()),
            (ItemKey::ReplayGainAlbumPeak, Vec::new()),
            (ItemKey::Unknown("R128_TRACK_GAIN".to_string()), Vec::new()),
            (ItemKey::Unknown("R128_ALBUM_GAIN".to_string()), Vec::new()),
        ]
    }
}

/// Normalize a Track + SkipIfComplete artifact to the same format-specific
/// desired metadata state that a fresh Track write would have produced.
/// Existing validated requested Track fields are preserved; incompatible or
/// stale field families are removed without starting a meter.
pub(crate) fn normalize_track_skip_metadata(paths: &[PathBuf]) -> io::Result<()> {
    for path in paths {
        let opus_header_before = opus_header_gain_q78(path)?;
        let changes = track_skip_normalization_changes(opus_header_before.is_some());
        let stale_keys = changes
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if !measurement_keys_present(path, &stale_keys)? {
            // The already-validated requested Track field set is already in
            // the writer's exact format-specific terminal state. Do not cross
            // the mutation boundary solely to perform an empty cleanup.
            continue;
        }
        write_desired_state(path, &changes)?;
        if let Some(header_before) = opus_header_before {
            let header_after = opus_header_gain_q78(path)?
                .ok_or_else(|| invalid("OpusHead disappeared after Track-skip metadata normalization"))?;
            if header_before != header_after {
                return Err(invalid(format!(
                    "Opus mandatory header gain changed during Track-skip metadata normalization: {header_before} -> {header_after}"
                )));
            }
        }
    }
    Ok(())
}

fn opus_header_gain_q78(path: &Path) -> io::Result<Option<i16>> {
    // Classify from the actual audio codec before looking for OpusHead bytes.
    // A magic-byte coincidence in another carrier must never switch the
    // ReplayGain serialization contract to Opus R128.
    crate::tui::probe::ensure_ffmpeg_init_pub();
    let input = ffmpeg::format::input(path)
        .map_err(|error| invalid(format!("open codec probe '{}': {error}", path.display())))?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| invalid(format!("no audio stream in '{}'", path.display())))?;
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|error| invalid(format!("read codec parameters '{}': {error}", path.display())))?;
    if context.id() != ffmpeg::codec::Id::OPUS {
        return Ok(None);
    }

    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.by_ref()
        .take(OPUS_HEAD_SCAN_LIMIT as u64)
        .read_to_end(&mut buffer)?;
    let offset = buffer
        .windows(8)
        .position(|window| window == b"OpusHead")
        .ok_or_else(|| invalid("Opus codec has no bounded, parseable OpusHead"))?;
    let gain_start = offset
        .checked_add(16)
        .ok_or_else(|| invalid("OpusHead gain offset overflow"))?;
    if gain_start + 2 > buffer.len() {
        return Err(invalid("truncated OpusHead before mandatory output gain"));
    }
    Ok(Some(i16::from_le_bytes([
        buffer[gain_start],
        buffer[gain_start + 1],
    ])))
}

pub(crate) fn integrated_unavailability(
    summary: &NativeObservationSummary,
) -> io::Result<Option<MathematicalUnavailability>> {
    mathematical_unavailability(&summary.loudness.integrated)
}

pub(crate) fn reporting_peak_dbtp(summary: &NativeObservationSummary) -> Option<f64> {
    match summary.reporting_peak.overall {
        PeakLevel::Silence => None,
        PeakLevel::Finite { dbtp, .. } if dbtp.is_finite() => Some(dbtp),
        _ => None,
    }
}

pub(crate) fn finite_integrated_lufs(summary: &NativeObservationSummary) -> Option<f64> {
    summary.loudness.integrated.finite_lufs()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue_ip07_synthetic_pcm(frames: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(frames * 8);
        for i in 0..frames {
            let value = (((i % 137) as i64 - 68) * 24_000_000) as i32;
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&(-value).to_le_bytes());
        }
        data
    }

    fn cue_ip07_spec(frames: u64) -> CueSourceRangeObservationSpec {
        CueSourceRangeObservationSpec {
            member_id: "source:1:disc:-:track:1".to_string(),
            sample_rate_hz: 44_100,
            channels: 2,
            frames,
            demand: MetricDemand::IntegratedOnly,
            storage_limit_bytes: 8 * 1024 * 1024,
        }
    }

    #[test]
    fn cue_ip07_exact_s32le_native_sample_and_peak_identity() {
        let frames = 44_100;
        let raw = cue_ip07_synthetic_pcm(frames);
        let spec = cue_ip07_spec(frames as u64);
        let scan = observe_cue_source_range_pcm(
            io::Cursor::new(&raw), &spec,
            crate::convert::pipeline::cue_flac_range::ExactPcmExtent::for_expected_bytes(raw.len() as u64),
            &CancellationToken::new(),
        ).expect("exact supervised-pipe-equivalent raw PCM");
        let subject = ProgrammeSubject {
            participant: spec.member_id.clone(),
            identity: format!("cue-source-pass:{}", spec.member_id),
            reader: ReaderAuthority::CueExactPcmMirror,
            sample_rate_hz: 44_100,
            roles: vec![ChannelRole::Left, ChannelRole::Right],
        };
        let mut reference = NativeReplayGainObserver::new_with_storage_limit(
            subject, spec.demand, spec.storage_limit_bytes,
        ).unwrap();
        for chunk in raw.chunks(16_384 * 8) {
            let samples = decode_pcm_bytes(chunk, PcmMirrorEncoding::S32Le).unwrap();
            reference.push_interleaved(&samples).unwrap();
        }
        let reference = reference.finish("same native S1 PCM chunks".to_string()).unwrap();
        assert_eq!(scan.summary.real_frames, 44_100);
        assert_eq!(scan.summary.subject, reference.summary.subject);
        assert_eq!(scan.summary.loudness, reference.summary.loudness);
        assert_eq!(scan.summary.reporting_peak, reference.summary.reporting_peak);
        assert_eq!(scan.measurement.statistics.integrated_observations(),
            reference.measurement.statistics.integrated_observations());
        assert_eq!(scan.measurement.statistics.lra_observations(),
            reference.measurement.statistics.lra_observations());
    }

    #[test]
    fn cue_ip07_reader_refuses_missing_extra_and_wrong_extent() {
        let raw = cue_ip07_synthetic_pcm(100);
        let spec = cue_ip07_spec(100);
        let extent = || crate::convert::pipeline::cue_flac_range::ExactPcmExtent::for_expected_bytes(raw.len() as u64);
        for data in [&raw[..raw.len() - 1], &raw[..raw.len() - 8]] {
            assert!(observe_cue_source_range_pcm(
                io::Cursor::new(data), &spec, extent(), &CancellationToken::new(),
            ).is_err(), "early EOF cannot create a mathematically unavailable observation");
        }
        let mut surplus = raw.clone();
        surplus.push(0);
        assert!(observe_cue_source_range_pcm(
            io::Cursor::new(surplus), &spec, extent(), &CancellationToken::new(),
        ).is_err(), "full expected prefix with surplus is an operational failure");
        assert!(observe_cue_source_range_pcm(
            io::Cursor::new(&raw), &spec,
            crate::convert::pipeline::cue_flac_range::ExactPcmExtent::for_expected_bytes((raw.len() + 4) as u64),
            &CancellationToken::new(),
        ).is_err(), "caller may not substitute an unsupported extent");
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(observe_cue_source_range_pcm(
            io::Cursor::new(&raw), &spec, extent(), &cancelled,
        ).unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn id3_wrapped_flac_observes_declared_extent_and_succeeds() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/regression/id3_wrapped_flac/id3v2_id3v1_48000.flac");
        let extent = crate::flac_envelope::wrapped_flac_extent(&path)
            .expect("wrapper inspection")
            .expect("fixture must be recognized as wrapped FLAC");
        assert_eq!(extent.sample_frames, 48_000);

        let observation = observe_file(
            &path,
            "id3-wrapped-flac".to_string(),
            MetricDemand::IntegratedOnly,
        )
        .expect("native ReplayGain/loudness reader must accept the verified trailing ID3v1 wrapper");
        assert_eq!(observation.summary.loudness.real_frames, 48_000);
    }

    #[test]
    fn id3v1_trailer_only_stream_copy_observes_declared_extent_and_succeeds() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/regression/id3_wrapped_flac/id3v2_id3v1_48000.flac");
        let bytes = std::fs::read(&source).expect("read wrapped FLAC fixture");
        assert!(bytes.starts_with(b"ID3"));
        let temp = tempfile::tempdir().expect("temp dir");
        let staged = temp.path().join("stream-copy.flac");
        std::fs::write(&staged, &bytes[10..]).expect("write trailer-only staged FLAC");

        assert_eq!(
            crate::flac_envelope::wrapped_flac_extent(&staged).expect("wrapper inspection"),
            None,
            "a trailer-only stream copy is not the exact legacy source wrapper"
        );
        let observation = observe_file(
            &staged,
            "id3v1-trailer-only-stream-copy".to_string(),
            MetricDemand::IntegratedOnly,
        )
        .expect("ReplayGain reader must accept a verified trailer-only stream copy");
        assert_eq!(observation.summary.loudness.real_frames, 48_000);
    }

    #[test]
    fn demand_is_derived_before_meter_construction() {
        assert_eq!(
            MetricDemand::from_requires_lra(false).native(),
            LoudnessMetricDemand::IntegratedOnly
        );
        assert_eq!(
            MetricDemand::from_requires_lra(true).native(),
            LoudnessMetricDemand::IntegratedAndRange
        );
    }

    #[test]
    fn prevention_mapping_is_explicit_and_projection_only() {
        let summary = LoudnessSummary {
            sample_rate_hz: 48_000,
            roles: vec![ChannelRole::Mono],
            profile: PRODUCTION_PROFILE,
            metric_coverage: LoudnessMetricCoverage::IntegratedOnly,
            real_frames: 48_000,
            integrated: IntegratedLoudness::Finite {
                lufs: -30.0,
                absolute_observations: 1,
                relative_observations: 1,
            },
            range: tonepoet_true_peak::loudness::LoudnessRange::NotRequested,
            absolute_integrated_observations: 1,
            absolute_lra_observations: 0,
            retained_storage_bytes: 0,
        };
        let limited = project_summary(&summary, 1.0, true, -18.0).unwrap();
        let unrestricted = project_summary(&summary, 1.0, false, -18.0).unwrap();
        assert!(limited.calculation.unwrap().limited);
        assert!(!unrestricted.calculation.unwrap().limited);
    }

    #[test]
    fn short_track_preserves_reporting_peak_without_inventing_gain() {
        let summary = LoudnessSummary {
            sample_rate_hz: 48_000,
            roles: vec![ChannelRole::Mono],
            profile: PRODUCTION_PROFILE,
            metric_coverage: LoudnessMetricCoverage::IntegratedOnly,
            real_frames: 100,
            integrated: IntegratedLoudness::InsufficientFrames {
                frames: 100,
                required_frames: 19_200,
            },
            range: tonepoet_true_peak::loudness::LoudnessRange::NotRequested,
            absolute_integrated_observations: 0,
            absolute_lra_observations: 0,
            retained_storage_bytes: 0,
        };
        let projected = project_summary(&summary, 0.91, true, -18.0).unwrap();
        assert!(projected.calculation.is_none());
        assert_eq!(projected.reporting_peak_linear, 0.91);
        assert!(matches!(
            projected.unavailable,
            Some(MathematicalUnavailability::TooShort { .. })
        ));
    }

    #[test]
    fn manifest_identity_is_ordered_and_rejects_duplicates() {
        let a = ReplayGainManifestBinding::new(vec!["a".into(), "b".into()]).unwrap();
        assert!(a.matches(&["a".into(), "b".into()]));
        assert!(!a.matches(&["b".into(), "a".into()]));
        assert!(ReplayGainManifestBinding::new(vec!["a".into(), "a".into()]).is_err());
    }

    #[test]
    fn direct_opus_projection_differs_from_shifted_limited_ordinary_projection() {
        let summary = LoudnessSummary {
            sample_rate_hz: 48_000,
            roles: vec![ChannelRole::Mono],
            profile: PRODUCTION_PROFILE,
            metric_coverage: LoudnessMetricCoverage::IntegratedOnly,
            real_frames: 48_000,
            integrated: IntegratedLoudness::Finite {
                lufs: -35.0,
                absolute_observations: 1,
                relative_observations: 1,
            },
            range: tonepoet_true_peak::loudness::LoudnessRange::NotRequested,
            absolute_integrated_observations: 1,
            absolute_lra_observations: 0,
            retained_storage_bytes: 0,
        };
        let ordinary = project_summary(&summary, 1.0, true, -18.0).unwrap().calculation.unwrap();
        let opus = project_summary(&summary, 1.0, true, -23.0).unwrap().calculation.unwrap();
        assert_ne!(opus.applied_gain_db, ordinary.applied_gain_db - 5.0);
    }

    #[test]
    fn measurement_presence_is_key_based_not_value_based() {
        use lofty::tag::{ItemValue, Tag, TagItem, TagType};

        assert!(is_measurement_key(&ItemKey::ReplayGainTrackGain));
        assert!(is_measurement_key(&ItemKey::ReplayGainTrackPeak));
        assert!(is_measurement_key(&ItemKey::ReplayGainAlbumGain));
        assert!(is_measurement_key(&ItemKey::ReplayGainAlbumPeak));
        assert!(is_measurement_key(&ItemKey::Unknown("replaygain_track_gain".into())));
        assert!(is_measurement_key(&ItemKey::Unknown("r128_track_gain".into())));
        assert!(is_measurement_key(&ItemKey::Unknown("R128_ALBUM_GAIN".into())));
        assert!(same_measurement_key(
            &ItemKey::ReplayGainTrackGain,
            &ItemKey::Unknown("replaygain_track_gain".into())
        ));
        assert!(!is_measurement_key(&ItemKey::TrackTitle));

        let empty = Tag::new(TagType::VorbisComments);
        assert!(!tags_contain_measurement_field(&[&empty]));

        let mut malformed = Tag::new(TagType::VorbisComments);
        malformed.push_unchecked(TagItem::new(
            ItemKey::ReplayGainTrackGain,
            ItemValue::Binary(vec![0xde, 0xad]),
        ));
        assert!(tags_contain_measurement_field(&[&malformed]));

        let mut empty_r128 = Tag::new(TagType::VorbisComments);
        empty_r128.push_unchecked(TagItem::new(
            ItemKey::Unknown("r128_album_gain".to_string()),
            ItemValue::Text(String::new()),
        ));
        assert!(tags_contain_measurement_field(&[&empty_r128]));
        assert!(tags_contain_any_measurement_key(
            &[&empty_r128],
            &[ItemKey::Unknown("R128_ALBUM_GAIN".to_string())],
        ));
    }

    #[test]
    fn completeness_rejects_malformed_or_duplicate_physical_instances() {
        use lofty::tag::{ItemValue, Tag, TagItem, TagType};

        let mut tag = Tag::new(TagType::VorbisComments);
        tag.push_unchecked(TagItem::new(
            ItemKey::ReplayGainTrackGain,
            ItemValue::Text("+1.00 dB".to_string()),
        ));
        assert!(exactly_one_valid_value(
            &[&tag],
            &ItemKey::ReplayGainTrackGain,
            valid_gain_text,
        ));

        tag.push_unchecked(TagItem::new(
            ItemKey::Unknown("replaygain_track_gain".to_string()),
            ItemValue::Binary(vec![0x01, 0x02]),
        ));
        assert!(!exactly_one_valid_value(
            &[&tag],
            &ItemKey::ReplayGainTrackGain,
            valid_gain_text,
        ));

        let mut malformed = Tag::new(TagType::VorbisComments);
        malformed.push_unchecked(TagItem::new(
            ItemKey::ReplayGainTrackPeak,
            ItemValue::Text(String::new()),
        ));
        assert!(!exactly_one_valid_value(
            &[&malformed],
            &ItemKey::ReplayGainTrackPeak,
            valid_peak_text,
        ));
    }

    #[test]
    fn track_skip_normalization_matches_format_specific_writer_family() {
        let ordinary = track_skip_normalization_changes(false)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        assert!(!ordinary.contains(&ItemKey::ReplayGainTrackGain));
        assert!(!ordinary.contains(&ItemKey::ReplayGainTrackPeak));
        assert!(ordinary.contains(&ItemKey::ReplayGainAlbumGain));
        assert!(ordinary.contains(&ItemKey::ReplayGainAlbumPeak));
        assert!(ordinary.contains(&ItemKey::Unknown("R128_TRACK_GAIN".into())));
        assert!(ordinary.contains(&ItemKey::Unknown("R128_ALBUM_GAIN".into())));

        let opus = track_skip_normalization_changes(true)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        assert!(!opus.contains(&ItemKey::Unknown("R128_TRACK_GAIN".into())));
        assert!(opus.contains(&ItemKey::Unknown("R128_ALBUM_GAIN".into())));
        assert!(opus.contains(&ItemKey::ReplayGainTrackGain));
        assert!(opus.contains(&ItemKey::ReplayGainTrackPeak));
        assert!(opus.contains(&ItemKey::ReplayGainAlbumGain));
        assert!(opus.contains(&ItemKey::ReplayGainAlbumPeak));
    }

    #[test]
    fn write_report_preserves_typed_unavailability() {
        let unavailable = MathematicalUnavailability::TooShort {
            frames: 100,
            required_frames: 19_200,
        };
        let report = ReplayGainWriteReport {
            files: vec![ReplayGainFileWriteReport {
                path: PathBuf::from("short.flac"),
                member_id: "short".into(),
                track: ProjectedGain {
                    calculation: None,
                    unavailable: Some(unavailable.clone()),
                    reporting_peak_linear: 0.91,
                },
                album: None,
            }],
        };
        assert!(report.has_unavailable_requested_gain());
        assert!(report.status_summary().contains(&unavailable.to_string()));
    }

    #[test]
    fn write_report_distinguishes_unavailable_track_from_finite_album() {
        let album_calculation = ReplayGainCalculation {
            requested_gain_db: 2.0,
            applied_gain_db: 2.0,
            reporting_peak_linear: 0.75,
            proposed_peak_linear: 0.94,
            resulting_peak_linear: 0.94,
            limited: false,
        };
        let report = ReplayGainWriteReport {
            files: vec![ReplayGainFileWriteReport {
                path: PathBuf::from("short-in-album.flac"),
                member_id: "short-in-album".into(),
                track: ProjectedGain {
                    calculation: None,
                    unavailable: Some(MathematicalUnavailability::TooShort {
                        frames: 100,
                        required_frames: 19_200,
                    }),
                    reporting_peak_linear: 0.99,
                },
                album: Some(ProjectedGain {
                    calculation: Some(album_calculation),
                    unavailable: None,
                    reporting_peak_linear: album_calculation.reporting_peak_linear,
                }),
            }],
        };
        assert!(report.has_unavailable_requested_gain());
        assert!(report.status_summary().contains("Track gain unavailable"));
        assert_eq!(
            report.files[0]
                .album
                .as_ref()
                .and_then(|album| album.calculation),
            Some(album_calculation)
        );
        assert_eq!(report.files[0].track.reporting_peak_linear, 0.99);
    }

    #[test]
    fn write_report_retains_exact_finite_projection_used_by_serializer() {
        let calculation = ReplayGainCalculation {
            requested_gain_db: 4.25,
            applied_gain_db: 1.75,
            reporting_peak_linear: 0.98,
            proposed_peak_linear: 1.60,
            resulting_peak_linear: 1.20,
            limited: true,
        };
        let projected = ProjectedGain {
            calculation: Some(calculation),
            unavailable: None,
            reporting_peak_linear: calculation.reporting_peak_linear,
        };
        let report = ReplayGainWriteReport {
            files: vec![ReplayGainFileWriteReport {
                path: PathBuf::from("finite.flac"),
                member_id: "finite".into(),
                track: projected,
                album: None,
            }],
        };
        let retained = report.files[0]
            .track
            .calculation
            .expect("finite calculation retained");
        assert_eq!(retained, calculation);
        assert_eq!(retained.requested_gain_db, 4.25);
        assert_eq!(retained.applied_gain_db, 1.75);
        assert_eq!(retained.reporting_peak_linear, 0.98);
        assert_eq!(retained.proposed_peak_linear, 1.60);
        assert_eq!(retained.resulting_peak_linear, 1.20);
        assert!(retained.limited);
        assert!(!report.has_unavailable_requested_gain());
    }

    #[test]
    fn mathematical_unavailability_reasons_remain_distinct_in_reports() {
        let reasons = [
            MathematicalUnavailability::TooShort {
                frames: 100,
                required_frames: 19_200,
            },
            MathematicalUnavailability::BelowAbsoluteGate,
            MathematicalUnavailability::BelowRelativeGate,
            MathematicalUnavailability::NoEligibleBlocks,
        ];
        let rendered = reasons.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
        assert_eq!(rendered.len(), reasons.len());
    }

    #[test]
    fn profiled_cue_meter_matches_unprofiled_samples_and_result_bits() {
        // Same production meters, input order, and exact f64 samples. The
        // profile is not allowed to affect accepted frames or arithmetic.
        let subject = ProgrammeSubject {
            participant: "profiling-equivalence".into(),
            identity: "cue-source-pass:profiling-equivalence".into(),
            reader: ReaderAuthority::CueExactPcmMirror,
            sample_rate_hz: 44_100,
            roles: vec![ChannelRole::Left, ChannelRole::Right],
        };
        let mut ordinary = NativeReplayGainObserver::new(subject.clone(), MetricDemand::IntegratedOnly).unwrap();
        let mut profiled = NativeReplayGainObserver::new(subject, MetricDemand::IntegratedOnly).unwrap();
        let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
        let audio: Vec<f64> = (0..(44_100 * 2))
            .map(|index| 0.4 * (index as f64 * 0.007).sin())
            .collect();
        for chunk in audio.chunks(1024) {
            ordinary.push_interleaved(chunk).unwrap();
            profiled.push_interleaved_profiled(chunk, Some(&mut stats)).unwrap();
        }
        let a = ordinary.finish("exact sample fixture".into()).unwrap();
        let b = profiled.finish_profiled("exact sample fixture".into(), Some(&mut stats)).unwrap();
        assert_eq!(a.summary, b.summary, "instrumentation must not change mathematical results");
        assert_eq!(a.reporting_peak_linear.to_bits(), b.reporting_peak_linear.to_bits());
        assert!(stats.loudness_push_ns > 0);
        assert!(stats.peak_push_ns > 0);
    }

    #[test]
    fn cue_pcm_decoder_rejects_partial_sample_and_nonfinite_float() {
        assert!(decode_pcm_bytes(&[0, 1, 2], PcmMirrorEncoding::S32Le).is_err());
        assert!(decode_pcm_bytes(&f32::NAN.to_le_bytes(), PcmMirrorEncoding::F32Le).is_err());
    }

    #[cfg(unix)]
    mod fifo_readiness_tests {
        use super::*;
        use std::io::Write;
        use std::sync::mpsc;
        use std::thread;

        fn fifo() -> (tempfile::TempDir, PathBuf) {
            use rustix::fs::{mknodat, FileType, Mode, CWD};
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("mirror.fifo");
            mknodat(CWD, path.as_path(), FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
            (temp, path)
        }

        #[test]
        fn no_writer_is_a_bounded_cancelable_setup_wait() {
            let (_dir, path) = fifo();
            let token = CancellationToken::new();
            let reader_cancel = token.clone();
            let (opened, ready) = mpsc::channel();
            let worker = thread::spawn(move || {
                let mut file = open_fifo_reader_nonblocking(&path).unwrap();
                opened.send(()).unwrap();
                let mut bytes = [0_u8; 8];
                let mut saw_data = false;
                let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
                let result = read_fifo_exact(&mut file, &mut bytes, &reader_cancel, &mut saw_data, Some(&mut stats));
                (result, stats)
            });
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
            // Allow the setup backoff to execute; no writer will ever connect.
            thread::sleep(Duration::from_millis(80));
            token.cancel();
            let (result, stats) = worker.join().unwrap();
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(stats.no_writer_poll_count > 0);
            assert!(stats.no_writer_poll_count < 100, "no-writer state must not spin");
            assert_eq!(stats.readiness_wait_count, 0, "never poll a disconnected FIFO");
        }

        #[test]
        fn connected_empty_writer_waits_for_readiness_without_spin_or_sleep() {
            let (_dir, path) = fifo();
            let mut reader = open_fifo_reader_nonblocking(&path).unwrap();
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let mut probe = [0_u8; 1];
            assert_eq!(reader.read(&mut probe).unwrap_err().kind(), io::ErrorKind::WouldBlock);
            let (tx, rx) = mpsc::channel();
            let worker = thread::spawn(move || {
                let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
                let result = wait_fifo_readable(&reader, &CancellationToken::new(), Some(&mut stats))
                    .and_then(|()| reader.read(&mut probe));
                tx.send((result, probe, stats)).unwrap();
            });
            assert!(matches!(rx.recv_timeout(Duration::from_millis(130)), Err(mpsc::RecvTimeoutError::Timeout)),
                "an attached but idle writer cannot make the FIFO readable");
            writer.write_all(b"A").unwrap();
            let (result, byte, stats) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.join().unwrap();
            assert_eq!(result.unwrap(), 1);
            assert_eq!(byte[0], b'A');
            assert!(stats.readiness_wait_count >= 1);
            assert!(stats.readiness_timeout_count <= stats.readiness_wait_count);
            assert!(stats.readiness_wait_count < 128, "idle writer caused a poll spin");
            assert_eq!(stats.poll_sleep_ns, 0, "active transfer must not sleep on EAGAIN");
        }

        #[test]
        fn exact_irregular_transfer_and_truncated_payload_preserve_byte_accounting() {
            let (_dir, path) = fifo();
            let reader = open_fifo_reader_nonblocking(&path).unwrap();
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let expected: Vec<u8> = (0..200_003).map(|index| (index % 251) as u8).collect();
            let (start_tx, start_rx) = mpsc::channel();
            let worker = thread::spawn(move || {
                let mut reader = reader;
                let mut output = vec![0_u8; 200_003];
                let mut saw_data = false;
                let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
                start_tx.send(()).unwrap();
                let result = read_fifo_exact(&mut reader, &mut output, &CancellationToken::new(), &mut saw_data, Some(&mut stats));
                (result, output, stats)
            });
            start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            // A real producer may pause between any two reads. Keep the writer
            // attached during the pause, then deliver uneven and short writes.
            writer.write_all(&expected[..13]).unwrap();
            thread::sleep(Duration::from_millis(80));
            for chunk in expected[13..].chunks(2333) {
                writer.write_all(chunk).unwrap();
            }
            drop(writer);
            let (result, actual, stats) = worker.join().unwrap();
            result.unwrap();
            assert_eq!(actual, expected);
            assert_eq!(stats.no_writer_poll_count, 0);
            assert_eq!(stats.poll_sleep_ns, 0);
            assert!(stats.readiness_wait_count > 0);

            // Early close must not silently accept a partial buffer, even if
            // POLLIN and POLLHUP are simultaneously reported by the kernel.
            let (_dir, path) = fifo();
            let mut reader = open_fifo_reader_nonblocking(&path).unwrap();
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writer.write_all(&[1, 2, 3]).unwrap();
            drop(writer);
            let mut bytes = [0_u8; 5];
            let mut saw_data = false;
            let err = read_fifo_exact(&mut reader, &mut bytes, &CancellationToken::new(), &mut saw_data, None).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
            assert_eq!(&bytes[..3], &[1, 2, 3]);
        }

        #[test]
        fn cancel_during_connected_idle_transfer_and_invalid_descriptor() {
            let (_dir, path) = fifo();
            let mut reader = open_fifo_reader_nonblocking(&path).unwrap();
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writer.write_all(&[0xA5; 13]).unwrap();
            let cancel = CancellationToken::new();
            let reader_cancel = cancel.clone();
            let worker = thread::spawn(move || {
                let mut output = [0_u8; 4096];
                let mut saw_data = false;
                let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
                let result = read_fifo_exact(&mut reader, &mut output, &reader_cancel, &mut saw_data, Some(&mut stats));
                (result, stats)
            });
            thread::sleep(Duration::from_millis(115));
            cancel.cancel();
            let (result, stats) = worker.join().unwrap();
            drop(writer);
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert_eq!(stats.poll_sleep_ns, 0);
            assert!(stats.readiness_wait_count >= 1);
            assert!(stats.readiness_wait_count < 128, "cancellable poll loop must not spin");

            let err = wait_fifo_fd_readable(i32::MAX, &CancellationToken::new(), None).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc::EBADF));
        }

        fn observe_one(extra_byte: bool, short_writes: bool) -> (io::Result<OwnedTrackObservation>, crate::convert::pipeline::rg_profile::ObserverMetrics) {
            let (_dir, path) = fifo();
            let header = b"CUE-MIRROR-TEST";
            let samples: Vec<u8> = (0..44_100_i32)
                .map(|frame| ((frame as f64 * 0.019).sin() * 0.4) as f32)
                .flat_map(f32::to_le_bytes)
                .collect();
            let writer_path = path.clone();
            let worker = thread::spawn(move || {
                let mut writer = std::fs::OpenOptions::new().write(true).open(writer_path).unwrap();
                writer.write_all(header).unwrap();
                if short_writes {
                    for part in samples.chunks(173) { writer.write_all(part).unwrap(); }
                } else {
                    writer.write_all(&samples).unwrap();
                }
                if extra_byte { writer.write_all(b"X").unwrap(); }
            });
            let mut stats = crate::convert::pipeline::rg_profile::ObserverMetrics::default();
            let result = observe_cue_mirror(
                &path, "test-member".into(), header, 44_100, 1, 44_100,
                PcmMirrorEncoding::F32Le, MetricDemand::IntegratedOnly,
                METER_STORAGE_ALLOWANCE_BYTES, &CancellationToken::new(), Some(&mut stats),
            );
            worker.join().unwrap();
            (result, stats)
        }

        #[test]
        fn exact_fifo_eof_accepts_buffered_hangup_and_rejects_surplus() {
            // The same float PCM must reach identical native meters whether
            // transferred in many short writes or in large kernel-buffer writes.
            let (a, first) = observe_one(false, false);
            let (b, second) = observe_one(false, true);
            let a = a.unwrap();
            let b = b.unwrap();
            assert_eq!(a.summary, b.summary);
            assert_eq!(a.reporting_peak_linear.to_bits(), b.reporting_peak_linear.to_bits());
            assert_eq!(a.summary.real_frames, 44_100);
            assert_eq!(first.pcm_bytes, 44_100 * 4);
            assert_eq!(second.pcm_bytes, first.pcm_bytes);
            let (surplus, _) = observe_one(true, false);
            assert_eq!(surplus.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
}

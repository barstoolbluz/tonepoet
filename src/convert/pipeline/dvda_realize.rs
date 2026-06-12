#![forbid(unsafe_code)]

//! DVD-Audio track realization: ATS-relative AOB sector reads, MPEG-PS demux,
//! MLP extraction via ffmpeg and LPCM unpacking to `pcm_s32le` WAV.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use super::dvda_channel_layout::{
    layout_for_assignment_code, normalized_ffmpeg_channel_layout, source_group_order_label,
    DvdaChannelOrderPolicy,
};
use super::dvda_demux::{
    parse_private_stream_1_packets, record_private_stream_1_packets, DvdaDemuxError,
    DvdaDemuxStats, DvdaPs1Packet, DvdaSubstreamKind,
    MLP_EXTRA_HEADER_LENGTH, MLP_STREAM_ID, PCM_EXTRA_HEADER_LENGTH, PCM_STREAM_ID,
    DVD_SECTOR_SIZE,
};
use super::dvda_lpcm::{
    DvdAudioLpcmDecoder, LpcmDecodeStats, LpcmParams, LpcmStreamExpectation,
};
use super::dvda_mlp::{inspect_mlp_file, MlpStreamExpectation, MlpStreamInspection};
use super::errors::{ConvertError, ToolRunnerError};
use super::progress::{heartbeat, OperationProgressTracker};
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::track_executor::{run_tool_command_with_concurrency, ToolConcurrencyLimits};
use super::types::{
    DvdaAobFileRef, DvdaDownmixPolicy, DvdaIsoBackend, DvdaSectorAddressSpace,
    DvdaSectorRangeRef, DvdaVolumeSourceRef, PreparedTrack, SourceAudioDescriptor, StagingDir,
    TrackSourceRef,
};
use crate::tui::dvda::sector::AobSectorReader;
use crate::tui::dvda::{
    AobFileEntry, DirectoryDvdaVolume, DvdaFile, DvdaVolume, Iso9660DvdaVolume,
    IsoUdfDvdaVolume,
};

const PTS_PER_SECOND: u128 = 90_000;
const DVDA_READ_CHUNK_SECTORS: u32 = 256;
const DVDA_FFMPEG_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const DVDA_STRICT_DURATION_ENV: &str = "TONEPOET_DVDA_PHASE3_STRICT_DURATION";
const DVDA_NEAR_EXACT_DURATION_ENV: &str = "TONEPOET_DVDA_PHASE3_NEAR_EXACT_DURATION";
const DVDA_STRICT_DEMUX_PARSER_ENV: &str = "TONEPOET_DVDA_PHASE3_STRICT_DEMUX_PARSER";
const DVDA_SKIP_MLP_INSPECT_ENV: &str = "TONEPOET_DVDA_PHASE3_SKIP_MLP_INSPECT";
const DVDA_STRICT_MLP_INSPECT_ENV: &str = "TONEPOET_DVDA_PHASE3_STRICT_MLP_INSPECT";
const DVDA_CORPUS_STRICT_ENV: &str = "TONEPOET_DVDA_PHASE3_CORPUS_STRICT";
const DVDA_SKIP_PACKET_VALIDATE_ENV: &str = "TONEPOET_DVDA_PHASE3_SKIP_PACKET_VALIDATE";
const DVDA_STRICT_PACKET_IFO_ENV: &str = "TONEPOET_DVDA_PHASE3_STRICT_PACKET_IFO";
const DVDA_TEMP_STALE_SECONDS_ENV: &str = "TONEPOET_DVDA_TEMP_STALE_SECONDS";
const DVDA_LOCK_STALE_SECONDS_ENV: &str = "TONEPOET_DVDA_LOCK_STALE_SECONDS";
const DVDA_LPCM_CHANNEL_ORDER_ENV: &str = "TONEPOET_DVDA_LPCM_CHANNEL_ORDER";
const DVDA_DEFAULT_STALE_TEMP_SECONDS: u64 = 72 * 60 * 60;
const DVDA_DEFAULT_STALE_LOCK_SECONDS: u64 = 24 * 60 * 60;
const DVDA_OUTPUT_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER: &str =
    "pan=stereo|FL=0.500*FL+0.354*FC+0.177*LFE+0.250*BL|FR=0.500*FR+0.354*FC+0.177*LFE+0.250*BR";

pub(super) async fn realize_dvda_track(
    src: &TrackSourceRef,
    expected_audio: DvdaSourceAudioExpectation,
    mut audio_policy: DvdaRealizationAudioPolicy,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<PathBuf, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    let track = DvdaTrackRealizeInput::try_from_source(src, expected_audio, audio_policy.downmix_policy)?;
    if audio_policy.downmix_policy != track.dvda_downmix_policy {
        audio_policy.downmix_policy = track.dvda_downmix_policy;
    }
    let realized_dir = staging.root.join("realized-dvda-tracks");
    fs::create_dir_all(&realized_dir)?;
    let out_path = realized_dir.join(track.output_file_name());

    recover_stale_dvda_temp_artifacts(&realized_dir)?;
    recover_stale_dvda_temp_artifacts(&staging.root.join("dvda-elementary-audio"))?;

    let _output_lock = acquire_dvda_output_lock(&out_path, cancel).await?;

    let cache_source_wav_expectation = track.source_wav_expectation();
    let base_wav_expectation = track.wav_expectation();
    if base_wav_expectation.can_validate_existing_cache() {
        if let Some(probe) = dvda_wav_is_ready(&out_path, base_wav_expectation, runner, cancel, tool_concurrency_limits).await? {
            let realization_metadata = DvdaRealizationMetadata::from_track_cache_hit(&track);
            log_dvda_audio_format_record(&out_path, &track, cache_source_wav_expectation, base_wav_expectation, &probe, &audio_policy, realization_metadata, "cache-hit");
            write_dvda_audio_format_manifest(&out_path, &track, cache_source_wav_expectation, base_wav_expectation, &probe, &audio_policy, realization_metadata, "cache-hit")?;
            return Ok(out_path);
        }
    } else if out_path.exists() {
        log::info!(
            "DVD-Audio cached WAV {} will be regenerated because active multi-format stream facts are not known before demux",
            out_path.display()
        );
    }

    let staging_root = staging.root.clone();
    let track_for_extract = track.clone();
    let lpcm_channel_order_policy = audio_policy.lpcm_channel_order_policy;
    let cancel_for_extract = cancel.clone();

    let extracted = match progress_tracker {
        Some(tracker) => {
            heartbeat::run_with_heartbeat(
                async move {
                    tokio::task::spawn_blocking(move || {
                        extract_track_audio_payload(&track_for_extract, &staging_root, lpcm_channel_order_policy, &cancel_for_extract)
                    })
                    .await
                    .map_err(|err| ConvertError::Realize(format!("DVD-Audio extraction task failed: {err}")))?
                },
                tracker,
                "dvda-audio-extraction",
                "Extracting DVD-Audio elementary audio...",
                Duration::from_secs(5),
            )
            .await?
        }
        None => {
            tokio::task::spawn_blocking(move || {
                extract_track_audio_payload(&track_for_extract, &staging_root, lpcm_channel_order_policy, &cancel_for_extract)
            })
            .await
            .map_err(|err| ConvertError::Realize(format!("DVD-Audio extraction task failed: {err}")))??
        }
    };

    let source_wav_expectation = extracted.source_wav_expectation(&track);
    let final_wav_expectation = source_wav_expectation.with_downmix_policy(audio_policy.downmix_policy);
    let realization_metadata = extracted.realization_metadata(&track);

    let final_probe = match extracted {
        ExtractedDvdaAudio::Mlp { mlp_path, inspection, .. } => {
            let mut mlp_guard = DvdaTempPathGuard::new(mlp_path);
            if let Some(inspection) = &inspection {
                log::info!(
                    "DVD-Audio MLP payload ready for decode: {}, frames={}, major_sync_frames={}",
                    mlp_guard.path().display(),
                    inspection.frame_count,
                    inspection.major_sync_frame_count
                );
            }

            let mlp_source_channel_count = inspection
                .as_ref()
                .and_then(|inspection| inspection.first_major_sync)
                .map(|info| info.channel_count);
            let probe = decode_mlp_to_wav(
                mlp_guard.path(),
                &out_path,
                final_wav_expectation,
                audio_policy.downmix_policy,
                mlp_source_channel_count,
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await?;
            mlp_guard.remove_now()?;
            probe
        }
        ExtractedDvdaAudio::Lpcm {
            raw_path,
            params,
            stats,
            ..
        } => {
            let mut raw_guard = DvdaTempPathGuard::new(raw_path);
            log::info!(
                "DVD-Audio LPCM payload ready for WAV mux: {}, packets={}, payload_bytes={}, decoded_bytes={}, samples_per_channel={}, sample_rate={} Hz, channels={}, source_bits={}, channel_order_policy={}, source_order=[{}], output_order=[{}]",
                raw_guard.path().display(),
                stats.packets,
                stats.payload_bytes,
                stats.bytes_decoded,
                stats.samples_per_channel,
                params.sample_rate,
                params.channel_count,
                params.bit_depth,
                audio_policy.lpcm_channel_order_policy.as_str(),
                params.source_channel_order_label(),
                params.output_channel_order_label(audio_policy.lpcm_channel_order_policy)
            );

            let probe = mux_s32le_to_wav(
                raw_guard.path(),
                &out_path,
                final_wav_expectation,
                params.sample_rate,
                params.channel_count,
                Some(params.channel_assignment_code),
                audio_policy.lpcm_channel_order_policy,
                audio_policy.downmix_policy,
                Some(params.channel_count),
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await?;
            raw_guard.remove_now()?;
            probe
        }
    };

    log_dvda_audio_format_record(&out_path, &track, source_wav_expectation, final_wav_expectation, &final_probe, &audio_policy, realization_metadata, "fresh-decode");
    write_dvda_audio_format_manifest(&out_path, &track, source_wav_expectation, final_wav_expectation, &final_probe, &audio_policy, realization_metadata, "fresh-decode")?;
    Ok(out_path)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DvdaSourceAudioExpectation {
    pub(super) sample_rate: Option<u32>,
    pub(super) channel_count: Option<u32>,
    pub(super) bit_depth: Option<u32>,
    pub(super) group1_sample_rate: Option<u32>,
    pub(super) group2_sample_rate: Option<u32>,
    pub(super) group1_bit_depth: Option<u32>,
    pub(super) group2_bit_depth: Option<u32>,
    pub(super) group1_channel_count: Option<u32>,
    pub(super) group2_channel_count: Option<u32>,
    pub(super) channel_assignment_code: Option<u8>,
}

impl DvdaSourceAudioExpectation {
    #[must_use]
    pub(super) fn from_prepared_track_and_source(
        track: Option<&PreparedTrack>,
        src: &TrackSourceRef,
    ) -> Self {
        let prepared = track.map(Self::from_prepared_track).unwrap_or_default();
        let source_ref = Self::from_source_ref(src);
        if let (Some(track), Some(code)) = (track, source_ref.channel_assignment_code) {
            if let (Some(source_order), Some(layout)) = (
                source_audio_channel_order_label(&track.source_audio),
                layout_for_assignment_code(code),
            ) {
                if source_order != layout.order_label() {
                    log::warn!(
                        "DVD-Audio prepared source channel order [{}] differs from source-ref assignment {} order [{}]",
                        source_order,
                        code,
                        layout.order_label()
                    );
                }
            }
        }
        prepared.with_missing_from(source_ref)
    }

    #[must_use]
    pub(super) fn from_prepared_track(track: &PreparedTrack) -> Self {
        Self {
            sample_rate: track.scalar_sample_rate(),
            channel_count: channel_count_from_source_audio(&track.source_audio),
            bit_depth: track
                .source_audio
                .bit_depth
                .filter(|bits| *bits != 0)
                .or_else(|| track.bit_depth.filter(|bits| *bits != 0)),
            group1_sample_rate: source_audio_group_sample_rate(&track.source_audio, 1),
            group2_sample_rate: source_audio_group_sample_rate(&track.source_audio, 2),
            group1_bit_depth: source_audio_group_bit_depth(&track.source_audio, 1),
            group2_bit_depth: source_audio_group_bit_depth(&track.source_audio, 2),
            group1_channel_count: source_audio_group_channel_count(&track.source_audio, 1),
            group2_channel_count: source_audio_group_channel_count(&track.source_audio, 2),
            channel_assignment_code: None,
        }
    }

    #[must_use]
    pub(super) fn from_source_ref(src: &TrackSourceRef) -> Self {
        let TrackSourceRef::DvdaTrack {
            expected_sample_rate,
            expected_channel_count,
            expected_bit_depth,
            expected_group1_sample_rate,
            expected_group2_sample_rate,
            expected_group1_bit_depth,
            expected_group2_bit_depth,
            expected_group1_channel_count,
            expected_group2_channel_count,
            expected_channel_assignment_code,
            ..
        } = src
        else {
            return Self::default();
        };

        Self {
            sample_rate: *expected_sample_rate,
            channel_count: *expected_channel_count,
            bit_depth: *expected_bit_depth,
            group1_sample_rate: *expected_group1_sample_rate,
            group2_sample_rate: *expected_group2_sample_rate,
            group1_bit_depth: *expected_group1_bit_depth,
            group2_bit_depth: *expected_group2_bit_depth,
            group1_channel_count: *expected_group1_channel_count,
            group2_channel_count: *expected_group2_channel_count,
            channel_assignment_code: *expected_channel_assignment_code,
        }
    }

    #[must_use]
    fn with_missing_from(self, fallback: Self) -> Self {
        Self {
            sample_rate: self.sample_rate.or(fallback.sample_rate),
            channel_count: self.channel_count.or(fallback.channel_count),
            bit_depth: self.bit_depth.or(fallback.bit_depth),
            group1_sample_rate: self.group1_sample_rate.or(fallback.group1_sample_rate),
            group2_sample_rate: self.group2_sample_rate.or(fallback.group2_sample_rate),
            group1_bit_depth: self.group1_bit_depth.or(fallback.group1_bit_depth),
            group2_bit_depth: self.group2_bit_depth.or(fallback.group2_bit_depth),
            group1_channel_count: self.group1_channel_count.or(fallback.group1_channel_count),
            group2_channel_count: self.group2_channel_count.or(fallback.group2_channel_count),
            channel_assignment_code: self.channel_assignment_code.or(fallback.channel_assignment_code),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DvdaRealizationAudioPolicy {
    pub(super) final_encode_bit_depth_policy: String,
    pub(super) final_encode_bit_depth: Option<u32>,
    pub(super) downmix_policy: DvdaDownmixPolicy,
    pub(super) lpcm_channel_order_policy: DvdaChannelOrderPolicy,
}

impl DvdaRealizationAudioPolicy {
    #[must_use]
    pub(super) fn new(
        final_encode_bit_depth_policy: String,
        final_encode_bit_depth: Option<u32>,
        downmix_policy: DvdaDownmixPolicy,
    ) -> Self {
        Self {
            final_encode_bit_depth_policy,
            final_encode_bit_depth,
            downmix_policy,
            lpcm_channel_order_policy: DvdaChannelOrderPolicy::from_env_var(DVDA_LPCM_CHANNEL_ORDER_ENV),
        }
    }

    #[must_use]
    #[allow(dead_code)]
    pub(super) fn unknown() -> Self {
        Self {
            final_encode_bit_depth_policy: "unknown".to_string(),
            final_encode_bit_depth: None,
            downmix_policy: DvdaDownmixPolicy::Auto,
            lpcm_channel_order_policy: DvdaChannelOrderPolicy::from_env_var(DVDA_LPCM_CHANNEL_ORDER_ENV),
        }
    }
}

fn channel_count_from_source_audio(source_audio: &SourceAudioDescriptor) -> Option<u32> {
    if source_audio.channel_groups.is_empty() {
        return None;
    }

    let mut total = 0_u32;
    for group in &source_audio.channel_groups {
        let channels = group.channels?;
        if channels == 0 {
            return None;
        }
        total = total.checked_add(u32::from(channels))?;
    }

    (total > 0).then_some(total)
}

fn source_audio_group_sample_rate(source_audio: &SourceAudioDescriptor, group_nr: u8) -> Option<u32> {
    source_audio
        .channel_groups
        .iter()
        .find(|group| group.group_nr == group_nr)
        .and_then(|group| group.sample_rate)
        .filter(|value| *value != 0)
}

fn source_audio_group_bit_depth(source_audio: &SourceAudioDescriptor, group_nr: u8) -> Option<u32> {
    source_audio
        .channel_groups
        .iter()
        .find(|group| group.group_nr == group_nr)
        .and_then(|group| group.bit_depth)
        .filter(|value| *value != 0)
}

fn source_audio_group_channel_count(source_audio: &SourceAudioDescriptor, group_nr: u8) -> Option<u32> {
    source_audio
        .channel_groups
        .iter()
        .find(|group| group.group_nr == group_nr)
        .and_then(|group| group.channels)
        .map(u32::from)
        .filter(|value| *value != 0)
}


fn source_audio_channel_order_label(source_audio: &SourceAudioDescriptor) -> Option<String> {
    let group1 = source_audio
        .channel_groups
        .iter()
        .find(|group| group.group_nr == 1)
        .and_then(|group| group.assignment.as_deref());
    let group2 = source_audio
        .channel_groups
        .iter()
        .find(|group| group.group_nr == 2)
        .and_then(|group| group.assignment.as_deref());
    source_group_order_label(group1, group2)
}

#[derive(Clone, Debug)]
struct DvdaTrackRealizeInput {
    volume_source: DvdaVolumeSourceRef,
    sector_address_space: DvdaSectorAddressSpace,
    group_nr: u8,
    group_track_ordinal: u32,
    title_set_nr: Option<u8>,
    title_nr: Option<u8>,
    title_ordinal: Option<u8>,
    ats_track_nr: Option<u8>,
    samg_track_nr: Option<u8>,
    samg_ordinal: Option<u16>,
    first_pts: u32,
    len_in_pts: u32,
    track_type: Option<u8>,
    index_start: Option<u8>,
    title_table_offset: Option<u32>,
    title_len_in_pts: Option<u32>,
    title_track_count_declared: Option<u8>,
    title_index_count_declared: Option<u8>,
    audio_format_index: Option<u8>,
    downmix_matrix: Option<u8>,
    dvda_downmix_policy: DvdaDownmixPolicy,
    expected_sample_rate: Option<u32>,
    expected_channel_count: Option<u32>,
    expected_bit_depth: Option<u32>,
    expected_channel_assignment_code: Option<u8>,
    expected_group1_sample_rate: Option<u32>,
    expected_group2_sample_rate: Option<u32>,
    expected_group1_bit_depth: Option<u32>,
    expected_group2_bit_depth: Option<u32>,
    expected_group1_channel_count: Option<u32>,
    expected_group2_channel_count: Option<u32>,
    sector_ranges: Vec<DvdaSectorRangeRef>,
    aob_files: Vec<DvdaAobFileRef>,
}

impl DvdaTrackRealizeInput {
    fn try_from_source(
        src: &TrackSourceRef,
        expected_audio: DvdaSourceAudioExpectation,
        realized_downmix_policy: DvdaDownmixPolicy,
    ) -> Result<Self, ConvertError> {
        let TrackSourceRef::DvdaTrack {
            volume_source,
            sector_address_space,
            group_nr,
            group_track_ordinal,
            title_set_nr,
            title_nr,
            title_ordinal,
            ats_track_nr,
            samg_track_nr,
            samg_ordinal,
            first_pts,
            len_in_pts,
            track_type,
            index_start,
            title_table_offset,
            title_len_in_pts,
            title_track_count_declared,
            title_index_count_declared,
            audio_format_index,
            downmix_matrix,
            dvda_downmix_policy,
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
        } = src
        else {
            return Err(ConvertError::UnsupportedTrackSource);
        };

        if sector_ranges.is_empty() {
            return Err(ConvertError::TrackValidation(
                "DVD-Audio track has no sector ranges".to_string(),
            ));
        }
        if matches!(sector_address_space, DvdaSectorAddressSpace::AtsAobRelative { .. }) && aob_files.is_empty() {
            return Err(ConvertError::TrackValidation(
                "DVD-Audio ATS-relative track has no AOB file inventory".to_string(),
            ));
        }

        let source_ref_expectation = DvdaSourceAudioExpectation {
            sample_rate: *expected_sample_rate,
            channel_count: *expected_channel_count,
            bit_depth: *expected_bit_depth,
            group1_sample_rate: *expected_group1_sample_rate,
            group2_sample_rate: *expected_group2_sample_rate,
            group1_bit_depth: *expected_group1_bit_depth,
            group2_bit_depth: *expected_group2_bit_depth,
            group1_channel_count: *expected_group1_channel_count,
            group2_channel_count: *expected_group2_channel_count,
            channel_assignment_code: *expected_channel_assignment_code,
        };
        let expected_audio = expected_audio.with_missing_from(source_ref_expectation);
        let effective_downmix_policy = match realized_downmix_policy {
            DvdaDownmixPolicy::Auto => *dvda_downmix_policy,
            policy => policy,
        };

        Ok(Self {
            volume_source: volume_source.clone(),
            sector_address_space: *sector_address_space,
            group_nr: *group_nr,
            group_track_ordinal: *group_track_ordinal,
            title_set_nr: *title_set_nr,
            title_nr: *title_nr,
            title_ordinal: *title_ordinal,
            ats_track_nr: *ats_track_nr,
            samg_track_nr: *samg_track_nr,
            samg_ordinal: *samg_ordinal,
            first_pts: *first_pts,
            len_in_pts: *len_in_pts,
            track_type: *track_type,
            index_start: *index_start,
            title_table_offset: *title_table_offset,
            title_len_in_pts: *title_len_in_pts,
            title_track_count_declared: *title_track_count_declared,
            title_index_count_declared: *title_index_count_declared,
            audio_format_index: *audio_format_index,
            downmix_matrix: *downmix_matrix,
            dvda_downmix_policy: effective_downmix_policy,
            expected_sample_rate: expected_audio.sample_rate,
            expected_channel_count: expected_audio.channel_count,
            expected_bit_depth: expected_audio.bit_depth,
            expected_channel_assignment_code: expected_audio.channel_assignment_code.or(*expected_channel_assignment_code),
            expected_group1_sample_rate: expected_audio.group1_sample_rate,
            expected_group2_sample_rate: expected_audio.group2_sample_rate,
            expected_group1_bit_depth: expected_audio.group1_bit_depth,
            expected_group2_bit_depth: expected_audio.group2_bit_depth,
            expected_group1_channel_count: expected_audio.group1_channel_count,
            expected_group2_channel_count: expected_audio.group2_channel_count,
            sector_ranges: sector_ranges.clone(),
            aob_files: aob_files.clone(),
        })
    }

    fn output_file_name(&self) -> String {
        let ats = self
            .title_set_nr
            .map(|value| format!("ats{value:02}"))
            .unwrap_or_else(|| "ats_unknown".to_string());
        let hash = self.stable_identity_hash();
        format!(
            "dvda_{ats}_g{:02}_t{:03}_{hash:016x}.wav",
            self.group_nr, self.group_track_ordinal
        )
    }

    fn mlp_file_name(&self) -> String {
        let hash = self.stable_identity_hash();
        format!(
            "dvda_g{:02}_t{:03}_{hash:016x}.mlp",
            self.group_nr, self.group_track_ordinal
        )
    }

    fn lpcm_file_name(&self) -> String {
        let hash = self.stable_identity_hash();
        format!(
            "dvda_g{:02}_t{:03}_{hash:016x}.s32le",
            self.group_nr, self.group_track_ordinal
        )
    }

    fn source_wav_expectation(&self) -> DvdaWavExpectation {
        DvdaWavExpectation {
            len_in_pts: self.len_in_pts,
            sample_rate: self.expected_sample_rate,
            channel_count: self.expected_channel_count,
            source_bit_depth: self.expected_bit_depth,
            channel_assignment_code: self.expected_channel_assignment_code,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        }
    }

    fn wav_expectation(&self) -> DvdaWavExpectation {
        self.source_wav_expectation()
            .with_downmix_policy(self.dvda_downmix_policy)
    }

    fn mlp_expectation(&self) -> MlpStreamExpectation {
        // For an authored stereo presentation that reuses a multichannel MLP
        // carrier, the IFO/SAMG-facing channel count may be 2 even though the
        // MLP major sync is 5.1. Do not feed that presentation-channel count
        // into the carrier inspector as a hard expectation when an explicit
        // downmix policy is active; let the MLP major sync establish the source
        // channel count used by the downmix and manifest.
        MlpStreamExpectation {
            sample_rate: self.expected_sample_rate,
            channel_count: if self.dvda_downmix_policy.is_active() {
                None
            } else {
                self.expected_channel_count
            },
            bit_depth: self.expected_bit_depth,
            group1_sample_rate: self.expected_group1_sample_rate,
            group2_sample_rate: self.expected_group2_sample_rate,
            group1_bit_depth: self.expected_group1_bit_depth,
            group2_bit_depth: self.expected_group2_bit_depth,
        }
    }

    fn lpcm_expectation(&self) -> LpcmStreamExpectation {
        LpcmStreamExpectation {
            sample_rate: self.expected_sample_rate,
            channel_count: self.expected_channel_count,
            bit_depth: self.expected_bit_depth,
            group1_sample_rate: self.expected_group1_sample_rate,
            group2_sample_rate: self.expected_group2_sample_rate,
            group1_bit_depth: self.expected_group1_bit_depth,
            group2_bit_depth: self.expected_group2_bit_depth,
            group1_channel_count: self.expected_group1_channel_count,
            group2_channel_count: self.expected_group2_channel_count,
            channel_assignment_code: self.expected_channel_assignment_code,
        }
    }

    fn stable_identity_hash(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325_u64;
        hash_path(&mut hash, self.volume_source.original_container());
        hash_u8(&mut hash, self.group_nr);
        if let Some(title_nr) = self.title_nr {
            hash_u8(&mut hash, title_nr);
        }
        if let Some(ats_track_nr) = self.ats_track_nr {
            hash_u8(&mut hash, ats_track_nr);
        }
        if let Some(samg_ordinal) = self.samg_ordinal {
            hash_u32(&mut hash, u32::from(samg_ordinal));
        }
        hash_u32(&mut hash, self.group_track_ordinal);
        hash_u32(&mut hash, self.first_pts);
        hash_u32(&mut hash, self.len_in_pts);
        if self.dvda_downmix_policy.is_active() {
            hash_u8(&mut hash, self.dvda_downmix_policy.cache_tag());
        }
        for range in &self.sector_ranges {
            hash_u8(&mut hash, range.index_nr);
            hash_u32(&mut hash, range.first);
            hash_u32(&mut hash, range.last);
        }
        hash
    }
}

#[derive(Debug)]
enum ExtractedDvdaAudio {
    Mlp {
        mlp_path: PathBuf,
        inspection: Option<MlpStreamInspection>,
        packet_stats: DvdaDemuxStats,
    },
    Lpcm {
        raw_path: PathBuf,
        params: LpcmParams,
        stats: LpcmDecodeStats,
        packet_stats: DvdaDemuxStats,
    },
}

impl ExtractedDvdaAudio {
    fn source_wav_expectation(&self, track: &DvdaTrackRealizeInput) -> DvdaWavExpectation {
        let base = track.source_wav_expectation();
        match self {
            Self::Mlp { inspection, .. } => {
                let Some(info) = inspection.as_ref().and_then(|inspection| inspection.first_major_sync) else {
                    return base;
                };
                base.with_stream_facts(StreamDerivedAudioFacts {
                    sample_rate: Some(info.group1_sample_rate),
                    channel_count: Some(info.channel_count),
                    bit_depth: Some(info.group2_bits.max(info.group1_bits)),
                    channel_assignment_code: u8::try_from(info.channel_arrangement).ok(),
                    evidence_source: "MLP major-sync",
                })
            }
            Self::Lpcm { params, stats, .. } => base
                .with_stream_facts(StreamDerivedAudioFacts {
                    sample_rate: Some(params.sample_rate),
                    channel_count: Some(params.channel_count),
                    bit_depth: Some(params.bit_depth),
                    channel_assignment_code: Some(params.channel_assignment_code),
                    evidence_source: "LPCM packet sub-header",
                })
                .with_channel_order_policy(stats.channel_order_policy),
        }
    }

    fn realization_metadata(&self, track: &DvdaTrackRealizeInput) -> DvdaRealizationMetadata {
        match self {
            Self::Mlp { inspection, packet_stats, .. } => {
                DvdaRealizationMetadata::from_mlp_track_and_packet_stats(track, packet_stats, inspection.as_ref())
            }
            Self::Lpcm { stats, packet_stats, .. } => {
                DvdaRealizationMetadata::from_lpcm_track_and_packet_stats(track, packet_stats, stats)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamDerivedAudioFacts {
    sample_rate: Option<u32>,
    channel_count: Option<u32>,
    bit_depth: Option<u32>,
    channel_assignment_code: Option<u8>,
    evidence_source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedElementaryStreamKind {
    Mlp,
    Lpcm,
}

impl ExpectedElementaryStreamKind {
    const fn stream_id(self) -> u8 {
        match self {
            Self::Mlp => MLP_STREAM_ID,
            Self::Lpcm => PCM_STREAM_ID,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Mlp => "MLP",
            Self::Lpcm => "LPCM",
        }
    }
}



fn classify_sector_elementary_stream(
    packets: &[DvdaPs1Packet<'_>],
    current_track_kind: Option<ExpectedElementaryStreamKind>,
    logical_sector: u32,
) -> Result<Option<ExpectedElementaryStreamKind>, DvdaDemuxError> {
    let strict_parser = strict_demux_parser_enabled();
    let mut sector_kind: Option<ExpectedElementaryStreamKind> = None;
    for packet in packets {
        let packet_kind = match packet.sub_header.kind() {
            DvdaSubstreamKind::Mlp => ExpectedElementaryStreamKind::Mlp,
            DvdaSubstreamKind::Pcm => ExpectedElementaryStreamKind::Lpcm,
            DvdaSubstreamKind::Unknown(stream_id) => {
                if strict_parser {
                    return Err(DvdaDemuxError::UnexpectedSubstream { stream_id });
                }
                log::warn!(
                    "DVD-Audio demux parser skipped unknown Private Stream 1 substream 0x{stream_id:02X} at sector {logical_sector}; set {DVDA_STRICT_DEMUX_PARSER_ENV}=1 to reject this"
                );
                continue;
            }
        };

        if let Some(existing) = sector_kind {
            if existing != packet_kind {
                return Err(DvdaDemuxError::PacketHandler(format!(
                    "sector {logical_sector} contains both {} and {} Private Stream 1 packets",
                    existing.label(),
                    packet_kind.label()
                )));
            }
        } else {
            sector_kind = Some(packet_kind);
        }
    }

    if let (Some(existing), Some(sector_kind)) = (current_track_kind, sector_kind) {
        if existing != sector_kind {
            return Err(DvdaDemuxError::PacketHandler(format!(
                "track contains both {} and {} Private Stream 1 packets; first mismatch at sector {logical_sector}",
                existing.label(),
                sector_kind.label()
            )));
        }
    }

    Ok(sector_kind)
}

fn strict_demux_parser_enabled() -> bool {
    std::env::var(DVDA_STRICT_DEMUX_PARSER_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
}

fn write_mlp_sector_payload<W: Write>(
    packets: &[DvdaPs1Packet<'_>],
    writer: &mut W,
) -> Result<(), DvdaDemuxError> {
    let mut pending = Vec::new();
    for packet in packets {
        if matches!(packet.sub_header.kind(), DvdaSubstreamKind::Mlp) {
            pending.extend_from_slice(packet.payload);
        }
    }
    writer.write_all(&pending).map_err(DvdaDemuxError::Write)
}

fn decode_lpcm_sector_payload<W: Write>(
    packets: &[DvdaPs1Packet<'_>],
    decoder: &mut DvdAudioLpcmDecoder,
    writer: &mut W,
) -> Result<(), DvdaDemuxError> {
    let mut pending = Vec::new();
    for packet in packets {
        if !matches!(packet.sub_header.kind(), DvdaSubstreamKind::Pcm) {
            continue;
        }
        let pcm_header = packet.sub_header.pcm.ok_or_else(|| {
            DvdaDemuxError::PacketHandler(
                "LPCM packet had no parsed DVD-Audio PCM sub-header".to_string(),
            )
        })?;
        decoder
            .decode_packet(pcm_header, packet.payload, &mut pending)
            .map_err(|err| DvdaDemuxError::PacketHandler(err.to_string()))?;
    }
    writer.write_all(&pending).map_err(DvdaDemuxError::Write)
}

enum TrackSectorReader<'a> {
    AtsRelative(AobSectorReader<'a, RealizeDvdaVolume>),
    DiscAbsoluteIso {
        file: File,
        file_len: u64,
        source: PathBuf,
    },
}

impl<'a> TrackSectorReader<'a> {
    fn new(
        track: &DvdaTrackRealizeInput,
        volume: &'a RealizeDvdaVolume,
        aob_entries: &'a [AobFileEntry],
    ) -> Result<Self, ConvertError> {
        match track.sector_address_space {
            DvdaSectorAddressSpace::AtsAobRelative { .. } => {
                if aob_entries.is_empty() {
                    return Err(ConvertError::TrackValidation(
                        "DVD-Audio ATS-relative track has no AOB file inventory".to_string(),
                    ));
                }
                Ok(Self::AtsRelative(AobSectorReader::new(volume, aob_entries)))
            }
            DvdaSectorAddressSpace::DiscAbsolute { .. } | DvdaSectorAddressSpace::SamgAbsolute => {
                Self::open_disc_absolute_iso(&track.volume_source)
            }
        }
    }

    fn open_disc_absolute_iso(source_ref: &DvdaVolumeSourceRef) -> Result<Self, ConvertError> {
        let source = match source_ref {
            DvdaVolumeSourceRef::Iso { path, .. } => path.clone(),
            DvdaVolumeSourceRef::StagedAudioTs { original, .. } if original.is_file() => original.clone(),
            DvdaVolumeSourceRef::StagedAudioTs { original, .. } => {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Audio disc-absolute sector reads require the original ISO image; staged source original {} is not a readable file",
                    original.display()
                )));
            }
            DvdaVolumeSourceRef::Directory { root } => {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Audio disc-absolute sector reads require an ISO image because directory copies do not preserve disc logical sector addresses: {}",
                    root.display()
                )));
            }
        };

        let file = File::open(&source).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to open DVD-Audio ISO for disc-absolute sector reads {}: {err}",
                source.display()
            ))
        })?;
        let file_len = file.metadata().map_err(|err| {
            ConvertError::Realize(format!(
                "failed to stat DVD-Audio ISO for disc-absolute sector reads {}: {err}",
                source.display()
            ))
        })?.len();

        Ok(Self::DiscAbsoluteIso { file, file_len, source })
    }

    fn read_blocks_into(
        &mut self,
        block_first: u32,
        block_count: u32,
        out: &mut [u8],
    ) -> Result<usize, ConvertError> {
        match self {
            Self::AtsRelative(reader) => reader
                .read_blocks_into(block_first, block_count, out)
                .map_err(|err| ConvertError::Realize(err.to_string())),
            Self::DiscAbsoluteIso { file, file_len, source } => {
                read_disc_absolute_blocks_from_iso(file, *file_len, source, block_first, block_count, out)
            }
        }
    }
}

fn read_disc_absolute_blocks_from_iso(
    file: &mut File,
    file_len: u64,
    source: &Path,
    block_first: u32,
    block_count: u32,
    out: &mut [u8],
) -> Result<usize, ConvertError> {
    if block_count == 0 {
        return Ok(0);
    }

    let required = (block_count as usize)
        .checked_mul(DVD_SECTOR_SIZE)
        .ok_or_else(|| {
            ConvertError::TrackValidation(
                "DVD-Audio disc-absolute sector read byte count overflowed usize".to_string(),
            )
        })?;
    if out.len() < required {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio disc-absolute sector destination buffer too small: need {required} bytes, have {} bytes",
            out.len()
        )));
    }

    let offset = u64::from(block_first)
        .checked_mul(DVD_SECTOR_SIZE as u64)
        .ok_or_else(|| {
            ConvertError::TrackValidation(format!(
                "DVD-Audio disc-absolute sector offset overflow for sector {block_first}"
            ))
        })?;
    let end = offset.checked_add(required as u64).ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio disc-absolute sector end offset overflow for sector {block_first}, sectors {block_count}"
        ))
    })?;
    if end > file_len {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio disc-absolute sector read exceeds ISO length for {}: sector {block_first}, sectors {block_count}, byte range {offset}..{end}, ISO bytes {file_len}",
            source.display()
        )));
    }

    file.seek(SeekFrom::Start(offset)).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to seek DVD-Audio ISO {} for disc-absolute sector {block_first}: {err}",
            source.display()
        ))
    })?;
    file.read_exact(&mut out[..required]).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to read DVD-Audio ISO {} for disc-absolute sector {block_first}, sectors {block_count}: {err}",
            source.display()
        ))
    })?;

    Ok(required)
}

fn extract_track_audio_payload(
    track: &DvdaTrackRealizeInput,
    staging_root: &Path,
    lpcm_channel_order_policy: DvdaChannelOrderPolicy,
    cancel: &CancellationToken,
) -> Result<ExtractedDvdaAudio, ConvertError> {
    let volume = open_realize_volume(&track.volume_source)?;
    let aob_entries = to_aob_entries(&track.aob_files);
    let mut sector_reader = TrackSectorReader::new(track, &volume, &aob_entries)?;

    let elementary_dir = staging_root.join("dvda-elementary-audio");
    fs::create_dir_all(&elementary_dir)?;
    let mut mlp_guard: Option<DvdaTempPathGuard> = None;
    let mut raw_guard: Option<DvdaTempPathGuard> = None;

    let mut mlp_writer: Option<BufWriter<File>> = None;
    let mut lpcm_writer: Option<BufWriter<File>> = None;
    let mut lpcm_decoder: Option<DvdAudioLpcmDecoder> = None;
    let mut stream_kind: Option<ExpectedElementaryStreamKind> = None;
    let mut stats = DvdaDemuxStats::default();
    let mut sector_buf = vec![0_u8; DVD_SECTOR_SIZE * DVDA_READ_CHUNK_SECTORS as usize];

    for range in &track.sector_ranges {
        if range.last < range.first {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio sector range {} has last sector {} before first sector {}",
                range.index_nr, range.last, range.first
            )));
        }

        let mut next_sector = range.first;
        let mut sectors_remaining = range.block_count();
        while sectors_remaining > 0 {
            if cancel.is_cancelled() {
                return Err(ConvertError::Realize("cancelled".to_string()));
            }

            let chunk_sectors = sectors_remaining.min(DVDA_READ_CHUNK_SECTORS);
            let chunk_bytes = chunk_sectors as usize * DVD_SECTOR_SIZE;
            let bytes_read = sector_reader
                .read_blocks_into(next_sector, chunk_sectors, &mut sector_buf[..chunk_bytes])
                .map_err(|err| {
                    ConvertError::Realize(format!(
                        "DVD-Audio sector read failed in {:?} range {} ({}..={}) at sector {next_sector} for {} sectors: {err}",
                        track.sector_address_space, range.index_nr, range.first, range.last, chunk_sectors
                    ))
                })?;
            if bytes_read != chunk_bytes {
                return Err(ConvertError::Realize(format!(
                    "DVD-Audio sector short read in {:?} range {} ({}..={}) at sector {next_sector}: requested {} bytes ({} sectors), read {} bytes",
                    track.sector_address_space, range.index_nr, range.first, range.last, chunk_bytes, chunk_sectors, bytes_read
                )));
            }

            for (idx, sector) in sector_buf[..bytes_read].chunks_exact(DVD_SECTOR_SIZE).enumerate() {
                let logical_sector = next_sector + idx as u32;
                let packets = parse_private_stream_1_packets(sector).map_err(|err| {
                    ConvertError::Realize(format!(
                        "DVD-Audio MPEG-PS demux failed at logical sector {logical_sector}: {err}"
                    ))
                })?;
                let sector_kind = classify_sector_elementary_stream(&packets, stream_kind, logical_sector)
                    .map_err(|err| {
                        ConvertError::Realize(format!(
                            "DVD-Audio MPEG-PS semantic validation failed at logical sector {logical_sector}: {err}"
                        ))
                    })?;

                match sector_kind {
                    Some(ExpectedElementaryStreamKind::Mlp) => {
                        if mlp_writer.is_none() {
                            let (path, file) = create_unique_dvda_temp_file(
                                &elementary_dir,
                                &format!(".{}.", track.mlp_file_name()),
                                ".tmp.mlp",
                            )
                            .map_err(|err| ConvertError::Realize(format!(
                                "failed to create DVD-Audio MLP temp file before sector {logical_sector}: {err}"
                            )))?;
                            mlp_guard = Some(DvdaTempPathGuard::new(path));
                            mlp_writer = Some(BufWriter::new(file));
                        }
                        let writer = mlp_writer.as_mut().expect("MLP writer initialized");
                        write_mlp_sector_payload(&packets, writer).map_err(|err| {
                            ConvertError::Realize(format!(
                                "DVD-Audio MLP sector-local commit failed at logical sector {logical_sector}: {err}"
                            ))
                        })?;
                        stream_kind = Some(ExpectedElementaryStreamKind::Mlp);
                    }
                    Some(ExpectedElementaryStreamKind::Lpcm) => {
                        if lpcm_writer.is_none() {
                            let (path, file) = create_unique_dvda_temp_file(
                                &elementary_dir,
                                &format!(".{}.", track.lpcm_file_name()),
                                ".tmp.s32le",
                            )
                            .map_err(|err| ConvertError::Realize(format!(
                                "failed to create DVD-Audio LPCM temp file before sector {logical_sector}: {err}"
                            )))?;
                            raw_guard = Some(DvdaTempPathGuard::new(path));
                            lpcm_writer = Some(BufWriter::new(file));
                        }
                        if lpcm_decoder.is_none() {
                            lpcm_decoder = Some(
                                DvdAudioLpcmDecoder::new(track.lpcm_expectation())
                                    .with_channel_order_policy(lpcm_channel_order_policy),
                            );
                        }
                        let writer = lpcm_writer.as_mut().expect("LPCM writer initialized");
                        let decoder = lpcm_decoder.as_mut().expect("LPCM decoder initialized");
                        decode_lpcm_sector_payload(&packets, decoder, writer).map_err(|err| {
                            ConvertError::Realize(format!(
                                "DVD-Audio LPCM sector-local commit failed at logical sector {logical_sector}: {err}"
                            ))
                        })?;
                        stream_kind = Some(ExpectedElementaryStreamKind::Lpcm);
                    }
                    None => {}
                }

                record_private_stream_1_packets(&mut stats, &packets);
            }

            next_sector = next_sector.saturating_add(chunk_sectors);
            sectors_remaining -= chunk_sectors;
        }
    }

    match stream_kind {
        Some(ExpectedElementaryStreamKind::Mlp) => {
            if let Some(writer) = mlp_writer.as_mut() {
                writer.flush()?;
            }
            drop(mlp_writer);
            drop(lpcm_writer);
            if let Some(mut guard) = raw_guard.take() {
                guard.remove_now()?;
            }
            let mut mlp_guard = mlp_guard.ok_or_else(|| {
                ConvertError::TrackValidation("DVD-Audio MLP packets were seen but no elementary temp file was created".to_string())
            })?;
            let mlp_path = mlp_guard.path().to_path_buf();

            if stats.mlp_payload_bytes == 0 {
                return Err(ConvertError::TrackValidation(
                    "DVD-Audio demux produced no MLP payload bytes".to_string(),
                ));
            }

            validate_packet_stream_expectations(&stats, track, ExpectedElementaryStreamKind::Mlp)?;

            let inspection = inspect_extracted_mlp_payload(&mlp_path, track)?;
            mlp_guard.disarm();

            Ok(ExtractedDvdaAudio::Mlp {
                mlp_path,
                inspection,
                packet_stats: stats,
            })
        }
        Some(ExpectedElementaryStreamKind::Lpcm) => {
            if let Some(writer) = lpcm_writer.as_mut() {
                writer.flush()?;
            }
            drop(lpcm_writer);
            drop(mlp_writer);
            if let Some(mut guard) = mlp_guard.take() {
                guard.remove_now()?;
            }
            let mut raw_guard = raw_guard.ok_or_else(|| {
                ConvertError::TrackValidation("DVD-Audio LPCM packets were seen but no raw PCM temp file was created".to_string())
            })?;
            let raw_path = raw_guard.path().to_path_buf();

            if stats.pcm_payload_bytes == 0 {
                return Err(ConvertError::TrackValidation(
                    "DVD-Audio demux produced no LPCM payload bytes".to_string(),
                ));
            }

            validate_packet_stream_expectations(&stats, track, ExpectedElementaryStreamKind::Lpcm)?;

            let decoder = lpcm_decoder.ok_or_else(|| {
                ConvertError::TrackValidation("DVD-Audio LPCM packets were seen but decoder was not initialized".to_string())
            })?;
            let params = decoder.params().ok_or_else(|| {
                ConvertError::TrackValidation("DVD-Audio LPCM packets were seen but stream parameters were not resolved".to_string())
            })?;
            let lpcm_stats = decoder.finish().map_err(|err| {
                ConvertError::TrackValidation(format!("DVD-Audio LPCM unpacking failed: {err}"))
            })?;
            validate_lpcm_ifo_cross_checks(&stats, &lpcm_stats, params, track)?;
            raw_guard.disarm();

            Ok(ExtractedDvdaAudio::Lpcm {
                raw_path,
                params,
                stats: lpcm_stats,
                packet_stats: stats,
            })
        }
        None => Err(ConvertError::TrackValidation(
            "DVD-Audio demux found no MLP or LPCM Private Stream 1 packets".to_string(),
        )),
    }
}

fn validate_packet_stream_expectations(
    stats: &DvdaDemuxStats,
    track: &DvdaTrackRealizeInput,
    expected_kind: ExpectedElementaryStreamKind,
) -> Result<(), ConvertError> {
    if std::env::var(DVDA_SKIP_PACKET_VALIDATE_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
    {
        log::warn!(
            "Skipping DVD-Audio packet/IFO validation for group {} track {} because {DVDA_SKIP_PACKET_VALIDATE_ENV} is set",
            track.group_nr,
            track.group_track_ordinal
        );
        return Ok(());
    }

    let strict = std::env::var(DVDA_STRICT_PACKET_IFO_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false);
    let Some(first_header) = stats.first_sub_header else {
        return Err(ConvertError::TrackValidation(
            "DVD-Audio demux produced payload bytes but no Private Stream 1 sub-header".to_string(),
        ));
    };

    let mut hard_findings = Vec::new();
    let mut metadata_findings = Vec::new();
    let mut soft_findings = Vec::new();

    if first_header.stream_id != expected_kind.stream_id() {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio packet stream id mismatch: expected {} 0x{:02X}, first packet carried 0x{:02X}",
            expected_kind.label(),
            expected_kind.stream_id(),
            first_header.stream_id
        )));
    }

    match expected_kind {
        ExpectedElementaryStreamKind::Mlp => {
            if stats.pcm_packets != 0 {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Audio packet stream contains {} LPCM packets in an MLP realization path",
                    stats.pcm_packets
                )));
            }
            if stats.nonstandard_mlp_extra_header_packets != 0 {
                metadata_findings.push(format!(
                    "{} MLP packets used extra_header_length other than the DVD-Audio MLP value {}",
                    stats.nonstandard_mlp_extra_header_packets,
                    MLP_EXTRA_HEADER_LENGTH
                ));
            }
        }
        ExpectedElementaryStreamKind::Lpcm => {
            if stats.mlp_packets != 0 {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Audio packet stream contains {} MLP packets in an LPCM realization path",
                    stats.mlp_packets
                )));
            }
            if stats.nonstandard_pcm_extra_header_packets != 0 {
                metadata_findings.push(format!(
                    "{} LPCM packets used extra_header_length other than the DVD-Audio LPCM value {}",
                    stats.nonstandard_pcm_extra_header_packets,
                    PCM_EXTRA_HEADER_LENGTH
                ));
            }
            if stats.pcm_format_change_count != 0 {
                hard_findings.push(format!(
                    "DVD-Audio LPCM packet format changed {} times across the track",
                    stats.pcm_format_change_count
                ));
            }
        }
    }

    if stats.extra_header_length_change_count != 0 {
        metadata_findings.push(format!(
            "DVD-Audio packet extra_header_length changed {} times across the track",
            stats.extra_header_length_change_count
        ));
    }
    if stats.cci_change_count != 0 {
        metadata_findings.push(format!(
            "DVD-Audio packet CCI changed {} times across the track",
            stats.cci_change_count
        ));
    }
    if stats.cyclic_discontinuity_count != 0 {
        soft_findings.push(format!(
            "DVD-Audio packet cyclic counter had {} discontinuities",
            stats.cyclic_discontinuity_count
        ));
    }

    if let (Some(track_type), Some(audio_format_index)) = (track.track_type, track.audio_format_index) {
        let candidate_index = track_type & 0x07;
        if candidate_index != audio_format_index {
            hard_findings.push(format!(
                "IFO track_type low bits suggest audio-format index {}, but Phase 2 selected audio-format index {}",
                candidate_index, audio_format_index
            ));
        }
    }

    log::info!(
        "DVD-Audio packet validation for group {} track {}: expected_kind={}, private_stream_1_packets={}, mlp_packets={}, pcm_packets={}, mlp_payload_bytes={}, pcm_payload_bytes={}, first_stream_id=0x{:02X}, first_cci={:?}, audio_format_index={:?}, track_type={:?}, downmix_matrix={:?}, dvda_downmix_policy={}, channel_assignment_code={:?}, channel_layout={}, group1={{rate={:?}, bits={:?}, channels={:?}}}, group2={{rate={:?}, bits={:?}, channels={:?}}}",
        track.group_nr,
        track.group_track_ordinal,
        expected_kind.label(),
        stats.private_stream_1_packets,
        stats.mlp_packets,
        stats.pcm_packets,
        stats.mlp_payload_bytes,
        stats.pcm_payload_bytes,
        first_header.stream_id,
        first_header.cci,
        track.audio_format_index,
        track.track_type,
        track.downmix_matrix,
        track.dvda_downmix_policy.as_str(),
        track.expected_channel_assignment_code,
        track.expected_channel_assignment_code
            .map(channel_layout_label_for_code)
            .unwrap_or_else(|| "unknown".to_string()),
        track.expected_group1_sample_rate,
        track.expected_group1_bit_depth,
        track.expected_group1_channel_count,
        track.expected_group2_sample_rate,
        track.expected_group2_bit_depth,
        track.expected_group2_channel_count
    );


    if let Some(matrix) = track.downmix_matrix {
        log::info!(
            "DVD-Audio track carries IFO downmix matrix {}; realization policy is {} ({})",
            matrix,
            track.dvda_downmix_policy.as_str(),
            track.dvda_downmix_policy.behavior()
        );
    }
    for finding in &soft_findings {
        log::warn!(
            "DVD-Audio packet validation advisory for group {} track {}: {}",
            track.group_nr,
            track.group_track_ordinal,
            finding
        );
    }
    for finding in &metadata_findings {
        log::warn!(
            "DVD-Audio packet metadata advisory for group {} track {}: {}",
            track.group_nr,
            track.group_track_ordinal,
            finding
        );
    }

    if !hard_findings.is_empty() {
        for finding in &hard_findings {
            log::error!(
                "DVD-Audio packet/IFO validation error for group {} track {}: {}",
                track.group_nr,
                track.group_track_ordinal,
                finding
            );
        }
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio packet/IFO audio-fact validation failed: {}",
            hard_findings.join("; ")
        )));
    }

    if strict && !metadata_findings.is_empty() {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio packet/IFO metadata validation failed in strict mode: {}",
            metadata_findings.join("; ")
        )));
    }

    Ok(())
}


fn channel_layout_label_for_code(code: u8) -> String {
    layout_for_assignment_code(code)
        .map(|layout| layout.group_label())
        .unwrap_or_else(|| format!("unsupported channel-assignment code {code}"))
}

fn compare_channel_assignment_layouts(
    expected_code: u8,
    actual_code: u8,
    expected_label: &str,
    actual_label: &str,
) -> Option<String> {
    let expected = layout_for_assignment_code(expected_code)?;
    let actual = layout_for_assignment_code(actual_code)?;
    if expected.order_label() == actual.order_label()
        && expected.group1 == actual.group1
        && expected.group2 == actual.group2
    {
        None
    } else {
        Some(format!(
            "{expected_label} {} ({}) differs from {actual_label} {} ({})",
            expected_code,
            expected.group_label(),
            actual_code,
            actual.group_label()
        ))
    }
}

fn validate_lpcm_ifo_cross_checks(
    demux_stats: &DvdaDemuxStats,
    decode_stats: &LpcmDecodeStats,
    params: LpcmParams,
    track: &DvdaTrackRealizeInput,
) -> Result<(), ConvertError> {
    if std::env::var(DVDA_SKIP_PACKET_VALIDATE_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let mut findings = Vec::new();

    if let Some(expected_rate) = track.expected_sample_rate {
        if params.sample_rate != expected_rate {
            findings.push(format!(
                "IFO sample rate {expected_rate} Hz differs from decoded LPCM sample rate {} Hz",
                params.sample_rate
            ));
        }
    }
    if let Some(expected_channels) = track.expected_channel_count {
        if params.channel_count != expected_channels {
            findings.push(format!(
                "IFO channel count {expected_channels} differs from decoded LPCM channel count {}",
                params.channel_count
            ));
        }
    }
    if let Some(expected_bits) = track.expected_bit_depth {
        if params.bit_depth != expected_bits {
            findings.push(format!(
                "IFO bit depth {expected_bits} differs from decoded LPCM bit depth {}",
                params.bit_depth
            ));
        }
    }
    if let (Some(expected_assignment), Some(header)) = (
        track.expected_channel_assignment_code,
        demux_stats.first_pcm_sub_header,
    ) {
        if let Some(finding) = compare_channel_assignment_layouts(
            expected_assignment,
            header.channel_assignment,
            "IFO channel layout",
            "LPCM packet channel layout",
        ) {
            findings.push(finding);
        }
    }

    log::info!(
        "DVD-Audio LPCM validation for group {} track {}: packets={}, payload_bytes={}, decoded_bytes={}, samples_per_channel={}, sample_rate={} Hz, channels={}, source_bits={}, channel_assignment={}, channel_order=[{}], first_audio_frame_pointer={:?}, group2_blocks_read={}, group2_blocks_repeated={}",
        track.group_nr,
        track.group_track_ordinal,
        decode_stats.packets,
        decode_stats.payload_bytes,
        decode_stats.bytes_decoded,
        decode_stats.samples_per_channel,
        params.sample_rate,
        params.channel_count,
        params.bit_depth,
        params.channel_assignment_code,
        params.output_channel_order_label(decode_stats.channel_order_policy),
        decode_stats.first_audio_frame_pointer,
        decode_stats.group2_blocks_read,
        decode_stats.group2_blocks_repeated
    );

    if findings.is_empty() {
        return Ok(());
    }

    for finding in &findings {
        log::error!(
            "DVD-Audio LPCM/IFO validation error for group {} track {}: {}",
            track.group_nr,
            track.group_track_ordinal,
            finding
        );
    }

    Err(ConvertError::TrackValidation(format!(
        "DVD-Audio LPCM/IFO audio-fact validation failed: {}",
        findings.join("; ")
    )))
}

fn inspect_extracted_mlp_payload(
    path: &Path,
    track: &DvdaTrackRealizeInput,
) -> Result<Option<MlpStreamInspection>, ConvertError> {
    if std::env::var(DVDA_SKIP_MLP_INSPECT_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
    {
        log::warn!(
            "Skipping DVD-Audio MLP frame inspection for {} because {DVDA_SKIP_MLP_INSPECT_ENV} is set",
            path.display()
        );
        return Ok(None);
    }

    let strict = mlp_inspection_is_strict();
    let inspection = match inspect_mlp_file(path, track.mlp_expectation()) {
        Ok(inspection) => inspection,
        Err(err) if strict || err.is_audio_fact_mismatch() => {
            let mode = if strict { "strict mode" } else { "audio-fact validation" };
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio MLP frame inspection failed in {mode} for {}: {err}",
                path.display()
            )));
        }
        Err(err) => {
            log::warn!(
                "DVD-Audio MLP frame inspection advisory for {}: {err}; continuing with ffmpeg decode because strict MLP inspection is disabled",
                path.display()
            );
            return Ok(None);
        }
    };

    validate_mlp_ifo_cross_checks(path, &inspection, track)?;

    let facts = inspection.first_major_sync.map_or_else(
        || "major_sync=none".to_string(),
        |info| {
            format!(
                "major_sync=0x{:02X}, sample_rate={} Hz, channels={}, bit_depth={}, access_unit_size={}, substreams={}",
                info.stream_type,
                info.group1_sample_rate,
                info.channel_count,
                info.group1_bits,
                info.access_unit_size,
                info.num_substreams
            )
        },
    );
    log::info!(
        "DVD-Audio MLP frame inspection for {}: mode={}, payload_bytes={}, frames={}, major_sync_frames={}, min_frame_bytes={:?}, max_frame_bytes={:?}, {}",
        path.display(),
        if strict { "strict" } else { "advisory" },
        inspection.payload_bytes,
        inspection.frame_count,
        inspection.major_sync_frame_count,
        inspection.min_frame_bytes,
        inspection.max_frame_bytes,
        facts
    );

    Ok(Some(inspection))
}

fn mlp_inspection_is_strict() -> bool {
    // MLP access-unit inspection is valuable diagnostics, but ffmpeg remains
    // the decoder authority until strict inspection has a larger authored-disc
    // proof base. Corpus strict mode tightens duration/sample validation; it
    // does not promote this independent MLP parser to a hard gate.
    std::env::var(DVDA_STRICT_MLP_INSPECT_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
}


fn validate_mlp_ifo_cross_checks(
    path: &Path,
    inspection: &MlpStreamInspection,
    track: &DvdaTrackRealizeInput,
) -> Result<(), ConvertError> {
    if std::env::var(DVDA_SKIP_PACKET_VALIDATE_ENV)
        .map(|value| env_flag_is_enabled(&value))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let Some(info) = inspection.first_major_sync else {
        return Ok(());
    };

    let mut findings = Vec::new();
    if let Some(expected_assignment) = track.expected_channel_assignment_code.filter(|_| !track.dvda_downmix_policy.is_active()) {
        match u8::try_from(info.channel_arrangement) {
            Ok(actual_assignment) => {
                if let Some(finding) = compare_channel_assignment_layouts(
                    expected_assignment,
                    actual_assignment,
                    "IFO channel layout",
                    "MLP major-sync channel layout",
                ) {
                    findings.push(finding);
                }
            }
            Err(_) => findings.push(format!(
                "MLP major-sync channel_arrangement {} is outside the DVD-Audio MLP/PCM assignment table",
                info.channel_arrangement
            )),
        }
    }

    log::info!(
        "DVD-Audio MLP channel-layout validation for {} group {} track {}: channel_arrangement={}, layout={}, channels={}",
        path.display(),
        track.group_nr,
        track.group_track_ordinal,
        info.channel_arrangement,
        u8::try_from(info.channel_arrangement)
            .ok()
            .map(channel_layout_label_for_code)
            .unwrap_or_else(|| "unsupported".to_string()),
        info.channel_count
    );

    if findings.is_empty() {
        return Ok(());
    }

    for finding in &findings {
        log::error!(
            "DVD-Audio packet/MLP/IFO validation error for {} group {} track {}: {}",
            path.display(),
            track.group_nr,
            track.group_track_ordinal,
            finding
        );
    }

    Err(ConvertError::TrackValidation(format!(
        "DVD-Audio packet/MLP/IFO audio-fact validation failed: {}",
        findings.join("; ")
    )))
}

fn append_downmix_ffmpeg_args(
    args: &mut Vec<String>,
    policy: DvdaDownmixPolicy,
    source_channel_count: Option<u32>,
) -> Result<(), ConvertError> {
    match policy {
        DvdaDownmixPolicy::Auto | DvdaDownmixPolicy::None => Ok(()),
        DvdaDownmixPolicy::FooInputDvdaCompatible => {
            if let Some(channels) = source_channel_count {
                if channels != 6 {
                    return Err(ConvertError::TrackValidation(format!(
                        "foo_input_dvda-compatible DVD-Audio downmix requires a 6-channel 5.1 source, but source metadata reports {channels} channels"
                    )));
                }
            }
            args.push("-af".to_string());
            args.push(FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER.to_string());
            Ok(())
        }
        DvdaDownmixPolicy::FfmpegDefault => {
            args.push("-ac".to_string());
            args.push("2".to_string());
            Ok(())
        }
    }
}

async fn decode_mlp_to_wav(
    mlp_path: &Path,
    out_path: &Path,
    expectation: DvdaWavExpectation,
    downmix_policy: DvdaDownmixPolicy,
    source_channel_count: Option<u32>,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<DvdaWavProbe, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    let parent = out_path.parent().ok_or_else(|| {
        ConvertError::TrackValidation(format!("DVD-Audio output has no parent path: {}", out_path.display()))
    })?;
    fs::create_dir_all(parent)?;

    let (tmp_wav, tmp_file) = create_unique_dvda_temp_file(
        parent,
        &format!(".{}.", out_path.file_name().and_then(|name| name.to_str()).unwrap_or("dvda-output.wav")),
        ".tmp.wav",
    )?;
    let mut tmp_wav_guard = DvdaTempPathGuard::new(tmp_wav);
    drop(tmp_file);

    let mut args = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-f".into(),
        "mlp".into(),
        "-i".into(),
        mlp_path.to_string_lossy().into_owned(),
        "-map".into(),
        "0:a:0".into(),
    ];
    append_downmix_ffmpeg_args(&mut args, downmix_policy, source_channel_count)?;
    args.extend([
        "-c:a".into(),
        "pcm_s32le".into(),
        "-f".into(),
        "wav".into(),
        tmp_wav_guard.path().to_string_lossy().into_owned(),
    ]);

    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args,
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DVDA_FFMPEG_TIMEOUT,
    };

    match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => {}
        Err(ToolRunnerError::Cancelled { .. }) => {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        Err(err) => {
            return Err(ConvertError::Tool(err));
        }
    }

    let metadata = fs::metadata(tmp_wav_guard.path()).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "ffmpeg did not write DVD-Audio WAV output {}: {err}",
            tmp_wav_guard.path().display()
        ))
    })?;
    if metadata.len() == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "ffmpeg wrote an empty DVD-Audio WAV output: {}",
            tmp_wav_guard.path().display()
        )));
    }

    let probe = validate_dvda_wav(tmp_wav_guard.path(), expectation, runner, cancel, tool_concurrency_limits).await?;
    atomically_replace_dvda_output(tmp_wav_guard.path(), out_path)?;
    tmp_wav_guard.disarm();
    Ok(probe)
}

async fn mux_s32le_to_wav(
    raw_path: &Path,
    out_path: &Path,
    expectation: DvdaWavExpectation,
    sample_rate: u32,
    channel_count: u32,
    channel_assignment_code: Option<u8>,
    lpcm_channel_order_policy: DvdaChannelOrderPolicy,
    downmix_policy: DvdaDownmixPolicy,
    source_channel_count: Option<u32>,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<DvdaWavProbe, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    let parent = out_path.parent().ok_or_else(|| {
        ConvertError::TrackValidation(format!("DVD-Audio output has no parent path: {}", out_path.display()))
    })?;
    fs::create_dir_all(parent)?;

    let (tmp_wav, tmp_file) = create_unique_dvda_temp_file(
        parent,
        &format!(".{}.", out_path.file_name().and_then(|name| name.to_str()).unwrap_or("dvda-output.wav")),
        ".tmp.wav",
    )?;
    let mut tmp_wav_guard = DvdaTempPathGuard::new(tmp_wav);
    drop(tmp_file);

    let mut args = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-f".into(),
        "s32le".into(),
        "-ar".into(),
        sample_rate.to_string(),
        "-ac".into(),
        channel_count.to_string(),
    ];
    if let Some(layout_name) = channel_assignment_code
        .and_then(layout_for_assignment_code)
        .and_then(|layout| layout.ffmpeg_input_layout_for_policy(lpcm_channel_order_policy))
    {
        args.push("-channel_layout".into());
        args.push(layout_name.into());
    } else if let Some(code) = channel_assignment_code {
        if let Some(layout) = layout_for_assignment_code(code) {
            log::warn!(
                "DVD-Audio LPCM WAV mux will omit channel_layout metadata for assignment {} under policy {} because output order [{}] has no safe standard WAV/ffmpeg layout alias",
                code,
                lpcm_channel_order_policy.as_str(),
                layout.output_order_label(lpcm_channel_order_policy)
            );
        }
    }
    args.extend([
        "-i".into(),
        raw_path.to_string_lossy().into_owned(),
        "-map".into(),
        "0:a:0".into(),
    ]);
    append_downmix_ffmpeg_args(&mut args, downmix_policy, source_channel_count)?;
    args.extend([
        "-c:a".into(),
        "pcm_s32le".into(),
        "-f".into(),
        "wav".into(),
        tmp_wav_guard.path().to_string_lossy().into_owned(),
    ]);

    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args,
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DVDA_FFMPEG_TIMEOUT,
    };

    match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => {}
        Err(ToolRunnerError::Cancelled { .. }) => {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        Err(err) => {
            return Err(ConvertError::Tool(err));
        }
    }

    let metadata = fs::metadata(tmp_wav_guard.path()).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "ffmpeg did not write DVD-Audio LPCM WAV output {}: {err}",
            tmp_wav_guard.path().display()
        ))
    })?;
    if metadata.len() == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "ffmpeg wrote an empty DVD-Audio LPCM WAV output: {}",
            tmp_wav_guard.path().display()
        )));
    }

    let probe = validate_dvda_wav(tmp_wav_guard.path(), expectation, runner, cancel, tool_concurrency_limits).await?;
    atomically_replace_dvda_output(tmp_wav_guard.path(), out_path)?;
    tmp_wav_guard.disarm();
    Ok(probe)
}


struct DvdaOutputLock {
    path: PathBuf,
}

impl DvdaOutputLock {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for DvdaOutputLock {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            if err.kind() != io::ErrorKind::NotFound {
                log::warn!(
                    "failed to release DVD-Audio realization lock {}: {err}",
                    self.path.display()
                );
            }
        }
    }
}

async fn acquire_dvda_output_lock(
    out_path: &Path,
    cancel: &CancellationToken,
) -> Result<DvdaOutputLock, ConvertError> {
    let lock_path = dvda_output_lock_path(out_path);
    let parent = lock_path.parent().ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio output lock has no parent path: {}",
            lock_path.display()
        ))
    })?;
    fs::create_dir_all(parent)?;

    loop {
        if cancel.is_cancelled() {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} acquired_unix_seconds={}",
                    std::process::id(),
                    unix_time_seconds()
                );
                let _ = file.sync_all();
                log::debug!(
                    "acquired DVD-Audio realization lock {}",
                    lock_path.display()
                );
                return Ok(DvdaOutputLock::new(lock_path));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if dvda_lock_file_is_stale(&lock_path)? {
                    log::warn!(
                        "removing stale DVD-Audio realization lock {}",
                        lock_path.display()
                    );
                    match fs::remove_file(&lock_path) {
                        Ok(()) => continue,
                        Err(remove_err) if remove_err.kind() == io::ErrorKind::NotFound => continue,
                        Err(remove_err) => return Err(ConvertError::Io(remove_err)),
                    }
                }
                tokio::time::sleep(DVDA_OUTPUT_LOCK_POLL_INTERVAL).await;
            }
            Err(err) => return Err(ConvertError::Io(err)),
        }
    }
}

fn dvda_output_lock_path(out_path: &Path) -> PathBuf {
    let file_name = out_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dvda-track.wav");
    out_path.with_file_name(format!(".{file_name}.realize.lock"))
}

fn dvda_lock_file_is_stale(path: &Path) -> Result<bool, ConvertError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(ConvertError::Io(err)),
    };
    let threshold = env_u64(DVDA_LOCK_STALE_SECONDS_ENV).unwrap_or(DVDA_DEFAULT_STALE_LOCK_SECONDS);
    file_age_seconds(&metadata)
        .map(|age| age >= threshold)
        .map_err(ConvertError::Io)
}

struct DvdaTempPathGuard {
    path: PathBuf,
    active: bool,
}

impl DvdaTempPathGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.active = false;
    }

    fn remove_now(&mut self) -> Result<(), ConvertError> {
        if self.active {
            match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(ConvertError::Io(err)),
            }
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for DvdaTempPathGuard {
    fn drop(&mut self) {
        if self.active {
            if let Err(err) = fs::remove_file(&self.path) {
                if err.kind() != io::ErrorKind::NotFound {
                    log::warn!(
                        "failed to clean DVD-Audio temp artifact {}: {err}",
                        self.path.display()
                    );
                }
            }
        }
    }
}

fn recover_stale_dvda_temp_artifacts(dir: &Path) -> Result<(), ConvertError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ConvertError::Io(err)),
    };
    let threshold = env_u64(DVDA_TEMP_STALE_SECONDS_ENV).unwrap_or(DVDA_DEFAULT_STALE_TEMP_SECONDS);
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !is_dvda_temp_artifact(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(ConvertError::Io(err)),
        };
        if !metadata.is_file() {
            continue;
        }
        if file_age_seconds(&metadata)? >= threshold {
            log::warn!(
                "removing stale DVD-Audio temp artifact {}",
                path.display()
            );
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(ConvertError::Io(err)),
            }
        }
    }
    Ok(())
}

fn is_dvda_temp_artifact(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with(".tonepoet-dvda")
        && (name.ends_with(".tmp.mlp")
            || name.ends_with(".tmp.s32le")
            || name.ends_with(".tmp.wav")
            || name.ends_with(".tmp.json"))
}

fn quarantine_invalid_dvda_cache(path: &Path) -> Result<(), ConvertError> {
    if !path.exists() {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dvda-track.wav");
    let quarantine_path = parent.join(format!(
        ".{name}.invalid-{}-{}.recovered",
        std::process::id(),
        unix_time_nanos()
    ));
    match fs::rename(path, &quarantine_path) {
        Ok(()) => {
            log::warn!(
                "quarantined invalid DVD-Audio cached WAV {} as {}",
                path.display(),
                quarantine_path.display()
            );
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(ConvertError::Io(err)),
    }
}

fn file_age_seconds(metadata: &fs::Metadata) -> io::Result<u64> {
    let modified = metadata.modified()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_else(|_| Duration::from_secs(0));
    Ok(age.as_secs())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse::<u64>().ok()
}

fn unix_time_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn unix_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos()
}

fn create_unique_dvda_temp_file(
    parent: &Path,
    prefix: &str,
    suffix: &str,
) -> io::Result<(PathBuf, File)> {
    fs::create_dir_all(parent)?;
    let temp_prefix = format!(".tonepoet-dvda{prefix}");
    let temp = tempfile::Builder::new()
        .prefix(&temp_prefix)
        .suffix(suffix)
        .tempfile_in(parent)?;
    let (file, path) = temp.keep().map_err(|err| err.error)?;
    Ok((path, file))
}

fn atomically_replace_dvda_output(tmp_path: &Path, out_path: &Path) -> Result<(), ConvertError> {
    #[cfg(windows)]
    if out_path.exists() {
        fs::remove_file(out_path)?;
    }
    fs::rename(tmp_path, out_path)?;
    Ok(())
}

async fn dvda_wav_is_ready(
    path: &Path,
    expectation: DvdaWavExpectation,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<Option<DvdaWavProbe>, ConvertError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ConvertError::Io(err)),
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return Ok(None);
    }

    match validate_dvda_wav(path, expectation, runner, cancel, tool_concurrency_limits).await {
        Ok(probe) => Ok(Some(probe)),
        Err(err) => {
            log::warn!(
                "DVD-Audio cached WAV {} is not reusable and will be regenerated: {err}",
                path.display()
            );
            quarantine_invalid_dvda_cache(path)?;
            Ok(None)
        }
    }
}

async fn validate_dvda_wav(
    path: &Path,
    expectation: DvdaWavExpectation,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<DvdaWavProbe, ConvertError> {
    let probe = probe_dvda_wav(path, runner, cancel, tool_concurrency_limits).await?;
    validate_dvda_wav_probe(path, &probe, expectation)?;
    Ok(probe)
}

fn validate_dvda_wav_probe(
    path: &Path,
    probe: &DvdaWavProbe,
    expectation: DvdaWavExpectation,
) -> Result<(), ConvertError> {
    validate_dvda_wav_probe_with_mode(
        path,
        probe,
        expectation,
        DvdaDurationValidationMode::from_env(),
    )
}

fn validate_dvda_wav_probe_with_mode(
    path: &Path,
    probe: &DvdaWavProbe,
    expectation: DvdaWavExpectation,
    duration_mode: DvdaDurationValidationMode,
) -> Result<(), ConvertError> {
    if probe.codec_name.as_deref() != Some("pcm_s32le") {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio WAV output codec is {}, expected pcm_s32le: {}",
            probe.codec_name.as_deref().unwrap_or("unknown"),
            path.display()
        )));
    }
    if probe.sample_rate == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio WAV output has no valid sample rate: {}",
            path.display()
        )));
    }
    if probe.channels == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio WAV output has no valid channel count: {}",
            path.display()
        )));
    }

    if let Some(expected_rate) = expectation.sample_rate {
        if probe.sample_rate != expected_rate {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio WAV sample rate mismatch for {}: IFO expected {expected_rate} Hz, decoded WAV reports {} Hz",
                path.display(),
                probe.sample_rate
            )));
        }
    }

    if let Some(expected_channels) = expectation.channel_count {
        if probe.channels != expected_channels {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio WAV channel count mismatch for {}: IFO expected {expected_channels}, decoded WAV reports {}",
                path.display(),
                probe.channels
            )));
        }
    }

    validate_dvda_wav_channel_layout(path, probe, expectation)?;

    if let Some(expected_bits) = expectation.source_bit_depth {
        if !matches!(expected_bits, 16 | 20 | 24) {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio IFO source bit depth for {} is {expected_bits}, expected one of 16, 20, or 24",
                path.display()
            )));
        }
    }

    if let (Some(expected_rate), true) = (expectation.sample_rate, expectation.len_in_pts > 0) {
        let actual_samples = probe.samples.ok_or_else(|| {
            ConvertError::TrackValidation(format!(
                "DVD-Audio WAV duration validation failed because ffprobe returned no sample count or duration: {}",
                path.display()
            ))
        })?;
        let expected_samples = pts_to_samples(expectation.len_in_pts, expected_rate);
        let delta = actual_samples.abs_diff(expected_samples);
        let allowed = duration_mode.allowed_sample_drift(expected_rate);
        let drift_log = format!(
            "DVD-Audio WAV sample drift for {}: expected_samples={}, actual_samples={}, drift_samples={}, allowed_samples={}, validation_mode={}",
            path.display(),
            expected_samples,
            actual_samples,
            delta,
            allowed,
            duration_mode.as_str()
        );
        if delta == 0 {
            log::info!("{drift_log}");
        } else {
            log::warn!("{drift_log}");
        }
        if delta > allowed {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Audio WAV sample drift for {}: expected {expected_samples}, got {actual_samples}, drift {delta}, allowed {allowed} in {} mode",
                path.display(),
                duration_mode.as_str()
            )));
        }
    }

    Ok(())
}


fn validate_dvda_wav_channel_layout(
    path: &Path,
    probe: &DvdaWavProbe,
    expectation: DvdaWavExpectation,
) -> Result<(), ConvertError> {
    let Some(code) = expectation.channel_assignment_code else {
        return Ok(());
    };
    let layout = layout_for_assignment_code(code).ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio IFO channel-assignment code {code} is unsupported while validating WAV layout: {}",
            path.display()
        ))
    })?;

    let output_order = layout.output_order_label(expectation.channel_order_policy);
    if expectation.channel_count.is_none() && probe.channels != layout.total_channel_count() {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Audio WAV channel-count/layout mismatch for {}: assignment {} expects {} channels in output order [{}] under policy {}, decoded WAV reports {} channels",
            path.display(),
            code,
            layout.total_channel_count(),
            output_order,
            expectation.channel_order_policy.as_str(),
            probe.channels
        )));
    }

    let expected_alias = layout.ffmpeg_input_layout_for_policy(expectation.channel_order_policy);
    let probe_layout = probe.channel_layout.as_deref().map(normalized_ffmpeg_channel_layout);
    match (expected_alias, probe_layout.as_deref()) {
        (Some(alias), Some(actual)) => {
            let normalized_alias = normalized_ffmpeg_channel_layout(alias);
            if normalized_alias != actual {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Audio WAV channel-layout mismatch for {}: assignment {} output order [{}] under policy {} expects ffmpeg/WAV layout {}, ffprobe reports {}",
                    path.display(),
                    code,
                    output_order,
                    expectation.channel_order_policy.as_str(),
                    alias,
                    actual
                )));
            }
            log::info!(
                "DVD-Audio WAV channel layout validated for {}: assignment {}, policy={}, output_order=[{}], ffprobe_layout={}",
                path.display(),
                code,
                expectation.channel_order_policy.as_str(),
                output_order,
                actual
            );
        }
        (Some(alias), None) => {
            log::warn!(
                "DVD-Audio WAV {} has no ffprobe channel_layout; assignment {} output order [{}] under policy {} is compatible with {}",
                path.display(),
                code,
                output_order,
                expectation.channel_order_policy.as_str(),
                alias
            );
        }
        (None, Some(actual)) => {
            log::warn!(
                "DVD-Audio WAV {} reports ffprobe channel_layout={} for assignment {} output order [{}] under policy {}, but no safe standard WAV/ffmpeg alias is known; treating ffprobe layout as inferred metadata and validating channel count/order by DVD-A manifest instead",
                path.display(),
                actual,
                code,
                output_order,
                expectation.channel_order_policy.as_str()
            );
        }
        (None, None) => {
            log::info!(
                "DVD-Audio WAV {} leaves channel_layout unspecified for assignment {} under policy {}; output order remains [{}]",
                path.display(),
                code,
                expectation.channel_order_policy.as_str(),
                output_order
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DvdaDurationValidationMode {
    Phase3Tolerance,
    NearExact,
    Strict,
}

impl DvdaDurationValidationMode {
    fn from_env() -> Self {
        if std::env::var(DVDA_STRICT_DURATION_ENV)
            .map(|value| env_flag_is_enabled(&value))
            .unwrap_or(false)
        {
            return Self::Strict;
        }

        if std::env::var(DVDA_NEAR_EXACT_DURATION_ENV)
            .map(|value| env_flag_is_enabled(&value))
            .unwrap_or(false)
            || std::env::var(DVDA_CORPUS_STRICT_ENV)
                .map(|value| env_flag_is_enabled(&value))
                .unwrap_or(false)
        {
            return Self::NearExact;
        }

        Self::Phase3Tolerance
    }

    const fn allowed_sample_drift(self, sample_rate: u32) -> u64 {
        match self {
            Self::Phase3Tolerance => sample_rate as u64,
            Self::NearExact => 1,
            Self::Strict => 0,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Phase3Tolerance => "phase3-tolerance",
            Self::NearExact => "near-exact",
            Self::Strict => "strict",
        }
    }
}

fn env_flag_is_enabled(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(normalized.as_str(), "1" | "true" | "yes" | "on" | "strict")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DvdaWavExpectation {
    len_in_pts: u32,
    sample_rate: Option<u32>,
    channel_count: Option<u32>,
    source_bit_depth: Option<u32>,
    channel_assignment_code: Option<u8>,
    channel_order_policy: DvdaChannelOrderPolicy,
}

impl DvdaWavExpectation {
    fn can_validate_existing_cache(self) -> bool {
        self.sample_rate.is_some() && self.channel_count.is_some()
    }

    fn with_stream_facts(self, facts: StreamDerivedAudioFacts) -> Self {
        // Carrier-derived facts must win over IFO/SAMG presentation facts.
        // Authored stereo presentations can report 2 channels in IFO-facing
        // metadata while the actual MLP carrier is 5.1; forced and automatic
        // downmix validation must therefore use the inspected carrier count.
        let merged = Self {
            len_in_pts: self.len_in_pts,
            sample_rate: facts.sample_rate.or(self.sample_rate),
            channel_count: facts.channel_count.or(self.channel_count),
            source_bit_depth: facts.bit_depth.or(self.source_bit_depth),
            channel_assignment_code: facts.channel_assignment_code.or(self.channel_assignment_code),
            channel_order_policy: self.channel_order_policy,
        };

        log_active_fact_resolution("sample rate", self.sample_rate, facts.sample_rate, facts.evidence_source);
        log_active_fact_resolution("channel count", self.channel_count, facts.channel_count, facts.evidence_source);
        log_active_fact_resolution("bit depth", self.source_bit_depth, facts.bit_depth, facts.evidence_source);
        log_active_fact_resolution("channel assignment", self.channel_assignment_code.map(u32::from), facts.channel_assignment_code.map(u32::from), facts.evidence_source);

        merged
    }

    fn with_downmix_policy(self, policy: DvdaDownmixPolicy) -> Self {
        match policy.output_channel_count() {
            Some(channel_count) => Self {
                channel_count: Some(channel_count),
                channel_assignment_code: None,
                channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
                ..self
            },
            None => self,
        }
    }

    fn with_channel_order_policy(self, policy: DvdaChannelOrderPolicy) -> Self {
        Self {
            channel_order_policy: policy,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DvdaRealizationMetadata {
    downmix_matrix: Option<u8>,
    downmix_policy: DvdaDownmixPolicy,
    packet_first_cci: Option<u8>,
    packet_last_cci: Option<u8>,
    packet_cci_change_count: Option<u64>,
    packet_cci_evidence: &'static str,
    demux: Option<DvdaDemuxAudit>,
    mlp: Option<DvdaMlpAudit>,
    lpcm: Option<DvdaLpcmAudit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DvdaDemuxAudit {
    sectors_seen: u64,
    private_stream_1_packets: u64,
    mlp_packets: u64,
    pcm_packets: u64,
    mlp_payload_bytes: u64,
    pcm_payload_bytes: u64,
    cyclic_discontinuity_count: u64,
    extra_header_length_change_count: u64,
    nonstandard_mlp_extra_header_packets: u64,
    nonstandard_pcm_extra_header_packets: u64,
}

impl DvdaDemuxAudit {
    fn from_stats(stats: &DvdaDemuxStats) -> Self {
        Self {
            sectors_seen: stats.sectors_seen,
            private_stream_1_packets: stats.private_stream_1_packets,
            mlp_packets: stats.mlp_packets,
            pcm_packets: stats.pcm_packets,
            mlp_payload_bytes: stats.mlp_payload_bytes,
            pcm_payload_bytes: stats.pcm_payload_bytes,
            cyclic_discontinuity_count: stats.cyclic_discontinuity_count,
            extra_header_length_change_count: stats.extra_header_length_change_count,
            nonstandard_mlp_extra_header_packets: stats.nonstandard_mlp_extra_header_packets,
            nonstandard_pcm_extra_header_packets: stats.nonstandard_pcm_extra_header_packets,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DvdaMlpAudit {
    payload_bytes: u64,
    frame_count: u64,
    major_sync_frame_count: u64,
    min_frame_bytes: Option<usize>,
    max_frame_bytes: Option<usize>,
}

impl DvdaMlpAudit {
    fn from_inspection(inspection: &MlpStreamInspection) -> Self {
        Self {
            payload_bytes: inspection.payload_bytes,
            frame_count: inspection.frame_count,
            major_sync_frame_count: inspection.major_sync_frame_count,
            min_frame_bytes: inspection.min_frame_bytes,
            max_frame_bytes: inspection.max_frame_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DvdaLpcmAudit {
    packets: u64,
    payload_bytes: u64,
    bytes_decoded: u64,
    samples_per_channel: u64,
    group2_blocks_read: u64,
    group2_blocks_repeated: u64,
    format_change_count: u64,
    channel_order_policy: DvdaChannelOrderPolicy,
}

impl DvdaLpcmAudit {
    fn from_stats(stats: &LpcmDecodeStats) -> Self {
        Self {
            packets: stats.packets,
            payload_bytes: stats.payload_bytes,
            bytes_decoded: stats.bytes_decoded,
            samples_per_channel: stats.samples_per_channel,
            group2_blocks_read: stats.group2_blocks_read,
            group2_blocks_repeated: stats.group2_blocks_repeated,
            format_change_count: stats.format_change_count,
            channel_order_policy: stats.channel_order_policy,
        }
    }
}

impl DvdaRealizationMetadata {
    fn from_track_cache_hit(track: &DvdaTrackRealizeInput) -> Self {
        Self {
            downmix_matrix: track.downmix_matrix,
            downmix_policy: track.dvda_downmix_policy,
            packet_first_cci: None,
            packet_last_cci: None,
            packet_cci_change_count: None,
            packet_cci_evidence: "not inspected on validated cache hit",
            demux: None,
            mlp: None,
            lpcm: None,
        }
    }

    fn from_mlp_track_and_packet_stats(
        track: &DvdaTrackRealizeInput,
        stats: &DvdaDemuxStats,
        inspection: Option<&MlpStreamInspection>,
    ) -> Self {
        Self {
            downmix_matrix: track.downmix_matrix,
            downmix_policy: track.dvda_downmix_policy,
            packet_first_cci: stats.first_sub_header.and_then(|header| header.cci),
            packet_last_cci: stats.last_sub_header.and_then(|header| header.cci),
            packet_cci_change_count: Some(stats.cci_change_count),
            packet_cci_evidence: "DVD-Audio Private Stream 1 sub-header",
            demux: Some(DvdaDemuxAudit::from_stats(stats)),
            mlp: inspection.map(DvdaMlpAudit::from_inspection),
            lpcm: None,
        }
    }

    fn from_lpcm_track_and_packet_stats(
        track: &DvdaTrackRealizeInput,
        stats: &DvdaDemuxStats,
        lpcm_stats: &LpcmDecodeStats,
    ) -> Self {
        Self {
            downmix_matrix: track.downmix_matrix,
            downmix_policy: track.dvda_downmix_policy,
            packet_first_cci: stats.first_sub_header.and_then(|header| header.cci),
            packet_last_cci: stats.last_sub_header.and_then(|header| header.cci),
            packet_cci_change_count: Some(stats.cci_change_count),
            packet_cci_evidence: "DVD-Audio Private Stream 1 sub-header",
            demux: Some(DvdaDemuxAudit::from_stats(stats)),
            mlp: None,
            lpcm: Some(DvdaLpcmAudit::from_stats(lpcm_stats)),
        }
    }

    const fn downmix_behavior(self) -> &'static str {
        self.downmix_policy.behavior()
    }

    const fn cci_behavior(self) -> &'static str {
        "record packet CCI; do not transform audio or enforce copy-control flags during realization"
    }
}

fn log_dvda_audio_format_record(
    path: &Path,
    track: &DvdaTrackRealizeInput,
    source_expectation: DvdaWavExpectation,
    expectation: DvdaWavExpectation,
    probe: &DvdaWavProbe,
    policy: &DvdaRealizationAudioPolicy,
    metadata: DvdaRealizationMetadata,
    state: &str,
) {
    let sample_audit = sample_count_audit(expectation, probe);
    log::info!(
        "DVD-Audio realization audit for {} ({}): volume={}, address_space={:?}, ats={:?}, group={}, title_nr={:?}, title_ordinal={:?}, group_track={}, ats_track={:?}, samg_track={:?}, sector_ranges={}, aob_parts_touched={}, ps1_packets={}, mlp_packets={}, lpcm_packets={}, mlp_payload_bytes={}, lpcm_payload_bytes={}, decoded_sample_rate={} Hz, decoded_channels={}, expected_samples={}, actual_samples={}, drift_samples={}, source_bit_depth={}, carrier_codec=pcm_s32le, final_encode_bit_depth_policy={}, final_encode_bit_depth={}, packet_first_cci={}, packet_last_cci={}, packet_cci_change_count={}, packet_cci_behavior={}, downmix_matrix={}, dvda_downmix_policy={}, downmix_behavior={}, lpcm_channel_order_policy={}, lpcm_channel_order_behavior={}, expected_channel_source_order={}, expected_channel_wave_order={}, expected_channel_output_order={}",
        path.display(),
        state,
        track.volume_source.original_container().display(),
        track.sector_address_space,
        track.title_set_nr,
        track.group_nr,
        track.title_nr,
        track.title_ordinal,
        track.group_track_ordinal,
        track.ats_track_nr,
        track.samg_track_nr,
        sector_ranges_log_label(track),
        aob_parts_touched_log_label(track),
        optional_u64_label(metadata.demux.map(|stats| stats.private_stream_1_packets)),
        optional_u64_label(metadata.demux.map(|stats| stats.mlp_packets)),
        optional_u64_label(metadata.demux.map(|stats| stats.pcm_packets)),
        optional_u64_label(metadata.demux.map(|stats| stats.mlp_payload_bytes)),
        optional_u64_label(metadata.demux.map(|stats| stats.pcm_payload_bytes)),
        probe.sample_rate,
        probe.channels,
        optional_u64_label(sample_audit.expected_samples),
        optional_u64_label(sample_audit.actual_samples),
        optional_u64_label(sample_audit.drift_samples),
        optional_u32_label(source_expectation.source_bit_depth),
        policy.final_encode_bit_depth_policy,
        optional_u32_label(policy.final_encode_bit_depth),
        optional_u8_label(metadata.packet_first_cci),
        optional_u8_label(metadata.packet_last_cci),
        optional_u64_label(metadata.packet_cci_change_count),
        metadata.cci_behavior(),
        optional_u8_label(metadata.downmix_matrix),
        metadata.downmix_policy.as_str(),
        metadata.downmix_behavior(),
        policy.lpcm_channel_order_policy.as_str(),
        policy.lpcm_channel_order_policy.behavior(),
        source_expectation.channel_assignment_code.and_then(layout_for_assignment_code).map(|layout| layout.order_label()).unwrap_or_else(|| "unknown".to_string()),
        expectation.channel_assignment_code.and_then(layout_for_assignment_code).map(|layout| layout.wave_order_label()).unwrap_or_else(|| "unknown".to_string()),
        expectation.channel_assignment_code.and_then(layout_for_assignment_code).map(|layout| layout.output_order_label(policy.lpcm_channel_order_policy)).unwrap_or_else(|| "unknown".to_string())
    );
}

fn write_dvda_audio_format_manifest(
    wav_path: &Path,
    track: &DvdaTrackRealizeInput,
    source_expectation: DvdaWavExpectation,
    expectation: DvdaWavExpectation,
    probe: &DvdaWavProbe,
    policy: &DvdaRealizationAudioPolicy,
    metadata: DvdaRealizationMetadata,
    state: &str,
) -> Result<(), ConvertError> {
    let manifest_path = dvda_audio_format_manifest_path(wav_path);
    let parent = manifest_path.parent().ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio audio-format manifest has no parent path: {}",
            manifest_path.display()
        ))
    })?;
    fs::create_dir_all(parent)?;

    let manifest = serde_json::json!({
        "schema": "tonepoet.dvda.realized_audio_format.v6",
        "state": state,
        "wav_path": wav_path.to_string_lossy().as_ref(),
        "volume": dvda_volume_source_json(&track.volume_source),
        "track": dvda_track_identity_json(track),
        "sectors": {
            "address_space": dvda_sector_address_space_label(track.sector_address_space),
            "ranges": dvda_sector_ranges_json(track),
            "total_sectors": track.sector_ranges.iter().map(|range| u64::from(range.block_count())).sum::<u64>(),
            "aob_parts_touched": dvda_aob_parts_touched_json(track)
        },
        "source": {
            "bit_depth": source_expectation.source_bit_depth,
            "sample_rate": source_expectation.sample_rate,
            "channel_count": source_expectation.channel_count,
            "channel_assignment_code": source_expectation.channel_assignment_code,
            "channel_order": channel_order_manifest_json(source_expectation, policy.lpcm_channel_order_policy),
            "bit_depth_validation": "IFO/source metadata; realized WAV uses pcm_s32le carrier"
        },
        "expected_output": {
            "sample_rate": expectation.sample_rate,
            "channel_count": expectation.channel_count,
            "channel_assignment_code": expectation.channel_assignment_code,
            "channel_order": channel_order_manifest_json(expectation, policy.lpcm_channel_order_policy)
        },
        "packet_metadata": {
            "first_cci": metadata.packet_first_cci,
            "last_cci": metadata.packet_last_cci,
            "cci_change_count": metadata.packet_cci_change_count,
            "cci_evidence": metadata.packet_cci_evidence,
            "cci_behavior": metadata.cci_behavior()
        },
        "downmix": {
            "ifo_matrix": metadata.downmix_matrix,
            "policy": metadata.downmix_policy.as_str(),
            "behavior": metadata.downmix_behavior(),
            "applied_during_realization": metadata.downmix_policy.is_active(),
            "foo_input_dvda_compatible_pan_filter": if metadata.downmix_policy == DvdaDownmixPolicy::FooInputDvdaCompatible { Some(FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER) } else { None }
        },
        "demux": dvda_demux_audit_json(metadata.demux),
        "mlp": dvda_mlp_audit_json(metadata.mlp),
        "lpcm": dvda_lpcm_audit_json(metadata.lpcm),
        "decoded": {
            "sample_rate": probe.sample_rate,
            "channel_count": probe.channels,
            "channel_layout": probe.channel_layout.as_deref(),
            "samples": probe.samples
        },
        "duration_validation": sample_count_audit_json(expectation, probe),
        "carrier": {
            "container": "wav",
            "codec": "pcm_s32le",
            "sample_format": "s32le",
            "bit_depth": 32,
            "sample_rate": probe.sample_rate,
            "channel_count": probe.channels,
            "channel_layout": probe.channel_layout.as_deref(),
            "channel_order_policy": expectation.channel_order_policy.as_str(),
            "channel_order_behavior": expectation.channel_order_policy.behavior(),
            "channel_order": expectation.channel_assignment_code.and_then(layout_for_assignment_code).map(|layout| layout.output_order_label(expectation.channel_order_policy))
        },
        "final_encode": {
            "bit_depth_policy": policy.final_encode_bit_depth_policy.as_str(),
            "resolved_bit_depth": policy.final_encode_bit_depth,
            "lpcm_channel_order_policy": policy.lpcm_channel_order_policy.as_str()
        },
        "support_boundaries": dvda_support_boundaries_json(track, metadata)
    });

    let payload = serde_json::to_vec_pretty(&manifest).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio audio-format manifest serialization failed for {}: {err}",
            wav_path.display()
        ))
    })?;
    let manifest_parent = manifest_path.parent().ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Audio audio-format manifest has no parent path: {}",
            manifest_path.display()
        ))
    })?;
    let (tmp_path, mut tmp_file) = create_unique_dvda_temp_file(
        manifest_parent,
        &format!(".{}.", manifest_path.file_name().and_then(|name| name.to_str()).unwrap_or("dvda-audio-format.json")),
        ".tmp.json",
    )?;
    let mut tmp_guard = DvdaTempPathGuard::new(tmp_path);
    let write_result = tmp_file
        .write_all(&payload)
        .and_then(|_| tmp_file.sync_all());
    drop(tmp_file);
    write_result?;
    atomically_replace_dvda_output(tmp_guard.path(), &manifest_path)?;
    tmp_guard.disarm();
    log::info!(
        "DVD-Audio realization audit manifest written: {}",
        manifest_path.display()
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DvdaSampleCountAudit {
    expected_samples: Option<u64>,
    actual_samples: Option<u64>,
    drift_samples: Option<u64>,
    allowed_samples: Option<u64>,
    validation_mode: DvdaDurationValidationMode,
}

fn dvda_support_boundaries_json(
    track: &DvdaTrackRealizeInput,
    metadata: DvdaRealizationMetadata,
) -> serde_json::Value {
    let address_space = dvda_sector_address_space_label(track.sector_address_space);
    let stream_kind = if metadata.mlp.is_some() {
        Some("mlp")
    } else if metadata.lpcm.is_some() {
        Some("lpcm")
    } else {
        None
    };

    serde_json::json!({
        "multi_format_ats": {
            "runtime_stream_facts_available": stream_kind.is_some(),
            "stream_kind": stream_kind,
            "corpus_proof_required": "TONEPOET_DVDA_PHASE3_REQUIRE_MULTIFORMAT_ATS_CORPUS=1",
            "status_note": "Unit and stream-derived validation exist; real multi-format ATS support is not corpus-proven until the extended corpus gate passes."
        },
        "samg_absolute": {
            "address_space": address_space,
            "iso_backed_runtime_path_exercised_by_this_track": matches!(track.sector_address_space, DvdaSectorAddressSpace::DiscAbsolute { .. } | DvdaSectorAddressSpace::SamgAbsolute),
            "directory_tree_supported": false,
            "corpus_proof_required": "TONEPOET_DVDA_PHASE3_REQUIRE_SAMG_ABSOLUTE_CORPUS=1",
            "status_note": "disc-absolute sectors are readable only from ISO-backed sources because directory copies do not preserve original disc LBAs. Real SAMG-driven support is not corpus-proven until the extended corpus gate passes."
        }
    })
}

fn sample_count_audit(expectation: DvdaWavExpectation, probe: &DvdaWavProbe) -> DvdaSampleCountAudit {
    let mode = DvdaDurationValidationMode::from_env();
    let expected_samples = expectation
        .sample_rate
        .filter(|_| expectation.len_in_pts > 0)
        .map(|sample_rate| pts_to_samples(expectation.len_in_pts, sample_rate));
    let actual_samples = probe.samples;
    let drift_samples = expected_samples.zip(actual_samples).map(|(expected, actual)| actual.abs_diff(expected));
    let allowed_samples = expectation
        .sample_rate
        .filter(|_| expectation.len_in_pts > 0)
        .map(|sample_rate| mode.allowed_sample_drift(sample_rate));
    DvdaSampleCountAudit {
        expected_samples,
        actual_samples,
        drift_samples,
        allowed_samples,
        validation_mode: mode,
    }
}

fn sample_count_audit_json(expectation: DvdaWavExpectation, probe: &DvdaWavProbe) -> serde_json::Value {
    let audit = sample_count_audit(expectation, probe);
    serde_json::json!({
        "expected_samples": audit.expected_samples,
        "actual_samples": audit.actual_samples,
        "drift_samples": audit.drift_samples,
        "allowed_samples": audit.allowed_samples,
        "validation_mode": audit.validation_mode.as_str(),
        "len_in_pts": expectation.len_in_pts,
        "pts_timebase_hz": PTS_PER_SECOND as u64,
        "sample_rate_source": if expectation.sample_rate.is_some() { "IFO or stream-derived expectation" } else { "unknown" }
    })
}


fn channel_order_manifest_json(
    expectation: DvdaWavExpectation,
    lpcm_policy: DvdaChannelOrderPolicy,
) -> serde_json::Value {
    let Some(code) = expectation.channel_assignment_code else {
        return serde_json::json!({
            "available": false,
            "policy": lpcm_policy.as_str(),
            "behavior": lpcm_policy.behavior()
        });
    };
    match layout_for_assignment_code(code) {
        Some(layout) => serde_json::json!({
            "available": true,
            "assignment_code": code,
            "dvd_audio_source_order": layout.order_label(),
            "waveformatextensible_order": layout.wave_order_label(),
            "realized_lpcm_policy": lpcm_policy.as_str(),
            "realized_lpcm_behavior": lpcm_policy.behavior(),
            "realized_lpcm_order": layout.output_order_label(lpcm_policy),
            "safe_ffmpeg_layout_for_realized_lpcm_order": layout.ffmpeg_input_layout_for_policy(lpcm_policy),
            "mlp_note": "MLP decode is delegated to ffmpeg; this policy applies to the in-process LPCM unpacker"
        }),
        None => serde_json::json!({
            "available": false,
            "assignment_code": code,
            "policy": lpcm_policy.as_str(),
            "behavior": lpcm_policy.behavior(),
            "error": "unsupported DVD-Audio channel-assignment code"
        }),
    }
}

fn dvda_volume_source_json(source: &DvdaVolumeSourceRef) -> serde_json::Value {
    match source {
        DvdaVolumeSourceRef::Directory { root } => serde_json::json!({
            "kind": "directory",
            "root": root.to_string_lossy().as_ref(),
            "original_container": source.original_container().to_string_lossy().as_ref()
        }),
        DvdaVolumeSourceRef::Iso { path, backend } => serde_json::json!({
            "kind": "iso",
            "path": path.to_string_lossy().as_ref(),
            "backend": format!("{backend:?}"),
            "original_container": source.original_container().to_string_lossy().as_ref()
        }),
        DvdaVolumeSourceRef::StagedAudioTs { original, root } => serde_json::json!({
            "kind": "staged_audio_ts",
            "original": original.to_string_lossy().as_ref(),
            "root": root.to_string_lossy().as_ref(),
            "original_container": source.original_container().to_string_lossy().as_ref()
        }),
    }
}

fn dvda_track_identity_json(track: &DvdaTrackRealizeInput) -> serde_json::Value {
    serde_json::json!({
        "title_set_nr": track.title_set_nr,
        "ats": track.title_set_nr,
        "group_nr": track.group_nr,
        "group_track_ordinal": track.group_track_ordinal,
        "title_nr": track.title_nr,
        "title_ordinal": track.title_ordinal,
        "ats_track_nr": track.ats_track_nr,
        "samg_track_nr": track.samg_track_nr,
        "samg_ordinal": track.samg_ordinal,
        "first_pts": track.first_pts,
        "len_in_pts": track.len_in_pts,
        "track_type": track.track_type,
        "index_start": track.index_start,
        "audio_format_index": track.audio_format_index,
        "title_table_offset": track.title_table_offset,
        "title_len_in_pts": track.title_len_in_pts,
        "title_track_count_declared": track.title_track_count_declared,
        "title_index_count_declared": track.title_index_count_declared,
        "downmix_matrix": track.downmix_matrix,
        "dvda_downmix_policy": track.dvda_downmix_policy.as_str(),
        "identity_hash": format!("{:016x}", track.stable_identity_hash())
    })
}

fn dvda_sector_address_space_label(address_space: DvdaSectorAddressSpace) -> &'static str {
    match address_space {
        DvdaSectorAddressSpace::AtsAobRelative { .. } => "ats_aob_relative",
        DvdaSectorAddressSpace::DiscAbsolute { .. } => "disc_absolute",
        DvdaSectorAddressSpace::SamgAbsolute => "samg_absolute",
    }
}

fn dvda_sector_ranges_json(track: &DvdaTrackRealizeInput) -> Vec<serde_json::Value> {
    track
        .sector_ranges
        .iter()
        .map(|range| serde_json::json!({
            "index_nr": range.index_nr,
            "first": range.first,
            "last": range.last,
            "sector_count": range.block_count()
        }))
        .collect()
}

fn dvda_aob_parts_touched_json(track: &DvdaTrackRealizeInput) -> Vec<serde_json::Value> {
    track
        .aob_files
        .iter()
        .filter(|aob| aob.exists)
        .filter_map(|aob| {
            let mut touched_first: Option<u32> = None;
            let mut touched_last: Option<u32> = None;
            for range in &track.sector_ranges {
                let first = range.first.max(aob.block_first);
                let last = range.last.min(aob.block_last);
                if first <= last {
                    touched_first = Some(touched_first.map_or(first, |value| value.min(first)));
                    touched_last = Some(touched_last.map_or(last, |value| value.max(last)));
                }
            }
            let touched_first = touched_first?;
            let touched_last = touched_last?;
            Some(serde_json::json!({
                "title_set_nr": aob.title_set_nr,
                "part_nr": aob.part_nr,
                "file_name": aob.file_name.as_str(),
                "byte_len": aob.byte_len,
                "block_first": aob.block_first,
                "block_last": aob.block_last,
                "touched_first": touched_first,
                "touched_last": touched_last,
                "touched_sector_count": touched_last.saturating_sub(touched_first).saturating_add(1)
            }))
        })
        .collect()
}

fn sector_ranges_log_label(track: &DvdaTrackRealizeInput) -> String {
    track
        .sector_ranges
        .iter()
        .map(|range| format!("{}:{}..={}", range.index_nr, range.first, range.last))
        .collect::<Vec<_>>()
        .join(",")
}

fn aob_parts_touched_log_label(track: &DvdaTrackRealizeInput) -> String {
    let labels = track
        .aob_files
        .iter()
        .filter(|aob| aob.exists)
        .filter(|aob| {
            track
                .sector_ranges
                .iter()
                .any(|range| range.first <= aob.block_last && range.last >= aob.block_first)
        })
        .map(|aob| format!("{}:{}", aob.part_nr, aob.file_name.as_str()))
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "none".to_string()
    } else {
        labels.join(",")
    }
}

fn dvda_demux_audit_json(audit: Option<DvdaDemuxAudit>) -> serde_json::Value {
    match audit {
        Some(audit) => serde_json::json!({
            "sectors_seen": audit.sectors_seen,
            "private_stream_1_packets": audit.private_stream_1_packets,
            "mlp_packets": audit.mlp_packets,
            "lpcm_packets": audit.pcm_packets,
            "mlp_payload_bytes": audit.mlp_payload_bytes,
            "lpcm_payload_bytes": audit.pcm_payload_bytes,
            "cyclic_discontinuity_count": audit.cyclic_discontinuity_count,
            "extra_header_length_change_count": audit.extra_header_length_change_count,
            "nonstandard_mlp_extra_header_packets": audit.nonstandard_mlp_extra_header_packets,
            "nonstandard_lpcm_extra_header_packets": audit.nonstandard_pcm_extra_header_packets
        }),
        None => serde_json::json!({
            "available": false,
            "reason": "not inspected on validated cache hit"
        }),
    }
}

fn dvda_mlp_audit_json(audit: Option<DvdaMlpAudit>) -> serde_json::Value {
    match audit {
        Some(audit) => serde_json::json!({
            "payload_bytes": audit.payload_bytes,
            "frame_count": audit.frame_count,
            "major_sync_frame_count": audit.major_sync_frame_count,
            "min_frame_bytes": audit.min_frame_bytes,
            "max_frame_bytes": audit.max_frame_bytes
        }),
        None => serde_json::json!({ "available": false }),
    }
}

fn dvda_lpcm_audit_json(audit: Option<DvdaLpcmAudit>) -> serde_json::Value {
    match audit {
        Some(audit) => serde_json::json!({
            "packets": audit.packets,
            "payload_bytes": audit.payload_bytes,
            "bytes_decoded": audit.bytes_decoded,
            "samples_per_channel": audit.samples_per_channel,
            "group2_blocks_read": audit.group2_blocks_read,
            "group2_blocks_repeated": audit.group2_blocks_repeated,
            "format_change_count": audit.format_change_count,
            "channel_order_policy": audit.channel_order_policy.as_str(),
            "channel_order_behavior": audit.channel_order_policy.behavior()
        }),
        None => serde_json::json!({ "available": false }),
    }
}

fn dvda_audio_format_manifest_path(wav_path: &Path) -> PathBuf {
    let file_name = wav_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dvda-track.wav");
    wav_path.with_file_name(format!("{file_name}.audio-format.json"))
}

fn optional_u32_label(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}


fn optional_u8_label(value: Option<u8>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn optional_u64_label(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn log_active_fact_resolution(label: &str, base: Option<u32>, stream: Option<u32>, evidence_source: &str) {
    match (base, stream) {
        (None, Some(value)) => log::info!(
            "DVD-Audio active {label} resolved from {evidence_source}: {value}"
        ),
        (Some(expected), Some(actual)) if expected == actual => log::info!(
            "DVD-Audio active {label} confirmed by {evidence_source}: {actual}"
        ),
        (Some(expected), Some(actual)) => log::warn!(
            "DVD-Audio active {label} from {evidence_source} is {actual}, while IFO/prepared metadata expected {expected}"
        ),
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DvdaWavProbe {
    codec_name: Option<String>,
    sample_rate: u32,
    channels: u32,
    channel_layout: Option<String>,
    samples: Option<u64>,
}

async fn probe_dvda_wav(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<DvdaWavProbe, ConvertError> {
    let cmd = ToolCommand {
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=codec_name,sample_rate,channels,channel_layout,duration_ts,time_base,duration".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            path.to_string_lossy().into_owned(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(120),
    };

    let output = match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(output) => output,
        Err(ToolRunnerError::Cancelled { .. }) => {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        Err(err) => return Err(ConvertError::Tool(err)),
    };

    parse_dvda_wav_probe(&output.stdout_tail)
}

fn parse_dvda_wav_probe(json: &str) -> Result<DvdaWavProbe, ConvertError> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|err| {
        ConvertError::TrackValidation(format!("ffprobe JSON parse failed for DVD-Audio WAV: {err}"))
    })?;
    let stream = value.pointer("/streams/0").ok_or_else(|| {
        ConvertError::TrackValidation("ffprobe returned no audio stream for DVD-Audio WAV".to_string())
    })?;

    let codec_name = stream
        .get("codec_name")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let sample_rate = stream
        .get("sample_rate")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let channels = stream
        .get("channels")
        .and_then(json_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let channel_layout = stream
        .get("channel_layout")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "unknown")
        .map(ToOwned::to_owned);

    let samples = samples_from_stream_duration_ts(stream, sample_rate).or_else(|| {
        duration_seconds(stream)
            .or_else(|| value.pointer("/format").and_then(duration_seconds))
            .map(|seconds| (seconds * f64::from(sample_rate)).round() as u64)
    });

    Ok(DvdaWavProbe {
        codec_name,
        sample_rate,
        channels,
        channel_layout,
        samples,
    })
}

fn duration_seconds(value: &serde_json::Value) -> Option<f64> {
    value
        .get("duration")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn samples_from_stream_duration_ts(stream: &serde_json::Value, sample_rate: u32) -> Option<u64> {
    if sample_rate == 0 {
        return None;
    }
    let duration_ts = stream.get("duration_ts").and_then(json_u64)?;
    let time_base = stream.get("time_base")?.as_str()?;
    let (num, den) = time_base.split_once('/')?;
    let num = num.parse::<u64>().ok()?;
    let den = den.parse::<u64>().ok()?;
    if den == 0 {
        return None;
    }
    let samples = (duration_ts as u128)
        .checked_mul(num as u128)?
        .checked_mul(sample_rate as u128)?
        .checked_div(den as u128)?;
    u64::try_from(samples).ok()
}

fn pts_to_samples(len_in_pts: u32, sample_rate: u32) -> u64 {
    let numerator = u128::from(len_in_pts) * u128::from(sample_rate);
    ((numerator + (PTS_PER_SECOND / 2)) / PTS_PER_SECOND) as u64
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
}

#[derive(Debug)]
enum RealizeDvdaVolume {
    Directory(DirectoryDvdaVolume),
    IsoUdf(IsoUdfDvdaVolume),
    Iso9660(Iso9660DvdaVolume),
}

impl DvdaVolume for RealizeDvdaVolume {
    fn open_audio_ts_file(&self, name: &str) -> crate::tui::dvda::Result<Box<dyn DvdaFile>> {
        match self {
            Self::Directory(volume) => volume.open_audio_ts_file(name),
            Self::IsoUdf(volume) => volume.open_audio_ts_file(name),
            Self::Iso9660(volume) => volume.open_audio_ts_file(name),
        }
    }

    fn file_len(&self, name: &str) -> crate::tui::dvda::Result<Option<u64>> {
        match self {
            Self::Directory(volume) => volume.file_len(name),
            Self::IsoUdf(volume) => volume.file_len(name),
            Self::Iso9660(volume) => volume.file_len(name),
        }
    }
}

fn open_realize_volume(source: &DvdaVolumeSourceRef) -> Result<RealizeDvdaVolume, ConvertError> {
    match source {
        DvdaVolumeSourceRef::Directory { root } | DvdaVolumeSourceRef::StagedAudioTs { root, .. } => {
            Ok(RealizeDvdaVolume::Directory(DirectoryDvdaVolume::new(root.clone())))
        }
        DvdaVolumeSourceRef::Iso { path, backend } => match backend {
            DvdaIsoBackend::Udf => IsoUdfDvdaVolume::open(path.clone())
                .map(RealizeDvdaVolume::IsoUdf)
                .map_err(|err| ConvertError::Realize(format!("failed to open DVD-Audio UDF ISO {}: {err}", path.display()))),
            DvdaIsoBackend::Iso9660Bridge => Iso9660DvdaVolume::open(path.clone())
                .map(RealizeDvdaVolume::Iso9660)
                .map_err(|err| {
                    ConvertError::Realize(format!(
                        "failed to open DVD-Audio ISO9660 bridge ISO {}: {err}",
                        path.display()
                    ))
                }),
            DvdaIsoBackend::ExplicitRawMagicOnly => Err(ConvertError::TrackValidation(format!(
                "DVD-Audio ISO {} was detected only by raw magic scan and has no readable AUDIO_TS filesystem backend",
                path.display()
            ))),
        },
    }
}

fn to_aob_entries(values: &[DvdaAobFileRef]) -> Vec<AobFileEntry> {
    values
        .iter()
        .map(|value| AobFileEntry {
            title_set_nr: value.title_set_nr,
            part_nr: value.part_nr,
            file_name: value.file_name.clone(),
            exists: value.exists,
            byte_len: value.byte_len,
            block_first: value.block_first,
            block_last: value.block_last,
        })
        .collect()
}

fn hash_path(hash: &mut u64, path: &Path) {
    for byte in path.to_string_lossy().as_bytes() {
        hash_u8(hash, *byte);
    }
}

fn hash_u8(hash: &mut u64, value: u8) {
    *hash ^= u64::from(value);
    *hash = hash.wrapping_mul(0x100000001b3);
}

fn hash_u32(hash: &mut u64, value: u32) {
    for byte in value.to_le_bytes() {
        hash_u8(hash, byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use std::env;

    use super::super::materializer_dvda::DvdaAudioMaterializer;
    use super::super::stages::Materializer;
    use super::super::tool::RealToolRunner;
    use super::super::dvda_mlp::{MlpMajorSyncInfo, MLP_STREAM_TYPE};
    use super::super::types::{
        ChannelGroupDescriptor, CueSidecarPolicy, DvdaGroupSelection, FailurePolicy, LogPolicy,
        NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PublishPolicy,
        SourceAudioCoding, SourceAudioDescriptor, SourceOptions, StagePolicy, StageRequirement,
        TrackSelection, PreparedTrack,
    };

    const DVDA_CORPUS_DIR_ENV: &str = "TONEPOET_DVDA_PHASE3_CORPUS_DIR";
    const DVDA_CORPUS_STRICT_ENV: &str = "TONEPOET_DVDA_PHASE3_CORPUS_STRICT";
    const DVDA_EXTENDED_CORPUS_STRICT_ENV: &str = "TONEPOET_DVDA_PHASE3_EXTENDED_CORPUS_STRICT";
    const DVDA_REQUIRE_MULTIFORMAT_ATS_CORPUS_ENV: &str = "TONEPOET_DVDA_PHASE3_REQUIRE_MULTIFORMAT_ATS_CORPUS";
    const DVDA_REQUIRE_SAMG_ABSOLUTE_CORPUS_ENV: &str = "TONEPOET_DVDA_PHASE3_REQUIRE_SAMG_ABSOLUTE_CORPUS";
    const DEFAULT_DVDA_CORPUS_DIR: &str = "/mnt/scratch/dev/dawdiolab/test-isos";

    fn sample_dvda_track_source() -> TrackSourceRef {
        TrackSourceRef::DvdaTrack {
            volume_source: DvdaVolumeSourceRef::Directory { root: PathBuf::from("/tmp/disc") },
            sector_address_space: DvdaSectorAddressSpace::AtsAobRelative { title_set_nr: 1 },
            group_nr: 1,
            title_set_nr: Some(1),
            title_nr: Some(2),
            title_ordinal: Some(1),
            ats_track_nr: Some(3),
            samg_track_nr: Some(4),
            samg_ordinal: Some(5),
            group_track_ordinal: 6,
            first_pts: 1_000,
            len_in_pts: 90_000,
            track_type: Some(0xa1),
            index_start: Some(1),
            downmix_matrix: Some(3),
            dvda_downmix_policy: DvdaDownmixPolicy::None,
            title_table_offset: Some(0x1234),
            title_len_in_pts: Some(180_000),
            title_track_count_declared: Some(6),
            title_index_count_declared: Some(6),
            audio_format_index: Some(1),
            expected_sample_rate: Some(192_000),
            expected_channel_count: Some(2),
            expected_bit_depth: Some(24),
            expected_channel_assignment_code: Some(1),
            expected_group1_sample_rate: Some(192_000),
            expected_group2_sample_rate: None,
            expected_group1_bit_depth: Some(24),
            expected_group2_bit_depth: None,
            expected_group1_channel_count: Some(2),
            expected_group2_channel_count: None,
            sector_ranges: vec![DvdaSectorRangeRef { index_nr: 1, first: 10, last: 20 }],
            aob_files: vec![DvdaAobFileRef {
                title_set_nr: 1,
                part_nr: 1,
                file_name: "ATS_01_1.AOB".to_string(),
                exists: true,
                byte_len: 2048 * 100,
                block_first: 0,
                block_last: 99,
            }],
        }
    }

    fn sample_dvda_track_realize_input() -> DvdaTrackRealizeInput {
        let source = sample_dvda_track_source();
        DvdaTrackRealizeInput::try_from_source(
            &source,
            DvdaSourceAudioExpectation::from_source_ref(&source),
            DvdaDownmixPolicy::None,
        )
        .expect("sample DVD-Audio source should map to realize input")
    }

    #[derive(Clone, Copy)]
    struct CorpusDisc {
        label: &'static str,
        filename_needles: &'static [&'static str],
        expected_tracks: usize,
    }

    const HDAD2009: CorpusDisc = CorpusDisc {
        label: "HDAD2009",
        filename_needles: &["hdad2009"],
        expected_tracks: 5,
    };
    const AP_I_ROBOT: CorpusDisc = CorpusDisc {
        label: "AP I Robot",
        filename_needles: &["robot"],
        expected_tracks: 10,
    };
    const AP_FRIENDLY_CARD: CorpusDisc = CorpusDisc {
        label: "AP Friendly Card",
        filename_needles: &["friendly", "card"],
        expected_tracks: 10,
    };
    const AP_EYE_IN_THE_SKY: CorpusDisc = CorpusDisc {
        label: "AP Eye in the Sky",
        filename_needles: &["eye", "sky"],
        expected_tracks: 10,
    };

    const PHASE3_CORPUS_DISCS: [CorpusDisc; 4] = [
        HDAD2009,
        AP_I_ROBOT,
        AP_FRIENDLY_CARD,
        AP_EYE_IN_THE_SKY,
    ];

    #[test]
    fn pts_to_samples_rounds_to_nearest_sample() {
        assert_eq!(pts_to_samples(90_000, 192_000), 192_000);
        assert_eq!(pts_to_samples(45_000, 44_100), 22_050);
    }

    #[test]
    fn parses_ffprobe_duration_ts_as_samples() {
        let json = r#"{
            "streams": [{
                "codec_name": "pcm_s32le",
                "sample_rate": "192000",
                "channels": 2,
                "duration_ts": 384000,
                "time_base": "1/192000"
            }]
        }"#;

        let probe = parse_dvda_wav_probe(json).expect("probe should parse");

        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s32le"));
        assert_eq!(probe.sample_rate, 192_000);
        assert_eq!(probe.channels, 2);
        assert_eq!(probe.samples, Some(384_000));
    }

    #[test]
    fn parses_ffprobe_duration_fallback_as_samples() {
        let json = r#"{
            "streams": [{
                "codec_name": "pcm_s32le",
                "sample_rate": "96000",
                "channels": 6,
                "duration": "1.5"
            }]
        }"#;

        let probe = parse_dvda_wav_probe(json).expect("probe should parse");

        assert_eq!(probe.samples, Some(144_000));
    }

    #[test]
    fn wav_validation_rejects_ifo_sample_rate_mismatch() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 96_000,
            channels: 2,
            channel_layout: None,
            samples: Some(96_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let err = validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect_err("sample-rate mismatch should fail validation");

        assert!(err.to_string().contains("sample rate mismatch"));
    }

    #[test]
    fn wav_validation_rejects_ifo_channel_count_mismatch() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 6,
            channel_layout: None,
            samples: Some(192_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let err = validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect_err("channel-count mismatch should fail validation");

        assert!(err.to_string().contains("channel count mismatch"));
    }



    #[test]
    fn wav_validation_accepts_matching_standard_channel_layout() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 96_000,
            channels: 6,
            channel_layout: Some("5.1".to_string()),
            samples: Some(96_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(96_000),
            channel_count: Some(6),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(12),
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect("assignment 12 order should be compatible with standard 5.1 WAV layout");
    }

    #[test]
    fn wav_validation_tolerates_inferred_layout_when_no_safe_alias_exists() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 96_000,
            channels: 6,
            channel_layout: Some("5.1".to_string()),
            samples: Some(96_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(96_000),
            channel_count: Some(6),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(20),
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect("ffmpeg may infer default 5.1 metadata even when assignment 20 remains in DVD-A source order; validation should not reject a valid decode solely because the carrier metadata is ambiguous");
    }

    #[test]
    fn wav_validation_accepts_assignment_20_after_wave_order_policy() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 96_000,
            channels: 6,
            channel_layout: Some("5.1".to_string()),
            samples: Some(96_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(96_000),
            channel_count: Some(6),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(20),
            channel_order_policy: DvdaChannelOrderPolicy::WaveExtensible,
        };

        validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect("assignment 20 is safe to label 5.1 after LPCM samples are reordered into WAVEFORMATEXTENSIBLE order");
    }

    #[test]
    fn wav_validation_rejects_invalid_ifo_source_bit_depth() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: None,
            samples: Some(192_000),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(32),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let err = validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect_err("invalid IFO source bit depth should fail validation");

        assert!(err.to_string().contains("IFO source bit depth"));
    }

    #[test]
    fn wav_validation_skips_duration_when_ifo_sample_rate_is_unknown() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: None,
            samples: None,
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: None,
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        validate_dvda_wav_probe(Path::new("track.wav"), &probe, expectation)
            .expect("unknown IFO sample rate should skip duration validation");
    }



    #[test]
    fn wav_validation_phase3_mode_allows_one_second_sample_drift() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: None,
            samples: Some(192_000 + 191_999),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        validate_dvda_wav_probe_with_mode(
            Path::new("track.wav"),
            &probe,
            expectation,
            DvdaDurationValidationMode::Phase3Tolerance,
        )
        .expect("Phase 3 tolerance should allow less than one second of sample drift");
    }


    #[test]
    fn wav_validation_near_exact_mode_rejects_more_than_one_sample_drift() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: None,
            samples: Some(192_002),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let err = validate_dvda_wav_probe_with_mode(
            Path::new("track.wav"),
            &probe,
            expectation,
            DvdaDurationValidationMode::NearExact,
        )
        .expect_err("near-exact duration mode should reject drift beyond one sample");

        assert!(err.to_string().contains("near-exact"));
        assert!(err.to_string().contains("drift 2"));
    }

    #[test]
    fn wav_validation_strict_mode_rejects_any_sample_drift() {
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: None,
            samples: Some(192_001),
        };
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let err = validate_dvda_wav_probe_with_mode(
            Path::new("track.wav"),
            &probe,
            expectation,
            DvdaDurationValidationMode::Strict,
        )
        .expect_err("strict duration mode should reject any sample drift");

        assert!(err.to_string().contains("strict"));
        assert!(err.to_string().contains("drift 1"));
    }

    #[test]
    fn audio_format_manifest_records_source_carrier_and_final_encode_depths() {
        let temp = tempfile::tempdir().expect("manifest temp dir");
        let wav_path = temp.path().join("track.wav");
        fs::write(&wav_path, b"placeholder").expect("placeholder WAV write");
        let expectation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(1),
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };
        let probe = DvdaWavProbe {
            codec_name: Some("pcm_s32le".to_string()),
            sample_rate: 192_000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            samples: Some(192_000),
        };
        let policy = DvdaRealizationAudioPolicy::new("16-bit PCM".to_string(), Some(16), DvdaDownmixPolicy::None);

        let track = sample_dvda_track_realize_input();
        let metadata = DvdaRealizationMetadata {
            downmix_matrix: Some(3),
            downmix_policy: DvdaDownmixPolicy::None,
            packet_first_cci: Some(0),
            packet_last_cci: Some(0),
            packet_cci_change_count: Some(0),
            packet_cci_evidence: "unit-test packet metadata",
            demux: Some(DvdaDemuxAudit {
                sectors_seen: 11,
                private_stream_1_packets: 7,
                mlp_packets: 7,
                pcm_packets: 0,
                mlp_payload_bytes: 12_345,
                pcm_payload_bytes: 0,
                cyclic_discontinuity_count: 0,
                extra_header_length_change_count: 0,
                nonstandard_mlp_extra_header_packets: 0,
                nonstandard_pcm_extra_header_packets: 0,
            }),
            mlp: Some(DvdaMlpAudit {
                payload_bytes: 12_345,
                frame_count: 100,
                major_sync_frame_count: 2,
                min_frame_bytes: Some(80),
                max_frame_bytes: Some(160),
            }),
            lpcm: None,
        };

        write_dvda_audio_format_manifest(&wav_path, &track, expectation, expectation, &probe, &policy, metadata, "unit-test")
            .expect("audio-format manifest write succeeds");

        let manifest_path = dvda_audio_format_manifest_path(&wav_path);
        let manifest = fs::read_to_string(&manifest_path).expect("manifest should be readable");
        let json: serde_json::Value = serde_json::from_str(&manifest).expect("manifest JSON");
        assert_eq!(json.pointer("/source/bit_depth").and_then(|value| value.as_u64()), Some(24));
        assert_eq!(json.pointer("/carrier/codec").and_then(|value| value.as_str()), Some("pcm_s32le"));
        assert_eq!(json.pointer("/carrier/bit_depth").and_then(|value| value.as_u64()), Some(32));
        assert_eq!(json.pointer("/final_encode/resolved_bit_depth").and_then(|value| value.as_u64()), Some(16));
        assert_eq!(json.pointer("/packet_metadata/first_cci").and_then(|value| value.as_u64()), Some(0));
        assert_eq!(json.pointer("/packet_metadata/cci_change_count").and_then(|value| value.as_u64()), Some(0));
        assert_eq!(json.pointer("/downmix/ifo_matrix").and_then(|value| value.as_u64()), Some(3));
        assert_eq!(json.pointer("/downmix/applied_during_realization").and_then(|value| value.as_bool()), Some(false));
        assert_eq!(json.pointer("/source/channel_order/realized_lpcm_policy").and_then(|value| value.as_str()), Some("preserve-dvd-audio-source-order"));
        assert_eq!(json.pointer("/source/channel_order/dvd_audio_source_order").and_then(|value| value.as_str()), Some("L,R"));
        assert_eq!(json.pointer("/carrier/channel_order").and_then(|value| value.as_str()), Some("L,R"));
        assert_eq!(json.pointer("/schema").and_then(|value| value.as_str()), Some("tonepoet.dvda.realized_audio_format.v6"));
        assert_eq!(json.pointer("/support_boundaries/multi_format_ats/corpus_proof_required").and_then(|value| value.as_str()), Some("TONEPOET_DVDA_PHASE3_REQUIRE_MULTIFORMAT_ATS_CORPUS=1"));
        assert_eq!(json.pointer("/support_boundaries/samg_absolute/directory_tree_supported").and_then(|value| value.as_bool()), Some(false));
        assert_eq!(json.pointer("/volume/kind").and_then(|value| value.as_str()), Some("directory"));
        assert_eq!(json.pointer("/track/group_nr").and_then(|value| value.as_u64()), Some(1));
        assert_eq!(json.pointer("/track/title_nr").and_then(|value| value.as_u64()), Some(2));
        assert_eq!(json.pointer("/track/group_track_ordinal").and_then(|value| value.as_u64()), Some(6));
        assert_eq!(json.pointer("/sectors/ranges/0/first").and_then(|value| value.as_u64()), Some(10));
        assert_eq!(json.pointer("/sectors/ranges/0/last").and_then(|value| value.as_u64()), Some(20));
        assert_eq!(json.pointer("/sectors/aob_parts_touched/0/part_nr").and_then(|value| value.as_u64()), Some(1));
        assert_eq!(json.pointer("/sectors/aob_parts_touched/0/touched_sector_count").and_then(|value| value.as_u64()), Some(11));
        assert_eq!(json.pointer("/demux/mlp_packets").and_then(|value| value.as_u64()), Some(7));
        assert_eq!(json.pointer("/demux/mlp_payload_bytes").and_then(|value| value.as_u64()), Some(12_345));
        assert_eq!(json.pointer("/mlp/frame_count").and_then(|value| value.as_u64()), Some(100));
        assert_eq!(json.pointer("/decoded/sample_rate").and_then(|value| value.as_u64()), Some(192_000));
        assert_eq!(json.pointer("/duration_validation/expected_samples").and_then(|value| value.as_u64()), Some(192_000));
        assert_eq!(json.pointer("/duration_validation/actual_samples").and_then(|value| value.as_u64()), Some(192_000));
        assert_eq!(json.pointer("/duration_validation/drift_samples").and_then(|value| value.as_u64()), Some(0));
    }

    #[test]
    fn dvd_audio_temp_files_are_randomized_within_the_requested_directory() {
        let temp = tempfile::tempdir().expect("DVD-Audio temp dir");
        let (first_path, first_file) = create_unique_dvda_temp_file(
            temp.path(),
            ".dvda-track.",
            ".tmp.mlp",
        )
        .expect("first unique temp file");
        let (second_path, second_file) = create_unique_dvda_temp_file(
            temp.path(),
            ".dvda-track.",
            ".tmp.mlp",
        )
        .expect("second unique temp file");

        drop(first_file);
        drop(second_file);

        assert_ne!(first_path, second_path);
        assert_eq!(first_path.parent(), Some(temp.path()));
        assert_eq!(second_path.parent(), Some(temp.path()));
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn dvd_audio_temp_guard_removes_active_artifact_on_error_path() {
        let temp = tempfile::tempdir().expect("DVD-Audio temp dir");
        let (path, file) = create_unique_dvda_temp_file(
            temp.path(),
            ".dvda-track.",
            ".tmp.mlp",
        )
        .expect("unique temp file");
        drop(file);

        {
            let _guard = DvdaTempPathGuard::new(path.clone());
            assert!(path.exists());
        }

        assert!(!path.exists());
    }

    #[test]
    fn dvd_audio_temp_guard_disarm_transfers_cleanup_responsibility() {
        let temp = tempfile::tempdir().expect("DVD-Audio temp dir");
        let (path, file) = create_unique_dvda_temp_file(
            temp.path(),
            ".dvda-track.",
            ".tmp.wav",
        )
        .expect("unique temp file");
        drop(file);

        {
            let mut guard = DvdaTempPathGuard::new(path.clone());
            guard.disarm();
        }

        assert!(path.exists());
        fs::remove_file(path).expect("manual temp cleanup after disarm");
    }

    #[test]
    fn wav_expectation_uses_stream_facts_when_ifo_scalar_rate_is_unknown() {
        let base = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: None,
            channel_count: None,
            source_bit_depth: None,
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let resolved = base.with_stream_facts(StreamDerivedAudioFacts {
            sample_rate: Some(192_000),
            channel_count: Some(2),
            bit_depth: Some(24),
            channel_assignment_code: Some(1),
            evidence_source: "test stream facts",
        });

        assert_eq!(resolved.sample_rate, Some(192_000));
        assert_eq!(resolved.channel_count, Some(2));
        assert_eq!(resolved.source_bit_depth, Some(24));
        assert_eq!(resolved.channel_assignment_code, Some(1));
        assert!(resolved.can_validate_existing_cache());
    }

    #[test]
    fn wav_expectation_requires_active_facts_before_cache_reuse() {
        let incomplete = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: None,
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };
        let complete = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(192_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: None,
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        assert!(!incomplete.can_validate_existing_cache());
        assert!(complete.can_validate_existing_cache());
    }

    #[test]
    fn prepared_source_audio_expectation_sums_channel_groups() {
        let source_audio = SourceAudioDescriptor {
            coding: Some(SourceAudioCoding::DvdaUnknown),
            primary_sample_rate: Some(192_000),
            bit_depth: Some(24),
            channel_groups: vec![
                ChannelGroupDescriptor {
                    group_nr: 1,
                    channels: Some(2),
                    assignment: Some("front stereo".to_string()),
                    sample_rate: Some(192_000),
                    bit_depth: Some(24),
                },
                ChannelGroupDescriptor {
                    group_nr: 2,
                    channels: Some(4),
                    assignment: Some("surround".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                },
            ],
        };

        assert_eq!(channel_count_from_source_audio(&source_audio), Some(6));
        assert_eq!(source_audio_group_sample_rate(&source_audio, 1), Some(192_000));
        assert_eq!(source_audio_group_sample_rate(&source_audio, 2), Some(96_000));
        assert_eq!(source_audio_group_bit_depth(&source_audio, 1), Some(24));
        assert_eq!(source_audio_group_channel_count(&source_audio, 2), Some(4));
    }

    #[test]
    fn dvda_track_realize_input_carries_ifo_bit_depth() {
        let source = TrackSourceRef::DvdaTrack {
            volume_source: DvdaVolumeSourceRef::Directory { root: PathBuf::from("/tmp/disc") },
            sector_address_space: DvdaSectorAddressSpace::AtsAobRelative { title_set_nr: 1 },
            group_nr: 1,
            title_set_nr: Some(1),
            title_nr: Some(1),
            title_ordinal: Some(1),
            ats_track_nr: Some(1),
            samg_track_nr: None,
            samg_ordinal: None,
            group_track_ordinal: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            track_type: Some(0xa1),
            index_start: Some(1),
            downmix_matrix: None,
            dvda_downmix_policy: DvdaDownmixPolicy::None,
            title_table_offset: None,
            title_len_in_pts: Some(90_000),
            title_track_count_declared: Some(1),
            title_index_count_declared: Some(1),
            audio_format_index: Some(1),
            expected_sample_rate: Some(192_000),
            expected_channel_count: Some(2),
            expected_bit_depth: Some(24),
            expected_channel_assignment_code: Some(1),
            expected_group1_sample_rate: Some(192_000),
            expected_group2_sample_rate: None,
            expected_group1_bit_depth: Some(24),
            expected_group2_bit_depth: None,
            expected_group1_channel_count: Some(2),
            expected_group2_channel_count: None,
            sector_ranges: vec![DvdaSectorRangeRef { index_nr: 1, first: 0, last: 0 }],
            aob_files: vec![DvdaAobFileRef {
                title_set_nr: 1,
                part_nr: 1,
                file_name: "ATS_01_1.AOB".to_string(),
                exists: true,
                byte_len: 2048,
                block_first: 0,
                block_last: 0,
            }],
        };

        let input = DvdaTrackRealizeInput::try_from_source(
            &source,
            DvdaSourceAudioExpectation::from_source_ref(&source),
            DvdaDownmixPolicy::None,
        )
        .expect("DVD-Audio source should map to realize input");
        assert_eq!(input.expected_sample_rate, Some(192_000));
        assert_eq!(input.expected_channel_count, Some(2));
        assert_eq!(input.expected_bit_depth, Some(24));
        assert_eq!(input.expected_channel_assignment_code, Some(1));
        assert_eq!(input.expected_group1_sample_rate, Some(192_000));
        assert_eq!(input.expected_group1_bit_depth, Some(24));
        assert_eq!(input.expected_group1_channel_count, Some(2));
        assert_eq!(input.wav_expectation().source_bit_depth, Some(24));

        let stats = DvdaDemuxStats {
            private_stream_1_packets: 1,
            mlp_packets: 1,
            mlp_payload_bytes: 128,
            first_sub_header: Some(DvdaSubHeader {
                stream_id: MLP_STREAM_ID,
                cyclic: 0,
                extra_header_length: MLP_EXTRA_HEADER_LENGTH,
                total_header_length: 4 + usize::from(MLP_EXTRA_HEADER_LENGTH),
                cci: Some(0),
                pcm: None,
            }),
            ..DvdaDemuxStats::default()
        };
        validate_packet_stream_expectations(&stats, &input, ExpectedElementaryStreamKind::Mlp)
            .expect("packet/IFO validation should accept matching MLP packet facts");
    }

    #[test]
    fn packet_audio_fact_mismatch_fails_by_default() {
        let mut input = sample_dvda_track_realize_input();
        input.track_type = Some(0x02);
        input.audio_format_index = Some(1);
        let stats = DvdaDemuxStats {
            private_stream_1_packets: 1,
            mlp_packets: 1,
            mlp_payload_bytes: 128,
            first_sub_header: Some(DvdaSubHeader {
                stream_id: MLP_STREAM_ID,
                cyclic: 0,
                extra_header_length: MLP_EXTRA_HEADER_LENGTH,
                total_header_length: 4 + usize::from(MLP_EXTRA_HEADER_LENGTH),
                cci: Some(0),
                pcm: None,
            }),
            ..DvdaDemuxStats::default()
        };

        let err = validate_packet_stream_expectations(&stats, &input, ExpectedElementaryStreamKind::Mlp)
            .expect_err("audio-format index mismatch should fail by default");
        assert!(format!("{err}").contains("audio-fact validation failed"));
    }

    #[test]
    fn packet_metadata_anomaly_is_advisory_by_default() {
        let input = sample_dvda_track_realize_input();
        let stats = DvdaDemuxStats {
            private_stream_1_packets: 1,
            mlp_packets: 1,
            mlp_payload_bytes: 128,
            nonstandard_mlp_extra_header_packets: 1,
            first_sub_header: Some(DvdaSubHeader {
                stream_id: MLP_STREAM_ID,
                cyclic: 0,
                extra_header_length: MLP_EXTRA_HEADER_LENGTH + 1,
                total_header_length: 10,
                cci: Some(0),
                pcm: None,
            }),
            ..DvdaDemuxStats::default()
        };

        validate_packet_stream_expectations(&stats, &input, ExpectedElementaryStreamKind::Mlp)
            .expect("metadata-only packet anomaly should be advisory by default");
    }

    #[test]
    fn mlp_ifo_channel_layout_mismatch_fails_by_default() {
        let input = sample_dvda_track_realize_input();
        let inspection = MlpStreamInspection {
            payload_bytes: 80,
            frame_count: 1,
            major_sync_frame_count: 1,
            min_frame_bytes: Some(80),
            max_frame_bytes: Some(80),
            first_major_sync: Some(MlpMajorSyncInfo {
                stream_type: MLP_STREAM_TYPE,
                group1_bits: 24,
                group2_bits: 0,
                group1_sample_rate: 192_000,
                group2_sample_rate: 0,
                channel_arrangement: 20,
                channel_count: 6,
                access_unit_size: 160,
                is_vbr: true,
                peak_bitrate: 0,
                num_substreams: 1,
            }),
            trailing_partial_frame_bytes: None,
        };

        let err = validate_mlp_ifo_cross_checks(Path::new("test.mlp"), &inspection, &input)
            .expect_err("channel-layout mismatch should fail by default");
        assert!(format!("{err}").contains("audio-fact validation failed"));
    }



    #[test]
    fn realization_audio_policy_controls_effective_downmix_policy() {
        let mut source = sample_dvda_track_source();
        if let TrackSourceRef::DvdaTrack {
            dvda_downmix_policy,
            expected_channel_count,
            expected_channel_assignment_code,
            ..
        } = &mut source
        {
            *dvda_downmix_policy = DvdaDownmixPolicy::None;
            *expected_channel_count = Some(2);
            *expected_channel_assignment_code = Some(1);
        }

        let input = DvdaTrackRealizeInput::try_from_source(
            &source,
            DvdaSourceAudioExpectation::from_source_ref(&source),
            DvdaDownmixPolicy::FooInputDvdaCompatible,
        )
        .expect("realization audio policy should provide the effective realized downmix policy");

        assert_eq!(input.expected_channel_count, Some(2));
        assert_eq!(input.dvda_downmix_policy, DvdaDownmixPolicy::FooInputDvdaCompatible);
        assert_eq!(input.mlp_expectation().channel_count, None);
        assert_eq!(input.wav_expectation().channel_count, Some(2));
    }

    #[test]
    fn active_downmix_treats_stereo_ifo_facts_as_presentation_not_mlp_carrier() {
        let mut input = sample_dvda_track_realize_input();
        input.dvda_downmix_policy = DvdaDownmixPolicy::FooInputDvdaCompatible;
        let inspection = MlpStreamInspection {
            payload_bytes: 80,
            frame_count: 1,
            major_sync_frame_count: 1,
            min_frame_bytes: Some(80),
            max_frame_bytes: Some(80),
            first_major_sync: Some(MlpMajorSyncInfo {
                stream_type: MLP_STREAM_TYPE,
                group1_bits: 24,
                group2_bits: 0,
                group1_sample_rate: 192_000,
                group2_sample_rate: 0,
                channel_arrangement: 20,
                channel_count: 6,
                access_unit_size: 160,
                is_vbr: true,
                peak_bitrate: 0,
                num_substreams: 1,
            }),
            trailing_partial_frame_bytes: None,
        };

        assert_eq!(input.expected_channel_count, Some(2));
        assert_eq!(input.mlp_expectation().channel_count, None);
        validate_mlp_ifo_cross_checks(Path::new("test.mlp"), &inspection, &input)
            .expect("active downmix should not compare stereo presentation layout to multichannel MLP carrier layout");

        let source = input
            .source_wav_expectation()
            .with_stream_facts(StreamDerivedAudioFacts {
                sample_rate: Some(192_000),
                channel_count: Some(6),
                bit_depth: Some(24),
                channel_assignment_code: Some(20),
                evidence_source: "MLP major-sync",
            });
        assert_eq!(source.channel_count, Some(6));
        assert_eq!(source.with_downmix_policy(input.dvda_downmix_policy).channel_count, Some(2));

        let mlp_source_channel_count = inspection
            .first_major_sync
            .map(|info| info.channel_count);
        assert_eq!(mlp_source_channel_count, Some(6));
        let mut ffmpeg_args = Vec::new();
        append_downmix_ffmpeg_args(
            &mut ffmpeg_args,
            DvdaDownmixPolicy::FooInputDvdaCompatible,
            mlp_source_channel_count,
        )
        .expect("forced foo-compatible downmix must validate against the MLP carrier, not stereo presentation facts");
        assert!(ffmpeg_args.iter().any(|arg| arg == FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER));
    }


    #[test]
    fn foo_compatible_downmix_ffmpeg_args_use_explicit_pan_filter() {
        let mut args = vec!["-f".to_string(), "mlp".to_string()];
        append_downmix_ffmpeg_args(
            &mut args,
            DvdaDownmixPolicy::FooInputDvdaCompatible,
            Some(6),
        )
        .expect("6-channel MLP carrier should accept foo-compatible downmix");

        assert_eq!(
            args,
            vec![
                "-f".to_string(),
                "mlp".to_string(),
                "-af".to_string(),
                FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER.to_string(),
            ]
        );
    }

    #[test]
    fn foo_compatible_downmix_rejects_known_non_5_1_sources() {
        let mut args = Vec::new();
        let err = append_downmix_ffmpeg_args(
            &mut args,
            DvdaDownmixPolicy::FooInputDvdaCompatible,
            Some(2),
        )
        .expect_err("foo-compatible matrix is defined only for 5.1 carrier input");

        assert!(format!("{err}").contains("requires a 6-channel 5.1 source"));
        assert!(args.is_empty());
    }

    #[test]
    fn ffmpeg_default_downmix_ffmpeg_args_use_ac_2() {
        let mut args = vec!["-map".to_string(), "0:a:0".to_string()];
        append_downmix_ffmpeg_args(
            &mut args,
            DvdaDownmixPolicy::FfmpegDefault,
            Some(6),
        )
        .expect("ffmpeg default downmix should not require a 5.1-specific pan matrix");

        assert_eq!(
            args,
            vec![
                "-map".to_string(),
                "0:a:0".to_string(),
                "-ac".to_string(),
                "2".to_string(),
            ]
        );
    }

    #[test]
    fn active_downmix_rewrites_six_channel_wav_expectation_to_stereo() {
        let native = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(96_000),
            channel_count: Some(6),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(20),
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let downmixed = native.with_downmix_policy(DvdaDownmixPolicy::FooInputDvdaCompatible);
        assert_eq!(downmixed.channel_count, Some(2));
        assert_eq!(downmixed.channel_assignment_code, None);
        assert_eq!(downmixed.sample_rate, Some(96_000));
        assert_eq!(downmixed.source_bit_depth, Some(24));

        let ffmpeg_default = native.with_downmix_policy(DvdaDownmixPolicy::FfmpegDefault);
        assert_eq!(ffmpeg_default.channel_count, Some(2));
    }

    #[test]
    fn ifo_stereo_mlp_5_1_forced_foo_downmix_uses_mlp_carrier_count() {
        let presentation = DvdaWavExpectation {
            len_in_pts: 90_000,
            sample_rate: Some(96_000),
            channel_count: Some(2),
            source_bit_depth: Some(24),
            channel_assignment_code: Some(1),
            channel_order_policy: DvdaChannelOrderPolicy::PreserveDvdAudio,
        };

        let carrier = presentation.with_stream_facts(StreamDerivedAudioFacts {
            sample_rate: Some(96_000),
            channel_count: Some(6),
            bit_depth: Some(24),
            channel_assignment_code: Some(20),
            evidence_source: "MLP major-sync",
        });
        assert_eq!(carrier.channel_count, Some(6));

        let mut args = Vec::new();
        append_downmix_ffmpeg_args(
            &mut args,
            DvdaDownmixPolicy::FooInputDvdaCompatible,
            carrier.channel_count,
        )
        .expect("forced foo-compatible downmix must validate against inspected MLP carrier channels, not IFO presentation channels");
        assert_eq!(args, vec!["-af".to_string(), FOO_INPUT_DVDA_COMPATIBLE_PAN_FILTER.to_string()]);

        let final_expectation = carrier.with_downmix_policy(DvdaDownmixPolicy::FooInputDvdaCompatible);
        assert_eq!(final_expectation.channel_count, Some(2));
        assert_eq!(final_expectation.channel_assignment_code, None);
    }


    #[test]
    fn samg_absolute_realize_input_allows_empty_aob_inventory() {
        let source = TrackSourceRef::DvdaTrack {
            volume_source: DvdaVolumeSourceRef::Iso {
                path: PathBuf::from("/tmp/disc.iso"),
                backend: DvdaIsoBackend::Udf,
            },
            sector_address_space: DvdaSectorAddressSpace::SamgAbsolute,
            group_nr: 1,
            title_set_nr: None,
            title_nr: None,
            title_ordinal: None,
            ats_track_nr: None,
            samg_track_nr: Some(1),
            samg_ordinal: Some(1),
            group_track_ordinal: 1,
            first_pts: 0,
            len_in_pts: 90_000,
            track_type: None,
            index_start: None,
            downmix_matrix: None,
            dvda_downmix_policy: DvdaDownmixPolicy::None,
            title_table_offset: None,
            title_len_in_pts: None,
            title_track_count_declared: None,
            title_index_count_declared: None,
            audio_format_index: None,
            expected_sample_rate: Some(192_000),
            expected_channel_count: Some(2),
            expected_bit_depth: Some(24),
            expected_channel_assignment_code: Some(1),
            expected_group1_sample_rate: Some(192_000),
            expected_group2_sample_rate: None,
            expected_group1_bit_depth: Some(24),
            expected_group2_bit_depth: None,
            expected_group1_channel_count: Some(2),
            expected_group2_channel_count: None,
            sector_ranges: vec![DvdaSectorRangeRef { index_nr: 1, first: 64, last: 65 }],
            aob_files: Vec::new(),
        };

        let input = DvdaTrackRealizeInput::try_from_source(
            &source,
            DvdaSourceAudioExpectation::from_source_ref(&source),
            DvdaDownmixPolicy::None,
        )
        .expect("disc-absolute tracks should not require ATS AOB inventory");

        assert_eq!(input.sector_address_space, DvdaSectorAddressSpace::SamgAbsolute);
        assert!(input.aob_files.is_empty());
    }

    #[test]
    fn samg_absolute_iso_reader_reads_raw_disc_sectors() {
        let temp = tempfile::tempdir().expect("disc-absolute reader temp dir");
        let iso_path = temp.path().join("samg.iso");
        let mut bytes = vec![0_u8; DVD_SECTOR_SIZE * 4];
        bytes[DVD_SECTOR_SIZE * 2..DVD_SECTOR_SIZE * 3].fill(0x5a);
        bytes[DVD_SECTOR_SIZE * 3..DVD_SECTOR_SIZE * 4].fill(0xa5);
        std::fs::write(&iso_path, &bytes).expect("write synthetic raw ISO sectors");

        let mut reader = TrackSectorReader::open_disc_absolute_iso(&DvdaVolumeSourceRef::Iso {
            path: iso_path,
            backend: DvdaIsoBackend::Udf,
        })
        .expect("raw ISO sector reader should open");
        let mut out = vec![0_u8; DVD_SECTOR_SIZE * 2];
        let read = reader
            .read_blocks_into(2, 2, &mut out)
            .expect("disc-absolute sectors should read from raw ISO offsets");

        assert_eq!(read, DVD_SECTOR_SIZE * 2);
        assert!(out[..DVD_SECTOR_SIZE].iter().all(|value| *value == 0x5a));
        assert!(out[DVD_SECTOR_SIZE..].iter().all(|value| *value == 0xa5));
    }

    #[test]
    fn samg_absolute_iso_reader_rejects_out_of_bounds_ranges() {
        let temp = tempfile::tempdir().expect("disc-absolute reader temp dir");
        let iso_path = temp.path().join("samg.iso");
        std::fs::write(&iso_path, vec![0_u8; DVD_SECTOR_SIZE]).expect("write one-sector ISO");

        let mut reader = TrackSectorReader::open_disc_absolute_iso(&DvdaVolumeSourceRef::Iso {
            path: iso_path,
            backend: DvdaIsoBackend::Udf,
        })
        .expect("raw ISO sector reader should open");
        let mut out = vec![0_u8; DVD_SECTOR_SIZE];
        let err = reader
            .read_blocks_into(1, 1, &mut out)
            .expect_err("sector beyond ISO length should fail");

        assert!(err.to_string().contains("exceeds ISO length"));
    }

    #[test]
    fn samg_absolute_directory_source_reports_unresolvable_address_space() {
        let err = TrackSectorReader::open_disc_absolute_iso(&DvdaVolumeSourceRef::Directory {
            root: PathBuf::from("/tmp/AUDIO_TS"),
        })
        .expect_err("directory copies do not preserve absolute disc sector addresses");

        assert!(err.to_string().contains("directory copies"));
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_hdad2009_track_1() {
        run_phase3_corpus_disc(HDAD2009).await;
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_ap_i_robot_track_1() {
        run_phase3_corpus_disc(AP_I_ROBOT).await;
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_ap_friendly_card_track_1() {
        run_phase3_corpus_disc(AP_FRIENDLY_CARD).await;
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_ap_eye_in_the_sky_track_1() {
        run_phase3_corpus_disc(AP_EYE_IN_THE_SKY).await;
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_every_track_from_all_unencrypted_discs() {
        for disc in PHASE3_CORPUS_DISCS {
            run_phase3_corpus_all_tracks_for_disc(disc).await;
        }
    }

    #[tokio::test]
    async fn phase3_corpus_extracts_an_aob_boundary_crossing_track() {
        if !tool_available_or_skip(HDAD2009, "ffmpeg") || !tool_available_or_skip(HDAD2009, "ffprobe") {
            return;
        }

        for disc in PHASE3_CORPUS_DISCS {
            let Some(iso_path) = find_corpus_iso_or_skip(disc) else {
                continue;
            };

            let temp = tempfile::tempdir().expect("DVD-Audio boundary corpus temp dir");
            let staging = StagingDir::borrowed(
                temp.path().join("staging"),
                format!("dvda-phase3-boundary-{}", corpus_slug(disc.label)),
            );
            let runner = RealToolRunner::new(HashMap::<String, PathBuf>::new());
            let cancel = CancellationToken::new();
            let tool_paths = HashMap::<String, PathBuf>::new();
            let req = dvda_corpus_request_for_selection(
                iso_path.clone(),
                temp.path().join("out"),
                temp.path().join("logs"),
                TrackSelection::All,
            );

            let materializer = DvdaAudioMaterializer;
            let prepared = materializer
                .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "{} all-track materialization failed while searching for an AOB boundary-crossing track from {}: {err}",
                        disc.label,
                        iso_path.display()
                    )
                });

            if let Some(track) = prepared.tracks.iter().find(|track| track_crosses_aob_part_boundary(track)) {
                validate_phase3_corpus_track(disc, track);
                let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(
                    Some(track),
                    &track.source_ref,
                );
                let wav_path = realize_dvda_track(
                    &track.source_ref,
                    expectation,
                    DvdaRealizationAudioPolicy::unknown(),
                    &staging,
                    &runner,
                    &cancel,
                    None,
                    None,
                )
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "{} AOB boundary-crossing track {} MLP to WAV realization failed: {err}",
                        disc.label, track.ordinal
                    )
                });
                assert!(wav_path.is_file(), "{} boundary-crossing WAV exists", disc.label);
                let probe = probe_dvda_wav(&wav_path, &runner, &cancel, None)
                    .await
                    .unwrap_or_else(|err| {
                        panic!(
                            "{} boundary-crossing track {} WAV probe failed: {err}",
                            disc.label, track.ordinal
                        )
                    });
                validate_phase3_corpus_wav_probe(disc, track, &probe);
                return;
            }
        }

        panic!(
            "DVD-Audio Phase 3 corpus did not expose any prepared track crossing an AOB part boundary"
        );
    }

    #[tokio::test]
    async fn phase3_extended_corpus_exercises_multiformat_ats_realization_or_declares_gap() {
        exercise_extended_corpus_track(
            "multi-format ATS active-format realization",
            DVDA_REQUIRE_MULTIFORMAT_ATS_CORPUS_ENV,
            |track| track_has_multiformat_ats_evidence(track),
        )
        .await;
    }

    #[tokio::test]
    async fn phase3_extended_corpus_exercises_samg_absolute_iso_realization_or_declares_gap() {
        exercise_extended_corpus_track(
            "disc-absolute sector ISO realization",
            DVDA_REQUIRE_SAMG_ABSOLUTE_CORPUS_ENV,
            |track| track_uses_samg_absolute_iso_sectors(track),
        )
        .await;
    }


    async fn exercise_extended_corpus_track<F>(
        coverage_label: &'static str,
        strict_env: &'static str,
        predicate: F,
    ) where
        F: Fn(&PreparedTrack) -> bool,
    {
        if !tool_available_or_skip_extended(coverage_label, strict_env, "ffmpeg")
            || !tool_available_or_skip_extended(coverage_label, strict_env, "ffprobe")
        {
            return;
        }

        let Some(iso_paths) = find_all_corpus_isos_or_skip_extended(coverage_label, strict_env) else {
            return;
        };

        let runner = RealToolRunner::new(HashMap::<String, PathBuf>::new());
        let cancel = CancellationToken::new();
        let tool_paths = HashMap::<String, PathBuf>::new();

        for (iso_index, iso_path) in iso_paths.iter().enumerate() {
            let temp = tempfile::tempdir().expect("DVD-Audio extended corpus temp dir");
            let slug = iso_path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(corpus_slug)
                .unwrap_or_else(|| format!("iso-{iso_index}"));
            let staging = StagingDir::borrowed(
                temp.path().join("staging"),
                format!("dvda-phase3-extended-{iso_index}-{slug}"),
            );
            let req = dvda_corpus_request_for_selection(
                iso_path.clone(),
                temp.path().join("out"),
                temp.path().join("logs"),
                TrackSelection::All,
            );

            let materializer = DvdaAudioMaterializer;
            let prepared = match materializer
                .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
                .await
            {
                Ok(prepared) => prepared,
                Err(err) => {
                    eprintln!(
                        "skipping DVD-Audio extended corpus candidate {} for {coverage_label}: materialization failed: {err}",
                        iso_path.display()
                    );
                    continue;
                }
            };

            if let Some(track) = prepared.tracks.iter().find(|track| predicate(track)) {
                let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(
                    Some(track),
                    &track.source_ref,
                );
                let wav_path = realize_dvda_track(
                    &track.source_ref,
                    expectation,
                    DvdaRealizationAudioPolicy::unknown(),
                    &staging,
                    &runner,
                    &cancel,
                    None,
                    None,
                )
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "DVD-Audio extended corpus {coverage_label} failed while realizing {} track {}: {err}",
                        iso_path.display(),
                        track.ordinal
                    )
                });
                let probe = probe_dvda_wav(&wav_path, &runner, &cancel, None)
                    .await
                    .unwrap_or_else(|err| {
                        panic!(
                            "DVD-Audio extended corpus {coverage_label} WAV probe failed for {} track {}: {err}",
                            iso_path.display(),
                            track.ordinal
                        )
                    });
                assert_eq!(
                    probe.codec_name.as_deref(),
                    Some("pcm_s32le"),
                    "DVD-Audio extended corpus {coverage_label} WAV codec"
                );
                assert!(probe.sample_rate > 0, "DVD-Audio extended corpus {coverage_label} sample rate");
                assert!(probe.channels > 0, "DVD-Audio extended corpus {coverage_label} channel count");
                assert!(
                    probe.samples.unwrap_or_default() > 0,
                    "DVD-Audio extended corpus {coverage_label} decoded sample count"
                );
                return;
            }
        }

        skip_or_panic_extended(
            coverage_label,
            strict_env,
            format!(
                "no ISO under {} materialized a track matching this coverage predicate; add a real disc image to the extended DVD-Audio corpus or unset {strict_env}/{DVDA_EXTENDED_CORPUS_STRICT_ENV}",
                corpus_root().display()
            ),
        );
    }

    fn track_has_multiformat_ats_evidence(track: &PreparedTrack) -> bool {
        if !matches!(
            &track.source_ref,
            TrackSourceRef::DvdaTrack {
                sector_address_space: DvdaSectorAddressSpace::AtsAobRelative { .. },
                ..
            }
        ) {
            return false;
        }

        matches!(
            track.metadata.extra.get("dvda_audio_format_resolution").map(String::as_str),
            Some("track_type_audio_format_index" | "multiple_present_formats_unknown_until_aob_demux")
        )
    }

    fn track_uses_samg_absolute_iso_sectors(track: &PreparedTrack) -> bool {
        matches!(
            &track.source_ref,
            TrackSourceRef::DvdaTrack {
                volume_source: DvdaVolumeSourceRef::Iso { .. },
                sector_address_space: DvdaSectorAddressSpace::SamgAbsolute,
                ..
            }
        )
    }

    async fn run_phase3_corpus_disc(disc: CorpusDisc) {
        if !tool_available_or_skip(disc, "ffmpeg") || !tool_available_or_skip(disc, "ffprobe") {
            return;
        }

        let Some(iso_path) = find_corpus_iso_or_skip(disc) else {
            return;
        };

        let temp = tempfile::tempdir().expect("DVD-Audio corpus temp dir");
        let staging = StagingDir::borrowed(
            temp.path().join("staging"),
            format!("dvda-phase3-corpus-{}", corpus_slug(disc.label)),
        );
        let runner = RealToolRunner::new(HashMap::<String, PathBuf>::new());
        let cancel = CancellationToken::new();
        let tool_paths = HashMap::<String, PathBuf>::new();
        let req = dvda_corpus_request(
            iso_path.clone(),
            temp.path().join("out"),
            temp.path().join("logs"),
        );

        let materializer = DvdaAudioMaterializer;
        let prepared = materializer
            .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "{} track 1 materialization failed from {}: {err}",
                    disc.label,
                    iso_path.display()
                )
            });

        assert_eq!(
            prepared.tracks.len(),
            1,
            "{} should materialize exactly track 1 from {}",
            disc.label,
            iso_path.display()
        );
        let track = prepared.tracks.first().expect("selected track exists");
        assert_eq!(track.sample_rate, Some(192_000), "{} IFO sample rate", disc.label);
        assert_eq!(track.source_audio.primary_sample_rate, Some(192_000), "{} typed source sample rate", disc.label);
        assert_eq!(track.bit_depth, Some(24), "{} IFO bit depth", disc.label);
        assert_eq!(track.source_audio.bit_depth, Some(24), "{} typed source bit depth", disc.label);
        assert_eq!(source_audio_channels(track), Some(2), "{} IFO channel count", disc.label);

        let TrackSourceRef::DvdaTrack {
            sector_address_space,
            sector_ranges,
            aob_files,
            len_in_pts,
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
            ..
        } = &track.source_ref
        else {
            panic!("{} track 1 should materialize as TrackSourceRef::DvdaTrack", disc.label);
        };
        assert!(
            matches!(sector_address_space, DvdaSectorAddressSpace::AtsAobRelative { .. }),
            "{} track 1 should use ATS-relative AOB sector reads",
            disc.label
        );
        assert!(!sector_ranges.is_empty(), "{} track 1 sector ranges", disc.label);
        assert!(!aob_files.is_empty(), "{} track 1 AOB inventory", disc.label);
        assert!(*len_in_pts > 0, "{} track 1 should carry PTS duration", disc.label);
        assert_eq!(*expected_sample_rate, Some(192_000), "{} source-ref expected sample rate", disc.label);
        assert_eq!(*expected_channel_count, Some(2), "{} source-ref expected channel count", disc.label);
        assert_eq!(*expected_bit_depth, Some(24), "{} source-ref expected bit depth", disc.label);

        let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(Some(&track), &track.source_ref);
        let wav_path = realize_dvda_track(&track.source_ref, expectation, DvdaRealizationAudioPolicy::unknown(), &staging, &runner, &cancel, None, None)
            .await
            .unwrap_or_else(|err| panic!("{} track 1 MLP to WAV realization failed: {err}", disc.label));
        assert!(wav_path.is_file(), "{} realized WAV exists", disc.label);

        let probe = probe_dvda_wav(&wav_path, &runner, &cancel, None)
            .await
            .unwrap_or_else(|err| panic!("{} realized WAV probe failed: {err}", disc.label));
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s32le"), "{} WAV codec", disc.label);
        assert_eq!(probe.sample_rate, 192_000, "{} WAV sample rate", disc.label);
        assert_eq!(probe.channels, 2, "{} WAV channels", disc.label);

        let actual_samples = probe
            .samples
            .unwrap_or_else(|| panic!("{} ffprobe should report WAV duration", disc.label));
        let expected_samples = pts_to_samples(*len_in_pts, 192_000);
        assert!(
            actual_samples.abs_diff(expected_samples) <= 192_000,
            "{} WAV duration drift exceeds one second: expected {expected_samples}, got {actual_samples}",
            disc.label
        );

        let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(Some(&track), &track.source_ref);
        let second_wav_path = realize_dvda_track(&track.source_ref, expectation, DvdaRealizationAudioPolicy::unknown(), &staging, &runner, &cancel, None, None)
            .await
            .unwrap_or_else(|err| panic!("{} second realization failed: {err}", disc.label));
        assert_eq!(second_wav_path, wav_path, "{} realization should reuse the staged WAV", disc.label);
    }

    async fn run_phase3_corpus_all_tracks_for_disc(disc: CorpusDisc) {
        if !tool_available_or_skip(disc, "ffmpeg") || !tool_available_or_skip(disc, "ffprobe") {
            return;
        }

        let Some(iso_path) = find_corpus_iso_or_skip(disc) else {
            return;
        };

        let temp = tempfile::tempdir().expect("DVD-Audio all-track corpus temp dir");
        let staging = StagingDir::borrowed(
            temp.path().join("staging"),
            format!("dvda-phase3-all-tracks-{}", corpus_slug(disc.label)),
        );
        let runner = RealToolRunner::new(HashMap::<String, PathBuf>::new());
        let cancel = CancellationToken::new();
        let tool_paths = HashMap::<String, PathBuf>::new();
        let req = dvda_corpus_request_for_selection(
            iso_path.clone(),
            temp.path().join("out"),
            temp.path().join("logs"),
            TrackSelection::All,
        );

        let materializer = DvdaAudioMaterializer;
        let prepared = materializer
            .materialize(&req, &staging, &runner, None, &tool_paths, &cancel)
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "{} all-track materialization failed from {}: {err}",
                    disc.label,
                    iso_path.display()
                )
            });

        assert_eq!(
            prepared.tracks.len(),
            disc.expected_tracks,
            "{} should materialize every expected unencrypted DVD-Audio track from {}",
            disc.label,
            iso_path.display()
        );

        let mut realized_ordinals = BTreeSet::new();
        for track in &prepared.tracks {
            validate_phase3_corpus_track(disc, track);
            let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(
                Some(track),
                &track.source_ref,
            );
            let wav_path = realize_dvda_track(
                &track.source_ref,
                expectation,
                DvdaRealizationAudioPolicy::unknown(),
                &staging,
                &runner,
                &cancel,
                None,
                None,
            )
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "{} track {} MLP to WAV realization failed: {err}",
                    disc.label, track.ordinal
                )
            });
            assert!(
                wav_path.is_file(),
                "{} track {} realized WAV exists",
                disc.label,
                track.ordinal
            );
            let probe = probe_dvda_wav(&wav_path, &runner, &cancel, None)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "{} track {} realized WAV probe failed: {err}",
                        disc.label, track.ordinal
                    )
                });
            validate_phase3_corpus_wav_probe(disc, track, &probe);
            assert!(
                realized_ordinals.insert(track.ordinal),
                "{} duplicate prepared track ordinal {}",
                disc.label,
                track.ordinal
            );
        }
    }

    fn dvda_corpus_request(container: PathBuf, output_root: PathBuf, log_root: PathBuf) -> PipelineRequest {
        let mut selected_tracks = BTreeSet::new();
        selected_tracks.insert(1);
        dvda_corpus_request_for_selection(
            container,
            output_root,
            log_root,
            TrackSelection::Set(selected_tracks),
        )
    }

    fn dvda_corpus_request_for_selection(
        container: PathBuf,
        output_root: PathBuf,
        log_root: PathBuf,
        track_selection: TrackSelection,
    ) -> PipelineRequest {
        PipelineRequest {
            job_id: "dvda-phase3-corpus".to_string(),
            item_id: "dvda-phase3-corpus".to_string(),
            container,
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: Some(1),
            merge: false,
            output_root,
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
                root: log_root,
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

    fn validate_phase3_corpus_track(
        disc: CorpusDisc,
        track: &super::super::types::PreparedTrack,
    ) {
        assert_eq!(track.sample_rate, Some(192_000), "{} track {} IFO sample rate", disc.label, track.ordinal);
        assert_eq!(
            track.source_audio.primary_sample_rate,
            Some(192_000),
            "{} track {} typed source sample rate",
            disc.label,
            track.ordinal
        );
        assert_eq!(track.bit_depth, Some(24), "{} track {} IFO bit depth", disc.label, track.ordinal);
        assert_eq!(
            track.source_audio.bit_depth,
            Some(24),
            "{} track {} typed source bit depth",
            disc.label,
            track.ordinal
        );
        assert_eq!(source_audio_channels(track), Some(2), "{} track {} IFO channel count", disc.label, track.ordinal);

        let TrackSourceRef::DvdaTrack {
            sector_address_space,
            sector_ranges,
            aob_files,
            len_in_pts,
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
            ..
        } = &track.source_ref
        else {
            panic!(
                "{} track {} should materialize as TrackSourceRef::DvdaTrack",
                disc.label, track.ordinal
            );
        };
        assert!(
            matches!(sector_address_space, DvdaSectorAddressSpace::AtsAobRelative { .. }),
            "{} track {} should use ATS-relative AOB sector reads",
            disc.label,
            track.ordinal
        );
        assert!(!sector_ranges.is_empty(), "{} track {} sector ranges", disc.label, track.ordinal);
        assert!(!aob_files.is_empty(), "{} track {} AOB inventory", disc.label, track.ordinal);
        assert!(*len_in_pts > 0, "{} track {} should carry PTS duration", disc.label, track.ordinal);
        assert_eq!(*expected_sample_rate, Some(192_000), "{} track {} source-ref expected sample rate", disc.label, track.ordinal);
        assert_eq!(*expected_channel_count, Some(2), "{} track {} source-ref expected channel count", disc.label, track.ordinal);
        assert_eq!(*expected_bit_depth, Some(24), "{} track {} source-ref expected bit depth", disc.label, track.ordinal);
        assert!(expected_channel_assignment_code.is_some(), "{} track {} source-ref channel assignment code", disc.label, track.ordinal);
        assert_eq!(*expected_group1_sample_rate, Some(192_000), "{} track {} source-ref group 1 sample rate", disc.label, track.ordinal);
        assert_eq!(*expected_group1_bit_depth, Some(24), "{} track {} source-ref group 1 bit depth", disc.label, track.ordinal);
        assert_eq!(*expected_group1_channel_count, Some(2), "{} track {} source-ref group 1 channels", disc.label, track.ordinal);
        assert!(expected_group2_sample_rate.is_none(), "{} track {} source-ref group 2 sample rate should be absent for stereo corpus", disc.label, track.ordinal);
        assert!(expected_group2_bit_depth.is_none(), "{} track {} source-ref group 2 bit depth should be absent for stereo corpus", disc.label, track.ordinal);
        assert!(expected_group2_channel_count.is_none(), "{} track {} source-ref group 2 channels should be absent for stereo corpus", disc.label, track.ordinal);
    }

    fn validate_phase3_corpus_wav_probe(
        disc: CorpusDisc,
        track: &super::super::types::PreparedTrack,
        probe: &DvdaWavProbe,
    ) {
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s32le"), "{} track {} WAV codec", disc.label, track.ordinal);
        assert_eq!(probe.sample_rate, 192_000, "{} track {} WAV sample rate", disc.label, track.ordinal);
        assert_eq!(probe.channels, 2, "{} track {} WAV channels", disc.label, track.ordinal);

        let TrackSourceRef::DvdaTrack { len_in_pts, .. } = &track.source_ref else {
            panic!("{} track {} should materialize as TrackSourceRef::DvdaTrack", disc.label, track.ordinal);
        };
        let actual_samples = probe
            .samples
            .unwrap_or_else(|| panic!("{} track {} ffprobe should report WAV duration", disc.label, track.ordinal));
        let expected_samples = pts_to_samples(*len_in_pts, 192_000);
        assert!(
            actual_samples.abs_diff(expected_samples) <= 192_000,
            "{} track {} WAV duration drift exceeds one second: expected {expected_samples}, got {actual_samples}",
            disc.label,
            track.ordinal
        );
    }

    fn track_crosses_aob_part_boundary(track: &super::super::types::PreparedTrack) -> bool {
        let TrackSourceRef::DvdaTrack {
            sector_ranges,
            aob_files,
            ..
        } = &track.source_ref
        else {
            return false;
        };

        sector_ranges.iter().any(|range| {
            aob_files.iter().any(|aob| {
                aob.exists
                    && aob.block_last >= range.first
                    && aob.block_last < range.last
                    && aob_files.iter().any(|next| {
                        next.exists
                            && next.part_nr != aob.part_nr
                            && next.block_first == aob.block_last.saturating_add(1)
                            && next.block_first <= range.last
                    })
            })
        })
    }

    fn source_audio_channels(track: &super::super::types::PreparedTrack) -> Option<u32> {
        let channels = track
            .source_audio
            .channel_groups
            .iter()
            .filter_map(|group| group.channels)
            .map(u32::from)
            .sum::<u32>();
        (channels > 0).then_some(channels)
    }

    fn tool_available_or_skip(disc: CorpusDisc, tool: &str) -> bool {
        if executable_on_path(tool) {
            true
        } else {
            skip_or_panic(
                disc,
                format!("{tool} is not available on PATH; required for DVD-Audio Phase 3 corpus extraction"),
            )
        }
    }

    fn tool_available_or_skip_extended(label: &'static str, strict_env: &'static str, tool: &str) -> bool {
        if executable_on_path(tool) {
            true
        } else {
            skip_or_panic_extended(
                label,
                strict_env,
                format!("{tool} is not available on PATH; required for DVD-Audio extended corpus realization"),
            )
        }
    }

    fn find_all_corpus_isos_or_skip_extended(
        label: &'static str,
        strict_env: &'static str,
    ) -> Option<Vec<PathBuf>> {
        let root = corpus_root();
        if !root.is_dir() {
            skip_or_panic_extended(
                label,
                strict_env,
                format!(
                    "DVD-Audio corpus directory does not exist: {}; set {DVDA_CORPUS_DIR_ENV} or create {}",
                    root.display(),
                    DEFAULT_DVDA_CORPUS_DIR
                ),
            );
            return None;
        }

        let mut matches = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
                {
                    matches.push(path);
                }
            }
        }
        matches.sort();

        if matches.is_empty() {
            skip_or_panic_extended(
                label,
                strict_env,
                format!("no ISO files found under {}", root.display()),
            );
            None
        } else {
            Some(matches)
        }
    }

    fn skip_or_panic_extended(label: &'static str, strict_env: &'static str, reason: String) -> bool {
        if extended_corpus_strict(strict_env) {
            panic!("DVD-Audio extended corpus test for {label} cannot run: {reason}");
        }
        eprintln!("skipping DVD-Audio extended corpus test for {label}: {reason}");
        false
    }

    fn extended_corpus_strict(strict_env: &'static str) -> bool {
        env_flag(strict_env) || env_flag(DVDA_EXTENDED_CORPUS_STRICT_ENV)
    }

    fn env_flag(name: &str) -> bool {
        let value = env::var(name).unwrap_or_default().to_ascii_lowercase();
        matches!(value.as_str(), "1" | "true" | "yes" | "on")
    }

    fn executable_on_path(name: &str) -> bool {
        let Some(paths) = env::var_os("PATH") else {
            return false;
        };
        env::split_paths(&paths).any(|dir| {
            let bare = dir.join(name);
            let suffixed = if env::consts::EXE_SUFFIX.is_empty() {
                bare.clone()
            } else {
                dir.join(format!("{name}{}", env::consts::EXE_SUFFIX))
            };
            bare.is_file() || suffixed.is_file()
        })
    }

    fn find_corpus_iso_or_skip(disc: CorpusDisc) -> Option<PathBuf> {
        let root = corpus_root();
        if !root.is_dir() {
            skip_or_panic(
                disc,
                format!(
                    "DVD-Audio Phase 3 corpus directory does not exist: {}; set {DVDA_CORPUS_DIR_ENV} or create {}",
                    root.display(),
                    DEFAULT_DVDA_CORPUS_DIR
                ),
            );
            return None;
        }

        let mut matches = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if iso_name_matches(&path, disc.filename_needles) {
                    matches.push(path);
                }
            }
        }
        matches.sort();

        match matches.len() {
            0 => {
                skip_or_panic(
                    disc,
                    format!(
                        "no matching ISO under {} for filename needles {:?}",
                        root.display(),
                        disc.filename_needles
                    ),
                );
                None
            }
            1 => matches.into_iter().next(),
            _ => {
                let selected = matches[0].clone();
                eprintln!(
                    "DVD-Audio Phase 3 corpus found multiple candidates for {}; using {}",
                    disc.label,
                    selected.display()
                );
                Some(selected)
            }
        }
    }

    fn corpus_root() -> PathBuf {
        match env::var_os(DVDA_CORPUS_DIR_ENV) {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => PathBuf::from(DEFAULT_DVDA_CORPUS_DIR),
        }
    }

    fn iso_name_matches(path: &Path, needles: &[&str]) -> bool {
        if !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
        {
            return false;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let lower = name.to_ascii_lowercase();
        needles.iter().all(|needle| lower.contains(needle))
    }

    fn skip_or_panic(disc: CorpusDisc, reason: String) -> bool {
        if corpus_strict() {
            panic!("DVD-Audio Phase 3 corpus test for {} cannot run: {reason}", disc.label);
        }
        eprintln!("skipping DVD-Audio Phase 3 corpus test for {}: {reason}", disc.label);
        false
    }

    fn corpus_strict() -> bool {
        env_flag(DVDA_CORPUS_STRICT_ENV)
    }

    fn corpus_slug(label: &str) -> String {
        label
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
            .collect()
    }

    #[test]
    fn sector_semantic_validation_precedes_payload_commit() {
        use super::super::dvda_demux::DvdaPcmSubHeader;

        let mlp_payload = [0x11, 0x22, 0x33];
        let pcm_payload = [0x44, 0x55, 0x66];
        let packets = [
            DvdaPs1Packet {
                sub_header: DvdaSubHeader {
                    stream_id: MLP_STREAM_ID,
                    cyclic: 0,
                    extra_header_length: MLP_EXTRA_HEADER_LENGTH,
                    total_header_length: 4 + usize::from(MLP_EXTRA_HEADER_LENGTH),
                    cci: Some(0),
                    pcm: None,
                },
                payload: &mlp_payload,
            },
            DvdaPs1Packet {
                sub_header: DvdaSubHeader {
                    stream_id: PCM_STREAM_ID,
                    cyclic: 1,
                    extra_header_length: PCM_EXTRA_HEADER_LENGTH,
                    total_header_length: 4 + usize::from(PCM_EXTRA_HEADER_LENGTH),
                    cci: Some(0),
                    pcm: Some(DvdaPcmSubHeader {
                        first_audio_frame: 0,
                        group1_bits_code: 2,
                        group2_bits_code: 0,
                        group1_sample_rate_code: 2,
                        group2_sample_rate_code: 0,
                        group1_bits: Some(24),
                        group2_bits: Some(16),
                        group1_sample_rate: Some(192_000),
                        group2_sample_rate: Some(48_000),
                        channel_assignment: 0,
                        cci: 0,
                    }),
                },
                payload: &pcm_payload,
            },
        ];

        let mut out = Vec::new();
        let result = (|| -> Result<(), DvdaDemuxError> {
            let sector_kind = classify_sector_elementary_stream(&packets, None, 42)?;
            if matches!(sector_kind, Some(ExpectedElementaryStreamKind::Mlp)) {
                write_mlp_sector_payload(&packets, &mut out)?;
            }
            Ok(())
        })();

        assert!(result.is_err());
        assert!(out.is_empty(), "mixed-sector semantic failure must not commit payload bytes");
    }

    #[test]
    fn sector_stream_kind_mismatch_precedes_payload_commit() {
        let payload = [0xAA, 0xBB, 0xCC];
        let packets = [DvdaPs1Packet {
            sub_header: DvdaSubHeader {
                stream_id: MLP_STREAM_ID,
                cyclic: 0,
                extra_header_length: MLP_EXTRA_HEADER_LENGTH,
                total_header_length: 4 + usize::from(MLP_EXTRA_HEADER_LENGTH),
                cci: Some(0),
                pcm: None,
            },
            payload: &payload,
        }];

        let mut out = Vec::new();
        let result = (|| -> Result<(), DvdaDemuxError> {
            let sector_kind = classify_sector_elementary_stream(
                &packets,
                Some(ExpectedElementaryStreamKind::Lpcm),
                43,
            )?;
            if matches!(sector_kind, Some(ExpectedElementaryStreamKind::Mlp)) {
                write_mlp_sector_payload(&packets, &mut out)?;
            }
            Ok(())
        })();

        assert!(result.is_err());
        assert!(out.is_empty(), "cross-sector semantic failure must not commit payload bytes");
    }

}

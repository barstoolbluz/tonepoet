//! Blu-ray track realization orchestration.
//!
//! Phase 3 keeps high-level control flow here: reject Phase 4 compressed codecs,
//! open libbluray, seek the selected chapter, stream TS bytes through the
//! selected-PID demuxer, pass complete PES packets to the LPCM extractor, then
//! validate and atomically publish the temporary WAV. Low-level TS/PES, PTS,
//! LPCM, and WAV details live in focused sibling modules.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use crate::disc::bluray_backend::{BluRayAudioCoding, BlurayBackend, BlurayDisplayAngle};
use crate::disc::bluray_backend_libbluray::BlurayBackendLibbluray;
use crate::disc::bluray_utils::bluray_source_path_for_backend;

use super::bluray_lpcm::{
    validate_bluray_lpcm_materialization_facts, BlurayLpcmExtractionConfig, BlurayLpcmExtractor,
    PesProcessingOutcome,
};
use super::bluray_pts::{build_pts_mapper, prepare_pts_mapper_for_realization};
use super::bluray_ts_demux::{
    find_next_ts_sync_at_cadence_with_format, M2TS_PACKET_SIZE, M2TS_TP_EXTRA_SIZE,
    SelectedPidPesDemuxer, TsPacketFormat, TS_PACKET_SIZE, TS_RESYNC_CONFIRMATION_PACKETS,
};
use super::bluray_wav_validate::{
    read_wav_info, rewrite_wav_header, validate_bluray_lpcm_wav, write_wav_header,
};
use super::errors::ConvertError;
use super::progress::OperationProgressTracker;
use super::tool::ToolRunner;
use super::track_executor::ToolConcurrencyLimits;
use super::types::{StagingDir, TrackSourceRef};

const BLURAY_READ_PACKET_COUNT: usize = 2048;
const BLURAY_READ_CHUNK_BYTES: usize = TS_PACKET_SIZE * BLURAY_READ_PACKET_COUNT;
const TS_FORMAT_DETECTION_CONFIRMATION_PACKETS: usize = 4;
const TS_FORMAT_DETECTION_MIN_BYTES: usize = M2TS_TP_EXTRA_SIZE
    + (TS_FORMAT_DETECTION_CONFIRMATION_PACKETS - 1) * M2TS_PACKET_SIZE
    + 1;

static BLURAY_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn sync_confirmation_bytes_required(format: TsPacketFormat, confirmations: usize) -> usize {
    debug_assert!(confirmations > 0);
    format.sync_byte_offset() + (confirmations - 1) * format.packet_size() + 1
}

fn has_sync_cadence_at_start(
    buf: &[u8],
    format: TsPacketFormat,
    confirmations: usize,
) -> bool {
    if buf.len() < sync_confirmation_bytes_required(format, confirmations) {
        return false;
    }

    (0..confirmations).all(|i| {
        let sync_index = i * format.packet_size() + format.sync_byte_offset();
        buf[sync_index] == 0x47
    })
}

fn detect_ts_packet_format(buf: &[u8]) -> Result<TsPacketFormat, ConvertError> {
    if has_sync_cadence_at_start(
        buf,
        TsPacketFormat::M2ts,
        TS_FORMAT_DETECTION_CONFIRMATION_PACKETS,
    ) {
        return Ok(TsPacketFormat::M2ts);
    }

    if has_sync_cadence_at_start(
        buf,
        TsPacketFormat::StandardTs,
        TS_FORMAT_DETECTION_CONFIRMATION_PACKETS,
    ) {
        return Ok(TsPacketFormat::StandardTs);
    }

    if find_next_ts_sync_at_cadence_with_format(buf, TsPacketFormat::M2ts).is_some() {
        return Ok(TsPacketFormat::M2ts);
    }
    if find_next_ts_sync_at_cadence_with_format(buf, TsPacketFormat::StandardTs).is_some() {
        return Ok(TsPacketFormat::StandardTs);
    }

    Err(ConvertError::TrackValidation(
        "Blu-ray title stream does not contain recognizable 188-byte TS or 192-byte M2TS packet sync"
            .to_string(),
    ))
}

fn eof_buffer_is_compatible_with_format(buf: &[u8], format: TsPacketFormat) -> bool {
    let packet_size = format.packet_size();
    if buf.is_empty() || buf.len() % packet_size != 0 {
        return false;
    }

    let complete_packets = buf.len() / packet_size;
    (0..complete_packets).all(|i| {
        let sync_index = i * packet_size + format.sync_byte_offset();
        buf[sync_index] == 0x47
    })
}

fn detect_ts_packet_format_at_eof(buf: &[u8]) -> Result<TsPacketFormat, ConvertError> {
    detect_ts_packet_format(buf).or_else(|_| {
        if eof_buffer_is_compatible_with_format(buf, TsPacketFormat::M2ts) {
            return Ok(TsPacketFormat::M2ts);
        }
        if eof_buffer_is_compatible_with_format(buf, TsPacketFormat::StandardTs) {
            return Ok(TsPacketFormat::StandardTs);
        }

        Err(ConvertError::TrackValidation(
            "Blu-ray title stream does not contain recognizable 188-byte TS or 192-byte M2TS packet sync"
                .to_string(),
        ))
    })
}

fn ts_resync_retention_bytes(format: TsPacketFormat) -> usize {
    format.packet_size() * (TS_RESYNC_CONFIRMATION_PACKETS - 1) + format.sync_byte_offset() + 1
}

pub async fn realize_bluray_track(
    src: &TrackSourceRef,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
    _progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<PathBuf, ConvertError> {
    let _ = (runner, tool_concurrency_limits);

    let TrackSourceRef::BluRayTrack {
        source,
        playlist_number,
        title_index,
        angle_number,
        chapter_number,
        chapter_start_pts_90k,
        chapter_end_pts_90k,
        audio_pid,
        audio_stream_index,
        audio_coding,
        sample_rate,
        bit_depth,
        channels,
        channel_layout,
    } = src
    else {
        return Err(ConvertError::Realize(
            "realize_bluray_track called with non-Blu-ray source".to_string(),
        ));
    };

    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    if *audio_coding != BluRayAudioCoding::Lpcm {
        return Err(ConvertError::Realize(format!(
            concat!(
                "Blu-ray compressed audio streams are not yet implemented; ",
                "selected {} stream {} PID 0x{:04x} requires decoder support before WAV realization"
            ),
            audio_coding.label(),
            u16::from(*audio_stream_index) + 1,
            audio_pid
        )));
    }

    validate_bluray_lpcm_materialization_facts(
        source,
        *playlist_number,
        *chapter_number,
        *audio_pid,
        *sample_rate,
        *bit_depth,
        *channels,
    )?;

    let realized_dir = staging.root.join("bluray-realized");
    fs::create_dir_all(&realized_dir).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to create Blu-ray realization directory '{}': {err}",
            realized_dir.display()
        ))
    })?;

    let stem = bluray_output_stem(
        source,
        *playlist_number,
        *title_index,
        *angle_number,
        *chapter_number,
        *chapter_start_pts_90k,
        *chapter_end_pts_90k,
        *audio_pid,
        *audio_stream_index,
        *audio_coding,
    );
    let wav_path = realized_dir.join(format!("{stem}.wav"));

    realize_lpcm_from_libbluray_title(
        source,
        &wav_path,
        *playlist_number,
        *title_index,
        *angle_number,
        *chapter_number,
        *chapter_start_pts_90k,
        *chapter_end_pts_90k,
        *audio_pid,
        *audio_stream_index,
        *sample_rate,
        *bit_depth,
        *channels,
        channel_layout.as_deref(),
        cancel,
    )?;

    validate_nonempty_wav(&wav_path)?;
    Ok(wav_path)
}

fn realize_lpcm_from_libbluray_title(
    source: &Path,
    wav_path: &Path,
    playlist_number: u32,
    materialized_title_index: usize,
    angle_number: u8,
    chapter_number: u32,
    chapter_start_pts_90k: u64,
    chapter_end_pts_90k: Option<u64>,
    audio_pid: u16,
    audio_stream_index: u8,
    expected_sample_rate: Option<u32>,
    expected_bit_depth: Option<u32>,
    expected_channels: Option<u8>,
    expected_channel_layout: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let backend_source = bluray_source_path_for_backend(source).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "Blu-ray source '{}' is not usable by the libbluray backend: {err}",
            source.display()
        ))
    })?;
    let disc = BlurayBackendLibbluray::open(&backend_source).map_err(|err| {
        ConvertError::Realize(format!(
            "Blu-ray open failed for '{}': {err}",
            backend_source.display()
        ))
    })?;
    let title = BlurayBackendLibbluray::title_by_playlist(&disc, playlist_number).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "Blu-ray playlist {playlist_number:05} from materialized source could not be reopened: {err}"
        ))
    })?;
    if title.title_index() as usize != materialized_title_index {
        log::warn!(
            "Blu-ray playlist {playlist_number:05} title index changed from materialized index {} to reopened index {}; using playlist identity",
            materialized_title_index,
            title.title_index()
        );
    }

    let display_angle = BlurayDisplayAngle::new(angle_number).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "Blu-ray playlist {playlist_number:05} angle {angle_number} is invalid: {err}"
        ))
    })?;
    let mut title_source = BlurayBackendLibbluray::open_title(&disc, title, display_angle, None)
        .map_err(|err| {
            ConvertError::Realize(format!(
                "failed to open Blu-ray playlist {playlist_number:05} angle {angle_number}: {err}"
            ))
        })?;

    let title_info = disc.title_info(title.title_index()).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "failed to re-read Blu-ray playlist {playlist_number:05} title metadata before realization: {err}"
        ))
    })?;
    let pts_mapper = build_pts_mapper(&disc, title, &mut title_source)
        .and_then(|mapper| prepare_pts_mapper_for_realization(mapper, title_info.clip_count, playlist_number))?;
    pts_mapper.prime_for_title_pts(chapter_start_pts_90k);

    if chapter_number == 0 {
        return Err(ConvertError::TrackValidation(
            "Blu-ray chapter numbers are one-based".to_string(),
        ));
    }
    let libbluray_chapter_index = chapter_number - 1;
    title_source.seek_chapter(libbluray_chapter_index).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to seek Blu-ray playlist {playlist_number:05} to chapter {chapter_number}: {err}"
        ))
    })?;

    let mut tmp = ScopedTempPath::for_final(wav_path, "lpcm-wav")?;
    let tmp_path = tmp.path().to_path_buf();
    let mut output = File::create(&tmp_path).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to create Blu-ray LPCM WAV '{}': {err}",
            tmp_path.display()
        ))
    })?;

    let mut extractor = BlurayLpcmExtractor::new(BlurayLpcmExtractionConfig {
        source: source.to_path_buf(),
        playlist_number,
        chapter_number,
        chapter_start_pts_90k,
        chapter_end_pts_90k,
        audio_pid,
        audio_stream_index,
        expected_sample_rate,
        expected_bit_depth,
        expected_channels,
        expected_channel_layout: expected_channel_layout.map(str::to_owned),
        pts_mapper,
    });

    let mut wav_header_written = false;
    let mut read_buffer = vec![0u8; BLURAY_READ_CHUNK_BYTES];
    let mut packet_buffer =
        Vec::<u8>::with_capacity(BLURAY_READ_CHUNK_BYTES + M2TS_PACKET_SIZE);
    let mut packet_demuxer = SelectedPidPesDemuxer::new(audio_pid);
    let mut packets_since_cancel_check = 0usize;
    let mut detected_format: Option<TsPacketFormat> = None;

    loop {
        if cancel.is_cancelled() {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        let read = title_source.read(&mut read_buffer).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to read Blu-ray playlist {playlist_number:05} title stream: {err}"
            ))
        })?;
        let reached_eof = read == 0;
        if reached_eof {
            if packet_buffer.is_empty() {
                break;
            }
        } else {
            packet_buffer.extend_from_slice(&read_buffer[..read]);
        }

        let ts_packet_format = match detected_format {
            Some(ts_packet_format) => ts_packet_format,
            None if reached_eof => {
                let ts_packet_format = detect_ts_packet_format_at_eof(&packet_buffer)?;
                detected_format = Some(ts_packet_format);
                ts_packet_format
            }
            None => {
                if packet_buffer.len() < TS_FORMAT_DETECTION_MIN_BYTES {
                    continue;
                }
                let ts_packet_format = detect_ts_packet_format(&packet_buffer)?;
                detected_format = Some(ts_packet_format);
                ts_packet_format
            }
        };

        let packet_size = ts_packet_format.packet_size();
        let sync_byte_offset = ts_packet_format.sync_byte_offset();
        let mut offset = 0usize;
        while packet_buffer.len().saturating_sub(offset) >= packet_size {
            if packet_buffer[offset + sync_byte_offset] != 0x47 {
                let Some(sync_offset) = find_next_ts_sync_at_cadence_with_format(
                    &packet_buffer[offset + 1..],
                    ts_packet_format,
                ) else {
                    let retain = ts_resync_retention_bytes(ts_packet_format);
                    let available = packet_buffer.len().saturating_sub(offset);
                    if available > retain {
                        offset = packet_buffer.len() - retain;
                    }
                    break;
                };
                log::warn!(
                    "Blu-ray TS sync loss while demuxing PID 0x{audio_pid:04x}; skipped {} byte(s) after confirming {}-byte sync cadence",
                    sync_offset + 1,
                    packet_size,
                );
                offset += sync_offset + 1;
                continue;
            }

            let ts_start = offset + sync_byte_offset;
            let ts_end = ts_start + TS_PACKET_SIZE;
            let packet = &packet_buffer[ts_start..ts_end];
            if let Some(pes) = packet_demuxer.push_ts_packet(packet)? {
                if !wav_header_written {
                    let wav_format = extractor.peek_or_create_wav_format(&pes.payload)?;
                    write_wav_header(&mut output, wav_format, 0)?;
                    wav_header_written = true;
                }
                if extractor.process_pes_packet(pes, &mut output)?
                    == PesProcessingOutcome::Stop
                {
                    packet_buffer.clear();
                    packet_demuxer.discard_current();
                    finalize_bluray_lpcm_output(
                        &mut output,
                        &tmp_path,
                        &mut tmp,
                        wav_path,
                        extractor,
                        wav_header_written,
                    )?;
                    return Ok(());
                }
            }
            offset += packet_size;
            packets_since_cancel_check += 1;
            if packets_since_cancel_check >= BLURAY_READ_PACKET_COUNT {
                packets_since_cancel_check = 0;
                if cancel.is_cancelled() {
                    return Err(ConvertError::Realize("cancelled".to_string()));
                }
            }
        }
        if offset > 0 {
            packet_buffer.drain(..offset);
        }
        if reached_eof {
            break;
        }
    }

    if !packet_buffer.is_empty() {
        let Some(ts_packet_format) = detected_format else {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray playlist {playlist_number:05} ended before packet format could be detected; {} byte(s) remained without recognizable 188-byte TS or 192-byte M2TS sync",
                packet_buffer.len()
            )));
        };
        let trailing = packet_buffer.len();
        if trailing < ts_packet_format.packet_size() {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray playlist {playlist_number:05} ended with {} trailing byte(s), not a complete {}-byte packet",
                trailing,
                ts_packet_format.packet_size(),
            )));
        }
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray playlist {playlist_number:05} ended after sync loss; {} byte(s) remained at {}-byte cadence",
            trailing,
            ts_packet_format.packet_size(),
        )));
    }

    if let Some(pes) = packet_demuxer.finish()? {
        if !wav_header_written {
            let format = extractor.peek_or_create_wav_format(&pes.payload)?;
            write_wav_header(&mut output, format, 0)?;
            wav_header_written = true;
        }
        let _ = extractor.process_pes_packet(pes, &mut output)?;
    }

    finalize_bluray_lpcm_output(
        &mut output,
        &tmp_path,
        &mut tmp,
        wav_path,
        extractor,
        wav_header_written,
    )
}

fn finalize_bluray_lpcm_output(
    output: &mut File,
    tmp_path: &Path,
    tmp: &mut ScopedTempPath,
    wav_path: &Path,
    mut extractor: BlurayLpcmExtractor,
    wav_header_written: bool,
) -> Result<(), ConvertError> {
    if !wav_header_written {
        let _ = fs::remove_file(tmp_path);
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray LPCM stream PID 0x{:04x} in playlist {:05} produced no PES payload",
            extractor.config.audio_pid, extractor.config.playlist_number
        )));
    }
    if extractor.pending_pcm_len() != 0 {
        let _ = fs::remove_file(tmp_path);
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray LPCM stream PID 0x{:04x} ended with {} trailing byte(s), not a complete sample frame",
            extractor.config.audio_pid,
            extractor.pending_pcm_len()
        )));
    }
    if extractor.data_bytes_written() == 0 {
        let _ = fs::remove_file(tmp_path);
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray LPCM stream {} PID 0x{:04x} in '{}' produced no audio payload",
            u16::from(extractor.config.audio_stream_index) + 1,
            extractor.config.audio_pid,
            extractor.config.source.display()
        )));
    }

    let format = extractor.format().ok_or_else(|| {
        ConvertError::Realize("Blu-ray LPCM WAV format was not initialized".to_string())
    })?;
    extractor.validate_reasonable_output_size()?;
    rewrite_wav_header(output, format, extractor.data_bytes_written())?;
    output.flush().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to flush Blu-ray LPCM WAV '{}': {err}",
            tmp_path.display()
        ))
    })?;
    output.sync_all().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to sync Blu-ray LPCM WAV '{}': {err}",
            tmp_path.display()
        ))
    })?;

    let expected_audio = extractor.expected_audio()?;
    validate_bluray_lpcm_wav(tmp_path, &expected_audio).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "Blu-ray LPCM WAV validation failed for '{}': {err}",
            tmp_path.display()
        ))
    })?;

    publish_temp_file(tmp, wav_path)
}

struct ScopedTempPath {
    path: PathBuf,
    armed: bool,
}

impl ScopedTempPath {
    fn for_final(final_path: &Path, purpose: &str) -> Result<Self, ConvertError> {
        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to create Blu-ray output directory '{}': {err}",
                parent.display()
            ))
        })?;
        let base = final_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(sanitize_component)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "bluray-output".to_string());
        let counter = BLURAY_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_name = format!(
            ".{base}.{purpose}.pid{}.attempt{counter}.{nonce}.tmp",
            std::process::id()
        );
        let path = parent.join(tmp_name);
        remove_scratch_file(&path)?;
        Ok(Self { path, armed: true })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ScopedTempPath {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn publish_temp_file(tmp: &mut ScopedTempPath, final_path: &Path) -> Result<(), ConvertError> {
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to create Blu-ray output directory '{}': {err}",
                parent.display()
            ))
        })?;
    }

    atomic_replace_file(tmp.path(), final_path).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to atomically publish Blu-ray output '{}' from '{}': {err}",
            final_path.display(),
            tmp.path().display()
        ))
    })?;
    tmp.disarm();

    if let Some(parent) = final_path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn atomic_replace_file(src: &Path, dst: &Path) -> io::Result<()> {
    atomic_replace_file_impl(src, dst)
}

#[cfg(unix)]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> {
    fs::rename(src, dst)
}

#[cfg(windows)]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => return Ok(()),
        Err(first_err) if first_err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(first_err) if dst.exists() => {
            let _ = first_err;
        }
        Err(first_err) => return Err(first_err),
    }

    let backup = unique_replace_backup_path(dst);
    fs::rename(dst, &backup)?;
    match fs::rename(src, dst) {
        Ok(()) => {
            let _ = fs::remove_file(&backup);
            Ok(())
        }
        Err(promote_err) => {
            let restore_result = fs::rename(&backup, dst);
            if let Err(restore_err) = restore_result {
                return Err(io::Error::new(
                    promote_err.kind(),
                    format!(
                        "failed to publish replacement '{}': {promote_err}; also failed to restore previous output from '{}': {restore_err}",
                        dst.display(),
                        backup.display()
                    ),
                ));
            }
            Err(promote_err)
        }
    }
}

#[cfg(windows)]
fn unique_replace_backup_path(dst: &Path) -> PathBuf {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dst
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "bluray-output".to_string());
    let counter = BLURAY_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(
        ".{file_name}.replace-backup.pid{}.attempt{counter}.{nonce}.tmp",
        std::process::id()
    ))
}

#[cfg(not(any(unix, windows)))]
fn atomic_replace_file_impl(src: &Path, dst: &Path) -> io::Result<()> {
    fs::rename(src, dst)
}

fn remove_scratch_file(path: &Path) -> Result<(), ConvertError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(ConvertError::Realize(format!(
            "failed to remove scratch file '{}': {err}",
            path.display()
        ))),
    }
}

fn validate_nonempty_wav(path: &Path) -> Result<(), ConvertError> {
    let wav = read_wav_info(path).map_err(|err| {
        ConvertError::TrackValidation(format!(
            "decoded Blu-ray WAV failed post-publish validation for '{}': {err}",
            path.display()
        ))
    })?;
    if wav.data_size == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "decoded Blu-ray WAV has an empty data chunk: {}",
            path.display()
        )));
    }
    Ok(())
}

fn bluray_output_stem(
    source: &Path,
    playlist_number: u32,
    title_index: usize,
    angle_number: u8,
    chapter_number: u32,
    chapter_start_pts_90k: u64,
    chapter_end_pts_90k: Option<u64>,
    audio_pid: u16,
    audio_stream_index: u8,
    audio_coding: BluRayAudioCoding,
) -> String {
    let name = source
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "bluray".to_string());
    let hash = bluray_stable_hash(
        source,
        playlist_number,
        title_index,
        angle_number,
        chapter_number,
        chapter_start_pts_90k,
        chapter_end_pts_90k,
        audio_pid,
        audio_stream_index,
    );
    format!(
        "{name}_pl{playlist_number:05}_a{angle_number}_c{chapter_number:03}_pid{audio_pid:04x}_s{audio_stream_index}_{}_{hash:016x}",
        audio_coding.label().to_ascii_lowercase().replace('-', "")
    )
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn bluray_stable_hash(
    source: &Path,
    playlist_number: u32,
    title_index: usize,
    angle_number: u8,
    chapter_number: u32,
    chapter_start_pts_90k: u64,
    chapter_end_pts_90k: Option<u64>,
    audio_pid: u16,
    audio_stream_index: u8,
) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    fn feed(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    feed(&mut hash, source.to_string_lossy().as_bytes());
    feed(&mut hash, &playlist_number.to_le_bytes());
    feed(&mut hash, &(title_index as u64).to_le_bytes());
    feed(&mut hash, &[angle_number]);
    feed(&mut hash, &chapter_number.to_le_bytes());
    feed(&mut hash, &chapter_start_pts_90k.to_le_bytes());
    feed(&mut hash, &chapter_end_pts_90k.unwrap_or(u64::MAX).to_le_bytes());
    feed(&mut hash, &audio_pid.to_le_bytes());
    feed(&mut hash, &[audio_stream_index]);
    hash
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::errors::ToolRunnerError;
    use super::super::tool::{ToolBinary, ToolCommand, ToolOutput};

    struct PanicToolRunner;

    #[async_trait::async_trait]
    impl ToolRunner for PanicToolRunner {
        async fn run(
            &self,
            _cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            panic!("compressed Blu-ray rejection must not execute external tools")
        }

        fn tool_version(&self, _binary: ToolBinary) -> Option<String> {
            None
        }
    }

    fn compressed_bluray_source(audio_coding: BluRayAudioCoding) -> TrackSourceRef {
        TrackSourceRef::BluRayTrack {
            source: PathBuf::from("/nonexistent/fixture.iso"),
            playlist_number: 12,
            title_index: 0,
            angle_number: 1,
            chapter_number: 1,
            chapter_start_pts_90k: 0,
            chapter_end_pts_90k: Some(90_000),
            audio_pid: 0x1100,
            audio_stream_index: 0,
            audio_coding,
            sample_rate: Some(48_000),
            bit_depth: None,
            channels: Some(6),
            channel_layout: Some("5.1".to_string()),
        }
    }

    fn synthetic_ts_packet(pid: u16, continuity_counter: u8) -> [u8; TS_PACKET_SIZE] {
        let mut packet = [0xffu8; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        packet[2] = pid as u8;
        packet[3] = 0x10 | (continuity_counter & 0x0f);
        packet
    }

    fn synthetic_m2ts_packet(
        ts_packet: &[u8; TS_PACKET_SIZE],
        arrival_time_stamp: u32,
    ) -> [u8; M2TS_PACKET_SIZE] {
        let mut packet = [0u8; M2TS_PACKET_SIZE];
        let tp_extra = arrival_time_stamp & 0x3fff_ffff;
        packet[..M2TS_TP_EXTRA_SIZE].copy_from_slice(&tp_extra.to_be_bytes());
        packet[M2TS_TP_EXTRA_SIZE..].copy_from_slice(ts_packet);
        packet
    }

    fn synthetic_standard_ts_stream(packet_count: u8) -> Vec<u8> {
        let mut stream = Vec::new();
        for cc in 0..packet_count {
            stream.extend_from_slice(&synthetic_ts_packet(0x1100, cc));
        }
        stream
    }

    fn synthetic_m2ts_stream(packet_count: u8) -> Vec<u8> {
        let mut stream = Vec::new();
        for cc in 0..packet_count {
            let ts = synthetic_ts_packet(0x1100, cc);
            let m2ts = synthetic_m2ts_packet(&ts, u32::from(cc));
            stream.extend_from_slice(&m2ts);
        }
        stream
    }


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

    fn synthetic_pes_packet(pts: u64, payload: &[u8]) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xbd];
        let packet_len = 3 + 5 + payload.len();
        pes.extend_from_slice(&(packet_len as u16).to_be_bytes());
        pes.extend_from_slice(&[0x80, 0x80, 0x05]);
        pes.extend_from_slice(&encode_pts(pts));
        pes.extend_from_slice(payload);
        pes
    }

    fn synthetic_ts_payload_packet(
        pid: u16,
        payload_unit_start: bool,
        continuity_counter: u8,
        payload: &[u8],
    ) -> [u8; TS_PACKET_SIZE] {
        assert!(payload.len() <= TS_PACKET_SIZE - 4);
        let mut packet = [0xffu8; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        if payload_unit_start {
            packet[1] |= 0x40;
        }
        packet[2] = pid as u8;
        let adaptation_field_control = if payload.len() == TS_PACKET_SIZE - 4 {
            1u8
        } else {
            3u8
        };
        packet[3] = (adaptation_field_control << 4) | (continuity_counter & 0x0f);
        if adaptation_field_control == 1 {
            packet[4..].copy_from_slice(payload);
        } else {
            let adaptation_len = (TS_PACKET_SIZE - 5) - payload.len();
            packet[4] = adaptation_len as u8;
            if adaptation_len > 0 {
                packet[5] = 0x00;
                for byte in &mut packet[6..5 + adaptation_len] {
                    *byte = 0xff;
                }
            }
            let payload_offset = 5 + adaptation_len;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        }
        packet
    }

    fn packetize_pes_for_read_loop(
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
            packets.push(synthetic_ts_payload_packet(
                pid,
                index == 0,
                cc,
                &pes[offset..end],
            ));
            offset = end;
            cc = (cc + 1) & 0x0f;
            if offset == pes.len() {
                break;
            }
        }
        if offset < pes.len() {
            packets.push(synthetic_ts_payload_packet(
                pid,
                packets.is_empty(),
                cc,
                &pes[offset..],
            ));
        }
        packets
    }

    fn synthetic_m2ts_stream_from_ts_packets(packets: &[[u8; TS_PACKET_SIZE]]) -> Vec<u8> {
        let mut stream = Vec::new();
        for (index, ts_packet) in packets.iter().enumerate() {
            let m2ts = synthetic_m2ts_packet(ts_packet, index as u32);
            stream.extend_from_slice(&m2ts);
        }
        stream
    }

    #[derive(Debug)]
    struct ReadLoopHarnessOutcome {
        detected_format: TsPacketFormat,
        pushed_packets: usize,
        resync_count: usize,
        drain_count: usize,
        trailing_bytes_at_eof: usize,
        pes_payloads: Vec<Vec<u8>>,
    }

    fn collect_pes_with_read_loop_equivalent(
        chunks: &[&[u8]],
        pid: u16,
    ) -> Result<ReadLoopHarnessOutcome, ConvertError> {
        let mut packet_buffer =
            Vec::<u8>::with_capacity(BLURAY_READ_CHUNK_BYTES + M2TS_PACKET_SIZE);
        let mut packet_demuxer = SelectedPidPesDemuxer::new(pid);
        let mut detected_format: Option<TsPacketFormat> = None;
        let mut pushed_packets = 0usize;
        let mut resync_count = 0usize;
        let mut drain_count = 0usize;
        let mut pes_payloads = Vec::new();

        for chunk_index in 0..=chunks.len() {
            let reached_eof = chunk_index == chunks.len();
            if reached_eof {
                if packet_buffer.is_empty() {
                    break;
                }
            } else {
                packet_buffer.extend_from_slice(chunks[chunk_index]);
            }

            let ts_packet_format = match detected_format {
                Some(ts_packet_format) => ts_packet_format,
                None if reached_eof => {
                    let ts_packet_format = detect_ts_packet_format_at_eof(&packet_buffer)?;
                    detected_format = Some(ts_packet_format);
                    ts_packet_format
                }
                None => {
                    if packet_buffer.len() < TS_FORMAT_DETECTION_MIN_BYTES {
                        continue;
                    }
                    let ts_packet_format = detect_ts_packet_format(&packet_buffer)?;
                    detected_format = Some(ts_packet_format);
                    ts_packet_format
                }
            };

            let packet_size = ts_packet_format.packet_size();
            let sync_byte_offset = ts_packet_format.sync_byte_offset();
            let mut offset = 0usize;
            while packet_buffer.len().saturating_sub(offset) >= packet_size {
                if packet_buffer[offset + sync_byte_offset] != 0x47 {
                    let Some(sync_offset) = find_next_ts_sync_at_cadence_with_format(
                        &packet_buffer[offset + 1..],
                        ts_packet_format,
                    ) else {
                        let retain = ts_resync_retention_bytes(ts_packet_format);
                        let available = packet_buffer.len().saturating_sub(offset);
                        if available > retain {
                            offset = packet_buffer.len() - retain;
                        }
                        break;
                    };
                    resync_count += 1;
                    offset += sync_offset + 1;
                    continue;
                }

                let ts_start = offset + sync_byte_offset;
                let ts_end = ts_start + TS_PACKET_SIZE;
                let packet = &packet_buffer[ts_start..ts_end];
                if let Some(pes) = packet_demuxer.push_ts_packet(packet)? {
                    pes_payloads.push(pes.payload);
                }
                pushed_packets += 1;
                offset += packet_size;
            }

            if offset > 0 {
                packet_buffer.drain(..offset);
                drain_count += 1;
            }

            if reached_eof {
                let trailing = packet_buffer.len();
                if trailing != 0 {
                    if trailing < ts_packet_format.packet_size() {
                        return Err(ConvertError::TrackValidation(format!(
                            "Blu-ray playlist 00000 ended with {} trailing byte(s), not a complete {}-byte packet",
                            trailing,
                            ts_packet_format.packet_size()
                        )));
                    }
                    return Err(ConvertError::TrackValidation(format!(
                        "Blu-ray playlist 00000 ended after sync loss; {} byte(s) remained at {}-byte cadence",
                        trailing,
                        ts_packet_format.packet_size()
                    )));
                }
                break;
            }
        }

        if let Some(pes) = packet_demuxer.finish()? {
            pes_payloads.push(pes.payload);
        }

        Ok(ReadLoopHarnessOutcome {
            detected_format: detected_format
                .expect("read-loop equivalent must detect a non-empty stream"),
            pushed_packets,
            resync_count,
            drain_count,
            trailing_bytes_at_eof: packet_buffer.len(),
            pes_payloads,
        })
    }

    #[test]
    fn detects_m2ts_packet_format_at_start_of_buffer() {
        let stream = synthetic_m2ts_stream(4);

        assert_eq!(
            detect_ts_packet_format(&stream).unwrap(),
            TsPacketFormat::M2ts
        );
    }

    #[test]
    fn detects_standard_ts_packet_format_at_start_of_buffer() {
        let stream = synthetic_standard_ts_stream(4);

        assert_eq!(
            detect_ts_packet_format(&stream).unwrap(),
            TsPacketFormat::StandardTs
        );
    }

    #[test]
    fn detects_m2ts_packet_format_after_misaligned_prefix() {
        let mut stream = vec![0xde, 0xad, 0xbe, 0xef, 0x47];
        stream.extend_from_slice(&synthetic_m2ts_stream(3));

        assert_eq!(
            detect_ts_packet_format(&stream).unwrap(),
            TsPacketFormat::M2ts
        );
    }

    #[test]
    fn packet_format_detection_rejects_unrecognizable_bytes() {
        let stream = vec![0x00; TS_FORMAT_DETECTION_MIN_BYTES];

        let err = detect_ts_packet_format(&stream).unwrap_err();
        assert!(
            err.to_string().contains("does not contain recognizable"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn packet_format_detection_uses_minimum_bytes_for_four_sync_confirmations() {
        assert_eq!(
            TS_FORMAT_DETECTION_MIN_BYTES,
            M2TS_TP_EXTRA_SIZE
                + (TS_FORMAT_DETECTION_CONFIRMATION_PACKETS - 1) * M2TS_PACKET_SIZE
                + 1
        );
        assert_eq!(TS_FORMAT_DETECTION_MIN_BYTES, 581);
    }

    #[test]
    fn eof_packet_format_detection_accepts_single_complete_m2ts_packet() {
        let stream = synthetic_m2ts_stream(1);

        assert_eq!(
            detect_ts_packet_format_at_eof(&stream).unwrap(),
            TsPacketFormat::M2ts
        );
    }

    #[test]
    fn eof_packet_format_detection_accepts_single_complete_standard_ts_packet() {
        let stream = synthetic_standard_ts_stream(1);

        assert_eq!(
            detect_ts_packet_format_at_eof(&stream).unwrap(),
            TsPacketFormat::StandardTs
        );
    }

    #[test]
    fn m2ts_resync_retention_keeps_candidate_tp_extra_prefix() {
        assert_eq!(
            ts_resync_retention_bytes(TsPacketFormat::StandardTs),
            TS_PACKET_SIZE * (TS_RESYNC_CONFIRMATION_PACKETS - 1) + 1
        );
        assert_eq!(
            ts_resync_retention_bytes(TsPacketFormat::M2ts),
            M2TS_PACKET_SIZE * (TS_RESYNC_CONFIRMATION_PACKETS - 1) + M2TS_TP_EXTRA_SIZE + 1
        );
        assert_eq!(ts_resync_retention_bytes(TsPacketFormat::M2ts), 389);
    }


    #[test]
    fn m2ts_read_loop_equivalent_reassembles_pes_after_detection_resync_and_draining() {
        let pid = 0x1100;
        let pes = synthetic_pes_packet(0, &[0x6a; 512]);
        let ts_packets = packetize_pes_for_read_loop(pid, 0, &pes, &[64, 130, 184, 80, 184]);
        assert!(ts_packets.len() >= TS_RESYNC_CONFIRMATION_PACKETS + 1);

        let first = synthetic_m2ts_packet(&ts_packets[0], 0);
        let rest = synthetic_m2ts_stream_from_ts_packets(&ts_packets[1..]);
        let mut stream = Vec::new();
        stream.extend_from_slice(&first);
        stream.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x47, 0x01]);
        stream.extend_from_slice(&rest);

        let split_points = [17usize, 241, 619, 907, stream.len()];
        let mut previous = 0usize;
        let chunks: Vec<&[u8]> = split_points
            .iter()
            .map(|&end| {
                let chunk = &stream[previous..end];
                previous = end;
                chunk
            })
            .collect();

        let outcome = collect_pes_with_read_loop_equivalent(&chunks, pid).unwrap();
        assert_eq!(outcome.detected_format, TsPacketFormat::M2ts);
        assert_eq!(outcome.pushed_packets, ts_packets.len());
        assert_eq!(outcome.resync_count, 1);
        assert!(
            outcome.drain_count >= 2,
            "expected multiple buffer drain operations, got {outcome:?}"
        );
        assert_eq!(outcome.trailing_bytes_at_eof, 0);
        assert_eq!(outcome.pes_payloads, vec![pes]);
    }

    #[test]
    fn m2ts_read_loop_equivalent_rejects_incomplete_trailing_packet_at_eof() {
        let pid = 0x1100;
        let mut stream = synthetic_m2ts_stream(4);
        stream.extend_from_slice(&[0x00, 0x00, 0x00, 0x47, 0xff]);
        let chunks = vec![&stream[..]];

        let err = collect_pes_with_read_loop_equivalent(&chunks, pid).unwrap_err();
        assert!(
            err.to_string().contains("ended with 5 trailing byte(s)"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn compressed_bluray_audio_returns_targeted_error_before_lpcm_realization() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = StagingDir::borrowed(temp.path().join("stage"), "job".to_string());
        let cancel = CancellationToken::new();
        let src = compressed_bluray_source(BluRayAudioCoding::DtsHdMaster);

        let err = realize_bluray_track(
            &src,
            &staging,
            &PanicToolRunner,
            &cancel,
            None,
            None,
        )
        .await
        .expect_err("compressed Blu-ray streams must be rejected before LPCM realization");

        match err {
            ConvertError::Realize(message) => {
                assert!(
                    message.contains("Blu-ray compressed audio streams are not yet implemented"),
                    "unexpected message: {message}"
                );
                assert!(message.contains("stream 1"), "unexpected message: {message}");
                assert!(message.contains("PID 0x1100"), "unexpected message: {message}");
            }
            other => panic!("unexpected error type: {other:?}"),
        }
    }
}

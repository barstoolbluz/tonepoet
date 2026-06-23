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
    find_next_ts_sync_at_cadence, SelectedPidPesDemuxer, TS_PACKET_SIZE, TS_RESYNC_CONFIRMATION_PACKETS,
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

static BLURAY_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
            "Blu-ray compressed codec extraction not yet implemented for {} stream {} PID 0x{:04x}",
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
    let mut packet_buffer = Vec::<u8>::with_capacity(BLURAY_READ_CHUNK_BYTES + TS_PACKET_SIZE);
    let mut packet_demuxer = SelectedPidPesDemuxer::new(audio_pid);
    let mut packets_since_cancel_check = 0usize;

    loop {
        if cancel.is_cancelled() {
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        let read = title_source.read(&mut read_buffer).map_err(|err| {
            ConvertError::Realize(format!(
                "failed to read Blu-ray playlist {playlist_number:05} title stream: {err}"
            ))
        })?;
        if read == 0 {
            break;
        }

        packet_buffer.extend_from_slice(&read_buffer[..read]);
        let mut offset = 0usize;
        while packet_buffer.len().saturating_sub(offset) >= TS_PACKET_SIZE {
            if packet_buffer[offset] != 0x47 {
                let Some(sync_offset) = find_next_ts_sync_at_cadence(&packet_buffer[offset + 1..]) else {
                    let retain = TS_PACKET_SIZE * (TS_RESYNC_CONFIRMATION_PACKETS - 1) + 1;
                    let available = packet_buffer.len().saturating_sub(offset);
                    if available > retain {
                        offset = packet_buffer.len() - retain;
                    }
                    break;
                };
                log::warn!(
                    "Blu-ray TS sync loss while demuxing PID 0x{audio_pid:04x}; skipped {} byte(s) after confirming 188-byte sync cadence",
                    sync_offset + 1
                );
                offset += sync_offset + 1;
                continue;
            }

            let packet = &packet_buffer[offset..offset + TS_PACKET_SIZE];
            if let Some(pes) = packet_demuxer.push_ts_packet(packet)? {
                if !wav_header_written {
                    let format = extractor.peek_or_create_wav_format(&pes.payload)?;
                    write_wav_header(&mut output, format, 0)?;
                    wav_header_written = true;
                }
                if extractor.process_pes_packet(pes, &mut output)? == PesProcessingOutcome::Stop {
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
            offset += TS_PACKET_SIZE;
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
    }

    if !packet_buffer.is_empty() {
        if packet_buffer.first().copied() != Some(0x47) {
            return Err(ConvertError::TrackValidation(format!(
                "Blu-ray playlist {playlist_number:05} ended after TS sync loss that could not be resynchronized at repeated 188-byte cadence; {} byte(s) remained",
                packet_buffer.len()
            )));
        }
        return Err(ConvertError::TrackValidation(format!(
            "Blu-ray playlist {playlist_number:05} ended with {} trailing byte(s), not a complete 188-byte TS packet",
            packet_buffer.len()
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

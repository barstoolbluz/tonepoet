//! DVD-Video track realization.
//!
//! DVD-Video LPCM and compressed audio require different realization paths:
//!
//! * LPCM sectors are demuxed with the existing DVD-Video Private Stream 1
//!   parser (`parse_private_stream_1_packets_with_mode(...,
//!   DvdaSubHeaderMode::DvdVideo)`) and written directly as a PCM WAV carrier.
//! * AC-3, DTS, and MPEG audio are extracted as elementary streams from the VOB
//!   PES packets and decoded with ffmpeg to a `pcm_s32le` WAV carrier.
//!
//! This deliberately avoids sending LPCM through ffmpeg as an opaque VOB
//! fragment. That keeps the LPCM path aligned with the DVD-Audio/DVD-Video
//! demuxer used elsewhere in the pipeline and makes the selected substream,
//! sample width, byte order, and sector addressing explicit. Cell sectors are
//! resolved through the VTS title-VOB inventory captured from
//! `DvdDisc.video_ts_files`; VTSI_MAT sector-pointer fields are not used as ISO
//! LBAs.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use super::errors::{ConvertError, ToolRunnerError};
use super::progress::OperationProgressTracker;
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::track_executor::{run_tool_command_with_concurrency, ToolConcurrencyLimits};
use super::types::{DvdVideoAudioCoding, DvdVideoVobFileRef, StagingDir, TrackSourceRef};
use super::{
    parse_private_stream_1_packets_with_mode, DvdaSubHeaderMode, DvdaSubstreamKind,
};

const DVD_SECTOR_SIZE: u64 = 2048;
const DVD_SECTOR_SIZE_USIZE: usize = DVD_SECTOR_SIZE as usize;
/// Bounded sequential read size for DVD-Video VOB spans. 512 sectors is 1 MiB,
/// large enough to amortize seeks/syscalls while keeping cancellation latency
/// and scratch memory bounded.
const DVDV_READ_CHUNK_SECTORS: u32 = 512;
const DVDV_READ_CHUNK_BYTES: usize = DVD_SECTOR_SIZE_USIZE * DVDV_READ_CHUNK_SECTORS as usize;
const DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_TRACK: u32 = 1024;
const DVDV_REALIZE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

static DVDV_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub async fn realize_dvdv_track(
    src: &TrackSourceRef,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
    _progress_tracker: Option<&mut OperationProgressTracker<'_>>,
) -> Result<PathBuf, ConvertError> {
    let TrackSourceRef::DvdVideoTrack {
        source,
        vts_number,
        title_number,
        angle_number: _,
        chapter_number,
        audio_stream_index,
        audio_coding,
        cell_sectors,
        vob_files,
        sample_rate,
        bit_depth,
        channels,
    } = src else {
        return Err(ConvertError::Realize(
            "realize_dvdv_track called with non-DVD-Video source".to_string(),
        ));
    };

    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }
    if cell_sectors.is_empty() {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video VTS {} title {} chapter {} has no cell sectors",
            vts_number, title_number, chapter_number
        )));
    }
    validate_vob_inventory(*vts_number, vob_files)?;
    validate_cell_coverage(cell_sectors, vob_files)?;
    preflight_no_css_scrambling_flags(source, vob_files, cell_sectors, cancel)?;

    let realized_dir = staging.root.join("dvdv-realized");
    fs::create_dir_all(&realized_dir).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to create DVD-Video realization directory '{}': {err}",
            realized_dir.display()
        ))
    })?;

    let stem = dvdv_output_stem(
        source,
        *vts_number,
        *title_number,
        *chapter_number,
        *audio_stream_index,
        *audio_coding,
        cell_sectors,
    );
    let wav_path = realized_dir.join(format!("{stem}.wav"));

    match audio_coding {
        DvdVideoAudioCoding::Lpcm => {
            realize_lpcm_with_dvdvideo_demuxer(
                source,
                &wav_path,
                vob_files,
                cell_sectors,
                *audio_stream_index,
                *sample_rate,
                *bit_depth,
                *channels,
                cancel,
            )?;
        }
        DvdVideoAudioCoding::Ac3 | DvdVideoAudioCoding::Dts | DvdVideoAudioCoding::Mpeg => {
            decode_compressed_audio_stream(
                source,
                &wav_path,
                &realized_dir,
                &stem,
                vob_files,
                cell_sectors,
                *audio_stream_index,
                *audio_coding,
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await?;
        }
    }

    if let Err(err) = validate_nonempty_wav(&wav_path) {
        let _ = remove_scratch_file(&wav_path);
        return Err(err);
    }
    Ok(wav_path)
}

fn realize_lpcm_with_dvdvideo_demuxer(
    source: &Path,
    wav_path: &Path,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    audio_stream_index: u8,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    channels: Option<u8>,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let sample_rate = sample_rate.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Video LPCM track '{}' is missing an IFO sample-rate assertion",
            source.display()
        ))
    })?;
    let bit_depth = bit_depth.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Video LPCM track '{}' is missing an IFO bit-depth assertion",
            source.display()
        ))
    })?;
    let channels = channels.ok_or_else(|| {
        ConvertError::TrackValidation(format!(
            "DVD-Video LPCM track '{}' is missing an IFO channel-count assertion",
            source.display()
        ))
    })?;

    if !matches!(sample_rate, 48_000 | 96_000) {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported DVD-Video LPCM sample rate {sample_rate}; expected 48000 or 96000 Hz"
        )));
    }
    if !matches!(bit_depth, 16 | 20 | 24) {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported DVD-Video LPCM bit depth {bit_depth}; expected 16, 20, or 24"
        )));
    }
    if channels == 0 || channels > 8 {
        return Err(ConvertError::TrackValidation(format!(
            "unsupported DVD-Video LPCM channel count {channels}; expected 1..=8"
        )));
    }

    let mut tmp = ScopedTempPath::for_final(wav_path, "lpcm-wav")?;
    let tmp_path = tmp.path().to_path_buf();
    let mut output = File::create(&tmp_path).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to create DVD-Video LPCM WAV '{}': {err}",
            tmp_path.display()
        ))
    })?;

    let format = LpcmWavFormat::new(sample_rate, channels, bit_depth)?;
    write_wav_header(&mut output, format, 0)?;

    let mut sector_reader = DvdVideoSectorReader::open(source, vob_files)?;
    let mut read_buffer = vec![0u8; DVDV_READ_CHUNK_BYTES];
    let mut pending = Vec::new();
    let mut data_bytes_written = 0u64;
    let target_substream = checked_dvdv_private_substream_id(DvdVideoAudioCoding::Lpcm, audio_stream_index)?;

    for_each_sector_in_authored_spans(
        &mut sector_reader,
        vob_files,
        cell_sectors,
        &mut read_buffer,
        cancel,
        |relative_sector, sector| {
            let selected_pes_packets = selected_private_stream_1_pes_packets(sector, target_substream);
            if selected_pes_packets.is_empty() {
                return Ok(());
            }

            for selected_pes in selected_pes_packets {
                let mut filtered_sector = vec![0u8; sector.len()];
                filtered_sector[selected_pes.start..selected_pes.end]
                    .copy_from_slice(selected_pes.packet);

                let packets = parse_private_stream_1_packets_with_mode(
                    &filtered_sector,
                    DvdaSubHeaderMode::DvdVideo,
                )
                .map_err(|err| {
                    ConvertError::Realize(format!(
                        "DVD-Video LPCM demux failed for selected substream 0x{target_substream:02X} at relative sector {relative_sector}: {err}"
                    ))
                })?;

                let mut selected_pcm_packet_count = 0usize;
                for packet in packets.iter() {
                    if packet.sub_header.kind() != DvdaSubstreamKind::Pcm {
                        return Err(ConvertError::TrackValidation(format!(
                            "DVD-Video selected LPCM substream 0x{target_substream:02X} demuxed as non-PCM at relative sector {relative_sector}"
                        )));
                    }
                    selected_pcm_packet_count += 1;

                    if let Some(pcm) = packet.sub_header.pcm.as_ref() {
                        if let Some(rate) = pcm.group1_sample_rate.or(pcm.group2_sample_rate) {
                            if rate != sample_rate {
                                return Err(ConvertError::TrackValidation(format!(
                                    "DVD-Video LPCM stream sample-rate mismatch: IFO says {sample_rate}, packet says {rate}"
                                )));
                            }
                        }
                        if let Some(bits) = pcm.group1_bits.or(pcm.group2_bits) {
                            if bits != bit_depth {
                                return Err(ConvertError::TrackValidation(format!(
                                    "DVD-Video LPCM stream bit-depth mismatch: IFO says {bit_depth}, packet says {bits}"
                                )));
                            }
                        }
                    }

                    data_bytes_written = data_bytes_written
                        .checked_add(write_dvdvideo_lpcm_payload_as_wav_samples(
                            &mut output,
                            &mut pending,
                            packet.payload,
                            channels,
                            bit_depth,
                        )?)
                        .ok_or_else(|| ConvertError::Realize("DVD-Video LPCM WAV size overflow".to_string()))?;
                }

                if selected_pcm_packet_count == 0 {
                    return Err(ConvertError::TrackValidation(format!(
                        "DVD-Video selected LPCM substream 0x{target_substream:02X} at relative sector {relative_sector} produced no PCM packet"
                    )));
                }
            }
            Ok(())
        },
    )?;

    if !pending.is_empty() {
        let _ = fs::remove_file(&tmp_path);
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video LPCM stream ended with {} trailing byte(s), not a complete {}-bit sample block",
            pending.len(),
            bit_depth
        )));
    }
    if data_bytes_written == 0 {
        let _ = fs::remove_file(&tmp_path);
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video LPCM stream {} in '{}' produced no audio payload",
            audio_stream_index,
            source.display()
        )));
    }

    rewrite_wav_header(&mut output, format, data_bytes_written)?;
    output.sync_all().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to sync DVD-Video LPCM WAV '{}': {err}",
            tmp_path.display()
        ))
    })?;
    drop(output);
    publish_temp_file(&mut tmp, wav_path)
}

async fn decode_compressed_audio_stream(
    source: &Path,
    wav_path: &Path,
    realized_dir: &Path,
    stem: &str,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    audio_stream_index: u8,
    audio_coding: DvdVideoAudioCoding,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), ConvertError> {
    // Keep compressed-codec decoding behind the project ToolRunner. The
    // previous streaming implementation spawned ffmpeg directly, which bypassed configured tool paths, command provenance,
    // test stubbing, stderr capture, and ToolConcurrencyLimits. Until the
    // shared ToolRunner contract grows an explicit stdin-producer mode, DVD-V
    // uses a scoped elementary scratch file plus the normal ToolRunner ffmpeg
    // invocation. The extraction remains chunked and buffered.
    let elementary_path = realized_dir.join(format!(
        "{stem}.{}",
        elementary_extension(audio_coding)
    ));
    remove_scratch_file(&elementary_path)?;
    extract_elementary_stream(
        source,
        &elementary_path,
        vob_files,
        cell_sectors,
        audio_stream_index,
        audio_coding,
        cancel,
    )?;

    let decode_result = if cancel.is_cancelled() {
        Err(ConvertError::Realize("cancelled".to_string()))
    } else {
        decode_elementary_stream(
            &elementary_path,
            audio_coding,
            wav_path,
            runner,
            cancel,
            tool_concurrency_limits,
        )
        .await
    };
    cleanup_after_decode(decode_result, &elementary_path)
}

fn extract_elementary_stream(
    source: &Path,
    out_path: &Path,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    audio_stream_index: u8,
    audio_coding: DvdVideoAudioCoding,
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    debug_assert!(!matches!(audio_coding, DvdVideoAudioCoding::Lpcm));

    let mut tmp = ScopedTempPath::for_final(out_path, "elementary-stream")?;
    let tmp_path = tmp.path().to_path_buf();
    let output = File::create(&tmp_path).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to create DVD-Video elementary stream '{}': {err}",
            tmp_path.display()
        ))
    })?;
    let mut output = BufWriter::with_capacity(DVDV_READ_CHUNK_BYTES, output);
    let bytes_written = extract_elementary_stream_to_writer(
        source,
        &mut output,
        vob_files,
        cell_sectors,
        audio_stream_index,
        audio_coding,
        cancel,
    )?;

    if bytes_written == 0 {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video {} stream {} in '{}' produced no elementary payload",
            audio_coding.label(),
            audio_stream_index,
            source.display()
        )));
    }

    output.flush().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to flush DVD-Video elementary stream '{}': {err}",
            tmp_path.display()
        ))
    })?;
    let output = output.into_inner().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to finish DVD-Video elementary stream '{}': {err}",
            tmp_path.display()
        ))
    })?;
    output.sync_all().map_err(|err| {
        ConvertError::Realize(format!(
            "failed to sync DVD-Video elementary stream '{}': {err}",
            tmp_path.display()
        ))
    })?;
    publish_temp_file(&mut tmp, out_path)
}

fn extract_elementary_stream_to_writer<W: Write>(
    source: &Path,
    output: &mut W,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    audio_stream_index: u8,
    audio_coding: DvdVideoAudioCoding,
    cancel: &CancellationToken,
) -> Result<u64, ConvertError> {
    debug_assert!(!matches!(audio_coding, DvdVideoAudioCoding::Lpcm));

    let mut sector_reader = DvdVideoSectorReader::open(source, vob_files)?;
    let mut read_buffer = vec![0u8; DVDV_READ_CHUNK_BYTES];
    let mut bytes_written = 0u64;

    for_each_sector_in_authored_spans(
        &mut sector_reader,
        vob_files,
        cell_sectors,
        &mut read_buffer,
        cancel,
        |_relative_sector, sector| {
            for pes in pes_payloads(sector) {
                let Some(elementary) = selected_elementary_payload(
                    pes,
                    audio_stream_index,
                    audio_coding,
                )? else {
                    continue;
                };
                if elementary.is_empty() {
                    continue;
                }
                output.write_all(elementary).map_err(|err| {
                    ConvertError::Realize(format!(
                        "failed to write DVD-Video elementary stream for '{}': {err}",
                        source.display()
                    ))
                })?;
                bytes_written = bytes_written
                    .checked_add(elementary.len() as u64)
                    .ok_or_else(|| ConvertError::Realize("DVD-Video elementary stream size overflow".to_string()))?;
            }
            Ok(())
        },
    )?;

    Ok(bytes_written)
}

async fn decode_elementary_stream(
    elementary_path: &Path,
    audio_coding: DvdVideoAudioCoding,
    wav_path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), ConvertError> {
    let mut tmp = ScopedTempPath::for_final(wav_path, "decoded-wav")?;
    let tmp_path = tmp.path().to_path_buf();
    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
    ];

    if let Some(format) = ffmpeg_input_format(audio_coding) {
        args.push("-f".into());
        args.push(format.into());
    }

    args.extend([
        "-i".into(),
        elementary_path.to_string_lossy().into_owned(),
        "-vn".into(),
        "-sn".into(),
        "-dn".into(),
        "-f".into(),
        "wav".into(),
        "-c:a".into(),
        "pcm_s32le".into(),
        tmp_path.to_string_lossy().into_owned(),
    ]);

    let cmd = ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args,
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: DVDV_REALIZE_TIMEOUT,
    };

    match run_tool_command_with_concurrency(cmd, runner, cancel, tool_concurrency_limits).await {
        Ok(_) => publish_temp_file(&mut tmp, wav_path),
        Err(ToolRunnerError::Cancelled { .. }) => Err(ConvertError::Realize("cancelled".to_string())),
        Err(err) => Err(ConvertError::Tool(err)),
    }
}

fn cleanup_after_decode(
    decode_result: Result<(), ConvertError>,
    elementary_path: &Path,
) -> Result<(), ConvertError> {
    let cleanup_result = remove_scratch_file(elementary_path);
    match (decode_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(cleanup_err)) => Err(cleanup_err),
        (Err(decode_err), Ok(())) => Err(decode_err),
        (Err(decode_err), Err(cleanup_err)) => Err(ConvertError::Realize(format!(
            "DVD-Video decode failed ({decode_err}); additionally failed to remove scratch elementary stream '{}': {cleanup_err}",
            elementary_path.display()
        ))),
    }
}

fn remove_scratch_file(path: &Path) -> Result<(), ConvertError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(ConvertError::Realize(format!(
            "failed to remove DVD-Video scratch file '{}': {err}",
            path.display()
        ))),
    }
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
                "failed to create DVD-Video output directory '{}': {err}",
                parent.display()
            ))
        })?;
        let base = final_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(sanitize_component)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "dvdv-output".to_string());
        let counter = DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
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
                "failed to create DVD-Video output directory '{}': {err}",
                parent.display()
            ))
        })?;
    }

    atomic_replace_file(tmp.path(), final_path).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to atomically publish DVD-Video output '{}' from '{}': {err}",
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

/// Replace `dst` with `src` without first unlinking `dst`.
///
/// The temporary file is always created beside the final path, so successful
/// publishes stay on the same filesystem. Unix uses `rename(2)`, which
/// atomically replaces an existing file. Windows has no overwrite-capable safe
/// `std` rename API, so the Windows implementation performs an idempotent
/// backup-then-promote sequence: move the old destination to a unique sibling,
/// move the new file into place, and restore the backup if promotion fails.
/// That avoids the previous rerun failure on existing outputs and preserves a
/// recoverable old file throughout std-only operation.
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
            // Some Windows filesystems report PermissionDenied for the same
            // condition. Try the replace path when the destination exists.
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
        .unwrap_or_else(|| "dvdv-output".to_string());
    let counter = DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
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

fn validate_vob_inventory(vts_number: u8, vob_files: &[DvdVideoVobFileRef]) -> Result<(), ConvertError> {
    if vob_files.is_empty() {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video VTS {vts_number} has no title VOB inventory"
        )));
    }

    let mut expected_next = 0u32;
    for file in vob_files {
        if file.vts_number != vts_number {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Video VOB '{}' belongs to VTS {}, expected VTS {vts_number}",
                file.file_name, file.vts_number
            )));
        }
        if file.block_first != expected_next || file.block_last < file.block_first {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Video VOB '{}' has non-contiguous sector map {}..{}, expected start {}",
                file.file_name, file.block_first, file.block_last, expected_next
            )));
        }
        expected_next = file.block_last.checked_add(1).ok_or_else(|| {
            ConvertError::TrackValidation(format!(
                "DVD-Video VOB '{}' sector map overflows u32",
                file.file_name
            ))
        })?;
    }
    Ok(())
}

fn validate_cell_coverage(
    cell_sectors: &[(u32, u32)],
    vob_files: &[DvdVideoVobFileRef],
) -> Result<(), ConvertError> {
    for &(first, last) in cell_sectors {
        if last < first {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Video cell sector range is invalid: {}..{}",
                first, last
            )));
        }
        for &edge in &[first, last] {
            if !vob_files.iter().any(|file| file.contains(edge)) {
                return Err(ConvertError::TrackValidation(format!(
                    "DVD-Video cell sector {edge} is outside the selected VTS title VOB inventory"
                )));
            }
        }
    }
    Ok(())
}

fn preflight_no_css_scrambling_flags(
    source: &Path,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    cancel: &CancellationToken,
) -> Result<(), ConvertError> {
    let mut reader = DvdVideoSectorReader::open(source, vob_files)?;
    let mut read_buffer = vec![0u8; DVDV_READ_CHUNK_BYTES];
    let mut scanned = 0u32;

    for &(first, last) in cell_sectors {
        if last < first {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Video cell sector range is invalid: {}..{}",
                first, last
            )));
        }
        let mut next_sector = first;
        while next_sector <= last && scanned < DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_TRACK {
            if cancel.is_cancelled() {
                return Err(ConvertError::Realize("cancelled".to_string()));
            }
            let remaining = last
                .checked_sub(next_sector)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| ConvertError::Realize("DVD-Video CSS preflight span underflow".to_string()))?;
            let sectors_read = reader.read_sector_chunk(
                &mut read_buffer,
                vob_files,
                next_sector,
                remaining.min(DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_TRACK - scanned),
            )?;
            let bytes_read = usize::try_from(sectors_read)
                .ok()
                .and_then(|sectors| sectors.checked_mul(DVD_SECTOR_SIZE_USIZE))
                .ok_or_else(|| ConvertError::Realize("DVD-Video CSS preflight byte count overflow".to_string()))?;
            for (offset, sector) in read_buffer[..bytes_read]
                .chunks_exact(DVD_SECTOR_SIZE_USIZE)
                .enumerate()
            {
                let relative_sector = next_sector
                    .checked_add(u32::try_from(offset).map_err(|_| {
                        ConvertError::Realize("DVD-Video CSS preflight sector chunk offset overflow".to_string())
                    })?)
                    .ok_or_else(|| ConvertError::Realize("DVD-Video CSS preflight sector number overflow".to_string()))?;
                if let Some(evidence) = scrambled_pes_in_sector(sector) {
                    let vob_label = vob_files
                        .iter()
                        .find(|vob| vob.contains(relative_sector))
                        .map(|vob| vob.file_name.as_str())
                        .unwrap_or("<unknown VOB>");
                    return Err(ConvertError::TrackValidation(format!(
                        "DVD-Video source appears CSS/encrypted: MPEG PES scrambling_control={} on stream 0x{:02X} at relative sector {} in {}. This build detects and explains likely CSS encryption but does not decrypt it; provide an unencrypted DVD-Video source and retry.",
                        evidence.scrambling_control, evidence.stream_id, relative_sector, vob_label
                    )));
                }
                scanned = scanned.saturating_add(1);
                if scanned >= DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_TRACK {
                    break;
                }
            }
            next_sector = next_sector
                .checked_add(sectors_read)
                .ok_or_else(|| ConvertError::Realize("DVD-Video CSS preflight sector iterator overflow".to_string()))?;
        }
        if scanned >= DVDV_CSS_PREFLIGHT_MAX_SECTORS_PER_TRACK {
            break;
        }
    }
    Ok(())
}

struct DvdVideoSectorReader {
    iso: Option<File>,
    directory_vobs: BTreeMap<u8, File>,
}

impl DvdVideoSectorReader {
    fn open(source: &Path, vob_files: &[DvdVideoVobFileRef]) -> Result<Self, ConvertError> {
        let directory_backed = vob_files.iter().any(|file| file.path.is_some());
        if directory_backed {
            let mut directory_vobs = BTreeMap::new();
            for vob in vob_files {
                let path = vob.path.as_ref().ok_or_else(|| {
                    ConvertError::TrackValidation(format!(
                        "DVD-Video VOB '{}' has no filesystem path in a directory-backed source",
                        vob.file_name
                    ))
                })?;
                let file = File::open(path).map_err(|err| {
                    ConvertError::Realize(format!(
                        "failed to open DVD-Video VOB '{}': {err}",
                        path.display()
                    ))
                })?;
                directory_vobs.insert(vob.vob_index, file);
            }
            Ok(Self {
                iso: None,
                directory_vobs,
            })
        } else {
            let iso = File::open(source).map_err(|err| {
                ConvertError::Realize(format!(
                    "failed to open DVD-Video ISO '{}': {err}",
                    source.display()
                ))
            })?;
            Ok(Self {
                iso: Some(iso),
                directory_vobs: BTreeMap::new(),
            })
        }
    }

    fn read_sector_chunk(
        &mut self,
        buffer: &mut [u8],
        vob_files: &[DvdVideoVobFileRef],
        relative_first: u32,
        requested_sectors: u32,
    ) -> Result<u32, ConvertError> {
        if requested_sectors == 0 {
            return Ok(0);
        }
        if buffer.len() < DVD_SECTOR_SIZE_USIZE {
            return Err(ConvertError::Realize(
                "DVD-Video sector read buffer is smaller than one sector".to_string(),
            ));
        }

        let vob = vob_files
            .iter()
            .find(|file| file.contains(relative_first))
            .ok_or_else(|| {
                ConvertError::TrackValidation(format!(
                    "DVD-Video relative sector {relative_first} is outside the selected VTS title VOB inventory"
                ))
            })?;
        let offset_in_vob = relative_first.checked_sub(vob.block_first).ok_or_else(|| {
            ConvertError::Realize("DVD-Video VOB-relative sector underflow".to_string())
        })?;
        let available_in_vob = vob
            .block_last
            .checked_sub(relative_first)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| ConvertError::Realize("DVD-Video VOB span underflow".to_string()))?;
        let max_buffered_sectors = (buffer.len() / DVD_SECTOR_SIZE_USIZE)
            .try_into()
            .unwrap_or(u32::MAX);
        let sectors_to_read = requested_sectors
            .min(available_in_vob)
            .min(DVDV_READ_CHUNK_SECTORS)
            .min(max_buffered_sectors);
        if sectors_to_read == 0 {
            return Err(ConvertError::Realize(
                "DVD-Video sector read computed an empty chunk".to_string(),
            ));
        }
        let bytes_to_read = usize::try_from(sectors_to_read)
            .ok()
            .and_then(|sectors| sectors.checked_mul(DVD_SECTOR_SIZE_USIZE))
            .ok_or_else(|| ConvertError::Realize("DVD-Video chunk byte count overflow".to_string()))?;

        if vob.path.is_some() {
            let file = self.directory_vobs.get_mut(&vob.vob_index).ok_or_else(|| {
                ConvertError::Realize(format!(
                    "DVD-Video directory VOB '{}' was not opened",
                    vob.file_name
                ))
            })?;
            let byte_offset = u64::from(offset_in_vob)
                .checked_mul(DVD_SECTOR_SIZE)
                .ok_or_else(|| ConvertError::Realize("DVD-Video directory VOB byte offset overflow".to_string()))?;
            file.seek(SeekFrom::Start(byte_offset)).map_err(|err| {
                ConvertError::Realize(format!(
                    "failed to seek {} relative sector {relative_first} at byte {byte_offset}: {err}",
                    vob.file_name
                ))
            })?;
            file.read_exact(&mut buffer[..bytes_to_read]).map_err(|err| {
                ConvertError::Realize(format!(
                    "failed to read {} relative sectors {}..{} at byte {byte_offset}: {err}",
                    vob.file_name,
                    relative_first,
                    relative_first + sectors_to_read - 1
                ))
            })?;
        } else {
            let input = self.iso.as_mut().ok_or_else(|| {
                ConvertError::Realize("DVD-Video ISO reader is missing".to_string())
            })?;
            let absolute_lba = u64::from(vob.lba)
                .checked_add(u64::from(offset_in_vob))
                .ok_or_else(|| ConvertError::Realize("DVD-Video sector address overflow".to_string()))?;
            let byte_offset = absolute_lba
                .checked_mul(DVD_SECTOR_SIZE)
                .ok_or_else(|| ConvertError::Realize("DVD-Video byte offset overflow".to_string()))?;
            input.seek(SeekFrom::Start(byte_offset)).map_err(|err| {
                ConvertError::Realize(format!(
                    "failed to seek ISO sector {absolute_lba} for {} relative sector {relative_first}: {err}",
                    vob.file_name
                ))
            })?;
            input.read_exact(&mut buffer[..bytes_to_read]).map_err(|err| {
                ConvertError::Realize(format!(
                    "failed to read ISO sectors {}..{} for {} relative sectors {}..{}: {err}",
                    absolute_lba,
                    absolute_lba + u64::from(sectors_to_read) - 1,
                    vob.file_name,
                    relative_first,
                    relative_first + sectors_to_read - 1
                ))
            })?;
        }

        Ok(sectors_to_read)
    }
}

fn for_each_sector_in_authored_spans<F>(
    reader: &mut DvdVideoSectorReader,
    vob_files: &[DvdVideoVobFileRef],
    cell_sectors: &[(u32, u32)],
    buffer: &mut [u8],
    cancel: &CancellationToken,
    mut visit: F,
) -> Result<(), ConvertError>
where
    F: FnMut(u32, &[u8]) -> Result<(), ConvertError>,
{
    for &(first, last) in cell_sectors {
        if last < first {
            return Err(ConvertError::TrackValidation(format!(
                "DVD-Video cell sector range is invalid: {}..{}",
                first, last
            )));
        }

        let mut next_sector = first;
        while next_sector <= last {
            if cancel.is_cancelled() {
                return Err(ConvertError::Realize("cancelled".to_string()));
            }
            let remaining = last
                .checked_sub(next_sector)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| ConvertError::Realize("DVD-Video sector span underflow".to_string()))?;
            let sectors_read = reader.read_sector_chunk(buffer, vob_files, next_sector, remaining)?;
            let bytes_read = usize::try_from(sectors_read)
                .ok()
                .and_then(|sectors| sectors.checked_mul(DVD_SECTOR_SIZE_USIZE))
                .ok_or_else(|| ConvertError::Realize("DVD-Video chunk byte count overflow".to_string()))?;
            for (offset, sector) in buffer[..bytes_read]
                .chunks_exact(DVD_SECTOR_SIZE_USIZE)
                .enumerate()
            {
                let relative_sector = next_sector
                    .checked_add(u32::try_from(offset).map_err(|_| {
                        ConvertError::Realize("DVD-Video sector chunk offset overflow".to_string())
                    })?)
                    .ok_or_else(|| ConvertError::Realize("DVD-Video sector number overflow".to_string()))?;
                visit(relative_sector, sector)?;
            }
            next_sector = next_sector
                .checked_add(sectors_read)
                .ok_or_else(|| ConvertError::Realize("DVD-Video sector iterator overflow".to_string()))?;
        }
    }
    Ok(())
}

fn selected_private_stream_1_pes_packets<'a>(
    sector: &'a [u8],
    target_substream: u8,
) -> Vec<PesPacket<'a>> {
    pes_packets(sector)
        .filter(|pes| pes.stream_id == 0xBD)
        .filter(|pes| pes.payload.first().copied() == Some(target_substream))
        .collect()
}

fn selected_elementary_payload<'a>(
    pes: PesPayload<'a>,
    audio_stream_index: u8,
    audio_coding: DvdVideoAudioCoding,
) -> Result<Option<&'a [u8]>, ConvertError> {
    match audio_coding {
        DvdVideoAudioCoding::Lpcm => Ok(None),
        DvdVideoAudioCoding::Mpeg => {
            let stream_id = checked_mpeg_audio_stream_id(audio_stream_index)?;
            if pes.stream_id == stream_id {
                Ok(Some(pes.payload))
            } else {
                Ok(None)
            }
        }
        DvdVideoAudioCoding::Ac3 | DvdVideoAudioCoding::Dts => {
            if pes.stream_id != 0xBD {
                return Ok(None);
            }
            let target_substream = checked_dvdv_private_substream_id(audio_coding, audio_stream_index)?;
            if pes.payload.first().copied() != Some(target_substream) {
                return Ok(None);
            }
            let payload = strip_dvdv_private_audio_subheader(pes.payload, audio_coding)?;
            Ok(Some(payload))
        }
    }
}

fn strip_dvdv_private_audio_subheader(
    payload: &[u8],
    audio_coding: DvdVideoAudioCoding,
) -> Result<&[u8], ConvertError> {
    // AC-3 and DTS DVD private-stream packets carry a one-byte substream ID
    // followed by three DVD-specific bytes (frame count plus first access-unit
    // pointer). The elementary bitstream follows immediately after those four
    // bytes. LPCM is intentionally not handled here because it must go through
    // the DVD-Video LPCM demuxer path above.
    let header_len = match audio_coding {
        DvdVideoAudioCoding::Ac3 | DvdVideoAudioCoding::Dts => 4,
        DvdVideoAudioCoding::Lpcm | DvdVideoAudioCoding::Mpeg => 0,
    };
    if payload.len() < header_len {
        return Err(ConvertError::Realize(format!(
            "DVD-Video {} private-stream packet is shorter than its {}-byte subheader",
            audio_coding.label(),
            header_len
        )));
    }
    Ok(&payload[header_len..])
}

#[derive(Debug, Clone, Copy)]
struct PesPayload<'a> {
    stream_id: u8,
    payload: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
struct PesPacket<'a> {
    stream_id: u8,
    start: usize,
    end: usize,
    packet: &'a [u8],
    payload: &'a [u8],
    mpeg2_header: Option<Mpeg2PesHeader>,
}

#[derive(Debug, Clone, Copy)]
struct Mpeg2PesHeader {
    scrambling_control: u8,
}

fn pes_payloads(sector: &[u8]) -> impl Iterator<Item = PesPayload<'_>> {
    pes_packets(sector).map(|pes| PesPayload {
        stream_id: pes.stream_id,
        payload: pes.payload,
    })
}

fn scrambled_pes_in_sector(sector: &[u8]) -> Option<CssScramblingEvidence> {
    pes_packets(sector).find_map(|pes| {
        let header = pes.mpeg2_header?;
        (header.scrambling_control != 0).then_some(CssScramblingEvidence {
            stream_id: pes.stream_id,
            scrambling_control: header.scrambling_control,
        })
    })
}

#[derive(Debug, Clone, Copy)]
struct CssScramblingEvidence {
    stream_id: u8,
    scrambling_control: u8,
}

fn pes_packets(sector: &[u8]) -> impl Iterator<Item = PesPacket<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i + 4 <= sector.len() {
        if !sector[i..].starts_with(&[0, 0, 1]) {
            i += 1;
            continue;
        }

        let stream_id = sector[i + 3];
        match ps_start_code_extent(sector, i, stream_id) {
            PsStartCodeExtent::Skip { end } => {
                i = end.max(i + 4);
                continue;
            }
            PsStartCodeExtent::Pes { data_start, packet_end } => {
                if packet_end <= data_start {
                    i += 4;
                    continue;
                }

                if let Some((payload_start, mpeg2_header)) = pes_payload_start_offset(&sector[i..packet_end], stream_id) {
                    let absolute_payload_start = i + payload_start;
                    if absolute_payload_start <= packet_end {
                        out.push(PesPacket {
                            stream_id,
                            start: i,
                            end: packet_end,
                            packet: &sector[i..packet_end],
                            payload: &sector[absolute_payload_start..packet_end],
                            mpeg2_header,
                        });
                    }
                }

                i = packet_end.max(i + 4);
            }
            PsStartCodeExtent::Malformed => {
                i += 4;
            }
        }
    }

    out.into_iter()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PsStartCodeExtent {
    Skip { end: usize },
    Pes { data_start: usize, packet_end: usize },
    Malformed,
}

fn ps_start_code_extent(sector: &[u8], start: usize, stream_id: u8) -> PsStartCodeExtent {
    match stream_id {
        0xBA => pack_header_end(sector, start)
            .map(|end| PsStartCodeExtent::Skip { end })
            .unwrap_or(PsStartCodeExtent::Malformed),
        0xBB => length_prefixed_start_code_end(sector, start)
            .map(|end| PsStartCodeExtent::Skip { end })
            .unwrap_or(PsStartCodeExtent::Malformed),
        _ => {
            let Some(data_start) = start.checked_add(6) else {
                return PsStartCodeExtent::Malformed;
            };
            if data_start > sector.len() {
                return PsStartCodeExtent::Malformed;
            }
            let Some(packet_len) = pes_packet_length(sector, start) else {
                return PsStartCodeExtent::Malformed;
            };
            let packet_end = if packet_len == 0 {
                find_next_start_code(sector, data_start).unwrap_or(sector.len())
            } else {
                let Some(end) = data_start.checked_add(packet_len).filter(|&end| end <= sector.len()) else {
                    return PsStartCodeExtent::Malformed;
                };
                end
            };
            PsStartCodeExtent::Pes { data_start, packet_end }
        }
    }
}

fn pes_packet_length(buf: &[u8], start: usize) -> Option<usize> {
    let len_start = start.checked_add(4)?;
    let len_end = start.checked_add(6)?;
    if len_end > buf.len() {
        return None;
    }
    Some(u16::from_be_bytes([buf[len_start], buf[len_start + 1]]) as usize)
}

fn length_prefixed_start_code_end(buf: &[u8], start: usize) -> Option<usize> {
    let len_end = start.checked_add(6)?;
    let packet_len = pes_packet_length(buf, start)?;
    len_end.checked_add(packet_len).filter(|&end| end <= buf.len())
}

fn pack_header_end(buf: &[u8], start: usize) -> Option<usize> {
    let marker = *buf.get(start.checked_add(4)?)?;
    if (marker & 0xC0) == 0x40 {
        // MPEG-2 pack header: 14 bytes plus stuffing_length in the low three
        // bits of byte 13. DVD VOB sectors normally start with this header.
        let stuffing_index = start.checked_add(13)?;
        let stuffing = (*buf.get(stuffing_index)? & 0x07) as usize;
        start.checked_add(14)?.checked_add(stuffing).filter(|&end| end <= buf.len())
    } else if (marker & 0xF0) == 0x20 {
        // MPEG-1 pack header. Rare for DVD-Video, but skipping it correctly
        // keeps the scanner from treating bytes 4..5 as a PES length.
        start.checked_add(12).filter(|&end| end <= buf.len())
    } else {
        None
    }
}

fn find_next_start_code(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i..].starts_with(&[0, 0, 1]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn pes_payload_start_offset(packet: &[u8], stream_id: u8) -> Option<(usize, Option<Mpeg2PesHeader>)> {
    if packet.len() < 6 {
        return None;
    }

    if is_unstructured_stream_id(stream_id) {
        return Some((6, None));
    }

    if packet.len() >= 9 && (packet[6] & 0xC0) == 0x80 {
        let offset = 9usize.checked_add(packet[8] as usize)?;
        return (offset <= packet.len()).then_some((
            offset,
            Some(Mpeg2PesHeader {
                scrambling_control: (packet[6] >> 4) & 0x03,
            }),
        ));
    }

    legacy_pes_payload_start_offset(packet).map(|offset| (offset, None))
}

fn legacy_pes_payload_start_offset(packet: &[u8]) -> Option<usize> {
    let mut offset = 6usize;
    while offset < packet.len() && packet[offset] == 0xFF {
        offset += 1;
    }
    if offset >= packet.len() {
        return None;
    }

    if (packet[offset] & 0xC0) == 0x40 {
        offset = offset.checked_add(2)?;
    }
    if offset >= packet.len() {
        return None;
    }

    offset = match packet[offset] & 0xF0 {
        0x20 => offset.checked_add(5)?,
        0x30 => offset.checked_add(10)?,
        _ if packet[offset] == 0x0F => offset.checked_add(1)?,
        _ => offset,
    };

    (offset <= packet.len()).then_some(offset)
}

fn is_unstructured_stream_id(stream_id: u8) -> bool {
    matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    )
}

fn is_dvdv_audio_private_substream(substream: u8) -> bool {
    matches!(substream, 0x80..=0x8F | 0xA0..=0xA7)
}

fn checked_dvdv_private_substream_id(
    audio_coding: DvdVideoAudioCoding,
    audio_stream_index: u8,
) -> Result<u8, ConvertError> {
    if audio_stream_index > 7 {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video audio stream index {audio_stream_index} is outside 0..=7"
        )));
    }
    let base = match audio_coding {
        DvdVideoAudioCoding::Ac3 => 0x80,
        DvdVideoAudioCoding::Dts => 0x88,
        DvdVideoAudioCoding::Lpcm => 0xA0,
        DvdVideoAudioCoding::Mpeg => {
            return Err(ConvertError::TrackValidation(
                "MPEG audio does not use DVD private-stream substream IDs".to_string(),
            ))
        }
    };
    Ok(base + audio_stream_index)
}

fn checked_mpeg_audio_stream_id(audio_stream_index: u8) -> Result<u8, ConvertError> {
    if audio_stream_index > 31 {
        return Err(ConvertError::TrackValidation(format!(
            "DVD-Video MPEG audio stream index {audio_stream_index} is outside 0..=31"
        )));
    }
    Ok(0xC0 + audio_stream_index)
}

fn write_dvdvideo_lpcm_payload_as_wav_samples(
    output: &mut File,
    pending: &mut Vec<u8>,
    payload: &[u8],
    channels: u8,
    bit_depth: u32,
) -> Result<u64, ConvertError> {
    pending.extend_from_slice(payload);
    let block_size = dvdvideo_lpcm_block_size(channels, bit_depth)?;
    let complete_len = pending.len() / block_size * block_size;
    if complete_len == 0 {
        return Ok(0);
    }

    let mut written = 0u64;
    for block in pending[..complete_len].chunks_exact(block_size) {
        written = written
            .checked_add(write_lpcm_block(output, block, channels, bit_depth)?)
            .ok_or_else(|| ConvertError::Realize("DVD-Video LPCM WAV size overflow".to_string()))?;
    }
    pending.drain(..complete_len);
    Ok(written)
}

fn dvdvideo_lpcm_block_size(channels: u8, bit_depth: u32) -> Result<usize, ConvertError> {
    let channels = usize::from(channels);
    match bit_depth {
        16 => Ok(2 * channels),
        20 => Ok(4 * channels + 2 * div_ceil_usize(channels, 2)),
        24 => Ok(6 * channels),
        _ => Err(ConvertError::TrackValidation(format!(
            "unsupported DVD-Video LPCM bit depth {bit_depth}; expected 16, 20, or 24"
        ))),
    }
}


fn div_ceil_usize(value: usize, divisor: usize) -> usize {
    debug_assert!(divisor != 0);
    (value + divisor - 1) / divisor
}

fn write_lpcm_block(
    output: &mut File,
    block: &[u8],
    channels: u8,
    bit_depth: u32,
) -> Result<u64, ConvertError> {
    let channels = usize::from(channels);
    match bit_depth {
        16 => {
            for sample in block.chunks_exact(2) {
                output.write_all(&[sample[1], sample[0]]).map_err(|err| {
                    ConvertError::Realize(format!("failed to write DVD-Video LPCM sample: {err}"))
                })?;
            }
            Ok(block.len() as u64)
        }
        20 => {
            let top_mid_len = 4 * channels;
            let lsb_bytes_per_frame = div_ceil_usize(channels, 2);
            let mut bytes_written = 0u64;
            for frame in 0..2usize {
                for channel in 0..channels {
                    let top_mid = (frame * channels + channel) * 2;
                    let lsb_index = top_mid_len + frame * lsb_bytes_per_frame + channel / 2;
                    let packed_lsb = block[lsb_index];
                    let low_nibble = if channel % 2 == 0 {
                        packed_lsb >> 4
                    } else {
                        packed_lsb & 0x0F
                    };
                    output
                        .write_all(&[low_nibble << 4, block[top_mid + 1], block[top_mid]])
                        .map_err(|err| {
                            ConvertError::Realize(format!(
                                "failed to write DVD-Video 20-bit LPCM sample: {err}"
                            ))
                        })?;
                    bytes_written += 3;
                }
            }
            Ok(bytes_written)
        }
        24 => {
            let top_mid_len = 4 * channels;
            let mut bytes_written = 0u64;
            for frame in 0..2usize {
                for channel in 0..channels {
                    let top_mid = (frame * channels + channel) * 2;
                    let low = top_mid_len + frame * channels + channel;
                    output
                        .write_all(&[block[low], block[top_mid + 1], block[top_mid]])
                        .map_err(|err| {
                            ConvertError::Realize(format!(
                                "failed to write DVD-Video 24-bit LPCM sample: {err}"
                            ))
                        })?;
                    bytes_written += 3;
                }
            }
            Ok(bytes_written)
        }
        _ => Err(ConvertError::TrackValidation(format!(
            "unsupported DVD-Video LPCM bit depth {bit_depth}; expected 16, 20, or 24"
        ))),
    }
}

#[derive(Debug, Clone, Copy)]
struct LpcmWavFormat {
    sample_rate: u32,
    channels: u16,
    container_bits_per_sample: u16,
    valid_bits_per_sample: u16,
    format_extensible: bool,
}

impl LpcmWavFormat {
    fn new(sample_rate: u32, channels: u8, source_bit_depth: u32) -> Result<Self, ConvertError> {
        let container_bits_per_sample = match source_bit_depth {
            16 => 16,
            20 | 24 => 24,
            _ => {
                return Err(ConvertError::TrackValidation(format!(
                    "unsupported DVD-Video LPCM bit depth {source_bit_depth}; expected 16, 20, or 24"
                )))
            }
        };
        Ok(Self {
            sample_rate,
            channels: u16::from(channels),
            container_bits_per_sample,
            valid_bits_per_sample: source_bit_depth as u16,
            format_extensible: source_bit_depth != u32::from(container_bits_per_sample) || channels > 2,
        })
    }

    fn bytes_per_sample(self) -> u16 {
        self.container_bits_per_sample / 8
    }

    fn block_align(self) -> Result<u16, ConvertError> {
        self.channels
            .checked_mul(self.bytes_per_sample())
            .ok_or_else(|| ConvertError::Realize("DVD-Video LPCM WAV block-align overflow".to_string()))
    }

    fn byte_rate(self) -> Result<u32, ConvertError> {
        self.sample_rate
            .checked_mul(u32::from(self.block_align()?))
            .ok_or_else(|| ConvertError::Realize("DVD-Video LPCM WAV byte-rate overflow".to_string()))
    }

    fn fmt_chunk_size(self) -> u32 {
        if self.format_extensible { 40 } else { 16 }
    }
}

fn write_wav_header(
    output: &mut File,
    format: LpcmWavFormat,
    data_len: u64,
) -> Result<(), ConvertError> {
    let riff_size = checked_riff_size(format, data_len)?;
    let data_size = u32::try_from(data_len).map_err(|_| {
        ConvertError::Realize(format!(
            "DVD-Video LPCM WAV data is too large for classic RIFF/WAVE: {data_len} bytes"
        ))
    })?;

    output.write_all(b"RIFF").map_err(wav_write_error)?;
    output.write_all(&riff_size.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(b"WAVE").map_err(wav_write_error)?;
    output.write_all(b"fmt ").map_err(wav_write_error)?;
    output
        .write_all(&format.fmt_chunk_size().to_le_bytes())
        .map_err(wav_write_error)?;

    if format.format_extensible {
        output.write_all(&0xFFFEu16.to_le_bytes()).map_err(wav_write_error)?;
    } else {
        output.write_all(&1u16.to_le_bytes()).map_err(wav_write_error)?;
    }
    output.write_all(&format.channels.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.sample_rate.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.byte_rate()?.to_le_bytes()).map_err(wav_write_error)?;
    output.write_all(&format.block_align()?.to_le_bytes()).map_err(wav_write_error)?;
    output
        .write_all(&format.container_bits_per_sample.to_le_bytes())
        .map_err(wav_write_error)?;

    if format.format_extensible {
        output.write_all(&22u16.to_le_bytes()).map_err(wav_write_error)?;
        output
            .write_all(&format.valid_bits_per_sample.to_le_bytes())
            .map_err(wav_write_error)?;
        output.write_all(&0u32.to_le_bytes()).map_err(wav_write_error)?; // channel mask unknown
        output
            .write_all(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00])
            .map_err(wav_write_error)?;
        output
            .write_all(&[0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71])
            .map_err(wav_write_error)?;
    }

    output.write_all(b"data").map_err(wav_write_error)?;
    output.write_all(&data_size.to_le_bytes()).map_err(wav_write_error)
}

fn rewrite_wav_header(
    output: &mut File,
    format: LpcmWavFormat,
    data_len: u64,
) -> Result<(), ConvertError> {
    output.seek(SeekFrom::Start(0)).map_err(|err| {
        ConvertError::Realize(format!("failed to seek DVD-Video LPCM WAV header: {err}"))
    })?;
    write_wav_header(output, format, data_len)
}

fn checked_riff_size(format: LpcmWavFormat, data_len: u64) -> Result<u32, ConvertError> {
    let riff_size = 20u64
        .checked_add(u64::from(format.fmt_chunk_size()))
        .and_then(|size| size.checked_add(data_len))
        .ok_or_else(|| ConvertError::Realize("DVD-Video LPCM WAV RIFF size overflow".to_string()))?;
    u32::try_from(riff_size).map_err(|_| {
        ConvertError::Realize(format!(
            "DVD-Video LPCM WAV is too large for classic RIFF/WAVE: {data_len} bytes of audio data"
        ))
    })
}

fn wav_write_error(err: std::io::Error) -> ConvertError {
    ConvertError::Realize(format!("failed to write DVD-Video LPCM WAV header: {err}"))
}

fn elementary_extension(audio_coding: DvdVideoAudioCoding) -> &'static str {
    match audio_coding {
        DvdVideoAudioCoding::Ac3 => "ac3",
        DvdVideoAudioCoding::Dts => "dts",
        DvdVideoAudioCoding::Mpeg => "mpa",
        DvdVideoAudioCoding::Lpcm => "lpcm",
    }
}

fn ffmpeg_input_format(audio_coding: DvdVideoAudioCoding) -> Option<&'static str> {
    match audio_coding {
        DvdVideoAudioCoding::Ac3 => Some("ac3"),
        DvdVideoAudioCoding::Dts => Some("dts"),
        DvdVideoAudioCoding::Mpeg | DvdVideoAudioCoding::Lpcm => None,
    }
}

fn validate_nonempty_wav(path: &Path) -> Result<(), ConvertError> {
    let len = fs::metadata(path)
        .map_err(|err| ConvertError::Realize(format!("decoded DVD-Video WAV is missing '{}': {err}", path.display())))?
        .len();
    if len <= 44 {
        return Err(ConvertError::TrackValidation(format!(
            "decoded DVD-Video WAV is empty or header-only: {}",
            path.display()
        )));
    }
    Ok(())
}

fn dvdv_output_stem(
    source: &Path,
    vts_number: u8,
    title_number: u8,
    chapter_number: u16,
    audio_stream_index: u8,
    audio_coding: DvdVideoAudioCoding,
    cell_sectors: &[(u32, u32)],
) -> String {
    let name = source
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "dvdv".to_string());
    let hash = dvdv_stable_hash(source, cell_sectors);
    format!(
        "{name}_vts{vts_number:02}_t{title_number:02}_c{chapter_number:03}_s{audio_stream_index}_{}_{hash:016x}",
        audio_coding.label().to_ascii_lowercase().replace('-', "")
    )
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '_' })
        .collect()
}

fn dvdv_stable_hash(source: &Path, cell_sectors: &[(u32, u32)]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for &(first, last) in cell_sectors {
        for byte in first.to_le_bytes().into_iter().chain(last.to_le_bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}


#[cfg(test)]
mod tests {
    use super::*;

    fn mpeg2_private_stream_packet(payload: &[u8]) -> Vec<u8> {
        let packet_len = 3usize + payload.len();
        let mut packet = vec![0x00, 0x00, 0x01, 0xBD];
        packet.extend_from_slice(&(packet_len as u16).to_be_bytes());
        packet.extend_from_slice(&[0x80, 0x00, 0x00]);
        packet.extend_from_slice(payload);
        packet
    }

    fn mpeg2_private_stream_packet_with_scrambling(payload: &[u8], scrambling_control: u8) -> Vec<u8> {
        let packet_len = 3usize + payload.len();
        let mut packet = vec![0x00, 0x00, 0x01, 0xBD];
        packet.extend_from_slice(&(packet_len as u16).to_be_bytes());
        packet.extend_from_slice(&[0x80 | ((scrambling_control & 0x03) << 4), 0x00, 0x00]);
        packet.extend_from_slice(payload);
        packet
    }


    fn mpeg2_pack_header(stuffing: u8) -> Vec<u8> {
        let stuffing = stuffing & 0x07;
        let mut packet = vec![0x00, 0x00, 0x01, 0xBA];
        packet.extend_from_slice(&[
            0x44, 0x00, 0x04, 0x00, 0x04, 0x01, 0x89, 0xC3, 0xF8,
            stuffing,
        ]);
        packet.extend(std::iter::repeat(0xFF).take(usize::from(stuffing)));
        packet
    }

    fn mpeg2_system_header(payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x00, 0x00, 0x01, 0xBB];
        packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn css_preflight_detects_mpeg_pes_scrambling_flags() {
        let mut sector = vec![0u8; 2048];
        let packet = mpeg2_private_stream_packet_with_scrambling(&[0xA0, 0x11, 0x22], 2);
        sector[48..48 + packet.len()].copy_from_slice(&packet);

        let evidence = scrambled_pes_in_sector(&sector).expect("scrambling evidence");
        assert_eq!(evidence.stream_id, 0xBD);
        assert_eq!(evidence.scrambling_control, 2);
    }

    #[test]
    fn css_preflight_ignores_clear_mpeg_pes_packets() {
        let mut sector = vec![0u8; 2048];
        let packet = mpeg2_private_stream_packet(&[0xA0, 0x11, 0x22]);
        sector[48..48 + packet.len()].copy_from_slice(&packet);

        assert!(scrambled_pes_in_sector(&sector).is_none());
    }

    #[test]
    fn pes_scanner_skips_dvd_pack_and_system_headers_before_audio_pes() {
        let mut sector = vec![0u8; 2048];
        let pack = mpeg2_pack_header(3);
        let system = mpeg2_system_header(&[0x80, 0x04, 0xE1, 0x7F]);
        let packet = mpeg2_private_stream_packet(&[0xA0, 0x55, 0x66]);
        let mut offset = 0usize;
        sector[offset..offset + pack.len()].copy_from_slice(&pack);
        offset += pack.len();
        sector[offset..offset + system.len()].copy_from_slice(&system);
        offset += system.len();
        sector[offset..offset + packet.len()].copy_from_slice(&packet);

        let selected = selected_private_stream_1_pes_packets(&sector, 0xA0);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].start, pack.len() + system.len());
        assert_eq!(selected[0].payload, &[0xA0, 0x55, 0x66]);
    }

    #[test]
    fn css_preflight_scans_past_pack_header_to_scrambled_audio_pes() {
        let mut sector = vec![0u8; 2048];
        let pack = mpeg2_pack_header(1);
        let packet = mpeg2_private_stream_packet_with_scrambling(&[0xA0, 0x11, 0x22], 3);
        sector[0..pack.len()].copy_from_slice(&pack);
        sector[pack.len()..pack.len() + packet.len()].copy_from_slice(&packet);

        let evidence = scrambled_pes_in_sector(&sector).expect("scrambling evidence after pack header");
        assert_eq!(evidence.stream_id, 0xBD);
        assert_eq!(evidence.scrambling_control, 3);
    }

    #[test]
    fn selected_private_stream_packets_keep_only_target_substream() {
        let mut sector = vec![0u8; 2048];
        let a0 = mpeg2_private_stream_packet(&[0xA0, 0x11, 0x22]);
        let a1 = mpeg2_private_stream_packet(&[0xA1, 0x33, 0x44]);
        sector[32..32 + a0.len()].copy_from_slice(&a0);
        sector[128..128 + a1.len()].copy_from_slice(&a1);

        let selected = selected_private_stream_1_pes_packets(&sector, 0xA1);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].start, 128);
        assert_eq!(selected[0].payload[0], 0xA1);
        assert_eq!(selected[0].packet, &a1[..]);
    }

    #[test]
    fn selected_private_stream_packets_preserve_multiple_target_packets_in_sector_order() {
        let mut sector = vec![0u8; 2048];
        let first = mpeg2_private_stream_packet(&[0xA0, 0x01]);
        let other = mpeg2_private_stream_packet(&[0xA1, 0xFF]);
        let second = mpeg2_private_stream_packet(&[0xA0, 0x02]);
        sector[16..16 + first.len()].copy_from_slice(&first);
        sector[96..96 + other.len()].copy_from_slice(&other);
        sector[160..160 + second.len()].copy_from_slice(&second);

        let selected = selected_private_stream_1_pes_packets(&sector, 0xA0);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].start, 16);
        assert_eq!(selected[0].payload, &[0xA0, 0x01]);
        assert_eq!(selected[1].start, 160);
        assert_eq!(selected[1].payload, &[0xA0, 0x02]);
    }

    #[test]
    fn selected_private_stream_packets_do_not_fallback_to_other_lpcm_streams() {
        let mut sector = vec![0u8; 2048];
        let a0 = mpeg2_private_stream_packet(&[0xA0, 0x11, 0x22]);
        sector[32..32 + a0.len()].copy_from_slice(&a0);

        let selected = selected_private_stream_1_pes_packets(&sector, 0xA2);
        assert!(selected.is_empty());
    }

    #[test]
    fn scoped_temp_paths_are_attempt_scoped() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-scoped-temp-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("chapter.wav");

        let first = ScopedTempPath::for_final(&final_path, "test").unwrap();
        let second = ScopedTempPath::for_final(&final_path, "test").unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(first.path().parent(), Some(dir.as_path()));
        assert_eq!(second.path().parent(), Some(dir.as_path()));

        drop(first);
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(windows))]
    fn publish_temp_file_replaces_existing_destination_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-publish-temp-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("chapter.wav");
        std::fs::write(&final_path, b"old").unwrap();

        let mut tmp = ScopedTempPath::for_final(&final_path, "test").unwrap();
        std::fs::write(tmp.path(), b"new").unwrap();
        publish_temp_file(&mut tmp, &final_path).unwrap();

        assert_eq!(std::fs::read(&final_path).unwrap(), b"new");
        assert!(!tmp.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn publish_temp_file_replaces_existing_destination_on_windows_rerun() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-publish-temp-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("chapter.wav");
        std::fs::write(&final_path, b"old").unwrap();

        let mut tmp = ScopedTempPath::for_final(&final_path, "test").unwrap();
        std::fs::write(tmp.path(), b"new").unwrap();
        publish_temp_file(&mut tmp, &final_path).unwrap();

        assert_eq!(std::fs::read(&final_path).unwrap(), b"new");
        assert!(!tmp.path().exists());
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("replace-backup"))
            .collect();
        assert!(backups.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_after_decode_removes_elementary_stream_on_decode_error() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-decode-cleanup-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let elementary = dir.join("chapter.ac3");
        std::fs::write(&elementary, b"scratch").unwrap();

        let result = cleanup_after_decode(
            Err(ConvertError::Realize("synthetic decode failure".to_string())),
            &elementary,
        );

        assert!(result.is_err());
        assert!(!elementary.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_numbered_vob(path: &Path, sectors: u8) {
        let mut bytes = Vec::new();
        for sector in 0..sectors {
            bytes.extend(std::iter::repeat(sector).take(DVD_SECTOR_SIZE_USIZE));
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn test_vob_ref(path: &Path, first: u32, last: u32, index: u8) -> DvdVideoVobFileRef {
        DvdVideoVobFileRef {
            vts_number: 1,
            vob_index: index,
            file_name: format!("VTS_01_{index}.VOB"),
            path: Some(path.to_path_buf()),
            lba: 0,
            byte_len: u64::from(last - first + 1) * DVD_SECTOR_SIZE,
            block_first: first,
            block_last: last,
        }
    }

    #[test]
    fn chunked_reader_does_not_cross_vob_boundaries() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-chunk-boundary-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first_vob = dir.join("VTS_01_1.VOB");
        let second_vob = dir.join("VTS_01_2.VOB");
        write_numbered_vob(&first_vob, 2);
        write_numbered_vob(&second_vob, 2);
        let vobs = vec![
            test_vob_ref(&first_vob, 0, 1, 1),
            test_vob_ref(&second_vob, 2, 3, 2),
        ];
        let mut reader = DvdVideoSectorReader::open(Path::new("dvd-root"), &vobs).unwrap();
        let mut buf = vec![0u8; DVDV_READ_CHUNK_BYTES];

        let first_count = reader.read_sector_chunk(&mut buf, &vobs, 1, 3).unwrap();
        assert_eq!(first_count, 1);
        assert_eq!(buf[0], 1);

        let second_count = reader.read_sector_chunk(&mut buf, &vobs, 2, 2).unwrap();
        assert_eq!(second_count, 2);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[DVD_SECTOR_SIZE_USIZE], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunked_span_iteration_preserves_authored_range_order() {
        let dir = std::env::temp_dir().join(format!(
            "dvdv-chunk-order-test-{}-{}",
            std::process::id(),
            DVDV_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let vob = dir.join("VTS_01_1.VOB");
        write_numbered_vob(&vob, 4);
        let vobs = vec![test_vob_ref(&vob, 0, 3, 1)];
        let mut reader = DvdVideoSectorReader::open(Path::new("dvd-root"), &vobs).unwrap();
        let mut buf = vec![0u8; DVDV_READ_CHUNK_BYTES];
        let mut seen = Vec::new();

        for_each_sector_in_authored_spans(
            &mut reader,
            &vobs,
            &[(2, 3), (0, 1)],
            &mut buf,
            &CancellationToken::new(),
            |_relative_sector, sector| {
                seen.push(sector[0]);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(seen, vec![2, 3, 0, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

}

//! Pre-publish structural serialization for intentionally merged programs.
//!
//! Structural sources are represented downstream as ordinary `PreparedTrack`s.
//! When those tracks are intentionally merged, this module emits the default
//! companion CUE and, for chapter-capable output, serializes the same ordered
//! structure as embedded chapters. It does not own a second chapter/CUE model:
//! boundaries come from the target-domain merged timeline and titles/metadata
//! come from the already-authoritative prepared source.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::errors::PublishError;
use super::metadata_rewrite::{
    metadata_rewrite_temp_path, replace_rewritten_metadata_file,
};
use super::tool::{ToolBinary, ToolCommand, ToolRunner};
use super::track_executor::{run_tool_command_with_concurrency, ToolConcurrencyLimits};
use super::types::{
    ArtifactSet, AudioArtifacts, MergedArtifact, PreparedSource, PreparedTrack, SidecarArtifact,
    SidecarKind, SourceKind, EMBEDDED_CHAPTER_STRUCTURE_EXTRA_KEY,
    EMBEDDED_CHAPTER_STRUCTURE_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChapterWriteEntry {
    title: String,
    start_sample: u64,
    end_sample: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct Mp4IlstSnapshot {
    /// The complete concrete ilst parsed before the structural FFmpeg remux.
    /// `None` means there was no ilst to preserve. Keeping the concrete tag,
    /// rather than a flattened generic map, retains freeform identities,
    /// repeated values, and multi-data atom groups already supported by Lofty.
    ilst: Option<lofty::mp4::Ilst>,
}

fn snapshot_mp4_ilst(path: &Path) -> Result<Mp4IlstSnapshot, String> {
    use lofty::file::AudioFile as _;

    let mut file = fs::File::open(path)
        .map_err(|error| format!("cannot open MP4 metadata carrier {}: {error}", path.display()))?;
    let mp4 = lofty::mp4::Mp4File::read_from(
        &mut file,
        lofty::config::ParseOptions::new().read_properties(false),
    )
    .map_err(|error| format!("cannot snapshot MP4 ilst in {}: {error}", path.display()))?;
    Ok(Mp4IlstSnapshot {
        ilst: mp4.ilst().cloned(),
    })
}

fn restore_mp4_ilst(path: &Path, snapshot: &Mp4IlstSnapshot) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::file::AudioFile as _;

    let Some(original_ilst) = snapshot.ilst.as_ref() else {
        return Ok(());
    };

    let mut file = fs::File::open(path)
        .map_err(|error| format!("cannot reopen MP4 chapter rewrite {}: {error}", path.display()))?;
    let mut mp4 = lofty::mp4::Mp4File::read_from(
        &mut file,
        lofty::config::ParseOptions::new().read_properties(false),
    )
    .map_err(|error| format!("cannot parse MP4 chapter rewrite {} before ilst restoration: {error}", path.display()))?;
    drop(file);

    mp4.set_ilst(original_ilst.clone());
    mp4.save_to_path(path, WriteOptions::default()).map_err(|error| {
        format!(
            "cannot restore preserved MP4 ilst after chapter rewrite in {}: {error}",
            path.display()
        )
    })
}

pub(crate) async fn finalize_structured_chapters_before_publish(
    artifacts: &mut ArtifactSet,
    source: &PreparedSource,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    restore_authoritative_mp4_metadata: bool,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), PublishError> {
    let merged = match &artifacts.audio {
        AudioArtifacts::Merged(merged) => merged.clone(),
        // Split export already represents the structure as one file per track.
        AudioArtifacts::Tracks(_) => return Ok(()),
    };

    let write_embedded_chapters = source_carries_program_structure(source)
        && chapter_capable_output_path(&merged.final_path);
    let write_default_merge_cue = merged_source_requires_structure_sidecar(source);
    if !write_embedded_chapters && !write_default_merge_cue {
        return Ok(());
    }

    // One authoritative target-domain program timeline feeds every structural
    // serialization. Lossless merges reuse exact counts captured during the
    // ordinary encode/merge path; lossy merges keep the existing ffprobe path
    // so codec priming/padding and concat-clock differences are measured facts.
    let timeline = resolve_merged_program_timeline(
        &merged,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await?;
    let entries = chapter_entries_for_merged(
        &merged,
        source,
        &timeline.track_samples,
        timeline.total_samples,
        timeline.sample_rate,
    )?;

    if write_embedded_chapters {
        write_embedded_chapters_for_merged(
            &merged,
            &entries,
            timeline.sample_rate,
            runner,
            cancel,
            tool_concurrency_limits,
        )
        .await?;

        if restore_authoritative_mp4_metadata
            && crate::convert::chapter_structure::chapter_container_capability(&merged.final_path)
                .is_some_and(|capability| capability.is_mp4_family())
        {
            super::stages::restore_merged_mp4_terminal_metadata_after_structural_remux(
                &merged.staged_path,
                source,
                runner,
                cancel,
                tool_concurrency_limits,
            )
            .await
            .map_err(|error| {
                chapter_error(format!(
                    "cannot restore terminal MP4 metadata after chapter rewrite for {}: {error}",
                    merged.staged_path.display()
                ))
            })?;

            // AtomicParsley/in-process terminal metadata layers execute after
            // the chapter remux. Verify the actual final staged artifact again
            // so chapter survival is proved after the last metadata mutation.
            verify_written_chapters(&merged.staged_path, &entries, timeline.sample_rate)?;
        }
    }

    if write_default_merge_cue {
        stage_default_merged_cue_sidecar(
            artifacts,
            source,
            &merged,
            &entries,
            timeline.sample_rate,
        )?;
    }

    Ok(())
}

fn merged_source_requires_structure_sidecar(source: &PreparedSource) -> bool {
    source.tracks.len() > 1 || source_carries_program_structure(source)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MergedProgramTimeline {
    sample_rate: u32,
    total_samples: u64,
    track_samples: Vec<u64>,
}

async fn resolve_merged_program_timeline(
    merged: &MergedArtifact,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<MergedProgramTimeline, PublishError> {
    let exact_track_counts_are_complete = merged.source_track_exact_samples.len()
        == merged.source_tracks.len()
        && !merged.source_track_exact_samples.is_empty()
        && merged.source_track_exact_samples.iter().all(|samples| *samples != 0);

    if exact_track_counts_are_complete {
        if let Some(sample_rate) = merged.timeline_sample_rate.filter(|rate| *rate != 0) {
            if merged.total_samples != 0 {
                return Ok(MergedProgramTimeline {
                    sample_rate,
                    total_samples: merged.total_samples,
                    track_samples: merged.source_track_exact_samples.clone(),
                });
            }
        }
    }

    let merged_timeline = probe_audio_timeline(
        &merged.staged_path,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await?;

    let track_samples = if exact_track_counts_are_complete {
        merged.source_track_exact_samples.clone()
    } else {
        probe_merged_source_track_samples(
            merged,
            merged_timeline.sample_rate,
            runner,
            cancel,
            tool_concurrency_limits,
        )
        .await?
    };

    Ok(MergedProgramTimeline {
        sample_rate: merged_timeline.sample_rate,
        total_samples: merged_timeline.samples,
        track_samples,
    })
}

async fn write_embedded_chapters_for_merged(
    merged: &MergedArtifact,
    entries: &[ChapterWriteEntry],
    sample_rate: u32,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), PublishError> {
    let ffmetadata = render_ffmetadata(entries, sample_rate)?;
    let parent = merged
        .staged_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut metadata_file = tempfile::Builder::new()
        .prefix(".tonepoet-chapters.")
        .suffix(".ffmeta")
        .tempfile_in(parent)
        .map_err(|error| chapter_error(format!(
            "cannot create temporary chapter metadata beside {}: {error}",
            merged.staged_path.display()
        )))?;
    metadata_file
        .write_all(ffmetadata.as_bytes())
        .and_then(|_| metadata_file.flush())
        .map_err(|error| chapter_error(format!(
            "cannot write temporary chapter metadata beside {}: {error}",
            merged.staged_path.display()
        )))?;

    let rewrite = metadata_rewrite_temp_path(&merged.staged_path).map_err(|error| {
        chapter_error(format!(
            "cannot reserve guarded chapter rewrite for {}: {error}",
            merged.staged_path.display()
        ))
    })?;
    let muxer = chapter_output_muxer(&merged.final_path).ok_or_else(|| {
        chapter_error(format!(
            "no chapter-capable muxer is defined for {}",
            merged.final_path.display()
        ))
    })?;
    let command = chapter_remux_command(
        &merged.staged_path,
        metadata_file.path(),
        rewrite.path(),
        muxer,
    );

    if let Err(error) = run_tool_command_with_concurrency(
        command,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await
    {
        rewrite.cleanup_best_effort();
        return Err(chapter_error(format!(
            "FFmpeg could not write chapters to {}: {error}",
            merged.staged_path.display()
        )));
    }

    if let Err(error) = verify_written_chapters(rewrite.path(), entries, sample_rate) {
        rewrite.cleanup_best_effort();
        return Err(error);
    }

    replace_rewritten_metadata_file(&merged.staged_path, rewrite).map_err(|error| {
        chapter_error(format!(
            "validated chapter rewrite could not atomically replace {}: {error}",
            merged.staged_path.display()
        ))
    })?;
    Ok(())
}

/// Rewrite embedded container chapters on an existing file for the metadata
/// editor's chapter-authoring surface.
///
/// The caller may pass a private same-directory batch stage instead of the
/// authoritative path. This function then performs its own guarded rewrite
/// inside that stage, verifies the serialized chapter table by reading it back,
/// and leaves outer publication to the caller. That layering lets chapter
/// entries participate in the metadata editor's all-or-nothing multi-carrier
/// transaction without duplicating the production chapter serializer.
pub(crate) async fn rewrite_embedded_chapters_for_authoring(
    path: &Path,
    chapters: &[(String, u64, u64)],
    sample_rate: u32,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), String> {
    if sample_rate == 0 {
        return Err("chapter write requires a non-zero sample rate".to_string());
    }
    if chapters.is_empty() {
        return Err("chapter write requires at least one chapter".to_string());
    }
    let mut entries = Vec::with_capacity(chapters.len());
    let mut previous_end = None;
    for (index, (title, start_sample, end_sample)) in chapters.iter().enumerate() {
        if *end_sample <= *start_sample {
            return Err(format!(
                "chapter {} has a non-positive sample range {}..{}",
                index + 1,
                start_sample,
                end_sample
            ));
        }
        if index == 0 && *start_sample != 0 {
            return Err(format!(
                "chapter 1 starts at sample {}; embedded chapter structure must begin at 0",
                start_sample
            ));
        }
        if previous_end.is_some_and(|end| end != *start_sample) {
            return Err(format!(
                "chapter {} does not begin at the previous chapter end",
                index + 1
            ));
        }
        entries.push(ChapterWriteEntry {
            title: if title.trim().is_empty() {
                format!("Chapter {}", index + 1)
            } else {
                title.clone()
            },
            start_sample: *start_sample,
            end_sample: *end_sample,
        });
        previous_end = Some(*end_sample);
    }

    let capability = crate::convert::chapter_structure::chapter_container_capability(path)
        .ok_or_else(|| format!("{} is not a chapter-capable container", path.display()))?;
    let muxer = capability.output_muxer();
    // Structural MOV remuxes do not faithfully preserve the complete iTunes
    // ilst (notably freeform atoms and repeated values). Snapshot the concrete
    // parsed ilst only for MP4-family carriers; Matroska/WebM retain their
    // metadata through the ordinary stream-copy remux and cannot be parsed as
    // MP4 by Lofty.
    let ilst_snapshot = capability
        .is_mp4_family()
        .then(|| snapshot_mp4_ilst(path))
        .transpose()?;
    let ffmetadata = render_ffmetadata(&entries, sample_rate)
        .map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut metadata_file = tempfile::Builder::new()
        .prefix(".tonepoet-chapter-authoring.")
        .suffix(".ffmeta")
        .tempfile_in(parent)
        .map_err(|error| format!(
            "cannot create temporary chapter metadata beside {}: {error}",
            path.display()
        ))?;
    metadata_file
        .write_all(ffmetadata.as_bytes())
        .and_then(|_| metadata_file.flush())
        .map_err(|error| format!(
            "cannot write temporary chapter metadata beside {}: {error}",
            path.display()
        ))?;

    let rewrite = metadata_rewrite_temp_path(path).map_err(|error| {
        format!(
            "cannot reserve guarded chapter rewrite for {}: {error}",
            path.display()
        )
    })?;
    let command = chapter_remux_command(path, metadata_file.path(), rewrite.path(), muxer);
    if let Err(error) = run_tool_command_with_concurrency(command, runner, cancel, None).await {
        rewrite.cleanup_best_effort();
        return Err(format!("FFmpeg could not write chapters to {}: {error}", path.display()));
    }
    if let Err(error) = verify_written_chapters(rewrite.path(), &entries, sample_rate) {
        rewrite.cleanup_best_effort();
        return Err(error.to_string());
    }
    if let Some(ilst_snapshot) = ilst_snapshot.as_ref() {
        if let Err(error) = restore_mp4_ilst(rewrite.path(), ilst_snapshot) {
            rewrite.cleanup_best_effort();
            return Err(error);
        }
        // Lofty's ilst restoration is the final metadata mutation in this
        // authoring rewrite. Verify the chapter table again afterwards so the
        // file is published only if both structure and unrelated metadata survive.
        if let Err(error) = verify_written_chapters(rewrite.path(), &entries, sample_rate) {
            rewrite.cleanup_best_effort();
            return Err(format!(
                "chapter verification after MP4 ilst restoration failed: {error}"
            ));
        }
    }
    replace_rewritten_metadata_file(path, rewrite).map_err(|error| {
        format!(
            "validated chapter rewrite could not atomically replace {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn stage_default_merged_cue_sidecar(
    artifacts: &mut ArtifactSet,
    source: &PreparedSource,
    merged: &MergedArtifact,
    entries: &[ChapterWriteEntry],
    sample_rate: u32,
) -> Result<(), PublishError> {
    let start_samples = entries
        .iter()
        .map(|entry| entry.start_sample)
        .collect::<Vec<_>>();
    let cue_content = super::stages::build_cue_sheet_with_merged_timeline(
        source,
        artifacts,
        Some((&start_samples, sample_rate)),
    );

    let staged_parent = merged
        .staged_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let final_parent = merged
        .final_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staged_path = staged_parent.join("album.cue");
    let final_path = final_parent.join("album.cue");
    fs::write(&staged_path, cue_content).map_err(|error| {
        chapter_error(format!(
            "cannot stage merged structural CUE {}: {error}",
            staged_path.display()
        ))
    })?;

    if let Some(existing) = artifacts
        .sidecars
        .iter_mut()
        .find(|sidecar| matches!(&sidecar.kind, SidecarKind::CueSheet))
    {
        existing.staged_path = staged_path;
        existing.final_path = final_path;
    } else {
        artifacts.sidecars.push(SidecarArtifact {
            kind: SidecarKind::CueSheet,
            staged_path,
            final_path,
        });
    }
    Ok(())
}

fn source_carries_program_structure(source: &PreparedSource) -> bool {
    source.kind == SourceKind::CueImage
        || source
            .album_metadata
            .extra
            .get(EMBEDDED_CHAPTER_STRUCTURE_EXTRA_KEY)
            .is_some_and(|value| value == EMBEDDED_CHAPTER_STRUCTURE_VERSION)
}

fn chapter_capable_output_path(path: &Path) -> bool {
    chapter_output_muxer(path).is_some()
}

fn chapter_output_muxer(path: &Path) -> Option<&'static str> {
    crate::convert::chapter_structure::chapter_container_capability(path)
        .map(|capability| capability.output_muxer())
}

fn chapter_entries_for_merged(
    merged: &MergedArtifact,
    source: &PreparedSource,
    track_timeline_samples: &[u64],
    merged_total_samples: u64,
    sample_rate: u32,
) -> Result<Vec<ChapterWriteEntry>, PublishError> {
    if track_timeline_samples.len() != merged.source_tracks.len() {
        return Err(chapter_error(format!(
            "merged structural output has {} track identities but {} measured encoded track lengths",
            merged.source_tracks.len(),
            track_timeline_samples.len()
        )));
    }
    if merged_total_samples == 0 {
        return Err(chapter_error(
            "merged structural output has a zero measured length".to_string(),
        ));
    }
    if sample_rate == 0 {
        return Err(chapter_error(
            "merged structural output has a zero measured sample rate".to_string(),
        ));
    }

    let measured_track_total = track_timeline_samples.iter().try_fold(0_u64, |total, samples| {
        total.checked_add(*samples).ok_or_else(|| {
            chapter_error("encoded structural track lengths overflowed u64 samples".to_string())
        })
    })?;
    let concat_clock_drift = measured_track_total.abs_diff(merged_total_samples);
    let concat_clock_tolerance = u64::from(sample_rate);
    if concat_clock_drift > concat_clock_tolerance {
        return Err(chapter_error(format!(
            "merged structural output timeline differs from its encoded track timelines by {} samples, exceeding the {}-sample concat tolerance",
            concat_clock_drift, concat_clock_tolerance
        )));
    }

    // FFmpeg's MP4-family stream-copy concat can retain one codec-priming
    // block in the merged presentation timeline even though each standalone
    // AAC carrier reports a priming-trimmed duration. That is an initial
    // presentation offset: assign the small measured concat delta to the first
    // chapter, then preserve every later encoded-track duration. Putting the
    // delta at the tail would shift every internal chapter boundary early.
    let first_track_samples = *track_timeline_samples.first().ok_or_else(|| {
        chapter_error("merged structural output has no measured track timelines".to_string())
    })?;
    let first_track_samples = if merged_total_samples >= measured_track_total {
        first_track_samples.checked_add(concat_clock_drift).ok_or_else(|| {
            chapter_error("first merged chapter length overflowed u64 samples".to_string())
        })?
    } else {
        first_track_samples.checked_sub(concat_clock_drift).ok_or_else(|| {
            chapter_error(format!(
                "merged structural output trims {} samples from a first encoded track of only {} samples",
                concat_clock_drift, first_track_samples
            ))
        })?
    };

    let mut entries = Vec::with_capacity(merged.source_tracks.len());
    let mut start_sample = 0_u64;
    for (index, track_id) in merged.source_tracks.iter().enumerate() {
        let track = source
            .tracks
            .iter()
            .find(|track| track.id == *track_id)
            .ok_or_else(|| {
                chapter_error(format!(
                    "merged structural track {:?} is absent from the prepared source",
                    track_id
                ))
            })?;
        let measured = if index == 0 {
            first_track_samples
        } else {
            track_timeline_samples[index]
        };
        if measured == 0 {
            return Err(chapter_error(format!(
                "merged structural track {:?} has zero measured encoded samples",
                track_id
            )));
        }

        let is_last = index + 1 == merged.source_tracks.len();
        let end_sample = if is_last {
            merged_total_samples
        } else {
            start_sample.checked_add(measured).ok_or_else(|| {
                chapter_error("chapter boundary overflowed u64 samples".to_string())
            })?
        };
        if end_sample <= start_sample || end_sample > merged_total_samples {
            return Err(chapter_error(format!(
                "merged structural track {:?} produces an invalid chapter range {}..{} within {} samples",
                track_id, start_sample, end_sample, merged_total_samples
            )));
        }
        entries.push(ChapterWriteEntry {
            title: chapter_title(track),
            start_sample,
            end_sample,
        });
        start_sample = end_sample;
    }

    if entries
        .last()
        .is_none_or(|entry| entry.end_sample != merged_total_samples)
    {
        return Err(chapter_error(
            "chapter partition does not cover the merged output".to_string(),
        ));
    }
    Ok(entries)
}

fn chapter_title(track: &PreparedTrack) -> String {
    track
        .metadata
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .chars()
                .map(|character| match character {
                    '\r' | '\n' | '\t' => ' ',
                    other => other,
                })
                .collect()
        })
        .unwrap_or_else(|| format!("Track {:02}", track.id.track_number))
}

fn render_ffmetadata(
    entries: &[ChapterWriteEntry],
    sample_rate: u32,
) -> Result<String, PublishError> {
    if sample_rate == 0 {
        return Err(chapter_error(
            "cannot serialize chapters with a zero sample rate".to_string(),
        ));
    }
    let mut output = String::from(";FFMETADATA1\n");
    for entry in entries {
        if entry.end_sample <= entry.start_sample {
            return Err(chapter_error(format!(
                "cannot serialize empty chapter range {}..{}",
                entry.start_sample, entry.end_sample
            )));
        }
        output.push_str("[CHAPTER]\n");
        output.push_str(&format!("TIMEBASE=1/{sample_rate}\n"));
        output.push_str(&format!("START={}\n", entry.start_sample));
        output.push_str(&format!("END={}\n", entry.end_sample));
        output.push_str("title=");
        output.push_str(&ffmetadata_escape(&entry.title));
        output.push('\n');
    }
    Ok(output)
}

fn ffmetadata_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '=' | ';' | '#' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\r' | '\n' | '\t' => escaped.push(' '),
            other => escaped.push(other),
        }
    }
    escaped
}

fn chapter_remux_command(
    input: &Path,
    metadata: &Path,
    output: &Path,
    muxer: &'static str,
) -> ToolCommand {
    ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Ffmpeg,
        args: vec![
            "-y".into(),
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            input.display().to_string(),
            "-f".into(),
            "ffmetadata".into(),
            "-i".into(),
            metadata.display().to_string(),
            // Do not map MP4 data streams: FFmpeg represents an existing
            // chapter table as a `text` data track. Copying that track while
            // also supplying `-map_chapters 1` makes retries non-idempotent
            // (and can make the ipod muxer reject its own prior output). Keep
            // the audio program, artwork/video, user subtitles, and ordinary
            // attachments; rebuild chapter data from the authoritative table.
            "-map".into(),
            "0:a".into(),
            "-map".into(),
            "0:v?".into(),
            "-map".into(),
            "0:s?".into(),
            "-map".into(),
            "0:t?".into(),
            "-map_metadata".into(),
            "0".into(),
            "-map_chapters".into(),
            "1".into(),
            "-c".into(),
            "copy".into(),
            "-f".into(),
            muxer.into(),
            output.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(6 * 60 * 60),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioTimelineProbe {
    sample_rate: u32,
    samples: u64,
}

async fn probe_merged_source_track_samples(
    merged: &MergedArtifact,
    merged_sample_rate: u32,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<Vec<u64>, PublishError> {
    if merged.source_track_staged_paths.len() != merged.source_tracks.len() {
        return Err(chapter_error(format!(
            "merged structural output has {} track identities but {} retained encoded carriers",
            merged.source_tracks.len(),
            merged.source_track_staged_paths.len()
        )));
    }

    let mut samples = Vec::with_capacity(merged.source_track_staged_paths.len());
    for (index, path) in merged.source_track_staged_paths.iter().enumerate() {
        let probe = probe_audio_timeline(
            path,
            runner,
            cancel,
            tool_concurrency_limits,
        )
        .await?;
        if probe.sample_rate != merged_sample_rate {
            return Err(chapter_error(format!(
                "encoded carrier {} for structural track {} uses {} Hz but merged output uses {} Hz",
                path.display(),
                index + 1,
                probe.sample_rate,
                merged_sample_rate
            )));
        }
        if probe.samples == 0 {
            return Err(chapter_error(format!(
                "encoded carrier {} for structural track {} has zero measured samples",
                path.display(),
                index + 1
            )));
        }
        samples.push(probe.samples);
    }
    Ok(samples)
}

async fn probe_audio_timeline(
    path: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_concurrency_limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<AudioTimelineProbe, PublishError> {
    let command = ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        binary: ToolBinary::Ffprobe,
        args: vec![
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a:0".into(),
            "-show_entries".into(),
            "stream=sample_rate,duration_ts,time_base,duration".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            path.display().to_string(),
        ],
        secret_args: vec![],
        cwd: None,
        env: vec![],
        timeout: Duration::from_secs(30),
    };
    let output = run_tool_command_with_concurrency(
        command,
        runner,
        cancel,
        tool_concurrency_limits,
    )
    .await
    .map_err(|error| {
        chapter_error(format!(
            "cannot probe audio timeline for {} before chapter write: {error}",
            path.display()
        ))
    })?;
    parse_audio_timeline_probe(&output.stdout_tail).map_err(|error| {
        chapter_error(format!(
            "cannot parse audio timeline for {} before chapter write: {error}",
            path.display()
        ))
    })
}

fn parse_audio_timeline_probe(json: &str) -> Result<AudioTimelineProbe, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("invalid ffprobe JSON: {error}"))?;
    let stream = value
        .pointer("/streams/0")
        .ok_or_else(|| "ffprobe returned no audio stream".to_string())?;
    let sample_rate = stream
        .get("sample_rate")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|sample_rate| *sample_rate != 0)
        .ok_or_else(|| "ffprobe returned no valid sample rate".to_string())?;

    let exact_samples = (|| {
        let duration_ts = stream
            .get("duration_ts")
            .and_then(json_u64)?;
        let time_base = stream.get("time_base")?.as_str()?;
        let (num, den) = time_base.split_once('/')?;
        let num = num.parse::<u64>().ok()?;
        let den = den.parse::<u64>().ok()?;
        if den == 0 {
            return None;
        }
        let numerator = u128::from(duration_ts)
            .checked_mul(u128::from(num))?
            .checked_mul(u128::from(sample_rate))?;
        let rounded = numerator
            .checked_add(u128::from(den) / 2)?
            .checked_div(u128::from(den))?;
        u64::try_from(rounded).ok()
    })();

    let samples = exact_samples.or_else(|| {
        let duration = stream
            .get("duration")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<f64>().ok())
            .or_else(|| {
                value
                    .pointer("/format/duration")
                    .and_then(|value| value.as_str())
                    .and_then(|value| value.parse::<f64>().ok())
            })?;
        if !duration.is_finite() || duration <= 0.0 {
            return None;
        }
        let samples = duration * f64::from(sample_rate);
        if !samples.is_finite() || samples <= 0.0 || samples > u64::MAX as f64 {
            return None;
        }
        Some(samples.round() as u64)
    });
    let samples = samples.filter(|samples| *samples != 0).ok_or_else(|| {
        "ffprobe returned no usable positive duration for the audio stream".to_string()
    })?;

    Ok(AudioTimelineProbe {
        sample_rate,
        samples,
    })
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn verify_written_chapters(
    path: &Path,
    expected: &[ChapterWriteEntry],
    sample_rate: u32,
) -> Result<(), PublishError> {
    let raw = crate::convert::chapter_structure::read_embedded_chapters(path).map_err(|error| {
        chapter_error(format!(
            "cannot read back chapters from rewritten {}: {error}",
            path.display()
        ))
    })?;
    if raw.len() != expected.len() {
        return Err(chapter_error(format!(
            "rewritten {} contains {} chapters, expected {}",
            path.display(),
            raw.len(),
            expected.len()
        )));
    }
    let actual = crate::convert::chapter_structure::normalize_embedded_chapters(&raw, sample_rate)
        .map_err(|error| {
            chapter_error(format!(
                "rewritten {} contains unusable chapter geometry: {error}",
                path.display()
            ))
        })?;

    for (index, ((raw_chapter, actual_chapter), expected_chapter)) in raw
        .iter()
        .zip(actual.iter())
        .zip(expected.iter())
        .enumerate()
    {
        let actual_start = actual_chapter.boundary.start_sample;
        let actual_end = actual_start
            .checked_add(actual_chapter.boundary.samples)
            .ok_or_else(|| chapter_error("read-back chapter range overflowed u64".to_string()))?;
        let tolerance = chapter_clock_tolerance_samples(raw_chapter, sample_rate);
        if actual_start.abs_diff(expected_chapter.start_sample) > tolerance
            || actual_end.abs_diff(expected_chapter.end_sample) > tolerance
        {
            return Err(chapter_error(format!(
                "rewritten {} chapter {} boundary drifted beyond container clock tolerance: expected {}..{}, read back {}..{}, tolerance {} samples",
                path.display(),
                index + 1,
                expected_chapter.start_sample,
                expected_chapter.end_sample,
                actual_start,
                actual_end,
                tolerance
            )));
        }
        if actual_chapter.title.as_deref() != Some(expected_chapter.title.as_str()) {
            return Err(chapter_error(format!(
                "rewritten {} chapter {} title mismatch: expected {:?}, read back {:?}",
                path.display(),
                index + 1,
                expected_chapter.title,
                actual_chapter.title
            )));
        }
    }
    Ok(())
}

fn chapter_clock_tolerance_samples(
    chapter: &crate::convert::chapter_structure::RawEmbeddedChapter,
    sample_rate: u32,
) -> u64 {
    if chapter.time_base_num <= 0 || chapter.time_base_den <= 0 {
        return 1;
    }
    let numerator = u128::from(sample_rate)
        .saturating_mul(chapter.time_base_num as u128);
    let denominator = (chapter.time_base_den as u128).saturating_mul(2).max(1);
    let half_tick_ceil = numerator
        .saturating_add(denominator - 1)
        .checked_div(denominator)
        .unwrap_or(u128::MAX);
    u64::try_from(half_tick_ceil)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

fn chapter_error(message: String) -> PublishError {
    PublishError::ChapterStructure(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::pipeline::tool::StubToolRunner;
    use crate::convert::pipeline::types::{
        AlbumMetadata, ExtractionProvenance, SourceAudioDescriptor, SourceAudioCoding, TrackId,
        TrackMetadata, TrackSourceRef,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn track(number: u32, title: &str) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: number,
                disc_number: None,
                track_number: number,
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("{number}.wav"))),
            metadata: TrackMetadata {
                title: Some(title.to_string()),
                track_number: Some(number),
                ..TrackMetadata::default()
            },
            expected_samples: Some(100),
            sample_rate: Some(48_000),
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(48_000),
                Some(16),
                Some(SourceAudioCoding::Pcm),
            ),
            bit_depth: Some(16),
            warnings: vec![],
        }
    }

    fn source() -> PreparedSource {
        let mut extra = BTreeMap::new();
        extra.insert(
            EMBEDDED_CHAPTER_STRUCTURE_EXTRA_KEY.to_string(),
            EMBEDDED_CHAPTER_STRUCTURE_VERSION.to_string(),
        );
        PreparedSource {
            container: PathBuf::from("book.m4b"),
            kind: SourceKind::SingleFile,
            tracks: vec![track(1, "One"), track(2, "Two")],
            album_metadata: AlbumMetadata {
                album: Some("Book".to_string()),
                total_tracks: 2,
                extra,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    #[test]
    fn one_chapter_structure_covers_the_entire_merged_timeline() {
        let mut source = source();
        source.tracks.truncate(1);
        source.album_metadata.total_tracks = 1;
        source.tracks[0].metadata.title = Some("Prologue".to_string());

        for extension in ["m4a", "m4b"] {
            let merged = MergedArtifact {
                staged_path: PathBuf::from(format!("merged.{extension}")),
                final_path: PathBuf::from(format!("merged.{extension}")),
                total_samples: 48_123,
                source_tracks: vec![source.tracks[0].id.clone()],
                source_track_staged_paths: vec![PathBuf::from(format!("1.{extension}"))],
                source_track_exact_samples: vec![],
                timeline_sample_rate: None,
                planned_command_hash: None,
            };

            let entries =
                chapter_entries_for_merged(&merged, &source, &[48_000], 48_123, 48_000)
                    .expect("single structural chapter must remain serializable");
            assert_eq!(
                entries,
                vec![ChapterWriteEntry {
                    title: "Prologue".to_string(),
                    start_sample: 0,
                    end_sample: 48_123,
                }],
                "{extension}"
            );

            let metadata = render_ffmetadata(&entries, 48_000)
                .expect("a single structural chapter must serialize normally");
            assert_eq!(metadata.matches("[CHAPTER]").count(), 1, "{extension}");
            assert!(metadata.contains("START=0\n"), "{extension}");
            assert!(metadata.contains("END=48123\n"), "{extension}");
            assert!(metadata.contains("title=Prologue\n"), "{extension}");
            assert!(chapter_output_muxer(&merged.final_path).is_some(), "{extension}");
        }
    }

    #[test]
    fn one_track_without_structure_is_not_a_chapter_source() {
        let mut source = source();
        source.tracks.truncate(1);
        source.album_metadata.total_tracks = 1;
        source
            .album_metadata
            .extra
            .remove(EMBEDDED_CHAPTER_STRUCTURE_EXTRA_KEY);

        assert!(!source_carries_program_structure(&source));
        assert!(!merged_source_requires_structure_sidecar(&source));
    }

    #[tokio::test]
    async fn merged_multitrack_pcm_targets_get_default_cue_without_probe_or_opt_in() {
        let mut source = source();
        source
            .album_metadata
            .extra
            .remove(EMBEDDED_CHAPTER_STRUCTURE_EXTRA_KEY);
        source.album_metadata.album_artist = Some("Resolved Author".to_string()).into();
        source.tracks[0].metadata.title = Some("Prologue".to_string());
        source.tracks[1].metadata.title = Some("Chapter One".to_string());

        for extension in ["flac", "wav"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let staged_path = temp.path().join(format!("merged.{extension}"));
            let final_dir = temp.path().join("published");
            let final_path = final_dir.join(format!("Resolved Book.{extension}"));
            let merged = MergedArtifact {
                staged_path,
                final_path,
                total_samples: 144_000,
                source_tracks: source.tracks.iter().map(|track| track.id.clone()).collect(),
                source_track_staged_paths: Vec::new(),
                source_track_exact_samples: vec![48_000, 96_000],
                timeline_sample_rate: Some(48_000),
                planned_command_hash: None,
            };
            let mut artifacts = ArtifactSet {
                audio: AudioArtifacts::Merged(merged),
                sidecars: Vec::new(),
            };
            let runner = StubToolRunner::new();

            finalize_structured_chapters_before_publish(
                &mut artifacts,
                &source,
                &runner,
                &CancellationToken::new(),
                false,
                None,
            )
            .await
            .expect("structured PCM merge must stage its default CUE");

            assert!(
                runner.transcript().is_empty(),
                "lossless exact-timeline fast path must not spawn probes for {extension}"
            );
            assert_eq!(artifacts.sidecars.len(), 1, "{extension}");
            let sidecar = &artifacts.sidecars[0];
            assert!(matches!(&sidecar.kind, SidecarKind::CueSheet));
            assert_eq!(sidecar.final_path, final_dir.join("album.cue"));
            let cue = fs::read_to_string(&sidecar.staged_path).expect("generated CUE");
            assert!(cue.contains("PERFORMER \"Resolved Author\""), "{extension}: {cue}");
            assert!(
                cue.contains(&format!("FILE \"Resolved Book.{extension}\" WAVE")),
                "{extension}: {cue}"
            );
            assert!(cue.contains("TITLE \"Prologue\""), "{extension}: {cue}");
            assert!(cue.contains("TITLE \"Chapter One\""), "{extension}: {cue}");
            assert!(cue.contains("INDEX 01 00:00:00"), "{extension}: {cue}");
            assert!(cue.contains("INDEX 01 00:01:00"), "{extension}: {cue}");

            // Re-finalization replaces the same logical sidecar rather than
            // accumulating a second CUE, and the deterministic content is stable.
            finalize_structured_chapters_before_publish(
                &mut artifacts,
                &source,
                &runner,
                &CancellationToken::new(),
                false,
                None,
            )
            .await
            .expect("repeat structural finalization");
            assert_eq!(artifacts.sidecars.len(), 1, "{extension}");
            assert_eq!(
                fs::read_to_string(&artifacts.sidecars[0].staged_path).expect("repeat CUE"),
                cue,
                "{extension}"
            );
            assert!(runner.transcript().is_empty(), "{extension}");
        }
    }

    #[tokio::test]
    async fn one_structural_chapter_merged_to_flac_gets_one_entry_cue() {
        let mut source = source();
        source.tracks.truncate(1);
        source.album_metadata.total_tracks = 1;
        source.tracks[0].metadata.title = Some("Prologue".to_string());

        let temp = tempfile::tempdir().expect("tempdir");
        let merged = MergedArtifact {
            staged_path: temp.path().join("merged.flac"),
            final_path: temp.path().join("Book.flac"),
            total_samples: 48_000,
            source_tracks: vec![source.tracks[0].id.clone()],
            source_track_staged_paths: Vec::new(),
            source_track_exact_samples: vec![48_000],
            timeline_sample_rate: Some(48_000),
            planned_command_hash: None,
        };
        let mut artifacts = ArtifactSet {
            audio: AudioArtifacts::Merged(merged),
            sidecars: Vec::new(),
        };
        let runner = StubToolRunner::new();

        finalize_structured_chapters_before_publish(
            &mut artifacts,
            &source,
            &runner,
            &CancellationToken::new(),
            false,
            None,
        )
        .await
        .expect("one real structural chapter remains structure on FLAC merge");

        let cue = fs::read_to_string(&artifacts.sidecars[0].staged_path).expect("generated CUE");
        assert_eq!(cue.matches("  TRACK ").count(), 1);
        assert!(cue.contains("TITLE \"Prologue\""));
        assert!(cue.contains("INDEX 01 00:00:00"));
        assert!(runner.transcript().is_empty());
    }

    #[test]
    fn merged_boundaries_assign_small_concat_clock_delta_to_first_chapter() {
        let source = source();
        let merged = MergedArtifact {
            staged_path: PathBuf::from("merged.m4b"),
            final_path: PathBuf::from("merged.m4b"),
            total_samples: 205,
            source_tracks: source.tracks.iter().map(|track| track.id.clone()).collect(),
            source_track_staged_paths: vec![PathBuf::from("1.m4a"), PathBuf::from("2.m4a")],
            source_track_exact_samples: vec![],
            timeline_sample_rate: None,
            planned_command_hash: None,
        };
        let entries = chapter_entries_for_merged(&merged, &source, &[100, 100], 205, 48_000)
            .expect("chapter entries");
        assert_eq!(entries[0].start_sample, 0);
        assert_eq!(entries[0].end_sample, 105);
        assert_eq!(entries[1].start_sample, 105);
        assert_eq!(entries[1].end_sample, 205);
    }

    #[test]
    fn merged_boundaries_reject_large_concat_clock_drift() {
        let source = source();
        let merged = MergedArtifact {
            staged_path: PathBuf::from("merged.m4b"),
            final_path: PathBuf::from("merged.m4b"),
            total_samples: 50_000,
            source_tracks: source.tracks.iter().map(|track| track.id.clone()).collect(),
            source_track_staged_paths: vec![PathBuf::from("1.m4a"), PathBuf::from("2.m4a")],
            source_track_exact_samples: vec![],
            timeline_sample_rate: None,
            planned_command_hash: None,
        };
        let error = chapter_entries_for_merged(&merged, &source, &[100, 100], 50_000, 48_000)
            .expect_err("large concat drift must be refused");
        assert!(error.to_string().contains("concat tolerance"));
    }

    #[test]
    fn chapter_title_normalizes_control_whitespace() {
        let chapter = track(1, "  Alpha\nBeta\tGamma  ");
        assert_eq!(chapter_title(&chapter), "Alpha Beta Gamma");
    }

    #[test]
    fn ffmetadata_escapes_control_syntax_without_multiline_titles() {
        assert_eq!(ffmetadata_escape("A # B; C=D\\E\nF"), "A \\# B\\; C\\=D\\\\E F");
    }

    #[test]
    fn timeline_probe_prefers_rational_stream_duration() {
        let probe = parse_audio_timeline_probe(
            r#"{
              "streams": [{
                "sample_rate": "48000",
                "duration_ts": 144001,
                "time_base": "1/48000",
                "duration": "999.0"
              }],
              "format": { "duration": "999.0" }
            }"#,
        )
        .expect("timeline probe");
        assert_eq!(probe.sample_rate, 48_000);
        assert_eq!(probe.samples, 144_001);
    }

    fn read_fixture_mp4_ilst(path: &Path) -> lofty::mp4::Ilst {
        use lofty::file::AudioFile as _;

        let mut file = fs::File::open(path).expect("open MP4 fixture");
        let mp4 = lofty::mp4::Mp4File::read_from(
            &mut file,
            lofty::config::ParseOptions::new().read_properties(false),
        )
        .expect("parse MP4 fixture");
        mp4.ilst().cloned().unwrap_or_default()
    }

    fn seed_authoring_ilst_preservation_fixture(path: &Path) -> lofty::mp4::Ilst {
        use lofty::config::WriteOptions;
        use lofty::file::AudioFile as _;

        let artist = lofty::mp4::AtomIdent::Fourcc(*b"\xa9ART");
        let title = lofty::mp4::AtomIdent::Fourcc(*b"\xa9nam");
        let note = lofty::mp4::AtomIdent::Freeform {
            mean: std::borrow::Cow::Borrowed("com.apple.iTunes"),
            name: std::borrow::Cow::Borrowed("MY_NOTE"),
        };
        let performer = lofty::mp4::AtomIdent::Freeform {
            mean: std::borrow::Cow::Borrowed("com.apple.iTunes"),
            name: std::borrow::Cow::Borrowed("PERFORMER"),
        };
        let mut ilst = lofty::mp4::Ilst::new();
        ilst.insert(lofty::mp4::Atom::new(
            title,
            lofty::mp4::AtomData::UTF8("Chapter Test".to_string()),
        ));
        ilst.insert(
            lofty::mp4::Atom::from_collection(
                artist,
                vec![
                    lofty::mp4::AtomData::UTF8("Artist One".to_string()),
                    lofty::mp4::AtomData::UTF8("Artist Two".to_string()),
                ],
            )
            .expect("artist atom"),
        );
        ilst.insert(lofty::mp4::Atom::new(
            note,
            lofty::mp4::AtomData::UTF8("keep-me".to_string()),
        ));
        ilst.insert(
            lofty::mp4::Atom::from_collection(
                performer,
                vec![
                    lofty::mp4::AtomData::UTF8("Performer One".to_string()),
                    lofty::mp4::AtomData::UTF8("Performer Two".to_string()),
                ],
            )
            .expect("performer atom"),
        );

        let mut file = fs::File::open(path).expect("open MP4 fixture for ilst seed");
        let mut mp4 = lofty::mp4::Mp4File::read_from(
            &mut file,
            lofty::config::ParseOptions::new().read_properties(false),
        )
        .expect("parse MP4 fixture for ilst seed");
        drop(file);
        mp4.set_ilst(ilst);
        mp4.save_to_path(path, WriteOptions::default())
            .expect("save MP4 ilst preservation fixture");
        read_fixture_mp4_ilst(path)
    }

    fn ilst_text_values(
        ilst: &lofty::mp4::Ilst,
        ident: &lofty::mp4::AtomIdent<'_>,
    ) -> Vec<String> {
        ilst.into_iter()
            .filter(|atom| atom.ident() == ident)
            .flat_map(|atom| atom.data())
            .filter_map(|data| match data {
                lofty::mp4::AtomData::UTF8(value) | lofty::mp4::AtomData::UTF16(value) => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn authoring_ilst_snapshot_restores_freeform_and_repeated_values_idempotently() {
        use lofty::config::WriteOptions;
        use lofty::file::AudioFile as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("authoring-ilst.m4a");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/metadata_persistence/mp4.m4a"),
            &path,
        )
        .expect("copy MP4 fixture");
        let expected = seed_authoring_ilst_preservation_fixture(&path);
        let snapshot = snapshot_mp4_ilst(&path).expect("snapshot concrete ilst");
        assert_eq!(snapshot.ilst.as_ref(), Some(&expected));

        // Simulate exactly the class of MOV-remux damage this authoring path
        // must repair: keep one scalar and one ARTIST value, drop freeforms and
        // the second repeated values.
        let mut file = fs::File::open(&path).expect("open fixture before simulated strip");
        let mut mp4 = lofty::mp4::Mp4File::read_from(
            &mut file,
            lofty::config::ParseOptions::new().read_properties(false),
        )
        .expect("parse fixture before simulated strip");
        drop(file);
        let mut stripped = lofty::mp4::Ilst::new();
        stripped.insert(lofty::mp4::Atom::new(
            lofty::mp4::AtomIdent::Fourcc(*b"\xa9nam"),
            lofty::mp4::AtomData::UTF8("Chapter Test".to_string()),
        ));
        stripped.insert(lofty::mp4::Atom::new(
            lofty::mp4::AtomIdent::Fourcc(*b"\xa9ART"),
            lofty::mp4::AtomData::UTF8("Artist One".to_string()),
        ));
        mp4.set_ilst(stripped);
        mp4.save_to_path(&path, WriteOptions::default())
            .expect("save simulated stripped ilst");
        assert_ne!(read_fixture_mp4_ilst(&path), expected);

        restore_mp4_ilst(&path, &snapshot).expect("restore complete concrete ilst");
        let restored = read_fixture_mp4_ilst(&path);
        assert_eq!(restored, expected);

        let artist = lofty::mp4::AtomIdent::Fourcc(*b"\xa9ART");
        let note = lofty::mp4::AtomIdent::Freeform {
            mean: std::borrow::Cow::Borrowed("com.apple.iTunes"),
            name: std::borrow::Cow::Borrowed("MY_NOTE"),
        };
        let performer = lofty::mp4::AtomIdent::Freeform {
            mean: std::borrow::Cow::Borrowed("com.apple.iTunes"),
            name: std::borrow::Cow::Borrowed("PERFORMER"),
        };
        assert_eq!(
            ilst_text_values(&restored, &artist),
            vec!["Artist One".to_string(), "Artist Two".to_string()]
        );
        assert_eq!(ilst_text_values(&restored, &note), vec!["keep-me".to_string()]);
        assert_eq!(
            ilst_text_values(&restored, &performer),
            vec!["Performer One".to_string(), "Performer Two".to_string()]
        );

        // Re-applying the same post-remux restoration is semantically stable.
        restore_mp4_ilst(&path, &snapshot).expect("repeat complete ilst restoration");
        assert_eq!(read_fixture_mp4_ilst(&path), expected);
    }

    #[tokio::test]
    async fn authoring_real_ffmpeg_rewrite_preserves_ilst_on_repeat_save_when_available() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map_or(true, |output| !output.status.success())
        {
            eprintln!("skipping real chapter-authoring MP4 rewrite: ffmpeg unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("authoring-real-remux.m4a");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/metadata_persistence/mp4.m4a"),
            &path,
        )
        .expect("copy MP4 fixture");
        let expected_ilst = seed_authoring_ilst_preservation_fixture(&path);
        let chapters = vec![
            ("One".to_string(), 0, 200),
            ("Two".to_string(), 200, 400),
        ];
        let runner = crate::convert::pipeline::tool::RealToolRunner::new(
            std::collections::HashMap::new(),
        );
        let cancel = CancellationToken::new();

        rewrite_embedded_chapters_for_authoring(&path, &chapters, 8_000, &runner, &cancel)
            .await
            .expect("first real chapter-authoring rewrite");
        assert_eq!(read_fixture_mp4_ilst(&path), expected_ilst);
        verify_written_chapters(
            &path,
            &[
                ChapterWriteEntry {
                    title: "One".to_string(),
                    start_sample: 0,
                    end_sample: 200,
                },
                ChapterWriteEntry {
                    title: "Two".to_string(),
                    start_sample: 200,
                    end_sample: 400,
                },
            ],
            8_000,
        )
        .expect("chapters survive first ilst restoration");

        rewrite_embedded_chapters_for_authoring(&path, &chapters, 8_000, &runner, &cancel)
            .await
            .expect("second real chapter-authoring rewrite");
        assert_eq!(read_fixture_mp4_ilst(&path), expected_ilst);
        verify_written_chapters(
            &path,
            &[
                ChapterWriteEntry {
                    title: "One".to_string(),
                    start_sample: 0,
                    end_sample: 200,
                },
                ChapterWriteEntry {
                    title: "Two".to_string(),
                    start_sample: 200,
                    end_sample: 400,
                },
            ],
            8_000,
        )
        .expect("chapters survive repeated ilst restoration");
    }

    #[tokio::test]
    async fn authoring_real_ffmpeg_round_trips_matroska_and_webm_on_repeat_save_when_available() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map_or(true, |output| !output.status.success())
        {
            eprintln!("skipping real Matroska/WebM chapter rewrite: ffmpeg unavailable");
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let chapters = vec![
            ("One".to_string(), 0, 48_000),
            ("Two".to_string(), 48_000, 96_000),
        ];
        let expected = [
            ChapterWriteEntry {
                title: "One".to_string(),
                start_sample: 0,
                end_sample: 48_000,
            },
            ChapterWriteEntry {
                title: "Two".to_string(),
                start_sample: 48_000,
                end_sample: 96_000,
            },
        ];
        let runner = crate::convert::pipeline::tool::RealToolRunner::new(
            std::collections::HashMap::new(),
        );
        let cancel = CancellationToken::new();

        for (extension, muxer) in [
            ("mka", "matroska"),
            ("mkv", "matroska"),
            ("webm", "webm"),
            ("weba", "webm"),
        ] {
            let path = temp.path().join(format!("chapter-roundtrip.{extension}"));
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-nostdin",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=1000:sample_rate=48000:duration=2",
                    "-c:a",
                    "libopus",
                    "-f",
                    muxer,
                ])
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .status()
                .expect("launch ffmpeg fixture seed");
            if !status.success() {
                eprintln!(
                    "skipping real {extension} chapter rewrite: ffmpeg could not seed a libopus fixture"
                );
                continue;
            }

            for attempt in 1..=2 {
                rewrite_embedded_chapters_for_authoring(
                    &path,
                    &chapters,
                    48_000,
                    &runner,
                    &cancel,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{extension} chapter rewrite attempt {attempt} failed: {error}")
                });
                verify_written_chapters(&path, &expected, 48_000).unwrap_or_else(|error| {
                    panic!("{extension} chapter verification attempt {attempt} failed: {error}")
                });
            }
        }
    }

    #[test]
    fn remux_is_stream_copy_and_replaces_chapters_only() {
        let command = chapter_remux_command(
            Path::new("in.m4b"),
            Path::new("chapters.ffmeta"),
            Path::new("out.m4b"),
            "ipod",
        );
        assert!(command.args.windows(2).any(|pair| pair == ["-c", "copy"]));
        assert!(command
            .args
            .windows(2)
            .any(|pair| pair == ["-map_chapters", "1"]));
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "0:a"]));
        assert!(!command.args.windows(2).any(|pair| pair == ["-map", "0"]));
        assert!(command
            .args
            .windows(2)
            .any(|pair| pair == ["-map_metadata", "0"]));
    }

    #[test]
    fn chapter_muxer_selection_uses_central_container_capability() {
        assert_eq!(chapter_output_muxer(Path::new("book.m4a")), Some("ipod"));
        assert_eq!(chapter_output_muxer(Path::new("book.m4b")), Some("ipod"));
        assert_eq!(chapter_output_muxer(Path::new("book.mp4")), Some("mp4"));
        assert_eq!(chapter_output_muxer(Path::new("book.mka")), Some("matroska"));
        assert_eq!(chapter_output_muxer(Path::new("book.mkv")), Some("matroska"));
        assert_eq!(chapter_output_muxer(Path::new("book.webm")), Some("webm"));
        assert_eq!(chapter_output_muxer(Path::new("book.weba")), Some("webm"));
        assert_eq!(chapter_output_muxer(Path::new("book.flac")), None);
    }
}

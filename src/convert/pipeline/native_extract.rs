//! Native disc extraction, separate from the Convert DSP/encoder pipeline.
//!
//! Materializers resolve the authored track/chapter, realizers decode its
//! carrier into a private WAV, and only a *completed* final file is published
//! into the destination. No conversion settings, ReplayGain, resampler, DSP
//! effect, or lossy encoder is involved. All staging stays in TonePoet's cache
//! or the configured scratch directory; cross-volume publication copies through
//! a cleaned-up temporary file at the final target before atomic rename.

use std::collections::{BTreeSet, HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use super::native_wav::{inspect_carrier, package_raw_pcm, concatenate_generated_waves, NativePcmFormat};
use super::actions::ActionPipeline;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use crate::disc::{DiscFormat, PresentationId};
use super::bluray_realize::realize_bluray_track_native;
use super::dvda_realize::{realize_dvda_track, DvdaRealizationAudioPolicy, DvdaSourceAudioExpectation};
use super::dvdv_realize::realize_dvdv_track_native;
use super::materializer_bluray::BlurayMaterializer;
use super::materializer_dvda::DvdaAudioMaterializer;
use super::materializer_dvdv::DvdVideoMaterializer;
use super::materializer_sacd::SacdIsoMaterializer;
use super::stages::Materializer;
use super::tool::{RealToolRunner, ToolBinary, ToolCommand, ToolRunner};
use super::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeWavContainer { Automatic, Riff, Rf64, W64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeOverwritePolicy { Fail, Skip, Replace }

/// Tags collected from actually probed disc/sidecar evidence. A missing
/// field stays missing; generic filename labels never become fabricated tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NativeTagValues {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum NativeTagSource { DiscAndSidecar, AuthoredTrack }
fn default_tag_source() -> NativeTagSource { NativeTagSource::DiscAndSidecar }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeTrackChoice {
    pub number: u32,
    pub name: String,
    #[serde(default)]
    pub tags: NativeTagValues,
}

fn default_angle_one() -> u8 { 1 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeProgramChoice {
    pub id: PresentationId,
    #[serde(default = "default_angle_one")]
    pub angle: u8,
    pub label: String,
    pub tracks: Vec<NativeTrackChoice>,
    pub keep_dst: bool,
    pub source_depth: Option<u32>,
    pub source_channels: Option<u8>,
    pub source_rate: Option<u32>,
    /// Source-reported layout, not derived from the channel count.
    #[serde(default)]
    pub source_layout: Option<String>,
    pub is_lossless: bool,
    #[serde(default)]
    pub tags: NativeTagValues,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeExtractionJob {
    pub source: PathBuf,
    pub format: DiscFormat,
    pub programs: Vec<NativeProgramChoice>,
    pub destination: PathBuf,
    pub working_root: PathBuf,
    pub wav_container: NativeWavContainer,
    pub assume_decrypted: bool,
    pub downmix: DvdaDownmixPolicy,
    #[serde(default = "default_naming_template")]
    pub naming_template: String,
    #[serde(default = "default_yes")]
    pub program_subfolders: bool,
    #[serde(default = "default_overwrite_policy")]
    pub overwrite_policy: NativeOverwritePolicy,
    #[serde(default)]
    pub extraction_log: bool,
    #[serde(default)]
    pub file_per_program: bool,
    #[serde(default)]
    pub cue: bool,
    #[serde(default)]
    pub transfer_tags: bool,
    #[serde(default = "default_tag_source")]
    pub tag_source: NativeTagSource,
    #[serde(default = "default_tag_fields")]
    pub tag_fields: u16,
    #[serde(default)]
    pub tag_artwork: bool,
}
fn default_tag_fields() -> u16 { 0x7f }
fn default_naming_template() -> String { "{track} - {title}".into() }
fn default_yes() -> bool { true }
fn default_overwrite_policy() -> NativeOverwritePolicy { NativeOverwritePolicy::Fail }

#[derive(Debug, Clone)]
pub struct NativeExtractionProgress {
    pub done: usize,
    pub total: usize,
    pub published: usize,
    pub current: String,
}

#[derive(Debug, Clone)]
pub struct NativeExtractionReport {
    pub published: Vec<PathBuf>,
}

/// Facts observed by the pre-extraction materializer. A successful probe must
/// not discard the format information needed by the size estimator and writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeProbeFacts {
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Make one private temporary directory per extraction.  `TempDir` owns and
/// removes the entire hierarchy on ordinary completion, error and cancellation.
/// An OS crash can leave only files in working storage, never partial outputs.
pub async fn execute(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    let mut published = 0usize;
    let destination = job.destination.clone();
    let result = execute_inner(job, cancel, |progress| {
        published = published.max(progress.published);
        on_progress(progress);
    }).await;
    result.map_err(|error| {
        if published == 0 { error }
        else { format!("{error}; {published} completed file(s) remain published under '{}'", destination.display()) }
    })
}

/// Perform bounded media preflight for the extraction overlay. DVD-Video's
/// existing materializer scans up to 512 authored sectors per selected chapter
/// for positive MPEG PES scrambling evidence. For a fast preview, inspect one
/// chapter per title rather than reading every chapter (the extraction worker
/// still checks each selected chapter independently before publication).
/// Unresolved lossless source depth is a blocker, never a guessed 24-bit tag.
pub async fn probe_selected_sources(
    job: NativeExtractionJob, cancel: CancellationToken,
) -> Result<NativeProbeFacts, String> {
    let temporary = tempfile::tempdir()
        .map_err(|e| format!("Create private probe storage: {e}"))?;
    let staging = StagingDir::borrowed(temporary.path().to_path_buf(), "native-extract-probe".into());
    let runner = RealToolRunner::new(HashMap::new());
    let tools = HashMap::new();
    let mut seen_dvd_titles = HashSet::new();
    let mut observed = NativeProbeFacts::default();
    for (index, program) in job.programs.iter().enumerate() {
        if cancel.is_cancelled() { return Err("Native source probe cancelled".into()); }
        if job.format == DiscFormat::DvdVideo {
            if let PresentationId::DvdVideoTitle { vts_number, title_number, .. } = program.id {
                if !seen_dvd_titles.insert((vts_number, title_number)) { continue; }
            }
        }
        let mut request = materializer_request(&job, program, index)?;
        let number = program.tracks.first().ok_or("Program has no probeable chapter")?.number;
        request.source.track_selection = TrackSelection::Set(BTreeSet::from([number]));
        let materializer: Box<dyn Materializer> = match job.format {
            DiscFormat::Sacd => Box::new(SacdIsoMaterializer),
            DiscFormat::DvdAudio => Box::new(DvdaAudioMaterializer),
            DiscFormat::DvdVideo => Box::new(DvdVideoMaterializer),
            DiscFormat::BluRay => Box::new(BlurayMaterializer),
        };
        let prepared = materializer.materialize(
            &request, &staging, &runner, None, &tools, &cancel
        ).await.map_err(|e| format!("{}: {e}",program.label))?;
        let first = prepared.tracks.first().ok_or_else(||format!("{}: no probed audio",program.label))?;
        let depth = first.bit_depth.or(first.source_audio.bit_depth).or(program.source_depth);
        if job.format != DiscFormat::Sacd && program.is_lossless && depth.is_none() {
            return Err(format!("{}: source depth remains unknown after media probe",program.label));
        }
        let channels = match &first.source_ref {
            TrackSourceRef::DvdVideoTrack { channels, .. } | TrackSourceRef::BluRayTrack { channels, .. } => *channels,
            _ => None,
        }.or(program.source_channels);
        observed = NativeProbeFacts {
            bit_depth: depth,
            sample_rate: first.scalar_sample_rate().or(program.source_rate),
            channels,
        };
    }
    Ok(observed)
}

async fn execute_inner(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    validate_job(&job)?;
    // Reject overlapping scratch before creating anything in working storage.
    // Older persisted jobs may contain the R5 implicit destination-sibling
    // staging path. Move that obsolete default to TonePoet's cache too.
    let work_path = native_working_storage(&job.working_root, &job.destination)?;
    fs::create_dir_all(&work_path).map_err(|e| format!("Working storage: {e}"))?;
    let temporary = tempfile::Builder::new()
        .prefix("tonepoet-extract-")
        .tempdir_in(&work_path)
        .map_err(|e| format!("Cannot allocate private working storage: {e}"))?;
    let staging = StagingDir::borrowed(temporary.path().to_path_buf(), "native-extract".to_string());
    let runner = RealToolRunner::new(HashMap::new());
    let tool_paths = HashMap::new();
    let total: usize = job.programs.iter().map(|p| p.tracks.len()).sum();
    let mut done = 0;
    let mut published = Vec::new();
    let mut reserved_names = HashSet::new();

    for (pi, program) in job.programs.iter().enumerate() {
        if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
        let grouped_target = proposed_destination_for_program(&job, program);
        if job.file_per_program && job.overwrite_policy == NativeOverwritePolicy::Skip
            && fs::symlink_metadata(&grouped_target).is_ok() {
            done += program.tracks.len();
            on_progress(NativeExtractionProgress { done, total, published: published.len(), current: format!("Skipped existing {}",program.label) });
            continue;
        }
        let mut grouped_outputs: Vec<(u32, PathBuf, String)> = Vec::new();
        let mut req = materializer_request(&job, program, pi)?;
        let selection: BTreeSet<u32> = program.tracks.iter().map(|track| track.number).collect();
        req.source.track_selection = TrackSelection::Set(selection);
        let materializer: Box<dyn Materializer> = match job.format {
            DiscFormat::Sacd => Box::new(SacdIsoMaterializer),
            DiscFormat::DvdAudio => Box::new(DvdaAudioMaterializer),
            DiscFormat::DvdVideo => Box::new(DvdVideoMaterializer),
            DiscFormat::BluRay => Box::new(BlurayMaterializer),
        };
        let prepared = materializer.materialize(
            &req, &staging, &runner, None, &tool_paths, &cancel,
        ).await.map_err(|e| format!("{}: {e}", program.label))?;
        if prepared.tracks.len() != program.tracks.len() {
            return Err(format!("{}: selected track inventory changed; refusing partial extraction", program.label));
        }
        for track in &prepared.tracks {
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            let requested = program.tracks.iter().find(|chosen| chosen.number == track.id.source_ordinal)
                .ok_or_else(|| format!("{}: materializer returned unexpected track {}", program.label, track.id.source_ordinal))?;
            on_progress(NativeExtractionProgress { done, total, published: published.len(), current: format!("{} · {}", program.label, requested.name) });
            let extension = extraction_extension(&job, program);
            let dest = proposed_destination(&job, program, requested);
            if !job.file_per_program {
                if !reserved_names.insert(dest.clone()) {
                    return Err(format!("Two selected items would publish to '{}'; choose different names", dest.display()));
                }
                if job.overwrite_policy == NativeOverwritePolicy::Skip && fs::symlink_metadata(&dest).is_ok() {
                    done += 1;
                    on_progress(NativeExtractionProgress { done, total, published: published.len(), current: format!("Skipped existing {}", requested.name) });
                    continue;
                }
                if job.overwrite_policy == NativeOverwritePolicy::Fail && fs::symlink_metadata(&dest).is_ok() {
                    return Err(format!("'{}' already exists; extraction never overwrites", dest.display()));
                }
            }

            let complete = temporary.path().join(format!("completed-{pi}-{}.{extension}", track.id.source_ordinal));
            if job.format == DiscFormat::Sacd {
                let src = track.source_ref.clone();
                let dst = complete.clone();
                let cancel_copy = cancel.clone();
                let keep_dst = program.keep_dst;
                tokio::task::spawn_blocking(move || extract_sacd_native(&src, &dst, keep_dst, &cancel_copy))
                    .await.map_err(|e| format!("SACD extraction task: {e}"))??;
            } else {
                if job.format == DiscFormat::DvdAudio {
                    let rates: BTreeSet<u32> = track.source_audio.channel_groups.iter()
                        .filter_map(|g| g.sample_rate).collect();
                    if rates.len() > 1 ||
                        (track.source_audio.channel_groups.len() > 1 &&
                         track.source_audio.channel_groups.iter().any(|g| g.sample_rate.is_none())) {
                        return Err(format!("{}: channel-group sample rates are mixed or unknown; use Convert", requested.name));
                    }
                }
                let carrier = match &track.source_ref {
                    TrackSourceRef::DvdaTrack { .. } => {
                        let expectation = DvdaSourceAudioExpectation::from_prepared_track_and_source(Some(track), &track.source_ref);
                        let policy = DvdaRealizationAudioPolicy::new("native".to_string(), None, job.downmix).for_native_extraction();
                        realize_dvda_track(&track.source_ref, expectation, policy, &staging, &runner, &cancel, None, None).await
                    }
                    TrackSourceRef::DvdVideoTrack { .. } => {
                        realize_dvdv_track_native(&track.source_ref, &staging, &runner, &cancel, None, None).await
                    }
                    TrackSourceRef::BluRayTrack { .. } => {
                        realize_bluray_track_native(&track.source_ref, &staging, &runner, &cancel, None, None).await
                    }
                    _ => return Err("Unexpected prepared track kind".into()),
                }.map_err(|e| format!("{}: {e}", requested.name))?;
                let depth = if program.is_lossless {
                    track.bit_depth.or(track.source_audio.bit_depth).or(program.source_depth).ok_or_else(||
                        format!("{}: native bit depth is unknown; refusing an unverified output", requested.name)
                    )?
                } else { 24 };
                let output_depth = match depth { 16 => 16, 20 | 24 => 24, _ => return Err(format!("Unsupported native bit depth {depth}")) };
                encode_pcm_container(&runner, &carrier, &complete, output_depth, depth, job.wav_container,
                    track.scalar_sample_rate().or(program.source_rate),
                    if job.downmix.is_active() { None } else { program.source_channels },
                    if job.downmix.is_active() { None } else { program.source_layout.as_deref() }, &cancel).await?;
                // This carrier was made in private staging, never in source.
                // Reclaim it before decoding the next selected item.
                let _ = fs::remove_file(&carrier);
            }
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if job.transfer_tags && !job.file_per_program && !program.keep_dst {
                let entries = extraction_tags(&job, program, requested, track, &prepared.album_metadata);
                let art = if job.tag_artwork { native_artwork_path(&prepared.album_metadata, &job.source) } else { None };
                if !entries.is_empty() || art.is_some() {
                    let complete_copy = complete.clone();
                    let cancel_copy = cancel.clone();
                    tokio::task::spawn_blocking(move || {
                        if cancel_copy.is_cancelled() { return Err("Extraction cancelled before metadata".to_string()); }
                        write_completed_native_metadata(&complete_copy, &entries, art.as_deref())?;
                        if cancel_copy.is_cancelled() { return Err("Extraction cancelled after metadata".to_string()); }
                        Ok::<(),String>(())
                    }).await.map_err(|e|format!("Metadata worker: {e}"))??;
                }
            }
            if fs::metadata(&complete).map_err(|e| format!("Completed file not found: {e}"))?.len() < 64 {
                return Err(format!("{}: extraction produced an empty or truncated output", requested.name));
            }
            File::open(&complete).and_then(|f| f.sync_all())
                .map_err(|e| format!("Cannot synchronize completed file: {e}"))?;
            if job.file_per_program {
                grouped_outputs.push((requested.number, complete, requested.name.clone()));
            } else {
                publish_finished_file(&job, &complete, &dest)?;
                published.push(dest);
            }
            done += 1;
            on_progress(NativeExtractionProgress { done, total, published: published.len(), current: requested.name.clone() });
        }
        if job.file_per_program {
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            grouped_outputs.sort_by_key(|(ordinal,_,_)| *ordinal);
            let parts = grouped_outputs.iter().map(|(_,path,_)|path.clone()).collect::<Vec<_>>();
            let grouped_file = temporary.path().join(format!("continuous-{pi}.{}", extraction_extension(&job, program)));
            let grouped = grouped_file.clone();
            let cancel_copy = cancel.clone();
            let container = job.wav_container;
            let sacd = job.format == DiscFormat::Sacd;
            let keep_dst = program.keep_dst;
            let (sample_counts, rate) = tokio::task::spawn_blocking(move || {
                if sacd { concatenate_sacd_tracks(&parts, &grouped, keep_dst, &cancel_copy) }
                else { concatenate_generated_waves(&parts, &grouped, container, &cancel_copy) }
            }).await.map_err(|e| format!("Continuous native audio worker: {e}"))??;
            let cue_file = if job.cue {
                let file_name = grouped_target.file_name().and_then(|s|s.to_str()).ok_or("Invalid continuous output name")?;
                let names = grouped_outputs.iter().map(|(_,_,name)| name.clone()).collect::<Vec<_>>();
                let cue_text = cue_for_continuous(file_name, &names, &sample_counts, rate)?;
                let path = temporary.path().join(format!("continuous-{pi}.cue"));
                let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
                    .map_err(|e| format!("Cannot create private CUE: {e}"))?;
                file.write_all(cue_text.as_bytes()).and_then(|()|file.sync_all())
                    .map_err(|e| format!("Cannot finish private CUE: {e}"))?;
                Some(path)
            } else { None };
            if job.transfer_tags && !program.keep_dst {
                let root_tags = if job.format == DiscFormat::Sacd { program.tags.clone() }
                else { match job.tag_source {
                    NativeTagSource::DiscAndSidecar => program.tags.clone(),
                    NativeTagSource::AuthoredTrack => NativeTagValues {
                        album: prepared.album_metadata.album.clone(),
                        artist: prepared.album_metadata.album_artist.as_deref().map(str::to_string),
                        genre: prepared.album_metadata.genre.as_deref().map(str::to_string),
                        year: prepared.album_metadata.date.clone(),
                        ..Default::default()
                    },
                }};
                let fields = job.tag_fields;
                let art = if job.tag_artwork { native_artwork_path(&prepared.album_metadata, &job.source) } else { None };
                let grouped_copy = grouped_file.clone();
                tokio::task::spawn_blocking(move || {
                    let values = selected_tag_items(&root_tags, fields);
                    write_completed_native_metadata(&grouped_copy, &values, art.as_deref())
                }).await.map_err(|e|format!("Metadata worker: {e}"))??;
            }
            publish_finished_file(&job, &grouped_file, &grouped_target)?;
            published.push(grouped_target.clone());
            if let Some(cue_file) = cue_file {
                let cue_target = grouped_target.with_extension("cue");
                publish_finished_file(&job, &cue_file, &cue_target)?;
                published.push(cue_target);
            }
            on_progress(NativeExtractionProgress { done, total, published: published.len(), current: format!("Completed {}",program.label) });
        }
    }
    if job.extraction_log {
        let log = temporary.path().join("native-extraction.log");
        let mut output = OpenOptions::new().create_new(true).write(true).open(&log)
            .map_err(|e| format!("Cannot stage extraction log: {e}"))?;
        writeln!(output, "Native extraction from {}", job.source.display()).map_err(|e| e.to_string())?;
        for item in &published { writeln!(output, "Completed: {}", item.display()).map_err(|e|e.to_string())?; }
        output.sync_all().map_err(|e|format!("Cannot sync extraction log: {e}"))?;
        let log_target = job.destination.join("native-extraction.log");
        publish_finished_file(&job, &log, &log_target)?;
        published.push(log_target);
    }
    Ok(NativeExtractionReport { published })
}

/// Publish only completed private artifacts. On a different filesystem the
/// shared Convert publisher copies through a disposable file in the target
/// directory and atomically renames it only after flushing the complete data.
/// A failed copy never exposes a partial result at the requested filename.
fn publish_finished_file(job: &NativeExtractionJob, complete: &Path, target: &Path) -> Result<(), String> {
    if job.overwrite_policy != NativeOverwritePolicy::Replace && fs::symlink_metadata(target).is_ok() {
        return Err(format!("Destination already exists: '{}'",target.display()));
    }
    let parent = target.parent().ok_or("Invalid destination path")?;
    let parent_existed = parent.exists();
    let root_existed = job.destination.exists();
    fs::create_dir_all(parent).map_err(|e|format!("Cannot create destination parent: {e}"))?;
    if let Err(error) = super::transactional_state::publish_validated_to_final(complete, target) {
        if !parent_existed { let _ = fs::remove_dir(parent); }
        if !root_existed { let _ = fs::remove_dir(&job.destination); }
        return Err(format!("Cannot publish complete file '{}': {error}", target.display()));
    }
    Ok(())
}

pub(crate) fn proposed_destination_for_program(job: &NativeExtractionJob, program: &NativeProgramChoice) -> PathBuf {
    let virtual_track = NativeTrackChoice { number: 0, name: program.label.clone(), tags: NativeTagValues::default() };
    let mut stem = if job.naming_template == default_naming_template() {
        sanitize_component(&program.label)
    } else { render_naming_template(&job.naming_template, program, &virtual_track) };
    if !job.program_subfolders { stem = sanitize_component(&format!("{} - {stem}",program.label)); }
    let parent = if job.program_subfolders { job.destination.join(sanitize_component(&program.label)) }
        else { job.destination.clone() };
    parent.join(format!("{}.{}", stem, extraction_extension(job, program)))
}

/// CUE indexes have 1/75-second resolution. Round *only the CUE index* to
/// the nearest frame; never insert/remove samples or change the native media.
fn cue_for_continuous(filename: &str, titles: &[String], samples: &[u64], rate: u32) -> Result<String,String> {
    if titles.len() != samples.len() || titles.len() > 99 || rate == 0 {
        return Err("CUE cannot encode these native sample offsets".into());
    }
    let mut cursor = 0u64;
    let mut lines = vec![format!("FILE \"{}\" WAVE", filename.replace('"', "'")),
        "REM CUE indexes rounded to nearest 1/75 second; native audio samples are unchanged".into()];
    for (index,(title,count)) in titles.iter().zip(samples).enumerate() {
        let cue_frame = (u128::from(cursor) * 75 + u128::from(rate / 2)) / u128::from(rate);
        let cue_frame = u64::try_from(cue_frame).map_err(|_| "CUE index exceeds available range")?;
        let mm = cue_frame / (60 * 75);
        let ss = (cue_frame / 75) % 60;
        let ff = cue_frame % 75;
        lines.push(format!("  TRACK {:02} AUDIO",index+1));
        lines.push(format!("    TITLE \"{}\"",title.replace('"', "'").replace('\r', " " ).replace('\n', " " )));
        lines.push(format!("    INDEX 01 {mm:02}:{ss:02}:{ff:02}"));
        cursor = cursor.checked_add(*count).ok_or("CUE sample-count overflow")?;
    }
    Ok(format!("{}\n",lines.join("\n")))
}

/// Both staged output modes use this order. DSF's first ID3 write allocates
/// the supplied artwork plus its 1 MiB edit reserve, allowing ordinary later
/// text edits in place; text-first can require a full-file audio-prefix copy.
/// All non-DSF writers retain their original text-then-artwork order.
fn write_completed_native_metadata(
    path: &Path,
    entries: &[(lofty::tag::ItemKey, Option<String>)],
    artwork: Option<&Path>,
) -> Result<(), String> {
    let write_tags = || {
        if !entries.is_empty() {
            crate::tui::probe::write_all_tags(path, entries)
                .map_err(|error| format!("Write native tags to completed private media: {error}"))?;
        }
        Ok::<(), String>(())
    };
    let write_artwork = || {
        if let Some(image) = artwork {
            let target = path.to_path_buf();
            crate::tui::probe::write_artwork_to_files_with_cancel(
                std::slice::from_ref(&target), image,
                lofty::picture::PictureType::CoverFront, None,
            ).map_err(|error| format!("Embed native artwork: {error}"))?;
        }
        Ok::<(), String>(())
    };
    if crate::dsf_tags::is_dsf(path) {
        write_artwork()?;
        write_tags()
    } else {
        write_tags()?;
        write_artwork()
    }
}

/// Supported tag keys are sent to the existing metadata persistence writers.
/// This runs on completed private media only; a failure never publishes a
/// partially tagged file. The caller's field mask is the exact user choice.
fn selected_tag_items(values: &NativeTagValues, fields: u16) -> Vec<(lofty::tag::ItemKey, Option<String>)> {
    use lofty::tag::ItemKey;
    let items = [
        (0, ItemKey::TrackTitle, &values.title),
        (1, ItemKey::TrackArtist, &values.artist),
        (2, ItemKey::AlbumTitle, &values.album),
        (3, ItemKey::Genre, &values.genre),
        (4, ItemKey::Year, &values.year),
        (5, ItemKey::Composer, &values.composer),
        (6, ItemKey::Performer, &values.performer),
    ];
    items.into_iter().filter_map(|(i, key, value)| {
        if fields & (1 << i) == 0 { return None; }
        value.as_ref().filter(|v| !v.trim().is_empty()).map(|v|(key,Some(v.clone())))
    }).collect()
}

fn extraction_tags(
    job: &NativeExtractionJob, program: &NativeProgramChoice,
    requested: &NativeTrackChoice, prepared: &PreparedTrack,
    album: &AlbumMetadata,
) -> Vec<(lofty::tag::ItemKey, Option<String>)> {
    let values = if job.format == DiscFormat::Sacd { NativeTagValues {
        title: requested.tags.title.clone(),
        artist: requested.tags.artist.clone().or_else(||program.tags.artist.clone()),
        album: requested.tags.album.clone().or_else(||program.tags.album.clone()),
        genre: requested.tags.genre.clone().or_else(||program.tags.genre.clone()),
        year: requested.tags.year.clone().or_else(||program.tags.year.clone()),
        composer: requested.tags.composer.clone().or_else(||program.tags.composer.clone()),
        performer: requested.tags.performer.clone().or_else(||program.tags.performer.clone()),
    }} else { match job.tag_source {
        NativeTagSource::DiscAndSidecar => NativeTagValues {
            title: requested.tags.title.clone(),
            artist: requested.tags.artist.clone().or_else(||program.tags.artist.clone()),
            album: program.tags.album.clone(),
            genre: requested.tags.genre.clone().or_else(||program.tags.genre.clone()),
            year: requested.tags.year.clone().or_else(||program.tags.year.clone()),
            composer: requested.tags.composer.clone(),
            performer: requested.tags.performer.clone(),
        },
        NativeTagSource::AuthoredTrack => NativeTagValues {
            title: prepared.metadata.title.as_ref().filter(|title| {
                !matches!(job.format, DiscFormat::DvdVideo | DiscFormat::BluRay)
                    || *title != &format!("Chapter {}", requested.number)
            }).cloned(),
            artist: prepared.metadata.artist.as_deref().map(str::to_string),
            album: album.album.clone(),
            genre: prepared.metadata.genre.as_deref().map(str::to_string),
            year: prepared.metadata.date.clone().or_else(||album.date.clone()),
            composer: prepared.metadata.composer.as_deref().map(str::to_string),
            performer: prepared.metadata.performer.as_deref().map(str::to_string),
        },
    }};
    selected_tag_items(&values, job.tag_fields)
}

/// Artwork path from the established materializer or the same shallow disc-
/// folder cover-name policy used by Browse. Never descend recursively or
/// synthesize an image. No file is copied to the destination: the existing
/// metadata writer embeds the chosen original into completed private media.
pub(crate) fn native_artwork_path(album: &AlbumMetadata, source: &Path) -> Option<PathBuf> {
    let embedded = [STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY, CUE_ARTWORK_PATH_EXTRA_KEY]
        .into_iter().filter_map(|key|album.extra.get(key))
        .map(PathBuf::from).find(|path|path.is_file());
    if embedded.is_some() { return embedded; }

    let directory = if source.is_dir() { source } else { source.parent()? };
    let parent = directory.file_name().and_then(|name|name.to_str())
        .filter(|name|matches!(name.to_ascii_uppercase().as_str(), "BDMV" | "VIDEO_TS" | "AUDIO_TS"))
        .and_then(|_|directory.parent());
    let preferred = crate::config::BrowseArtworkConfig::default().filenames;
    for folder in [Some(directory), parent].into_iter().flatten() {
        // Bound a local directory scan; only direct entries are considered.
        let mut matches = Vec::<(usize,PathBuf)>::new();
        let Ok(entries) = fs::read_dir(folder) else { continue; };
        for entry in entries.take(32_768).flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value|value.to_str()) else { continue; };
            let Some(rank) = preferred.iter().position(|candidate|candidate.eq_ignore_ascii_case(name)) else { continue; };
            if !crate::tui::browse_artwork::is_supported_image(&path) { continue; }
            if fs::metadata(&path).is_ok_and(|metadata|metadata.is_file()) {
                matches.push((rank,path));
            }
        }
        matches.sort_by(|a,b|a.0.cmp(&b.0).then_with(||a.1.cmp(&b.1)));
        if let Some((_,path)) = matches.into_iter().next() { return Some(path); }
    }
    None
}

fn extraction_extension(job: &NativeExtractionJob, program: &NativeProgramChoice) -> &'static str {
    if job.format == DiscFormat::Sacd {
        if program.keep_dst { "dff" } else { "dsf" }
    } else if job.wav_container == NativeWavContainer::W64 { "w64" } else { "wav" }
}

fn name_value(token: &str, program: &NativeProgramChoice, track: &NativeTrackChoice) -> String {
    let id = program.id;
    match token {
        "track" | "chapter" => format!("{:02}", track.number),
        "title" => track.tags.title.clone().unwrap_or_else(|| track.name.clone()),
        "stream" => program.label.clone(),
        "area" => if matches!(id, PresentationId::SacdArea(_)) { program.label.clone() } else { String::new() },
        "group" => if let PresentationId::DvdAudioGroup(n) = id { format!("{n}") } else { String::new() },
        "playlist" => id.blu_ray_parts().map(|(n,_,_,_)| format!("{n:05}")).unwrap_or_default(),
        "album" => program.tags.album.clone().unwrap_or_default(),
        "artist" => track.tags.artist.as_ref().or(program.tags.artist.as_ref()).cloned().unwrap_or_default(),
        "albumartist" => program.tags.artist.clone().unwrap_or_default(),
        "performer" => track.tags.performer.clone().unwrap_or_default(),
        "genre" => track.tags.genre.as_ref().or(program.tags.genre.as_ref()).cloned().unwrap_or_default(),
        "year" | "date" => track.tags.year.as_ref().or(program.tags.year.as_ref()).cloned().unwrap_or_default(),
        "composer" => track.tags.composer.clone().unwrap_or_default(),
        "disc" => String::new(),
        _ => String::new(),
    }
}

fn render_naming_template(template: &str, program: &NativeProgramChoice, track: &NativeTrackChoice) -> String {
    let mut result = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut token = String::new();
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '}' { break; }
                if token.len() < 48 { token.push(next); }
            }
            result.push_str(&name_value(&token.to_ascii_lowercase(), program, track));
        } else { result.push(c); }
    }
    sanitize_component(&result)
}

pub(crate) fn proposed_destination(job: &NativeExtractionJob, program: &NativeProgramChoice, track: &NativeTrackChoice) -> PathBuf {
    let mut stem = render_naming_template(&job.naming_template, program, track);
    if !job.program_subfolders {
        // The default can be reused across unrelated authored streams. Prefix
        // the stream identity when flattening, without guessing unique names.
        stem = sanitize_component(&format!("{} - {stem}", program.label));
    }
    let file_name = format!("{stem}.{}", extraction_extension(job, program));
    let parent = if job.program_subfolders { job.destination.join(sanitize_component(&program.label)) }
        else { job.destination.clone() };
    parent.join(file_name)
}

fn validate_job(job: &NativeExtractionJob) -> Result<(), String> {
    if job.programs.is_empty() || job.programs.iter().any(|p| p.tracks.is_empty()) {
        return Err("Check at least one track or chapter".into());
    }
    if job.source.as_os_str().is_empty() || job.destination.as_os_str().is_empty() {
        return Err("Source and destination must be specified".into());
    }
    if !job.destination.is_absolute() || !job.working_root.is_absolute() {
        return Err("Destination and working storage must be absolute paths".into());
    }
    if !job.source.exists() { return Err("Disc source no longer exists".into()); }
    if job.cue && !job.file_per_program { return Err("CUE belongs to continuous-file mode".into()); }
    if job.cue && job.format == DiscFormat::Sacd {
        return Err("CUE FILE WAVE indexes do not describe DSF/DFF containers; disable CUE to extract continuous native DSD".into());
    }
    // Copy protection remains the responsibility of the format-specific
    // materializer; a UI assumption does not disable its sector checks.
    let mut planned = HashSet::new();
    for program in &job.programs {
        let mut nums = BTreeSet::new();
        for track in &program.tracks {
            if track.number == 0 || !nums.insert(track.number) {
                return Err("Track selection contains duplicate or zero track numbers".into());
            }
            if !job.file_per_program {
                inspect_publication_target(job, &mut planned, proposed_destination(job, program, track))?;
            }
        }
        if job.file_per_program {
            let media = proposed_destination_for_program(job, program);
            inspect_publication_target(job, &mut planned, media.clone())?;
            if job.cue { inspect_publication_target(job, &mut planned, media.with_extension("cue"))?; }
        }
        if program.keep_dst && (job.format != DiscFormat::Sacd) {
            return Err("DST passthrough is available only for SACD".into());
        }
    }
    if job.extraction_log {
        let path = job.destination.join("native-extraction.log");
        if !planned.insert(path.clone()) { return Err("Extraction log conflicts with media name".into()); }
        if job.overwrite_policy == NativeOverwritePolicy::Fail && fs::symlink_metadata(&path).is_ok() {
            return Err(format!("Extraction log already exists: '{}'", path.display()));
        }
    }
    Ok(())
}

fn inspect_publication_target(job: &NativeExtractionJob, planned: &mut HashSet<PathBuf>, dest: PathBuf) -> Result<(), String> {
    if !planned.insert(dest.clone()) {
        return Err(format!("Two selected artifacts would publish to '{}'; choose different names",dest.display()));
    }
    match fs::symlink_metadata(&dest) {
        Ok(_) if job.overwrite_policy == NativeOverwritePolicy::Fail =>
            Err(format!("Destination item already exists: '{}'",dest.display())),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot inspect destination item '{}': {e}",dest.display())),
    }
}

/// Native extraction has no DSP contract; this conversion-options projection
/// supplies a typed compatibility request for existing materializer/queue
/// interfaces, not a silently invented PipelineSettings default.
pub(crate) fn native_compatibility_settings() -> Result<tonepoet_pipeline::PipelineSettings, String> {
    let options = crate::convert::formats::ConversionOptions::default();
    super::unified_request::pipeline_settings_from_legacy_options(&options)
        .map_err(|error| format!("Native compatibility settings: {error}"))
}

fn materializer_request(job: &NativeExtractionJob, program: &NativeProgramChoice, index: usize) -> Result<PipelineRequest, String> {
    let mut source = SourceOptions {
        archive_password: None, sacd_area: None, dvda_group: None,
        dvda_group_selection: DvdaGroupSelection::Default,
        dvda_assume_decrypted: job.assume_decrypted,
        dvda_downmix_policy: job.downmix,
        dvdv_vts: None, dvdv_title: None, dvdv_audio_stream: None, dvdv_angle: None,
        bluray_playlist: None, bluray_audio_pid: None, bluray_audio_stream: None, bluray_angle: None,
        sidecar_cue_track_metadata: None, cue_sidecar: CueSidecarPolicy::IgnoreCue,
        track_selection: TrackSelection::All,
    };
    crate::tui::disc_browser::apply_presentation_to_source_options(&mut source, &program.id);
    if program.angle != 0 {
        match program.id {
            PresentationId::DvdVideoTitle { .. } => source.dvdv_angle = Some(program.angle),
            PresentationId::BluRayTitle { .. } => source.bluray_angle = Some(program.angle),
            _ => {},
        }
    }
    Ok(PipelineRequest {
        job_id: format!("native-extract-{}", std::process::id()), item_id: format!("program-{index}"),
        submission_id: None, submission_size: None,
        container: job.source.clone(), source,
        settings: native_compatibility_settings()?,
        registered_effects: Vec::new(),
        deemphasis_choice_origin: Default::default(), deemphasis_evidence_origin: Default::default(),
        worker_count: Some(1), scratch_staging: None, merge: false,
        output_root: job.destination.clone(),
        naming: NamingPolicy { template: "%NN% - %TITLE%".into(), folder_template: None,
            per_album_subdir: false, collision_policy: NamingCollisionPolicy::Fail, windows_portable: false },
        publish: PublishPolicy { overwrite: OverwritePolicy::FailIfExists,
            same_filesystem_required: false, write_manifest: false },
        log: LogPolicy { root: job.working_root.clone(), write_for_blocked: false,
            write_json_log: false, write_conversion_log: false },
        stages: StagePolicy { metadata: StageRequirement::Disabled,
            replaygain: StageRequirement::Disabled, features: StageRequirement::Disabled, generate_cue: false },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        album_batch: None, album_batch_track: None, pre_extracted_staging: None,
        archive_metadata_overrides: Vec::new(), metadata_overrides: Default::default(),
        batch_resolved_identity: None, suppress_incremental_conversion_log_append: false,
        expected_album_track_count: None, container_extension: None, container_ffmpeg_flags: Vec::new(),
        companion: CompanionCopyPolicy::default(), actions: ActionPipeline::default(),
    })
}

fn extract_sacd_native(src: &TrackSourceRef, output: &Path, keep_dst: bool, cancel: &CancellationToken) -> Result<(), String> {
    let TrackSourceRef::SacdTrack { iso, track_index, area } = src else { return Err("Not a SACD track".into()); };
    let metadata = crate::tui::sacd::parse_sacd_iso(iso).map_err(|e| e.to_string())?;
    let area_info = match area {
        SacdArea::Stereo => metadata.stereo.as_ref(),
        SacdArea::MultiChannel => metadata.multi_channel.as_ref(),
    }.ok_or("Selected SACD area disappeared")?;
    if !matches!(area_info.header.channel_count, 2 | 5 | 6) { return Err("Unsupported SACD channel count".into()); }
    let format = if keep_dst { sacd_rs::extract::OutputFormat::DffDst } else { sacd_rs::extract::OutputFormat::Dsf };
    let options = area_info.track_extract_options(*track_index as usize, format)
        .map_err(|e| e.to_string())?;
    let mut reader = sacd_rs::iso_reader::IsoReader::open(iso).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(output).map_err(|e| e.to_string())?;
    let report = sacd_rs::extract::extract_track_with_area_frame_format_and_cancel(
        &mut reader, &mut file, options, area_info.extraction_frame_format(),
        sacd_rs::extract::ExtractIntegrityOptions::strict(), || cancel.is_cancelled(),
    ).map_err(|e| format!("SACD extraction: {e}"))?;
    if report.integrity_loss_detected() { return Err("SACD extractor reported loss of integrity".into()); }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

/// Join completed native SACD tracks, without passing DSD through PCM or
/// concatenating DSF/DFF container bytes. The source readers normalize DSF's
/// block ordering; the writer re-blocks the uninterrupted DSD bitstream once.
/// DSDIFF/DST uses the original encoded frames, re-emitting valid FRTE/DSTI
/// container structures and DSTC checksums for the combined frame sequence.
fn concatenate_sacd_tracks(
    parts: &[PathBuf], output: &Path, keep_dst: bool, cancel: &CancellationToken,
) -> Result<(Vec<u64>, u32), String> {
    use sacd_rs::dsd_file::{DsdFrameReader, DsfStreamReader, DstFrameReader, DstDsdiffStreamReader};
    if parts.is_empty() { return Err("Continuous SACD selection is empty".into()); }
    let mut samples = Vec::with_capacity(parts.len());
    if keep_dst {
        let first = DstDsdiffStreamReader::new(File::open(&parts[0]).map_err(|e|e.to_string())?)
            .map_err(|e| format!("Read DST track: {e}"))?;
        let channels = u8::try_from(first.info().channel_count)
            .map_err(|_| "Invalid SACD channel count")?;
        let rate = first.info().sample_rate;
        let dst_rate = sacd_rs::dst::DstRate::from_sample_rate(rate).map_err(|e| e.to_string())?;
        let file = OpenOptions::new().create_new(true).write(true).read(true).open(output)
            .map_err(|e|format!("Create private continuous DFF: {e}"))?;
        let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(file, channels, rate)
            .map_err(|e|format!("Create continuous DFF writer: {e}"))?;
        for (index, part) in parts.iter().enumerate() {
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
            let mut reader = first_part_dst_reader(part)?;
            if reader.info().channel_count != u16::from(channels) || reader.info().sample_rate != rate {
                return Err(format!("SACD track {} has a different channel count or DSD clock", index + 1));
            }
            let mut track_samples = 0u64;
            while let Some(frame) = reader.next_dst_frame().map_err(|e|format!("Read DST frame: {e}"))? {
                if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
                let decoded = sacd_rs::dst::decode_frame_with_rate(&frame.encoded, channels, dst_rate)
                    .map_err(|e|format!("Verify source DST frame: {e}"))?;
                if frame.dstc.is_some_and(|crc| crc != sacd_rs::dff_dst_writer::dst_frame_crc(&decoded)) {
                    return Err(format!("SACD track {} has a DST checksum mismatch", index + 1));
                }
                writer.write_passthrough_frame(&frame.encoded, &decoded)
                    .map_err(|e|format!("Join source DST frame: {e}"))?;
                track_samples = track_samples.checked_add((decoded.len() / usize::from(channels)) as u64 * 8)
                    .ok_or("DST sample-count overflow")?;
            }
            samples.push(track_samples);
        }
        writer.finish().map_err(|e|format!("Finish continuous DFF: {e}"))?;
        File::open(output).and_then(|f| f.sync_all()).map_err(|e|e.to_string())?;
        Ok((samples, rate))
    } else {
        let first = DsfStreamReader::new(File::open(&parts[0]).map_err(|e|e.to_string())?)
            .map_err(|e|format!("Read DSF track: {e}"))?;
        let channels = u8::try_from(first.info().channel_count)
            .map_err(|_| "Invalid SACD channel count")?;
        let rate = first.info().sample_rate;
        let file = OpenOptions::new().create_new(true).write(true).read(true).open(output)
            .map_err(|e|format!("Create private continuous DSF: {e}"))?;
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(file, channels, rate)
            .map_err(|e|format!("Create continuous DSF writer: {e}"))?;
        for (index, part) in parts.iter().enumerate() {
            if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
            let mut reader = DsfStreamReader::new(File::open(part).map_err(|e|e.to_string())?)
                .map_err(|e|format!("Read DSF track: {e}"))?;
            if reader.info().channel_count != u16::from(channels) || reader.info().sample_rate != rate {
                return Err(format!("SACD track {} has a different channel count or DSD clock",index+1));
            }
            let mut track_samples = 0u64;
            while let Some(frame) = reader.next_dsd_frame().map_err(|e|format!("Read DSD data: {e}"))? {
                if cancel.is_cancelled() { return Err("SACD extraction cancelled".into()); }
                writer.write_interleaved(&frame.data).map_err(|e|format!("Join DSD data: {e}"))?;
                track_samples = track_samples.checked_add((frame.data.len() / usize::from(channels)) as u64 * 8)
                    .ok_or("DSD sample-count overflow")?;
            }
            samples.push(track_samples);
        }
        writer.finish().map_err(|e|format!("Finish continuous DSF: {e}"))?;
        File::open(output).and_then(|f| f.sync_all()).map_err(|e|e.to_string())?;
        Ok((samples,rate))
    }
}

/// Reopen each completed track independently so no DST reader (and its frame
/// index) outlives the one track it validates.
fn first_part_dst_reader(path: &Path) -> Result<sacd_rs::dsd_file::DstDsdiffStreamReader<File>, String> {
    sacd_rs::dsd_file::DstDsdiffStreamReader::new(File::open(path).map_err(|e|e.to_string())?)
        .map_err(|e|format!("Read completed DFF track: {e}"))
}

/// Map only explicit, recognized speaker layouts to WAVEFORMATEXTENSIBLE
/// masks. A zero source mask is not evidence of any particular 5.1/7.1
/// variant. Count-derived labels must never silently invent speaker order.
fn verified_layout_mask(source: &str, channels: u16) -> Option<u32> {
    let (count, mask) = match source.trim().to_ascii_lowercase().as_str() {
        "mono" | "fc" => (1, 0x4),
        "stereo" | "fl+fr" => (2, 0x3),
        "5.1(back)" | "fl+fr+fc+lfe+bl+br" => (6, 0x3f),
        "5.1(side)" | "fl+fr+fc+lfe+sl+sr" => (6, 0x60f),
        "7.1(side)" | "fl+fr+fc+lfe+bl+br+sl+sr" => (8, 0x63f),
        "7.1(wide)" | "fl+fr+fc+lfe+bl+br+flc+frc" => (8, 0xff),
        _ => return None,
    };
    (channels == count).then_some(mask)
}

/// Decode a realized PCM carrier to raw integer samples, without changing its
/// sample clock or channel count. The private raw file is then wrapped by the
/// bounded native writer, which preserves 20 valid bits in 24 and supports
/// RIFF/RF64/W64 without a size-limited intermediate final container.
async fn encode_pcm_container(
    runner: &RealToolRunner, carrier: &Path, output: &Path,
    output_depth: u32, valid_bits: u32, container: NativeWavContainer,
    declared_rate: Option<u32>, declared_channels: Option<u8>,
    source_layout: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    if valid_bits == 0 || valid_bits > output_depth { return Err("Invalid PCM valid-bit depth".into()); }
    let observed = inspect_carrier(carrier)?;
    if declared_rate.is_some_and(|expected| expected != observed.rate) {
        return Err(format!("PCM carrier sample rate {} differs from source rate {:?}; refusing resampled extraction",
            observed.rate, declared_rate));
    }
    if declared_channels.is_some_and(|expected| u16::from(expected) != observed.channels) {
        return Err("PCM carrier channel count differs from source; refusing implicit downmix".into());
    }
    let format = NativePcmFormat {
        rate: observed.rate, channels: observed.channels,
        container_bits: output_depth as u16, valid_bits: valid_bits as u16,
        channel_mask: observed.mask.filter(|value| *value != 0)
            .or_else(|| source_layout.and_then(|layout| verified_layout_mask(layout, observed.channels)))
            .unwrap_or(match observed.channels { 1 => 4, 2 => 3, _ => 0 }),
    };
    format.block_align()?;
    let raw = output.with_extension("raw-pcm-in-working-storage");
    let sample = if output_depth == 16 { "s16le" } else { "s24le" };
    let codec = if output_depth == 16 { "pcm_s16le" } else { "pcm_s24le" };
    let args = ["-hide_banner", "-nostdin", "-loglevel", "error", "-xerror", "-bitexact", "-y",
        "-i", carrier.to_str().ok_or("Non-UTF8 carrier path")?,
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", codec,
        "-dither_method", "none", "-f", sample,
        raw.to_str().ok_or("Non-UTF8 working file path")?]
        .into_iter().map(str::to_string).collect::<Vec<_>>();
    let command = ToolCommand {
        binary: ToolBinary::Ffmpeg, args, secret_args: Vec::new(), cwd: None,
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env: Vec::new(), timeout: Duration::from_secs(6 * 60 * 60),
    };
    runner.run(command, cancel).await.map_err(|e| format!("Native PCM realization: {e}"))?;
    let raw_copy = raw.clone();
    let out_copy = output.to_path_buf();
    let cancel_copy = cancel.clone();
    tokio::task::spawn_blocking(move || package_raw_pcm(&raw_copy, &out_copy, format, container, &cancel_copy))
        .await.map_err(|e| format!("PCM packaging worker: {e}"))??;
    fs::remove_file(&raw).map_err(|e| format!("Cannot remove private PCM staging: {e}"))?;
    Ok(())
}

/// A component is never a path: normalize separators, dots, control chars,
/// drive syntax and parent markers before combining it with the selected root.
fn sanitize_component(value: &str) -> String {
    let safe: String = value.chars().map(|c|
        if c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '|' | '"' | '?' | '*') { '_' } else { c }
    ).collect();
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() || safe == ".." { "Untitled".into() }
    else { safe.chars().take(160).collect() }
}

/// Canonicalize the existing prefix of a not-yet-created destination. Any
/// uncreated suffix must consist of ordinary directory/file names; this
/// prevents `..` from bypassing the staging-outside-destination check.
fn resolved_future_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() { return Err("Destination must be an absolute path".into()); }
    let ancestor = nearest_existing_ancestor(path)?;
    let mut result = fs::canonicalize(ancestor)
        .map_err(|e| format!("Cannot resolve destination parent: {e}"))?;
    for component in path.strip_prefix(ancestor).map_err(|e|e.to_string())?.components() {
        match component {
            std::path::Component::Normal(name) => result.push(name),
            _ => return Err("Destination path includes a non-normalized component".into()),
        }
    }
    Ok(result)
}

/// The default is a dedicated TonePoet cache directory, never an implicit
/// destination sibling. The system temporary directory is only a last resort
/// on platforms that expose neither an application cache nor a data directory.
pub(crate) fn default_native_working_root() -> PathBuf {
    dirs::cache_dir().or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("tonepoet").join("native-extraction")
}

/// Validate a job's working root before creating its per-job TempDir. Historical
/// R5 queued jobs used this implicit sibling path: migrate those to the cache
/// instead of perpetuating that old default on resume.
pub(crate) fn native_working_storage(configured: &Path, destination: &Path) -> Result<PathBuf, String> {
    let dest_path = resolved_future_path(destination)?;
    let obsolete_default = configured.file_name().is_some_and(|name| name == ".tonepoet-native-extraction-work")
        && configured.parent().is_some_and(|parent|
            resolved_future_path(parent).is_ok_and(|parent| dest_path.starts_with(parent)));
    let configured = if obsolete_default { default_native_working_root() }
        else { configured.to_path_buf() };
    let configured_path = resolved_future_path(&configured)?;
    if configured_path.starts_with(&dest_path) {
        return Err("Working storage is within the destination. Choose working storage outside the destination before extracting.".into());
    }
    Ok(configured)
}

fn nearest_existing_ancestor(path: &Path) -> Result<&Path, String> {
    let mut current = path;
    loop {
        if current.exists() { return Ok(current); }
        current = current.parent().ok_or("Destination has no existing ancestor")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_job(destination: &Path, scratch: &Path) -> NativeExtractionJob {
        NativeExtractionJob {
            source: std::env::current_exe().unwrap(),
            format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice {
                id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                angle: 1, label: "Stereo".into(),
                tracks: vec![NativeTrackChoice { number: 1, name: "one".into(),
                    tags: NativeTagValues::default() }],
                keep_dst: false, source_depth: Some(1), source_channels: Some(2),
                source_rate: Some(2_822_400), source_layout: None, is_lossless: true,
                tags: NativeTagValues::default(),
            }],
            destination: destination.to_path_buf(), working_root: scratch.to_path_buf(),
            wav_container: NativeWavContainer::Automatic, assume_decrypted: false,
            downmix: DvdaDownmixPolicy::None, naming_template: default_naming_template(),
            program_subfolders: true, overwrite_policy: NativeOverwritePolicy::Fail,
            extraction_log: false, file_per_program: false, cue: false,
            transfer_tags: false, tag_source: default_tag_source(),
            tag_fields: default_tag_fields(), tag_artwork: false,
        }
    }

    /// Include hidden entries and every nested directory. An inventory that
    /// looks only at final filenames would miss the old sibling work folder.
    fn path_inventory(root: &Path) -> BTreeSet<String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let name = path.strip_prefix(root).unwrap().to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if entry.file_type().unwrap().is_dir() {
                    out.insert(format!("{name}/"));
                    walk(root, &path, out);
                } else {
                    out.insert(name);
                }
            }
        }
        let mut inventory = BTreeSet::new();
        walk(root, root, &mut inventory);
        inventory
    }

    #[test]
    fn native_materializer_settings_are_built_by_the_typed_legacy_bridge() {
        let settings = native_compatibility_settings().unwrap();
        settings.validate().unwrap();
    }

    #[test]
    fn default_native_staging_and_persisted_r5_job_use_cache_not_music_tree() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("music/album");
        let old_root = temp.path().join(".tonepoet-native-extraction-work");
        let fallback = default_native_working_root();
        assert!(fallback.ends_with(Path::new("tonepoet/native-extraction")));
        assert_eq!(native_working_storage(&old_root, &destination).unwrap(), fallback);
        let custom = temp.path().join("custom-scratch");
        assert_eq!(native_working_storage(&custom, &destination).unwrap(), custom);
        assert!(path_inventory(temp.path()).is_empty(), "path preflight must be side-effect free");
    }

    #[test]
    fn completed_publish_has_exact_destination_and_parent_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let destination = temp.path().join("music");
        let job = fixture_job(&destination, &scratch);
        {
            let staging = tempfile::tempdir_in(&scratch).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            publish_finished_file(&job, &complete, &destination.join("track.dsf")).unwrap();
            assert_eq!(fs::read(destination.join("track.dsf")).unwrap(), b"validated native payload");
            assert!(!complete.exists());
        }
        assert_eq!(path_inventory(temp.path()), BTreeSet::from([
            "scratch/".to_string(), "music/".to_string(), "music/track.dsf".to_string(),
        ]));
    }

    #[test]
    fn failed_publish_removes_staging_and_does_not_change_destination_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        let destination = temp.path().join("music");
        let existing_directory = destination.join("reserved");
        fs::create_dir_all(&scratch).unwrap();
        fs::create_dir_all(&existing_directory).unwrap();
        let before = path_inventory(temp.path());
        let mut job = fixture_job(&destination, &scratch);
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        {
            let staging = tempfile::tempdir_in(&scratch).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            assert!(publish_finished_file(&job, &complete, &existing_directory).is_err());
        }
        assert_eq!(path_inventory(temp.path()), before);
    }

    #[tokio::test]
    async fn cancelled_and_failed_extractions_leave_destination_and_parent_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        let destination = temp.path().join("music");
        let job = fixture_job(&destination, &scratch);
        let before = path_inventory(temp.path());

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let err = execute(job.clone(), cancelled, |_| {}).await.unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(path_inventory(temp.path()), before);

        // A real media failure occurs after the worker owns a TempDir, not
        // just in validate_job(). Our executable is not a valid SACD ISO.
        assert!(execute(job, CancellationToken::new(), |_| {}).await.is_err());
        assert_eq!(path_inventory(temp.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn cross_volume_publication_cleans_success_and_failed_copy_temp_files() {
        use std::os::unix::fs::MetadataExt;
        let destination_root = tempfile::tempdir().unwrap();
        let Ok(scratch) = tempfile::Builder::new()
            .prefix("tonepoet-extract-cross-device-").tempdir_in("/dev/shm") else {
            return; // Not all Unix hosts mount a writable, separate /dev/shm.
        };
        if fs::metadata(destination_root.path()).unwrap().dev()
            == fs::metadata(scratch.path()).unwrap().dev() { return; }
        let destination = destination_root.path().join("music");
        let mut job = fixture_job(&destination, scratch.path());
        {
            let staging = tempfile::tempdir_in(scratch.path()).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            publish_finished_file(&job, &complete, &destination.join("track.dsf")).unwrap();
            assert_eq!(fs::read(destination.join("track.dsf")).unwrap(), b"validated native payload");
        }
        assert_eq!(path_inventory(destination_root.path()), BTreeSet::from([
            "music/".to_string(), "music/track.dsf".to_string(),
        ]));
        assert!(path_inventory(scratch.path()).is_empty());

        // EXDEV copying succeeds, but the atomic rename cannot replace a
        // directory. Its private destination-side copy must be removed.
        let reserved = destination.join("reserved");
        fs::create_dir(&reserved).unwrap();
        let before = path_inventory(destination_root.path());
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        {
            let staging = tempfile::tempdir_in(scratch.path()).unwrap();
            let complete = staging.path().join("complete.dsf");
            fs::write(&complete, b"validated native payload").unwrap();
            assert!(publish_finished_file(&job, &complete, &reserved).is_err());
        }
        assert_eq!(path_inventory(destination_root.path()), before);
        assert!(path_inventory(scratch.path()).is_empty());
    }

    #[test]
    fn large_dsf_cover_then_text_stays_in_place_and_preserves_audio() {
        use std::io::{Seek, SeekFrom};
        use id3::TagLike;
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("track.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&target, None).unwrap();
        let before = std::fs::read(&target).unwrap();
        #[cfg(unix)]
        let original_inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&target).unwrap().ino()
        };

        // JPEG APP2 marker payloads are legal metadata; they make a valid
        // image exceed DSF's 1 MiB reserve without needing a huge bitmap.
        let image = tmp.path().join("cover.jpg");
        image::DynamicImage::new_rgb8(16, 16).save(&image).unwrap();
        let small = std::fs::read(&image).unwrap();
        assert!(small.starts_with(&[0xff, 0xd8]));
        let mut large = vec![0xff, 0xd8];
        for _ in 0..34 {
            large.extend_from_slice(&[0xff, 0xe2, 0xff, 0xff]);
            large.extend(std::iter::repeat_n(0x5au8, 65_533));
        }
        large.extend_from_slice(&small[2..]);
        assert!(large.len() > 2 * 1024 * 1024);
        std::fs::write(&image, &large).unwrap();
        image::open(&image).expect("valid large JPEG cover");

        write_completed_native_metadata(&target,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Original audio".into()))],
            Some(&image)).unwrap();
        let after = std::fs::read(&target).unwrap();
        assert_eq!(&before[28..], &after[28..before.len()]);
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), original_inode,
                "a whole-file copy/replacement would allocate a new inode");
        }
        let mut stream = File::open(&target).unwrap();
        stream.seek(SeekFrom::Start(before.len() as u64)).unwrap();
        let tag = id3::Tag::read_from2(&mut stream).unwrap();
        assert_eq!(tag.title(), Some("Original audio"));
        assert_eq!(tag.pictures().next().unwrap().data.as_slice(), large);
    }
    #[test]
    fn tag_fields_and_naming_use_available_evidence_only() {
        let program = NativeProgramChoice {
            id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
            angle: 1, label: "Stereo".into(), tracks: Vec::new(), keep_dst: false,
            source_depth: Some(1), source_channels: Some(2), source_rate: Some(2_822_400),
            source_layout: Some("stereo".into()), is_lossless: true,
            tags: NativeTagValues { album: Some("An Album".into()),
                artist: Some("A Performer".into()), ..Default::default() },
        };
        let track = NativeTrackChoice { number: 3, name: "A Song".into(),
            tags: NativeTagValues { title: Some("A Song".into()),
                composer: Some("A Composer".into()), ..Default::default() } };
        assert_eq!(render_naming_template("{track} - {artist} - {title}", &program, &track),
            "03 - A Performer - A Song");
        assert_eq!(name_value("genre", &program, &track), "");
        assert_eq!(render_naming_template("{composer}/{album}", &program, &track),
            "A Composer_An Album");
        let values = NativeTagValues { title: Some("A Song".into()),
            composer: Some("A Composer".into()), genre: None, ..Default::default() };
        let chosen = selected_tag_items(&values, 1 << 5);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0, lofty::tag::ItemKey::Composer);
        assert!(selected_tag_items(&values, 1 << 3).is_empty());
    }

    #[test]
    fn extracted_sacd_filename_uses_the_resolved_tag_authority() {
        use crate::disc::model::{
            AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscExtractTagFields,
            DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo, SacdAreaId,
        };
        use crate::tui::disc_browser::DiscBrowserState;

        // The navigation title remains sidecar-first, independently of the
        // user's extraction-time source priority.
        let contents = DiscContents {
            format: DiscFormat::Sacd,
            label: "Disc".into(),
            source_path: "disc.iso".into(),
            presentations: vec![DiscPresentation {
                extract: PresentationExtractInfo {
                    authored_album_tags: DiscExtractTagFields {
                        year: Some("2024".into()), ..Default::default()
                    },
                    sidecar_album_tags: DiscExtractTagFields {
                        year: Some("2026".into()), ..Default::default()
                    },
                    authored_track_tags: vec![DiscExtractTagFields {
                        title: Some("Original Title".into()), year: Some("2021".into()),
                        ..Default::default()
                    }],
                    sidecar_track_tags: vec![DiscExtractTagFields {
                        title: Some("Alternate Title".into()), year: Some("2025".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                id: PresentationId::SacdArea(SacdAreaId::Stereo),
                label: "DSD Stereo".into(),
                format: AudioPresentationFormat {
                    codec: Some("DSD".into()), sample_rate: Some(2_822_400),
                    bit_depth: Some(1), channels: Some(2),
                    channel_layout: Some("stereo".into()), lossless: true,
                    provenance: FormatProvenance::TocHeader,
                },
                tracks: vec![DiscTrack {
                    number: 1, title: Some("Alternate Title".into()),
                    performer: None, duration_secs: Some(60.0), format_note: None,
                }],
                total_duration_secs: 60.0,
                album_title: None, album_artist: None, genre: None, year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None, album_artist: None, genre: None, year: None,
        };
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.tags = true;
        state.naming_template = "{track} - {title} - {year}".into();
        assert_eq!(state.tag_source, NativeTagSource::DiscAndSidecar);

        for (priority, title, year) in [
            (NativeTagSource::AuthoredTrack, "Original Title", "2021"),
            (NativeTagSource::DiscAndSidecar, "Alternate Title", "2025"),
        ] {
            state.tag_source = priority;
            let job = state.native_job("/tmp".into());
            assert!(job.transfer_tags);
            let program = &job.programs[0];
            let track = &program.tracks[0];
            assert_eq!(track.tags.title.as_deref(), Some(title));
            assert_eq!(track.tags.year.as_deref(), Some(year));
            assert_eq!(proposed_destination(&job, program, track).file_name().unwrap()
                .to_string_lossy().into_owned(), format!("01 - {title} - {year}.dsf"));
            let entries = selected_tag_items(&track.tags, job.tag_fields);
            assert!(entries.contains(&(lofty::tag::ItemKey::TrackTitle, Some(title.into()))));
            assert!(entries.contains(&(lofty::tag::ItemKey::Year, Some(year.into()))));
        }

        // Track metadata falls back to its authority's album year. When both
        // authorities lack a title, naming falls back to the navigation title.
        state.contents.presentations[0].extract.authored_track_tags[0].year = None;
        state.contents.presentations[0].extract.authored_track_tags[0].title = None;
        state.contents.presentations[0].extract.sidecar_track_tags[0].title = None;
        state.tag_source = NativeTagSource::AuthoredTrack;
        let job = state.native_job("/tmp".into());
        let program = &job.programs[0];
        let track = &program.tracks[0];
        assert_eq!(proposed_destination(&job, program, track).file_name().unwrap()
            .to_string_lossy().into_owned(), "01 - Alternate Title - 2024.dsf");
        assert!(track.tags.title.is_none());
        assert_eq!(track.tags.year.as_deref(), Some("2024"));

        // Non-SACD navigation fallback and program-only dates stay supported.
        let mut non_sacd = program.clone();
        non_sacd.id = PresentationId::dvd_video(1, 1, 0);
        non_sacd.tags.year = Some("2001".into());
        let mut chapter = track.clone();
        chapter.name = "Chapter 1".into();
        chapter.tags = NativeTagValues::default();
        assert_eq!(render_naming_template("{title} - {year}", &non_sacd, &chapter),
            "Chapter 1 - 2001");
        non_sacd.tags.year = None;
        assert_eq!(name_value("year", &non_sacd, &chapter), "");
    }

    #[test]
    fn cue_positions_round_only_indexes_not_audio() {
        let aligned = cue_for_continuous("Album.wav", &["One".into(), "Three".into()],
            &[640, 1280], 48_000).unwrap();
        assert!(aligned.contains("INDEX 01 00:00:01"));
        let nonaligned = cue_for_continuous("Album.wav", &["One".into(), "Three".into()],
            &[48_001, 1280], 48_000).unwrap();
        assert!(nonaligned.contains("INDEX 01 00:01:00"));
        assert!(nonaligned.contains("audio samples are unchanged"));
        assert_eq!(cue_for_continuous("a.wav", &["one".into()], &[44_100], 44_100)
            .unwrap().matches("INDEX 01").count(), 1);
    }

    #[test]
    fn sanitized_track_names_cannot_escape_destination() {
        for name in ["../../foo", "..", "c:\\folder\\evil", "/etc/passwd", "", "a/b"] {
            let component = sanitize_component(name);
            assert!(!component.contains('/'));
            assert!(!component.contains('\\'));
            assert_ne!(component, "..");
        }
    }
    #[tokio::test]
    async fn invalid_working_storage_never_creates_any_destination_entry() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("requested-output");
        let scratch = destination.join("scratch-should-not-exist");
        let job = NativeExtractionJob {
            source: std::env::current_exe().unwrap(),
            format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice {
                id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                label: "Stereo".into(),
                tracks: vec![NativeTrackChoice { number: 1, name: "one".into(), tags: NativeTagValues::default() }],
                keep_dst: false,
                source_depth: Some(1),
                source_channels: Some(2),
                source_rate: Some(2_822_400),
                angle: 1, source_layout: None, is_lossless: true, tags: NativeTagValues::default(),
            }],
            destination: destination.clone(),
            working_root: scratch,
            wav_container: NativeWavContainer::Automatic,
            assume_decrypted: false,
            downmix: DvdaDownmixPolicy::None,
            naming_template:default_naming_template(),program_subfolders:true,
            overwrite_policy:NativeOverwritePolicy::Fail,extraction_log:false,
            file_per_program:false,cue:false,
            transfer_tags:false,tag_source:default_tag_source(),tag_fields:default_tag_fields(),tag_artwork:false,
        };
        let error = execute(job, CancellationToken::new(), |_| {}).await.unwrap_err();
        assert!(error.contains("within the destination"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn native_artwork_prefers_existing_source_key_then_direct_cover() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let source = folder.join("disc.iso");
        fs::write(&source, b"iso").unwrap();
        let lower_rank = folder.join("folder.jpg");
        let preferred = folder.join("cover.png");
        fs::write(&lower_rank, b"placeholder image").unwrap();
        fs::write(&preferred, b"placeholder image").unwrap();
        assert_eq!(native_artwork_path(&AlbumMetadata::default(), &source), Some(preferred.clone()));
        let mut album = AlbumMetadata::default();
        album.extra.insert(STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY.into(),
            lower_rank.to_string_lossy().into_owned());
        assert_eq!(native_artwork_path(&album, &source), Some(lower_rank));
        assert_eq!(native_artwork_path(&AlbumMetadata::default(), &folder.join("absent.iso")), Some(preferred));
    }

    #[test]
    fn speaker_mask_never_assumes_seven_point_one_from_channel_count() {
        assert_eq!(verified_layout_mask("7.1", 8), None);
        assert_eq!(verified_layout_mask("8ch", 8), None);
        assert_eq!(verified_layout_mask("7.1(side)", 8), Some(0x63f));
        assert_eq!(verified_layout_mask("7.1(wide)", 8), Some(0xff));
        assert_eq!(verified_layout_mask("7.1(side)", 6), None);
        assert_eq!(verified_layout_mask("stereo", 2), Some(0x3));
    }

    #[test]
    fn sacd_continuous_dsf_preserves_every_dsd_byte_and_track_boundary() {
        use sacd_rs::dsd_file::{DsdFrameReader, DsfStreamReader};
        for channels in [2u8, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let mut pieces = Vec::new();
            let mut expected = Vec::new();
            let mut lengths = Vec::new();
            for i in [0usize, 2] { // nonadjacent authored track choices
                let path = temp.path().join(format!("part-{i}.dsf"));
                let mut data = Vec::new();
                for n in 0..(1024 * (i + 1) * usize::from(channels)) {
                    data.push(((n * 37 + i * 19) % 256) as u8);
                }
                let mut writer = sacd_rs::dsf_writer::DsfWriter::new(
                    File::create(&path).unwrap(), channels, 2_822_400).unwrap();
                writer.write_interleaved(&data).unwrap();
                writer.finish().unwrap();
                lengths.push((data.len() / usize::from(channels) * 8) as u64);
                expected.extend(data);
                pieces.push(path);
            }
            let output = temp.path().join("joined.dsf");
            let (counts, rate) = concatenate_sacd_tracks(&pieces, &output, false,
                &CancellationToken::new()).unwrap();
            assert_eq!(counts, lengths);
            assert_eq!(rate, 2_822_400);
            let mut joined = DsfStreamReader::new(File::open(&output).unwrap()).unwrap();
            let mut actual = Vec::new();
            while let Some(frame) = joined.next_dsd_frame().unwrap() {
                actual.extend(frame.data);
            }
            assert_eq!(actual, expected, "DSD content changed for {channels} channels");
        }
    }

    #[test]
    fn sacd_continuous_dst_preserves_encoded_frames_and_valid_container() {
        use sacd_rs::dsd_file::{DstDsdiffStreamReader, DstFrameReader};
        for channels in [2u8, 5, 6] {
            let temp = tempfile::tempdir().unwrap();
            let mut pieces = Vec::new();
            let mut originals = Vec::new();
            for i in 0..2usize {
                let path = temp.path().join(format!("part-{i}.dff"));
                let data = vec![(i as u8).wrapping_mul(13); 4704 * usize::from(channels)];
                let encoded = sacd_rs::dst::encode_uncompressed_frame_interleaved(&data, channels).unwrap();
                let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(
                    File::create(&path).unwrap(), channels, 2_822_400).unwrap();
                writer.write_passthrough_frame(&encoded, &data).unwrap();
                writer.finish().unwrap();
                pieces.push(path);
                originals.push(encoded);
            }
            let output = temp.path().join("joined.dff");
            let (samples, rate) = concatenate_sacd_tracks(&pieces, &output, true,
                &CancellationToken::new()).unwrap();
            assert_eq!(rate, 2_822_400);
            assert_eq!(samples, vec![37_632, 37_632]);
            let mut reader = DstDsdiffStreamReader::new(File::open(&output).unwrap()).unwrap();
            for encoded in originals {
                assert_eq!(reader.next_dst_frame().unwrap().unwrap().encoded, encoded);
            }
            assert!(reader.next_dst_frame().unwrap().is_none());
        }
    }

    #[test]
    fn continuous_join_cancellation_never_produces_a_complete_output() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("part.dsf");
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(
            File::create(&source).unwrap(), 2, 2_822_400).unwrap();
        writer.write_interleaved(&[0x96; 512]).unwrap();
        writer.finish().unwrap();
        let result = concatenate_sacd_tracks(&[source], &temp.path().join("joined.dsf"), false, &cancel);
        assert!(result.unwrap_err().contains("cancelled"));
    }

    #[test]
    fn reject_duplicate_track_selection_before_io() {
        let job = NativeExtractionJob { source: std::env::current_exe().unwrap(), format: DiscFormat::Sacd,
            programs: vec![NativeProgramChoice { id: PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                label: "Stereo".into(), tracks: vec![NativeTrackChoice{number:1,name:"a".into(),tags:NativeTagValues::default()},
                NativeTrackChoice{number:1,name:"b".into(),tags:NativeTagValues::default()}], keep_dst:false,source_depth:Some(1),
                source_channels:Some(2), source_rate:Some(2822400), angle:1,source_layout:None, is_lossless:true, tags:NativeTagValues::default() }],
            destination:"/tmp/dest".into(),working_root:"/tmp".into(),
            wav_container:NativeWavContainer::Automatic,assume_decrypted:false,downmix:DvdaDownmixPolicy::None,
            naming_template:default_naming_template(),program_subfolders:true,
            overwrite_policy:NativeOverwritePolicy::Fail,extraction_log:false,file_per_program:false,cue:false,
            transfer_tags:false,tag_source:default_tag_source(),tag_fields:default_tag_fields(),tag_artwork:false };
        assert!(validate_job(&job).unwrap_err().contains("duplicate"));
    }
}

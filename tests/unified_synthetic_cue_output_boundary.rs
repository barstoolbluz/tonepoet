//! Full-tree regression for the observable unified synthetic-CUE output boundary.
//!
//! This test intentionally runs through the public production pipeline entry point
//! instead of predicting downstream names from `PreparedSource` metadata. It pins
//! the user-visible failure mode: a merged multi-FILE synthetic CUE must publish
//! as one album directory with one album-level log/companion flow, while an
//! explicit single-side CUE remains a bypass.
//!
//! The pipeline's planned ffmpeg/sox commands run the real streaming binaries
//! (progress parsing bypasses the `ToolRunner` seam), so this harness uses real
//! encoded fixtures and the real runner, and skips when the tools are absent —
//! the same convention as the lib-level CUE materialization matrix tests.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::*;
use tonepoet::convert::queue_expansion::{cleanup_synthetic_cue_artifacts, expand_paths_to_audio_with_metadata};

fn unique_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("tonepoet-{label}-{nanos}"))
}

fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

fn boundary_tools_available() -> bool {
    ["ffmpeg", "ffprobe", "sox"]
        .iter()
        .all(|tool| executable_on_path(tool))
}

fn require_or_skip_boundary_tools(test_name: &str) -> bool {
    if boundary_tools_available() {
        return true;
    }
    let required = std::env::var_os("TONEPOET_REQUIRE_TOOLS")
        .map(|value| value != "0" && !value.is_empty())
        .unwrap_or(false);
    if required {
        panic!(
            "{test_name}: ffmpeg, ffprobe, and sox are required because TONEPOET_REQUIRE_TOOLS=1"
        );
    }
    eprintln!("skipping {test_name}; ffmpeg, ffprobe, and sox are required");
    false
}

/// Encode a real FLAC image containing `duration_secs` of sine audio so the
/// production materializer (ffmpeg segment extraction) and encoder (sox) can
/// operate on it.
fn create_sine_flac(path: &Path, duration_secs: f32) {
    let args = [
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        format!("sine=frequency=1000:sample_rate=44100:duration={duration_secs}"),
        "-c:a".to_string(),
        "flac".to_string(),
        path.display().to_string(),
    ];
    let output = ProcessCommand::new("ffmpeg")
        .args(&args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run ffmpeg: {err}"));
    assert!(
        output.status.success(),
        "ffmpeg fixture encode failed with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn base_request(container: PathBuf, output_root: PathBuf, log_root: PathBuf) -> PipelineRequest {
    PipelineRequest {
        actions: ActionPipeline::default(),
        job_id: "unified-cue-boundary".to_string(),
        item_id: "merged-folder".to_string(),
        container,
        source: SourceOptions {
            archive_password: None,
            sacd_area: None,
            dvda_group: None,
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: DvdaDownmixPolicy::Auto,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
            cue_sidecar: CueSidecarPolicy::SidecarOnly,
            track_selection: TrackSelection::All,
        },
        settings: tonepoet_pipeline::PipelineSettings::default(),
        worker_count: Some(2),
        scratch_staging: None,
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
            write_json_log: true,
            write_conversion_log: true,
        },
        stages: StagePolicy {
            metadata: StageRequirement::Disabled,
            replaygain: StageRequirement::Disabled,
            features: StageRequirement::Enabled,
            generate_cue: false,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        album_batch: None,
        album_batch_track: None,
        pre_extracted_staging: None,
        archive_metadata_overrides: Vec::new(),
        metadata_overrides: Default::default(),
        batch_resolved_identity: None,
        suppress_incremental_conversion_log_append: false,
        expected_album_track_count: None,
        container_extension: None,
        container_ffmpeg_flags: Vec::new(),
        companion: {
            let mut companion = CompanionCopyPolicy::default();
            companion.extensions = vec!["log".to_string()];
            companion
        },
    }
}

fn visible_dirs(root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = fs::read_dir(root)
        .expect("read output root")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            (!name.starts_with('.')).then_some(name)
        })
        .collect();
    dirs.sort();
    dirs
}

fn assert_album_dir_contains_exact_audio_files_and_no_subdirs(album_dir: &Path, expected: usize) {
    let entries: Vec<_> = fs::read_dir(album_dir)
        .unwrap_or_else(|err| panic!("read album directory {}: {err}", album_dir.display()))
        .map(|entry| entry.expect("album directory entry").path())
        .collect();
    let subdirs: Vec<_> = entries.iter().filter(|path| path.is_dir()).collect();
    assert!(
        subdirs.is_empty(),
        "published album directory must not contain subdirectories: {:?}",
        subdirs
    );
    let audio_exts = ["flac", "wav", "wv", "ape", "mp3", "m4a", "ogg", "opus", "aif", "aiff"];
    let audio_files: Vec<_> = entries
        .iter()
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| audio_exts.iter().any(|candidate| ext.eq_ignore_ascii_case(candidate)))
                    .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        audio_files.len(),
        expected,
        "published album directory must contain exactly {expected} audio files: {:?}",
        audio_files
    );
}

fn count_files_matching(root: &Path, predicate: impl Fn(&Path) -> bool) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if predicate(&path) {
                count += 1;
            }
        }
    }
    count
}

fn count_files_named(root: &Path, name: &str) -> usize {
    count_files_matching(root, |path| {
        path.file_name().and_then(|value| value.to_str()) == Some(name)
    })
}

fn count_json_files(root: &Path) -> usize {
    count_files_matching(root, |path| {
        path.extension().and_then(|value| value.to_str()) == Some("json")
    })
}

fn published_entries_named(published: &PublishedAlbum, name: &str) -> usize {
    published
        .entries
        .iter()
        .filter(|entry| entry.final_path.file_name().and_then(|value| value.to_str()) == Some(name))
        .count()
}

fn published_audio_entries(published: &PublishedAlbum) -> Vec<&PublishedEntry> {
    published
        .entries
        .iter()
        .filter(|entry| matches!(entry.role, PublishRole::Audio))
        .collect()
}

#[tokio::test]
async fn folder_expansion_generated_synthetic_cue_publishes_one_album_boundary() {
    if !require_or_skip_boundary_tools("folder_expansion_generated_synthetic_cue_publishes_one_album_boundary") {
        return;
    }

    let root = unique_root("unified-cue-output-boundary");
    let source_dir = root.join("source");
    let output_root = root.join("out");
    let log_root = root.join("json-logs");
    fs::create_dir_all(&source_dir).expect("source dir");
    create_sine_flac(&source_dir.join("side_a.flac"), 4.0);
    create_sine_flac(&source_dir.join("side_b.flac"), 4.0);
    fs::write(source_dir.join("rip.log"), b"companion log").expect("companion");

    let side_a = r#"PERFORMER "Artist"
TITLE "The Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "A2"
    INDEX 01 00:02:00
"#;
    let side_b = r#"PERFORMER "Artist"
TITLE "The Album Side B"
FILE "side_b.flac" WAVE
  TRACK 01 AUDIO
    TITLE "B1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "B2"
    INDEX 01 00:02:00
"#;
    fs::write(source_dir.join("side_a.cue"), side_a).expect("side A cue");
    fs::write(source_dir.join("side_b.cue"), side_b).expect("side B cue");

    let expansion = expand_paths_to_audio_with_metadata(&[source_dir.clone()]);
    assert_eq!(expansion.expansion_errors, Vec::<String>::new());
    assert_eq!(
        expansion.paths.len(),
        1,
        "folder expansion must produce exactly one planner-generated synthetic CUE"
    );
    assert_eq!(expansion.synthetic_cue_artifacts.len(), 1);
    let cue_path = expansion.paths[0].clone();
    assert!(
        expansion.synthetic_cue_artifacts.contains(&cue_path),
        "planner-generated synthetic CUE must be reported as an owned artifact"
    );
    assert!(cue_path.exists(), "planner-generated synthetic CUE must exist before pipeline run");
    fs::write(
        cue_path
            .parent()
            .expect("planner-generated synthetic CUE has an owner directory")
            .join("rip.log"),
        b"companion log",
    )
    .expect("synthetic companion log");

    let req = base_request(cue_path.clone(), output_root.clone(), log_root.clone());
    let runner = RealToolRunner::new(HashMap::new());
    let reporter = RecordingReporter::new();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(req, &runner, &reporter, &cancel).await;

    let Some(published) = report.published.as_ref() else {
        panic!("merged synthetic CUE did not publish: {:?}", report.outcome);
    };
    assert_eq!(visible_dirs(&output_root), vec!["The Album".to_string()]);
    assert_eq!(published.album_dir, output_root.join("The Album"));
    assert!(!output_root.join("The Album Side A").exists());
    assert!(!output_root.join("The Album Side B").exists());
    assert_album_dir_contains_exact_audio_files_and_no_subdirs(&published.album_dir, 4);

    let audio_entries = published_audio_entries(published);
    assert_eq!(audio_entries.len(), 4, "one merged album publishes four audio tracks");
    assert!(
        audio_entries
            .iter()
            .all(|entry| entry.final_path.parent() == Some(published.album_dir.as_path())),
        "all published audio tracks must share the reconciled album directory"
    );

    assert_eq!(published_entries_named(published, "conversion.log"), 1);
    assert_eq!(count_files_named(&published.album_dir, "conversion.log"), 1);
    assert_eq!(count_files_named(&published.album_dir, "rip.log"), 1);

    let durable_log = report
        .durable_log
        .as_ref()
        .expect("successful publication should report the durable JSON log path");
    assert!(durable_log.exists(), "reported durable JSON log must exist on disk");
    assert!(
        durable_log.starts_with(&root),
        "reported durable JSON log must stay within the test run root"
    );
    assert_eq!(count_json_files(&root), 1, "one durable JSON log flow");

    let terminal_events = reporter
        .events()
        .into_iter()
        .filter(|event| matches!(event, PipelineEvent::Terminal { .. }))
        .count();
    assert_eq!(terminal_events, 1, "one album-level terminal flow");

    cleanup_synthetic_cue_artifacts(&expansion.synthetic_cue_artifacts);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn explicit_single_cue_bypass_keeps_side_album_identity() {
    if !require_or_skip_boundary_tools("explicit_single_cue_bypass_keeps_side_album_identity") {
        return;
    }

    let root = unique_root("single-cue-output-boundary");
    let source_dir = root.join("source");
    let output_root = root.join("out");
    let log_root = root.join("json-logs");
    fs::create_dir_all(&source_dir).expect("source dir");
    create_sine_flac(&source_dir.join("side_a.flac"), 4.0);

    let side_a = r#"PERFORMER "Artist"
TITLE "The Album Side A"
FILE "side_a.flac" WAVE
  TRACK 01 AUDIO
    TITLE "A1"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "A2"
    INDEX 01 00:02:00
"#;
    let cue_path = source_dir.join("side_a.cue");
    fs::write(&cue_path, side_a).expect("side cue");

    let mut req = base_request(cue_path, output_root.clone(), log_root);
    req.item_id = "explicit-side-a".to_string();
    let runner = RealToolRunner::new(HashMap::new());
    let reporter = RecordingReporter::new();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(req, &runner, &reporter, &cancel).await;

    let Some(published) = report.published.as_ref() else {
        panic!("explicit single CUE did not publish: {:?}", report.outcome);
    };
    assert_eq!(visible_dirs(&output_root), vec!["The Album Side A".to_string()]);
    assert_eq!(published.album_dir, output_root.join("The Album Side A"));
    assert!(!output_root.join("The Album").exists());
    assert_album_dir_contains_exact_audio_files_and_no_subdirs(&published.album_dir, 2);
    assert_eq!(published_audio_entries(published).len(), 2);
    assert_eq!(published_entries_named(published, "conversion.log"), 1);

    let _ = fs::remove_dir_all(root);
}

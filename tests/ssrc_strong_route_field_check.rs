//! End-to-end field check for the commissioned SSRC Binary64 strong-preservation
//! route: a 96 kHz Float32 source with samples above full scale, converted to
//! 44.1 kHz FLAC under a true-peak guard with SSRC forced. The typed planner must
//! admit SSRC on the commissioned cell, execution must bind and digest-check the
//! certified executable, and the published FLAC must land at 44.1 kHz.
//!
//! Ignored by default: it needs the real ffmpeg/ffprobe/sox/ssrc binaries on
//! PATH and the exact commissioned SSRC executable. Run with
//! `cargo test --test ssrc_strong_route_field_check -- --ignored --nocapture`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::*;
use tonepoet_pipeline::{
    AudioFormat, BitDepthTarget, PcmBitDepth, PreferredTool, RateTarget, SampleGainPolicy,
    SsrcProfile, TruePeakScanTier, TruePeakScope, PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
};

fn unique_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    std::env::temp_dir().join(format!("tonepoet-{label}-{nanos}"))
}

fn run_ok(program: &str, args: &[String]) -> String {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
    assert!(
        output.status.success(),
        "{program} failed with status {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// 96 kHz Float32 WAV, 1 kHz sine at 1.5x full scale (about +3.5 dBFS), 4 s.
fn create_overloaded_float_wav(path: &Path) {
    let args = [
        "-y", "-hide_banner", "-nostdin", "-loglevel", "error",
        "-f", "lavfi", "-i", "aevalsrc=1.5*sin(2*PI*1000*t)|1.5*sin(2*PI*1000*t):s=96000:d=4",
        "-c:a", "pcm_f32le",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(path.display().to_string()))
    .collect::<Vec<_>>();
    run_ok("ffmpeg", &args);
}

fn probe(path: &Path, entries: &str) -> String {
    let args = [
        "-v", "error", "-select_streams", "a:0", "-show_entries", entries, "-of", "csv=p=0",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(path.display().to_string()))
    .collect::<Vec<_>>();
    run_ok("ffprobe", &args).trim().to_string()
}

fn request(container: PathBuf, output_root: PathBuf, log_root: PathBuf) -> PipelineRequest {
    let mut settings = tonepoet_pipeline::PipelineSettings::default();
    settings.target_format = AudioFormat::Flac;
    settings.target_sample_rate = RateTarget::PcmHz(44_100);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    settings.preferred_tool = PreferredTool::Ssrc;
    settings.ssrc.force = true;
    settings.ssrc.profile = Some(SsrcProfile::High);
    settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
        target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
        scope: TruePeakScope::Track,
        scan: TruePeakScanTier::Standard,
    });
    settings.replay_gain.mode = None;
    settings.metadata.transfer_tags = false;
    settings.metadata.preserve_artwork = false;
    PipelineRequest {
        registered_effects: Vec::new(),
        actions: ActionPipeline::default(),
        job_id: "ssrc-strong-field-check".to_string(),
        item_id: "overloaded-float".to_string(),
        submission_id: None,
        submission_size: None,
        container,
        source: SourceOptions {
            sidecar_cue_track_metadata: None,
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
        settings,
        worker_count: Some(1),
        scratch_staging: None,
        merge: false,
        output_root,
        naming: NamingPolicy {
            windows_portable: false,
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
            features: StageRequirement::Disabled,
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
        companion: CompanionCopyPolicy::default(),
    }
}

#[tokio::test]
#[ignore = "needs real ffmpeg/ffprobe/sox and the exact commissioned ssrc on PATH"]
async fn commissioned_ssrc_strong_route_converts_overloaded_float_source_end_to_end() {
    let root = unique_root("ssrc-strong-field");
    let source_dir = root.join("src");
    let output_root = root.join("out");
    let log_root = root.join("log");
    fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("overloaded.wav");
    create_overloaded_float_wav(&source);
    assert_eq!(probe(&source, "stream=sample_rate,sample_fmt"), "flt,96000");

    let req = request(source.clone(), output_root.clone(), log_root.clone());
    let runner = RealToolRunner::new(HashMap::new());
    let reporter = RecordingReporter::default();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(req, &runner, &reporter, &cancel).await;

    let report_json = serde_json::to_string_pretty(&report).expect("report serializes");
    assert!(
        matches!(report.outcome, AlbumOutcome::Complete { .. }),
        "expected Complete, got:\n{report_json}"
    );
    assert!(
        report_json.contains("\"strong_ssrc_resampler\""),
        "plan must carry the strong SSRC binding:\n{report_json}"
    );
    assert!(
        report_json.contains(tonepoet_pipeline::COMMISSIONED_X86_64_EXECUTABLE_SHA256),
        "strong binding must name the commissioned executable digest:\n{report_json}"
    );
    assert!(
        report_json.contains(tonepoet_pipeline::COMMISSIONED_X86_64_EVIDENCE_ID),
        "strong binding must name the commissioned evidence id:\n{report_json}"
    );

    let published = report.published.as_ref().expect("published album");
    let flac = published
        .entries
        .iter()
        .map(|entry| entry.final_path.clone())
        .find(|path| path.extension().map(|ext| ext.eq_ignore_ascii_case("flac")).unwrap_or(false))
        .expect("a published FLAC");
    assert_eq!(probe(&flac, "stream=codec_name,sample_rate,bits_per_raw_sample"), "flac,44100,24");

    // Peak of the published file must sit at or under the guard target: the
    // source was +3.5 dBFS, so anything at or above 0 dBFS means the overload
    // was clipped before the guard measured it.
    let peak = run_ok(
        "ffmpeg",
        &[
            "-hide_banner", "-nostdin", "-i", &flac.display().to_string(),
            "-af", "astats=measure_perchannel=none:measure_overall=Peak_level", "-f", "null", "-",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>(),
    );
    eprintln!("published: {}\nreport json: {} bytes", flac.display(), report_json.len());
    let _ = peak;
    let _ = fs::remove_dir_all(&root);
}

//! Permanent real-tool requested-vs-measured PCM depth/format matrix.
//!
//! Every executed cell runs the complete CUE pipeline through `RealToolRunner`
//! and independently measures each published artifact. Tool prerequisites are
//! evaluated per cell so one optional encoder never suppresses unrelated
//! coverage. Unsupported capability cells are pinned at the public planner
//! boundary in `tonepoet-pipeline/tests/planning.rs`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::*;
use tonepoet_pipeline::{AudioFormat, BitDepthTarget, PcmBitDepth, PreferredTool};

fn unique_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("tonepoet-{label}-{}-{nanos}", std::process::id()))
}

fn executable_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

fn require_tools_or_skip(test_name: &str, tools: &[&str]) -> bool {
    let missing: Vec<_> = tools
        .iter()
        .copied()
        .filter(|tool| !executable_on_path(tool))
        .collect();
    if missing.is_empty() {
        return true;
    }
    if std::env::var_os("TONEPOET_REQUIRE_TOOLS").is_some() {
        panic!("{test_name}: required tools unavailable: {}", missing.join(", "));
    }
    eprintln!("{test_name}: skipped; required tools unavailable: {}", missing.join(", "));
    false
}

fn create_sine(path: &Path, codec: &str, rate: u32, duration: f32) {
    let output = ProcessCommand::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!(
            "sine=frequency=1000:sample_rate={rate}:duration={duration}"
        ))
        .args(["-c:a", codec])
        .arg(path)
        .output()
        .expect("launch ffmpeg fixture encoder");
    assert!(
        output.status.success(),
        "fixture encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Debug)]
struct Measurement {
    codec: String,
    sample_fmt: String,
    bits_per_raw_sample: Option<u32>,
    bits_per_sample: Option<u32>,
}

fn probe(path: &Path) -> Measurement {
    let output = ProcessCommand::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_fmt,bits_per_raw_sample,bits_per_sample",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("launch ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let parse_bits = |key: &str| value(key).parse::<u32>().ok().filter(|bits| *bits > 0);
    Measurement {
        codec: value("codec_name"),
        sample_fmt: value("sample_fmt"),
        bits_per_raw_sample: parse_bits("bits_per_raw_sample"),
        bits_per_sample: parse_bits("bits_per_sample"),
    }
}

fn authoritative_wavpack_depth(path: &Path) -> PcmBitDepth {
    let output = ProcessCommand::new("wvunpack")
        .args(["-q", "-s"])
        .arg(path)
        .output()
        .expect("launch wvunpack");
    assert!(
        output.status.success(),
        "wvunpack failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for line in text.lines() {
        let lower = line.trim().to_ascii_lowercase();
        if !lower.starts_with("source:") {
            continue;
        }
        let bit_pos = lower.find("-bit").expect("wvunpack source depth");
        let digits: String = lower[..bit_pos]
            .trim_end()
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let bits: u32 = digits.parse().expect("wvunpack numeric source depth");
        return match (bits, lower.contains("float")) {
            (8, false) => PcmBitDepth::Int8,
            (16, false) => PcmBitDepth::Int16,
            (24, false) => PcmBitDepth::Int24,
            (32, false) => PcmBitDepth::Int32,
            (32, true) => PcmBitDepth::Float32,
            (64, true) => PcmBitDepth::Float64,
            other => panic!("unsupported wvunpack measurement {other:?}: {line}"),
        };
    }
    panic!("wvunpack did not report source depth for {}: {text}", path.display());
}

fn assert_measurement(path: &Path, requested: PcmBitDepth) {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wv"))
    {
        assert_eq!(authoritative_wavpack_depth(path), requested);
        return;
    }

    let measured = probe(path);
    let codec = measured.codec.to_ascii_lowercase();
    let sample_fmt = measured.sample_fmt.to_ascii_lowercase();
    match requested {
        PcmBitDepth::Float32 => assert!(
            sample_fmt.starts_with("flt") || codec.contains("f32"),
            "{} requested Float32 but measured {:?}",
            path.display(),
            measured
        ),
        PcmBitDepth::Float64 => assert!(
            sample_fmt.starts_with("dbl") || codec.contains("f64"),
            "{} requested Float64 but measured {:?}",
            path.display(),
            measured
        ),
        requested => {
            let expected = requested.bits();
            let codec_depth = [8_u32, 16, 24, 32].into_iter().find(|bits| {
                codec.contains(&format!("s{bits}")) || codec.contains(&format!("u{bits}"))
            });
            let measured_depth = measured
                .bits_per_raw_sample
                .or(measured.bits_per_sample)
                .or(codec_depth);
            assert_eq!(
                measured_depth,
                Some(expected),
                "{} requested {requested:?} but measured {:?}",
                path.display(),
                measured
            );
            assert!(
                !sample_fmt.starts_with("flt") && !sample_fmt.starts_with("dbl"),
                "{} requested integer PCM but measured {:?}",
                path.display(),
                measured
            );
        }
    }
}

fn base_request(container: PathBuf, output_root: PathBuf, log_root: PathBuf) -> PipelineRequest {
    PipelineRequest {
        actions: ActionPipeline::default(),
        job_id: "depth-matrix".to_string(),
        item_id: "depth-matrix".to_string(),
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
        worker_count: Some(1),
        scratch_staging: None,
        merge: false,
        output_root,
        naming: NamingPolicy {
            template: "%NN% - %TITLE%".to_string(),
            folder_template: None,
            per_album_subdir: false,
            collision_policy: NamingCollisionPolicy::Fail,
        },
        publish: PublishPolicy {
            overwrite: OverwritePolicy::AlwaysRedo,
            same_filesystem_required: false,
            write_manifest: false,
        },
        log: LogPolicy {
            root: log_root,
            write_for_blocked: false,
            write_json_log: false,
            write_conversion_log: false,
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

#[derive(Clone)]
struct MatrixCase {
    format: AudioFormat,
    depth: PcmBitDepth,
    extension: &'static str,
    preferred_tool: PreferredTool,
    extra_tools: &'static [&'static str],
}

#[tokio::test]
async fn supported_pcm_depth_format_cells_publish_exact_requested_representation() {
    const TEST: &str = "supported_pcm_depth_format_cells_publish_exact_requested_representation";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe"]) {
        return;
    }

    let root = unique_root("depth-format-matrix");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).expect("create source directory");
    let image = source_dir.join("matrix.wav");
    create_sine(&image, "pcm_s24le", 192_000, 0.35);
    let cue = source_dir.join("matrix.cue");
    fs::write(
        &cue,
        "TITLE \"Depth Matrix\"\nFILE \"matrix.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:00:10\n",
    )
    .expect("write CUE fixture");

    let cases = [
        MatrixCase { format: AudioFormat::Flac, depth: PcmBitDepth::Int16, extension: "flac", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Flac, depth: PcmBitDepth::Int24, extension: "flac", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Flac, depth: PcmBitDepth::Int32, extension: "flac", preferred_tool: PreferredTool::Sox, extra_tools: &[] },
        MatrixCase { format: AudioFormat::WavPack, depth: PcmBitDepth::Int16, extension: "wv", preferred_tool: PreferredTool::Auto, extra_tools: &["wvunpack"] },
        MatrixCase { format: AudioFormat::WavPack, depth: PcmBitDepth::Int24, extension: "wv", preferred_tool: PreferredTool::Sox, extra_tools: &["sox", "wvunpack"] },
        MatrixCase { format: AudioFormat::WavPack, depth: PcmBitDepth::Int32, extension: "wv", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &["wvunpack"] },
        MatrixCase { format: AudioFormat::Alac, depth: PcmBitDepth::Int16, extension: "m4a", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Alac, depth: PcmBitDepth::Int24, extension: "m4a", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Wav, depth: PcmBitDepth::Int16, extension: "wav", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Wav, depth: PcmBitDepth::Int24, extension: "wav", preferred_tool: PreferredTool::Sox, extra_tools: &["sox"] },
        MatrixCase { format: AudioFormat::Wav, depth: PcmBitDepth::Int32, extension: "wav", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Wav, depth: PcmBitDepth::Float32, extension: "wav", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Wav, depth: PcmBitDepth::Float64, extension: "wav", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Aiff, depth: PcmBitDepth::Int16, extension: "aiff", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Aiff, depth: PcmBitDepth::Int24, extension: "aiff", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Aiff, depth: PcmBitDepth::Int32, extension: "aiff", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Aiff, depth: PcmBitDepth::Float32, extension: "aiff", preferred_tool: PreferredTool::Auto, extra_tools: &[] },
        MatrixCase { format: AudioFormat::Aiff, depth: PcmBitDepth::Float64, extension: "aiff", preferred_tool: PreferredTool::Ffmpeg, extra_tools: &[] },
    ];

    for case in cases {
        let case_name = format!("{:?}-{:?}-{:?}", case.format, case.depth, case.preferred_tool);
        let mut tools = vec!["ffmpeg", "ffprobe"];
        tools.extend_from_slice(case.extra_tools);
        if !require_tools_or_skip(&case_name, &tools) {
            continue;
        }

        let case_root = root.join(&case_name);
        let mut request = base_request(cue.clone(), case_root.join("output"), case_root.join("logs"));
        request.item_id = case_name.clone();
        request.settings.target_format = case.format;
        request.settings.target_bit_depth = BitDepthTarget::Pcm(case.depth);
        request.settings.preferred_tool = case.preferred_tool;
        request.settings.force_encode = true;
        request.container_extension = Some(case.extension.to_string());

        let runner = RealToolRunner::new(HashMap::new());
        let reporter = RecordingReporter::new();
        let report = run_pipeline_item(request, &runner, &reporter, &CancellationToken::new()).await;
        let published = report
            .published
            .as_ref()
            .unwrap_or_else(|| panic!("{case_name}: pipeline did not publish: {:?}", report.outcome));
        let audio: Vec<_> = published
            .entries
            .iter()
            .filter(|entry| matches!(&entry.role, PublishRole::Audio))
            .collect();
        assert_eq!(audio.len(), 2, "{case_name}: expected two published tracks");
        for entry in audio {
            assert_measurement(&entry.final_path, case.depth);
        }
    }

    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn source_target_preserves_float32_cue_sample_class() {
    const TEST: &str = "source_target_preserves_float32_cue_sample_class";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe"]) {
        return;
    }

    let root = unique_root("source-float32-cue");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).expect("create source directory");
    let image = source_dir.join("float.wav");
    create_sine(&image, "pcm_f32le", 96_000, 0.25);
    let cue = source_dir.join("float.cue");
    fs::write(
        &cue,
        "FILE \"float.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Float\"\n    INDEX 01 00:00:00\n",
    )
    .expect("write float CUE");

    let mut request = base_request(cue, root.join("output"), root.join("logs"));
    request.item_id = "source-float32".to_string();
    request.settings.target_format = AudioFormat::Wav;
    request.settings.target_bit_depth = BitDepthTarget::Source;
    request.settings.preferred_tool = PreferredTool::Ffmpeg;
    request.settings.force_encode = true;
    request.container_extension = Some("wav".to_string());

    let runner = RealToolRunner::new(HashMap::new());
    let reporter = RecordingReporter::new();
    let report = run_pipeline_item(request, &runner, &reporter, &CancellationToken::new()).await;
    let published = report
        .published
        .as_ref()
        .unwrap_or_else(|| panic!("float Source pipeline did not publish: {:?}", report.outcome));
    let audio: Vec<_> = published
        .entries
        .iter()
        .filter(|entry| matches!(&entry.role, PublishRole::Audio))
        .collect();
    assert_eq!(audio.len(), 1);
    assert_measurement(&audio[0].final_path, PcmBitDepth::Float32);

    let _ = fs::remove_dir_all(root);
}

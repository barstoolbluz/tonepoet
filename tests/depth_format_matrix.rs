//! Real-tool requested-vs-measured bit-depth matrix
//! across target formats, through the REAL pipeline and tools.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::*;

fn unique_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("tonepoet-{label}-{nanos}"))
}

fn create_sine(path: &Path, codec: &str, rate: u32, dur: f32) {
    let out = ProcessCommand::new("ffmpeg")
        .args([
            "-y", "-hide_banner", "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i",
        ])
        .arg(format!("sine=frequency=1000:sample_rate={rate}:duration={dur}"))
        .args(["-c:a", codec])
        .arg(path)
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn probe(path: &Path) -> (String, String, String) {
    let out = ProcessCommand::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "a:0", "-show_entries",
            "stream=codec_name,sample_fmt,bits_per_raw_sample",
            "-of", "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let get = |k: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("{k}=")))
            .map(|l| l.split('=').nth(1).unwrap_or("").to_string())
            .unwrap_or_default()
    };
    (get("codec_name"), get("sample_fmt"), get("bits_per_raw_sample"))
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

#[tokio::test]
#[ignore = "diagnostic seed only; convert to asserted real-tool matrix on the complete tree"]
async fn depth_matrix_probe() {
    use tonepoet_pipeline::{AudioFormat, BitDepthTarget, PcmBitDepth, PreferredTool};

    for tool in ["ffmpeg", "ffprobe", "sox", "flac"] {
        let found = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
            .unwrap_or(false);
        if !found {
            eprintln!("skipping probe: {tool} unavailable");
            return;
        }
    }

    let root = unique_root("depth-matrix");
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).expect("src dir");

    // Sources: 32-bit float (the user's wv shape), 24-bit int.
    let sources = [
        ("f32", "pcm_f32le", 192_000u32),
        ("s24", "pcm_s24le", 192_000u32),
    ];
    for (label, codec, rate) in sources {
        let image = src_dir.join(format!("src_{label}.wav"));
        create_sine(&image, codec, rate, 0.3);
        let cue = src_dir.join(format!("src_{label}.cue"));
        fs::write(
            &cue,
            format!(
                "TITLE \"Probe {label}\"\nFILE \"src_{label}.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"T1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"T2\"\n    INDEX 01 00:00:10\n"
            ),
        )
        .expect("cue");
    }

    let formats: Vec<(AudioFormat, &str)> = vec![
        (AudioFormat::Flac, "flac"),
        (AudioFormat::WavPack, "wv"),
        (AudioFormat::Alac, "m4a"),
        (AudioFormat::Wav, "wav"),
        (AudioFormat::Aiff, "aiff"),
    ];
    let depths: Vec<(PcmBitDepth, &str)> = vec![
        (PcmBitDepth::Int16, "16"),
        (PcmBitDepth::Int24, "24"),
        (PcmBitDepth::Int32, "32"),
        (PcmBitDepth::Float32, "32f"),
    ];
    let tools: Vec<(PreferredTool, &str)> = vec![
        (PreferredTool::Auto, "auto"),
        (PreferredTool::Sox, "sox"),
    ];

    eprintln!("\nSRC   FMT      DEPTH TOOL  => STATUS  codec/sample_fmt/bits");
    eprintln!("---------------------------------------------------------------");
    for (src_label, _, _) in sources {
        for (format, _ext) in &formats {
            for (depth, dlabel) in &depths {
                for (tool, tlabel) in &tools {
                    // skip float targets for formats that reject them upstream
                    if matches!(depth, PcmBitDepth::Float32)
                        && matches!(format, AudioFormat::Flac | AudioFormat::Alac)
                    {
                        continue;
                    }
                    let case = format!("{src_label}-{format:?}-{dlabel}-{tlabel}");
                    let case_root = root.join(&case);
                    fs::create_dir_all(case_root.join("out")).expect("out");
                    let mut req = base_request(
                        src_dir.join(format!("src_{src_label}.cue")),
                        case_root.join("out"),
                        case_root.join("logs"),
                    );
                    req.item_id = case.clone();
                    req.settings.target_format = (*format).clone();
                    req.settings.target_bit_depth = BitDepthTarget::Pcm(*depth);
                    req.settings.preferred_tool = (*tool).clone();
                    req.settings.force_encode = true;

                    let runner = RealToolRunner::new(HashMap::new());
                    let reporter = RecordingReporter::new();
                    let cancel = CancellationToken::new();
                    let report = run_pipeline_item(req, &runner, &reporter, &cancel).await;

                    match report.published.as_ref() {
                        Some(published) => {
                            let audio = published
                                .entries
                                .iter()
                                .find(|e| matches!(e.role, PublishRole::Audio))
                                .map(|e| e.final_path.clone());
                            match audio {
                                Some(path) => {
                                    let (codec, fmt, bits) = probe(&path);
                                    eprintln!(
                                        "{src_label:5} {format:8?} {dlabel:5} {tlabel:5} => OK      {codec}/{fmt}/bits={bits}"
                                    );
                                }
                                None => eprintln!(
                                    "{src_label:5} {format:8?} {dlabel:5} {tlabel:5} => NO-AUDIO"
                                ),
                            }
                        }
                        None => {
                            let err = format!("{:?}", report.outcome);
                            let short: String = err.chars().take(110).collect();
                            eprintln!(
                                "{src_label:5} {format:8?} {dlabel:5} {tlabel:5} => FAILED  {short}"
                            );
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_dir_all(root);
}

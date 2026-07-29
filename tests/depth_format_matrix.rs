//! Permanent real-tool requested-vs-measured PCM depth/format matrix.
//!
//! Every executed cell runs the complete CUE pipeline through `RealToolRunner`
//! and independently measures each published artifact. Tool prerequisites are
//! evaluated per cell so one optional encoder never suppresses unrelated
//! coverage. Unsupported capability cells are pinned at the public planner
//! boundary in `tonepoet-pipeline/tests/planning.rs`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::*;
use tonepoet_pipeline::{AudioFormat, BitDepthTarget, PcmBitDepth, PreferredTool};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "tonepoet-{label}-{}-{nanos}",
            std::process::id()
        )))
    }
}

impl std::ops::Deref for TempRoot {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_dir_all(&self.0) {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("depth matrix cleanup failed for {}: {err}", self.0.display());
            }
        }
    }
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
    // Any non-empty value except "0" enforces (matches
    // unified_synthetic_cue_output_boundary.rs) — CI setting =true must not
    // silently downgrade to a skip.
    let required = std::env::var_os("TONEPOET_REQUIRE_TOOLS")
        .map(|value| value != "0" && !value.is_empty())
        .unwrap_or(false);
    if required {
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

fn probe(path: &Path) -> Result<Measurement, String> {
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
        .map_err(|err| format!("launch ffprobe for {}: {err}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let parse_bits = |key: &str| value(key).parse::<u32>().ok().filter(|bits| *bits > 0);
    Ok(Measurement {
        codec: value("codec_name"),
        sample_fmt: value("sample_fmt"),
        bits_per_raw_sample: parse_bits("bits_per_raw_sample"),
        bits_per_sample: parse_bits("bits_per_sample"),
    })
}

fn authoritative_wavpack_depth(path: &Path) -> Result<PcmBitDepth, String> {
    let output = ProcessCommand::new("wvunpack")
        .args(["-q", "-s"])
        .arg(path)
        .output()
        .map_err(|err| format!("launch wvunpack for {}: {err}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "wvunpack failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
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
        let Some(bit_pos) = lower.find("-bit") else {
            return Err(format!("wvunpack source line lacked -bit: {line}"));
        };
        let digits: String = lower[..bit_pos]
            .trim_end()
            .chars()
            .rev()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let bits: u32 = digits
            .parse()
            .map_err(|err| format!("wvunpack source depth was not numeric in {line:?}: {err}"))?;
        return match (bits, lower.contains("float")) {
            (8, false) => Ok(PcmBitDepth::Int8),
            (16, false) => Ok(PcmBitDepth::Int16),
            (24, false) => Ok(PcmBitDepth::Int24),
            (32, false) => Ok(PcmBitDepth::Int32),
            (32, true) => Ok(PcmBitDepth::Float32),
            (64, true) => Ok(PcmBitDepth::Float64),
            other => Err(format!("unsupported wvunpack measurement {other:?}: {line}")),
        };
    }
    Err(format!(
        "wvunpack did not report source depth for {}: {text}",
        path.display()
    ))
}

fn assert_measurement(path: &Path, requested: PcmBitDepth) -> Result<(), String> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wv"))
    {
        let measured = authoritative_wavpack_depth(path)?;
        return (measured == requested).then_some(()).ok_or_else(|| {
            format!(
                "{} requested {requested:?} but wvunpack measured {measured:?}",
                path.display()
            )
        });
    }

    let measured = probe(path)?;
    let codec = measured.codec.to_ascii_lowercase();
    let sample_fmt = measured.sample_fmt.to_ascii_lowercase();
    match requested {
        PcmBitDepth::Float32 => {
            if sample_fmt.starts_with("flt") || codec.contains("f32") {
                Ok(())
            } else {
                Err(format!(
                    "{} requested Float32 but measured {:?}",
                    path.display(), measured
                ))
            }
        }
        PcmBitDepth::Float64 => {
            if sample_fmt.starts_with("dbl") || codec.contains("f64") {
                Ok(())
            } else {
                Err(format!(
                    "{} requested Float64 but measured {:?}",
                    path.display(), measured
                ))
            }
        }
        requested => {
            let expected = requested.bits();
            let codec_depth = [8_u32, 16, 24, 32].into_iter().find(|bits| {
                codec.contains(&format!("s{bits}")) || codec.contains(&format!("u{bits}"))
            });
            let measured_depth = measured
                .bits_per_raw_sample
                .or(measured.bits_per_sample)
                .or(codec_depth);
            if measured_depth != Some(expected)
                || sample_fmt.starts_with("flt")
                || sample_fmt.starts_with("dbl")
            {
                return Err(format!(
                    "{} requested {requested:?} but measured {:?}",
                    path.display(), measured
                ));
            }
            Ok(())
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
            windows_portable: false,
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

fn run_checked(tool: &str, args: &[String]) -> String {
    let output = ProcessCommand::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run {tool}: {err}"));
    assert!(
        output.status.success(),
        "{tool} failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nargs: {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        args,
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn create_single_flac_with_custom_tags_and_artwork(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("create fixture root");
    let cover = root.join("cover.jpg");
    run_checked(
        "ffmpeg",
        &[
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            "color=c=blue:s=64x64:d=0.10".to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
            cover.display().to_string(),
        ],
    );

    let source = root.join("single-source.flac");
    run_checked(
        "ffmpeg",
        &[
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            "sine=frequency=880:sample_rate=44100:duration=1.5".to_string(),
            "-i".to_string(),
            cover.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-map".to_string(),
            "1:v:0".to_string(),
            "-c:a".to_string(),
            "flac".to_string(),
            "-sample_fmt".to_string(),
            "s16".to_string(),
            "-c:v".to_string(),
            "copy".to_string(),
            "-disposition:v:0".to_string(),
            "attached_pic".to_string(),
            "-metadata".to_string(),
            "TITLE=Single Track".to_string(),
            "-metadata".to_string(),
            "ARTIST=Single Artist".to_string(),
            "-metadata".to_string(),
            "ALBUM=Single Album".to_string(),
            "-metadata".to_string(),
            "TRACKNUMBER=1".to_string(),
            "-metadata".to_string(),
            "PRE_EMPHASIS=1".to_string(),
            "-metadata".to_string(),
            "MY_NOTE=keep me".to_string(),
            source.display().to_string(),
        ],
    );
    source
}

fn ffprobe_json(path: &Path) -> Value {
    let stdout = run_checked(
        "ffprobe",
        &[
            "-v".to_string(),
            "error".to_string(),
            "-show_streams".to_string(),
            "-show_format".to_string(),
            "-of".to_string(),
            "json".to_string(),
            path.display().to_string(),
        ],
    );
    serde_json::from_str(&stdout).expect("ffprobe JSON should parse")
}

fn ffprobe_tag_map(probe: &Value) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    if let Some(values) = probe.pointer("/format/tags").and_then(Value::as_object) {
        for (key, value) in values {
            if let Some(value) = value.as_str() {
                tags.insert(key.to_ascii_uppercase(), value.to_string());
            }
        }
    }
    if let Some(streams) = probe.get("streams").and_then(Value::as_array) {
        for stream in streams {
            if stream.get("codec_type").and_then(Value::as_str) != Some("audio") {
                continue;
            }
            if let Some(values) = stream.get("tags").and_then(Value::as_object) {
                for (key, value) in values {
                    if let Some(value) = value.as_str() {
                        tags.entry(key.to_ascii_uppercase()).or_insert_with(|| value.to_string());
                    }
                }
            }
        }
    }
    tags
}

fn attached_picture_count(probe: &Value) -> usize {
    probe
        .get("streams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        .filter(|stream| {
            stream
                .pointer("/disposition/attached_pic")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                == 1
        })
        .count()
}

fn read_be_u32(bytes: &[u8], offset: usize) -> usize {
    u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("four-byte big-endian field"),
    ) as usize
}

fn read_be_u64(bytes: &[u8], offset: usize) -> usize {
    usize::try_from(u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("eight-byte big-endian field"),
    ))
    .expect("MP4 box size should fit usize")
}

fn count_mp4_ilst_atom(
    bytes: &[u8],
    mut pos: usize,
    end: usize,
    in_ilst: bool,
    target: &[u8; 4],
) -> usize {
    let mut count = 0usize;
    while pos + 8 <= end {
        let size32 = read_be_u32(bytes, pos);
        let code = &bytes[pos + 4..pos + 8];
        let mut header_size = 8usize;
        let size = match size32 {
            0 => end.saturating_sub(pos),
            1 if pos + 16 <= end => {
                header_size = 16;
                read_be_u64(bytes, pos + 8)
            }
            _ => size32,
        };
        if size < header_size || pos.saturating_add(size) > end {
            break;
        }

        let data_start = pos + header_size;
        let data_end = pos + size;
        if in_ilst {
            if code == target {
                count += 1;
            }
        } else if code == b"moov" || code == b"udta" {
            count += count_mp4_ilst_atom(bytes, data_start, data_end, false, target);
        } else if code == b"meta" && data_start + 4 <= data_end {
            count += count_mp4_ilst_atom(bytes, data_start + 4, data_end, false, target);
        } else if code == b"ilst" {
            count += count_mp4_ilst_atom(bytes, data_start, data_end, true, target);
        }

        pos += size;
    }
    count
}

fn mp4_ilst_atom_count(path: &Path, atom: &[u8; 4]) -> usize {
    let bytes = fs::read(path).expect("read M4A output for ilst inspection");
    count_mp4_ilst_atom(&bytes, 0, bytes.len(), false, atom)
}

fn assert_m4a_custom_artwork_state(path: &Path, pass: &str) -> BTreeMap<String, String> {
    let probe = ffprobe_json(path);
    let tags = ffprobe_tag_map(&probe);
    assert_eq!(
        tags.get("PRE_EMPHASIS").map(String::as_str),
        Some("1"),
        "{pass}: PRE_EMPHASIS freeform atom missing; tags were {tags:?}",
    );
    assert_eq!(
        tags.get("MY_NOTE").map(String::as_str),
        Some("keep me"),
        "{pass}: MY_NOTE freeform atom missing; tags were {tags:?}",
    );
    assert_eq!(
        attached_picture_count(&probe),
        1,
        "{pass}: ALAC/M4A must contain exactly one attached artwork stream",
    );
    assert_eq!(
        mp4_ilst_atom_count(path, b"covr"),
        1,
        "{pass}: ALAC/M4A must contain exactly one covr atom",
    );
    tags
}

#[tokio::test]
async fn strict_gate_exercises_single_file_m4a_freeform_artwork_and_loudgain_invariants() {
    const TEST: &str =
        "strict_gate_exercises_single_file_m4a_freeform_artwork_and_loudgain_invariants";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe", "AtomicParsley", "loudgain"]) {
        return;
    }

    let root = TempRoot::new("single-m4a-freeform-artwork-rg");
    let source_path = create_single_flac_with_custom_tags_and_artwork(&root.join("source"));
    assert_eq!(
        attached_picture_count(&ffprobe_json(&source_path)),
        1,
        "strict fixture must begin with exactly one embedded artwork stream",
    );
    let case_root = root.join("case");
    fs::create_dir_all(case_root.join("output")).expect("create output root");
    fs::create_dir_all(case_root.join("logs")).expect("create log root");

    let mut request = base_request(
        source_path,
        case_root.join("output"),
        case_root.join("logs"),
    );
    request.job_id = "single-m4a-freeform-artwork-rg".to_string();
    request.item_id = request.job_id.clone();
    request.settings.target_format = AudioFormat::Alac;
    request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
    request.settings.force_encode = true;
    // Exercise the exact production split: the planner transfers native tags
    // and embedded artwork, while the orchestrator must still run Metadata for
    // non-native M4A keys and apply AtomicParsley after the artwork-bearing
    // ffmpeg output exists.
    request.settings.metadata.transfer_tags = true;
    request.settings.metadata.preserve_artwork = true;
    request.settings.metadata.store_source_audio_md5 = false;
    request.settings.replay_gain.mode = Some(tonepoet_pipeline::ReplayGainMode::Both);
    request.settings.replay_gain.existing_tags =
        tonepoet_pipeline::ReplayGainExistingTagPolicy::Rescan;
    request.stages.metadata = StageRequirement::Enabled;
    request.stages.replaygain = StageRequirement::Disabled;
    request.container_extension = Some("m4a".to_string());

    let runner = RealToolRunner::new(HashMap::new());
    let reporter = RecordingReporter::new();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(request.clone(), &runner, &reporter, &cancel).await;
    assert!(
        matches!(&report.outcome, AlbumOutcome::Complete { .. }),
        "single-file ALAC pipeline did not complete: {:?}",
        report.outcome,
    );
    let stage_records = match &report.outcome {
        AlbumOutcome::Complete { stages, .. }
        | AlbumOutcome::Partial { stages, .. }
        | AlbumOutcome::Blocked { stages, .. } => stages,
    };
    assert_eq!(
        stage_records
            .iter()
            .find(|record| record.stage == PipelineStage::Metadata)
            .map(|record| &record.outcome),
        Some(&StageOutcome::Ok),
        "single-file M4A with custom keys must route through the production Metadata stage",
    );

    let source = report
        .source
        .as_ref()
        .expect("completed pipeline should retain prepared source facts");
    assert_eq!(source.kind, SourceKind::SingleFile);
    assert_eq!(source.tracks.len(), 1);
    assert!(source.tracks[0].metadata.pre_emphasis);
    assert_eq!(
        source.tracks[0]
            .metadata
            .extra
            .get("my_note")
            .map(String::as_str),
        Some("keep me"),
    );

    let mut published_artifacts = report
        .artifacts
        .clone()
        .expect("completed pipeline should retain artifact records");
    let output_path = match &mut published_artifacts.audio {
        AudioArtifacts::Tracks(tracks) => {
            assert_eq!(tracks.len(), 1, "single-file ALAC should publish one track");
            let track = tracks.first_mut().expect("one ALAC artifact");
            assert!(
                track.metadata_satisfaction.source_tags_transferred,
                "planner must report native source-tag transfer before authoritative M4A metadata",
            );
            assert!(
                track.metadata_satisfaction.artwork_transferred,
                "planner must report artwork transfer before the authoritative metadata rewrite",
            );
            assert!(track.final_path.is_file(), "published ALAC output should exist");
            track.staged_path = track.final_path.clone();
            track.final_path.clone()
        }
        AudioArtifacts::Merged(_) => panic!("single-file ALAC should not produce a merged artifact"),
    };
    let published = report
        .published
        .as_ref()
        .expect("completed pipeline should publish output");
    assert_eq!(
        published
            .entries
            .iter()
            .filter(|entry| matches!(&entry.role, PublishRole::Audio))
            .map(|entry| entry.final_path.as_path())
            .collect::<Vec<_>>(),
        vec![output_path.as_path()],
        "artifact and publication records must identify the same ALAC file",
    );

    let first_tags = assert_m4a_custom_artwork_state(&output_path, "production metadata pass");

    apply_metadata(&published_artifacts, source, &request, &runner, &cancel)
        .await
        .expect("second metadata/freeform pass on published output");
    let second_tags = assert_m4a_custom_artwork_state(&output_path, "second metadata pass");
    assert_eq!(
        first_tags, second_tags,
        "repeated metadata/freeform application must converge semantically",
    );

    request.stages.replaygain = StageRequirement::Enabled;
    let replaygain = apply_replaygain_with_source_and_tool_limits(
        &published_artifacts,
        Some(source),
        &request,
        &runner,
        &cancel,
        None,
    )
    .await
    .expect("loudgain scan");
    assert!(matches!(replaygain.outcome, StageOutcome::Ok));

    let after_replaygain = assert_m4a_custom_artwork_state(&output_path, "after loudgain");
    for key in [
        "REPLAYGAIN_TRACK_GAIN",
        "REPLAYGAIN_TRACK_PEAK",
        "REPLAYGAIN_ALBUM_GAIN",
        "REPLAYGAIN_ALBUM_PEAK",
    ] {
        assert!(
            after_replaygain
                .get(key)
                .is_some_and(|value| !value.trim().is_empty()),
            "after loudgain: missing {key}; tags were {after_replaygain:?}",
        );
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

    let root = TempRoot::new("depth-format-matrix");
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

    let mut failures = Vec::new();
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
        let Some(published) = report.published.as_ref() else {
            failures.push(format!(
                "{case_name}: pipeline did not publish: {:?}",
                report.outcome
            ));
            continue;
        };
        let audio: Vec<_> = published
            .entries
            .iter()
            .filter(|entry| matches!(&entry.role, PublishRole::Audio))
            .collect();
        if audio.len() != 2 {
            failures.push(format!(
                "{case_name}: expected two published tracks, got {}",
                audio.len()
            ));
            continue;
        }
        for entry in audio {
            if let Err(err) = assert_measurement(&entry.final_path, case.depth) {
                failures.push(format!("{case_name}: {err}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "depth/format matrix failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn source_target_preserves_float32_cue_sample_class() {
    const TEST: &str = "source_target_preserves_float32_cue_sample_class";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe"]) {
        return;
    }

    let root = TempRoot::new("source-float32-cue");
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
    assert_measurement(&audio[0].final_path, PcmBitDepth::Float32)
        .expect("float32 Source output measurement");

}
#[tokio::test]
async fn lossy_cue_source_defaults_to_integer_pcm_for_flac_and_wav() {
    const TEST: &str = "lossy_cue_source_defaults_to_integer_pcm_for_flac_and_wav";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe"]) {
        return;
    }

    let root = TempRoot::new("lossy-cue-source-default");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).expect("create source directory");
    let image = source_dir.join("lossy.mp3");
    create_sine(&image, "libmp3lame", 44_100, 2.0);
    // Two tracks: track 1 ends at an INDEX position (interior boundary stays
    // exact), track 2 is the image tail whose length is MEASURED from the
    // decode (the MP3 header's frame count includes encoder delay/padding).
    let cue = source_dir.join("lossy.cue");
    fs::write(
        &cue,
        "FILE \"lossy.mp3\" MP3\n  TRACK 01 AUDIO\n    TITLE \"Lossy One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Lossy Two\"\n    INDEX 01 00:01:00\n",
    )
    .expect("write lossy CUE");

    let cases = [
        (AudioFormat::Flac, "flac"),
        (AudioFormat::Wav, "wav"),
    ];
    let mut failures = Vec::new();
    for (format, extension) in cases {
        let case_name = format!("lossy-source-{format:?}");
        let mut request = base_request(
            cue.clone(),
            root.join(&case_name).join("output"),
            root.join(&case_name).join("logs"),
        );
        request.item_id = case_name.clone();
        request.settings.target_format = format;
        request.settings.target_bit_depth = BitDepthTarget::Source;
        request.settings.preferred_tool = PreferredTool::Ffmpeg;
        request.settings.force_encode = true;
        request.container_extension = Some(extension.to_string());

        let runner = RealToolRunner::new(HashMap::new());
        let reporter = RecordingReporter::new();
        let report = run_pipeline_item(request, &runner, &reporter, &CancellationToken::new()).await;
        let Some(published) = report.published.as_ref() else {
            failures.push(format!("{case_name}: pipeline did not publish: {:?}", report.outcome));
            continue;
        };
        let audio: Vec<_> = published
            .entries
            .iter()
            .filter(|entry| matches!(&entry.role, PublishRole::Audio))
            .collect();
        if audio.len() != 2 {
            failures.push(format!(
                "{case_name}: expected two published tracks, got {}",
                audio.len()
            ));
            continue;
        }
        for entry in &audio {
            if let Err(error) = assert_measurement(&entry.final_path, PcmBitDepth::Int24) {
                failures.push(format!("{case_name}: {error}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "lossy CUE Source-default failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn source_target_preserves_float32_wavpack_cue_sample_class() {
    const TEST: &str = "source_target_preserves_float32_wavpack_cue_sample_class";
    if !require_tools_or_skip(TEST, &["ffmpeg", "ffprobe", "wvunpack"]) {
        return;
    }

    let root = TempRoot::new("source-float32-wavpack-cue");
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).expect("create source directory");
    let image = source_dir.join("float.wv");
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
            "sine=frequency=1000:sample_rate=96000:duration=0.25",
            "-c:a",
            "wavpack",
            "-sample_fmt",
            "fltp",
        ])
        .arg(&image)
        .output()
        .expect("launch float WavPack fixture encoder");
    assert!(
        output.status.success(),
        "float WavPack fixture encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        authoritative_wavpack_depth(&image).expect("measure float WavPack fixture"),
        PcmBitDepth::Float32,
        "fixture must be genuinely float WavPack"
    );

    let cue = source_dir.join("float.cue");
    fs::write(
        &cue,
        "FILE \"float.wv\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Float\"\n    INDEX 01 00:00:00\n",
    )
    .expect("write float WavPack CUE");

    let mut request = base_request(cue, root.join("output"), root.join("logs"));
    request.item_id = "source-float32-wavpack".to_string();
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
        .unwrap_or_else(|| panic!("float WavPack Source pipeline did not publish: {:?}", report.outcome));
    let audio: Vec<_> = published
        .entries
        .iter()
        .filter(|entry| matches!(&entry.role, PublishRole::Audio))
        .collect();
    assert_eq!(audio.len(), 1);
    assert_measurement(&audio[0].final_path, PcmBitDepth::Float32)
        .expect("float32 WavPack Source output measurement");
}

//! Cross-tool interop guard for the album-gain PCM carrier.
//!
//! The carrier is written by SoX and read back by the production FFmpeg
//! consumer. A same-tool round trip cannot detect a disagreement between those
//! two, which is how a Float64 CAF carrier once shipped that SoX wrote
//! correctly and FFmpeg decoded as full-scale noise. These tests therefore
//! exercise the real write-tool/read-tool pair at a known level.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use tonepoet_pipeline::{
    build_album_gain_analysis_command, extract_single_sox_stats_peak_report, plan_conversion,
    AudioCodec, AudioFormat, BitDepthTarget, DbNano, DsdAutoGainScope, DsdRate,
    DsdToPcmGainMode, InputSource, OutputSink, PcmBitDepth, PipelineSettings,
    PlanAction, PlanRequest, PreferredTool, RateTarget, SampleKind, SourceInfo,
    SourceRepresentationKind, ToolIdentifier,
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tonepoet-dsd-album-carrier-interop-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create carrier interop temp root");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
    let required = std::env::var_os("TONEPOET_REQUIRE_TOOLS")
        .map(|value| value != "0" && !value.is_empty())
        .unwrap_or(false);
    if required {
        panic!("{test_name}: required tools unavailable: {}", missing.join(", "));
    }
    eprintln!(
        "{test_name}: skipped; required tools unavailable: {}",
        missing.join(", ")
    );
    false
}

fn album_settings(target_format: AudioFormat, target_depth: PcmBitDepth) -> PipelineSettings {
    let mut settings = PipelineSettings::default();
    settings.target_format = target_format;
    settings.target_sample_rate = RateTarget::PcmHz(96_000);
    settings.target_bit_depth = BitDepthTarget::Pcm(target_depth);
    settings.preferred_tool = PreferredTool::Ffmpeg;
    settings.force_encode = true;
    settings.metadata.transfer_tags = false;
    settings.metadata.preserve_artwork = false;
    settings.metadata.store_source_audio_md5 = false;
    settings
        .dsd
        .set_legacy_dsd_to_pcm_gain(DsdToPcmGainMode::Auto, 0.15, None)
        .expect("legacy album auto gain");
    settings.dsd.set_auto_gain_scope(DsdAutoGainScope::Album);
    settings
}

fn dsd_source() -> SourceInfo {
    SourceInfo {
        dsd_source_kind: None,
        format: AudioFormat::Dsf,
        codec: AudioCodec::Dsd,
        sample_rate_hz: Some(DsdRate::Dsd64.hz()),
        bit_depth: None,
        true_source_depth: None,
        source_representation: SourceRepresentationKind::Dsd,
        sample_kind: Some(SampleKind::Dsd),
        channels: Some(2),
        duration: None,
        audio_md5: None,
    }
}

fn run_checked(tool: &str, args: &[String], description: &str) -> std::process::Output {
    let output = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not launch {description}: {error}"));
    assert!(
        output.status.success(),
        "{description} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn carrier_writer_output_args(
    settings: &PipelineSettings,
    carrier: &Path,
) -> Vec<String> {
    let synthetic_input = Path::new("synthetic-album-input.dsf");
    let planned = build_album_gain_analysis_command(
        settings,
        &dsd_source(),
        synthetic_input,
        carrier,
        None,
    )
    .expect("production album-gain analysis command");

    let input_text = synthetic_input.display().to_string();
    let output_text = carrier.display().to_string();
    let input_index = planned
        .args
        .iter()
        .position(|arg| arg == &input_text)
        .expect("analysis command contains input path");
    let output_index = planned
        .args
        .iter()
        .position(|arg| arg == &output_text)
        .expect("analysis command contains carrier path");
    assert!(
        input_index < output_index,
        "unexpected analysis argv: {:?}",
        planned.args
    );
    planned.args[input_index + 1..=output_index].to_vec()
}

#[test]
fn sox_written_album_carrier_survives_production_ffmpeg_consumer_at_known_level() {
    const TEST: &str =
        "sox_written_album_carrier_survives_production_ffmpeg_consumer_at_known_level";
    if !require_tools_or_skip(TEST, &["sox", "ffmpeg"]) {
        return;
    }

    let root = TempRoot::new();
    let fixture = root.join("known-level.wav");
    let carrier = root.join("album-carrier.f64le");
    let requested_output = root.join("decoded.flac");

    run_checked(
        "sox",
        &[
            "-n".to_string(),
            "-r".to_string(),
            "96000".to_string(),
            "-c".to_string(),
            "2".to_string(),
            fixture.display().to_string(),
            "synth".to_string(),
            "0.25".to_string(),
            "sine".to_string(),
            "1000".to_string(),
            "vol".to_string(),
            "-6dB".to_string(),
        ],
        "known-level SoX fixture generation",
    );

    let mut writer_args = vec![
        "-S".to_string(),
        "-D".to_string(),
        fixture.display().to_string(),
    ];
    let writer_settings = album_settings(AudioFormat::Wav, PcmBitDepth::Float64);
    writer_args.extend(carrier_writer_output_args(&writer_settings, &carrier));
    run_checked(
        "sox",
        &writer_args,
        "production-format album carrier writer",
    );
    assert!(
        fs::metadata(&carrier)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false),
        "album carrier was not created"
    );

    let mut consumer_settings = album_settings(AudioFormat::Flac, PcmBitDepth::Int24);
    consumer_settings
        .dsd
        .set_runtime_album_gain_db(Some("0.000000000".parse().expect("zero dB")));
    let request = PlanRequest {
        resolved_output_target: None,
        reference_programme_scope: Default::default(),
        planned_riff_non_audio_upper_bound_bytes: None,
        input_path: carrier.clone(),
        output_path: requested_output,
        source: SourceInfo {
            dsd_source_kind: None,
            format: AudioFormat::Wav,
            codec: AudioCodec::PcmFloat,
            sample_rate_hz: Some(96_000),
            bit_depth: Some(PcmBitDepth::Float64),
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Float),
            channels: Some(2),
            duration: None,
            audio_md5: None,
        },
        settings: consumer_settings,
        intermediate_dir: Some(root.0.clone()),
        container_ffmpeg_flags: Vec::new(),
    };

    let plan = plan_conversion(&request).expect("production album-carrier consumer plan");
    let command = match plan.action {
        PlanAction::Execute { commands, steps, .. } => {
            assert!(steps.is_empty(), "legacy album carrier must use static commands");
            assert_eq!(commands.len(), 1, "unexpected consumer plan: {commands:?}");
            commands.into_iter().next().expect("consumer command")
        }
        other => panic!("album carrier unexpectedly planned as {other:?}"),
    };
    assert_eq!(command.tool, ToolIdentifier::Ffmpeg);
    assert_eq!(command.input, InputSource::Path(carrier.clone()));
    let encoded = match &command.output {
        OutputSink::Path(path) => path.clone(),
        other => panic!("unexpected consumer output: {other:?}"),
    };
    run_checked("ffmpeg", &command.args, "production FFmpeg album carrier consumer");

    let measurement = run_checked(
        "sox",
        &[
            encoded.display().to_string(),
            "-n".to_string(),
            "stats".to_string(),
        ],
        "post-consumer known-level measurement",
    );
    let stderr = String::from_utf8_lossy(&measurement.stderr);
    let peak: DbNano = extract_single_sox_stats_peak_report(&stderr, 2)
        .unwrap_or_else(|error| panic!("could not parse post-consumer peak: {error}\n{stderr}"))
        .parse()
        .expect("finite known-level SoX peak");
    let lower: DbNano = "-6.100000000".parse().expect("lower bound");
    let upper: DbNano = "-5.900000000".parse().expect("upper bound");
    assert!(
        peak >= lower && peak <= upper,
        "SoX-written production carrier did not survive the production FFmpeg read: peak={peak}; expected approximately -6 dBFS\nconsumer argv={:?}\nmeasurement stderr:\n{stderr}",
        command.args,
    );
}

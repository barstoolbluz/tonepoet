//! Tool-gated release qualification for the P0 Reference DSD pathway.
//!
//! Run with:
//! `TONEPOET_REQUIRE_TOOLS=1 cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture`
//!
//! The test is inert unless explicitly selected. Release automation must set
//! the gate while using the flake-owned SoX-ng and FFmpeg paths.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use fs2::FileExt;
use sacd_rs::dsd_file::DsdFrameReader;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tonepoet::convert::pipeline::{
    qualify_reference_materialization_identity_digest,
    qualify_reference_source_materialization,
};
use tonepoet_pipeline::{
    build_reference_render_transcript_fixture, build_reference_silence_scan_command,
    extract_single_loudnorm_report, parse_reference_true_peak_measurement, plan_conversion,
    plan_reference_dsd,
    resolve_reference_deferred_command, validate_post_final_true_peak,
    validate_signed_zero_f64le, AudioCodec, AudioFormat, BitDepthTarget, ConversionPlan, DbNano,
    DsdInputFrontEnd, DsdReconstructionSelection, DsdReferencePolicyVersion, DsdSourceGainMode,
    DsdSourceKind, Finalization, MeasurementId, MeasurementParser, PcmBitDepth, PlanAction,
    PipelineSettings, PlanRequest, PlannedArg, PlannedCommand, PlannedExecutionStep, RateTarget,
    ReferenceErrorCode, ResolvedDsdProfile,
    ReferenceProgrammeScope, ResolvedGainPolicy, ResolvedOutputTarget, SampleKind,
    SourceInfo, SourceRepresentationKind, ToolIdentifier, TruePeakMeasurement,
    TruePeakPurpose, TruePeakValue, WavPackMode,
};

const GATE: &str = "TONEPOET_REQUIRE_TOOLS";
const SOX_ENV: &str = "TONEPOET_REFERENCE_SOX_PATH";
const FFMPEG_ENV: &str = "TONEPOET_REFERENCE_FFMPEG_PATH";
const QUALIFICATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const QUALIFICATION_PIPELINE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const QUALIFICATION_TERMINATION_TIMEOUT: Duration = Duration::from_secs(10);
const QUALIFICATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn selected() -> bool {
    std::env::var(GATE).as_deref() == Ok("1")
}

fn required_tool(variable: &str) -> PathBuf {
    let raw = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} must be set by the qualified package or dev shell"));
    fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("cannot canonicalize {variable}={}: {error}", Path::new(&raw).display()))
}

fn required_sibling_tool(tool: &Path, executable: &str) -> PathBuf {
    let candidate = tool
        .parent()
        .unwrap_or_else(|| panic!("qualified tool has no parent: {}", tool.display()))
        .join(executable);
    fs::canonicalize(&candidate).unwrap_or_else(|error| {
        panic!(
            "cannot canonicalize qualified sibling {} for {}: {error}",
            candidate.display(),
            tool.display()
        )
    })
}

fn apply_qualified_environment(command: &mut Command) {
    command.env_clear();
    command.env("LC_ALL", "C");
}

const QUALIFICATION_RETAINED_TAIL_BYTES: usize = 64 * 1024;

struct OutputDrain {
    label: &'static str,
    tail: Arc<Mutex<Vec<u8>>>,
    completion: mpsc::Receiver<Result<(), String>>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl OutputDrain {
    fn finish(mut self) -> (Vec<u8>, Option<String>) {
        let result = self
            .completion
            .recv_timeout(QUALIFICATION_TERMINATION_TIMEOUT)
            .map_err(|error| {
                format!(
                    "{} drain did not finish within {:?}: {error}",
                    self.label, QUALIFICATION_TERMINATION_TIMEOUT
                )
            })
            .and_then(|result| result);
        if result.is_ok() {
            if let Some(task) = self.task.take() {
                if task.join().is_err() {
                    return (
                        self.tail.lock().expect("output tail lock").clone(),
                        Some(format!("{} drain thread panicked", self.label)),
                    );
                }
            }
        }
        let tail = self.tail.lock().expect("output tail lock").clone();
        (tail, result.err())
    }
}

fn drain_child_output(
    mut stream: impl Read + Send + 'static,
    label: &'static str,
) -> OutputDrain {
    let tail = Arc::new(Mutex::new(Vec::with_capacity(
        QUALIFICATION_RETAINED_TAIL_BYTES,
    )));
    let writer_tail = tail.clone();
    let (sender, completion) = mpsc::sync_channel(1);
    let task = std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let result = loop {
            match stream.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(read) => {
                    let mut retained = writer_tail.lock().expect("output tail lock");
                    retained.extend_from_slice(&buffer[..read]);
                    if retained.len() > QUALIFICATION_RETAINED_TAIL_BYTES {
                        let excess = retained.len() - QUALIFICATION_RETAINED_TAIL_BYTES;
                        retained.drain(..excess);
                    }
                }
                Err(error) => break Err(format!("cannot drain {label}: {error}")),
            }
        };
        let _ = sender.send(result);
    });
    OutputDrain {
        label,
        tail,
        completion,
        task: Some(task),
    }
}

fn terminate_and_reap_result(child: &mut Child, label: &str) -> Result<ExitStatus, String> {
    let inspection_error = match child.try_wait() {
        Ok(Some(status)) => return Ok(status),
        Ok(None) => None,
        Err(error) => Some(error),
    };
    let kill_error = child.kill().err();
    let deadline = Instant::now() + QUALIFICATION_TERMINATION_TIMEOUT;
    let mut last_wait_error = inspection_error;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => last_wait_error = Some(error),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{label} did not terminate and reap within {:?}; kill_error={:?}; wait_error={:?}",
                QUALIFICATION_TERMINATION_TIMEOUT, kill_error, last_wait_error
            ));
        }
        std::thread::sleep(QUALIFICATION_POLL_INTERVAL);
    }
}

fn terminate_and_reap(child: &mut Child, label: &str) -> ExitStatus {
    terminate_and_reap_result(child, label).unwrap_or_else(|message| panic!("{message}"))
}

fn wait_with_deadline(
    child: &mut Child,
    label: &str,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(QUALIFICATION_POLL_INTERVAL);
            }
            Ok(None) => {
                return match terminate_and_reap_result(child, label) {
                    Ok(status) => Err(format!(
                        "{label} exceeded qualification deadline {timeout:?} and was terminated/reaped with status {status}"
                    )),
                    Err(termination) => Err(format!(
                        "{label} exceeded qualification deadline {timeout:?}; {termination}"
                    )),
                };
            }
            Err(error) => {
                return match terminate_and_reap_result(child, label) {
                    Ok(status) => Err(format!(
                        "cannot inspect {label}: {error}; terminated/reaped with status {status}"
                    )),
                    Err(termination) => Err(format!(
                        "cannot inspect {label}: {error}; {termination}"
                    )),
                };
            }
        }
    }
}

fn run_configured_command<F>(path: &Path, args: &[String], configure_environment: F) -> Output
where
    F: FnOnce(&mut Command),
{
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_environment(&mut command);
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("failed to run {} {:?}: {error}", path.display(), args));
    let stdout_task = drain_child_output(
        child.stdout.take().expect("qualified command stdout is piped"),
        "qualified command stdout",
    );
    let stderr_task = drain_child_output(
        child.stderr.take().expect("qualified command stderr is piped"),
        "qualified command stderr",
    );
    let status = wait_with_deadline(&mut child, "qualified command", QUALIFICATION_COMMAND_TIMEOUT);
    let (stdout, stdout_drain_error) = stdout_task.finish();
    let (stderr, stderr_drain_error) = stderr_task.finish();
    let status = status.unwrap_or_else(|failure| {
        panic!(
            "{failure}; stdout_drain_error={stdout_drain_error:?}; \
             stderr_drain_error={stderr_drain_error:?}; stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        )
    });
    assert!(
        stdout_drain_error.is_none() && stderr_drain_error.is_none(),
        "qualified command output drain failed: stdout={stdout_drain_error:?} stderr={stderr_drain_error:?}"
    );
    let output = Output {
        status,
        stdout,
        stderr,
    };
    assert!(
        output.status.success(),
        "{} {:?} failed: stdout={} stderr={}",
        path.display(),
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn run_with_pre_clear_environment(
    path: &Path,
    args: &[String],
    pre_clear_environment: &[(&str, &str)],
) -> Output {
    run_configured_command(path, args, |command| {
        for (key, value) in pre_clear_environment {
            command.env(key, value);
        }
        apply_qualified_environment(command);
    })
}

fn run_planned_legacy_command(path: &Path, planned: &PlannedCommand) -> Output {
    run_configured_command(path, &planned.args, |command| {
        match planned.environment_policy {
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet => {}
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet => {
                command.env_clear();
            }
        }
        for (key, value) in &planned.environment {
            command.env(key, value);
        }
    })
}

fn run(path: &Path, args: &[String]) -> Output {
    run_with_pre_clear_environment(path, args, &[])
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn qualified_environment_probe_child() {
    println!(
        "ambient={} lc_all={}",
        std::env::var("TONEPOET_QUALIFICATION_AMBIENT_POISON")
            .unwrap_or_else(|_| "unset".to_string()),
        std::env::var("LC_ALL").unwrap_or_else(|_| "unset".to_string()),
    );
}

fn qualify_subprocess_environment_isolation() -> Value {
    let executable = std::env::current_exe().expect("resolve qualification test executable");
    let output = run_with_pre_clear_environment(
        &executable,
        &[
            "--exact".to_string(),
            "qualified_environment_probe_child".to_string(),
            "--nocapture".to_string(),
        ],
        &[("TONEPOET_QUALIFICATION_AMBIENT_POISON", "present")],
    );
    let text = combined(&output);
    assert!(
        text.contains("ambient=unset lc_all=C"),
        "clear-and-set environment probe observed unexpected child environment: {text}"
    );
    serde_json::json!({
        "status": "passed",
        "schema": "tonepoet-reference-subprocess-environment-probe/v1",
        "policy": "clear_and_set",
        "qualified_environment": {"LC_ALL": "C"},
        "ambient_poison_key": "TONEPOET_QUALIFICATION_AMBIENT_POISON",
        "ambient_poison_observed": false,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_fixture_corpus_digest(files: &[(&str, &[u8])]) -> String {
    let mut sorted = files.to_vec();
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    let mut hasher = Sha256::new();
    hasher.update(b"sacd-rs-dst-reference-fixtures/v2\0");
    for (name, bytes) in sorted {
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn json_u64(value: &Value, field: &str) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        .unwrap_or_else(|| panic!("ffprobe field {field} is not an unsigned integer: {value}"))
}

fn optional_json_u64(value: Option<&Value>, field: &str) -> u64 {
    match value {
        None | Some(Value::Null) => 0,
        Some(value) => json_u64(value, field),
    }
}

fn assert_exact_package_probe(
    ffprobe: &Path,
    input: &Path,
    target: &str,
    depth: &str,
    sample_rate_hz: u32,
    channels: u16,
) {
    let output = run(
        ffprobe,
        &[
            "-v".to_string(),
            "error".to_string(),
            "-select_streams".to_string(),
            "a:0".to_string(),
            "-show_entries".to_string(),
            "stream=codec_name,sample_fmt,sample_rate,channels,bits_per_sample,bits_per_raw_sample:format=format_name".to_string(),
            "-of".to_string(),
            "json".to_string(),
            input.display().to_string(),
        ],
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "ffprobe JSON did not parse for {}: {error}",
            input.display()
        )
    });
    let streams = value["streams"]
        .as_array()
        .unwrap_or_else(|| panic!("ffprobe omitted streams for {}", input.display()));
    assert_eq!(streams.len(), 1, "expected exactly one selected audio stream");
    let stream = &streams[0];
    let expected_codec = match (target, depth) {
        ("flac_native", _) => "flac",
        ("wav_riff" | "wav_rf64" | "wav_w64", "int16") => "pcm_s16le",
        ("wav_riff" | "wav_rf64" | "wav_w64", "int24") => "pcm_s24le",
        ("wav_riff" | "wav_rf64" | "wav_w64", "float32") => "pcm_f32le",
        ("wav_riff" | "wav_rf64" | "wav_w64", "float64") => "pcm_f64le",
        ("aiff_native", "int16") => "pcm_s16be",
        ("aiff_native", "int24") => "pcm_s24be",
        ("wavpack_native", _) => "wavpack",
        ("alac_m4a", _) => "alac",
        _ => panic!("unsupported probe cell {target}/{depth}"),
    };
    assert_eq!(
        stream["codec_name"], expected_codec,
        "codec mismatch for {target}/{depth}"
    );
    assert_eq!(
        json_u64(&stream["sample_rate"], "sample_rate"),
        u64::from(sample_rate_hz),
        "sample-rate mismatch for {target}/{depth}"
    );
    assert_eq!(
        json_u64(&stream["channels"], "channels"),
        u64::from(channels),
        "channel mismatch for {target}/{depth}"
    );
    let bits_per_raw_sample =
        optional_json_u64(stream.get("bits_per_raw_sample"), "bits_per_raw_sample");
    let bits_per_sample = optional_json_u64(stream.get("bits_per_sample"), "bits_per_sample");
    // Prefer the codec's authoritative raw-depth declaration when present.
    // Container sample widths may be wider than the semantically stored PCM.
    let effective_bits = if bits_per_raw_sample != 0 {
        bits_per_raw_sample
    } else {
        bits_per_sample
    };
    let expected_bits = match depth {
        "int16" => 16,
        "int24" => 24,
        "float32" => 32,
        "float64" => 64,
        _ => panic!("unknown depth {depth}"),
    };
    assert_eq!(
        effective_bits, expected_bits,
        "terminal-depth mismatch for {target}/{depth}"
    );
    let sample_fmt = stream["sample_fmt"]
        .as_str()
        .unwrap_or_else(|| panic!("ffprobe omitted sample_fmt for {target}/{depth}"));
    if depth.starts_with("float") {
        assert!(
            sample_fmt.starts_with("flt") || sample_fmt.starts_with("dbl"),
            "float cell reported integer sample format {sample_fmt}"
        );
    } else {
        assert!(
            !sample_fmt.starts_with("flt") && !sample_fmt.starts_with("dbl"),
            "integer cell reported floating sample format {sample_fmt}"
        );
    }
    let format_name = value["format"]["format_name"]
        .as_str()
        .unwrap_or_else(|| panic!("ffprobe omitted format_name for {target}/{depth}"));
    let expected_format = match target {
        "flac_native" => "flac",
        "wav_riff" | "wav_rf64" => "wav",
        "wav_w64" => "w64",
        "aiff_native" => "aiff",
        "wavpack_native" => "wv",
        "alac_m4a" => "m4a",
        _ => panic!("unknown target {target}"),
    };
    assert!(
        format_name.split(',').any(|name| name == expected_format),
        "container mismatch for {target}/{depth}: {format_name}"
    );
}

fn ffmpeg_sample_hash(ffmpeg: &Path, input: &Path, pcm_codec: &str) -> String {
    let output = run(
        ffmpeg,
        &[
            "-nostdin".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-map_metadata".to_string(),
            "-1".to_string(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-dn".to_string(),
            "-c:a".to_string(),
            pcm_codec.to_string(),
            "-f".to_string(),
            "hash".to_string(),
            "-hash".to_string(),
            "sha256".to_string(),
            "-".to_string(),
        ],
    );
    combined(&output)
        .lines()
        .find_map(|line| line.trim().strip_prefix("SHA256="))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("FFmpeg hash output was missing SHA256=: {}", combined(&output)))
}


fn sox_streamed_float64_w64_sample_hash(
    sox: &Path,
    ffmpeg: &Path,
    input: &Path,
    sample_rate_hz: u32,
    channels: u16,
) -> String {
    let producer_args = vec![
        "-S".to_string(),
        "-D".to_string(),
        input.display().to_string(),
        "-t".to_string(),
        "raw".to_string(),
        "-e".to_string(),
        "floating-point".to_string(),
        "-b".to_string(),
        "64".to_string(),
        "-L".to_string(),
        "-".to_string(),
    ];
    let consumer_args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "f64le".to_string(),
        "-ar".to_string(),
        sample_rate_hz.to_string(),
        "-ac".to_string(),
        channels.to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-vn".to_string(),
        "-sn".to_string(),
        "-dn".to_string(),
        "-c:a".to_string(),
        "pcm_f64le".to_string(),
        "-f".to_string(),
        "hash".to_string(),
        "-hash".to_string(),
        "sha256".to_string(),
        "-".to_string(),
    ];

    let mut producer_command = Command::new(sox);
    producer_command
        .args(&producer_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut producer_command);
    let mut producer_child = producer_command.spawn().unwrap_or_else(|error| {
        panic!("failed to spawn Float64 hash producer {}: {error}", sox.display())
    });
    let producer_stderr_task = drain_child_stderr(&mut producer_child, "Float64 hash producer");
    let producer_stdout = producer_child
        .stdout
        .take()
        .expect("Float64 hash producer stdout is piped");

    let mut consumer_command = Command::new(ffmpeg);
    consumer_command
        .args(&consumer_args)
        .stdin(Stdio::from(producer_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut consumer_command);
    let consumer_child = match consumer_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let status = terminate_and_reap(
                &mut producer_child,
                "Float64 hash producer after consumer spawn failure",
            );
            let (producer_stderr, producer_drain_error) = producer_stderr_task.finish();
            panic!(
                "failed to spawn Float64 hash consumer {}: {error}; producer_status={status}; producer_drain_error={producer_drain_error:?}; producer_stderr={}",
                ffmpeg.display(),
                String::from_utf8_lossy(&producer_stderr),
            )
        }
    };
    let output = supervise_qualified_pipeline(
        producer_child,
        producer_stderr_task,
        consumer_child,
        "Float64 W64 sample hash pipeline",
    );
    assert!(output.producer.status.success());
    assert!(output.consumer.status.success());
    combined(&output.consumer)
        .lines()
        .find_map(|line| line.trim().strip_prefix("SHA256="))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!(
                "streamed Float64 hash output was missing SHA256=: {}",
                combined(&output.consumer)
            )
        })
}

fn synth_r64_fixture(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    amplitude: &str,
    silence: bool,
) {
    let mut args = vec![
        "-S".to_string(),
        "-D".to_string(),
        "-n".to_string(),
        "-r".to_string(),
        sample_rate_hz.to_string(),
        "-c".to_string(),
        channels.to_string(),
        "-t".to_string(),
        "w64".to_string(),
        "-e".to_string(),
        "floating-point".to_string(),
        "-b".to_string(),
        "64".to_string(),
        output.display().to_string(),
    ];
    if silence {
        args.extend(["trim".to_string(), "0".to_string(), "0.05".to_string()]);
    } else {
        args.extend([
            "synth".to_string(),
            "0.05".to_string(),
            "sine".to_string(),
            "997".to_string(),
            "vol".to_string(),
            amplitude.to_string(),
        ]);
    }
    run(sox, &args);
}

fn write_dsf_reference_fixture(path: &Path, channels: u16, sample_rate_hz: u32) {
    let file = File::create(path).expect("create DSF qualification fixture");
    let mut writer = sacd_rs::dsf_writer::DsfWriter::new(
        file,
        channels.try_into().expect("DSF fixture channel count fits u8"),
        sample_rate_hz,
    )
    .expect("create DSF qualification writer");
    let payload_bytes = usize::from(channels) * 32_768;
    let payload = vec![0x69; payload_bytes];
    writer
        .write_interleaved(&payload)
        .expect("write deterministic DSF payload");
    writer.finish().expect("finish DSF qualification fixture");
}

fn qualify_default_settings_dsd64_dsf_to_flac() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let ffprobe = required_sibling_tool(&ffmpeg, "ffprobe");
    let temp = TempDir::new().expect("default-settings smoke tempdir");
    let input = temp.path().join("default-settings-dsd64.dsf");
    let output = temp.path().join("default-settings.flac");
    let work = temp.path().join("work");
    fs::create_dir_all(&work).expect("create default-settings smoke workdir");
    write_dsf_reference_fixture(&input, 2, 2_822_400);

    let settings = PipelineSettings::default();
    assert!(
        !settings.dsd.is_native_v2(),
        "pre-promotion default must retain the frozen legacy DSD route"
    );
    let request = PlanRequest {
        input_path: input.clone(),
        output_path: output.clone(),
        source: SourceInfo {
            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            dsd_source_kind: None,
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(work),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: None,
        reference_programme_scope: ReferenceProgrammeScope::Singleton,
        planned_riff_non_audio_upper_bound_bytes: None,
    };
    let plan = plan_conversion(&request).expect("default-settings DSD64 DSF plan");
    assert!(
        plan.reference.is_none(),
        "default-settings smoke must exercise the frozen legacy route"
    );

    let (commands, finalization) = match &plan.action {
        PlanAction::Execute {
            commands,
            steps,
            finalization,
            ..
        } => {
            assert!(
                steps.is_empty(),
                "legacy plan must not contain native Reference steps"
            );
            assert!(
                !commands.is_empty(),
                "legacy plan must execute at least one command"
            );
            (commands, finalization.as_ref().expect("legacy finalization"))
        }
        PlanAction::PassthroughCopy { .. } => {
            panic!("DSD64 DSF to FLAC must not plan a passthrough copy")
        }
    };

    for command in commands {
        assert_eq!(
            command.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            "legacy command environment identity drifted"
        );
        let tool = match &command.tool {
            ToolIdentifier::Sox => &sox,
            ToolIdentifier::Ffmpeg => &ffmpeg,
            other => panic!("unexpected default-settings smoke tool {other:?}"),
        };
        let executed = run_planned_legacy_command(tool, command);
        assert!(
            executed.status.success(),
            "default-settings smoke command failed: tool={:?} args={:?} status={} stderr={}",
            command.tool,
            command.args,
            executed.status,
            String::from_utf8_lossy(&executed.stderr),
        );
    }
    match finalization {
        Finalization::AtomicRename { from, to } => {
            assert_eq!(to, &output);
            fs::rename(from, to).expect("publish default-settings smoke output");
        }
    }

    assert_exact_package_probe(&ffprobe, &output, "flac_native", "int24", 88_200, 2);
    let output_bytes = fs::read(&output).expect("read default-settings smoke output");
    serde_json::json!({
        "status": "passed",
        "route": "legacy_flat_v1",
        "source": "dsd64_dsf",
        "target": "flac_native",
        "sample_rate_hz": 88200,
        "channels": 2,
        "bit_depth": "int24",
        "command_count": commands.len(),
        "commands": commands.iter().map(|command| serde_json::json!({
            "tool": command.tool.program(),
            "args": &command.args,
            "environment_policy": "inherit_and_set",
        })).collect::<Vec<_>>(),
        "output_sha256": sha256_hex(&output_bytes),
    })
}

#[test]
fn default_settings_dsd64_dsf_to_flac_live_smoke() {
    if !selected() {
        eprintln!("skipping; set {GATE}=1 to run the default-settings DSD smoke");
        return;
    }
    let _ = qualify_default_settings_dsd64_dsf_to_flac();
}

fn write_dff_reference_fixture(path: &Path, channels: u16, sample_rate_hz: u32) {
    let file = File::create(path).expect("create DSDIFF qualification fixture");
    let mut writer = sacd_rs::dff_writer::DffWriter::new(
        file,
        channels
            .try_into()
            .expect("DSDIFF fixture channel count fits u8"),
        sample_rate_hz,
    )
    .expect("create DSDIFF qualification writer");
    let payload_bytes = usize::from(channels) * 32_768;
    let payload = vec![0x96; payload_bytes];
    writer
        .write_frame(&payload)
        .expect("write deterministic DSDIFF payload");
    writer.finish().expect("finish DSDIFF qualification fixture");
}

fn collect_decoded_dsd(path: &Path) -> Vec<u8> {
    let file = File::open(path).expect("open decoded DSD fixture");
    let mut reader = sacd_rs::open_dsd_as_decoded_reader(file)
        .expect("open DSD fixture through production reader");
    let mut decoded = Vec::new();
    while let Some(frame) = reader
        .next_dsd_frame()
        .expect("decode DSD fixture frame")
    {
        decoded.extend_from_slice(&frame.data);
    }
    decoded
}

#[cfg(unix)]
fn assert_not_hard_linked(left: &Path, right: &Path) {
    use std::os::unix::fs::MetadataExt;
    let left = fs::metadata(left).expect("stat source materialization");
    let right = fs::metadata(right).expect("stat private materialization");
    assert_ne!(
        (left.dev(), left.ino()),
        (right.dev(), right.ino()),
        "Reference private materialization must not hard-link the source",
    );
}

#[cfg(not(unix))]
fn assert_not_hard_linked(left: &Path, right: &Path) {
    assert_ne!(
        fs::canonicalize(left).expect("canonical source"),
        fs::canonicalize(right).expect("canonical private materialization"),
    );
}

#[derive(Debug, Clone, Copy)]
enum AnalyzerPeakPosition {
    Early,
    Late,
}

impl AnalyzerPeakPosition {
    const fn key(self) -> &'static str {
        match self {
            Self::Early => "early",
            Self::Late => "late",
        }
    }
}

fn write_analytic_analyzer_fixture(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    true_peak_dbfs: f64,
    normalized_frequency: f64,
    phase_radians: f64,
    duration_seconds: f64,
    peak_position: AnalyzerPeakPosition,
) -> f64 {
    write_analytic_analyzer_fixture_with_depth(
        sox,
        output,
        sample_rate_hz,
        channels,
        true_peak_dbfs,
        normalized_frequency,
        phase_radians,
        duration_seconds,
        peak_position,
        64,
    )
}

fn write_analytic_analyzer_fixture_with_depth(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    true_peak_dbfs: f64,
    normalized_frequency: f64,
    phase_radians: f64,
    duration_seconds: f64,
    peak_position: AnalyzerPeakPosition,
    output_bits: u16,
) -> f64 {
    assert!(matches!(output_bits, 32 | 64), "unsupported float fixture depth");
    let amplitude = 10_f64.powf(true_peak_dbfs / 20.0);
    let raw = output.with_extension("f64le");
    let sample_count = ((f64::from(sample_rate_hz) * duration_seconds).ceil() as u32)
        .max(4_096);
    let active_len = sample_count * 2 / 5;
    let active_start = match peak_position {
        AnalyzerPeakPosition::Early => sample_count / 20,
        AnalyzerPeakPosition::Late => sample_count - sample_count / 20 - active_len,
    };
    let active_end = active_start + active_len;
    let ramp_len = (active_len / 5).max(64);
    let angular_frequency = std::f64::consts::TAU * normalized_frequency;
    let mut bytes = Vec::with_capacity(sample_count as usize * usize::from(channels) * 8);
    let mut sample_peak = 0.0_f64;
    for index in 0..sample_count {
        let envelope = if index < active_start || index >= active_end {
            0.0
        } else if index < active_start + ramp_len {
            let offset = index - active_start;
            0.5 - 0.5
                * (std::f64::consts::PI * f64::from(offset) / f64::from(ramp_len)).cos()
        } else if index >= active_end - ramp_len {
            let remaining = active_end - 1 - index;
            0.5 - 0.5
                * (std::f64::consts::PI * f64::from(remaining) / f64::from(ramp_len)).cos()
        } else {
            1.0
        };
        for channel in 0..channels {
            // Stereo uses an independent phase relationship so the analyzer is
            // qualified against channel maxima rather than duplicated samples.
            let channel_phase = phase_radians
                + f64::from(channel) * std::f64::consts::FRAC_PI_3;
            let phase = angular_frequency * f64::from(index) + channel_phase;
            let sample = amplitude * envelope * phase.sin();
            sample_peak = sample_peak.max(sample.abs());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    fs::write(&raw, bytes).expect("write analytic f64 analyzer fixture");
    run(
        sox,
        &[
            "-D".to_string(),
            "-t".to_string(),
            "f64".to_string(),
            "-L".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-r".to_string(),
            sample_rate_hz.to_string(),
            "-c".to_string(),
            channels.to_string(),
            raw.display().to_string(),
            "-t".to_string(),
            "w64".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            output_bits.to_string(),
            output.display().to_string(),
        ],
    );
    fs::remove_file(raw).expect("remove analytic raw analyzer fixture");
    20.0 * sample_peak.log10()
}

fn write_analytic_multitone_fixture(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    true_peak_dbfs: f64,
    peak_offset_samples: f64,
    peak_position: AnalyzerPeakPosition,
) -> f64 {
    const FREQUENCIES: [f64; 4] = [0.03125, 0.1171875, 0.2734375, 0.4453125];
    const WEIGHTS: [f64; 4] = [0.4, 0.3, 0.2, 0.1];
    let amplitude = 10_f64.powf(true_peak_dbfs / 20.0);
    let raw = output.with_extension("multitone.f64le");
    let sample_count = (f64::from(sample_rate_hz) * 0.250).ceil() as u32;
    let sample_count = sample_count.max(8_192);
    let active_len = sample_count * 2 / 5;
    let active_start = match peak_position {
        AnalyzerPeakPosition::Early => sample_count / 20,
        AnalyzerPeakPosition::Late => sample_count - sample_count / 20 - active_len,
    };
    let active_end = active_start + active_len;
    let ramp_len = (active_len / 5).max(128);
    let peak_time = f64::from(active_start + ramp_len + 64) + peak_offset_samples;
    assert!(peak_time + 1.0 < f64::from(active_end - ramp_len));
    let mut bytes = Vec::with_capacity(sample_count as usize * usize::from(channels) * 8);
    let mut sample_peak = 0.0_f64;
    for index in 0..sample_count {
        let envelope = if index < active_start || index >= active_end {
            0.0
        } else if index < active_start + ramp_len {
            let offset = index - active_start;
            0.5 - 0.5
                * (std::f64::consts::PI * f64::from(offset) / f64::from(ramp_len)).cos()
        } else if index >= active_end - ramp_len {
            let remaining = active_end - 1 - index;
            0.5 - 0.5
                * (std::f64::consts::PI * f64::from(remaining) / f64::from(ramp_len)).cos()
        } else {
            1.0
        };
        for channel in 0..channels {
            let channel_peak_time = peak_time + f64::from(channel) * 0.125;
            let normalized = FREQUENCIES
                .iter()
                .zip(WEIGHTS)
                .map(|(frequency, weight)| {
                    weight
                        * (std::f64::consts::TAU
                            * frequency
                            * (f64::from(index) - channel_peak_time))
                            .cos()
                })
                .sum::<f64>();
            let sample = amplitude * envelope * normalized;
            sample_peak = sample_peak.max(sample.abs());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    fs::write(&raw, bytes).expect("write analytic multitone analyzer fixture");
    run(
        sox,
        &[
            "-D".to_string(),
            "-t".to_string(),
            "f64".to_string(),
            "-L".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-r".to_string(),
            sample_rate_hz.to_string(),
            "-c".to_string(),
            channels.to_string(),
            raw.display().to_string(),
            "-t".to_string(),
            "w64".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            output.display().to_string(),
        ],
    );
    fs::remove_file(raw).expect("remove analytic multitone raw fixture");
    20.0 * sample_peak.log10()
}

fn target_format(target: ResolvedOutputTarget) -> AudioFormat {
    match target {
        ResolvedOutputTarget::FlacNative => AudioFormat::Flac,
        ResolvedOutputTarget::WavRiff
        | ResolvedOutputTarget::WavRf64
        | ResolvedOutputTarget::WavW64 => AudioFormat::Wav,
        ResolvedOutputTarget::AiffNative => AudioFormat::Aiff,
        ResolvedOutputTarget::WavPackNative => AudioFormat::WavPack,
        ResolvedOutputTarget::AlacM4a => AudioFormat::Alac,
        other => panic!("unsupported qualification target {other:?}"),
    }
}

fn target_key(target: ResolvedOutputTarget) -> &'static str {
    match target {
        ResolvedOutputTarget::FlacNative => "flac_native",
        ResolvedOutputTarget::WavRiff => "wav_riff",
        ResolvedOutputTarget::WavRf64 => "wav_rf64",
        ResolvedOutputTarget::WavW64 => "wav_w64",
        ResolvedOutputTarget::AiffNative => "aiff_native",
        ResolvedOutputTarget::WavPackNative => "wavpack_native",
        ResolvedOutputTarget::AlacM4a => "alac_m4a",
        other => panic!("unsupported qualification target {other:?}"),
    }
}

fn target_extension(target: ResolvedOutputTarget) -> &'static str {
    match target {
        ResolvedOutputTarget::FlacNative => "flac",
        ResolvedOutputTarget::WavRiff => "wav",
        ResolvedOutputTarget::WavRf64 => "rf64.wav",
        ResolvedOutputTarget::WavW64 => "w64",
        ResolvedOutputTarget::AiffNative => "aiff",
        ResolvedOutputTarget::WavPackNative => "wv",
        ResolvedOutputTarget::AlacM4a => "m4a",
        other => panic!("unsupported qualification target {other:?}"),
    }
}

fn wavpack_mode(level: u8) -> WavPackMode {
    match level {
        0 => WavPackMode::Fast,
        1 => WavPackMode::Normal,
        2 => WavPackMode::High,
        3 => WavPackMode::VeryHigh,
        _ => panic!("invalid WavPack level {level}"),
    }
}

fn assert_production_plan_structure(
    plan: &ConversionPlan,
    expected_compression_level: Option<u8>,
) {
    let summary = plan.reference.as_ref().expect("Reference summary");
    let steps = plan.steps();
    assert!(matches!(steps.len(), 4 | 5));

    let render = match &steps[0] {
        PlannedExecutionStep::Command(command) => command,
        other => panic!("first Reference step is not render: {other:?}"),
    };
    assert!(matches!(&render.tool, ToolIdentifier::Sox));
    assert_eq!(render.args.first().map(String::as_str), Some("-S"));
    assert_eq!(render.args.get(1).map(String::as_str), Some("-D"));
    assert!(!render.args.iter().any(|arg| matches!(arg.as_str(), "-G" | "-R" | "norm" | "dither")));
    let gain = render
        .args
        .windows(2)
        .position(|window| window[0] == "gain" && window[1] == "-12.000000000")
        .expect("render has exact headroom gain");
    let rate = render
        .args
        .windows(2)
        .position(|window| window[0] == "rate" && window[1] == "-u")
        .expect("render has rate -u");
    assert!(gain < rate, "render gain must precede rate -u");
    if let Some(sinc) = render.args.iter().position(|arg| arg == "sinc") {
        assert!(rate < sinc, "rate -u must precede explicit sinc");
    }

    let measurements: Vec<_> = steps
        .iter()
        .filter_map(|step| match step {
            PlannedExecutionStep::Measurement(measurement) => Some(measurement),
            _ => None,
        })
        .collect();
    assert_eq!(measurements.len(), 2);
    assert_eq!(measurements[0].id, MeasurementId(1));
    assert_eq!(measurements[0].purpose, TruePeakPurpose::GainAuthority);
    assert_eq!(measurements[1].id, MeasurementId(2));
    assert_eq!(measurements[1].purpose, TruePeakPurpose::PostFinalAcceptance);
    assert!(measurements
        .iter()
        .all(|measurement| measurement.parser == MeasurementParser::FfmpegLoudnormInputTpV3));
    for measurement in &measurements {
        assert_eq!(measurement.command.tool, ToolIdentifier::Ffmpeg);
        let carrier = measurement
            .carrier_path()
            .expect("measurement carrier is path-backed")
            .to_string_lossy()
            .into_owned();
        let direct_float32_post = summary.final_pcm.bit_depth == PcmBitDepth::Float32
            && measurement.purpose == TruePeakPurpose::PostFinalAcceptance;
        if direct_float32_post {
            assert!(measurement.input_stage.is_none());
            assert_eq!(
                measurement.command.input.as_path(),
                measurement.carrier_path()
            );
            assert!(measurement
                .command
                .args
                .windows(2)
                .any(|window| window[0] == "-i" && window[1] == carrier));
            assert!(!measurement
                .command
                .args
                .windows(2)
                .any(|window| window[0] == "-f" && window[1] == "wav"));
        } else {
            let producer = measurement
                .input_stage
                .as_ref()
                .expect("policy v7 f64 measurement has a typed producer");
            assert_eq!(producer.tool, ToolIdentifier::Sox);
            assert_eq!(producer.output, tonepoet_pipeline::OutputSink::Stdout);
            assert_eq!(producer.args.get(0).map(String::as_str), Some("-S"));
            assert_eq!(producer.args.get(1).map(String::as_str), Some("-D"));
            assert!(producer
                .args
                .windows(2)
                .any(|window| window[0] == "-t" && window[1] == "wav"));
            assert!(producer
                .args
                .windows(2)
                .any(|window| window[0] == "-e" && window[1] == "floating-point"));
            assert!(producer
                .args
                .windows(2)
                .any(|window| window[0] == "-b" && window[1] == "64"));
            assert_eq!(producer.args.last().map(String::as_str), Some("-"));
            assert_eq!(measurement.command.input, tonepoet_pipeline::InputSource::Stdin);
            assert!(measurement
                .command
                .args
                .windows(2)
                .any(|window| window[0] == "-f" && window[1] == "wav"));
            assert!(measurement
                .command
                .args
                .windows(2)
                .any(|window| window[0] == "-i" && window[1] == "pipe:0"));
            assert!(!measurement.command.args.iter().any(|arg| arg == &carrier));
        }
    }

    let deferred: Vec<_> = steps
        .iter()
        .filter_map(|step| match step {
            PlannedExecutionStep::DeferredCommand(command) => Some(command),
            _ => None,
        })
        .collect();
    assert_eq!(deferred.len(), 1, "exactly one terminal realization is planned");
    let terminal = deferred[0];
    assert!(matches!(&terminal.tool, ToolIdentifier::Sox));
    assert!(matches!(terminal.args.first(), Some(PlannedArg::Literal(value)) if value == "-S"));
    assert!(matches!(terminal.args.get(1), Some(PlannedArg::Literal(value)) if value == "-D"));
    let bound_gain_count = terminal
        .args
        .iter()
        .filter(|arg| matches!(arg, PlannedArg::BoundGainDb { .. }))
        .count();
    match summary.gain_policy {
        ResolvedGainPolicy::NormalizePeak { .. } => assert_eq!(bound_gain_count, 0),
        _ => assert_eq!(bound_gain_count, 1),
    }

    let packages: Vec<_> = steps
        .iter()
        .skip(1)
        .filter_map(|step| match step {
            PlannedExecutionStep::Command(command) => Some(command),
            _ => None,
        })
        .collect();
    if summary.target == ResolvedOutputTarget::WavW64 {
        assert!(packages.is_empty());
        assert_eq!(summary.qpcm_path, summary.packaged_path);
    } else {
        assert_eq!(packages.len(), 1);
        let package = packages[0];
        assert!(matches!(&package.tool, ToolIdentifier::Ffmpeg));
        assert!(!package.args.iter().any(|arg| matches!(
            arg.as_str(),
            "-af" | "-filter:a" | "-ar" | "-sample_fmt"
        )));
        match summary.target {
            ResolvedOutputTarget::FlacNative | ResolvedOutputTarget::WavPackNative => {
                let expected = expected_compression_level.expect("compression level").to_string();
                assert!(package.args.windows(2).any(|window| {
                    window[0] == "-compression_level" && window[1] == expected
                }));
            }
            _ => assert!(expected_compression_level.is_none()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn planned_reference_source_cell(
    root: &Path,
    input: &Path,
    source_rate_hz: u32,
    target_rate_hz: u32,
    channels: u16,
    source_format: AudioFormat,
    source_kind: DsdSourceKind,
    depth: PcmBitDepth,
    target: ResolvedOutputTarget,
    profile: DsdReconstructionSelection,
    gain_mode: DsdSourceGainMode,
    fixed_gain_db: Option<DbNano>,
    normalize_target_dbfs: DbNano,
    compression_level: Option<u8>,
) -> ConversionPlan {
    let mut settings = PipelineSettings::default();
    settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
    settings.target_format = target_format(target);
    settings.target_sample_rate = RateTarget::PcmHz(target_rate_hz);
    settings.target_bit_depth = BitDepthTarget::Pcm(depth);
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V7;
    settings.dsd.from_dsd.profile = profile;
    settings.dsd.from_dsd.gain_mode = gain_mode;
    settings.dsd.from_dsd.fixed_gain_db = fixed_gain_db;
    settings.dsd.from_dsd.normalize_peak_target_dbfs = normalize_target_dbfs;
    settings.wavpack.hybrid = false;
    settings.wavpack.correction_file = false;
    if target == ResolvedOutputTarget::FlacNative {
        settings.flac.compression_level = compression_level.expect("FLAC level");
    }
    if target == ResolvedOutputTarget::WavPackNative {
        settings.wavpack.mode = wavpack_mode(compression_level.expect("WavPack level"));
    }
    let output = root.join(format!(
        "final-{}-{}-{}ch.{}",
        target_key(target),
        target_rate_hz,
        channels,
        target_extension(target)
    ));
    let request = PlanRequest {
        input_path: input.to_path_buf(),
        output_path: output,
        source: SourceInfo {
            format: source_format,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(source_rate_hz),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(channels),
            duration: Some(std::time::Duration::from_millis(50)),
            dsd_source_kind: Some(source_kind),
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(root.join("work")),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: Some(target),
        reference_programme_scope: ReferenceProgrammeScope::Singleton,
        planned_riff_non_audio_upper_bound_bytes: Some(0),
    };
    fs::create_dir_all(root.join("work")).expect("create planner work directory");
    let plan = plan_reference_dsd(&request).expect("qualified Reference cell must plan");
    assert_production_plan_structure(&plan, compression_level);
    plan
}

#[allow(clippy::too_many_arguments)]
fn planned_reference_cell(
    root: &Path,
    input: &Path,
    source_rate_hz: u32,
    target_rate_hz: u32,
    channels: u16,
    depth: PcmBitDepth,
    target: ResolvedOutputTarget,
    profile: DsdReconstructionSelection,
    gain_mode: DsdSourceGainMode,
    fixed_gain_db: Option<DbNano>,
    normalize_target_dbfs: DbNano,
    compression_level: Option<u8>,
) -> ConversionPlan {
    planned_reference_source_cell(
        root,
        input,
        source_rate_hz,
        target_rate_hz,
        channels,
        AudioFormat::Dsf,
        DsdSourceKind::DsfUncompressed,
        depth,
        target,
        profile,
        gain_mode,
        fixed_gain_db,
        normalize_target_dbfs,
        compression_level,
    )
}

fn run_planned_command(
    command: &PlannedCommand,
    sox: &Path,
    ffmpeg: &Path,
) -> Output {
    assert_eq!(
        command.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(command.environment.len(), 1);
    assert_eq!(command.environment.get("LC_ALL").map(String::as_str), Some("C"));
    let tool = match &command.tool {
        ToolIdentifier::Sox => sox,
        ToolIdentifier::Ffmpeg => ffmpeg,
        other => panic!("unexpected Reference qualification tool {other:?}"),
    };
    run(tool, &command.args)
}

struct PlannedPipelineOutput {
    producer: Output,
    consumer: Output,
}

struct PlannedMeasurementOutput {
    producer: Option<Output>,
    consumer: Output,
}

fn drain_child_stderr(child: &mut Child, label: &'static str) -> OutputDrain {
    drain_child_output(
        child
            .stderr
            .take()
            .unwrap_or_else(|| panic!("{label} stderr is piped")),
        label,
    )
}

fn supervise_qualified_pipeline(
    mut producer_child: Child,
    producer_stderr_task: OutputDrain,
    mut consumer_child: Child,
    label: &str,
) -> PlannedPipelineOutput {
    let consumer_stdout_task = drain_child_output(
        consumer_child
            .stdout
            .take()
            .expect("qualified pipeline consumer stdout is piped"),
        "qualified pipeline consumer stdout",
    );
    let consumer_stderr_task = drain_child_output(
        consumer_child
            .stderr
            .take()
            .expect("qualified pipeline consumer stderr is piped"),
        "qualified pipeline consumer stderr",
    );
    let deadline = Instant::now() + QUALIFICATION_PIPELINE_TIMEOUT;
    let mut producer_status = None;
    let mut consumer_status = None;
    let mut supervisor_failure: Option<String> = None;

    loop {
        if producer_status.is_none() {
            match producer_child.try_wait() {
                Ok(status) => producer_status = status,
                Err(error) => {
                    supervisor_failure = Some(format!("cannot inspect {label} producer: {error}"));
                }
            }
        }
        if consumer_status.is_none() {
            match consumer_child.try_wait() {
                Ok(status) => consumer_status = status,
                Err(error) => {
                    supervisor_failure = Some(format!("cannot inspect {label} consumer: {error}"));
                }
            }
        }
        if producer_status
            .as_ref()
            .is_some_and(|status| !status.success())
            && consumer_status.is_none()
        {
            match terminate_and_reap_result(
                &mut consumer_child,
                "qualified pipeline consumer after producer failure",
            ) {
                Ok(status) => consumer_status = Some(status),
                Err(error) => supervisor_failure = Some(error),
            }
        }
        if consumer_status
            .as_ref()
            .is_some_and(|status| !status.success())
            && producer_status.is_none()
        {
            match terminate_and_reap_result(
                &mut producer_child,
                "qualified pipeline producer after consumer failure",
            ) {
                Ok(status) => producer_status = Some(status),
                Err(error) => supervisor_failure = Some(error),
            }
        }
        if supervisor_failure.is_some()
            || (producer_status.is_some() && consumer_status.is_some())
        {
            break;
        }
        if Instant::now() >= deadline {
            supervisor_failure = Some(format!(
                "{label} exceeded qualification deadline {:?}",
                QUALIFICATION_PIPELINE_TIMEOUT
            ));
            break;
        }
        std::thread::sleep(QUALIFICATION_POLL_INTERVAL);
    }

    if supervisor_failure.is_some() {
        if producer_status.is_none() {
            match terminate_and_reap_result(
                &mut producer_child,
                "failed qualified pipeline producer",
            ) {
                Ok(status) => producer_status = Some(status),
                Err(error) => supervisor_failure
                    .as_mut()
                    .expect("supervisor failure exists")
                    .push_str(&format!("; producer termination failure: {error}")),
            }
        }
        if consumer_status.is_none() {
            match terminate_and_reap_result(
                &mut consumer_child,
                "failed qualified pipeline consumer",
            ) {
                Ok(status) => consumer_status = Some(status),
                Err(error) => supervisor_failure
                    .as_mut()
                    .expect("supervisor failure exists")
                    .push_str(&format!("; consumer termination failure: {error}")),
            }
        }
    }

    let (producer_stderr, producer_drain_error) = producer_stderr_task.finish();
    let (consumer_stdout, consumer_stdout_drain_error) = consumer_stdout_task.finish();
    let (consumer_stderr, consumer_stderr_drain_error) = consumer_stderr_task.finish();
    for drain_error in [
        producer_drain_error,
        consumer_stdout_drain_error,
        consumer_stderr_drain_error,
    ]
    .into_iter()
    .flatten()
    {
        match supervisor_failure.as_mut() {
            Some(failure) => failure.push_str(&format!("; {drain_error}")),
            None => supervisor_failure = Some(drain_error),
        }
    }

    if let Some(failure) = supervisor_failure {
        panic!(
            "{failure}; producer_status={producer_status:?} consumer_status={consumer_status:?} \
             producer_stderr={} consumer_stdout={} consumer_stderr={}",
            String::from_utf8_lossy(&producer_stderr),
            String::from_utf8_lossy(&consumer_stdout),
            String::from_utf8_lossy(&consumer_stderr),
        );
    }

    PlannedPipelineOutput {
        producer: Output {
            status: producer_status.expect("qualified producer has terminal status"),
            stdout: Vec::new(),
            stderr: producer_stderr,
        },
        consumer: Output {
            status: consumer_status.expect("qualified consumer has terminal status"),
            stdout: consumer_stdout,
            stderr: consumer_stderr,
        },
    }
}

fn run_streamed_measurement_pipeline(
    measurement: &tonepoet_pipeline::PlannedMeasurement,
    sox: &Path,
    ffmpeg: &Path,
) -> PlannedPipelineOutput {
    let producer = measurement
        .input_stage
        .as_ref()
        .expect("streamed v7-inherited measurement has a typed input stage");
    assert_eq!(producer.tool, ToolIdentifier::Sox);
    assert_eq!(producer.input.as_path(), measurement.carrier_path());
    assert_eq!(producer.output, tonepoet_pipeline::OutputSink::Stdout);
    assert_eq!(
        producer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(producer.environment, BTreeMap::from([("LC_ALL".to_string(), "C".to_string())]));
    assert_eq!(measurement.command.tool, ToolIdentifier::Ffmpeg);
    assert_eq!(measurement.command.input, tonepoet_pipeline::InputSource::Stdin);
    assert_eq!(
        measurement.command.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(
        measurement.command.environment,
        BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
    );

    let mut producer_command = Command::new(sox);
    producer_command
        .args(&producer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut producer_command);
    let mut producer_child = producer_command.spawn().unwrap_or_else(|error| {
        panic!("failed to spawn {} {:?}: {error}", sox.display(), producer.args)
    });
    let producer_stderr_task = drain_child_stderr(&mut producer_child, "measurement producer");
    let producer_stdout = producer_child
        .stdout
        .take()
        .expect("measurement producer stdout is piped");

    let mut consumer_command = Command::new(ffmpeg);
    consumer_command
        .args(&measurement.command.args)
        .stdin(Stdio::from(producer_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut consumer_command);
    let consumer_child = match consumer_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let status = terminate_and_reap(
                &mut producer_child,
                "measurement producer after consumer spawn failure",
            );
            let (producer_stderr, producer_drain_error) = producer_stderr_task.finish();
            panic!(
                "failed to spawn {} {:?}: {error}; producer_status={status}; producer_drain_error={producer_drain_error:?}; producer_stderr={}",
                ffmpeg.display(),
                measurement.command.args,
                String::from_utf8_lossy(&producer_stderr),
            )
        }
    };
    let output = supervise_qualified_pipeline(
        producer_child,
        producer_stderr_task,
        consumer_child,
        "measurement pipeline",
    );
    assert!(
        output.producer.status.success(),
        "{} {:?} failed: stderr={}",
        sox.display(),
        producer.args,
        String::from_utf8_lossy(&output.producer.stderr),
    );
    assert!(
        output.consumer.status.success(),
        "{} {:?} failed: stdout={} stderr={}",
        ffmpeg.display(),
        measurement.command.args,
        String::from_utf8_lossy(&output.consumer.stdout),
        String::from_utf8_lossy(&output.consumer.stderr),
    );
    output
}


fn run_planned_command_pipeline(
    pipeline: &tonepoet_pipeline::PlannedCommandPipeline,
    sox: &Path,
    ffmpeg: &Path,
) -> PlannedPipelineOutput {
    assert_eq!(pipeline.producer.tool, ToolIdentifier::Sox);
    assert_eq!(pipeline.producer.output, tonepoet_pipeline::OutputSink::Stdout);
    assert_eq!(
        pipeline.producer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(
        pipeline.producer.environment,
        BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
    );
    assert_eq!(pipeline.consumer.tool, ToolIdentifier::Ffmpeg);
    assert_eq!(pipeline.consumer.input, tonepoet_pipeline::InputSource::Stdin);
    assert_eq!(
        pipeline.consumer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(
        pipeline.consumer.environment,
        BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
    );

    let mut producer_command = Command::new(sox);
    producer_command
        .args(&pipeline.producer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut producer_command);
    let mut producer_child = producer_command.spawn().unwrap_or_else(|error| {
        panic!(
            "failed to spawn planned package producer {} {:?}: {error}",
            sox.display(),
            pipeline.producer.args
        )
    });
    let producer_stderr_task = drain_child_stderr(&mut producer_child, "package producer");
    let producer_stdout = producer_child
        .stdout
        .take()
        .expect("package producer stdout is piped");

    let mut consumer_command = Command::new(ffmpeg);
    consumer_command
        .args(&pipeline.consumer.args)
        .stdin(Stdio::from(producer_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut consumer_command);
    let consumer_child = match consumer_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let status = terminate_and_reap(
                &mut producer_child,
                "package producer after consumer spawn failure",
            );
            let (producer_stderr, producer_drain_error) = producer_stderr_task.finish();
            panic!(
                "failed to spawn planned package consumer {} {:?}: {error}; producer_status={status}; producer_drain_error={producer_drain_error:?}; producer_stderr={}",
                ffmpeg.display(),
                pipeline.consumer.args,
                String::from_utf8_lossy(&producer_stderr),
            )
        }
    };
    let output = supervise_qualified_pipeline(
        producer_child,
        producer_stderr_task,
        consumer_child,
        "planned Float64 package pipeline",
    );
    assert!(output.producer.status.success());
    assert!(output.consumer.status.success());
    output
}

fn loudnorm_input_tp(stderr: &[u8]) -> f64 {
    let stderr = String::from_utf8_lossy(stderr);
    let raw = extract_single_loudnorm_report(&stderr)
        .unwrap_or_else(|error| panic!("loudnorm report extraction failed: {error}"));
    let report: Value = serde_json::from_str(&raw).expect("loudnorm report parses");
    report["input_tp"]
        .as_str()
        .expect("loudnorm input_tp is a string")
        .parse::<f64>()
        .expect("loudnorm input_tp is finite decimal evidence")
}

#[cfg(unix)]
fn require_sparse_file_support(directory: &Path) {
    use std::os::unix::fs::MetadataExt;

    let probe = directory.join(".tonepoet-sparse-capacity-probe");
    let file = File::create(&probe).expect("create sparse-file capability probe");
    file.set_len(16 * 1024 * 1024)
        .expect("size sparse-file capability probe");
    let metadata = file
        .metadata()
        .expect("inspect sparse-file capability probe");
    drop(file);
    fs::remove_file(&probe).expect("remove sparse-file capability probe");
    let allocated = metadata.blocks().saturating_mul(512);
    assert!(
        allocated < metadata.len() / 8,
        "the mandatory >4 GiB analyzer-carrier fixture requires a sparse-file-capable qualification filesystem (logical={} allocated={allocated})",
        metadata.len(),
    );
}

#[cfg(not(unix))]
fn require_sparse_file_support(_directory: &Path) {
    panic!("the mandatory >4 GiB analyzer-carrier qualification fixture requires Unix sparse-file accounting");
}

fn create_sparse_w64_capacity_fixture(seed: &Path, output: &Path) -> u64 {
    const RIFF_GUID: &[u8; 16] = b"riff.\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\x00\x00";
    const FACT_GUID: &[u8; 16] = b"fact\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";
    const DATA_GUID: &[u8; 16] = b"data\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";
    const AUDIO_PAYLOAD_BYTES: u64 = (1_u64 << 32) + 8;

    let seed_bytes = fs::read(seed).expect("read seed W64");
    assert_eq!(&seed_bytes[..16], RIFF_GUID, "seed is W64");
    let fact = seed_bytes
        .windows(16)
        .position(|window| window == FACT_GUID)
        .expect("W64 fact chunk");
    let data = seed_bytes
        .windows(16)
        .position(|window| window == DATA_GUID)
        .expect("W64 data chunk");
    let payload_offset = data + 24;
    assert!(payload_offset <= seed_bytes.len(), "valid W64 data header");
    let frame_count = AUDIO_PAYLOAD_BYTES / 8;
    let file_size = u64::try_from(payload_offset).expect("W64 header size") + AUDIO_PAYLOAD_BYTES;

    let mut header = seed_bytes[..payload_offset].to_vec();
    header[16..24].copy_from_slice(&file_size.to_le_bytes());
    header[fact + 24..fact + 32].copy_from_slice(&frame_count.to_le_bytes());
    header[data + 16..data + 24]
        .copy_from_slice(&(AUDIO_PAYLOAD_BYTES + 24).to_le_bytes());

    let mut file = File::create(output).expect("create sparse W64 capacity fixture");
    file.write_all(&header).expect("write sparse W64 header");
    file.set_len(file_size).expect("size sparse W64 fixture");
    file.sync_all().expect("sync sparse W64 fixture");
    assert!(AUDIO_PAYLOAD_BYTES > u64::from(u32::MAX));
    AUDIO_PAYLOAD_BYTES
}

fn inspect_streaming_wav_header(producer: &PlannedCommand, sox: &Path) -> (u32, u32) {
    const HEADER_CAPTURE_BYTES: usize = 4096;
    const STREAMING_SENTINEL_FLOOR: u32 = 0x7fff_0000;

    assert_eq!(
        producer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(producer.environment, BTreeMap::from([("LC_ALL".to_string(), "C".to_string())]));
    let mut command = Command::new(sox);
    command
        .args(&producer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut command);
    let mut child = command.spawn().unwrap_or_else(|error| {
        panic!(
            "failed to spawn streaming-header producer {} {:?}: {error}",
            sox.display(),
            producer.args
        )
    });
    let stderr_task = drain_child_stderr(&mut child, "streaming-header producer");
    let mut stdout = child
        .stdout
        .take()
        .expect("streaming-header producer stdout is piped");
    let (sender, receiver) = mpsc::sync_channel(1);
    let header_task = std::thread::spawn(move || {
        let mut header = vec![0_u8; HEADER_CAPTURE_BYTES];
        let result = stdout
            .read_exact(&mut header)
            .map(|()| header)
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    let header = match receiver.recv_timeout(QUALIFICATION_COMMAND_TIMEOUT) {
        Ok(Ok(header)) => header,
        Ok(Err(error)) => {
            let status = terminate_and_reap(&mut child, "streaming-header producer after read failure");
            let (stderr, stderr_drain_error) = stderr_task.finish();
            panic!(
                "cannot read streaming WAV header: {error}; status={status}; stderr_drain_error={stderr_drain_error:?}; stderr={}",
                String::from_utf8_lossy(&stderr)
            );
        }
        Err(error) => {
            let status = terminate_and_reap(&mut child, "streaming-header producer after deadline");
            let (stderr, stderr_drain_error) = stderr_task.finish();
            panic!(
                "streaming WAV header read exceeded {:?}: {error}; status={status}; stderr_drain_error={stderr_drain_error:?}; stderr={}",
                QUALIFICATION_COMMAND_TIMEOUT,
                String::from_utf8_lossy(&stderr)
            );
        }
    };
    let status = terminate_and_reap(&mut child, "streaming-header producer after capture");
    header_task.join().expect("streaming-header reader joins");
    let (stderr, stderr_drain_error) = stderr_task.finish();
    assert!(
        stderr_drain_error.is_none(),
        "streaming-header stderr drain failed: {stderr_drain_error:?}; stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    let _ = status;

    assert_eq!(&header[..4], b"RIFF", "streaming carrier is RIFF/WAVE");
    assert_eq!(&header[8..12], b"WAVE", "streaming carrier is RIFF/WAVE");
    let riff_size_field = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let mut offset = 12_usize;
    let mut data_size_field = None;
    while offset.checked_add(8).is_some_and(|end| end <= header.len()) {
        let chunk_id = &header[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(header[offset + 4..offset + 8].try_into().unwrap());
        if chunk_id == b"data" {
            data_size_field = Some(chunk_size);
            break;
        }
        let padded = usize::try_from(chunk_size)
            .expect("WAV chunk size fits usize")
            .checked_add(1)
            .expect("WAV chunk size does not overflow")
            & !1;
        offset = offset
            .checked_add(8)
            .and_then(|value| value.checked_add(padded))
            .expect("WAV chunk traversal does not overflow");
    }
    let data_size_field = data_size_field.unwrap_or_else(|| {
        panic!(
            "streaming WAV data chunk was not present in the first {HEADER_CAPTURE_BYTES} bytes; stderr={}",
            String::from_utf8_lossy(&stderr)
        )
    });
    assert!(
        riff_size_field >= STREAMING_SENTINEL_FLOOR
            && data_size_field >= STREAMING_SENTINEL_FLOOR,
        "SoX-ng did not emit the frozen large streaming-WAV size sentinels: \
         riff={riff_size_field:#010x}, data={data_size_field:#010x}, stderr={}",
        String::from_utf8_lossy(&stderr),
    );
    (riff_size_field, data_size_field)
}

fn run_capacity_carrier_pipeline(
    producer: &PlannedCommand,
    sox: &Path,
    ffmpeg: &Path,
) -> PlannedPipelineOutput {
    assert_eq!(
        producer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(producer.environment, BTreeMap::from([("LC_ALL".to_string(), "C".to_string())]));
    let mut producer_command = Command::new(sox);
    producer_command
        .args(&producer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut producer_command);
    let mut producer_child = producer_command.spawn().unwrap_or_else(|error| {
        panic!(
            "failed to spawn capacity producer {} {:?}: {error}",
            sox.display(),
            producer.args
        )
    });
    let producer_stderr_task = drain_child_stderr(&mut producer_child, "capacity producer");
    let producer_stdout = producer_child
        .stdout
        .take()
        .expect("capacity producer stdout is piped");
    let consumer_args = [
        "-nostdin", "-hide_banner", "-nostats", "-loglevel", "info", "-f", "wav", "-i",
        "pipe:0", "-map", "0:a:0", "-c:a", "copy", "-f", "null", "-",
    ];
    let mut consumer_command = Command::new(ffmpeg);
    consumer_command
        .args(consumer_args)
        .stdin(Stdio::from(producer_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut consumer_command);
    let consumer_child = match consumer_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let status = terminate_and_reap(
                &mut producer_child,
                "capacity producer after consumer spawn failure",
            );
            let (producer_stderr, producer_drain_error) = producer_stderr_task.finish();
            panic!(
                "failed to spawn capacity consumer {}: {error}; producer_status={status}; \
                 producer_drain_error={producer_drain_error:?}; producer_stderr={}",
                ffmpeg.display(),
                String::from_utf8_lossy(&producer_stderr),
            )
        }
    };
    let output = supervise_qualified_pipeline(
        producer_child,
        producer_stderr_task,
        consumer_child,
        "greater-than-4-GiB capacity pipeline",
    );
    assert!(
        output.producer.status.success(),
        "capacity producer failed: {}",
        String::from_utf8_lossy(&output.producer.stderr)
    );
    assert!(
        output.consumer.status.success(),
        "capacity consumer failed: {}",
        String::from_utf8_lossy(&output.consumer.stderr)
    );
    output
}

fn qualify_analyzer_carrier_contract() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("analyzer carrier tempdir");
    let root = temp.path().join("carrier");
    fs::create_dir_all(&root).expect("create analyzer carrier root");

    // Float64 W64: FFmpeg 7.1 applies an erroneous 2^31 scale when it reads
    // SoX-ng's IEEE-f64 W64 directly. The canonical v6 route therefore keeps
    // the exact SoX W64 QPCM but streams a sample-exact f64 RIFF/WAV view into
    // FFmpeg. This is a transport, not a disk intermediate.
    let f64_source = root.join("f64-source-placeholder.dsf");
    let f64_plan = planned_reference_cell(
        &root,
        &f64_source,
        2_822_400,
        48_000,
        1,
        PcmBitDepth::Float64,
        ResolvedOutputTarget::WavW64,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let f64_summary = f64_plan.reference.as_ref().expect("Reference summary");
    let f64_analytic_peak = write_analytic_analyzer_fixture(
        &sox,
        &f64_summary.r64_path,
        48_000,
        1,
        -20.0,
        0.25,
        0.0,
        0.125,
        AnalyzerPeakPosition::Early,
    );
    let f64_measurement = f64_plan
        .steps()
        .iter()
        .find_map(|step| match step {
            PlannedExecutionStep::Measurement(measurement)
                if measurement.purpose == TruePeakPurpose::GainAuthority => Some(measurement),
            _ => None,
        })
        .expect("planner emits f64 gain measurement");

    let f64_direct_args = [
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-nostats".to_string(),
        "-loglevel".to_string(),
        "info".to_string(),
        "-i".to_string(),
        f64_summary.r64_path.display().to_string(),
        "-filter:a".to_string(),
        "loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let f64_direct = run(&ffmpeg, &f64_direct_args);
    let f64_direct_input_tp = loudnorm_input_tp(&f64_direct.stderr);
    let f64_defect_delta_db = f64_direct_input_tp - f64_analytic_peak;
    assert!(
        f64_direct_input_tp > 100.0
            && (f64_defect_delta_db - 20.0 * (2_f64.powi(31)).log10()).abs() < 0.02,
        "pinned direct f64-W64 defect changed: analytic={f64_analytic_peak}, direct={f64_direct_input_tp}, delta={f64_defect_delta_db}"
    );

    let f64_corrected = execute_measurement(f64_measurement, &sox, &ffmpeg, &root);
    let f64_corrected_input_tp = match f64_corrected.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("-20 dB f64 carrier measured as silence"),
    };
    assert!(
        (f64_corrected_input_tp - f64_analytic_peak).abs() <= 0.02,
        "streamed f64 carrier changed true peak: analytic={f64_analytic_peak}, corrected={f64_corrected_input_tp}"
    );

    let f64_producer = f64_measurement
        .input_stage
        .as_ref()
        .expect("policy v7 f64 producer stage");
    let streamed = run(&sox, &f64_producer.args);
    assert!(
        streamed.status.success(),
        "producer stream capture failed: {}",
        String::from_utf8_lossy(&streamed.stderr)
    );
    assert_eq!(&streamed.stdout[..4], b"RIFF");
    assert_eq!(&streamed.stdout[8..12], b"WAVE");
    let streamed_wav = root.join("captured-f64-analyzer-carrier.wav");
    fs::write(&streamed_wav, &streamed.stdout).expect("write captured analyzer stream");
    let f64_source_sample_bits = sox_f64_samples(&sox, &f64_summary.r64_path)
        .into_iter()
        .map(f64::to_bits)
        .collect::<Vec<_>>();
    let f64_streamed_sample_bits = sox_f64_samples(&sox, &streamed_wav)
        .into_iter()
        .map(f64::to_bits)
        .collect::<Vec<_>>();
    assert_eq!(
        f64_source_sample_bits, f64_streamed_sample_bits,
        "f64 W64 to streamed f64 WAV re-container changed decoded sample bits"
    );

    // Float32 W64: direct FFmpeg decoding is correct, while routing the same
    // carrier through SoX's W64 decoder before loudnorm drives the analyzer
    // result to full scale. The v6 post-terminal measurement must therefore be
    // direct and path-backed. This is the crossed binding fixed by v6.
    let f32_root = root.join("float32");
    fs::create_dir_all(&f32_root).expect("create Float32 carrier root");
    let f32_source = f32_root.join("f32-source-placeholder.dsf");
    let f32_plan = planned_reference_cell(
        &f32_root,
        &f32_source,
        2_822_400,
        48_000,
        1,
        PcmBitDepth::Float32,
        ResolvedOutputTarget::WavW64,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Fixed,
        Some(DbNano::ZERO),
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let f32_summary = f32_plan.reference.as_ref().expect("Float32 Reference summary");
    assert_eq!(f32_summary.qpcm_path, f32_summary.packaged_path);
    assert_eq!(
        f32_summary.qpcm_path.extension().and_then(|value| value.to_str()),
        Some("w64")
    );
    let f32_analytic_peak = write_analytic_analyzer_fixture_with_depth(
        &sox,
        &f32_summary.qpcm_path,
        48_000,
        1,
        -20.0,
        0.25,
        0.0,
        0.125,
        AnalyzerPeakPosition::Early,
        32,
    );
    assert!(
        f32_plan.steps().iter().all(|step| !matches!(
            step,
            PlannedExecutionStep::Command(command)
                if command.description == "Package terminal PCM without sample changes"
        )),
        "Float32 W64 must be direct QPCM without a package step"
    );
    let f32_post = f32_plan
        .steps()
        .iter()
        .find_map(|step| match step {
            PlannedExecutionStep::Measurement(measurement)
                if measurement.purpose == TruePeakPurpose::PostFinalAcceptance => Some(measurement),
            _ => None,
        })
        .expect("planner emits Float32 post measurement");
    assert!(f32_post.input_stage.is_none());
    assert_eq!(f32_post.command.input.as_path(), Some(f32_summary.qpcm_path.as_path()));
    assert_eq!(f32_post.parser, MeasurementParser::FfmpegLoudnormInputTpV3);
    let f32_direct = execute_measurement(f32_post, &sox, &ffmpeg, &f32_root);
    let f32_direct_input_tp = match f32_direct.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("-20 dB Float32 carrier measured as silence"),
    };
    assert!(
        (f32_direct_input_tp - f32_analytic_peak).abs() <= 0.02,
        "direct Float32-W64 measurement changed true peak: analytic={f32_analytic_peak}, direct={f32_direct_input_tp}"
    );

    let mut f32_sox_recontainer = f32_plan
        .steps()
        .iter()
        .find_map(|step| match step {
            PlannedExecutionStep::Measurement(measurement)
                if measurement.purpose == TruePeakPurpose::GainAuthority => Some(measurement.clone()),
            _ => None,
        })
        .expect("Float32 plan has streamed pre measurement");
    let f32_recontainer_producer = f32_sox_recontainer
        .input_stage
        .as_mut()
        .expect("pre measurement has SoX producer");
    f32_recontainer_producer.input =
        tonepoet_pipeline::InputSource::Path(f32_summary.qpcm_path.clone());
    f32_recontainer_producer.args[2] = f32_summary.qpcm_path.display().to_string();
    let f32_recontainer = run_streamed_measurement_pipeline(&f32_sox_recontainer, &sox, &ffmpeg);
    let f32_sox_recontainer_input_tp = loudnorm_input_tp(&f32_recontainer.consumer.stderr);
    let f32_sox_defect_delta_db = f32_sox_recontainer_input_tp - f32_direct_input_tp;
    assert!(
        f32_sox_recontainer_input_tp >= -0.10 && f32_sox_defect_delta_db > 10.0,
        "pinned SoX Float32-W64 readback defect changed: direct={f32_direct_input_tp}, recontainer={f32_sox_recontainer_input_tp}, delta={f32_sox_defect_delta_db}"
    );

    // Reproduce the exact admission failure shape independently: a -20 dBFS
    // Float32 cell, Reference-compensated gain, and a RIFF final target whose
    // terminal authority is still the W64 QPCM. Both measurements must bind to
    // that cell's own paths and the post-terminal result must remain below the
    // public -1 dBTP ceiling.
    let f1_root = root.join("f1-reference-gain-regression");
    fs::create_dir_all(&f1_root).expect("create F1 regression root");
    let f1_source = f1_root.join("f1-source-placeholder.dsf");
    let f1_plan = planned_reference_cell(
        &f1_root,
        &f1_source,
        2_822_400,
        44_100,
        1,
        PcmBitDepth::Float32,
        ResolvedOutputTarget::WavRiff,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let f1_summary = f1_plan.reference.as_ref().expect("F1 Reference summary");
    assert_eq!(
        f1_summary.qpcm_path.extension().and_then(|value| value.to_str()),
        Some("w64")
    );
    assert_eq!(
        f1_summary.packaged_path.extension().and_then(|value| value.to_str()),
        Some("wav")
    );
    write_analytic_analyzer_fixture(
        &sox,
        &f1_summary.r64_path,
        44_100,
        1,
        -20.0,
        0.25,
        0.0,
        0.125,
        AnalyzerPeakPosition::Early,
    );
    let f1_chain = execute_planned_terminal_chain(&f1_plan, &sox, &ffmpeg, &f1_root, false)
        .unwrap_or_else(|error| panic!("isolated F1 regression failed: {error}"));
    assert!(f1_chain.package_args.is_some(), "RIFF finalization must package from W64 QPCM");
    let f1_pre = f1_chain
        .measurements
        .get(&MeasurementId(1))
        .expect("F1 pre-final measurement");
    let f1_post = f1_chain
        .measurements
        .get(&MeasurementId(2))
        .expect("F1 post-final measurement");
    let f1_pre_reported = match f1_pre.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("F1 pre-final carrier measured as silence"),
    };
    let f1_post_reported = match f1_post.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("F1 post-final carrier measured as silence"),
    };
    assert!(
        (-20.20..=-19.80).contains(&f1_pre_reported),
        "F1 pre-final measurement observed the wrong fixture: {f1_pre_reported} dBTP"
    );
    let f1_applied_gain_db = f1_chain
        .terminal_args
        .windows(2)
        .find_map(|pair| {
            if pair[0] == "gain" {
                pair[1].parse::<f64>().ok()
            } else {
                None
            }
        })
        .expect("F1 terminal argv contains the resolved gain");
    let f1_expected_post_reported = f1_pre_reported + f1_applied_gain_db;
    assert!(
        (f1_post_reported - f1_expected_post_reported).abs() <= 0.03,
        "F1 pre/post measurements are not bound to one gain-consistent carrier: pre={f1_pre_reported}, gain={f1_applied_gain_db}, expected_post={f1_expected_post_reported}, post={f1_post_reported}"
    );
    match f1_post.conservative_upper {
        tonepoet_pipeline::TruePeakValue::Finite(upper) => assert!(
            upper <= DbNano::REFERENCE_CEILING,
            "F1 post-final conservative upper exceeds the public ceiling: reported={f1_post_reported}, conservative={}",
            upper.render(false)
        ),
        tonepoet_pipeline::TruePeakValue::VerifiedSilence => {}
    }

    require_sparse_file_support(&root);
    let sparse = root.join("over-4-gib.w64");
    let capacity_payload_bytes = create_sparse_w64_capacity_fixture(&f64_summary.r64_path, &sparse);
    let mut capacity_producer = f64_producer.clone();
    capacity_producer.input = tonepoet_pipeline::InputSource::Path(sparse.clone());
    capacity_producer.args[2] = sparse.display().to_string();
    let (riff_size_field, data_size_field) =
        inspect_streaming_wav_header(&capacity_producer, &sox);
    let capacity = run_capacity_carrier_pipeline(&capacity_producer, &sox, &ffmpeg);

    serde_json::json!({
        "status": "passed",
        "contract": "tonepoet-reference-analyzer-carrier/v2",
        "routing_rule": "float32_w64_direct_ffmpeg_else_sox_f64_wav_stream",
        "known_defect": {
            "status": "reproduced",
            "carrier": "sox_f64_w64_direct_to_ffmpeg_7_1",
            "analytic_peak_dbfs": f64_analytic_peak,
            "reported_input_tp_dbtp": f64_direct_input_tp,
            "scaling_delta_db": f64_defect_delta_db,
            "expected_scaling": "2^31",
        },
        "corrected_path": {
            "status": "passed",
            "carrier_depth": "float64",
            "transport": "direct_stdout_to_stdin_no_shell",
            "stream_encoding": "pcm_f64le",
            "reported_input_tp_dbtp": f64_corrected_input_tp,
            "sample_exact_recontainer": true,
            "parser": "ffmpeg_loudnorm_input_tp_v3",
            "environment_policy": "clear_and_set",
            "environment": {"LC_ALL": "C"},
            "pipeline_deadline_seconds": QUALIFICATION_PIPELINE_TIMEOUT.as_secs(),
            "termination_reap_deadline_seconds": QUALIFICATION_TERMINATION_TIMEOUT.as_secs(),
            "failure_contract": "terminate_and_reap_or_fail",
            "producer_argv": f64_producer.args.clone(),
            "consumer_input_argv": ["-f", "wav", "-i", "pipe:0"],
            "consumer_argv": f64_measurement.command.args.clone(),
        },
        "float32_direct_path": {
            "status": "passed",
            "carrier_depth": "float32",
            "carrier_container": "w64",
            "disk_intermediate": false,
            "package_step": false,
            "reported_input_tp_dbtp": f32_direct_input_tp,
            "analytic_peak_dbfs": f32_analytic_peak,
            "parser": "ffmpeg_loudnorm_input_tp_v3",
            "environment_policy": "clear_and_set",
            "environment": {"LC_ALL": "C"},
            "input_stage": null,
            "consumer_argv": f32_post.command.args.clone(),
        },
        "float32_sox_recontainer_defect": {
            "status": "reproduced",
            "carrier": "sox_float32_w64_to_f64_riff_stream",
            "direct_reported_input_tp_dbtp": f32_direct_input_tp,
            "recontainer_reported_input_tp_dbtp": f32_sox_recontainer_input_tp,
            "scaling_delta_db": f32_sox_defect_delta_db,
        },
        "f1_reference_gain_regression": {
            "status": "passed",
            "target_rate_hz": 44_100,
            "channels": 1,
            "depth": "float32",
            "final_target": "wav_riff",
            "qpcm_container": "w64",
            "pre_reported_input_tp_dbtp": f1_pre_reported,
            "applied_gain_db": f1_applied_gain_db,
            "expected_post_from_pre_and_gain_dbtp": f1_expected_post_reported,
            "post_reported_input_tp_dbtp": f1_post_reported,
            "post_conservative_upper_dbtp": match f1_post.conservative_upper {
                tonepoet_pipeline::TruePeakValue::Finite(upper) => upper.render(false),
                tonepoet_pipeline::TruePeakValue::VerifiedSilence => "-inf".to_string(),
            },
            "terminal_argv": f1_chain.terminal_args.clone(),
            "package_argv": f1_chain.package_args.clone(),
        },
        "greater_than_4_gib_stream": {
            "status": "passed",
            "sparse_source_container": "w64",
            "environment_policy": "clear_and_set",
            "environment": {"LC_ALL": "C"},
            "pipeline_deadline_seconds": QUALIFICATION_PIPELINE_TIMEOUT.as_secs(),
            "termination_reap_deadline_seconds": QUALIFICATION_TERMINATION_TIMEOUT.as_secs(),
            "failure_contract": "terminate_and_reap_or_fail",
            "audio_payload_bytes": capacity_payload_bytes,
            "riff_u32_max_bytes": u32::MAX,
            "streaming_sentinel_floor": 0x7fff_0000_u32,
            "riff_size_field": riff_size_field,
            "data_size_field": data_size_field,
            "read_to_eof": true,
            "consumer_mode": "stream_copy_to_null",
            "producer_exit": capacity.producer.status.code(),
            "consumer_exit": capacity.consumer.status.code(),
        },
    })
}

fn run_planned_measurement(
    measurement: &tonepoet_pipeline::PlannedMeasurement,
    sox: &Path,
    ffmpeg: &Path,
) -> PlannedMeasurementOutput {
    assert_eq!(measurement.parser, MeasurementParser::FfmpegLoudnormInputTpV3);
    assert_eq!(
        measurement.command.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(
        measurement.command.environment,
        BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
    );
    if measurement.input_stage.is_some() {
        let output = run_streamed_measurement_pipeline(measurement, sox, ffmpeg);
        PlannedMeasurementOutput {
            producer: Some(output.producer),
            consumer: output.consumer,
        }
    } else {
        assert_eq!(measurement.command.tool, ToolIdentifier::Ffmpeg);
        assert_eq!(measurement.command.input.as_path(), measurement.carrier_path());
        PlannedMeasurementOutput {
            producer: None,
            consumer: run(ffmpeg, &measurement.command.args),
        }
    }
}

fn policy_measurement_bounds() -> (DbNano, DbNano) {
    let qualification: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7.json"
    )))
    .expect("qualification JSON parses");
    let q = qualification["analyzer"]["reporting_uncertainty_db"]
        .as_str()
        .expect("reporting uncertainty is a string")
        .parse()
        .expect("reporting uncertainty is canonical DbNano");
    let e = qualification["analyzer"]["analyzer_residual_db"]
        .as_str()
        .expect("analyzer residual is a string")
        .parse()
        .expect("analyzer residual is canonical DbNano");
    (q, e)
}

fn execute_measurement(
    measurement: &tonepoet_pipeline::PlannedMeasurement,
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
) -> TruePeakMeasurement {
    let output = run_planned_measurement(measurement, sox, ffmpeg);
    if let Some(producer) = &output.producer {
        assert!(producer.stdout.is_empty());
    }
    let stderr = String::from_utf8_lossy(&output.consumer.stderr);
    let raw = extract_single_loudnorm_report(&stderr)
        .unwrap_or_else(|error| panic!("production loudnorm extraction failed: {error}"));
    let report: Value = serde_json::from_str(&raw).expect("loudnorm report parses as JSON");
    let silence = report["input_tp"] == "-inf";
    if silence {
        let input = measurement
            .carrier_path()
            .expect("measurement carrier is path-backed");
        let raw_path = root.join(format!("silence-{}.f64le", measurement.id.0));
        let scan = build_reference_silence_scan_command(input, &raw_path);
        run_planned_command(&scan, sox, ffmpeg);
        let bytes = fs::read(&raw_path).expect("read production silence scan");
        validate_signed_zero_f64le(&bytes).expect("production signed-zero proof");
        fs::remove_file(raw_path).expect("remove silence scan");
    }
    let (q, e) = policy_measurement_bounds();
    let parsed = parse_reference_true_peak_measurement(
        measurement.id,
        measurement.scope,
        measurement.purpose,
        raw,
        q,
        e,
        silence,
    )
    .unwrap_or_else(|error| panic!("production true-peak parser failed: {error}"));
    if let (TruePeakValue::Finite(reported), TruePeakValue::Finite(upper)) =
        (parsed.reported, parsed.conservative_upper)
    {
        assert_eq!(upper, reported.checked_add(q).and_then(|v| v.checked_add(e)).unwrap());
    }
    parsed
}

#[derive(Debug)]
struct PlannedChainResult {
    measurements: BTreeMap<MeasurementId, TruePeakMeasurement>,
    terminal_args: Vec<String>,
    terminal_stderr: String,
    package_producer_args: Option<Vec<String>>,
    package_args: Option<Vec<String>>,
}

fn execute_planned_render_only(
    plan: &ConversionPlan,
    sox: &Path,
    ffmpeg: &Path,
) -> Vec<String> {
    let summary = plan.reference.as_ref().expect("Reference summary");
    let command = match plan.steps().first() {
        Some(PlannedExecutionStep::Command(command)) => command,
        _ => panic!("Reference plan does not begin with its render command"),
    };
    assert_eq!(command.output.as_path(), Some(summary.r64_path.as_path()));
    run_planned_command(command, sox, ffmpeg);
    assert!(summary.r64_path.is_file(), "planner render did not create R64");
    command.args.clone()
}

fn execute_planned_terminal_chain(
    plan: &ConversionPlan,
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
    execute_render: bool,
) -> Result<PlannedChainResult, String> {
    let summary = plan.reference.as_ref().expect("Reference summary");
    let mut measurements = BTreeMap::new();
    let mut terminal_args = None;
    let mut terminal_stderr = None;
    let mut package_producer_args = None;
    let mut package_args = None;
    let mut deferred_count = 0_usize;
    for (index, step) in plan.steps().iter().enumerate() {
        match step {
            PlannedExecutionStep::Command(command) if index == 0 => {
                assert_eq!(command.output.as_path(), Some(summary.r64_path.as_path()));
                if execute_render {
                    run_planned_command(command, sox, ffmpeg);
                } else {
                    assert!(
                        summary.r64_path.is_file(),
                        "controlled R64 fixture must exist when render execution is disabled"
                    );
                }
            }
            PlannedExecutionStep::Command(command) => {
                assert_eq!(
                    command.description,
                    "Package terminal PCM without sample changes",
                    "unexpected unplanned command after the Reference render"
                );
                assert!(package_args.is_none(), "more than one package command planned");
                run_planned_command(command, sox, ffmpeg);
                package_args = Some(command.args.clone());
            }
            PlannedExecutionStep::Pipeline(pipeline) => {
                assert!(package_args.is_none(), "more than one package operation planned");
                assert_eq!(
                    pipeline.description,
                    "Package Float64 QPCM through the qualified SoX-to-FFmpeg stream"
                );
                run_planned_command_pipeline(pipeline, sox, ffmpeg);
                package_producer_args = Some(pipeline.producer.args.clone());
                package_args = Some(pipeline.consumer.args.clone());
            }
            PlannedExecutionStep::Measurement(measurement) => {
                let parsed = execute_measurement(measurement, sox, ffmpeg, root);
                if measurement.purpose == TruePeakPurpose::PostFinalAcceptance {
                    if let Err(error) =
                        validate_post_final_true_peak(parsed.conservative_upper, summary.gain_policy)
                    {
                        let pre = measurements.get(&MeasurementId(1));
                        return Err(format!(
                            "{error}; pre_reported={:?}; pre_conservative_upper={:?}; \
                             post_reported={:?}; post_conservative_upper={:?}; \
                             gain_policy={:?}; terminal_args={:?}",
                            pre.map(|value: &tonepoet_pipeline::TruePeakMeasurement| value.reported),
                            pre.map(|value: &tonepoet_pipeline::TruePeakMeasurement| {
                                value.conservative_upper
                            }),
                            parsed.reported,
                            parsed.conservative_upper,
                            summary.gain_policy,
                            terminal_args,
                        ));
                    }
                }
                assert!(measurements.insert(measurement.id, parsed).is_none());
            }
            PlannedExecutionStep::DeferredCommand(deferred) => {
                deferred_count += 1;
                let resolved = resolve_reference_deferred_command(deferred, &measurements)?;
                terminal_args = Some(resolved.args.clone());
                let output = run_planned_command(&resolved, sox, ffmpeg);
                terminal_stderr = Some(combined(&output));
            }
        }
    }
    if deferred_count != 1 {
        return Err(format!(
            "expected exactly one terminal realization, planned {deferred_count}"
        ));
    }
    if measurements.len() != 2 {
        return Err(format!("expected two measurements, got {}", measurements.len()));
    }
    if measurements.get(&MeasurementId(1)).map(|value| value.purpose)
        != Some(TruePeakPurpose::GainAuthority)
        || measurements.get(&MeasurementId(2)).map(|value| value.purpose)
            != Some(TruePeakPurpose::PostFinalAcceptance)
    {
        return Err(
            "Reference plan did not produce the exact pre/post measurement authority".to_string(),
        );
    }
    Ok(PlannedChainResult {
        measurements,
        terminal_args: terminal_args.ok_or_else(|| "terminal command missing".to_string())?,
        terminal_stderr: terminal_stderr
            .ok_or_else(|| "terminal command output missing".to_string())?,
        package_producer_args,
        package_args,
    })
}

fn sox_f64_samples(sox: &Path, input: &Path) -> Vec<f64> {
    let output = run(
        sox,
        &[
            "-D".to_string(),
            input.display().to_string(),
            "-t".to_string(),
            "f64".to_string(),
            "-L".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-".to_string(),
        ],
    );
    assert!(!output.stdout.is_empty(), "SoX produced no decoded samples");
    assert_eq!(
        output.stdout.len() % 8,
        0,
        "SoX produced a truncated f64 stream"
    );
    output
        .stdout
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("f64 sample width")))
        .collect()
}

fn terminal_bound_q63(policy: ResolvedGainPolicy) -> u64 {
    match policy {
        ResolvedGainPolicy::ReferenceCompensated { terminal_bound, .. }
        | ResolvedGainPolicy::NativeLevelExact { terminal_bound, .. }
        | ResolvedGainPolicy::FixedExact { terminal_bound, .. } => {
            terminal_bound.max_added_peak_fs_q63_ceil
        }
        ResolvedGainPolicy::NormalizePeak { .. } => {
            panic!("NormalizePeak has no Reference terminal-error authority")
        }
    }
}

fn assert_terminal_realization_bound(
    sox: &Path,
    summary: &tonepoet_pipeline::DsdReferencePlanSummary,
    terminal_args: &[String],
) -> f64 {
    let gain_db = gain_arg(terminal_args)
        .expect("Reference terminal command has one gain")
        .parse::<f64>()
        .expect("Reference gain token parses as f64");
    let gain = 10_f64.powf(gain_db / 20.0);
    let input = sox_f64_samples(sox, &summary.r64_path);
    let output = sox_f64_samples(sox, &summary.qpcm_path);
    assert_eq!(input.len(), output.len(), "terminal realization changed duration/channels");
    let observed = input
        .iter()
        .zip(&output)
        .map(|(before, after)| (after - before * gain).abs())
        .fold(0.0_f64, f64::max);
    let bound = terminal_bound_q63(summary.gain_policy) as f64 / 9_223_372_036_854_775_808.0_f64;
    assert!(
        observed <= bound,
        "terminal realization error {observed:.18e} exceeded policy bound {bound:.18e} for {:?}",
        summary.final_pcm.bit_depth
    );
    observed
}

fn package_stream_copy_metadata_args(input: &Path, output: &Path, target: &str) -> Vec<String> {
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-i".to_string(),
        input.display().to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-metadata".to_string(),
        "title=Reference qualification".to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
    ];
    match target {
        "wav_riff" => args.extend(["-f".to_string(), "wav".to_string()]),
        "wav_rf64" => args.extend([
            "-f".to_string(),
            "wav".to_string(),
            "-rf64".to_string(),
            "always".to_string(),
        ]),
        "wav_w64" => args.extend(["-f".to_string(), "w64".to_string()]),
        "aiff_native" => args.extend(["-f".to_string(), "aiff".to_string()]),
        "alac_m4a" => args.extend(["-f".to_string(), "ipod".to_string()]),
        "flac_native" | "wavpack_native" => {}
        other => panic!("unknown metadata qualification target {other}"),
    }
    args.push(output.display().to_string());
    args
}

fn qualify_lossless_package_cells() -> (usize, usize) {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let ffprobe = required_sibling_tool(&ffmpeg, "ffprobe");
    let temp = TempDir::new().expect("package qualification tempdir");
    let mut case_count = 0_usize;
    let mut terminal_bound_cells = BTreeSet::new();

    let rates = [
        44_100_u32, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    let depths = [
        (PcmBitDepth::Int24, "int24", "pcm_s24le"),
        (PcmBitDepth::Float32, "float32", "pcm_f32le"),
        (PcmBitDepth::Float64, "float64", "pcm_f64le"),
    ];

    for sample_rate_hz in rates {
        for channels in [1_u16, 2_u16] {
            for (depth, depth_key, pcm_codec) in depths {
                let targets: Vec<(ResolvedOutputTarget, Vec<Option<u8>>)> =
                    if matches!(depth, PcmBitDepth::Float32 | PcmBitDepth::Float64) {
                        vec![
                            (ResolvedOutputTarget::WavRiff, vec![None]),
                            (ResolvedOutputTarget::WavRf64, vec![None]),
                            (ResolvedOutputTarget::WavW64, vec![None]),
                        ]
                    } else {
                        vec![
                            (
                                ResolvedOutputTarget::FlacNative,
                                (0_u8..=8).map(Some).collect(),
                            ),
                            (ResolvedOutputTarget::WavRiff, vec![None]),
                            (ResolvedOutputTarget::WavRf64, vec![None]),
                            (ResolvedOutputTarget::WavW64, vec![None]),
                            (ResolvedOutputTarget::AiffNative, vec![None]),
                            (
                                ResolvedOutputTarget::WavPackNative,
                                (0_u8..=3).map(Some).collect(),
                            ),
                            (ResolvedOutputTarget::AlacM4a, vec![None]),
                        ]
                    };
                for (target, levels) in targets {
                    for level in levels {
                        let suffix = level.map_or_else(|| "default".to_string(), |v| v.to_string());
                        let case_root = temp.path().join(format!(
                            "{sample_rate_hz}-{channels}ch-{depth_key}-{}-{suffix}",
                            target_key(target)
                        ));
                        fs::create_dir_all(&case_root).expect("create package case root");
                        let source = case_root.join("source-placeholder.dsf");
                        let plan = planned_reference_cell(
                            &case_root,
                            &source,
                            2_822_400,
                            sample_rate_hz,
                            channels,
                            depth,
                            target,
                            DsdReconstructionSelection::Reference,
                            DsdSourceGainMode::Reference,
                            None,
                            DbNano::DEFAULT_NORMALIZE_TARGET,
                            level,
                        );
                        let summary = plan.reference.as_ref().expect("Reference summary");
                        synth_r64_fixture(
                            &sox,
                            &summary.r64_path,
                            sample_rate_hz,
                            channels,
                            "0.025",
                            false,
                        );
                        let chain = execute_planned_terminal_chain(
                            &plan,
                            &sox,
                            &ffmpeg,
                            &case_root,
                            false,
                        )
                        .unwrap_or_else(|error| panic!("production chain failed: {error}"));
                        if terminal_bound_cells.insert((sample_rate_hz, channels, depth)) {
                            let observed = assert_terminal_realization_bound(
                                &sox,
                                summary,
                                &chain.terminal_args,
                            );
                            assert!(observed.is_finite());
                        }
                        let packaged = &summary.packaged_path;
                        assert_exact_package_probe(
                            &ffprobe,
                            packaged,
                            target_key(target),
                            depth_key,
                            sample_rate_hz,
                            channels,
                        );
                        let qpcm_hash = if depth == PcmBitDepth::Float64 {
                            sox_streamed_float64_w64_sample_hash(
                                &sox,
                                &ffmpeg,
                                &summary.qpcm_path,
                                sample_rate_hz,
                                channels,
                            )
                        } else {
                            ffmpeg_sample_hash(&ffmpeg, &summary.qpcm_path, pcm_codec)
                        };
                        let packaged_hash = if target == ResolvedOutputTarget::WavW64
                            && depth == PcmBitDepth::Float64
                        {
                            sox_streamed_float64_w64_sample_hash(
                                &sox,
                                &ffmpeg,
                                packaged,
                                sample_rate_hz,
                                channels,
                            )
                        } else {
                            ffmpeg_sample_hash(&ffmpeg, packaged, pcm_codec)
                        };
                        assert_eq!(
                            packaged_hash,
                            qpcm_hash,
                            "decoded samples changed for {}",
                            case_root.display()
                        );
                        let dither_tail: &[&str] = match depth {
                            PcmBitDepth::Int24 => &["dither"],
                            PcmBitDepth::Float32 | PcmBitDepth::Float64 => &[],
                            _ => unreachable!(),
                        };
                        if dither_tail.is_empty() {
                            assert!(!chain.terminal_args.iter().any(|arg| arg == "dither"));
                        } else {
                            assert!(chain
                                .terminal_args
                                .windows(dither_tail.len())
                                .any(|window| window.iter().map(String::as_str).eq(dither_tail.iter().copied())));
                        }
                        assert_eq!(
                            chain.terminal_args.iter().filter(|arg| arg.as_str() == "gain").count(),
                            1
                        );
                        if target == ResolvedOutputTarget::WavW64 {
                            assert!(chain.package_producer_args.is_none());
                            assert!(chain.package_args.is_none());
                            assert_eq!(summary.qpcm_path, summary.packaged_path);
                            assert_eq!(
                                summary.qpcm_path.extension().and_then(|value| value.to_str()),
                                Some("w64")
                            );
                        } else {
                            let args = chain.package_args.as_ref().expect("package command");
                            assert!(!args.iter().any(|arg| matches!(
                                arg.as_str(),
                                "-af" | "-filter:a" | "-sample_fmt"
                            )));
                            if depth == PcmBitDepth::Float64 {
                                let producer = chain
                                    .package_producer_args
                                    .as_ref()
                                    .expect("Float64 package producer");
                                assert!(producer.windows(2).any(|window| {
                                    window[0] == "-t" && window[1] == "raw"
                                }));
                                assert!(producer.windows(2).any(|window| {
                                    window[0] == "-b" && window[1] == "64"
                                }));
                                assert!(producer.iter().any(|arg| arg == "-L"));
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-f" && window[1] == "f64le"
                                }));
                                assert_eq!(
                                    args.iter().filter(|arg| arg.as_str() == "-ar").count(),
                                    1,
                                    "raw Float64 package input must declare rate exactly once"
                                );
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-ar"
                                        && window[1] == sample_rate_hz.to_string()
                                }));
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-ac"
                                        && window[1] == channels.to_string()
                                }));
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-i" && window[1] == "pipe:0"
                                }));
                                assert!(!args.iter().any(|arg| arg == &summary.qpcm_path.display().to_string()));
                            } else {
                                assert!(chain.package_producer_args.is_none());
                                assert!(!args.iter().any(|arg| arg == "-ar"));
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-i"
                                        && window[1] == summary.qpcm_path.display().to_string()
                                }));
                            }
                            if target == ResolvedOutputTarget::WavPackNative
                                && depth == PcmBitDepth::Int24
                            {
                                assert!(args.windows(2).any(|window| {
                                    window[0] == "-bits_per_raw_sample" && window[1] == "24"
                                }));
                            }
                        }
                        assert!(matches!(
                            chain.measurements.get(&MeasurementId(1)).map(|m| m.purpose),
                            Some(TruePeakPurpose::GainAuthority)
                        ));
                        assert!(matches!(
                            chain.measurements.get(&MeasurementId(2)).map(|m| m.purpose),
                            Some(TruePeakPurpose::PostFinalAcceptance)
                        ));

                        let tagged = case_root.join(format!("tagged.{}", target_extension(target)));
                        run(
                            &ffmpeg,
                            &package_stream_copy_metadata_args(packaged, &tagged, target_key(target)),
                        );
                        let tagged_hash = if target == ResolvedOutputTarget::WavW64
                            && depth == PcmBitDepth::Float64
                        {
                            sox_streamed_float64_w64_sample_hash(
                                &sox,
                                &ffmpeg,
                                &tagged,
                                sample_rate_hz,
                                channels,
                            )
                        } else {
                            ffmpeg_sample_hash(&ffmpeg, &tagged, pcm_codec)
                        };
                        assert_eq!(
                            tagged_hash,
                            qpcm_hash,
                            "test-only package stream-copy metadata rewrite changed decoded samples"
                        );
                        let generated = BTreeSet::from([
                            summary.r64_path.clone(),
                            summary.qpcm_path.clone(),
                            summary.packaged_path.clone(),
                            tagged,
                        ]);
                        for path in generated {
                            if path.exists() {
                                fs::remove_file(&path).unwrap_or_else(|error| {
                                    panic!("cannot remove qualification artifact {}: {error}", path.display())
                                });
                            }
                        }
                        case_count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(case_count, 480);
    assert_eq!(terminal_bound_cells.len(), 60);
    (case_count, terminal_bound_cells.len())
}

fn gain_arg(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|window| window[0] == "gain")
        .map(|window| window[1].as_str())
}

fn gain_policy_evidence(policy: ResolvedGainPolicy, terminal_args: &[String]) -> Value {
    let applied_gain_db = gain_arg(terminal_args).map(str::to_owned);
    match policy {
        ResolvedGainPolicy::ReferenceCompensated {
            requested_gain,
            ceiling,
            terminal_bound,
        } => serde_json::json!({
            "mode": "reference_compensated",
            "requested_gain_db": requested_gain.render(true),
            "applied_gain_db": applied_gain_db,
            "acceptance_ceiling_dbtp": ceiling.render(false),
            "terminal_max_added_peak_fs_q63_ceil": terminal_bound.max_added_peak_fs_q63_ceil,
            "terminal_safe_pre_terminal_ceiling_dbtp": terminal_bound.safe_pre_terminal_ceiling_dbtp.render(false),
            "terminal_derivation_digest": terminal_bound.derivation_digest.to_hex(),
            "post_final_acceptance_reserve_db": DbNano::POST_FINAL_ACCEPTANCE_RESERVE.render(false),
        }),
        ResolvedGainPolicy::NativeLevelExact {
            gain,
            ceiling,
            terminal_bound,
        } => serde_json::json!({
            "mode": "native_level_exact",
            "requested_gain_db": gain.render(true),
            "applied_gain_db": applied_gain_db,
            "acceptance_ceiling_dbtp": ceiling.render(false),
            "terminal_max_added_peak_fs_q63_ceil": terminal_bound.max_added_peak_fs_q63_ceil,
            "terminal_safe_pre_terminal_ceiling_dbtp": terminal_bound.safe_pre_terminal_ceiling_dbtp.render(false),
            "terminal_derivation_digest": terminal_bound.derivation_digest.to_hex(),
            "post_final_acceptance_reserve_db": DbNano::POST_FINAL_ACCEPTANCE_RESERVE.render(false),
        }),
        ResolvedGainPolicy::FixedExact {
            gain,
            ceiling,
            terminal_bound,
        } => serde_json::json!({
            "mode": "fixed_exact",
            "requested_gain_db": gain.render(true),
            "applied_gain_db": applied_gain_db,
            "acceptance_ceiling_dbtp": ceiling.render(false),
            "terminal_max_added_peak_fs_q63_ceil": terminal_bound.max_added_peak_fs_q63_ceil,
            "terminal_safe_pre_terminal_ceiling_dbtp": terminal_bound.safe_pre_terminal_ceiling_dbtp.render(false),
            "terminal_derivation_digest": terminal_bound.derivation_digest.to_hex(),
            "post_final_acceptance_reserve_db": DbNano::POST_FINAL_ACCEPTANCE_RESERVE.render(false),
        }),
        ResolvedGainPolicy::NormalizePeak { target_dbfs } => serde_json::json!({
            "mode": "normalize_peak",
            "target_dbfs": target_dbfs.render(false),
            "applied_gain_db": applied_gain_db,
        }),
    }
}

fn qualify_true_peak_analyzer_authority() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("analyzer qualification tempdir");
    const RATES: [u32; 10] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    const CHANNELS: [u16; 2] = [1, 2];
    const NORMALIZED_FREQUENCIES: [f64; 2] = [0.25, 0.45];
    const PHASES: [f64; 2] = [0.0, std::f64::consts::FRAC_PI_4];
    const TRUE_PEAK_LEVELS_DBFS: [f64; 3] = [-120.003, -12.003, -0.500];
    const DURATIONS_SECONDS: [f64; 2] = [0.125, 0.500];
    const PEAK_POSITIONS: [AnalyzerPeakPosition; 2] = [
        AnalyzerPeakPosition::Early,
        AnalyzerPeakPosition::Late,
    ];
    const MULTITONE_FREQUENCIES: [f64; 4] = [0.03125, 0.1171875, 0.2734375, 0.4453125];
    const MULTITONE_PEAK_OFFSETS: [f64; 2] = [0.25, 0.75];
    const SINGLE_TONE_CASE_COUNT: usize = RATES.len()
        * CHANNELS.len()
        * NORMALIZED_FREQUENCIES.len()
        * PHASES.len()
        * TRUE_PEAK_LEVELS_DBFS.len()
        * DURATIONS_SECONDS.len()
        * PEAK_POSITIONS.len();
    const MULTITONE_CASE_COUNT: usize = RATES.len()
        * CHANNELS.len()
        * MULTITONE_PEAK_OFFSETS.len()
        * TRUE_PEAK_LEVELS_DBFS.len()
        * PEAK_POSITIONS.len();
    const REQUIRED_CASE_COUNT: usize = SINGLE_TONE_CASE_COUNT + MULTITONE_CASE_COUNT;

    let mut case_count = 0_usize;
    let mut worst_under_report_db = f64::NEG_INFINITY;
    let mut worst_over_report_db = f64::NEG_INFINITY;
    let mut maximum_intersample_delta_db = f64::NEG_INFINITY;
    let mut near_silence_finite_count = 0_usize;
    let mut cell_summary: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
    let mut evidence_hasher = Sha256::new();
    evidence_hasher.update(b"tonepoet-reference-analyzer-qualification/v4\0");

    for sample_rate_hz in RATES {
        for channels in CHANNELS {
            for normalized_frequency in NORMALIZED_FREQUENCIES {
                for phase_radians in PHASES {
                    for duration_seconds in DURATIONS_SECONDS {
                        for peak_position in PEAK_POSITIONS {
                            let mut prior_reported = None;
                            for true_peak_dbfs in TRUE_PEAK_LEVELS_DBFS {
                                let root = temp.path().join(format!(
                                    "analyzer-{sample_rate_hz}-{channels}ch-{normalized_frequency:.3}-{phase_radians:.6}-{duration_seconds:.3}-{}-{true_peak_dbfs:.3}",
                                    peak_position.key(),
                                ));
                                fs::create_dir_all(&root)
                                    .expect("create analyzer qualification case root");
                                let source = root.join("source-placeholder.dsf");
                                let plan = planned_reference_cell(
                                    &root,
                                    &source,
                                    2_822_400,
                                    sample_rate_hz,
                                    channels,
                                    PcmBitDepth::Float64,
                                    ResolvedOutputTarget::WavW64,
                                    DsdReconstructionSelection::Reference,
                                    DsdSourceGainMode::Reference,
                                    None,
                                    DbNano::DEFAULT_NORMALIZE_TARGET,
                                    None,
                                );
                                let summary = plan.reference.as_ref().expect("Reference summary");
                                let sample_peak_dbfs = write_analytic_analyzer_fixture(
                                    &sox,
                                    &summary.r64_path,
                                    sample_rate_hz,
                                    channels,
                                    true_peak_dbfs,
                                    normalized_frequency,
                                    phase_radians,
                                    duration_seconds,
                                    peak_position,
                                );
                                let measurement = plan
                                    .steps()
                                    .iter()
                                    .find_map(|step| match step {
                                        PlannedExecutionStep::Measurement(measurement)
                                            if measurement.purpose
                                                == TruePeakPurpose::GainAuthority =>
                                        {
                                            Some(measurement)
                                        }
                                        _ => None,
                                    })
                                    .expect("planner emits pre-final true-peak measurement");
                                let parsed = execute_measurement(
                                    measurement,
                                    &sox,
                                    &ffmpeg,
                                    &root,
                                );
                                let TruePeakValue::Finite(reported) = parsed.reported else {
                                    panic!(
                                        "nonzero analytic fixture was misclassified as silence: rate={sample_rate_hz}, channels={channels}, level={true_peak_dbfs}"
                                    );
                                };
                                let TruePeakValue::Finite(upper) = parsed.conservative_upper else {
                                    panic!("finite analytic fixture has a non-finite conservative bound");
                                };
                                let reported_dbfs = reported.0 as f64 / 1_000_000_000.0;
                                let upper_dbfs = upper.0 as f64 / 1_000_000_000.0;
                                let under_report_db = true_peak_dbfs - reported_dbfs;
                                let over_report_db = reported_dbfs - true_peak_dbfs;
                                assert!(
                                    under_report_db <= 0.110_000_001,
                                    "loudnorm under-report {under_report_db:.9} dB exceeded Q+E authority: rate={sample_rate_hz}, channels={channels}, normalized_frequency={normalized_frequency}, phase={phase_radians}, duration={duration_seconds}, position={}, level={true_peak_dbfs}",
                                    peak_position.key(),
                                );
                                assert!(
                                    upper_dbfs + 1e-9 >= true_peak_dbfs,
                                    "conservative true-peak bound {upper_dbfs:.9} dBTP fell below analytic truth {true_peak_dbfs:.9} dBTP"
                                );
                                if let Some(prior) = prior_reported {
                                    assert!(
                                        reported_dbfs > prior,
                                        "true-peak sweep was not monotonic for a fixed analyzer cell"
                                    );
                                }
                                prior_reported = Some(reported_dbfs);
                                if true_peak_dbfs == TRUE_PEAK_LEVELS_DBFS[0] {
                                    near_silence_finite_count += 1;
                                }
                                let intersample_delta_db = reported_dbfs - sample_peak_dbfs;
                                maximum_intersample_delta_db =
                                    maximum_intersample_delta_db.max(intersample_delta_db);
                                worst_under_report_db = worst_under_report_db.max(under_report_db);
                                worst_over_report_db = worst_over_report_db.max(over_report_db);
                                let key = format!("{sample_rate_hz}/{channels}");
                                let entry = cell_summary
                                    .entry(key)
                                    .or_insert((0, f64::NEG_INFINITY, f64::NEG_INFINITY));
                                entry.0 += 1;
                                entry.1 = entry.1.max(under_report_db);
                                entry.2 = entry.2.max(over_report_db);
                                evidence_hasher.update(format!(
                                    "single_tone|{sample_rate_hz}|{channels}|{normalized_frequency:.9}|{phase_radians:.9}|{duration_seconds:.9}|{}|{true_peak_dbfs:.9}|{sample_peak_dbfs:.9}|{reported_dbfs:.9}|{upper_dbfs:.9}\n",
                                    peak_position.key(),
                                ));
                                fs::remove_file(&summary.r64_path)
                                    .expect("remove analyzer qualification carrier");
                                case_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    for sample_rate_hz in RATES {
        for channels in CHANNELS {
            for peak_offset_samples in MULTITONE_PEAK_OFFSETS {
                for peak_position in PEAK_POSITIONS {
                    let mut prior_reported = None;
                    for true_peak_dbfs in TRUE_PEAK_LEVELS_DBFS {
                        let root = temp.path().join(format!(
                            "analyzer-multitone-{sample_rate_hz}-{channels}ch-{peak_offset_samples:.3}-{}-{true_peak_dbfs:.3}",
                            peak_position.key(),
                        ));
                        fs::create_dir_all(&root)
                            .expect("create multitone analyzer qualification case root");
                        let source = root.join("source-placeholder.dsf");
                        let plan = planned_reference_cell(
                            &root,
                            &source,
                            2_822_400,
                            sample_rate_hz,
                            channels,
                            PcmBitDepth::Float64,
                            ResolvedOutputTarget::WavW64,
                            DsdReconstructionSelection::Reference,
                            DsdSourceGainMode::Reference,
                            None,
                            DbNano::DEFAULT_NORMALIZE_TARGET,
                            None,
                        );
                        let summary = plan.reference.as_ref().expect("Reference summary");
                        let sample_peak_dbfs = write_analytic_multitone_fixture(
                            &sox,
                            &summary.r64_path,
                            sample_rate_hz,
                            channels,
                            true_peak_dbfs,
                            peak_offset_samples,
                            peak_position,
                        );
                        let measurement = plan
                            .steps()
                            .iter()
                            .find_map(|step| match step {
                                PlannedExecutionStep::Measurement(measurement)
                                    if measurement.purpose == TruePeakPurpose::GainAuthority =>
                                {
                                    Some(measurement)
                                }
                                _ => None,
                            })
                            .expect("planner emits pre-final true-peak measurement");
                        let parsed = execute_measurement(measurement, &sox, &ffmpeg, &root);
                        let TruePeakValue::Finite(reported) = parsed.reported else {
                            panic!("nonzero analytic multitone fixture was misclassified as silence");
                        };
                        let TruePeakValue::Finite(upper) = parsed.conservative_upper else {
                            panic!("finite analytic multitone fixture has a non-finite conservative bound");
                        };
                        let reported_dbfs = reported.0 as f64 / 1_000_000_000.0;
                        let upper_dbfs = upper.0 as f64 / 1_000_000_000.0;
                        let under_report_db = true_peak_dbfs - reported_dbfs;
                        let over_report_db = reported_dbfs - true_peak_dbfs;
                        assert!(
                            under_report_db <= 0.110_000_001,
                            "multitone loudnorm under-report {under_report_db:.9} dB exceeded Q+E authority"
                        );
                        assert!(
                            upper_dbfs + 1e-9 >= true_peak_dbfs,
                            "multitone conservative true-peak bound fell below analytic truth"
                        );
                        if let Some(prior) = prior_reported {
                            assert!(
                                reported_dbfs > prior,
                                "multitone true-peak sweep was not monotonic for a fixed analyzer cell"
                            );
                        }
                        prior_reported = Some(reported_dbfs);
                        if true_peak_dbfs == TRUE_PEAK_LEVELS_DBFS[0] {
                            near_silence_finite_count += 1;
                        }
                        let intersample_delta_db = reported_dbfs - sample_peak_dbfs;
                        maximum_intersample_delta_db =
                            maximum_intersample_delta_db.max(intersample_delta_db);
                        worst_under_report_db = worst_under_report_db.max(under_report_db);
                        worst_over_report_db = worst_over_report_db.max(over_report_db);
                        let key = format!("{sample_rate_hz}/{channels}");
                        let entry = cell_summary
                            .entry(key)
                            .or_insert((0, f64::NEG_INFINITY, f64::NEG_INFINITY));
                        entry.0 += 1;
                        entry.1 = entry.1.max(under_report_db);
                        entry.2 = entry.2.max(over_report_db);
                        evidence_hasher.update(format!(
                            "phase_aligned_multitone|{sample_rate_hz}|{channels}|{peak_offset_samples:.9}|{}|{true_peak_dbfs:.9}|{sample_peak_dbfs:.9}|{reported_dbfs:.9}|{upper_dbfs:.9}\n",
                            peak_position.key(),
                        ));
                        fs::remove_file(&summary.r64_path)
                            .expect("remove multitone analyzer qualification carrier");
                        case_count += 1;
                    }
                }
            }
        }
    }

    assert_eq!(case_count, REQUIRED_CASE_COUNT);
    assert_eq!(near_silence_finite_count, REQUIRED_CASE_COUNT / TRUE_PEAK_LEVELS_DBFS.len());
    assert!(
        maximum_intersample_delta_db > 2.8,
        "analyzer corpus did not exercise a known material inter-sample peak"
    );
    let per_rate_channel = cell_summary
        .into_iter()
        .map(|(cell, (cases, under, over))| {
            serde_json::json!({
                "cell": cell,
                "case_count": cases,
                "worst_under_report_db": under,
                "worst_over_report_db": over,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "status": "passed",
        "method": "analytic single-tone and phase-aligned multitone bursts with raised-cosine boundaries; planner-emitted loudnorm command and production parser/conservative arithmetic",
        "waveform_families": ["single_tone", "phase_aligned_multitone"],
        "single_tone_case_count": SINGLE_TONE_CASE_COUNT,
        "phase_aligned_multitone_case_count": MULTITONE_CASE_COUNT,
        "case_count": case_count,
        "required_case_count": REQUIRED_CASE_COUNT,
        "rates_hz": RATES,
        "channels": CHANNELS,
        "normalized_frequencies_cycles_per_sample": NORMALIZED_FREQUENCIES,
        "phases_radians": PHASES,
        "analytic_true_peak_levels_dbfs": TRUE_PEAK_LEVELS_DBFS,
        "durations_seconds": DURATIONS_SECONDS,
        "peak_positions": ["early", "late"],
        "aligned_multitone_normalized_frequencies_cycles_per_sample": MULTITONE_FREQUENCIES,
        "aligned_multitone_peak_offsets_samples": MULTITONE_PEAK_OFFSETS,
        "aligned_multitone_duration_seconds": 0.250_f64,
        "worst_under_report_db": worst_under_report_db,
        "worst_over_report_db": worst_over_report_db,
        "maximum_intersample_delta_db": maximum_intersample_delta_db,
        "one_sided_authority_db": 0.110000000_f64,
        "monotonic_per_cell": true,
        "nonzero_near_silence_remained_finite": true,
        "per_rate_channel": per_rate_channel,
        "evidence_digest": format!("{:x}", evidence_hasher.finalize()),
    })
}

fn qualify_production_measurement_gain_terminal_chain() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("gain qualification tempdir");
    let mut results = serde_json::Map::new();

    let end_to_end_root = temp.path().join("planner_render_end_to_end");
    fs::create_dir_all(&end_to_end_root).expect("create end-to-end case root");
    let end_to_end_source = end_to_end_root.join("source.dsf");
    write_dsf_reference_fixture(&end_to_end_source, 2, 2_822_400);
    let end_to_end_plan = planned_reference_cell(
        &end_to_end_root,
        &end_to_end_source,
        2_822_400,
        176_400,
        2,
        PcmBitDepth::Int24,
        ResolvedOutputTarget::FlacNative,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        Some(5),
    );
    let end_to_end_summary = end_to_end_plan.reference.as_ref().expect("Reference summary");
    let end_to_end = execute_planned_terminal_chain(
        &end_to_end_plan,
        &sox,
        &ffmpeg,
        &end_to_end_root,
        true,
    )
    .unwrap_or_else(|error| panic!("planner render end-to-end chain failed: {error}"));
    assert!(end_to_end_summary.r64_path.is_file());
    assert!(end_to_end_summary.qpcm_path.is_file());
    assert!(end_to_end_summary.packaged_path.is_file());
    assert!(end_to_end.package_args.is_some());
    assert_eq!(
        ffmpeg_sample_hash(&ffmpeg, &end_to_end_summary.packaged_path, "pcm_s24le"),
        ffmpeg_sample_hash(&ffmpeg, &end_to_end_summary.qpcm_path, "pcm_s24le"),
        "planner render end-to-end package changed terminal samples",
    );
    results.insert(
        "planner_render_end_to_end".to_string(),
        serde_json::json!({
            "status": "passed",
            "steps_executed": end_to_end_plan.steps().len(),
            "terminal_realization_count": 1,
            "measurement_count": end_to_end.measurements.len(),
            "render_args": match &end_to_end_plan.steps()[0] {
                PlannedExecutionStep::Command(command) => command.args.clone(),
                _ => panic!("first planned step is not render"),
            },
            "terminal_args": end_to_end.terminal_args,
            "package_args": end_to_end.package_args,
        }),
    );

    let success_cases = [
        (
            "reference_constrained",
            DsdSourceGainMode::Reference,
            None,
            "0.500",
            false,
        ),
        (
            "native_exact",
            DsdSourceGainMode::NativeLevel,
            None,
            "0.020",
            false,
        ),
        (
            "fixed_exact",
            DsdSourceGainMode::Fixed,
            Some(DbNano(3_000_000_000)),
            "0.020",
            false,
        ),
        (
            "normalize",
            DsdSourceGainMode::NormalizePeak,
            None,
            "0.500",
            false,
        ),
        (
            "verified_silence",
            DsdSourceGainMode::Reference,
            None,
            "0.000",
            true,
        ),
    ];

    for (name, mode, fixed, amplitude, silence) in success_cases {
        let root = temp.path().join(name);
        fs::create_dir_all(&root).expect("create gain case root");
        let source = root.join("source-placeholder.dsf");
        let plan = planned_reference_cell(
            &root,
            &source,
            2_822_400,
            176_400,
            2,
            PcmBitDepth::Int24,
            ResolvedOutputTarget::WavW64,
            DsdReconstructionSelection::Reference,
            mode,
            fixed,
            DbNano::DEFAULT_NORMALIZE_TARGET,
            None,
        );
        let summary = plan.reference.as_ref().expect("Reference summary");
        synth_r64_fixture(
            &sox,
            &summary.r64_path,
            176_400,
            2,
            amplitude,
            silence,
        );
        let chain = execute_planned_terminal_chain(&plan, &sox, &ffmpeg, &root, false)
            .unwrap_or_else(|error| panic!("{name} production chain failed: {error}"));
        let pre = chain
            .measurements
            .get(&MeasurementId(1))
            .expect("pre-final measurement");
        let post = chain
            .measurements
            .get(&MeasurementId(2))
            .expect("post-final measurement");
        match name {
            "reference_constrained" => {
                let applied: DbNano = gain_arg(&chain.terminal_args)
                    .expect("Reference gain token")
                    .parse()
                    .expect("Reference gain is canonical");
                assert!(applied < DbNano(18_020_599_913));
                assert!(applied > DbNano::ZERO);
            }
            "native_exact" => {
                assert_eq!(gain_arg(&chain.terminal_args), Some("+12.000000000"));
            }
            "fixed_exact" => {
                assert_eq!(gain_arg(&chain.terminal_args), Some("+15.000000000"));
            }
            "normalize" => {
                assert!(chain.terminal_args.windows(2).any(|window| {
                    window[0] == "norm" && window[1] == "-0.150000000"
                }));
                assert!(gain_arg(&chain.terminal_args).is_none());
            }
            "verified_silence" => {
                assert_eq!(pre.reported, TruePeakValue::VerifiedSilence);
                assert!(
                    matches!(post.reported, TruePeakValue::Finite(_)),
                    "integer terminal dither must make post-final silence finite"
                );
                assert_eq!(gain_arg(&chain.terminal_args), Some("+18.020599913"));
            }
            _ => unreachable!(),
        }
        validate_post_final_true_peak(post.conservative_upper, summary.gain_policy)
            .expect("post-final acceptance");
        results.insert(
            name.to_string(),
            serde_json::json!({
                "pre_reported": format!("{:?}", pre.reported),
                "pre_conservative_upper": format!("{:?}", pre.conservative_upper),
                "post_reported": format!("{:?}", post.reported),
                "post_conservative_upper": format!("{:?}", post.conservative_upper),
                "gain_policy": gain_policy_evidence(summary.gain_policy, &chain.terminal_args),
                "terminal_args": chain.terminal_args,
                "terminal_realization_count": 1,
            }),
        );
    }

    let root = temp.path().join("int24_tpdf");
    fs::create_dir_all(&root).expect("create Int24 dither case root");
    let source = root.join("source-placeholder.dsf");
    let plan = planned_reference_cell(
        &root,
        &source,
        2_822_400,
        44_100,
        1,
        PcmBitDepth::Int24,
        ResolvedOutputTarget::WavW64,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let summary = plan.reference.as_ref().expect("Reference summary");
    synth_r64_fixture(&sox, &summary.r64_path, 44_100, 1, "0.000", true);
    let chain = execute_planned_terminal_chain(&plan, &sox, &ffmpeg, &root, false)
        .unwrap_or_else(|error| panic!("Int24 TPDF production chain failed: {error}"));
    let samples = sox_f64_samples(&sox, &summary.qpcm_path);
    let max_abs = samples.iter().copied().map(f64::abs).fold(0.0_f64, f64::max);
    let bound = terminal_bound_q63(summary.gain_policy) as f64
        / 9_223_372_036_854_775_808.0_f64;
    assert!(
        max_abs <= bound,
        "Int24 TPDF silence peak {max_abs:.18e} exceeded policy bound {bound:.18e}"
    );
    assert_eq!(chain.terminal_args.last().map(String::as_str), Some("dither"));
    results.insert(
        "dither_semantics".to_string(),
        serde_json::json!({
            "int16_shibata": "unavailable:DSD-REF-P0-022",
            "int24_tpdf": {
                "observed_silence_peak_fs": max_abs,
                "policy_peak_bound_fs": bound,
                "terminal_args": chain.terminal_args
            }
        }),
    );

    for (name, mode, fixed) in [
        ("native_unsafe_refusal", DsdSourceGainMode::NativeLevel, None),
        (
            "fixed_unsafe_refusal",
            DsdSourceGainMode::Fixed,
            Some(DbNano::ZERO),
        ),
    ] {
        let root = temp.path().join(name);
        fs::create_dir_all(&root).expect("create refusal case root");
        let source = root.join("source-placeholder.dsf");
        let plan = planned_reference_cell(
            &root,
            &source,
            2_822_400,
            176_400,
            2,
            PcmBitDepth::Int24,
            ResolvedOutputTarget::WavW64,
            DsdReconstructionSelection::Reference,
            mode,
            fixed,
            DbNano::DEFAULT_NORMALIZE_TARGET,
            None,
        );
        let summary = plan.reference.as_ref().expect("Reference summary");
        synth_r64_fixture(
            &sox,
            &summary.r64_path,
            176_400,
            2,
            "0.950",
            false,
        );
        let error = execute_planned_terminal_chain(&plan, &sox, &ffmpeg, &root, false)
            .expect_err("unsafe exact gain must fail before terminal realization");
        assert!(error.contains("DSD-REF-P0-016"), "unexpected refusal: {error}");
        assert!(!summary.qpcm_path.exists());
        results.insert(
            name.to_string(),
            serde_json::json!({
                "status": "refused_before_terminal_realization",
                "error": error,
            }),
        );
    }

    let (q, e) = policy_measurement_bounds();
    let parser_probe = parse_reference_true_peak_measurement(
        MeasurementId(99),
        tonepoet_pipeline::MeasurementScope::Plan,
        TruePeakPurpose::GainAuthority,
        r#"{
            "input_i":"-23.00","input_tp":"-3.000000000","input_lra":"0.10",
            "input_thresh":"-33.00","output_i":"-23.00","output_tp":"9.000000000",
            "output_lra":"0.10","output_thresh":"-33.00","normalization_type":"linear",
            "target_offset":"0.00"
        }"#
        .to_string(),
        q,
        e,
        false,
    )
    .expect("strict parser probe");
    assert_eq!(parser_probe.reported, TruePeakValue::Finite(DbNano(-3_000_000_000)));
    assert_eq!(
        parser_probe.conservative_upper,
        TruePeakValue::Finite(DbNano(-2_890_000_000))
    );
    results.insert(
        "strict_parser_input_tp_and_q_plus_e".to_string(),
        serde_json::json!({
            "reported": "-3.000000000",
            "output_tp_ignored": "9.000000000",
            "reporting_uncertainty": q.to_string(),
            "analyzer_residual": e.to_string(),
            "conservative_upper": "-2.890000000",
        }),
    );

    Value::Object(results)
}

fn qualify_production_source_front_end_integration() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let ffprobe = required_sibling_tool(&ffmpeg, "ffprobe");
    const PREDICTIVE_DST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/frame_001.dst.bin"
    ));
    const PREDICTIVE_DSD: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/frame_001.dsd.bin"
    ));

    let temp = TempDir::new().expect("source-front-end qualification tempdir");
    let mut native_rows = Vec::new();
    for (format_key, format, source_kind) in [
        ("dsf_uncompressed", AudioFormat::Dsf, DsdSourceKind::DsfUncompressed),
        (
            "dsdiff_uncompressed",
            AudioFormat::Dff,
            DsdSourceKind::DsdiffUncompressed,
        ),
    ] {
        for source_rate_hz in [2_822_400_u32, 5_644_800, 11_289_600] {
            for channels in [1_u16, 2_u16] {
                let root = temp.path().join(format!(
                    "{format_key}-{source_rate_hz}-{channels}ch"
                ));
                fs::create_dir_all(&root).expect("create native source case root");
                let source = root.join(if format == AudioFormat::Dsf {
                    "source.dsf"
                } else {
                    "source.dff"
                });
                if format == AudioFormat::Dsf {
                    write_dsf_reference_fixture(&source, channels, source_rate_hz);
                } else {
                    write_dff_reference_fixture(&source, channels, source_rate_hz);
                }
                let work = root.join("materialized");
                let materialized = qualify_reference_source_materialization(
                    &source_kind,
                    &source,
                    &work,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "production native materialization failed for {format_key}/{source_rate_hz}/{channels}: {error}"
                    )
                });
                assert_not_hard_linked(&source, &materialized.materialized_path);
                assert_eq!(
                    materialized.source_content_sha256,
                    materialized.canonical_materialization_sha256,
                    "native source materialization must be byte-identical"
                );
                let plan = planned_reference_source_cell(
                    &root,
                    &materialized.materialized_path,
                    source_rate_hz,
                    if source_rate_hz == 2_822_400 { 88_200 } else { 176_400 },
                    channels,
                    format.clone(),
                    source_kind.clone(),
                    PcmBitDepth::Int24,
                    ResolvedOutputTarget::WavW64,
                    DsdReconstructionSelection::Reference,
                    DsdSourceGainMode::Reference,
                    None,
                    DbNano::DEFAULT_NORMALIZE_TARGET,
                    None,
                );
                assert!(matches!(
                    plan.reference.as_ref().map(|summary| &summary.front_end),
                    Some(&DsdInputFrontEnd::NativeUncompressed)
                ));
                let summary = plan.reference.as_ref().expect("Reference summary");
                let render_args = execute_planned_render_only(&plan, &sox, &ffmpeg);
                assert_exact_package_probe(
                    &ffprobe,
                    &summary.r64_path,
                    "wav_w64",
                    "float64",
                    summary.final_pcm.sample_rate_hz,
                    summary.final_pcm.channels,
                );
                fs::remove_file(&summary.r64_path).expect("remove native source render carrier");
                native_rows.push(serde_json::json!({
                    "source_kind": format_key,
                    "source_rate_hz": source_rate_hz,
                    "channels": channels,
                    "source_sha256": materialized.source_content_sha256.to_string(),
                    "materialized_sha256": materialized.canonical_materialization_sha256.to_string(),
                    "materialization_identity_digest": materialized.materialization_identity_digest.to_string(),
                    "hard_link": false,
                    "planner_render": "passed",
                    "render_args_sha256": sha256_hex(render_args.join("\0").as_bytes()),
                }));
            }
        }
    }

    let dst_root = temp.path().join("dsdiff-dst-dsd64-stereo");
    fs::create_dir_all(&dst_root).expect("create DSDIFF/DST source case root");
    let dst_source = dst_root.join("source.dff");
    {
        let file = File::create(&dst_source).expect("create DSDIFF/DST fixture");
        let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(
            file,
            2,
            2_822_400,
        )
        .expect("create DSDIFF/DST writer");
        writer
            .write_encoded_frame(PREDICTIVE_DST, PREDICTIVE_DSD)
            .expect("write independent-oracle predictive DST frame");
        writer.finish().expect("finish DSDIFF/DST fixture");
    }
    let dst_materialized = qualify_reference_source_materialization(
        &DsdSourceKind::DsdiffDst,
        &dst_source,
        &dst_root.join("materialized"),
    )
    .expect("production DSDIFF/DST materialization");
    assert_not_hard_linked(&dst_source, &dst_materialized.materialized_path);
    assert_ne!(
        dst_materialized.source_content_sha256,
        dst_materialized.canonical_materialization_sha256,
        "DST materialization must bind both compressed source and canonical DSD identities"
    );
    assert_eq!(collect_decoded_dsd(&dst_materialized.materialized_path), PREDICTIVE_DSD);
    let tampered_binding = qualify_reference_materialization_identity_digest(
        &DsdSourceKind::DsdiffDst,
        dst_materialized.source_content_sha256,
        tonepoet_pipeline::Sha256Digest::of_bytes(b"deliberately different canonical materialization"),
    );
    assert_ne!(
        dst_materialized.materialization_identity_digest,
        tampered_binding,
        "canonical materialization drift must invalidate executed-evidence identity",
    );
    let dst_plan = planned_reference_source_cell(
        &dst_root,
        &dst_materialized.materialized_path,
        2_822_400,
        88_200,
        2,
        AudioFormat::Dff,
        DsdSourceKind::DsdiffDst,
        PcmBitDepth::Int24,
        ResolvedOutputTarget::WavW64,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    assert!(matches!(
        dst_plan.reference.as_ref().map(|summary| &summary.front_end),
        Some(&DsdInputFrontEnd::DsdiffDst { .. })
    ));
    let dst_summary = dst_plan.reference.as_ref().expect("Reference summary");
    let dst_render_args = execute_planned_render_only(&dst_plan, &sox, &ffmpeg);
    assert_exact_package_probe(
        &ffprobe,
        &dst_summary.r64_path,
        "wav_w64",
        "float64",
        dst_summary.final_pcm.sample_rate_hz,
        dst_summary.final_pcm.channels,
    );
    fs::remove_file(&dst_summary.r64_path).expect("remove DST source render carrier");

    let wrong_classification = temp.path().join("wrong-classification");
    fs::create_dir_all(&wrong_classification).expect("create wrong-classification root");
    let plain_dff = wrong_classification.join("plain.dff");
    write_dff_reference_fixture(&plain_dff, 2, 2_822_400);
    let classification_error = qualify_reference_source_materialization(
        &DsdSourceKind::DsdiffDst,
        &plain_dff,
        &wrong_classification.join("work"),
    )
    .expect_err("CMPR mismatch must fail before decode");
    assert!(classification_error
        .to_string()
        .contains("classification mismatch"));

    let corrupt_root = temp.path().join("dstc-corrupt");
    fs::create_dir_all(&corrupt_root).expect("create corrupt DSTC root");
    let corrupt = corrupt_root.join("corrupt.dff");
    let mut corrupt_bytes = fs::read(&dst_source).expect("read DSDIFF/DST fixture");
    let dstc = corrupt_bytes
        .windows(4)
        .position(|window| window == b"DSTC")
        .expect("DSDIFF/DST fixture contains DSTC");
    corrupt_bytes[dstc + 15] ^= 0x01;
    fs::write(&corrupt, corrupt_bytes).expect("write corrupt DSTC fixture");
    let dstc_error = qualify_reference_source_materialization(
        &DsdSourceKind::DsdiffDst,
        &corrupt,
        &corrupt_root.join("work"),
    )
    .expect_err("production materializer must reject a DSTC mismatch");
    assert!(dstc_error.to_string().contains("DSTC mismatch"));

    serde_json::json!({
        "status": "passed",
        "production_seam": "tonepoet::convert::pipeline::qualify_reference_source_materialization",
        "native_cases": native_rows,
        "native_case_count": native_rows.len(),
        "dsdiff_dst": {
            "source_rate_hz": 2822400,
            "channels": 2,
            "source_sha256": dst_materialized.source_content_sha256.to_string(),
            "canonical_materialization_sha256": dst_materialized.canonical_materialization_sha256.to_string(),
            "materialization_identity_digest": dst_materialized.materialization_identity_digest.to_string(),
            "materialization_identity_tamper_rejected": true,
            "decoded_oracle_sha256": sha256_hex(PREDICTIVE_DSD),
            "cmpr_classification": "passed",
            "dstc_verification": "passed",
            "canonical_dff_readback": "passed",
            "planner_render": "passed",
            "render_args_sha256": sha256_hex(dst_render_args.join("\0").as_bytes()),
            "executed_evidence_binding_schema": "tonepoet-reference-executed-evidence/v2"
        },
        "sacd_dsd": "unavailable:DSD-REF-P0-023",
        "sacd_dst": "unavailable:DSD-REF-P0-023"
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DstQualificationCounts {
    total: usize,
    predictive_independent_oracle: usize,
    predictive_stereo_reference: usize,
    predictive_six_channel_decoder_only: usize,
    standards_literal_geometry: usize,
}

fn qualify_dst_oracle_fixture_authority() -> DstQualificationCounts {
    use sacd_rs::dst::DstRate;

    const CHECKSUMS: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/P0_SHA256SUMS"
    ));
    const PROVENANCE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/P0_PROVENANCE.json"
    ));
    const GENERATOR: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/generate_p0_raw_fixtures.py"
    ));
    const RAW_ORACLE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py"
    ));
    const CASES: [(&str, &[u8], &str, &[u8], DstRate, u8, bool); 12] = [
        (
            "frame_001.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_001.dst.bin")),
            "frame_001.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_001.dsd.bin")),
            DstRate::Dsd64,
            2,
            true,
        ),
        (
            "frame_001_6ch.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_001_6ch.dst.bin")),
            "frame_001_6ch.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_001_6ch.dsd.bin")),
            DstRate::Dsd64,
            6,
            true,
        ),
        (
            "frame_002.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_002.dst.bin")),
            "frame_002.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_002.dsd.bin")),
            DstRate::Dsd64,
            2,
            true,
        ),
        (
            "frame_002_6ch.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_002_6ch.dst.bin")),
            "frame_002_6ch.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_002_6ch.dsd.bin")),
            DstRate::Dsd64,
            6,
            true,
        ),
        (
            "frame_003.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_003.dst.bin")),
            "frame_003.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_003.dsd.bin")),
            DstRate::Dsd64,
            2,
            true,
        ),
        (
            "frame_003_6ch.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_003_6ch.dst.bin")),
            "frame_003_6ch.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/frame_003_6ch.dsd.bin")),
            DstRate::Dsd64,
            6,
            true,
        ),
        (
            "raw_dsd64_mono.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd64_mono.dst.bin")),
            "raw_dsd64_mono.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd64_mono.dsd.bin")),
            DstRate::Dsd64,
            1,
            false,
        ),
        (
            "raw_dsd64_6ch.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd64_6ch.dst.bin")),
            "raw_dsd64_6ch.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd64_6ch.dsd.bin")),
            DstRate::Dsd64,
            6,
            false,
        ),
        (
            "raw_dsd128_mono.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd128_mono.dst.bin")),
            "raw_dsd128_mono.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd128_mono.dsd.bin")),
            DstRate::Dsd128,
            1,
            false,
        ),
        (
            "raw_dsd128_stereo.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd128_stereo.dst.bin")),
            "raw_dsd128_stereo.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd128_stereo.dsd.bin")),
            DstRate::Dsd128,
            2,
            false,
        ),
        (
            "raw_dsd256_mono.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd256_mono.dst.bin")),
            "raw_dsd256_mono.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd256_mono.dsd.bin")),
            DstRate::Dsd256,
            1,
            false,
        ),
        (
            "raw_dsd256_stereo.dst.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd256_stereo.dst.bin")),
            "raw_dsd256_stereo.dsd.bin",
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/sacd-rs/src/dst/fixtures/raw_dsd256_stereo.dsd.bin")),
            DstRate::Dsd256,
            2,
            false,
        ),
    ];

    let checksum_text = std::str::from_utf8(CHECKSUMS).expect("P0 checksums are UTF-8");
    let checksums: std::collections::BTreeMap<_, _> = checksum_text
        .lines()
        .map(|line| {
            let (digest, name) = line
                .split_once("  ")
                .expect("P0 checksum line uses two-space separator");
            (name, digest)
        })
        .collect();
    assert_eq!(checksums.len(), CASES.len() * 2);

    let provenance: Value = serde_json::from_slice(PROVENANCE).expect("P0 provenance parses");
    assert_eq!(provenance["schema_version"], 2);
    assert_eq!(
        provenance["oracle"]["oracle_output_set_identity"],
        "sha256:1375f3c65a04a81f59cf944b3be9b9e4565f9d43cccf3356af570358c237c2fe"
    );
    assert_eq!(
        provenance["authority"],
        "commission_attestation_plus_content_addressed_independent_oracles_and_standards_literal_outputs"
    );
    assert_eq!(
        provenance["cases"].as_array().map(Vec::len),
        Some(CASES.len())
    );
    assert_eq!(
        provenance["standards_literal_authority"]["generator_sha256"],
        sha256_hex(GENERATOR)
    );
    assert_eq!(
        provenance["standards_literal_authority"]["independent_oracle_sha256"],
        sha256_hex(RAW_ORACLE)
    );

    let mut corpus_files: Vec<(&str, &[u8])> = vec![
        ("P0_PROVENANCE.json", PROVENANCE),
        ("P0_SHA256SUMS", CHECKSUMS),
        ("generate_p0_raw_fixtures.py", GENERATOR),
        ("verify_p0_raw_oracle.py", RAW_ORACLE),
    ];
    let mut saw_predictive = false;
    let mut saw_raw = false;
    let mut saw_six_channel = false;
    let mut predictive_independent_oracle = 0_usize;
    let mut predictive_stereo_reference = 0_usize;
    let mut predictive_six_channel_decoder_only = 0_usize;
    let mut standards_literal_geometry = 0_usize;
    let mut rate_coverage = [false; 3];
    let mut predictive_rate_coverage = [false; 3];
    let mut channel_coverage = [false; 7];

    for (encoded_name, encoded, decoded_name, expected, rate, channels, predictive) in CASES {
        let encoded_digest = sha256_hex(encoded);
        let decoded_digest = sha256_hex(expected);
        assert_eq!(
            checksums.get(encoded_name).copied(),
            Some(encoded_digest.as_str())
        );
        assert_eq!(
            checksums.get(decoded_name).copied(),
            Some(decoded_digest.as_str())
        );
        let actual = sacd_rs::dst::decode_frame_with_rate(encoded, channels, rate)
            .expect("qualified DST fixture must decode");
        assert_eq!(actual.as_slice(), expected);

        let raw = sacd_rs::dst::encode_uncompressed_frame_interleaved_with_rate(
            expected,
            channels,
            rate,
        )
        .expect("qualified decoded frame must encode as explicit raw DST");
        let roundtrip = sacd_rs::dst::decode_frame_with_rate(&raw, channels, rate)
            .expect("explicit raw DST frame must decode");
        assert_eq!(roundtrip.as_slice(), expected);

        saw_predictive |= predictive;
        saw_raw |= !predictive;
        saw_six_channel |= channels == 6;
        if predictive {
            predictive_independent_oracle += 1;
            if channels == 2 {
                predictive_stereo_reference += 1;
            } else if channels == 6 {
                predictive_six_channel_decoder_only += 1;
            }
        } else {
            standards_literal_geometry += 1;
        }
        let rate_index = match rate {
            DstRate::Dsd64 => 0,
            DstRate::Dsd128 => 1,
            DstRate::Dsd256 => 2,
        };
        rate_coverage[rate_index] = true;
        if predictive {
            predictive_rate_coverage[rate_index] = true;
        }
        channel_coverage[usize::from(channels)] = true;
        corpus_files.push((encoded_name, encoded));
        corpus_files.push((decoded_name, expected));
    }
    assert!(saw_predictive && saw_raw && saw_six_channel);
    assert_eq!(rate_coverage, [true, true, true]);
    assert_eq!(
        predictive_rate_coverage,
        [true, false, false],
        "only DSD64 has independent-oracle predictive compressed-DST evidence"
    );
    assert!(channel_coverage[1] && channel_coverage[2] && channel_coverage[6]);
    assert_eq!(channel_coverage.iter().filter(|covered| **covered).count(), 3);

    let corpus_digest = canonical_fixture_corpus_digest(&corpus_files);
    assert_eq!(
        sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID,
        format!("sha256:{corpus_digest}")
    );
    assert_eq!(
        sacd_rs::DST_REFERENCE_FIXTURE_MANIFEST_ID,
        format!("sha256:{}", sha256_hex(CHECKSUMS))
    );
    assert_eq!(
        sacd_rs::DST_REFERENCE_FIXTURE_PROVENANCE_ID,
        format!("sha256:{}", sha256_hex(PROVENANCE))
    );

    let commission = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/brief_dsd_reference_p0_scope_and_commission.md"
    ));
    assert_eq!(
        provenance["attestation"]["document_sha256"],
        sha256_hex(commission)
    );

    let mut settings = PipelineSettings::default();
    settings.target_format = AudioFormat::Flac;
    settings.target_sample_rate = RateTarget::PcmHz(88_200);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
    let request = PlanRequest {
        input_path: PathBuf::from("qualified-six-channel.dff"),
        output_path: PathBuf::from("rejected.flac"),
        source: SourceInfo {
            format: AudioFormat::Dff,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(6),
            duration: Some(std::time::Duration::from_secs(1)),
            dsd_source_kind: Some(DsdSourceKind::DsdiffDst),
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(PathBuf::from("work")),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: Some(ResolvedOutputTarget::FlacNative),
        reference_programme_scope: ReferenceProgrammeScope::Singleton,
        planned_riff_non_audio_upper_bound_bytes: None,
    };
    let error = plan_reference_dsd(&request).expect_err("six-channel Reference must reject");
    assert_eq!(
        error.to_string(),
        format!(
            "invalid settings for source.channels: {}",
            tonepoet_pipeline::reference_error_text(ReferenceErrorCode::UnsupportedChannels)
        )
    );

    for (source_rate_hz, channels) in [
        (2_822_400_u32, 1_u16),
        (5_644_800_u32, 1_u16),
        (5_644_800_u32, 2_u16),
        (11_289_600_u32, 1_u16),
        (11_289_600_u32, 2_u16),
    ] {
        let mut unsupported_request = request.clone();
        unsupported_request.source.channels = Some(channels);
        unsupported_request.source.sample_rate_hz = Some(source_rate_hz);
        let error = plan_reference_dsd(&unsupported_request)
            .expect_err("predictive compressed DST outside DSD64 stereo must reject before decode");
        assert_eq!(
            error.to_string(),
            format!(
                "invalid settings for source.dsd_source_kind: {}",
                tonepoet_pipeline::reference_error_text(
                    ReferenceErrorCode::CompressedDstRateUnqualified
                )
            )
        );
    }

    let mut supported_request = request;
    supported_request.source.channels = Some(2);
    supported_request.source.sample_rate_hz = Some(2_822_400);
    plan_reference_dsd(&supported_request)
        .expect("predictive compressed DSD64 stereo has independent-oracle authority");

    let counts = DstQualificationCounts {
        total: CASES.len(),
        predictive_independent_oracle,
        predictive_stereo_reference,
        predictive_six_channel_decoder_only,
        standards_literal_geometry,
    };
    assert_eq!(
        counts,
        DstQualificationCounts {
            total: 12,
            predictive_independent_oracle: 6,
            predictive_stereo_reference: 3,
            predictive_six_channel_decoder_only: 3,
            standards_literal_geometry: 6,
        }
    );
    counts
}

#[test]
fn p0_dst_oracle_fixture_authority_is_complete_and_byte_exact() {
    assert_eq!(
        qualify_dst_oracle_fixture_authority(),
        DstQualificationCounts {
            total: 12,
            predictive_independent_oracle: 6,
            predictive_stereo_reference: 3,
            predictive_six_channel_decoder_only: 3,
            standards_literal_geometry: 6,
        }
    );
}

fn sox_rms_amplitude(sox: &Path, input: &Path) -> f64 {
    rms_amplitude_in_window(sox, input, "0.5", "3.0")
}

fn rms_amplitude_in_window(sox: &Path, input: &Path, start: &str, duration: &str) -> f64 {
    let output = run(
        sox,
        &[
            input.display().to_string(),
            "-n".to_string(),
            "trim".to_string(),
            start.to_string(),
            duration.to_string(),
            "stat".to_string(),
        ],
    );
    let text = combined(&output);
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("RMS     amplitude:")
                .and_then(|value| value.trim().parse::<f64>().ok())
        })
        .unwrap_or_else(|| panic!("SoX stat output omitted RMS amplitude: {text}"))
}

fn planned_render_command(
    root: &Path,
    input: &Path,
    source_rate_hz: u32,
    target_rate_hz: u32,
    selection: DsdReconstructionSelection,
    fixture_profile: Option<ResolvedDsdProfile>,
) -> (PlannedCommand, PathBuf) {
    let output = root.join("render-output.w64");
    if let Some(profile) = fixture_profile {
        return (
            build_reference_render_transcript_fixture(
                input,
                &output,
                target_rate_hz,
                profile,
                Some(std::time::Duration::from_secs(4)),
            ),
            output,
        );
    }
    let plan = planned_reference_cell(
        root,
        input,
        source_rate_hz,
        target_rate_hz,
        1,
        PcmBitDepth::Float64,
        ResolvedOutputTarget::WavW64,
        selection,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let command = match &plan.steps()[0] {
        PlannedExecutionStep::Command(command) => command.clone(),
        other => panic!("first Reference step is not the render command: {other:?}"),
    };
    let output = plan
        .reference
        .as_ref()
        .expect("Reference summary")
        .r64_path
        .clone();
    (command, output)
}

#[allow(clippy::too_many_arguments)]
fn planned_response_db(
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
    name: &str,
    source_rate_hz: u32,
    target_rate_hz: u32,
    frequency_hz: u32,
    selection: DsdReconstructionSelection,
    fixture_profile: Option<ResolvedDsdProfile>,
) -> f64 {
    let case_root = root.join(format!("{name}-{frequency_hz}"));
    fs::create_dir_all(&case_root).expect("create response case root");
    let input = case_root.join("input.w64");
    run(
        sox,
        &[
            "-n".to_string(),
            "-r".to_string(),
            source_rate_hz.to_string(),
            "-c".to_string(),
            "1".to_string(),
            "-t".to_string(),
            "w64".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            input.display().to_string(),
            "synth".to_string(),
            "4".to_string(),
            "sine".to_string(),
            frequency_hz.to_string(),
            "vol".to_string(),
            "0.25".to_string(),
        ],
    );
    let (command, output) = planned_render_command(
        &case_root,
        &input,
        source_rate_hz,
        target_rate_hz,
        selection,
        fixture_profile,
    );
    run_planned_command(&command, sox, ffmpeg);
    let before = sox_rms_amplitude(sox, &input);
    let after = sox_rms_amplitude(sox, &output);
    assert!(before > 0.0, "input RMS must be positive");
    let response_db = if after == 0.0 {
        f64::NEG_INFINITY
    } else {
        // The production render includes its mandatory -12 dB headroom. Remove
        // that known level term when measuring the filter response itself.
        20.0 * (after / before).log10() + 12.0
    };
    fs::remove_file(&input).expect("remove response input fixture");
    fs::remove_file(&output).expect("remove response output fixture");
    response_db
}

fn assert_planned_w64_bridge(
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
    target_rate_hz: u32,
) {
    let case_root = root.join(format!("bridge-{target_rate_hz}"));
    fs::create_dir_all(&case_root).expect("create bridge case root");
    let input = case_root.join("input.w64");
    run(
        sox,
        &[
            "-n".to_string(),
            "-r".to_string(),
            "2822400".to_string(),
            "-c".to_string(),
            "2".to_string(),
            "-t".to_string(),
            "w64".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            input.display().to_string(),
            "synth".to_string(),
            "2".to_string(),
            "sine".to_string(),
            "1000".to_string(),
            "sine".to_string(),
            "2000".to_string(),
            "vol".to_string(),
            "0.1".to_string(),
        ],
    );
    let plan = planned_reference_cell(
        &case_root,
        &input,
        2_822_400,
        target_rate_hz,
        2,
        PcmBitDepth::Float64,
        ResolvedOutputTarget::WavW64,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let summary = plan.reference.as_ref().expect("Reference summary");
    let command = match &plan.steps()[0] {
        PlannedExecutionStep::Command(command) => command,
        other => panic!("first Reference step is not render: {other:?}"),
    };
    run_planned_command(command, sox, ffmpeg);
    let rate = combined(&run(
        sox,
        &["--i".to_string(), "-r".to_string(), summary.r64_path.display().to_string()],
    ));
    assert_eq!(rate.trim(), target_rate_hz.to_string());
    let channels = combined(&run(
        sox,
        &["--i".to_string(), "-c".to_string(), summary.r64_path.display().to_string()],
    ));
    assert_eq!(channels.trim(), "2");
    let duration = combined(&run(
        sox,
        &["--i".to_string(), "-D".to_string(), summary.r64_path.display().to_string()],
    ));
    let duration = duration.trim().parse::<f64>().expect("SoX duration is numeric");
    assert!((duration - 2.0).abs() <= 1.0 / f64::from(target_rate_hz));
    fs::remove_file(input).expect("remove W64 bridge input fixture");
    fs::remove_file(&summary.r64_path).expect("remove W64 bridge output fixture");
}

fn qualify_pinned_reference_toolchain_and_profile_responses() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let ffprobe = required_sibling_tool(&ffmpeg, "ffprobe");

    let sox_version = combined(&run(&sox, &["--version".to_string()]));
    assert!(
        sox_version.contains("14.8.0.1"),
        "unexpected SoX-ng version: {sox_version}"
    );
    let sox_sinc = combined(&run(
        &sox,
        &["--help-effect".to_string(), "sinc".to_string()],
    ));
    assert!(sox_sinc.to_ascii_lowercase().contains("sinc"));

    let ffmpeg_version = combined(&run(&ffmpeg, &["-version".to_string()]));
    let first = ffmpeg_version.lines().next().unwrap_or_default();
    let ffprobe_version = combined(&run(&ffprobe, &["-version".to_string()]));
    let ffprobe_first = ffprobe_version.lines().next().unwrap_or_default();
    assert!(
        first.split_whitespace().any(|token| token.starts_with("7.")),
        "qualified FFmpeg must report major version 7: {first}"
    );
    let loudnorm = combined(&run(
        &ffmpeg,
        &[
            "-hide_banner".to_string(),
            "-h".to_string(),
            "filter=loudnorm".to_string(),
        ],
    ));
    assert!(loudnorm.contains("print_format"));

    let qualification: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7.json"
    )))
    .expect("qualification JSON parses");
    let manifest_bytes = &include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7.json"
    ))[..];
    let candidate_bytes = &include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7_candidate.json"
    ))[..];
    match qualification["status"].as_str() {
        Some("qualification_candidate") => {
            assert_eq!(
                manifest_bytes, candidate_bytes,
                "the unpromoted v7 manifest must equal its preserved candidate snapshot"
            );
            assert!(qualification["release_certification"]["report_sha256"].is_null());
            assert!(
                qualification["release_certification"]["candidate_manifest_sha256"].is_null()
            );
        }
        Some("qualified_release") => {
            assert!(qualification["release_certification"]["report_sha256"].is_string());
            let candidate_digest = qualification["release_certification"]
                ["candidate_manifest_sha256"]
                .as_str()
                .expect("promoted policy binds candidate manifest digest");
            assert_eq!(candidate_digest, sha256_hex(candidate_bytes));
        }
        other => panic!("unexpected v7 policy status: {other:?}"),
    }
    assert_eq!(qualification["sox_ng"]["revision"], "324b8cf873fd7836e8848bd87f7a90d8faa6f849");
    assert_eq!(
        qualification["in_process"]["sacd_rs_build_identity"],
        sacd_rs::REFERENCE_BUILD_ID
    );
    assert_eq!(
        qualification["in_process"]["dst_fixture_digest"],
        sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID
            .strip_prefix("sha256:")
            .expect("fixture corpus identity is SHA-256")
    );
    assert_eq!(
        qualification["in_process"]["dst_fixture_manifest_digest"],
        sacd_rs::DST_REFERENCE_FIXTURE_MANIFEST_ID
            .strip_prefix("sha256:")
            .expect("fixture manifest identity is SHA-256")
    );
    assert_eq!(
        qualification["in_process"]["dst_fixture_provenance_digest"],
        sacd_rs::DST_REFERENCE_FIXTURE_PROVENANCE_ID
            .strip_prefix("sha256:")
            .expect("fixture provenance identity is SHA-256")
    );
    assert_eq!(
        qualification["in_process"]["qualification_method"],
        "compressed_dsd64_independent_oracle_plus_standards_literal_geometry_corpus"
    );

    let temp = TempDir::new().expect("qualification tempdir");
    let mut integrated_rate_results = Vec::new();
    for (name, target_rate, interior_hz, nominal_bandwidth_hz, stopband_hz) in [
        ("b1", 44_100, 20_000, 20_950, 24_000),
        ("b2", 48_000, 22_000, 22_800, 26_000),
    ] {
        let interior_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            2_822_400,
            target_rate,
            interior_hz,
            DsdReconstructionSelection::Reference,
            None,
        );
        let nominal_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            2_822_400,
            target_rate,
            nominal_bandwidth_hz,
            DsdReconstructionSelection::Reference,
            None,
        );
        let stopband_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            2_822_400,
            target_rate,
            stopband_hz,
            DsdReconstructionSelection::Reference,
            None,
        );
        assert!(
            (-0.10..=0.02).contains(&interior_db),
            "{name} integrated rate-u interior response {interior_db:.6} dB is not flat"
        );
        assert!(
            (-3.50..=0.02).contains(&nominal_db),
            "{name} integrated rate-u nominal 95% bandwidth response {nominal_db:.6} dB is outside qualification bounds"
        );
        assert!(
            stopband_db <= -140.0,
            "{name} integrated rate-u stopband/alias response {stopband_db:.6} dB is insufficient"
        );
        assert_planned_w64_bridge(&sox, &ffmpeg, temp.path(), target_rate);
        integrated_rate_results.push(serde_json::json!({
            "profile": name,
            "source_rate_hz": 2_822_400,
            "target_rate_hz": target_rate,
            "interior_db": interior_db,
            "nominal_bandwidth_db": nominal_db,
            "stopband_alias_db": stopband_db,
            "w64_bridge": "passed",
        }));
    }

    let mut explicit_profile_results = Vec::new();
    for (name, source_rate, target_rate, passband, transition, center, stopband) in [
        ("b3", 2_822_400, 88_200, 25_000, 10_000, 30_000, 35_000),
        ("b4", 5_644_800, 176_400, 30_000, 15_000, 37_500, 45_000),
        ("b4w", 5_644_800, 176_400, 35_000, 15_000, 42_500, 50_000),
        ("b5", 11_289_600, 176_400, 48_000, 22_000, 59_000, 70_000),
        ("b6_fixture_only", 11_289_600, 352_800, 88_200, 51_800, 114_100, 140_000),
    ] {
        let selection = if name == "b4w" {
            DsdReconstructionSelection::Wideband
        } else {
            DsdReconstructionSelection::Reference
        };
        let fixture_profile = if name == "b6_fixture_only" {
            Some(ResolvedDsdProfile::B6 {
                passband_hz: passband,
                transition_hz: transition,
                center_hz: center,
            })
        } else {
            None
        };
        let passband_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            source_rate,
            target_rate,
            passband,
            selection,
            fixture_profile,
        );
        let center_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            source_rate,
            target_rate,
            center,
            selection,
            fixture_profile,
        );
        let stopband_db = planned_response_db(
            &sox,
            &ffmpeg,
            temp.path(),
            name,
            source_rate,
            target_rate,
            stopband,
            selection,
            fixture_profile,
        );
        assert!(
            passband_db.abs() <= 0.02,
            "{name} passband edge response {passband_db:.6} dB is not flat"
        );
        assert!(
            (-6.25..=-5.80).contains(&center_db),
            "{name} center response {center_db:.6} dB is not the measured -6 dB point"
        );
        assert!(
            stopband_db <= -170.0,
            "{name} stopband response {stopband_db:.6} dB does not substantiate the 180 dB design target"
        );
        explicit_profile_results.push(serde_json::json!({
            "profile": name,
            "source_rate_hz": source_rate,
            "target_rate_hz": target_rate,
            "passband_db": passband_db,
            "center_db": center_db,
            "stopband_db": stopband_db,
        }));
    }
    let sox_store = std::env::var("TONEPOET_REFERENCE_SOX_STORE_PATH")
        .expect("qualified package must expose the exact SoX-ng store path");
    let ffmpeg_store = std::env::var("TONEPOET_REFERENCE_FFMPEG_STORE_PATH")
        .expect("qualified package must expose the exact FFmpeg store path");
    assert_eq!(
        fs::canonicalize(Path::new(&sox_store).join("bin/sox"))
            .expect("qualified SoX-ng store must contain bin/sox"),
        sox,
        "SoX-ng activation path does not belong to the compiled qualification store"
    );
    assert_eq!(
        fs::canonicalize(Path::new(&ffmpeg_store).join("bin/ffmpeg"))
            .expect("qualified FFmpeg store must contain bin/ffmpeg"),
        ffmpeg,
        "FFmpeg activation path does not belong to the compiled qualification store"
    );
    assert_eq!(
        fs::canonicalize(Path::new(&ffmpeg_store).join("bin/ffprobe"))
            .expect("qualified FFmpeg store must contain bin/ffprobe"),
        ffprobe,
        "FFprobe does not belong to the qualified FFmpeg store"
    );
    serde_json::json!({
        "sox_ng": {
            "canonical_path": sox.display().to_string(),
            "store_path": sox_store,
            "executable_sha256": sha256_hex(&fs::read(&sox).expect("read qualified SoX-ng executable")),
            "reported_version": sox_version.lines().next().unwrap_or_default(),
            "required_probe": "sinc",
        },
        "ffmpeg": {
            "canonical_path": ffmpeg.display().to_string(),
            "store_path": ffmpeg_store,
            "executable_sha256": sha256_hex(&fs::read(&ffmpeg).expect("read qualified FFmpeg executable")),
            "reported_version": first,
            "required_probes": ["loudnorm", "print_format"],
            "ffprobe": {
                "canonical_path": ffprobe.display().to_string(),
                "executable_sha256": sha256_hex(&fs::read(&ffprobe).expect("read qualified FFprobe executable")),
                "reported_version": ffprobe_first,
            },
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY,
        },
        "integrated_rate_profiles": integrated_rate_results,
        "explicit_composite_profiles": explicit_profile_results,
    })
}

fn write_report_atomically(path: &Path, value: &Value) {
    let parent = path.parent().expect("qualification report has a parent");
    fs::create_dir_all(parent).expect("create qualification report directory");

    // A report is release authority. Serialize writers before creating a unique
    // same-directory temporary so two commissioned gates cannot overwrite or
    // rename each other's partial output.
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("qualification report filename is valid UTF-8");
    let lock_path = parent.join(format!(".{file_name}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open qualification report writer lock");
    lock.try_lock_exclusive().unwrap_or_else(|error| {
        panic!(
            "another qualification writer owns {}: {error}",
            lock_path.display()
        )
    });

    let bytes = serde_json::to_vec_pretty(value).expect("serialize qualification report");
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{file_name}."))
        .suffix(".tmp")
        .tempfile_in(parent)
        .expect("create unique qualification report temporary");
    temporary
        .write_all(&bytes)
        .expect("write qualification report");
    temporary
        .write_all(b"\n")
        .expect("terminate qualification report");
    temporary
        .as_file()
        .sync_all()
        .expect("sync qualification report temporary");
    temporary
        .persist(path)
        .unwrap_or_else(|error| panic!("atomically install qualification report: {error}"));
    File::open(parent)
        .expect("open qualification report parent")
        .sync_all()
        .expect("sync qualification report parent");
    FileExt::unlock(&lock).expect("release qualification report writer lock");
}

#[test]
fn complete_p0_reference_qualification_report() {
    if !selected() {
        eprintln!("skipping; set {GATE}=1 to run the mandatory real-tool Reference qualification");
        return;
    }
    let default_settings_live_smoke = qualify_default_settings_dsd64_dsf_to_flac();
    let environment_probe_results = qualify_subprocess_environment_isolation();
    let (package_case_count, terminal_bound_case_count) = qualify_lossless_package_cells();
    let analyzer_carrier_results = qualify_analyzer_carrier_contract();
    let analyzer_results = qualify_true_peak_analyzer_authority();
    let gain_terminal_results = qualify_production_measurement_gain_terminal_chain();
    let dst_counts = qualify_dst_oracle_fixture_authority();
    let source_front_end_results = qualify_production_source_front_end_integration();
    let profile_results = qualify_pinned_reference_toolchain_and_profile_responses();
    let qualification_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v7.json"
    ));
    let qualification: Value =
        serde_json::from_slice(qualification_bytes).expect("qualification manifest parses");
    let profile_cells = qualification["cell_contract"]["profile_cells"]
        .as_array()
        .expect("profile cell table is an array");
    let target_depth_cells = qualification["cell_contract"]["target_depth_cells"]
        .as_array()
        .expect("target/depth cell table is an array");
    let supported_profile_cells = profile_cells
        .iter()
        .filter(|cell| {
            cell["result"]
                .as_str()
                .is_some_and(|result| !result.starts_with("error:"))
        })
        .count();
    let rejected_profile_cells = profile_cells.len() - supported_profile_cells;
    let supported_target_depth_cells = target_depth_cells
        .iter()
        .filter(|cell| cell["result"] == "supported")
        .count();
    let rejected_target_depth_cells = target_depth_cells.len() - supported_target_depth_cells;
    let report = serde_json::json!({
        "schema_version": 7,
        "policy": tonepoet_pipeline::DSD_REFERENCE_POLICY_V7_KEY,
        "status": "passed",
        "qualification_manifest_digest": sha256_hex(qualification_bytes),
        "toolchain": profile_results,
        "default_settings_live_smoke": default_settings_live_smoke,
        "in_process_backend": qualification["in_process"].clone(),
        "subprocess_environment": qualification["subprocess_environment"].clone(),
        "qualification_supervision": qualification["qualification_supervision"].clone(),
        "subprocess_environment_probe": environment_probe_results.clone(),
        "dst_independent_oracle": {
            "status": "passed",
            "total_case_count": dst_counts.total,
            "predictive_independent_oracle_case_count": dst_counts.predictive_independent_oracle,
            "predictive_stereo_reference_oracle_case_count": dst_counts.predictive_stereo_reference,
            "predictive_six_channel_decoder_only_case_count": dst_counts.predictive_six_channel_decoder_only,
            "standards_literal_geometry_case_count": dst_counts.standards_literal_geometry,
            "predictive_reference_cells": [
                {"source_rate_hz": 2822400, "channels": 2}
            ],
            "unavailable_predictive_reference_cells": [
                {"source_rate_hz": 2822400, "channels": 1},
                {"source_rate_hz": 5644800, "channels": 1},
                {"source_rate_hz": 5644800, "channels": 2},
                {"source_rate_hz": 11289600, "channels": 1},
                {"source_rate_hz": 11289600, "channels": 2}
            ],
            "fixture_corpus_id": sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID,
            "fixture_manifest_id": sacd_rs::DST_REFERENCE_FIXTURE_MANIFEST_ID,
            "fixture_provenance_id": sacd_rs::DST_REFERENCE_FIXTURE_PROVENANCE_ID,
        },
        "analyzer_carrier": analyzer_carrier_results,
        "production_true_peak_analyzer": analyzer_results,
        "production_source_front_end_integration": source_front_end_results,
        "production_measurement_gain_terminal_chain": gain_terminal_results,
        "analyzer_policy_bounds": qualification["analyzer"].clone(),
        "terminal_bounds": qualification["terminal_bounds"].clone(),
        "riff_capacity": qualification["riff_capacity"].clone(),
        "float64_package_pipeline": qualification["packaging"].clone(),
        "sample_identity_oracle": qualification["sample_identity"].clone(),
        "evidence_command_environment": {
            "status": "passed",
            "policy": qualification["subprocess_environment"].clone(),
            "runtime_probe": environment_probe_results.clone(),
        },
        "package_decode_back": {
            "status": "passed",
            "case_count": package_case_count,
            "empirical_terminal_bound_case_count": terminal_bound_case_count,
            "rates_hz": [44100,48000,88200,96000,176400,192000,352800,384000,705600,768000],
            "channels": [1,2],
            "depths": ["int24","float32","float64"],
            "targets": ["flac_native","wav_riff","wav_rf64","wav_w64","aiff_native","wavpack_native","alac_m4a"],
            "flac_compression_levels": [0,1,2,3,4,5,6,7,8],
            "wavpack_compression_levels": [0,1,2,3],
            "wavpack_int24_required_args": ["-bits_per_raw_sample","24"],
            "package_stream_copy_metadata_sample_identity": "passed",
            "production_metadata_mutator_qualification": "not claimed; production retains mandatory post-mutation decoded-sample verification",
            "command_authority": "exact PlannedExecutionStep vectors from plan_reference_dsd",
        },
        "qualified_cell_contract": {
            "supported_profile_cells": supported_profile_cells,
            "rejected_profile_cells": rejected_profile_cells,
            "supported_target_depth_cells": supported_target_depth_cells,
            "rejected_target_depth_cells": rejected_target_depth_cells,
            "source_rate_channel_cells": qualification["cell_contract"]["source_rate_channel_cells"].clone(),
            "expanded_supported_cell_count": qualification["cell_contract"]["expanded_supported_cell_count"].clone(),
            "expanded_supported_cell_digest": qualification["cell_contract"]["expanded_supported_cell_digest"].clone(),
        },
        "workspace_gate_dependency": "compiled policy, parser, arithmetic, migration, fingerprint, manifest, rerun, publication, UI, CLI, and sentinel tests must already have passed in the same release build",
        "outcome": "pass",
    });
    let path = std::env::var_os("TONEPOET_DSD_REFERENCE_REPORT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/dsd_reference_qualification_report.json")
        });
    write_report_atomically(&path, &report);
    eprintln!("DSD Reference qualification report: {}", path.display());
}

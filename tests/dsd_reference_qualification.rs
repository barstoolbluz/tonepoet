//! Tool-gated release qualification for the P0 Reference DSD pathway.
//!
//! Run with:
//! `TONEPOET_REQUIRE_TOOLS=1 cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture`
//!
//! The test is inert unless explicitly selected. Release automation must set
//! the gate while using the flake-owned SoX-ng and FFmpeg paths.

// The terminal qualification report is one large `serde_json::json!` literal;
// raise the macro recursion limit for this test crate to expand it.
#![recursion_limit = "512"]

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use tokio_util::sync::CancellationToken;
use tonepoet::convert::pipeline::{
    plan_request_for_track, qualify_production_metadata_mutation,
    qualify_reference_materialization_identity_digest,
    qualify_reference_source_materialization, ActionPipeline, AlbumMetadata,
    CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, FailurePolicy, LogPolicy,
    NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PreparedTrack,
    PublishPolicy, RealToolRunner, SacdArea, SourceAudioCoding, SourceAudioDescriptor,
    SourceOptions, StagePolicy, StageRequirement, ToolBinary, TrackId, TrackMetadata,
    TrackSelection, TrackSourceRef,
};
use tonepoet_pipeline::{
    build_reference_render_transcript_fixture, build_reference_silence_scan_command,
    extract_single_loudnorm_report, extract_single_sox_stats_peak_report,
    parse_reference_sox_stats_true_peak_measurement, parse_reference_true_peak_measurement,
    plan_conversion,
    plan_reference_dsd, reference_true_peak_measurement_deadline,
    resolve_reference_deferred_command, validate_post_final_true_peak,
    validate_reference_decode_mechanism, validate_signed_zero_f64le, AudioCodec, AudioFormat,
    BitDepthTarget, ConversionPlan, DbNano, DsdInputFrontEnd, DsdReconstructionSelection,
    DsdReferencePolicyVersion, DsdSourceGainMode, DsdSourceKind, FinalPcmContract, Finalization,
    MeasurementId, MeasurementParser, PcmBitDepth, PlanAction, PipelineSettings, PlanRequest,
    PlannedArg, PlannedCommand, PlannedExecutionStep, PlannedMeasurement, RateTarget,
    ReferenceDecodeAuthority,
    ReferenceDecodeMechanism, ReferenceDecodedCarrier, ReferenceDecodedCarrierSelector,
    ReferenceDecodedSampleRole, ReferenceDither, ReferenceErrorCode,
    ReferenceStreamedWavBoundaryObservationV2, ReferenceStreamedWavCapacityEvidenceV2,
    ReferenceStreamedWavCapacityEvidenceV3, ReferenceStreamedWavDataWrapWitnessV2,
    ReferenceProgrammeScope, ReferenceSampleHashEncoding, ResolvedDsdProfile,
    ResolvedGainPolicy, ResolvedOutputTarget, SampleKind, SourceInfo, SourceRepresentationKind,
    ToolIdentifier, TruePeakMeasurement, TruePeakPurpose, TruePeakValue, WavPackMode,
    W64PcmExpectation, W64PcmFormatExpectation, W64SampleEncoding,
    inspect_exact_w64_pcm, validate_exact_w64_pcm,
    REFERENCE_DECODE_ROUTE_RULES, REFERENCE_SAMPLE_HASH_FORMAT,
    REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS, REFERENCE_TRUE_PEAK_GRID_BOUND,
    REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES,
    REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL,
    REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS,
    REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND,
    REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY, REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR,
    REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT,
};


// Frozen v15 checker compatibility marker. The current report is schema v16.
// append-only v15 checker source marker: "schema_version": 15
// append-only v15 checker source marker: "silent_float64_w64_open_defect"
const _V15_APPEND_ONLY_REPORT_MARKER: &str = concat!(
    r#"\"schema_version\": 15"#,
    r#"\"silent_float64_w64_open_defect\""#,
    "all_zero_content_not_threshold_or_first_block_silence",
);
const GATE: &str = "TONEPOET_REQUIRE_TOOLS";
const SOX_ENV: &str = "TONEPOET_REFERENCE_SOX_PATH";
const FFMPEG_ENV: &str = "TONEPOET_REFERENCE_FFMPEG_PATH";
const METAFLAC_ENV: &str = "TONEPOET_REFERENCE_METAFLAC_PATH";
const WVTAG_ENV: &str = "TONEPOET_REFERENCE_WVTAG_PATH";
const ATOMIC_PARSLEY_ENV: &str = "TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH";
const METAFLAC_STORE_ENV: &str = "TONEPOET_REFERENCE_METAFLAC_STORE_PATH";
const WVTAG_STORE_ENV: &str = "TONEPOET_REFERENCE_WVTAG_STORE_PATH";
const ATOMIC_PARSLEY_STORE_ENV: &str = "TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH";
const QUALIFICATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const QUALIFICATION_PIPELINE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const QUALIFICATION_TERMINATION_TIMEOUT: Duration = Duration::from_secs(10);
const QUALIFICATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

const W64_RIFF_GUID: &[u8; 16] = b"riff.\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\x00\x00";
const W64_FACT_GUID: &[u8; 16] = b"fact\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";
const W64_DATA_GUID: &[u8; 16] = b"data\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";

fn selected() -> bool {
    std::env::var(GATE).as_deref() == Ok("1")
}

#[test]
fn historical_v12_streamed_wav_capacity_contract_remains_frozen() {
    let transition_count = usize::try_from(
        ReferenceStreamedWavCapacityEvidenceV2::expected_transition_count(),
    )
    .expect("historical v12 transition count fits usize");
    assert_eq!(transition_count, 10);
    assert_eq!(ReferenceStreamedWavCapacityEvidenceV2::STREAM_HEADER_BYTES, 66);
    let identity = serde_json::json!({
        "schema_version": 12,
        "policy": tonepoet_pipeline::DSD_REFERENCE_POLICY_V12_KEY,
    });
    assert_eq!(identity["policy"], "sox_ng_14_8_0_1_v12");
}

fn required_tool(variable: &str) -> PathBuf {
    let raw = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} must be set by the qualified package or dev shell"));
    fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("cannot canonicalize {variable}={}: {error}", Path::new(&raw).display()))
}

fn production_metadata_runner(
    ffmpeg: &Path,
    metaflac: &Path,
    wvtag: &Path,
    atomic_parsley: &Path,
) -> RealToolRunner {
    RealToolRunner::new(HashMap::from([
        ("ffmpeg".to_string(), ffmpeg.to_path_buf()),
        ("metaflac".to_string(), metaflac.to_path_buf()),
        ("wvtag".to_string(), wvtag.to_path_buf()),
        ("AtomicParsley".to_string(), atomic_parsley.to_path_buf()),
    ]))
}

fn qualification_metadata() -> (TrackMetadata, AlbumMetadata) {
    let mut track_extra = BTreeMap::new();
    track_extra.insert("MY_NOTE".to_string(), "Reference production mutator".to_string());
    let track = TrackMetadata {
        title: Some("Reference qualification track".to_string()),
        artist: Some("Reference qualification artist".to_string()).into(),
        album_artist: Some("Reference qualification album artist".to_string()).into(),
        composer: Some("Reference qualification composer".to_string()).into(),
        performer: Some("Reference qualification performer".to_string()).into(),
        genre: Some("Reference qualification genre".to_string()).into(),
        date: Some("2026".to_string()),
        track_number: Some(1),
        disc_number: Some(1),
        isrc: Some("USRC17607839".to_string()),
        publisher: Some("Reference qualification publisher".to_string()),
        copyright: Some("Reference qualification copyright".to_string()),
        comment: Some("Exact production metadata path".to_string()),
        pre_emphasis: true,
        extra: track_extra,
    };
    let mut album_extra = BTreeMap::new();
    album_extra.insert("CATALOG".to_string(), "1234567890123".to_string());
    let album = AlbumMetadata {
        album: Some("Reference qualification album".to_string()),
        album_artist: Some("Reference qualification album artist".to_string()).into(),
        genre: Some("Reference qualification genre".to_string()).into(),
        date: Some("2026".to_string()),
        total_tracks: 1,
        total_discs: Some(1),
        disc_number: Some(1),
        extra: album_extra,
    };
    (track, album)
}

fn w64_planner_request(root: &Path, sample_rate_hz: u32, depth: PcmBitDepth) -> PipelineRequest {
    let mut settings = PipelineSettings::default();
    settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
    settings.target_format = AudioFormat::Wav;
    settings.target_sample_rate = RateTarget::PcmHz(sample_rate_hz);
    settings.target_bit_depth = BitDepthTarget::Pcm(depth);

    PipelineRequest {
        job_id: "reference-w64-matrix".to_string(),
        actions: ActionPipeline::default(),
        item_id: "reference-w64-matrix".to_string(),
        container: root.join("source.dsf"),
        source: SourceOptions {
            sidecar_cue_track_metadata: None,
            archive_password: None,
            sacd_area: Some(SacdArea::Stereo),
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_group: None,
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
            cue_sidecar: CueSidecarPolicy::PreferSidecar,
            track_selection: TrackSelection::All,
        },
        settings,
        worker_count: Some(1),
        scratch_staging: None,
        merge: false,
        output_root: root.join("out"),
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
            root: root.join("logs"),
            write_for_blocked: false,
            write_json_log: false,
            write_conversion_log: true,
        },
        stages: StagePolicy {
            metadata: StageRequirement::Enabled,
            replaygain: StageRequirement::Disabled,
            features: StageRequirement::Disabled,
            generate_cue: false,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        album_batch: None,
        album_batch_track: None,
        suppress_incremental_conversion_log_append: false,
        companion: Default::default(),
        pre_extracted_staging: None,
        archive_metadata_overrides: Vec::new(),
        expected_album_track_count: None,
        container_extension: Some("w64".to_string()),
        container_ffmpeg_flags: Vec::new(),
        batch_resolved_identity: None,
        metadata_overrides: Default::default(),
    }
}

fn w64_planner_track(input: &Path, channels: u16) -> PreparedTrack {
    PreparedTrack {
        id: TrackId {
            source_ordinal: 1,
            disc_number: None,
            track_number: 1,
        },
        source_ref: TrackSourceRef::StagedFile(input.to_path_buf()),
        metadata: qualification_metadata().0,
        expected_samples: Some(262_144),
        sample_rate: Some(2_822_400),
        source_audio: SourceAudioDescriptor::from_scalar(
            Some(2_822_400),
            None,
            Some(SourceAudioCoding::Dsd),
        ),
        bit_depth: None,
        warnings: if channels == 1 {
            vec!["qualification mono fixture".to_string()]
        } else {
            Vec::new()
        },
    }
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

fn run_configured_command_unchecked<F>(
    path: &Path,
    args: &[String],
    configure_environment: F,
) -> Output
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
    Output {
        status,
        stdout,
        stderr,
    }
}

fn run_configured_command<F>(path: &Path, args: &[String], configure_environment: F) -> Output
where
    F: FnOnce(&mut Command),
{
    let output = run_configured_command_unchecked(path, args, configure_environment);
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

fn run_unchecked(path: &Path, args: &[String]) -> Output {
    run_configured_command_unchecked(path, args, apply_qualified_environment)
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn first_nonempty_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
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

#[derive(Debug, Clone, Copy)]
struct W64HeaderObservation {
    file_bytes: u64,
    riff_size_field: u64,
    data_chunk_offset: usize,
    data_chunk_size_field: u64,
    payload_offset: usize,
    payload_bytes_present: u64,
}

fn read_le_u64(bytes: &[u8], label: &str) -> u64 {
    u64::from_le_bytes(
        bytes
            .try_into()
            .unwrap_or_else(|_| panic!("{label} is not an eight-byte little-endian field")),
    )
}

fn inspect_w64_header(input: &Path) -> W64HeaderObservation {
    let bytes = fs::read(input)
        .unwrap_or_else(|error| panic!("cannot read W64 fixture {}: {error}", input.display()));
    assert!(
        bytes.len() >= 40,
        "W64 fixture is shorter than its RIFF/WAVE header: {}",
        input.display()
    );
    assert_eq!(
        &bytes[..16],
        W64_RIFF_GUID,
        "fixture is not a W64 RIFF-GUID file: {}",
        input.display()
    );
    let data_chunk_offset = bytes
        .windows(W64_DATA_GUID.len())
        .position(|window| window == W64_DATA_GUID)
        .unwrap_or_else(|| panic!("W64 data GUID is absent: {}", input.display()));
    let payload_offset = data_chunk_offset
        .checked_add(24)
        .expect("W64 payload offset arithmetic does not overflow");
    assert!(
        payload_offset <= bytes.len(),
        "W64 data chunk header is truncated: {}",
        input.display()
    );
    let file_bytes = u64::try_from(bytes.len()).expect("W64 fixture length fits u64");
    W64HeaderObservation {
        file_bytes,
        riff_size_field: read_le_u64(&bytes[16..24], "W64 RIFF size"),
        data_chunk_offset,
        data_chunk_size_field: read_le_u64(
            &bytes[data_chunk_offset + 16..data_chunk_offset + 24],
            "W64 data-chunk size",
        ),
        payload_offset,
        payload_bytes_present: file_bytes
            .checked_sub(u64::try_from(payload_offset).expect("W64 payload offset fits u64"))
            .expect("W64 payload offset does not exceed file length"),
    }
}

fn sox_info_value(sox: &Path, input: &Path, flag: &str) -> String {
    let output = run(
        sox,
        &[
            "--i".to_string(),
            flag.to_string(),
            input.display().to_string(),
        ],
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|error| {
            panic!(
                "SoX info output is not UTF-8 for {}: {error}",
                input.display()
            )
        })
        .trim()
        .to_string()
}

fn sox_reported_sample_frames(sox: &Path, input: &Path) -> u64 {
    sox_info_value(sox, input, "-s")
        .parse::<u64>()
        .unwrap_or_else(|error| {
            panic!(
                "SoX info sample count is not an unsigned integer for {}: {error}",
                input.display()
            )
        })
}

fn assert_exact_w64_package_probe(
    sox: &Path,
    ffprobe: &Path,
    input: &Path,
    depth: &str,
    sample_rate_hz: u32,
    channels: u16,
    expected_frames: u64,
) {
    let (expected_bits, encoding, expected_encoding) = match depth {
        "int16" => (16, W64SampleEncoding::SignedInteger, "Signed Integer PCM"),
        "int24" => (24, W64SampleEncoding::SignedInteger, "Signed Integer PCM"),
        "float32" => (32, W64SampleEncoding::FloatingPoint, "Floating Point PCM"),
        "float64" => (64, W64SampleEncoding::FloatingPoint, "Floating Point PCM"),
        _ => panic!("unknown depth {depth}"),
    };
    let mut file = File::open(input)
        .unwrap_or_else(|error| panic!("open exact W64 probe {}: {error}", input.display()));
    let structure = validate_exact_w64_pcm(
        &mut file,
        W64PcmExpectation {
            sample_rate_hz,
            channels,
            bits_per_sample: expected_bits,
            sample_frames: expected_frames,
            encoding,
        },
    )
    .unwrap_or_else(|error| panic!("exact W64 structure rejected {}: {error}", input.display()));
    assert_eq!(structure.declared_file_bytes, structure.physical_file_bytes);
    assert_eq!(structure.sample_frames, expected_frames);
    assert_eq!(
        structure.declared_data_bytes,
        expected_frames
            .checked_mul(u64::from(channels))
            .and_then(|frames| frames.checked_mul(u64::from(expected_bits / 8)))
            .expect("expected W64 payload arithmetic does not overflow"),
    );

    assert_eq!(sox_info_value(sox, input, "-t"), "w64");
    assert_eq!(
        sox_info_value(sox, input, "-r")
            .parse::<u32>()
            .expect("SoX W64 sample rate is an integer"),
        sample_rate_hz,
        "sample-rate mismatch for wav_w64/{depth}"
    );
    assert_eq!(
        sox_info_value(sox, input, "-c")
            .parse::<u16>()
            .expect("SoX W64 channel count is an integer"),
        channels,
        "channel mismatch for wav_w64/{depth}"
    );
    assert_eq!(
        sox_info_value(sox, input, "-b")
            .parse::<u16>()
            .expect("SoX W64 bit depth is an integer"),
        expected_bits,
        "terminal-depth mismatch for wav_w64/{depth}"
    );
    assert_eq!(
        sox_info_value(sox, input, "-e"),
        expected_encoding,
        "sample encoding mismatch for wav_w64/{depth}"
    );
    assert_eq!(
        sox_reported_sample_frames(sox, input),
        expected_frames,
        "SoX frame count disagrees with exact W64 authority for {}",
        input.display(),
    );

    // A second implementation must parse the exact container. This is not a
    // metadata-only probe: ffprobe must accept the declared extents and stream.
    let output = run(
        ffprobe,
        &[
            "-v".to_string(),
            "error".to_string(),
            "-select_streams".to_string(),
            "a:0".to_string(),
            "-show_entries".to_string(),
            "stream=codec_name,sample_fmt,sample_rate,channels,bits_per_sample,bits_per_raw_sample,duration_ts,time_base:format=format_name".to_string(),
            "-of".to_string(),
            "json".to_string(),
            input.display().to_string(),
        ],
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("ffprobe JSON did not parse for {}: {error}", input.display())
    });
    let streams = value["streams"]
        .as_array()
        .unwrap_or_else(|| panic!("ffprobe omitted streams for {}", input.display()));
    assert_eq!(streams.len(), 1, "expected exactly one selected audio stream");
    let stream = &streams[0];
    let expected_codec = match depth {
        "int16" => "pcm_s16le",
        "int24" => "pcm_s24le",
        "float32" => "pcm_f32le",
        "float64" => "pcm_f64le",
        _ => unreachable!(),
    };
    assert_eq!(stream["codec_name"], expected_codec);
    assert_eq!(json_u64(&stream["sample_rate"], "sample_rate"), u64::from(sample_rate_hz));
    assert_eq!(json_u64(&stream["channels"], "channels"), u64::from(channels));
    let duration_ts = optional_json_u64(stream.get("duration_ts"), "duration_ts");
    if duration_ts != 0 {
        assert_eq!(duration_ts, expected_frames, "ffprobe exact frame duration mismatch");
    }
    let format_name = value["format"]["format_name"]
        .as_str()
        .expect("ffprobe omitted W64 format_name");
    assert!(format_name.split(',').any(|name| name == "w64"));
    let ffmpeg = required_sibling_tool(ffprobe, "ffmpeg");
    let traversal = probe_ffmpeg_w64_full_traversal(&ffmpeg, input);
    assert!(
        traversal.status.success(),
        "FFmpeg full traversal rejected exact W64 {}: {}",
        input.display(),
        String::from_utf8_lossy(&traversal.stderr),
    );
}

fn assert_exact_package_probe(
    sox: &Path,
    ffprobe: &Path,
    input: &Path,
    target: &str,
    depth: &str,
    sample_rate_hz: u32,
    channels: u16,
    expected_frames: Option<u64>,
) {
    if target == "wav_w64" {
        assert_exact_w64_package_probe(
            sox,
            ffprobe,
            input,
            depth,
            sample_rate_hz,
            channels,
            expected_frames.expect("W64 exact package probe requires an independent frame count"),
        );
        return;
    }

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
        ("wav_riff" | "wav_rf64", "int16") => "pcm_s16le",
        ("wav_riff" | "wav_rf64", "int24") => "pcm_s24le",
        ("wav_riff" | "wav_rf64", "float32") => "pcm_f32le",
        ("wav_riff" | "wav_rf64", "float64") => "pcm_f64le",
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

fn r64_decode_authority(final_pcm: FinalPcmContract) -> ReferenceDecodeAuthority {
    tonepoet_pipeline::reference_decode_authority(
        ReferenceDecodedSampleRole::ReconstructionR64W64,
        FinalPcmContract {
            sample_rate_hz: final_pcm.sample_rate_hz,
            channels: final_pcm.channels,
            sample_kind: SampleKind::Float,
            bit_depth: PcmBitDepth::Float64,
            dither: ReferenceDither::None,
        },
    )
    .expect("qualified R64 decode authority")
}

fn qpcm_decode_authority(contract: FinalPcmContract) -> ReferenceDecodeAuthority {
    tonepoet_pipeline::reference_decode_authority(
        ReferenceDecodedSampleRole::TerminalQpcmW64,
        contract,
    )
    .expect("qualified QPCM decode authority")
}

fn packaged_decode_authority(
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
) -> ReferenceDecodeAuthority {
    tonepoet_pipeline::reference_decode_authority(
        ReferenceDecodedSampleRole::PackagedOutput { target },
        contract,
    )
    .expect("qualified packaged-output decode authority")
}

fn post_metadata_decode_authority(
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
) -> ReferenceDecodeAuthority {
    tonepoet_pipeline::reference_decode_authority(
        ReferenceDecodedSampleRole::PostMetadataOutput { target },
        contract,
    )
    .expect("qualified post-metadata decode authority")
}

fn assert_qualification_decode_route_table() -> Value {
    assert_eq!(REFERENCE_DECODE_ROUTE_RULES.len(), 16);
    let int24 = FinalPcmContract {
        sample_rate_hz: 176_400,
        channels: 2,
        sample_kind: SampleKind::SignedInteger,
        bit_depth: PcmBitDepth::Int24,
        dither: ReferenceDither::Tpdf,
    };
    let float32 = FinalPcmContract {
        sample_kind: SampleKind::Float,
        bit_depth: PcmBitDepth::Float32,
        dither: ReferenceDither::None,
        ..int24
    };
    let float64 = FinalPcmContract {
        sample_kind: SampleKind::Float,
        bit_depth: PcmBitDepth::Float64,
        dither: ReferenceDither::None,
        ..int24
    };

    assert_eq!(
        qpcm_decode_authority(int24).mechanism(),
        ReferenceDecodeMechanism::DirectFfmpeg
    );
    assert_eq!(
        qpcm_decode_authority(float32).mechanism(),
        ReferenceDecodeMechanism::DirectFfmpeg
    );
    assert_eq!(
        qpcm_decode_authority(float64).mechanism(),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream
    );
    assert_eq!(
        packaged_decode_authority(ResolvedOutputTarget::WavW64, float64).mechanism(),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream
    );
    assert_eq!(
        packaged_decode_authority(ResolvedOutputTarget::WavRiff, float64).mechanism(),
        ReferenceDecodeMechanism::DirectFfmpeg
    );
    assert_eq!(
        packaged_decode_authority(ResolvedOutputTarget::WavRf64, float64).mechanism(),
        ReferenceDecodeMechanism::DirectFfmpeg
    );
    assert_eq!(
        qpcm_decode_authority(int24).hash_encoding(),
        ReferenceSampleHashEncoding::SignedInt24Le
    );
    assert_eq!(
        qpcm_decode_authority(float32).hash_encoding(),
        ReferenceSampleHashEncoding::Float32Le
    );
    assert_eq!(
        qpcm_decode_authority(float64).hash_encoding(),
        ReferenceSampleHashEncoding::Float64Le
    );
    assert_eq!(
        qpcm_decode_authority(float64).hash_format(),
        REFERENCE_SAMPLE_HASH_FORMAT
    );

    let mut rejected_roles = Vec::new();
    for (role, role_key) in [
        (
            ReferenceDecodedSampleRole::ReconstructionR64W64,
            "r64_float64_w64",
        ),
        (
            ReferenceDecodedSampleRole::TerminalQpcmW64,
            "qpcm_float64_w64",
        ),
        (
            ReferenceDecodedSampleRole::PackagedOutput {
                target: ResolvedOutputTarget::WavW64,
            },
            "packaged_float64_w64",
        ),
        (
            ReferenceDecodedSampleRole::PostMetadataOutput {
                target: ResolvedOutputTarget::WavW64,
            },
            "post_metadata_float64_w64",
        ),
    ] {
        let contract = if role == ReferenceDecodedSampleRole::ReconstructionR64W64 {
            FinalPcmContract {
                sample_rate_hz: float64.sample_rate_hz,
                channels: float64.channels,
                sample_kind: SampleKind::Float,
                bit_depth: PcmBitDepth::Float64,
                dither: ReferenceDither::None,
            }
        } else {
            float64
        };
        let error = validate_reference_decode_mechanism(
            role,
            contract,
            ReferenceDecodeMechanism::DirectFfmpeg,
        )
        .expect_err("direct FFmpeg must be rejected for every Float64 W64 role");
        assert!(error.to_string().contains("required route is sox_f64le_raw_stream"));
        rejected_roles.push(role_key);
    }

    let carrier_temp = TempDir::new().expect("carrier-binding regression tempdir");
    let carrier_source = carrier_temp.path().join("source-placeholder.dsf");
    let carrier_plan = planned_reference_cell(
        carrier_temp.path(),
        &carrier_source,
        2_822_400,
        88_200,
        2,
        PcmBitDepth::Float64,
        ResolvedOutputTarget::WavRiff,
        DsdReconstructionSelection::Reference,
        DsdSourceGainMode::Reference,
        None,
        DbNano::DEFAULT_NORMALIZE_TARGET,
        None,
    );
    let carrier_summary = carrier_plan.reference.as_ref().expect("Reference summary");
    let mislabeled_error = carrier_summary
        .bind_decoded_carrier(
            ReferenceDecodedCarrierSelector::PackagedOutput,
            &carrier_summary.qpcm_path,
        )
        .expect_err("Float64 QPCM W64 must not impersonate Float64 RIFF package");
    assert!(mislabeled_error.to_string().contains("carrier path mismatch"));

    serde_json::json!({
        "status": "passed",
        "attempted_mechanism": ReferenceDecodeMechanism::DirectFfmpeg.key(),
        "required_mechanism": ReferenceDecodeMechanism::SoxFloat64W64RawStream.key(),
        "rejected_role_count": rejected_roles.len(),
        "rejected_roles": rejected_roles,
        "mislabeled_carrier_regression": {
            "status": "passed",
            "attempted_path_role": "qpcm_w64_as_packaged_riff",
            "rejected_before_command_construction": true,
        },
    })
}

fn qualification_decode_route_table_evidence() -> Value {
    let mut routes = serde_json::Map::new();
    for rule in REFERENCE_DECODE_ROUTE_RULES {
        let key = format!(
            "{}:{}",
            rule.role_class().key(),
            rule.hash_encoding().key(),
        );
        let previous = routes.insert(
            key.clone(),
            serde_json::json!({
                "bit_depth": rule.bit_depth().bits(),
                "mechanism": rule.mechanism().key(),
                "hash_encoding": rule.hash_encoding().key(),
            }),
        );
        assert!(previous.is_none(), "duplicate route-evidence key {key}");
    }
    assert_eq!(routes.len(), REFERENCE_DECODE_ROUTE_RULES.len());
    Value::Object(routes)
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

fn decoded_sample_hash(
    carrier: &ReferenceDecodedCarrier,
    sox: &Path,
    ffmpeg: &Path,
) -> String {
    let authority = carrier.authority();
    let contract = authority.contract();
    match authority.mechanism() {
        ReferenceDecodeMechanism::DirectFfmpeg => ffmpeg_sample_hash(
            ffmpeg,
            carrier.path(),
            authority.hash_encoding().ffmpeg_codec(),
        ),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream => {
            assert_eq!(
                authority.hash_encoding(),
                ReferenceSampleHashEncoding::Float64Le,
                "the streamed W64 route is reserved for Float64"
            );
            sox_streamed_float64_w64_sample_hash(
                sox,
                ffmpeg,
                carrier.path(),
                contract.sample_rate_hz,
                contract.channels,
            )
        }
    }
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
    synth_r64_fixture_duration(
        sox,
        output,
        sample_rate_hz,
        channels,
        amplitude,
        silence,
        "0.05",
    );
}

fn synth_r64_fixture_duration(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    amplitude: &str,
    silence: bool,
    duration_seconds: &str,
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
        args.extend([
            "trim".to_string(),
            "0".to_string(),
            duration_seconds.to_string(),
        ]);
    } else {
        args.extend([
            "synth".to_string(),
            duration_seconds.to_string(),
            "sine".to_string(),
            "997".to_string(),
            "vol".to_string(),
            amplitude.to_string(),
        ]);
    }
    run(sox, &args);
}

fn probe_direct_ffmpeg_f64_w64(ffmpeg: &Path, input: &Path) -> Output {
    let args = vec![
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-nostats".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        input.display().to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-vn".to_string(),
        "-sn".to_string(),
        "-dn".to_string(),
        "-c:a".to_string(),
        "pcm_f64le".to_string(),
        "-f".to_string(),
        "f64le".to_string(),
        "pipe:1".to_string(),
    ];
    run_unchecked(ffmpeg, &args)
}

fn encode_float64_w64_fixture(
    sox: &Path,
    root: &Path,
    name: &str,
    sample_rate_hz: u32,
    samples: &[f64],
) -> PathBuf {
    let raw = root.join(format!("{name}.f64le"));
    let output = root.join(format!("{name}.w64"));
    let mut raw_file = File::create(&raw).expect("create Float64 W64 fixture source");
    for sample in samples {
        raw_file
            .write_all(&sample.to_le_bytes())
            .expect("write Float64 W64 fixture sample");
    }
    raw_file
        .sync_all()
        .expect("sync Float64 W64 fixture source");
    drop(raw_file);
    run(
        sox,
        &[
            "-S".to_string(),
            "-D".to_string(),
            "-t".to_string(),
            "raw".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-L".to_string(),
            "-r".to_string(),
            sample_rate_hz.to_string(),
            "-c".to_string(),
            "1".to_string(),
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
    output
}

fn exact_float64_w64_header(observation: W64HeaderObservation) -> bool {
    observation.riff_size_field == observation.file_bytes
        && observation.data_chunk_size_field
            == observation
                .payload_bytes_present
                .checked_add(24)
                .expect("W64 data-chunk size arithmetic does not overflow")
}


fn encode_w64_characterization_fixture(
    sox: &Path,
    root: &Path,
    name: &str,
    sample_rate_hz: u32,
    channels: u16,
    depth: &str,
    samples: &[f64],
) -> PathBuf {
    assert_eq!(samples.len() % usize::from(channels), 0);
    let raw = root.join(format!("{name}.f64le"));
    let output = root.join(format!("{name}.w64"));
    let mut raw_file = File::create(&raw).expect("create W64 characterization source");
    for sample in samples {
        raw_file
            .write_all(&sample.to_le_bytes())
            .expect("write W64 characterization sample");
    }
    raw_file.sync_all().expect("sync W64 characterization source");
    drop(raw_file);
    let (encoding, bits) = match depth {
        "int24" => ("signed-integer", "24"),
        "float32" => ("floating-point", "32"),
        "float64" => ("floating-point", "64"),
        _ => panic!("unsupported W64 characterization depth {depth}"),
    };
    run(
        sox,
        &[
            "-S".to_string(),
            "-D".to_string(),
            "-t".to_string(),
            "raw".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-L".to_string(),
            "-r".to_string(),
            sample_rate_hz.to_string(),
            "-c".to_string(),
            channels.to_string(),
            raw.display().to_string(),
            "-t".to_string(),
            "w64".to_string(),
            "-e".to_string(),
            encoding.to_string(),
            "-b".to_string(),
            bits.to_string(),
            output.display().to_string(),
            // Exercise the same signed-Q1.31 effects boundary used by the
            // terminal gain path without changing the mathematical gain.
            "gain".to_string(),
            "0".to_string(),
        ],
    );
    output
}

fn w64_payload_is_all_zero(path: &Path) -> bool {
    let observation = inspect_w64_header(path);
    let bytes = fs::read(path).expect("read W64 characterization payload");
    bytes[observation.payload_offset..].iter().all(|byte| *byte == 0)
}

fn probe_ffmpeg_w64_full_traversal(ffmpeg: &Path, input: &Path) -> Output {
    run_unchecked(
        ffmpeg,
        &[
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-xerror".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-dn".to_string(),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ],
    )
}

fn decode_w64_to_f64(ffmpeg: &Path, input: &Path, expected_values: usize) -> Vec<f64> {
    let output = run(
        ffmpeg,
        &[
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-xerror".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-dn".to_string(),
            "-c:a".to_string(),
            "pcm_f64le".to_string(),
            "-f".to_string(),
            "f64le".to_string(),
            "pipe:1".to_string(),
        ],
    );
    assert_eq!(output.stdout.len() % 8, 0, "decoded f64 output is misaligned");
    let values = output
        .stdout
        .chunks_exact(8)
        .map(|bytes| f64::from_le_bytes(bytes.try_into().expect("f64 chunk is exact")))
        .collect::<Vec<_>>();
    assert_eq!(values.len(), expected_values, "decoded sample count is not exact");
    values
}

fn exact_w64_frame_count(
    path: &Path,
    sample_rate_hz: u32,
    channels: u16,
    bits_per_sample: u16,
    encoding: W64SampleEncoding,
) -> u64 {
    let mut file = File::open(path)
        .unwrap_or_else(|error| panic!("open exact W64 frame authority {}: {error}", path.display()));
    inspect_exact_w64_pcm(
        &mut file,
        W64PcmFormatExpectation {
            sample_rate_hz,
            channels,
            bits_per_sample,
            encoding,
        },
    )
    .unwrap_or_else(|error| panic!("exact W64 frame authority rejected {}: {error}", path.display()))
    .sample_frames
}

fn exact_w64_characterization_result(
    path: &Path,
    sample_rate_hz: u32,
    channels: u16,
    depth: &str,
    sample_frames: u64,
) -> Result<tonepoet_pipeline::W64ExactStructure, String> {
    let (bits_per_sample, encoding) = match depth {
        "int24" => (24, W64SampleEncoding::SignedInteger),
        "float32" => (32, W64SampleEncoding::FloatingPoint),
        "float64" => (64, W64SampleEncoding::FloatingPoint),
        _ => return Err(format!("unsupported depth {depth}")),
    };
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    validate_exact_w64_pcm(
        &mut file,
        W64PcmExpectation {
            sample_rate_hz,
            channels,
            bits_per_sample,
            sample_frames,
            encoding,
        },
    )
    .map_err(|error| error.to_string())
}

fn qualify_w64_exact_integrity_contract() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("W64 exact-integrity tempdir");
    let sample_frames = 257_usize;
    let rates = [
        44_100_u32, 48_000, 88_200, 96_000, 176_400,
        192_000, 352_800, 384_000, 705_600, 768_000,
    ];
    let depths = ["int24", "float32", "float64"];
    let channels_set = [1_u16, 2_u16];
    let exponents = (-96_i32..=-1_i32).collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut malformed_all_zero_cells = 0_u64;
    let mut valid_all_zero_cells = 0_u64;

    for sample_rate_hz in rates {
        for channels in channels_set {
            for depth in depths {
                let cell = temp.path().join(format!("{sample_rate_hz}-{channels}-{depth}"));
                fs::create_dir_all(&cell).expect("create W64 characterization cell");
                let value_count = sample_frames * usize::from(channels);
                let mut scan_samples = vec![0.0_f64; value_count];
                for (index, exponent) in exponents.iter().enumerate() {
                    let frame = index + 1;
                    scan_samples[frame * usize::from(channels)] = 2_f64.powi(*exponent);
                }
                let scan = encode_w64_characterization_fixture(
                    &sox,
                    &cell,
                    "boundary-scan",
                    sample_rate_hz,
                    channels,
                    depth,
                    &scan_samples,
                );
                exact_w64_characterization_result(
                    &scan,
                    sample_rate_hz,
                    channels,
                    depth,
                    sample_frames as u64,
                )
                .unwrap_or_else(|error| panic!("boundary scan structure failed: {error}"));
                let scan_decoded = decode_w64_to_f64(&ffmpeg, &scan, value_count);
                let surviving = exponents
                    .iter()
                    .enumerate()
                    .map(|(index, exponent)| {
                        let frame = index + 1;
                        (*exponent, scan_decoded[frame * usize::from(channels)] != 0.0)
                    })
                    .collect::<Vec<_>>();
                let threshold_index = surviving
                    .iter()
                    .position(|(_, nonzero)| *nonzero)
                    .unwrap_or_else(|| panic!("no reachable nonzero value for {sample_rate_hz}/{channels}/{depth}"));
                assert!(
                    surviving[..threshold_index].iter().all(|(_, nonzero)| !*nonzero),
                    "sub-threshold values survived non-monotonically"
                );
                assert!(
                    surviving[threshold_index..].iter().all(|(_, nonzero)| *nonzero),
                    "at/above-threshold power-of-two values did not survive monotonically"
                );
                let threshold_exponent = surviving[threshold_index].0;
                let below_exponent = threshold_exponent - 1;

                // Bracket the actual transition region with 256 ordered inputs
                // spanning [2^(e-1), 2^e] at a resolution of 2^e / 510.
                let boundary_denominator = 510_u64;
                let boundary_numerators = (255_u64..=510_u64).collect::<Vec<_>>();
                let boundary_base = 2_f64.powi(threshold_exponent);
                let mut boundary_samples = vec![0.0_f64; value_count];
                for (index, numerator) in boundary_numerators.iter().enumerate() {
                    let frame = index + 1;
                    boundary_samples[frame * usize::from(channels)] =
                        boundary_base * (*numerator as f64) / (boundary_denominator as f64);
                }
                let boundary = encode_w64_characterization_fixture(
                    &sox,
                    &cell,
                    "boundary-neighborhood",
                    sample_rate_hz,
                    channels,
                    depth,
                    &boundary_samples,
                );
                exact_w64_characterization_result(
                    &boundary,
                    sample_rate_hz,
                    channels,
                    depth,
                    sample_frames as u64,
                )
                .unwrap_or_else(|error| panic!("boundary neighborhood structure failed: {error}"));
                let boundary_decoded = decode_w64_to_f64(&ffmpeg, &boundary, value_count);
                let boundary_survival = boundary_numerators
                    .iter()
                    .enumerate()
                    .map(|(index, numerator)| {
                        let frame = index + 1;
                        (*numerator, boundary_decoded[frame * usize::from(channels)] != 0.0)
                    })
                    .collect::<Vec<_>>();
                let first_boundary_nonzero = boundary_survival
                    .iter()
                    .position(|(_, nonzero)| *nonzero)
                    .unwrap_or_else(|| panic!("boundary neighborhood has no surviving value"));
                assert!(first_boundary_nonzero > 0, "2^(e-1) unexpectedly survived");
                assert!(
                    boundary_survival[..first_boundary_nonzero]
                        .iter()
                        .all(|(_, nonzero)| !*nonzero),
                    "boundary neighborhood contains a non-monotonic zero region"
                );
                assert!(
                    boundary_survival[first_boundary_nonzero..]
                        .iter()
                        .all(|(_, nonzero)| *nonzero),
                    "boundary neighborhood contains a non-monotonic nonzero region"
                );
                let largest_zero_multiplier_numerator =
                    boundary_survival[first_boundary_nonzero - 1].0;
                let smallest_nonzero_multiplier_numerator =
                    boundary_survival[first_boundary_nonzero].0;
                assert_eq!(
                    smallest_nonzero_multiplier_numerator,
                    largest_zero_multiplier_numerator + 1,
                    "boundary region is not adjacent at the declared probe resolution"
                );

                let make_impulse = |frame: usize, exponent: i32| {
                    let mut samples = vec![0.0_f64; value_count];
                    samples[frame * usize::from(channels)] = 2_f64.powi(exponent);
                    samples
                };
                let all_zero_samples = vec![0.0_f64; value_count];
                let below_samples = make_impulse(sample_frames / 2, below_exponent);
                let at_samples = make_impulse(sample_frames / 2, threshold_exponent);
                let leading_samples = make_impulse(sample_frames * 3 / 4, threshold_exponent);
                let trailing_samples = make_impulse(sample_frames / 4, threshold_exponent);
                let all_zero = encode_w64_characterization_fixture(
                    &sox, &cell, "all-zero", sample_rate_hz, channels, depth, &all_zero_samples,
                );
                let below = encode_w64_characterization_fixture(
                    &sox, &cell, "below-boundary", sample_rate_hz, channels, depth, &below_samples,
                );
                let at = encode_w64_characterization_fixture(
                    &sox, &cell, "at-boundary", sample_rate_hz, channels, depth, &at_samples,
                );
                let leading = encode_w64_characterization_fixture(
                    &sox, &cell, "leading-silence", sample_rate_hz, channels, depth, &leading_samples,
                );
                let trailing = encode_w64_characterization_fixture(
                    &sox, &cell, "trailing-silence", sample_rate_hz, channels, depth, &trailing_samples,
                );

                assert!(w64_payload_is_all_zero(&all_zero));
                assert!(w64_payload_is_all_zero(&below));
                for nonzero in [&at, &leading, &trailing] {
                    assert!(!w64_payload_is_all_zero(nonzero));
                    exact_w64_characterization_result(
                        nonzero,
                        sample_rate_hz,
                        channels,
                        depth,
                        sample_frames as u64,
                    )
                    .unwrap_or_else(|error| panic!("nonzero control structure failed: {error}"));
                    let traversal = probe_ffmpeg_w64_full_traversal(&ffmpeg, nonzero);
                    assert!(
                        traversal.status.success(),
                        "FFmpeg rejected exact nonzero control {}: {}",
                        nonzero.display(),
                        String::from_utf8_lossy(&traversal.stderr),
                    );
                    let decoded = decode_w64_to_f64(&ffmpeg, nonzero, value_count);
                    assert!(decoded.iter().any(|sample| *sample != 0.0));
                }

                let all_zero_exact = exact_w64_characterization_result(
                    &all_zero, sample_rate_hz, channels, depth, sample_frames as u64,
                );
                let below_exact = exact_w64_characterization_result(
                    &below, sample_rate_hz, channels, depth, sample_frames as u64,
                );
                assert_eq!(
                    all_zero_exact.is_ok(),
                    below_exact.is_ok(),
                    "encoded-all-zero structural disposition changed across the quantization boundary"
                );
                let all_zero_ffmpeg = probe_ffmpeg_w64_full_traversal(&ffmpeg, &all_zero);
                let below_ffmpeg = probe_ffmpeg_w64_full_traversal(&ffmpeg, &below);
                assert_eq!(all_zero_ffmpeg.status.success(), below_ffmpeg.status.success());
                assert_eq!(all_zero_exact.is_ok(), all_zero_ffmpeg.status.success());
                if all_zero_exact.is_ok() {
                    valid_all_zero_cells += 1;
                } else {
                    malformed_all_zero_cells += 1;
                }

                let at_decoded = decode_w64_to_f64(&ffmpeg, &at, value_count);
                assert_ne!(
                    at_decoded[(sample_frames / 2) * usize::from(channels)],
                    0.0,
                    "smallest reachable injected sample did not survive independent decode"
                );
                rows.push(serde_json::json!({
                    "sample_rate_hz": sample_rate_hz,
                    "channels": channels,
                    "depth": depth,
                    "scan_exponents": [-96, -1],
                    "smallest_reachable_nonzero_power_of_two_exponent": threshold_exponent,
                    "immediately_below_boundary_exponent": below_exponent,
                    "boundary_probe_denominator": boundary_denominator,
                    "boundary_probe_count": boundary_numerators.len(),
                    "largest_zero_multiplier_numerator": largest_zero_multiplier_numerator,
                    "smallest_nonzero_multiplier_numerator": smallest_nonzero_multiplier_numerator,
                    "boundary_region_width_base_fraction": "1/510",
                    "boundary_neighborhood_structure": "exact",
                    "smallest_bracketed_nonzero_decoded_nonzero": true,
                    "all_zero_payload_physically_zero": true,
                    "below_boundary_payload_physically_zero": true,
                    "all_zero_structure": if all_zero_exact.is_ok() { "exact" } else { "malformed_rejected" },
                    "below_boundary_structure": if below_exact.is_ok() { "exact" } else { "malformed_rejected" },
                    "ffmpeg_all_zero_opened": all_zero_ffmpeg.status.success(),
                    "ffmpeg_below_boundary_opened": below_ffmpeg.status.success(),
                    "at_boundary_structure": "exact",
                    "at_boundary_decoded_nonzero": true,
                    "leading_silence_control": "exact_and_decoded_nonzero",
                    "trailing_silence_control": "exact_and_decoded_nonzero",
                    "exact_sample_frames": sample_frames,
                }));
            }
        }
    }

    assert_eq!(rows.len(), rates.len() * channels_set.len() * depths.len());
    serde_json::json!({
        "schema": "tonepoet-reference-w64-exact-integrity/v1",
        "status": "passed",
        "policy": tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY,
        "parser_authority": "independent_root_and_chunk_traversal_exact/v1",
        "carrier_contract_digest": "tonepoet-reference-carrier-probe/v2",
        "declared_riff_extent_equals_physical_extent": true,
        "declared_data_extent_equals_exact_payload": true,
        "exact_frame_count_required": true,
        "alignment_and_padding_validated": true,
        "undeclared_trailing_bytes_rejected": true,
        "independent_consumer": "ffmpeg_full_decode_xerror",
        "writer_trigger_classification": "encoded_all_zero_after_depth_and_effects_quantization; input_threshold_is_cell_specific_and_empirically_bounded",
        "boundary_region_resolution_base_fraction": "1/510",
        "enabled_depths": depths,
        "rates_hz": rates,
        "channels": channels_set,
        "cell_count": rows.len(),
        "malformed_all_zero_cell_count": malformed_all_zero_cells,
        "valid_all_zero_cell_count": valid_all_zero_cells,
        "uncharacterized_enabled_cells": 0,
        "same_path_qpcm_package_hash_counted_as_independent_packaging": false,
        "w64_delivery_mode": "terminal_qpcm_is_delivered_directly_after_exact_structure_and_full_consumer_traversal",
        "cells": rows,
    })
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

    assert_exact_package_probe(&sox, &ffprobe, &output, "flac_native", "int24", 88_200, 2, None);
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

#[derive(Debug, Clone, Copy)]
enum AdversarialAnalyzerFixture {
    Impulse,
    NearBandEdgeBurst,
    AlternatingSign,
    BroadbandDeterministic,
    BoundaryTransient,
}

impl AdversarialAnalyzerFixture {
    const ALL: [Self; 5] = [
        Self::Impulse,
        Self::NearBandEdgeBurst,
        Self::AlternatingSign,
        Self::BroadbandDeterministic,
        Self::BoundaryTransient,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Impulse => "impulse",
            Self::NearBandEdgeBurst => "near_band_edge_burst",
            Self::AlternatingSign => "alternating_sign",
            Self::BroadbandDeterministic => "broadband_deterministic",
            Self::BoundaryTransient => "boundary_transient",
        }
    }
}

fn write_adversarial_analyzer_fixture(
    sox: &Path,
    output: &Path,
    sample_rate_hz: u32,
    channels: u16,
    fixture: AdversarialAnalyzerFixture,
    peak_position: AnalyzerPeakPosition,
) -> f64 {
    const PEAK_DBFS: f64 = -0.500;
    let amplitude = 10_f64.powf(PEAK_DBFS / 20.0);
    let sample_count = ((f64::from(sample_rate_hz) * 0.050).ceil() as usize).max(4_096);
    let active_len = (sample_count / 4).clamp(512, 8_192);
    let active_start = match peak_position {
        AnalyzerPeakPosition::Early => 16,
        AnalyzerPeakPosition::Late => sample_count - active_len - 16,
    };
    let active_end = active_start + active_len;
    let mut samples = vec![0.0_f64; sample_count * usize::from(channels)];

    for channel in 0..usize::from(channels) {
        let phase = channel as f64 * std::f64::consts::FRAC_PI_3;
        for index in 0..sample_count {
            let local = index.saturating_sub(active_start);
            let inside = index >= active_start && index < active_end;
            let value = match fixture {
                AdversarialAnalyzerFixture::Impulse => {
                    let center = match peak_position {
                        AnalyzerPeakPosition::Early => active_start + 8 + channel,
                        AnalyzerPeakPosition::Late => active_end - 9 - channel,
                    };
                    match index.abs_diff(center) {
                        0 => 1.0,
                        1 => -0.5,
                        2 => 0.25,
                        _ => 0.0,
                    }
                }
                AdversarialAnalyzerFixture::NearBandEdgeBurst if inside => {
                    let envelope = 0.5
                        - 0.5
                            * (std::f64::consts::TAU * local as f64
                                / (active_len - 1) as f64)
                                .cos();
                    envelope
                        * (std::f64::consts::TAU * 0.49 * local as f64 + phase).sin()
                }
                AdversarialAnalyzerFixture::AlternatingSign if inside => {
                    if (local + channel) % 2 == 0 { 1.0 } else { -1.0 }
                }
                AdversarialAnalyzerFixture::BroadbandDeterministic if inside => {
                    let t = local as f64;
                    0.40 * (std::f64::consts::TAU * 0.03125 * t + phase).cos()
                        + 0.25 * (std::f64::consts::TAU * 0.173 * t + phase * 0.5).sin()
                        + 0.20 * (std::f64::consts::TAU * 0.307 * t - phase).cos()
                        + 0.15 * (std::f64::consts::TAU * 0.463 * t + phase * 1.5).sin()
                }
                AdversarialAnalyzerFixture::BoundaryTransient => {
                    let boundary = match peak_position {
                        AnalyzerPeakPosition::Early => 0,
                        AnalyzerPeakPosition::Late => sample_count - 1,
                    };
                    let distance = index.abs_diff(boundary);
                    if distance < 64 {
                        let sign = if (distance + channel) % 2 == 0 { 1.0 } else { -1.0 };
                        sign * (1.0 - distance as f64 / 64.0)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            };
            samples[index * usize::from(channels) + channel] = value;
        }
    }

    let unscaled_peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f64, f64::max);
    assert!(unscaled_peak > 0.0, "adversarial fixture must contain signal");
    let scale = amplitude / unscaled_peak;
    let mut bytes = Vec::with_capacity(samples.len() * 8);
    for sample in samples {
        bytes.extend_from_slice(&(sample * scale).to_le_bytes());
    }

    let raw = output.with_extension(format!("{}.f64le", fixture.key()));
    fs::write(&raw, bytes).expect("write adversarial analyzer fixture");
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
    fs::remove_file(raw).expect("remove adversarial raw analyzer fixture");
    PEAK_DBFS
}

fn measurement_with_oversample_factor(
    measurement: &PlannedMeasurement,
    sample_rate_hz: u32,
    oversample_factor: u32,
) -> PlannedMeasurement {
    let mut oracle = measurement.clone();
    let oversampled_rate = sample_rate_hz
        .checked_mul(oversample_factor)
        .expect("oracle oversampling rate fits u32");
    let rate_index = oracle
        .command
        .args
        .iter()
        .position(|arg| arg == "-s")
        .expect("SoX analyzer command contains -s")
        + 1;
    oracle.command.args[rate_index] = oversampled_rate.to_string();
    oracle.command.description = format!(
        "{} ({oversample_factor}x qualification oracle)",
        oracle.command.description
    );
    oracle
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
        .all(|measurement| measurement.parser == MeasurementParser::SoxStatsPkLevDbV1));
    for measurement in &measurements {
        let carrier = measurement
            .carrier_path()
            .expect("measurement carrier is path-backed")
            .to_string_lossy()
            .into_owned();
        let float32_post = summary.final_pcm.bit_depth == PcmBitDepth::Float32
            && measurement.purpose == TruePeakPurpose::PostFinalAcceptance;
        assert_eq!(measurement.parser, MeasurementParser::SoxStatsPkLevDbV1);
        assert_eq!(measurement.command.tool, ToolIdentifier::Sox);
        let deadline = measurement
            .command
            .expected_duration
            .expect("policy v15 analyzer binds a workload-derived deadline");
        assert_eq!(summary.analyzer_deadline, deadline);
        assert!(
            deadline >= Duration::from_secs(REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS)
                && deadline <= Duration::from_secs(REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS)
        );
        assert!(measurement.command.args.iter().any(|arg| arg == "stats"));
        assert!(measurement.command.args.windows(2).any(|window| {
            window[0] == "-s"
                && window[1]
                    == (summary.final_pcm.sample_rate_hz
                        * REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR)
                        .to_string()
        }));
        if float32_post {
            let producer = measurement
                .input_stage
                .as_ref()
                .expect("policy v15 Float32 measurement has a typed FFmpeg producer");
            assert_eq!(producer.expected_duration, measurement.command.expected_duration);
            assert_eq!(producer.tool, ToolIdentifier::Ffmpeg);
            assert_eq!(producer.input.as_path(), measurement.carrier_path());
            assert_eq!(measurement.command.input, tonepoet_pipeline::InputSource::Stdin);
            assert!(producer.args.windows(2).any(|window| {
                window[0] == "-c:a" && window[1] == "pcm_f64le"
            }));
            assert!(measurement.command.args.windows(2).any(|window| {
                window[0] == "-t" && window[1] == "raw"
            }));
        } else {
            assert!(measurement.input_stage.is_none());
            assert_eq!(measurement.command.input.as_path(), measurement.carrier_path());
            assert!(measurement.command.args.iter().any(|arg| arg == &carrier));
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
    let package_pipelines: Vec<_> = steps
        .iter()
        .skip(1)
        .filter_map(|step| match step {
            PlannedExecutionStep::Pipeline(pipeline) => Some(pipeline),
            _ => None,
        })
        .collect();
    let float64_wav_pipeline_target = summary.final_pcm.bit_depth == PcmBitDepth::Float64
        && matches!(
            summary.target,
            ResolvedOutputTarget::WavRiff | ResolvedOutputTarget::WavRf64
        );
    if summary.target == ResolvedOutputTarget::WavW64 {
        assert!(packages.is_empty());
        assert!(package_pipelines.is_empty());
        assert_eq!(summary.qpcm_path, summary.packaged_path);
    } else if float64_wav_pipeline_target {
        // Float64 RIFF/RF64 packaging is a typed two-process pipeline: the
        // defective direct FFmpeg f64-W64 decode route is forbidden, so the
        // producer streams raw f64le and FFmpeg consumes stdin.
        assert!(packages.is_empty());
        assert_eq!(package_pipelines.len(), 1);
        let pipeline = package_pipelines[0];
        assert!(matches!(&pipeline.producer.tool, ToolIdentifier::Sox));
        assert!(matches!(&pipeline.consumer.tool, ToolIdentifier::Ffmpeg));
        // Raw f64le stdin has no header, so `-ar`/`-ac` are mandatory
        // INPUT-side declarations and must precede `-i`; they are not
        // resampling requests. Filter/resample flags remain forbidden.
        assert!(!pipeline.consumer.args.iter().any(|arg| matches!(
            arg.as_str(),
            "-af" | "-filter:a" | "-sample_fmt"
        )));
        let input_index = pipeline
            .consumer
            .args
            .iter()
            .position(|arg| arg == "-i")
            .expect("pipeline consumer declares -i");
        let ar_index = pipeline
            .consumer
            .args
            .iter()
            .position(|arg| arg == "-ar")
            .expect("raw f64le consumer declares -ar");
        assert!(
            ar_index < input_index,
            "-ar must be an input-side declaration, not an output resample"
        );
        assert!(
            !pipeline.consumer.args[input_index..]
                .iter()
                .any(|arg| arg == "-ar"),
            "no output-side -ar (resample) is permitted"
        );
        assert!(expected_compression_level.is_none());
    } else {
        assert_eq!(packages.len(), 1);
        assert!(package_pipelines.is_empty());
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
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V16;
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
        .expect("streamed v15 measurement has a typed input stage");
    assert_eq!(producer.input.as_path(), measurement.carrier_path());
    assert_eq!(producer.output, tonepoet_pipeline::OutputSink::Stdout);
    assert_eq!(
        producer.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(producer.environment, BTreeMap::from([("LC_ALL".to_string(), "C".to_string())]));
    assert_eq!(measurement.command.input, tonepoet_pipeline::InputSource::Stdin);
    assert_eq!(
        measurement.command.environment_policy,
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
    );
    assert_eq!(
        measurement.command.environment,
        BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
    );

    let tool_path = |tool: ToolIdentifier| match tool {
        ToolIdentifier::Sox => sox,
        ToolIdentifier::Ffmpeg => ffmpeg,
        other => panic!("unexpected measurement pipeline tool {other:?}"),
    };
    let producer_path = tool_path(producer.tool.clone());
    let consumer_path = tool_path(measurement.command.tool.clone());

    let mut producer_command = Command::new(producer_path);
    producer_command
        .args(&producer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_qualified_environment(&mut producer_command);
    let mut producer_child = producer_command.spawn().unwrap_or_else(|error| {
        panic!("failed to spawn {} {:?}: {error}", producer_path.display(), producer.args)
    });
    let producer_stderr_task = drain_child_stderr(&mut producer_child, "measurement producer");
    let producer_stdout = producer_child
        .stdout
        .take()
        .expect("measurement producer stdout is piped");

    let mut consumer_command = Command::new(consumer_path);
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
                consumer_path.display(),
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
        producer_path.display(),
        producer.args,
        String::from_utf8_lossy(&output.producer.stderr),
    );
    assert!(
        output.consumer.status.success(),
        "{} {:?} failed: stdout={} stderr={}",
        consumer_path.display(),
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

fn create_sparse_w64_capacity_fixture(
    seed: &Path,
    output: &Path,
    audio_payload_bytes: u64,
) -> u64 {
    assert_eq!(
        audio_payload_bytes % tonepoet_pipeline::REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
        0,
        "capacity fixture payload must be frame aligned",
    );
    let seed_bytes = fs::read(seed).expect("read seed W64");
    assert_eq!(&seed_bytes[..16], W64_RIFF_GUID, "seed is W64");
    let fact = seed_bytes
        .windows(16)
        .position(|window| window == W64_FACT_GUID)
        .expect("W64 fact chunk");
    let data = seed_bytes
        .windows(16)
        .position(|window| window == W64_DATA_GUID)
        .expect("W64 data chunk");
    let payload_offset = data + 24;
    assert!(payload_offset <= seed_bytes.len(), "valid W64 data header");
    let frame_count =
        audio_payload_bytes / tonepoet_pipeline::REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
    let file_size = u64::try_from(payload_offset)
        .expect("W64 header size")
        .checked_add(audio_payload_bytes)
        .expect("sparse W64 fixture size does not overflow");
    let data_chunk_size = audio_payload_bytes
        .checked_add(24)
        .expect("sparse W64 data-chunk size does not overflow");

    let mut header = seed_bytes[..payload_offset].to_vec();
    header[16..24].copy_from_slice(&file_size.to_le_bytes());
    header[fact + 24..fact + 32].copy_from_slice(&frame_count.to_le_bytes());
    header[data + 16..data + 24].copy_from_slice(&data_chunk_size.to_le_bytes());

    let mut file = File::create(output).expect("create sparse W64 capacity fixture");
    file.write_all(&header).expect("write sparse W64 header");
    file.set_len(file_size).expect("size sparse W64 fixture");
    file.sync_all().expect("sync sparse W64 fixture");
    frame_count
}

fn duration_for_guarded_output_frames(sample_frames: u64, sample_rate_hz: u32) -> Duration {
    let unguarded_frames = sample_frames
        .checked_sub(tonepoet_pipeline::REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES)
        .expect("capacity boundary includes the mandatory guard frame");
    let duration_floor_frames = unguarded_frames
        .checked_sub(1)
        .expect("capacity-boundary duration requires at least one unguarded frame");
    let nanos = u128::from(duration_floor_frames)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_div(u128::from(sample_rate_hz)))
        .and_then(|value| value.checked_add(1))
        .expect("capacity-boundary duration arithmetic does not overflow");
    let duration = Duration::from_nanos(u64::try_from(nanos).expect("boundary duration fits u64"));
    let planned_unguarded = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate_hz))
        .and_then(|value| value.checked_add(999_999_999))
        .map(|value| value / 1_000_000_000)
        .expect("boundary duration arithmetic does not overflow");
    assert_eq!(planned_unguarded, u128::from(unguarded_frames));
    duration
}

fn capacity_boundary_plan_result(
    root: &Path,
    input: &Path,
    sample_frames: u64,
) -> tonepoet_pipeline::Result<ConversionPlan> {
    const SAMPLE_RATE_HZ: u32 = ReferenceStreamedWavCapacityEvidenceV3::SAMPLE_RATE_HZ;
    let mut settings = PipelineSettings::default();
    settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
    settings.target_format = target_format(ResolvedOutputTarget::WavW64);
    settings.target_sample_rate = RateTarget::PcmHz(SAMPLE_RATE_HZ);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float64);
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V16;
    settings.dsd.from_dsd.profile = DsdReconstructionSelection::Reference;
    settings.dsd.from_dsd.gain_mode = DsdSourceGainMode::Reference;
    let request = PlanRequest {
        input_path: input.to_path_buf(),
        output_path: root.join(format!("capacity-{sample_frames}.w64")),
        source: SourceInfo {
            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(1),
            duration: Some(duration_for_guarded_output_frames(sample_frames, SAMPLE_RATE_HZ)),
            dsd_source_kind: Some(DsdSourceKind::DsfUncompressed),
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(root.join("capacity-work")),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: Some(ResolvedOutputTarget::WavW64),
        reference_programme_scope: ReferenceProgrammeScope::Singleton,
        planned_riff_non_audio_upper_bound_bytes: Some(0),
    };
    fs::create_dir_all(root.join("capacity-work")).expect("create capacity planner work directory");
    plan_reference_dsd(&request)
}

fn inspect_streaming_wav_header(producer: &PlannedCommand, sox: &Path) -> (u32, u32, usize) {
    const HEADER_CAPTURE_BYTES: usize = 4096;

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
    let mut data_header = None;
    while offset.checked_add(8).is_some_and(|end| end <= header.len()) {
        let chunk_id = &header[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(header[offset + 4..offset + 8].try_into().unwrap());
        if chunk_id == b"data" {
            data_header = Some((chunk_size, offset + 8));
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
    let (data_size_field, data_payload_offset) = data_header.unwrap_or_else(|| {
        panic!(
            "streaming WAV data chunk was not present in the first {HEADER_CAPTURE_BYTES} bytes; stderr={}",
            String::from_utf8_lossy(&stderr)
        )
    });
    (riff_size_field, data_size_field, data_payload_offset)
}

fn qualify_analyzer_carrier_contract() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("analyzer carrier tempdir");
    let root = temp.path().join("carrier");
    fs::create_dir_all(&root).expect("create analyzer carrier root");

    // Float64 W64: preserve the inherited FFmpeg direct-decode defect witness,
    // while policy v15 measures the path-backed carrier directly with SoX-ng
    // after creating a qualified 16x measurement-only view.
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

    let f64_corrected = execute_measurement(f64_summary, f64_measurement, &sox, &ffmpeg, &root, 1);
    let f64_corrected_input_tp = match f64_corrected.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("-20 dB f64 carrier measured as silence"),
    };
    assert!(
        (f64_corrected_input_tp - f64_analytic_peak).abs() <= 0.02,
        "v15 oversampled f64 measurement changed true peak: analytic={f64_analytic_peak}, corrected={f64_corrected_input_tp}"
    );

    // Permanently reproduce the pinned SoX-ng Float64-W64 silent-content writer
    // defect. FFmpeg is only a corroborating consumer here: structural W64
    // qualification uses SoX because SoX reads its own malformed silent files.
    let silence_defect_root = root.join("f64-w64-silence-header-defect");
    fs::create_dir_all(&silence_defect_root).expect("create silence defect probe root");
    let sample_rate_hz = 88_200_u32;
    let sample_frames = 8_820_usize;
    let tone_samples = (0..sample_frames)
        .map(|frame| {
            let phase = std::f64::consts::TAU * 1_000.0 * frame as f64
                / f64::from(sample_rate_hz);
            0.5 * phase.sin()
        })
        .collect::<Vec<_>>();
    let silence_samples = vec![0.0_f64; sample_frames];
    let mut tiny_nonzero_samples = silence_samples.clone();
    tiny_nonzero_samples[sample_frames / 2] = 2_f64.powi(-24);
    let mut leading_silence_samples = silence_samples.clone();
    leading_silence_samples[sample_frames / 2..]
        .copy_from_slice(&tone_samples[sample_frames / 2..]);
    let mut trailing_silence_samples = silence_samples.clone();
    trailing_silence_samples[..sample_frames / 2]
        .copy_from_slice(&tone_samples[..sample_frames / 2]);

    let tone_path = encode_float64_w64_fixture(
        &sox,
        &silence_defect_root,
        "tone",
        sample_rate_hz,
        &tone_samples,
    );
    let silence_path = encode_float64_w64_fixture(
        &sox,
        &silence_defect_root,
        "all-zero",
        sample_rate_hz,
        &silence_samples,
    );
    let tiny_nonzero_path = encode_float64_w64_fixture(
        &sox,
        &silence_defect_root,
        "tiny-nonzero",
        sample_rate_hz,
        &tiny_nonzero_samples,
    );
    let leading_silence_path = encode_float64_w64_fixture(
        &sox,
        &silence_defect_root,
        "leading-silence-then-tone",
        sample_rate_hz,
        &leading_silence_samples,
    );
    let trailing_silence_path = encode_float64_w64_fixture(
        &sox,
        &silence_defect_root,
        "tone-then-trailing-silence",
        sample_rate_hz,
        &trailing_silence_samples,
    );

    let tone_header = inspect_w64_header(&tone_path);
    let silence_header = inspect_w64_header(&silence_path);
    let tiny_nonzero_header = inspect_w64_header(&tiny_nonzero_path);
    let leading_silence_header = inspect_w64_header(&leading_silence_path);
    let trailing_silence_header = inspect_w64_header(&trailing_silence_path);
    let expected_payload_bytes = u64::try_from(sample_frames)
        .expect("fixture sample count fits u64")
        .checked_mul(8)
        .expect("fixture payload size does not overflow");
    let expected_file_bytes = u64::try_from(silence_header.payload_offset)
        .expect("W64 payload offset fits u64")
        .checked_add(expected_payload_bytes)
        .expect("W64 fixture size does not overflow");
    let expected_data_chunk_size = expected_payload_bytes
        .checked_add(24)
        .expect("W64 data-chunk size does not overflow");

    for (name, path, observation) in [
        ("tone", &tone_path, tone_header),
        ("tiny_nonzero", &tiny_nonzero_path, tiny_nonzero_header),
        (
            "leading_silence_then_tone",
            &leading_silence_path,
            leading_silence_header,
        ),
        (
            "tone_then_trailing_silence",
            &trailing_silence_path,
            trailing_silence_header,
        ),
    ] {
        assert_eq!(observation.file_bytes, expected_file_bytes, "{name} file size");
        assert_eq!(
            observation.payload_bytes_present,
            expected_payload_bytes,
            "{name} payload size"
        );
        assert!(
            exact_float64_w64_header(observation),
            "{name} did not receive exact W64 size fields: {observation:?}"
        );
        assert_eq!(
            observation.data_chunk_size_field,
            expected_data_chunk_size,
            "{name} data-chunk size"
        );
        exact_w64_characterization_result(
            path,
            sample_rate_hz,
            1,
            "float64",
            u64::try_from(sample_frames).expect("fixture sample count fits u64"),
        )
        .unwrap_or_else(|error| panic!("{name} exact W64 validation failed: {error}"));
    }
    assert_eq!(silence_header.file_bytes, expected_file_bytes);
    assert_eq!(silence_header.payload_bytes_present, expected_payload_bytes);
    assert_eq!(silence_header.data_chunk_offset, 112);
    assert_eq!(silence_header.payload_offset, 136);
    assert_eq!(
        silence_header.riff_size_field,
        u64::try_from(silence_header.payload_offset).expect("W64 payload offset fits u64"),
        "silent W64 RIFF size must reproduce the pinned header-only defect"
    );
    assert_eq!(
        silence_header.data_chunk_size_field, 24,
        "silent W64 data chunk must reproduce the pinned empty-payload declaration"
    );
    assert!(
        !exact_float64_w64_header(silence_header),
        "pinned SoX-ng unexpectedly fixed the silent W64 header without a policy/pin lift"
    );
    let silence_validation_error = exact_w64_characterization_result(
        &silence_path,
        sample_rate_hz,
        1,
        "float64",
        u64::try_from(sample_frames).expect("fixture sample count fits u64"),
    )
    .expect_err(
        "the pinned all-zero W64 witness must be rejected by the independent exact parser",
    );
    let silence_diagnostic = format!(
        "{} qualification all-zero Wave64 witness: {silence_validation_error}",
        tonepoet_pipeline::reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
    );
    assert!(
        silence_diagnostic.starts_with("DSD-REF-P0-026:"),
        "silent W64 rejection lost the production diagnostic: {silence_diagnostic}"
    );
    assert!(
        silence_validation_error.contains("root declares 136 bytes")
            && silence_validation_error.contains("physical file contains 70696 bytes"),
        "silent W64 rejection did not identify the known false declared extent: {silence_validation_error}"
    );

    for path in [
        &tone_path,
        &silence_path,
        &tiny_nonzero_path,
        &leading_silence_path,
        &trailing_silence_path,
    ] {
        assert_eq!(
            sox_reported_sample_frames(&sox, path),
            u64::try_from(sample_frames).expect("fixture sample count fits u64"),
            "SoX did not read the complete W64 payload for {}",
            path.display()
        );
    }

    let silence_direct = probe_direct_ffmpeg_f64_w64(&ffmpeg, &silence_path);
    let tone_direct = probe_direct_ffmpeg_f64_w64(&ffmpeg, &tone_path);
    let tiny_nonzero_direct = probe_direct_ffmpeg_f64_w64(&ffmpeg, &tiny_nonzero_path);
    let leading_silence_direct = probe_direct_ffmpeg_f64_w64(&ffmpeg, &leading_silence_path);
    let trailing_silence_direct = probe_direct_ffmpeg_f64_w64(&ffmpeg, &trailing_silence_path);
    assert!(
        !silence_direct.status.success(),
        "FFmpeg unexpectedly ignored the pinned all-zero W64 header defect"
    );
    for (name, output) in [
        ("tone", &tone_direct),
        ("tiny_nonzero", &tiny_nonzero_direct),
        ("leading_silence_then_tone", &leading_silence_direct),
        ("tone_then_trailing_silence", &trailing_silence_direct),
    ] {
        assert!(
            output.status.success(),
            "matched {name} Float64 W64 control failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let silent_w64_header_finalization_defect = serde_json::json!({
        "status": "sox_writer_defect_reproduced_and_bounded",
        "writer": "sox_ng_14_8_0_1",
        "container": "w64",
        "sample_encoding": "float64",
        "sample_rate_hz": sample_rate_hz,
        "channels": 1,
        "sample_frames": sample_frames,
        "file_bytes": silence_header.file_bytes,
        "data_chunk_offset_bytes": silence_header.data_chunk_offset,
        "payload_offset_bytes": silence_header.payload_offset,
        "payload_bytes_present": silence_header.payload_bytes_present,
        "nonzero_riff_size_field": tone_header.riff_size_field,
        "nonzero_data_chunk_size_field": tone_header.data_chunk_size_field,
        "silence_riff_size_field": silence_header.riff_size_field,
        "silence_data_chunk_size_field": silence_header.data_chunk_size_field,
        "sox_reported_silence_frames": sox_reported_sample_frames(&sox, &silence_path),
        "direct_ffmpeg_silence_opened": silence_direct.status.success(),
        "direct_ffmpeg_tone_opened": tone_direct.status.success(),
        "direct_ffmpeg_tiny_nonzero_opened": tiny_nonzero_direct.status.success(),
        "direct_ffmpeg_leading_silence_opened": leading_silence_direct.status.success(),
        "direct_ffmpeg_trailing_silence_opened": trailing_silence_direct.status.success(),
        "trigger_classification": "historical_float64_single_amplitude_witness_only",
        "ffmpeg_disposition": "correctly_refuses_declared_empty_w64_payload",
        "qualification_probe_disposition": "superseded_by_v16_independent_exact_parser",
        "exact_parser_rejected_silence": true,
        "exact_parser_diagnostic_code": "DSD-REF-P0-026",
        "exact_parser_error": silence_validation_error,
        "exact_parser_diagnostic": silence_diagnostic,
        "production_disposition": "malformed_w64_rejected_before_publication_DSD-REF-P0-026",
        "ffmpeg_error": first_nonempty_line(&String::from_utf8_lossy(
            &silence_direct.stderr,
        ))
        .to_string(),
    });

    // Retain the historical v13 streamed-WAV capacity probe as a separate,
    // conservative admission witness. It is no longer the v15 analyzer route.
    let mut f64_producer = PlannedCommand::new(
        ToolIdentifier::Sox,
        vec![
            "-S".to_string(),
            "-D".to_string(),
            f64_summary.r64_path.display().to_string(),
            "-t".to_string(),
            "wav".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-".to_string(),
        ],
        tonepoet_pipeline::InputSource::Path(f64_summary.r64_path.clone()),
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        "historical v13 streamed-WAV capacity probe",
    );
    f64_producer.environment_policy =
        tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    f64_producer
        .environment
        .insert("LC_ALL".to_string(), "C".to_string());
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
    let f64_source_sample_bits = streamed_float64_w64_f64_samples(
        &sox,
        &f64_summary.r64_path,
    )
        .into_iter()
        .map(f64::to_bits)
        .collect::<Vec<_>>();
    let f64_streamed_sample_bits = direct_ffmpeg_f64_samples(&ffmpeg, &streamed_wav)
        .into_iter()
        .map(f64::to_bits)
        .collect::<Vec<_>>();
    assert_eq!(
        f64_source_sample_bits, f64_streamed_sample_bits,
        "f64 W64 to streamed f64 WAV re-container changed decoded sample bits"
    );

    // Float32 W64: retain the qualified direct FFmpeg decode seam because SoX-ng
    // mis-scales this carrier. Policy v15 pipes headerless f64le into SoX-ng,
    // which creates and measures the same 16x view used by all other depths.
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
    let f32_producer = f32_post
        .input_stage
        .as_ref()
        .expect("v15 Float32 post measurement has an FFmpeg raw producer");
    assert_eq!(f32_producer.tool, ToolIdentifier::Ffmpeg);
    assert_eq!(f32_producer.input.as_path(), Some(f32_summary.qpcm_path.as_path()));
    assert_eq!(f32_post.command.tool, ToolIdentifier::Sox);
    assert_eq!(f32_post.command.input, tonepoet_pipeline::InputSource::Stdin);
    assert_eq!(f32_post.parser, MeasurementParser::SoxStatsPkLevDbV1);
    let f32_direct = execute_measurement(f32_summary, f32_post, &sox, &ffmpeg, &f32_root, 1);
    let f32_direct_input_tp = match f32_direct.reported {
        TruePeakValue::Finite(value) => value.0 as f64 / 1_000_000_000.0,
        TruePeakValue::VerifiedSilence => panic!("-20 dB Float32 carrier measured as silence"),
    };
    assert!(
        (f32_direct_input_tp - f32_analytic_peak).abs() <= 0.02,
        "v15 Float32 FFmpeg-to-SoX measurement changed true peak: analytic={f32_analytic_peak}, measured={f32_direct_input_tp}"
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
    let bytes_per_sample = tonepoet_pipeline::REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
    let largest_admitted_payload =
        ReferenceStreamedWavCapacityEvidenceV3::largest_frame_aligned_admitted_payload();
    let first_policy_rejected_payload = largest_admitted_payload
        .checked_add(bytes_per_sample)
        .expect("first rejected payload arithmetic does not overflow");
    let data_wrap_payload = ReferenceStreamedWavCapacityEvidenceV3::DATA_WRAP_PAYLOAD_BYTES;
    let transition_count = usize::try_from(
        ReferenceStreamedWavCapacityEvidenceV3::expected_transition_count(),
    )
    .expect("transition scan length fits usize");
    assert_eq!(transition_count, 9);

    let stream_header_bytes = ReferenceStreamedWavCapacityEvidenceV3::STREAM_HEADER_BYTES;
    assert_eq!(stream_header_bytes, 58);

    let mut transition_scan = Vec::with_capacity(transition_count);
    for offset_frames in 0..transition_count {
        let payload_bytes = largest_admitted_payload
            .checked_add(
                u64::try_from(offset_frames)
                    .expect("transition offset fits u64")
                    .checked_mul(bytes_per_sample)
                    .expect("transition payload arithmetic does not overflow"),
            )
            .expect("transition payload arithmetic does not overflow");
        let sparse = root.join(format!("stream-capacity-edge-{offset_frames:02}.w64"));
        let sample_frames =
            create_sparse_w64_capacity_fixture(&f64_summary.r64_path, &sparse, payload_bytes);
        let mut producer = f64_producer.clone();
        producer.input = tonepoet_pipeline::InputSource::Path(sparse.clone());
        producer.args[2] = sparse.display().to_string();

        let sparse_info = run(
            &sox,
            &["--i".to_string(), "-s".to_string(), sparse.display().to_string()],
        );
        assert!(
            sparse_info.status.success(),
            "SoX-ng could not read sparse W64 capacity fixture {offset_frames}: {}",
            String::from_utf8_lossy(&sparse_info.stderr)
        );
        let reported_frames = String::from_utf8_lossy(&sparse_info.stdout)
            .trim()
            .parse::<u64>()
            .expect("SoX-ng reports an integer sparse-fixture sample count");
        assert_eq!(reported_frames, sample_frames);
        assert_eq!(sample_frames, payload_bytes / bytes_per_sample);

        let (observed_riff_size_field, observed_data_size_field, observed_header_bytes) =
            inspect_streaming_wav_header(&producer, &sox);
        assert_eq!(
            observed_header_bytes,
            usize::try_from(stream_header_bytes).expect("streamed header size fits usize"),
        );
        let structural_riff_size = payload_bytes
            .checked_add(tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES)
            .expect("structural RIFF size does not overflow");
        let structural_riff_size_representable =
            structural_riff_size <= tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX;
        let header_fields_exact = structural_riff_size_representable
            && observed_riff_size_field
                == u32::try_from(structural_riff_size)
                    .expect("representable structural RIFF size fits u32")
            && observed_data_size_field
                == u32::try_from(payload_bytes).expect("admitted payload fits u32");

        let (planner_admission, planner_error_code) =
            match capacity_boundary_plan_result(&root, &f64_source, sample_frames) {
                Ok(_) => ("accepted".to_string(), None),
                Err(error) => {
                    let message = error.to_string();
                    assert!(
                        message.contains("DSD-REF-P0-025"),
                        "capacity rejection did not carry the stable policy error: {message}"
                    );
                    (
                        "rejected".to_string(),
                        Some(ReferenceStreamedWavCapacityEvidenceV3::ERROR_CODE.to_string()),
                    )
                }
            };
        if offset_frames == 0 {
            assert_eq!(planner_admission, "accepted");
            assert!(
                header_fields_exact,
                "the largest admitted carrier must have exact, nonwrapped RIFF and data fields: riff={observed_riff_size_field}, data={observed_data_size_field}, structural_riff={structural_riff_size}, payload={payload_bytes}"
            );
        } else {
            assert_eq!(planner_admission, "rejected");
        }

        transition_scan.push(ReferenceStreamedWavBoundaryObservationV2 {
            sample_frames,
            audio_payload_bytes: payload_bytes,
            observed_riff_size_field,
            observed_data_size_field,
            structural_riff_size,
            structural_riff_size_representable,
            header_fields_exact,
            planner_admission,
            planner_error_code,
        });
    }

    let accepted_edge = transition_scan
        .first()
        .expect("transition scan contains accepted edge")
        .clone();
    let first_policy_rejected_edge = transition_scan
        .get(1)
        .expect("transition scan contains first rejected edge")
        .clone();
    assert_eq!(accepted_edge.audio_payload_bytes, largest_admitted_payload);
    assert_eq!(first_policy_rejected_edge.audio_payload_bytes, first_policy_rejected_payload);
    assert!(accepted_edge.structural_riff_size_representable);
    assert!(accepted_edge.header_fields_exact);
    assert!(!first_policy_rejected_edge.structural_riff_size_representable);
    assert!(!first_policy_rejected_edge.header_fields_exact);

    let first_observed_riff_wrap_offset_frames = transition_scan
        .windows(2)
        .position(|pair| {
            pair[1].observed_riff_size_field < pair[0].observed_riff_size_field
        })
        .map(|index| u64::try_from(index + 1).expect("wrap offset fits u64"))
        .expect("the pinned writer must exhibit a RIFF-size field wrap in the boundary scan");
    assert!(first_observed_riff_wrap_offset_frames >= 1);

    let data_wrap = transition_scan
        .last()
        .expect("transition scan contains data-wrap witness")
        .clone();
    let expected_modulo_data_size_field = u32::try_from(
        data_wrap_payload & u64::from(u32::MAX),
    )
    .expect("modulo-2^32 data size fits u32");
    assert_eq!(data_wrap.audio_payload_bytes, data_wrap_payload);
    assert_eq!(data_wrap.sample_frames, 536_870_913);
    assert_eq!(data_wrap.observed_riff_size_field, 58);
    assert_eq!(expected_modulo_data_size_field, 8);
    assert_eq!(data_wrap.observed_data_size_field, expected_modulo_data_size_field);

    let streamed_wav_capacity = ReferenceStreamedWavCapacityEvidenceV3 {
        status: "passed".to_string(),
        contract: ReferenceStreamedWavCapacityEvidenceV3::CONTRACT.to_string(),
        sparse_source_container: "w64".to_string(),
        sample_rate_hz: ReferenceStreamedWavCapacityEvidenceV3::SAMPLE_RATE_HZ,
        channels: ReferenceStreamedWavCapacityEvidenceV3::CHANNELS,
        sample_encoding: "pcm_f64le".to_string(),
        bytes_per_sample,
        riff_size_field_max: tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX,
        riff_size_overhead_bytes:
            tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
        max_audio_payload_bytes:
            tonepoet_pipeline::REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
        duration_guard_frames:
            tonepoet_pipeline::REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
        stream_header_bytes,
        accepted_edge,
        first_policy_rejected_edge,
        transition_scan,
        first_observed_riff_wrap_offset_frames,
        data_wrap_witness: ReferenceStreamedWavDataWrapWitnessV2 {
            sample_frames: data_wrap.sample_frames,
            audio_payload_bytes: data_wrap.audio_payload_bytes,
            observed_riff_size_field: data_wrap.observed_riff_size_field,
            observed_data_size_field: data_wrap.observed_data_size_field,
            expected_modulo_data_size_field,
            wrapped_header_is_sentinel: false,
            consumer_completeness_claim: false,
        },
        error_code: ReferenceStreamedWavCapacityEvidenceV3::ERROR_CODE.to_string(),
    };

    serde_json::json!({
        "status": "passed",
        "contract": "tonepoet-reference-analyzer-carrier/v4",
        "routing_rule": "float32_w64_ffmpeg_f64le_raw_to_sox_else_sox_path",
        "known_defect": {
            "status": "reproduced",
            "carrier": "sox_f64_w64_direct_to_ffmpeg_7_1",
            "analytic_peak_dbfs": f64_analytic_peak,
            "reported_input_tp_dbtp": f64_direct_input_tp,
            "scaling_delta_db": f64_defect_delta_db,
            "expected_scaling": "2^31",
        },
        "silent_w64_header_finalization_defect": silent_w64_header_finalization_defect,
        "direct_sox_path": {
            "status": "passed",
            "carrier_depth": "float64",
            "reported_peak_dbtp": f64_corrected_input_tp,
            "analytic_peak_dbfs": f64_analytic_peak,
            "parser": "sox_stats_pk_lev_db_v1",
            "oversample_factor": REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR,
            "analytic_grid_bound_db": REFERENCE_TRUE_PEAK_GRID_BOUND.render(false),
            "environment_policy": "clear_and_set",
            "environment": {"LC_ALL": "C"},
            "command_argv": f64_measurement.command.args.clone(),
        },
        "float32_pipe_path": {
            "status": "passed",
            "carrier_depth": "float32",
            "carrier_container": "w64",
            "disk_intermediate": false,
            "package_step": false,
            "reported_peak_dbtp": f32_direct_input_tp,
            "analytic_peak_dbfs": f32_analytic_peak,
            "parser": "sox_stats_pk_lev_db_v1",
            "environment_policy": "clear_and_set",
            "environment": {"LC_ALL": "C"},
            "producer_argv": f32_producer.args.clone(),
            "consumer_argv": f32_post.command.args.clone(),
        },
        "historical_streamed_wav_capacity_probe": {
            "status": "retained_conservative_admission_witness",
            "producer_argv": f64_producer.args.clone(),
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
        "streamed_wav_capacity": streamed_wav_capacity,
    })
}

fn run_planned_measurement(
    measurement: &tonepoet_pipeline::PlannedMeasurement,
    sox: &Path,
    ffmpeg: &Path,
) -> PlannedMeasurementOutput {
    assert_eq!(measurement.parser, MeasurementParser::SoxStatsPkLevDbV1);
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
        assert_eq!(measurement.command.tool, ToolIdentifier::Sox);
        assert_eq!(measurement.command.input.as_path(), measurement.carrier_path());
        PlannedMeasurementOutput {
            producer: None,
            consumer: run(sox, &measurement.command.args),
        }
    }
}

fn policy_measurement_bounds() -> (DbNano, DbNano) {
    let qualification: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
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
    summary: &tonepoet_pipeline::DsdReferencePlanSummary,
    measurement: &tonepoet_pipeline::PlannedMeasurement,
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
    channels: u16,
) -> TruePeakMeasurement {
    let output = run_planned_measurement(measurement, sox, ffmpeg);
    if let Some(producer) = &output.producer {
        assert!(producer.stdout.is_empty());
    }
    let stderr = String::from_utf8_lossy(&output.consumer.stderr);
    let raw = extract_single_sox_stats_peak_report(&stderr, channels)
        .unwrap_or_else(|error| panic!("production SoX stats extraction failed: {error}"));
    let silence = raw == "-inf";
    if silence {
        let selector = match measurement.purpose {
            TruePeakPurpose::GainAuthority => ReferenceDecodedCarrierSelector::ReconstructionR64,
            TruePeakPurpose::PostFinalAcceptance => ReferenceDecodedCarrierSelector::TerminalQpcm,
        };
        let carrier = summary
            .decoded_carrier(selector)
            .expect("measurement silence carrier has an admitted decode route");
        assert_eq!(carrier.path(), measurement.carrier_path().unwrap());
        let raw_path = root.join(format!("silence-{}.f64le", measurement.id.0));
        let scan = build_reference_silence_scan_command(&carrier, &raw_path);
        run_planned_command(&scan, sox, ffmpeg);
        let bytes = fs::read(&raw_path).expect("read production silence scan");
        validate_signed_zero_f64le(&bytes).expect("production signed-zero proof");
        fs::remove_file(raw_path).expect("remove silence scan");
    }
    let (q, e) = policy_measurement_bounds();
    let parsed = parse_reference_sox_stats_true_peak_measurement(
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
                let parsed = execute_measurement(
                    summary,
                    measurement,
                    sox,
                    ffmpeg,
                    root,
                    summary.final_pcm.channels,
                );
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

fn decode_f64le_samples(output: &Output, route: ReferenceDecodeMechanism) -> Vec<f64> {
    assert!(
        !output.stdout.is_empty(),
        "{route:?} produced no decoded samples"
    );
    assert_eq!(
        output.stdout.len() % 8,
        0,
        "{route:?} produced a truncated f64le stream"
    );
    output
        .stdout
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("f64 sample width")))
        .collect()
}

fn direct_ffmpeg_f64_samples(ffmpeg: &Path, input: &Path) -> Vec<f64> {
    let route = ReferenceDecodeMechanism::DirectFfmpeg;
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
            "pcm_f64le".to_string(),
            "-f".to_string(),
            "f64le".to_string(),
            "-".to_string(),
        ],
    );
    decode_f64le_samples(&output, route)
}

fn streamed_float64_w64_f64_samples(sox: &Path, input: &Path) -> Vec<f64> {
    let route = ReferenceDecodeMechanism::SoxFloat64W64RawStream;
    let output = run(
        sox,
        &[
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
        ],
    );
    decode_f64le_samples(&output, route)
}

fn decoded_f64_samples(
    carrier: &ReferenceDecodedCarrier,
    sox: &Path,
    ffmpeg: &Path,
) -> Vec<f64> {
    match carrier.authority().mechanism() {
        ReferenceDecodeMechanism::DirectFfmpeg => {
            direct_ffmpeg_f64_samples(ffmpeg, carrier.path())
        }
        ReferenceDecodeMechanism::SoxFloat64W64RawStream => {
            streamed_float64_w64_f64_samples(sox, carrier.path())
        }
    }
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
    ffmpeg: &Path,
    summary: &tonepoet_pipeline::DsdReferencePlanSummary,
    terminal_args: &[String],
) -> f64 {
    let gain_db = gain_arg(terminal_args)
        .expect("Reference terminal command has one gain")
        .parse::<f64>()
        .expect("Reference gain token parses as f64");
    let gain = 10_f64.powf(gain_db / 20.0);
    let input_carrier = summary
        .decoded_carrier(ReferenceDecodedCarrierSelector::ReconstructionR64)
        .expect("qualified R64 carrier binding");
    let output_carrier = summary
        .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
        .expect("qualified QPCM carrier binding");
    let input = decoded_f64_samples(&input_carrier, sox, ffmpeg);
    let output = decoded_f64_samples(&output_carrier, sox, ffmpeg);
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

fn known_defective_w64_metadata_remux_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
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
        "-f".to_string(),
        "w64".to_string(),
        output.display().to_string(),
    ]
}


fn deterministic_int24_mono_bytes(sample_count: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(sample_count * 3);
    for index in 0..sample_count {
        let unsigned = ((index as u32).wrapping_mul(7_919).wrapping_add(1_337)) & 0x00ff_ffff;
        let value = unsigned as i32 - 0x0080_0000;
        let encoded = value.to_le_bytes();
        bytes.extend_from_slice(&encoded[..3]);
    }
    bytes
}

fn sox_raw_int24_mono_container(
    sox: &Path,
    raw: &Path,
    output: &Path,
    output_type: &str,
) {
    run(
        sox,
        &[
            "-S".to_string(),
            "-D".to_string(),
            "-t".to_string(),
            "raw".to_string(),
            "-e".to_string(),
            "signed-integer".to_string(),
            "-b".to_string(),
            "24".to_string(),
            "-L".to_string(),
            "-r".to_string(),
            "88200".to_string(),
            "-c".to_string(),
            "1".to_string(),
            raw.display().to_string(),
            "-t".to_string(),
            output_type.to_string(),
            "-e".to_string(),
            "signed-integer".to_string(),
            "-b".to_string(),
            "24".to_string(),
            output.display().to_string(),
        ],
    );
}

fn ffmpeg_decode_int24_bytes(ffmpeg: &Path, input: &Path) -> Vec<u8> {
    run(
        ffmpeg,
        &[
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-f".to_string(),
            "s24le".to_string(),
            "-c:a".to_string(),
            "pcm_s24le".to_string(),
            "pipe:1".to_string(),
        ],
    )
    .stdout
}

fn qualify_alignment_metadata_mutation_probes(
    sox: &Path,
    ffmpeg: &Path,
    root: &Path,
    runtime: &tokio::runtime::Runtime,
    metadata_runner: &RealToolRunner,
    track_metadata: &TrackMetadata,
    album_metadata: &AlbumMetadata,
) -> Value {
    let probe_root = root.join("metadata-alignment-probes");
    fs::create_dir_all(&probe_root).expect("create metadata alignment probe root");

    let w64_raw = probe_root.join("w64-int24-mono.raw");
    let w64_original = probe_root.join("w64-int24-mono.w64");
    let w64_rewrite = probe_root.join("w64-int24-mono-tagged.w64");
    let w64_expected = deterministic_int24_mono_bytes(8_820);
    assert_eq!(w64_expected.len(), 26_460);
    assert_ne!(w64_expected.len() % 8, 0);
    fs::write(&w64_raw, &w64_expected).expect("write W64 alignment probe raw PCM");
    sox_raw_int24_mono_container(sox, &w64_raw, &w64_original, "w64");
    run(
        ffmpeg,
        &known_defective_w64_metadata_remux_args(&w64_original, &w64_rewrite),
    );
    let w64_original_decoded = ffmpeg_decode_int24_bytes(ffmpeg, &w64_original);
    let w64_rewrite_decoded = ffmpeg_decode_int24_bytes(ffmpeg, &w64_rewrite);
    assert_eq!(w64_original_decoded, w64_expected);
    assert_eq!(w64_rewrite_decoded.len(), w64_expected.len() + 3);
    assert_eq!(&w64_rewrite_decoded[..w64_expected.len()], w64_expected.as_slice());
    assert_eq!(&w64_rewrite_decoded[w64_expected.len()..], &[0, 0, 0]);

    let riff_raw = probe_root.join("riff-int24-mono.raw");
    let riff_original = probe_root.join("riff-int24-mono.wav");
    let riff_expected = deterministic_int24_mono_bytes(8_821);
    assert_eq!(riff_expected.len(), 26_463);
    assert_eq!(riff_expected.len() % 2, 1);
    fs::write(&riff_raw, &riff_expected).expect("write RIFF alignment probe raw PCM");
    sox_raw_int24_mono_container(sox, &riff_raw, &riff_original, "wav");
    let riff_original_decoded = ffmpeg_decode_int24_bytes(ffmpeg, &riff_original);
    assert_eq!(riff_original_decoded, riff_expected);
    let outcome = runtime
        .block_on(qualify_production_metadata_mutation(
            &riff_original,
            track_metadata,
            album_metadata,
            metadata_runner,
            &CancellationToken::new(),
        ))
        .expect("production RIFF metadata mutation probe");
    assert_eq!(outcome.primary_mutator, Some(ToolBinary::Ffmpeg));
    assert!(!outcome.m4a_freeform_mutator_applied);
    let riff_post_metadata_decoded = ffmpeg_decode_int24_bytes(ffmpeg, &riff_original);
    assert_eq!(riff_post_metadata_decoded, riff_expected);

    serde_json::json!({
        "schema": "tonepoet-reference-metadata-alignment-probes/v1",
        "status": "passed",
        "w64_non_8_aligned_int24_mono": {
            "sample_rate_hz": 88200,
            "channels": 1,
            "sample_count_before": 8820,
            "sample_count_after_ffmpeg_w64_remux": 8821,
            "data_bytes_before": 26460,
            "decoded_prefix_identity": "passed",
            "phantom_trailing_sample": "000000",
            "disposition": "known_muxer_defect_route_rejected",
            "rejection_code": "DSD-REF-P0-024"
        },
        "riff_odd_byte_int24_mono": {
            "sample_rate_hz": 88200,
            "channels": 1,
            "sample_count": 8821,
            "data_bytes": 26463,
            "post_metadata_sample_identity": "passed",
            "production_entry_point": "qualify_production_metadata_mutation",
            "production_primary_mutator": "ffmpeg",
            "disposition": "qualified"
        }
    })
}

struct PackageQualificationEvidence {
    case_count: usize,
    terminal_bound_case_count: usize,
    terminal_observed_max_error_by_depth: BTreeMap<String, f64>,
    sample_identity_oracle: Value,
}

fn record_decode_authority(
    route_counts: &mut BTreeMap<String, usize>,
    encoding_counts: &mut BTreeMap<String, usize>,
    phase: &str,
    authority: ReferenceDecodeAuthority,
) {
    assert_eq!(authority.hash_format(), REFERENCE_SAMPLE_HASH_FORMAT);
    *route_counts
        .entry(format!("{phase}:{}", authority.mechanism().key()))
        .or_default() += 1;
    *encoding_counts
        .entry(format!("{phase}:{}", authority.hash_encoding().key()))
        .or_default() += 1;
}

fn qualify_lossless_package_cells(
    forbidden_route_regression: Value,
) -> PackageQualificationEvidence {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let metaflac = required_tool(METAFLAC_ENV);
    let wvtag = required_tool(WVTAG_ENV);
    let atomic_parsley = required_tool(ATOMIC_PARSLEY_ENV);
    let ffprobe = required_sibling_tool(&ffmpeg, "ffprobe");
    let temp = TempDir::new().expect("package qualification tempdir");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("production metadata qualification runtime");
    let metadata_runner = production_metadata_runner(&ffmpeg, &metaflac, &wvtag, &atomic_parsley);
    let (track_metadata, album_metadata) = qualification_metadata();
    let planner_mono_source = temp.path().join("planner-mono.dsf");
    let planner_stereo_source = temp.path().join("planner-stereo.dsf");
    write_dsf_reference_fixture(&planner_mono_source, 1, 2_822_400);
    write_dsf_reference_fixture(&planner_stereo_source, 2, 2_822_400);

    let mut case_count = 0_usize;
    let mut terminal_bound_cells = BTreeSet::new();
    let mut route_counts = BTreeMap::<String, usize>::new();
    let mut encoding_counts = BTreeMap::<String, usize>::new();
    let mut terminal_route_counts = BTreeMap::<String, usize>::new();
    let mut terminal_observed_max_error_by_depth = BTreeMap::<String, f64>::new();
    let mut production_primary_mutator_case_counts = BTreeMap::<String, usize>::new();
    let mut production_m4a_freeform_case_count = 0_usize;
    let mut independent_float64_riff_rf64_case_count = 0_usize;
    let mut package_identity_comparison_count = 0_usize;
    let mut w64_direct_delivery_exact_validation_count = 0_usize;
    let mut post_metadata_identity_comparison_count = 0_usize;
    let mut w64_planner_entry_rejection_count = 0_usize;
    let mut w64_metadata_entry_rejection_count = 0_usize;
    let alignment_probes = qualify_alignment_metadata_mutation_probes(
        &sox,
        &ffmpeg,
        temp.path(),
        &runtime,
        &metadata_runner,
        &track_metadata,
        &album_metadata,
    );

    let rates = [
        44_100_u32, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    let depths = [
        (PcmBitDepth::Int24, "int24"),
        (PcmBitDepth::Float32, "float32"),
        (PcmBitDepth::Float64, "float64"),
    ];

    for sample_rate_hz in rates {
        for channels in [1_u16, 2_u16] {
            for (depth, depth_key) in depths {
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
                            let r64_authority = r64_decode_authority(summary.final_pcm);
                            let terminal_qpcm_authority =
                                qpcm_decode_authority(summary.final_pcm);
                            *terminal_route_counts
                                .entry(format!(
                                    "r64:{}",
                                    r64_authority.mechanism().key(),
                                ))
                                .or_default() += 1;
                            *terminal_route_counts
                                .entry(format!(
                                    "qpcm:{}",
                                    terminal_qpcm_authority.mechanism().key(),
                                ))
                                .or_default() += 1;
                            let observed = assert_terminal_realization_bound(
                                &sox,
                                &ffmpeg,
                                summary,
                                &chain.terminal_args,
                            );
                            assert!(observed.is_finite());
                            terminal_observed_max_error_by_depth
                                .entry(depth_key.to_string())
                                .and_modify(|maximum| *maximum = (*maximum).max(observed))
                                .or_insert(observed);
                        }
                        let packaged = &summary.packaged_path;
                        assert_exact_package_probe(
                            &sox,
                            &ffprobe,
                            packaged,
                            target_key(target),
                            depth_key,
                            sample_rate_hz,
                            channels,
                            if target == ResolvedOutputTarget::WavW64 {
                                Some(exact_w64_frame_count(
                                    &summary.r64_path,
                                    summary.final_pcm.sample_rate_hz,
                                    summary.final_pcm.channels,
                                    64,
                                    W64SampleEncoding::FloatingPoint,
                                ))
                            } else {
                                None
                            },
                        );
                        let qpcm_authority = qpcm_decode_authority(summary.final_pcm);
                        let packaged_authority =
                            packaged_decode_authority(target, summary.final_pcm);
                        record_decode_authority(
                            &mut route_counts,
                            &mut encoding_counts,
                            "qpcm",
                            qpcm_authority,
                        );
                        record_decode_authority(
                            &mut route_counts,
                            &mut encoding_counts,
                            "packaged",
                            packaged_authority,
                        );
                        if depth == PcmBitDepth::Float64
                            && matches!(
                                target,
                                ResolvedOutputTarget::WavRiff | ResolvedOutputTarget::WavRf64
                            )
                        {
                            assert_eq!(
                                qpcm_authority.mechanism(),
                                ReferenceDecodeMechanism::SoxFloat64W64RawStream
                            );
                            assert_eq!(
                                packaged_authority.mechanism(),
                                ReferenceDecodeMechanism::DirectFfmpeg
                            );
                            independent_float64_riff_rf64_case_count += 1;
                        }
                        let qpcm_carrier = summary
                            .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
                            .expect("qualified QPCM carrier binding");
                        let packaged_carrier = summary
                            .decoded_carrier(ReferenceDecodedCarrierSelector::PackagedOutput)
                            .expect("qualified packaged carrier binding");
                        assert_eq!(qpcm_carrier.authority(), qpcm_authority);
                        assert_eq!(packaged_carrier.authority(), packaged_authority);
                        let qpcm_hash = decoded_sample_hash(&qpcm_carrier, &sox, &ffmpeg);
                        if target == ResolvedOutputTarget::WavW64 {
                            assert_eq!(summary.qpcm_path, summary.packaged_path);
                            assert_eq!(qpcm_carrier.path(), packaged_carrier.path());
                            // No package transform exists for W64. The exact parser,
                            // exact frame authority, and full FFmpeg traversal above
                            // qualify direct delivery; comparing this path with itself
                            // is explicitly not independent packaging evidence.
                            w64_direct_delivery_exact_validation_count += 1;
                        } else {
                            let packaged_hash =
                                decoded_sample_hash(&packaged_carrier, &sox, &ffmpeg);
                            assert_eq!(
                                packaged_hash,
                                qpcm_hash,
                                "decoded samples changed for {}",
                                case_root.display()
                            );
                            package_identity_comparison_count += 1;
                        }
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

                        if target == ResolvedOutputTarget::WavW64 {
                            let rejection = tonepoet_pipeline::reference_error_text(
                                ReferenceErrorCode::W64MetadataMutationUnqualified,
                            );
                            assert_eq!(
                                tonepoet_pipeline::reference_metadata_mutation_rejection(target),
                                Some(rejection),
                            );

                            let planner_source = if channels == 1 {
                                &planner_mono_source
                            } else {
                                &planner_stereo_source
                            };
                            let planner_output = case_root.join("planner-rejected.w64");
                            let planner_work = case_root.join("planner-rejected-work");
                            let planner_request =
                                w64_planner_request(&case_root, sample_rate_hz, depth);
                            let planner_track = w64_planner_track(planner_source, channels);
                            let planner_error = plan_request_for_track(
                                &planner_request,
                                &planner_track,
                                planner_source,
                                &planner_output,
                                planner_work.clone(),
                            )
                            .expect_err("W64 metadata admission must fail at the production planner entry");
                            assert_eq!(
                                planner_error.to_string(),
                                format!("backend encode failed: {rejection}"),
                            );
                            assert!(!planner_output.exists());
                            assert!(!planner_work.exists());
                            w64_planner_entry_rejection_count += 1;

                            let metadata_error = runtime
                                .block_on(qualify_production_metadata_mutation(
                                    packaged,
                                    &track_metadata,
                                    &album_metadata,
                                    &metadata_runner,
                                    &CancellationToken::new(),
                                ))
                                .expect_err("W64 metadata admission must fail at the production metadata entry");
                            assert_eq!(metadata_error.to_string(), rejection);
                            w64_metadata_entry_rejection_count += 1;
                        } else {
                            let outcome = runtime
                                .block_on(qualify_production_metadata_mutation(
                                    packaged,
                                    &track_metadata,
                                    &album_metadata,
                                    &metadata_runner,
                                    &CancellationToken::new(),
                                ))
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "production metadata mutation failed for {}: {error}",
                                        case_root.display()
                                    )
                                });
                            let expected_mutator = match target {
                                ResolvedOutputTarget::FlacNative => ToolBinary::Metaflac,
                                ResolvedOutputTarget::WavPackNative => ToolBinary::Wvtag,
                                ResolvedOutputTarget::WavRiff
                                | ResolvedOutputTarget::WavRf64
                                | ResolvedOutputTarget::AiffNative
                                | ResolvedOutputTarget::AlacM4a => ToolBinary::Ffmpeg,
                                ResolvedOutputTarget::WavW64 => unreachable!(),
                                // The qualification loop iterates only the
                                // seven enabled Reference targets above.
                                other => unreachable!(
                                    "non-Reference target {other:?} in metadata mutator qualification"
                                ),
                            };
                            assert_eq!(outcome.primary_mutator, Some(expected_mutator));
                            *production_primary_mutator_case_counts
                                .entry(expected_mutator.canonical_name().to_string())
                                .or_default() += 1;
                            if target == ResolvedOutputTarget::AlacM4a {
                                assert!(outcome.m4a_freeform_mutator_applied);
                                production_m4a_freeform_case_count += 1;
                            } else {
                                assert!(!outcome.m4a_freeform_mutator_applied);
                            }

                            assert_exact_package_probe(
                                &sox,
                                &ffprobe,
                                packaged,
                                target_key(target),
                                depth_key,
                                sample_rate_hz,
                                channels,
                                if target == ResolvedOutputTarget::WavW64 {
                                    Some(exact_w64_frame_count(
                                    &summary.r64_path,
                                    summary.final_pcm.sample_rate_hz,
                                    summary.final_pcm.channels,
                                    64,
                                    W64SampleEncoding::FloatingPoint,
                                ))
                                } else {
                                    None
                                },
                            );
                            let post_metadata_authority =
                                post_metadata_decode_authority(target, summary.final_pcm);
                            record_decode_authority(
                                &mut route_counts,
                                &mut encoding_counts,
                                "post_metadata",
                                post_metadata_authority,
                            );
                            let mut post_metadata_summary = summary.clone();
                            post_metadata_summary.delivered_path = packaged.clone();
                            let post_metadata_carrier = post_metadata_summary
                                .bind_decoded_carrier(
                                    ReferenceDecodedCarrierSelector::PostMetadataOutput,
                                    packaged,
                                )
                                .expect("qualified production post-metadata carrier binding");
                            assert_eq!(
                                post_metadata_carrier.authority(),
                                post_metadata_authority
                            );
                            let post_metadata_hash =
                                decoded_sample_hash(&post_metadata_carrier, &sox, &ffmpeg);
                            assert_eq!(
                                post_metadata_hash,
                                qpcm_hash,
                                "production metadata mutation changed decoded samples for {}",
                                case_root.display()
                            );
                            post_metadata_identity_comparison_count += 1;
                        }
                        let mut generated = BTreeSet::from([
                            summary.r64_path.clone(),
                            summary.qpcm_path.clone(),
                            summary.packaged_path.clone(),
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
    assert_eq!(package_identity_comparison_count, 420);
    assert_eq!(w64_direct_delivery_exact_validation_count, 60);
    assert_eq!(post_metadata_identity_comparison_count, 420);
    assert_eq!(w64_planner_entry_rejection_count, 60);
    assert_eq!(w64_metadata_entry_rejection_count, 60);
    assert_eq!(
        production_primary_mutator_case_counts,
        BTreeMap::from([
            ("ffmpeg".to_string(), 160),
            ("metaflac".to_string(), 180),
            ("wvtag".to_string(), 80),
        ])
    );
    assert_eq!(production_m4a_freeform_case_count, 20);
    assert_eq!(independent_float64_riff_rf64_case_count, 40);
    assert_eq!(
        terminal_route_counts,
        BTreeMap::from([
            ("qpcm:ffmpeg_direct".to_string(), 40),
            ("qpcm:sox_f64le_raw_stream".to_string(), 20),
            ("r64:sox_f64le_raw_stream".to_string(), 60),
        ])
    );
    assert_eq!(
        route_counts,
        BTreeMap::from([
            ("packaged:ffmpeg_direct".to_string(), 460),
            ("packaged:sox_f64le_raw_stream".to_string(), 20),
            ("post_metadata:ffmpeg_direct".to_string(), 420),
            ("qpcm:ffmpeg_direct".to_string(), 420),
            ("qpcm:sox_f64le_raw_stream".to_string(), 60),
        ])
    );
    assert_eq!(
        encoding_counts,
        BTreeMap::from([
            ("packaged:float32_le".to_string(), 60),
            ("packaged:float64_le".to_string(), 60),
            ("packaged:int24_le".to_string(), 360),
            ("post_metadata:float32_le".to_string(), 40),
            ("post_metadata:float64_le".to_string(), 40),
            ("post_metadata:int24_le".to_string(), 340),
            ("qpcm:float32_le".to_string(), 60),
            ("qpcm:float64_le".to_string(), 60),
            ("qpcm:int24_le".to_string(), 360),
        ])
    );

    assert_eq!(
        terminal_observed_max_error_by_depth.keys().cloned().collect::<Vec<_>>(),
        vec!["float32".to_string(), "float64".to_string(), "int24".to_string()]
    );

    PackageQualificationEvidence {
        case_count,
        terminal_bound_case_count: terminal_bound_cells.len(),
        terminal_observed_max_error_by_depth,
        sample_identity_oracle: serde_json::json!({
            "schema": "tonepoet-reference-sample-identity-oracle/v4",
            "status": "passed",
            "route_authority": "typed_plan_carrier_path_role_target_depth_v2",
            "hash_format": REFERENCE_SAMPLE_HASH_FORMAT,
            "hash_codecs": {
                "int24": ReferenceSampleHashEncoding::SignedInt24Le.ffmpeg_codec(),
                "float32": ReferenceSampleHashEncoding::Float32Le.ffmpeg_codec(),
                "float64": ReferenceSampleHashEncoding::Float64Le.ffmpeg_codec(),
            },
            "measured_route_case_counts": route_counts,
            "measured_hash_encoding_case_counts": encoding_counts,
            "measured_terminal_realization_route_case_counts": terminal_route_counts,
            "package_identity_comparison_count": package_identity_comparison_count,
            "w64_direct_delivery_exact_validation_count": w64_direct_delivery_exact_validation_count,
            "w64_same_path_hash_counted_as_independent_packaging": false,
            "post_metadata_identity_comparison_count": post_metadata_identity_comparison_count,
            "production_metadata_mutation": {
                "schema": "tonepoet-reference-production-metadata-mutation/v1",
                "entry_point": "tonepoet::convert::pipeline::qualify_production_metadata_mutation",
                "shared_production_implementation": "apply_production_metadata_to_file",
                "authoritative_tag_source": "authoritative_metadata_tags",
                "qualification_scope": "authoritative_tag_mutation_without_artwork_or_replaygain",
                "environment_policy": "clear_and_set",
                "environment": {"LC_ALL": "C"},
                "admitted_cell_count": post_metadata_identity_comparison_count,
                "primary_mutator_case_counts": production_primary_mutator_case_counts,
                "m4a_atomicparsley_freeform_case_count": production_m4a_freeform_case_count,
                "post_mutation_sample_identity_count": post_metadata_identity_comparison_count,
                "post_mutation_container_contract_rechecked": true,
                "rf64_preservation": "source_magic_RF64_requires_ffmpeg_-rf64_always",
                "w64_rejection": {
                    "planner_entry_point": "plan_request_for_track",
                    "planner_case_count": w64_planner_entry_rejection_count,
                    "metadata_entry_point": "qualify_production_metadata_mutation",
                    "metadata_case_count": w64_metadata_entry_rejection_count,
                    "code": "DSD-REF-P0-024"
                }
            },
            "metadata_alignment_probes": alignment_probes,
            "independent_float64_riff_rf64_case_count": independent_float64_riff_rf64_case_count,
            "forbidden_float64_w64_direct_route_regression": forbidden_route_regression,
            "oracle_independence":
                "float64_w64_source_sox_decode_vs_riff_rf64_output_ffmpeg_decode",
        }),
    }
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
    const FIXED_FREQUENCIES_HZ: [u32; 4] = [1_000, 20_000, 48_000, 70_000];
    const FIXED_FREQUENCY_MAX_NORMALIZED: f64 = 0.49;
    const FIXED_FREQUENCY_DURATION_SECONDS: f64 = 0.250;
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
    // 32 valid rate/frequency cells: 1/20 kHz at all ten rates, plus
    // 48/70 kHz at the six rates from 176.4 through 768 kHz.
    const FIXED_FREQUENCY_CASE_COUNT: usize = 32
        * CHANNELS.len()
        * PHASES.len()
        * TRUE_PEAK_LEVELS_DBFS.len()
        * PEAK_POSITIONS.len();
    const MULTITONE_CASE_COUNT: usize = RATES.len()
        * CHANNELS.len()
        * MULTITONE_PEAK_OFFSETS.len()
        * TRUE_PEAK_LEVELS_DBFS.len()
        * PEAK_POSITIONS.len();
    const ADVERSARIAL_CASE_COUNT: usize = RATES.len()
        * CHANNELS.len()
        * AdversarialAnalyzerFixture::ALL.len()
        * PEAK_POSITIONS.len();
    const ANALYTIC_CASE_COUNT: usize =
        SINGLE_TONE_CASE_COUNT + FIXED_FREQUENCY_CASE_COUNT + MULTITONE_CASE_COUNT;
    const REQUIRED_CASE_COUNT: usize =
        ANALYTIC_CASE_COUNT + ADVERSARIAL_CASE_COUNT;

    let mut case_count = 0_usize;
    let mut worst_under_report_db = f64::NEG_INFINITY;
    let mut worst_over_report_db = f64::NEG_INFINITY;
    let mut maximum_intersample_delta_db = f64::NEG_INFINITY;
    let mut maximum_adversarial_oracle_under_report_db = f64::NEG_INFINITY;
    let mut maximum_empirical_resampler_component_db = f64::NEG_INFINITY;
    let mut near_silence_finite_count = 0_usize;
    let mut cell_summary: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
    let mut evidence_hasher = Sha256::new();
    evidence_hasher.update(b"tonepoet-reference-analyzer-qualification/v6\0");

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
                                    summary,
                                    measurement,
                                    &sox,
                                    &ffmpeg,
                                    &root,
                                    channels,
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
                                    "oversampled analyzer under-report {under_report_db:.9} dB exceeded Q+E authority: rate={sample_rate_hz}, channels={channels}, normalized_frequency={normalized_frequency}, phase={phase_radians}, duration={duration_seconds}, position={}, level={true_peak_dbfs}",
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
        for fixed_frequency_hz in FIXED_FREQUENCIES_HZ {
            let normalized_frequency = f64::from(fixed_frequency_hz) / f64::from(sample_rate_hz);
            if normalized_frequency > FIXED_FREQUENCY_MAX_NORMALIZED {
                continue;
            }
            for channels in CHANNELS {
                for phase_radians in PHASES {
                    for peak_position in PEAK_POSITIONS {
                        let mut prior_reported = None;
                        for true_peak_dbfs in TRUE_PEAK_LEVELS_DBFS {
                            let root = temp.path().join(format!(
                                "analyzer-fixed-{sample_rate_hz}-{fixed_frequency_hz}hz-{channels}ch-{phase_radians:.6}-{}-{true_peak_dbfs:.3}",
                                peak_position.key(),
                            ));
                            fs::create_dir_all(&root)
                                .expect("create fixed-frequency analyzer qualification case root");
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
                                FIXED_FREQUENCY_DURATION_SECONDS,
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
                            let parsed =
                                execute_measurement(summary, measurement, &sox, &ffmpeg, &root, channels);
                            let TruePeakValue::Finite(reported) = parsed.reported else {
                                panic!("fixed-frequency fixture was misclassified as silence");
                            };
                            let TruePeakValue::Finite(upper) = parsed.conservative_upper else {
                                panic!("fixed-frequency fixture has a non-finite conservative bound");
                            };
                            let reported_dbfs = reported.0 as f64 / 1_000_000_000.0;
                            let upper_dbfs = upper.0 as f64 / 1_000_000_000.0;
                            let under_report_db = true_peak_dbfs - reported_dbfs;
                            let over_report_db = reported_dbfs - true_peak_dbfs;
                            assert!(
                                under_report_db <= 0.110_000_001,
                                "fixed-frequency analyzer under-report {under_report_db:.9} dB exceeded Q+E authority: rate={sample_rate_hz}, fixed_frequency_hz={fixed_frequency_hz}, channels={channels}, phase={phase_radians}, position={}, level={true_peak_dbfs}",
                                peak_position.key(),
                            );
                            assert!(
                                upper_dbfs + 1e-9 >= true_peak_dbfs,
                                "fixed-frequency conservative bound fell below analytic truth"
                            );
                            if let Some(prior) = prior_reported {
                                assert!(
                                    reported_dbfs > prior,
                                    "fixed-frequency true-peak sweep was not monotonic"
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
                                "fixed_frequency_single_tone|{sample_rate_hz}|{fixed_frequency_hz}|{channels}|{phase_radians:.9}|{}|{true_peak_dbfs:.9}|{sample_peak_dbfs:.9}|{reported_dbfs:.9}|{upper_dbfs:.9}\n",
                                peak_position.key(),
                            ));
                            fs::remove_file(&summary.r64_path)
                                .expect("remove fixed-frequency analyzer carrier");
                            case_count += 1;
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
                        let parsed =
                            execute_measurement(summary, measurement, &sox, &ffmpeg, &root, channels);
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
                            "multitone oversampled analyzer under-report {under_report_db:.9} dB exceeded Q+E authority"
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

    for sample_rate_hz in RATES {
        for channels in CHANNELS {
            for fixture in AdversarialAnalyzerFixture::ALL {
                for peak_position in PEAK_POSITIONS {
                    let root = temp.path().join(format!(
                        "analyzer-adversarial-{sample_rate_hz}-{channels}ch-{}-{}",
                        fixture.key(),
                        peak_position.key(),
                    ));
                    fs::create_dir_all(&root)
                        .expect("create adversarial analyzer qualification case root");
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
                    let sample_peak_dbfs = write_adversarial_analyzer_fixture(
                        &sox,
                        &summary.r64_path,
                        sample_rate_hz,
                        channels,
                        fixture,
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
                        .expect("planner emits adversarial true-peak measurement");
                    let production =
                        execute_measurement(summary, measurement, &sox, &ffmpeg, &root, channels);
                    let oracle_measurement =
                        measurement_with_oversample_factor(measurement, sample_rate_hz, 64);
                    let oracle = execute_measurement(
                        summary,
                        &oracle_measurement,
                        &sox,
                        &ffmpeg,
                        &root,
                        channels,
                    );
                    let TruePeakValue::Finite(production_reported) = production.reported else {
                        panic!("adversarial fixture was misclassified as silence");
                    };
                    let TruePeakValue::Finite(production_upper) = production.conservative_upper else {
                        panic!("adversarial fixture has no finite conservative bound");
                    };
                    let TruePeakValue::Finite(oracle_reported) = oracle.reported else {
                        panic!("64x adversarial oracle was misclassified as silence");
                    };
                    let production_dbfs = production_reported.0 as f64 / 1_000_000_000.0;
                    let production_upper_dbfs = production_upper.0 as f64 / 1_000_000_000.0;
                    let oracle_dbfs = oracle_reported.0 as f64 / 1_000_000_000.0;
                    let oracle_under_report_db = oracle_dbfs - production_dbfs;
                    let empirical_resampler_component_db = (oracle_under_report_db
                        - 0.010_000_000
                        - REFERENCE_TRUE_PEAK_GRID_BOUND.0 as f64 / 1_000_000_000.0)
                        .max(0.0);
                    assert!(
                        empirical_resampler_component_db
                            <= REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT.0 as f64
                                / 1_000_000_000.0
                                + 1e-9,
                        "adversarial pinned-resampler component {empirical_resampler_component_db:.9} dB exceeded policy authority: rate={sample_rate_hz}, channels={channels}, fixture={}, position={}",
                        fixture.key(),
                        peak_position.key(),
                    );
                    assert!(
                        production_upper_dbfs + 1e-9 >= oracle_dbfs,
                        "adversarial conservative bound fell below the 64x pinned-tool oracle"
                    );
                    maximum_adversarial_oracle_under_report_db =
                        maximum_adversarial_oracle_under_report_db.max(oracle_under_report_db);
                    maximum_empirical_resampler_component_db =
                        maximum_empirical_resampler_component_db
                            .max(empirical_resampler_component_db);
                    maximum_intersample_delta_db = maximum_intersample_delta_db
                        .max(oracle_dbfs - sample_peak_dbfs);
                    worst_under_report_db = worst_under_report_db.max(oracle_under_report_db);
                    worst_over_report_db = worst_over_report_db
                        .max(production_dbfs - oracle_dbfs);
                    let key = format!("{sample_rate_hz}/{channels}");
                    let entry = cell_summary
                        .entry(key)
                        .or_insert((0, f64::NEG_INFINITY, f64::NEG_INFINITY));
                    entry.0 += 1;
                    entry.1 = entry.1.max(oracle_under_report_db);
                    entry.2 = entry.2.max(production_dbfs - oracle_dbfs);
                    evidence_hasher.update(format!(
                        "{}|{sample_rate_hz}|{channels}|{}|{sample_peak_dbfs:.9}|{production_dbfs:.9}|{oracle_dbfs:.9}|{production_upper_dbfs:.9}|{empirical_resampler_component_db:.9}\n",
                        fixture.key(),
                        peak_position.key(),
                    ));
                    fs::remove_file(&summary.r64_path)
                        .expect("remove adversarial analyzer qualification carrier");
                    case_count += 1;
                }
            }
        }
    }

    assert_eq!(case_count, REQUIRED_CASE_COUNT);
    assert_eq!(
        near_silence_finite_count,
        ANALYTIC_CASE_COUNT / TRUE_PEAK_LEVELS_DBFS.len()
    );
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
        "method": "analytic tones/multitones plus adversarial impulse, near-band-edge burst, alternating-sign, deterministic broadband, and boundary-transient fixtures; planner-emitted 16x SoX stats command; 64x pinned-tool adversarial oracle; production parser and conservative arithmetic",
        "waveform_families": ["single_tone", "fixed_frequency_single_tone", "phase_aligned_multitone", "impulse", "near_band_edge_burst", "alternating_sign", "broadband_deterministic", "boundary_transient"],
        "single_tone_case_count": SINGLE_TONE_CASE_COUNT,
        "fixed_frequency_single_tone_case_count": FIXED_FREQUENCY_CASE_COUNT,
        "phase_aligned_multitone_case_count": MULTITONE_CASE_COUNT,
        "adversarial_case_count": ADVERSARIAL_CASE_COUNT,
        "case_count": case_count,
        "required_case_count": REQUIRED_CASE_COUNT,
        "rates_hz": RATES,
        "channels": CHANNELS,
        "normalized_frequencies_cycles_per_sample": NORMALIZED_FREQUENCIES,
        "fixed_frequencies_hz": FIXED_FREQUENCIES_HZ,
        "fixed_frequency_max_normalized": FIXED_FREQUENCY_MAX_NORMALIZED,
        "fixed_frequency_duration_seconds": FIXED_FREQUENCY_DURATION_SECONDS,
        "phases_radians": PHASES,
        "analytic_true_peak_levels_dbfs": TRUE_PEAK_LEVELS_DBFS,
        "durations_seconds": DURATIONS_SECONDS,
        "peak_positions": ["early", "late"],
        "aligned_multitone_normalized_frequencies_cycles_per_sample": MULTITONE_FREQUENCIES,
        "aligned_multitone_peak_offsets_samples": MULTITONE_PEAK_OFFSETS,
        "aligned_multitone_duration_seconds": 0.250_f64,
        "adversarial_peak_level_dbfs": -0.500_f64,
        "adversarial_oracle_oversample_factor": 64,
        "maximum_adversarial_oracle_under_report_db": maximum_adversarial_oracle_under_report_db,
        "maximum_empirical_resampler_component_db": maximum_empirical_resampler_component_db,
        "worst_under_report_db": worst_under_report_db,
        "worst_over_report_db": worst_over_report_db,
        "maximum_intersample_delta_db": maximum_intersample_delta_db,
        "oversample_factor": REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR,
        "analytic_grid_bound_db": REFERENCE_TRUE_PEAK_GRID_BOUND.render(false),
        "pinned_resampler_component_limit_db":
            REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT.0 as f64 / 1_000_000_000.0,
        "reporting_quantization_component_db":
            DbNano::POST_FINAL_ACCEPTANCE_RESERVE.0 as f64 / 1_000_000_000.0,
        "analyzer_residual_sum_db":
            REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL.0 as f64 / 1_000_000_000.0,
        "one_sided_authority_db":
            REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY.0 as f64 / 1_000_000_000.0,
        "monotonic_per_cell": true,
        "nonzero_near_silence_remained_finite": true,
        "per_rate_channel": per_rate_channel,
        "evidence_digest": format!("{:x}", evidence_hasher.finalize()),
    })
}

fn qualify_analyzer_deadline_model() -> Value {
    let sox = required_tool(SOX_ENV);
    let ffmpeg = required_tool(FFMPEG_ENV);
    let temp = TempDir::new().expect("deadline qualification tempdir");
    let root = temp.path().join("analyzer-deadline-throughput");
    fs::create_dir_all(root.join("work")).expect("create deadline qualification root");
    let source = root.join("source-placeholder.dsf");
    let duration = Duration::from_secs(2);
    let target_rate_hz = 768_000;
    let channels = 2;

    let mut settings = PipelineSettings::default();
    settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
    settings.target_format = AudioFormat::Wav;
    settings.target_sample_rate = RateTarget::PcmHz(target_rate_hz);
    settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float64);
    settings.dsd.from_dsd.reference_policy = DsdReferencePolicyVersion::SoxNg14801V16;
    let request = PlanRequest {
        input_path: source,
        output_path: root.join("deadline.w64"),
        source: SourceInfo {
            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(channels),
            duration: Some(duration),
            dsd_source_kind: Some(DsdSourceKind::DsfUncompressed),
            audio_md5: None,
        },
        settings,
        intermediate_dir: Some(root.join("work")),
        container_ffmpeg_flags: Vec::new(),
        resolved_output_target: Some(ResolvedOutputTarget::WavW64),
        reference_programme_scope: ReferenceProgrammeScope::Singleton,
        planned_riff_non_audio_upper_bound_bytes: Some(0),
    };
    let plan = plan_reference_dsd(&request).expect("deadline benchmark plan is admitted");
    let summary = plan.reference.as_ref().expect("deadline benchmark summary");
    let measurement = plan
        .steps()
        .iter()
        .find_map(|step| match step {
            PlannedExecutionStep::Measurement(measurement)
                if measurement.purpose == TruePeakPurpose::GainAuthority => Some(measurement),
            _ => None,
        })
        .expect("deadline benchmark has pre-final measurement");
    let expected_deadline = reference_true_peak_measurement_deadline(
        Some(duration),
        target_rate_hz,
        channels,
    )
    .expect("deadline benchmark arithmetic resolves");
    assert_eq!(measurement.command.expected_duration, Some(expected_deadline));
    write_analytic_analyzer_fixture(
        &sox,
        &summary.r64_path,
        target_rate_hz,
        channels,
        -0.500,
        0.45,
        std::f64::consts::FRAC_PI_4,
        duration.as_secs_f64(),
        AnalyzerPeakPosition::Late,
    );
    let started = Instant::now();
    let measured = execute_measurement(summary, measurement, &sox, &ffmpeg, &root, channels);
    let elapsed = started.elapsed();
    assert!(matches!(measured.reported, TruePeakValue::Finite(_)));
    assert!(elapsed < expected_deadline, "pinned analyzer exceeded its derived deadline");

    let guarded_frames = duration.as_nanos() * u128::from(target_rate_hz)
        / 1_000_000_000
        + 1;
    let workload_sample_values = guarded_frames
        * u128::from(channels)
        * u128::from(REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR);
    let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
    let observed_sample_values_per_second = workload_sample_values as f64 / elapsed_seconds;
    assert!(
        observed_sample_values_per_second
            >= REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND as f64,
        "pinned analyzer throughput {observed_sample_values_per_second:.3} sample-values/s is below the policy floor"
    );
    let max_workload_seconds = (REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES
        + REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND
        - 1)
        / REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND;
    let max_deadline =
        REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS + max_workload_seconds;
    assert_eq!(max_deadline, REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS);

    fs::remove_file(&summary.r64_path).expect("remove deadline benchmark carrier");
    serde_json::json!({
        "status": "passed",
        "schema": "tonepoet-reference-analyzer-deadline-qualification/v1",
        "benchmark_rate_hz": target_rate_hz,
        "benchmark_channels": channels,
        "benchmark_duration_seconds": duration.as_secs(),
        "benchmark_workload_sample_values": u64::try_from(workload_sample_values).expect("benchmark workload fits u64"),
        "elapsed_seconds": elapsed_seconds,
        "observed_oversampled_sample_values_per_second": observed_sample_values_per_second,
        "required_minimum_oversampled_sample_values_per_second": REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND,
        "derived_deadline_seconds": expected_deadline.as_secs(),
        "maximum_admitted_workload_sample_values": REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES,
        "maximum_derived_deadline_seconds": max_deadline,
        "planner_bound_identical_pipeline_deadlines": true,
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
    let end_to_end_packaged = end_to_end_summary
        .decoded_carrier(ReferenceDecodedCarrierSelector::PackagedOutput)
        .expect("end-to-end packaged carrier binding");
    let end_to_end_qpcm = end_to_end_summary
        .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
        .expect("end-to-end QPCM carrier binding");
    assert_eq!(
        decoded_sample_hash(&end_to_end_packaged, &sox, &ffmpeg),
        decoded_sample_hash(&end_to_end_qpcm, &sox, &ffmpeg),
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
    let qpcm_carrier = summary
        .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
        .expect("Int24 TPDF QPCM carrier binding");
    let samples = decoded_f64_samples(&qpcm_carrier, &sox, &ffmpeg);
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
                    &sox,
                    &ffprobe,
                    &summary.r64_path,
                    "wav_w64",
                    "float64",
                    summary.final_pcm.sample_rate_hz,
                    summary.final_pcm.channels,
                    Some(exact_w64_frame_count(
                                    &summary.r64_path,
                                    summary.final_pcm.sample_rate_hz,
                                    summary.final_pcm.channels,
                                    64,
                                    W64SampleEncoding::FloatingPoint,
                                )),
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
        &sox,
        &ffprobe,
        &dst_summary.r64_path,
        "wav_w64",
        "float64",
        dst_summary.final_pcm.sample_rate_hz,
        dst_summary.final_pcm.channels,
        Some(exact_w64_frame_count(
            &dst_summary.r64_path,
            dst_summary.final_pcm.sample_rate_hz,
            dst_summary.final_pcm.channels,
            64,
            W64SampleEncoding::FloatingPoint,
        )),
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
        "/assets/dsd_reference/brief_dsd_reference_p0_scope_and_commission.md"
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
    let metaflac = required_tool(METAFLAC_ENV);
    let wvtag = required_tool(WVTAG_ENV);
    let atomic_parsley = required_tool(ATOMIC_PARSLEY_ENV);
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

    let metaflac_version = combined(&run(&metaflac, &["--version".to_string()]));
    assert!(
        metaflac_version.to_ascii_lowercase().contains("metaflac"),
        "unexpected metaflac version response: {metaflac_version}"
    );
    let wvtag_version = combined(&run(&wvtag, &["--version".to_string()]));
    assert!(
        wvtag_version.to_ascii_lowercase().contains("wvtag"),
        "unexpected wvtag version response: {wvtag_version}"
    );
    // AtomicParsley reports its version banner when invoked without arguments.
    let atomic_parsley_version = combined(&run(&atomic_parsley, &[]));
    let atomic_parsley_reported_version = atomic_parsley_version
        .lines()
        .map(str::trim)
        .find(|line| {
            line.to_ascii_lowercase()
                .contains("atomicparsley version")
        })
        .unwrap_or_else(|| {
            panic!("unexpected AtomicParsley version response: {atomic_parsley_version}")
        });

    let qualification: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
    )))
    .expect("qualification JSON parses");
    let manifest_bytes = &include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
    ))[..];
    let candidate_bytes = &include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_candidate.json"
    ))[..];
    match qualification["status"].as_str() {
        Some("qualification_candidate") => {
            assert_eq!(
                manifest_bytes, candidate_bytes,
                "the unpromoted v16 manifest must equal its preserved candidate snapshot"
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
        other => panic!("unexpected v15 policy status: {other:?}"),
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
    let metaflac_store = std::env::var(METAFLAC_STORE_ENV)
        .expect("qualified package must expose the exact metaflac store path");
    let wvtag_store = std::env::var(WVTAG_STORE_ENV)
        .expect("qualified package must expose the exact wvtag store path");
    let atomic_parsley_store = std::env::var(ATOMIC_PARSLEY_STORE_ENV)
        .expect("qualified package must expose the exact AtomicParsley store path");
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
    assert_eq!(
        fs::canonicalize(Path::new(&metaflac_store).join("bin/metaflac"))
            .expect("qualified metaflac store must contain bin/metaflac"),
        metaflac,
        "metaflac activation path does not belong to the compiled qualification store"
    );
    assert_eq!(
        fs::canonicalize(Path::new(&wvtag_store).join("bin/wvtag"))
            .expect("qualified wvtag store must contain bin/wvtag"),
        wvtag,
        "wvtag activation path does not belong to the compiled qualification store"
    );
    assert_eq!(
        fs::canonicalize(Path::new(&atomic_parsley_store).join("bin/AtomicParsley"))
            .expect("qualified AtomicParsley store must contain bin/AtomicParsley"),
        atomic_parsley,
        "AtomicParsley activation path does not belong to the compiled qualification store"
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
        "production_metadata_mutators": {
            "metaflac": {
                "canonical_path": metaflac.display().to_string(),
                "store_path": metaflac_store,
                "executable_sha256": sha256_hex(&fs::read(&metaflac).expect("read qualified metaflac executable")),
                "reported_version": first_nonempty_line(&metaflac_version),
            },
            "wvtag": {
                "canonical_path": wvtag.display().to_string(),
                "store_path": wvtag_store,
                "executable_sha256": sha256_hex(&fs::read(&wvtag).expect("read qualified wvtag executable")),
                "reported_version": first_nonempty_line(&wvtag_version),
            },
            "AtomicParsley": {
                "canonical_path": atomic_parsley.display().to_string(),
                "store_path": atomic_parsley_store,
                "executable_sha256": sha256_hex(&fs::read(&atomic_parsley).expect("read qualified AtomicParsley executable")),
                "reported_version": atomic_parsley_reported_version,
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
    let forbidden_route_regression = assert_qualification_decode_route_table();
    let decode_route_table = qualification_decode_route_table_evidence();
    let default_settings_live_smoke = qualify_default_settings_dsd64_dsf_to_flac();
    let environment_probe_results = qualify_subprocess_environment_isolation();
    let PackageQualificationEvidence {
        case_count: package_case_count,
        terminal_bound_case_count,
        terminal_observed_max_error_by_depth,
        sample_identity_oracle,
    } = qualify_lossless_package_cells(forbidden_route_regression);
    let analyzer_carrier_results = qualify_analyzer_carrier_contract();
    let w64_exact_integrity = qualify_w64_exact_integrity_contract();
    let analyzer_results = qualify_true_peak_analyzer_authority();
    let analyzer_deadline_results = qualify_analyzer_deadline_model();
    let gain_terminal_results = qualify_production_measurement_gain_terminal_chain();
    let dst_counts = qualify_dst_oracle_fixture_authority();
    let source_front_end_results = qualify_production_source_front_end_integration();
    let profile_results = qualify_pinned_reference_toolchain_and_profile_responses();
    let qualification_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
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
    let production_metadata_mutation_evidence =
        sample_identity_oracle["production_metadata_mutation"].clone();
    let report = serde_json::json!({
        "schema_version": 16,
        "policy": tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY,
        "status": "passed",
        "qualification_manifest_digest": sha256_hex(qualification_bytes),
        "toolchain": profile_results,
        "runtime_metadata_mutator_binding": {
            "schema": "tonepoet-reference-runtime-metadata-mutator-binding/v1",
            "status": "passed",
            "certified_identity_source": "toolchain.production_metadata_mutators",
            "compiled_store_binding": "required_for_metaflac_wvtag_atomicparsley",
            "activation_path_policy": "must_equal_compiled_store_and_certified_canonical_path",
            "runner_resolution_policy": "resolved_canonical_path_must_equal_certified_path",
            "execution_authority": "exact_canonical_path_plus_executable_sha256",
            "pre_mutation_reverification": "path_sha256_version_closure",
            "per_output_authority": "ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1"
        },
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
        "streamed_wav_capacity": analyzer_carrier_results["streamed_wav_capacity"].clone(),
        "analyzer_carrier": analyzer_carrier_results,
        "w64_exact_integrity": w64_exact_integrity,
        "production_true_peak_analyzer": analyzer_results,
        "analyzer_deadline_model": analyzer_deadline_results,
        "executor_liveness": {
            "status": "passed_by_workspace_gate",
            "test": "reference_pipeline_composite_permits_prevent_opposite_direction_deadlock",
            "global_tool_family_order": ["sox", "ffmpeg", "ssrc"],
            "permit_set": "deduplicated_cancellation_safe_raii",
            "interleaving": "barrier_forced_no_sleep"
        },
        "production_source_front_end_integration": source_front_end_results,
        "production_measurement_gain_terminal_chain": gain_terminal_results,
        "analyzer_policy_bounds": qualification["analyzer"].clone(),
        "terminal_bounds": qualification["terminal_bounds"].clone(),
        "riff_capacity": qualification["riff_capacity"].clone(),
        "streamed_wav_capacity_policy": qualification["streamed_wav_capacity"].clone(),
        "float64_package_pipeline": qualification["packaging"].clone(),
        "sample_identity_oracle": sample_identity_oracle,
        "evidence_command_environment": {
            "status": "passed",
            "policy": qualification["subprocess_environment"].clone(),
            "runtime_probe": environment_probe_results.clone(),
        },
        "package_decode_back": {
            "status": "passed",
            "decode_route_table": decode_route_table,
            "case_count": package_case_count,
            "empirical_terminal_bound_case_count": terminal_bound_case_count,
            "terminal_observed_max_error_by_depth": terminal_observed_max_error_by_depth,
            "terminal_effects_boundary_audit": {
                "sox_internal_sample_domain": "signed_q1_31",
                "round_to_nearest_half_step_peak_bound": "2^-32",
                "inherited_float64_arithmetic_bound": "2^-51",
                "combined_float64_peak_bound": "2^-32_plus_2^-51",
                "int24_disposition": "retained_2^-22_bound_contains_effects_rounding",
                "float32_disposition": "retained_2^-23_bound_contains_effects_and_carrier_rounding",
                "float64_disposition": "corrected_to_2^-32_plus_2^-51",
                "enabled_cells_rejected": 0
            },
            "terminal_effects_source_proof": {
                "schema": "tonepoet-reference-terminal-effects-source-proof/v1",
                "policy": tonepoet_pipeline::DSD_REFERENCE_POLICY_V8_KEY,
                "sox_ng_revision": "324b8cf873fd7836e8848bd87f7a90d8faa6f849",
                "sox_ng_nar_hash": "sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=",
                "proof_path": "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md",
                "proof_sha256": sha256_hex(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md"
                ))),
                "internal_sample_domain": "signed_twos_complement_int32_q1_31",
                "float64_carrier_grid_round_trip": "exact_for_every_sox_sample_t_grid_value",
                "gain_rounding_site": "gain.c:flow_gain:SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)",
                "non_clipping_rounding_bound": "one_half_internal_sample_equals_2^-32_fs",
                "gain_mode_scope": ["reference_compensated", "native_level_exact", "fixed_exact"],
                "combined_float64_bound": "2^-32_plus_2^-51"
            },
            "rates_hz": [44100,48000,88200,96000,176400,192000,352800,384000,705600,768000],
            "channels": [1,2],
            "depths": ["int24","float32","float64"],
            "targets": ["flac_native","wav_riff","wav_rf64","wav_w64","aiff_native","wavpack_native","alac_m4a"],
            "flac_compression_levels": [0,1,2,3,4,5,6,7,8],
            "wavpack_compression_levels": [0,1,2,3],
            "wavpack_int24_required_args": ["-bits_per_raw_sample","24"],
            "container_level_post_mutation_sample_identity": "passed_for_420_admitted_non_w64_cells",
            "production_metadata_mutation_qualification": production_metadata_mutation_evidence,
            "command_authority": "exact PlannedExecutionStep vectors from plan_reference_dsd plus the shared production per-file metadata implementation",
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

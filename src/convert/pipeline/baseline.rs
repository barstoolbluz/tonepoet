//! Measurement-only Stage A instrumentation for the pipeline adversarial audit.
//!
//! This module is deliberately inert unless `TONEPOET_PIPELINE_BASELINE_JSONL`
//! names an output file.  It owns no execution decisions and must never turn an
//! instrumentation failure into a conversion failure.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const BASELINE_ENV: &str = "TONEPOET_PIPELINE_BASELINE_JSONL";

struct Sink {
    file: Mutex<File>,
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();
static RUN_STARTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
static STAGE_STARTS: OnceLock<Mutex<HashMap<(String, String), Instant>>> = OnceLock::new();

static TOOL_ACTIVE: AtomicU64 = AtomicU64::new(0);
static TOOL_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
static CERTIFIED_SCAN_ACTIVE: AtomicU64 = AtomicU64::new(0);
static CERTIFIED_SCAN_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
static REPLAYGAIN_DECODER_ACTIVE: AtomicU64 = AtomicU64::new(0);
static REPLAYGAIN_DECODER_HIGH_WATER: AtomicU64 = AtomicU64::new(0);

fn sink() -> Option<&'static Sink> {
    SINK.get_or_init(|| {
        let path = std::env::var_os(BASELINE_ENV)?;
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Sink {
            file: Mutex::new(file),
        })
    })
    .as_ref()
}

pub(crate) fn enabled() -> bool {
    sink().is_some()
}

// CUE IP-01 reuses the existing opt-in JSONL sink. An Instant-relative clock
// connects scheduler/worker/postprocess events without a shared per-album
// telemetry map, clock synchronization, or always-on allocation. The clock is
// process-local; never compare its values between separate runs.
static CUE_CLOCK: OnceLock<Instant> = OnceLock::new();
static CUE_ID_SALT: OnceLock<[u8; 16]> = OnceLock::new();

// This context exists ONLY on the native post-output worker thread. It binds
// its preexisting replaygain_measure scope to the corresponding CUE run,
// instead of guessing from the number of events in a possibly mixed trace.
thread_local! {
    static CUE_NATIVE_REPLAYGAIN_RUN: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct CueNativeReplayGainGuard(Option<String>);

impl Drop for CueNativeReplayGainGuard {
    fn drop(&mut self) {
        CUE_NATIVE_REPLAYGAIN_RUN.with(|slot| {
            *slot.borrow_mut() = self.0.take();
        });
    }
}

pub(crate) fn with_cue_native_replaygain_correlation<T>(
    job_id: &str,
    operation: impl FnOnce() -> T,
) -> T {
    if !enabled() {
        return operation();
    }
    let previous = CUE_NATIVE_REPLAYGAIN_RUN.with(|slot| {
        slot.borrow_mut().replace(cue_run_id(job_id))
    });
    let _guard = CueNativeReplayGainGuard(previous);
    operation()
}

fn cue_run_id(job_id: &str) -> String {
    let salt = CUE_ID_SALT.get_or_init(|| *uuid::Uuid::new_v4().as_bytes());
    let mut digest = Sha256::new();
    digest.update(salt);
    digest.update(job_id.as_bytes());
    // Privacy-preserving correlation within one process only. Never emit the
    // source path, track title, raw item ID, or raw job ID in IP-01 fields.
    hex::encode(&digest.finalize()[..12])
}

fn cue_elapsed_us(start: &Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(*start).as_micros()).unwrap_or(u64::MAX)
}

/// Captured before attempting scheduler submission, so a worker that starts
/// immediately cannot precede the admitted timestamp. This is *not* the UI
/// accept timestamp, which IP-01 deliberately does not instrument.
pub(crate) fn cue_timestamp_us() -> Option<u64> {
    if !enabled() {
        return None;
    }
    let start = CUE_CLOCK.get_or_init(Instant::now);
    Some(cue_elapsed_us(start, Instant::now()))
}

fn cue_event_value(job_id: &str, phase: &'static str, at_us: u64, fields: Value) -> Value {
    let mut map = match fields {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    // These reserved keys always have trusted types/values, regardless of the
    // supplied diagnostic fields.
    map.insert("cue_schema".into(), json!("tonepoet.cue.ip01.v1"));
    map.insert("cue_run_id".into(), json!(cue_run_id(job_id)));
    map.insert("cue_phase".into(), json!(phase));
    map.insert("cue_monotonic_us".into(), json!(at_us));
    Value::Object(map)
}

pub(crate) fn cue_emit_at(
    job_id: &str,
    phase: &'static str,
    at_us: Option<u64>,
    fields: Value,
) {
    if !enabled() {
        return;
    }
    let at_us = at_us.or_else(cue_timestamp_us).unwrap_or(0);
    emit("cue_ip01", cue_event_value(job_id, phase, at_us, fields));
}

pub(crate) fn cue_emit(job_id: &str, phase: &'static str, fields: Value) {
    cue_emit_at(job_id, phase, None, fields);
}

/// The actual user-visible repaint is a host stopwatch obligation, not a
/// property of a scheduler or publication callback.
#[cfg(test)]
mod cue_ip01_tests {
    use super::*;

    #[test]
    fn cue_ip01_disabled_produces_no_line_and_does_not_touch_a_fixture() {
        fn write_if_enabled(out: &mut Vec<u8>, enabled: bool, value: &Value) {
            if enabled {
                serde_json::to_writer(&mut *out, value).unwrap();
                out.push(b'\n');
            }
        }
        let sample = cue_event_value("first", "route_selected", 10, json!({"route": "s1"}));
        let mut existing_log = b"untouched fixture\n".to_vec();
        write_if_enabled(&mut existing_log, false, &sample);
        assert_eq!(existing_log, b"untouched fixture\n");
    }

    #[test]
    fn cue_ip01_jsonl_is_bounded_private_and_correlates_interleaved_albums() {
        let mut lines = Vec::new();
        for (job, phase, instant, ordinal) in [
            ("album one /private/secret.wav", "work_started", 10_u64, 1_u32),
            ("album two /private/title.wav", "work_started", 12, 3),
            ("album two /private/title.wav", "work_finished", 16, 4),
            ("album one /private/secret.wav", "work_finished", 20, 2),
        ] {
            let record = cue_event_value(job, phase, instant, json!({"track_ordinal": ordinal}));
            serde_json::to_writer(&mut lines, &record).unwrap();
            lines.push(b'\n');
        }
        let raw = String::from_utf8(lines).unwrap();
        assert!(!raw.contains("/private/"));
        let parsed = raw.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0]["cue_run_id"], parsed[3]["cue_run_id"]);
        assert_eq!(parsed[1]["cue_run_id"], parsed[2]["cue_run_id"]);
        assert_ne!(parsed[0]["cue_run_id"], parsed[1]["cue_run_id"]);
        assert_eq!(parsed[2]["track_ordinal"], 4);
        assert_eq!(parsed[3]["track_ordinal"], 2);
        assert!(parsed[0]["cue_monotonic_us"].as_u64().unwrap()
            < parsed[3]["cue_monotonic_us"].as_u64().unwrap());
        assert!(parsed.iter().all(|record|
            record["cue_schema"] == "tonepoet.cue.ip01.v1"
                && record["cue_run_id"].as_str().is_some_and(|id| id.len() == 24)
        ));
        // cue_event_value is the typed payload. emit() supplies the outer
        // event name only on the actually enabled sink path.
    }

    // Child runs are fresh test-harness processes: a OnceLock sink can only
    // be tested honestly by choosing the env var before the first emit.
    #[test]
    fn cue_ip01_actual_sink_subprocess_child() {
        let Ok(mode) = std::env::var("TONEPOET_IP01_SINK_CHILD") else {
            return; // ordinary full-workspace test execution
        };
        assert_eq!(enabled(), mode == "enabled");
        let jobs = ["/private/album-one.flac", "/private/album-two.flac"];
        std::thread::scope(|scope| {
            for (index, job) in jobs.iter().enumerate() {
                scope.spawn(move || {
                    cue_emit(job, "route_selected", json!({
                        "group": format!("cue-stream:{index:04}-{index:04}"),
                        "route": "s1",
                        "track_ordinal": index,
                    }));
                    cue_emit(job, "work_finished", json!({
                        "group": format!("cue-stream:{index:04}-{index:04}"),
                        "elapsed_ms": 0.5,
                    }));
                    if index == 0 {
                        with_cue_native_replaygain_correlation(job, || {
                            emit("replaygain_measure_started", json!({"mode": "Both"}));
                            emit("replaygain_measure_finished", json!({"elapsed_ms": 0.4}));
                        });
                    }
                });
            }
        });
    }

    #[test]
    fn cue_ip01_real_enabled_disabled_sink_and_concurrent_identity() {
        let exe = std::env::current_exe().expect("test harness path");
        for mode in ["disabled", "enabled"] {
            let temp = tempfile::tempdir().expect("isolated trace dir");
            let trace = temp.path().join("diagnostics.jsonl");
            let original = temp.path().join("original-conversion.log");
            std::fs::write(&original, b"existing log unaffected\n").unwrap();
            let mut child = std::process::Command::new(&exe);
            child.arg("cue_ip01_actual_sink_subprocess_child")
                .arg("--nocapture")
                .env("TONEPOET_IP01_SINK_CHILD", mode)
                .env_remove(BASELINE_ENV);
            if mode == "enabled" {
                child.env(BASELINE_ENV, &trace);
            }
            let output = child.output().expect("child test launched");
            assert!(output.status.success(), "child test failed: {} / {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "the child filter must execute exactly one test");
            assert_eq!(std::fs::read(&original).unwrap(), b"existing log unaffected\n");
            if mode == "disabled" {
                assert!(!trace.exists(), "disabled diagnostics must not create a file");
                continue;
            }
            let raw = std::fs::read_to_string(&trace).expect("real JSONL file exists");
            assert!(!raw.contains("/private/"), "CUE records leak private job IDs");
            let records = raw.lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 6, "atomic concurrent JSONL plus one native scope");
            let cue = records.iter().filter(|value| value["event"] == "cue_ip01")
                .collect::<Vec<_>>();
            assert_eq!(cue.len(), 4);
            let ids = cue.iter().map(|record| record["cue_run_id"].as_str().unwrap())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(ids.len(), 2, "two jobs must have distinct salted run IDs");
            for record in &cue {
                assert!(record["cue_monotonic_us"].as_u64().is_some());
            }
            let native = records.iter().find(|record|
                record["event"] == "replaygain_measure_finished").unwrap();
            let native_start = records.iter().find(|record|
                record["event"] == "replaygain_measure_started").unwrap();
            assert_eq!(native["cue_run_id"], native_start["cue_run_id"]);
            let matching = cue.iter().filter(|record|
                record["cue_phase"] == "route_selected"
                    && record["cue_run_id"] == native["cue_run_id"])
                .count();
            assert_eq!(matching, 1);
        }
    }

    #[test]
    fn cue_ip01_overlapping_intervals_are_not_summed() {
        // A decoder can overlap encoders; the observable transport wall clock
        // is the union, not the sum of child process runtimes.
        let decoder = (0_u64, 120_u64);
        let encoder = (10_u64, 110_u64);
        let observed_wall = decoder.1.max(encoder.1) - decoder.0.min(encoder.0);
        assert_eq!(observed_wall, 120);
        assert_ne!(observed_wall, (decoder.1 - decoder.0) + (encoder.1 - encoder.0));
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn current_item() -> Option<String> {
    crate::concurrency::current_execution_item()
}

pub(crate) fn emit(event: &str, fields: Value) {
    let Some(sink) = sink() else {
        return;
    };
    let mut object = match fields {
        Value::Object(object) => object,
        other => {
            let mut object = Map::new();
            object.insert("value".to_string(), other);
            object
        }
    };
    object.insert("event".to_string(), Value::String(event.to_string()));
    object.insert("unix_ms".to_string(), json!(unix_millis()));
    if matches!(event, "replaygain_measure_started" | "replaygain_measure_finished") {
        // This key is added only by the CUE worker's explicit thread-local
        // scope, not from a legacy item_id or event-count heuristic.
        if let Some(run) = CUE_NATIVE_REPLAYGAIN_RUN.with(|slot| slot.borrow().clone()) {
            object.insert("cue_run_id".to_string(), json!(run));
        }
    }
    // CUE IP-01 identifies runs only by their salted correlation key;
    // the existing baseline records retain their established item_id policy.
    if event != "cue_ip01" {
        if let Some(item) = current_item() {
            object.entry("item_id".to_string()).or_insert(Value::String(item));
        }
    }
    let Ok(mut encoded) = serde_json::to_vec(&Value::Object(object)) else {
        return;
    };
    encoded.push(b'\n');
    if let Ok(mut file) = sink.file.lock() {
        let _ = file.write_all(&encoded);
    }
}

/// Emit the Stage A per-operation selected-vs-emitted lowering observations.
///
/// This is deliberately measurement-only. Diagnostic failure is recorded in
/// JSONL and never changes admission, lowering, or execution behavior.
pub(crate) fn selected_vs_emitted(request: &tonepoet_pipeline::PlanRequest) {
    if !enabled() {
        return;
    }
    match tonepoet_pipeline::stage_a_selected_vs_emitted_diagnostics(request) {
        Ok(records) => {
            let represented_operations = records.len();
            for record in records {
                let matches_selected_realization = record.matches_selected_realization();
                match serde_json::to_value(&record) {
                    Ok(Value::Object(mut fields)) => {
                        fields.insert(
                            "matches_selected_realization".to_string(),
                            json!(matches_selected_realization),
                        );
                        emit("selected_vs_emitted", Value::Object(fields));
                    }
                    Ok(_) | Err(_) => emit(
                        "selected_vs_emitted_error",
                        json!({"error": "could not serialize selected-vs-emitted record"}),
                    ),
                }
            }
            emit(
                "selected_vs_emitted_summary",
                json!({
                    "eligible_operations": represented_operations,
                    "represented_operations": represented_operations,
                }),
            );
        }
        Err(error) => emit(
            "selected_vs_emitted_error",
            json!({"error": error.to_string()}),
        ),
    }
}

fn update_high_water(high_water: &AtomicU64, active: u64) -> u64 {
    let mut observed = high_water.load(Ordering::Relaxed);
    while active > observed {
        match high_water.compare_exchange_weak(
            observed,
            active,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return active,
            Err(actual) => observed = actual,
        }
    }
    observed
}

pub(crate) fn run_started(item_id: &str) {
    if !enabled() {
        return;
    }
    RUN_STARTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .map(|mut starts| starts.insert(item_id.to_string(), Instant::now()));
    emit("run_started", json!({"item_id": item_id}));
}

pub(crate) fn run_finished(item_id: &str, terminal_status: &str) {
    if !enabled() {
        return;
    }
    let elapsed_ms = RUN_STARTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|mut starts| starts.remove(item_id))
        .map(|started| started.elapsed().as_secs_f64() * 1000.0);
    emit(
        "run_finished",
        json!({
            "item_id": item_id,
            "terminal_status": terminal_status,
            "elapsed_ms": elapsed_ms,
            "rss_kib": rss_kib(),
            "tool_active_high_water": TOOL_HIGH_WATER.load(Ordering::Relaxed),
            "certified_scan_active_high_water": CERTIFIED_SCAN_HIGH_WATER.load(Ordering::Relaxed),
            "replaygain_decoder_active_high_water": REPLAYGAIN_DECODER_HIGH_WATER.load(Ordering::Relaxed),
        }),
    );
}

pub(crate) fn stage_started(item_id: &str, stage: &str) {
    if !enabled() {
        return;
    }
    STAGE_STARTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .map(|mut starts| starts.insert((item_id.to_string(), stage.to_string()), Instant::now()));
    emit("stage_started", json!({"item_id": item_id, "stage": stage}));
}

pub(crate) fn stage_finished(item_id: &str, stage: &str, outcome: &str) {
    if !enabled() {
        return;
    }
    let elapsed_ms = STAGE_STARTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|mut starts| starts.remove(&(item_id.to_string(), stage.to_string())))
        .map(|started| started.elapsed().as_secs_f64() * 1000.0);
    emit(
        "stage_finished",
        json!({
            "item_id": item_id,
            "stage": stage,
            "outcome": outcome,
            "elapsed_ms": elapsed_ms,
            "rss_kib": rss_kib(),
        }),
    );
}

pub(crate) struct ToolRunGuard {
    binary: &'static str,
    started: Instant,
    active_at_start: u64,
}

pub(crate) fn tool_run_started(binary: &'static str, arg_count: usize) -> Option<ToolRunGuard> {
    if !enabled() {
        return None;
    }
    let active = TOOL_ACTIVE.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let high_water = update_high_water(&TOOL_HIGH_WATER, active);
    emit(
        "tool_run_started",
        json!({
            "tool": binary,
            "arg_count": arg_count,
            "active": active,
            "active_high_water": high_water,
        }),
    );
    Some(ToolRunGuard {
        binary,
        started: Instant::now(),
        active_at_start: active,
    })
}

impl Drop for ToolRunGuard {
    fn drop(&mut self) {
        let active_after = TOOL_ACTIVE.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        emit(
            "tool_run_finished",
            json!({
                "tool": self.binary,
                "elapsed_ms": self.started.elapsed().as_secs_f64() * 1000.0,
                "active_at_start": self.active_at_start,
                "active_after": active_after,
            }),
        );
    }
}

pub(crate) struct CertifiedScanGuard {
    kind: &'static str,
    path: String,
    logical_bytes: u64,
    started: Instant,
    complete: bool,
}

pub(crate) fn certified_scan_started(
    kind: &'static str,
    path: &Path,
    logical_bytes: u64,
) -> Option<CertifiedScanGuard> {
    if !enabled() {
        return None;
    }
    let active = CERTIFIED_SCAN_ACTIVE
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let high_water = update_high_water(&CERTIFIED_SCAN_HIGH_WATER, active);
    emit(
        "certified_scan_started",
        json!({
            "kind": kind,
            "path": path.display().to_string(),
            "logical_bytes": logical_bytes,
            "active": active,
            "active_high_water": high_water,
        }),
    );
    Some(CertifiedScanGuard {
        kind,
        path: path.display().to_string(),
        logical_bytes,
        started: Instant::now(),
        complete: false,
    })
}

impl CertifiedScanGuard {
    pub(crate) fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for CertifiedScanGuard {
    fn drop(&mut self) {
        let active_after = CERTIFIED_SCAN_ACTIVE
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        emit(
            "certified_scan_finished",
            json!({
                "kind": self.kind,
                "path": self.path,
                "logical_bytes": self.logical_bytes,
                "complete": self.complete,
                "elapsed_ms": self.started.elapsed().as_secs_f64() * 1000.0,
                "active_after": active_after,
            }),
        );
    }
}

pub(crate) struct ReplayGainObservationGuard {
    path: String,
    input_bytes: u64,
    started: Instant,
    complete: bool,
    decoded_pcm_bytes: u64,
    frame_allocations: u64,
    frame_allocation_bytes: u64,
}

pub(crate) fn replaygain_observation_started(
    path: &Path,
    input_bytes: u64,
) -> Option<ReplayGainObservationGuard> {
    if !enabled() {
        return None;
    }
    let active = REPLAYGAIN_DECODER_ACTIVE
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let high_water = update_high_water(&REPLAYGAIN_DECODER_HIGH_WATER, active);
    emit(
        "replaygain_observation_started",
        json!({
            "path": path.display().to_string(),
            "input_bytes": input_bytes,
            "active": active,
            "active_high_water": high_water,
        }),
    );
    Some(ReplayGainObservationGuard {
        path: path.display().to_string(),
        input_bytes,
        started: Instant::now(),
        complete: false,
        decoded_pcm_bytes: 0,
        frame_allocations: 0,
        frame_allocation_bytes: 0,
    })
}

impl ReplayGainObservationGuard {
    pub(crate) fn mark_complete(
        &mut self,
        decoded_pcm_bytes: u64,
        frame_allocations: u64,
        frame_allocation_bytes: u64,
    ) {
        self.complete = true;
        self.decoded_pcm_bytes = decoded_pcm_bytes;
        self.frame_allocations = frame_allocations;
        self.frame_allocation_bytes = frame_allocation_bytes;
    }
}

impl Drop for ReplayGainObservationGuard {
    fn drop(&mut self) {
        let active_after = REPLAYGAIN_DECODER_ACTIVE
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        emit(
            "replaygain_observation_finished",
            json!({
                "path": self.path,
                "input_bytes": self.input_bytes,
                "complete": self.complete,
                "decoded_pcm_bytes": self.decoded_pcm_bytes,
                "frame_allocations": self.frame_allocations,
                "frame_allocation_bytes": self.frame_allocation_bytes,
                "elapsed_ms": self.started.elapsed().as_secs_f64() * 1000.0,
                "active_after": active_after,
            }),
        );
    }
}

pub(crate) struct ScopedEvent {
    event: &'static str,
    fields: Value,
    started: Instant,
}

pub(crate) fn scoped_event(event: &'static str, fields: Value) -> Option<ScopedEvent> {
    if !enabled() {
        return None;
    }
    emit(&format!("{event}_started"), fields.clone());
    Some(ScopedEvent {
        event,
        fields,
        started: Instant::now(),
    })
}

impl Drop for ScopedEvent {
    fn drop(&mut self) {
        let mut fields = match self.fields.clone() {
            Value::Object(map) => map,
            value => {
                let mut map = Map::new();
                map.insert("value".to_string(), value);
                map
            }
        };
        fields.insert(
            "elapsed_ms".to_string(),
            json!(self.started.elapsed().as_secs_f64() * 1000.0),
        );
        emit(&format!("{}_finished", self.event), Value::Object(fields));
    }
}

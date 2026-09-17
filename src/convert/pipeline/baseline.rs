//! Measurement-only Stage A instrumentation for the pipeline adversarial audit.
//!
//! This module is deliberately inert unless `TONEPOET_PIPELINE_BASELINE_JSONL`
//! names an output file.  It owns no execution decisions and must never turn an
//! instrumentation failure into a conversion failure.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

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
    if let Some(item) = current_item() {
        object.entry("item_id".to_string()).or_insert(Value::String(item));
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

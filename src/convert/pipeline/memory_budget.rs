//! Scratch-staging memory budgeting and validation.
//!
//! This module is deliberately conservative: tmpfs staging is an optimization,
//! not a correctness requirement. Any uncertainty falls back to the existing
//! disk-backed staging path.

use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;

use crate::concurrency::{
    ClaimMode, ClaimScope, MutationClaimGuard, PathClaim, PathResolutionSemantics,
};
use super::types::SourceKind;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const DEFAULT_UNKNOWN_ESTIMATE_BYTES: u64 = 512 * MIB;
const MAX_DIRECTORY_ESTIMATE_ENTRIES: usize = 16_384;
const SCRATCH_PROBE_FILE: &str = ".tonepoet-scratch-write-test";
const SCRATCH_CLEANUP_LOCK_FILE: &str = ".tonepoet-staging.cleanup.lock";
const RUN_LOCK_SUFFIX: &str = ".run.lock";
const STAGING_OWNER_MARKER: &str = ".tonepoet-staging-owner";

#[derive(Debug, Clone)]
pub struct ScratchStagingConfig {
    root: PathBuf,
    memory_limit_percent: u8,
    budget: Arc<ScratchMemoryBudget>,
    validation: Arc<Mutex<ScratchValidationState>>,
}

#[derive(Debug, Default)]
struct ScratchValidationState {
    checked: bool,
    warned_not_ram_backed: bool,
    ram_backed: Option<bool>,
}

impl ScratchStagingConfig {
    #[must_use]
    pub fn new(root: PathBuf, memory_limit_percent: u8) -> Self {
        let memory_limit_percent = clamp_memory_limit_percent(memory_limit_percent);
        Self {
            root,
            memory_limit_percent,
            budget: Arc::new(ScratchMemoryBudget::new(memory_limit_percent)),
            validation: Arc::new(Mutex::new(ScratchValidationState::default())),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_fixed_memory_and_filesystem_for_test(
        root: PathBuf,
        memory_limit_percent: u8,
        total_memory_bytes: u64,
        available_memory_bytes: u64,
        total_filesystem_bytes: u64,
        available_filesystem_bytes: u64,
    ) -> Self {
        let memory_limit_percent = clamp_memory_limit_percent(memory_limit_percent);
        Self {
            root,
            memory_limit_percent,
            budget: Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
                memory_limit_percent,
                total_memory_bytes,
                available_memory_bytes,
                total_filesystem_bytes,
                available_filesystem_bytes,
            )),
            validation: Arc::new(Mutex::new(ScratchValidationState {
                checked: false,
                warned_not_ram_backed: false,
                ram_backed: Some(true),
            })),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn memory_limit_percent(&self) -> u8 {
        self.memory_limit_percent
    }

    pub fn ensure_usable(&self, staging_parent: &Path) -> io::Result<()> {
        let mut state = self.validation.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.checked {
            return Ok(());
        }

        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(staging_parent)?;
        verify_writable(&self.root)?;

        match is_ram_backed_path(&self.root) {
            Ok(true) => {
                state.ram_backed = Some(true);
            }
            Ok(false) => {
                state.ram_backed = Some(false);
                if !state.warned_not_ram_backed {
                    log::warn!(
                        "configured scratch_directory is not on tmpfs/ramfs: {}; treating it as disk-backed scratch and gating admission by filesystem free space rather than system RAM budget",
                        self.root.display()
                    );
                    state.warned_not_ram_backed = true;
                }
            }
            Err(err) => {
                state.ram_backed = None;
                if !state.warned_not_ram_backed {
                    log::warn!(
                        "could not determine whether scratch_directory is tmpfs/ramfs ({}): {err}; proceeding conservatively with RAM and filesystem admission gates",
                        self.root.display()
                    );
                    state.warned_not_ram_backed = true;
                }
            }
        }

        cleanup_stale_staging_trees(staging_parent)?;
        state.checked = true;
        Ok(())
    }

    pub fn try_reserve(&self, estimated_bytes: u64) -> Result<ScratchReservation, ScratchAdmissionError> {
        let ram_backed = self
            .validation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ram_backed
            .unwrap_or(true);
        self.budget
            .try_reserve_with_ram_gate(estimated_bytes, &self.root, ram_backed)
    }

    #[cfg(test)]
    #[must_use]
    pub fn active_reserved_bytes_for_test(&self) -> u64 {
        self.budget.active_reserved_bytes()
    }
}

#[derive(Debug)]
pub struct ScratchMemoryBudget {
    memory_limit_percent: u8,
    active_reserved_bytes: Mutex<u64>,
    memory_source: MemorySource,
    filesystem_source: FilesystemSource,
}

impl ScratchMemoryBudget {
    #[must_use]
    pub fn new(memory_limit_percent: u8) -> Self {
        Self {
            memory_limit_percent: clamp_memory_limit_percent(memory_limit_percent),
            active_reserved_bytes: Mutex::new(0),
            memory_source: MemorySource::System,
            filesystem_source: FilesystemSource::System,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_fixed_total_memory(memory_limit_percent: u8, total_memory_bytes: u64) -> Self {
        Self::with_fixed_memory_and_filesystem(
            memory_limit_percent,
            total_memory_bytes,
            total_memory_bytes,
            u64::MAX / 4,
            u64::MAX / 4,
        )
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_fixed_memory_and_filesystem(
        memory_limit_percent: u8,
        total_memory_bytes: u64,
        available_memory_bytes: u64,
        total_filesystem_bytes: u64,
        available_filesystem_bytes: u64,
    ) -> Self {
        Self {
            memory_limit_percent: clamp_memory_limit_percent(memory_limit_percent),
            active_reserved_bytes: Mutex::new(0),
            memory_source: MemorySource::Fixed {
                total_memory_bytes,
                available_memory_bytes,
            },
            filesystem_source: FilesystemSource::Fixed {
                total_filesystem_bytes,
                available_filesystem_bytes,
                headroom_bytes: 0,
            },
        }
    }

    pub fn try_reserve(
        self: &Arc<Self>,
        estimated_bytes: u64,
        scratch_root: &Path,
    ) -> Result<ScratchReservation, ScratchAdmissionError> {
        self.try_reserve_with_ram_gate(estimated_bytes, scratch_root, true)
    }

    pub fn try_reserve_with_ram_gate(
        self: &Arc<Self>,
        estimated_bytes: u64,
        scratch_root: &Path,
        enforce_ram_budget: bool,
    ) -> Result<ScratchReservation, ScratchAdmissionError> {
        let estimated_bytes = estimated_bytes.max(1);
        if self.memory_limit_percent == 0 {
            return Err(ScratchAdmissionError::new(
                ScratchAdmissionFailureKind::Disabled,
                "scratch staging is disabled by scratch_memory_limit_percent=0",
            ));
        }

        let filesystem = self.filesystem_source.snapshot(scratch_root).map_err(|err| {
            ScratchAdmissionError::new(
                ScratchAdmissionFailureKind::FilesystemCapacity,
                format!(
                    "could not read scratch filesystem capacity for {}: {err}",
                    scratch_root.display()
                ),
            )
        })?;

        let memory = if enforce_ram_budget {
            Some(self.memory_source.snapshot().map_err(|err| {
                ScratchAdmissionError::new(
                    ScratchAdmissionFailureKind::AvailableMemory,
                    format!("could not read system memory budget: {err}"),
                )
            })?)
        } else {
            None
        };

        let configured_limit = memory
            .map(|snapshot| percent_of(snapshot.total_memory_bytes, self.memory_limit_percent))
            .unwrap_or(u64::MAX);
        if enforce_ram_budget && configured_limit == 0 {
            return Err(ScratchAdmissionError::new(
                ScratchAdmissionFailureKind::MemoryBudget,
                "scratch memory budget rounds down to zero bytes",
            ));
        }

        let mut active = self
            .active_reserved_bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active_before = *active;
        let ram_budget_remaining = if enforce_ram_budget {
            configured_limit.saturating_sub(active_before)
        } else {
            u64::MAX
        };
        let mem_available_remaining = if let Some(memory) = memory {
            memory.available_memory_bytes.saturating_sub(active_before)
        } else {
            u64::MAX
        };
        let filesystem_usable_remaining = filesystem.usable_available_bytes.saturating_sub(active_before);
        let ram_gate_remaining = ram_budget_remaining.min(mem_available_remaining);
        let admission_ceiling = if enforce_ram_budget {
            ram_gate_remaining.min(filesystem_usable_remaining)
        } else {
            filesystem_usable_remaining
        };

        if estimated_bytes > admission_ceiling {
            let (bottleneck, failure_kind) = if filesystem_usable_remaining <= ram_gate_remaining || !enforce_ram_budget {
                (
                    "scratch filesystem usable free space",
                    ScratchAdmissionFailureKind::FilesystemCapacity,
                )
            } else if mem_available_remaining <= ram_budget_remaining {
                (
                    "currently available memory",
                    ScratchAdmissionFailureKind::AvailableMemory,
                )
            } else {
                (
                    "configured RAM budget",
                    ScratchAdmissionFailureKind::MemoryBudget,
                )
            };
            let ram_description = if enforce_ram_budget {
                format!(
                    "RAM budget remaining {}, MemAvailable remaining {}, ",
                    format_bytes(ram_budget_remaining),
                    format_bytes(mem_available_remaining),
                )
            } else {
                "RAM budget gate disabled for non-tmpfs scratch, ".to_string()
            };
            return Err(ScratchAdmissionError::new(
                failure_kind,
                format!(
                    "estimated scratch peak {} exceeds admission ceiling {} ({bottleneck}; {}scratch filesystem usable remaining {}; scratch filesystem available {}, scratch filesystem headroom {}, scratch filesystem total {}, active reservations {}, scratch_memory_limit_percent={})",
                    format_bytes(estimated_bytes),
                    format_bytes(admission_ceiling),
                    ram_description,
                    format_bytes(filesystem_usable_remaining),
                    format_bytes(filesystem.available_bytes),
                    format_bytes(filesystem.headroom_bytes),
                    format_bytes(filesystem.total_bytes),
                    format_bytes(active_before),
                    self.memory_limit_percent,
                ),
            ));
        }

        *active = active_before.saturating_add(estimated_bytes);

        Ok(ScratchReservation {
            budget: self.clone(),
            bytes: estimated_bytes,
            admission: ScratchAdmissionSummary {
                memory_limit_percent: self.memory_limit_percent,
                enforce_ram_budget,
                configured_limit_bytes: if enforce_ram_budget { Some(configured_limit) } else { None },
                memory_available_bytes: memory.map(|snapshot| snapshot.available_memory_bytes),
                active_before_bytes: active_before,
                ram_budget_remaining_bytes: if enforce_ram_budget { Some(ram_budget_remaining) } else { None },
                mem_available_remaining_bytes: if enforce_ram_budget { Some(mem_available_remaining) } else { None },
                filesystem_total_bytes: filesystem.total_bytes,
                filesystem_available_bytes: filesystem.available_bytes,
                filesystem_headroom_bytes: filesystem.headroom_bytes,
                filesystem_usable_remaining_bytes: filesystem_usable_remaining,
                admission_ceiling_before_bytes: admission_ceiling,
            },
            log_context: None,
        })
    }

    fn release(&self, bytes: u64) -> u64 {
        let mut active = self
            .active_reserved_bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *active = active.saturating_sub(bytes);
        *active
    }

    #[cfg(test)]
    #[must_use]
    pub fn active_reserved_bytes(&self) -> u64 {
        *self
            .active_reserved_bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug)]
enum MemorySource {
    System,
    #[cfg(test)]
    Fixed {
        total_memory_bytes: u64,
        available_memory_bytes: u64,
    },
}

impl MemorySource {
    fn snapshot(&self) -> io::Result<MemorySnapshot> {
        match self {
            Self::System => read_proc_meminfo(),
            #[cfg(test)]
            Self::Fixed {
                total_memory_bytes,
                available_memory_bytes,
            } => Ok(MemorySnapshot {
                total_memory_bytes: *total_memory_bytes,
                available_memory_bytes: *available_memory_bytes,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MemorySnapshot {
    total_memory_bytes: u64,
    available_memory_bytes: u64,
}

#[derive(Debug)]
enum FilesystemSource {
    System,
    #[cfg(test)]
    Fixed {
        total_filesystem_bytes: u64,
        available_filesystem_bytes: u64,
        headroom_bytes: u64,
    },
}

impl FilesystemSource {
    fn snapshot(&self, scratch_root: &Path) -> io::Result<FilesystemSnapshot> {
        match self {
            Self::System => scratch_filesystem_snapshot(scratch_root),
            #[cfg(test)]
            Self::Fixed {
                total_filesystem_bytes,
                available_filesystem_bytes,
                headroom_bytes,
            } => Ok(FilesystemSnapshot::new(
                *total_filesystem_bytes,
                *available_filesystem_bytes,
                *headroom_bytes,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FilesystemSnapshot {
    total_bytes: u64,
    available_bytes: u64,
    headroom_bytes: u64,
    usable_available_bytes: u64,
}

impl FilesystemSnapshot {
    fn new(total_bytes: u64, available_bytes: u64, headroom_bytes: u64) -> Self {
        Self {
            total_bytes,
            available_bytes,
            headroom_bytes,
            usable_available_bytes: available_bytes.saturating_sub(headroom_bytes),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScratchAdmissionSummary {
    memory_limit_percent: u8,
    enforce_ram_budget: bool,
    configured_limit_bytes: Option<u64>,
    memory_available_bytes: Option<u64>,
    active_before_bytes: u64,
    ram_budget_remaining_bytes: Option<u64>,
    mem_available_remaining_bytes: Option<u64>,
    filesystem_total_bytes: u64,
    filesystem_available_bytes: u64,
    filesystem_headroom_bytes: u64,
    filesystem_usable_remaining_bytes: u64,
    admission_ceiling_before_bytes: u64,
}

impl ScratchAdmissionSummary {
    fn describe(&self) -> String {
        let ram_part = if self.enforce_ram_budget {
            format!(
                "configured RAM budget {}, MemAvailable {}, RAM budget remaining {}, MemAvailable remaining {}, ",
                format_bytes(self.configured_limit_bytes.unwrap_or_default()),
                format_bytes(self.memory_available_bytes.unwrap_or_default()),
                format_bytes(self.ram_budget_remaining_bytes.unwrap_or_default()),
                format_bytes(self.mem_available_remaining_bytes.unwrap_or_default()),
            )
        } else {
            "RAM budget gate disabled for non-tmpfs scratch, ".to_string()
        };
        format!(
            "scratch_memory_limit_percent={}, {}active reservations before admission {}, scratch filesystem usable remaining {} (available {}, headroom {}, total {})",
            self.memory_limit_percent,
            ram_part,
            format_bytes(self.active_before_bytes),
            format_bytes(self.filesystem_usable_remaining_bytes),
            format_bytes(self.filesystem_available_bytes),
            format_bytes(self.filesystem_headroom_bytes),
            format_bytes(self.filesystem_total_bytes),
        )
    }

    fn remaining_after_reservation(&self, reserved_bytes: u64) -> u64 {
        self.admission_ceiling_before_bytes.saturating_sub(reserved_bytes)
    }
}

#[derive(Debug, Clone)]
struct ScratchReservationLogContext {
    job_id: String,
    item_id: String,
    scratch_path: PathBuf,
}

#[derive(Debug)]
pub struct ScratchReservation {
    budget: Arc<ScratchMemoryBudget>,
    bytes: u64,
    admission: ScratchAdmissionSummary,
    log_context: Option<ScratchReservationLogContext>,
}

impl ScratchReservation {
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn admission_summary(&self) -> String {
        self.admission.describe()
    }

    #[must_use]
    pub fn remaining_after_reservation_bytes(&self) -> u64 {
        self.admission.remaining_after_reservation(self.bytes)
    }

    #[must_use]
    pub fn active_after_reservation_bytes(&self) -> u64 {
        self.admission.active_before_bytes.saturating_add(self.bytes)
    }

    #[must_use]
    pub fn with_log_context(
        mut self,
        job_id: impl Into<String>,
        item_id: impl Into<String>,
        scratch_path: PathBuf,
    ) -> Self {
        self.log_context = Some(ScratchReservationLogContext {
            job_id: job_id.into(),
            item_id: item_id.into(),
            scratch_path,
        });
        if let Some(context) = &self.log_context {
            log::debug!(
                "scratch reservation acquired: job_id={}, item_id={}, bytes_reserved={}, active_reserved_after={}, scratch_path={}",
                context.job_id,
                context.item_id,
                format_bytes(self.bytes),
                format_bytes(self.active_after_reservation_bytes()),
                context.scratch_path.display(),
            );
        }
        self
    }
}

impl Drop for ScratchReservation {
    fn drop(&mut self) {
        let active_after_release = self.budget.release(self.bytes);
        if let Some(context) = &self.log_context {
            log::debug!(
                "scratch reservation released: job_id={}, item_id={}, bytes_released={}, active_reserved_after={}, scratch_path={}",
                context.job_id,
                context.item_id,
                format_bytes(self.bytes),
                format_bytes(active_after_release),
                context.scratch_path.display(),
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchAdmissionFailureKind {
    Disabled,
    MemoryBudget,
    FilesystemCapacity,
    AvailableMemory,
}

#[derive(Debug, Clone)]
pub struct ScratchAdmissionError {
    kind: ScratchAdmissionFailureKind,
    reason: String,
}

impl ScratchAdmissionError {
    fn new(kind: ScratchAdmissionFailureKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ScratchAdmissionFailureKind {
        self.kind
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl std::fmt::Display for ScratchAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for ScratchAdmissionError {}

#[must_use]
pub fn estimate_job_peak_bytes(container: &Path, source_kind: Option<SourceKind>) -> u64 {
    let source_bytes = source_size_bytes(container).unwrap_or(DEFAULT_UNKNOWN_ESTIMATE_BYTES);
    match source_kind {
        Some(SourceKind::SingleFile) => source_bytes
            .saturating_mul(3)
            .saturating_add(128 * MIB)
            .max(256 * MIB),
        Some(SourceKind::CueImage) => source_bytes
            .saturating_mul(2)
            .saturating_add(512 * MIB)
            .max(1 * GIB),
        Some(SourceKind::Archive) => source_bytes
            .saturating_mul(4)
            .saturating_add(512 * MIB)
            .max(1 * GIB),
        Some(SourceKind::SacdIso) => source_bytes
            .saturating_mul(2)
            .saturating_add(1 * GIB)
            .max(4 * GIB),
        Some(SourceKind::DvdAudio) | Some(SourceKind::DvdVideo) | Some(SourceKind::BluRay) => {
            source_bytes
                .saturating_mul(2)
                .saturating_add(2 * GIB)
                .max(4 * GIB)
        }
        None => source_bytes
            .saturating_mul(3)
            .saturating_add(512 * MIB)
            .max(DEFAULT_UNKNOWN_ESTIMATE_BYTES),
    }
}

#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[must_use]
pub fn clamp_memory_limit_percent(value: u8) -> u8 {
    value.min(90)
}

pub fn write_staging_owner_marker(staging_root: &Path, run_lock_file_name: &str) -> io::Result<()> {
    if !is_safe_file_name(run_lock_file_name) || !is_run_lock_file_name(run_lock_file_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run lock file name must be a safe .*.run.lock file name",
        ));
    }

    fs::create_dir_all(staging_root)?;
    let marker = staging_root.join(STAGING_OWNER_MARKER);
    let temp_marker = staging_root.join(format!(
        ".{STAGING_OWNER_MARKER}.tmp-{}-{}",
        std::process::id(),
        unique_nanos(),
    ));
    let contents = format!(
        "run_lock={run_lock_file_name}\npid={}\ncreated_unix_nanos={}\n",
        std::process::id(),
        unique_nanos(),
    );

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_marker)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_marker, &marker)?;
        sync_directory_best_effort(staging_root);
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_marker);
    }
    write_result
}

fn sync_directory_best_effort(path: &Path) {
    if let Ok(file) = fs::File::open(path) {
        let _ = file.sync_all();
    }
}

fn source_size_bytes(path: &Path) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(DEFAULT_UNKNOWN_ESTIMATE_BYTES);
    }

    let mut total = 0_u64;
    let mut seen = 0_usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            } else if metadata.is_dir() {
                stack.push(entry.path());
            }
            seen += 1;
            if seen >= MAX_DIRECTORY_ESTIMATE_ENTRIES {
                return Ok(total.max(DEFAULT_UNKNOWN_ESTIMATE_BYTES));
            }
        }
    }
    Ok(total.max(DEFAULT_UNKNOWN_ESTIMATE_BYTES))
}

fn read_proc_meminfo() -> io::Result<MemorySnapshot> {
    let content = fs::read_to_string("/proc/meminfo")?;
    let mut total_kib = None;
    let mut available_kib = None;
    for line in content.lines() {
        if let Some(value) = parse_meminfo_kib(line, "MemTotal:") {
            total_kib = Some(value);
        } else if let Some(value) = parse_meminfo_kib(line, "MemAvailable:") {
            available_kib = Some(value);
        }
        if total_kib.is_some() && available_kib.is_some() {
            break;
        }
    }
    let total_memory_bytes = total_kib
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "MemTotal missing from /proc/meminfo"))?
        .saturating_mul(1024);
    let available_memory_bytes = available_kib
        .unwrap_or(total_kib.unwrap_or(0))
        .saturating_mul(1024);
    Ok(MemorySnapshot {
        total_memory_bytes,
        available_memory_bytes,
    })
}

fn parse_meminfo_kib(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?.trim();
    rest.split_whitespace().next()?.parse().ok()
}

fn percent_of(total: u64, percent: u8) -> u64 {
    total.saturating_mul(percent as u64) / 100
}

fn scratch_filesystem_snapshot(root: &Path) -> io::Result<FilesystemSnapshot> {
    fs::create_dir_all(root)?;
    let total = fs2::total_space(root)?;
    let available = fs2::available_space(root)?;
    Ok(FilesystemSnapshot::new(
        total,
        available,
        scratch_filesystem_headroom(total),
    ))
}

fn scratch_filesystem_headroom(total_bytes: u64) -> u64 {
    if total_bytes == 0 {
        return 0;
    }
    let five_percent = percent_of(total_bytes, 5);
    let bounded = five_percent.max(64 * MIB).min(GIB);
    bounded.min(total_bytes.saturating_sub(1))
}

fn verify_writable(root: &Path) -> io::Result<()> {
    let probe = root.join(format!(
        "{SCRATCH_PROBE_FILE}.{}.{}",
        std::process::id(),
        unique_nanos()
    ));
    fs::write(&probe, b"tonepoet scratch probe")?;
    fs::remove_file(probe)?;
    Ok(())
}

fn unique_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn cleanup_stale_staging_trees(staging_parent: &Path) -> io::Result<()> {
    fs::create_dir_all(staging_parent)?;
    let Some(_cleanup_lock) = try_acquire_cleanup_lock(staging_parent)? else {
        return Ok(());
    };

    let mut staging_dirs = Vec::new();
    let mut run_locks = Vec::new();
    for entry in fs::read_dir(staging_parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                log::warn!("could not inspect scratch staging entry {}: {err}", entry.path().display());
                continue;
            }
        };

        if file_type.is_file() && is_run_lock_file_name(&name) {
            run_locks.push(entry.path());
        } else if file_type.is_dir() && !name.starts_with('.') {
            staging_dirs.push(entry.path());
        }
    }

    for staging_dir in &staging_dirs {
        cleanup_stale_staging_dir(staging_parent, staging_dir);
    }

    for lock_path in run_locks {
        cleanup_orphaned_run_lock(staging_parent, &lock_path);
    }

    Ok(())
}

fn cleanup_stale_staging_dir(staging_parent: &Path, staging_dir: &Path) {
    let lock_file_name = read_staging_owner_lock_name(staging_dir)
        .or_else(|| inferred_run_lock_name_for_staging_dir(staging_dir));
    let Some(lock_file_name) = lock_file_name else {
        log::warn!(
            "could not determine run lock for scratch staging tree {}; skipping cleanup",
            staging_dir.display()
        );
        return;
    };
    let lock_path = staging_parent.join(&lock_file_name);

    let run_lock = match probe_existing_run_lock(&lock_path) {
        Ok(RunLockProbe::Held) => {
            log::debug!(
                "scratch stale cleanup: skipped active lock: staging_path={}, lock_holder={}",
                staging_dir.display(),
                lock_path.display()
            );
            return;
        }
        Ok(RunLockProbe::Missing) => RunLockProbe::Missing,
        Ok(RunLockProbe::Unlocked(file)) => RunLockProbe::Unlocked(file),
        Err(err) => {
            log::warn!(
                "could not check scratch run lock {} for stale staging tree {}; skipping cleanup: {err}",
                lock_path.display(),
                staging_dir.display()
            );
            return;
        }
    };

    // The legacy .run.lock is only a cheap local liveness hint. Final-family
    // ExecutionStaging descriptors are the authority for cross-session staging
    // lifetime, including RecoveryReserved state after every kernel holder has
    // closed. Deletion therefore requires ordinary shared mutation admission.
    let claim = match PathClaim::resolve_with_semantics(
        staging_dir,
        ClaimMode::Write,
        ClaimScope::Subtree,
        PathResolutionSemantics::NamespaceObject,
    ) {
        Ok(claim) => claim,
        Err(error) => {
            log::warn!(
                "scratch stale cleanup: could not resolve shared claim for {}; leaving tree intact: {error}",
                staging_dir.display(),
            );
            return;
        }
    };
    let admitted_path = claim.identity.resolved_io_path.clone();
    let _guard = match MutationClaimGuard::acquire_ephemeral(vec![claim]) {
        Ok(guard) => guard,
        Err(error) => {
            log::debug!(
                "scratch stale cleanup: shared staging ownership kept {} intact: {error}",
                staging_dir.display(),
            );
            return;
        }
    };

    if remove_stale_staging_tree(&admitted_path) {
        if let RunLockProbe::Unlocked(file) = run_lock {
            // Retire the old compatibility lock only while the same shared
            // mutation admission that authorized tree deletion remains held.
            remove_unlocked_stale_run_lock(file, &lock_path);
        }
    }
}

fn cleanup_orphaned_run_lock(staging_parent: &Path, lock_path: &Path) {
    let Some(lock_name) = lock_path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Some(staging_name) = staging_name_for_run_lock_name(lock_name) else {
        return;
    };
    let staging_dir = staging_parent.join(staging_name);
    if staging_dir.exists() {
        return;
    }

    match probe_existing_run_lock(lock_path) {
        Ok(RunLockProbe::Unlocked(file)) => remove_unlocked_stale_run_lock(file, lock_path),
        Ok(RunLockProbe::Held) | Ok(RunLockProbe::Missing) => {}
        Err(err) => log::warn!("could not inspect orphaned scratch run lock {}: {err}", lock_path.display()),
    }
}

fn remove_stale_staging_tree(path: &Path) -> bool {
    match fs::remove_dir_all(path) {
        Ok(()) => {
            log::info!("scratch stale cleanup: removed tree: staging_path={}", path.display());
            true
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => {
            log::warn!("could not remove stale scratch staging tree {}: {err}", path.display());
            false
        }
    }
}

fn remove_unlocked_stale_run_lock(file: fs::File, lock_path: &Path) {
    #[cfg(unix)]
    {
        match fs::remove_file(lock_path) {
            Ok(()) => log::info!("removed stale scratch run lock {}", lock_path.display()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => log::warn!("could not remove stale scratch run lock {}: {err}", lock_path.display()),
        }
        let _ = file.unlock();
    }

    #[cfg(not(unix))]
    {
        let _ = file.unlock();
        drop(file);
        match fs::remove_file(lock_path) {
            Ok(()) => log::info!("removed stale scratch run lock {}", lock_path.display()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => log::warn!("could not remove stale scratch run lock {}: {err}", lock_path.display()),
        }
    }
}

fn read_staging_owner_lock_name(staging_dir: &Path) -> Option<String> {
    let marker = staging_dir.join(STAGING_OWNER_MARKER);
    let content = match fs::read_to_string(&marker) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!(
                "could not read scratch staging owner marker in {}; falling back to inferred legacy lock name: {err}",
                staging_dir.display()
            );
            return None;
        }
    };
    for line in content.lines() {
        let Some(value) = line.strip_prefix("run_lock=") else {
            continue;
        };
        if is_safe_file_name(value) && is_run_lock_file_name(value) {
            return Some(value.to_string());
        }
        log::warn!(
            "invalid scratch staging owner marker run_lock value for {}; falling back to inferred legacy lock name: {}",
            staging_dir.display(),
            value
        );
        return None;
    }
    log::warn!(
        "scratch staging owner marker in {} did not contain a valid run_lock entry; falling back to inferred legacy lock name",
        staging_dir.display()
    );
    None
}

fn inferred_run_lock_name_for_staging_dir(staging_dir: &Path) -> Option<String> {
    let name = staging_dir.file_name()?.to_str()?;
    Some(format!(".{name}{RUN_LOCK_SUFFIX}"))
}

fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\')
}

fn is_run_lock_file_name(name: &str) -> bool {
    name.starts_with('.')
        && name.ends_with(RUN_LOCK_SUFFIX)
        && name.len() > 1 + RUN_LOCK_SUFFIX.len()
        && is_safe_file_name(name)
}

fn staging_name_for_run_lock_name(lock_name: &str) -> Option<&str> {
    let without_prefix = lock_name.strip_prefix('.')?;
    without_prefix.strip_suffix(RUN_LOCK_SUFFIX)
}

enum RunLockProbe {
    Missing,
    Held,
    Unlocked(fs::File),
}

fn probe_existing_run_lock(lock_path: &Path) -> io::Result<RunLockProbe> {
    let file = match fs::OpenOptions::new().read(true).write(true).open(lock_path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(RunLockProbe::Missing),
        Err(err) => return Err(err),
    };

    match file.try_lock_exclusive() {
        Ok(()) => Ok(RunLockProbe::Unlocked(file)),
        Err(err) if is_lock_contention(&err) => Ok(RunLockProbe::Held),
        Err(err) => Err(err),
    }
}

struct ScratchCleanupLock {
    file: fs::File,
    path: PathBuf,
}

impl Drop for ScratchCleanupLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

fn try_acquire_cleanup_lock(staging_parent: &Path) -> io::Result<Option<ScratchCleanupLock>> {
    let lock_path = staging_parent.join(SCRATCH_CLEANUP_LOCK_FILE);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(ScratchCleanupLock {
            file,
            path: lock_path,
        })),
        Err(err) if is_lock_contention(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn is_lock_contention(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(err.raw_os_error(), Some(11) | Some(35))
    }
    #[cfg(windows)]
    {
        err.raw_os_error() == Some(33)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn is_ram_backed_path(path: &Path) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let canonical = fs::canonicalize(path)?;
        let mounts = fs::read_to_string("/proc/mounts")?;
        let mut best_mount_len = 0_usize;
        let mut best_fs_type: Option<String> = None;
        for line in mounts.lines() {
            let mut fields = line.split_whitespace();
            let _source = fields.next();
            let Some(mount_point) = fields.next() else { continue; };
            let Some(fs_type) = fields.next() else { continue; };
            let mount_point = PathBuf::from(decode_mount_escape(mount_point));
            if canonical.starts_with(&mount_point) {
                let len = mount_point.as_os_str().len();
                if len >= best_mount_len {
                    best_mount_len = len;
                    best_fs_type = Some(fs_type.to_string());
                }
            }
        }
        Ok(matches!(best_fs_type.as_deref(), Some("tmpfs" | "ramfs")))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Ok(false)
    }
}

fn decode_mount_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'\\' && idx + 3 < bytes.len() {
            let octal = &value[idx + 1..idx + 4];
            if let Ok(byte) = u8::from_str_radix(octal, 8) {
                out.push(byte as char);
                idx += 4;
                continue;
            }
        }
        out.push(bytes[idx] as char);
        idx += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_reservations_release_on_drop() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_total_memory(50, 1_000));
        let first = budget.try_reserve(300, temp.path()).expect("first reservation");
        assert_eq!(budget.active_reserved_bytes(), 300);
        let second = budget.try_reserve(200, temp.path()).expect("second reservation");
        assert_eq!(budget.active_reserved_bytes(), 500);
        let err = budget
            .try_reserve(1, temp.path())
            .expect_err("configured memory budget should be exhausted");
        assert_eq!(err.kind(), ScratchAdmissionFailureKind::MemoryBudget);
        drop(first);
        assert_eq!(budget.active_reserved_bytes(), 200);
        drop(second);
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn zero_percent_disables_scratch_reservations() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_total_memory(0, 1_000));
        let err = budget
            .try_reserve(1, temp.path())
            .expect_err("zero percent disables scratch reservations");
        assert_eq!(err.kind(), ScratchAdmissionFailureKind::Disabled);
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn memory_limit_percent_is_clamped_to_ninety() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_total_memory(100, 1_000));
        let _held = budget.try_reserve(900, temp.path()).expect("first reservation within clamped 90% budget");
        let err = budget
            .try_reserve(1, temp.path())
            .expect_err("budget should be exhausted after reserving 90% of total");
        assert_eq!(err.kind(), ScratchAdmissionFailureKind::MemoryBudget);
    }

    #[test]
    fn available_memory_is_part_of_admission_gate() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
            90,
            10_000,
            399,
            10_000,
            10_000,
        ));
        let err = budget
            .try_reserve(400, temp.path())
            .expect_err("MemAvailable must gate tmpfs admission independently of configured budget");
        assert_eq!(err.kind(), ScratchAdmissionFailureKind::AvailableMemory);
        assert!(err.reason().contains("available memory") || err.reason().contains("MemAvailable"), "{err}");
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn filesystem_capacity_is_part_of_admission_gate() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
            90,
            10_000,
            10_000,
            10_000,
            400,
        ));
        let err = budget
            .try_reserve(401, temp.path())
            .expect_err("scratch filesystem free space must gate admission");
        assert_eq!(err.kind(), ScratchAdmissionFailureKind::FilesystemCapacity);
        assert!(err.reason().contains("scratch filesystem"), "{err}");
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn filesystem_gate_subtracts_active_reservations() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
            90,
            10_000,
            10_000,
            10_000,
            700,
        ));
        let first = budget.try_reserve(400, temp.path()).expect("first reservation");
        assert!(budget.try_reserve(301, temp.path()).is_err());
        drop(first);
        assert!(budget.try_reserve(700, temp.path()).is_ok());
    }

    #[test]
    fn non_ram_backed_scratch_admission_uses_filesystem_gate_without_ram_gate() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
            50,
            1_000,
            1_000,
            10_000,
            9_000,
        ));

        let reservation = budget
            .try_reserve_with_ram_gate(4_000, temp.path(), false)
            .expect("disk-backed scratch should be admitted by filesystem capacity even when it exceeds RAM budget");
        assert!(reservation
            .admission_summary()
            .contains("RAM budget gate disabled"));
        assert_eq!(budget.active_reserved_bytes(), 4_000);
        drop(reservation);
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn estimates_are_conservative_and_nonzero() {
        let temp = tempfile::tempdir().expect("temp dir");
        let input = temp.path().join("input.flac");
        fs::write(&input, vec![0_u8; 1024]).expect("input");
        assert!(estimate_job_peak_bytes(&input, Some(SourceKind::SingleFile)) >= 256 * MIB);
        assert!(estimate_job_peak_bytes(&input, Some(SourceKind::Archive)) >= GIB);
    }

    #[test]
    fn stale_unlocked_run_lock_does_not_block_cleanup() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_parent = temp.path().join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");
        let stale_dir = staging_parent.join("job-item");
        fs::create_dir_all(&stale_dir).expect("stale dir");
        fs::write(stale_dir.join(STAGING_OWNER_MARKER), "run_lock=.job-item.run.lock\n").expect("marker");
        fs::write(staging_parent.join(".job-item.run.lock"), b"").expect("stale lock");

        cleanup_stale_staging_trees(&staging_parent).expect("cleanup");

        assert!(!stale_dir.exists());
        assert!(!staging_parent.join(".job-item.run.lock").exists());
    }

    #[test]
    fn corrupt_owner_marker_falls_back_to_inferred_legacy_lock_name() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_parent = temp.path().join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");

        let stale_dir = staging_parent.join("job-item");
        fs::create_dir_all(&stale_dir).expect("stale dir");
        fs::write(stale_dir.join(STAGING_OWNER_MARKER), "run_lock=../../not-safe\n")
            .expect("corrupt marker");
        fs::write(staging_parent.join(".job-item.run.lock"), b"").expect("stale lock");

        cleanup_stale_staging_trees(&staging_parent).expect("cleanup");

        assert!(
            !stale_dir.exists(),
            "corrupt owner markers must not permanently protect stale scratch trees"
        );
        assert!(!staging_parent.join(".job-item.run.lock").exists());
    }

    #[test]
    fn owner_marker_write_is_atomic_and_parseable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_root = temp.path().join("job-item");
        fs::create_dir_all(&staging_root).expect("staging root");

        write_staging_owner_marker(&staging_root, ".job-item.run.lock").expect("marker write");

        assert_eq!(
            read_staging_owner_lock_name(&staging_root).as_deref(),
            Some(".job-item.run.lock")
        );
        let temp_markers: Vec<_> = fs::read_dir(&staging_root)
            .expect("read staging root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tonepoet-staging-owner.tmp"))
            .collect();
        assert!(temp_markers.is_empty(), "atomic marker writes should not leave temp files on success");
    }

    #[test]
    fn stale_cleanup_handles_legacy_markerless_tree_and_orphan_lock() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_parent = temp.path().join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");

        let legacy_dir = staging_parent.join("legacy-job-item");
        fs::create_dir_all(&legacy_dir).expect("legacy dir");
        fs::write(legacy_dir.join("partial.tmp"), b"intermediate bytes").expect("legacy payload");
        fs::write(staging_parent.join(".legacy-job-item.run.lock"), b"").expect("legacy stale lock");

        let orphan_lock = staging_parent.join(".orphan-job.run.lock");
        fs::write(&orphan_lock, b"").expect("orphan stale lock");

        cleanup_stale_staging_trees(&staging_parent).expect("cleanup");

        assert!(!legacy_dir.exists(), "legacy markerless stale tree should be removed by inferred lock name");
        assert!(
            !staging_parent.join(".legacy-job-item.run.lock").exists(),
            "unlocked legacy run lock should be removed with its tree"
        );
        assert!(!orphan_lock.exists(), "unlocked orphan run lock should be removed");
    }

    #[test]
    fn ensure_usable_runs_stale_cleanup_once_for_configured_scratch_parent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let scratch_root = temp.path().join("scratch");
        let staging_parent = scratch_root.join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");

        let stale_dir = staging_parent.join("job-item");
        fs::create_dir_all(&stale_dir).expect("stale dir");
        fs::write(stale_dir.join(STAGING_OWNER_MARKER), "run_lock=.job-item.run.lock\n").expect("marker");
        fs::write(staging_parent.join(".job-item.run.lock"), b"").expect("stale lock");

        let config = ScratchStagingConfig::new(scratch_root, 50);
        config.ensure_usable(&staging_parent).expect("scratch validation");

        assert!(!stale_dir.exists(), "first scratch validation should clean stale staging trees");
        assert!(!staging_parent.join(".job-item.run.lock").exists());
    }

    #[test]
    fn execution_staging_live_and_recovery_reserved_block_stale_cleanup_until_retired() {
        let temp = tempfile::tempdir().expect("temp dir");
        let _concurrency = crate::concurrency::install_scoped_test_coordination_root(
            &temp.path().join("claims"),
        );

        let staging_parent = temp.path().join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");
        let claimed_dir = staging_parent.join("claimed-job");
        fs::create_dir_all(&claimed_dir).expect("claimed staging dir");
        fs::write(
            claimed_dir.join(STAGING_OWNER_MARKER),
            "run_lock=.claimed-job.run.lock
",
        )
        .expect("claimed marker");
        fs::write(staging_parent.join(".claimed-job.run.lock"), b"").expect("old unlocked run lock");

        let unrelated_dir = staging_parent.join("unrelated-stale-job");
        fs::create_dir_all(&unrelated_dir).expect("unrelated staging dir");
        fs::write(
            unrelated_dir.join(STAGING_OWNER_MARKER),
            "run_lock=.unrelated-stale-job.run.lock
",
        )
        .expect("unrelated marker");
        fs::write(staging_parent.join(".unrelated-stale-job.run.lock"), b"")
            .expect("unrelated old run lock");

        let execution_id = uuid::Uuid::new_v4();
        let family = crate::concurrency::LeaseFamily::ExecutionStaging { execution_id };
        let claim = crate::concurrency::PathClaim::resolve_with_semantics(
            &claimed_dir,
            crate::concurrency::ClaimMode::Write,
            crate::concurrency::ClaimScope::Subtree,
            crate::concurrency::PathResolutionSemantics::NamespaceObject,
        )
        .expect("staging claim");
        let guard = crate::concurrency::MutationClaimGuard::acquire(family.clone(), vec![claim])
            .expect("live execution staging lease");
        let descriptor = guard.lease().descriptor_path().to_path_buf();

        cleanup_stale_staging_trees(&staging_parent).expect("cleanup while live");
        assert!(claimed_dir.exists(), "live ExecutionStaging must protect its tree");
        assert!(staging_parent.join(".claimed-job.run.lock").exists());
        assert!(!unrelated_dir.exists(), "busy candidate must not block disjoint stale cleanup");

        drop(guard);
        cleanup_stale_staging_trees(&staging_parent).expect("cleanup while recovery reserved");
        assert!(
            claimed_dir.exists(),
            "free final-family kernel lease remains RecoveryReserved until lifecycle retirement"
        );
        assert!(staging_parent.join(".claimed-job.run.lock").exists());

        crate::concurrency::retire_descriptor_after_lifecycle_release(&descriptor, &family)
            .expect("retire execution staging after lifecycle release");
        cleanup_stale_staging_trees(&staging_parent).expect("cleanup after lifecycle release");
        assert!(!claimed_dir.exists(), "retired staging ownership permits stale cleanup");
        assert!(!staging_parent.join(".claimed-job.run.lock").exists());

    }

    #[test]
    fn filesystem_admission_uses_minimum_of_ram_and_mount_space() {
        let temp = tempfile::tempdir().expect("temp dir");
        let budget = Arc::new(ScratchMemoryBudget::with_fixed_memory_and_filesystem(
            90,
            10 * GIB,
            10 * GIB,
            10 * GIB,
            256 * MIB,
        ));

        let err = budget
            .try_reserve(257 * MIB, temp.path())
            .expect_err("mount free space must cap admission even when RAM budget is ample");

        assert_eq!(err.kind(), ScratchAdmissionFailureKind::FilesystemCapacity);
        assert!(err.reason().contains("scratch filesystem"), "{err}");
        assert_eq!(budget.active_reserved_bytes(), 0);
    }

    #[test]
    fn held_run_lock_skips_only_active_tree() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_parent = temp.path().join(".tonepoet-staging");
        fs::create_dir_all(&staging_parent).expect("staging parent");

        let active_dir = staging_parent.join("active-job");
        fs::create_dir_all(&active_dir).expect("active dir");
        fs::write(active_dir.join(STAGING_OWNER_MARKER), "run_lock=.active-job.run.lock\n").expect("marker");
        let active_lock_path = staging_parent.join(".active-job.run.lock");
        let active_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&active_lock_path)
            .expect("active lock");
        active_lock.try_lock_exclusive().expect("hold active lock");

        let stale_dir = staging_parent.join("stale-job");
        fs::create_dir_all(&stale_dir).expect("stale dir");
        fs::write(stale_dir.join(STAGING_OWNER_MARKER), "run_lock=.stale-job.run.lock\n").expect("marker");
        fs::write(staging_parent.join(".stale-job.run.lock"), b"").expect("stale lock");

        cleanup_stale_staging_trees(&staging_parent).expect("cleanup");

        assert!(active_dir.exists());
        assert!(active_lock_path.exists());
        assert!(!stale_dir.exists());
        assert!(!staging_parent.join(".stale-job.run.lock").exists());
        let _ = active_lock.unlock();
    }
}

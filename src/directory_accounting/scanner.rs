//! Iterative, no-follow directory accounting. Only *finished* subtrees produce
//! persistent totals. Automatic scans have hard work budgets; explicitly
//! requested scans retain the existing exhaustive, cancellable walker.
use std::collections::{HashSet, VecDeque};
use std::fs::{self, Metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering}, Arc};
use std::time::{Duration, Instant, SystemTime};
use tempfile::NamedTempFile;
use sha2::{Digest, Sha256};
use super::model::{is_audio_name, CompleteMeasurement, Counts, Observation, ScanEnd, ScanProgress};
use super::repository::{path_key, Repository, SubtreeWrite};
use super::volume::{Inventory, NetworkProbeGate, Volume, VolumeBudgets, VolumeClass};

const MEMORY_CHILDREN_LIMIT: usize = 2048;
const BACKGROUND_SCRATCH_QUOTA: u64 = 64 * 1024 * 1024;

/// Caps *selected-subtree* automatic recursion, never parent/sibling fanout.
/// The existing background I/O throttle remains independent of these limits.
/// A Refresh or opted-in folder-size sort uses the same walker without caps.
#[derive(Debug, Clone, Copy)]
struct AutoScanLimits {
    entry_reads: u64,
    directories: u64,
    depth: usize,
    elapsed: Duration,
}
const AUTO_SCAN_LIMITS: AutoScanLimits = AutoScanLimits {
    entry_reads: 16_384,
    directories: 512,
    depth: 16,
    elapsed: Duration::from_secs(8),
};

struct AutoScanBudget {
    limits: AutoScanLimits,
    started: Instant,
    entry_reads: u64,
    directories: u64,
}
impl AutoScanBudget {
    fn new() -> Self { Self { limits: AUTO_SCAN_LIMITS, started: Instant::now(), entry_reads: 0, directories: 0 } }
    fn check_time(&self) -> Result<(), ScanExit> {
        if self.started.elapsed() >= self.limits.elapsed {
            Err(ScanExit::Limited("elapsed-time budget reached"))
        } else { Ok(()) }
    }
    fn enter_directory(&mut self, depth: usize) -> Result<(), ScanExit> {
        self.check_time()?;
        if depth > self.limits.depth { return Err(ScanExit::Limited("directory-depth budget reached")); }
        if self.directories >= self.limits.directories {
            return Err(ScanExit::Limited("directory-count budget reached"));
        }
        self.directories += 1;
        Ok(())
    }
    /// Reserve permission *before* advancing ReadDir: even obtaining its next
    /// entry may block on a remote filesystem. The EOF probe counts as a read;
    /// an exact-limit directory may conservatively require manual Refresh.
    fn before_next_entry(&mut self) -> Result<(), ScanExit> {
        self.check_time()?;
        if self.entry_reads >= self.limits.entry_reads {
            return Err(ScanExit::Limited("directory-entry budget reached"));
        }
        self.entry_reads += 1;
        Ok(())
    }
}

// A one-level preview is not recursion, but a single huge or remote directory
// can also monopolize a worker. Never publish its partial counts.
const PREVIEW_MAX_ENTRY_READS: u64 = 8_192;
const PREVIEW_MAX_ELAPSED: Duration = Duration::from_secs(4);

#[derive(Clone)]
struct ScratchBudget { used: Arc<AtomicU64>, priority: Arc<AtomicU8> }
impl ScratchBudget {
    fn charge(&self, size: u64) -> Result<(), String> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(size).ok_or("scratch usage overflow")?;
            if self.priority.load(Ordering::Acquire) < 2 && next > BACKGROUND_SCRATCH_QUOTA {
                return Err("background scratch quota exceeded (64 MiB); request explicit Folder Properties for this directory".into());
            }
            match self.used.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(()), Err(now) => current = now,
            }
        }
    }
    fn release(&self, size: u64) { self.used.fetch_sub(size, Ordering::AcqRel); }
}


/// Same path-key representation as the repository; preserves non-UTF8 Unix
/// names and non-Unicode Windows names when a wide directory is spooled.
fn decode_path(data: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)] {
        use std::os::unix::ffi::OsStringExt;
        if data.first() != Some(&1) { return Err("invalid scratch path encoding".into()); }
        Ok(std::ffi::OsString::from_vec(data[1..].to_vec()).into())
    }
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStringExt;
        if data.first() != Some(&2) || (data.len() - 1) % 2 != 0 { return Err("invalid scratch path encoding".into()); }
        let units = data[1..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>();
        Ok(std::ffi::OsString::from_wide(&units).into())
    }
    #[cfg(not(any(unix, windows)))] {
        if data.first() != Some(&3) { return Err("invalid scratch path encoding".into()); }
        Ok(PathBuf::from(std::str::from_utf8(&data[1..]).map_err(|e| e.to_string())?))
    }
}

/// Spills *directory references only* to an OS-managed temporary file. Never
/// accumulates all filenames or directory entries in memory.
struct ChildSpool {
    memory: VecDeque<PathBuf>,
    spilled: Option<NamedTempFile>,
    read_position: u64,
    retained: Vec<PathBuf>,
    scratch: ScratchBudget,
    stored_bytes: u64,
}
impl ChildSpool {
    fn new(scratch: ScratchBudget) -> Self { Self {
        memory: VecDeque::new(), spilled: None, read_position: 0,
        retained: Vec::new(), scratch, stored_bytes: 0,
    }}
    fn push(&mut self, path: PathBuf) -> Result<(), String> {
        if self.spilled.is_none() && self.memory.len() < MEMORY_CHILDREN_LIMIT {
            self.retained.push(path.clone());
            self.memory.push_back(path);
            return Ok(());
        }
        if self.spilled.is_none() {
            let file = NamedTempFile::new().map_err(|e| format!("create directory queue: {e}"))?;
            self.spilled = Some(file);
            while let Some(queued) = self.memory.pop_front() { self.write_path(&queued)?; }
            self.retained.clear();
        }
        self.write_path(&path)
    }
    fn write_path(&mut self, path: &Path) -> Result<(), String> {
        let encoded = path_key(path);
        let len = u32::try_from(encoded.len()).map_err(|_| "directory path too long".to_string())?;
        let charged = 4u64.checked_add(len as u64).ok_or("scratch quota overflow")?;
        self.scratch.charge(charged)?;
        self.stored_bytes += charged;
        let file = self.spilled.as_mut().ok_or("missing scratch file")?.as_file_mut();
        file.write_all(&len.to_le_bytes()).and_then(|_| file.write_all(&encoded))
            .map_err(|e| format!("write directory queue: {e}"))
    }
    fn next(&mut self) -> Result<Option<PathBuf>, String> {
        if let Some(path) = self.memory.pop_front() { return Ok(Some(path)); }
        let Some(spill) = self.spilled.as_mut() else { return Ok(None); };
        let file = spill.as_file_mut();
        file.seek(SeekFrom::Start(self.read_position)).map_err(|e| e.to_string())?;
        let mut size = [0; 4];
        match file.read_exact(&mut size) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Partial record is never treated as a clean EOF.
                let end = file.metadata().map_err(|e| e.to_string())?.len();
                return if self.read_position == end { Ok(None) } else { Err("truncated directory queue".into()) };
            }
            Err(e) => return Err(e.to_string()),
        }
        let len = u32::from_le_bytes(size) as usize;
        if len == 0 || len > 1024 * 1024 { return Err("corrupt directory queue record".into()); }
        let mut bytes = vec![0u8; len];
        file.read_exact(&mut bytes).map_err(|e| format!("truncated directory queue: {e}"))?;
        self.read_position = self.read_position.checked_add(4 + len as u64).ok_or("queue offset overflow")?;
        decode_path(&bytes).map(Some)
    }
    /// Replay exact child identities after postorder validation without
    /// retaining arbitrary fanout in RAM. Used by the atomic edge publisher.
    fn each_child(&self, mut visit: impl FnMut(PathBuf) -> Result<(), String>) -> Result<(), String> {
        if let Some(spilled) = &self.spilled {
            let mut reader = spilled.as_file().try_clone().map_err(|e| e.to_string())?;
            reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            loop {
                let mut len = [0u8; 4];
                match reader.read_exact(&mut len) {
                    Ok(()) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        if reader.stream_position().map_err(|e| e.to_string())? - 0 == self.stored_bytes { break; }
                        return Err("truncated scratch edge queue".into());
                    }
                    Err(e) => return Err(e.to_string()),
                }
                let size = u32::from_le_bytes(len) as usize;
                if size == 0 || size > 1024 * 1024 { return Err("corrupt scratch edge queue".into()); }
                let mut value = vec![0u8; size];
                reader.read_exact(&mut value).map_err(|e| e.to_string())?;
                visit(decode_path(&value)?)?;
            }
        } else {
            for path in &self.retained { visit(path.clone())?; }
        }
        Ok(())
    }

}
impl Drop for ChildSpool {
    fn drop(&mut self) { self.scratch.release(self.stored_bytes); }
}


#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<SystemTime>,
    length: u64,
    #[cfg(unix)] dev: u64,
    #[cfg(unix)] ino: u64,
    #[cfg(unix)] ctime: i64,
    #[cfg(unix)] ctime_nsec: i64,
}
fn fingerprint(meta: &Metadata) -> Fingerprint {
    #[cfg(unix)] use std::os::unix::fs::MetadataExt;
    Fingerprint {
        modified: meta.modified().ok(), length: meta.len(),
        #[cfg(unix)] dev: meta.dev(),
        #[cfg(unix)] ino: meta.ino(),
        #[cfg(unix)] ctime: meta.ctime(),
        #[cfg(unix)] ctime_nsec: meta.ctime_nsec(),
    }
}
/// Versioned metadata fingerprint. It is an identity/validation hint, not a
/// replacement for traversing descendants when notifications are unavailable.
fn identity_bytes(meta: &Metadata) -> Vec<u8> {
    let fingerprint = fingerprint(meta);
    format!("v1:{fingerprint:?}").into_bytes()
}

/// Commutative SHA-256 entry multiset: independent of `read_dir` ordering,
/// bounded by 64 bytes even for a million direct children. A fingerprint
/// includes native name bytes, kind, logical size and metadata change stamps.
fn add_entry_digest(digest: &mut [u8;64], path: &Path, meta: &Metadata) {
    let mut hasher = Sha256::new();
    hasher.update(path_key(path));
    let kind = if is_excluded(meta) { 1 } else if meta.is_file() { 2 }
        else if meta.is_dir() { 3 } else { 4 };
    hasher.update([kind]);
    hasher.update(meta.len().to_le_bytes());
    hasher.update(format!("{:?}", fingerprint(meta)).as_bytes());
    let hash = hasher.finalize();
    for (i, b) in hash.iter().enumerate() {
        digest[i] ^= *b;
        digest[32+i] = digest[32+i].wrapping_add(*b);
    }
}

pub(super) fn is_excluded(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() { return true; }
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT: junctions and other redirects are never followed.
        if meta.file_attributes() & 0x400 != 0 { return true; }
    }
    false
}

struct Frame {
    path: PathBuf,
    before: Fingerprint,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    started: SystemTime,
    total: Counts,
    local: Counts,
    digest: [u8;64],
    root_identity: Vec<u8>,
    children: ChildSpool,
    expected_epoch: i64,
    observation: Observation,
}

/// One cooperative governor per worker. Background metadata steps draw from
/// shared per-volume buckets; foreground steps only check cancellation and
/// periodically yield. Sleepers never hold locks.
pub struct RateGovernor {
    cancel: Arc<AtomicBool>,
    pub(super) priority: Arc<AtomicU8>,
    background_pause: Arc<AtomicBool>,
    background_busy: Arc<AtomicBool>,
    budgets: Arc<VolumeBudgets>,
    network_gate: Arc<NetworkProbeGate>,
    inventory: Inventory,
    current_volume: Volume,
    current_directory: Option<PathBuf>,
    last_network_probe: Instant,
    scratch: ScratchBudget,
    operations: u64,
    auto_budget: AutoScanBudget,
    #[cfg(test)] unlimited_fixture: bool,
}
impl RateGovernor {
    #[cfg(test)]
    pub fn new(cancel: Arc<AtomicBool>, priority: Arc<AtomicU8>, pause: Arc<AtomicBool>, busy: Arc<AtomicBool>, _slow: bool) -> Self {
        let mut value = Self::with_budgets(cancel, priority, pause, busy, Arc::new(VolumeBudgets::default()), Arc::new(NetworkProbeGate::default()), Inventory::inspect());
        value.unlimited_fixture = true;
        value
    }
    pub fn with_budgets(cancel: Arc<AtomicBool>, priority: Arc<AtomicU8>, pause: Arc<AtomicBool>, busy: Arc<AtomicBool>, budgets: Arc<VolumeBudgets>, network_gate: Arc<NetworkProbeGate>, inventory: Inventory) -> Self {
        Self {
            scratch: ScratchBudget { used: Arc::new(AtomicU64::new(0)), priority: priority.clone() },
            cancel, priority, background_pause: pause, background_busy: busy,
            budgets, network_gate, inventory,
            current_volume: Volume { key: "unclassified".into(), class: VolumeClass::NetworkOrUnknown },
            current_directory: None, last_network_probe: Instant::now(), operations: 0,
            auto_budget: AutoScanBudget::new(),
            #[cfg(test)] unlimited_fixture: false,
        }
    }
    fn budget_active(&self) -> bool { self.priority.load(Ordering::Acquire) < 2 }
    fn check_budget(&self) -> Result<(), ScanExit> {
        if self.cancel.load(Ordering::Acquire) { return Err(ScanExit::Cancelled); }
        if self.budget_active() { self.auto_budget.check_time()?; }
        Ok(())
    }
    fn before_next_entry(&mut self) -> Result<(), ScanExit> {
        self.check_budget()?;
        if self.budget_active() { self.auto_budget.before_next_entry()?; }
        Ok(())
    }
    pub fn enter_directory(&mut self, path: &Path, depth: usize) -> Result<(), ScanExit> {
        self.check_budget()?;
        if self.budget_active() { self.auto_budget.enter_directory(depth)?; }
        self.current_volume = self.inventory.for_path(path);
        self.current_directory = Some(path.to_path_buf());
        #[cfg(test)]
        if self.unlimited_fixture { return Ok(()); }
        if self.priority.load(Ordering::Acquire) < 2
            && self.current_volume.class == VolumeClass::NetworkOrUnknown {
            self.check_network_background(path)?;
        }
        Ok(())
    }
    fn check_network_background(&mut self, path: &Path) -> Result<(), ScanExit> {
        let permitted = self.network_gate.permit(&self.current_volume, path, &self.cancel);
        self.last_network_probe = Instant::now();
        match permitted {
            Some(true) => Ok(()),
            // A Folder Properties request can promote this worker while the
            // probe is doing synchronous metadata I/O.
            Some(false) if self.priority.load(Ordering::Acquire) >= 2 => Ok(()),
            Some(false) => Err(ScanExit::Deferred),
            None => Err(ScanExit::Cancelled),
        }
    }
    pub fn tick(&mut self, cost: f64) -> Result<(), ScanExit> {
        loop {
            self.check_budget()?;
            #[cfg(test)]
            if self.unlimited_fixture { return Ok(()); }
            let priority = self.priority.load(Ordering::Acquire);
            let foreground = priority >= 2;
            // Priority 3 is opt-in Browse accounting: do not rate-limit an
            // expressly requested folder walk, but yield to Properties and
            // active playback/analysis just like other background work.
            if priority != 2 && (self.background_pause.load(Ordering::Acquire)
                || self.background_busy.load(Ordering::Acquire)) {
                std::thread::sleep(Duration::from_millis(15));
                continue;
            }
            // Foreground scans are user-requested. Never debit the shared
            // background bucket or sleep for token replenishment; retained
            // cancellation checks and cooperative yields still apply.
            if foreground {
                self.operations = self.operations.wrapping_add(1);
                if self.operations % 128 == 0 { std::thread::yield_now(); }
                return Ok(());
            }
            // Reprobe during a very wide directory too: otherwise a single
            // read_dir that takes hours could retain an obsolete fast-link
            // admission indefinitely without entering another directory.
            if self.current_volume.class == VolumeClass::NetworkOrUnknown
                && self.last_network_probe.elapsed() >= Duration::from_secs(60) {
                if let Some(path) = self.current_directory.clone() {
                    self.check_network_background(&path)?;
                }
            }
            // Priority can change while an sshfs metadata probe blocks.
            if self.priority.load(Ordering::Acquire) >= 2 { continue; }
            let delay = self.budgets.charge(&self.current_volume, false, cost);
            if delay.is_zero() {
                self.operations = self.operations.wrapping_add(1);
                if self.operations % 128 == 0 { std::thread::yield_now(); }
                return Ok(());
            }
            std::thread::sleep(delay.min(Duration::from_millis(20)));
        }
    }
}

#[derive(Debug)]
pub enum ScanExit { Cancelled, Deferred, Limited(&'static str), Failed(String), Changed(String) }
impl ScanExit {
    fn into_end(self, progress: ScanProgress) -> ScanEnd {
        match self {
            Self::Cancelled => ScanEnd::Cancelled(progress),
            Self::Deferred => ScanEnd::Deferred(progress),
            Self::Limited(reason) => ScanEnd::Limited { reason, progress },
            Self::Failed(reason) => ScanEnd::Failed { reason, progress },
            Self::Changed(reason) => ScanEnd::Changed { reason, progress },
        }
    }
}
impl From<String> for ScanExit { fn from(value: String) -> Self { Self::Failed(value) } }

fn io_error(path: &Path, what: &str, error: impl std::fmt::Display) -> ScanExit {
    ScanExit::Failed(format!("{}: {what}: {error}", path.display()))
}

fn inspect_directory(path: &Path, governor: &mut RateGovernor, progress: &mut ScanProgress,
    mut on_progress: impl FnMut(&ScanProgress),
) -> Result<(Counts, ChildSpool, Fingerprint, Option<SystemTime>, Option<SystemTime>, Observation, [u8;64], Vec<u8>), ScanExit> {
    governor.tick(1.0)?;
    let before_meta = fs::symlink_metadata(path).map_err(|e| io_error(path, "read directory attributes", e))?;
    if is_excluded(&before_meta) || !before_meta.is_dir() {
        return Err(ScanExit::Changed(format!("{} is no longer a real directory", path.display())));
    }
    let before = fingerprint(&before_meta);
    let root_identity = identity_bytes(&before_meta);
    let created = before_meta.created().ok();
    let modified = before_meta.modified().ok();
    governor.tick(4.0)?;
    let mut read = fs::read_dir(path).map_err(|e| io_error(path, "list directory", e))?;
    let mut counts = Counts::default();
    let mut digest = [0u8;64];
    let mut children = ChildSpool::new(governor.scratch.clone());
    loop {
        // Charge/check *before* ReadDir::next(), not after a potentially
        // blocking remote fetch. No new metadata I/O starts after exhaustion.
        governor.before_next_entry()?;
        governor.tick(2.0)?; // charge the enumeration AND metadata lookup
        let Some(result) = read.next() else { break; };
        let entry = result.map_err(|e| io_error(path, "enumerate entry", e))?;
        let entry_path = entry.path();
        let meta = fs::symlink_metadata(&entry_path).map_err(|e| io_error(&entry_path, "read entry", e))?;
        add_entry_digest(&mut digest, &entry_path, &meta);
        if is_excluded(&meta) {
            counts.excluded_links = counts.excluded_links.checked_add(1).ok_or_else(|| ScanExit::Failed("link count overflow".into()))?;
            progress.links_skipped = progress.links_skipped.checked_add(1).ok_or_else(|| ScanExit::Failed("link count overflow".into()))?;
        } else if meta.is_file() {
            let audio = is_audio_name(&entry_path);
            counts.add_file(meta.len(), audio).map_err(|e| ScanExit::Failed(e.into()))?;
            progress.files_examined = progress.files_examined.checked_add(1).ok_or_else(|| ScanExit::Failed("file count overflow".into()))?;
            if audio {
                progress.audio_examined = progress.audio_examined.checked_add(1).ok_or_else(|| ScanExit::Failed("audio count overflow".into()))?;
            }
            progress.bytes_examined = progress.bytes_examined.checked_add(meta.len() as u128).ok_or_else(|| ScanExit::Failed("byte count overflow".into()))?;
        } else if meta.is_dir() {
            children.push(entry_path).map_err(ScanExit::Failed)?;
            counts.folders = counts.folders.checked_add(1).ok_or_else(|| ScanExit::Failed("folder count overflow".into()))?;
            progress.folders_discovered = progress.folders_discovered.checked_add(1).ok_or_else(|| ScanExit::Failed("folder count overflow".into()))?;
        } else {
            counts.other = counts.other.checked_add(1).ok_or_else(|| ScanExit::Failed("entry count overflow".into()))?;
        }
        on_progress(progress);
    }
    // ReadDir is dropped at function return. Detect typical concurrent changes
    // to the enumerated directory, but do not assert snapshot isolation.
    governor.tick(1.0)?;
    let after_meta = fs::symlink_metadata(path).map_err(|e| io_error(path, "recheck directory", e))?;
    if is_excluded(&after_meta) || !after_meta.is_dir() || before != fingerprint(&after_meta) {
        return Err(ScanExit::Changed(format!("{} changed during enumeration", path.display())));
    }
    let observation = if before.modified.is_none() { Observation::Indeterminate } else { Observation::AsObserved };
    Ok((counts, children, before, created, modified, observation, digest, root_identity))
}

/// Count exactly one directory level for a settled Browse highlight. Unlike
/// `scan`, this never descends, writes SQL, or publishes a recursive total.
/// Errors and cancellations produce no counts, not partial numbers. It uses
/// the same no-follow and audio-extension policy as the full walker.
pub(super) fn preview_local_counts(path: &Path, cancel: &AtomicBool) -> Result<Option<Counts>, String> {
    preview_local_counts_bounded(path, cancel, PREVIEW_MAX_ENTRY_READS, PREVIEW_MAX_ELAPSED)
}

fn preview_local_counts_bounded(path: &Path, cancel: &AtomicBool,
    entry_limit: u64, time_limit: Duration) -> Result<Option<Counts>, String> {
    let started = Instant::now();
    if cancel.load(Ordering::Acquire) { return Ok(None); }
    let before_meta = fs::symlink_metadata(path)
        .map_err(|e| format!("{}: read directory attributes: {e}", path.display()))?;
    if !before_meta.is_dir() || is_excluded(&before_meta) {
        return Err(format!("{} is not a real directory", path.display()));
    }
    let before = fingerprint(&before_meta);
    if started.elapsed() >= time_limit { return Ok(None); }
    let mut entries = fs::read_dir(path)
        .map_err(|e| format!("{}: list directory: {e}", path.display()))?;
    let mut counts = Counts::default();
    for _ in 0..entry_limit {
        if cancel.load(Ordering::Acquire) || started.elapsed() >= time_limit { return Ok(None); }
        let Some(entry) = entries.next() else {
            if cancel.load(Ordering::Acquire) || started.elapsed() >= time_limit { return Ok(None); }
            let after_meta = fs::symlink_metadata(path)
                .map_err(|e| format!("{}: recheck directory: {e}", path.display()))?;
            if !after_meta.is_dir() || is_excluded(&after_meta) || fingerprint(&after_meta) != before {
                return Err(format!("{} changed during enumeration", path.display()));
            }
            return Ok(Some(counts));
        };
        let entry = entry.map_err(|e| format!("{}: enumerate entry: {e}", path.display()))?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)
            .map_err(|e| format!("{}: read entry: {e}", child.display()))?;
        if is_excluded(&metadata) {
            counts.excluded_links = counts.excluded_links.checked_add(1)
                .ok_or("link count overflow")?;
        } else if metadata.is_file() {
            counts.add_file(metadata.len(), is_audio_name(&child))?;
        } else if metadata.is_dir() {
            counts.folders = counts.folders.checked_add(1)
                .ok_or("folder count overflow")?;
        } else {
            counts.other = counts.other.checked_add(1)
                .ok_or("entry count overflow")?;
        }
    }
    // The budget was consumed before an EOF probe could establish completeness.
    Ok(None)
}

/// Foreground and opted-in traversals are exhaustive (and cancellable).
/// Background recursion is bounded; a limited root is never a complete total.
pub fn scan<F, L>(root: &Path, generation: i64, repo: &mut Option<Repository>, governor: &mut RateGovernor,
    mut progress_event: F, mut local_event: L,
) -> ScanEnd
where F: FnMut(ScanProgress), L: FnMut(Counts) {
    let mut progress = ScanProgress::default();
    let root = root.to_path_buf();
    let root_started = SystemTime::now();
    let mut stack: Vec<Frame> = Vec::new();
    let mut next = Some(root.clone());
    let mut last_report = Instant::now() - Duration::from_secs(1);
    let report_priority = governor.priority.clone();
    let mut report = |progress: &ScanProgress| {
        let delay = if report_priority.load(Ordering::Relaxed) >= 2 { 250 } else { 500 };
        if last_report.elapsed() >= Duration::from_millis(delay) {
            progress_event(progress.clone());
            last_report = Instant::now();
        }
    };
    let mut persistence_error = None;
    let mut all_saved = repo.is_some();
    let mut pending_writes: Vec<SubtreeWrite> = Vec::with_capacity(32);
    let mut batches_until_prune: u8 = 16;
    let expected_sequence = match repo.as_ref().map(|r| r.mutation_sequence()).transpose() {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            all_saved = false;
            persistence_error.get_or_insert(error);
            -1
        }
    };
    let mut qualified_reuses: u64 = 0;
    let mut active_ids: HashSet<(u64,u64)> = HashSet::new();

    loop {
        if let Some(path) = next.take() {
            progress.current = Some(path.clone());
            let started = SystemTime::now();
            // Reject a new directory before issuing even a repository lookup.
            if let Err(exit) = governor.enter_directory(&path, stack.len()) {
                return exit.into_end(progress);
            }
            let epoch = match repo.as_ref().map(|r| r.epoch(&path)).transpose() {
                Ok(v) => v.unwrap_or(0),
                Err(error) => { all_saved = false; persistence_error.get_or_insert(error); 0 }
            };
            let inspected = inspect_directory(&path, governor, &mut progress, |p| report(p));
            let (counts, children, before, created, modified, observation, digest, root_identity) = match inspected {
                Ok(value) => value,
                Err(exit) => return exit.into_end(progress),
            };
            #[cfg(unix)] {
                if !active_ids.insert((before.dev,before.ino)) {
                    return ScanEnd::Failed { reason: format!("directory cycle or duplicate mount at {}", path.display()), progress };
                }
            }
            if path == root { local_event(counts); }
            progress.folders_examined = match progress.folders_examined.checked_add(1) {
                Some(v) => v,
                None => return ScanEnd::Failed { reason: "folder count overflow".into(), progress },
            };
            stack.push(Frame {
                path, before, created, modified, started, total: counts, local: counts,
                digest, root_identity, children, expected_epoch: epoch, observation,
            });
        }
        let Some(frame) = stack.last_mut() else {
            return ScanEnd::Failed { reason: "accounting stack unexpectedly empty".into(), progress };
        };
        if let Err(exit) = governor.check_budget() { return exit.into_end(progress); }
        match frame.children.next() {
            Ok(Some(child)) => { next = Some(child); continue; }
            Err(reason) => return ScanEnd::Failed { reason, progress },
            Ok(None) => {},
        }
        // All children are accounted for; only now is this subtree complete.
        let complete_frame = stack.pop().expect("checked nonempty");
        #[cfg(unix)] { active_ids.remove(&(complete_frame.before.dev, complete_frame.before.ino)); }
        if let Err(exit) = governor.tick(1.0) { return exit.into_end(progress); }
        let final_meta = match fs::symlink_metadata(&complete_frame.path) {
            Ok(m) => m,
            Err(e) => return ScanEnd::Changed { reason: format!("{} disappeared before completion: {e}", complete_frame.path.display()), progress },
        };
        if is_excluded(&final_meta) || !final_meta.is_dir() || fingerprint(&final_meta) != complete_frame.before {
            return ScanEnd::Changed { reason: format!("{} changed before completion", complete_frame.path.display()), progress };
        }
        // Every eligible child has just been fully walked and checked in this
        // request. Only now may a historic completed aggregate be reused,
        // provided its local digest, identity, edges and independently observed
        // recursive counters agree. No blind subtree skip without watchers.
        let qualified = match repo.as_mut() {
            Some(repository) => repository.qualified_counts(&complete_frame.path,
                &complete_frame.local, &complete_frame.digest, &complete_frame.root_identity,
                &complete_frame.total),
            None => Ok(None),
        };
        let reused = match qualified {
            Ok(Some(value)) => { qualified_reuses = qualified_reuses.saturating_add(1); Some(value) },
            Ok(None) => None,
            Err(error) => { all_saved = false; persistence_error.get_or_insert(error); None },
        };
        let complete = CompleteMeasurement {
            path: complete_frame.path.clone(), counts: reused.unwrap_or(complete_frame.total),
            started: if complete_frame.path == root { root_started } else { complete_frame.started },
            finished: SystemTime::now(), created: complete_frame.created, modified: complete_frame.modified,
            generation, observation: complete_frame.observation,
        };
        let mut child_paths = if complete_frame.local.folders <= 256 { Some(Vec::new()) } else { None };
        if let Some(paths) = child_paths.as_mut() {
            if let Err(reason) = complete_frame.children.each_child(|path| { paths.push(path); Ok(()) }) {
                return ScanEnd::Failed { reason, progress };
            }
            if paths.len() as u128 != complete_frame.local.folders {
                return ScanEnd::Failed { reason: "child-edge spool disagrees with enumerated folder count".into(), progress };
            }
        }
        pending_writes.push(SubtreeWrite {
            value: complete.clone(), epoch: complete_frame.expected_epoch,
            local: complete_frame.local, digest: complete_frame.digest,
            root_identity: complete_frame.root_identity, children: child_paths,
        });
        // Bound each SQLite transaction by both subtree count and number of
        // edges. A high-fanout batch must not pin the shared application DB
        // for thousands of inserts at once.
        let edge_batch_size: usize = pending_writes.iter()
            .map(|write| write.children.as_ref().map_or(0, Vec::len)).sum();
        if pending_writes.len() == 32 || edge_batch_size >= 512 || stack.is_empty() {
            // A completed subtree can be staged while Cancel is requested.
            // Recheck immediately before committing the next bounded batch.
            // Earlier *complete child* batches may already exist; an
            // interrupted root is never published as a complete aggregate.
            if governor.cancel.load(Ordering::Acquire) {
                return ScanEnd::Cancelled(progress);
            }
            if let Some(repository) = repo.as_mut() {
                match repository.put_batch_at_sequence(&pending_writes, expected_sequence) {
                    Ok(results) if results.iter().all(|&saved| saved) => {},
                    Ok(_) => { all_saved = false; persistence_error.get_or_insert("invalidation or newer measurement prevented cache publication".into()); }
                    Err(error) => { all_saved = false; persistence_error.get_or_insert(error); }
                }
                // Enforce retention during a million-folder traversal, not
                // only on shutdown or after the root completes. A single
                // eviction sweep handles a limited number of rows.
                batches_until_prune = batches_until_prune.saturating_sub(1);
                if batches_until_prune == 0 {
                    if let Err(error) = repository.prune() {
                        log::warn!("directory accounting incremental retention: {error}");
                    }
                    batches_until_prune = 16;
                }
            }
            pending_writes.clear();
        }
        if let Some(parent) = stack.last_mut() {
            if let Err(error) = parent.total.merge(&complete.counts) {
                return ScanEnd::Failed { reason: error.into(), progress };
            }
            // A child with indeterminate stamps cannot prove temporal stability.
            if complete.observation == Observation::Indeterminate { parent.observation = Observation::Indeterminate; }
        } else {
            // Drop the rate-throttled progress callback's mutable borrow
            // before emitting the terminal progress update.
            drop(report);
            log::debug!("directory accounting {}: {} fully validated cached subtrees", root.display(), qualified_reuses);
            progress_event(progress);
            return ScanEnd::Complete { value: complete, saved: all_saved, persistence_error };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU8};
    fn execute(path: &Path, cancel: bool) -> ScanEnd {
        let c = Arc::new(AtomicBool::new(cancel));
        let priority = Arc::new(AtomicU8::new(2));
        let pause = Arc::new(AtomicBool::new(false));
        let mut governor = RateGovernor::new(c, priority, pause, Arc::new(AtomicBool::new(false)), false);
        scan(path, 1, &mut None, &mut governor, |_| {}, |_| {})
    }
    #[test]
    fn deep_tree_and_symlinks_do_not_produce_partial_totals() {
        let dir = tempfile::tempdir().unwrap();
        let mut current = dir.path().to_path_buf();
        for _ in 0..9 { current = current.join("album"); fs::create_dir(&current).unwrap(); }
        fs::write(current.join("song.flac"), vec![3u8; 2048]).unwrap();
        #[cfg(unix)] std::os::unix::fs::symlink(dir.path(), current.join("cycle")).unwrap();
        match execute(dir.path(), false) {
            ScanEnd::Complete { value, .. } => {
                assert_eq!(value.counts.folders, 9);
                assert_eq!(value.counts.files, 1);
                assert_eq!(value.counts.bytes, 2048);
                assert_eq!(value.counts.audio, 1);
                #[cfg(unix)] assert_eq!(value.counts.excluded_links, 1);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
    /// Original regression: a time/depth bounded walk might see a few KiB at
    /// the root and miss a large collection deeper than four directories.
    #[test]
    fn hundreds_of_descendants_never_collapse_into_a_small_shallow_subtotal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("cover.txt"), vec![0u8; 420 * 1024]).unwrap();
        let mut expected = (420 * 1024) as u128;
        for artist in 0..75 {
            let track_dir = dir.path().join(format!("artist-{artist}"))
                .join("year").join("album").join("disc").join("master");
            fs::create_dir_all(&track_dir).unwrap();
            let file = fs::File::create(track_dir.join("track.flac")).unwrap();
            file.set_len(16 * 1024 * 1024).unwrap(); // sparse fixture; no giant allocation
            expected += 16 * 1024 * 1024;
        }
        match execute(dir.path(), false) {
            ScanEnd::Complete { value, .. } => {
                assert_eq!(value.counts.folders, 75 * 5);
                assert_eq!(value.counts.files, 76);
                assert_eq!(value.counts.audio, 75);
                assert_eq!(value.counts.bytes, expected);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
    #[cfg(unix)]
    #[test]
    fn hard_links_count_once_per_directory_entry_and_broken_links_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one.flac");
        fs::write(&first, vec![0u8; 100]).unwrap();
        fs::hard_link(&first, dir.path().join("two.flac")).unwrap();
        std::os::unix::fs::symlink("missing", dir.path().join("dead_link")).unwrap();
        match execute(dir.path(), false) {
            ScanEnd::Complete { value, .. } => {
                assert_eq!(value.counts.files, 2);
                assert_eq!(value.counts.bytes, 200);
                assert_eq!(value.counts.audio, 2);
                assert_eq!(value.counts.excluded_links, 1);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
    #[test]
    fn nested_file_size_change_invalidates_cached_recursive_count() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("cache.db");
        crate::db::Database::open_path(&db).unwrap();
        let root = temp.path().join("Music");
        let nested = root.join("Artist/Album");
        fs::create_dir_all(&nested).unwrap();
        let song = nested.join("song.flac");
        fs::write(&song, vec![0u8; 12]).unwrap();
        let root_mtime = fs::metadata(&root).unwrap().modified().ok();
        let mut repo = Some(Repository::open(&db).unwrap());
        let cancel = Arc::new(AtomicBool::new(false));
        let priority = Arc::new(AtomicU8::new(2));
        let mut governor = RateGovernor::new(cancel, priority,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), false);
        let first_generation = repo.as_mut().unwrap().allocate_generation().unwrap();
        let first = scan(&root, first_generation, &mut repo, &mut governor, |_|{}, |_|{});
        assert!(matches!(&first, ScanEnd::Complete { saved: true, .. }), "{first:?}");
        fs::write(&song, vec![0u8; 140]).unwrap();
        assert_eq!(fs::metadata(&root).unwrap().modified().ok(), root_mtime,
            "changing nested file contents should not touch collection root mtime");
        let second_generation = repo.as_mut().unwrap().allocate_generation().unwrap();
        let second = scan(&root, second_generation, &mut repo, &mut governor, |_|{}, |_|{});
        match second {
            ScanEnd::Complete { value, saved: true, .. } => assert_eq!(value.counts.bytes, 140),
            other => panic!("nested change was not reconciled: {other:?}"),
        }
        let historical = repo.as_mut().unwrap().lookup(&root).unwrap().unwrap();
        assert_eq!(historical.measurement.counts.bytes, 140);
        assert_eq!(historical.freshness, super::super::model::Freshness::Unverified);
    }

    #[test]
    fn cancellation_after_complete_root_listing_never_commits_a_total() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("cache.db");
        crate::db::Database::open_path(&db).unwrap();
        let root = temp.path().join("music");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("track.flac"), [0u8;16]).unwrap();
        let mut repo = Some(Repository::open(&db).unwrap());
        let generation = repo.as_mut().unwrap().allocate_generation().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut governor = RateGovernor::new(cancel.clone(), Arc::new(AtomicU8::new(2)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), false);
        let result = scan(&root, generation, &mut repo, &mut governor, |_|{}, |_| {
            // Between the finished direct enumeration and postorder commit.
            cancel.store(true, Ordering::Release);
        });
        assert!(matches!(&result, ScanEnd::Cancelled(_)), "{result:?}");
        assert!(repo.as_mut().unwrap().lookup(&root).unwrap().is_none());
    }

    #[test]
    fn disappearing_descendant_cannot_publish_a_complete_parent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let child = root.join("album");
        fs::create_dir_all(&child).unwrap();
        let target = child.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut governor = RateGovernor::new(cancel, Arc::new(AtomicU8::new(2)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), false);
        let result = scan(&root, 1, &mut None, &mut governor, |_|{}, |_| {
            // The root has been enumerated, but its child no longer exists.
            fs::remove_dir(&target).unwrap();
        });
        assert!(matches!(&result, ScanEnd::Failed { .. } | ScanEnd::Changed { .. }),
            "{result:?}");
    }

    #[test]
    fn cancelled_never_produces_completed_total() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(execute(dir.path(), true), ScanEnd::Cancelled(_)));
    }
    #[test]
    fn wide_tree_with_more_than_2048_folders_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..2055 { fs::create_dir(dir.path().join(format!("d{n}"))).unwrap(); }
        match execute(dir.path(), false) {
            ScanEnd::Complete { value, .. } => assert_eq!(value.counts.folders, 2055),
            other => panic!("unexpected result: {other:?}"),
        }
    }
}

#[cfg(test)]
mod local_preview_tests {
    use super::*;

    #[test]
    fn one_level_counts_do_not_descend_or_publish_link_targets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("music");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("album")).unwrap();
        fs::write(root.join("album").join("nested.flac"), b"nested").unwrap();
        fs::write(root.join("song.FLAC"), b"audio").unwrap();
        fs::write(root.join("cover.jpg"), b"cover").unwrap();
        #[cfg(unix)] {
            std::os::unix::fs::symlink(root.join("album"), root.join("shortcut")).unwrap();
            std::os::unix::fs::symlink(root.join("song.FLAC"), root.join("linked.flac")).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let counts = preview_local_counts(&root, &cancel).unwrap().unwrap();
        assert_eq!(counts.folders, 1);
        assert_eq!(counts.files, 2);
        assert_eq!(counts.audio, 1);
        assert_eq!(counts.bytes, 10);
        #[cfg(unix)] assert_eq!(counts.excluded_links, 2);
        assert_eq!(counts.audio_bytes, 5);
        assert_eq!(counts.other, 0);
    }

    #[test]
    fn cancelled_or_non_directory_preview_has_no_counts() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        assert!(preview_local_counts(temp.path(), &cancel).unwrap().is_none());
        let file = temp.path().join("file.flac");
        fs::write(&file, b"audio").unwrap();
        assert!(preview_local_counts(&file, &AtomicBool::new(false)).is_err());
        #[cfg(unix)] {
            let link = temp.path().join("symlink");
            std::os::unix::fs::symlink(temp.path(), &link).unwrap();
            assert!(preview_local_counts(&link, &AtomicBool::new(false)).is_err());
        }
    }
}

#[cfg(test)]
mod automatic_scan_budget_tests {
    use super::*;

    fn governor_with_limits(limits: AutoScanLimits) -> RateGovernor {
        let mut governor = RateGovernor::new(
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicU8::new(1)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), false);
        // Disable only volume I/O-rate/latency probes for deterministic tests.
        // The *automatic traversal budget* stays active on this governor.
        governor.auto_budget.limits = limits;
        governor
    }
    fn limits() -> AutoScanLimits {
        AutoScanLimits { entry_reads: 128, directories: 16,
            depth: 8, elapsed: Duration::from_secs(8) }
    }
    fn walk(root: &Path, governor: &mut RateGovernor) -> ScanEnd {
        scan(root, 1, &mut None, governor, |_| {}, |_| {})
    }

    #[test]
    fn entry_limit_stops_without_claiming_or_persisting_a_partial_root() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("cache.db");
        crate::db::Database::open_path(&db).unwrap();
        let root = tmp.path().join("music");
        fs::create_dir(&root).unwrap();
        for i in 0..10 { fs::write(root.join(format!("{i}.flac")), b"audio").unwrap(); }
        let mut limits = limits();
        limits.entry_reads = 3;
        let mut governor = governor_with_limits(limits);
        let mut repo = Some(Repository::open(&db).unwrap());
        let generation = repo.as_mut().unwrap().allocate_generation().unwrap();
        let result = scan(&root, generation, &mut repo, &mut governor, |_| {}, |_| {});
        match result {
            ScanEnd::Limited { reason, progress } => {
                assert_eq!(reason, "directory-entry budget reached");
                assert_eq!(progress.files_examined, 3);
            }
            other => panic!("entry budget must not report complete: {other:?}"),
        }
        assert!(repo.as_mut().unwrap().lookup(&root).unwrap().is_none(),
            "no complete root measurement may be cached");
    }

    #[test]
    fn directory_count_and_depth_have_independent_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("collection");
        let nested = root.join("artist").join("album").join("disc");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("track.flac"), b"1234").unwrap();
        let mut count = limits();
        count.directories = 1;
        match walk(&root, &mut governor_with_limits(count)) {
            ScanEnd::Limited { reason, progress } => {
                assert_eq!(reason, "directory-count budget reached");
                assert_eq!(progress.folders_examined, 1);
            }
            other => panic!("unexpected directory-count outcome: {other:?}"),
        }
        let mut depth = limits();
        depth.depth = 1;
        match walk(&root, &mut governor_with_limits(depth)) {
            ScanEnd::Limited { reason, progress } => {
                assert_eq!(reason, "directory-depth budget reached");
                assert_eq!(progress.folders_examined, 2);
            }
            other => panic!("unexpected depth outcome: {other:?}"),
        }
    }

    #[test]
    fn elapsed_limit_is_checked_before_starting_new_filesystem_work() {
        let tmp = tempfile::tempdir().unwrap();
        let mut limits = limits();
        limits.elapsed = Duration::from_secs(1);
        let mut governor = governor_with_limits(limits);
        // Simulate an I/O stall without relying on real network latency.
        governor.auto_budget.started = Instant::now() - Duration::from_secs(2);
        match walk(tmp.path(), &mut governor) {
            ScanEnd::Limited { reason, progress } => {
                assert_eq!(reason, "elapsed-time budget reached");
                assert_eq!(progress.folders_examined, 0);
                assert_eq!(governor.auto_budget.directories, 0);
                assert_eq!(governor.auto_budget.entry_reads, 0);
            }
            other => panic!("unexpected elapsed outcome: {other:?}"),
        }
    }

    #[test]
    fn selection_scans_one_album_not_its_two_hundred_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        fs::create_dir(&root).unwrap();
        for album in 0..200 {
            let directory = root.join(format!("album-{album:03}"));
            fs::create_dir(&directory).unwrap();
            for track in 0..30 {
                fs::write(directory.join(format!("{track:02}.flac")), b"a").unwrap();
            }
        }
        let selected = root.join("album-117");
        let mut governor = governor_with_limits(limits());
        match walk(&selected, &mut governor) {
            ScanEnd::Complete { value, .. } => {
                assert_eq!(value.path, selected);
                assert_eq!(value.counts.files, 30);
                assert_eq!(value.counts.audio, 30);
                assert_eq!(value.counts.folders, 0);
                assert!(governor.auto_budget.entry_reads < 40,
                    "200 sibling albums did not consume the selected budget");
            }
            other => panic!("single album must stay automatic: {other:?}"),
        }
    }

    #[test]
    fn explicit_refresh_bypasses_auto_budgets_without_changing_walk_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("album");
        fs::create_dir(&root).unwrap();
        for n in 0..12 { fs::write(root.join(format!("{n}.flac")), b"t").unwrap(); }
        let mut tiny = limits();
        tiny.entry_reads = 1;
        assert!(matches!(walk(&root, &mut governor_with_limits(tiny)), ScanEnd::Limited { .. }));
        let mut manual = governor_with_limits(tiny);
        manual.priority.store(2, Ordering::Release);
        match walk(&root, &mut manual) {
            ScanEnd::Complete { value, .. } => assert_eq!(value.counts.audio, 12),
            other => panic!("explicit must finish the same root: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cycles_are_never_followed_by_automatic_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("album");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
        fs::write(root.join("one.flac"), b"123").unwrap();
        match walk(&root, &mut governor_with_limits(limits())) {
            ScanEnd::Complete { value, .. } => {
                assert_eq!(value.counts.files, 1);
                assert_eq!(value.counts.excluded_links, 1);
            }
            other => panic!("symlink must not be followed: {other:?}"),
        }
    }

    #[test]
    fn disappearing_or_inaccessible_directory_is_not_reported_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("missing");
        assert!(matches!(walk(&absent, &mut governor_with_limits(limits())),
            ScanEnd::Failed { .. } | ScanEnd::Changed { .. }));
        let not_directory = tmp.path().join("file.txt");
        fs::write(&not_directory, b"one").unwrap();
        assert!(matches!(walk(&not_directory, &mut governor_with_limits(limits())),
            ScanEnd::Failed { .. } | ScanEnd::Changed { .. }));
    }

    #[test]
    fn one_level_preview_exhaustion_does_not_publish_truncated_counts() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 { fs::write(tmp.path().join(format!("{i}.flac")), b"x").unwrap(); }
        let cancel = AtomicBool::new(false);
        assert!(preview_local_counts_bounded(tmp.path(), &cancel, 2,
            Duration::from_secs(10)).unwrap().is_none());
        let full = preview_local_counts_bounded(tmp.path(), &cancel, 20,
            Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(full.files, 5);
        assert_eq!(full.audio, 5);
        assert!(preview_local_counts_bounded(tmp.path(), &cancel, 20,
            Duration::ZERO).unwrap().is_none());
    }
}

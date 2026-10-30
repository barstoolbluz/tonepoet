//! Conservative, shared metadata-I/O budgets per mounted filesystem.
//! Classification is done on a worker once, never from Browse drawing.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeClass { Ssd, Rotational, NetworkOrUnknown }

#[derive(Debug, Clone)]
pub struct Volume { pub key: String, pub class: VolumeClass }

#[derive(Debug)]
struct Mount { point: PathBuf, volume: Volume }

#[derive(Debug)]
pub struct Inventory { mounts: Vec<Mount> }

/// Detect Linux mounts including nested mount boundaries; unknown backends
/// use the most conservative background budget and a read-only latency gate.
impl Inventory {
    pub fn inspect() -> Self {
        let mut mounts = Vec::new();
        #[cfg(target_os="linux")]
        if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
            for line in text.lines() {
                let Some((head, rest)) = line.split_once(" - ") else { continue };
                let fields: Vec<&str> = head.split_whitespace().collect();
                if fields.len() < 5 { continue }
                let fstype = rest.split_whitespace().next().unwrap_or("");
                let point = PathBuf::from(decode_mount_path(fields[4]));
                let class = if matches!(fstype, "ext4"|"xfs"|"btrfs"|"f2fs"|"zfs") {
                    if rotational(fields[2]) { VolumeClass::Rotational } else { VolumeClass::Ssd }
                } else { VolumeClass::NetworkOrUnknown };
                mounts.push(Mount { point, volume: Volume {
                    // Bind mounts on one backing device share one budget.
                    key: format!("{}:{fstype}", fields[2]), class,
                }});
            }
        }
        mounts.sort_by_key(|m| std::cmp::Reverse(m.point.components().count()));
        Self { mounts }
    }
    pub fn for_path(&self, path: &Path) -> Volume {
        self.mounts.iter().find(|m| path.starts_with(&m.point))
            .map(|m| m.volume.clone()).unwrap_or_else(|| Volume {
                // No shared "unclassified" decision: it would admit a slow
                // mount using another volume's fast cached probe result.
                key: fallback_volume_key(path), class: VolumeClass::NetworkOrUnknown,
            })
    }
}

/// Identify a fallback volume at directory boundaries, never for every
/// enumerated entry. On Unix, st_dev distinguishes sshfs/FUSE mounts from
/// local filesystems, including when Linux mountinfo is unavailable.
#[cfg(unix)]
fn fallback_volume_key(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => format!("device:{}", metadata.dev()),
        Err(_) => format!("unclassified-path:{}", path.display()),
    }
}

/// A drive letter or UNC server/share identifies independent Windows roots
/// without an extra metadata query. Reparse points are already excluded from
/// traversal. For paths without a prefix, keep decisions path-scoped rather
/// than sharing a global fallback identity.
#[cfg(windows)]
fn fallback_volume_key(path: &Path) -> String {
    match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => {
            format!("windows-root:{}", prefix.as_os_str().to_string_lossy().to_lowercase())
        }
        _ => format!("unclassified-path:{}", path.display()),
    }
}

#[cfg(not(any(unix, windows)))]
fn fallback_volume_key(path: &Path) -> String {
    format!("unclassified-path:{}", path.display())
}

#[cfg(target_os="linux")]
fn decode_mount_path(path: &str) -> String {
    // mountinfo escapes space, tab, newline and backslash as octal sequences.
    let mut out = Vec::new();
    let mut bytes = path.as_bytes().iter().copied().peekable();
    while let Some(ch) = bytes.next() {
        if ch == b'\\' {
            let octal: Vec<u8> = bytes.clone().take(3).collect();
            if octal.len() == 3 && octal.iter().all(|n| (b'0'..=b'7').contains(n)) {
                for _ in 0..3 { bytes.next(); }
                out.push(((octal[0]-b'0') << 6) | ((octal[1]-b'0') << 3) | (octal[2]-b'0'));
                continue;
            }
        }
        out.push(ch);
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os="linux")]
fn rotational(device: &str) -> bool {
    // Unknown storage is conservatively treated as rotational.
    let base = PathBuf::from(format!("/sys/dev/block/{device}"));
    let mut resolved = match base.canonicalize() { Ok(p) => p, Err(_) => return true };
    loop {
        if let Ok(flag) = std::fs::read_to_string(resolved.join("queue/rotational")) {
            return flag.trim() != "0";
        }
        if !resolved.pop() || resolved == Path::new("/") { break; }
    }
    true
}


/// Admission for unclassified/network mounts. Decisions are shared by workers
/// and expire: an sshfs link that recovers or degrades is reassessed. The
/// probe is read-only, samples *different* names, and runs only on a worker.
/// An attribute cache may still make an entire sample look fast; neither a
/// userspace probe nor a mounted filesystem API can force a genuine RTT.
const PROBE_TTL: Duration = Duration::from_secs(60);
const MAX_PROBE_ENTRIES: usize = 48;
const MAX_PROBE_STATS: usize = 12;
const MAX_AVERAGE_METADATA_LATENCY: Duration = Duration::from_millis(12);
const MAX_SINGLE_METADATA_LATENCY: Duration = Duration::from_millis(120);
const MAX_PROBE_TIME: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy)]
struct ProbeResult { at: Instant, allowed: bool }

#[derive(Debug, Default)]
pub struct NetworkProbeGate { results: Mutex<HashMap<String, ProbeResult>> }

impl NetworkProbeGate {
    /// None means cancelled, Some(false) means defer background work. Explicit
    /// work never calls this gate. No mutex is held across filesystem I/O.
    pub fn permit(&self, volume: &Volume, path: &Path, cancel: &AtomicBool) -> Option<bool> {
        if cancel.load(Ordering::Acquire) { return None; }
        if volume.class != VolumeClass::NetworkOrUnknown
            || std::env::var_os("TONEPOET_ACCOUNTING_ALLOW_UNKNOWN_VOLUMES")
                .as_deref() == Some(std::ffi::OsStr::new("1")) {
            return Some(true);
        }
        if let Some(result) = self.results.lock().unwrap_or_else(|p| p.into_inner())
            .get(&volume.key).copied().filter(|r| r.at.elapsed() < PROBE_TTL) {
            return Some(result.allowed);
        }
        let allowed = probe_directory_metadata(path, cancel)?;
        let mut results = self.results.lock().unwrap_or_else(|p| p.into_inner());
        // Bound memory even on hosts with many transient mounts.
        if results.len() >= 128 && !results.contains_key(&volume.key) {
            if let Some(oldest) = results.iter().min_by_key(|(_, r)| r.at)
                .map(|(key, _)| key.clone()) {
                results.remove(&oldest);
            }
        }
        results.insert(volume.key.clone(), ProbeResult { at: Instant::now(), allowed });
        Some(allowed)
    }
}

fn metadata_sample_fast(total: Duration, slowest: Duration, operations: usize) -> bool {
    operations >= 2
        && total <= MAX_PROBE_TIME
        && slowest <= MAX_SINGLE_METADATA_LATENCY
        && total / (operations as u32) <= MAX_AVERAGE_METADATA_LATENCY
}

/// Include uncached-by-this-probe directory entries, rather than repeatedly
/// stat-ing the root. Collect no file contents and follow no symlinks.
fn probe_directory_metadata(path: &Path, cancel: &AtomicBool) -> Option<bool> {
    let mut total = Duration::ZERO;
    let mut slowest = Duration::ZERO;
    let mut operations = 0;
    macro_rules! record {
        ($elapsed:expr, $metadata:expr) => {{
            let elapsed = $elapsed;
            total += elapsed;
            slowest = slowest.max(elapsed);
            if $metadata { operations += 1; }
        }};
    }
    let start = Instant::now();
    let root = std::fs::symlink_metadata(path);
    record!(start.elapsed(), true);
    if cancel.load(Ordering::Acquire) { return None; }
    if !root.is_ok_and(|m| m.is_dir() && !super::scanner::is_excluded(&m)) {
        return Some(false);
    }
    let start = Instant::now();
    let entries = std::fs::read_dir(path);
    record!(start.elapsed(), true);
    let Ok(mut entries) = entries else { return Some(false); };
    let mut sampled = 0;
    for index in 0..MAX_PROBE_ENTRIES {
        if cancel.load(Ordering::Acquire) { return None; }
        if total > MAX_PROBE_TIME { return Some(false); }
        let start = Instant::now();
        let entry = entries.next();
        record!(start.elapsed(), false);
        let Some(entry) = entry else { break; };
        let Ok(entry) = entry else { return Some(false); };
        // Spread the metadata requests over distinct paths, including some
        // entries not used in the root's existing cursor/preview sample.
        if index % 4 != 0 { continue; }
        let start = Instant::now();
        let metadata = std::fs::symlink_metadata(entry.path());
        record!(start.elapsed(), true);
        if cancel.load(Ordering::Acquire) { return None; }
        if metadata.is_err() { return Some(false); }
        sampled += 1;
        if sampled >= MAX_PROBE_STATS { break; }
    }
    Some(metadata_sample_fast(total, slowest, operations))
}

#[derive(Debug)]
struct Bucket { tokens: f64, last: Instant }
#[derive(Debug, Default)]
pub struct VolumeBudgets { buckets: Mutex<HashMap<String,Bucket>> }
impl VolumeBudgets {
    /// Charge an operation from the per-volume bucket. Return the necessary
    /// sleep duration without holding the mutex while a worker sleeps.
    pub fn charge(&self, volume: &Volume, foreground: bool, cost: f64) -> std::time::Duration {
        let (rate, burst) = match (volume.class, foreground) {
            (VolumeClass::Ssd, false) => (250.0, 250.0),
            (VolumeClass::Ssd, true) => (1000.0, 500.0),
            (VolumeClass::Rotational, false) => (75.0, 75.0),
            (VolumeClass::Rotational, true) => (250.0, 125.0),
            (VolumeClass::NetworkOrUnknown, false) => (50.0, 50.0),
            (VolumeClass::NetworkOrUnknown, true) => (100.0, 100.0),
        };
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        // Both worker classes consume the same per-volume token pool. A
        // foreground promotion changes the refill rate; it does not create an
        // independent pool capable of doubling aggregate I/O on the volume.
        let bucket = buckets.entry(volume.key.clone())
            .or_insert(Bucket { tokens: burst, last: now });
        bucket.tokens = (bucket.tokens + now.duration_since(bucket.last).as_secs_f64() * rate).min(burst);
        bucket.last = now;
        if bucket.tokens >= cost {
            bucket.tokens -= cost;
            std::time::Duration::ZERO
        } else {
            // Do not debit a refused request: callers retry after sleeping.
            std::time::Duration::from_secs_f64(((cost - bucket.tokens) / rate).min(0.100))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_governor_never_touches_shared_token_bucket() {
        use super::super::scanner::{RateGovernor, ScanExit};
        use std::sync::Arc;
        use std::sync::atomic::AtomicU8;
        let temp = tempfile::tempdir().unwrap();
        let inventory = Inventory::inspect();
        let volume = inventory.for_path(temp.path());
        let budgets = Arc::new(VolumeBudgets::default());
        // Charge once to create the bucket. Thereafter every foreground tick
        // must bypass it completely, including the refill timestamp.
        budgets.charge(&volume, false, 2.0);
        let before = {
            let held = budgets.buckets.lock().unwrap();
            let bucket = held.get(&volume.key).unwrap();
            (bucket.tokens, bucket.last)
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let mut governor = RateGovernor::with_budgets(cancel.clone(), Arc::new(AtomicU8::new(2)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            budgets.clone(), Arc::new(NetworkProbeGate::default()), inventory);
        governor.enter_directory(temp.path()).unwrap();
        for _ in 0..1000 { governor.tick(2.0).expect("foreground must not sleep for tokens"); }
        let after = {
            let held = budgets.buckets.lock().unwrap();
            let bucket = held.get(&volume.key).unwrap();
            (bucket.tokens, bucket.last)
        };
        assert_eq!(before, after, "foreground must not charge or refill background budgets");
        cancel.store(true, Ordering::Release);
        assert!(matches!(governor.tick(1.0), Err(ScanExit::Cancelled)));
    }

    #[test]
    fn per_volume_buckets_are_independent_and_debt_is_not_reused() {
        let budgets = VolumeBudgets::default();
        let a = Volume { key: "A".into(), class: VolumeClass::NetworkOrUnknown };
        let b = Volume { key: "B".into(), class: VolumeClass::NetworkOrUnknown };
        assert_eq!(budgets.charge(&a, false, 50.0), std::time::Duration::ZERO);
        assert!(budgets.charge(&a, false, 1.0) > std::time::Duration::ZERO);
        assert_eq!(budgets.charge(&b, false, 50.0), std::time::Duration::ZERO);
    }
    #[test]
    fn latency_admission_observes_average_tail_and_total_caps() {
        assert!(metadata_sample_fast(Duration::from_millis(36), Duration::from_millis(9), 4));
        assert!(!metadata_sample_fast(Duration::from_millis(60), Duration::from_millis(60), 2));
        assert!(!metadata_sample_fast(Duration::from_millis(240), Duration::from_millis(150), 24));
        assert!(!metadata_sample_fast(Duration::from_millis(320), Duration::from_millis(10), 32));
        assert!(!metadata_sample_fast(Duration::ZERO, Duration::ZERO, 1));
    }
    #[cfg(unix)]
    #[test]
    fn fallback_uses_actual_device_and_never_a_global_unclassified_key() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        // Force the fallback path even on Linux, where a normal mountinfo
        // inventory must remain authoritative.
        let inventory = Inventory { mounts: Vec::new() };
        let parent_volume = inventory.for_path(temp.path());
        let child_volume = inventory.for_path(&child);
        assert_eq!(parent_volume.key, child_volume.key);
        assert_eq!(parent_volume.key, format!("device:{}",
            std::fs::symlink_metadata(temp.path()).unwrap().dev()));
        let missing_a = inventory.for_path(&temp.path().join("missing-a"));
        let missing_b = inventory.for_path(&temp.path().join("missing-b"));
        assert_ne!(missing_a.key, missing_b.key);
        assert_ne!(missing_a.key, parent_volume.key);
    }

    #[cfg(windows)]
    #[test]
    fn fallback_separates_windows_drives_and_unc_shares_without_io() {
        let inventory = Inventory { mounts: Vec::new() };
        let drive_a = inventory.for_path(Path::new(r"C:\Music\Album"));
        let drive_same = inventory.for_path(Path::new(r"c:\Music\Other"));
        let drive_b = inventory.for_path(Path::new(r"Z:\Music\Album"));
        assert_eq!(drive_a.key, drive_same.key);
        assert_ne!(drive_a.key, drive_b.key);
        let share_a = inventory.for_path(Path::new(r"\\server\share-a\Album"));
        let share_b = inventory.for_path(Path::new(r"\\server\share-b\Album"));
        assert_ne!(share_a.key, share_b.key);
        assert_ne!(share_a.key, drive_b.key);
    }

    #[test]
    fn cached_opposite_volume_decisions_do_not_cross_admit() {
        let gate = NetworkProbeGate::default();
        let a = Volume { key: "independent-a".into(), class: VolumeClass::NetworkOrUnknown };
        let b = Volume { key: "independent-b".into(), class: VolumeClass::NetworkOrUnknown };
        let cancel = AtomicBool::new(false);
        let mut decisions = gate.results.lock().unwrap();
        decisions.insert(a.key.clone(), ProbeResult { at: Instant::now(), allowed: true });
        decisions.insert(b.key.clone(), ProbeResult { at: Instant::now(), allowed: false });
        drop(decisions);
        assert_eq!(gate.permit(&a, Path::new("/not-probed"), &cancel), Some(true));
        assert_eq!(gate.permit(&b, Path::new("/not-probed"), &cancel), Some(false));
        gate.results.lock().unwrap().get_mut(&a.key).unwrap().allowed = false;
        assert_eq!(gate.permit(&b, Path::new("/not-probed"), &cancel), Some(false));
    }

    #[test]
    fn cached_volume_decisions_expire_and_are_separated_by_mount_key() {
        let gate = NetworkProbeGate::default();
        let fast = Volume { key: "fast-sshfs".into(), class: VolumeClass::NetworkOrUnknown };
        let slow = Volume { key: "slow-sshfs".into(), class: VolumeClass::NetworkOrUnknown };
        {
            let mut decisions = gate.results.lock().unwrap();
            decisions.insert(fast.key.clone(), ProbeResult { at: Instant::now(), allowed: true });
            decisions.insert(slow.key.clone(), ProbeResult { at: Instant::now(), allowed: false });
        }
        let cancel = AtomicBool::new(false);
        assert_eq!(gate.permit(&fast, Path::new("/nonexistent"), &cancel), Some(true));
        assert_eq!(gate.permit(&slow, Path::new("/nonexistent"), &cancel), Some(false));
        cancel.store(true, Ordering::Release);
        assert_eq!(gate.permit(&fast, Path::new("/nonexistent"), &cancel), None);
        let mut decisions = gate.results.lock().unwrap();
        decisions.get_mut(&fast.key).unwrap().at = Instant::now() - PROBE_TTL - Duration::from_secs(1);
        drop(decisions);
        cancel.store(false, Ordering::Release);
        assert_eq!(gate.permit(&fast, Path::new("/nonexistent"), &cancel), Some(false));
    }
    #[cfg(target_os="linux")]
    #[test]
    fn escaped_mount_paths_are_decoded() {
        assert_eq!(decode_mount_path("/media/a\\040b\\134c"), "/media/a b\\c");
    }
}

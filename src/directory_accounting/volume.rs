//! Conservative, shared metadata-I/O budgets per mounted filesystem.
//! Classification is done on a worker once, never from Browse drawing.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeClass { Ssd, Rotational, NetworkOrUnknown }

#[derive(Debug, Clone)]
pub struct Volume { pub key: String, pub class: VolumeClass }

#[derive(Debug)]
struct Mount { point: PathBuf, volume: Volume }

#[derive(Debug)]
pub struct Inventory { mounts: Vec<Mount> }

/// Detect Linux mounts including nested mount boundaries; unknown backends
/// deliberately receive the safest throttle and no background opt-in.
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
    pub fn permits_background_root(&self, path: &Path) -> bool {
        if std::env::var_os("TONEPOET_ACCOUNTING_ALLOW_UNKNOWN_VOLUMES")
            .as_deref() == Some(std::ffi::OsStr::new("1")) { return true; }
        self.for_path(path).class != VolumeClass::NetworkOrUnknown
            && !self.mounts.iter().any(|mount| mount.point.starts_with(path)
                && mount.volume.class == VolumeClass::NetworkOrUnknown)
    }
    pub fn for_path(&self, path: &Path) -> Volume {
        self.mounts.iter().find(|m| path.starts_with(&m.point))
            .map(|m| m.volume.clone()).unwrap_or_else(|| Volume {
                key: "unclassified".into(), class: VolumeClass::NetworkOrUnknown,
            })
    }
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
    fn per_volume_buckets_are_independent_and_debt_is_not_reused() {
        let budgets = VolumeBudgets::default();
        let a = Volume { key: "A".into(), class: VolumeClass::NetworkOrUnknown };
        let b = Volume { key: "B".into(), class: VolumeClass::NetworkOrUnknown };
        assert_eq!(budgets.charge(&a, false, 50.0), std::time::Duration::ZERO);
        assert!(budgets.charge(&a, false, 1.0) > std::time::Duration::ZERO);
        assert_eq!(budgets.charge(&b, false, 50.0), std::time::Duration::ZERO);
    }
    #[cfg(target_os="linux")]
    #[test]
    fn escaped_mount_paths_are_decoded() {
        assert_eq!(decode_mount_path("/media/a\\040b\\134c"), "/media/a b\\c");
    }
}

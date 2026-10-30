//! Accounting policy and value types. No partial result implements `Complete`.
use std::path::PathBuf;
use std::time::SystemTime;

pub const POLICY_VERSION: i64 = 1;
pub const AUDIO_POLICY_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub files: u128,
    /// Descendants only; the selected directory itself is never counted.
    pub folders: u128,
    pub audio: u128,
    /// Logical bytes per regular-file directory entry, including hard links.
    pub bytes: u128,
    pub audio_bytes: u128,
    pub excluded_links: u128,
    pub other: u128,
}

impl Counts {
    pub fn merge(&mut self, other: &Counts) -> Result<(), &'static str> {
        macro_rules! checked { ($field:ident) => { self.$field = self.$field.checked_add(other.$field).ok_or("directory accounting overflow")?; } }
        checked!(files); checked!(folders); checked!(audio); checked!(bytes);
        checked!(audio_bytes); checked!(excluded_links); checked!(other);
        Ok(())
    }
    pub fn add_file(&mut self, length: u64, audio: bool) -> Result<(), &'static str> {
        self.files = self.files.checked_add(1).ok_or("file count overflow")?;
        self.bytes = self.bytes.checked_add(length as u128).ok_or("size overflow")?;
        if audio {
            self.audio = self.audio.checked_add(1).ok_or("audio count overflow")?;
            self.audio_bytes = self.audio_bytes.checked_add(length as u128).ok_or("audio size overflow")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub files_examined: u128,
    pub audio_examined: u128,
    pub folders_examined: u128,
    pub folders_discovered: u128,
    pub bytes_examined: u128,
    pub links_skipped: u128,
    pub current: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CompleteMeasurement {
    pub path: PathBuf,
    pub counts: Counts,
    pub started: SystemTime,
    pub finished: SystemTime,
    /// Root directory's own creation/modification timestamps, not descendants'.
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub generation: i64,
    /// Complete-as-observed is not an atomic filesystem snapshot.
    pub observation: Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation { AsObserved, Indeterminate }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness { AsObserved, Unverified, KnownInvalid }

#[derive(Debug, Clone)]
pub struct CachedMeasurement {
    pub measurement: CompleteMeasurement,
    pub freshness: Freshness,
}

#[derive(Debug, Clone)]
pub enum ScanEnd {
    Complete { value: CompleteMeasurement, saved: bool, persistence_error: Option<String> },
    Cancelled(ScanProgress),
    /// A network/unknown mount failed its read-only metadata-latency gate.
    Deferred(ScanProgress),
    Failed { reason: String, progress: ScanProgress },
    Changed { reason: String, progress: ScanProgress },
}

#[derive(Debug, Clone)]
pub enum AccountingEvent {
    Cached { id: u64, path: PathBuf, value: Option<CachedMeasurement>, error: Option<String> },
    Local { id: u64, path: PathBuf, counts: Counts },
    /// Ephemeral one-level Browse read, never a persisted subtree measurement.
    LocalPreview { id: u64, path: PathBuf, result: Result<Option<Counts>, String> },
    Progress { id: u64, path: PathBuf, progress: ScanProgress },
    Finished { id: u64, path: PathBuf, end: ScanEnd },
    Ready { id: u64, path: PathBuf },
}

pub fn is_audio_name(path: &std::path::Path) -> bool {
    // Use the same extension authority as Browse classification.
    crate::convert::classify::is_audio_file_path(path)
}

/// Produce IEC units without converting an entire `u128` to `f64`.
/// Rounds half up at the requested decimal precision.
pub fn format_bytes(bytes: u128) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 { return format!("{bytes} B"); }
    let mut scale = 1u128;
    let mut unit = 0;
    while unit + 1 < UNITS.len() && bytes / scale >= 1024 {
        scale *= 1024;
        unit += 1;
    }
    let precision = if unit >= 3 { 100 } else { 10 };
    let integer = bytes / scale;
    let remainder = bytes % scale;
    let fractional = (remainder * precision + scale / 2) / scale;
    // `remainder * 100` is safe: scale <= 2^60 for all supported units.
    let rounded = integer * precision + fractional;
    if precision == 100 {
        format!("{}.{:02} {}", rounded / 100, rounded % 100, UNITS[unit])
    } else {
        format!("{}.{} {}", rounded / 10, rounded % 10, UNITS[unit])
    }
}

pub fn format_exact(bytes: u128) -> String {
    let s = bytes.to_string();
    let mut grouped = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 { grouped.push(','); }
        grouped.push(c);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn units_and_overflow() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_exact(83_880_004_232), "83,880,004,232");
        assert_eq!(format_bytes(u128::MAX).ends_with("EiB"), true);
        let mut huge = Counts { bytes: u128::MAX, ..Counts::default() };
        assert!(huge.merge(&Counts { bytes: 1, ..Counts::default() }).is_err());
    }
    #[test]
    fn folders_count_only_once() {
        let mut root = Counts { folders: 1, ..Counts::default() };
        root.merge(&Counts { files: 3, folders: 2, ..Counts::default() }).unwrap();
        assert_eq!(root.folders, 3);
    }
}

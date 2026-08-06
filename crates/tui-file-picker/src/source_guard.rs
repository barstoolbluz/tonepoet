//! Stable source identity and mutation detection for copy-then-delete moves.
//!
//! A pathname is not an object identity.  These helpers capture the underlying
//! filesystem object plus a conservative version token, and provide a small
//! SHA-256 implementation so callers can prove that the bytes copied are still
//! the bytes present immediately before source cleanup.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::state::VerificationMode;

const MAX_MANIFEST_ENTRIES: usize = 100_000;
const MAX_MANIFEST_DEPTH: usize = 1_024;

/// Deterministic per-operation filesystem I/O accounting.
///
/// These counters are intentionally byte- and call-based rather than timing-
/// based so tests can detect accidental proof amplification without depending
/// on machine or mount performance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileOperationIoCounters {
    pub bytes_copied: u64,
    pub source_bytes_hashed: u64,
    pub destination_bytes_hashed: u64,
    pub bytes_redundantly_rehashed: u64,
    pub source_tree_walks: u64,
    /// Complete recursive destination membership/content-enumeration passes.
    pub destination_tree_walks: u64,
    /// Complete manifest-wide destination stability passes performed while the
    /// source tree is traversed for cleanup. Strict mounts use exact metadata
    /// tokens without a second destination enumeration; portable mounts may
    /// rehash files and re-enumerate directory membership.
    pub destination_entry_verification_passes: u64,
    pub rename_attempts: u64,
    pub rename_fallbacks: u64,
    pub file_sync_calls: u64,
    pub directory_sync_calls: u64,
}

impl FileOperationIoCounters {
    pub fn merge(&mut self, other: Self) {
        self.bytes_copied = self.bytes_copied.saturating_add(other.bytes_copied);
        self.source_bytes_hashed = self
            .source_bytes_hashed
            .saturating_add(other.source_bytes_hashed);
        self.destination_bytes_hashed = self
            .destination_bytes_hashed
            .saturating_add(other.destination_bytes_hashed);
        self.bytes_redundantly_rehashed = self
            .bytes_redundantly_rehashed
            .saturating_add(other.bytes_redundantly_rehashed);
        self.source_tree_walks = self.source_tree_walks.saturating_add(other.source_tree_walks);
        self.destination_tree_walks = self
            .destination_tree_walks
            .saturating_add(other.destination_tree_walks);
        self.destination_entry_verification_passes = self
            .destination_entry_verification_passes
            .saturating_add(other.destination_entry_verification_passes);
        self.rename_attempts = self.rename_attempts.saturating_add(other.rename_attempts);
        self.rename_fallbacks = self.rename_fallbacks.saturating_add(other.rename_fallbacks);
        self.file_sync_calls = self.file_sync_calls.saturating_add(other.file_sync_calls);
        self.directory_sync_calls = self
            .directory_sync_calls
            .saturating_add(other.directory_sync_calls);
    }
}

/// Whether a mount capability has been established. `Unknown` is deliberately
/// not treated as support: destructive cleanup falls back to content-authority
/// checks whenever the stronger local-filesystem semantics cannot be proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemSemantics {
    StableLocal,
    NetworkOrReduced,
    Unknown,
}

/// Per-mount capabilities used by file-operation safety policy. Identity and
/// timestamp guarantees are independent from metadata and publication
/// capabilities; a mount can therefore degrade only the proof that it lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemCapabilities {
    pub semantics: FilesystemSemantics,
    pub stable_path_identity: CapabilitySupport,
    pub nanosecond_timestamps: CapabilitySupport,
    pub extended_attributes: CapabilitySupport,
    pub directory_sync: CapabilitySupport,
    pub atomic_no_replace_rename: CapabilitySupport,
    pub filesystem_type: Option<u64>,
}

impl FilesystemCapabilities {
    const fn stable_local(filesystem_type: Option<u64>) -> Self {
        Self {
            semantics: FilesystemSemantics::StableLocal,
            stable_path_identity: CapabilitySupport::Supported,
            nanosecond_timestamps: CapabilitySupport::Supported,
            extended_attributes: CapabilitySupport::Unknown,
            directory_sync: CapabilitySupport::Unknown,
            atomic_no_replace_rename: CapabilitySupport::Unknown,
            filesystem_type,
        }
    }

    const fn reduced(filesystem_type: Option<u64>) -> Self {
        Self {
            semantics: FilesystemSemantics::NetworkOrReduced,
            stable_path_identity: CapabilitySupport::Unsupported,
            nanosecond_timestamps: CapabilitySupport::Unsupported,
            extended_attributes: CapabilitySupport::Unknown,
            directory_sync: CapabilitySupport::Unknown,
            atomic_no_replace_rename: CapabilitySupport::Unknown,
            filesystem_type,
        }
    }

    const fn conservative(filesystem_type: Option<u64>) -> Self {
        Self {
            semantics: FilesystemSemantics::Unknown,
            stable_path_identity: CapabilitySupport::Unknown,
            nanosecond_timestamps: CapabilitySupport::Unknown,
            extended_attributes: CapabilitySupport::Unknown,
            directory_sync: CapabilitySupport::Unknown,
            atomic_no_replace_rename: CapabilitySupport::Unknown,
            filesystem_type,
        }
    }

    const fn assumed_strict() -> Self {
        Self {
            semantics: FilesystemSemantics::StableLocal,
            stable_path_identity: CapabilitySupport::Supported,
            nanosecond_timestamps: CapabilitySupport::Supported,
            extended_attributes: CapabilitySupport::Supported,
            directory_sync: CapabilitySupport::Supported,
            atomic_no_replace_rename: CapabilitySupport::Supported,
            filesystem_type: None,
        }
    }

    const fn assumed_portable() -> Self {
        Self::conservative(None)
    }

    pub const fn identity_policy(self) -> FilesystemIdentityPolicy {
        if matches!(self.semantics, FilesystemSemantics::StableLocal)
            && matches!(self.stable_path_identity, CapabilitySupport::Supported)
            && matches!(self.nanosecond_timestamps, CapabilitySupport::Supported)
        {
            FilesystemIdentityPolicy::Strict
        } else {
            FilesystemIdentityPolicy::ContentVerifiedPortable
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemCapabilityKind {
    StablePathIdentity,
    NanosecondTimestamps,
    ExtendedAttributes,
    DirectorySync,
    AtomicNoReplaceRename,
}

/// Strength of pathname/object identity evidence available on the containing
/// filesystem. This remains as a compact compatibility view; the underlying
/// decision is derived from the full independent capability record above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemIdentityPolicy {
    Strict,
    ContentVerifiedPortable,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LinuxMountKey {
    device: u64,
}

#[cfg(target_os = "linux")]
static FILESYSTEM_CAPABILITY_CACHE: OnceLock<Mutex<HashMap<LinuxMountKey, FilesystemCapabilities>>> =
    OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_FILESYSTEM_CAPABILITY_OVERRIDE: std::cell::Cell<Option<FilesystemCapabilities>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestFilesystemCapabilityOverrideGuard {
    previous: Option<FilesystemCapabilities>,
}

#[cfg(test)]
impl Drop for TestFilesystemCapabilityOverrideGuard {
    fn drop(&mut self) {
        TEST_FILESYSTEM_CAPABILITY_OVERRIDE.with(|slot| slot.set(self.previous));
    }
}

/// Install a thread-local capability classification for deterministic tests.
/// The scoped guard restores the prior value, so parallel tests cannot leak a
/// synthetic mount policy into one another.
#[cfg(test)]
pub(crate) fn test_override_filesystem_capabilities(
    capabilities: FilesystemCapabilities,
) -> TestFilesystemCapabilityOverrideGuard {
    let previous = TEST_FILESYSTEM_CAPABILITY_OVERRIDE.with(|slot| {
        let previous = slot.get();
        slot.set(Some(capabilities));
        previous
    });
    TestFilesystemCapabilityOverrideGuard { previous }
}

#[cfg(target_os = "linux")]
fn capability_cache() -> &'static Mutex<HashMap<LinuxMountKey, FilesystemCapabilities>> {
    FILESYSTEM_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(target_os = "linux")]
fn nearest_existing_probe_path(path: &Path) -> io::Result<PathBuf> {
    let mut probe = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&probe) {
            Ok(metadata) => {
                if metadata.file_type().is_file() || metadata.file_type().is_dir() {
                    return Ok(probe);
                }
                if !probe.pop() {
                    return Ok(PathBuf::from("."));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !probe.pop() || probe.as_os_str().is_empty() {
                    return Ok(PathBuf::from("."));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_mount_descriptor(path: &Path) -> io::Result<(LinuxMountKey, PathBuf)> {
    use std::os::unix::fs::MetadataExt;

    let probe = nearest_existing_probe_path(path)?;
    let device = fs::symlink_metadata(&probe)?.dev();
    Ok((LinuxMountKey { device }, probe))
}

#[cfg(target_os = "linux")]
fn linux_filesystem_type(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem probe path contains NUL: {}", path.display()),
        )
    })?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() }.f_type as u64)
}

#[cfg(target_os = "linux")]
fn classify_linux_filesystem_type(filesystem_type: u64) -> FilesystemCapabilities {
    const EXT_SUPER_MAGIC: u64 = 0x0000_ef53;
    const XFS_SUPER_MAGIC: u64 = 0x5846_5342;
    const BTRFS_SUPER_MAGIC: u64 = 0x9123_683e;
    const TMPFS_MAGIC: u64 = 0x0102_1994;
    const RAMFS_MAGIC: u64 = 0x8584_58f6;
    const ZFS_SUPER_MAGIC: u64 = 0x2fc1_2fc1;
    const F2FS_SUPER_MAGIC: u64 = 0xf2f5_2010;
    const JFS_SUPER_MAGIC: u64 = 0x3153_464a;

    const CIFS_SUPER_MAGIC: u64 = 0xff53_4d42;
    const SMB2_SUPER_MAGIC: u64 = 0xfe53_4d42;
    const FUSE_SUPER_MAGIC: u64 = 0x6573_5546;
    const NTFS3_SUPER_MAGIC: u64 = 0x5346_544e;
    const NFS_SUPER_MAGIC: u64 = 0x0000_6969;
    const V9FS_MAGIC: u64 = 0x0102_1997;
    const CEPH_SUPER_MAGIC: u64 = 0x00c3_6400;
    const CODA_SUPER_MAGIC: u64 = 0x7375_7245;
    const AFS_SUPER_MAGIC: u64 = 0x5346_414f;
    const MSDOS_SUPER_MAGIC: u64 = 0x0000_4d44;
    const EXFAT_SUPER_MAGIC: u64 = 0x2011_bab0;
    const HFS_SUPER_MAGIC: u64 = 0x0000_4244;
    const HFSPLUS_SUPER_MAGIC: u64 = 0x482b;

    if matches!(
        filesystem_type,
        EXT_SUPER_MAGIC
            | XFS_SUPER_MAGIC
            | BTRFS_SUPER_MAGIC
            | TMPFS_MAGIC
            | RAMFS_MAGIC
            | ZFS_SUPER_MAGIC
            | F2FS_SUPER_MAGIC
            | JFS_SUPER_MAGIC
    ) {
        FilesystemCapabilities::stable_local(Some(filesystem_type))
    } else if matches!(
        filesystem_type,
        CIFS_SUPER_MAGIC
            | SMB2_SUPER_MAGIC
            | FUSE_SUPER_MAGIC
            | NTFS3_SUPER_MAGIC
            | NFS_SUPER_MAGIC
            | V9FS_MAGIC
            | CEPH_SUPER_MAGIC
            | CODA_SUPER_MAGIC
            | AFS_SUPER_MAGIC
            | MSDOS_SUPER_MAGIC
            | EXFAT_SUPER_MAGIC
            | HFS_SUPER_MAGIC
            | HFSPLUS_SUPER_MAGIC
    ) {
        FilesystemCapabilities::reduced(Some(filesystem_type))
    } else {
        // An unrecognized mount is never silently promoted to strict local
        // semantics. It remains portable until its guarantees are known.
        FilesystemCapabilities::conservative(Some(filesystem_type))
    }
}

#[cfg(target_os = "linux")]
fn probe_path_handle_identity(path: &Path) -> CapabilitySupport {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return CapabilitySupport::Unknown,
    };
    let opened = match snapshot_open_handle(&file) {
        Ok(snapshot) => snapshot,
        Err(_) => return CapabilitySupport::Unknown,
    };
    let pathname = match snapshot_path(path) {
        Ok(snapshot) => snapshot,
        Err(_) => return CapabilitySupport::Unknown,
    };
    if opened.kind == pathname.kind && opened.identity == pathname.identity {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unsupported
    }
}

#[cfg(target_os = "linux")]
fn merge_observed_identity(
    baseline: CapabilitySupport,
    semantics: FilesystemSemantics,
    observed: CapabilitySupport,
) -> CapabilitySupport {
    match semantics {
        FilesystemSemantics::NetworkOrReduced => CapabilitySupport::Unsupported,
        FilesystemSemantics::StableLocal => match observed {
            CapabilitySupport::Supported | CapabilitySupport::Unsupported => observed,
            CapabilitySupport::Unknown => baseline,
        },
        FilesystemSemantics::Unknown => observed,
    }
}

#[cfg(target_os = "linux")]
fn probe_extended_attributes(path: &Path) -> CapabilitySupport {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(path.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(_) => return CapabilitySupport::Unknown,
    };
    let result = unsafe { libc::listxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if result >= 0 {
        return CapabilitySupport::Supported;
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EOPNOTSUPP)
        || error.raw_os_error() == Some(libc::ENOTSUP)
    {
        CapabilitySupport::Unsupported
    } else {
        CapabilitySupport::Unknown
    }
}

#[cfg(target_os = "linux")]
fn probe_linux_capabilities(path: &Path) -> FilesystemCapabilities {
    let Ok((key, probe)) = linux_mount_descriptor(path) else {
        return FilesystemCapabilities::conservative(None);
    };
    if let Ok(cache) = capability_cache().lock() {
        if let Some(capabilities) = cache.get(&key) {
            return *capabilities;
        }
    }

    let mut capabilities = linux_filesystem_type(&probe)
        .map(classify_linux_filesystem_type)
        .unwrap_or_else(|_| FilesystemCapabilities::conservative(None));
    let observed_identity = probe_path_handle_identity(&probe);
    capabilities.stable_path_identity = merge_observed_identity(
        capabilities.stable_path_identity,
        capabilities.semantics,
        observed_identity,
    );
    capabilities.extended_attributes = probe_extended_attributes(&probe);

    if let Ok(mut cache) = capability_cache().lock() {
        // Preserve any runtime observation installed while this thread was
        // probing; a stale baseline must never overwrite a known capability.
        return *cache.entry(key).or_insert(capabilities);
    }
    capabilities
}

pub fn filesystem_capabilities(path: &Path) -> FilesystemCapabilities {
    #[cfg(test)]
    if let Some(capabilities) = TEST_FILESYSTEM_CAPABILITY_OVERRIDE.with(|slot| slot.get()) {
        let _ = path;
        return capabilities;
    }
    #[cfg(target_os = "linux")]
    {
        return probe_linux_capabilities(path);
    }
    #[cfg(windows)]
    {
        let _ = path;
        return FilesystemCapabilities {
            semantics: FilesystemSemantics::StableLocal,
            stable_path_identity: CapabilitySupport::Supported,
            nanosecond_timestamps: CapabilitySupport::Supported,
            extended_attributes: CapabilitySupport::Unsupported,
            directory_sync: CapabilitySupport::Unknown,
            atomic_no_replace_rename: CapabilitySupport::Supported,
            filesystem_type: None,
        };
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = path;
        FilesystemCapabilities::conservative(None)
    }
}

fn update_capability_record(
    capabilities: &mut FilesystemCapabilities,
    capability: FilesystemCapabilityKind,
    support: CapabilitySupport,
) {
    match capability {
        FilesystemCapabilityKind::StablePathIdentity => {
            capabilities.stable_path_identity = support
        }
        FilesystemCapabilityKind::NanosecondTimestamps => {
            capabilities.nanosecond_timestamps = support
        }
        FilesystemCapabilityKind::ExtendedAttributes => {
            capabilities.extended_attributes = support
        }
        FilesystemCapabilityKind::DirectorySync => capabilities.directory_sync = support,
        FilesystemCapabilityKind::AtomicNoReplaceRename => {
            capabilities.atomic_no_replace_rename = support
        }
    }
}

/// Records a capability learned from an actual operation on the mount. This
/// lets runtime rename and directory-sync attempts refine the cached per-device
/// record instead of repeating unsupported probes or relying only on type
/// heuristics.
pub fn record_filesystem_capability(
    path: &Path,
    capability: FilesystemCapabilityKind,
    support: CapabilitySupport,
) {
    #[cfg(target_os = "linux")]
    {
        // Populate the complete baseline first. Otherwise an early runtime
        // observation (for example rename support) could create a partial
        // cache entry and accidentally suppress the identity/xattr probes.
        let baseline = probe_linux_capabilities(path);
        let Ok((key, _probe)) = linux_mount_descriptor(path) else {
            return;
        };
        let mut cache = match capability_cache().lock() {
            Ok(cache) => cache,
            Err(_) => return,
        };
        let entry = cache.entry(key).or_insert(baseline);
        update_capability_record(entry, capability, support);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, capability, support);
    }
}

pub fn filesystem_identity_policy(path: &Path) -> FilesystemIdentityPolicy {
    filesystem_capabilities(path).identity_policy()
}

pub fn filesystem_identity_policy_notice(path: &Path) -> Option<String> {
    let capabilities = filesystem_capabilities(path);
    (capabilities.identity_policy() == FilesystemIdentityPolicy::ContentVerifiedPortable).then(|| {
        format!(
            "filesystem guarantees are reduced or unproven ({:?}; identity={:?}, timestamp-ns={:?}, xattrs={:?}, directory-sync={:?}, atomic-no-replace={:?}); native renames use retained-handle/type/size/path-transition evidence, while unavoidable copy/delete cleanup uses identity/tree evidence in standard mode and content hashes in strong mode",
            capabilities.semantics,
            capabilities.stable_path_identity,
            capabilities.nanosecond_timestamps,
            capabilities.extended_attributes,
            capabilities.directory_sync,
            capabilities.atomic_no_replace_rename,
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SourceVersion {
    #[cfg(unix)]
    Unix {
        len: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_sec: i64,
        mtime_nsec: i64,
        ctime_sec: i64,
        ctime_nsec: i64,
    },
    #[cfg(windows)]
    Windows {
        len: u64,
        creation_time: u64,
        last_write_time: u64,
        attributes: u32,
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        len: u64,
        modified: Option<std::time::SystemTime>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceSnapshot {
    kind: SourceKind,
    identity: SourceIdentity,
    version: SourceVersion,
    symlink_target: Option<PathBuf>,
}

impl SourceSnapshot {
    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub fn len(&self) -> u64 {
        match &self.version {
            #[cfg(unix)]
            SourceVersion::Unix { len, .. } => *len,
            #[cfg(windows)]
            SourceVersion::Windows { len, .. } => *len,
            #[cfg(not(any(unix, windows)))]
            SourceVersion::Portable { len, .. } => *len,
        }
    }

    pub fn supports_identity_proof(&self) -> bool {
        #[cfg(any(unix, windows))]
        {
            true
        }
        #[cfg(not(any(unix, windows)))]
        {
            !matches!(&self.identity, SourceIdentity::Unsupported)
        }
    }

    pub fn verify_same_identity(&self, current: &Self) -> Result<(), String> {
        if self.kind != current.kind {
            return Err(format!("source kind changed from {:?} to {:?}", self.kind, current.kind));
        }
        if self.identity != current.identity {
            return Err("source object identity changed".to_string());
        }
        Ok(())
    }

    pub fn verify_same_identity_with_policy(
        &self,
        current: &Self,
        policy: FilesystemIdentityPolicy,
    ) -> Result<(), String> {
        if self.kind != current.kind {
            return Err(format!("source kind changed from {:?} to {:?}", self.kind, current.kind));
        }
        if policy == FilesystemIdentityPolicy::Strict {
            if self.identity != current.identity {
                return Err("source object identity changed".to_string());
            }
            return Ok(());
        }

        let same_length = match (&self.version, &current.version) {
            #[cfg(unix)]
            (SourceVersion::Unix { len: left, .. }, SourceVersion::Unix { len: right, .. }) => {
                left == right
            }
            #[cfg(windows)]
            (
                SourceVersion::Windows { len: left, .. },
                SourceVersion::Windows { len: right, .. },
            ) => left == right,
            #[cfg(not(any(unix, windows)))]
            (
                SourceVersion::Portable { len: left, .. },
                SourceVersion::Portable { len: right, .. },
            ) => left == right,
            #[allow(unreachable_patterns)]
            _ => false,
        };
        if self.kind != SourceKind::Directory && !same_length {
            return Err("source length changed".to_string());
        }
        if self.kind == SourceKind::Symlink && self.symlink_target != current.symlink_target {
            return Err("symlink target changed".to_string());
        }
        Ok(())
    }

    pub fn verify_same_object_after_rename(&self, current: &Self) -> Result<(), String> {
        self.verify_same_object_after_rename_with_capabilities(
            current,
            FilesystemCapabilities::assumed_strict(),
        )
    }

    pub fn verify_same_object_after_rename_with_policy(
        &self,
        current: &Self,
        policy: FilesystemIdentityPolicy,
    ) -> Result<(), String> {
        let capabilities = match policy {
            FilesystemIdentityPolicy::Strict => FilesystemCapabilities::assumed_strict(),
            FilesystemIdentityPolicy::ContentVerifiedPortable => {
                FilesystemCapabilities::assumed_portable()
            }
        };
        self.verify_same_object_after_rename_with_capabilities(current, capabilities)
    }

    pub fn verify_same_object_after_rename_with_capabilities(
        &self,
        current: &Self,
        capabilities: FilesystemCapabilities,
    ) -> Result<(), String> {
        self.verify_same_identity_with_policy(current, capabilities.identity_policy())?;
        let stable = match (&self.version, &current.version) {
            #[cfg(unix)]
            (
                SourceVersion::Unix {
                    len: left_len,
                    mode: left_mode,
                    uid: left_uid,
                    gid: left_gid,
                    mtime_sec: left_mtime_sec,
                    mtime_nsec: left_mtime_nsec,
                    ..
                },
                SourceVersion::Unix {
                    len: right_len,
                    mode: right_mode,
                    uid: right_uid,
                    gid: right_gid,
                    mtime_sec: right_mtime_sec,
                    mtime_nsec: right_mtime_nsec,
                    ..
                },
            ) => {
                if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                    left_len == right_len
                        && left_mode == right_mode
                        && left_uid == right_uid
                        && left_gid == right_gid
                        && left_mtime_sec == right_mtime_sec
                        && left_mtime_nsec == right_mtime_nsec
                } else {
                    let length_stable = self.kind == SourceKind::Directory || left_len == right_len;
                    let timestamp_stable = match capabilities.nanosecond_timestamps {
                        CapabilitySupport::Supported => {
                            left_mtime_sec == right_mtime_sec
                                && left_mtime_nsec == right_mtime_nsec
                        }
                        CapabilitySupport::Unsupported | CapabilitySupport::Unknown => {
                            (*left_mtime_sec).abs_diff(*right_mtime_sec) <= 2
                        }
                    };
                    length_stable && timestamp_stable
                }
            }
            #[cfg(windows)]
            (SourceVersion::Windows { .. }, SourceVersion::Windows { .. }) => {
                if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                    self.version == current.version
                } else {
                    self.len() == current.len()
                }
            }
            #[cfg(not(any(unix, windows)))]
            (SourceVersion::Portable { .. }, SourceVersion::Portable { .. }) => {
                if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                    self.version == current.version
                } else {
                    self.len() == current.len()
                }
            }
        };
        if stable {
            Ok(())
        } else {
            Err("source metadata/content change token changed".to_string())
        }
    }

    pub fn verify_same_object_and_version_with_capabilities(
        &self,
        current: &Self,
        capabilities: FilesystemCapabilities,
    ) -> Result<(), String> {
        if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
            self.verify_same_object_and_version(current)
        } else {
            self.verify_same_object_after_rename_with_capabilities(current, capabilities)
        }
    }

    pub fn verify_same_object_and_version(&self, current: &Self) -> Result<(), String> {
        if self.kind != current.kind {
            return Err(format!(
                "source kind changed from {:?} to {:?}",
                self.kind, current.kind
            ));
        }
        if self.identity != current.identity {
            return Err("source object identity changed".to_string());
        }
        if self.version != current.version {
            return Err("source size or filesystem change token changed".to_string());
        }
        if self.symlink_target != current.symlink_target {
            return Err("source symlink target changed".to_string());
        }
        Ok(())
    }
}

pub fn snapshot_path(path: &Path) -> io::Result<SourceSnapshot> {
    let metadata = fs::symlink_metadata(path)?;
    let kind = kind_from_metadata(&metadata)?;
    let symlink_target = if kind == SourceKind::Symlink {
        Some(fs::read_link(path)?)
    } else {
        None
    };
    let identity = path_identity(path, kind, &metadata)?;
    Ok(SourceSnapshot {
        kind,
        identity,
        version: version_from_metadata(&metadata),
        symlink_target,
    })
}

/// Snapshot an already-open regular-file or directory handle without
/// re-resolving its pathname. This is the general primitive used by mount
/// capability probes; callers that require regular-file semantics should use
/// [`snapshot_open_file`].
pub fn snapshot_open_handle(file: &File) -> io::Result<SourceSnapshot> {
    let metadata = file.metadata()?;
    let kind = kind_from_metadata(&metadata)?;
    if kind == SourceKind::Symlink {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened handle unexpectedly resolved as a symlink",
        ));
    }
    Ok(SourceSnapshot {
        kind,
        identity: file_identity(file, &metadata)?,
        version: version_from_metadata(&metadata),
        symlink_target: None,
    })
}

pub fn snapshot_open_file(file: &File) -> io::Result<SourceSnapshot> {
    let snapshot = snapshot_open_handle(file)?;
    if snapshot.kind != SourceKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened handle is not a regular file",
        ));
    }
    Ok(snapshot)
}


/// Evidence retained across a native rename. Opening directories is supported
/// on Unix and may be unavailable on other platforms; the handle proof is
/// therefore optional, while the captured pathname snapshot is mandatory.
pub struct RenameSourceProof {
    snapshot: SourceSnapshot,
    open_handle: Option<File>,
    open_snapshot: Option<SourceSnapshot>,
}

impl RenameSourceProof {
    /// Bind a retained root handle/snapshot to the root entry of a manifest
    /// captured immediately before a rename. This closes the gap between the
    /// recursive manifest pass and the namespace mutation: a root replacement
    /// cannot be moved under authority established for a different object.
    pub fn verify_manifest_root(
        &self,
        manifest: &SourceManifest,
        capabilities: FilesystemCapabilities,
    ) -> Result<(), String> {
        let expected = manifest
            .expected_snapshot(Path::new(""))
            .ok_or_else(|| "source manifest has no root entry".to_string())?;
        expected
            .verify_same_object_and_version_with_capabilities(&self.snapshot, capabilities)
            .map_err(|error| format!("rename source changed after manifest capture: {error}"))
    }

    pub fn capture(path: &Path) -> io::Result<Self> {
        let snapshot = snapshot_path(path)?;
        let (open_handle, open_snapshot) = if snapshot.kind() == SourceKind::Symlink {
            (None, None)
        } else {
            match File::open(path) {
                Ok(handle) => match snapshot_open_handle(&handle) {
                    Ok(open_snapshot) => (Some(handle), Some(open_snapshot)),
                    Err(_) => (None, None),
                },
                Err(_) => (None, None),
            }
        };
        Ok(Self {
            snapshot,
            open_handle,
            open_snapshot,
        })
    }

    pub fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameVerification {
    /// Snapshot of the exact destination object that matched the retained
    /// pre-rename authority. Callers can derive a complete destination
    /// manifest without reopening or recursively rehashing the published tree.
    pub destination_snapshot: SourceSnapshot,
    /// At least one side lacks strict pathname identity or nanosecond timestamp
    /// guarantees, or a strict metadata/version check was inconclusive after
    /// the portable retained-handle/path-transition proof succeeded.
    pub portable_evidence: bool,
    /// A committed rename whose portable proof is complete but whose optional
    /// strict metadata/version proof was inconclusive. Strict object-identity
    /// contradictions remain hard failures. Callers must surface this as
    /// completed-with-warning, never as a failed/retryable operation.
    pub warning: Option<String>,
}

/// Verify a committed native rename without reading file contents.
///
/// Source-owned evidence is always interpreted with source filesystem
/// capabilities; destination-owned evidence is interpreted with destination
/// capabilities. The cross-path transition can be strict only when both sides
/// advertise strict identity semantics. Portable evidence (source pathname
/// absent, destination present, kind/size/target consistent, and retained
/// handle stable when available) is authoritative on reduced or unknown
/// filesystems. A strict metadata/version mismatch after that proof succeeds is
/// returned as a warning; a strict identity contradiction remains a hard
/// failure because it can indicate destination replacement.
pub fn verify_committed_rename(
    source: &Path,
    destination: &Path,
    proof: &RenameSourceProof,
    source_capabilities: FilesystemCapabilities,
    destination_capabilities: FilesystemCapabilities,
) -> Result<RenameVerification, String> {
    match fs::symlink_metadata(source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "source pathname still exists after rename: {}",
                source.display()
            ))
        }
        Err(error) => {
            return Err(format!(
                "could not prove source pathname disappearance after rename: {error}"
            ))
        }
    }
    verify_renamed_destination(
        destination,
        proof,
        source_capabilities,
        destination_capabilities,
    )
}

/// Verify that `destination` is the object represented by retained pre-rename
/// authority, without requiring the original pathname to be absent.
///
/// Transactional rename cycles legitimately repopulate every source pathname
/// with another member of the same permutation. The transaction planner proves
/// the namespace topology; this function proves each final object-to-destination
/// binding and returns the exact root snapshot for operation-time undo proof.
pub fn verify_renamed_destination(
    destination: &Path,
    proof: &RenameSourceProof,
    source_capabilities: FilesystemCapabilities,
    destination_capabilities: FilesystemCapabilities,
) -> Result<RenameVerification, String> {
    let destination_snapshot = snapshot_path(destination)
        .map_err(|error| format!("could not identify renamed destination: {error}"))?;
    let source_policy = source_capabilities.identity_policy();
    let destination_policy = destination_capabilities.identity_policy();
    let source_strict = source_capabilities.semantics == FilesystemSemantics::StableLocal
        && source_policy == FilesystemIdentityPolicy::Strict;
    let destination_strict =
        destination_capabilities.semantics == FilesystemSemantics::StableLocal
            && destination_policy == FilesystemIdentityPolicy::Strict;
    let strict_transition = source_strict && destination_strict;
    let mut strict_warnings = Vec::new();

    // Establish the portable path-transition proof first. This is the
    // authoritative floor on reduced/unknown mounts and the safe fallback when
    // strict metadata changes despite a committed rename.
    proof
        .snapshot
        .verify_same_identity_with_policy(
            &destination_snapshot,
            FilesystemIdentityPolicy::ContentVerifiedPortable,
        )
        .map_err(|error| format!("renamed destination does not match the source: {error}"))?;

    if strict_transition {
        proof
            .snapshot
            .verify_same_identity(&destination_snapshot)
            .map_err(|error| {
                format!(
                    "strict renamed-destination identity contradicts the portable transition proof: {error}"
                )
            })?;
        if let Err(error) = proof
            .snapshot
            .verify_same_object_after_rename_with_capabilities(
                &destination_snapshot,
                destination_capabilities,
            )
        {
            // Identity is already proven above. A remaining mismatch is a
            // metadata/version-token discrepancy and may be retained as a
            // completed-with-warning result.
            strict_warnings.push(format!(
                "strict renamed-destination metadata could not be proven after the committed rename: {error}"
            ));
        }
    }

    if let (Some(handle), Some(open_before)) = (&proof.open_handle, &proof.open_snapshot) {
        let open_after = snapshot_open_handle(handle)
            .map_err(|error| format!("could not re-identify retained rename handle: {error}"))?;

        // A retained source handle belongs to the source mount. Never apply a
        // destination mount's identity policy to this comparison.
        open_before
            .verify_same_identity_with_policy(
                &open_after,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )
            .map_err(|error| {
                format!("retained source handle changed while native rename committed: {error}")
            })?;
        if source_strict {
            open_before.verify_same_identity(&open_after).map_err(|error| {
                format!(
                    "strict retained-source identity changed while native rename committed: {error}"
                )
            })?;
            if let Err(error) = open_before.verify_same_object_after_rename_with_capabilities(
                &open_after,
                source_capabilities,
            ) {
                // The handle still names the same object; only its strict
                // metadata/version token is uncertain.
                strict_warnings.push(format!(
                    "strict retained-source metadata could not be proven after the committed rename: {error}"
                ));
            }
        }

        open_after
            .verify_same_identity_with_policy(
                &destination_snapshot,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )
            .map_err(|error| {
                format!(
                    "renamed destination no longer corresponds to the retained source handle: {error}"
                )
            })?;
        if strict_transition {
            open_after
                .verify_same_identity(&destination_snapshot)
                .map_err(|error| {
                    format!(
                        "strict destination identity does not match the retained source handle: {error}"
                    )
                })?;
        }
    }

    let warning = (!strict_warnings.is_empty()).then(|| {
        format!(
            "{} [source semantics={:?}, policy={:?}, fs-type={:?}; destination semantics={:?}, policy={:?}, fs-type={:?}]",
            strict_warnings.join("; "),
            source_capabilities.semantics,
            source_policy,
            source_capabilities.filesystem_type,
            destination_capabilities.semantics,
            destination_policy,
            destination_capabilities.filesystem_type,
        )
    });
    Ok(RenameVerification {
        destination_snapshot,
        portable_evidence: !strict_transition || warning.is_some(),
        warning,
    })
}

pub fn verify_path_with_capabilities(
    path: &Path,
    expected: &SourceSnapshot,
    capabilities: FilesystemCapabilities,
) -> Result<(), String> {
    let current = snapshot_path(path)
        .map_err(|error| format!("could not re-read source identity: {error}"))?;
    expected.verify_same_object_and_version_with_capabilities(&current, capabilities)
}

pub fn verify_path(path: &Path, expected: &SourceSnapshot) -> Result<(), String> {
    let current = snapshot_path(path)
        .map_err(|error| format!("could not re-read source identity: {error}"))?;
    expected.verify_same_object_and_version(&current)
}

/// Preserve regular-file metadata from one open handle to another.
///
/// Source metadata is read from the already-open source object, never by
/// re-resolving its pathname. Destination metadata is applied to the already-
/// open staged/reserved object. Failures are returned as explicit fidelity
/// warnings because content publication remains independently verifiable.
pub fn preserve_open_file_metadata(source: &File, destination: &File) -> Vec<String> {
    let mut warnings = Vec::new();
    let metadata = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            warnings.push(format!("read source metadata from open handle: {error}"));
            return warnings;
        }
    };

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let source_fd = source.as_raw_fd();
        let destination_fd = destination.as_raw_fd();
        if unsafe { libc::fchown(destination_fd, metadata.uid(), metadata.gid()) } != 0 {
            warnings.push(format!("ownership: {}", io::Error::last_os_error()));
        }
        let times = [
            libc::timespec {
                tv_sec: metadata.atime(),
                tv_nsec: metadata.atime_nsec() as _,
            },
            libc::timespec {
                tv_sec: metadata.mtime(),
                tv_nsec: metadata.mtime_nsec() as _,
            },
        ];
        if unsafe { libc::futimens(destination_fd, times.as_ptr()) } != 0 {
            warnings.push(format!("timestamps: {}", io::Error::last_os_error()));
        }

        #[cfg(target_os = "linux")]
        warnings.extend(copy_linux_xattrs_between_fds(source_fd, destination_fd));

        // Ownership and ACL writes can clear set-ID bits, so permissions are
        // restored last.
        if unsafe { libc::fchmod(destination_fd, metadata.mode() & 0o7777) } != 0 {
            warnings.push(format!("permissions: {}", io::Error::last_os_error()));
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = destination.set_permissions(metadata.permissions()) {
        warnings.push(format!("permissions: {error}"));
    }

    warnings
}

#[cfg(target_os = "linux")]
fn copy_linux_xattrs_between_fds(source_fd: i32, destination_fd: i32) -> Vec<String> {
    use std::ffi::CString;

    let mut warnings = Vec::new();
    let size = unsafe { libc::flistxattr(source_fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EOPNOTSUPP) {
            warnings.push(format!("list extended attributes: {error}"));
        }
        return warnings;
    }
    if size == 0 {
        return warnings;
    }

    let mut names = vec![0u8; size as usize];
    let read = unsafe { libc::flistxattr(source_fd, names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        warnings.push(format!(
            "read extended attribute names: {}",
            io::Error::last_os_error()
        ));
        return warnings;
    }

    for raw_name in names[..read as usize]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = match CString::new(raw_name) {
            Ok(name) => name,
            Err(_) => {
                warnings.push("extended attribute name contains NUL".to_string());
                continue;
            }
        };
        let value_size = unsafe {
            libc::fgetxattr(source_fd, name.as_ptr(), std::ptr::null_mut(), 0)
        };
        if value_size < 0 {
            warnings.push(format!(
                "read extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
            continue;
        }
        let mut value = vec![0u8; value_size as usize];
        if value_size > 0 {
            let got = unsafe {
                libc::fgetxattr(
                    source_fd,
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            if got < 0 {
                warnings.push(format!(
                    "read extended attribute {:?}: {}",
                    String::from_utf8_lossy(raw_name),
                    io::Error::last_os_error()
                ));
                continue;
            }
            value.truncate(got as usize);
        }
        if unsafe {
            libc::fsetxattr(
                destination_fd,
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } != 0
        {
            warnings.push(format!(
                "write extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
        }
    }
    warnings
}

fn kind_from_metadata(metadata: &fs::Metadata) -> io::Result<SourceKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Ok(SourceKind::File)
    } else if file_type.is_dir() {
        Ok(SourceKind::Directory)
    } else if file_type.is_symlink() {
        Ok(SourceKind::Symlink)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "special filesystem objects are not supported",
        ))
    }
}

#[cfg(unix)]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    use std::os::unix::fs::MetadataExt;
    SourceVersion::Unix {
        len: metadata.len(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    use std::os::windows::fs::MetadataExt;
    SourceVersion::Windows {
        len: metadata.len(),
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
        attributes: metadata.file_attributes(),
    }
}

#[cfg(not(any(unix, windows)))]
fn version_from_metadata(metadata: &fs::Metadata) -> SourceVersion {
    SourceVersion::Portable {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

#[cfg(unix)]
fn path_identity(
    _path: &Path,
    _kind: SourceKind,
    metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(SourceIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(SourceIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn path_identity(
    path: &Path,
    kind: SourceKind,
    _metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    let mut flags = 0;
    if kind == SourceKind::Directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    if kind == SourceKind::Symlink {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
    }
    let file = options.custom_flags(flags).open(path)?;
    windows_file_identity(&file)
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    windows_file_identity(file)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<SourceIdentity> {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    let ok = unsafe {
        GetFileInformationByHandle(
            file.as_raw_handle().cast(),
            information.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(SourceIdentity::Windows {
        volume_serial: information.volume_serial_number,
        file_index: ((information.file_index_high as u64) << 32)
            | information.file_index_low as u64,
    })
}

#[cfg(not(any(unix, windows)))]
fn path_identity(
    _path: &Path,
    _kind: SourceKind,
    _metadata: &fs::Metadata,
) -> io::Result<SourceIdentity> {
    Ok(SourceIdentity::Unsupported)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, _metadata: &fs::Metadata) -> io::Result<SourceIdentity> {
    Ok(SourceIdentity::Unsupported)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ContentDigest(pub [u8; 32]);

impl ContentDigest {
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667,
                0xbb67ae85,
                0x3c6ef372,
                0xa54ff53a,
                0x510e527f,
                0x9b05688c,
                0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.block_len != 0 {
            let needed = 64 - self.block_len;
            let take = needed.min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == 64 {
                let block = self.block;
                self.compress(&block);
                self.block_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.block[..data.len()].copy_from_slice(data);
            self.block_len = data.len();
        }
    }

    pub fn finalize(mut self) -> ContentDigest {
        let bit_len = self.total_len.wrapping_mul(8);
        self.block[self.block_len] = 0x80;
        self.block_len += 1;
        if self.block_len > 56 {
            for byte in &mut self.block[self.block_len..] {
                *byte = 0;
            }
            let block = self.block;
            self.compress(&block);
            self.block = [0; 64];
            self.block_len = 0;
        }
        for byte in &mut self.block[self.block_len..56] {
            *byte = 0;
        }
        self.block[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.block;
        self.compress(&block);

        let mut output = [0u8; 32];
        for (chunk, word) in output.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        ContentDigest(output)
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
            0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
            0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
            0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
            0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
            0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
            0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
            0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for (index, chunk) in block.chunks_exact(4).take(16).enumerate() {
            w[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        let mut digest = Sha256::new();
        digest.update(b"abc");
        assert_eq!(
            digest.finalize().to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty_and_split_updates_match_known_vectors() {
        assert_eq!(
            Sha256::new().finalize().to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let mut split = Sha256::new();
        split.update(b"a");
        split.update(b"b");
        split.update(b"c");
        assert_eq!(
            split.finalize().to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn standard_manifest_is_digest_free_and_strong_manifest_retains_content_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album.flac");
        fs::write(&source, b"audio payload").expect("fixture");

        let standard = capture_manifest_with_mode(&source, VerificationMode::Standard)
            .expect("standard manifest");
        let strong = capture_manifest_with_mode(&source, VerificationMode::Strong)
            .expect("strong manifest");

        assert_eq!(standard.verification(), VerificationMode::Standard);
        assert!(!standard.has_content_digests());
        assert_eq!(strong.verification(), VerificationMode::Strong);
        assert!(strong.has_content_digests());
    }

    #[test]
    fn mixed_manifest_authority_is_rejected_before_digest_comparison() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album.flac");
        fs::write(&source, b"audio payload").expect("fixture");

        let strong = capture_manifest_with_mode(&source, VerificationMode::Strong)
            .expect("strong manifest");
        let standard = capture_manifest_with_mode(&source, VerificationMode::Standard)
            .expect("standard manifest");
        let destination = strong.destination_identity_for_same_tree();

        let error = destination
            .verify_captured_replay_source(
                &strong,
                &standard,
                filesystem_capabilities(&source),
            )
            .expect_err("mixed authority must fail closed");
        assert!(error.contains("verification authority mismatch"), "{error}");
        assert!(!error.contains("content changed"), "authority must gate first: {error}");
    }

    #[test]
    fn pathname_replacement_is_not_the_captured_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write original");
        let captured = snapshot_path(&source).expect("capture source");

        fs::rename(&source, &displaced).expect("displace original");
        fs::write(&source, b"replaced").expect("write same-size replacement");

        let error = verify_path(&source, &captured).expect_err("replacement must be rejected");
        assert!(error.contains("identity changed"), "unexpected error: {error}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn filesystem_classifier_is_strict_only_for_known_local_filesystems() {
        const EXT_SUPER_MAGIC: u64 = 0x0000_ef53;
        const NFS_SUPER_MAGIC: u64 = 0x0000_6969;
        const V9FS_MAGIC: u64 = 0x0102_1997;
        const UNKNOWN_MAGIC: u64 = 0xdead_beef;

        let ext = classify_linux_filesystem_type(EXT_SUPER_MAGIC);
        assert_eq!(ext.semantics, FilesystemSemantics::StableLocal);
        assert_eq!(ext.identity_policy(), FilesystemIdentityPolicy::Strict);

        for filesystem_type in [NFS_SUPER_MAGIC, V9FS_MAGIC] {
            let capabilities = classify_linux_filesystem_type(filesystem_type);
            assert_eq!(
                capabilities.semantics,
                FilesystemSemantics::NetworkOrReduced
            );
            assert_eq!(
                capabilities.stable_path_identity,
                CapabilitySupport::Unsupported
            );
            assert_eq!(
                capabilities.nanosecond_timestamps,
                CapabilitySupport::Unsupported
            );
            assert_eq!(
                capabilities.identity_policy(),
                FilesystemIdentityPolicy::ContentVerifiedPortable
            );
        }

        let unknown = classify_linux_filesystem_type(UNKNOWN_MAGIC);
        assert_eq!(unknown.semantics, FilesystemSemantics::Unknown);
        assert_eq!(
            unknown.identity_policy(),
            FilesystemIdentityPolicy::ContentVerifiedPortable,
            "an unrecognized mount must never be silently promoted to strict semantics"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directory_first_capability_probe_preserves_local_identity_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("track.flac");
        fs::write(&file, b"audio").expect("file");
        let missing_destination = temp.path().join("new").join("album");

        let directory_observation = probe_path_handle_identity(temp.path());
        assert_eq!(
            directory_observation,
            CapabilitySupport::Supported,
            "directory handles must participate in pathname/open-handle identity probes"
        );
        let mut simulated_local = FilesystemCapabilities::stable_local(None);
        simulated_local.stable_path_identity = merge_observed_identity(
            simulated_local.stable_path_identity,
            simulated_local.semantics,
            directory_observation,
        );
        assert_eq!(
            simulated_local.identity_policy(),
            FilesystemIdentityPolicy::Strict,
            "a successful directory observation must preserve a known-local strict baseline"
        );
        assert_eq!(
            nearest_existing_probe_path(&missing_destination).expect("nearest probe"),
            temp.path(),
            "a nonexistent destination must be classified from its nearest existing directory"
        );

        let (key, probe) = linux_mount_descriptor(temp.path()).expect("mount descriptor");
        capability_cache().lock().expect("cache").remove(&key);
        let baseline = classify_linux_filesystem_type(
            linux_filesystem_type(&probe).expect("filesystem type"),
        );
        let directory_first = filesystem_capabilities(&missing_destination);
        let regular_file_second = filesystem_capabilities(&file);

        assert_eq!(
            directory_first.semantics,
            regular_file_second.semantics,
            "directory and regular-file probes on one device must retain the same semantics"
        );
        assert_eq!(
            directory_first.stable_path_identity,
            regular_file_second.stable_path_identity,
            "a later regular-file lookup must not repair or alter a bad directory-first cache entry"
        );
        assert_eq!(
            directory_first.identity_policy(),
            regular_file_second.identity_policy(),
            "the cached move policy must be stable after a directory-first lookup"
        );
        if baseline.semantics == FilesystemSemantics::StableLocal {
            assert_eq!(
                directory_first.identity_policy(),
                FilesystemIdentityPolicy::Strict,
                "a directory-first probe must not route a known local device through recursive portable moves"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inapplicable_identity_observation_does_not_erase_known_local_support() {
        assert_eq!(
            merge_observed_identity(
                CapabilitySupport::Supported,
                FilesystemSemantics::StableLocal,
                CapabilitySupport::Unknown,
            ),
            CapabilitySupport::Supported
        );
        assert_eq!(
            merge_observed_identity(
                CapabilitySupport::Unknown,
                FilesystemSemantics::Unknown,
                CapabilitySupport::Unknown,
            ),
            CapabilitySupport::Unknown
        );
    }

    #[test]
    fn capability_updates_are_independent() {
        let mut capabilities = FilesystemCapabilities::conservative(None);
        update_capability_record(
            &mut capabilities,
            FilesystemCapabilityKind::DirectorySync,
            CapabilitySupport::Supported,
        );
        update_capability_record(
            &mut capabilities,
            FilesystemCapabilityKind::AtomicNoReplaceRename,
            CapabilitySupport::Unsupported,
        );

        assert_eq!(
            capabilities.stable_path_identity,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            capabilities.nanosecond_timestamps,
            CapabilitySupport::Unknown
        );
        assert_eq!(
            capabilities.extended_attributes,
            CapabilitySupport::Unknown
        );
        assert_eq!(capabilities.directory_sync, CapabilitySupport::Supported);
        assert_eq!(
            capabilities.atomic_no_replace_rename,
            CapabilitySupport::Unsupported
        );
        assert_eq!(
            capabilities.identity_policy(),
            FilesystemIdentityPolicy::ContentVerifiedPortable
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn atomic_no_replace_publication_never_replaces_a_destination_that_appeared() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("staged-payload");
        let destination = temp.path().join("published-album");
        fs::create_dir(&source).expect("staged directory");
        fs::write(source.join("new.txt"), b"new payload").expect("staged child");
        fs::create_dir(&destination).expect("racing destination");
        fs::write(destination.join("existing.txt"), b"existing payload")
            .expect("existing child");

        let error = rename_path_no_replace(&source, &destination)
            .expect_err("no-replace publication must reject an occupied destination");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(destination.join("existing.txt")).expect("destination survived"),
            b"existing payload",
        );
        assert_eq!(
            fs::read(source.join("new.txt")).expect("staging survived"),
            b"new payload",
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(irrefutable_let_patterns)] // other cfg targets add enum variants
    fn portable_rename_policy_tolerates_pseudo_inode_and_timestamp_precision_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        fs::write(&source, b"original").expect("write source");
        let captured = snapshot_path(&source).expect("capture source");
        let mut reopened = captured.clone();

        if let SourceIdentity::Unix { device, inode } = &mut reopened.identity {
            *device = device.wrapping_add(1);
            *inode = inode.wrapping_add(1);
        }
        if let SourceVersion::Unix { mtime_nsec, .. } = &mut reopened.version {
            *mtime_nsec = 0;
        }

        assert!(captured.verify_same_object_after_rename(&reopened).is_err());
        captured
            .verify_same_object_after_rename_with_policy(
                &reopened,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )
            .expect("portable policy must defer to manifest/content proof");

        if let SourceVersion::Unix { len, .. } = &mut reopened.version {
            *len = len.saturating_add(1);
        }
        assert!(captured
            .verify_same_object_after_rename_with_policy(
                &reopened,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn committed_rename_reproof_uses_source_mount_policy_for_retained_handle() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("source");
        let proof = RenameSourceProof::capture(&source).expect("capture proof");

        fs::rename(&source, &destination).expect("rename");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
            .expect("change post-rename metadata");

        let verification = verify_committed_rename(
            &source,
            &destination,
            &proof,
            FilesystemCapabilities::assumed_portable(),
            FilesystemCapabilities::assumed_strict(),
        )
        .expect("source-owned handle must use source portable semantics");

        assert!(verification.portable_evidence);
        assert!(verification.warning.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn strict_only_reproof_doubt_is_completed_with_warning_after_portable_proof() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("source");
        let proof = RenameSourceProof::capture(&source).expect("capture proof");

        fs::rename(&source, &destination).expect("rename");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
            .expect("change post-rename metadata");

        let verification = verify_committed_rename(
            &source,
            &destination,
            &proof,
            FilesystemCapabilities::assumed_strict(),
            FilesystemCapabilities::assumed_strict(),
        )
        .expect("portable proof must preserve committed disposition");

        assert!(verification.portable_evidence);
        let warning = verification.warning.as_deref().expect("strict warning");
        assert!(warning.contains("strict"));
        assert!(warning.contains("source semantics="));
        assert!(warning.contains("destination semantics="));
    }

    #[cfg(unix)]
    #[test]
    #[allow(irrefutable_let_patterns)] // other cfg targets add enum variants
    fn reduced_mount_reproof_accepts_changed_pseudo_identity_with_portable_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("source");
        let mut proof = RenameSourceProof::capture(&source).expect("capture proof");

        // Model a reduced/network filesystem that reports a different pseudo
        // inode and coarser timestamp for the same retained object after the
        // rename. Keep the real destination and retained handle intact: this
        // tests capability attribution, not acceptance of a replacement file.
        if let SourceIdentity::Unix { device, inode } = &mut proof.snapshot.identity {
            *device = device.wrapping_add(1);
            *inode = inode.wrapping_add(1);
        }
        if let SourceVersion::Unix { mtime_nsec, .. } = &mut proof.snapshot.version {
            *mtime_nsec = 0;
        }
        if let Some(open_before) = proof.open_snapshot.as_mut() {
            if let SourceIdentity::Unix { device, inode } = &mut open_before.identity {
                *device = device.wrapping_add(1);
                *inode = inode.wrapping_add(1);
            }
            if let SourceVersion::Unix { mtime_nsec, .. } = &mut open_before.version {
                *mtime_nsec = 0;
            }
        }

        fs::rename(&source, &destination).expect("rename");

        let verification = verify_committed_rename(
            &source,
            &destination,
            &proof,
            FilesystemCapabilities::assumed_portable(),
            FilesystemCapabilities::assumed_portable(),
        )
        .expect("reduced semantics use retained-handle/type/size/path-transition evidence");

        assert!(verification.portable_evidence);
        assert!(verification.warning.is_none());
    }

    #[cfg(unix)]
    #[test]
    #[allow(irrefutable_let_patterns)] // other cfg targets add enum variants
    fn unknown_semantics_never_promote_committed_rename_to_strict_proof() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("source");
        let mut proof = RenameSourceProof::capture(&source).expect("capture proof");

        if let SourceIdentity::Unix { device, inode } = &mut proof.snapshot.identity {
            *device = device.wrapping_add(1);
            *inode = inode.wrapping_add(1);
        }
        if let Some(open_before) = proof.open_snapshot.as_mut() {
            if let SourceIdentity::Unix { device, inode } = &mut open_before.identity {
                *device = device.wrapping_add(1);
                *inode = inode.wrapping_add(1);
            }
        }
        fs::rename(&source, &destination).expect("rename");

        let unknown_observed_supported = FilesystemCapabilities {
            semantics: FilesystemSemantics::Unknown,
            stable_path_identity: CapabilitySupport::Supported,
            nanosecond_timestamps: CapabilitySupport::Supported,
            extended_attributes: CapabilitySupport::Unknown,
            directory_sync: CapabilitySupport::Unknown,
            atomic_no_replace_rename: CapabilitySupport::Unknown,
            filesystem_type: Some(0xfeed_beef),
        };
        assert_eq!(
            unknown_observed_supported.identity_policy(),
            FilesystemIdentityPolicy::ContentVerifiedPortable,
            "Unknown semantics must never be promoted to strict policy by favorable runtime observations",
        );

        let verification = verify_committed_rename(
            &source,
            &destination,
            &proof,
            unknown_observed_supported,
            unknown_observed_supported,
        )
        .expect("Unknown semantics must retain portable proof regardless of observed identity");

        assert!(verification.portable_evidence);
        assert!(verification.warning.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires TONEPOET_REDUCED_FS_TEST_DIR on the user's CIFS/SMB or NTFS mount"]
    fn live_reduced_mount_reports_the_actual_capability_route_and_committed_disposition() {
        let mount = std::env::var_os("TONEPOET_REDUCED_FS_TEST_DIR")
            .map(std::path::PathBuf::from)
            .expect("set TONEPOET_REDUCED_FS_TEST_DIR to a writable directory on the affected mount");
        let root = tempfile::Builder::new()
            .prefix(".tonepoet-capability-probe-")
            .tempdir_in(&mount)
            .expect("create live capability probe directory");
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        fs::write(&source, b"tonepoet live reduced-filesystem proof")
            .expect("write live source");

        let (source_mount_key, source_probe_path) =
            linux_mount_descriptor(&source).expect("resolve live source mount descriptor");
        let source_filesystem_type = linux_filesystem_type(&source_probe_path)
            .expect("read live source filesystem type");
        let source_baseline = classify_linux_filesystem_type(source_filesystem_type);
        let source_capabilities = filesystem_capabilities(&source);
        assert_ne!(
            source_capabilities.semantics,
            FilesystemSemantics::StableLocal,
            "the configured test directory resolved as stable-local rather than CIFS/SMB/NTFS: {source_capabilities:?}",
        );
        assert_eq!(
            source_capabilities.identity_policy(),
            FilesystemIdentityPolicy::ContentVerifiedPortable,
            "reduced or unknown semantics must select portable proof: {source_capabilities:?}",
        );
        let proof = RenameSourceProof::capture(&source).expect("capture live source proof");
        fs::rename(&source, &destination).expect("perform live native rename");
        let (destination_mount_key, destination_probe_path) = linux_mount_descriptor(&destination)
            .expect("resolve live destination mount descriptor");
        let destination_filesystem_type = linux_filesystem_type(&destination_probe_path)
            .expect("read live destination filesystem type");
        let destination_baseline = classify_linux_filesystem_type(destination_filesystem_type);
        let destination_capabilities = filesystem_capabilities(&destination);
        eprintln!(
            "live reduced-filesystem route: source_key={source_mount_key:?}, source_probe={}, source_fs_type={source_filesystem_type:#x}, source_baseline={source_baseline:?}, source_effective={source_capabilities:?}; destination_key={destination_mount_key:?}, destination_probe={}, destination_fs_type={destination_filesystem_type:#x}, destination_baseline={destination_baseline:?}, destination_effective={destination_capabilities:?}",
            source_probe_path.display(),
            destination_probe_path.display(),
        );
        assert_eq!(
            source_mount_key, destination_mount_key,
            "a same-directory rename resolved source and destination to different capability-cache keys"
        );
        assert_eq!(
            source_filesystem_type, destination_filesystem_type,
            "a same-directory rename resolved source and destination to different filesystem types"
        );
        assert_ne!(
            destination_capabilities.semantics,
            FilesystemSemantics::StableLocal,
            "the renamed destination unexpectedly resolved as stable-local: {destination_capabilities:?}",
        );
        assert_eq!(
            destination_capabilities.identity_policy(),
            FilesystemIdentityPolicy::ContentVerifiedPortable,
            "the renamed destination must retain portable proof: {destination_capabilities:?}",
        );

        let verification = verify_committed_rename(
            &source,
            &destination,
            &proof,
            source_capabilities,
            destination_capabilities,
        )
        .expect("a committed live rename must satisfy the portable evidence floor");
        assert!(verification.portable_evidence);
        assert!(
            !source.exists() && destination.exists(),
            "live rename path transition must be committed"
        );

        fs::remove_file(&destination).expect("remove live destination fixture");
        root.close().expect("remove live capability probe directory");
    }

    #[cfg(unix)]
    #[test]
    fn strict_identity_replacement_is_never_downgraded_to_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("source");
        let proof = RenameSourceProof::capture(&source).expect("capture proof");

        fs::rename(&source, &destination).expect("rename");
        fs::rename(&destination, &displaced).expect("displace renamed object");
        fs::write(&destination, b"replaced").expect("same-length replacement");

        let error = verify_committed_rename(
            &source,
            &destination,
            &proof,
            FilesystemCapabilities::assumed_strict(),
            FilesystemCapabilities::assumed_strict(),
        )
        .expect_err("strict object replacement must remain a hard proof failure");

        assert!(error.contains("identity"), "unexpected error: {error}");
    }

    #[test]
    fn manifest_rejects_same_object_content_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        fs::write(&source, b"original").expect("write original");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::write(&source, b"mutated!").expect("mutate source");

        let error = manifest.verify_at(&source).expect_err("mutation must be rejected");
        assert!(
            error.contains("changed") || error.contains("digest"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn manifest_rejects_same_size_replacement_after_quarantine() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let original = temp.path().join("original.bin");
        let quarantine = temp.path().join("quarantine.bin");
        fs::write(&source, b"original").expect("write original");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::rename(&source, &original).expect("displace original");
        fs::write(&source, b"replaced").expect("write same-size replacement");
        fs::rename(&source, &quarantine).expect("quarantine replacement");

        let error = manifest
            .verify_at(&quarantine)
            .expect_err("replacement must not authorize cleanup");
        assert!(
            error.contains("identity") || error.contains("digest"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&quarantine).expect("replacement retained"), b"replaced");
        assert_eq!(fs::read(&original).expect("original retained"), b"original");
    }

    #[test]
    fn directory_manifest_rejects_unplanned_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        fs::create_dir(&source).expect("create source");
        fs::write(source.join("track.flac"), b"audio").expect("write track");
        let manifest = capture_manifest(&source).expect("capture manifest");

        fs::write(source.join("late.flac"), b"late").expect("write late entry");

        let error = manifest
            .verify_at(&source)
            .expect_err("new entry must prevent cleanup");
        assert!(error.contains("membership"), "unexpected error: {error}");
    }

    #[test]
    fn final_destination_entry_proof_rejects_same_size_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write source");
        let manifest = capture_manifest(&source).expect("capture manifest");
        fs::copy(&source, &destination).expect("copy destination");
        manifest
            .verify_copy_entry_at(Path::new(""), &destination)
            .expect("initial destination proof");

        fs::rename(&destination, &displaced).expect("displace copied destination");
        fs::write(&destination, b"replaced").expect("write same-size replacement");

        let error = manifest
            .verify_copy_entry_at(Path::new(""), &destination)
            .expect_err("replacement must revoke source-deletion authority");
        assert!(error.contains("digest"), "unexpected error: {error}");
        assert_eq!(fs::read(&destination).expect("replacement retained"), b"replaced");
        assert_eq!(fs::read(&displaced).expect("original copy retained"), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn strict_destination_stability_uses_exact_ctime_version_token() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("write source");
        let source_manifest = capture_manifest(&source).expect("capture source manifest");
        fs::copy(&source, &destination).expect("copy destination");
        let mut destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("capture verified destination");

        let expected = destination_manifest
            .entries
            .get_mut(Path::new(""))
            .expect("root destination proof");
        match &mut expected.version {
            SourceVersion::Unix {
                ctime_sec,
                ctime_nsec,
                ..
            } => {
                if *ctime_nsec < 999_999_999 {
                    *ctime_nsec += 1;
                } else {
                    *ctime_nsec = 0;
                    *ctime_sec = (*ctime_sec).saturating_add(1);
                }
            }
        }

        let current = snapshot_path(&destination).expect("current destination snapshot");
        expected
            .verify_same_object_after_rename_with_capabilities(
                &current,
                FilesystemCapabilities::assumed_strict(),
            )
            .expect("the rename comparator intentionally ignores ctime");

        let mut keep_going = |_: &Path| true;
        let error = destination_manifest
            .verify_entry_at_with_cancel_with_capabilities(
                &source_manifest,
                Path::new(""),
                &destination,
                FilesystemCapabilities::assumed_strict(),
                &mut keep_going,
            )
            .expect_err("the post-verification stability gate must include ctime");
        assert!(error.contains("version"), "unexpected error: {error}");
    }

    #[test]
    fn portable_destination_stability_rehashes_before_source_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("write source");
        let source_manifest = capture_manifest(&source).expect("capture source manifest");
        fs::copy(&source, &destination).expect("copy destination");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("capture verified destination");

        fs::write(&destination, b"replaced").expect("same-length in-place mutation");

        let mut keep_going = |_: &Path| true;
        let error = destination_manifest
            .verify_entry_at_with_cancel_with_capabilities(
                &source_manifest,
                Path::new(""),
                &destination,
                FilesystemCapabilities::assumed_portable(),
                &mut keep_going,
            )
            .expect_err("portable final rehash must revoke cleanup authority");
        assert!(error.contains("digest"), "unexpected error: {error}");
    }

    #[cfg(unix)]
    #[test]
    fn strict_source_cleanup_rejects_ctime_only_descendant_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let quarantine = temp.path().join("quarantine");
        fs::create_dir(&source).expect("source directory");
        fs::write(source.join("track.bin"), b"original").expect("source child");
        let mut manifest = capture_manifest(&source).expect("capture source manifest");
        fs::rename(&source, &quarantine).expect("quarantine source root");

        let proof = manifest
            .entries
            .get_mut(Path::new("track.bin"))
            .expect("child proof");
        match &mut proof.snapshot.version {
            SourceVersion::Unix {
                ctime_sec,
                ctime_nsec,
                ..
            } => {
                if *ctime_nsec < 999_999_999 {
                    *ctime_nsec += 1;
                } else {
                    *ctime_nsec = 0;
                    *ctime_sec = (*ctime_sec).saturating_add(1);
                }
            }
        }

        let mut keep_going = |_: &Path| true;
        let error = verify_source_entry_after_root_rename_with_capabilities(
            &quarantine.join("track.bin"),
            proof,
            false,
            VerificationMode::Strong,
            FilesystemCapabilities::assumed_strict(),
            &mut keep_going,
        )
        .expect_err("strict descendant cleanup must include the copy-time ctime token");
        assert!(error.contains("changed"), "unexpected error: {error}");
    }

    #[test]
    fn portable_destination_stability_rechecks_directory_membership() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::write(source.join("track.bin"), b"original").expect("source child");
        let source_manifest = capture_manifest(&source).expect("capture source manifest");
        fs::create_dir(&destination).expect("destination directory");
        fs::copy(source.join("track.bin"), destination.join("track.bin"))
            .expect("copy destination child");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("capture verified destination");

        fs::write(destination.join("unexpected.bin"), b"unexpected")
            .expect("add unexpected destination entry");

        let mut keep_going = |_: &Path| true;
        let error = destination_manifest
            .verify_entry_at_with_cancel_with_capabilities(
                &source_manifest,
                Path::new(""),
                &destination,
                FilesystemCapabilities::assumed_portable(),
                &mut keep_going,
            )
            .expect_err("portable directory membership change must revoke cleanup authority");
        assert!(error.contains("membership"), "unexpected error: {error}");
    }

    #[test]
    fn destination_verifiers_enumerate_each_directory_once_per_recursive_pass() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::create_dir(source.join("disc")).expect("source nested directory");
        fs::write(source.join("disc/track.bin"), b"audio").expect("source file");
        fs::create_dir(&destination).expect("destination directory");
        fs::create_dir(destination.join("disc")).expect("destination nested directory");
        fs::copy(
            source.join("disc/track.bin"),
            destination.join("disc/track.bin"),
        )
        .expect("destination file");
        let source_manifest = capture_manifest(&source).expect("source manifest");

        reset_test_destination_directory_enumerations();
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("initial destination verification");
        assert_eq!(
            take_test_destination_directory_enumerations(),
            2,
            "initial verification must enumerate the root and nested directory exactly once"
        );

        reset_test_destination_directory_enumerations();
        destination_manifest
            .verify_reused_copy_at_with_cancel(
                &source_manifest,
                &destination,
                |_: &Path| true,
            )
            .expect("retry destination verification");
        assert_eq!(
            take_test_destination_directory_enumerations(),
            2,
            "retry verification must reuse each directory listing for membership and descent"
        );
    }

    #[test]
    fn destination_identity_manifest_rejects_same_content_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let displaced = temp.path().join("displaced.bin");
        fs::write(&source, b"original").expect("write source");
        let source_manifest = capture_manifest(&source).expect("capture source manifest");
        fs::copy(&source, &destination).expect("copy destination");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("capture verified destination identity");

        fs::rename(&destination, &displaced).expect("displace verified destination");
        fs::write(&destination, b"original").expect("write same-content replacement");

        let mut keep_going = |_: &Path| true;
        let error = destination_manifest
            .verify_entry_at_with_cancel_with_capabilities(
                &source_manifest,
                Path::new(""),
                &destination,
                FilesystemCapabilities::assumed_strict(),
                &mut keep_going,
            )
            .expect_err("strict destination proof must reject same-content replacement");
        assert!(
            error.contains("object") || error.contains("version"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&destination).expect("replacement retained"), b"original");
        assert_eq!(fs::read(&displaced).expect("verified copy retained"), b"original");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceEntryProof {
    pub snapshot: SourceSnapshot,
    pub digest: Option<ContentDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceManifest {
    verification: VerificationMode,
    entries: std::collections::BTreeMap<PathBuf, SourceEntryProof>,
}

impl Default for SourceManifest {
    fn default() -> Self {
        Self::new(VerificationMode::Strong)
    }
}

/// Identity/version snapshots for the exact destination objects accepted at
/// the manifest's verification authority. A later source cleanup step must
/// satisfy both this destination-ownership proof and the source manifest at
/// the same authority before it may remove the corresponding quarantined
/// source object.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinationManifest {
    verification: VerificationMode,
    entries: std::collections::BTreeMap<PathBuf, SourceSnapshot>,
}

impl Default for DestinationManifest {
    fn default() -> Self {
        Self::new(VerificationMode::Strong)
    }
}

impl DestinationManifest {
    pub fn new(verification: VerificationMode) -> Self {
        Self {
            verification,
            entries: std::collections::BTreeMap::new(),
        }
    }

    pub const fn verification(&self) -> VerificationMode {
        self.verification
    }

    /// Update only the root snapshot after a staging tree is atomically
    /// renamed into its public destination. Descendant objects are unchanged
    /// by the root namespace transition.
    pub fn identity_after_root_rename(
        &self,
        destination_root: SourceSnapshot,
        capabilities: FilesystemCapabilities,
    ) -> Result<Self, String> {
        let expected_root = self
            .entries
            .get(Path::new(""))
            .ok_or_else(|| "destination manifest has no root entry".to_string())?;
        expected_root
            .verify_same_identity_with_policy(&destination_root, capabilities.identity_policy())
            .map_err(|error| format!("published root does not match staged identity: {error}"))?;
        let mut published = self.clone();
        published
            .entries
            .insert(PathBuf::new(), destination_root);
        Ok(published)
    }

    /// Reject operation proofs whose captured metadata already makes
    /// delete-based copy undo unsupported. This is a zero-I/O classification
    /// used by the worker before offering undo; destructive cleanup repeats a
    /// complete live authority/ACL/immutable-state preflight later.
    pub fn validate_copy_undo_metadata_contract(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            let effective_uid = unsafe { libc::geteuid() };
            for (relative, snapshot) in &self.entries {
                let SourceVersion::Unix { mode, uid, .. } = &snapshot.version;
                if *uid != effective_uid {
                    return Err(format!(
                        "entry {} is not owned by the current user",
                        relative.display(),
                    ));
                }
                match snapshot.kind() {
                    SourceKind::File => {
                        if *mode & 0o400 == 0 {
                            return Err(format!(
                                "file {} is not owner-readable",
                                relative.display(),
                            ));
                        }
                        if *mode & 0o022 != 0 {
                            return Err(format!(
                                "file {} is group- or other-writable",
                                relative.display(),
                            ));
                        }
                    }
                    SourceKind::Directory => {
                        if *mode & 0o700 != 0o700 {
                            return Err(format!(
                                "directory {} lacks owner read/write/execute permission",
                                relative.display(),
                            ));
                        }
                        if *mode & 0o022 != 0 {
                            return Err(format!(
                                "directory {} is group- or other-writable",
                                relative.display(),
                            ));
                        }
                    }
                    SourceKind::Symlink => {}
                }
            }
        }
        Ok(())
    }

    /// Build a cleanup manifest for the exact published objects represented by
    /// this destination proof. Digests come from the copy-time source proof;
    /// identity/version snapshots come from the destination objects that
    /// passed publication verification. The result can safely follow a root
    /// rename into a private quarantine before destructive removal.
    pub fn cleanup_manifest(
        &self,
        source_manifest: &SourceManifest,
    ) -> Result<SourceManifest, String> {
        self.require_matching_verification(source_manifest.verification())?;
        if !self.entries.keys().eq(source_manifest.entries.keys()) {
            return Err(
                "destination proof no longer corresponds to the source manifest".to_string(),
            );
        }
        let mut cleanup = SourceManifest::new(self.verification);
        for (relative, snapshot) in &self.entries {
            let source = source_manifest.entries.get(relative).ok_or_else(|| {
                format!("missing source proof for {}", relative.display())
            })?;
            cleanup.insert(relative.clone(), snapshot.clone(), source.digest)?;
        }
        Ok(cleanup)
    }

    /// Verify that a manifest captured by a replay worker is still the exact
    /// tree authorized by an earlier operation proof. This comparison happens
    /// before publication or source removal and consumes the manifest already
    /// produced by the mutation engine, so it adds no second recursive hash
    /// pass.
    pub fn verify_captured_replay_source(
        &self,
        operation_source: &SourceManifest,
        captured_source: &SourceManifest,
        capabilities: FilesystemCapabilities,
    ) -> Result<(), String> {
        self.require_matching_verification(operation_source.verification())?;
        self.require_matching_verification(captured_source.verification())?;
        if !self.entries.keys().eq(operation_source.entries.keys())
            || !self.entries.keys().eq(captured_source.entries.keys())
        {
            return Err(
                "replay source tree membership no longer matches operation-time proof"
                    .to_string(),
            );
        }

        for (relative, expected_destination) in &self.entries {
            let operation_entry = operation_source.entries.get(relative).ok_or_else(|| {
                format!("missing operation-time source proof for {}", relative.display())
            })?;
            let captured_entry = captured_source.entries.get(relative).ok_or_else(|| {
                format!("missing replay source proof for {}", relative.display())
            })?;

            expected_destination
                .verify_same_object_and_version_with_capabilities(
                    &captured_entry.snapshot,
                    capabilities,
                )
                .map_err(|error| {
                    format!(
                        "replay source identity changed at {}: {error}",
                        relative.display(),
                    )
                })?;
            if self.verification == VerificationMode::Strong
                && operation_entry.digest != captured_entry.digest
            {
                return Err(format!(
                    "replay source content changed at {}",
                    relative.display(),
                ));
            }
        }
        Ok(())
    }

    fn require_matching_verification(
        &self,
        verification: VerificationMode,
    ) -> Result<(), String> {
        if self.verification != verification {
            return Err(format!(
                "verification authority mismatch: retained proof is {:?}, replay proof is {:?}",
                self.verification, verification
            ));
        }
        Ok(())
    }

    pub fn insert(
        &mut self,
        relative_path: PathBuf,
        snapshot: SourceSnapshot,
    ) -> Result<(), String> {
        if self.entries.insert(relative_path.clone(), snapshot).is_some() {
            return Err(format!(
                "duplicate destination manifest entry: {}",
                relative_path.display()
            ));
        }
        Ok(())
    }

    /// Reconfirm that an entry which passed authoritative verification,
    /// checked against the manifest's retained authority level, still
    /// authorizes destructive source cleanup.
    pub fn verify_entry_at(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_entry_at_with_cancel(
            source_manifest,
            relative_path,
            path,
            &mut keep_going,
        )
    }

    pub fn verify_entry_at_with_cancel<F>(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.verify_entry_at_with_cancel_counted(
            source_manifest,
            relative_path,
            path,
            keep_going,
        )
        .map(|_| ())
    }

    /// Counted form of the final destination-stability proof.
    ///
    /// Strict mounts use the exact captured object/version token, including
    /// ctime. The weaker comparator used specifically for a rename transition
    /// is deliberately not reused here because no destination rename occurs
    /// after this manifest is captured.
    ///
    /// Reduced-semantics mounts cannot make pathname identity or coarse
    /// timestamps authoritative. Regular files are therefore rehashed once,
    /// immediately before the corresponding source entry is removed. The
    /// returned byte count lets callers account for that irreducible final read.
    pub fn verify_entry_at_with_cancel_counted<F>(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<u64, String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.verify_entry_at_with_cancel_with_capabilities(
            source_manifest,
            relative_path,
            path,
            filesystem_capabilities(path),
            keep_going,
        )
    }

    fn verify_entry_at_with_cancel_with_capabilities<F>(
        &self,
        source_manifest: &SourceManifest,
        relative_path: &Path,
        path: &Path,
        capabilities: FilesystemCapabilities,
        keep_going: &mut F,
    ) -> Result<u64, String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.require_matching_verification(source_manifest.verification())?;
        if !keep_going(path) {
            return Err("destination ownership verification was interrupted".to_string());
        }
        let expected_destination = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no captured identity proof: {}",
                relative_path.display()
            )
        })?;
        let source_proof = source_manifest.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no source proof: {}",
                relative_path.display()
            )
        })?;

        if self.verification == VerificationMode::Standard {
            if source_proof.snapshot.kind() == SourceKind::Directory {
                verify_portable_destination_directory_membership(
                    path,
                    source_manifest.expected_direct_children(relative_path),
                )?;
            }
            verify_identity_destination_entry(path, expected_destination, capabilities)?;
            return Ok(0);
        }

        if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
            verify_exact_destination_entry(path, expected_destination)?;
            return Ok(0);
        }

        if source_proof.snapshot.kind() == SourceKind::Directory {
            verify_portable_destination_directory_membership(
                path,
                source_manifest.expected_direct_children(relative_path),
            )?;
        }
        let current_destination = verify_destination_entry(path, source_proof, keep_going)?;
        expected_destination
            .verify_same_identity_with_policy(
                &current_destination,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )
            .map_err(|error| {
                format!(
                    "destination object changed after content verification at {}: {error}",
                    path.display()
                )
            })?;
        Ok(if source_proof.snapshot.kind() == SourceKind::File {
            source_proof.snapshot.len()
        } else {
            0
        })
    }

    /// Revalidate a previously verified published tree for retry.
    ///
    /// Strict mounts reuse retained pathname identity/version evidence without
    /// rereading file contents. Reduced-semantics mounts cannot prove that a
    /// same-size pathname still contains the previously verified bytes across
    /// operation attempts, so each regular file is rehashed exactly once. The
    /// snapshot returned by that same read is reused for the retained-object
    /// comparison; no second pathname verification helper restats the entry.
    pub fn verify_reused_copy_at_with_cancel<F>(
        &self,
        source_manifest: &SourceManifest,
        root: &Path,
        mut keep_going: F,
    ) -> Result<u64, String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.require_matching_verification(source_manifest.verification())?;
        if !self.entries.keys().eq(source_manifest.entries.keys()) {
            return Err(
                "retry destination proof no longer corresponds to the source manifest"
                    .to_string(),
            );
        }
        let mut visited = std::collections::BTreeSet::new();
        let mut destination_bytes_rehashed = 0u64;
        verify_reused_destination_node(
            self,
            source_manifest,
            root,
            Path::new(""),
            0,
            &mut visited,
            &mut destination_bytes_rehashed,
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            source_manifest.entries.keys().cloned().collect();
        if visited != expected {
            let missing = expected
                .difference(&visited)
                .take(8)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            return Err(format!(
                "retry destination tree is missing expected entries: [{}]",
                missing.join(", ")
            ));
        }
        Ok(destination_bytes_rehashed)
    }

}

fn verify_portable_destination_directory_membership(
    path: &Path,
    expected_children: std::collections::BTreeSet<std::ffi::OsString>,
) -> Result<(), String> {
    let actual_children = sorted_directory_entries(path)?
        .into_iter()
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    if actual_children == expected_children {
        Ok(())
    } else {
        Err(format!(
            "destination directory membership changed after proof capture at {}",
            path.display()
        ))
    }
}

fn verify_exact_destination_entry(
    path: &Path,
    expected: &SourceSnapshot,
) -> Result<(), String> {
    if expected.kind() == SourceKind::File {
        let file = File::open(path)
            .map_err(|error| format!("open verified destination {}: {error}", path.display()))?;
        let opened = snapshot_open_file(&file)
            .map_err(|error| format!("identify verified destination {}: {error}", path.display()))?;
        expected
            .verify_same_object_and_version(&opened)
            .map_err(|error| {
                format!(
                    "destination object/version changed after content verification at {}: {error}",
                    path.display()
                )
            })?;
        let pathname = snapshot_path(path)
            .map_err(|error| format!("re-identify destination path {}: {error}", path.display()))?;
        opened
            .verify_same_object_and_version(&pathname)
            .map_err(|error| {
                format!(
                    "destination pathname changed after content verification at {}: {error}",
                    path.display()
                )
            })?;
        return Ok(());
    }

    let current = snapshot_path(path)
        .map_err(|error| format!("re-identify destination {}: {error}", path.display()))?;
    expected
        .verify_same_object_and_version(&current)
        .map_err(|error| {
            format!(
                "destination object/version changed after content verification at {}: {error}",
                path.display()
            )
        })
}

fn verify_identity_destination_entry(
    path: &Path,
    expected: &SourceSnapshot,
    capabilities: FilesystemCapabilities,
) -> Result<(), String> {
    let policy = capabilities.identity_policy();
    if expected.kind() == SourceKind::File {
        let file = File::open(path)
            .map_err(|error| format!("open destination {}: {error}", path.display()))?;
        let opened = snapshot_open_file(&file)
            .map_err(|error| format!("identify destination {}: {error}", path.display()))?;
        let compare_expected = if policy == FilesystemIdentityPolicy::Strict {
            expected.verify_same_object_and_version(&opened)
        } else {
            expected.verify_same_identity_with_policy(&opened, policy)
        };
        compare_expected.map_err(|error| {
            format!(
                "destination identity/version changed at {}: {error}",
                path.display()
            )
        })?;
        let pathname = snapshot_path(path)
            .map_err(|error| format!("re-identify destination {}: {error}", path.display()))?;
        let bind_path = if policy == FilesystemIdentityPolicy::Strict {
            opened.verify_same_object_and_version(&pathname)
        } else {
            opened.verify_same_identity_with_policy(&pathname, policy)
        };
        return bind_path.map_err(|error| {
            format!(
                "destination pathname changed while identity was checked at {}: {error}",
                path.display()
            )
        });
    }

    let current = snapshot_path(path)
        .map_err(|error| format!("identify destination {}: {error}", path.display()))?;
    let comparison = if policy == FilesystemIdentityPolicy::Strict {
        expected.verify_same_object_and_version(&current)
    } else {
        expected.verify_same_identity_with_policy(&current, policy)
    };
    comparison.map_err(|error| {
        format!(
            "destination identity/version changed at {}: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
std::thread_local! {
    static TEST_DESTINATION_DIRECTORY_ENUMERATIONS: std::cell::Cell<u64> =
        std::cell::Cell::new(0);
}

#[cfg(test)]
fn reset_test_destination_directory_enumerations() {
    TEST_DESTINATION_DIRECTORY_ENUMERATIONS.with(|count| count.set(0));
}

#[cfg(test)]
fn take_test_destination_directory_enumerations() -> u64 {
    TEST_DESTINATION_DIRECTORY_ENUMERATIONS.with(|count| count.replace(0))
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    #[cfg(test)]
    TEST_DESTINATION_DIRECTORY_ENUMERATIONS.with(|count| {
        count.set(count.get().saturating_add(1));
    });
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("read destination directory {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read destination entry {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
fn capture_verified_destination_node<F>(
    source_manifest: &SourceManifest,
    path: &Path,
    relative: &Path,
    depth: usize,
    visited: &mut std::collections::BTreeSet<PathBuf>,
    destination_manifest: &mut DestinationManifest,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if depth > MAX_MANIFEST_DEPTH {
        return Err(format!(
            "destination tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
            path.display()
        ));
    }
    if !visited.insert(relative.to_path_buf()) {
        return Err(format!(
            "duplicate destination entry while verifying {}",
            relative.display()
        ));
    }
    if visited.len() > source_manifest.entries.len() {
        return Err(format!(
            "destination tree contains an unexpected entry: {}",
            relative.display()
        ));
    }
    let proof = source_manifest.entries.get(relative).ok_or_else(|| {
        format!(
            "destination tree contains an unexpected entry: {}",
            relative.display()
        )
    })?;
    let snapshot = verify_destination_entry(path, proof, keep_going)?;
    destination_manifest.insert(relative.to_path_buf(), snapshot)?;

    if proof.snapshot.kind() == SourceKind::Directory {
        for entry in sorted_directory_entries(path)? {
            let child_relative = relative.join(entry.file_name());
            capture_verified_destination_node(
                source_manifest,
                &entry.path(),
                &child_relative,
                depth + 1,
                visited,
                destination_manifest,
                keep_going,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn capture_identity_destination_node<F>(
    source_manifest: &SourceManifest,
    path: &Path,
    relative: &Path,
    depth: usize,
    visited: &mut std::collections::BTreeSet<PathBuf>,
    destination_manifest: &mut DestinationManifest,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err("destination identity capture was interrupted".to_string());
    }
    if depth > MAX_MANIFEST_DEPTH {
        return Err(format!(
            "destination tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
            path.display()
        ));
    }
    if !visited.insert(relative.to_path_buf()) {
        return Err(format!(
            "duplicate destination entry while capturing identity: {}",
            relative.display()
        ));
    }
    if visited.len() > source_manifest.entries.len() {
        return Err(format!(
            "destination tree contains an unexpected entry: {}",
            relative.display()
        ));
    }
    let source = source_manifest.entries.get(relative).ok_or_else(|| {
        format!(
            "destination tree contains an unexpected entry: {}",
            relative.display()
        )
    })?;
    let snapshot = snapshot_path(path)
        .map_err(|error| format!("identify destination {}: {error}", path.display()))?;
    source
        .snapshot
        .verify_same_identity_with_policy(
            &snapshot,
            FilesystemIdentityPolicy::ContentVerifiedPortable,
        )
        .map_err(|error| format!("destination shape mismatch at {}: {error}", path.display()))?;
    destination_manifest.insert(relative.to_path_buf(), snapshot)?;

    if source.snapshot.kind() == SourceKind::Directory {
        for entry in sorted_directory_entries(path)? {
            let child_relative = relative.join(entry.file_name());
            capture_identity_destination_node(
                source_manifest,
                &entry.path(),
                &child_relative,
                depth + 1,
                visited,
                destination_manifest,
                keep_going,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_reused_destination_node<F>(
    destination_manifest: &DestinationManifest,
    source_manifest: &SourceManifest,
    path: &Path,
    relative: &Path,
    depth: usize,
    visited: &mut std::collections::BTreeSet<PathBuf>,
    destination_bytes_rehashed: &mut u64,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if depth > MAX_MANIFEST_DEPTH {
        return Err(format!(
            "retry destination tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
            path.display()
        ));
    }
    if !visited.insert(relative.to_path_buf()) {
        return Err(format!(
            "duplicate retry destination entry while verifying {}",
            relative.display()
        ));
    }
    if visited.len() > source_manifest.entries.len() {
        return Err(format!(
            "retry destination tree contains an unexpected entry: {}",
            relative.display()
        ));
    }
    let source_proof = source_manifest.entries.get(relative).ok_or_else(|| {
        format!(
            "retry destination tree contains an unexpected entry: {}",
            relative.display()
        )
    })?;

    let directory_entries = if source_proof.snapshot.kind() == SourceKind::Directory {
        let expected_destination = destination_manifest.entries.get(relative).ok_or_else(|| {
            format!(
                "retry destination entry has no retained proof: {}",
                relative.display()
            )
        })?;
        let capabilities = filesystem_capabilities(path);
        if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
            if !keep_going(path) {
                return Err("retry destination verification was interrupted".to_string());
            }
            verify_exact_destination_entry(path, expected_destination)?;
        } else {
            let current = verify_destination_entry(path, source_proof, keep_going)?;
            expected_destination
                .verify_same_identity_with_policy(
                    &current,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
                .map_err(|error| {
                    format!(
                        "retry destination object changed at {}: {error}",
                        path.display()
                    )
                })?;
        }
        Some(sorted_directory_entries(path)?)
    } else {
        let rehashed = destination_manifest.verify_entry_at_with_cancel_counted(
            source_manifest,
            relative,
            path,
            keep_going,
        )?;
        *destination_bytes_rehashed = (*destination_bytes_rehashed).saturating_add(rehashed);
        None
    };

    if let Some(entries) = directory_entries {
        for entry in entries {
            let child_relative = relative.join(entry.file_name());
            verify_reused_destination_node(
                destination_manifest,
                source_manifest,
                &entry.path(),
                &child_relative,
                depth + 1,
                visited,
                destination_bytes_rehashed,
                keep_going,
            )?;
        }
    }
    Ok(())
}

impl SourceManifest {
    pub fn new(verification: VerificationMode) -> Self {
        Self {
            verification,
            entries: std::collections::BTreeMap::new(),
        }
    }

    pub const fn verification(&self) -> VerificationMode {
        self.verification
    }

    pub fn has_content_digests(&self) -> bool {
        self.entries.values().any(|entry| entry.digest.is_some())
    }

    /// Convert a manifest captured from a tree in place into the destination
    /// identity half of an operation proof without another filesystem pass.
    /// Every snapshot already names the exact object/version observed during
    /// the authoritative capture; file digests remain in `self`.
    pub fn destination_identity_for_same_tree(&self) -> DestinationManifest {
        DestinationManifest {
            verification: self.verification,
            entries: self
                .entries
                .iter()
                .map(|(relative, proof)| (relative.clone(), proof.snapshot.clone()))
                .collect(),
        }
    }

    /// Build destination identity after a root rename using the verified
    /// post-rename root snapshot and the pre-operation descendant snapshots.
    /// Renaming a directory does not recreate its descendants, so this avoids
    /// a post-publication tree walk while still accounting for root metadata
    /// changes caused by the namespace operation.
    pub fn destination_identity_after_root_rename(
        &self,
        destination_root: SourceSnapshot,
        capabilities: FilesystemCapabilities,
    ) -> Result<DestinationManifest, String> {
        let expected_root = self
            .entries
            .get(Path::new(""))
            .ok_or_else(|| "source manifest has no root entry".to_string())?;
        expected_root
            .snapshot
            .verify_same_identity_with_policy(
                &destination_root,
                capabilities.identity_policy(),
            )
            .map_err(|error| format!("renamed root does not match source manifest: {error}"))?;
        let mut destination = self.destination_identity_for_same_tree();
        destination
            .entries
            .insert(PathBuf::new(), destination_root);
        Ok(destination)
    }

    pub fn root_kind(&self) -> Option<SourceKind> {
        self.entries.get(Path::new("")).map(|entry| entry.snapshot.kind())
    }

    pub fn expected_snapshot(&self, relative_path: &Path) -> Option<&SourceSnapshot> {
        self.entries.get(relative_path).map(|entry| &entry.snapshot)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn total_file_bytes(&self) -> u64 {
        self.entries
            .values()
            .filter(|entry| entry.snapshot.kind() == SourceKind::File)
            .fold(0u64, |total, entry| total.saturating_add(entry.snapshot.len()))
    }

    pub fn entry_proof(&self, relative_path: &Path) -> Option<&SourceEntryProof> {
        self.entries.get(relative_path)
    }

    pub fn relative_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.entries.keys()
    }

    pub fn expected_direct_children(
        &self,
        relative_directory: &Path,
    ) -> std::collections::BTreeSet<std::ffi::OsString> {
        self.entries
            .keys()
            .filter_map(|relative| {
                let parent = relative.parent().unwrap_or_else(|| Path::new(""));
                (parent == relative_directory)
                    .then(|| relative.file_name().map(|name| name.to_os_string()))
                    .flatten()
            })
            .collect()
    }

    /// Revalidate one manifest entry at its quarantined pathname immediately
    /// before unlinking it.
    pub fn verify_entry_at(
        &self,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_entry_at_with_cancel(relative_path, path, &mut keep_going)
    }

    pub fn verify_entry_at_with_cancel<F>(
        &self,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.verify_cleanup_entry_at_with_cancel(relative_path, path, keep_going)
            .map(|_| ())
    }

    /// Counted cleanup form. Standard authority uses identity/version and tree
    /// membership only on every mount. Strong authority preserves the historical
    /// content fallback: a regular-file root is rehashed after quarantine changes
    /// its rename-sensitive version token, and reduced-semantics mounts rehash
    /// regular files whose identity token cannot carry the strong proof alone.
    pub fn verify_cleanup_entry_at(
        &self,
        relative_path: &Path,
        path: &Path,
    ) -> Result<u64, String> {
        let mut keep_going = |_: &Path| true;
        self.verify_cleanup_entry_at_with_cancel(relative_path, path, &mut keep_going)
    }

    pub fn verify_cleanup_entry_at_with_cancel<F>(
        &self,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<u64, String>
    where
        F: FnMut(&Path) -> bool,
    {
        let proof = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "unplanned source entry appeared during cleanup: {}",
                relative_path.display()
            )
        })?;
        verify_source_entry_after_root_rename(
            path,
            proof,
            relative_path.as_os_str().is_empty(),
            self.verification,
            keep_going,
        )
    }

    /// Revalidate the complete tree after its root has been atomically renamed.
    /// Membership is checked first, then every entry is bound to the retained
    /// object identity and content proof using root-rename-aware semantics.
    pub fn verify_cleanup_tree_at(&self, root: &Path) -> Result<u64, String> {
        if !self.entries.contains_key(Path::new("")) {
            return Err("cleanup manifest has no root entry".to_string());
        }
        let mut keep_going = |_: &Path| true;
        let actual = enumerate_relative_paths_with_cancel(
            root,
            self.entries.len().saturating_add(1),
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            self.entries.keys().cloned().collect();
        if actual != expected {
            let added = actual
                .difference(&expected)
                .take(8)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            let missing = expected
                .difference(&actual)
                .take(8)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            return Err(format!(
                "cleanup tree membership changed (unexpected: [{}]; missing: [{}])",
                added.join(", "),
                missing.join(", "),
            ));
        }

        let mut rehashed = 0u64;
        for relative in self.entries.keys() {
            let path = if relative.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(relative)
            };
            rehashed = rehashed.saturating_add(
                self.verify_cleanup_entry_at_with_cancel(
                    relative,
                    &path,
                    &mut keep_going,
                )?,
            );
        }
        Ok(rehashed)
    }

    /// Revalidate one destination entry against a strong source proof used to
    /// copy it. Standard authority deliberately has no content digest and must
    /// use the destination-identity cleanup gate instead.
    pub fn verify_copy_entry_at(
        &self,
        relative_path: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let mut keep_going = |_: &Path| true;
        self.verify_copy_entry_at_with_cancel(relative_path, path, &mut keep_going)
    }

    pub fn verify_copy_entry_at_with_cancel<F>(
        &self,
        relative_path: &Path,
        path: &Path,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        if self.verification != VerificationMode::Strong {
            return Err(
                "content copy verification requires a strong source manifest".to_string(),
            );
        }
        let proof = self.entries.get(relative_path).ok_or_else(|| {
            format!(
                "destination entry has no source proof: {}",
                relative_path.display()
            )
        })?;
        verify_destination_entry(path, proof, keep_going).map(|_| ())
    }

    pub fn insert(
        &mut self,
        relative_path: PathBuf,
        snapshot: SourceSnapshot,
        digest: Option<ContentDigest>,
    ) -> Result<(), String> {
        if snapshot.kind() == SourceKind::File {
            match (self.verification, digest.is_some()) {
                (VerificationMode::Strong, false) => {
                    return Err(format!(
                        "strong regular-file proof is missing a content digest: {}",
                        relative_path.display()
                    ));
                }
                (VerificationMode::Standard, true) => {
                    return Err(format!(
                        "standard regular-file proof unexpectedly contains a content digest: {}",
                        relative_path.display()
                    ));
                }
                _ => {}
            }
        }
        if !snapshot.supports_identity_proof() {
            return Err(format!(
                "stable source identity is unavailable on this platform for {}",
                relative_path.display()
            ));
        }
        if self
            .entries
            .insert(relative_path.clone(), SourceEntryProof { snapshot, digest })
            .is_some()
        {
            return Err(format!(
                "duplicate source manifest entry: {}",
                relative_path.display()
            ));
        }
        Ok(())
    }

    pub fn verify_copy_at(&self, root: &Path) -> Result<(), String> {
        self.capture_verified_copy_at(root).map(|_| ())
    }

    pub fn verify_copy_at_with_cancel<F>(
        &self,
        root: &Path,
        keep_going: F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        self.capture_verified_copy_at_with_cancel(root, keep_going)
            .map(|_| ())
    }

    pub fn capture_verified_copy_at(
        &self,
        root: &Path,
    ) -> Result<DestinationManifest, String> {
        self.capture_verified_copy_at_with_cancel(root, |_: &Path| true)
    }

    pub fn capture_verified_copy_at_with_cancel<F>(
        &self,
        root: &Path,
        mut keep_going: F,
    ) -> Result<DestinationManifest, String>
    where
        F: FnMut(&Path) -> bool,
    {
        if !self.entries.contains_key(Path::new("")) {
            return Err("source manifest has no root entry".to_string());
        }
        let mut destination_manifest = DestinationManifest::new(self.verification);
        let mut visited = std::collections::BTreeSet::new();
        capture_verified_destination_node(
            self,
            root,
            Path::new(""),
            0,
            &mut visited,
            &mut destination_manifest,
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            self.entries.keys().cloned().collect();
        if visited != expected {
            let missing = expected
                .difference(&visited)
                .take(8)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            return Err(format!(
                "destination tree is missing expected entries: [{}]",
                missing.join(", ")
            ));
        }
        Ok(destination_manifest)
    }

    /// Capture destination object identities and tree membership without
    /// reading file contents. This is valid only for a standard manifest.
    pub fn capture_identity_copy_at(
        &self,
        root: &Path,
    ) -> Result<DestinationManifest, String> {
        self.capture_identity_copy_at_with_cancel(root, |_: &Path| true)
    }

    /// Capture destination object identities and tree membership without
    /// reading file contents. This is valid only for a standard manifest.
    pub fn capture_identity_copy_at_with_cancel<F>(
        &self,
        root: &Path,
        mut keep_going: F,
    ) -> Result<DestinationManifest, String>
    where
        F: FnMut(&Path) -> bool,
    {
        if self.verification != VerificationMode::Standard {
            return Err(
                "identity-only destination capture cannot satisfy a strong manifest".to_string(),
            );
        }
        if !self.entries.contains_key(Path::new("")) {
            return Err("source manifest has no root entry".to_string());
        }
        let mut destination_manifest = DestinationManifest::new(self.verification);
        let mut visited = std::collections::BTreeSet::new();
        capture_identity_destination_node(
            self,
            root,
            Path::new(""),
            0,
            &mut visited,
            &mut destination_manifest,
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> = self.entries.keys().cloned().collect();
        if visited != expected {
            let missing = expected
                .difference(&visited)
                .take(8)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            return Err(format!(
                "destination tree is missing expected entries: [{}]",
                missing.join(", ")
            ));
        }
        Ok(destination_manifest)
    }

    pub fn verify_at(&self, root: &Path) -> Result<(), String> {
        self.verify_at_with_cancel(root, |_: &Path| true)
    }

    pub fn verify_at_with_cancel<F>(
        &self,
        root: &Path,
        mut keep_going: F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        if !self.entries.contains_key(Path::new("")) {
            return Err("source manifest has no root entry".to_string());
        }
        let actual = enumerate_relative_paths_with_cancel(
            root,
            self.entries.len().saturating_add(1),
            &mut keep_going,
        )?;
        let expected: std::collections::BTreeSet<PathBuf> =
            self.entries.keys().cloned().collect();
        if actual != expected {
            let added: Vec<String> = actual
                .difference(&expected)
                .take(8)
                .map(|path| path.display().to_string())
                .collect();
            let missing: Vec<String> = expected
                .difference(&actual)
                .take(8)
                .map(|path| path.display().to_string())
                .collect();
            return Err(format!(
                "source tree membership changed (unexpected: [{}]; missing: [{}])",
                added.join(", "),
                missing.join(", ")
            ));
        }

        for relative in self.entries.keys() {
            let path = if relative.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(relative)
            };
            self.verify_entry_at_with_cancel(relative, &path, &mut keep_going)?;
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn verify_portable_path_file_digest<F>(
    path: &Path,
    expected: ContentDigest,
    keep_going: &mut F,
    role: &str,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err(format!("{role} pathname verification was interrupted"));
    }
    let mut file = File::open(path)
        .map_err(|error| format!("re-open {role} pathname {}: {error}", path.display()))?;
    let before = snapshot_open_file(&file)
        .map_err(|error| format!("identify re-opened {role} {}: {error}", path.display()))?;
    let digest = digest_open_file_with_cancel(&mut file, path, keep_going)
        .map_err(|error| format!("digest re-opened {role} {}: {error}", path.display()))?;
    let after = snapshot_open_file(&file)
        .map_err(|error| format!("re-identify re-opened {role} {}: {error}", path.display()))?;
    before.verify_same_object_and_version(&after).map_err(|error| {
        format!("re-opened {role} changed while being verified at {}: {error}", path.display())
    })?;
    let pathname = snapshot_path(path)
        .map_err(|error| format!("re-identify {role} pathname {}: {error}", path.display()))?;
    after
        .verify_same_identity_with_policy(&pathname, FilesystemIdentityPolicy::ContentVerifiedPortable)
        .map_err(|error| {
            format!("{role} pathname changed after portable verification at {}: {error}", path.display())
        })?;
    if digest != expected {
        return Err(format!(
            "{role} pathname content digest mismatch after portable re-open at {}",
            path.display()
        ));
    }
    Ok(())
}

fn verify_destination_entry<F>(
    path: &Path,
    proof: &SourceEntryProof,
    keep_going: &mut F,
) -> Result<SourceSnapshot, String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err("destination verification was interrupted".to_string());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("stat destination {}: {error}", path.display()))?;
    let kind = kind_from_metadata(&metadata)
        .map_err(|error| format!("classify destination {}: {error}", path.display()))?;
    if kind != proof.snapshot.kind() {
        return Err(format!("destination kind mismatch at {}", path.display()));
    }
    match kind {
        SourceKind::File => {
            let mut file = File::open(path)
                .map_err(|error| format!("open destination {}: {error}", path.display()))?;
            let opened = snapshot_open_file(&file)
                .map_err(|error| format!("identify destination {}: {error}", path.display()))?;
            let digest = digest_open_file_with_cancel(&mut file, path, keep_going)
                .map_err(|error| format!("digest destination {}: {error}", path.display()))?;
            let after = snapshot_open_file(&file)
                .map_err(|error| format!("re-identify destination {}: {error}", path.display()))?;
            opened.verify_same_object_and_version(&after).map_err(|error| {
                format!(
                    "destination changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            let path_snapshot = snapshot_path(path)
                .map_err(|error| format!("re-identify destination path {}: {error}", path.display()))?;
            let path_policy = filesystem_identity_policy(path);
            let path_binding = if path_policy == FilesystemIdentityPolicy::Strict {
                after.verify_same_object_and_version(&path_snapshot)
            } else {
                after.verify_same_identity_with_policy(&path_snapshot, path_policy)
            };
            path_binding.map_err(|error| {
                format!(
                    "destination path changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            if proof.digest != Some(digest) {
                return Err(format!(
                    "destination content digest mismatch at {}",
                    path.display()
                ));
            }
            return Ok(path_snapshot);
        }
        SourceKind::Symlink => {
            let before = snapshot_path(path)
                .map_err(|error| format!("identify destination symlink {}: {error}", path.display()))?;
            let target = fs::read_link(path)
                .map_err(|error| format!("read destination symlink {}: {error}", path.display()))?;
            let after = snapshot_path(path)
                .map_err(|error| format!("re-identify destination symlink {}: {error}", path.display()))?;
            before.verify_same_object_and_version(&after).map_err(|error| {
                format!(
                    "destination symlink changed while being verified at {}: {error}",
                    path.display()
                )
            })?;
            if proof.snapshot.symlink_target.as_ref() != Some(&target) {
                return Err(format!(
                    "destination symlink target mismatch at {}",
                    path.display()
                ));
            }
            return Ok(after);
        }
        SourceKind::Directory => {}
    }
    snapshot_path(path)
        .map_err(|error| format!("identify destination directory {}: {error}", path.display()))
}

fn verify_source_entry_after_root_rename<F>(
    path: &Path,
    proof: &SourceEntryProof,
    moved_root: bool,
    verification: VerificationMode,
    keep_going: &mut F,
) -> Result<u64, String>
where
    F: FnMut(&Path) -> bool,
{
    verify_source_entry_after_root_rename_with_capabilities(
        path,
        proof,
        moved_root,
        verification,
        filesystem_capabilities(path),
        keep_going,
    )
}

fn verify_source_entry_after_root_rename_with_capabilities<F>(
    path: &Path,
    proof: &SourceEntryProof,
    moved_root: bool,
    verification: VerificationMode,
    capabilities: FilesystemCapabilities,
    keep_going: &mut F,
) -> Result<u64, String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(path) {
        return Err("source verification was interrupted".to_string());
    }
    match proof.snapshot.kind() {
        SourceKind::File => {
            let mut file = File::open(path)
                .map_err(|error| format!("open {} for verification: {error}", path.display()))?;
            let before = snapshot_open_file(&file)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            let strict_descendant = capabilities.identity_policy()
                == FilesystemIdentityPolicy::Strict
                && !moved_root;

            if strict_descendant {
                // Renaming the root does not rename descendants. Their exact
                // copy-time identity/version token, including ctime, therefore
                // remains authoritative and avoids a second content read.
                proof
                    .snapshot
                    .verify_same_object_and_version(&before)
                    .map_err(|error| {
                        format!("{} changed before cleanup: {error}", path.display())
                    })?;
                let pathname = snapshot_path(path)
                    .map_err(|error| format!("re-identify path {}: {error}", path.display()))?;
                before
                    .verify_same_object_and_version(&pathname)
                    .map_err(|error| {
                        format!(
                            "{} pathname changed before cleanup: {error}",
                            path.display()
                        )
                    })?;
                return Ok(0);
            }

            // A regular file that is itself the quarantined root has a new ctime
            // because of the rename. Reduced-semantics mounts also lack an exact
            // pathname version token. In either case the copy-time digest is the
            // irreducible final authority immediately before unlink.
            proof
                .snapshot
                .verify_same_object_after_rename_with_capabilities(&before, capabilities)
                .map_err(|error| {
                    format!("{} changed before cleanup: {error}", path.display())
                })?;
            if verification == VerificationMode::Standard {
                let pathname = snapshot_path(path)
                    .map_err(|error| format!("re-identify path {}: {error}", path.display()))?;
                let pathname_binding = if capabilities.identity_policy()
                    == FilesystemIdentityPolicy::Strict
                {
                    before.verify_same_object_and_version(&pathname)
                } else {
                    before.verify_same_identity_with_policy(
                        &pathname,
                        FilesystemIdentityPolicy::ContentVerifiedPortable,
                    )
                };
                pathname_binding.map_err(|error| {
                    format!(
                        "{} pathname changed while its opened object was being checked: {error}",
                        path.display()
                    )
                })?;
                return Ok(0);
            }
            let digest = digest_open_file_with_cancel(&mut file, path, keep_going)
                .map_err(|error| format!("digest {}: {error}", path.display()))?;
            let after = snapshot_open_file(&file)
                .map_err(|error| format!("re-identify {}: {error}", path.display()))?;
            before
                .verify_same_object_and_version(&after)
                .map_err(|error| {
                    format!("{} changed while being verified: {error}", path.display())
                })?;
            let pathname = snapshot_path(path)
                .map_err(|error| format!("re-identify path {}: {error}", path.display()))?;
            let pathname_binding = if capabilities.identity_policy()
                == FilesystemIdentityPolicy::Strict
            {
                after.verify_same_object_and_version(&pathname)
            } else {
                after.verify_same_identity_with_policy(
                    &pathname,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
            };
            pathname_binding.map_err(|error| {
                format!(
                    "{} pathname changed while its opened object was being verified: {error}",
                    path.display()
                )
            })?;
            if proof.digest != Some(digest) {
                return Err(format!(
                    "{} content digest changed before cleanup",
                    path.display()
                ));
            }
            Ok(proof.snapshot.len())
        }
        SourceKind::Directory => {
            let current = snapshot_path(path)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            let precheck = if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                if moved_root {
                    proof
                        .snapshot
                        .verify_same_object_after_rename_with_capabilities(&current, capabilities)
                } else {
                    proof.snapshot.verify_same_object_and_version(&current)
                }
            } else {
                proof.snapshot.verify_same_identity_with_policy(
                    &current,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
            };
            precheck
                .map_err(|error| format!("{} changed before cleanup: {error}", path.display()))?;
            Ok(0)
        }
        SourceKind::Symlink => {
            let before = snapshot_path(path)
                .map_err(|error| format!("identify {}: {error}", path.display()))?;
            let precheck = if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                if moved_root {
                    proof
                        .snapshot
                        .verify_same_object_after_rename_with_capabilities(&before, capabilities)
                } else {
                    proof.snapshot.verify_same_object_and_version(&before)
                }
            } else {
                proof.snapshot.verify_same_identity_with_policy(
                    &before,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
            };
            precheck
                .map_err(|error| format!("{} changed before cleanup: {error}", path.display()))?;
            let target = fs::read_link(path)
                .map_err(|error| format!("read symlink {}: {error}", path.display()))?;
            let after = snapshot_path(path)
                .map_err(|error| format!("re-identify symlink {}: {error}", path.display()))?;
            before
                .verify_same_object_and_version(&after)
                .map_err(|error| {
                    format!("{} changed while being verified: {error}", path.display())
                })?;
            if proof.snapshot.symlink_target.as_ref() != Some(&target) {
                return Err(format!(
                    "{} symlink target changed before cleanup",
                    path.display()
                ));
            }
            Ok(0)
        }
    }
}

pub fn digest_open_file(file: &mut File) -> io::Result<ContentDigest> {
    let mut keep_going = |_: &Path| true;
    digest_open_file_with_cancel(file, Path::new(""), &mut keep_going)
}

fn digest_open_file_with_cancel<F>(
    file: &mut File,
    path: &Path,
    keep_going: &mut F,
) -> io::Result<ContentDigest>
where
    F: FnMut(&Path) -> bool,
{
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut sha = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if !keep_going(path) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "content verification was interrupted",
            ));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha.update(&buffer[..read]);
    }
    Ok(sha.finalize())
}

fn enumerate_relative_paths_with_cancel<F>(
    root: &Path,
    maximum_entries: usize,
    keep_going: &mut F,
) -> Result<std::collections::BTreeSet<PathBuf>, String>
where
    F: FnMut(&Path) -> bool,
{
    let limit = maximum_entries.min(MAX_MANIFEST_ENTRIES.saturating_add(1));
    let mut result = std::collections::BTreeSet::new();
    result.insert(PathBuf::new());
    enumerate_relative_paths_inner(root, Path::new(""), 0, limit, &mut result, keep_going)?;
    Ok(result)
}

fn enumerate_relative_paths_inner<F>(
    absolute: &Path,
    relative: &Path,
    depth: usize,
    maximum_entries: usize,
    result: &mut std::collections::BTreeSet<PathBuf>,
    keep_going: &mut F,
) -> Result<(), String>
where
    F: FnMut(&Path) -> bool,
{
    if !keep_going(absolute) {
        return Err("filesystem tree verification was interrupted".to_string());
    }
    if depth > MAX_MANIFEST_DEPTH {
        return Err(format!(
            "filesystem tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
            absolute.display()
        ));
    }
    let metadata = fs::symlink_metadata(absolute)
        .map_err(|error| format!("stat {}: {error}", absolute.display()))?;
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(absolute)
        .map_err(|error| format!("read directory {}: {error}", absolute.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("read directory entry {}: {error}", absolute.display()))?;
        if !keep_going(&entry.path()) {
            return Err("filesystem tree verification was interrupted".to_string());
        }
        let child_relative = relative.join(entry.file_name());
        if !result.insert(child_relative.clone()) {
            return Err(format!(
                "duplicate directory entry while verifying {}",
                child_relative.display()
            ));
        }
        if result.len() > maximum_entries {
            return Err(format!(
                "filesystem tree contains more entries than the bounded verification limit of {maximum_entries}"
            ));
        }
        enumerate_relative_paths_inner(
            &entry.path(),
            &child_relative,
            depth + 1,
            maximum_entries,
            result,
            keep_going,
        )?;
    }
    Ok(())
}


fn recovery_commitment_os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        return value.as_bytes().to_vec();
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for word in value.encode_wide() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        return bytes;
    }
    #[cfg(not(any(unix, windows)))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}

fn recovery_commitment_update_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn recovery_commitment_update_snapshot(
    hasher: &mut Sha256,
    snapshot: &SourceSnapshot,
    include_rename_sensitive_version: bool,
) {
    hasher.update(&[match snapshot.kind {
        SourceKind::File => 1,
        SourceKind::Directory => 2,
        SourceKind::Symlink => 3,
    }]);
    match &snapshot.identity {
        #[cfg(unix)]
        SourceIdentity::Unix { device, inode } => {
            hasher.update(&[1]);
            hasher.update(&device.to_be_bytes());
            hasher.update(&inode.to_be_bytes());
        }
        #[cfg(windows)]
        SourceIdentity::Windows {
            volume_serial,
            file_index,
        } => {
            hasher.update(&[2]);
            hasher.update(&volume_serial.to_be_bytes());
            hasher.update(&file_index.to_be_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        SourceIdentity::Unsupported => hasher.update(&[0]),
    }
    match &snapshot.version {
        #[cfg(unix)]
        SourceVersion::Unix {
            len,
            mode,
            uid,
            gid,
            mtime_sec,
            mtime_nsec,
            ctime_sec,
            ctime_nsec,
        } => {
            hasher.update(&len.to_be_bytes());
            hasher.update(&mode.to_be_bytes());
            hasher.update(&uid.to_be_bytes());
            hasher.update(&gid.to_be_bytes());
            hasher.update(&mtime_sec.to_be_bytes());
            hasher.update(&mtime_nsec.to_be_bytes());
            if include_rename_sensitive_version {
                hasher.update(&ctime_sec.to_be_bytes());
                hasher.update(&ctime_nsec.to_be_bytes());
            }
        }
        #[cfg(windows)]
        SourceVersion::Windows {
            len,
            creation_time,
            last_write_time,
            attributes,
        } => {
            hasher.update(&len.to_be_bytes());
            hasher.update(&creation_time.to_be_bytes());
            hasher.update(&last_write_time.to_be_bytes());
            hasher.update(&attributes.to_be_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        SourceVersion::Portable { len, modified } => {
            hasher.update(&len.to_be_bytes());
            let token = modified
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| (duration.as_secs(), duration.subsec_nanos()));
            match token {
                Some((seconds, nanos)) => {
                    hasher.update(&[1]);
                    hasher.update(&seconds.to_be_bytes());
                    hasher.update(&nanos.to_be_bytes());
                }
                None => hasher.update(&[0]),
            }
        }
    }
    match &snapshot.symlink_target {
        Some(target) => {
            hasher.update(&[1]);
            recovery_commitment_update_bytes(
                hasher,
                &recovery_commitment_os_bytes(target.as_os_str()),
            );
        }
        None => hasher.update(&[0]),
    }
}

impl SourceManifest {
    /// Stable commitment used by crash-recovery journals. It binds the journal
    /// to the complete operation-time tree, including filesystem identities,
    /// descendant version tokens, symlink targets, and content digests when the
    /// manifest carries strong authority. The root
    /// ctime is deliberately excluded because the quarantine rename itself
    /// changes it on Unix; every other root attribute and identity remains
    /// committed.
    fn recovery_commitment(&self) -> ContentDigest {
        let mut hasher = Sha256::new();
        // Preserve the historical strong commitment byte-for-byte so v4
        // recovery journals remain valid. V5 journals bind verification
        // authority separately; standard file entries also carry explicit
        // digest-absence markers in this commitment.
        hasher.update(b"tonepoet-copy-undo-recovery-commitment-v1\0");
        hasher.update(&(self.entries.len() as u64).to_be_bytes());
        for (relative, proof) in &self.entries {
            recovery_commitment_update_bytes(
                &mut hasher,
                &recovery_commitment_os_bytes(relative.as_os_str()),
            );
            recovery_commitment_update_snapshot(
                &mut hasher,
                &proof.snapshot,
                !relative.as_os_str().is_empty(),
            );
            match proof.digest {
                Some(digest) => {
                    hasher.update(&[1]);
                    hasher.update(&digest.0);
                }
                None => hasher.update(&[0]),
            }
        }
        hasher.finalize()
    }
}

fn decode_content_digest(value: &str) -> Result<ContentDigest, String> {
    if value.len() != 64 {
        return Err("recovery proof digest must contain 64 hexadecimal digits".to_string());
    }
    let bytes = decode_hex(value)?;
    let digest: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "recovery proof digest has the wrong length".to_string())?;
    Ok(ContentDigest(digest))
}


const REMOVAL_JOURNAL_PREFIX: &str = ".tui-file-picker-copy-undo-";
const REMOVAL_JOURNAL_VERSION: &str = "tonepoet-copy-undo-v5";
const LEGACY_REMOVAL_JOURNAL_VERSION: &str = "tonepoet-copy-undo-v4";

fn active_removal_journals() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
#[derive(Clone)]
struct JournalPublicationTestHook {
    parent: PathBuf,
    callback: std::sync::Arc<dyn Fn() + Send + Sync>,
}

#[cfg(test)]
fn journal_publication_test_hook() -> &'static Mutex<Option<JournalPublicationTestHook>> {
    static HOOK: OnceLock<Mutex<Option<JournalPublicationTestHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn run_journal_publication_test_hook(parent: &Path) {
    let hook = journal_publication_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|hook| hook.parent == parent)
        .cloned();
    if let Some(hook) = hook {
        (hook.callback)();
    }
}

fn active_removal_journal_key(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    }
}

fn register_active_removal_journal(path: &Path) {
    active_removal_journals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(active_removal_journal_key(path));
}

fn unregister_active_removal_journal(path: &Path) {
    active_removal_journals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&active_removal_journal_key(path));
}

fn is_active_removal_journal(path: &Path) -> bool {
    active_removal_journals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&active_removal_journal_key(path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifiedRemovalPhase {
    Pending,
    Detached,
    Prepared,
    DeletionStarted,
}

impl VerifiedRemovalPhase {
    fn suffix(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Detached => "detached",
            Self::Prepared => "prepared",
            Self::DeletionStarted => "deletion-started",
        }
    }
}

#[derive(Debug, Clone)]
struct RemovalJournal {
    base: PathBuf,
    path: PathBuf,
    phase: VerifiedRemovalPhase,
}

impl RemovalJournal {
    fn path_for(base: &Path, phase: VerifiedRemovalPhase) -> PathBuf {
        let mut name = base.as_os_str().to_os_string();
        name.push(".");
        name.push(phase.suffix());
        PathBuf::from(name)
    }

    fn transition(&mut self, phase: VerifiedRemovalPhase) -> Result<(), String> {
        if self.phase == phase {
            return Ok(());
        }
        let next = Self::path_for(&self.base, phase);
        register_active_removal_journal(&next);
        if let Err(error) = rename_path_no_replace(&self.path, &next) {
            unregister_active_removal_journal(&next);
            return Err(format!(
                "advance copy-undo recovery journal {} -> {}: {error}",
                self.path.display(),
                next.display(),
            ));
        }
        unregister_active_removal_journal(&self.path);
        self.path = next;
        self.phase = phase;
        sync_journal_parent(&self.path)?;
        Ok(())
    }

    fn remove(self) -> Result<(), String> {
        let result = match fs::remove_file(&self.path) {
            Ok(()) => sync_journal_parent(&self.path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove copy-undo recovery journal {}: {error}",
                self.path.display(),
            )),
        };
        unregister_active_removal_journal(&self.path);
        result
    }

    fn deactivate(&self) {
        unregister_active_removal_journal(&self.path);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterruptedRemovalRecovery {
    pub restored: Vec<PathBuf>,
    pub cleaned_markers: Vec<PathBuf>,
    pub retained: Vec<(PathBuf, String)>,
    /// Journals that still belong to a live operation. These are not errors,
    /// but their directories must remain eligible for a later recovery scan.
    pub deferred: Vec<PathBuf>,
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("hex value has odd length".to_string());
    }
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = digit(pair[0]).ok_or_else(|| "invalid hex digit".to_string())?;
        let low = digit(pair[1]).ok_or_else(|| "invalid hex digit".to_string())?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn encode_os_component(value: &std::ffi::OsStr) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        return encode_hex(value.as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for word in value.encode_wide() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        return encode_hex(&bytes);
    }
    #[cfg(not(any(unix, windows)))]
    {
        encode_hex(value.to_string_lossy().as_bytes())
    }
}

fn decode_os_component(value: &str) -> Result<std::ffi::OsString, String> {
    let bytes = decode_hex(value)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        return Ok(std::ffi::OsString::from_vec(bytes));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        if bytes.len() % 2 != 0 {
            return Err("Windows component has an odd byte count".to_string());
        }
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return Ok(std::ffi::OsString::from_wide(&words));
    }
    #[cfg(not(any(unix, windows)))]
    {
        String::from_utf8(bytes)
            .map(std::ffi::OsString::from)
            .map_err(|error| format!("invalid UTF-8 component: {error}"))
    }
}

fn validate_single_component(value: &std::ffi::OsStr) -> Result<(), String> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(component)), None) if component == value => Ok(()),
        _ => Err("recovery journal contains a non-component pathname".to_string()),
    }
}

#[derive(Debug, Clone)]
struct RecoveryJournalRecord {
    owner_token: String,
    original_name: std::ffi::OsString,
    quarantine_name: std::ffi::OsString,
    verification: VerificationMode,
    commitment: ContentDigest,
}

fn linux_process_start_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let command_end = stat.rfind(')')?;
        let fields = stat.get(command_end + 2..)?.split_whitespace().collect::<Vec<_>>();
        // The first token after the command is field 3 (state); starttime is
        // field 22, therefore index 19 in this tail.
        return fields.get(19)?.parse().ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_process_start_token(pid: u32) -> Option<(u64, u64)> {
    use std::mem::MaybeUninit;

    const PROC_PIDTBSDINFO: i32 = 3;
    const MAXCOMLEN: usize = 16;
    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [libc::c_char; MAXCOMLEN],
        pbi_name: [libc::c_char; MAXCOMLEN * 2],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }
    #[link(name = "proc")]
    extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let mut info = MaybeUninit::<ProcBsdInfo>::uninit();
    let size = std::mem::size_of::<ProcBsdInfo>();
    let result = unsafe {
        proc_pidinfo(
            libc::c_int::try_from(pid).ok()?,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            libc::c_int::try_from(size).ok()?,
        )
    };
    if result != size as libc::c_int {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(windows)]
fn windows_process_start_token(pid: u32) -> Option<u64> {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut creation = FileTime { low: 0, high: 0 };
    let mut exit = FileTime { low: 0, high: 0 };
    let mut kernel = FileTime { low: 0, high: 0 };
    let mut user = FileTime { low: 0, high: 0 };
    let ok = unsafe {
        GetProcessTimes(
            handle,
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    unsafe {
        CloseHandle(handle);
    }
    (ok != 0).then_some(((creation.high as u64) << 32) | creation.low as u64)
}

fn process_owner_token(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let start = linux_process_start_ticks(pid)?;
        return Some(format!("linux:{}:{pid}:{start}", boot_id.trim()));
    }
    #[cfg(target_os = "macos")]
    {
        let (seconds, micros) = macos_process_start_token(pid)?;
        return Some(format!("macos:{pid}:{seconds}:{micros}"));
    }
    #[cfg(windows)]
    {
        let start = windows_process_start_token(pid)?;
        return Some(format!("windows:{pid}:{start}"));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pid;
        None
    }
}

fn current_process_owner_token() -> String {
    let pid = std::process::id();
    process_owner_token(pid).unwrap_or_else(|| format!("pid-only:{pid}"))
}

fn owner_token_pid(token: &str) -> Option<u32> {
    let mut fields = token.split(':');
    match fields.next()? {
        "linux" => {
            fields.next()?;
            fields.next()?.parse().ok()
        }
        "macos" | "windows" | "pid-only" => fields.next()?.parse().ok(),
        _ => None,
    }
}

fn owner_token_is_live(token: &str) -> bool {
    let Some(pid) = owner_token_pid(token) else {
        return false;
    };
    if token.starts_with("pid-only:") {
        // A platform that could not obtain a process-start token must remain
        // conservative. PID reuse may defer recovery, but must never let a
        // scanner race a demonstrably live operation.
        return process_is_alive_pid_only(pid);
    }
    process_owner_token(pid).is_some_and(|current| current == token)
}

fn legacy_recovery_journal_binding(
    owner_token: &str,
    original_name: &std::ffi::OsStr,
    quarantine_name: &std::ffi::OsStr,
    commitment: ContentDigest,
) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-copy-undo-journal-binding-v1\0");
    recovery_commitment_update_bytes(&mut hasher, owner_token.as_bytes());
    recovery_commitment_update_bytes(
        &mut hasher,
        &recovery_commitment_os_bytes(original_name),
    );
    recovery_commitment_update_bytes(
        &mut hasher,
        &recovery_commitment_os_bytes(quarantine_name),
    );
    hasher.update(&commitment.0);
    hasher.finalize()
}

fn recovery_journal_binding(
    owner_token: &str,
    original_name: &std::ffi::OsStr,
    quarantine_name: &std::ffi::OsStr,
    verification: VerificationMode,
    commitment: ContentDigest,
) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-copy-undo-journal-binding-v2\0");
    recovery_commitment_update_bytes(&mut hasher, owner_token.as_bytes());
    recovery_commitment_update_bytes(
        &mut hasher,
        &recovery_commitment_os_bytes(original_name),
    );
    recovery_commitment_update_bytes(
        &mut hasher,
        &recovery_commitment_os_bytes(quarantine_name),
    );
    hasher.update(&[match verification {
        VerificationMode::Standard => 0,
        VerificationMode::Strong => 1,
    }]);
    hasher.update(&commitment.0);
    hasher.finalize()
}

fn journal_payload(
    original_name: &std::ffi::OsStr,
    quarantine_name: &std::ffi::OsStr,
    verification: VerificationMode,
    commitment: ContentDigest,
) -> String {
    let owner_token = current_process_owner_token();
    let binding = recovery_journal_binding(
        &owner_token,
        original_name,
        quarantine_name,
        verification,
        commitment,
    );
    let verification = match verification {
        VerificationMode::Standard => "standard",
        VerificationMode::Strong => "strong",
    };
    format!(
        "{REMOVAL_JOURNAL_VERSION}\nowner={}\noriginal={}\nquarantine={}\nverification={verification}\ncommitment={}\nbinding={}\n",
        encode_hex(owner_token.as_bytes()),
        encode_os_component(original_name),
        encode_os_component(quarantine_name),
        commitment.to_hex(),
        binding.to_hex(),
    )
}

fn parse_journal_payload(bytes: &[u8]) -> Result<RecoveryJournalRecord, String> {
    if bytes.len() > 64 * 1024 {
        return Err("recovery journal exceeds the safety limit".to_string());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("recovery journal is not UTF-8: {error}"))?;
    let mut lines = text.lines();
    let version = lines
        .next()
        .ok_or_else(|| "recovery journal is empty".to_string())?;
    let legacy = match version {
        REMOVAL_JOURNAL_VERSION => false,
        LEGACY_REMOVAL_JOURNAL_VERSION => true,
        _ => return Err("unsupported copy-undo recovery journal version".to_string()),
    };
    let owner = lines
        .next()
        .and_then(|line| line.strip_prefix("owner="))
        .ok_or_else(|| "recovery journal is missing owner identity".to_string())?;
    let owner_token = String::from_utf8(decode_hex(owner)?)
        .map_err(|error| format!("recovery journal owner identity is invalid UTF-8: {error}"))?;
    if owner_token_pid(&owner_token).is_none() {
        return Err("recovery journal owner identity is malformed".to_string());
    }
    let original = lines
        .next()
        .and_then(|line| line.strip_prefix("original="))
        .ok_or_else(|| "recovery journal is missing original component".to_string())?;
    let quarantine = lines
        .next()
        .and_then(|line| line.strip_prefix("quarantine="))
        .ok_or_else(|| "recovery journal is missing quarantine component".to_string())?;
    let verification = if legacy {
        VerificationMode::Strong
    } else {
        match lines
            .next()
            .and_then(|line| line.strip_prefix("verification="))
            .ok_or_else(|| "recovery journal is missing verification authority".to_string())?
        {
            "standard" => VerificationMode::Standard,
            "strong" => VerificationMode::Strong,
            _ => return Err("recovery journal has an invalid verification authority".to_string()),
        }
    };
    let commitment = lines
        .next()
        .and_then(|line| line.strip_prefix("commitment="))
        .ok_or_else(|| "recovery journal is missing operation-time proof".to_string())?;
    let binding = lines
        .next()
        .and_then(|line| line.strip_prefix("binding="))
        .ok_or_else(|| "recovery journal is missing its authority binding".to_string())?;
    if lines.next().is_some() {
        return Err("recovery journal has unexpected trailing fields".to_string());
    }
    let original_name = decode_os_component(original)?;
    let quarantine_name = decode_os_component(quarantine)?;
    validate_single_component(&original_name)?;
    validate_single_component(&quarantine_name)?;
    let commitment = decode_content_digest(commitment)?;
    let actual_binding = decode_content_digest(binding)?;
    let expected_binding = if legacy {
        legacy_recovery_journal_binding(
            &owner_token,
            &original_name,
            &quarantine_name,
            commitment,
        )
    } else {
        recovery_journal_binding(
            &owner_token,
            &original_name,
            &quarantine_name,
            verification,
            commitment,
        )
    };
    if actual_binding != expected_binding {
        return Err(
            "recovery journal authority binding does not match its owner, names, verification level, and operation-time proof"
                .to_string(),
        );
    }
    Ok(RecoveryJournalRecord {
        owner_token,
        original_name,
        quarantine_name,
        verification,
        commitment,
    })
}

fn process_is_alive_pid_only(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return true;
        };
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 {
            return true;
        }
        return matches!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
    }
    #[cfg(windows)]
    {
        windows_process_start_token(pid).is_some()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}
fn read_regular_recovery_journal(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|error| format!("open recovery journal without following links: {error}"))?
    };
    #[cfg(windows)]
    let mut file = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|error| format!("open recovery journal without following links: {error}"))?
    };
    #[cfg(not(any(unix, windows)))]
    let mut file = File::open(path)
        .map_err(|error| format!("open recovery journal: {error}"))?;

    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect opened recovery journal: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("recovery marker is not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("recovery journal is not owned by the current user".to_string());
        }
        if metadata.mode() & 0o077 != 0 {
            return Err("recovery journal permissions are broader than owner-only".to_string());
        }
        if metadata.nlink() != 1 {
            return Err("recovery journal has unexpected hard links".to_string());
        }
    }

    let mut payload = Vec::new();
    file.by_ref()
        .take(64 * 1024 + 1)
        .read_to_end(&mut payload)
        .map_err(|error| format!("read recovery journal: {error}"))?;
    if payload.len() > 64 * 1024 {
        return Err("recovery journal exceeds the safety limit".to_string());
    }
    Ok(payload)
}

fn sync_journal_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    {
        return File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("synchronize recovery directory {}: {error}", parent.display()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let result = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)
            .and_then(|directory| directory.sync_all());
        return match result {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied
                        | io::ErrorKind::InvalidInput
                        | io::ErrorKind::Unsupported
                ) => Ok(()),
            Err(error) => Err(format!(
                "synchronize recovery directory {}: {error}",
                parent.display(),
            )),
        };
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Ok(())
    }
}

fn create_removal_journal(
    parent: &Path,
    nonce: u128,
    original_name: &std::ffi::OsStr,
    quarantine_name: &std::ffi::OsStr,
    verification: VerificationMode,
    commitment: ContentDigest,
) -> io::Result<RemovalJournal> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let base = parent.join(format!("{REMOVAL_JOURNAL_PREFIX}{nonce:032x}"));
    let path = RemovalJournal::path_for(&base, VerifiedRemovalPhase::Pending);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    // Hold the active-registry lock across journal publication. Recovery
    // scanners consult the same lock after discovering a marker, so there is
    // no interval in which a visible live journal can be mistaken for an
    // abandoned operation owned by this process.
    let key = active_removal_journal_key(&path);
    let mut active = active_removal_journals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut file = options.open(&path)?;
    if let Err(error) = file
        .write_all(journal_payload(original_name, quarantine_name, verification, commitment).as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    drop(file);
    if let Err(error) = sync_journal_parent(&path) {
        let _ = fs::remove_file(&path);
        return Err(io::Error::new(io::ErrorKind::Other, error));
    }
    #[cfg(test)]
    run_journal_publication_test_hook(parent);
    active.insert(key);
    drop(active);
    Ok(RemovalJournal {
        base,
        path,
        phase: VerifiedRemovalPhase::Pending,
    })
}

fn journal_identity_from_path(
    path: &Path,
) -> Option<(VerifiedRemovalPhase, std::ffi::OsString)> {
    let name = path.file_name()?.to_str()?;
    let (phase, suffix) = if name.ends_with(".pending") {
        (VerifiedRemovalPhase::Pending, ".pending")
    } else if name.ends_with(".detached") {
        (VerifiedRemovalPhase::Detached, ".detached")
    } else if name.ends_with(".prepared") {
        (VerifiedRemovalPhase::Prepared, ".prepared")
    } else if name.ends_with(".deletion-started") {
        (
            VerifiedRemovalPhase::DeletionStarted,
            ".deletion-started",
        )
    } else {
        return None;
    };
    let nonce = name
        .strip_prefix(REMOVAL_JOURNAL_PREFIX)?
        .strip_suffix(suffix)?;
    if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        phase,
        std::ffi::OsString::from(format!(
            ".tui-file-picker-undo-quarantine-{nonce}"
        )),
    ))
}

fn verify_recovery_commitment_at(
    path: &Path,
    verification: VerificationMode,
    expected: ContentDigest,
) -> Result<(), String> {
    let current = capture_manifest_with_mode(path, verification)
        .map_err(|error| format!("capture quarantined recovery proof: {error}"))?;
    let actual = current.recovery_commitment();
    if actual != expected {
        return Err(format!(
            "quarantined object does not match the operation-time recovery commitment (expected {}, got {})",
            expected.to_hex(),
            actual.to_hex(),
        ));
    }
    Ok(())
}

/// Recover copy-undo detaches only when the detached object still satisfies the
/// operation-time tree commitment stored in the owner-only journal and the
/// journal's owner/name/proof binding validates. A `deletion-started` tree is
/// restored when the complete commitment still matches, proving that no
/// destructive entry removal occurred; a partial or replaced tree is retained
/// for explicit inspection. As with all same-account filesystem recovery, a
/// deliberately malicious process running under the identical OS security
/// principal is outside the authority boundary; accidental mutation, PID
/// reuse, stale owners, and independently replaced objects fail closed.
pub fn recover_interrupted_verified_removals(
    directory: &Path,
) -> Result<InterruptedRemovalRecovery, String> {
    recover_interrupted_verified_removals_internal(directory, false)
}

fn recover_interrupted_verified_removals_internal(
    directory: &Path,
    ignore_live_owner: bool,
) -> Result<InterruptedRemovalRecovery, String> {
    let mut report = InterruptedRemovalRecovery::default();
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "scan copy-undo recovery directory {}: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "read copy-undo recovery entry in {}: {error}",
                directory.display()
            )
        })?;
        let journal_path = entry.path();
        let Some((phase, expected_quarantine_name)) =
            journal_identity_from_path(&journal_path)
        else {
            continue;
        };
        if is_active_removal_journal(&journal_path) {
            report.deferred.push(journal_path);
            continue;
        }
        let payload = match read_regular_recovery_journal(&journal_path) {
            Ok(payload) => payload,
            Err(error) => {
                report.retained.push((journal_path.clone(), error));
                continue;
            }
        };
        let record = match parse_journal_payload(&payload) {
            Ok(parsed) => parsed,
            Err(error) => {
                report.retained.push((journal_path.clone(), error));
                continue;
            }
        };
        if !ignore_live_owner && owner_token_is_live(&record.owner_token) {
            report.deferred.push(journal_path);
            continue;
        }
        if record.quarantine_name != expected_quarantine_name
            || record.original_name == record.quarantine_name
        {
            report.retained.push((
                journal_path.clone(),
                "recovery journal identity does not match its nonce-bound quarantine name"
                    .to_string(),
            ));
            continue;
        }
        let original = directory.join(&record.original_name);
        let quarantine = directory.join(&record.quarantine_name);
        let original_exists = fs::symlink_metadata(&original).is_ok();
        let quarantine_exists = fs::symlink_metadata(&quarantine).is_ok();

        match (original_exists, quarantine_exists) {
            (false, true) => {
                if let Err(error) =
                    verify_recovery_commitment_at(
                        &quarantine,
                        record.verification,
                        record.commitment,
                    )
                {
                    let state = match phase {
                        VerifiedRemovalPhase::DeletionStarted => {
                            "destructive cleanup may already be partial"
                        }
                        _ => "the detached object was replaced or changed",
                    };
                    report.retained.push((
                        quarantine,
                        format!(
                            "interrupted copy-undo state was not restored because {state}: {error}"
                        ),
                    ));
                    continue;
                }
                rename_path_no_replace(&quarantine, &original).map_err(|error| {
                    format!(
                        "restore interrupted copy-undo detach {} -> {}: {error}",
                        quarantine.display(),
                        original.display(),
                    )
                })?;
                fs::remove_file(&journal_path).map_err(|error| {
                    format!(
                        "remove restored recovery journal {}: {error}",
                        journal_path.display()
                    )
                })?;
                sync_journal_parent(&journal_path)?;
                report.restored.push(original);
            }
            (true, false) => {
                fs::remove_file(&journal_path).map_err(|error| {
                    format!(
                        "remove completed recovery journal {}: {error}",
                        journal_path.display()
                    )
                })?;
                sync_journal_parent(&journal_path)?;
                report.cleaned_markers.push(journal_path);
            }
            (false, false) => {
                fs::remove_file(&journal_path).map_err(|error| {
                    format!(
                        "remove orphaned recovery journal {}: {error}",
                        journal_path.display()
                    )
                })?;
                sync_journal_parent(&journal_path)?;
                report.cleaned_markers.push(journal_path);
            }
            (true, true) => report.retained.push((
                quarantine,
                format!(
                    "interrupted copy-undo detach cannot be restored because original pathname {} is occupied",
                    original.display(),
                ),
            )),
        }
    }
    Ok(report)
}
/// Scan a directory during each filesystem refresh. Recovery markers can be
/// created by this process or another process after an earlier clean scan, so
/// caching a directory as permanently recovered would strand later crashes.
/// The picker never invokes this from cursor movement; it runs only where the
/// directory is already being refreshed from disk.
pub fn recover_interrupted_verified_removals_once(
    directory: &Path,
) -> Result<InterruptedRemovalRecovery, String> {
    recover_interrupted_verified_removals(directory)
}

/// A verified root atomically moved out of its public pathname to a unique,
/// same-directory quarantine name. No object is deleted during preparation.
/// The caller may prepare every root first, restore all guards on any preflight
/// failure, and only then commit destructive removal.
#[derive(Debug)]
pub struct VerifiedRemoval {
    original: PathBuf,
    quarantine_root: PathBuf,
    quarantine_name: std::ffi::OsString,
    cleanup_manifest: SourceManifest,
    journal: Option<RemovalJournal>,
    armed: bool,
    #[cfg(unix)]
    original_parent_handle: File,
    #[cfg(unix)]
    original_name: std::ffi::OsString,
}

impl VerifiedRemoval {
    pub fn original(&self) -> &Path {
        &self.original
    }

    pub fn quarantine_root(&self) -> &Path {
        &self.quarantine_root
    }

    /// Restore the verified object to its original pathname without replacing
    /// anything that appeared there after quarantine. On conflict, the object
    /// remains intact under its quarantine name and the diagnostic names it.
    pub fn restore(mut self) -> Result<(), String> {
        self.restore_impl(true)?;
        self.finish_journal_after_namespace_commit("restore");
        Ok(())
    }

    /// Restore the exact namespace entry captured by the atomic quarantine
    /// rename before it has passed operation-time proof. This is used only by
    /// preparation's immediate mismatch rollback; verified guards always use
    /// `restore`, which first proves that the quarantined tree is unchanged.
    fn restore_unverified_capture(mut self) -> Result<(), String> {
        self.restore_impl(false)?;
        self.finish_journal_after_namespace_commit("rollback");
        Ok(())
    }

    fn quarantine_matches_proof(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            let retained_root = capability_child_path(
                &self.original_parent_handle,
                &self.quarantine_name,
            )
            .map_err(|error| format!("resolve quarantined object before verification: {error}"))?;
            return self
                .cleanup_manifest
                .verify_cleanup_tree_at(&retained_root)
                .map(|_| ())
                .map_err(|error| {
                    format!(
                        "quarantined object at {} no longer matches operation-time proof: {error}",
                        self.quarantine_root.display(),
                    )
                });
        }

        #[cfg(windows)]
        {
            return self
                .cleanup_manifest
                .verify_cleanup_tree_at(&self.quarantine_root)
                .map_err(|error| {
                    format!(
                        "quarantined object at {} no longer matches operation-time proof: {error}",
                        self.quarantine_root.display(),
                    )
                });
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err("identity-bound quarantine verification is unavailable on this platform".to_string())
        }
    }

    fn restore_impl(&self, require_proof: bool) -> Result<(), String> {
        if require_proof {
            self.quarantine_matches_proof().map_err(|error| {
                format!("refusing to restore changed quarantined object: {error}")
            })?;
        }

        #[cfg(unix)]
        {
            rename_between_open_directories_no_replace(
                &self.original_parent_handle,
                &self.quarantine_name,
                &self.original_parent_handle,
                &self.original_name,
            )
            .map_err(|error| {
                format!(
                    "restore verified object {} -> {}: {error}",
                    self.quarantine_root.display(),
                    self.original.display(),
                )
            })?;
            return Ok(());
        }

        #[cfg(windows)]
        {
            windows_move_no_replace(&self.quarantine_root, &self.original).map_err(|error| {
                format!(
                    "restore verified object {} -> {}: {error}",
                    self.quarantine_root.display(),
                    self.original.display(),
                )
            })?;
            return Ok(());
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err(format!(
                "identity-bound restore is unavailable on this platform; verified data remains intact at {}",
                self.quarantine_root.display(),
            ))
        }
    }

    /// Complete every non-destructive authority, permission, membership, and
    /// object-proof check without retaining one handle per manifest entry.
    /// The returned guard owns the detached namespace object but performs no
    /// deletion until `PreparedVerifiedRemoval::commit` is called. Hosts can
    /// therefore prepare every root in a multi-root undo before committing the
    /// first destructive operation.
    pub fn prepare_for_commit(mut self) -> Result<PreparedVerifiedRemoval, String> {
        #[cfg(unix)]
        preflight_verified_removal_unix(&mut self)?;

        #[cfg(windows)]
        preflight_verified_removal_windows(&mut self)?;

        #[cfg(not(any(unix, windows)))]
        return Err(format!(
            "identity-bound copy-undo cleanup is unavailable on this platform; verified data remains intact at {}",
            self.quarantine_root.display(),
        ));

        self.journal_mut()?
            .transition(VerifiedRemovalPhase::Prepared)?;
        Ok(PreparedVerifiedRemoval {
            removal: Some(self),
        })
    }

    /// Convenience single-root path. Multi-root callers must use
    /// `prepare_for_commit` for every root first so predictable failures cannot
    /// partially undo a logical operation.
    pub fn commit(self) -> Result<(), String> {
        self.prepare_for_commit()?.commit()
    }

    fn journal_mut(&mut self) -> Result<&mut RemovalJournal, String> {
        self.journal.as_mut().ok_or_else(|| {
            format!(
                "copy-undo recovery journal is unavailable for {}",
                self.original.display(),
            )
        })
    }

    fn finish_journal_after_namespace_commit(&mut self, action: &str) {
        // Once the namespace action has committed, marker cleanup is
        // maintenance, not transaction outcome. Reporting a failed marker
        // fsync/remove as a failed undo would invite a destructive retry
        // against an already-restored or already-deleted root. Leave any
        // surviving marker for the recovery scanner and report it loudly.
        self.armed = false;
        if let Some(journal) = self.journal.take() {
            if let Err(error) = journal.remove() {
                log::warn!(
                    "copy-undo {action} committed for {}, but recovery-journal cleanup failed: {error}",
                    self.original.display(),
                );
            }
        }
    }
}


/// A detached root whose complete non-destructive cleanup preflight succeeded.
/// It retains only the root guard and manifest; no per-entry descriptor or
/// handle vector is held between preparation and commit.
#[derive(Debug)]
pub struct PreparedVerifiedRemoval {
    removal: Option<VerifiedRemoval>,
}

impl PreparedVerifiedRemoval {
    pub fn original(&self) -> &Path {
        self.removal
            .as_ref()
            .expect("prepared removal always owns its guard")
            .original()
    }

    pub fn restore(mut self) -> Result<(), String> {
        self.removal
            .take()
            .expect("prepared removal always owns its guard")
            .restore()
    }

    pub fn commit(mut self) -> Result<(), String> {
        let mut removal = self
            .removal
            .take()
            .expect("prepared removal always owns its guard");

        #[cfg(unix)]
        {
            commit_prepared_verified_removal_unix(&mut removal)?;
            removal.finish_journal_after_namespace_commit("cleanup");
            return Ok(());
        }

        #[cfg(windows)]
        {
            commit_prepared_verified_removal_windows(&mut removal)?;
            removal.finish_journal_after_namespace_commit("cleanup");
            return Ok(());
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err(format!(
                "identity-bound copy-undo cleanup is unavailable on this platform; verified data remains intact at {}",
                removal.quarantine_root.display(),
            ))
        }
    }
}


#[cfg(unix)]
fn component_cstring_for_capability(component: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(component.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("filesystem component contains NUL: {:?}", component),
        )
    })
}

#[cfg(unix)]
fn open_directory_capability(path: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("directory path contains NUL: {}", path.display()),
        )
    })?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn verify_unix_parent_mode_contract(
    owner_uid: u32,
    mode: u32,
    effective_uid: u32,
    displayed_path: &Path,
) -> Result<(), String> {
    if owner_uid != effective_uid {
        return Err(format!(
            "copy undo requires every naming directory to be owned by the current user; refusing {} owned by uid {}",
            displayed_path.display(),
            owner_uid,
        ));
    }
    if mode & 0o300 != 0o300 {
        return Err(format!(
            "copy undo requires owner write and execute permission on naming directory {}; mode is {:04o}",
            displayed_path.display(),
            mode & 0o7777,
        ));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "copy undo refuses group- or other-writable naming directory {} with mode {:04o}; another identity could add or replace namespace entries during cleanup",
            displayed_path.display(),
            mode & 0o7777,
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_unix_quarantine_parent_contract(
    directory: &File,
    displayed_path: &Path,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory.metadata().map_err(|error| {
        format!(
            "inspect undo-quarantine parent {}: {error}",
            displayed_path.display(),
        )
    })?;
    verify_unix_parent_mode_contract(
        metadata.uid(),
        metadata.mode(),
        unsafe { libc::geteuid() },
        displayed_path,
    )?;
    unix_reject_extended_acl(directory, displayed_path, SourceKind::Directory)?;
    unix_reject_immutable_state(directory, displayed_path, SourceKind::Directory)
}

#[cfg(unix)]
fn capability_child_path(directory: &File, child: &std::ffi::OsStr) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    {
        return Ok(PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(child));
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        const F_GETPATH: libc::c_int = 50;
        const MAXPATHLEN: usize = 1024;
        let mut buffer = vec![0u8; MAXPATHLEN];
        let result = unsafe {
            libc::fcntl(
                directory.as_raw_fd(),
                F_GETPATH,
                buffer.as_mut_ptr().cast::<libc::c_char>(),
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        let length = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
        buffer.truncate(length);
        return Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)).join(child));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (directory, child);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory capability path resolution is unavailable",
        ))
    }
}


#[cfg(unix)]
fn rename_between_open_directories_no_replace(
    source_parent: &File,
    source_name: &std::ffi::OsStr,
    destination_parent: &File,
    destination_name: &std::ffi::OsStr,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let source_name = component_cstring_for_capability(source_name)?;
    let destination_name = component_cstring_for_capability(destination_name)?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            source_parent.as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    extern "C" {
        fn renameatx_np(
            fromfd: libc::c_int,
            from: *const libc::c_char,
            tofd: libc::c_int,
            to: *const libc::c_char,
            flags: libc::c_uint,
        ) -> libc::c_int;
    }
    #[cfg(target_os = "macos")]
    let result = unsafe {
        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        renameatx_np(
            source_parent.as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.as_raw_fd(),
            destination_name.as_ptr(),
            RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = -1;
    if result == 0 {
        Ok(())
    } else {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this Unix platform",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EEXIST) => Err(io::Error::new(io::ErrorKind::AlreadyExists, error)),
                Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => Err(
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "atomic no-replace rename is unavailable on this kernel or filesystem",
                    ),
                ),
                _ => Err(error),
            };
        }
    }
}


/// Atomically rename one pathname to another without replacing an existing
/// destination on Linux, macOS, and Windows. Unsupported targets return
/// `ErrorKind::Unsupported`; callers must not silently degrade to a racy
/// existence check when no-clobber is a correctness invariant.
pub fn rename_path_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let source_name = source.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("rename source has no basename: {}", source.display()),
            )
        })?;
        let destination_name = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("rename destination has no basename: {}", destination.display()),
            )
        })?;
        let source_parent = source
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let destination_parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let source_parent = open_directory_capability(source_parent)?;
        let destination_parent = open_directory_capability(destination_parent)?;
        return rename_between_open_directories_no_replace(
            &source_parent,
            source_name,
            &destination_parent,
            destination_name,
        );
    }

    #[cfg(windows)]
    {
        return windows_move_no_replace(source, destination);
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
fn unix_component_cstring(component: &std::ffi::OsStr) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(component.as_bytes()).map_err(|_| {
        format!(
            "filesystem component contains NUL and cannot be removed safely: {:?}",
            component,
        )
    })
}

#[cfg(unix)]
fn unix_open_component(
    parent: &File,
    name: &std::ffi::OsStr,
    expected_kind: SourceKind,
) -> Result<File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let name = unix_component_cstring(name)?;
    let flags = match expected_kind {
        SourceKind::Directory => {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        }
        SourceKind::File => libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        SourceKind::Symlink => {
            #[cfg(target_os = "linux")]
            {
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
            }
            #[cfg(target_os = "macos")]
            {
                libc::O_RDONLY | libc::O_SYMLINK | libc::O_CLOEXEC
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            }
        }
    };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(format!(
            "open quarantined component {:?}: {}",
            std::ffi::OsStr::from_bytes(name.as_bytes()),
            io::Error::last_os_error(),
        ));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect opened quarantined component: {error}"))?;
    let actual_kind = kind_from_metadata(&metadata)
        .map_err(|error| format!("classify opened quarantined component: {error}"))?;
    if actual_kind != expected_kind {
        return Err(format!(
            "quarantined component kind changed from {expected_kind:?} to {actual_kind:?}",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn unix_readlink_component(
    parent: &File,
    name: &std::ffi::OsStr,
) -> Result<PathBuf, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let name = unix_component_cstring(name)?;
    let mut capacity = 256usize;
    loop {
        if capacity > 1024 * 1024 {
            return Err("quarantined symlink target exceeds the safety limit".to_string());
        }
        let mut bytes = vec![0u8; capacity];
        let length = unsafe {
            libc::readlinkat(
                parent.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length < 0 {
            return Err(format!(
                "read quarantined symlink {:?}: {}",
                std::ffi::OsStr::from_bytes(name.as_bytes()),
                io::Error::last_os_error(),
            ));
        }
        let length = length as usize;
        if length < bytes.len() {
            bytes.truncate(length);
            return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)));
        }
        capacity = capacity.saturating_mul(2);
    }
}

#[cfg(unix)]
fn unix_snapshot_open_component(
    file: &File,
    parent: &File,
    name: &std::ffi::OsStr,
    kind: SourceKind,
) -> Result<SourceSnapshot, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect opened quarantined component: {error}"))?;
    let actual_kind = kind_from_metadata(&metadata)
        .map_err(|error| format!("classify opened quarantined component: {error}"))?;
    if actual_kind != kind {
        return Err(format!(
            "quarantined component kind changed from {kind:?} to {actual_kind:?}",
        ));
    }
    Ok(SourceSnapshot {
        kind,
        identity: file_identity(file, &metadata)
            .map_err(|error| format!("identify opened quarantined component: {error}"))?,
        version: version_from_metadata(&metadata),
        symlink_target: if kind == SourceKind::Symlink {
            Some(unix_readlink_component(parent, name)?)
        } else {
            None
        },
    })
}

#[cfg(unix)]
fn unix_open_parent_for_relative(
    quarantine_parent: &File,
    quarantine_name: &std::ffi::OsStr,
    relative: &Path,
    manifest: &SourceManifest,
    capabilities: FilesystemCapabilities,
    allow_directory_metadata_changes: bool,
) -> Result<(File, std::ffi::OsString), String> {
    let mut parent = quarantine_parent
        .try_clone()
        .map_err(|error| format!("clone quarantine parent capability: {error}"))?;
    if relative.as_os_str().is_empty() {
        return Ok((parent, quarantine_name.to_os_string()));
    }

    parent = unix_open_component(&parent, quarantine_name, SourceKind::Directory)?;
    let root_expected = manifest
        .expected_snapshot(Path::new(""))
        .ok_or_else(|| "cleanup manifest has no root directory proof".to_string())?;
    if root_expected.kind() != SourceKind::Directory {
        return Err("cleanup manifest root is not a directory".to_string());
    }
    let root_current = snapshot_open_handle(&parent)
        .map_err(|error| format!("identify opened quarantine root: {error}"))?;
    let root_comparison = if allow_directory_metadata_changes {
        root_expected.verify_same_identity(&root_current)
    } else {
        root_expected.verify_same_object_after_rename_with_capabilities(
            &root_current,
            capabilities,
        )
    };
    root_comparison
        .map_err(|error| format!("quarantine root changed during traversal: {error}"))?;

    if let Some(relative_parent) = relative.parent() {
        let mut walked = PathBuf::new();
        for component in relative_parent.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(format!(
                    "refusing non-normal manifest path during cleanup: {}",
                    relative.display(),
                ));
            };
            walked.push(name);
            let expected = manifest
                .expected_snapshot(&walked)
                .ok_or_else(|| format!("missing directory proof for {}", walked.display()))?;
            if expected.kind() != SourceKind::Directory {
                return Err(format!(
                    "manifest ancestor is not a directory: {}",
                    walked.display(),
                ));
            }
            let next = unix_open_component(&parent, name, SourceKind::Directory)?;
            let current = snapshot_open_handle(&next).map_err(|error| {
                format!("identify opened manifest ancestor {}: {error}", walked.display())
            })?;
            let comparison = if allow_directory_metadata_changes {
                expected.verify_same_identity(&current)
            } else if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
                expected.verify_same_object_and_version(&current)
            } else {
                expected.verify_same_identity_with_policy(
                    &current,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
            };
            comparison.map_err(|error| {
                format!("manifest ancestor {} changed: {error}", walked.display())
            })?;
            parent = next;
        }
    }
    let name = relative.file_name().ok_or_else(|| {
        format!("manifest entry has no basename: {}", relative.display())
    })?;
    Ok((parent, name.to_os_string()))
}

#[cfg(unix)]
fn unix_verify_opened_entry(
    proof: &SourceEntryProof,
    relative: &Path,
    parent: &File,
    name: &std::ffi::OsStr,
    mut opened: File,
    verification: VerificationMode,
    capabilities: FilesystemCapabilities,
) -> Result<SourceSnapshot, String> {
    let moved_root = relative.as_os_str().is_empty();
    let before = unix_snapshot_open_component(
        &opened,
        parent,
        name,
        proof.snapshot.kind(),
    )?;
    match proof.snapshot.kind() {
        SourceKind::File => {
            let strict_descendant = capabilities.identity_policy()
                == FilesystemIdentityPolicy::Strict
                && !moved_root;
            if strict_descendant {
                proof
                    .snapshot
                    .verify_same_object_and_version(&before)
                    .map_err(|error| format!("opened file changed before cleanup: {error}"))?;
            } else {
                proof
                    .snapshot
                    .verify_same_object_after_rename_with_capabilities(&before, capabilities)
                    .map_err(|error| format!("opened file changed before cleanup: {error}"))?;
                if verification == VerificationMode::Standard {
                    return Ok(before);
                }
                let digest = digest_open_file(&mut opened)
                    .map_err(|error| format!("digest opened quarantined file: {error}"))?;
                let after = unix_snapshot_open_component(
                    &opened,
                    parent,
                    name,
                    SourceKind::File,
                )?;
                before
                    .verify_same_object_and_version(&after)
                    .map_err(|error| format!("opened file changed while verified: {error}"))?;
                if proof.digest != Some(digest) {
                    return Err("opened quarantined file digest changed".to_string());
                }
                return Ok(after);
            }
        }
        SourceKind::Directory | SourceKind::Symlink => {
            let comparison = if capabilities.identity_policy()
                == FilesystemIdentityPolicy::Strict
            {
                if moved_root {
                    proof
                        .snapshot
                        .verify_same_object_after_rename_with_capabilities(&before, capabilities)
                } else {
                    proof.snapshot.verify_same_object_and_version(&before)
                }
            } else {
                proof.snapshot.verify_same_identity_with_policy(
                    &before,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
            };
            comparison.map_err(|error| {
                format!("opened quarantined component changed before cleanup: {error}")
            })?;
        }
    }
    Ok(before)
}

#[cfg(unix)]
fn unix_reject_extended_acl(
    file: &File,
    displayed_path: &Path,
    kind: SourceKind,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    if kind == SourceKind::Symlink {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        for name in [
            b"system.posix_acl_access\0".as_slice(),
            b"system.posix_acl_default\0".as_slice(),
        ] {
            let result = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    name.as_ptr().cast(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if result >= 0 {
                return Err(format!(
                    "copy undo refuses extended POSIX ACL authority at {}",
                    displayed_path.display(),
                ));
            }
            let error = io::Error::last_os_error();
            let code = error.raw_os_error();
            if code != Some(libc::ENODATA)
                && code != Some(libc::ENOTSUP)
                && code != Some(libc::EOPNOTSUPP)
            {
                return Err(format!(
                    "inspect POSIX ACL authority at {}: {error}",
                    displayed_path.display(),
                ));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::ffi::c_void;
        type Acl = *mut c_void;
        const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
        const ACL_FIRST_ENTRY: libc::c_int = 0;
        #[link(name = "System")]
        extern "C" {
            fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
            fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut *mut c_void) -> libc::c_int;
            fn acl_free(object: *mut c_void) -> libc::c_int;
        }

        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            return Err(format!(
                "inspect macOS ACL authority at {}: {}",
                displayed_path.display(),
                io::Error::last_os_error(),
            ));
        }
        let mut entry = std::ptr::null_mut();
        let status = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
        let entry_error = if status == 0 {
            None
        } else {
            Some(io::Error::last_os_error())
        };
        let _ = unsafe { acl_free(acl) };
        if status == 0 {
            return Err(format!(
                "copy undo refuses extended macOS ACL authority at {}",
                displayed_path.display(),
            ));
        }
        let error = entry_error.expect("failed ACL lookup records its error");
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(format!(
                "inspect macOS ACL entries at {}: {error}",
                displayed_path.display(),
            ));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn unix_reject_immutable_state(
    file: &File,
    displayed_path: &Path,
    kind: SourceKind,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    #[cfg(target_os = "linux")]
    if kind == SourceKind::Symlink {
        // Linux does not expose inode flags for an O_PATH symlink descriptor;
        // unlink authority is governed by the verified parent directory.
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
        const FS_IMMUTABLE_FL: libc::c_long = 0x0000_0010;
        const FS_APPEND_FL: libc::c_long = 0x0000_0020;
        let mut flags: libc::c_long = 0;
        let result = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) };
        if result == 0 {
            if flags & (FS_IMMUTABLE_FL | FS_APPEND_FL) != 0 {
                return Err(format!(
                    "copy undo refuses immutable or append-only object {}",
                    displayed_path.display(),
                ));
            }
        } else {
            let error = io::Error::last_os_error();
            let code = error.raw_os_error();
            if code != Some(libc::ENOTTY)
                && code != Some(libc::EOPNOTSUPP)
                && code != Some(libc::ENOSYS)
            {
                return Err(format!(
                    "inspect immutable state at {}: {error}",
                    displayed_path.display(),
                ));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const UF_IMMUTABLE: u32 = 0x0000_0002;
        const UF_APPEND: u32 = 0x0000_0004;
        const SF_IMMUTABLE: u32 = 0x0002_0000;
        const SF_APPEND: u32 = 0x0004_0000;
        let flags = file
            .metadata()
            .map_err(|error| format!("inspect file flags at {}: {error}", displayed_path.display()))?
            .st_flags();
        if flags & (UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_APPEND) != 0 {
            return Err(format!(
                "copy undo refuses immutable or append-only object {}",
                displayed_path.display(),
            ));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn verify_unix_entry_contract(
    opened: &File,
    displayed_path: &Path,
    kind: SourceKind,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = opened.metadata().map_err(|error| {
        format!(
            "inspect copy-undo authority at {}: {error}",
            displayed_path.display(),
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(format!(
            "copy undo refuses an entry not owned by the current user: {}",
            displayed_path.display(),
        ));
    }

    unix_reject_extended_acl(opened, displayed_path, kind)?;
    unix_reject_immutable_state(opened, displayed_path, kind)?;

    let mode = metadata.mode();
    match kind {
        SourceKind::File => {
            if mode & 0o400 == 0 {
                return Err(format!(
                    "copy undo cannot verify unreadable preserved file {}; owner-read permission is required",
                    displayed_path.display(),
                ));
            }
            if mode & 0o022 != 0 {
                return Err(format!(
                    "copy undo refuses group- or other-writable file {} with mode {:04o}",
                    displayed_path.display(),
                    mode & 0o7777,
                ));
            }
        }
        SourceKind::Directory => {
            if mode & 0o700 != 0o700 {
                return Err(format!(
                    "copy undo cannot safely traverse and remove preserved directory {}; owner read/write/execute permission is required (mode {:04o})",
                    displayed_path.display(),
                    mode & 0o7777,
                ));
            }
            if mode & 0o022 != 0 {
                return Err(format!(
                    "copy undo refuses group- or other-writable directory {} with mode {:04o}; another identity could add namespace entries during cleanup",
                    displayed_path.display(),
                    mode & 0o7777,
                ));
            }
        }
        SourceKind::Symlink => {}
    }
    Ok(())
}

#[cfg(unix)]
fn preflight_verified_removal_unix(removal: &mut VerifiedRemoval) -> Result<(), String> {
    let quarantine_parent = removal
        .original_parent_handle
        .try_clone()
        .map_err(|error| {
            format!(
                "clone undo quarantine parent capability for {}: {error}",
                removal.quarantine_root.display(),
            )
        })?;
    let retained_payload = capability_child_path(&quarantine_parent, &removal.quarantine_name)
        .map_err(|error| format!("resolve retained quarantine root: {error}"))?;
    removal
        .cleanup_manifest
        .verify_cleanup_tree_at(&retained_payload)
        .map_err(|error| {
            format!(
                "verified object changed after quarantine at {}: {error}",
                removal.quarantine_root.display(),
            )
        })?;

    let capabilities = filesystem_capabilities(&retained_payload);
    for relative in removal.cleanup_manifest.relative_paths() {
        let proof = removal
            .cleanup_manifest
            .entry_proof(relative)
            .ok_or_else(|| format!("missing cleanup proof for {}", relative.display()))?;
        let (parent, name) = unix_open_parent_for_relative(
            &quarantine_parent,
            &removal.quarantine_name,
            relative,
            &removal.cleanup_manifest,
            capabilities,
            false,
        )?;
        let parent_display = if relative.as_os_str().is_empty() {
            removal
                .original
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            match relative.parent().filter(|path| !path.as_os_str().is_empty()) {
                Some(relative_parent) => removal.quarantine_root.join(relative_parent),
                None => removal.quarantine_root.clone(),
            }
        };
        verify_unix_quarantine_parent_contract(&parent, &parent_display)?;
        let opened = unix_open_component(&parent, &name, proof.snapshot.kind())?;
        let verifier = opened
            .try_clone()
            .map_err(|error| format!("clone opened cleanup capability: {error}"))?;
        unix_verify_opened_entry(
            proof,
            relative,
            &parent,
            &name,
            verifier,
            removal.cleanup_manifest.verification(),
            capabilities,
        )?;
        let displayed = if relative.as_os_str().is_empty() {
            removal.quarantine_root.clone()
        } else {
            removal.quarantine_root.join(relative)
        };
        verify_unix_entry_contract(&opened, &displayed, proof.snapshot.kind())?;
    }
    Ok(())
}

#[cfg(unix)]
fn unix_unlink_component(
    parent: &File,
    name: &std::ffi::OsStr,
    kind: SourceKind,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let name = unix_component_cstring(name)?;
    let flags = if kind == SourceKind::Directory {
        libc::AT_REMOVEDIR
    } else {
        0
    };
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } != 0 {
        return Err(format!(
            "unlink verified quarantined component {:?}: {}",
            std::ffi::OsStr::from_bytes(name.as_bytes()),
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn commit_prepared_verified_removal_unix(removal: &mut VerifiedRemoval) -> Result<(), String> {
    let quarantine_parent = removal
        .original_parent_handle
        .try_clone()
        .map_err(|error| {
            format!(
                "clone undo quarantine parent capability for {}: {error}",
                removal.quarantine_root.display(),
            )
        })?;
    let retained_payload = capability_child_path(&quarantine_parent, &removal.quarantine_name)
        .map_err(|error| format!("resolve retained quarantine root: {error}"))?;
    let capabilities = filesystem_capabilities(&retained_payload);
    let mut relative_paths = removal
        .cleanup_manifest
        .relative_paths()
        .cloned()
        .collect::<Vec<_>>();
    relative_paths.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| right.cmp(left))
    });
    if relative_paths.is_empty() {
        return Err("cleanup manifest contains no root entry".to_string());
    }

    let mut deletion_started = false;
    for relative in relative_paths {
        let proof = removal
            .cleanup_manifest
            .entry_proof(&relative)
            .ok_or_else(|| format!("missing cleanup proof for {}", relative.display()))?;
        let entry_kind = proof.snapshot.kind();
        let (parent, name) = unix_open_parent_for_relative(
            &quarantine_parent,
            &removal.quarantine_name,
            &relative,
            &removal.cleanup_manifest,
            capabilities,
            true,
        )?;
        let parent_display = if relative.as_os_str().is_empty() {
            removal
                .original
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            match relative.parent().filter(|path| !path.as_os_str().is_empty()) {
                Some(relative_parent) => removal.quarantine_root.join(relative_parent),
                None => removal.quarantine_root.clone(),
            }
        };
        verify_unix_quarantine_parent_contract(&parent, &parent_display)?;
        let opened = unix_open_component(&parent, &name, proof.snapshot.kind())?;
        let displayed = if relative.as_os_str().is_empty() {
            removal.quarantine_root.clone()
        } else {
            removal.quarantine_root.join(&relative)
        };
        verify_unix_entry_contract(&opened, &displayed, proof.snapshot.kind())?;

        let current = if proof.snapshot.kind() == SourceKind::Directory {
            unix_snapshot_open_component(&opened, &parent, &name, SourceKind::Directory)?
        } else {
            let verifier = opened
                .try_clone()
                .map_err(|error| format!("clone opened cleanup capability: {error}"))?;
            unix_verify_opened_entry(
                proof,
                &relative,
                &parent,
                &name,
                verifier,
                removal.cleanup_manifest.verification(),
                capabilities,
            )?
        };
        if proof.snapshot.kind() == SourceKind::Directory {
            proof
                .snapshot
                .verify_same_identity(&current)
                .map_err(|error| {
                    format!(
                        "quarantined directory changed immediately before unlink at {}: {error}",
                        relative.display(),
                    )
                })?;
        }

        if !deletion_started {
            removal
                .journal_mut()?
                .transition(VerifiedRemovalPhase::DeletionStarted)?;
            deletion_started = true;
        }
        unix_unlink_component(&parent, &name, entry_kind)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_open_for_verified_delete(path: &Path, kind: SourceKind) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const DELETE_ACCESS: u32 = 0x0001_0000;
    const FILE_READ_DATA: u32 = 0x0000_0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = fs::OpenOptions::new();
    options
        .access_mode(DELETE_ACCESS | FILE_READ_ATTRIBUTES | FILE_READ_DATA)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    let mut flags = 0;
    if kind == SourceKind::Directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    if kind == SourceKind::Symlink {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
    }
    options.custom_flags(flags).open(path)
}

#[cfg(windows)]
fn windows_snapshot_opened_component(
    opened: &File,
    path: &Path,
    expected_kind: SourceKind,
) -> Result<SourceSnapshot, String> {
    if expected_kind != SourceKind::Symlink {
        let snapshot = snapshot_open_handle(opened)
            .map_err(|error| format!("identify opened cleanup handle: {error}"))?;
        if snapshot.kind() != expected_kind {
            return Err(format!(
                "cleanup object kind changed from {expected_kind:?} to {:?}",
                snapshot.kind(),
            ));
        }
        return Ok(snapshot);
    }

    let snapshot = snapshot_path(path)
        .map_err(|error| format!("inspect quarantined reparse point: {error}"))?;
    if snapshot.kind() != SourceKind::Symlink {
        return Err("quarantined reparse point changed kind".to_string());
    }
    let handle_identity = windows_file_identity(opened)
        .map_err(|error| format!("identify opened reparse-point handle: {error}"))?;
    if snapshot.identity() != &handle_identity {
        return Err("quarantined reparse point changed while opening it".to_string());
    }
    Ok(snapshot)
}

#[cfg(windows)]
fn windows_verify_opened_cleanup_entry(
    proof: &SourceEntryProof,
    relative: &Path,
    path: &Path,
    handle: &File,
    capabilities: FilesystemCapabilities,
    verification: VerificationMode,
) -> Result<SourceSnapshot, String> {
    let before = windows_snapshot_opened_component(handle, path, proof.snapshot.kind())?;
    let moved_root = relative.as_os_str().is_empty();
    let comparison = if moved_root {
        proof
            .snapshot
            .verify_same_object_after_rename_with_capabilities(&before, capabilities)
    } else if capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
        proof.snapshot.verify_same_object_and_version(&before)
    } else {
        proof.snapshot.verify_same_identity_with_policy(
            &before,
            FilesystemIdentityPolicy::ContentVerifiedPortable,
        )
    };
    comparison.map_err(|error| {
        format!("opened quarantined object {} changed: {error}", path.display())
    })?;
    if proof.snapshot.kind() == SourceKind::File {
        if verification == VerificationMode::Standard {
            if proof.digest.is_some() {
                return Err(format!(
                    "standard cleanup proof unexpectedly carries a content digest: {}",
                    path.display(),
                ));
            }
            return Ok(before);
        }
        let mut verifier = handle
            .try_clone()
            .map_err(|error| format!("clone cleanup handle {}: {error}", path.display()))?;
        let digest = digest_open_file(&mut verifier)
            .map_err(|error| format!("digest opened quarantined file {}: {error}", path.display()))?;
        if proof.digest != Some(digest) {
            return Err(format!(
                "opened quarantined file digest changed: {}",
                path.display(),
            ));
        }
        let after = windows_snapshot_opened_component(handle, path, SourceKind::File)?;
        before.verify_same_object_and_version(&after).map_err(|error| {
            format!("opened quarantined file changed while hashing {}: {error}", path.display())
        })?;
        return Ok(after);
    }
    Ok(before)
}

#[cfg(windows)]
fn windows_mark_handle_for_delete(file: &File) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    const FILE_DISPOSITION_INFO_CLASS: i32 = 4;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x0000_0001;
    const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x0000_0010;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const ERROR_NOT_SUPPORTED: i32 = 50;

    #[repr(C)]
    struct FileDispositionInfo {
        delete_file: i32,
    }

    #[repr(C)]
    struct FileDispositionInfoEx {
        flags: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut c_void,
            information_class: i32,
            information: *const c_void,
            information_size: u32,
        ) -> i32;
    }

    let extended = FileDispositionInfoEx {
        flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FILE_DISPOSITION_INFO_EX_CLASS,
            (&extended as *const FileDispositionInfoEx).cast(),
            std::mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let extended_error = io::Error::last_os_error();
    if !matches!(
        extended_error.raw_os_error(),
        Some(ERROR_INVALID_PARAMETER) | Some(ERROR_NOT_SUPPORTED)
    ) {
        return Err(format!("mark verified object for deletion: {extended_error}"));
    }

    let legacy = FileDispositionInfo { delete_file: 1 };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FILE_DISPOSITION_INFO_CLASS,
            (&legacy as *const FileDispositionInfo).cast(),
            std::mem::size_of::<FileDispositionInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(format!(
            "mark verified object for deletion: {}",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn preflight_verified_removal_windows(removal: &mut VerifiedRemoval) -> Result<(), String> {
    removal
        .cleanup_manifest
        .verify_cleanup_tree_at(&removal.quarantine_root)
        .map_err(|error| {
            format!(
                "verified object changed after quarantine at {}: {error}",
                removal.quarantine_root.display(),
            )
        })?;

    let capabilities = filesystem_capabilities(&removal.quarantine_root);
    let verification = removal.cleanup_manifest.verification();
    for relative in removal.cleanup_manifest.relative_paths() {
        let proof = removal
            .cleanup_manifest
            .entry_proof(relative)
            .ok_or_else(|| format!("missing cleanup proof for {}", relative.display()))?;
        let path = if relative.as_os_str().is_empty() {
            removal.quarantine_root.clone()
        } else {
            removal.quarantine_root.join(relative)
        };
        let handle = windows_open_for_verified_delete(&path, proof.snapshot.kind()).map_err(|error| {
            format!("open verified quarantined object {}: {error}", path.display())
        })?;
        windows_verify_opened_cleanup_entry(
            proof,
            relative,
            &path,
            &handle,
            capabilities,
            verification,
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn commit_prepared_verified_removal_windows(
    removal: &mut VerifiedRemoval,
) -> Result<(), String> {
    let capabilities = filesystem_capabilities(&removal.quarantine_root);
    let verification = removal.cleanup_manifest.verification();
    let mut relative_paths = removal
        .cleanup_manifest
        .relative_paths()
        .cloned()
        .collect::<Vec<_>>();
    relative_paths.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| right.cmp(left))
    });
    if relative_paths.is_empty() {
        return Err("cleanup manifest contains no root entry".to_string());
    }

    let mut deletion_started = false;
    for relative in relative_paths {
        let proof = removal
            .cleanup_manifest
            .entry_proof(&relative)
            .ok_or_else(|| format!("missing cleanup proof for {}", relative.display()))?;
        let path = if relative.as_os_str().is_empty() {
            removal.quarantine_root.clone()
        } else {
            removal.quarantine_root.join(&relative)
        };
        let handle = windows_open_for_verified_delete(&path, proof.snapshot.kind()).map_err(|error| {
            format!("open verified quarantined object {}: {error}", path.display())
        })?;
        let current = if proof.snapshot.kind() == SourceKind::Directory {
            windows_snapshot_opened_component(&handle, &path, SourceKind::Directory)?
        } else {
            windows_verify_opened_cleanup_entry(
                proof,
                &relative,
                &path,
                &handle,
                capabilities,
                verification,
            )?
        };
        if proof.snapshot.kind() == SourceKind::Directory {
            proof.snapshot.verify_same_identity(&current).map_err(|error| {
                format!(
                    "quarantined directory changed immediately before deletion at {}: {error}",
                    path.display(),
                )
            })?;
        }
        if !deletion_started {
            removal
                .journal_mut()?
                .transition(VerifiedRemovalPhase::DeletionStarted)?;
            deletion_started = true;
        }
        windows_mark_handle_for_delete(&handle)?;
        drop(handle);
    }
    Ok(())
}

impl Drop for VerifiedRemoval {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        // A failed first unlink can leave the journal in DeletionStarted even
        // though the complete detached tree is still intact. Restore whenever
        // the operation-time proof still matches; retain only genuinely partial
        // or replaced trees for explicit recovery.
        match self.quarantine_matches_proof() {
            Ok(()) => match self.restore_impl(false) {
                Ok(()) => {
                    self.finish_journal_after_namespace_commit("automatic restore");
                }
                Err(error) => {
                    if let Some(journal) = self.journal.as_ref() {
                        journal.deactivate();
                    }
                    log::error!(
                        "could not restore intact interrupted copy-undo removal for {}; retained at {}: {error}",
                        self.original.display(),
                        self.quarantine_root.display(),
                    );
                }
            },
            Err(error) => {
                if let Some(journal) = self.journal.as_ref() {
                    journal.deactivate();
                }
                log::error!(
                    "copy-undo cleanup left a partial or changed quarantine for {}; retained at {}: {error}",
                    self.original.display(),
                    self.quarantine_root.display(),
                );
            }
        }
    }
}

fn private_quarantine_nonce(sequence: u64) -> u128 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};

    fn half(seed: RandomState, sequence: u64, discriminator: u64) -> u64 {
        let mut hasher = seed.build_hasher();
        std::process::id().hash(&mut hasher);
        sequence.hash(&mut hasher);
        discriminator.hash(&mut hasher);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
        hasher.finish()
    }

    ((half(RandomState::new(), sequence, 0) as u128) << 64)
        | half(RandomState::new(), sequence, 1) as u128
}

/// Atomically detach a public pathname into a private same-directory
/// quarantine, then verify the detached object against operation-time proof.
/// A replacement that wins the race at the public pathname is captured but not
/// deleted: verification fails and the function restores it when possible.
pub fn prepare_verified_removal(
    source_manifest: &SourceManifest,
    destination_manifest: &DestinationManifest,
    root: &Path,
) -> Result<VerifiedRemoval, String> {
    #[cfg(unix)]
    {
        prepare_verified_removal_unix(source_manifest, destination_manifest, root)
    }
    #[cfg(windows)]
    {
        prepare_verified_removal_windows(source_manifest, destination_manifest, root)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source_manifest, destination_manifest);
        Err(format!(
            "identity-bound copy-undo cleanup is unavailable on this platform; refusing to detach {}",
            root.display(),
        ))
    }
}

#[cfg(unix)]
fn prepare_verified_removal_unix(
    source_manifest: &SourceManifest,
    destination_manifest: &DestinationManifest,
    root: &Path,
) -> Result<VerifiedRemoval, String> {
    let original_name = root.file_name().ok_or_else(|| {
        format!("refusing to quarantine filesystem root: {}", root.display())
    })?;
    let parent = root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let original_parent_handle = open_directory_capability(parent).map_err(|error| {
        format!(
            "open parent directory capability for {}: {error}",
            root.display(),
        )
    })?;
    verify_unix_quarantine_parent_contract(&original_parent_handle, parent)?;
    let cleanup_manifest = destination_manifest.cleanup_manifest(source_manifest)?;
    let recovery_commitment = cleanup_manifest.recovery_commitment();

    static NEXT_QUARANTINE: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let mut allocated = None;
    for _ in 0..1024 {
        let sequence = NEXT_QUARANTINE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = private_quarantine_nonce(sequence);
        let name = std::ffi::OsString::from(format!(
            ".tui-file-picker-undo-quarantine-{nonce:032x}",
        ));
        let journal = match create_removal_journal(
            parent,
            nonce,
            original_name,
            &name,
            cleanup_manifest.verification(),
            recovery_commitment,
        ) {
            Ok(journal) => journal,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create copy-undo recovery journal beside {}: {error}",
                    root.display(),
                ));
            }
        };
        match rename_between_open_directories_no_replace(
            &original_parent_handle,
            original_name,
            &original_parent_handle,
            &name,
        ) {
            Ok(()) => {
                allocated = Some((name, journal));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                journal.remove()?;
                continue;
            }
            Err(error) => {
                let journal_error = journal.remove().err();
                return Err(format!(
                    "atomically quarantine {} before undo removal: {error}{}",
                    root.display(),
                    journal_error
                        .map(|cleanup| format!("; also failed to remove unused recovery journal: {cleanup}"))
                        .unwrap_or_default(),
                ));
            }
        }
    }
    let (quarantine_name, mut journal) = allocated.ok_or_else(|| {
        format!("could not allocate undo quarantine beside {}", root.display())
    })?;
    let quarantine_root = parent.join(&quarantine_name);

    if let Err(error) = journal.transition(VerifiedRemovalPhase::Detached) {
        let guard = VerifiedRemoval {
            original: root.to_path_buf(),
            quarantine_root,
            quarantine_name,
            cleanup_manifest,
            journal: Some(journal),
            armed: true,
            original_parent_handle,
            original_name: original_name.to_os_string(),
        };
        return match guard.restore_unverified_capture() {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!("{error}; {restore_error}")),
        };
    }

    let guard = VerifiedRemoval {
        original: root.to_path_buf(),
        quarantine_root,
        quarantine_name,
        cleanup_manifest,
        journal: Some(journal),
        armed: true,
        original_parent_handle,
        original_name: original_name.to_os_string(),
    };
    let retained_root = capability_child_path(
        &guard.original_parent_handle,
        &guard.quarantine_name,
    )
    .map_err(|error| format!("resolve retained quarantine root: {error}"))?;
    if let Err(error) = guard.cleanup_manifest.verify_cleanup_tree_at(&retained_root) {
        return match guard.restore_unverified_capture() {
            Ok(()) => Err(format!(
                "copy undo refused because the detached object no longer matches operation-time proof: {error}"
            )),
            Err(restore_error) => Err(format!(
                "copy undo refused because the detached object no longer matches operation-time proof: {error}; {restore_error}"
            )),
        };
    }
    Ok(guard)
}

#[cfg(windows)]
fn windows_move_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let ok = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(80) | Some(183) => Err(io::Error::new(io::ErrorKind::AlreadyExists, error)),
            _ => Err(error),
        }
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn prepare_verified_removal_windows(
    source_manifest: &SourceManifest,
    destination_manifest: &DestinationManifest,
    root: &Path,
) -> Result<VerifiedRemoval, String> {
    let original_name = root.file_name().ok_or_else(|| {
        format!("refusing to quarantine filesystem root: {}", root.display())
    })?;
    let parent = root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let cleanup_manifest = destination_manifest.cleanup_manifest(source_manifest)?;
    let recovery_commitment = cleanup_manifest.recovery_commitment();

    static NEXT_QUARANTINE: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let mut allocated = None;
    for _ in 0..1024 {
        let sequence = NEXT_QUARANTINE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = private_quarantine_nonce(sequence);
        let quarantine_name = std::ffi::OsString::from(format!(
            ".tui-file-picker-undo-quarantine-{nonce:032x}",
        ));
        let quarantine_root = parent.join(&quarantine_name);
        let journal = match create_removal_journal(
            parent,
            nonce,
            original_name,
            &quarantine_name,
            cleanup_manifest.verification(),
            recovery_commitment,
        ) {
            Ok(journal) => journal,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create copy-undo recovery journal beside {}: {error}",
                    root.display(),
                ));
            }
        };
        match windows_move_no_replace(root, &quarantine_root) {
            Ok(()) => {
                allocated = Some((quarantine_name, quarantine_root, journal));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                journal.remove()?;
                continue;
            }
            Err(error) => {
                let journal_error = journal.remove().err();
                return Err(format!(
                    "atomically quarantine {} before undo removal: {error}{}",
                    root.display(),
                    journal_error
                        .map(|cleanup| format!("; also failed to remove unused recovery journal: {cleanup}"))
                        .unwrap_or_default(),
                ));
            }
        }
    }
    let (quarantine_name, quarantine_root, mut journal) = allocated.ok_or_else(|| {
        format!("could not allocate undo quarantine beside {}", root.display())
    })?;

    if let Err(error) = journal.transition(VerifiedRemovalPhase::Detached) {
        let guard = VerifiedRemoval {
            original: root.to_path_buf(),
            quarantine_root,
            quarantine_name,
            cleanup_manifest,
            journal: Some(journal),
            armed: true,
        };
        return match guard.restore_unverified_capture() {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!("{error}; {restore_error}")),
        };
    }

    let guard = VerifiedRemoval {
        original: root.to_path_buf(),
        quarantine_root,
        quarantine_name,
        cleanup_manifest,
        journal: Some(journal),
        armed: true,
    };
    if let Err(error) = guard
        .cleanup_manifest
        .verify_cleanup_tree_at(&guard.quarantine_root)
    {
        return match guard.restore_unverified_capture() {
            Ok(()) => Err(format!(
                "copy undo refused because the detached object no longer matches operation-time proof: {error}"
            )),
            Err(restore_error) => Err(format!(
                "copy undo refused because the detached object no longer matches operation-time proof: {error}; {restore_error}"
            )),
        };
    }
    Ok(guard)
}

pub fn capture_manifest(root: &Path) -> Result<SourceManifest, String> {
    capture_manifest_with_mode(root, VerificationMode::Strong)
}

pub fn capture_manifest_with_cancel<F>(
    root: &Path,
    keep_going: F,
) -> Result<SourceManifest, String>
where
    F: FnMut(&Path) -> bool,
{
    capture_manifest_with_mode_and_cancel(root, VerificationMode::Strong, keep_going)
}

pub fn capture_manifest_with_mode(
    root: &Path,
    verification: VerificationMode,
) -> Result<SourceManifest, String> {
    capture_manifest_with_mode_and_cancel(root, verification, |_: &Path| true)
}

pub fn capture_manifest_with_mode_and_cancel<F>(
    root: &Path,
    verification: VerificationMode,
    mut keep_going: F,
) -> Result<SourceManifest, String>
where
    F: FnMut(&Path) -> bool,
{
    fn capture_node<F>(
        root: &Path,
        path: &Path,
        verification: VerificationMode,
        manifest: &mut SourceManifest,
        entries: &mut usize,
        depth: usize,
        keep_going: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&Path) -> bool,
    {
        if !keep_going(path) {
            return Err("source manifest capture was interrupted".to_string());
        }
        if depth > MAX_MANIFEST_DEPTH {
            return Err(format!(
                "source tree exceeds the maximum supported nesting depth of {MAX_MANIFEST_DEPTH}: {}",
                path.display()
            ));
        }
        *entries = entries.saturating_add(1);
        if *entries > MAX_MANIFEST_ENTRIES {
            return Err(format!(
                "source tree exceeds the bounded manifest limit of {MAX_MANIFEST_ENTRIES} entries; split the move into smaller roots"
            ));
        }
        let before = snapshot_path(path)
            .map_err(|error| format!("capture source {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("source escaped manifest root: {}", path.display()))?
            .to_path_buf();
        match before.kind() {
            SourceKind::File => {
                let mut file = File::open(path)
                    .map_err(|error| format!("open source {}: {error}", path.display()))?;
                let opened = snapshot_open_file(&file)
                    .map_err(|error| format!("identify opened source {}: {error}", path.display()))?;
                before.verify_same_object_and_version(&opened).map_err(|error| {
                    format!("source changed before manifest capture {}: {error}", path.display())
                })?;
                let digest = if verification == VerificationMode::Strong {
                    Some(
                        digest_open_file_with_cancel(&mut file, path, keep_going)
                            .map_err(|error| {
                                format!("read source {}: {error}", path.display())
                            })?,
                    )
                } else {
                    None
                };
                let after = snapshot_open_file(&file)
                    .map_err(|error| format!("re-identify source {}: {error}", path.display()))?;
                opened.verify_same_object_and_version(&after).map_err(|error| {
                    format!("source changed during manifest capture {}: {error}", path.display())
                })?;
                manifest.insert(relative, before, digest)?;
            }
            SourceKind::Symlink => {
                manifest.insert(relative, before, None)?;
            }
            SourceKind::Directory => {
                manifest.insert(relative, before.clone(), None)?;
                let directory_entries = fs::read_dir(path)
                    .map_err(|error| format!("read source directory {}: {error}", path.display()))?;
                for entry in directory_entries {
                    let entry = entry
                        .map_err(|error| format!("read source entry {}: {error}", path.display()))?;
                    capture_node(
                        root,
                        &entry.path(),
                        verification,
                        manifest,
                        entries,
                        depth + 1,
                        keep_going,
                    )?;
                }
                let after = snapshot_path(path)
                    .map_err(|error| format!("re-identify directory {}: {error}", path.display()))?;
                before.verify_same_object_and_version(&after).map_err(|error| {
                    format!("source directory changed during manifest capture {}: {error}", path.display())
                })?;
            }
        }
        Ok(())
    }

    let mut manifest = SourceManifest::new(verification);
    let mut entries = 0usize;
    capture_node(
        root,
        root,
        verification,
        &mut manifest,
        &mut entries,
        0,
        &mut keep_going,
    )?;
    Ok(manifest)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod verified_removal_tests {
    use super::*;

    fn copy_proof(source: &Path, destination: &Path) -> (SourceManifest, DestinationManifest) {
        let source_manifest = capture_manifest(source).expect("source manifest");
        fs::copy(source, destination).expect("copy fixture");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(destination)
            .expect("destination proof");
        (source_manifest, destination_manifest)
    }

    #[test]
    fn standard_recovery_journal_reconstructs_identity_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let source_manifest = capture_manifest_with_mode(
            &source,
            VerificationMode::Standard,
        )
        .expect("standard source manifest");
        fs::copy(&source, &destination).expect("copy fixture");
        let destination_manifest = source_manifest
            .capture_identity_copy_at(&destination)
            .expect("standard destination proof");

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare standard verified removal");
        let journal = removal.journal.as_ref().expect("journal").path.clone();
        removal.journal.as_ref().expect("journal").deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals_internal(temp.path(), true)
            .expect("recover standard journal");
        assert_eq!(report.restored, vec![destination.clone()]);
        assert!(report.retained.is_empty(), "unexpected retained state: {:?}", report.retained);
        assert!(destination.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn legacy_v4_recovery_journal_is_parsed_as_strong_authority() {
        let owner_token = current_process_owner_token();
        let original_name = std::ffi::OsStr::new("copy.flac");
        let quarantine_name = std::ffi::OsStr::new(
            ".tui-file-picker-undo-quarantine-00000000000000000000000000001234",
        );
        let commitment = ContentDigest([0x5a; 32]);
        let binding = legacy_recovery_journal_binding(
            &owner_token,
            original_name,
            quarantine_name,
            commitment,
        );
        let payload = format!(
            "{LEGACY_REMOVAL_JOURNAL_VERSION}\nowner={}\noriginal={}\nquarantine={}\ncommitment={}\nbinding={}\n",
            encode_hex(owner_token.as_bytes()),
            encode_os_component(original_name),
            encode_os_component(quarantine_name),
            commitment.to_hex(),
            binding.to_hex(),
        );

        let parsed = parse_journal_payload(payload.as_bytes()).expect("parse legacy journal");
        assert_eq!(parsed.verification, VerificationMode::Strong);
        assert_eq!(parsed.commitment, commitment);
    }

    #[test]
    fn verified_removal_deletes_only_the_atomically_detached_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        assert!(!destination.exists(), "public pathname is detached before deletion");
        assert_eq!(
            fs::read(removal.quarantine_root()).expect("quarantined payload"),
            b"operation bytes",
        );
        let quarantine_root = removal.quarantine_root().to_path_buf();
        let journal_path = removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .path
            .clone();
        removal.commit().expect("commit verified removal");
        assert!(!destination.exists());
        assert!(
            !quarantine_root.exists(),
            "successful cleanup must leave no quarantine container or payload",
        );
        assert!(!journal_path.exists(), "successful cleanup removes its journal");
    }

    #[test]
    fn dropping_prepared_removal_restores_the_detached_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let journal = removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .path
            .clone();
        drop(removal);

        assert_eq!(fs::read(&destination).expect("restored copy"), b"operation bytes");
        assert!(!quarantine.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn startup_recovery_restores_a_crash_interrupted_detach() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let journal = removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .path
            .clone();
        removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals_internal(temp.path(), true)
            .expect("recover interrupted detach");

        assert_eq!(report.restored, vec![destination.clone()]);
        assert_eq!(fs::read(&destination).expect("restored copy"), b"operation bytes");
        assert!(!quarantine.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn active_recovery_registry_normalizes_parent_path_spelling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let name = ".tui-file-picker-copy-undo-00000000000000000000000000000001.detached";
        let canonical_spelling = temp.path().join(name);
        let alternate_spelling = temp.path().join(".").join(name);

        register_active_removal_journal(&alternate_spelling);
        assert!(is_active_removal_journal(&canonical_spelling));
        unregister_active_removal_journal(&canonical_spelling);
        assert!(!is_active_removal_journal(&alternate_spelling));
    }

    #[test]
    fn recovery_scan_ignores_a_live_removal_owned_by_this_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();

        let report = recover_interrupted_verified_removals(temp.path())
            .expect("scan while removal is active");

        assert_eq!(report.deferred.len(), 1);
        assert!(report.restored.is_empty());
        assert!(report.cleaned_markers.is_empty());
        assert!(report.retained.is_empty());
        assert!(!destination.exists());
        assert!(quarantine.exists());
        drop(removal);
        assert_eq!(fs::read(&destination).expect("drop restored copy"), b"operation bytes");
    }

    #[test]
    fn startup_recovery_restores_an_intact_deletion_started_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let mut removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        removal
            .journal_mut()
            .expect("recovery journal")
            .transition(VerifiedRemovalPhase::DeletionStarted)
            .expect("mark deletion started");
        let quarantine = removal.quarantine_root().to_path_buf();
        removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals_internal(temp.path(), true)
            .expect("scan interrupted deletion");

        assert_eq!(report.restored, vec![destination.clone()]);
        assert_eq!(fs::read(&destination).expect("restored copy"), b"operation bytes");
        assert!(!quarantine.exists());
        assert!(report.retained.is_empty());
    }

    #[test]
    fn dropping_an_intact_deletion_started_guard_restores_the_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let mut removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        removal
            .journal_mut()
            .expect("recovery journal")
            .transition(VerifiedRemovalPhase::DeletionStarted)
            .expect("mark deletion started");
        let quarantine = removal.quarantine_root().to_path_buf();
        drop(removal);

        assert_eq!(fs::read(&destination).expect("restored copy"), b"operation bytes");
        assert!(!quarantine.exists());
    }

    #[test]
    fn recovery_refuses_a_replaced_quarantine_object() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let relocated = temp.path().join("relocated-operation-copy.flac");
        fs::rename(&quarantine, &relocated).expect("relocate real quarantine");
        fs::write(&quarantine, b"unrelated replacement").expect("replacement");
        removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals_internal(temp.path(), true)
            .expect("scan replaced quarantine");

        assert!(!destination.exists());
        assert_eq!(report.retained.len(), 1);
        assert_eq!(report.retained[0].0, quarantine);
        assert_eq!(fs::read(&relocated).expect("real copy retained"), b"operation bytes");
        assert_eq!(fs::read(&report.retained[0].0).expect("replacement retained"), b"unrelated replacement");
    }

    #[test]
    fn recovery_refuses_a_forged_or_mismatched_commitment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let journal = removal.journal.as_ref().expect("journal").path.clone();
        let payload = fs::read_to_string(&journal).expect("read journal");
        let forged = payload
            .lines()
            .map(|line| {
                if line.starts_with("commitment=") {
                    format!("commitment={}", "00".repeat(32))
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n") + "\n";
        fs::write(&journal, forged).expect("forge commitment");
        removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals_internal(temp.path(), true)
            .expect("scan forged journal");

        assert!(!destination.exists());
        assert!(quarantine.exists());
        assert_eq!(report.retained.len(), 1);
        assert!(report.retained[0].1.contains("authority binding"));
    }

    #[test]
    fn stale_process_start_token_does_not_defer_recovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let journal = removal.journal.as_ref().expect("journal").path.clone();
        let payload = fs::read_to_string(&journal).expect("read journal");
        let current = current_process_owner_token();
        let mut fields = current.split(':').map(str::to_string).collect::<Vec<_>>();
        let last = fields.last_mut().expect("owner token field");
        let stale_number = last.parse::<u128>().unwrap_or_default().wrapping_add(1);
        *last = stale_number.to_string();
        let stale_token = fields.join(":");
        let original_name = removal.original.file_name().expect("original component");
        let quarantine_name = removal.quarantine_root.file_name().expect("quarantine component");
        let commitment = removal.cleanup_manifest.recovery_commitment();
        let stale_binding = recovery_journal_binding(
            &stale_token,
            original_name,
            quarantine_name,
            removal.cleanup_manifest.verification(),
            commitment,
        );
        let stale_owner = encode_hex(stale_token.as_bytes());
        let rewritten = payload
            .lines()
            .map(|line| {
                if line.starts_with("owner=") {
                    format!("owner={stale_owner}")
                } else if line.starts_with("binding=") {
                    format!("binding={}", stale_binding.to_hex())
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n") + "\n";
        fs::write(&journal, rewritten).expect("rewrite owner token");
        removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .deactivate();
        std::mem::forget(removal);

        let report = recover_interrupted_verified_removals(temp.path())
            .expect("recover stale owner token");

        assert_eq!(report.restored, vec![destination.clone()]);
        assert!(report.deferred.is_empty());
        assert_eq!(fs::read(destination).expect("restored copy"), b"operation bytes");
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_shared_parent_is_rejected_before_detach() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let shared = temp.path().join("shared");
        fs::create_dir(&shared).expect("shared parent");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777))
            .expect("make parent non-sticky and shared");
        let source = temp.path().join("source.flac");
        let destination = shared.join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let error = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect_err("unsafe parent must fail closed");

        assert!(error.contains("group- or other-writable"));
        assert_eq!(fs::read(destination).expect("copy remains public"), b"operation bytes");
    }

    #[test]
    fn replacement_before_quarantine_is_restored_and_never_deleted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);
        fs::remove_file(&destination).expect("remove operation copy");
        fs::write(&destination, b"replacement bytes").expect("replacement");

        let error = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect_err("replacement must fail proof");
        assert!(error.contains("no longer matches operation-time proof"));
        assert_eq!(
            fs::read(&destination).expect("replacement restored"),
            b"replacement bytes",
        );
    }

    #[test]
    fn replacement_after_quarantine_survives_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        fs::write(&destination, b"new unrelated object").expect("replacement");
        removal.commit().expect("remove quarantined operation copy");

        assert_eq!(
            fs::read(&destination).expect("replacement retained"),
            b"new unrelated object",
        );
    }


    #[test]
    fn quarantine_path_replacement_cannot_redirect_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let relocated = temp.path().join("relocated-operation-copy.flac");
        fs::rename(&quarantine, &relocated).expect("relocate real quarantine");
        fs::write(&quarantine, b"unrelated replacement").expect("replacement payload");

        let error = removal
            .commit()
            .expect_err("namespace replacement must fail closed");

        assert!(error.contains("changed") || error.contains("identity"));
        assert_eq!(
            fs::read(&quarantine).expect("replacement retained"),
            b"unrelated replacement",
        );
        assert_eq!(
            fs::read(&relocated).expect("operation copy retained"),
            b"operation bytes",
        );
    }

    #[test]
    fn quarantine_path_replacement_cannot_redirect_restore() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let relocated = temp.path().join("relocated-operation-copy.flac");
        fs::rename(&quarantine, &relocated).expect("relocate real quarantine");
        fs::write(&quarantine, b"unrelated replacement").expect("replacement payload");

        let error = removal
            .restore()
            .expect_err("restore must not move a replacement object");

        assert!(error.contains("refusing to restore changed quarantined object"));
        assert!(!destination.exists());
        assert_eq!(
            fs::read(&quarantine).expect("replacement retained"),
            b"unrelated replacement",
        );
        assert_eq!(
            fs::read(&relocated).expect("operation copy retained"),
            b"operation bytes",
        );
    }

    fn copy_directory_fixture(source: &Path, destination: &Path) {
        fs::create_dir(destination).expect("create destination root");
        for entry in fs::read_dir(source).expect("read source directory") {
            let entry = entry.expect("source directory entry");
            let target = destination.join(entry.file_name());
            let metadata = entry.metadata().expect("source metadata");
            if metadata.is_dir() {
                copy_directory_fixture(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("copy source file");
            }
        }
    }

    #[test]
    fn verified_removal_commits_nested_directory_bottom_up() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source-album");
        let destination = temp.path().join("copied-album");
        fs::create_dir(&source).expect("source root");
        fs::create_dir(source.join("disc 01")).expect("source disc");
        fs::write(source.join("cover.jpg"), b"cover").expect("cover");
        fs::write(source.join("disc 01/01.flac"), b"audio").expect("audio");

        let source_manifest = capture_manifest(&source).expect("source manifest");
        copy_directory_fixture(&source, &destination);
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("destination proof");

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare nested verified removal");
        let quarantined = removal.quarantine_root().to_path_buf();
        removal.commit().expect("commit nested verified removal");

        assert!(!destination.exists());
        assert!(!quarantined.exists());
        assert!(source.join("disc 01/01.flac").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_nested_parent_fails_preflight_and_restores_the_complete_tree() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source-album");
        let destination = temp.path().join("copied-album");
        fs::create_dir(&source).expect("source root");
        fs::create_dir(source.join("shared")).expect("source shared directory");
        fs::set_permissions(
            source.join("shared"),
            fs::Permissions::from_mode(0o777),
        )
        .expect("source shared permissions");
        fs::write(source.join("shared/01.flac"), b"audio").expect("source audio");

        let source_manifest = capture_manifest(&source).expect("source manifest");
        copy_directory_fixture(&source, &destination);
        fs::set_permissions(
            destination.join("shared"),
            fs::Permissions::from_mode(0o777),
        )
        .expect("destination shared permissions");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("destination proof");
        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");

        let error = removal
            .commit()
            .expect_err("unsafe nested namespace must fail before deletion");

        assert!(error.contains("group- or other-writable"));
        assert_eq!(
            fs::read(destination.join("shared/01.flac")).expect("tree restored"),
            b"audio",
        );
    }

    #[test]
    fn unexpected_quarantine_child_fails_before_any_planned_child_is_unlinked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source-album");
        let destination = temp.path().join("copied-album");
        fs::create_dir(&source).expect("source root");
        fs::write(source.join("01.flac"), b"audio").expect("source audio");

        let source_manifest = capture_manifest(&source).expect("source manifest");
        copy_directory_fixture(&source, &destination);
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("destination proof");
        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantined = removal.quarantine_root().to_path_buf();
        fs::write(quarantined.join("unexpected.txt"), b"unrelated")
            .expect("inject unexpected child");

        let error = removal.commit().expect_err("unexpected child must fail closed");

        assert!(error.contains("tree membership changed"), "unexpected error: {error}");
        assert_eq!(fs::read(quarantined.join("01.flac")).expect("planned child retained"), b"audio");
        assert_eq!(fs::read(quarantined.join("unexpected.txt")).expect("unexpected child retained"), b"unrelated");
    }

    #[test]
    fn mutation_inside_quarantine_fails_closed_without_unlinking_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantined = removal.quarantine_root().to_path_buf();
        fs::write(&quarantined, b"mutated in quarantine").expect("mutate quarantine");
        let error = removal.commit().expect_err("mutation must fail closed");

        assert!(error.contains("changed after quarantine"));
        assert_eq!(
            fs::read(&quarantined).expect("mutated object retained"),
            b"mutated in quarantine",
        );
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    fn file_copy_proof_with_destination_mode(
        source: &Path,
        destination: &Path,
        mode: u32,
    ) -> (SourceManifest, DestinationManifest) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(source, b"operation bytes").expect("source fixture");
        let source_manifest = capture_manifest(source).expect("source manifest");
        fs::copy(source, destination).expect("copy fixture");
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))
            .expect("set destination mode");
        let mut destination_manifest = DestinationManifest::default();
        destination_manifest
            .insert(PathBuf::new(), snapshot_path(destination).expect("destination snapshot"))
            .expect("destination proof root");
        (source_manifest, destination_manifest)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn prepared_removal_retains_constant_descriptor_count_for_large_trees() {
        fn fd_count() -> usize {
            fs::read_dir("/proc/self/fd")
                .expect("enumerate process descriptors")
                .count()
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::create_dir(&destination).expect("destination directory");
        for index in 0..1_500usize {
            let name = format!("track-{index:04}.flac");
            fs::write(source.join(&name), b"audio").expect("source member");
            fs::write(destination.join(&name), b"audio").expect("destination member");
        }
        let source_manifest = capture_manifest(&source).expect("source manifest");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("destination proof");

        let baseline = fd_count();
        let prepared = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("detach destination")
        .prepare_for_commit()
        .expect("bounded preflight");
        // The process-wide count includes descriptors sibling tests hold
        // transiently; `prepared`'s own retention is constant, so re-sample
        // until concurrent churn settles before judging it.
        let mut retained = fd_count().saturating_sub(baseline);
        for _ in 0..20 {
            if retained <= 8 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            retained = fd_count().saturating_sub(baseline);
        }
        assert!(
            retained <= 8,
            "prepared cleanup retained {retained} descriptors for a 1,501-entry tree",
        );
        prepared.restore().expect("restore prepared tree");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_preserved_file_is_rejected_before_deletion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        let (source_manifest, destination_manifest) =
            file_copy_proof_with_destination_mode(&source, &destination, 0o000);

        let error = match prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        ) {
            Ok(removal) => removal
                .prepare_for_commit()
                .expect_err("mode-0000 copy must be non-undoable")
                .to_string(),
            Err(error) => error,
        };

        assert!(
            error.contains("Permission denied")
                || error.contains("matches operation-time proof")
                || error.contains("owner-read permission"),
        );
        assert!(destination.exists(), "failed preflight restores the destination");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_directory_with_children_is_rejected_before_deletion() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::create_dir(&destination).expect("destination directory");
        fs::write(source.join("track.flac"), b"audio").expect("source member");
        fs::write(destination.join("track.flac"), b"audio").expect("destination member");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o555))
            .expect("source directory mode");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o555))
            .expect("destination directory mode");
        let source_manifest = capture_manifest(&source).expect("source manifest");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(&destination)
            .expect("destination proof");

        let error = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("detach read-only directory")
        .prepare_for_commit()
        .expect_err("mode-0555 directory must be non-undoable");

        assert!(error.contains("read/write/execute"));
        assert!(destination.exists(), "failed preflight restores the directory");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .expect("restore fixture permissions");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755))
            .expect("restore source fixture permissions");
    }

    #[cfg(unix)]
    #[test]
    fn group_writable_file_is_rejected_before_deletion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        let (source_manifest, destination_manifest) =
            file_copy_proof_with_destination_mode(&source, &destination, 0o660);
        assert!(
            destination_manifest
                .validate_copy_undo_metadata_contract()
                .is_err(),
            "the copy worker must classify this proof as non-reversible",
        );

        let error = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("detach group-writable copy")
        .prepare_for_commit()
        .expect_err("group-writable copy must be non-undoable");

        assert!(error.contains("group- or other-writable file"));
        assert!(destination.exists(), "failed preflight restores the destination");
    }

    #[cfg(unix)]
    #[test]
    fn sticky_directory_owned_by_another_user_fails_the_authority_contract() {
        let error = verify_unix_parent_mode_contract(
            2000,
            0o1777,
            1000,
            Path::new("/shared-owned-by-another-user"),
        )
        .expect_err("sticky bit does not substitute for parent ownership");
        assert!(error.contains("owned by the current user"));
    }

    #[test]
    fn recovery_cannot_observe_visible_journal_before_active_registration() {
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        let temp = tempfile::tempdir().expect("tempdir");
        let proof_root = temp.path().join("proof-root");
        fs::write(&proof_root, b"operation bytes").expect("proof fixture");
        let commitment = capture_manifest(&proof_root)
            .expect("proof manifest")
            .recovery_commitment();
        let published = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hook_parent = temp.path().to_path_buf();
        let hook_published = Arc::clone(&published);
        let hook_release = Arc::clone(&release);
        *journal_publication_test_hook()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
            JournalPublicationTestHook {
                parent: hook_parent.clone(),
                callback: Arc::new(move || {
                    hook_published.wait();
                    hook_release.wait();
                }),
            },
        );

        let creator_parent = temp.path().to_path_buf();
        let creator = std::thread::spawn(move || {
            create_removal_journal(
                &creator_parent,
                0x1234,
                std::ffi::OsStr::new("copy.flac"),
                std::ffi::OsStr::new(
                    ".tui-file-picker-undo-quarantine-00000000000000000000000000001234",
                ),
                VerificationMode::Strong,
                commitment,
            )
            .expect("create recovery journal")
        });
        published.wait();

        let recovery_parent = temp.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let recovery = std::thread::spawn(move || {
            let result = recover_interrupted_verified_removals_internal(
                &recovery_parent,
                true,
            );
            let _ = tx.send(result);
        });
        let early_result = rx.recv_timeout(Duration::from_millis(100));
        release.wait();
        let journal = creator.join().expect("journal creator");
        assert!(
            matches!(
                early_result,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "recovery must block on the active-registry publication boundary",
        );
        let report = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("recovery result")
            .expect("recovery scan");
        recovery.join().expect("recovery thread");
        assert_eq!(report.deferred, vec![journal.path.clone()]);
        assert!(report.restored.is_empty());
        assert!(report.cleaned_markers.is_empty());
        assert!(report.retained.is_empty());

        *journal_publication_test_hook()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        journal.remove().expect("remove test journal");
    }

    #[test]
    fn live_current_process_owner_defers_recovery_even_without_registry_entry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);
        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let journal = removal.journal.as_ref().expect("journal").path.clone();
        removal.journal.as_ref().expect("journal").deactivate();

        let report = recover_interrupted_verified_removals(temp.path())
            .expect("scan live current-process journal");
        assert!(report.deferred.contains(&journal));
        assert!(!destination.exists(), "live detach must not be recovered concurrently");
        drop(removal);
        assert!(destination.exists(), "guard drop restores the live detach");
    }

}

#[cfg(all(test, windows))]
mod verified_removal_windows_tests {
    use super::*;

    fn copy_proof(source: &Path, destination: &Path) -> (SourceManifest, DestinationManifest) {
        let source_manifest = capture_manifest(source).expect("source manifest");
        fs::copy(source, destination).expect("copy fixture");
        let destination_manifest = source_manifest
            .capture_verified_copy_at(destination)
            .expect("destination proof");
        (source_manifest, destination_manifest)
    }

    #[test]
    fn handle_bound_windows_cleanup_removes_only_the_detached_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine_root = removal.quarantine_root().to_path_buf();
        let journal_path = removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .path
            .clone();
        fs::write(&destination, b"unrelated replacement").expect("public replacement");

        removal.commit().expect("commit handle-bound removal");

        assert_eq!(
            fs::read(&destination).expect("replacement retained"),
            b"unrelated replacement",
        );
        assert!(!quarantine_root.exists());
        assert!(!journal_path.exists(), "successful cleanup removes its journal");
    }


    #[test]
    fn dropping_windows_prepared_removal_restores_payload_and_removes_journal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let journal = removal
            .journal
            .as_ref()
            .expect("recovery journal")
            .path
            .clone();
        drop(removal);

        assert_eq!(fs::read(&destination).expect("restored copy"), b"operation bytes");
        assert!(!quarantine.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn windows_restore_refuses_a_replaced_quarantine_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("copy.flac");
        fs::write(&source, b"operation bytes").expect("source");
        let (source_manifest, destination_manifest) = copy_proof(&source, &destination);

        let removal = prepare_verified_removal(
            &source_manifest,
            &destination_manifest,
            &destination,
        )
        .expect("prepare verified removal");
        let quarantine = removal.quarantine_root().to_path_buf();
        let relocated = temp.path().join("relocated-operation-copy.flac");
        windows_move_no_replace(&quarantine, &relocated).expect("relocate operation copy");
        fs::write(&quarantine, b"unrelated replacement").expect("replacement");

        let error = removal
            .restore()
            .expect_err("replacement must not be restored");

        assert!(error.contains("refusing to restore changed quarantined object"));
        assert!(!destination.exists());
        assert_eq!(fs::read(&relocated).expect("operation copy retained"), b"operation bytes");
        assert_eq!(fs::read(&quarantine).expect("replacement retained"), b"unrelated replacement");
    }
}

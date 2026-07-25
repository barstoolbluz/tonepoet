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
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};

const MAX_MANIFEST_ENTRIES: usize = 100_000;
const MAX_MANIFEST_DEPTH: usize = 1_024;


/// Deterministic per-operation filesystem I/O accounting.
///
/// These counters are intentionally byte- and call-based rather than timing-
/// based so tests can detect accidental proof amplification without depending
/// on machine or mount performance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
        if matches!(self.stable_path_identity, CapabilitySupport::Supported)
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
            "filesystem guarantees are reduced or unproven ({:?}; identity={:?}, timestamp-ns={:?}, xattrs={:?}, directory-sync={:?}, atomic-no-replace={:?}); native renames use retained-handle/type/size/path-transition evidence, while unavoidable copy/delete cleanup uses content hashes and tree membership",
            capabilities.semantics,
            capabilities.stable_path_identity,
            capabilities.nanosecond_timestamps,
            capabilities.extended_attributes,
            capabilities.directory_sync,
            capabilities.atomic_no_replace_rename,
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The destination mount lacks strict pathname identity or nanosecond
    /// timestamp guarantees, so the committed rename was accepted using the
    /// cheaper retained-handle/type/size/path-transition proof.
    pub portable_evidence: bool,
}

/// Verify a committed native rename without reading file contents.
///
/// A successful same-filesystem rename is itself the primary ownership event.
/// We additionally require disappearance of the source pathname, appearance
/// of the expected destination kind/size/target, stability of any retained
/// open handle, and strict path identity when the mount advertises it. Reduced-
/// semantics mounts deliberately use the cheaper type/size/path-transition
/// proof rather than forcing a recursive copy merely because inode or
/// nanosecond timestamp evidence is weak.
pub fn verify_committed_rename(
    source: &Path,
    destination: &Path,
    proof: &RenameSourceProof,
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

    let destination_snapshot = snapshot_path(destination)
        .map_err(|error| format!("could not identify renamed destination: {error}"))?;
    if destination_capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
        proof
            .snapshot
            .verify_same_object_after_rename_with_capabilities(
                &destination_snapshot,
                destination_capabilities,
            )?;
    } else {
        // Reduced-semantics mounts may round or synthesize timestamps during a
        // successful rename. Require only the trustworthy pathname evidence
        // here; any retained handle below supplies the stronger object proof.
        // This avoids turning an otherwise successful O(1) rename into a
        // recursive copy solely because timestamp fidelity is weak.
        proof
            .snapshot
            .verify_same_identity_with_policy(
                &destination_snapshot,
                FilesystemIdentityPolicy::ContentVerifiedPortable,
            )?;
    }

    if let (Some(handle), Some(open_before)) = (&proof.open_handle, &proof.open_snapshot) {
        let open_after = snapshot_open_handle(handle)
            .map_err(|error| format!("could not re-identify retained rename handle: {error}"))?;
        if destination_capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
            open_before
                .verify_same_object_and_version(&open_after)
                .map_err(|error| format!("source changed while native rename committed: {error}"))?;
        } else {
            open_before
                .verify_same_identity_with_policy(
                    &open_after,
                    FilesystemIdentityPolicy::ContentVerifiedPortable,
                )
                .map_err(|error| {
                    format!("retained source handle changed while native rename committed: {error}")
                })?;
        }
        open_after
            .verify_same_identity_with_policy(
                &destination_snapshot,
                destination_capabilities.identity_policy(),
            )
            .map_err(|error| {
                format!(
                    "renamed destination no longer corresponds to the retained source handle: {error}"
                )
            })?;
    } else if destination_capabilities.identity_policy() == FilesystemIdentityPolicy::Strict {
        proof
            .snapshot
            .verify_same_identity(&destination_snapshot)
            .map_err(|error| format!("renamed destination identity is unproven: {error}"))?;
    }

    Ok(RenameVerification {
        portable_evidence: destination_capabilities.identity_policy()
            == FilesystemIdentityPolicy::ContentVerifiedPortable,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntryProof {
    pub snapshot: SourceSnapshot,
    pub digest: Option<ContentDigest>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceManifest {
    entries: std::collections::BTreeMap<PathBuf, SourceEntryProof>,
}

/// Identity/version snapshots for the exact destination objects that passed
/// whole-tree content verification. A later source cleanup step must satisfy
/// both this destination-ownership proof and the source manifest's content
/// proof before it may remove the corresponding quarantined source object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DestinationManifest {
    entries: std::collections::BTreeMap<PathBuf, SourceSnapshot>,
}

impl DestinationManifest {
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

    /// Reconfirm that an entry which passed authoritative content verification
    /// still authorizes destructive source cleanup.
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
                "destination entry has no source content proof: {}",
                relative_path.display()
            )
        })?;

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
            "destination directory membership changed after content verification at {}",
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

    /// Counted cleanup form. For strict mounts, non-root entries retain an
    /// exact copy-time version token: renaming the source root does not rename
    /// descendants, so ctime remains authoritative and no content read is
    /// needed. A regular file that is itself the moved root is rehashed because
    /// the quarantine rename changes that file's ctime. Reduced-semantics mounts
    /// likewise rehash regular files.
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
            keep_going,
        )
    }

    /// Revalidate one destination entry against the source proof used to copy it.
    /// This supports a final destination-presence/content gate immediately before
    /// the corresponding source object is removed.
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
        if snapshot.kind() == SourceKind::File && digest.is_none() {
            return Err(format!(
                "regular-file proof is missing a content digest: {}",
                relative_path.display()
            ));
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
        let mut destination_manifest = DestinationManifest::default();
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
    keep_going: &mut F,
) -> Result<u64, String>
where
    F: FnMut(&Path) -> bool,
{
    verify_source_entry_after_root_rename_with_capabilities(
        path,
        proof,
        moved_root,
        filesystem_capabilities(path),
        keep_going,
    )
}

fn verify_source_entry_after_root_rename_with_capabilities<F>(
    path: &Path,
    proof: &SourceEntryProof,
    moved_root: bool,
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

pub fn capture_manifest(root: &Path) -> Result<SourceManifest, String> {
    capture_manifest_with_cancel(root, |_: &Path| true)
}

pub fn capture_manifest_with_cancel<F>(
    root: &Path,
    mut keep_going: F,
) -> Result<SourceManifest, String>
where
    F: FnMut(&Path) -> bool,
{
    fn capture_node<F>(
        root: &Path,
        path: &Path,
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
                use std::io::Read;
                let mut file = File::open(path)
                    .map_err(|error| format!("open source {}: {error}", path.display()))?;
                let opened = snapshot_open_file(&file)
                    .map_err(|error| format!("identify opened source {}: {error}", path.display()))?;
                before.verify_same_object_and_version(&opened).map_err(|error| {
                    format!("source changed before manifest capture {}: {error}", path.display())
                })?;
                let mut sha = Sha256::new();
                let mut buffer = vec![0u8; 1024 * 1024];
                loop {
                    if !keep_going(path) {
                        return Err("source manifest capture was interrupted".to_string());
                    }
                    let read = file
                        .read(&mut buffer)
                        .map_err(|error| format!("read source {}: {error}", path.display()))?;
                    if read == 0 {
                        break;
                    }
                    sha.update(&buffer[..read]);
                }
                let digest = sha.finalize();
                let after = snapshot_open_file(&file)
                    .map_err(|error| format!("re-identify source {}: {error}", path.display()))?;
                opened.verify_same_object_and_version(&after).map_err(|error| {
                    format!("source changed during manifest capture {}: {error}", path.display())
                })?;
                manifest.insert(relative, before, Some(digest))?;
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

    let mut manifest = SourceManifest::default();
    let mut entries = 0usize;
    capture_node(root, root, &mut manifest, &mut entries, 0, &mut keep_going)?;
    Ok(manifest)
}

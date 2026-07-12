//! Descriptor-relative filesystem capabilities for conversion actions.
//!
//! This module is deliberately independent of conversion-action and TUI types.
//! Absolute paths are accepted only while acquiring a root capability. Every
//! subsequent lookup and mutation walks validated relative components from a
//! retained directory descriptor with no-follow semantics.

#![cfg_attr(not(unix), allow(dead_code))]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::sync::{MutexGuard, OnceLock};

#[cfg(all(test, target_os = "linux"))]
static FORCE_OPENAT2_FALLBACK: AtomicBool = AtomicBool::new(false);
#[cfg(all(test, target_os = "linux"))]
static OPENAT2_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static FALLBACK_COMPONENT_OPENS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static DIRECTORY_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static DIRECTORY_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RacePoint {
    BeforeCreate,
    BeforeMkdir,
    BeforeRename,
    BeforeLink,
    BeforeUnlink,
}

#[cfg(test)]
type RaceCallback = Box<dyn FnOnce() + Send + 'static>;

#[cfg(test)]
static RACE_HOOK: OnceLock<Mutex<Option<(RacePoint, std::thread::ThreadId, RaceCallback)>>> =
    OnceLock::new();
#[cfg(test)]
static RACE_SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
struct RaceHookGuard {
    _serial: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl RaceHookGuard {
    fn install(point: RacePoint, callback: impl FnOnce() + Send + 'static) -> Self {
        // Poison-tolerant: a failing hooked test must not cascade into every
        // later hook/guard user via a poisoned mutex.
        let serial = RACE_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut slot = RACE_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(slot.is_none(), "a capability race hook is already installed");
        // Scope to the installing thread: hooked operations run on the test's
        // own thread, and this prevents concurrent tests from stealing the
        // hook when they hit the same race point.
        *slot = Some((point, std::thread::current().id(), Box::new(callback)));
        drop(slot);
        Self { _serial: serial }
    }
}

#[cfg(test)]
impl Drop for RaceHookGuard {
    fn drop(&mut self) {
        if let Some(lock) = RACE_HOOK.get() {
            *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
struct Openat2FallbackGuard {
    _serial: MutexGuard<'static, ()>,
    previous: bool,
}

#[cfg(all(test, target_os = "linux"))]
impl Openat2FallbackGuard {
    fn install() -> Self {
        let serial = RACE_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = FORCE_OPENAT2_FALLBACK.swap(true, Ordering::Relaxed);
        OPENAT2_ATTEMPTS.store(0, Ordering::Relaxed);
        FALLBACK_COMPONENT_OPENS.store(0, Ordering::Relaxed);
        Self {
            _serial: serial,
            previous,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
impl Drop for Openat2FallbackGuard {
    fn drop(&mut self) {
        FORCE_OPENAT2_FALLBACK.store(self.previous, Ordering::Relaxed);
    }
}

#[cfg(test)]
fn run_race_hook(point: RacePoint) {
    let lock = RACE_HOOK.get_or_init(|| Mutex::new(None));
    let callback = {
        let mut slot = lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match slot.take() {
            Some((expected, owner, callback))
                if expected == point && owner == std::thread::current().id() =>
            {
                Some(callback)
            }
            other => {
                *slot = other;
                None
            }
        }
    };
    if let Some(callback) = callback {
        callback();
    }
}


#[derive(Debug, thiserror::Error)]
pub enum CapFsError {
    #[error("invalid capability path: {0}")]
    InvalidPath(String),
    #[error("capability scope conflict: {0}")]
    ScopeConflict(String),
    #[error("path is outside every retained capability root: {0}")]
    OutsideScope(String),
    #[error("unsupported filesystem object: {0}")]
    UnsupportedObject(String),
    #[error("destination already exists: {0}")]
    AlreadyExists(String),
    #[error("atomic no-clobber rename is unavailable for this object/platform: {0}")]
    NoClobberUnavailable(String),
    #[error("filesystem capability contradiction: {0}")]
    Contradiction(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl CapFsError {
    /// The underlying OS errno when this error wraps an I/O failure.
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            CapFsError::Io(error) => error.raw_os_error(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameNoClobberOutcome {
    Renamed,
    CrossDevice,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    pub fn new(value: impl Into<String>) -> Result<Self, CapFsError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 160
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(CapFsError::InvalidPath(format!(
                "invalid scope id {value:?}"
            )));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ScopeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScopeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        ScopeId::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(PathBuf);

impl RelativePath {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, CapFsError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Ok(Self(PathBuf::new()));
        }
        if path.is_absolute() {
            return Err(CapFsError::InvalidPath(format!(
                "relative operand is absolute: {}",
                path.display()
            )));
        }
        let raw = path.as_os_str().as_bytes();
        if raw.split(|byte| *byte == b'/').any(|component| {
            component.is_empty() || component == b"." || component == b".."
        }) {
            return Err(CapFsError::InvalidPath(format!(
                "relative operand contains an empty or unstable component: {}",
                path.display()
            )));
        }
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    validate_component(name)?;
                    normalized.push(name);
                }
                Component::CurDir => {
                    return Err(CapFsError::InvalidPath(format!(
                        "relative operand contains '.': {}",
                        path.display()
                    )))
                }
                Component::ParentDir => {
                    return Err(CapFsError::InvalidPath(format!(
                        "relative operand contains '..': {}",
                        path.display()
                    )))
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(CapFsError::InvalidPath(format!(
                        "relative operand contains a root/prefix: {}",
                        path.display()
                    )))
                }
            }
        }
        Ok(Self(normalized))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.as_os_str().is_empty()
    }

    pub fn join(&self, child: impl AsRef<Path>) -> Result<Self, CapFsError> {
        Self::new(self.0.join(child))
    }

    pub fn parent(&self) -> Option<Self> {
        self.0.parent().and_then(|parent| Self::new(parent).ok())
    }

    pub fn file_name(&self) -> Option<&OsStr> {
        self.0.file_name()
    }
}

impl Serialize for RelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let path = PathBuf::deserialize(deserializer)?;
        RelativePath::new(path).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScopedPath {
    pub scope: ScopeId,
    pub relative: RelativePath,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRecord {
    pub id: ScopeId,
    /// Path whose descriptor was retained. This can be an existing ancestor
    /// when the configured logical root did not exist at acquisition time.
    pub acquisition_path: PathBuf,
    /// Configured root boundary. Scoped operands are always relative to this
    /// path, never to the broader acquisition ancestor.
    pub logical_path: PathBuf,
    pub base_relative: RelativePath,
    pub device: u64,
    pub inode: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_device: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_inode: Option<u64>,
    /// Random journal-bound authority for first-time logical-root publication.
    /// Existing roots do not need one. Missing roots are staged under a name
    /// derived from this token and carry the token in a private marker until
    /// their materialized device/inode is durable in the action journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization_token: Option<String>,
    /// Descriptor-relative bootstrap authority file used only while a
    /// Tonepoet-owned internal logical root has not yet reached its first
    /// durable journal generation. The name is deterministic and validated
    /// against the scope identity and relative root before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization_authority_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapFileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapMetadata {
    pub file_type: CapFileType,
    pub mode: u32,
    pub device: u64,
    pub inode: u64,
    pub length: u64,
    pub accessed_seconds: i64,
    pub accessed_nanos: i64,
    pub modified_seconds: i64,
    pub modified_nanos: i64,
    pub changed_seconds: i64,
    pub changed_nanos: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapEntryIdentity {
    pub file_type: CapFileType,
    pub device: u64,
    pub inode: u64,
}

pub fn metadata_for_open_file(file: &File) -> Result<CapMetadata, CapFsError> {
    fstat_fd(file.as_raw_fd()).map_err(Into::into)
}

impl CapMetadata {
    pub fn entry_identity(self) -> CapEntryIdentity {
        CapEntryIdentity {
            file_type: self.file_type,
            device: self.device,
            inode: self.inode,
        }
    }
}

fn same_directory_entry(left: CapMetadata, right: CapMetadata) -> bool {
    left.file_type == right.file_type
        && left.device == right.device
        && left.inode == right.inode
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub metadata: CapMetadata,
}

struct RetainedLogicalRoot {
    fd: OwnedFd,
    /// Descriptor for the top-level directory that owns the first-publication
    /// marker.  Retaining this separately keeps marker cleanup bound to the
    /// directory that was actually published even if its pathname is renamed
    /// or replaced before the next durable journal generation.
    marker_parent: Option<OwnedFd>,
    device: u64,
    inode: u64,
}

struct CapabilityRoot {
    id: ScopeId,
    acquisition_path: PathBuf,
    logical_path: PathBuf,
    base_relative: RelativePath,
    fd: OwnedFd,
    device: u64,
    inode: u64,
    materialization_token: Option<String>,
    materialization_authority_name: Option<OsString>,
    /// Tonepoet-private recovery/journal roots are recreated per run; their
    /// scope id may rebind to a fresh same-logical-path object.
    recoverable: bool,
    logical_root: Mutex<Option<RetainedLogicalRoot>>,
}

impl fmt::Debug for CapabilityRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityRoot")
            .field("id", &self.id)
            .field("acquisition_path", &self.acquisition_path)
            .field("logical_path", &self.logical_path)
            .field("base_relative", &self.base_relative)
            .field("device", &self.device)
            .field("inode", &self.inode)
            .finish_non_exhaustive()
    }
}

const DIRECTORY_CACHE_CAPACITY: usize = 128;
const MATERIALIZATION_STAGE_PREFIX: &str = ".tonepoet-root-stage-";
const MATERIALIZATION_MARKER: &str = ".tonepoet-root-owner";
const MATERIALIZATION_AUTHORITY_PREFIX: &str = ".tonepoet-root-authority-";

#[derive(Debug)]
struct CachedDirectory {
    fd: OwnedFd,
    device: u64,
    inode: u64,
}

#[derive(Debug, Default)]
struct DirectoryCache {
    entries: BTreeMap<(ScopeId, RelativePath), CachedDirectory>,
    order: VecDeque<(ScopeId, RelativePath)>,
}

impl DirectoryCache {
    fn duplicate(
        &mut self,
        key: &(ScopeId, RelativePath),
    ) -> io::Result<Option<(OwnedFd, u64, u64)>> {
        let Some(entry) = self.entries.get(key) else {
            return Ok(None);
        };
        let duplicate = duplicate_fd(entry.fd.as_raw_fd())?;
        let device = entry.device;
        let inode = entry.inode;
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
        Ok(Some((duplicate, device, inode)))
    }

    fn insert(
        &mut self,
        key: (ScopeId, RelativePath),
        fd: OwnedFd,
        metadata: CapMetadata,
    ) {
        self.order.retain(|candidate| candidate != &key);
        self.entries.insert(
            key.clone(),
            CachedDirectory {
                fd,
                device: metadata.device,
                inode: metadata.inode,
            },
        );
        self.order.push_back(key);
        while self.entries.len() > DIRECTORY_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn invalidate_subtree(&mut self, scope: &ScopeId, relative: &RelativePath) {
        self.entries.retain(|(candidate_scope, candidate), _| {
            candidate_scope != scope
                || !(candidate == relative
                    || candidate.as_path().starts_with(relative.as_path()))
        });
        self.order.retain(|(candidate_scope, candidate)| {
            candidate_scope != scope
                || !(candidate == relative
                    || candidate.as_path().starts_with(relative.as_path()))
        });
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[derive(Debug, Default)]
struct Registry {
    by_id: BTreeMap<ScopeId, Arc<CapabilityRoot>>,
    // Exact display roots. Resolution chooses the longest lexical prefix.
    by_path: HashMap<PathBuf, BTreeSet<ScopeId>>,
}


/// A directory object deliberately opened through a trusted configured route.
///
/// The configured route may contain symlinks. They are resolved exactly once
/// by `open_trusted`; all later authority operations are descriptor-relative
/// and use `O_NOFOLLOW` for children. This deliberately distinguishes a trusted
/// symlink used to acquire a configured root from an untrusted symlink found
/// beneath that root.
#[cfg(target_os = "linux")]
fn set_errno_zero() {
    // SAFETY: writes the calling thread's errno slot.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(target_os = "macos")]
fn set_errno_zero() {
    // SAFETY: writes the calling thread's errno slot.
    unsafe { *libc::__error() = 0 };
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn set_errno_zero() {}

#[derive(Debug)]
pub struct PinnedDirectoryCapability {
    directory: File,
    display_path: PathBuf,
    identity: CapEntryIdentity,
}

impl PinnedDirectoryCapability {
    pub fn open_trusted(path: impl AsRef<Path>) -> Result<Self, CapFsError> {
        let requested = normalize_absolute(path.as_ref())?;
        let resolved = fs::canonicalize(&requested)?;
        let fd = open_absolute_directory(&resolved)?;
        let metadata = fstat_fd(fd.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::InvalidPath(format!(
                "trusted capability root is not a directory: {}",
                requested.display()
            )));
        }
        Ok(Self {
            directory: File::from(fd),
            display_path: resolved,
            identity: metadata.entry_identity(),
        })
    }

    fn from_child(
        directory: OwnedFd,
        display_path: PathBuf,
    ) -> Result<Self, CapFsError> {
        let metadata = fstat_fd(directory.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::InvalidPath(format!(
                "capability child is not a directory: {}",
                display_path.display()
            )));
        }
        Ok(Self {
            directory: File::from(directory),
            display_path,
            identity: metadata.entry_identity(),
        })
    }

    pub fn display_path(&self) -> &Path {
        &self.display_path
    }

    pub fn identity(&self) -> CapEntryIdentity {
        self.identity
    }

    pub fn duplicate_file(&self) -> Result<File, CapFsError> {
        Ok(File::from(duplicate_fd(self.directory.as_raw_fd())?))
    }

    /// Duplicate this exact retained directory object without resolving its
    /// display pathname again. The clone carries the same stable device/inode
    /// authority and is suitable for injection into another capability
    /// registry under a durable logical path.
    pub fn try_clone(&self) -> Result<Self, CapFsError> {
        let duplicate = duplicate_fd(self.directory.as_raw_fd())?;
        let metadata = fstat_fd(duplicate.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory
            || metadata.entry_identity() != self.identity
        {
            return Err(CapFsError::Contradiction(format!(
                "duplicated directory capability changed identity: {}",
                self.display_path.display()
            )));
        }
        Ok(Self {
            directory: File::from(duplicate),
            display_path: self.display_path.clone(),
            identity: self.identity,
        })
    }

    /// Open a descendant directory through this retained directory object.
    /// Only normal relative components are accepted. When `create` is true,
    /// missing components are created and fsynced one at a time beneath the
    /// retained parent, so lexical parent replacement cannot redirect the
    /// operation.
    pub fn open_directory_descendant(
        &self,
        relative: &Path,
        create: bool,
        mode: u32,
    ) -> Result<Self, CapFsError> {
        if relative.is_absolute() {
            return Err(CapFsError::InvalidPath(format!(
                "capability descendant must be relative: {}",
                relative.display()
            )));
        }
        let mut current = self.try_clone()?;
        for component in relative.components() {
            let name = match component {
                Component::Normal(name) => name,
                Component::CurDir => continue,
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(CapFsError::InvalidPath(format!(
                        "capability descendant escapes retained root: {}",
                        relative.display()
                    )))
                }
            };
            current = current.open_directory_child(name, create, mode)?;
        }
        Ok(current)
    }

    /// Return a kernel-provided pathname anchored to this retained directory
    /// descriptor.  Unlike `display_path`, this route continues to name the
    /// exact opened directory object after the caller's lexical parent path is
    /// renamed, replaced, or remounted.
    ///
    /// Linux exposes open descriptors through procfs and macOS through fdescfs.
    /// The route is accepted only after opening it as a directory and proving
    /// that its device/inode identity matches the retained descriptor.  All
    /// publication paths derived from this anchor therefore remain bound to the
    /// capability rather than re-resolving the original output pathname.
    pub fn descriptor_path(&self) -> Result<PathBuf, CapFsError> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let path = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
        #[cfg(target_os = "macos")]
        let path = PathBuf::from(format!("/dev/fd/{}", self.directory.as_raw_fd()));
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            return Err(CapFsError::UnsupportedObject(
                "descriptor-backed publication paths are unavailable on this platform".to_string(),
            ));
        }

        // Probe through a child component, not only by reopening the fd node.
        // Publication appends album/temp/backup names to this route, so a
        // platform where `/dev/fd/N` can be duplicated but not traversed must
        // fail closed before any mutation begins.
        let traversal_path = path.join(".");
        let probe = File::open(&traversal_path)?;
        let observed = fstat_fd(probe.as_raw_fd())?;
        if observed.file_type != CapFileType::Directory
            || observed.entry_identity() != self.identity
        {
            return Err(CapFsError::Contradiction(format!(
                "descriptor namespace route does not identify retained directory: {}",
                path.display()
            )));
        }
        // Force one directory traversal now.  This rejects systems where the
        // descriptor namespace can duplicate an fd but cannot be used as a
        // directory path prefix (publication needs `anchor/child`).
        let _ = fs::read_dir(&traversal_path)?;
        Ok(path)
    }

    pub fn list_entries(&self) -> Result<Vec<(OsString, CapEntryIdentity)>, CapFsError> {
        let duplicate = duplicate_fd(self.directory.as_raw_fd())?;
        let raw = duplicate.into_raw_fd();
        // SAFETY: `raw` is a fresh duplicate owned by `fdopendir` on success.
        let directory = unsafe { libc::fdopendir(raw) };
        if directory.is_null() {
            // SAFETY: ownership was not transferred when fdopendir failed.
            unsafe { libc::close(raw) };
            return Err(io::Error::last_os_error().into());
        }
        // The duplicate shares its file offset with the retained descriptor's
        // whole dup family; a prior enumeration leaves it at end-of-directory
        // and a fresh fdopendir would then read zero entries. Always rewind.
        // SAFETY: `directory` is a live DIR pointer.
        unsafe { libc::rewinddir(directory) };
        let mut entries = Vec::new();
        loop {
            set_errno_zero();
            // SAFETY: `directory` remains valid until the single `closedir`
            // below; callers serialize authority mutation while inventorying.
            let entry = unsafe { libc::readdir(directory) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: closes both the DIR stream and its owned descriptor.
                unsafe { libc::closedir(directory) };
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error.into());
                }
                break;
            }
            // SAFETY: readdir returns a NUL-terminated d_name for the lifetime
            // of the next readdir call; copy it immediately.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            #[cfg(unix)]
            let name = OsString::from_vec(bytes.to_vec());
            #[cfg(not(unix))]
            let name = OsString::from(String::from_utf8_lossy(bytes).into_owned());
            let identity = fstatat_no_follow(self.directory.as_raw_fd(), &name)?.entry_identity();
            entries.push((name, identity));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    pub fn entry_identity(&self, name: &OsStr) -> Result<Option<CapEntryIdentity>, CapFsError> {
        validate_component(name)?;
        match fstatat_no_follow(self.directory.as_raw_fd(), name) {
            Ok(metadata) => Ok(Some(metadata.entry_identity())),
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn open_directory_child(
        &self,
        name: &OsStr,
        create: bool,
        mode: u32,
    ) -> Result<Self, CapFsError> {
        validate_component(name)?;
        if create {
            match mkdirat_component(self.directory.as_raw_fd(), name, mode) {
                Ok(()) => sync_fd_best_effort(self.directory.as_raw_fd())?,
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let directory = openat_owned(
            self.directory.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )?;
        Self::from_child(directory, self.display_path.join(name))
    }

    pub fn open_regular_child(
        &self,
        name: &OsStr,
        create: bool,
        mode: u32,
    ) -> Result<File, CapFsError> {
        validate_component(name)?;
        let mut flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if create {
            flags |= libc::O_CREAT;
        }
        let fd = openat_owned(self.directory.as_raw_fd(), name, flags, mode)?;
        let metadata = fstat_fd(fd.as_raw_fd())?;
        if metadata.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "capability authority child is not a regular file: {}",
                self.display_path.join(name).display()
            )));
        }
        Ok(File::from(fd))
    }


    pub fn read_regular_child_optional(
        &self,
        name: &OsStr,
    ) -> Result<Option<(Vec<u8>, CapEntryIdentity)>, CapFsError> {
        validate_component(name)?;
        let Some(identity) = self.entry_identity(name)? else {
            return Ok(None);
        };
        if identity.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "capability authority child is not a regular file: {}",
                self.display_path.join(name).display()
            )));
        }
        let mut file = self.open_regular_child(name, false, 0o600)?;
        let observed = fstat_fd(file.as_raw_fd())?.entry_identity();
        if observed != identity {
            return Err(CapFsError::Contradiction(format!(
                "capability authority child changed while opening: {}",
                self.display_path.join(name).display()
            )));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some((bytes, identity)))
    }

    pub fn write_regular_child_create_new_durable(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: u32,
    ) -> Result<CapEntryIdentity, CapFsError> {
        validate_component(name)?;
        let fd = openat_owned(
            self.directory.as_raw_fd(),
            name,
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW,
            mode,
        )?;
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        let identity = fstat_fd(file.as_raw_fd())?.entry_identity();
        self.sync()?;
        Ok(identity)
    }

    pub fn publish_regular_child_no_clobber(
        &self,
        source_name: &OsStr,
        destination_name: &OsStr,
        expected_source: CapEntryIdentity,
    ) -> Result<(), CapFsError> {
        validate_component(source_name)?;
        validate_component(destination_name)?;
        let source = self.entry_identity(source_name)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "capability publication source vanished: {}",
                self.display_path.join(source_name).display()
            ))
        })?;
        if source != expected_source || source.file_type != CapFileType::Regular {
            return Err(CapFsError::Contradiction(format!(
                "capability publication source changed: {}",
                self.display_path.join(source_name).display()
            )));
        }
        match platform_rename_no_clobber(
            self.directory.as_raw_fd(),
            source_name,
            self.directory.as_raw_fd(),
            destination_name,
        ) {
            Ok(()) => {
                self.sync()?;
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                Err(CapFsError::AlreadyExists(
                    self.display_path.join(destination_name).display().to_string(),
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn replace_regular_child(
        &self,
        source_name: &OsStr,
        destination_name: &OsStr,
        expected_source: CapEntryIdentity,
        expected_destination: Option<CapEntryIdentity>,
    ) -> Result<(), CapFsError> {
        validate_component(source_name)?;
        validate_component(destination_name)?;
        if self.entry_identity(source_name)? != Some(expected_source) {
            return Err(CapFsError::Contradiction(format!(
                "capability replacement source changed: {}",
                self.display_path.join(source_name).display()
            )));
        }
        if self.entry_identity(destination_name)? != expected_destination {
            return Err(CapFsError::Contradiction(format!(
                "capability replacement destination changed: {}",
                self.display_path.join(destination_name).display()
            )));
        }
        let source = os_cstring(source_name)?;
        let destination = os_cstring(destination_name)?;
        // SAFETY: both names are validated single components and both fds are
        // the retained directory object.
        if unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                source.as_ptr(),
                self.directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        self.sync()
    }

    pub fn remove_regular_child_if_identity(
        &self,
        name: &OsStr,
        expected: CapEntryIdentity,
    ) -> Result<bool, CapFsError> {
        validate_component(name)?;
        let Some(current) = self.entry_identity(name)? else {
            return Ok(false);
        };
        if current != expected || current.file_type != CapFileType::Regular {
            return Err(CapFsError::Contradiction(format!(
                "capability authority child changed before removal: {}",
                self.display_path.join(name).display()
            )));
        }
        unlinkat_component(self.directory.as_raw_fd(), name, false)?;
        sync_fd_best_effort(self.directory.as_raw_fd())?;
        Ok(true)
    }

    pub fn remove_empty_directory_child(&self, name: &OsStr) -> Result<bool, CapFsError> {
        validate_component(name)?;
        match unlinkat_component(self.directory.as_raw_fd(), name, true) {
            Ok(()) => {
                sync_fd_best_effort(self.directory.as_raw_fd())?;
                Ok(true)
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(false),
            Err(error) if error.raw_os_error() == Some(libc::ENOTEMPTY) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn sync(&self) -> Result<(), CapFsError> {
        match sync_fd(self.directory.as_raw_fd()) {
            Ok(()) => Ok(()),
            Err(error) if directory_sync_unsupported(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Default)]
pub struct CapabilityFilesystem {
    registry: Mutex<Registry>,
    directory_cache: Mutex<DirectoryCache>,
    #[cfg(test)]
    force_rename_exdev: AtomicBool,
}

impl CapabilityFilesystem {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn set_force_rename_exdev(&self, enabled: bool) {
        self.force_rename_exdev.store(enabled, Ordering::Relaxed);
    }

    /// Acquire `requested` or its nearest existing directory ancestor and bind
    /// it to `id`. Acquisition itself is a no-follow component walk from `/`.
    pub fn pin_root(
        &self,
        id: ScopeId,
        requested: impl AsRef<Path>,
    ) -> Result<ScopedPath, CapFsError> {
        let requested = normalize_absolute(requested.as_ref())?;
        {
            let registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&id) {
                if existing.logical_path != requested {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} was requested for a different logical root",
                        id.as_str()
                    )));
                }
                return Ok(ScopedPath {
                    scope: id,
                    relative: RelativePath::new(Path::new(""))?,
                });
            }
        }
        let (opened_path, remainder, fd) = open_nearest_existing_directory(&requested)?;
        let metadata = fstat_fd(fd.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::InvalidPath(format!(
                "capability root is not a directory: {}",
                opened_path.display()
            )));
        }

        let mut registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        let base_relative = RelativePath::new(remainder)?;
        if let Some(existing) = registry.by_id.get(&id) {
            if existing.device != metadata.device
                || existing.inode != metadata.inode
                || existing.logical_path != requested
                || existing.base_relative != base_relative
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} was rebound to a different capability boundary",
                    id.as_str()
                )));
            }
            return Ok(ScopedPath {
                scope: id,
                relative: RelativePath::new(Path::new(""))?,
            });
        }
        let materialization_token = if base_relative.is_empty() {
            None
        } else {
            Some(Uuid::new_v4().simple().to_string())
        };
        let root = Arc::new(CapabilityRoot {
            id: id.clone(),
            acquisition_path: opened_path,
            logical_path: requested.clone(),
            base_relative,
            fd,
            device: metadata.device,
            inode: metadata.inode,
            materialization_token,
            materialization_authority_name: None,
            recoverable: false,
            logical_root: Mutex::new(None),
        });
        registry.by_path.entry(requested).or_default().insert(id.clone());
        registry.by_id.insert(id.clone(), root);
        Ok(ScopedPath {
            scope: id,
            relative: RelativePath::new(Path::new(""))?,
        })
    }

    /// Determine the narrow first-publication boundary for a logical root.
    ///
    /// If the complete root already exists, the root itself is returned. If
    /// one or more components are missing, this returns the first missing
    /// component beneath the nearest existing no-follow ancestor. Calling
    /// this for every rendered destination before mutation causes sibling
    /// roots beneath one absent parent to converge on the same durable scope.
    /// The returned path is only a logical identity; `pin_root` performs the
    /// authoritative descriptor acquisition immediately afterward.
    pub fn first_materialization_boundary(
        requested: impl AsRef<Path>,
    ) -> Result<PathBuf, CapFsError> {
        let requested = normalize_absolute(requested.as_ref())?;
        let (opened_path, remainder, _fd) = open_nearest_existing_directory(&requested)?;
        let remainder = RelativePath::new(remainder)?;
        if remainder.is_empty() {
            return Ok(requested);
        }
        let first = normal_components(remainder.as_path())?
            .into_iter()
            .next()
            .ok_or_else(|| {
                CapFsError::Contradiction(
                    "non-empty materialization remainder had no component".to_string(),
                )
            })?;
        Ok(opened_path.join(first))
    }

    /// Acquire a Tonepoet-owned internal root and recover the narrow crash
    /// window before its first journal generation exists.  The immediate
    /// parent must already exist.  A published root is adopted only when its
    /// private owner marker contains one valid token; an unpublished stage is
    /// resumed only when exactly one token-named, token-marked stage exists.
    ///
    /// This must not be used for user-selected external roots: those require a
    /// token already bound into the durable action journal.
    pub fn pin_recoverable_internal_root(
        &self,
        id: ScopeId,
        requested: impl AsRef<Path>,
    ) -> Result<ScopedPath, CapFsError> {
        let requested = normalize_absolute(requested.as_ref())?;
        {
            let registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&id) {
                if existing.logical_path != requested {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} was requested for a different recoverable internal root",
                        id.as_str()
                    )));
                }
                return Ok(ScopedPath {
                    scope: id,
                    relative: RelativePath::new(Path::new(""))?,
                });
            }
        }

        let parent_path = match requested.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return self.pin_root(id, requested),
        };
        let name = requested.file_name().ok_or_else(|| {
            CapFsError::InvalidPath(format!(
                "recoverable internal root has no final component: {}",
                requested.display()
            ))
        })?;
        validate_component(name)?;
        let parent_fd = match open_absolute_directory(&parent_path) {
            Ok(fd) => fd,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                return self.pin_root(id, requested)
            }
            Err(error) => return Err(error.into()),
        };
        let parent_metadata = fstat_fd(parent_fd.as_raw_fd())?;
        let base_relative = RelativePath::new(Path::new(name))?;
        let authority_name = materialization_authority_name(&id, &base_relative);
        let authority_token = read_materialization_authority(
            parent_fd.as_raw_fd(),
            &authority_name,
        )?;

        let mut staged_tokens = Vec::new();
        for entry in read_directory_entries(duplicate_fd(parent_fd.as_raw_fd())?)? {
            let bytes = entry.name.as_bytes();
            let prefix = MATERIALIZATION_STAGE_PREFIX.as_bytes();
            if !bytes.starts_with(prefix) {
                continue;
            }
            let token = std::str::from_utf8(&bytes[prefix.len()..]).map_err(|_| {
                CapFsError::ScopeConflict(format!(
                    "non-UTF-8 interrupted internal-root stage beneath {}",
                    parent_path.display()
                ))
            })?;
            if !valid_materialization_token(token) {
                return Err(CapFsError::ScopeConflict(format!(
                    "malformed interrupted internal-root stage beneath {}: {}",
                    parent_path.display(),
                    entry.name.to_string_lossy()
                )));
            }
            if entry.metadata.file_type != CapFileType::Directory {
                return Err(CapFsError::ScopeConflict(format!(
                    "interrupted internal-root stage changed type beneath {}: {}",
                    parent_path.display(),
                    entry.name.to_string_lossy()
                )));
            }
            staged_tokens.push(token.to_string());
        }
        staged_tokens.sort();
        staged_tokens.dedup();

        let (materialization_token, logical_root) = match fstatat_no_follow(
            parent_fd.as_raw_fd(),
            name,
        ) {
            Ok(metadata) => {
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::ScopeConflict(format!(
                        "recoverable internal root changed type: {}",
                        requested.display()
                    )));
                }
                if !staged_tokens.is_empty() {
                    return Err(CapFsError::ScopeConflict(format!(
                        "published internal root coexists with interrupted staging beneath {}",
                        parent_path.display()
                    )));
                }
                let logical = openat_owned(
                    parent_fd.as_raw_fd(),
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?;
                match read_materialization_marker(logical.as_raw_fd())? {
                    None if authority_token.is_none() => return self.pin_root(id, requested),
                    None => {
                        return Err(CapFsError::ScopeConflict(format!(
                            "internal-root bootstrap authority remains but the published owner marker is absent: {}",
                            requested.display()
                        )))
                    }
                    Some(marker) => {
                        let marker = std::str::from_utf8(&marker).map_err(|_| {
                            CapFsError::ScopeConflict(
                                "recoverable internal-root marker is not UTF-8".to_string(),
                            )
                        })?;
                        if !valid_materialization_token(marker) {
                            return Err(CapFsError::ScopeConflict(
                                "recoverable internal-root marker has an invalid token".to_string(),
                            ));
                        }
                        let authority = authority_token.as_deref().ok_or_else(|| {
                            CapFsError::ScopeConflict(format!(
                                "published internal root has no durable bootstrap authority: {}",
                                requested.display()
                            ))
                        })?;
                        if authority != marker {
                            return Err(CapFsError::ScopeConflict(format!(
                                "internal-root bootstrap authority and owner marker disagree: {}",
                                requested.display()
                            )));
                        }
                        let observed = fstat_fd(logical.as_raw_fd())?;
                        (
                            marker.to_string(),
                            Some(RetainedLogicalRoot {
                                marker_parent: Some(duplicate_fd(logical.as_raw_fd())?),
                                fd: logical,
                                device: observed.device,
                                inode: observed.inode,
                            }),
                        )
                    }
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                let token = match authority_token {
                    Some(token) => token,
                    None if staged_tokens.is_empty() => {
                        let token = Uuid::new_v4().simple().to_string();
                        create_materialization_authority(
                            parent_fd.as_raw_fd(),
                            &authority_name,
                            &token,
                        )?;
                        token
                    }
                    None => {
                        return Err(CapFsError::ScopeConflict(format!(
                            "interrupted internal-root staging has no durable bootstrap authority beneath {}",
                            parent_path.display()
                        )))
                    }
                };
                if staged_tokens.iter().any(|candidate| candidate != &token) {
                    return Err(CapFsError::ScopeConflict(format!(
                        "internal-root staging does not match its durable bootstrap authority beneath {}",
                        parent_path.display()
                    )));
                }
                if staged_tokens.len() > 1 {
                    return Err(CapFsError::ScopeConflict(format!(
                        "multiple interrupted internal-root publications exist beneath {}",
                        parent_path.display()
                    )));
                }
                if staged_tokens.first() == Some(&token) {
                    let stage_name = materialization_stage_name(&token);
                    let stage = openat_owned(
                        parent_fd.as_raw_fd(),
                        &stage_name,
                        libc::O_RDONLY
                            | libc::O_DIRECTORY
                            | libc::O_CLOEXEC
                            | libc::O_NOFOLLOW,
                        0,
                    )?;
                    claim_or_verify_materialization_stage(stage.as_raw_fd(), &token)?;
                }
                (token, None)
            }
            Err(error) => return Err(error.into()),
        };

        let root = Arc::new(CapabilityRoot {
            id: id.clone(),
            acquisition_path: parent_path,
            logical_path: requested.clone(),
            base_relative,
            fd: parent_fd,
            device: parent_metadata.device,
            inode: parent_metadata.inode,
            materialization_token: Some(materialization_token),
            materialization_authority_name: Some(authority_name),
            recoverable: true,
            logical_root: Mutex::new(logical_root),
        });
        let mut registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        if let Some(existing) = registry.by_id.get(&id) {
            if existing.logical_path != requested
                || existing.acquisition_path != root.acquisition_path
                || existing.base_relative != root.base_relative
                || existing.device != root.device
                || existing.inode != root.inode
                || existing.materialization_token != root.materialization_token
                || existing.materialization_authority_name
                    != root.materialization_authority_name
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} was concurrently bound to different internal-root authority",
                    id.as_str()
                )));
            }
            return Ok(ScopedPath {
                scope: id,
                relative: RelativePath::new(Path::new(""))?,
            });
        }
        registry
            .by_path
            .entry(requested)
            .or_default()
            .insert(id.clone());
        registry.by_id.insert(id.clone(), root);
        Ok(ScopedPath {
            scope: id,
            relative: RelativePath::new(Path::new(""))?,
        })
    }

    /// Bind a durable logical root to an already-open exact directory object.
    /// The logical path is used in journals and diagnostics; all live I/O uses
    /// the duplicated descriptor. This deliberately separates durable identity
    /// from ephemeral `/proc/self/fd` or `/dev/fd` access routes.
    pub fn pin_existing_capability(
        &self,
        id: ScopeId,
        logical_root: impl AsRef<Path>,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), CapFsError> {
        self.pin_existing_capability_impl(id, logical_root, capability, false)
    }

    /// Like `pin_existing_capability`, but marks the scope as a
    /// Tonepoet-private recoverable root (recreated per run): later pins and
    /// prior-generation journal records may rebind it at the same logical
    /// path.
    pub fn pin_existing_recoverable_capability(
        &self,
        id: ScopeId,
        logical_root: impl AsRef<Path>,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), CapFsError> {
        self.pin_existing_capability_impl(id, logical_root, capability, true)
    }

    fn pin_existing_capability_impl(
        &self,
        id: ScopeId,
        logical_root: impl AsRef<Path>,
        capability: &PinnedDirectoryCapability,
        recoverable: bool,
    ) -> Result<(), CapFsError> {
        let logical_root = normalize_absolute(logical_root.as_ref())?;
        {
            let registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&id) {
                if existing.logical_path == logical_root
                    && existing.base_relative.is_empty()
                    && existing.device == capability.identity.device
                    && existing.inode == capability.identity.inode
                {
                    return Ok(());
                }
                if existing.logical_path == logical_root
                    && (existing.recoverable
                        || capability_object_is_unlinked(existing.fd.as_raw_fd()))
                {
                    // The held object was legitimately retired (unlinked);
                    // fall through and rebind below.
                } else {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} already identifies another directory: held {} (dev/ino {:?}/{:?}), requested {} (dev/ino {:?}/{:?})",
                    id.as_str(),
                    existing.logical_path.display(),
                    existing.device,
                    existing.inode,
                    logical_root.display(),
                    capability.identity.device,
                    capability.identity.inode,
                )));
                }
            }
        }

        let duplicated = duplicate_fd(capability.directory.as_raw_fd())?;
        let metadata = fstat_fd(duplicated.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory
            || metadata.entry_identity() != capability.identity
        {
            return Err(CapFsError::Contradiction(format!(
                "retained capability changed identity before scope binding: {}",
                capability.display_path.display()
            )));
        }
        let entry = Arc::new(CapabilityRoot {
            id: id.clone(),
            acquisition_path: capability.display_path.clone(),
            logical_path: logical_root.clone(),
            base_relative: RelativePath::new(Path::new(""))?,
            fd: duplicated,
            device: metadata.device,
            inode: metadata.inode,
            materialization_token: None,
            materialization_authority_name: None,
            recoverable,
            logical_root: Mutex::new(None),
        });
        let mut registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        if let Some(existing) = registry.by_id.get(&id) {
            if existing.logical_path == logical_root
                && existing.base_relative.is_empty()
                && existing.device == metadata.device
                && existing.inode == metadata.inode
            {
                return Ok(());
            }
            let rebindable = existing.logical_path == logical_root
                && (existing.recoverable
                    || capability_object_is_unlinked(existing.fd.as_raw_fd()));
            if !rebindable {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} was concurrently bound to another directory",
                    id.as_str()
                )));
            }
        }
        registry
            .by_path
            .entry(logical_root)
            .or_default()
            .insert(id.clone());
        registry.by_id.insert(id, entry);
        Ok(())
    }

    /// Bind `logical_root` beneath an already-retained ancestor capability.
    ///
    /// Unlike `pin_root`, this never reopens the ancestor through its lexical
    /// pathname. Existing descendants are walked no-follow from the retained
    /// descriptor. If part of the logical root does not yet exist, the nearest
    /// existing descriptor-relative ancestor is retained and the missing
    /// suffix receives the same journal-bound first-materialization authority
    /// used by ordinary capability roots.
    pub fn pin_descendant_capability(
        &self,
        id: ScopeId,
        logical_root: impl AsRef<Path>,
        ancestor_logical_root: impl AsRef<Path>,
        ancestor: &PinnedDirectoryCapability,
    ) -> Result<(), CapFsError> {
        let logical_root = normalize_absolute(logical_root.as_ref())?;
        let ancestor_logical_root = normalize_absolute(ancestor_logical_root.as_ref())?;
        let relative = logical_root.strip_prefix(&ancestor_logical_root).map_err(|_| {
            CapFsError::OutsideScope(format!(
                "logical root {} is not beneath retained ancestor {}",
                logical_root.display(),
                ancestor_logical_root.display()
            ))
        })?;
        RelativePath::new(relative)?;
        if relative.as_os_str().is_empty() {
            return self.pin_existing_capability(id, logical_root, ancestor);
        }

        let chain = open_existing_descendant_chain_from_capability(
            &ancestor_logical_root,
            ancestor.display_path(),
            ancestor.directory.as_raw_fd(),
            &logical_root,
        )?;
        {
            let registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&id) {
                let matching_acquisition = chain.iter().find(|(_, _, _, metadata)| {
                    metadata.device == existing.device && metadata.inode == existing.inode
                });
                let matches = matching_acquisition
                    .and_then(|(opened_logical, acquisition_path, _, _)| {
                        logical_root
                            .strip_prefix(opened_logical)
                            .ok()
                            .and_then(|relative| RelativePath::new(relative).ok())
                            .map(|base_relative| {
                                existing.logical_path == logical_root
                                    && existing.acquisition_path.as_path()
                                        == acquisition_path.as_path()
                                    && existing.base_relative == base_relative
                            })
                    })
                    .unwrap_or(false);
                if matches {
                    return Ok(());
                }
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} already identifies another retained descendant",
                    id.as_str()
                )));
            }
        }
        let (opened_logical, acquisition_path, fd, metadata) = chain
            .into_iter()
            .last()
            .ok_or_else(|| {
                CapFsError::Contradiction(
                    "retained descendant acquisition produced no directory".to_string(),
                )
            })?;
        let base_relative = RelativePath::new(
            logical_root.strip_prefix(&opened_logical).map_err(|_| {
                CapFsError::Contradiction(format!(
                    "retained acquisition directory is not an ancestor of {}",
                    logical_root.display()
                ))
            })?,
        )?;
        let materialization_token = if base_relative.is_empty() {
            None
        } else {
            Some(Uuid::new_v4().simple().to_string())
        };
        let root = Arc::new(CapabilityRoot {
            id: id.clone(),
            acquisition_path,
            logical_path: logical_root.clone(),
            base_relative,
            fd,
            device: metadata.device,
            inode: metadata.inode,
            materialization_token,
            materialization_authority_name: None,
            recoverable: false,
            logical_root: Mutex::new(None),
        });
        let mut registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        if let Some(existing) = registry.by_id.get(&id) {
            if existing.logical_path == logical_root
                && existing.acquisition_path == root.acquisition_path
                && existing.base_relative == root.base_relative
                && existing.device == root.device
                && existing.inode == root.inode
            {
                return Ok(());
            }
            return Err(CapFsError::ScopeConflict(format!(
                "scope {} was concurrently bound to another retained descendant",
                id.as_str()
            )));
        }
        registry
            .by_path
            .entry(logical_root)
            .or_default()
            .insert(id.clone());
        registry.by_id.insert(id, root);
        Ok(())
    }

    pub fn pin_existing_root(
        &self,
        id: ScopeId,
        root: impl AsRef<Path>,
    ) -> Result<(), CapFsError> {
        let root = normalize_absolute(root.as_ref())?;
        {
            let registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&id) {
                if existing.logical_path == root && existing.base_relative.is_empty() {
                    return Ok(());
                }
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} already identifies another logical root",
                    id.as_str()
                )));
            }
        }
        let fd = open_absolute_directory(&root)?;
        let metadata = fstat_fd(fd.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::InvalidPath(format!(
                "capability root is not a directory: {}",
                root.display()
            )));
        }
        let mut registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        if let Some(existing) = registry.by_id.get(&id) {
            if existing.device == metadata.device
                && existing.inode == metadata.inode
                && existing.logical_path == root
                && existing.base_relative.is_empty()
            {
                return Ok(());
            }
            if existing.logical_path == root
                && (existing.recoverable
                    || capability_object_is_unlinked(existing.fd.as_raw_fd()))
            {
                // Retired (unlinked) object at the same logical path:
                // rebind below.
            } else {
            return Err(CapFsError::ScopeConflict(format!(
                "scope {} already identifies another directory: held {} (dev/ino {:?}/{:?}), requested {} (dev/ino {:?}/{:?})",
                id.as_str(),
                existing.logical_path.display(),
                existing.device,
                existing.inode,
                root.display(),
                metadata.device,
                metadata.inode,
            )));
            }
        }
        let entry = Arc::new(CapabilityRoot {
            id: id.clone(),
            acquisition_path: root.clone(),
            logical_path: root.clone(),
            base_relative: RelativePath::new(Path::new(""))?,
            fd,
            device: metadata.device,
            inode: metadata.inode,
            materialization_token: None,
            materialization_authority_name: None,
            recoverable: false,
            logical_root: Mutex::new(None),
        });
        registry.by_path.entry(root).or_default().insert(id.clone());
        registry.by_id.insert(id, entry);
        Ok(())
    }

    /// Restore journal-bound root capabilities without using journal paths as
    /// lookup authority. `expected_roots` is re-derived from the current
    /// action configuration and context. For each scope we walk only the
    /// ancestor chain of that expected logical root, no-follow, and select the
    /// ancestor whose device/inode matches the durable record. A journal may
    /// describe the selected ancestor for diagnostics, but cannot redirect the
    /// open to another absolute path.
    pub fn restore_scope_records(
        &self,
        records: &[ScopeRecord],
        expected_roots: &[(ScopeId, PathBuf)],
    ) -> Result<(), CapFsError> {
        let expected: BTreeMap<ScopeId, PathBuf> = expected_roots
            .iter()
            .map(|(id, path)| Ok((id.clone(), normalize_absolute(path)?)))
            .collect::<Result<_, CapFsError>>()?;
        if expected.len() != expected_roots.len() || records.len() != expected.len() {
            return Err(CapFsError::ScopeConflict(
                "journal capability-root set does not match the configured scope set".to_string(),
            ));
        }
        let mut seen = BTreeMap::<ScopeId, ()>::new();
        for record in records {
            if seen.insert(record.id.clone(), ()).is_some() {
                return Err(CapFsError::ScopeConflict(format!(
                    "duplicate capability scope {} in journal",
                    record.id.as_str()
                )));
            }
            let logical_path = expected.get(&record.id).ok_or_else(|| {
                CapFsError::ScopeConflict(format!(
                    "journal contains an unexpected capability scope {}",
                    record.id.as_str()
                ))
            })?;
            if &normalize_absolute(&record.logical_path)? != logical_path {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} logical root does not match the configured root",
                    record.id.as_str()
                )));
            }
            let materialized_identity = match (record.materialized_device, record.materialized_inode) {
                (Some(device), Some(inode)) => Some((device, inode)),
                (None, None) => None,
                _ => {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} has a partial materialized-root identity",
                        record.id.as_str()
                    )))
                }
            };
            let base_relative = RelativePath::new(record.base_relative.as_path())?;
            let materialization_token = match record.materialization_token.as_deref() {
                Some(token) if valid_materialization_token(token) => Some(token.to_string()),
                Some(_) => {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} has an invalid materialization token",
                        record.id.as_str()
                    )))
                }
                None => None,
            };
            let materialization_authority_name = match
                record.materialization_authority_name.as_deref()
            {
                Some(name) => {
                    let name = OsString::from(name);
                    validate_component(&name)?;
                    let expected = materialization_authority_name(&record.id, &base_relative);
                    if name != expected {
                        return Err(CapFsError::ScopeConflict(format!(
                            "scope {} has a foreign materialization authority name",
                            record.id.as_str()
                        )));
                    }
                    Some(name)
                }
                None => None,
            };
            if materialization_authority_name.is_some()
                && (base_relative.is_empty() || materialization_token.is_none())
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} has contradictory bootstrap materialization authority",
                    record.id.as_str()
                )));
            }
            if base_relative.is_empty() && materialization_token.is_some() {
                return Err(CapFsError::ScopeConflict(format!(
                    "existing scope {} unexpectedly has materialization authority",
                    record.id.as_str()
                )));
            }
            if !base_relative.is_empty()
                && materialized_identity.is_none()
                && materialization_token.is_none()
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} predates durable logical-root materialization; administrative recovery is required",
                    record.id.as_str()
                )));
            }

            // Live action execution may already have installed an exact
            // retained descriptor for this logical scope. Validate that
            // descriptor directly against the durable record and do not
            // reopen the logical pathname merely to rediscover the same
            // object. This is the capability-bound path used after a parent
            // rename or mount/path replacement.
            {
                let registry = self.registry.lock().map_err(|_| {
                    CapFsError::Contradiction(
                        "capability registry mutex poisoned".to_string(),
                    )
                })?;
                if let Some(existing) = registry.by_id.get(&record.id) {
                    let existing_materialized = if existing.base_relative.is_empty() {
                        Some((existing.device, existing.inode))
                    } else {
                        existing
                            .logical_root
                            .lock()
                            .map_err(|_| {
                                CapFsError::Contradiction(
                                    "logical-root capability mutex poisoned".to_string(),
                                )
                            })?
                            .as_ref()
                            .map(|retained| (retained.device, retained.inode))
                    };
                    // Match the same durable transition accepted by
                    // `validate_scope_records`: a token-authenticated logical
                    // root may have materialized after the last journal
                    // generation.  An exact retained descriptor for that root
                    // is authoritative and must not be discarded merely
                    // because the previous record still contains `None`.
                    let materialized_matches = match materialized_identity {
                        Some(expected) => existing_materialized == Some(expected),
                        None => {
                            existing_materialized.is_none()
                                || (existing.materialization_token.is_some()
                                    && existing.base_relative == base_relative)
                        }
                    };
                    let matches = existing.acquisition_path
                        == normalize_absolute(&record.acquisition_path)?
                        && existing.logical_path == *logical_path
                        && existing.base_relative == base_relative
                        && existing.device == record.device
                        && existing.inode == record.inode
                        && existing.materialization_token == materialization_token
                        && existing.materialization_authority_name
                            == materialization_authority_name
                        && materialized_matches;
                    // Tonepoet-private recoverable roots (journal/recovery
                    // dirs) are recreated per run; a record from a prior
                    // generation may carry the retired object's identity.
                    // The retained current entry is authoritative.
                    let prior_generation_of_recoverable = existing.recoverable
                        && existing.logical_path == *logical_path;
                    if !matches && !prior_generation_of_recoverable {
                        return Err(CapFsError::ScopeConflict(format!(
                            "scope {} conflicts with an already retained capability",
                            record.id.as_str()
                        )));
                    }
                    continue;
                }
            }

            let retained_ancestor = {
                let registry = self.registry.lock().map_err(|_| {
                    CapFsError::Contradiction(
                        "capability registry mutex poisoned".to_string(),
                    )
                })?;
                registry
                    .by_id
                    .values()
                    .filter(|root| {
                        root.base_relative.is_empty()
                            && logical_path.starts_with(&root.logical_path)
                    })
                    .max_by_key(|root| root.logical_path.components().count())
                    .map(|root| -> Result<_, CapFsError> {
                        Ok((
                            root.logical_path.clone(),
                            root.acquisition_path.clone(),
                            duplicate_fd(root.fd.as_raw_fd())?,
                        ))
                    })
                    .transpose()?
            };
            let mut chain: Vec<(PathBuf, PathBuf, OwnedFd, CapMetadata)> =
                if let Some((ancestor_logical, ancestor_acquisition, ancestor_fd)) =
                    retained_ancestor
                {
                    open_existing_descendant_chain_from_capability(
                        &ancestor_logical,
                        &ancestor_acquisition,
                        ancestor_fd.as_raw_fd(),
                        logical_path,
                    )?
                } else {
                    open_existing_ancestor_chain(logical_path)?
                        .into_iter()
                        .map(|(path, fd, metadata)| (path.clone(), path, fd, metadata))
                        .collect()
                };
            let selected_index = chain
                .iter()
                .position(|(_, _, _, metadata)| {
                    metadata.file_type == CapFileType::Directory
                        && metadata.device == record.device
                        && metadata.inode == record.inode
                })
                .ok_or_else(|| {
                    CapFsError::ScopeConflict(format!(
                        "scope {} acquisition directory is no longer on the expected root's ancestor chain",
                        record.id.as_str()
                    ))
                })?;
            let (acquisition_logical_path, acquisition_path, fd, metadata) =
                chain.swap_remove(selected_index);
            let expected_base = logical_path
                .strip_prefix(&acquisition_logical_path)
                .map_err(|_| {
                CapFsError::ScopeConflict(format!(
                    "scope {} acquisition directory is not an ancestor of its logical root",
                    record.id.as_str()
                ))
            })?;
            if base_relative != RelativePath::new(expected_base)?
                || normalize_absolute(&record.acquisition_path)? != acquisition_path
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} has contradictory acquisition metadata",
                    record.id.as_str()
                )));
            }
            if let (Some(authority_name), Some(token)) = (
                materialization_authority_name.as_ref(),
                materialization_token.as_deref(),
            ) {
                if let Some(observed) =
                    read_materialization_authority(fd.as_raw_fd(), authority_name)?
                {
                    if observed.as_str() != token {
                        return Err(CapFsError::ScopeConflict(format!(
                            "scope {} bootstrap authority does not match its journal token",
                            record.id.as_str()
                        )));
                    }
                }
            }

            let logical_root = if base_relative.is_empty() {
                if materialized_identity != Some((metadata.device, metadata.inode)) {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} has a contradictory existing-root identity",
                        record.id.as_str()
                    )));
                }
                None
            } else if let Some((expected_device, expected_inode)) = materialized_identity {
                let mut current = duplicate_fd(fd.as_raw_fd())?;
                let mut marker_parent = None;
                for (index, component) in normal_components(base_relative.as_path())?
                    .into_iter()
                    .enumerate()
                {
                    current = openat_owned(
                        current.as_raw_fd(),
                        &component,
                        libc::O_RDONLY
                            | libc::O_DIRECTORY
                            | libc::O_CLOEXEC
                            | libc::O_NOFOLLOW,
                        0,
                    )?;
                    let observed = fstat_fd(current.as_raw_fd())?;
                    if observed.file_type != CapFileType::Directory {
                        return Err(CapFsError::ScopeConflict(format!(
                            "scope {} materialized root contains a non-directory component",
                            record.id.as_str()
                        )));
                    }
                    if index == 0 && materialization_token.is_some() {
                        marker_parent = Some(duplicate_fd(current.as_raw_fd())?);
                    }
                }
                let observed = fstat_fd(current.as_raw_fd())?;
                if observed.device != expected_device || observed.inode != expected_inode {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} materialized logical root was replaced",
                        record.id.as_str()
                    )));
                }
                Some(RetainedLogicalRoot {
                    fd: current,
                    marker_parent,
                    device: observed.device,
                    inode: observed.inode,
                })
            } else {
                let components = normal_components(base_relative.as_path())?;
                let first = components.first().ok_or_else(|| {
                    CapFsError::Contradiction("non-empty base path had no components".to_string())
                })?;
                let token = materialization_token.as_deref().ok_or_else(|| {
                    CapFsError::ScopeConflict(format!(
                        "scope {} has no first-publication authority",
                        record.id.as_str()
                    ))
                })?;
                match fstatat_no_follow(fd.as_raw_fd(), first) {
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                        let stage_name = materialization_stage_name(token);
                        match fstatat_no_follow(fd.as_raw_fd(), &stage_name) {
                            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
                            Ok(stage_metadata) if stage_metadata.file_type == CapFileType::Directory => {
                                let stage = openat_owned(
                                    fd.as_raw_fd(),
                                    &stage_name,
                                    libc::O_RDONLY
                                        | libc::O_DIRECTORY
                                        | libc::O_CLOEXEC
                                        | libc::O_NOFOLLOW,
                                    0,
                                )?;
                                claim_or_verify_materialization_stage(stage.as_raw_fd(), token)?;
                                None
                            }
                            Ok(_) => {
                                return Err(CapFsError::ScopeConflict(format!(
                                    "scope {} materialization staging changed type",
                                    record.id.as_str()
                                )))
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Ok(observed) if observed.file_type == CapFileType::Directory => {
                        let (marker_parent, current) = open_published_materialized_root(
                            fd.as_raw_fd(),
                            &components,
                            token,
                        )?;
                        let observed = fstat_fd(current.as_raw_fd())?;
                        Some(RetainedLogicalRoot {
                            fd: current,
                            marker_parent: Some(marker_parent),
                            device: observed.device,
                            inode: observed.inode,
                        })
                    }
                    Ok(_) => {
                        return Err(CapFsError::ScopeConflict(format!(
                            "scope {} logical root appeared as a non-directory",
                            record.id.as_str()
                        )))
                    }
                    Err(error) => return Err(error.into()),
                }
            };

            let mut registry = self.registry.lock().map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?;
            if let Some(existing) = registry.by_id.get(&record.id) {
                let existing_materialized = if existing.base_relative.is_empty() {
                    Some((existing.device, existing.inode))
                } else {
                    existing
                        .logical_root
                        .lock()
                        .map_err(|_| {
                            CapFsError::Contradiction(
                                "logical-root capability mutex poisoned".to_string(),
                            )
                        })?
                        .as_ref()
                        .map(|retained| (retained.device, retained.inode))
                };
                let mismatch = existing.acquisition_path != acquisition_path
                    || existing.logical_path != *logical_path
                    || existing.base_relative != base_relative
                    || existing.device != metadata.device
                    || existing.inode != metadata.inode
                    || match materialized_identity {
                        Some(expected) => existing_materialized != Some(expected),
                        None => existing_materialized.is_some() && materialization_token.is_none(),
                    }
                    || existing.materialization_token != materialization_token
                    || existing.materialization_authority_name
                        != materialization_authority_name;
                // See the retained-capability branch above: prior-generation
                // records of recoverable private roots defer to the retained
                // current entry.
                let prior_generation_of_recoverable =
                    existing.recoverable && existing.logical_path == *logical_path;
                if mismatch && !prior_generation_of_recoverable {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} conflicts with an already retained capability",
                        record.id.as_str()
                    )));
                }
                continue;
            }
            let root = Arc::new(CapabilityRoot {
                id: record.id.clone(),
                acquisition_path,
                logical_path: logical_path.clone(),
                base_relative,
                fd,
                device: metadata.device,
                inode: metadata.inode,
                materialization_token,
                materialization_authority_name,
                recoverable: false,
            logical_root: Mutex::new(logical_root),
            });
            registry
                .by_path
                .entry(logical_path.clone())
                .or_default()
                .insert(record.id.clone());
            registry.by_id.insert(record.id.clone(), root);
        }
        Ok(())
    }

    /// Read an absolute regular file through a one-shot `/` capability. This is
    /// used only to bootstrap a journal before its persisted scope records can
    /// be restored. It performs no mutation and follows no symlink component.
    pub fn bootstrap_read_absolute(path: &Path) -> Result<Option<Vec<u8>>, CapFsError> {
        let path = normalize_absolute(path)?;
        let filesystem = CapabilityFilesystem::new();
        let scope = ScopeId::new("bootstrap")?;
        filesystem.pin_existing_root(scope.clone(), Path::new("/"))?;
        let relative = path.strip_prefix(Path::new("/")).map_err(|_| {
            CapFsError::InvalidPath(format!("cannot bootstrap path {}", path.display()))
        })?;
        let scoped = ScopedPath {
            scope,
            relative: RelativePath::new(relative)?,
        };
        match filesystem.metadata_no_follow(&scoped)? {
            None => Ok(None),
            Some(metadata) if metadata.file_type == CapFileType::Regular => {
                filesystem.read_bytes(&scoped).map(Some)
            }
            Some(_) => Err(CapFsError::UnsupportedObject(format!(
                "bootstrap journal is not a regular file: {}",
                path.display()
            ))),
        }
    }

    pub fn scoped_path(&self, path: impl AsRef<Path>) -> Result<ScopedPath, CapFsError> {
        let path = normalize_absolute(path.as_ref())?;
        let registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        let mut selected: Option<(&PathBuf, &ScopeId)> = None;
        for (root_path, ids) in &registry.by_path {
            if path == *root_path || path.starts_with(root_path) {
                let id = ids.iter().next().ok_or_else(|| {
                    CapFsError::Contradiction("empty scope alias set".to_string())
                })?;
                let should_select = match selected {
                    None => true,
                    Some((current_path, current_id)) => {
                        let candidate_depth = root_path.components().count();
                        let current_depth = current_path.components().count();
                        candidate_depth > current_depth
                            || (candidate_depth == current_depth && id < current_id)
                    }
                };
                if should_select {
                    selected = Some((root_path, id));
                }
            }
        }
        let (root_path, id) = selected.ok_or_else(|| {
            CapFsError::OutsideScope(format!("{}", path.display()))
        })?;
        let relative = path.strip_prefix(root_path).map_err(|_| {
            CapFsError::OutsideScope(format!("{}", path.display()))
        })?;
        Ok(ScopedPath {
            scope: id.clone(),
            relative: RelativePath::new(relative)?,
        })
    }

    pub fn display_path(&self, path: &ScopedPath) -> Result<PathBuf, CapFsError> {
        let root = self.root(&path.scope)?;
        Ok(root.logical_path.join(path.relative.as_path()))
    }

    pub fn validate_scoped_path(&self, path: &ScopedPath) -> Result<(), CapFsError> {
        RelativePath::new(path.relative.as_path())?;
        let _ = self.root(&path.scope)?;
        Ok(())
    }

    pub fn scope_records(&self) -> Result<Vec<ScopeRecord>, CapFsError> {
        let registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        let mut records = Vec::with_capacity(registry.by_id.len());
        for root in registry.by_id.values() {
            let materialized = if root.base_relative.is_empty() {
                Some((root.device, root.inode))
            } else {
                root.logical_root
                    .lock()
                    .map_err(|_| {
                        CapFsError::Contradiction(
                            "logical-root capability mutex poisoned".to_string(),
                        )
                    })?
                    .as_ref()
                    .map(|logical| (logical.device, logical.inode))
            };
            records.push(ScopeRecord {
                id: root.id.clone(),
                acquisition_path: root.acquisition_path.clone(),
                logical_path: root.logical_path.clone(),
                base_relative: root.base_relative.clone(),
                device: root.device,
                inode: root.inode,
                materialized_device: materialized.map(|value| value.0),
                materialized_inode: materialized.map(|value| value.1),
                materialization_token: root.materialization_token.clone(),
                materialization_authority_name: root
                    .materialization_authority_name
                    .as_ref()
                    .map(|name| name.to_string_lossy().into_owned()),
            });
        }
        Ok(records)
    }

    /// Materialize a configured logical root after durable intent has been
    /// recorded. For a root that was absent at acquisition, every component is
    /// created exclusively and the resulting directory descriptor is retained
    /// for the lifetime of this filesystem instance.
    pub fn materialize_scope(&self, id: &ScopeId, mode: u32) -> Result<(), CapFsError> {
        let root = self.root(id)?;
        let descriptor = self.logical_root_fd(&root, Some(mode))?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "failed to materialize logical capability root: {}",
                root.logical_path.display()
            ))
        })?;
        sync_fd_best_effort(descriptor.as_raw_fd())
    }

    pub fn validate_scope_records(&self, expected: &[ScopeRecord]) -> Result<(), CapFsError> {
        let current = self.scope_records()?;
        let current: BTreeMap<_, _> = current
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        if current.len() != expected.len() {
            return Err(CapFsError::ScopeConflict(format!(
                "journal has {} capability roots but process acquired {}",
                expected.len(),
                current.len()
            )));
        }
        for record in expected {
            let observed = current.get(&record.id).ok_or_else(|| {
                CapFsError::ScopeConflict(format!("missing scope {}", record.id.as_str()))
            })?;
            let stable_fields_match = observed.id == record.id
                && observed.acquisition_path == record.acquisition_path
                && observed.logical_path == record.logical_path
                && observed.base_relative == record.base_relative
                && observed.device == record.device
                && observed.inode == record.inode
                && observed.materialization_token == record.materialization_token
                && observed.materialization_authority_name
                    == record.materialization_authority_name;
            let expected_materialized = match (
                record.materialized_device,
                record.materialized_inode,
            ) {
                (Some(device), Some(inode)) => Some((device, inode)),
                (None, None) => None,
                _ => {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} has a partial durable materialized identity",
                        record.id.as_str()
                    )))
                }
            };
            let observed_materialized = match (
                observed.materialized_device,
                observed.materialized_inode,
            ) {
                (Some(device), Some(inode)) => Some((device, inode)),
                (None, None) => None,
                _ => {
                    return Err(CapFsError::ScopeConflict(format!(
                        "scope {} has a partial observed materialized identity",
                        record.id.as_str()
                    )))
                }
            };
            // A token-authenticated root may have been published in the crash
            // window after the prior generation. Its identity is permitted to
            // advance from None to Some, and the next journal persistence will
            // make that identity durable before removing the owner marker.
            let materialized_matches = match expected_materialized {
                Some(expected) => observed_materialized == Some(expected),
                None => {
                    observed_materialized.is_none()
                        || (observed.materialization_token.is_some()
                            && observed.base_relative == record.base_relative)
                }
            };
            // Prior-generation records of a recoverable private root (the
            // per-run journal dir) legitimately differ in device/inode from
            // the retained current object at the same logical path.
            let prior_generation_of_recoverable = {
                let registry = self.registry.lock().map_err(|_| {
                    CapFsError::Contradiction(
                        "capability registry mutex poisoned".to_string(),
                    )
                })?;
                registry.by_id.get(&record.id).is_some_and(|entry| {
                    entry.recoverable && entry.logical_path == record.logical_path
                })
            };
            if (!stable_fields_match || !materialized_matches)
                && !prior_generation_of_recoverable
            {
                return Err(CapFsError::ScopeConflict(format!(
                    "scope {} no longer identifies the journal-bound directory",
                    record.id.as_str()
                )));
            }
        }
        Ok(())
    }

    fn logical_root_fd(
        &self,
        root: &CapabilityRoot,
        create_mode: Option<u32>,
    ) -> Result<Option<OwnedFd>, CapFsError> {
        if root.base_relative.is_empty() {
            return Ok(Some(duplicate_fd(root.fd.as_raw_fd())?));
        }

        let mut retained = root.logical_root.lock().map_err(|_| {
            CapFsError::Contradiction("logical-root capability mutex poisoned".to_string())
        })?;
        if let Some(existing) = retained.as_ref() {
            return Ok(Some(duplicate_fd(existing.fd.as_raw_fd())?));
        }

        let components = normal_components(root.base_relative.as_path())?;
        let first = components.first().ok_or_else(|| {
            CapFsError::Contradiction("non-empty base path had no components".to_string())
        })?;
        let token = root.materialization_token.as_deref();

        // Recovery may observe a root that was atomically published after the
        // previous journal generation but before its device/inode was durable.
        // The private owner marker is the journal-bound authority for adopting
        // that exact publication; an unrelated appeared pathname fails closed.
        match fstatat_no_follow(root.fd.as_raw_fd(), first) {
            Ok(metadata) => {
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::ScopeConflict(format!(
                        "logical capability root appeared as a non-directory: {}",
                        root.logical_path.display()
                    )));
                }
                let token = token.ok_or_else(|| {
                    CapFsError::ScopeConflict(format!(
                        "logical capability root appeared without durable materialization authority: {}",
                        root.logical_path.display()
                    ))
                })?;
                let (marker_parent, logical) =
                    open_published_materialized_root(root.fd.as_raw_fd(), &components, token)?;
                let observed = fstat_fd(logical.as_raw_fd())?;
                *retained = Some(RetainedLogicalRoot {
                    fd: duplicate_fd(logical.as_raw_fd())?,
                    marker_parent: Some(marker_parent),
                    device: observed.device,
                    inode: observed.inode,
                });
                return Ok(Some(logical));
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error.into()),
        }

        let Some(mode) = create_mode else {
            return Ok(None);
        };
        let token = token.ok_or_else(|| {
            CapFsError::ScopeConflict(format!(
                "missing logical root has no durable materialization token: {}",
                root.logical_path.display()
            ))
        })?;
        if !valid_materialization_token(token) {
            return Err(CapFsError::ScopeConflict(format!(
                "logical root has an invalid materialization token: {}",
                root.logical_path.display()
            )));
        }
        let stage_name = materialization_stage_name(token);

        let stage = match openat_owned(
            root.fd.as_raw_fd(),
            &stage_name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            Ok(stage) => {
                claim_or_verify_materialization_stage(stage.as_raw_fd(), token)?;
                stage
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                #[cfg(test)]
                run_race_hook(RacePoint::BeforeMkdir);
                mkdirat_component(root.fd.as_raw_fd(), &stage_name, 0o700)?;
                sync_fd_best_effort(root.fd.as_raw_fd())?;
                let stage = openat_owned(
                    root.fd.as_raw_fd(),
                    &stage_name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?;
                create_materialization_marker(stage.as_raw_fd(), token)?;
                stage
            }
            Err(error) => return Err(error.into()),
        };

        let mut current = duplicate_fd(stage.as_raw_fd())?;
        for component in components.iter().skip(1) {
            #[cfg(test)]
            run_race_hook(RacePoint::BeforeMkdir);
            match mkdirat_component(current.as_raw_fd(), component, mode) {
                Ok(()) => sync_fd_best_effort(current.as_raw_fd())?,
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
                Err(error) => return Err(error.into()),
            }
            let next = openat_owned(
                current.as_raw_fd(),
                component,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0,
            )?;
            let metadata = fstat_fd(next.as_raw_fd())?;
            if metadata.file_type != CapFileType::Directory {
                return Err(CapFsError::InvalidPath(format!(
                    "logical capability root staging contains a non-directory component: {}",
                    component.to_string_lossy()
                )));
            }
            current = next;
        }
        sync_fd_best_effort(current.as_raw_fd())?;

        #[cfg(test)]
        run_race_hook(RacePoint::BeforeRename);
        match platform_rename_no_clobber(
            root.fd.as_raw_fd(),
            &stage_name,
            root.fd.as_raw_fd(),
            first,
        ) {
            Ok(()) => sync_fd_best_effort(root.fd.as_raw_fd())?,
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                return Err(CapFsError::ScopeConflict(format!(
                    "logical capability root was concurrently published: {}",
                    root.logical_path.display()
                )))
            }
            Err(error) => return Err(error.into()),
        }

        let observed = fstat_fd(current.as_raw_fd())?;
        *retained = Some(RetainedLogicalRoot {
            fd: duplicate_fd(current.as_raw_fd())?,
            marker_parent: Some(duplicate_fd(stage.as_raw_fd())?),
            device: observed.device,
            inode: observed.inode,
        });
        Ok(Some(current))
    }

    /// Remove first-publication owner markers only after the materialized
    /// device/inode has been serialized and durably installed in the journal.
    /// A crash before this call leaves enough authority for deterministic
    /// adoption; a crash after it is safe because the identity is already
    /// durable.
    pub fn finalize_materialized_roots(&self) -> Result<(), CapFsError> {
        let roots: Vec<Arc<CapabilityRoot>> = self
            .registry
            .lock()
            .map_err(|_| {
                CapFsError::Contradiction("capability registry mutex poisoned".to_string())
            })?
            .by_id
            .values()
            .cloned()
            .collect();
        for root in roots {
            if root.base_relative.is_empty() {
                continue;
            }
            let Some(token) = root.materialization_token.as_deref() else {
                // Older already-materialized schema-3 roots have no marker.
                continue;
            };
            let mut retained = root.logical_root.lock().map_err(|_| {
                CapFsError::Contradiction("logical-root capability mutex poisoned".to_string())
            })?;
            let Some(retained_root) = retained.as_mut() else {
                continue;
            };

            if let Some(marker_parent) = retained_root.marker_parent.as_ref() {
                match read_materialization_marker(marker_parent.as_raw_fd())? {
                    None => {}
                    Some(observed) if observed == token.as_bytes() => {
                        let marker = OsStr::new(MATERIALIZATION_MARKER);
                        let metadata = fstatat_no_follow(marker_parent.as_raw_fd(), marker)?;
                        if metadata.file_type != CapFileType::Regular {
                            return Err(CapFsError::ScopeConflict(format!(
                                "logical-root owner marker changed type: {}",
                                root.logical_path.display()
                            )));
                        }
                        unlinkat_component(marker_parent.as_raw_fd(), marker, false)?;
                        sync_fd_best_effort(marker_parent.as_raw_fd())?;
                    }
                    Some(_) => {
                        return Err(CapFsError::ScopeConflict(format!(
                            "logical-root owner marker does not match its journal token: {}",
                            root.logical_path.display()
                        )))
                    }
                }
                // Dropping this descriptor after the identity-bearing journal
                // generation is durable cannot redirect any later cleanup.
                retained_root.marker_parent = None;
            }

            if let Some(authority_name) = root.materialization_authority_name.as_ref() {
                match read_materialization_authority(root.fd.as_raw_fd(), authority_name)? {
                    None => {}
                    Some(observed) if observed.as_str() == token => {
                        let metadata =
                            fstatat_no_follow(root.fd.as_raw_fd(), authority_name)?;
                        if metadata.file_type != CapFileType::Regular {
                            return Err(CapFsError::ScopeConflict(format!(
                                "logical-root bootstrap authority changed type: {}",
                                root.logical_path.display()
                            )));
                        }
                        unlinkat_component(root.fd.as_raw_fd(), authority_name, false)?;
                        sync_fd_best_effort(root.fd.as_raw_fd())?;
                    }
                    Some(_) => {
                        return Err(CapFsError::ScopeConflict(format!(
                            "logical-root bootstrap authority does not match its journal token: {}",
                            root.logical_path.display()
                        )))
                    }
                }
            }
        }
        Ok(())
    }

    pub fn metadata_no_follow(&self, path: &ScopedPath) -> Result<Option<CapMetadata>, CapFsError> {
        let root = self.root(&path.scope)?;
        if path.relative.is_empty() {
            return match self.logical_root_fd(&root, None)? {
                Some(fd) => Ok(Some(fstat_fd(fd.as_raw_fd())?)),
                None => Ok(None),
            };
        }
        let (parent, name) = match self.open_parent(&root, &path.relative, false) {
            Ok(value) => value,
            Err(CapFsError::Io(error)) if error.raw_os_error() == Some(libc::ENOENT) => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        match fstatat_no_follow(parent.as_raw_fd(), &name) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn entry_identity(
        &self,
        path: &ScopedPath,
    ) -> Result<Option<CapEntryIdentity>, CapFsError> {
        Ok(self.metadata_no_follow(path)?.map(CapMetadata::entry_identity))
    }

    pub fn read_bytes_with_identity_optional(
        &self,
        path: &ScopedPath,
    ) -> Result<Option<(Vec<u8>, CapEntryIdentity)>, CapFsError> {
        let Some(expected) = self.metadata_no_follow(path)? else {
            return Ok(None);
        };
        if expected.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "expected regular file: {}",
                self.display_path(path)?.display()
            )));
        }
        let mut file = self.open_regular_read_checked(path, expected)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let observed = fstat_fd(file.as_raw_fd())?;
        if !same_directory_entry(expected, observed) {
            return Err(CapFsError::Contradiction(format!(
                "regular file changed while it was read: {}",
                self.display_path(path)?.display()
            )));
        }
        Ok(Some((bytes, expected.entry_identity())))
    }

    pub fn read_bytes_with_identity(
        &self,
        path: &ScopedPath,
    ) -> Result<(Vec<u8>, CapEntryIdentity), CapFsError> {
        self.read_bytes_with_identity_optional(path)?.ok_or_else(|| {
            CapFsError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("regular file vanished: {}", self.display_path(path).map(|p| p.display().to_string()).unwrap_or_default()),
            ))
        })
    }

    pub fn open_regular_read(&self, path: &ScopedPath) -> Result<File, CapFsError> {
        let root = self.root(&path.scope)?;
        let (parent, name) = self.open_parent(&root, &path.relative, false)?;
        // O_NONBLOCK: a read-only open of a FIFO otherwise blocks until a
        // writer appears — a planted FIFO must fail closed, not hang the
        // pipeline. Cleared after the regular-file check; it has no effect on
        // regular-file reads but the returned handle should carry clean flags.
        let fd = openat_owned(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?;
        let metadata = fstat_fd(fd.as_raw_fd())?;
        if metadata.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "expected regular file: {}",
                self.display_path(path)?.display()
            )));
        }
        // SAFETY: fd is owned and valid; F_GETFL/F_SETFL on an owned fd.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            if flags >= 0 {
                let _ = libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
        Ok(File::from(fd))
    }

    pub fn open_regular_read_checked(
        &self,
        path: &ScopedPath,
        expected: CapMetadata,
    ) -> Result<File, CapFsError> {
        if expected.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "checked regular-file open received non-file identity: {}",
                self.display_path(path)?.display()
            )));
        }
        let root = self.root(&path.scope)?;
        let (parent, name) = self.open_parent(&root, &path.relative, false)?;
        let fd = openat_owned(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )?;
        let observed = fstat_fd(fd.as_raw_fd())?;
        if !same_directory_entry(expected, observed) {
            return Err(CapFsError::Contradiction(format!(
                "regular-file entry changed while it was opened: {}",
                self.display_path(path)?.display()
            )));
        }
        Ok(File::from(fd))
    }

    pub fn create_regular_exclusive(
        &self,
        path: &ScopedPath,
        mode: u32,
    ) -> Result<File, CapFsError> {
        let root = self.root(&path.scope)?;
        let (parent, name) = self.open_parent(&root, &path.relative, true)?;
        #[cfg(test)]
        run_race_hook(RacePoint::BeforeCreate);
        let fd = match openat_owned(
            parent.as_raw_fd(),
            &name,
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW,
            mode,
        ) {
            Ok(fd) => fd,
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                return Err(CapFsError::AlreadyExists(
                    self.display_path(path)?.display().to_string(),
                ))
            }
            Err(error) => return Err(error.into()),
        };
        Ok(File::from(fd))
    }

    pub fn read_bytes(&self, path: &ScopedPath) -> Result<Vec<u8>, CapFsError> {
        let mut file = self.open_regular_read(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn write_bytes_exclusive_durable(
        &self,
        path: &ScopedPath,
        bytes: &[u8],
        mode: u32,
    ) -> Result<(), CapFsError> {
        let mut file = self.create_regular_exclusive(path, mode)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        self.sync_parent(path)
    }

    pub fn mkdir_all(&self, path: &ScopedPath, mode: u32) -> Result<(), CapFsError> {
        let root = self.root(&path.scope)?;
        let mut current = self.logical_root_fd(&root, None)?.ok_or_else(|| {
            CapFsError::ScopeConflict(format!(
                "logical capability root must be materialized before child creation: {}",
                root.logical_path.display()
            ))
        })?;
        let mut current_relative = RelativePath(PathBuf::new());
        for component in normal_components(path.relative.as_path())? {
            #[cfg(test)]
            run_race_hook(RacePoint::BeforeMkdir);
            match mkdirat_component(current.as_raw_fd(), &component, mode) {
                Ok(()) => sync_fd_best_effort(current.as_raw_fd())?,
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
                Err(error) => return Err(error.into()),
            }
            let next = openat_owned(
                current.as_raw_fd(),
                &component,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0,
            )?;
            let metadata = fstat_fd(next.as_raw_fd())?;
            if metadata.file_type != CapFileType::Directory {
                return Err(CapFsError::InvalidPath(format!(
                    "mkdir traversal encountered non-directory: {}",
                    component.to_string_lossy()
                )));
            }
            current_relative = current_relative.join(&component)?;
            self.cache_directory(&root, &current_relative, next.as_raw_fd())?;
            current = next;
        }
        sync_fd_best_effort(current.as_raw_fd())?;
        if path.relative.is_empty() {
            return Ok(());
        }
        self.sync_parent(path)
    }

    pub fn enumerate(&self, path: &ScopedPath) -> Result<Vec<DirectoryEntry>, CapFsError> {
        let directory = self.open_directory(path)?;
        read_directory_entries(directory)
    }

    pub fn enumerate_checked(
        &self,
        path: &ScopedPath,
        expected: CapMetadata,
    ) -> Result<Vec<DirectoryEntry>, CapFsError> {
        if expected.file_type != CapFileType::Directory {
            return Err(CapFsError::UnsupportedObject(format!(
                "checked directory enumeration received non-directory identity: {}",
                self.display_path(path)?.display()
            )));
        }
        let directory = self.open_directory(path)?;
        let observed = fstat_fd(directory.as_raw_fd())?;
        if !same_directory_entry(expected, observed) {
            return Err(CapFsError::Contradiction(format!(
                "directory entry changed while it was opened: {}",
                self.display_path(path)?.display()
            )));
        }
        read_directory_entries(directory)
    }

    pub fn open_directory(&self, path: &ScopedPath) -> Result<OwnedFd, CapFsError> {
        let root = self.root(&path.scope)?;
        self.walk_directory(&root, &path.relative)
    }

    pub fn sync_directory(&self, path: &ScopedPath) -> Result<(), CapFsError> {
        let directory = self.open_directory(path)?;
        match sync_fd(directory.as_raw_fd()) {
            Ok(()) => Ok(()),
            Err(error) if directory_sync_unsupported(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn sync_parent(&self, path: &ScopedPath) -> Result<(), CapFsError> {
        let root = self.root(&path.scope)?;
        let parent = path
            .relative
            .parent()
            .unwrap_or_else(|| RelativePath(PathBuf::new()));
        let directory = self.walk_effective_directory(&root, &parent)?;
        match sync_fd(directory.as_raw_fd()) {
            Ok(()) => Ok(()),
            Err(error) if directory_sync_unsupported(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn replace_owned_regular(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        expected_source: CapEntryIdentity,
        expected_destination: Option<CapEntryIdentity>,
    ) -> Result<(), CapFsError> {
        let source_root = self.root(&source.scope)?;
        let destination_root = self.root(&destination.scope)?;
        let (source_parent, source_name) =
            self.open_parent(&source_root, &source.relative, false)?;
        let (destination_parent, destination_name) =
            self.open_parent(&destination_root, &destination.relative, true)?;
        let source_metadata = fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
        if source_metadata.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(
                "owned replacement source is not a regular file".to_string(),
            ));
        }
        if source_metadata.entry_identity() != expected_source {
            return Err(CapFsError::Contradiction(
                "owned replacement source changed after validation".to_string(),
            ));
        }
        let destination_metadata = match fstatat_no_follow(
            destination_parent.as_raw_fd(),
            &destination_name,
        ) {
            Ok(metadata) if metadata.file_type == CapFileType::Regular => Some(metadata),
            Ok(_) => {
                return Err(CapFsError::Contradiction(
                    "owned replacement destination is not a regular file".to_string(),
                ))
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
            Err(error) => return Err(error.into()),
        };
        if destination_metadata.map(CapMetadata::entry_identity) != expected_destination {
            return Err(CapFsError::Contradiction(
                "owned replacement destination changed after validation".to_string(),
            ));
        }

        #[cfg(test)]
        run_race_hook(RacePoint::BeforeRename);
        let source_immediately_before =
            fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
        if source_immediately_before.entry_identity() != expected_source {
            return Err(CapFsError::Contradiction(
                "owned replacement source changed immediately before publication".to_string(),
            ));
        }
        let destination_immediately_before = match fstatat_no_follow(
            destination_parent.as_raw_fd(),
            &destination_name,
        ) {
            Ok(metadata) if metadata.file_type == CapFileType::Regular => {
                Some(metadata.entry_identity())
            }
            Ok(_) => {
                return Err(CapFsError::Contradiction(
                    "owned replacement destination changed to a non-regular object".to_string(),
                ))
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => None,
            Err(error) => return Err(error.into()),
        };
        if destination_immediately_before != expected_destination {
            return Err(CapFsError::Contradiction(
                "owned replacement destination changed immediately before publication".to_string(),
            ));
        }
        if let Some(previous) = destination_metadata {
            match platform_rename_exchange(
                source_parent.as_raw_fd(),
                &source_name,
                destination_parent.as_raw_fd(),
                &destination_name,
            ) {
                Ok(()) => {}
                Err(error) if no_clobber_unavailable(&error) => {
                    return Err(CapFsError::NoClobberUnavailable(format!(
                        "atomic owned replacement for {}",
                        self.display_path(destination)?.display()
                    )))
                }
                Err(error) => return Err(error.into()),
            }
            let published =
                fstatat_no_follow(destination_parent.as_raw_fd(), &destination_name)?;
            let displaced = fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
            if !same_directory_entry(source_metadata, published)
                || !same_directory_entry(previous, displaced)
            {
                return Err(CapFsError::Contradiction(
                    "owned replacement operands changed during atomic exchange; both entries were preserved"
                        .to_string(),
                ));
            }
            sync_fd_best_effort(destination_parent.as_raw_fd())?;
            if source_parent.as_raw_fd() != destination_parent.as_raw_fd() {
                sync_fd_best_effort(source_parent.as_raw_fd())?;
            }
            // The source name now contains the verified previous owned file.
            // Revalidate immediately before unlink so a concurrent replacement
            // is preserved rather than deleted.
            #[cfg(test)]
            run_race_hook(RacePoint::BeforeUnlink);
            let before_unlink = fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
            if !same_directory_entry(previous, before_unlink) {
                return Err(CapFsError::Contradiction(
                    "displaced owned replacement file changed before cleanup".to_string(),
                ));
            }
            unlinkat_component(source_parent.as_raw_fd(), &source_name, false)?;
            sync_fd_best_effort(source_parent.as_raw_fd())?;
        } else {
            // Journal temporary and destination live in one directory by
            // construction; the test-only forced-EXDEV knob targets payload
            // moves and must not apply here.
            match self.try_rename_no_clobber_checked_impl(
                source,
                destination,
                Some(expected_source),
                false,
            )? {
                RenameNoClobberOutcome::Renamed => {}
                RenameNoClobberOutcome::CrossDevice => {
                    return Err(CapFsError::Contradiction(
                        "owned journal temporary and destination are unexpectedly cross-filesystem"
                            .to_string(),
                    ))
                }
            }
        }
        Ok(())
    }

    pub fn rename_no_clobber(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
    ) -> Result<(), CapFsError> {
        self.rename_no_clobber_checked(source, destination, None)
    }

    pub fn rename_no_clobber_checked(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        expected: Option<CapEntryIdentity>,
    ) -> Result<(), CapFsError> {
        // The hard-error variant is used only where source and destination
        // share a filesystem by construction (staging siblings, witnesses).
        // The test-only forced-EXDEV knob targets fallback-capable `try_*`
        // payload probes and must not apply here.
        match self.try_rename_no_clobber_checked_impl(source, destination, expected, false)? {
            RenameNoClobberOutcome::Renamed => Ok(()),
            RenameNoClobberOutcome::CrossDevice => Err(CapFsError::Io(
                io::Error::from_raw_os_error(libc::EXDEV),
            )),
        }
    }

    pub fn try_rename_no_clobber(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
    ) -> Result<RenameNoClobberOutcome, CapFsError> {
        self.try_rename_no_clobber_checked(source, destination, None)
    }

    pub fn try_rename_no_clobber_checked(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        expected: Option<CapEntryIdentity>,
    ) -> Result<RenameNoClobberOutcome, CapFsError> {
        self.try_rename_no_clobber_checked_impl(source, destination, expected, true)
    }

    fn try_rename_no_clobber_checked_impl(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        expected: Option<CapEntryIdentity>,
        honor_forced_exdev: bool,
    ) -> Result<RenameNoClobberOutcome, CapFsError> {
        #[cfg(not(test))]
        let _ = honor_forced_exdev;
        let source_root = self.root(&source.scope)?;
        let destination_root = self.root(&destination.scope)?;
        let (source_parent, source_name) = self.open_parent(&source_root, &source.relative, false)?;
        let (destination_parent, destination_name) =
            self.open_parent(&destination_root, &destination.relative, true)?;
        let source_metadata = fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
        if let Some(expected) = expected {
            if source_metadata.entry_identity() != expected {
                return Err(CapFsError::Contradiction(format!(
                    "rename source changed before descriptor-relative mutation: {}",
                    self.display_path(source)?.display()
                )));
            }
        }
        let destination_parent_metadata = fstat_fd(destination_parent.as_raw_fd())?;
        if source_metadata.file_type == CapFileType::Symlink
            || source_metadata.file_type == CapFileType::Other
        {
            return Err(CapFsError::UnsupportedObject(format!(
                "refusing to rename special/symlink object: {}",
                self.display_path(source)?.display()
            )));
        }
        if source_metadata.device != destination_parent_metadata.device {
            return Ok(RenameNoClobberOutcome::CrossDevice);
        }
        #[cfg(test)]
        if honor_forced_exdev && self.force_rename_exdev.load(Ordering::Relaxed) {
            return Ok(RenameNoClobberOutcome::CrossDevice);
        }
        #[cfg(test)]
        run_race_hook(RacePoint::BeforeRename);
        let source_immediately_before =
            fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
        if !same_directory_entry(source_metadata, source_immediately_before) {
            return Err(CapFsError::Contradiction(
                "rename source changed immediately before descriptor-relative mutation"
                    .to_string(),
            ));
        }
        match fstatat_no_follow(destination_parent.as_raw_fd(), &destination_name) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Ok(_) => {
                return Err(CapFsError::AlreadyExists(
                    self.display_path(destination)?.display().to_string(),
                ))
            }
            Err(error) => return Err(error.into()),
        }
        match platform_rename_no_clobber(
            source_parent.as_raw_fd(),
            &source_name,
            destination_parent.as_raw_fd(),
            &destination_name,
        ) {
            Ok(()) => {
                let published =
                    fstatat_no_follow(destination_parent.as_raw_fd(), &destination_name)?;
                if !same_directory_entry(source_metadata, published) {
                    return Err(CapFsError::Contradiction(
                        "rename source changed between validation and rename".to_string(),
                    ));
                }
                sync_fd_best_effort(source_parent.as_raw_fd())?;
                if source_parent.as_raw_fd() != destination_parent.as_raw_fd() {
                    sync_fd_best_effort(destination_parent.as_raw_fd())?;
                }
                self.clear_directory_cache()?;
                Ok(RenameNoClobberOutcome::Renamed)
            }
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                Ok(RenameNoClobberOutcome::CrossDevice)
            }
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Err(
                CapFsError::AlreadyExists(self.display_path(destination)?.display().to_string()),
            ),
            Err(error) if no_clobber_unavailable(&error) => {
                if source_metadata.file_type == CapFileType::Regular {
                    // linkat is atomic create-if-absent. Only unlink the source
                    // after the destination link and destination directory are
                    // durable. This fallback never overwrites.
                    #[cfg(test)]
                    run_race_hook(RacePoint::BeforeLink);
                    let source_before_link =
                        fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
                    if !same_directory_entry(source_metadata, source_before_link) {
                        return Err(CapFsError::Contradiction(
                            "link-based rename source changed immediately before publication"
                                .to_string(),
                        ));
                    }
                    match linkat_no_follow(
                        source_parent.as_raw_fd(),
                        &source_name,
                        destination_parent.as_raw_fd(),
                        &destination_name,
                    ) {
                        Ok(()) => {}
                        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                            return Ok(RenameNoClobberOutcome::CrossDevice)
                        }
                        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                            return Err(CapFsError::AlreadyExists(
                                self.display_path(destination)?.display().to_string(),
                            ))
                        }
                        Err(error) => return Err(error.into()),
                    }
                    let published =
                        fstatat_no_follow(destination_parent.as_raw_fd(), &destination_name)?;
                    if !same_directory_entry(source_metadata, published) {
                        return Err(CapFsError::Contradiction(
                            "link-based rename source changed during publication".to_string(),
                        ));
                    }
                    sync_fd_best_effort(destination_parent.as_raw_fd())?;
                    #[cfg(test)]
                    run_race_hook(RacePoint::BeforeUnlink);
                    let current_source =
                        fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
                    if !same_directory_entry(source_metadata, current_source) {
                        return Err(CapFsError::Contradiction(
                            "link-based rename source changed before removal".to_string(),
                        ));
                    }
                    unlinkat_component(source_parent.as_raw_fd(), &source_name, false)?;
                    sync_fd_best_effort(source_parent.as_raw_fd())?;
                    self.clear_directory_cache()?;
                    Ok(RenameNoClobberOutcome::Renamed)
                } else {
                    Err(CapFsError::NoClobberUnavailable(format!(
                        "directory {}",
                        self.display_path(source)?.display()
                    )))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Publish a regular file without removing the temporary hard-link source;
    /// publish a directory by no-clobber rename.
    pub fn publish_no_clobber(
        &self,
        temporary: &ScopedPath,
        destination: &ScopedPath,
    ) -> Result<(), CapFsError> {
        let metadata = self.metadata_no_follow(temporary)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "publication temporary vanished: {}",
                self.display_path(temporary).map(|p| p.display().to_string()).unwrap_or_default()
            ))
        })?;
        match metadata.file_type {
            CapFileType::Regular => {
                let source_root = self.root(&temporary.scope)?;
                let destination_root = self.root(&destination.scope)?;
                let (source_parent, source_name) =
                    self.open_parent(&source_root, &temporary.relative, false)?;
                let (destination_parent, destination_name) =
                    self.open_parent(&destination_root, &destination.relative, true)?;
                #[cfg(test)]
                run_race_hook(RacePoint::BeforeLink);
                let source_immediately_before =
                    fstatat_no_follow(source_parent.as_raw_fd(), &source_name)?;
                if !same_directory_entry(metadata, source_immediately_before) {
                    return Err(CapFsError::Contradiction(
                        "publication temporary changed immediately before link".to_string(),
                    ));
                }
                match linkat_no_follow(
                    source_parent.as_raw_fd(),
                    &source_name,
                    destination_parent.as_raw_fd(),
                    &destination_name,
                ) {
                    Ok(()) => {
                        let published =
                            fstatat_no_follow(destination_parent.as_raw_fd(), &destination_name)?;
                        if !same_directory_entry(metadata, published) {
                            return Err(CapFsError::Contradiction(
                                "publication temporary changed during link".to_string(),
                            ));
                        }
                        sync_fd_best_effort(destination_parent.as_raw_fd())?;
                        Ok(())
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Err(
                        CapFsError::AlreadyExists(
                            self.display_path(destination)?.display().to_string(),
                        ),
                    ),
                    Err(error) => Err(error.into()),
                }
            }
            CapFileType::Directory => self.rename_no_clobber(temporary, destination),
            CapFileType::Symlink | CapFileType::Other => Err(CapFsError::UnsupportedObject(
                format!(
                    "publication temporary has unsupported type: {}",
                    self.display_path(temporary)?.display()
                ),
            )),
        }
    }

    pub fn remove_tree_matching(
        &self,
        path: &ScopedPath,
        expected: CapEntryIdentity,
    ) -> Result<(), CapFsError> {
        let metadata = self.metadata_no_follow(path)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "owned object vanished before checked removal: {}",
                self.display_path(path).map(|p| p.display().to_string()).unwrap_or_default()
            ))
        })?;
        if metadata.entry_identity() != expected {
            return Err(CapFsError::Contradiction(format!(
                "owned object changed before checked removal: {}",
                self.display_path(path)?.display()
            )));
        }
        self.remove_tree_checked(path, metadata)
    }

    pub fn remove_tree(&self, path: &ScopedPath) -> Result<(), CapFsError> {
        let metadata = match self.metadata_no_follow(path)? {
            Some(metadata) => metadata,
            None => return Ok(()),
        };
        self.remove_tree_checked(path, metadata)
    }

    fn remove_tree_checked(
        &self,
        path: &ScopedPath,
        expected: CapMetadata,
    ) -> Result<(), CapFsError> {
        match expected.file_type {
            CapFileType::Regular => self.unlink_checked(path, false, expected),
            CapFileType::Directory => {
                let mut entries = self.enumerate_checked(path, expected)?;
                entries.sort_by(|left, right| left.name.cmp(&right.name));
                for entry in entries {
                    let child = ScopedPath {
                        scope: path.scope.clone(),
                        relative: path.relative.join(&entry.name)?,
                    };
                    match entry.metadata.file_type {
                        CapFileType::Regular => {
                            self.unlink_checked(&child, false, entry.metadata)?
                        }
                        CapFileType::Directory => {
                            self.remove_tree_checked(&child, entry.metadata)?
                        }
                        CapFileType::Symlink | CapFileType::Other => {
                            return Err(CapFsError::UnsupportedObject(format!(
                                "journal-owned tree contains special/symlink object: {}",
                                self.display_path(&child)?.display()
                            )))
                        }
                    }
                }
                self.unlink_checked(path, true, expected)
            }
            CapFileType::Symlink | CapFileType::Other => Err(CapFsError::Contradiction(
                format!(
                    "refusing to remove special/symlink object: {}",
                    self.display_path(path)?.display()
                ),
            )),
        }
    }

    pub fn copy_object(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        include_hidden: bool,
    ) -> Result<(), CapFsError> {
        let source_metadata = self.metadata_no_follow(source)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "copy source vanished: {}",
                self.display_path(source).map(|p| p.display().to_string()).unwrap_or_default()
            ))
        })?;
        match source_metadata.file_type {
            CapFileType::Regular => self.copy_regular(source, destination, source_metadata),
            CapFileType::Directory => {
                let destination_metadata =
                    self.mkdir_exact(destination, source_metadata.mode & 0o7777)?;
                let mut pending_metadata = vec![(
                    destination.clone(),
                    source_metadata,
                    destination_metadata,
                )];
                let mut stack = vec![(source.clone(), destination.clone(), source_metadata)];
                while let Some((source_dir, destination_dir, expected_directory)) = stack.pop() {
                    let mut entries = self.enumerate_checked(&source_dir, expected_directory)?;
                    entries.sort_by(|left, right| left.name.cmp(&right.name));
                    for entry in entries {
                        if !include_hidden && is_hidden(&entry.name) {
                            continue;
                        }
                        let child_source = ScopedPath {
                            scope: source_dir.scope.clone(),
                            relative: source_dir.relative.join(&entry.name)?,
                        };
                        let child_destination = ScopedPath {
                            scope: destination_dir.scope.clone(),
                            relative: destination_dir.relative.join(&entry.name)?,
                        };
                        match entry.metadata.file_type {
                            CapFileType::Regular => {
                                self.copy_regular(&child_source, &child_destination, entry.metadata)?
                            }
                            CapFileType::Directory => {
                                let child_destination_metadata = self.mkdir_exact(
                                    &child_destination,
                                    entry.metadata.mode & 0o7777,
                                )?;
                                pending_metadata.push((
                                    child_destination.clone(),
                                    entry.metadata,
                                    child_destination_metadata,
                                ));
                                stack.push((child_source, child_destination, entry.metadata));
                            }
                            CapFileType::Symlink | CapFileType::Other => {
                                return Err(CapFsError::UnsupportedObject(format!(
                                    "copy tree contains special/symlink object: {}",
                                    self.display_path(&child_source)?.display()
                                )))
                            }
                        }
                    }
                }
                pending_metadata.sort_by_key(|(path, _, _)| {
                    std::cmp::Reverse(path.relative.as_path().components().count())
                });
                for (directory, source_metadata, destination_metadata) in pending_metadata {
                    self.apply_metadata_checked(
                        &directory,
                        source_metadata,
                        destination_metadata,
                    )?;
                    self.sync_directory(&directory)?;
                }
                self.sync_parent(destination)
            }
            CapFileType::Symlink | CapFileType::Other => Err(CapFsError::UnsupportedObject(
                format!(
                    "copy source has unsupported type: {}",
                    self.display_path(source)?.display()
                ),
            )),
        }
    }


    /// Reapply the copy contract's mode and timestamps from `source` to an
    /// existing content-equivalent destination. Every object is opened
    /// descriptor-relative with O_NOFOLLOW and revalidated against the
    /// directory entry observed during the paired traversal. The operation is
    /// idempotent and may safely be repeated after a crash.
    pub fn repair_copy_metadata(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        include_hidden: bool,
    ) -> Result<(), CapFsError> {
        let source_root = self.metadata_no_follow(source)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "copy metadata source vanished: {}",
                self.display_path(source).map(|p| p.display().to_string()).unwrap_or_default()
            ))
        })?;
        let destination_root = self.metadata_no_follow(destination)?.ok_or_else(|| {
            CapFsError::Contradiction(format!(
                "copy metadata destination vanished: {}",
                self.display_path(destination).map(|p| p.display().to_string()).unwrap_or_default()
            ))
        })?;
        if source_root.file_type != destination_root.file_type {
            return Err(CapFsError::Contradiction(format!(
                "copy metadata source/destination types differ: {} -> {}",
                self.display_path(source)?.display(),
                self.display_path(destination)?.display()
            )));
        }

        let mut pairs = vec![(
            source.clone(),
            destination.clone(),
            source_root,
            destination_root,
        )];
        if source_root.file_type == CapFileType::Directory {
            let mut stack = vec![(source.clone(), destination.clone(), source_root, destination_root)];
            while let Some((source_dir, destination_dir, expected_source_dir, expected_destination_dir)) = stack.pop() {
                let mut source_entries = self.enumerate_checked(&source_dir, expected_source_dir)?;
                let mut destination_entries = self.enumerate_checked(&destination_dir, expected_destination_dir)?;
                if !include_hidden {
                    source_entries.retain(|entry| !is_hidden(&entry.name));
                    destination_entries.retain(|entry| !is_hidden(&entry.name));
                }
                source_entries.sort_by(|left, right| left.name.cmp(&right.name));
                destination_entries.sort_by(|left, right| left.name.cmp(&right.name));
                if source_entries.len() != destination_entries.len() {
                    return Err(CapFsError::Contradiction(format!(
                        "copy metadata trees differ while pairing {} -> {}",
                        self.display_path(&source_dir)?.display(),
                        self.display_path(&destination_dir)?.display()
                    )));
                }
                for (source_entry, destination_entry) in
                    source_entries.into_iter().zip(destination_entries)
                {
                    if source_entry.name != destination_entry.name
                        || source_entry.metadata.file_type != destination_entry.metadata.file_type
                    {
                        return Err(CapFsError::Contradiction(format!(
                            "copy metadata trees have different entries under {} -> {}",
                            self.display_path(&source_dir)?.display(),
                            self.display_path(&destination_dir)?.display()
                        )));
                    }
                    match source_entry.metadata.file_type {
                        CapFileType::Regular | CapFileType::Directory => {}
                        CapFileType::Symlink | CapFileType::Other => {
                            return Err(CapFsError::UnsupportedObject(format!(
                                "copy metadata tree contains special/symlink object: {}",
                                source_entry.name.to_string_lossy()
                            )))
                        }
                    }
                    let child_source = ScopedPath {
                        scope: source_dir.scope.clone(),
                        relative: source_dir.relative.join(&source_entry.name)?,
                    };
                    let child_destination = ScopedPath {
                        scope: destination_dir.scope.clone(),
                        relative: destination_dir.relative.join(&destination_entry.name)?,
                    };
                    pairs.push((
                        child_source.clone(),
                        child_destination.clone(),
                        source_entry.metadata,
                        destination_entry.metadata,
                    ));
                    if source_entry.metadata.file_type == CapFileType::Directory {
                        stack.push((
                            child_source,
                            child_destination,
                            source_entry.metadata,
                            destination_entry.metadata,
                        ));
                    }
                }
            }
        } else if source_root.file_type != CapFileType::Regular {
            return Err(CapFsError::UnsupportedObject(format!(
                "copy metadata source has unsupported type: {}",
                self.display_path(source)?.display()
            )));
        }

        // Children before parents preserves copied directory timestamps after
        // metadata changes to descendants.
        pairs.sort_by_key(|(_, path, _, _)| {
            std::cmp::Reverse(path.relative.as_path().components().count())
        });
        for (_, destination_path, source_metadata, expected_destination) in pairs {
            self.apply_any_metadata_checked(
                &destination_path,
                source_metadata,
                expected_destination,
            )?;
        }
        self.sync_parent(destination)
    }

    fn copy_regular(
        &self,
        source: &ScopedPath,
        destination: &ScopedPath,
        metadata: CapMetadata,
    ) -> Result<(), CapFsError> {
        let mut input = self.open_regular_read_checked(source, metadata)?;
        let mut output = self.create_regular_exclusive(destination, metadata.mode & 0o7777)?;
        io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        apply_fd_metadata(output.as_raw_fd(), metadata)?;
        output.sync_all()?;
        self.sync_parent(destination)
    }

    fn apply_metadata_checked(
        &self,
        path: &ScopedPath,
        source_metadata: CapMetadata,
        expected_destination: CapMetadata,
    ) -> Result<(), CapFsError> {
        self.apply_any_metadata_checked(path, source_metadata, expected_destination)
    }

    fn apply_any_metadata_checked(
        &self,
        path: &ScopedPath,
        source_metadata: CapMetadata,
        expected_destination: CapMetadata,
    ) -> Result<(), CapFsError> {
        if source_metadata.file_type != expected_destination.file_type
            || !matches!(source_metadata.file_type, CapFileType::Regular | CapFileType::Directory)
        {
            return Err(CapFsError::Contradiction(format!(
                "metadata repair received incompatible object types: {}",
                self.display_path(path)?.display()
            )));
        }
        let root = self.root(&path.scope)?;
        let (parent, name) = self.open_parent(&root, &path.relative, false)?;
        let before_open = fstatat_no_follow(parent.as_raw_fd(), &name)?;
        if !same_directory_entry(expected_destination, before_open) {
            return Err(CapFsError::Contradiction(format!(
                "copied object changed before metadata finalization: {}",
                self.display_path(path)?.display()
            )));
        }
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if expected_destination.file_type == CapFileType::Directory {
            flags |= libc::O_DIRECTORY;
        }
        let fd = openat_owned(parent.as_raw_fd(), &name, flags, 0)?;
        let opened = fstat_fd(fd.as_raw_fd())?;
        if !same_directory_entry(expected_destination, opened) {
            return Err(CapFsError::Contradiction(format!(
                "copied object changed while opening for metadata finalization: {}",
                self.display_path(path)?.display()
            )));
        }
        apply_fd_metadata(fd.as_raw_fd(), source_metadata)?;
        sync_fd_best_effort(fd.as_raw_fd())
    }

    fn mkdir_exact(&self, path: &ScopedPath, mode: u32) -> Result<CapMetadata, CapFsError> {
        let root = self.root(&path.scope)?;
        let effective = path.relative.clone();
        let (parent, name) = self.open_parent(&root, &path.relative, true)?;
        match mkdirat_component(parent.as_raw_fd(), &name, mode) {
            Ok(()) => {
                let directory = openat_owned(
                    parent.as_raw_fd(),
                    &name,
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0,
                )?;
                let metadata = fstat_fd(directory.as_raw_fd())?;
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::Contradiction(format!(
                        "new copy directory was replaced before it could be retained: {}",
                        self.display_path(path)?.display()
                    )));
                }
                self.cache_directory(&root, &effective, directory.as_raw_fd())?;
                sync_fd_best_effort(parent.as_raw_fd())?;
                Ok(metadata)
            }
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Err(
                CapFsError::AlreadyExists(self.display_path(path)?.display().to_string()),
            ),
            Err(error) => Err(error.into()),
        }
    }

    fn unlink_checked(
        &self,
        path: &ScopedPath,
        directory: bool,
        expected: CapMetadata,
    ) -> Result<(), CapFsError> {
        let root = self.root(&path.scope)?;
        let (parent, name) = self.open_parent(&root, &path.relative, false)?;
        let observed = match fstatat_no_follow(parent.as_raw_fd(), &name) {
            Ok(metadata) => metadata,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !same_directory_entry(expected, observed) {
            return Err(CapFsError::Contradiction(format!(
                "object changed before conditional unlink: {}",
                self.display_path(path)?.display()
            )));
        }
        #[cfg(test)]
        run_race_hook(RacePoint::BeforeUnlink);
        let immediately_before = match fstatat_no_follow(parent.as_raw_fd(), &name) {
            Ok(metadata) => metadata,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !same_directory_entry(expected, immediately_before) {
            return Err(CapFsError::Contradiction(format!(
                "object changed immediately before conditional unlink: {}",
                self.display_path(path)?.display()
            )));
        }
        match unlinkat_component(parent.as_raw_fd(), &name, directory) {
            Ok(()) => {
                sync_fd_best_effort(parent.as_raw_fd())?;
                if directory {
                    self.clear_directory_cache()?;
                }
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn clear_directory_cache(&self) -> Result<(), CapFsError> {
        self.directory_cache
            .lock()
            .map_err(|_| CapFsError::Contradiction("directory cache mutex poisoned".to_string()))?
            .clear();
        Ok(())
    }

    fn cache_directory(
        &self,
        root: &CapabilityRoot,
        relative: &RelativePath,
        fd: RawFd,
    ) -> Result<(), CapFsError> {
        if relative.is_empty() {
            return Ok(());
        }
        let metadata = fstat_fd(fd)?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::Contradiction(
                "attempted to cache a non-directory descriptor".to_string(),
            ));
        }
        let duplicate = duplicate_fd(fd)?;
        self.directory_cache
            .lock()
            .map_err(|_| CapFsError::Contradiction("directory cache mutex poisoned".to_string()))?
            .insert((root.id.clone(), relative.clone()), duplicate, metadata);
        Ok(())
    }

    fn cached_directory(
        &self,
        root: &CapabilityRoot,
        relative: &RelativePath,
    ) -> Result<Option<OwnedFd>, CapFsError> {
        if relative.is_empty() {
            return Ok(None);
        }
        let key = (root.id.clone(), relative.clone());
        let cached = self
            .directory_cache
            .lock()
            .map_err(|_| CapFsError::Contradiction("directory cache mutex poisoned".to_string()))?
            .duplicate(&key)?;
        let Some((fd, device, inode)) = cached else {
            #[cfg(test)]
            DIRECTORY_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        let parent_relative = relative.parent().unwrap_or_else(|| {
            RelativePath(PathBuf::new())
        });
        let name = relative.file_name().ok_or_else(|| {
            CapFsError::Contradiction("cached directory has no final component".to_string())
        })?;
        let parent = self.walk_effective_directory(root, &parent_relative)?;
        let current = match fstatat_no_follow(parent.as_raw_fd(), name) {
            Ok(metadata) => metadata,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                self.directory_cache
                    .lock()
                    .map_err(|_| CapFsError::Contradiction("directory cache mutex poisoned".to_string()))?
                    .invalidate_subtree(&root.id, relative);
                return Err(CapFsError::Contradiction(format!(
                    "previously observed directory component vanished: {}",
                    name.to_string_lossy()
                )));
            }
            Err(error) => return Err(error.into()),
        };
        if current.file_type != CapFileType::Directory
            || current.device != device
            || current.inode != inode
        {
            self.directory_cache
                .lock()
                .map_err(|_| CapFsError::Contradiction("directory cache mutex poisoned".to_string()))?
                .invalidate_subtree(&root.id, relative);
            return Err(CapFsError::Contradiction(format!(
                "previously observed directory component was replaced: {}",
                name.to_string_lossy()
            )));
        }
        #[cfg(test)]
        DIRECTORY_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        Ok(Some(fd))
    }

    fn root(&self, id: &ScopeId) -> Result<Arc<CapabilityRoot>, CapFsError> {
        let registry = self.registry.lock().map_err(|_| {
            CapFsError::Contradiction("capability registry mutex poisoned".to_string())
        })?;
        registry
            .by_id
            .get(id)
            .cloned()
            .ok_or_else(|| CapFsError::OutsideScope(format!("unknown scope {}", id.as_str())))
    }

    fn walk_directory(
        &self,
        root: &CapabilityRoot,
        relative: &RelativePath,
    ) -> Result<OwnedFd, CapFsError> {
        self.walk_effective_directory(root, relative)
    }

    fn walk_effective_directory(
        &self,
        root: &CapabilityRoot,
        relative: &RelativePath,
    ) -> Result<OwnedFd, CapFsError> {
        if relative.is_empty() {
            return self.logical_root_fd(root, None)?.ok_or_else(|| {
                CapFsError::Io(io::Error::from_raw_os_error(libc::ENOENT))
            });
        }
        if let Some(cached) = self.cached_directory(root, relative)? {
            return Ok(cached);
        }
        let logical_root = self.logical_root_fd(root, None)?.ok_or_else(|| {
            CapFsError::Io(io::Error::from_raw_os_error(libc::ENOENT))
        })?;
        #[cfg(target_os = "linux")]
        if let Some(fd) = try_openat2_directory(logical_root.as_raw_fd(), relative.as_path())? {
            self.cache_directory(root, relative, fd.as_raw_fd())?;
            return Ok(fd);
        }

        // Portable correctness backend used on macOS and whenever openat2 is
        // unavailable. Each component is opened O_NOFOLLOW and retained until
        // the next component has been validated as a directory.
        let mut current = logical_root;
        for component in normal_components(relative.as_path())? {
            #[cfg(test)]
            FALLBACK_COMPONENT_OPENS.fetch_add(1, Ordering::Relaxed);
            current = openat_owned(
                current.as_raw_fd(),
                &component,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0,
            )?;
            let metadata = fstat_fd(current.as_raw_fd())?;
            if metadata.file_type != CapFileType::Directory {
                return Err(CapFsError::InvalidPath(format!(
                    "intermediate component is not a directory: {}",
                    component.to_string_lossy()
                )));
            }
        }
        self.cache_directory(root, relative, current.as_raw_fd())?;
        Ok(current)
    }

    fn open_parent(
        &self,
        root: &CapabilityRoot,
        relative: &RelativePath,
        create_parents: bool,
    ) -> Result<(OwnedFd, OsString), CapFsError> {
        let name = relative.file_name().ok_or_else(|| {
            CapFsError::InvalidPath("operation may not target a capability root directly".to_string())
        })?;
        let parent_relative = relative
            .parent()
            .unwrap_or_else(|| RelativePath(PathBuf::new()));
        if create_parents {
            let mut current = self.logical_root_fd(root, None)?.ok_or_else(|| {
                CapFsError::ScopeConflict(format!(
                    "logical capability root must be materialized before parent creation: {}",
                    root.logical_path.display()
                ))
            })?;
            let mut current_relative = RelativePath(PathBuf::new());
            for component in normal_components(parent_relative.as_path())? {
                #[cfg(test)]
                run_race_hook(RacePoint::BeforeMkdir);
                match mkdirat_component(current.as_raw_fd(), &component, 0o755) {
                    Ok(()) => sync_fd_best_effort(current.as_raw_fd())?,
                    Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                let next = openat_owned(
                    current.as_raw_fd(),
                    &component,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?;
                let metadata = fstat_fd(next.as_raw_fd())?;
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::InvalidPath(format!(
                        "parent creation encountered non-directory: {}",
                        component.to_string_lossy()
                    )));
                }
                current_relative = current_relative.join(&component)?;
                // A previously observed component must still be the SAME
                // directory object: parent creation may not silently adopt a
                // replacement that raced in behind a cached descriptor (same
                // contradiction cached_directory reports on the read path).
                {
                    let key = (root.id.clone(), current_relative.clone());
                    let cached = self
                        .directory_cache
                        .lock()
                        .map_err(|_| {
                            CapFsError::Contradiction(
                                "directory cache mutex poisoned".to_string(),
                            )
                        })?
                        .duplicate(&key)?;
                    if let Some((_fd, device, inode)) = cached {
                        if metadata.device != device || metadata.inode != inode {
                            self.directory_cache
                                .lock()
                                .map_err(|_| {
                                    CapFsError::Contradiction(
                                        "directory cache mutex poisoned".to_string(),
                                    )
                                })?
                                .invalidate_subtree(&root.id, &current_relative);
                            return Err(CapFsError::Contradiction(format!(
                                "previously observed directory component was replaced: {}",
                                component.to_string_lossy()
                            )));
                        }
                    }
                }
                self.cache_directory(root, &current_relative, next.as_raw_fd())?;
                current = next;
            }
            return Ok((current, name.to_os_string()));
        }
        let parent_fd = self.walk_effective_directory(root, &parent_relative)?;
        Ok((parent_fd, name.to_os_string()))
    }

}

pub fn deterministic_scope_id(prefix: &str, path: &Path) -> Result<ScopeId, CapFsError> {
    let normalized = normalize_absolute(path)?;
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_os_str().as_bytes());
    let digest = hex::encode(hasher.finalize());
    ScopeId::new(format!("{prefix}-{}", &digest[..24]))
}

pub fn normalize_absolute(path: &Path) -> Result<PathBuf, CapFsError> {
    if !path.is_absolute() {
        return Err(CapFsError::InvalidPath(format!(
            "path is not absolute: {}",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::Normal(name) => {
                validate_component(name)?;
                normalized.push(name);
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(CapFsError::InvalidPath(format!(
                    "path contains unstable component: {}",
                    path.display()
                )))
            }
        }
    }
    Ok(normalized)
}

fn validate_component(name: &OsStr) -> Result<(), CapFsError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&0) {
        return Err(CapFsError::InvalidPath(format!(
            "invalid path component {:?}",
            name
        )));
    }
    Ok(())
}

fn normal_components(path: &Path) -> Result<Vec<OsString>, CapFsError> {
    RelativePath::new(path)?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect())
}

fn open_existing_ancestor_chain(
    logical_path: &Path,
) -> Result<Vec<(PathBuf, OwnedFd, CapMetadata)>, CapFsError> {
    let logical_path = normalize_absolute(logical_path)?;
    let mut current_path = PathBuf::from("/");
    let mut current = open_owned(
        Path::new("/"),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let mut chain = vec![(
        current_path.clone(),
        duplicate_fd(current.as_raw_fd())?,
        fstat_fd(current.as_raw_fd())?,
    )];
    for component in logical_path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        match openat_owned(
            current.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            Ok(next) => {
                let metadata = fstat_fd(next.as_raw_fd())?;
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::InvalidPath(format!(
                        "expected-root ancestor is not a directory: {}",
                        current_path.join(name).display()
                    )));
                }
                current_path.push(name);
                chain.push((
                    current_path.clone(),
                    duplicate_fd(next.as_raw_fd())?,
                    metadata,
                ));
                current = next;
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(chain)
}

fn open_existing_descendant_chain_from_capability(
    ancestor_logical_path: &Path,
    ancestor_acquisition_path: &Path,
    ancestor_fd: RawFd,
    logical_path: &Path,
) -> Result<Vec<(PathBuf, PathBuf, OwnedFd, CapMetadata)>, CapFsError> {
    let ancestor_logical_path = normalize_absolute(ancestor_logical_path)?;
    let ancestor_acquisition_path = normalize_absolute(ancestor_acquisition_path)?;
    let logical_path = normalize_absolute(logical_path)?;
    let relative = logical_path
        .strip_prefix(&ancestor_logical_path)
        .map_err(|_| {
            CapFsError::OutsideScope(format!(
                "logical root {} is not beneath retained ancestor {}",
                logical_path.display(),
                ancestor_logical_path.display()
            ))
        })?;
    RelativePath::new(relative)?;

    let mut current_logical = ancestor_logical_path;
    let mut current_acquisition = ancestor_acquisition_path;
    let mut current = duplicate_fd(ancestor_fd)?;
    let mut chain = vec![(
        current_logical.clone(),
        current_acquisition.clone(),
        duplicate_fd(current.as_raw_fd())?,
        fstat_fd(current.as_raw_fd())?,
    )];
    for component in normal_components(relative)? {
        match openat_owned(
            current.as_raw_fd(),
            &component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            Ok(next) => {
                let metadata = fstat_fd(next.as_raw_fd())?;
                if metadata.file_type != CapFileType::Directory {
                    return Err(CapFsError::InvalidPath(format!(
                        "retained descendant is not a directory: {}",
                        current_logical.join(&component).display()
                    )));
                }
                current_logical.push(&component);
                current_acquisition.push(&component);
                chain.push((
                    current_logical.clone(),
                    current_acquisition.clone(),
                    duplicate_fd(next.as_raw_fd())?,
                    metadata,
                ));
                current = next;
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(chain)
}

fn open_nearest_existing_directory(
    path: &Path,
) -> Result<(PathBuf, PathBuf, OwnedFd), CapFsError> {
    let mut candidate = path.to_path_buf();
    let mut suffix = Vec::<OsString>::new();
    loop {
        match open_absolute_directory(&candidate) {
            Ok(fd) => {
                let mut remainder = PathBuf::new();
                for component in suffix.iter().rev() {
                    remainder.push(component);
                }
                return Ok((candidate, remainder, fd));
            }
            Err(CapFsError::Io(error))
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOENT) | Some(libc::ENOTDIR)
                ) =>
            {
                let name = candidate.file_name().ok_or_else(|| {
                    CapFsError::InvalidPath(format!(
                        "no existing directory ancestor for {}",
                        path.display()
                    ))
                })?;
                suffix.push(name.to_os_string());
                candidate = candidate.parent().ok_or_else(|| {
                    CapFsError::InvalidPath(format!(
                        "no existing directory ancestor for {}",
                        path.display()
                    ))
                })?.to_path_buf();
            }
            Err(error) => return Err(error),
        }
    }
}

fn open_absolute_directory(path: &Path) -> Result<OwnedFd, CapFsError> {
    let normalized = normalize_absolute(path)?;
    let mut current = open_owned(
        Path::new("/"),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    for component in normalized.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current = openat_owned(
            current.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )?;
    }
    Ok(current)
}

fn os_cstring(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[cfg(target_os = "linux")]
fn try_openat2_directory(dirfd: RawFd, relative: &Path) -> Result<Option<OwnedFd>, CapFsError> {
    #[cfg(test)]
    OPENAT2_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    if FORCE_OPENAT2_FALLBACK.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let relative = os_cstring(relative.as_os_str())?;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: `relative` and `how` remain alive for the syscall and `dirfd` is
    // borrowed. A successful descriptor is uniquely transferred to OwnedFd.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            relative.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    } as i32;
    if fd >= 0 {
        return Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Unsupported kernel, old structure size, filesystem refusal, or a
        // sandbox/seccomp policy: retain correctness through the openat walk.
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::E2BIG) | Some(libc::EPERM) => {
            Ok(None)
        }
        _ => Err(error.into()),
    }
}

fn open_owned(path: &Path, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    let path = os_cstring(path.as_os_str())?;
    // SAFETY: `path` is NUL-terminated and alive for the call. On success the
    // returned descriptor is uniquely owned and transferred into `OwnedFd`.
    let fd = unsafe { libc::open(path.as_ptr(), flags, mode as libc::mode_t) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn valid_materialization_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn materialization_stage_name(token: &str) -> OsString {
    let mut name = OsString::from(MATERIALIZATION_STAGE_PREFIX);
    name.push(token);
    name
}

fn materialization_authority_name(id: &ScopeId, base: &RelativePath) -> OsString {
    let mut hasher = Sha256::new();
    hasher.update(id.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(base.as_path().as_os_str().as_bytes());
    let mut name = OsString::from(MATERIALIZATION_AUTHORITY_PREFIX);
    name.push(hex::encode(hasher.finalize()));
    name
}

fn create_materialization_authority(
    dirfd: RawFd,
    name: &OsStr,
    token: &str,
) -> Result<(), CapFsError> {
    if !valid_materialization_token(token) {
        return Err(CapFsError::InvalidPath(
            "invalid logical-root bootstrap authority token".to_string(),
        ));
    }
    match openat_owned(
        dirfd,
        name,
        libc::O_WRONLY
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW,
        0o600,
    ) {
        Ok(fd) => {
            let mut file = File::from(fd);
            file.write_all(token.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            sync_fd_best_effort(dirfd)?;
            Ok(())
        }
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
            match read_materialization_authority(dirfd, name)? {
                Some(observed) if observed.as_str() == token => Ok(()),
                Some(_) => Err(CapFsError::ScopeConflict(
                    "logical-root bootstrap authority token mismatch".to_string(),
                )),
                None => Err(CapFsError::Contradiction(
                    "logical-root bootstrap authority disappeared after EEXIST".to_string(),
                )),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn read_materialization_authority(
    dirfd: RawFd,
    name: &OsStr,
) -> Result<Option<String>, CapFsError> {
    validate_component(name)?;
    let fd = match openat_owned(
        dirfd,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    ) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = fstat_fd(fd.as_raw_fd())?;
    if metadata.file_type != CapFileType::Regular || metadata.length > 128 {
        return Err(CapFsError::ScopeConflict(
            "logical-root bootstrap authority is not a small regular file".to_string(),
        ));
    }
    let mut file = File::from(fd);
    let mut bytes = Vec::with_capacity(metadata.length as usize);
    file.read_to_end(&mut bytes)?;
    let token = String::from_utf8(bytes).map_err(|_| {
        CapFsError::ScopeConflict(
            "logical-root bootstrap authority token is not UTF-8".to_string(),
        )
    })?;
    if !valid_materialization_token(&token) {
        return Err(CapFsError::ScopeConflict(
            "logical-root bootstrap authority token is invalid".to_string(),
        ));
    }
    Ok(Some(token))
}

fn create_materialization_marker(dirfd: RawFd, token: &str) -> Result<(), CapFsError> {
    if !valid_materialization_token(token) {
        return Err(CapFsError::InvalidPath(
            "invalid logical-root materialization token".to_string(),
        ));
    }
    let marker = OsStr::new(MATERIALIZATION_MARKER);
    match openat_owned(
        dirfd,
        marker,
        libc::O_WRONLY
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW,
        0o600,
    ) {
        Ok(fd) => {
            let mut file = File::from(fd);
            file.write_all(token.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            sync_fd_best_effort(dirfd)?;
            Ok(())
        }
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
            verify_materialization_marker(dirfd, token)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_materialization_marker(dirfd: RawFd) -> Result<Option<Vec<u8>>, CapFsError> {
    let marker = OsStr::new(MATERIALIZATION_MARKER);
    let fd = match openat_owned(
        dirfd,
        marker,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    ) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = fstat_fd(fd.as_raw_fd())?;
    if metadata.file_type != CapFileType::Regular || metadata.length > 128 {
        return Err(CapFsError::ScopeConflict(
            "logical-root materialization marker is not a small regular file".to_string(),
        ));
    }
    let mut file = File::from(fd);
    let mut bytes = Vec::with_capacity(metadata.length as usize);
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn verify_materialization_marker(dirfd: RawFd, token: &str) -> Result<(), CapFsError> {
    match read_materialization_marker(dirfd)? {
        Some(bytes) if bytes == token.as_bytes() => Ok(()),
        Some(_) => Err(CapFsError::ScopeConflict(
            "logical-root materialization marker token mismatch".to_string(),
        )),
        None => Err(CapFsError::ScopeConflict(
            "logical-root staging directory has no owner marker".to_string(),
        )),
    }
}

fn claim_or_verify_materialization_stage(
    dirfd: RawFd,
    token: &str,
) -> Result<(), CapFsError> {
    match read_materialization_marker(dirfd)? {
        Some(bytes) if bytes == token.as_bytes() => Ok(()),
        Some(_) => Err(CapFsError::ScopeConflict(
            "logical-root staging marker token mismatch".to_string(),
        )),
        None => {
            // The only anonymous interval is mkdirat -> marker fsync.  The
            // random, journal-known stage name plus an empty no-follow-opened
            // directory is sufficient to resume that exact interrupted step.
            // Any content before ownership is established is contradictory.
            if !read_directory_entries(duplicate_fd(dirfd)?)?.is_empty() {
                return Err(CapFsError::ScopeConflict(
                    "unmarked logical-root staging directory is not empty".to_string(),
                ));
            }
            create_materialization_marker(dirfd, token)
        }
    }
}

fn open_published_materialized_root(
    acquisition_fd: RawFd,
    components: &[OsString],
    token: &str,
) -> Result<(OwnedFd, OwnedFd), CapFsError> {
    if !valid_materialization_token(token) {
        return Err(CapFsError::ScopeConflict(
            "invalid logical-root materialization authority".to_string(),
        ));
    }
    let first = components.first().ok_or_else(|| {
        CapFsError::Contradiction("materialized root has no components".to_string())
    })?;
    let top = openat_owned(
        acquisition_fd,
        first,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    verify_materialization_marker(top.as_raw_fd(), token)?;
    let mut current = duplicate_fd(top.as_raw_fd())?;
    for component in components.iter().skip(1) {
        current = openat_owned(
            current.as_raw_fd(),
            component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )?;
        let metadata = fstat_fd(current.as_raw_fd())?;
        if metadata.file_type != CapFileType::Directory {
            return Err(CapFsError::ScopeConflict(
                "published logical-root path contains a non-directory component".to_string(),
            ));
        }
    }
    Ok((top, current))
}

fn openat_owned(dirfd: RawFd, name: &OsStr, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    validate_component(name).map_err(cap_to_io)?;
    let name = os_cstring(name)?;
    // SAFETY: `dirfd` is borrowed for the call; `name` is a valid C string.
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), flags, mode as libc::mode_t) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn duplicate_fd(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `fd` remains owned by the caller; `dup` returns a new descriptor.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn mkdirat_component(dirfd: RawFd, name: &OsStr, mode: u32) -> io::Result<()> {
    validate_component(name).map_err(cap_to_io)?;
    let name = os_cstring(name)?;
    // SAFETY: arguments are valid and borrowed only for this call.
    if unsafe { libc::mkdirat(dirfd, name.as_ptr(), mode as libc::mode_t) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unlinkat_component(dirfd: RawFd, name: &OsStr, directory: bool) -> io::Result<()> {
    validate_component(name).map_err(cap_to_io)?;
    let name = os_cstring(name)?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: arguments are valid and borrowed only for this call.
    if unsafe { libc::unlinkat(dirfd, name.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn linkat_no_follow(
    source_dirfd: RawFd,
    source_name: &OsStr,
    destination_dirfd: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_component(source_name).map_err(cap_to_io)?;
    validate_component(destination_name).map_err(cap_to_io)?;
    let source_name = os_cstring(source_name)?;
    let destination_name = os_cstring(destination_name)?;
    // No AT_SYMLINK_FOLLOW: link the directory entry itself, never its target.
    let result = unsafe {
        libc::linkat(
            source_dirfd,
            source_name.as_ptr(),
            destination_dirfd,
            destination_name.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}


/// True when the held directory object has been unlinked from the filesystem
/// (link count zero). A retired object can never again be reached or mutated
/// through the stale capability, so rebinding its scope id to a fresh object
/// at the same logical path is exactly what restart recovery would do.
fn capability_object_is_unlinked(fd: RawFd) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes the buffer on success; fd is a live descriptor.
    let rc = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if rc != 0 {
        return false;
    }
    // SAFETY: initialized by successful fstat.
    let stat = unsafe { stat.assume_init() };
    stat.st_nlink == 0
}

fn fstat_fd(fd: RawFd) -> io::Result<CapMetadata> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: kernel initializes `stat` on success.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(metadata_from_stat(unsafe { stat.assume_init() }))
}

fn fstatat_no_follow(dirfd: RawFd, name: &OsStr) -> io::Result<CapMetadata> {
    validate_component(name).map_err(cap_to_io)?;
    let name = os_cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: kernel initializes `stat` on success; AT_SYMLINK_NOFOLLOW is
    // mandatory so a substituted final symlink is observed, not traversed.
    if unsafe {
        libc::fstatat(
            dirfd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(metadata_from_stat(unsafe { stat.assume_init() }))
}

fn metadata_from_stat(stat: libc::stat) -> CapMetadata {
    let kind = stat.st_mode & libc::S_IFMT;
    let file_type = if kind == libc::S_IFREG {
        CapFileType::Regular
    } else if kind == libc::S_IFDIR {
        CapFileType::Directory
    } else if kind == libc::S_IFLNK {
        CapFileType::Symlink
    } else {
        CapFileType::Other
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let (accessed_seconds, accessed_nanos, modified_seconds, modified_nanos, changed_seconds, changed_nanos) = (
        stat.st_atime as i64,
        stat.st_atime_nsec as i64,
        stat.st_mtime as i64,
        stat.st_mtime_nsec as i64,
        stat.st_ctime as i64,
        stat.st_ctime_nsec as i64,
    );
    #[cfg(target_os = "macos")]
    let (accessed_seconds, accessed_nanos, modified_seconds, modified_nanos, changed_seconds, changed_nanos) = (
        stat.st_atimespec.tv_sec as i64,
        stat.st_atimespec.tv_nsec as i64,
        stat.st_mtimespec.tv_sec as i64,
        stat.st_mtimespec.tv_nsec as i64,
        stat.st_ctimespec.tv_sec as i64,
        stat.st_ctimespec.tv_nsec as i64,
    );
    CapMetadata {
        file_type,
        mode: stat.st_mode as u32,
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        length: stat.st_size.max(0) as u64,
        accessed_seconds,
        accessed_nanos,
        modified_seconds,
        modified_nanos,
        changed_seconds,
        changed_nanos,
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: `DirectoryStream` uniquely owns the live DIR pointer.
        unsafe { libc::closedir(self.0) };
    }
}

fn read_directory_entries(directory: OwnedFd) -> Result<Vec<DirectoryEntry>, CapFsError> {
    let raw = directory.as_raw_fd();
    let duplicate = duplicate_fd(raw)?;
    let raw_duplicate = duplicate.as_raw_fd();
    std::mem::forget(duplicate);
    // SAFETY: ownership of `raw_duplicate` transfers to `DIR*`; the RAII guard
    // closes it on every success and error path. The original fd remains live.
    let dir = unsafe { libc::fdopendir(raw_duplicate) };
    if dir.is_null() {
        // SAFETY: fdopendir failed and therefore did not consume the descriptor.
        unsafe { libc::close(raw_duplicate) };
        return Err(io::Error::last_os_error().into());
    }
    let stream = DirectoryStream(dir);
    // Same shared-offset hazard as `list_entries`: the dup family's offset may
    // already be at end-of-directory from an earlier enumeration. Rewind.
    // SAFETY: `stream.0` is a live DIR pointer.
    unsafe { libc::rewinddir(stream.0) };
    let mut entries = Vec::new();
    loop {
        set_errno_zero();
        // SAFETY: `stream` owns a live DIR pointer. The returned dirent is
        // consumed before the next call to readdir.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) == 0 {
                break;
            }
            return Err(error.into());
        }
        // SAFETY: d_name is NUL-terminated for the live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let name = OsString::from_vec(name.to_bytes().to_vec());
        validate_component(&name)?;
        let metadata = fstatat_no_follow(raw, &name)?;
        entries.push(DirectoryEntry { name, metadata });
    }
    Ok(entries)
}


fn sync_fd(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is borrowed and remains owned by the caller.
    if unsafe { libc::fsync(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn sync_fd_best_effort(fd: RawFd) -> Result<(), CapFsError> {
    match sync_fd(fd) {
        Ok(()) => Ok(()),
        Err(error) if directory_sync_unsupported(&error) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn directory_sync_unsupported(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        code == libc::EINVAL || code == libc::ENOTSUP || code == libc::EOPNOTSUPP
    })
}

fn apply_fd_metadata(fd: RawFd, metadata: CapMetadata) -> Result<(), CapFsError> {
    // SAFETY: fd is valid for the call.
    if unsafe { libc::fchmod(fd, (metadata.mode & 0o7777) as libc::mode_t) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let times = [
        libc::timespec {
            tv_sec: metadata.accessed_seconds as libc::time_t,
            tv_nsec: metadata.accessed_nanos as libc::c_long,
        },
        libc::timespec {
            tv_sec: metadata.modified_seconds as libc::time_t,
            tv_nsec: metadata.modified_nanos as libc::c_long,
        },
    ];
    // SAFETY: pointer addresses a fixed two-element timespec array.
    if unsafe { libc::futimens(fd, times.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_rename_no_clobber(
    source_dirfd: RawFd,
    source_name: &OsStr,
    destination_dirfd: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_component(source_name).map_err(cap_to_io)?;
    validate_component(destination_name).map_err(cap_to_io)?;
    let source_name = os_cstring(source_name)?;
    let destination_name = os_cstring(destination_name)?;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: syscall receives valid directory fds and C strings. ENOSYS and
    // EINVAL are handled by the conservative fallback above.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_dirfd,
            source_name.as_ptr(),
            destination_dirfd,
            destination_name.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
type RenameAtxNp = unsafe extern "C" fn(
    libc::c_int,
    *const libc::c_char,
    libc::c_int,
    *const libc::c_char,
    libc::c_uint,
) -> libc::c_int;

#[cfg(target_os = "macos")]
fn macos_renameatx_np() -> io::Result<RenameAtxNp> {
    const SYMBOL: &[u8] = b"renameatx_np\0";
    // Resolve at runtime so deployment on a system without renameatx_np fails
    // at the operation boundary with ENOSYS instead of failing to load the
    // process. RTLD_DEFAULT searches the already loaded system libraries.
    // SAFETY: SYMBOL is a static NUL-terminated C string. A non-null result is
    // the documented renameatx_np function and is converted to that exact ABI.
    let symbol = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            SYMBOL.as_ptr().cast::<libc::c_char>(),
        )
    };
    if symbol.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }
    // SAFETY: the symbol name and function signature match <sys/stdio.h>.
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, RenameAtxNp>(symbol) })
}

#[cfg(target_os = "macos")]
fn platform_rename_no_clobber(
    source_dirfd: RawFd,
    source_name: &OsStr,
    destination_dirfd: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_component(source_name).map_err(cap_to_io)?;
    validate_component(destination_name).map_err(cap_to_io)?;
    let source_name = os_cstring(source_name)?;
    let destination_name = os_cstring(destination_name)?;
    const RENAME_EXCL: libc::c_uint = 0x0000_0004;
    let renameatx_np = macos_renameatx_np()?;
    // SAFETY: valid fds, C strings, and documented RENAME_EXCL flag.
    let result = unsafe {
        renameatx_np(
            source_dirfd,
            source_name.as_ptr(),
            destination_dirfd,
            destination_name.as_ptr(),
            RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn platform_rename_no_clobber(
    _source_dirfd: RawFd,
    _source_name: &OsStr,
    _destination_dirfd: RawFd,
    _destination_name: &OsStr,
) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_rename_exchange(
    source_dirfd: RawFd,
    source_name: &OsStr,
    destination_dirfd: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_component(source_name).map_err(cap_to_io)?;
    validate_component(destination_name).map_err(cap_to_io)?;
    let source_name = os_cstring(source_name)?;
    let destination_name = os_cstring(destination_name)?;
    const RENAME_EXCHANGE: libc::c_uint = 2;
    // SAFETY: valid borrowed directory descriptors and C strings.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_dirfd,
            source_name.as_ptr(),
            destination_dirfd,
            destination_name.as_ptr(),
            RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn platform_rename_exchange(
    source_dirfd: RawFd,
    source_name: &OsStr,
    destination_dirfd: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    validate_component(source_name).map_err(cap_to_io)?;
    validate_component(destination_name).map_err(cap_to_io)?;
    let source_name = os_cstring(source_name)?;
    let destination_name = os_cstring(destination_name)?;
    const RENAME_SWAP: libc::c_uint = 0x0000_0002;
    let renameatx_np = macos_renameatx_np()?;
    // SAFETY: valid fds, C strings, and documented RENAME_SWAP flag.
    let result = unsafe {
        renameatx_np(
            source_dirfd,
            source_name.as_ptr(),
            destination_dirfd,
            destination_name.as_ptr(),
            RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn platform_rename_exchange(
    _source_dirfd: RawFd,
    _source_name: &OsStr,
    _destination_dirfd: RawFd,
    _destination_name: &OsStr,
) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

fn no_clobber_unavailable(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        code == libc::ENOSYS
            || code == libc::EINVAL
            || code == libc::ENOTSUP
            || code == libc::EOPNOTSUPP
    })
}

fn is_hidden(name: &OsStr) -> bool {
    name.as_bytes().first() == Some(&b'.')
}

fn cap_to_io(error: CapFsError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn first_materialization_boundary_coalesces_siblings_only_while_parent_is_missing() {
        let temp = TempDir::new().unwrap();
        let shared = temp.path().join("exports");
        let logs = shared.join("logs");
        let cues = shared.join("cues");

        assert_eq!(
            CapabilityFilesystem::first_materialization_boundary(&logs).unwrap(),
            shared
        );
        assert_eq!(
            CapabilityFilesystem::first_materialization_boundary(&cues).unwrap(),
            shared
        );

        std::fs::create_dir(&shared).unwrap();
        assert_eq!(
            CapabilityFilesystem::first_materialization_boundary(&logs).unwrap(),
            logs
        );
        assert_eq!(
            CapabilityFilesystem::first_materialization_boundary(&cues).unwrap(),
            cues
        );
    }

    #[test]
    fn relative_paths_reject_escape_components() {
        for value in [
            "/absolute",
            "../escape",
            "a/../b",
            "./a",
            "a/./b",
            "a//b",
            "a/",
            "a\0b",
        ] {
            assert!(RelativePath::new(value).is_err(), "accepted {value:?}");
        }
        assert!(RelativePath::new("a/b").is_ok());
    }

    #[test]
    fn retained_root_survives_pathname_replacement() {
        let temp = TempDir::new().unwrap();
        let original = temp.path().join("album");
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("inside"), b"original").unwrap();

        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("album").unwrap();
        capabilities.pin_existing_root(scope.clone(), &original).unwrap();
        std::fs::rename(&original, temp.path().join("renamed")).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("inside"), b"attacker").unwrap();

        let bytes = capabilities
            .read_bytes(&ScopedPath {
                scope,
                relative: RelativePath::new("inside").unwrap(),
            })
            .unwrap();
        assert_eq!(bytes, b"original");
    }

    #[test]
    fn substituted_intermediate_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let attacker = temp.path().join("attacker");
        std::fs::create_dir_all(root.join("parent")).unwrap();
        std::fs::create_dir(&attacker).unwrap();

        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        capabilities
            .open_directory(&ScopedPath {
                scope: scope.clone(),
                relative: RelativePath::new("parent").unwrap(),
            })
            .unwrap();
        std::fs::remove_dir(root.join("parent")).unwrap();
        symlink(&attacker, root.join("parent")).unwrap();

        let result = capabilities.write_bytes_exclusive_durable(
            &ScopedPath {
                scope,
                relative: RelativePath::new("parent/payload").unwrap(),
            },
            b"blocked",
            0o600,
        );
        assert!(result.is_err());
        assert!(!attacker.join("payload").exists());
    }

    #[test]
    fn concurrent_destination_creation_is_no_clobber() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("source"), b"source").unwrap();
        std::fs::write(root.join("destination"), b"other").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let result = capabilities.rename_no_clobber(
            &ScopedPath {
                scope: scope.clone(),
                relative: RelativePath::new("source").unwrap(),
            },
            &ScopedPath {
                scope,
                relative: RelativePath::new("destination").unwrap(),
            },
        );
        assert!(matches!(result, Err(CapFsError::AlreadyExists(_))));
        assert_eq!(std::fs::read(root.join("source")).unwrap(), b"source");
        assert_eq!(std::fs::read(root.join("destination")).unwrap(), b"other");
    }
    #[test]
    fn logical_nonexistent_root_does_not_authorize_its_existing_parent() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let logical = parent.join("allowed");
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        capabilities.pin_root(scope.clone(), &logical).unwrap();
        capabilities.materialize_scope(&scope, 0o755).unwrap();
        assert!(matches!(
            capabilities.scoped_path(parent.join("sibling")),
            Err(CapFsError::OutsideScope(_))
        ));
        let child = ScopedPath {
            scope,
            relative: RelativePath::new("nested/file").unwrap(),
        };
        capabilities
            .write_bytes_exclusive_durable(&child, b"inside", 0o600)
            .unwrap();
        assert_eq!(std::fs::read(logical.join("nested/file")).unwrap(), b"inside");
        assert!(!parent.join("sibling").exists());
    }

    #[test]
    fn mutations_remain_bound_after_root_pathname_is_replaced() {
        let temp = TempDir::new().unwrap();
        let original = temp.path().join("album");
        let retained = temp.path().join("album-retained");
        std::fs::create_dir(&original).unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("album").unwrap();
        capabilities.pin_existing_root(scope.clone(), &original).unwrap();
        std::fs::rename(&original, &retained).unwrap();
        std::fs::create_dir(&original).unwrap();
        capabilities
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope,
                    relative: RelativePath::new("created").unwrap(),
                },
                b"retained",
                0o600,
            )
            .unwrap();
        assert_eq!(std::fs::read(retained.join("created")).unwrap(), b"retained");
        assert!(!original.join("created").exists());
    }

    #[test]
    fn nonexistent_external_root_remains_bound_when_parent_path_is_replaced() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("export");
        let retained_parent = temp.path().join("export-retained");
        std::fs::create_dir(&parent).unwrap();
        let logical = parent.join("album");
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        capabilities.pin_root(scope.clone(), &logical).unwrap();
        std::fs::rename(&parent, &retained_parent).unwrap();
        capabilities.materialize_scope(&scope, 0o755).unwrap();
        std::fs::create_dir(&parent).unwrap();
        capabilities
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope,
                    relative: RelativePath::new("track.flac").unwrap(),
                },
                b"audio",
                0o600,
            )
            .unwrap();
        assert_eq!(
            std::fs::read(retained_parent.join("album/track.flac")).unwrap(),
            b"audio"
        );
        assert!(!parent.join("album/track.flac").exists());
    }

    #[test]
    fn newly_materialized_logical_root_remains_bound_after_path_replacement() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("export");
        let logical = parent.join("album");
        let retained = parent.join("album-retained");
        std::fs::create_dir(&parent).unwrap();

        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        capabilities.pin_root(scope.clone(), &logical).unwrap();
        capabilities.materialize_scope(&scope, 0o755).unwrap();
        capabilities
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope: scope.clone(),
                    relative: RelativePath::new("first").unwrap(),
                },
                b"first",
                0o600,
            )
            .unwrap();

        std::fs::rename(&logical, &retained).unwrap();
        std::fs::create_dir(&logical).unwrap();
        capabilities
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope,
                    relative: RelativePath::new("second").unwrap(),
                },
                b"second",
                0o600,
            )
            .unwrap();
        assert_eq!(std::fs::read(retained.join("first")).unwrap(), b"first");
        assert_eq!(std::fs::read(retained.join("second")).unwrap(), b"second");
        assert!(!logical.join("second").exists());
    }

    #[test]
    fn retained_descendant_root_materializes_under_original_album_after_replacement() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");
        let retained_output = temp.path().join("output-retained");
        let album = output.join("Album");
        let destination = album.join("backup");
        std::fs::create_dir_all(&album).unwrap();

        let album_capability = PinnedDirectoryCapability::open_trusted(&album).unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("destination-post-backup").unwrap();
        capabilities
            .pin_descendant_capability(
                scope.clone(),
                &destination,
                &album,
                &album_capability,
            )
            .unwrap();

        std::fs::rename(&output, &retained_output).unwrap();
        std::fs::create_dir_all(output.join("Album")).unwrap();

        capabilities.materialize_scope(&scope, 0o755).unwrap();
        capabilities
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope,
                    relative: RelativePath::new("copied.flac").unwrap(),
                },
                b"retained",
                0o600,
            )
            .unwrap();

        assert_eq!(
            std::fs::read(retained_output.join("Album/backup/copied.flac")).unwrap(),
            b"retained"
        );
        assert!(!output.join("Album/backup/copied.flac").exists());
    }

    #[test]
    fn descendant_scope_recovery_uses_retained_ancestor_not_replacement_path() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");
        let retained_output = temp.path().join("output-retained");
        let album = output.join("Album");
        let destination = album.join("backup");
        std::fs::create_dir_all(&album).unwrap();

        let album_capability = PinnedDirectoryCapability::open_trusted(&album).unwrap();
        let album_scope = ScopeId::new("album").unwrap();
        let destination_scope = ScopeId::new("destination-post-backup").unwrap();
        let first = CapabilityFilesystem::new();
        first
            .pin_existing_capability(album_scope.clone(), &album, &album_capability)
            .unwrap();
        first
            .pin_descendant_capability(
                destination_scope.clone(),
                &destination,
                &album,
                &album_capability,
            )
            .unwrap();
        first.materialize_scope(&destination_scope, 0o755).unwrap();
        let records = first.scope_records().unwrap();

        std::fs::rename(&output, &retained_output).unwrap();
        std::fs::create_dir_all(output.join("Album/backup")).unwrap();

        let restored = CapabilityFilesystem::new();
        restored
            .pin_existing_capability(album_scope.clone(), &album, &album_capability)
            .unwrap();
        restored
            .restore_scope_records(
                &records,
                &[
                    (album_scope, album.clone()),
                    (destination_scope.clone(), destination.clone()),
                ],
            )
            .unwrap();
        restored.validate_scope_records(&records).unwrap();
        restored
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope: destination_scope,
                    relative: RelativePath::new("recovered.flac").unwrap(),
                },
                b"recovered",
                0o600,
            )
            .unwrap();

        assert_eq!(
            std::fs::read(retained_output.join("Album/backup/recovered.flac")).unwrap(),
            b"recovered"
        );
        assert!(!output.join("Album/backup/recovered.flac").exists());
    }

    #[test]
    fn tampered_serialized_relative_paths_fail_during_deserialization() {
        let malicious = r#"{"scope":"album","relative":"../escape"}"#;
        assert!(serde_json::from_str::<ScopedPath>(malicious).is_err());
        let absolute = r#"{"scope":"album","relative":"/escape"}"#;
        assert!(serde_json::from_str::<ScopedPath>(absolute).is_err());
        let empty_component = r#"{"scope":"album","relative":"safe//escape"}"#;
        assert!(serde_json::from_str::<ScopedPath>(empty_component).is_err());
        let trailing_separator = r#"{"scope":"album","relative":"safe/"}"#;
        assert!(serde_json::from_str::<ScopedPath>(trailing_separator).is_err());
    }

    #[test]
    fn concurrent_mkdir_all_is_race_safe() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let capabilities = Arc::new(CapabilityFilesystem::new());
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let target = ScopedPath {
            scope,
            relative: RelativePath::new("a/b/c").unwrap(),
        };
        let mut threads = Vec::new();
        for _ in 0..8 {
            let capabilities = Arc::clone(&capabilities);
            let target = target.clone();
            threads.push(std::thread::spawn(move || {
                capabilities.mkdir_all(&target, 0o755)
            }));
        }
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert!(root.join("a/b/c").is_dir());
    }

    #[test]
    fn symlink_loops_are_enumerated_but_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        symlink("loop", root.join("loop")).unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let entries = capabilities
            .enumerate(&ScopedPath {
                scope,
                relative: RelativePath::new("").unwrap(),
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].metadata.file_type, CapFileType::Symlink);
    }

    #[test]
    fn special_files_are_rejected() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let fifo = root.join("fifo");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let path = ScopedPath {
            scope,
            relative: RelativePath::new("fifo").unwrap(),
        };
        assert_eq!(
            capabilities.metadata_no_follow(&path).unwrap().unwrap().file_type,
            CapFileType::Other
        );
        assert!(matches!(
            capabilities.open_regular_read(&path),
            Err(CapFsError::UnsupportedObject(_))
        ));
    }

    #[test]
    fn source_substitution_during_rename_is_detected_and_never_silent() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let saved = root.join("saved-original");
        let replacement = root.join("replacement");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let _hook = RaceHookGuard::install(RacePoint::BeforeRename, {
            let source = source.clone();
            let saved = saved.clone();
            let replacement = replacement.clone();
            move || {
                std::fs::rename(&source, &saved).unwrap();
                std::fs::rename(&replacement, &source).unwrap();
            }
        });
        let result = capabilities.rename_no_clobber(
            &ScopedPath {
                scope: scope.clone(),
                relative: RelativePath::new("source").unwrap(),
            },
            &ScopedPath {
                scope,
                relative: RelativePath::new("destination").unwrap(),
            },
        );
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(saved).unwrap(), b"original");
        assert_eq!(std::fs::read(source).unwrap(), b"replacement");
        assert!(!root.join("destination").exists());
    }

    #[cfg(target_os = "linux")]

    #[test]
    fn checked_rename_rejects_a_source_replaced_after_planning_without_mutation() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let planned = root.join("planned");
        std::fs::write(&source, b"planned").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let source_scoped = ScopedPath {
            scope: scope.clone(),
            relative: RelativePath::new("source").unwrap(),
        };
        let expected = capabilities
            .metadata_no_follow(&source_scoped)
            .unwrap()
            .unwrap()
            .entry_identity();
        std::fs::rename(&source, &planned).unwrap();
        std::fs::write(&source, b"replacement").unwrap();

        let result = capabilities.rename_no_clobber_checked(
            &source_scoped,
            &ScopedPath {
                scope,
                relative: RelativePath::new("destination").unwrap(),
            },
            Some(expected),
        );
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(source).unwrap(), b"replacement");
        assert_eq!(std::fs::read(planned).unwrap(), b"planned");
        assert!(!root.join("destination").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openat2_unavailable_uses_component_walk_without_following_symlinks() {
        let _fallback = Openat2FallbackGuard::install();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/file"), b"ok").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let bytes = capabilities
            .read_bytes(&ScopedPath {
                scope,
                relative: RelativePath::new("a/b/file").unwrap(),
            })
            .unwrap();
        assert_eq!(bytes, b"ok");
        assert!(OPENAT2_ATTEMPTS.load(Ordering::Relaxed) >= 1);
        assert!(FALLBACK_COMPONENT_OPENS.load(Ordering::Relaxed) >= 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_renameatx_no_clobber_preserves_existing_destination() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("source"), b"source").unwrap();
        std::fs::write(root.join("destination"), b"destination").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        assert!(matches!(
            capabilities.rename_no_clobber(
                &ScopedPath { scope: scope.clone(), relative: RelativePath::new("source").unwrap() },
                &ScopedPath { scope, relative: RelativePath::new("destination").unwrap() },
            ),
            Err(CapFsError::AlreadyExists(_))
        ));
        assert_eq!(std::fs::read(root.join("source")).unwrap(), b"source");
        assert_eq!(std::fs::read(root.join("destination")).unwrap(), b"destination");
    }

    #[test]
    fn equal_logical_root_aliases_choose_a_stable_scope() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let capabilities = CapabilityFilesystem::new();
        capabilities
            .pin_existing_root(ScopeId::new("z-scope").unwrap(), &root)
            .unwrap();
        capabilities
            .pin_existing_root(ScopeId::new("a-scope").unwrap(), &root)
            .unwrap();
        let selected = capabilities.scoped_path(root.join("child")).unwrap();
        assert_eq!(selected.scope.as_str(), "a-scope");
    }

    #[test]
    fn restore_never_uses_a_journal_provided_foreign_absolute_path() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let original = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        original.pin_existing_root(scope.clone(), &root).unwrap();
        let mut record = original.scope_records().unwrap().remove(0);
        let foreign_meta = std::fs::metadata(&foreign).unwrap();
        use std::os::unix::fs::MetadataExt;
        record.acquisition_path = foreign.clone();
        record.base_relative = RelativePath::new("").unwrap();
        record.device = foreign_meta.dev();
        record.inode = foreign_meta.ino();

        let restored = CapabilityFilesystem::new();
        let result = restored.restore_scope_records(
            &[record],
            &[(scope, root)],
        );
        assert!(matches!(result, Err(CapFsError::ScopeConflict(_))));
    }

    #[test]
    fn recovery_fails_closed_after_root_pathname_replacement() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let retained = temp.path().join("retained");
        std::fs::create_dir(&root).unwrap();
        let original = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        original.pin_existing_root(scope.clone(), &root).unwrap();
        let records = original.scope_records().unwrap();
        std::fs::rename(&root, &retained).unwrap();
        std::fs::create_dir(&root).unwrap();

        let recovered = CapabilityFilesystem::new();
        assert!(matches!(
            recovered.restore_scope_records(&records, &[(scope, root.clone())]),
            Err(CapFsError::ScopeConflict(_))
        ));
        assert!(!root.join("mutated").exists());
        assert!(!retained.join("mutated").exists());
    }

    #[test]
    fn destination_created_at_publication_race_is_preserved() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("temporary"), b"planned").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let _hook = RaceHookGuard::install(RacePoint::BeforeLink, {
            let destination = root.join("destination");
            move || std::fs::write(destination, b"concurrent").unwrap()
        });
        let result = capabilities.publish_no_clobber(
            &ScopedPath {
                scope: scope.clone(),
                relative: RelativePath::new("temporary").unwrap(),
            },
            &ScopedPath {
                scope,
                relative: RelativePath::new("destination").unwrap(),
            },
        );
        assert!(matches!(result, Err(CapFsError::AlreadyExists(_))));
        assert_eq!(std::fs::read(root.join("temporary")).unwrap(), b"planned");
        assert_eq!(std::fs::read(root.join("destination")).unwrap(), b"concurrent");
    }

    #[test]
    fn regular_file_publication_uses_same_inode_hardlink_witness() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("temporary"), b"payload").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        capabilities
            .publish_no_clobber(
                &ScopedPath {
                    scope: scope.clone(),
                    relative: RelativePath::new("temporary").unwrap(),
                },
                &ScopedPath {
                    scope,
                    relative: RelativePath::new("destination").unwrap(),
                },
            )
            .unwrap();
        use std::os::unix::fs::MetadataExt;
        let temporary = std::fs::metadata(root.join("temporary")).unwrap();
        let destination = std::fs::metadata(root.join("destination")).unwrap();
        assert_eq!((temporary.dev(), temporary.ino()), (destination.dev(), destination.ino()));
    }


    #[test]
    fn owned_regular_replace_atomically_exchanges_and_removes_the_old_generation() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("journal.write-tmp"), b"new-generation").unwrap();
        std::fs::write(root.join("journal.json"), b"old-generation").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let source = ScopedPath {
            scope: scope.clone(),
            relative: RelativePath::new("journal.write-tmp").unwrap(),
        };
        let destination = ScopedPath {
            scope,
            relative: RelativePath::new("journal.json").unwrap(),
        };
        let expected_source = capabilities.entry_identity(&source).unwrap().unwrap();
        let expected_destination = capabilities.entry_identity(&destination).unwrap();
        capabilities
            .replace_owned_regular(
                &source,
                &destination,
                expected_source,
                expected_destination,
            )
            .unwrap();
        assert_eq!(std::fs::read(root.join("journal.json")).unwrap(), b"new-generation");
        assert!(!root.join("journal.write-tmp").exists());
    }

    #[test]
    fn owned_regular_replace_rejects_source_replacement_before_exchange() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let temporary = root.join("journal.write-tmp");
        let saved = root.join("saved-new");
        let foreign = root.join("foreign");
        let destination = root.join("journal.json");
        std::fs::write(&temporary, b"new-generation").unwrap();
        std::fs::write(&foreign, b"foreign").unwrap();
        std::fs::write(&destination, b"old-generation").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let source_scoped = ScopedPath {
            scope: scope.clone(),
            relative: RelativePath::new("journal.write-tmp").unwrap(),
        };
        let destination_scoped = ScopedPath {
            scope,
            relative: RelativePath::new("journal.json").unwrap(),
        };
        let expected_source = capabilities.entry_identity(&source_scoped).unwrap().unwrap();
        let expected_destination = capabilities.entry_identity(&destination_scoped).unwrap();
        let _hook = RaceHookGuard::install(RacePoint::BeforeRename, {
            let temporary = temporary.clone();
            let saved = saved.clone();
            let foreign = foreign.clone();
            move || {
                std::fs::rename(&temporary, &saved).unwrap();
                std::fs::rename(&foreign, &temporary).unwrap();
            }
        });
        let result = capabilities.replace_owned_regular(
            &source_scoped,
            &destination_scoped,
            expected_source,
            expected_destination,
        );
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(saved).unwrap(), b"new-generation");
        assert_eq!(std::fs::read(temporary).unwrap(), b"foreign");
        assert_eq!(std::fs::read(destination).unwrap(), b"old-generation");
    }

    #[test]
    fn owned_regular_replace_preserves_a_displaced_entry_changed_before_cleanup() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let temporary = root.join("journal.write-tmp");
        let destination = root.join("journal.json");
        let preserved_old = root.join("preserved-old");
        let foreign = root.join("foreign");
        std::fs::write(&temporary, b"new-generation").unwrap();
        std::fs::write(&destination, b"old-generation").unwrap();
        std::fs::write(&foreign, b"foreign").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let _hook = RaceHookGuard::install(RacePoint::BeforeUnlink, {
            let temporary = temporary.clone();
            let preserved_old = preserved_old.clone();
            let foreign = foreign.clone();
            move || {
                std::fs::rename(&temporary, &preserved_old).unwrap();
                std::fs::rename(&foreign, &temporary).unwrap();
            }
        });
        let source_scoped = ScopedPath {
            scope: scope.clone(),
            relative: RelativePath::new("journal.write-tmp").unwrap(),
        };
        let destination_scoped = ScopedPath {
            scope,
            relative: RelativePath::new("journal.json").unwrap(),
        };
        let expected_source = capabilities.entry_identity(&source_scoped).unwrap().unwrap();
        let expected_destination = capabilities.entry_identity(&destination_scoped).unwrap();
        let result = capabilities.replace_owned_regular(
            &source_scoped,
            &destination_scoped,
            expected_source,
            expected_destination,
        );
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(destination).unwrap(), b"new-generation");
        assert_eq!(std::fs::read(preserved_old).unwrap(), b"old-generation");
        assert_eq!(std::fs::read(temporary).unwrap(), b"foreign");
    }

    #[test]
    fn network_style_errors_are_not_filesystem_name_special_cased() {
        assert!(no_clobber_unavailable(&io::Error::from_raw_os_error(libc::EOPNOTSUPP)));
        assert!(!no_clobber_unavailable(&io::Error::from_raw_os_error(libc::EIO)));
        assert!(!directory_sync_unsupported(&io::Error::from_raw_os_error(libc::EIO)));
    }

    #[test]
    fn source_replacement_before_unlink_is_detected_without_deleting_either_object() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let saved = root.join("saved");
        let replacement = root.join("replacement");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let _hook = RaceHookGuard::install(RacePoint::BeforeUnlink, {
            let source = source.clone();
            let saved = saved.clone();
            let replacement = replacement.clone();
            move || {
                std::fs::rename(&source, &saved).unwrap();
                std::fs::rename(&replacement, &source).unwrap();
            }
        });
        let result = capabilities.remove_tree(&ScopedPath {
            scope,
            relative: RelativePath::new("source").unwrap(),
        });
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(saved).unwrap(), b"original");
        assert_eq!(std::fs::read(source).unwrap(), b"replacement");
    }


    #[test]
    fn matching_removal_rejects_replacement_before_cleanup_begins() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let victim = root.join("victim");
        let saved = root.join("saved");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(&victim, b"planned").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let scoped = ScopedPath {
            scope,
            relative: RelativePath::new("victim").unwrap(),
        };
        let expected = capabilities.entry_identity(&scoped).unwrap().unwrap();
        std::fs::rename(&victim, &saved).unwrap();
        std::fs::write(&victim, b"replacement").unwrap();
        let result = capabilities.remove_tree_matching(&scoped, expected);
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(saved).unwrap(), b"planned");
        assert_eq!(std::fs::read(victim).unwrap(), b"replacement");
    }

    #[test]
    fn checked_recursive_removal_rejects_a_replaced_directory_tree() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let victim = root.join("victim");
        let saved = root.join("saved");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("original"), b"original").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        let scoped = ScopedPath {
            scope,
            relative: RelativePath::new("victim").unwrap(),
        };
        let expected = capabilities
            .metadata_no_follow(&scoped)
            .unwrap()
            .unwrap();
        std::fs::rename(&victim, &saved).unwrap();
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("replacement"), b"replacement").unwrap();

        let result = capabilities.remove_tree_checked(&scoped, expected);
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert_eq!(std::fs::read(saved.join("original")).unwrap(), b"original");
        assert_eq!(std::fs::read(victim.join("replacement")).unwrap(), b"replacement");
    }

    #[test]
    fn observed_intermediate_directory_replacement_fails_closed() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let saved = temp.path().join("saved-parent");
        std::fs::create_dir_all(root.join("parent")).unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        capabilities
            .open_directory(&ScopedPath {
                scope: scope.clone(),
                relative: RelativePath::new("parent").unwrap(),
            })
            .unwrap();
        std::fs::rename(root.join("parent"), &saved).unwrap();
        std::fs::create_dir(root.join("parent")).unwrap();

        let result = capabilities.write_bytes_exclusive_durable(
            &ScopedPath {
                scope,
                relative: RelativePath::new("parent/payload").unwrap(),
            },
            b"must-not-write",
            0o600,
        );
        assert!(matches!(result, Err(CapFsError::Contradiction(_))));
        assert!(!root.join("parent/payload").exists());
        assert!(!saved.join("payload").exists());
    }

    #[test]
    fn bounded_directory_cache_reuses_and_revalidates_intermediates() {
        let _serial = RACE_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        DIRECTORY_CACHE_HITS.store(0, Ordering::Relaxed);
        DIRECTORY_CACHE_MISSES.store(0, Ordering::Relaxed);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/one"), b"1").unwrap();
        std::fs::write(root.join("a/b/two"), b"2").unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("root").unwrap();
        capabilities.pin_existing_root(scope.clone(), &root).unwrap();
        assert_eq!(
            capabilities
                .read_bytes(&ScopedPath {
                    scope: scope.clone(),
                    relative: RelativePath::new("a/b/one").unwrap(),
                })
                .unwrap(),
            b"1"
        );
        assert_eq!(
            capabilities
                .read_bytes(&ScopedPath {
                    scope,
                    relative: RelativePath::new("a/b/two").unwrap(),
                })
                .unwrap(),
            b"2"
        );
        assert!(DIRECTORY_CACHE_MISSES.load(Ordering::Relaxed) >= 1);
        assert!(DIRECTORY_CACHE_HITS.load(Ordering::Relaxed) >= 1);
        let cache = capabilities.directory_cache.lock().unwrap();
        assert!(cache.entries.len() <= DIRECTORY_CACHE_CAPACITY);
    }

    #[test]
    fn recovery_rebinds_durably_materialized_missing_logical_root() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let logical = parent.join("new-root");
        std::fs::create_dir(&parent).unwrap();
        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        first.pin_root(scope.clone(), &logical).unwrap();
        first.materialize_scope(&scope, 0o755).unwrap();
        first
            .write_bytes_exclusive_durable(
                &ScopedPath {
                    scope: scope.clone(),
                    relative: RelativePath::new("payload").unwrap(),
                },
                b"durable",
                0o600,
            )
            .unwrap();
        let records = first.scope_records().unwrap();

        let recovered = CapabilityFilesystem::new();
        recovered
            .restore_scope_records(&records, &[(scope.clone(), logical.clone())])
            .unwrap();
        assert_eq!(
            recovered
                .read_bytes(&ScopedPath {
                    scope,
                    relative: RelativePath::new("payload").unwrap(),
                })
                .unwrap(),
            b"durable"
        );
    }


    #[test]
    fn recovery_adopts_token_owned_publication_before_identity_generation() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let logical = parent.join("new-root");
        std::fs::create_dir(&parent).unwrap();
        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        first.pin_root(scope.clone(), &logical).unwrap();
        let pre_publication_records = first.scope_records().unwrap();
        assert!(pre_publication_records[0].materialized_device.is_none());
        assert!(pre_publication_records[0].materialization_token.is_some());

        first.materialize_scope(&scope, 0o755).unwrap();
        assert!(logical.join(MATERIALIZATION_MARKER).is_file());
        drop(first);

        let recovered = CapabilityFilesystem::new();
        recovered
            .restore_scope_records(
                &pre_publication_records,
                &[(scope.clone(), logical.clone())],
            )
            .unwrap();
        recovered
            .validate_scope_records(&pre_publication_records)
            .unwrap();
        let identity_records = recovered.scope_records().unwrap();
        assert!(identity_records[0].materialized_device.is_some());
        assert!(identity_records[0].materialized_inode.is_some());
        recovered.finalize_materialized_roots().unwrap();
        assert!(!logical.join(MATERIALIZATION_MARKER).exists());
    }

    #[test]
    fn recovery_resumes_token_owned_stage_before_publication() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let logical = parent.join("new-root");
        std::fs::create_dir(&parent).unwrap();
        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        first.pin_root(scope.clone(), &logical).unwrap();
        let records = first.scope_records().unwrap();
        let token = records[0].materialization_token.as_deref().unwrap();
        let stage = parent.join(materialization_stage_name(token));

        let hook = RaceHookGuard::install(RacePoint::BeforeRename, || {
            panic!("simulated process death before root publication")
        });
        let interrupted = catch_unwind(AssertUnwindSafe(|| {
            first.materialize_scope(&scope, 0o755).unwrap();
        }));
        assert!(interrupted.is_err());
        drop(hook);
        assert!(stage.is_dir());
        assert!(!logical.exists());
        drop(first);

        let recovered = CapabilityFilesystem::new();
        recovered
            .restore_scope_records(&records, &[(scope.clone(), logical.clone())])
            .unwrap();
        recovered.materialize_scope(&scope, 0o755).unwrap();
        assert!(logical.is_dir());
        assert!(!stage.exists());
        recovered.finalize_materialized_roots().unwrap();
        assert!(!logical.join(MATERIALIZATION_MARKER).exists());
    }

    #[test]
    fn marker_cleanup_uses_retained_published_directory_after_path_replacement() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let logical = parent.join("new-root");
        let retained = parent.join("retained-root");
        std::fs::create_dir(&parent).unwrap();
        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        capabilities.pin_root(scope.clone(), &logical).unwrap();
        capabilities.materialize_scope(&scope, 0o755).unwrap();
        let records = capabilities.scope_records().unwrap();
        assert!(records[0].materialized_device.is_some());
        assert!(logical.join(MATERIALIZATION_MARKER).is_file());

        std::fs::rename(&logical, &retained).unwrap();
        std::fs::create_dir(&logical).unwrap();
        std::fs::write(logical.join(MATERIALIZATION_MARKER), b"attacker").unwrap();

        capabilities.finalize_materialized_roots().unwrap();
        assert!(!retained.join(MATERIALIZATION_MARKER).exists());
        assert_eq!(
            std::fs::read(logical.join(MATERIALIZATION_MARKER)).unwrap(),
            b"attacker"
        );
    }

    #[test]
    fn recoverable_internal_root_adopts_publication_before_first_scope_record() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir(&parent).unwrap();

        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("journal").unwrap();
        first
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        let initial = first.scope_records().unwrap();
        let authority = parent.join(
            initial[0]
                .materialization_authority_name
                .as_deref()
                .unwrap(),
        );
        assert!(authority.is_file());
        first.materialize_scope(&scope, 0o700).unwrap();
        assert!(logical.join(MATERIALIZATION_MARKER).is_file());
        drop(first);

        let recovered = CapabilityFilesystem::new();
        recovered
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        let records = recovered.scope_records().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].materialization_token.is_some());
        assert!(records[0].materialized_device.is_some());
        recovered.finalize_materialized_roots().unwrap();
        assert!(!logical.join(MATERIALIZATION_MARKER).exists());
        assert!(!authority.exists());
    }

    #[test]
    fn recoverable_internal_root_resumes_stage_before_first_scope_record() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir(&parent).unwrap();

        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("journal").unwrap();
        first
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        let records = first.scope_records().unwrap();
        let token = records[0].materialization_token.as_deref().unwrap();
        let authority = parent.join(
            records[0]
                .materialization_authority_name
                .as_deref()
                .unwrap(),
        );
        assert!(authority.is_file());
        let stage = parent.join(materialization_stage_name(token));
        let hook = RaceHookGuard::install(RacePoint::BeforeRename, || {
            panic!("simulated death before first internal-root publication")
        });
        assert!(catch_unwind(AssertUnwindSafe(|| {
            first.materialize_scope(&scope, 0o700).unwrap();
        }))
        .is_err());
        drop(hook);
        assert!(stage.is_dir());
        assert!(!logical.exists());
        drop(first);

        let recovered = CapabilityFilesystem::new();
        recovered
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        recovered.materialize_scope(&scope, 0o700).unwrap();
        assert!(logical.is_dir());
        assert!(!stage.exists());
        recovered.finalize_materialized_roots().unwrap();
        assert!(!logical.join(MATERIALIZATION_MARKER).exists());
        assert!(!authority.exists());
    }


    #[test]
    fn recoverable_internal_root_claims_empty_unmarked_authorized_stage() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir(&parent).unwrap();

        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("journal").unwrap();
        first
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        let records = first.scope_records().unwrap();
        let token = records[0].materialization_token.as_deref().unwrap();
        let stage = parent.join(materialization_stage_name(token));
        std::fs::create_dir(&stage).unwrap();
        drop(first);

        let recovered = CapabilityFilesystem::new();
        recovered
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        assert_eq!(
            std::fs::read(stage.join(MATERIALIZATION_MARKER)).unwrap(),
            token.as_bytes()
        );
        recovered.materialize_scope(&scope, 0o700).unwrap();
        assert!(logical.is_dir());
        recovered.finalize_materialized_roots().unwrap();
    }

    #[test]
    fn bootstrap_authority_cleanup_uses_retained_parent_after_path_replacement() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let retained_parent = temp.path().join("retained-coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir(&parent).unwrap();

        let capabilities = CapabilityFilesystem::new();
        let scope = ScopeId::new("journal").unwrap();
        capabilities
            .pin_recoverable_internal_root(scope.clone(), &logical)
            .unwrap();
        capabilities.materialize_scope(&scope, 0o700).unwrap();
        let records = capabilities.scope_records().unwrap();
        let authority_name = records[0]
            .materialization_authority_name
            .as_deref()
            .unwrap();
        assert!(parent.join(authority_name).is_file());

        std::fs::rename(&parent, &retained_parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join(authority_name), b"attacker").unwrap();

        capabilities.finalize_materialized_roots().unwrap();
        assert!(!retained_parent.join(authority_name).exists());
        assert!(!retained_parent
            .join(".tonepoet-action-journals")
            .join(MATERIALIZATION_MARKER)
            .exists());
        assert_eq!(std::fs::read(parent.join(authority_name)).unwrap(), b"attacker");
    }

    #[test]
    fn recoverable_internal_root_rejects_marker_without_bootstrap_authority() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir_all(&logical).unwrap();
        std::fs::write(
            logical.join(MATERIALIZATION_MARKER),
            b"0123456789abcdef0123456789abcdef",
        )
        .unwrap();

        let capabilities = CapabilityFilesystem::new();
        let result = capabilities.pin_recoverable_internal_root(
            ScopeId::new("journal").unwrap(),
            &logical,
        );
        assert!(matches!(result, Err(CapFsError::ScopeConflict(_))));
    }

    #[test]
    fn recoverable_internal_root_rejects_malformed_stage_artifacts() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("coordination");
        let logical = parent.join(".tonepoet-action-journals");
        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(parent.join(format!(
            "{MATERIALIZATION_STAGE_PREFIX}not-a-valid-token"
        )))
        .unwrap();

        let capabilities = CapabilityFilesystem::new();
        let result = capabilities.pin_recoverable_internal_root(
            ScopeId::new("journal").unwrap(),
            &logical,
        );
        assert!(matches!(result, Err(CapFsError::ScopeConflict(_))));
        assert!(!logical.exists());
    }

    #[test]
    fn recovery_refuses_logical_root_created_without_durable_materialized_identity() {
        let temp = TempDir::new().unwrap();
        let parent = temp.path().join("parent");
        let logical = parent.join("new-root");
        std::fs::create_dir(&parent).unwrap();
        let first = CapabilityFilesystem::new();
        let scope = ScopeId::new("external").unwrap();
        first.pin_root(scope.clone(), &logical).unwrap();
        let records = first.scope_records().unwrap();
        std::fs::create_dir(&logical).unwrap();

        let recovered = CapabilityFilesystem::new();
        assert!(matches!(
            recovered.restore_scope_records(&records, &[(scope, logical)]),
            Err(CapFsError::ScopeConflict(_))
        ));
    }


    #[cfg(unix)]
    #[test]
    fn trusted_symlink_route_is_resolved_once_and_retained_after_alias_retarget() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let alias = temp.path().join("configured-output");
        symlink(&first, &alias).unwrap();

        let capability = PinnedDirectoryCapability::open_trusted(&alias).unwrap();
        std::fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();

        capability
            .write_regular_child_create_new_durable(
                OsStr::new("authority"),
                b"retained-first",
                0o600,
            )
            .unwrap();
        assert_eq!(std::fs::read(first.join("authority")).unwrap(), b"retained-first");
        assert!(!second.join("authority").exists());
        assert_eq!(capability.display_path(), std::fs::canonicalize(&first).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn pinned_root_refuses_untrusted_symlink_beneath_acquired_capability() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("child")).unwrap();

        let capability = PinnedDirectoryCapability::open_trusted(&root).unwrap();
        assert!(matches!(
            capability.open_directory_child(OsStr::new("child"), false, 0o700),
            Err(CapFsError::Io(_)) | Err(CapFsError::UnsupportedObject(_))
        ));
        assert!(!outside.join("authority").exists());
    }

}

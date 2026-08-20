//! Cross-process concurrency primitives for independent tonepoet sessions.
//!
//! Persistent leases deliberately differ from the repository's short-lived
//! local file locks: `PersistentLease::drop` is close-only, never explicitly
//! unlocks a possibly-shared open-file description, and never unlinks its
//! descriptor. Ordinary `MutationClaimGuard` teardown may retire an unexported
//! ephemeral descriptor immediately before that close; detached/exported
//! authority keeps the persistent close-only/lazy-retirement contract.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DESCRIPTOR_SCHEMA: u32 = 1;
const DESCRIPTOR_MAX_BYTES: u64 = 1024 * 1024;
const REGISTRY_WAIT: Duration = Duration::from_millis(250);
const REGISTRY_RETRY: Duration = Duration::from_millis(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerProcessIdentity {
    pub pid: u64,
    pub start_ticks: u64,
    pub boot_id_hash: u64,
    pub process_token: u64,
}

impl OwnerProcessIdentity {
    pub fn current() -> Self {
        let pid = std::process::id();
        Self {
            pid: pid as u64,
            start_ticks: process_start_ticks(pid).unwrap_or(0),
            boot_id_hash: boot_id_hash().unwrap_or(0),
            process_token: process_instance_token(),
        }
    }

    pub fn appears_active(self) -> bool {
        if self.pid == 0 {
            return false;
        }
        let Ok(pid) = u32::try_from(self.pid) else {
            return false;
        };
        if pid == std::process::id()
            && self.process_token != 0
            && self.process_token == process_instance_token()
        {
            return true;
        }
        if self.start_ticks == 0 || self.boot_id_hash == 0 {
            return false;
        }
        let Some(boot) = boot_id_hash() else {
            return false;
        };
        boot == self.boot_id_hash && process_start_ticks(pid) == Some(self.start_ticks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeaseFamily {
    JournalOperation { job_id: Uuid },
    QueueScope { scope_id: Uuid },
    QueueExecution { execution_id: Uuid },
    ExecutionClaim { execution_id: Uuid },
    ExecutionStaging { execution_id: Uuid },
    EphemeralMutation { claim_id: Uuid },
}

impl LeaseFamily {
    pub fn lifecycle_id(&self) -> Uuid {
        match self {
            Self::JournalOperation { job_id } => *job_id,
            Self::QueueScope { scope_id } => *scope_id,
            Self::QueueExecution { execution_id }
            | Self::ExecutionClaim { execution_id }
            | Self::ExecutionStaging { execution_id } => *execution_id,
            Self::EphemeralMutation { claim_id } => *claim_id,
        }
    }

    fn namespace(&self) -> &'static str {
        match self {
            Self::JournalOperation { .. } => "journal-operation",
            Self::QueueScope { .. } => "queue-scope",
            Self::QueueExecution { .. } => "queue-execution",
            Self::ExecutionClaim { .. } => "execution-claim",
            Self::ExecutionStaging { .. } => "execution-staging",
            Self::EphemeralMutation { .. } => "ephemeral-mutation",
        }
    }

    pub fn reserve_after_owner_death(&self) -> bool {
        matches!(
            self,
            Self::JournalOperation { .. }
                | Self::QueueExecution { .. }
                | Self::ExecutionClaim { .. }
                | Self::ExecutionStaging { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAvailability {
    Live,
    RecoveryReserved,
    ReclaimableEphemeral,
}

pub fn classify_availability(family: &LeaseFamily, lock_contended: bool) -> ClaimAvailability {
    if lock_contended {
        ClaimAvailability::Live
    } else if family.reserve_after_owner_death() || matches!(family, LeaseFamily::QueueScope { .. }) {
        ClaimAvailability::RecoveryReserved
    } else {
        ClaimAvailability::ReclaimableEphemeral
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimScope {
    Exact,
    Subtree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathResolutionSemantics {
    /// Later I/O intentionally follows the final pathname component to the
    /// object reached through the path.
    FollowReferent,
    /// Later I/O mutates the final directory entry itself. Parent aliases are
    /// still stabilized, but a final symlink remains the admitted object.
    NamespaceObject,
}

mod lossless_path_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize)]
    #[serde(untagged)]
    enum EncodedPath<'a> {
        Utf8(&'a str),
        #[cfg(unix)]
        UnixBytes { unix_bytes: &'a [u8] },
        #[cfg(windows)]
        WindowsWide { windows_wide: Vec<u16> },
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DecodedPath {
        Utf8(String),
        #[cfg(unix)]
        UnixBytes { unix_bytes: Vec<u8> },
        #[cfg(windows)]
        WindowsWide { windows_wide: Vec<u16> },
    }

    fn encode(path: &Path) -> EncodedPath<'_> {
        if let Some(text) = path.to_str() {
            return EncodedPath::Utf8(text);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            return EncodedPath::UnixBytes {
                unix_bytes: path.as_os_str().as_bytes(),
            };
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            return EncodedPath::WindowsWide {
                windows_wide: path.as_os_str().encode_wide().collect(),
            };
        }
        #[allow(unreachable_code)]
        EncodedPath::Utf8("")
    }

    fn decode<E: serde::de::Error>(encoded: DecodedPath) -> Result<PathBuf, E> {
        match encoded {
            DecodedPath::Utf8(text) => Ok(PathBuf::from(text)),
            #[cfg(unix)]
            DecodedPath::UnixBytes { unix_bytes } => {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_vec(unix_bytes)))
            }
            #[cfg(windows)]
            DecodedPath::WindowsWide { windows_wide } => {
                use std::os::windows::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_wide(&windows_wide)))
            }
        }
    }

    pub fn serialize<S>(path: &PathBuf, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        encode(path).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        decode(DecodedPath::deserialize(deserializer)?)
    }

    pub fn serialize_vec<S>(paths: &Vec<PathBuf>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        paths.iter().map(|path| encode(path)).collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize_vec<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<DecodedPath>::deserialize(deserializer)?
            .into_iter()
            .map(decode)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPathIdentity {
    #[serde(
        serialize_with = "lossless_path_serde::serialize",
        deserialize_with = "lossless_path_serde::deserialize"
    )]
    pub original: PathBuf,
    /// Absolute lexical namespace identity before symlink traversal.  This is
    /// carried alongside the resolved I/O identity so replacing/renaming a
    /// symlink or directory entry cannot silently rebind admitted work.
    #[serde(
        default,
        serialize_with = "lossless_path_serde::serialize",
        deserialize_with = "lossless_path_serde::deserialize"
    )]
    pub namespace_path: PathBuf,
    /// Exact namespace objects whose binding was followed while resolving the
    /// admitted I/O path. Each key has its parent stabilized through preceding
    /// aliases while its final symlink entry remains unfollowed, so equivalent
    /// parent spellings compare as the same dependency object. Replacing one
    /// of these aliases requires WRITE and must conflict with this claim's
    /// implicit READ dependency.
    #[serde(
        default,
        serialize_with = "lossless_path_serde::serialize_vec",
        deserialize_with = "lossless_path_serde::deserialize_vec"
    )]
    pub namespace_dependencies: Vec<PathBuf>,
    #[serde(
        serialize_with = "lossless_path_serde::serialize",
        deserialize_with = "lossless_path_serde::deserialize"
    )]
    pub resolved_io_path: PathBuf,
    #[serde(
        serialize_with = "lossless_path_serde::serialize",
        deserialize_with = "lossless_path_serde::deserialize"
    )]
    pub canonical_existing_ancestor: PathBuf,
    #[serde(
        serialize_with = "lossless_path_serde::serialize",
        deserialize_with = "lossless_path_serde::deserialize"
    )]
    pub suffix: PathBuf,
    #[serde(default)]
    pub dev: Option<u64>,
    #[serde(default)]
    pub ino: Option<u64>,
}

fn absolute_path_preserving_component_order(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|error| format!("resolve current directory for path claim: {error}"))?
            .join(path))
    }
}

#[derive(Debug)]
struct OrderedPathResolution {
    resolved_io_path: PathBuf,
    canonical_existing_ancestor: PathBuf,
    suffix: PathBuf,
}

/// Resolve an absolute pathname in filesystem component order. Existing
/// prefixes are handed to `canonicalize` without first collapsing `..`, so an
/// earlier symlink participates exactly as it would in an actual filesystem
/// lookup. Once the first missing component is reached, only the prospective
/// suffix is normalized lexically, and it may not escape above the canonical
/// existing ancestor selected at that point.
fn resolve_follow_referent_ordered(absolute: &Path) -> Result<OrderedPathResolution, String> {
    use std::path::Component;

    if !absolute.is_absolute() {
        return Err(format!(
            "path claim resolution requires an absolute path: {}",
            absolute.display()
        ));
    }

    let components: Vec<_> = absolute.components().collect();
    let mut raw_prefix = PathBuf::new();
    let mut canonical_current: Option<PathBuf> = None;

    for (index, component) in components.iter().enumerate() {
        match component {
            Component::CurDir => continue,
            Component::Prefix(_) | Component::RootDir => {
                raw_prefix.push(component.as_os_str());
                if let Ok(canonical) = std::fs::canonicalize(&raw_prefix) {
                    canonical_current = Some(canonical);
                }
                continue;
            }
            Component::ParentDir => raw_prefix.push(".."),
            Component::Normal(name) => raw_prefix.push(name),
        }

        match std::fs::canonicalize(&raw_prefix) {
            Ok(canonical) => {
                canonical_current = Some(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let anchor = canonical_current.ok_or_else(|| {
                    format!(
                        "path claim has no existing ancestor before {}",
                        raw_prefix.display()
                    )
                })?;
                let mut prospective: Vec<std::ffi::OsString> = Vec::new();
                for suffix_component in &components[index..] {
                    match suffix_component {
                        Component::CurDir => {}
                        Component::Normal(name) => prospective.push(name.to_os_string()),
                        Component::ParentDir => {
                            if prospective.pop().is_none() {
                                return Err(format!(
                                    "prospective path claim escapes canonical existing ancestor {}: {}",
                                    anchor.display(),
                                    absolute.display()
                                ));
                            }
                        }
                        Component::Prefix(_) | Component::RootDir => {
                            return Err(format!(
                                "prospective path claim contains a new root after existing ancestor {}: {}",
                                anchor.display(),
                                absolute.display()
                            ));
                        }
                    }
                }
                let mut suffix = PathBuf::new();
                for component in prospective {
                    suffix.push(component);
                }
                return Ok(OrderedPathResolution {
                    resolved_io_path: anchor.join(&suffix),
                    canonical_existing_ancestor: anchor,
                    suffix,
                });
            }
            Err(error) => {
                return Err(format!(
                    "canonicalize path claim prefix {}: {error}",
                    raw_prefix.display()
                ));
            }
        }
    }

    let canonical = canonical_current.ok_or_else(|| {
        format!(
            "path claim has no canonical existing identity: {}",
            absolute.display()
        )
    })?;
    Ok(OrderedPathResolution {
        resolved_io_path: canonical.clone(),
        canonical_existing_ancestor: canonical,
        suffix: PathBuf::new(),
    })
}

impl ResolvedPathIdentity {
    pub fn resolve(path: &Path) -> Result<Self, String> {
        Self::resolve_with_semantics(path, PathResolutionSemantics::FollowReferent)
    }

    pub fn resolve_with_semantics(
        path: &Path,
        semantics: PathResolutionSemantics,
    ) -> Result<Self, String> {
        let original = path.to_path_buf();
        let absolute = absolute_path_preserving_component_order(path)?;
        let namespace_path = absolute.clone();

        let (resolution, namespace_dependencies, resolved_io_path, suffix) = match semantics {
            PathResolutionSemantics::FollowReferent => {
                let dependencies = namespace_symlink_dependencies(&absolute, true)?;
                let resolution = resolve_follow_referent_ordered(&absolute)?;
                let resolved = resolution.resolved_io_path.clone();
                let suffix = resolution.suffix.clone();
                (resolution, dependencies, resolved, suffix)
            }
            PathResolutionSemantics::NamespaceObject => {
                use std::path::Component;
                let final_component = absolute.components().last().ok_or_else(|| {
                    format!(
                        "namespace-object path claim has no final entry: {}",
                        absolute.display()
                    )
                })?;
                let final_name = match final_component {
                    Component::Normal(name) => name.to_os_string(),
                    Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        return Err(format!(
                            "namespace-object path claim requires a real final entry, not {}",
                            absolute.display()
                        ));
                    }
                };
                let parent = absolute.parent().ok_or_else(|| {
                    format!(
                        "namespace-object path claim has no parent: {}",
                        absolute.display()
                    )
                })?;
                let dependencies = namespace_symlink_dependencies(parent, true)?;
                let resolution = resolve_follow_referent_ordered(parent)?;
                let resolved = resolution.resolved_io_path.join(&final_name);
                let mut suffix = resolution.suffix.clone();
                suffix.push(&final_name);
                (resolution, dependencies, resolved, suffix)
            }
        };

        #[cfg(unix)]
        let (dev, ino) = match match semantics {
            PathResolutionSemantics::FollowReferent => std::fs::metadata(&resolved_io_path),
            PathResolutionSemantics::NamespaceObject => std::fs::symlink_metadata(&resolved_io_path),
        } {
            Ok(metadata)
                if metadata.is_file()
                    || (matches!(semantics, PathResolutionSemantics::NamespaceObject)
                        && metadata.file_type().is_symlink()) =>
            {
                use std::os::unix::fs::MetadataExt;
                (Some(metadata.dev()), Some(metadata.ino()))
            }
            _ => (None, None),
        };
        #[cfg(not(unix))]
        let (dev, ino) = (None, None);

        Ok(Self {
            original,
            namespace_path,
            namespace_dependencies,
            resolved_io_path,
            canonical_existing_ancestor: resolution.canonical_existing_ancestor,
            suffix,
            dev,
            ino,
        })
    }

    fn normalized_key(&self) -> &Path {
        &self.resolved_io_path
    }
}

fn namespace_symlink_dependencies(
    namespace_path: &Path,
    include_final: bool,
) -> Result<Vec<PathBuf>, String> {
    use std::collections::HashSet;

    // Dependency keys name the namespace object that a peer would replace,
    // not the spelling used by this caller. Prefix inspection deliberately
    // preserves component order: `alias/..` is presented to the filesystem as
    // such, so an earlier symlink is followed before the parent component is
    // interpreted. Stabilizing each symlink's parent then appending its
    // basename gives equivalent canonical-parent spellings the same key.
    let mut dependencies = Vec::new();
    let mut seen = HashSet::new();
    discover_namespace_symlink_dependencies(
        namespace_path,
        include_final,
        0,
        &mut dependencies,
        &mut seen,
    )?;
    Ok(dependencies)
}

const MAX_NAMESPACE_DEPENDENCY_SYMLINK_DEPTH: usize = 40;

fn discover_namespace_symlink_dependencies(
    path: &Path,
    include_final: bool,
    depth: usize,
    dependencies: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), String> {
    use std::path::Component;

    if depth > MAX_NAMESPACE_DEPENDENCY_SYMLINK_DEPTH {
        return Err(format!(
            "path-claim namespace dependency traversal exceeded {} symlink levels at {}",
            MAX_NAMESPACE_DEPENDENCY_SYMLINK_DEPTH,
            path.display()
        ));
    }

    let absolute = absolute_path_preserving_component_order(path)?;
    let components: Vec<_> = absolute.components().collect();
    let final_index = components.iter().rposition(|component| {
        matches!(component, Component::Normal(_))
    });
    let mut prefix = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::CurDir => continue,
            Component::Prefix(_) | Component::RootDir => {
                prefix.push(component.as_os_str());
                continue;
            }
            Component::ParentDir => {
                prefix.push("..");
                // Keep the raw ordered prefix. The next filesystem lookup will
                // resolve an earlier symlink before interpreting this `..`.
                continue;
            }
            Component::Normal(name) => prefix.push(name),
        }

        if !include_final && Some(index) == final_index {
            break;
        }
        let metadata = match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "inspect path-claim namespace component {}: {error}",
                    prefix.display()
                ));
            }
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }

        let basename = prefix.file_name().ok_or_else(|| {
            format!(
                "path-claim namespace dependency has no final entry: {}",
                prefix.display()
            )
        })?;
        let parent = prefix.parent().ok_or_else(|| {
            format!(
                "path-claim namespace dependency has no parent: {}",
                prefix.display()
            )
        })?;
        let stabilized_parent = std::fs::canonicalize(parent).map_err(|error| {
            format!(
                "stabilize path-claim namespace dependency parent {}: {error}",
                parent.display()
            )
        })?;
        let dependency = stabilized_parent.join(basename);
        let first_visit = seen.insert(dependency.clone());
        if first_visit {
            dependencies.push(dependency.clone());
        }

        // A symlink target can itself traverse namespace aliases that never
        // appear as prefixes of the caller's spelling. Follow that target only
        // for admission-time dependency discovery; the key above still names
        // the non-followed symlink object itself.
        if first_visit {
            let target = std::fs::read_link(&prefix).map_err(|error| {
                format!(
                    "read path-claim namespace dependency {}: {error}",
                    prefix.display()
                )
            })?;
            let target_path = if target.is_absolute() {
                target
            } else {
                stabilized_parent.join(target)
            };
            discover_namespace_symlink_dependencies(
                &target_path,
                true,
                depth + 1,
                dependencies,
                seen,
            )?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathClaim {
    pub identity: ResolvedPathIdentity,
    pub mode: ClaimMode,
    pub scope: ClaimScope,
}

impl PathClaim {
    pub fn resolve(path: &Path, mode: ClaimMode, scope: ClaimScope) -> Result<Self, String> {
        Self::resolve_with_semantics(
            path,
            mode,
            scope,
            PathResolutionSemantics::FollowReferent,
        )
    }

    pub fn resolve_with_semantics(
        path: &Path,
        mode: ClaimMode,
        scope: ClaimScope,
        semantics: PathResolutionSemantics,
    ) -> Result<Self, String> {
        Ok(Self {
            identity: ResolvedPathIdentity::resolve_with_semantics(path, semantics)?,
            mode,
            scope,
        })
    }

    pub fn conflicts_with(&self, other: &Self) -> bool {
        if self.mode == ClaimMode::Read && other.mode == ClaimMode::Read {
            return false;
        }
        if self.identity.dev.is_some()
            && self.identity.dev == other.identity.dev
            && self.identity.ino.is_some()
            && self.identity.ino == other.identity.ino
        {
            return true;
        }
        let left = self.identity.normalized_key();
        let right = other.identity.normalized_key();
        let resolved_conflict = left == right
            || matches!(self.scope, ClaimScope::Subtree) && right.starts_with(left)
            || matches!(other.scope, ClaimScope::Subtree) && left.starts_with(right);
        if resolved_conflict {
            return true;
        }
        let left_namespace = &self.identity.namespace_path;
        let right_namespace = &other.identity.namespace_path;
        let namespace_conflict = !left_namespace.as_os_str().is_empty()
            && !right_namespace.as_os_str().is_empty()
            && (left_namespace == right_namespace
                || matches!(self.scope, ClaimScope::Subtree) && right_namespace.starts_with(left_namespace)
                || matches!(other.scope, ClaimScope::Subtree) && left_namespace.starts_with(right_namespace));
        if namespace_conflict {
            return true;
        }

        fn write_touches_dependency(writer: &PathClaim, dependency: &Path) -> bool {
            if writer.mode != ClaimMode::Write {
                return false;
            }
            let touches = |path: &Path| {
                path == dependency
                    || (matches!(writer.scope, ClaimScope::Subtree)
                        && dependency.starts_with(path))
            };
            touches(&writer.identity.namespace_path)
                || touches(writer.identity.normalized_key())
        }

        self.identity
            .namespace_dependencies
            .iter()
            .any(|dependency| write_touches_dependency(other, dependency))
            || other
                .identity
                .namespace_dependencies
                .iter()
                .any(|dependency| write_touches_dependency(self, dependency))
    }

    /// Return true when this admitted capability is at least as strong and at
    /// least as broad as `other`. This is deliberately one-way: a READ claim
    /// never covers a WRITE, and an Exact claim never covers a Subtree claim.
    pub fn covers(&self, other: &Self) -> bool {
        if self.mode == ClaimMode::Read && other.mode == ClaimMode::Write {
            return false;
        }
        if self.scope == ClaimScope::Exact && other.scope == ClaimScope::Subtree {
            return false;
        }

        let covers_path = |outer: &Path, inner: &Path| {
            outer == inner || (self.scope == ClaimScope::Subtree && inner.starts_with(outer))
        };
        let dependencies_covered = other.identity.namespace_dependencies.iter().all(|dependency| {
            self.identity.namespace_dependencies.contains(dependency)
                || (!self.identity.namespace_path.as_os_str().is_empty()
                    && covers_path(&self.identity.namespace_path, dependency))
                || covers_path(self.identity.normalized_key(), dependency)
        });
        if !dependencies_covered {
            return false;
        }
        if covers_path(
            self.identity.normalized_key(),
            other.identity.normalized_key(),
        ) {
            return true;
        }

        let outer_namespace = &self.identity.namespace_path;
        let inner_namespace = &other.identity.namespace_path;
        !outer_namespace.as_os_str().is_empty()
            && !inner_namespace.as_os_str().is_empty()
            && covers_path(outer_namespace, inner_namespace)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseDescriptor {
    schema: u32,
    descriptor_id: Uuid,
    family: LeaseFamily,
    owner: OwnerProcessIdentity,
    created_unix_ms: u64,
    #[serde(default)]
    claims: Vec<PathClaim>,
    /// Run-unique internal cohort allowed to co-hold otherwise-overlapping
    /// claims (used for same-album batch siblings). Never derived from user
    /// metadata or a stable path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coordination_group: Option<String>,
}

pub struct PersistentLease {
    file: Arc<File>,
    descriptor_path: PathBuf,
    descriptor_id: Uuid,
    family: LeaseFamily,
    claims: Arc<[PathClaim]>,
    // A fork may transiently duplicate any CLOEXEC fd until the child reaches
    // exec; that accidental co-holder is not transferable mutation authority.
    // An explicit lifetime-file export is different: it may intentionally
    // outlive this Rust lease and is part of the cross-process authority
    // protocol. Once one has ever been handed out, guard teardown must leave
    // descriptor retirement to the ordinary lock-aware scanner rather than
    // hiding a still-live exported OFD.
    lifetime_file_exported: std::sync::atomic::AtomicBool,
}

/// Process-local view of descriptor handles created by this process. Weak
/// references deliberately do not extend lease lifetime; they only let a
/// recovery path co-hold the exact already-locked open-file description when
/// the durable lifecycle itself has explicitly become recoverable before the
/// creating handle is dropped (notably deterministic in-process recovery
/// tests and same-process handoff). Foreign owners can never enter this path.
fn local_persistent_lease_files() -> &'static Mutex<HashMap<PathBuf, std::sync::Weak<File>>> {
    static FILES: OnceLock<Mutex<HashMap<PathBuf, std::sync::Weak<File>>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_local_persistent_lease(path: &Path, file: &Arc<File>) {
    local_persistent_lease_files()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), Arc::downgrade(file));
}

fn unregister_local_persistent_lease(path: &Path, file: &Arc<File>) {
    let mut files = local_persistent_lease_files()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let remove = match files.get(path).and_then(std::sync::Weak::upgrade) {
        // `registered` is one temporary strong reference and `file` is this
        // lease's reference. A count of two therefore means this is the last
        // process-local co-holder of the registered open-file description.
        Some(registered) => Arc::ptr_eq(&registered, file) && Arc::strong_count(&registered) == 2,
        None => true,
    };
    if remove {
        files.remove(path);
    }
}

fn local_persistent_lease_file(path: &Path) -> Option<Arc<File>> {
    let mut files = local_persistent_lease_files()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let file = files.get(path).and_then(std::sync::Weak::upgrade);
    if file.is_none() {
        files.remove(path);
    }
    file
}

fn current_process_coheld_descriptor(
    path: &Path,
    descriptor: &LeaseDescriptor,
) -> Result<Option<Arc<File>>, String> {
    // Same-process co-holding exists solely for durable file-task journal
    // recovery. Keep every other lease family on the ordinary exclusive-lock
    // path so this narrow handoff cannot grow into a generic local bypass.
    if !matches!(&descriptor.family, LeaseFamily::JournalOperation { .. })
        || descriptor.owner != OwnerProcessIdentity::current()
    {
        return Ok(None);
    }
    let Some(file) = local_persistent_lease_file(path) else {
        return Ok(None);
    };
    verify_coordination_path_binding(&file, path, "process-local persistent lease")?;
    Ok(Some(file))
}

/// Removes a coordination pathname owned by this creation attempt on ordinary
/// error or unwind. On Unix the cleanup is inode-bound so a same-user pathname
/// rebind cannot make the guard remove somebody else's file. SIGKILL/power-loss
/// recovery is handled by atomic publication plus scanner/lifecycle repair.
struct PendingPathCleanup {
    path: PathBuf,
    #[cfg(unix)]
    expected_dev: u64,
    #[cfg(unix)]
    expected_ino: u64,
    armed: bool,
}

impl PendingPathCleanup {
    fn new(path: PathBuf, file: &File) -> Result<Self, String> {
        let metadata = file
            .metadata()
            .map_err(|error| format!("fstat pending coordination file {}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "pending coordination file is not regular: {}",
                path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                path,
                expected_dev: metadata.dev(),
                expected_ino: metadata.ino(),
                armed: true,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self { path, armed: true })
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn remove_owned_path_now(&mut self) -> Result<(), String> {
        if !self.still_owns_path() {
            return Err(format!(
                "coordination pathname rebound before cleanup: {}",
                self.path.display()
            ));
        }
        std::fs::remove_file(&self.path).map_err(|error| {
            format!(
                "remove owned coordination pathname {}: {error}",
                self.path.display()
            )
        })?;
        self.disarm();
        Ok(())
    }

    fn still_owns_path(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
                return false;
            };
            metadata.file_type().is_file()
                && metadata.dev() == self.expected_dev
                && metadata.ino() == self.expected_ino
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

impl Drop for PendingPathCleanup {
    fn drop(&mut self) {
        if !self.armed || !self.still_owns_path() {
            return;
        }
        if std::fs::remove_file(&self.path).is_ok() {
            if let Some(parent) = self.path.parent() {
                let _ = sync_coordination_directory(parent);
            }
        }
    }
}

impl std::fmt::Debug for PersistentLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentLease")
            .field("descriptor_path", &self.descriptor_path)
            .field("descriptor_id", &self.descriptor_id)
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

fn open_existing_descriptor(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

fn verify_coordination_path_binding(
    file: &File,
    path: &Path,
    label: &str,
) -> Result<std::fs::Metadata, String> {
    let descriptor_metadata = file
        .metadata()
        .map_err(|e| format!("fstat {label} {}: {e}", path.display()))?;
    if !descriptor_metadata.file_type().is_file() {
        return Err(format!("{label} descriptor is not a regular file: {}", path.display()));
    }
    let pathname_metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("lstat {label} pathname {}: {e}", path.display()))?;
    if !pathname_metadata.file_type().is_file() {
        return Err(format!("{label} pathname is not a regular file: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if descriptor_metadata.dev() != pathname_metadata.dev()
            || descriptor_metadata.ino() != pathname_metadata.ino()
        {
            return Err(format!("{label} pathname rebound after open: {}", path.display()));
        }
    }
    Ok(descriptor_metadata)
}

fn sync_coordination_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let directory = File::open(path)
            .map_err(|e| format!("open coordination directory for fsync {}: {e}", path.display()))?;
        if let Err(error) = directory.sync_all() {
            #[cfg(target_os = "macos")]
            if matches!(error.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTSUP)) {
                return Ok(());
            }
            return Err(format!(
                "fsync coordination directory {}: {error}",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn reclaim_empty_descriptor_from_locked_file(file: &File, path: &Path) -> Result<bool, String> {
    let metadata =
        verify_coordination_path_binding(file, path, "empty coordination descriptor")?;
    if metadata.len() != 0 {
        return Ok(false);
    }
    let mut cleanup = PendingPathCleanup::new(path.to_path_buf(), file)?;
    cleanup.remove_owned_path_now().map_err(|error| {
        format!(
            "reclaim empty coordination descriptor {}: {error}",
            path.display()
        )
    })?;
    if let Some(parent) = path.parent() {
        sync_coordination_directory(parent)?;
    }
    Ok(true)
}

fn structurally_ephemeral_descriptor_path(path: &Path) -> bool {
    if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        != Some("ephemeral-mutation")
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(base) = name.strip_suffix(".lease") else {
        return false;
    };
    let Some((lifecycle, descriptor)) = base.split_once("--") else {
        return false;
    };
    Uuid::parse_str(lifecycle).is_ok() && Uuid::parse_str(descriptor).is_ok()
}

fn reclaim_invalid_ephemeral_descriptor_from_locked_file(
    file: &File,
    path: &Path,
) -> Result<bool, String> {
    if !structurally_ephemeral_descriptor_path(path) {
        return Ok(false);
    }
    verify_coordination_path_binding(file, path, "invalid ephemeral coordination descriptor")?;
    let mut cleanup = PendingPathCleanup::new(path.to_path_buf(), file)?;
    cleanup.remove_owned_path_now().map_err(|error| {
        format!(
            "reclaim invalid ephemeral coordination descriptor {}: {error}",
            path.display()
        )
    })?;
    if let Some(parent) = path.parent() {
        sync_coordination_directory(parent)?;
    }
    Ok(true)
}

fn reclaim_unlocked_empty_descriptor_locked(path: &Path) -> Result<bool, String> {
    let file = match open_existing_descriptor(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(format!(
                "open possible empty coordination descriptor {}: {error}",
                path.display()
            ))
        }
    };
    match file.try_lock_exclusive() {
        Ok(()) => reclaim_empty_descriptor_from_locked_file(&file, path),
        Err(error) if is_lock_contended(&error) => Ok(false),
        Err(error) => Err(format!(
            "lock possible empty coordination descriptor {}: {error}",
            path.display()
        )),
    }
}

fn structurally_descriptor_temp_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(name) = name.strip_prefix('.') else {
        return false;
    };
    let Some((identity, temp_id)) = name.split_once(".lease.tmp-") else {
        return false;
    };
    let Some((lifecycle_id, descriptor_id)) = identity.split_once("--") else {
        return false;
    };
    Uuid::parse_str(lifecycle_id).is_ok()
        && Uuid::parse_str(descriptor_id).is_ok()
        && Uuid::parse_str(temp_id).is_ok()
}

fn remove_abandoned_descriptor_temp_locked(path: &Path) -> Result<bool, String> {
    if !structurally_descriptor_temp_path(path) {
        return Ok(false);
    }
    let file = match open_existing_descriptor(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "open abandoned persistent lease staging file {}: {error}",
                path.display()
            ))
        }
    };
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if is_lock_contended(&error) => return Ok(false),
        Err(error) => {
            return Err(format!(
                "lock abandoned persistent lease staging file {}: {error}",
                path.display()
            ))
        }
    }
    verify_coordination_path_binding(&file, path, "persistent lease staging file")?;
    let mut cleanup = PendingPathCleanup::new(path.to_path_buf(), &file)?;
    cleanup.remove_owned_path_now().map_err(|error| {
        format!(
            "remove abandoned persistent lease staging file {}: {error}",
            path.display()
        )
    })?;
    Ok(true)
}

fn cleanup_abandoned_descriptor_temps_locked(family_dir: &Path) -> Result<(), String> {
    let mut removed_any = false;
    for entry in std::fs::read_dir(family_dir)
        .map_err(|e| format!("read persistent lease family {}: {e}", family_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read persistent lease family entry: {e}"))?;
        removed_any |= remove_abandoned_descriptor_temp_locked(&entry.path())?;
    }
    if removed_any {
        sync_coordination_directory(family_dir)?;
    }
    Ok(())
}

impl PersistentLease {
    pub fn create(family: LeaseFamily, claims: &[PathClaim]) -> Result<Self, String> {
        let root = coordination_root();
        create_private_dir(&root)?;
        let _registry = RegistryLock::acquire(&root)?;
        Self::create_while_registry_locked(&root, family, claims, None, false)
    }

    fn create_while_registry_locked(
        root: &Path,
        family: LeaseFamily,
        claims: &[PathClaim],
        coordination_group: Option<String>,
        registry_scan_swept_staging: bool,
    ) -> Result<Self, String> {
        let family_dir = root.join(family.namespace());
        create_private_dir(&family_dir)?;
        let lifecycle_id = family.lifecycle_id();
        let singular_lifecycle = matches!(
            &family,
            LeaseFamily::JournalOperation { .. }
                | LeaseFamily::QueueScope { .. }
                | LeaseFamily::QueueExecution { .. }
        );
        if singular_lifecycle {
            let prefix = format!("{lifecycle_id}--");
            let mut removed_staging = false;
            for entry in std::fs::read_dir(&family_dir)
                .map_err(|e| format!("read persistent lease family {}: {e}", family_dir.display()))?
            {
                let entry = entry.map_err(|e| format!("read persistent lease family entry: {e}"))?;
                let path = entry.path();
                if !registry_scan_swept_staging {
                    removed_staging |= remove_abandoned_descriptor_temp_locked(&path)?;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".lease") {
                    if reclaim_unlocked_empty_descriptor_locked(&path)? {
                        continue;
                    }
                    return Err(format!(
                        "persistent lease lifecycle already has a descriptor: {:?} at {}",
                        family,
                        path.display()
                    ));
                }
            }
            if removed_staging {
                sync_coordination_directory(&family_dir)?;
            }
        } else if !registry_scan_swept_staging {
            cleanup_abandoned_descriptor_temps_locked(&family_dir)?;
        }
        let descriptor_id = Uuid::new_v4();
        let path = family_dir.join(format!("{lifecycle_id}--{descriptor_id}.lease"));
        // Complete every fallible/allocating in-memory preparation before any
        // pathname is published. Once `final_cleanup` is disarmed below, the
        // success path is move-only.
        let descriptor_claims = claims.to_vec();
        let retained_claims: Arc<[PathClaim]> = descriptor_claims.clone().into();
        let body = LeaseDescriptor {
            schema: DESCRIPTOR_SCHEMA,
            descriptor_id,
            family: family.clone(),
            owner: OwnerProcessIdentity::current(),
            created_unix_ms: unix_ms(),
            claims: descriptor_claims,
            coordination_group,
        };
        let encoded = serde_json::to_vec(&body)
            .map_err(|e| format!("serialize persistent lease {}: {e}", path.display()))?;
        // EphemeralMutation has no post-crash recovery authority: after a
        // machine loss every holder is dead and its unlocked descriptor is
        // reclaimable. Durable lifecycle families, by contrast, must preserve
        // their descriptor body across power loss because it reserves recovery
        // authority after the owner disappears.
        let crash_durable_descriptor = !matches!(
            &family,
            LeaseFamily::EphemeralMutation { .. }
        );

        // Build the complete descriptor on an unscanned name first. The file
        // lock is taken before any bytes are written and remains attached to
        // this inode after no-clobber publication below.
        let temp_path = family_dir.join(format!(
            ".{lifecycle_id}--{descriptor_id}.lease.tmp-{}",
            Uuid::new_v4()
        ));
        let mut temp_options = OpenOptions::new();
        temp_options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            temp_options
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .mode(0o600);
        }
        let mut file = temp_options.open(&temp_path).map_err(|e| {
            format!(
                "create persistent lease staging file {}: {e}",
                temp_path.display()
            )
        })?;
        let mut temp_cleanup = PendingPathCleanup::new(temp_path.clone(), &file)?;
        set_private_file_permissions(&file)?;
        file.try_lock_exclusive().map_err(|e| {
            format!(
                "lock new persistent lease staging file {}: {e}",
                temp_path.display()
            )
        })?;
        file.write_all(&encoded).map_err(|e| {
            format!(
                "write persistent lease staging file {}: {e}",
                temp_path.display()
            )
        })?;
        if crash_durable_descriptor {
            file.sync_all().map_err(|e| {
                format!(
                    "fsync persistent lease staging file {}: {e}",
                    temp_path.display()
                )
            })?;
        } else {
            // `File::flush` preserves the previous cheap ephemeral behavior;
            // no durable recovery authority survives a machine loss.
            file.flush().map_err(|e| {
                format!(
                    "flush persistent lease staging file {}: {e}",
                    temp_path.display()
                )
            })?;
        }
        verify_coordination_path_binding(&file, &temp_path, "persistent lease staging file")?;

        // Publish with a filesystem-level no-clobber operation. A hard link is
        // atomic, fails if `path` already exists, and names the already complete
        // locked inode directly. Thus scanners can never observe an empty or
        // partially written descriptor created by this implementation. The
        // temp link is removed only after the final link is verified.
        std::fs::hard_link(&temp_path, &path).map_err(|e| {
            format!(
                "publish persistent lease {} from {} without clobber: {e}",
                path.display(),
                temp_path.display()
            )
        })?;
        let mut final_cleanup = PendingPathCleanup::new(path.clone(), &file)?;
        verify_coordination_path_binding(&file, &path, "published persistent lease")?;
        if crash_durable_descriptor {
            // Make the final link durable before retiring the staging link. If
            // power is lost after this point, recovery authority is reachable
            // through `path` even if the subsequent unlink is only partially
            // persisted.
            sync_coordination_directory(&family_dir)?;
        }
        temp_cleanup.remove_owned_path_now().map_err(|error| {
            format!(
                "remove published persistent lease staging link {}: {error}",
                temp_path.display()
            )
        })?;
        // Do not fsync the directory again solely for staging-link retirement.
        // The pre-unlink directory sync above already makes the final recovery
        // pathname durable. If a crash resurrects this hidden hard-link name,
        // startup/admission cleanup recognizes and removes it.
        // Close the publication window with the same pathname/inode binding
        // check used by readers. Do not return an authority whose public name
        // was rebound while publication cleanup was in progress.
        verify_coordination_path_binding(&file, &path, "published persistent lease")?;
        final_cleanup.disarm();
        let file = Arc::new(file);
        if matches!(&family, LeaseFamily::JournalOperation { .. }) {
            register_local_persistent_lease(&path, &file);
        }
        Ok(Self {
            file,
            descriptor_path: path,
            descriptor_id,
            family,
            claims: retained_claims,
            lifetime_file_exported: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Acquire durable recovery authority using the global lock order.
    /// Classification may happen lock-free beforehand, but any transition from
    /// RecoveryReserved to a live recovery owner must take registry -> descriptor.
    /// This ordinary entry point remains strict: any locked descriptor is live.
    pub fn acquire_existing_recovery(
        path: &Path,
        expected_family: &LeaseFamily,
    ) -> Result<Self, String> {
        Self::acquire_existing_recovery_internal(path, expected_family, false)
    }

    /// Recover a durable lifecycle after the owning subsystem has durably
    /// established that a same-process handle is only a stale handoff holder.
    /// Foreign-process owners remain live and cannot enter this path.
    pub fn acquire_existing_recovery_with_local_handoff(
        path: &Path,
        expected_family: &LeaseFamily,
    ) -> Result<Self, String> {
        Self::acquire_existing_recovery_internal(path, expected_family, true)
    }

    fn acquire_existing_recovery_internal(
        path: &Path,
        expected_family: &LeaseFamily,
        allow_local_handoff: bool,
    ) -> Result<Self, String> {
        let root = coordination_root();
        create_private_dir(&root)?;
        let _registry = RegistryLock::acquire(&root)?;
        let mut opened = open_existing_descriptor(path)
            .map_err(|e| format!("open persistent lease {}: {e}", path.display()))?;
        match opened.try_lock_exclusive() {
            Ok(()) => {
                let descriptor = read_descriptor_from(&mut opened, path)?;
                if &descriptor.family != expected_family {
                    return Err(format!(
                        "persistent lease family mismatch for {}: expected {:?}, found {:?}",
                        path.display(), expected_family, descriptor.family
                    ));
                }
                let file = Arc::new(opened);
                if matches!(&descriptor.family, LeaseFamily::JournalOperation { .. })
                    && descriptor.owner == OwnerProcessIdentity::current()
                {
                    register_local_persistent_lease(path, &file);
                }
                return Ok(Self {
                    file,
                    descriptor_path: path.to_path_buf(),
                    descriptor_id: descriptor.descriptor_id,
                    family: descriptor.family,
                    claims: descriptor.claims.into(),
                    lifetime_file_exported: std::sync::atomic::AtomicBool::new(false),
                });
            }
            Err(error) if is_lock_contended(&error) => {}
            Err(error) => {
                return Err(format!("lock persistent lease {}: {error}", path.display()));
            }
        }

        let descriptor = read_descriptor_from(&mut opened, path)?;
        if &descriptor.family != expected_family {
            return Err(format!(
                "persistent lease family mismatch for {}: expected {:?}, found {:?}",
                path.display(), expected_family, descriptor.family
            ));
        }
        if !allow_local_handoff {
            return Err(format!("persistent lease is live-owned: {}", path.display()));
        }

        // The caller has already proven a durable same-process handoff state.
        // Co-hold only the exact locally-created locked OFD; never unlock,
        // duplicate by pathname, or bypass a foreign owner's descriptor.
        let Some(file) = current_process_coheld_descriptor(path, &descriptor)? else {
            return Err(format!("persistent lease is live-owned: {}", path.display()));
        };
        Ok(Self {
            file,
            descriptor_path: path.to_path_buf(),
            descriptor_id: descriptor.descriptor_id,
            family: descriptor.family,
            claims: descriptor.claims.into(),
            lifetime_file_exported: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn acquire_existing(path: &Path, expected_family: &LeaseFamily) -> Result<Self, String> {
        let mut file = open_existing_descriptor(path)
            .map_err(|e| format!("open persistent lease {}: {e}", path.display()))?;
        file.try_lock_exclusive().map_err(|e| {
            if is_lock_contended(&e) {
                format!("persistent lease is live-owned: {}", path.display())
            } else {
                format!("lock persistent lease {}: {e}", path.display())
            }
        })?;
        let descriptor = read_descriptor_from(&mut file, path)?;
        if &descriptor.family != expected_family {
            return Err(format!(
                "persistent lease family mismatch for {}: expected {:?}, found {:?}",
                path.display(), expected_family, descriptor.family
            ));
        }
        Ok(Self {
            file: Arc::new(file),
            descriptor_path: path.to_path_buf(),
            descriptor_id: descriptor.descriptor_id,
            family: descriptor.family,
            claims: descriptor.claims.into(),
            lifetime_file_exported: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn descriptor_path(&self) -> &Path { &self.descriptor_path }
    pub fn descriptor_id(&self) -> Uuid { self.descriptor_id }
    pub fn family(&self) -> &LeaseFamily { &self.family }
    pub fn claims(&self) -> &[PathClaim] { &self.claims }

    /// Duplicate the descriptor while preserving the same underlying open-file
    /// description. The duplicate is close-only and is suitable for handoff to
    /// a trusted tonepoet supervisor; no caller receives an unlock primitive.
    ///
    /// Record successful export before returning it. An exported descriptor may
    /// intentionally outlive a `MutationClaimGuard`, so guard teardown must not
    /// unlink the public descriptor while that duplicate can still hold `flock`.
    pub fn duplicate_lifetime_file(&self) -> Result<Arc<File>, String> {
        let duplicate = self.file.try_clone().map_err(|e| {
            format!(
                "duplicate persistent lease {}: {e}",
                self.descriptor_path.display()
            )
        })?;
        self.lifetime_file_exported
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(Arc::new(duplicate))
    }

    /// End the public lifetime of an ordinary ephemeral guard before its file
    /// descriptor closes. This turns guard teardown into an explicit state
    /// transition instead of asking a later admission to discover and reclaim
    /// an unlocked stale descriptor. It also prevents a transient fork-time
    /// CLOEXEC duplicate from extending public mutation authority after the
    /// lexical guard has ended but before the child reaches exec.
    ///
    /// A lease that has exported a lifetime fd is deliberately excluded: the
    /// exported fd is real authority and may still be live after the guard is
    /// gone. In that case the existing scanner remains responsible for lazy
    /// retirement after the final holder closes.
    fn retire_ephemeral_descriptor_on_guard_drop(&self) {
        if !matches!(&self.family, LeaseFamily::EphemeralMutation { .. })
            || self
                .lifetime_file_exported
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }

        // Never remove a pathname merely because its UUID matches. Prove that
        // the public path still names this exact open descriptor first. The
        // Unix verifier compares dev/ino; same-file supplies the equivalent
        // file-identity check on other platforms.
        #[cfg(unix)]
        if verify_coordination_path_binding(
            self.file.as_ref(),
            &self.descriptor_path,
            "ephemeral guard retirement",
        )
        .is_err()
        {
            return;
        }

        #[cfg(not(unix))]
        {
            let Ok(held_file) = self.file.try_clone() else {
                return;
            };
            let Ok(held) = same_file::Handle::from_file(held_file) else {
                return;
            };
            let Ok(current) = same_file::Handle::from_path(&self.descriptor_path) else {
                return;
            };
            if held != current {
                return;
            }
        }

        // Ephemeral descriptors carry no post-crash recovery authority, so a
        // directory fsync would buy nothing here. Removal is best-effort in a
        // destructor: if it fails, closing the lease still makes the descriptor
        // ReclaimableEphemeral and the established scanner path repairs it.
        let _ = std::fs::remove_file(&self.descriptor_path);
    }

    #[cfg(unix)]
    pub fn inherited_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        // Returning a raw fd explicitly exposes authority outside this lease's
        // lexical ownership. Treat it exactly like duplicate_lifetime_file so
        // guard Drop can never hide a descriptor a caller may deliberately
        // arrange to survive in another process.
        self.lifetime_file_exported
            .store(true, std::sync::atomic::Ordering::Release);
        self.file.as_raw_fd()
    }

    #[cfg(unix)]
    pub unsafe fn from_inherited_fd(
        raw_fd: std::os::fd::RawFd,
        descriptor_path: PathBuf,
        expected_family: LeaseFamily,
    ) -> Result<Self, String> {
        use std::os::fd::FromRawFd;
        let mut file = unsafe { File::from_raw_fd(raw_fd) };
        let descriptor = read_descriptor_from(&mut file, &descriptor_path)?;
        if descriptor.family != expected_family {
            return Err(format!("inherited persistent lease family mismatch for {}", descriptor_path.display()));
        }
        Ok(Self {
            file: Arc::new(file),
            descriptor_path,
            descriptor_id: descriptor.descriptor_id,
            family: descriptor.family,
            claims: descriptor.claims.into(),
            lifetime_file_exported: std::sync::atomic::AtomicBool::new(true),
        })
    }
}

impl Drop for PersistentLease {
    fn drop(&mut self) {
        // This is bookkeeping only: pruning the weak process-local co-hold
        // index neither unlocks nor unlinks authority. Only JournalOperation
        // descriptors participate, so ordinary ephemeral/queue leases pay no
        // recovery-index synchronization cost on drop.
        if matches!(&self.family, LeaseFamily::JournalOperation { .. }) {
            unregister_local_persistent_lease(&self.descriptor_path, &self.file);
        }
    }
}

#[derive(Debug)]
pub struct MutationClaimGuard {
    // `Option` lets `into_lease` explicitly transfer authority even though this
    // wrapper has a Drop implementation. Ordinary drop keeps the lease present
    // long enough to retire its ephemeral descriptor before the locked file is
    // closed by field destruction.
    lease: Option<PersistentLease>,
    claims: std::sync::Arc<[PathClaim]>,
}

impl MutationClaimGuard {
    pub fn acquire_ephemeral(claims: Vec<PathClaim>) -> Result<Self, String> {
        Self::acquire(LeaseFamily::EphemeralMutation { claim_id: Uuid::new_v4() }, claims)
    }

    pub fn acquire(family: LeaseFamily, claims: Vec<PathClaim>) -> Result<Self, String> {
        Self::acquire_grouped(family, claims, None)
    }

    pub fn acquire_grouped(
        family: LeaseFamily,
        claims: Vec<PathClaim>,
        coordination_group: Option<String>,
    ) -> Result<Self, String> {
        Self::acquire_grouped_internal(family, claims, coordination_group, None)
    }

    /// Upgrade-only admission for an explicitly selected ownerless legacy
    /// journal. The caller has already performed close-old-session confirmation;
    /// this operation publishes recovery authority only and never resumes user
    /// mutation. Other legacy journals remain activation blockers until they are
    /// separately adopted/reconciled. The registry/descriptor protocol itself is
    /// exactly the ordinary mutation-admission protocol.
    pub fn acquire_legacy_journal_adoption(
        family: LeaseFamily,
        claims: Vec<PathClaim>,
        legacy_journal: &Path,
    ) -> Result<Self, String> {
        Self::acquire_grouped_internal(family, claims, None, Some(legacy_journal))
    }

    fn acquire_grouped_internal(
        family: LeaseFamily,
        claims: Vec<PathClaim>,
        coordination_group: Option<String>,
        legacy_exception: Option<&Path>,
    ) -> Result<Self, String> {
        reject_detectable_legacy_mutation_ambiguity_except(legacy_exception)?;
        let root = coordination_root();
        create_private_dir(&root)?;
        let _registry = RegistryLock::acquire(&root)?;
        let descriptors = descriptor_paths(&root)?;
        for path in descriptors {
            let probe = classify_descriptor(&path)?;
            let Some((availability, existing_family, existing_claims, existing_group)) = probe else { continue };
            if same_execution_lifecycle(&family, &existing_family)
                || coordination_group.as_deref().is_some_and(|group| existing_group.as_deref() == Some(group))
            {
                continue;
            }
            if matches!(availability, ClaimAvailability::ReclaimableEphemeral) {
                let _ = remove_reclaimable_ephemeral_locked(&path);
                continue;
            }
            if let Some((requested, existing)) = first_conflict(&claims, &existing_claims) {
                let owner = match availability {
                    ClaimAvailability::Live => "live owner",
                    ClaimAvailability::RecoveryReserved => "recovery reservation",
                    ClaimAvailability::ReclaimableEphemeral => unreachable!(),
                };
                return Err(format!(
                    "filesystem mutation conflicts with {owner}: '{}' overlaps '{}'",
                    requested.identity.original.display(), existing.identity.original.display()
                ));
            }
        }
        let lease = PersistentLease::create_while_registry_locked(
            &root,
            family,
            &claims,
            coordination_group,
            true,
        )?;
        Ok(Self {
            lease: Some(lease),
            claims: claims.into(),
        })
    }

    pub fn claims(&self) -> &[PathClaim] { &self.claims }
    pub fn lease(&self) -> &PersistentLease {
        self.lease
            .as_ref()
            .expect("MutationClaimGuard lease is present until authority transfer")
    }

    pub fn into_lease(mut self) -> PersistentLease {
        self.lease
            .take()
            .expect("MutationClaimGuard lease can be transferred only once")
    }
}

impl Drop for MutationClaimGuard {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.as_ref() {
            lease.retire_ephemeral_descriptor_on_guard_drop();
        }
    }
}


/// Enumerate lifecycle ids from descriptor pathnames without trusting descriptor
/// bodies. This is intentionally lifecycle-repair-only: generic claim GC never
/// calls it, and callers must independently prove the durable backing object is
/// absent before retirement.
pub fn lifecycle_descriptor_hints(family_namespace: &LeaseFamily) -> Result<Vec<(Uuid, PathBuf)>, String> {
    let dir = coordination_root().join(family_namespace.namespace());
    if !dir.exists() { return Ok(Vec::new()); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("read lifecycle descriptor directory {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read lifecycle descriptor entry: {e}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some((lifecycle, rest)) = name.split_once("--") else { continue };
        if !rest.ends_with(".lease") { continue; }
        if let Ok(id) = Uuid::parse_str(lifecycle) { out.push((id, entry.path())); }
    }
    out.sort_by(|a,b| a.1.cmp(&b.1));
    Ok(out)
}

/// Lifecycle-owned setup-orphan retirement that can repair a torn/malformed
/// descriptor body. The caller must already have proved that the corresponding
/// durable journal/DB row was never published or has been terminally removed.
pub fn retire_setup_orphan_by_path_identity(path: &Path, expected_family: &LeaseFamily) -> Result<(), String> {
    let root = coordination_root();
    create_private_dir(&root)?;
    let _registry = RegistryLock::acquire(&root)?;
    let expected_prefix = format!("{}--", expected_family.lifecycle_id());
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("invalid descriptor pathname {}", path.display()))?;
    let expected_parent = root.join(expected_family.namespace());
    if path.parent() != Some(expected_parent.as_path())
        || !name.starts_with(&expected_prefix) || !name.ends_with(".lease")
    {
        return Err(format!("setup-orphan descriptor pathname does not match {:?}: {}", expected_family, path.display()));
    }
    let file = open_existing_descriptor(path)
        .map_err(|e| format!("open setup-orphan descriptor {}: {e}", path.display()))?;
    file.try_lock_exclusive().map_err(|e| {
        if is_lock_contended(&e) { format!("setup-orphan descriptor still has a live holder: {}", path.display()) }
        else { format!("lock setup-orphan descriptor {}: {e}", path.display()) }
    })?;
    std::fs::remove_file(path).map_err(|e| format!("remove setup-orphan descriptor {}: {e}", path.display()))?;
    Ok(())
}

pub fn find_family_descriptor(family: &LeaseFamily) -> Result<Option<PathBuf>, String> {
    let root = coordination_root();
    let dir = root.join(family.namespace());
    if !dir.exists() { return Ok(None); }
    let prefix = format!("{}--", family.lifecycle_id());
    let mut found = None;
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("read lease family directory {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read lease family entry {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".lease") {
            if found.is_some() {
                return Err(format!("multiple persistent lease descriptors for {:?}", family));
            }
            found = Some(entry.path());
        }
    }
    Ok(found)
}

pub fn descriptor_availability(path: &Path) -> Result<(LeaseFamily, ClaimAvailability), String> {
    let mut file = open_existing_descriptor(path)
        .map_err(|e| format!("open persistent lease {}: {e}", path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let descriptor = read_descriptor_from(&mut file, path)?;
            Ok((descriptor.family.clone(), classify_availability(&descriptor.family, false)))
        }
        Err(error) if is_lock_contended(&error) => {
            let descriptor = read_descriptor_from(&mut file, path)?;
            Ok((descriptor.family.clone(), ClaimAvailability::Live))
        }
        Err(error) => Err(format!("probe persistent lease {}: {error}", path.display())),
    }
}

/// Classify a descriptor after the owning subsystem has durably established
/// a same-process recovery handoff. A foreign owner remains `Live`; only the
/// exact locally-created locked OFD can be treated by its post-owner lifecycle
/// semantics. Ordinary admission must continue to use `descriptor_availability`.
pub fn descriptor_recovery_availability_with_local_handoff(
    path: &Path,
) -> Result<(LeaseFamily, ClaimAvailability), String> {
    let mut file = open_existing_descriptor(path)
        .map_err(|e| format!("open persistent lease {}: {e}", path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let descriptor = read_descriptor_from(&mut file, path)?;
            Ok((
                descriptor.family.clone(),
                classify_availability(&descriptor.family, false),
            ))
        }
        Err(error) if is_lock_contended(&error) => {
            let descriptor = read_descriptor_from(&mut file, path)?;
            let availability = if current_process_coheld_descriptor(path, &descriptor)?.is_some() {
                classify_availability(&descriptor.family, false)
            } else {
                ClaimAvailability::Live
            };
            Ok((descriptor.family.clone(), availability))
        }
        Err(error) => Err(format!("probe persistent lease {}: {error}", path.display())),
    }
}

pub fn retire_descriptor_after_lifecycle_release(path: &Path, expected_family: &LeaseFamily) -> Result<(), String> {
    let root = coordination_root();
    create_private_dir(&root)?;
    let _registry = RegistryLock::acquire(&root)?;
    let lease = PersistentLease::acquire_existing(path, expected_family)?;
    let metadata_before = std::fs::symlink_metadata(path)
        .map_err(|e| format!("lstat persistent lease before retirement {}: {e}", path.display()))?;
    if !metadata_before.file_type().is_file() {
        return Err(format!("persistent lease pathname is not a regular file before retirement: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let fd_metadata = lease.file.metadata()
            .map_err(|e| format!("fstat persistent lease before retirement {}: {e}", path.display()))?;
        if metadata_before.dev() != fd_metadata.dev() || metadata_before.ino() != fd_metadata.ino() {
            return Err(format!("persistent lease pathname rebound before retirement: {}", path.display()));
        }
    }
    std::fs::remove_file(path)
        .map_err(|e| format!("retire persistent lease {}: {e}", path.display()))?;
    drop(lease);
    Ok(())
}

fn first_conflict<'a>(requested: &'a [PathClaim], existing: &'a [PathClaim]) -> Option<(&'a PathClaim, &'a PathClaim)> {
    requested.iter().find_map(|left| {
        existing.iter().find_map(|right| left.conflicts_with(right).then_some((left, right)))
    })
}

fn classify_descriptor(path: &Path) -> Result<Option<(ClaimAvailability, LeaseFamily, Vec<PathClaim>, Option<String>)>, String> {
    let file = match open_existing_descriptor(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open coordination descriptor {}: {error}", path.display())),
    };
    let lock_state = match file.try_lock_exclusive() {
        Ok(()) => false,
        Err(error) if is_lock_contended(&error) => true,
        Err(error) => return Err(format!("probe coordination descriptor {}: {error}", path.display())),
    };
    classify_opened_descriptor(file, path, lock_state)
}

/// Finish classification after the scanner has opened and lock-probed a
/// descriptor. Ordinary ephemeral guard teardown may intentionally unpublish
/// that descriptor without taking the registry lock. If the pathname vanishes
/// during this window, the already-open inode no longer represents published
/// authority and must not participate in admission.
fn classify_opened_descriptor(
    mut file: File,
    path: &Path,
    lock_state: bool,
) -> Result<Option<(ClaimAvailability, LeaseFamily, Vec<PathClaim>, Option<String>)>, String> {
    if !lock_state && reclaim_empty_descriptor_from_locked_file(&file, path)? {
        return Ok(None);
    }
    let descriptor = match read_descriptor_from(&mut file, path) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            if ephemeral_descriptor_unpublished_during_classification(path)? {
                return Ok(None);
            }
            if !lock_state
                && reclaim_invalid_ephemeral_descriptor_from_locked_file(&file, path)?
            {
                return Ok(None);
            }
            return Err(if lock_state {
                format!("malformed contended coordination descriptor (fail closed): {error}")
            } else {
                format!("malformed coordination descriptor requires lifecycle repair: {error}")
            });
        }
    };
    let availability = classify_availability(&descriptor.family, lock_state);
    Ok(Some((availability, descriptor.family, descriptor.claims, descriptor.coordination_group)))
}

/// A missing pathname is special only for structurally valid ephemeral
/// descriptors. Round-7 lexical retirement can remove such a pathname after a
/// scanner has opened the inode but before it verifies the binding. Durable
/// families, published rebinds, malformed live descriptors, and all other I/O
/// failures keep their existing fail-closed behavior.
fn ephemeral_descriptor_unpublished_during_classification(path: &Path) -> Result<bool, String> {
    if !structurally_ephemeral_descriptor_path(path) {
        return Ok(false);
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "recheck ephemeral coordination descriptor publication {}: {error}",
            path.display()
        )),
    }
}

fn same_execution_lifecycle(left: &LeaseFamily, right: &LeaseFamily) -> bool {
    let execution_family = |family: &LeaseFamily| matches!(family,
        LeaseFamily::QueueExecution { .. } | LeaseFamily::ExecutionClaim { .. } | LeaseFamily::ExecutionStaging { .. }
    );
    execution_family(left) && execution_family(right) && left.lifecycle_id() == right.lifecycle_id()
}

fn remove_reclaimable_ephemeral_locked(path: &Path) -> Result<(), String> {
    let mut file = match open_existing_descriptor(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("open ephemeral descriptor {}: {error}", path.display())),
    };
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if is_lock_contended(&error) => return Ok(()),
        Err(error) => return Err(format!("lock ephemeral descriptor {}: {error}", path.display())),
    }
    let descriptor = read_descriptor_from(&mut file, path)?;
    if !matches!(descriptor.family, LeaseFamily::EphemeralMutation { .. }) {
        return Ok(());
    }
    std::fs::remove_file(path).map_err(|e| format!("remove ephemeral descriptor {}: {e}", path.display()))?;
    Ok(())
}

fn descriptor_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    if !root.exists() { return Ok(paths); }
    for entry in std::fs::read_dir(root).map_err(|e| format!("read coordination root {}: {e}", root.display()))? {
        let entry = entry.map_err(|e| format!("read coordination root entry: {e}"))?;
        let ty = entry.file_type().map_err(|e| format!("inspect coordination entry: {e}"))?;
        if !ty.is_dir() { continue; }
        let family_dir = entry.path();
        let mut removed_staging = false;
        for child in std::fs::read_dir(&family_dir).map_err(|e| format!("read coordination family {}: {e}", family_dir.display()))? {
            let child = child.map_err(|e| format!("read coordination descriptor entry: {e}"))?;
            let child_path = child.path();
            if child_path.extension().and_then(|v| v.to_str()) == Some("lease") {
                paths.push(child_path);
            } else {
                removed_staging |= remove_abandoned_descriptor_temp_locked(&child_path)?;
            }
        }
        if removed_staging {
            sync_coordination_directory(&family_dir)?;
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_descriptor_from(file: &mut File, path: &Path) -> Result<LeaseDescriptor, String> {
    let descriptor_metadata = verify_coordination_path_binding(file, path, "persistent lease")?;
    if descriptor_metadata.len() > DESCRIPTOR_MAX_BYTES {
        return Err(format!("persistent lease descriptor exceeds {} bytes: {}", DESCRIPTOR_MAX_BYTES, path.display()));
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("seek persistent lease {}: {e}", path.display()))?;
    let mut bytes = Vec::with_capacity(descriptor_metadata.len() as usize);
    file.read_to_end(&mut bytes).map_err(|e| format!("read persistent lease {}: {e}", path.display()))?;
    let descriptor: LeaseDescriptor = serde_json::from_slice(&bytes)
        .map_err(|e| format!("decode persistent lease {}: {e}", path.display()))?;
    if descriptor.schema != DESCRIPTOR_SCHEMA {
        return Err(format!("unsupported persistent lease schema {} in {}", descriptor.schema, path.display()));
    }
    if !path_matches_family(path, &descriptor.family, descriptor.descriptor_id) {
        return Err(format!("persistent lease pathname/body identity mismatch: {}", path.display()));
    }
    Ok(descriptor)
}

fn path_matches_family(path: &Path, family: &LeaseFamily, descriptor_id: Uuid) -> bool {
    path.parent().and_then(Path::file_name).and_then(|v| v.to_str()) == Some(family.namespace())
        && path.file_name().and_then(|v| v.to_str())
            == Some(format!("{}--{}.lease", family.lifecycle_id(), descriptor_id).as_str())
}

struct RegistryLock(File);
impl RegistryLock {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join("registry.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(&path)
            .map_err(|e| format!("open coordination registry lock {}: {e}", path.display()))?;
        set_private_file_permissions(&file)?;
        let descriptor_metadata = file
            .metadata()
            .map_err(|e| format!("stat coordination registry descriptor {}: {e}", path.display()))?;
        if !descriptor_metadata.file_type().is_file() {
            return Err(format!("coordination registry descriptor is not a regular file: {}", path.display()));
        }
        let pathname_metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("stat coordination registry pathname {}: {e}", path.display()))?;
        if !pathname_metadata.file_type().is_file() {
            return Err(format!("coordination registry pathname is not a regular file: {}", path.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if descriptor_metadata.dev() != pathname_metadata.dev()
                || descriptor_metadata.ino() != pathname_metadata.ino()
            {
                return Err(format!("coordination registry pathname rebound after open: {}", path.display()));
            }
        }
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self(file)),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if is_lock_contended(&error) => {
                    if started.elapsed() >= REGISTRY_WAIT {
                        return Err("coordination registry busy; retry the operation".to_string());
                    }
                    std::thread::sleep(REGISTRY_RETRY);
                }
                Err(error) => return Err(format!("lock coordination registry {}: {error}", path.display())),
            }
        }
    }
}
impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// After the v24 epoch is active, a legacy top-level journal is an observable
/// signal that an old build may still mutate without participating in claims.
/// Check this before taking the registry lock; journal directory I/O never runs
/// in the shared critical section. Only nonterminal or unclassifiable top-level
/// legacy journals block; terminal-clean records are inert historical evidence.
fn reject_detectable_legacy_mutation_ambiguity() -> Result<(), String> {
    reject_detectable_legacy_mutation_ambiguity_except(None)
}

fn reject_detectable_legacy_mutation_ambiguity_except(legacy_exception: Option<&Path>) -> Result<(), String> {
    if legacy_exception.is_some() {
        // Explicit adoption publishes only durable v2 recovery authority; it
        // does not execute the legacy operation. Remaining legacy records stay
        // visible to the activation gate and ordinary mutation admission.
        return Ok(());
    }
    let blockers = crate::tui::file_task_runtime::nonterminal_legacy_journals();
    if let Some(entry) = blockers.first() {
        let detail = entry.classification_error.as_deref().unwrap_or("nonterminal legacy mutation obligation");
        return Err(format!(
            "unsupported mixed-version mutation state: legacy top-level file-operation journal {} remains unresolved ({detail}); explicitly adopt/reconcile it after closing v0.4.8 sessions before mutating",
            entry.journal_path.display()
        ));
    }

    // Do not infer protocol age from executable path/inode identity. A separately
    // installed or replaced protocol-aware binary is not evidence of a legacy
    // session. v24 activation performs the supported close-old-sessions check;
    // after activation, semantic legacy queue/journal signals remain authoritative.
    Ok(())
}

const TEST_CONCURRENCY_INHERIT_ENV: &str = "TONEPOET_TEST_CONCURRENCY_DIR_INHERIT";

#[cfg(test)]
struct TestCoordinationRootOverrideState {
    owner: std::thread::ThreadId,
    path: PathBuf,
}

#[cfg(test)]
fn test_coordination_serial() -> &'static Mutex<()> {
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    SERIAL.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
fn test_coordination_override_state() -> &'static Mutex<Option<TestCoordinationRootOverrideState>> {
    static STATE: OnceLock<Mutex<Option<TestCoordinationRootOverrideState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn scoped_test_coordination_state() -> &'static Mutex<Option<PathBuf>> {
    static STATE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Thread-owned root override for narrowly local descriptor fixtures. This is
/// intentionally not inherited by worker threads. Tests whose coordination
/// activity can cross a thread/task boundary must use
/// `scoped_test_coordination_root` (or its explicit-path variant) instead.
#[cfg(test)]
pub(crate) struct TestCoordinationRootGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    owner: std::thread::ThreadId,
}

#[cfg(test)]
pub(crate) fn install_test_coordination_root(path: &Path) -> TestCoordinationRootGuard {
    let serial = test_coordination_serial()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let owner = std::thread::current().id();
    let mut state = test_coordination_override_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *state = Some(TestCoordinationRootOverrideState {
        owner: owner.clone(),
        path: path.to_path_buf(),
    });
    drop(state);
    TestCoordinationRootGuard {
        _serial: serial,
        owner,
    }
}

#[cfg(test)]
impl Drop for TestCoordinationRootGuard {
    fn drop(&mut self) {
        let mut state = test_coordination_override_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .as_ref()
            .is_some_and(|current| current.owner == self.owner)
        {
            *state = None;
        }
    }
}

#[cfg(test)]
fn current_test_coordination_root_override() -> Option<PathBuf> {
    let owner = std::thread::current().id();
    test_coordination_override_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|current| current.owner == owner)
        .map(|current| current.path.clone())
}

/// Process-visible, serialized coordination root for one coordination-touching
/// unit test. The guard owns the environment override for the whole test so
/// worker threads/tasks and production-like helper subprocesses inherit the
/// same registry. Every coordination-touching unit test must hold this serial
/// scope; unrelated tests need not.
#[cfg(test)]
pub(crate) struct ScopedTestCoordinationRootGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: PathBuf,
    previous_root_env: Option<std::ffi::OsString>,
    previous_inherit_env: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl ScopedTestCoordinationRootGuard {
    fn install(path: PathBuf) -> Self {
        let serial = test_coordination_serial()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_root_env = std::env::var_os("TONEPOET_CONCURRENCY_DIR");
        let previous_inherit_env = std::env::var_os(TEST_CONCURRENCY_INHERIT_ENV);

        {
            let mut state = scoped_test_coordination_state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(state.is_none(), "test coordination scope must be singular");
            *state = Some(path.clone());
        }
        std::env::set_var("TONEPOET_CONCURRENCY_DIR", &path);
        std::env::remove_var(TEST_CONCURRENCY_INHERIT_ENV);

        Self {
            _serial: serial,
            root: path,
            previous_root_env,
            previous_inherit_env,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
pub(crate) fn scoped_test_coordination_root() -> ScopedTestCoordinationRootGuard {
    // The override is process-visible so production-like worker threads can
    // inherit it. A parallel libtest worker can therefore capture this path
    // before it has entered the serialized fixture protocol. Deleting the
    // directory when this guard drops races that in-flight borrower and can
    // turn an otherwise valid lease staging create into ENOENT.
    //
    // Use a run-private, UUID-namespaced root beneath the cargo-test fallback
    // and never delete it from inside the test process. The path is never
    // reused, so retirement cannot expose stale authority to a later scoped
    // test, and no production coordination semantics are changed.
    let process_root = cargo_test_coordination_root()
        .expect("unit tests must have a process-private coordination root");
    let root = process_root
        .join("scoped")
        .join(Uuid::new_v4().to_string());
    create_private_dir(&root).expect("create isolated test coordination root");
    ScopedTestCoordinationRootGuard::install(root)
}

#[cfg(test)]
pub(crate) fn install_scoped_test_coordination_root(
    path: &Path,
) -> ScopedTestCoordinationRootGuard {
    ScopedTestCoordinationRootGuard::install(path.to_path_buf())
}

#[cfg(test)]
impl Drop for ScopedTestCoordinationRootGuard {
    fn drop(&mut self) {
        let mut state = scoped_test_coordination_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.as_deref() == Some(self.root.as_path()) {
            *state = None;
        }
        drop(state);

        match self.previous_root_env.take() {
            Some(previous) => std::env::set_var("TONEPOET_CONCURRENCY_DIR", previous),
            None => std::env::remove_var("TONEPOET_CONCURRENCY_DIR"),
        }
        match self.previous_inherit_env.take() {
            Some(previous) => std::env::set_var(TEST_CONCURRENCY_INHERIT_ENV, previous),
            None => std::env::remove_var(TEST_CONCURRENCY_INHERIT_ENV),
        }
    }
}

#[cfg(test)]
fn current_scoped_test_coordination_root() -> Option<PathBuf> {
    scoped_test_coordination_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Cargo's test executables live under `target/{profile}/deps` and carry a
/// metadata hash suffix.  Detect that shape at runtime as well as under
/// `cfg(test)` so integration-test dependencies never fall back to the user's
/// real coordination registry.
pub(crate) fn running_under_cargo_test_harness() -> bool {
    if cfg!(test) {
        return true;
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Some(deps) = executable.parent() else {
        return false;
    };
    if deps.file_name().and_then(|value| value.to_str()) != Some("deps") {
        return false;
    }
    let Some(profile_dir) = deps.parent() else {
        return false;
    };
    // Integration-test dependencies are compiled without cfg(test). Require
    // Cargo's adjacent fingerprint directory as well as the hashed libtest
    // executable shape so an installed production binary named `*-deadbeef`
    // under an unrelated `deps/` directory cannot disable activation checks.
    if !profile_dir.join(".fingerprint").is_dir() {
        return false;
    }
    let Some(stem) = executable.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some((_, suffix)) = stem.rsplit_once('-') else {
        return false;
    };
    suffix.len() >= 8 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cargo_test_coordination_root() -> Option<PathBuf> {
    // Safety fallback only: coordination-touching unit tests are required to
    // hold `scoped_test_coordination_root` (or an approved explicit fixture).
    // Keeping a process-private fallback prevents an accidentally unscoped
    // future test from ever consulting the user's real ~/.config registry;
    // it is not the isolation boundary for reviewed coordination tests.
    if !running_under_cargo_test_harness() {
        return None;
    }
    static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();
    Some(
        TEST_ROOT
            .get_or_init(|| {
                std::env::temp_dir()
                    .join("tonepoet-test-concurrency-v1")
                    .join(format!(
                        "{}-{:016x}",
                        std::process::id(),
                        process_instance_token()
                    ))
            })
            .clone(),
    )
}

pub fn coordination_root() -> PathBuf {
    if running_under_cargo_test_harness() {
        #[cfg(test)]
        if let Some(path) = current_scoped_test_coordination_root() {
            return path;
        }
        #[cfg(test)]
        if let Some(path) = current_test_coordination_root_override() {
            return path;
        }
        if std::env::var_os(TEST_CONCURRENCY_INHERIT_ENV).as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            if let Some(path) = std::env::var_os("TONEPOET_CONCURRENCY_DIR") {
                return PathBuf::from(path);
            }
        }
        if let Some(path) = cargo_test_coordination_root() {
            return path;
        }
    }
    if let Some(path) = std::env::var_os("TONEPOET_CONCURRENCY_DIR") {
        return PathBuf::from(path);
    }
    crate::config::TonepoetConfig::config_path()
        .parent()
        .map(|p| p.join("concurrency-v1"))
        .unwrap_or_else(|| PathBuf::from(".tonepoet-concurrency-v1"))
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("create coordination directory {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("set coordination directory permissions {}: {e}", path.display()))?;
    }
    Ok(())
}

fn set_private_file_permissions(file: &File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("set coordination file permissions: {e}"))?;
    }
    Ok(())
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == fs2::lock_contended_error().kind()
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn process_instance_token() -> u64 {
    static TOKEN: OnceLock<u64> = OnceLock::new();
    *TOKEN.get_or_init(|| {
        let mut bytes = [0u8; 8];
        if getrandom::getrandom(&mut bytes).is_ok() {
            let value = u64::from_le_bytes(bytes);
            if value != 0 { return value; }
        }
        let raw = format!("{}:{}:{:p}", std::process::id(), unix_ms(), &TOKEN);
        checksum64(raw.as_bytes()).max(1)
    })
}

fn checksum64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn process_start_ticks(pid: u32) -> Option<u64> {
    crate::convert::script_supervisor::local_process_start_identity(pid)
        .ok()
        .flatten()
        .map(|identity| checksum64(identity.as_bytes()).max(1))
}

fn boot_id_hash() -> Option<u64> {
    let host = crate::convert::script_supervisor::current_host_boot_identity();
    if host.boot_identity.is_empty() || host.boot_identity == "boot-id-unavailable" {
        return None;
    }
    let identity = format!(
        "{}\0{}\0{}",
        host.machine_identity, host.host_identity, host.boot_identity
    );
    Some(checksum64(identity.as_bytes()).max(1))
}

pub fn normalized_claim_roots(claims: &[PathClaim]) -> BTreeSet<PathBuf> {
    claims.iter().map(|claim| claim.identity.resolved_io_path.clone()).collect()
}


#[derive(Debug, Clone)]
struct RuntimeExecutionAuthority {
    execution_id: Uuid,
    queue_lease: Arc<PersistentLease>,
    supplemental_leases: Vec<Arc<PersistentLease>>,
    database_path: Option<PathBuf>,
    item_supervisor: Option<crate::convert::script_supervisor::ItemExecutionSupervisorClient>,
}

fn runtime_execution_authorities() -> &'static Mutex<HashMap<String, RuntimeExecutionAuthority>> {
    static AUTHORITIES: OnceLock<Mutex<HashMap<String, RuntimeExecutionAuthority>>> = OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Publish the in-process queue execution authority after the QueueExecution
/// descriptor is durably committed. This is process-local routing metadata only;
/// the descriptor/DB row remain the crash-recovery authorities.
pub fn register_runtime_execution(
    item_id: &str,
    execution_id: Uuid,
    queue_lease: Arc<PersistentLease>,
    database_path: Option<PathBuf>,
) -> Result<(), String> {
    {
        let map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = map.get(item_id) {
            if existing.execution_id == execution_id {
                return Ok(());
            }
            return Err(format!("item {item_id} already has a different runtime execution authority"));
        }
    }
    // Unit and integration tests use the same item-supervisor process boundary
    // as production. `resolve_supervisor_helper_executable` maps a Cargo test
    // harness to the built tonepoet binary without requiring an environment
    // override, so tests do not silently skip lifecycle supervision.
    let queue_file = queue_lease.duplicate_lifetime_file()?;
    let item_supervisor = Some(
        crate::convert::script_supervisor::ItemExecutionSupervisorClient::start(&[queue_file])
            .map_err(|error| format!("start item execution supervisor for {item_id}: {error}"))?,
    );
    let mut map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = map.get(item_id) {
        if let Some(supervisor) = item_supervisor.as_ref() { let _ = supervisor.shutdown(); }
        if existing.execution_id == execution_id { return Ok(()); }
        return Err(format!("item {item_id} acquired a different runtime execution authority concurrently"));
    }
    map.insert(
        item_id.to_string(),
        RuntimeExecutionAuthority {
            execution_id,
            queue_lease,
            supplemental_leases: Vec::new(),
            database_path,
            item_supervisor,
        },
    );
    Ok(())
}

pub fn unregister_runtime_execution(item_id: &str) {
    let authority = {
        let mut map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.remove(item_id)
    };
    if let Some(mut authority) = authority {
        // Terminality is established before unregister. Ask the one per-item
        // supervisor to stop only after all contained backend workers have
        // positively terminated; until then it remains a co-holder of every
        // execution/path/staging lease handed to it.
        if let Some(supervisor) = authority.item_supervisor.as_ref() {
            if let Err(error) = supervisor.shutdown() {
                log::warn!("item execution supervisor shutdown for {item_id} was incomplete: {error}");
            }
        }
        let supplemental = std::mem::take(&mut authority.supplemental_leases);
        let retire = supplemental.iter()
            .map(|lease| (lease.descriptor_path().to_path_buf(), lease.family().clone()))
            .collect::<Vec<_>>();
        drop(supplemental);
        // Lifecycle terminality is established by the caller before unregister.
        // Best-effort setup-orphan cleanup remains safe because durable claims
        // are never removed by generic GC.
        for (path, family) in retire {
            let _ = retire_descriptor_after_lifecycle_release(&path, &family);
        }
    }
}

pub fn runtime_execution_id(item_id: &str) -> Option<Uuid> {
    let map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(item_id).map(|authority| authority.execution_id)
}

/// Snapshot the path capabilities currently retained by one live conversion
/// execution. Action-phase admission uses this to avoid publishing duplicate
/// claims for namespaces already covered by the conversion's album claim.
pub fn runtime_execution_claims(item_id: &str) -> Result<Vec<PathClaim>, String> {
    let map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let authority = map
        .get(item_id)
        .ok_or_else(|| format!("no active QueueExecution authority for item {item_id}"))?;
    let mut claims = authority.queue_lease.claims().to_vec();
    for lease in &authority.supplemental_leases {
        claims.extend_from_slice(lease.claims());
    }
    Ok(claims)
}

/// Add a durable execution-scoped claim/staging holder. It remains owned by the
/// pipeline and is additionally discoverable for supervisor handoff.
pub fn register_runtime_supplemental_lease(
    item_id: &str,
    lease: Arc<PersistentLease>,
) -> Result<(), String> {
    let (execution_id, supervisor) = {
        let map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let authority = map
            .get(item_id)
            .ok_or_else(|| format!("no active QueueExecution authority for item {item_id}"))?;
        (authority.execution_id, authority.item_supervisor.clone())
    };
    if let Some(supervisor) = supervisor.as_ref() {
        let handoff = lease.duplicate_lifetime_file()?;
        supervisor
            .handoff_lifetime_file(handoff.as_ref())
            .map_err(|error| format!("handoff supplemental lease for {item_id}: {error}"))?;
    }
    // Recheck after the IPC acknowledgement so a terminal transition cannot
    // attach a late claim to a replacement execution incarnation.
    let mut map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let authority = map
        .get_mut(item_id)
        .ok_or_else(|| format!("QueueExecution authority for item {item_id} ended during lease handoff"))?;
    if authority.execution_id != execution_id {
        return Err(format!("QueueExecution authority for item {item_id} changed during lease handoff"));
    }
    authority.supplemental_leases.push(lease);
    Ok(())
}

pub fn runtime_item_supervisor(
    item_id: &str,
) -> Result<crate::convert::script_supervisor::ItemExecutionSupervisorClient, String> {
    let map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(item_id)
        .and_then(|authority| authority.item_supervisor.clone())
        .ok_or_else(|| format!("no active item execution supervisor for {item_id}"))
}

/// Snapshot close-only duplicates for transfer to a trusted tonepoet process.
/// Third-party children never receive these descriptors.
pub fn runtime_supervision_lifetime_files(item_id: &str) -> Result<Vec<Arc<File>>, String> {
    let map = runtime_execution_authorities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let authority = map
        .get(item_id)
        .ok_or_else(|| format!("no active QueueExecution authority for item {item_id}"))?;
    let mut files = Vec::with_capacity(1 + authority.supplemental_leases.len());
    files.push(authority.queue_lease.duplicate_lifetime_file()?);
    for lease in &authority.supplemental_leases {
        files.push(lease.duplicate_lifetime_file()?);
    }
    Ok(files)
}

tokio::task_local! {
    static CURRENT_EXECUTION_ITEM: String;
    static ADDITIONAL_SUPERVISION_LIFETIME_FILES: Vec<Arc<File>>;
}
thread_local! {
    // Blocking/manual mutation paths are not inside a Tokio task scope. This
    // narrow stack lets their trusted tonepoet supervisor receive the outer
    // admission lease without teaching third-party programs about it.
    static THREAD_SUPERVISION_LIFETIME_FILES: std::cell::RefCell<Vec<Arc<File>>> = std::cell::RefCell::new(Vec::new());
    // Metadata batches retain one outer MutationClaimGuard while existing
    // backend writers continue to own their local locks/journals. This stack
    // lets nested writers prove that the outer capability already covers an
    // exact path instead of reacquiring a conflicting EphemeralMutation.
    static THREAD_MUTATION_CAPABILITIES: std::cell::RefCell<Vec<Arc<[PathClaim]>>> = std::cell::RefCell::new(Vec::new());
}

pub fn with_thread_supervision_lifetime_files<T>(
    files: Vec<Arc<File>>,
    operation: impl FnOnce() -> T,
) -> T {
    THREAD_SUPERVISION_LIFETIME_FILES.with(|slot| {
        let previous = slot.replace(files);
        struct Restore<'a> {
            slot: &'a std::cell::RefCell<Vec<Arc<File>>>,
            previous: Option<Vec<Arc<File>>>,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Some(previous) = self.previous.take() {
                    self.slot.replace(previous);
                }
            }
        }
        let _restore = Restore { slot, previous: Some(previous) };
        operation()
    })
}

/// Execute nested mutation code under an already-held set of live path
/// capabilities. The caller must keep the corresponding MutationClaimGuard or
/// durable execution authority alive for the entire closure. This is scoped to
/// the current blocking thread and never publishes new registry state.
pub fn with_scoped_mutation_claims<T>(claims: &[PathClaim], operation: impl FnOnce() -> T) -> T {
    THREAD_MUTATION_CAPABILITIES.with(|slot| {
        slot.borrow_mut().push(claims.to_vec().into());
        struct Pop<'a> {
            slot: &'a std::cell::RefCell<Vec<Arc<[PathClaim]>>>,
        }
        impl Drop for Pop<'_> {
            fn drop(&mut self) {
                let _ = self.slot.borrow_mut().pop();
            }
        }
        let _pop = Pop { slot };
        operation()
    })
}

/// Snapshot scoped mutation capabilities so a caller that deliberately
/// creates worker threads can transfer the already-held authority without
/// publishing another registry descriptor. The originating guard remains
/// owned by the outer operation.
pub fn current_scoped_mutation_claims() -> Vec<PathClaim> {
    THREAD_MUTATION_CAPABILITIES.with(|slot| {
        slot.borrow()
            .iter()
            .flat_map(|claims| claims.iter().cloned())
            .collect()
    })
}

/// Return whether a live capability already available to this execution
/// covers `claim`. Scoped outer mutation batches are checked first, followed by
/// the current conversion ExecutionClaim when this code runs in its task scope.
pub fn current_mutation_authority_covers(claim: &PathClaim) -> Result<bool, String> {
    let scoped = THREAD_MUTATION_CAPABILITIES.with(|slot| {
        slot.borrow()
            .iter()
            .rev()
            .any(|claims| claims.iter().any(|held| held.covers(claim)))
    });
    if scoped {
        return Ok(true);
    }
    let Some(item_id) = current_execution_item() else {
        return Ok(false);
    };
    Ok(runtime_execution_claims(&item_id)?
        .iter()
        .any(|held| held.covers(claim)))
}

pub async fn with_runtime_execution_scope<F>(item_id: String, future: F) -> F::Output
where
    F: std::future::Future,
{
    CURRENT_EXECUTION_ITEM.scope(item_id, future).await
}

pub async fn with_additional_supervision_lifetime_files<F>(files: Vec<Arc<File>>, future: F) -> F::Output
where
    F: std::future::Future,
{
    ADDITIONAL_SUPERVISION_LIFETIME_FILES.scope(files, future).await
}

pub fn current_supervision_lifetime_files() -> Result<Vec<Arc<File>>, String> {
    let mut files = CURRENT_EXECUTION_ITEM
        .try_with(|item_id| runtime_supervision_lifetime_files(item_id))
        .unwrap_or_else(|_| Ok(Vec::new()))?;
    if let Ok(additional) = ADDITIONAL_SUPERVISION_LIFETIME_FILES.try_with(Clone::clone) {
        files.extend(additional);
    }
    THREAD_SUPERVISION_LIFETIME_FILES.with(|additional| {
        files.extend(additional.borrow().iter().cloned());
    });
    Ok(files)
}


pub fn current_execution_item() -> Option<String> {
    CURRENT_EXECUTION_ITEM.try_with(Clone::clone).ok()
}

/// Durably record containment before the supervisor exec gate is acknowledged.
/// This opens a short independent SQLite connection so the callback can run in
/// the blocking supervisor thread without sharing a rusqlite Connection.
pub fn record_execution_containment(
    item_id: &str,
    token: &str,
    runtime_directory: &Path,
    descriptor: &crate::convert::script_supervisor::ContainmentDescriptor,
) -> Result<(), String> {
    let (execution_id, database_path) = {
        let map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let authority = map
            .get(item_id)
            .ok_or_else(|| format!("no active execution authority for item {item_id}"))?;
        (authority.execution_id, authority.database_path.clone())
    };
    let Some(database_path) = database_path else { return Ok(()) };
    let conn = rusqlite::Connection::open(&database_path)
        .map_err(|e| format!("open execution containment database {}: {e}", database_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("configure execution containment database: {e}"))?;
    let tx = conn.unchecked_transaction()
        .map_err(|e| format!("begin execution containment update: {e}"))?;
    let current: Option<String> = tx.query_row(
        "SELECT containment_json FROM conversion_queue_executions WHERE execution_id=?1 AND item_id=?2",
        rusqlite::params![execution_id.to_string(), item_id],
        |row| row.get(0),
    ).map_err(|e| format!("read execution containment row before release: {e}"))?;
    let mut entries = current
        .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
        .unwrap_or_default();
    entries.retain(|entry| entry.get("token").and_then(|v| v.as_str()) != Some(token));
    entries.push(serde_json::json!({
        "token": token,
        "runtime_directory": runtime_directory,
        "descriptor": descriptor,
        "released": false,
    }));
    let changed = tx.execute(
        "UPDATE conversion_queue_executions SET containment_json=?1, updated_unix_ms=?2 WHERE execution_id=?3 AND item_id=?4",
        rusqlite::params![serde_json::to_string(&entries).map_err(|e| format!("serialize containment set: {e}"))?, unix_ms() as i64, execution_id.to_string(), item_id],
    ).map_err(|e| format!("persist execution containment before release: {e}"))?;
    if changed != 1 {
        return Err(format!("execution containment row disappeared before release for item {item_id}"));
    }
    tx.commit().map_err(|e| format!("commit execution containment before release: {e}"))?;
    Ok(())
}

/// Mark a previously persisted containment as having crossed its exec gate.
/// `ContainmentPrepared` deliberately leaves `external_released=false`; only
/// the supervisor's authenticated `UserCodeReleased` event may set this bit.
pub fn mark_execution_containment_released(item_id: &str, token: &str) -> Result<(), String> {
    let (execution_id, database_path) = {
        let map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let authority = map
            .get(item_id)
            .ok_or_else(|| format!("no active execution authority for item {item_id}"))?;
        (authority.execution_id, authority.database_path.clone())
    };
    let Some(database_path) = database_path else { return Ok(()) };
    let mut conn = rusqlite::Connection::open(&database_path)
        .map_err(|e| format!("open execution containment database {}: {e}", database_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("configure execution containment database: {e}"))?;
    let tx = conn.unchecked_transaction()
        .map_err(|e| format!("begin execution release update: {e}"))?;
    let current: Option<String> = tx.query_row(
        "SELECT containment_json FROM conversion_queue_executions WHERE execution_id=?1 AND item_id=?2",
        rusqlite::params![execution_id.to_string(), item_id],
        |row| row.get(0),
    ).map_err(|e| format!("read execution containment before release mark: {e}"))?;
    let mut entries = current
        .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
        .unwrap_or_default();
    let mut found = false;
    for entry in &mut entries {
        if entry.get("token").and_then(|value| value.as_str()) == Some(token) {
            let Some(object) = entry.as_object_mut() else {
                return Err("execution containment entry is not an object".to_string());
            };
            object.insert("released".to_string(), serde_json::Value::Bool(true));
            found = true;
        }
    }
    if !found {
        return Err(format!("execution containment token {token} was not persisted before release"));
    }
    let changed = tx.execute(
        "UPDATE conversion_queue_executions SET external_released=1, containment_json=?1, updated_unix_ms=?2 WHERE execution_id=?3 AND item_id=?4",
        rusqlite::params![serde_json::to_string(&entries).map_err(|e| format!("serialize released containment set: {e}"))?, unix_ms() as i64, execution_id.to_string(), item_id],
    ).map_err(|e| format!("persist execution released state: {e}"))?;
    if changed != 1 {
        return Err(format!("execution containment row disappeared while marking release for item {item_id}"));
    }
    tx.commit().map_err(|e| format!("commit execution release update: {e}"))?;
    Ok(())
}

/// Remove one completed containment only after the supervisor has durably
/// reported emptiness. If it was the last released containment, the execution
/// returns to the not-currently-external state while the QueueExecution lease
/// and path claims remain live.
pub fn clear_execution_containment(item_id: &str, token: &str) -> Result<(), String> {
    let (execution_id, database_path) = {
        let map = runtime_execution_authorities()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let authority = map
            .get(item_id)
            .ok_or_else(|| format!("no active execution authority for item {item_id}"))?;
        (authority.execution_id, authority.database_path.clone())
    };
    let Some(database_path) = database_path else { return Ok(()) };
    let mut conn = rusqlite::Connection::open(&database_path)
        .map_err(|e| format!("open execution containment database {}: {e}", database_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("configure execution containment database: {e}"))?;
    let tx = conn.unchecked_transaction()
        .map_err(|e| format!("begin execution containment cleanup: {e}"))?;
    let current: Option<String> = tx.query_row(
        "SELECT containment_json FROM conversion_queue_executions WHERE execution_id=?1 AND item_id=?2",
        rusqlite::params![execution_id.to_string(), item_id],
        |row| row.get(0),
    ).map_err(|e| format!("read execution containment cleanup row: {e}"))?;
    let mut entries = current
        .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
        .unwrap_or_default();
    entries.retain(|entry| entry.get("token").and_then(|v| v.as_str()) != Some(token));
    let any_released = entries.iter().any(|entry| entry.get("released").and_then(|value| value.as_bool()) == Some(true));
    let encoded = if entries.is_empty() { None } else { Some(serde_json::to_string(&entries).map_err(|e| format!("serialize containment cleanup set: {e}"))?) };
    tx.execute(
        "UPDATE conversion_queue_executions SET external_released=?1, containment_json=?2, updated_unix_ms=?3 WHERE execution_id=?4 AND item_id=?5",
        rusqlite::params![if any_released {1} else {0}, encoded, unix_ms() as i64, execution_id.to_string(), item_id],
    ).map_err(|e| format!("persist execution containment cleanup: {e}"))?;
    tx.commit().map_err(|e| format!("commit execution containment cleanup: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_root<T>(f: impl FnOnce(&Path) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let _root = install_test_coordination_root(dir.path());
        f(dir.path())
    }

    fn reacquire_after_intentional_ephemeral_authority_closes(
        claim: PathClaim,
        context: &str,
    ) -> MutationClaimGuard {
        // An explicitly exported or detached fd can itself be inherited by an
        // unrelated concurrent fork. Its public descriptor must stay visible
        // while any such kernel co-holder exists, so final-close reclamation is
        // intentionally eventual rather than lexical. Bound the wait so the
        // regression tolerates only that pre-exec window, never a real leak.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]) {
                Ok(guard) => return guard,
                Err(error) if error.contains("live owner") && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("{context}: {error}"),
            }
        }
    }

    #[test]
    fn scoped_test_coordination_root_is_visible_to_spawned_worker_thread() {
        let scope = scoped_test_coordination_root();
        let expected = scope.path().to_path_buf();
        assert_eq!(coordination_root(), expected);

        let (worker_root, worker_env_root) = std::thread::spawn(|| {
            (
                coordination_root(),
                std::env::var_os("TONEPOET_CONCURRENCY_DIR"),
            )
        })
        .join()
        .expect("coordination-root worker thread");
        assert_eq!(
            worker_root, expected,
            "a worker spawned by a scoped coordination test must share its registry"
        );
        assert_eq!(
            worker_env_root.as_deref(),
            Some(expected.as_os_str()),
            "the scoped environment root must be visible to spawned workers"
        );
    }

    #[test]
    fn scoped_test_coordination_root_retirement_keeps_captured_family_path_alive() {
        let scope = scoped_test_coordination_root();
        let expected_root = scope.path().to_path_buf();
        let (captured_tx, captured_rx) = std::sync::mpsc::channel();
        let (retired_tx, retired_rx) = std::sync::mpsc::channel();

        let worker = std::thread::spawn(move || {
            // This is the problematic libtest interleaving: an unrelated worker
            // resolves the process-visible scoped root while the owner is live,
            // but does not create its lease staging file until after retirement.
            let root = coordination_root();
            assert_eq!(root, expected_root);
            let family_dir = root.join(LeaseFamily::EphemeralMutation {
                claim_id: Uuid::new_v4(),
            }
            .namespace());
            create_private_dir(&family_dir).expect("create captured family directory");
            captured_tx
                .send(())
                .expect("report captured coordination root");
            retired_rx
                .recv()
                .expect("wait for scoped-root retirement");

            let staging = family_dir.join(format!(
                ".{}--{}.lease.tmp-{}",
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .mode(0o600);
            }
            let file = options
                .open(&staging)
                .expect("captured coordination family must survive scoped-root retirement");
            drop(file);
            std::fs::remove_file(&staging).expect("remove retirement regression staging file");
        });

        captured_rx
            .recv()
            .expect("worker must capture scoped root before retirement");
        drop(scope);
        retired_tx
            .send(())
            .expect("release worker after scoped-root retirement");
        worker.join().expect("captured-root worker");
    }

    #[test]
    fn scoped_test_coordination_roots_isolate_durable_state_between_tests() {
        let parent = tempfile::tempdir().expect("test root parent");
        let root_a = parent.path().join("test-a");
        let root_b = parent.path().join("test-b");
        let family = LeaseFamily::JournalOperation {
            job_id: Uuid::new_v4(),
        };

        let descriptor_a = {
            let scope_a = install_scoped_test_coordination_root(&root_a);
            assert_eq!(coordination_root(), scope_a.path().to_path_buf());
            let lease = PersistentLease::create(family.clone(), &[])
                .expect("create recovery-reserved descriptor in test root A");
            let descriptor = lease.descriptor_path().to_path_buf();
            drop(lease);
            descriptor
        };
        assert!(descriptor_a.exists(), "root A descriptor must survive owner drop");

        {
            let scope_b = install_scoped_test_coordination_root(&root_b);
            assert_eq!(coordination_root(), scope_b.path().to_path_buf());
            assert!(
                find_family_descriptor(&family)
                    .expect("scan isolated test root B")
                    .is_none(),
                "root B must not enumerate durable state from root A"
            );
            let guard = MutationClaimGuard::acquire_ephemeral(Vec::new())
                .expect("root B admission must ignore root A recovery authority");
            drop(guard);
        }

        assert!(
            descriptor_a.exists(),
            "activity in root B must never retire root A recovery authority"
        );
    }

    #[test]
    fn durable_family_free_is_recovery_reserved() {
        let id = Uuid::new_v4();
        assert_eq!(classify_availability(&LeaseFamily::JournalOperation { job_id: id }, false), ClaimAvailability::RecoveryReserved);
        assert_eq!(classify_availability(&LeaseFamily::QueueExecution { execution_id: id }, false), ClaimAvailability::RecoveryReserved);
        assert_eq!(classify_availability(&LeaseFamily::ExecutionClaim { execution_id: id }, false), ClaimAvailability::RecoveryReserved);
        assert_eq!(classify_availability(&LeaseFamily::ExecutionStaging { execution_id: id }, false), ClaimAvailability::RecoveryReserved);
        assert_eq!(classify_availability(&LeaseFamily::EphemeralMutation { claim_id: id }, false), ClaimAvailability::ReclaimableEphemeral);
    }

    #[test]
    fn read_read_does_not_conflict_but_write_subtree_does() {
        let dir = tempfile::tempdir().unwrap();
        let root = PathClaim::resolve(dir.path(), ClaimMode::Read, ClaimScope::Subtree).unwrap();
        let child = PathClaim::resolve(&dir.path().join("future"), ClaimMode::Read, ClaimScope::Subtree).unwrap();
        assert!(!root.conflicts_with(&child));
        let write = PathClaim::resolve(dir.path(), ClaimMode::Write, ClaimScope::Subtree).unwrap();
        assert!(write.conflicts_with(&child));
    }

    #[test]
    fn claim_coverage_is_directional_by_mode_scope_and_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Album");
        std::fs::create_dir_all(&album).unwrap();
        let track = album.join("01.flac");
        std::fs::write(&track, b"track").unwrap();

        let album_write = PathClaim::resolve(&album, ClaimMode::Write, ClaimScope::Subtree).unwrap();
        let track_write = PathClaim::resolve(&track, ClaimMode::Write, ClaimScope::Exact).unwrap();
        let track_read = PathClaim::resolve(&track, ClaimMode::Read, ClaimScope::Exact).unwrap();
        let album_read = PathClaim::resolve(&album, ClaimMode::Read, ClaimScope::Subtree).unwrap();

        assert!(album_write.covers(&track_write));
        assert!(album_write.covers(&track_read));
        assert!(!album_read.covers(&track_write));
        assert!(!track_write.covers(&album_write));
    }

    #[test]
    fn runtime_claim_snapshot_includes_registered_execution_supplementals() {
        with_root(|_| {
            let item_id = format!("phase-claim-{}", Uuid::new_v4());
            let execution_id = Uuid::new_v4();
            let queue_lease = std::sync::Arc::new(
                PersistentLease::create(
                    LeaseFamily::QueueExecution { execution_id },
                    &[],
                )
                .unwrap(),
            );
            register_runtime_execution(
                &item_id,
                execution_id,
                std::sync::Arc::clone(&queue_lease),
                None,
            )
            .unwrap();

            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join("external");
            let claim =
                PathClaim::resolve(&destination, ClaimMode::Write, ClaimScope::Subtree).unwrap();
            let lease = MutationClaimGuard::acquire(
                LeaseFamily::ExecutionClaim { execution_id },
                vec![claim.clone()],
            )
            .unwrap()
            .into_lease();
            register_runtime_supplemental_lease(&item_id, std::sync::Arc::new(lease)).unwrap();

            let snapshot = runtime_execution_claims(&item_id).unwrap();
            assert!(snapshot.iter().any(|held| held.covers(&claim)));

            unregister_runtime_execution(&item_id);
            drop(queue_lease);
        });
    }

    #[test]
    fn hard_link_aliases_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let a = PathClaim::resolve(&a, ClaimMode::Write, ClaimScope::Exact).unwrap();
        let b = PathClaim::resolve(&b, ClaimMode::Read, ClaimScope::Exact).unwrap();
        assert!(a.conflicts_with(&b));
    }

    #[test]
    fn run_unique_batch_group_allows_sibling_output_coownership_only() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let claim = PathClaim::resolve(dir.path(), ClaimMode::Write, ClaimScope::Subtree).unwrap();
            let first_id = Uuid::new_v4();
            let second_id = Uuid::new_v4();
            let _first = MutationClaimGuard::acquire_grouped(
                LeaseFamily::ExecutionClaim { execution_id: first_id },
                vec![claim.clone()],
                Some("album-batch:test-run-unique".to_string()),
            ).unwrap();
            let _second = MutationClaimGuard::acquire_grouped(
                LeaseFamily::ExecutionClaim { execution_id: second_id },
                vec![claim.clone()],
                Some("album-batch:test-run-unique".to_string()),
            ).unwrap();
            let error = MutationClaimGuard::acquire_grouped(
                LeaseFamily::ExecutionClaim { execution_id: Uuid::new_v4() },
                vec![claim],
                Some("album-batch:different-run".to_string()),
            ).unwrap_err();
            assert!(error.contains("live owner"));
        });
    }

    #[test]
    fn holder_drop_does_not_unlink_descriptor() {
        with_root(|_| {
            let lease = PersistentLease::create(LeaseFamily::EphemeralMutation { claim_id: Uuid::new_v4() }, &[]).unwrap();
            let path = lease.descriptor_path().to_path_buf();
            drop(lease);
            assert!(path.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn scanner_treats_opened_unpublished_ephemeral_descriptor_as_absent() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("scanner-race.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            let scanner = open_existing_descriptor(&descriptor).unwrap();
            let lock_error = scanner
                .try_lock_exclusive()
                .expect_err("scanner probe must observe the live guard flock");
            assert!(
                is_lock_contended(&lock_error),
                "scanner probe must fail specifically because the descriptor is locked: {lock_error}"
            );

            // Deterministic form of the registry-lock/unlink interleaving: the
            // scanner already owns an fd and has observed lock contention, then
            // ordinary lexical teardown unpublishes the pathname lock-free.
            drop(guard);
            assert!(
                !descriptor.exists(),
                "ordinary lexical teardown must unpublish the ephemeral descriptor"
            );

            let classified = classify_opened_descriptor(scanner, &descriptor, true)
                .expect("open-but-unpublished ephemeral descriptor must not become ENOENT admission failure");
            assert!(
                classified.is_none(),
                "an unpublished ephemeral inode no longer participates in mutation admission"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn scanner_keeps_published_ephemeral_rebind_fail_closed() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("scanner-rebind.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            let scanner = open_existing_descriptor(&descriptor).unwrap();
            let lock_error = scanner
                .try_lock_exclusive()
                .expect_err("scanner probe must observe the live guard flock");
            assert!(is_lock_contended(&lock_error));

            let displaced = descriptor.with_extension("lease.displaced-for-scanner");
            std::fs::rename(&descriptor, &displaced).unwrap();
            std::fs::write(&descriptor, b"replacement descriptor pathname").unwrap();

            let error = classify_opened_descriptor(scanner, &descriptor, true).unwrap_err();
            assert!(
                error.contains("fail closed") && error.contains("pathname rebound after open"),
                "a still-published rebound path must remain a fail-closed classification error: {error}"
            );
            drop(guard);
            assert_eq!(
                std::fs::read(&descriptor).unwrap(),
                b"replacement descriptor pathname",
                "neither scanner classification nor guard teardown may unlink a rebound pathname"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn scanner_keeps_disappeared_durable_descriptor_fail_closed() {
        with_root(|_| {
            let lease = PersistentLease::create(
                LeaseFamily::JournalOperation { job_id: Uuid::new_v4() },
                &[],
            )
            .unwrap();
            let descriptor = lease.descriptor_path().to_path_buf();
            let scanner = open_existing_descriptor(&descriptor).unwrap();
            let lock_error = scanner
                .try_lock_exclusive()
                .expect_err("scanner probe must observe durable live authority");
            assert!(is_lock_contended(&lock_error));

            std::fs::remove_file(&descriptor).unwrap();
            let error = classify_opened_descriptor(scanner, &descriptor, true).unwrap_err();
            assert!(
                error.contains("fail closed")
                    && error.contains("lstat persistent lease pathname"),
                "durable descriptor disappearance must not be reinterpreted as ephemeral retirement: {error}"
            );
            drop(lease);
        });
    }

    #[test]
    fn ordinary_ephemeral_guard_drop_retires_descriptor_before_reacquire() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("metadata.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            assert!(descriptor.exists(), "live guard must publish its descriptor");

            drop(guard);
            assert!(
                !descriptor.exists(),
                "ordinary ephemeral guard drop must retire its descriptor synchronously"
            );

            let replacement = MutationClaimGuard::acquire_ephemeral(vec![claim])
                .expect("same-path admission immediately after guard drop must be deterministic");
            let replacement_descriptor = replacement.lease().descriptor_path().to_path_buf();
            assert!(replacement_descriptor.exists());
            drop(replacement);
            assert!(
                !replacement_descriptor.exists(),
                "replacement guard must use the same eager retirement path"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn fork_inherited_cloexec_ephemeral_fd_cannot_block_next_guard() {
        use std::os::fd::AsRawFd;

        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("fork-race.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            // Read the private fd directly: inherited_fd() is an intentional
            // export API and therefore correctly disables eager retirement.
            let fd_flags = unsafe { libc::fcntl(guard.lease().file.as_raw_fd(), libc::F_GETFD) };
            assert!(fd_flags >= 0, "inspect ephemeral lease fd flags");
            assert_ne!(
                fd_flags & libc::FD_CLOEXEC,
                0,
                "regression requires the production CLOEXEC lease fd"
            );

            // CLOEXEC does not prevent fork-time descriptor inheritance; it
            // closes the inherited fd only when the child reaches exec. Hold a
            // fork child before exec so it deterministically co-holds the
            // guard's flock after the parent-side File is dropped. The child
            // executes only async-signal-safe libc calls after fork.
            let mut ready = [-1; 2];
            let mut release = [-1; 2];
            assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
            assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
            let child = unsafe { libc::fork() };
            if child == 0 {
                unsafe {
                    libc::close(ready[0]);
                    libc::close(release[1]);
                    let ready_byte = [b'R'];
                    if libc::write(ready[1], ready_byte.as_ptr().cast(), 1) != 1 {
                        libc::_exit(101);
                    }
                    let mut release_byte = [0u8; 1];
                    if libc::read(release[0], release_byte.as_mut_ptr().cast(), 1) != 1 {
                        libc::_exit(102);
                    }
                    libc::_exit(0);
                }
            }
            if child < 0 {
                unsafe {
                    libc::close(ready[0]);
                    libc::close(ready[1]);
                    libc::close(release[0]);
                    libc::close(release[1]);
                }
                panic!("fork test child: {}", std::io::Error::last_os_error());
            }
            unsafe {
                libc::close(ready[1]);
                libc::close(release[0]);
            }
            let mut ready_byte = [0u8; 1];
            let ready_result = unsafe { libc::read(ready[0], ready_byte.as_mut_ptr().cast(), 1) };
            unsafe { libc::close(ready[0]) };

            let mut descriptor_retired = false;
            let mut replacement = None;
            if ready_result == 1 {
                drop(guard);
                descriptor_retired = !descriptor.exists();
                replacement = Some(MutationClaimGuard::acquire_ephemeral(vec![claim]));
            } else {
                // The child still owns the inherited descriptor. End lexical
                // authority before cleanup, but do not try the substantive
                // assertion path when synchronization itself failed.
                drop(guard);
            }

            // Release and reap the fork child before any assertion below can
            // panic; otherwise a failed test could strand a pre-exec child.
            let release_byte = [b'X'];
            let signalled =
                unsafe { libc::write(release[1], release_byte.as_ptr().cast(), 1) == 1 };
            unsafe { libc::close(release[1]) };
            let mut status = 0;
            let waited = loop {
                let result = unsafe { libc::waitpid(child, &mut status, 0) };
                if result >= 0
                    || std::io::Error::last_os_error().kind()
                        != std::io::ErrorKind::Interrupted
                {
                    break result;
                }
            };

            assert_eq!(
                ready_result, 1,
                "fork child must confirm inherited-fd hold"
            );
            assert!(signalled, "release fork child");
            assert_eq!(waited, child, "reap fork child");
            assert!(libc::WIFEXITED(status), "fork child must exit normally");
            assert_eq!(libc::WEXITSTATUS(status), 0, "fork child exit status");
            assert!(
                descriptor_retired,
                "ordinary guard teardown must retire public authority even while an accidental fork duplicate still co-holds the old inode"
            );
            let replacement = replacement
                .expect("fork synchronization must produce a replacement attempt")
                .expect(
                    "fork-time CLOEXEC inheritance must not create a same-path self-overlap after guard teardown",
                );
            drop(replacement);
        });
    }

    #[test]
    fn exported_ephemeral_lifetime_keeps_descriptor_visible_until_final_holder_closes() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("supervised.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            let exported = guard
                .lease()
                .duplicate_lifetime_file()
                .expect("export close-only lifetime fd");
            drop(guard);

            assert!(
                descriptor.exists(),
                "guard teardown must not hide authority held by an exported lifetime fd"
            );
            let error = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap_err();
            assert!(
                error.contains("live owner"),
                "exported lifetime fd must continue excluding overlap: {error}"
            );

            drop(exported);
            let replacement = reacquire_after_intentional_ephemeral_authority_closes(
                claim,
                "scanner must reclaim the exported descriptor after its final holder closes",
            );
            assert!(
                !descriptor.exists(),
                "lazy reclamation must remove the retired exported descriptor"
            );
            drop(replacement);
        });
    }

    #[cfg(unix)]
    #[test]
    fn raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("raw-export.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            let raw = guard.lease().inherited_fd();
            let exported = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(exported >= 0, "duplicate raw inherited lease fd");
            drop(guard);

            let descriptor_visible = descriptor.exists();
            let competing = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]);
            let blocked_by_live_export = match competing {
                Err(error) => error.contains("live owner"),
                Ok(unexpected) => {
                    drop(unexpected);
                    false
                }
            };

            unsafe { libc::close(exported) };
            let replacement = reacquire_after_intentional_ephemeral_authority_closes(
                claim,
                "scanner must reclaim raw-export descriptor after duplicate closes",
            );

            assert!(
                descriptor_visible,
                "raw inherited-fd export must disable eager descriptor retirement"
            );
            assert!(
                blocked_by_live_export,
                "raw inherited-fd export must preserve live overlap exclusion"
            );
            assert!(
                !descriptor.exists(),
                "scanner must retire the raw-export descriptor after its duplicate closes"
            );
            drop(replacement);
        });
    }

    #[test]
    fn into_lease_preserves_detached_ephemeral_authority() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("detached.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let lease = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()])
                .unwrap()
                .into_lease();
            let descriptor = lease.descriptor_path().to_path_buf();
            assert!(descriptor.exists());

            let error = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap_err();
            assert!(
                error.contains("live owner"),
                "detached lease must remain externally visible: {error}"
            );

            drop(lease);
            assert!(
                descriptor.exists(),
                "raw PersistentLease drop keeps the existing close-only/lazy retirement contract"
            );
            let replacement = reacquire_after_intentional_ephemeral_authority_closes(
                claim,
                "scanner must reclaim a closed detached ephemeral lease",
            );
            assert!(!descriptor.exists());
            drop(replacement);
        });
    }

    #[cfg(unix)]
    #[test]
    fn ephemeral_guard_retirement_never_unlinks_a_rebound_descriptor_path() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("rebound.mp3");
            std::fs::write(&target, b"fixture").unwrap();
            let claim = PathClaim::resolve(&target, ClaimMode::Write, ClaimScope::Exact).unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(vec![claim]).unwrap();
            let descriptor = guard.lease().descriptor_path().to_path_buf();
            let displaced = descriptor.with_extension("lease.displaced");
            std::fs::rename(&descriptor, &displaced).unwrap();
            std::fs::write(&descriptor, b"replacement owned by another actor").unwrap();

            drop(guard);
            assert_eq!(
                std::fs::read(&descriptor).unwrap(),
                b"replacement owned by another actor",
                "guard retirement must prove pathname identity before unlinking"
            );

            std::fs::remove_file(&descriptor).unwrap();
            std::fs::remove_file(&displaced).unwrap();
        });
    }

    #[test]
    fn durable_descriptor_cannot_be_reclaimed_by_generic_admission() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let claim = PathClaim::resolve(dir.path(), ClaimMode::Write, ClaimScope::Subtree).unwrap();
            let lease = PersistentLease::create(LeaseFamily::JournalOperation { job_id: Uuid::new_v4() }, std::slice::from_ref(&claim)).unwrap();
            let descriptor = lease.descriptor_path().to_path_buf();
            drop(lease);
            let error = MutationClaimGuard::acquire_ephemeral(vec![claim]).unwrap_err();
            assert!(error.contains("recovery reservation"));
            assert!(descriptor.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn symlink_namespace_identity_conflicts_with_alias_replacement() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"x").unwrap();
        let alias = dir.path().join("alias");
        symlink(&target, &alias).unwrap();
        let admitted = PathClaim::resolve(&alias, ClaimMode::Read, ClaimScope::Exact).unwrap();
        let namespace_writer = PathClaim::resolve_with_semantics(
            &alias,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(admitted.conflicts_with(&namespace_writer));
        assert_eq!(admitted.identity.resolved_io_path, target.canonicalize().unwrap());
        assert_eq!(admitted.identity.namespace_path, alias);
        assert_eq!(admitted.identity.namespace_dependencies, vec![alias.clone()]);
        assert_eq!(namespace_writer.identity.resolved_io_path, alias);
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_alias_dependency_blocks_rebinding_without_locking_siblings() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-a");
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let alias = dir.path().join("alias");
        symlink(&real, &alias).unwrap();

        let admitted = PathClaim::resolve_with_semantics(
            &alias.join("new"),
            ClaimMode::Write,
            ClaimScope::Subtree,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert_eq!(admitted.identity.resolved_io_path, real.join("new"));
        assert_eq!(admitted.identity.namespace_dependencies, vec![alias.clone()]);

        let replace_alias = PathClaim::resolve_with_semantics(
            &alias,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(admitted.conflicts_with(&replace_alias));

        let unrelated = PathClaim::resolve(
            &sibling.join("other"),
            ClaimMode::Write,
            ClaimScope::Subtree,
        )
        .unwrap();
        assert!(!admitted.conflicts_with(&unrelated));
    }

    #[cfg(unix)]
    #[test]
    fn nested_alias_dependency_uses_stabilized_namespace_object_identity() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real-root");
        let target_root = dir.path().join("target-root");
        let sibling_target = dir.path().join("sibling-target");
        std::fs::create_dir_all(&real_root).unwrap();
        std::fs::create_dir_all(&target_root).unwrap();
        std::fs::create_dir_all(&sibling_target).unwrap();
        let front = dir.path().join("front");
        let inner = real_root.join("inner");
        let sibling = real_root.join("sibling");
        symlink(&real_root, &front).unwrap();
        symlink(&target_root, &inner).unwrap();
        symlink(&sibling_target, &sibling).unwrap();

        let through_front = PathClaim::resolve(
            &front.join("inner/new.flac"),
            ClaimMode::Read,
            ClaimScope::Exact,
        )
        .unwrap();
        assert_eq!(
            through_front.identity.resolved_io_path,
            target_root.join("new.flac")
        );
        assert_eq!(
            through_front.identity.namespace_dependencies,
            vec![front.clone(), inner.clone()]
        );

        let canonical_parent_writer = PathClaim::resolve_with_semantics(
            &inner,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(through_front.conflicts_with(&canonical_parent_writer));
        with_root(|_| {
            let _active = MutationClaimGuard::acquire_ephemeral(vec![through_front.clone()])
                .expect("admit active claim through outer alias");
            let error = MutationClaimGuard::acquire_ephemeral(vec![
                canonical_parent_writer.clone(),
            ])
            .expect_err("canonical-parent spelling must be Busy while dependency is live");
            assert!(error.contains("filesystem mutation conflicts"));
        });

        let lexical_writer = PathClaim::resolve_with_semantics(
            &front.join("inner"),
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(through_front.conflicts_with(&lexical_writer));

        let canonical_parent_subtree = PathClaim::resolve_with_semantics(
            &real_root,
            ClaimMode::Write,
            ClaimScope::Subtree,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(through_front.conflicts_with(&canonical_parent_subtree));

        let unrelated = PathClaim::resolve_with_semantics(
            &sibling,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(!through_front.conflicts_with(&unrelated));

        let dependency_reader = PathClaim::resolve_with_semantics(
            &inner,
            ClaimMode::Read,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(!through_front.conflicts_with(&dependency_reader));

        let canonical_spelling = PathClaim::resolve(
            &real_root.join("inner/new.flac"),
            ClaimMode::Read,
            ClaimScope::Exact,
        )
        .unwrap();
        assert!(through_front.covers(&canonical_spelling));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_then_parent_dir_preserves_filesystem_component_order() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let real = dir.path().join("real");
        let album = real.join("Album");
        let real_other = real.join("Other");
        let lexical_other = library.join("Other");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::create_dir_all(&real_other).unwrap();
        std::fs::create_dir_all(&lexical_other).unwrap();
        let current = library.join("current");
        symlink(&album, &current).unwrap();

        let requested = current.join("../Other/new.flac");
        let identity = ResolvedPathIdentity::resolve(&requested).unwrap();
        assert_eq!(identity.resolved_io_path, real_other.join("new.flac"));
        assert_ne!(identity.resolved_io_path, lexical_other.join("new.flac"));
        assert_eq!(identity.namespace_dependencies, vec![current.clone()]);

        let replace_current = PathClaim::resolve_with_semantics(
            &current,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        let active = PathClaim::resolve(&requested, ClaimMode::Write, ClaimScope::Exact).unwrap();
        assert!(active.conflicts_with(&replace_current));
    }

    #[cfg(unix)]
    #[test]
    fn prospective_suffix_normalizes_only_after_canonical_alias_anchor() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("real/Album");
        std::fs::create_dir_all(&album).unwrap();
        let alias = dir.path().join("alias");
        symlink(&album, &alias).unwrap();

        let requested = alias.join("missing/../new.flac");
        let identity = ResolvedPathIdentity::resolve(&requested).unwrap();
        assert_eq!(identity.canonical_existing_ancestor, album.canonicalize().unwrap());
        assert_eq!(identity.suffix, PathBuf::from("new.flac"));
        assert_eq!(identity.resolved_io_path, album.join("new.flac"));

        let escaping = alias.join("missing/../../outside.flac");
        let error = ResolvedPathIdentity::resolve(&escaping)
            .expect_err("prospective suffix must not escape above admitted ancestor");
        assert!(error.contains("escapes canonical existing ancestor"));
    }

    #[cfg(unix)]
    #[test]
    fn namespace_object_parent_uses_ordered_symlink_parent_semantics() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let album = dir.path().join("real/Album");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&album).unwrap();
        let current = library.join("current");
        symlink(&album, &current).unwrap();
        let final_referent = dir.path().join("referent");
        std::fs::write(&final_referent, b"referent").unwrap();
        let final_link = real.join("entry");
        symlink(&final_referent, &final_link).unwrap();

        let requested = current.join("../entry");
        let identity = ResolvedPathIdentity::resolve_with_semantics(
            &requested,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert_eq!(identity.resolved_io_path, final_link);
        assert_eq!(identity.namespace_dependencies, vec![current]);
        assert!(std::fs::symlink_metadata(&identity.resolved_io_path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn dependency_discovery_follows_symlink_targets_that_contain_more_aliases() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real-root");
        let target_root = dir.path().join("target-root");
        std::fs::create_dir_all(&real_root).unwrap();
        std::fs::create_dir_all(&target_root).unwrap();
        let inner = real_root.join("inner");
        symlink(&target_root, &inner).unwrap();
        let front = dir.path().join("front");
        symlink(&inner, &front).unwrap();

        let admitted = PathClaim::resolve(
            &front.join("new.flac"),
            ClaimMode::Read,
            ClaimScope::Exact,
        )
        .unwrap();
        assert_eq!(admitted.identity.resolved_io_path, target_root.join("new.flac"));
        assert_eq!(
            admitted.identity.namespace_dependencies,
            vec![front.clone(), inner.clone()]
        );

        let replace_inner = PathClaim::resolve_with_semantics(
            &inner,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert!(admitted.conflicts_with(&replace_inner));
    }

    #[cfg(unix)]
    #[test]
    fn namespace_object_resolution_preserves_final_symlink_and_resolves_parent_alias() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_parent = dir.path().join("real-parent");
        let parent_alias = dir.path().join("parent-alias");
        let referent = dir.path().join("referent");
        std::fs::create_dir_all(&real_parent).unwrap();
        std::fs::write(&referent, b"x").unwrap();
        symlink(&real_parent, &parent_alias).unwrap();
        let final_link = parent_alias.join("final-link");
        symlink(&referent, &final_link).unwrap();

        let identity = ResolvedPathIdentity::resolve_with_semantics(
            &final_link,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        assert_eq!(identity.resolved_io_path, real_parent.join("final-link"));
        assert_eq!(identity.namespace_dependencies, vec![parent_alias]);
        assert!(std::fs::symlink_metadata(&identity.resolved_io_path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn scoped_mutation_capability_can_be_transferred_to_bounded_worker_thread() {
        with_root(|_| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("track.mp3");
            std::fs::write(&path, b"fixture").unwrap();
            let claim = PathClaim::resolve_with_semantics(
                &path,
                ClaimMode::Write,
                ClaimScope::Exact,
                PathResolutionSemantics::NamespaceObject,
            )
            .unwrap();
            let _guard = MutationClaimGuard::acquire_ephemeral(vec![claim.clone()]).unwrap();

            with_scoped_mutation_claims(std::slice::from_ref(&claim), || {
                let inherited = current_scoped_mutation_claims();
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            with_scoped_mutation_claims(&inherited, || {
                                assert!(current_mutation_authority_covers(&claim).unwrap());
                            });
                        })
                        .join()
                        .unwrap();
                });
            });
        });
    }

    #[test]
    fn owner_identity_rejects_pid_reuse_and_boot_mismatch() {
        let current = OwnerProcessIdentity::current();
        assert!(current.appears_active(), "current process identity must validate itself");
        if current.start_ticks != 0 {
            let mut reused = current;
            reused.process_token = 0;
            reused.start_ticks = reused.start_ticks.wrapping_add(1).max(1);
            assert!(!reused.appears_active(), "same PID with different start identity is not the owner");
        }
        if current.boot_id_hash != 0 {
            let mut prior_boot = current;
            prior_boot.process_token = 0;
            prior_boot.boot_id_hash = prior_boot.boot_id_hash.wrapping_add(1).max(1);
            assert!(!prior_boot.appears_active(), "prior boot identity cannot own a current lease");
        }
    }

    #[test]
    fn immutable_family_is_encoded_in_descriptor_path_and_body() {
        with_root(|_| {
            let job_id = Uuid::new_v4();
            let lease = PersistentLease::create(LeaseFamily::JournalOperation { job_id }, &[]).unwrap();
            let path = lease.descriptor_path().to_path_buf();
            assert_eq!(path.parent().and_then(Path::file_name).and_then(|v| v.to_str()), Some("journal-operation"));
            let wrong = LeaseFamily::QueueExecution { execution_id: job_id };
            let error = PersistentLease::acquire_existing(&path, &wrong).unwrap_err();
            assert!(error.contains("live-owned") || error.contains("family mismatch"));
            drop(lease);
            let error = PersistentLease::acquire_existing(&path, &wrong).unwrap_err();
            assert!(error.contains("family mismatch"));
        });
    }

    #[test]
    fn truncated_durable_descriptor_routes_by_path_to_lifecycle_cleanup_only() {
        with_root(|_| {
            let job_id = Uuid::new_v4();
            let lease = PersistentLease::create(LeaseFamily::JournalOperation { job_id }, &[]).unwrap();
            let path = lease.descriptor_path().to_path_buf();
            drop(lease);
            std::fs::write(&path, b"{\"schema\":1").unwrap();
            let error = MutationClaimGuard::acquire_ephemeral(Vec::new()).unwrap_err();
            assert!(error.contains("lifecycle repair"), "generic admission must not infer durable malformed state: {error}");
            retire_setup_orphan_by_path_identity(&path, &LeaseFamily::JournalOperation { job_id }).unwrap();
            assert!(!path.exists());
        });
    }

    #[test]
    fn zero_length_crash_orphan_is_reclaimed_by_next_admission() {
        with_root(|root| {
            let orphan_family = LeaseFamily::JournalOperation { job_id: Uuid::new_v4() };
            let family_dir = root.join(orphan_family.namespace());
            create_private_dir(&family_dir).unwrap();
            let orphan_path = family_dir.join(format!(
                "{}--{}.lease",
                orphan_family.lifecycle_id(),
                Uuid::new_v4()
            ));
            let orphan = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&orphan_path)
                .unwrap();
            set_private_file_permissions(&orphan).unwrap();
            drop(orphan);
            assert_eq!(std::fs::metadata(&orphan_path).unwrap().len(), 0);

            let guard = MutationClaimGuard::acquire_ephemeral(Vec::new())
                .expect("admission should self-heal an unlocked zero-length create orphan");
            assert!(
                !orphan_path.exists(),
                "zero-length descriptor left by a killed creator must be reclaimed"
            );
            drop(guard);
        });
    }

    #[test]
    fn malformed_unlocked_ephemeral_descriptor_is_reclaimable_after_machine_loss() {
        with_root(|root| {
            let claim_id = Uuid::new_v4();
            let family = LeaseFamily::EphemeralMutation { claim_id };
            let family_dir = root.join(family.namespace());
            create_private_dir(&family_dir).unwrap();
            let orphan_path =
                family_dir.join(format!("{claim_id}--{}.lease", Uuid::new_v4()));
            std::fs::write(&orphan_path, b"{\"schema\":1").unwrap();

            let guard = MutationClaimGuard::acquire_ephemeral(Vec::new())
                .expect("unlocked malformed ephemeral state has no recovery authority");
            assert!(!orphan_path.exists());
            drop(guard);
        });
    }

    #[test]
    fn singular_lifecycle_create_reclaims_zero_length_reservation_orphan() {
        with_root(|root| {
            let job_id = Uuid::new_v4();
            let family = LeaseFamily::JournalOperation { job_id };
            let family_dir = root.join(family.namespace());
            create_private_dir(&family_dir).unwrap();
            let orphan_path = family_dir.join(format!("{job_id}--{}.lease", Uuid::new_v4()));
            let orphan = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&orphan_path)
                .unwrap();
            set_private_file_permissions(&orphan).unwrap();
            drop(orphan);

            let lease = PersistentLease::create(family, &[])
                .expect("singular lifecycle creation should repair its empty reservation orphan");
            assert!(!orphan_path.exists());
            assert!(lease.descriptor_path().exists());
        });
    }

    #[test]
    fn singular_lifecycle_create_cleans_abandoned_atomic_staging_file() {
        with_root(|root| {
            let job_id = Uuid::new_v4();
            let family = LeaseFamily::JournalOperation { job_id };
            let family_dir = root.join(family.namespace());
            create_private_dir(&family_dir).unwrap();
            let abandoned = family_dir.join(format!(
                ".{job_id}--{}.lease.tmp-{}",
                Uuid::new_v4(),
                Uuid::new_v4()
            ));
            std::fs::write(&abandoned, b"partial descriptor body").unwrap();

            let lease = PersistentLease::create(family, &[])
                .expect("new lifecycle creation should retire abandoned unscanned staging files");
            assert!(!abandoned.exists());
            assert!(lease.descriptor_path().exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_open_rejects_symlink_rebinding() {
        use std::os::unix::fs::symlink;
        with_root(|root| {
            let family = LeaseFamily::EphemeralMutation { claim_id: Uuid::new_v4() };
            let lease = PersistentLease::create(family.clone(), &[]).unwrap();
            let path = lease.descriptor_path().to_path_buf();
            drop(lease);
            let real = root.join("real-descriptor-copy");
            std::fs::rename(&path, &real).unwrap();
            symlink(&real, &path).unwrap();
            let error = PersistentLease::acquire_existing(&path, &family).unwrap_err();
            assert!(error.contains("open persistent lease"));
        });
    }

    #[test]
    fn same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire() {
        with_root(|_| {
            let family = LeaseFamily::JournalOperation {
                job_id: Uuid::new_v4(),
            };
            let lease = PersistentLease::create(family.clone(), &[])
                .expect("create durable lease");
            let path = lease.descriptor_path().to_path_buf();
            let descriptor_id = lease.descriptor_id();

            assert_eq!(
                descriptor_availability(&path).expect("ordinary availability").1,
                ClaimAvailability::Live,
                "generic admission must continue to see the live local owner"
            );
            assert_eq!(
                descriptor_recovery_availability_with_local_handoff(&path)
                    .expect("recovery handoff availability")
                    .1,
                ClaimAvailability::RecoveryReserved,
                "explicit recovery discovery may co-hold its own exact descriptor"
            );

            let strict_recovery_error = PersistentLease::acquire_existing_recovery(&path, &family)
                .expect_err("ordinary recovery must still reject a live local holder");
            assert!(
                strict_recovery_error.contains("live-owned"),
                "unexpected error: {strict_recovery_error}"
            );
            let recovery = PersistentLease::acquire_existing_recovery_with_local_handoff(&path, &family)
                .expect("explicit same-process handoff should co-hold exact locked descriptor");
            assert_eq!(recovery.descriptor_id(), descriptor_id);
            let strict_error = PersistentLease::acquire_existing(&path, &family)
                .expect_err("strict lifecycle acquisition must still reject a live holder");
            assert!(strict_error.contains("live-owned"), "unexpected error: {strict_error}");

            drop(recovery);
            drop(lease);
            let reclaimed = PersistentLease::acquire_existing_recovery(&path, &family)
                .expect("dead local owner should be acquired normally");
            assert_eq!(reclaimed.descriptor_id(), descriptor_id);
            drop(reclaimed);
            assert!(
                local_persistent_lease_file(&path).is_none(),
                "dropping the last local holder must prune the weak co-hold index"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn path_claim_json_round_trips_non_utf8_losslessly_and_keeps_utf8_wire_compatible() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let non_utf8 = dir
            .path()
            .join(OsString::from_vec(b"track-\xff.dsf".to_vec()));
        std::fs::write(&non_utf8, b"fixture").expect("non-UTF fixture");
        let claim = PathClaim::resolve_with_semantics(
            &non_utf8,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .expect("resolve non-UTF claim");
        let encoded = serde_json::to_value(&claim).expect("serialize non-UTF claim");
        assert!(
            encoded["identity"]["original"].get("unix_bytes").is_some(),
            "non-UTF Unix paths must use the lossless byte representation"
        );
        let decoded: PathClaim = serde_json::from_value(encoded).expect("deserialize non-UTF claim");
        assert_eq!(decoded, claim);

        let utf8 = dir.path().join("ordinary.dsf");
        std::fs::write(&utf8, b"fixture").expect("UTF-8 fixture");
        let utf8_claim = PathClaim::resolve(&utf8, ClaimMode::Write, ClaimScope::Exact)
            .expect("resolve UTF-8 claim");
        let utf8_json = serde_json::to_value(&utf8_claim).expect("serialize UTF-8 claim");
        assert!(
            utf8_json["identity"]["original"].is_string(),
            "schema-v1 descriptors must keep their existing JSON string encoding for UTF-8 paths"
        );
    }

}

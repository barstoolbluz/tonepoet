//! Cross-process concurrency primitives for independent tonepoet sessions.
//!
//! Persistent leases deliberately differ from the repository's short-lived
//! local file locks: a persistent lease is close-only, never explicitly
//! unlocks a possibly-shared open-file description, and never unlinks its
//! descriptor from `Drop`.

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPathIdentity {
    pub original: PathBuf,
    /// Absolute lexical namespace identity before symlink traversal.  This is
    /// carried alongside the resolved I/O identity so replacing/renaming a
    /// symlink or directory entry cannot silently rebind admitted work.
    #[serde(default)]
    pub namespace_path: PathBuf,
    /// Exact namespace objects whose binding was followed while resolving the
    /// admitted I/O path. Each key has its parent stabilized through preceding
    /// aliases while its final symlink entry remains unfollowed, so equivalent
    /// parent spellings compare as the same dependency object. Replacing one
    /// of these aliases requires WRITE and must conflict with this claim's
    /// implicit READ dependency.
    #[serde(default)]
    pub namespace_dependencies: Vec<PathBuf>,
    pub resolved_io_path: PathBuf,
    pub canonical_existing_ancestor: PathBuf,
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
    file: File,
    descriptor_path: PathBuf,
    descriptor_id: Uuid,
    family: LeaseFamily,
    claims: Arc<[PathClaim]>,
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

impl PersistentLease {
    pub fn create(family: LeaseFamily, claims: &[PathClaim]) -> Result<Self, String> {
        let root = coordination_root();
        create_private_dir(&root)?;
        let _registry = RegistryLock::acquire(&root)?;
        Self::create_while_registry_locked(&root, family, claims, None)
    }

    fn create_while_registry_locked(
        root: &Path,
        family: LeaseFamily,
        claims: &[PathClaim],
        coordination_group: Option<String>,
    ) -> Result<Self, String> {
        let family_dir = root.join(family.namespace());
        create_private_dir(&family_dir)?;
        let lifecycle_id = family.lifecycle_id();
        let singular_lifecycle = matches!(
            family,
            LeaseFamily::JournalOperation { .. }
                | LeaseFamily::QueueScope { .. }
                | LeaseFamily::QueueExecution { .. }
        );
        if singular_lifecycle {
            let prefix = format!("{lifecycle_id}--");
            for entry in std::fs::read_dir(&family_dir)
                .map_err(|e| format!("read persistent lease family {}: {e}", family_dir.display()))?
            {
                let entry = entry.map_err(|e| format!("read persistent lease family entry: {e}"))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".lease") {
                    return Err(format!(
                        "persistent lease lifecycle already has a descriptor: {:?} at {}",
                        family,
                        entry.path().display()
                    ));
                }
            }
        }
        let descriptor_id = Uuid::new_v4();
        let path = family_dir.join(format!("{lifecycle_id}--{descriptor_id}.lease"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&path)
            .map_err(|e| format!("create persistent lease {}: {e}", path.display()))?;
        set_private_file_permissions(&file)?;
        file.try_lock_exclusive()
            .map_err(|e| format!("lock new persistent lease {}: {e}", path.display()))?;
        let body = LeaseDescriptor {
            schema: DESCRIPTOR_SCHEMA,
            descriptor_id,
            family: family.clone(),
            owner: OwnerProcessIdentity::current(),
            created_unix_ms: unix_ms(),
            claims: claims.to_vec(),
            coordination_group,
        };
        let encoded = serde_json::to_vec(&body)
            .map_err(|e| format!("serialize persistent lease {}: {e}", path.display()))?;
        file.write_all(&encoded)
            .map_err(|e| format!("write persistent lease {}: {e}", path.display()))?;
        file.flush()
            .map_err(|e| format!("flush persistent lease {}: {e}", path.display()))?;
        Ok(Self {
            file,
            descriptor_path: path,
            descriptor_id,
            family,
            claims: claims.to_vec().into(),
        })
    }

    /// Acquire durable recovery authority using the global lock order.
    /// Classification may happen lock-free beforehand, but any transition from
    /// RecoveryReserved to a live recovery owner must take registry -> descriptor.
    pub fn acquire_existing_recovery(path: &Path, expected_family: &LeaseFamily) -> Result<Self, String> {
        let root = coordination_root();
        create_private_dir(&root)?;
        let _registry = RegistryLock::acquire(&root)?;
        Self::acquire_existing(path, expected_family)
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
            file,
            descriptor_path: path.to_path_buf(),
            descriptor_id: descriptor.descriptor_id,
            family: descriptor.family,
            claims: descriptor.claims.into(),
        })
    }

    pub fn descriptor_path(&self) -> &Path { &self.descriptor_path }
    pub fn descriptor_id(&self) -> Uuid { self.descriptor_id }
    pub fn family(&self) -> &LeaseFamily { &self.family }
    pub fn claims(&self) -> &[PathClaim] { &self.claims }

    /// Duplicate the descriptor while preserving the same underlying open-file
    /// description.  The duplicate is close-only and is suitable for handoff to
    /// a trusted tonepoet supervisor; no caller receives an unlock primitive.
    pub fn duplicate_lifetime_file(&self) -> Result<Arc<File>, String> {
        self.file
            .try_clone()
            .map(Arc::new)
            .map_err(|e| format!("duplicate persistent lease {}: {e}", self.descriptor_path.display()))
    }

    #[cfg(unix)]
    pub fn inherited_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
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
            file,
            descriptor_path,
            descriptor_id: descriptor.descriptor_id,
            family: descriptor.family,
            claims: descriptor.claims.into(),
        })
    }
}

// Intentionally no Drop implementation. Dropping `File` closes only this
// descriptor. In particular there is no FileExt::unlock and no unlink here.

#[derive(Debug)]
pub struct MutationClaimGuard {
    lease: PersistentLease,
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
            &root, family, &claims, coordination_group
        )?;
        Ok(Self { lease, claims: claims.into() })
    }

    pub fn claims(&self) -> &[PathClaim] { &self.claims }
    pub fn lease(&self) -> &PersistentLease { &self.lease }
    pub fn into_lease(self) -> PersistentLease { self.lease }
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
    let mut file = match open_existing_descriptor(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open coordination descriptor {}: {error}", path.display())),
    };
    let lock_state = match file.try_lock_exclusive() {
        Ok(()) => false,
        Err(error) if is_lock_contended(&error) => true,
        Err(error) => return Err(format!("probe coordination descriptor {}: {error}", path.display())),
    };
    let descriptor = read_descriptor_from(&mut file, path).map_err(|error| {
        if lock_state {
            format!("malformed contended coordination descriptor (fail closed): {error}")
        } else {
            format!("malformed coordination descriptor requires lifecycle repair: {error}")
        }
    })?;
    let availability = classify_availability(&descriptor.family, lock_state);
    Ok(Some((availability, descriptor.family, descriptor.claims, descriptor.coordination_group)))
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
        for child in std::fs::read_dir(entry.path()).map_err(|e| format!("read coordination family {}: {e}", entry.path().display()))? {
            let child = child.map_err(|e| format!("read coordination descriptor entry: {e}"))?;
            if child.path().extension().and_then(|v| v.to_str()) == Some("lease") {
                paths.push(child.path());
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_descriptor_from(file: &mut File, path: &Path) -> Result<LeaseDescriptor, String> {
    let descriptor_metadata = file.metadata()
        .map_err(|e| format!("fstat persistent lease {}: {e}", path.display()))?;
    if descriptor_metadata.len() > DESCRIPTOR_MAX_BYTES {
        return Err(format!("persistent lease descriptor exceeds {} bytes: {}", DESCRIPTOR_MAX_BYTES, path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let pathname_metadata = std::fs::symlink_metadata(path)
            .map_err(|e| format!("lstat persistent lease pathname {}: {e}", path.display()))?;
        if !pathname_metadata.file_type().is_file() {
            return Err(format!("persistent lease pathname is not a regular file: {}", path.display()));
        }
        if descriptor_metadata.dev() != pathname_metadata.dev() || descriptor_metadata.ino() != pathname_metadata.ino() {
            return Err(format!("persistent lease pathname rebound after open: {}", path.display()));
        }
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

pub fn coordination_root() -> PathBuf {
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
    let item_supervisor = if cfg!(test) && std::env::var_os("TONEPOET_SCRIPT_SUPERVISOR_HELPER").is_none() {
        // Pure unit tests exercise queue/DB state without a real CLI binary;
        // integration tests set the helper to CARGO_BIN_EXE_tonepoet and cover
        // the actual process boundary. Production always starts the supervisor.
        None
    } else {
        let queue_file = queue_lease.duplicate_lifetime_file()?;
        Some(crate::convert::script_supervisor::ItemExecutionSupervisorClient::start(&[queue_file])
            .map_err(|error| format!("start item execution supervisor for {item_id}: {error}"))?)
    };
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
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _serial = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TONEPOET_CONCURRENCY_DIR", dir.path());
        let result = f(dir.path());
        std::env::remove_var("TONEPOET_CONCURRENCY_DIR");
        result
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

}

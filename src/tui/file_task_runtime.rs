//! Durable supervision support for Browse copy/move jobs.
//!
//! The foreground TUI never owns blocking filesystem calls. Production jobs run
//! in a helper process and communicate over newline-delimited JSON. A local,
//! append-only journal records operation intent and recovery authority before a
//! helper can be abandoned. Journal records live beside the tonepoet config,
//! not on the source or destination mount, so an unavailable remote/removable
//! filesystem cannot prevent control-plane recovery.
//!
//! The first line of a journal is a complete snapshot. Subsequent lines are
//! compact typed deltas. This keeps destructive intent fsyncs small and makes
//! large multi-root jobs scale linearly in journal bytes instead of repeatedly
//! serializing the complete plan. A torn final append is ignored and repaired
//! before the next mutation.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::Arc;

use crate::concurrency::{ClaimMode, ClaimScope, LeaseFamily, MutationClaimGuard, OwnerProcessIdentity, PathClaim, PathResolutionSemantics, PersistentLease};

use super::browse::{BrowseMoveRecoveryProof, BrowsePasteRetryPlan};

pub const FILE_TASK_JOURNAL_SCHEMA: u32 = 1;
pub const DEFAULT_FILE_TASK_STALL_TIMEOUT_SECS: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableFileTaskLifecycle {
    Planned,
    Running,
    Stalled,
    Paused,
    Cancelling,
    Cancelled,
    Failed,
    Completed,
    AwaitingReconciliation,
    Reconciled,
}

impl DurableFileTaskLifecycle {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Failed | Self::Completed | Self::Reconciled
        )
    }

    pub const fn needs_reconciliation(self) -> bool {
        matches!(
            self,
            Self::Planned
                | Self::Running
                | Self::Stalled
                | Self::Paused
                | Self::Cancelling
                | Self::Cancelled
                | Self::Failed
                | Self::AwaitingReconciliation
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableTempArtifactKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableTempArtifact {
    pub path: PathBuf,
    pub destination: PathBuf,
    pub kind: DurableTempArtifactKind,
    pub owner_job_id: String,
    pub owner_generation: u64,
}

impl DurableTempArtifact {
    pub fn is_safe_private_artifact(&self, expected_job_id: &str) -> bool {
        if self.owner_job_id != expected_job_id
            || self.path.parent() != self.destination.parent()
        {
            return false;
        }
        let Some(name) = self.path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let kind_token = match self.kind {
            DurableTempArtifactKind::File => ".tonepoet-part-",
            DurableTempArtifactKind::Directory => ".tonepoet-tree-",
            DurableTempArtifactKind::Symlink => ".tonepoet-link-",
        };
        let token = format!(
            "{kind_token}{}-{}-",
            self.owner_job_id, self.owner_generation
        );
        private_artifact_name_matches(name, &token)
    }
}

fn private_artifact_name_matches(name: &str, token: &str) -> bool {
    let Some((prefix, suffix)) = name.split_once(token) else {
        return false;
    };
    let Some(sequence) = suffix.strip_suffix(".tmp") else {
        return false;
    };
    let Some((pid, nonce)) = sequence.split_once('-') else {
        return false;
    };
    prefix.starts_with('.')
        && prefix.len() > 1
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nonce.is_empty()
        && nonce.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DurableQuarantineState {
    /// The private path is reserved and the journal records the intended
    /// source-to-quarantine rename, but the rename may not have completed.
    #[default]
    IntentRecorded,
    /// The helper completed the source-to-quarantine rename and durably
    /// checkpointed that fact. The move is still reversible because no source
    /// entry has been removed.
    RenameConfirmed,
    /// At least one source entry was removed and the helper durably
    /// checkpointed the irreversible cleanup boundary.
    DeletionStarted,
}

impl DurableQuarantineState {
    pub const fn is_irreversibly_committed(self) -> bool {
        matches!(self, Self::DeletionStarted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableQuarantineArtifact {
    pub path: PathBuf,
    pub original_source: PathBuf,
    pub destination: PathBuf,
    #[serde(default)]
    pub state: DurableQuarantineState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableEndpointRole {
    Source,
    Destination,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DurableUnixEndpointStableIdentity {
    /// Persistent block-filesystem identity discovered through the platform's
    /// stable UUID namespace. The mount root is retained so a bind/subvolume
    /// view cannot silently substitute for the recorded endpoint.
    FilesystemUuid {
        uuid: String,
        filesystem_type: String,
        mount_root: PathBuf,
    },
    /// Stable remote/userspace mount source, such as an sshfs remote or NFS
    /// export. This is intentionally limited to filesystem types whose source
    /// names identify an endpoint across remounts.
    MountSource {
        source: String,
        filesystem_type: String,
        mount_root: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DurableEndpointVolumeIdentity {
    /// Legacy schema emitted by the first endpoint-aware implementation. A
    /// matching device remains a valid fast check, but a changed device is
    /// ambiguous rather than proof that a different endpoint is attached.
    UnixDevice { device: u64 },
    UnixMount {
        transient_device: u64,
        mount_point: PathBuf,
        #[serde(default)]
        stable: Option<DurableUnixEndpointStableIdentity>,
    },
    WindowsVolume { volume_serial: u32 },
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableEndpointIdentity {
    pub role: DurableEndpointRole,
    /// Logical operation root. Source identities use the selected source path;
    /// destination identities use the user-selected destination directory.
    pub operation_root: PathBuf,
    /// Existing directory whose volume identity was captured before mutation.
    /// Reconciliation must prove this exact endpoint is attached before it may
    /// interpret NotFound or mutate anything below the operation root.
    pub anchor_path: PathBuf,
    pub volume: DurableEndpointVolumeIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableNativeRenameIntent {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub source_manifest: tui_file_picker::SourceManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableFileTaskRecord {
    pub schema: u32,
    pub job_id: String,
    pub generation: u64,
    pub session_id: u64,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub lifecycle: DurableFileTaskLifecycle,
    pub is_move: bool,
    pub verification: tui_file_picker::VerificationMode,
    pub stall_timeout_secs: u64,
    pub mappings: Vec<tui_file_picker::PasteMapping>,
    /// Filesystem-authoritative endpoints resolved when the operation first
    /// entered the shared mutation registry. These are deliberately separate
    /// from `mappings`, which remain the user's lexical/display/retry identity.
    #[serde(default)]
    pub admitted_mappings: Vec<tui_file_picker::PasteMapping>,
    /// Resolved destination directory used for endpoint capture, root
    /// creation, and later recovery without re-resolving a mutable alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_destination_root: Option<PathBuf>,
    pub job: serde_json::Value,
    pub retry_plan: Option<BrowsePasteRetryPlan>,
    pub roots: Vec<tui_file_picker::FileTaskRootResult>,
    pub temp_artifacts: Vec<DurableTempArtifact>,
    #[serde(default)]
    pub artifact_generations: Vec<u64>,
    pub quarantine_artifacts: Vec<DurableQuarantineArtifact>,
    /// New journals set this before helper launch. Its presence means the
    /// helper cannot reach any filesystem mutation until endpoint identities
    /// have been durably captured, so a later generation may safely retry an
    /// interrupted pre-mutation capture. Older journals default to false and
    /// remain fail-closed.
    #[serde(default)]
    pub endpoint_identity_protocol: bool,
    #[serde(default)]
    pub endpoint_identities: Vec<DurableEndpointIdentity>,
    #[serde(default)]
    pub native_rename_intents: Vec<DurableNativeRenameIntent>,
    pub last_status: Option<String>,
    pub abandoned_reason: Option<String>,
    /// Strong origin identity for protocol-v2 ownership diagnostics and reboot/PID-reuse handling.
    #[serde(default)]
    pub origin_owner: Option<OwnerProcessIdentity>,
    /// Final-family lease descriptor. Its family is also encoded in the pathname.
    #[serde(default)]
    pub lease_descriptor: Option<PathBuf>,
    /// Fully resolved admission claims captured before registry acquisition.
    #[serde(default)]
    pub path_claims: Vec<PathClaim>,
    /// Upgrade-only diagnostic link to an adopted v0.4.8 top-level journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_journal_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_job_id: Option<String>,
}

impl DurableFileTaskRecord {
    pub fn admitted_mappings_for(
        &self,
        requested: &[tui_file_picker::PasteMapping],
    ) -> Result<Vec<tui_file_picker::PasteMapping>, String> {
        if self.mappings.len() != self.admitted_mappings.len() {
            return Err(
                "file-operation journal has no complete durable admitted mapping set"
                    .to_string(),
            );
        }
        requested
            .iter()
            .map(|mapping| {
                self.mappings
                    .iter()
                    .position(|known| known == mapping)
                    .and_then(|index| self.admitted_mappings.get(index))
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "file-operation journal has no admitted mapping for {} -> {}",
                            mapping.source.display(),
                            mapping.destination.display()
                        )
                    })
            })
            .collect()
    }

    pub fn logical_source_for_admitted(&self, admitted: &Path) -> Option<PathBuf> {
        self.admitted_mappings
            .iter()
            .position(|mapping| mapping.source == admitted)
            .and_then(|index| self.mappings.get(index))
            .map(|mapping| mapping.source.clone())
            .or_else(|| {
                self.admitted_mappings
                    .is_empty()
                    .then(|| {
                        self.mappings
                            .iter()
                            .find(|mapping| mapping.source == admitted)
                            .map(|mapping| mapping.source.clone())
                    })
                    .flatten()
            })
    }

    pub fn pending_mappings(&self) -> Vec<tui_file_picker::PasteMapping> {
        self.mappings
            .iter()
            .filter(|mapping| {
                !self.roots.iter().any(|root| {
                    root.source == mapping.source && root.disposition.is_completed()
                })
            })
            .cloned()
            .collect()
    }

    /// Include every mapping explicitly requested for the next generation and
    /// every mapping whose source still owns a durable quarantine obligation.
    /// The latter roots are not user-facing retry work: the helper consumes
    /// them during its reconciliation prelude before planning ordinary roots.
    pub fn mappings_for_reconciliation(
        &self,
        requested: &[tui_file_picker::PasteMapping],
    ) -> Vec<tui_file_picker::PasteMapping> {
        let quarantine_sources = self
            .quarantine_artifacts
            .iter()
            .filter_map(|artifact| {
                // Current helpers record admitted source paths, while older
                // and synthetic/recovery journals may contain the logical
                // source spelling. Accept either representation, but only
                // when it maps to this journal's immutable mapping set.
                self.logical_source_for_admitted(&artifact.original_source)
                    .or_else(|| {
                        self.mappings
                            .iter()
                            .find(|mapping| mapping.source == artifact.original_source)
                            .map(|mapping| mapping.source.clone())
                    })
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.mappings
            .iter()
            .filter(|mapping| {
                requested.contains(*mapping)
                    || quarantine_sources.contains(&mapping.source)
            })
            .cloned()
            .collect()
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.lifecycle.needs_reconciliation()
            || !self.pending_mappings().is_empty()
            || !self.temp_artifacts.is_empty()
            || !self.quarantine_artifacts.is_empty()
            || !self.native_rename_intents.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryRecordVersion {
    pub generation: u64,
    pub updated_unix_ms: u64,
    pub committed_len: u64,
    pub committed_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySurfaceAvailability {
    Live,
    Recoverable,
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct RecoverySurfaceRecord {
    pub journal_path: PathBuf,
    pub record: Option<DurableFileTaskRecord>,
    pub version: Option<RecoveryRecordVersion>,
    pub availability: RecoverySurfaceAvailability,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryDiscardSummary {
    pub deleted_incomplete: usize,
    pub restored_sources: usize,
    pub released_paths: usize,
}

pub(super) fn recovery_blocked_path_count(record: &DurableFileTaskRecord) -> usize {
    record
        .path_claims
        .iter()
        .map(|claim| claim.identity.original.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

const RECOVERY_DISCARDING_MARKER: &str = "recovery-ui-discard-in-progress";
const RECOVERY_DISCARDED_MARKER: &str = "recovery-ui-discarded";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableFileTaskJournalEntry {
    schema: u32,
    job_id: String,
    generation: u64,
    updated_unix_ms: u64,
    #[serde(flatten)]
    mutation: DurableFileTaskJournalMutation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
enum DurableFileTaskJournalMutation {
    Snapshot {
        record: DurableFileTaskRecord,
    },
    Lifecycle {
        lifecycle: DurableFileTaskLifecycle,
        status: String,
        #[serde(default)]
        abandoned_reason: Option<String>,
    },
    TempArtifactUpsert {
        artifact: DurableTempArtifact,
    },
    TempArtifactRemove {
        path: PathBuf,
    },
    QuarantineArtifactUpsert {
        artifact: DurableQuarantineArtifact,
    },
    QuarantineArtifactRemove {
        path: PathBuf,
    },
    QuarantineArtifactState {
        path: PathBuf,
        state: DurableQuarantineState,
    },
    EndpointIdentitiesSet {
        identities: Vec<DurableEndpointIdentity>,
    },
    NativeRenameIntentUpsert {
        intent: DurableNativeRenameIntent,
    },
    NativeRenameIntentRemove {
        source: PathBuf,
    },
    MoveRecoveryProofUpsert {
        source: PathBuf,
        proof: BrowseMoveRecoveryProof,
    },
    Checkpoint {
        lifecycle: DurableFileTaskLifecycle,
        status: String,
        roots: Vec<tui_file_picker::FileTaskRootResult>,
        #[serde(default)]
        retry_plan: Option<BrowsePasteRetryPlan>,
        #[serde(default)]
        cleared_native_sources: Vec<PathBuf>,
    },
}

impl DurableFileTaskJournalEntry {
    fn new(
        job_id: &str,
        generation: u64,
        mutation: DurableFileTaskJournalMutation,
    ) -> Self {
        Self {
            schema: FILE_TASK_JOURNAL_SCHEMA,
            job_id: job_id.to_string(),
            generation,
            updated_unix_ms: unix_ms(),
            mutation,
        }
    }

    fn snapshot(record: DurableFileTaskRecord) -> Self {
        Self {
            schema: FILE_TASK_JOURNAL_SCHEMA,
            job_id: record.job_id.clone(),
            generation: record.generation,
            updated_unix_ms: record.updated_unix_ms,
            mutation: DurableFileTaskJournalMutation::Snapshot { record },
        }
    }
}

#[derive(Debug)]
struct JournalRecoveryLease {
    lease: Arc<PersistentLease>,
}

impl JournalRecoveryLease {
    fn acquire(
        path: &Path,
        family: &LeaseFamily,
        allow_local_handoff: bool,
    ) -> Result<Self, String> {
        let lease = if allow_local_handoff {
            PersistentLease::acquire_existing_recovery_with_local_handoff(path, family)?
        } else {
            PersistentLease::acquire_existing_recovery(path, family)?
        };
        Ok(Self { lease: Arc::new(lease) })
    }

    fn holder(&self) -> Arc<PersistentLease> {
        Arc::clone(&self.lease)
    }
}

fn permits_same_process_recovery_handoff(
    journal_path: &Path,
    record: &DurableFileTaskRecord,
) -> bool {
    let root = journal_path.parent().unwrap_or_else(|| Path::new("."));
    if abandon_marker_path(root, &record.job_id, record.generation).exists() {
        return true;
    }
    matches!(
        record.lifecycle,
        DurableFileTaskLifecycle::Cancelled
            | DurableFileTaskLifecycle::Failed
            | DurableFileTaskLifecycle::Completed
            | DurableFileTaskLifecycle::AwaitingReconciliation
            | DurableFileTaskLifecycle::Reconciled
    )
}

#[derive(Debug, Clone)]
pub struct FileTaskJournalHandle {
    path: PathBuf,
    abandon_marker: PathBuf,
    job_id: String,
    generation: u64,
    lease: Option<Arc<PersistentLease>>,
}

impl FileTaskJournalHandle {
    pub fn create(
        job_id: String,
        generation: u64,
        session_id: u64,
        is_move: bool,
        verification: tui_file_picker::VerificationMode,
        stall_timeout_secs: u64,
        mappings: Vec<tui_file_picker::PasteMapping>,
        retry_plan: Option<BrowsePasteRetryPlan>,
        job: serde_json::Value,
    ) -> Result<Self, String> {
        let root = file_task_journal_dir();
        create_private_journal_directory(&root)
            .map_err(|error| format!("create file-operation journal directory: {error}"))?;
        if let Some(parent) = root.parent() {
            sync_directory_best_effort(parent);
        }

        let path = root.join(format!("{job_id}.jsonl"));
        let abandon_marker = abandon_marker_path(&root, &job_id, generation);
        let now = unix_ms();
        let FileTaskPathAdmission {
            claims,
            admitted_mappings,
        } = file_task_path_admission(is_move, &mappings)?;
        let admitted_destination_root = file_task_destination_root_from_job(&job, &mappings)?;
        let record = DurableFileTaskRecord {
            schema: FILE_TASK_JOURNAL_SCHEMA,
            job_id: job_id.clone(),
            generation,
            session_id,
            created_unix_ms: now,
            updated_unix_ms: now,
            lifecycle: DurableFileTaskLifecycle::Planned,
            is_move,
            verification,
            stall_timeout_secs: stall_timeout_secs.max(1),
            mappings,
            admitted_mappings,
            admitted_destination_root: Some(admitted_destination_root),
            job,
            retry_plan,
            roots: Vec::new(),
            temp_artifacts: Vec::new(),
            artifact_generations: vec![generation],
            quarantine_artifacts: Vec::new(),
            endpoint_identity_protocol: true,
            endpoint_identities: Vec::new(),
            native_rename_intents: Vec::new(),
            last_status: Some("planned".to_string()),
            abandoned_reason: None,
            origin_owner: Some(OwnerProcessIdentity::current()),
            lease_descriptor: None,
            path_claims: Vec::new(),
            legacy_journal_path: None,
            legacy_job_id: None,
        };
        let job_uuid = uuid::Uuid::parse_str(&job_id)
            .map_err(|error| format!("invalid file-operation job UUID {job_id}: {error}"))?;
        let guard = MutationClaimGuard::acquire(
            LeaseFamily::JournalOperation { job_id: job_uuid },
            claims.clone(),
        )?;
        let lease = Arc::new(guard.into_lease());
        let mut record = record;
        record.path_claims = claims.clone();
        record.lease_descriptor = Some(lease.descriptor_path().to_path_buf());
        if let Err(error) = create_record(&path, &record) {
            let descriptor = lease.descriptor_path().to_path_buf();
            let family = lease.family().clone();
            drop(lease);
            let _ = crate::concurrency::retire_descriptor_after_lifecycle_release(&descriptor, &family);
            return Err(error);
        }
        Ok(Self {
            path,
            abandon_marker,
            job_id,
            generation,
            lease: Some(lease),
        })
    }

    pub fn resume(
        path: PathBuf,
        generation: u64,
        session_id: u64,
        is_move: bool,
        verification: tui_file_picker::VerificationMode,
        stall_timeout_secs: u64,
        mappings: Vec<tui_file_picker::PasteMapping>,
        retry_plan: Option<BrowsePasteRetryPlan>,
        job: serde_json::Value,
    ) -> Result<Self, String> {
        let previous = load_record(&path)?;
        let job_uuid = uuid::Uuid::parse_str(&previous.job_id)
            .map_err(|error| format!("invalid file-operation job UUID {}: {error}", previous.job_id))?;
        let family = LeaseFamily::JournalOperation { job_id: job_uuid };
        let descriptor_path = previous.lease_descriptor.clone()
            .or_else(|| crate::concurrency::find_family_descriptor(&family).ok().flatten())
            .ok_or_else(|| "legacy or incomplete file-operation journal has no protocol-v2 ownership descriptor; automatic recovery is refused".to_string())?;
        let recovery = JournalRecoveryLease::acquire(
            &descriptor_path,
            &family,
            permits_same_process_recovery_handoff(&path, &previous),
        )?;
        let record = resume_record_locked(
            &recovery,
            &path,
            generation,
            session_id,
            is_move,
            verification,
            stall_timeout_secs,
            mappings,
            retry_plan,
            job,
        )?;
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let abandon_marker = abandon_marker_path(root, &record.job_id, generation);
        Ok(Self {
            path,
            abandon_marker,
            job_id: record.job_id,
            generation,
            lease: Some(recovery.holder()),
        })
    }

    pub fn open(path: PathBuf) -> Result<Self, String> {
        let record = load_record(&path)?;
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let abandon_marker = abandon_marker_path(root, &record.job_id, record.generation);
        Ok(Self {
            path,
            abandon_marker,
            job_id: record.job_id,
            generation: record.generation,
            lease: None,
        })
    }

    #[cfg(unix)]
    pub unsafe fn open_with_inherited_lease(path: PathBuf, lease_fd: std::os::fd::RawFd) -> Result<Self, String> {
        let record = load_record(&path)?;
        let job_uuid = uuid::Uuid::parse_str(&record.job_id)
            .map_err(|error| format!("invalid file-operation job UUID {}: {error}", record.job_id))?;
        let family = LeaseFamily::JournalOperation { job_id: job_uuid };
        let descriptor_path = record.lease_descriptor.clone()
            .ok_or_else(|| "file-operation journal is missing its ownership descriptor reference".to_string())?;
        let lease = Arc::new(PersistentLease::from_inherited_fd(lease_fd, descriptor_path, family)?);
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let abandon_marker = abandon_marker_path(root, &record.job_id, record.generation);
        Ok(Self { path, abandon_marker, job_id: record.job_id, generation: record.generation, lease: Some(lease) })
    }

    #[cfg(unix)]
    pub fn lease_fd(&self) -> Result<std::os::fd::RawFd, String> {
        self.lease.as_ref().map(|lease| lease.inherited_fd())
            .ok_or_else(|| "file-operation journal handle has no ownership lease".to_string())
    }

    /// Retire a journal and its durable descriptor only after a clean terminal
    /// checkpoint and after all helper/supervisor holders have exited.
    pub fn retire_terminal_clean(self) -> Result<bool, String> {
        let record = self.load()?;
        let clean = matches!(record.lifecycle, DurableFileTaskLifecycle::Completed | DurableFileTaskLifecycle::Reconciled)
            && record.pending_mappings().is_empty()
            && record.temp_artifacts.is_empty()
            && record.quarantine_artifacts.is_empty()
            && record.native_rename_intents.is_empty();
        if !clean { return Ok(false); }
        let job_id = uuid::Uuid::parse_str(&record.job_id)
            .map_err(|e| format!("terminal journal has invalid job UUID: {e}"))?;
        let descriptor = record.lease_descriptor.clone()
            .or_else(|| crate::concurrency::find_family_descriptor(&LeaseFamily::JournalOperation { job_id }).ok().flatten());
        fs::remove_file(&self.path)
            .map_err(|e| format!("remove terminal file-operation journal {}: {e}", self.path.display()))?;
        if let Some(parent) = self.path.parent() {
            File::open(parent).and_then(|dir| dir.sync_all())
                .map_err(|e| format!("sync terminal journal directory {}: {e}", parent.display()))?;
        }
        let family = LeaseFamily::JournalOperation { job_id };
        drop(self);
        if let Some(descriptor) = descriptor {
            crate::concurrency::retire_descriptor_after_lifecycle_release(&descriptor, &family)?;
        }
        Ok(true)
    }


    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_abandoned(&self) -> bool {
        self.abandon_marker.exists()
    }

    pub fn mark_abandoned(&self, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        self.create_abandon_marker(&reason)?;
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::Lifecycle {
                lifecycle: DurableFileTaskLifecycle::AwaitingReconciliation,
                status: "helper abandoned; reconciliation required".to_string(),
                abandoned_reason: Some(reason),
            },
        )
    }

    pub fn mark_lifecycle(
        &self,
        lifecycle: DurableFileTaskLifecycle,
        status: impl Into<String>,
    ) -> Result<(), String> {
        let durable = lifecycle.is_terminal()
            || matches!(
                lifecycle,
                DurableFileTaskLifecycle::Cancelling
                    | DurableFileTaskLifecycle::AwaitingReconciliation
            );
        self.append_mutation(
            durable,
            true,
            DurableFileTaskJournalMutation::Lifecycle {
                lifecycle,
                status: status.into(),
                abandoned_reason: None,
            },
        )
    }

    pub fn record_temp_artifact(&self, path: &Path, destination: &Path) -> Result<(), String> {
        self.record_temporary_artifact(path, destination, DurableTempArtifactKind::File)
    }

    pub fn record_temp_directory_artifact(
        &self,
        path: &Path,
        destination: &Path,
    ) -> Result<(), String> {
        self.record_temporary_artifact(path, destination, DurableTempArtifactKind::Directory)
    }

    pub fn record_temp_symlink_artifact(
        &self,
        path: &Path,
        destination: &Path,
    ) -> Result<(), String> {
        self.record_temporary_artifact(path, destination, DurableTempArtifactKind::Symlink)
    }

    fn record_temporary_artifact(
        &self,
        path: &Path,
        destination: &Path,
        kind: DurableTempArtifactKind,
    ) -> Result<(), String> {
        // The durable generation snapshot is written before any helper I/O.
        // Per-file temp intent is a non-fsync hint: after a crash or forced
        // termination, reconciliation derives exact generation-qualified temp
        // names from the durable plan and records each discovered obligation
        // durably before removing it. This preserves the local-copy fast path.
        self.append_mutation(
            false,
            true,
            DurableFileTaskJournalMutation::TempArtifactUpsert {
                artifact: DurableTempArtifact {
                    path: path.to_path_buf(),
                    destination: destination.to_path_buf(),
                    kind,
                    owner_job_id: self.job_id.clone(),
                    owner_generation: self.generation,
                },
            },
        )
    }

    pub fn record_recovered_temp_artifact(
        &self,
        path: &Path,
        destination: &Path,
        kind: DurableTempArtifactKind,
        owner_generation: u64,
    ) -> Result<(), String> {
        let artifact = DurableTempArtifact {
            path: path.to_path_buf(),
            destination: destination.to_path_buf(),
            kind,
            owner_job_id: self.job_id.clone(),
            owner_generation,
        };
        if !artifact.is_safe_private_artifact(&self.job_id) {
            return Err(format!(
                "refused to journal invalid recovered temporary artifact {}",
                path.display()
            ));
        }
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::TempArtifactUpsert { artifact },
        )
    }

    pub fn clear_temp_artifact(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            false,
            true,
            DurableFileTaskJournalMutation::TempArtifactRemove {
                path: path.to_path_buf(),
            },
        )
    }

    /// Clear a cleanup obligation from the controlling process after an
    /// abandoned helper has been reaped. Unlike helper-originated updates,
    /// this control-plane mutation is allowed after the generation's abandon
    /// marker exists.
    pub fn clear_recovered_temp_artifact(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::TempArtifactRemove {
                path: path.to_path_buf(),
            },
        )
    }

    pub fn record_quarantine_artifact(
        &self,
        path: &Path,
        original_source: &Path,
        destination: &Path,
    ) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::QuarantineArtifactUpsert {
                artifact: DurableQuarantineArtifact {
                    path: path.to_path_buf(),
                    original_source: original_source.to_path_buf(),
                    destination: destination.to_path_buf(),
                    state: DurableQuarantineState::IntentRecorded,
                },
            },
        )
    }

    pub fn mark_quarantine_renamed(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::QuarantineArtifactState {
                path: path.to_path_buf(),
                state: DurableQuarantineState::RenameConfirmed,
            },
        )
    }

    pub fn mark_quarantine_deletion_started(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::QuarantineArtifactState {
                path: path.to_path_buf(),
                state: DurableQuarantineState::DeletionStarted,
            },
        )
    }

    pub fn record_endpoint_identities(
        &self,
        identities: &[DurableEndpointIdentity],
    ) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::EndpointIdentitiesSet {
                identities: identities.to_vec(),
            },
        )
    }

    pub fn clear_quarantine_artifact(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            false,
            true,
            DurableFileTaskJournalMutation::QuarantineArtifactRemove {
                path: path.to_path_buf(),
            },
        )
    }

    /// Clear a source-restoration obligation from the controlling process after
    /// recovery has acquired the durable operation lease. This remains allowed
    /// after an abandon marker because the helper no longer owns mutation.
    pub fn clear_recovered_quarantine_artifact(&self, path: &Path) -> Result<(), String> {
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::QuarantineArtifactRemove {
                path: path.to_path_buf(),
            },
        )
    }

    fn mark_recovery_discard_started(&self) -> Result<(), String> {
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::Lifecycle {
                lifecycle: DurableFileTaskLifecycle::AwaitingReconciliation,
                status: RECOVERY_DISCARDING_MARKER.to_string(),
                abandoned_reason: None,
            },
        )
    }

    fn mark_recovery_discarded(&self) -> Result<(), String> {
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::Lifecycle {
                lifecycle: DurableFileTaskLifecycle::Reconciled,
                status: RECOVERY_DISCARDED_MARKER.to_string(),
                abandoned_reason: None,
            },
        )
    }

    pub fn record_native_rename_intent(
        &self,
        source: &Path,
        destination: &Path,
        source_manifest: &tui_file_picker::SourceManifest,
    ) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::NativeRenameIntentUpsert {
                intent: DurableNativeRenameIntent {
                    source: source.to_path_buf(),
                    destination: destination.to_path_buf(),
                    source_manifest: source_manifest.clone(),
                },
            },
        )
    }

    pub fn clear_native_rename_intent(&self, source: &Path) -> Result<(), String> {
        self.append_mutation(
            false,
            true,
            DurableFileTaskJournalMutation::NativeRenameIntentRemove {
                source: source.to_path_buf(),
            },
        )
    }

    /// Clear an interrupted direct-rename obligation after the controlling
    /// process has acquired the durable recovery lease. As with recovered
    /// source-quarantine cleanup, this is permitted after helper abandonment.
    pub fn clear_recovered_native_rename_intent(&self, source: &Path) -> Result<(), String> {
        self.append_mutation(
            true,
            false,
            DurableFileTaskJournalMutation::NativeRenameIntentRemove {
                source: source.to_path_buf(),
            },
        )
    }

    /// Persist the exact proof that authorizes source cleanup for a copied move
    /// root. This is a small delta and is fsynced before quarantine begins.
    pub fn record_move_recovery_proof(
        &self,
        source: &Path,
        proof: &BrowseMoveRecoveryProof,
    ) -> Result<(), String> {
        self.append_mutation(
            true,
            true,
            DurableFileTaskJournalMutation::MoveRecoveryProofUpsert {
                source: source.to_path_buf(),
                proof: proof.clone(),
            },
        )
    }

    pub fn record_checkpoint(
        &self,
        lifecycle: DurableFileTaskLifecycle,
        status: impl Into<String>,
        roots: &[tui_file_picker::FileTaskRootResult],
        retry_plan: Option<&BrowsePasteRetryPlan>,
    ) -> Result<(), String> {
        self.record_checkpoint_clearing_native_intents(
            lifecycle,
            status,
            roots,
            retry_plan,
            &[],
        )
    }

    pub fn record_checkpoint_clearing_native_intents(
        &self,
        lifecycle: DurableFileTaskLifecycle,
        status: impl Into<String>,
        roots: &[tui_file_picker::FileTaskRootResult],
        retry_plan: Option<&BrowsePasteRetryPlan>,
        cleared_sources: &[PathBuf],
    ) -> Result<(), String> {
        let durable = lifecycle.is_terminal()
            || matches!(
                lifecycle,
                DurableFileTaskLifecycle::Cancelling
                    | DurableFileTaskLifecycle::AwaitingReconciliation
            )
            || !cleared_sources.is_empty();
        self.append_mutation(
            durable,
            true,
            DurableFileTaskJournalMutation::Checkpoint {
                lifecycle,
                status: status.into(),
                roots: roots.to_vec(),
                retry_plan: retry_plan.cloned(),
                cleared_native_sources: cleared_sources.to_vec(),
            },
        )
    }

    pub fn load(&self) -> Result<DurableFileTaskRecord, String> {
        load_record(&self.path)
    }

    fn append_mutation(
        &self,
        durable: bool,
        reject_if_abandoned: bool,
        mutation: DurableFileTaskJournalMutation,
    ) -> Result<(), String> {
        let entry = DurableFileTaskJournalEntry::new(&self.job_id, self.generation, mutation);
        append_entry_locked(
            &self.path,
            &self.job_id,
            self.generation,
            durable,
            || reject_if_abandoned && self.is_abandoned(),
            &entry,
        )
    }

    fn create_abandon_marker(&self, reason: &str) -> Result<(), String> {
        match create_private_marker_file(&self.abandon_marker) {
            Ok(mut file) => {
                file.write_all(reason.as_bytes())
                    .map_err(|error| format!("write file-task abandon marker: {error}"))?;
                file.write_all(b"\n")
                    .map_err(|error| format!("finish file-task abandon marker: {error}"))?;
                file.sync_all()
                    .map_err(|error| format!("sync file-task abandon marker: {error}"))?;
                if let Some(parent) = self.abandon_marker.parent() {
                    sync_directory_best_effort(parent);
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(format!("create file-task abandon marker: {error}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileTaskWireEvent {
    Progress {
        update: tui_file_picker::FileTaskProgressUpdate,
    },
    Complete {
        report: tui_file_picker::FileTaskCompletionReport,
        retry_plan: Option<BrowsePasteRetryPlan>,
    },
}

pub fn write_wire_event(writer: &mut impl Write, event: &FileTaskWireEvent) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, event)
        .map_err(|error| format!("serialize file-task event: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("write file-task event delimiter: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("flush file-task event: {error}"))
}

pub fn write_wire_control(
    writer: &mut impl Write,
    control: &super::app::FileTaskControl,
) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, control)
        .map_err(|error| format!("serialize file-task control: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("write file-task control delimiter: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("flush file-task control: {error}"))
}

pub fn read_wire_events(
    reader: impl std::io::Read,
) -> impl Iterator<Item = Result<FileTaskWireEvent, String>> {
    BufReader::new(reader).lines().map(|line| {
        let line = line.map_err(|error| format!("read file-task event: {error}"))?;
        serde_json::from_str(&line).map_err(|error| format!("parse file-task event: {error}"))
    })
}

pub fn load_record(path: &Path) -> Result<DurableFileTaskRecord, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("open file-operation journal {}: {error}", path.display()))?;
    file.lock_shared()
        .map_err(|error| format!("lock file-operation journal {}: {error}", path.display()))?;
    let result = scan_record_file(&mut file, path).map(|(record, _)| record);
    let _ = FileExt::unlock(&file);
    result
}

fn load_record_with_version(
    path: &Path,
) -> Result<(DurableFileTaskRecord, RecoveryRecordVersion), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("open file-operation journal {}: {error}", path.display()))?;
    file.lock_shared()
        .map_err(|error| format!("lock file-operation journal {}: {error}", path.display()))?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let (record, committed_len) =
            scan_record_file_inner(&mut file, path, Some(&mut hasher))?;
        let version = RecoveryRecordVersion {
            generation: record.generation,
            updated_unix_ms: record.updated_unix_ms,
            committed_len,
            committed_sha256: hasher.finalize().into(),
        };
        Ok((record, version))
    })();
    let _ = FileExt::unlock(&file);
    result
}

fn scan_record_file(
    file: &mut File,
    path: &Path,
) -> Result<(DurableFileTaskRecord, u64), String> {
    scan_record_file_inner(file, path, None)
}

fn scan_record_file_inner(
    file: &mut File,
    path: &Path,
    mut committed_hasher: Option<&mut Sha256>,
) -> Result<(DurableFileTaskRecord, u64), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek file-operation journal {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut latest = None;
    let mut valid_len = 0u64;
    let mut consumed = 0u64;
    loop {
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("read file-operation journal {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        consumed = consumed.saturating_add(read as u64);
        // Every committed journal entry is newline-terminated. Even when an
        // unterminated tail happens to contain valid JSON, it is not a
        // committed append: accepting it would make the next append concatenate
        // two JSON values and permanently hide all later deltas.
        if line.last() != Some(&b'\n') {
            break;
        }
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            if let Some(hasher) = committed_hasher.as_deref_mut() {
                hasher.update(&line);
            }
            valid_len = consumed;
            continue;
        }
        match decode_journal_line(&line) {
            Ok(DecodedJournalLine::Entry(entry)) => {
                apply_journal_entry(&mut latest, entry, path)?;
                if let Some(hasher) = committed_hasher.as_deref_mut() {
                    hasher.update(&line);
                }
                valid_len = consumed;
            }
            Ok(DecodedJournalLine::LegacySnapshot(record)) => {
                if record.schema != FILE_TASK_JOURNAL_SCHEMA {
                    return Err(format!(
                        "unsupported file-operation journal schema in {}",
                        path.display()
                    ));
                }
                latest = Some(record);
                if let Some(hasher) = committed_hasher.as_deref_mut() {
                    hasher.update(&line);
                }
                valid_len = consumed;
            }
            Err(_) => {
                // A crash can tear only the final append. Earlier complete
                // entries remain authoritative and are retained.
                break;
            }
        }
    }
    latest
        .map(|record| (record, valid_len))
        .ok_or_else(|| format!("file-operation journal {} has no valid record", path.display()))
}

enum DecodedJournalLine {
    Entry(DurableFileTaskJournalEntry),
    LegacySnapshot(DurableFileTaskRecord),
}

fn decode_journal_line(line: &[u8]) -> Result<DecodedJournalLine, serde_json::Error> {
    match serde_json::from_slice::<DurableFileTaskJournalEntry>(line) {
        Ok(entry) => Ok(DecodedJournalLine::Entry(entry)),
        Err(entry_error) => match serde_json::from_slice::<DurableFileTaskRecord>(line) {
            Ok(record) => Ok(DecodedJournalLine::LegacySnapshot(record)),
            Err(_) => Err(entry_error),
        },
    }
}

fn apply_journal_entry(
    latest: &mut Option<DurableFileTaskRecord>,
    entry: DurableFileTaskJournalEntry,
    path: &Path,
) -> Result<(), String> {
    let DurableFileTaskJournalEntry {
        schema,
        job_id,
        generation,
        updated_unix_ms,
        mutation,
    } = entry;
    if schema != FILE_TASK_JOURNAL_SCHEMA {
        return Err(format!(
            "unsupported file-operation journal schema in {}",
            path.display()
        ));
    }

    let mutation = match mutation {
        DurableFileTaskJournalMutation::Snapshot { record } => {
            if record.schema != FILE_TASK_JOURNAL_SCHEMA
                || record.job_id != job_id
                || record.generation != generation
            {
                return Err(format!(
                    "invalid file-operation journal snapshot identity in {}",
                    path.display()
                ));
            }
            if let Some(previous) = latest.as_ref() {
                if previous.job_id != record.job_id || record.generation <= previous.generation {
                    return Err(format!(
                        "invalid file-operation journal generation transition in {}",
                        path.display()
                    ));
                }
            }
            *latest = Some(record);
            return Ok(());
        }
        mutation => mutation,
    };

    let record = latest.as_mut().ok_or_else(|| {
        format!(
            "file-operation journal {} begins with a delta instead of a snapshot",
            path.display()
        )
    })?;
    if record.job_id != job_id || record.generation != generation {
        return Err(format!(
            "file-operation journal generation changed unexpectedly in {}",
            path.display()
        ));
    }
    record.updated_unix_ms = updated_unix_ms;
    match mutation {
        DurableFileTaskJournalMutation::Snapshot { .. } => unreachable!(),
        DurableFileTaskJournalMutation::Lifecycle {
            lifecycle,
            status,
            abandoned_reason,
        } => {
            record.lifecycle = lifecycle;
            record.last_status = Some(status);
            if abandoned_reason.is_some() {
                record.abandoned_reason = abandoned_reason;
            }
        }
        DurableFileTaskJournalMutation::TempArtifactUpsert { artifact } => {
            record
                .temp_artifacts
                .retain(|known| known.path != artifact.path);
            if !record
                .artifact_generations
                .contains(&artifact.owner_generation)
            {
                record
                    .artifact_generations
                    .push(artifact.owner_generation);
            }
            record.temp_artifacts.push(artifact);
        }
        DurableFileTaskJournalMutation::TempArtifactRemove { path } => {
            record.temp_artifacts.retain(|known| known.path != path);
        }
        DurableFileTaskJournalMutation::QuarantineArtifactUpsert { artifact } => {
            record
                .quarantine_artifacts
                .retain(|known| known.original_source != artifact.original_source);
            record.quarantine_artifacts.push(artifact);
        }
        DurableFileTaskJournalMutation::QuarantineArtifactRemove { path } => {
            record
                .quarantine_artifacts
                .retain(|known| known.path != path);
        }
        DurableFileTaskJournalMutation::QuarantineArtifactState { path, state } => {
            let artifact = record
                .quarantine_artifacts
                .iter_mut()
                .find(|known| known.path == path)
                .ok_or_else(|| {
                    format!(
                        "file-operation journal has no quarantine artifact at {}",
                        path.display()
                    )
                })?;
            if artifact.state == DurableQuarantineState::DeletionStarted
                && state != DurableQuarantineState::DeletionStarted
            {
                return Err(format!(
                    "file-operation journal refused to move quarantine {} before its irreversible cleanup boundary",
                    path.display()
                ));
            }
            artifact.state = state;
        }
        DurableFileTaskJournalMutation::EndpointIdentitiesSet { identities } => {
            if record.endpoint_identities.is_empty() {
                record.endpoint_identities = identities;
            } else if record.endpoint_identities != identities {
                return Err(
                    "file-operation endpoint identities changed after initial capture".to_string(),
                );
            }
        }
        DurableFileTaskJournalMutation::NativeRenameIntentUpsert { intent } => {
            record
                .native_rename_intents
                .retain(|known| known.source != intent.source);
            record.native_rename_intents.push(intent);
        }
        DurableFileTaskJournalMutation::NativeRenameIntentRemove { source } => {
            record
                .native_rename_intents
                .retain(|known| known.source != source);
        }
        DurableFileTaskJournalMutation::MoveRecoveryProofUpsert { source, proof } => {
            let retry = record.retry_plan.get_or_insert_with(|| {
                BrowsePasteRetryPlan::from_plan(tui_file_picker::PastePlan {
                    mode: if record.is_move {
                        tui_file_picker::FilePickerClipboardMode::Cut
                    } else {
                        tui_file_picker::FilePickerClipboardMode::Copy
                    },
                    mappings: record.mappings.clone(),
                })
            });
            retry.recovery_by_source.insert(source, proof);
        }
        DurableFileTaskJournalMutation::Checkpoint {
            lifecycle,
            status,
            roots,
            retry_plan,
            cleared_native_sources,
        } => {
            merge_root_results(&mut record.roots, &roots);
            record.native_rename_intents.retain(|intent| {
                !cleared_native_sources
                    .iter()
                    .any(|source| source == &intent.source)
            });
            if let Some(retry_plan) = retry_plan {
                record.retry_plan = Some(retry_plan);
            }
            let unresolved = !record.pending_mappings().is_empty()
                || !record.temp_artifacts.is_empty()
                || !record.quarantine_artifacts.is_empty()
                || !record.native_rename_intents.is_empty();
            record.lifecycle = if unresolved
                && matches!(
                    lifecycle,
                    DurableFileTaskLifecycle::Completed | DurableFileTaskLifecycle::Reconciled
                )
            {
                DurableFileTaskLifecycle::AwaitingReconciliation
            } else {
                lifecycle
            };
            record.last_status = Some(status);
            if !unresolved {
                record.retry_plan = None;
            }
        }
    }
    Ok(())
}


#[derive(Debug, Clone)]
pub struct LegacyFileTaskJournalInventoryEntry {
    pub journal_path: PathBuf,
    pub record: Option<DurableFileTaskRecord>,
    pub terminal_clean: bool,
    pub classification_error: Option<String>,
}

/// Scan only the top-level v0.4.8 namespace. Protocol-v2 journals below `v2/`
/// remain invisible to old binaries and are intentionally excluded here.
pub fn legacy_journal_inventory() -> Vec<LegacyFileTaskJournalInventoryEntry> {
    let root = file_task_journal_base_dir();
    let Ok(entries) = fs::read_dir(&root) else { return Vec::new() };
    let mut inventory = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.parent() == Some(root.as_path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|journal_path| match load_record(&journal_path) {
            Ok(record) => {
                let terminal_clean = !record.needs_reconciliation();
                Some(LegacyFileTaskJournalInventoryEntry {
                    journal_path,
                    record: Some(record),
                    terminal_clean,
                    classification_error: None,
                })
            }
            Err(error) => {
                // An unreadable top-level journal is ambiguous, not terminal.
                log::warn!("cannot classify legacy file-operation journal {}: {error}", journal_path.display());
                Some(LegacyFileTaskJournalInventoryEntry {
                    journal_path,
                    record: None,
                    terminal_clean: false,
                    classification_error: Some(error),
                })
            }
        })
        .collect::<Vec<_>>();
    inventory.sort_by(|left, right| left.journal_path.cmp(&right.journal_path));
    inventory
}

pub fn nonterminal_legacy_journals() -> Vec<LegacyFileTaskJournalInventoryEntry> {
    legacy_journal_inventory()
        .into_iter()
        .filter(|entry| !entry.terminal_clean)
        .collect()
}

/// Explicitly convert one ownerless v0.4.8 nonterminal journal into the v2
/// recovery model. This never resumes work. The new final-family descriptor and
/// v2 snapshot are durable before the top-level legacy file leaves the old
/// scanner namespace; dropping the local holder then leaves RecoveryReserved
/// authority for the ordinary explicit recovery review.
pub fn adopt_legacy_journal(journal_path: &Path) -> Result<PathBuf, String> {
    // Invoking the dedicated adoption API/CLI is itself the explicit operator
    // action. Safety comes from proving that no observable peer can still own
    // v0.4.8 mutation state, not from a second environment-variable ritual.
    let peer_signals = crate::db::observable_tonepoet_peer_processes();
    if !peer_signals.is_empty() {
        return Err(format!(
            "legacy file-operation adoption is blocked while another tonepoet process is observable: {}",
            peer_signals.join("; ")
        ));
    }
    let root = file_task_journal_base_dir();
    let parent = journal_path.parent().ok_or_else(|| "legacy journal has no parent".to_string())?;
    if parent != root || journal_path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return Err(format!(
            "legacy adoption accepts only top-level file-operation journals under {}",
            root.display()
        ));
    }
    let mut record = load_record(journal_path)?;
    if !record.needs_reconciliation() {
        return Err(format!(
            "legacy journal {} is terminal-clean and does not require adoption",
            journal_path.display()
        ));
    }

    // Idempotent retry after a v2 snapshot became durable but before the legacy
    // rename completed: locate the snapshot by its diagnostic origin link.
    if let Ok(entries) = fs::read_dir(file_task_journal_dir()) {
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") { continue; }
            if let Ok(existing) = load_record(&path) {
                if existing.legacy_journal_path.as_deref() == Some(journal_path) {
                    archive_adopted_legacy_journal(journal_path)?;
                    return Ok(path);
                }
            }
        }
    }

    let legacy_job_id = record.job_id.clone();
    let job_id = uuid::Uuid::new_v4();
    let FileTaskPathAdmission {
        claims,
        admitted_mappings,
    } = file_task_path_admission(record.is_move, &record.mappings)?;
    let admitted_destination_root = file_task_destination_root_from_job(&record.job, &record.mappings)?;
    let guard = MutationClaimGuard::acquire_legacy_journal_adoption(
        LeaseFamily::JournalOperation { job_id },
        claims.clone(),
        journal_path,
    )?;
    let descriptor_path = guard.lease().descriptor_path().to_path_buf();
    let v2_root = file_task_journal_dir();
    fs::create_dir_all(&v2_root)
        .map_err(|error| format!("create protocol-v2 journal directory {}: {error}", v2_root.display()))?;
    let v2_path = v2_root.join(format!("{job_id}.jsonl"));
    record.schema = FILE_TASK_JOURNAL_SCHEMA;
    record.job_id = job_id.to_string();
    record.origin_owner = Some(OwnerProcessIdentity::current());
    record.lease_descriptor = Some(descriptor_path);
    record.path_claims = claims;
    record.admitted_mappings = admitted_mappings;
    record.admitted_destination_root = Some(admitted_destination_root);
    record.legacy_journal_path = Some(journal_path.to_path_buf());
    record.legacy_job_id = Some(legacy_job_id);
    record.updated_unix_ms = unix_ms();
    create_record(&v2_path, &record)?;
    archive_adopted_legacy_journal(journal_path)?;
    drop(guard);
    Ok(v2_path)
}

fn archive_adopted_legacy_journal(journal_path: &Path) -> Result<(), String> {
    if !journal_path.exists() { return Ok(()) }
    let root = file_task_journal_base_dir();
    let archive = root.join("legacy-adopted");
    fs::create_dir_all(&archive)
        .map_err(|error| format!("create legacy adoption archive {}: {error}", archive.display()))?;
    let name = journal_path.file_name().ok_or_else(|| "legacy journal has no filename".to_string())?;
    let mut destination = archive.join(name);
    if destination.exists() {
        destination = archive.join(format!("{}-{}", unix_ms(), name.to_string_lossy()));
    }
    fs::rename(journal_path, &destination).map_err(|error| format!(
        "move adopted legacy journal {} out of v0.4.8 scanner namespace: {error}",
        journal_path.display()
    ))?;
    sync_directory_best_effort(&root);
    sync_directory_best_effort(&archive);
    Ok(())
}

pub fn cleanup_setup_orphan_journal_descriptors() {
    let probe = LeaseFamily::JournalOperation { job_id: uuid::Uuid::nil() };
    let Ok(hints) = crate::concurrency::lifecycle_descriptor_hints(&probe) else { return };
    let journal_root = file_task_journal_dir();
    for (job_id, descriptor) in hints {
        let journal = journal_root.join(format!("{job_id}.jsonl"));
        if journal.exists() { continue; }
        let family = LeaseFamily::JournalOperation { job_id };
        // Absence of the final journal pathname proves only setup never committed;
        // live holders still veto retirement inside the lifecycle helper.
        let _ = crate::concurrency::retire_setup_orphan_by_path_identity(&descriptor, &family);
    }
}

fn recovery_record_is_discarding(record: &DurableFileTaskRecord) -> bool {
    record.lifecycle == DurableFileTaskLifecycle::AwaitingReconciliation
        && record.last_status.as_deref() == Some(RECOVERY_DISCARDING_MARKER)
}

fn recovery_record_is_discarded(record: &DurableFileTaskRecord) -> bool {
    record.lifecycle == DurableFileTaskLifecycle::Reconciled
        && record.last_status.as_deref() == Some(RECOVERY_DISCARDED_MARKER)
}

pub(super) fn recovery_record_is_discard_cleanup(record: &DurableFileTaskRecord) -> bool {
    recovery_record_is_discarding(record) || recovery_record_is_discarded(record)
}

fn recovery_descriptor_path(
    path: &Path,
    record: &DurableFileTaskRecord,
) -> Result<Option<PathBuf>, String> {
    let job_id = uuid::Uuid::parse_str(&record.job_id).map_err(|error| {
        format!(
            "file-operation record {} has an invalid job identifier: {error}",
            path.display()
        )
    })?;
    let family = LeaseFamily::JournalOperation { job_id };
    if let Some(descriptor) = record
        .lease_descriptor
        .clone()
        .or_else(|| crate::concurrency::find_family_descriptor(&family).ok().flatten())
    {
        return Ok(Some(descriptor));
    }

    if recovery_record_is_discarded(record) {
        // Discard already completed its filesystem work. A prior cleanup may
        // have retired the descriptor before a crash left the clean marker.
        return Ok(None);
    }

    let current = OwnerProcessIdentity::current();
    let prior_boot = record
        .origin_owner
        .map(|owner| {
            owner.boot_id_hash != 0
                && current.boot_id_hash != 0
                && owner.boot_id_hash != current.boot_id_hash
        })
        .unwrap_or(false);
    if prior_boot && !record.path_claims.is_empty() {
        let guard = MutationClaimGuard::acquire(family, record.path_claims.clone()).map_err(|error| {
            format!(
                "could not reconstruct the prior-session recovery reservation for {}: {error}",
                path.display()
            )
        })?;
        let descriptor = guard.lease().descriptor_path().to_path_buf();
        drop(guard); // final-family descriptor remains RecoveryReserved
        return Ok(Some(descriptor));
    }

    Err(format!(
        "file-operation record {} has no recoverable ownership descriptor",
        path.display()
    ))
}

fn classify_recovery_surface(
    path: &Path,
    record: &DurableFileTaskRecord,
) -> Result<RecoverySurfaceAvailability, String> {
    let Some(descriptor) = recovery_descriptor_path(path, record)? else {
        return Ok(RecoverySurfaceAvailability::Recoverable);
    };
    let availability = if permits_same_process_recovery_handoff(path, record) {
        crate::concurrency::descriptor_recovery_availability_with_local_handoff(&descriptor)
    } else {
        crate::concurrency::descriptor_availability(&descriptor)
    }?;
    match availability.1 {
        crate::concurrency::ClaimAvailability::Live => Ok(RecoverySurfaceAvailability::Live),
        crate::concurrency::ClaimAvailability::RecoveryReserved => {
            Ok(RecoverySurfaceAvailability::Recoverable)
        }
        crate::concurrency::ClaimAvailability::ReclaimableEphemeral => Err(format!(
            "file-operation record {} references a non-durable ownership family",
            path.display()
        )),
    }
}

/// Return every unresolved copy/move record that needs a recovery surface.
/// Unlike `pending_journals`, this includes live and unreadable records so the
/// UI can explain why no automatic action is currently offered.
pub fn recovery_surface_records() -> Vec<RecoverySurfaceRecord> {
    cleanup_setup_orphan_journal_descriptors();
    cleanup_discarded_recovery_records();
    let root = file_task_journal_dir();
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut records = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|path| match load_record_with_version(&path) {
            Ok((record, version))
                if record.needs_reconciliation() || recovery_record_is_discarded(&record) =>
            {
                match classify_recovery_surface(&path, &record) {
                    Ok(availability) => Some(RecoverySurfaceRecord {
                        journal_path: path,
                        record: Some(record),
                        version: Some(version),
                        availability,
                        diagnostic: None,
                    }),
                    Err(error) => Some(RecoverySurfaceRecord {
                        journal_path: path,
                        record: Some(record),
                        version: Some(version),
                        availability: RecoverySurfaceAvailability::Unreadable,
                        diagnostic: Some(error),
                    }),
                }
            }
            Ok(_) => None,
            Err(error) => Some(RecoverySurfaceRecord {
                journal_path: path,
                record: None,
                version: None,
                availability: RecoverySurfaceAvailability::Unreadable,
                diagnostic: Some(error),
            }),
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        let left_key = left
            .record
            .as_ref()
            .map(|record| (0u8, record.created_unix_ms, record.updated_unix_ms))
            .unwrap_or((1u8, u64::MAX, u64::MAX));
        let right_key = right
            .record
            .as_ref()
            .map(|record| (0u8, record.created_unix_ms, record.updated_unix_ms))
            .unwrap_or((1u8, u64::MAX, u64::MAX));
        left_key
            .cmp(&right_key)
            .then_with(|| left.journal_path.cmp(&right.journal_path))
    });
    records
}

fn pending_journals_from_surfaces(
    surfaces: &[RecoverySurfaceRecord],
) -> Vec<(PathBuf, DurableFileTaskRecord)> {
    let mut pending = surfaces
        .iter()
        .filter(|surface| surface.availability == RecoverySurfaceAvailability::Recoverable)
        .filter_map(|surface| {
            let record = surface.record.as_ref()?;
            (record.needs_reconciliation() && !recovery_record_is_discard_cleanup(record))
                .then_some((surface.journal_path.clone(), record.clone()))
        })
        .collect::<Vec<_>>();
    // Preserve the existing executable-recovery ordering: the historical
    // scanner sorted by last update, while the new presentation surface is
    // deliberately sorted by creation time (oldest operation first).
    pending.sort_by_key(|(_, record)| record.updated_unix_ms);
    pending
}

pub fn pending_journals() -> Vec<(PathBuf, DurableFileTaskRecord)> {
    let surfaces = recovery_surface_records();
    pending_journals_from_surfaces(&surfaces)
}

pub enum RecoveryDiscardPreparation {
    Ready {
        handle: FileTaskJournalHandle,
        record: DurableFileTaskRecord,
    },
    CleanupOnly {
        released_paths: usize,
    },
}

pub fn prepare_recovery_discard(
    path: &Path,
    expected_version: RecoveryRecordVersion,
) -> Result<RecoveryDiscardPreparation, String> {
    let (record, version) = load_record_with_version(path)?;
    if version != expected_version {
        return Err("The interrupted operation changed after it was reviewed; review it again before discarding.".to_string());
    }
    if recovery_record_is_discarded(&record) {
        let released_paths = recovery_blocked_path_count(&record);
        finish_discarded_recovery_retirement(path, &record)?;
        return Ok(RecoveryDiscardPreparation::CleanupOnly { released_paths });
    }
    if !record.needs_reconciliation() {
        return Err("This copy or move no longer needs recovery.".to_string());
    }
    let job_id = uuid::Uuid::parse_str(&record.job_id)
        .map_err(|error| format!("invalid interrupted-operation identifier: {error}"))?;
    let family = LeaseFamily::JournalOperation { job_id };
    let descriptor = recovery_descriptor_path(path, &record)?
        .ok_or_else(|| "The interrupted operation no longer has a recovery reservation.".to_string())?;
    let recovery = JournalRecoveryLease::acquire(
        &descriptor,
        &family,
        permits_same_process_recovery_handoff(path, &record),
    )?;

    // The durable lease prevents another Tonepoet mutation from changing the
    // record while discard owns it. Re-read the exact committed revision only
    // after acquiring that lease so a screen snapshot can never authorize a
    // later journal state.
    let (record, locked_version) = load_record_with_version(path)?;
    if locked_version != expected_version {
        return Err("The interrupted operation changed after it was reviewed; review it again before discarding.".to_string());
    }
    if recovery_record_is_discarded(&record) {
        drop(recovery);
        let released_paths = recovery_blocked_path_count(&record);
        finish_discarded_recovery_retirement(path, &record)?;
        return Ok(RecoveryDiscardPreparation::CleanupOnly { released_paths });
    }
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let handle = FileTaskJournalHandle {
        path: path.to_path_buf(),
        abandon_marker: abandon_marker_path(root, &record.job_id, record.generation),
        job_id: record.job_id.clone(),
        generation: record.generation,
        lease: Some(recovery.holder()),
    };
    // Persist the reviewed destructive choice before the first filesystem
    // mutation. If Tonepoet exits during cleanup, startup must resume cleanup
    // rather than ever presenting this record as resumable transfer work.
    if !recovery_record_is_discarding(&record) {
        handle.mark_recovery_discard_started()?;
    }
    let record = handle.load()?;
    Ok(RecoveryDiscardPreparation::Ready { handle, record })
}

pub fn commit_recovery_discard(handle: FileTaskJournalHandle) -> Result<(), String> {
    let before = handle.load()?;
    if !before.temp_artifacts.is_empty()
        || !before.quarantine_artifacts.is_empty()
        || !before.native_rename_intents.is_empty()
    {
        return Err("Discard cleanup is incomplete; the recovery record was kept for another review.".to_string());
    }
    handle.mark_recovery_discarded()?;
    let path = handle.path.clone();
    let record = handle.load()?;
    drop(handle);
    finish_discarded_recovery_retirement(&path, &record)
}

fn finish_discarded_recovery_retirement(
    path: &Path,
    record: &DurableFileTaskRecord,
) -> Result<(), String> {
    if !recovery_record_is_discarded(record)
        || !record.temp_artifacts.is_empty()
        || !record.quarantine_artifacts.is_empty()
        || !record.native_rename_intents.is_empty()
    {
        return Err("refused to retire an interrupted-operation record before discard cleanup was durable".to_string());
    }
    let job_id = uuid::Uuid::parse_str(&record.job_id)
        .map_err(|error| format!("invalid interrupted-operation identifier: {error}"))?;
    let family = LeaseFamily::JournalOperation { job_id };
    let descriptor = record
        .lease_descriptor
        .clone()
        .or_else(|| crate::concurrency::find_family_descriptor(&family).ok().flatten());
    if let Some(descriptor) = descriptor {
        match fs::symlink_metadata(&descriptor) {
            Ok(_) => crate::concurrency::retire_descriptor_after_lifecycle_release(
                &descriptor,
                &family,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect interrupted-operation reservation {}: {error}",
                    descriptor.display()
                ))
            }
        }
    }
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|error| {
                        format!(
                            "sync interrupted-operation record directory {}: {error}",
                            parent.display()
                        )
                    })?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "remove resolved interrupted-operation record {}: {error}",
                path.display()
            ))
        }
    }
    Ok(())
}

fn cleanup_discarded_recovery_records() {
    let root = file_task_journal_dir();
    let Ok(entries) = fs::read_dir(&root) else { return };
    for path in entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
    {
        let Ok(record) = load_record(&path) else { continue };
        if recovery_record_is_discarded(&record) {
            if let Err(error) = finish_discarded_recovery_retirement(&path, &record) {
                log::debug!(
                    "resolved interrupted-operation cleanup remains pending for {}: {error}",
                    path.display()
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StartupFileTaskRecovery {
    pub journal_path: PathBuf,
    pub total_pending_jobs: usize,
    pub clipboard: tui_file_picker::FilesystemClipboard,
    pub retry_plan: BrowsePasteRetryPlan,
    pub destination_dir: Option<PathBuf>,
    pub temp_artifact_count: usize,
    pub quarantine_artifact_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct StartupFileTaskRecoveryInventory {
    pub total_pending_jobs: usize,
    pub recoveries: Vec<StartupFileTaskRecovery>,
    /// Immutable presentation snapshots captured by the same control-plane
    /// scan used to reconstruct the executable recovery queue. Reusing these
    /// avoids a second startup read/hash pass over every saved operation.
    pub recovery_surface_records: Vec<RecoverySurfaceRecord>,
    /// Valid, reconciliation-required journals that could not be represented as
    /// an executable recovery snapshot. They remain on disk and are surfaced to
    /// the user rather than being silently omitted.
    pub unreconstructable_journals: Vec<PathBuf>,
}

fn mark_recovery_surface_unreadable(
    surfaces: &mut [RecoverySurfaceRecord],
    journal_path: &Path,
    diagnostic: impl Into<String>,
) {
    if let Some(surface) = surfaces
        .iter_mut()
        .find(|surface| surface.journal_path == journal_path)
    {
        surface.availability = RecoverySurfaceAvailability::Unreadable;
        surface.diagnostic = Some(diagnostic.into());
    }
}

/// Inspect every interrupted job without touching source/destination mounts.
///
/// Journals remain the authority. This function only reconstructs immutable
/// clipboard/plan snapshots; actual verification and cleanup still run inside
/// the cancellable helper process when each recovery is dispatched.
pub fn startup_file_task_recovery_inventory() -> StartupFileTaskRecoveryInventory {
    let recovery_surface_records = recovery_surface_records();
    let pending = pending_journals_from_surfaces(&recovery_surface_records);
    let total_pending_jobs = recovery_surface_records.len();
    let mut inventory = StartupFileTaskRecoveryInventory {
        total_pending_jobs,
        recovery_surface_records,
        ..StartupFileTaskRecoveryInventory::default()
    };

    for (journal_path, record) in pending {
        let pending_mappings = record.pending_mappings();
        let requested_mappings = if pending_mappings.is_empty() {
            record.mappings.clone()
        } else {
            pending_mappings
        };
        let mappings = record.mappings_for_reconciliation(&requested_mappings);
        if mappings.is_empty() {
            log::warn!(
                "pending file-operation journal {} has no reconstructable mappings; leaving it on disk for inspection",
                journal_path.display()
            );
            mark_recovery_surface_unreadable(
                &mut inventory.recovery_surface_records,
                &journal_path,
                "The saved operation has no file mappings that can be resumed safely.",
            );
            inventory.unreconstructable_journals.push(journal_path);
            continue;
        }
        let mode = if record.is_move {
            tui_file_picker::FilePickerClipboardMode::Cut
        } else {
            tui_file_picker::FilePickerClipboardMode::Copy
        };
        let sources = mappings
            .iter()
            .map(|mapping| mapping.source.clone())
            .collect::<Vec<_>>();
        let Some(clipboard) = tui_file_picker::FilesystemClipboard::new(mode, sources.clone()) else {
            log::warn!(
                "pending file-operation journal {} produced an empty clipboard snapshot; leaving it on disk for inspection",
                journal_path.display()
            );
            mark_recovery_surface_unreadable(
                &mut inventory.recovery_surface_records,
                &journal_path,
                "The saved operation has no source items that can be resumed safely.",
            );
            inventory.unreconstructable_journals.push(journal_path);
            continue;
        };
        let plan = tui_file_picker::PastePlan {
            mode,
            mappings: mappings.clone(),
        };
        let mut retry_plan = record
            .retry_plan
            .as_ref()
            .and_then(|retry| retry.retain_sources(&sources))
            .unwrap_or_else(|| BrowsePasteRetryPlan::from_plan(plan));
        retry_plan.recovery_journal_path = Some(journal_path.clone());
        let destination_dir = mappings
            .first()
            .and_then(|mapping| mapping.destination.parent())
            .map(Path::to_path_buf);
        if destination_dir.is_none() {
            log::warn!(
                "pending file-operation journal {} has no recoverable destination directory; leaving it on disk for inspection",
                journal_path.display()
            );
            mark_recovery_surface_unreadable(
                &mut inventory.recovery_surface_records,
                &journal_path,
                "The saved operation has no destination that can be resumed safely.",
            );
            inventory.unreconstructable_journals.push(journal_path);
            continue;
        }
        inventory.recoveries.push(StartupFileTaskRecovery {
            journal_path,
            total_pending_jobs,
            clipboard,
            retry_plan,
            destination_dir,
            temp_artifact_count: record.temp_artifacts.len(),
            quarantine_artifact_count: record.quarantine_artifacts.len(),
        });
    }

    inventory
}

/// Load every recoverable interrupted job in oldest-to-newest journal order.
pub fn startup_file_task_recoveries() -> Vec<StartupFileTaskRecovery> {
    startup_file_task_recovery_inventory().recoveries
}

/// Compatibility helper for callers that explicitly want the newest recovery.
pub fn startup_file_task_recovery() -> Option<StartupFileTaskRecovery> {
    startup_file_task_recoveries().pop()
}

#[cfg(test)]
pub(crate) fn test_environment_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct FileTaskPathAdmission {
    pub(crate) claims: Vec<PathClaim>,
    pub(crate) admitted_mappings: Vec<tui_file_picker::PasteMapping>,
}

pub(crate) fn file_task_path_admission(
    is_move: bool,
    mappings: &[tui_file_picker::PasteMapping],
) -> Result<FileTaskPathAdmission, String> {
    let mut claims = Vec::with_capacity(mappings.len() * 2);
    let mut admitted_mappings = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let (source_semantics, scope) = match std::fs::symlink_metadata(&mapping.source) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                (PathResolutionSemantics::NamespaceObject, ClaimScope::Exact)
            }
            Ok(metadata) if metadata.file_type().is_dir() => {
                (PathResolutionSemantics::FollowReferent, ClaimScope::Subtree)
            }
            Ok(_) => (PathResolutionSemantics::FollowReferent, ClaimScope::Exact),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (PathResolutionSemantics::FollowReferent, ClaimScope::Subtree)
            }
            Err(error) => {
                return Err(format!(
                    "inspect file-operation source {} before admission: {error}",
                    mapping.source.display()
                ))
            }
        };
        let source_claim = PathClaim::resolve_with_semantics(
            &mapping.source,
            if is_move { ClaimMode::Write } else { ClaimMode::Read },
            scope,
            source_semantics,
        )?;
        // Root destinations are namespace entries. Intermediate aliases are
        // followed and recorded as dependencies, while an existing final
        // symlink remains the entry the copy/move conflict engine can replace.
        let destination_claim = PathClaim::resolve_with_semantics(
            &mapping.destination,
            ClaimMode::Write,
            scope,
            PathResolutionSemantics::NamespaceObject,
        )?;
        admitted_mappings.push(tui_file_picker::PasteMapping {
            source: source_claim.identity.resolved_io_path.clone(),
            destination: destination_claim.identity.resolved_io_path.clone(),
        });
        claims.push(source_claim);
        claims.push(destination_claim);
    }
    Ok(FileTaskPathAdmission {
        claims,
        admitted_mappings,
    })
}

fn file_task_destination_root_from_job(
    job: &serde_json::Value,
    mappings: &[tui_file_picker::PasteMapping],
) -> Result<PathBuf, String> {
    let fallback = mappings
        .first()
        .and_then(|mapping| mapping.destination.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination = job
        .get("dest")
        .and_then(serde_json::Value::as_str)
        .map(Path::new)
        .unwrap_or(fallback);
    Ok(crate::concurrency::ResolvedPathIdentity::resolve(destination)?
        .resolved_io_path)
}

fn admitted_mappings_from_durable_claims(
    mappings: &[tui_file_picker::PasteMapping],
    claims: &[PathClaim],
) -> Result<Vec<tui_file_picker::PasteMapping>, String> {
    if claims.len() != mappings.len().saturating_mul(2) {
        return Err(
            "file-operation journal predates durable admitted mappings and its claim set cannot reconstruct them"
                .to_string(),
        );
    }
    let mut admitted = Vec::with_capacity(mappings.len());
    for (index, pair) in claims.chunks_exact(2).enumerate() {
        // Old protocol-v2 records that traversed an alias cannot prove whether
        // the final source component was a symlink object or a followed
        // referent. Refuse to manufacture a new meaning during recovery.
        if pair.iter().any(|claim| {
            claim.identity.namespace_path != claim.identity.resolved_io_path
                && claim.identity.namespace_dependencies.is_empty()
        }) {
            return Err(format!(
                "file-operation journal mapping {} predates durable alias-dependency/admitted-path semantics; explicit recovery cannot safely reinterpret its symlink binding",
                index + 1
            ));
        }
        admitted.push(tui_file_picker::PasteMapping {
            source: pair[0].identity.resolved_io_path.clone(),
            destination: pair[1].identity.resolved_io_path.clone(),
        });
    }
    Ok(admitted)
}

fn admitted_destination_root_from_mappings(
    job: &serde_json::Value,
    logical: &[tui_file_picker::PasteMapping],
    admitted: &[tui_file_picker::PasteMapping],
) -> Result<PathBuf, String> {
    let original_root = PathBuf::from(
        job.get("dest")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "serialized file-task job has no destination root".to_string())?,
    );
    for (logical_mapping, admitted_mapping) in logical.iter().zip(admitted) {
        if let Ok(relative) = logical_mapping.destination.strip_prefix(&original_root) {
            let mut root = admitted_mapping.destination.clone();
            for _ in relative.components() {
                if !root.pop() {
                    break;
                }
            }
            return Ok(root);
        }
    }
    Err("file-operation journal cannot reconstruct its admitted destination root".to_string())
}

fn ensure_durable_admitted_file_task_state(
    record: &mut DurableFileTaskRecord,
) -> Result<(), String> {
    if record.admitted_mappings.is_empty() {
        record.admitted_mappings =
            admitted_mappings_from_durable_claims(&record.mappings, &record.path_claims)?;
    }
    if record.admitted_mappings.len() != record.mappings.len() {
        return Err(
            "file-operation journal has a partial durable admitted mapping set".to_string(),
        );
    }
    if record.admitted_destination_root.is_none() {
        record.admitted_destination_root = Some(admitted_destination_root_from_mappings(
            &record.job,
            &record.mappings,
            &record.admitted_mappings,
        )?);
    }
    Ok(())
}

pub fn file_task_journal_base_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("TONEPOET_FILE_OPERATION_JOURNAL_DIR") {
        PathBuf::from(path)
    } else {
        crate::config::TonepoetConfig::config_path()
            .parent()
            .map(|parent| parent.join("file-operation-journal"))
            .unwrap_or_else(|| PathBuf::from(".tonepoet-file-operation-journal"))
    }
}

pub fn file_task_journal_dir() -> PathBuf {
    // v0.4.8 scans only top-level *.jsonl. Protocol-v2 journals live below a
    // subdirectory so an older concurrently-running binary cannot seize them.
    file_task_journal_base_dir().join("v2")
}

fn abandon_marker_path(root: &Path, job_id: &str, generation: u64) -> PathBuf {
    root.join(format!("{job_id}-{generation}.abandoned"))
}

fn create_record(path: &Path, record: &DurableFileTaskRecord) -> Result<(), String> {
    let mut file = create_private_journal_file(path)
        .map_err(|error| format!("create file-operation journal {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("lock file-operation journal {}: {error}", path.display()))?;
    let entry = DurableFileTaskJournalEntry::snapshot(record.clone());
    let result = append_journal_entry(&mut file, &entry, true);
    let _ = FileExt::unlock(&file);
    if result.is_ok() {
        if let Some(parent) = path.parent() {
            sync_directory_best_effort(parent);
        }
    }
    result
}

fn merge_resume_retry_plan(
    previous: Option<BrowsePasteRetryPlan>,
    supplied: Option<BrowsePasteRetryPlan>,
    mappings: &[tui_file_picker::PasteMapping],
) -> Option<BrowsePasteRetryPlan> {
    let sources = mappings
        .iter()
        .map(|mapping| mapping.source.clone())
        .collect::<Vec<_>>();
    match (previous, supplied) {
        (Some(previous), Some(supplied)) => {
            let mut merged = previous
                .retain_sources(&sources)
                .unwrap_or_else(|| supplied.clone());
            // The caller's plan is the exact generation subset selected by the
            // UI. Durable proofs from the prior generation remain authoritative,
            // while any newer supplied proof replaces the same source entry.
            merged.plan = supplied.plan;
            merged.recovery_by_source.extend(supplied.recovery_by_source);
            if supplied.recovery_journal_path.is_some() {
                merged.recovery_journal_path = supplied.recovery_journal_path;
            }
            Some(merged)
        }
        (Some(previous), None) => previous.retain_sources(&sources),
        (None, supplied) => supplied,
    }
}

fn resume_record_locked(
    _recovery: &JournalRecoveryLease,
    path: &Path,
    generation: u64,
    session_id: u64,
    is_move: bool,
    verification: tui_file_picker::VerificationMode,
    stall_timeout_secs: u64,
    mappings: Vec<tui_file_picker::PasteMapping>,
    retry_plan: Option<BrowsePasteRetryPlan>,
    job: serde_json::Value,
) -> Result<DurableFileTaskRecord, String> {
    let mut file = OpenOptions::new()
        .append(true)
        .read(true)
        .open(path)
        .map_err(|error| format!("open file-operation journal {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("lock file-operation journal {}: {error}", path.display()))?;
    let result = (|| {
        let (mut previous, valid_len) = scan_record_file(&mut file, path)?;
        repair_torn_tail(&mut file, path, valid_len)?;
        ensure_durable_admitted_file_task_state(&mut previous)?;
        if generation <= previous.generation {
            return Err(format!(
                "file-operation journal generation must increase (previous {}, requested {})",
                previous.generation, generation
            ));
        }
        if previous.is_move != is_move {
            return Err("file-operation journal kind changed during resume".to_string());
        }
        if !mappings
            .iter()
            .all(|mapping| previous.mappings.iter().any(|known| known == mapping))
        {
            return Err(
                "file-operation journal resume mappings are not an exact retained subset"
                    .to_string(),
            );
        }
        if !previous.artifact_generations.contains(&generation) {
            previous.artifact_generations.push(generation);
        }
        if previous.mappings.is_empty() {
            previous.mappings = mappings.clone();
        }
        previous.schema = FILE_TASK_JOURNAL_SCHEMA;
        previous.generation = generation;
        previous.session_id = session_id;
        previous.updated_unix_ms = unix_ms();
        previous.lifecycle = DurableFileTaskLifecycle::Planned;
        previous.verification = verification;
        previous.stall_timeout_secs = stall_timeout_secs.max(1);
        previous.job = job;
        previous.retry_plan = merge_resume_retry_plan(
            previous.retry_plan.take(),
            retry_plan,
            &mappings,
        );
        previous.last_status = Some("planned reconciliation generation".to_string());
        previous.abandoned_reason = None;
        let entry = DurableFileTaskJournalEntry::snapshot(previous.clone());
        append_journal_entry(&mut file, &entry, true)?;
        Ok(previous)
    })();
    let _ = FileExt::unlock(&file);
    result
}

fn append_entry_locked(
    path: &Path,
    expected_job_id: &str,
    expected_generation: u64,
    durable: bool,
    skip: impl FnOnce() -> bool,
    entry: &DurableFileTaskJournalEntry,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .append(true)
        .read(true)
        .open(path)
        .map_err(|error| format!("open file-operation journal {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("lock file-operation journal {}: {error}", path.display()))?;
    let result = (|| {
        let (job_id, generation, valid_len) = latest_identity_and_valid_len(&mut file, path)?;
        repair_torn_tail(&mut file, path, valid_len)?;
        if job_id != expected_job_id || generation != expected_generation {
            return Err("file-operation journal generation changed".to_string());
        }
        // Re-check the generation-scoped abandon marker while holding the
        // journal lock. This closes the race where a helper passed an earlier
        // marker check immediately before the supervisor abandoned it.
        if skip() {
            return Err("file-operation generation was abandoned".to_string());
        }
        append_journal_entry(&mut file, entry, durable)
    })();
    let _ = FileExt::unlock(&file);
    result
}

fn latest_identity_and_valid_len(
    file: &mut File,
    path: &Path,
) -> Result<(String, u64, u64), String> {
    let actual_len = file
        .metadata()
        .map_err(|error| format!("inspect file-operation journal {}: {error}", path.display()))?
        .len();
    if let Some(line) = read_last_complete_line(file, actual_len)? {
        match decode_journal_line(&line) {
            Ok(DecodedJournalLine::Entry(entry))
                if entry.schema == FILE_TASK_JOURNAL_SCHEMA =>
            {
                return Ok((entry.job_id, entry.generation, actual_len));
            }
            Ok(DecodedJournalLine::LegacySnapshot(record))
                if record.schema == FILE_TASK_JOURNAL_SCHEMA =>
            {
                return Ok((record.job_id, record.generation, actual_len));
            }
            _ => {}
        }
    }
    let (record, valid_len) = scan_record_file(file, path)?;
    Ok((record.job_id, record.generation, valid_len))
}

fn read_last_complete_line(file: &mut File, len: u64) -> Result<Option<Vec<u8>>, String> {
    if len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(len - 1))
        .map_err(|error| format!("seek file-operation journal tail: {error}"))?;
    let mut final_byte = [0u8; 1];
    file.read_exact(&mut final_byte)
        .map_err(|error| format!("read file-operation journal tail: {error}"))?;
    if final_byte[0] != b'\n' {
        return Ok(None);
    }

    let end = len - 1;
    let mut cursor = end;
    const CHUNK: u64 = 8 * 1024;
    loop {
        let start = cursor.saturating_sub(CHUNK);
        let chunk_len = (cursor - start) as usize;
        let mut chunk = vec![0u8; chunk_len];
        file.seek(SeekFrom::Start(start))
            .map_err(|error| format!("seek file-operation journal line: {error}"))?;
        file.read_exact(&mut chunk)
            .map_err(|error| format!("read file-operation journal line: {error}"))?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            let line_start = start + index as u64 + 1;
            let mut line = vec![0u8; (end - line_start) as usize];
            file.seek(SeekFrom::Start(line_start))
                .map_err(|error| format!("seek file-operation journal last line: {error}"))?;
            file.read_exact(&mut line)
                .map_err(|error| format!("read file-operation journal last line: {error}"))?;
            return Ok((!line.iter().all(|byte| byte.is_ascii_whitespace())).then_some(line));
        }
        if start == 0 {
            let mut line = vec![0u8; end as usize];
            file.seek(SeekFrom::Start(0))
                .map_err(|error| format!("seek file-operation journal first line: {error}"))?;
            file.read_exact(&mut line)
                .map_err(|error| format!("read file-operation journal first line: {error}"))?;
            return Ok((!line.iter().all(|byte| byte.is_ascii_whitespace())).then_some(line));
        }
        cursor = start;
    }
}

fn repair_torn_tail(file: &mut File, path: &Path, valid_len: u64) -> Result<(), String> {
    let actual_len = file
        .metadata()
        .map_err(|error| {
            format!(
                "inspect file-operation journal {} before append: {error}",
                path.display()
            )
        })?
        .len();
    if valid_len < actual_len {
        file.set_len(valid_len).map_err(|error| {
            format!(
                "repair torn file-operation journal {} before append: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn merge_root_results(
    retained: &mut Vec<tui_file_picker::FileTaskRootResult>,
    updates: &[tui_file_picker::FileTaskRootResult],
) {
    for update in updates {
        if let Some(existing) = retained
            .iter_mut()
            .find(|existing| existing.source == update.source)
        {
            *existing = update.clone();
        } else {
            retained.push(update.clone());
        }
    }
}

fn append_journal_entry(
    file: &mut File,
    entry: &DurableFileTaskJournalEntry,
    durable: bool,
) -> Result<(), String> {
    file.seek(SeekFrom::End(0))
        .map_err(|error| format!("seek file-operation journal for append: {error}"))?;
    serde_json::to_writer(&mut *file, entry)
        .map_err(|error| format!("serialize file-operation journal: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("append file-operation journal: {error}"))?;
    file.flush()
        .map_err(|error| format!("flush file-operation journal: {error}"))?;
    if durable {
        file.sync_data()
            .map_err(|error| format!("sync file-operation journal: {error}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_journal_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_journal_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_journal_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_journal_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .open(path)
}

#[cfg(unix)]
fn create_private_marker_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_marker_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn unix_ms() -> u64 {
    // u64 milliseconds are sufficient for hundreds of millions of years and,
    // unlike u128, survive serde's flatten/tag Content buffering round-trip.
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn sync_directory_best_effort(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory_best_effort(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct JournalDirGuard {
        _concurrency: crate::concurrency::ScopedTestCoordinationRootGuard,
    }

    impl JournalDirGuard {
        fn install(path: &Path) -> Self {
            let concurrency = crate::concurrency::install_scoped_test_coordination_root(
                &path.join("claims"),
            );
            std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", path);
            Self {
                _concurrency: concurrency,
            }
        }
    }

    impl Drop for JournalDirGuard {
        fn drop(&mut self) {
            std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");
        }
    }

    fn sample_mapping() -> tui_file_picker::PasteMapping {
        tui_file_picker::PasteMapping {
            source: PathBuf::from("source"),
            destination: PathBuf::from("destination"),
        }
    }

    fn sample_retry_plan() -> BrowsePasteRetryPlan {
        BrowsePasteRetryPlan {
            plan: tui_file_picker::PastePlan {
                mode: tui_file_picker::FilePickerClipboardMode::Copy,
                mappings: vec![sample_mapping()],
            },
            recovery_by_source: std::collections::BTreeMap::new(),
            recovery_journal_path: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_task_admission_tracks_destination_alias_dependency_and_blocks_rebind() {
        use std::os::unix::fs::symlink;

        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let source = temp.path().join("source.flac");
        let real_a = temp.path().join("real-a");
        let real_b = temp.path().join("real-b");
        let alias = temp.path().join("alias");
        fs::write(&source, b"audio").unwrap();
        fs::create_dir_all(&real_a).unwrap();
        fs::create_dir_all(&real_b).unwrap();
        symlink(&real_a, &alias).unwrap();
        let mapping = tui_file_picker::PasteMapping {
            source: source.clone(),
            destination: alias.join("new.flac"),
        };

        let admission = file_task_path_admission(false, std::slice::from_ref(&mapping)).unwrap();
        assert_eq!(admission.admitted_mappings[0].destination, real_a.join("new.flac"));
        assert_eq!(
            admission.claims[1].identity.namespace_dependencies,
            vec![alias.clone()]
        );

        let guard = MutationClaimGuard::acquire_ephemeral(admission.claims).unwrap();
        let replacement = PathClaim::resolve_with_semantics(
            &alias,
            ClaimMode::Write,
            ClaimScope::Exact,
            PathResolutionSemantics::NamespaceObject,
        )
        .unwrap();
        let error = MutationClaimGuard::acquire_ephemeral(vec![replacement])
            .expect_err("protocol-aware alias replacement must lose admission");
        assert!(error.contains("live owner"));

        let disjoint = PathClaim::resolve(
            &real_b.join("other.flac"),
            ClaimMode::Write,
            ClaimScope::Exact,
        )
        .unwrap();
        let disjoint_guard = MutationClaimGuard::acquire_ephemeral(vec![disjoint]).unwrap();
        drop(disjoint_guard);
        drop(guard);
    }

    #[cfg(unix)]
    #[test]
    fn file_task_destination_admission_preserves_symlink_then_parent_dir_semantics() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("library");
        let real = temp.path().join("real");
        let album = real.join("Album");
        let real_other = real.join("Other");
        let lexical_other = library.join("Other");
        let source = temp.path().join("source.flac");
        fs::create_dir_all(&album).unwrap();
        fs::create_dir_all(&real_other).unwrap();
        fs::create_dir_all(&lexical_other).unwrap();
        fs::write(&source, b"audio").unwrap();
        let current = library.join("current");
        symlink(&album, &current).unwrap();

        let mapping = tui_file_picker::PasteMapping {
            source,
            destination: current.join("../Other/new.flac"),
        };
        let admission = file_task_path_admission(false, std::slice::from_ref(&mapping)).unwrap();
        assert_eq!(
            admission.admitted_mappings[0].destination,
            real_other.join("new.flac")
        );
        assert_ne!(
            admission.admitted_mappings[0].destination,
            lexical_other.join("new.flac")
        );
        assert_eq!(
            admission.claims[1].identity.namespace_dependencies,
            vec![current]
        );

        fs::write(&admission.admitted_mappings[0].destination, b"copied").unwrap();
        assert_eq!(fs::read(real_other.join("new.flac")).unwrap(), b"copied");
        assert!(!lexical_other.join("new.flac").exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_task_symlink_source_admission_protects_namespace_object_for_copy_and_move() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let referent = temp.path().join("referent.flac");
        let source_link = temp.path().join("source-link.flac");
        fs::write(&referent, b"audio").unwrap();
        symlink(&referent, &source_link).unwrap();
        let mapping = tui_file_picker::PasteMapping {
            source: source_link.clone(),
            destination: temp.path().join("copy-link.flac"),
        };

        for is_move in [false, true] {
            let admission = file_task_path_admission(is_move, std::slice::from_ref(&mapping)).unwrap();
            let source_claim = &admission.claims[0];
            assert_eq!(source_claim.scope, ClaimScope::Exact);
            assert_eq!(
                source_claim.mode,
                if is_move { ClaimMode::Write } else { ClaimMode::Read }
            );
            assert_eq!(admission.admitted_mappings[0].source, source_link);
            assert_ne!(admission.admitted_mappings[0].source, referent.canonicalize().unwrap());
            assert!(fs::symlink_metadata(&admission.admitted_mappings[0].source)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn resumed_file_task_reuses_durable_admitted_destination_after_alias_rebind() {
        use std::os::unix::fs::symlink;

        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let source = temp.path().join("source.flac");
        let real_a = temp.path().join("real-a");
        let real_b = temp.path().join("real-b");
        let alias = temp.path().join("alias");
        fs::write(&source, b"audio").unwrap();
        fs::create_dir_all(&real_a).unwrap();
        fs::create_dir_all(&real_b).unwrap();
        symlink(&real_a, &alias).unwrap();
        let mapping = tui_file_picker::PasteMapping {
            source,
            destination: alias.join("new.flac"),
        };
        let job_id = uuid::Uuid::new_v4().to_string();
        let job = serde_json::json!({"dest": alias.to_string_lossy()});
        let first = FileTaskJournalHandle::create(
            job_id,
            1,
            11,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![mapping.clone()],
            None,
            job.clone(),
        )
        .unwrap();
        let journal_path = first.path().to_path_buf();
        let first_record = first.load().unwrap();
        assert_eq!(first_record.admitted_mappings[0].destination, real_a.join("new.flac"));
        drop(first);

        fs::remove_file(&alias).unwrap();
        symlink(&real_b, &alias).unwrap();

        let resumed = FileTaskJournalHandle::resume(
            journal_path,
            2,
            12,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![mapping],
            None,
            job,
        )
        .unwrap();
        let resumed_record = resumed.load().unwrap();
        assert_eq!(resumed_record.admitted_mappings[0].destination, real_a.join("new.flac"));
        assert_eq!(resumed_record.admitted_destination_root.as_deref(), Some(real_a.as_path()));
    }

    #[test]
    fn legacy_unix_device_endpoint_identity_remains_readable() {
        let value = serde_json::json!({
            "kind": "unix_device",
            "device": 42,
        });
        let identity: DurableEndpointVolumeIdentity =
            serde_json::from_value(value).expect("legacy Unix endpoint identity");
        assert_eq!(
            identity,
            DurableEndpointVolumeIdentity::UnixDevice { device: 42 }
        );
    }

    #[test]
    fn stable_unix_mount_endpoint_identity_round_trips() {
        let identity = DurableEndpointVolumeIdentity::UnixMount {
            transient_device: 7,
            mount_point: PathBuf::from("/run/media/user/Drive"),
            stable: Some(DurableUnixEndpointStableIdentity::FilesystemUuid {
                uuid: "11111111-2222-3333-4444-555555555555".to_string(),
                filesystem_type: "ext4".to_string(),
                mount_root: PathBuf::from("/"),
            }),
        };
        let encoded = serde_json::to_value(&identity).expect("serialize stable endpoint identity");
        let decoded: DurableEndpointVolumeIdentity =
            serde_json::from_value(encoded).expect("deserialize stable endpoint identity");
        assert_eq!(decoded, identity);
    }

    #[test]
    fn macos_volume_uuid_endpoint_identity_round_trips() {
        let identity = DurableEndpointVolumeIdentity::UnixMount {
            transient_device: 1_000_042,
            mount_point: PathBuf::from("/Volumes/Studio Drive"),
            stable: Some(DurableUnixEndpointStableIdentity::FilesystemUuid {
                uuid: "12345678-90ab-cdef-1234-567890abcdef".to_string(),
                filesystem_type: "apfs".to_string(),
                mount_root: PathBuf::from("/"),
            }),
        };
        let encoded = serde_json::to_string(&identity)
            .expect("serialize macOS volume UUID endpoint identity");
        let decoded: DurableEndpointVolumeIdentity = serde_json::from_str(&encoded)
            .expect("deserialize macOS volume UUID endpoint identity");
        assert_eq!(decoded, identity);
    }

    #[test]
    fn journal_ignores_torn_final_append() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            7,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![sample_mapping()],
            None,
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        handle
            .mark_lifecycle(DurableFileTaskLifecycle::Running, "running")
            .expect("checkpoint");
        let mut file = OpenOptions::new()
            .append(true)
            .open(handle.path())
            .expect("open journal");
        file.write_all(b"{\"schema\":1").expect("torn append");
        file.sync_all().expect("sync torn append");

        let record = load_record(handle.path()).expect("load valid prefix");
        assert_eq!(record.lifecycle, DurableFileTaskLifecycle::Running);
    }

    #[test]
    fn next_checkpoint_repairs_a_torn_final_append() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            8,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![sample_mapping()],
            None,
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        let mut file = OpenOptions::new()
            .append(true)
            .open(handle.path())
            .expect("open journal");
        file.write_all(b"{\"schema\":1").expect("torn append");
        file.sync_all().expect("sync torn append");
        drop(file);

        handle
            .mark_lifecycle(DurableFileTaskLifecycle::Running, "repaired checkpoint")
            .expect("repair and checkpoint");
        let record = load_record(handle.path()).expect("load repaired journal");
        assert_eq!(record.lifecycle, DurableFileTaskLifecycle::Running);
        assert_eq!(record.last_status.as_deref(), Some("repaired checkpoint"));
    }

    #[test]
    fn parseable_but_unterminated_tail_is_not_committed() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            81,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![sample_mapping()],
            None,
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        handle
            .mark_lifecycle(DurableFileTaskLifecycle::Running, "running")
            .expect("checkpoint");

        let entry = DurableFileTaskJournalEntry::new(
            handle.job_id(),
            handle.generation(),
            DurableFileTaskJournalMutation::Lifecycle {
                lifecycle: DurableFileTaskLifecycle::Paused,
                status: "unterminated but parseable".to_string(),
                abandoned_reason: None,
            },
        );
        let bytes = serde_json::to_vec(&entry).expect("serialize entry");
        let mut file = OpenOptions::new()
            .append(true)
            .open(handle.path())
            .expect("open journal");
        file.write_all(&bytes).expect("write unterminated entry");
        file.sync_all().expect("sync unterminated entry");
        drop(file);

        let record = load_record(handle.path()).expect("load committed prefix");
        assert_eq!(record.lifecycle, DurableFileTaskLifecycle::Running);

        handle
            .mark_lifecycle(DurableFileTaskLifecycle::Stalled, "tail repaired")
            .expect("repair and append");
        let record = load_record(handle.path()).expect("load repaired journal");
        assert_eq!(record.lifecycle, DurableFileTaskLifecycle::Stalled);
        assert_eq!(record.last_status.as_deref(), Some("tail repaired"));
    }

    #[test]
    fn abandon_marker_blocks_late_checkpoints() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            9,
            true,
            tui_file_picker::VerificationMode::Standard,
            8,
            Vec::new(),
            None,
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        handle.mark_abandoned("test cancellation").expect("abandon");
        let error = handle
            .mark_lifecycle(DurableFileTaskLifecycle::Completed, "late completion")
            .expect_err("late checkpoint must be rejected");
        assert!(error.contains("abandoned"));
        let record = load_record(handle.path()).expect("load journal");
        assert_eq!(
            record.lifecycle,
            DurableFileTaskLifecycle::AwaitingReconciliation
        );
    }

    #[test]
    fn compact_deltas_do_not_repeat_the_complete_job_plan() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let mappings = (0..2_000)
            .map(|index| tui_file_picker::PasteMapping {
                source: PathBuf::from(format!("source-{index}")),
                destination: PathBuf::from(format!("destination-{index}")),
            })
            .collect::<Vec<_>>();
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            10,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            mappings,
            None,
            serde_json::json!({"large": "plan"}),
        )
        .expect("create journal");
        let initial_len = std::fs::metadata(handle.path()).expect("initial metadata").len();
        for index in 0..100 {
            handle
                .mark_lifecycle(
                    DurableFileTaskLifecycle::Running,
                    format!("checkpoint {index}"),
                )
                .expect("append compact lifecycle delta");
        }
        let final_len = std::fs::metadata(handle.path()).expect("final metadata").len();
        assert!(
            final_len < initial_len.saturating_mul(2),
            "100 lifecycle checkpoints must not duplicate the complete 2,000-root plan"
        );
    }

    #[test]
    fn move_recovery_proof_delta_survives_reload() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let mapping = sample_mapping();
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            17,
            true,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![mapping.clone()],
            Some(BrowsePasteRetryPlan::from_plan(tui_file_picker::PastePlan {
                mode: tui_file_picker::FilePickerClipboardMode::Cut,
                mappings: vec![mapping.clone()],
            })),
            serde_json::json!({"test": true}),
        )
        .expect("create move journal");
        let proof = BrowseMoveRecoveryProof {
            source_manifest: tui_file_picker::SourceManifest::default(),
            destination_manifest: tui_file_picker::DestinationManifest::default(),
        };
        handle
            .record_move_recovery_proof(&mapping.source, &proof)
            .expect("persist move proof");

        let record = handle.load().expect("reload move proof");
        assert_eq!(
            record
                .retry_plan
                .as_ref()
                .and_then(|retry| retry.recovery_by_source.get(&mapping.source)),
            Some(&proof)
        );
    }

    #[test]
    fn resume_keeps_durable_move_proofs_when_the_ui_plan_is_stale() {
        let mapping = sample_mapping();
        let proof = BrowseMoveRecoveryProof {
            source_manifest: tui_file_picker::SourceManifest::default(),
            destination_manifest: tui_file_picker::DestinationManifest::default(),
        };
        let mut previous = BrowsePasteRetryPlan::from_plan(tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Cut,
            mappings: vec![mapping.clone()],
        });
        previous
            .recovery_by_source
            .insert(mapping.source.clone(), proof.clone());
        previous.recovery_journal_path = Some(PathBuf::from("durable.jsonl"));
        let supplied = BrowsePasteRetryPlan::from_plan(tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Cut,
            mappings: vec![mapping.clone()],
        });

        let merged = merge_resume_retry_plan(Some(previous), Some(supplied), &[mapping.clone()])
            .expect("merged retry plan");
        assert_eq!(merged.recovery_by_source.get(&mapping.source), Some(&proof));
        assert_eq!(
            merged.recovery_journal_path.as_deref(),
            Some(Path::new("durable.jsonl"))
        );
    }

    #[test]
    fn resume_increments_generation_and_preserves_deferred_artifacts() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let retry = sample_retry_plan();
        let first = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            11,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![sample_mapping()],
            Some(retry.clone()),
            serde_json::json!({"generation": 1}),
        )
        .expect("create journal");
        let temp_artifact = temp.path().join(format!(
            ".destination.tonepoet-part-{}-1-1-1.tmp",
            first.job_id()
        ));
        first
            .record_temp_artifact(&temp_artifact, &temp.path().join("destination"))
            .expect("record artifact");
        first.mark_abandoned("injected wedge").expect("abandon");

        let second = FileTaskJournalHandle::resume(
            first.path().to_path_buf(),
            2,
            12,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![sample_mapping()],
            Some(retry),
            serde_json::json!({"generation": 2}),
        )
        .expect("resume journal");
        assert_eq!(second.job_id(), first.job_id());
        assert_eq!(second.generation(), 2);
        assert!(!second.is_abandoned(), "abandon markers are generation-scoped");
        second
            .mark_lifecycle(DurableFileTaskLifecycle::Running, "resumed")
            .expect("new generation checkpoint");
        let record = second.load().expect("load resumed record");
        assert_eq!(record.generation, 2);
        assert_eq!(record.lifecycle, DurableFileTaskLifecycle::Running);
        assert_eq!(record.temp_artifacts.len(), 1);
        assert_eq!(record.temp_artifacts[0].path, temp_artifact);
    }

    #[test]
    fn resume_and_checkpoint_preserve_completed_roots_from_prior_generations() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let first_mapping = sample_mapping();
        let second_mapping = tui_file_picker::PasteMapping {
            source: PathBuf::from("source-two"),
            destination: PathBuf::from("destination-two"),
        };
        let first = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            14,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![first_mapping.clone(), second_mapping.clone()],
            None,
            serde_json::json!({"generation": 1}),
        )
        .expect("create journal");
        let completed = tui_file_picker::FileTaskRootResult {
            source: first_mapping.source.clone(),
            destination: first_mapping.destination.clone(),
            disposition: tui_file_picker::FileTaskRootDisposition::Completed,
            message: None,
            undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
            proof: None,
        };
        first
            .record_checkpoint(
                DurableFileTaskLifecycle::AwaitingReconciliation,
                "one root pending",
                std::slice::from_ref(&completed),
                None,
            )
            .expect("checkpoint first generation");

        let second = FileTaskJournalHandle::resume(
            first.path().to_path_buf(),
            2,
            15,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![second_mapping.clone()],
            None,
            serde_json::json!({"generation": 2}),
        )
        .expect("resume journal");
        let reconciled = tui_file_picker::FileTaskRootResult {
            source: second_mapping.source.clone(),
            destination: second_mapping.destination.clone(),
            disposition: tui_file_picker::FileTaskRootDisposition::Completed,
            message: None,
            undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
            proof: None,
        };
        second
            .record_checkpoint(
                DurableFileTaskLifecycle::Completed,
                "all roots complete",
                std::slice::from_ref(&reconciled),
                None,
            )
            .expect("checkpoint second generation");

        let record = second.load().expect("load merged journal");
        assert_eq!(record.mappings, vec![first_mapping, second_mapping]);
        assert_eq!(record.roots.len(), 2);
        assert!(record.pending_mappings().is_empty());
    }

    #[test]
    fn reconciliation_mappings_keep_journal_owned_quarantine_roots() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let committed = sample_mapping();
        let pending = tui_file_picker::PasteMapping {
            source: PathBuf::from("source-two"),
            destination: PathBuf::from("destination-two"),
        };
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Cut,
            mappings: vec![committed.clone(), pending.clone()],
        };
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            17,
            true,
            tui_file_picker::VerificationMode::Standard,
            8,
            plan.mappings.clone(),
            Some(BrowsePasteRetryPlan::from_plan(plan)),
            serde_json::json!({"generation": 1}),
        )
        .expect("create move journal");
        let proof = BrowseMoveRecoveryProof {
            source_manifest: tui_file_picker::SourceManifest::default(),
            destination_manifest: tui_file_picker::DestinationManifest::default(),
        };
        handle
            .record_move_recovery_proof(&committed.source, &proof)
            .expect("record committed move proof");
        handle
            .record_quarantine_artifact(
                &temp.path().join("quarantine"),
                &committed.source,
                &committed.destination,
            )
            .expect("record quarantine obligation");
        handle
            .record_checkpoint(
                DurableFileTaskLifecycle::AwaitingReconciliation,
                "one committed cleanup and one retryable root",
                &[tui_file_picker::FileTaskRootResult {
                    source: committed.source.clone(),
                    destination: committed.destination.clone(),
                    disposition:
                        tui_file_picker::FileTaskRootDisposition::CompletedWithWarning,
                    message: Some("cleanup pending".to_string()),
                    undo_disposition:
                        tui_file_picker::FileTaskUndoDisposition::NotReversible,
                    proof: None,
                }],
                None,
            )
            .expect("checkpoint committed root");

        let record = handle.load().expect("load journal");
        assert_eq!(record.pending_mappings(), vec![pending.clone()]);
        assert_eq!(
            record.mappings_for_reconciliation(std::slice::from_ref(&pending)),
            vec![committed, pending],
            "the helper generation must reconcile committed cleanup before ordinary retry work"
        );
    }

    #[test]
    fn startup_recovery_restores_exact_copy_plan() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let mapping = sample_mapping();
        let retry = sample_retry_plan();
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            13,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![mapping.clone()],
            Some(retry),
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        handle.mark_abandoned("restart test").expect("abandon");

        let recovery = startup_file_task_recovery().expect("pending recovery");
        assert_eq!(recovery.journal_path, handle.path());
        assert_eq!(
            recovery.retry_plan.plan.mode,
            tui_file_picker::FilePickerClipboardMode::Copy
        );
        assert_eq!(recovery.retry_plan.plan.mappings.as_slice(), &[mapping]);
        assert_eq!(
            recovery.retry_plan.recovery_journal_path.as_deref(),
            Some(handle.path())
        );
    }

    #[test]
    fn startup_recovery_surfaces_cleanup_only_obligations() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let mapping = sample_mapping();
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            16,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            vec![mapping.clone()],
            None,
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        let completed = tui_file_picker::FileTaskRootResult {
            source: mapping.source.clone(),
            destination: mapping.destination.clone(),
            disposition: tui_file_picker::FileTaskRootDisposition::Completed,
            message: None,
            undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
            proof: None,
        };
        handle
            .record_checkpoint(
                DurableFileTaskLifecycle::Completed,
                "copy complete",
                std::slice::from_ref(&completed),
                None,
            )
            .expect("complete root");
        let private_artifact = temp.path().join(format!(
            ".destination.tonepoet-part-{}-1-1-1.tmp",
            handle.job_id()
        ));
        handle
            .record_temp_artifact(&private_artifact, &temp.path().join("destination"))
            .expect("record cleanup obligation");

        let recovery = startup_file_task_recovery().expect("cleanup-only recovery");
        assert_eq!(recovery.retry_plan.plan.mappings.as_slice(), &[mapping]);
        assert_eq!(recovery.temp_artifact_count, 1);
    }

    #[test]
    fn confirmed_discard_is_durable_and_never_reenters_resume_inventory() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![sample_mapping()],
        };
        let handle = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),
            1,
            30,
            false,
            tui_file_picker::VerificationMode::Standard,
            8,
            plan.mappings.clone(),
            Some(BrowsePasteRetryPlan::from_plan(plan)),
            serde_json::json!({"test": true}),
        )
        .expect("create journal");
        handle.mark_abandoned("restart test").expect("abandon");
        handle
            .mark_recovery_discard_started()
            .expect("persist confirmed discard");
        let path = handle.path().to_path_buf();
        drop(handle);

        let record = load_record(&path).expect("reload confirmed discard");
        assert!(record.needs_reconciliation());
        assert!(recovery_record_is_discard_cleanup(&record));
        assert_eq!(
            record.last_status.as_deref(),
            Some(RECOVERY_DISCARDING_MARKER)
        );

        let inventory = startup_file_task_recovery_inventory();
        assert_eq!(inventory.total_pending_jobs, 1);
        assert!(inventory.recoveries.is_empty());
        assert_eq!(inventory.recovery_surface_records.len(), 1);
        assert!(inventory.recovery_surface_records[0]
            .record
            .as_ref()
            .is_some_and(recovery_record_is_discard_cleanup));
    }

    #[test]
    fn startup_recovery_inventory_surfaces_every_pending_journal() {
        let _lock = test_environment_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _environment = JournalDirGuard::install(temp.path());

        let first_mapping = tui_file_picker::PasteMapping {
            source: PathBuf::from("first-source"),
            destination: PathBuf::from("first-destination"),
        };
        let second_mapping = tui_file_picker::PasteMapping {
            source: PathBuf::from("second-source"),
            destination: PathBuf::from("second-destination"),
        };
        let mut expected_paths = Vec::new();
        for (session_id, mapping) in [(31, first_mapping), (32, second_mapping)] {
            let plan = tui_file_picker::PastePlan {
                mode: tui_file_picker::FilePickerClipboardMode::Copy,
                mappings: vec![mapping],
            };
            let handle = FileTaskJournalHandle::create(
                uuid::Uuid::new_v4().to_string(),
                1,
                session_id,
                false,
                tui_file_picker::VerificationMode::Standard,
                8,
                plan.mappings.clone(),
                Some(BrowsePasteRetryPlan::from_plan(plan)),
                serde_json::json!({"test": true}),
            )
            .expect("create journal");
            handle.mark_abandoned("restart test").expect("abandon");
            expected_paths.push(handle.path().to_path_buf());
        }

        let inventory = startup_file_task_recovery_inventory();
        assert_eq!(inventory.total_pending_jobs, 2);
        assert_eq!(inventory.recoveries.len(), 2);
        assert!(inventory.unreconstructable_journals.is_empty());
        let recovered_paths = inventory
            .recoveries
            .iter()
            .map(|recovery| recovery.journal_path.as_path())
            .collect::<Vec<_>>();
        for expected in &expected_paths {
            assert!(recovered_paths.contains(&expected.as_path()));
        }
    }

}

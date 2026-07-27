//! Rename plan: validation + journal-based execution with rollback.
//!
//! The bulk rename wizard builds a `RenamePlan` from the editable preview
//! list. Before touching disk, `validate_plan` catches conflicts, missing
//! sources, traversal attacks, and already-correct names. `execute_plan`
//! creates directories and moves files with a journal so that any mid-
//! operation failure triggers a rollback to the original state.

use std::path::{Path, PathBuf};

use crate::convert::rename_plan::{
    plan_rename_transaction, RenameIntent, RenameTransactionPlan,
};

// ── Data structures ─────────────────────────────────────────────────

/// Status of a single rename operation.
#[derive(Debug, Clone, PartialEq)]
pub enum OpStatus {
    /// Ready to execute.
    Pending,
    /// Successfully moved.
    Succeeded,
    /// Skipped with a reason (not an error — just nothing to do).
    Skipped(String),
    /// Failed during execution.
    Failed(String),
    /// Two or more ops share the same target — must be resolved by the
    /// user before committing.
    Conflict,
}

/// A single file rename operation.
#[derive(Debug, Clone)]
pub struct RenameOp {
    /// Original file path (absolute).
    pub source: PathBuf,
    /// Proposed new name, relative to `RenamePlan::base_dir`. May
    /// contain `/` to create subdirectories.
    pub target_relative: String,
    /// Current status (set by validation and execution).
    pub status: OpStatus,
}

/// A validated, executable rename plan.
#[derive(Debug, Clone)]
pub struct RenamePlan {
    pub ops: Vec<RenameOp>,
    /// Directory relative to which targets are resolved. Typically the
    /// current browse directory.
    pub base_dir: PathBuf,
}

// ── Sanitization ────────────────────────────────────────────────────

/// Sanitize a resolved template path for the filesystem. Applies
/// `sanitize_for_filesystem` to each path component individually so
/// that `/` separators (for subdirectory creation) are preserved while
/// unsafe characters within components are replaced.
///
/// Also rejects `.` and `..` components (directory traversal).
pub fn sanitize_path(resolved: &str) -> Result<String, String> {
    let components: Vec<String> = resolved
        .split('/')
        .filter(|c| !c.is_empty())
        .map(|component| {
            let trimmed = component.trim();
            if trimmed == "." || trimmed == ".." {
                Err(format!(
                    "template produces unsafe path component: '{}'",
                    trimmed
                ))
            } else {
                Ok(crate::convert::renaming::sanitize_for_filesystem(trimmed))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    if components.is_empty() {
        return Err("template produces empty filename".to_string());
    }

    Ok(components.join("/"))
}

// ── Plan building ───────────────────────────────────────────────────

impl RenamePlan {
    /// Build a plan from a list of (source, proposed_target_relative) pairs.
    pub fn new(base_dir: PathBuf, items: Vec<(PathBuf, String)>) -> Self {
        let ops = items
            .into_iter()
            .map(|(source, target_relative)| RenameOp {
                source,
                target_relative,
                status: OpStatus::Pending,
            })
            .collect();
        Self { ops, base_dir }
    }

    /// Full target path for an operation.
    pub fn full_target(&self, op: &RenameOp) -> PathBuf {
        self.base_dir.join(&op.target_relative)
    }

    /// Number of ops that are still `Pending` after validation.
    pub fn pending_count(&self) -> usize {
        self.ops
            .iter()
            .filter(|op| op.status == OpStatus::Pending)
            .count()
    }

    /// Number of ops with `Conflict` status.
    pub fn conflict_count(&self) -> usize {
        self.ops
            .iter()
            .filter(|op| op.status == OpStatus::Conflict)
            .count()
    }
}

// ── Validation ──────────────────────────────────────────────────────

/// Validate a rename plan. Sets the status of each op:
/// - `Skipped` for source==target, missing source, existing target
/// - `Conflict` for duplicate targets
/// - `Pending` for valid, ready-to-execute ops
///
/// Returns the number of conflicts (which block execution).
pub fn validate_plan(plan: &mut RenamePlan) -> usize {
    for op in &mut plan.ops {
        op.status = OpStatus::Pending;
    }

    for op in &mut plan.ops {
        let full_target = plan.base_dir.join(&op.target_relative);
        if op.source == full_target {
            op.status = OpStatus::Skipped("already named correctly".into());
        } else if !op.source.exists() {
            op.status = OpStatus::Skipped("source missing".into());
        }
    }

    let intents = plan
        .ops
        .iter()
        .filter(|op| op.status == OpStatus::Pending)
        .map(|op| RenameIntent {
            source: op.source.clone(),
            destination: plan.base_dir.join(&op.target_relative),
        })
        .collect::<Vec<_>>();

    match plan_rename_transaction(&plan.base_dir, intents) {
        Ok(_) => 0,
        Err(error) => {
            // Browse keeps a deliberately simple UI status model. The shared
            // planner supplies the authoritative end-state validation; when it
            // rejects the transaction, every still-pending row is marked as a
            // conflict so the user cannot commit a partial subset accidentally.
            let mut conflicts = 0;
            for op in &mut plan.ops {
                if op.status == OpStatus::Pending {
                    op.status = OpStatus::Conflict;
                    conflicts += 1;
                }
            }
            if conflicts == 0 && !plan.ops.is_empty() {
                plan.ops[0].status = OpStatus::Skipped(error);
            }
            conflicts
        }
    }
}

// ── Execution ───────────────────────────────────────────────────────

/// Return the exact old-to-new mappings whose plan rows completed. This is
/// deliberately derived from status after execution so skipped/conflicted/
/// rolled-back rows can never leak into the undo journal.
pub fn succeeded_mappings(plan: &RenamePlan) -> Vec<(PathBuf, PathBuf)> {
    plan.ops
        .iter()
        .filter(|op| op.status == OpStatus::Succeeded)
        .map(|op| {
            (
                op.source.clone(),
                plan.base_dir.join(&op.target_relative),
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct RenameExecutionRoot {
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Authoritative proof assembled from the pre-operation source manifest
    /// and the retained-handle-verified post-rename root binding.
    pub proof: Result<tui_file_picker::FileTaskRootProof, String>,
}

#[derive(Debug, Clone)]
pub struct RenameExecutionReport {
    pub succeeded_count: usize,
    pub roots: Vec<RenameExecutionRoot>,
    pub warning: Option<String>,
}

/// Execute a validated rename plan. Creates target directories, then
/// moves files one at a time. On the first failure, rolls back ALL
/// previously-succeeded moves and returns an error.
///
/// **Precondition:** `validate_plan` must have been called and
/// `conflict_count() == 0`. Panics if conflicts remain.
///
/// Returns `Ok(succeeded_count)` on full success, or `Err(message)` on
/// failure (with rollback already performed).
pub fn execute_plan(plan: &mut RenamePlan) -> Result<usize, String> {
    execute_plan_with_proofs(plan).map(|report| report.succeeded_count)
}

/// Execute a rename transaction and return operation-time authority for every
/// committed mapping. Recursive source manifests are captured before any
/// namespace mutation; after the shared staging transaction commits, retained
/// source handles bind each original object to its final destination. No
/// published path is recursively recaptured after completion.
pub fn execute_plan_with_proofs(
    plan: &mut RenamePlan,
) -> Result<RenameExecutionReport, String> {
    execute_plan_with_proofs_at_verification(
        plan,
        tui_file_picker::VerificationMode::Strong,
    )
}

pub fn execute_plan_with_proofs_at_verification(
    plan: &mut RenamePlan,
    verification: tui_file_picker::VerificationMode,
) -> Result<RenameExecutionReport, String> {
    execute_plan_with_proofs_internal(plan, None, verification)
}

/// Execute a replay transaction only if each pre-mutation source manifest
/// matches the operation-time authority retained by the undo journal. The
/// comparison uses the same manifest pass required to produce the next undo
/// proof; it does not re-open or re-hash the tree after publication.
pub fn execute_plan_with_proofs_and_expected_sources(
    plan: &mut RenamePlan,
    expected_sources: &[tui_file_picker::FileTaskRootProof],
) -> Result<RenameExecutionReport, String> {
    execute_plan_with_proofs_and_expected_sources_at_verification(
        plan,
        expected_sources,
        tui_file_picker::VerificationMode::Strong,
    )
}

pub fn execute_plan_with_proofs_and_expected_sources_at_verification(
    plan: &mut RenamePlan,
    expected_sources: &[tui_file_picker::FileTaskRootProof],
    verification: tui_file_picker::VerificationMode,
) -> Result<RenameExecutionReport, String> {
    execute_plan_with_proofs_internal(plan, Some(expected_sources), verification)
}

fn execute_plan_with_proofs_internal(
    plan: &mut RenamePlan,
    expected_sources: Option<&[tui_file_picker::FileTaskRootProof]>,
    verification: tui_file_picker::VerificationMode,
) -> Result<RenameExecutionReport, String> {
    assert_eq!(
        plan.conflict_count(),
        0,
        "execute_plan_with_proofs called with unresolved conflicts"
    );

    let pending_indices = plan
        .ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| (op.status == OpStatus::Pending).then_some(index))
        .collect::<Vec<_>>();
    if pending_indices.is_empty() {
        return Ok(RenameExecutionReport {
            succeeded_count: 0,
            roots: Vec::new(),
            warning: None,
        });
    }
    if let Some(expected) = expected_sources {
        if expected.len() != pending_indices.len() {
            return Err(format!(
                "rename replay supplied {} source proofs for {} pending mappings",
                expected.len(),
                pending_indices.len(),
            ));
        }
    }

    struct PreimageAuthority {
        index: usize,
        source: PathBuf,
        destination: PathBuf,
        manifest: tui_file_picker::SourceManifest,
        rename_proof: tui_file_picker::RenameSourceProof,
        source_capabilities: tui_file_picker::FilesystemCapabilities,
    }

    let mut authorities = Vec::with_capacity(pending_indices.len());
    for (proof_index, &index) in pending_indices.iter().enumerate() {
        let source = plan.ops[index].source.clone();
        let destination = plan.base_dir.join(&plan.ops[index].target_relative);
        let manifest = tui_file_picker::capture_manifest_with_mode(&source, verification).map_err(|error| {
            format!(
                "could not capture authoritative rename preimage for {}: {error}",
                source.display(),
            )
        })?;
        let source_capabilities = tui_file_picker::filesystem_capabilities(&source);
        if let Some(expected) = expected_sources {
            expected[proof_index]
                .destination_manifest
                .verify_captured_replay_source(
                    &expected[proof_index].source_manifest,
                    &manifest,
                    source_capabilities,
                )
                .map_err(|error| {
                    format!(
                        "rename replay source {} no longer matches operation-time authority: {error}",
                        source.display(),
                    )
                })?;
        }
        let rename_proof = tui_file_picker::RenameSourceProof::capture(&source).map_err(|error| {
            format!(
                "could not retain rename source authority for {}: {error}",
                source.display(),
            )
        })?;
        rename_proof
            .verify_manifest_root(&manifest, source_capabilities)
            .map_err(|error| {
                format!(
                    "rename source {} changed while authority was established: {error}",
                    source.display(),
                )
            })?;
        authorities.push(PreimageAuthority {
            index,
            source: source.clone(),
            destination,
            manifest,
            rename_proof,
            source_capabilities,
        });
    }

    let transaction = plan_rename_transaction(
        &plan.base_dir,
        pending_indices.iter().map(|&index| RenameIntent {
            source: plan.ops[index].source.clone(),
            destination: plan.base_dir.join(&plan.ops[index].target_relative),
        }),
    )?;

    let workspace = create_unique_rename_workspace(&plan.base_dir)?;
    let result = execute_shared_transaction(&transaction, &workspace);
    let cleanup_result = std::fs::remove_dir(&workspace);
    match result {
        Ok((count, degraded_warning)) => {
            for &index in &pending_indices {
                plan.ops[index].status = OpStatus::Succeeded;
            }
            let cleanup_warning = cleanup_result.err().map(|error| {
                format!(
                    "rename completed but empty transaction workspace cleanup failed at {}: {error}",
                    workspace.display(),
                )
            });
            if let Some(warning) = cleanup_warning.as_deref() {
                log::warn!("{warning}");
            }
            let cleanup_warning = match (degraded_warning, cleanup_warning) {
                (Some(degraded), Some(cleanup)) => Some(format!("{degraded}; {cleanup}")),
                (Some(degraded), None) => Some(degraded.to_string()),
                (None, cleanup) => cleanup,
            };

            let roots = authorities
                .into_iter()
                .map(|authority| {
                    let destination_capabilities =
                        tui_file_picker::filesystem_capabilities(&authority.destination);
                    let verification = tui_file_picker::verify_renamed_destination(
                        &authority.destination,
                        &authority.rename_proof,
                        authority.source_capabilities,
                        destination_capabilities,
                    );
                    let proof = match verification {
                        Ok(verification) => authority
                            .manifest
                            .destination_identity_after_root_rename(
                                verification.destination_snapshot,
                                destination_capabilities,
                            )
                            .map(|destination_manifest| {
                                tui_file_picker::FileTaskRootProof {
                                    source_manifest: authority.manifest,
                                    destination_manifest,
                                }
                            }),
                        Err(error) => Err(error),
                    }
                    .map_err(|error| {
                        format!(
                            "rename committed from {} to {}, but operation-time undo authority could not be established: {error}",
                            authority.source.display(),
                            authority.destination.display(),
                        )
                    });
                    debug_assert_eq!(plan.ops[authority.index].status, OpStatus::Succeeded);
                    RenameExecutionRoot {
                        source: authority.source,
                        destination: authority.destination,
                        proof,
                    }
                })
                .collect();
            Ok(RenameExecutionReport {
                succeeded_count: count,
                roots,
                warning: cleanup_warning,
            })
        }
        Err(error) => {
            for &index in &pending_indices {
                if plan.ops[index].status == OpStatus::Pending {
                    plan.ops[index].status = OpStatus::Failed(error.clone());
                }
            }
            Err(error)
        }
    }
}

fn create_unique_rename_workspace(base_dir: &Path) -> Result<PathBuf, String> {
    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1024 {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = base_dir.join(format!(
            ".tonepoet-browse-rename-{}-{sequence}",
            std::process::id()
        ));
        #[cfg(unix)]
        let create = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&candidate)
        };
        #[cfg(not(unix))]
        let create = std::fs::create_dir(&candidate);
        match create {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create rename transaction workspace {}: {error}",
                    candidate.display(),
                ))
            }
        }
    }
    Err(format!(
        "could not allocate a rename transaction workspace under {}",
        base_dir.display()
    ))
}

fn execute_shared_transaction(
    transaction: &RenameTransactionPlan,
    workspace: &Path,
) -> Result<(usize, Option<&'static str>), String> {
    // Default-path posture: use the degrading no-replace ladder so renames work
    // on mounts without renameat2 authority (cifs, ntfs-3g); the first degraded
    // hop yields a single post-commit warning instead of a hard failure.
    let mut degraded: Option<&'static str> = None;
    let staging_paths = transaction
        .entries
        .iter()
        .enumerate()
        .map(|(index, _)| workspace.join(format!("entry-{index:06}")))
        .collect::<Vec<_>>();
    let mut staged = Vec::new();
    let mut installed = Vec::new();

    let operation = (|| {
        for index in transaction.staging_order() {
            let _entry = &transaction.entries[index];
            let source = effective_source_after_ancestor_staging(
                index,
                transaction,
                &staging_paths,
                &staged,
            );
            match std::fs::symlink_metadata(&source) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(format!(
                        "rename source changed after validation: {}",
                        source.display()
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "inspect rename source after validation {}: {error}",
                        source.display()
                    ));
                }
            }
            let mode = tui_file_picker::rename_no_replace(&source, &staging_paths[index])
                .map_err(|error| {
                    format!(
                        "rename staging failed: {} -> {}: {error}",
                        source.display(),
                        staging_paths[index].display()
                    )
                })?;
            degraded = degraded.or(mode.degraded_warning());
            staged.push(index);
        }

        for index in transaction.installation_order() {
            let entry = &transaction.entries[index];
            if let Some(parent) = entry.destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "failed to create rename destination directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
            let mode = tui_file_picker::rename_no_replace(&staging_paths[index], &entry.destination)
                .map_err(|error| {
                    format!(
                        "rename publish failed: {} -> {}: {error}",
                        staging_paths[index].display(),
                        entry.destination.display()
                    )
                })?;
            degraded = degraded.or(mode.degraded_warning());
            installed.push(index);
        }
        Ok(transaction.entries.len())
    })();

    match operation {
        Ok(count) => Ok((count, degraded)),
        Err(primary) => {
            let mut rollback_errors = Vec::new();
            for &index in installed.iter().rev() {
                let entry = &transaction.entries[index];
                match std::fs::symlink_metadata(&entry.destination) {
                    Ok(_) => {
                        if let Err(error) = tui_file_picker::rename_no_replace(
                            &entry.destination,
                            &staging_paths[index],
                        ) {
                            rollback_errors.push(format!(
                                "{} -> {}: {error}",
                                entry.destination.display(),
                                staging_paths[index].display()
                            ));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => rollback_errors.push(format!(
                        "inspect rollback destination {}: {error}",
                        entry.destination.display(),
                    )),
                }
            }
            for &index in staged.iter().rev() {
                match std::fs::symlink_metadata(&staging_paths[index]) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        rollback_errors.push(format!(
                            "inspect staged rollback object {}: {error}",
                            staging_paths[index].display(),
                        ));
                        continue;
                    }
                }
                let restore_target = effective_restore_target(
                    index,
                    transaction,
                    &staging_paths,
                    &staged,
                );
                if let Some(parent) = restore_target.parent() {
                    if let Err(error) = std::fs::create_dir_all(parent) {
                        rollback_errors.push(format!(
                            "create {} for rollback: {error}",
                            parent.display()
                        ));
                        continue;
                    }
                }
                if let Err(error) = tui_file_picker::rename_no_replace(
                    &staging_paths[index],
                    &restore_target,
                ) {
                    rollback_errors.push(format!(
                        "{} -> {}: {error}",
                        staging_paths[index].display(),
                        restore_target.display()
                    ));
                }
            }
            if rollback_errors.is_empty() {
                Err(format!("{primary}. All staged renames rolled back."))
            } else {
                Err(format!(
                    "{primary}. Rollback partially failed; preserved workspace {}: {}",
                    workspace.display(),
                    rollback_errors.join("; ")
                ))
            }
        }
    }
}

fn effective_source_after_ancestor_staging(
    index: usize,
    transaction: &RenameTransactionPlan,
    staging_paths: &[PathBuf],
    staged: &[usize],
) -> PathBuf {
    let source = &transaction.entries[index].source;
    staged
        .iter()
        .filter_map(|&ancestor_index| {
            let ancestor = &transaction.entries[ancestor_index].source;
            source
                .strip_prefix(ancestor)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| (ancestor.components().count(), staging_paths[ancestor_index].join(relative)))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| path)
        .unwrap_or_else(|| source.clone())
}

fn effective_restore_target(
    index: usize,
    transaction: &RenameTransactionPlan,
    staging_paths: &[PathBuf],
    staged: &[usize],
) -> PathBuf {
    let source = &transaction.entries[index].source;
    staged
        .iter()
        .filter(|&&ancestor_index| ancestor_index != index && staging_paths[ancestor_index].exists())
        .filter_map(|&ancestor_index| {
            let ancestor = &transaction.entries[ancestor_index].source;
            source
                .strip_prefix(ancestor)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| (ancestor.components().count(), staging_paths[ancestor_index].join(relative)))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| path)
        .unwrap_or_else(|| source.clone())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod live_mount_repro {
    use super::*;

    /// Field-repro harness: drives the real rename engine against a live mount.
    /// Run manually:
    ///   TONEPOET_REPRO_DIR=/path/on/mount cargo test -p tonepoet --lib --     ///     live_mount_repro --ignored --nocapture
    #[test]
    #[ignore]
    fn directory_rename_on_live_mount() {
        let Some(base) = std::env::var_os("TONEPOET_REPRO_DIR") else {
            eprintln!("TONEPOET_REPRO_DIR not set; skipping");
            return;
        };
        let base = PathBuf::from(base);
        let scratch = base
            .join(format!(".tonepoet-repro-{}", std::process::id()))
            .join("Air Supply - Lost in Love (1980) [VINYL] {24-192}");
        std::fs::create_dir_all(scratch.join("Artworks-old")).expect("scratch dir");
        std::fs::write(scratch.join("Artworks-old/cover.jpg"), b"jpg").expect("file");

        let mut plan = RenamePlan::new(
            scratch.clone(),
            vec![(scratch.join("Artworks-old"), "Artworks-new".to_string())],
        );
        let conflicts = validate_plan(&mut plan);
        eprintln!("validate_plan conflicts: {conflicts}");
        for op in &plan.ops {
            eprintln!("op status: {:?}", op.status);
        }
        if conflicts == 0 {
            match execute_plan_with_proofs(&mut plan) {
                Ok(report) => {
                    eprintln!("SUCCEEDED: {} (warning: {:?})", report.succeeded_count, report.warning);
                    for root in &report.roots {
                        eprintln!("root proof: {:?}", root.proof.as_ref().map(|_| "ok").map_err(|e| e.clone()));
                    }
                }
                Err(error) => eprintln!("FAILED: {error}"),
            }
        }
        let _ = std::fs::remove_dir_all(scratch.parent().unwrap_or(&scratch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique temp dir per test invocation. Uses a counter to
    /// avoid collisions when tests run in parallel within the same process.
    fn tmp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tonepoet_rename_test_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_path_preserves_slashes() {
        let result = sanitize_path("Artist/Album/01 - Song.flac").unwrap();
        assert_eq!(result, "Artist/Album/01 - Song.flac");
    }

    #[test]
    fn sanitize_path_cleans_components() {
        // AC/DC is a special case → ACDC in the component
        let result = sanitize_path("AC/DC/Album/Song.flac").unwrap();
        // "AC" is clean, "DC" is clean, so it stays as-is
        // (only the literal string "AC/DC" triggers the special case in
        //  sanitize_for_filesystem, but here "AC" and "DC" are separate
        //  components after splitting on /)
        assert_eq!(result, "AC/DC/Album/Song.flac");
    }

    #[test]
    fn sanitize_path_rejects_dotdot() {
        assert!(sanitize_path("../etc/passwd").is_err());
        assert!(sanitize_path("Artist/../../../etc").is_err());
    }

    #[test]
    fn sanitize_path_rejects_empty() {
        assert!(sanitize_path("").is_err());
    }

    #[test]
    fn sanitize_path_replaces_unsafe_chars() {
        let result = sanitize_path("Artist: Name/Song?.flac").unwrap();
        assert_eq!(result, "Artist- Name/Song_.flac");
    }

    #[test]
    fn validate_skips_source_eq_target() {
        let dir = tmp_dir();
        let file = dir.join("song.flac");
        fs::write(&file, b"test").unwrap();

        let mut plan = RenamePlan::new(dir.clone(), vec![(file.clone(), "song.flac".to_string())]);
        let conflicts = validate_plan(&mut plan);
        assert_eq!(conflicts, 0);
        assert!(matches!(plan.ops[0].status, OpStatus::Skipped(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_detects_conflicts() {
        let dir = tmp_dir();
        let a = dir.join("a.flac");
        let b = dir.join("b.flac");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        let mut plan = RenamePlan::new(
            dir.clone(),
            vec![(a, "same.flac".to_string()), (b, "same.flac".to_string())],
        );
        let conflicts = validate_plan(&mut plan);
        assert_eq!(conflicts, 2);
        assert_eq!(plan.ops[0].status, OpStatus::Conflict);
        assert_eq!(plan.ops[1].status, OpStatus::Conflict);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_skips_missing_source() {
        let dir = tmp_dir();
        let missing = dir.join("gone.flac");

        let mut plan = RenamePlan::new(dir.clone(), vec![(missing, "new.flac".to_string())]);
        let conflicts = validate_plan(&mut plan);
        assert_eq!(conflicts, 0);
        assert!(matches!(plan.ops[0].status, OpStatus::Skipped(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_simple_rename() {
        let dir = tmp_dir();
        let file = dir.join("old.flac");
        fs::write(&file, b"data").unwrap();

        let mut plan = RenamePlan::new(dir.clone(), vec![(file.clone(), "new.flac".to_string())]);
        validate_plan(&mut plan);
        let result = execute_plan(&mut plan);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
        assert!(!file.exists());
        assert!(dir.join("new.flac").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_creates_subdirectories() {
        let dir = tmp_dir();
        let file = dir.join("song.flac");
        fs::write(&file, b"data").unwrap();

        let mut plan = RenamePlan::new(
            dir.clone(),
            vec![(file.clone(), "Artist/Album/01 - Song.flac".to_string())],
        );
        validate_plan(&mut plan);
        let result = execute_plan(&mut plan);
        assert!(result.is_ok());
        assert!(!file.exists());
        assert!(dir.join("Artist/Album/01 - Song.flac").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_rollback_on_failure() {
        let dir = tmp_dir();
        let a = dir.join("a.flac");
        let b = dir.join("b.flac");
        fs::write(&a, b"aaa").unwrap();
        fs::write(&b, b"bbb").unwrap();

        // Make target for b's rename a read-only directory to force failure.
        // (On most systems, creating a file inside a non-writable dir fails.)
        let block_dir = dir.join("blocked");
        fs::create_dir(&block_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&block_dir, fs::Permissions::from_mode(0o000)).unwrap();
        }

        let mut plan = RenamePlan::new(
            dir.clone(),
            vec![
                (a.clone(), "a_new.flac".to_string()),
                (b.clone(), "blocked/b_new.flac".to_string()),
            ],
        );
        validate_plan(&mut plan);
        let result = execute_plan(&mut plan);

        // Restore permissions for cleanup.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&block_dir, fs::Permissions::from_mode(0o755));
        }

        // If the blocked rename failed (Unix only), rollback should have
        // restored a.flac. On non-Unix this might succeed.
        #[cfg(unix)]
        {
            assert!(result.is_err());
            // a.flac should be back at its original location (rolled back).
            assert!(a.exists(), "a.flac should have been rolled back");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn standard_directory_rename_and_replay_proofs_remain_digest_free() {
        let dir = tmp_dir();
        let source = dir.join("album");
        fs::create_dir(&source).expect("album");
        fs::write(source.join("track.flac"), b"audio payload").expect("track");

        let mut plan = RenamePlan::new(
            dir.clone(),
            vec![(source.clone(), "renamed-album".to_string())],
        );
        assert_eq!(validate_plan(&mut plan), 0);
        let report = execute_plan_with_proofs_at_verification(
            &mut plan,
            tui_file_picker::VerificationMode::Standard,
        )
        .expect("standard rename");
        assert_eq!(report.succeeded_count, 1);
        let proof = report.roots[0].proof.as_ref().expect("rename proof");
        assert_eq!(
            proof.source_manifest.verification(),
            tui_file_picker::VerificationMode::Standard
        );
        assert!(!proof.source_manifest.has_content_digests());
        assert_eq!(
            proof.destination_manifest.verification(),
            tui_file_picker::VerificationMode::Standard
        );

        let renamed = dir.join("renamed-album");
        let mut replay = RenamePlan::new(
            dir.clone(),
            vec![(renamed.clone(), "album".to_string())],
        );
        assert_eq!(validate_plan(&mut replay), 0);
        let replay_report = execute_plan_with_proofs_and_expected_sources_at_verification(
            &mut replay,
            std::slice::from_ref(proof),
            tui_file_picker::VerificationMode::Standard,
        )
        .expect("standard undo replay");
        let replay_proof = replay_report.roots[0]
            .proof
            .as_ref()
            .expect("redo proof");
        assert!(!replay_proof.source_manifest.has_content_digests());
        assert!(source.exists());
        assert!(!renamed.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_authority_rejects_changed_source_before_staging() {
        let dir = tmp_dir();
        let source = dir.join("source.flac");
        let destination = dir.join("destination.flac");
        fs::write(&source, b"authorized bytes").expect("source");
        let source_manifest = tui_file_picker::capture_manifest(&source)
            .expect("capture retained source");
        let expected = tui_file_picker::FileTaskRootProof {
            destination_manifest: source_manifest.destination_identity_for_same_tree(),
            source_manifest,
        };
        fs::write(&source, b"replacement bytes").expect("replace source");

        let mut plan = RenamePlan::new(
            dir.clone(),
            vec![(source.clone(), "destination.flac".to_string())],
        );
        assert_eq!(validate_plan(&mut plan), 0);
        let error = execute_plan_with_proofs_and_expected_sources(
            &mut plan,
            &[expected],
        )
        .expect_err("changed replay source must be refused");

        assert!(error.contains("operation-time authority"));
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).expect("source retained"), b"replacement bytes");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_second_run() {
        let dir = tmp_dir();
        let file = dir.join("old.flac");
        fs::write(&file, b"data").unwrap();

        // First run: rename old.flac → new.flac
        let mut plan = RenamePlan::new(dir.clone(), vec![(file.clone(), "new.flac".to_string())]);
        validate_plan(&mut plan);
        execute_plan(&mut plan).unwrap();

        // Second run: same plan. Source is gone → skipped.
        let mut plan2 = RenamePlan::new(dir.clone(), vec![(file.clone(), "new.flac".to_string())]);
        let conflicts = validate_plan(&mut plan2);
        assert_eq!(conflicts, 0);
        assert!(matches!(plan2.ops[0].status, OpStatus::Skipped(_)));
        // Nothing to execute — pending_count is 0.
        assert_eq!(plan2.pending_count(), 0);

        let _ = fs::remove_dir_all(&dir);
    }
}

//! Rename plan: validation + journal-based execution with rollback.
//!
//! The bulk rename wizard builds a `RenamePlan` from the editable preview
//! list. Before touching disk, `validate_plan` catches conflicts, missing
//! sources, traversal attacks, and already-correct names. `execute_plan`
//! creates directories and moves files with a journal so that any mid-
//! operation failure triggers a rollback to the original state.

use std::collections::HashMap;
use std::path::PathBuf;

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
    // Reset all statuses to Pending first (re-validation).
    for op in &mut plan.ops {
        op.status = OpStatus::Pending;
    }

    // 1. Source == target (already correctly named)
    for op in &mut plan.ops {
        let full_target = plan.base_dir.join(&op.target_relative);
        if op.source == full_target {
            op.status = OpStatus::Skipped("already named correctly".into());
            continue;
        }
        // Canonicalized comparison for case-insensitive filesystems.
        if let (Ok(src), Ok(dst)) = (op.source.canonicalize(), full_target.canonicalize()) {
            if src == dst {
                op.status = OpStatus::Skipped("already named correctly".into());
            }
        }
    }

    // 2. Source doesn't exist (already moved in a previous run)
    for op in &mut plan.ops {
        if op.status == OpStatus::Pending && !op.source.exists() {
            op.status = OpStatus::Skipped("source missing".into());
        }
    }

    // 3. Target already exists. We conservatively skip ALL cases where
    //    the target is already on disk, even if it's a source in another
    //    op (swap scenario). True swaps (A→B, B→A) would require a temp-
    //    file intermediary to avoid data loss from sequential renames;
    //    we don't implement that. Users resolve swaps by editing per-line
    //    to use intermediate names. For idempotent re-runs, the source
    //    won't exist (caught in step 2 above), so this step only fires
    //    for genuine collisions.
    for i in 0..plan.ops.len() {
        if plan.ops[i].status != OpStatus::Pending {
            continue;
        }
        let full_target = plan.base_dir.join(&plan.ops[i].target_relative);
        if full_target.exists() {
            plan.ops[i].status = OpStatus::Skipped("target already exists".into());
        }
    }

    // 4. Detect duplicate targets among pending ops.
    let mut target_counts: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, op) in plan.ops.iter().enumerate() {
        if op.status == OpStatus::Pending {
            target_counts
                .entry(op.target_relative.to_lowercase()) // case-insensitive
                .or_default()
                .push(i);
        }
    }
    let mut conflicts = 0;
    for (_target, indices) in &target_counts {
        if indices.len() > 1 {
            for &i in indices {
                plan.ops[i].status = OpStatus::Conflict;
                conflicts += 1;
            }
        }
    }

    conflicts
}

// ── Execution ───────────────────────────────────────────────────────

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
    assert_eq!(
        plan.conflict_count(),
        0,
        "execute_plan called with unresolved conflicts"
    );

    // Phase 1: collect and create all needed target directories.
    let dirs: Vec<PathBuf> = plan
        .ops
        .iter()
        .filter(|op| op.status == OpStatus::Pending)
        .filter_map(|op| {
            let full = plan.base_dir.join(&op.target_relative);
            full.parent().map(|p| p.to_path_buf())
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for dir in &dirs {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Err(format!(
                "failed to create directory {}: {}",
                dir.display(),
                e
            ));
        }
    }

    // Phase 2: move files with journal for rollback.
    let mut journal: Vec<(PathBuf, PathBuf)> = Vec::new();

    for op in &mut plan.ops {
        if op.status != OpStatus::Pending {
            continue;
        }

        let full_target = plan.base_dir.join(&op.target_relative);

        match std::fs::rename(&op.source, &full_target) {
            Ok(()) => {
                journal.push((op.source.clone(), full_target));
                op.status = OpStatus::Succeeded;
            }
            Err(e) => {
                let err_msg = format!(
                    "rename failed: {} -> {}: {}",
                    op.source.display(),
                    full_target.display(),
                    e
                );
                op.status = OpStatus::Failed(err_msg.clone());

                // Rollback all previously-succeeded moves.
                let mut rollback_errors = Vec::new();
                for (original, moved_to) in journal.iter().rev() {
                    if let Err(rb) = std::fs::rename(moved_to, original) {
                        rollback_errors.push(format!(
                            "{} -> {}: {}",
                            moved_to.display(),
                            original.display(),
                            rb
                        ));
                    }
                }

                // Reset succeeded ops to Pending (they've been rolled back).
                for op in plan.ops.iter_mut() {
                    if op.status == OpStatus::Succeeded {
                        op.status = OpStatus::Pending;
                    }
                }

                if rollback_errors.is_empty() {
                    return Err(format!("{}. All previous renames rolled back.", err_msg));
                } else {
                    return Err(format!(
                        "{}. Rollback partially failed: {}",
                        err_msg,
                        rollback_errors.join("; ")
                    ));
                }
            }
        }
    }

    let succeeded = journal.len();
    Ok(succeeded)
}

// ── Tests ───────────────────────────────────────────────────────────

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

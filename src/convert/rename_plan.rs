//! Shared deterministic rename transaction planning.
//!
//! Both Browse bulk rename and conversion actions use this module for
//! end-state collision analysis and ordering. Execution remains surface-specific:
//! Browse provides interactive rollback, while conversion actions add a durable
//! journal and crash recovery around the same transaction map.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameIntent {
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameTransactionEntry {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub source_depth: usize,
    pub destination_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenameTransactionPlan {
    pub entries: Vec<RenameTransactionEntry>,
    pub no_ops: Vec<RenameIntent>,
}

impl RenameTransactionPlan {
    /// Sources are detached shallowest first. When both a directory and one
    /// of its descendants are selected, staging the ancestor first preserves
    /// the entire subtree under a journal-owned path; descendant sources are
    /// then re-resolved beneath that staged ancestor. This avoids mutating an
    /// ancestor before its original identity has been protected.
    pub fn staging_order(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..self.entries.len()).collect();
        indices.sort_by(|&left, &right| {
            self.entries[left]
                .source_depth
                .cmp(&self.entries[right].source_depth)
                .then_with(|| self.entries[left].source.cmp(&self.entries[right].source))
        });
        indices
    }

    /// Destinations are installed shallowest first so a destination parent
    /// produced by another operation exists before a child is installed.
    pub fn installation_order(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..self.entries.len()).collect();
        indices.sort_by(|&left, &right| {
            self.entries[left]
                .destination_depth
                .cmp(&self.entries[right].destination_depth)
                .then_with(|| {
                    self.entries[left]
                        .destination
                        .cmp(&self.entries[right].destination)
                })
        });
        indices
    }
}

pub fn plan_rename_transaction(
    base_dir: &Path,
    intents: impl IntoIterator<Item = RenameIntent>,
) -> Result<RenameTransactionPlan, String> {
    let base_dir = lexical_absolute(base_dir)?;
    reject_unstable_path(&base_dir)?;

    let mut normalized = Vec::new();
    for intent in intents {
        let source = lexical_absolute(&intent.source)?;
        let destination = lexical_absolute(&intent.destination)?;
        reject_unstable_path(&source)?;
        reject_unstable_path(&destination)?;
        if !source.starts_with(&base_dir) || !destination.starts_with(&base_dir) {
            return Err(format!(
                "rename escapes transaction root {}: {} -> {}",
                base_dir.display(),
                source.display(),
                destination.display()
            ));
        }
        if source == base_dir || destination == base_dir {
            return Err("rename may not replace the transaction root".to_string());
        }
        if destination != source && destination.starts_with(&source) {
            return Err(format!(
                "rename destination may not be nested inside its own source: {} -> {}",
                source.display(),
                destination.display()
            ));
        }
        normalized.push(RenameIntent { source, destination });
    }

    let mut source_keys = BTreeMap::<String, usize>::new();
    for (index, intent) in normalized.iter().enumerate() {
        let key = casefold_path(&intent.source);
        if let Some(previous) = source_keys.insert(key, index) {
            return Err(format!(
                "rename source appears more than once: {} (operations {} and {})",
                intent.source.display(),
                previous + 1,
                index + 1
            ));
        }
    }

    let mut destination_keys = BTreeMap::<String, usize>::new();
    for (index, intent) in normalized.iter().enumerate() {
        let key = casefold_path(&intent.destination);
        if let Some(previous) = destination_keys.insert(key, index) {
            return Err(format!(
                "rename end-state collision at {} (operations {} and {})",
                intent.destination.display(),
                previous + 1,
                index + 1
            ));
        }
    }

    // A destination may exist only when that exact pathname is another source
    // in this transaction. Staging every source first makes swaps and cycles
    // safe without last-write-wins behavior.
    let source_path_keys: BTreeSet<String> = normalized
        .iter()
        .map(|intent| casefold_path(&intent.source))
        .collect();

    let mut plan = RenameTransactionPlan::default();
    for intent in normalized {
        if paths_equal_for_plan(&intent.source, &intent.destination) {
            plan.no_ops.push(intent);
            continue;
        }
        if !intent.source.exists() {
            return Err(format!("rename source is missing: {}", intent.source.display()));
        }
        if intent.destination.exists()
            && !source_path_keys.contains(&casefold_path(&intent.destination))
        {
            return Err(format!(
                "rename destination already exists and is not vacated by this plan: {}",
                intent.destination.display()
            ));
        }
        plan.entries.push(RenameTransactionEntry {
            source_depth: path_depth(&intent.source),
            destination_depth: path_depth(&intent.destination),
            source: intent.source,
            destination: intent.destination,
        });
    }

    // Installing a directory on top of a destination nested beneath another
    // installed non-directory is impossible. Catch the deterministic shape
    // before mutation; runtime type revalidation still occurs immediately
    // before each transition.
    for (left_index, left) in plan.entries.iter().enumerate() {
        for (right_index, right) in plan.entries.iter().enumerate() {
            if left_index == right_index {
                continue;
            }
            if right.destination.starts_with(&left.destination)
                && right.destination != left.destination
                && left.source.is_file()
            {
                return Err(format!(
                    "rename destination {} would be nested under file destination {}",
                    right.destination.display(),
                    left.destination.display()
                ));
            }
        }
    }

    Ok(plan)
}

fn paths_equal_for_plan(left: &Path, right: &Path) -> bool {
    // A case-only rename is a real transaction on case-sensitive filesystems
    // and must still be staged on case-insensitive filesystems. Only an exact
    // lexical identity is a no-op.
    left == right
}

fn casefold_path(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("rename path is not absolute: {}", path.display()));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(format!("rename path escapes root: {}", path.display()));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn reject_unstable_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.parent().is_none() {
        return Err(format!("unsafe rename path: {}", path.display()));
    }
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(format!("unstable rename path: {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn cycles_are_valid_and_have_stable_orders() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let plan = plan_rename_transaction(
            temp.path(),
            [
                RenameIntent { source: a.clone(), destination: b.clone() },
                RenameIntent { source: b, destination: a },
            ],
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.staging_order().len(), 2);
        assert_eq!(plan.installation_order().len(), 2);
    }

    #[test]
    fn directory_cannot_be_renamed_into_its_own_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("parent");
        fs::create_dir_all(&source).unwrap();
        let error = plan_rename_transaction(
            temp.path(),
            [RenameIntent {
                source: source.clone(),
                destination: source.join("child"),
            }],
        )
        .unwrap_err();
        assert!(error.contains("nested inside its own source"));
    }

    #[test]
    fn duplicate_end_state_is_rejected_case_insensitively() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let error = plan_rename_transaction(
            temp.path(),
            [
                RenameIntent { source: a, destination: temp.path().join("same") },
                RenameIntent { source: b, destination: temp.path().join("SAME") },
            ],
        )
        .unwrap_err();
        assert!(error.contains("end-state collision"));
    }

    #[test]
    fn nested_sources_stage_ancestors_first_for_rebasing() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        let plan = plan_rename_transaction(
            temp.path(),
            [
                RenameIntent {
                    source: parent.clone(),
                    destination: temp.path().join("renamed-parent"),
                },
                RenameIntent {
                    source: child.clone(),
                    destination: temp.path().join("renamed-child"),
                },
            ],
        )
        .unwrap();
        let order = plan.staging_order();
        assert_eq!(plan.entries[order[0]].source, parent);
        assert_eq!(plan.entries[order[1]].source, child);
    }
}

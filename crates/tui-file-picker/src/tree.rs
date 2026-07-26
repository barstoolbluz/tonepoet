use crate::state::{home_dir, TreeNode};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const MAX_HAS_CHILD_CACHE_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryFingerprint {
    modified: Option<SystemTime>,
    len: u64,
    readonly: bool,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime_sec: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl DirectoryFingerprint {
    fn read(dir: &Path) -> Option<Self> {
        fs::metadata(dir).ok().map(|meta| Self {
            modified: meta.modified().ok(),
            len: meta.len(),
            readonly: meta.permissions().readonly(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
            #[cfg(unix)]
            ctime_sec: meta.ctime(),
            #[cfg(unix)]
            ctime_nsec: meta.ctime_nsec(),
        })
    }
}

type HasChildCacheKey = (PathBuf, bool);

struct HasChildCacheEntry {
    fingerprint: Option<DirectoryFingerprint>,
    verdict: bool,
    last_used: u64,
}

#[derive(Default)]
struct HasChildDirectoriesCache {
    entries: HashMap<HasChildCacheKey, HasChildCacheEntry>,
    tick: u64,
}

static HAS_CHILD_DIRECTORIES_CACHE: OnceLock<Mutex<HasChildDirectoriesCache>> = OnceLock::new();

/// Build initial tree nodes using the same filesystem discovery rules as the
/// file picker, with an explicit hidden-directory policy for hosts such as the
/// Browse screen.
pub fn initial_tree_nodes_with_hidden(current_dir: &Path, show_hidden: bool) -> Vec<TreeNode> {
    let mut nodes = Vec::new();
    if let Some(home) = home_dir() {
        nodes.push(TreeNode {
            name: home
                .file_name()
                .and_then(OsStr::to_str)
                .map(str::to_string)
                .unwrap_or_else(|| "Home".to_string()),
            path: home.clone(),
            depth: 0,
            expanded: current_dir.starts_with(&home),
            has_children: has_child_directories(&home, show_hidden),
        });
    }

    let root = filesystem_root();
    if !nodes.iter().any(|node| node.path == root) {
        nodes.push(TreeNode {
            name: "Filesystem".to_string(),
            path: root,
            depth: 0,
            expanded: false,
            has_children: true,
        });
    }

    if let Some(network) = network_root() {
        nodes.push(TreeNode {
            name: "Network".to_string(),
            path: network.clone(),
            depth: 0,
            expanded: false,
            has_children: has_child_directories(&network, show_hidden),
        });
    }

    expand_ancestors_for_current_dir(&mut nodes, current_dir, show_hidden);
    nodes
}

pub fn refresh_tree_children(nodes: &mut Vec<TreeNode>, dir: &Path, show_hidden: bool) {
    let Some(index) = nodes.iter().position(|node| same_path(&node.path, dir)) else {
        return;
    };
    if !nodes[index].expanded {
        return;
    }
    let depth = nodes[index].depth;
    let insert_at = index + 1;
    let remove_end = nodes[insert_at..]
        .iter()
        .position(|node| node.depth <= depth)
        .map(|pos| insert_at + pos)
        .unwrap_or(nodes.len());
    nodes.drain(insert_at..remove_end);
    let children = child_directories(dir, depth + 1, show_hidden);
    nodes.splice(insert_at..insert_at, children);
}

/// Expand tree nodes so ancestors of `current_dir` are materialized and open.
pub fn expand_tree_to_path(nodes: &mut Vec<TreeNode>, current_dir: &Path, show_hidden: bool) {
    expand_ancestors_for_current_dir(nodes, current_dir, show_hidden);
}

fn expand_ancestors_for_current_dir(nodes: &mut Vec<TreeNode>, current_dir: &Path, show_hidden: bool) {
    let mut index = 0usize;
    while index < nodes.len() {
        if current_dir.starts_with(&nodes[index].path) && nodes[index].has_children {
            nodes[index].expanded = true;
            let base = nodes[index].path.clone();
            refresh_tree_children(nodes, &base, show_hidden);
        }
        index += 1;
    }
}

pub fn child_directories(dir: &Path, depth: usize, show_hidden: bool) -> Vec<TreeNode> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string());
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) || path.is_dir() {
            children.push(TreeNode {
                has_children: has_child_directories(&path, show_hidden),
                expanded: false,
                depth,
                name,
                path,
            });
        }
    }
    children.sort_by_cached_key(|node| node.name.to_ascii_lowercase());
    children
}

fn has_child_directories(dir: &Path, show_hidden: bool) -> bool {
    let fingerprint = DirectoryFingerprint::read(dir);
    let key = (dir.to_path_buf(), show_hidden);
    let cache = HAS_CHILD_DIRECTORIES_CACHE.get_or_init(|| {
        Mutex::new(HasChildDirectoriesCache::default())
    });

    if let Ok(mut cache) = cache.lock() {
        cache.tick = cache.tick.wrapping_add(1);
        let tick = cache.tick;
        if let Some(entry) = cache.entries.get_mut(&key) {
            if entry.fingerprint == fingerprint {
                entry.last_used = tick;
                return entry.verdict;
            }
        }
    }

    let verdict = has_child_directories_uncached(dir, show_hidden);

    if let Ok(mut cache) = cache.lock() {
        cache.tick = cache.tick.wrapping_add(1);
        let tick = cache.tick;
        if cache.entries.len() >= MAX_HAS_CHILD_CACHE_ENTRIES {
            evict_old_has_child_cache_entries(&mut cache);
        }
        // `false` is safe to memoize only for this directory identity. Permission
        // or child changes alter the fingerprint on supported platforms; the
        // cache is in-memory only, so transient filesystem states cannot poison
        // persistent Browse behavior.
        cache.entries.insert(
            key,
            HasChildCacheEntry {
                fingerprint,
                verdict,
                last_used: tick,
            },
        );
    }

    verdict
}

fn evict_old_has_child_cache_entries(cache: &mut HasChildDirectoriesCache) {
    if cache.entries.len() < MAX_HAS_CHILD_CACHE_ENTRIES {
        return;
    }

    // This cache is an interactivity optimization, not authoritative state.
    // Trim the oldest half only when the cap is reached so one unusually large
    // tree walk does not discard every hot ancestor and sibling entry.
    let target_len = MAX_HAS_CHILD_CACHE_ENTRIES / 2;
    let remove_count = cache.entries.len().saturating_sub(target_len);
    let mut by_age: Vec<(HasChildCacheKey, u64)> = cache
        .entries
        .iter()
        .map(|(key, entry)| (key.clone(), entry.last_used))
        .collect();
    by_age.sort_by_key(|(_, last_used)| *last_used);
    for (key, _) in by_age.into_iter().take(remove_count) {
        cache.entries.remove(&key);
    }
}

fn has_child_directories_uncached(dir: &Path, show_hidden: bool) -> bool {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return false;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) || entry.path().is_dir() {
            return true;
        }
    }
    false
}

pub fn filesystem_root() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemDrive")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"))
    } else {
        PathBuf::from("/")
    }
}

fn network_root() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/Volumes"]
    } else if cfg!(windows) {
        &[]
    } else {
        &["/mnt", "/media", "/run/media"]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_dir())
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let a_canon = a.canonicalize();
    let b_canon = b.canonicalize();
    matches!((a_canon, b_canon), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static TEST_CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_test_cache() -> std::sync::MutexGuard<'static, ()> {
        TEST_CACHE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("test cache lock")
    }

    fn clear_has_child_directories_cache() {
        let cache = HAS_CHILD_DIRECTORIES_CACHE.get_or_init(|| {
            Mutex::new(HasChildDirectoriesCache::default())
        });
        let mut cache = cache.lock().expect("cache lock");
        cache.entries.clear();
        cache.tick = 0;
    }

    #[test]
    fn tree_contains_current_dir_ancestor() {
        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");
        let mut nodes = vec![TreeNode {
            path: temp.path().to_path_buf(),
            name: "temp".to_string(),
            depth: 0,
            expanded: true,
            has_children: true,
        }];
        refresh_tree_children(&mut nodes, temp.path(), false);
        assert!(nodes.iter().any(|node| node.path == child));
    }

    #[test]
    fn has_child_cache_keeps_hidden_policy_separate() {
        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".hidden-child")).expect("hidden child");

        assert!(!has_child_directories(temp.path(), false));
        assert!(has_child_directories(temp.path(), true));

        let visible = child_directories(temp.path(), 1, false);
        let hidden_allowed = child_directories(temp.path(), 1, true);
        assert!(visible.is_empty());
        assert_eq!(hidden_allowed.len(), 1);
        assert_eq!(hidden_allowed[0].name, ".hidden-child");
    }

    #[test]
    fn stale_cache_with_same_mtime_but_different_identity_is_ignored() {
        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(temp.path().join("child")).expect("child");

        let mut stale_fingerprint =
            DirectoryFingerprint::read(temp.path()).expect("directory fingerprint");
        stale_fingerprint.len = stale_fingerprint.len.wrapping_add(1);

        let cache = HAS_CHILD_DIRECTORIES_CACHE.get_or_init(|| {
            Mutex::new(HasChildDirectoriesCache::default())
        });
        let mut cache = cache.lock().expect("cache lock");
        cache.entries.insert(
            (temp.path().to_path_buf(), false),
            HasChildCacheEntry {
                fingerprint: Some(stale_fingerprint),
                verdict: false,
                last_used: 1,
            },
        );
        cache.tick = 1;
        drop(cache);

        assert!(has_child_directories(temp.path(), false));
    }

    #[test]
    fn has_child_cache_eviction_retains_recent_entries() {
        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let mut cache = HasChildDirectoriesCache::default();
        for idx in 0..MAX_HAS_CHILD_CACHE_ENTRIES {
            cache.entries.insert(
                (PathBuf::from(format!("cache-entry-{idx}")), false),
                HasChildCacheEntry {
                    fingerprint: None,
                    verdict: idx % 2 == 0,
                    last_used: idx as u64,
                },
            );
        }

        evict_old_has_child_cache_entries(&mut cache);

        assert_eq!(cache.entries.len(), MAX_HAS_CHILD_CACHE_ENTRIES / 2);
        assert!(!cache
            .entries
            .contains_key(&(PathBuf::from("cache-entry-0"), false)));
        assert!(cache.entries.contains_key(&cache_key(MAX_HAS_CHILD_CACHE_ENTRIES - 1)));
    }

    fn cache_key(idx: usize) -> HasChildCacheKey {
        (PathBuf::from(format!("cache-entry-{idx}")), false)
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_directory_is_reported_as_tree_child() {
        use std::os::unix::fs::symlink;

        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        let link = temp.path().join("linked");
        fs::create_dir(&target).expect("target dir");
        symlink(&target, &link).expect("directory symlink");

        let children = child_directories(temp.path(), 1, false);
        assert!(children
            .iter()
            .any(|node| node.name == "linked" && node.path == link));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_treated_as_leaf_when_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = lock_test_cache();
        clear_has_child_directories_cache();
        let temp = tempfile::tempdir().expect("tempdir");
        let denied = temp.path().join("denied");
        fs::create_dir(&denied).expect("denied dir");
        fs::create_dir(denied.join("child")).expect("child");

        let original_permissions = fs::metadata(&denied).expect("metadata").permissions();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000))
            .expect("remove permissions");

        let read_dir_failed = fs::read_dir(&denied).is_err();
        let verdict = has_child_directories(&denied, false);
        let children = child_directories(&denied, 1, false);

        fs::set_permissions(&denied, original_permissions).expect("restore permissions");

        if read_dir_failed {
            assert!(!verdict);
            assert!(children.is_empty());
        }
    }
}

use crate::state::{home_dir, TreeNode};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

pub fn initial_tree_nodes(current_dir: &Path) -> Vec<TreeNode> {
    initial_tree_nodes_with_hidden(current_dir, false)
}

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
    children.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    children
}

fn has_child_directories(dir: &Path, show_hidden: bool) -> bool {
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

    #[test]
    fn tree_contains_current_dir_ancestor() {
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
}

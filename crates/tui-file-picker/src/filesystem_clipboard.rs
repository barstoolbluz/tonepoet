//! Retained filesystem clipboard transaction metadata.

use crate::FilePickerClipboardMode;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilesystemClipboard {
    mode: FilePickerClipboardMode,
    paths: Vec<PathBuf>,
}

impl FilesystemClipboard {
    pub fn new<I>(mode: FilePickerClipboardMode, paths: I) -> Option<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        // Keep only independent roots. A parent path subsumes any selected
        // descendants; retaining both would make a cut/delete sequence depend
        // on iteration order after the parent has moved or disappeared.
        let mut normalized: Vec<PathBuf> = Vec::new();
        for path in paths {
            if normalized.iter().any(|existing| path.starts_with(existing)) {
                continue;
            }
            normalized.retain(|existing| !existing.starts_with(&path));
            normalized.push(path);
        }
        (!normalized.is_empty()).then_some(Self { mode, paths: normalized })
    }

    pub fn mode(&self) -> FilePickerClipboardMode {
        self.mode
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|candidate| candidate == path)
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Plain-text projection mirrored to the host clipboard.
    ///
    /// The host clipboard is authoritative for each user-initiated paste. This
    /// projection lets a retained transaction recover Cut/Copy semantics only
    /// while the host text still matches it exactly, and is also portable for
    /// pasting the selected paths into a shell, editor, or file manager.
    pub fn text_projection(&self) -> String {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether host clipboard text still names this exact transaction.
    /// Native clipboard readers differ on whether they retain one trailing
    /// newline, so accept that transport-level difference without weakening
    /// path or ordering equality.
    pub fn matches_host_text(&self, text: &str) -> bool {
        normalize_host_clipboard_text(text) == self.text_projection()
    }
}

fn normalize_host_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .trim_end_matches(|ch| matches!(ch, '\r' | '\n'))
        .to_string()
}

/// Resolve one host-clipboard snapshot into a filesystem operation.
///
/// If the text still matches Tonepoet's retained transaction exactly, retain
/// its Cut/Copy mode and retry identity. Otherwise the host clipboard is the
/// authority and any valid path list becomes a fresh Copy transaction.
pub fn filesystem_clipboard_from_host_text(
    text: &str,
    retained: Option<&FilesystemClipboard>,
) -> Result<FilesystemClipboard, String> {
    if let Some(retained) = retained {
        if retained.matches_host_text(text) {
            return Ok(retained.clone());
        }
    }

    let normalized = normalize_host_clipboard_text(text);
    if normalized.is_empty() {
        return Err("terminal clipboard is empty".to_string());
    }

    let mut paths = Vec::new();
    for line in normalized.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            return Err("terminal clipboard contains an empty path line".to_string());
        }
        let path = PathBuf::from(line);
        if !path.exists() {
            return Err(format!(
                "terminal clipboard path does not exist: {}",
                path.display()
            ));
        }
        paths.push(path);
    }

    FilesystemClipboard::new(FilePickerClipboardMode::Copy, paths)
        .ok_or_else(|| "terminal clipboard contains no pasteable filesystem paths".to_string())
}

/// Remap a currently viewed path after a successful cut/paste operation.
///
/// `destinations` must be in the same order as [`FilesystemClipboard::paths`],
/// as guaranteed by `paste_filesystem_clipboard`. Returns `None` for copy
/// operations or when the viewed path was not inside a moved root.
pub fn remap_path_after_cut(
    current: &Path,
    clipboard: &FilesystemClipboard,
    destinations: &[PathBuf],
) -> Option<PathBuf> {
    if clipboard.mode() != FilePickerClipboardMode::Cut {
        return None;
    }
    clipboard
        .paths()
        .iter()
        .zip(destinations)
        .find_map(|(source, destination)| {
            current
                .strip_prefix(source)
                .ok()
                .map(|suffix| destination.join(suffix))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_selection_subsumes_descendants_regardless_of_input_order() {
        let root = PathBuf::from("/music/album");
        let child = root.join("disc-1").join("track.flac");

        let parent_last = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![child.clone(), root.clone()],
        )
        .expect("clipboard");
        assert_eq!(parent_last.paths(), &[root.clone()]);

        let parent_first = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![root.clone(), child],
        )
        .expect("clipboard");
        assert_eq!(parent_first.paths(), &[root]);
    }

    #[test]
    fn remaps_viewed_descendant_after_cut() {
        let source = PathBuf::from("/music/album");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let destination = PathBuf::from("/archive/album");

        assert_eq!(
            remap_path_after_cut(
                &source.join("disc-1"),
                &clipboard,
                std::slice::from_ref(&destination),
            ),
            Some(destination.join("disc-1"))
        );
    }

    #[test]
    fn copy_never_remaps_viewed_path() {
        let source = PathBuf::from("/music/album");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Copy,
            vec![source.clone()],
        )
        .expect("clipboard");
        assert_eq!(
            remap_path_after_cut(
                &source,
                &clipboard,
                &[PathBuf::from("/archive/album")],
            ),
            None
        );
    }

    #[test]
    fn text_projection_preserves_clipboard_order_and_uses_newline_delimiters() {
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Copy,
            vec![
                PathBuf::from("/music/disc 1/01.flac"),
                PathBuf::from("/music/disc 2/02.flac"),
            ],
        )
        .expect("clipboard");

        assert_eq!(
            clipboard.text_projection(),
            "/music/disc 1/01.flac\n/music/disc 2/02.flac"
        );
    }

    #[test]
    fn host_text_preserves_cut_only_while_projection_still_matches() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        std::fs::create_dir(&source).expect("source");
        let retained = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");

        let matched = filesystem_clipboard_from_host_text(
            &format!("{}\n", source.display()),
            Some(&retained),
        )
        .expect("matching host clipboard");
        assert_eq!(matched.mode(), FilePickerClipboardMode::Cut);

        let other = temp.path().join("other");
        std::fs::create_dir(&other).expect("other");
        let replaced = filesystem_clipboard_from_host_text(
            &other.display().to_string(),
            Some(&retained),
        )
        .expect("external host clipboard");
        assert_eq!(replaced.mode(), FilePickerClipboardMode::Copy);
        assert_eq!(replaced.paths(), &[other]);
    }

    #[test]
    fn host_text_rejects_nonexistent_external_paths_instead_of_using_stale_transaction() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        std::fs::create_dir(&source).expect("source");
        let retained = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source],
        )
        .expect("clipboard");

        let error = filesystem_clipboard_from_host_text(
            "/definitely/not/a/tonepoet/path",
            Some(&retained),
        )
        .expect_err("stale transaction must not win");
        assert!(error.contains("does not exist"));
    }
}

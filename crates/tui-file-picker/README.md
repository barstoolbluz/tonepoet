# tui-file-picker

Reusable `ratatui` + `crossterm` file-picker/file-browser overlay.

The crate is intentionally application-neutral. It does not know about artwork,
metadata, audio files, or any host application's button map. Hosts pass a
configuration, render the picker each frame, feed key/mouse input back to the
state, and interpret `FilePickerAction::Selected(PathBuf)` in their own domain.

```rust
use std::path::PathBuf;
use tui_file_picker::{FileOperationPolicy, FilePickerAction, FilePickerConfig, FilePickerFilter, FilePickerState};

let operation_policy = FileOperationPolicy {
    allow_delete: false,
    ..FileOperationPolicy::default()
};

let mut picker = FilePickerState::new(FilePickerConfig {
    start_dir: PathBuf::from("~"),
    filter: FilePickerFilter::Images,
    title: "Select artwork".to_string(),
    operation_policy,
    ..FilePickerConfig::default()
});

// Optional host-provided filesystem fact. The crate intentionally avoids
// non-std disk-space dependencies.
picker.set_free_space_bytes(Some(142 * 1024 * 1024 * 1024));

// In the draw loop:
// picker.render(frame, area);

// In the input loop:
// match picker.handle_key(key_event) {
//     FilePickerAction::Selected(path) => { /* host owns side effects */ }
//     FilePickerAction::Cancelled => { /* close overlay */ }
//     FilePickerAction::None => {}
// }
```

## Design points

- Zero host-application dependencies.
- No global state and no process clipboard coupling.
- `FilePickerState` keeps invariants private; hosts use methods and explicit policies.
- Back/forward/up navigation is deterministic and idempotent.
- Directory listings are refreshed explicitly and sorted folder-first.
- Direct path entry supports `~` expansion and relative paths.
- The toolbar, address action, and confirmation actions render as themed button pills rather than label-like text.
- Filtered pickers still show directories so users can navigate to matching descendants.
- Mouse hit regions are crate-owned and available to host applications via `hit_regions()`.
- Mouse handling is tied to the last render pass; a non-default input area must match the last rendered area.
- File rows support double-click activation by interpreting two consecutive clicks on the same row within the double-click window as open/select.
- The status bar reports item count, total visible size, and host-provided free space or an explicit unavailable marker; expected modal states do not masquerade as errors.
- New File/New Folder prompts for a name before creating anything.
- File-operation behavior is controlled through `FileOperationPolicy`, including operation allow/deny switches, symlink-copy, cross-device cut, and delete behavior.
- Copy operations stage into the destination directory and clean up partial staged copies on failure.
- Symlinks are rejected by default. Following targets is opt-in.
- Delete is two-step and defaults to files plus empty directories only; recursive deletion is opt-in. The confirmation dialog defaults keyboard focus to Cancel, supports Tab/Left/Right focus movement, and keeps Delete/Cancel mouse-clickable.

The layout follows the mock file-manager overlay: toolbar, address bar, folder-tree pane, file-list table, status bar, File Operations dropdown with a New submenu, and a properties popup.

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

// In the draw loop, without terminal image previews:
// picker.render(frame, area);

// In the input loop:
// match picker.handle_key(key_event) {
//     FilePickerAction::Selected(path) => { /* host owns side effects */ }
//     FilePickerAction::Cancelled => { /* close overlay */ }
//     FilePickerAction::None => {}
// }
```


## Image previews

The `image-preview` feature is enabled by default. Hosts that only need the
file-manager UI may keep using `picker.render(frame, area)`. That path still
shows the preview pane's pending/error/placeholder text, but it intentionally
does not perform terminal graphics protocol detection or image protocol
creation inside the reusable crate.

For actual terminal image previews, own a single `ratatui_image::picker::Picker`
in the host application, render with `render_with_image_picker(...)`, and call
`prepare_image_preview_protocol_with_retransmit_generation(...)` from the host
update loop. This keeps terminal capability detection startup-owned and keeps
disk I/O, image decode, and protocol creation out of the render path.

Ghostty/Kitty note: Ghostty uses Kitty graphics. Mouse motion can damage the
terminal-side graphics layer even when ratatui's text buffer is unchanged. Do
not try to repair that by appending raw ANSI bytes to cell symbols or by
mutating every frame. Instead, increment a **separate**, rate-limited Kitty
`retransmit_generation` and pass it to
`prepare_image_preview_protocol_with_retransmit_generation(...)`. The picker
will rebuild the cached `StatefulProtocol` from already-decoded pixels only for
Kitty, and only when that generation changes. Non-Kitty protocols ignore the
retransmit generation.

Tmux/byobu note: do not run Kitty protocol detection through tmux. Force
`ProtocolType::Halfblocks` when `TMUX` or a tmux `TERM_PROGRAM` is present;
otherwise a Kitty-capable outer terminal can send protocol responses that tmux
prints into the pane as text.

```rust,no_run
use std::time::{Duration, Instant};
use ratatui_image::picker::{Picker, ProtocolType};

let mut image_picker = Picker::from_termios().unwrap_or_else(|_| Picker::new((8, 16)));
if std::env::var_os("TMUX").is_some() || std::env::var("TERM_PROGRAM").as_deref() == Ok("tmux") {
    image_picker.protocol_type = ProtocolType::Halfblocks;
} else {
    image_picker.guess_protocol();
}
let mut image_picker_generation = 0usize;
let mut kitty_retransmit_generation = 0usize;
let mut last_kitty_retransmit = None::<Instant>;

// In the update loop, before drawing, so mouse-damage retransmits are ready
// for the frame about to be rendered. Also call it once after drawing so first
// layout/geometry discovery can prepare the next frame.
picker.prepare_image_preview_protocol_with_retransmit_generation(
    &mut image_picker,
    image_picker_generation,
    kitty_retransmit_generation,
);

// In the draw loop:
// terminal.draw(|frame| {
//     picker.render_with_image_picker(
//         frame,
//         area,
//         &mut image_picker,
//         image_picker_generation,
//     );
// })?;
//
// picker.prepare_image_preview_protocol_with_retransmit_generation(
//     &mut image_picker,
//     image_picker_generation,
//     kitty_retransmit_generation,
// );

// On mouse motion/drag/click events that can damage Kitty graphics:
if image_picker.protocol_type() == ProtocolType::Kitty {
    let now = Instant::now();
    let elapsed = last_kitty_retransmit
        .map(|last| now.duration_since(last) >= Duration::from_millis(33))
        .unwrap_or(true);
    if elapsed {
        kitty_retransmit_generation = kitty_retransmit_generation.saturating_add(1);
        last_kitty_retransmit = Some(now);
    }
}

// When the terminal is resized or cell metrics/protocol assumptions change:
image_picker = Picker::from_termios().unwrap_or_else(|_| Picker::new((8, 16)));
if std::env::var_os("TMUX").is_some() || std::env::var("TERM_PROGRAM").as_deref() == Ok("tmux") {
    image_picker.protocol_type = ProtocolType::Halfblocks;
} else {
    image_picker.guess_protocol();
}
image_picker_generation = image_picker_generation.saturating_add(1);
last_kitty_retransmit = None;
picker.invalidate_image_preview_cache();
```

Internally the picker keeps desired render geometry/generation separate from
encoded protocol geometry/generation. Mouse-damage retransmit generation is
separate from resize/cell-metric protocol generation, so a Ghostty/Kitty repair
does not make stale resize state appear valid.

Applications that compile with `--no-default-features` do not build the image
preview code paths or pull in `ratatui-image` / `image`.

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

## File task conflict semantics

The reusable file-task progress overlay owns only UI state, rendering, input
mapping, and semantic conflict actions. Host applications remain responsible for
all filesystem policy and side effects.

When a host detects an existing destination or another file-task conflict, it may
send `FileTaskProgressUpdate::ShowConflict` with a `ConflictPromptState`. The
overlay keeps the normal progress display visible, renders the conflict prompt,
and emits `FileTaskUserAction::ChooseConflictResolution` when the user chooses a
resolution. The response carries the host-provided conflict request id so stale
responses can be ignored by the host.

Supported reusable conflict actions are:

- `Overwrite`: replace an existing destination for this item after explicit host
  confirmation. For file copies, hosts should write to a temporary file first and
  only replace the final destination after the copy is complete.
- `Merge`: merge a source directory into an existing destination directory. This
  is intentionally distinct from overwrite so directory merges are never hidden
  behind a vague replacement label.
- `Skip`: leave the existing destination untouched, account this item as skipped,
  and continue the job.
- `Rename`: ask the host to choose a deterministic non-conflicting destination,
  such as `name copy.ext`, `name copy 2.ext`, and so on.
- `Abort`: stop the whole job. Hosts should clean up only temporary files created
  by the current operation and must not remove pre-existing destinations.

`Apply to all conflicts` is intended for overwrite/merge/skip policies only.
Rename-all should be avoided unless the host can guarantee deterministic,
non-surprising names. For copy-then-delete moves, conflict resolution occurs
during the copy phase; source cleanup must preserve skipped or failed sources and
should only remove source entries that the host has verified as successfully
moved.

## File-task progress, conflict, and finalization semantics

The progress overlay is a reusable UI/state component. Hosts feed it snapshots,
conflict prompts, and terminal summaries; it renders state and emits semantic
user intent such as pause, resume, skip, abort, overwrite, merge, rename, or
apply-to-all. The crate never mutates the filesystem and does not decide file
operation policy.

Conflict handling is intentionally split at the crate/host boundary. The crate
owns `ConflictPromptState`, conflict rendering, focus/input handling, and
semantic `ConflictResolution` values. The host owns conflict detection,
filesystem mutation, race-safe finalization, move cleanup, and stale-response
validation using job/session and conflict request identifiers.

For copy/move hosts, non-overwrite file finalization must be no-clobber: a
completed temporary file is published without replacing an existing path. The
host implementation in this bundle first tries a same-directory hard-link
publish, which fails if the destination exists, and falls back to reserving the
final path with `create_new(true)`. Both strategies prevent overwriting a path
that appears after an earlier conflict check. Overwrite finalization is a
separate explicit operation used only after force/overwrite policy and rejects
file replacement when the destination is a directory. On Unix, replacing a
symlink path replaces the symlink itself rather than following it; on Windows,
the existing file/symlink is removed deliberately before rename because standard
rename does not replace it.

Symlink copies follow the same policy boundary. Non-overwrite symlink creation
uses a create operation that fails if the destination already exists. Overwrite
uses an explicit temporary symlink plus replacement step. Skip and abort remove
only temporary paths created by the current operation and never remove or
truncate a pre-existing destination.

Recursive copy/move is planned before mutation. Planning builds a tree of source
nodes, target nodes, file kind, byte length where known, aggregate subtree stats,
and child nodes for directories. Execution consumes that plan tree instead of
remeasuring subtrees during copy. If the filesystem changes after planning, the
host treats the affected node as an operation error or late conflict rather than
silently changing progress totals.

Planning remains interruptible. Abort stops before destination mutation. Pause
and resume are honored during the scan. Skip during planning applies to the
currently displayed node/subtree, not to an ancestor that merely contains it;
skipped unknown-size nodes are accounted as skipped/unknown without adding fake
transferred bytes.

Directory merge is distinct from overwrite. Merge means copying planned children
into an existing destination directory and preserving existing destination
entries unless child-level conflict policy says otherwise. Merge increments the
`merged` counter. Overwrite is reserved for deliberate replacement of an
existing file/symlink path and increments `overwritten`.

Move cleanup is conservative. For copy-then-delete moves, sources are removed
only after the corresponding planned tree has copied and verified successfully.
Skipped, failed, or partially copied sources remain in place, and terminal
summaries distinguish completed, skipped, overwritten, renamed, merged, failed,
and not-attempted work.

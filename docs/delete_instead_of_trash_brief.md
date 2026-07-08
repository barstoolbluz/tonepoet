# Replace "Move to Trash" with Permanent Delete

## What the User Wants

Replace the "Move to Trash" file operation with a permanent delete. On Linux, the `trash` crate creates `.Trash-1000` directories at the root of the volume where the files live, which is unwanted. The user wants files deleted immediately and permanently.

## Current Implementation

- **Dependency:** `trash = "4"` in `Cargo.toml`
- **The actual delete call:** `trash::delete(path)` at `keybindings.rs:23327`, inside `ConfirmAction::TrashSelection(paths)` handler
- **Context menu entry:** `"Move to Trash"` label with `ContextAction::MoveToTrash` at `context_menu.rs:348`
- **Context menu handler:** `context_menu.rs:1044` — dispatches to `command::execute_command(app, Command::Delete, tx)` for filesystem entries, or `start_browse_archive_entry_delete` for archive entries
- **Command:** `:del` described as "Move selected to trash" in `help.rs:84`
- **Confirmation dialog:** `"Move {} item(s) to trash?"` at `command.rs:4806`
- **Delete key binding:** `keybindings.rs:3098` — Delete key triggers the same trash flow for filesystem entries
- **ConfirmAction variant:** `TrashSelection(Vec<PathBuf>)` at `app.rs:7493`

## Constraints

- The confirmation dialog is important — keep it. Permanent delete is more destructive than trash, so the confirmation must be clear about what's happening.
- Archive entry deletion (inside archive browse mode) is a separate code path (`start_browse_archive_entry_delete`) that stages deletions for later repackaging. That path should not be changed.
- The `trash` crate dependency can be removed from `Cargo.toml` after the change.

## Files in This Bundle

- `src/tui/keybindings.rs` — `ConfirmAction::TrashSelection` handler, Delete key binding
- `src/tui/context_menu.rs` — `ContextAction::MoveToTrash`, menu label, handler dispatch
- `src/tui/command.rs` — `:del` command, confirmation dialog message
- `src/tui/app.rs` — `ConfirmAction::TrashSelection` variant
- `src/tui/help.rs` — `:del` help text
- `Cargo.toml` — `trash` dependency

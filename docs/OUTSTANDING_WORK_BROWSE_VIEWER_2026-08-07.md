# Outstanding work — Browse UX + file viewer (2026-08-07)

User-requested additions to the bill of work. Not yet briefed. Anchors recon'd against
`hardening` @ f3218c9. Each becomes its own reasoning-model brief when scheduled.

---

## 1. Rename in Browse resets the view to the top — preserve cursor + scroll

**Symptom.** Renaming a file/folder inline in the Browse view causes the list to reload and
the highlight/selection bar to jump to the TOP of the current folder, losing the user's
place. On folders with thousands of entries this is a major PITA — you rename one item deep
in a list and have to scroll all the way back.

**Desired.** After an inline rename commits, the view stays put: the highlight lands on the
RENAMED entry (at its new sorted position) and the scroll offset is preserved (or adjusted
minimally to keep the renamed entry visible). No jump to top.

**Anchors / mechanism.**
- The machinery already exists: `BrowseState.cursor_restore_target: Option<String>`
  (src/tui/browse.rs:2738) restores the cursor to a NAMED entry after a refresh, and is used
  after file-task completions (browse.rs:~4553, `refresh_after_file_task_nonblocking`
  ~3675). The **rename path evidently does not set it to the new name** (and/or resets
  `scroll = 0`).
- Rename commit: `commit_browse_inline_edit` (keybindings.rs:3672) →
  `BrowseInlineCommit::Rename` → `commit_browse_rename` (keybindings.rs ~3678 area). There
  are many `state.scroll = 0` / cursor resets in the browse refresh paths
  (keybindings.rs ~7989-8536) — the rename refresh likely goes through one.
- **Fix direction:** on rename commit, set `cursor_restore_target = Some(new_name)` before
  the folder refresh, and preserve `scroll` instead of zeroing it (or recompute from the
  restored cursor). Verify the same for the sequential-rename (Tab/BackTab) flow
  (`sequential_inline_rename_target`, keybindings.rs ~3699) so tabbing through renames
  doesn't scroll-jump either.
- Likely relates to the parked [browse_ux_hardening_track] leftovers.

---

## 2. `bat` as the default read-only viewer for `.log` (config-toggle, opt-in to own editor)

**Desired.** `.log` files open in **`bat`** by default (nicer styling than $EDITOR), strictly
read-only. A `config.toml` option lets the user opt OUT (use their own editor). Default =
bat; opt-in = own editor. Keep the existing read-only code paths for real editors.

**Anchors / mechanism.**
- `bat` is ALREADY recognized as an inherently read-only viewer in
  `src/tui/external_editor.rs:145` (`"less" | "more" | "bat" | "cat" => None`), and the
  read-only view path (`open_in_viewer`-style, external_editor.rs ~127-181) already picks a
  viewer and applies `-R`/`-v` for vim/nano. So wiring bat as the .log viewer is small.
- `.log` files are already treated as non-editable / read-only elsewhere (browse.rs:12130
  "excludes `.log` files (rip integrity records should not be modified)"; command.rs:3261
  "View a text file in read-only mode" vs 3263 "Edit a text file (not .log files)").
- **`bat` must be added to the nix flake dev shell** (flake.nix `buildInputs` / the tools
  list) and the packaged PATH. It is NOT currently a dependency.
- **Config:** add a `[viewer]`/`[ui]` option, e.g. `log_viewer = "bat" | "editor"` (default
  `bat`), serde-defaulted. Follow the `[metadata] sidecar_save_with_warnings` /
  `[file_operations]` precedent in src/config.rs.
- **DOCUMENT IT TWICE (per user):** a doc comment on the config field/code path AND a
  commented note in the generated `config.toml`, both flagging `TODO(config-screen)` — this
  becomes an explicit toggle when the full Config screen is built (same pattern as
  `[ui] manage_tmux_clipboard`, see [[tmux-clipboard-config-exposure]]).

---

## 3. Default "open" associations: Enter on `.log` `.txt` `.nfo` `.md` `.cue`

**Desired.** Highlighting a file with one of these extensions and pressing **Enter** opens
it: `.log` → **`bat`** (read-only, per #2); `.txt` / `.nfo` / `.md` / `.cue` → the default
editor (read-write, or read-only where appropriate — `.cue` may warrant view). A brilliant
UX affordance — today Enter on these files does nothing useful.

**Anchors / mechanism.**
- Browse Enter dispatch: keybindings.rs:6251 (`(KeyCode::Enter, KeyModifiers::NONE)`) is the
  main handler (directory = navigate in; file = currently no open action). Add an
  extension→action map for the recognized text types.
- Reuse `external_editor::open_in_editor` (command.rs:7608 / external_editor.rs:105) for the
  editable types and the read-only viewer path for `.log` (and `.cue` if we want view-only).
- Respect #2's config (log→bat vs editor). Keep it extensible — this is effectively a small
  file-association table; structure it so more extensions/handlers can be added later
  (candidate for the same future Config screen).
- Byobu-safe: Enter only; no new chords required.

---

## Cross-cutting
- Items 2 and 3 share the viewer/association layer — could be one brief; item 1 is
  independent (Browse refresh). Sequence at the user's discretion.
- Also still open from the untaggable-carrier work:
  `docs/BRIEF_transfer_tags_untaggable_2026-08-07.md` (Transfer Tags both directions) — a
  separate, already-drafted brief awaiting dispatch.

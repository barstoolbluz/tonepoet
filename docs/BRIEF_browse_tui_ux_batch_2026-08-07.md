# tonepoet — Browse/TUI UX batch: transfer-tags + rename-scroll + log viewer (2026-08-07)

You are starting **fresh**; everything you need is in this bundle. Outcomes + guardrails;
diagnosis is evidence, not prescription — you choose HOW.

**Project:** tonepoet (Rust TUI, ratatui 0.26 / crossterm 0.27, tokio, edition 2021),
version 0.4.6 — do not bump. Gate `cargo test --workspace --no-fail-fast` green ×2. Anchors
recon'd + audited against `hardening` @ f3218c9 (source unchanged since). Byobu/tmux input
rules throughout: no F-keys; no Shift+Click/Shift+arrows/Ctrl+Space as the ONLY path to
anything; keep existing chords.

Four independent workstreams, one delivery. A (transfer tags) is lodestar-governed and the
heaviest; B/C are Browse UX. Do them all; they share the Browse/TUI subsystem.

---

## A. Transfer Tags for untaggable carriers — BOTH directions (cue authority)

**Problem.** TUI "Transfer tags" fails for folders of **untaggable carriers** (`.dff`, and by
the same mechanism any lofty-unsupported format) that have a sidecar `.cue`, in BOTH
directions: transferring FROM such an album errors
`tag transfer source read failed for '…/01 - ….dff' (9 of 9 sources unreadable)`, and
transferring TO such an album cannot land tags on the untaggable carriers. Repro:
right-click `~/torrents/Michael Jackson - Thriller. 1984 Japan/` → Properties → Tags →
Transfer tags → Canonical → pick a source → the read error. User: neither "to a
folder/file(s)" nor "from a folder/file(s)" works.

The metadata EDITOR (Properties) already treats a valid sidecar cue as the authority for
untaggable carriers (`metadata_editor_untaggable_sidecar_authority`, keybindings.rs:10172,
plus the shipped cue-writeback that saves edits to the sidecar). Transfer Tags has no
counterpart — it insists on reading/writing the carriers directly.

**Diagnosis (consensus-verified by two independent audits).**
- Read side: for a `TransferCarrier::SidecarCue` with role `MetadataSidecar`, the transfer
  path extracts cue entries (`cue_sheet_transfer_entries`, tag_interchange.rs:3042) but then
  STILL reads the carriers (the `MetadataSidecar` arm at tag_interchange.rs:3060):
  `read_transfer_source_entries(track_audio_paths, …)?` (call at tag_interchange.rs:3062).
  That reads each `.dff` via lofty → `UnknownFormat` →
  `MetadataReadIssueKind::UnsupportedFormat` (probe.rs ~7912) → `blocks_metadata_use()` true
  (probe.rs:7905, true for anything but `RecoverableTagWarning`) → the whole op aborts
  (tag_interchange.rs:3154, "(N of M sources unreadable)"). The `?` makes one unreadable
  carrier fail everything. The sibling `EmbeddedCue` branch (tag_interchange.rs:3086) shows
  the correct pattern: derive entries from `cue_sheet_transfer_entries(sheet)` without
  demanding a readable carrier.
- Write side: the apply/commit half must route tags for an untaggable `MetadataSidecar`
  target to the sidecar cue (as the editor's cue-writeback does), not attempt embedded-tag
  writes to `.dff`. There is a sidecar write method already
  (`SidecarCueWriteMethod::PerFileAndSidecar`, tag_interchange.rs:38/188) — confirm where an
  untaggable target currently fails or no-ops and route it through the established
  sidecar-cue writer.

**Outcomes.**
- A1 — Transfer FROM an untaggable+cue album reads its metadata from the sidecar cue,
  tolerating unreadable carriers instead of aborting (mirror the editor's
  untaggable-sidecar authority). No valid cue → the existing honest failure is fine.
- A2 — Transfer TO an untaggable+cue album writes the transferred canonical tags to the
  **sidecar cue** via the established atomic sidecar-cue writer, with per-carrier embedded
  writes marked Blocked/Unsupported — never a hard failure, never a false "wrote tags to
  .dff" claim. Honest status/log about where the tags landed (the cue).
- A3 — Works for the transfer SCOPES (Canonical / All / field subsets) and for folder AND
  file(s) selections, consistent with taggable behavior.
- A4 — Consistency with the editor: a given cue yields the same per-track values via
  Transfer Tags as the metadata editor shows/writes. Reuse the editor's untaggable-authority
  + cue-writeback machinery; no second cue reader/writer.

**Guardrails (A).** Do NOT regress taggable-album transfer (read/write carriers directly),
`EmbeddedCue`/split-source paths, or non-cue folders. Untaggable class = the existing
classification (`blocks_metadata_use`/lofty-unsupported), not an extension list.
Lodestar-governed (docs/metadata_source_selection_heuristic.md, bundled) — source
selection/admission unchanged; full-gate ×2 posture.
**Tests (A):** (a) transfer FROM dff+cue yields cue-sourced entries, no abort on unreadable
carriers; (b) transfer TO dff+cue lands canonical tags in the sidecar, carriers untouched,
honest status; (c) an SHN/DTS untaggable variant both directions; (d) regression: taggable
album transfer unchanged; (e) no-valid-cue untaggable album still fails honestly.

---

## B. Renaming in Browse resets the view to the top — preserve cursor + scroll

**Problem.** Renaming a file/folder inline in Browse reloads the list and jumps the
highlight/scroll to the TOP of the current folder. On folders with thousands of entries this
is a major PITA — you rename an item deep in the list and lose your place.

**Desired.** After an inline rename commits, the highlight lands on the RENAMED entry (at its
new sorted position) and scroll is preserved (or minimally adjusted to keep it visible). No
jump to top.

**Diagnosis (audited — the rename path is ASYNC).**
- The machinery exists: `BrowseState.cursor_restore_target: Option<String>` (browse.rs:2738)
  restores the cursor to a NAMED entry after a refresh. The **CREATE path already does it
  right** and is the pattern to copy (keybindings.rs ~47883-47892): sets
  `cursor_restore_target = Some(name)`, `refresh_with_search`, find-by-path →
  `selected_index` → `ensure_visible()`.
- Rename chain (async): `commit_browse_inline_edit` (keybindings.rs:3672) →
  `commit_browse_rename` (keybindings.rs:47919) which validates then `spawn_rename_plan(…)`
  (a background task) and returns. On completion the event loop handles
  `AppMessage::RenamePlanComplete` (event_loop.rs:5663) → `complete_rename_plan`
  (keybindings.rs:1491) → `refresh_browse_after_undo_redo` (keybindings.rs:1765), which does
  `rebuild_tree_preserving_expansion()` + `refresh_with_search()` but **never sets
  `cursor_restore_target`** → the cursor falls to the top.
- **Fix locus:** `complete_rename_plan` (1491). The new path is in the report:
  `result: Result<rename_plan::RenameExecutionReport, String>`, and each
  `RenameExecutionReport.roots[i]` (`RenameExecutionRoot`, rename_plan.rs:197) carries
  `source` and `destination: PathBuf` — restore name = `roots[0].destination.file_name()`.
  Set `cursor_restore_target` to it around the refresh, `ensure_visible()`, preserve scroll;
  for multi-item renames restore to the primary root. Also confirm the sequential-rename
  (Tab/BackTab) flow (`sequential_inline_rename_target`, keybindings.rs:3699) doesn't
  scroll-jump.

**Outcome (B).** Rename keeps the user's position; the renamed entry stays highlighted and
visible. Test: rename an entry mid-list in a large fixture folder → after
`complete_rename_plan`, `selected_index` is the renamed entry and scroll is unchanged/keeps
it visible (drive the real completion reducer, not the fs op directly).

---

## C. `bat` default read-only `.log` viewer + Enter file-associations

Two related viewer features.

### C1 — `bat` as the default read-only viewer for `.log` (config, opt-in to own editor)
**Desired.** `.log` files open in **`bat`** by default (nicer styling), strictly read-only.
A `config.toml` option lets the user opt OUT (use their own editor). Default = bat; opt-in =
own editor. Keep the existing read-only paths for real editors.
**Anchors.** `bat` is already recognized as inherently read-only in external_editor.rs:145
(`"less" | "more" | "bat" | "cat" => None`); `open_in_viewer` (external_editor.rs:132)
already picks a viewer and applies `-R`/`-v` for vim/nano. `.log` is already treated
read-only (browse.rs:12130; command.rs:3261 View vs 3263 Edit-not-.log). **`bat` is NOT in
flake.nix — add it** to the dev-shell tools + packaged PATH. Config: add an option (e.g.
`[ui] log_viewer = "bat" | "editor"`, default `bat`, serde-defaulted) following the
`[metadata] sidecar_save_with_warnings` / `[file_operations]` precedent in src/config.rs.
**DOCUMENT TWICE (per user):** a doc comment on the config field AND a commented note in the
generated `config.toml`, both flagging `TODO(config-screen)` — same pattern as
`[ui] manage_tmux_clipboard` (config.rs:362). Missing `bat` at runtime must degrade
gracefully (fall back to the existing read-only viewer), not error.

### C2 — Enter opens `.log` `.txt` `.nfo` `.md` `.cue`
**Desired.** Highlighting one of these and pressing **Enter** opens it: `.log` → `bat`
(read-only, per C1); `.txt`/`.nfo`/`.md`/`.cue` → the default editor (read-write, or
read-only where sensible — `.cue` may warrant view). Today Enter on these does nothing
useful.
**Anchors.** Browse Enter dispatch: keybindings.rs:6251 (`(KeyCode::Enter,
KeyModifiers::NONE)` — dir = navigate in; file = no open today). Add an extension→action map;
reuse `external_editor::open_in_editor` (command.rs:7608 / external_editor.rs:105) for
editable types and the read-only viewer path for `.log`/`.cue`. Respect C1's config. Keep it
a small, extensible file-association table (more extensions/handlers later — future Config
screen). Byobu-safe: Enter only, no new chords.

**Outcome (C).** `.log` opens in bat read-only by default (config-overridable); the five
extensions open on Enter via the right handler; `bat` in the nix shell; documented per
above. Tests: config default = bat; Enter dispatch selects the right handler per extension;
graceful fallback when bat is absent.

---

## Deliverables
Complete replacement files or unambiguous patches; a WHY summary per workstream (A: how read
tolerates unreadable carriers under cue authority + how write routes to the sidecar; B: the
cursor-restore in complete_rename_plan; C: viewer/association + config); test list; honest
unverifiable-in-your-environment note (no real .dff fixtures unless you synthesize headers;
no terminal for interactive open — cue parsing, dispatch, and config are testable without
either).

## Bundle manifest
- This brief; docs/metadata_source_selection_heuristic.md (LODESTAR, for A).
- Complete `src/` tree (tag_interchange.rs, keybindings.rs, event_loop.rs, browse.rs,
  command.rs, external_editor.rs, probe.rs, config.rs, rename_plan.rs, and the editor's
  untaggable-authority + cue-writeback code for reuse) + `crates/tui-file-picker`; root
  `Cargo.toml`, `flake.nix` (add bat here), `CLAUDE.md`.
NOT included: other workspace crates, target/, other docs. If anything is missing, say so
rather than guessing.

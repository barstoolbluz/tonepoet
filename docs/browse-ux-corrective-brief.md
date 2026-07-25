# Corrective Brief — Browse UX Hardening Round 2

Field-testing feedback on the v5 delivery (applied at bf91649). Twelve items: three
P0 defects, one instrumented mystery, eight UX corrections. All findings below are
evidence-backed from a diagnosis pass on the exact applied tree; line anchors from
bf91649 — re-locate before editing.

## P0-1. Move leaves source in place (SMB/CIFS, but the bug is ours)

**Environment:** Ubuntu 22.04, browsing an ext4 volume over an SMB/CIFS mount.
**Symptom:** folder and file moves copy the tree correctly, then fail with
`Failed: <name> — move left original in place because copied tree was incomplete`.
Source and destination are byte-identical afterward. Never happened before the v9
file-op machinery.

**Mechanism (diagnosed, then audit-refined):** the message comes from an early guard
in `move_via_copy_verify_remove_node` (`src/tui/keybindings.rs:27429-27435`) that
aborts the move if the copy phase incremented `self.totals.errors` OR
`self.totals.skipped` — with no record of WHICH entry tripped it or why. The
manifest verification downstream (from :27456; `source_guard.rs` — which already
tolerates cifs xattr `EOPNOTSUPP` at ~:282 and routes fidelity doubts to the
separate ":27516 destination fidelity … could not be fully confirmed" retention)
never runs when the guard fires. AUDIT CORRECTION: metadata/xattr preservation
warnings do NOT increment these counters (they land in `pending_root_warnings`,
`keybindings.rs:28091-28099, 28272-28276, 28475-28487`) — the counters move only on
(a) real per-entry copy errors and (b) skip decisions (user Skip via poll_controls
:28206-28208, or conflict-policy skips). The leading field hypothesis is therefore a
RETRY scenario: a first move attempt commits the destination but retains the source
(e.g., via the P0-2 quarantine failure or fidelity retention), the user retries, the
second attempt's copy hits already-present destination entries → conflict skips →
the guard reads skips as "incomplete tree" even though content is complete. Your
first task on this item is empirical: determine what actually incremented the
counters in the user's runs (add per-entry diagnostics first if needed).

**Required:**
- The completeness decision must distinguish: (a) an entry genuinely failed to copy →
  refuse, naming the entries; (b) a conflict-skip where the destination entry already
  matches the planned copy (retry-over-identical) → NOT incompleteness; let the
  manifest verification arbitrate actual completeness; (c) a user-requested skip →
  honest partial result, labeled as such, never the generic "incomplete" wording.
- The refusal message must name the offending entries (bounded list + count), never
  just "copied tree was incomplete".
- Long failure/retention messages truncate in the status bar (field report: the NTFS
  message cut off mid-sentence). Route detailed multi-line failure text through an
  inspectable surface (the existing error-detail overlay pattern, a `:messages`-style
  log, or wrapping) so diagnostic detail is never lost to status-line width.
- Regression tests simulating both classes (a copy error refuses + names; a
  metadata-warning-only copy proceeds and completes the move).
- Also audit the per-entry source/destination verification for cifs fragility the
  user will hit next once moves proceed: `verify_same_object_after_rename` compares
  `mtime_nsec` exactly (`source_guard.rs:133-138`) — cifs truncates timestamp
  precision, so quarantine re-verification can false-fail; and path-vs-handle inode
  identity comparisons (`source_guard.rs:1177-1184`) — cifs pseudo-inodes are not
  stable across re-open. Decide and document a cifs-compatible identity/mtime policy
  (e.g., tolerance window or capability-derived downgrade with explicit warning)
  rather than exact-equality fail-closed.

## P0-2. Rename refused on filesystems without renameat2(RENAME_NOREPLACE)

**Symptom:** both `:rename` and inline rename fail on the cifs mount with
`rename refused: this platform/filesystem cannot guarantee no-clobber rename (atomic
no-clobber rename is not supported by this kernel/filesystem)`.

**Mechanism (diagnosed):** `try_fast_no_clobber_rename`
(`src/tui/keybindings.rs:28886-28936`, landed in the v9 bundle) issues
`renameat2(..., RENAME_NOREPLACE)`; cifs does not implement the flag; `EOPNOTSUPP`
is mapped to `ErrorKind::Unsupported`; the caller (`commit_browse_rename`,
`keybindings.rs:31362-31372`) fails closed. AUDIT REFINEMENT: fallbacks for
`Unsupported` DO already exist in some move paths (`move_root_progress_resolved_node`,
`keybindings.rs:27343-27346, 27398-27402`) — study their pattern and extend the same
treatment to the paths that lack one: inline/`:rename` commit (:31362), source
quarantine (:29514-29522), and transactional directory publication (:27869-27883,
which currently treats `Unsupported` like any other error).

**Blast radius (field-confirmed on a second filesystem):** this same helper is the
quarantine primitive for MOVES. `quarantine_source_root`
(`keybindings.rs:29495-29525`) renames the source into a private quarantine dir via
`try_fast_no_clobber_rename` (line 29514); on a local NTFS mount (`/mnt/hodgepodge`)
this fails and every move ends with "destination committed; source retained because
safe cleanup could not begin: could not quarantine source … (atomic no-clobber
rename is not supported …)". The picker crate has a twin retention path with the
same dependency (`crates/tui-file-picker/src/state.rs:3376`). So this one missing
fallback breaks: inline rename, `:rename`, app-side move cleanup on NTFS, and
picker-side paste cleanup.

**Required:** a graduated fallback when RENAME_NOREPLACE is unsupported —
best-effort no-clobber (e.g., lstat/openat probe of the target then plain rename),
with the narrow TOCTOU window on such filesystems accepted and documented (same
honesty standard the v5 db-patch applicator used for its own limits). Never refuse a
rename — or a quarantine, or a cleanup — solely because the fast path is
unavailable. Note the quarantine case is even safer than general rename: the target
lives inside a directory this process just created exclusively, so the no-replace
guarantee is already structural. Optional: per-mount capability memo so the syscall
probe isn't repeated. Cover every call site of the helper in BOTH the app and the
crate (enumerate them; known: commit_browse_rename, quarantine_source_root, the
crate's paste/cleanup path). Regression tests for the fallback (simulate
`Unsupported` from the fast path) in both surfaces.

**Governing principle for P0-1/P0-2 (user directive):** the file-operation machinery
must have a PRAGMATIC, honest degraded mode for filesystems that lack ext4 semantics
(cifs, ntfs3/ntfs-3g, FUSE generally): probe capabilities per mount, rely on what
works everywhere (content hashes, size, structural no-clobber via exclusive parent
dirs) instead of what doesn't (renameat2 flags, stable inode identity, nanosecond
mtimes, xattrs), degrade with an explicit one-line notice rather than refusing the
operation, and reserve fail-closed retention for genuine content-verification
failure. "Works only on local ext4" is a defect, not a safety posture.

## P0-3 (downgraded by audit). Bookmark store migration: verify and pin, don't build

**Field observation:** the redesigned manager renders "0 bookmarks" (user screenshot);
`~/.config/tonepoet/bookmarks.toml` absent; SQLite `bookmarks` table empty; the new
`.bookmarks.lock` appeared at first v5 run.

**Audit finding (corrects the original diagnosis):** the feared
absent-TOML-ignores-SQLite hazard is ALREADY HANDLED — `load_from_db`
(`src/tui/bookmarks.rs:165-272`) detects an absent shared store and seeds it from a
non-empty SQLite table via `initialize_bookmarks_if_absent`
(`crates/tui-file-picker/src/bookmarks.rs:223-256`). The user's empty store is
therefore most plausibly genuine (no bookmarks were ever persisted on this machine),
and the visible failure is item 6's empty-state collapse, not data loss.

**Required (reduced scope):**
- Regression-pin the migration and the invariants with tests: absent-TOML +
  non-empty-DB seeds the TOML once, preserving `position` order; absent-TOML +
  non-empty-DB can never clear the DB mirror; empty-but-PRESENT TOML does clear the
  mirror (a real empty); migration is idempotent.
- Add a visible one-time notice when migration runs ("migrated N bookmarks from the
  database"), and surface store-load errors in the manager (the loader distinguishes
  NotFound from real errors — make a real error visible, not an empty list).
- Verify the reconciliation call chain cannot run with an empty in-memory set before
  the store has been read (ordering guard + test).

## 4. Dead Ctrl+V (file paste) — cause unknown; find it

**Symptom:** after a successful Ctrl+X ("Cut N items" status), Ctrl+V does nothing:
no progress overlay, no status message (status returns to the idle key-hint bar).

**What we proved:** the code path is sound in isolation. This probe test (drive it
through the real dispatcher) PASSES on the applied tree — adopt it as a permanent
regression test:

```rust
#[tokio::test]
async fn files_ctrl_x_then_ctrl_v_starts_paste_task() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("src");
    std::fs::create_dir(&dst_dir).expect("dst");
    let file = src_dir.join("track.flac");
    std::fs::write(&file, b"audio").expect("fixture");
    let mut app = AppState::new_for_test(TonepoetConfig::default());
    app.current_screen = AppScreen::Browse;
    app.browse.current_dir = src_dir.clone();
    app.browse.entries = vec![browse_file_entry(file.clone())];
    app.browse.set_navigation_pane(crate::tui::browse::BrowseNavigationPane::Files);
    let (tx, _rx) = mpsc::channel(16);
    handle_key(&mut app, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL), &tx);
    assert!(app.browse.filesystem_clipboard.is_some(), "cut must stage clipboard");
    app.browse.current_dir = dst_dir.clone();
    app.browse.entries = vec![];
    handle_key(&mut app, KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), &tx);
    assert!(app.browse.pending_clipboard_paste.is_some(),
        "Ctrl+V must start a paste task; status={:?}", app.status_message);
}
```

**Therefore something stateful in a live session consumes Ctrl+V before the clipboard
arm — we don't know what. Figure it out.** Facts to work with: every refusal door in
the paste path sets a status ("Nothing to paste", "Paste failed: …", "A clipboard
paste is already running", archive refusal) and none appeared; Ctrl+X in the SAME
match arm (`'c'|'x'|'v'`, `keybindings.rs:3850+`) worked seconds earlier; dispatch
order routes to search/filter/path-input/inline-edit handlers first
(`keybindings.rs:91-135`) — audit each for silent Ctrl+V consumption when its state
lingers (e.g., search left active but unfocused); also consider the FileTaskProgress
overlay wedging `pending_clipboard_paste` after the P0-1 move failures (that door
prints a status, but verify the status actually survives to render). Whatever the
cause: make every consumption path either act or say something visible, and add
regression coverage for the discovered scenario.

## 5. No decorative glyphs — plain words

Remove the v5-introduced iconography (rule is permanent, applies to all future work):
- Path-row button ` ★ ▾ ` (`draw_browse.rs:376`) → label it `bookmarks ▾` (▾ is an
  established disclosure glyph and stays).
- Dropdown rows ` ★ Bookmark this` / ` ⚙ Manage…` (`draw_browse.rs:930, 938`) →
  `Bookmark this dir` / `Manage bookmarks…`.
- Detail-card `✕ missing` (`bookmarks_overlay.rs:440`) → `missing` in destructive
  color (the color carries the state), `● reachable` → `reachable` in green likewise.

RULING (default, user may override in review): pre-existing FUNCTIONAL state
indicators stay — `●`/`○` toggles, `☑☐` checkboxes, `✓` active-filter checks,
`▲▼`/`↑↓` sort-direction arrows, `▸▾` disclosure, `█░` scrollbar, `▌` caret.
Banned: iconographic decoration (stars, gears, x-marks, flags, and all emoji). No
new indicator glyphs without explicit approval.

## 6. Bookmark manager: empty state, medium widths, picker parity

The two-column design IS implemented (`bookmarks_overlay.rs`) but three gaps made it
invisible in the field:
- **Empty state collapses the design** (user's screenshot): with 0 bookmarks nothing
  but a truncated hint renders ("press a to add the cur" — also fix the truncation:
  wrap, don't chop). Required: full anatomy even when empty — header, list column
  with the (wrapped) empty-state message, and a placeholder detail card ("no bookmark
  selected — a adds the current directory"), so the surface reads as the new manager.
- **Medium widths drop the card silently** (`TWO_COLUMN_MIN_WIDTH = 92`,
  `bookmarks_overlay.rs:18,105`): between compact and 92 inner columns, stack the
  layout vertically (list above, shorter detail card below) instead of hiding the
  card entirely.
- **Picker parity untouched** (verified zero diff in the picker bookmark panel this
  round): bring the picker's bookmark UI to the same design per the standing parity
  principle — shared components where practical, UX equivalence otherwise.

## 7. Solid title bars for overlays

The metadata/Properties editor uses a plain `Block` border title
(`draw_overlays.rs:4785-4794`); the user has asked for the solid inverted bar
(Browse/Convert pane style: `draw_browse.rs:433-443`, `draw_output.rs:410-435`)
repeatedly. Required: the Properties/metadata editor gets the solid bar NOW; then
convert the remaining major overlays for consistency (inventory from diagnosis:
theme builder `theme_builder.rs:1662`, confirmation/error/file-input/text-edit/batch
list/analysis/preemphasis/MusicBrainz/GNUDB/AccurateRip overlays in
`draw_overlays.rs` — all plain Block titles today). One shared helper, not N copies.

## 8. Right-click must not mark items selected

Right-click currently inserts the target into `multi_selected` when unselected
(`keybindings.rs:33909-33913`), leaving the cyan `●` mark after the menu closes.
Required: right-click moves the cursor highlight and the context menu operates on
the right-click target (passed explicitly), but the marker set is NEVER mutated by
right-click. The §11.4 bulk rule still holds: clicking a marked item → menu actions
apply to the marked set; clicking an unmarked item → actions apply to that item only
(without marking it). Update the tests that pinned the marking behavior.

## 9. Inline-edit mouse contract

No text-level mouse support exists in the inline editor today: a hit target exists
for the tree-pane inline editor (`BrowseTreeInlineEdit`, `button_map.rs:256`) but
clicks carry no cursor placement; `TextInputState` has no cursor-from-column/mouse
API at all; the files-pane editor's click behavior needs the same treatment; and the
list drag handler lacks an inline-edit guard (`keybindings.rs:33587-33589` checks
only `ActiveOverlay::None`; `browse_inline_edit` is a separate field,
`app.rs:10373`), so dragging while editing range-selects list items underneath. Required behavior (user-specified,
verbatim contract):
- double-click inside the inline-edit field → select all text;
- double-click with text already selected → select all again (Windows semantics);
- single-click with text selected → clear selection + place cursor at click point;
- single-click without selection → place cursor at click point;
- mouse drag inside the field → select the dragged text range;
- while an inline editor is open, list drag-selection underneath is suppressed.
This needs: a hit region for the editor field, display-width-aware column→cursor
mapping in the shared text engine (crate `text_input.rs` — mind wide glyphs; reuse
`display_width`), drag state for text selection, and the same for the path-bar
editor. Also: the user reports Ctrl+A while inline editing selected FILES, not text —
the code appears to route Ctrl+A to `select_all_text` (`text_input.rs:696-699`), so
either find the path where inline-edit does not consume it or prove it with a
dispatch-order regression test either way.

## 10. Two text clipboards: route terminal paste into editors

Design (user requirement): Ctrl+V in text editors pastes from the IN-APP clipboard
(already true: `TEXT_INPUT_CLIPBOARD`, `text_input.rs:7-10, 659-710`); the LOCAL
(terminal-side) clipboard arrives via bracketed paste. The gap: `handle_paste`
(`event_loop.rs:5600-5740`) routes `Event::Paste` only to a few overlay editors and
SILENTLY DROPS it for the Browse inline editor, the Browse path bar, and while
FileTaskProgress is open (fallthrough at :5736-5738). Required: route `Event::Paste`
text into whichever text editor has focus (inline edit, path bar, filter, search
input — same focus order as key dispatch), inserting at cursor / replacing selection.
With that, the terminal's own paste chord (usually Ctrl+Shift+V at the emulator)
delivers "local clipboard" behavior; document this mapping. Do NOT attempt to read
the system clipboard programmatically (no arboard; OSC52 stays write-only for Copy
Path). If a raw Ctrl+Shift+V KeyEvent does reach the app (some terminals send it),
treat it as a no-op rather than in-app paste, so the two chords keep distinct
meanings.

## 11. Tokyo Night selection contrast

Root cause: `selection_bg` == `border` == #3b4261 in the tokyo-night palette
(`theme.rs:294`, 9th palette slot) on a #1a1b26 background. Required: raise the
default tokyo-night selection to a clearly distinct step (and ensure selected-row fg
uses `text_bright` + BOLD consistently — some sites do, audit the rest). Check the
other built-in dark palettes for the same collision (gruvbox/catppuccin/rose-pine
also reuse their border color as selection_bg — same line range). Preserve the theme
builder's derived-token pipeline; this is a palette-value fix plus a
selected-row-style audit, not a new system. User-visible acceptance: selected row
unmistakable at a glance in default theme.

## 12. Constraints (unchanged + new)

- NO function keys; NO emojis/decorative unicode (items 5's ruling); Ctrl+Q stays quit.
- Preserve all green v5 behaviors and tests; the bookmark worker/lock/detail
  machinery stays — this round redirects and completes it.
- Deletion permanence guards unchanged; bookmark deletion deletes bookmarks only.
- Two-pass rendering; crate compiles standalone; `cargo test --workspace` zero
  failures untruncated.
- Deliverable: overlay tar.gz + MANIFEST + ENGINEERING_REPORT + SHA256SUMS as before.
  If a file you need is missing from the bundle, request it; hash-guarded narrow
  patches (v5 db.rs style) are the right pattern for files you don't have — but
  record the preimage hash of THIS bundle's files, not an older snapshot's.

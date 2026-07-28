# Round-6 Brief — Tag Interchange: text clipboard, transfer, editor clipboard citizenship

**Branch:** `hardening` @ 04e127f. **Baseline suite:** 5,214 passed / 0 failed
(56 targets). **Version stays 0.4.4.**

All mechanisms below are research-verified with citations; do not re-derive
them. Standing directives: the rigor-vs-usability directive
(perf-restoration-brief.md §0); NO function keys; NO emoji/decorative
unicode; Ctrl+Q stays quit; `cargo test --workspace` stays green; never
regress the mouse text contract, 4-state cursor matrix, `:messages`,
degraded-rename ladder, or the standard/strong verification split.

**NEW STANDING PRINCIPLE (user-directed):** key combinations are scoped to
the active screen / active overlay. Every binding added this round MUST be
scoped (the metadata-editor overlay router already preempts globals —
keybindings.rs:62-65 — so editor-phase chords are naturally scoped). A full
scoping audit of EXISTING global bindings is a separate future round — do
not attempt it here.

## 1. The interchange text format ("field blocks") — foundation

Canonical text serialization used by Copy tags → text clipboard, Get tags
from Clipboard, Get tags from File, and Editing-phase bracketed paste:

```
TITLE
Behind the Lines
Duchess

ARTIST
Genesis
```

- One block per field: the display key alone on the first line, then ONE
  VALUE PER LINE — per file, in order — then exactly one blank line
  (byte-empty after CRLF normalization; whitespace-only lines are VALUE
  lines, not delimiters) between blocks. Single-file copies have one value line per block.
- KEY grammar (audit-forced, load-bearing for §5a disambiguation): a block
  first-line must match `^[A-Z0-9_]+$` (upper-case, no spaces). Every
  block must have ≥ 1 value line — a key line with zero value lines is NOT
  a valid block.
- EMPTY VALUES (audit-found blocker — empty per-file values are routine,
  e.g. new fields initialize as `vec![String::new(); n]`, probe.rs:6902,
  and a raw empty line would collide with the block delimiter): an empty
  value serializes as the single-character line `~`. A literal value
  consisting ONLY of tildes serializes with one extra tilde (`~` value →
  `~~` line, etc.); parsing reverses this (a line of N tildes, N ≥ 2 →
  N−1 literal tildes; exactly `~` → empty string). All other lines are
  verbatim. This keeps the format hand-editable while making empty
  representable.
- NEWLINE-BEARING VALUES (audit-verified these EXIST today — multi-line
  COMMENT/LYRICS are real; the renderer sanitizes them to ↵ at
  draw_overlays.rs:5131 etc.): the serializer SKIPS any field containing a
  value with an embedded newline, and Copy tags discloses it in the status
  ("COMMENT skipped: multi-line value not representable"). The internal
  full-fidelity TagClipboard still carries such fields untouched.
- Parsing is EXACT and fail-closed AT THE BLOCK-APPLY ENTRY POINTS (menu
  Get-tags-from Clipboard/File, §4): CRLF normalized; a block whose value
  count is neither 1 (broadcast) nor exactly the target file count → the
  WHOLE apply fails with a status naming the offending key and counts
  ("TRACKNUMBER has 11 values for 12 files"). Unknown keys (matching the
  KEY grammar) create custom fields AT THESE ENTRY POINTS ONLY. Empty
  input / not-our-format → honest status, no partial application. The
  §5a row-targeted paste has its own explicitly-specified precedence
  (see §5a) — its fallback is deliberately NOT governed by this rule.
- Serializer and parser live together as a small reusable module (they are
  the item-2/3/5 shared foundation); round-trip property-tested:
  serialize→parse == identity on the SERIALIZED subset (newline-skipped
  fields are excluded by definition and asserted absent), with tilde
  empty-value encoding covered by the property.

## 2. Copy tags → also the text clipboard (+ best-effort system clipboard)

Current state: Copy tags stores only the internal full-fidelity
`TagClipboard { source_paths, entries }` (browse.rs:2263-2266), published in
the generation-guarded completion handler (context_menu.rs:1521-1573 with
the publish at :1546).

Required: at that exact publish point (main thread, winning generation
only), ADDITIONALLY:
1. Serialize the selected entries to field blocks (§1) and write tonepoet's
   shared text clipboard. The shared clipboard
   (`shared_text_input_clipboard()`, crates/tui-file-picker/src/
   text_input.rs:7-11) is PRIVATE — add a small pub write/read API to
   text_input.rs (the honest seam; do not fake it through a synthetic
   TextInputState).
2. Best-effort system-clipboard write via OSC 52, reusing the exact
   precedent at context_menu.rs:1790-1798 (CopyPath emit + the base64
   helper at :2467). This is advisory and unverifiable — under byobu/tmux
   it needs `set-clipboard on` — NEVER load-bearing; no error surfaced if
   the terminal ignores it. (Size note: terminals cap OSC 52 payloads —
   emit only if the serialized text is ≤ ~64 KiB, else skip the OSC write
   silently; the internal clipboard always gets the full text.)
3. The internal full-fidelity TagClipboard remains EXACTLY as is (it stays
   the future Paste/Custom substrate).
4. Status line gains the destination: "Copied 4 fields from 12 files (text
   clipboard)".

## 3. Browse context menu: Transfer tags; Paste stub removed

New shape under Tags & Tagging:

```
Copy tags ▸           (existing submenu, unchanged incl. Custom... deferred)
Transfer tags ▸
    Transfer to ▸       Canonical
                        All
    Transfer from ▸     Canonical
                        All
```

- REMOVE `Paste tags` outright — full inventory (audit-verified):
  `ContextAction::PasteTagsDeferred` (context_menu.rs:208), menu item
  (:663), dispatch arm + status (:1993-1995), test blocks (:3244-3252 and
  the Paste half of :3328-3335). `CopyTagsCustomDeferred` STAYS as the
  deferred honest-status item (the Custom builder is next round — user has
  mockups).
- Depth check (verified): the stack cap is `MAX_CONTEXT_MENU_DEPTH = 4`
  (context_menu.rs:25-27) and root → Tags & Tagging → Transfer tags →
  Transfer to = level 4 with Canonical/All as ITEMS at level 4 — allowed,
  with production precedent (File ops → Rename → Fix capitalization) and
  the `deep_menu_compiles_and_nests_4_levels` test. NOTHING may ever nest
  below Canonical/All. If implementation friction appears, the sanctioned
  flattening is four depth-3 items ("Transfer to (canonical)" etc.).
- Transfer semantics:
  - "Transfer to": SOURCE = current context-menu selection (same target
    resolution as Copy tags: command.rs:83-101 + the bounded expansion);
    picker selects the TARGET (file or directory).
  - "Transfer from": picker selects the SOURCE; TARGET = current selection.
  - Picker: reuse the embedded-picker pattern from Browse side — a
    standalone FilePicker overlay with `FilePickerFilter::Audio`,
    `selection_mode: FilesOrDirectories` (crates/tui-file-picker/src/
    state.rs:26-30, filter.rs:25-31). The picker returns ONE path per
    completion (verified constraint): a directory expands via
    `expand_audio_paths_for_metadata_limited` with the existing caps.
  - Scope Canonical = the exact predicate
    `tag_entry_matches_copy_selection` with
    `TagCopySelection::CanonicalOnly` (context_menu.rs:1342-1382 —
    display-key matching, NOT ItemKey; the totals trap is already handled
    there). All = `TagCopySelection::All`. Binary entries (`is_binary`)
    are ALWAYS excluded; artwork does NOT transfer (text tags only — state
    this in the engineering report as a disclosed limitation).
  - Alignment rules (audit-recommended, brief-adopted): source 1 file →
    N targets: broadcast scalar fields; NEVER broadcast
    TRACKNUMBER/TRACKTOTAL/DISCNUMBER/DISCTOTAL (skip them with a status
    note in 1→N). Source N → N targets (equal counts): positional using
    THE SAME TRAVERSAL ORDER Copy tags produces (selection order + per-
    directory sorted files-before-subdirectories DFS — audit-verified this
    differs from lexicographic path sort; both sides MUST use the
    traversal comparator so Transfer pairs files the same way the
    clipboard's positional model does). N→M mismatch: HARD FAIL with an explicit
    status (stricter than the MB review flow's silent skip — an explicit
    user-invoked transfer must not silently drop). Entries with
    `per_file_stored_value_counts[i] > 1` collapse to the display value
    with the existing cardinality warning pattern.
  - WRITE SEAM (audit-decided): the classified write path —
    `write_all_tags_with_cancel_report_classified_at_verification`
    (probe.rs:8200, delegating to the unclassified core at :8247) per
    target file, with per-file diffs built the
    way the editor save does (PATTERN at probe.rs:7931-7954, inside
    apply_audio_tag_changes_..._at_verification, fn probe.rs:7909; write
    call :8024 — NOTE: imitate the pattern, do not call it; transfer's
    "changed" compares SOURCE values vs TARGET-current values, not editor
    vals vs origs): `(key, Some(v))` only where changed; never touch
    unrelated fields; transfer never DELETES a target field the source
    lacks (audit-verified constructible: deletion requires an explicit
    `(key, None)` entry — a Some-only diff cannot delete). This inherits
    standard/strong modes,
    native-FLAC (incl. round-5 ID3-prefix files), DSF, DFF fail-closed,
    and cancellation for free. Do NOT use accuraterip's
    `copy_tags_via_lofty` (verified unsuitable: primary-tag-only, no
    native routing, no verification modes, clobber semantics).
  - Runs on a worker (tokio::spawn + spawn_blocking) with the Copy-tags
    generation/cancellation pattern; progress via status; completion
    status names files written/failed; `:messages` retains details on
    failure (reuse the retained-report pattern if cheap, else status).

## 4. Metadata editor: the `tags` popup (replaces `:tags-mb` pill)

- The Editing-phase footer pill `:tags-mb` (draw_overlays.rs:5318; hit list
  keybindings.rs:26483 — the two lists MUST stay in sync; the in-code
  comments at both sites reference a `project_editor_footer_pills.md` that
  does not exist — the requirement is real, the pointer is dead; keep the
  two sites in sync directly) becomes **`tags`**. Clicking it (or a
  scoped chord if trivially free — optional) opens a popup menu ANCHORED
  ABOVE the footer (start-menu style, bottom edge flush above the pill
  row).
- Popup-up geometry (verified): no flip-up primitive exists —
  `context_menu_panel_rect` (keybindings.rs:27328-27345) only clamps y.
  Add a bottom-anchor mode (e.g. `anchor_bottom` computing
  `y = origin.1.saturating_sub(menu_h)`) rather than pre-offsetting the
  origin (pre-offset breaks when entry counts change). Everything else —
  stack, hit-test, hover, cascade flip, editor parking
  (`pending_metadata_editor` park + `close_context_menu_restoring_parked`,
  keybindings.rs:8945-8971) — reuses as-is.
- Popup contents:

```
Get tags from ▸     MusicBrainz
                    gnuDB
                    Clipboard
                    File
Transfer tags from ▸   Canonical / All
Transfer tags to ▸     Canonical / All
```

  - MusicBrainz → `Command::TagsFromMb { query: None, catno: None,
    year: None }` via execute_command — with the editor open it routes
    deterministically through `try_dispatch_in_editor_tags_mb`
    (command.rs:13430, dispatch :5654) inheriting in-flight guard,
    parking, MbSelect review, and `[use MB]`/`[revert]` pills. The `:`
    colon command stays (power users); only the pill changes.
  - gnuDB → `execute_gnudb_query` (context_menu.rs:2559). DECISION taken:
    expose it even though gnudb.org's endpoint has been dark since
    2026-05-10 (context_menu.rs:587-595) — the path is preserved and
    fails with an honest network status; when the service returns (or the
    client migrates to CDDBP/8880 — NOT this round) the entry lights up.
  - Clipboard → parse tonepoet's shared text clipboard as field blocks
    (§1) and apply to the OPEN EDITOR's entries (not directly to files):
    per-block, find-or-create the field row (the gnudb populate's
    find-or-create pattern, gnudb.rs:482-507, is the model), set
    per-file values positionally (1 value = broadcast; count == file
    count = positional; else fail the whole apply), recompute
    mixed/dirty. The user then reviews and saves normally — applying to
    the editor (not files) keeps this reviewable and undoable via the
    editor's existing revert machinery. SUCCESS STATUS (verifier-added):
    a successful block apply names the keys and mode ("applied TITLE
    (broadcast to 12 files), TRACKNUMBER (positional) — review before
    save") — a 1-value TITLE block rewriting every title must be visibly
    acknowledged, not silent; pin the wording. SYSTEM clipboard reality
    (verified, must be documented in the report): app-initiated system-
    clipboard READ is impossible (no OSC 52 query path, crossterm cannot
    surface responses); the menu item reads tonepoet's clipboard ONLY;
    system-clipboard content arrives via the user's terminal paste
    (§5's Editing-phase paste arm) — the status for an empty/invalid
    tonepoet clipboard should say exactly that ("tonepoet's clipboard has
    no tag blocks; paste from the system clipboard with your terminal's
    paste key instead").
  - File → embedded editor file picker (the artwork-picker pattern:
    `MetadataFilePickerState`, keybindings.rs:10721-10757, completion via
    `AppMessage::FilePickerComplete` + `reduce_file_picker_complete`,
    event_loop.rs:813-845) with a NEW `FilePickerPurpose` variant and
    `FilePickerFilter::Custom { label: "Text", extensions: ["txt"] }`
    (plus All available via the picker's own controls). Reads the file,
    parses field blocks, applies to the open editor exactly like
    Clipboard.
  - Transfer tags from/to → same machinery as §3, with the editor's OWN
    file set as the fixed side: "from" = picker source → apply INTO THE
    OPEN EDITOR (reviewable, like Clipboard — not direct file writes);
    "to" = write the editor's CURRENT (possibly unsaved) values to picker
    targets via the §3 write seam. DECISION (audit-forced — "confirmation
    status" straddled two primitives): if the editor is DIRTY, a BLOCKING
    confirmation overlay (the existing confirm machinery) states "Transfer
    N unsaved edits to M files?" before writing; if clean, proceed
    directly with a status line counting targets. A destructive multi-file
    write of unsaved values warrants the one extra keypress; a clean
    transfer does not.
- The editor's Add-field/row model already supports created fields;
  Track-scoped rows (row_scope == Track) are skipped by Clipboard/File
  apply with a status note (same guard the save path uses —
  audit-confirmed at probe.rs:7937-7938).
- THREE BROADCAST REGIMES EXIST BY DESIGN (audit-flagged; declared, not
  unified): (a) editor block-apply (§4 Clipboard/File) allows 1-value
  broadcast for ANY key including track numbers — deliberately unguarded
  because the result lands in the OPEN EDITOR and is reviewable before
  save; (b) Transfer (§3) writes files directly, hence its track-number
  broadcast guard; (c) `metadata_editor_apply_detail_paste` keeps its own
  unchanged semantics (album-key replication of line 1 for
  ALBUM/ALBUMARTIST/DATE/GENRE/CATALOGNUMBER, positional otherwise).
  State this trichotomy in the engineering report.

## 5. Metadata editor: full clipboard citizenship

### 5a. Row-level selection (new)

- New `selected_rows: BTreeSet<usize>` — PER SURFACE, on
  `PresentationTab` mirroring `deleted` (audit-corrected: `deleted:
  Vec<usize>` at app.rs:7357 lives on PresentationTab, NOT the model; a
  model-level selection would dangle across presentation-tab switches).
  Verified: NO multi-row selection exists today (app.rs:7549-7594).
  Rendering: selection marker in the row loop (draw_overlays.rs:4960-5140,
  entry loop at :4962), using existing selection styling conventions (no
  new glyphs beyond the functional set).
- Scoped chords in the Editing-phase key match (keybindings.rs:12796+;
  the overlay router preempts all globals). Audit correction: the Editing
  arm is NOT chord-free — Ctrl+Shift+Tab/Ctrl+BackTab (:12810-12815) and
  Ctrl+R (Details tab retry, :12890) exist; the proposed set collides with
  NONE of them. PLACEMENT (audit-forced): the new chord arms go AFTER the
  content-tab guard arm (~:12849), so they fire on the METADATA tab only —
  clipboard row ops are meaningless on ReplayGain/Artwork/Details tabs:
  - Ctrl+A / Alt+L: select all rows; Ctrl+/ (both Char('/') and Char('_')
    CONTROL arms — the 0x1F pattern at
    crates/tui-file-picker/src/text_input.rs:806-813): deselect all.
    Space keeps its current behavior (bound only when
    shows_presentation_control(), :12827); Invert via menu (a chord is
    optional).
  - Ctrl+C: copy selected rows (or cursor row if none selected) as field
    blocks (§1) to tonepoet's text clipboard (+ OSC 52 best-effort, §2
    rules). Ctrl+X: DECISION (audit-forced — "clear values" vs "mark
    deleted" produce different files): cut = copy + MARK ROWS DELETED via
    the existing deleted-row model (`metadata_editor_delete_cursor`
    keybindings.rs:9507, honoring its refusals: CUESHEET confirmation,
    per-track/blocked rows refuse with status). Deleted rows are
    restorable before save exactly as today — cut is reviewable, not
    destructive. NOT value-blanking.
  - Ctrl+V / Ctrl+P (and the Editing-phase bracketed paste below):
    DISAMBIGUATION + COUNT RULES (audit-forced rework — the naive
    "valid field blocks else fallback" breaks the brief's own Duke
    example, since ANY ≥2-line text parses as key+values):
    1. Classify the pasted text as FIELD BLOCKS only if its first line
       matches the §1 KEY grammar AND is a KNOWN key (canonical set or an
       existing row's display_key) AND ≥1 value line follows. Unknown-key
       custom-field creation is menu-entry-only (§1) — a row-targeted
       paste never creates fields from a coincidentally ALL-CAPS first
       line.
    2. If classified as field blocks: apply per §4 Clipboard semantics;
       a count failure is an HONEST ERROR (never silently degrades to
       row paste — a genuine block with a count typo must not be pasted
       as literal text).
    3. Otherwise: ROW PASTE onto the cursor row — pre-validated, then
       `metadata_editor_apply_detail_paste` (keybindings.rs:27010).
       PRE-VALIDATION (audit-forced — the function itself is lenient:
       single line touches ONE slot for non-album keys, extras truncate,
       fewer partial-fill; it stays UNCHANGED so DetailEdit paste keeps
       its behavior): line count must equal the row's slot count
       (positional — the Duke flow: 12 lines → 12 per-file values), OR
       be a single line for the 5 album-replicated keys (the function's
       own regime). Any other count → honest error naming counts. The
       Duke example classifies as row paste (first line "Behind the
       Lines" fails the KEY grammar) and passes pre-validation at 12=12.
- Bracketed paste (system clipboard) in the EDITING phase: the currently
  silent no-op arm (event_loop.rs:6050-6100 has DetailEdit :6069-6089 and
  InlineEdit :6090-6098 arms; Editing falls through silently) gains the
  SAME classification precedence as Ctrl+V above (steps 1-3), with an
  honest status when neither applies. This is how Ctrl+Shift+V "just
  works" — we bind NOTHING (terminal-mediated; raw Ctrl+Shift+V key
  events stay swallowed, keybindings.rs:31-36).

### 5b. Per-column context menus (key AND value)

- Right-click on an entry row currently opens the row menu regardless of
  column (keybindings.rs ~26010-26100; row = y-math only — verified mx is
  never consulted). Add column classification: `mx <` key-column boundary
  (fixed `key_col_w = 22`, draw_overlays.rs:4954 — NOTE this constant is
  duplicated at keybindings.rs:13005 and ~26244; use one shared const) →
  KEY cell, else VALUE cell.
- Both columns get clipboard entries IN ADDITION to the existing row menu
  items (keep MB revert / Edit value / Delete / autonumber etc.):
  Copy, Cut, Paste, Select All, Invert Selection, Deselect — operating on
  rows (key-cell and value-cell right-click both act on the ROW for
  copy/cut/paste; the distinction the user asked for is that BOTH columns
  respond — semantics are row-level. Key-cell Copy copies the field
  block; value-cell Copy the same. Single-row Copy = one field block).
- `<multiple values>` rows: right-click gains "Edit values (per file)" →
  `metadata_editor_begin_detail_edit_for_entry` (keybindings.rs:26813,
  pass `edit_first_writable_slot: false` to mirror the double-click
  browse-values path) —
  verified double-click ALREADY opens it (via
  metadata_editor_begin_cursor_value_edit :9433/:9461); paste inside
  DetailEdit already fully works (bracketed → apply_detail_paste,
  event_loop.rs:6068-6089; Ctrl+V/P inside detail_edit input). So this
  sub-item is: one new menu entry + verifying the existing paths with
  pins, not new machinery.
- Menus park via the existing `pending_metadata_editor` mechanism; actions
  flow through `execute_context_action` taking the parked state
  (context_menu.rs:1996+ pattern).

## 6. Queue Ctrl+L unbind (user-directed)

Remove the Queue clear-finished chord: binding at keybindings.rs:6804
(inside handle_queue_key), footer pill LABEL "C-l clear done" at
draw_queue.rs:459 (the pill/button itself stays — mouse still works; label
becomes just "clear done"), help row at help.rs:177. No tests pin it
(verified). Clear-finished remains available via mouse and any existing
command. Ctrl+L remains bound in the file picker (address bar — browser
convention, deliberately kept).

## 7. Constraints recap and deliverables

- All new bindings scoped per the standing principle. New chords live
  inside the editor's phase dispatch or the popup handler — nothing
  global.
- gnuDB network migration (CDDBP/8880) is OUT OF SCOPE.
- Custom builder OUT OF SCOPE (next round; keep the deferred item).
- Artwork/pictures do NOT transfer or serialize (text tags only) —
  disclose in the report.
- Tests per item, minimum: field-block round-trip + exact-parse failure
  cases (counts, malformed); Copy tags writes text clipboard (+ cap-gated
  OSC skip); Transfer 1→N broadcast (track numbers skipped), N→N
  positional, N→M hard-fail, native-FLAC + DSF targets routed, standard
  vs strong inherited; popup opens ABOVE the anchor (bottom-anchor
  geometry pin) and its EIGHT leaf actions dispatch (Get tags from: 4;
  Transfer from: 2; Transfer to: 2); editor row selection
  ops; Ctrl+C serializes selected rows; Ctrl+X marks rows deleted and
  honors the delete refusals; row paste positional (the Duke
  case: 12 lines → 12 per-file values) incl. the classification
  precedence (KNOWN-key block vs row paste vs count-typo honest error);
  the empty/invalid-clipboard honest status AND the block-apply success
  status (exact wordings pinned);
  Editing-phase bracketed-paste arm
  (field-block AND single-row fallback AND honest-status); per-column
  right-click classification; multiple-values right-click entry; Queue
  Ctrl+L unbound (clear-finished still reachable by mouse).
- Deliverables: overlay bundle with preimage manifest; engineering report
  with per-item named pinning tests, the disclosed limitations (OSC 52
  advisory, no system-clipboard read, artwork excluded, gnuDB endpoint
  dark), and any deviation with rationale.

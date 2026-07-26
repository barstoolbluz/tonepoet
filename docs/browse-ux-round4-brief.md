# Brief — Browse/Picker UX Round 4

Nine user-requested items plus two carried-forward from round 3. Tree @ f108541
(= main, version 0.4.4). Line anchors from that snapshot — re-locate before editing.
A recurring theme this round: items 3 and 4 have field observations that CONTRADICT
what static reading of the code predicts (item 6's mechanism was subsequently
identified — see its section). For the contradicted items, the mandate is
reproduce-first: build a failing test from the user's exact inputs through the real
entry point, find the actual mechanism, then fix. Do not "fix" code that analysis
says is already correct without a repro proving otherwise.

## 1. Sortable columns in the file picker

The machinery already exists: `FilePickerSortKey { Name, Type, Size, Modified }` +
`sort_reverse` (`crates/tui-file-picker/src/state.rs:66-77, 739-740`), `set_sort()`
with same-key direction toggle (:1954), comparator `compare_entries` (:4728). What's
missing is the UI: the header is a static ratatui Row ("Name/Size/Type/Modified",
`render.rs:732`) with no hit regions and no direction indicator.

Required: clickable column headers (hit regions per column), `▲`/`▼` indicator on the
active column (established functional glyphs), same click-to-toggle semantics as the
Browse pane (`draw_browse.rs:2455-2500` header rendering; `TuiButton::BrowseColumn`
registration ~:1902 — imitate, and share code where cheap). Keyboard access too (the
picker has an Alt+O menu — a Sort submenu there mirrors Browse's "Change sort").
Persist the picker's sort choice consistently with how Browse persists its browsing
config. Regression: header click cycles asc→desc, indicator matches, order proven.

## 2. Inline-editor cursor contrast (regression from round 3)

**Field report:** after round 3 fixed selected-text visibility, the CURSOR is now
nearly indistinguishable from the field background when text is NOT selected. With
text selected, the cursor contrasts fine.

**Analysis vs field:** static reading says the cursor uses the inverse-video pair
(`inline_edit.rs:42-47, 76-77`: cursor cell = fg(theme.bg) bg(text_bright)) which
should be high-contrast on the `input_focused_bg` field. The user sees otherwise —
so either some editing path doesn't render the embedded cursor cell (falling back to
the terminal hardware cursor, whose visibility varies by terminal), or a
row/field-restyle overrides it in the unselected state, or the flags passed to
`render_editing_spans` differ per path. REPRODUCE VISUALLY first, per state, per
surface (browse rename/create, path bar, picker editors).

**Required — the four-state contrast matrix as a hard contract**, because this is the
second consecutive round a fix for one state broke another:

| state | requirement |
|---|---|
| normal text, no selection | text clearly readable on field bg; field bg distinct from row bar |
| cursor on unselected text | cursor cell clearly distinct from BOTH field bg AND adjacent text |
| selected text | selection clearly distinct from field bg and row bar |
| cursor within selected text | cursor cell clearly distinct from the selection surface |

Deliver a render regression test asserting all four states produce pairwise-distinct
styles (fg/bg pairs) in the default theme AND in at least one light palette, on a
selected row. The human-eye rule the user stated: every one of these states must be
unambiguous at a glance.

## 3. Non-ASCII characters lost in generated file/folder names (repro-first)

**Field report:** converting Blue Öyster Cult's *Fire of Unknown Origin*, the `Ö`
present in the metadata tags did not materialize in the generated folder name.
Display of `Ö` is fine; manually typing/pasting `Ö` into names is fine. Not new —
predates this branch.

**Verified facts that ELIMINATE the obvious suspects:** the pipeline sanitizers
preserve non-ASCII (`sanitize_component`, `stages.rs:35198-35218`;
`sanitize_title_extra_component`, :35228-35254 — both map only filesystem-forbidden
+ control chars); `renaming.rs::sanitize_for_filesystem` (:192-213) likewise; the
canonical-artist list contains no ASCII "Blue Oyster Cult" entry (hypothesis checked
and dead). So the loss happens elsewhere.

**Required:** reproduce with a real fixture (tags containing `Ö`, `é`, `ü`, CJK)
through the REAL conversion path(s) the user exercises (folder + file templates,
single-file and album/batch), find the dropping site, fix it. Candidate sites to
check: tag ingestion per source type (especially CUE/sidecar encoding decode paths —
SHIFT_JIS/latin-1 handling), template token rendering, the capitalize/fixcaps chain
(`capitalize_word` heuristics use `is_ascii_*` filters — verify they pass unicode
through rather than dropping), rename/action flows. The fix must preserve unicode by
default; if any transliteration option is added it must be opt-in config, never
default. Regression across all template paths.

## 4. Capitalize after '&' (repro-first)

**Field report:** words after `&` get article lowercasing — user expects
`Booker T & The MG's`, `Neil Young & The Shocking Pinks`.

**Verified fact that contradicts it:** `capitalize_section`
(`renaming.rs:473-508`) HAS an `after_ampersand` rule ("Check if previous word was
'&' — if so, always capitalize"). So some path the user actually hit lacks it.

**Required:** reproduce with the user's exact strings through EVERY fixcaps entry
point: metadata-editor fixcaps (`keybindings.rs:14931-14934` maps
TITLE→`capitalize_title`, ARTIST/ALBUM/ALBUMARTIST/PERFORMER→`capitalize_section`),
`capitalize_title`'s paren-splitting path (`renaming.rs:414-471` — its segment
handling may break the words[i-1] adjacency, e.g. when '&' abuts a paren boundary or
punctuation tokenization separates it), the rename/naming pipeline path, and any
other caller of these functions. Fix whichever path fails; add the user's two
examples as regression cases plus `&` adjacent to punctuation/parens.

## 5. Editor context menu + Rename/Fix-capitalization menus (all NEW — verified absent)

Verified current state: no text-editing context menu exists anywhere, and
right-click inside an editor destroys the edit session — the Browse inline editors
COMMIT on right-click (`finish_inline_edit_before_focus_change`,
`keybindings.rs:1140-1142`, ~36002) while the metadata detail editor CANCELS
(`keybindings.rs:24015-24017`). No case-transform action exists anywhere in
`src/tui/` (the only fixcaps surface is the metadata editor's key-driven action).

Required:
- Right-click inside any active text editor (inline rename/create, path bar — and as
  the item-7 rollout proceeds, every editor) opens a text context menu:
  `Paste / Copy / Cut / Delete / Select All` operating on the editor's
  selection/clipboard (in-app clipboard; Paste follows the same semantics as Ctrl+V/P).
  Right-click must no longer commit or cancel the edit — the session stays live under
  the menu and resumes when it closes.
- In the Browse "File operations" submenu (`context_menu.rs:402-427`), `Rename`
  becomes a submenu: `Rename` and `Fix capitalization ▸` with children
  `Title Case / UPPERCASE / lowercase` — applying the transform directly to the
  file/folder NAME (through the same no-clobber rename pipeline as a manual rename;
  bulk-capable over the selection per the established §11.4 rules).
- The same `Fix capitalization ▸` submenu appears in the editor context menu,
  transforming the editor's text (selection if any, else whole value).
- Title Case = the existing `capitalize_title`/`capitalize_section` behavior (with
  items 4/6 fixed); UPPERCASE/lowercase are plain Unicode case maps.
- Plain words only; no new glyphs; update menu regression tests.

## 6. Fixcaps mangles parenthetical designators (repro-first; mechanism UNCONFIRMED)

**Field report:** fix-caps in the metadata editor turns
`(Japan P-11356 Promo LP / 32-192)` into `(     P-11356 Promo LP / 32-192)` —
"Japan" replaced by BLANKS (not lowercased). `US`/`JP` survive. Affects other
leading tokens after `(` too.

**MECHANISM IDENTIFIED (for the lowercasing): `capitalize_word` is
punctuation-blind.** `capitalize_section` tokenizes on whitespace, so the first word
of a parenthetical arrives as `"(Japan"`. `capitalize_word`'s default arm
(`renaming.rs:~582-590`) uppercases the FIRST CHARACTER — which is `(`, a no-op —
and LOWERCASES THE REST → `"(japan"`. `"(US"`/`"(JP"` survive only by accident: the
acronym-preservation branch (:~565-570, len 2-5 + all uppercase-or-non-alphabetic,
and `(` counts as non-alphabetic) returns them verbatim. Not country recognition at
all. This also predicts the user's `(Foo Foo …)` → `(foo Foo …)` example exactly,
and the same flaw hits ANY token carrying leading punctuation (quotes, brackets).
The originally-transcribed "Japan → five blanks" symptom is not fully explained by
this (lowercase ≠ blanks) — reproduce the exact editor flow once to check whether
blanking was a secondary artifact or a distinct bug; the lowercasing defect is
proven regardless.

**Required:**
- `capitalize_word` must skip a leading non-alphanumeric prefix, case the
  alphanumeric core, and reattach the prefix (and same for trailing punctuation as
  applicable) — so `"(Japan"` → `"(Japan"`, `"(foo"` → `"(Foo"`. The
  first-word/after-`&`/small-word logic must judge the CORE, not the punctuation.
- Convention guard: the first word inside a parenthetical is always capitalized
  (falls out naturally once the prefix is skipped, since it's position-0 of its
  section — verify for the `capitalize_section`-on-whole-string path where
  `"(Japan"` sits mid-token-stream: the paren prefix must force first-word
  treatment, or route parentheticals through the `capitalize_title` paren-splitting
  path consistently).
- A case transform must NEVER remove, blank, or reorder tokens — casing only.
- Regression: the user's exact string `(Japan P-11356 Promo LP / 32-192)` (expect
  `Japan` intact and capitalized, `P-11356`/`LP`/`32-192` preserved),
  `(Foo Foo LP / 24-96)`, quotes/bracket-prefixed tokens, and the acronym cases
  `(US …)`/`(JP …)` still preserved — via BOTH `capitalize_title` and
  `capitalize_section` entry points and the metadata-editor fixcaps action.

## 7. Mouse text contract everywhere (standardize)

Round 3 delivered the full contract (click-to-place-cursor via
`place_cursor_from_view_column`, double-click select-all via `ClickTracker`,
single-click-deselects-and-places, drag-select — `text_input.rs:671-700`,
`click_timing.rs`, dispatcher `keybindings.rs:35456-35536`) for THREE surfaces:
browse file/tree inline edit and the path bar. The user wants it on ALL text
surfaces. Verified inventory of what lacks it (from research — confirm each):

| surface | today |
|---|---|
| Browse search input | cursor only; no click/drag/double-click; no selection rendering hit |
| Browse filter input | same |
| Metadata editor inline edit | keyboard only, no hit region |
| Metadata editor detail edit | keyboard only; right-click cancels |
| Convert metadata inline edit | keyboard only |
| Output options inline edit | keyboard only |
| Info pane metadata editing | click opens edit; no in-field mouse |
| Theme builder hex input | partial via overlay handler; no contract |
| Bulk rename template + per-line | keyboard only |
| TextEdit/FileInput/command overlays | keyboard only |

Required: every surface above gets a hit region and the full identical contract
(double-click selects all; double-click with selection selects all again;
single-click with selection deselects + places cursor; single-click without places
cursor; drag selects; selection rendered visibly per item 2's matrix). One shared
dispatcher generalized from the round-3 implementation — not N copies. Regression
per surface class.

## 8. Undo/redo for file operations (Ctrl+Z / Ctrl+Y)

Scope per user: paste (copy), move, rename — including bulk rename. **NO undo for
delete** (explicitly out of scope). Ctrl+Z/Ctrl+Y verified unbound everywhere.

Foundations already present (reuse, don't duplicate): completed paste mappings
(`PasteMapping` vectors, crate `state.rs:3045`; `completed_mappings()` on the
retained `FileTaskCompletionReport`, `progress.rs:285-302`); move recovery proofs
(`BrowseMoveRecoveryProof` source+destination manifests, `browse.rs:2176`;
`move_recovery_by_source`); the retained `last_file_task_progress` report; the
rename-first no-clobber pipeline itself (the inverse of a move IS a move).

Gaps to close: single/bulk rename currently record no reversible mapping
(`rename_plan.rs:189` `execute_plan` returns only a count; ops are marked in-place —
retain the executed plan's old→new pairs); an undo journal (in-session, bounded
depth — design yours) with per-entry staleness verification before undoing (the
retained manifests can prove the destination is still exactly what the operation
produced — refuse to undo if it changed, with a clear message).

Inverse semantics: undo move = move back via the same rename-first no-clobber
pipeline (cross-device moves reverse via the same copy-verify-remove machinery);
undo rename = rename back (no-clobber); redo = re-execute the recorded forward op.
**Undo of a copy-paste deletes the files the operation created** — that is a delete
of op-created artifacts only, gated on manifest verification that they are still
byte-identical to what the op produced, and it must present a confirm step the first
time (design the confirmation; it must not be a silent delete). Never touch
anything the operation didn't create. `:messages`/details surface records each
undo/redo like any other file task.

## 9. Editor select-all alternative + deselect chord

Field: byobu steals Ctrl+A during inline editing on the user's setup. Ctrl+A's
current editor behavior (select_all_text) STAYS for terminals that deliver it.

- **Ctrl+Shift+A does not work** (user asked): legacy terminal encoding transmits
  Ctrl+Shift+A and Ctrl+A as the same byte (0x01); distinguishing needs the kitty
  keyboard protocol, which the user's terminals don't reliably support. Ruled out.
- **Deselect-all: Ctrl+/ (user-decided).** Terminals transmit Ctrl+/ as 0x1F, which
  crossterm may surface as Ctrl+'/' or Ctrl+'_' depending on terminal — bind BOTH
  representations, and verify empirically on Windows Terminal + xfce4-terminal.
  In-editor: clears selection (cursor stays). Also consider it at browse-list level
  as a deselect-all alias alongside Esc's existing role — optional, note your choice.
- **Select-all alternative in editors:** pick from verified-free editor chords
  (Ctrl+L, Ctrl+G, Ctrl+T, Alt+A, Alt+U — the editor's readline block already uses
  Ctrl+B/F/H/D/W/U/K/E, and Ctrl+Z/Y are now undo/redo). RECOMMENDED: **Alt+A**,
  mirroring the browse-list's existing Alt+A select-all — one mnemonic across
  contexts. Your call with rationale if you deviate. Document in help surfaces.

## 10. Carried forward from round 3 (both small, both diagnosed)

- **Test hermeticity vs a live TUI:** theme-persist and bookmark-store tests fail
  when a live TUI runs concurrently (field-proven; green with the TUI closed).
  AUDIT REFINEMENT: isolation seams ALREADY EXIST — the theme test uses an
  `isolated_config_home` (`app.rs:~17164`) and the bookmark tests use a
  `TestConfigHome` XDG override + `BOOKMARK_ENV_LOCK` (`bookmarks.rs:~1208-1215`) —
  yet interference still occurred. The task is to FIND THE LEAK in the existing
  seams (env-var overrides racing across threads? a code path under test that reads
  the real path directly, bypassing the seam? process-global state the override
  doesn't cover?) and close it, with a regression that simulates a concurrent
  external writer.
- **`has_child_directories` verdict-cache staleness:** the fingerprint
  (`tree.rs:13-44`) already includes `ctime_nsec` on unix, so field granularity is
  NOT the gap. The identified vector is the non-atomic capture window in
  `has_child_directories` (`tree.rs:171-211`): the fingerprint is read FIRST, the
  directory can mutate, then the verdict is computed and stored paired with the
  stale pre-compute fingerprint — a later reader whose fresh fingerprint matches
  that stale one gets the wrong verdict. Fix the pairing (re-read the fingerprint
  after computing and store only if unchanged, or equivalent), and/or short-TTL
  negative verdicts. Keep the cache — it's a real interactivity win.

## 11. Constraints (standing)

- NO function keys; NO emojis/decorative unicode (functional ●○✓▲▼/▸▾/█░/▌ only);
  Ctrl+Q stays quit; chords must survive chord-stealing terminals where specified.
- Preserve all round-3 green behavior (5088/0 baseline); crate builds standalone;
  two-pass rendering; `cargo test --workspace` zero failures untruncated.
- Repro-first items (3, 4, and item 6's residual blanks question): the engineering
  report must name the actual mechanism found, with the failing-then-passing test.
- Deliverable: overlay tar.gz + MANIFEST (preimage hashes from THIS bundle) +
  ENGINEERING_REPORT + SHA256SUMS. Request missing files rather than guessing.

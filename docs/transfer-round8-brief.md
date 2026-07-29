# Transfer Round 8 — Full Carrier Matrix, Multi-FILE CUEs, Picker Selection Redesign

**Baseline:** branch `hardening` @ 612a6f3; `cargo test --workspace` =
5,295 passed / 0 failed across 56 targets. Version stays **0.4.4**.

**Field driver (user, 2026-07-28):** round-7 Transfer tags fails on the
user's real library: (a) their sidecar CUEs are one-FILE-per-track and hit
"multi-file CUE albums are not yet transfer targets/sources"; (b) picker
multi-select was unreachable (Ctrl+Space is byobu-hostile; Space confirms
instead of marking); (c) no range select; (d) the confirm button moved to
the left end, breaking positional habit. The user requires the FULL
source→target matrix (§4) plus a picker selection redesign (§1).

All citations verified at 612a6f3 by three research passes. Line numbers
are exact at that revision.

---

## §0 Carrier taxonomy truth — read this first

Two DIFFERENT multi-image CUE shapes exist, discriminated by
`SplitCueMemberRole` (src/convert/split_cue_album.rs:604-611), and they
have different natural write targets. Any code that lifts the round-7
fences MUST consult the role — lifting on image-count alone conflates
them.

- **`MetadataSidecar`** — every FILE maps 1:1 to one track (the user's
  field-typical shape: per-track files + one describing .cue).
  `admit_split_cue_member` (split_cue_album.rs:1277-1437) yields
  `referenced_audio` (distinct images, first-reference order) and
  `track_audio_paths` (per-track image, aligned with `sheet.tracks`).
  Today the metadata editor DECOMPOSES these albums into plain per-file
  editing and never touches the sidecar on save. The decompose
  mechanism, precisely: expansion into `ordinary_paths` at
  keybindings.rs:14461-14474; the plain tab attaches `cue_source` only
  for `audio_paths.len() == 1` surfaces (:16317-16320), so
  `cue_sidecar_writeback_plan_for_state` bails at :10091-10095; the
  `referenced_audio.len() == 1` preflight at :10096-10104 is a tertiary
  belt-and-braces fence. Per-track tag authority today = the files'
  own tags.
- **`SyntheticAlbumPart`** — at least one FILE owns >1 track (true
  gapless multi-part images). These open through the editor's UNIFIED
  ALBUM MODEL (`build_metadata_editor_for_cue_surfaces_with_policy`,
  keybindings.rs:15932-16148; the multi-FILE branch starts at :15981): the sheet is the SOLE per-track
  authority; save regenerates the full sheet
  (`cue_album_generate_synthetic_cuesheet` :15586-15739 — emits FILE
  lines on image change, renumbers positionally), writes it as an
  embedded CUESHEET tag into EVERY member image, force-deletes replaced
  per-file keys, and (when the sidecar was the policy-selected source)
  rewrites the sidecar through the byte-span engine after ALL member
  saves succeed (:10123-10185, :10239-10251).

**Engine fact (load-bearing):** the byte-span sidecar rewrite engine
(`rewrite_cue_sidecar_metadata_from_cuesheet(_validated)`,
src/convert/cue_parser.rs:467-526) is ALREADY FILE-geometry-agnostic —
`cue_metadata_layout` (:1602-1650) scopes purely by `TRACK NN AUDIO`
headers; FILE lines are ignored except as the album-header insertion
anchor (:1612-1617); track pairing is by authored TRACK number with
count-equality and duplicate checks (:696-754). The unified-album save
already exercises it on multi-FILE sheets in production. The replacement
text built by transfer (`cue_metadata_replacement_text`,
src/tui/tag_interchange.rs:2312-2388) emits no FILE/INDEX lines and is
geometry-compatible as-is. **The single-image assumption lives ONLY in
the transfer validator** `validate_sidecar_transfer_snapshot`
(tag_interchange.rs:2394-2440, the `referenced_by_identity.len() != 1`
check at :2425-2432) and in carrier construction
(`TransferCarrier::SidecarCue.image_path` is single,
tag_interchange.rs:34-35).

**Policy seam truth (unchanged from round 7):** admission is policy-free;
`DEFAULT_FRONTEND_CUE_POLICY = PreferSidecar`
(src/tui/cue_parser.rs:78-79) is consumed only at
`resolve_metadata_cue_source` (keybindings.rs:14118-14174) via
`default_transfer_cue_policy()` (:14490-14492). All policy variants
(`SidecarOnly`, `EmbeddedOnly`, `PreferEmbedded`) are ALREADY implemented
there with error strings (:14152-14172). Round 8 threads per-gesture
policy through this seam (§3); it does NOT add the config setting (still
future, per the [design stake] — this round is the gesture-level
installment).

---

## §1 Picker selection redesign (crates/tui-file-picker)

### 1.1 Space becomes toggle-mark + advance (files pane only)

- `handle_file_key` (input.rs:484-587): the unmodified-Space arm (:564,
  currently `accept_current_selection`) becomes: toggle mark on the
  cursor row (`toggle_current_multi_selection`) then advance the cursor
  one row (mc/ranger convention). Files and FilesOrDirectories modes.
- **Directories mode keeps Space = confirm** (pinned by
  `space_accepts_current_directory_in_directory_mode` input.rs:2011-2027
  — keep that pin; marks are files-only and a Directories-mode picker
  has nothing to mark). State this asymmetry in the help/footer hint.
- Space's confirm/activate meaning in OTHER surfaces is UNTOUCHED: tree
  pane (:419), Properties (:760), DeleteConfirm (:779),
  SaveOverwriteConfirm (:829), text inputs (literal space).
- **Ctrl+Space stays as a toggle-mark alias** (:534) — never the only
  gesture (byobu/NUL). Ctrl+A mark-all stays (:530). Esc mark-clearing
  stays (:487-491).
- Directories are markable as before; the confirm-time directory filter,
  sorted visible-order emission, and "(N directories ignored)"
  accounting are UNCHANGED (state.rs:1223-1237, :2343-2365).
- Confirmation gestures after this change: the toolbar confirm button
  (§1.3), and Enter on a file row (single-path `Selected` — Enter
  ignores marks by round-7 design, state.rs:2327-2341; KEEP that). There
  is no keyboard chord that emits `SelectedMany` after Space is
  repurposed — ADD ONE: **Alt+Enter = confirm selection** (same path as
  the toolbar button, `accept_current_selection`). PLACEMENT
  (audit-forced): the plain-Enter arm at input.rs:563 has NO modifier
  guard, so Alt+Enter matches it TODAY (behaves as plain Enter) — the
  new Alt+Enter arm MUST precede :563 or it is dead code. Host-side is
  clean: both funnels forward the raw KeyEvent
  (keybindings.rs:6924, :11878) and no global intercept touches
  Alt+Enter. Byobu-safe precedent: Alt+O/Alt+Left/Alt+Right shipped
  (:565/:492/:522).

### 1.2 Range select

- **Alt+Click = primary range gesture** (byobu-safe, user-verified).
  In `apply_click_action` (input.rs:836-937), the FileRow arm (:873-891)
  currently inspects only CONTROL (:877). Add BEFORE the CONTROL branch:
  if ALT (or SHIFT — secondary alias for non-multiplexed terminals) is
  set: range-mark from the anchor to the clicked row (inclusive, in
  VISIBLE order), set cursor to the clicked row, and MIRROR THE CONTROL
  BRANCH's epilogue including `self.last_click = None` (:878) — 
  `classify_click` records the click at :858 BEFORE the arm runs, so
  returning without clearing makes the NEXT plain click on that row
  classify as a double/delayed-repeat (opens the file or starts a
  rename).
- **Range anchor = new dedicated field** (e.g.
  `range_anchor: Option<PathBuf>`). It must NOT live in click/pointer
  state — `handle_key` clears those on every key (input.rs:93-96).
  Anchor semantics (EXACT rule — audit-forced): the anchor is the row
  of the most recent mark/unmark gesture or plain click — Space sets the
  anchor to the row it TOGGLED (not the post-advance cursor row); a
  plain click sets it to the clicked row; plain cursor movement does NOT
  move the anchor; a range gesture does NOT move the anchor (repeated
  Alt+Clicks extend from the same origin). Cleared on directory
  change/refresh prune (state.rs:1996-1997 region) and Esc.
- **Keyboard range (byobu-safe): `v` visual toggle.** Plain `v` in the
  files pane anchors visual mode at the cursor; Up/Down/PageUp/etc.
  extend the marked range live; a second `v` or Space commits the range
  as marks and exits visual mode; Esc exits without marking.
  PRECEDENCE (audit-forced, state explicitly in code): the visual-mode
  arms run BEFORE the normal Space-toggle arm; the commit gesture does
  NOT additionally toggle or advance at the cursor row; Esc order is
  visual-exit → mark-clear (:487) → Cancelled (:491). COST: only
  LOWERCASE `v` is sacrificed from type-ahead (input.rs:579-584;
  uppercase `V` stays type-ahead via the SHIFT arm :580); disclose in
  the engineering report. **Shift+Up/Down as a secondary alias**
  (extend range) for terminals that pass it — never the only path
  (byobu steals Shift+arrows).
- Range marking is ADDITIVE to the existing mark set (union), matching
  mc semantics. Ctrl+A/invert/deselect unchanged (state.rs:1277-1293).

### 1.3 Confirm button: right-aligned, reserved width

- The round-7 priority placement (confirm FIRST, render.rs:230-237) is
  REPLACED: confirm moves to the RIGHT END of the toolbar — the position
  the pre-round-7 `Select Folder` always occupied — with RESERVED width
  so it can never be clipped out.
- Layout: compute the confirm button width first; the left-to-right run
  (Back/Forward/Up/…, render.rs:238-293) gets a budget of
  `toolbar_right − confirm_width − gap` and hard-clips within it (tail
  buttons clip, as today); confirm renders at the fixed right edge. The
  Go button already uses this exact right-anchor pattern on the address
  row (render.rs:333-345) — generalize it, don't invent new machinery.
- Keep `record_toolbar_button_geometry` registration for AcceptSelection
  (state.rs:1883-1896) — menu anchoring and hit-testing depend on the
  registry (render.rs:1044-1047). The relocated confirm gets NO `│`
  separator (the round-7 one at render.rs:236 goes away with the
  priority block); the right-edge gap is 2 cols.
- Tests to UPDATE deliberately: `contextual_confirm_remains_visible_at_
  embedded_picker_minimum_width` (render.rs:2628-2657 — assertions still
  hold; update the comment/message pinning the "place first" mechanism);
  `toolbar_hit_regions_are_clipped_to_the_visible_toolbar_area`
  (render.rs:2148-2191). ADD a pin: at 56 cols (the true embedded
  minimum, src/tui/draw_overlays.rs:5399-5405 `.max(56)`) the confirm's
  rect ends at the toolbar's right edge AND `File Operations` clips
  partially (audit-verified arithmetic: the Files-mode left run
  Back/Forward/Up/sep puts File Operations at x-offset 34-53; with
  "Select File" (13 wide) + 2-col gap reserved, the left budget is ~41,
  so File Operations clips and Search/Properties/Bookmarks vanish).
- Labels unchanged (`selection_confirmation_label`, state.rs:1240-1259).

### 1.4 Mark lifecycle hardening (round-7 audit debts)

- **Path-equality unification:** the refresh prune uses exact `PathBuf`
  equality (state.rs:1997) while rendering uses canonicalizing
  `same_path` (:1203, :5706-5713). Unify on ONE comparison for the mark
  set (recommendation: store marks as the exact entry paths the picker
  itself listed — then exact equality is correct everywhere and the
  per-frame canonicalize in `is_path_multi_selected` (render.rs:802) can
  drop to exact compare, fixing the O(n·syscall) render cost too).
- **Marked-then-hidden disclosure:** a marked file removed from
  visibility by filter/hidden-toggle is silently pruned
  (state.rs:1996-1997) or silently omitted at confirm. Add an
  "N marked file(s) no longer visible were dropped" one-shot channel
  beside `last_selection_ignored_directories` (state.rs:1027-1029) OR
  prune eagerly on filter/hidden change with a status line. Either way:
  no silent drops.
- Pin the user-visible "(N directories ignored)" suffix end-to-end
  through `handle_message` (round-7 gap: only the picker-level count and
  message field are pinned, keybindings.rs:47917).

### 1.5 Preserved contracts (do not regress)

- `SelectedMany` emission only from explicit confirm; visible-sorted,
  files-only output (state.rs:9050, :9076 pins).
- All 10 reducer arms / 12 purpose variants; the 10 non-transfer
  purposes keep first-path compat (keybindings.rs:47862 pin enumerates
  exactly those 10).
- Right-click marked-set contracts (input.rs:2141, :2183 pins);
  delayed-repeat rename gate `multi_selected.len() <= 1` (input.rs:885).
- Type-ahead for all letters except the deliberate `v` sacrifice.

---

## §2 Multi-FILE CUE carriers

### 2.1 Lift the two fences, role-aware

The fences at keybindings.rs:14504-14508
(`transfer_carrier_from_admitted_surface`) and :14547-14551
(`transfer_carrier_from_explicit_cue`) are lifted. Carrier construction
consults the role:

- **`MetadataSidecar` (one FILE per track)** → new carrier shape. Widen
  `TransferCarrier::SidecarCue` with per-track image ownership (e.g.
  `image_paths: Vec<PathBuf>` == `referenced_audio` +
  `track_audio_paths: Vec<PathBuf>`), or an equivalent new variant —
  model's choice, but the validator and write seams below consume the
  vector, plus a WRITE-METHOD marker recording the gesture (sidecar-only
  vs per-file+sidecar, §2.3) — neither `TransferCarrier` nor
  `PreparedTagTransfer` (browse.rs:2304-2313) records the gesture today
  and both the executor and the confirm prompt (§4.4) need it.
  **ORDERING TRAP (audit-found):** `admit_split_cue_member` builds
  `track_audio_paths` in PARSE order (split_cue_album.rs:1416), but
  transfer planning and replacement text are TRACK-NUMBER-sorted
  (tag_interchange.rs:2000-2001, :2366-2368, gapped/non-monotonic
  numbering accepted). The carrier MUST re-sort the vector by authored
  TRACK number at construction, or value i writes to the wrong file for
  out-of-order-authored sheets. Pin with an out-of-number-order fixture.
  Sheets with DUPLICATE authored TRACK numbers (restart-at-01-per-FILE
  authoring exists in the wild) pass admission but the engine refuses at
  pairing (cue_parser.rs:714-731) — honest refusal, sidecar untouched;
  under §2.3 ordering the member files may already be written when the
  sidecar refuses; disclose this in the report and the failure status.
  `dimension()` stays `Tracks(sheet.tracks.len())`.
- **`SyntheticAlbumPart` (true multi-part images)** → SAME widened
  carrier (the write seam differs only in §2.3's per-file arm being
  absent). Role travels with the carrier.
- Explicit `.cue` pick, folder pick, and member-image pick must all
  reach the same carrier for the same album (carrier-consistency rule
  extends to multi-FILE). Member-image gesture: `by_image` currently
  indexes only single-image surfaces (keybindings.rs:14631-14636) —
  index multi-FILE members too, so picking any member file resolves the
  album (today it silently degrades to a single-file Files carrier).

### 2.2 Reads (source side) — trivial

`cue_sheet_transfer_entries` (tag_interchange.rs:1997-2048) is already
FILE-agnostic (sheet-derived TITLE/ARTIST/ISRC per track + album
fields). The sheet is the read authority for CUE carriers — per-file
tags of members are NOT read (consistent with round 7; disclose the
divergence in the report). NOTE the parser's performer inheritance
(src/convert/cue_parser.rs:2216-2222): absent per-track PERFORMER reads
as the album performer — acceptable, already round-7 behavior for
single-image.

### 2.3 Writes (target side)

- **Sidecar text**: through the EXISTING validated engine —
  `rewrite_cue_sidecar_metadata_from_cuesheet_validated` with
  `validate_sidecar_transfer_snapshot` GENERALIZED: replace the
  "resolves to exactly one expected image" check (:2425-2432) with
  "re-resolved per-track image vector equals the classification-time
  `track_audio_paths`" (mirror the unified-save preflight,
  keybindings.rs:10148-10166). `cue_track_geometry_matches`
  (:2442-2458) is already multi-FILE-safe as an equality check. The
  engine itself needs NO changes (§0).
- **Per-file tags (`MetadataSidecar` targets only)**: after the sidecar
  write plan is validated, per-track values ALSO write to each member
  file's own tags via the EXISTING Files write engine
  (tag_interchange.rs:2235-2297 loop +
  `write_all_tags_for_transfer_at_verification` probe.rs:8353-8367),
  pairing track i → `track_audio_paths[i]`. Rationale: for these albums
  the files' own tags are today's authority (§0) — writing only the
  sidecar would update text nobody reads. FIELD-SET RULE (audit-forced —
  the two arms use DIFFERENT plans): the per-file arm plans at
  Files-dimension (full field set: ISRC, SONGWRITER, COMMENT, etc. flow
  to member files exactly as they would to plain Files targets, with the
  same numbering-skip rules); the sidecar arm plans at Tracks-dimension
  (CUE field cap applies: tag_interchange.rs:1870-1891). A single
  Tracks-capped plan would silently starve member files of every field
  outside the cap — forbidden. The confirm prompt reports both counts
  (§4.4). ORDER + partial-failure contract (mirrors unified-model
  precedent keybindings.rs:10239-10251): per-file writes first; the
  sidecar rewrite runs ONLY if every per-file write succeeded; on
  partial failure report per-path failures and "sidecar left unchanged
  (N member write(s) failed)". Album-level fields write to every member
  file (as Files targets do today) and to the sidecar header.
- **`SyntheticAlbumPart` targets**: sidecar text ONLY this round (the
  sheet is the authority; per-file tags don't exist for these). Embedded
  CUESHEET fan-out to member images (the unified model's save shape) is
  FENCED this round — refuse "transfer to a multi-part image album
  updates the sidecar only; open the album in the metadata editor to
  regenerate embedded sheets" (honest wording, model may improve).
- **Explicit `.cue` gesture** writes the sidecar text ONLY (no per-file
  fan-out) — the user's stake: ".cue selected → write to the sidecar
  CUE". The folder/member-image gestures use the default method =
  per-file + sidecar for `MetadataSidecar` (§2.3 above), sidecar-only
  for `SyntheticAlbumPart`. Disclose the gesture difference in the
  confirm prompt (§4.4).

### 2.4 Embedded multi-FILE CUESHEET

- Classification today HARD-ERRORS ("embedded CUE cannot reference
  multiple audio files", keybindings.rs:14040-14043 surfaced via
  :14830-14833). Round 8: as a SOURCE, admit it read-only; as a TARGET,
  refuse gracefully with the :9318-9319 rationale (a single member
  cannot authoritatively rewrite a sheet naming siblings).
  **SEAM (audit-forced — the naive lift leaks):**
  `validate_embedded_cue_sheet_for_metadata` is shared with the EDITOR's
  policy resolution (via `embedded_cue_candidate_for_metadata_at`
  :14094, consumed at :14187) — do NOT relax it. And
  `classify_tag_transfer_roots` is direction-agnostic (the same call
  classifies source and target, context_menu.rs:1884 vs :1888) — the
  source/target distinction CANNOT live in classification. Instead:
  transfer classification catches the multi-FILE rejection specifically
  and constructs an EmbeddedCue carrier flagged multi-FILE (or an
  equivalent distinct state); reads proceed (sheet → entries is
  FILE-agnostic); `execute_tag_transfer_to_cue` refuses flagged targets
  at write-dispatch with the honest message. The editor's validator,
  read-only fence, and transfer-into-editor gates (:21093, :29346) are
  UNCHANGED.

### 2.5 Pins to flip/extend

- `explicit_cue_classification_refuses_invalid_fenced_and_mixed_
  selections` (keybindings.rs:53535): the multi-FILE arm (:53603-53605,
  fixture :53591-53601) flips from expect_err to a positive carrier
  expectation (role `MetadataSidecar`, track_audio_paths aligned).
- `directory_cue_and_image_gestures_resolve_the_same_sidecar_carrier`
  (:53659): §3 SPLITS this pin into three single-image arms (see §3
  PIN FLIP); the multi-FILE twin required here is ADDED ALONGSIDE those
  arms — for multi-FILE albums all three gestures (folder, .cue,
  member-image) converge on the same carrier BECAUSE album resolution
  wins over the image-gesture policy (§3 precedence caveat (b)). One
  test family, four arms total.
- `validate_sidecar_transfer_snapshot` tests (tag_interchange.rs
  :1509-1563 region): add multi-FILE vectors (member renamed → refuse;
  member list changed → refuse; untouched → pass).
- New: per-file + sidecar ordering pin (partial member failure leaves
  sidecar byte-identical); explicit-.cue-writes-sidecar-only pin.

---

## §3 Explicit-carrier override at the policy seam

The gesture now carries intent (first installment of the configurable
default-method stake; the Config setting remains future):

- **Explicit `.cue` pick** ⇒ KEEP today's bypass exactly
  (`transfer_carrier_from_explicit_cue` :14535-14572 performs no
  resolution, no tag read, no embedded-text substitution — the sidecar
  TEXT is the authority; the §2.1 fence lift INSIDE this function
  still applies). Do NOT route it through
  `resolve_metadata_cue_source`: that would inherit `sidecar_source()`'s
  embedded-text substitution (:14124-14135) and add a tag read. This is
  a deliberate semantic choice, not an omission.
- **Explicit image-file pick** ⇒ resolve with **`PreferEmbedded`** (NOT
  EmbeddedOnly — audit-forced): image carries an embedded CUESHEET →
  EmbeddedCue (creates the missing gesture for "write the embedded CUE
  of an album that HAS a sidecar"; round-7 gap: `by_image` reroutes to
  the sidecar via PreferSidecar, keybindings.rs:14818-14822 +
  :14511-14515); image has NO embedded sheet → falls back to the
  sidecar carrier, PRESERVING today's working image-pick→sidecar row
  (EmbeddedOnly would have turned it into a refusal).
  `transfer_carrier_from_admitted_surface` already constructs the
  EmbeddedCue variant when resolution identity is Embedded
  (:14525-14531) — no new construction needed. CAVEATS: (a) the
  FLAC-only embedded write cap (tag_interchange.rs:2583-2591, pure
  extension check — classification uses the same extension mapping,
  classify.rs:164-177, so early refusal is safe and probe-free) must
  surface at CLASSIFICATION time for image-pick targets that resolve
  Embedded; (b) PRECEDENCE: a member-image pick of a MULTI-FILE album
  resolves the ALBUM per §2.1 — album resolution WINS over the gesture
  policy; consequently a member's own single-file embedded CUESHEET is
  unreachable by any gesture (deliberate; state in the report).
- **PIN FLIP (audit-forced):** the round-7 pin
  `directory_cue_and_image_gestures_resolve_the_same_sidecar_carrier`
  (keybindings.rs:53658-53718) asserts the image gesture yields the
  SAME sidecar carrier — its fixture has a structurally-matching
  embedded sheet, so PreferEmbedded BREAKS it by design. Split it into
  three pins: (a) directory + .cue gestures → same SidecarCue
  (unchanged); (b) image gesture WITH embedded sheet → EmbeddedCue
  (new intent); (c) image gesture WITHOUT embedded sheet → the same
  SidecarCue as (a). The §2.1 carrier-consistency rule is hereby
  SCOPED: it binds the directory and .cue gestures; the image gesture
  deliberately expresses embedded intent when an embedded sheet exists.
- **Folder pick** ⇒ default method (today: `default_transfer_cue_policy()`
  = PreferSidecar), UNCHANGED seam (:14490-14515) — the future config
  cascade replaces the value here.
- Implementation: thread a per-root policy override into
  `transfer_carrier_from_admitted_surface` /
  `transfer_carrier_from_explicit_cue` call sites; admission stays
  policy-free (two-layer truth preserved).
- STRENGTHEN the round-7 tautological pin: replace
  `transfer_resolution_consults_the_frontend_default_cue_policy`
  (keybindings.rs:53388-53394, asserts a function equals the constant it
  returns) with a routing test. Once the policy parameter is threaded,
  NO injection machinery is needed: call
  `transfer_carrier_from_admitted_surface` directly with `PreferSidecar`
  vs `PreferEmbedded` on a sidecar+embedded fixture (the :53664-53689
  fixture pattern exists) and assert DIFFERENT carrier variants. This is
  the swappability guarantee the round-7 brief demanded and didn't get.

---

## §4 Matrix completion + planner guards

### 4.1 The target matrix (all rows must work or refuse honestly)

With §1-§3 landed, the user's 9 rows resolve as: folder→files,
folder→sidecar/embedded, files→folder, files→cue, sidecar→sidecar,
sidecar→embedded (via image-pick gesture), sidecar→files,
sidecar→folder, embedded→all — each pairing through the existing planner
(`plan_transfer_values_for_dimensions_with_collapse`,
tag_interchange.rs:1818-1929). Equal-count cross-dimension pairing
(Files(n)↔Tracks(n)) is already legal (:1825, :1827) and REMAINS the
mechanism for files↔cue rows.

### 4.2 Pairing corroboration guard (new)

Files carriers are PATH-SORTED (command.rs:242, keybindings.rs
:14941-14947); CUE carriers are TRACK-NUMBER-sorted
(tag_interchange.rs:2000-2001). Files(n)↔Tracks(n) therefore pairs
filename order against track order on count equality alone. ADD a
corroboration check, PLACED as follows (audit-forced — tag availability
differs by side):
- SOURCE-side Files carrier: at PLAN time — source tags including
  TRACKNUMBER are already read (`read_transfer_source_entries`
  tag_interchange.rs:2076-2110); zero new I/O.
- TARGET-side Files carrier: at EXECUTE start — target tags are first
  read there (:2225-2267); classification never reads
  directory-expanded files (:14666-14680 seeds only file roots and
  admitted images). A post-confirm refusal is acceptable and honest;
  do NOT add a classification-time batch read.
Precedence when both exist: the TRACKNUMBER tag wins over the filename
prefix. Reuse existing parsers — `strict_track_number_from_dispatch_path`
/ `has_strict_track_prefix_separator` (src/convert/processor.rs:1197,
:1219) and `leading_track_number`
(src/tui/preemphasis/metadata.rs:531) — do NOT write a third parser
with new acceptance rules. Rule: when every file yields a number and
that order DISAGREES with the path-sort order used for pairing, refuse
("file order and track numbers disagree; renumber or rename before
transferring"). When numbers are absent/unparseable, pair positionally
as today but append a disclosed warning ("paired by filename order; no
track numbers to corroborate"). No silent misalignment.

### 4.3 Directory-with-embedded-only degradation (fix)

A folder whose only album shape is an embedded-CUE image classifies
Files(1) today (embedded candidates are consulted only for audio-file
roots and admitted-surface images, keybindings.rs:14666-14680, never for
the directory fallback :14792-14801) — inconsistent with picking the
image directly. Extend the directory branch: when the bounded fallback
finds exactly one audio file and that file carries a valid ≥2-track
embedded CUESHEET, classify EmbeddedCue (same as the direct image pick).
IMPLEMENTATION NOTE (audit-found): the directory-expanded file is NOT
in the embedded-candidates map — extend `read_transfer_embedded_
candidates` seeding (or read the single file's tags at that branch);
this is a new bounded read of ONE file, acceptable. Multiple
embedded-CUE images in one folder → refuse with the existing "multiple
CUE albums" wording. KNOWN LIMITATION (disclose, don't fix this
round): a folder with SEVERAL audio files where exactly one carries an
embedded CUE (image + accidental extras) stays Files(n) — the same
inconsistency one file removed; recorded for a future round.

### 4.4 Confirm-prompt clarity

`PreparedTagTransfer::confirmation_prompt` (src/tui/browse.rs:2316-2338,
exactly three arms today) gains arms for the new shapes and must name
the WRITE FAN-OUT explicitly, e.g.:
- "Write N file field(s) + K CUE field(s) to M file(s) + sidecar CUE
  (T tracks)?" (multi-FILE MetadataSidecar via folder/member gesture —
  the two counts come from the two §2.3 plans)
- "Write K field(s) to sidecar CUE only (T tracks)?" (explicit .cue)
- "Write K field(s) to embedded CUE (T tracks)?" (image gesture)
REQUIRED FIELD (audit-forced): neither `PreparedTagTransfer`
(browse.rs:2304-2313) nor `TransferCarrier` records the gesture/
write-method today — the §2.1 write-method marker is what both the
prompt and the write dispatcher consume; without it these prompts are
unbuildable.
The silent folder-with-sidecar→sidecar surprise (single-image albums)
is mitigated by the same wording — the prompt already says "sidecar
CUE"; keep it, no behavior change for single-image.

### 4.5 Files-target TOCTOU at confirm (fix)

CUE targets re-validate at write; Files carriers classified at prepare
time are executed against the stale list after the blocking confirm
(context_menu.rs:2057-2065). Re-expand/re-verify Files target roots at
confirm accept: if the resolved path set differs from the prepared set,
refuse with "target folder changed since the confirmation was prepared;
retry" (cheap: compare sorted path vectors; the re-expansion is the
bounded `expand_audio_paths_for_transfer_limited` walk — acceptable
synchronously in `launch_prepared_tag_transfer`, context_menu.rs
:2021-2078). THREADING (audit-forced): the original target ROOTS
survive only in `PendingTagTransferTarget::Roots` (browse.rs:2288-2291)
and are DROPPED when `PreparedTagTransfer` is built (it carries only
the classified carrier) — thread the roots into `PreparedTagTransfer`
or re-verification is impossible.

### 4.6 Explicitly UNCHANGED

- 1→N broadcast semantics (numbering skipped, disclosed suffix).
- Tracks(*)→Files(1) stays refused on disk paths (first-track collapse
  remains editor-From-only; revisit on field demand).
- CUE field cap, SONGWRITER exclusion, ISRC read-only, TRACKNUMBER
  structural (tag_interchange.rs:1870-1891).
- FLAC-only embedded writes (now surfaced earlier, §3).
- Disc images refused; archives refused.

---

## §5 Round-7 audit debts folded in

1. Policy-swappability pin → real routing test (§3).
2. `classify_file_maps_supported_audio_extensions_case_insensitively`
   (src/convert/classify.rs:210-227) — restore exact per-extension
   format assertions (round 7 weakened them to `AudioFile(_)`).
3. cue+cue multi-select refusal wording (keybindings.rs:14886-14890
   says "mixes a CUE with audio files" for two CUEs) — accurate message.
4. Unused `_verification` on
   `write_embedded_cuesheet_for_transfer_at_verification`
   (probe.rs:8377) — consume or rename with rationale.
5. Non-transfer pickers consuming only `paths.first()` of a multi-mark:
   with Space-marking, accidental multi-marks in single-path pickers get
   MORE likely. When a single-path purpose receives SelectedMany with
   len > 1, append an honest status "(first of N selected files used)".
   INSERTION POINT (audit-forced): NOT the send funnel
   (keybindings.rs:11900-11914) — reducer arms set their own statuses
   afterward and would clobber it. Use the post-reducer append site,
   exactly like `append_ignored_directory_disclosure`
   (event_loop.rs:870-885, invoked at :5205-5206 AFTER
   `reduce_file_picker_complete`): append the suffix there, gated on a
   single-path-purpose predicate; `paths` is already in the message.

---

## §5A Field fix: tolerant APEv2 reads + writes (WavPack)

**Field case (user, 2026-07-28):** `~/livetorrents/Supertramp – Even In
The Quietest Moments...-1977` — seven valid WavPack files (32-bit/384k)
whose APEv2 tags carry the item key `&год` (Cyrillic; a Russian
tagger's "year"). APEv2 keys must be printable ASCII; lofty hard-fails
the ENTIRE read: `Ape: APE tag item key contains invalid characters`.
The editor shows "unreadable: failed to read '…'" and MB tagging is
blocked. EMPIRICALLY VERIFIED: lofty 0.21 fails in ALL parsing modes —
`ParsingMode::BestAttempt` AND `ParsingMode::Relaxed` both return the
same error (tested against the field files), so no lofty configuration
fixes this; the fallback must be tonepoet-native. Because lofty writes
are read-modify-write, WRITES to such files fail too.

Requirements:
- **Tolerant native APEv2 read fallback.** When a lofty read fails and
  the error classifies as an APE tag error, parse the APEv2 tag
  natively: locate the 32-byte APE footer at EOF (accounting for an
  optional trailing ID3v1 block), walk items (u32 value-len, u32 flags,
  NUL-terminated key, value), ACCEPT spec-valid items, SKIP invalid-key
  items while retaining their raw bytes, and surface the valid items as
  tag entries plus a disclosed per-file issue naming the skipped key(s)
  (display-escaped), e.g. "1 invalid APE key skipped: '&год'". The
  fallback engages ONLY on APE-classified lofty errors — all other
  failures keep today's honest unreadable state. Seams + error
  classification (verified): at probe.rs:7370-7380 the TYPED
  `lofty::error::LoftyError` is available — the field error is
  `ErrorKind::FileDecoding` with `.format() == Some(FileType::Ape)`
  (lofty-0.21.1 ape/tag/item.rs:37 emits exactly the observed
  message), a clean discriminant. The :7301 seam stringifies via
  `map_err` — hook the fallback BEFORE that erasure (one-line local
  restructure, no refactor). Read fallback is container-agnostic (APE
  tag at EOF) and may serve .wv/.ape/.mpc.
- **Write path must not fail closed (scoped to .wv this round).** Add a
  native APE write shape for WavPack files whose tag lofty cannot read:
  rebuild the APEv2 tag from (valid items merged with the user's
  changes) + (invalid-key items PRESERVED byte-for-byte, never silently
  dropped), rewrite atomically (temp + rename mirroring the existing
  FLAC-overflow guards: symlink rejection probe.rs:2290-2300,
  hardlink nlink>1 :2308-2318, permissions/ownership/timestamps
  snapshot restore :9506-9548), routed through
  `metadata_persistence_route_for_path` — probe.rs:8442-8458 is the
  DISPATCH match; the function and the `MetadataPersistenceRoute` enum
  live in src/metadata_persistence.rs:105-110/:235-245, where the new
  route arm is added (.wv hits the `Lofty` catch-all today; .ape/.mpc
  keep read-fallback only and refuse writes honestly this round). Do NOT shell out to wvtag — in-process consistency and
  the verification split govern.
- **MB flow unblocked:** with the read fallback, tags-from-MusicBrainz
  proceeds on such albums (the read supplies the entries the flow
  needs); no MB-side changes expected.
- Pins: a constructed .wv fixture with an invalid-key APE item — read
  succeeds with the disclosure and valid items intact; a field write
  succeeds and the invalid item's raw bytes survive byte-identically;
  a non-APE lofty failure still reports unreadable (fallback does not
  over-trigger). The user will field-test on the Supertramp album.

## §5B Field fix: system-clipboard publication for ALL copies (SSH)

**Field case (user, 2026-07-28, SSH + byobu):** copying from the
`path:` bar or copying individual tag fields never reaches the system
clipboard. User directive: "a copy or cut in tonepoet [must] make the
contents available in both the app and the ssh system's clipboards."

Current truth (verified):
- Round 6 shipped the dual-plane seam `publish_text_clipboard`
  (context_menu.rs:3222-3226: app text clipboard + best-effort OSC 52
  via `write_osc52_clipboard_to` :3231-3243, 64KiB size gate pinned at
  :4075). Its production callers are Copy tags AND the round-6 editor
  field/row copies (`metadata_editor_copy_selected_rows`
  keybindings.rs:12917; editor cut :12930-12938 routes through it) —
  those are ALREADY dual-plane.
- The browse context-menu "Copy path" emits its own INLINE OSC 52
  (context_menu.rs:2384-2391, pre-round-6 duplicate of the encoder).
- The app-clipboard-ONLY writers are the TEXT-INPUT copies (the path
  bar, search, save-name, bookmark fields —
  `TextInputState::copy_selection` text_input.rs:227-228 →
  `write_shared_text_clipboard` :24; `cut_selection` :232-238 routes
  through copy_selection, so one hook covers both).
- Field diagnosis, precisely: the path-bar failure = the text-input
  gap above; the TAG-FIELD failure = the copies DO emit OSC 52 but
  bare OSC 52 is swallowed by the user's byobu/tmux — the multiplexer
  passthrough requirement below is the fix for that half. The editor
  "publishes both planes" pin below is therefore a REGRESSION pin on
  existing behavior, not new work — the report must present it as
  such.

Requirements:
- **One publish authority.** Every app-clipboard text write (copy AND
  cut) also attempts OSC 52. Mechanism: the picker crate gains an
  optional publish hook (static, set once by the host at startup —
  e.g. `set_shared_clipboard_publish_hook(fn(&str))`) invoked by
  `write_shared_text_clipboard`; the host installs the OSC 52 emitter.
  App-side call sites (editor copies, CopyPath, Copy tags) route
  through `publish_text_clipboard`, which itself writes the shared
  clipboard — after this round there is exactly ONE emit helper; the
  inline duplicate at context_menu.rs:2386-2391 is deleted.
- **Multiplexer passthrough.** The user runs byobu/tmux: bare OSC 52
  from an inner app reaches the outer terminal only when tmux's
  `set-clipboard` allows it (default `external` in tmux >= 3.2 works;
  byobu profiles may override). When `$TMUX` is set, ALSO emit the
  tmux-passthrough-wrapped variant (`\x1bPtmux;` + escaped payload +
  `\x1b\\`; honored when `allow-passthrough` is on) — belt and
  braces, both are no-ops when unsupported. Document the byobu
  requirement (`set -g set-clipboard on` / `allow-passthrough on`) in
  the engineering report and the in-app help.
- The size gate stays; oversized payloads still land in the app
  clipboard with today's semantics. System-clipboard READ remains
  impossible app-side (round-6 constraint, unchanged) — do not attempt.
- Pins: hook fires on text-input Ctrl+C and Ctrl+X (captured via an
  injected writer/hook in tests); editor field copy publishes both
  planes; CopyPath routes through the unified helper (inline encoder
  gone); size gate preserved; cut publishes like copy.

---

## §6 Fences (unchanged this round)

Custom tag builder + Paste tags execution (next round — user has
mockups); the Config cascade setting (future; §3 is the gesture-level
installment); library; disc images; SACD/archives; ISRC/SONGWRITER
writeback; embedded CUESHEET fan-out for SyntheticAlbumPart targets;
first-track collapse on disk paths; multi-FILE embedded CUESHEET as a
WRITE target; gnuDB endpoint. NO F-keys; NO emoji/decorative unicode
(functional set only); Ctrl+Q stays quit; new bindings scoped to the
picker surface; version stays 0.4.4; never truncate gate output;
`:messages`, verification split, rounds 5-7 machinery must not regress.

## §7 Deliverables

Overlay bundle (tar.gz, nested dir, preimage manifest with SHA-256 of
exact base revisions) + engineering report with: per-item named pinning
tests (minimum: the §1.3/§1.4/§2.5/§3/§4.2-4.5/§5A/§5B pins), the implemented
carrier matrix as a table, the role-discriminated write-fan-out contract
stated, disclosed limitations (type-ahead `v` sacrifice, sheet-vs-file
read divergence, SyntheticAlbumPart sidecar-only, embedded multi-FILE
read-only), and any deviation with rationale. `cargo test --workspace`
green against 5,295/0; new tests must FAIL if the behavior they pin
regresses.

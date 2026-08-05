# tonepoet — Batch brief for reasoning model (2026-08-04)

You are starting **fresh** with no prior context. Everything you need is in this bundle.
This brief describes **outcomes we want and guardrails you must respect**. Where we include
diagnosis, it is *evidence for your use*, not a prescription — you choose HOW to implement,
so long as the outcomes are met and nothing in the guardrails is violated.

**Project:** tonepoet, a Rust CLI + TUI audio conversion toolkit (ratatui 0.26 / crossterm 0.27,
tokio, edition 2021). Branch `hardening` @ `da6a83d`, version 0.4.6. The full workspace test
gate is currently green (5508 passed / 0 failed). **Do not bump the version.**

**Global constraints (apply to every item):**
- Every change must keep `cargo test --workspace --no-fail-fast` fully green. Add regression
  tests for each fix — each item lists its minimum test expectations.
- Match surrounding code style (thiserror in library code, `log` macros, module re-exports
  via `mod.rs`, immutable-state draw functions with second-pass mouse registration).
- **Input rules (byobu/tmux user):** never bind F-keys. Never make Shift+Click, Shift+arrows,
  or Ctrl+Space the *only* path to a function. Keep existing binding conventions.
- Deliver complete replacement files (or unambiguous per-file patches) for every file you
  change, plus a summary of what changed and why, and the tests you added.

---

## Item 1 — URGENT: single-image sidecar CUE albums open as flat single-file metadata (+ false "Discard changes?" prompt)

### Outcome
A folder containing **one audio file + a sidecar `.cue` (+ optionally an embedded cue)** whose
cue proves multi-track image content (1 file / N≥2 tracks) must open the metadata editor
(Properties) as a **per-track cue album**: editable per-track rows, cue album title, populated
`cue_album_synthetic_sheet` — exactly like multi-image cue albums already do. It must NOT open
as a flat single-file tab showing the audio file's filename-derived tags.

Additionally: opening Properties on such a folder and immediately pressing Escape with zero
edits must **not** prompt "Discard unsaved metadata changes?".

Genuine 1-track single-file cues (cue proves nothing beyond a single track) must **continue**
to open flat, as today.

### Governing spec — read first
`docs/metadata_source_selection_heuristic.md` (included in bundle) is the LODESTAR for this
area. Do not re-derive intended behavior. Core rules:
- Configuration is a **preference among viable candidates**, never a way to select a nonviable one.
- Individual-files is **nonviable** when a valid CUE proves image content by per-carrier
  mapping (≥1 carrier holds >1 track).
- A selected CUE is **structurally authoritative**: its logical track structure must survive
  into presentation; the physical container's flat tags must not override it.

This area has regressed 6–7 times historically. Treat it with maximum care.

### Evidence (verified empirically; you may re-verify but need not re-derive)
Repro: `Blondie - Plastic Letters/` — one `.flac` + sidecar cue (13 tracks, INDEX 01 + titles)
+ embedded `cuesheet=` VORBIS comment. Also `Eddy Grant - Going For Broke/`. Reproduces on
every folder of this shape.

Upstream is **correct** end-to-end:
- `parse_cue_file` → 13 tracks, album "Plastic Letters".
- `admit_split_cue_member` → Ok (`role=SyntheticAlbumPart`, 13 tracks, 1 image, exact_refs=true).
- `metadata_cue_surface_proves_image_content` (src/tui/keybindings.rs:24341) → TRUE for
  1-file/13-track, so individual-files is nonviable.
- Resolution (`resolve_edit_metadata_directory_groups` at keybindings.rs:19491 →
  `resolve_directory_metadata_groups` at 16929 → `resolve_aggregate_metadata_target`) correctly
  selects **SidecarCue** under the default priority `[SidecarCue, EmbeddedCue, IndividualFiles]`.

The defect is in the **presentation builder**
`build_metadata_editor_for_cue_surfaces_with_policy_and_member_file_order`
(src/tui/keybindings.rs:18835). The single-carrier guard at 18881:

```rust
if sorted.len() == 1 && sorted[0].audio_paths.len() == 1 {
    ... PresentationTab::for_files(...)   // flat tab; cue_album_synthetic_sheet stays None
```

keys only on `audio_paths.len() == 1` and does not exclude a single-image sidecar whose sheet
has ≥2 tracks. For Blondie (1 image, 13 tracks) it fires → flat single-file tab
(`PresentationTab::for_files`, app.rs:7608); the 13-track structure survives only as a
collapsed CUESHEET blob (TITLE = "<multiple values>"). The correct path is the unified
synthetic-sheet builder (`build_unified_cue_album_sheet_with_combined_limit`,
keybindings.rs:18688, called at ~18919) — the only path that populates
`cue_album_synthetic_sheet`. The `audio_paths.len() > 1` native-multi-file guard at 18867
reaches it; a single-image sidecar never does.

Empirically validated (2026-08-04): driving a single-image / 3-track sidecar fixture through the
real production path reproduces `cue_album_synthetic_sheet = None`; adding the exclusion
`&& !metadata_cue_surface_proves_image_content(&sorted[0])` to the ~18881 guard yields
`cue_album_synthetic_sheet = Some(...)` with a per-track album. Both drop point and fix locus
are causally proven. (Note: `proves_image_content` is false for a genuine 1-track single file,
so those correctly remain flat under this exclusion.)

**False-dirty is the same root cause.** On the wrong flat path, file-level TITLE auto-populate
(`ensure_and_auto_populate_track_title_entries`, keybindings.rs:23924; dirtiness via
`any_presentation_dirty`, app.rs:8740) fills TITLE from the filename and marks the surface
dirty with zero user edits. In the Blondie folder the embedded cuesheet happened to mask it
(originals pre-filled → not dirty); without that masking it prompts falsely. Routing these
folders to the synthetic-sheet path removes the flat-path auto-populate entirely.

### Guardrails
- Follow the lodestar. Structural authority of the selected CUE is the invariant being restored.
- Do not disturb: sidecar-vs-embedded policy selection, member-file save ordering, native
  multi-file cue albums, multi-image albums, or genuine 1-track single-file cues.
- Existing synthetic-sheet test coverage is multi-image only (`build_dsotm_unified_editor` and
  friends) — which is exactly why this single-image regression shipped green. Your tests must
  close that hole.

### Acceptance
- Single-image / N≥2-track sidecar folder opens as per-track cue album
  (`cue_album_synthetic_sheet` populated, per-track titles editable, album title from the cue).
- Immediate Escape with no edits → no discard prompt (with AND without an embedded cuesheet
  present, i.e. the unmasked variant too).
- Genuine 1-track single-file cue still opens flat.
- New regression tests: at minimum (a) single-image multi-track sidecar → synthetic sheet path;
  (b) 1-track single-file cue → flat path; (c) no false-dirty on open for (a).
- Full workspace gate green.

---

## Item 4 — Pre-emphasis catalog matching produces false positives

### Outcome
The advisory pre-emphasis detector (`src/tui/preemphasis/`, entry
`detect_preemphasis_metadata_catalog` in mod.rs) must stop reporting CDs as pre-emphasized via
**catalog-number matches they do not actually have**. Matching must be rebuilt around the
authoritative reference list (provided in the bundle as CSV, extracted from
`docs/cds-with-preemphasis-shf.xlsx`), and reported confidence must honestly reflect match
quality (an exact catalog match is not the same confidence as a series/prefix match — today
both flatten to `StrongCandidate`).

### Evidence / verified scope
- Current implementation: `catalog.rs` — a normalization regex + `KNOWN_PE_EXACT` map +
  anchored `KNOWN_PE_SERIES` patterns. The series patterns are the suspected false-positive
  source (they match catalog ranges the authoritative list does not support).
- Verified flow (mod.rs:92 `detect_preemphasis_metadata_catalog`): explicit PRE tag / CUE
  FLAGS evidence → `Detected`; otherwise ANY `catalog::check_catalog_evidence` hit →
  `StrongCandidate` regardless of exact-vs-series — that is the confidence flattening to fix.
- This detector is **display/advisory only**. It does NOT affect conversion:
  `source_text_tags_indicate_pre_emphasis` uses only real tags, and no de-emphasis filter
  exists. Keep it that way — this item is about honest advisories, not DSP.
- Detection is deliberately **catalog/tag-only** (documented on the function). Do not add
  signal analysis, and do not route metadata-editor results through the spectral scorer.
- Authoritative list CSV: extracted from the Steve Hoffman forum spreadsheet; ~1,020 rows,
  header row 4, columns include ARTIST, RELEASE TITLE, LABEL, **CATALOG ID#**, CTRY MFR/MKT,
  MFR, MATRIX, PE FLAG, NOTES, Discogs URL. Some leading banner rows and blank rows/columns —
  parse defensively. How you embed it (build-time codegen, include_str! + parser, or a curated
  static table derived from it) is your choice; state your approach and keep it auditable
  against the CSV.

### Guardrails
- Only Red Book CDs (16-bit/44.1 kHz) can be pre-emphasized — never advise pre-emphasis for
  sources above 16/44.1 (see also Item 24).
- Do not over-state confidence. Distinguish exact vs series/heuristic matches in the reported
  result; naming/enum shape is yours to choose, but a downstream consumer must be able to tell
  them apart.
- False negatives are acceptable; false positives are the defect. When in doubt, don't match.

### Acceptance
- Catalog numbers NOT on (or provably implied by) the authoritative list no longer match.
- Everything on the authoritative list still matches.
- Match-quality distinction surfaced in the result type.
- Tests: false-positive regressions (catalog numbers near, but not in, known series), plus
  positive coverage from the authoritative list.

---

## Item 5 — Surface PRE_EMPHASIS (and CUE_FLAGS) in the Canonical tag-grid view

### Outcome
`PRE_EMPHASIS` must appear in the editable **Canonical** view of the metadata editor tag grid
(the "View: Canonical | All" toggle), alongside TITLE/ARTIST/etc. Add `CUE_FLAGS` as well
unless you find a concrete reason it doesn't belong.

### Mechanism (verified)
`STANDARD_KEY_ORDER` (src/tui/probe.rs:7220) plays two roles: sort/promotion order for
entries, AND the Canonical-view **visibility filter** — `metadata_entry_is_visible`
(src/tui/app.rs:7829) shows an entry in Canonical view iff its canonicalized key is a member
of `STANDARD_KEY_ORDER`. That membership test is why `PRE_EMPHASIS` today appears only under
"All". Adding the keys to `STANDARD_KEY_ORDER` therefore fixes both visibility and ordering.

### Guardrails
- Do NOT add these to `CORE_EDITOR_FIELDS` (probe.rs, which forces always-present empty rows)
  unless you can argue they belong there — we believe they should appear only when present on
  the file.
- Place them sensibly in the order (near CUESHEET / the technical tail, not amid the core
  artist/title block).

### Acceptance
- A file tagged `PRE_EMPHASIS` shows it in Canonical view; untagged files show nothing new.
- Test covering canonical promotion of the new keys.

---

## Item 6 — Title-casing lower-cases "The" in "Kool & The Gang"

### Outcome
Converting `Kool & The Gang, Emergency, 1984/Flac` must produce a finalized folder name that
keeps **"The" capitalized** in the band name "Kool & The Gang". Ordinary small-word behavior in
titles ("Dark Side of the Moon" → lowercase "of the") must not regress.

### Evidence — and an open diagnosis you must complete
- The title-case core is `capitalize_title` / `capitalize_section`
  (src/convert/renaming.rs:436+). It is wired as the `fixcaps` renderer in the naming-template
  publish path (stages.rs:22372, actions.rs:17018, conversion_actions_ui.rs:2373) and in
  source-metadata normalization (src/convert/pipeline/source_heuristics.rs ~262–272).
- Small-word downcasing: `lowercase_word_core` (renaming.rs:552) + `NON_CAPITALIZED_WORDS`
  (renaming.rs:13 — includes "the").
- **Important clue:** `capitalize_section` already contains an `after_ampersand` carve-out
  (renaming.rs ~505) that capitalizes the token following a token containing `&` — and a naive
  reading says "Kool & The Gang" should therefore already come out right. Yet the user observes
  the lowercased result on a real conversion. So you must diagnose the actual failing path
  before fixing: candidates include (a) the real input string differing from the assumed one
  (tokenization, punctuation attached to `&`, comma-separated folder template parts processed
  per-segment so "The" is no longer after "&"), (b) a different normalization path that
  downcases without the carve-out (e.g. the all-caps `normalize_all_caps` branch or
  source_heuristics), (c) the template splitting artist/album before casing.
- Diagnose with the real folder-name shape from the repro
  (`Kool & The Gang, Emergency, 1984/Flac`, tags from that conversion), then fix at the true
  locus.

### Guardrails
- Case-only transform invariant (documented in `capitalize_title`): never insert, remove, or
  reorder tokens/punctuation.
- Do not regress ordinary small-word titles; keep `SPECIAL_CASES` behavior.
- Add the failing real-world string(s) as regression tests, plus adversarial neighbors
  ("Jack and the Beanstalk" should keep lowercase "the"; "Between the Buried and Me"-style
  names are out of scope unless trivially covered).

### Acceptance
- The repro folder converts to a name with "Kool & The Gang" correctly cased.
- Explanation of which path actually produced the bad casing (so we can record it).
- Regression tests for the fixed path; existing renaming tests stay green.

---

## Item 7 — Dual-clipboard on cut/copy (host + internal), paste distinguishes

### Outcome
Every user-facing **cut or copy** in the TUI must populate **both** clipboards — the host
system clipboard AND tonepoet's internal clipboard(s). Surfaces in scope:
- inline editing of file/folder names (Browse rename),
- editing metadata field values in the metadata editor,
- the **Copy tags** action (both the context menu and the metadata overlay).

Only **paste** distinguishes source:
- **SHIFT+CTRL+V** → paste from the **host** clipboard.
- **CTRL+V / CTRL+P** → paste from **tonepoet's internal** clipboard (existing behavior).

### Context
- Internal clipboards today (src/tui/app.rs ~2271–2475): `filesystem_clipboard`
  (`tui_file_picker::FilesystemClipboard`, defined in
  `crates/tui-file-picker/src/filesystem_clipboard.rs`, Cut/Copy/Paste of files) and
  `tag_clipboard` (`TagClipboard`, app.rs:2276 — metadata entries, positionally complete,
  with copy-generation/cancel plumbing for async copies).
- Copy tags surfaces: context menu submenu (`src/tui/context_menu.rs:638`,
  `ContextAction::CopyTags(TagCopySelection)`) and the metadata-overlay copy path in
  keybindings.rs; inline rename + metadata field editing live in keybindings.rs /
  tui-file-picker text input.
- There is currently **no host-clipboard integration at all** (no arboard/copypasta/OSC52 in
  the dependency tree). You choose the mechanism. Constraints on that choice:
  - Environment: Linux terminal app, frequently inside **byobu/tmux** — plain OSC 52
    pass-through cannot be assumed, and OSC 52 is effectively write-only in most terminals.
    A crate like `arboard` (X11/Wayland) is acceptable; a write path via OSC 52 as fallback is
    acceptable; document what you pick and its failure mode.
  - Host-clipboard writes must be **non-blocking / failure-tolerant**: a missing display
    server or denied clipboard must never break or delay the internal copy. Internal clipboard
    remains the source of truth; host mirroring is best-effort.
- Text vs structured content: filesystem cut/copy and Copy tags carry structured payloads.
  For the host mirror, write a sensible **text projection** (e.g. newline-separated paths;
  tag `KEY=value` lines or the field value being edited). Internal semantics unchanged.
- SHIFT+CTRL+V (host paste) needs defined behavior per surface: in text-editing contexts
  (inline rename, metadata field edit) paste the host text at the cursor. For filesystem paste
  from host, do the simplest sane thing or omit it — state your choice. Do not break existing
  CTRL+V/CTRL+P semantics.

### Guardrails
- Byobu-safe input rules (top of brief). Keep existing binding conventions; SHIFT+CTRL+V is
  the sanctioned new chord.
- No blocking calls on the render/event path for host clipboard I/O.
- New dependency (if any) must build inside the nix sandbox (pure Rust preferred).

### Acceptance
- Cut/copy on each in-scope surface lands content on both clipboards (host best-effort).
- SHIFT+CTRL+V pastes host text in text-editing contexts; CTRL+V/CTRL+P unchanged.
- Failure tolerance: with no host clipboard available, all copy/cut/paste-internal flows work
  exactly as today.
- Tests for the text projections and dispatch plumbing (host clipboard itself may be faked
  behind a trait — headless CI cannot own a real clipboard).

---

## Item 24 — Details pane: hide pre-emphasis row for non-Red-Book sources

### Outcome
The pre-emphasis row in the metadata editor's **Details/analysis pane** must be **hidden**
for sources above 16-bit/44.1 kHz — only Red Book CDs can carry pre-emphasis, so showing the
row (currently with an "N/A"-style empty state) for hi-res material is noise. While there,
reconsider the "N/A" empty-state wording for sources where the row IS shown but no
determination exists — pick wording that doesn't imply a scan happened when it didn't.

### Context (verified)
The row was added in the `da6a83d` round. The display logic lives in
`src/tui/metadata_view_models.rs`:
- Row emission: `build_details_view_model` → line ~229 pushes the "Pre-emphasis" field when
  `metadata_preemphasis_is_applicable_to_active_surface` (line 455) is true.
- An applicability gate **already exists**: `preemphasis_applicable_for_file` (line 464)
  excludes disc structures, DSF/DFF, lossy formats, and DSD codecs — but does **not** check
  bit depth / sample rate. That gate is the natural extension locus: a probed source above
  16-bit/44.1 kHz should be inapplicable. Note `media_facts` (`ProbeState::Ready(facts)`)
  carries the probed format facts you need; handle the not-yet-probed state conservatively
  (don't hide the row solely because the probe hasn't completed, unless you argue otherwise).
- Empty-state wordings: `metadata_preemphasis_status` (line 607) returns "N/A" for disc
  surfaces; also note `metadata_preemphasis_is_applicable_to_active_surface` returns `true`
  unconditionally for disc surfaces (line 456) which then renders "N/A" — consider whether
  that pairing still makes sense under your change.
This is a display-layer change only.

### Acceptance
- Row absent for probed sources >16-bit or >44.1 kHz; present for 16/44.1.
- Improved empty-state wording (state your choice).
- Tests in `metadata_view_models` covering the new gate (hi-res hidden, Red Book shown).

---

## Bundle manifest (what you've been given)

- This brief (`docs/BRIEF_batch_2026-08-04.md`).
- `docs/metadata_source_selection_heuristic.md` — LODESTAR for Item 1.
- `docs/cds-with-preemphasis-shf.csv` — authoritative pre-emphasis list for Item 4 (extracted
  from the original xlsx; header row 4, banner rows above it).
- The **complete `src/` tree** of the main crate (all of `src/tui/`, `src/convert/`, CLI,
  config) — every referenced area lives here: keybindings.rs, app.rs, probe.rs,
  metadata_view_models.rs, draw_overlays.rs, browse.rs, context_menu.rs, event_loop.rs,
  message.rs, command.rs, conversion_actions_ui.rs, preemphasis/*, convert/renaming.rs,
  convert/pipeline/{stages,actions,source_heuristics}.rs, etc.
- The complete `crates/tui-file-picker/` crate (filesystem clipboard, text input — Item 7).
- The complete `crates/tonepoet-backend/` and `crates/tonepoet-features/` crates (metadata
  I/O, CUE generation — supporting context for Items 1/5).
- Root `Cargo.toml` (workspace + main crate deps) and `CLAUDE.md` (project overview, build
  and test commands).

NOT included (not germane, large): `crates/{sacd-rs,dvda-demuxer,dvdvideo,tonepoet-wizard}`,
`target/`, other docs. If anything you need is missing from the bundle, say so explicitly in
your reply rather than guessing at its contents.

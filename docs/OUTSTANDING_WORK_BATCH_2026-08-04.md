# Outstanding work batch — 2026-08-04

**RESOLVED 2026-08-05 @ `98b2edb`** (reasoning-model batch delivery, brief
`docs/BRIEF_batch_2026-08-04.md`, + direct single-image integration fix): items 1, 4, 5, 6, 7,
and #24 are **DONE**; #2 remains **PARKED**; #3 was already done at `da6a83d`. Gate green ×2
(5524/0). Field-verified by the user across all items. Version stays **0.4.6**.

Original worklist below, statuses updated.

Legend: **DONE** / **PARKED** / **TODO** / **IN-DIAGNOSIS**.

---

## 1. Metadata source-selection regression + false "Discard changes?" — DONE (98b2edb)
**Symptom.** A folder with **one audio file + a sidecar `.cue` + an embedded cue** opens Properties
using the **flat/filename tags of the audio file** and **ignores the authoritative sidecar cue**.
E.g. `~/torrents/Blondie - Plastic Letters/` shows album TITLE = "Blondie - Plastic Letters" (the
FILE/folder name) with none of the sidecar's 13 per-track titles. Also reproduces on
`~/torrents/Eddy Grant - Going For Broke/`. Happens on *every* folder of this shape (single audio +
sidecar + embedded cue).

**Co-symptom.** Right-click folder → Properties → press **Escape immediately** (no edits) → prompts
**"Discard unsaved metadata changes?"** with zero changes made. This is the filename-TITLE
auto-populate marking the surface dirty on the individual-file path.

**Governing spec (LODESTAR).** `docs/metadata_source_selection_heuristic.md`. This area has regressed
6–7×; the memory `lodestar_metadata_source_selection` names this exact case (single-file album image,
1 file / N tracks, "e.g. Blondie - Plastic Letters") and the false-dirty co-symptom. DO NOT re-derive
intended behavior — configuration = PREFERENCE among *viable* candidates; individual-files is nonviable
when a valid CUE proves image content by **per-carrier** mapping (≥1 carrier holds >1 track).

**Verified so far (do not re-check):**
- `parse_cue_file` on the Blondie sidecar → 13 tracks, all with INDEX 01 + titles, album "Plastic
  Letters" (handles the inconsistent `TRACK 10..13` un-indentation fine).
- `admit_split_cue_member` → Ok, `role=SyntheticAlbumPart, 13 tracks, 1 image, exact_refs=true`.
- `metadata_cue_surface_proves_image_content` (src/tui/keybindings.rs ~24341) uses per-carrier mapping
  and returns TRUE for 1-file/13-track (so individual-files SHOULD be nonviable and the sidecar SHOULD
  win under any config priority).
- So parse + admission + viability are all **correct**. The bug is **downstream** in the editor
  open/resolution path dropping the proven single-image sidecar and falling to the individual audio
  file (which then filename-auto-populates TITLE → false-dirty).

**ROOT CAUSE (pinned + instrumented + code-verified).** Selection is **correct** end-to-end — the
resolver picks SidecarCue (`resolve_edit_metadata_directory_groups` 19491 → `resolve_directory_metadata_groups`
16929 → `resolve_aggregate_metadata_target`; `individual_files = all surfaces !proves_image = false` →
nonviable; default priority `[SidecarCue, EmbeddedCue, IndividualFiles]` → SidecarCue). The bug is purely
in the **presentation builder**: `build_metadata_editor_for_cue_surfaces_with_policy_and_member_file_order`,
the single-carrier guard at **keybindings.rs:18881**:
```rust
if sorted.len() == 1 && sorted[0].audio_paths.len() == 1 {   // fires for a 1-image / 13-track sidecar!
    ... PresentationTab::for_files(...)   // flat single-file tab, cue_album_synthetic_sheet = None
```
This guard keys only on `audio_paths.len() == 1` and does **not** exclude a single-image sidecar whose
sheet has ≥2 tracks. For Blondie (1 image, 13 tracks) it fires → `PresentationTab::for_files`
(app.rs:7608, leaves `cue_album_synthetic_sheet = None`), so the 13-track structure survives only as a
collapsed CUESHEET blob (TITLE shows "<multiple values>") instead of editable per-track rows and the cue
album title. The CORRECT path is the unified synthetic-sheet builder (`build_unified_cue_album_sheet_with_combined_limit`,
called at keybindings.rs:18919) — the only path that populates `cue_album_synthetic_sheet`. The
`audio_paths.len() > 1` native-multi-file guard at 18867 reaches it; a **single**-image sidecar never
does. (NB: 18881 is the single-image builder branch touched during the album-group round — likely the
regression origin.)

**FIX LOCUS.** The 18881 guard must not swallow a proven single-image album surface: exclude when
`metadata_cue_surface_proves_image_content(&sorted[0])` (equivalently `sorted[0].sheet.tracks.len() >= 2`)
and route it to the unified `cue_album_synthetic_sheet` path (18919+), same as multi-image. Lodestar
rule violated: structural authority — a selected CUE is authoritative for logical track structure; the
builder discarded it and rendered the physical container's flat tags.

**FALSE-DIRTY = same root cause.** Because the album opens as an individual-file surface, it is subject
to file-level TITLE auto-populate (`ensure_and_auto_populate_track_title_entries`, keybindings.rs ~3789;
`any_presentation_dirty`, app.rs:8740 → discard prompt). In THIS folder it happened NOT to reproduce
because the .flac carries an embedded `cuesheet=` VORBIS comment that pre-fills TITLE originals
(per_file_values == per_file_originals → not dirty); on the same wrong path without that masking
(no/partial embedded cuesheet) the auto-populate fills TITLE and marks dirty → false "Discard changes?".
Fixing the drop point (route single-image sidecars to the synthetic-sheet builder) removes the
individual-file auto-populate entirely and eliminates the false-dirty. (The embedded cuesheet references
a stale `CDImage.wav`; it did NOT cause the drop — admission still produced the sidecar surface — it only
masked the false-dirty.)

**VALIDATED (empirical, 2026-08-04).** Drove a single-image / 3-track sidecar fixture through the real
production builder (`collect_metadata_cue_surfaces` → `apply_cached_or_ladder_split_cue_grouping...` →
`build_metadata_editor_for_cue_surfaces`): CURRENT → `cue_album_synthetic_sheet = None`, tabs collapse,
TITLE = "<multiple values>" (bug reproduced). Applying the one-line exclusion at 18881
(`&& !metadata_cue_surface_proves_image_content(&sorted[0])`) → `cue_album_synthetic_sheet = Some`,
single_surface per-track album (fix locus confirmed). Both the drop point and the fix locus are causally
proven. (Existing synthetic-sheet coverage is multi-image only — `build_dsotm_unified_editor` — which is
why the single-image regression shipped green.)

**Fix posture.** Lodestar-governed; the fix locus is a single precise guard, but source-selection has
regressed 6–7× — before shipping, run the FULL gate ×2 (does routing single-image through the unified
path disturb sidecar-vs-embedded policy / member-file ordering / genuine 1-track single-file cues? —
note `proves_image_content` is false for a 1-track single file, so those correctly stay flat) and add a
single-image regression test. Strong candidate for a self-contained reasoning-model brief (include the
lodestar), or a careful direct fix + full gate.

---

## 2. DSD→PCM explicit dither at 32-bit is silently dropped — PARKED
User ruling: **"if the user wants dither, the user gets dither"** (reject the "−186 dBFS doesn't matter"
argument). **PARKED until the DSD→PCM pipeline + planner refactor.** Verified physics: `sox`'s `dither`
effect is a **structural no-op at 32-bit integer output** (int32 sample pipeline; byte-identical even
from a 54-bit Float64 W64 intermediate; dithers fine at 16/24-bit). Fix direction (for the refactor):
the **planner routes an explicit-dither + 32-bit-int request through ffmpeg** (which must be verified to
emit *genuine* stochastic dither at int32, not the deterministic token seen in the ad-hoc test). Full
detail: `docs/RESEARCH_dsd_dither_dsp_honesty.md`, memory `dsd_pipeline_followups`.

---

## 3. "Metadata: Skipped" log label — DONE (da6a83d; rebuilt + verified)
Fixed on `da6a83d`. `StageOutcome` gained `NotRequested` + `SkippedWithReason(String)`; the
planner-transferred case now renders **`Metadata: Skipped (already satisfied by the output planner)`**
(stages.rs `stage_outcome_label` + the orchestrators). The user's confusing log was from a pre-`da6a83d`
build — **rebuild to see the new wording**. Optional follow-up: if "Skipped (already satisfied…)" still
reads oddly, change to e.g. "Applied by planner".

---

## 4. Pre-emphasis false catalog matches — DONE (98b2edb: authoritative-list rebuild, cache v25)
Falsely reports some CDs as pre-emphasized via **catalog-number matching**, even though the falsely
matched CDs don't share catalog numbers with the authoritative list. Early, non-robust implementation
(`src/tui/preemphasis/` — catalog.rs regex + `KNOWN_PE_EXACT` + anchored `KNOWN_PE_SERIES`; mod.rs
`detect_preemphasis_metadata_catalog`). Verified earlier: it's **display/advisory only** (does NOT
affect conversion — `source_text_tags_indicate_pre_emphasis` uses only real tags; no de-emphasis filter
exists), it's **catalog/tag-only** (no signal — signal analysis is deliberately undocumented/unreliable),
and it **flattens** exact vs series matches to `StrongCandidate`. **Authoritative reference list:**
`docs/cds-with-preemphasis-shf.xlsx`. → Reasoning-model brief: rebuild catalog matching around the
authoritative list, eliminate false positives, and don't over-state confidence.

---

## 5. Surface PRE_EMPHASIS in the CANONICAL tag grid — DONE (98b2edb)
**Correct scope:** promote `PRE_EMPHASIS` into the editable **Canonical** tag-grid view (the
"View: Canonical | All" toggle in the metadata editor), alongside TITLE/ARTIST/etc. Today it appears
only under "All" because it's not in `STANDARD_KEY_ORDER` (src/tui/probe.rs ~7220, a sort/promotion
list, not a filter). Small fix: add `PRE_EMPHASIS` (and likely `CUE_FLAGS`?) to the canonical set.
NOTE: my `da6a83d` work put pre-emphasis in the Details **analysis** pane — that was the **wrong
surface** for this ask; it stays (harmless) but does not satisfy #5. Related: task #24 (hide the Details
pre-emphasis row for sources >16/44.1; reconsider its "N/A" wording).

---

## 6. Capitalization lower-cases "The" in "Kool & The Gang" — DONE (98b2edb: root cause was canonical_artists_reference.txt row, not capitalize_title)
Converting `~/torrents/Kool & The Gang, Emergency, 1984/Flac` produces a finalized folder name that
**lower-cases "The"** in "Kool & The Gang". VERIFIED: the title-case core `capitalize_title`
(src/convert/renaming.rs:436) is active and wired as the `fixcaps` function in the naming-template
publish path (`stages.rs:22372`) and in source-metadata normalization
(`source_heuristics.rs:262-272`); the small-word downcasing lives in `lowercase_word_core`
(renaming.rs:552) + its small-word list — that is where "the" is lowered mid-title. Fix: keep "The"
capitalized when it's part of a proper/band name (e.g. after "&", or make the small-word rule
context-aware). Diagnosis needed: confirm which path renders THIS folder name (naming template vs the
source-heuristics normalization), then adjust the small-word rule without regressing ordinary titles.

---

## 7. Dual-clipboard on cut/copy — DONE (98b2edb: host mirror + SHIFT+CTRL+V host paste)
tonepoet keeps its own clipboard. Desired: whenever the user **cuts or copies** — inline editing of
file/folder names, editing metadata fields, and the **Copy tags** action (context menu *and*
metadata overlay) — the content goes to **both** clipboards (host system **and** tonepoet's internal).
Only **paste** distinguishes: **SHIFT+CTRL+V = host** clipboard; **CTRL+V / CTRL+P = tonepoet**
clipboard. Today copy/cut only populates one, which is biting the user. (Byobu-safe input rules apply —
no F-keys; keep the existing binding conventions.)

---

## Also queued / context
- **task #24 — DONE (98b2edb)**: hide the Details pre-emphasis row for sources >16-bit/44.1 kHz (only Red Book CDs can be
  pre-emphasized); reconsider the "N/A" empty-state wording. (Follow-up from the shipped #5 mis-scope.)
- **Process discipline (memory):** never bump the version or fast-forward `main` without explicit
  per-turn instruction; commit to `hardening` only by default.
- The `~10-item bill (O1, 1–9)` referenced in older task #22 — this doc is the current live batch;
  reconcile with the user if there are additional items beyond #1–#7.

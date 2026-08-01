# Round-12b — corrective brief (v5 → v6)

## Status

- **Baseline:** branch `hardening` == `main` @ `a6b8236`, version **0.4.5**.
- Your round-12 **v5** delivery was applied on top of the baseline. After the applying side
  made *compile-directed caller updates to out-of-bundle files*, it **compiles clean**
  (`cargo check --workspace --all-targets` = 0 errors).
- Full gate `nix develop --command cargo test --workspace`:
  **4304 passed / 10 failed / 10 ignored** — every other target green. **All 10 failures are
  in your v5 in-bundle changes.** Exact root causes + minimal fixes below (each verified against
  the applied source; two were reproduced with instrumentation).

Your job: fix exactly these 10, nothing more. No compiler on your side — the applying side
compile-fixes trivial slips and re-gates.

---

## What the applying side already did — do NOT touch, do NOT include in your delivery

- **`src/tui/context_menu.rs` and `src/tui/event_loop.rs`** — updated every
  `classify_tag_transfer_roots` call site to the new 3-arg `(roots, &metadata_target_priority,
  cancel)` signature; built `FilePickerPurpose::BrowseTagTransfer { … metadata_target_priority }`;
  destructured `metadata_target_priority` in the `MetadataTagTransfer` completion; threaded a
  live-config priority through `launch_tag_transfer` / `launch_prepared_tag_transfer` /
  `reverify_prepared_files_target`. **These are correct and OUT of your scope. Your v6 must NOT
  modify context_menu.rs or event_loop.rs.**
- The applying side also added two imports to your new test module in `keybindings.rs`
  (`mod metadata_cue_source_coverage_tests`), which your v5 omitted so it did not compile:
  ```rust
  use super::single_image_metadata_editor_regression_tests::{
      create_flac_fixture, fixture_tool_available, select_foxy_route,   // ← select_foxy_route added
  };
  use super::*;
  use crate::config::TonepoetConfig;                                    // ← added
  ```
  **When you re-deliver `keybindings.rs`, keep these two imports** (your test module uses both
  `TonepoetConfig` and `select_foxy_route`).

The new 3-arg `classify_tag_transfer_roots` signature, `AggregateMetadataTarget` config, the
`metadata_target_priority` field on both `FilePickerPurpose` variants, and the construct at
`keybindings.rs:11106` are all correct — **keep them**.

---

## ⛔ Scope discipline (unchanged from round 12)

Single-user desktop audio TUI. No new subsystems/protocols/journals/transaction managers.
Smallest correct change in the surrounding style. These are surgical fixes to logic **you already
wrote** — do not rebuild anything, do not "improve" adjacent code.

---

## Deliverable

Complete updated versions of **only** the in-bundle files a fix requires — expected:
`src/convert/pipeline/materializer_cue.rs`, `src/tui/keybindings.rs`, `src/tui/tag_interchange.rs`
(plus `src/tui/app.rs` **only** if FIX 3 needs the length-1 presentation-tab constructor). Version
stays **0.4.5**. Short report. Ensure **your own** new tests in `metadata_cue_source_coverage_tests`,
plus the `single_image_metadata_editor_regression_tests`, `materializer_cue_tests`, and
`tag_interchange::tests` cases below, all pass — and do not regress the 4304 now-passing tests.

---

## The 10 failures

### FIX 1 — item-1 per-track MusicBrainz id leaks into album metadata  *(1 test)*

**Test:** `materializer_cue_tests::lofty_reads_real_flac_tags_and_embedded_cuesheet_fixture`
— assert at `src/convert/pipeline/materializer_cue.rs:5859`:
`assert!(!image_metadata.extra.contains_key("musicbrainz_trackid"), …)`.

**Cause:** lofty 0.21 maps the Vorbis key `MUSICBRAINZ_TRACKID` → `ItemKey::MusicBrainzRecordingId`
(Debug `"MusicBrainzRecordingId"` → normalized `musicbrainzrecordingid`). Your guard
`cue_image_tag_is_structural_or_track_scoped` (materializer_cue.rs:2939) lists `"musicbrainztrackid"`
(which actually catches `MUSICBRAINZ_RELEASETRACKID` → `ItemKey::MusicBrainzTrackId`) and a dead
`"musicbrainzreleasetrackid"`, but **not** `"musicbrainzrecordingid"`. So the recording id passes the
guard, and the new source-text passthrough (materializer_cue.rs:2843-2846) inserts a **bare**
`musicbrainz_trackid` into album `extra` via `item_key_to_extra_key` + `insert_source_text_tag`.

**Fix (one literal):** add `"musicbrainzrecordingid"` to the match arm at ~2949-2950:

```rust
            | "totaltracks"
            | "musicbrainztrackid"
            | "musicbrainzrecordingid"      // ← add: lofty maps MUSICBRAINZ_TRACKID to MusicBrainzRecordingId
            | "musicbrainzreleasetrackid"
```

---

### FIX 2 — embedded-CUE fail-closed contract mismatch  *(1 test)*

**Test:** `tag_interchange::tests::embedded_cue_writes_fail_closed_for_read_only_targets`
(tag_interchange.rs:1531) calls `execute_tag_transfer_to_cue(…).expect_err("read-only embedded
writes must fail closed")` and asserts the error equals
`"embedded CUE write is not supported for this audio carrier"`.

**Cause:** your impl returns `Ok(TagTransferReport { written: 0, failed: [("image.ape", "embedded
CUE write is not supported for this audio carrier")] })` for a **wholly-unsupported** target carrier,
so `expect_err` panics.

**Resolution — pick ONE and make both halves consistent:**
- **(a) recommended:** treat a wholly-unsupported target carrier as a **pre-flight rejection** —
  have `execute_tag_transfer_to_cue` return `Err("embedded CUE write is not supported for this audio
  carrier")` *before* any per-track work when the embedded carrier is not writable, reserving the
  `Ok(report.failed)` shape for genuine per-track partial failures. Keep the test as-is.
- **(b)** keep the report-based contract and update the test to assert on the report
  (`report.written == 0` and `report.failed` contains the message) instead of `expect_err`.

Choose whichever matches your intended production call path (does the real caller distinguish a
top-level `Err` from an `Ok` with all-failed entries?). State your choice in the report.

---

### FIX 3 — single-image CUE surface never routed to the CUE-surface editor  *(coverage tests 2, 4, 5, 6 + pre-existing `single_image:55065`)*

**Tests:**
- `metadata_cue_source_coverage_tests::metadata_editor_directory_entry_uses_configured_priority_and_explicit_cue_bypasses_it` (56647: `explicit CUE selection must open metadata editor`)
- `…::explicit_cue_or_image_in_multi_surface_folder_opens_only_selected_surface` (56832: `presentation_tabs` `left 0, right 1`)
- `…::metadata_editor_explicit_multi_file_selection_bypasses_neighboring_cue` (56753: same `0 vs 1`)
- `…::read_only_embedded_cue_falls_through_to_sidecar_for_transfer_and_editor` (57375: editor `cue_source` not `Sidecar`)
- `single_image_metadata_editor_regression_tests::native_multi_file_cue_opens_as_one_album_from_folder_cue_or_member_image` (55065: `left None, right Some(album.cue)`)

**Cause:** In `open_metadata_editor_impl`, a single-disc **single-image** CUE (≥2 tracks, all pointing
at one audio file, role `SyntheticAlbumPart`) is admitted as one surface with `audio_paths.len()==1`
and empty `admitted_ordinary_paths`. The CUE-surface dispatch guards all miss it:
- `keybindings.rs:22112` requires `!admitted_ordinary_paths.is_empty()` (empty here);
- `:22124` `one_native_multi_file_surface` requires `audio_paths.len() > 1` (false);
- `:22127` requires `cue_surfaces.len() > 1` (it is 1).

So control falls through to the generic `for_files` path (`:22137+`), which re-derives audio via
`detect_single_image_cue` (cue_parser.rs:397) — that **probes real sample counts** and rejects the
sheet when a track INDEX exceeds the tiny fixture's length → editor either never opens ("No audio
files selected") or builds the **legacy file-surface model** (`presentation_tabs` stays empty → 0).
This round **deleted** the old "explicit file/CUE expands to the parent album's surfaces" block that
used to route these into the presentation-tab editor. Note the standing comment at
`keybindings.rs:22137-22140` — this call site has regressed before.

`build_metadata_editor_for_cue_surfaces_with_policy` (keybindings.rs:16859) **already handles this
case correctly** at ~16878-16912: it uses `surface.audio_paths` directly (no sample probe) and sets
`cue_source = Some(selected.identity)`.

**Fix:**
1. Before the `for_files` fallback (between `:22124` and `:22135`), route the **single admitted
   surface** case — `cue_surfaces.len() == 1` (including single-image single-audio, not only
   `audio_paths.len() > 1`) — through
   `open_metadata_editor_for_cue_surfaces_with_active_and_policy(app, cue_surfaces, active_surface,
   cue_policy)`. This takes audio from `surface.audio_paths` and sets `cue_source`.
2. Ensure that path materializes a **length-1 presentation-tab model** (e.g.
   `MetadataEditorModel::with_presentations(vec![tab], 0)`, app.rs:7800) rather than
   `single_surface(...)` (app.rs:7793), so `presentation_tabs.len() == 1` (tests 4 and 6 assert this).
3. For `single_image:55065` (member image of a **multi-image sidecar** album with no standalone
   embedded cue): restore member-image→album resolution — when an explicit single audio file has no
   usable standalone embedded cue but its parent folder holds a native multi-FILE / single-image
   **sidecar** album referencing it, adopt that album admission with `cue_policy = SidecarOnly` (this
   is the narrow behavior of the deleted block; do not re-expand for the single-image-embedded case,
   which FIX 5 governs).

Keep the intended single-image-embedded policy (a file that *owns* its embedded cue) — that is pinned
by `foxy_explicit_cue_and_image_bypass_conflicting_cue_policy`, which must stay green.

---

### FIX 4 — explicit-audio transfer bypasses the shared instrumented passes  *(coverage test 1)*

**Test:** `metadata_cue_source_coverage_tests::multi_file_transfer_classification_batches_admission_and_embedded_reads_once`
(56205: `all selected roots must enter one batched folder-admission pass`, `left 0, right 1`).

**Cause:** the test asserts the thread-locals `TRANSFER_CLASSIFICATION_ADMISSION_PASSES`
(incremented only in `collect_transfer_metadata_cue_admission`, keybindings.rs:15243) and
`TRANSFER_CLASSIFICATION_EMBEDDED_BATCH_READS` (incremented only in
`read_transfer_embedded_candidates`, :15347) each equal 1. But for a multi-file explicit-audio
selection, `classify_tag_transfer_roots_with_priority_and_limits` returns early at
**keybindings.rs:15712-15714** via `explicit_audio_transfer_carrier(roots, cancel)`, which does its
own read through `usable_embedded_transfer_carriers_for_paths` and touches **neither** instrumented
helper. Both counters stay 0.

**Fix:** route the explicit multi-file audio path through the same instrumented shared pipeline the
main body uses (`collect_transfer_metadata_cue_admission` for the admission-pass counter +
`read_transfer_embedded_candidates` for the single merged embedded read) rather than the standalone
`explicit_audio_transfer_carrier` / `usable_embedded_transfer_carriers_for_paths` shortcut — while
preserving explicit-audio carrier semantics (the surrounding explicit-audio tests must stay green).

---

### FIX 5 — read-only writability guard missing on the explicit single-file editor branch  *(coverage test 3)*

**Test:** `metadata_cue_source_coverage_tests::read_only_embedded_cue_falls_through_to_files_and_explicit_selection_stays_exact`
(57446: `explicit_state.active_surface().cue_source.is_none()`).

**Cause:** the fixture writes an embedded CUESHEET via the writable WavPack route, then renames the
file to `.ape` (content readable, but `.ape` is a **read-only** metadata target per
`embedded_cue_metadata_target_is_writable`, tag_interchange.rs:3268). The directory-fallback and
transfer-classification assertions pass because both `usable_embedded_metadata_surfaces_for_paths`
(keybindings.rs:14643) and `validate_embedded_cue_transfer_target` (tag_interchange.rs:3278) apply the
writable predicate. But the **explicit single-file editor** branch at
`keybindings.rs:22212-22218` calls `embedded_cue_candidate_for_metadata` (:14455), which validates
only sheet structure and **omits** the writability check — so the read-only `.ape` returns `Valid`,
and the code sets `cue_source = Some(Embedded(disc.ape))` instead of leaving it `None`.

**Fix:** gate the `EmbeddedCueCandidate::Valid` arm at `keybindings.rs:22214-22218` on
`embedded_cue_metadata_target_is_writable(&audio_path)` (and, for parity with
`usable_embedded_metadata_surfaces_for_paths`, `sheet.tracks.len() >= 2`); when not writable, take
the `Absent | Invalid` arm (`suppress_cuesheet_entry_for_individual_file_target`, leave
`cue_source = None`).

---

### FIX 6 — aggregate `else` early-return pre-empts the cue-less fallback  *(pre-existing `single_image:54477`)*

**Test:** `single_image_metadata_editor_regression_tests::metadata_unresolved_only_cue_without_audio_surfaces_the_fallback_failure`
(54477: `status.contains("no CUE")`).

**Cause:** the folder holds only `broken.cue` (references a nonexistent audio file; no audio). In
`open_metadata_editor_impl`, `resolve_directory_metadata_target` returns `None`, and your new arm at
**keybindings.rs:22022-22025** sets `"metadata: selected folder contains no applicable metadata
target"` and **returns early** — pre-empting the pre-existing warnings fallback at `:22044` and the
empty-paths fallback at `:22171-22173` (which surface the specific
`"…no CUE… ordinary file/TOC discovery"` / `"… no supported audio files were found"` status).

**Fix:** only take the generic-message early-return when there is nothing else to say — i.e. when
`cue_admission_warnings.is_empty()`. When warnings are present (an unresolvable CUE), skip the
`match target` (there is no target) and **fall through** so control reaches the existing warning
emission at `:22044` and the empty-paths fallback at `:22171`. Do not otherwise change the
`IndividualFiles` / `SidecarCue` / `EmbeddedCue` arms.

---

## Fences (do NOT fold in)

- No Library. No config UX/presentation work. No custom-tag-builder / Paste-tags. No vinyl
  side-number parsing. No changes to context_menu.rs / event_loop.rs.
- Do not add new tests beyond what is needed to keep your existing pins meaningful; the failing
  tests above already encode the contracts — make them pass.

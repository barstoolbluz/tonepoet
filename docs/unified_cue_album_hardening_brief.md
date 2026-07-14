# Brief: unified synthetic cue album — hardening round (13 audited findings)

Date: 2026-07-14. For a fresh reasoning-model session. Baseline: branch
`working` at 36ff51a (a7251a2 unified model + f1a0780 title casing +
36ff51a dirty-state fix), suite 3298/0, zero cold-build warnings. The
sandbox CANNOT compile or run tests — the applier compiles, runs the
suite, and validates on the real tree. Every finding below was
adversarially audited and mechanically verified in the code at this
baseline; line numbers refer to it.

The unified-model core is sound (verified clean: grouping ladder
order-independence, scavenger live-owner safety on Unix+Windows,
distinct-image merge guard, 99-track/missing-INDEX fail-closed paths,
stale-session save guards, byte-stable parse∘generate round-trip
tests). This round fixes the integration seams around it. Do not
redesign the model; fix the seams.

## Test-authoring rules (non-negotiable)

The previous round shipped 12 broken tests the applier had to debug for
hours. This round:
- Every new test MUST reuse the existing harness shapes named per
  finding below. Do not invent fixture constructors, do not guess field
  names — mirror the neighboring test in the same module.
- Key existing harnesses:
  - `write_state()` + `tag()` in src/tui/app.rs (module
    `metadata_presentation_tab_tests`, ~12760) — MetadataEditorState
    with 2 paths for apply_write_results tests. See
    `unified_cue_album_row_entries_clear_dirty_after_all_member_images_save`
    (~12900) for the unified-sheet variant.
  - The DSOTM-shaped fixture builder in src/tui/keybindings.rs tests
    (~34690, `fixture_cue` + real ffmpeg sine FLACs + stale embedded
    CUESHEET written through `crate::tui::probe::write_all_tags`) — the
    canonical unified-surface integration fixture. Tests that need a
    real unified surface MUST build it through
    `build_metadata_editor_for_cue_surfaces` /
    `build_metadata_editor_for_cue_surfaces_with_mb_release` on that
    fixture, never by hand-assembling PresentationTab.
  - Registered cue fixture corpus: tests/fixtures/cue_roundtrip/ +
    `complete_registered_project_cue_fixture_corpus_participates_in_roundtrip_property`
    (src/convert/queue_expansion.rs ~1994). Cue fixtures are parse-only;
    .flac placeholders are 1-line text files, never probed.
  - Queue/planner tests in src/convert/queue_expansion.rs (~2500+) and
    ConversionManager tests in src/convert/mod.rs.
  - Source-scan sentinel tests (src/tui/command.rs ~16100) — string
    scans over include_str!("command.rs") pinning reducer-safety spans.
- Real-tool tests: only where the path genuinely probes/encodes; gate
  with `executable_on_path` skip guards exactly like
  tests/unified_synthetic_cue_output_boundary.rs does.
- If a finding's fix is in an async/UI seam you cannot unit-test
  without inventing infrastructure, say so in the report and pin the
  pure helper functions instead. An honest gap beats a fabricated test.

## Standing constraints

- src/convert/pipeline/ is FROZEN except where a finding names it.
- Reducer rule: no filesystem probing/parse/tag reads on the TUI
  reducer path — blocking workers only (finding 10 exists because this
  was violated).
- All tag writes through the existing lofty machinery (probe.rs
  write_all_tags / snapshot pipeline). Never shell out to wvtag/metaflac.
- MB etiquette unchanged: `mb_acquire` rate limiting, cache-first,
  pre-fetched cache bodies in tests (no network).
- TUI conventions: two-pass draw + ButtonRenderMap; every new
  command/action discoverable via context menu + :help.
- Complete-file delivery (the applier replaces whole files); include
  every file you touch, plus IMPLEMENTATION_REPORT.md describing what
  changed per finding and any honest gaps.

---

## F1 — reopen-after-save reverts saved edits (SEVERE, data loss)

src/tui/keybindings.rs:9863-10023 (unified builder). The builder always
reconstructs rows from the SIDECAR cues, regenerates the synthetic
sheet from those values, and when the members' embedded sheets disagree
it sets `tab.dirty = true` (9989-10016) — displaying the OLD sidecar
values as a "repair". But SAVE deliberately writes the concatenated
sheet only to the embedded CUESHEET tags of every member image
(sidecars untouched, by design: `cue_sidecar_writeback_plan_for_state`
returns None for multi-path, keybindings.rs:6352). So: edit track
titles → save (embedded sheets updated) → close → reopen → editor shows
pre-edit titles flagged dirty → any save reverts the user's saved work.
User-verified on the real DSOTM tree.

Required authority rule: when EVERY member image carries an embedded
CUESHEET, the texts are IDENTICAL, and that text parses to a multi-FILE
sheet whose FILE set matches the member images (same resolution the
save path used), the embedded concatenated sheet is AUTHORITATIVE: the
builder populates per-track rows, album fields, and the synthetic sheet
model from IT, and the surface opens clean (dirty=false). Sidecar truth
+ repair-dirty remains ONLY for: any member missing the sheet, texts
differing, parse failure, or FILE-set mismatch (that includes the
original stale side-A-subset case — its FILE set names one image, not
all members, so it stays a repair). INDEX drift between sidecar and
embedded (same track count) follows the embedded sheet under the
authority rule — the user may have hand-edited indexes via
:cuesheet-edit.

Tests (keybindings.rs, DSOTM-shaped fixture):
1. Build editor → mutate a track title row → run the save snapshot path
   far enough to compute the regenerated sheet → write it to both
   images through the same helpers save uses (write_all_tags) → rebuild
   the editor from scratch → assert the edited title is displayed,
   dirty == false, and the CUESHEET row originals equal the embedded
   text.
2. Stale-subset regression (the fixture's existing stale side-A sheet):
   rebuild → sidecar truth wins, dirty == true (this is the current
   test's assertion — it must keep passing).
3. Members with DIFFERING embedded sheets → sidecar truth, dirty=true.

## F2 — MB supplemental populate writes WRONG per-track IDs (data corruption)

src/tui/musicbrainz.rs:1160 (`populate_editor_mb_supplemental_with_per_track_decision`)
uses `n = state.active_surface().paths.len()` and unguarded
`per_file_values[i]` writes (1384-1444). On a unified surface (2 paths,
10 rows): ISRC/MUSICBRAINZ_TRACKID/RELEASETRACKID/ARTISTID entries
created absent get FILE dimension, then MB track 1's recording ID is
written as image A's whole-file tag and track 2's as image B's — and
because those entries ARE file-aligned, the save path persists the
wrong values. MB tracks 3..10 are dropped. Pre-existing row-dim ISRC
entries get only rows 0..1 populated.

Fix: give the supplemental pass the same dimension rule as the main
populate (row count when `cue_album_synthetic_sheet.is_some()`), the
same bounds-guarded slot writes (`set_slot`, musicbrainz.rs ~1840), and
classify created MBID/ISRC entries as per-track (row-dim) on unified
surfaces. Decide and document what whole-file MBID tags mean for a
member image of a unified album — recommendation: do NOT write
file-level MUSICBRAINZ_TRACKID/RELEASETRACKID to member images at all
(they are per-track concepts; per-track rows carry them into the
CUESHEET via ISRC only), but keep MUSICBRAINZ_ALBUMID (album-dim).

Tests (keybindings.rs MB-apply fixture, extended): apply a 10-track
release onto the 2-image unified fixture → assert ISRC row has 10
values matching MB positions 1..10; assert NO file-aligned
MUSICBRAINZ_TRACKID entry was created; assert no entry received
track-1's ID at file slot 0.

## F3 — MB apply loses ALBUM/DATE when cues lack headers

src/tui/musicbrainz.rs:1902-1919: `find_or_create` for ALBUM (1907) and
DATE (1916) uses `n` = row count on unified surfaces. The unified
builder only creates ALBUM/DATE at file dimension when the merged cue
model has values (keybindings.rs:9423-9459), so cue groups without
TITLE/REM DATE headers get MB values written into a row-dim entry that
(a) the tag writer skips (probe.rs:6577) and (b) the sheet generator
misclassifies as per-track, omitting TITLE/REM DATE from the
regenerated cue (keybindings.rs:9577-9595). Editor shows the value;
save "succeeds"; value exists nowhere.

Fix: on unified surfaces, album-scoped keys (ALBUM, DATE, GENRE,
CATALOGNUMBER, ALBUMARTIST if handled) must be created/grown at FILE
dimension (`paths.len()`), while per-track keys (TITLE, ARTIST,
TRACKNUMBER, ISRC) use row dimension. TRACKNUMBER at 1914 currently
uses `n` — verify it is row-dim on unified (correct) AND file-dim on
plain multi-file editors (also correct today via n=paths.len(); keep
both true).

Tests: unified fixture whose cues have NO album TITLE and NO REM DATE →
MB apply → assert ALBUM/DATE entries have per_file_values.len() ==
paths.len(); save-snapshot the surface and assert the regenerated sheet
contains `TITLE "<mb album>"` and `REM DATE <year>`.

## F4 — Delete key on the CUESHEET row silently destroys all embedded sheets

src/tui/keybindings.rs:5959-5983 (`metadata_editor_delete_cursor`) has
no CUESHEET guard; the unified save path has no "CUESHEET deleted +
per-track dirt" refusal (contrast the single-image path's refusal at
8962-8971). Scenario: unified surface, edit titles, press Delete on the
CUESHEET row, save → the writer pushes (CUESHEET, None) for every
member image (probe.rs:6580), deleting all sheets; per-track rows are
skipped; save reports success. Everything gone, no confirmation.

Fix: (a) `metadata_editor_delete_cursor` on a CUESHEET row must not
stage a bare row delete — route it into the existing
`:cuesheet-delete` confirmation flow
(`open_embedded_cuesheet_delete_confirmation`) on any surface where
that flow applies, else refuse with a status; (b) add the unified
equivalent of the single-image refusal: deleted CUESHEET + per-track
dirt ⇒ Err from `regenerate_unified_cue_album_cuesheet_for_save`, same
wording pattern as 8962-8971.

Tests: (1) unit — delete_cursor on the CUESHEET row of the unified
fixture leaves `deleted` empty and returns/queues the confirmation
status; (2) unified refusal — force `deleted` to contain cue_idx +
dirty title row → save regen returns Err containing "CUESHEET".

## F5 — post-:cuesheet-delete orphaned rows; dirty-fix stamps them saved

After a confirmed unified `:cuesheet-delete`
(keybindings.rs:10840-10861) the per-track rows remain visible and
editable, but they have no write path (writer skips row-dim entries;
regen bails on `pending_embedded_cuesheet_delete`, 8836-8838). Then
`reduce_saved_slots`' unified branch (src/tui/app.rs:7566-7581, added
in 36ff51a) marks those rows saved once the delete write lands —
falsely. Scenario: :cuesheet-delete → confirm → edit track 3 title →
:w → "saved", rendered clean, written nowhere.

Fix: on unified surfaces, a confirmed embedded-CUESHEET delete must
also drop the synthetic-sheet model: clear
`cue_album_synthetic_sheet`, remove the row-dimensioned per-track
entries (mirror `remove_cuesheet_derived_per_track_rows`, which today
requires paths.len()==1 — extend or add the unified variant), and
re-shape the surface as a plain 2-file editor. Additionally guard the
app.rs:7566 branch with `!tab.pending_embedded_cuesheet_delete` so
in-flight deletes never mark row entries saved.

Tests: (1) app.rs `metadata_presentation_tab_tests`: unified sheet +
pending_embedded_cuesheet_delete + dirty row → both slots saved →
assert row originals did NOT advance; (2) keybindings: confirmed
unified delete → assert cue_album_synthetic_sheet is None and no
row-dim entries remain.

## F6 — retry of a failed/cancelled merged album can never succeed

src/convert/mod.rs:700-702 (`update_item_status`) and ~846-858
(`stop_all_conversions`) delete the synthetic album.cue on first
terminal status. But Failed/Partial/Cancelled items are retryable
(src/convert/queue.rs:377-384 `can_retry`; 781-796 `retry_failed`
re-queues the same input_path; TUI at keybindings.rs:27231). Retry then
points at a deleted file — permanent failure until the album is
re-added.

Fix: artifact cleanup moves from terminal-status to queue REMOVAL
(remove_item / clear_completed / queue drop) and process exit;
Completed status may clean eagerly (not retryable... verify `can_retry`
— if Completed is retryable too, only clean on removal). Keep the
ownership map keyed by item id; retry must find the artifact intact.

Tests (mod.rs ConversionManager tests): mark a synthetic-artifact item
Failed → assert artifact file still exists → retry_failed → assert the
re-queued item's input_path exists; remove the item → assert artifact
cleaned.

## F7 — try_read claim race deletes live artifacts

src/convert/mod.rs:565-586
(`register_synthetic_cue_artifacts_for_current_queue`) uses
`self.queue.try_read()`; under lock contention (processor holds write
lock during active runs) it returns zero claims, and both callers —
mod.rs:299-302 (add_directory) and src/tui/command.rs:6657-6662 (TUI
commit) — then cleanup every "unclaimed" artifact, deleting album.cue
files for items enqueued milliseconds earlier.

Fix: claim at enqueue time under the SAME write lock that adds the
items (the Browse path at command.rs:6041-6045 already does this
correctly — make it the only pattern), or make the function async and
take a blocking read. Never treat lock contention as "no matching
items".

Tests: deterministic unit test — hold a write guard on the queue while
calling the registration path; assert it either blocks-and-succeeds or
defers, and that no artifact under live items is cleaned. If the
async-refactor makes the old fn unreachable, delete it.

## F8 — `"` in folder path breaks merged conversion

src/convert/queue_expansion.rs:1168-1170 `quote_cue_value` (`"`→`'`)
is applied to the absolute FILE path at 825-829. A path like
`…/12" Mixes/side_a.flac` is emitted as a nonexistent `'`-path; the
materializer's verbatim-then-name search fails; the merged album item
fails where per-cue conversion worked.

Fix: FILE paths must round-trip exactly. CUE cannot escape `"` inside
quoted values — so fail closed at PLAN time: if any member image's
absolute path contains `"`, refuse the merge for that group
(expansion_errors entry naming the path) and fall back to per-cue
items. Titles/performers keep the lossy `'` substitution.

Tests (queue_expansion planner tests): group whose image path contains
`"` (create the fixture dir with a quoted name; skip on filesystems
that refuse) → planner declines merge with a clear error and per-cue
items survive; sheet generation for quote-free paths byte-identical to
before.

## F9 — rejected :cuesheet-edit leaves a parseable .cue in the album folder

keybindings.rs:10702-10721 creates the edit buffer as
`.{stem}.tonepoet-embedded-cuesheet-{pid}-{nanos}.cue` INSIDE the audio
folder and deliberately keeps it on reject (11291-11313). Neither
`find_sidecar_cue_for_audio_image` (src/tui/cue_parser.rs:49-97) nor
the surface collectors filter it: a kept buffer makes sidecar detection
ambiguous (2 matches → None → sidecar writeback/shadow/delete-reshape
silently disabled) or joins the next folder open as a duplicate
surface whose save then stages duplicated tracks into every image.

Fix (both halves): (1) move the edit buffer OUT of the album folder —
use the same temp-root convention as the synthetic artifacts
(std::env::temp_dir()/tonepoet-embedded-cuesheet-edits/process-…),
keep-on-reject still fine there; (2) defense in depth: the sidecar/
surface collectors ignore `.`-hidden cue files (dotfiles are not
sidecars on any platform we ship) — implement as a shared predicate
used by find_sidecar_cue_for_audio_image AND the browse/metadata cue
collectors.

Tests: (1) cue_parser unit — a dot-prefixed valid cue next to a real
sidecar does not make detection ambiguous; (2) collector unit — folder
with real sidecars + a leftover buffer yields exactly the real
surfaces; (3) edit-reject path asserts the buffer path is under the
temp root, not the album folder.

## F10 — context-menu build does blocking tag reads on the reducer

src/tui/context_menu.rs:616-630 `audio_file_has_embedded_cuesheet`
calls `probe::read_all_tags` (which may first run FLAC journal
RECOVERY — write I/O) synchronously during right-click menu build
(reachable via keybindings.rs:5895) for every cue-less audio file.
TUI freeze on slow mounts; violates the worker-only probing rule.

Fix: menu build must not read tags. Options (pick one, document): show
the embedded-cuesheet items whenever the entry is an audio file whose
folder has a sidecar OR the file's extension supports embedded sheets,
and let the DISPATCH (already worker-safe and cleanly erroring —
keybindings.rs:10662-10699) resolve actual presence; or cache
presence from prior probes (app-level cache keyed by path+mtime) and
treat unknown as enabled. No read_all_tags on the reducer.

Tests: extend the source-scan sentinel suite (command.rs ~16100
pattern, or a new one over context_menu.rs) asserting the menu-build
span contains no `read_all_tags` / `recover_before_read` call.

## F11 — discarded expansion results leak synthetic artifacts

src/tui/command.rs:607-618: the three discard paths in
`handle_browse_convert_expansion_complete` (stale generation,
superseded generation, selection changed) drop the
`BrowseConvertExpansion` without `cleanup_synthetic_cue_artifacts`,
while every accept path transfers or cleans. No Drop impl, no startup
sweep — orphaned artifact dirs persist until another process's 24h TTL
scavenge.

Fix: clean the expansion's unowned artifacts on every discard path
(one helper, called at all three returns). Consider (optional,
low-risk) a startup scavenge pass calling the existing TTL scavenger.

Tests: reducer-level test constructing a BrowseConvertExpansion with a
temp artifact + stale generation → handler discards → artifact gone.
Mirror the existing
`compatibility_folder_adapter_rejects_owned_synthetic_cue_artifacts`
shape.

## F12 — CLI convert drops planner errors and leaks artifacts

src/main.rs:1033-1070 (`plan_cli_convert_queue`) ignores
`expansion.expansion_errors` and `expansion.synthetic_cue_artifacts`.
(a) fail-closed planner errors (>99 tracks, unparseable member cue)
⇒ silently empty queue; (b) success ⇒ artifact never owned, leaks
until TTL.

Fix: surface expansion_errors to stderr and exit non-zero when the
expansion produced errors and no queueable items (partial expansion:
print warnings, continue); register artifact ownership with the CLI's
ConversionManager the same way the TUI commit does, so completion/
removal cleans them.

Tests: main.rs unit tests exist for classify/plan paths — extend
`plan_cli_convert_queue` tests: a folder that fail-closes yields
Err/error list, not silently empty; a merged group registers its
artifact for cleanup (assert via the returned plan structure — do not
spawn conversions).

## F13 — in-editor :tags-mb has no unified-surface awareness (user-reported)

Browse context-menu "Get tags from MusicBrainz" works; `:tags-mb`
inside the unified editor yields "No MusicBrainz release matched this
disc TOC". Cause (verified): both split-cue helpers predate the
unified model — `split_cue_infos_from_metadata_editor`
(src/tui/command.rs:1660) requires `presentation_tabs.len() >= 2`
(unified: 0 tabs) and
`split_cue_infos_from_single_editor_source_folder` (1688) requires
`surface.paths.len() == 1` (unified: N paths). The unified surface
falls through to the plain file arm: a TOC built from the member
images with `fallback_seed: None` — guaranteed miss, no fallback.

Fix: add the unified branch FIRST in `try_dispatch_in_editor_tags_mb`
(src/tui/command.rs:11132): when
`state.active_surface().cue_album_synthetic_sheet.is_some()`, rebuild
the member infos from the sheet (cue_paths/audio_paths are stored on
it; `collect_single_image_cue_infos_for_sources` over the audio paths
matches the Browse path) and route through the SAME
`dispatch_split_cue_musicbrainz_concat_or_text_fallback` the tabbed
branch uses — concat-TOC probe first, then the album text fallback
seeded with the merged title (`common_cue_album_title`). Never a bare
single/concatenated-image TOC miss with no fallback. MB results then
apply positionally through the existing unified populate
(`build_metadata_editor_for_cue_surfaces_with_mb_release` /
`populate_editor_from_mb_with_per_track_decision`) — which this round
also fixes (F2/F3).

Tests: unit over the branch-selection helper: a unified-surface editor
state routes to the split-cue dispatch (assert via the same seam the
existing tabbed-branch tests use); regression: a unified state must
NOT reach the plain-file TOC arm. Use the DSOTM-shaped fixture; no
network (cache-body injection as in existing MB tests).

## Minor/latent (fix if cheap, else note in report)

- src/tui/command.rs:6126-6129: bare `retain` drops artifact ownership
  without cleanup — use the cleanup helper.
- src/convert/mod.rs:294-297: `add_directory` error path cleans ALL
  artifacts while items already added stay queued (latent; no non-test
  callers).
- src/convert/queue_expansion.rs:144-156: legacy `expand_paths_to_audio`
  adapter returns nothing for a merged album (member cues consumed,
  synthetic stripped). Dead code today (`scan_directory`); either
  re-expand with grouping disabled or document loudly.
- `cue_surface_tabs` is vestigial (never set true since the unified
  builder replaced the tabbed one; gate at event_loop.rs:5432
  unreachable). Remove marker + gate, or re-wire; do not leave dead.
- context_menu: embedded-cuesheet items enabled for sidecar-only
  sources always error at dispatch — acceptable if F10's chosen fix
  keeps dispatch-side resolution, but align enablement text/status.

## Coverage debts (close them in this round)

- tests/unified_synthetic_cue_output_boundary.rs never exercises queue
  expansion — a regression to two-folder output passes. Add one test
  that starts from a FOLDER of two side cues + images, runs the real
  planner (`expand + grouping decision` — the queue_expansion public
  API), takes the produced synthetic album.cue, and feeds THAT to
  run_pipeline_item; assert one album dir. Keep the real-tool skip
  guard.
- The tools-missing skip is silent: honor an env override
  (`TONEPOET_REQUIRE_TOOLS=1` ⇒ panic instead of skip) so CI can
  refuse to green-skip.
- Assert on-disk audio contents: exactly N audio files under
  album_dir, no subdirectories.

## Real-tree acceptance (applier runs; user verifies in TUI)

DSOTM tree (`~/livetorrents/Pink Floyd - 1973 - The Dark Side Of The
Moon (LP, 24-192, Japanese EOP-80778)`), which now carries IDENTICAL
saved concatenated sheets on both wv images from the user's session:
- Reopen Edit metadata → unified surface shows the SAVED (MB) titles,
  dirty == false (F1).
- :tags-mb inside the editor reaches concat-TOC or text fallback,
  never a bare no-match (F13).
- Fail one conversion → retry succeeds (F6).
- Suite green, zero cold warnings; boundary tests + new expansion
  boundary test pass with real tools.

## Files in this bundle

Complete files at baseline 36ff51a. Modify: src/tui/keybindings.rs,
src/tui/command.rs, src/tui/musicbrainz.rs, src/tui/app.rs,
src/tui/context_menu.rs, src/tui/event_loop.rs, src/tui/probe.rs,
src/tui/cue_parser.rs, src/convert/mod.rs,
src/convert/queue_expansion.rs, src/convert/split_cue_album.rs,
src/convert/queue.rs, src/main.rs,
tests/unified_synthetic_cue_output_boundary.rs. Reference-only:
src/tui/message.rs, src/tui/accuraterip.rs, src/tui/browse.rs,
src/tui/help.rs, src/convert/classify.rs, src/convert/cue_parser.rs,
src/convert/pipeline/mod.rs, src/convert/pipeline/materializer_cue.rs,
docs/unified_synthetic_cue_album_brief.md (prior round's model spec).
Manifests: Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md.

# Brief: unified cue album — conversion metadata, editor field order, and apply-audit hardening (G1-G3 + H1-H15)

Date: 2026-07-15. For a fresh reasoning-model session. Baseline: branch
`working` at 68c30ce (the applied 15-finding hardening round d28f081
plus five apply-audit fixes), suite 3240 lib / 0 failed, zero
cold-build warnings. The sandbox CANNOT compile or run tests — the
applier compiles, runs the suite, and validates on the real tree.

Two groups of work: G1-G3 (user-reported conversion-metadata and
editor-order gaps, root-caused) and H1-H15 (a four-way adversarial
audit of the previous apply round — every H finding was mechanically
verified in source at this baseline; line numbers refer to it).

ALREADY FIXED at 68c30ce (context, not work): ConvertCustom archive-iso
hijack; remove/clear deleting in-flight synthetic inputs (Processing
items now preserved + read-verified deferred terminal cleanup);
contention-lost completed cleanup; context-menu retry stranding
(ConversionQueue::retry_all_failed); :view error-path redraw.

Context you must not regress: the hardening round's editor model works —
saving the unified surface writes the concatenated embedded CUESHEET
(with the user's full album title and CATALOG) identically to every
member image, plus album-scoped file tags (ALBUM, CATALOGNUMBER,
RELEASECOUNTRY, MB IDs, ORIGINALYEAR, ALBUMARTIST) on each member image,
and reopening projects from the embedded sheet (authority rule),
opening clean. All of that is real-tree verified. What is broken is
that CONVERSION ignores everything the editor saved.

## Test-authoring rules and standing constraints

Same as docs/unified_cue_album_hardening_brief.md (read it — it is in
this bundle): reuse the named harnesses, mirror neighboring tests,
worker-only reducer rule, lofty-only tag writes, complete-file delivery
with IMPLEMENTATION_REPORT.md. ONE CHANGE to the frozen-pipeline rule:
src/convert/pipeline/materializer_cue.rs and (minimally, only for the
extra-metadata passthrough) src/convert/pipeline/types.rs are IN SCOPE
for G2, and EXACTLY ONE function in src/convert/pipeline/stages.rs is
unfrozen for G2's real-tag emission: `authoritative_metadata_tags`
(stages.rs:3801; plus `is_internal_metadata_extra_key` at 3790 if a new
key must stay internal). Nothing else in stages.rs may change.

## Real-tree evidence (user's conversion output)

Editor state on disk (verified): both member wv images carry the
identical embedded sheet beginning
`CATALOG EOP-80778` / `TITLE "The Dark Side of the Moon (Japan Toshiba
Harvest-Odeon EOP-80778 LP / 24-192)"` / `REM DATE 1973` /
`REM GENRE "Rock"`, and file tags Album (full pressing string),
CatalogNumber EOP-80778, RELEASECOUNTRY JP, ORIGINALYEAR 1973,
MUSICBRAINZ_ALBUMID/ALBUMARTISTID/RELEASEGROUPID, Album Artist.

Conversion output (template `%ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%]
{%TITLE_EXTRA%}`): folder `Pink Floyd - The Dark Side of the Moon
(1973) [FLAC]` — plain merged title, `{%TITLE_EXTRA%}` dropped as
empty. Track tags: Album = "The Dark Side of the Moon" (plain), no
CATALOGNUMBER, no RELEASECOUNTRY, no MB IDs, no ORIGINALYEAR.

## G1 — the conversion planner rebuilds the synthetic sheet from
## sidecars, ignoring the members' saved embedded sheet

src/convert/queue_expansion.rs: `generate_queue_synthetic_cue_album`
(843; FILE emission ~890) builds the synthetic album.cue for a merged group
from the SIDECAR cues: merged title via the common-prefix helper,
tracks/titles from sidecar TRACK blocks. The editor's durable album
truth — the identical embedded concatenated CUESHEET on every member
image — is never consulted. Result (real tree): the artifact's TITLE is
the plain common-prefix title, so the whole conversion sees the wrong
album identity even though the user saved the right one.

Required: extend the hardening round's F1 authority rule to PLANNING.
When every member image of a merged group carries an embedded CUESHEET,
the texts are IDENTICAL, and the text parses to a multi-FILE sheet
whose FILE set matches the group's member images, then the synthetic
album.cue artifact CONTENT is that embedded text with each FILE
reference rewritten to the member image's absolute path (the same
absolute-path emission and `"`-fail-closed policy the generator already
applies).

IMPLEMENTATION NOTE (audited): the editor's F1 helper
`cue_album_authoritative_embedded_cuesheet` (src/tui/keybindings.rs:9776)
is NOT directly hoistable — it validates against the TUI's
CueAlbumSyntheticSheet via `validate_unified_cue_album_edit_identity`
and uses the TUI cue parser. Implement the planner-side authority check
in src/convert/split_cue_album.rs (or queue_expansion.rs) on
`crate::convert::cue_parser` types, mirroring the SAME rules: every
member's embedded text present, trimmed texts identical, parses to a
multi-FILE sheet, FILE references resolve to exactly the group's member
images (same resolution the planner already uses for sidecar FILE
refs), plausible track structure. Add a consistency test asserting the
planner-side check and the editor-side helper accept/reject the same
fixture set (identical / differing / stale-subset), so the two layers
cannot drift. Sidecar regeneration remains the fallback
for: any member missing the sheet, texts differing, parse failure,
FILE-set mismatch. Track selection/explicit single-cue bypass behavior
unchanged.

Note: reading embedded tags is I/O — the planner already runs on
blocking workers for TUI flows (grouping/augment path) and in
plan-time CLI code; keep the tag reads inside the existing planning
step (never add reducer-path reads). lofty reads only.

Tests (queue_expansion, mirror the existing planner tests + registered
fixture conventions; real ffmpeg fixtures only where tags must be
written through lofty — follow the keybindings DSOTM fixture shape):
1. Group with identical embedded concatenated sheets on both member
   images (write through crate::tui::probe::write_all_tags or a
   convert-layer lofty helper) → artifact content equals the embedded
   text with FILE lines rewritten to absolute member paths; album
   TITLE preserves the full pressing string.
2. Members with differing/absent embedded sheets → current
   sidecar-regenerated artifact (existing tests keep passing).
3. Stale-subset embedded sheet (side A's old 5-track sheet) → sidecar
   fallback (FILE-set mismatch).

## G2 — member-image album tags never reach conversion metadata

src/convert/pipeline/materializer_cue.rs:
- `ImageAlbumMetadata` (2144) captures only album/album_artist/artist/
  genre/date/discs; everything else read from the image tags is
  dropped (`read_image_album_metadata`, 2202).
- `cue_album_metadata` (2413) prefers the SHEET for album/artist/genre/
  date and copies only the sheet's CATALOG into `extra` — so even
  when G1 fixes the sheet, CATALOGNUMBER-from-tags, RELEASECOUNTRY,
  ORIGINALYEAR, and the MusicBrainz album IDs can never reach
  `AlbumMetadata.extra`, output tags, or naming (`%TITLE_EXTRA%` /
  `%CATALOG%` resolve from album metadata + label enrichment,
  stages.rs `enrich_source_with_label_info` ~17478 and the token table
  ~29878 — reference only, do not edit).

Required:
- Extend `ImageAlbumMetadata` with a whitelisted extra map read from
  member image tags: CATALOGNUMBER, RELEASECOUNTRY, ORIGINALYEAR/
  ORIGINALDATE, MUSICBRAINZ_ALBUMID, MUSICBRAINZ_ALBUMARTISTID,
  MUSICBRAINZ_RELEASEGROUPID, ALBUMARTIST (whole-file album-scoped
  keys only — never per-track keys; the hardening round deliberately
  scrubs per-track whole-file pollution).
- Merge policy across member images: first non-empty wins in member
  order (consistent with the existing field merges at 2155); a
  conflict (differing non-empty values) logs and keeps the first.
- Flow the merged extras into `AlbumMetadata.extra` in
  `cue_album_metadata` using the existing lowercase key convention
  (`catalog` is the precedent). Sheet CATALOG wins over tag
  CATALOGNUMBER when both exist (the sheet is the editor's regenerated
  truth); tags fill what the sheet cannot express.
- AUDITED MECHANISM you must extend: `authoritative_metadata_tags`
  (stages.rs:3801) emits `extra` keys as PREFIXED tags via
  `cue_extra_tag_key("ALBUM", key)` — i.e. TONEPOET_ALBUM_<KEY>; only
  `catalog` has a real-tag special case (line ~3889 emits CATALOG).
  Without extending it, RELEASECOUNTRY etc. would surface as
  TONEPOET_ALBUM_RELEASECOUNTRY, which fails the user-visible goal.
  Add real-tag mappings in that one function for the whitelisted keys:
  catalognumber → CATALOGNUMBER, releasecountry → RELEASECOUNTRY,
  originalyear/originaldate → ORIGINALYEAR/ORIGINALDATE,
  musicbrainz_albumid → MUSICBRAINZ_ALBUMID, musicbrainz_albumartistid
  → MUSICBRAINZ_ALBUMARTISTID, musicbrainz_releasegroupid →
  MUSICBRAINZ_RELEASEGROUPID (ALBUMARTIST flows via the existing
  album_artist field — do not duplicate). Keys outside the whitelist
  keep today's prefixed behavior. Mirror the existing `catalog`
  special-case shape; suppress the generic prefixed emission for keys
  you map (no double tags).
- GOOD NEWS you must NOT re-implement: `%ALBUM%`/`%TITLE_EXTRA%`
  resolve from the album string via `album_and_title_extra_for_template`
  (stages.rs:30928) / `extract_title_extra` (31489) — with G1 fixed the
  sheet TITLE carries the parenthetical pressing info and naming works
  untouched. Do not modify naming/token code.
- Precedence for the classic fields stays sheet-first (with G1 the
  sheet now carries the editor's values); image tags remain the
  fallback exactly as today.
- AlbumMetadata (src/convert/pipeline/types.rs:1580) may gain nothing —
  `extra` already exists; only touch types.rs if a helper is genuinely
  needed.

Tests (materializer_cue unit tests, mirror the existing
cue_album_metadata/merge tests): image tags with the whitelisted keys →
extras present in AlbumMetadata.extra with sheet-CATALOG precedence;
conflicting member values → first non-empty + no panic; per-track keys
(ISRC, MUSICBRAINZ_TRACKID) never copied. Also unit-test the real-tag
emission: `authoritative_metadata_tags` with an album whose extras
carry the whitelisted keys emits CATALOGNUMBER/RELEASECOUNTRY/
ORIGINALYEAR/MB album-ID tags as REAL tag keys (and does NOT also emit
the TONEPOET_ALBUM_-prefixed duplicates for them), while an
unrecognized extra key keeps the prefixed form. Plus one boundary-test
extension in tests/unified_synthetic_cue_output_boundary.rs: write an
ALBUM + CATALOGNUMBER tag onto the member images (lofty), convert, and
assert the published album dir name / conversion output reflect the
full album string (keep the real-tool skip guard + TONEPOET_REQUIRE_TOOLS).

## G3 — unified editor field order is wrong and keys are duplicated

User screenshots (bundle-external, described): the unified surface
opens with COMPOSER first, then PERFORMER/TOTALTRACKS/DISCNUMBER/
TOTALDISCS/COMMENT/Year/RELEASECOUNTRY/MB IDs/Album Artist/
ORIGINALYEAR, and only then ALBUM/ALBUMARTIST/DATE/GENRE/
CATALOGNUMBER/TRACKNUMBER/TITLE/ARTIST/ISRC (the builder's upserts
append after the passthrough file tags). Every other editor opens as:
TITLE, ARTIST, ALBUM, DATE, GENRE, COMPOSER, PERFORMER, ALBUMARTIST,
TRACKNUMBER, TOTALTRACKS, DISCNUMBER, TOTALDISCS, COMMENT, then the
remainder. Additionally the unified surface shows DUPLICATE keys from
un-normalized display names: `Year` alongside `DATE`, `Album Artist`
alongside `ALBUMARTIST`.

Required, in the unified builder (src/tui/keybindings.rs,
build_metadata_editor_for_cue_surfaces at 10146 and its upsert helpers):
- Normalize display keys before upserting so loaded file tags and
  builder-created rows collapse into ONE row per logical key (Year →
  DATE, Album Artist → ALBUMARTIST — find the existing normalization
  the plain editor uses in probe.rs read_all_tags_merged and reuse it;
  do not write a second mapping table).
- After assembly, order entries with the SAME machinery the plain
  multi-file editor uses — `STANDARD_KEY_ORDER` +
  `sort_entries_by_standard_order` (src/tui/probe.rs:5662 / 5738,
  pub(super)) — with the unified extras (ISRC is already in
  STANDARD_KEY_ORDER; CUE MERGE NOTES and CUESHEET at the tail) in
  their existing relative positions. Do not write a second order table.
- Cursor/detail focus indices and the CUESHEET-row helpers must keep
  working after reordering (they resolve by display_key, verify).

Tests (keybindings unified fixture): assert the first N display_keys
of a freshly built unified surface are exactly the canonical prefix
(TITLE, ARTIST, ALBUM, DATE, GENRE, ...); assert exactly one DATE row
and one ALBUMARTIST row when member images carry Year/Album Artist
tags (extend the DSOTM-shaped fixture to write those tags first).

## H findings — verified defects from the apply audit

### H1 — partial save + successful retry leaves the surface dirty forever (SEVERE)

src/tui/app.rs:7607-7610: `unified_cue_album_fully_saved` requires every
member slot to appear in ONE write-result batch. Scenario: edit a track
title, `:w`, image A saves, image B fails (file locked); retry `:w` —
the writer plans work only for image B (A has no diffs), the batch
contains only B, the all-slots check fails, row originals never
advance, and every later `:w` plans zero writes while the surface stays
dirty (the Alt+O-can't-close class, unrecoverable without discarding).
Fix: track saved slots cumulatively per save generation across batches
(e.g. per-tab set of slots whose CUESHEET original already equals the
staged sheet counts as saved), so slot A's earlier success plus slot
B's retry success completes the row advance. Tests: partial save then
successful retry → rows advance, dirty clears; partial save then
failing retry → still dirty.

### H2 — forced cleanup irreversibly deletes LEGITIMATE tags (SEVERE)

src/tui/keybindings.rs:9691-9727: any non-empty whole-file
ISRC/MUSICBRAINZ_TRACKID/RELEASETRACKID/ARTISTID/TRACKNUMBER on a
member image is classified as F2 pollution, and the cleanup-only save
gate (keybindings.rs:6150-6171) deletes them on a plain `:w` of an
otherwise clean surface — no preview, no undo. But whole-file
MUSICBRAINZ_ARTISTID is a legitimate album-artist id other taggers
write, and foreign ISRC/MBID values that never came from tonepoet's F2
bug are destroyed with no CUE copy. Fix policy: (a) always cleanable:
MUSICBRAINZ_TRACKID / MUSICBRAINZ_RELEASETRACKID (recording/track ids
are never legitimate whole-file album tags on a multi-track image);
(b) ISRC / TRACKNUMBER cleanable ONLY when the value matches the F2
pollution signature — equal to the projected per-track row value of
that image's first track (track 1 on image A, first-track-of-image-B
etc.); (c) MUSICBRAINZ_ARTISTID never force-cleaned. Tests: signature
match cleans; foreign non-matching ISRC survives; ARTISTID survives.

### H3 — embedded-authority accepts sheets it cannot round-trip; first save normalizes them (data loss)

Acceptance (keybindings.rs:9776-9800) validates member/track identity
only, but every save regenerates canonical text
(regenerate_unified_cue_album_cuesheet_for_save) — a foreign-tool
sheet carrying FLAGS/PREGAP/SONGWRITER/REM COMMENT/extra INDEX lines
opens clean and then ANY save (including cleanup-only) rewrites every
member image with the normalized sheet, silently dropping those lines.
Fix: authority acceptance additionally requires round-trip stability —
regenerate(project(parse(text))) must equal the embedded text (trim
tolerance); otherwise fall back to sidecar repair (dirty=true) so the
user sees the rewrite happening. Tonepoet-generated sheets round-trip
(verified), so the real-tree flow is unaffected. Tests: foreign sheet
with FLAGS → repair-dirty, save does not silently rewrite; tonepoet
sheet → clean authority (existing tests keep passing).

### H4 — Add-field bypasses the F15 persistence gate

keybindings.rs:8817-8841: `:a` accepts any key on a unified surface,
creating a file-dim ItemKey::Unknown row — `:a TRACKNUMBER 1` writes
whole-file TRACKNUMBER to every member image (recreating the exact
pollution H2/F2 cleans; churn loop). Fix: on unified surfaces, refuse
adding keys that are managed per-track keys (TRACKNUMBER, ISRC, the MB
track ids) or that normalize into an existing row-dim key, with the
same status wording the F15 gates use. Tests: `:a TRACKNUMBER` refused
on unified; allowed on plain editors.

### H5 — bracketed-paste in the detail overlay is mis-dimensioned and ungated

src/tui/event_loop.rs:4705-4750: the paste handler caps writes at
paths.len() instead of the entry's own dimension and bypasses the F15/
slot gates — pasting a 10-line tracklist into a unified TITLE detail
applies 2 lines; on single-image per-track TITLE only line 1 applies.
Fix: dimension by the entry's per_file_values.len(), route each slot
write through the same refusal/writability checks the keyboard editor
uses. Tests: 10-row unified TITLE paste applies 10 lines; unpersistable
key paste refused.

### H6 — detail-slot eligibility indexes files by ROW on unified surfaces

keybindings.rs:21272-21283 forwards the detail ROW cursor into
`metadata_editor_slot_edit_block_reason`, which indexes
technical_details.files (member images): rows 0-1 get blocked by member
files 0-1's writability, rows 2-9 are never checked. Fix: map the row
to its owning member image via cue_album_synthetic_sheet.track_sources
before the file-eligibility lookup. Test: unified surface with image B
read-only → rows belonging to image B blocked, image A's rows editable.

### H7 — `find_or_create`'s resize DESTROYS per-track values on the single-image guard-failure path (SEVERE regression)

src/tui/musicbrainz.rs:1845 + src/tui/probe.rs:5121 (`ensure_dim_replicate`
resizes DOWN as well as up). Single-image rip with embedded CUESHEET +
sidecar cue (per-track guards fail → track_dim = 1): Phase 2 opened
TITLE/ARTIST at per-track dim; find_or_create("TITLE", …, 1) truncates
values AND originals to 1 slot, then writes the album title into slot
0 — per-track titles destroyed, revert impossible. Fix: find_or_create
must never shrink an existing entry (grow-only; use max(existing_dim,
requested_dim) or skip ensure_dim_replicate when existing len >
requested). Tests: guard-failure single-image populate leaves the
10-slot TITLE row intact with the dim-1 skip guard honored.

### H8 — stale-completion restore clobbers an open MbSelect picker

`take_metadata_editor` (event_loop.rs:5227) prefers the pending slot;
every rejection path restores with `active_overlay =
MetadataEditor(s)` (event_loop.rs:5520/5536/5544/5553/5562/5165,
command.rs:11391-11397) — destroying whatever overlay is active (e.g.
the picker for a SECOND :tags-mb run mid-selection). Fix: record which
slot the editor was taken from and restore to THAT slot (pending →
pending) when the current active_overlay is not None/MetadataEditor.
Tests: stale completion while an MbSelect is open → picker survives,
editor back in pending.

### H9 — session guard is per-SURFACE; switching tabs rejects valid completions

Dispatch captures active_surface().technical_details.session_id
(command.rs:1713-1729); completion re-reads active_surface()
(event_loop.rs:5299-5306). On tabbed editors (legacy split-cue, SACD
areas) switching tabs during the lookup makes valid completions reject
with "editor changed since lookup; rerun". Fix: the guard should carry
an editor-level identity (e.g. the session id of EVERY tab, or a
dedicated editor-instance id on MetadataEditorState) and match against
the same editor regardless of the active tab. Tests: tabbed editor,
dispatch on tab A, switch to tab B, completion applies.

### H10 — unified completions permanently rejected when the grouping ladder splits

command.rs:1448-1470 dispatches only the ACTIVE group's track paths
when the ladder resolves per-cue distinct releases, but the completion
path-match compares against the full track_sources projection
(event_loop.rs:5552-5559) and the split-cue transition check refuses
unified surfaces — every apply deterministically rejected. Fix: the
path-match must accept a dispatch vector that is a per-group SUBSET of
the unified projection (group membership derived from track_sources'
cue_paths), or the dispatch must carry the full projection with a
group annotation. Tests: unified surface + split decision → completion
applies to the active group's rows.

### H11 — apply-time split-CUE transition silently discards edits typed during the lookup

The dirty check runs only at discovery completion
(command.rs:11441-11448); the apply-time transition
(event_loop.rs:5528-5534) drops the source editor without checking
any_presentation_dirty(). Fix: same refusal the discovery path uses
(status + keep editor; user reruns after saving). Test: dirty source
editor + arriving apply → refused with status, edits intact.

### H12 — Convert `:commit` drops archive passwords (regression)

The legacy path resolved session override → keychain MRU → config and
passed it into admission; the transaction path creates items with
archive_password: None (mod.rs:678-683) and the closure only READS
item.archive_password (command.rs:6891). The Browse queue path
(command.rs:6169-6188) still resolves correctly — mirror it: resolve
per-path passwords BEFORE the transaction and set them in the
configuration closure. Test: encrypted-archive item committed with a
session password → queued item carries it.

### H13 — Convert `:commit` is all-or-nothing on format detection (regression)

mod.rs:597-608 fails the whole batch on the first detect error; the
legacy path skipped the file and queued the rest. Fix: per-file skip
with error count in the outcome (keep the transaction semantics for
admission itself). Test: batch with one unreadable file → others
queued, outcome reports 1 error.

### H14 — Convert `:commit` no longer persists the queue (regression)

The legacy helper ended with app.save_queue(); the transaction path
never calls it (only the Browse path at command.rs:6230 does). Fix:
save_queue on successful commit. Test: source-scan or behavioral.

### H15 — dot-cue filtering is asymmetric; hidden cues still poison planning and CUE import

The TUI/sidecar collectors now ignore dot-prefixed cues, but (a) queue
expansion's candidate walker and classify::is_cue_sheet_path accept
them (src/convert/classify.rs:69, queue_expansion.rs:1293) — an
AppleDouble `._album.cue` makes the synthetic grouping parse-fail and
fail-close the whole folder while every editor surface shows one clean
cue; (b) `find_cues_in_dir` (src/tui/gnudb.rs:694) feeds unfiltered
lists to the Command::ImportCue handler (command.rs:5178/5243) — a
hidden cue becomes a phantom album part. Fix: ONE shared
hidden-cue predicate applied in classify/queue-expansion walker and
find_cues_in_dir (and any other .cue enumerator — grep). Tests:
planner ignores `._album.cue`; ImportCue with a hidden cue takes the
single-CUE path.

Also fix (small, same files): context-menu CUE actions disappeared for
sidecar-bearing non-embedded carriers (`audio_file_is_cue_bearing`,
src/tui/context_menu.rs:583, is extension-only and excludes wav/aiff/
mp3 images with sidecar cues — previously covered by the sibling scan).
Show the CUE menu items for ALL audio files and let dispatch resolve,
matching the directory-menu approach. And `commit_batch` transaction
reporting: artifacts registered to pre-existing skipped items are
dropped from every reported ownership set (mod.rs:668 vs 719) —
under-reports manager ownership; include them in
artifacts_transferred_to_manager.

## Real-tree acceptance (applier runs; user verifies in TUI)

DSOTM tree (current saved state: identical embedded sheets with full
album title + CATALOG on both images; file tags carry the full album
set):
- Convert folder → output folder named `Pink Floyd - The Dark Side of
  the Moon (1973) [FLAC] {Japan Toshiba Harvest-Odeon EOP-80778 LP
  24-192}`-shaped per the user's template — `extract_title_extra`
  deliberately splits the album's parenthetical into %TITLE_EXTRA%, so
  %ALBUM% renders the base title and the pressing info lands in the
  braces (this is the user's originally requested shape). mediainfo on
  track 01 shows the full Album string (tags keep the parenthetical),
  CATALOGNUMBER, RELEASECOUNTRY, ORIGINALYEAR, and MB album IDs as
  real tag keys.
- Edit metadata → field order matches the plain editor's canonical
  order; single DATE row, single ALBUMARTIST row.
- Suite green; zero cold warnings; boundary tests (incl. the new G2
  extension) pass with real tools.
- H acceptance highlights: partial-save retry clears dirty (H1); a
  foreign ISRC tag survives a cleanup-only save (H2); `:a TRACKNUMBER`
  refused on unified (H4); single-image embedded+sidecar rip keeps
  per-track titles through :tags-mb (H7); encrypted-archive commit
  carries the session password (H12); a `._album.cue` no longer
  fail-closes the folder (H15).

## Files in this bundle

Complete files at baseline 68c30ce. Modify: src/convert/queue_expansion.rs,
src/convert/split_cue_album.rs, src/convert/classify.rs,
src/convert/pipeline/materializer_cue.rs,
src/convert/pipeline/types.rs (only if needed),
src/convert/pipeline/stages.rs (ONLY authoritative_metadata_tags /
is_internal_metadata_extra_key per the G2 unfreeze — deliver the whole
file as usual but change nothing else in it), src/convert/mod.rs
(H12/H13/H14 commit transaction + reporting), src/tui/keybindings.rs,
src/tui/probe.rs, src/tui/musicbrainz.rs, src/tui/event_loop.rs,
src/tui/command.rs, src/tui/context_menu.rs, src/tui/app.rs,
src/tui/gnudb.rs, src/tui/message.rs (if the guard shape changes),
tests/unified_synthetic_cue_output_boundary.rs.
Reference-only: src/convert/pipeline/mod.rs, src/convert/cue_parser.rs,
src/convert/queue.rs, src/tui/cue_parser.rs, src/convert/labels.rs,
src/tui/external_editor.rs, src/tui/disc_browser_actions.rs,
docs/unified_cue_album_hardening_brief.md (prior round; contains the
test-authoring rules and the F1 authority rule this round extends).
Manifests: Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md.

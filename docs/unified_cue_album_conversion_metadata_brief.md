# Brief: unified cue album — conversion metadata propagation + editor field order

Date: 2026-07-15. For a fresh reasoning-model session. Baseline: branch
`working` at d28f081 (the applied 15-finding hardening round), suite
3366/0, zero cold-build warnings. The sandbox CANNOT compile or run
tests — the applier compiles, runs the suite, and validates on the real
tree. Three findings, all user-reported during real-tree verification
and root-caused in source at this baseline (line numbers refer to it).

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
for G2. src/convert/pipeline/stages.rs remains frozen — G2 must work
through data (AlbumMetadata) consumed by existing stages code, not by
editing stages.rs.

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
(FILE emission ~825) builds the synthetic album.cue for a merged group
from the SIDECAR cues: merged title via the common-prefix helper,
tracks/titles from sidecar TRACK blocks. The editor's durable album
truth — the identical embedded concatenated CUESHEET on every member
image — is never consulted. Result (real tree): the artifact's TITLE is
the plain common-prefix title, so the whole conversion sees the wrong
album identity even though the user saved the right one.

Required: extend the hardening round's F1 authority rule to PLANNING.
When every member image of a merged group carries an embedded CUESHEET,
the texts are IDENTICAL, and the text parses to a multi-FILE sheet
whose FILE set matches the group's member images (same resolution the
editor uses — reuse/share the F1 helper from keybindings rather than
reimplementing; hoist it into src/convert/split_cue_album.rs or another
convert-layer home so both editor and planner call ONE implementation),
then the synthetic album.cue artifact CONTENT is that embedded text
with each FILE reference rewritten to the member image's absolute path
(the same absolute-path emission and `"`-fail-closed policy the
generator already applies). Sidecar regeneration remains the fallback
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
  `cue_album_metadata` under the key names the naming/labels/tag
  layers already understand — inspect how `extra` keys become output
  tags and template tokens (labels enrichment fills gaps only;
  tag-sourced values take priority) and use the existing key
  conventions (e.g. `catalog`) rather than inventing new ones. Sheet
  CATALOG wins over tag CATALOGNUMBER when both exist (the sheet is
  the editor's regenerated truth); tags fill what the sheet cannot
  express.
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
(ISRC, MUSICBRAINZ_TRACKID) never copied. Plus one boundary-test
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
build_metadata_editor_for_cue_surfaces ~9863 and its upsert helpers):
- Normalize display keys before upserting so loaded file tags and
  builder-created rows collapse into ONE row per logical key (Year →
  DATE, Album Artist → ALBUMARTIST — find the existing normalization
  the plain editor uses in probe.rs read_all_tags_merged and reuse it;
  do not write a second mapping table).
- After assembly, order entries by the SAME canonical order the plain
  multi-file editor produces (locate the existing ordering — the plain
  editor's shape in the user's correct-order screenshot comes from the
  probe.rs merge path; reuse that ordering fn or extract it) with the
  unified extras (ISRC, CUE MERGE NOTES, CUESHEET, + Add field) in
  their existing relative positions at the tail.
- Cursor/detail focus indices and the CUESHEET-row helpers must keep
  working after reordering (they resolve by display_key, verify).

Tests (keybindings unified fixture): assert the first N display_keys
of a freshly built unified surface are exactly the canonical prefix
(TITLE, ARTIST, ALBUM, DATE, GENRE, ...); assert exactly one DATE row
and one ALBUMARTIST row when member images carry Year/Album Artist
tags (extend the DSOTM-shaped fixture to write those tags first).

## Real-tree acceptance (applier runs; user verifies in TUI)

DSOTM tree (current saved state: identical embedded sheets with full
album title + CATALOG on both images; file tags carry the full album
set):
- Convert folder → output folder named `Pink Floyd - The Dark Side of
  the Moon (Japan Toshiba Harvest-Odeon EOP-80778 LP / 24-192) (1973)
  [FLAC] {...}`-shaped per the user's template (full ALBUM, TITLE_EXTRA
  populated by label enrichment), and mediainfo on track 01 shows the
  full Album string, CATALOGNUMBER, RELEASECOUNTRY, ORIGINALYEAR, MB
  album IDs.
- Edit metadata → field order matches the plain editor's canonical
  order; single DATE row, single ALBUMARTIST row.
- Suite green; zero cold warnings; boundary tests (incl. the new G2
  extension) pass with real tools.

## Files in this bundle

Complete files at baseline d28f081. Modify: src/convert/queue_expansion.rs,
src/convert/split_cue_album.rs, src/convert/pipeline/materializer_cue.rs,
src/convert/pipeline/types.rs (only if needed), src/tui/keybindings.rs,
src/tui/probe.rs, tests/unified_synthetic_cue_output_boundary.rs.
Reference-only: src/convert/pipeline/mod.rs, src/convert/mod.rs,
src/convert/cue_parser.rs, src/tui/cue_parser.rs, src/tui/command.rs,
src/tui/app.rs, src/convert/labels.rs,
docs/unified_cue_album_hardening_brief.md (prior round; contains the
test-authoring rules and the F1 authority rule this round extends).
Manifests: Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md.

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

## Files in this bundle

Complete files at baseline d28f081. Modify: src/convert/queue_expansion.rs,
src/convert/split_cue_album.rs, src/convert/pipeline/materializer_cue.rs,
src/convert/pipeline/types.rs (only if needed),
src/convert/pipeline/stages.rs (ONLY authoritative_metadata_tags /
is_internal_metadata_extra_key per the G2 unfreeze — deliver the whole
file as usual but change nothing else in it), src/tui/keybindings.rs,
src/tui/probe.rs, tests/unified_synthetic_cue_output_boundary.rs.
Reference-only: src/convert/pipeline/mod.rs, src/convert/mod.rs,
src/convert/cue_parser.rs, src/tui/cue_parser.rs, src/tui/command.rs,
src/tui/app.rs, src/convert/labels.rs,
docs/unified_cue_album_hardening_brief.md (prior round; contains the
test-authoring rules and the F1 authority rule this round extends).
Manifests: Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md.

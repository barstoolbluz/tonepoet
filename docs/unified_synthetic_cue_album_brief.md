# Brief: unified synthetic cue album — one view, one embedded sheet, one output

Date: 2026-07-14. For a fresh reasoning-model session. Baseline: branch
`working` at 59f45b1 (pushed), suite
3253/0, zero cold-build warnings. The sandbox cannot compile — favor
mechanically verifiable changes; the applier compiles, runs the suite, and
validates on the real tree.

THIS IS THE FOURTH ROUND on this album shape. Rounds 1–3 built working
machinery with the wrong user model. Read this section before anything:

- What exists and works: per-cue "surface" collection with FILE-reference
  pairing (`collect_metadata_cue_surfaces`, keybindings.rs); the
  album-grouping heuristic ladder (title prefix → concat-TOC hit=merge →
  per-cue distinct hits=split → conservative merge; `cached_or_title_split_
  cue_album_grouping_decision` + friends, command.rs) wired into metadata/
  GNUDB/MB dispatch with TOC-string caching; a presentation-TABBED editor
  (one tab per cue surface, `cue_surface_tabs` marker) incl. MB-apply
  population that slices a release across tabs; `:cuesheet-delete` /
  `:cuesheet-edit` commands (staged tag tombstone + confirm; system-editor
  edit via `external_editor::open_in_editor` with parse-validation).
- Why the user rejected it: (1) TABS are the wrong model — one album
  ripped in two pieces must present as ONE track list; the user hit "only
  5 tracks visible" three times because side B lived behind a `▾`
  selector. (2) The cuesheet commands have ZERO surfacing — no help
  entry, no menu item, no overlay affordance; only the read-only view is
  findable. (3) Conversion ignores the merge decision: converting the
  folder produced TWO output albums ("…{Japan Toshiba…}" and
  "…(Side B) (1973) [FLAC]").

## The user model (authoritative)

A same-folder cue group the ladder MERGES is ONE album everywhere:

1. METADATA: one flattened surface — all tracks (e.g. 10), continuous
   numbering, one album header. No tabs, no selector. Each row maps
   internally to its source (cue, image, local track index).
2. PERSISTENCE: one CONCATENATED synthetic cue sheet — a single valid
   multi-FILE sheet (`FILE "tdsotm_a.wv"` … tracks 01–05, `FILE
   "tdsotm_b.wv"` … tracks 06–10, continuous TRACK numbers, album-level
   PERFORMER/TITLE/DATE from the merged view). On save it is written
   IDENTICALLY to EVERY member image's embedded CUESHEET tag (the album
   is then self-describing from any file). This regeneration is
   AUTOMATIC whenever any pertinent field changes in the editor and is
   saved — not a separate user action.
3. CONVERSION: converting the folder yields ONE output album — one
   folder named from the merged album title, tracks 01–10 continuous.
   No "(Side A/B)" folders.
4. ESCAPE HATCH: explicitly selecting ONE cue (right-click a `.cue` →
   Convert) BYPASSES all grouping heuristics and converts that side
   alone, exactly as today. (Metadata on one cue still opens the unified
   album view — the bypass is conversion-only.)

## Deliverable 1 — unified metadata surface (replaces tabs for cue groups)

Replace the cue-surface TABBED editor with a single-surface editor for a
merged group:

- Build one surface whose rows are the concatenation of the group's cue
  tracks in filename-sort order, numbered 01..N. Per-row source mapping
  (cue path, image path, local index) must be carried in the editor state
  — saving and cuesheet regeneration depend on it.
- Album-level fields: reconcile across the member cues — the merged
  album title via the existing `common_cue_album_title` logic, other
  album fields (PERFORMER/DATE/GENRE/CATALOG) first-non-empty with
  divergences surfaced, not silently dropped (e.g. a row-level note or
  the mixed-value convention the editor already uses).
- Cue-parsing flexibility: member cues may differ in REM fields, casing,
  header completeness, CRLF, per-track PERFORMER presence. The unified
  view surfaces the material fields in the right places; parsing must
  not require identical headers.
- MB-apply and in-editor :tags-mb populate THIS surface positionally
  (release track k → unified row k). This replaces the per-tab slicing
  (`populate_split_cue_metadata_editor_from_mb_release`); the
  `cue_surface_tabs` marker and tab builder become dead paths for cue
  groups — remove or repurpose them coherently (disc editors keep their
  tabs; that marker was added precisely to protect them).
- A stale embedded CUESHEET on a member image (the real tree has one:
  609-byte side-A-only sheet on `tdsotm_a.wv`) must NOT shape the view —
  sidecar-derived group wins; the stale tag gets OVERWRITTEN by the
  synthetic sheet on first save.
- Track count expectations, single-image guards, CUESHEET embed logic
  for true single-file albums are unchanged.

## Deliverable 2 — the concatenated synthetic sheet

- Generator: unified view → one multi-FILE cue sheet text. Requirements:
  valid cue syntax; per-track FILE association preserved; TRACK numbers
  continuous across files (cue permits TRACK 06 as a file's first
  track); INDEX times remain LOCAL to each file (a track's INDEX is
  relative to its own image — do NOT accumulate absolute times);
  album header from the reconciled fields; deterministic output
  (byte-stable given the same view) so saves are idempotent.
- Save path: regenerate on save when any pertinent field changed; write
  to every member image's CUESHEET tag through the existing lofty-based
  tag-save machinery. Sidecar `.cue` FILES ARE NOT MODIFIED.
- Read path: when member images already carry an identical synthetic
  concatenated sheet, the editor may open from it directly, but the
  sidecar-derived group remains authoritative when they disagree
  (sidecars are the ground truth the user curates by hand).
- `:cuesheet-delete` deletes the embedded sheet from ALL member images
  (staged, confirmed, one save); `:cuesheet-edit` round-trips the
  CONCATENATED sheet through the system editor (validation must accept
  multi-FILE sheets and continuous track numbers).

## Deliverable 3 — conversion of a merged group is ONE album

Converting a folder whose cue group the ladder merges produces one album:
single output folder from the merged album title, tracks 01–10
continuous, single conversion log/companions per the normal album flow.

Design freedom, with constraints: the cleanest shape is likely to feed
conversion the SAME concatenated synthetic sheet (the CUE machinery
already resolves per-track FILE references — `analyze_cue_for_queue`,
`materializer_cue.rs` mixed layouts), staged wherever transient inputs
normally live, so one queue item = one album. But you own the design:
queue-level pairing is acceptable too. Hard constraints:
- No changes to `src/convert/pipeline/` unless a small, isolated,
  justified extension is truly unavoidable (materializer_cue.rs is
  included as REFERENCE; say so in your report if you touch it).
- Explicit single-cue conversion (Deliverable 4 path) must not regress.
- The two-folder failure mode must be pinned by a regression test.

## Deliverable 4 — explicit single-cue bypass

Right-click one `.cue` → Convert converts exactly that cue's side,
bypassing the ladder and all merge affordances. This mostly exists —
verify and pin with a test (fixture: two-cue folder, convert one cue,
assert single side staged with its own title).

## Deliverable 5 — discoverability (hard requirement, acceptance-tested)

A user who has never read the docs must be able to find view/edit/delete
for embedded cue sheets:
- Metadata overlay: a visible affordance (the overlay already surfaces
  the CUESHEET entry; give it explicit actions — view / edit / delete —
  reachable by keyboard AND mouse per the coeval convention).
- Context menu: entries under the Tagging submenu when the selection is
  cue-bearing (cue file, image with embedded sheet, or merged folder).
- `:help` (src/tui/help.rs): entries for :cuesheet-delete, :cuesheet-edit
  and the existing cue view.
- Regression tests assert the menu/help entries exist (the codebase has
  precedent for asserting rendered menu items).

## Robustness requirements (non-negotiable — a brittle pass fails review)

This is the fourth round on this feature. The implementation must be
structural, not shaped around the one test tree. Specifically:

1. MODEL, NOT TEXT SPLICING. The concatenated sheet is generated from a
   parsed, unified in-memory model (parse each member cue → merge →
   regenerate). Concatenating or regex-editing cue TEXT is forbidden.
   Required property test: parse(generate(model)) round-trips the model,
   for every fixture in the suite.
2. N PARTS, NOT TWO. Everything works for 2..N member cues (test with 3),
   with differing track counts per part. TRACK numbers are 2-digit in cue
   syntax: a merged group exceeding 99 tracks must fail closed with a
   clear status message, not emit an invalid sheet.
3. HOSTILE-INPUT TOLERANCE. Member cues may differ in: CRLF vs LF, BOM,
   header field presence/order, REM fields, casing, quoting, per-track
   PERFORMER presence, INDEX 00 pregaps, and encodings the existing
   `decode_cue_bytes_for_path` handles. None of these may break merging;
   divergent album-level values are reconciled per Deliverable 1 and
   surfaced, never silently dropped, never crashed on.
4. STABLE ROW↔SOURCE MAPPING. The unified surface's row-to-(cue, image,
   local-index) mapping must survive editing operations the editor
   already supports on other surfaces. No positional assumptions that
   break when the view is re-sorted or a field is edited.
5. MULTI-FILE SAVE IS ALL-OR-NOTHING IN EFFECT. Writing the sheet to N
   images can partially fail (permissions, formats). Either stage
   through the editor's existing save/dirty machinery so a failure
   surfaces per-file with nothing silently half-applied, or write with
   explicit per-file error reporting and a consistent retry story. A
   save that updates one image and silently skips the other is a
   defect. Re-saving an unchanged view is a byte-stable no-op.
6. TOTAL PRECEDENCE RULES. Read-path behavior must be defined (and
   tested) for every combination: no embedded sheets / one member has a
   stale per-side sheet / all members carry identical synthetic sheets /
   members carry DIVERGENT synthetic sheets / sheet on some members
   only / unparseable embedded sheet. Sidecars are ground truth on any
   disagreement; corrupt embedded data degrades gracefully to the
   sidecar-derived view with a status note.
7. CONVERSION HAS NO PART-COUNT OR NAME ASSUMPTIONS. The one-album
   conversion works for N parts, does not assume side-letter filenames,
   and if it stages a transient synthetic cue, its FILE references must
   resolve robustly (the existing resolution machinery, not string
   paths glued together) and the transient artifact is cleaned up on
   every exit path.
8. NO PANICS, NO SILENT FALLBACKS. Production paths use the status-line
   error conventions; `unwrap`/`expect` on data derived from user files
   is forbidden. Where the code cannot honor the user model (e.g. >99
   tracks, unwritable tag format), it says so explicitly and leaves
   files untouched.
9. TESTS PROVE THE PROPERTIES, NOT THE HAPPY PATH. Minimum: 3-part
   fixture; divergent-header fixture; stale-sheet-overwrite fixture;
   divergent-embedded-sheets fixture; partial-save-failure fixture
   (injectable through the existing save machinery); generator
   round-trip property test; single-cue bypass test; two-folder
   regression test. Fixtures follow the suite's existing patterns
   (real ffmpeg-encoded images where probing is involved, placeholder
   bytes where it is not — check which the exercised path needs).
10. REPORT YOUR SEAMS. The implementation report must enumerate every
    behavioral decision made where this brief left freedom, every
    precedence rule as implemented, and every place you knowingly
    accepted a limitation — so review verifies decisions, not
    archaeology.

## Real-tree acceptance (applier runs; user verifies in the TUI)

Tree: `~/livetorrents/Pink Floyd - 1973 - The Dark Side Of The Moon (LP,
24-192, Japanese EOP-80778)` — tdsotm_a/b.cue + .wv, CRLF cues, and the
STALE side-A-only embedded CUESHEET still on tdsotm_a.wv (deliberately
kept as the acceptance case).
- Edit metadata (folder OR either cue/wv file) → ONE surface, 10 rows,
  continuous numbers, merged album title.
- MB apply → proposals land on rows 1–10 positionally.
- Save → both wv files carry the identical concatenated CUESHEET tag;
  the stale side-A sheet is gone.
- Convert folder → ONE output folder, tracks 01–10.
- Convert one cue → that side only.
- :cuesheet-delete/-edit findable via menu + help, and operate on the
  concatenated sheet across both files.

## Constraints

Suite baseline 3253/0 must hold; zero cold-build warnings; tag writes via
the existing lofty machinery only; TUI conventions (two-pass draw +
ButtonRenderMap, coeval mouse/keyboard); MB etiquette unchanged
(mb_acquire, cache-first); tests use pre-fetched cache bodies, no
network; the conversion-actions UI gate untouched.

## Files in this bundle

Complete manifests included (Cargo.toml, Cargo.lock, src/lib.rs,
CLAUDE.md). Core: src/tui/command.rs, src/tui/keybindings.rs,
src/tui/event_loop.rs, src/tui/musicbrainz.rs, src/tui/cue_parser.rs,
src/tui/gnudb.rs, src/tui/accuraterip.rs, src/tui/app.rs,
src/tui/message.rs, src/tui/probe.rs, src/tui/context_menu.rs,
src/tui/draw_overlays.rs, src/tui/inline_edit.rs, src/tui/browse.rs,
src/tui/external_editor.rs, src/tui/help.rs, src/tui/mod.rs;
src/convert/classify.rs, src/convert/formats.rs,
src/convert/queue_expansion.rs, src/convert/source_admission.rs,
src/convert/cue_parser.rs, src/convert/mod.rs. Reference-only:
src/convert/pipeline/mod.rs, src/convert/pipeline/materializer_cue.rs.

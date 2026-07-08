# Brief: batch-scope multi-disc album identity + album-artist override

Date: 2026-07-07

## The two problems, from real conversions

Both are album-identity fragmentation. Detection of disc membership is
currently **per-item** (each single-track job proves its own disc layout from
its own tags — `source_has_proven_multi_disc_layout`, stages.rs:15963), and
album grouping for the *output* is driven by each track's raw ALBUM/ARTIST
tags through the folder template. Converging batch-scope evidence is never
weighed.

### Case 1 — Eat a Peach (Japan, Polydor P58P 25005-6)

One album folder containing `disc 01/`, `disc 02/`, `artwork/`. Tags are
complete and correct per disc, but the ALBUM strings differ by per-disc
catalog number:

```
disc 01: ALBUM=Eat a Peach (Japan / Polydor P58P 25005)   DISCNUMBER=1  DISCTOTAL=2
disc 02: ALBUM=Eat a Peach (Japan / Polydor P58P 25006)   DISCNUMBER=2  DISCTOTAL=2
ALBUMARTIST=The Allman Brothers Band (both), DATE=1972 (both)
```

Converting this folder produces **two** album output directories (one per
ALBUM string) — verified empirically: disc 1's four tracks land in the
`25005` dir, disc 2's five in the `25006` dir, and the companion nested
scan then copies BOTH source discs' companion files (cue/m3u/EAC logs)
into EACH album dir, so every fragment dir grows both `disc 01/` and
`disc 02/` folders. All this even though disc tags prove a single 2-disc
set and the source layout says the same. (Note:
mediainfo displays these tags as `Part`/`Part/Total`, which is why they can
look absent; `metaflac --export-tags-to=-` shows the truth.)

### Case 2 — Dreams (1989), 4-disc box

One album folder containing `disc 01/`..`disc 04/`. Per-disc ALBUM tags are
`Dreams (Disc 1)`..`Dreams (Disc 4)`, and one disc-1 track (an outtake
credited to Duane Allman) carries a different ALBUMARTIST. Converting the
folder produces **five** output directories:

```
Duane Allman - Dreams (Disc 1) (1989) [FLAC]/
The Allman Brothers Band - Dreams (Disc 1) (1989) [FLAC]/
The Allman Brothers Band - Dreams (Disc 2) (1989) [FLAC]/
... (Disc 3), (Disc 4)
```

The Duane Allman file lives inside `.../Dreams (1989)/disc 01/` alongside 16
other disc-1 tracks (17 total on the disc); every other identity signal (album title modulo the
`(Disc N)` designator, year, source directory) says it belongs to the set.

### Case 3 — the degenerate case (no disc tags at all)

Already documented as a caveat in `pre_existing_test_failures_triage_brief.md`:
a folder batch whose files have disc numbers but no DISCTOTAL (or no disc
tags at all) cannot prove multi-disc membership per-item; disc 1 tracks land
in the album root while disc 2+ get subfolders. Batch-scope evidence (sibling
`disc NN` directories, consistent artist/album/year) exists but is unused.

## Desired behavior

All of this is **gated on the user's multi-disc switch** — the existing
`create_disc_subfolders` option (`ConversionOptions`, formats.rs:444; the
TUI "disc dirs" pill). When it is off, nothing changes.

### A. Batch-scope multi-disc identity resolution

When a folder album batch is dispatched (grouping is already path-based and
disc-dir-aware: `source_grouping_root_for_dispatch_request`,
processor.rs:506, promotes `disc NN`/`cd N` dirs to their parent), resolve a
single album identity for the batch by weighing converging evidence across
all items:

- explicit disc tags (DISCNUMBER/DISCTOTAL) where present;
- `disc NN` / `disk N` / `cd N` source subdirectories (the existing
  path-hint machinery: `track_disc_number_hint`, stages.rs:16011);
- ALBUM strings that are equal after stripping disc designators —
  `(Disc N)`, `[CD N]`, `- Disc N`, etc. — and after tolerating trailing
  per-disc catalog variance (Case 1's `25005` vs `25006`; note the
  `%TITLE_EXTRA%` machinery, `extract_title_extra` stages.rs:16410, already
  parses trailing parentheticals and there is a label/catalog reference in
  docs/hexload_labels_reference.rs if useful);
- ALBUMARTIST/ARTIST and DATE agreement across the batch (majority evidence;
  Case 2's single outlier track must not veto the set).

The resolved identity must flow to everything that currently fragments:

1. **Folder-template rendering** — `%ALBUM%` (and `%ALBUMARTIST%`/`%ARTIST%`
   at folder scope) should render the resolved identity, not the raw
   per-track tag, so one album directory results. Acceptance: Eat a Peach →
   one album dir with `disc 01/`, `disc 02/`; Dreams → one album dir with
   `disc 01/`..`disc 04/` including the Duane Allman track in `disc 01/`.
2. **Disc-subfolder assignment** — per-item disc numbers derived from the
   batch evidence even when the item's own tags are silent (Case 3: all
   discs get folders, not just disc 2+).
3. **Album batch identity** — `AlbumBatchContext` / conversion-log fragment
   identity and expected track counts must treat the set as one album, so
   the assembled conversion.log covers the whole set and batch completion
   counts the union. Today per-disc ALBUM strings split the fragment
   groups across the fragmented album dirs, so none ever reaches the
   batch's expected track count: `.tonepoet-log-fragments/` litter is left
   in every dir, and with write-log on no set-wide conversion.log can
   assemble (verified on both Dreams and Eat a Peach).
4. **Companion copying** — the loose-file/nested companion scan runs per
   item from the batch source root into that item's album dir, which is
   what sprays every disc's cue/m3u/EAC-log companions into every
   fragmented dir (Case 1). Unifying the album dir should resolve this as
   a side effect — but treat it as a named requirement, not a hoped-for
   one: each source disc directory's companions must land only in the
   matching `disc NN/` folder of the single album dir, exactly once.
5. Decide and document whether resolved identity affects **written tags**.
   Suggested default: organization only — output files keep their source
   ALBUM/ALBUMARTIST tags unless the user sets the explicit override below.
   If you conclude tags should follow the resolved identity, make it a
   deliberate, documented, separately-testable choice.

Where the resolution lives is your call (the dispatcher in processor.rs
already sees the whole batch; the planner in stages.rs sees one item at a
time — that asymmetry is the design constraint to solve). Prior art for
carrying batch-scope facts into per-item requests: `AlbumBatchContext` /
`album_batch_track` on `PipelineRequest`.

### B. User-controlled album artist for the conversion

A new below-the-fold field in the TUI metadata pane: "album artist"
(user phrasing: *set album artist for conversion*). Semantics when set:

- output files are written with this ALBUMARTIST;
- identity resolution and folder rendering use it, so tag-variant tracks
  (Case 2's Duane Allman outtake) join the set unconditionally;
- empty means "no override" (heuristics above still apply).

Reuse the existing override machinery: `MetadataTextOverride`
(pipeline/types.rs:272) and the `archive_metadata_overrides` request field
show the pattern for carrying user metadata intent to the metadata stage. A
request-scope album-artist override is the analogous seam.

TUI wiring should mirror the recently added Output Options "exclude" field
end to end (that change is a good template): state field + parsed accessor in
`OutputOptionsState`-style struct (here: the metadata pane state in app.rs),
draw row, `button_map.rs` variant + `convert_screen.rs` hitbox registration +
keybindings inline-edit read/write arms + cursor mapping, preset
capture/apply round-trip, and projection into `ConversionOptions` /
`PipelineRequest`.

## Hard constraints

- Heuristics run **only** when `create_disc_subfolders` is on. Off = today's
  behavior exactly.
- Batch scope only: never merge across different source grouping roots. All
  the evidence weighing happens within one dispatched batch.
- Conservative resolution: when evidence genuinely conflicts (different
  years AND unrelated album strings AND no disc structure), do not merge —
  fall back to today's per-tag grouping. Prefer false negatives (extra
  album dirs) over false positives (unrelated albums merged).
- Deterministic: same input tree → same identity, same output layout.
- Suite baseline: 2548 lib tests passing, 9 known pre-existing failures
  (documented in docs/pre_existing_test_failures_triage_brief.md — three of
  them are disc-renumbering tests in this same feature area; fixing them
  alongside is welcome but do not regress anything else). Zero warnings.
- The sandbox cannot compile. The applier fixes compile errors; favor
  mechanically verifiable changes and state your intended behavior per
  heuristic in tests.
- Multi-disc sets also arrive as per-disc CUE images (one CUE per disc
  directory). The CUE materializer is included in this bundle — make sure
  the identity resolution and disc assignment hold for CUE-decomposed
  batches too, not just loose-file batches.

## Acceptance (real trees, will be run by the applier)

- `~/library/abb/The Allman Brothers Band - Eat a Peach (1972) [FLAC] {Japan  Polydor P58P 25005-6}`
  with disc subfolders on → ONE album dir, `disc 01` + `disc 02`, all 9
  tracks, 0 failures; each disc's companion files (cue/m3u/EAC logs)
  appear only in their own disc folder, once — no cross-disc
  contamination, no duplicate copies.
- `~/library/abb/The Allman Brothers Band - Dreams (1989) [FLAC]` → ONE
  album dir, `disc 01`..`disc 04`, 55 tracks including the Duane Allman
  outtake in `disc 01`, 0 failures; with the album-artist override set to
  "The Allman Brothers Band", output tags carry it.
- Both re-run twice → identical layouts.
- With disc subfolders off → identical to today's output.

## Files in this bundle

- `docs/multidisc_identity_brief.md` — this brief
- `docs/pre_existing_test_failures_triage_brief.md` — related known failures
- Conversion domain: `src/convert/{mod.rs, formats.rs, metadata.rs,
  processor.rs, queue_expansion.rs, classify.rs, cue_parser.rs}`
- Pipeline: `src/convert/pipeline/{mod.rs, types.rs, stages.rs,
  unified_request.rs, materializer_single.rs, materializer_cue.rs,
  materializer_archive.rs}`
- CLI: `src/main.rs`; crate wiring: `src/lib.rs`
- TUI: `src/tui/{mod.rs, app.rs, draw_metadata.rs, metadata_view_models.rs,
  draw_output_options.rs, convert_actions.rs, command.rs, keybindings.rs,
  presets.rs, button_map.rs, convert_screen.rs}`

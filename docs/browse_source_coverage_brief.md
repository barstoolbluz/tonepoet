# Brief: Browse source coverage — formats, multi-cue folders, cue/ISO actions

Date: 2026-07-13. For a fresh reasoning-model session. Baseline: branch
`working` at 1fb4716 (== main), suite 3205/0, zero cold-build warnings.
The sandbox cannot compile — favor mechanically verifiable changes; the
applier compiles, runs the suite, and exercises the real trees below.

Five user-reported gaps in the Browse screen's right-click flows. All are
about SOURCE COVERAGE: which files count as audio, how cue/image pairs are
grouped, and which entry kinds offer Convert / Edit metadata. The
conversion engine itself already handles every format involved (APE/DSF/
DFF/SHN are documented input formats; SACD/DVD-A ISOs have full pipelines)
— the gaps are all in classification, expansion, pairing, and menu gating.

## Verified root causes (traced in code — start here, but re-verify)

1. `src/convert/classify.rs::classify_file` maps ONLY flac/wav/wave/aiff/
   aif/aifc/wv/mp3/m4a/mp4/aac/opus to `EntryKind::AudioFile`. There is no
   arm for `ape`, `dsf`, `dff`, `shn`, `tta`, or `ogg` — they fall to
   `EntryKind::OtherFile` — even though `AudioFormat` already has `Ape`,
   `Dsf`, `Dff` (src/convert/formats.rs) and the pipeline decodes them.
   Everything downstream shares this one classifier:
   `queue_expansion::expand_paths_to_all_audio` (metadata-editor
   expansion), the queue expansion proper, browse listing kinds, and the
   context-menu arms.
2. `.iso` maps to `EntryKind::Archive` in `classify_file`. Browse upgrades
   ISO entries LAZILY to SacdIso/DvdAudioIso/DvdVideoIso/BlurayIso via
   magic-byte probes cached by path + mtime (+ len for DVD kinds) —
   "after settled focus or explicit actions" (see the EntryKind doc
   comments). Until that probe
   lands, a right-click sees the Archive menu.
3. `src/tui/context_menu.rs` menu arms (fn around line 540):
   - `EntryKind::SacdIso` offers "Edit metadata" and a stream submenu
     (only when the disc probe is cached) but NO general Convert item —
     unlike the DVD/Bluray arm, which has "Convert (default stream)".
   - cue files are `OtherFile`, and that arm ALREADY special-cases cue
     and shows the Convert submenu (context_menu.rs ~695) — but no Edit
     metadata item. The user still cannot convert via that submenu in
     practice; see cause 6.
   - `EntryKind::Archive` (which is what an un-probed ISO is) has a
     Convert submenu and an "Edit metadata" item, but that Edit metadata
     path expands to zero audio and dies with "No audio files selected".
4. `src/tui/gnudb.rs::find_cue_in_dir` sorts the directory's cues and
   returns THE FIRST — a one-cue-per-folder assumption. It feeds
   `cue_parser.rs` (line ~117) and the gnudb/MB TOC scan flows.
5. Metadata-editor entry (`src/tui/keybindings.rs` ~11940): directory
   selections special-case single SACD/DVD-A/DVD-V/Bluray sources, then
   fall through to `expand_audio_paths_for_metadata` → empty →
   "No audio files selected".
6. `queue_expansion::analyze_cue_for_queue` (~line 690) returns
   Err("did not resolve to a supported audio file") whenever the cue's
   FILE reference resolves to a path `is_audio_file_path` (= the same
   `classify_file`) rejects. An APE/DSF-backed cue therefore fails cue
   analysis outright — the Convert submenu renders but converting does
   nothing useful. This is the classify gap surfacing a second way, and
   it means extending `classify_file` also unlocks cue-driven conversion
   for those images. WavPack-backed cues (DSOTM) pass this check; their
   failures come from causes 4 and the pairing gap.

## Gap A+C — folders of APE/DSF/DFF (+cue) refuse Edit metadata / Convert

Real tree (Gap A, the user's "two-subfolder edge case" — its actual root
is the missing APE arm, not the nesting):

```
~/livetorrents/CCR_CHRONICLE_THE_20_GREATEST_HITS_1995_(24_K_GOLD_DISC)/
├── Artwork/…
├── CCR_CHRONICLE_THE_20_GREATEST_HITS_CD1/
│   ├── CCR_CHRONICLE_THE_20_GREATEST_HITS_CD1.ape   ← single image
│   ├── CCR_CHRONICLE_THE_20_GREATEST_HITS_CD1.cue
│   └── …(.log/.txt)
└── CCR_CHRONICLE_THE_20_GREATEST_HITS_CD2/  (same shape)
```

Right-click the top folder → Edit metadata → "No audio files selected".
Same for any folder of dsf/dff/ape with a cue; Convert from the context
menu is likewise unavailable/empty for these.

Required:
- Extend `classify_file` with `ape`, `dsf`, `dff`, `shn` (map to their
  existing `AudioFormat` variants; add variants only if one is genuinely
  missing). Audit `AudioFormat` for what exists before inventing.
  The user's list — dff, dsf, ape, wv, alac, aiff, wav — is the floor;
  wv/alac(m4a)/aiff/wav already classify, so verify rather than re-add.
- Confirm the whole downstream lights up from that one change (metadata
  edit incl. single-image cue pairing, convert expansion, analyze,
  browse listing) and pin each with a test. Where a downstream site has
  its own extension list (grep for hardcoded extension matches in
  tui/command.rs, tui/keybindings.rs, queue_expansion.rs), reconcile it.
- CCR acceptance: right-click the TOP folder → Edit metadata opens the
  editor with both discs' tracks (two single-image cue surfaces or an
  equivalent multi-disc shape — follow the existing multi-disc editor
  conventions); Convert → Custom stages both discs.

## Gap B — one folder, N cue/image pairs (sides/discs)

Real tree:

```
~/livetorrents/Pink Floyd - 1973 - The Dark Side Of The Moon (LP, 24-192, Japanese EOP-80778)/
├── tdsotm_a.cue   ├── tdsotm_a.wv   ← side A image (cue references it)
├── tdsotm_b.cue   ├── tdsotm_b.wv   ← side B image
└── Covers/…
```

Symptoms (all three): metadata edit does not surface per-track cue rows
correctly; conversion does not decompose both sides properly; the MB and
gnudb TOC scans cannot handle the folder (find_cue_in_dir picks side A
only, and side-level TOCs are half an album anyway).

Required:
- Pair each cue with ITS image (the cue's FILE reference — cue_parser
  already resolves file references; never pair by sort order alone) and
  treat the folder as an ordered multi-part source: metadata editor gets
  every track of both sides (existing multi-surface/tab conventions);
  conversion decomposes each image against its own cue, with track
  numbering/disc handling following whatever the existing multi-disc CUE
  conventions produce elsewhere.
- TOC scan: a per-side TOC will not match MB releases for an LP rip split
  by sides. Do NOT fabricate a joined pseudo-disc TOC silently. The
  acceptable shapes: per-cue TOC attempts (each side may legitimately
  match a release for single-sided pressings), plus the existing
  text-search fallback seeded from cue metadata for the album-level
  lookup. State clearly in your report what you chose and why.
- Replace/augment `find_cue_in_dir`'s first-cue-wins with an API that
  returns ALL cues (and their paired images); update its callers
  (cue_parser.rs, the :cue-* flows, TOC scans) deliberately — enumerate
  every caller in your report.
- DSOTM acceptance: Edit metadata shows sides A+B tracks; Convert stages
  both sides; :tags-mb reaches the text-fallback (or a per-side match)
  instead of failing outright.

## Gap D — right-click a cue file → Convert (make it WORK, not just render)

The Convert submenu already renders for cue files (root cause 3); the
user still reports Convert unusable. Required: trace the full action path
for a cue selection (ConvertCustom / ConvertLastUsed / preset →
BrowseConvertExpansionRequest → queue expansion → cue decision) and make
it work end-to-end for cues backed by EVERY supported image format —
including the ones cause 6 currently rejects. The pipeline's CUE
materializer already exists; expect this to be classification +
expansion, not new conversion machinery. Multi-select of cue files
behaves sanely (each pairs with its own image). Add a regression test per
image format (flac/wv/ape at minimum).

## Gap E — right-click an ISO file → Convert + Edit metadata

- `EntryKind::SacdIso`: add the general Convert entry ("Convert (default
  stream)" / the convert submenu — mirror the DVD/Bluray arm's shape).
- The Archive-until-probed window: right-clicking an `.iso` before the
  lazy classification lands shows the Archive menu. Options (choose and
  justify): run the cheap magic probes synchronously when building the
  context menu for a bare `.iso` (they read a few sectors — measure
  before rejecting), or have the Archive arm for `.iso` files defer/
  trigger classification and rebuild. A right-click on an ISO must not
  present archive semantics for a disc image.
- Edit metadata on SacdIso exists; verify it works when invoked on the
  FILE (not just via directory scan `find_single_sacd_in_dir`) and add
  the same for DvdAudioIso files if missing (the arm lists it — verify
  the action handler routes ISO file paths, `keybindings.rs` ~11940).

## Constraints

- The conversion-actions UI gate (`[ui] show_conversion_actions`) and its
  tests must be untouched.
- `src/convert/pipeline/` (actions engine, safety perimeter) is out of
  scope; the CUE/SACD materializers may be READ for interface knowledge
  but not modified unless a gap is impossible to close otherwise — if so,
  isolate and justify the change in your report.
- Keep the browse lazy-probe architecture (no synchronous heavyweight
  probing on the render path); the context-menu probe question in Gap E
  is the one sanctioned exception to consider, with measurement.
- Follow the existing TUI conventions: two-pass draw + ButtonRenderMap,
  async expansion via BrowseConvertExpansionRequest (command.rs ~100),
  status-line errors, tests colocated per module.
- Every gap gets regression tests shaped on the real trees above
  (fixture directories with tiny placeholder files; cue content matters,
  audio bytes mostly do not — check how existing cue tests fabricate
  images).
- Suite baseline 3205/0 must hold; zero cold-build warnings; stdin
  sentinel applies to any new subprocess use.

## Files in this bundle

Complete manifests and module declarations are included so nothing needs
to be requested mid-round (Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md).

Core surfaces: src/convert/classify.rs, src/convert/formats.rs,
src/convert/queue_expansion.rs, src/convert/mod.rs;
src/tui/context_menu.rs, src/tui/browse.rs, src/tui/keybindings.rs,
src/tui/command.rs, src/tui/event_loop.rs, src/tui/app.rs,
src/tui/message.rs, src/tui/probe.rs, src/tui/cue_parser.rs,
src/convert/cue_parser.rs (the queue-side parser analyze_cue_for_queue
uses — TWO distinct cue parsers exist),
src/tui/gnudb.rs, src/tui/accuraterip.rs, src/tui/musicbrainz.rs,
src/tui/cue_generate.rs, src/tui/sacd.rs, src/tui/dvda/ (directory),
src/tui/dvda_metabase.rs, src/tui/disc_browser_actions.rs, src/tui/mod.rs;
src/disc/dvda_utils.rs, src/disc/dvdv_utils.rs, src/disc/mod.rs;
src/main.rs (CLI tags-mb parity for anything you change in TOC scanning).
Reference-only: src/convert/pipeline/mod.rs, materializer_cue.rs.

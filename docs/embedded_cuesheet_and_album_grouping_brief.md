# Brief: embedded-CUESHEET lifecycle + split-cue album grouping heuristics

Date: 2026-07-14. For a fresh reasoning-model session. Baseline: branch
`working` at d09a4cc (pushed), suite 3229/0, zero cold-build warnings. The
sandbox cannot compile — favor mechanically verifiable changes; the applier
compiles, runs the suite, and validates on the real tree below.

## The real-world evidence (user-reproduced, applier-verified on disk)

Tree: `~/livetorrents/Pink Floyd - 1973 - The Dark Side Of The Moon (LP,
24-192, Japanese EOP-80778)` — `tdsotm_a.cue`+`tdsotm_a.wv` (side A, 5
tracks), `tdsotm_b.cue`+`tdsotm_b.wv` (side B, 5 tracks), CRLF cues.

APEv2 tags on the images (via wvtag):
- `tdsotm_a.wv` carries a STALE embedded `CUESHEET` tag (609 bytes — side
  A's 5 tracks only, written by an earlier session) plus a custom Album
  tag ("The Dark Side of the Moon (Japan Toshiba-EMI EOP-80778 LP /
  24-192)").
- `tdsotm_b.wv` has NO CUESHEET.

The multi-cue SURFACE editor works (right-click → Edit metadata → two
tabs, 10 tracks — shipped at d09a4cc). The two bugs below are in OTHER
flows that ignore the sidecar surfaces.

## Bug 1 — the MB-apply editor lets a stale embedded CUESHEET win

Flow: Browse → right-click folder → Tagging → "Get tags from MusicBrainz"
→ split-cue text fallback finds the right release → user selects it →
the editor opens via the MB-APPLY construction path
(`open_editor_with_mb_release` → `apply_editor_with_mb_release_decision`,
src/tui/event_loop.rs; ctx.paths built by
`paths_for_cue_metadata_surfaces`, src/tui/command.rs). That path builds
from merged file tags where the documented precedence is "embedded
CUESHEET wins" — so the editor shape comes from side A's stale 5-track
embedded sheet: 5 rows, side B absent, MB data visible only as proposals
against the wrong structure.

Required:
- When the MB result's source context resolves to MULTIPLE cue surfaces
  (the same `collect_metadata_cue_surfaces` shape the right-click path
  uses, src/tui/keybindings.rs), the MB-apply flow must open the SAME
  tabbed surface editor, with MB proposals mapped sensibly: album-level
  fields on every tab; per-track proposals mapped by position across the
  concatenated track sequence (side A tracks 1..5 = MB tracks 1..5, side
  B tracks 1..5 = MB tracks 6..10) — the ordered surface list defines the
  concatenation order (sorted by cue path, matching the collector).
- Embedded-CUESHEET precedence rule: an embedded CUESHEET that describes
  a STRICT SUBSET of the folder's sidecar surfaces (fewer tracks than the
  sidecar set; matches one surface's shape) is STALE for album-shaping
  purposes — sidecar surfaces win. Keep embedded-wins for the true
  single-file case (one file, no sidecar or one matching sidecar).
- Regression tests must reproduce this exact fixture shape: two cue/image
  pairs where ONE image carries an embedded CUESHEET describing only its
  own side (write the APEv2/for-FLAC tag through the same helpers the
  save path uses), then assert the MB-apply editor opens 2 tabs/10 rows.

## Bug 2 — in-editor :tags-mb has no split-cue awareness

Inside the (mis-shaped or even correctly-shaped) editor, `:tags-mb`
(`try_dispatch_in_editor_tags_mb`, src/tui/command.rs) computes a TOC
from the active surface's single image — via the embedded cue or sample
counts — producing a 5-track side TOC no MB release has, and the plain
file-editor path carries `fallback_seed: None`, so the user gets
"No MusicBrainz release matched this TOC" with no fallback.

Required: parity with the Browse dispatch (which since ff155b6 detects
split-cue albums and uses the album text fallback with the common-title
seed — `dispatch_split_cue_musicbrainz_text_fallback*`,
`common_cue_album_title`, src/tui/command.rs). When the editor's surfaces
(or its source folder) resolve to a multi-cue album: use the grouping
outcome (below) — concat-TOC first where the heuristic merged, then the
album text fallback seed; never emit a bare single-side TOC miss with no
fallback.

## Feature A — delete an embedded CUESHEET

User-facing ability to remove the embedded CUESHEET tag from files, at
minimum from within the metadata editor (the CUESHEET entry already
surfaces there) and ideally as a vi command (e.g. `:cuesheet-delete`,
scoped to the active surface's file(s), with a confirm). Deletion goes
through the SAME tag-save machinery the editor already uses (lofty writes
APEv2 for WavPack, Vorbis comments for FLAC, etc.) — do not shell out to
wvtag/metaflac. Deleting must also drop the synthetic per-track rows that
were derived from that CUESHEET in the open editor (re-shape from sidecar
surfaces if present).

## Feature B — hand-edit an embedded CUESHEET (SYSTEM editor)

Use the SYSTEM editor, not a new built-in widget. Precedent: sidecar
`.cue` files are already system-editable today via
`src/tui/external_editor.rs::open_in_editor` (the `:edit-file` command /
browse "Edit" action suspends the TUI, launches the user's editor, and
resumes with `force_redraw`). Embedded CUESHEETs must get the symmetric
flow:

- From the metadata editor (command, e.g. `:cuesheet-edit`, plus a
  context affordance where natural): extract the active surface's
  embedded CUESHEET text to a temp file (respect the scratch/temp
  conventions), run `open_in_editor`, and on return parse-validate the
  result (`parse_cue`; reject sheets with no tracks or tracks missing
  INDEX 01) BEFORE accepting.
- Accept: write the edited text back through the SAME tag-save machinery
  the editor uses (lofty; sidecar untouched) and re-shape the open
  editor's synthetic per-track rows from the new sheet.
- Reject: keep the buffer (tell the user where the temp file is or
  re-open on retry), surface the parse error in the status line, and
  leave the file's tag unchanged.
- The existing read-only CuePreview (`MetadataCueView`) stays as the
  quick-inspection surface. Do NOT build a multi-line ratatui editor for
  this — that is explicitly out of scope for this round.

## Feature C — album-grouping heuristic ladder for same-folder cue sets

Core conditions (already implemented): a "surface" is a cue with >= 2
tracks all referencing ONE audio file; grouping candidates are surfaces
in the SAME directory (not across subfolders — subfolder recursion serves
multi-disc trees like CCR and keeps its current behavior).

New: decide whether N same-folder surfaces are ONE album or SEVERAL,
by this ladder (stop at the first decisive rung):
1. TITLE similarity (offline): common-prefix relationship between the
   cues' TITLEs (the `common_cue_album_title` >= 4-meaningful-chars rule
   is the existing shape) — clearly-shared prefix ⇒ merge.
2. CONCAT-TOC probe (network, decisive-positive): concatenate the
   surfaces' track durations IN FILENAME-SORT ORDER into one synthetic
   TOC via the existing builders, run it through
   `lookup_release_by_toc_cascading` (stub-drop cascade composes for
   free). ANY hit ⇒ single album, definitively — an accidental
   concatenation matching a real release is vanishingly unlikely.
3. PER-CUE probes (network, decisive-negative): if each surface's own
   TOC matches a DIFFERENT MB release, that is definitive separateness ⇒
   treat as separate albums (separate metadata groupings, separate seeds).
4. Still ambiguous ⇒ conservative merge (current behavior): tabs are
   per-surface, nothing cross-contaminates, and a wrong merge is
   recoverable while a wrong split hides tracks.
A probe MISS is never treated as evidence of separateness on its own
(albums with no CD edition or no MB TOC data miss legitimately).

Wiring: the ladder's outcome feeds (a) metadata-editor surface grouping,
(b) MB dispatch (concat-TOC lookup for merged groups BEFORE the text
fallback — a TOC hit is strictly better data than a text search), and
(c) GNUDB dispatch (per-group per-cue lookups as today). Probes go
through the existing `mb_acquire` rate limiting and are cached by TOC
string (`musicbrainz_toc_cache`); the ladder must be lazy — no network
when rung 1 decides, and results reused across flows within a session.

## Constraints

- Do not modify `src/convert/pipeline/` or the conversion engine; the
  conversion-actions UI gate and its tests stay untouched.
- Tag writing stays inside the existing lofty-based save machinery.
- MB etiquette: 1 req/s global (`mb_acquire`), cache-first, and the
  ladder caps at (1 concat + N per-cue) probes per folder decision.
- Regression tests for every rung of the ladder with cached/mocked MB
  responses (the lookup fns take pre-fetched cache bodies — use that to
  avoid network in tests), plus the Bug 1 fixture described above.
- Real-tree acceptance (applier runs): DSOTM MB-apply opens 2 tabs/10
  rows despite the stale side-A CUESHEET; in-editor :tags-mb reaches a
  concat-TOC hit or the album text fallback (never a bare no-match);
  CUESHEET delete removes the tag from tdsotm_a.wv-shaped fixtures and
  reshapes the editor; suite baseline 3229/0 holds; zero cold warnings.

## Files in this bundle

Complete manifests included (Cargo.toml, Cargo.lock, src/lib.rs,
CLAUDE.md). Core: src/tui/command.rs, src/tui/keybindings.rs,
src/tui/event_loop.rs, src/tui/musicbrainz.rs, src/tui/cue_parser.rs,
src/tui/gnudb.rs, src/tui/accuraterip.rs, src/tui/app.rs,
src/tui/message.rs, src/tui/probe.rs, src/tui/context_menu.rs,
src/tui/draw_overlays.rs, src/tui/inline_edit.rs, src/tui/browse.rs,
src/tui/mod.rs; src/convert/classify.rs, src/convert/formats.rs,
src/convert/queue_expansion.rs, src/convert/source_admission.rs,
src/convert/cue_parser.rs, src/convert/mod.rs. Reference-only:
src/convert/pipeline/mod.rs.

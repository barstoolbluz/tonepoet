# Brief: bit-depth honesty + residual hardening (D1-D8, J1-J10, L1-L7, P1-P4, E1-E10)

Date: 2026-07-15. For a fresh reasoning-model session. Baseline: branch
`working` at 5b096ef (pushed), suite 3392/0, zero cold-build warnings.
The sandbox CANNOT compile or run tests — the applier compiles, runs
the suite, runs the real-tool probes, and validates on the real tree.

Two sources feed this round: (1) a USER-FOUND bug class, empirically
verified TWICE with disjoint toolchains (ffprobe cross-checked against
metaflac / wvunpack / soxi / mediainfo) through the REAL pipeline; and
(2) a whole-system journey audit at 5b096ef (four independent
reviewers; the D-series and every Tier-1 item was re-verified
mechanically by the applier; Tier-2/3 line refs come from the audit at
5b096ef and were spot-checked).

Work is TIERED. Tier 1 is non-negotiable; Tiers 2-3 are in scope but
may be honestly reported as gaps if they would compromise Tier 1
quality. Never silently skip — every dropped item must appear in
IMPLEMENTATION_REPORT.md.

## Test-authoring rules and standing constraints

Same as docs/unified_cue_album_hardening_brief.md (in this bundle):
reuse named harnesses, mirror neighboring tests, worker-only reducer
rule, lofty-only tag writes, complete-file delivery + report.
Pipeline-freeze exceptions FOR THIS ROUND (nothing else in
src/convert/pipeline/stages.rs may change):
- `validate_encoded_output_with_tool_limits` (stages.rs:1558) and its
  helpers — D5's verification extension.
- The conversion-log per-track assembly that feeds
  `crates/tonepoet-features/src/log_writer.rs` (the "Conversion: X →
  Y" line is formatted there ~314/400) — D6.
- tonepoet-pipeline (the sub-crate) is fully in scope: plugins.rs,
  mapping.rs, settings.rs, enums.rs, tools.rs, plan.rs.

========================================================================
TIER 1 — bit-depth/format honesty (the D-series) + two HIGHs + lifecycle
========================================================================

## D. The silent bit-depth/format substitution class

EMPIRICAL MATRIX (real pipeline `run_pipeline_item` + RealToolRunner,
0.3s sine CUE image sources at 192kHz in pcm_f32le and pcm_s24le,
every cell identical for preferred_tool Auto and Sox; the probe seed
ships in this bundle at tests/tmp_depth_matrix_probe.rs):

| Target  | Requested      | Actual (verified 2x)        |
|---------|----------------|-----------------------------|
| FLAC    | 16 / 24        | 16 / 24 (honored)           |
| FLAC    | **Int32**      | **24** (metaflac bps=24)    |
| WavPack | 16 / 24 / 32   | honored (wvunpack: 32-bit ints) |
| WavPack | **Float32**    | **32-bit ints** (wvunpack)  |
| ALAC    | 16 / 24        | honored                     |
| ALAC    | **Int32**      | **24** (mediainfo)          |
| WAV     | 16/24/32/32f   | ALL honored (incl pcm_f32le)|
| AIFF    | 16 / 24 / 32   | honored                     |
| AIFF    | **Float32**    | **32-bit int** (soxi: Signed Integer PCM) |

Tool-level facts (verified on the flake's tools):
- ffmpeg's FLAC encoder writes 24-bit from s32 input by DEFAULT; with
  `-strict experimental` it writes true 32-bit (verified on ffmpeg 7).
- The native `flac` CLI (1.4+) writes true 32-bit from a pcm_s32le WAV
  (verified).
- sox/sox_ng: `sox WARN formats: flac can't encode to 32-bit` →
  silently writes 24-bit. sox must NEVER be selected for FLAC+Int32.
- Root cause in tonepoet: `mapping.rs:491` maps `Int24 | Int32` to the
  SAME ffmpeg sample_fmt "s32" and `add_ffmpeg_pcm_encoder_args`
  (tonepoet-pipeline/src/plugins.rs:1327+) adds NO bits control for
  FLAC/ALAC — the 32-bit intent dies at the encoder invocation. For
  WavPack float: the wvunpack dumps show the Int32 leg used ffmpeg's
  internal WavPack writer ("encoder version 4") and the 32f leg used
  the native wavpack CLI ("encoder version 5, very high") — BOTH
  produce ints for a float request, so float intent is not plumbed on
  either route. Real-world evidence: the user's DSOTM conversion log
  (ffmpeg route, `-c:a flac -compression_level 8`, zero depth args)
  claims "32-bit/192kHz WavPack → 32-bit/192kHz FLAC" while the
  published file is 24-bit.

### D1 — honor FLAC + Int32
Route the encode so true 32-bit FLAC is produced: prefer the native
`flac` CLI for Int32 (input segments are already PcmS32LeWav, which
flac 1.4+ ingests to 32bps — verified), or ffmpeg with
`-strict experimental`; document the chosen route in the plan
description. Exclude sox from FLAC+Int32 via the capability/support
tables (ToolSupport), not via runtime warnings.

### D2 — honor ALAC + Int32
Same class. ffmpeg's ALAC encoder emitted 24 from s32p input —
determine whether ffmpeg's alac encoder supports 32bps at all (probe
empirically; afconvert does but is macOS-only). If NO tool on the
flake can produce 32-bit ALAC, then FAIL CLOSED at plan time with a
clear error ("ALAC 32-bit not supported by available encoders — choose
24-bit or WavPack/WAV"), and disable the 32 pill for ALAC in the TUI
constraint cascade (src/tui/app.rs apply_format_constraints ~3508).
Silent downgrade is the only forbidden outcome.

### D3 — honor WavPack + Float32
The native wavpack CLI encodes float input losslessly (its WAV reader
accepts pcm_f32le). Plumb float intent: when target is Float32 and
format WavPack, the segment/realize step must NOT integer-ize
(materializer CUE segments normalize to PcmS32LeWav today — the
float→int happens at extraction; see CueSegmentCarrier in
src/convert/pipeline/), and the encoder route must be the native
wavpack CLI fed float WAV. If extending the segment carrier to float
(PcmF32LeWav) is too invasive for one round, FAIL CLOSED at plan time
for CUE-image sources with a clear message, and honor Float32 for
direct file conversions where the source is already float — but state
which you did in the report. Silent int substitution is forbidden.

### D4 — honor AIFF + Float32 (or fail closed)
AIFF-C supports fl32. If ffmpeg can write it (pcm_f32be in AIFF —
probe empirically), route it; else fail closed at plan time + disable
the pill for AIFF. (WAV already honors everything and is the
reference behavior.)

### D5 — post-encode verification must assert depth/sample-format
Extend `validate_encoded_output_with_tool_limits` (stages.rs:1558,
unfrozen for this) to probe the ENCODED file's bits_per_raw_sample /
sample_fmt and fail the track when it does not satisfy the requested
target (exact depth for int targets; float sample_fmt for float
targets; document container quirks: WAV/AIFF report depth via codec
name, bits_per_raw_sample may be N/A — derive from codec). This turns
every present and future silent substitution into a loud failure.
Gate carefully: BitDepthTarget::Source asserts against the SOURCE
depth; lossy formats (MP3/AAC/Opus) are exempt.

### D6 — conversion.log must report MEASURED output
The per-track "Conversion: X → Y" line
(crates/tonepoet-features/src/log_writer.rs ~314/400) currently
formats the PLAN. Feed it the verified post-encode measurement from D5
and flag mismatches loudly if D5 is somehow bypassed. The user's real
log claimed 32-bit while shipping 24 — the paper trail must never lie.

### D7 — permanent depth/format matrix test
Graduate tests/tmp_depth_matrix_probe.rs (in this bundle) into a
permanent real-tool matrix test (convention of the CUE matrix test in
stages.rs and the boundary tests: executable_on_path skip +
TONEPOET_REQUIRE_TOOLS): for every supported (format × depth) cell,
assert requested == measured, with fail-closed cells asserted to fail
AT PLAN TIME (not at the encoder). This is the regression fence for
the whole class.

### D8 — compatibility posture (small, decide + document)
32-bit FLAC requires flac 1.4+/ffmpeg 6.1+ to DECODE; many players and
DAPs cannot. Add a one-line note to the TUI 32-bit pill description
for FLAC (pill subtitle or status hint) rather than a confirm dialog.

### D9 — sanitizer whitespace (tiny)
Folder component sanitization turned "LP / 24-192" into "LP   24-192"
(separator removed, spaces kept). Collapse runs of whitespace after
separator stripping in the naming sanitizer. Test with the exact
string.

## J1 — HIGH: row-kind must be explicit, not inferred from length

src/tui/keybindings.rs:10030 (`is_per_track = per_file_values.len() !=
n_paths`) — and the same inference in the tag writer
(src/tui/probe.rs:6603 region), `reduce_saved_slots`
(src/tui/app.rs:7624-7627), and `mark_sidecar_cue_writeback_saved`.
For a 2-image unified album with exactly 2 total tracks (one side-long
track per image — a real vinyl shape), per-track rows are
indistinguishable from file-aligned rows: the generated sheet DROPS
all track TITLE/ISRC lines, the writer sprays track titles as
whole-file tags on the images (recreating the F2 pollution this
codebase cleans), and `album_performer` falls back to track 1's
artist. The save-side gate special-cases only TRACKNUMBER.

Required: add an explicit row-kind marker to TagEntry (e.g.
`row_scope: RowScope { File, Track }` defaulting to File; the unified
builder and MB populate set Track on per-track rows) and convert the
FOUR inference sites to consult it (length inference stays as a
fallback for legacy paths that never set it — but every unified-surface
path must set it). Tests: the 2-tracks/2-images fixture end-to-end —
MB apply lands titles in the sheet, save writes NO whole-file TITLE
tags, regenerated sheet carries both TRACK TITLEs; plus regression
that 10-track surfaces behave unchanged.

## J2 — HIGH: unguarded Browse MB completion destroys an open editor

src/tui/event_loop.rs:5171 (`open_mb_select_picker`): when
`ctx.editor_park == false` (all Browse-originated lookups) the parking
block is skipped and `app.active_overlay =
ActiveOverlay::MbSelect(...)` unconditionally replaces — and DROPS —
whatever overlay is live, including a metadata editor holding unsaved
edits opened after the Browse lookup was dispatched. Esc from that
picker restores nothing.

Required: when `editor_park == false` and the active overlay is a
MetadataEditor (or any modal that must survive), park it (pending
slot) before opening the picker and restore it on every picker exit
path — or refuse to open the picker with a status and re-queue the
completion. Test: dispatch Browse-shaped completion with an open dirty
editor → editor survives picker open/cancel with edits intact.

## L1 — limited Browse expansion: artifact leak + warnings treated as fatal

src/convert/queue_expansion.rs:371-377: the limited expansion variant
returns Err on ANY `expansion_errors` entry — INCLUDING the nonfatal
per-folder warnings the unlimited variant deliberately returns
alongside usable paths — and it does so AFTER synthetic artifacts were
staged, dropping the QueueExpansionResult without cleanup. The Browse
caller's error arm (src/tui/command.rs:495-502) then has nothing to
clean; the process-root flock blocks the TTL scavenger for the process
lifetime. Required: align the limited variant with the unlimited
semantics (fatal only when nothing queueable survives; warnings carried
in the result), and clean staged artifacts on every error return.
Tests: two-folder expansion where folder B hits the quoted-path
fallback → folder A queues, warning surfaced, no artifact leak; and a
truly fatal expansion cleans everything it staged.

## L2 — commit rollback deletes sibling artifacts the retained retry batch references

src/convert/mod.rs:805-822: on mid-batch ownership/config failure the
transaction rolls back admitted items AND deletes the already-
transferred sibling artifacts (`cleanup_rolled_back_synthetic_cue_artifacts`),
while the TUI (src/tui/command.rs:6992+) deliberately keeps the source
batch loaded for retry — whose `all_paths()` still lists the deleted
album.cue files. Retry then admits items whose container no longer
exists (detect is lexical) and each fails at materialize with no
explanation. Required: on rollback, either (a) do NOT delete
transferred sibling artifacts — return them in
`artifacts_remaining_caller_owned` so the retained batch stays
self-consistent (preferred; they are ordinary caller-owned artifacts
again after rollback), or (b) also purge them from the retained source
batch. Tests: mid-batch registry failure → retry `:commit` succeeds
end-to-end.

## L3 — queue-screen retry misses unsettled Failed/Cancelled items

src/convert/queue.rs:836-857 (`retry_failed`) drains only
`self.completed`; items marked Failed/Cancelled IN PLACE in
`self.items` (stop with no active run; mid-run failure before
settle_finished) are missed while the TUI reports "Re-queued failed
items for retry" (src/tui/keybindings.rs:27864-27870 entry). The bulk
`retry_all_failed` already settles first (queue.rs:866+). Required:
`retry_failed` calls `settle_finished()` first (mirroring
retry_all_failed). Test: stop-with-no-run → select → retry → item is
Queued in the active queue.

========================================================================
TIER 2 — MB picker cluster + remaining lifecycle + editor/planner drift
========================================================================

## J3 — no in-flight :tags-mb latch
A second `:tags-mb` while one is in flight can apply a release the
user never picked or replace an open picker mid-navigation (dispatch
paths take the parked editor with a still-valid guard). Required: an
in-flight marker on the editor session (cleared on completion/
rejection); second dispatch refused with a status. Test at the
dispatch helper level.

## J4 — parked-editor completions dropped; ReplayGain wedge
The five `MetadataEditor*Complete` reducers (DetailsProbe
event_loop.rs:3707, DetailsAnalysis 3725, ReplayGain 3836,
ArtworkWrite 3897, Write 3976+) match only
`ActiveOverlay::MetadataEditor` and ignore `pending_metadata_editor`,
unlike AnalysisComplete/PreemphasisComplete (3127-3131) which handle
both slots. A ReplayGain completion arriving while the editor is
parked behind a picker is dropped; `state.replaygain_scan` never
clears; the editor can never close ("wait for completion", no cancel).
Required: all five reducers check the pending slot too (mirror the
AnalysisComplete shape). Test: parked editor + ReplayGain completion →
scan state cleared.

## J5 — :q bypasses the dirty-editor confirmation
Command::Quit (src/tui/command.rs:2919-2927) sets should_quit
unconditionally; Esc on the same dirty editor asks "Discard unsaved
metadata changes?". Required: :q with a dirty metadata editor (active
OR pending) routes through the same confirmation. Test: dirty editor +
:q → confirmation overlay, not quit.

## J6 — prefix-subset apply stamps album fields across all images
The 5b096ef prefix rule stops wrong-row TITLE corruption, but a
guarded per-group apply (grouping ladder split) still writes ALBUM/
DATE/CATALOGNUMBER/MB-album-IDs at FILE dimension — both member
images — from a release that describes only the first group.
Required: when the dispatched paths are a proper subset (prefix
group), restrict the populate to per-track fields for the covered rows
and SKIP album-scoped writes (status notes "per-group apply: album
fields unchanged"). Full-projection applies keep today's behavior.
This also unblocks lifting the prefix-only restriction later.

## J7 — false track-count warning on unified applies
`track_count_mismatch_message` (src/tui/musicbrainz.rs:1509-1519)
compares paths.len() (2 images) with release track count → "[MB
release has 12 tracks, editor has 2]" on every correct unified apply.
Use the row dimension for unified surfaces.

## J8 — :mb-back drops the editor
Command::MbBack (src/tui/command.rs:4079 region) drops the post-apply
editor and rebuilds a guard-less picker; Esc from that picker restores
nothing, and the ≤1-surface fallback opens an editor for an unrelated
Browse selection. Required: park the current editor for the rebuilt
picker (same slot-aware helpers), restore on cancel; the fallback must
not open an unrelated editor.

## J9 — per-surface guard vs editor-wide mutation on tabbed editors
Guard captures only the active surface's session (command.rs:1713-1721)
but `populate_split_cue_metadata_editor_from_mb_release` mutates every
tab; a save on another tab between dispatch and completion is not
detected. Required: editor-level generation (bump on any tab's save)
in the guard, or validate every tab's save_generation.

## J10 — multi-medium releases half-apply on unified surfaces
Unified populate matches strictly on position == i+1; a 2LP 5+5
release applies medium 1 to rows 1-5 and silently leaves 6-10.
Required: flatten multi-medium track lists to global positions for
unified surfaces (or refuse with a status naming the mismatch).

## L4 — mixed-batch commit: detect-failures silently vanish
src/convert/mod.rs:596-615 + src/tui/command.rs:7043-7065: per-file
detect errors are counted but the failed input disappears from the
Convert pane (source cleared on partial success) and
`outcome.last_error` (file + reason) is shown only when enqueued == 0.
Required: keep failed inputs in the pane (or list them in the status),
always surface last_error detail.

## L5 — clear drains pending artifacts while the item is Processing
src/convert/mod.rs:1487-1517: `cleanup_all_synthetic_cue_artifacts_except`
preserves registry entries for Processing ids but line ~1512 drains the
ENTIRE pending set (path-only, no ids). Deferred-registration artifact
+ clear-all during the race window = deletion while the worker reads
it. Required: pending entries survive the clear when ANY Processing
item's input_path matches (the queue snapshot is available at the
call sites); they remain cleaned by drop/scavenger otherwise.

## L6 — stop_all_conversions silently no-ops under contention
src/convert/mod.rs:1778: `try_write` miss → items never marked
Cancelled, no retry; next run converts work the user stopped.
Required: retry the marking (bounded retries off the reducer, or defer
via the existing pending/deferred machinery) — the cancel token fires
regardless, so this is about queue-state truth.

## L7 — latent planner retain guard
src/convert/queue_expansion.rs:575-577 removes artifacts from the
returned set without disk cleanup (contrast command.rs:6291-6296).
Make it clean the difference defensively.

## P1-P4 — editor-vs-planner admission drift (one shared policy)
The editor collector and the planner disagree on what constitutes the
album: (P1) editor recurses subfolders into one unified surface while
the planner groups strictly per-parent (Album/CD1+CD2 edits as one,
converts as two); (P2) multi-FILE side cues: planner merges them into
the synthetic group, editor excludes them (surface shows one side,
conversion merges both; grouping-decision keys can never match);
(P3) missing image: editor silently drops the dangling cue and opens
the survivor, planner fail-closes the whole folder; (P4) any surface
that fails `detect_single_image_cue` silently bypasses the grouping
ladder with unfiltered surfaces. Required: ONE membership/admission
policy, implemented in src/convert/split_cue_album.rs and consumed by
BOTH the editor collector (src/tui/keybindings.rs:9380-9530 region)
and the planner (src/convert/queue_expansion.rs:606-680), with the
editor surfacing warnings where the planner would fail closed
("member image missing: X — conversion will not include this folder").
Add a cross-layer membership lockstep test (same fixture matrix
through both, mirroring the embedded-authority lockstep test).

========================================================================
TIER 3 — editor tail
========================================================================

## E1 — FLAGS/REM round-trip in the cue model
CueTrack carries no FLAGS/REM; adopting a foreign embedded sheet with
`FLAGS PRE` and saving strips it — destroying the de-emphasis signal
the materializer consumes (materializer_cue.rs:2584-2593). Required:
carry FLAGS and unknown track-level REM lines through parse→model→
generate verbatim (ordered), byte-stable round-trip test with an
EAC-shaped sheet; until then the H3 round-trip gate correctly keeps
such sheets on the repair path — do not weaken it.

## E2 — :cue-view shows the summary instead of the sheet
src/tui/command.rs:4257 seeds the preview from `entry.value` (the
summary string on unified surfaces); the mouse pill and context menu
correctly use `per_file_values.first()`. Align the colon command; also
fix the CUESHEET row rendering (draw_overlays.rs:4423-4426 shows
summary-of-summary).

## E3 — post-:cuesheet-delete surface shows stale CUE-derived values
After delete+save reshapes to a plain editor, the rows still show
CUE-model values with clean state instead of re-reading the files.
Re-read tags after the delete save completes (worker-side).

## E4 — MB apply re-sorts entries while `deleted` holds indices
sort_entries_standard_first (musicbrainz.rs:2092) permutes entries;
remap `tab.deleted` through the same permutation.

## E5 — F2-signature misses single-member pollution shapes
cue_album_f2_signature_cleanup_key compares member i against global
track i+1 only; pollution written by saving one member standalone
(TRACKNUMBER "1" on image B) never matches. Extend the signature to
also match the member's OWN first-track values (local track 1).

## E6 — deleting the CUE MERGE NOTES row aborts save
It is informational; deletion should simply remove the row (exempt it
from the unpersistable-tombstone refusal).

## E7 — single-member open of a unified pair (dim-1 edge)
Opening one member whose sidecar has 1 track lets an album edit route
through regenerate_cue_with_overrides and write a corrupt single-FILE
10-track sheet to that image only. Guard: when the member's embedded
sheet is a multi-FILE sheet naming other images, the single-surface
editor must open READ-ONLY for CUESHEET-affecting fields with a status
pointing at the folder-level editor.

## E8 — wvunpack decode ladder missing in the materializer
The TUI/AccurateRip have a wvunpack fallback for WavPack files ffmpeg
cannot read; the materializer probes/extracts via ffmpeg only. Port
the ladder (same tool detection) so a TUI-editable album cannot fail
at materialize.

## E9 — F14 sentinel + edit-buffer GC
Extend the redraw sentinel to the :cuesheet-edit and context-menu
call sites; GC stale tonepoet-embedded-cuesheet-edits/process-* dirs
for dead PIDs at startup (same liveness check as the artifact
scavenger).

## E10 — password persistence decision (DECISION REQUIRED, do not implement silently)
Queue persistence writes item.archive_password cleartext to
conversion_queue.json and SQLite (pre-existing; the commit path now
also does it). Options: (a) #[serde(skip)] + re-resolve from
session/keychain/config on load (encrypted-archive resume changes
behavior); (b) keychain reference instead of value. Present the
tradeoff in the report; implement ONLY if the user has pre-approved in
the prompt accompanying this brief; otherwise leave untouched.

## Real-tree acceptance (applier runs; user verifies in TUI)

- DSOTM wv (32-bit float, 192kHz) → FLAC with 32-bit selected →
  mediainfo/metaflac show 32-bit output; conversion.log says
  "→ 32-bit ... FLAC" AND the file matches; a deliberate misroute
  (force sox for FLAC+32 in a test) fails loudly, not silently.
- Folder name renders "{Japan Toshiba Harvest-Odeon EOP-80778 LP
  24-192}" with single spaces (D9).
- Matrix test green under TONEPOET_REQUIRE_TOOLS=1.
- 2-tracks/2-images fixture: MB apply + save round-trips titles through
  the sheet (J1).
- Browse :tags-mb + open editor race → editor survives (J2).
- Retry after stop-with-no-run works from the queue screen (L3).

## Files in this bundle

Complete files at baseline 5b096ef. Modify:
tonepoet-pipeline/src/{plugins.rs,mapping.rs,settings.rs,enums.rs,tools.rs,plan.rs},
src/convert/pipeline/stages.rs (ONLY the D5/D6 functions named above),
src/convert/pipeline/materializer_cue.rs (D3 carrier + E8),
src/convert/pipeline/types.rs (only if the carrier/verification needs it),
crates/tonepoet-features/src/log_writer.rs (D6),
src/convert/{queue_expansion.rs,mod.rs,queue.rs,split_cue_album.rs,classify.rs},
src/tui/{keybindings.rs,event_loop.rs,command.rs,app.rs,musicbrainz.rs,probe.rs,context_menu.rs,message.rs,draw_overlays.rs,accuraterip.rs,cue_parser.rs},
tests/unified_synthetic_cue_output_boundary.rs,
tests/tmp_depth_matrix_probe.rs (RENAME to tests/depth_format_matrix.rs and graduate per D7).
Reference-only: src/convert/pipeline/{mod.rs,track_executor.rs,unified_request.rs},
src/convert/cue_parser.rs, src/tui/gnudb.rs, src/tui/external_editor.rs,
docs/unified_cue_album_hardening_brief.md,
docs/unified_cue_album_conversion_metadata_brief.md.
Manifests: Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md.

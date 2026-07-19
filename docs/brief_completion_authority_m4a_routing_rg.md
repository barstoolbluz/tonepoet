# Brief: m4a custom-tag routing, legacy completion authority, RG gating/parity, editor & DSF residuals

HEAD at time of writing: `e3a8bd4` plus this brief's commit; the archive is cut
at the brief-inclusive commit. Verify `docs/handoff_manifest.txt` FIRST
(SHA-256 of every archived file, generated at archive-cut time — it exists
only inside the archive; if your copy is incomplete, say so and scope down).
`docs/handoff_git_history.txt` carries the recent commit log + full diffs for
offline commit review. Standards unchanged: fail closed, structured signals,
value-asserting pins, honest degradation, complete files, worst-case I/O cost
statements for changed I/O paths. Every finding is mechanically verified at
HEAD with file:line evidence; refs may drift a few lines.

Context on the prior round (your round-2 delivery, applied at `5fa4831` with
apply-side fixes at `e3a8bd4` — diffs in the history dump): the DSF write
chain audited with ZERO high/medium findings. Two corrections you should
know: (1) on ffmpeg 7.1, `-movflags +use_metadata_tags` and an attached
picture are mutually exclusive in one mov mux, so m4a custom keys now ride an
AtomicParsley iTunes-freeform pass (`apply_m4a_freeform_tags`, stages.rs)
that runs AFTER artwork embedding; a hermetic stub-transcript pin covers the
invocation. (2) Unpublished tail-journal temps attribute by NAME under the
target write lock. Build on these, don't revert them.

---

# P0

## P0-1. Custom tags never reach m4a/ALAC outputs from single-file and archive sources

The freeform pass runs only inside the orchestrator metadata stage — which is
SKIPPED for SingleFile/SevenZip sources: `source_needs_authoritative_metadata`
matches `CueImage | SacdIso | DvdVideo` (conjoined with
`prepared_source_has_metadata`; SingleFile/SevenZip are excluded by the kind
match regardless — plan_bridge.rs ~381-384), and
`planner_metadata_already_satisfied` (stages.rs ~3829-3870) reports satisfied
because `supports_planner_source_tag_transfer` grants Aac/Alac
(tonepoet-pipeline/src/plugins.rs ~877-888) — even though the mov muxer's
atom allowlist then DROPS every custom key the round-2 materializers so
carefully enumerated. The log claims "planner transferred source tags"
(stages.rs ~15677). Scenario: 7z of FLACs carrying CATALOGNUMBER/BARCODE →
ALAC: keys silently dropped, log says success.

Wanted: single-file and archive sources targeting m4a/mp4 (AAC and ALAC) get
the same custom-tag guarantee as cue albums. Your design choice, documented —
candidates: stop claiming planner satisfaction for mov-muxed targets whose
payload includes non-native keys (route them through the metadata stage), or
run the freeform pass as a standalone post-step for these sources. Constraints:
rerun convergence must hold; artwork must survive (remember the mutual
exclusion); the conversion log must tell the truth about which mechanism
carried the tags. Pins: extend the hermetic invocation pin to the
single-file and archive shapes; extend the real-tools matrix (a single-file
m4a case with PRE_EMPHASIS/MY_NOTE through two passes).

## P0-2. Legacy completion family: overlay authority + in-flight guards (third pass of the same template)

You built the operation-authority discipline twice (MB/GNUDB, then
Analysis/AR/CTDB). The audit found the remaining unconverted periphery — all
verified:

- `VerifyComplete` / `CompareComplete` / `PreemphasisComplete` publish
  overlays with ZERO authority (event_loop.rs ~3383/3426/3465); a `:verify`
  completing over a `:password` prompt destroys the typed password and
  permanently strands the editor parked under it (quit then blocks on the
  parked-editor guard ~371). `PreemphasisComplete` additionally mutates the
  occupying editor AND `pending_metadata_editor` with no session guard
  (~3350-3355) — contrast Analysis (~3252-3264).
- No in-flight guards: `:verify`/`:compare`/`:preemph` use bare pending
  counters (`verify_pending = paths.len()`, command.rs ~3941; preemph ~5234)
  — overlapping runs make each LATE completion saturating-sub to zero and
  re-execute the publish block, repeatedly re-clobbering whatever the user
  opened.
- Password/confirmation Err-arms without slot checks, same clobber class:
  the Convert-screen archive-preview password prompt (event_loop.rs
  ~1367-1373; generation+path checked, slot not), the
  `ArchivePasswordForMetadataEdit` extraction-failure prompt (~1572-1580),
  and the repackage cancelled/failed confirmations (~2118/2149).
- `pending_ctdb_repair` leaks when the AR result doesn't match the deferred
  repair's first path (cleared only `if matched_page_idx.is_some()`,
  ~4948-4973) — a later unrelated `:ar` whose first-track path matches pops
  a repair Confirmation built from STALE parity data.
- `message_mutates_browse_visible_state` classifies `CtdbRepairComplete`
  but omits `OffsetCorrectionComplete` (~274-292 vs ~4612), which equally
  rewrites files.
- Note the round-2 apply added one slot guard already
  (`ArchiveListingComplete` wrong-password re-prompt, pinned) — match its
  shape for the remaining prompt arms.

Wanted: the full treatment for this family — operation ids checked first,
overlay publication only into an unobstructed slot, parked sessions restored
on failure/retirement, terminal completions for empty results, in-flight op
state replacing the raw counters, stale `pending_ctdb_repair` cleared on
mismatch. Pins per completion: a completion arriving over a dirty
editor/parked session/occupied prompt must not destroy it; overlapping runs
publish once.

## P0-3. TUI editor loudgain diverges from the pipeline's ReplayGain policy

The metadata editor's own loudgain invocation
(`metadata_replaygain_tool_args`, keybindings.rs ~7522) hardcodes `-k` — ignoring `settings.replay_gain.prevent_clipping`, which the
pipeline now honors at both its sites — and track-only editor scans never
remove stale ALBUM_GAIN/PEAK, which the pipeline stage does
(`remove_stale_album_replaygain_tags`). The same file gets clip-capped gains
from the editor and uncapped gains from the pipeline. Wanted: one shared
loudgain argument builder + post-scan album-tag policy used by both callers;
the editor path reads the same settings. Pin: editor scan with
prevent_clipping=false emits no `-k`; track-only editor scan strips ALBUM_*.

---

# P1

## P1-1. ReplayGain format gating and skip-path completeness

- loudgain is invoked on ANY staged output (stages.rs ~8191-8236) including
  containers it cannot open (DSF/DFF/W64/RF64/raw PCM/MKA are emittable) →
  NonZeroExit → stage Failed → album blocked. Scenario:
  `convert x.iso --format dsf --replaygain album`. Gate the stage on format
  capability; unsupported targets degrade with an honest log label (the
  provenance-label enum from round 2 is the place). Same gap in
  `remove_stale_album_replaygain_tags`'s lofty read (~8002-8007).
- `SkipIfComplete` + Track mode publishes stale inherited ALBUM_* tags: the
  skip path returns (~8158-8172) before the removal that the scan path
  enforces (~8237), and completeness checks track keys only (~7889). Strip
  (or refuse to trust) inherited album tags on the track-mode skip path.
- Depth-Source trust gap: the recompute predicate is conservative for
  unmeasurable source depth only under an EXPLICIT depth target
  (~7960-7971); with `BitDepthTarget::Source` the `(None, Some(_))` and
  `(Some, None)` shapes fall to `_ => false` and TRUST inherited tags even
  though the planner-default output depth may differ. Make the Source-target
  arms conservative like the unknown-rate arms (~7927/7939).

## P1-2. Verify the taglib leg of the m4a "freeform runs last" invariant

The freeform pass's doc claims it is the last writer; ReplayGain runs after
Metadata at both orchestrator sites, and its two m4a writers are: loudgain
(taglib, in-place ilst edit — UNVERIFIED whether taglib preserves iTunes
freeform atoms) and the lofty track-mode ALBUM_* removal (VERIFIED
freeform-preserving, lofty 0.21.1 companion-tag round-trip). No test ever
exercises m4a + RG + custom tags together (the matrix disables replaygain).
Wanted: a tool-gated real-file test (m4a with freeform atoms → loudgain scan
→ atoms still present); if taglib drops them, reorder or re-apply and update
the invariant doc.

## P1-3. AtomicParsley dependency policy for m4a outputs

`apply_m4a_freeform_tags` hard-errors when the tool is missing, and its pair
set is non-empty for virtually every cue album (PERFORMER/ISRC/CATALOG/
TONEPOET_* extras all ride freeform) — a non-nix user without AtomicParsley
now fails whole m4a conversions that previously succeeded minus custom tags.
The nix flake ships it and check-tools probes it. Decide and document:
degrade (skip the freeform pass with a visible warning + log honesty) vs
hard-fail; if degrading, the conversion log must say which keys were dropped.

## P1-4. DSF alias double-canonicalization in the editor read path

`probe.rs` (~6224-6245) looks up stored-value counts with the EDITOR
canonical key against maps keyed by the DSF mapper's canon (dsf_tags.rs
~3027): a Picard-tagged DSF (`MusicBrainz Album Id` style keys) records
count 0 for a slot with a value (duplicate-frame carriers can never warn);
two raw spellings unifying only at the editor layer produce duplicate rows
on the single-file path (no dedup, ~6271-6292) and last-writer-wins in the
merged paths (~6789/6973/7017) — the first alias's value AND count are
silently dropped instead of summed. Unify the canonicalization (one mapping,
applied once) and merge alias carriers by summing counts and joining
distinct values, as the intra-DSF merge already does (dsf_tags.rs ~3061).

## P1-5. Selection rendering for the remaining text inputs

The round-2 inverse-video selection covers the inline-edit renderers.
Still selection-BLIND while accepting shift-selection and Ctrl+X/C/V
through `handle_text_input_key`:
- ~16 format-overlay numeric inputs (bitrate/complexity/vbr/sinc params...,
  keybindings.rs ~20330-20612; raw `input.view()` renderers in
  draw_overlays.rs ~1412-2867 sites),
- the template-builder input (template_builder.rs ~321-323),
- generic prompt overlays + the vi command line (draw_overlays.rs
  ~1145-1216, ~2990).
Route them through the selection-aware renderer (structurally: selection
painting lives only in inline_edit.rs — lift it or reuse it). Shift+Home
then Ctrl+X currently deletes text with zero visual feedback.

---

# P2 (fix if the round has room; else report)

- DSF artwork rollback's Id3 branch lacks the staleness guard its Untagged
  sibling has (dsf_tags.rs ~653-663 vs ~1853-1858) — guard on the snapshot's
  expected geometry before restoring.
- Legacy v2 journal identity (audio prefix + size only) can mis-bind a stale
  journal to a byte-coincident SIBLING in the directory scanner
  (~1215-1284, ambiguity refusal only fires on ≥2 matches ~2430-2443) —
  tighten scanner-side attribution for v2 (e.g. also require the journal's
  target-name hint or refuse single-match restores whose identity was
  computed against a modified file).
- Legacy-journal attribution reads can race parallel workers into spurious
  fail-closed refusals (~855-858 vs ~2331-2347) — serialize the batch when
  ANY legacy journal exists in the directory, not only on shared authority.
- Preflight temp removal lacks NotFound tolerance (~2767-2776).
- No cap keeps tag+reserve under 64MiB; oversized tags degrade to perpetual
  full rewrites (~780, ~794-798) — cap the reserve or document.
- Scanner retention message for unmatched hashed journals claims identity
  mismatch though hashed attribution is name-first (~2925-2929).
- Browse: tree-pane right-clicks fall through to the file-list entry menu
  with wrong dir context (keybindings.rs ~6232-6244); column-header vs
  border-row menu inconsistency; the empty-space menu offers New
  file/folder inside archive listings where creation is refused
  (context_menu.rs ~761-779) — hide or disable there.
- `:new-file "a b.txt"` creates a literally-quoted name (command.rs ~2579,
  ~2702) — strip balanced quotes.
- ReplayGain editor refresh replaces values but retains stale
  per_file_stored_value_counts (event_loop.rs ~772-782) → spurious collapse
  warning after a scan.
- Unified cue-album upsert: new album rows get empty stored-value counts
  despite real member carriers, and the existing-row branch overwrites
  per_file_originals under retained counts, inverting revert-no-warn
  (keybindings.rs ~10488-10531; the per-track branch ~10544 is correct).
- `mark_tag_entry_saved` advances originals for misaligned rows it
  otherwise skips (app.rs ~8294-8310).
- Narrow terminals: the DetailEdit footer (~51 chars) exceeds the minimum
  inner width; the restore pill registers past the in_footer x-gate —
  clamp/wrap (draw_overlays.rs ~5778-5800, keybindings.rs ~21446).
- The detail-pill PRODUCTION button registration (draw_overlays.rs
  ~5790-5801) is untested — add a registration-geometry pin so drift can't
  make real pills dead while the self-recording tests stay green.
- Theme builder: warn (non-blocking) when a custom palette's inverse pair
  falls below the contrast thresholds the built-in pin enforces; note the
  pin is truecolor-only (256/16-color quantization unverified).
- Oversize tag values: materializers enumerate ALL text items with no size
  cap; multi-MB lyrics ride writer argv and can exceed ARG_MAX → spawn
  failure. Cap enumerated value size (with a log warning) or pass via file.
- Archive materializer lacks the `disctotal` hint the single materializer
  has (materializer_archive.rs ~1816-1819 vs materializer_single.rs
  ~280-282).
- `MetadataMutationReport::between` counts a re-keyed row as changed and
  skips its collapse check (probe.rs ~5371-5415).
- Config load-reconcile degrade nuance: `published_config_secret_references`
  still `?`-propagates (config.rs ~1531-1552) — benign (config load fails
  on the same input) but note or align.

# Explicitly out of scope

- Companion-cue behavior: RESOLVED by the user — cue copying is governed by
  the companion files-to-copy include list, exactly as today. Do not add
  layout-specific suppression or rewriting; do not re-raise the question.
- DFF tag writing; ambiguous-EAW terminal handling.

# Delivery contract

Unchanged: complete files, one archive, IMPLEMENTATION_REPORT with files/
assumptions/cut-list/I-O-cost statements, value-asserting pins, weakened
tests justified. New crates: declare against documented APIs behind
NEEDS-VERIFICATION seams; no hand-rolled crypto. Acceptance on our side:
`cargo test --workspace` green (4459 at HEAD), zero cold-build warnings,
`TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix` green.
Priorities are ordered: P0 fully correct beats P2 breadth; if an item can't
be done to standard, ship the subset that can and say what you cut.

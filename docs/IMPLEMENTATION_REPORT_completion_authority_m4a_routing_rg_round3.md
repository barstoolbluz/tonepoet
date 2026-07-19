# Implementation Report: completion authority, M4A routing, and ReplayGain parity — round 3

Date: 2026-07-18

> Release status: superseded by `IMPLEMENTATION_REPORT_completion_authority_m4a_routing_rg_round4.md`. The corrective round implements P1-5 and closes the strict-gate, artwork-survival, and source-less ReplayGain residuals identified after round 3.

## Delivery summary

This report records the original round-3 implementation. The release bundle now also includes the round-4 corrective work: P1-5 is implemented, the prescribed strict integration target executes the M4A/artwork/loudgain invariant, and source-less ReplayGain entry points fail closed. The remaining P2 backlog is still deliberately cut.

The prior apply-side corrections were preserved:

1. M4A/MP4 custom keys remain an AtomicParsley iTunes-freeform pass after the ffmpeg metadata rewrite and after artwork embedding. This round does not reintroduce `-movflags +use_metadata_tags`.
2. DSF unpublished tail-journal temporary files remain attributed by name under the target write lock. This round does not alter that path.

No new dependency or cryptographic implementation was added.

## Baseline verification

The supplied archive was extracted to a clean working directory and verified before edits with the required command:

```sh
cd tonepoet
sha256sum -c <(grep -v '^#' docs/handoff_manifest.txt)
```

Result:

- 567 manifest entries checked
- 567 passed
- 0 failed
- 631 tar members total, including directories and archive metadata entries
- 568 regular files in the supplied tree, including `docs/handoff_manifest.txt`

The source tree was therefore treated as complete. No scope reduction for missing files was necessary.

## Implemented work

### P0-1: authoritative M4A custom-tag routing for single-file and archive sources

- Added payload-aware M4A/MP4 metadata satisfaction logic. A track whose authoritative metadata includes any non-native MOV key is no longer declared complete merely because the planner transferred native source tags.
- Routed CueImage, SingleFile, and Archive M4A/MP4 artifacts with non-native keys through the existing orchestrator metadata path.
- Preserved the established write order:
  1. ffmpeg native metadata rewrite;
  2. artwork embedding, when requested;
  3. AtomicParsley freeform atoms last.
- Kept rerun convergence: the ffmpeg rewrite removes prior metadata, then AtomicParsley writes the current freeform set with `--overWrite`.
- Made conversion-log output distinguish planner-transferred native tags from non-native keys written by AtomicParsley. Disabled, skipped, failed, and unavailable outcomes no longer claim that the planner carried custom keys.
- Chose an explicit fail-closed AtomicParsley policy. When a non-native M4A/MP4 pair set is non-empty, a missing or failing AtomicParsley invocation fails the metadata stage rather than silently dropping keys. `README.md` documents this policy.
- Added ALAC to the real custom-tag matrix and its AtomicParsley dependency gate.
- Extended the real single-file two-pass custom-tag matrix to ALAC.
- Added a hermetic invocation pin covering CueImage, SingleFile, and Archive M4A shapes, including ffmpeg-before-AtomicParsley ordering, absence of executable `-movflags +use_metadata_tags`, `MY_NOTE` freeform arguments, overwrite semantics, and truthful log text.

### P0-2: Verify/Compare/Preemphasis completion authority and prompt ownership

- Added `Verify`, `Compare`, and `Preemphasis` to the existing completion-operation authority family.
- Replaced the three process-global raw pending counters with operation-owned `CompletionBatchProgress { total, remaining }`.
- Each worker completion now carries an operation ID. Stale IDs and duplicate terminal completions are rejected before result, counter, editor, overlay, or status mutation.
- Overlapping operations in the same completion family are rejected before previous results are cleared.
- Terminal overlay publication requires an unobstructed slot. Dirty active editors, parked editors, password prompts, and unrelated overlays are preserved.
- Retirement restores only the exact parked metadata-editor session captured at dispatch, and only when the overlay slot is unobstructed.
- Pre-emphasis enrichment now applies only to the exact active or parked editor session captured by the operation; it cannot mutate a later editor.
- Status progress derives from the active operation's batch state rather than unrelated global counters.
- Added slot ownership checks to:
  - Convert-screen archive-preview password prompting;
  - archive metadata-editor extraction password prompting;
  - archive repackage cancelled/failed confirmations.
- Added progress-session identity to archive-repackage progress and result messages. A late worker from an earlier same-path retry is a total no-op: it cannot change status, consume the newer context, retire the newer session, mutate deferred navigation/quit state, or replace an overlay.
- Repackage progress updates and failure/cancellation prompts may mutate only the exact `FileTaskProgress` session that launched the operation.
- Clear `pending_ctdb_repair` whenever an AccurateRip completion does not match the deferred repair's first path.
- Classify `OffsetCorrectionComplete` as a Browse-visible file mutation, matching `CtdbRepairComplete`.

### P0-3: shared ReplayGain policy for pipeline and metadata editor

- Added `src/convert/replaygain.rs` as the single shared implementation for:
  - canonical loudgain argv construction;
  - track-vs-album grouping;
  - `prevent_clipping` / `-k` policy;
  - stale album ReplayGain tag removal after track-only scans.
- The pipeline and metadata editor now call the same argv builder.
- The metadata editor resolves `ReplayGainSettings.prevent_clipping` through the same TUI-to-`PipelineSettings` conversion used by conversion requests.
- Track-only metadata-editor scans remove stale `REPLAYGAIN_ALBUM_GAIN` and `REPLAYGAIN_ALBUM_PEAK` before rereading tags into the editor.
- Files without album ReplayGain tags are not rewritten by the cleanup path.

### P1-1: ReplayGain format gating and complete skip behavior

- Added a typed ReplayGain target-support decision before any Lofty tag read or loudgain invocation.
- Explicitly unsupported output families include DSF, DFF, W64, RF64, raw PCM, MKA/MKV/WebM audio containers, DTS, AC-3, and unknown custom containers.
- Unsupported targets now degrade to a successful skipped stage with a warning and conversion-log text stating that no ReplayGain tags were written.
- Track-mode `SkipIfComplete` now removes inherited album-level tags before returning the skipped result.
- Source-relative bit-depth trust is conservative whenever either resolved source or target PCM depth is unknown. Unknown depth is not treated as proof of signal/peak equivalence.
- Request-time and terminal conversion-log labels cover disabled, unsupported, trusted, recomputed, failed, and no-successful-output states.

### P1-2: freeform preservation through loudgain/taglib

- Added a tool-gated real-file test that:
  1. creates an ALAC/M4A output carrying custom freeform keys;
  2. applies the real AtomicParsley pass;
  3. runs the pipeline ReplayGain stage through real loudgain/taglib;
  4. verifies that the custom keys remain and that all four ReplayGain keys are present.
- In the final corrective bundle, the invariant also lives in `tests/depth_format_matrix.rs`; therefore `TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix` fails when ffmpeg/ffprobe/AtomicParsley/loudgain are unavailable and executes the production-path preservation test when they are present.
- No speculative reorder or duplicate freeform write was added without evidence that the pinned taglib version drops atoms.

### P1-3: AtomicParsley dependency policy

- Policy: hard requirement only when the authoritative non-native M4A/MP4 pair set is non-empty.
- Native-only M4A metadata does not invoke AtomicParsley.
- Non-native-key conversions fail closed if AtomicParsley is unavailable or returns failure.
- The README and implementation comments state the policy and its rationale.

### P1-4: DSF alias canonicalization

- Replaced the duplicate DSF/editor canonicalization tables with one shared `canonical_metadata_key` mapping.
- Added MusicBrainz/Picard spellings to the shared map.
- DSF snapshots are canonicalized once in the backend. The editor consumes snapshot keys as-is rather than applying a second transformation before count lookup.
- Alias carriers merge into one row with canonical-spelling values first, distinct alias values retained, and stored-value counts summed.
- Added backend and editor pins for a canonical MusicBrainz key plus a Picard-spelled alias producing one row, two values, and a carrier count of three.

### Included P2 correction: ReplayGain editor refresh carrier counts

- A ReplayGain refresh now replaces `per_file_stored_value_counts` from the newly read values and clears stale `has_multiple_stored_values` state.
- This prevents a completed scan from retaining duplicate-carrier warnings derived from the pre-scan snapshot.

## Files touched

- `README.md`
  - documents the fail-closed AtomicParsley policy for authoritative non-native M4A/MP4 tags.
- `src/convert/mod.rs`
  - registers the shared ReplayGain module.
- `src/convert/replaygain.rs` (new)
  - shared loudgain argv and track-only album-tag cleanup policy, with value-asserting tests.
- `src/convert/pipeline/stages.rs`
  - M4A routing, freeform payload detection, log truthfulness, ReplayGain target gating/trust logic, shared ReplayGain calls, and hermetic/real-tool pins.
- `src/dsf_tags.rs`
  - shared canonical metadata-key mapping and alias/count merge pin.
- `src/tui/app.rs`
  - completion operation kinds, operation-owned batch state, archive repackage progress authority, and status derivation.
- `src/tui/command.rs`
  - operation allocation and identity propagation for Verify/Compare/Preemphasis workers.
- `src/tui/event_loop.rs`
  - completion acceptance/retirement/publication, prompt-slot guards, archive-repackage session authority, CTDB repair cleanup, OffsetCorrection mutation classification, ReplayGain editor refresh correction, and regression tests.
- `src/tui/keybindings.rs`
  - metadata-editor ReplayGain policy resolution, shared argv, and track-only cleanup.
- `src/tui/message.rs`
  - operation IDs for legacy completion messages and progress-session IDs for archive repackage messages.
- `src/tui/probe.rs`
  - shared DSF/editor canonicalization and single-pass DSF snapshot consumption.
- `docs/IMPLEMENTATION_REPORT_completion_authority_m4a_routing_rg_round3.md` (new)
  - this report.
- `docs/handoff_manifest.txt`
  - regenerated after all edits so the delivered archive verifies independently.

## Tests and source-level validation added

Value-asserting coverage includes:

- CueImage, SingleFile, and Archive M4A custom-tag routing through AtomicParsley.
- ffmpeg rewrite before AtomicParsley and no executable `+use_metadata_tags` path.
- ALAC two-pass custom-tag convergence.
- M4A custom freeform survival through real loudgain/taglib.
- ReplayGain `-k` present/absent according to `prevent_clipping`.
- Shared track-only cleanup preserves track tags and removes album tags.
- Track-mode `SkipIfComplete` cleanup without a loudgain subprocess.
- Unsupported formats skip before tag parsing or subprocess launch.
- Unsupported-format table coverage and honest conversion-log output.
- Conservative unknown source/target depth handling.
- Verify/Compare/Preemphasis dirty-editor preservation.
- occupied password-prompt preservation.
- prompt close restoring the exact parked editor.
- overlap rejection and exactly-once terminal publication.
- stale same-path archive-repackage progress/result rejection by session ID.
- owned repackage completion refusing to replace a newer overlay.
- archive password-prompt slot ownership.
- stale deferred CTDB repair removal.
- OffsetCorrection Browse mutation classification.
- ReplayGain editor carrier-count replacement.
- DSF MusicBrainz/Picard alias value and carrier-count merging.

Static checks performed in this no-compiler environment:

- required baseline SHA-256 verification: 567/567 passed;
- all changed Rust files decoded as UTF-8 and contained no NUL bytes;
- no trailing whitespace in changed files;
- delimiter-aware Rust source scan: balanced parentheses, brackets, and braces in all 10 changed Rust files;
- no conflict markers;
- no remaining `verify_pending`, `compare_pending`, or `preemph_pending` references;
- every Verify/Compare/Preemphasis message construction and match includes `operation_id`;
- every archive-repackage progress/result construction and match includes `progress_session_id`;
- every `ActiveCompletionOperation` constructor supplies `batch`;
- shared ReplayGain builder/cleanup wiring checked at both callers;
- executable M4A metadata path checked for artwork-before-AtomicParsley ordering and absence of `+use_metadata_tags`;
- `git diff --no-index --check` reported no whitespace errors;
- final regenerated manifest verified in the working tree and again after extracting the delivery archive.

## Acceptance commands not executed here

The environment intentionally contains no Rust compiler, Cargo, rustfmt, network access, AtomicParsley, or loudgain. Therefore these acceptance gates were not claimed as run:

```sh
cargo test --workspace
TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix
cargo build --workspace
```

They remain the downstream apply-side gates. In the final corrective bundle, the exact strict command above contains and enforces the production-path single-file ALAC custom-tag, artwork, convergence, and loudgain-preservation test.

## Assumptions and decisions

- The supplied archive and its offline history are authoritative for all touched APIs.
- Lofty remains at the lockfile-pinned API already used by the removed pipeline-local cleanup implementation; the shared module reuses the same read/write calls rather than introducing an unverified API.
- AtomicParsley freeform behavior and ffmpeg 7.1 artwork incompatibility are taken from the brief's apply-side correction and existing invocation pin. This round preserves, rather than revisits, that decision.
- AtomicParsley is a hard dependency only for a non-empty non-native M4A/MP4 tag set. Silent loss was rejected as incompatible with an authoritative-tag guarantee.
- The taglib leg is not assumed safe solely from documentation. A real-tool preservation test is the authority; if downstream pinned taglib fails it, the test must drive a reorder/reapply correction before release.
- Unsupported ReplayGain containers are skipped rather than failed because ReplayGain is post-processing metadata, while the conversion log and warning make the degradation explicit.
- Unknown PCM depth on either side is treated as non-equivalence for inherited ReplayGain trust.
- Completion-operation IDs share the existing monotonic identity source used by the established authority framework.
- Archive-repackage path/staging equality is insufficient authority because a retry can reuse both; the `FileTaskProgressSession` identity is the operation discriminator.

## Cut list

### P1 status

No P1 cut remains in the final bundle. P1-5 was implemented in the round-4 corrective work through the centralized selection-aware renderer; format overlays, the template builder, generic prompts, and the vi command line are pinned.

### P2 cut

All P2 items were left unchanged except the directly related ReplayGain editor carrier-count refresh correction. Specifically not implemented:

- DSF Id3 artwork rollback geometry guard;
- legacy v2 journal sibling attribution tightening;
- legacy-journal batch serialization;
- preflight temp NotFound tolerance;
- tag/reserve 64 MiB cap;
- unmatched hashed-journal retention wording;
- Browse tree/header/archive context-menu corrections;
- quoted `:new-file` parsing;
- unified cue-album stored-value count/revert fixes;
- `mark_tag_entry_saved` misaligned-row original handling;
- narrow DetailEdit footer layout and production-pill geometry pin;
- custom inverse-palette contrast warning;
- oversized materializer tag-value transport/cap;
- archive materializer `disctotal` hint;
- `MetadataMutationReport::between` re-key/collapse behavior;
- config load-reconcile secret-reference propagation alignment.

### Explicit out-of-scope items preserved

- Companion-CUE behavior remains governed by the existing companion include list.
- DFF tag writing was not changed.
- Ambiguous-EAW terminal handling was not changed.

## Worst-case I/O cost statements

### M4A/MP4 metadata routing

For each routed artifact of size `S`:

- ffmpeg metadata rewrite: reads `O(S)` and writes `O(S)` to a replacement temporary file;
- artwork embedding, when enabled: tool-dependent in-place/rewrite behavior, conservatively `O(S)` read plus `O(S)` write;
- AtomicParsley freeform pass: conservatively `O(S)` read plus `O(S)` rewrite/replace.

Worst-case aggregate per artifact is three full-container read/write passes, `O(S)` space for each sequential temporary/replacement representation, and `O(S)` peak auxiliary disk usage because the passes are sequential rather than retaining all copies simultaneously. Across `N` artifacts, total bytes processed are `O(sum(S_i))` with a constant factor of up to three full passes. The payload-aware planner check itself performs no file I/O; it is `O(K + V)` CPU/memory over authoritative metadata keys and values.

### Pipeline ReplayGain scan

For paths with total encoded size `A` and decoded duration/sample volume `D`:

- loudgain reads/decodes every selected audio file: `O(A)` encoded input and `O(D)` signal processing;
- taglib writes are conservatively `O(A)` total if each container requires an in-place structure rewrite;
- track-only stale album-tag cleanup reads each output once and rewrites only files containing album tags, worst-case another `O(A)` read plus `O(A)` write.

The unsupported-format gate is `O(1)` over request fields and performs zero file reads, tag parses, or subprocess launches.

### Track-mode `SkipIfComplete`

Completeness inspection reads tag/container metadata for every output. Depending on container/parser behavior, worst-case input is `O(A)`. If all track tags are complete, stale album cleanup performs one additional read per file and rewrites only files carrying album tags; worst-case `O(A)` read plus `O(A)` write. No loudgain audio decode occurs on this path.

### Metadata-editor ReplayGain scan

- loudgain cost matches the pipeline scan: `O(A)` encoded reads and `O(D)` decode/analysis;
- track-only cleanup is worst-case `O(A)` read plus `O(A)` rewrite;
- metadata reread after processing is conservatively `O(A)` input, though normal tag readers consume only container/tag regions;
- editor model refresh is `O(F * R)` for `F` files and four ReplayGain rows, effectively linear in file count.

Failure during post-scan cleanup leaves the loudgain writes on disk and reports the cleanup failure; it does not claim the editor snapshot was refreshed.

### DSF alias canonicalization

No additional disk I/O was introduced. The already-read snapshot is transformed in memory. For `K` keys, `V` total values, and `C` carrier-count entries, canonicalization is `O(K + V + C)` plus ordered-map logarithmic factors, with `O(K + V)` additional memory. Sharing the map removes the second editor-side transformation/count lookup rather than adding a pass.

### Completion authority and prompt guards

No file or subprocess I/O was added. Begin, completion, publication, prompt-slot, and retirement checks are `O(log M)` map operations for `M` active completion families and otherwise `O(1)`. Result sorting remains the pre-existing behavior.

### Archive-repackage session authority

No archive I/O was added. Each progress or terminal message adds `O(1)` session-ID and context comparisons before any existing mutation. Stale messages now perform zero archive, staging, database, overlay, or status I/O/mutation.

### CTDB deferred-repair mismatch cleanup and OffsetCorrection classification

No new file I/O was added. The mismatch check reuses already materialized result pages and is `O(P)` over pages until the first matching path; clearing stale state is `O(1)`. OffsetCorrection classification is `O(1)`.

### ReplayGain editor carrier-count refresh

No new file I/O was added. Counts are rebuilt in memory in `O(F)` per refreshed ReplayGain row, with `O(F)` replacement storage.

### Tests

Hermetic tests use bounded temporary fixtures and stubbed subprocess transcripts. Real-tool M4A tests may perform multiple full ffmpeg/AtomicParsley/loudgain passes over small generated fixtures; their asymptotic costs match the production paths above and are bounded by test fixture size.

# Implementation Report: completion authority, M4A routing, and ReplayGain parity — corrective round 4

Date: 2026-07-18

## Delivery summary

This corrective round closes all four post-review release blockers in the round-3 bundle:

1. P1-5 is implemented for every text-input surface named by the brief: format-setting overlays, the template builder, generic prompts, and the vi command line. The same renderer was also applied to the bulk-rename template input because it had the identical hidden-selection behavior.
2. The prescribed strict command, `TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix`, now contains the production-path single-file ALAC/M4A custom-tag, artwork, two-pass convergence, and loudgain/taglib preservation test. It cannot pass by silently skipping that invariant when a required tool is absent.
3. The strict ALAC case begins with embedded artwork, lets the planner carry that artwork into the encoded output, asserts that the production Metadata stage ran, reapplies metadata to the same published file, and requires exactly one attached-picture stream and one `covr` atom after each pass and after loudgain.
4. The public source-less ReplayGain entry points no longer trust inherited tags without source sample-rate and bit-depth facts. They conservatively rescan; source-aware production calls may still trust a complete inherited set only when the source facts prove signal equivalence.

The prior round-3 P0/P1 implementation and both apply-side corrections remain intact. No dependency or cryptographic implementation was added.

## Baseline verification

The incoming `tonepoet_completion_authority_m4a_rg_round3.tar.gz` archive was extracted into a clean working directory and verified before edits:

```sh
cd tonepoet
sha256sum -c <(grep -v '^#' docs/handoff_manifest.txt)
```

Result: 569/569 manifest entries passed. No file was missing, so no scope reduction was required.

## Implemented corrections

### P1-5: selection-aware rendering for all remaining text inputs

- Generalized the existing inverse-video selection painter in `src/tui/inline_edit.rs` so callers provide their normal field style while the shared renderer owns UTF-8-safe scrolling, selection intersection, newline sanitization, and optional padding.
- Preserved each surface's pre-existing non-selected foreground/background instead of imposing one codec-field palette globally.
- Routed all format-overlay text fields through the shared painter:
  - FLAC compression;
  - AAC bitrate;
  - Opus bitrate and complexity;
  - MP3 VBR quality and bitrate;
  - WavPack hybrid bitrate;
  - SSRC attenuation, dither ID, and PDF type;
  - SoX bandwidth, phase, and all sinc numeric fields;
  - soxr cutoff and phase.
- Routed generic file/text prompts, the template-builder input, and the vi command line through the same selection-aware path.
- Routed the bulk-rename template input as an adjacent correction because it accepted the same selection/edit commands while providing no selection feedback.
- Retained terminal-cursor positioning through `TextInputState::view`; rendering and cursor placement now derive from the same visible width.
- Added value-asserting buffer/style tests for a partial `bcd` selection on format fields, generic prompts, the vi command line, and the template builder, plus focused-vs-unfocused behavior in the shared renderer.

### Strict acceptance target now enforces the new real-tool invariants

`tests/depth_format_matrix.rs` now contains `strict_gate_exercises_single_file_m4a_freeform_artwork_and_loudgain_invariants`.

The test uses the existing `require_tools_or_skip` policy. With `TONEPOET_REQUIRE_TOOLS=1`, absence of any of `ffmpeg`, `ffprobe`, `AtomicParsley`, or `loudgain` is a hard test failure. When present, the test:

1. creates a FLAC single-file source with native tags, `PRE_EMPHASIS`, `MY_NOTE`, and one embedded JPEG;
2. runs the complete production pipeline to ALAC/M4A with planner tag/artwork transfer enabled and ReplayGain temporarily disabled;
3. requires the pipeline outcome to be complete and the production Metadata stage outcome to be exactly `Ok`, proving the single-file custom-key route was not skipped;
4. verifies `PRE_EMPHASIS=1`, `MY_NOTE=keep me`, exactly one ffprobe `attached_pic` stream, and exactly one MP4 `ilst/covr` atom on the published output;
5. reapplies the metadata/freeform stage to that same published file and requires the complete tag map to converge exactly while artwork remains singular;
6. runs the source-aware pipeline ReplayGain stage through real loudgain/taglib;
7. reasserts both custom keys, singular artwork, and all four non-empty ReplayGain tags.

This places both previously optional library-test invariants inside the exact strict integration-test target named by the delivery contract.

### Source-less ReplayGain entry points fail closed

- `inherited_replaygain_tag_policy(None, ...)` now records unavailable source-rate facts for source-relative rate planning.
- Missing source facts also prevent proving source/target PCM-depth equivalence. The depth branch no longer treats `BitDepthTarget::Source` plus `source: None` as unchanged.
- Explicit target rates without source facts also conservatively require recomputation because equivalence cannot be established.
- Public wrapper documentation now states the invariant and directs production callers to the source-aware entry point.
- Existing tests whose purpose is to pin a trusted `SkipIfComplete` fast path now supply concrete source facts rather than relying on the source-less wrapper.
- Added an exact policy pin showing the same settings return `Trust` with source facts and `Recompute` without them.
- Added a behavioral pin showing the public source-less wrapper invokes loudgain once even when the artifact already has a complete inherited tag set.

## Files touched in this corrective round

- `src/convert/pipeline/stages.rs`
  - fail-closed source-less ReplayGain policy, wrapper documentation, and source-aware/source-less regression pins.
- `src/tui/inline_edit.rs`
  - generalized shared selection-aware renderer and focused/unfocused selection pin.
- `src/tui/draw_overlays.rs`
  - format-overlay, generic-prompt, vi-command-line, and bulk-rename routing plus render-buffer pins.
- `src/tui/template_builder.rs`
  - selection-aware template input and render-buffer pin.
- `tests/depth_format_matrix.rs`
  - strict production-path ALAC custom-tag/artwork/convergence/loudgain invariant and MP4 artwork inspection helpers.
- `docs/IMPLEMENTATION_REPORT_completion_authority_m4a_routing_rg_round3.md`
  - marked superseded for release and corrected the historical P1/strict-gate status.
- `docs/IMPLEMENTATION_REPORT_completion_authority_m4a_routing_rg_round4.md`
  - this report.
- `docs/handoff_manifest.txt`
  - regenerated after all changes.

## Assumptions and decisions

- The incoming round-3 tree and its manifest are authoritative for all APIs touched here.
- Selection is painted only on the focused input. Unfocused and disabled fields retain their prior palette and do not display an active selection.
- The inverse selection pair remains `theme.bg` on `theme.text_bright`, reusing the built-in contrast pins from the existing inline renderer.
- A source-less ReplayGain call cannot prove rate or depth equivalence. Recomputing is preferable to publishing inherited values whose signal provenance is unknown.
- The strict test uses ALAC because ffmpeg supplies the encoder while the target still exercises the MOV/M4A metadata path and AtomicParsley freeform atoms.
- `attached_pic == 1` and `ilst/covr == 1` are both required. The former pins the demux-visible representation; the latter independently rejects duplicate cover atoms in the container structure.
- The second metadata pass intentionally operates on the same published file rather than re-encoding from the source, so the test pins writer convergence rather than merely repeatable fresh conversion.

## Cut list

### P0 and P1

No P0 or P1 item remains cut in the final bundle.

### P2

No additional P2 item was taken in this corrective round. The round-3 P2 cut list remains unchanged except for the ReplayGain editor carrier-count correction already delivered there:

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

### Explicitly out of scope

- Companion-CUE behavior remains governed by the existing companion include list.
- DFF tag writing was not changed.
- Ambiguous-EAW terminal handling was not changed.

## Worst-case I/O and complexity statements

### Selection rendering paths

The UI changes perform no filesystem or subprocess I/O. For an input whose visible field width is `W` and whose underlying UTF-8 text length is `T`, each render obtains the existing scrolled view and scans at most the visible characters plus the prefix needed to calculate selection display columns. The existing `TextInputState` operations make the conservative CPU bound `O(T + W)` and allocate `O(W)` span/text data per rendered field. Terminal width bounds `W`; no persistent state or external I/O is added.

### Source-less ReplayGain behavior

The policy decision itself is `O(number of prepared tracks)` with no I/O when source facts are present and `O(1)` when absent. The behavior change affects a source-less `SkipIfComplete` call that previously trusted inherited tags: it may now execute the ordinary rescan path.

For artifacts with total encoded size `A` and decoded signal volume `D`, worst case is:

- loudgain reads/decodes `O(A)` / `O(D)` and updates tags;
- container/tag rewriting is conservatively `O(A)` read plus `O(A)` write;
- track-only stale album-tag cleanup, when applicable, may add another `O(A)` read plus `O(A)` write.

No new pass is added to source-aware production calls whose source facts already prove equivalence.

### Strict ALAC integration test

This is test-only I/O and does not affect production runtime:

- fixture JPEG and FLAC creation: `O(I + S)` writes, where `I` is artwork size and `S` is source-audio size;
- production ALAC conversion and publication: `O(S)` read and `O(M)` write for output size `M`;
- each metadata/freeform pass: ffmpeg and AtomicParsley are conservatively two sequential full-container read/write passes, `O(M)` each, with `O(M)` peak temporary disk space;
- each ffprobe inspection is conservatively `O(M)` read;
- each `covr` assertion reads the full M4A into memory, `O(M)` I/O and `O(M)` memory;
- loudgain reads/decodes `O(M)` / `O(D)` and may rewrite `O(M)` container data.

The test performs three complete state assertions (production metadata, second metadata pass, post-loudgain), so its conservative total remains `O(M)` asymptotically with a fixed number of full-file passes.

## Validation performed without a compiler

- Incoming round-3 manifest: 569/569 passed before edits.
- Every changed Rust file decoded as UTF-8 and contained no NUL bytes or merge-conflict markers.
- Delimiter-aware Rust lexical scans found balanced parentheses, brackets, and braces.
- `git diff --no-index --check` found no whitespace errors.
- No raw selection-blind `.view()` renderer remains in the specified format-overlay, template-builder, generic-prompt, or vi-command-line paths; the sole format helper use is the disabled-field display path, which cannot own an active selection.
- The strict fixture's ffmpeg artwork construction and the independent ffprobe/MP4 `covr` counting logic were exercised locally with the available ffmpeg/ffprobe tools and reported one attached picture and one `covr` atom.
- The final manifest is verified in the working tree and again from a fresh extraction of the delivered archive.

## Acceptance commands not executable here

This environment has no Cargo, rustc, rustfmt, AtomicParsley, or loudgain. Therefore no compilation or complete executable acceptance result is claimed. The downstream release gates remain:

```sh
cargo test --workspace
TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix
cargo build --workspace
```

The second command now mechanically includes the strict production-path M4A custom-tag, artwork, convergence, and loudgain-preservation test described above.

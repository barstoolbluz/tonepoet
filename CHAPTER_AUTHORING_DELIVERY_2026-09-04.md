# Chapter authoring delivery — 2026-09-04

This delivery starts from the supplied `tonepoet_bundle_chapter_authoring_2026-09-04` bundle (the corrected chapters-as-structure implementation) and implements `BRIEF_chapter_authoring_2026-09-04.md` without replacing the existing chapter/CUE conversion machinery.

## Implemented outcomes

- Added a Chapters surface to the existing Metadata Editor for one continuous program image.
- Existing CUE structure opens frame-native; embedded MP4-family chapter starts open sample-native; an unstructured file opens as one chapter at sample 0.
- Division points can be inserted, removed, retyped, or nudged.
- Pregap/interstitial starts can be authored by editing a chapter's pregap duration.
- Fixed-duration and uniform-count generation are available.
- Title-pattern generation is a separate action and reuses the existing `NumberingScheme` implementation (`N`, `NN`, `N/NN`, `NN/NN`).
- Individual titles remain editable and newline-separated bulk title paste is available.
- Clear-to-one-chapter is available without creating a second structure model.
- Invalid mid-edit geometry is shown in the Chapters surface and save is blocked until it is valid.
- Malformed embedded MP4-family structure has a repair path: readable start points are imported even when redundant end seams are invalid; an unreadable chapter table opens a blank repair map rather than making authoring impossible.
- Save destinations are format-aware:
  - sidecar CUE for any supported single program image;
  - MP4-family chapter entries for `.m4a`, `.m4b`, `.mp4`;
  - embedded `CUESHEET` for FLAC;
  - no inert in-file option for WAV;
  - split-on-next-conversion is exposed as conversion state, not as a second structure carrier.
- Chapterless `.m4b` is once again an ordinary one-track source. A true chapter-inspection error still fails closed so conversion cannot silently discard a chapter table it could not inspect.
- `tests/chunk2_orchestrator_contract.rs` is now behavioral. It no longer reads source files or asserts token placement/proximity. Non-observable source-placement rules are intentionally not recreated as disguised text tests.

## Structural model and coordinate rule

No parallel persistent division-point representation was added.

`CueAlbumTrackSource` remains the Metadata Editor's row carrier. Two optional exact sample-domain fields were added beside the existing CUE-frame fields:

- `index01_sample`: exact authored chapter start;
- `index00_sample`: exact authored pregap start.

The authority rule is explicit:

1. For an untouched CUE row, existing `index00_frames` / `index01_frames` remain authoritative and serialize exactly as read.
2. Once a boundary is authored from a sample/time input, its exact sample position is authoritative.
3. A CUE destination floors exact samples to the preceding 1/75-second CUE frame, because that loss of resolution is inherent in CUE.
4. MP4-family chapter entries retain the exact sample positions unless the user explicitly chooses to snap all destinations to the CUE grid.
5. If CUE is the only durable geometry, or the user explicitly selects global Snap, the clean editor state is canonicalized to the exact serialized CUE frames (`index00_frames` / `index01_frames`) and exact-sample overrides are cleared. It is never canonicalized through `frame -> floored sample -> frame`.
6. If sidecar CUE and MP4 chapters are saved together with Snap off, the live editor keeps its sample-native geometry. The sidecar is still recorded as the product's reopen authority, but that bookkeeping does not quantize the just-saved MP4 positions or make a title-only follow-up move them.

This lets MP4 authoring stay sample-accurate without inventing a new independent boundary collection, while legacy CUE material retains its native representation until edited.

## Corrective pass — reviewer findings

A focused corrective pass was applied after validation found two localized correctness defects. No chapter-model, transaction, conversion, or format-list redesign was added.

### CUE/sample reconciliation idempotency

- CUE canonicalization is now frame-native: it captures exactly the frames `cue_index00_frames` / `cue_index01_frames` serialize, stores those frame fields, and clears only the sample overrides. This prevents a second sample-to-frame floor at rates such as 32 kHz.
- A dual sidecar-CUE + embedded-MP4 save with global Snap off no longer quantizes the live sample-native geometry after success. A title-only second save therefore preserves the exact MP4 chapter position.
- Explicit global Snap still projects every selected destination to the CUE grid, but frame-native rows are projected directly from their existing CUE frame rather than through a floored display sample.
- The save-dialog/status CUE movement preview is source-aware: an untouched frame-native row reports zero movement.
- The misleading 44.1 kHz round-trip regression was replaced with 32 kHz coverage, including frame 7501, sample-native start 1000, and INDEX 00 pregap idempotency.

### MP4 ilst preservation across authoring remux

- `rewrite_embedded_chapters_for_authoring` snapshots the complete concrete Lofty `Ilst` before FFmpeg. It does not flatten metadata into Tonepoet's scalar field model.
- After the existing chapter remux and initial chapter verification, the saved ilst is restored onto the guarded rewrite file.
- Chapters are verified a second time after the Lofty ilst mutation; only then may the rewrite replace the staged input.
- Pure regression coverage preserves a scalar title, two ARTIST values, arbitrary `com.apple.iTunes:MY_NOTE`, and two `com.apple.iTunes:PERFORMER` values, and checks repeated restoration is idempotent.
- Tool-gated regressions exercise the real authoring FFmpeg rewrite twice and the combined new-sidecar + MP4 batch transaction when FFmpeg is available.

## Repair and validation behavior

The authoring validator derives every chapter duration from the next start and EOF. It requires:

- a positive source sample rate and duration;
- at least one chapter;
- first start at sample 0;
- strictly increasing starts inside the program;
- pregap start no later than its chapter start and no earlier than the previous chapter start.

CUE save performs a second projection validation so two valid sample-domain boundaries cannot silently collapse into one CUE frame, and a positive pregap cannot silently disappear when floored.

Malformed embedded chapter ends do not become a second source of geometry. Tonepoet imports the ordered starts, reports end/seam discrepancies, and rebuilds continuous durations from the next start/EOF on save.

## Save correctness and isolation

The save path deliberately distinguishes ordinary metadata writeback from structural authoring:

- Ordinary Album-view CUE metadata saves keep the established structure-preserving rewrite behavior.
- Chapter authoring explicitly opts into complete CUE geometry replacement.
- Existing sidecar replacement requires the exact raw bytes observed when the Chapters surface opened. A changed sidecar is refused before publication.
- Existing embedded FLAC `CUESHEET` replacement requires the exact editor-open CUESHEET payload already used by the metadata writer's optimistic-concurrency checks.
- A same-basename sidecar that physically exists but could not be parsed is treated as an existing repair target, not as a nonexistent create-only destination.
- MP4 chapter rewrite reuses the existing chapter serializer/remux/read-back verifier. The authoring-only rewrite snapshots and restores the complete concrete MP4 ilst around FFmpeg's structural remux, then re-verifies chapters after the ilst restoration. When a sidecar and MP4 chapter table are selected together, both are staged and committed through the existing metadata batch transaction.
- After a sidecar is written, the editor records it as authority under the product's existing `PreferSidecar` policy, preventing a later embedded-only save from being shadowed on reopen.
- A structural save is planned from a chapter-only projection of editor state. Chapter-owned per-track `TITLE`/`TRACKNUMBER` changes are included; unrelated unsaved metadata rows, deletions, and cleanup intents are restored/suppressed in the planning clone. Those unrelated edits remain dirty in the live editor for a later explicit metadata save.
- The generic Metadata Apply/OK path refuses to save around dirty chapter structure and directs the user through the Chapters save dialog so carrier selection cannot be bypassed.

## Performance / interaction choices

- Opening or navigating the Chapters surface performs no synchronous media probing.
- Source sample rate/count, embedded chapter inspection, sidecar snapshotting, and embedded-CUESHEET snapshotting run in the existing async/background pattern.
- EOF is established with Tonepoet's integer sample-count probe, not floating-point seconds multiplied by sample rate.
- Boundary edits and validation are in-memory integer operations.
- No new decode pass is added to conversion, and no change was made to the existing sample-accurate cutter.

## Files changed

- `src/convert/chapter_structure.rs`
- `src/convert/cue_parser.rs`
- `src/convert/pipeline/chapter_write.rs`
- `src/convert/pipeline/materializer_single.rs`
- `src/convert/processor.rs`
- `src/tui/app.rs`
- `src/tui/button_map.rs`
- `src/tui/chapter_authoring.rs` (new)
- `src/tui/draw_overlays.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/message.rs`
- `src/tui/metadata_autonumber.rs`
- `src/tui/mod.rs`
- `tests/chunk2_orchestrator_contract.rs`

No workspace manifest or lockfile was changed. `crates/tonepoet-true-peak` is byte-for-byte unchanged relative to the supplied bundle.

## Validation performed in this environment

The supplied environment has no `cargo`, `rustc`, `rustfmt`, `rust-analyzer`, or `nix`, and the brief explicitly assigns the Rust/Nix gate to the operator. Therefore **this delivery does not claim a compile, Rust test, rustfmt, or Nix build**. FFmpeg/ffprobe are present, which allowed validation of the tiny MP4 fixture and the remux inputs, but the new tool-gated Rust tests themselves cannot be executed without Rust.

Static/source checks performed after the final edit:

- `git diff --no-index --check` — no whitespace errors.
- Compared `Cargo.toml` and `Cargo.lock` to the supplied bundle — unchanged.
- Recursively compared `crates/tonepoet-true-peak` to the supplied bundle — unchanged.
- Searched `tonepoet-pipeline/src` for `std::process` / `tokio::process` — none.
- Audited newly added production diff lines — no non-ASCII user text, direct `Color::...`, F-key labels, `TODO`/`FIXME`, `unsafe`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, or `println!` additions. The two real-tool tests use `eprintln!` only to report an intentional skip when FFmpeg is unavailable, matching the repository's existing fixture-test convention.
- Checked modified tree for merge-conflict markers — none.
- Audited all `CueAlbumTrackSource` literals for the new sample fields and all actual `CueAlbumSyntheticSheet` literals for program sample-rate/length fields.
- Confirmed the rewritten `tests/chunk2_orchestrator_contract.rs` does not read or include source files; the only `include_str!` occurrence is the explanatory comment saying the file no longer uses it.
- Corrective source-code diff against the previous delivery contains only `src/tui/chapter_authoring.rs`, `src/tui/keybindings.rs`, `src/tui/draw_overlays.rs`, and `src/convert/pipeline/chapter_write.rs`; this handoff note is the only additional changed file.
- Audited structural CUE generation to confirm it already serializes through `cue_index00_frames` / `cue_index01_frames`; no duplicate projection path was added.
- Confirmed no stale call remains to the old sample-canonicalizing post-save helper and no UI preview directly applies `cue_floor_error_samples` to a frame-native row.

## Operator gate (must run in the repository's Nix environment)

Per `CLAUDE.md`:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo check
cargo test --workspace
nix build --extra-experimental-features 'nix-command flakes'
```

Do not substitute the host/system Rust toolchain for this gate.

The brief identifies these two existing low-rate flakes as outside this work's responsibility:

- `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20)
- `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31)

Any other failure should be treated as a real handoff blocker until explained.

## Focused runtime acceptance scenarios

In addition to the full workspace gate, these are the highest-value manual/real-tool checks:

1. **Chapterless M4B:** convert a chapterless `.m4b`; it must enter the ordinary one-track path rather than being refused.
2. **Existing chaptered M4B:** open Chapters, move one boundary, edit one title, save in-file, reopen, and verify exact positions/titles; then convert split and merged.
3. **Unstructured WAV:** open Chapters, generate several divisions, save sidecar CUE, reopen, and convert using that structure. No in-file destination should be offered.
4. **Unstructured FLAC:** author divisions and save both sidecar CUE and embedded CUESHEET; reopen and confirm sidecar authority under the existing preference.
5. **Malformed embedded MP4 chapters:** use a table with bad end seams but readable starts; confirm starts import with repair notes and a corrected save replaces the invalid table.
6. **Unreadable embedded chapter table:** confirm the authoring surface opens a blank repair map, and save requires replacing/superseding the current in-file authority rather than allowing a silent side path.
7. **Malformed/invalid physical sidecar CUE:** confirm a same-basename sidecar can be structurally replaced after opening, but a concurrent external change after editor-open is refused.
8. **Embedded FLAC CUESHEET conflict:** modify the embedded CUESHEET outside the editor after opening and verify structural save refuses the stale snapshot.
9. **Mixed dirty state:** change an unrelated album metadata field, then author chapters and save structure; confirm the unrelated metadata edit remains dirty and is not committed by the chapter save.
10. **CUE-grid loss/idempotency:** at 48 kHz author sample 1000. Sidecar + MP4 with Snap off must write CUE frame 1 and MP4 sample 1000, leave the live boundary at sample 1000, and a title-only second save must still emit MP4 sample 1000. With global Snap on, the live structure and MP4 projection must become sample 640 and remain 640. At 32 kHz, an untouched frame-native CUE boundary at frame 7501 must remain frame 7501 across save/reconcile/save, and a sample-native start at sample 1000 must canonicalize CUE-only to frame 2 and remain frame 2. Repeat the frame-native check for INDEX 00/pregaps.
11. **Pregap:** author a positive pregap, verify it survives a CUE carrier, and verify MP4-only save is refused unless a CUE carrier is also selected because MP4 chapter entries cannot represent CUE pregaps.
12. **Generation/title flows:** fixed duration, uniform count, title-only pattern generation, individual correction, bulk paste, insertion/removal, and clear-to-one-chapter over a 45+ chapter audiobook-scale list.
13. **Contract tests:** confirm `tests/chunk2_orchestrator_contract.rs` compiles and its scheduler/settings/album-accounting behavior tests pass without source-text inspection.
14. **MP4 ilst preservation:** seed an `.m4a`/`.m4b` with a scalar title, repeated ARTIST values, an arbitrary iTunes freeform atom, and repeated PERFORMER freeform values. Change only chapters, including the default Sidecar CUE + MP4 transaction, and verify the complete ilst logical contents/cardinality survive. Repeat the chapter-only save and verify they remain unchanged.

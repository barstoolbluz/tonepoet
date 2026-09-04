# Chapters as first-class structure - corrected implementation notes (R3)

Date: 2026-09-04
Correction lineage: starts from `tonepoet_bundle_chapters_as_structure_CORRECTED_R2_2026-09-04`, not from `main` or an earlier chapter implementation. The original brief's historical base remains `main` @ `d4d1d85`.

## Scope of this correction

R3 preserves the R2 architecture: embedded chapters bridge immediately into the existing `PreparedTrack` / CUE carrier machinery, the one-decode chapter splitter remains intact, and MP4-family embedded chapter write-back remains a guarded pre-publish remux with readback verification.

This correction changes only the requested behaviors:

1. intentional structured merge now gets a companion CUE by default, including FLAC/WAV and chapter-capable MP4-family outputs;
2. AAC + M4B is accepted and preserved end-to-end as an MP4-family output container;
3. R2's genuine one-chapter write-back correction is retained unchanged in semantics and extended to cover M4B.

No global source-format authority consolidation, new chapter/CUE model, UI/settings subsystem, or unrelated recovery machinery was added.

## Files changed from R2

- `src/convert/pipeline/chapter_write.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/types.rs`
- `tonepoet-pipeline/src/plan.rs`
- `tonepoet-pipeline/src/plugins.rs`
- `CHAPTERS_IMPLEMENTATION_NOTES_2026-09-04.md`

Root `Cargo.toml`, root `Cargo.lock`, and `crates/tonepoet-true-peak/` are unchanged.

## 1. Default CUE for intentional structured merge

### Policy

The existing `generate_cue` setting still controls ordinary **non-merged** CUE generation. It is not treated as an explicit opt-out for structural merge preservation.

For an `AudioArtifacts::Merged` result, pre-publish structural finalization generates a companion CUE when either:

- the prepared source contains more than one ordered track; or
- the source carries the existing explicit program-structure marker (including a genuine one-chapter embedded table or CUE-image structure).

A genuinely unstructured ordinary one-track source does not acquire a CUE. Split-export behavior is unchanged. A future explicit merge-CUE opt-out can be added as a separate policy input without reinterpreting the legacy `generate_cue` boolean.

Chapter-capable M4A/M4B/MP4 merges still receive embedded chapters as before; the CUE is an additional default structural companion. FLAC/WAV merges remain allowed and receive the CUE even though Tonepoet does not embed chapters in those containers.

### Metadata authority

`build_cue_sheet(...)` remains the renderer. The new merged-timeline entry point changes timing geometry only.

Album and track metadata are read exclusively from the already-prepared `PreparedSource` / `PreparedTrack` values. Those values have already passed through Tonepoet's configured aggregate metadata-source priority. The structural finalizer does not reread individual tags, sidecar CUEs, embedded CUEs, or embedded chapters and does not introduce a second precedence resolver.

Therefore merge policy decides that a sidecar exists; the existing metadata-authority pipeline decides what it says.

### Published-output boundaries

`MergedArtifact` now carries two optional facts that ordinary merge work may already know:

- exact per-track target-domain sample counts for PCM/lossless carriers;
- the merged target sample rate already measured during merge validation.

The structural finalizer resolves one `MergedProgramTimeline`, then feeds the same timeline to both embedded chapter serialization and merged CUE generation.

- **PCM/lossless multi-track fast path:** reuses exact target sample counts plus the already-measured merged sample rate. No new per-track or merged ffprobe calls are added by structural finalization.
- **Lossy/AAC path:** retains the existing target-domain probes of the encoded per-track carriers and merged output. The measured concat-clock delta (including AAC priming/padding effects) is assigned using the existing chapter-boundary logic rather than source-domain estimates.
- **CUE quantization:** measured sample seams are converted to 75 Hz CUE frames by flooring to the preceding representable frame, so quantization cannot move an index after the measured seam and drop presentation samples.
- **FILE reference:** merged CUE generation uses the actual planned published audio filename (`MergedArtifact.final_path`), not the internal staging filename.

The staged `album.cue` is deterministic. Re-finalization replaces the same logical CUE sidecar instead of accumulating duplicates.

## 2. AAC + M4B output authorities corrected

M4B is now treated as an AAC MP4-family output wherever that meaning is appropriate, without broadening ALAC and without enabling raw ADTS AAC.

### `tonepoet-pipeline/src/plan.rs`

- requested AAC container validation accepts `.m4a`, `.m4b`, and `.mp4`;
- `.m4b` survives `PlanContext` work-path derivation instead of normalizing to `.m4a`;
- raw `.aac` remains rejected;
- ALAC remains restricted to its existing `.m4a` / `.mp4` set.

### `tonepoet-pipeline/src/plugins.rs`

- AAC-family container validation accepts `.m4b`;
- M4B uses the existing MP4/iPod muxing path (`-f ipod`), never raw AAC;
- ALAC validation is unchanged.

The production `tonepoet-pipeline/src` remains a pure planner/command builder: no `std::process` or `tokio::process` use was introduced.

### `src/convert/pipeline/plan_bridge.rs`

- `.m4b` maps to planner AAC format alongside `.m4a` / `.mp4` (and the existing raw `.aac` classification, which is still rejected by output validation).

### `src/convert/pipeline/stages.rs`

AAC M4B is now consistently covered by the output-side MP4-family authorities for:

- final-container validation;
- default/final extension preservation;
- staged carrier extension preservation;
- post-encode metadata requirement and FFmpeg metadata rewrite allowlists;
- iTunes freeform metadata projection / AtomicParsley path;
- artwork rewrite and embedded-picture path;
- fixed-vocabulary/multivalue overlay and warning behavior;
- the focused real-output matrix case.

The existing chapter writer already recognized `.m4b` and selects the `ipod` muxer; that behavior is retained. `src/convert/formats.rs` already exposed AAC -> M4B and therefore did not need correction.

One cross-stage preservation issue was corrected as part of this audit: Tonepoet's metadata code explicitly documents that a later MOV remux strips AtomicParsley freeform atoms. Chapter write-back is such a later remux. After a structured MP4-family chapter rewrite, when the metadata stage was enabled, the finalizer therefore re-applies **only** the same authoritative terminal layers used by ordinary merged metadata handling: AtomicParsley freeform atoms followed by the preservation-aware in-process multivalue overlay. It does not rerun the primary FFmpeg metadata rewrite or artwork remux. The chapter table is read back again after these terminal mutations, so the published staged artifact is verified after its last metadata change.

Occurrences representing ALAC-only behavior or unrelated source/companion classification were deliberately left alone. In particular, this correction does not perform the out-of-scope repository-wide source-format-list consolidation.

## 3. Genuine one-chapter structure

R2 had already removed chapter-count-as-writeback-eligibility. R3 preserves that correction.

For a source carrying the structural marker:

- zero structural entries can still fail/skip as appropriate;
- one chapter serializes normally as one entry covering the complete measured merged timeline;
- N chapters serialize as N entries.

The same normal MP4-family writer handles one and N chapters; there is no special one-chapter writer. Deterministic coverage now exercises the one-entry renderer/muxer selection for both M4A and M4B, while a genuinely unstructured one-track source remains chapterless and sidecar-free.

## Tests and checks added

Deterministic Rust tests added or extended cover:

- pure planner accepts explicit AAC `.m4b` and preserves `.m4b` work paths;
- raw `.aac` remains rejected;
- ALAC `.m4b` remains rejected;
- FFmpeg AAC M4B command pins `-f ipod`;
- production final-container validation accepts AAC `.m4b`;
- default/final and staged AAC M4B extensions remain `.m4b`;
- planner bridge classifies M4B as AAC;
- M4B freeform metadata projection matches M4A/MP4 and targets the M4B artifact;
- MP4-family metadata, artwork, fixed-vocabulary, and multivalue test matrices include M4B where semantically appropriate;
- CUE real-output matrix includes AAC M4B;
- merged CUE target-timeline override ignores deliberately wrong source-domain timing and names the published audio file;
- sample -> CUE-frame quantization never advances past the measured seam;
- ordinary two-track FLAC **and WAV** merged programs generate a CUE without the legacy opt-in and without any structural-finalizer probes;
- repeat finalization does not duplicate/change that CUE;
- one real structural chapter merged to FLAC still gets a one-entry CUE;
- one embedded chapter remains serializable for M4A and M4B;
- unstructured one-track input remains non-structural.

## Validation actually run in this container

### Toolchain availability

Unavailable: `cargo`, `rustc`, `rustfmt`, `nix`, `AtomicParsley`.
Available and used: `/usr/bin/ffmpeg`, `/usr/bin/ffprobe`.

Accordingly, **no Rust compilation, `cargo test --workspace`, or Nix build is claimed**.

### Static/code-level validation

- `git diff --no-index --check` against the R2 starting tree produced no whitespace-error output (the no-index command returns 1 because the trees intentionally differ).
- all `MergedArtifact` initializers were audited for the new timeline fields;
- no `source_tracks.len() <= 1` / `entries.len() <= 1` chapter suppression remains;
- `tonepoet-pipeline/src` contains no `std::process` or `tokio::process` use;
- `crates/tonepoet-true-peak/` is byte-for-byte unchanged from R2;
- root `Cargo.toml` and `Cargo.lock` are unchanged.

### Real FFmpeg/ffprobe probes run for this correction

Using FFmpeg's available native AAC encoder (the container does not provide `libfdk_aac`):

- created AAC `.m4b` with explicit `-f ipod`; ffprobe identifies it as the MP4-family `mov,mp4,m4a,3gp,3g2,mj2` container;
- standard title/artist/album metadata and attached artwork survive M4B creation and chapter stream-copy rewrite;
- a chapterless M4B reads back with zero chapters;
- one-chapter M4B rewrite reads exactly one `Prologue` from sample 0 through the full 96,000-sample audio timeline;
- a second M4B rewrite still reads exactly one identical chapter and preserves artwork;
- the same one-chapter/rewrite check passes for MP4;
- two independently encoded AAC M4B carriers measure 48,000 and 96,000 samples, while stream-copy concat measures 145,024 samples: the 1,024-sample concat-clock delta is therefore real and is handled by the shared target-domain logic;
- the corresponding measured second CUE seam quantizes to `00:01:01` rather than a source-domain estimate;
- a two-chapter M4B rewrite and second rewrite both read back the same two chapter boundaries; MP4 clock quantization is 16 samples at 48 kHz in this probe.

AtomicParsley is absent, so a real freeform iTunes-atom round trip was **not** executed here. The deterministic metadata-path tests were added, and the operator test below remains required. The repository's configured FDK-AAC path also could not be run here because `libfdk_aac` is unavailable.

## Operator handoff

Run the normal repository gate in its Nix development shell:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo check
cargo test --workspace
nix build --extra-experimental-features 'nix-command flakes'
```

Then run focused real-tool conversions covering:

1. chaptered M4B -> split FLAC;
2. chaptered M4B -> merged FLAC + automatically generated CUE with the ordinary CUE-generation setting false;
3. ordinary CUE/structured album -> merged FLAC + automatically generated CUE;
4. chaptered M4B -> merged M4B with embedded chapters **and** sidecar CUE;
5. AAC -> M4B metadata, multivalue/freeform tags, and artwork preservation using the repository's actual encoder/tagger toolchain;
6. one-chapter M4B/MP4 -> one-chapter merged M4B/MP4 with full-timeline coverage;
7. repeat execution/finalization produces the same chapter table and one deterministic sidecar rather than accumulating structure.

The original brief identifies two pre-existing low-rate flakes as outside this work: `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20) and `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31).

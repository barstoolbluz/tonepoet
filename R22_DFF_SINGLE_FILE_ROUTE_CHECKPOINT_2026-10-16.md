# R22 DFF metadata-sidecar SingleFile regression — focused corrective checkpoint

Date: 2026-10-16  
Status: **source-audited corrective candidate; build-host acceptance pending**.

## Inputs and integrity

- Full repository input: `TonePoet_LOGGING_R22_dff_route_2026-10-16.tar.gz`; SHA-256 `60e25d5e58b02e5cd81172ceed8620ba67d842e48c75d2cc3a0580fd668cb8d3`.
- Original root `SHA256SUMS`: SHA-256 `3efdcb0bdc518c6c59b71a83eadbd884d68fc04406fa39e9fc6bda3d3b1ffbd4`; **all 1030/1030 original entries verified** before editing.
- Brief: `BRIEF_R22_dff_single_file_route_2026-10-16.md` (preserved in this repository).
- R21/R21C authority changes are accepted upstream; keep them unchanged. Prior host report: **7518 passed, 1 failed**. This is *not* an R22 test result.

## Root cause and acceptance interpretation

The failing `nine_dff_metadata_sidecar_album_drives_real_conversion_naming_and_flac_tags` test called `cue_artifact_commit_decision_for_path(..., &[], PreferSidecar)`.

Under the accepted R21C rule, an explicitly **empty** metadata-target list disables all aggregate targets. Only a **missing** setting receives the historical default during TOML deserialization. There is no implicit default inside the resolver (`normalized_aggregate_metadata_target_priority` preserves omissions). The test's `&[]` therefore selected no metadata representation and reached the preexisting fail-closed `EmbeddedOnly` sentinel; it was not exercising a default configuration as the brief presumed.

The actual default is `[SidecarCue, EmbeddedCue, IndividualFiles]`. For a valid one-track-per-file DFF sidecar metadata artifact, `SidecarCue` is viable, so the resolver returns `IgnoreCue` **for audio structure** and carries the exact sidecar track metadata into the existing `SingleFileMaterializer`. The CUE contributes names/tags; it does not define a CUE-image conversion route.

**Do not change production authority or re-introduce fallback defaults for explicit `[]`: doing so would invalidate R21C's authorization contract.**

## Corrections (test-only; no production changes)

1. `src/convert/pipeline/stages.rs`: the nine-DFF integration fixture obtains the actual serde default once and passes it into all nine queue decisions; exact `IgnoreCue` and sidecar provenance assertions remain, as do the real DFF-to-FLAC conversion, naming, tag, and album-gain checks.
2. `src/convert/queue_expansion.rs`: the existing priority/viability unit test now explicitly contrasts `&[]` (no sidecar transfer) with the configured default (sidecar transfer, `IgnoreCue`) for the same DFF-path metadata mapping.
3. `R22_DFF_SINGLE_FILE_ROUTE.patch`: replayable two-file unified patch. All production source files, build inputs, DSP/Reference source locks, dependency manifest, and R21C metadata-write guards remain byte-identical to input.

No routing-policy implementation, extra configuration knob, exception, fallback, tag writer, or dependency change is warranted by this failure.

## Validation performed in this session

- Original 1030-root-entry `SHA256SUMS` verification: **PASS** before changes.
- Exact input-vs-output archive member comparison: **PASS**; only the two named pre-existing test source files changed.
- Patch dry run (`patch -p1 --fuzz=0 --dry-run`) and replay equivalence: **PASS**.
- Focused static inspection of the test/default/authority flow: **PASS**.
- `Cargo.lock`, `src/config.rs`, `src/metadata_authority.rs`, `src/tui/app.rs`, `src/tui/keybindings.rs`, all original production files: **byte-identical**.
- Rust compilation, formatter, and tests: **NOT RUN**. `cargo`, `rustc`, `rustfmt`, and `nix` are unavailable in this runtime; network DNS access to `static.rust-lang.org` failed. Do **not** represent the suite as passed.

## Build-host acceptance — mandatory before release

Use the repository's prescribed pinned Nix development shell, with FFmpeg/FFprobe, FLAC encoder, and the requisite taggers available:

```bash
set -eu
sha256sum -c SHA256SUMS
cargo fmt --all -- --check
cargo test --locked cue_artifact_commit_decision_honors_priority_and_source_viability
TONEPOET_CUE_MATRIX_STRICT=1 cargo test --locked nine_dff_metadata_sidecar_album_drives_real_conversion_naming_and_flac_tags
cargo test --locked r21c_
cargo test --locked r21_
cargo test --locked --test reference_qualification_freshness
# Run the workspace gate as prescribed by CLAUDE.md; on the build host,
# use its long-running execution procedure rather than a short foreground timeout.
cargo test --locked --workspace --no-fail-fast --exclude tonepoet-true-peak
```

Do not claim a fully green gate until the nine-DFF integration test gets past its old first assertion and validates actual conversion/naming/FLAC tags. If any later failure is observed, capture its exact evidence, adjust narrowly, and update this checkpoint and checksums. No tolerance changes or routing relaxations are authorized by this brief.

## Deferred real-world acceptance (unchanged scope)

- R5: five UI changes remain uninspected by the user.
- #75: physical SACD tag-transfer round trip on a real disc still outstanding.
- #62: reopen the real ten-track album with its 2.19 GB RF64 carrier.
- #73: popup-body Markdown intentionally deferred.

## Resume procedure

Verify the packaged root `SHA256SUMS`, read this checkpoint and the R22 brief, apply no changes unless build-host evidence identifies a concrete failure, and continue the acceptance commands above. Source-limited corrective patch and original R21C checkpoints are included. Archive-level SHA-256 is recorded in the delivery response; validate it before unpacking whenever available.

# TonePoet R33C — narrowly scoped corrective handoff (2026-10-11)

## Authority and changes

Input: the full R33 engineering bundle (`TonePoet_R33_engineering_bundle_2026-10-11.tar.gz`, SHA-256 `489fa29e4118d41fc7d754b41db677064017a1ebd520d86f1c4cb1b4984c0d58`). This correction implements the user's two demonstrated defects, not a new metadata subsystem. In the full R33C tree **only two Rust source files differ from R33**.

1. **Issue #76: sidecar authority owns its edits.** In `src/tui/keybindings.rs`, ordinary Album-view Save with resolved `SidecarCue` now passes the original editor entries and deletions through the *same existing CUE generator* used by explicit sidecar-only save. Remove the old native-`DISCNUMBER` staging loop and the helper that hid that row from the sidecar. The planner no longer admits or stages FLAC writes for a `SidecarCue` side; existing native `IndividualFiles` and `EmbeddedCue` routing is untouched. The existing Tonepoet-extended-metadata consent gate now includes side-scoped `DISCNUMBER` for ordinary Save as it does for explicit sidecar-only Save; no consent bypass is retained. The special sidecar/native exemption in the unsupported-field check is removed because the extended CUE representation already supports this key.

2. **R30 regression test integration:** In `src/tui/command.rs`, `r30_set_unsigned_headroom_supports_pcm_and_dsd_without_losing_target_authority` now admits DSD target `0.0001` (`DbNano(-100_000)`) instead of an R33-disallowed nine-decimal value, asserts that `0.000000001` is rejected by the parser without changing the selected target or user provenance, and preserves the PCM and out-of-range checks. There is **no production true-peak change**.

No Rust files outside these two were modified. No SACD transfer, DSP, solver, Reference qualification, format-routing, or data model changes were made. R33's 0.0001 dBTP input grid and the qualified ceiling-as-upper-bound semantics are preserved; this is not a claim of exact delivered peak equality.

## Focused regression coverage

`r33c_album_view_discnumber_follows_selected_carrier_authority` replaces the old test that required native writes under `SidecarCue`. It constructs two real FLAC images with sidecar CUEs, saves `DISCNUMBER=1/2` under sidecar-first authority, checks the save plan has **zero audio writes**, asserts byte-for-byte identity of both FLACs, checks each CUE's track-specific extended DISCNUMBER, reopens and verifies both side values, deletes them and verifies sidecar deletion without audio mutation. The same physical files are then opened individual-files-first: the plan must contain native writes and no sidecar writes, the CUEs remain byte-identical, the FLACs change and carry their expected native DISCNUMBER values, and reopening restores both side values.

Two additional pre-existing unit tests now assert that: (a) multi-file sidecar album partitions serialize the per-side numbers into the corresponding CUE tracks rather than staging native writes; (b) a non-writable audio tag format does not block a sidecar-authoritative save. The test also checks that ordinary Save retains the explicit CUE extension consent path.

## Validation boundaries

**Passed in this execution environment**: original R33 archive checksum, Rust lexical delimiter checks on the two modified files, shell syntax check, targeted source assertions, R33C patch applying and reversing against R33 sources *byte-for-byte with zero fuzz*, archive extraction and checksum manifest verification (see `R33C_STATIC_AUDIT.txt`).

**Not run / not claimed**: `cargo test`, Rust compilation, `cargo fmt`, a real end-to-end metadata Save, SACD transfer, full workspace acceptance or source-locked Reference requalification. The environment has no Cargo, rustc, rustfmt or Nix. The compiled assertions above are *tests added*, not reports of passing runtime tests. Real FLAC generation in the test requires `ffmpeg` on the build host.

## Build-host acceptance

From the delivered `tonepoet/` root, with the supported Rust toolchain, FFmpeg, qualified Reference prerequisites and a **real** SACD ISO:

```bash
R33C_CHECK_FORMAT=1 bash ./R33C_ACCEPTANCE.sh /absolute/path/to/real.sacd.iso
```

This executes the corrected R30 test, both R33C-focused tests, the existing DISCNUMBER editing regression, the R33/R32/R31/R30 chains, and full Reference requalification and workspace gate via the existing `scripts/requalify_reference_r20.sh`. Run `cargo fmt --all` only if needed; repeat acceptance and requalification after any formatting or source modification, especially locked `stages.rs` / `settings.rs` changes.

The application of this bundle must not be represented as production-ready until those host gates pass. No source-locked override should be used. The existing R33 historical handoff/checkpoint describes the *pre-corrective* candidate; this R33C handoff supersedes its sidecar-authority caveat.

## Paths / application

* `R33C_FROM_R33_SOURCE.patch`: apply to the original R33 source root with `patch -p1 --fuzz=0 < R33C_FROM_R33_SOURCE.patch`. Only patches the two Rust files; distribute the acceptance runner separately if using patch-only transfer.
* `R33C_ACCEPTANCE.sh`: focused + chained test runner.
* `R33C_RESUME_CHECKPOINT_2026-10-11.md`: resumable checkpoints and open qualification.
* `R33_SOURCE_SHA256SUMS.txt`: regenerated integrity manifest for the **corrected** source tree (the earlier R33 version is in the preserved original R33 archive).

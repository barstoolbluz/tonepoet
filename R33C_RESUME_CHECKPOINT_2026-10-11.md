# R33C resume checkpoint — 2026-10-11

## Read first

- Base source: `TonePoet_R33_engineering_bundle_2026-10-11.tar.gz` (SHA-256 `489fa29e4118d41fc7d754b41db677064017a1ebd520d86f1c4cb1b4984c0d58`). The user-supplied corrective audit identifies two real defects and explicitly excludes subsystem redesign.
- This repository is the corrected **R33C candidate**, built by applying `R33C_FROM_R33_SOURCE.patch` to the two original R33 Rust files. `R33C_DELIVERY_NOTES_2026-10-11.md` is the current handoff; `R33_DELIVERY_NOTES_2026-10-11.md` documents an earlier pre-corrective revision.

## Completed

1. `src/tui/keybindings.rs`: ordinary SidecarCue authority now generates and writes only the sidecar, with DISCNUMBER in the existing per-track Tonepoet extension; removed native sidecar-mode projection and ignored write-eligibility dependency; restored the normal consent path. IndividualFiles/EmbeddedCue native planning is unchanged.
2. `src/tui/command.rs`: correct R30 DSD target expectation to 0.0001/DbNano(-100_000); rejects 0.000000001 and retains value + User provenance; PCM and out-of-range expectations preserved.
3. Updated three keybindings unit regressions, including a real-FLAC end-to-end save/reopen/deletion test with both configured authority orders. Tests require compilation and FFmpeg before they can be counted as passing.
4. Created `R33C_ACCEPTANCE.sh`, delivery notes, patched-source diff, and regenerated source checksum manifest. Static lexical balancing, shell syntax, zero-fuzz apply/reverse reproduction and manifest verification are available as independently reproducible evidence.

## Open build-host gates (do not silently waive)

- `cargo fmt --all -- --check` and Rust build/compiler diagnostics.
- `cargo test --locked --lib r30_set_unsigned_headroom_supports_pcm_and_dsd_without_losing_target_authority`.
- `cargo test --locked --lib r33c_` (especially actual audio/CUE byte identities and reopen).
- `cargo test --locked --lib r30_` and `cargo test --locked --lib r33_`.
- Full `R33C_ACCEPTANCE.sh` chain, workspace suite and requalification against the current source-locked Reference sidecars using a real SACD ISO.
- Any nonzero result is a concrete blocker to investigate; do not expand the architecture in anticipation.

## Resume procedure

1. `cd tonepoet && sha256sum -c R33_SOURCE_SHA256SUMS.txt`; the corrected source manifest should have no failures. Verify downloadable R33C archive against its published hash as well.
2. Read `R33C_DELIVERY_NOTES_2026-10-11.md`, inspect `R33C_FROM_R33_SOURCE.patch` (two Rust files), and run `bash -n R33C_ACCEPTANCE.sh`.
3. On the configured build host, run `R33C_CHECK_FORMAT=1 bash R33C_ACCEPTANCE.sh /absolute/path/to/real.sacd.iso`, record results, and requalify after any source changes.
4. Scope remains only the two demonstrated defects. Do not rebuild metadata authority, SACD transport, true-peak solver, or concurrency infrastructure without a newly demonstrated failure.

## Integrity

The R33C patch applies and reverses against the supplied R33 Rust files without fuzz; per-file `cmp` confirmed byte-for-byte reproduction. `R33_SOURCE_SHA256SUMS.txt` is regenerated after the corrections (not a statement about the old R33 tree). Toolchain-based acceptance is **not verified** in this environment.

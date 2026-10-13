# TonePoet R36 implementation checkpoint — 2026-10-12

## State

R36 is a *test-only* correction based on the attached R35 source snapshot and the R36 brief. Implementation, source patch reproduction, source inventory comparison, and offline verification are complete. **Cargo tests and SACD Reference requalification are outstanding** because the build host/toolchain and real SACD ISO were not available here.

## Exact authority and change set

- Brief: `BRIEF_LOGGING_R36_core_editor_row_2026-10-12.md`.
- Supplied source archive SHA-256: `d724434c87b578e5b79206572af50ecb7f41e59cc0c9a18559301e07aabacf50`.
- Changed pre-existing file: `src/tui/keybindings.rs` **only**, inside `r33c_album_view_discnumber_follows_selected_carrier_authority`.
- R35 input `keybindings.rs` SHA-256: `37224d4440b5b2123bfe81d1a273ade440cb494ebce8ef89c6bde7b1b5d90a23`.
- R36 corrected `keybindings.rs` SHA-256: `3bdb8b8a1f384bcd2dbce9e0310e2a3335066e2d78643524ff91d7717d1a8120`.
- `R36_FROM_R35.patch` SHA-256: `e18345624e97c472e0ebd9bca9203b7dd93d0471afd7be2ceba1cfcaa2da861c`.
- `R36_SOURCE_SHA256SUMS.txt` inventories every regular file in the R36 delivered tree except itself. Symlink: `crates/dvda-demuxer/tests/fixtures` -> `../../../tests/fixtures`, retained from R35.

## Technical finding

The native File tags presentation intentionally contains an empty synthesized `DISCNUMBER` core row. The old R35 test's `no DISCNUMBER` assertion and subsequent Add Key interaction were invalid. R36 checks that the row has *zero stored values* on both FLACs, edits the existing row through the editor's inline route, and preserves all downstream production Save/readback/byte-identity assertions. Do not remove the injected row or change production tag handling just to satisfy the old test.

## Resume procedure on build host

1. Extract the **full R36 archive** and `cd tonepoet`; do **not** apply `R36_FROM_R35.patch` to this already-corrected source tree.
2. `sha256sum -c R36_SOURCE_SHA256SUMS.txt` (expect no failures).
3. `cargo test --locked --lib r33c_album_view_discnumber_follows_selected_carrier_authority` (the previously failing isolated test).
4. `bash ./R36_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso` on a configured Rust/audio host to run all preserved focused gates, actual Reference requalification, and full workspace acceptance. `R36_CHECK_FORMAT=1` optionally includes `cargo fmt --all -- --check`.
5. If the Reference source-lock freshness issue reported upstream remains, requalify via the existing R30->`scripts/requalify_reference_r20.sh` chain; do not mask this failure, fabricate a lock, or enable an unqualified override. Source-lock artifacts can be rewritten by this procedure.
6. If any failure remains in the focused regression, investigate the failing line and runtime trace rather than broadening architecture or presuming a new production defect. If code/evidence changes, create a successor checkpoint, updated patch, checksums, and clean archive.

## Applying just the patch to a clean R35 input

From the original, *unmodified* R35 tree root:

```bash
sha256sum -c R35_SOURCE_SHA256SUMS.txt
git apply --check R36_FROM_R35.patch
git apply R36_FROM_R35.patch
```

The patch is intentionally non-idempotent: applying it twice is refused rather than silently repeating or damaging a change. On an already-corrected R36 tree, `git apply --reverse --check R36_FROM_R35.patch` succeeds. The full R36 bundle is the preferred handoff.

## Scope and outstanding work

No production paths changed. The R35 input inventory was 972/972 good; patch replay/reversal verified. No Cargo/rustc installed locally, so **compiled acceptance remains unverified**. An upstream report of 7,457 passed/2 failed is reported evidence, *not* a new gate result from this delivery. The second known failure was source-lock freshness and remains dependent on physical-SACD requalification.

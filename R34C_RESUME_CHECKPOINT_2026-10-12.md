# TonePoet R34C — corrective handoff and resume checkpoint

Date: 2026-10-12
Disposition: **ONE SOURCE DEFECT CORRECTED; COMPILED AND PHYSICAL ACCEPTANCE OPEN**.

## Provenance

- Corrected parent source bundle: `TonePoet_R34_engineering_bundle_2026-10-12.tar.gz`, SHA-256 `8bd36f3a633ec97dbd903b01226cfad7cb178c6880282cdcc695c593073c8a87`.
- The earlier uncorrected R33C source input to R34: `TonePoet_LOGGING_R34_five_regressions_2026-10-12.tar.gz`, SHA-256 `440fd00ab05564fbfa3ed3528c64a5c5908ce39d531a5ca8835682c0581f7c79`.
- R34C source delta: `R34C_FROM_R34_SOURCE.patch`, SHA-256 `928ac9e677a37f133d98833302a0bf98ad9e1a0a1231fd140908270ab674cc71`.
- Source bundle is complete and does **not** require applying patches. Use the delta patch only against the *already corrected R34* source, not against the R33C input or the R34C tree.
- The existing `R34_FROM_R33C_SOURCE.patch` is intentionally unchanged and still represents the preceding step.

## Source-level correction (one Rust file)

`src/tui/keybindings.rs` — `save_sacd_sidecar` (multitab branch only):

- Read stereo and multichannel physical track counts from the presentation tabs constructed from the ISO TOC.
- Use the existing `tag_interchange::sacd_transfer_area_track_ids` for both areas, matching the SACD editor's read projection and strict tag-transfer mapping.
- No reliance on the first XML `<track>` record's `TOTALTRACKS` field for the multitab write boundary. XML track rows may be out of order, and `TOTALTRACKS` is area-local.
- Keep the existing `apply_sacd_entries_to_sidecar` and sidecar writer. The mapper's errors still prevent publication of an incompletely mapped sidecar.
- Leave all single-tab mirroring, single-area multichannel fallback, parser, models, metadata authority, CUE handling, tag transfer, picker, DSP, and source-locked reference material untouched.

## Regression coverage

New test: `r34c_multitab_save_uses_iso_area_counts_for_out_of_order_xml` (in the same Rust source file).

It constructs a five-track hybrid SACD with three stereo tracks and two multichannel tracks. XML order is `[4, 5, 1, 2, 3]`; the first records are MCH rows with `TOTALTRACKS=2` while stereo rows have `TOTALTRACKS=3`. It checks the initial stereo and MCH read projection, changes a different title on each tab, saves, checks all five titles and preserved foreign and album metadata, and rebuilds the editor from the saved XML to check both areas again.

The prior R34 test `save_sacd_sidecar_writes_multi_tab_areas_without_implicit_mirror`, the hybrid MCH read regression, and the single-area multichannel save regression were not altered. The original five R33/R33C regression fixes remain as R34 shipped.

## Verification actually performed here

- Verified the exact parent R34 archive checksum and ran its `R34_OFFLINE_VERIFY.sh` successfully *before modification*.
- Independently extracted that published R34 archive; applied `R34C_FROM_R34_SOURCE.patch` using `patch -p1 --fuzz=0`; byte-compared patched `keybindings.rs` with the corrected tree; reversed it with `patch -R -p1 --fuzz=0`; byte-compared the restored file against the R34 source.
- Compared `src/` trees: **only `src/tui/keybindings.rs` differs from R34**.
- Verified test XML is well-formed and reproduces the old incorrect 2/3 split versus TOC-authoritative 3/2 (this is fixture/source-level validation, not Rust execution).
- Pygments Rust lexical pass, delimiter balance, focused source-path assertions, `git diff --check`, and shell syntax checks passed. These do **not** establish Rust type-checking or test success.
- Rebuilt the tree checksum manifest (`R34_SOURCE_SHA256SUMS.txt`) for the R34C tree, retaining its filename so `R34_OFFLINE_VERIFY.sh` remains useful. `R34C_OFFLINE_VERIFY.sh` chains it.
- Built the complete source archive without local `.git` metadata and checked a fresh extraction against the manifest.

## Build-host acceptance — not yet run

Rust/Cargo/Nix and a physical SACD ISO are unavailable in this authoring environment. **No compiled acceptance, end-to-end SACD test, or physical disc qualification is claimed.**

From the extracted `tonepoet/` directory on a configured build host:

```bash
bash ./R34C_OFFLINE_VERIFY.sh
# Focused new test without requiring a physical disc:
cargo test --locked --lib r34c_multitab_save_uses_iso_area_counts_for_out_of_order_xml
# Full production/test-target compile + original R34/R33C test and Reference chain:
bash ./R34C_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
```

`R34C_ACCEPTANCE.sh` begins with `cargo check --locked --workspace --all-targets`, runs the new test, and chains `R34_ACCEPTANCE.sh` (which covers the reported SACD, CUE, authority and transfer-filter tests, all `r34_` tests, R33C/R33 and Reference requalification). It inherits the real SACD ISO and no-unqualified-Reference-override requirements.

On a physical SACD, manually inspect both area selections and verify independent XML edits survive Save and reopen, without altering unselected/foreign fields. Keep pre-existing contention-only scanner flakiness and Reference source-lock freshness qualification separately tracked; do not mark them resolved by this source correction.

## Resume rule

Do not broaden the fix unless a **new, reproducible defect** is observed. Any changes after this checkpoint require rerunning the applicable gates, regenerating `R34C_FROM_R34_SOURCE.patch`, `R34_SOURCE_SHA256SUMS.txt`, and delivery checksums. Only one Rust file is intentionally changed relative to R34; performance characteristics and the audio pipeline are unchanged.

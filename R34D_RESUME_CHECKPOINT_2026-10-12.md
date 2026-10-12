# TonePoet R34D — SACD TRACKNUMBER repair fallback (2026-10-12)

## Disposition

**ONE NARROW SOURCE CORRECTION; BUILD-HOST AND REAL-DISC ACCEPTANCE OPEN.**

R34C is the sole parent. Published parent source:
`TonePoet_R34C_HYBRID_SACD_SAVE_CORRECTIVE_2026-10-12.tar.gz`
SHA-256 `48c0e53631beae3b714ee75972c4db8bdbfa1bd72815834ffa572e8c6deefc69`.
R34D is a complete, independently extractable source bundle. The source-only
patch `R34D_FROM_R34C_SOURCE.patch` applies **only** to the published R34C tree,
not R34, R33C, or an already patched R34D tree.

## Genuine defect and corrective scope

R34C's multitab `save_sacd_sidecar()` uses the strict
`tag_interchange::sacd_transfer_area_track_ids()` mapper. The strict mapping
correctly handles out-of-order XML and unequal SACD area track counts, but
refuses duplicate/missing/invalid stored `TRACKNUMBER` values. A user can Save
such ordinals from a valid state and reopen them in the editor, but the next
Save cannot repair them: the editor falls back to
`SidecarMetadata::tracks_for_area()` when numbering is invalid.

**Only `src/tui/keybindings.rs` is changed.** Save still tries the strict
TOC-authoritative mapper first. On strict failure, Save may use the editor's
pre-existing `tracks_for_area(area_idx)` ordering **only when** those fallback
IDs match, exactly and uniquely, the physical ID interval derived from ISO
TOC tab lengths. Stereo expects `1..=stereo_count`; MCH expects
`stereo_count+1..=stereo_count+mch_count`. Overflow and `u32` conversion are
checked, and empty, incomplete, duplicated, or out-of-area IDs cause the
original strict error to propagate before XML publication.

This avoids error-string matching. With a complete physical ID set, the
existing strict mapper's remaining failures concern stored `TRACKNUMBER`
ordinals. Critically, Save uses the **same fallback ordering the editor used**;
sorting only the *copy* needed to establish unique ID-set equivalence avoids
misdirecting track titles and other metadata.

**Unchanged:** strict tag-transfer mapper, sidecar parser/serialization,
read-side builder, metadata authority, transfer picker, DSP, legacy single-tab
mirror, and R34C's unequal-count/out-of-order XML mapping.

## Focused regression

`r34d_multitab_save_can_repair_duplicate_track_numbers_without_cross_area_writes`
in `src/tui/keybindings.rs`:

1. Open a synthetic 2-stereo/2-MCH XML metabase with IDs 1–4 and normal
   `TRACKNUMBER` `01`, `02` in each area.
2. Edit stereo ID 2 to `TRACKNUMBER=01`, Save, and re-read duplicated values;
   show strict tag transfer still rejects them.
3. Reopen the editor (legacy read fallback), repair stereo ID 2 to `02`, edit
   stereo ID 2's title and independently edit MCH ID 3's title, then Save.
4. Re-read and reopen both areas. Verify the correct titles, exact ordinals,
   and unchanged, independent stereo/MCH ALBUM values.
5. Remove physical stereo track ID 2 in a negative XML fixture, attempt the
   same Save, and confirm refusal with the file unchanged.

R34C's `r34c_multitab_save_uses_iso_area_counts_for_out_of_order_xml`, the R34
single-area MCH regression, and earlier multitab Save tests remain unchanged.

## Verification already performed / remaining

- Baseline R34C archive checksum and the embedded SHA-256 inventory verified.
- R34C's offline integrity/shell gate passed **before** edits.
- The correction targets one production Rust source file; no other existing
  source was modified. A one-file patch and repeatable acceptance wrappers are
  included in R34D.
- See `R34D_VERIFICATION_REPORT.txt` for source/static, patch replay, archive
  integrity, and offline-check results obtained when packaging.
- This environment has no Rust compiler, Cargo, Nix, or real SACD ISO.
  **No compiled tests, functional runtime success, full-workspace build, or
  physical SACD / Reference qualification is claimed.**

## Build-host acceptance commands

From the extracted `tonepoet/` directory:

```bash
bash ./R34D_OFFLINE_VERIFY.sh
cargo test --locked --lib r34d_multitab_save_can_repair_duplicate_track_numbers_without_cross_area_writes
cargo test --locked --lib r34c_multitab_save_uses_iso_area_counts_for_out_of_order_xml
bash ./R34D_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso
```

`R34D_ACCEPTANCE.sh` runs the new regression, then chains the existing
`R34C_ACCEPTANCE.sh` script, including `cargo check --locked --workspace
--all-targets`, prior R34/R33C tests and source-locked Reference requalification.
A real ISO and actual playback-metadata inspection are still required. The
unqualified Reference override must not be enabled.

## Resume rule

No redesign or further hardening without a new reproducible defect. If edits
occur later, regenerate the one-file patch, tree checksum inventory, checks,
and delivery archive hash; do not claim static audits establish compile success.

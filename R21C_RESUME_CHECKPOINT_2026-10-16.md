# R21C — dedicated-sidecar native-write authority corrective

Date: 2026-10-16. This checkpoint supersedes **only the dedicated-sidecar secondary-write gap** in `R21_RESUME_CHECKPOINT_2026-10-16.md`. The R21 fixes remain included unchanged.

## Baseline and obligation

- Input: `TonePoet_R21_DITHER_METADATA_AUTHORITY_FULL_REPO_2026-10-16.tar.gz`, SHA-256 `a575d77b1f27cb571c1f1e6b01ad6faafff693f8e2b5b9a0940af817254afbbb`.
- Defect: `aggregate_metadata_target_priority = ["sidecar-cue"]` admitted a selected sidecar editor, but its secondary one-track-per-file ownership classifier still considered native FLAC tags writable solely because they were taggable. An edited native-origin COMPOSER could therefore be written to FLAC during ordinary Save.
- Desired policy: **every** actual destination must be listed in the configured target priority. A sidecar-only COMPOSER edit should use the existing 1:1 CUE track-metadata / Tonepoet REM projection and leave all FLAC bytes unchanged. When a field cannot be represented by any admitted destination, refuse before writing. Preserve the existing native file path when `individual-files` is also configured, and preserve embedded-CUE behavior.

## Corrective implementation (two source files)

1. `src/tui/app.rs`: `PresentationTab.native_file_tags_authorized_for_save` stores the live IndividualFiles admission **for the active Save only**. The field defaults to `true` for legacy direct-model paths, is retained by `PresentationTab::from_editor_state`, and is read by `taggable_metadata_sidecar_dual_authority_for_reduction`. A completed CUE-only Save consequently consumes the correct ownership and clears dirt without alleging a native-file save.
2. `src/tui/keybindings.rs`: each ordinary Save refreshes that permission from `app.config.conversion.aggregate_metadata_target_priority` after any selected-source tab switch. The existing dedicated-sidecar classifier uses that permission when classifying rows, generating CUE metadata, snapshotting native tags, and constructing the required-audio set. This makes native-origin, file-scoped COMPOSER sidecar-owned under `[SidecarCue]`, preserving its original file->track mapping through the existing `cue_album_one_track_per_file_slot_map` and REM serializer. Sidecar-only saves reject any residual planned native audio mutations (including forced deletes) **before** dispatch. The check is deliberately scoped to dedicated sidecar saves; writing an embedded CUESHEET within an explicitly authorized `EmbeddedCue` carrier remains legal. Unsupported edits with no admitted carrier must refuse rather than proceeding through the opt-in partial-revert path.
3. No new persistence mechanism, transaction layer, writer, configuration knob, DSP change, RF64 change, or Reference source-lock change.

## New integration regression

`r21c_sidecar_only_native_composer_save_never_writes_flac_tags` reuses `create_presplit_metadata_sidecar_fixture` (three real tagged FLAC files, one metadata sidecar CUE, nontrivial file/track ordering):

- Configures **only** `SidecarCue`; edits the native-origin `COMPOSER` entry for file 01, which is CUE track 02, using its natural `RowScope::File` representation (no synthetic scope conversion).
- Runs the ordinary async Save with the existing `CueTonepoetMetadataPreference::Always` permission to represent nonstandard fields in readable REM metadata.
- Requires the planned writeback to be sidecar-only, with no required audio paths and no native write results; requires all three FLACs to remain **byte-identical**.
- Requires the parsed saved sidecar's track 02 `COMPOSER` to equal the edit; reopens the metadata editor and checks that the value is visible there.
- Requires save-completion reduction to clear dirt and a repeat Save to report `No changes to save`.

Existing `presplit_metadata_sidecar_keeps_non_cue_track_fields_file_authoritative` continues to cover dual-authority `[SidecarCue, IndividualFiles, EmbeddedCue]`; existing R21 tests cover `[IndividualFiles]`, `[IndividualFiles, SidecarCue]`, target omission, and preflight no-write refusals. None were removed.

## Validation status and strict claims

**Rust compile/test status: NOT RUN** in this environment (`cargo`, `rustc`, `rustfmt`, and `nix` absent). This is an integrity/source-audited corrective candidate, **not build-host acceptance**. Do not claim the added regression passes until executed. The independent compile-free verification and SHA inventory are in `R21C_VALIDATION_2026-10-16.txt` and `SHA256SUMS`.

The changed existing source files, relative to R21, are exactly `src/tui/app.rs` and `src/tui/keybindings.rs`. The old R21 patch/brief/checkpoint remain intact. All 24 Reference-locked entries, `Cargo.lock`, the SACD crate and reference assets remain byte-identical to R21.

## Required build-host acceptance

Enter the repository's pinned Nix development shell (see `CLAUDE.md`) and run:

```sh
set -eu
sha256sum -c SHA256SUMS
cargo fmt --all -- --check
cargo test --locked r21c_sidecar_only_native_composer_save_never_writes_flac_tags
cargo test --locked presplit_metadata_sidecar_keeps_non_cue_track_fields_file_authoritative
cargo test --locked r21_
cargo test --locked rf64_
cargo test --locked hdcd_off_preserves_user_dither_and_independent_deemphasis_tpdf
cargo test --locked --test reference_qualification_freshness
cargo test --locked --workspace --no-fail-fast --exclude tonepoet-true-peak
```

Run the separately gated true-peak tests per `CLAUDE.md` if required by the delivery gate. A build or test failure needs a **specific smallest coherent correction** and a new verified checkpoint before release. Do not change source-locked paths, regeneration/qualification certificates, production DSP, or unrelated modules to pass this issue.

## Continuation protocol

1. Verify the bundle SHA file and unpack it; check `SHA256SUMS`.
2. Read this checkpoint, the R21 predecessor checkpoint, the focused patch, and the relevant original brief.
3. Run the pinned Rust build and targeted tests first, then the workspace gate. If a regression fails, inspect exact source and revise narrowly.
4. On a new package, regenerate SHA256SUMS and verify a clean extract. Preserve unmodified baseline file bytes; avoid broad refactors.

Outstanding items unrelated to this correction remain exactly as recorded in R21 (in-field dither toggle sequence, R5 UI review, #75 physical SACD tag-transfer round trip, and #73 intentionally out of scope).

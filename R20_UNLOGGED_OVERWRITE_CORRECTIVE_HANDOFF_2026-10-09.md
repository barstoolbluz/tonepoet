# TonePoet R20 corrective — single-file overwrite with human logging disabled

**Date:** 2026-10-09. **Status:** Source correction and regression tests written, not compiled or runtime-qualified on this host. **Do not release as qualified Reference.**

## Defect and fix

An album already containing `01.flac` and `02.flac` could lose `02.flac` when a later conversion selected only source track 01 with `--overwrite-output` / `if exists: overwrite` and Write log disabled. The single selected input has no dispatcher album-batch context, standalone `conversion.log`, or hidden log fragment. The old `is_incremental_single_audio_publish` predicate therefore returned false and entered whole-directory `ReplaceWithBackup`, installing only `01.flac`.

The correction is confined to the existing predicate and its immediate call site in `src/convert/pipeline/stages.rs`:

- Pass the explicit overwrite decision to `is_incremental_single_audio_publish`.
- Accept **exactly one audio output representing exactly one source track** under explicit `ReplaceWithBackup` or legacy `AlwaysRedo`, even with neither type of log artifact. The preexisting incremental publisher then replaces that one audio file with its existing transactional rollback.
- Keep `suppress_incremental_conversion_log_append` as the first, absolute fail-closed check. The existing outer refusal remains unchanged.
- `FailIfExists` / `KeepBoth` do **not** gain the no-log eligibility. The default refusal of an existing destination is unchanged.
- Multi-audio and merged multi-source-track payloads are still excluded and retain the whole-album publication/recovery path. Human logging remains optional. No scheduler, ReplayGain, CLI, TUI, transaction scheme, or Reference code was redesigned.

## Tests and scope

`r20_single_file_overwrite_without_logging_preserves_other_album_tracks` creates a two-file album, stages **only** `01.flac` with `write_conversion_log = false` and no staged log/fragment, and checks: default `FailIfExists` still refuses; explicit overwrite replaces 01 but preserves the bytes of 02; no visible conversion log appears; no successful whole-album backup remains. It also checks the predicate's emergency suppression and merged multi-track boundaries. Existing regression tests for interrupted-publish backup/recovery and logged independent-file overwrites remain intact.

`scripts/smoke_r20_album_publication.sh` additionally executes a real CLI single-WAV redo without `--write-log` after an earlier three-track album has been published. It checks that all three FLAC files remain, unselected files retain their SHA-256 bytes, existing visible and timestamped log history remains unchanged, and no album backup survives. This smoke runs in the configured build environment; it was **not executed here**.

## Build-host acceptance and Reference qualification

From the **final unchanged source checkout** with Rust/Cargo and qualified real audio tools:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
bash -n scripts/smoke_r20_album_publication.sh scripts/requalify_reference_r20.sh
cargo test --lib r20_single_file_overwrite_without_logging_preserves_other_album_tracks
cargo test --lib r20_independent_overwrite_preserves_one_two_and_three_track_albums
cargo test --lib r20_ungrouped_overwrite_refuses_directory_replacement_and_preserves_siblings
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

Finally perform the **separate mutated-source negative Reference smoke** specified by `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`. The R20 requalification runner reruns the full gates and creates a fresh, coherent four-sidecar set; do not hand-edit source locks or bypass the freshness gate. Because `stages.rs` is source-locked, this corrective invalidates any qualification for the earlier R20 source. The R19 green 7,376-test baseline cannot serve as proof that this new source passes.

## Resume and provenance

The complete corrective source archive includes this handoff, `RESUME_R20.md`, and the earlier R20 handoffs. The companion Git history bundle includes the original archive-derived baseline and all local implementation checkpoints. The small corrective patch applies **only to prior R20 checkpoint `38808f5`**; do not apply it to the original R19 archive or a source tree that already contains this correction. The full source archive is independently usable without the patch. Use the supplied SHA-256 manifest to verify transport integrity.

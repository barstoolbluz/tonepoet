# Album ReplayGain corrective delivery — 2026-09-25

## Baseline

This delivery is based on the supplied `tonepoet_album_replaygain_corrective_2026-09-25` tree, identified by the task brief as branch `apply/five-field-defects-2026-09-25` at bundled head `abdc89f`.

## Correction

The existing independent-file Album/Both ReplayGain preflight, post-encode barrier, and native batch reduction were already the correct coordination mechanism. The defect was dispatch: eligible `SingleFile` items still entered `build_single_file_work`, which performs ReplayGain and publication inside one work unit and returns `QueueWorkOutput::PostProcessed`. Those items therefore never emitted `Encoded` and never reached the existing shared ReplayGain barrier.

`src/convert/processor.rs` now keeps the direct single-file fast path for ordinary conversions, Track-only ReplayGain, disabled ReplayGain, and singleton batches, but routes multi-member dispatcher-authored Album/Both ReplayGain batches through the existing scheduler-split path. Each member therefore materializes and encodes independently, reaches the shared post-encode barrier, participates in one batch measurement, and only then proceeds to its existing post-processing/publish path.

No second ReplayGain implementation or reduction path was added. The existing fail-closed batch accounting remains authoritative: incomplete, mixed-policy, malformed, duplicate, or otherwise failed participants cannot silently fall back to a one-item album reduction.

## Regression coverage

The existing end-to-end regression `queued_independent_album_replaygain_reduces_across_all_members` now:

- exercises `ReplayGainMode::Album`, matching the field failure;
- installs real per-item runtime execution authority so scheduled fake-tool execution has the same item-supervisor capability required by production containment;
- unregisters every runtime execution authority before asserting the conversion result; and
- continues to require distinct per-track gain/peak values and identical album gain/peak values across the two independent files.

A focused dispatch regression, `album_replaygain_batch_routes_single_files_through_scheduler_barrier_path`, verifies that a prepared two-member independent-file Album ReplayGain batch produces `WorkKind::MaterializeItem` rather than the direct `WorkKind::SingleFile` executor. The pre-existing boundary regression still verifies that a prepared album batch without this ReplayGain requirement retains the direct single-file fast path.

## Performance and lifecycle behavior

Only multi-member dispatcher-authored Album/Both ReplayGain batches leave the direct single-file executor. Track-only ReplayGain, disabled ReplayGain, singleton batches, and unrelated single-file conversions retain the existing fast path. Album/Both batches perform one native ReplayGain measurement over the complete cohort, then project the retained per-track observation plus shared album reduction into each member during the existing post-processing stage.

The correction does not alter scheduler terminal accounting, cancellation behavior, publication ordering, queue persistence, or conversion-log assembly. Runtime execution authority added to the regression harness is test-only setup and cleanup; production execution acquisition is unchanged.

## Reference qualification

No Reference source-lock file changed. In particular, this delivery does **not** modify:

- `build.rs`
- `reference_source_lock.rs`
- `src/convert/replaygain.rs`
- `src/convert/pipeline/stages.rs`

The implementation change is confined to `src/convert/processor.rs`. The installed Reference report, certification, registry, and evidence are untouched. **No Reference requalification/reinstallation is required for this delivery.**

## Validation performed here

This environment does not provide `cargo`, `rustc`, `rustfmt`, `flox`, or `nix`, so I could not compile the workspace or execute the Rust regressions locally. I did not substitute a partial or unrelated test run for the required gate.

Static delivery validation completed successfully:

- byte-for-byte comparison confirmed that only `src/convert/processor.rs` changed before delivery metadata was added;
- the Reference source-lock boundary files listed above are byte-identical to the supplied baseline;
- the routing predicate is limited to multi-member Album/Both ReplayGain batches;
- the broader preflight predicate remains intact so malformed batch contracts still fail closed;
- the field regression contains real runtime execution registration and deterministic cleanup;
- the focused dispatch regression protects the root-cause branch; and
- `git diff --no-index --check` reported no whitespace errors in the source delta.

The operator should run the task's normal workspace gate on the intended Flox/Nix toolchain. The repository documents the gate command as:

```sh
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

Useful focused checks before the full gate are:

```sh
cargo test -p tonepoet queued_independent_album_replaygain_reduces_across_all_members -- --nocapture
cargo test -p tonepoet album_replaygain_batch_routes_single_files_through_scheduler_barrier_path
```

## Changed files relative to the supplied bundle head

See `CHANGED_FILES_album_replaygain_corrective_2026-09-25.txt`.

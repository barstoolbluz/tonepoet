# TonePoet R20 — engineering delivery and build-host handoff

Date: **2026-10-09**. Status: **SOURCE IMPLEMENTED; NOT COMPILED, TESTED OR REFERENCE-QUALIFIED ON THIS HOST. NOT YET A RELEASE.**

Source authority: `BRIEF_LOGGING_R20_overwrite_loss_rg_survivors_logs_pills_2026-10-09.md` (the uploaded R20 brief). Source baseline: uploaded `TonePoet_LOGGING_R20_overwrite_loss_rg_survivors_logs_pills_2026-10-09.tar.gz`, described by the author as `origin/main` at `d4d6870` and a previously passing 7,376/0 R19 baseline. **That green gate was prior to R20; it is not evidence of R20 passing.** The local Git history was initialized from the archive because the archive did not include `.git`; its commit hashes are checkpoints rather than upstream ancestry.

## Scope implemented

**#69 Overwrite retains the album.** A one-track independent-file publish into an existing album now uses the existing journalled incremental publication with per-file rollback for the replaced audio, rather than moving/replacing the entire shared album directory. Whole-album backup-and-rename is retained for genuine multi-track payloads and cancellation/interrupt recovery. Successful whole-album publication removes the backup directory and recovery marker. An ungroupable emergency-fallback track now **refuses** destructive whole-album overwrite rather than claiming success while dropping siblings.

**#68 ReplayGain surviving cohort.** At the already-established post-encode barrier, failed input jobs are counted as terminal but excluded from the album measurement. Successfully encoded members proceed as the contributing measurement cohort and keep their album tags. The surviving member's log fragment includes the count, excluded source paths, and their reasons; the assembly logic selects a populated disclosure even if an earlier failed fragment precedes it. The failed item retains its own error/nonzero CLI status. This does not change `--partial` semantics for multi-track sources. Pre-materialization fragment logging remains in place.

**#70 Historical logs.** When a human conversion log is requested, per-run `conversion-<UTC-nanosecond>-<run-id>.log` snapshots are created beside `conversion.log`; repeated finalization of the *same* batch updates its snapshot, but different run IDs create different files even if the log text matches. Old `conversion.log` is snapshotted before replacement if its bytes are absent from history. Historical logs follow legitimate whole-album replacement. Terminal-failure/cancel finalization also attempts archival. Fresh logging-disabled outputs still have no log artifacts.

**Output Options controls.** `partial (source)` is a bool pill and `if exists` offers `fail`, `overwrite` (backup retained as internal safety mechanism), and `keep both`. CLI supports `--if-exists fail|overwrite|keep-both`, retaining `--overwrite-output` alias. Numeric keep-both suffixes (`Album (2)`, `Album (3)`) shorten UTF-8 by filesystem component byte limit. A dispatcher-resolved shared album destination is pinned across independent tracks. Unsupported disc-token/unresolved destinations are refused rather than guessed. Browse/queue option propagation and TUI preset read/write are updated. `AlwaysRedo` remains an internal compatibility variant, not a fourth pill.

## Verification included in source

New tests include `r20_independent_overwrite_preserves_one_two_and_three_track_albums`, `r20_ungrouped_overwrite_refuses_directory_replacement_and_preserves_siblings`, `r20_terminal_failure_is_excluded_without_releasing_replaygain_barrier_early`, `r20_reduced_album_gain_log_names_excluded_source_and_measurement_size`, `r20_distinct_runs_retain_distinct_log_snapshots_and_legacy_history`, and `keep_both_siblings_are_numbered_and_utf8_component_safe`. Both #65 strict destination-inventory tests were extended to expect **exactly one** timestamped log snapshot on a first human-log-enabled run, and none otherwise; they still refuse arbitrary files. Existing interrupted-publish and Reference tests remain intact.

`scripts/smoke_r20_album_publication.sh` is a **real compiled CLI** test: one/two/three tracks twice, backup cleanup, accumulated log snapshots, one shared keep-both album, and failed ReplayGain member with a surviving tagged file and named exclusion. It uses isolated HOME/XDG paths and cleans scratch files. `scripts/requalify_reference_r20.sh` is derived from the R18 successful requalification approach: real-tool qualification, four fresh coherent JSON sidecars, atomic-ish installation with rollback on certification/freshness failure, focused regressions, full workspace checks, the R19 #67 CLI exit smoke, R20 black-box smoke, and optional positive real SACD ISO smoke. A wrong-source runtime-mismatch negative smoke is an additional manual release check; see `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`.

## Known validation gap — a release blocker, not a new feature request

This container has **no `cargo`, `rustc` or `nix`**. R20 Rust compilation, unit/integration suite, real-audio output, 7,376-test baseline comparison and actual Reference qualification are **unexecuted**. Bash syntax validation, Git whitespace checks, input archive source integrity, and a conservative lexical bracket audit of all modified Rust files are static checks only and cannot establish compiler or runtime correctness. The local Source-Lock JSON's raw non-Cargo digest audit shows that **`src/convert/pipeline/stages.rs` and `src/fs_limits.rs`** changed; installed Reference qualification sidecars no longer bind to this source. Their source-lock digests must never be hand-updated or bypassed.

## Build-host acceptance procedure

Use the intended Nix/qualified audio-tool build environment, with any runtime-closure pair configured as documented in `docs/packaging/reference-runtime-staging.md`, from the **final, unchanged source bytes**:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
bash -n scripts/requalify_reference_r20.sh scripts/smoke_r20_album_publication.sh
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The R20 requalification script runs a second complete test pass by design; avoid claiming acceptance from the first pass alone. Then perform the **isolated mutated-source negative Reference smoke** in `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`. Confirm four fresh sidecars, zero failures, qualified positive and refusal negative before merge/release. If a failure occurs, correct only the demonstrated defect and repeat these steps, regenerating qualification after the *last* locked-source modification.

## Reproducible artifact recovery

Distribution includes (a) complete working source tarball, (b) a cumulative unified patch against local baseline Git commit `7ccec9e`, (c) a Git history `.bundle` containing all checkpoint commits, and (d) a SHA-256 manifest. The Git history is local-only and **not** the original repository's upstream lineage. To resume independently, extract the complete R20 source tarball and use it directly, OR clone the Git `.bundle`; do not apply the patch atop already patched source. For original repo integration, review the patch against R19 baseline before cherry-picking or applying. No outstanding background work or deferred automation is claimed.

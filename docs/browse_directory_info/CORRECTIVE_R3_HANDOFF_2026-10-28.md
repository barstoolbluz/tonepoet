# TonePoet Browse directory summaries: corrective R3

Date: October 28, 2026 (America/New_York). Status: **D-03 source correction delivered; Rust host qualification pending**.

## Exact input and scope

- Input: `TonePoet_Browse_Info_Directory_Summaries_CORRECTIVE_R2_2026-10-28.tar.gz`
- Independently calculated input SHA-256: `41cf9da94c228ae7064f3a9f30ec2ceca2372614d23d3cd6afa9ed5d67e0c8ec`
- Output: `TonePoet_Browse_Info_Directory_Summaries_CORRECTIVE_R3_2026-10-28.tar.gz`. Its SHA-256 is recorded in the accompanying delivery message.
- Relative to R2, only production file `src/tui/browse.rs` changes. D-01, D-02, and all other source paths remain byte-for-byte unchanged. The only other additions are these R3 handoff/verification documents and the R2-to-R3 patch.

## D-03 correction

The existing `compute_dir_stats_with_budget()` still runs the same bounded BFS, with the same deadline, cancellation behavior, traversal depth and count limits, and single active worker. The walk now records `immediate_complete` separately from `DirStats.complete`.

- A failed *top-level* `read_dir` entry or `DirEntry::file_type()` lookup sets `stats.complete = false` **and** disables `stats.immediate`, preventing incorrect exact-looking Folders/Files/Audio files counts for a highlighted collection folder.
- A failure or depth cutoff **in a descendant** sets `stats.complete = false` but leaves exact first-level counts intact if that level finished without errors.
- Exceeding the budget before completing the first level still returns no immediate counts; exhausting it only after finishing the first level preserves those counts.
- The v4 persisted summary records retain their existing format: missing `immediate` is stored as `-` for the three fields. No cache schema change or second scanner was added.

A small generic reader parameter lets tests append deterministic `read_dir` failures without adding mock plumbing to the production path or introducing a different walker.

## Regression tests added (written, NOT run)

1. `browse_directory_info_tests::recursive_stats_readdir_failure_at_first_level_cannot_cache_exact_counts`: injects a first-level enumeration error after actual entries, checks `complete=false` and `immediate=None`, checks the selected child Info pane path exposes no exact counts, and checks the directory-summary v4 persistence round-trip retains `immediate=None`.
2. `browse_directory_info_tests::recursive_stats_descendant_readdir_failure_preserves_complete_first_level`: injects an error in a nested album, checks `complete=false` but correct direct counts (1 folder, 2 files, 1 audio file) remain.

The existing `browse_directory_info_tests::bounded_walk_records_direct_counts_even_when_descendants_are_skipped` remains a separate control for depth-limit truncation.

## Verification performed

- Verified the R2 input archive SHA-256 against the accepted R2 delivery SHA above.
- Independently extracted R2 into a new R3 working tree.
- Compared regular source files against R2; only `src/tui/browse.rs` differs (plus added R3 documentation).
- Generated a unified, R2-root-relative patch, ran `patch --dry-run -p1` successfully on R2, and ran `git diff --no-index --check` successfully.
- Compared production `Cargo.toml` and all other Rust sources against R2; unchanged.
- Validated final archive extraction, file hashes, and member safety; see `CORRECTIVE_R3_STATIC_VERIFICATION.txt`.

**Not executed:** Rust formatting, compilation, `cargo test`, interactive TUI behavior, actual mount and large-directory tests, CPU measurements. There is no `cargo`, `rustc`, or `rustfmt` installed in this execution environment. These remain host qualifications, not passes. The workspace declares `rust-version = "1.93"`.

## Host qualification

From the extracted `tonepoet/` root with Rust 1.93 or newer:

```bash
cargo fmt --all --check
cargo check -p tonepoet
cargo test -p tonepoet recursive_stats_readdir_failure_at_first_level_cannot_cache_exact_counts
cargo test -p tonepoet recursive_stats_descendant_readdir_failure_preserves_complete_first_level
cargo test -p tonepoet bounded_walk_records_direct_counts_even_when_descendants_are_skipped
cargo test -p tonepoet browse_directory_info_tests
cargo test -p tonepoet directory_info_presentation_tests
```

Run existing Browse regressions and the project's normal full test suite before merge, then test a root directory, collection folder, multi-disc album, archive, and mounted directory in the TUI. Check rapid selection changes and idle CPU against the accepted baseline. R2 was never approved for merge; if it *was* run locally with persistent directory-summary caching, old v4 records written during a failed first-level scan might need invalidation before judging recomputed counts. The v4 record itself cannot identify that historical R2 omission.

## Engineering handoff

This closes the specific D-03 source path without a new indexing mechanism, cache schema, worker, dependency, or UI component. Merge acceptance must still wait for host Rust and TUI qualification; do not substitute the static checks for executed Rust regression evidence.

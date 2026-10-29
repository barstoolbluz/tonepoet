# Browse Info pane directory summaries - implementation handoff

Date: 2026-10-28
Status: **Implemented in the supplied source tree; Rust build and runtime qualification NOT executed in this environment**.

## Input identity

`TonePoet_SNAPSHOT_d5b57aa_2026-10-28.tar.gz`
SHA-256: `67adafdb0b1649825101210a652f1ec3a8a7df9ae25e29d6ad3477799e23b800`

This is the implementation input, superseding the older archive discussed in the brief. The output is a complete source tree, not a patch-only overlay. No dependency or Cargo configuration changes were made.

## Production changes

- `src/tui/browse.rs`: The `..` info target is `current_dir`, not its parent-navigation path. The completed unfiltered directory listing supplies immediate folder/file/audio counts. Its audio count is precomputed once at authoritative scan publication, so rendering does no per-frame enumeration. Collection child counts are collected in the existing bounded directory-stats worker. The same worker returns a complete/incomplete indicator and caches bounded partial results rather than repeating failed budget-limited traversals. Existing cache persistence now has a version-4 payload that records completeness and immediate counts while reading legacy v1-v3 classification facts without claiming their old sizes are exact. The current tab retains one last focused-directory filesystem-space value and a single debounced request, fetched using `fs2` on Tokio's existing blocking pool.
- `src/tui/draw_browse.rs`: Parent-directory selection displays the displayed directory's name and existing modification date plus the requested rows; collection/ordinary directory rows show immediate counts, separate contents size and disk free/total capacity, with binary KiB/GiB/TiB units. Album, multi-disc, disc-source, individual-audio, and logical archive presentations retain their dedicated paths.
- `src/tui/message.rs`: Adds one tab/generation/request-owned filesystem-space completion message.
- `src/tui/event_loop.rs`: Routes the completion through the existing Browse deferred-update path and permits current-directory `..` stats identity recovery.

`src/db.rs` changes only an existing unit-test `DirStats` literal to accommodate the additive fields. No database schema migration and no additional source indexing/worker scheduler were introduced.

## Display semantics

- `Folders`, `Files`, `Audio files`: **immediate children** only. For `..`, taken from `all_dirs` and `all_files` only after terminal completion; streamed partial results show `scanning...`. For a highlighted collection child, taken from the first (fully enumerated) directory level of the existing background walk. Unfinished first-level reads show `unavailable` or `calculating...` rather than zero.
- `Contents size`: **sum of logical regular-file sizes** for descendants visited by the existing bounded non-following traversal; not allocated storage or free disk space. A total is displayed without qualification only when the walk reports full coverage. Truncated walks, unreadable subdirectories, and symlink omissions show a positive lower-bound amount marked `(partial)`; if no positive amount is known, show `not calculated`. The controls remain depth 4, files 50,000, entries 75,000, elapsed 200 ms. This is intentionally not an exact total for arbitrary large/deep trees.
- `Disk free`: available-to-current-user space and total capacity from the filesystem containing the selected directory, queried via `fs2::available_space` and `fs2::total_space`. Failure shows `unavailable`; requests are focus-settled, asynchronous and non-polling.
- `Updated`: existing modification date when available. On `..`, metadata for `current_dir` rather than for its parent.
- Cache scope: identity scoped to the selected directory. A new descendant edit that does not change the selected directory's own identity may leave a cached recursive figure stale until refresh. Strict recursive invalidation is intentionally outside scope; no subtree index was created.

## Focused Rust regression tests added/adjusted

- `browse_directory_info_tests::parent_row_summarizes_current_directory_from_unfiltered_completed_listing`
- `browse_directory_info_tests::partial_streamed_listing_never_exposes_final_immediate_counts`
- `browse_directory_info_tests::parent_focus_queues_bounded_stats_for_current_not_ancestor`
- `browse_directory_info_tests::filesystem_space_lookup_uses_selected_mount_and_rejects_stale_focus`
- `browse_directory_info_tests::bounded_walk_records_direct_counts_even_when_descendants_are_skipped`
- `browse_directory_info_tests::filesystem_root_has_no_synthetic_parent_row`
- `directory_info_presentation_tests::parent_row_displays_current_directory_and_not_ancestor`
- `directory_info_presentation_tests::incomplete_collection_size_is_explicitly_partial_and_binary`
- `directory_info_presentation_tests::binary_size_formatter_does_not_call_gibibytes_gigabytes`
- `directory_info_presentation_tests::archive_navigation_rows_do_not_claim_filesystem_directory_stats`
- Existing directory-summary cache serialization, v2/v3 compatibility, classification, folder stats and TUI reducer fixtures adjusted where their assertions depended on old output or literal layout.

## Executed qualification

- Input snapshot SHA-256 (`sha256sum`): **PASS**; see identity above.
- Full source inventory comparison against extracted input: **PASS**, 1,348 original regular files retained; exactly five original Rust files modified before new handoff files were added.
- `git diff --check` on five source changes against the input snapshot: **PASS**, no whitespace errors.
- Source static consistency and packaged archive checks: recorded in accompanying verification log.
- **NOT RUN:** `cargo fmt`, `cargo check`, `cargo test`, release build, end-to-end TUI navigation or live multi-mount validation. `cargo`, `rustc`, and `rustfmt` are not installed in this execution environment and network install was unavailable. Do not infer compile success or test pass from the static checks.

## Host qualification commands

From the extracted `tonepoet/` root with the supported Rust toolchain and dependencies:

```bash
cargo fmt --all --check
cargo check -p tonepoet
cargo test -p tonepoet browse_directory_info_tests
cargo test -p tonepoet directory_info_presentation_tests
cargo test -p tonepoet folder_classification_info_pane_tests
cargo test -p tonepoet directory_summary
cargo test -p tonepoet browse_perf
```

Then interactively navigate a large directory, move selection rapidly between a collection and `..`, switch tabs while capacity probes run, inspect an album, an SACD ISO, and an archive, and compare idle CPU and responsiveness to the original snapshot. Validate separately mounted paths where the host has them. Do not approve this work as host-qualified until these checks pass.

## Engineering handoff

The change is scoped to existing Browse state, cached summaries, worker, event reducer and Info pane. If qualification reveals a failure, fix it locally rather than introducing an indexing subsystem. The attached resume checkpoint names the exact input and remaining work.

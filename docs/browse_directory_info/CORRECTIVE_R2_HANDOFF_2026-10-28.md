# Browse Info pane directory summaries: corrective R2 handoff

Date: October 28, 2026 (New York). Status: **source corrected; Rust host qualification pending**.

## Source and scope

- Input: `TonePoet_Browse_Info_Directory_Summaries_2026-10-28.tar.gz`
- Input SHA-256: `79326f91c487e8b55f1ad73a5b148ac646c8db306cdeb1000063ea2a61246bdb`
- Original implementation source: `TonePoet_SNAPSHOT_d5b57aa_2026-10-28.tar.gz` (details in the retained initial handoff).
- Output: full-source `TonePoet_Browse_Info_Directory_Summaries_CORRECTIVE_R2_2026-10-28.tar.gz`. Check its SHA-256 against the external delivery report.
- No changes to Cargo dependencies, directory index architecture, existing traversal budgets, summary persistence, Info pane layout, or classification behavior.

## Corrective inventory (relative to the R1 source)

1. `src/tui/browse.rs`:
   - D-01: Stop ordinary directory enumeration on individual `read_dir` errors, and stop on failed `symlink_metadata` rather than silently dropping entries. The existing terminal scan-error reducer preserves partial streamed entries but sets `browse.error`, which prevents `current_directory_listing_counts()` from asserting completeness. The actual scan loop is shared with small fault-injection tests; there is no additional scan or worker.
   - D-02: Give each launched `DirStatsActiveJob` a monotonic request ID. Preserve the counter with shared Browse-tab worker state. Reject unowned completions before touching the pending marker, active worker, or queue. The counter is not reset on navigation.
2. `src/tui/message.rs`: Carry `request_id` in `DirStatsComplete`.
3. `src/tui/event_loop.rs`: Supply the request ID to the stats completion reducer before identity validation or cache mutation. Adjust two existing completion tests to start owned requests.

The entire source change is within these three Rust files. The retained R1 implementation and its prior handoff remain present.

## Targeted regressions added

- `browse_directory_info_tests::failed_directory_enumeration_cannot_publish_authoritative_immediate_counts`: An injected error after a valid directory entry must fail the scan rather than publish incomplete counts.
- `browse_directory_info_tests::failed_entry_metadata_cannot_finish_directory_scan_successfully`: Removing a real file between directory enumeration and lstat must fail the scan.
- `async_message_drain_tests::late_stats_completion_cannot_displace_newer_request_for_same_directory`: Start A, cancel by moving focus, start B on the same path/generation/identity, deliver A's cancelled and delayed-success completions, confirm B retains ownership, then deliver B's completion and confirm only B's value is published.
- Existing `stale_dir_stats_completion_rechecks_only_when_current_and_statable` and `missing_dir_stats_completion_stops_without_retry` now begin owned stats requests before submitting completions. The missing-directory fixture waits for its worker to finish reading before removing the directory.
- Existing explicit `DirStatsActiveJob` test fixtures gained the new field.

## Verification record

- Source archive input hash: verified against the value above.
- Extraction and source inventory comparison against the R1 tree: verified. No R1 files removed; only `browse.rs`, `message.rs` and `event_loop.rs` differ before adding R2 handoff artifacts.
- Diff whitespace: checked, no errors reported.
- All stats job constructors and completion message sites: source-level inventory checked for request ID wiring.
- **Not executed:** `cargo fmt`, `cargo check`, `cargo test`, complete build, real mount testing, interactive UI tests, CPU measurements. This container has no `cargo`, `rustc` or `rustfmt`; attempts to acquire a toolchain are blocked by unavailable network access. These are unqualified, not passes.

## Host qualification

From the extracted `tonepoet/` root with the supported Rust toolchain:

```bash
cargo fmt --all --check
cargo check -p tonepoet
cargo test -p tonepoet failed_directory_enumeration_cannot_publish_authoritative_immediate_counts
cargo test -p tonepoet failed_entry_metadata_cannot_finish_directory_scan_successfully
cargo test -p tonepoet late_stats_completion_cannot_displace_newer_request_for_same_directory
cargo test -p tonepoet stale_dir_stats_completion_rechecks_only_when_current_and_statable
cargo test -p tonepoet missing_dir_stats_completion_stops_without_retry
cargo test -p tonepoet browse_directory_info_tests
cargo test -p tonepoet directory_info_presentation_tests
cargo test -p tonepoet browse_perf
```

Then qualify real navigation under a large listing, rapid cursor return to the same directory, `..`, album, SACD/ISO, archive, tab switches, and mount-specific disk space. Compare responsiveness and idle CPU against the accepted host baseline. Run the full repository regression suite under the normal project configuration before merging.

## Remaining limitation

The contents-size walk remains intentionally bounded; `complete=false` continues to mean partial or unavailable, not an exact whole-tree sum. Directory listings cannot be atomic against external filesystem changes: the new correction detects enumeration and metadata errors rather than claiming an error-tainted scan was complete. Cached recursive summaries may become stale if descendants change without an observable parent-directory identity update. Both are existing, documented constraints; no full-library indexing subsystem was added.

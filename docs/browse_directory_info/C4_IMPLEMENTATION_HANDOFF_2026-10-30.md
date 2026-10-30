# TonePoet C4 directory accounting: implementation handoff

**Date:** 2026-10-30
**Base:** `TonePoet_SNAPSHOT_04ecaa3_directory-accounting_2026-10-30.tar.gz`
**Verified base SHA-256:** `cf68a5cb663f450ec008aa12b203058381eed202c3277605788545f8f3f5ce3c`
**Brief:** `BOUNCE_DIRECTORY_ACCOUNTING_C4_2026-10-30.md` and its `revised-information-display.png`.
**Status:** Implementation candidate; **Rust build and test suite not executed in the authoring environment** (no Rust/Cargo toolchain).

## Scope and behavior

1. **Foreground:** `RateGovernor::tick()` bypasses the per-volume token bucket completely for an explicit or promoted foreground scan. Background metadata rates and shared budgets remain unchanged. Cancellation is still checked at each accounting step; cooperative yields remain.
2. **Cancellation:** Browse Info uses one focusable, clickable block whose label changes between **Refresh** and **Cancel**. Both mouse and keyboard dispatch into the same action handler. Folder Properties `C` now requests cancellation of the shared walk instead of detaching its modal subscription. `Esc` still just closes/detaches. Queued requests for the same path are also discarded, and the same root is suppressed from automatic rescheduling for the normal 24-hour background attempt window; explicit Refresh is still available. After cancellation the service ignores late progress/direct-count messages and restores an independently known one-level count, if one existed. It never publishes a partial root total as a complete measurement.
3. **Background network/unknown volume admission:** A worker performs read-only `symlink_metadata`, `read_dir`, and at most 12 distinct entry `symlink_metadata` calls while inspecting at most 48 entries. The decision is cached per volume for 60 seconds. Admission requires an average observed metadata-call cost <=12 ms, no sampled operation >120 ms, and total probe time <=300 ms. Enumeration time is included in the latency numerator; unlike metadata operations it does not dilute the denominator. A slow or failing probe defers the background pass and retries after 60 seconds. A worker rechecks on entering new folders **and every 60 seconds inside a single long directory**. The existing `TONEPOET_ACCOUNTING_ALLOW_UNKNOWN_VOLUMES=1` remains an explicit override. Foreground requests never probe and never defer for volume class.
4. **Info:** The four counted rows are always present, with `unknown` in dim grey if unavailable. **Folders is the immediate direct-child directory count**; **Files, Audio files, and Total size are exhaustive recursive measurements**, provisional only during a running walk. This deliberately prevents one-level previews from being shown as known recursive file totals. Counted rows, disk capacity, and Updated use a wider aligned value column; Kind retains the narrow original column. Running rows have a purple braille spinner and Total size an ellipsis. Disk free shows a used/free bar with a solid light-blue used portion and shaded available portion. Updated displays only the date. The purple Refresh/Cancel block is on its own row after a blank row, aligned with the metric labels. No block appears when measured totals are known and no scan is active. Optional Disk free and Updated stay hidden when unavailable.
5. **Short terminals:** A conventional 24-row Browse Info pane cannot physically display the full Name/Kind/stats/Updated/button stack. The renderer shows the end of that summary so that Refresh/Cancel and its real mouse hitbox are visible. Tall panes show the reference image's order and spacing from the top. No unrelated pane-height budget or TUI chrome was changed.

## Accounting invariants preserved

- Recursive values are only persisted for completed and validated subtrees; no synthetic partial aggregate is marked complete.
- No symlink/reparse traversal by the scanner or network metadata probe.
- Freshness states, epochs, generation checks, invalidation policy, SQLite schema, subtree qualification and background metadata budgets are unchanged.
- A cancellation request takes effect cooperatively at the next metadata boundary. It cannot interrupt a kernel syscall already blocked on an unresponsive filesystem.
- **Persistence nuance:** Under the established incremental-commit design, a child subtree fully completed *before* cancellation may already have been saved. A cancelled root is not marked complete, and unsaved running progress is discarded. A previous independently complete root measurement is not erased. The brief's tentative assertion that cancellation saves absolutely nothing does not hold for the original complete-subtree persistence model; changing that would require a disruptive persistence redesign and is out of scope.
- A terminal success that was committed before the user pressed Cancel may still be reported as complete. Cancellation is not retroactive.
- Probes are intentionally read-only. An sshfs client-side attribute cache can understate network RTT; the probe samples distinct entries, but cannot defeat an arbitrarily long or aggressive mount-side cache.

## Source changes

- `src/directory_accounting/volume.rs`: read-only per-volume latency gate, bounded sample, expiry, safe root policy, focused tests.
- `src/directory_accounting/scanner.rs`: foreground bypass; periodic network recheck; deferral result; additional cancellation check before bounded persistence transactions; audio-file progress.
- `src/directory_accounting/model.rs`: ephemeral audio progress and deferral status.
- `src/directory_accounting/mod.rs`: scan cancellation, queued removal, canceled event fencing and direct-count restoration, per-path retry handling, shared probe gate.
- `src/tui/draw_browse.rs`: Info layout, status-aware counters, button, spinner, capacity bar, compact viewport, render/hitbox regression cases.
- `src/tui/keybindings.rs`: common mouse and keyboard Refresh/Cancel activation, focus handling, regression case.
- `src/tui/directory_properties.rs`, `src/tui/draw_directory_properties.rs`: cancel semantics and status.

## Validation performed here

- Base snapshot SHA-256 independently verified against the brief.
- `git diff --check` passed on the C4 source changes.
- `python3 validation/directory_accounting_sqlite_gate.py`: **5 tests passed**, 35 repository SQL literals prepared. This is a SQL-only supplement, not a Rust compiler/test replacement.
- Rust and Cargo are not installed in the environment. No claim of successful `cargo fmt`, `cargo test`, interactive Browse checks, or sshfs behavior is made.

## Required local handoff gate

From the extracted C4 source root on the supported Rust 1.93+ host, with TonePoet native dependencies installed:

```bash
cargo fmt --all -- --check
cargo test --locked --lib directory_accounting::volume::tests
cargo test --locked --lib directory_accounting::service_tests
cargo test --locked --lib directory_accounting::scanner::tests
cargo test --locked --lib directory_info_presentation_tests
cargo test --locked --lib browse_info_keyboard_cancel_stops_the_same_refresh_mouse_controls
./validation/run_directory_accounting_host_gate.sh
```

The existing host gate includes the full workspace suite and the independent SQLite verification. Follow with manual tests on actual sshfs (warm and cold attribute caches, fast and slowed link, link degradation mid-scan), local SSD and rotational volumes, 80x24 and full-height terminals, mouse and keyboard Cancel, a mid-scan Cancel followed by Refresh, Folder Properties `C` and `Esc`, symlink and Windows reparse-point exclusions, and Ctrl+P / Ctrl+Shift+P terminal encodings. No host-specific execution is asserted here.

## Resume / integration

The downloadable engineering bundle includes (a) this complete source tree, (b) a clean source delta from the imported C4 base, (c) a two-commit self-contained Git bundle, (d) the original brief and reference image, and (e) SHA-256 hashes. A later session can inspect `README-HANDOFF.md` in the outer archive, apply the delta to the specified base, or clone the Git bundle directly. Do not report the new Rust tests as passed until an actual host run finishes successfully.

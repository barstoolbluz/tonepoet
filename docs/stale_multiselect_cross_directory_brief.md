# Brief: stale cross-directory multi-select marks drive destructive file operations

Date: 2026-07-08

## The incident (real, reproduced)

The user tagged and converted five CCR albums from
`~/livetorrents/Creedence Clearwater Revival - 24 KT Gold Collection/` into
`~/temp/`. Later, in `~/temp/`, they multi-selected the five **converted**
album folders and used context menu → File operations → Move to... →
`~/temp/destination-test/`.

Result: **ten** directories landed in `destination-test/` — the five
converted albums AND the five **source** album folders, which were moved out
of the user's source library (`~/livetorrents/...` was left holding only the
one album they never selected). The user's first hypothesis was hard-linking
or copy-back; it was neither. The move operation's source list silently
contained ten absolute paths: five marks made in `~/temp` plus five marks
made in `~/livetorrents` earlier in the session and never cleared.

This is the survivable variant. The same selection list feeds **Delete
permanently** — and the `trash` crate was removed, so deletion is
unrecoverable. One stale-marks Delete would have permanently destroyed
source albums in a directory the user was not even looking at.

## Root cause

1. Multi-select marks are stored as absolute paths in
   `BrowseState::multi_selected` (src/tui/browse.rs:2169).
2. **Directory navigation never clears them.** `enter_selected`
   (browse.rs:3983), `go_parent` (browse.rs:4001), `navigate_to`
   (browse.rs:4022), `navigate_to_str`'s synchronous fallback
   (browse.rs:4097), and `navigate_without_history` (browse.rs:2908 — the
   nav-history back/forward seam, and the landing point of async
   `navigate_to_str` resolution) all call `reset_nav_state()`
   (browse.rs:5751), which clears the multi-select *anchor*, filter,
   type-ahead, probe queues — but not `multi_selected` itself. The only sites that clear marks are the
   archive enter/exit paths (browse.rs:3223/3302/3337/3350/3372) and
   explicit clear (browse.rs:4487). Marks therefore accumulate silently
   across every directory visited, for the life of the session.
3. `collect_selection_for_file_ops` (src/tui/command.rs:5710) returns the
   **entire** mark list whenever it is non-empty, with no scoping to
   `browse.current_dir` and no visibility check.
4. Marks are only rendered as per-row checkmarks in the *current* listing
   (`is_multi_selected`, src/tui/draw_browse.rs:1796). There is no global
   mark count in the status bar or anywhere else. Off-screen marks are
   completely invisible: the UI shows five selected rows while the
   operation acts on ten paths.

## Blast radius — every consumer of the stale list

`collect_selection_for_file_ops` call sites (all take the full cross-
directory list today):

- Context menu file ops: **Move to... / Copy to...** (context_menu.rs:1349,
  1357 → `open_file_picker_for_copy_move`, command.rs:5555), **bulk rename**
  (context_menu.rs:1332), **Delete permanently** (context_menu.rs:1068 →
  `Command::Delete` → `execute_delete`, command.rs:5461), metadata-editor
  bulk guard
  (context_menu.rs:1057, 1531, 1541, 1557).
- `:` commands in command.rs — ~20 sites (lines 85, 521, 2023, 2379, 2401,
  2480, 2498, 2928, 3170, 3189, 3340, 3374, 3514, 3616, 3753, 3930, 5461,
  5527): mv/cp/delete, tagging (`:tags-mb` etc.), analyze, metadata editor,
  and more.
- keybindings.rs: 3465, 11083, 25485 (keyboard equivalents + guards).
- Conversion queueing: `collect_selection_for_queue` (browse.rs:4518+,
  called at browse.rs:15556/15598 and command.rs:4824) reads the same
  `multi_selected` — non-destructive, but the same "acts on things you
  can't see" astonishment applies.

Note `bulk_guard_frozen_paths` (command.rs:5711) intentionally freezes a
snapshot during a guarded bulk operation — that mechanism must keep working.

## Desired behavior

**Recommended design (applier's diagnosis, user-approved direction):**

1. **Scope marks to the directory being viewed.** Clear `multi_selected`
   (and the anchor) on every directory change — `enter_selected`,
   `go_parent`, `navigate_to`, `navigate_to_str`, nav-history jumps,
   bookmarks, and any other path that changes `current_dir`. The natural
   seam is `reset_nav_state()` — and it is clean: the applier verified that
   ALL five of its production callers are exactly the directory-change
   functions listed in Root cause item 2 (the remaining callers are
   tests), and that sort/filter/search/refresh/hidden-toggle never call
   it. Re-verify this before relying on it, then keep it true: any future
   caller of `reset_nav_state()` inherits mark-clearing, so document the
   contract at the function. Re-entering the SAME directory should be a
   no-op on marks: idempotent.
2. **Defense in depth at the consumer.** `collect_selection_for_file_ops`
   (and `collect_selection_for_queue`) must never return a path outside
   the current view even if state is somehow stale: filter to paths whose
   parent is `browse.current_dir` (mind the archive-browse case, which has
   its own synthetic paths, and the root-dir edge). This guard is what
   makes the fix robust rather than merely patched: either layer alone
   protects the user; together a regression in one is caught by the other.
   Decide and document whether the filter silently drops or surfaces a
   status when it removes stale paths — silence hides bugs; prefer a
   status line when anything was dropped, since a dropped path means an
   invariant already failed upstream.
3. **Zero behavior change otherwise.** Within one directory, marking,
   range-select (anchor machinery, browse.rs:4375-4421), select-all
   toggle (browse.rs:4457), disc-source-root preservation
   (browse.rs:4492), and the frozen bulk-guard snapshot must behave
   exactly as today. Sort, filter, search, refresh, hidden-toggle keep
   marks for paths still present.

If you conclude cross-directory marking should instead become a real,
deliberate feature (ranger-style global marks), do NOT build it here —
that requires visible affordances (persistent mark-count indicator,
destructive ops confirming with the full path list) and is out of scope.
This brief is the safety fix.

## Hard constraints

- **World-class and idempotent.** Same session, same inputs → same
  selection, every time. Navigating A→B→A leaves zero marks; re-running
  any clearing path twice is a no-op; the consumer-side filter is a pure
  function of (marks, current_dir) with no ordering dependence. State the
  invariant in a doc comment where it is enforced: *a mark is only ever
  visible-and-actionable in the directory that contains it.*
- Destructive operations (move/delete/rename) must be provably covered by
  tests that construct the incident: mark paths in dir A, navigate to dir
  B, mark paths in B, invoke the op → ONLY B's paths are acted on. One
  such test per destructive surface (move/copy source list, delete list,
  bulk-rename list) — behavioral, not source-text tripwires.
- The anchor (`multi_select_anchor`) and range-selection state must never
  outlive the marks they refer to.
- Archive browse: entering/exiting archives already clears marks — keep
  that, and make sure the new clearing composes with it (no double-clear
  panics, no ordering assumptions).
- Deterministic: no time, no randomness.
- Suite baseline: 2597 lib tests passing, 9 known pre-existing failures
  (docs/pre_existing_test_failures_triage_brief.md); zero warnings from
  `cargo check` and `cargo test --no-run`. Do not regress either number.
- The sandbox cannot compile; the applier fixes compile errors. Favor
  mechanically verifiable, minimal-surface changes. State intended
  behavior per change in tests.
- Existing tests that mark-then-navigate as *setup* may exist; if any
  break, fix the test only when its intent was incidental to the old
  persistence, and say so explicitly per test.

## Acceptance

- Incident reproduction (as a test AND run by the applier on a real tree):
  mark 5 folders in dir A, navigate to dir B, mark 5 folders, Move to...
  dest → exactly B's 5 folders move; dir A untouched.
- Same shape for Delete and bulk rename path collection (collection-level
  assertion is fine for delete; do not delete real files in the applier's
  manual run).
- Marks survive sort/filter/search/refresh within a directory; vanish on
  any navigation away; A→B→A round trip leaves zero marks.
- Queue collection (`collect_selection_for_queue`) never returns paths
  outside the current directory.
- Bulk-guard frozen snapshot still wins when set (existing test
  command.rs:13834 keeps passing).

## Files in this bundle

- `docs/stale_multiselect_cross_directory_brief.md` — this brief
- `src/tui/browse.rs` — BrowseState, marks, navigation, reset_nav_state,
  collect_selection_for_queue
- `src/tui/command.rs` — collect_selection_for_file_ops + all `: `command
  consumers, open_file_picker_for_copy_move, bulk guard
- `src/tui/context_menu.rs` — Move/Copy/Delete/rename/metadata actions
- `src/tui/keybindings.rs` — keyboard equivalents + guards
- `src/tui/event_loop.rs` — file-picker MoveTo/CopyTo execution
- `src/tui/draw_browse.rs` — mark rendering (visibility context)
- `src/tui/app.rs` — AppState, FilePickerPurpose, bulk_guard_frozen_paths

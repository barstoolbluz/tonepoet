# Follow-up hardening: async Browse Convert context-menu behavior

## Issue addressed

The async folder-expansion handoff already carried the post-expansion intent for context-menu Convert actions. That preserved the intended `Convert -> Last used` / `Convert -> preset` auto-commit behavior after a regular audio folder expands on the worker.

Two engineering gaps remained:

1. The highest-value context-menu regression coverage was still partly source-text tripwires instead of behavioral tests.
2. Pending folder expansion was cancelled on newer expansion requests and screen changes, but many ordinary Browse selection/navigation changes relied only on stale-result rejection at completion time. That was safe for state, but it allowed a large recursive walk to continue unnecessarily.

## Changes

### Behavioral context-menu tests

`src/tui/context_menu.rs` now includes async behavioral tests that execute the real context-menu dispatcher against a temp regular audio directory:

- `ContextAction::ConvertCustom` starts async expansion and completion publishes a Convert review source whose `all_paths()` are the expanded audio files, not the directory.
- `ContextAction::ConvertLastUsed` starts async expansion with a `Commit { start: false }` continuation; completion publishes the source and then commits it, leaving queued items for the expanded files.

The old `include_str!("context_menu.rs")` context-menu routing assertion was removed. The test now exercises the dispatcher, worker completion message, and completion reducer rather than checking source text.

### Stale-result behavioral coverage

`src/tui/command.rs` adds a behavioral stale-completion test. It starts a pending expansion for one directory, changes the Browse selection before completion, delivers the old completion, and asserts that no Convert source is published and no queue items are committed.

### Earlier cooperative cancellation

`AppState` now exposes `cancel_browse_convert_expansion_for_browse_change(reason)`, which cancels the active worker token and records an explicit status. Key Browse navigation/selection paths call it when the selection context changes:

- keyboard list movement, range selection, type-ahead, filter navigation/input;
- Browse entry activation and archive navigation;
- mouse selection, range extension, row click, toolbar back/forward/up/refresh, path navigation, and show-hidden/filter changes;
- context-menu Select/Select All/Invert/Deselect/OpenEntry navigation paths.

This does not replace generation and selection freshness checks. Completion still validates the pending generation/request and the current selection before publishing state or running a post-load commit. Cancellation is an efficiency improvement; stale-result rejection remains the safety boundary.

## Preserved behavior

- Regular filesystem audio folders still expand to deterministic, deduplicated audio-file paths before Convert publication.
- `Convert -> Custom` remains review-only.
- `Convert -> Last used` and preset continuation still run only after fresh expansion, successful source publication, and preset load when applicable.
- Valid disc/archive/CUE sources still route to their existing paths.
- Blu-ray structural checks, backend preflight before `bd_open`, Unix stderr redirection guard, and processor fail-closed behavior are unchanged.

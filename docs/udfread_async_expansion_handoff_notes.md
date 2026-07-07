# udfread crash follow-up: integrated async Browse folder expansion

Date: 2026-07-07

## Root-cause guarantee preserved

The prior root-cause fix remains intact: regular filesystem audio folders are not published as opaque Convert sources, Blu-ray routing uses structural candidate checks, `BlurayHandle::open` preflights before `bd_open`, Unix stderr redirection guards the narrow `bd_open` call, and the processor fails unsupported opaque sources instead of falling through to generic materialization.

This follow-up changes only the Browse regular-folder expansion lifecycle. It moves recursive filesystem traversal out of the raw-mode TUI/reducer path and into the existing Tokio mailbox/worker pattern used elsewhere by the app.

## Synchronous path that existed before

The previous async-hardening bundle still left recursive folder expansion reachable from reducer-side Browse handoff code:

- `execute_queue(...)` for Browse `Command::Queue`, which is also the actual context-menu path for `ConvertCustom`, `ConvertLastUsed`, and `ConvertWithPreset`;
- `install_convert_source_with_async_probe(...)` for direct Browse source load;
- `BrowseReturnTarget::ConvertQueue` for direct Browse queue load.

Those paths used cheap candidate detection, but still allowed the recursive walk to occur before control returned to the event loop. On very large trees, removable media, slow disks, or network mounts, that could block the raw-mode UI.

## Worker/message path now used

The shared entry point is `start_browse_convert_folder_expansion(...)` in `src/tui/command.rs`.

It performs only cheap candidate checks on the reducer path, records an active pending expansion in `AppState`, then dispatches recursive expansion via `tokio::task::spawn_blocking`. Completion returns through the existing TUI mailbox as `AppMessage::BrowseConvertExpansionComplete` and is reduced by `handle_browse_convert_expansion_complete(...)`.

The uploaded `app.rs` and `message.rs` have now been integrated into the bundle as real source files, not only as hand-off notes. `src/tui/app.rs` owns the pending expansion handle, generation id, and cancellation token. `src/tui/message.rs` contains the concrete `BrowseConvertExpansionComplete` variant.

The following Browse flows now use the same async worker path whenever a regular filesystem audio directory is present:

- context-menu Convert -> Custom;
- context-menu Convert -> Last used;
- context-menu Convert -> preset;
- Browse `Command::Queue` / `:queue`;
- direct Browse source load;
- direct Browse queue load;
- multi-selection containing regular audio directories.

Single supported audio files, valid Blu-ray/DVD-Audio/DVD-Video/SACD/CUE/archive sources, and selections without regular audio-directory candidates still route immediately through their existing paths.


## Follow-up: context-menu auto-commit/start continuation

The first async-expansion pass exposed a product-behavior regression: context-menu `Convert -> Last used` and `Convert -> preset` used to call `Command::Queue`, then immediately commit/start if Queue synchronously switched to Convert. Regular audio folders no longer switch synchronously because expansion now runs in a blocking worker, so the immediate `app.current_screen == AppScreen::Convert` check skipped the intended commit/start action.

This bundle fixes that by carrying a typed `BrowseConvertPostLoad` continuation in the pending expansion request. `Convert -> Custom` uses `ReviewOnly`; `Convert -> Last used` and `Convert -> preset` use `Commit { start }`. The shared queue finalization path runs the continuation after successful source publication for both synchronous non-folder selections and fresh async expansion completions. Stale, cancelled, failed, and empty expansion results never run the continuation.

## Generation and cancellation safety

`AppState::begin_browse_convert_expansion(...)` now:

- cancels any older pending Browse Convert expansion;
- increments the monotonic `probe_generation`;
- stores `PendingBrowseConvertExpansion { generation, request, cancel }`;
- returns a `CancellationToken` cloned into the blocking worker.

The worker checks the token before and during traversal. Screen changes and newer source/queue requests cancel the active token through `AppState::cancel_browse_convert_expansion()`.

`handle_browse_convert_expansion_complete(...)` accepts a completion only when all of these remain true:

- the completion matches the active pending generation and request;
- the generation still equals `app.probe_generation`;
- the app is still on Browse;
- the current Browse selection matches the sorted/deduplicated request snapshot.

Stale completions from older jobs cannot publish Convert state, queue items, or source metadata.

## Expansion semantics

The background expansion:

- uses the existing Browse audio classification helper;
- sorts and deduplicates deterministically;
- preserves real disc/archive/CUE sources as opaque inputs;
- enforces `BROWSE_CONVERT_FOLDER_EXPANSION_MAX_VISITED`;
- distinguishes empty supported-audio folders from traversal failures;
- surfaces walk errors instead of silently dropping them;
- never publishes an empty folder or failed scan as an opaque source.

## Tests changed/added

The previous source-text assertions were supplemented with state-level lifecycle tests in `src/tui/app.rs` proving that:

- starting expansion records the active generation and request;
- starting a newer expansion cancels and replaces the older one;
- stale completion bookkeeping cannot clear the current pending expansion.

The existing Browse/command tests still assert that:

- `execute_queue` starts the async expansion path before source publication;
- `execute_queue` no longer calls the blocking recursive implementation;
- completion checks generation and current selection before publishing;
- CUE sidecar metadata is recaptured only after freshness checks;
- regular audio folder expansion and deterministic multi-selection expansion remain behaviorally covered through the worker seam;
- valid Blu-ray directories remain non-expanded and continue through Blu-ray detection;
- regular audio folders still fail the Blu-ray backend-open predicate and backend preflight.

## Validation

The sandbox does not have `cargo`, `rustfmt`, or a complete Cargo workspace, so I could not run:

- `cargo fmt`;
- `cargo check`;
- unit/integration tests.

I did run source-level validation in the available partial bundle:

- verified brace balance in all modified Rust files;
- checked that all `BrowseConvertExpansion` constructors include the new cancellation state;
- checked that the event loop handles `BrowseConvertExpansionComplete`;
- checked that context-menu `Convert -> Custom` still routes through `Command::Queue`;
- checked that context-menu `Convert -> Last used` and `Convert -> preset` use the shared post-load continuation helper instead of an immediate Convert-screen check;
- checked tarball integrity after packaging.

See `docs/udfread_async_expansion_integration_fix.patch` for the integration delta against the previous async bundle plus the uploaded `app.rs`/`message.rs` files. See `docs/udfread_async_expansion_autocommit_fix.patch` for the follow-up auto-commit/start continuation fix.

## Follow-up: context-menu behavior tests and earlier cancellation

The post-expansion continuation was already present in the async request. This follow-up hardens the surrounding engineering:

- replaces the context-menu source-text tripwire with async behavioral tests for `ConvertCustom` and `ConvertLastUsed` on temp audio directories;
- adds a stale-completion behavior test that proves late folder-expansion results cannot publish Convert state or commit queued items after selection changes;
- adds `AppState::cancel_browse_convert_expansion_for_browse_change(...)` and calls it from key Browse keyboard, mouse, toolbar, filter, and context-menu selection/navigation paths so large scans are cancelled earlier instead of merely ignored at completion.

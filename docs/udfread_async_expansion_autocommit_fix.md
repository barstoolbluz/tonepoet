# udfread async Browse expansion: context-menu auto-commit continuation fix

Date: 2026-07-07

## Blocker fixed

The async Browse folder-expansion fix preserved `Convert -> Custom`, but regressed `Convert -> Last used` and `Convert -> preset` for regular audio folders.

Before this follow-up, those context-menu actions still performed the old synchronous pattern:

1. resolve whether conversion should start immediately;
2. dispatch `Command::Queue`;
3. immediately check `app.current_screen == AppScreen::Convert`;
4. commit/start if Convert source publication already happened.

That worked for direct audio files because `Command::Queue` still published the Convert source synchronously. It failed for regular audio directories because `Command::Queue` now starts `BrowseConvertExpansionComplete` work and returns while the app remains on Browse. The immediate Convert-screen check therefore skipped the intended commit/start continuation, and completion later opened only the Convert review source.

## Fix

`BrowseConvertExpansionTarget::ConvertReview` now carries a `BrowseConvertPostLoad` continuation:

- `ReviewOnly` for `:queue` and context-menu `Convert -> Custom`;
- `Commit { start }` for context-menu `Convert -> Last used` and `Convert -> preset`.

The shared Browse queue implementation now has one internal finalization path:

- non-folder selections publish synchronously and then run the same post-load continuation;
- regular audio-folder selections start async expansion with the continuation embedded in the pending request;
- fresh completion publishes the expanded Convert source and then runs the continuation;
- stale, cancelled, failed, or empty completions never commit/start.

`ContextAction::ConvertLastUsed` and `ContextAction::ConvertWithPreset` now call `execute_queue_with_post_load_commit(...)` instead of manually dispatching `Command::Queue` and checking `app.current_screen` immediately. This preserves existing product behavior for direct files and restores it for async-expanded regular folders.

## Root-cause guarantee preserved

The change does not weaken the original libudfread hardening:

- regular audio folders still expand to supported audio files before Convert publication;
- regular folders are never installed as opaque conversion sources;
- Blu-ray candidate checks still require structural BDMV markers;
- `is_bluray_backend_open_candidate` still rejects ordinary audio folders;
- `BlurayHandle::open` still preflights before `bd_open`;
- the Unix stderr guard around `bd_open` remains fail-closed;
- processor fallback for unsupported opaque folders remains explicit failure.

## Tests changed/added

Source-level regression coverage was updated to assert that:

- `Convert -> Custom` still uses the shared queue path and opens Convert for review only;
- `Convert -> Last used` and `Convert -> preset` no longer rely on an immediate `app.current_screen == AppScreen::Convert` check;
- those auto-commit actions use the shared queue continuation helper;
- `ConvertReview` async targets carry the post-load continuation;
- async expansion completion resumes the continuation only after freshness checks and successful source publication;
- synchronous non-folder queue publication and async folder completion both run through the same post-load continuation path.

## Validation

The sandbox still lacks `cargo`, `rustfmt`, and a complete Cargo workspace, so I could not run `cargo fmt`, `cargo check`, or Rust tests.

Validation performed in the partial source bundle:

- generated and inspected the follow-up diff;
- verified all `ConvertReview` constructors include `post_load`;
- verified `ConvertLastUsed` and `ConvertWithPreset` no longer contain the stale immediate Convert-screen check;
- verified async completion invokes `apply_browse_convert_post_load_action(...)` only after `finish_browse_queue_review_after_expansion(...)` succeeds;
- verified the original Blu-ray/backend hardening files were not changed by this follow-up;
- verified tarball integrity.

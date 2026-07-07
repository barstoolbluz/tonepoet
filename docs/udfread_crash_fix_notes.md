# UDF/libudfread Crash-on-Convert Fix Notes

Date: 2026-07-07

## Root cause

The rebuilt bundle confirmed that Browse context-menu Convert actions do not call `load_browse_selection_pub`. `ContextAction::ConvertCustom`, `ContextAction::ConvertLastUsed`, and `ContextAction::ConvertWithPreset` all dispatch through `Command::Queue`. The queue path could preserve a selected filesystem directory as one opaque conversion source instead of expanding a normal album folder to its supported audio files. That let later source detection/materialization treat the directory as an unknown source and eventually reach the Blu-ray/libbluray path, where libudfread writes directly to stderr.

## Routing fix

`Command::Queue` now expands regular filesystem audio folders before publishing the Convert source. `Convert -> Custom` still dispatches to `Command::Queue` for manual review. `Convert -> Last used` and `Convert -> preset` use the same Browse queue implementation through `execute_queue_with_post_load_commit(...)`, carrying the post-load commit/start continuation across asynchronous folder expansion. Command-mode `:queue` keeps the review-only behavior. Direct Browse source loading and ConvertQueue loading now call the same regular-folder expansion predicate/helper, so the context-menu path and direct path do not diverge.

The expansion helper preserves real disc and virtual/archive behavior: valid Blu-ray, DVD-Audio, and DVD-Video directories remain opaque disc sources; archive virtual directories are not expanded as filesystem folders; ordinary single audio files remain ordinary single sources. Expanded paths are sorted and deduplicated before publication. Empty regular audio folders produce an explicit status instead of falling through to disc/materializer routing. CUE sidecar metadata produced by the existing queue collection path is retained only for paths that survive source publication, avoiding stale directory metadata after expansion.

## Blu-ray hardening

`materializer_bluray::is_bluray_candidate` no longer allows explicit Blu-ray options to promote arbitrary directories. A directory must have a valid Blu-ray BDMV layout. Explicit Blu-ray intent can only disambiguate structurally valid sources. ISO handling still requires bounded Blu-ray markers before libbluray is called.

`bluray_utils::is_bluray_backend_open_candidate` is a bounded structural preflight used before backend opening. `BlurayHandle::open` calls it immediately before `bd_open`, so ordinary audio folders are rejected before libbluray/libudfread can run.

`BlurayHandle::open` also installs a fail-closed Unix stderr redirection guard only around `bd_open`. The guard serializes fd mutation with a process-wide mutex, saves fd 2 with `dup`, redirects fd 2 to `/dev/null` with `dup2`, restores stderr in `Drop`, and returns an explicit error if the guard cannot be installed. Non-Unix builds use a successful no-op guard.

## Processor fallback

The processor now fails queue items whose `detect_source_kind(&request)` returns no supported source kind. It no longer falls through to generic materialization. The error explicitly states that regular audio folders must be expanded into supported audio files before queue processing.

## Tests added

Focused coverage was added for:

- context-menu Convert dispatch using the shared Browse queue path;
- context-menu `Convert -> Last used` / preset preserving auto-commit/start after async folder expansion;
- regular audio folder expansion for direct Browse handoff;
- deterministic multi-selection expansion and deduplication;
- preserving valid Blu-ray directories as Blu-ray candidates;
- rejecting fake regular directories even with explicit Blu-ray fields;
- rejecting ordinary audio folders in `is_bluray_backend_open_candidate`;
- rejecting ordinary audio folders in the libbluray backend before `bd_open`;
- explicit processor failure for unsupported source kind rather than generic materialization;
- queue-source publication ordering so expansion happens before Convert source publication.

## Validation

This upload is still a partial source snapshot, not a complete Cargo workspace. The sandbox also does not have `cargo` or `rustfmt` installed, so I could not run `cargo fmt`, `cargo check`, or the Rust test suite. I performed static inspection, targeted grep checks for the known failure patterns, generated `docs/udfread_crash_fix.patch`, and verified the corrected archive contents.


## Follow-up hardening: async context-menu continuation and cancellation

The async request already carried `BrowseConvertPostLoad`, so `Convert -> Last used` and preset conversions resume commit/start after fresh expansion and source publication. This pass strengthens the implementation by adding executable context-menu tests and by cancelling pending folder-expansion workers on ordinary Browse selection/navigation changes. The existing freshness checks remain in place and still prevent stale completions from mutating Convert state or committing queued work.

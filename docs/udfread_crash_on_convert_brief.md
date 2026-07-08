# Crash: "udfread ERROR: ECMA 167 Volume Recognition failed" on Convert

## The Bug

User navigates to a regular folder containing FLAC files in the Browse screen, right-clicks, chooses "Convert", selects "Custom", and the TUI displays garbled text including `udfread ERROR: ECMA 167 Volume Recognition failed`. The app crashes.

This is a regular folder of audio files, not a Blu-ray disc, DVD-Audio, SACD ISO, or any disc format. The folder should go through the normal archive/single-file conversion path, not a disc probe path.

## What We Know

- The error comes from **libudfread**, a C library loaded by **libbluray** internally. It writes directly to stderr, which in raw terminal mode corrupts the TUI rendering.
- `bd_set_debug_mask(0)` at `bluray_backend_libbluray.rs:184` suppresses libbluray's own debug output but does NOT suppress libudfread's stderr writes.
- libbluray's `bd_open` at `bluray_backend_libbluray.rs:186` is the call that triggers libudfread internally.

## What We Don't Know

- Why the Blu-ray/disc probe path is being triggered at all for a regular FLAC folder. The convert action for a normal directory should not call `BlurayBackendLibbluray::open`.
- Whether the folder is being misclassified as a disc source somewhere in the convert/queue path.
- Whether `probe_disc_contents` (disc_browser.rs:421) is being called as part of the convert flow, or whether the materializer selection is routing the folder to `materializer_bluray.rs:55`.
- Whether stderr output from C libraries loaded via FFI can be suppressed globally (e.g., by redirecting fd 2 before the call and restoring after).

## Reproduction

Navigate to any folder containing regular audio files (FLACs). Right-click → Convert → Custom. The error appears and the app crashes.

## Files

- `src/disc/bluray_backend_libbluray.rs` — `bd_open`, `bd_set_debug_mask`
- `src/disc/bluray_utils.rs` — `is_bluray_source`, `is_bluray_iso`, `is_bluray_directory`
- `src/tui/disc_browser.rs` — `probe_disc_contents`
- `src/convert/pipeline/materializer_bluray.rs` — `BlurayBackendLibbluray::open` in materializer
- `src/tui/keybindings.rs` — convert action dispatch from context menu
- `src/tui/event_loop.rs` — convert flow message handling
- `src/convert/processor.rs` — source classification and materializer routing

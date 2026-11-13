# Issue #88 R5: localized corrective status (2026-11-13)

Base: audited R4 HEAD `bd0f1293ba0159866c3670e3e20c5e609c528a20`.
Scope: R5-01 and R5-02 only. The original brief and all prior correction histories remain in the archive.

## R5-01: DSF artwork metadata and peak storage

The separate-track and continuous native output paths call the same small metadata helper. DSF writes requested artwork before ordinary text tags, so the initial native ID3 append allocates room for the image plus the existing 1 MiB edit pad. Subsequent ordinary text updates can use the bounded in-place journal writer instead of copying the DSF audio into a second complete container. For non-DSF output the prior text-then-artwork order remains unchanged. This does not change the DFF/PCM metadata writer or the Convert pipeline.

The display/preflight estimate now includes the native DSF 1 MiB padding and the cover file length when discoverable beside the source, once per output, plus the largest transient ID3 tail journal for artwork-plus-text. It does not assume that a source cover invisible until materialization has a knowable size; build-host/media validation must compare the estimate against actual peak usage for such sources.

Focused Rust test: `large_dsf_cover_then_text_stays_in_place_and_preserves_audio` uses a valid >2 MiB JPEG on a real DSF fixture, checks the embedded APIC and title, checks the unchanged audio, and on Unix checks that the file inode does not change. `native_space_includes_known_dsf_artwork_and_writer_padding` covers both separate and continuous budget paths.

## R5-02: overwrite-aware preflight

All preflight names come from the worker's existing `proposed_destination` and `proposed_destination_for_program`; filename rendering and sanitization are not duplicated. For Skip, existing destinations are omitted from payload and transient requirements; a present continuous output skips its whole program. For Replace, the estimator follows sequential publications and the peak private output, subtracting capacity reclaimed when a completed previous target is atomically replaced. Existing Fail and PCM estimates are retained when no skip/replace reduction applies. The UI caches destination existence/size checks until selection or output paths change. Extract bypasses the cache and refreshes the inventory immediately before queuing the job.

Focused Rust test: `native_space_skip_and_replace_follow_the_workers_output_paths` covers three present and one missing SACD track, all-present Skip, continuous Skip, same-size Replace, Fail behavior, flat naming, subfolder naming, and fresh-on-launch inventory. The existing continuous PCM capacity regression remains in the build-host test selection.

## Verification boundary

**Completed in this environment:** source review, bounded independent arithmetic examples, diff whitespace check, Rust delimiter balancing, valid JPEG fixture decoding with Pillow, Bash syntax checking, archive integrity and patch replay (once packaged), and independent Python WAV-header reference tests.

**NOT completed here:** Rust compilation, rustfmt, Rust tests, actual SACD fixture extraction, real peak-capacity measurement, interactive TUI checks. `rustc`/`cargo` are unavailable and the Rust distribution host does not resolve. These are mandatory on a Rust >=1.93 build host before production handoff. Run `verification/verify_on_build_host.sh`, then test a four-track 60-minute DSD64 SACD with 2-3 MiB artwork (separate and continuous), Skip/Replace under limited free space, small/non-adjacent selections, native audio comparison and cancellation.

Do not reopen previous R3/R4 corrections without a reproduced failure. The existing editor's explicit Save behavior remains unchanged.

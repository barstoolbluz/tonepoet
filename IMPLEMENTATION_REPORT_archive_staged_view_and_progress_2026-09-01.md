# IMPLEMENTATION REPORT — Staged archive state: stale views, latched dirtiness, and transfer progress — CORRECTED R4

**Date:** 2026-09-01  
**Brief:** `BRIEF_archive_staged_view_and_progress_2026-09-01.md`  
**Starting corrective bundle:** `tonepoet_archive_staged_view_and_progress_2026-09-01_CORRECTED_R3_bundle.tar.gz`

## Scope

R4 is a narrow correction for one R3 defect: inline Browse edits of ordered-list metadata fields (`ARTIST` and `GENRE`) did not carry the exact ordered-value representation that R3's conservative dirty-state reducer requires.

R4 preserves the R3 reducer, sticky-unknown recovery semantics, staged-rename probe fallback, exact structural projection, metadata path-origin accounting, coalescing barriers, and all archive progress/preemption behavior. No archive format policy, archive tool selection, transfer FIFO, database schema, final install transaction, or recovery protocol changed.

## Defect

R3 correctly stopped using joined display strings to prove equality for ordered-list metadata. This prevents the false-clean collision between one stored member `['Alice; Bob']` and two ordered members `['Alice', 'Bob']`, both of which display as `Alice; Bob`.

However, inline Browse completion still emitted only scalar `value` / `original_value`. `ARTIST` and `GENRE` are both available in Browse's inline editor and both belong to the ordered-list contract, so even a simple `Alice -> Bob -> Alice` sequence ended with no exact vectors and therefore remained conservatively dirty forever.

## Correction

The correction keeps R3's exact-vector requirement intact.

- `MetadataWriteComplete` now carries optional `ordered_values` and `original_ordered_values` alongside the existing scalar fields.
- Only when the target is a staged archive member and the inline field is set-valued, the blocking metadata worker reads the exact pre-write ordered values through `read_all_tags()` / `TagEntry` / `MetadataFieldValues`.
- This extra read stays off the TUI thread and is not performed for Title, Album, Year, filesystem Browse, cursor movement, probing, or ordinary archive browsing.
- The post-write exact representation is derived from the scalar inline writer's actual semantics: an empty scalar deletes the field (`[]`); any non-empty scalar produces exactly one stored member (`[value]`). Delimiters are not parsed into multiple values.
- On successful completion, archive staging records set-valued inline edits through `StagedArchiveMetadataChange::field_with_ordered_values()` when both exact sides are known.
- If the exact pre-write read fails, the write can still complete, but staging falls back to an unknown exact baseline and therefore remains conservative-dirty. Scalar display equality is never used as a substitute.

This composes with the existing R3 coalescer exactly as intended: the first exact archive baseline is retained, later inline writes replace only the exact final vector, and an old recovery row whose exact original baseline is unknown can never acquire a fabricated baseline from staged state.

## Focused verification added

`archive_inline_artist_producer_tracks_exact_ordered_values_and_real_revert_is_clean` exercises the real async inline producer and completion path against a staged FLAC fixture:

1. seed exact `ARTIST=['Alice']`;
2. install the archive Browse/staging context and an identity-valid Browse probe;
3. call the production `apply_text_edit()` path for `Alice -> Bob`;
4. assert the emitted `MetadataWriteComplete` carries exact `['Alice'] -> ['Bob']` vectors;
5. pass that completion through the normal event-loop reducer/recording path;
6. simulate the ordinary post-write reprobe with scalar `Bob`;
7. call the production inline path for `Bob -> Alice`;
8. assert exact `['Bob'] -> ['Alice']` vectors;
9. process the second completion normally;
10. reread the staged file and confirm exact `ARTIST=['Alice']`;
11. reconcile archive staging and assert the session is clean.

The same test includes a lightweight assertion that inline `GENRE` is classified through the same ordered-value producer.

R3's existing regressions remain unchanged and continue to prove that:

- `['Alice; Bob']` versus `['Alice', 'Bob']` stays dirty despite equal display strings;
- restoring the exact original ordered representation becomes clean;
- old recovered `MetadataWrite` rows without exact ordered vectors remain conservatively dirty and cannot backfill an unknown baseline.

## Files changed relative to R3

- `src/tui/message.rs`
- `src/tui/probe.rs`
- `src/tui/keybindings.rs`
- `src/tui/event_loop.rs`
- this implementation report

No `browse.rs` reducer changes were required.

## Verification performed in this environment

The supplied brief states that this container has no Rust toolchain or Nix. This environment has no `cargo`, `rustc`, `rustfmt`, or `nix`, so the Rust/Nix gate was not run and no compile/test-pass claim is made.

Artifact/static verification performed for R4:

- checked the R3 -> R4 directory diff and confirmed code changes are limited to the four intended TUI files;
- ran `git diff --check` against the R3 base with no whitespace-error report;
- generated an R3 -> R4 corrective patch;
- generated a full original-bundle -> R4 patch;
- dry-run applied and then applied both patches to fresh extractions of their stated bases;
- byte-compared the patched code and report against the R4 worktree;
- extracted the final R4 bundle and byte-compared its changed files against the worktree;
- generated a SHA-256 checksum for the final bundle.

The operator should run the repository's normal Rust/Nix gate in the intended development environment.

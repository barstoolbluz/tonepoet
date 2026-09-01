# Implementation report - archive locality, throughput, and supervised spawn

**Date:** 2026-09-01
**Brief:** `BRIEF_archive_locality_and_spawn_2026-09-01.md`
**Base:** `main` @ `77d55bd` as recorded by the brief
**Baseline bundle:** supplied `tonepoet_archive_locality_and_spawn_2026-09-01_bundle.tar.gz`
**Corrective revision:** R2 - 7z fallback selection fixed; speculative Section C case normalization removed

## Summary

This round implements the smallest coherent correction for the confirmed locality/spawn defects, plus the two corrective findings from review:

1. archive edit work on network/FUSE storage is localized before seek-heavy extraction or native mutation, and verified replacement archives are built locally before one large sequential copy back to a same-directory install temporary;
2. direct supervised tool execution resolves bare executable names through `PATH` before canonicalization, fixing the production `xorriso` spawn failure; production-style multi-candidate 7z selection now also chooses the first candidate actually present on `PATH` instead of blindly selecting `7zz`;
3. the speculative Section C case-normalization behavior has been removed. Browse and staged mutation paths again use exact archive-path spelling, so explicit user-created or case-only-renamed entries such as `artwork` remain visible and selectable beside `Artwork`. The original Animals `Artwork`/`artwork` observation remains intentionally unresolved until the raw extracted tree is field-inspected.

The implementation does not redesign archive commits, does not implement `OUTSTANDING_ISSUES.md` #22 or #23, does not change read-only mount behavior, and does not add a new capacity-reservation subsystem or transfer-queue integration.

## A. Remote archive mutation work is local-first

### Existing staging location retained

Archive edit staging already lives under the process temporary directory. This round reuses that established locality rather than introducing another configurable scratch root.

For an edit source classified as remote by the existing Browse filesystem classifier (`nfs`, `cifs`, `sshfs`, arbitrary `fuse.*`, and the other already-recognized remote types), Tonepoet now creates a deterministic companion directory beside the extracted staging directory:

```text
<temp-parent>/.<staging-name>.tonepoet-local-source/
```

The source archive copy lives outside the extracted staging tree, so it cannot appear in Browse or be accidentally included in a repackaged archive. Cleanup is idempotent and is wired into the existing archive-staging cleanup function used by normal completion, cancellation, discard, and error paths.

### Sequential source transfer before extraction

`extract_archive_to_staging_with_progress` accepts an explicit locality decision from the existing filesystem classifier. For remote edit sources it now:

1. copies the archive sequentially to a UUID `.partial` file in the local companion directory;
2. syncs and byte-count verifies the copy through the existing large-file copy primitive;
3. renames the completed partial to the stable local source path;
4. runs the existing extraction pipeline against that local copy rather than against sshfs/FUSE.

The copy primitive keeps the existing performance hierarchy: Linux reflink when possible, `copy_file_range` in bounded chunks when supported, then an 8 MiB buffered sequential fallback. The UI callback is polled at 250 ms from an atomic byte counter, avoiding a high-frequency event-channel send from the blocking copy loop.

This affects the full-extraction edit paths for metadata, delete, rename fallback, and ISO-WV create. Read-only archive preview/mount paths are intentionally unchanged.

### Local repackage and sequential copy-back

When a remote edit session has a local source companion, full save/repackage now creates and verifies the replacement archive in that local work directory. Encryption-policy inspection also reuses the local source copy instead of re-reading a potentially seek-sensitive remote archive.

After successful creation and verification, Tonepoet copies the replacement sequentially to a same-parent install temporary on the source filesystem. Only that source-filesystem temporary participates in the established backup/install/restore transaction, so the final replacement remains a same-filesystem rename operation.

Local archives retain the previous one-temporary path: there is no extra localize/copy-back pass for ordinary local storage.

### Native rename receives the same locality policy

The format-native 7z/ZIP and ISO-WV rename path previously made its transactional copy beside the source archive. On sshfs this still forced the native tool to work against a remote file.

For remote sources the transactional copy now lives in the local temporary directory. The native tool edits and verifies that local copy, then Tonepoet performs one sequential copy to a same-parent install temporary and enters the unchanged atomic backup/install/restore transaction.

The existing native ISO-WV CUE repair and adjacent Tonepoet metadata-snapshot synchronization remain in the same relative transaction order. No audio extraction is added to the native path.

### Conflict safety across long transfers

The existing UI edit-session conflict check remains unchanged. In addition, full repackage captures a source fingerprint after that admission and checks it:

- after local build/verification and before copy-back;
- again after the potentially long copy-back and before installation.

Native rename likewise retains its original expected-fingerprint check and adds a post-copy-back recheck for localized transactions.

Cancellation remains effective through source localization and copy-back. Once the verified source-filesystem install temporary has passed the final conflict/cancellation checks, installation is deliberately non-cancellable, preserving the safer existing rule that the same-filesystem backup/install/restore transaction is completed rather than interrupted mid-swap.

### Diagnostics

Archive edit localization, extraction completion, native localization, and replacement copy-back now emit informational timing/byte/rate logs. This gives field testing concrete evidence for which source was used and where elapsed time was spent without adding per-chunk logging overhead.

Any performance comparison still needs to record cold versus warm source state, as required by the brief; this implementation cannot make page-cache state observable or comparable by itself.

## B. Bare production tool names resolve through PATH before supervised launch

`run_supervised_with_stdio` no longer canonicalizes the caller-supplied `binary_path` directly. It first calls the existing `resolve_command_launch_path` helper, then canonicalizes and opens the resolved executable for the supervisor's existing identity/safety checks.

This makes direct supervised execution consistent with `RealToolRunner::tool_version` and the other runner paths that already resolve a bare executable name before use.

The production archive repackaging case now works as intended when `tool_paths` is empty:

```text
xorriso -> PATH-resolved absolute path -> canonicalize/open -> supervised execution
```

The same boundary correction applies to bare `7zz`/`7z` direct repackage launches. Configured absolute or explicit relative paths keep their existing behavior.

A focused Unix test calls `run_with_binary_path` with bare `sh`, supplies a working directory that deliberately does not contain `sh`, and requires the process to exit successfully. This exercises the exact branch the production archive path previously missed.

### Multi-candidate 7z selection now matches the advertised fallback

`repackage_tool_path()` still preserves explicit configured-path precedence exactly as before. When no configured candidate exists, it now walks the supplied bare names in order and returns the first candidate for which the existing `command_path_available()` check succeeds. Only if none are available does it retain the historical `names[0]` fallback so missing-tool error behavior stays unchanged.

For `&["7zz", "7z"]`, production-empty `tool_paths` therefore behaves as intended:

- `7zz` and `7z` present -> select `7zz`;
- only `7z` present -> select `7z`;
- neither present -> retain `7zz` as the diagnostic fallback, after which the existing availability check reports the established `7zz or 7z` error.

The deterministic regression test runs the test binary as a child with a controlled `PATH`; it does not mutate process-global `PATH` in a concurrently running test process. The child verifies selector output, ZIP/7z preflight admission, and ZIP/7z native-rename admission with an empty production-style tool map.

## C. Speculative case normalization removed

The brief explicitly recorded the Animals `Artwork`/`artwork` explanation as an unverified hypothesis. The first implementation nevertheless introduced unique ASCII-case-insensitive path reconciliation across Browse, probes, tag extraction, rename/delete validation, and create/rename destination-parent resolution. Review identified a deterministic regression: an explicit user-created `artwork` beside logical `Artwork`, or a case-only staged rename `Artwork -> artwork`, could be suppressed or resolved back to the old logical spelling even though the staged bytes that would be saved contained the user-authored spelling.

That behavior has been fully cut back for this round.

- staged Browse deduplication is exact-path only again;
- staged filesystem resolution is exact spelling again;
- rename/delete validation uses the established exact staged path;
- create/rename target collision checks use the established exact destination path;
- recursive search, audio probes, and tag extraction no longer perform case-insensitive staged fallback.

No edit-aware case overlay or extractor-normalization namespace has been added. That would be materially broader than the evidence supports. The raw extracted Animals staging tree should be captured in the qualified field environment before any future normalization behavior is designed.

Focused view regressions now prove that a case-sensitive staging tree keeps an explicit staged `artwork` visible and selectable beside logical `Artwork`, that the post-state of a case-only rename keeps the new spelling visible/selectable, and that a source archive containing two genuine case-distinct logical entries exposes both.

## Tests added or extended

- `run_with_binary_path_resolves_bare_program_before_cwd_lookup`
  - direct supervisor bare-name PATH resolution with a non-matching cwd.
- `production_style_seven_zip_selector_honors_all_path_candidates`
  - controlled child-process `PATH` proves `7z` fallback, `7zz` preference, empty-map ZIP/7z preflight, native rename admission, and configured-path precedence.
- `edit_extraction_uses_localized_archive_source_when_requested`
  - byte-exact local source copy, extraction command points at the local copy, and copy/extraction progress is emitted.
- `local_archive_source_cache_is_outside_staging_and_cleanup_is_idempotent`
  - companion cannot be repackaged and repeated cleanup is harmless.
- `staged_archive_view_preserves_explicit_case_distinct_create`
  - logical `Artwork` plus explicit staged `artwork` both remain visible; `artwork` is selectable under its authored spelling.
- `staged_archive_view_keeps_case_only_rename_spelling_selectable`
  - a staged `artwork` post-state against a stale logical `Artwork` listing remains visible/selectable rather than being swallowed by case folding.
- `archive_listing_preserves_genuine_case_distinct_names`
  - genuine logical `Artwork` and `artwork` entries remain distinct.

Existing native real-tool tests were updated only for the new explicit `localize_transaction` argument and continue to exercise the local-storage path when the qualified environment provides the tools.

## Files changed

- `src/convert/pipeline/materializer_archive.rs`
- `src/convert/pipeline/tool.rs`
- `src/tui/app.rs`
- `src/tui/browse.rs`
- `src/tui/keybindings.rs`
- `IMPLEMENTATION_REPORT_archive_locality_and_spawn_2026-09-01.md` (new)

No other baseline source/configuration file was intentionally modified.

## Scope intentionally not taken

- No deferred-commit/virtual-view redesign.
- No implementation of `OUTSTANDING_ISSUES.md` #22 or #23.
- No read-only archive-mount localization; the brief explicitly leaves that decision open and lazy remote reads may already be acceptable.
- No new disk-capacity planner/reservation layer. Local-source or local-repackage ENOSPC is reported by the existing filesystem operation, partial output is cleaned, and the original archive is not installed over.
- No automatic use of the general file-transfer queue, preemption model, journal recovery, or minimized-by-default policy. The brief identifies those as UX/design questions, not requirements; importing those semantics here would materially widen integration cost.
- No extractor-case normalization is implemented in this round; the original Animals artifact remains a field-investigation item until the raw extracted tree is recorded.

## Verification performed in this environment

The brief states that the implementation container lacks Rust, Nix, and archive executables. That is true in this runtime: `cargo`, `rustc`, `rustfmt`, `nix`, `7z`, `7zz`, and `xorriso` are unavailable.

Therefore **no claim is made that compilation, formatting, the Rust test suite, Nix evaluation, real-tool archive tests, or sshfs throughput measurements were executed here**.

Static/differential checks performed before packaging:

- compared the working tree against a fresh extraction of the supplied baseline;
- `git diff --no-index --check` reports no whitespace-error hunks;
- scanned all modified Rust files with a string/comment-aware delimiter-balance checker;
- scanned modified source/report paths for merge-conflict markers;
- reviewed every call site changed by the native-rename signature and extraction-locality argument;
- reviewed staging cleanup paths to keep the local companion covered by the existing session cleanup boundary;
- verified the existing remote-filesystem classifier includes `sshfs`, `fuse`, and `fuse.*`;
- packaged and independently re-applied the generated patch to a pristine baseline copy, then compared the result byte-for-byte/tree-for-tree with the implementation tree;
- extracted the generated source bundle independently and compared it tree-for-tree with the implementation tree;
- verified the gzip archive integrity and generated SHA-256 checksums for the deliverables.

## Operator gate

Run in the repository's qualified development environment:

```bash
cargo fmt --check
cargo test --workspace --no-fail-fast
```

Useful focused tests before/after the full gate:

```bash
cargo test -p tonepoet --lib run_with_binary_path_resolves_bare_program_before_cwd_lookup
cargo test -p tonepoet --lib edit_extraction_uses_localized_archive_source_when_requested
cargo test -p tonepoet --lib local_archive_source_cache_is_outside_staging_and_cleanup_is_idempotent
cargo test -p tonepoet --lib production_style_seven_zip_selector_honors_all_path_candidates
cargo test -p tonepoet --lib staged_archive_view_preserves_explicit_case_distinct_create
cargo test -p tonepoet --lib staged_archive_view_keeps_case_only_rename_spelling_selectable
cargo test -p tonepoet --lib archive_listing_preserves_genuine_case_distinct_names
cargo test -p tonepoet --lib native_iso_wv_real_rename_repairs_cue_and_snapshot_without_extracting_audio
cargo test -p tonepoet --lib native_zip_real_multi_pair_rename_handles_archive_without_directory_records
```

The real-tool filters self-skip or require the qualified environment's archive executables according to their existing setup.

## Field acceptance checks for the brief

1. In a fresh Nix shell with production-empty `tool_paths`, save an ISO-WV and confirm `xorriso` launches from `PATH`. Separately test a PATH containing `7z` but no `7zz`: ZIP/7z preflight, native rename admission, and an actual tiny save should use `7z`. With both installed, confirm `7zz` remains preferred.
2. On sshfs, start a first edit against a cold multi-gigabyte archive. Confirm the log records `localized archive edit source` and the extraction log's `extraction_source` points to local temporary storage.
3. State cold/warm cache state explicitly for every timing. Compare source-copy time plus local extraction against the prior cold remote-extraction baseline.
4. Save that staged edit. Confirm create/verify work remains local, the log records one replacement copy-back with byte count/rate, and the final archive is installed through the existing same-parent transaction.
5. Modify the original archive externally while the replacement is being copied back. Save must refuse installation after the post-transfer fingerprint check and leave the external version in place.
6. Cancel during source localization and during replacement copy-back. The original must remain untouched and `.partial`/install temporaries must be cleaned.
7. Exercise insufficient local scratch space. The operation should fail on copy/build without entering archive installation; after restoring space, retrying the staged edit should remain valid.
8. Revisit the Animals ISO and capture the raw extracted staging tree immediately after extraction, before Browse merging. Record whether 7z writes `Artwork`, `artwork`, or both. Do not reintroduce case-normalization behavior until that observation establishes the extractor-side cause; meanwhile verify explicit staged `Artwork` + `artwork` edits remain separately visible/selectable.

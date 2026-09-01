# Implementation report — Archive access and structural edits

**Date:** 2026-08-31  
**Brief:** `BRIEF_archive_access_and_structural_edits_2026-08-31.md`

## Summary

This implementation removes whole-archive extraction from the common rename path for ISO-WV, 7z, and ZIP while preserving Tonepoet's existing exact install/restore transaction semantics. It also generalizes the existing ISO-WV read-in-place conversion shape to non-ISO archives through `fuse-archive` with a conservative extraction fallback, adds visible progress to unavoidable Browse extraction paths, allows navigation away while preparation is in flight, and adds the requested `:l` / `:list` forced archive-listing command.

The implementation deliberately does **not** mutate the user's only archive copy in place. Native rename runs against a transactional sibling copy; the finished archive is installed through the existing backup/install/restore swap. On Linux the copy path attempts, in order, filesystem reflink (`FICLONE`), `copy_file_range` (which can use kernel or server-side offload), and a cancellable buffered copy. This retains exact rollback rather than replacing it with an inverse archive operation whose bytes/hash may not reproduce the original.

## Implemented outcomes

### 1. Native archive rename without extraction

`src/convert/pipeline/materializer_archive.rs` now provides a format-explicit native rename path:

- **7z / ZIP:** `7z rn -spd -- ...` on the transactional copy. `-spd` disables wildcard interpretation so member names containing `*` or `?` are literal.
- **ISO-WV:** `xorriso -dev ... -mv ... -- -commit` on the transactional copy.
- **TAR / TAR.GZ:** retain extraction/repackage fallback; no new unproven native mutation path.
- **RAR:** no native rename path. Tonepoet preserves the existing exact RAR repackage path when a configured RARLAB `rar` writer is available. If no writer is available, mutation is refused before extraction; Tonepoet never silently changes the container format.
- **Encrypted archives:** deliberately retain the proven password-aware extraction/repackage path. Native/header-only mutation is not assumed safe for encrypted or header-encrypted containers.
- **Implicit archive directories:** for 7z/ZIP, Browse converts synthesized prefix-only directories into a native multi-member rename plan; formats without a suitable native primitive retain extraction/repackage fallback.

The native path acquires the same cross-session exact WRITE mutation claim semantics used by other archive mutations before preparing/installing the replacement.

### 2. Generic archive read-in-place materialization

The conversion materializer now attempts a read-only `fuse-archive` mount for non-ISO archives before extraction. The mount uses **lazy caching** so mount acquisition itself does not eagerly decompress the whole archive.

The mount option is version-aware:

- `< 1.14`: capability miss; fall back to extraction because `lazycache` is unavailable.
- `1.14 .. 1.19`: `lazycache,auto_unmount` (these versions predate automatic tree trimming).
- `>= 1.20`: `lazycache,notrim,auto_unmount` so mounted paths preserve the archive member tree despite the newer default trimming behavior.
- unknown version, missing binary, missing `/dev/fuse`, unsupported archive, mount error, or readiness timeout: soft capability miss and existing extraction fallback.
- cancellation remains terminal rather than being converted into fallback work.

`fuse-archive` is added to `flake.nix` on Linux and to the pipeline tool/version inventory. The implementation is Linux-only; other platforms keep the extraction fallback.

### 3. Convert preview no longer forces extraction first

The TUI's Convert preview previously extracted a generic archive before `ArchiveMaterializer` ran, which would have defeated a materializer-only mount optimization. Preview now attempts the same generic read-only mount for unencrypted archives and holds its FUSE lease for the preview lifetime. Mounted previews are intentionally not transferred to a queued conversion; queueing drops that lease and the pipeline establishes its own mount under its own staging lifetime.

### 4. Slow Browse paths report progress and do not trap navigation

Metadata edit, delete, rename fallback, and ISO-WV create extraction now use a Browse-facing progress wrapper around the existing extractor. It emits coarse progress from staged regular-file bytes without altering the established extraction implementation. When the archive listing exposes a total uncompressed size, the UI reports an approximate percentage; otherwise it reports extracted MiB plus elapsed seconds.

In-flight archive edit **preparation** no longer pins the user in Browse. Switching screens, tabs, or leaving the archive cancels and detaches the pending preparation. Late worker completions are rejected by ownership checks and clean only their own staging. A native rename that crossed the install point before cancellation is treated as a real committed mutation and refreshes relevant archive state rather than being silently ignored.

A stale-result race was explicitly guarded: an old cancelled worker must not delete the archive-keyed durable recovery row belonging to a newer edit session on the same archive.

Dirty archive staging is unchanged: once a user edit actually exists, the existing deferred-save/repackage lifecycle still owns preservation and save semantics.

### 5. Archive listing override

The broken “press `l`” guidance is replaced with the requested command-mode override:

- `:l`
- `:list`

Both force listing of the selected archive from Browse and bypass the remote/disabled listing policy gate. Plain letter keybindings remain untouched, preserving type-ahead behavior.

### 6. Archive directory rename UI

Archive directories can now enter the same archive-aware rename path as files. Context-menu and inline-rename eligibility were updated; unrelated filesystem operations remain hidden inside archive listings.

## Due-diligence corrective follow-up

Three post-implementation defects were confirmed and corrected without changing the archive transaction architecture.

### Native ISO-WV rename keeps CUE geometry authoritative

The native ISO branch now target-reads the single visible CUE from the transactional ISO copy, using no audio extraction. The archive-relative FILE remapping logic is shared with the established staged rename path, and the existing byte-preserving CUE rewrite machinery is reused for encoding/BOM, quoting, CRLF/LF, legacy unquoted FILE forms, and unrelated bytes. After `xorriso -mv`, any required CUE rewrite is mapped back into the transactional image and target-read again for an exact byte check before install. Malformed, ambiguous, missing-target, or otherwise unsafe CUE cases decline the native optimization and continue through the existing extraction fallback with the user's original archive untouched.

A Tonepoet-owned adjacent `.iso.wv.cue` metadata snapshot is synchronized with the same FILE replacements while preserving its metadata. Its write claim is held through the archive swap; if archive installation fails, the exact original sidecar bytes are restored. An adjacent CUE without the Tonepoet snapshot marker is not rewritten.

### Implicit 7z/ZIP directories use native multi-member rename

Browse-synthesized directories that have descendants but no explicit directory member no longer force extraction for 7z/ZIP. The already-loaded archive listing is converted into literal descendant `(old,new)` pairs and one `7z rn -spd -- ... old1 new1 old2 new2 ...` operation is applied to the transactional copy. Existing destination/subtree collision checks still run before mutation. Explicit directories retain the existing single-pair native path.

### Encrypted fallback writeback preserves protection or refuses

The slow edit path is now password-aware on both read and write. Before a replacement archive is created, Tonepoet probes the original container's reproducible encryption policy. 7z data-only versus header encryption is recreated with `-p` plus `-mhe=off/on`; supported ZIP ZipCrypto/AES strength is retained with the corresponding 7-Zip method switch; RAR data-only versus header encryption is retained with `-p` versus `-hp`. Mixed or unsupported encryption policies fail closed before install and leave staged edits available. The raw password remains in memory only and every password-bearing external-tool argument is indexed in `secret_args`; the direct policy probe suppresses stderr and never formats the password into diagnostics.

## Transaction and failure semantics

Native rename never edits the user's original archive in place. The sequence is:

1. Acquire exact cross-session mutation claim.
2. Re-check the source fingerprint.
3. Create a transactional sibling copy (reflink -> kernel/server copy offload -> buffered fallback).
4. Apply the format-native rename to that copy.
5. Verify the rewritten container header/tree without decompressing all payload data.
6. Re-check the original fingerprint.
7. Preserve install metadata.
8. Use the existing original->backup, replacement->original, restore-on-install-failure transaction.
9. Report non-fatal backup-cleanup / metadata-preservation warnings through the existing `ArchiveRepackageReport` shape.

This is intentionally more conservative than direct `7z rn` / `xorriso -commit` against the user's only file. It preserves the brief's existing exact rollback model. On filesystems supporting reflink or copy offload, preparation avoids pulling the whole archive through userspace; on filesystems that support neither, exact rollback can still require a full sequential copy, with visible progress and cancellation before the install boundary.

## Files changed

- `flake.nix`
- `src/convert/cue_parser.rs`
- `src/convert/pipeline/materializer_archive.rs`
- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/tool.rs`
- `src/convert/pipeline/types.rs`
- `src/tui/app.rs`
- `src/tui/command.rs`
- `src/tui/context_menu.rs`
- `src/tui/event_loop.rs`
- `src/tui/keybindings.rs`
- `src/tui/message.rs`

## Verification performed in this environment

The brief states that the implementation container cannot run the project test gate and that verification is the operator's job. This environment also has no `cargo`, `rustc`, `rustfmt`, `nix`, `7z`/`7zz`, `xorriso`, or `fuse-archive` executable, so no claim is made that the Rust test suite or real archive integration commands were executed here.

Static verification performed before packaging:

- reviewed every changed hunk against the supplied baseline bundle;
- checked all changed files for merge-conflict markers and trailing whitespace;
- checked the newly-added `ToolBinary::FuseArchive` against relevant exhaustive tool-name/version mappings and cross-references;
- performed a delimiter-balance scan over all changed Rust files;
- added focused unit coverage for fuse-archive version option selection, native rename format capability, transactional copy exactness/pre-cancellation, RAR writer refusal, command parsing for `:l`/`:list`, archive-directory rename exposure, stale archive-edit completion ownership, native ISO-WV CUE remapping/byte preservation/snapshot rollback, implicit ZIP descendant-pair planning, and encryption-policy parsing/fail-closed behavior;
- added real-tool integration tests which self-skip when their executables are unavailable for ISO-WV CUE/snapshot native rename and safe decline, implicit-directory ZIP multi-pair rename, encrypted 7z/ZIP writeback, and encrypted RAR writeback; these tests were not executable in this container;
- verified current upstream `fuse-archive` documentation/release history for `--version`, `lazycache`, and the v1.20 `notrim`/tree-trimming change while designing the version compatibility path.

## Operator gate recommended

Run the normal repository formatter/build/test gate in the project's qualified Nix environment, then exercise at least these integration cases with real tools/files:

1. Native ISO-WV `album.wv -> renamed.wv`, including quoted and unquoted/trailing-whitespace FILE lines, directory moves, CUE-only moves, CUE+audio moves, unrelated artwork, and a Tonepoet `.iso.wv.cue` metadata snapshot.
2. Force an ISO-WV CUE safety decline/rewrite failure and confirm the original ISO and any Tonepoet metadata snapshot remain byte-for-byte unchanged before fallback.
3. ZIP with no explicit directory entries: rename a synthesized folder, verify one native multi-pair rename, nested descendants re-parented, payload hashes unchanged, and an existing target subtree rejected without modification. Repeat for 7z if a convenient implicit-directory fixture is available.
4. Encrypted 7z with visible headers and with `-mhe=on`; verify correct password succeeds after save, wrong/no password fails as appropriate, and header visibility is unchanged.
5. Supported encrypted ZIP modes, including the mode/strength detected from `7z l -slt`, and RAR `-p`/`-hp` when the configured writer is present. Confirm no password appears in sanitized command records/logs.
6. An unencrypted 7z/ZIP/RAR repackage to confirm its existing creation command and behavior are unchanged by the encryption policy boundary.
7. Cancellation/navigation during a large transactional copy and during extraction fallback.
8. Generic 7z/ZIP/RAR conversion preview + conversion through the FUSE mount, confirming member paths match the archive listing and payload hashes match extracted equivalents.
9. FUSE unavailable/unsupported -> extraction fallback; encrypted archives continue to skip FUSE and use password-aware extraction.
10. RAR mutation with configured `rar` writer, and refusal without it.
11. `:l` / `:list` override with archive listing set to Never and with a remote-filesystem Auto refusal.

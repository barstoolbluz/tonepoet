# Implementation report — R3 corrective: archive access and structural edits

**Date:** 2026-08-31  
**Brief:** `BRIEF_archive_access_R3_corrective_2026-08-31.md`  
**Baseline:** supplied `tonepoet_archive_access_R3_corrective_2026-08-31_bundle.tar.gz`

## Summary

This corrective round addresses only defects A–D from the R3 brief. It does not redesign the archive transaction architecture and it does not implement `OUTSTANDING_ISSUES.md` #22 or #23.

The changes preserve the R2 native-rename transaction, exact install/restore semantics, extraction fallbacks, password-aware writeback policy, and navigation cancellation model. The corrections are deliberately local:

1. classify 7z directory records from `Attributes = D ...` when `Folder =` is absent;
2. keep ISO-WV target-read and rewritten-CUE staging on separate disk paths so xorriso's restored read-only mode cannot block the rewrite;
3. preserve navigation guards while reporting a wrong-password completion as a password failure rather than falsely as navigation/overlay cancellation;
4. complete the `Extracting archive:` -> `Preparing archive:` wording change at the three stale sites recorded by the brief.

## A. Encrypted 7z directories no longer look like plaintext members

`ArchiveEncryptionListingParser` still treats `Folder = +/-` as authoritative when the field exists. When it is absent, the parser now falls back to the first 7-Zip `Attributes` token and classifies records beginning with `D` as directories.

This keeps the fail-closed mixed-encryption policy intact. The change does **not** reinterpret a plaintext file as safe: it only prevents an unencrypted directory record from setting `any_unencrypted` when 7z omits the `Folder` field.

A focused unit test reproduces the R3 listing shape exactly: one `Attributes = D drwx...` directory with `Encrypted = -`, followed by encrypted regular files with `Attributes = A -rw...`. The resulting facts contain encrypted payload members and no plaintext payload member.

The existing real-tool test `repackage_archive_preserves_real_7z_and_zip_encryption_when_7z_is_available` remains the integration proof for the actual encrypted 7z/ZIP save path.

## B. Native ISO-WV CUE repair no longer writes over xorriso's read-only extraction

The original R2 path target-read the CUE with xorriso and then reused the extracted path as the source file for the rewritten CUE. Because xorriso restores the ISO member's recorded mode, that path can be mode 0444 and `fs::write` fails with `EACCES`.

The corrective path now uses two guarded sibling temp paths:

- a **read target** used only for xorriso `-extract_single` and decoding the original CUE;
- a distinct **rewrite/map source** created by Tonepoet with `fs::write` when the CUE bytes change.

The xorriso native rename then maps the writable rewrite source into the transactional ISO exactly as before. After commit, the same map-source pathname can safely be removed and reused by the existing target-read verification. No audio extraction is introduced.

This avoids chmod/permission mutation entirely, which is preferable for archives stored on filesystems such as sshfs where chmod behavior may be unavailable or policy-dependent. The original transactional archive, sidecar synchronization, post-write exact-byte verification, safety decline, and install/rollback behavior are unchanged.

The existing real-tool test `native_iso_wv_real_rename_repairs_cue_and_snapshot_without_extracting_audio` is the direct integration proof for this correction.

## C. Wrong-password completions remain visible after navigation state changes

The R2 navigation guards remain in place: a successful metadata-preparation result is still not allowed to install an editor after Browse/screen/archive ownership has changed.

The handler now snapshots whether the completion error is a recognized archive-password failure before those guards run. If a guard rejects the completion:

- owned temporary staging is cleaned exactly as before;
- no password prompt is forced after the user has left the relevant archive/screen;
- an occupied editor/overlay remains untouched;
- the status reports the password failure (including the underlying sanitized error) instead of misreporting it solely as a navigation or overlay cancellation.

If Browse still owns the same archive and the overlay slot is available, the existing password re-prompt branch remains unchanged.

This restores the pre-existing `archive_metadata_password_prompt_preserves_a_parked_editor` expectation and adds `archive_metadata_password_failure_is_reported_after_archive_view_change` to lock the navigation case: no prompt is opened, owned staging is cleaned, and the wrong-password reason remains visible.

## D. Archive preparation wording is consistent

The three stale status sites identified by the brief now use `Preparing archive:`:

- `src/tui/app.rs`
- `src/tui/command.rs`
- `src/tui/keybindings.rs`

A repository scan of `src/` finds no remaining `Extracting archive:` literal. The existing `ARCHIVE_PREVIEW_EXTRACTING_NOTICE` symbol name is intentionally left unchanged because renaming an internal identifier is unnecessary churn; its user-visible value remains `Preparing archive...`.

## Files changed in this corrective round

- `src/convert/pipeline/materializer_archive.rs`
- `src/tui/event_loop.rs`
- `src/tui/app.rs`
- `src/tui/command.rs`
- `src/tui/keybindings.rs`
- `IMPLEMENTATION_REPORT_archive_access_R3_corrective_2026-08-31.md` (new)

No other baseline source/configuration file was modified.

## Scope intentionally not taken

The two measured performance/design observations recorded as `OUTSTANDING_ISSUES.md` #22 and #23 are not part of this round. No macOS `clonefile` path, cross-platform copy redesign, or native-rename batching/deferral policy was added.

## Verification performed in this environment

The supplied brief states that this implementation container has no Rust toolchain, Nix, or archive executables. That is also true of this runtime: `cargo`, `rustfmt`, `nix`, `7z`/`7zz`, and `xorriso` are unavailable. Therefore **no claim is made that compilation, formatting, the Rust test suite, or the real-tool integration tests were executed here**.

Static/differential checks performed before packaging:

- compared the corrected tree against a fresh extraction of the supplied baseline and confirmed only the five intended source files changed before adding this report;
- confirmed the operator-applied `F: FnMut(ArchiveNativeRenameProgressSnapshot) + Send + 'static` bound remains present;
- confirmed the brief embedded in the bundle is byte-identical to the uploaded R3 brief;
- scanned all five changed Rust files for merge-conflict markers and trailing whitespace;
- ran a string/comment-aware delimiter-balance scan over all five changed Rust files;
- scanned `src/` and confirmed there is no remaining `Extracting archive:` literal;
- reviewed every changed hunk against the R3 evidence and existing surrounding transaction/fallback behavior.

## Operator gate

Run the normal qualified environment gate from the brief:

```bash
cargo test --workspace --no-fail-fast
```

If the repository gate also requires formatting in the qualified environment, run:

```bash
cargo fmt --check
```

Useful focused filters before/after the full gate:

```bash
cargo test -p tonepoet --lib encryption_listing_parser_uses_attributes_when_7z_omits_folder_field
cargo test -p tonepoet --lib repackage_archive_preserves_real_7z_and_zip_encryption_when_7z_is_available
cargo test -p tonepoet --lib native_iso_wv_real_rename_repairs_cue_and_snapshot_without_extracting_audio
cargo test -p tonepoet --lib archive_metadata_password_prompt_preserves_a_parked_editor
cargo test -p tonepoet --lib archive_metadata_password_failure_is_reported_after_archive_view_change
```

The two real-tool filters require the qualified environment's archive tools; their tests self-skip when the executables are absent.

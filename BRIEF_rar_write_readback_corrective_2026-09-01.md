# BRIEF — R3 corrective: RAR write support produces archives Tonepoet cannot read

**Date:** 2026-09-01
**Base:** `main` @ `3714ac1`, with the R2 passwords/RAR/prompt delivery applied
**Prior:** `BRIEF_archive_passwords_and_rar_writes_2026-09-01.md` and its implementation report

## Gate result

`cargo test --workspace --no-fail-fast`: **5391 passed, 2 failed**, 57 result lines.

- `tui::draw_overlays::multiline_display_width_tests::archive_password_prompt_uses_prompt_chrome_not_generic_edit_chrome`
  — already fixed in the working tree, see "changes already made".
- `convert::pipeline::materializer_archive::tests::archive_materializer_extracts_real_rar_fixture_when_available`
  — the subject of this brief. It is not a test defect.

The rest of the delivery gated clean, including the password-cycling work.

## The defect

The delivered RAR creation command is:

```
rar a -r [-p… | -hp…] <archive> .
```

There is no `-m` flag, so it takes `rar 7.20`'s default compression. That default writes RAR
**version 6** compression, which **7-Zip 25.01 cannot decode**.

Tonepoet reads archives with 7-Zip. `archive_listing.rs` shells `7z l -slt` through
`detect_7z_binary()` for every format, and the materializer extracts with
`ToolBinary::SevenZip`. So a RAR that Tonepoet repackages with compression is an archive
Tonepoet can no longer list or extract.

In user terms: edit a RAR, save successfully, then be unable to reopen it.

### Measured

Same `rar 7.20`, same `7zz 25.01`, differing only in content:

| Content | Size → archived | Method 7-Zip reports | `7zz x` |
|---|---|---|---|
| 16-bit PCM `sine.wav` | 88,244 → 3,037 bytes | `v6:128K:m3` | **`ERROR: Unsupported Method`** |
| Incompressible 1 MB | 1,000,000 → 1,000,164 | `v6:1M:m0` (stored) | succeeds |

Compression-level flags do not avoid it:

| Options | Method written | 7-Zip can extract |
|---|---|---|
| *(default, what the code sends)* | `v6:256K:m3` | no |
| `-ma5` | `v6:256K:m3` | no |
| `-ma5 -m3` | `v6:256K:m3` | no |
| `-ma5 -m0` | `v6:128K:m0` | yes |

Only `-m0` — store, no compression — produced 7-Zip-readable output in testing. `-ma5` did
not downgrade the method on this version.

`rar` itself reads its own output correctly (`rar t` reports `All OK`), so this is a
one-directional interop gap, not archive corruption.

### The delivery's own verification cannot catch this

`verify_repackaged_archive` checks a freshly written RAR by running **`rar t`** — the writer
verifying its own output. That always passes, because `rar` understands v6 perfectly well.

Meanwhile `run_archive_extract_command` hardcodes `binary: ToolBinary::SevenZip` and contains
no RAR branch at all, and `archive_listing.rs` shells 7-Zip through `detect_7z_binary()` for
every format. So the two `ToolBinary::Rar` uses in the tree are *create* and *verify*; nothing
ever reads a RAR back with the tool that will actually be used to read it.

That is why the save path reports success on an archive that cannot subsequently be opened.
Whatever direction is chosen, verification that proves readability needs to run through the
reader Tonepoet will really use, not through the writer.

### Why the failing test is the honest signal

`archive_materializer_extracts_real_rar_fixture_when_available` builds a fixture with `rar`
and then asks the materializer to extract it with 7-Zip. Its fixture content is a PCM WAV
written by `write_pcm_wav`. That is precisely the round trip a user performs after saving,
and precisely the content class that triggers the failure. The test was previously skipped
because no `rar` binary existed; adding `rar` to the flake made it run for the first time,
and it immediately found this.

Note the practical exposure is content-dependent in a way that could mislead casual testing:
an archive of FLAC or other already-compressed audio will often be *stored* and stay readable,
while an archive of WAV — CD rips, needledrops — will not. A quick manual check with the
wrong sample material would suggest this works.

## What we need decided

Whether to ship RAR write support at all under these conditions, and if so, how Tonepoet
reads back what it writes. Three directions, none of them free:

- **Store only (`-m0`).** Output stays 7-Zip-readable. Costs compression entirely, which
  changes the character of a user's archive — a repackaged RAR could grow substantially, and
  the user did not ask for that.
- **Give Tonepoet a RAR reader.** `nixpkgs#unrar` exists, and `rar` itself extracts its own
  output. Preserves compression, but the read path becomes format-dependent rather than
  uniformly 7-Zip, which touches listing, extraction, and probably the mount work.
- **Do not ship RAR writes.** The refusal path from the prior round still exists and already
  fires before extraction. This round's own evidence is that the writer cannot round-trip
  through the reader.

The user's decision to ship RAR write support was made before this interop gap was known, so
it is worth re-confirming rather than assuming it still holds.

Whatever is chosen, the invariant that matters is: **Tonepoet must be able to read anything
Tonepoet writes.** A save path that produces an unreadable archive is worse than a refusal,
because the refusal is visible and the unreadable archive is not.

## Changes already made to the delivered code

1. **A stale test call site.** `handle_archive_preview_result` gained an
   `Option<SecretString>` resolved-password parameter; the production caller was updated and
   `convert_archive_preview_password_prompt_preserves_an_occupied_overlay` was not. It now
   passes `None`, which matches the test's wrong-password scenario.

2. **A wrap-split assertion.** `archive_password_prompt_uses_prompt_chrome_not_generic_edit_chrome`
   asserted that the rendered buffer contains
   `"Saved passwords are tried automatically first"`, but the prompt word-wraps that sentence
   across two rows inside a bordered block, so the substring never appears contiguously. The
   rendering is correct; the assertion was not wrap-tolerant. It now strips box-drawing
   characters and collapses whitespace before matching, so it tests the message rather than
   the line breaks.

Neither change touches the RAR behaviour.

## Not in scope

The password-cycling work gated clean and is not implicated. The prompt and `ErrorDetail`
changes gated clean apart from the assertion above.

`docs/OUTSTANDING_ISSUES.md` in the delivered bundle predated entry #27 and was not applied;
the working tree's copy is authoritative.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- Passwords must not reach logs, status text, or sanitized command records; the existing
  `secret_args` indexing exists for that.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.

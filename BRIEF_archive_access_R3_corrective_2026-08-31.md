# BRIEF — R3 corrective: archive access and structural edits

**Date:** 2026-08-31
**Prior:** `BRIEF_archive_access_and_structural_edits_2026-08-31.md`, and the R2 delivery
(`IMPLEMENTATION_REPORT_archive_access_and_structural_edits_2026-08-31.md`).

The R2 delivery was applied to `main` at `78f24ed` and gated. This brief records what the
gate and follow-up investigation found. Nothing here asks for a particular fix; each item is
described with the evidence that produced it, and the response is yours to choose.

## Gate result

`cargo test --workspace --no-fail-fast`, run in the qualified Nix environment:

```
passed=6541  failed=4  ignored=15   (57 result lines)
```

All four failures were in the lib binary. One has already been resolved in the working tree
(see "Changes already made"); that test now passes. The remaining three, plus one defect
found afterwards, are described below.

Each of the three was re-run on its own with `--exact`, outside the full suite, and each
failed there too. They are deterministic and reproduce in isolation, so none of them is a
parallelism or shared-state artifact.

## Changes already made to the delivered code

Two changes were made when applying the bundle, so your baseline differs from what you
shipped:

1. **A `'static` bound.** The delivery did not compile: eight `E0310`/`E0311` errors, all
   from one cause. `rename_archive_entry_native_transactional` passes its `progress` closure
   into `tokio::task::spawn_blocking`, which requires the closure to outlive the call. The
   bound is now:

   ```rust
   F: FnMut(ArchiveNativeRenameProgressSnapshot) + Send + 'static,
   ```

2. **One stale test expectation.** `convert_source_cached_archive_preserves_r11_direct_preview_state`
   asserted the literal status `"Extracting archive: nonexistent-album.zip"`. The delivery
   deliberately renamed that notice, so the expectation was updated to
   `"Preparing archive: nonexistent-album.zip"`. See item D, which concerns the same rename.

## A. Encrypted 7z containing any directory refuses to save

**Failing test:** `repackage_archive_preserves_real_7z_and_zip_encryption_when_7z_is_available`

```
encrypted repackage Visible.7z: archive mixes encrypted and unencrypted members;
Tonepoet cannot reproduce that per-member encryption policy safely
```

The fixture is built by `write_repackage_staging`, which creates `stage/Disc 1/01.txt` and
`stage/manifest.txt`, then packs with `-p… -mhe=off`.

Reproducing that exact shape by hand, 7-Zip 25.01 reports:

```
Path = Disc 1
Attributes = D drwxrwxr-x
Encrypted = -

Path = Disc 1/01.txt
Attributes = A -rw-rw-r--
Encrypted = +

Path = manifest.txt
Attributes = A -rw-rw-r--
Encrypted = +
```

The listing contains **zero** `Folder =` lines for this 7z archive. Packing the *same* three
staging entries as a ZIP and listing it with the same binary emits three `Folder =` lines,
one per member. The difference is therefore format-specific, not version-specific.

`ArchiveEncryptionListingParser` derives directory-ness from that field:

```rust
// src/convert/pipeline/materializer_archive.rs:1185
"Folder" => self.current_is_dir = Some(value.trim() == "+"),
```

and `finish_current` skips a member only when `self.current_is_dir == Some(true)`.

**Impact beyond the test:** this is not a fixture artifact. Any encrypted 7z containing at
least one directory takes the same path, so the fail-closed guard fires and the save is
refused. Most real album archives contain a directory.

Note that the exclusion logic itself is present and correct in intent — directories *are*
meant to be skipped. Only the signal it depends on is absent for this format.

## B. Native ISO-WV rename always declines when the CUE needs rewriting

**Failing test:** `native_iso_wv_real_rename_repairs_cue_and_snapshot_without_extracting_audio`,
which fails on `.expect("ISO-WV native path should be admitted")` — that is,
`rename_archive_entry_native_transactional` returned `Ok(None)`.

The decline reason is reported through `log::debug!`, and `env_logger` is not initialized
under test, so the cause is invisible in gate output. Temporarily replacing that call with an
`eprintln!` (since reverted) produced:

```
native ISO-WV rename declined: write rewritten ISO-WV CUE staging file failed:
Permission denied (os error 13)
```

The relevant sequence is in `prepare_native_iso_wv_cue_repair` and `target_read_iso_member`:

1. The CUE is target-read out of the transactional ISO copy with
   `xorriso -osirrox on -indev <iso> -extract_single /<member> <disk_path>`.
2. The rewritten CUE is then written back to that same `disk_path`.

Running that same `xorriso` command by hand against an equivalent fixture succeeds and
restores the file as:

```
-r--r--r-- 1 daedalus daedalus 63 ... out.cue
```

`osirrox` restores members carrying the permissions recorded in the image, and ISO members
are read-only, so the extracted staging file is mode 444. The subsequent write to it fails
with `EACCES`.

This was observed independently — the read-only mode was noted while reproducing the extract,
before the error text was captured — so the two lines of evidence agree.

Because the decline is silent and falls back to extraction, the effect in normal use is not a
visible failure: the ISO-WV native fast path simply never engages whenever a CUE rewrite is
required, which is the case this work exists to accelerate.

## C. A wrong archive password is reported as a navigation failure

**Failing test:** `archive_metadata_password_prompt_preserves_a_parked_editor` (pre-existing,
from `26e640d`; neither it nor `prompt_overlay_slot_is_unobstructed` was modified by R2).

The delivery inserted a new guard at the top of `handle_archive_metadata_editor_prepared`
(the function begins at line 2845; the guard is at 2903):

```rust
if !browse_holds_same_archive {
    let _pending = app.pending_browse_archive_metadata.take();
    ...
    app.set_status("archive metadata editor cancelled: archive view changed before extraction finished");
    return;
}
```

The password-error handling, which distinguishes "prompt for a password" from "an editor or
overlay is already open, so preserve it", lives at line 3003 in the same function — after the
new guard.

The test calls `handle_archive_metadata_editor_prepared` directly with
`Err("Wrong password")` and never populates `app.browse.archive`, so the guard returns first
and the password branch is unreachable. The test's other assertions still pass: the parked
editor survives, because the guard takes `pending_browse_archive_metadata`, a different field
from `pending_metadata_editor`. Only the status differs.

Two things are worth separating here. The guard implements a wanted outcome — that a user may
navigate away during preparation without being trapped. But its placement means a
wrong-password result is now reported as an archive-view change, and the password failure is
not surfaced at all. That is most likely to happen in exactly the situation the guard was
added to support, since navigating away is now permitted.

Whether the correct response is to reorder, to narrow the guard, to change what the test
expects, or something else, depends on what the intended user-visible behaviour is in that
combination. That judgement is yours; this brief only records that the two behaviours
currently collide.

## D. The "Extracting" → "Preparing" rename is partial

R2 changed `ARCHIVE_PREVIEW_EXTRACTING_NOTICE` to `"Preparing archive..."` and one status
format in `src/tui/app.rs` to `"Preparing archive: {}"`. Three sites still emit the old
wording:

- `src/tui/app.rs:15026`
- `src/tui/command.rs:8605`
- `src/tui/keybindings.rs:46683`

The third is the notable one. It sits inside an archive-preview starter
(`remove_batch_at_cursor_with_archive_starter`), in the same block that sets the renamed
constant for the source pane:

```rust
Some(ARCHIVE_PREVIEW_EXTRACTING_NOTICE.to_string()),   // now "Preparing archive..."
...
app.set_status(format!("Extracting archive: {}", path.display()));
```

So one operation describes itself two different ways at once: the source pane reads
"Preparing archive...", the status bar reads "Extracting archive: ...". The preview work
itself is dispatched elsewhere (`src/tui/app.rs:1483` calls `try_mount_archive_readonly`),
so the status wording can also be inaccurate whenever that mount succeeds.

## Context that may bear on your choices

Two tracker entries were opened from measurements taken against this delivery. Neither is a
request for work in this round; they are recorded because they affect how the current design
performs in practice.

- **`OUTSTANDING_ISSUES.md` #22** — the transactional copy's accelerated paths are
  unavailable on common configurations. `FICLONE` and `copy_file_range` are Linux-only, and
  both call sites are `#[cfg(target_os = "linux")]`, while the flake builds darwin through
  `eachDefaultSystem`; macOS therefore always takes the buffered copy despite APFS supporting
  cloning via `clonefile`. On this operator's machine, `/home/daedalus/dev` is ext4 and
  `cp --reflink=always` fails with `Operation not supported`; the archives themselves live on
  an sshfs mount.

- **`OUTSTANDING_ISSUES.md` #23** — the native rename path commits immediately, while delete
  and the rename extraction fallback stage their edits and repackage once on navigate-away or
  quit. Counting deferral markers per completion arm: native rename 0, fallback rename 3,
  delete 2. Combined with #22, N renames cost roughly 2N passes over the payload on
  non-reflink storage, against ~4 passes total for the deferred path regardless of N — so the
  native path is ahead for one rename, level at two, and behind from three onward on such
  storage.

These are noted because A–D may interact with them, not because either is in scope here.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools;
  running the gate is the operator's job and no delivery should assume it has been run.
- `src/convert/pipeline/mod.rs:13` carries `#![deny(unsafe_code)]`. Files beneath it that
  need `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying comment;
  `tool.rs:261` and `progress/streaming.rs:175` are the established examples, and R2's own
  `copy_archive_for_native_edit` follows the same form.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- Bundles should be self-contained and gzipped.

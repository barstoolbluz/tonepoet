# BRIEF — Staged archive state: stale views, latched dirtiness, and transfer progress

**Date:** 2026-09-01
**Base:** `main` @ `653cb1e`
**Prior:** `BRIEF_archive_locality_and_spawn_2026-09-01.md` and its implementation report.

Three problems found in field use after `653cb1e`. Each is described with its evidence.
None has a prescribed fix; the approach is yours to choose. Sections A and B describe
behaviour in the same subsystem; C is independent of both.

## A. A staged rename leaves the pre-rename name visible beside the new one

### What the user sees

Renaming a folder inside an archive to a case-only variant — `Artwork` to `artwork` — leaves
**both** entries in the in-archive view, apparently identical in content.

The packaged archive is correct. The duplicate is never persisted, and saving produces the
single intended directory. The defect is confined to what Browse displays while edits are
staged. That is not a small consolation, though: the user cannot tell which of the two is
real, and it reads as data corruption in a tool whose entire job is not corrupting people's
music libraries.

This is the observation recorded earlier as unexplained, in
`BRIEF_archive_locality_and_spawn_2026-09-01.md` section C. It now has a reproduction: a
case-only rename of an existing archive directory, viewed while the edit is staged.

### Where the view comes from

`BrowseState::rebuild_archive_raw_entries` (`src/tui/browse.rs:5177`) builds the rows in two
passes.

Pass one, from `src/tui/browse.rs:5202`:

```rust
let items = arc.listing.entries_at(&arc.inner_path);
let mut listing_paths = HashSet::new();
for item in &items {
    listing_paths.insert(item.full_path.clone());
    let staged_metadata = /* ... look up the staged path ... */;
    // kind/size/mtime are refined from staged_metadata when present
```

`arc.listing` is the listing captured from the archive *before* any edits. Every entry it
returns is rendered. `staged_metadata` is consulted only to refine kind, size and modified
time — there is no branch that skips an entry whose staged path no longer exists.

Pass two, from `src/tui/browse.rs:5260`, scans the staging directory for the current inner
path and appends children that are not already present:

```rust
if listing_paths.contains(&inner) {
    continue;
}
```

That is an exact string comparison.

After renaming `Artwork` to `artwork`, the staging tree contains only `artwork`. Pass one
still emits `Artwork` from the stale listing. Pass two sees `artwork`, finds no exact match
in `listing_paths`, and appends it. Both render.

### Adjacent state, recorded here for completeness

`ArchiveStagingSession` (`src/tui/browse.rs:3009`) carries `edits: Vec<ArchiveEdit>`
(`:3015`), and `ArchiveEdit` (`:2986`) includes:

```rust
Rename { from: String, to: String },
```

That log exists primarily as the crash-recovery record and is persisted for that purpose.
`rebuild_archive_raw_entries` contains zero references to it. This is noted as a fact about
the current code, not as an indication of where a fix belongs.

### Scope is wider than the reported symptom

- Because the comparison is exact, the mechanism is not specific to case. Any staged rename
  should leave its old name behind. Case-only renames are simply the variant where the two
  entries look like duplicates of one thing rather than two unrelated names, which is why
  this is the form that got noticed.
- `archive_recursive_search_entries` (`src/tui/browse.rs:7588`) repeats the same shape: it
  renders from `arc.listing.entries`, then walks the staging tree with `walkdir` and appends
  anything not caught by the same exact `listing_paths.contains(&inner)` guard. So recursive
  search results carry the stale entry too, not just the directory listing.
- Renames that take the format-native path rewrite and install the archive immediately, and
  their completion arm then calls `invalidate_archive_listing_cache_for_path` followed by
  `start_browse_archive_listing(..., force = true)` when Browse still holds that archive
  (`src/tui/event_loop.rs:5350`). That is a genuine re-listing, so those views are correct.
  The stale view is a property of the staged/deferred path, which only ever calls
  `refresh_with_search` — and that reaches `rebuild_archive_raw_entries` via
  `refresh_archive_view_with_search` (`src/tui/browse.rs:5148`) without re-reading the
  archive. `arc.listing` is never reassigned after the archive is opened. Which path a rename takes depends on format, tool availability, and
  whether the directory is explicit or synthesized in the archive — so two operations that
  look identical to the user can behave differently.
- The same reasoning suggests staged deletes may leave their entries visible as well. That
  has not been reproduced and should be checked rather than assumed.

### A previous attempt in this area, and why it was withdrawn

A previous round implemented ASCII-case-insensitive path reconciliation across Browse,
probes, tag extraction, rename/delete validation, and destination-parent resolution. It was
cut back before landing because review found it could suppress a genuinely distinct
user-created `artwork` sitting beside `Artwork`, and could resolve a case-only staged rename
back to the old spelling even though the staged bytes carried the user's spelling. See the
R2 corrective section of `IMPLEMENTATION_REPORT_archive_locality_and_spawn_2026-09-01.md`.

Recorded so the same ground is not covered twice, and because the failure mode it produced —
suppressing a real user-authored entry — is worth knowing about in advance.

### Outcomes wanted

- What Browse shows while edits are staged should match what saving would produce.
- A user should not need to know whether a rename took the native or the staged path in order
  to predict what the view will show.
- Distinct entries that genuinely differ only by case must remain independently visible and
  selectable.

## B. Reverting an edit still leaves the archive marked as needing a repackage

### What the user sees

Extract an archive, change something, then change it back. Tonepoet still wants to repackage
the archive, even though the staged tree now matches what the archive already contains.

The cost is not cosmetic. Repackaging a multi-gigabyte archive is the expensive operation
this whole line of work exists to avoid, and the user is prompted to pay it for a net change
of nothing — including, on remote storage, a full sequential copy back.

### Mechanism

`ArchiveStagingSession` (`src/tui/browse.rs:3009`) carries `dirty: bool` alongside its
`edits` log. Every mutation records an edit and latches the flag:

```rust
pub fn append_edit(&mut self, edit: ArchiveEdit) {
    self.edits.push(edit);
    self.dirty = true;
}
```

The session has three mutation methods — `append_edit`, `append_metadata_write`, and
`append_content_modified` — and all five of their write paths latch the flag
(`self.dirty = true` at `src/tui/browse.rs:3063`, `:3081`, `:3090`, `:3103`, `:3108`). The
string `dirty = false` does not appear anywhere in `src/tui/browse.rs`. The flag is monotonic
for the lifetime of the session.

So renaming `Artwork` to `artwork` and back appends two `ArchiveEdit::Rename` records and
leaves `dirty` latched, even though the staging tree is once again identical to the archive.
`append_metadata_write` does coalesce repeated writes to the same field, but it still sets
`dirty = true` on the coalesced result, so writing a tag back to its original value latches
too.

### An observable property shared with section A

A and B share an observable property: in neither case does anything establish the current
relationship between the staged tree and the archive's original contents. Section A's view
does read the staging tree, but only to add entries and to refine kind, size and mtime — it
never uses it to retire a listing entry that staging no longer backs. Section B's flag
records that a mutation occurred and cannot be cleared by any later state, including a state
identical to the original.

Whether that shared property means they share a solution, and what any such approach would
cost on a multi-gigabyte archive, is not something this brief attempts to settle.

Worth noting in passing: `edits` is also the crash-recovery log, so it has a second consumer
whose needs may differ from the view's and the dirty flag's. Any change to what it records or
means should account for that.

### Outcomes wanted

- A user who reverts their changes should not be asked to repackage.
- Whatever answers "does this need saving" should agree with what a save would actually
  produce, for the same reason section A's view should.

## C. Long archive transfers should use the existing progress surface

### What happens now

`653cb1e` made archive editing copy remote archives to local storage before working on them,
and reports that copy and the subsequent extraction through the **status bar**. That is a
large improvement on the previous silence, but it is a single line of text for an operation
that can move several gigabytes and run for tens of seconds or minutes.

The user's position is that not using the existing progress machinery here is a mistake.

### The machinery that already exists

The project owns a surface built for exactly this shape of work — a long file transfer the
user should be able to ignore while continuing to work:

- `AppState::minimized_file_task_progress` (`src/tui/app.rs:12702`) holds a
  `FileTaskProgressSession` (`app.rs:6658`) parked outside the modal overlay and rendered in
  the shared footer rather than as a pop-up box.
- Its doc comment records the property that matters here: the session retains its
  `controls: mpsc::Sender<FileTaskControl>`, so "cancellation/conflict guarantees are
  identical whether the surface is visible or in the shared footer."
- `FileTransferQueueState::keep_minimized_across_jobs` (`app.rs:6780`) already expresses
  "keep this minimized"; `blocked_for_attention` already expresses "this needs the user now".
- Coverage exists, for example `minimized_footer_state_tracks_live_progress_and_fifo_depth`
  and `visible_archive_install_preserves_scheduler_owned_minimized_transfer` in
  `src/tui/event_loop.rs`.

### What the user wants

Archive localization and extraction should use that surface, and such an operation could
reasonably start **minimized by default** — the footer progress bar rather than a modal box —
so the user keeps working, still sees progress, and retains the ability to cancel.

### Open questions

- Whether minimized-by-default should apply to every archive transfer, only above some size
  threshold, or be a user preference.
- Whether the archive copy should become a first-class job in the existing transfer queue or
  merely borrow its progress surface. The queue carries FIFO ordering, preemption, and
  journal-based crash recovery, which may or may not be wanted for a transfer that already
  sits inside its own archive-edit transaction.
- How this interacts with the copy-back leg, which runs inside the backup/install/restore
  transaction and is deliberately non-cancellable once past the final conflict check. A
  progress surface that offers cancellation during a phase that cannot honour it would be
  worse than the status line.
- Whether the extraction phase, which is driven by an external tool and reports coarse staged
  byte counts, can present meaningfully on the same surface as the copy phase, which has
  exact byte totals.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `src/convert/pipeline/mod.rs:13` carries `#![deny(unsafe_code)]`. Files beneath it that need
  `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying comment;
  `tool.rs:261` and `progress/streaming.rs:175` are the established examples.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. `Alt+L` is taken by the
  metadata editor's select-all, which exists because tmux users have `Ctrl+A` bound. No emoji
  or decorative unicode in UI text.
- Tests that mutate process-global state have caused repeated flakes in this project. The
  previous round's `PATH`-dependent test ran the selector in a child process with a controlled
  environment rather than mutating the parent's; that pattern is worth preserving.
- `OUTSTANDING_ISSUES.md` #22 through #26 are the tracker entries for this area. #24,
  #25 and #26 correspond to sections A, C and B here respectively. #22 and #23 remain open and
  are not in scope.

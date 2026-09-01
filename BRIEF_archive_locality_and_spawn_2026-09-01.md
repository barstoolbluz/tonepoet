# BRIEF — Archive work on network storage: locality, throughput, and a spawn failure

**Date:** 2026-09-01
**Base:** `main` @ `77d55bd`
**Prior:** `BRIEF_archive_access_and_structural_edits_2026-08-31.md`,
`BRIEF_archive_access_R3_corrective_2026-08-31.md`

Field testing the shipped archive work found one blocking defect, one severe performance
problem, and one unexplained artifact. Each is described with its evidence. No fix is
prescribed.

## Context: where the user's archives actually live

`~/livetorrents` is an **sshfs mount** over a 10 Gbps LAN. Large `.iso.wv` albums live there.
`~/dev` is local NVMe. The same album in the two locations behaves completely differently,
which masked this problem initially: an extraction that took 4m50s from sshfs took under 20s
from NVMe, with identical code.

## A. Extraction over network storage is roughly 18x slower than the hardware allows

### Measured, 2026-09-01

Against `ZZ Top - Eliminator ... .iso.wv`, 2,946,327,775 bytes, on the sshfs mount:

| Operation | Time | Effective rate |
|---|---|---|
| Sequential read, `dd bs=8M` | 4.69s for 1 GiB → ~13s projected for the whole file | **229 MB/s** |
| `7z x` reading from sshfs, writing to local NVMe | **59.8s** (2,810 MiB extracted) | ~49 MB/s |
| Tonepoet's own extraction (user-observed, 25% at 60s) | **~240s** | ~12 MB/s |

For comparison, on local NVMe the same class of work is:

| Operation | Time |
|---|---|
| Raw sequential read, NVMe | 1.4 GB/s |
| `7z x` of the 2.6 GB Animals ISO to NVMe | **2.28s** |
| Same, with tonepoet's exact argv (`x`, `-mmt=on`, `-o`, `-y`) and progress output piped | **1.55s** |

### Cache state moves these numbers by 4.4x — control for it

Repeating the same extraction after the data had been touched once:

| Same file, same command, same destination | Time |
|---|---|
| `7z x` sshfs → local NVMe, **cold** (first touch this session) | **59.8s** |
| `7z x` sshfs → local NVMe, **warm** | **13.5s** |
| `7z x` sshfs → sshfs, warm | 25.5s |
| Sequential `dd`, cold / warm | 229 MB/s / 219 MB/s |

Two things follow. First, any future measurement here is meaningless unless it states whether
the source was cold, because the same command varies by 4.4x. Sequential reads barely move,
so the cache sensitivity is specifically in the many-small-request pattern, which is
consistent with the round-trip explanation below. Second, warm `7z x` (13.5s) converges on
the sequential-read time, i.e. when round trips are cheap the seekiness costs nothing.

The comparisons in the previous section are cold-versus-cold: the user's Eliminator run was a
first extraction of a file that had not been read before, and the 59.8s baseline was likewise
this session's first touch of it.

### Two separate multipliers

**7z's read pattern costs ~4.6x over FUSE.** The same file on the same mount reads at 229 MB/s
sequentially but only ~49 MB/s through `7z x`. Extracting an ISO seeks — directory
structures, then per-member reads — and every seek is a FUSE round trip to a remote host.
Bandwidth is not the constraint; round trips are.

**Tonepoet appears a further ~4x slower than plain 7z on the identical source** — 59.8s for
`7z x` against roughly 240s observed in the application. Treat that multiplier as
approximate rather than measured: the application figure is extrapolated from a progress
display the user read at 25% after 60s rather than a completed timed run, and the user's
partial run may have pre-warmed part of the file before the 59.8s baseline, which would make
that baseline optimistic and the real gap smaller. What is solid is that a gap of this order
exists and is not accounted for by 7z, the disk, or the link. It has not been located. Ruled
out so far:

- *7z's own progress output.* Tonepoet does not pass `-bso0 -bsp0`, so 7z emits progress.
  Measured both ways with output piped: 1.548s vs 1.558s. Not a factor.
- *The extraction progress poller.* `extract_archive_to_staging_with_progress` wakes every
  750 ms and calls `staged_regular_file_bytes`, a directory walk. The albums involved contain
  roughly 10-15 files. Negligible.
- *`extract_compressed_tar_payloads`,* which runs after extraction. For a `.iso.wv` its
  `intermediate_tar_files_for_compressed_tar` returns empty and it exits immediately.

Note also that this code path emits **no log output whatsoever**, so a four-minute operation
leaves nothing in `~/.cache/tonepoet/tonepoet.log` to diagnose after the fact.

### What the numbers imply

Reading the archive **sequentially** to local storage costs ~13s. Extracting locally then
costs ~2s. That is ~15s of work against the ~240s observed, without changing the
architecture at all — purely by not making a remote filesystem service a seeky read pattern.

### Direction the user has chosen

Do archive work on local storage, and cross the network only in large sequential transfers:
on the first change, copy the archive to local temporary storage; apply the edits there;
repackage there; then transparently copy the result back. The user has explicitly deferred
the larger deferred-commit / virtual-view design to a later round — this brief is about
locality and throughput only.

Points that will need decisions, listed as questions rather than requirements: where that
local staging lives and how its capacity is decided; how the copy-back interacts with the
existing backup/install/restore transaction and its fingerprint re-checks; what happens when
local free space is insufficient; and whether the same locality rule should apply to the
read-only mount paths, which currently read remote data lazily and may already be fine.

### Existing progress machinery worth considering for the transfer

Today these operations report through the status bar, which helps but is a single line of
text. The project already owns a richer surface built for exactly this shape of work — a
long file transfer the user should be able to ignore.

`AppState::minimized_file_task_progress` (`src/tui/app.rs:12691`) holds a
`FileTaskProgressSession` (`app.rs:6647`) parked outside the modal overlay, rendered in the
shared footer rather than as a pop-up. Its own doc comment records the property that matters
here: the session retains its `controls: mpsc::Sender<FileTaskControl>`, so
"cancellation/conflict guarantees are identical whether the surface is visible or in the
shared footer." `FileTransferQueueState::keep_minimized_across_jobs` (`app.rs:6769`) already
expresses "keep this minimized", and `blocked_for_attention` already expresses "this needs the
user now". Coverage exists — for example
`minimized_footer_state_tracks_live_progress_and_fifo_depth` and
`visible_archive_install_preserves_scheduler_owned_minimized_transfer` in `event_loop.rs`.

The user's suggestion is that a multi-gigabyte archive transfer is a natural fit for this
surface, and that such an operation could reasonably start **minimized by default**, showing
the footer progress bar rather than a modal box, so the user can keep working and still see
progress and retain the ability to cancel. Whether that should be the default for every
archive transfer, only above some size threshold, or a user preference, is open. So is
whether the archive copy should join the existing transfer queue as a first-class job or
merely borrow its progress surface — the queue carries FIFO, preemption, and journal-recovery
semantics that may or may not be wanted here.

## B. Saving an ISO-WV archive fails with "create ISO-WV image: failed to spawn tool"

Reproduced by the user with `xorriso` present and on `PATH` in a fresh Nix shell, so this is
not a missing-tool problem. The mechanism, traced by reading:

1. `repackage_tool_path(tool_paths, &["xorriso"])` returns the configured path when present,
   and otherwise falls back to `PathBuf::from("xorriso")` — a bare name.
2. `tool_paths` is **always empty in production**. Every construction site builds it with
   `HashMap::new()` — `src/main.rs:1639`, `src/tui/convert_actions.rs:662`,
   `src/convert/processor.rs:5619` and `:9394` — and `src/config.rs` has no tool-path field
   at all, so there is no configuration that could populate it. The bare-name fallback is
   therefore not an edge case; it is the only production behaviour.
3. `run_repackage_command` builds a `RealToolRunner::new(HashMap::new())` and calls
   `run_with_binary_path(command, binary_path, cancel)`, which delegates to
   `run_supervised_with_stdio`.
4. That function does, at `src/convert/pipeline/tool.rs:1321`:

   ```rust
   let launch_path = binary_path;
   let reviewed_path = std::fs::canonicalize(&launch_path)
   ```

   `std::fs::canonicalize` on a bare name resolves it against the **current working
   directory**, not `PATH`. The ISO-WV create call sets its cwd to the staging directory, so
   this looks for `<staging>/xorriso` and fails.

Worth noting: `RealToolRunner::tool_version` (`tool.rs:1505`) *does* call
`resolve_command_launch_path` before use, and that helper exists specifically to turn a bare
name into a PATH-resolved absolute path. The execution path does not make that call. So
version probing and execution disagree about how a bare tool name is resolved.

### Why 6,547 passing tests did not catch this

The real-tool repackage tests inject **absolute** paths into `tool_paths` before calling in.
They obtain them from a test helper `find_executable` (`materializer_archive.rs:5437`) that
walks `PATH` and returns `dir.join(candidate)`, then pass, for example,
`HashMap::from([("xorriso".to_string(), xorriso)])`. Production passes an empty map.

So the suite exercises the absolute-path branch of `repackage_tool_path` on every run and
never exercises the bare-name branch that production always takes. The defect is invisible to
the entire gate by construction, not by omission. Any fix probably wants a test that reaches
the spawn the way production does.

Also unresolved: this reasoning applies identically to the 7z and ZIP repackage paths, which
reach the same fallback with `&["7zz", "7z"]`. Whether archive saving is equally broken for
those formats in production has not been checked and is worth establishing early, since it
would widen the defect well beyond ISO-WV. `detect_7z_binary`
(`src/lib.rs:25`) also returns a bare name, but `archive_listing.rs:302` spawns it with a
plain `Command::new(bin)`, which resolves via `PATH` normally — that is a different route
from the supervised one.

## C. Duplicate case-variant directories in the extracted view

The user observed both `Artwork` and `artwork`, with the same contents, in the extracted view
of the Animals archive. The changes were never committed, because saving failed with the
defect in section B, so this is staging-tree or view state rather than anything written back.

Not reproduced from the image itself. Against
`Pink Floyd - Animals ... .iso.wv`:

- `7z l` reports a single `Artwork` and totals 15 files, 3 folders (with one unexplained
  `Warnings: 1` line).
- `xorriso -toc` reports `ISO offers : Joliet` and `ISO loaded : Joliet` — no Rock Ridge.
- A `fuseiso` mount lists a single `Artwork`.

So three independent tools each see one directory. The duplicate appears somewhere between
the image and what Browse displayed after extraction. A plain ISO-9660 namespace would carry
`ARTWORK` in upper case, and some readers lower-case such names to `artwork` — that is a
hypothesis worth checking against the actual extraction output, not a finding.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the archive tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `src/convert/pipeline/mod.rs:13` carries `#![deny(unsafe_code)]`. Files beneath it that need
  `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying comment;
  `tool.rs:261` and `progress/streaming.rs:175` are the established examples.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- `OUTSTANDING_ISSUES.md` #22 and #23 remain open and are not in scope here, though #23
  (structural edits committing per-operation rather than as a batch) is closely related to
  the locality work and may constrain how it is staged.

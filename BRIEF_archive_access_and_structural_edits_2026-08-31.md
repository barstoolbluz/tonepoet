# BRIEF — Archive access and structural edits: stop extracting whole archives

**Raised:** 2026-08-31, after field-testing the `.iso.wv` support that landed at `9ca2106`.

## What prompted this

Renaming a folder inside a 2.7 GB `.iso.wv` album currently extracts the entire archive
first. On network-bound storage this takes minutes, the user is given no indication that
anything is happening, and attempting to navigate away during it is refused. The operation
being paid for is a directory rename.

The same shape applies to the other archive formats, which take the same
extract-modify-repack path.

## Measurements

All figures below were measured on this machine on 2026-08-31, against real files where
possible. They are offered as evidence, not as a design.

**Conditions, which matter:** every timing was taken on local disk with a warm page cache.
They establish differences in CPU work and in bytes moved; they are not predictions of
behaviour on the network-bound storage where this problem actually hurts. Expect the
relative gaps to widen there, not narrow, since the slow path's cost is dominated by bytes
pulled across the wire — but treat the absolute numbers as a floor, not a forecast.

### Mounting instead of extracting (already implemented for ISO)

Against the real 2.7 GB `Pink Floyd - Animals ... .iso.wv`:

| What | Result |
|---|---|
| `fuseiso` mount | **0.010s** |
| Sequential read through the mount (300 MB) | **515 MB/s** |
| `front.jpg` read through mount vs. extracted independently by `7z` | **md5 identical** (`55c5345f…`) |

`.iso.wv` is a plain ISO9660 image containing WavPack audio — `CD001` is present at offset
32769 on 8 of 8 sampled files. It is not a WavPack-compressed ISO, and no `wvunpack` step is
involved.

Note that targeted extraction from an ISO is *also* cheap: `7z` pulled a single file out of
the 2.7 GB image in 0.007s, because ISO is not solid. The expensive thing is specifically
*whole-archive* extraction.

### Format-native structural edits, without extracting

| Format | Operation | Time | Verified |
|---|---|---|---|
| ISO | `xorriso -dev img -mv /a /b -- -commit` | **0.109s**, 411 KB written (50 MB image) | tree re-parented |
| ISO | same, separate run on a 200 MB image | timing not recorded | extracted payload md5 matches original |
| 7z (solid, default compression) | `7z rn` | **0.063s** (100 MB) – 0.222s (400 MB) | `7z t` OK, md5 matches |
| zip (store) | `7z rn` | **0.272s** | `7z t` OK, md5 matches |
| 7z / zip | folder rename (whole subtree), store mode | 0.240s / 0.261s | subtree re-parented correctly |
| 7z / zip | add small file | ~0.007s | `7z t` OK |

The ISO in-place form appends a session: the file grows by the size of the new directory
data (411 KB in the test) and its hash changes. The out-of-place form
(`-indev` / `-outdev`) avoids session accumulation but writes a whole new image.

### Where the fast path does *not* hold

Delete is not uniformly cheap. On a solid, compressed 7z whose creation took 10.411s:

| Operation | Time |
|---|---|
| Rename any entry | 0.063s |
| Delete an entry **inside a shared solid block** | **10.561s** (a full repack) |
| Delete an entry that occupies its own block | 0.428s |

So rename is header-only regardless of solidity, while delete depends on archive layout.
Any user-facing promise about "fast structural edits" has to survive this distinction.

### Copy-on-write overlay was considered and measured

An overlay with the read-only mount as the lower layer was the first idea raised. Measured
with `fuse-overlayfs`, a lower layer holding one 400 MB file:

```
upper before: 0 MB
mv merged/big.wv merged/renamed.wv
upper after:  382 MB
```

overlayfs copies up an entire *file* on rename; CoW is per-file, not per-block. A typical
album here is one large audio file plus small companions, so an overlay would copy the
whole payload to rename it, and a repackage would still follow. It is a poor fit for the
dominant case. It remains reasonable as staging for *added* or small edited files, where
nothing large is copied up.

### Format capability matrix

| Format | Mount / read | Format-native structural edit |
|---|---|---|
| ISO (`.iso.wv`) | yes — `fuseiso`, in tree today | yes — `xorriso` |
| 7z | yes | yes — `7z rn` (delete: layout-dependent) |
| zip | yes | yes — `7z rn` |
| RAR | **yes** — 7-Zip 25.01 carries its own `Rar`/`Rar5` codec; listing and decoding both verified (`7z x -so` streamed 8 MB from a 59-volume 2.9 GB set) | **none possible** — `7z a -trar` fails; no RAR writer exists |

There is no `unrar` binary in the dev shell, but it is not needed: 7-Zip reads RAR itself.

The "mount / read" column above records format support in principle, not a working
implementation. Only ISO has one today (`fuseiso`). Two general-purpose candidates exist in
nixpkgs and were confirmed present but **not functionally tested here**: `archivemount`
(libarchive-backed) and `ratarmount` (builds an index, so random access stays cheap on
formats where it otherwise would not). Whether either is worth adopting, and for which
formats, is an open question — note that solid compression makes arbitrary random access
expensive in general, though for already-compressed audio payloads that penalty is largely
absent.

## Current implementation

- `src/convert/classify.rs:198` — `is_iso_wv_container` is a filename-suffix test.
- `src/convert/pipeline/materializer_archive.rs:74-115` — the ISO-WV read path already
  implements exactly the shape this brief generalizes: try to mount, fall back to
  extraction, and record which happened as `IsoWvPayloadAccess::{Mounted, Extracted}`.
- `run_archive_extract_command` builds `7z x … -o<root> -y` — a whole-archive extraction
  with no entry filter.
- `src/tui/keybindings.rs:34173`, `:34922`, `:58945`, `:59261` — the four Browse edit
  entry points (metadata edit, entry delete, entry rename, ISO-WV create). **All four pass
  `None` as the reporter**, so the `OperationProgressTracker` that
  `run_archive_extract_command` would otherwise drive is never fed on the Browse path.
- `src/tui/event_loop.rs:103`, `:2898`, `:2910` — the cancellation messages a user sees if
  they navigate or open another overlay while extraction is running.
- `src/tui/archive_listing.rs:303` — **Browse listing already does not extract.** It runs
  `7z l -slt`, a header read, for every format. Do not replace this with a mount on the
  assumption that listing is slow; it is not, and the existing remote-filesystem gate exists
  because header reads on remote storage can still be slow, not because listing extracts.
  The extraction problem is confined to the edit path and to conversion materialization.

## Outcomes wanted

1. A folder or file rename inside an archive should not require extracting the archive,
   for any format where the container supports it.
2. Materializing a non-ISO archive for conversion should not require extracting the whole
   thing when the payload could be read in place, generalizing what the ISO path already
   does. (Browsing is *not* part of this: see below.)
3. Where a slow path is genuinely unavoidable (a delete inside a solid block; any
   structural edit on RAR), the user should be told what is happening and roughly how far
   along it is, and should not be trapped in the archive while it runs.
4. The existing extraction path remains the fallback and must keep working for every case
   the fast path cannot serve.

## Constraints and guardrails

- **Transaction semantics are the crux and must not regress.** The current design builds a
  replacement and swaps it under an install/restore transaction, which is what makes
  rollback exact. `7z rn` and `xorriso -commit` mutate the user's original file in place.
  That is faster, but it changes rollback from "restore the previous file" to "apply the
  inverse operation," and it changes the file's hash in place. Whether in-place is safe
  enough to be the default, or should be opt-in, or should be used only where an inverse is
  provably available, is a judgement for the implementer — it is the most consequential
  decision in this work.
- **RAR needs an explicit answer, not a discovered one.** It can be read and browsed, but no
  structural edit can be written back in RAR format. The options are to refuse such edits or
  to emit a different container, which silently changes the user's file format. Whichever is
  chosen should be a deliberate, stated decision.
- **Encrypted archives.** Archive passwords are already supported. How they interact with
  mounting, and whether header-encrypted archives permit header-only operations, needs
  checking rather than assuming.
- **Keybindings.** Plain letters in Browse are reserved for type-ahead and must stay that
  way. No F-keys. `Alt+L` is already select-all in the metadata editor, which exists because
  tmux users have `Ctrl+A` taken. No emoji or decorative unicode in any UI text.
- `src/convert/pipeline/mod.rs:13` carries `#![deny(unsafe_code)]`. Files under that module
  that need `unsafe` use a narrowly scoped `#[allow(unsafe_code)]` with a justifying
  comment; see `tool.rs:258` and `progress/streaming.rs:175` for the established form.
- Any new external tool must be declared in `flake.nix`, and its absence must degrade
  gracefully rather than fail — the ISO path's capability-miss fallback is the model.
- The implementer cannot run the test suite in its container; verification is the operator's
  job. Deliveries should not assume a gate has been run.

## Related, and worth folding in

`OUTSTANDING_ISSUES.md` **#21** — archive listing can be refused with no way to override it.
Both refusal messages tell the user to press `l`, which is unbound in Browse and would be
swallowed by type-ahead anyway; every `force = true` caller is internal. The chosen
direction is a `:l` command (both `"l"` and `"list"` are unclaimed in `command.rs`, and
single-letter commands are already the convention there). This is the same subsystem and
the same user journey — the reason the user could not open the archive at all.

## Out of scope

- The flake in `OUTSTANDING_ISSUES.md` #20.
- Changing archive *creation* defaults, compression levels, or solidity.
- Anything about the conversion pipeline's own use of materialized archives beyond keeping
  it working.

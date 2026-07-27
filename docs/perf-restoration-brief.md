# Performance Restoration Brief — File Operations (copy / move / rename / undo)

**Feature-lineage commit:** `hardening` @ 839baab (the degraded-rename ladder introduced there remains load-bearing).
**Delivery/apply baseline:** `hardening` @ 83fe80e, as identified by the later handoff document; exact preimage hashes remain the fail-closed application authority.
**Baseline suite:** 5,162 passed / 0 failed (56 targets). **Version stays 0.4.4.**

## 0. The governing directive (from the user, verbatim intent)

> "We've introduced so much rigor and correctness that it's breaking casual usage.
> It's also slowed down copy/move operations significantly — not just transfer
> rates, but the verification/validation steps. There's a place for this (maybe
> some sort of strong validation/verification option the user can opt into as
> part of advanced settings), but there's gotta be a happy medium where we get
> good to great performance with safeguards that address all but the most arcane
> edge cases. I feel like we've been addressing arcane issues that are unlikely
> to be encountered in the wild."

This is a **top-level design constraint**, not a preference. The last three
correction rounds each broke a basic operation (move, rename) on the user's
real mounts (fuse.sshfs, ntfs-3g) in the name of proof machinery, while the
residuals being defended against are millisecond-scale same-tick races and
adversarial concurrent mutators that do not occur in a music-library workflow.
**Rigor-creep into the default path is henceforth a defect.** The full proof
machinery survives — behind an opt-in setting.

## 1. Measured reality (empirical, this machine)

Harness: `live_mount_perf` (ignored test, `src/tui/keybindings.rs`,
`TONEPOET_PERF_DIR=<mount> cargo test --release -p tonepoet --lib -- live_mount_perf
--ignored --nocapture`). It drives the real `FileTaskWorker` end-to-end and
compares with a plain `std::fs::copy` loop over the same tree.

Release build, 2026-07-26, this machine:

| Mount | Tree | Plain copy baseline | Engine copy | Engine move (same mount) |
|---|---|---|---|---|
| local ext4 (NVMe) | 192 MiB / 24 files | 110 ms | **3.59 s (32.5× baseline)** | **1.36 s** (a rename loop is ~1 ms) |
| fuse.sshfs (user's ~/torrents) | 32 MiB / 8 files | 683 ms | **3.96 s (5.8× baseline)** | 457 ms (rename-first + degraded ladder — this part works) |

Notes for interpretation:
- The ext4 copy multiplier (32.5×) has two co-dominant terms: the redundant
  verification passes (SHA-256 over 2×S bytes) AND the per-file fsyncs of §2.2
  — the plain-copy baseline never fsyncs, so on fast storage the fsync policy
  is as large a factor as the hashing. Both are §3.1 targets.
- The ext4 same-mount move (1.36 s for what is one rename syscall) is purely
  the §2.1 manifest hash. It scales linearly with S: a 10 GB album ≈ 70+ s.
- On sshfs the same-mount move is already acceptable (the 839baab rename-first
  ladder). The copy multiplier (5.8×) is the 2×S read + per-file round-trip
  chatter over the network; a cross-device MOVE onto sshfs runs the §2.2
  portable-policy 4×S pattern and is proportionally worse.
- Measure with `--release` only: debug builds inflate SHA-256 ~20-50× (an
  earlier debug run of the same ext4 case showed 41 s / 390×; that number is
  NOT representative and must not be quoted).

The user's field report matches: operations that should be instant (same-mount
move = one rename) or disk-speed (copy) are perceived as many times too slow.

## 2. Where the time goes (mechanically verified inventory, all file:line cited)

For one operation on a tree of N files / S total bytes. `state.rs` /
`source_guard.rs` = `crates/tui-file-picker/src/…`; `rename_plan.rs` =
`src/tui/rename_plan.rs`.

### 2.1 MOVE, same device (native-rename route — the everyday case)

| Stage | Site | Cost |
|---|---|---|
| `capture_manifest(source)` | state.rs:4563 → source_guard.rs:6652-6756 | **reads + SHA-256-hashes all S bytes** (hash loop source_guard.rs:6701-6715); ~1 open + 3 stats per file; 1 full tree walk |
| `RenameSourceProof::capture` | state.rs:4582 | root-only; cheap |
| the actual rename | state.rs:4592 | 1-2 syscalls |
| `verify_committed_rename` | state.rs:4602 | stat-level; cheap |
| parent-dir fsyncs | state.rs:4653-4668 | 2 |

**A same-mount move of a 10 GB album reads and hashes 10 GB to perform one
rename.** For a first-time move the manifest's only consumer is the undo
`MoveRecoveryProof` (state.rs:4619-4633); it contributes nothing to no-clobber
(the rename ladder provides that) and nothing is copied.

LOAD-BEARING nuance (audit-verified twice): on a *replay* of a move (undo/redo),
the freshly captured manifest's digests ARE consumed — `verify_captured_replay_source`
compares them against the operation-time digests (state.rs:4568-4580 →
source_guard.rs:2528). `Option<ContentDigest>` compares `None == None` as equal,
so digest-free standard-mode manifests pass this gate ONLY if both capture and
operation-time proof are at the same authority level; a mixed strong-proof /
standard-capture replay hard-fails with a misleading "content changed" error.
The §3.3 authority-level requirement is therefore mandatory plumbing, not
defensive nicety.

### 2.2 COPY (paste), and MOVE via copy-then-delete (cross-device)

| Stage | Site | Content I/O |
|---|---|---|
| copy to staging, SHA-256 inline on the same read | state.rs:6160/6255/6420, hash at 6452-6471 | read S + write S (this part is fine) |
| **`sync_all` per file + `sync_directory` per staged dir** | state.rs:6497, 6335 | N + D fsyncs |
| publish rename | state.rs:6188 | cheap |
| **full destination rehash** `capture_verified_copy_at` | state.rs:6208-6212 → source_guard.rs:3251 | **reads S again — on BOTH mount policies**; 2nd full walk |
| (move only) quarantine + verified deletion | state.rs:5005-5398 | 3rd walk; strict mounts: stat-level for tree DESCENDANTS only — a regular-file quarantine ROOT is still rehashed even on ext4 (`strict_descendant = strict && !moved_root`, source_guard.rs:3501-3503); **portable mounts (sshfs/NTFS/cifs/exFAT/unknown): reads S a 3rd time** (source_guard.rs:3538) |
| (move only) destination re-verify before each unlink | state.rs:5354-5415 → source_guard.rs:2599-2673 | strict: stat-level; **portable: reads S a 4th time** (2656 → 3405) |

**Bottom line: copy = 2×S read + 1×S write + N fsyncs. Cross-device move on the
user's actual mounts (fuse.sshfs, ntfs-3g are both "portable" per
source_guard.rs:287-307) = 4×S read + 1×S write.** The mounts users actually
move media across do the MOST redundant I/O — over the network, four times.

### 2.3 RENAME (every inline rename, bulk rename, fixcaps rename)

`execute_plan_with_proofs_internal` (rename_plan.rs:247-425) runs
`capture_manifest` (rename_plan.rs:293) → **full subtree walk + SHA-256 of all
S bytes under the renamed root** before two syscall-level renames. Renaming an
album folder hashes the whole album. Post-commit verification is stat-level
(`verify_renamed_destination`, source_guard.rs:925-1044, zero content reads) —
the preimage hash is ~100% of the cost.

### 2.4 UNDO / REDO replay

- Rename replay re-enters the same path: **one full re-hash per replay**
  (rename_plan.rs:293, comparison at 300-313). Rename → undo → redo of a 10 GB
  album reads ~30 GB total.
- Copy-undo runs **four per-entry verification passes** (two identical
  `verify_cleanup_tree_at` calls at source_guard.rs:6495 + 5830, then per-entry
  re-verification again in preflight 5839-5883 AND commit 5943-6012); on
  portable mounts up to 4×S content rehash for one undo.

### 2.5 METADATA writes (user-confirmed slow; verified by two independent auditors)

Two write surfaces exist with DIFFERENT rollback authorities (the split the
parked transaction-hardening memory records — do not unify them here, but do
not deepen either):

**(a) Inline Browse edit, generic formats (WAV/AIFF/MP3/M4A/APE/WavPack/OGG)
— the expensive case.** `write_metadata_field_transactional_with_control`
(src/tui/probe.rs:5209) → `db.atomic_metadata_write` (src/db.rs:2381):
full copy of the ENTIRE audio file to a rollback marker (`std::io::copy` +
`sync_all`, db.rs:2478-2479) + parent sync, a 5-statement SQLite journal state
machine (SELECT + INSERT allocating + UPDATE prepared + UPDATE committed +
DELETE; each write commit fsyncs the WAL), the lofty in-place rewrite (ID3v2
writer does `read_to_end` of the whole file and writes it back), destination +
parent + marker-removal syncs. **Per field: ~2×S read + ~2×S write, 5 explicit
fsyncs + ~4 WAL fsyncs.** Inline edits are inherently per-field: editing three
fields of a 60 MB WAV/MP3 moves ~720 MB. (No content re-read/verification
happens after the write — the cost is backup + journal + rewrite + durability.)

**(b) Metadata editor save (`:w`), generic formats.** Batches ALL changed
fields into ONE write per file already (probe.rs:7538-7584 → one
`write_all_tags_with_cancel_report_classified` per file, 4-way parallel) via
the legacy standalone `.tonepoet-bak` full-file copy (probe.rs:8410-8412,
db.rs:2960): ~2×S bytes but only 1-2 fsyncs and ZERO SQLite transactions —
notably it never fsyncs the rewritten destination (asymmetry worth fixing in
whichever direction is chosen). It also runs the lofty parse twice (preflight
probe.rs:8403 + write 8417) — a free small win.

**(c) Native FLAC and DSF — ALREADY CHEAP; EXEMPT from the rewrite directive.**
FLAC inline/editor writes go through the native metadata-region writer: with
sufficient padding the edit is an in-place metadata-region-only overwrite with
a region-only journal — **KB-scale regardless of file size** (probe.rs:1815,
1973-1995; journal stores only `raw_metadata_region`, :3146). Only tag-growth
overflow triggers a full-file `stream_rewrite` (probe.rs:1998-2051) which is
ALREADY write-to-temp + rename and grows padding by 1 MiB so the next edit is
in-place. DSF is analogous (bounded tail journal, dsf_tags.rs:682-785).
Applying "no journal, temp+rename" to these would REGRESS KB-scale edits to
S-scale. Available wins there: fsync-count trims only (~6-7 per edit today).

**(d) Pipeline/conversion tagging is OUT OF SCOPE** — it uses external taggers
(metaflac/opustags/wvtag/AtomicParsley) on staged artifacts, one invocation
per track, no backup, no journal (stages.rs:4166→5197). Fixcaps and
GNUDB/MB populates are memory-only until editor save.

Standard-mode direction (generic formats only): crash safety via temp + atomic
rename-into-place, with these audit-established constraints:
- Lofty 0.21 has NO write-to-destination API (`save_to_path` rewrites in
  place). The 1×S floor is achievable ONLY via the in-memory route: read file
  into `Vec<u8>` once (1×S read), run `save_to` against `Cursor<Vec<u8>>`
  (lofty's `FileLike` is implemented for it — zero disk I/O), write the buffer
  to a temp (1×S write), one `sync_all`, rename into place. Needs a RAM guard
  (~S transient, up to ~2×S): above a size threshold fall back to
  copy-to-temp + `save_to_path(temp)` + rename (≈2×S — still no backup copy,
  no journal).
- Keep ONE pre-write journal/marker check: startup recovery
  (`recover_stale_metadata_writes`, main.rs:2492) restores old bytes over the
  file for an armed PREPARED journal entry (db.rs:2667-2680) — a standard-mode
  write that ignores an armed journal or a stale legacy `.tonepoet-bak`
  (db.rs:2443-2448) would be silently destroyed at next startup. "Zero journal
  transactions" means zero WRITES; the armed-state refusal check stays.
- The temp+rename writer must mirror the FLAC overflow rewrite's existing
  guards: symlink refusal (probe.rs:2081), hardlink refusal (:2099),
  permission/ownership preservation (:2016/2038), restrictive temp mode.
  Note: nothing preserves xattrs today on any path, and temp+rename drops
  them where in-place kept them — disclose in the engineering report.
- Multi-field batching for the editor surface already exists (see (b)); the
  inline surface is inherently per-field — its win is cheapening the per-write
  machinery, not batching.
- Strong mode: today's journaled/backup paths unchanged.

### 2.6 Already fine (do not regress)

- User-facing DELETE is already minimal (keybindings.rs:36032-36080: guards +
  `remove_dir_all`). No proof machinery on that path.
- Nothing heavy runs on the UI thread; workers are real threads. The progress
  channel is throttled and cheap.
- The inline SHA-256 during the copy read (state.rs:6452-6471) rides a read
  that must happen anyway — CPU-only, keep it available for strong mode.
- No-clobber is carried entirely by stat-level checks + the `rename_no_replace`
  ladder + `create_new` (state.rs:6151, 6447-6450, 6188) — cheap, and stays in
  every mode.

## 3. Required design: two verification modes

### 3.1 `standard` (the DEFAULT)

Safeguards kept, all stat-level or riding existing I/O:
- No-clobber everywhere (ladder + `create_new` + planner collision validation).
- Copy-loop length check (state.rs:6481-6490): "the copy completed and is the
  right size".
- Identity/staleness checks that need no content read: dev/ino/size/mtime
  snapshot comparisons (the machinery already proves strict-mount descendants
  without rehash — source_guard.rs:3505-3525; `verify_renamed_destination` is
  already zero-read).
- Degraded-mode disclosure stays (one honest line, quiet by default per §5).

Costs removed from the default path:
1. **No content hashing in `capture_manifest` for the default mode** — capture
   stat-only manifests. This single change fixes same-mount move (§2.1),
   rename (§2.3), and replay re-hash (§2.4) at once. Undo proofs become
   identity-level: undo still verifies "same object, same size/mtime, nothing
   appeared/vanished" before acting — it no longer proves content-equality.
   That is the accepted trade-off; disclose it in the engineering report, not
   to the user per-op.
   **FIRST BLOCKER (audit-found): `SourceManifest::insert` hard-rejects
   digest-free regular-file entries** ("regular-file proof is missing a content
   digest", source_guard.rs:3199-3208), and `cleanup_manifest`
   (source_guard.rs:2484) inherits the same invariant — a `digest: None`
   standard-mode manifest is currently UNCONSTRUCTIBLE. Relaxing this (via a
   `ManifestDepth`/authority-level variant, not by silently dropping the
   invariant for strong mode) is the first change; everything else in this
   section depends on it. The identity machinery itself (snapshots,
   `verify_same_object_and_version*`, tree-membership checks at
   source_guard.rs:2500-2507) genuinely needs no content reads.
2. **No post-copy destination rehash** (drop stage state.rs:6208-6212 in
   standard mode; destination proof = size + identity snapshot captured from
   the just-written files' metadata).
3. **No pre-delete content rehashes** on either mount policy; quarantined
   cleanup verifies membership + identity only, and the four passes collapse
   into at most two (one tree verification + commit-time per-entry open checks).
4. **fsync policy**: at most ONE `sync_all` per root (final publish) + parent
   dir sync, not per file / per staged dir. (Data-loss window on power failure
   = same as `cp`; acceptable default.)
5. Portable-mount policy in standard mode must NOT add content passes that the
   strict policy skips. Weaker identity tokens on those mounts mean weaker
   undo authority — disclosed, not compensated with 4×S reads.

### 3.2 `strong` (opt-in)

Exactly today's behavior: full SHA-256 manifests, post-copy rehash, verified
4-pass quarantine deletion, per-replay content proof. No code deleted — the
mode flag chooses at the existing call sites.

### 3.3 Plumbing (research already done — use these seams)

- `FileOperationPolicy` (state.rs:368-391) flows into the picker engine's
  paste/copy (state.rs:6141), move (state.rs:4535/4553), and copy/move replay
  (keybindings.rs:1796-1799): add `verification: VerificationMode` there for
  those paths.
- **The RENAME path has NO policy plumbing at all (audit-refuted seam —
  do not assume it exists):** `execute_plan_with_proofs(_and_expected_sources)`
  (rename_plan.rs:230-245) takes only plan + proofs; `spawn_rename_plan`
  (keybindings.rs:1387) passes plan/description/tx; `execute_transactional_rename_replay`
  (keybindings.rs:1586) has no policy parameter; `capture_manifest`
  (source_guard.rs:6648) is a free function with no mode. The implementing
  model must thread the verification mode through all three rename UI entries
  (`commit_browse_rename` keybindings.rs:34852, `execute_bulk_rename`
  ~27243, `rename_paths_with_case_transform` ~1505) → `spawn_rename_plan` →
  both execute functions → the capture call, plus the rename replay.
  (Archive-entry rename branches earlier into archive-rewrite machinery and is
  out of scope — leave it.)
- Persisted setting: new `[file_operations]` table in `TonepoetConfig`
  (config.rs — nothing verification-related exists yet):
  `verification = "standard" | "strong"`, default `standard`.
- Surface: advanced/config screen + a vi command (`:set verification=strong`
  or similar). Per-mount identity policy (`FilesystemIdentityPolicy`,
  source_guard.rs:180-183) stays automatic — the user knob selects the
  verification depth, not mount classification.
- `capture_manifest` gains a depth parameter (or a sibling
  `capture_identity_manifest`); type-level guarantee that strong-mode proofs
  and standard-mode proofs are not confused (undo entries must record which
  authority level they carry, and replay must verify at the SAME level —
  a standard-mode proof must never satisfy a strong-mode gate). Audit notes on
  feasibility: NO existing enum/field carries authority level anywhere in the
  proof chain (`SourceEntryProof` source_guard.rs:2395, `FileTaskRootProof`
  progress.rs:308-311, `FileOperationUndoMapping` app.rs:10382-10404) — this
  is new surface, but cheap: the undo journal is in-memory only (no serde, not
  persisted), so no migration. Without an explicit level field,
  `verify_captured_replay_source`'s digest check (source_guard.rs:2528) passes
  `None == None` vacuously and fails mixed levels with a wrong error message —
  the level field must gate BEFORE the digest compare. Digest comparisons that
  must become level-aware: source_guard.rs:2528, 3565 (`verify_cleanup_tree_at`
  path), 5562 (`unix_verify_opened_entry`).

## 4. Performance budget (acceptance criteria — tests must pin these)

On a local strict mount, release build, tree of ≥16 files / ≥128 MiB.
The counting mechanism ALREADY EXISTS: `FileOperationIoCounters`
(source_guard.rs:24-41) threads `source_bytes_hashed` /
`destination_bytes_hashed` / `bytes_redundantly_rehashed` / `file_sync_calls` /
`directory_sync_calls` through every path in §2 — pin the counted criteria on
it, no new shim needed.
1. Same-device MOVE: **zero content-byte reads** — PINNED via counters and/or
   digest absence in the captured manifest. (Wall-time is NOT a pinned
   assertion — timed comparisons go in the harness report only; a 50 ms wall
   bound in the always-on suite would be flaky.)
2. COPY: exactly 1×S read + 1×S write; `source_bytes_hashed == 0` in standard
   mode (the inline SHA-256 is DROPPED in standard mode — identity-level
   proofs don't use it, and hashing 10 GB costs ~7 s of CPU even riding a free
   read; strong mode keeps it); fsyncs ≤ roots + parents, never per file — all
   PINNED via counters. The ≤1.5×-of-plain-copy wall target is measured by the
   `live_mount_perf` harness for the engineering report, not pinned in the
   suite.
3. RENAME of a directory: zero content reads (assert identity-level manifest).
4. UNDO of a rename/move: zero content reads in standard mode.
5. Strong mode: unchanged behavior, existing proof tests keep passing under
   the strong flag (flip the relevant tests to construct strong-mode policy).
6. Portable mounts: same zero-content-read budget in standard mode (the 4×S
   pattern must be strong-mode only). AUDIT NOTE: no test seam exists today to
   force portable classification (`TONEPOET_REDUCED_FS_TEST_DIR` needs a real
   reduced mount; `TEST_FORCE_COPY_THEN_DELETE_MOVE` forces the route, not the
   policy) — add a capability-override test seam (test-only constructor or
   env-gated injection into the capability cache) so this criterion is
   pinnable; otherwise it ships as an env-gated ignored test plus harness
   evidence.

7. METADATA single-field edit, GENERIC formats, standard mode: 1×S read +
   1×S write via the in-memory lofty route (≤ RAM threshold; above it, the
   copy-to-temp fallback's ≈2×S is the accepted floor) + ≤2 fsyncs; zero
   backup-copy bytes; zero journal WRITE transactions (the armed-journal /
   stale-bak refusal check stays — see §2.5). Native FLAC/DSF are exempt
   (already KB-scale; do not regress them — optionally trim their fsync
   count). Multi-field edits from one editor session: ONE rewrite total —
   ALREADY TRUE today (probe.rs:7538-7584); pin it with a test rather than
   re-implementing. Pipeline/conversion tagging out of scope. Strong mode:
   today's journaled backup paths unchanged.

The `live_mount_perf` harness output for both modes and both mounts must be
retained as completed handoff evidence. To keep the source overlay immutable
and rerunnable, the acceptance runner may place the after tables and raw logs
in a separate atomically generated, independently checksummed results artifact
referenced by the engineering report. Extend the harness (or add a sibling) to
time a single-field metadata edit on a ≥50 MB file, both modes.

## 5. Secondary items (same bundle, small, fully specified)

### 5.1 Quiet status bar for file operations
Cut/copy/paste/rename/delete currently narrate on the status bar. Default:
suppress routine success/progress status messages for file ops; keep errors
and degraded-mode warnings. A verbosity control gates the chatter — extend the
existing `:file-notices quiet|verbose` runtime toggle
(`file_task_verbose_degrade_notices`, app.rs:10602, command.rs:4192-4210, not
currently persisted) into a persisted `[file_operations] status_verbosity =
"quiet" | "verbose"` setting covering both degrade notices and routine
narration. `:messages` retains the full record regardless (that surface is
load-bearing since 839baab — do not regress it).

### 5.2 Progress dialog: close-on-success option
Add a clickable option to the move/copy progress overlay (suggested label:
`[x] Close when done`) — when enabled and the task finishes clean, the dialog
dismisses itself without requiring OK. The checkbox state persists to
`~/.config/tonepoet/config.toml` (`[file_operations] auto_close_progress =
true|false`, default false). Mouse contract per the standardized round-4
rules; no F-keys; no new glyphs beyond the functional set.

Audit-verified implementation facts + decisions (do not re-derive, do not
guess):
- Nothing auto-closes today; dismissal is `FileTaskUserAction::Acknowledge`
  only (keyboard progress.rs:880/890, mouse :932; handled keybindings.rs
  ~11890-11955). The correct auto-close hook is `reduce_file_task_complete`
  (event_loop.rs:1185-1245): the authoritative `FileTaskCompletionReport`
  lands there and `terminal_update_from_completion_report`
  (event_loop.rs:1138-1183) already computes the needed predicate. Retained
  state for `:messages` is installed in the same reducer BEFORE any close —
  auto-close must not skip that (it doesn't if hooked after retention).
- "Finishes clean" is DEFINED as: terminal `Finished` with zero errors, zero
  `CompletedWithWarning` roots, AND zero skipped roots. A partially-skipped
  task stays open — skips are surprising outcomes the user should see.
  (There is no warning counter on `ProgressTotals` and no severity on
  `FileTaskErrorRecord`; warnings exist only as per-root
  `CompletedWithWarning` dispositions, progress.rs:271 — use the report.)
- Scope: the checkbox renders on move/copy (and undo/redo replay) progress
  overlays — the flows that produce completion reports. Delete/Archive/other
  `FileTaskKind` overlays finish via a report-free path
  (`reduce_file_task_progress`, event_loop.rs:1001) with no warning signal:
  do NOT show the checkbox there in this round.
- Wiring: the checkbox is crate-rendered UI inside `FileTaskProgressState`,
  but the SETTING is host-owned. Recommended shape: host passes the initial
  value into the progress session, the crate surfaces toggle clicks as a new
  `FileTaskUserAction::ToggleAutoClose` (same channel as Acknowledge), and the
  host persists on toggle. Equivalent alternatives are acceptable; pick one
  and say so in the engineering report.

### 5.3 Inline-editing cursor color: congruent, not just contrasty
Field complaint: the embedded editing cursor reads as a maroon-ish block on the
user's theme — brilliant contrast, but incongruous with the palette. Cause: the
cursor-outside-selection cell is hard-wired to `bg = theme.info` + BOLD
(`src/tui/inline_edit.rs:57-59` in `editing_cell_styles`), so it inherits
whatever the palette's info accent happens to be.

Required change: promote the editing-cursor surface to a proper themed element
instead of borrowing `info`. The naive version of this spec was audit-refuted
on the numbers — follow this corrected one:
- Add an `editing_cursor` derived element (derived-elements table
  `DERIVED_SPECS`, src/tui/theme.rs:731-761, is the established pattern).
  Default surface formula: the **amber accent** (`accents[12]` / `WARM_ACCENT`,
  present with sane values in all 22 builtin palettes — user's stated
  preference).
- **The cursor FOREGROUND must be computed, not inherited**: today's pattern
  (`fg = theme.bg`) fails the 4.5:1 glyph floor on every light palette when
  the surface is amber. Rule: fg = whichever of the palette's dark/light poles
  (e.g. `theme.bg` vs `theme.text_bright`) yields the higher contrast against
  the final cursor surface; floor ≥ 4.5:1.
- **Surface floor**: cursor surface vs `input_focused_bg` ≥ 3.0:1. Where raw
  amber fails this (measured: solarized-light 2.40, everforest-light 1.92,
  rose-pine-dawn 1.71), apply a DETERMINISTIC adjustment, not a different hue:
  step the amber's lightness away from `input_focused_bg`'s pole (darken on
  light themes, lighten on dark) in fixed increments until the floor passes
  (bounded steps; assert convergence in the test).
- **Do NOT specify a cyan fallback.** Audit-critical fact: `theme.cyan` and
  `theme.info` are the SAME accent slot (`accents[14]`, theme.rs:194 vs 201) —
  "fall back to cyan" re-delivers exactly the color complained about. The
  deterministic lightness adjustment above IS the fallback.
- **Drop the "≥3:1 vs the selection surface" numeric floor** — unsatisfiable
  by any light accent on dark palettes (the inline selection surface is
  `text_bright`; amber measures ~1.3-1.7 against it everywhere). Distinctness
  from the selection surface is carried by the existing 4-state matrix
  style-distinctness contract (cursor never reuses the selection style) plus
  BOLD, which is unchanged.
- Extend the per-palette contrast test
  (`inverse_selection_pair_clears_contrast_thresholds_in_every_builtin_theme`,
  src/tui/inline_edit.rs:332) to assert the TWO floors above (glyph ≥ 4.5,
  surface-vs-input_focused_bg ≥ 3.0 post-adjustment) for every builtin
  palette, light themes included.
- The theme builder lists it (derived tab) so users can override it; custom
  themes without the key use the formula.
- Update the pinned cursor-style regression tests (they currently assert
  `bg == theme.info`, e.g. inline_edit.rs and the round-4 select-all test);
  the 4-state matrix contract itself is unchanged.

## 6. Constraints (standing, unchanged)

- NO function-key bindings (byobu). NO emojis / decorative unicode.
- Ctrl+Q quit must not be shadowed.
- Version stays 0.4.4.
- `cargo test --workspace` must stay green (baseline 5,162/0); never truncate
  gate output.
- The `:messages` diagnostics surface and the degraded-rename ladder from
  839baab are load-bearing; do not regress them.
- Delete remains non-undoable (no journal variant exists — by design).

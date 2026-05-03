# Tonepoet: Session Continuation Prompt

## What is tonepoet?

Tonepoet is a Rust CLI+TUI audio conversion toolkit. Read `CLAUDE.md` in the project root for build instructions, workspace structure, key types, and coding conventions. Everything builds inside `nix develop`.

## Where we are

The CTDB Reed-Solomon repair pipeline is **functionally complete and compiles clean**, but **has not been validated end-to-end against real CDs yet**. The next session is dedicated to debugging the integration — exercising it with actual rips, surfacing latent bugs, and tightening the user-facing flows.

The approved repair plan is at `docs/ctdb-repair-plan.md`. The previous two sessions implemented the plan; nothing in the plan is outstanding.

### What was built and audited in the previous sessions

| Area | Status |
|------|--------|
| RS codec (`src/ctdb_rs/mod.rs`) | 5 unit tests pass; 1109 LOC; the underlying math is solid. |
| Per-track repair (`ctdb::repair_album`) | Wired end-to-end: download parity, decode, assemble disc image with STRIDE leadin/leadout, RS repair, split, re-encode, copy metadata, per-track CRC32 verify, backup/restore replace. |
| Single-image CUE repair (`ctdb::repair_single_image`) | Decode once, repair the whole image, re-encode single file, per-track CRC verify via CUE boundaries, single-file backup/restore. |
| AR offset auto-detect | `detect_ar_offset_from_cache()` returns `Option<i32>`; `Some(n)` for confirmed cached value (n may be 0), `None` to force a fresh `:ar` run. |
| Deferred-AR flow | If AR cache is empty, `:ctdb-repair` stashes a `PendingCtdbRepair` in `AppState`, dispatches `:ar`, and the `AccurateRipComplete` handler resolves the offset and pops the repair confirmation. |
| Context-menu / direct `:ctdb-repair` from no overlay | Sets `auto_repair_on_ctdb_complete`, dispatches `:ctdb`, and the `CtdbComplete` handler picks the first repairable page and re-dispatches `Command::CtdbRepair`. |
| Confirmation messages | Distinguish "from AR cache", "verified by AR", "from AR verification", and "AR could not determine a drive offset — proceeding at +0 may produce incorrect repairs" (yes/no dialog gives the user the call). |
| Safety net | Post-repair CRC32 must match the CTDB-database expected value for **every** track before originals are touched. Backup/restore on any failure. |
| Clean compile | `cargo check` zero warnings; `cargo test --lib` 195 pass + 1 pre-existing baseline fail (`command_completion_prefix_con_matches_convert` — unrelated, "context" was added before this work). |

### Where bugs are most likely to surface

These are the places to probe first when something misbehaves on a real disc:

1. **Single-image repair, every step.** `repair_single_image` at `ctdb.rs:840` was implemented from spec; it has never been run against real audio. Specific risks:
   - **Embedded `CUESHEET` block loss** — `copy_metadata_metaflac` in `accuraterip.rs:2165` copies tags + embedded picture but does not preserve the FLAC `CUESHEET` block. The external `.cue` file is unaffected, but tools that read embedded CUE will lose it. Pre-existing helper limitation; if a user reports it, add `--export-cuesheet-to`/`--import-cuesheet-from` calls.
   - **Encode for unusual extensions** — `encode_corrected_track` in `accuraterip.rs:2061` falls through to FLAC compression flags for unknown extensions. Single-image albums in ALAC/m4a, WavPack, APE are explicitly handled; others (e.g. raw PCM, `.tta`) will not round-trip correctly.
   - **Decode/probe length mismatch** — `info.total_samples` (set at single-image detection) versus `decode_track_to_raw_i16(&audio_path).len()` (used at repair time) are computed via different paths. If they disagree, `compute_suffix_skip(info.total_samples)` may not align with the actual audio buffer length. Verify with a real disc.
2. **Deferred-AR offset path with multi-disc selections.** The match-by-first-track-path logic (`event_loop.rs:847`) handles the typical case but has not been exercised with a real multi-disc album. If `pending_ctdb_repair` doesn't match any AR page, pending is preserved (correctly, but the user sees the AR overlay instead of the expected repair confirmation).
3. **Single-image AR cache storage.** `db.rs::store_ar` does `DELETE WHERE file_path = ?1` then INSERT in a per-track loop; for single-image albums where N tracks share `info.audio_path`, only the last track's cache entry survives. `detect_ar_offset_from_cache` works around this in practice (drive offset is uniform across tracks in the real world), but the cache is technically lossy here. **Pre-existing**, not introduced by the repair work — flag for later if cache reliability matters.
4. **Recursive command dispatch.** `Command::CtdbRepair` no-overlay branch dispatches `Command::Ctdb`, and the `CtdbComplete` handler re-dispatches `Command::CtdbRepair`. Each dispatch returns synchronously. Should not loop, but watch for it if the user sees status flicker or duplicate confirmations.
5. **Pre-emphasis discs.** CRC32 is computed over raw PCM regardless of pre-emphasis state, so this should not affect repair correctness — but the project memory note `project_preemph_dr.md` flags pre-emph for analysis. If a repair on a pre-emph disc fails CRC verification, suspect the AR/CTDB database having a different pre-emph assumption rather than the repair pipeline.
6. **`:ar` failures.** If `:ar` fires (deferred path) but the multi-disc all-fail branch in `command.rs:1812` skips the `AccurateRipComplete` send, `pending_ctdb_repair` lingers and is consumed by the next unrelated `:ar`. Worst case is a spurious confirmation the user cancels; not a data-loss path.

### Key files

| File | What's there |
|------|--------------|
| `docs/ctdb-repair-plan.md` | The approved plan — source of truth for design decisions. |
| `src/ctdb_rs/mod.rs` | RS codec API: `CtdbCodec::repair()`, `STRIDE = 11_760` (i16 count), `RepairResult`, `RepairError`. Offset arg is CD stereo sample-pair count. |
| `src/tui/ctdb.rs` | `repair_album` (line ~638), `repair_single_image` (line ~840), `download_parity`, `compute_suffix_skip`, `compute_track_crc32`, verify functions. |
| `src/tui/command.rs` | `Command::CtdbRepair` handler (line ~1819, with-overlay + no-overlay branches), `detect_ar_offset_from_cache` (line ~2800). |
| `src/tui/app.rs` | `ConfirmAction::CtdbRepair` / `CtdbRepairSingleImage`, `PendingCtdbRepair`, `pending_ctdb_repair`, `auto_repair_on_ctdb_complete`. |
| `src/tui/event_loop.rs` | `CtdbComplete` handler with auto-repair re-dispatch (~line 734); `AccurateRipComplete` handler with deferred-repair consumer (~line 847). |
| `src/tui/keybindings.rs` | `execute_confirm_action` arms for both `CtdbRepair` and `CtdbRepairSingleImage` (~line 5474–5503). |
| `src/tui/accuraterip.rs` | `detect_uniform_offset`, `encode_corrected_track`, `copy_metadata`. AR offset is in stereo sample pairs. |
| `src/tui/cue_parser.rs` | `SingleImageInfo`, `detect_single_image`. `track_boundaries` are `(start_sample, count_sample)` in stereo pairs. |
| `src/tui/context_menu.rs` | "CUETools DB repair" Verify-submenu entry dispatching `Command::CtdbRepair`. |
| `src/db.rs` | `get_cached_ar` / `store_ar` (single-image cache caveat above). |

### How to verify

1. `nix develop --extra-experimental-features 'nix-command flakes'`
2. `cargo check` — should be clean, zero warnings.
3. `cargo test --lib` — 195 pass, 1 pre-existing fail (`command_completion_prefix_con_matches_convert`).
4. Real-world smoke matrix to step through:
   - Per-track album, AR cache present (offset 0 and offset != 0): run `:ctdb` → `:ctdb-repair` from overlay; expect "from AR cache" confirmation.
   - Per-track album, no AR cache: `:ctdb-repair` from overlay → expect "Running AccurateRip..." status → confirmation with derived offset.
   - Per-track album, no overlay (context menu "CUETools DB repair"): expect verify run → first repairable disc auto-selected → confirmation pops.
   - Single-image CUE album with mismatches + parity: expect end-to-end repair, originals replaced only after all-track CRC32 verify.
   - Single-image CUE album with no mismatches: status "No mismatches detected — repair not needed", originals untouched.
   - Multi-disc album with mixed mismatches: confirm first repairable disc is auto-selected.
   - Disc not in CTDB: status "No parity data available" or "Disc not in CUETools database".
5. Audit after each bug fix. The user's standing rule: "Whenever we fix bugs we change code, whenever we change code we re-audit." Re-audit not just the fix but the surrounding context.

### Not committed / not pushed

The current branch (`main`) has all the repair work staged in the working tree but **not committed**. The user has been explicit about not committing without permission. Do not run `git commit` or `git push` unless asked.

### User preferences (carried over)

- **Develop a rigorous plan before implementing. Report the plan for approval. Do not freewheel.**
- Audit after implementation. Re-audit after fixing bugs found in audit.
- Concise communication, no unnecessary summaries.
- Commit and push only when asked.
- Use `nix develop` for all builds. Do not use system Rust.
- For yes/no decisions affecting the user, use the existing `ActiveOverlay::Confirmation` dialog rather than silent fallbacks.

# Handoff — Performance Restoration Round (bundle readme)

**Governing document:** `docs/perf-restoration-brief.md`. Read it in full
before touching anything; every §2 claim is audit-verified to the line and
several name defects in prior deliveries — do not re-derive the seams it
corrects (§3.3 rename plumbing, the manifest insert invariant, the lofty
in-place constraint, the cyan==info accent slot).

**Baseline:** branch `hardening` @ 83fe80e; `cargo test --workspace` =
5,162 passed / 0 failed across 56 targets. Version stays **0.4.4**.
The governing brief's `839baab` identifier is the earlier feature-lineage commit for
the degraded-rename ladder, not this overlay's apply target. Verify that ancestry
in the complete repository and require every supplied preimage hash to match.

## Scope, in priority order

1. §3 standard/strong verification split for copy/move/rename/undo, meeting
   the §4 budget (pin counted criteria on the existing
   `FileOperationIoCounters`).
2. §2.5 metadata-write standard mode (generic formats only; FLAC/DSF exempt).
3. §5.1 quiet status bar (persisted `[file_operations] status_verbosity`).
4. §5.2 progress-dialog close-on-success (persisted, report-carrying kinds
   only).
5. §5.3 `editing_cursor` derived theme element (amber, computed fg,
   deterministic lightness-step fallback — NO cyan fallback).

## Non-negotiable constraints

- The user's directive in §0 governs: rigor-creep into the default path is a
  defect. Strong mode preserves today's behavior exactly; no proof machinery
  is deleted.
- No function-key bindings; no emoji/decorative unicode; Ctrl+Q stays quit;
  version stays 0.4.4; delete remains non-undoable.
- Do not regress: the `:messages` diagnostics surface, the 839baab degraded
  rename ladder, native FLAC/DSF metadata writers, the standardized mouse
  text contract, the 4-state cursor matrix distinctness contract.
- Authority levels: a standard-mode proof must never satisfy a strong-mode
  gate, and level checks run BEFORE digest comparison (see §2.1/§3.3).
- Standard-mode metadata writes keep the armed-journal/stale-bak refusal
  READ (see §2.5) or startup recovery destroys them.
- On Windows, journal-free generic metadata commit uses `ReplaceFileW` with
  `dwReplaceFlags = 0` as the sole replacement primitive. Do not add a
  `MoveFileExW` fallback: documented failure states can already have changed
  the namespace, and a rename fallback does not preserve the destination's
  DACL and other Windows file properties.

## Deliverables

- Overlay bundle (tar.gz, nested dir) with a preimage manifest covering every
  modified file (SHA-256 of the exact base revisions you received).
- Immutable engineering report plus source- and bundle-bound generated acceptance evidence stored outside the tested repository.
  The source report records the design, before measurements, counter evidence,
  named pinning tests, residuals, and deviations. `RUN_ACCEPTANCE.sh` writes the
  completed release tables plus the exact tested diff/tree authority outside the
  overlay, checksums every tested source file, and supports a read-only `--verify`
  rerun. Do not edit a source postimage after acceptance. Release numbers only —
  debug SHA-256 is inflated 20-50x and must not be quoted.
- New/updated tests keep `cargo test --workspace` green; tests you add for
  the budget must FAIL if a content-read regression is introduced later.

## Harnesses available to you (already in tree, ignored by default)

- `live_mount_perf` (src/tui/keybindings.rs): engine-vs-baseline timing on
  any mount via `TONEPOET_PERF_DIR`.
- `live_mount_repro` (src/tui/rename_plan.rs): drives the real rename engine
  incl. proof capture on any mount via `TONEPOET_REPRO_DIR`.

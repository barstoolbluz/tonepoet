# Metadata transaction-authority hardening — design set (PRESERVED for a future dedicated project)

Status: **DESIGN REFERENCE ONLY. The implementation these docs describe was
REJECTED as-delivered.** Preserved 2026-07-23 during the metadata-autonumber
corrective rounds, at the user's direction, so the design thinking survives for a
future, dedicated hardening project.

## What this is

During the metadata-autonumber v6 corrective round (which asked for **3 small
test fixes**), the reasoning model instead diagnosed a real architectural smell —
the **split transaction authority** for metadata writes: generic Lofty mutations
(inline edits, full-editor writes, ReplayGain cleanup, AccurateRip tag copy,
generic artwork) used a standalone `.tonepoet-bak` backup/restore path, separate
from the DB-journaled crash-recovery protocol used by the native FLAC/DSF
writers. It designed and implemented a **unified, DB-backed, file-scoped
transaction authority** ("descriptor-bound native writes recovery") to replace
the split, threaded through every metadata writer.

The nine reports below (internal iterations V24–V31) are the design output.

## Why the implementation was rejected (do NOT reuse the v31 code as-is)

The delivered bundle
(`metadata_autonumber_descriptor_bound_native_writes_recovery_v31_fixed_bundle.tar.gz`,
repo root) was a ~15K-line change across 28 files (`db.rs` +10K, `probe.rs`
+2.7K, `dsf_tags.rs` +1.9K, materializers, `processor.rs`, ReplayGain, …). It
was gated with the real toolchain and **failed decisively**:

- Did **not compile** as delivered (10 compile errors; fixed locally only to run
  the gate).
- **90 test failures**, including **17 in its own new `db::tests`** (the new
  transaction subsystem does not pass its own tests).
- **Regressed previously-green core code**: 17 `materializer_cue` failures (CUE
  image decomposition — boundary math, multifile pregap, track numbering,
  UTF-8/BOM metadata) and 9 `dsf_tags` failures (DSD-adjacent).
- Tripped the `subprocess_stdin_convention` sentinel (new subprocess launches
  inheriting stdin — the hang class the codebase guards against).
- **Did not even fix the 3 target bugs** it was sent for, and **regressed
  `id3v2_numbering`** which was green in v5.

So the *idea* is sound; the *code* is not. It violated the corrective round's
explicit "preserve everything else" scope and put the working conversion +
DSD-adjacent paths at risk.

## For the future dedicated project

- Reimplement the transaction-authority unification **correctly, in its own
  scoped reasoning-model session**, gated hard (full suite + DSD checkers +
  smoke + the stdin sentinel), not smuggled into a feature apply.
- First establish whether the split `.tonepoet-bak` path is a **real risk on
  main today** (does it actually lose data / fail to recover?) or working-but-
  inelegant — that sets the priority.
- Fold in the **3 still-open metadata-autonumber bugs** (they were the original
  target and intertwine with the write path): APE (WavPack) disc-number
  round-trip gap, and the ID3v2 no-op-preflight stale-snapshot correctness bug.
  See `docs/brief_metadata_autonumber_corrective_v6.md` for their exact evidence.
- The rejected implementation attempt (full v31 code) lives in the tarball above
  as a reference for what was tried; it needs 10 compile fixes + a full rewrite
  of the failing paths, so treat it as a sketch, not a base.

## The preserved design reports (V24–V31)

- `IMPLEMENTATION_REPORT_METADATA_CARRIER_LOCAL_AUTHORITY_V24.md`
- `IMPLEMENTATION_REPORT_METADATA_CARRIER_GENERATION_READ_BARRIER_V25.md`
- `IMPLEMENTATION_REPORT_METADATA_SHARED_READ_GENERATION_TOKEN_PARENT_RENAME_V26.md`
- `IMPLEMENTATION_REPORT_METADATA_DESCRIPTOR_BOUND_UNIFIED_NATIVE_AUTHORITY_V27.md`
- `IMPLEMENTATION_REPORT_NATIVE_DESCRIPTOR_BOUND_WRITES_RECOVERY_V28.md`
- `IMPLEMENTATION_REPORT_NATIVE_DESCRIPTOR_BOUND_WRITES_RECOVERY_V29.md`
- `IMPLEMENTATION_REPORT_NATIVE_DESCRIPTOR_BOUND_WRITES_RECOVERY_V30.md`
- `IMPLEMENTATION_REPORT_NATIVE_RECOVERY_ARTIFACT_RETIREMENT_V31.md`
- `IMPLEMENTATION_REPORT_METADATA_TRANSACTION_LIVENESS_HARDLINK_PATHS.md`

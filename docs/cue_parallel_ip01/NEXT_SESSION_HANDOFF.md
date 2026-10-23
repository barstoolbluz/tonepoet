# TonePoet CUE IP-01 R2 corrective — durable handoff

**Status:** `implemented-unqualified` / **NOT ACCEPTED**. Do **not** start IP-02 from an unaccepted packet.

## Read order

This handoff → `IMPLEMENTATION_STATUS.md` → `CORRECTIVE_AUDIT_ADJUDICATION.md` → `VERIFICATION_RESULTS.md` → `CONFORMANCE_MATRIX.md` → `TIMING_RECIPE.md` → `ARCHITECTURE_DECISIONS.md` → `planning_authority/08_HOST_GATE_AMENDMENT.md` → `planning_authority/packets/IP-01_observe_baseline.md`.

## Sources and exact archive identity

Pristine source: `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz`, SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. Reported Git commit `98aae2b` cannot be verified from a git-less archive. Normative amended plan SHA-256 `b64b05c23afb0accf8679a26e91f5306ba1a8e6b8f8e8e654b60e2d077f96d47` is preserved byte-for-byte under `planning_authority/`. Rejected original IP-01 archive SHA-256 `4c5626722df94860e49127ff7675f533eeb77bc44cede7711a922e63db90ec99` is the predecessor, **not the accepted IP-02 input**.

**Corrective R2 archive identity:** See the external `TonePoet_CUE_IP01_R2_DELIVERY_RECEIPT_<SHA12>.md` delivered alongside the full cumulative source. An archive cannot include its own SHA-256; verify this externally. Run `sha256sum -c docs/cue_parallel_ip01/SHA256SUMS` after extraction. Validate `SOURCE_MANIFEST.json` and `PRODUCTION_DIFF.patch` against the original snapshot.

## Correction scope

Audit D-01--D-06 addressed in the same three Rust modules plus offline summarizer, tests, worksheet and handoff docs. H-01 belongs to IP-06's evaluator and is **not patched or quietly reinterpreted** here. Full defect-by-defect evidence is in `CORRECTIVE_AUDIT_ADJUDICATION.md`.

The same existing `TONEPOET_PIPELINE_BASELINE_JSONL` remains opt-in. Native ReplayGain scan-only elapsed joins require an exact CUE run token from the worker. Source scan application is settled only after actual stage Result; selection and completed observation alone do not count as applied metadata. Physical CUE group identity survives a recovered S1 retry. Pre-admission queue deferrals and track authored ordinals are now visible.

**The amended ReplayGain sequencing correction remains normative; no source-pass ReplayGain observer was implemented under IP-01.** No S3, speculative PX-C, source PCM transport or scheduler redesign.

## Qualified-host mandatory checks

Run the exact commands in `VERIFICATION_RESULTS.md` on the **exact R2 source** with Rust 1.93+ (or the pinned compatible project toolchain). Preserve Rust stderr, test counts and binary SHA-256; a zero-test filter is not a PASS. Execute diagnostic-on/off child tests and both direct/fallback routes, real source-pass eligible and ineligible, failed native tag writer, observer causal failure/recovery, real post-output measurement, queue cancellation before/after admission and terminal publication/failure. Compare complete original vs R2 output PCM hashes, frame counts, tags/artwork, ReplayGain, logging, cancellation, and atomic publication. Measure opt-in overhead.

Fill host `TIMING_WORKSHEET.csv` with matched RG Off/Both S1 FLAC+CUE trials, exact fixture/CUE/tool/config/binary SHA values and action→visible time. Keep incomplete/foreign native scan events as `NOT INSTRUMENTED`; do not backfill estimated durations. Later IP-06 S1/S3 comparisons must fix H-01 cross-mode common configuration digest before promotion. No S3 benchmark has been performed.

**Next packet:** only the exact R2 `TonePoet_CUE_IP01_FULL_SOURCE_<SHA12>.tar.gz` once host tests and media parity have passed and signoff records the actual accepted SHA. If tests fail, return to the implementing session for another small corrective archive. Neither R1 nor R2 archive creation equals packet acceptance.

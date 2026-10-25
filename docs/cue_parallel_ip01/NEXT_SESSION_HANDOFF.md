# CURRENT IP-05 corrective handoff — 2026-10-25

**Correction D-01/D-02:** Added the full authored CUE count to the group-level
S3 prelaunch gate and a named 12-authored/2-selected regression; removed two
inherited Python bytecode caches from the delivered full source. The previous
IP-05 archive `4467bd785bbb...` is **superseded**. Read
`ip05_verification/CORRECTIVE_D01_D02_2026-10-25.md` and use the **new**
external delivery receipt for this exact source/archive identity. The rest of
IP-05 remains unchanged, `implemented-unqualified`, S3 default-OFF, host
Rust and real album qualification **NOT RUN**. IP-06 stays blocked.

---
# Original IP-05 implementation handoff — 2026-10-25 (historical after corrective)

**State: `implemented-unqualified`; S3 DEFAULT OFF; NOT ACCEPTED.** Input is owner-accepted IP-04 `TonePoet_SNAPSHOT_9c06f0f_2026-10-25.tar.gz`, SHA-256 `de88377467699875b265231893623b49ee1cfd6a68c20b284a6bf361444defa0`. The supplied owner identity note verifies IP-04 host compilation/qualification; older in-archive IP-04 candidate "not accepted" headings are historical and should not be treated as current. This source is a Git archive without `.git`; owner-reported commit `9c06f0f` is not independently verifiable here.

**Read first:** `ip05_verification/README.md` → `ip05_verification/HOST_GATE.md` → `ip05_verification/VERIFICATION_RESULTS.md` → `ip05_verification/CONFORMANCE_MATRIX.md` → `ip05_verification/ARCHITECTURE_DECISIONS.md` → `ip05_verification/SOURCE_MANIFEST.json` and `SOURCE_DIFF.json` → `planning_authority/packets/IP-05_integrate_pilot.md`, then code.

**Implementation:** ONLY `src/convert/pipeline/stages.rs` and `track_executor.rs` differ among input production modules. One hidden, process-scoped `TONEPOET_CUE_S3_PILOT=1` exact opt-in routes a complete, eligible physical FLAC+CUE album through IP-04 independent sample-qualified workers, IP-03 shared native concurrency budget and ordinary album post-processing/publisher. Default or rejected request uses S1 exactly. Selected pilot uses ordinary native *post-output* ReplayGain, never the incompatible S1 source-pass FIFO. Full group preflight occurs before first child. No child failure is retried under S1. Nothing new is written into visible destination, which must contain only requested output audio and enabled log, CUE and artwork. This is **not a claim** of actual destination parity: host must check the output set.

**Verified here:** native FLAC exact decoded PCM probes PASS (16/24 bits, 3/12/30 tracks, gapped range); destination-auditor unit self-test PASS; source static inventory PASS after build; Rust compile/format/tests and actual TonePoet end-to-end conversion **NOT RUN** (toolchain absent). The input owner separately reported 7618 pass/2 non-IP-05 failures; do not attribute these or claim this IP-05 test suite passed. Evidence under `ip05_verification/`.

**Exact generated output identity / next input:** the deliverable's **external** SHA-bound `TonePoet_CUE_IP05_DELIVERY_RECEIPT_<SHA12>.md` records the complete new archive's literal filename and full SHA-256. An archive cannot embed its own checksum. After all `HOST_GATE.md` gates pass and owner accepts IP-05, **and only then**, IP-06 consumes that **exact verified IP-05 archive**. Until then preserve the accepted IP-04 tarball as rollback and leave the pilot flag unset. Source control commit/merge is for the host; this candidate has no `.git`.

---
# Current IP-04 corrective handoff — October 25, 2026

**Status: `implemented-unqualified` — S3 OFF — NOT ACCEPTED.** D-01/D-02/D-03 are corrected in the candidate source; unexecuted Rust tests remain unqualified. Preserve the accepted IP-03 source `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz` (`fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7`) and the pre-correction IP-04 candidate (`662379ddebf6...`) as rollback/review inputs. Read `ip04_verification/CORRECTIVE_D01_D03_2026-10-25.md`, `ip04_verification/NEXT_SESSION_HANDOFF.md`, and host gate commands. New archive identity is in the **external** delivery receipt; no `.git` in the archive. No normal dispatcher may call the private worker.
---

# IP-04 handoff — CURRENT (October 24, 2026)

**`implemented-unqualified` — S3 OFF — NOT ACCEPTED.** Exact input `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz` SHA-256 `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7` is host-accepted IP-03 by supplied identity letter; earlier in-archive IP-03 candidate notes are historical. Current candidate output's true full SHA-256 and file name are in the **external** `TonePoet_CUE_IP04_DELIVERY_RECEIPT_<sha12>.md`. Read `ip04_verification/NEXT_SESSION_HANDOFF.md` and `ip04_verification/VERIFICATION_RESULTS.md` first. 42/42 independent native reference PCM slices passed, but **Rust build/tests NOT RUN** because no toolchain. Qualify IP-04 on a Rust host before doing IP-05; keep accepted input as rollback and preserve S1 selection unchanged.

---

# IP-03 durable handoff — current (2026-10-24)

**Status: `implemented-unqualified` / NOT ACCEPTED. IP-04 is blocked until owner/host acceptance.** This current handoff supersedes the preserved IP-01 historical handoff below.

**Input:** `TonePoet_SNAPSHOT_d8c25d3_2026-10-24.tar.gz`, SHA-256 `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95`. Origin letter reports `main` commit `d8c25d3`, clean and pushed; the archive has no `.git` to independently prove those Git assertions. Host input notes 24/24 FLAC seek cells, 840/840 ranges, FFmpeg 7.1.3. Its 7601/7602 Rust gate contains one known Reference qualification lock drift failure.

**Read order:** this file → `ip03_verification/README.md` → current `IMPLEMENTATION_STATUS.md` → current `ARCHITECTURE_DECISIONS.md` → current `CONFORMANCE_MATRIX.md` → current `VERIFICATION_RESULTS.md` → `ip03_verification/STATIC_AUDIT.md` → `planning_authority/packets/IP-03_global_resource_admission.md` → `planning_authority/packets/IP-04_parallel_worker_private.md`. Previous IP-01/IP-02 evidence and amended plan preserved without modification.

**Change:** only `src/convert/pipeline/track_executor.rs` differs from input among original production modules. Adds inert global S3 in-flight cap to already global `ToolConcurrencyLimits`, a nonblocking pair reservation and an opt-in/private `run_cue_s3_pipeline_if_admitted`. Unit/probe tests assert FFmpeg/FFmpeg two permits, FFmpeg/SoX one each, partial declines, one-token host, ordinary FFprobe coexistence, two albums sharing the cap, 200 attempts, release on cancellation/panic/error, closed family, real supervised early-exit/slow-pipe/cancel. No production S3 call site exists; old S1 reference is untouched.

**Qualification:** Native FFmpeg pair resource envelope and exact 16 ranged reconstructed PCM comparisons PASS in the independent Python harness. `cargo`/`rustc`/`rustfmt` unavailable, so ten authored Rust tests, real supervisor execution, native child reaping, broad regression and full application resource measurements are NOT RUN. Legacy S1 segmented FFmpeg/FFmpeg remains one token; do not assert an absolute total-native-FFmpeg cap when legacy paths overlap. The current input's known reference lock-source drift is separate and must be recorded in the full suite. Critical IP-04 future prerequisite: integrate the IP-02 independent exact-byte extent contract into the real supervised pipe before allowing any private output to count as verified. A successful FFmpeg exit alone is not proof of full sample extent.

**Archive identity / next packet input:** the archive's **actual** name and SHA-256 are written to the separate `TonePoet_CUE_IP03_DELIVERY_RECEIPT_<SHA12>.md` accompanying the tar, because embedding an archive's own digest would change the digest. The receipt is the binding identity for **the one exact next IP-04 input**. Do not infer the file name from this source tree or begin IP-04 until the host has signed off IP-03 with that exact hash. If a gate fails, produce a small corrective IP-03 successor and new receipt instead.

**Rollback:** exact retained accepted input bytes are the external archive named above; no feature flags or user paths need switching, because the new S3 branch is not called. All modifications are additive to global limits while inactive; restore the input `track_executor.rs` to remove them.

---

## Preserved IP-01 historical handoff

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

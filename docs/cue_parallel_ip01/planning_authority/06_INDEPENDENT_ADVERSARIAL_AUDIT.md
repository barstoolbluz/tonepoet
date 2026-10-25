# Independent adversarial architecture and packet audit — TonePoet parallel CUE R1

**Date:** 2026-10-22  
**Audited planning archive:** `TonePoet_CUE_Parallel_Plan_R1_98aae2b_2026-10-22.tar.gz`  
**Audited archive SHA-256:** `a2251e223fd094719aa0878b88258c98a03b8c0536a760ccbba9452ab4c16ada`  
**Original brief:** `TonePoet_Parallel_CUE_Implementation_Planning_Prompt_R2_2026-10-22.md` (mounted copy has ` (1)` suffix; text identical to attached brief)  
**Baseline source:** `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz`  
**Verified baseline source SHA-256:** `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`  
**Review mode:** independent source/static, script execution and limited FFmpeg fixture smoke test. **No TonePoet production code modified. No Rust build or TonePoet CUE performance trial performed.**

## Executive decision

**R1 is not approved unchanged.** Its primary architecture is defensible: preserve the existing single-source sequential transport (S1), then qualify a **narrow, sample-exact FLAC range worker (S3)** and select it only when end-to-end conversion, including the Convert UI's ReplayGain default, proves faster. Its physical versus logical source analysis, sample-boundary policy, planner admission, metadata authority and old-route fallback are materially correct. The source does **not** substantiate a current 2–5x bottleneck cause: only a possible bottleneck and a plausible parallel remedy are established.

Three actionable problems were identified. One would let a future session offer a route the real host runner cannot execute, one commits a disproportionate cross-cutting resource-manager change before establishing any incremental need, and one lets the bundled trial helper label incomparable configurations a performance candidate. They are corrected in **this R2 planning bundle**, without expanding the proposed decoder design or implementation packet count. Additional performance representativeness and telemetry-scope adjustments are recorded as proportionality reductions, not as standalone mandatory engineering machinery.

**R2's recommended first shippable functionality:** an explicitly gated, qualified *single physical FLAC+CUE image → ordinary FLAC tracks* parallel candidate, retaining S1/file-backed behavior elsewhere and current ReplayGain Both, metadata, artwork, logs, cancellation and album publisher. No new audio parser, general codec registry, DSP engine, publisher, or global resource scheduler. Six sequential packet IDs are retained to avoid a needless redesign of handoff contracts; the previously overbuilt IP-03 is now narrowly scoped. Each packet still requires acceptance evidence against its *actual* cumulative full-source archive.

## Validation inventory: what was proved versus not proved

| Assertion | Evidence and disposition |
|---|---|
| Source archive authenticity | SHA-256 computed on attached 17-MB source archive; **MATCH** to source identity letter. No `.git` in archive; branch, commit and clean tree are reports, not independently verified Git assertions. |
| Planning archive integrity | SHA-256 above computed independently; R1 `SHA256SUMS` verified for every listed payload; `tools/verify_bundle.py .`: **PASS, 28 hashed payload files, 6 prompts, acyclic DAG**. No missing named planning deliverable. |
| Source citations | R1 `tools/verify_source_refs.py . <extracted-source>`: **154 numerically valid anchors; zero errors**. That script checks path existence / line bounds, not semantic correctness. Independently inspected the relevant cited Rust functions described below. |
| Scripts | `python3 -m py_compile tools/*.py` and help interfaces passed. `tools/generate_cue_fixture.py --minutes 1 --rate 44100 --depth 16 --tracks 12` produced a valid 60-second stereo/16-bit FLAC+CUE and per-file checksums with installed FFmpeg/FFprobe. Its source fixture is **not** a TonePoet benchmark or a seek proof. |
| Baseline Rust test results | Accompanying identity letter reports 7,575 passing tests and zero warnings on a different host. `cargo`/`rustc` not available here: **NOT RERUN**. |
| Ranged FLAC exactness | Still an unqualified hypothesis beyond R1's stated nine-offset FFmpeg experiment. IP-02 must run real boundary and error tests before S3 use. |
| Speed advantage and correct output equivalence | **NOT MEASURED** in R1 or this review. No S3 implementation exists. No 1.25x, 2x or 5x result may be claimed. |

## Source-level adjudication of the architectural premises

1. **Queue and physical-image identity: supported.** `src/convert/queue_expansion.rs:723-945,1487-1807,1844-2265` handles CUE association, merge, embedded/sidecar selection and multi-FILE forms. `src/convert/pipeline/materializer_cue.rs:491-690,2335-2492` makes per-physical-image stream segments and sample ranges from CUE INDEX 01, retaining known exact sample authorities. The plan correctly refuses to reparse synthesized CUE text inside parallel workers.
2. **An actual S1 fast path already exists: supported.** `src/convert/pipeline/stages.rs:44190-44224` builds a raw FFmpeg source command. `:45202-45585` preflights all member planner shapes and splits each source into exact byte ranges; `src/convert/pipeline/tool.rs:2374-2773` owns supervised segmented transport. Consumers are sequential within a physical-image group. Do not assert eager PCM staging on this qualified path.
3. **One disqualified member can force fallback for that physical group: supported.** `src/convert/pipeline/stages.rs:45202-45372` has prelaunch group-plan admission. This is a reachable extra-I/O cliff but its frequency and cost are not measured. It does not mean other independent queue work is serial.
4. **Separate albums and other track work already have concurrency: supported.** `src/convert/processor.rs:5350-5555` submits one scheduled work unit for a collection of adjacent CUE streams, and `src/convert/pipeline/scheduler.rs:1-165` serves queue work from a shared pool. The remaining serial region is mainly intra-source encoding/group iteration.
5. **ReplayGain defaults need care: supported.** Convert UI starts at `Both` (`src/tui/app.rs:5546-5554`), but generic pipeline settings default to no scan (`tonepoet-pipeline/src/settings.rs:118-145`). The eligible Unix S1 route mirrors selected PCM to native observers (`src/convert/pipeline/stages.rs:44299-44325,44441-44943`), avoiding a second output scan in some cases. Proposed S3 output-scan fallback can be semantically correct but its **extra output traversal may erase a speedup**; the R1 full-run trial gate correctly refuses to ignore this.
6. **Finalization and safe publication remain distinct: supported.** `src/convert/pipeline/stages.rs:45014-45200` records direct results and validates staged audio. Album metadata/ReplayGain and publication run later (`:15554-15870,27544-28100`; `src/convert/pipeline/transactional_state.rs:20-128`). No worker may publish directly. The R1 plan preserves this.
7. **Critical existing assets omitted from the original architecture cost accounting:** `ToolConcurrencyLimits` already shares global per-family semaphores (`src/convert/pipeline/track_executor.rs:709-804`; `src/convert/processor.rs:2167-2185`), and `ToolRunner::run_pipeline` supervises a two-child producer/consumer directly (`src/convert/pipeline/tool.rs:2170-2372`). A new general worker scheduler or transport is unnecessary for one qualified S3 worker.

## Genuine defects (rules 1–3)

### D1 — High: S3 can be admitted where the production two-process runner does not exist

**Triggering scenario:** A desktop on Windows has FFmpeg installed. An operator opts into the proposed S3 pilot. The IP-02 standalone FFmpeg seek tests establish correct FLAC sample ranges and the proposed admission sees a compatible source, output plan, tool version and two-worker budget. The implementation calls `ToolRunner::run_pipeline` to join the range decoder to the existing encoder. That production runner returns `Unsupported` on non-Unix, regardless of successful independent seek testing.

**Incorrect result:** The optimization path fails at worker execution instead of rejecting itself *before* work admission. This is a predictable normal-user platform incompatibility, not a speculative hostile edge case. A failed new route must not imply the baseline Windows streaming route works: the current segmented runner also has a non-Unix `Unsupported` branch. The requirement is **no new regression and truthful capability admission**, not an unrelated Windows transport rewrite.

**Brief requirement:** §4 (lines 77–81): "How work admission decides among qualified execution modes" and "When sequential processing will intentionally remain faster or safer." §6 (line 120): "Correct stop/cancel/kill behavior, deterministic output association, clean retries, conflict handling, and no change to existing failure policy without explicit approval." §8 (line 165): "Have an explicit rollback strategy or retain a proven fallback."

**Source evidence:** `src/convert/pipeline/tool.rs:2170-2372` implements `run_pipeline` only under `#[cfg(unix)]` and refuses on `#[cfg(not(unix))]` at `2361-2372`. `:2374-2787` shows the same platform limitation for S1 segmented piping. R1 requires qualified decoder tool/version (ADR `02_ARCHITECTURE_DECISION.md:30-36`) but does not explicitly gate **native supervised pipeline availability**; `packets/IP-04_parallel_worker_private.md:17-20` asks for reuse of that runner, and IP-05's admission list at `packets/IP-05_integrate_pilot.md:17-22` does not explicitly reject an unsupported runner.

**Detailed correction:** At the existing prelaunch CUE-group route selector, require both (a) an exact qualified seek method and (b) a production `ToolRunner`/platform that supports the supervised two-process pipe. For the current source, a narrow `cfg!(unix)`/production-runner capability check is sufficient; fake runners may expose a test-only capability when appropriate. On failure, do not start a range decoder and return to the preexisting route with a clear unsupported-runner reason. Do **not** request a new Windows pipe implementation in the first critical path. Preserve group plan, ReplayGain, publisher, metadata and failure handling.

**Focused verification:** Unit admission test on an unsupported-runner fake records zero S3 subprocess launches and the same old-route result; a separate production `run_pipeline` smoke test on each admitted Unix host exercises actual decoder→consumer piping, midpipe cancellation and child reaping. A standalone Windows FFmpeg sample test cannot substitute for this. Keep IP-02's byte comparisons independent of this orchestration capability.

**R2 correction:** Added explicit source/runner separation to `01_SOURCE_AUDIT.md`, ADR, format matrix, conformance contract and IP-02/IP-04/IP-05 prompts. No unrelated platform work added.

### D2 — High: IP-03 mandates a new cross-cutting resource system despite an adequate shared core

**Triggering scenario:** A fresh implementing session follows R1 `packets/IP-03_global_resource_admission.md:11-29` literally, adding global weighted process/RAM/FD/I/O reservation ownership, lock ordering, predictions and runner handoffs across the shared tool executor. That central path is used by ordinary non-CUE conversion, Reference execution and native tools. Broad changes threaten existing tests and serial-route throughput *before* the user receives a parallel CUE speed benefit, and may disable S3 outright on a normal machine for lack of a configured FD estimate.

**Why the assignment is wrong:** This is a **planning-scope violation**, not a claim that a new implementation already corrupts audio. The attached brief §4 (line 71) explicitly directs: "Choose the smallest architecture that has a credible path to materially improved elapsed time" and "Do not duplicate TonePoet's existing planner, DSP engine, execution recorder, or publication system merely to add concurrency." §8 (line 161) requires "one cohesive change with an explicit boundary, avoiding simultaneous unrelated refactoring." §4 (line 78) separately requires global CPU/process, RAM, FD and I/O budgets; the fix must **respect** that requirement, not simply delete resource admission.

**Source evidence:** `src/convert/pipeline/track_executor.rs:709-804` has shared family semaphores and a global parallelism-derived limit; `src/convert/processor.rs:2167-2185` shares it across the active processor. `src/convert/pipeline/track_executor.rs:11607-11661` counts one permit per distinct family, which undercounts a *simultaneously* live FFmpeg decoder+FFmpeg encoder pair; `src/convert/pipeline/tool.rs:2170-2372` already supervises the two-child pipeline. The actual incremental gap is **two-child accounting for S3**, plus a bounded in-flight worker count. R1 `02_ARCHITECTURE_DECISION.md:38-45`, `03_IMPLEMENTATION_PLAN.md:12-16,29-35`, and IP-03's Objective and source contract turn it into a general weighted scheduler.

**Detailed correction:** Keep the same six packet IDs but narrowly redefine IP-03 to acquire an appropriate pair of existing shared per-family permits *for one new S3 pipeline*. Two simultaneous FFmpeg children require two permits, FFmpeg + SoX one of each. Acquire the complete set before spawn (atomically/nonblocking per family or with canonical no-wait release), own via a single RAII guard, then call the already-supervised `ToolRunner::run_pipeline` **without a nested limit acquisition**. A single globally shared live-S3 count limits workers across albums; bounded existing pipes and fixed per-worker process/files/descriptors imply O(N) RAM/FD/source-read upper bounds. Measure normal-host high water and cap N. This addresses §4 global admission in proportion to the *fixed new worker shape* without claiming impossible exact RAM/I/O bandwidth metering.

Existing S1's family-based accounting must be examined in the coexistence test: when legacy S1 + S3 run together and a real child-count cap would be exceeded, narrowly account the relevant *concurrently live* segmented children or decline extra S3 admission. Do **not** silently rework all existing tool execution, and do not increase serial-route cost unmeasured. Reuse canonical track plans, `run_pipeline`, cancellation and publisher as before. A blocked reservation is a benign S3 refusal, not a failed conversion.

**Focused verification:** Two-FFmpeg and FFmpeg+SoX permit counters; one-token host declines S3 without deadlock; simultaneous independent albums and an unrelated FFprobe workload remain bounded and finish; cancellation, early consumer exit and spawn failure return all permits; 200-track fixture never has more than N live decoder/encoder pairs; memory and FD high water stay within documented per-worker budget. Run unchanged S1 and Reference regression tests to establish that feature-OFF performance/behavior is unchanged.

**R2 correction:** IP-03, ADR, implementation DAG, status and other worker/activation prompts now require a **minimal additive permit adapter**, not a cross-application reservation architecture. This is a reduction in implementation surface, not a request for extra resilience machinery.

### D3 — Medium: bundled trial evaluator can call incomparable S1/S3 timings a performance candidate

**Triggering scenario:** During IP-06, S1 and S3 runs use the same input FLAC and broad cohort labels (`host`, `storage`, `cache_state`, `tracks`, `replaygain`), but the S3 operator accidentally changes the FLAC compression level, artwork setting or encoder backend. Because the initial helper groups only on those broad labels plus source and fixture hashes, all five pairs can be accepted and a ratio above 1.25 can be printed as `CANDIDATE`. That is an incorrect actionable comparison, even though the prose separately instructs operators to keep settings equal.

**Brief requirement:** §7 (line 136): "Specify output format/encoder settings, hardware, storage, build variant, cache state, and repetition policy." §7 (line 138): "Adopt or reject an optimization based on correctness first and credible elapsed-time benefit second; do not count simply launching more workers as success."

**Evidence:** R1 `tools/evaluate_trials.py:15-16,34-65` did **not** carry a settings/build/backend identity in required CSV fields or its grouping key. `05_CONFORMANCE_AND_PERFORMANCE.md:49-62` and IP-06's test requirements correctly require matched settings verbally, but the included calculation could still mislead a subsequent agent.

**Detailed correction:** Require one `configuration_sha256` value in every CSV row. Compute it from a canonical manifest of the comparison's TonePoet build identity, tool versions, requested output format and codec/backends, compression/profile, source target rate/depth, ReplayGain/artwork/verification settings, and staging/destination policy, **excluding only the S1-vs-S3 route selection**. Include it in the helper's matching cohort. An S1/S3 mismatch then cannot form a valid pair; the tool reports insufficient paired trials instead of a false candidate. Keep the helper intentionally small: it remains an analytical screen, not a conformance/correctness oracle.

**Focused verification:** Construct five legitimate same-config S1/S3 pairs and verify the helper reports an eligible candidate only when ratios support it. Change S3 config digest for one or all S3 rows; expect incomplete pairs and **no `CANDIDATE`**. Reject malformed digest strings. Numeric evidence still cannot override audio, metadata, resource or publication gates.

**R2 correction:** `tools/evaluate_trials.py` requires a canonical-configuration digest and includes it in pairing; 05 and IP-06 say how to produce/use it.

## Proportionality check

The R1 **choice of S4/S3**, retaining S1 and all old output/publishing semantics, is appropriately scoped. The overbuild was principally **IP-03's global weighted resource redesign**, now removed. IP-01 originally invited a new versioned JSONL trace schema and instrumentation over UI→queue→stages even though `ConversionRunTiming` (`stages.rs:16541-16565`) and `SchedulerMetrics` (`scheduler.rs:81-149`) exist. R2 asks IP-01 to reuse those first, with opt-in local route/phase hooks only for a measured gap; an external action-to-visible stopwatch is an acceptable end-to-end baseline until a production hook is demonstrably needed. No new default telemetry, duplicated ledgers, crash-recovery, hostile-filesystem, or general worker abstraction is justified.

The selected **FLAC seek qualification before a vertical slice** is correct: precise input ranges are an actual uncertainty and producer output cannot be safely assumed exact. The 6-packet segmentation is conservative, but keeping the existing IDs and reducing IP-03 is less disruptive than inventing an entirely new sequencing scheme in this corrective handoff. The IP-05 integration packet should remain a narrow call into the current scheduler/stages and existing postprocessing barrier; if it unexpectedly demands a new publisher or album engine, stop rather than expand the assignment.

## Alternative architectures and critical-path assessment

- **S1 retained sequential**: clear performance reference; lowest I/O and startup. Appropriate for one or a few short tracks, HDD/network files, or unqualified source/plan.
- **S2 shared decoder with concurrent encoders**: attractive only with material overlap. A bounded producer cannot advance into a far-away track while a previous encoder consumes minutes of PCM without buffering/staging substantial data. It may overlap encoder finalization, but cannot credibly promise a large album speedup as the primary fix. Reject as default critical path pending measurement.
- **S3 independent decoder seek + encoder**: credible on seekable FLAC from SSD when CPU compression is costly; bounded PCM memory. Requires independent sample-exact seeks and checks because decoder seek behavior and cost are not established by static Rust inspection. Preserve S1 as fallback. Best first single-image candidate.
- **S4 limited capability-selected hybrid**: selected. Select S3 only for exact FLAC range, authorized simple encoder, supported Unix runner, source/album policy, process budget, and representative end-to-end benefit. Do not generalize to lossy/DSD/chapter inputs before testing.
- **Independent physical images**: existing `for group in groups` in `stages.rs:44441-44943` is serial within an album work unit, so concurrency between independent physical images is a plausible lower-risk later acceleration. It is *not* a replacement for the brief's required common **single-image** vertical slice. Treat as provisional if user data shows multi-FILE workloads justify it.

**Critical path:** R2's IP-01 (narrow measurement) → IP-02 (FLAC range go/no-go) → IP-03 (small shared S3 permits) → IP-04 (private direct worker) → IP-05 (default-off existing lifecycle integration) → IP-06 (qualified whole-run decision). First safely shippable functionality after IP-05 and build-host validation; default promotion only after IP-06. Do not turn contingent future codec families into prerequisites. Real toolchain/test absence remains NOT RUN, and cannot be reinterpreted as passing.

**Performance representativeness adjustment (not a new infrastructure defect):** A generated `anoisesrc` FLAC (R1 `tools/generate_cue_fixture.py:54-63`) is useful for sample identity but not fully representative of encoder cost on normal musical material. R2 keeps the synthetic cohort and requests **one available representative music or realistically compressible album** before turning on automatic default selection; no new library of fixtures, no commercial data bundling, and no requirement to access foobar2000.

## Optional, for the human to decide

- If real data shows multi-FILE CUE albums are common, a later isolated scheduling packet could overlap independent *physical* images using the existing shared queue and publisher; not a prerequisite for FLAC S3.
- If IP-06 shows ReplayGain Both erases S3's net gain, a narrowly integrated per-range native source observer may be worth a follow-on trial; do **not** implement it speculatively.

## Hand-off correction and source safety

All R2 work here is **plan/scripts only**. Baseline source SHA stays `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. All implementation packet IDs and provisional codec-family scopes are retained. `NEXT_SESSION_HANDOFF.md` still starts IP-01 against that original full source, not the planning archive. Later sessions must use the actual accepted cumulative source archive, record host commands/results as PASS/FAIL/NOT RUN and preserve an exact next-session input/handoff. The R2 bundle's manifest and checksums are generated after the amendments, not inherited from R1.

**Original R1 audit disposition:** Not approved unchanged; material planning corrections supplied in R2. **R2 is an executable plan, not an implementation or runtime qualification.**

Verdict — Fix defects

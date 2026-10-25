# CURRENT IP-05 corrective architecture — 2026-10-25

**D-01:** The selection-filtered prepared/scheduled track counts must also
equal the unfiltered authored CUE `album_metadata.total_tracks`; this check
precedes the existing typed planner. It does not depend on whether a full
selection was specified as All, Range or Set. **D-02** affects packaging only:
compiled Python bytecode caches are omitted. No new subsystem or publisher.

---
# Original IP-05 architecture — 2026-10-25 (retained)

Narrow reversible hidden `TONEPOET_CUE_S3_PILOT=1` at the existing CUE album encode-work boundary. Complete physical FLAC+CUE, exact sample/image facts, typed planner, unchanged-rate/depth FLAC, source metadata and native child budget all preflight before spawn; rejected albums use old S1. Real selected workers reuse IP-04 exact-range verified-pipe and IP-03 shared semaphores with cancellation-aware wait on transient contention. Existing processor, metadata, native post-output RG and album publisher remain the owners. S1 source-pass FIFO cannot be attached to parallel readers, therefore selected S3 uses post-output ReplayGain, respecting user settings. Fixed PCM gain is explicitly ineligible even when not true-peak. Destination contains only requested files. No new scheduler or output control files. Host qualification pending; details in `ip05_verification/ARCHITECTURE_DECISIONS.md`.

---
# IP-04 architecture decision checkpoint — 2026-10-24

New private CUE S3 execution uses IP-02 range qualification, IP-03 weighted RAII permits, an opt-in checked `ToolRunner` pipe, and the existing direct-plan/finalization and staged result. No separate scheduler/encoder/metadata/publisher and no general process-management rewrite. The upstream unchecked two-child `run_pipeline` and S1 semantics are unchanged. Private S3 result ordering is detached from native worker completion order. For full reasoning see `ip04_verification/ARCHITECTURE_DECISIONS.md`.

---

# IP-03 addendum — shared two-child permits

1. **Existing tool semaphores remain authoritative.** Add only a globally shared `cue_s3_in_flight` semaphore to `ToolConcurrencyLimits`; every S3 worker counts two actual children with `try_acquire_many_owned` (FFmpeg/FFmpeg) or one token from each family (FFmpeg/SoX). No generic RAM/FD/I/O scheduler and no changes to the ordinary S1 or single-tool limiter.
2. **Admission is immediate, complete, and all-or-nothing.** Reserve the S3 slot, then SoX (if needed), then FFmpeg; no `.await` occurs while a partial set is held. Tokio RAII drops a partially acquired set on failure/cancellation/panic. `Closed` and invalid types are errors, not ordinary capacity deferrals.
3. **One execution lifetime for both children.** Keep the guard through `runner.run_pipeline` (the existing supervised Unix pipe). The API returns `Declined` only before spawn, versus `Executed(Result)` after attempting the real pipeline. A failed decode/encode cannot silently retry as S1. No `run_tool_command_with_concurrency` / reference-permit double charge.
4. **Bound S3 independently across albums, but do not misstate old S1.** Hard cap `N <= 2`, further bounded by FFmpeg/2, with a single OS pipe and a fixed number of 64 KiB tool tails for each worker. New S3 contributes at most two new native children per admitted worker. Historical S1 segmented FFmpeg/FFmpeg consumes only one family token: the test identifies this known undercount, so no assertion of an absolute cap across *all* existing operations is made.
5. **No source execution or user activation in IP-03.** Typed FLAC range-to-planner worker and exact stream extent integration are IP-04; album ownership, ReplayGain, metadata and publication are IP-05. S3 is unreachable from ordinary conversion routes until those gates complete. Preserve the amended IP-06 ReplayGain-Off/Both measurement gate.
6. **Qualification split:** Authoring host lacks Rust; all new Rust tests remain NOT RUN. The executed native FFmpeg fixture is independent resource/PCM evidence, not a substitute. Actual host FD/RSS/child-reaping, effective family cap under shared S1 activity, and full regressions are mandatory before acceptance.

---

## Preserved IP-01 historical decisions

# CUE IP-01 — architecture decisions and retained plan authority

The original amended planning decisions remain authoritative in the byte-for-byte `planning_authority/` directory. Additive IP-01 decisions:

1. **Reuse the existing opt-in Stage A JSONL sink** (`TONEPOET_PIPELINE_BASELINE_JSONL`) in `src/convert/pipeline/baseline.rs`, not a new analytics service or persistent event subsystem. Enabled-only CUE records use schema `tonepoet.cue.ip01.v1`, process-relative monotonic microseconds and a process-salted pseudonymous CUE job correlation ID. The sink is best effort; diagnostics failure never blocks conversion. This allows natural suppression when the env var is unset.
2. **Queue admission precedes worker start semantically:** capture its monotonic timestamp before submitting a work unit, emit only if submission succeeded. Emit `queue_deferred` with `queue_busy`/`queue_full` when admission fails; this measures first deferral→successful admission separately from admission→worker start. A cancellation without admission must never manufacture admission. Writes may appear after worker events on disk. Associate by CUE group ID + salted run ID; never infer TUI accept→queue time.
3. **Record S1-versus-fallback routes at admission decisions**, including stable categorical reasons and private-safe physical group identity. An existing causal source-pass observer failure triggers one album-level `source_pass_observer_recovery_retry` boundary; subsequent non-mirrored S1 routes are **attempts**, not additional physical groups. Execution failure after S1 begins is not an excuse to retry via generic fallback. Group scheduling and fallback work remain unchanged.
4. **Keep overlapping measurements separate:** group elapsed, segmented transport wall, individual decoder/encoder lifetimes, native source-pass observation, and post-output worker+measurement inclusive durations. Do not add them together or subtract one overlapping phase as 'saved' time. Existing `ConversionRunTiming`, scheduler counters, Stage A stage scopes and `replaygain_measure` retain their existing meaning.
5. **ReplayGain selection is not application.** `replaygain_policy` is requested Off/Track/Album/Both, `source_pass_candidate` describes eligibility, and `source_pass_replaygain_result` describes an observation that was retained. `replaygain_application_path` is the chosen branch, **never proof of a tag write**. New `replaygain_application_settled` reports the actual selected path and `tag_action=applied|skipped|failed|not_applied` after its Result resolves. A native scan-only duration is accepted only when both existing `replaygain_measure_started` and `replaygain_measure_finished` bear the **same exact** CUE token as one settled post-output worker. New opt-in thread-local correlation on the existing blocking worker attaches that token without modifying the native measurement or relying on event cardinality. Earlier uncorrelated traces yield `NOT INSTRUMENTED`.
6. **Existing observer reused, not introduced:** `observe_cue_mirror` reports inclusive time and success **with both local observer index and the original authored `track_ordinal`**. Neither these intervals nor transport child lifetimes are additive. No observer is attached to S3 or made part of a new worker/transport architecture. No source-pass observer was implemented under IP-01.
7. **Publisher and terminal retained:** the pipeline emits `report_terminal` at report/terminal-event submission and `album_published` reflects the already-existing published-album object. An emitted terminal event is not proof the TUI repainted; the action-to-visible timer belongs to an external host stopwatch. Published audio with post-action errors is distinguished from failure with no publication.
8. **Amended IP-06 performance gate is normative and unchanged:** pair S1/S3 `Off` and `Both`, attribute extra S3 post-output scan, use `deferred`/PX-C observer only upon qualifying evidence. Do not prematurely reject promising Off-winning S3 or speculate an observer without need. Preserve S1 default until fully qualified.

Only the same three originally modified Rust modules are changed. R2 adds isolated **real sink** subprocess tests, actual scheduler-deferral and existing CUE direct/fallback fixture assertions, source-RG eligibility and negative writer-path classification tests. Their Rust execution still requires the qualified build host. No new planner flags, public API, WAV generation, source metadata, codec implementation, worker scheduler, publication policy or parallel ReplayGain design is admitted.

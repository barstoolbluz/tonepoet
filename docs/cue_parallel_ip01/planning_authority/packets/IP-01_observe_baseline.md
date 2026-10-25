# IP-01 — Instrument the CUE acceptance-to-publication wall-clock and rejected-route reasons

**Stable ID:** IP-01. **Risk:** low/medium. **Input:** Initial **verified** `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` (SHA-256 printed above), plus this packet; no prior IP inputs.


> **Execute this packet in one GPT-6 implementation session.** Production editing is authorized **only in that session**, not in the preceding planning session. Treat this prompt as a standalone engineering contract. This packet consumes a **full cumulative source archive** plus the cumulative ledger/handoff; do not rely on prior chat messages. Preserve unrelated user changes. The source paths/line numbers below are source-verified for baseline 98aae2b and **must be independently rechecked** on the actual accepted input.

**Repository:** TonePoet Rust; original source git archive at commit `98aae2b`, original SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. That source archive has no `.git`; this commit identification is from the accompanying identity letter. **Critical fact:** an exact PCM/lossless CUE can already follow an FFmpeg shared decoder + one-at-a-time encoder transport, so do **not** rewrite CUE parsing, planner, metadata, ReplayGain, execution logging or publisher. In the Convert UI the initial ReplayGain pill is `Both` (`src/tui/app.rs:5546-5554`), while the generic API default is `None`. Every optimization must preserve the actual user-selected policy.

**Universal constraints:** exact CUE `INDEX 01` half-open sample ranges and physical FILE identity; `INDEX 00`/HTOA remain as currently implemented; sample representation/channel ordering unchanged; source metadata/artwork authority and per-track execution records preserved; existing typed planner and native ReplayGain retain authority; original `FailurePolicy`, cancellation/child reaping and atomic album publication unchanged. No silent lossy synthetic-CUE reparse, no new encoder implementation, no unbounded queues/PCM arrays, no duplicate result publishing, and no optimistic fallback after a tool actually begins and fails. Keep old S1/fallback as an independent reference and feature-off escape.
## Objective and smallest change

First **reuse** existing `ConversionRunTiming` (`src/convert/pipeline/stages.rs:16541-16565`), scheduler metrics and conversion records to generate a comparable no-production-change baseline recipe. Add only the specific opt-in CUE route/admission or queue/visible timing hooks needed to explain an actually missing observable phase; no parallel telemetry schema or new always-on log. Existing logging privacy and audio behavior must be unchanged. Distinguish queue wait, source decode, encode, ReplayGain, validation and publication where cheap source boundaries exist; mark other intervals NOT INSTRUMENTED, never invent a timestamp. A host-controlled action-to-visible stopwatch is acceptable as the overall timer.

## Source touchpoints and change contract

- Source review: `src/convert/processor.rs:4660-5100` and `:5350-5555` queue dispatch/group building; `src/convert/pipeline/scheduler.rs:60-150` existing atomic scheduler counters; `src/convert/pipeline/stages.rs:44190-44943`, `:45202-45585`, `:27544-28100` direct/fallback, child/album record and publication.
- Input: current request and track/group identities, existing conversion-log run timing (`stages.rs:16541-16565`) and scheduler counters. Output: existing record/timing plus, **only if essential**, a small opt-in per-CUE route/reason/interval record in the existing diagnostics channel; no new telemetry persistence architecture.
- Explicitly report `request_accepted` time when known; if the precise TUI event cannot be hooked without a third subsystem, separately record `queue_admitted` and label `acceptance -> queue` **NOT INSTRUMENTED** rather than mislabeling it end to end. At least queue dispatch to final visibility must be covered by hooks or documented existing reports.
- Capture child spawn→exit lifetimes, active workers, option-dependent source RG vs output RG, temp-file materialization, output FFprobe, publication same/cross-device where measurable. Measure stage `Instant` durations without aggregating overlapping child times. Require a stable `route_reason` for group plan fallback, S1 admission, insufficient resources and future S3 rejection.
- Do not log private filenames/album tags beyond existing policy. No added PCM buffering, I/O reads or default enabled telemetry. No source DSP/codec path changes; no new conversion configuration defaults.

## Specific adversarial tests and acceptance

- Focused no-observation test proves disabled path leaves artifacts and logs unchanged and doesn't leak file paths. Enabled fake clock or controlled phase injection emits monotonic, uniquely attributed records for two albums and out-of-order track completion; JSONL is valid and all `track_id` mappings correct.
- Gated S1 and file-backed fallback report different reasons. Unix source-pass ReplayGain eligible/ineligible branches and `ReplayGain Both/Off` are disambiguated; no RG value inferred from a timestamp.
- Simulate cancellation/error before/after group admission; terminal event distinguishes failure from visible publication. Verify no double-counting of aggregate decoder/encoder time.
- Provide a **reproducible timing worksheet/run recipe** for common single FLAC+CUE → FLAC, unchanged rate/depth, RG Both, plus Off control, same output/staging filesystems and comparable cache state. If an actual real-source run is unavailable, the worksheet and counters are delivered with `BENCHMARK NOT RUN`.
- Accept only if focused tests and full build on the qualified host show zero audio/policy changes, bounded/race-safe event records, and correct semantics for a "completed but unpublished" stage. Keep opt-in disabled by default.

## Required verification and handoff discipline

Before editing, `sha256sum <INPUT_FULL_SOURCE.tar.gz>` **must** equal the exact accepted hash recorded by the supplied handoff; unpack to a fresh location, inspect current paths/line ranges and existing tests, and confirm any source drift. In a tree with `.git`, capture `git status --short` and commit; for a git archive without `.git`, hash all input files and record the provenance limitation. Preserve all unrelated changes; only modify the necessary production modules, tests and the cumulative ledger. Use `rg -n 'cue_stream|CueStreamSegment|ReplayGain' src/convert src/tui tonepoet-pipeline/src` (as relevant) to refresh touchpoints before changing code.

Where the build host supports Rust and the native dependencies, execute and preserve output for at least:

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

If a test filter finds zero tests, do **not** treat it as a passing gate: identify and run specific added test names and record exact commands. Add and run the packet-specific named tests below. Do not rely exclusively on a mock runner for seek/execution behavior. Keep errors and stderr, no fabricated durations. If `cargo` or hardware integration fixtures are unavailable, mark **BUILD/EXECUTION NOT RUN** and state `implemented-unqualified`; do **not** mark accepted or proceed on an unqualified assumption. Scope limited to this packet; no unrelated refactor.

**Every result must include:** an entire *new* cumulative full-source archive `TonePoet_CUE_IP01_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz` (all accepted earlier changes, no build caches), real SHA-256 and file manifest, tracked changed-file summary/diff where available, executed commands with PASS/FAIL/NOT RUN by source ID/environment, source/audio fixture checksums, static audit, current packet state, `IMPLEMENTATION_STATUS.md`, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `NEXT_SESSION_HANDOFF.md` and updated `SHA256SUMS`. Record actual source/archive identity and *exact* next packet input in the handoff. Source changes are not accepted merely because this archive was generated; the host/owner verifies qualification.

**Rollback:** keep the exact accepted input archive and an unchanged way to select S1; do not switch ordinary user routes until the packet's focused gate is proven.

**STOP/SPLIT RULE:** If a source assumption fails, tool API is unavailable, sample identity cannot be proven, a required fixture/toolchain is missing, more than a few large subsystems require invasive edits, or a preexisting semantic bug forces a separate redesign, stop the activation. Preserve the verified increment, label its state `implemented-unqualified`/`blocked`, describe the smallest new dependent corrective packet and its acceptance gate, and hand over durable artifacts. Never call an unfinished feature production-ready.

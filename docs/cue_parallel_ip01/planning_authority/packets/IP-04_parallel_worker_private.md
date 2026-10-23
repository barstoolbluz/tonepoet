# IP-04 — Implement a private bounded per-track FLAC-range decode plus authorized-encoder executor

**Stable ID:** IP-04. **Risk:** high. **Input:** Exact **accepted** IP-03 full cumulative source archive with verified weighted global admission, plus accepted exact range adapter from IP-02.


> **Execute this packet in one GPT-6 implementation session.** Production editing is authorized **only in that session**, not in the preceding planning session. Treat this prompt as a standalone engineering contract. This packet consumes a **full cumulative source archive** plus the cumulative ledger/handoff; do not rely on prior chat messages. Preserve unrelated user changes. The source paths/line numbers below are source-verified for baseline 98aae2b and **must be independently rechecked** on the actual accepted input.

**Repository:** TonePoet Rust; original source git archive at commit `98aae2b`, original SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. That source archive has no `.git`; this commit identification is from the accompanying identity letter. **Critical fact:** an exact PCM/lossless CUE can already follow an FFmpeg shared decoder + one-at-a-time encoder transport, so do **not** rewrite CUE parsing, planner, metadata, ReplayGain, execution logging or publisher. In the Convert UI the initial ReplayGain pill is `Both` (`src/tui/app.rs:5546-5554`), while the generic API default is `None`. Every optimization must preserve the actual user-selected policy.

**Universal constraints:** exact CUE `INDEX 01` half-open sample ranges and physical FILE identity; `INDEX 00`/HTOA remain as currently implemented; sample representation/channel ordering unchanged; source metadata/artwork authority and per-track execution records preserved; existing typed planner and native ReplayGain retain authority; original `FailurePolicy`, cancellation/child reaping and atomic album publication unchanged. No silent lossy synthetic-CUE reparse, no new encoder implementation, no unbounded queues/PCM arrays, no duplicate result publishing, and no optimistic fallback after a tool actually begins and fails. Keep old S1/fallback as an independent reference and feature-off escape.
## Objective and staged output interface

Implement a **test-invocable private S3 worker** producing independently staged FLAC track artifacts using IP-02's sample-qualified FLAC range reader and TonePoet's **existing** `prepare_cue_stream_direct_track_plan`, `cue_stream_consumer_command`, and `ToolRunner` infrastructure. Still **unreachable** through user-facing CUE dispatch. This is a single vertical execution primitive, not activation, album publication, a new encoder, or a format-general scheduling crate.

## Source touchpoints and control flow

- `src/convert/pipeline/stages.rs:43897-44195`, `:44190-44296`, `:45014-45200`, `:45202-45585` contains group execution, FFmpeg decoder/consumer builder, finalization, immutable member records. `src/convert/pipeline/track_executor.rs:1355-1539` planner admission and `:11607-11676` reference-pipeline resource handling. `src/convert/pipeline/tool.rs:527-601`, `:2170-2700` existing process-pipe supervision. `src/convert/pipeline/types.rs:1249-1277` carries physical source and sample facts.
- Input: qualified `CueStreamSegment`, index/`TrackId`, per-track planner-approved direct plan, exact `[start, end)` counts, the small shared RAII child-permit guard (IP-03), cancellation, and immutable staged/final target paths. Output: exactly one staged artifact plus one `ScheduledTrackOutput` with truthful source-range reader and encoder command records, expected sample/depth validation, metadata satisfaction and source/path attribution, or a structured failure with cleanup.
- The coordinator's private `run_qualified_flac_ranges_for_group` launches at most admitted N workers, validates **every group member's** independent range capability and planner shape before launch, shares no mutable per-track command or metadata, bounds pipes, and returns a Vec ordered by original member index even if completions are out of order. The decoded range may not be cached or staged in a whole-track PCM WAV just to coordinate workers.
- Use the existing **Unix-only** decoder+encoder supervisor (`ToolRunner::run_pipeline`, `src/convert/pipeline/tool.rs:2170-2372`); feature-disabled private tests can exercise it on qualified Unix hosts. Do not activate S3 if this runner/host cannot execute a supervised pipeline. Do not hand-roll subprocess management, orphan children or leave one side running after the other fails. If the runner does not permit a precise range producer without changing its abstraction, add a **small typed extension** with focused tests; defer any broad rewrite. Never claim samples correct solely because FFmpeg exited zero: independently compare decoded PCM reference and actual output content/sample count.
- Output stays in private existing staging (`staged_audio_path`); no tag mutation, native ReplayGain, final-visible publication or new retry logic inside a worker. The album integrator in IP-05 owns those policies. Preserve command timing, tool versions, source offsets and failed-attempt evidence.

## Acceptance tests and rejection conditions

- Fake runner: three tracks complete in order `3,1,2`, aggregation returns `1,2,3` with correct source ids, unique temp paths, independent stdout/stderr, no cross-record attribution; equal PCM ranges including middle frame, final partial track and omitted/unselected intervals.
- Real subprocess tests: 16-bit and 24-bit qualified FLAC, 12- and 30-track fixture, two concurrent encoder processes where resource limit permits, matched decoded PCM/content and sample counts to S1; record actual N processes, peak buffered PCM and max FD. Compare one worker S3 to S1 to ensure worker correctness independent of speed.
- Inject decoder exits nonzero, encoder exits early, broken pipe, slow consumer, canceled parent while two children live, disk full and stdout stderr load; no deadlock/orphans or partial final-visible outputs. Cleanup all issued stages; no automatic replay through S1 after a failing command.
- More than budgeted tracks and two simultaneous private album runners share IP-03 counters; bounded memory and fair acquisition. A 200-member test must not spawn 200 processes.
- Accept only if the executor emits truthful per-track staged results and records and all critical failure injection passes. No user-facing route and no production publication switch yet; if tests/build absent, `implemented-unqualified`.

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

**Every result must include:** an entire *new* cumulative full-source archive `TonePoet_CUE_IP04_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz` (all accepted earlier changes, no build caches), real SHA-256 and file manifest, tracked changed-file summary/diff where available, executed commands with PASS/FAIL/NOT RUN by source ID/environment, source/audio fixture checksums, static audit, current packet state, `IMPLEMENTATION_STATUS.md`, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `NEXT_SESSION_HANDOFF.md` and updated `SHA256SUMS`. Record actual source/archive identity and *exact* next packet input in the handoff. Source changes are not accepted merely because this archive was generated; the host/owner verifies qualification.

**Rollback:** keep the exact accepted input archive and an unchanged way to select S1; do not switch ordinary user routes until the packet's focused gate is proven.

**STOP/SPLIT RULE:** If a source assumption fails, tool API is unavailable, sample identity cannot be proven, a required fixture/toolchain is missing, more than a few large subsystems require invasive edits, or a preexisting semantic bug forces a separate redesign, stop the activation. Preserve the verified increment, label its state `implemented-unqualified`/`blocked`, describe the smallest new dependent corrective packet and its acceptance gate, and hand over durable artifacts. Never call an unfinished feature production-ready.

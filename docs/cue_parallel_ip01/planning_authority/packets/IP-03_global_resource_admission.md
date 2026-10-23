# IP-03 — Reuse shared tool permits for bounded parallel CUE pipelines

**Stable ID:** IP-03. **Risk:** medium; narrowly contained to new S3 path. **Input:** Exact **accepted** IP-02 full cumulative source archive, with proven FLAC seek cell and updated ledger; otherwise **BLOCKED**.


> **Execute this packet in one GPT-6 implementation session.** Production editing is authorized **only in that session**, not in the preceding planning session. Treat this prompt as a standalone engineering contract. This packet consumes a **full cumulative source archive** plus the cumulative ledger/handoff; do not rely on prior chat messages. Preserve unrelated user changes. The source paths/line numbers below are source-verified for baseline 98aae2b and **must be independently rechecked** on the actual accepted input.

**Repository:** TonePoet Rust; original source git archive at commit `98aae2b`, original SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. That source archive has no `.git`; this commit identification is from the accompanying identity letter. **Critical fact:** an exact PCM/lossless CUE can already follow an FFmpeg shared decoder + one-at-a-time encoder transport, so do **not** rewrite CUE parsing, planner, metadata, ReplayGain, execution logging or publisher. In the Convert UI the initial ReplayGain pill is `Both` (`src/tui/app.rs:5546-5554`), while the generic API default is `None`. Every optimization must preserve the actual user-selected policy.

**Universal constraints:** exact CUE `INDEX 01` half-open sample ranges and physical FILE identity; `INDEX 00`/HTOA remain as currently implemented; sample representation/channel ordering unchanged; source metadata/artwork authority and per-track execution records preserved; existing typed planner and native ReplayGain retain authority; original `FailurePolicy`, cancellation/child reaping and atomic album publication unchanged. No silent lossy synthetic-CUE reparse, no new encoder implementation, no unbounded queues/PCM arrays, no duplicate result publishing, and no optimistic fallback after a tool actually begins and fails. Keep old S1/fallback as an independent reference and feature-off escape.
## Objective and smallest change

**Compatibility:** the historical packet filename and stable `IP-03` ID are retained only to avoid breaking already-distributed dependency links; the old broad resource-manager objective is explicitly superseded.

Add **one narrow, feature-disabled S3 two-process permit adapter**, reusing the already shared `ToolConcurrencyLimits`. Count concurrently live decoder + encoder children accurately, without changing public S1, generic command scheduling or unrelated conversion paths. The processor already shares per-family semaphores globally (`src/convert/processor.rs:2167-2185`; `src/convert/pipeline/track_executor.rs:709-804`). A second weighted global RAM/FD/I/O scheduler is **not warranted** for the proposed fixed two-child streaming worker.

## Source, permit ownership and resource contract

- Existing `src/convert/pipeline/track_executor.rs:709-804` has global `sox`, `ffmpeg`, `ssrc` Tokio semaphores; `:11607-11661` acquires once per **distinct** family for the segmented S1 path; `src/convert/pipeline/tool.rs:2170-2372` already supervises producer+consumer as one pipeline attempt. Preserve those paths initially. Build the smallest additional helper for new S3 only, with focused tests.
- For one live S3 worker, acquire **two FFmpeg permits** for an FFmpeg decoder + FFmpeg encoder, or **one FFmpeg + one SoX** as appropriate. Use an all-or-nothing nonblocking reservation or deterministic family ordering with immediate release on failure; never wait for the second permit while holding the first. Acquire before spawn. Keep the shared Arc and RAII ownership through both child exits; do not reacquire in `run_tool_command_with_concurrency` or `run_reference_capture_pipeline`. Call `ToolRunner::run_pipeline` directly under the owning guard.
- Bound live S3 workers globally across albums with a **small** in-flight cap (count/rate based on existing tool process capacity and measured headroom), not a new per-album pool. Each worker uses the existing supervised bounded pipe; explicit `N × fixed_per_worker_buffers`, `N × FD_per_worker` and `N` input readers provide conservative RAM/FD/I/O high-water bounds. If the host cannot admit the pair, use the existing path; do not treat a declined optimization as a conversion failure.
- A live S1 or unrelated job also competes for existing family permits. Source shows legacy segmented helper counts one permit per distinct family even if the decoder and consumer are both FFmpeg; **test S3 coexistence with that behavior**. If it violates a measured process/resource bound, narrowly correct the specific concurrently-live segmented boundary or conservatively deny S3 under contention; do not mandate retrofitting the entire application with weighted token ledgers. Record any unavoidable S1 throughput change before acceptance.
- No separate process family hierarchy, no general FD/token ownership ledger, no I/O bandwidth prediction, no global scheduler rewrite, no new file-reservation or crash-recovery layer. Correctness requires bounded actual children, pipe buffers, source readers and cleanup, not fictionally precise dynamic system-wide RAM numbers.

## Focused adversarial verification

- Unit/fake-runner: FFmpeg/FFmpeg pair costs two shared permits; FFmpeg/SoX costs one of each; insufficient capacity refuses **before** spawn; a pair never deadlocks waiting for itself; one RAII owner returns permits on success/failure/cancel/panic.
- Two independent albums plus existing non-S3 conversions share the same `ToolConcurrencyLimits`: actual S3 in-flight and native child counts stay within policy, ordinary work still proceeds. Exercise one- and two-FFmpeg-permit hosts; an inadmissible pair must choose the old route. Do not pretend to enforce a strict live-child cap on legacy commands without inspecting the existing per-family helper's semantics.
- Slow stdin/stdout, encoder early exit, cancellation during admission/transfer and 200-track album: bounded live children, fixed pipe memory, FD high-water and no token leaks. Use normal-host FD/RSS checks to select a conservative N; do **not** install a generic FD/RAM reservation service.
- Confirm existing S1 stream, native source-pass ReplayGain and unrelated source/track conversion tests remain unchanged with the feature disabled. If integration requires a broad core scheduler/ToolRunner refactor, stop/split and revisit architecture instead of undertaking that refactor under this packet.
- Acceptance: two-child accounting proven on the real supervised pipeline, cancellation/cleanup and cross-album admission tests pass. Where native toolchain is absent, mark NOT RUN and keep the new route disabled.

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

**Every result must include:** an entire *new* cumulative full-source archive `TonePoet_CUE_IP03_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz` (all accepted earlier changes, no build caches), real SHA-256 and file manifest, tracked changed-file summary/diff where available, executed commands with PASS/FAIL/NOT RUN by source ID/environment, source/audio fixture checksums, static audit, current packet state, `IMPLEMENTATION_STATUS.md`, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `NEXT_SESSION_HANDOFF.md` and updated `SHA256SUMS`. Record actual source/archive identity and *exact* next packet input in the handoff. Source changes are not accepted merely because this archive was generated; the host/owner verifies qualification.

**Rollback:** keep the exact accepted input archive and an unchanged way to select S1; do not switch ordinary user routes until the packet's focused gate is proven.

**STOP/SPLIT RULE:** If a source assumption fails, tool API is unavailable, sample identity cannot be proven, a required fixture/toolchain is missing, more than a few large subsystems require invasive edits, or a preexisting semantic bug forces a separate redesign, stop the activation. Preserve the verified increment, label its state `implemented-unqualified`/`blocked`, describe the smallest new dependent corrective packet and its acceptance gate, and hand over durable artifacts. Never call an unfinished feature production-ready.

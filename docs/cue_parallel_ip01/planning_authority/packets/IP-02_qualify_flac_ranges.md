# IP-02 — Prove FLAC independent decoder ranges are sample-exact before any parallel activation

**Stable ID:** IP-02. **Risk:** high feasibility. **Input:** Exact **accepted** IP-01 full cumulative source archive + SHA-256 and updated ledgers; do not reuse the original archive if IP-01 landed.


> **Execute this packet in one GPT-6 implementation session.** Production editing is authorized **only in that session**, not in the preceding planning session. Treat this prompt as a standalone engineering contract. This packet consumes a **full cumulative source archive** plus the cumulative ledger/handoff; do not rely on prior chat messages. Preserve unrelated user changes. The source paths/line numbers below are source-verified for baseline 98aae2b and **must be independently rechecked** on the actual accepted input.

**Repository:** TonePoet Rust; original source git archive at commit `98aae2b`, original SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. That source archive has no `.git`; this commit identification is from the accompanying identity letter. **Critical fact:** an exact PCM/lossless CUE can already follow an FFmpeg shared decoder + one-at-a-time encoder transport, so do **not** rewrite CUE parsing, planner, metadata, ReplayGain, execution logging or publisher. In the Convert UI the initial ReplayGain pill is `Both` (`src/tui/app.rs:5546-5554`), while the generic API default is `None`. Every optimization must preserve the actual user-selected policy.

**Universal constraints:** exact CUE `INDEX 01` half-open sample ranges and physical FILE identity; `INDEX 00`/HTOA remain as currently implemented; sample representation/channel ordering unchanged; source metadata/artwork authority and per-track execution records preserved; existing typed planner and native ReplayGain retain authority; original `FailurePolicy`, cancellation/child reaping and atomic album publication unchanged. No silent lossy synthetic-CUE reparse, no new encoder implementation, no unbounded queues/PCM arrays, no duplicate result publishing, and no optimistic fallback after a tool actually begins and fails. Keep old S1/fallback as an independent reference and feature-off escape.
## Objective and change boundary

Implement a **nonactivated, typed, test-invocable FLAC ranged-source command builder/adapter** that can produce the *same* raw PCM carrier as existing `cue_stream_decoder_command`, but only for an exact integer source interval. Qualify its seek/preroll and EOF contract under actual FFmpeg and/or maintained `flac` tool versions. **No production admission, worker pool or public route changes here.** If exactness cannot be proved, reject S3 and update ADR instead of approximating indexes or reading whole image for every track.

## Source touchpoints, producer contract and invariants

- Reuse `TrackSourceRef::CueStreamSegment` fields (`src/convert/pipeline/types.rs:1249-1277`); exact FLAC STREAMINFO and CUE sample bounds (`src/convert/pipeline/materializer_cue.rs:74-107`, `:491-690`, `:2335-2492`); raw PCM representation in `src/convert/pipeline/stages.rs:44190-44224`; existing `ToolCommand`/`ToolRunner` in `src/convert/pipeline/tool.rs:256-320`.
- Input: canonical physical FLAC path, `sample_rate`, `channels`, integer source depth, `carrier` (existing s32le normalization), inclusive start and *exclusive* end sample, verified total-sample authority, cancel token. Output: typed command/qualified range contract and **exactly `length × channels × carrier_bytes` raw PCM bytes** or error. Never use float-seconds as sole boundary authority.
- Analyze current system `ffmpeg -ss` before input, `-ss` after input, a decoder/filter `atrim` and native `flac --skip/--until` as candidate implementations; choose **one** that can be proved exact without an O(start_sample) decode for every worker and that emits the exact expected s32le carrier for integer 16/24-bit data. A plain post-input `atrim` from sample zero is not an accepted 'seek optimization'. A native decoder yielding s24 instead of normalized s32 cannot silently change planner input.
- Pin/record an executable decoder/tool behavior and version cell; handle FLAC seektable and no seektable separately. **Seek qualification is distinct from full worker transport qualification**: current `RealToolRunner::run_pipeline` is Unix-only (`src/convert/pipeline/tool.rs:2170-2372`), so S3 admission must be explicitly off on non-Unix even if standalone FFmpeg produces exact bytes. Canonical input path remains distinct from logical track identity; no direct publication or metadata mutations.
- Validate checked `u128` byte-count arithmetic; zero, reversed, past-EOF, overflow and unsupported depth/channel cases fail closed without process spawn. EOF before expected count or surplus decoded samples is an error. Correct stdout/stderr supervision and cancellation; no unbounded memory consumption.

## Independent test and gate

- Unit vectors: integer boundary conversion (75fps), `[0,1)`, arbitrary non-frame-aligned offsets, last sample, multiple ranges that tile and recombine without a gap, very large offset and overflow refusal.
- Real `flac`/FFmpeg-generated 16-bit and 24-bit stereo/mono files at 44.1/48/96 kHz, noisy per-sample signatures, FLAC with and without SEEKTABLE, small files, long offsets, corrupt/truncated input. Compare **canonical widened decoded PCM** from the existing serial command to the range output **sample for sample and byte for byte**, not encoded-file bytes.
- Adversarial offsets at `0`, `1`, `587`, `588`, `589` for 44.1k, inside codec blocks, different middle/near-EOF starts; test many random deterministic offsets, sample limits, decoder start/stop under cancellation, no leaks on stderr/full pipe and under slow consumer. Check startup/read scaling so 'independent range' really avoids full-source replay.
- If the test host or exact native dependency is absent, leave adapter unused and mark `SEEK-EXACTNESS NOT QUALIFIED` even if static tests pass. Explicitly record tool version and platform; no promise for other hosts. A small one-file experiment is not sufficient.
- **Go:** all exact samples and verified length plus valid child lifecycle for the specified FLAC type/version cells. **No-go:** any first/last sample mismatch, source-dependent startup requiring whole-image reading, unsupported raw class, unbounded output, or missing real-run evidence. On no-go retain S1 and reopen ADR; do not unblock IP-03.

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

**Every result must include:** an entire *new* cumulative full-source archive `TonePoet_CUE_IP02_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz` (all accepted earlier changes, no build caches), real SHA-256 and file manifest, tracked changed-file summary/diff where available, executed commands with PASS/FAIL/NOT RUN by source ID/environment, source/audio fixture checksums, static audit, current packet state, `IMPLEMENTATION_STATUS.md`, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `NEXT_SESSION_HANDOFF.md` and updated `SHA256SUMS`. Record actual source/archive identity and *exact* next packet input in the handoff. Source changes are not accepted merely because this archive was generated; the host/owner verifies qualification.

**Rollback:** keep the exact accepted input archive and an unchanged way to select S1; do not switch ordinary user routes until the packet's focused gate is proven.

**STOP/SPLIT RULE:** If a source assumption fails, tool API is unavailable, sample identity cannot be proven, a required fixture/toolchain is missing, more than a few large subsystems require invasive edits, or a preexisting semantic bug forces a separate redesign, stop the activation. Preserve the verified increment, label its state `implemented-unqualified`/`blocked`, describe the smallest new dependent corrective packet and its acceptance gate, and hand over durable artifacts. Never call an unfinished feature production-ready.

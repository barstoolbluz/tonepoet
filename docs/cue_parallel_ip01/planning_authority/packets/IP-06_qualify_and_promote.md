# IP-06 — Adversarially qualify the opt-in pilot and decide narrow default auto-admission using end-to-end wall time

**Stable ID:** IP-06. **Risk:** medium. **Input:** Exact **accepted** IP-05 full cumulative source archive with verified default-OFF pilot and current status/evidence.


> **Execute this packet in one GPT-6 implementation session.** Production editing is authorized **only in that session**, not in the preceding planning session. Treat this prompt as a standalone engineering contract. This packet consumes a **full cumulative source archive** plus the cumulative ledger/handoff; do not rely on prior chat messages. Preserve unrelated user changes. The source paths/line numbers below are source-verified for baseline 98aae2b and **must be independently rechecked** on the actual accepted input.

**Repository:** TonePoet Rust; original source git archive at commit `98aae2b`, original SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. That source archive has no `.git`; this commit identification is from the accompanying identity letter. **Critical fact:** an exact PCM/lossless CUE can already follow an FFmpeg shared decoder + one-at-a-time encoder transport, so do **not** rewrite CUE parsing, planner, metadata, ReplayGain, execution logging or publisher. In the Convert UI the initial ReplayGain pill is `Both` (`src/tui/app.rs:5546-5554`), while the generic API default is `None`. Every optimization must preserve the actual user-selected policy.

**Universal constraints:** exact CUE `INDEX 01` half-open sample ranges and physical FILE identity; `INDEX 00`/HTOA remain as currently implemented; sample representation/channel ordering unchanged; source metadata/artwork authority and per-track execution records preserved; existing typed planner and native ReplayGain retain authority; original `FailurePolicy`, cancellation/child reaping and atomic album publication unchanged. No silent lossy synthetic-CUE reparse, no new encoder implementation, no unbounded queues/PCM arrays, no duplicate result publishing, and no optimistic fallback after a tool actually begins and fails. Keep old S1/fallback as an independent reference and feature-off escape.
## Objective

Qualify or reject production automatic S3 selection from **whole-conversion**, repeated paired trials on the exact current source, prioritizing the real Convert UI default **ReplayGain Both** and complete publication. This packet makes at most a narrow admission-policy change after qualification. It does not add new source formats, codecs, DSP paths, threads, metadata logic or a new benchmark-only fast path. If the speedup is poor or the proof fails, **leave S1 default and record the rejected optimization**; this is a successful honest outcome.

## Source/evidence and decision implementation

- Use packet observations from `src/convert/processor.rs:5350-5555`, `src/convert/pipeline/scheduler.rs:60-150`, `src/convert/pipeline/stages.rs:44413-45585`, `:27544-28100`; source identity `98aae2b` is historical, this packet's compiled code/source hash must be the new cumulative identity. Full policy is in the companion planning bundle `05_CONFORMANCE_AND_PERFORMANCE.md`, but all required criteria are repeated here.
- Generate/reference identical decoded 60-minute 16-bit 44.1k FLAC data with CUE splits of 1/3/12/30 tracks; a 24-bit and 96k variant; include at least one available realistic/compressible music album for automatic-promotion decisions (noise-only codec behavior is not representative). For default both/off and artwork on/off cells, compare S1 against S3 on the **same** toolchain/tool versions, FLAC compression profile, actual user settings, output and staging filesystems and cache cohort. Store a per-run canonical **configuration SHA-256** (route excluded) so `tools/evaluate_trials.py` refuses mixed settings within pairs. Counterbalance order; collect ≥5 pairs per primary cell, median, variation, peak RSS/FD/temp, child counts and bandwidth, wall time from admission to visible publication. If possible run both SSD and HDD, multiple core counts; keep untested storage/platform fallback to S1.
- Re-run adversarial PCM equivalence with all boundaries; compare normalized decoded track PCM hashes/samples and real metadata, artwork, ReplayGain, execution/provenance and final publisher outputs. Inject cancellation/partial album, corrupt input, disk-full, child/scan/rename faults, concurrent albums, early encoder exit, slow pipe. Any mismatch blocks admission, independent of speed.
- **Performance target (decision threshold, not prediction):** repeatable median whole-run `S1_wall / S3_wall >= 1.25` on representative 12-track SSD/qualified hardware with RG Both; compare paired differences and confidence rather than claiming single-run success. Do not auto-select S3 for 1–3 short tracks or hosts with insufficient weighted process tokens. Disallow a policy with common-case slowdown >10% or unbounded high-water resources. If evidence is thin/uncertain, keep pilot explicitly opt-in and S1 default; do not fabricate an optimal cutoff.
- Only after tests prove benefit, implement **one simple reversible admission policy** using existing capability/tool-version/cpu/physical-source/workload facts. The selector must log reason and never reinterpret metadata/semantics. Preserve manual override to S1 and feature-off switch; if policy drifts, fail closed to S1. Do not hard-code SSD/HDD assertions based only on filename/extension; unobserved storage classes remain old route.
- Expand `CONFORMANCE_MATRIX.md` with exact source/destination/tool/version/platform/codec/depth cells actually proven, not theoretical output formats. `VERIFICATION_RESULTS.md` includes commands, fixtures/hashes, raw timing JSONL, plots/tables if useful, and per-gate verdict. Update `ARCHITECTURE_DECISIONS.md` with acceptance or explicit reversal.

## ReplayGain attribution (required before any rejection)

S1's direct CUE route can compute ReplayGain during the decode it already performs, when
the source-pass eligibility predicate holds (`cue_source_replaygain_eligible` in
`src/convert/pipeline/stages.rs`). An S3 worker reading its own range cannot reuse that
shared sequential mirror, so with the UI default **RG Both** the initial pilot pays an
additional post-output scan that S1 does not. A wall-clock loss caused by that extra scan
is **not** evidence that independent ranged decoding and parallel encoding are inherently
slower, and must not be recorded as one.

Therefore, before any reject-or-promote verdict:

- Record, per run, whether S1 **actually** took the source-pass path or fell back to the
  post-output scan. Do not infer it from settings; observe it. A pair in which S1 used
  source-pass analysis and S3 used a post-output scan is a legitimate end-to-end
  comparison, but it is **not** a comparison of the two transports.
- Run every primary cell under matched **RG Off** as well as **RG Both**, with the same
  configuration SHA-256 as its paired run.
- Attribute the additional scan cost explicitly: report S3's measured post-output
  ReplayGain scan time as its own interval, and state the RG-Off speedup and the RG-Both
  speedup separately. Do not report a single blended figure.

## Acceptance or rejection

- **Approve narrow default only when** compiled-host runtime, exact decoded audio, metadata/RG, logging/failures, budgets, publication, artifact hashes and paired total-wall benefit are all qualified. A speedup with RG Off but slowdown with UI default Both **does not** qualify default on.
- **Preserve the pilot and defer the decision** when the attribution above shows a
  meaningful S3 advantage with RG **Off** but a loss with RG **Both** that is attributable
  primarily to S3's additional output scan. This outcome is neither acceptance nor
  rejection: keep S3 opt-in with S1 default, record the measured attribution, and promote
  the source-pass ReplayGain observer outline in `provisional/PX-C_stateful_and_fallback.md`
  into one focused packet reusing TonePoet's existing native measurement and album-reduction
  machinery. Repeat this packet's qualification on the result before any final architecture
  verdict. Do **not** implement that observer speculatively inside this packet, and do
  **not** promote S3 on an RG-Off result alone.
- **Keep S1 default** when CI lacks native FFmpeg/SoX, a needed fixture is absent, performance is inconclusive, or any safety gate fails. Mark executed tests PASS/FAIL/NOT RUN truthfully; submit a measured conditional follow-on (e.g. source-pass RG observer, output format expansion) only if the real bottleneck justifies it.
- Produce the full cumulative source archive even if no route promotion occurs (ledger and benchmark evidence may be its only delta); do not pretend the no-change path is a product acceleration.

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

**Every result must include:** an entire *new* cumulative full-source archive `TonePoet_CUE_IP06_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz` (all accepted earlier changes, no build caches), real SHA-256 and file manifest, tracked changed-file summary/diff where available, executed commands with PASS/FAIL/NOT RUN by source ID/environment, source/audio fixture checksums, static audit, current packet state, `IMPLEMENTATION_STATUS.md`, `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `NEXT_SESSION_HANDOFF.md` and updated `SHA256SUMS`. Record actual source/archive identity and *exact* next packet input in the handoff. Source changes are not accepted merely because this archive was generated; the host/owner verifies qualification.

**Rollback:** keep the exact accepted input archive and an unchanged way to select S1; do not switch ordinary user routes until the packet's focused gate is proven.

**STOP/SPLIT RULE:** If a source assumption fails, tool API is unavailable, sample identity cannot be proven, a required fixture/toolchain is missing, more than a few large subsystems require invasive edits, or a preexisting semantic bug forces a separate redesign, stop the activation. Preserve the verified increment, label its state `implemented-unqualified`/`blocked`, describe the smallest new dependent corrective packet and its acceptance gate, and hand over durable artifacts. Never call an unfinished feature production-ready.

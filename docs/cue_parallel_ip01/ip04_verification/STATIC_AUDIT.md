# Corrective source audit — October 25, 2026

D-01 expected-depth `u16` cast present. D-02 uses `VerifiedPipeFault` with role-based peer retention and broken-pipe-only causal consumer grace. D-03 adds actual private-coordinator test with deterministic 3,1,2 completions, stage/command attribution and negative controls. No user-facing dispatch call site, S1 pipeline mutation or new resource scheduler. Updated static verifier checks these structural signatures and cumulative source identity; it is **not** Rust compilation. See `CORRECTIVE_D01_D03_2026-10-25.md`.
---

# Independent static scope and code-path audit — IP-04

**Decision: implemented-unqualified; no activation.** Input archive authenticated to `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7` before edits. Git-less input has no independently checkable commit history. All unrelated tracked source files are unchanged (verify with manifest/baseline diff).

## Obligations and flow

1. `stages.rs` obtains independent FLAC range from IP-02 `CueFlacRangeDecoder::prepare` (which compares typed source sample rate/channels/depth/total against FLAC STREAMINFO). Group-wide preflight uses original CUE half-open sample bounds and physical source identity, rejects duplicates/stale destination names and planner failures before starting children. Explicit target FLAC gate; no generalized route policy.
2. Per-member worker calls the **one existing** `run_cue_s3_pipeline_if_admitted` in `track_executor.rs` with a typed verifier; S3 RAII permits reserve two FFmpeg or one FFmpeg and one SoX child and a shared in-flight slot. Never call the generic single-child concurrency wrapper or reacquire another token. A decline is pre-spawn and is returned as a failed private staging attempt, never a post-spawn retry.
3. `ToolRunner`'s new *optional* exact-pipe method is unsupported by default; only `RealToolRunner` implements it. Child execution still goes through `run_supervised_with_stdio`. Reader and writer pipes are cloexec. The 64-KiB streaming buffer is constant, and the IP-02 `ExactPcmExtent` receives every decoded byte before any write to the consumer. Read at EOF invokes `finish`. A mismatch, child/tool error or cancellation cancels both supervisors and awaits their completion.
4. Existing `finalize_cue_stream_consumer_output` remains the sole conversion stage finalizer; new typed optional range context carries start/end and expected byte count and records a passed *actual boundary* extent verification. Both preexisting S1 calls pass `None` and retain their unqualified shared-decoder semantics. Stage bytes, expected samples/depth and metadata are validated as before.
5. Member failure carries tool records and deletes any private staged artifact; no final-visible file created by this function. The group coordinator uses two futures max, no per-track spawned Tokio tasks, no unbounded FIFO of PCM; result records sorted to source index. Normal scheduler/publisher has **zero call sites** to `run_qualified_flac_ranges_for_group`.

## Realistic unresolved qualification

- **Rust compilation and integration NOT RUN**: no cargo/rustc/rustfmt. Typechecking, full compatibility and test failures must be resolved only against actual compiler evidence, not by broad speculative refactor.
- Child error/EOF/cancellation behavior is designed and covered by authored tests but **not executed in this Rust runtime**. External native fixtures cannot prove Rust executor supervision, child termination, staging cleanup or measured FD/RSS caps in TonePoet.
- The optional test harness currently qualifies 3-track 16/24-bit cases by default; 12/30-track cases are `#[ignore]` and must be explicitly selected on host. More aggressive disk-full and simultaneous mixed-album stress tests remain part of owner-host signoff; do not mistake independent native Python tests for those tests.
- No throughput improvement is claimed by packet IP-04; policy/activation and S1/S3 performance evaluation are IP-05/IP-06 scope.

## Narrow rollback

Retain `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz` (`fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7`) unchanged. Since no ordinary CUE route calls S3, leave current production routing as-is. If IP-04 host gates fail, make a focused correction to these four production files, re-run gate, and repackage. No unrelated scheduler redesign or global backpressure infrastructure is warranted by current evidence.

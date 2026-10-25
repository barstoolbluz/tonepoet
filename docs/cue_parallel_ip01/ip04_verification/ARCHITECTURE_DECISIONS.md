# IP-04 corrective decision AD-04.7 — October 25, 2026

**Failure evidence over transport symptom:** In the private verified pipe, treat consumer nonzero/spawn/timeout/termination as causal over a secondary `BrokenPipe`, provided its supervisor independently reports the cause; retain other child records by producer/consumer role instead of filtering an ambiguous combined list. Permit bounded 750-ms diagnostic settlement only on broken-pipe failure, preserving byte-count verifier authority and normal fast paths. The private 3,1,2 completion test uses 1,3,2 submission to satisfy the mandatory two-worker limit; the production scheduler/dispatch is untouched.
---

# IP-04 architecture decisions

**AD-04.1 — Private vertical path, no dispatch.** Extend the existing S1 direct-plan builder and finalizer rather than building a separate encoder/planner/metadata pipeline. IP-05 owns activation and policy. No public S3 entry point or new configuration.

**AD-04.2 — Source-qualified byte extent at the actual pipe.** Checking only decoder exit status or final output frames cannot prove the decoder supplied an exact half-open range. Carry IP-02's typed `ExactPcmExtent` into a small new *optional* `ToolRunner` operation, leaving original unchecked `run_pipeline` byte-identical. Fail closed for runners that cannot verify the transfer. A 64-KiB reader/writer buffer plus two OS pipes is bounded in memory and FD count; no intermediate PCM file.

**AD-04.3 — No second acquisition.** IP-03's `run_cue_s3_pipeline_if_admitted` must retain custody of both native permits until BOTH supervisors settle; pass the byte-verifier option through that wrapper. Never invoke a second resource scheduler from inside the worker.

**AD-04.4 — All-member preflight.** Verify source/STREAMINFO/range/planner and distinct output paths for every member before launching a private child. A real decode/encode failure is a per-track failed attempt; there is no automatic replay on S1. Return records in source index order; use a sliding two-worker window, avoiding N async task allocations and pair-wise wait barriers.

**AD-04.5 — Stage and evidence integrity.** Reuse original plan's staged artifact, post-encode sample/depth validation and completion record; add explicit sample offsets and observed/expected byte extent evidence only for verified S3. Never generate tag mutation, ReplayGain, publisher side effects, or user-visible artifacts here. Cleanup failed stage after both children have settled.

**AD-04.6 — Qualification is evidence-bound.** Native FFmpeg Python simulations prove exact PCM feasibility and permit a bounded envelope probe; they cannot validate uncompiled Rust. Preserve all known prior verification records but do not relabel them as IP-04 host tests. No performance refactor of S1 while S3 is private.

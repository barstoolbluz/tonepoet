# IP-05 engineering decisions — 2026-10-25

1. **Existing album scheduler remains the authority.** Connect at
   `encode_cue_stream_album_for_scheduler_with_tool_limits_and_version_cache`
   after complete CUE materialization; return the same `ScheduledCueStreamBatch`
   as S1. Do not fork processor, metadata, logs or publisher.
2. **Explicit process opt-in, OFF by default.** Only
   `TONEPOET_CUE_S3_PILOT=1` invokes whole-album preflight. No new UI or
   persisted job/output setting. Intended exclusively for owner qualification.
3. **Reject conservatively and group-atomically.** Require the same complete
   authored track list, one physical source, source-rate/depth FLAC, unchanged
   PCM, supported platform, native child family budget, and typed IP-02/IP-04
   exact-source+planner preflight for all members before running any child.
   Fixed gain is an active PCM effect even though `is_true_peak()` is false:
   test `is_active()` rather than merely `is_true_peak()`.
   **D-01 corrective clarification:** `source.tracks` is selection-filtered;
   compare its length to the unfiltered CUE `album_metadata.total_tracks`
   before typed preflight. Do not infer completeness from the equality of
   two filtered arrays, or reject fully selected Range/Set selections.
4. **Do not attempt source-pass ReplayGain under S3.** The source-pass FIFO is
   attached to the shared S1 decoder. IP-05 instead returns no source-pass
   result, which feeds the existing native post-output scan and tag policies.
   Preserve exact user-selected Off/Track/Album/Both/SkipIfComplete policy.
5. **Preserve IP-03 private nonblocking entry point.** An already admitted
   album must not fail on transient global family-token scarcity; the new
   adapter first attempts the nonblocking fast path and, on a transient
   decline, acquires the *same* in-flight and family semaphores in consistent
   order via cancellation-aware awaits. Native supervised transport owns the
   guards until exit. No spin or extra scheduler.
6. **Failures and output ownership.** A child failure remains an attempted
   failure and is never silently re-run on S1. Existing `FailurePolicy` and
   album transaction own downstream handling; workers never publish to final
   paths. Do not add any destination state for admission or diagnostics.
7. **Qualification remains external.** Native FFmpeg reference probes passing
   here is not compiled Rust host evidence. No default promotion, performance
   claim or acceptance is made in IP-05; IP-06 is separately gated.

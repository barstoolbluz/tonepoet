# IP-05 static/adversarial source review

**Scope:** entire supplied accepted IP-04 source; only two Rust files modified
in production; additions reuse previously verified IP-02 range/extent and
IP-03/IP-04 native workers. Historical older `IP-01` to `IP-04` artifacts are
retained without rewriting their success claims or old source hashes.

- **Default safety:** read-only env decision exactly `== Ok("1")`; disabled
  returns before all new preflight. No S3 code in `processor.rs` or publisher.
- **Admission:** source.kind, selected count, whole authored sequence, physical
  group, source==decode FLAC path, source rate/depth, registered effects,
  active PCM gain, requested dither, Unix two-process capability, sufficient weighted FFmpeg
  native permits, full per-member typed IP-04 STREAMINFO/seek/planner and
  distinct scratch/final path checks. One member rejection => S1 for whole
  album, before child spawn; typed planner failure is not silently ignored.
- **D-01 correction:** `PreparedSource.tracks` and scheduler tracks can both
  contain only a selected subset. The guard additionally requires their length
  to equal `source.album_metadata.total_tracks`, which the CUE materializer
  obtains from the *unfiltered* sheet. Explicit full Range/Set selections
  remain eligible; incomplete ones reject before typed preflight or spawn.
- **D-02 correction:** source archive member inventory excludes all Python
  `__pycache__` and `.pyc` files; `verify_static_ip05.py` now rejects either.
- **Execution:** at most two in-flight S3 workers; initial IP-03 nonblocking
  fast path, then cancellation-aware wait on **same shared** global
  semaphores; per-worker error/cancel returns a failed `ScheduledTrackOutput`
  rather than retrying on S1. Exact PCM-byte verifier in existing native
  transport; child records and completed/failed verification evidence retained.
  Results ordered by authored index rather than completion order.
- **ReplayGain:** selected branch does not instantiate the source-pass FIFO;
  it sets `source_replaygain: None`, and existing album post-output stage owns
  `Off`, `Both`, `Album`, `Track`, inherited-tag and skip policy. It emits the
  existing `replaygain_policy` event and an explicit pilot choice record.
- **Publication:** S3 only returns staged `TrackArtifact`; no `output_root`
  writes added and no new visible files or data manifests. Existing
  `publish_album_output_with_authority` alone is final owner. No change in
  failure-policy logic, cross-device transaction or sidecar policy.
- **Performance:** bounded 2-worker window, one short native extent buffer
  per supervised pipe, no decoded PCM accumulation and no spin polling.
  Exact-path checks happen once per candidate before spawn. Temporary
  token contention queues instead of dropping an admitted member.
- **Known remaining qualification risks:** not compiled locally. Host must
  exercise source path shape, all RG modes and skip inheritance, actual
  tag/artwork order, external encoder subprocess errors/stop behavior,
  cancellation under contention and after a child starts, album failure vs
  allow-partial, double conversion prevention, same-FS and cross-FS
  transaction, dest exact-set while running and after failure. Only these
  grounded gates block promotion; no architectural rewrite requested.

# D-01/D-02/D-03 corrective conformance — October 25, 2026

| Finding | Narrow implementation | Qualification |
|---|---|---|
| D-01 Rust test tuple u16/u32 mismatch | Cast only expected streaminfo source depth to `u16` | Static corrected; **Rust compile NOT RUN** |
| D-02 broken-pipe encoder precedence/peer record omission | 750-ms diagnostic grace for broken-pipe consumer, error authority conditional on substantive consumer failure; other records by role | Early exit code-7/stderr and Io-primary peer preservation tests authored; **NOT RUN** |
| D-03 asynchronous 3,1,2 attribution test absent | Test-only runner controls completion, actual supervised PCM/encode, unique stages, source/record identity, omitted intervals, negative swaps | 3/3 independent native gap reference PASS; Rust coordinator test **NOT RUN** |
| S3 off / no S1 routing changes | No normal call site introduced | Static source scope PASS; host gate outstanding |

**Acceptance:** `implemented-unqualified`. Preserve admitted IP-03 and old S1 behavior. Audit correction source is not an acceptance sign-off.
---

# IP-04 packet conformance matrix

| IP-04 requirement | Implementation / evidence | State |
|---|---|---|
| Exact accepted IP-03 source identity | Input `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz`, SHA-256 `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7`; source identity letter | **PASS** |
| Reuse weighted IP-03 semaphore once | `track_executor.rs::run_cue_s3_pipeline_if_admitted`; typed verifier argument, one guard | Implemented; Rust host gate pending |
| Exact producer byte verifier on supervised pipe | `tool.rs::RealToolRunner::run_pipeline_with_exact_pcm_extent`; `ExactPcmExtent::observe_bytes` and `finish` | Implemented; Rust host gate pending |
| Typed IP-02 independent FLAC range preflight | `CueFlacRangeDecoder::prepare`, source STREAMINFO, `[start,end)`, planner/depth/physical source checks | Implemented; Rust host gate pending |
| Group-wide planner go/no-go before first child | `prepare_qualified_flac_ranges_for_group` checks all tracks | Implemented; Rust host gate pending |
| Stage independently, source-order results, no visible publication | `run_qualified_flac_range_member`/`run_qualified_flac_ranges_for_group`; reorder unit | Implemented; Rust host gate pending |
| Faithful metadata/producer+consumer records | Reuses `finalize_cue_stream_consumer_output`, added qualified-range evidence | Implemented; Rust host gate pending |
| Bounded resource / no whole-track raw staging | Two worker futures, shared IP03 cap, 2 pipes + 64-KiB transfer buffer/worker | Source-reviewed, runtime measurement pending |
| 16-bit + 24-bit 12/30 tracks PCM parity | Native independently checked 42 exact-range FLAC outputs against full-image PCM reference | **PASS native only**, Rust tests NOT RUN |
| Failure/EOF/cancel/reap/evidence | Focused Rust process tests authored, not run | **UNQUALIFIED** |
| 200-member + two-album contention / disk-full failure | IP-03 permit tests retained; full end-to-end 200-member / simultaneous-album IP-04 host gate pending | **UNQUALIFIED** |
| No S1 dispatch/publisher activation | No normal call site to private worker; S1 routing preserved | **PASS static** |
| Format / full suite / performance parity | No Rust toolchain; no S3 speedup claim | **NOT RUN** |
| Full cumulative archive / SHA / diff / handoff | New IP-04 full source and external SHA receipt; manifest / patch / ledger | **PACKAGED**, host acceptance pending |

No coverage claim goes beyond executable evidence. Remaining qualification concerns are focused host checks, not a request for redesign.

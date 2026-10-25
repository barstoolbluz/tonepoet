# IP-07 packet conformance matrix — implementation vs host acceptance

| Packet requirement | Integration location | Status in delivered candidate |
|---|---|---|
| Preserve input source and S1, default OFF | Unchanged S3 flag `cue_s3_pilot_requested`, original worker and S1 | Static source scope PASS; runtime NOT RUN |
| Whole-album IP-05 gate precedes separate meter gate | `stages.rs::cue_s3_pilot_preflight` then `cue_s3_source_meter_plan` | Implemented; native application NOT RUN |
| ReplayGain policy Trust + identity PCM | `cue_source_replaygain_eligible` checked after IP-05; FFmpeg source/extent typed checks | Implemented; edge policy tests require host |
| Physical FLAC generation, exact CUE half-open ranges | `CueS3SourceGeneration` + `CueFlacRangeDecoder::command/extent_verifier` | Implemented + independent FFmpeg sample slices PASS; Rust behavior NOT RUN |
| Supervised decoder-to-meter stream, no materialized PCM | `RealToolRunner::run_cue_source_range_observation` uses CLOEXEC anonymous pipe and existing supervisor | Implemented; actual child supervision/cancel NOT RUN |
| Signed s32le / native ReplayGain profile and per-track stats | `observe_cue_source_range_pcm`, existing `NativeReplayGainObserver`, `decode_pcm_bytes` | Implemented + synthetic native-equivalence Rust test authored, NOT RUN |
| Accurate extent, EOF, success, CRC failure | Owned reader extent plus supervised child completion and `-err_detect crccheck+explode` | Implemented; corruption/end-to-end NOT RUN |
| Global permit through complete child and reader lifetime | `track_executor.rs::run_cue_source_meter_with_concurrency` | Implemented; contention/cancel NOT RUN |
| Album-local cap, 256-MiB live statistics, separate reducer budget | `run_cue_s3_source_meter_album`, `cue_observer_storage_limit(prepared.len())`, existing reducer | Implemented; measured peak RSS/FD NOT RUN |
| Authored order & native owned sufficient-statistics reduction | Ordered `done[index]` + unchanged `reduce_cue_observations` | Implemented; out-of-order native trial NOT RUN |
| No original full-album tags on partial/failing conversion | `cue_s3_source_meter_outputs_match` and source-generation final check | Implemented; failure-policy adversarial tests NOT RUN |
| Optional observer failure falls back, not conversion retry | `try_encode_cue_s3_pilot` retains `source_replaygain: None` and original stage | Implemented; failure injections NOT RUN |
| Fatal writer/application failure, no rescanning | Existing `apply_cue_source_replaygain_if_eligible` + existing album stage | No source changes to writer/publisher; live negative test NOT RUN |
| Provenance / meter elapsed, queue, reason, exact member | Gated Stage-A `s3_source_meter_*` events + ordinary logs | Implemented; host interpretation and high-water NOT RUN |
| One cumulative source and independent SHA, host qualification | `SOURCE_INVENTORY.json`, `PRODUCTION_DIFF.patch`, external receipt and `HOST_QUALIFICATION.md` | Delivered; host acceptance PENDING |
| IP-06 sequencing | Packet amendment copied verbatim | IP-06 BLOCKED until exact host-accepted IP-07 |

# IP-04 conformance checkpoint — 2026-10-24

Private worker, exact boundary verifier, IP-03 weighted token custody and S1 isolation are source-implemented. Native independently checked 42/42 FLAC range PCM outputs. **Rust host gate NOT RUN / not accepted.** The authoritative IP-04 matrix is `ip04_verification/CONFORMANCE_MATRIX.md`; below is preserved IP-03/IP-01 history.

---

# IP-03 current conformance (October 24, 2026)

| Requirement | Implementation/evidence | Status |
| --- | --- | --- |
| Exact accepted input archive | Verified SHA-256 `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95`; git-less origin letter `d8c25d3` | PASS provenance of supplied bytes; Git cleanliness cannot be re-proven here |
| Shared FFmpeg/FFmpeg = 2, FFmpeg/SoX = 1+1 | `try_acquire_cue_s3_pipeline_permits`, named `cue_ip03_...` tests | Static PASS; Rust NOT RUN |
| Atomic decline/no self-deadlock/pre-spawn fallback | `try_acquire_many_owned`, partial-guard drop, `Declined` only prelaunch | Static PASS; Rust NOT RUN |
| Live-child permit lifetime across both children | `run_cue_s3_pipeline_if_admitted` holds guard over `ToolRunner::run_pipeline` | Static PASS; real supervisor test NOT RUN |
| Cancellation, tool failure, panic, early exit, slow pipe | Named `cue_ip03_failure_...`, `cue_ip03_supervised_...` tests | Authored; Rust NOT RUN |
| Shared global S3 cap and coexistence | `ToolConcurrencyLimits::cue_s3_in_flight` shared by `Arc`; two-album test includes ordinary FFprobe and historical S1 permit accounting | Static PASS; Rust NOT RUN |
| 200-track album does not accumulate permits | `cue_ip03_200_track_album_has_constant_inflight_and_no_leaks` | Authored; Rust NOT RUN; 200 native tracked album NOT RUN |
| Native independent FFmpeg two-process resource envelope | `ip03_verification/native_pair_envelope_probe.py`, `native_pair_probe_results.json`; 16 ranged cases byte-identical; two-pair max 4 children, 26 sampled parent+child FDs, 64 KiB kernel pipe | PASS **for independent subprocess fixture only** |
| No change to S1, planner, ReplayGain, publisher, IP-02 adapter | Baseline-vs-result file hashes (`SOURCE_DIFF.json`) | Static PASS; regression runtime NOT RUN |
| No strict all-native-child limit falsely claimed for S1 | Explicit one-family S1 permit coexistence test and caveat | Requirement met by honest disclosure; host process-limit review still needed |
| Rust fmt/compile/CUE & full workspace tests | Exact commands in current `VERIFICATION_RESULTS.md` | NOT RUN; required before IP-03 acceptance |
| S3 user admission | No production caller; S3 still disabled | PASS scope |

---

## Preserved IP-01 historical conformance

# IP-01 R2 corrective conformance map (not a Rust qualification)

| Requirement / audit issue | Corrected evidence | Executed status | Outstanding host gate |
|---|---|---|---|
| Original source and amended planning identities | `SOURCE_MANIFEST.json`, `PRODUCTION_DIFF.patch`, `verify_static_ip01.py` | Static **PASS** | No historical `.git` proof |
| D-01: no success inferred from selected RG path | `replaygain_application_settled` and fail-closed trace analyzer | Python **PASS** | Native failure/tag writer host **NOT RUN** |
| D-02: no unrelated scan correlation | Worker TLS correlation, tagged native start/finish, no legacy count join | Python **PASS** | Actual native worker **NOT RUN** |
| D-03: existing observer recovery != new group | Album recovery event, private-safe physical group on route and transport | Python **PASS** | Injected observer recovery in Rust **NOT RUN** |
| D-04: preadmission deferral vs admitted-to-start | `queue_deferred`/`queue_admitted`, named non-additive phases | Python **PASS** | Real scheduler child test **NOT RUN** |
| D-05: actual diagnostic sink and real route fixtures | Fresh-process Rust sink, live saturated queue, S1/fallback child tests | Tests *authored*, **NOT RUN** | Rust compilation, outputs, cancel/failure, parity |
| D-06: source-ordinal join | `source_pass_track_observer_settled.track_ordinal` and test | Static **PASS** | Rust noncontiguous source test **NOT RUN** |
| Fallible diagnostics leave conversions unaffected | Existing disabled gate, append best effort, no new defaults | Static **PASS** | Executable audio/log artifact parity **NOT RUN** |
| Overlap-safe performance attribution | Separate inclusive group/transport/observer/native intervals | Python **PASS** | Measured host timings and processor concurrency |
| End-to-end action→visible terminal, FFprobe, publication | Host stopwatch/ConversionRunTiming; unknown phases remain `NOT INSTRUMENTED` | **NOT INSTRUMENTED** | Host stopwatch and full file checks |
| RG Off/Both comparability | R2 worksheet and byte-identical host-gate-amended authority | Recipe **PASS**; benchmark **NOT RUN** | IP-06 cross-mode digest H-01 future corrective |
| Native Rust compile/fmt/tests | `VERIFICATION_RESULTS.md` named host commands | **NOT RUN** (no Rust toolchain) | Mandatory before accepting IP-01 |
| Future source-pass observer | **Not implemented**, conditional PX-C remains in IP-06 gate | Scope **PASS** | Only if measured IP-06 trigger |

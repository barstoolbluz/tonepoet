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

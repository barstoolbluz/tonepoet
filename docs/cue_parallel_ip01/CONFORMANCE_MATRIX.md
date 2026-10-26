# CURRENT IP-06 S3 default-on status — owner decision 2026-10-26

- **Policy selector:** IMPLEMENTED as default-on with trimmed `TONEPOET_CUE_S3_PILOT=0` opting out. Complete-album semantic/resource preflight is unchanged; disabled or ineligible inputs use S1 before any S3 spawn. New Rust assertions added; native compilation/tests **NOT RUN** in this environment.
- **Product disposition:** ACCEPTED by explicit owner direction. The requirement for narrow, measured CPU/storage default-admission cohorts is an **acknowledged exception**, not a PASS of that original gate.
- **Host speed data:** 140 reported timing rows, internally verified and recalculated in the previous independent audit. Source/configuration-bound qualification and varied host/attached-HDD measurements remain **NOT PROVEN**. No unobserved cohort is characterized as measured.
- **Full correctness, resource and fault gates:** No new native evidence since the independent review. Reported host PCM/tag/file parity and two adverse cases remain **HOST-REPORTED**; Rust full runtime and extended failure/peak budgets **NOT RUN** here.
- **Offline checks:** See new external receipt for the currently executed Python regression, selector source-scope and package-roundtrip checks; retained 25 FFmpeg-only PCM range probes do not constitute Rust qualification.

**Owner-approved default policy; native host release qualification outstanding.** Current record: `ip06_verification/DECISION_RECORD.md`. Everything below is dated historical evidence.

---

# HISTORICAL 2026-10-25 IP-06 evidence/conformance status (reviewed default-off state)

- **Selector rollback:** PASS by clean inverse-patch application and source-scope audit; compile/runtime assertion **NOT RUN**. The exact `"1"` opt-in is restored; all other Rust source files remain identical to the supplied archive.
- **Supplied primary timing table:** PASS **for internal CSV arithmetic only** (140 entries; 70 paired comparisons; seven workload types, two ReplayGain modes, five repetitions each). Source-bound full-app performance, route-excluded config hashes and admission-to-publication timing **NOT PROVEN**.
- **Actual S1/S3 ReplayGain instrumentation:** 12-track RG Both source meter selected and no post-output worker **reported** in accompanying summary; no per-run trace JSONL or measured fallback attribution supplied. **NOT INDEPENDENTLY VERIFIED** for the matrix.
- **Decoded PCM, ReplayGain, metadata, artwork, destination file parity:** **HOST-REPORTED** for basic cases. Audio/tag inventories and fixture identities were not supplied for independent comparison; artwork on/off matrix and additional IP-06 variants are not shown. **NOT INDEPENDENTLY VERIFIED**.
- **Directly attached HDD / constrained host / platform / tool-version:** **NOT RUN**. Four cold-first SSHFS spinning-disk albums were reported, but no direct HDD-under-pressure cell or matched small-host qualification. These unobserved cohorts cannot be globally auto-admitted.
- **Failure, cancellation and resource high-water:** SIGINT and corrupt frame are **HOST-REPORTED** only. Disk full, early encoder exit, blocked pipe, concurrent albums, scan/tag/publish faults, peak RSS/FD/temp/children/bandwidth: **NOT INDEPENDENTLY VERIFIED**.
- **Current sandbox offline tests:** 11 H-01 evaluator tests, 15 existing trace tests, exact destination self-test and 25 standalone FFmpeg PCM range checks **PASS**. Rust compilation, TUI/runtime execution and true end-to-end publication **NOT RUN** (no Cargo/rustc/rustfmt).

For provenance, exact fields and statuses see `ip06_verification/DECISION_RECORD.md` and `VERIFICATION_RESULTS.md`. The preceding IP-07 and historical matrices below remain unchanged.

---

# CURRENT IP-07 conformance checkpoint — 2026-10-25

Implementation covers an optional concurrent source-range native ReplayGain path after IP-05 S3 admission, separate supervised FFmpeg permits, exact EOF/decoder-exit and source-generation evidence, bounded two-meter window/256-MiB shared statistics, unchanged manifest-ordered album reduction, source-observer failure-to-postoutput fallback, fatal original metadata failure and unchanged S1, pilot gate and publisher. **Source scope static PASS and 25 standalone FFmpeg PCM range checks PASS; all Rust, tag, native app, failure injection, publication, resource and performance qualification NOT RUN.** Status: `implemented-unqualified`, no host acceptance, S3 default-OFF, IP-06 not executable. The detailed criterion-by-criterion table is `ip07_verification/CONFORMANCE_MATRIX.md` and the mandatory host recipe is `ip07_verification/HOST_QUALIFICATION.md`; the copied `ip07_verification/IP-06_SEQUENCING_AMENDMENT.md` controls next-packet order. Input: accepted IP-05 SHA-256 `af66b5c2a40904d4db6fec3fd4185914b4aadd32c74cf6fd6ff15568bf2cbd20` (reported host acceptance, exact bytes verified, no Git metadata).

---

# CURRENT IP-05 corrective conformance — 2026-10-25

D-01 incomplete selected CUE albums: authored-count guard and a named
12-authored/2-selected Rust regression added (**NOT RUN**, no Rust toolchain).
D-02 packaging: inherited `.pyc`/`__pycache__` excluded and checked in
source static verifier. Host run for actual S1 fallback and full album
qualification remains mandatory. Details: `ip05_verification/CONFORMANCE_MATRIX.md`.

---
# Original IP-05 conformance — 2026-10-25 (retained)

Static implementation covers explicit default-OFF gate, all-member prelaunch admission, non-Unix decline, original fallback, exact ranged children, native post-output ReplayGain, existing metadata/publisher and cancellation-aware weighted native permits. Independent native PCM probes PASS for 16/24 bit, 3/12/30 tracks and gapped range; destination checker self-test PASS. Rust integration, actual RG/tags/artwork/parity, failure injection, publication and cross-device gates **NOT RUN** (no Rust toolchain). The authoritative granular matrix is `ip05_verification/CONFORMANCE_MATRIX.md`.

---
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

# Current checkpoint — IP-03 (October 24, 2026)

**State: `implemented-unqualified` — NOT ACCEPTED, S3 remains OFF.** This section is the current cumulative status. The original IP-01 R2 corrective ledger is preserved below as historical evidence, not as the current next-packet instruction.

**Accepted input source:** `TonePoet_SNAPSHOT_d8c25d3_2026-10-24.tar.gz`, SHA-256 `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95`, `git archive` reported commit `d8c25d3` with no `.git`. Supplied `TonePoet_SOURCE_IDENTITY_IP03_2026-10-24.md` explicitly identifies it as the accepted IP-02 tree, with 24 native cells/840 ranges passing on FFmpeg 7.1.3; supplied host Rust gate was **7601 PASS, 1 known Reference qualification failure**, not a clean full-suite PASS. No fabricated Git commit/status.

**Scope:** One pre-spawn all-or-nothing nonblocking two-child S3 adapter within `src/convert/pipeline/track_executor.rs`; global shared S3 in-flight semaphore added to existing `ToolConcurrencyLimits`. Family cost is FFmpeg=2 for FFmpeg/FFmpeg; FFmpeg=1 and SoX=1 for FFmpeg/SoX. Max S3 workers = `min(2, max(1, ffmpeg_family_capacity / 2))`, shared by all holders of the same limits. Failed acquire releases all earlier tokens; closed semaphore/cancellation/invalid family is an error. A genuine runner execution failure is `Executed(Err(_))`, never a pre-spawn `Declined` fallback. The existing `ToolRunner::run_pipeline` is invoked directly under RAII; no second permit acquisition. S1 and unrelated command scheduling are untouched.

**Evidence:** `ip03_verification/` (static delta auditor, current source manifest, original-vs-output diff, native FFmpeg pair resource probe with SHA-256 audio hashes). Authoring environment has FFmpeg but **no Cargo, rustc or rustfmt**. The independent native probe passed 16 PCM-exact ranges at one/two concurrent FFmpeg pairs; it cannot qualify the Rust transport/supervision. The Rust unit, failure/cancel/panic and real supervised-process tests are *authored but NOT RUN*. Host acceptance requires `cargo fmt` + named `cue_ip03` + full CUE/full workspace tests, real child FD/RSS and cancellation/reaping, and at least the preexisting known Reference qualification drift tracked separately.

**Compatibility:** No S3 scheduler/worker caller is wired in; no ordinary conversions can select this new path. `cue_flac_range.rs`, ReplayGain, source/metadata authority, planner, publisher, and existing S1 remain byte-identical to the input. A legacy S1 segmented pipeline still holds one FFmpeg family token for its two live FFmpeg children; the IP-03 admission test explicitly exposes that historical accounting rather than claiming strict limits for all native children. Do not change S1 without independent resource evidence and a measured throughput review.

**Next packet:** IP-04 only **after the owner/build host accepts the exact IP-03 archive**. Use the exact delivered archive name and SHA from the external IP-03 `DELIVERY_RECEIPT_*.md` (the digest of an archive cannot be embedded in the archive itself without invalidating that digest). If a gate fails, correct IP-03 narrowly and repackage first.

---

## Preserved IP-01 historical ledger

# TonePoet Parallel CUE — cumulative implementation status after IP-01

**Recorded:** 2026-10-22 (America/New_York). **Planning authority:** full byte-identical R2 corrective **host-gate-amended** bundle in `planning_authority/`. This file supersedes the preserved plan's *historical* pre-implementation `IMPLEMENTATION_STATUS.md`; the historical plan is not edited in place.

**Input source:** `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz`, exact SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`; source contains no `.git` and its reported commit `98aae2b` is not independently re-verifiable here.

**IP-01 R2 corrective state: `implemented-unqualified`. NOT ACCEPTED.** The independent read-only audit of R1 (`4c5626722df9...`) reported D-01 through D-06; these receive narrow observation/analysis/test corrections. Exactly the same three existing Rust modules differ from the original source. The amended authority remains byte-identical. Offline adversarial trace tests expanded from four to fifteen and PASS; **the new Rust subprocess and native fixture tests have not run** because `cargo`, `rustc`, and `rustfmt` are absent. Native audio/tag/cancellation parity and real benchmarks remain **NOT RUN**, not PASS. Full archive SHA is recorded in the separately delivered corrective receipt; the archive cannot embed its own digest.

| ID | State | Dependency/next action |
|---|---|---|
| IP-01 Observability and baseline | **implemented-unqualified** | Build host verifies exact delivered archive hash; runs Rust fmt/test/fixture, compares audio/policies; obtains real Off/Both S1 baseline before acceptance |
| IP-02 Sample-exact ranged seek | **planned — blocked on accepted IP-01** | Consume exact **accepted** IP-01 full source, not baseline archive |
| IP-03 Shared resource permits | **planned** | Consume accepted IP-02 |
| IP-04 Private parallel pilot worker | **planned** | Consume accepted IP-03 |
| IP-05 Feature-OFF end-to-end S3 integration | **planned** | Consume accepted IP-04 |
| IP-06 RG-attributed qualification | **planned** | Consume accepted IP-05; matched Off/Both cells and amended defer branch |
| PX-C source-pass per-worker observer | **deferred/conditional; NOT IMPLEMENTED** | Only after IP-06 measured S3 Off win/Both loss attributable to additional output scan |

No changes to ordinary routing defaults, CUE parsing, source-pass ReplayGain engine, scheduler semantics, native codecs, atomic publication, cancellation, logging formats or metadata authorities. IP-01 **did not implement** a new source-pass ReplayGain observer; `source_pass_track_observer_settled` merely times the **already-existing** native S1 FIFO observer.

## Next-packet acceptance rule

Until the host signs IP-01 off, **do not start IP-02 as accepted dependent work**. A future host acceptance must include named test outputs, media hashes, compilation/format gate, diagnostic-disabled parity and host benchmark data bound to the exact archive SHA. If a Rust/compiler defect is found, issue the smallest corrective IP-01 packet and regenerate the cumulative source with a new digest.

## Corrective audit ledger (R2)

| Audit | Resolution in R2 | Verification boundary |
|---|---|---|
| D-01: source scan selected misreported applied | Settled `replaygain_application_settled` with actual Result + `tag_action` enum; summary refuses selection-only success and distinguishes skip/failure | Python failing/partial/skipped fixture PASS; Rust writer outcome fixture **NOT RUN** |
| D-02: foreign native scan rebound by cardinality | Thread-local opt-in CUE token on existing native measure event pair; summary joins exact token only, validates finite nonnegative durations and refuses mixed/partial legacy attribution | Python mixed/two-run/invalid-duration fixtures PASS; actual native worker **NOT RUN** |
| D-03: observer retry counted as extra group | Single observer-recovery *episode* boundary; group IDs on route/transport; summary counts physical groups and attempts separately | Python recovered group/unknown duplicate fixtures PASS; source-pass observer failure injection **NOT RUN** |
| D-04: preadmission deferral invisible | `queue_deferred` with stable queue_full/queue_busy reason, independent first-deferral-to-admission interval | Python cancellation/deferral fixtures PASS; Rust live queue sink test **NOT RUN** |
| D-05: synthetic tests insufficient | Fresh-process sink-on/off, concurrent JSONL correlation, native token isolation, existing direct/fallback route fixture subprocess, native source eligibility and negative metadata writer test cases | New Rust test bodies **NOT RUN**; full conversion/audio/terminal/cancellation parity still host-gated |
| D-06: observer lacks authored track key | Emits `track_ordinal` alongside local group index; noncontiguous group regression added | Source static PASS; Rust regression **NOT RUN** |
| H-01: cross-RG configuration digest | **Not touched**. It is an amended **IP-06** evaluator gate, not part of IP-01 source correction | IP-06 evaluator correction/qualification required before comparison |

No PX-C observer, S3 worker, new PCM transport, scheduler behavior change or ReplayGain engine rewrite was implemented.

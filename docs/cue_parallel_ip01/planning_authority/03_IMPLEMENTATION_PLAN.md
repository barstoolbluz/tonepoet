# Executable plan, dependency graph and session-sized scopes

**Baseline:** `98aae2b`, original archive SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. Six **committed** critical-path packets; **six one-session implementation attempts minimum**, plausibly six to ten sessions including source drift/corrective passes, and separate build-host qualification as needed. These are session counts, **not wall-clock or delivery-date promises**. Each completed packet delivers a new **full cumulative source archive**, not a patch-only handoff.

## Critical path (DAG)

```text
[baseline 98aae2b]
       |
     IP-01  baseline observation / before-and-after recipe
       |
     IP-02  FLAC sample-exact range adapter + go/no-go
       |
     IP-03  minimal shared S3 pipeline permits and bounded worker cap
       |
     IP-04  private, feature-disabled bounded ranged worker
       |
     IP-05  end-to-end pilot behind off-by-default gate
       |
     IP-06  reproducible conformance + performance, conditional promotion
       |
[qualified limited FLAC+CUE -> FLAC tracks or retained S1]
```

Dependencies are **acceptance**, not just a file existing. If IP-02 cannot prove exact FLAC range access, IP-03–IP-06 are blocked and ADR decision returns to review. If the build host cannot run Cargo in the implementing session, the implementation status must remain `implemented-unqualified`, and the next implementation packet is **not** treated as safely unblocked until the specified host evidence lands.

## Committed packets and testable artifacts

| ID (link) | Cohesive scope, example touchpoints | Independent gate | Rollback | Risk |
|---|---|---|---|---|
| [IP-01](packets/IP-01_observe_baseline.md) | Existing production queue-stage timing and opt-in CUE decision reasons (`processor.rs`, `stages.rs`), plus source-independent fixture/trial recipe | Race-safe minimal timing fields, no behavioral change, sample baseline recorded or NOT RUN | Remove opt-in trace | Low-medium |
| [IP-02](packets/IP-02_qualify_flac_ranges.md) | Nonactivated FLAC integer-sample source-range command and exactness harness (`materializer_cue.rs`, nearby tool/stages tests) | End-to-end decoded samples equal serial reference for adversarial offsets/variants; fail closed on unknown/flawed seeks | No production route switched | High feasibility |
| [IP-03](packets/IP-03_global_resource_admission.md) | Narrow reuse of existing shared per-family `ToolConcurrencyLimits` for one S3 decoder/encoder pair, plus one global S3 in-flight cap; no general RAM/FD/I/O scheduler (`track_executor.rs`, small new worker helper) | Exact live-child permits, no partial wait/deadlock, bounded pipe/FD/process peak across album groups | New path disabled, old run paths unchanged | Medium |
| [IP-04](packets/IP-04_parallel_worker_private.md) | Internal typed ranged worker → **existing** planner-authorized encoder, bounded concurrent staging results (`stages.rs`, `track_executor.rs`, runner API if required) | Test-invoked private parallel result exactly matches reference and returns correct records/cleanup; no user activation | Private route remains unreachable | High |
| [IP-05](packets/IP-05_integrate_pilot.md) | Off-by-default admission into real CUE album ownership; ReplayGain Both, metadata, verification, failure policy, cancellation, publication, progress and log identity in same slice | Real fixture end-to-end complete or NOT RUN; no regression; gate off if unqualified | Disable pilot route and select S1 | High |
| [IP-06](packets/IP-06_qualify_and_promote.md) | Host-verified conformance + representative trials; measured selection/crossover, optional guarded promotion or rejection | Full-run net benefit with RG Both; gate outcomes and honest qualified matrix | Keep default S1 if evidence is negative/absent | Medium |

**First shippable functionality** is the IP-05 guarded pilot **after** correctness and host build verification (not automatically the default). IP-06 owns the explicit choice to turn it on by default for a small supported subset. The user need not wait for every codec family.

## Packet acceptance and source dependency ledger

| Packet | Must consume exactly | Accepted successor artifact name (pattern; SHA recorded when real) |
|---|---|---|
| IP-01 | Original verified `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` | `TonePoet_CUE_IP01_FULL_SOURCE_<SHA12>.tar.gz` |
| IP-02 | **Accepted** IP-01 cumulative full-source archive | `TonePoet_CUE_IP02_FULL_SOURCE_<SHA12>.tar.gz` |
| IP-03 | **Accepted** IP-02 cumulative full-source archive | `TonePoet_CUE_IP03_FULL_SOURCE_<SHA12>.tar.gz` |
| IP-04 | **Accepted** IP-03 cumulative full-source archive | `TonePoet_CUE_IP04_FULL_SOURCE_<SHA12>.tar.gz` |
| IP-05 | **Accepted** IP-04 cumulative full-source archive | `TonePoet_CUE_IP05_FULL_SOURCE_<SHA12>.tar.gz` |
| IP-06 | **Accepted** IP-05 cumulative full-source archive | `TonePoet_CUE_IP06_FULL_SOURCE_<SHA12>.tar.gz` |

`<SHA12>` is a naming *placeholder*, not a claimed output path. Each next packet receives the exact **actual** accepted archive path/hash via `NEXT_SESSION_HANDOFF.md`; never construct an unverified path by replacing these placeholders.

## Integration contracts already in production

- **Identity:** `TrackSourceRef::CueStreamSegment` in `src/convert/pipeline/types.rs:1249` and `CueImageMaterializer` in `src/convert/pipeline/materializer_cue.rs:491-690`. Never redefine CUE file/track authorities.
- **Planner:** `cue_stream_phase1_direct_plan_for_paths` in `src/convert/pipeline/track_executor.rs:1355-1539`; planner controls codec, effects and output representation.
- **Execution:** `ToolRunner`, `run_pipeline`, segmented pipeline and cancellation in `src/convert/pipeline/tool.rs:256-320` and `:2374-2700`; `ToolConcurrencyLimits` in `src/convert/pipeline/track_executor.rs:709-804`.
- **Album/group:** `src/convert/processor.rs:5350-5555`, `src/convert/pipeline/stages.rs:44413-44943` and `:45202-45585`; preserve group and original index in results.
- **Postprocess:** `src/convert/pipeline/stages.rs:2436-2575`, `:15554-15870`, `:27544-28100` and `src/convert/pipeline/transactional_state.rs:20-128`. Do not publish from a worker.

## Packet sizing justification and stop/split rule

IP-01 reuses existing wall-clock/log counters, adding only a cheap missing CUE-specific boundary if a real measurement gap remains. IP-02 creates a probeable, unused producer—not both producer and activation. IP-03 only extends existing shared *S3* pipeline child-permit acquisition, with a bounded in-flight count; it does not rework unrelated execution or implement a second resource-manager subsystem. IP-04 changes the private executor and returns staged results, not UI ownership. IP-05 is the sole public-control integration packet; its acceptance includes every necessary metadata, ReplayGain, provenance and transaction guarantee **within that packet**, not deferred cleanup. IP-06 is a narrow measurement/admission-decision packet. All are intended for one focused GPT-6 session; IP-04/05 may require a new dependent subpacket if more than two or three large production modules must change. Stop on missing toolchain/fixture, unsupported source drift or risk to publication; publish honest `implemented-unqualified` or `blocked` state and an independent corrective prompt rather than silently broadening scope.

## Conditional extensions — not committed work

See [provisional/INDEX.md](provisional/INDEX.md) for promotion gates and outlines. Extensions considered **only after** IP-05/IP-06 data: FLAC input → additional planner-authorized output containers; WAV/RF64 exact source-range readers; Wave64/AIFF; WavPack `wvunpack` seek contract; ALAC and other lossless; lossy sample-limited segments and gapless delay; per-track effect/fallback-cliff avoidance; multi-FILE scheduling; source-pass ReplayGain observer; specialized disc-derived inputs. No adapter is declared safe because FFmpeg happens to recognize a codec.

## Non-negotiable end-state

A qualifying source emits exactly the same decoded PCM samples per output as the established reference, plus semantically equivalent album/track RG tags, all original multi-valued metadata, artwork, records, safe publication and failure behavior. A nonqualifying source **keeps the old path**. A parallel path with no reproducible, end-to-end elapsed-time benefit is not promoted.

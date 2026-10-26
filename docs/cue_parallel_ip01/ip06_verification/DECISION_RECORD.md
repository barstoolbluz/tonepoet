# IP-06 decision record — owner-directed S3 default ON

**Decision date:** 2026-10-26 (America/New_York)  
**Product disposition:** **APPROVE S3 AS THE DEFAULT REQUEST** for every album that passes the pre-existing complete-album S3 preflight.  
**Engineering disposition:** **OWNER-ACCEPTED PERFORMANCE QUALIFICATION EXCEPTION; NATIVE RELEASE QUALIFICATION OUTSTANDING.**  
**Reversibility:** `TONEPOET_CUE_S3_PILOT=0` selects S1. Ineligible albums retain the existing automatic S1 path.

## Decision authority and exact scope

The product owner explicitly instructed on 2026-10-26: "make the verdict: turn this on. i'm okay with it being on." This supersedes the **recommendation**, not the measurements or findings, in the independent 2026-10-25 review, preserved verbatim in `INDEPENDENT_REVIEW_2026-10-25.md`.

The owner elects the *default-on policy* in the face of documented performance-qualification gaps. This is an express exception to the IP-06 requirement to confine automatic admission to measured hardware/storage cohorts. **It is not a finding that IP-06's full acceptance matrix passed.** The historic IP-06 planning packet and its IP-07 sequencing amendment remain byte-identical; this contemporaneous decision records their authorized policy exception without rewriting that history. No unresolved correctness regression is being accepted as known good, and native release gates remain necessary before shipping.

Implementation is deliberately narrow: restore the 2026-10-25 candidate selector and its deterministic unit assertions in `src/convert/pipeline/stages.rs`; do not change the existing all-member preflight, typed FLAC range decoder, concurrent workers, source-meter ReplayGain, metadata or publication. Change comments to accurately identify this as an owner-selected policy rather than a performance-certified universal selector. Every code path other than the request default is unchanged from the independently reviewed default-off archive.

The selector's precise behavior is:

- Environment variable **unset**: request S3.
- `TONEPOET_CUE_S3_PILOT=0`, including surrounding whitespace: select S1.
- Explicit `1` and every other present value: request S3 (current compatibility behavior; no new parser or user-facing setting).
- Any failed S3 *preflight* returns S1 before S3 children start, subject to preexisting admission behavior. Do **not** retry a failed running S3 conversion under S1.

The preflight checks complete multi-track authored CUE membership, identity-PCM FLAC output and effects policy, exact single-file source/planner properties, available native family capacity and supported supervised runner. It does **not** certify CPU model, filesystem latency, direct-HDD random-seek behavior, memory pressure, or whole-run speed on an unmeasured host. These limitations are *accepted as performance risk* by the owner, not relabeled as tested.

## Evidence supporting the policy choice

The 2026-10-25 supplied 140-row timing CSV has seven workload categories, two distinct ReplayGain modes and five S1/S3 pairs per category and mode. The independent review verified its internal pairing, exit-code entries and reported ratios, but did not reproduce the native TonePoet conversions. Recorded ratios on the supplied Threadripper 7970X/NVMe host include 12-track 16/44.1 at **1.44x RG Off / 1.78x RG Both**, and the 12-track realistic-album case at **1.39x RG Off / 1.70x RG Both**. The one-track control is an S1/S1 refusal, not S3 evidence. Four commercial albums read from an SSHFS-backed spinning disk were reported at **1.58–1.67x RG Both**; the remote page cache and SSHFS read-ahead limit extrapolation to directly attached HDDs.

The input report states that decoded PCM, tags, output file sets, a SIGINT mid-run and corrupt-source failure behaved correctly on the build host. Those source-bound outputs and complete run receipts were not supplied. The independent reviewer found no demonstrated S3 output defect in the evidence available; importantly, **absence of an observed defect is not full correctness qualification**.

## Explicitly accepted uncertainty

The owner authorizes the selector despite missing matched measurements for smaller CPUs, non-NVMe filesystem/under-pressure regimes and diverse FFmpeg/tool versions. The earlier IP-06 review also identified missing route-excluded per-pair configuration hashes; source/fixture identities; admission-to-visible-publication timings; route and ReplayGain path trace for every run; peak RSS, FD, temporary-space, bandwidth and child counts; and a complete failure/artwork/tag/publication matrix. The supplied timing CSV does not meet the formal IP-06 evaluator schema, and must not be laundered into a PASS. These gaps remain on the engineering qualification register.

This product decision accepts possible performance regressions or higher resource consumption on unmeasured, *functionally eligible* hardware. It does **not** waive real conversion correctness, atomic publication, absence of orphaned processes, or native compiler/test verification. S1 is the immediate rollback option if trouble is observed.

## Implementation and verification boundary

- Source input: `TonePoet_CUE_IP06_FULL_SOURCE_3d561894f1a4.tar.gz`, independently hashed as `9722a3e9551fa2f2e257a3d690ef9c6e19945072c61375d01fb95875d736d921`. That archive itself came from the original default-on snapshot SHA-256 `1f1fcf261ac35aa585580489a64989541a71916af58f88bc592f0a91aa196d2e`.
- Owner change: **only** `src/convert/pipeline/stages.rs` among production modules, plus current decision documentation and reproducible packaging/checksum tooling. Preexisting IP-06 H-01 evaluator correction stays in place.
- Offline qualification: selector change is anchored to the supplied original patch; a complete previous-archive byte comparison and package round-trip are required by the new delivery receipt. The existing Python H-01 and trace suites are executable locally. **Rust compile, formatting and native end-to-end tests are NOT RUN here** because `cargo`, `rustc` and `rustfmt` are unavailable.
- Native host release gate: validate archive hashes; `cargo fmt --all -- --check`; targeted S3/CUE tests; full workspace tests; default-on, explicit `0`, and preflight-fallback album selection; RG Both/Off parity; interrupted/corrupted conversions; destination-file integrity and child reaping. Record any known non-IP-06 Reference drift separately. Do not claim a native test passed unless it ran. After host acceptance, owner may merge and deploy this default-on policy.

**Decision:** S3 default-on **AUTHORIZED BY PRODUCT OWNER**, operational verification **PENDING NATIVE HOST**, global IP-06 engineering qualification **INCOMPLETE BY EXPRESS POLICY EXCEPTION**. The independent review's prior dissent is retained as an auditable historical input, not silently erased.

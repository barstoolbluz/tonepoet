# TonePoet: parallel CUE decomposition — execution guide

**Planning release:** R2 corrective, 2026-10-22. **Mode:** source-grounded architecture and execution packets; **no production code has been edited**. This is a planning/tooling bundle, not a modified TonePoet source archive.

## Baseline identity and limits

- Source input: `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz`; SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26` (**independently checked**).
- Source identity letter: commit `98aae2b`, branch `main`, clean tree; these are **reported by the attached source-identity document**, not independently established from the archive, which lacks `.git`.
- Source letter reports `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`: 7,575 tests passed, zero compiler warnings **at that prior checkout**. No Rust toolchain is installed in this planning environment; this session did **not** run Cargo or qualify any implementation.
- User's reported 2–5× CUE slowdown compared with foobar2000 is an uncontrolled observation. No CUE wall-clock benchmark has been run against this source in this planning session.
- A small FFmpeg/FLAC seeking experiment appears in the source audit as *exploratory only*; it is not a certification of independent sample-exact range access.

## Reading order and decision

1. [01_SOURCE_AUDIT.md](01_SOURCE_AUDIT.md): current path and evidence, including the UI's ReplayGain default.
2. [02_ARCHITECTURE_DECISION.md](02_ARCHITECTURE_DECISION.md): proposed minimal S4 hybrid (retained sequential transport + narrowly qualified independent FLAC range workers), conditional on hard gates, including the Unix runner boundary and existing shared permit reuse.
3. [03_IMPLEMENTATION_PLAN.md](03_IMPLEMENTATION_PLAN.md): critical-path DAG and packets, first shippable outcome, conditional extensions.
4. [04_FORMAT_CAPABILITY_MATRIX.md](04_FORMAT_CAPABILITY_MATRIX.md): verified source routes versus **unqualified** independent range capabilities.
5. [05_CONFORMANCE_AND_PERFORMANCE.md](05_CONFORMANCE_AND_PERFORMANCE.md): qualification and performance decisions.
6. [06_INDEPENDENT_ADVERSARIAL_AUDIT.md](06_INDEPENDENT_ADVERSARIAL_AUDIT.md) and [07_CHANGE_SUMMARY.md](07_CHANGE_SUMMARY.md): original R1 independent audit, exact corrections, and proportionate exclusions.
7. [08_HOST_GATE_AMENDMENT.md](08_HOST_GATE_AMENDMENT.md): build-host correction to IP-06's decision procedure; architecture and packets unchanged.
8. [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md), [ARCHITECTURE_DECISIONS.md](ARCHITECTURE_DECISIONS.md), [CONFORMANCE_MATRIX.md](CONFORMANCE_MATRIX.md), [VERIFICATION_RESULTS.md](VERIFICATION_RESULTS.md), [NEXT_SESSION_HANDOFF.md](NEXT_SESSION_HANDOFF.md).

**Committed sequential critical path (6 modest packets, with IP-03 now a scoped permit helper rather than a global resource-management rewrite):** [IP-01](packets/IP-01_observe_baseline.md) → [IP-02](packets/IP-02_qualify_flac_ranges.md) → [IP-03](packets/IP-03_global_resource_admission.md) → [IP-04](packets/IP-04_parallel_worker_private.md) → [IP-05](packets/IP-05_integrate_pilot.md) → [IP-06](packets/IP-06_qualify_and_promote.md). IP-01 is independently useful; IP-02 is a **go/no-go** feasibility gate. Do **not** push past a failed gate. Conditional follow-ons are in [provisional](provisional/INDEX.md).

## One fresh session = one packet

For each session supply: (1) the *latest accepted full-source archive* with exact SHA-256; (2) exactly one `packets/IP-XX_*.md` prompt; (3) the current cumulative ledger files above, updated by the latest accepted session; and, when available, focused fixture/evidence. For the **first** session only, use this baseline archive. Do not rely on a previous chat, a flattened patch without its parent, an unverified handoff, or the source name alone.

**In each implementation session**: check archive SHA and state; inspect actual touched source and current assumptions; protect unrelated user changes; implement a narrowly scoped, independently reviewable change; execute every test actually possible; record unsupported test/build/bench steps as **NOT RUN**; run static checks; update all affected ledgers and exact cumulative source hash; produce an **entire cumulative full-source archive** plus a short changed-files report. All output files and logs must be packaged and hashed; build cannot be claimed from source inspection. Preserve the previous accepted archive as rollback input. If a review finds a defect, correct within the session if feasible; otherwise publish `implemented-unqualified` or `blocked`, not `accepted`.

**Acceptance authority:** The user/build host explicitly accepts a packet after reproducing the required evidence on the exact delivered source and tool versions. A session with no Rust toolchain can deliver source **implemented-unqualified**, never imply tests passed. Before starting the next packet, rebind its prerequisites to the actual accepted cumulative archive.

**Stop/split policy:** If a proposed packet spans unanticipated planner/publication changes, cannot prove its seek boundary, requires an unqualified tool dependency, or cannot be tested/verified coherently in one session, preserve the verified increment in the archive, mark it `implemented-unqualified` or `blocked`, explain the missing gate, and draft a new *dependent* corrective packet. Never declare its successor unblocked on the strength of a plausible source-level argument.

## Integrity checks

From the extracted planning directory:

```sh
python3 tools/verify_bundle.py .
# Optional when the exact original source is separately extracted:
python3 tools/verify_source_refs.py . /path/to/tonepoet
sha256sum -c SHA256SUMS
```

`manifest.json` is an inventory of files, source identity, packet DAG, and per-file hashes. `SHA256SUMS` includes `manifest.json` and all other distributed files except itself. The archive SHA-256 is shown in the handoff/outer delivery and is checked separately. The planning package deliberately does **not** redistribute the large source archive.

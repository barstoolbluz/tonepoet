# IP-07 verification results (authoring environment 2026-10-25)

| Gate | Verdict | Evidence / limitation |
|---|---|---|
| Accepted input archive identity | **PASS** | SHA-256 `af66b5c2a40904d4db6fec3fd4185914b4aadd32c74cf6fd6ff15568bf2cbd20` independently recalculated |
| Input prompt packet identity and completeness | **PASS** | Original `TOOLS/verify_packet.py` passed; packet includes the IP-06 sequencing amendment |
| Source scope and static assertions | **PASS when accompanying STATIC_GATE_OUTPUT.txt says PASS** | `verify_ip07_static.py --base INPUT_SOURCE_ROOT`; exactly five Rust production modules changed, each within required integration surface; no changes to S1, planner, publisher or native true-peak crate |
| Native standalone IP-02 FFmpeg seek/CRC/s32le sample contract | **PASS (25 cells)** | 5 FLAC rate/channel/depth combinations x 5 nontrivial ranges; exact byte-for-byte PCM vs independent full decoder, including nonsecond and final EOF. `NATIVE_FFMPEG_RANGE_RESULTS.json` + reproducible script |
| Source observation Rust tests | **NOT RUN** | `cue_ip07_*` tests added to replaygain and stages, toolchain absent |
| Cargo fmt/build/full suite | **NOT RUN** | `cargo`, `rustc`, `rustfmt` unavailable and Rust distribution endpoint unavailable |
| Real S1 / original S3 / IP-07 source-meter app-level tag parity | **NOT RUN** | No built TonePoet executable, real source scan/gain/tag application not directly executed |
| Supervisor lifecycle, contention, cancellation, error recovery | **NOT RUN** | Source static review only; real child shutdown/FD/RSS requires host |
| Metadata writer failure/partial cohort and destination publication | **NOT RUN** | Existing fatal writer and publication owner reused; integration not exercised |
| Matched >=5 paired performance / >=1.25x promotion gate | **NOT RUN** | Explicitly **IP-06**, no IP-07 promotion decision |
| Real native-child/RSS/FD high water and meter queue wait | **NOT RUN** | IP-07 emits gated per-member admission/wall/permit-wait telemetry; no complete app workload collected |

**Previous host measurements must not be recast as IP-07 data.** The packet's 1.088-second standalone parallel meter time is reported historical evidence, not measured integrated CPU or end-to-end speed.

**Risk and acceptance blockers:** (1) Rust compile/fmt and broad regression suite; (2) real per-track and album loudness/serialized tag equivalence in modes Track/Album/Both, inherited trust/skip and silent/short conditions; (3) assured child cleanup/permit release on all adversarial errors and source-generation drift; (4) fallback-to-post-output and fatal writer paths; (5) full metadata/artwork/provenance/destination parity; (6) actual concurrent resource and wall-clock measurements. See `HOST_QUALIFICATION.md` before sign-off.

# LIVE cumulative handoff — IP-02 delivered but NOT YET ACCEPTED

**Date:** October 24, 2026. **Next:** verify the exact IP-02 source bundle on a Rust/FFmpeg build host, then seek explicit owner acceptance. Only after IP-02 is accepted should IP-03 begin. Do not re-execute IP-01 or accidentally start a parallel worker in this packet.

**Consumed full-source archive:** `TonePoet_SNAPSHOT_847fc5e_2026-10-24.tar.gz`, SHA-256 `880c5e2693ee09ef56723c0ac7b72cbd6080340c08671f0f36e9288b9a994112`. Supplied identity letter reports commit `847fc5e`, clean and pushed, with IP-01 and the ReplayGain FIFO readiness correction already integrated. There is no `.git` inside the archive; the report's Git facts are not independently proven here.

**Exact next input file:** `TonePoet_CUE_IP02_FULL_SOURCE_f97081a14dec.tar.gz`. Its stable 12-hex suffix is from the actual deterministic 1,124-file source/repository tree digest `f97081a14dece5c1d1553cfbaf3bd60ecc02876b85fb98ba7346f7e8b0419308`. It is **not a claimed archive SHA**. The complete delivery archive's independently computed SHA-256 is in the delivered `IP02_DELIVERY_SHA256SUMS.txt` external sidecar and must be verified before any acceptance. A single tar archive cannot truthfully embed its own final gzip SHA-256.

**What was implemented:** an unused typed FLAC range producer using integer-second input seek plus exact-sample `atrim` in the same s32le carrier as S1; verified STREAMINFO tuple; `u128` exact byte-extent verifier; fail-closed unsupported source/range refusal; Rust boundary unit tests; native seeded multi-format/seektable byte-level verification and adversarial lifecycle tests. CRC checks require `crccheck+explode`, not `explode` alone. **No production route/admission/worker/publisher change.** S1 is intact.

**State:** `IP-02: implemented-unqualified`, **SEEK-EXACTNESS NATIVE TEST PASS ONLY ON THE RECORDED TOOL CELL; BUILD/EXECUTION NOT RUN**. No local Cargo/Rustfmt/Nix exists. The native evidence is reproducible with `docs/cue_parallel_ip01/ip02_verification/native_qualify_ranges.py`, with exact fixture hashes and version fingerprints in `native_host_results_*.json`. The original planning-status ledger remains below as history.

**Acceptance gate to run now:** SHA-256 verify, extract source, verify embedded source-file manifest, `cargo fmt --all -- --check`; specific named tests `cargo test --workspace --exclude tonepoet-true-peak cue_ip02 -- --nocapture`; then `cue_stream`, `cue`, and full `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak`. Re-run native qualification on the build host's actual FFmpeg install/CRC path against the exact source, record complete exit codes (no zero-test passes), sample identities, tool SHA-256 and Rust versions in `VERIFICATION_RESULTS.md`, and update the IP-02 ledger only on genuine acceptance. The previous two source-lock drift failures reported by the input identity letter must not be concealed or mislabelled.

**Required next packet (after owner accepts IP-02):** `packets/IP-03_global_resource_admission.md`, consuming the accepted IP-02 archive hash. The existing ToolRunner two-child transport is Unix-only; count both FFmpeg subprocesses using shared limits. A successful standalone FFmpeg seek does NOT qualify its pipeline runner. Do not spin up IP-03 from this unqualified snapshot. If any seek exactness/CRC/lifecycle test fails, preserve the inert adapter as an experiment, keep S1, and issue a narrowly scoped correction.

---

## Historical planning handoff from 2026-10-22 (superseded; retained for provenance)

# Durable next-session handoff — start IP-01

**Date:** 2026-10-22. **Completed:** R2-corrected planning documents, independent audit, six scoped execution-ready packets and conditional adapter outlines. **Not completed:** any production code edits, any production CUE runtime benchmark, new Rust build/qualification.

**Next:** execute [packets/IP-01_observe_baseline.md](packets/IP-01_observe_baseline.md) in one fresh GPT-6 session.

**Exact source input:** `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` with verified SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. The planning tar archive is **not** an input full-source archive. Attach this exact original source archive and the planning bundle (or at least the IP-01 prompt + five current ledger files). Branch/commit clean state is reported only; archive has no `.git`.

**Read:** `00_START_HERE.md` → `01_SOURCE_AUDIT.md` → `02_ARCHITECTURE_DECISION.md` → IP-01; `05_CONFORMANCE_AND_PERFORMANCE.md` for eventual acceptance. Start by independently validating SHA and source drift; do not assume future workspace equals the original archive.

**Critical details:** current optimized S1 CUE transport uses one FFmpeg decoder and sequential encoders; whole-group fallback is gated by any direct-plan ineligibility; a physical CUE group is one scheduled work unit; unrelated queue tasks already have global workers; Unix source-pass RG can avoid output reread, and Convert UI ReplayGain starts at **Both** despite pipeline generic default off. S3 precision, new-pipeline child process accounting, metadata/publication and net speed are **not yet qualified**. The real two-child `run_pipeline` is Unix-only in the baseline: deny S3 prelaunch on non-Unix. IP-03 must reuse shared process permits instead of building a general global FD/RAM/I/O manager. Before auto-promotion, use identical per-pair configuration hashes and at least one representative compressible/music album.

**Host gate amendment (2026-10-22):** IP-06's decision procedure now requires ReplayGain attribution before any rejection and carries a third `deferred` verdict promoting the PX-C source-pass observer. Architecture, packet IDs, scopes and dependencies are unchanged, and IP-01 is unaffected. See `08_HOST_GATE_AMENDMENT.md`.

**Acceptance discipline:** IP-01 implementation may be statically correct but will arrive uncompiled if the environment still lacks Rust. Mark `implemented-unqualified` until the receiving build host supplies compiled/native test evidence. Then accept and record the exact new archive hash; only then launch IP-02 using **that archive**, not this snapshot. If a prerequisite fails, preserve source and issue smallest separate corrective packet. Always update this file before ending the session.

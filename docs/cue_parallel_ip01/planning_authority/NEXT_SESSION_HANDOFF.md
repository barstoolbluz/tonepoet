# Durable next-session handoff — start IP-01

**Date:** 2026-10-22. **Completed:** R2-corrected planning documents, independent audit, six scoped execution-ready packets and conditional adapter outlines. **Not completed:** any production code edits, any production CUE runtime benchmark, new Rust build/qualification.

**Next:** execute [packets/IP-01_observe_baseline.md](packets/IP-01_observe_baseline.md) in one fresh GPT-6 session.

**Exact source input:** `TonePoet_SNAPSHOT_98aae2b_2026-10-22.tar.gz` with verified SHA-256 `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`. The planning tar archive is **not** an input full-source archive. Attach this exact original source archive and the planning bundle (or at least the IP-01 prompt + five current ledger files). Branch/commit clean state is reported only; archive has no `.git`.

**Read:** `00_START_HERE.md` → `01_SOURCE_AUDIT.md` → `02_ARCHITECTURE_DECISION.md` → IP-01; `05_CONFORMANCE_AND_PERFORMANCE.md` for eventual acceptance. Start by independently validating SHA and source drift; do not assume future workspace equals the original archive.

**Critical details:** current optimized S1 CUE transport uses one FFmpeg decoder and sequential encoders; whole-group fallback is gated by any direct-plan ineligibility; a physical CUE group is one scheduled work unit; unrelated queue tasks already have global workers; Unix source-pass RG can avoid output reread, and Convert UI ReplayGain starts at **Both** despite pipeline generic default off. S3 precision, new-pipeline child process accounting, metadata/publication and net speed are **not yet qualified**. The real two-child `run_pipeline` is Unix-only in the baseline: deny S3 prelaunch on non-Unix. IP-03 must reuse shared process permits instead of building a general global FD/RAM/I/O manager. Before auto-promotion, use identical per-pair configuration hashes and at least one representative compressible/music album.

**Host gate amendment (2026-10-22):** IP-06's decision procedure now requires ReplayGain attribution before any rejection and carries a third `deferred` verdict promoting the PX-C source-pass observer. Architecture, packet IDs, scopes and dependencies are unchanged, and IP-01 is unaffected. See `08_HOST_GATE_AMENDMENT.md`.

**Acceptance discipline:** IP-01 implementation may be statically correct but will arrive uncompiled if the environment still lacks Rust. Mark `implemented-unqualified` until the receiving build host supplies compiled/native test evidence. Then accept and record the exact new archive hash; only then launch IP-02 using **that archive**, not this snapshot. If a prerequisite fails, preserve source and issue smallest separate corrective packet. Always update this file before ending the session.

## D-01 verification correction (October 24, 2026)

A subsequent independent audit reproduced a false manifest mismatch when an
unprivileged user extracted the intact IP-03 archive with a normal `umask 022`.
The original manifest recorded group-write modes which `tar` masked. The
corrected `verify_static_ip03.py` retains complete content/type/symlink/digest
checks and owner-executable identity, checks no execute grant is added, and
normalizes only extraction-dependent mode bits when replaying `SOURCE_DIFF`.
The new six-case Python regression test and ordinary-extraction checks are
documented in `CORRECTIVE_D01_VERIFICATION.md`. No Rust production source,
existing supervisor, native probe, planning packet, or S3 activation changed.
Rust host qualification remains outstanding; the new corrective archive and
external receipt supersede the original `25052d48...` delivery identity.

---

# IP-03 persisted checkpoint — October 24, 2026

**Authoring run state:** Implementation and independent native resource envelope complete; Rust compile/format/integration tests blocked by absent toolchain. S3 remains feature disabled. This is a durable restart marker, not host acceptance.

## Completed

1. SHA-256 authenticated input `TonePoet_SNAPSHOT_d8c25d3_2026-10-24.tar.gz`, `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95`. Accepted-IP-02 identity letter supplied, no `.git` in archive.
2. Read actual IP-03 packet and amended planning authority, rechecked source access and S1 semantics; found existing Tokio family semaphores and supervised two-process `ToolRunner::run_pipeline`.
3. Implemented only in `track_executor.rs`: one shared global S3 concurrency slot, all-or-nothing immediate multi-family permit reservation, FFmpeg pair cost 2 / FFmpeg+SoX cost 1 each, direct guarded supervised pipeline invocation, error-vs-decline result; no production caller and no S1 changes.
4. Added ten named `cue_ip03` Rust unit/fake-runner/real supervisor tests in the same original module. **Not compiled or executed.**
5. Generated reproducible 8-second 44.1 kHz stereo FLAC input, independently piped real FFmpeg sample-range producer into real FFmpeg FLAC consumer for 16 ranges under one/two-pair caps. Verified every decoded PCM byte and source/output hashes; resource observations recorded in JSON. Python syntax checks PASS.
6. Updated current IP-03 cumulative ledgers without deleting historical IP-01/IP-02 evidence. Added static manifest/diff verifier, production patch and native probe harness; authoritative planning packet files unchanged.

## Known limits / next actions for a receiving host

- No `cargo`, `rustc`, or `rustfmt` was present in the authoring environment. Do not report PASS for Rust. Run `cargo fmt --all -- --check`, `cargo test --workspace --exclude tonepoet-true-peak cue_ip03 -- --nocapture`, `cue_stream`, `cue`, and full workspace suite on the exact archive. Count matched tests, keep full output.
- Review source-lifetime cancellation test on the actual host; a user-facing S3 activation remains explicitly prohibited. An aborted execution task after native spawn may have broader supervisor lifetime semantics than a normal token-driven cancellation; host should qualify the supervised child-reaping contract and avoid claiming absolute caps without it.
- Historical S1 segmented FFmpeg/FFmpeg counts one permit for two native children. The private S3 adapter accurately counts **its** two; legacy global live-child bound is not strictly guaranteed. Measure S1/S3 coexistence before acceptance; correct only if a concrete process/FD overrun justifies the throughput tradeoff.
- Actual IP-04 next source must come from exact external IP-03 delivery receipt after host qualification, not this file's historical packet naming template.
- IP-04 must couple IP-02's independently checked byte extent to the actual supervised streaming path, not treat success exit as proof of full source range.

## Resumption order

Read current `NEXT_SESSION_HANDOFF.md`, `VERIFICATION_RESULTS.md`, this checkpoint, `README.md`, `STATIC_AUDIT.md`, source patch and actual `SOURCE_DIFF.json`/`SOURCE_MANIFEST.json`. Verify external archive receipt SHA, source manifest, original IP-02 unchanged modules, then run host gates. On failure, use narrow IP-03 corrective changes and new manifest/archive/receipt; do not branch into unrelated scheduling architecture.

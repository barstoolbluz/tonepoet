# IP-04 D-01..D-03 corrective — CURRENT (2026-10-25)

**`implemented-unqualified` / NOT ACCEPTED / S3 OFF.** Corrections and exact test instructions: `CORRECTIVE_D01_D03_2026-10-25.md`. Recomputed cumulative manifest/diff/checksums in this archive; use the new *external* receipt for actual SHA256-bound archive identity. Retain accepted input SHA `fc3ea07b...` and previous IP-04 candidate `662379ddebf6...`. No Rust compiler/toolchain exists here; real Rust runner and all host validation remain **NOT RUN**. Qualify IP-04 on the Rust build host, do not activate or start IP-05.
---

# IP-04 persistent handoff — 2026-10-24

**State: IMPLEMENTED-UNQUALIFIED / NOT ACCEPTED / S3 OFF.** This cumulative source archive is an IP-04 candidate, not an authorized replacement for the accepted IP-03 source. No Rust host compiler was present, so no Rust test/fmt result can be asserted.

**Exact accepted input:** `TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz` / SHA-256 `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7`; user-supplied identity records `c1103be` clean, accepted IP-03. Preserve that archive and checksum as rollback. The original archive has no `.git` and contains 1348 git-archive entries including directories. All previous IP-01..IP-03 evidence and source changes remain in this cumulative tree; only IP-04 changes described here are new.

**Exact *candidate* next input:** The *separately provided* `TonePoet_CUE_IP04_DELIVERY_RECEIPT_<sha12>.md` identifies the finalized full-source `.tar.gz` by its actual name, complete archive SHA-256 and source manifest tree digest. A tar cannot contain its own SHA-256. Do not infer the hash from its filename without verifying `sha256sum`; do not feed IP-05 an unaccepted IP-04 candidate. If IP-04 host gate fails, use this cumulative source as the narrowly scoped correction base and issue a new receipt.

**Read order:** This file → `docs/cue_parallel_ip01/ip04_verification/README.md` → `VERIFICATION_RESULTS.md` → `STATIC_AUDIT.md` → `CONFORMANCE_MATRIX.md` → packet `planning_authority/packets/IP-04_parallel_worker_private.md` → IP-03 `ip03_verification/README.md` → source `cue_flac_range.rs`, `track_executor.rs`, `tool.rs`, `stages.rs`.

**New implementation:** A private group preflight/worker, shared IP03 admission guard with typed IP02 verifier, exact supervised 64KiB PCM pipe and faithful staged records. No normal scheduler call site, no user output routing change. Independent native probe PASS 42 tracks (12 x16-bit, 30 x24-bit), with exact reference PCM comparison, 4 max native children and 16 max parent FDs *in independent Python harness*, not Rust executor.

**Outstanding hard gates:** Run Rust fmt, named `cue_ip04`/`cue_ip03`/`cue_stream`/`cue` tests, 12/30 ignored regression and full workspace on qualified host as prescribed in `ip04_verification/VERIFICATION_RESULTS.md`. Inject cancellation/child failures/disk-full and measure actual Rust process/FD/RSS. Compare audio, metadata/tags, ReplayGain and logs against accepted S1 with matching settings. Prior locked-reference qualification expected to drift (`installed_reference_qualification_binds_the_current_locked_sources`), to be requalified only at agreed end of series; other failures require narrow correction.

**DO NOT:** activate S3 from CUE dispatch, silently retry S1 after a launched S3 failure, reopen IP-01/02/03 architecture, claim the independent simulation executes Rust, or begin IP-05 before owner-host acceptance.

**Reconstruction:** From source root run `python3 -B docs/cue_parallel_ip01/ip04_verification/verify_static_ip04.py`, optional `--baseline /path/to/accepted-ip03/tonepoet`, `sha256sum -c docs/cue_parallel_ip01/ip04_verification/SHA256SUMS` and the Rust host gates. `SOURCE_DIFF.json` preserves every changed/new path from input, `PRODUCTION_DIFF.patch` gives exactly four Rust deltas, `SOURCE_MANIFEST.json` binds content/modes/symlink targets. No .git or build caches inside output archive.

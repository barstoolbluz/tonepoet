# IP-03 D-01 — independent audit correction and verification

**Date:** 2026-10-24 (America/New_York). **Disposition:** source-verifier correction complete, Rust IP-03 still `implemented-unqualified`; S3 inactive and IP-04 blocked pending host acceptance.

## Trigger and constrained remedy

The original cumulative IP-03 archive (`25052d48edb6...`) passed verification only when exact group/other permission bits were preserved. Its `SOURCE_MANIFEST.json` includes modes such as `0o664`; a normal unprivileged `tar -xzf` under `umask 022` yields `0o644` without changing file bytes. The original `verify_static_ip03.py` incorrectly demanded strict equality of all raw modes. The independent audit reproduced this, and the corrective author independently reproduced `AssertionError: source manifest differs from current source` under unprivileged `umask 022`.

The **only functional fix** is inside `ip03_verification/verify_static_ip03.py`:

1. Preserve the archive's full, original `SOURCE_MANIFEST.json`, with its recorded raw modes and canonical `source_tree_sha256` digest; verify that digest against the **stored manifest**, not a mode-masked replacement.
2. Compare full SHA-256 file/content identity, file type, symlink target, and **owner-execute identity**. Reject newly added executable grants. Ignore raw group/other read/write bits and group/other execute bits removed by `umask 077`.
3. Normalize only the nonportable mode bits when comparing a newly extracted source and baseline against the original raw-mode `SOURCE_DIFF.json` receipt. The archived diff and manifest retain their original mode evidence.
4. Preserve the complete SHA256SUMS check, including the manifest file itself, the original production-diff gate, the 16 native range evidence checks, and the ten named Rust-test identity checks.

**Limit of mode checks:** group/other executable bits removed by `umask 077` cannot be distinguished from a later removal; the owning user's execute bit *can* and is checked. The integrity authority for the complete archive is its externally published SHA-256, not extraction-dependent read/write bits. No repository-wide permission changes were made.

## Focused verification and continuity

- Six stdlib-only tests in `test_verify_static_ip03.py` cover 022/077 masking, changed content hashes, lost owner-executable permission, unexpected executable grants, symlink type/target/hash, inventory changes, and permission-insensitive delta comparison.
- The delivery receipt records the clean **unprivileged**, ordinary `tar -xzf` extraction checks for `umask 022` and `077`, including verification with `--baseline` against an independently extracted original snapshot, and mutation/refusal tests (content, executable, symlink, stored digest and checksum list).
- `PRODUCTION_DIFF.patch` and `src/convert/pipeline/track_executor.rs` are **byte-for-byte unchanged** from the audited IP-03 R1 archive. The accepted IP-02 code and authoritative planning packet are not edited; no S3 call site was enabled. The original IP-01 15/15, native FFmpeg 16/16, and patch-application evidence remain applicable, but do **not** establish Rust correctness.
- This correction has **not** run `cargo fmt`, Rust tests, workspace tests, or real Rust supervisor reaping checks. The previous Reference qualification drift in `stages.rs`/`replaygain.rs` is pre-existing and unrelated.

Read the new **external corrective delivery receipt** for the exact new tarball name, length, SHA-256, build-host commands, and final verification log. The old archive identity `25052d48edb6...` is superseded, not rewritten; IP-04 remains blocked until host qualification and human acceptance.

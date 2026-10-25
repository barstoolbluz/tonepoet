# IP-03 — Shared two-child CUE S3 admission (private, feature disabled)

**State:** `implemented-unqualified` (October 24, 2026). No activation of S3, no accepted IP-03 host signoff. This evidence is additive to the original `planning_authority/` and retained IP-01/IP-02 evidence.

- **Verified input:** `TonePoet_SNAPSHOT_d8c25d3_2026-10-24.tar.gz`, SHA-256 `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95` (origin letter reports accepted IP-02 commit `d8c25d3`; archive contains no `.git`).
- **Only changed original production file:** `src/convert/pipeline/track_executor.rs`. Introduces an unused S3 helper and one shared in-flight semaphore in existing `ToolConcurrencyLimits`. Original CUE planner, metadata, exact range helper, native ReplayGain, tool runner, legacy segmented S1 route, user interface, publisher and scheduler remain byte-identical.
- **Semantics:** A feature-off S3 caller can try an FFmpeg/FFmpeg pair (two FFmpeg tokens) or an FFmpeg/SoX pair (one each). It reserves a bounded global S3 slot and full family permit set immediately or declines before spawn. No waiting on a second token; partial permits release on failure. Cancellation/closed semaphore/invalid family are errors; an attempted child execution is always returned as `Executed(Result)` and never constitutes pre-spawn fallback. The guard lives over a single direct call to `ToolRunner::run_pipeline`.
- **Bound:** `N = min(2, max(1, floor(ffmpeg_family_limit/2)))`; for N admitted S3 workers, at most `2N` S3 tool children plus `N` bounded OS pipes and `N` decoder source readers. FFmpeg/FFmpeg consumes `2N` existing family permits; FFmpeg/SoX consumes `N` from each. Existing ToolRunner records have fixed 64 KiB tails; this adapter stages no PCM array and creates no extra pipes. The native independent Linux probe measured a 65,536-byte pipe; actual capacity varies by host. FD/RSS high-water figures remain host-specific and need the real TonePoet runtime gate.
- **Historical S1 accounting limitation:** The existing segmented pipeline counts distinct families, so a two-FFmpeg S1 transport counts one FFmpeg permit. IP-03 does not change that live system and **cannot** claim a strict global live-native-child cap across arbitrary existing S1 overlap. `cue_ip03_two_albums_share_cap_and_do_not_starve_ordinary_tools` exercises this contract explicitly. The host must check whether real simultaneous S1/S3/ordinary work exceeds practical measured process/FD headroom; avoid retrofitting the general scheduler without data.

## Reproduce the independent checks

Extract the full archive normally, as an unprivileged user. Both `umask 022`
and `umask 077` are supported; `tar -xzpf` and root ownership are **not**
required. From the extracted `tonepoet/` root:

```sh
cd docs/cue_parallel_ip01/planning_authority && sha256sum -c SHA256SUMS && cd ../../..
python3 -B docs/cue_parallel_ip01/ip03_verification/test_verify_static_ip03.py
python3 -B docs/cue_parallel_ip01/ip03_verification/verify_static_ip03.py
# For the full source-delta gate, unpack the exact input separately and add:
python3 -B docs/cue_parallel_ip01/ip03_verification/verify_static_ip03.py --baseline /path/to/input/tonepoet
# Optional new native envelope data (generates temp audio only outside source):
python3 -B docs/cue_parallel_ip01/ip03_verification/native_pair_envelope_probe.py --output /tmp/ip03-native-results.json
```

On the build host (not executed here):

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_ip03 -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

**Do not count zero matching tests as PASS.** The ten named IP-03 Rust tests include the shared permit fake gate, two albums, one/two FFmpeg hosts, 200 admissions, failure/cancellation/panic RAII release, real supervised FFmpeg/SoX early exit and cancellation, and slow FFmpeg/FFmpeg transport. Rust/fmt/actual supervisor tests could not be run in the authoring environment: no Rust toolchain. Native FFmpeg pair probe is deliberately independent from Rust and does not establish supervisor correctness.

`SOURCE_MANIFEST.json` and `SHA256SUMS` cover the current source tree (both self-hash files excluded from the manifest, but the manifest itself included in SHA256SUMS). The manifest retains the original archived mode bits and its digest authenticates those bytes; the verifier compares content hashes, entry types, symlink targets and owner-executable identity, rejecting unexpected execute grants while tolerating group/other bits stripped by `umask`. Group/other execute bits cannot be required after a `umask 077` extraction and are not accepted as a source-integrity gate. The optional `--baseline` comparison normalizes those nonportable mode differences for both the current tree and original raw-mode `SOURCE_DIFF.json` receipt. `SOURCE_DIFF.json` continues to preserve the actual archived source identities, including symlinks and raw modes. The D-01 correction is recorded in `CORRECTIVE_D01_VERIFICATION.md`. `PRODUCTION_DIFF.patch` is the exact Rust patch (independent apply/re-apply results in `PATCH_CHECK.txt`). The original 15 IP-01 offline tests still PASS (`LEGACY_IP01_PYTHON_TEST_OUTPUT.txt`). `native_pair_probe_results.json` gives binary and audio SHA-256s, complete range hashes, measured FD/RSS/process/pipe observations, and native commands reconstructed in `native_pair_envelope_probe.py`.

## IP-04 dependency

IP-04 must reuse this private `run_cue_s3_pipeline_if_admitted` wrapper to avoid bypassing or reacquiring tokens, and must integrate the IP-02 exact streamed byte-count verifier into the supervised producer→consumer boundary before treating output as valid. The present `ToolRunner::run_pipeline` does not on its own check that the decoder emitted exactly `(end-start) × channels × 4` PCM bytes. Any failure after a child starts must be surfaced under the existing `FailurePolicy`, never retried as S1. Preserve S1 as rollback and hold S3 user admission until IP-05/IP-06.

**Archive identity is external:** the separately delivered `TonePoet_CUE_IP03_DELIVERY_RECEIPT_<SHA12>.md` contains the exact cumulative tar name and SHA. A file cannot contain its own verified archive digest without changing it; this receipt binds the exact IP-04 input after host acceptance.

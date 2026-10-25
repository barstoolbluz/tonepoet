# IP-04 D-01..D-03 corrective verification — October 25, 2026

**Implemented-unqualified / S3 OFF / NOT ACCEPTED.** `sha256sum` of both original archives: PASS. Native exact gapped-reference PCM: **3/3 PASS (native only)**, see `native_gap_reference_results.json`. Existing IP-03 identity regression: **6/6 PASS**. D-01 u16 assertion, D-02 causal early consumer code-7/stderr + command-less primary peer retention, and D-03 gated 3,1,2 fake runner/negative controls are **authored but NOT COMPILED OR EXECUTED**; `cargo`, `rustc`, `rustfmt` are unavailable here. Previous 6/42 native reference checks remain historical. Run all named Rust tests and mandatory full host gate listed in `CORRECTIVE_D01_D03_2026-10-25.md` before approval. No full-suite, FD/RSS, real S3 parity, or speed claims.
---

# IP-04 verification results — 2026-10-24

**Result: implemented-unqualified (BUILD/EXECUTION NOT RUN).** Authoring runtime: Debian 13 Linux, FFmpeg 7.1.5, native FLAC available, **no cargo/rustc/rustfmt**. Host user reports 7,611 prior tests passed / 1 current locked-reference qualification drift failure on accepted IP-03 input (not a new IP-04 run).

| Gate | Status | Observed result |
|---|---|---|
| SHA-256 `sha256sum TonePoet_SNAPSHOT_c1103be_2026-10-24.tar.gz` | **PASS** | `fc3ea07b6a8ffac0b21f366b50e01693ee6b0b626a89749f551f964bcda815d7` |
| Archive safe extract; compare input with new source | **PASS** | Verified git-less tar and delta; manifest and diff receipt accompany bundle |
| `python3 -B docs/cue_parallel_ip01/ip03_verification/test_verify_static_ip03.py` | **PASS** | 6 original integrity tests |
| Independent native harness `--tracks16 3 --tracks24 3` | **PASS** | 6 ranged PCM comparisons, 2 concurrently running decoder/encoders |
| Independent native harness `--tracks16 12 --tracks24 30` | **PASS** | 42 ranged PCM comparisons, decoded PCM identical byte-for-byte to full-image reference |
| Native harness process/descriptor observations | **PASS (native only)** | 4 peak native FFmpeg children, 16 maximum parent FDs across full cases, no extracted PCM files; not TonePoet runtime |
| Current IP-04 static manifest, original-delta and scope | **PASS** | `verify_static_ip04.py` plus SHA256SUMS, see output in `STATIC_GATE_OUTPUT.txt` |
| Re-extract delivered archive / check checksums | **PASS** | See `PACKAGE_CHECK.txt` after output archive is sealed |
| Historical IP-03 `verify_static_ip03.py` against new IP-04 tree | **EXPECTED FAIL** | Old IP-03 manifest refers to the pre-IP-04 source; preserve it as historical, not a current gate |
| Rustfmt and Rust tests | **NOT RUN** | `cargo`, `rustc`, `rustfmt` not available; no claim of compile/test pass |
| Actual TonePoet private 3/12/30-track S3 + failure tests | **NOT RUN** | Tests authored, unqualified |
| S1 vs S3 measured performance / full media parity | **NOT RUN** | Later host gate + IP-05/IP-06 |

Native results in `native_quick_results.json` and `native_full_results.json` include individual source-reference PCM SHA-256 hashes and stage artifact SHA-256 hashes. The Python harness writes fixtures to temporary directories and deletes them; see `native_flac_pairs.py` for independent reproduction.

## Required qualified-host gate

From **this exact re-extracted source** and qualified native FFmpeg/FLAC environment, preserve stdout/stderr, tool versions, exact test counts, fixture hashes and any failures:

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_ip04 -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip03 -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip04_private_real_twelve_and_thirty_tracks_bounded -- --ignored --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

**Count actual matching tests. A zero-test result is not a pass.** Specifically run `cue_ip04_supervised_exact_pipe_rejects_short_and_surplus_and_proves_success`, `cue_ip04_supervised_exact_pipe_reaps_both_on_consumer_error_and_cancel`, `cue_ip04_supervised_exact_pipe_preserves_tool_failures_and_stderr_load`, `cue_ip04_private_real_three_tracks_16_and_24_exact_pcm`, `cue_ip04_private_real_twelve_and_thirty_tracks_bounded` (ignored by default), `cue_ip04_private_reorders_three_two_one_completion_by_source_identity`, and `cue_ip04_private_preflight_refuses_unqualified_source_without_spawning`.

Host also needs to inject slow consumer, nonzero decoder, early encoder exit, disk-full/staging failure, simultaneous private albums, 200-member load and cancellation with two children active; check native PID/FD/RSS high-water and no orphan/partial final-visible output. The new authored tests cover most process/EOF failure mechanisms; do not elevate remaining unobserved stress cases to PASS without executing them. Compare exactly decoded PCM, frames, metadata/tags, ReplayGain policy and logs against the accepted S1 path. The pre-existing `installed_reference_qualification_binds_the_current_locked_sources` drift is *separately known*; do not rewrite the lock mid-series merely to make the gate green.

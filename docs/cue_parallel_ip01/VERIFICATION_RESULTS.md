# IP-04 verification checkpoint — 2026-10-24

Exact input SHA-256 confirmed; IP-03 static-unit helper 6/6 PASS; independent 16-/24-bit FFmpeg range probe 42/42 exact PCM PASS, bounded 4 native child high-water (Python, not Rust); no Rust compiler and therefore **NO RUST FORMAT, BUILD OR TEST RESULTS**. Old IP-03 source-manifest verifier is expected to fail on this new tree and is not the IP-04 gate. `ip04_verification/VERIFICATION_RESULTS.md` documents command-by-command results and mandatory owner-host gates. **State implemented-unqualified; S1 remains live.**

---

# IP-03 verification current state — 2026-10-24

**Status:** `implemented-unqualified`; NO Rust compile/fmt/host supervised process qualification. Source identity `d8c25d3` supplied and input SHA matched. Full archive checksum and manifest are in `ip03_verification` and the separately distributed external delivery receipt.

| Executed check | Outcome | Notes |
| --- | --- | --- |
| `sha256sum TonePoet_SNAPSHOT_d8c25d3_2026-10-24.tar.gz` | **PASS** | `fa51a09ad0c71fe17564053c8fd309f38bb5592730bb80fbac29dcc9e519fd95` |
| `cd docs/cue_parallel_ip01/planning_authority && sha256sum -c SHA256SUMS` | **PASS** | All original authoritative planning files byte-identical |
| `python3 -m py_compile docs/cue_parallel_ip01/ip03_verification/native_pair_envelope_probe.py` | **PASS** | Python syntax only; cache removed from archive |
| `cd docs/cue_parallel_ip01/tools && python3 -B -m unittest -v test_trace_summary.py` | **PASS, 15/15** | Preserved IP-01 offline trace/ReplayGain analysis tests; `ip03_verification/LEGACY_IP01_PYTHON_TEST_OUTPUT.txt` |
| `patch --dry-run --batch --forward -p1` on pristine d8c25d3 + independent apply/re-apply | **PASS** | Six hunks apply; second forward application refuses all; patched byte SHA exactly matches authoring source (see `PATCH_CHECK.txt`) |
| `python3 docs/cue_parallel_ip01/ip03_verification/native_pair_envelope_probe.py --output docs/cue_parallel_ip01/ip03_verification/native_pair_probe_results.json` | **PASS** | 16 independent real FFmpeg decoder→encoder ranges, reconstructed sample bytes identical; 1/2-worker resource observations in JSON |
| `python3 -B docs/cue_parallel_ip01/ip03_verification/verify_static_ip03.py --baseline ...` | **PASS when delivered** | Exact unchanged-production audit and full manifest check (not Rust compilation) |
| `cargo fmt --all -- --check` | **NOT RUN** | `cargo`, `rustc` and `rustfmt` missing in authoring environment |
| `cargo test --workspace --exclude tonepoet-true-peak cue_ip03 -- --nocapture` | **NOT RUN** | 10 newly authored named Rust tests; nonzero execution required |
| `cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture` | **NOT RUN** | Original path regressions |
| `cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture` | **NOT RUN** | Existing album/cue tests |
| `cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak` | **NOT RUN** | Preexisting known Reference qualification gate expected unresolved; record actual result, never silently waive |
| Real TonePoet S3 processing of 2 albums, 200 tracks, native process peak/RSS/FD under cancellation/failure | **NOT RUN** | S3 intentionally disabled; real private host test at minimum, eventual integrated 200-track test belongs IP-04/05 |

**Native envelope details:** 1 pair: 2 children, 15 sampled FDs (parent + children), 168,840 KiB sampled combined RSS. 2 pairs: 4 children, 26 sampled FDs, 232,944 KiB sampled combined RSS. Both observed kernel pipe capacity 65,536 bytes. These metrics were collected under Python orchestration on this Linux host and are **not** TonePoet process measurements or guaranteed maxima; they justify choosing a conservative S3 cap of 2 pending build-host checks. The native fixture includes SHA-256 for input raw PCM, encoded FLAC, whole decoded reference, each ranged PCM and each encoded/decoded track. Sample geometry: 44.1 kHz, stereo, 16-bit, 8 seconds. Measurements are host/version specific, not universal audio qualification.

**Before acceptance on the build host:** extract exact delivered tar, verify `SOURCE_MANIFEST.json` and SHA256SUMS and run `verify_static_ip03.py`. Run all Rust commands above and record named counts/exit status/stdout/stderr. Trigger real supervisor cancellation while both children are live; prove the execution guard isn't dropped before reaping and no child leaks. Measure active children, FD/RSS high-water with at least two independent albums alongside a real non-S3 conversion, including one- and two-FFmpeg hosts. Run slow input/output, encoder early exit and a 200-track admission stress. Evaluate the known legacy segmented FFmpeg/FFmpeg undercount against measured live-child headroom; no silent generic-limiter rewrite. Confirm diagnostic-disabled parity and S1 source-pass ReplayGain unchanged. Do not treat the host's known installed Reference qualification drift as success.

---

## Preserved IP-01 historical verification

# IP-01 R2 corrective verification — 2026-10-22 America/New_York / 2026-10-23 UTC

**Status: IMPLEMENTED-UNQUALIFIED; NOT ACCEPTED.** Exact archive SHA-256 and file manifest are in the separately delivered `TonePoet_CUE_IP01_R2_DELIVERY_RECEIPT_<SHA12>.md` and internal `SOURCE_MANIFEST.json`. The earlier R1 archive `4c5626722df94860e49127ff7675f533eeb77bc44cede7711a922e63db90ec99` was rejected by the independent audit. No S3 or conditional source-pass observer landed.

## Executed offline gates

| Command / evidence | Result | Scope |
|---|---|---|
| SHA-256 of original and amended plan archives | PASS | Exact original source SHA `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`; plan SHA `b64b05c23afb0accf8679a26e91f5306ba1a8e6b8f8e8e654b60e2d077f96d47` |
| `python3 -B docs/cue_parallel_ip01/tools/verify_static_ip01.py --source <R2> --input-snapshot <original> --plan <amended unpacked plan>` | **PASS**; see `STATIC_GATE_OUTPUT.txt` | Exactly three original Rust modules differ, no amendment drift, expected R2 source anchors. **Not a Rust parser or test execution** |
| `cd docs/cue_parallel_ip01/tools && python3 -B -m unittest -v test_trace_summary.py` | **PASS, 15/15**; see `PYTHON_TEST_OUTPUT.txt` | Executed D-01 tag-failure/partial and skipped state, D-02 partial/foreign/two-CUE/nonfinite scan, D-03 real group vs observer recovery, D-04 deferred-before-admit/cancel, event ordering, overlapping timing and CLI rejection |
| `python3 -B -m compileall -q docs/cue_parallel_ip01/tools` | **PASS**; see `PYTHON_COMPILE_OUTPUT.txt` | Syntax of offline Python tools only; generated caches removed before delivery |
| `cd docs/cue_parallel_ip01/planning_authority && sha256sum -c SHA256SUMS` | **PASS**; see `PLANNING_SHA256_OUTPUT.txt` | Byte-identical authoritative amended plan, including IP-06 decision gate |
| Cumulative source SHA256SUMS and manifest, full clean extraction | See final externally bound delivery receipt | SHA verification of the **delivered R2 archive**, not just the authoring tree |
| `PRODUCTION_DIFF.patch` and `CORRECTIVE_DIFF_R1_TO_R2.patch` byte-for-byte roundtrip | See final externally bound delivery receipt | Patches apply to baseline and R1 respectively; every intended Rust module verified |

## Rust qualification — NOT RUN on authoring host

Neither `cargo`, `rustc` nor `rustfmt` is installed here. No claim of Rust compilation, formatting, link resolution, native tool execution or parity is made. On the exact delivered corrective source run at least:

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_ip01_ -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

For the added `cue_ip01_` tests, explicitly record nonzero executed counts and all subprocess child failures/stdout/stderr. In particular run and inspect:

- `cue_ip01_real_enabled_disabled_sink_and_concurrent_identity` — real fresh-process sink On/Off and concurrent correlation, with no private paths.
- `cue_ip01_real_queue_deferred_event_before_admission` — saturated pool and *actual* queue-deferred file record, no spurious admission.
- `cue_ip01_real_s1_and_fallback_diagnostic_routes_on_isolated_sink` — preexisting true S1 failure and prelaunch fallback execution under isolated real JSONL sinks.
- `cue_ip01_source_tag_application_outcome_requires_settled_writer` — real ReplayGain manifest refusal, missing metadata writer path, settled outcome classification.
- `cue_ip01_eligible_cue_source_and_off_or_lossy_controls` — source-pass eligibility vs Off and lossy output.
- `cue_ip01_noncontiguous_authored_ordinals_have_stable_group_identity` — ordinals 2/5/9 rather than position 0/1/2.

The child helpers are actual Rust tests that return without side effects in normal test runs; their parent tests launch them in isolated test-harness subprocesses with deterministic diagnostic environment. **None was executed in this authoring environment.** A zero-test filter is not a PASS.

## Native gate — NOT RUN; mandatory before IP-01 acceptance

Run real FLAC+CUE→FLAC under RG Off and Both in fresh processes with default diagnostics disabled and then opt-in enabled, comparing the original snapshot against R2. Preserve canonical decoded PCM hashes, sample counts, metadata/artwork/ReplayGain tags, publication and cancellation behavior, conversion.log and tool/output receipts. Drive actual source-pass eligible/ineligible, post-output native scan, source observer causal failure/recovery, failed tag write, S1/fallback, worker/encoder/decoder failure, cancellation before/after admission, and multiple concurrent albums. Verify opt-in overhead and race-safe complete JSONL; tests of a fixture generator or Python analyzer cannot substitute for this.

For failed/native paths check exact stage Result, album publication, terminal and trace correlation; do not promote selected source-pass to applied tag status without `replaygain_application_settled`. `native_replaygain_measure_ms` may be `NOT INSTRUMENTED` on legacy uncorrelated logs. Do not add child/observer inclusive intervals together. Actual TUI acceptance→visible repaint must be measured externally. Physical performance trials are **BENCHMARK NOT RUN** and both trial rows remain placeholders.

**H-01:** The amended IP-06 evaluator's cross-RG paired configuration digest is not modified under IP-01; must be corrected/qualified before using IP-06 for promotion. The conditional PX-C source-pass observer remains **NOT IMPLEMENTED**.

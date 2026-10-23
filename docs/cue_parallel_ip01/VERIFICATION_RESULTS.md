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

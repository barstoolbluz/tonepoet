# BRIEF — HDCD/CUE R2: one plan test fails, and five warnings

Applied. Gate: **7516 passed / 2 failed**. One failure is the expected
source-lock freshness, since `plan_bridge.rs`, `stages.rs`,
`track_executor.rs` and `semantic_plan.rs` all moved; requalification follows
once the tree is otherwise clean.

The decode-before-cut ordering is the right correction and the physical
witness is a good one — the 390-sample difference on the old route is what
makes the comparison non-vacuous. Named suites pass: `hdcd` 47 of 47,
`cue_stream_admission` and `grouped_cue_fallback` 1 of 1 each,
`hdcd_true_peak` 3 of 3.

## 1. A plan combining HDCD with certified gain selects no PCM terminal

```
semantic_plan::tests::hdcd_certified_gain_four_policies_measure_the_decoded_float64_signal
  plan must select a PCM terminal
```

2 of 2 alone. The four certified gain policies over an HDCD source no longer
resolve a terminal.

**What we want:** HDCD composed with each certified gain policy resolves a PCM
terminal, as the test requires — or, where a combination is legitimately
refused, the refusal is explicit and the test says which and why.

## 2. It did not compile

Two errors, both fixed here.

`src/tui/command.rs` uses `hdcd_decode_enabled` in
`execute_commit_with_source_options_transform`, but the binding that sat beside
`hdcd_path_states` is gone while the use remains.

`src/convert/processor.rs` cannot construct
`CertifiedTruePeakExecutionContract`: `plan_bridge.rs` gained a `pre_resample`
field and this constructor was not updated. `processor.rs` is not in the
changed-file list, so a struct grew a field and a caller in an untouched file
was missed.

## 3. Five compilation warnings

`tonepoet-pipeline` emits five `missing_docs` warnings, all from this round's
HDCD work in `semantic_plan.rs`: the `source_size_bytes` and
`source_modified_ns` fields of `RegisteredUnaryEffect::HdcdDecode`, and all
three fields of `HdcdCueSegment`. The crate lints on missing documentation and
the neighbouring `cue_segments` field is already documented.

**What we want:** a clean build with no warnings.

## Still needing the user

The five R5 UI changes have never been looked at. #75's SACD tag-transfer round
trip has never run against a real disc. #62's real album, the ten-track one
with the 2.19 GB RF64 file, has not been opened since the read path landed.

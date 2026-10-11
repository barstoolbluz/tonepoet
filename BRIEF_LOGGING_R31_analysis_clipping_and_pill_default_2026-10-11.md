# BRIEF — R30C is accepted bar one test constant; and the analysis calls 0 dBFS clipping

R30C is applied and gated. All three R30 items work: the ceiling shows two
decimals, the `custom` pill opens an in-row editor, and unsigned input commits
as headroom. One test fails, and a separate defect is described in item 2.

This is how it reads to us from the outside. We misapprehend this codebase
routinely; where we are wrong, trust your own reading. The measurements below
we ran ourselves; the field report opening item 2 is the user's own
observation, and the mechanism we offer for it is a guess.

## What is verified

Gate: **7434 passed / 2 failed**. Compile clean, no unused warnings, and the
`missing_docs` warnings are gone.

```
r30_pcm_and_dsd_custom_pills_have_real_hit_targets_and_inline_inputs
installed_reference_qualification_binds_the_current_locked_sources
```

The second is expected; `stages.rs` and `settings.rs` moved again. The three
named acceptance tests pass, and `r30_` is 9 of 10.

## 1. One test expects a default the product does not have

`r30_pcm_and_dsd_custom_pills_have_real_hit_targets_and_inline_inputs` asserts
the row renders `-1.00 dBTP`. It renders `-0.10 dBTP`.

`PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP` is `DbNano(-100_000_000)` and feeds both the
PCM and DSD rows. R30 did not change it, so `-0.10` looks correct and is the
two-decimal default that was asked for. We think the `-1` came from the sample
row quoted in the last brief, which was not a default; that ambiguity was ours.

Changing that one expectation to `-0.10` makes the test pass whole — pill hit
region, its width, the click landing, the editor opening, and the pill
remaining visible during editing all hold. We reverted that edit; the tree is
as delivered.

## 2. The analysis reports clipping on signals that reach 0 dBFS

Observed in the field, and the whole of what is known for certain: with the
true-peak margin set to 0.0000, 0.001, 0.0001 or 0.00001, the analysis reports
some number of clipped samples. Our guess is that each of those margins places
peaks against full scale, where the counter below flags them, which would make
the margin's resolution irrelevant to the report. We have not confirmed that.

**What we want:** content that reaches 0 dBFS without exceeding it is not
reported as clipped. Analysis time should not appreciably increase.

The clipping count in `src/tui/analyze.rs` is a sample-domain test against a
single constant, `abs_val >= 0.9999695`, which is -0.000265 dBFS. `analyze_file`
takes a path and a sample range and nothing else, so the margin is not an input
to it. We measured the counter on 22,050 full-scale 16-bit samples per case:

| 16-bit content | clipping_count | peak_db |
| --- | --- | --- |
| positive full scale, 32767 | 0 | -0.000265 |
| negative full scale, -32768 | 22050 | +0.000000 |
| one code below, 32766 | 0 | -0.000530 |
| about -0.1 dBFS, 32390 | 0 | -0.101 |

i16 is normalised by 32768, so positive full scale is 0.9999694824 and sits
1.76e-08 below the threshold while negative full scale is exactly 1.0. The
count is therefore sign-asymmetric at 16-bit: negative full-scale samples are
counted and positive ones are not.

The constant is also depth-blind. The band it calls clipped spans about one
code at 16-bit, 256 at 24-bit and 65,500 at 32-bit, so the number means
something different at each depth.

Related, and possibly the same work: `AnalysisResult` carries
`true_peak_dbtp`, the database has the column, and the overlay renders it when
present, but both production constructors set `None` and nothing sets `Some`.
So inter-sample peaks — the only thing that can exceed 0 dBFS in integer PCM —
are not currently observed. Whether true peak is the right basis for this, or a
heuristic over runs of consecutive full-scale samples, or something else, is
open.

We do not believe this implementation has been examined in an earlier round.
Its rigor, precision and performance are worth your own assessment beyond the
defect above.

## Requalification

`stages.rs` and `tonepoet-pipeline/src/settings.rs` are both source-locked and
both moved. `R30_ACCEPTANCE.sh` wraps the focused tests,
`scripts/requalify_reference_r20.sh` unchanged, the workspace gate, release
builds and a positive qualified Reference conversion; we have not run it, since
item 1 fails first. `src/tui/analyze.rs` is not source-locked.

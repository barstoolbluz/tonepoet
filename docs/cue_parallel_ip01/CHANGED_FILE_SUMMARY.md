# IP-01 R2 corrective changed-file summary

Original source SHA-256: `0740a9c8cbd8e59648981207daf62e6373f4b544ad3a40c7872b3890c9d76d26`.
Rejected R1 source SHA-256: `4c5626722df94860e49127ff7675f533eeb77bc44cede7711a922e63db90ec99`.

## Original source files changed (3)

- `src/convert/pipeline/baseline.rs` — diagnostic-only R2 correction, regression tests; patch in `PRODUCTION_DIFF.patch`
- `src/convert/pipeline/stages.rs` — diagnostic-only R2 correction, regression tests; patch in `PRODUCTION_DIFF.patch`
- `src/convert/processor.rs` — diagnostic-only R2 correction, regression tests; patch in `PRODUCTION_DIFF.patch`

No other originally present production source files changed. The original source's `.git` directory was absent, so file ancestry beyond these bytes cannot be proved.

## IP-01 additive documentation, tests and handoff

All 53 files added to the original source reside under `docs/cue_parallel_ip01/`; no separate production or test crate introduced. R2 modifies the existing offline summarizer, Python tests, static checker, source manifest, verification and benchmark artifacts, and adds the independent audit adjudication plus a R1→R2 corrective patch.

**Corrections:** D-01 source-pass tag-success settlement; D-02 exact native scan correlation; D-03 group versus retry semantics; D-04 deferral reasons; D-05 real sink/route/queue Rust test bodies; D-06 authored track ordinal. H-01 remains the future IP-06 evaluator obligation. The normative plan copy is byte-identical.

**Patch roundtrips:** `PRODUCTION_DIFF.patch` is the complete cumulative source delta from original 98aae2b; `CORRECTIVE_DIFF_R1_TO_R2.patch` is the three-module delta from rejected R1. Both are exact byte reproductions when applied to their stated input.

**Qualification:** static audit and offline Python tests executed; Rust and native FLAC+CUE end-to-end tests **NOT RUN**. No S3, PX-C, new ReplayGain engine, parser, scheduler or publication changes.

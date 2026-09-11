# Offline qualification and selective Fast066 commissioning

The source-round scripts independently re-derive the frozen mathematical
artifacts used by HQ1024V1 and the selective native-input Fast066V2 screen.
They are source evidence, not a substitute for compiling and executing Rust.

Run the offline source round with:

```text
python3 -B qualification/run_offline_qualification.py
```

The runner:

1. regenerates HQ1024 coefficients and requires byte identity with the
   checked-in Rust/JSON artifacts;
2. regenerates the qualified 2x prefix and requires byte identity;
3. verifies the fixed-radix-2 prefix enclosure, packed-channel scaling,
   same-graph AVX source shape, and DAZ/FTZ-safe classification;
4. independently verifies the existing certified-search coefficient bounds;
5. regenerates the local Fast HQ4 metadata and requires byte identity;
6. independently composes all 1,025 native HQ1024 phases, regenerates the
   `A_raw`/`B_raw` screen metadata byte-for-byte, and runs the exact raw-screen
   summation-by-parts/range/outward-rounding tests;
7. source-audits the selective Fast graph, frozen public/configuration markers,
   absence of the retired quota policy, and required test hooks; and
8. unit-tests the commissioning driver's duration-normalized stop/go rule,
   benchmark identity validation, result parser, and A/B/C/D orchestration.

The source audit in step 7 intentionally does **not** claim that Rust tests ran.
The Rust suite is the behavioral authority for buffer indexing, direct/FFT
numerical enclosure, raw rejection against the independent finite target,
streaming state transitions, certificate coverage, and exceptional inputs.

`FAST_INPUT_SCREEN_SPECIFICATION.md` is the implementation specification used
for this revision. `raw_screen_metadata.json` is the checked-in exact metadata
artifact. `screening_probe.py` is a workload-density probe only; its survivor
counts are not wall-time measurements.

## A/B/C/D commissioning

`commission_fast066_v2.py` remains an external commissioning driver and adds no
production API. It requires the pinned Fast066V1 comparison archive and the
operator's carrier. The source declarations for `SurveyBoundsOnly` and
`NominationOnly` are frozen. Because the retired nomination stage no longer
exists, both V2 ablations now stop after native screening, selective HQ4 survey,
and local flat bounds. The benchmark reports `fast-nominate` as
`survey-bounds-only-compat`; C therefore repeats B under the compatibility name
rather than pretending a nomination stage executed.

Example:

```text
python3 -B qualification/commission_fast066_v2.py \
  --v1-archive /path/to/tonepoet-fast066-v1-implementation-corrected-2026-09-09.tar.gz \
  --carrier /path/to/carrier.f64le \
  --sample-rate 192000 \
  --channels 2 \
  --expected-point-dbtp <independently-checked-HQ-target> \
  --rustflags '<operator release flags>' \
  --output /path/to/fast066-commissioning.json
```

The driver runs A (V1), B (V2 bounds ablation), C (the equivalent frozen
compatibility ablation), D-instrumented, and D-binding. C remains the strict
pre-D wall stop/go measurement for continuity with the existing commissioning
workflow. D-binding is the uninstrumented shipping measurement. Release requires
the 0.66 s/programme-minute wall gate, the Fast per-channel 0.01 dB
certificate-width gate, and (when an independent target is supplied) a 0.01 dB
point-accuracy gate; exact silence `[0,0]` is accepted without inventing a
logarithmic width.

The driver enforces the target-directory separation itself. Its machine-readable
manifest records the pinned V1 archive digest, V1/V2 source-tree identities,
carrier digest and geometry, edge policy, `rustc -vV`, Cargo version, operator
codegen environment, parsed vectorizer setting, and the actual crate/example
`rustc` command lines emitted by fresh `cargo build -vv` prebuilds. Those hashes,
compiler queries, and builds occur outside every benchmark's internal measured
wall interval. Each A/B/C/D record points to its source and build identity and
records the active SIMD backend, individual elapsed time, raw certificate/error
statistics, and explicit V2 work counters. If C is at or above 0.66
s/programme-minute, the manifest records `profile_mandatory_path: true` and D is
neither built nor run for performance commissioning.

Do not present an instrumented run, a screening-probe work fraction, or an
operation-count estimate as a shipping speed result.

`fast066_numerical_probe_report.json` is inherited pre-redesign evidence from the
frozen baseline. The selective implementation does not regenerate or use it as
release evidence; the current authorities are the raw-screen derivation/tests,
Rust regressions, and target-machine commissioning described above.

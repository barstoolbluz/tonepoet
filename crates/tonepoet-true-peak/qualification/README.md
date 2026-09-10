# Offline qualification and Fast066 commissioning

The source-round scripts independently re-derive the frozen mathematical
artifacts used by the surviving HQ1024V1 certified surface. They are source
evidence, not a substitute for compiling and executing the Rust tests.

Run the offline source round with:

```text
python3 -B qualification/run_offline_qualification.py
```

The runner:

1. regenerates HQ1024 coefficients and requires byte identity with checked-in
   Rust/JSON artifacts;
2. regenerates the qualified 2x prefix and requires byte identity;
3. independently verifies the fixed-radix-2 prefix enclosure, twiddles,
   packed-channel scaling, AVX same-graph source shape, and DAZ/FTZ-safe
   classification;
4. independently verifies HQ/Legacy dyadic bounds, operator norms, dense Legacy
   oracle composition, HQ response probes, and shared Reference/Standard source
   invariants;
5. regenerates Fast066V2 midpoint/phase metadata, including the common outward
   tail-L1 maximum, verifies the two 4x-child coefficient bounds, and exercises
   the flat 256-frame envelope over deterministic exact-rational cases;
6. checks the 64-nominee spatial/global policy under saturation and audits the
   fixed 64/8/104 source constants;
7. source-audits the Fast066V2 graph and the presence of the targeted Rust
   falsification hooks; and
8. unit-tests the commissioning driver's duration-normalized strict C stop/go
   rule, benchmark identity validation, result parser, and A/B/C/D orchestration.

The source audit in step 7 intentionally does **not** claim that Rust tests ran.
The Rust unit/integration suite is the behavioral authority for per-tile work,
rejected-neighborhood oracle enumeration, prefix-error-run envelope behavior,
finishing stop conditions, tile ordering, and future-input chunk invariance.

## A/B/C/D commissioning

`commission_fast066_v2.py` is an external commissioning driver. It does not add
a V1 mode or a fourth tier to the production API. It requires the exact V1
baseline archive and verifies this SHA-256 before extraction:

```text
604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0
```

Example:

```text
python3 -B qualification/commission_fast066_v2.py \
  --v1-archive /path/to/tonepoet-fast066-v1-implementation-corrected-2026-09-09.tar.gz \
  --carrier /path/to/carrier.f64le \
  --sample-rate 192000 \
  --channels 2 \
  --expected-point-dbtp <revalidated-dense-HQ-point> \
  --rustflags '<operator release flags, including any vectorizer setting>' \
  --output /path/to/fast066-commissioning.json
```

The driver hashes the carrier and source inputs before measured execution. It
then runs:

- **A** — unchanged V1 Fast, instrumented and nonbinding;
- **B** — V2 survey + bounds (`fast-survey`), instrumented and nonbinding;
- **C** — V2 B + nomination (`fast-nominate`), instrumented and nonbinding;
- **D-instrumented** — complete V2 with stage attribution, only if C is strictly
  below 0.66 seconds per programme minute; and
- **D-binding** — complete V2 without `fast-stage-timing`, which is the binding
  wall-time gate.

If C is at or above 0.66 s/min, the driver records the stop decision and does not
run D for performance purposes. The JSON manifest records one carrier digest,
A/B/C/D labels, edge policy, V1/V2 source identities, algorithm revisions,
`rustc -vV`, Cargo version, operator/codegen inputs, release profile settings,
active SIMD backend, wall times, certificate/error data, V2 work counters, and
the final speed/accuracy gate state. Carrier/source hashing and compiler metadata
collection occur outside the benchmark executable's wall-time interval. V1 and
V2 use separate Cargo target directories so one source tree cannot satisfy the
other's build artifacts accidentally. V2 runs from a disposable byte-checked
source copy, and any Cargo-generated lockfile digest is recorded separately from
the immutable source-tree identity.

The research probes that selected 64 nominees, eight finishers, and step 32 are
external numerical evidence. They do not replace the crate's independent Rust
oracle or establish production throughput.

# Offline qualification

These scripts independently re-derive the frozen mathematical artifacts used by
the surviving HQ1024V1 certified surface. They are source-round evidence, not a
substitute for Rust compilation or operator commissioning.

Run:

```text
python3 qualification/run_offline_qualification.py
```

The runner:

1. regenerates HQ1024 coefficients and requires byte identity with checked-in
   Rust/JSON artifacts;
2. regenerates the qualified 2x prefix and requires byte identity;
3. independently verifies the owned fixed-radix-2 prefix enclosure, twiddles,
   packed-channel scaling, AVX same-graph source shape, and DAZ/FTZ-safe
   classification;
4. independently verifies HQ/Legacy dyadic bounds, operator norms, dense
   Legacy oracle composition, HQ response probes, and the certified search
   source invariants;
5. source-audits the new three-tier HQ surface and the Fast rate-derived,
   fail-closed deadline behavior, including the outward FIR-L1 prefix fallback
   and tile-local raw-support fallback.

The old point-estimator-ladder ceiling and fast-headroom qualification scripts
were removed with the retired public ladder. LegacyHeadroom64 construction data
remains because it is still the independent internal oracle used by tests.

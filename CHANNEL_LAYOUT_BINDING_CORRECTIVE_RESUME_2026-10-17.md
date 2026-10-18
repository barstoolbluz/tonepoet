# Resume checkpoint — stereo-fold source speaker binding corrective

**Task:** Repair stereo downmix that lost or misrouted speaker roles after Float64 intermediates. **Authoritative code base:** this repository archive. **Input:** previously delivered channel/DC source archive. **Date:** 2026-10-17.

## Implemented

- `src/convert/pipeline/stages.rs`: original source ffprobe channel-layout field; WAVEFORMATEXTENSIBLE mask reader; codec/DVD-A/SACD verified binding; physical `-channel_layout` ahead of pan ingress; layout carried across pre/post resampling and in the DSD certified observation route; refusal for ambiguous multichannel layouts; focused Rust unit tests.
- `validation/channel_layout_binding_corrective.py`: 14 real decoded-audio verification cases. Existing `validation/channel_dc_smoke.py` unchanged.
- `CHANNEL_LAYOUT_BINDING_CORRECTIVE_DELIVERY_2026-10-17.md`: design, verification, boundaries, and build-host acceptance steps.

## Tests performed

Run from repository root:

```
python3 validation/channel_dc_smoke.py
python3 validation/channel_layout_binding_corrective.py
```

Both passed: **13 + 14** DSP smoke cases. `git diff --check` passed. The output file/patch SHA-256 list is adjacent to the downloadable artifacts.

## Known remaining gate

Rust toolchain absent; **no Rust compilation or full test gate performed**. First action on a build host: run `cargo check --workspace`, `cargo test --workspace`, and the repo's existing qualification gate. Pay particular attention to the new source-layout helper, the DSD certified true-peak layout propagation, real FLAC/WavPack ISO integration, and the unit tests added at the `r18_deemphasis_lowering_tests` module. If compilation fails, fix only concrete diagnostic issues; do not introduce extra abstractions or expand the brief.

## Out of scope

Do not alter normalized pan coefficients, true-peak gain policy, dither ownership, registered-effect scheduler, Reference replay proofs, or unrelated failure-accounting/transaction flows without separate evidence of a directly implicated defect.

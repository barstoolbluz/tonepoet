# DSD Reference P0 Corrective v5 Handoff

**Date:** 2026-07-19  
**Active candidate policy:** `sox_ng_14_8_0_1_v3`  
**Runtime exposure:** disabled until promotion; the checked-in artifact is `qualification_candidate`

## Corrective scope

This tree addresses the five defects reported against corrected v4 without altering the historical v1 or v2 policy artifacts.

1. **Planner rejection precedence is deterministic.** `plan_reference_dsd()` checks Manual, policy, programme scope, source facts/cell admission, then target/profile/depth/gain. Cartesian unit coverage proves Manual always returns `DSD-REF-P0-001`, including through the public `plan_conversion()` entrypoint, regardless of malformed source facts, historical policy, programme scope, target, or depth.
2. **Int16 is unavailable under v3.** No two-LSB claim remains as enabled authority. Every Int16 Reference target fails before render with `DSD-REF-P0-022`. Shibata remains representable only for historical decoding and future evidence work.
3. **Analyzer promotion requires systematic evidence.** The mandatory gate executes 1,200 planner-command/production-parser cases across every supported rate, mono/stereo, two single-tone frequencies, two phases, three peak levels, two durations, early/late placement, and a phase-aligned four-tone family with fractional-sample peaks. It records a canonical digest and rejects any cell whose conservative upper result falls below analytic truth or exceeds the frozen 0.110000000 dB one-sided authority.
4. **Enabled source cells exercise the production materializer and planner render.** Native DSF and DSDIFF/DSD run through the exact private-copy seam for DSD64/128/256 mono/stereo. DSDIFF/DST DSD64 stereo runs through CMPR classification, production predictive decode, DSTC verification, canonical DFF write/readback, oracle comparison, executed-evidence binding, and the exact planner render. SACD DSD/DST is fail-closed with `DSD-REF-P0-023` until pinned ISO fixtures qualify the production extraction seam.
5. **Metadata evidence is narrowly named.** The package matrix proves only test-side FFmpeg stream-copy metadata sample identity. It does not claim independent qualification of the production metadata/artwork mutator; production still requires post-mutation decode-and-compare before publication.

## V3 enabled matrix

Source cells:

- DSF uncompressed: DSD64, DSD128, DSD256; mono and stereo.
- DSDIFF/DSD uncompressed: DSD64, DSD128, DSD256; mono and stereo.
- DSDIFF/DST predictive: DSD64 stereo only.
- SACD DSD/DST: unavailable under v3.

Terminal cells:

- Int24 for the seven P0 lossless targets.
- Float32 and Float64 for RIFF, RF64, and W64.
- Int16: unavailable under v3.

The expanded supported-cell count is `13,248`; its canonical digest is:

```text
8655f32296e3ac0012357c321cae026eb0effbcb3e128d5a1fad673fe12927a3
```

## Policy promotion contract

The following files are append-only v3 candidate authority:

```text
docs/brief_dsd_reference_p0_policy_v3_amendment.md
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_candidate.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_certification.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_report.md
```

The current and candidate policy JSON files are byte-identical and unpromoted. Runtime rejects the policy unless all of the following are true:

- the current artifact status is changed to `qualified_release`;
- the exact preserved candidate-manifest SHA-256 is bound;
- the exact schema-version-3 machine report SHA-256 is bound;
- that report passes strict structural and evidence validation;
- the compiled policy tables, source cells, target/depth cells, package arguments, analyzer dimensions, and expanded-cell digest match the embedded artifact;
- the exact SoX-ng/FFmpeg package identities and behavior probes match.

Do not promote by editing status alone.

## Mandatory gates

Run unchanged in the commissioned environment:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The final gate must produce a passing schema-version-3 report. Bind that report and the preserved candidate digest into a promoted artifact only after all earlier gates pass in the same source/build environment.

## Changed files in this corrective round

```text
docs/brief_dsd_reference_p0_policy_v3_amendment.md
docs/handoff_dsd_reference_p0_corrected_v5.md
src/convert/pipeline/manifest.rs
src/convert/pipeline/manifest_builder.rs
src/convert/pipeline/track_executor.rs
tests/dsd_reference_qualification.rs
tests/dsd_reference_settings_sentinel.rs
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_candidate.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_certification.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v3_report.md
tonepoet-pipeline/src/dsd_reference.rs
tonepoet-pipeline/src/settings.rs
```

## Environment limitation

The assembly environment used for this bundle did not contain Cargo, rustc/rustfmt, Nix, Flox, or the commissioned SoX-ng closure. No release qualification is claimed. The source-controlled candidate deliberately remains non-executable until the mandatory gates and promotion binding succeed.

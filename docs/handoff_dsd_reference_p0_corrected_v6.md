# DSD Reference P0 Corrective v6 Handoff

> **Superseded historical snapshot.** This document records an earlier corrective round and does not describe the current policy or handoff state. The current authority is [`docs/handoff_dsd_reference_p0_current.md`](handoff_dsd_reference_p0_current.md), with candidate policy `sox_ng_14_8_0_1_v5`.

**Date:** 2026-07-19
**Historical candidate policy at this snapshot:** `sox_ng_14_8_0_1_v3`
**Runtime exposure:** disabled until promotion; the checked-in artifact remains `qualification_candidate`

## Corrective scope

This narrow corrective round fixes the full-domain `DbNano` parsing defect reported against corrected v5.

1. `DbNano::from_str()` now parses decimal components with checked `i128` arithmetic, applies the sign in that wider domain, and performs one final `i64::try_from` range check.
2. Both endpoints of the persisted domain round-trip exactly:
   - `DbNano(i64::MIN)` ↔ `-9223372036.854775808`
   - `DbNano(i64::MAX)` ↔ `9223372036.854775807`
3. The adjacent out-of-range values are rejected:
   - `9223372036.854775808`
   - `-9223372036.854775809`
4. Serde round-trip coverage is included when the `serde` feature is enabled.
5. The existing typed parse tests for both v3 policy JSON files remain in place and now explicitly assert that the disabled Int16 sentinel deserializes as `DbNano(i64::MIN)`.

The v3 policy artifacts are unchanged. This round corrects the generic fixed-point parser rather than redefining policy evidence or changing the candidate identity.

## Changed files

```text
docs/handoff_dsd_reference_p0_corrected_v6.md
src/convert/pipeline/track_executor.rs
tonepoet-pipeline/src/dsd_reference.rs
docs/handoff_manifest.txt
```

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

## Environment limitation

The assembly environment did not contain Cargo, rustc/rustfmt, Nix, Flox, or the commissioned SoX-ng closure. No release qualification is claimed. The v3 candidate remains fail-closed and non-executable until the mandatory gates and promotion binding succeed.

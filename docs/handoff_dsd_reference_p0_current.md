# Current DSD Reference P0 Handoff

**Date:** 2026-07-20
**Current candidate policy:** `sox_ng_14_8_0_1_v5`
**Runtime exposure:** fail-closed until promotion; ordinary defaults remain exact legacy behavior
**Supersedes:** all earlier `handoff_dsd_reference_p0_*` snapshots for current-state claims

## Current routing authority

`tonepoet_pipeline::selects_reference_dsd_to_pcm()` is the sole admission predicate for native-v2 Reference planning and Reference-only preflight work. It accepts the settings plus an authoritative DSD-source classification and requires all three conditions:

1. native-v2 DSD settings;
2. a DSD source;
3. a non-DSD target.

The bridge passes `SourceInfo::is_dsd()` before source-kind probing, settings-validation branching, and Reference target-catalog resolution. The pure planner passes the same classification for final dispatch. Pre-realization SACD paths pass the source-kind fact directly. Native-v2 DSD-to-DSD requests therefore remain on the ordinary DSD topology and do not perform Reference-only SACD rejection, deferred Reference materialization, rerun preflight, or DSF/DSDIFF source-kind probing.

Regression coverage pins:

- native-v2 SACD to DSF with a nonexistent ISO;
- native-v2 SACD to DFF with a nonexistent ISO;
- native-v2 staged DSF and DSDIFF to DSF;
- native-v2 SACD to FLAC still returning `DSD-REF-P0-023` without TOC I/O;
- the shared predicate's native/source/target admission matrix.

## Runtime defaults

`DsdSettings::default()` remains the exact legacy-v1 representation until a qualified policy is promoted. CLI Reference controls explicitly migrate to native-v2. Pre-promotion TUI defaults do not expose Reference-only controls or create mixed-origin settings.

## Policy-v5 terminal bound

The public post-final ceiling remains exactly `-1.000000000 dBTP`. Policy v5 reserves exactly one analyzer reporting quantum (`0.010000000 dB`) before terminal realization.

The deterministic standard-library generator

```text
tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py
```

recomputes each safe pre-terminal ceiling at 120-digit decimal precision from the public ceiling, reserve, and Q1.63 epsilon, then rounds toward negative infinity to one nanodecibel. `--check` verifies both checked-in v5 qualification artifacts and the compiled Rust constants/cells. Runtime artifact validation additionally requires:

```text
terminal_bounds.post_final_acceptance_reserve_db
    == analyzer.reporting_uncertainty_db
```

Current qualified-candidate cells remain:

- Int24 TPDF: `-1.010002327 dBTP`;
- Float32: `-1.010001164 dBTP`;
- Float64: `-1.010000001 dBTP`.

Historical v1-v4 artifacts remain decode/history-only and were not modified.

## Verification commands

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v5_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

## Environment limitation

The bundle-assembly environment does not provide Cargo, rustc, rustfmt, Clippy, Nix, Flox, or the pinned SoX-ng closure. The Python high-precision derivation check and archive/manifest checks can run here; Rust compilation, tests, linting, formatting, and commissioned live qualification must be run in the declared toolchain. No promotion or release qualification is claimed.

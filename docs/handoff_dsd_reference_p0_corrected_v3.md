# DSD Reference P0 corrected-v3 handoff

**Date:** 2026-07-19
**Authority:** `docs/brief_dsd_reference_p0_implementation.md`
**Implementation exposure:** native-v2 Reference behavior remains exposed only through the qualified, fail-closed path commissioned by the brief; Manual, lossy, programme-wide, B6 execution, multichannel Reference, DSD512/1024, and PCM→DSD Reference remain unavailable.

## Corrective scope completed

This tree incorporates the prior P0 implementation and corrects the release-blocking findings from the independent audit:

1. The bundled implementation brief is byte-identical to the supplied authority.
2. `DsdSettings` has one authoritative directional public shape: `pcm_to_dsd`, `from_dsd`, and a private origin. The complete legacy flat wire remains private and retains exact legacy serialization, planner behavior, and fingerprint identity.
3. Planner errors resolve the concrete source rate, target, depth, and exact gain mode while preserving the stable P0 codes and wording.
4. Ordinary RIFF admission requires a deterministic non-audio upper bound supplied by the bridge; it no longer substitutes one unexplained fixed reserve for all metadata/artwork plans.
5. The qualified Nix derivations are compiled into the binary. Runtime requires runner, activation, and compiled executable paths to canonicalize to the same object and binds executable content, store identity, version, behavior probes, ABI, and dispatch into execution identity.
6. The mandatory real-tool selector is exactly `TONEPOET_REQUIRE_TOOLS=1`; the commissioned gate cannot silently select a different environment variable.
7. The DST release corpus contains 12 pinned cases: compressed DSD64 stereo and six-channel oracle pairs plus independent standards-literal DSD64/128/256 geometry fixtures. Six-channel decode is tested while Reference admission remains rejected.
8. The D1 real-tool gate measures B1/B2 integrated `rate -u` behavior and the full composite `rate -u`→`sinc` chain for B3, B4, B4W, B5, and fixture-only B6.
9. The package gate covers all ten target rates, mono/stereo, every permitted target/depth pair, FLAC 0–8, WavPack 0–3, decoded-sample identity, RIFF/RF64/W64 identity, and repeated identity after metadata mutation: 840 package cases. WavPack Int24 uses the policy-frozen `-bits_per_raw_sample 24` declaration because direct command validation proved that the otherwise literal FFmpeg suffix can promote the terminal stream to 32-bit storage.
10. The generated qualification report records concrete tool/store paths, executable hashes and versions, platform facts, measured response values, DST authority, analyzer and terminal bounds, package results, matrix counts/digest, and outcome.
11. Historical handoff files are marked superseded so their old gate spelling and reduced fixture scope cannot be mistaken for current authority.

## Source-controlled evidence

- Policy artifact: `tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v1.json`
- Human-readable policy evidence: `tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v1_report.md`
- DST provenance: `crates/sacd-rs/src/dst/fixtures/P0_PROVENANCE.json`
- DST payload manifest: `crates/sacd-rs/src/dst/fixtures/P0_SHA256SUMS`
- Independent standards-literal oracle: `crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py`
- Mandatory generated release report: `target/dsd_reference_qualification_report.json`, written atomically by the real-tool gate

## Mandatory release gates

Run from the qualified Flox/Nix environment without changing policy data or weakening assertions:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

Every command is mandatory. A skipped tool gate, compilation error, warning, test failure, response-bound failure, package mismatch, or report mismatch is a release blocker.

## Validation performed in the handoff environment

The handoff environment did not contain Cargo, rustc/rustfmt, Nix, Flox, or the commissioned SoX-ng 14.8.0.1 closure. It therefore did not execute or claim the mandatory gates above.

The following non-substitute checks were executed successfully:

- all 24 files in `P0_SHA256SUMS` matched;
- the independent standards-literal oracle verified all six raw fixtures;
- every repository JSON file parsed with duplicate-key rejection;
- every `Cargo.toml` parsed as TOML;
- every Python file compiled;
- all 326 Rust files passed delimiter/state scanning and tree-sitter Rust syntax parsing;
- all 27 parsed `PlanRequest` literals contain the RIFF-bound field;
- the expanded qualification matrix recomputed to 53,760 cells with digest `23571712e01ba8b27c62ebe24930036d9e27e46f41a9e5263955b864ac8ce452`;
- the corrected composite SoX command shape was smoke-executed with the available unqualified system SoX solely to reject malformed argv; this is not policy qualification;
- the available unqualified FFmpeg 7.1.3 demonstrated that WavPack Int24 without `-bits_per_raw_sample 24` reports 32 raw bits, while the corrected canonical argv reports 24; this is command-contract validation, not qualification of the pinned FFmpeg closure;
- the final archive was freshly extracted and its complete handoff manifest reverified.

## Required recipient report

After running the mandatory gates, report:

- exact pass/fail result for every gate;
- generated qualification-report path and SHA-256;
- exact SoX-ng and FFmpeg store paths, executable hashes, and versions;
- archive SHA-256 and manifest pass count;
- any skipped gate or environmental deviation as a release blocker.

## Changed files in this corrective round

```text
build.rs
crates/sacd-rs/build.rs
crates/sacd-rs/src/dst/fixtures/P0_PROVENANCE.json
crates/sacd-rs/src/dst/fixtures/P0_SHA256SUMS
crates/sacd-rs/src/dst/fixtures/generate_p0_raw_fixtures.py
crates/sacd-rs/src/dst/fixtures/raw_dsd64_6ch.dsd.bin
crates/sacd-rs/src/dst/fixtures/raw_dsd64_6ch.dst.bin
crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py
docs/brief_dsd_reference_p0_implementation.md
docs/handoff_dsd_reference_p0_corrected_v3.md
docs/handoff_dsd_reference_p0_corrective_round.md
docs/handoff_dsd_reference_p0_second_pass.md
flake.nix
src/convert/pipeline/plan_bridge.rs
src/convert/pipeline/stages.rs
src/convert/pipeline/track_executor.rs
src/main.rs
tests/dsd_reference_qualification.rs
tests/settings_sentinel.rs
tonepoet-pipeline/API_SURFACE.md
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v1.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v1_report.md
tonepoet-pipeline/src/dsd_reference.rs
tonepoet-pipeline/src/enums.rs
tonepoet-pipeline/src/fingerprint.rs
tonepoet-pipeline/src/plan.rs
tonepoet-pipeline/src/plugins.rs
tonepoet-pipeline/src/settings.rs
tonepoet-pipeline/tests/planning.rs
tonepoet-pipeline/tests/settings_fingerprint.rs
```

No new P0 `NEEDS-VERIFICATION` API seam was introduced. Release certification remains blocked until the mandatory compiled and qualified-tool gates run in the commissioned Flox/Nix environment. The archive SHA-256 and fresh-extraction manifest result are supplied alongside the archive in the external validation report.

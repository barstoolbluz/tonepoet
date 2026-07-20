# DSD Reference P0 corrected-v4 handoff

**Date:** 2026-07-19  
**Authority:** `docs/brief_dsd_reference_p0_implementation.md`, narrowed by `docs/brief_dsd_reference_p0_policy_v2_amendment.md` for the append-only corrected policy.  
**Implementation exposure:** the checked-in `sox_ng_14_8_0_1_v2` artifact is a fail-closed `qualification_candidate`. Runtime accepts it only after the mandatory gates pass and the generated certification report is cryptographically bound into a promoted `qualified_release` artifact.

## Corrective scope

This round addresses the four reported defects without reinterpreting policy v1.

1. **One command authority.** `tests/dsd_reference_qualification.rs` no longer constructs Reference render, measurement, terminal, or package argv independently. It obtains the exact `PlannedExecutionStep` sequence from `plan_reference_dsd()`, executes the planner-emitted commands, and rejects any unexpected command or second terminal realization. B6 remains a transcript-only fixture produced by the production render builder.
2. **Production measurement and gain machinery.** Loudnorm object extraction, strict `input_tp` parsing, signed-zero validation, conservative `Q + E` construction, deferred-gain resolution, and post-final ceiling validation are production functions in `tonepoet-pipeline`. The executor and qualification target call the same implementations. One deterministic DSF fixture executes the complete planner-emitted render → pre-final measurement → deferred binding → terminal realization → post-final measurement → package chain. Controlled R64 fixtures cover Reference reduction, NativeLevel and Fixed exact success/refusal, NormalizePeak, verified silence, Int16 Shibata, Int24 TPDF, and Float32/64 bounds.
3. **Empirical analyzer and terminal authority.** The real-tool gate measures analytically known Float64 inter-sample-peak fixtures through the exact planner measurement command, including a monotonic sweep and nonzero near-silence. It verifies conservative bounds against analytic truth. Every target-rate/channel/depth terminal cell is measured against its Q1.63 error ceiling, and any SoX Shibata fallback is fatal.
4. **Compressed DST evidence boundary.** Predictive DSDIFF/DST and SACD/DST are supported only for stereo DSD64, the sole mono/stereo cell represented by the independent predictive-compression oracle corpus. DSD64 mono and DSD128/256 `DSTCoded=0` fixtures remain geometry/raw-frame evidence and are not represented as predictive-compression qualification. Every predictive-DST cell outside DSD64 stereo fails before decode with `DSD-REF-P0-021`.
5. **Append-only policy governance.** WavPack Int24 `-bits_per_raw_sample 24` exists only under `sox_ng_14_8_0_1_v2`. V1 remains historical and cannot execute as v2. The v2 manifest explicitly binds the altered command contract and narrowed source-kind/rate/channel matrix.
6. **Promotion is fail-closed.** The source tree contains a candidate manifest snapshot and a placeholder certification report. A passing commissioned gate produces the machine report. Promotion must preserve the candidate snapshot, bind its SHA-256 and the installed report SHA-256, then change status to `qualified_release`. Runtime validates the candidate snapshot, report bytes, report contents, cell matrix, and all other policy evidence before resolving tools. It also normalizes only the three permitted promotion fields—status and the two certification digests—and requires the promoted manifest to equal the preserved candidate in every other field.

## Policy-v2 evidence

```text
docs/brief_dsd_reference_p0_policy_v2_amendment.md
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_candidate.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_certification.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_report.md
```

The candidate matrix contains 35,616 supported cells with canonical digest:

```text
3cba170e0958da5532704c2147cb713f502547d2128c7d0cf89d4ec22df825d5
```

## Mandatory gates and promotion

Run from the commissioned Flox/Nix environment:

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The final command replaces the placeholder certification file atomically. Before changing manifest status, record:

- SHA-256 of the preserved candidate manifest in `release_certification.candidate_manifest_sha256`;
- SHA-256 of the generated certification file in `release_certification.report_sha256`;
- `status = "qualified_release"`.

Then rebuild and rerun the formatting, unit, workspace, and clippy gates. Re-run the real-tool qualification as a regression check with `TONEPOET_DSD_REFERENCE_REPORT_PATH` directed to a non-authoritative target path (for example, `target/dsd_reference_qualification_report.json`); do not overwrite the certification file whose digest was bound during promotion. Finally, verify that runtime attestation accepts the bound candidate snapshot and certification report. Any source, command, candidate-matrix, report, toolchain, or output change after certification requires a new policy identity rather than mutation of v2.

## Validation limits in this environment

Cargo, rustc/rustfmt, Nix, Flox, and the commissioned SoX-ng 14.8.0.1 closure are unavailable. This handoff does not claim any mandatory gate passed.

The available system SoX 14.4.2 and FFmpeg 7.1.3 were used only for adversarial command probes. Those unqualified probes exposed exactly why promotion remains gated: the local stack does not satisfy the candidate analyzer/terminal assumptions. No result from those binaries is used as qualification evidence or to weaken a candidate cell.

## Changed files

```text
docs/brief_dsd_reference_p0_policy_v2_amendment.md
docs/handoff_dsd_reference_p0_corrected_v4.md
docs/handoff_manifest.txt
src/convert/pipeline/track_executor.rs
tests/dsd_reference_qualification.rs
tests/dsd_reference_settings_sentinel.rs
tonepoet-pipeline/Cargo.toml
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_candidate.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_certification.json
tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v2_report.md
tonepoet-pipeline/src/dsd_reference.rs
tonepoet-pipeline/src/settings.rs
```

Historical v1 files remain byte-for-byte policy history; they are not current execution authority. Manual, lossy, programme-wide, B6 execution, multichannel Reference, DSD512/1024, and PCM→DSD Reference remain unavailable.

# R31 durable resume checkpoint -- 2026-10-11

## Authority and source basis

- Task: `BRIEF_LOGGING_R31_analysis_clipping_and_pill_default_2026-10-11.md`.
- Supplied full-tree archive: `TonePoet_LOGGING_R31_analysis_clipping_and_pill_default_2026-10-11.tar.gz`; SHA-256 `a5e85c3a5be8f3733110d54d5414dab38b73bf694e6a88d2ebbf36edff826856`.
- Supplied brief SHA-256: `9c48c5dad9b81c8a3ab69b62bbe8b6f84cc2ab63a22fad21433c3385299e6ef3`.
- This archive is the complete proposed R31 source candidate, not a compiled/Reference-qualified release. `R31_FROM_INPUT.patch` replays its changed Rust sources onto the identical supplied input snapshot.

## Exact correction and constraints

- Four changed production/source-tree paths: `src/tui/analyze.rs`, `src/tui/convert_screen.rs` (test only), `src/tui/draw_overlays.rs`, `src/db.rs`.
- Count sample over-range when decoded `abs(sample) > 1.0`; remove the sign-asymmetric and bit-depth-blind near-ceiling threshold. `Clipping` UI label -> `Samples > 0 dBFS`. No separate audio scan or inference heuristic.
- R30 default row test expects `-0.10 dBTP` for PCM and DSD; production/default values unchanged.
- Cache algo version 27 -> 28; metadata-only UPSERT carries an existing count only when the file identity and algorithm version both match. This prevents old false counts from reappearing on the next Analyze invocation.
- Do not change `src/convert/pipeline/stages.rs`, `tonepoet-pipeline/src/settings.rs`, the true-peak DSP/normalizer, Reference sidecars, or `scripts/requalify_reference_r20.sh`. No dependencies added.

## Recorded checks

- `git diff --check`: PASS; independent code-patch replay: PASS.
- Bash syntax on R31, R30, Reference scripts: PASS.
- FFmpeg CLI decoder boundary checks (all five integer/float depths): PASS. The CLI's native FFmpeg decode mirrors the relevant sample-domain semantics, but does not constitute a compiled TonePoet test.
- SQLite executing the actual metadata-only UPSERT extracted from `src/db.rs`: PASS for stale/current/different-file versions.
- `./R31_ACCEPTANCE.sh`: correctly refused (exit 2, Cargo unavailable). Rust tests, full workspace tests, release builds, positive SACD conversion, and Reference requalification are still hard gates.
- The received brief reports 7,434 prior test passes and two failures. Do not cite those as verified by this R31 session.

## Next action on build host

1. Extract the complete R31 archive and verify `sha256sum -c R31_SOURCE_SHA256SUMS.txt` from the project root.
2. Run `cargo test --locked --lib r31_` (integer PCM, floating PCM/seek, cache promotion) and `cargo test --locked --lib r30_` (including the corrected custom pill test).
3. Run `./R31_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso`, including fresh source-locked Reference qualification, full workspace gates and release builds; perform the negative runtime-closure smoke separately.
4. If a gate fails, isolate and correct only the demonstrated defect, update the patch/inventory/checkpoint, and rerun the affected gates. Do not introduce broad rearchitecting or extrapolated robustness work.

The package's source checksum inventory covers all files except its own manifest, and the former R30 inventory is historical. The full archive is the preferred resume input.

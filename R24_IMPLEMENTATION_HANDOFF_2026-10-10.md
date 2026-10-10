# TonePoet R24 implementation handoff — October 10, 2026

## Authority and scope

Implemented against `BRIEF_LOGGING_R24_gain_truth_failfast_provenance_2026-10-10.md` using the supplied R24 full-tree input. Only these production sources were touched:

- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/execution_evidence.rs`
- `src/convert/pipeline/materializer_sacd.rs`
- `src/tui/app.rs`
- `src/tui/draw_output_options.rs`

The existing Reference audio rendering and DSP source (`tonepoet-pipeline/src/dsd_reference.rs`, `semantic_plan.rs`, and `src/convert/pipeline/track_executor.rs`) is untouched. No new journal, cache, transaction subsystem, or background scanner was added.

## Changes and status

1. **Reference gain truth (implemented):** The qualified Reference path reconstructs protected R64 with SoX `gain -12.000000000`. Its terminal scalar restores this fixed 12 dB and applies the programme-level gain. The *separate* General DSD export-level 6.020599913 dB compensation must **not** be subtracted here. Human album-gain text now displays music-level gain (`terminal_applied - 12 dB`) and music-level true peak (`protected_R64_peak + 12 dB`), the fixed excluded term, the actual terminal scalar, and the target. The precise scalar, scan, gain authority, and structured evidence are unchanged.
2. **Performed gain (implemented):** After the certified Reference terminal completes, `completed_reference_plan_evidence` emits a completed `apply_gain` operation, bound to its actual terminal invocation indices. The backend is SoX for regular Reference terminals or TonePoet's Float64 scalar pump for admitted Int32; the operation records the terminal scalar and fixed 12 dB restoration. Complete, uniform album gains are promoted once; differing gains remain on their respective tracks. Existing decision evidence is retained.
3. **Human words (implemented):** `Artifact work:` → `Output file:` and `Package output` → `Write output file`; the structured operation `kind` and domain remain unchanged.
4. **TUI wording (implemented):** `source track fails / fail source / keep successes` → `on failure / fail conversion / keep partial output`. The persisted bool, behavior, and CLI `--partial` are untouched.
5. **Per-field provenance (implemented for authoritative SACD/override/enrichment paths):** SACD sidecar XML, SACD disc text/TOC, filename/folder heuristic, label-dictionary enrichment, batch-resolved identity, and explicit metadata override/clear are identified per changed field. Unannotated input families are labeled by their source kind, not guessed to be a sidecar or embedded tag. Absent fields are rendered as blank rows. For merged album fragments, the origin follows the winning field value. Internal origin keys are excluded from audio metadata export. The SACD disc catalog number is now recognized if album catalog is absent. Old log fragments deserialize with an empty origin map.
6. **Fail-fast destination (#72) (PARTIAL, intentionally):** Once output paths are planned, existing directory/file obstructions that categorically prevent publication under `FailIfExists` are refused *before audio conversion/ReplayGain*. **Ordinary pre-existing audio is not refused at this point**, because an identical rerun is accepted at publication, and path existence, tags, or a prior settings fingerprint do not prove byte inequality. Exact existing-versus-new byte comparison still occurs only after conversion; a general early failure for different *regular* files is not implemented. Also, the plan currently follows materialization, so this new guard cannot eliminate pre-materialization work. Satisfying the stronger literal requirement without breaking identical reruns would require an independently qualified equality certificate or equivalent trusted proof not present in the current architecture. Do **not** substitute `path.exists() => reject` or speculative fingerprint-based refusal.

## Added focused tests

- `r24_reference_protection_is_twelve_db_not_general_export_compensation`
- `r24_applied_gain_appears_once_when_uniform_and_on_each_track_when_divergent`
- `r24_source_information_shows_selected_origins_and_explicit_blank_fields`
- `r24_destination_preflight_refuses_only_proven_obstructions`
- `r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers`

R24 tests added to existing test modules; the first gain test calls the exact production formatter; no standalone new runtime dependencies.

## Qualification status

**NOT BUILD-HOST-QUALIFIED.** `cargo`, `rustc`, and `rustfmt` are absent in this environment. The full Rust workspace, TUI interaction, and live SACD conversions have **not** been executed. Source-locked `stages.rs` has changed: any installed Reference qualification sidecars reflect earlier bytes until the existing `scripts/requalify_reference_r20.sh` runner is rerun on a qualifying build host. Before release: run the workspace gates and that runner, both existing black-box smokes, the native SACD positive smoke, and R24 new tests. Issue #43's live Browse → Convert → preset gesture against a real SACD ISO is still open separately, as stated in the brief.

## Do not expand scope without a failing concrete test

No wholesale publication redesign, cache/journal, audio graph changes, or replacement of trusted source metadata is authorized by this patch. For the remaining #72 gap, first decide with the human how to reconcile the requirement for early *different-file* rejection with the required byte-identical rerun acceptance; no safe path-only decision exists.

#!/usr/bin/env python3
"""Independent offline qualification for the deterministic Fast066 path.

The checks here intentionally do not execute the Rust scanner. They verify the
coefficient theorem and generated metadata independently of audio-shaped input,
exercise the fixed nomination policy under saturation, and audit the source for
the execution-graph obligations that are easy to regress accidentally. Rust
unit/integration tests remain the behavioral authority for the implementation.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
from fractions import Fraction
from pathlib import Path

CRATE_ROOT = Path(__file__).resolve().parents[1]
QUALIFICATION = CRATE_ROOT / "qualification"
LIB_RUST = CRATE_ROOT / "src" / "lib.rs"
FAST_RUST = CRATE_ROOT / "src" / "fast_scan.rs"
CERTIFIED_RUST = CRATE_ROOT / "src" / "certified_scan.rs"
PREFIX_RUST = CRATE_ROOT / "src" / "qualified_half_delay_fft.rs"
COEFFICIENT_RUST = CRATE_ROOT / "src" / "hq1024_coefficients.rs"
FAST_TEST = CRATE_ROOT / "tests" / "fast_scan.rs"
METER_TEST = CRATE_ROOT / "tests" / "meter.rs"
CARGO_TOML = CRATE_ROOT / "Cargo.toml"
BENCH_RUST = CRATE_ROOT / "examples" / "bench_ceiling_f64le.rs"
TIMING_RUST = CRATE_ROOT / "src" / "fast_stage_timing.rs"
COMMISSION_SCRIPT = QUALIFICATION / "commission_fast066_v2.py"
COMMISSION_TEST = QUALIFICATION / "test_commission_fast066_v2.py"

TAIL_FACTOR = 512
TARGET_FACTOR = 1024
TILE_FRAMES = 4096
GROUP_FRAMES = 256
MAX_NOMINEES = 64
MAX_FINISHERS = 8
MAX_EXTRA_FINE_PER_FINISHER = 5
MAX_FINE_PER_TILE_CHANNEL = MAX_NOMINEES + MAX_FINISHERS * MAX_EXTRA_FINE_PER_FINISHER
TAIL_OFFSET_MIN = -16
TAIL_OFFSET_MAX = 17


def import_generator():
    source = QUALIFICATION / "generate_fast_metadata.py"
    spec = importlib.util.spec_from_file_location("tonepoet_fast_metadata", source)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {source}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def exact_abs(value: Fraction) -> Fraction:
    return value if value >= 0 else -value


def exact_value(
    coarse: dict[int, Fraction],
    bank: list[list[Fraction]],
    q: int,
) -> Fraction:
    cell, phase = divmod(q, TAIL_FACTOR)
    if phase == 0:
        return coarse[cell]
    total = Fraction(0)
    row = bank[phase]
    for offset, coefficient in zip(range(TAIL_OFFSET_MIN, TAIL_OFFSET_MAX + 1), row):
        total += coarse[cell + offset] * coefficient
    return total


def coefficient_flat_bound_cases(generator, metadata: dict[str, object]) -> dict[str, object]:
    text = COEFFICIENT_RUST.read_text(encoding="utf-8")
    bank_float = generator.rust_array(text, "HQ1024_TAIL_COEFFICIENTS")
    bank = [
        [Fraction.from_float(float(coefficient)) for coefficient in row]
        for row in bank_float
    ]
    a4 = Fraction.from_float(float.fromhex(str(metadata["a4_upper_hex"])))
    b4 = Fraction.from_float(float.fromhex(str(metadata["b4_upper_hex"])))

    # Two original-sample intervals are enough to exercise both dyadic children
    # in multiple coarse cells while keeping this exact-rational check quick.
    group_end = 2
    first = -16
    last = 2 * group_end + 16
    indices = list(range(first, last + 1))

    patterns: list[tuple[str, dict[int, Fraction]]] = []
    patterns.append(("zero", {index: Fraction(0) for index in indices}))
    patterns.append(("constant", {index: Fraction(13, 16) for index in indices}))
    patterns.append(
        (
            "alternating",
            {index: Fraction(15 if index % 2 == 0 else -15, 16) for index in indices},
        )
    )
    patterns.append(
        (
            "saw",
            {index: Fraction(((index * 7) % 23) - 11, 16) for index in indices},
        )
    )
    for impulse_index in (-16, 0, 1, 3, 4, last):
        coarse = {index: Fraction(0) for index in indices}
        coarse[impulse_index] = Fraction(-7 if impulse_index & 1 else 7, 8)
        patterns.append((f"impulse_{impulse_index}", coarse))

    # A deterministic dyadic pseudo-random vector prevents the exact checks from
    # degenerating into only highly structured sequences.
    state = 0xD1B54A32D192ED03
    random_coarse: dict[int, Fraction] = {}
    for index in indices:
        state ^= state >> 12
        state ^= (state << 25) & ((1 << 64) - 1)
        state ^= state >> 27
        state &= (1 << 64) - 1
        raw = (state * 0x2545F4914F6CDD1D) & ((1 << 64) - 1)
        random_coarse[index] = Fraction(int((raw >> 54) & 0x3FF) - 512, 512)
    patterns.append(("dyadic_random", random_coarse))

    case_reports: list[dict[str, object]] = []
    dense_knots = group_end * TARGET_FACTOR + 1
    for name, coarse in patterns:
        m = max(exact_abs(value) for value in coarse.values())
        d2 = max(
            exact_abs(coarse[index] - 2 * coarse[index + 1] + coarse[index + 2])
            for index in range(first, last - 1)
        )
        p4 = max(
            exact_abs(exact_value(coarse, bank, q))
            for q in range(0, group_end * TARGET_FACTOR + 1, 256)
        )
        upper = p4 + a4 * d2 + b4 * m
        dense = max(
            exact_abs(exact_value(coarse, bank, q))
            for q in range(0, group_end * TARGET_FACTOR + 1)
        )
        if dense > upper:
            raise AssertionError(f"flat coefficient envelope failed for {name}")
        case_reports.append(
            {
                "case": name,
                "dense_knots": dense_knots,
                "slack_hex": float(upper - dense).hex(),
            }
        )

    return {
        "passed": True,
        "cases": case_reports,
        "total_exact_dense_knots": dense_knots * len(case_reports),
    }


def score(values: list[float], index: int) -> float:
    center = abs(values[index])
    if index == 0 or index + 1 == len(values) or center == 0.0:
        return center
    sign = -1.0 if math.copysign(1.0, values[index]) < 0.0 else 1.0
    left = sign * values[index - 1]
    right = sign * values[index + 1]
    if left < 0.0 or right < 0.0:
        return center
    curvature = left - 2.0 * center + right
    if not math.isfinite(curvature) or curvature >= 0.0:
        return center
    delta = 0.5 * (left - right) / curvature
    if not math.isfinite(delta) or abs(delta) > 0.5:
        return center
    proposed = center - 0.25 * (left - right) * delta
    return proposed if math.isfinite(proposed) and proposed >= center else center


def candidate_policy_saturation() -> dict[str, object]:
    survey_len = 4 * TILE_FRAMES + 1
    values = [0.0] * survey_len
    # Thousands of strict local maxima. The strongest nominees are deliberately
    # concentrated near the end so the spatial reserve has work to do that a
    # global-only top-N policy would omit.
    for index in range(2, survey_len - 1, 4):
        microgroup = index // (4 * GROUP_FRAMES)
        local_rank = (index % (4 * GROUP_FRAMES)) // 4
        crowded_bonus = 1000.0 if microgroup == 15 and local_rank >= 192 else 0.0
        values[index] = 1.0 + crowded_bonus + microgroup * 0.01 + local_rank * 1.0e-5

    candidates: list[tuple[float, int]] = []
    spatial: list[tuple[float, int] | None] = [None] * 16
    for index in range(0, 4 * TILE_FRAMES):
        magnitude = abs(values[index])
        local = magnitude != 0.0 if index == 0 else (
            magnitude > abs(values[index - 1]) and magnitude >= abs(values[index + 1])
        )
        if not local:
            continue
        candidate = (score(values, index), index)
        candidates.append(candidate)
        group = index // (4 * GROUP_FRAMES)
        current = spatial[group]
        if current is None or (-candidate[0], candidate[1]) < (-current[0], current[1]):
            spatial[group] = candidate

    ranked = sorted(candidates, key=lambda item: (-item[0], item[1]))
    selected = [candidate for candidate in spatial if candidate is not None]
    selected_indices = {candidate[1] for candidate in selected}
    for candidate in ranked[:MAX_NOMINEES]:
        if len(selected) == MAX_NOMINEES:
            break
        if candidate[1] not in selected_indices:
            selected.append(candidate)
            selected_indices.add(candidate[1])
    selected.sort(key=lambda item: (-item[0], item[1]))

    if len(candidates) <= MAX_NOMINEES:
        raise AssertionError("candidate saturation fixture did not saturate")
    if len(selected) != MAX_NOMINEES:
        raise AssertionError("candidate policy did not fill fixed quota")
    represented = {candidate[1] // (4 * GROUP_FRAMES) for candidate in selected}
    if represented != set(range(16)):
        raise AssertionError(f"spatial reserve lost microgroups: {sorted(represented)}")
    return {
        "passed": True,
        "observed": len(candidates),
        "selected": len(selected),
        "microgroups_represented": len(represented),
        "global_top64_microgroups": len({candidate[1] // (4 * GROUP_FRAMES) for candidate in ranked[:64]}),
    }


def l1_enclosures(generator, metadata: dict[str, object]) -> dict[str, object]:
    text = COEFFICIENT_RUST.read_text(encoding="utf-8")
    bank = generator.rust_array(text, "HQ1024_TAIL_COEFFICIENTS")
    stored = [float.fromhex(value) for value in metadata["phase_l1_upper_hex"]]
    if len(stored) != 512:
        raise AssertionError("Fast metadata does not contain every phase L1")
    common = float.fromhex(str(metadata["tail_l1_upper_hex"]))
    if common != max(stored):
        raise AssertionError("common tail L1 is not the maximum stored phase envelope")
    minimum_margin: Fraction | None = None
    for phase, row in enumerate(bank):
        exact = sum((Fraction.from_float(abs(float(value))) for value in row), Fraction(0))
        outward = Fraction.from_float(stored[phase])
        if outward < exact:
            raise AssertionError(f"phase {phase} L1 is not outward")
        margin = outward - exact
        minimum_margin = margin if minimum_margin is None else min(minimum_margin, margin)
    return {
        "passed": True,
        "phase_count": len(stored),
        "common_tail_l1_upper_hex": common.hex(),
        "minimum_outward_margin_hex": float(minimum_margin or Fraction(0)).hex(),
    }


def source_contract() -> dict[str, object]:
    lib = LIB_RUST.read_text(encoding="utf-8")
    fast = FAST_RUST.read_text(encoding="utf-8")
    certified = CERTIFIED_RUST.read_text(encoding="utf-8")
    certified_tests = certified.split("#[cfg(test)]\nmod tests {", 1)[1]
    generated_metadata = json.loads((QUALIFICATION / "fast066_metadata.json").read_text(encoding="utf-8"))
    midpoint_l1_authority = float.fromhex(str(generated_metadata["midpoint_l1_upper_hex"]))
    prefix = PREFIX_RUST.read_text(encoding="utf-8")
    tests = FAST_TEST.read_text(encoding="utf-8") + "\n" + METER_TEST.read_text(encoding="utf-8")
    cargo = CARGO_TOML.read_text(encoding="utf-8")
    bench = BENCH_RUST.read_text(encoding="utf-8")
    timing = TIMING_RUST.read_text(encoding="utf-8")
    commission = COMMISSION_SCRIPT.read_text(encoding="utf-8")
    commission_test = COMMISSION_TEST.read_text(encoding="utf-8")
    checks = {
        "versioned_revision": 'FAST_ALGORITHM_REVISION: &str = "Fast066V2"' in lib,
        "midpoint_metadata_test_matches_generated_authority": (
            midpoint_l1_authority == 1.9638475377212528
            and "1.963_847_537_721_252_8" in fast
            and "1.963_847_537_721_250_3" not in fast
        ),
        "certified_scan_test_gain_constant_import_present": (
            "use crate::HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER;" in certified_tests
        ),
        "legacy_scaled_peak_helper_present": (
            "#[cfg(test)]\n#[inline]\nfn update_channel_peaks_scaled" in lib
            and "f64_magnitude_bits_for_db(sample * scale)" in lib
            and lib.count("update_channel_peaks_scaled(") == 2
        ),
        "nanosecond_rate": "FAST_WALL_NANOS_PER_PROGRAMME_MINUTE: u64 = 660_000_000" in lib,
        "integer_seconds_rate_removed": "FAST_WALL_SECONDS_PER_PROGRAMME_MINUTE" not in lib,
        "direct_fast_backend": "CertifiedPeakBackend::Fast(" in lib and "fast_scan::FastPeakMeterImpl::new" in lib,
        "no_clock_in_fast_backend": "Instant" not in fast and "SystemTime" not in fast,
        "no_old_search_in_fast_backend": "CertifiedScanner" not in fast and "RetiredClockFast1s" not in fast,
        "canonical_tile": "const TILE_FRAMES: i128 = 4096;" in fast,
        "canonical_group": "const GROUP_FRAMES: i128 = 256;" in fast,
        "nominee_cap": "const MAX_NOMINEES_PER_TILE_CHANNEL: usize = 64;" in fast,
        "finisher_cap": "const MAX_FINISHERS_PER_TILE_CHANNEL: usize = 8;" in fast,
        "fine_work_cap": "const MAX_FINE_EVALUATIONS_PER_TILE_CHANNEL: u64 = 104;" in fast,
        "v1_stencil_removed": all(token not in fast for token in (
            "STENCIL_POINTS", "STENCIL_STEP_Q", "STENCIL_RADIUS_Q", "fn refine_candidate",
        )),
        "two_stage_policy": all(token in fast for token in (
            "fn probe_nominees", "fn finish_proposals", "proposal_priority",
            "FINISH_PROBE_STEP_Q: i128 = 32", "MAX_FINISHERS_PER_TILE_CHANNEL",
        )),
        "certified_neighborhood_dominance": all(token in fast for token in (
            "fn candidate_neighborhood_upper", "CANDIDATE_DOMAIN_RADIUS_Q: i128 = 256",
            "left_halo_upper", "self.channel_lower_peaks[channel]",
            "fast_bound_pruned_channel_tiles", "fast_bound_pruned_nominees",
            "fast_bound_pruned_finishers",
        )),
        "cached_common_tail_envelope": all(token in fast for token in (
            "tail_l1_upper", "fn fine_evaluation_context", "cached_error",
            "fine_support_input_max", "fine_support_coarse_error",
        )),
        "block_prefix_api": "pub(crate) struct QualifiedHalfDelayBlock" in prefix and "process_interleaved_blocks" in prefix,
        "legacy_frame_adapter_retained": "emit_block_frames" in prefix and "process_frame_with_fft_permission" in prefix,
        "numerical_roundoff_terms": all(token in fast for token in (
            "MIDPOINT_ROUNDING_OPS: usize = 48",
            "TAIL_ROUNDING_OPS: usize = 96",
            "SECOND_DIFFERENCE_ROUNDING_OPS: usize = 8",
            "dot_rounding_upper",
        )),
        "prefix_error_cross_block_reduction": "fn max_error" in fast and "coarse.max_error" in fast,
        # These are source-presence audits only. The Rust tests themselves are
        # the behavioral authority; this qualifier must never present token
        # matching as executed falsification.
        "per_tile_quota_regression_hook": all(token in fast for token in (
            "v2_full_tile_capacity_and_work_ceiling_are_per_channel_tile",
            "assert_eq!(diagnostics.tiles_processed, 1);",
            "assert_eq!(diagnostics.fast_candidates_selected, 64);",
            "assert_eq!(diagnostics.fast_proposals_evaluated, 64);",
            "assert_eq!(diagnostics.fast_finishing_candidates, 8);",
            "assert_eq!(diagnostics.fast_fine_knots_evaluated, 104);",
        )),
        "rejected_nominee_oracle_regression_hook": all(token in fast for token in (
            "struct RejectedNomineeAudit",
            "rejected_nominee_audit = Some",
            "v2_rejected_nominee_bound_encloses_every_hq_knot_and_records_same_channel_witness",
            "oracle_hq1024_domain_magnitudes",
            "magnitude <= audit.neighborhood_upper",
            "audit.neighborhood_upper <= audit.lower_witness",
        )),
        "cross_group_behavior_regression_hook": all(token in fast for token in (
            "v2_cross_group_neighbor_prevents_center_group_only_rejection",
            ".probe_nominees(0, start, end, final_q, context)",
            "state.group_uppers[0] <= state.channel_lower_peaks[0]",
            "neighborhood_upper > state.channel_lower_peaks[0]",
        )),
        "prefix_error_run_envelope_regression_hook": all(token in fast for token in (
            "v2_common_tail_envelope_covers_multiple_prefix_error_runs_and_boundary_supports",
            "first_error = [1.0e-12_f64]",
            "second_error = [4.0e-12_f64]",
            "state.coarse.max_error",
            "evaluation.error >= local",
        )),
        "finishing_stop_regressions_hook": all(token in fast for token in (
            "v2_finishing_missing_endpoint_stops_after_available_probe",
            "v2_finishing_invalid_curvature_stops_after_two_probes",
            "v2_finishing_left_tie_stops_without_opening_a_second_search",
            "v2_finishing_larger_probe_stops_after_two_probes",
            "fast_fine_knots_evaluated - before",
        )),
        "tile_ordering_regression_hook": (
            "v2_loud_then_quiet_gates_a_dominated_tile_and_quiet_first_gets_optional_work" in fast
        ),
        "chunk_boundary_regression": (
            "fast066_is_chunk_invariant_including_exact_tile_and_fft_boundaries" in tests
            and "const FRAMES: usize = 8193;" in tests
            and "6657" in tests
        ),
        "future_validation_max_regression_hook": all(token in tests for token in (
            "fast066_pruning_ignores_future_validation_peak_in_the_same_caller_push",
            "const FUTURE_PEAK_FRAME: usize = 7000;",
            "assert_eq!(whole, one",
            "assert_eq!(whole, irregular",
        )),
        "edge_scratch_reused_without_frame_clone": (
            "self.first_frame.clone()" not in fast and "self.last_frame.clone()" not in fast
        ),
        "dense_fixture_regression": "bundled_recording_fast_point_regresses_to_dense_hq1024_target" in tests,
        "dense_all_knot_oracle": all(token in fast for token in (
            "fn dense_hq1024_oracle",
            "dense_all_hq1024_knots_are_contained_per_channel_for_finite_edge_cases",
            "certificate.channel_intervals[channel]",
            "asymmetric_stereo_dense_targets_are_contained_per_channel",
        )),
        "candidate_independent_envelope_regression": all(token in fast for token in (
            "disable_candidates_for_test",
            "flat_envelope_contains_dense_target_when_candidate_refinement_is_starved",
        )),
        "fast_kernel_enclosure_regressions": all(token in fast for token in (
            "scalar_and_avx_midpoint_graphs_fit_declared_enclosure",
            "scalar_and_avx_tail_graphs_fit_declared_enclosure",
            "daz_ftz_cannot_invalidate_fast_midpoint_or_tail_enclosures",
            "flat_group_inequality_contains_dense_hq1024_knots_on_arbitrary_coarse_arrays",
        )),
        "v2_behavioral_regressions": all(token in fast for token in (
            "v2_proposal_priority_uses_measured_magnitude_then_coordinate_then_nominee",
            "v2_signed_fits_round_only_bounded_local_offsets_and_respect_ties",
            "v2_neighborhood_bound_covers_cross_group_and_left_halo_domains",
            "v2_common_tail_envelope_dominates_every_phase_local_envelope",
        )),
        "commissioning_timing_is_feature_gated": (
            'fast-stage-timing = []' in cargo
            and 'mod fast_stage_timing;' in lib
            and 'StageTimer(Instant)' in timing
            and '#[cfg(feature = "fast-stage-timing")]' in fast
            and 'Instant' not in fast
        ),
        "commissioning_ablation_is_feature_gated": (
            "FastCommissioningMode" in lib
            and "SurveyBoundsOnly" in lib
            and "NominationOnly" in lib
            and '#[cfg(feature = "fast-stage-timing")]' in lib
            and "fast-survey" in bench
            and "fast-nominate" in bench
        ),
        "external_abcd_commissioning_driver": all(token in commission for token in (
            "604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0",
            'label="A"',
            'label="B"',
            'label="C"',
            'label="D-instrumented"',
            'label="D-binding"',
            "def c_allows_d",
            "carrier_sha256",
            '"edge_policy": EDGE_POLICY',
            '"rustc_vV"',
            '"operator_rustflags"',
            "active_simd_backend",
            '"stop_go"',
            '"release_gate"',
        )) and all(token in commission_test for token in (
            "test_stop_go_threshold_is_strict",
            "test_stop_go_normalizes_programme_duration",
        )),
        "commissioning_stage_fields": all(token in bench for token in (
            "input_read_decode_nanos",
            "qualified_prefix_block_ingest",
            "hq4_midpoint_survey",
            "flat_envelope",
            "nomination",
            "proposal",
            "finishing",
            "candidate_pipeline_inclusive",
            "final_reduction",
            "binding_fast_wall_gate",
        )) and "commissioning_stage_timing_accumulates_for_nontrivial_fast_scan" in fast,
    }
    failed = [name for name, passed in checks.items() if not passed]
    if failed:
        raise AssertionError(f"Fast066 source contract failed: {failed}")
    return {"passed": True, "checks": checks}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=QUALIFICATION / "fast066_verification.json",
    )
    args = parser.parse_args()

    generator = import_generator()
    metadata = generator.derive()
    report = {
        "schema": "tonepoet-fast066-verification-v2",
        "algorithm_revision": metadata["algorithm_revision"],
        "coefficient_source_sha256": metadata["coefficient_source_sha256"],
        "metadata_exact_child_checks": all(
            child["stored_a_hex"] and child["stored_b_hex"]
            for child in metadata["exact_dyadic_child_checks"]
        ),
        "phase_l1_enclosures": l1_enclosures(generator, metadata),
        "coefficient_flat_bound": coefficient_flat_bound_cases(generator, metadata),
        "candidate_policy_saturation": candidate_policy_saturation(),
        "source_contract": source_contract(),
        "fixed_work_caps": {
            "tile_frames": TILE_FRAMES,
            "group_frames": GROUP_FRAMES,
            "max_nominees_per_tile_channel": MAX_NOMINEES,
            "max_finishers_per_tile_channel": MAX_FINISHERS,
            "max_additional_fine_knots_per_finisher": MAX_EXTRA_FINE_PER_FINISHER,
            "max_new_fine_knots_per_full_tile_channel": MAX_FINE_PER_TILE_CHANNEL,
        },
    }
    if not report["metadata_exact_child_checks"]:
        raise AssertionError("exact child metadata check missing")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"Fast066 qualification passed: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

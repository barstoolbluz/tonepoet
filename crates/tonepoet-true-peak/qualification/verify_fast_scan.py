#!/usr/bin/env python3
"""Offline source audit for the selective Fast066V2 implementation.

This qualifier deliberately does not claim Rust execution. It checks generated
metadata, frozen public/configuration markers, and source obligations that are
cheap to regress accidentally. Cargo tests remain the behavioral authority.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

CRATE_ROOT = Path(__file__).resolve().parents[1]
Q = CRATE_ROOT / "qualification"
SRC = CRATE_ROOT / "src"
FAST = SRC / "fast_scan.rs"
LIB = SRC / "lib.rs"
PREFIX = SRC / "qualified_half_delay_fft.rs"
RAW = SRC / "raw_screen_metadata.rs"
COEFF = SRC / "hq1024_coefficients.rs"
CARGO = CRATE_ROOT / "Cargo.toml"
FAST_TEST = CRATE_ROOT / "tests" / "fast_scan.rs"
METER_TEST = CRATE_ROOT / "tests" / "meter.rs"
BENCH = CRATE_ROOT / "examples" / "bench_ceiling_f64le.rs"
COMMISSION = Q / "commission_fast066_v2.py"

EXPECTED_COEFFICIENT_SHA256 = "7070c2e9abc255062dd30aaa516c0827d969d238759e14a61c5d1da94a67de9d"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(checks: dict[str, bool]) -> None:
    failed = [name for name, passed in checks.items() if not passed]
    if failed:
        raise AssertionError(f"selective Fast source audit failed: {failed}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Q / "fast066_verification.json")
    args = parser.parse_args()

    fast = FAST.read_text(encoding="utf-8")
    lib = LIB.read_text(encoding="utf-8")
    prefix = PREFIX.read_text(encoding="utf-8")
    raw = RAW.read_text(encoding="utf-8")
    cargo = CARGO.read_text(encoding="utf-8")
    tests = FAST_TEST.read_text(encoding="utf-8") + "\n" + METER_TEST.read_text(encoding="utf-8")
    bench = BENCH.read_text(encoding="utf-8")
    commission = COMMISSION.read_text(encoding="utf-8")
    metadata = json.loads((Q / "raw_screen_metadata.json").read_text(encoding="utf-8"))

    checks = {
        "frozen_algorithm_revision": 'FAST_ALGORITHM_REVISION: &str = "Fast066V2"' in lib,
        "frozen_wall_constant": "FAST_WALL_NANOS_PER_PROGRAMME_MINUTE: u64 = 660_000_000" in lib,
        "fast_interval_objective_stays_none": "Self::Fast => None" in lib,
        "cargo_feature_set_unchanged_shape": '[features]' in cargo and 'fast-stage-timing = []' in cargo,
        "sole_runtime_dependency_num_complex": cargo.count("num-complex") == 1,
        "raw_module_private": "mod raw_screen_metadata;" in lib and "pub mod raw_screen_metadata" not in lib,
        "raw_a_bits": "0x3ff0_2862_ce0a_81a1" in raw,
        "raw_b_bits": "0x3cce_9970_cbf1_2e12" in raw,
        "private_tolerance_bits": "0x3ff0_04b7_e9b5_ce5c" in raw,
        "raw_source_hash_documented": EXPECTED_COEFFICIENT_SHA256 in raw,
        "raw_halo_777": "const RAW_HALO_FRAMES: i128 = 777;" in fast,
        "tile_4096": "const TILE_INTERVALS: i128 = 4096;" in fast,
        "root_256": "const ROOT_INTERVALS: i128 = 256;" in fast,
        "child_32": "const CHILD_INTERVALS: i128 = 32;" in fast,
        "d_bins_32": "const D_SUMMARY_STARTS: usize = 32;" in fast,
        "fixed_dense_threshold": "const DENSE_HALF_KNOT_THRESHOLD: usize = 128;" in fast,
        "screen_fail_open_subnormal": "bits != 0 && bits < F64_MIN_NORMAL_BITS" in fast
            and "Fail the cheap screen open" in fast
            and "qualified = false" in fast,
        "screen_fail_open_unusable_bound": "if !e_d.is_finite()" in fast and "qualified = false" in fast,
        "strict_l4_raw_rejection": "root_upper <= self.channel_l4_lower[channel]" in fast
            and "child_upper <= self.channel_l4_lower[channel]" in fast,
        "separate_l4_lall": "channel_l4_lower" in fast and "channel_lower_peaks" in fast,
        "selective_symmetric_direct_fir": "fn direct_half_scalar" in fast
            and "HQ1024_HALF_DELAY_COEFFICIENTS" in fast
            and "m + 768" in fast and "m - 767" in fast,
        "sparse_direct_support_is_range_local": "let input_first = first - 767" in fast
            and "let input_last = last + 768" in fast
            and "Sparse work therefore never copies the full tile halo" in fast,
        "direct_3072_rounding_account": "DIRECT_HALF_ROUNDING_OPS: usize = 3072" in fast,
        "direct_extreme_rounding_account": "DIRECT_HALF_EXTREME_ROUNDING_OPS: usize = 6144" in fast,
        "midpoint_extreme_rounding_account": "MIDPOINT_EXTREME_ROUNDING_OPS: usize = 64" in fast,
        "tail_extreme_rounding_account": "TAIL_EXTREME_ROUNDING_OPS: usize = 128" in fast,
        "direct_avx_batch4": "fn direct_half_batch4_avx" in fast and '#[target_feature(enable = "avx")]' in fast,
        "finite_window_fft_adapter": "process_finite_window_blocks" in prefix
            and "reset_finite_window_state" in prefix,
        "finite_window_fft_boundary_regression":
            "finite_window_adapter_retained_edge_outputs_match_direct_fir" in prefix,
        "lazy_dense_fft": "dense_prefix: Option<QualifiedHalfDelayFft>" in fast
            and "get_or_insert_with" in fast,
        "local_hq4_survey": "fn build_local_survey" in fast,
        "local_flat_bound": "fn local_flat_upper" in fast,
        "uncapped_dyadic_resolver": "fn resolve_span" in fast
            and "ResolveNode" in fast
            and "MAX_NOMINEES_PER_TILE_CHANNEL" not in fast
            and "MAX_FINISHERS_PER_TILE_CHANNEL" not in fast
            and "MAX_FINE_EVALUATIONS_PER_TILE_CHANNEL" not in fast,
        "no_fast_clock": "Instant" not in fast and "SystemTime" not in fast,
        "retired_nomination_pipeline_removed": all(token not in fast for token in (
            "fn discover_candidates", "fn select_candidates", "fn probe_nominees", "fn finish_proposals",
        )),
        "dense_direct_tightening": "needs_direct_tightening" in fast
            and "cache.dense_source" in fast and "direct_rescore_evaluations" in fast,
        "commission_modes_equivalent_after_flat": "SurveyBoundsOnly | Self::NominationOnly" in fast,
        "transactional_push_validation": "Validate the complete caller push before any measurement state moves" in fast,
        "future_validation_peak_not_screen_witness": "validation_peaks" in fast
            and "channel_l4_lower" in fast,
        "canonical_chunk_commit": "within_tile" in fast and "until_boundary" in fast,
        "retired_fast_counters_not_written": all(token not in fast for token in (
            ".fast_candidates_observed =", ".fast_candidates_selected =", ".fast_proposals_evaluated =",
            ".fast_finishing_candidates =", ".fast_fine_knots_evaluated =",
        )),
        "benchmark_per_channel_width_gate": "fast_channel_widths_db" in bench
            and "fast_accuracy_target_met" in bench
            and "exceeded the private 0.01 dB per-channel certificate-width gate" in bench,
        "benchmark_silence_width_exception": "interval.lower_linear.to_bits() == 0" in bench
            and "interval.upper_linear.to_bits() == 0" in bench,
        "benchmark_commission_compat_label": '"survey-bounds-only-compat"' in bench,
        "commission_driver_compat_label": '"survey-bounds-only-compat"' in commission,
        "commission_driver_pins_v1_archive":
            "604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0" in commission,
        "commission_driver_runs_external_a": 'label="A"' in commission
            and 'expected_algorithm_revision="Fast066V1"' in commission,
        "commission_driver_distinct_abcd_labels": all(token in commission for token in (
            'label="A"', 'label="B"', 'label="C"', 'label="D-instrumented"', 'label="D-binding"',
        )),
        "commission_driver_strict_c_stop_go":
            "< FAST_TARGET_SECONDS_PER_PROGRAMME_MINUTE" in commission
            and '"profile_mandatory_path": not go' in commission,
        "commission_driver_separate_binding_target":
            'cargo-target-v2-instrumented' in commission
            and 'cargo-target-v2-binding' in commission,
        "commission_driver_captures_effective_rustc":
            '"-vv"' in commission and '"rustc_invocations": invocations' in commission,
        "commission_driver_per_record_source_build_identity": all(token in commission for token in (
            '"source_role": source_role', '"source_tree_sha256": source_tree_sha256',
            '"build_id": build_id',
        )),
        "commission_driver_explicit_v2_work_counters":
            'record["v2_work_counters"] = v2_work_counters(benchmark)' in commission
            and "strict_coarse_evaluations" in bench
            and "dense_phase_evaluations" in bench
            and "direct_rescore_evaluations" in bench,
        "commission_point_gate_matches_contract": "default=0.01" in commission
            and '"certificate_width": certificate_width_pass' in commission
            and "fast_accuracy_target_met" in commission,
        "analytical_point_accuracy_regression":
            "fast_point_meets_001_db_on_independently_known_windowed_multitone_peaks" in tests,
        "chunk_invariance_regression": "fast066_is_chunk_invariant_including_exact_tile_and_fft_boundaries" in tests,
        "future_push_regression": "fast066_pruning_ignores_future_validation_peak_in_the_same_caller_push" in tests,
        "subnormal_regression": "subnormal" in tests,
        "raw_all_phase_runtime_regression":
            "raw_screen_runtime_ranges_enclose_all_frozen_phases_and_support_edges" in fast,
        "raw_rejection_l4_witness_regression":
            "raw_screen_rejection_is_backed_by_the_same_channel_l4_witness" in fast,
        "pruned_hq4_complete_oracle_regression":
            "pruned_hq4_diagnostic_matches_independent_complete_hq4_survey" in fast,
        "direct_scalar_avx_oracle_regression":
            "direct_first_stage_scalar_and_avx_fit_the_declared_enclosure" in fast,
        "sparse_dense_same_span_regression":
            "sparse_and_dense_first_stage_cover_the_same_requested_span" in fast,
        "daz_ftz_fail_open_regression":
            "raw_screen_and_direct_enclosure_fail_closed_under_daz_ftz" in fast
            and ".map(magnitude_bits)" in fast
            and "if magnitude_bits(error) < F64_MIN_NORMAL_BITS" in fast,
        "parent_replacement_regression": "dyadic_child_bounds_replace_their_loose_parent" in fast,
        "retired_104_cap_regression":
            "mandatory_resolver_can_exceed_the_retired_104_evaluation_cap" in fast
            and "state.diagnostics.refined_cells > 104" in fast
            and "mandatory_resolution_uses_live_generic_counters_not_retired_v2_counters" in tests,
        "partial_clone_regression":
            "fast066_partial_clone_is_independent_of_completion_chunking" in tests,
        "short_boundary_chunk_regression":
            "fast066_short_and_tile_boundary_lengths_are_chunk_invariant" in tests,
        "dense_fixture_regression": "bundled_recording_meets_accuracy_floor_and_contains_dense_hq1024_target" in tests,
    }
    require(checks)

    if metadata.get("coefficient_source_sha256") != EXPECTED_COEFFICIENT_SHA256:
        raise AssertionError("checked-in raw metadata names the wrong frozen coefficient source")

    report = {
        "schema": "tonepoet-fast066-selective-source-audit-v1",
        "algorithm_revision": "Fast066V2",
        "coefficient_source_sha256": EXPECTED_COEFFICIENT_SHA256,
        "hq1024_coefficients_file_sha256": sha256(COEFF),
        "raw_screen_metadata_sha256": sha256(Q / "raw_screen_metadata.json"),
        "source_contract": {"passed": True, "checks": checks},
        "limitations": [
            "This source audit does not compile or execute Rust.",
            "Cargo tests and target-machine commissioning remain required release evidence.",
        ],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"selective Fast source audit passed: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

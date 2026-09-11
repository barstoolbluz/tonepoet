use std::env;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use tonepoet_true_peak::{
    CertifiedPeakMeter, EdgePolicy, PeakCertificate, PeakLevel, PeakTier,
    FAST_ALGORITHM_REVISION, FAST_WALL_NANOS_PER_PROGRAMME_MINUTE,
};
#[cfg(feature = "fast-stage-timing")]
use tonepoet_true_peak::FastCommissioningMode;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: bench_ceiling_f64le <path> <sample-rate-hz> <channels> <reference|standard|fast> (with fast-stage-timing: also fast-survey|fast-nominate)".into());
    }
    let sample_rate_hz: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let (tier, fast_configuration) = match args[4].as_str() {
        "reference" => (PeakTier::Reference, "not-fast"),
        "standard" => (PeakTier::Standard, "not-fast"),
        "fast" => (PeakTier::Fast, "production"),
        #[cfg(feature = "fast-stage-timing")]
        "fast-survey" => (PeakTier::Fast, "survey-bounds-only"),
        #[cfg(feature = "fast-stage-timing")]
        "fast-nominate" => (PeakTier::Fast, "survey-bounds-only-compat"),
        other => return Err(format!("unknown tier/configuration: {other}").into()),
    };

    let total_started = Instant::now();
    #[cfg(feature = "fast-stage-timing")]
    let mut meter = match args[4].as_str() {
        "fast-survey" => CertifiedPeakMeter::new_fast_commissioning(
            sample_rate_hz, channels, EdgePolicy::RepeatEndpoints,
            FastCommissioningMode::SurveyBoundsOnly,
        )?,
        "fast-nominate" => CertifiedPeakMeter::new_fast_commissioning(
            sample_rate_hz, channels, EdgePolicy::RepeatEndpoints,
            FastCommissioningMode::NominationOnly,
        )?,
        _ => CertifiedPeakMeter::new(
            sample_rate_hz, channels, EdgePolicy::RepeatEndpoints, tier,
        )?,
    };
    #[cfg(not(feature = "fast-stage-timing"))]
    let mut meter = CertifiedPeakMeter::new(
        sample_rate_hz, channels, EdgePolicy::RepeatEndpoints, tier,
    )?;
    let frame_bytes = channels.checked_mul(8).ok_or("frame size overflow")?;
    let buffer_bytes = (1024 * 1024 / frame_bytes).max(1) * frame_bytes;
    let mut file = File::open(&args[1])?;
    let file_bytes = file.metadata()?.len();
    if file_bytes % frame_bytes as u64 != 0 {
        return Err("f64le input length is not a whole number of frames".into());
    }
    let frames = file_bytes / frame_bytes as u64;
    let mut bytes = vec![0_u8; buffer_bytes];
    let mut samples = Vec::<f64>::with_capacity(buffer_bytes / 8);
    let mut remaining = file_bytes;
    #[cfg(feature = "fast-stage-timing")]
    let mut input_read_decode_nanos = 0_u64;

    let scan_started = Instant::now();
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer_bytes as u64))?;
        #[cfg(feature = "fast-stage-timing")]
        let input_started = Instant::now();
        file.read_exact(&mut bytes[..count])?;
        samples.clear();
        for raw in bytes[..count].chunks_exact(8) {
            samples.push(f64::from_le_bytes(raw.try_into().expect("8-byte f64")));
        }
        #[cfg(feature = "fast-stage-timing")]
        {
            let nanos = input_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            input_read_decode_nanos = input_read_decode_nanos.saturating_add(nanos);
        }
        meter.push_interleaved(&samples)?;
        remaining -= count as u64;
    }
    let certificate = meter.finalize()?;
    let scan_elapsed = scan_started.elapsed();
    let total_elapsed = total_started.elapsed();
    let scan_seconds = scan_elapsed.as_secs_f64();
    let total_seconds = total_elapsed.as_secs_f64();
    let programme_seconds = frames as f64 / f64::from(sample_rate_hz);
    let target_nanos = u128::from(frames)
        .checked_mul(u128::from(FAST_WALL_NANOS_PER_PROGRAMME_MINUTE))
        .ok_or("fast target duration overflow")?
        / (u128::from(sample_rate_hz) * 60);
    let target_seconds = target_nanos as f64 / 1_000_000_000.0;
    let fast_wall_target_met = tier != PeakTier::Fast || total_elapsed.as_nanos() <= target_nanos;
    let fast_channel_widths_db = certificate
        .channel_intervals
        .iter()
        .map(|interval| interval.width_db())
        .collect::<Vec<_>>();
    let fast_accuracy_target_met = tier != PeakTier::Fast
        || certificate
            .channel_intervals
            .iter()
            .zip(&fast_channel_widths_db)
            .all(|(interval, width)| {
                (interval.lower_linear.to_bits() == 0 && interval.upper_linear.to_bits() == 0)
                    || width.is_some_and(|value| value <= 0.01)
            });
    let binding_fast_wall_gate = !cfg!(feature = "fast-stage-timing");
    let binding_fast_accuracy_gate = !cfg!(feature = "fast-stage-timing");
    let build_metadata = build_metadata_json();
    #[cfg(feature = "fast-stage-timing")]
    let fast_stage_timing = fast_stage_timing_json(
        &certificate,
        input_read_decode_nanos,
        total_elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
    );
    #[cfg(not(feature = "fast-stage-timing"))]
    let fast_stage_timing = String::new();

    println!(
        "{{\"tier\":\"{:?}\",\"fast_algorithm_revision\":\"{}\",\"fast_configuration\":\"{}\",\"build\":{},\"bytes_read\":{},\"frames\":{},\"sample_rate_hz\":{},\"channels\":{},\"scan_wall_seconds\":{:.9},\"total_wall_seconds\":{:.9},\"programme_seconds\":{:.9},\"fast_target_nanos\":{},\"fast_target_seconds\":{:.9},\"fast_wall_target_met\":{},\"binding_fast_wall_gate\":{},\"fast_channel_widths_db\":{},\"fast_accuracy_target_met\":{},\"binding_fast_accuracy_gate\":{},\"point_dbtp\":{},\"ceiling_dbtp\":{},\"certificate\":{}{} }}",
        tier,
        FAST_ALGORITHM_REVISION,
        fast_configuration,
        build_metadata,
        file_bytes,
        frames,
        sample_rate_hz,
        channels,
        scan_seconds,
        total_seconds,
        programme_seconds,
        target_nanos,
        target_seconds,
        fast_wall_target_met,
        binding_fast_wall_gate,
        option_f64_vec_json(&fast_channel_widths_db),
        fast_accuracy_target_met,
        binding_fast_accuracy_gate,
        level_json(certificate.reported_point_estimate.overall),
        level_json(certificate.upper_level()),
        certificate_json(&certificate),
        fast_stage_timing,
    );
    if binding_fast_wall_gate && !fast_wall_target_met {
        return Err("fast tier exceeded 0.66 seconds of total wall time per minute of programme audio".into());
    }
    if binding_fast_accuracy_gate && !fast_accuracy_target_met {
        return Err("fast tier exceeded the private 0.01 dB per-channel certificate-width gate".into());
    }
    Ok(())
}


fn build_metadata_json() -> String {
    format!(
        "{{\"crate_version\":\"{}\",\"target_arch\":\"{}\",\"target_os\":\"{}\",\"debug_assertions\":{},\"fast_stage_timing_feature\":{}}}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(debug_assertions),
        cfg!(feature = "fast-stage-timing"),
    )
}

#[cfg(feature = "fast-stage-timing")]
fn fast_stage_timing_json(
    c: &PeakCertificate,
    input_read_decode_nanos: u64,
    total_wall_nanos: u64,
) -> String {
    let accounted = input_read_decode_nanos
        .saturating_add(c.diagnostics.fast_stage_prefix_block_ingest_nanos)
        .saturating_add(c.diagnostics.fast_stage_midpoint_survey_nanos)
        .saturating_add(c.diagnostics.fast_stage_flat_envelope_nanos)
        .saturating_add(c.diagnostics.fast_stage_candidate_refinement_nanos)
        .saturating_add(c.diagnostics.fast_stage_finalize_nanos);
    let unaccounted = total_wall_nanos.saturating_sub(accounted);
    format!(
        ",\"fast_stage_timing_nanos\":{{\"input_read_decode\":{},\"selective_first_stage\":{},\"hq4_midpoint_survey\":{},\"native_and_flat_envelope\":{},\"nomination\":{},\"proposal\":{},\"finishing\":{},\"mandatory_resolution\":{},\"final_reduction\":{},\"unaccounted_total_wall\":{}}}",
        input_read_decode_nanos,
        c.diagnostics.fast_stage_prefix_block_ingest_nanos,
        c.diagnostics.fast_stage_midpoint_survey_nanos,
        c.diagnostics.fast_stage_flat_envelope_nanos,
        c.diagnostics.fast_stage_nomination_nanos,
        c.diagnostics.fast_stage_proposal_nanos,
        c.diagnostics.fast_stage_finishing_nanos,
        c.diagnostics.fast_stage_candidate_refinement_nanos,
        c.diagnostics.fast_stage_finalize_nanos,
        unaccounted,
    )
}

fn certificate_json(c: &PeakCertificate) -> String {
    format!(
        "{{\"reconstruction\":\"{:?}\",\"tier\":\"{:?}\",\"status\":\"{:?}\",\"lower_linear\":{:.17e},\"upper_linear\":{:.17e},\"interval_width_db\":{},\"reconstruction_linf_gain_upper\":{:.17e},\"numerical_envelope_linear\":{:.17e},\"tiles_processed\":{},\"groups_rejected\":{},\"groups_expanded\":{},\"candidate_cells\":{},\"refined_cells\":{},\"phase_evaluations\":{},\"authoritative_coarse_values\":{},\"authoritative_coarse_groups\":{},\"accelerated_l1_groups_tested\":{},\"accelerated_l1_groups_rejected\":{},\"accelerated_curvature_roots\":{},\"accelerated_same_graph_avx_prefix_active\":{},\"strict_coarse_evaluations\":{},\"dense_regions\":{},\"dense_intermediate_cells\":{},\"dense_complete_regions\":{},\"dense_phase_evaluations\":{},\"direct_rescore_evaluations\":{},\"work_credits_consumed\":{},\"work_limited_tiles\":{},\"time_bounded_prefix_blocks_skipped\":{},\"time_limited_tiles\":{},\"fast_survey_knots\":{},\"fast_candidates_observed\":{},\"fast_flat_groups\":{},\"fast_candidates_selected\":{},\"fast_candidate_saturated_tiles\":{},\"fast_proposals_evaluated\":{},\"fast_proposal_fine_knots_evaluated\":{},\"fast_finishing_candidates\":{},\"fast_finishing_fine_knots_evaluated\":{},\"fast_fine_knots_evaluated\":{},\"fast_bound_pruned_channel_tiles\":{},\"fast_bound_pruned_nominees\":{},\"fast_bound_pruned_finishers\":{},\"fast_invalid_proposal_fits\":{},\"fast_unbracketed_finishers\":{},\"fast_invalid_finishing_fits\":{},\"fast_input_sample_peak_linear\":{:.17e},\"fast_hq4_peak_linear\":{:.17e},\"max_evaluation_error_linear\":{:.17e},\"unresolved_upper_linear\":{:.17e}}}",
        c.reconstruction, c.tier, c.status,
        c.finite_interval.lower_linear, c.finite_interval.upper_linear,
        option_f64_json(c.interval_width_db()), c.reconstruction_linf_gain_upper,
        c.numerical_envelope_linear, c.diagnostics.tiles_processed,
        c.diagnostics.groups_rejected, c.diagnostics.groups_expanded,
        c.diagnostics.candidate_cells, c.diagnostics.refined_cells,
        c.diagnostics.phase_evaluations,
        c.diagnostics.authoritative_coarse_values,
        c.diagnostics.authoritative_coarse_groups,
        c.diagnostics.accelerated_l1_groups_tested,
        c.diagnostics.accelerated_l1_groups_rejected, c.diagnostics.accelerated_curvature_roots,
        c.diagnostics.accelerated_same_graph_avx_prefix_active,
        c.diagnostics.strict_coarse_evaluations,
        c.diagnostics.dense_regions,
        c.diagnostics.dense_intermediate_cells,
        c.diagnostics.dense_complete_regions,
        c.diagnostics.dense_phase_evaluations,
        c.diagnostics.direct_rescore_evaluations,
        c.diagnostics.work_credits_consumed, c.diagnostics.work_limited_tiles,
        c.diagnostics.time_bounded_prefix_blocks_skipped,
        c.diagnostics.time_limited_tiles,
        c.diagnostics.fast_survey_knots,
        c.diagnostics.fast_candidates_observed,
        c.diagnostics.fast_flat_groups,
        c.diagnostics.fast_candidates_selected,
        c.diagnostics.fast_candidate_saturated_tiles,
        c.diagnostics.fast_proposals_evaluated,
        c.diagnostics.fast_proposal_fine_knots_evaluated,
        c.diagnostics.fast_finishing_candidates,
        c.diagnostics.fast_finishing_fine_knots_evaluated,
        c.diagnostics.fast_fine_knots_evaluated,
        c.diagnostics.fast_bound_pruned_channel_tiles,
        c.diagnostics.fast_bound_pruned_nominees,
        c.diagnostics.fast_bound_pruned_finishers,
        c.diagnostics.fast_invalid_proposal_fits,
        c.diagnostics.fast_unbracketed_finishers,
        c.diagnostics.fast_invalid_finishing_fits,
        c.diagnostics.fast_input_sample_peak_linear,
        c.diagnostics.fast_hq4_peak_linear,
        c.diagnostics.max_evaluation_error_linear,
        c.diagnostics.unresolved_upper_linear,
    )
}

fn option_f64_vec_json(values: &[Option<f64>]) -> String {
    let body = values
        .iter()
        .copied()
        .map(option_f64_json)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

fn option_f64_json(value: Option<f64>) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.12}"),
        Some(value) if value.is_infinite() && value.is_sign_positive() => "\"inf\"".to_string(),
        Some(_) => "\"-inf\"".to_string(),
        None => "null".to_string(),
    }
}

fn level_json(level: PeakLevel) -> String {
    match level {
        PeakLevel::Silence => "\"-inf\"".to_string(),
        PeakLevel::Finite { dbtp, .. } => format!("{dbtp:.12}"),
    }
}

use std::env;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use tonepoet_true_peak::{
    CertifiedPeakMeter, EdgePolicy, PeakCertificate, PeakLevel, PeakTier,
    FAST_WALL_SECONDS_PER_PROGRAMME_MINUTE,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: bench_ceiling_f64le <path> <sample-rate-hz> <channels> <reference|standard|fast>".into());
    }
    let sample_rate_hz: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let tier = match args[4].as_str() {
        "reference" => PeakTier::Reference,
        "standard" => PeakTier::Standard,
        "fast" => PeakTier::Fast,
        other => return Err(format!("unknown tier: {other}").into()),
    };

    let total_started = Instant::now();
    let mut meter = CertifiedPeakMeter::new(sample_rate_hz, channels, EdgePolicy::RepeatEndpoints, tier)?;
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

    let scan_started = Instant::now();
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer_bytes as u64))?;
        file.read_exact(&mut bytes[..count])?;
        samples.clear();
        for raw in bytes[..count].chunks_exact(8) {
            samples.push(f64::from_le_bytes(raw.try_into().expect("8-byte f64")));
        }
        meter.push_interleaved(&samples)?;
        remaining -= count as u64;
    }
    let certificate = meter.finalize()?;
    let scan_seconds = scan_started.elapsed().as_secs_f64();
    let total_seconds = total_started.elapsed().as_secs_f64();
    let programme_seconds = frames as f64 / f64::from(sample_rate_hz);
    let target_seconds = programme_seconds / 60.0 * FAST_WALL_SECONDS_PER_PROGRAMME_MINUTE as f64;
    let fast_wall_target_met = tier != PeakTier::Fast || total_seconds <= target_seconds;

    println!(
        "{{\"tier\":\"{:?}\",\"bytes_read\":{},\"frames\":{},\"sample_rate_hz\":{},\"channels\":{},\"scan_wall_seconds\":{:.9},\"total_wall_seconds\":{:.9},\"programme_seconds\":{:.9},\"fast_target_seconds\":{:.9},\"fast_wall_target_met\":{},\"point_dbtp\":{},\"ceiling_dbtp\":{},\"certificate\":{}}}",
        tier,
        file_bytes,
        frames,
        sample_rate_hz,
        channels,
        scan_seconds,
        total_seconds,
        programme_seconds,
        target_seconds,
        fast_wall_target_met,
        level_json(certificate.reported_point_estimate.overall),
        level_json(certificate.upper_level()),
        certificate_json(&certificate),
    );
    if !fast_wall_target_met {
        return Err("fast tier exceeded one second of total wall time per minute of programme audio".into());
    }
    Ok(())
}

fn certificate_json(c: &PeakCertificate) -> String {
    format!(
        "{{\"reconstruction\":\"{:?}\",\"tier\":\"{:?}\",\"status\":\"{:?}\",\"lower_linear\":{:.17e},\"upper_linear\":{:.17e},\"interval_width_db\":{},\"reconstruction_linf_gain_upper\":{:.17e},\"numerical_envelope_linear\":{:.17e},\"tiles_processed\":{},\"groups_rejected\":{},\"groups_expanded\":{},\"candidate_cells\":{},\"refined_cells\":{},\"phase_evaluations\":{},\"accelerated_l1_groups_tested\":{},\"accelerated_l1_groups_rejected\":{},\"accelerated_curvature_roots\":{},\"accelerated_same_graph_avx_prefix_active\":{},\"work_credits_consumed\":{},\"work_limited_tiles\":{},\"time_bounded_prefix_blocks_skipped\":{},\"time_limited_tiles\":{},\"unresolved_upper_linear\":{:.17e}}}",
        c.reconstruction, c.tier, c.status,
        c.finite_interval.lower_linear, c.finite_interval.upper_linear,
        option_f64_json(c.interval_width_db()), c.reconstruction_linf_gain_upper,
        c.numerical_envelope_linear, c.diagnostics.tiles_processed,
        c.diagnostics.groups_rejected, c.diagnostics.groups_expanded,
        c.diagnostics.candidate_cells, c.diagnostics.refined_cells,
        c.diagnostics.phase_evaluations, c.diagnostics.accelerated_l1_groups_tested,
        c.diagnostics.accelerated_l1_groups_rejected, c.diagnostics.accelerated_curvature_roots,
        c.diagnostics.accelerated_same_graph_avx_prefix_active,
        c.diagnostics.work_credits_consumed, c.diagnostics.work_limited_tiles,
        c.diagnostics.time_bounded_prefix_blocks_skipped,
        c.diagnostics.time_limited_tiles, c.diagnostics.unresolved_upper_linear,
    )
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

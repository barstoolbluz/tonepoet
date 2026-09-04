use std::env;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use tonepoet_true_peak::{
    EdgePolicy, HeadroomCeilingMeter, HeadroomScanMode, PeakLevel,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(
            "usage: bench_ceiling_f64le <path> <sample-rate-hz> <channels> <reference|fast|fastest>"
                .into(),
        );
    }
    let sample_rate_hz: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let scan_mode = match args[4].as_str() {
        "reference" => HeadroomScanMode::Reference,
        "fast" => HeadroomScanMode::Fast,
        "fastest" => HeadroomScanMode::Fastest,
        other => return Err(format!("unknown scan mode: {other}").into()),
    };
    let mut meter = HeadroomCeilingMeter::new_with_scan_mode(
        sample_rate_hz,
        channels,
        EdgePolicy::RepeatEndpoints,
        scan_mode,
    )?;

    let frame_bytes = channels
        .checked_mul(8)
        .ok_or("frame size overflow")?;
    if frame_bytes == 0 {
        return Err("channel count must be greater than zero".into());
    }
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

    // Match the production retained-carrier scan: file I/O and f64 decoding are
    // inside the interval because users pay for both in the real path. Read an
    // exact whole-frame chunk each iteration: Read::read is allowed to return a
    // short non-EOF read, which must not be mistaken for a truncated frame.
    let started = Instant::now();
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer_bytes as u64))
            .map_err(|_| "benchmark chunk size overflow")?;
        file.read_exact(&mut bytes[..count])?;
        samples.clear();
        for raw in bytes[..count].chunks_exact(8) {
            samples.push(f64::from_le_bytes(raw.try_into().expect("8-byte f64")));
        }
        meter.push_interleaved(&samples)?;
        remaining -= count as u64;
    }
    let result = meter.finalize()?;
    let elapsed = started.elapsed();
    let wall_seconds = elapsed.as_secs_f64();
    let programme_seconds = frames as f64 / f64::from(sample_rate_hz);
    let realtime = programme_seconds / wall_seconds;
    let wall_minutes_for_40_minute_album = 40.0 / realtime;

    println!(
        "{{\"mode\":\"{}\",\"frames\":{},\"sample_rate_hz\":{},\"channels\":{},\"wall_seconds\":{:.9},\"realtime\":{:.6},\"wall_minutes_for_40_minute_album\":{:.6},\"point_dbtp\":{},\"ceiling_dbtp\":{}}}",
        args[4],
        frames,
        sample_rate_hz,
        channels,
        wall_seconds,
        realtime,
        wall_minutes_for_40_minute_album,
        level_json(result.point_estimate.overall),
        level_json(result.reconstruction_upper),
    );
    Ok(())
}

fn level_json(level: PeakLevel) -> String {
    match level {
        PeakLevel::Silence => "\"-inf\"".to_string(),
        PeakLevel::Finite { dbtp, .. } => format!("{dbtp:.12}"),
    }
}

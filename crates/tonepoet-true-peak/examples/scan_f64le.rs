use std::env;
use std::fs::File;
use std::io::{Read, Result as IoResult};
use std::time::Instant;

use tonepoet_true_peak::{PeakLevel, TruePeakConfig, TruePeakMeter, TruePeakMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: scan_f64le <path> <sample-rate-hz> <channels> <reporting4x|headroom64x>".into());
    }
    let sample_rate_hz: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let mode = match args[4].as_str() {
        "reporting4x" => TruePeakMode::Reporting4x,
        "headroom64x" => TruePeakMode::Headroom64x,
        other => return Err(format!("unknown mode: {other}").into()),
    };
    let mut meter =
        TruePeakMeter::new(TruePeakConfig::new(sample_rate_hz, channels).with_mode(mode))?;

    // Time only the production-style streaming scan/finalize. Opening the file
    // and parsing command-line arguments stay outside the interval; file reads
    // themselves remain inside because retained-carrier I/O is part of the
    // production cost this example is intended to reproduce.
    let started = Instant::now();
    scan_file(&args[1], channels, &mut meter)?;
    let result = meter.finalize()?;
    let elapsed = started.elapsed();
    let wall_seconds = elapsed.as_secs_f64();
    let programme_seconds = result.frames as f64 / f64::from(sample_rate_hz);
    let realtime = programme_seconds / wall_seconds;
    let ns_per_original_frame_channel = elapsed.as_nanos() as f64
        / (result.frames as f64 * channels as f64);

    match result.overall {
        PeakLevel::Silence => println!(
            "{{\"frames\":{},\"sample_rate_hz\":{},\"channels\":{},\"wall_seconds\":{:.9},\"realtime\":{:.6},\"ns_per_original_frame_channel\":{:.6},\"linear\":0.0,\"dbtp\":\"-inf\"}}",
            result.frames,
            sample_rate_hz,
            channels,
            wall_seconds,
            realtime,
            ns_per_original_frame_channel,
        ),
        PeakLevel::Finite { linear, dbtp } => println!(
            "{{\"frames\":{},\"sample_rate_hz\":{},\"channels\":{},\"wall_seconds\":{:.9},\"realtime\":{:.6},\"ns_per_original_frame_channel\":{:.6},\"linear\":{linear:.17},\"dbtp\":{dbtp:.12}}}",
            result.frames,
            sample_rate_hz,
            channels,
            wall_seconds,
            realtime,
            ns_per_original_frame_channel,
        ),
    }
    Ok(())
}

fn scan_file(path: &str, channels: usize, meter: &mut TruePeakMeter) -> IoResult<()> {
    let mut file = File::open(path)?;
    let frame_bytes = channels
        .checked_mul(8)
        .ok_or_else(|| std::io::Error::other("frame size overflow"))?;
    let buffer_bytes = (1024 * 1024 / frame_bytes).max(1) * frame_bytes;
    let mut bytes = vec![0_u8; buffer_bytes];
    let mut carry = Vec::<u8>::new();
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        carry.extend_from_slice(&bytes[..count]);
        let complete = carry.len() / frame_bytes * frame_bytes;
        if complete == 0 {
            continue;
        }
        let mut samples = Vec::with_capacity(complete / 8);
        for raw in carry[..complete].chunks_exact(8) {
            samples.push(f64::from_le_bytes(raw.try_into().expect("8-byte f64")));
        }
        meter
            .push_interleaved(&samples)
            .map_err(std::io::Error::other)?;
        carry.drain(..complete);
    }
    if !carry.is_empty() {
        return Err(std::io::Error::other("truncated f64le frame at end of file"));
    }
    Ok(())
}

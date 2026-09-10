use std::env;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use tonepoet_true_peak::{PeakLevel, ReportingPeakMeter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: scan_f64le <path> <sample-rate-hz> <channels>".into());
    }
    let sample_rate_hz: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let mut meter = ReportingPeakMeter::new(sample_rate_hz, channels)?;
    let frame_bytes = channels.checked_mul(8).ok_or("frame size overflow")?;
    let buffer_bytes = (1024 * 1024 / frame_bytes).max(1) * frame_bytes;
    let mut file = File::open(&args[1])?;
    let file_bytes = file.metadata()?.len();
    if file_bytes % frame_bytes as u64 != 0 {
        return Err("f64le input length is not a whole number of frames".into());
    }
    let mut bytes = vec![0_u8; buffer_bytes];
    let mut samples = Vec::<f64>::with_capacity(buffer_bytes / 8);
    let mut remaining = file_bytes;
    let started = Instant::now();
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
    let result = meter.finalize()?;
    let wall_seconds = started.elapsed().as_secs_f64();
    let programme_seconds = result.frames as f64 / f64::from(sample_rate_hz);
    let realtime = programme_seconds / wall_seconds;
    match result.overall {
        PeakLevel::Silence => println!("{{\"profile\":\"reporting4x\",\"frames\":{},\"wall_seconds\":{wall_seconds:.9},\"realtime\":{realtime:.6},\"dbtp\":\"-inf\"}}", result.frames),
        PeakLevel::Finite { linear, dbtp } => println!("{{\"profile\":\"reporting4x\",\"frames\":{},\"wall_seconds\":{wall_seconds:.9},\"realtime\":{realtime:.6},\"linear\":{linear:.17},\"dbtp\":{dbtp:.12}}}", result.frames),
    }
    Ok(())
}

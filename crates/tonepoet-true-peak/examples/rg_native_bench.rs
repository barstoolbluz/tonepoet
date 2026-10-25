//! Read-only, bounded-memory benchmark of the EXACT native production CUE
//! ReplayGain meter configuration (NativeEbu2023 IntegratedOnly + reporting
//! peak). Uses the same block size (16_384 frames) and PCM conversion as
//! observe_cue_mirror. Does not invoke TonePoet or modify any audio artifact.
//!
//! cargo run -p tonepoet-true-peak --release --example rg_native_bench -- \
//!     INPUT.raw 44100 2 s32le both 3
//! modes: both, loudness, peak, decode. Run both first for the real workload.

use std::env;
use std::fs::File;
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

use tonepoet_true_peak::loudness::{
    ChannelRole, LoudnessMeter, LoudnessMetricDemand, LoudnessProfile,
};
use tonepoet_true_peak::ReportingPeakMeter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 7 {
        return Err("usage: rg_native_bench INPUT.raw RATE CHANNELS s32le|f32le|f64le both|loudness|peak|decode REPEATS".into());
    }
    let path = &args[1];
    let sample_rate: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;
    let width: usize = match args[4].as_str() {
        "s32le" | "f32le" => 4,
        "f64le" => 8,
        _ => return Err("encoding must be s32le, f32le or f64le".into()),
    };
    let mode = args[5].as_str();
    if !matches!(mode, "both" | "loudness" | "peak" | "decode") {
        return Err("mode must be both, loudness, peak or decode".into());
    }
    let repeats: usize = args[6].parse()?;
    if !(1..=20).contains(&repeats) {
        return Err("REPEATS must be 1..=20".into());
    }
    let roles = match channels {
        1 => vec![ChannelRole::Mono],
        2 => vec![ChannelRole::Left, ChannelRole::Right],
        _ => return Err("production CUE source-pass admits mono and stereo only".into()),
    };
    let frame_bytes = width.checked_mul(channels).ok_or("frame width overflow")?;
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() == 0 || before.len() % frame_bytes as u64 != 0 {
        return Err("input must be a nonempty, frame-aligned regular PCM file".into());
    }
    let frames = before.len() / frame_bytes as u64;
    let n_per_chunk = 16_384usize.checked_mul(channels).ok_or("chunk overflow")?;
    let chunk_bytes = 16_384usize.checked_mul(frame_bytes).ok_or("chunk overflow")?;
    let mut raw = vec![0u8; chunk_bytes];
    let mut samples: Vec<f64> = Vec::with_capacity(n_per_chunk);

    for round in 1..=repeats {
        file.seek(SeekFrom::Start(0))?;
        let mut loudness = if matches!(mode, "both" | "loudness") {
            Some(LoudnessMeter::with_roles_and_metric_demand_and_limit(
                sample_rate, &roles, LoudnessProfile::NativeEbu2023,
                LoudnessMetricDemand::IntegratedOnly, 256 * 1024 * 1024,
            )?)
        } else { None };
        let mut peak = if matches!(mode, "both" | "peak") {
            Some(ReportingPeakMeter::new(sample_rate, channels)?)
        } else { None };
        let mut remaining = before.len();
        let mut read_ns = 0u128;
        let mut decode_ns = 0u128;
        let mut loudness_ns = 0u128;
        let mut peak_ns = 0u128;
        let total = Instant::now();
        while remaining > 0 {
            let take = usize::try_from(remaining.min(chunk_bytes as u64))?;
            let t = Instant::now();
            file.read_exact(&mut raw[..take])?;
            read_ns += t.elapsed().as_nanos();
            samples.clear();
            let t = Instant::now();
            for b in raw[..take].chunks_exact(width) {
                let value = match args[4].as_str() {
                    "s32le" => f64::from(i32::from_le_bytes(b.try_into()?)) / 2147483648.0,
                    "f32le" => f64::from(f32::from_le_bytes(b.try_into()?)),
                    "f64le" => f64::from_le_bytes(b.try_into()?),
                    _ => unreachable!("validated encoding"),
                };
                samples.push(value);
            }
            decode_ns += t.elapsed().as_nanos();
            if let Some(meter) = loudness.as_mut() {
                let t = Instant::now();
                meter.push_interleaved(black_box(&samples))?;
                loudness_ns += t.elapsed().as_nanos();
            }
            if let Some(meter) = peak.as_mut() {
                let t = Instant::now();
                meter.push_interleaved(black_box(&samples))?;
                peak_ns += t.elapsed().as_nanos();
            }
            remaining -= take as u64;
        }
        let mut lufs_bits = "null".to_owned();
        if let Some(meter) = loudness {
            let t = Instant::now();
            let result = meter.finalize()?;
            loudness_ns += t.elapsed().as_nanos();
            if result.summary.real_frames != frames {
                return Err("loudness frame count mismatch".into());
            }
            if let Some(value) = result.summary.integrated.finite_lufs() {
                lufs_bits = format!("\"{:016x}\"", black_box(value.to_bits()));
            }
        }
        let mut peak_bits = "null".to_owned();
        if let Some(meter) = peak {
            let t = Instant::now();
            let result = meter.finalize()?;
            peak_ns += t.elapsed().as_nanos();
            if result.frames != frames {
                return Err("peak frame count mismatch".into());
            }
            peak_bits = format!("\"{:016x}\"", black_box(result.overall.linear().to_bits()));
        }
        let total_ns = total.elapsed().as_nanos();
        let after = file.metadata()?;
        if after.len() != before.len() || after.modified()? != before.modified()? {
            return Err("input size or modification time changed during benchmark".into());
        }
        // Integers are emitted as exact JSON integers, never floats. The
        // subprocess's own process CPU can be sampled by run_host.py.
        println!(
            "{{\"schema\":\"tonepoet.rg_native_bench.v1\",\"round\":{},\"mode\":\"{}\",\"encoding\":\"{}\",\"sample_rate_hz\":{},\"channels\":{},\"frames\":{},\"input_bytes\":{},\"wall_ns\":{},\"read_ns\":{},\"decode_ns\":{},\"loudness_ns\":{},\"peak_ns\":{},\"lufs_bits\":{},\"peak_linear_bits\":{}}}",
            round, mode, args[4], sample_rate, channels, frames, before.len(),
            total_ns, read_ns, decode_ns, loudness_ns, peak_ns, lufs_bits, peak_bits,
        );
    }
    Ok(())
}

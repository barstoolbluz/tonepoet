// Differential harness: run the crate's libebur128-1.2.6-compatible loudness
// profile on raw f64le PCM and print integrated LUFS + LRA as JSON, for
// comparison against loudgain 0.6.8. Not shipped; validation only.
use std::env;
use std::fs::File;
use std::io::Read;

use tonepoet_true_peak::loudness::{LoudnessMeter, LoudnessProfile};
use tonepoet_true_peak::ReportingPeakMeter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        return Err("usage: loudness_f64le <path.f64le> <sample_rate_hz> <channels> <libebur128|native>".into());
    }
    let sample_rate: u32 = args[2].parse()?;
    let channels: usize = args[3].parse()?;

    let mut meter = match args[4].as_str() {
        "libebur128" => LoudnessMeter::libebur128_126_default_layout(sample_rate, channels)?,
        "native" => {
            let roles: Vec<_> = match channels {
                1 => vec![tonepoet_true_peak::loudness::ChannelRole::Mono],
                2 => vec![
                    tonepoet_true_peak::loudness::ChannelRole::Left,
                    tonepoet_true_peak::loudness::ChannelRole::Right,
                ],
                _ => return Err("native harness supports mono/stereo".into()),
            };
            LoudnessMeter::with_roles(sample_rate, &roles, LoudnessProfile::NativeEbu2023)?
        }
        other => return Err(format!("unknown profile: {other}").into()),
    };

    let frame_bytes = channels.checked_mul(8).ok_or("frame overflow")?;
    let buffer_bytes = (1024 * 1024 / frame_bytes).max(1) * frame_bytes;
    let mut file = File::open(&args[1])?;
    let file_bytes = file.metadata()?.len();
    if file_bytes % frame_bytes as u64 != 0 {
        return Err("f64le length is not a whole number of frames".into());
    }
    // Also run the libebur128-compatible true-peak meter on the SAME samples,
    // timed separately, so we can report a full loudness+peak in-process cost.
    let mut peak = ReportingPeakMeter::new(sample_rate, channels)?;

    let mut bytes = vec![0u8; buffer_bytes];
    let mut samples: Vec<f64> = Vec::with_capacity(buffer_bytes / 8);
    let mut remaining = file_bytes;
    let mut meter_nanos: u128 = 0;
    let mut peak_nanos: u128 = 0;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer_bytes as u64))?;
        file.read_exact(&mut bytes[..count])?;
        samples.clear();
        for raw in bytes[..count].chunks_exact(8) {
            samples.push(f64::from_le_bytes(raw.try_into().expect("8-byte f64")));
        }
        let t = std::time::Instant::now();
        meter.push_interleaved(&samples)?;
        meter_nanos += t.elapsed().as_nanos();
        let t = std::time::Instant::now();
        peak.push_interleaved(&samples)?;
        peak_nanos += t.elapsed().as_nanos();
        remaining -= count as u64;
    }

    let t = std::time::Instant::now();
    let m = meter.finalize()?;
    meter_nanos += t.elapsed().as_nanos();
    let t = std::time::Instant::now();
    let _peak = peak.finalize()?;
    peak_nanos += t.elapsed().as_nanos();
    let lufs = m.summary.integrated.finite_lufs();
    let lra = m.summary.range.finite_lu();
    let f = |v: Option<f64>| v.map(|x| format!("{x:.6}")).unwrap_or_else(|| "null".to_string());
    let programme_s = m.summary.real_frames as f64 / f64::from(sample_rate);
    let meter_s = meter_nanos as f64 / 1e9;
    let peak_s = peak_nanos as f64 / 1e9;
    let ppm = |s: f64| s / programme_s * 60.0;
    println!(
        "{{\"integrated_lufs\":{}, \"lra_lu\":{}, \"programme_seconds\":{:.1}, \"loudness_s_per_min\":{:.4}, \"peak_s_per_min\":{:.4}, \"loudness_plus_peak_s_per_min\":{:.4}}}",
        f(lufs),
        f(lra),
        programme_s,
        ppm(meter_s),
        ppm(peak_s),
        ppm(meter_s + peak_s)
    );
    Ok(())
}

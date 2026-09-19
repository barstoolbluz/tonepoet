// Qualification timing harness for the private loudness SIMD backends.
//
// Runs the NativeEbu2023 loudness meter over a deterministic synthetic
// programme with one pinned backend and prints wall time plus the result
// bits, so paired scalar/SSE2/AVX rounds can be compared for speed and
// bit identity. Not shipped; validation only.
//
//   loudness_simd_timing <scalar|sse2|avx> <channels> <programme_seconds> [sample_rate_hz] [seed]
use std::env;
use std::time::Instant;

use tonepoet_true_peak::loudness::{
    ChannelRole, LoudnessMeter, LoudnessProfile, LoudnessSimdBackend,
};

fn roles(channels: usize) -> Result<Vec<ChannelRole>, String> {
    use ChannelRole::*;
    Ok(match channels {
        1 => vec![Mono],
        2 => vec![Left, Right],
        6 => vec![Left, Right, Center, Lfe, LeftSurround, RightSurround],
        8 => vec![
            Left,
            Right,
            Center,
            Lfe,
            LeftSurround,
            RightSurround,
            LeftBack,
            RightBack,
        ],
        other => {
            return Err(format!(
                "unsupported channel count {other}; use 1, 2, 6, or 8"
            ))
        }
    })
}

/// SplitMix64: deterministic, cheap, good enough for a programme-shaped
/// noise-plus-tones fixture. The fixture only has to be identical across
/// backends and rounds.
struct SplitMix(u64);
impl SplitMix {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 || args.len() > 6 {
        return Err(
            "usage: loudness_simd_timing <scalar|sse2|avx> <channels> <programme_seconds> [sample_rate_hz] [seed]"
                .into(),
        );
    }
    let backend = match args[1].as_str() {
        "scalar" => LoudnessSimdBackend::Scalar,
        "sse2" => LoudnessSimdBackend::Sse2,
        "avx" => LoudnessSimdBackend::Avx,
        other => return Err(format!("unknown backend: {other}").into()),
    };
    let channels: usize = args[2].parse()?;
    let programme_seconds: u64 = args[3].parse()?;
    let sample_rate: u32 = args
        .get(4)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(48_000);
    let seed: u64 = args
        .get(5)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(0x5EED_2026_09_19);

    let roles = roles(channels)?;
    let mut meter = LoudnessMeter::with_roles(sample_rate, &roles, LoudnessProfile::NativeEbu2023)?;
    meter.force_simd_backend_for_qualification(backend)?;

    // Programme: per-channel tone at a distinct frequency plus noise, with a
    // slow amplitude envelope so gating admits and rejects blocks.
    let total_frames = programme_seconds * u64::from(sample_rate);
    let block_frames = 4096usize;
    let mut block = vec![0f64; block_frames * channels];
    let mut rng = SplitMix(seed);
    let two_pi = std::f64::consts::TAU;
    let mut frame_index: u64 = 0;
    let mut remaining = total_frames;
    let mut nanos: u128 = 0;
    while remaining != 0 {
        let frames = usize::try_from(remaining.min(block_frames as u64))?;
        for f in 0..frames {
            let t = (frame_index + f as u64) as f64 / f64::from(sample_rate);
            let envelope = 0.55 + 0.45 * (two_pi * t / 7.3).sin();
            for c in 0..channels {
                let tone = (two_pi * (220.0 + 137.0 * c as f64) * t).sin();
                block[f * channels + c] = 0.25 * envelope * (0.7 * tone + 0.3 * rng.next_f64());
            }
        }
        let start = Instant::now();
        meter.push_interleaved(&block[..frames * channels])?;
        nanos += start.elapsed().as_nanos();
        frame_index += frames as u64;
        remaining -= frames as u64;
    }
    let start = Instant::now();
    let measurement = meter.finalize()?;
    nanos += start.elapsed().as_nanos();

    let summary = measurement.summary;
    let bits = |v: Option<f64>| {
        v.map(|x| format!("\"{:016x}\"", x.to_bits()))
            .unwrap_or_else(|| "null".into())
    };
    let num = |v: Option<f64>| {
        v.map(|x| format!("{x:.6}"))
            .unwrap_or_else(|| "null".into())
    };
    println!(
        "{{\"backend\":\"{}\",\"channels\":{},\"programme_seconds\":{},\"sample_rate_hz\":{},\"nanos\":{},\"integrated_lufs\":{},\"lra_lu\":{},\"integrated_bits\":{},\"lra_bits\":{},\"integrated_observations\":{},\"lra_observations\":{},\"real_frames\":{}}}",
        args[1],
        channels,
        programme_seconds,
        sample_rate,
        nanos,
        num(summary.integrated.finite_lufs()),
        num(summary.range.finite_lu()),
        bits(summary.integrated.finite_lufs()),
        bits(summary.range.finite_lu()),
        summary.absolute_integrated_observations,
        summary.absolute_lra_observations,
        summary.real_frames,
    );
    Ok(())
}

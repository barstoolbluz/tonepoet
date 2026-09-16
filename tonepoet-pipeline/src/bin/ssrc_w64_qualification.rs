use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use tonepoet_pipeline::{
    copy_exact_w64_pcm_payload, W64PcmFormatExpectation, W64SampleEncoding,
};

fn parse_u32(value: &std::ffi::OsStr, label: &str) -> Result<u32, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not valid UTF-8"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid {label}: {error}"))
}

fn parse_u16(value: &std::ffi::OsStr, label: &str) -> Result<u16, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not valid UTF-8"))?
        .parse::<u16>()
        .map_err(|error| format!("invalid {label}: {error}"))
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(args.next().ok_or_else(|| {
        "usage: ssrc_w64_qualification <input.w64> <output.f64le> <rate_hz> <channels>".to_owned()
    })?);
    let output = PathBuf::from(args.next().ok_or_else(|| "missing output path".to_owned())?);
    let rate = parse_u32(
        &args.next().ok_or_else(|| "missing rate_hz".to_owned())?,
        "rate_hz",
    )?;
    let channels = parse_u16(
        &args.next().ok_or_else(|| "missing channels".to_owned())?,
        "channels",
    )?;
    if args.next().is_some() {
        return Err("too many arguments".to_owned());
    }

    let mut input_file =
        File::open(&input).map_err(|error| format!("could not open {}: {error}", input.display()))?;
    let mut output_file = File::create(&output)
        .map_err(|error| format!("could not create {}: {error}", output.display()))?;
    let structure = copy_exact_w64_pcm_payload(
        &mut input_file,
        &mut output_file,
        W64PcmFormatExpectation {
            sample_rate_hz: rate,
            channels,
            bits_per_sample: 64,
            encoding: W64SampleEncoding::FloatingPoint,
        },
    )
    .map_err(|error| format!("Tonepoet exact W64 validation/payload bridge failed: {error}"))?;
    output_file
        .flush()
        .map_err(|error| format!("could not flush {}: {error}", output.display()))?;
    output_file
        .sync_all()
        .map_err(|error| format!("could not sync {}: {error}", output.display()))?;

    println!(
        "{{\"sample_frames\":{},\"declared_data_bytes\":{},\"sample_rate_hz\":{},\"channels\":{},\"bits_per_sample\":64,\"encoding\":\"floating_point\"}}",
        structure.sample_frames, structure.declared_data_bytes, rate, channels,
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

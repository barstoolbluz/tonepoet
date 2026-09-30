//! Qualification helper for the certified SSRC true-peak terminal.
//!
//! The default two-argument mode delegates dB -> binary64 conversion and matrix
//! rendering to production code so the external qualification harness never
//! reimplements the hard-ceiling scalar authority in Python.
//!
//! `--inspect-ssrc-w64` delegates SSRC-specific Wave64 admission to the same
//! production validator used by conversion. The Python harness may still decode
//! already-admitted PCM bytes, but it does not own the container exception.

use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;

use tonepoet_pipeline::{
    inspect_ssrc_w64_pcm, W64PcmFormatExpectation, W64SampleEncoding,
};

fn usage() -> ! {
    eprintln!(
        "usage:\n  ssrc_true_peak_gain_qualification <gain-db> <channels>\n  \
ssrc_true_peak_gain_qualification --inspect-ssrc-w64 <input.w64> <rate_hz> <channels> <bits> <signed_integer|floating_point>"
    );
    std::process::exit(2);
}

fn parse_u32(value: &OsStr, label: &str) -> Result<u32, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not valid UTF-8"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid {label}: {error}"))
}

fn parse_u16(value: &OsStr, label: &str) -> Result<u16, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not valid UTF-8"))?
        .parse::<u16>()
        .map_err(|error| format!("invalid {label}: {error}"))
}

fn inspect_ssrc_w64(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let input = PathBuf::from(args.next().ok_or_else(|| "missing input path".to_owned())?);
    let rate = parse_u32(
        &args.next().ok_or_else(|| "missing rate_hz".to_owned())?,
        "rate_hz",
    )?;
    let channels = parse_u16(
        &args.next().ok_or_else(|| "missing channels".to_owned())?,
        "channels",
    )?;
    let bits = parse_u16(
        &args.next().ok_or_else(|| "missing bits".to_owned())?,
        "bits",
    )?;
    let encoding_raw = args.next().ok_or_else(|| "missing encoding".to_owned())?;
    let encoding = match encoding_raw.to_str() {
        Some("signed_integer") => W64SampleEncoding::SignedInteger,
        Some("floating_point") => W64SampleEncoding::FloatingPoint,
        Some(other) => return Err(format!("unsupported encoding {other:?}")),
        None => return Err("encoding is not valid UTF-8".to_owned()),
    };
    if args.next().is_some() {
        return Err("too many --inspect-ssrc-w64 arguments".to_owned());
    }

    let mut file = File::open(&input)
        .map_err(|error| format!("could not open {}: {error}", input.display()))?;
    let structure = inspect_ssrc_w64_pcm(
        &mut file,
        W64PcmFormatExpectation {
            sample_rate_hz: rate,
            channels,
            bits_per_sample: bits,
            encoding,
        },
    )
    .map_err(|error| format!("Tonepoet SSRC Wave64 validation failed: {error}"))?;
    let data_end = structure
        .data_payload_offset()
        .checked_add(structure.declared_data_bytes)
        .ok_or_else(|| "validated data extent overflowed u64".to_owned())?;
    let trailing_bytes_after_data = structure
        .physical_file_bytes
        .checked_sub(data_end)
        .ok_or_else(|| "validated data extent exceeds physical file".to_owned())?;

    println!(
        "{{\"sample_frames\":{},\"declared_data_bytes\":{},\"physical_file_bytes\":{},\"trailing_bytes_after_data\":{}}}",
        structure.sample_frames,
        structure.declared_data_bytes,
        structure.physical_file_bytes,
        trailing_bytes_after_data,
    );
    Ok(())
}

fn render_gain(gain_raw: String, channels_raw: String) -> Result<(), String> {
    let gain = gain_raw
        .parse::<tonepoet_pipeline::DbNano>()
        .map_err(|error| format!("invalid gain: {error}"))?;
    let channels = channels_raw
        .parse::<u16>()
        .map_err(|error| format!("invalid channel count: {error}"))?;
    let matrix = tonepoet_pipeline::ssrc_true_peak_gain_matrix(gain, channels)
        .map_err(|error| format!("could not render matrix: {error}"))?;
    println!("{matrix}");
    Ok(())
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let first = args.next().unwrap_or_else(|| usage());
    if first.as_os_str() == OsStr::new("--inspect-ssrc-w64") {
        return inspect_ssrc_w64(args);
    }

    let gain_raw = first
        .into_string()
        .map_err(|_| "gain is not valid UTF-8".to_owned())?;
    let channels_raw = args
        .next()
        .unwrap_or_else(|| usage())
        .into_string()
        .map_err(|_| "channel count is not valid UTF-8".to_owned())?;
    if args.next().is_some() {
        usage();
    }
    render_gain(gain_raw, channels_raw)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

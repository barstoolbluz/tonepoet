//! Manual byte-comparison harness for sacd-rs vs sacd-extract.
//!
//! Usage:
//!
//! ```text
//! cargo run --example extract_track --release -- \
//!     --iso /path/to/album.iso \
//!     --start-lsn 12345 \
//!     --end-lsn 23456 \
//!     --channels 2 \
//!     --format dff \
//!     --output /tmp/track.dff
//! ```
//!
//! Find the LSN range for a track by running `sacd-extract --verbose
//! ...` and reading the per-track start_lsn + sector_count from its
//! log output, then compute `end_lsn = start_lsn + sector_count`.

use sacd_rs::extract::{extract_track, OutputFormat};
use sacd_rs::iso_reader::IsoReader;
use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    iso: PathBuf,
    output: PathBuf,
    start_lsn: u64,
    end_lsn: u64,
    channels: u8,
    format: OutputFormat,
}

fn print_usage() {
    eprintln!(
        "usage: extract_track --iso PATH --start-lsn N --end-lsn N \\
                      --channels N --format dsf|dff --output PATH"
    );
}

fn parse_args() -> Result<Args, String> {
    let mut iso = None;
    let mut output = None;
    let mut start_lsn = None;
    let mut end_lsn = None;
    let mut channels = None;
    let mut format = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("missing value for {}", flag));
        match flag.as_str() {
            "--iso" => iso = Some(PathBuf::from(val()?)),
            "--output" | "-o" => output = Some(PathBuf::from(val()?)),
            "--start-lsn" => {
                start_lsn = Some(val()?.parse::<u64>().map_err(|e| e.to_string())?);
            }
            "--end-lsn" => {
                end_lsn = Some(val()?.parse::<u64>().map_err(|e| e.to_string())?);
            }
            "--channels" => {
                channels = Some(val()?.parse::<u8>().map_err(|e| e.to_string())?);
            }
            "--format" => {
                format = Some(match val()?.as_str() {
                    "dsf" => OutputFormat::Dsf,
                    "dff" | "dsdiff" => OutputFormat::Dff,
                    other => return Err(format!("unknown format: {}", other)),
                });
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
    }

    Ok(Args {
        iso: iso.ok_or("--iso required")?,
        output: output.ok_or("--output required")?,
        start_lsn: start_lsn.ok_or("--start-lsn required")?,
        end_lsn: end_lsn.ok_or("--end-lsn required")?,
        channels: channels.ok_or("--channels required")?,
        format: format.ok_or("--format required")?,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {}", e);
            print_usage();
            return ExitCode::from(2);
        }
    };

    let mut iso = match IsoReader::open(&args.iso) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("failed to open {}: {}", args.iso.display(), e);
            return ExitCode::FAILURE;
        }
    };

    let mut output_file = match File::create(&args.output) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to create {}: {}", args.output.display(), e);
            return ExitCode::FAILURE;
        }
    };

    match extract_track(
        &mut iso,
        &mut output_file,
        args.start_lsn,
        args.end_lsn,
        args.channels,
        args.format,
    ) {
        Ok(stats) => {
            println!(
                "wrote {} ({} frames, {} audio bytes)",
                args.output.display(),
                stats.frames_read,
                stats.audio_bytes,
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("extraction failed: {}", e);
            eprintln!("(output file at {} is incomplete; discard it)", args.output.display());
            ExitCode::FAILURE
        }
    }
}

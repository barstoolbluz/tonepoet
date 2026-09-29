//! Qualification helper for the certified SSRC true-peak gain matrix.
//!
//! This binary deliberately delegates dB -> binary64 conversion and matrix
//! rendering to production code so the external qualification harness never
//! reimplements the hard-ceiling scalar authority in Python.

use std::env;
use std::process::ExitCode;

fn usage() -> ! {
    eprintln!("usage: ssrc_true_peak_gain_qualification <gain-db> <channels>");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(gain_raw) = args.next() else { usage() };
    let Some(channels_raw) = args.next() else { usage() };
    if args.next().is_some() {
        usage();
    }

    let gain = match gain_raw.parse::<tonepoet_pipeline::DbNano>() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid gain: {error}");
            return ExitCode::from(2);
        }
    };
    let channels = match channels_raw.parse::<u16>() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid channel count: {error}");
            return ExitCode::from(2);
        }
    };
    match tonepoet_pipeline::ssrc_true_peak_gain_matrix(gain, channels) {
        Ok(matrix) => {
            println!("{matrix}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not render matrix: {error}");
            ExitCode::from(1)
        }
    }
}

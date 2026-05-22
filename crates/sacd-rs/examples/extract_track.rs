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
//! Two valid usage modes:
//!
//! **Pre-trimmed mode** (default): pass the per-track SACDTRL1
//! range from tonepoet's `examples/dump_sacd_lsn` (which prints
//! `TrackEntry.start_lsn` + length). No `--time-filter-*` args.
//!
//! **Wide-range mode** (sacd_extract-faithful): pass the wider
//! `area_toc.track_start..track_start_lsn[next]` range plus
//! `--time-filter-start MM:SS:FF --time-filter-duration MM:SS:FF`
//! from SACDTRL2. Matches sacd_extract's default
//! `audio_frame_trimming=1` behavior.
//!
//! Both modes produce sacd_extract-default-equivalent audio.

use sacd_rs::dff_footer::DffMetadata;
use sacd_rs::extract::{extract_track, ExtractOptions, OutputFormat, TimeFilter};
use sacd_rs::id3::Id3Metadata;
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
    time_filter: Option<TimeFilter>,
    id3: Id3Metadata,
    dff_footer_opts: DffFooterOptions,
}

/// Collected from --dff-* CLI flags. Combined with `--id3-*` flags
/// to construct a full DffMetadata if any field is populated.
#[derive(Default)]
struct DffFooterOptions {
    diar: Option<String>,
    diti: Option<String>,
    duration_minutes_total: Option<u32>,
    duration_seconds: Option<u8>,
    duration_frames: Option<u8>,
    disc_date_year: Option<u16>,
    disc_date_month_1_indexed: Option<u8>,
    disc_date_day: Option<u8>,
    disc_or_album_title: Option<String>,
    creation_year: Option<u16>,
    creation_month_0_indexed: Option<u8>,
    creation_day: Option<u8>,
    creation_hour: Option<u8>,
    creation_minute: Option<u8>,
    creating_machine: Option<String>,
}

fn print_usage() {
    eprintln!(
        "usage: extract_track --iso PATH --start-lsn N --end-lsn N \\
                      --channels N --format dsf|dff --output PATH \\
                      [--time-filter-start MM:SS:FF --time-filter-duration MM:SS:FF] \\
                      [--id3-title S] [--id3-album S] [--id3-artist S] \\
                      [--id3-album-artist S] [--id3-performer S] [--id3-composer S] \\
                      [--id3-isrc S] [--id3-publisher S] [--id3-copyright S] \\
                      [--id3-disc N/M] [--id3-genre S] [--id3-year YYYY] \\
                      [--id3-date MMDD] [--id3-track N/M]"
    );
}

/// Parse \"N/M\" → (N, M) as u16 pair.
fn parse_pair_u16(s: &str) -> Result<(u16, u16), String> {
    let (a, b) = s
        .split_once('/')
        .ok_or_else(|| format!("expected N/M, got {:?}", s))?;
    let n: u16 = a.parse().map_err(|_| format!("bad first u16: {}", a))?;
    let m: u16 = b.parse().map_err(|_| format!("bad second u16: {}", b))?;
    Ok((n, m))
}

/// Parse "MMDD" → (M, D) as u8 pair. SMPTE-style fields not enforced
/// (caller passes whatever the SACD's master_toc gives).
fn parse_mmdd(s: &str) -> Result<(u8, u8), String> {
    if s.len() != 4 {
        return Err(format!("expected MMDD (4 chars), got {:?}", s));
    }
    let m: u8 = s[..2]
        .parse()
        .map_err(|_| format!("bad month: {}", &s[..2]))?;
    let d: u8 = s[2..]
        .parse()
        .map_err(|_| format!("bad day: {}", &s[2..]))?;
    Ok((m, d))
}

/// Parse a `MM:SS:FF` timecode into a total 75fps frame count.
/// Validates SS ∈ [0, 60) and FF ∈ [0, 75). Handles arbitrarily
/// large MM (parses as u32 directly, not u8).
fn parse_mmss_ff(s: &str) -> Result<u32, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected MM:SS:FF, got {:?}", s));
    }
    let m: u32 = parts[0]
        .parse()
        .map_err(|_| format!("bad minutes: {}", parts[0]))?;
    let sec: u32 = parts[1]
        .parse()
        .map_err(|_| format!("bad seconds: {}", parts[1]))?;
    let f: u32 = parts[2]
        .parse()
        .map_err(|_| format!("bad frames: {}", parts[2]))?;
    if sec >= 60 {
        return Err(format!("seconds out of range: {}", sec));
    }
    if f >= 75 {
        return Err(format!("frames out of range (must be < 75): {}", f));
    }
    m.checked_mul(60 * 75)
        .and_then(|x| x.checked_add(sec * 75))
        .and_then(|x| x.checked_add(f))
        .ok_or_else(|| format!("timecode overflow: {}", s))
}

fn parse_args() -> Result<Args, String> {
    let mut iso = None;
    let mut output = None;
    let mut start_lsn = None;
    let mut end_lsn = None;
    let mut channels = None;
    let mut format = None;
    let mut time_filter_start: Option<u32> = None;
    let mut time_filter_duration: Option<u32> = None;
    let mut id3 = Id3Metadata::default();
    let mut dff_opts = DffFooterOptions::default();

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut val = || {
            args.next()
                .ok_or_else(|| format!("missing value for {}", flag))
        };
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
            "--time-filter-start" => {
                time_filter_start = Some(parse_mmss_ff(&val()?)?);
            }
            "--time-filter-duration" => {
                time_filter_duration = Some(parse_mmss_ff(&val()?)?);
            }
            "--id3-title" => id3.tit2 = Some(val()?),
            "--id3-album" => id3.talb = Some(val()?),
            "--id3-artist" => id3.tpe1 = Some(val()?),
            "--id3-album-artist" => id3.tpe2 = Some(val()?),
            "--id3-performer" => id3.txxx_performer = Some(val()?),
            "--id3-composer" => id3.tcom = Some(val()?),
            "--id3-isrc" => id3.tsrc = Some(val()?),
            "--id3-publisher" => id3.tpub = Some(val()?),
            "--id3-copyright" => id3.tcop = Some(val()?),
            "--id3-disc" => id3.tpos = Some(parse_pair_u16(&val()?)?),
            "--id3-genre" => id3.tcon = Some(val()?),
            "--id3-year" => {
                id3.tyer = Some(
                    val()?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                )
            }
            "--id3-date" => id3.tdat = Some(parse_mmdd(&val()?)?),
            "--id3-track" => id3.trck = Some(parse_pair_u16(&val()?)?),
            // --- DFF footer args ---
            "--dff-diar" => dff_opts.diar = Some(val()?),
            "--dff-diti" => dff_opts.diti = Some(val()?),
            "--dff-duration-minutes" => {
                dff_opts.duration_minutes_total = Some(
                    val()?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                )
            }
            "--dff-duration-seconds" => {
                dff_opts.duration_seconds = Some(
                    val()?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                )
            }
            "--dff-duration-frames" => {
                dff_opts.duration_frames = Some(
                    val()?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                )
            }
            "--dff-disc-date" => {
                // Format YYYY-MM-DD (month 1-indexed)
                let s = val()?;
                let parts: Vec<&str> = s.split('-').collect();
                if parts.len() != 3 {
                    return Err(format!("--dff-disc-date expects YYYY-MM-DD, got {}", s));
                }
                dff_opts.disc_date_year = Some(
                    parts[0]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                dff_opts.disc_date_month_1_indexed = Some(
                    parts[1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                dff_opts.disc_date_day = Some(
                    parts[2]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
            }
            "--dff-title" => dff_opts.disc_or_album_title = Some(val()?),
            "--dff-creation-time" => {
                // Format YYYY-MM-DD-HH:MM (month 0-indexed for tm_mon)
                let s = val()?;
                let dash_parts: Vec<&str> = s.split('-').collect();
                if dash_parts.len() != 4 {
                    return Err(format!(
                        "--dff-creation-time expects YYYY-MM-DD-HH:MM, got {}",
                        s
                    ));
                }
                dff_opts.creation_year = Some(
                    dash_parts[0]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                dff_opts.creation_month_0_indexed = Some(
                    dash_parts[1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                dff_opts.creation_day = Some(
                    dash_parts[2]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                let hm: Vec<&str> = dash_parts[3].split(':').collect();
                if hm.len() != 2 {
                    return Err(format!("--dff-creation-time HH:MM section bad, got {}", s));
                }
                dff_opts.creation_hour = Some(
                    hm[0]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                dff_opts.creation_minute = Some(
                    hm[1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
            }
            "--dff-creating-machine" => dff_opts.creating_machine = Some(val()?),
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
    }

    // Time-filter args are mutually required: both or neither.
    let time_filter = match (time_filter_start, time_filter_duration) {
        (Some(s), Some(d)) => Some(TimeFilter::new(s, d)),
        (None, None) => None,
        (Some(_), None) => return Err("--time-filter-start requires --time-filter-duration".into()),
        (None, Some(_)) => return Err("--time-filter-duration requires --time-filter-start".into()),
    };

    Ok(Args {
        iso: iso.ok_or("--iso required")?,
        output: output.ok_or("--output required")?,
        start_lsn: start_lsn.ok_or("--start-lsn required")?,
        end_lsn: end_lsn.ok_or("--end-lsn required")?,
        channels: channels.ok_or("--channels required")?,
        format: format.ok_or("--format required")?,
        time_filter,
        id3,
        dff_footer_opts: dff_opts,
    })
}

/// True if any --dff-* flag was specified.
fn dff_opts_populated(d: &DffFooterOptions) -> bool {
    d.diar.is_some()
        || d.diti.is_some()
        || d.duration_minutes_total.is_some()
        || d.duration_seconds.is_some()
        || d.duration_frames.is_some()
        || d.disc_date_year.is_some()
        || d.disc_or_album_title.is_some()
        || d.creation_year.is_some()
        || d.creating_machine.is_some()
}

/// Build a DffMetadata from the collected CLI options + ID3 metadata.
/// All --dff-* and the ID3 fields must be present.
fn build_dff_metadata(opts: DffFooterOptions, id3: Id3Metadata) -> Result<DffMetadata, String> {
    Ok(DffMetadata {
        diar: opts.diar,
        diti: opts.diti,
        duration_minutes_total: opts
            .duration_minutes_total
            .ok_or("--dff-duration-minutes required for DFF footer")?,
        duration_seconds: opts
            .duration_seconds
            .ok_or("--dff-duration-seconds required")?,
        duration_frames: opts
            .duration_frames
            .ok_or("--dff-duration-frames required")?,
        disc_date_year: opts.disc_date_year.ok_or("--dff-disc-date required")?,
        disc_date_month_1_indexed: opts.disc_date_month_1_indexed.unwrap(),
        disc_date_day: opts.disc_date_day.unwrap(),
        disc_or_album_title: opts
            .disc_or_album_title
            .ok_or("--dff-title required for COMT comment 1")?,
        creation_year: opts.creation_year.ok_or("--dff-creation-time required")?,
        creation_month_0_indexed: opts.creation_month_0_indexed.unwrap(),
        creation_day: opts.creation_day.unwrap(),
        creation_hour: opts.creation_hour.unwrap(),
        creation_minute: opts.creation_minute.unwrap(),
        creating_machine: opts
            .creating_machine
            .ok_or("--dff-creating-machine required")?,
        id3,
    })
}

/// True if any field on `Id3Metadata` is populated.
fn id3_is_populated(m: &Id3Metadata) -> bool {
    m.tit2.is_some()
        || m.talb.is_some()
        || m.tpe1.is_some()
        || m.tpe2.is_some()
        || m.txxx_performer.is_some()
        || m.tcom.is_some()
        || m.tsrc.is_some()
        || m.tpub.is_some()
        || m.tcop.is_some()
        || m.tpos.is_some()
        || m.tcon.is_some()
        || m.tyer.is_some()
        || m.tdat.is_some()
        || m.trck.is_some()
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

    let mut opts = ExtractOptions::new(args.start_lsn, args.end_lsn, args.channels, args.format);
    if let Some(tf) = args.time_filter {
        opts = opts.with_time_filter(tf);
    }
    // DFF footer: if any --dff-* arg was set, build DffMetadata
    // (will fail if a required sub-field is missing). When set,
    // attach to opts. ID3 fields are also threaded through because
    // the embedded ID3 chunk inside the DFF footer uses them.
    if dff_opts_populated(&args.dff_footer_opts) {
        let id3_clone = args.id3.clone();
        let dff_meta = match build_dff_metadata(args.dff_footer_opts, id3_clone) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {}", e);
                return ExitCode::from(2);
            }
        };
        opts = opts.with_dff_metadata(dff_meta);
    }
    if id3_is_populated(&args.id3) {
        opts = opts.with_id3_metadata(args.id3);
    }

    match extract_track(&mut iso, &mut output_file, opts) {
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
            eprintln!(
                "(output file at {} is incomplete; discard it)",
                args.output.display()
            );
            ExitCode::FAILURE
        }
    }
}

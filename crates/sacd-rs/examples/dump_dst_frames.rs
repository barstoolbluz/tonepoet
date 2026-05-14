//! Extract raw DST-encoded frame bytes from a SACD ISO for fixture
//! generation. Used to build PR 2 (DST decoder) test fixtures.
//!
//! ## Usage
//!
//! ```text
//! cargo run --example dump_dst_frames --release -- \
//!     --iso /path/to/album.iso \
//!     --start-lsn 661 --end-lsn 40788 \
//!     --time-filter-start 00:02:00 --time-filter-duration 04:42:22 \
//!     --out-dir /tmp/dst-fixtures \
//!     --count 3
//! ```
//!
//! Writes `frame_<N>.dst.bin` files (where N is 1-indexed) containing
//! the raw DST-encoded payload of each in-range frame. Stops after
//! `count` frames or when the LSN range is exhausted.

use sacd_rs::extract::TimeFilter;
use sacd_rs::frame::FrameReader;
use sacd_rs::iso_reader::IsoReader;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    iso: PathBuf,
    start_lsn: u64,
    end_lsn: u64,
    time_filter: Option<TimeFilter>,
    out_dir: PathBuf,
    count: usize,
}

fn parse_mmss_ff(s: &str) -> Result<u32, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected MM:SS:FF, got {:?}", s));
    }
    let m: u32 = parts[0].parse().map_err(|_| format!("bad minutes: {}", parts[0]))?;
    let sec: u32 = parts[1].parse().map_err(|_| format!("bad seconds: {}", parts[1]))?;
    let f: u32 = parts[2].parse().map_err(|_| format!("bad frames: {}", parts[2]))?;
    if sec >= 60 { return Err(format!("seconds out of range: {}", sec)); }
    if f >= 75 { return Err(format!("frames out of range: {}", f)); }
    m.checked_mul(60 * 75)
        .and_then(|x| x.checked_add(sec * 75))
        .and_then(|x| x.checked_add(f))
        .ok_or_else(|| format!("timecode overflow: {}", s))
}

fn parse_args() -> Result<Args, String> {
    let mut iso = None;
    let mut start_lsn = None;
    let mut end_lsn = None;
    let mut tf_start: Option<u32> = None;
    let mut tf_dur: Option<u32> = None;
    let mut out_dir = None;
    let mut count = 3usize;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("missing value for {}", flag));
        match flag.as_str() {
            "--iso" => iso = Some(PathBuf::from(val()?)),
            "--start-lsn" => start_lsn = Some(val()?.parse::<u64>().map_err(|e| e.to_string())?),
            "--end-lsn" => end_lsn = Some(val()?.parse::<u64>().map_err(|e| e.to_string())?),
            "--time-filter-start" => tf_start = Some(parse_mmss_ff(&val()?)?),
            "--time-filter-duration" => tf_dur = Some(parse_mmss_ff(&val()?)?),
            "--out-dir" => out_dir = Some(PathBuf::from(val()?)),
            "--count" => count = val()?.parse().map_err(|e: std::num::ParseIntError| e.to_string())?,
            "-h" | "--help" => {
                eprintln!("usage: dump_dst_frames --iso PATH --start-lsn N --end-lsn N \\");
                eprintln!("       --time-filter-start MM:SS:FF --time-filter-duration MM:SS:FF \\");
                eprintln!("       --out-dir PATH --count N");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
    }

    let time_filter = match (tf_start, tf_dur) {
        (Some(s), Some(d)) => Some(TimeFilter::new(s, d)),
        (None, None) => None,
        _ => return Err("--time-filter-start and --time-filter-duration must both be set".into()),
    };

    Ok(Args {
        iso: iso.ok_or("--iso required")?,
        start_lsn: start_lsn.ok_or("--start-lsn required")?,
        end_lsn: end_lsn.ok_or("--end-lsn required")?,
        time_filter,
        out_dir: out_dir.ok_or("--out-dir required")?,
        count,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::from(2);
        }
    };

    std::fs::create_dir_all(&args.out_dir).expect("create out dir");
    // Open ISO in read-only mode (IsoReader::open uses File::open which is read-only).
    let mut iso = IsoReader::open(&args.iso).expect("open iso (read-only)");
    let mut reader = FrameReader::new(&mut iso, args.start_lsn, args.end_lsn);

    let mut dumped = 0usize;
    let mut frame_idx = 0usize;
    while dumped < args.count {
        let frame = match reader.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                eprintln!("frame read error: {}", e);
                return ExitCode::FAILURE;
            }
        };
        frame_idx += 1;
        let tc = frame.timecode.as_frame_count();

        // Apply time filter if set (matching sacd_extract's behavior).
        if let Some(tf) = args.time_filter {
            if !tf.includes(tc) {
                continue;
            }
        }

        if !frame.dst_encoded {
            eprintln!(
                "warning: frame {} (tc={}) is NOT DST-encoded; skipping",
                frame_idx, tc,
            );
            continue;
        }

        dumped += 1;
        let path = args.out_dir.join(format!("frame_{:03}.dst.bin", dumped));
        let mut f = File::create(&path).expect("create fixture file");
        f.write_all(&frame.data).expect("write");
        println!(
            "frame {:03}: tc={} bytes={} → {}",
            dumped,
            tc,
            frame.data.len(),
            path.display(),
        );
    }

    println!("dumped {} DST frame(s)", dumped);
    ExitCode::SUCCESS
}

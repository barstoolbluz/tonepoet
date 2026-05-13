//! Dump per-track LSN ranges from a SACD ISO. Used to source
//! start_lsn/end_lsn values for the sacd-rs byte-comparison harness.
//!
//! Usage:
//!   cargo run --example dump_sacd_lsn -- <iso-path>

use std::path::PathBuf;
use tonepoet::tui::sacd::parse_sacd_iso;

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: dump_sacd_lsn <iso-path>");
            std::process::exit(2);
        }
    };
    let meta = match parse_sacd_iso(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("parse failed: {:?}", e);
            std::process::exit(1);
        }
    };
    for (label, area) in [
        ("stereo", &meta.stereo),
        ("multi_channel", &meta.multi_channel),
    ] {
        let Some(area) = area else { continue };
        println!("=== {} ===", label);
        println!(
            "channel_count={}  dst_encoded={}  track_count={}",
            area.header.channel_count,
            area.header.frame_format.is_dst_encoded(),
            area.header.track_count,
        );
        println!(
            "area_toc.track_start_lsn={}  area_toc.track_end_lsn={}",
            area.header.track_start_lsn,
            area.header.track_end_lsn,
        );
        for (i, t) in area.tracks.iter().enumerate() {
            let end_lsn = t.start_lsn as u64 + t.length_lsn as u64;
            println!(
                "  track {:>2}: lsn=[{:>7}, {:>7}) ({:>7} sectors)  time-filter-start={:02}:{:02}:{:02}  time-filter-duration={:02}:{:02}:{:02}",
                i + 1,
                t.start_lsn,
                end_lsn,
                t.length_lsn,
                t.start_time.minutes, t.start_time.seconds, t.start_time.frames,
                t.duration.minutes, t.duration.seconds, t.duration.frames,
            );
        }
    }
}

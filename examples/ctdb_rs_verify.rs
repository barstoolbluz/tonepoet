//! End-to-end CTDB verification driver. Runs the full verify pipeline
//! (TOC build → CTDB query → decode → RS verify → per-track CRC) on
//! the supplied tracks and prints results per track plus the matched
//! entry's confidence.
//!
//! Usage:
//!   cargo run --example ctdb_rs_verify --release -- <track1> <track2> ...

use std::path::PathBuf;

use tonepoet::db::Database;
use tonepoet::tui::accuraterip;
use tonepoet::tui::ctdb;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: ctdb_rs_verify <track1> <track2> ...");
        std::process::exit(1);
    }
    let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();

    let (sample_counts, sample_rate) = match accuraterip::collect_sample_counts(&paths) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("collect_sample_counts: {}", e);
            std::process::exit(2);
        }
    };
    println!("Probed {} tracks @ {} Hz", sample_counts.len(), sample_rate);
    for (i, &count) in sample_counts.iter().enumerate() {
        println!("  Track {} = {} stereo pairs", i + 1, count);
    }
    println!();

    // Open the persistent SQLite cache so this driver exercises the
    // parity-cache path the same way the TUI would.
    let db = Database::open().expect("open tonepoet db");
    let cache_key = ctdb::compute_ctdb_parity_cache_key(&paths);
    let cached_parity = cache_key
        .as_deref()
        .and_then(|k| db.get_cached_ctdb_parity(k, 16));
    let cache_hit = cached_parity.is_some();
    if cache_hit {
        println!("Parity cache: HIT (skipping ~10s parity computation)");
    } else {
        println!("Parity cache: MISS (will compute and store)");
    }
    println!();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut result = runtime.block_on(async {
        ctdb::verify_ctdb(
            &paths,
            &sample_counts,
            sample_rate,
            cache_key,
            cached_parity,
        )
        .await
    });

    // Persist freshly computed parity to the cache (matches event_loop's
    // CtdbComplete handler behavior).
    if let Some((key, parity)) = result.parity_cache_write.take() {
        if let Err(e) = db.store_ctdb_parity(&key, 16, &parity) {
            eprintln!("CTDB parity cache store failed: {}", e);
        } else {
            println!(
                "Parity cache: stored {} bytes for key {}",
                parity.len() * parity[0].len() * 2,
                &key[..16]
            );
        }
    }

    println!("TOC: {}", result.toc);
    if let Some(npar) = result.npar {
        println!(
            "Matched entry: npar={}, stride={:?}, parity_url={:?}",
            npar, result.stride, result.parity_url
        );
    }
    println!();
    println!("Per-track results:");
    for t in &result.tracks {
        let status = match &t.status {
            ctdb::CtdbTrackStatus::Verified => "Verified".to_string(),
            ctdb::CtdbTrackStatus::VerifiedRs => {
                "VerifiedRs (RS-verified, CRC differs)".to_string()
            }
            ctdb::CtdbTrackStatus::Mismatch => "Mismatch".to_string(),
            ctdb::CtdbTrackStatus::NoDiscInDatabase => "NoDiscInDatabase".to_string(),
            ctdb::CtdbTrackStatus::Error(e) => format!("Error: {}", e),
        };
        println!(
            "  Track {} [{}]  computed={:08x}  expected={}  conf={}",
            t.track_number,
            status,
            t.computed_crc32,
            t.expected_crc32
                .map(|c| format!("{:08x}", c))
                .unwrap_or_else(|| "—".into()),
            t.confidence
                .map(|c| c.to_string())
                .unwrap_or_else(|| "—".into()),
        );
    }
    println!();
    println!("Summary: {}", ctdb::format_ctdb_summary(&result));
}

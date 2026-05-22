use std::env;
use std::path::{Path, PathBuf};

use tonepoet::ctdb_rs::STRIDE;
use tonepoet::tui::{accuraterip, ctdb};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        print_usage();
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let mut toc: Option<String> = None;
    let mut entry_id: Option<String> = None;
    let mut confidence: Option<u32> = None;
    let mut print_all = false;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--toc" => {
                toc = Some(args.next().ok_or("--toc requires a CTDB TOC value")?);
            }
            "--entry-id" => {
                entry_id = Some(args.next().ok_or("--entry-id requires an id")?);
            }
            "--confidence" => {
                let value = args.next().ok_or("--confidence requires a number")?;
                confidence = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("invalid confidence: {value}"))?,
                );
            }
            "--all" => {
                print_all = true;
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option: {arg}"));
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }

    if paths.is_empty() {
        return Err("provide one or more track audio paths".into());
    }

    let toc = match toc {
        Some(toc) => toc,
        None => infer_ctdb_toc(&paths)?,
    };

    println!("toc={toc}");
    let response = ctdb::query_ctdb(&toc)
        .await?
        .ok_or_else(|| "CTDB returned no entries for this TOC".to_string())?;

    let entry = select_entry(&response.entries, entry_id.as_deref(), confidence)?;
    println!(
        "entry id={} confidence={} npar={} stride_words={} has_syndrome={} has_parity_url={}",
        entry.id,
        entry.confidence,
        entry.npar,
        entry.stride,
        entry.syndrome.as_deref().is_some_and(|s| !s.is_empty()),
        entry.has_parity.as_deref().is_some_and(|s| !s.is_empty()),
    );

    let mut image: Vec<i16> = Vec::new();
    image.extend(std::iter::repeat(0i16).take(STRIDE));
    for path in &paths {
        let decoded = accuraterip_decode(path)?;
        println!("decoded {} i16_words={}", path.display(), decoded.len());
        image.extend(decoded);
    }
    image.extend(std::iter::repeat(0i16).take(STRIDE));

    let payload_words = image.len().saturating_sub(STRIDE * 2);
    println!(
        "image_i16_words={} payload_i16_words={} stridecount={} laststride={}",
        image.len(),
        payload_words,
        payload_words / STRIDE,
        STRIDE + (payload_words % STRIDE),
    );

    let parity16 = ctdb::compute_audio_parity16(&image)
        .ok_or_else(|| "failed to compute maxNpar=16 CTDB parity".to_string())?;

    let rows = ctdb::ctdb_probe_entry_offsets_with_parity(
        &image,
        &parity16,
        entry,
        (STRIDE as i32 / 2) - 1,
    )
    .ok_or_else(|| "probe failed to decode entry row or build syndrome context".to_string())?;

    let mut exact_hits = 0usize;
    let mut chien_hits = 0usize;
    let mut printed = 0usize;
    let best = rows
        .iter()
        .filter_map(|row| row.errors_found.map(|errors| (errors, row.offset)))
        .min();

    for row in &rows {
        if row.exact_zero {
            exact_hits += 1;
        }
        if row.chien_succeeds {
            chien_hits += 1;
        }
        if print_all || row.exact_zero || row.chien_succeeds {
            printed += 1;
            println!(
                "offset={:+} exact_zero={} nonzero_words={} delta_or=0x{:04x} errors_found={} chien={} positions={:?}",
                row.offset,
                row.exact_zero,
                row.nonzero_syndrome_words,
                row.delta_or,
                row.errors_found
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                row.chien_succeeds,
                row.positions,
            );
        }
    }

    if printed == 0 {
        println!(
            "no exact-zero or Chien-valid offsets printed; rerun with --all for the full table"
        );
    }
    println!(
        "summary exact_hits={} chien_hits={} best_errors_offset={:?}",
        exact_hits, chien_hits, best,
    );

    Ok(())
}

fn infer_ctdb_toc(paths: &[PathBuf]) -> Result<String, String> {
    if let Some(first) = paths.first() {
        if let Some(dir) = first.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Some(toc_sectors) = accuraterip_find_toc_offsets(dir) {
                if toc_sectors.len() == paths.len() + 1 {
                    return Ok(ctdb::build_ctdb_toc(&toc_sectors));
                }
            }
        }
    }

    let (sample_counts, sample_rate) = accuraterip_collect_sample_counts(paths)?;
    Ok(ctdb::build_ctdb_toc_from_samples(
        &sample_counts,
        sample_rate,
    ))
}

fn select_entry<'a>(
    entries: &'a [ctdb::CtdbEntry],
    entry_id: Option<&str>,
    confidence: Option<u32>,
) -> Result<&'a ctdb::CtdbEntry, String> {
    if let Some(id) = entry_id {
        return entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("CTDB entry id {id} not found"));
    }

    if let Some(confidence) = confidence {
        let mut matches = entries
            .iter()
            .filter(|entry| entry.confidence == confidence);
        let first = matches
            .next()
            .ok_or_else(|| format!("no CTDB entry has confidence {confidence}"))?;
        if let Some(second) = matches.next() {
            return Err(format!(
                "confidence {confidence} is ambiguous: at least ids {} and {}; use --entry-id",
                first.id, second.id,
            ));
        }
        return Ok(first);
    }

    entries
        .iter()
        .max_by_key(|entry| entry.confidence)
        .ok_or_else(|| "CTDB response had no entries".to_string())
}

fn print_usage() {
    eprintln!(
        "usage: cargo run --example ctdb_disc_syndrome_probe -- [--toc CTDB_TOC] [--entry-id ID | --confidence N] [--all] TRACK..."
    );
}

// Small wrappers keep the example readable and make compiler errors point at
// the public tonepoet APIs this diagnostic depends on.
fn accuraterip_decode(path: &Path) -> Result<Vec<i16>, String> {
    accuraterip_decode_track_to_raw_i16(path)
}

fn accuraterip_decode_track_to_raw_i16(path: &Path) -> Result<Vec<i16>, String> {
    accuraterip::decode_track_to_raw_i16(path)
}

fn accuraterip_collect_sample_counts(paths: &[PathBuf]) -> Result<(Vec<u64>, u32), String> {
    accuraterip::collect_sample_counts(paths)
}

fn accuraterip_find_toc_offsets(dir: &Path) -> Option<Vec<u32>> {
    accuraterip::find_toc_offsets(dir)
}

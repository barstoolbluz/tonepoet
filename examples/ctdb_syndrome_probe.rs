//! Focused diagnostic: compute our audio's syndrome at small offsets
//! using the existing `parity_to_syndrome_with_word_offset`, and compare
//! row 0 to the canonical 896 entry's syndrome bytes.
//!
//! This bypasses the patch's offset-trial loop and just answers:
//! "Does our codec's syndrome computation produce the same bytes as the
//! 896 entry's syndrome attribute, at any small offset?"
//!
//! Usage:
//!   cargo run --example ctdb_syndrome_probe --release -- <track1> <track2> <track3> <track4>

use std::process::Command;

const STRIDE: usize = 11_760;
const NPAR: usize = 16;

// 896 entry: syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="
// Decoded: 32 bytes = 16 little-endian u16
const ENTRY_896_SYNDROME_LE_U16: [u16; NPAR] = [
    0x8d1c, 0xf108, 0x4bcc, 0x9ecf, 0xd0a2, 0x8497, 0x77c9, 0x002b, 0xd5a5, 0xf2b3, 0x5745, 0xe009,
    0x469e, 0xf199, 0xe3ad, 0x9f3b,
];

fn decode_flac_to_i16(path: &str) -> Vec<i16> {
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path,
            "-f",
            "s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
            "-",
        ])
        .output()
        .expect("ffmpeg failed");
    if !output.status.success() {
        panic!(
            "ffmpeg decode failed for {}: {}",
            path,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
        .stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.len() != 4 {
        eprintln!("usage: ctdb_syndrome_probe <track1> <track2> <track3> <track4>");
        std::process::exit(1);
    }

    eprintln!("Decoding 4 tracks...");
    let mut tracks: Vec<Vec<i16>> = Vec::with_capacity(4);
    for p in &paths {
        let t = decode_flac_to_i16(p);
        eprintln!("  {} → {} i16", p, t.len());
        tracks.push(t);
    }

    // Assemble disc image: [STRIDE leadin] + tracks + [STRIDE leadout]
    let total_track_i16: usize = tracks.iter().map(|t| t.len()).sum();
    let image_len = STRIDE + total_track_i16 + STRIDE;
    let mut image: Vec<i16> = Vec::with_capacity(image_len);
    image.extend(std::iter::repeat(0i16).take(STRIDE));
    for t in &tracks {
        image.extend_from_slice(t);
    }
    image.extend(std::iter::repeat(0i16).take(STRIDE));
    drop(tracks);
    eprintln!(
        "Disc image: {} i16 ({} stereo pairs)",
        image.len(),
        image.len() / 2
    );

    eprintln!("Computing audio parity matrix (npar={})...", NPAR);
    let codec = tonepoet::ctdb_rs::CtdbCodec::new();
    let parity =
        tonepoet::ctdb_rs::syndrome::compute_parity_matrix_from_audio(codec.galois(), &image, NPAR)
            .expect("compute parity");

    eprintln!();
    eprintln!(
        "Our audio parity[0]   (16 u16): {}",
        parity[0]
            .iter()
            .take(NPAR)
            .map(|w| format!("{:04x}", w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    eprintln!(
        "Our audio parity[1]   (16 u16): {}",
        parity[1]
            .iter()
            .take(NPAR)
            .map(|w| format!("{:04x}", w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    eprintln!(
        "Our audio parity[STRIDE-1] (16 u16): {}",
        parity[STRIDE - 1]
            .iter()
            .take(NPAR)
            .map(|w| format!("{:04x}", w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    eprintln!();
    eprintln!(
        "896 entry syndrome (target):  {}",
        ENTRY_896_SYNDROME_LE_U16
            .iter()
            .map(|w| format!("{:04x}", w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    eprintln!();

    // Probe: at various small word offsets, compute syn[0] and print.
    // word_offset is in u16 words = 2× stereo sample pairs.
    let word_offsets: Vec<i64> = (-30..=30).map(|i| i * 2).collect();

    eprintln!("Trying offsets ±30 stereo samples (word offsets ±60), reading row 0:");
    let mut best_match: Option<(i64, usize)> = None;
    for &word_offset in &word_offsets {
        // Read row at index (-word_offset) mod stride (the row that becomes syn[0]).
        let row_idx = (-word_offset).rem_euclid(STRIDE as i64) as usize;
        let parity_row = &parity[row_idx];

        // Compute the syndrome row from this single parity row. This mirrors
        // parity_to_syndrome_with_word_offset's inner loop for y=0.
        let gf = codec.galois();
        let mut syn = vec![0u16; NPAR];
        for x1 in 0..NPAR {
            let par = parity_row[x1];
            if par == 0 {
                continue;
            }
            let llo = gf.log(par).unwrap() + tonepoet::ctdb_rs::galois::MAX;
            for x in 0..NPAR {
                syn[x] ^= gf.exp_table()[llo - (1 + x1) * x];
            }
        }

        // Count matching positions
        let matches: usize = syn
            .iter()
            .zip(&ENTRY_896_SYNDROME_LE_U16)
            .filter(|(a, b)| a == b)
            .count();
        if matches > 0 {
            eprintln!("  offset {:+3} (word {:+3}, row_idx {}): {} matches  syn[0..4] = {:04x} {:04x} {:04x} {:04x}",
                word_offset / 2, word_offset, row_idx, matches,
                syn[0], syn[1], syn[2], syn[3]);
        }
        if matches > best_match.map(|m| m.1).unwrap_or(0) {
            best_match = Some((word_offset, matches));
        }
        if matches == NPAR {
            println!();
            println!(
                "EXACT MATCH at sample offset {:+}:  {}",
                word_offset / 2,
                syn.iter()
                    .map(|w| format!("{:04x}", w))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            return;
        }
    }

    // Also try BIG-ENDIAN interpretation in case the patch got the byte order wrong.
    eprintln!();
    eprintln!("--- Trying BIG-ENDIAN interpretation of the syndrome bytes ---");
    let entry_syndrome_be: [u16; NPAR] = [
        0x1c8d, 0x08f1, 0xcc4b, 0xcf9e, 0xa2d0, 0x9784, 0xc977, 0x2b00, 0xa5d5, 0xb3f2, 0x4557,
        0x09e0, 0x9e46, 0x99f1, 0xade3, 0x3b9f,
    ];
    eprintln!(
        "896 entry syndrome (BE u16):  {}",
        entry_syndrome_be
            .iter()
            .map(|w| format!("{:04x}", w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    for &word_offset in &word_offsets {
        let row_idx = (-word_offset).rem_euclid(STRIDE as i64) as usize;
        let parity_row = &parity[row_idx];
        let gf = codec.galois();
        let mut syn = vec![0u16; NPAR];
        for x1 in 0..NPAR {
            let par = parity_row[x1];
            if par == 0 {
                continue;
            }
            let llo = gf.log(par).unwrap() + tonepoet::ctdb_rs::galois::MAX;
            for x in 0..NPAR {
                syn[x] ^= gf.exp_table()[llo - (1 + x1) * x];
            }
        }
        let matches: usize = syn
            .iter()
            .zip(&entry_syndrome_be)
            .filter(|(a, b)| a == b)
            .count();
        if matches >= 4 {
            eprintln!("  BE: offset {:+3}: {} matches", word_offset / 2, matches);
        }
        if matches == NPAR {
            println!("EXACT MATCH (BE) at sample offset {:+}", word_offset / 2);
            return;
        }
    }

    eprintln!();
    if let Some((wo, m)) = best_match {
        eprintln!("Best partial match (syndrome interpretation): word offset {} with {} of {} symbols matching", wo, m, NPAR);
    } else {
        eprintln!("No partial matches in syndrome interpretation.");
    }

    // ──────────── HYPOTHESIS 2: the "syndrome" bytes are actually a parity row ──
    eprintln!();
    eprintln!("--- Trying hypothesis: syndrome bytes are a PARITY row directly ---");
    // Search ALL parity rows for one that matches the target bytes (LE).
    let target = ENTRY_896_SYNDROME_LE_U16;
    let mut best_row: Option<(usize, usize)> = None;
    for row_idx in 0..STRIDE {
        let parity_row = &parity[row_idx];
        if parity_row.len() < NPAR {
            continue;
        }
        let matches: usize = parity_row
            .iter()
            .take(NPAR)
            .zip(&target)
            .filter(|(a, b)| a == b)
            .count();
        if matches >= NPAR / 2 {
            eprintln!(
                "  parity row {}: {} matches  [{}]",
                row_idx,
                matches,
                parity_row
                    .iter()
                    .take(4)
                    .map(|w| format!("{:04x}", w))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if matches > best_row.map(|m| m.1).unwrap_or(0) {
            best_row = Some((row_idx, matches));
        }
        if matches == NPAR {
            println!();
            println!("EXACT MATCH at parity row {}", row_idx);
            return;
        }
    }
    if let Some((row, m)) = best_row {
        eprintln!(
            "Best parity-row match: row {} with {} of {} symbols",
            row, m, NPAR
        );
    } else {
        eprintln!(
            "No partial parity-row matches (best <{} symbols).",
            NPAR / 2
        );
    }

    // ──────────── HYPOTHESIS 3: bytes are a single COLUMN slice (parity[*][col]) ──
    eprintln!();
    eprintln!("--- Trying hypothesis: syndrome bytes are a COLUMN slice across rows ---");
    // For each column c in 0..NPAR, take parity[0..NPAR][c] — i.e. the c-th
    // parity symbol from the first NPAR rows.
    for col in 0..NPAR {
        let column_slice: Vec<u16> = parity.iter().take(NPAR).map(|row| row[col]).collect();
        let matches: usize = column_slice
            .iter()
            .zip(&target)
            .filter(|(a, b)| a == b)
            .count();
        if matches >= NPAR / 2 {
            eprintln!("  column {}: {} matches", col, matches);
        }
        if matches == NPAR {
            println!("EXACT MATCH at parity column {} (first NPAR rows)", col);
            return;
        }
    }

    // ──────────── HYPOTHESIS 4: syndrome bytes ARE entry's parity[0]; BM-decode the XOR delta ──
    eprintln!();
    eprintln!("--- Trying hypothesis: syndrome = entry's parity[0]; BM-decode XOR delta ---");
    // For each candidate row of OUR parity (which corresponds to an offset),
    // XOR with the entry's syndrome bytes (= entry's parity[0]) and BM-decode.
    let gf = codec.galois();
    let max_correctable = NPAR / 2;
    let mut best: Option<(i64, usize)> = None;
    // Try a wider window than ±30: ±5879 would be ideal but is slow. Start narrow.
    let test_offsets: Vec<i64> = (-50..=50).map(|i| (i * 2) as i64).collect();
    for &word_offset in &test_offsets {
        let row_idx = (-word_offset).rem_euclid(STRIDE as i64) as usize;
        let our_row = &parity[row_idx];
        let mut delta = [0u16; NPAR];
        for k in 0..NPAR {
            delta[k] = our_row[k] ^ ENTRY_896_SYNDROME_LE_U16[k];
        }
        // Use the codec's berlekamp_massey via the public re-export.
        let bm = tonepoet::ctdb_rs::berlekamp_massey(gf, &delta, NPAR);
        match bm {
            Some((_sigma, errors_found)) => {
                if errors_found <= max_correctable {
                    eprintln!(
                        "  word_offset {:+} (row {}): BM found {} errors (≤ {}) ← CORRECTABLE",
                        word_offset, row_idx, errors_found, max_correctable
                    );
                    if best.map(|b| errors_found < b.1).unwrap_or(true) {
                        best = Some((word_offset, errors_found));
                    }
                }
            }
            None => {}
        }
    }
    match best {
        Some((wo, errs)) => {
            println!();
            println!("RESULT: column 0 verifiable at word_offset {:+} ({} stereo samples) with {} errors",
                wo, wo / 2, errs);
            println!("This validates the algorithm: XOR audio's parity row with entry's syndrome,");
            println!("then BM-decode the result. Fast (no full parity download required).");
        }
        None => {
            eprintln!("BM-decode found no correctable result in tested offsets.");
        }
    }
}

// CTDB repair fast path translated from CUETools.NET.
//
// Drop this block into src/tui/ctdb.rs after the existing verify-side
// CUETools helpers. It intentionally does not modify src/ctdb_rs/mod.rs:
// the codec's GF, BM, Chien, Forney, residual, and parity builders are reused.
//
// Required existing items in the same module:
// - CtdbEntry
// - CUETOOLS_MAX_NPAR
// - CuetoolsSyndromeContext
// - cuetools_build_syndrome_context
// - cuetools_bytes_to_syndrome_matrix
// - cuetools_parity_row_to_syndrome_row
// - decode_entry_row_cuetools
// - gf_mul_exp / gf_div_exp
//
// The important source correction: CTDB hasparity blobs are CUETools syndrome
// matrices. Decode them with Bytes2Syndrome layout and do not run Parity2Syndrome
// over the downloaded blob again.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CuetoolsRepairCorrection {
    pub row: usize,
    pub column: usize,
    pub magnitude: u16,
    pub audio_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuetoolsRepairOutcome {
    pub corrected_samples: usize,
    pub error_positions: Vec<(usize, usize)>,
    pub corrections: Vec<CuetoolsRepairCorrection>,
    pub verified_after: bool,
}

#[inline]
fn ctdb_repair_invalid_input(msg: impl Into<String>) -> crate::ctdb_rs::RepairError {
    crate::ctdb_rs::RepairError::InvalidInput(msg.into())
}

#[inline]
fn ctdb_repair_invalid_parity(msg: impl Into<String>) -> crate::ctdb_rs::RepairError {
    crate::ctdb_rs::RepairError::InvalidParity(msg.into())
}

// Full-matrix form of AccurateRipVerify.GetSyndrome(npar, -1, offset).
//
// matches CUETools.AccurateRip/AccurateRip.cs:2782-2802
// matches CUETools.AccurateRip/AccurateRip.cs:2805-2848
fn cuetools_get_syndrome_matrix(
    gf: &crate::ctdb_rs::Galois16,
    parity16: &[Vec<u16>],
    ctx: &CuetoolsSyndromeContext,
    out_npar: usize,
    sample_offset: i32,
) -> Option<Vec<Vec<u16>>> {
    if out_npar == 0 || out_npar > CUETOOLS_MAX_NPAR || parity16.len() < ctx.stride {
        return None;
    }

    let stride_i64 = ctx.stride as i64;
    let parity2syndrome_word_offset = -2_i64 * sample_offset as i64;
    let offset_words = 2_i64 * sample_offset as i64;
    let mut matrix = Vec::with_capacity(ctx.stride);

    // C# uses `if (strides == -1) strides = stride` and then applies the
    // same `part2` loop/body used by the one-row verifier.
    for part2 in 0..ctx.stride {
        // matches CUETools.Parity/Parity2Syndrome.cs:787-812 via
        // AccurateRip.cs:2798-2802
        let part2_i64 = part2 as i64;
        let y1 = (part2_i64 - parity2syndrome_word_offset).rem_euclid(stride_i64) as usize;
        let mut syn = cuetools_parity_row_to_syndrome_row(
            gf,
            parity16.get(y1)?,
            out_npar,
            CUETOOLS_MAX_NPAR,
        )?;

        // matches CUETools.AccurateRip/AccurateRip.cs:2808
        let part = (part2_i64 + offset_words).rem_euclid(stride_i64);

        // C# first-boundary correction.
        // matches CUETools.AccurateRip/AccurateRip.cs:2810-2827
        if part < offset_words {
            let part_usize = part as usize;
            for i in 0..out_npar {
                let mut syn_i = gf_mul_exp(gf, syn[i], i);
                syn_i ^= *ctx.leadout.get(ctx.laststride.checked_sub(part_usize + 1)?)?;
                let leadin_index = ctx.stride.checked_add(part_usize)?;
                let exp = (i * ctx.stridecount) % crate::ctdb_rs::galois::MAX;
                syn_i ^= gf_mul_exp(gf, *ctx.leadin.get(leadin_index)?, exp);
                syn[i] = syn_i;
            }
        }

        // C# last-boundary correction.
        // matches CUETools.AccurateRip/AccurateRip.cs:2829-2846
        if part >= stride_i64 + offset_words {
            let part_usize = part as usize;
            for i in 0..out_npar {
                let leadout_index = ctx
                    .laststride
                    .checked_add(ctx.stride)?
                    .checked_sub(part_usize + 1)?;
                let exp = (i * ctx.stridecount) % crate::ctdb_rs::galois::MAX;
                let mut syn_i = syn[i]
                    ^ *ctx.leadout.get(leadout_index)?
                    ^ gf_mul_exp(gf, *ctx.leadin.get(part_usize)?, exp);
                syn_i = gf_div_exp(gf, syn_i, i);
                syn[i] = syn_i;
            }
        }

        matrix.push(syn);
    }

    Some(matrix)
}

// Decode the downloaded hasparity payload as a syndrome matrix.
//
// matches CUETools.CTDB/CUEToolsDB.cs:2168-2280
// matches CUETools.Parity/Parity2Syndrome.cs:710-735
fn cuetools_decode_hasparity_syndrome_matrix(
    gf: &crate::ctdb_rs::Galois16,
    blob_bytes: &[u8],
    entry: &CtdbEntry,
    ctx: &CuetoolsSyndromeContext,
    npar: usize,
) -> Result<Vec<Vec<u16>>, crate::ctdb_rs::RepairError> {
    if entry.stride != 0 && entry.stride != ctx.stride {
        return Err(ctdb_repair_invalid_parity(format!(
            "CTDB entry stride {} does not match audio stride {}",
            entry.stride, ctx.stride
        )));
    }

    let matrix = cuetools_bytes_to_syndrome_matrix(blob_bytes, ctx.stride, npar)
        .ok_or_else(|| {
            ctdb_repair_invalid_parity(format!(
                "hasparity blob has {} bytes; need at least {} bytes for stride {} npar {}",
                blob_bytes.len(),
                ctx.stride * npar * 2,
                ctx.stride,
                npar
            ))
        })?;

    // CUETools FetchDB checks that downloaded row 0 equals the XML row.
    // That catches truncated/range-wrong/mismatched parity records early.
    // matches CUETools.CTDB/CUEToolsDB.cs:2264-2276
    if let Some((entry_row, entry_npar, _source)) = decode_entry_row_cuetools(gf, entry) {
        let compare_npar = npar.min(entry_npar).min(entry_row.len());
        for i in 0..compare_npar {
            if matrix[0][i] != entry_row[i] {
                return Err(ctdb_repair_invalid_parity(format!(
                    "hasparity row 0 symbol {} = {:04x}, entry row = {:04x}",
                    i, matrix[0][i], entry_row[i]
                )));
            }
        }
    }

    Ok(matrix)
}

#[inline]
fn cuetools_syndrome_matrices_equal(
    lhs: &[Vec<u16>],
    rhs: &[Vec<u16>],
    npar: usize,
) -> bool {
    lhs.len() == rhs.len()
        && lhs.iter().zip(rhs).all(|(a, b)| {
            a.len() >= npar && b.len() >= npar && a[..npar] == b[..npar]
        })
}

// Padded-image adaptation of CUETools' error offset calculation.
//
// C# source form:
//   pos = toPos(...) * stride + part2
//   erroffi = stride + pos + pregap * 2 - actualOffset * 2
//
// Tonepoet hands the codec a padded disc image:
//   [STRIDE i16 leadin] + protected PCM + [STRIDE i16 leadout]
// with pregap not represented as a separate source-stream coordinate. At offset
// zero this reduces to the existing codec mapping: STRIDE + row * STRIDE + col.
//
// matches CUETools.AccurateRip/CDRepair.cs:1440-1446
// matches CUETools.AccurateRip/CDRepair.cs:1661-1681
#[inline]
fn cuetools_repair_audio_index(
    row: usize,
    column: usize,
    actual_offset: i32,
    audio_len: usize,
) -> Option<usize> {
    // Chien position 0 corresponds to the first row of CUETools' protected
    // middle span — i.e., payload row 1 (audio offset 2*STRIDE), because the
    // first stride of payload lives in leadin and is excluded from the LFSR.
    // matches CUETools.AccurateRip/AccurateRip.cs:3099-3105
    let idx = 2_i64 * crate::ctdb_rs::STRIDE as i64
        + row as i64 * crate::ctdb_rs::STRIDE as i64
        + column as i64
        - 2_i64 * actual_offset as i64;
    if idx < 0 || idx as usize >= audio_len {
        None
    } else {
        Some(idx as usize)
    }
}

fn cuetools_repaired_audio_matches_blob(
    gf: &crate::ctdb_rs::Galois16,
    audio: &[i16],
    blob_syndrome: &[Vec<u16>],
    npar: usize,
    actual_offset: i32,
) -> bool {
    let Some(ctx) = cuetools_build_syndrome_context(audio) else {
        return false;
    };
    let Some(parity16) = compute_audio_parity16(audio) else {
        return false;
    };
    let Some(our_syndrome) =
        cuetools_get_syndrome_matrix(gf, &parity16, &ctx, npar, -actual_offset)
    else {
        return false;
    };
    cuetools_syndrome_matrices_equal(&our_syndrome, blob_syndrome, npar)
}

// Blocking form for call sites that already run inside spawn_blocking.
//
// matches CUETools.AccurateRip/CDRepair.cs:1351-1498
pub fn repair_disc_via_rs_blocking(
    audio: &mut [i16],
    blob_bytes: &[u8],
    entry: &CtdbEntry,
    actual_offset: i32,
) -> Result<CuetoolsRepairOutcome, crate::ctdb_rs::RepairError> {
    let npar = (entry.npar as usize).min(CUETOOLS_MAX_NPAR);
    crate::ctdb_rs::syndrome::validate_npar(npar).map_err(crate::ctdb_rs::RepairError::from)?;

    let codec = crate::ctdb_rs::CtdbCodec::new();
    let gf = codec.galois();
    let ctx = cuetools_build_syndrome_context(audio)
        .ok_or_else(|| ctdb_repair_invalid_input("audio is too short for CTDB repair context"))?;

    let blob_syndrome = cuetools_decode_hasparity_syndrome_matrix(
        gf,
        blob_bytes,
        entry,
        &ctx,
        npar,
    )?;

    // CUETools VerifyParity computes GetSyndrome(npar, -1, -actualOffset) over
    // the middle-span parity workspace — first stride and final laststride live
    // in leadin/leadout, not in the LFSR.
    // matches CUETools.AccurateRip/CDRepair.cs:1363
    // matches CUETools.AccurateRip/AccurateRip.cs:3099-3105
    let parity16 = compute_audio_parity16(audio)
        .ok_or_else(|| ctdb_repair_invalid_input("failed to compute CTDB parity workspace"))?;
    let our_syndrome = cuetools_get_syndrome_matrix(gf, &parity16, &ctx, npar, -actual_offset)
        .ok_or_else(|| ctdb_repair_invalid_input("failed to compute CUETools syndrome matrix"))?;

    let max_correctable = npar / 2;
    let mut all_corrections = Vec::<CuetoolsRepairCorrection>::new();

    // Per-column XOR and decoder pass.
    // matches CUETools.AccurateRip/CDRepair.cs:1395-1463
    for column in 0..ctx.stride {
        let lhs = our_syndrome.get(column).ok_or_else(|| {
            ctdb_repair_invalid_input(format!("computed syndrome missing column {column}"))
        })?;
        let rhs = blob_syndrome.get(column).ok_or_else(|| {
            ctdb_repair_invalid_parity(format!("downloaded syndrome missing column {column}"))
        })?;
        if lhs.len() < npar || rhs.len() < npar {
            return Err(ctdb_repair_invalid_parity(format!(
                "syndrome column {column} is shorter than npar {npar}"
            )));
        }

        let mut delta = vec![0u16; npar];
        for i in 0..npar {
            delta[i] = lhs[i] ^ rhs[i];
        }
        if crate::ctdb_rs::decoder::all_zero(&delta) {
            continue;
        }

        let (sigma, errors_found) = crate::ctdb_rs::berlekamp_massey(gf, &delta, npar)
            .ok_or(crate::ctdb_rs::RepairError::Uncorrectable {
                column,
                errors_found: 0,
                max_correctable,
            })?;
        if errors_found == 0 || errors_found > max_correctable {
            return Err(crate::ctdb_rs::RepairError::Uncorrectable {
                column,
                errors_found,
                max_correctable,
            });
        }

        let positions = crate::ctdb_rs::chien_search(gf, &sigma, errors_found, ctx.stridecount)
            .ok_or(crate::ctdb_rs::RepairError::Uncorrectable {
                column,
                errors_found,
                max_correctable,
            })?;
        if positions.len() != errors_found {
            return Err(crate::ctdb_rs::RepairError::Uncorrectable {
                column,
                errors_found,
                max_correctable,
            });
        }

        let magnitudes = crate::ctdb_rs::forney(
            gf,
            &delta,
            &sigma,
            &positions,
            npar,
            ctx.stridecount,
        )
        .ok_or(crate::ctdb_rs::RepairError::Uncorrectable {
            column,
            errors_found,
            max_correctable,
        })?;

        let located: Vec<crate::ctdb_rs::LocatedError> = positions
            .iter()
            .copied()
            .zip(magnitudes.iter().copied())
            .map(|(row, magnitude)| crate::ctdb_rs::LocatedError { row, magnitude })
            .collect();
        let residual = crate::ctdb_rs::decoder::residual_after_correction(
            gf,
            &delta,
            &located,
            npar,
            ctx.stridecount,
        )
        .ok_or(crate::ctdb_rs::RepairError::Uncorrectable {
            column,
            errors_found,
            max_correctable,
        })?;
        if !crate::ctdb_rs::decoder::all_zero(&residual) {
            return Err(crate::ctdb_rs::RepairError::Uncorrectable {
                column,
                errors_found,
                max_correctable,
            });
        }

        for correction in located {
            let audio_index = cuetools_repair_audio_index(
                correction.row,
                column,
                actual_offset,
                audio.len(),
            )
            .ok_or_else(|| {
                ctdb_repair_invalid_input(format!(
                    "repair correction row {} column {} falls outside padded audio at offset {:+}",
                    correction.row, column, actual_offset
                ))
            })?;
            all_corrections.push(CuetoolsRepairCorrection {
                row: correction.row,
                column,
                magnitude: correction.magnitude,
                audio_index,
            });
        }
    }

    all_corrections.sort_by_key(|c| c.audio_index);
    let initially_equal = cuetools_syndrome_matrices_equal(&our_syndrome, &blob_syndrome, npar);

    let mut old_values = Vec::with_capacity(all_corrections.len());
    for correction in &all_corrections {
        old_values.push((correction.audio_index, audio[correction.audio_index]));
        let fixed = crate::ctdb_rs::syndrome::i16_to_u16_bits(audio[correction.audio_index])
            ^ correction.magnitude;
        audio[correction.audio_index] = crate::ctdb_rs::syndrome::u16_to_i16_bits(fixed);
    }

    // CUETools validates candidate repairs with CTDBCRC before returning the
    // fix object. This Rust wrapper does not have the AccurateRipVerify CRC
    // cache here, so it rechecks the full syndrome matrix instead and reverts
    // samples on failure.
    // matches CUETools.AccurateRip/CDRepair.cs:1469-1479
    let verified_after = if all_corrections.is_empty() {
        initially_equal
    } else {
        cuetools_repaired_audio_matches_blob(gf, audio, &blob_syndrome, npar, actual_offset)
    };

    if !verified_after {
        for (idx, old) in old_values {
            audio[idx] = old;
        }
        return Err(ctdb_repair_invalid_input(
            "post-repair syndrome verification failed; corrections reverted",
        ));
    }

    Ok(CuetoolsRepairOutcome {
        corrected_samples: all_corrections.len(),
        error_positions: all_corrections
            .iter()
            .map(|c| (c.row, c.column))
            .collect(),
        corrections: all_corrections,
        verified_after,
    })
}

// Minimal-call-site adapter for the current repair_album / repair_single_image
// signatures. Prefer repair_disc_via_rs_blocking with a real CtdbEntry when the
// caller can pass the selected entry through.
pub fn repair_disc_via_rs_with_npar_blocking(
    audio: &mut [i16],
    blob_bytes: &[u8],
    npar: usize,
    actual_offset: i32,
) -> Result<CuetoolsRepairOutcome, crate::ctdb_rs::RepairError> {
    let entry = CtdbEntry {
        id: format!("synthetic-npar-{npar}"),
        crc32: 0,
        confidence: 0,
        npar: npar as u32,
        stride: crate::ctdb_rs::STRIDE,
        has_parity: None,
        parity: None,
        syndrome: None,
        track_crcs: Vec::new(),
    };
    repair_disc_via_rs_blocking(audio, blob_bytes, &entry, actual_offset)
}

// Async convenience form. The existing repair_album / repair_single_image
// functions already call spawn_blocking around the repair, so prefer
// repair_disc_via_rs_blocking there to avoid a second full-image copy.
pub async fn repair_disc_via_rs(
    audio: &mut [i16],
    blob_bytes: &[u8],
    entry: &CtdbEntry,
    actual_offset: i32,
) -> Result<CuetoolsRepairOutcome, crate::ctdb_rs::RepairError> {
    let mut image = audio.to_vec();
    let blob = blob_bytes.to_vec();
    let entry = entry.clone();
    let (outcome, repaired) = tokio::task::spawn_blocking(move || {
        repair_disc_via_rs_blocking(&mut image, &blob, &entry, actual_offset)
            .map(|outcome| (outcome, image))
    })
    .await
    .map_err(|e| ctdb_repair_invalid_input(format!("repair task failed: {e}")))??;
    audio.copy_from_slice(&repaired);
    Ok(outcome)
}

#[cfg(test)]
mod cuetools_repair_translation_fixtures {
    use super::*;

    fn repair_fixture_audio() -> Vec<i16> {
        let stride = crate::ctdb_rs::STRIDE;
        let full_rows = 5usize; // lead-in row, 3 protected rows, lead-out row.
        let mut audio = vec![0i16; full_rows * stride];
        for idx in stride..(stride + 3 * stride) {
            let word = ((idx as u32 * 257 + 0x1234) & 0xffff) as u16;
            audio[idx] = word as i16;
        }
        audio
    }

    #[test]
    fn cuetools_full_matrix_get_syndrome_repair_fixture() {
        let gf = crate::ctdb_rs::Galois16::new();
        let npar = 4usize;
        let audio = repair_fixture_audio();
        let ctx = cuetools_build_syndrome_context(&audio).unwrap();
        let parity16 = crate::ctdb_rs::syndrome::compute_parity_matrix_from_audio(
            &gf,
            &audio,
            CUETOOLS_MAX_NPAR,
        )
        .unwrap();
        let syndrome = cuetools_get_syndrome_matrix(&gf, &parity16, &ctx, npar, 0).unwrap();

        assert_eq!(syndrome.len(), crate::ctdb_rs::STRIDE);
        assert_eq!(syndrome[10], vec![0x143e, 0x2e8a, 0x9480, 0xfd0a]);
        assert_eq!(syndrome[11], vec![0x153f, 0x298d, 0x8195, 0xb443]);
    }

    /// Build a synthetic disc image whose payload spans 5 strides, giving
    /// stridecount=3 (5 full payload strides minus the first stride and the
    /// final laststride) under CUETools' middle-span parity model. Errors
    /// injected into payload rows 1, 2, 3 fall inside the protected span and
    /// land at Chien positions 0, 1, 2.
    fn repair_fixture_audio_middle_span() -> Vec<i16> {
        let stride = crate::ctdb_rs::STRIDE;
        let full_rows = 7usize; // lead-in pad + 5 payload rows + lead-out pad.
        let mut audio = vec![0i16; full_rows * stride];
        for idx in stride..(stride + 5 * stride) {
            let word = ((idx as u32 * 257 + 0x1234) & 0xffff) as u16;
            audio[idx] = word as i16;
        }
        audio
    }

    #[test]
    fn cuetools_repair_synthetic_syndrome_blob_fixture() {
        let gf = crate::ctdb_rs::Galois16::new();
        let npar = 4usize;
        let mut audio = repair_fixture_audio_middle_span();
        let original = audio.clone();
        let ctx = cuetools_build_syndrome_context(&audio).unwrap();
        // Use the production middle-span parity so the test's clean syndrome
        // matches what repair_disc_via_rs_blocking sees at runtime.
        let parity16 = compute_audio_parity16(&audio).unwrap();
        let clean_syndrome = cuetools_get_syndrome_matrix(&gf, &parity16, &ctx, npar, 0).unwrap();
        let blob = crate::ctdb_rs::syndrome::parity_to_bytes(
            &clean_syndrome,
            crate::ctdb_rs::STRIDE,
            npar,
        );

        let stride = crate::ctdb_rs::STRIDE;
        // Inject errors into payload rows 1, 2, 3 — the protected middle span.
        audio[stride + stride + 10] ^= 0x0101u16 as i16;
        audio[stride + 2 * stride + 11] ^= 0x7777u16 as i16;
        audio[stride + 3 * stride + 10] ^= 0x0202u16 as i16;

        let entry = CtdbEntry {
            id: "synthetic".to_string(),
            crc32: 0,
            confidence: 1,
            npar: npar as u32,
            stride,
            has_parity: Some("memory://synthetic".to_string()),
            parity: None,
            syndrome: Some({
                let mut row0 = Vec::with_capacity(npar * 2);
                for &word in clean_syndrome[0].iter().take(npar) {
                    row0.extend_from_slice(&word.to_le_bytes());
                }
                base64::engine::general_purpose::STANDARD.encode(row0)
            }),
            track_crcs: Vec::new(),
        };

        let outcome = repair_disc_via_rs_blocking(&mut audio, &blob, &entry, 0).unwrap();
        assert_eq!(outcome.corrected_samples, 3);
        assert_eq!(outcome.verified_after, true);
        assert_eq!(audio, original);
        assert_eq!(
            outcome.corrections,
            vec![
                CuetoolsRepairCorrection {
                    row: 0,
                    column: 10,
                    magnitude: 0x0101,
                    audio_index: stride + stride + 10,
                },
                CuetoolsRepairCorrection {
                    row: 1,
                    column: 11,
                    magnitude: 0x7777,
                    audio_index: stride + 2 * stride + 11,
                },
                CuetoolsRepairCorrection {
                    row: 2,
                    column: 10,
                    magnitude: 0x0202,
                    audio_index: stride + 3 * stride + 10,
                },
            ]
        );
    }
}

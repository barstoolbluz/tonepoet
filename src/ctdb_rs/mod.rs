// CTDB Reed-Solomon codec over GF(2^16).
//
// Drop this file in a Rust crate as, for example, `src/ctdb_rs/mod.rs`, or
// include it with `mod ctdb_rs;`. It has no dependencies beyond `std`.
//
// The implementation intentionally follows the CUETools/CTDB orientation for
// encoding and decoding:
//   * GF polynomial: 0x1100B
//   * generator roots: alpha^0 .. alpha^(npar-1)
//   * data row `r` corresponds to locator exponent `data_rows - 1 - r`
//   * high-level offset parameters are CD stereo sample-pair offsets; helpers
//     with `_word_offset` accept already-converted u16 column offsets.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtdbRsError {
    InvalidNpar(usize),
    InvalidAudioLength { words: usize, stride: usize },
    InvalidParityLength { got: usize, need_at_least: usize },
    InvalidParityShape { stride: usize, npar: usize },
}

impl fmt::Display for CtdbRsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNpar(npar) => write!(
                f,
                "invalid npar {npar}; CTDB-compatible values are 4, 8, or 16"
            ),
            Self::InvalidAudioLength { words, stride } => write!(
                f,
                "audio has {words} u16 words; need at least two full CTDB rows of {stride} words"
            ),
            Self::InvalidParityLength { got, need_at_least } => write!(
                f,
                "parity byte buffer has {got} bytes; need at least {need_at_least} bytes"
            ),
            Self::InvalidParityShape { stride, npar } => write!(
                f,
                "parity matrix does not contain {stride} columns with {npar} symbols each"
            ),
        }
    }
}

impl std::error::Error for CtdbRsError {}

pub mod galois {
    pub const FIELD_SIZE: usize = 65_536;
    pub const MAX: usize = FIELD_SIZE - 1;
    pub const PRIMITIVE_POLY: u32 = 0x1100B;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Galois16 {
        exp_tbl: Vec<u16>,
        log_tbl: Vec<u16>,
    }

    impl Default for Galois16 {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Galois16 {
        pub fn new() -> Self {
            let (exp_tbl, log_tbl) = generate_tables();
            Self { exp_tbl, log_tbl }
        }

        pub fn exp_table(&self) -> &[u16] {
            &self.exp_tbl
        }

        pub fn log_table(&self) -> &[u16] {
            &self.log_tbl
        }

        #[inline]
        pub fn alpha_pow(&self, n: usize) -> u16 {
            self.exp_tbl[n % MAX]
        }

        #[inline]
        pub fn log(&self, a: u16) -> Option<usize> {
            if a == 0 {
                None
            } else {
                Some(self.log_tbl[a as usize] as usize)
            }
        }

        #[inline]
        pub fn mul(&self, a: u16, b: u16) -> u16 {
            if a == 0 || b == 0 {
                0
            } else {
                self.exp_tbl[self.log_tbl[a as usize] as usize + self.log_tbl[b as usize] as usize]
            }
        }

        #[inline]
        pub fn div(&self, a: u16, b: u16) -> u16 {
            assert!(b != 0, "division by zero in GF(2^16)");
            if a == 0 {
                0
            } else {
                self.exp_tbl[self.log_tbl[a as usize] as usize + MAX - self.log_tbl[b as usize] as usize]
            }
        }

        #[inline]
        pub fn pow(&self, a: u16, n: usize) -> u16 {
            if a == 0 {
                0
            } else {
                self.exp_tbl[(self.log_tbl[a as usize] as usize * n) % MAX]
            }
        }

        #[inline]
        pub fn mul_exp(&self, a: u16, n: usize) -> u16 {
            if a == 0 {
                0
            } else {
                self.exp_tbl[self.log_tbl[a as usize] as usize + (n % MAX)]
            }
        }

        #[inline]
        pub fn div_exp(&self, a: u16, n: usize) -> u16 {
            if a == 0 {
                0
            } else {
                self.exp_tbl[self.log_tbl[a as usize] as usize + MAX - (n % MAX)]
            }
        }

        #[inline]
        pub fn inv(&self, a: u16) -> u16 {
            assert!(a != 0, "inverse of zero in GF(2^16)");
            self.exp_tbl[MAX - self.log_tbl[a as usize] as usize]
        }
    }

    pub fn generate_tables() -> (Vec<u16>, Vec<u16>) {
        let mut exp_tbl = vec![0u16; MAX * 2];
        let mut log_tbl = vec![0u16; MAX + 1];

        let mut d: u32 = 1;
        for i in 0..MAX {
            exp_tbl[i] = d as u16;
            exp_tbl[MAX + i] = d as u16;
            log_tbl[d as usize] = i as u16;

            d <<= 1;
            if ((d >> 16) & 1) != 0 {
                d = (d ^ PRIMITIVE_POLY) & 0xFFFF;
            }
        }

        (exp_tbl, log_tbl)
    }
}

pub mod syndrome {
    use super::galois::{Galois16, MAX};
    use super::CtdbRsError;

    /// 10 CD sectors * 588 stereo sample frames/sector * 2 u16 channels.
    pub const STRIDE: usize = 11_760;

    pub const DEFAULT_NPAR: usize = 8;

    #[inline]
    pub fn i16_to_u16_bits(sample: i16) -> u16 {
        sample as u16
    }

    #[inline]
    pub fn u16_to_i16_bits(word: u16) -> i16 {
        word as i16
    }

    pub fn validate_npar(npar: usize) -> Result<(), CtdbRsError> {
        match npar {
            4 | 8 | 16 => Ok(()),
            other => Err(CtdbRsError::InvalidNpar(other)),
        }
    }

    pub fn data_row_count(audio_words: usize) -> Result<usize, CtdbRsError> {
        if audio_words < STRIDE * 2 {
            return Err(CtdbRsError::InvalidAudioLength {
                words: audio_words,
                stride: STRIDE,
            });
        }
        Ok(audio_words / STRIDE - 2)
    }

    /// CUETools-compatible LFSR generator coefficients.
    ///
    /// The returned vector has length `npar` and omits the leading monic
    /// coefficient. Coefficients are ordered for the systematic encoder shift
    /// register: index 0 is the x^(npar-1) coefficient, and index npar-1 is
    /// the constant term.
    pub fn make_generator_poly(gf: &Galois16, npar: usize) -> Result<Vec<u16>, CtdbRsError> {
        validate_npar(npar)?;

        let mut gx = vec![0u16; npar];
        gx[npar - 1] = 1;

        for root_exp in 0..npar {
            let root = gf.alpha_pow(root_exp);
            for j in 0..(npar - 1) {
                gx[j] = gf.mul(gx[j], root) ^ gx[j + 1];
            }
            gx[npar - 1] = gf.mul(gx[npar - 1], root);
        }

        Ok(gx)
    }

    #[inline]
    fn update_lfsr(gf: &Galois16, gx: &[u16], wr: &mut [u16], data_word: u16) {
        let npar = wr.len();
        let feedback = wr[0] ^ data_word;
        for i in 0..(npar - 1) {
            wr[i] = wr[i + 1] ^ gf.mul(gx[i], feedback);
        }
        wr[npar - 1] = gf.mul(gx[npar - 1], feedback);
    }

    pub fn compute_column_parity(
        gf: &Galois16,
        gx: &[u16],
        column_data: &[u16],
        npar: usize,
    ) -> Vec<u16> {
        assert_eq!(gx.len(), npar, "generator length must match npar");
        let mut wr = vec![0u16; npar];
        for &data_word in column_data {
            update_lfsr(gf, gx, &mut wr, data_word);
        }
        wr
    }

    pub fn compute_parity_matrix_from_audio(
        gf: &Galois16,
        audio: &[i16],
        npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError> {
        use rayon::prelude::*;

        let rows = data_row_count(audio.len())?;
        let gx = make_generator_poly(gf, npar)?;
        let mut parity = vec![vec![0u16; npar]; STRIDE];

        // Each disc-image column is an independent LFSR — parallelize across
        // STRIDE columns. `gf`/`gx`/`audio` are shared immutable borrows, and
        // each thread mutates only its own `parity[col]`. No data races.
        parity
            .par_iter_mut()
            .enumerate()
            .for_each(|(col, wr)| {
                for row in 0..rows {
                    let idx = STRIDE + row * STRIDE + col;
                    update_lfsr(gf, &gx, wr, i16_to_u16_bits(audio[idx]));
                }
            });

        Ok(parity)
    }

    pub fn compute_parity_matrix_from_u16_words(
        gf: &Galois16,
        words: &[u16],
        npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError> {
        use rayon::prelude::*;

        let rows = data_row_count(words.len())?;
        let gx = make_generator_poly(gf, npar)?;
        let mut parity = vec![vec![0u16; npar]; STRIDE];

        parity
            .par_iter_mut()
            .enumerate()
            .for_each(|(col, wr)| {
                for row in 0..rows {
                    let idx = STRIDE + row * STRIDE + col;
                    update_lfsr(gf, &gx, wr, words[idx]);
                }
            });

        Ok(parity)
    }

    pub fn try_bytes_to_parity(
        data: &[u8],
        stride: usize,
        npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError> {
        validate_npar(npar)?;
        let need = stride * npar * 2;
        if data.len() < need {
            return Err(CtdbRsError::InvalidParityLength {
                got: data.len(),
                need_at_least: need,
            });
        }

        let mut parity = vec![vec![0u16; npar]; stride];
        for i in 0..npar {
            for j in 0..stride {
                let offset = (j + i * stride) * 2;
                parity[j][i] = u16::from_le_bytes([data[offset], data[offset + 1]]);
            }
        }
        Ok(parity)
    }

    pub fn bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Vec<Vec<u16>> {
        try_bytes_to_parity(data, stride, npar).expect("invalid CTDB parity bytes")
    }

    pub fn parity_to_bytes(parity: &[Vec<u16>], stride: usize, npar: usize) -> Vec<u8> {
        assert!(
            parity.len() >= stride && parity.iter().take(stride).all(|row| row.len() >= npar),
            "parity matrix shape must be [stride][npar]"
        );

        let mut data = vec![0u8; stride * npar * 2];
        for i in 0..npar {
            for j in 0..stride {
                let offset = (j + i * stride) * 2;
                let bytes = parity[j][i].to_le_bytes();
                data[offset] = bytes[0];
                data[offset + 1] = bytes[1];
            }
        }
        data
    }

    fn check_parity_shape(
        parity: &[Vec<u16>],
        stride: usize,
        npar: usize,
    ) -> Result<(), CtdbRsError> {
        if parity.len() < stride || parity.iter().take(stride).any(|row| row.len() < npar) {
            Err(CtdbRsError::InvalidParityShape { stride, npar })
        } else {
            Ok(())
        }
    }

    /// Convert a parity matrix to a syndrome matrix, rotating columns by an
    /// already-converted u16-word offset.
    pub fn parity_to_syndrome_with_word_offset(
        gf: &Galois16,
        parity: &[Vec<u16>],
        npar: usize,
        stride: usize,
        word_offset: i64,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError> {
        validate_npar(npar)?;
        check_parity_shape(parity, stride, npar)?;

        let mut syn = vec![vec![0u16; npar]; stride];
        let stride_i64 = stride as i64;

        for y in 0..stride {
            let y1 = (y as i64 - word_offset).rem_euclid(stride_i64) as usize;
            for x1 in 0..npar {
                let par = parity[y1][x1];
                if par != 0 {
                    let llo = gf.log(par).expect("nonzero parity must have a log") + MAX;
                    for x in 0..npar {
                        syn[y][x] ^= gf.exp_table()[llo - (1 + x1) * x];
                    }
                }
            }
        }

        Ok(syn)
    }

    /// Convert a parity matrix to a syndrome matrix, treating `sample_offset`
    /// as a CD stereo sample-pair offset. Since parity columns are u16 words,
    /// this is equivalent to a word offset of `sample_offset * 2`.
    pub fn parity_to_syndrome(
        gf: &Galois16,
        parity: &[Vec<u16>],
        npar: usize,
        stride: usize,
        sample_offset: i32,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError> {
        parity_to_syndrome_with_word_offset(
            gf,
            parity,
            npar,
            stride,
            i64::from(sample_offset) * 2,
        )
    }

    pub fn xor_syndrome_matrices(lhs: &[Vec<u16>], rhs: &[Vec<u16>]) -> Vec<Vec<u16>> {
        assert_eq!(lhs.len(), rhs.len(), "matrix stride mismatch");
        lhs.iter()
            .zip(rhs)
            .map(|(a, b)| {
                assert_eq!(a.len(), b.len(), "matrix npar mismatch");
                a.iter().zip(b).map(|(&x, &y)| x ^ y).collect()
            })
            .collect()
    }
}

pub mod decoder {
    use super::galois::{Galois16, MAX};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LocatedError {
        pub row: usize,
        pub magnitude: u16,
    }

    #[inline]
    pub fn all_zero(words: &[u16]) -> bool {
        words.iter().all(|&x| x == 0)
    }

    pub fn berlekamp_massey(
        gf: &Galois16,
        syndromes: &[u16],
        npar: usize,
    ) -> Option<(Vec<u16>, usize)> {
        if syndromes.len() < npar {
            return None;
        }
        if all_zero(&syndromes[..npar]) {
            return Some((vec![1], 0));
        }

        let mut c = vec![0u16; npar + 1];
        let mut b = vec![0u16; npar + 1];
        c[0] = 1;
        b[0] = 1;

        let mut degree = 0usize;
        let mut m = 1usize;
        let mut last_discrepancy = 1u16;

        for n in 0..npar {
            let mut discrepancy = syndromes[n];
            for i in 1..=degree.min(n) {
                if c[i] != 0 && syndromes[n - i] != 0 {
                    discrepancy ^= gf.mul(c[i], syndromes[n - i]);
                }
            }

            if discrepancy == 0 {
                m += 1;
                continue;
            }

            let previous_c = c.clone();
            let scale = gf.div(discrepancy, last_discrepancy);
            if m <= npar {
                for j in 0..=(npar - m) {
                    if b[j] != 0 {
                        c[j + m] ^= gf.mul(scale, b[j]);
                    }
                }
            }

            if 2 * degree <= n {
                degree = n + 1 - degree;
                b = previous_c;
                last_discrepancy = discrepancy;
                m = 1;
            } else {
                m += 1;
            }
        }

        let actual_degree = c.iter().rposition(|&v| v != 0).unwrap_or(0);
        Some((c[..=actual_degree].to_vec(), actual_degree))
    }

    fn eval_poly_at_alpha_exp(gf: &Galois16, poly: &[u16], alpha_exp: usize) -> u16 {
        let mut out = 0u16;
        for (degree, &coeff) in poly.iter().enumerate() {
            if coeff != 0 {
                out ^= gf.mul_exp(coeff, (alpha_exp * degree) % MAX);
            }
        }
        out
    }

    /// Find error rows. This follows CUETools position orientation: row r has
    /// locator exponent `stridecount - 1 - r`.
    pub fn chien_search(
        gf: &Galois16,
        sigma: &[u16],
        error_count: usize,
        stridecount: usize,
    ) -> Option<Vec<usize>> {
        if error_count == 0 {
            return Some(Vec::new());
        }
        if stridecount == 0 || stridecount > MAX {
            return None;
        }

        let mut rows = Vec::with_capacity(error_count);
        for locator_exp in 0..stridecount {
            let z_exp = (MAX - (locator_exp % MAX)) % MAX;
            if eval_poly_at_alpha_exp(gf, sigma, z_exp) == 0 {
                rows.push(stridecount - 1 - locator_exp);
                if rows.len() > error_count {
                    return None;
                }
            }
        }

        if rows.len() == error_count {
            rows.sort_unstable();
            Some(rows)
        } else {
            None
        }
    }

    pub fn syndrome_evaluator(
        gf: &Galois16,
        syndromes: &[u16],
        sigma: &[u16],
        npar: usize,
    ) -> Vec<u16> {
        let mut omega = vec![0u16; npar];
        for (i, &a) in sigma.iter().enumerate() {
            if a == 0 || i >= npar {
                continue;
            }
            for j in 0..(npar - i) {
                let b = syndromes[j];
                if b != 0 {
                    omega[i + j] ^= gf.mul(a, b);
                }
            }
        }
        omega
    }

    fn sigma_derivative_at(gf: &Galois16, sigma: &[u16], z_exp: usize) -> u16 {
        let mut out = 0u16;
        for degree in (1..sigma.len()).step_by(2) {
            let coeff = sigma[degree];
            if coeff != 0 {
                out ^= gf.mul_exp(coeff, (z_exp * (degree - 1)) % MAX);
            }
        }
        out
    }

    /// Compute Forney magnitudes for row positions returned by `chien_search`.
    pub fn forney(
        gf: &Galois16,
        syndromes: &[u16],
        sigma: &[u16],
        error_positions: &[usize],
        npar: usize,
        stridecount: usize,
    ) -> Option<Vec<u16>> {
        if syndromes.len() < npar || stridecount == 0 || stridecount > MAX {
            return None;
        }

        let omega = syndrome_evaluator(gf, syndromes, sigma, npar);
        let mut magnitudes = Vec::with_capacity(error_positions.len());

        for &row in error_positions {
            if row >= stridecount {
                return None;
            }
            let locator_exp = stridecount - 1 - row;
            let z_exp = (MAX - (locator_exp % MAX)) % MAX;
            let omega_value = eval_poly_at_alpha_exp(gf, &omega, z_exp);
            let derivative_value = sigma_derivative_at(gf, sigma, z_exp);
            if derivative_value == 0 {
                return None;
            }
            let magnitude = gf.mul(
                gf.alpha_pow(locator_exp),
                gf.div(omega_value, derivative_value),
            );
            magnitudes.push(magnitude);
        }

        Some(magnitudes)
    }

    pub fn residual_after_correction(
        gf: &Galois16,
        syndromes: &[u16],
        corrections: &[LocatedError],
        npar: usize,
        stridecount: usize,
    ) -> Option<Vec<u16>> {
        if syndromes.len() < npar || stridecount == 0 || stridecount > MAX {
            return None;
        }
        let mut residual = syndromes[..npar].to_vec();

        for &LocatedError { row, magnitude } in corrections {
            if row >= stridecount {
                return None;
            }
            let locator_exp = stridecount - 1 - row;
            for k in 0..npar {
                residual[k] ^= gf.mul_exp(magnitude, (locator_exp * k) % MAX);
            }
        }

        Some(residual)
    }

    pub fn decode_column_syndromes(
        gf: &Galois16,
        syndromes: &[u16],
        npar: usize,
        stridecount: usize,
    ) -> Option<Vec<LocatedError>> {
        if syndromes.len() < npar {
            return None;
        }
        if all_zero(&syndromes[..npar]) {
            return Some(Vec::new());
        }

        let (sigma, error_count) = berlekamp_massey(gf, syndromes, npar)?;
        if error_count == 0 || error_count > npar / 2 {
            return None;
        }

        let positions = chien_search(gf, &sigma, error_count, stridecount)?;
        let magnitudes = forney(gf, syndromes, &sigma, &positions, npar, stridecount)?;
        let corrections: Vec<LocatedError> = positions
            .into_iter()
            .zip(magnitudes)
            .map(|(row, magnitude)| LocatedError { row, magnitude })
            .collect();

        let residual = residual_after_correction(gf, syndromes, &corrections, npar, stridecount)?;
        if all_zero(&residual) {
            Some(corrections)
        } else {
            None
        }
    }
}

pub mod codec {
    use super::decoder::{self, LocatedError};
    use super::galois::Galois16;
    use super::syndrome::{self, STRIDE};
    use super::CtdbRsError;
    use std::fmt;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CtdbCodec {
        gf: Galois16,
    }

    impl Default for CtdbCodec {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CtdbCodec {
        pub fn new() -> Self {
            Self {
                gf: Galois16::new(),
            }
        }

        pub fn galois(&self) -> &Galois16 {
            &self.gf
        }

        pub fn try_compute_parity(
            &self,
            audio: &[i16],
            npar: usize,
        ) -> Result<Vec<u8>, CtdbRsError> {
            let parity = syndrome::compute_parity_matrix_from_audio(&self.gf, audio, npar)?;
            Ok(syndrome::parity_to_bytes(&parity, STRIDE, npar))
        }

        /// Compute parity from interleaved i16 stereo PCM.
        ///
        /// Panics on invalid `npar` or too-short audio. Use
        /// `try_compute_parity` for fallible input handling.
        pub fn compute_parity(&self, audio: &[i16], npar: usize) -> Vec<u8> {
            self.try_compute_parity(audio, npar)
                .expect("invalid CTDB parity computation input")
        }

        pub fn try_verify(
            &self,
            audio: &[i16],
            parity_bytes: &[u8],
            npar: usize,
            offset: i32,
        ) -> Result<VerifyResult, RepairError> {
            self.try_verify_with_sample_offset(audio, parity_bytes, npar, offset)
        }

        pub fn verify(
            &self,
            audio: &[i16],
            parity_bytes: &[u8],
            npar: usize,
            offset: i32,
        ) -> VerifyResult {
            self.try_verify(audio, parity_bytes, npar, offset)
                .expect("invalid CTDB verify input")
        }

        pub fn try_verify_with_sample_offset(
            &self,
            audio: &[i16],
            parity_bytes: &[u8],
            npar: usize,
            sample_offset: i32,
        ) -> Result<VerifyResult, RepairError> {
            self.try_verify_with_word_offset(
                audio,
                parity_bytes,
                npar,
                i64::from(sample_offset) * 2,
            )
        }

        pub fn try_verify_with_word_offset(
            &self,
            audio: &[i16],
            parity_bytes: &[u8],
            npar: usize,
            word_offset: i64,
        ) -> Result<VerifyResult, RepairError> {
            let syndromes = self.syndrome_delta_with_word_offset(audio, parity_bytes, npar, word_offset)?;
            Ok(VerifyResult::from_syndromes(&syndromes))
        }

        /// Repair audio in-place. The `offset` argument is a CD stereo
        /// sample-pair offset and is converted internally to two u16 columns.
        pub fn repair(
            &self,
            audio: &mut [i16],
            parity_bytes: &[u8],
            npar: usize,
            offset: i32,
        ) -> Result<RepairResult, RepairError> {
            self.repair_with_sample_offset(audio, parity_bytes, npar, offset)
        }

        pub fn repair_with_sample_offset(
            &self,
            audio: &mut [i16],
            parity_bytes: &[u8],
            npar: usize,
            sample_offset: i32,
        ) -> Result<RepairResult, RepairError> {
            self.repair_with_word_offset(
                audio,
                parity_bytes,
                npar,
                i64::from(sample_offset) * 2,
            )
        }

        pub fn repair_with_word_offset(
            &self,
            audio: &mut [i16],
            parity_bytes: &[u8],
            npar: usize,
            word_offset: i64,
        ) -> Result<RepairResult, RepairError> {
            syndrome::validate_npar(npar).map_err(RepairError::from)?;
            let stridecount = syndrome::data_row_count(audio.len()).map_err(RepairError::from)?;
            let syndromes = self.syndrome_delta_with_word_offset(audio, parity_bytes, npar, word_offset)?;
            let max_correctable = npar / 2;

            let mut all_corrections: Vec<(usize, usize, u16)> = Vec::new();
            for (column, column_syndromes) in syndromes.iter().enumerate() {
                if decoder::all_zero(column_syndromes) {
                    continue;
                }

                let (sigma, errors_found) = decoder::berlekamp_massey(&self.gf, column_syndromes, npar)
                    .ok_or(RepairError::Uncorrectable {
                        column,
                        errors_found: 0,
                        max_correctable,
                    })?;

                if errors_found == 0 || errors_found > max_correctable {
                    return Err(RepairError::Uncorrectable {
                        column,
                        errors_found,
                        max_correctable,
                    });
                }

                let positions = decoder::chien_search(&self.gf, &sigma, errors_found, stridecount)
                    .ok_or(RepairError::Uncorrectable {
                        column,
                        errors_found,
                        max_correctable,
                    })?;
                let magnitudes = decoder::forney(
                    &self.gf,
                    column_syndromes,
                    &sigma,
                    &positions,
                    npar,
                    stridecount,
                )
                .ok_or(RepairError::Uncorrectable {
                    column,
                    errors_found,
                    max_correctable,
                })?;

                let corrections: Vec<LocatedError> = positions
                    .iter()
                    .copied()
                    .zip(magnitudes.iter().copied())
                    .map(|(row, magnitude)| LocatedError { row, magnitude })
                    .collect();
                let residual = decoder::residual_after_correction(
                    &self.gf,
                    column_syndromes,
                    &corrections,
                    npar,
                    stridecount,
                )
                .ok_or(RepairError::Uncorrectable {
                    column,
                    errors_found,
                    max_correctable,
                })?;
                if !decoder::all_zero(&residual) {
                    return Err(RepairError::Uncorrectable {
                        column,
                        errors_found,
                        max_correctable,
                    });
                }

                for correction in corrections {
                    all_corrections.push((correction.row, column, correction.magnitude));
                }
            }

            for &(row, column, magnitude) in &all_corrections {
                let idx = STRIDE + row * STRIDE + column;
                let fixed = syndrome::i16_to_u16_bits(audio[idx]) ^ magnitude;
                audio[idx] = syndrome::u16_to_i16_bits(fixed);
            }

            Ok(RepairResult {
                corrected_samples: all_corrections.len(),
                error_positions: all_corrections
                    .iter()
                    .map(|&(row, column, _)| (row, column))
                    .collect(),
            })
        }

        fn syndrome_delta_with_word_offset(
            &self,
            audio: &[i16],
            parity_bytes: &[u8],
            npar: usize,
            word_offset: i64,
        ) -> Result<Vec<Vec<u16>>, RepairError> {
            syndrome::validate_npar(npar).map_err(RepairError::from)?;
            let computed = syndrome::compute_parity_matrix_from_audio(&self.gf, audio, npar)
                .map_err(RepairError::from)?;
            let provided = syndrome::try_bytes_to_parity(parity_bytes, STRIDE, npar)
                .map_err(RepairError::from)?;

            let computed_syndrome = syndrome::parity_to_syndrome_with_word_offset(
                &self.gf,
                &computed,
                npar,
                STRIDE,
                0,
            )
            .map_err(RepairError::from)?;
            let provided_syndrome = syndrome::parity_to_syndrome_with_word_offset(
                &self.gf,
                &provided,
                npar,
                STRIDE,
                word_offset,
            )
            .map_err(RepairError::from)?;

            Ok(syndrome::xor_syndrome_matrices(
                &computed_syndrome,
                &provided_syndrome,
            ))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct VerifyResult {
        pub matches: bool,
        pub error_columns: usize,
        pub nonzero_syndromes: usize,
        /// Per-column syndrome bitwise OR. A value of 0 means the column
        /// matches; nonzero means at least one syndrome symbol is nonzero.
        pub column_magnitudes: Vec<u16>,
    }

    impl VerifyResult {
        pub fn from_syndromes(syndromes: &[Vec<u16>]) -> Self {
            let mut error_columns = 0usize;
            let mut nonzero_syndromes = 0usize;
            let mut column_magnitudes = Vec::with_capacity(syndromes.len());

            for column in syndromes {
                let mut magnitude = 0u16;
                for &value in column {
                    magnitude |= value;
                    if value != 0 {
                        nonzero_syndromes += 1;
                    }
                }
                if magnitude != 0 {
                    error_columns += 1;
                }
                column_magnitudes.push(magnitude);
            }

            Self {
                matches: error_columns == 0,
                error_columns,
                nonzero_syndromes,
                column_magnitudes,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RepairResult {
        pub corrected_samples: usize,
        pub error_positions: Vec<(usize, usize)>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RepairError {
        Uncorrectable {
            column: usize,
            errors_found: usize,
            max_correctable: usize,
        },
        InvalidParity(String),
        InvalidInput(String),
    }

    impl From<CtdbRsError> for RepairError {
        fn from(value: CtdbRsError) -> Self {
            match value {
                CtdbRsError::InvalidParityLength { .. }
                | CtdbRsError::InvalidParityShape { .. } => Self::InvalidParity(value.to_string()),
                other => Self::InvalidInput(other.to_string()),
            }
        }
    }

    impl fmt::Display for RepairError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Uncorrectable {
                    column,
                    errors_found,
                    max_correctable,
                } => write!(
                    f,
                    "column {column} is uncorrectable: found {errors_found} errors, max correctable is {max_correctable}"
                ),
                Self::InvalidParity(msg) => write!(f, "invalid parity: {msg}"),
                Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            }
        }
    }

    impl std::error::Error for RepairError {}
}

pub use codec::{CtdbCodec, RepairError, RepairResult, VerifyResult};
pub use decoder::{berlekamp_massey, chien_search, forney, LocatedError};
pub use galois::Galois16;
pub use syndrome::{bytes_to_parity, parity_to_bytes, DEFAULT_NPAR, STRIDE};

#[cfg(test)]
mod tests {
    use super::codec::CtdbCodec;
    use super::decoder;
    use super::galois::Galois16;
    use super::syndrome;
    use super::STRIDE;

    #[test]
    fn galois_tables_match_ctdb_vectors() {
        let gf = Galois16::new();
        assert_eq!(gf.exp_table()[0], 1);
        assert_eq!(gf.exp_table()[1], 2);
        assert_eq!(gf.exp_table()[15], 32_768);
        assert_eq!(gf.exp_table()[16], 0x100B);
    }

    #[test]
    fn generator_coefficients_match_cuetools_orientation() {
        let gf = Galois16::new();
        let gx = syndrome::make_generator_poly(&gf, 4).unwrap();
        assert_eq!(gx, vec![15, 54, 120, 64]);
    }

    #[test]
    fn parity_serialization_round_trips() {
        let parity = vec![vec![1u16, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10, 11, 12]];
        let bytes = syndrome::parity_to_bytes(&parity, 3, 4);
        let decoded = syndrome::try_bytes_to_parity(&bytes, 3, 4).unwrap();
        assert_eq!(decoded, parity);
    }

    /// Parallel `compute_parity_matrix_from_audio` must produce byte-identical
    /// output to a sequential reference loop. Tests against three rows of
    /// non-trivial synthetic audio so any per-column LFSR state leak between
    /// rayon threads would flip at least one symbol.
    #[test]
    fn parity_matrix_parallel_matches_sequential_reference() {
        let gf = Galois16::new();
        let npar = 4;
        let rows = 3;
        let total = STRIDE * (rows + 2);
        let mut audio = vec![0i16; total];
        // Fill the protected region with a deterministic non-zero pattern; leave
        // the leadin/leadout zeros (matches CTDB convention).
        for idx in STRIDE..STRIDE + rows * STRIDE {
            let word = ((idx as u32 * 257 + 0x1234) & 0xffff) as u16;
            audio[idx] = word as i16;
        }

        let parallel = syndrome::compute_parity_matrix_from_audio(&gf, &audio, npar).unwrap();

        // Sequential reference implementation, identical to the loop body
        // before the rayon conversion.
        let gx = syndrome::make_generator_poly(&gf, npar).unwrap();
        let mut sequential = vec![vec![0u16; npar]; STRIDE];
        for col in 0..STRIDE {
            let mut wr = vec![0u16; npar];
            for row in 0..rows {
                let idx = STRIDE + row * STRIDE + col;
                let data_word = audio[idx] as u16;
                let feedback = wr[0] ^ data_word;
                let mut new_wr = vec![0u16; npar];
                for i in 0..(npar - 1) {
                    new_wr[i] = wr[i + 1] ^ gf.mul(gx[i], feedback);
                }
                new_wr[npar - 1] = gf.mul(gx[npar - 1], feedback);
                wr = new_wr;
            }
            sequential[col] = wr;
        }

        assert_eq!(parallel, sequential);
    }

    /// Determinism check — the same input should always produce the same
    /// parity matrix, regardless of rayon's scheduling.
    #[test]
    fn parity_matrix_parallel_is_deterministic() {
        let gf = Galois16::new();
        let npar = 8;
        let rows = 5;
        let mut audio = vec![0i16; STRIDE * (rows + 2)];
        for idx in STRIDE..STRIDE + rows * STRIDE {
            audio[idx] = (idx as i32 * 31 - 0x4000) as i16;
        }

        let a = syndrome::compute_parity_matrix_from_audio(&gf, &audio, npar).unwrap();
        let b = syndrome::compute_parity_matrix_from_audio(&gf, &audio, npar).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn decoder_recovers_column_error_locations_and_magnitudes() {
        let gf = Galois16::new();
        let npar = 8;
        let rows = 120usize;
        let gx = syndrome::make_generator_poly(&gf, npar).unwrap();
        let mut column = vec![0u16; rows];
        column[0] = 0x1111;
        column[7] = 0x2222;
        column[119] = 0x3333;
        let parity = syndrome::compute_column_parity(&gf, &gx, &column, npar);
        let matrix = vec![parity];
        let mut syn_matrix = syndrome::parity_to_syndrome_with_word_offset(&gf, &matrix, npar, 1, 0)
            .unwrap();
        let syn = syn_matrix.remove(0);

        let corrections = decoder::decode_column_syndromes(&gf, &syn, npar, rows).unwrap();
        assert_eq!(
            corrections,
            vec![
                decoder::LocatedError {
                    row: 0,
                    magnitude: 0x1111
                },
                decoder::LocatedError {
                    row: 7,
                    magnitude: 0x2222
                },
                decoder::LocatedError {
                    row: 119,
                    magnitude: 0x3333
                },
            ]
        );
    }

    #[test]
    fn codec_repairs_corrupted_audio() {
        let codec = CtdbCodec::new();
        let npar = 4;
        let full_rows = 5usize; // lead-in row, 3 protected rows, lead-out row.
        let mut audio = vec![0i16; full_rows * STRIDE];
        for (i, sample) in audio.iter_mut().enumerate() {
            let word = ((i as u32 * 257 + 0x1234) & 0xFFFF) as u16;
            *sample = word as i16;
        }
        let original = audio.clone();
        let parity = codec.compute_parity(&audio, npar);

        let flips = [
            (0usize, 10usize, 0x0101u16),
            (2usize, 10usize, 0x0202u16),
            (1usize, 11usize, 0x7777u16),
        ];
        for &(row, column, mask) in &flips {
            let idx = STRIDE + row * STRIDE + column;
            audio[idx] = ((audio[idx] as u16) ^ mask) as i16;
        }

        let bad = codec.verify(&audio, &parity, npar, 0);
        assert!(!bad.matches);
        assert_eq!(bad.error_columns, 2);

        let repair = codec.repair(&mut audio, &parity, npar, 0).unwrap();
        assert_eq!(repair.corrected_samples, 3);
        assert_eq!(audio, original);

        let good = codec.verify(&audio, &parity, npar, 0);
        assert!(good.matches);
        assert_eq!(good.error_columns, 0);
    }
}

# CTDB Reed-Solomon Codec — Rust Implementation Spec

## Overview

Implement a Reed-Solomon error correction codec over GF(2^16) that is byte-compatible with the CUETools Database (CTDB) format. The codec must:

1. Compute parity/syndromes from CD audio (encode)
2. Verify audio against CTDB parity data (syndrome comparison)
3. Repair damaged audio samples using CTDB parity (decode)

This is a self-contained pure-math module with no I/O, no TUI, no network dependencies. It operates on slices of `u16` values representing 16-bit audio samples.

## Module Structure

```
src/ctdb_rs/
  mod.rs          // re-exports
  galois.rs       // GF(2^16) arithmetic
  syndrome.rs     // syndrome/parity computation
  decoder.rs      // Berlekamp-Massey + Chien + Forney
  codec.rs        // high-level encode/verify/repair API
```

---

## Part 1: GF(2^16) Arithmetic (`galois.rs`)

### Primitive Polynomial

```
P = 0x1100B  (x^16 + x^12 + x^3 + x + 1)
```

### Field Parameters

```rust
const FIELD_SIZE: usize = 65536;  // 2^16
const MAX: usize = 65535;         // 2^16 - 1
```

### Lookup Tables

Two tables:
- `EXP_TBL: [u16; 131070]` — doubled exp table (MAX * 2 entries)
- `LOG_TBL: [u16; 65536]` — log table (MAX + 1 entries)

### Table Generation

```rust
fn generate_tables() -> (Vec<u16>, Vec<u16>) {
    let mut exp_tbl = vec![0u16; MAX * 2];
    let mut log_tbl = vec![0u16; MAX + 1];
    
    let mut d: u32 = 1;
    for i in 0..MAX {
        exp_tbl[i] = d as u16;
        exp_tbl[MAX + i] = d as u16;  // doubled for mod-free multiply
        log_tbl[d as usize] = i as u16;
        d <<= 1;
        if (d >> 16) & 1 != 0 {
            d = (d ^ 0x1100B) & 0xFFFF;
        }
    }
    
    (exp_tbl, log_tbl)
}
```

### Operations

```rust
fn mul(a: u16, b: u16) -> u16
    if a == 0 || b == 0 { return 0; }
    EXP_TBL[LOG_TBL[a] as usize + LOG_TBL[b] as usize]

fn div(a: u16, b: u16) -> u16
    if a == 0 { return 0; }
    assert!(b != 0);
    EXP_TBL[LOG_TBL[a] as usize + MAX - LOG_TBL[b] as usize]

fn pow(a: u16, n: usize) -> u16
    if a == 0 { return 0; }
    EXP_TBL[(LOG_TBL[a] as usize * n) % MAX]

fn mul_exp(a: u16, n: usize) -> u16
    // multiply a by alpha^n
    if a == 0 { return 0; }
    EXP_TBL[LOG_TBL[a] as usize + n]

fn div_exp(a: u16, n: usize) -> u16
    // divide a by alpha^n
    if a == 0 { return 0; }
    EXP_TBL[LOG_TBL[a] as usize + MAX - (n % MAX)]

fn inv(a: u16) -> u16
    assert!(a != 0);
    EXP_TBL[MAX - LOG_TBL[a] as usize]
```

### Struct

```rust
pub struct Galois16 {
    exp_tbl: Vec<u16>,
    log_tbl: Vec<u16>,
}

impl Galois16 {
    pub fn new() -> Self { /* generate tables */ }
    pub fn mul(&self, a: u16, b: u16) -> u16 { ... }
    pub fn div(&self, a: u16, b: u16) -> u16 { ... }
    pub fn pow(&self, a: u16, n: usize) -> u16 { ... }
    pub fn mul_exp(&self, a: u16, n: usize) -> u16 { ... }
    pub fn div_exp(&self, a: u16, n: usize) -> u16 { ... }
    pub fn inv(&self, a: u16) -> u16 { ... }
}
```

---

## Part 2: Syndrome/Parity Computation (`syndrome.rs`)

### Constants

```rust
/// 10 CD sectors × 588 samples/sector × 2 channels = 11760 u16 words per row
pub const STRIDE: usize = 11760;

/// Default parity symbol count (error correction capacity = npar/2)
pub const DEFAULT_NPAR: usize = 8;
```

### Audio Matrix Layout

CD audio is interleaved 16-bit stereo PCM: `[L0, R0, L1, R1, ...]`

Each 32-bit sample pair is split into two u16 values:
- `lo = sample_pair & 0xFFFF` (left channel as unsigned u16)
- `hi = sample_pair >> 16` (right channel as unsigned u16)

The audio is arranged as a matrix:
- **Columns**: `STRIDE` = 11760 positions (left and right alternate: col 0 = L of sample 0 in the row, col 1 = R of sample 0, col 2 = L of sample 1, etc.)
- **Rows**: `stridecount = total_u16_words / STRIDE - 2` (minus 2 for leadin/leadout)

Reed-Solomon encoding runs independently on each of the 11760 columns.

### Generator Polynomial

Roots are `alpha^0, alpha^1, ..., alpha^(npar-1)`.

```rust
fn make_generator_poly(gf: &Galois16, npar: usize) -> Vec<u16> {
    // g(x) = (x - alpha^0)(x - alpha^1)...(x - alpha^(npar-1))
    let mut gx = vec![0u16; npar + 1];
    gx[0] = 1;
    for i in 0..npar {
        // multiply by (x - alpha^i)
        let root = gf.exp_tbl[i]; // alpha^i
        for j in (1..=i+1).rev() {
            gx[j] = gx[j] ^ gf.mul(gx[j-1], root);
        }
    }
    gx
}
```

### Parity Computation (Encoding)

For each column, process all rows through a systematic RS LFSR:

```rust
fn compute_column_parity(
    gf: &Galois16,
    gx: &[u16],        // generator polynomial coefficients [npar]
    column_data: &[u16], // one value per row for this column
    npar: usize,
) -> Vec<u16> {
    let mut wr = vec![0u16; npar]; // working register
    
    for &data_word in column_data {
        let feedback = wr[0] ^ data_word;
        // Shift left
        for i in 0..npar-1 {
            wr[i] = wr[i+1] ^ gf.mul(gx[i], feedback);
        }
        wr[npar-1] = gf.mul(gx[npar-1], feedback);
    }
    
    wr
}
```

Full parity for the disc: run `compute_column_parity` for each of the STRIDE columns.

### Parity ↔ Syndrome Conversion

**Parity to Syndrome** (for verification/repair):

```rust
fn parity_to_syndrome(
    gf: &Galois16,
    parity: &[Vec<u16>],  // [stride][npar] parity values
    npar: usize,
    stride: usize,
    offset: i32,          // drive read offset in u16 positions
) -> Vec<Vec<u16>> {      // [stride][npar] syndromes
    let stride2 = stride;
    let mut syn = vec![vec![0u16; npar]; stride2];
    
    for y in 0..stride2 {
        let y1 = ((y as i64 - offset as i64 * 2 + stride2 as i64 * 2) % stride2 as i64) as usize;
        for x1 in 0..npar {
            let par = parity[y1][x1];
            if par != 0 {
                let llo = gf.log_tbl[par as usize] as usize + MAX;
                for x in 0..npar {
                    syn[y][x] ^= gf.exp_tbl[llo - (1 + x1) * x];
                }
            }
        }
    }
    
    syn
}
```

### Parity Byte Serialization

**Column-major, little-endian u16:**

```rust
fn bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Vec<Vec<u16>> {
    // data layout: for i in 0..npar, for j in 0..stride: u16 at data[(j + i*stride)*2..]
    let mut parity = vec![vec![0u16; npar]; stride];
    for i in 0..npar {
        for j in 0..stride {
            let offset = (j + i * stride) * 2;
            parity[j][i] = u16::from_le_bytes([data[offset], data[offset + 1]]);
        }
    }
    parity
}

fn parity_to_bytes(parity: &[Vec<u16>], stride: usize, npar: usize) -> Vec<u8> {
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
```

---

## Part 3: RS Decoder (`decoder.rs`)

### Berlekamp-Massey Algorithm

Computes the error-locator polynomial `sigma(x)` from syndromes.

```rust
fn berlekamp_massey(
    gf: &Galois16,
    syndromes: &[u16],  // [npar] syndrome values for one column
    npar: usize,
) -> (Vec<u16>, usize)  // (sigma coefficients, error count)
```

Standard BM iteration with two alternating polynomial buffers. Returns sigma(x) and its degree (= number of errors). If degree > npar/2, the errors are uncorrectable.

### Chien Search

Finds the roots of sigma(x) by evaluating it at `alpha^(-i)` for `i = 0..stridecount-1`. A root at `alpha^(-i)` means row `i` has an error.

```rust
fn chien_search(
    gf: &Galois16,
    sigma: &[u16],
    error_count: usize,
    stridecount: usize,
) -> Option<Vec<usize>>  // error positions (row indices), or None if wrong number of roots
```

Must find exactly `error_count` roots. If not, decoding fails (uncorrectable).

### Forney's Algorithm

Computes the error magnitude at each located position:

```rust
fn forney(
    gf: &Galois16,
    syndromes: &[u16],
    sigma: &[u16],
    error_positions: &[usize],
    npar: usize,
) -> Vec<u16>  // error magnitudes (one per error position)
```

For each error at position `pos`:
1. Compute `omega(x) = sigma(x) * S(x) mod x^npar` (S(x) = syndrome polynomial)
2. Compute `sigma'(x)` = formal derivative of sigma (in char 2: odd-indexed coefficients only)
3. `E = alpha^pos * omega(alpha^(-pos)) / sigma'(alpha^(-pos))`

The correction is `data[pos, col] ^= E` (XOR in GF(2^16)).

---

## Part 4: High-Level API (`codec.rs`)

```rust
pub struct CtdbCodec {
    gf: Galois16,
}

impl CtdbCodec {
    pub fn new() -> Self;
    
    /// Compute parity from audio samples.
    /// `audio` is interleaved i16 stereo PCM (reinterpreted as u16).
    /// Returns parity bytes in CTDB serialization format.
    pub fn compute_parity(
        &self,
        audio: &[i16],
        npar: usize,
    ) -> Vec<u8>;
    
    /// Verify audio against CTDB parity data.
    /// Returns per-column syndrome magnitudes (0 = match).
    pub fn verify(
        &self,
        audio: &[i16],
        parity_bytes: &[u8],
        npar: usize,
        offset: i32,
    ) -> VerifyResult;
    
    /// Repair audio using CTDB parity data.
    /// Modifies `audio` in-place. Returns the number of corrected samples,
    /// or an error if the damage exceeds correction capacity.
    pub fn repair(
        &self,
        audio: &mut [i16],
        parity_bytes: &[u8],
        npar: usize,
        offset: i32,
    ) -> Result<RepairResult, RepairError>;
}

pub struct VerifyResult {
    pub matches: bool,
    pub error_columns: usize,  // columns with non-zero syndromes
}

pub struct RepairResult {
    pub corrected_samples: usize,
    pub error_positions: Vec<(usize, usize)>,  // (row, column) pairs
}

pub enum RepairError {
    Uncorrectable { column: usize, errors_found: usize, max_correctable: usize },
    InvalidParity(String),
}
```

---

## Key Constraints

- All GF(2^16) arithmetic uses the primitive polynomial `0x1100B`
- Generator polynomial roots: `alpha^0` through `alpha^(npar-1)`
- Audio treated as unsigned u16 (i16 reinterpreted via bit cast, NOT numeric conversion)
- Parity serialization: column-major, little-endian u16
- No sample skipping (unlike AccurateRip, CTDB processes the full disc)
- Drive read offset handled by column rotation in parity↔syndrome conversion
- npar values: 4, 8, or 16 (8 is the common case)
- Error correction capacity: npar/2 errors per column (4 with npar=8)

## Test Vectors

The implementation should be validated against CUETools by:
1. Computing parity on a known audio file and comparing against CTDB's stored parity
2. Introducing known errors (flip specific samples) and verifying repair restores the original
3. Verifying that the GF(2^16) tables match: `exp_tbl[0] = 1`, `exp_tbl[1] = 2`, `exp_tbl[15] = 32768`, `exp_tbl[16] = 0x100B` (after reduction)

## Dependencies

None. Pure computation — no I/O, no allocator tricks, no unsafe required (though unsafe pointer casts for i16↔u16 reinterpretation are acceptable for performance).

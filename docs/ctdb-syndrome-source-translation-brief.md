# CTDB syndrome verification — source-translation request

## What we're asking you to do

We've tried twice to derive a CTDB Reed-Solomon syndrome fast-path from prose descriptions of CUETools.NET's algorithm. Both rounds produced subtle sign/transform bugs we only caught at runtime. **We're done guessing.** Please:

1. Web-fetch the relevant CUETools.NET C# source from `https://github.com/gchudov/cuetools.net`.
2. Quote the literal C# code of the functions involved (with file path and line range).
3. Translate them line-by-line into Rust against the codec primitives listed below. Comment each translated block with `// matches <file>:<line>` so we can audit.
4. Where there's an ambiguity or the C# uses helpers we don't have, point it out — don't paper over.
5. If possible, derive a small numeric test fixture (intermediate values) so we can pin down divergence empirically the next time something breaks.

You're more reliable than us at this. We're not RS-codec experts; we keep introducing transform errors when we paraphrase. Please go to source.

## Files to fetch (best-guess paths — confirm against the repo tree)

- `CUETools.Codecs/Galois.cs` — GF(2^16) operations
- `CUETools.Codecs/AccurateRipVerify.cs` — has `GetSyndrome(npar, columns, offset)` which is the function CUETools' verifier compares against entries' stored rows
- `CUETools.Codecs/ParityToSyndrome.cs` — has `Bytes2Syndrome`, `Syndrome2Bytes`, `Parity2Syndrome`, `Syndrome2Parity` conversions
- `CUETools.CTDB/CDRepairEncode.cs` — has `FindOffset(entry.syndrome, entry.crc, ...)` which is the actual offset-trial verifier we want to mirror
- `CUETools.CTDB/CTDBResponseEntry.cs` — XML attribute schema
- `CUETools.CTDB/DBEntry.cs` — normalizes XML into `ushort[,]` syndrome (`Bytes2Syndrome` for `syndrome=…`, `Parity2Syndrome` for legacy inline `parity=…`)
- `CUETools.CTDB/CUEToolsDB.cs` — `DoVerify` calls `verify.FindOffset(...)` per entry

The high-confidence canonical entry stores both `syndrome="…"` and `hasparity="<URL>"`; legacy inline `parity="…"` is the small NPAR=8 form.

## Empirical context (verified, ground truth)

Test disc: Allman Brothers *At Fillmore East* Disc 2 (Japan SHM-CD), 4 per-track FLACs. CUETools-on-Windows reports `CTDB: verified OK, confidence 896`.

CTDB query (sent and verified):
```
GET https://db.cue.tools/lookup2.php?version=3&ctdb=1&toc=0:24159:127166:278762:309864
```

Canonical entry (confidence 896):
```xml
<entry confidence="896" crc32="13f525de" hasparity="http://p.cuetools.net/113068" id="113068"
       npar="16" stride="5880" syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="
       toc="0:24159:127166:278762:309864" trackcrcs="40c5dc10 65dfcc8a 1ef7b539 d21a8789" />
```

Empirically confirmed (with Python and the downloaded blob):
- The `hasparity` URL serves a 376_320-byte blob = `STRIDE × NPAR × 2` bytes for `STRIDE=11_760` (i16-words count), `NPAR=16`.
- The blob layout matches our codec's `bytes_to_parity`: `parity[j][i] = u16::from_le_bytes(blob[(j + i*stride)*2..+2])` for `j in 0..stride`, `i in 0..npar`.
- The base64-decoded `syndrome` attribute (32 bytes / 16 LE u16) is **byte-for-byte identical** to `parity[0][0..NPAR]` of that blob.

Our rip's situation:
- 4-tuple `trackcrcs` of our rip: `c17a4a77 fabb2be6 52898fe0 96af30c8`.
- Canonical 896 entry's `trackcrcs`: `40c5dc10 65dfcc8a 1ef7b539 d21a8789`.
- Our 4-tuple is **not** present as a single entry anywhere in the response (~36 entries). Tracks 1–3 of our rip match many low-confidence entries; track 4 doesn't pair with our track 3 in any submitter's set.
- CUETools nonetheless reports `verified OK confidence 896` — i.e. via Reed-Solomon column-equivalence, not byte-equality.

EAC log says `Read offset correction: 6, Fill up missing offset samples with silence: Yes`.

Disc image we hand the codec is `[STRIDE i16 zeros] + concat(track audio i16) + [STRIDE i16 zeros]`. For this disc that's `364_423_584` total i16 samples, so `audio.len() / STRIDE - 2 = 30986` data rows (codec's `data_row_count`).

## Our codec — primitives you can call

`src/ctdb_rs/mod.rs`. GF(2^16) primitive poly `0x1100B`, generator roots `α^0..α^(npar-1)`. Codec was hand-written to match CUETools/CTDB orientation, but we have NOT validated it against an actual CUETools-produced fixture (only against itself via 5 round-trip unit tests).

```rust
pub mod galois {
    pub const FIELD_SIZE: usize = 65_536;
    pub const MAX: usize = FIELD_SIZE - 1;
    pub const PRIMITIVE_POLY: u32 = 0x1100B;

    pub struct Galois16 { /* … */ }
    impl Galois16 {
        pub fn new() -> Self;
        pub fn alpha_pow(&self, n: usize) -> u16;       // returns α^n
        pub fn mul(&self, a: u16, b: u16) -> u16;
        pub fn div(&self, a: u16, b: u16) -> u16;
        pub fn pow(&self, a: u16, n: usize) -> u16;
        pub fn inv(&self, a: u16) -> u16;
        pub fn log(&self, a: u16) -> Option<usize>;     // returns log_α(a)
        pub fn exp_table(&self) -> &[u16];              // exp_table[i] = α^i for i in 0..MAX, with extra range for negative-arithmetic safety
        pub fn log_table(&self) -> &[u16];
    }
}

pub mod syndrome {
    pub const STRIDE: usize = 11_760;        // i16 count = 5_880 stereo pairs
    pub const DEFAULT_NPAR: usize = 8;

    /// Compute the disc-image parity matrix.
    /// `audio` length = STRIDE leadin + tracks + STRIDE leadout (i16).
    /// Returns parity[j][i] for j in 0..STRIDE (disc-image-column), i in 0..npar.
    pub fn compute_parity_matrix_from_audio(
        gf: &Galois16, audio: &[i16], npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    /// Single-column parity. `column_data` is the column's u16 stream.
    pub fn compute_column_parity(
        gf: &Galois16, gx: &[u16], column_data: &[u16], npar: usize,
    ) -> Vec<u16>;

    pub fn make_generator_poly(gf: &Galois16, npar: usize) -> Result<Vec<u16>, CtdbRsError>;

    /// Layout: bytes[(j + i*stride)*2 .. + 2] = parity[j][i] as little-endian u16.
    pub fn try_bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Result<Vec<Vec<u16>>, CtdbRsError>;
    pub fn bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Vec<Vec<u16>>;
    pub fn parity_to_bytes(parity: &[Vec<u16>], stride: usize, npar: usize) -> Vec<u8>;

    /// Convert a parity matrix to a syndrome matrix, rotating columns by an
    /// already-converted u16-word offset.
    pub fn parity_to_syndrome_with_word_offset(
        gf: &Galois16, parity: &[Vec<u16>],
        npar: usize, stride: usize, word_offset: i64,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    /// Convert a parity matrix to a syndrome matrix; sample_offset is in
    /// stereo sample pairs (multiplies internally by 2 for word offset).
    pub fn parity_to_syndrome(
        gf: &Galois16, parity: &[Vec<u16>],
        npar: usize, stride: usize, sample_offset: i32,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    pub fn xor_syndrome_matrices(lhs: &[Vec<u16>], rhs: &[Vec<u16>]) -> Vec<Vec<u16>>;
}

pub mod decoder {
    pub fn berlekamp_massey(gf: &Galois16, syndromes: &[u16], npar: usize)
        -> Option<(Vec<u16>, usize)>;   // returns (sigma, errors_found)

    /// Roots in [0..stridecount). Returns Some only if all errors_found roots
    /// fall inside the data range.
    pub fn chien_search(gf: &Galois16, sigma: &[u16],
                        error_count: usize, stridecount: usize)
        -> Option<Vec<usize>>;

    pub fn forney(gf: &Galois16, syndromes: &[u16], sigma: &[u16],
                  positions: &[usize], npar: usize, stridecount: usize)
        -> Option<Vec<u16>>;

    pub struct LocatedError { pub row: usize, pub magnitude: u16 }
}

pub mod codec {
    pub struct CtdbCodec { /* … */ }
    impl CtdbCodec {
        pub fn new() -> Self;
        pub fn galois(&self) -> &Galois16;
        pub fn try_compute_parity(&self, audio: &[i16], npar: usize) -> Result<Vec<u8>, CtdbRsError>;

        /// Exact match only — returns matches=true iff every column's syndrome XOR
        /// is zero. NOT what we want for syndrome fast-path; this is the slow
        /// full-blob path that recomputes audio parity per call.
        pub fn try_verify_with_word_offset(
            &self, audio: &[i16], parity_bytes: &[u8],
            npar: usize, word_offset: i64,
        ) -> Result<VerifyResult, RepairError>;

        pub fn repair_with_word_offset(/* … */) -> Result<RepairResult, RepairError>;
    }

    pub struct VerifyResult {
        pub matches: bool,
        pub error_columns: usize,
        pub nonzero_syndromes: usize,
        pub column_magnitudes: Vec<u16>,
    }
}

pub use codec::{CtdbCodec, RepairError, RepairResult, VerifyResult};
pub use decoder::{berlekamp_massey, chien_search, forney, LocatedError};
pub use galois::Galois16;
pub use syndrome::{bytes_to_parity, parity_to_bytes, DEFAULT_NPAR, STRIDE};
```

## What we tried that didn't work

**Round 1 (v2):** Treated the `syndrome` attribute as an already-computed RS syndrome and applied a syndrome-conversion to OUR audio's parity row before XOR. Found zero matches at any offset because the entry's bytes are not a syndrome — they're parity[0]. We discovered this empirically.

**Round 2 (v3):** Treated the entry's `syndrome` as parity[0] (correct) and converted OUR row through a `parity_row_to_syndrome_row` helper before XOR (per CUETools' `GetSyndrome(npar, 1, -offset)` description). At runtime, BM never produces a Chien-validated match at any offset in `±5879` for any of the response's ~36 entries. Test hangs in the slow full-parity fallback.

The v3 syndrome-conversion code is at the bottom of this brief for context. Ignore it if it'll bias you — it might be wrong in a sign or transform direction we can't pinpoint.

## What we want as output

Generate Rust replacement code for `verify_disc_via_rs` and its helpers in `src/tui/ctdb.rs` that **literally translates** CUETools.NET's `FindOffset` + `GetSyndrome` + the parity-vs-syndrome conversions, with each block annotated to its C# source location. Use our codec's primitives. Don't reinvent the GF math.

Include:

1. The C# source you read, quoted in the response (with file paths + line numbers from GitHub).
2. The line-by-line Rust translation, with `// matches CUETools.Codecs/AccurateRipVerify.cs:NNN-MMM` style markers.
3. A clear answer to: what does `offset` mean in `GetSyndrome(npar, columns, offset)`? Stereo sample pairs? Words? What's its sign?
4. A clear answer to: when CUETools' `DBEntry` reads `Bytes2Syndrome` vs `Parity2Syndrome`, what's the resulting `ushort[,]` layout? `[1, npar]`? `[npar, 1]`? What's the indexing convention vs our `Vec<Vec<u16>>`?
5. If there's a numeric fixture you can compute from C# code values (e.g. for a synthetic small disc), include it so we can verify our translation.
6. Failure modes / edge cases your translation handles vs ours doesn't.

Output Rust code in fenced blocks. We'll drop it into `src/tui/ctdb.rs` directly.

## Existing v3 code (for context, may be wrong)

Don't trust it. Use only as a hint about the surface our consumers expect (`verify_disc_via_rs(audio, entries, offset_window) -> Option<RsVerifiedMatch>`, `RsVerifiedMatch { entry, offset, confidence, npar, source, column0_errors }`, `RsVerifySource::{Syndrome, InlineParity, FullParity}`).

```rust
fn parity_row_to_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    parity_row: &[u16],
    npar: usize,
) -> Option<Vec<u16>> {
    if parity_row.len() < npar {
        return None;
    }
    let mut syndrome = vec![0u16; npar];
    let gf_max = crate::ctdb_rs::galois::MAX;
    for x1 in 0..npar {
        let lo = parity_row[x1];
        if lo == 0 { continue; }
        let log_lo = gf.log(lo)?;
        for x in 0..npar {
            let decrement = ((1 + x1) * x) % gf_max;
            let exp = (log_lo + gf_max - decrement) % gf_max;
            syndrome[x] ^= gf.alpha_pow(exp);
        }
    }
    Some(syndrome)
}

fn row_for_sample_offset(offset: i32) -> usize {
    let stride = crate::ctdb_rs::STRIDE as i64;
    ((-2_i64 * offset as i64).rem_euclid(stride)) as usize
}

fn verify_entry_via_syndrome_fast_path(
    gf: &crate::ctdb_rs::Galois16,
    parity_matrix: &[Vec<u16>],   // result of compute_parity_matrix_from_audio
    entry: &CtdbEntry,
    offset_window: i32,            // ±5879 stereo sample pairs
    stridecount: usize,            // = audio.len()/STRIDE - 2 = 30986 for our disc
) -> Option<RsVerifiedMatch> {
    let npar = (entry.npar as usize).min(16);
    let entry_row = decode_entry_row(gf, entry, npar)?;
    for offset in (-offset_window)..=offset_window {
        let row = row_for_sample_offset(offset);
        let computed_row = parity_matrix_row_to_syndrome_row(gf, parity_matrix, row, npar)?;
        let delta: Vec<u16> = computed_row.iter().zip(&entry_row).map(|(a,b)| a^b).collect();
        if delta.iter().all(|&w| w == 0) { return Some(/* hit, errors=0 */); }
        let (sigma, errors_found) = berlekamp_massey(gf, &delta, npar)?;
        if errors_found == 0 || errors_found > npar/2 { continue; }
        let positions = chien_search(gf, &sigma, errors_found, stridecount)?;
        if positions.len() == errors_found { return Some(/* hit, errors */); }
    }
    None
}
```

Empirically, for the test disc, this hits zero matches at any offset in `[-5879, +5879]` against any of the response's 36 entries — including the canonical 896 entry whose `syndrome` we've confirmed equals `parity[0]` of the downloadable blob. Something in the algorithm is off; please tell us what, by reading the C#.

## Don't get distracted

We don't need theory about Galois fields, Reed-Solomon, or RS-decoding background. We need source-grounded Rust translation. Cite the line numbers.

# CTDB syndrome verification — second-pass brief

## Context for the model

Tonepoet, a Rust CD-rip toolkit, has a CRC32-based CTDB verifier that works for ~95% of discs (rips that byte-match a CTDB submitter exactly). It fails on niche pressings (Japanese SHM-CD, MFSL Gold, mastering variants) where CUETools-on-Windows reports `verified OK confidence N` via Reed-Solomon, but our impl says `Mismatch` because no single CTDB entry's `trackcrcs` matches our 4-tuple.

A previous reasoning model produced a patch implementing RS verification (`docs/ctdb-syndrome-verification-bundle.md` is the brief that drove that work). Empirical testing of that patch on a real disc (Allman Brothers *At Fillmore East* Japan SHM Disc 2, CUETools verified at 896 confidence) revealed its syndrome interpretation was wrong: zero matches in any byte order or any small offset.

We then **discovered the actual format** (see "Empirical findings" below). We now need a reasoning model to (1) confirm the discovery against CUETools.NET source, (2) give us the precise verification algorithm, (3) generate Rust code to replace the broken syndrome path in the existing patch.

## Empirical findings (verified)

The CTDB API returns entries like:

```xml
<entry confidence="896" crc32="13f525de" hasparity="http://p.cuetools.net/113068" id="113068"
       npar="16" stride="5880" syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="
       toc="0:24159:127166:278762:309864" trackcrcs="40c5dc10 65dfcc8a 1ef7b539 d21a8789" />
```

We downloaded the parity blob from `hasparity` (376_320 bytes = `STRIDE × NPAR × 2` for STRIDE=11760, NPAR=16) and decoded the `syndrome` attribute (32 bytes = NPAR×2). They are byte-for-byte identical:

```
Parity blob row 0 (parity[0][0..16]):
  8d1c f108 4bcc 9ecf d0a2 8497 77c9 002b d5a5 f2b3 5745 e009 469e f199 e3ad 9f3b

896 entry `syndrome` attribute decoded as little-endian u16:
  8d1c f108 4bcc 9ecf d0a2 8497 77c9 002b d5a5 f2b3 5745 e009 469e f199 e3ad 9f3b
```

**Conclusion: the `syndrome` XML attribute is a verbatim copy of `parity[0][0..NPAR]` from the full parity blob.** It's an inlined fast-path so a verifier doesn't need to download the 376KB blob just to do a quick check on column 0 of the disc.

Our parity blob layout (from `src/ctdb_rs/mod.rs::syndrome::try_bytes_to_parity`):
```rust
// parity[j][i] = u16::from_le_bytes(data[(j + i*stride)*2 .. (j + i*stride)*2 + 2])
// for j in 0..stride (= 11_760), i in 0..npar (= 16).
```

In Rust, `parity[j][i]` is "the i-th RS parity symbol for disc-image column j". The first inner `Vec` `parity[0]` contains the NPAR parity symbols for disc-image column 0; that's the row that equals the inlined `syndrome` attribute.

## Our codec's API surface (relevant parts only)

```rust
// src/ctdb_rs/mod.rs

pub mod syndrome {
    pub const STRIDE: usize = 11_760;          // i16 count = 5_880 stereo pairs

    /// Compute full parity matrix from full disc image audio.
    /// `audio` length must be `STRIDE leadin + tracks + STRIDE leadout` (i16).
    /// Returns `[STRIDE][NPAR]` matrix.
    pub fn compute_parity_matrix_from_audio(
        gf: &Galois16, audio: &[i16], npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    /// Single-column parity. `column_data` is the column's u16 stream
    /// (one u16 per disc-image row). Output: `npar` u16 LFSR state.
    pub fn compute_column_parity(
        gf: &Galois16, gx: &[u16], column_data: &[u16], npar: usize,
    ) -> Vec<u16>;

    pub fn make_generator_poly(gf: &Galois16, npar: usize) -> Result<Vec<u16>, CtdbRsError>;
}

pub mod decoder {
    /// Standard BM. Returns `Some((sigma, errors_found))` or `None` if
    /// no consistent locator polynomial exists.
    pub fn berlekamp_massey(
        gf: &Galois16, syndromes: &[u16], npar: usize,
    ) -> Option<(Vec<u16>, usize)>;

    /// Find roots of sigma in [0..stridecount). Returns row indices of
    /// errors. `Some(positions)` with len == `error_count` only if all
    /// roots lie inside the data range. Returns `None` when fewer than
    /// `error_count` valid roots are found.
    pub fn chien_search(
        gf: &Galois16, sigma: &[u16], error_count: usize, stridecount: usize,
    ) -> Option<Vec<usize>>;

    /// Magnitudes for the located errors.
    pub fn forney(
        gf: &Galois16, syndromes: &[u16], sigma: &[u16],
        positions: &[usize], npar: usize, stridecount: usize,
    ) -> Option<Vec<u16>>;
}

pub use codec::CtdbCodec;
pub use decoder::{berlekamp_massey, chien_search, forney};
pub use galois::Galois16;
pub use syndrome::STRIDE;

impl CtdbCodec {
    /// Full RS verify, requires full parity blob. Returns matches=true if
    /// every column's syndrome XOR-difference is zero.
    pub fn try_verify_with_word_offset(
        &self, audio: &[i16], parity_bytes: &[u8],
        npar: usize, word_offset: i64,
    ) -> Result<VerifyResult, RepairError>;

    pub fn repair_with_word_offset(...) -> Result<RepairResult, RepairError>;
    // (full RS repair — already used by :ctdb-repair)
}
```

The codec's high-level `try_verify` does full-matrix verification: it computes the audio's full parity, syndrome-converts both audio and provided parity by word offset, XORs, returns `matches` if all column magnitudes are zero. It does **not** do RS-correctable verification (only exact byte-equality). Repair adds the BM/Chien/Forney machinery to fix correctable columns.

We have GF(2^16) primitive poly `0x1100B`, generator roots `α^0..α^(npar-1)`. The codec's docstring says "intentionally follows the CUETools/CTDB orientation".

## Concrete data from the test disc

Allman Brothers At Fillmore East Deluxe Disc 2 (Japan SHM), 4 per-track FLACs, EAC log says `Read offset correction: 6, Fill up missing offset samples with silence: Yes`. CUETools verifies at confidence 896.

- TOC sent: `0:24159:127166:278762:309864`
- Disc image (after assembling `[STRIDE leadin] + 4 tracks + [STRIDE leadout]`): 364_423_584 i16 = 182_211_792 stereo pairs. Each track length matches EAC log's TOC sectors × 588 exactly.
- Our audio's `parity[0][0..16]` (computed via `compute_parity_matrix_from_audio` with NPAR=16):
  ```
  4d6a c44e 14c2 d096 3b5e b2d9 94d2 e227 0bb7 5919 8a51 fc5a e873 cc6f 1dba 9a83
  ```
- 896 entry's parity[0][0..16] (= its `syndrome` attribute, verified above):
  ```
  8d1c f108 4bcc 9ecf d0a2 8497 77c9 002b d5a5 f2b3 5745 e009 469e f199 e3ad 9f3b
  ```
- XOR delta:
  ```
  c076 3546 5f0e 4e59 ebfc 364e e31b e20c de12 abaa dd14 1c53 aeed 3df6 fe17 05b8
  ```

Our per-track CRC32s (`c17a4a77 fabb2be6 52898fe0 96af30c8`) are different from 896's `trackcrcs` (`40c5dc10 65dfcc8a 1ef7b539 d21a8789`); our 4-tuple is **not present** as a single entry anywhere in the response (~36 entries total). Tracks 1-3 of our rip match patterns in many conf=1 entries but track 4 doesn't pair with track 3 in any submitter's set. CUETools nevertheless reports verified at 896 — i.e., the 896 audio is RS-equivalent to ours within `npar/2 = 8` errors per column.

## What we need from the model

We've started a Rust experiment: at each candidate word offset O, take row `(-O mod STRIDE)` of our parity matrix, XOR with the entry's syndrome bytes, and run `berlekamp_massey` on the delta. Across the 101 even word offsets in `[-100, +100]`, BM returned `Some((sigma, errors_found))` with `errors_found ≤ npar/2 = 8` at 21 offsets (word offsets +60 through +100, all consecutive, all reporting exactly `errors_found = 8` — BM's degree ceiling). That looks more like BM accepting random-looking syndromes at a degenerate boundary than a real RS-correctable column. We did **not** validate any of these with `chien_search` (which requires the located error positions to fall inside `[0..stridecount)` of the disc data; for our disc `stridecount = (audio_image_len_i16 / STRIDE) - 2 = 30986`). Without Chien validation, BM's "success" is likely spurious.

Please answer:

1. **Algorithm confirmation.** Is the correct CTDB syndrome-fast-path verification algorithm: "BM-decode the XOR delta of `audio.parity[(-O) mod STRIDE]` and `entry.syndrome`, then validate with Chien search against `stridecount = audio_rows`, considered verified iff `errors_found ≤ npar/2` AND all Chien roots lie in `[0..stridecount)`"? Or does CUETools.NET use a different test (e.g., trial-and-correct via Forney, then re-verify column with corrected values)? Cite the call chain in CUETools.NET — files `CUETools.CTDB/CDRepairEncode.cs`, `CDRepairData.cs`, `CTDBResponseEntry.cs` are the right places to look (`https://github.com/gchudov/cuetools.net`).

2. **Offset semantics.** When CUETools `FindOffset` decides "RS-verified at offset O", what does O mean physically? Stereo sample-pair shift between our rip's column-0 stream and theirs? And what's the relationship between an "AccurateRip drive offset" (we observed `+6` in the EAC log for the test disc) and this RS column-0 offset O? If they're the same units, what window does CUETools search?

3. **Fast-path scope.** Does CUETools' fast-path verify only column 0 (using just the inline `syndrome` attribute), or does it verify all STRIDE columns when full parity is available? If it only uses column 0 from `syndrome`, what's the false-negative rate (rip is RS-equivalent in column 0 but not in some other column)? Implications: should our code report `VerifiedRs` from a column-0-only check, or only after pulling the full parity blob and verifying every column?

4. **`npar=8` entries with inline `parity="<base64>"` (no `syndrome` attr).** The bytes are also NPAR×2 = 16 bytes. Are they semantically the same thing — `parity[0][0..NPAR]` of the full matrix that the entry doesn't expose via `hasparity` — or something different? CUETools must have code that reads both attributes; what's the equivalence?

5. **Implementation.** Generate Rust code to replace the broken `verify_disc_via_syndromes_blocking` and `parity_matrix_row_to_syndrome_row` from the v2 patch (in `src/tui/ctdb.rs`, applied to the codebase, see "Existing patch" below). Constraints:
   - Use the codec's existing `compute_parity_matrix_from_audio`, `berlekamp_massey`, `chien_search` (all `pub`, signatures above). Don't reimplement GF math.
   - Cache the audio's parity matrix per NPAR (one full computation per unique NPAR across entries — npar∈{8,16}, so at most two computations).
   - Accept entries where `syndrome` or inline `parity` is present; for offset trial use `±(STRIDE/2 - 1)` stereo pairs window (CUETools-equivalent).
   - Fall back to full-parity-blob verification (existing `try_verify_with_word_offset`) only when a `hasparity` URL is present **and** the column-0 fast path was inconclusive.
   - Sort entries by descending confidence before iterating; first verified hit wins.

6. **Failure-mode flagging.** What edge cases should we test for? E.g.: rip with read-offset that's exactly at the Chien-search boundary, rip that is RS-correctable in column 0 but not other columns, multi-disc albums where disc N entries leak into disc M's verification.

## Existing patch (to be amended, not replaced)

`docs/ctdb-syndrome-verification-bundle.md` (the prior brief) contains the v2 patch context. The relevant signatures the new code must keep working:

- `pub struct CtdbEntry { id, crc32, confidence, npar, stride, has_parity, parity, syndrome, track_crcs }` — fields already added by the v2 patch.
- `pub enum CtdbTrackStatus { Verified, VerifiedRs, Mismatch, NoDiscInDatabase, Error(String) }` — `VerifiedRs` already wired through draw_overlays.rs.
- `pub async fn verify_disc_via_rs(audio_image: &[i16], entries: &[CtdbEntry], offset_window: i32) -> Option<RsVerifiedMatch>` — public API (async because the parity-blob fallback path downloads); only its internals need replacing.

The flow in `verify_ctdb` and `verify_ctdb_single_image` already (a) decodes audio, (b) assembles `[STRIDE leadin] + audio + [STRIDE leadout]`, (c) calls `verify_disc_via_rs`, (d) uses the matched entry's `track_crcs` for per-track diagnostic display while letting the RS match drive the album-level status. Don't change that flow — just replace the `verify_disc_via_syndromes_blocking` body and any helpers it calls (`syndrome_row_from_entry`, `parity_matrix_row_to_syndrome_row`).

Output format: Rust source in fenced blocks, each headed with `// in src/tui/ctdb.rs (replace fn X)` so we can drop them in. If you also need to change anything in `src/ctdb_rs/mod.rs`, say so explicitly — we'd prefer not to, since the codec is unit-tested.

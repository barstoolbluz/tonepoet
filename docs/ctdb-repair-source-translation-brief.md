# CTDB repair — source-translation request (round 2)

## Context

You produced the CTDB **verify** path translation in `ctdb_cuetools_source_translation_patch.zip` (now landed as commit `d5f2e52` in `tonepoet`). It works: the Allman Brothers Japan SHM Disc 2 test rip verifies at confidence 896 against entry id 113068 at sample offset +5408 with `column0_errors=0`. Two CUETools-derived numeric fixtures (`parity2syndrome_single_row_fixture` and `bytes2syndrome_layout_fixture`) pass, so our codec's GF(2¹⁶) arithmetic and serialization layout are byte-compatible with CUETools.NET.

Your README flagged a remaining issue: **the `hasparity` blob is a syndrome matrix, not raw parity** (CUETools serializes it via `Syndrome2Bytes(GetSyndrome(npar, -1, offset))`). Our existing `CtdbCodec::repair_with_word_offset` and `try_verify_with_word_offset` treat the blob as a raw parity matrix and apply `Parity2Syndrome` to it, producing `Parity2Syndrome(syndrome)` — a meaningless transform. Self-loop unit tests pass because the codec writes blobs in the same wrong format it reads, so cancellation works internally — but against real CTDB blobs from the wire, repair would produce wrong corrections. The verify path doesn't use the blob at all (only the inline `syndrome=` attribute), so verify is unaffected.

Please translate the **repair** path, mirroring the source-faithful approach you used for verify.

## What's already in place (don't reinvent)

Inside `src/tui/ctdb.rs` (verify-side, your prior translation):

```rust
const CUETOOLS_MAX_NPAR: usize = 16;

struct CuetoolsSyndromeContext { stride, stridecount, laststride, leadin, leadout }

fn cuetools_build_syndrome_context(audio: &[i16]) -> Option<CuetoolsSyndromeContext>;
fn cuetools_bytes_to_syndrome_matrix(bytes: &[u8], stride: usize, npar: usize) -> Option<Vec<Vec<u16>>>;
fn cuetools_parity_row_to_syndrome_row(gf, parity_row, out_npar, source_npar) -> Option<Vec<u16>>;

/// columns=1 case of CUETools.AccurateRip/AccurateRip.cs::GetSyndrome
fn cuetools_get_syndrome_row(
    gf: &Galois16,
    parity16: &[Vec<u16>],
    ctx: &CuetoolsSyndromeContext,
    out_npar: usize,
    sample_offset: i32,
) -> Option<Vec<u16>>;
```

`cuetools_get_syndrome_row` handles the column-0 case with first/last-boundary corrections. Repair needs the `columns=-1` case (full STRIDE rows) which is the same logic generalized.

The codec primitives (untouched, validated):

```rust
crate::ctdb_rs::syndrome::compute_parity_matrix_from_audio(gf, audio, npar)
    -> Result<Vec<Vec<u16>>, _>;             // STRIDE × NPAR parity matrix from audio[i16]

crate::ctdb_rs::berlekamp_massey(gf, syndromes, npar) -> Option<(Vec<u16>, usize)>;
crate::ctdb_rs::chien_search(gf, sigma, error_count, stridecount) -> Option<Vec<usize>>;
crate::ctdb_rs::forney(gf, syndromes, sigma, positions, npar, stridecount) -> Option<Vec<u16>>;

crate::ctdb_rs::CtdbCodec::repair_with_word_offset(
    audio: &mut [i16], parity_bytes: &[u8], npar: usize, word_offset: i64,
) -> Result<RepairResult, RepairError>;        // existing — treats blob as parity (wrong for CTDB blobs)
```

## What we need translated

In CUETools.NET, after `CDRepair.FindOffset` returns an `actualOffset`, the repair flow:

1. Fetches the full `hasparity` blob (already done in `tonepoet`).
2. Decodes the blob as a syndrome matrix via `Bytes2Syndrome(stride_words, npar, blob)` → C# `ushort[stride, npar]`. (Our `cuetools_bytes_to_syndrome_matrix` already does this; we just need to call it.)
3. Computes the audio's full syndrome matrix at `-actualOffset` via `GetSyndrome(npar, -1, -actualOffset)` (the all-columns case).
4. XORs the two syndrome matrices column-by-column to produce a delta matrix.
5. For each column with non-zero delta: BM → if `errors > 0 && errors ≤ npar/2`, Chien-validate against `stridecount`, then Forney to compute error magnitudes at located positions.
6. Apply each `(column, row, magnitude)` triple back to the audio at sample index `STRIDE + row * STRIDE + column` (the same indexing `compute_parity_matrix_from_audio` uses).
7. Optionally re-run verify on the corrected audio to confirm.

Source files (same repo as before): `https://github.com/gchudov/cuetools.net`. Likely paths:
- `CUETools.AccurateRip/CDRepair.cs` — `FindOffset`'s caller, plus repair logic. README v4 mentioned `1252-1349` for FindOffset; the repair function is nearby.
- `CUETools.AccurateRip/AccurateRip.cs` — `GetSyndrome` lines 2782-2848 are the `columns=1` translation you produced; the `columns=-1` branch is in the same function or adjacent.
- `CUETools.CTDB/CUEToolsDB.cs` — `DoVerify` was at 2594-2674 in your v4 README; the repair caller is nearby (`DoVerifyAndRepair` or similar).

## Specific questions

1. **Full-matrix `GetSyndrome(npar, -1, offset)`.** Does CUETools loop the column-0 logic over all stride columns (varying `part2 = 0..stride-1`), with each column's first/last-boundary corrections applied based on its own `part` value? Or is there a different code path for `columns=-1` that we should mirror? Quote the relevant C# block.

2. **Blob row alignment vs offset.** When repairing at `actualOffset = +5408`, what's the exact alignment between `our_syndrome_matrix` (computed from audio at some offset) and `blob_syndrome_matrix` (decoded from the bytes via `Bytes2Syndrome(stride, npar, blob)`)? Do they XOR row-for-row, or is one side rotated by some function of `actualOffset`? Quote the C# block that produces the delta.

3. **Per-column repair loop.** Quote the C# that takes the delta syndrome matrix, runs BM/Chien/Forney per column, and produces `(row, magnitude)` corrections. What's the exact threshold for "uncorrectable"? `errors > npar/2` per column, OR a global bound across columns?

4. **Audio sample-index mapping.** For a Forney correction at `(column, row)` with magnitude `m`: confirm the audio index is `STRIDE + row * STRIDE + column` (this matches `compute_parity_matrix_from_audio`'s loop), and that the correction is `audio[idx] ^= u16_to_i16_bits(m)`.

5. **Repair invariants.** Does CUETools re-verify the audio after applying corrections (confirming the syndrome matrix is now zero), or trust BM/Chien/Forney as definitive? What's the failure semantic if some columns are correctable but others aren't?

6. **Numeric fixture for repair.** A small synthetic disc (a few stride-rows of audio + fabricated parity blob with a known error pattern) where the expected repaired audio is computable in C#. We can use it as a regression fixture analogous to the verify-side `parity2syndrome_single_row_fixture`.

## What we want as output

1. A new `cuetools_get_syndrome_matrix(gf, parity16, ctx, out_npar, sample_offset) -> Option<Vec<Vec<u16>>>` (the `columns=-1` generalization of `cuetools_get_syndrome_row`).
2. A new `pub async fn repair_disc_via_rs(audio: &mut [i16], blob_bytes: &[u8], entry: &CtdbEntry, sample_offset: i32) -> Result<RepairOutcome, RepairError>` (or similar — feel free to shape the API as makes sense).
3. Tonepoet's `repair_album` and `repair_single_image` (in `src/tui/ctdb.rs`) currently call `CtdbCodec::repair_with_word_offset(audio, parity_bytes, npar, offset)`. Indicate in the patch how the call site should change to use the new function — we expect to replace that call entirely, since the existing function's semantics are wrong for real CTDB blobs.
4. A numeric fixture covering the repair path that can run as a `#[test]` alongside the verify-side fixtures.
5. Each non-trivial helper annotated with `// matches CUETools.AccurateRip/...:lines` markers so the audit trail continues from the verify work.

Don't modify `src/ctdb_rs/mod.rs` if avoidable — the codec primitives are validated by the v4 fixtures and we'd prefer to leave them alone. If you do need to add a new public function to the codec (e.g., a syndrome-matrix-based verify), say so explicitly.

## Empirical anchor

Test disc: same Allman Brothers Japan SHM Disc 2.

- Audio: 4 FLACs, total 182_200_032 stereo pairs. Disc image (with STRIDE leadin/leadout) = 364_423_584 i16 = 30_988 stride rows, `stridecount = 30_986`.
- TOC: `0:24159:127166:278762:309864`.
- Verify result: entry 113068 confidence 896, `actualOffset = +5408`, column-0 syndrome delta is exactly zero at that offset.
- Blob URL: `http://p.cuetools.net/113068`. 376_320 bytes (= STRIDE × NPAR × 2 with NPAR=16).

If your repair translation is correct, applying `repair_disc_via_rs(audio, blob, entry, +5408)` should leave the audio unchanged (since the column-0 syndrome already matches; if the canonical 896 audio also matches at all other columns, total `corrected_samples` should be zero or very small).

If repair finds non-zero columns and applies corrections, the resulting audio's per-track CRC32s should match the canonical entry's `trackcrcs="40c5dc10 65dfcc8a 1ef7b539 d21a8789"` exactly. (Currently they're `c17a4a77 fabb2be6 52898fe0 96af30c8` — different bytes, RS-equivalent.)

This gives us a clean ground-truth check: post-repair CRCs == canonical entry's `trackcrcs`.

## Don't get distracted

We don't need RS theory or CTDB protocol context. We have working verify and a passing fixture battery. Source-grounded Rust translation only, line-numbered against CUETools.NET.

# CTDB Verification: CRC32-Only vs Reed-Solomon Syndrome

## TL;DR

Our `:ctdb` (CUETools Database) verification reports per-track CRC32 mismatches against the highest-confidence database entry, while CUETools on Windows reports the same rip as `verified OK, confidence 896`. We have empirical evidence that our CRC32 logic itself is correct; the gap is that CTDB's strongest verification signal is a Reed-Solomon **syndrome** check (or full RS verification using downloadable parity bytes), not the per-track CRC32 lookup we're doing. **We need to add RS-syndrome-based verification.** Our codebase already contains a complete Galois-16 RS codec with `try_verify_with_word_offset(audio, parity_bytes, npar, word_offset) -> VerifyResult`. We need a reasoning model to (a) confirm the diagnosis, (b) design the verification flow, (c) generate the wiring code.

---

## What CTDB is

CUETools Database (CTDB), `https://db.cue.tools/lookup2.php`, is a community CD verification database. A submission consists of a TOC (track sector offsets), per-track CRC32 values, a Reed-Solomon parity matrix over the disc's audio, and a compact "syndrome" derived from that parity. The disc image is conceptually the concatenation `[STRIDE leadin zeros] + [track1 audio] + [track2 audio] + … + [STRIDE leadout zeros]` where `STRIDE = 11_760` i16 values (= 5_880 stereo sample pairs = 10 CD sectors).

A CTDB lookup by TOC returns one or more `<entry>` elements; multiple entries can exist for the same TOC because pressings differ subtly (sometimes only in trailing samples or pre-emphasis flag). Each entry carries:

- `confidence="N"` — count of identical submissions.
- `trackcrcs="hex hex hex …"` — per-track CRC32 over each track's audio with a CTDB-specific prefix-skip on the first track and a TOC-derived suffix-skip on the last track.
- `syndrome="<base64>"` — a compact RS syndrome that encodes the canonical audio fingerprint with strong error-correction capability.
- `parity="<base64>"` (small NPAR) **or** `hasparity="<URL>"` (URL pointing to full parity bytes for larger NPAR).
- `npar="8"` or `npar="16"` — number of parity symbols per RS column (max correctable errors = `npar/2`).
- `stride="5880"` — CTDB stride in stereo pairs (matches our `STRIDE / 2`).

CUETools' "verified OK" means RS verification against the entry's syndrome/parity passes with **zero** RS errors at the rip's drive-read offset (CUETools also tries a small offset window). Per-track CRC32 mismatch is non-fatal — RS verification is the authoritative signal.

## What our implementation currently does (incorrect)

`src/tui/ctdb.rs::verify_ctdb` builds a TOC, queries CTDB, picks the entry with the highest confidence (`max_by_key(|e| e.confidence)`), then for each track:

```rust
let computed = compute_track_crc32(&decoded, is_first, is_last, suffix_skip);
let db_crc = entry.track_crcs.get(i).copied();
let status = match db_crc {
    Some(expected) if expected == computed => CtdbTrackStatus::Verified,
    Some(_) => CtdbTrackStatus::Mismatch,
    None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
};
```

`compute_track_crc32` applies:
- `PREFIX_SKIP_I16 = STRIDE_WORDS = 11_760` (i16 count) at the start of the first track.
- `compute_suffix_skip(total_disc_samples) = STRIDE_WORDS + (total_disc_words % STRIDE_WORDS)` (i16 count) at the end of the last track.
- No skip on middle tracks.
- CRC32 (IEEE 802.3, via `crc32fast::hash`) over the i16 buffer reinterpreted as little-endian u8 bytes.

This logic is correct (validated below), but it only matches when the rip exactly reproduces the byte stream of whichever CTDB entry happens to have the highest confidence. For widely-pressed albums whose canonical 896-confidence entry came from a master tape that differs in trailing samples or pre-emphasis from typical commercial pressings, our rip ends up with CRCs that match a constellation of low-confidence entries (one per submitter pattern) rather than the high-confidence canonical, and we declare "Mismatch" — even though the rip is RS-verifiable against the canonical.

## Empirical evidence

Test disc: *The Allman Brothers Band – At Fillmore East Deluxe Edition (Disc 2) (Japan SHM)*. Four per-track FLACs, ripped by EAC with `Read offset correction: 6` and `Fill up missing offset samples with silence: Yes`. The EAC log's TOC matches the FLAC sample counts exactly, and the FLAC sample counts equal `disc sectors × 588`.

CUETools (Windows) on these exact files: `CTDB: verified OK, confidence 896, or differs in 125 samples, confidence 3, or differs in 44 samples, confidence 2`.

Our `:ctdb` against the same files: all 4 tracks "Mismatch".

A standalone Rust diagnostic decoded each FLAC via `ffmpeg -f s16le -ar 44100 -ac 2`, applied our exact `compute_track_crc32` logic, and compared against:

- the EAC log's per-track CRC32 (no skip — over the full WAV);
- the canonical (confidence 896) CTDB entry's `trackcrcs`.

| | EAC log CRC (no skip) | Our CTDB compute (with skip) | 896 entry `trackcrcs` |
|--|--|--|--|
| Track 1 | `00666F5F` | `c17a4a77` | `40c5dc10` |
| Track 2 | `FABB2BE6` | `fabb2be6` | `65dfcc8a` |
| Track 3 | `52898FE0` | `52898fe0` | `1ef7b539` |
| Track 4 | `D8316AE9` | `96af30c8` | `d21a8789` |

Tracks 2 and 3 (no skip in CTDB) — our CRC matches EAC's whole-track CRC32 exactly. Track 1 and Track 4's CRCs differ from EAC's because of CTDB's prefix/suffix skips, as designed. **Our CRC computation is correct.**

The 896 entry's `trackcrcs` simply describes a *different* byte sequence from this rip. Our rip's individual track CRCs do appear in CTDB — `c17a4a77` is the track-1 CRC of ~15 distinct confidence-1 entries, `fabb2be6` is track-2 of ~14 entries — but our specific 4-tuple `(c17a4a77, fabb2be6, 52898fe0, 96af30c8)` is not a single entry in the database. Our rip's audio is not byte-identical to any submitter's, but is RS-equivalent to the canonical (confidence 896) submission.

## The CTDB response (representative subset)

Sent: `GET https://db.cue.tools/lookup2.php?version=3&ctdb=1&toc=0:24159:127166:278762:309864`

```xml
<ctdb xmlns="http://db.cuetools.net/ns/mmd-1.0#" xmlns:ext="http://db.cuetools.net/ns/ext-1.0#">
 <entry confidence="896" crc32="13f525de" hasparity="http://p.cuetools.net/113068" id="113068"
        npar="16" stride="5880" syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="
        toc="0:24159:127166:278762:309864" trackcrcs="40c5dc10 65dfcc8a 1ef7b539 d21a8789" />
 <entry confidence="3" crc32="5701aff0" hasparity="http://p.cuetools.net/8888389" id="8888389"
        npar="8" stride="5880" syndrome="t8QZhJ3lAyZZZkSjASM7xA=="
        toc="0:24159:127166:278762:309864" trackcrcs="ca4cb88d 3fa34037 3df63007 8f4bfab1" />
 <entry confidence="2" crc32="30ff8c29" hasparity="http://p.cuetools.net/10527693" id="10527693"
        npar="8" stride="5880" syndrome="fXJG3GrPHlTfpvHQDeUi/g=="
        toc="0:24159:127166:278762:309864" trackcrcs="4b4fce69 4dd48851 affbbca3 310638ad" />
 <entry confidence="1" crc32="81ae880d" id="1867848" npar="8" parity="zqlFbO8Nfi1SnryIl2O2rw=="
        stride="5880" toc="0:24159:127166:278762:309864" trackcrcs="bb450280 c458b0b1 68919243 7569f562" />
 <entry confidence="1" crc32="27397da5" id="2374177" npar="8" parity="+ykHWZ3Z27G3FJm6gR4+aQ=="
        stride="5880" toc="0:24159:127166:278762:309864" trackcrcs="c17a4a77 fabb2be6 694a392b 2848faf3" />
 <!-- … 30+ more confidence=1 entries with `syndrome=…` (no full parity inline);
      see https://db.cue.tools/lookup2.php?version=3&ctdb=1&toc=0:24159:127166:278762:309864 -->
 <metadata />
</ctdb>
```

Notes:
- Some entries have `parity="<base64>"` directly (small NPAR=8). Most have `hasparity="<URL>"` — a downloadable blob containing the full parity matrix bytes (`stride * npar * 2` bytes laid out as expected by our codec's `bytes_to_parity`).
- All entries have a `syndrome="<base64>"` field. The syndrome is **not** the same as parity; it's a smaller compact value. Empirically: confidence=896 entry's syndrome decodes to ~32 bytes; confidence=8 entry's syndrome decodes to ~16 bytes. The exact algorithm CUETools uses to derive the syndrome from parity is what we need to replicate (or bypass by downloading parity).

## Available primitives in our RS codec

`src/ctdb_rs/mod.rs` (Galois-field-16 over polynomial `0x1100B`) exposes:

```rust
pub const STRIDE: usize = 11_760;             // i16 count (= 5_880 stereo pairs)
pub const DEFAULT_NPAR: usize = 8;

pub struct CtdbCodec { /* … */ }

impl CtdbCodec {
    pub fn new() -> Self;
    pub fn galois(&self) -> &Galois16;

    pub fn try_compute_parity(&self, audio: &[i16], npar: usize)
        -> Result<Vec<u8>, CtdbRsError>;

    /// `audio` length should equal a full disc image: STRIDE leadin + tracks + STRIDE leadout.
    /// `parity_bytes` is the entry's parity matrix as raw bytes (npar columns × stride rows × 2).
    /// `offset` is a CD stereo sample-pair offset (positive = drive read late).
    /// Returns VerifyResult { matches: bool, error_columns, nonzero_syndromes, column_magnitudes }.
    pub fn try_verify(&self, audio: &[i16], parity_bytes: &[u8], npar: usize, offset: i32)
        -> Result<VerifyResult, RepairError>;

    pub fn try_verify_with_sample_offset(/* same as try_verify */) -> /* … */;
    pub fn try_verify_with_word_offset(&self, audio: &[i16], parity_bytes: &[u8],
                                       npar: usize, word_offset: i64)
        -> Result<VerifyResult, RepairError>;

    pub fn repair(&self, audio: &mut [i16], parity_bytes: &[u8], npar: usize, offset: i32)
        -> Result<RepairResult, RepairError>;
}

pub fn syndrome::parity_to_syndrome(gf: &Galois16, parity: &[Vec<u16>],
                                    npar: usize, stride: usize, sample_offset: i32)
    -> Result<Vec<Vec<u16>>, CtdbRsError>;

pub fn syndrome::bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Vec<Vec<u16>>;
```

`VerifyResult.matches == true` iff every per-column syndrome is zero — i.e. the rip's audio (at the given offset) is byte-identical to the parity-source audio. `error_columns` and `nonzero_syndromes` quantify the divergence when not equal; we don't need to use them for a yes/no verify, but they could feed a "near match" UI.

## Current CTDB call site (relevant excerpts)

```rust
// src/tui/ctdb.rs

pub struct CtdbEntry {
    pub id: String,
    pub crc32: u32,
    pub confidence: u32,
    pub npar: u32,
    pub stride: usize,
    pub has_parity: Option<String>,    // the URL string from `hasparity` attr
    pub track_crcs: Vec<u32>,
    // NOTE: no `syndrome` or inline `parity` fields parsed yet.
}

pub async fn verify_ctdb(
    paths: &[PathBuf],
    sample_counts: &[u64],
    sample_rate: u32,
) -> CtdbVerifyResult {
    // 1. Build TOC, query CTDB.
    // 2. best_entry = entries.iter().max_by_key(|e| e.confidence)
    // 3. For each track: decode -> compute_track_crc32 -> compare to entry.track_crcs[i]
    //    -> CtdbTrackStatus::{Verified | Mismatch | Error}
}

async fn download_parity(url: &str) -> Result<Vec<u8>, String> {
    /* HTTP GET, return bytes */
}

const CTDB_BASE: &str = "https://db.cue.tools/lookup2.php";
```

The XML parser is line-based (one `<entry …/>` per line) using a small `extract_attr(line, name)` helper. Adding `syndrome` and inline `parity` attribute parsing is trivial.

## What we want from a reasoning model

Please produce a single response that addresses **all** of the following:

### A. Diagnosis confirmation

Given the empirical CRC table above, the CTDB response sample, and our RS-codec primitives, confirm or correct this hypothesis:

> CUETools' `verified OK, confidence 896` is the result of an RS verification against the 896 entry's parity (or syndrome), not a per-track CRC32 match. Our rip's audio is RS-equivalent to (and therefore a valid alternate-pressing of) the 896-canonical, but not byte-identical, so its per-track CRC32s do not match the 896 entry's `trackcrcs`. To match CUETools' semantics, we must verify against entries' parity/syndrome, not just `trackcrcs`.

If this hypothesis is wrong or incomplete, explain why.

### B. Verification design

Specify the algorithm we should implement. At minimum address:

1. **What disc-image buffer do we hand the codec?** Per-track flow: `[STRIDE zeros] + concat(decoded tracks in TOC order) + [STRIDE zeros]`. Single-image flow: same with one decoded image. Confirm or correct.
2. **Which entries do we verify against and in what order?** Specifically: should we (a) iterate all entries highest-confidence-first and pick the first that verifies, or (b) verify against all and report the highest-confidence match? What about offset trial — do we attempt a small `±N` window of stereo-sample offsets per entry, or only offset 0? CUETools tries small offsets; what's the standard window?
3. **Parity vs syndrome.** Some entries embed a small `parity="<base64>"` (NPAR=8) directly; most provide only `hasparity="<URL>"` (NPAR=16). All entries provide a base64 `syndrome` field. Should we always download parity and use `try_verify_with_word_offset`, or can we replicate CUETools' "syndrome-only" check from the `syndrome` field alone? If syndrome-only is feasible, what's the algorithm to derive a verification yes/no from the entry's `syndrome` string and our rip's audio without downloading parity? (Hint: CUETools.NET's `CTDBResponseEntry.HasParity` and `CDRepairEncode` are likely sources; cite or reproduce the relevant logic if you can.)
4. **Result semantics.** When an entry verifies via RS but our per-track CRC32 against that entry's `trackcrcs` still doesn't match (because the byte sequences differ but parity tolerates it), what status do we report? Proposed: `CtdbTrackStatus::VerifiedRs` (RS-verified, CRC differs) distinct from `Verified` (CRC matches), distinct from `Mismatch` (RS verification failed). UI consumers of `:ctdb-repair` should treat `VerifiedRs` as "no repair needed" — not the same as a CRC32 mismatch.
5. **Backward compatibility.** Repair flow currently uses the matched entry's `trackcrcs` for post-repair CRC32 verification. If we adopt RS-syndrome verification for the verify path, what does the post-repair check do? (Likely: still compute per-track CRC32 from the *repaired* audio, but compare against the canonical entry's `trackcrcs` — repair is supposed to bring the audio to byte-identical-with-canonical, so post-repair CRC32 *should* now match.)
6. **Multi-entry trial.** If RS verification at offset 0 fails for the 896 entry but succeeds for a confidence-3 entry, what do we report? Probably the 896-fail and the 3-confirm should both surface — UI proposal: show the highest-confidence verified entry, with a note if it's not the highest-confidence in the response.
7. **Performance.** RS verification involves multiplying `STRIDE × NPAR` GF(2^16) matrix syndromes. The codec is single-threaded; for a 70-min disc the audio buffer is ~700 MB i16. Is per-entry RS verify fast enough to iterate dozens of entries, or do we need to verify against just the top-K? Estimate.

### C. Implementation

Generate Rust code that adds RS-syndrome verification to `src/tui/ctdb.rs`, with these constraints:

1. Keep the existing `verify_ctdb` and `verify_ctdb_single_image` async functions' return type `CtdbVerifyResult` — extend if needed (new variant on `CtdbTrackStatus`, new fields on `CtdbVerifyResult`), but don't break existing call sites in `src/tui/command.rs` more than necessary.
2. Add `syndrome: Option<String>` and `parity: Option<String>` (base64) to `CtdbEntry`; extend `parse_ctdb_response` to extract them.
3. Implement `verify_disc_via_rs(audio_image: &[i16], entries: &[CtdbEntry], offset_window: i32) -> Option<RsVerifiedMatch>` (or whatever signature you find natural) that iterates entries, downloads parity for each as needed (cache by URL), runs `CtdbCodec::try_verify_with_word_offset` at each offset in the window, returns the highest-confidence verified entry.
4. Wire it into the existing track-CRC32 path: per-track CRC32 still runs and is shown in the overlay (useful diagnostic), but the OVERALL "verified" status comes from RS verification.
5. If you propose a `syndrome`-only fast path that avoids the parity download, implement it; otherwise note that CUETools' syndrome encoding is opaque and we must download parity.
6. Keep the existing `download_parity(url)` helper; cache results in an `HashMap<String, Vec<u8>>` for the duration of one verify call.
7. The codec's verify entry points are synchronous and CPU-bound — wrap in `tokio::task::spawn_blocking`.
8. Don't change the repair flow other than: post-repair, CRC32 verification *should* now succeed against the canonical entry's `trackcrcs` (since RS repair brings audio to canonical bytes); if it doesn't, surface the discrepancy (don't replace originals).

Output Rust source in fenced code blocks with clear `// in src/tui/ctdb.rs` annotations identifying the target file/region for each block. We do not need a complete file rewrite — diffs/insertions are fine, as long as the integration points are unambiguous.

### D. Open questions to flag back

If during your design you encounter ambiguity that requires source documentation we should fetch (CUETools.NET source, CTDB schema docs, RS codec spec details), list those questions in a final section so we can resolve them before writing more code.

---

## Reference materials you may want

- CUETools.NET source: https://github.com/gchudov/cuetools.net (in particular `CUETools.CTDB/CDRepairEncode.cs`, `CTDBResponseEntry.cs`, `CDRepairEncodeData.cs`).
- CTDB API (no public schema doc; sample request/response above).
- Reed-Solomon over GF(2^16) with primitive polynomial `0x1100B` (from CUETools.NET's `Galois.cs`).
- Our local RS codec: `src/ctdb_rs/mod.rs` (already-tested; 5 unit tests pass).
- Our current verify call site: `src/tui/ctdb.rs::verify_ctdb`, `verify_ctdb_single_image`.

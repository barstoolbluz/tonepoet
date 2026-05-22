//! DSDIFF (.dff) footer renderer for sacd_extract parity.
//!
//! Mirrors `dsdiff.c`'s footer emission in non-edit-master, default
//! mode (`-p` flag). The footer consists of three top-level chunks:
//!
//! 1. **DIIN** (Edited Master Information Chunk) — container holding
//!    a single MARK chunk (INDEX_ENTRY for the track), plus optional
//!    DIAR (artist) and DITI (title) text chunks.
//! 2. **COMT** (Comments Chunk) — two FILE_HISTORY comments: one
//!    with the SACD master_toc disc date plus "Material ripped from
//!    SACD: <title>", one with the wall-clock extraction time plus
//!    `SACD_RIPPER_VERSION_INFO`.
//! 3. **ID3** chunk (custom chunk_id `"ID3 "`) — embeds the same
//!    ID3v2.4 tag that DSF uses (reused from [`crate::id3`]).
//!
//! For Solo Monk track 1, the canonical footer is 454 bytes
//! (SHA-256 `2e84daf5...d38b2d4d`); the embedded ID3 tag is 179
//! bytes (SHA-256 `08c1e8fc...c090f5`, matching PR 3b).
//!
//! ## libdsdiff quirks replicated for byte-exact match
//!
//! 1. **Comment 1 month is 1-indexed** (`master_toc.disc_date_month`);
//!    **Comment 2 month is 0-indexed** (`tm_mon` from `localtime`).
//!    The two month fields use different conventions in the SAME
//!    file. Mirror.
//! 2. **`CALC_CHUNK_SIZE` rounds chunk_data_size up to even**. For
//!    odd payloads, the chunk_data_size field counts the trailing
//!    pad byte. Pad byte is always `0x00`.
//! 3. **MARK chunk for non-edit-master mode** has `count = 0` and no
//!    `marker_text`. Only one MARK per track (INDEX_ENTRY).
//! 4. **MARK `samples` field** encodes track duration's fractional
//!    frames as `frames × 588 × 64`. The `hours` field is
//!    `total_minutes / 60`, `minutes` is `total_minutes % 60`.
//! 5. **DIAR fallback chain ≠ DITI fallback chain**. The caller must
//!    do the resolution (DffMetadata accepts pre-resolved strings).
//! 6. **DITI uses TRACK title preferentially**, not album title. For
//!    Solo Monk track 1: DITI = "DINAH" (track title), NOT
//!    "SOLO MONK" (album title).
//! 7. **Comment 1 text format**: `"Material ripped from SACD: " + title`
//!    where title comes from `disc_title` or `album_title` fallback.
//! 8. **Comment 2 text** is `SACD_RIPPER_VERSION_INFO` — a build-time
//!    macro on the sacd_extract side. Varies by sacd_extract build
//!    (contains git commit hash). Caller passes the exact string.
//! 9. **ID3 chunk_id is `"ID3 "`** with TRAILING SPACE (4 ASCII bytes).
//!    Not the spec's `"ID3"`.
//! 10. **`localtime`-dependent wall-clock timestamp** in comment 2 —
//!    timezone-and-time-specific. Caller provides; reproducible
//!    given identical inputs.

use crate::id3::{render_id3v24, Id3Metadata};

/// Round `n` up to the next even number. Mirrors libsacd's
/// `CEIL_ODD_NUMBER` macro (despite the misleading name, it rounds
/// UP to even).
fn ceil_odd(n: u64) -> u64 {
    if n % 2 == 1 {
        n + 1
    } else {
        n
    }
}

/// All metadata required to render the DFF footer for non-edit-master
/// output. Each frame is independent; absent fields produce no chunk.
///
/// Caller is responsible for SACD-specific fallback resolution. The
/// `diar` and `diti` fields should hold the pre-resolved strings,
/// not the raw SACD metadata.
#[derive(Debug, Clone)]
pub struct DffMetadata {
    /// DIAR text. Resolve via scarletbook_id3's fallback chain:
    /// track_type_performer → disc_artist → disc_artist_phonetic →
    /// album_artist → album_artist_phonetic → disc_title → ...
    pub diar: Option<String>,

    /// DITI text. Resolve via scarletbook_id3's fallback chain:
    /// track_type_title → album_title → album_title_phonetic →
    /// disc_title → disc_title_phonetic.
    pub diti: Option<String>,

    /// Total minutes in track duration (NOT split into hours/minutes
    /// yet — renderer does the hours = N/60, minutes = N%60 split).
    pub duration_minutes_total: u32,
    /// Seconds component of duration [0, 59].
    pub duration_seconds: u8,
    /// Fractional-frames component of duration [0, 74] @ 75fps.
    pub duration_frames: u8,

    /// COMT comment 1 timestamp: SACD master_toc disc date.
    pub disc_date_year: u16,
    /// 1-indexed month [1, 12] from master_toc.disc_date_month.
    pub disc_date_month_1_indexed: u8,
    pub disc_date_day: u8,
    /// Title used in COMT comment 1's "Material ripped from SACD: X".
    pub disc_or_album_title: String,

    /// COMT comment 2 timestamp: wall-clock at extraction time
    /// (`localtime`). Different runs produce different bytes by
    /// design.
    pub creation_year: u16,
    /// 0-indexed month [0, 11] from `tm_mon` (e.g., May = 4).
    pub creation_month_0_indexed: u8,
    pub creation_day: u8,
    pub creation_hour: u8,
    pub creation_minute: u8,
    /// COMT comment 2 text — sacd_extract's `SACD_RIPPER_VERSION_INFO`
    /// equivalent. Includes build-specific git hash. Pass the exact
    /// string for byte-exact match.
    pub creating_machine: String,

    /// Embedded ID3v2.4 tag. Reuses [`crate::id3::render_id3v24`]
    /// from PR 3b.
    pub id3: Id3Metadata,
}

const MARK_TYPE_INDEX_ENTRY: u16 = 4;
const COMMENT_TYPE_FILE_HISTORY: u16 = 3;
const COMMENT_REF_FILE_HISTORY_GENERAL: u16 = 0;
const COMMENT_REF_FILE_HISTORY_CREATING_MACHINE: u16 = 2;
const SAMPLES_PER_FRAME: u32 = 588;

/// Emit a single MARK chunk for non-edit-master mode (INDEX_ENTRY
/// type, count=0). Layout per the `marker_chunk_t` struct (34 bytes,
/// no marker_text since count=0).
fn emit_mark(out: &mut Vec<u8>, meta: &DffMetadata) {
    let hours = (meta.duration_minutes_total / 60) as u16;
    let minutes = (meta.duration_minutes_total % 60) as u8;
    let samples = meta.duration_frames as u32 * SAMPLES_PER_FRAME * 64;

    out.extend_from_slice(b"MARK");
    out.extend_from_slice(&22u64.to_be_bytes()); // chunk_data_size = 22
    out.extend_from_slice(&hours.to_be_bytes());
    out.push(minutes);
    out.push(meta.duration_seconds);
    out.extend_from_slice(&samples.to_be_bytes());
    out.extend_from_slice(&0i32.to_be_bytes()); // offset
    out.extend_from_slice(&MARK_TYPE_INDEX_ENTRY.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // mark_channel = ALL
    out.extend_from_slice(&0u16.to_be_bytes()); // track_flags
    out.extend_from_slice(&0u32.to_be_bytes()); // count = 0 (no marker_text)
}

/// Emit a text chunk (DIAR or DITI) with the given chunk_id and
/// text. Layout: 4 chunk_id + 8 chunk_data_size + 4 count + N text
/// bytes + optional pad byte (if text length is odd).
fn emit_text_chunk(out: &mut Vec<u8>, chunk_id: &[u8; 4], text: &str) {
    let text_bytes = text.as_bytes();
    let count = text_bytes.len() as u32;
    // chunk_data_size = CALC_CHUNK_SIZE(4 (count field) + count text).
    // Round up to even.
    let raw_size = 4 + count as u64;
    let chunk_data_size = ceil_odd(raw_size);

    out.extend_from_slice(chunk_id);
    out.extend_from_slice(&chunk_data_size.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(text_bytes);
    if count % 2 == 1 {
        out.push(0x00); // pad byte
    }
}

/// Emit the DIIN container with MARK + optional DIAR + optional DITI.
fn emit_diin(out: &mut Vec<u8>, meta: &DffMetadata) {
    // Render the inner chunks first, then prepend the DIIN header
    // with the correct chunk_data_size.
    let mut inner = Vec::<u8>::new();
    emit_mark(&mut inner, meta);
    if let Some(ref s) = meta.diar {
        emit_text_chunk(&mut inner, b"DIAR", s);
    }
    if let Some(ref s) = meta.diti {
        emit_text_chunk(&mut inner, b"DITI", s);
    }

    out.extend_from_slice(b"DIIN");
    out.extend_from_slice(&(inner.len() as u64).to_be_bytes());
    out.extend_from_slice(&inner);
}

/// Emit the COMT chunk with exactly 2 comments matching
/// sacd_extract's emission.
fn emit_comt(out: &mut Vec<u8>, meta: &DffMetadata) {
    let mut inner = Vec::<u8>::new();
    // numcomments
    inner.extend_from_slice(&2u16.to_be_bytes());

    // Comment 1: "Material ripped from SACD: <title>" with disc-date
    // timestamp (1-indexed month from master_toc).
    let comment_1_text = format!("Material ripped from SACD: {}", meta.disc_or_album_title);
    emit_comment(
        &mut inner,
        meta.disc_date_year,
        meta.disc_date_month_1_indexed, // 1-indexed (master_toc convention)
        meta.disc_date_day,
        0,
        0,
        COMMENT_TYPE_FILE_HISTORY,
        COMMENT_REF_FILE_HISTORY_GENERAL,
        &comment_1_text,
    );

    // Comment 2: SACD_RIPPER_VERSION_INFO with wall-clock timestamp
    // (0-indexed month from tm_mon).
    emit_comment(
        &mut inner,
        meta.creation_year,
        meta.creation_month_0_indexed, // 0-indexed (tm_mon convention)
        meta.creation_day,
        meta.creation_hour,
        meta.creation_minute,
        COMMENT_TYPE_FILE_HISTORY,
        COMMENT_REF_FILE_HISTORY_CREATING_MACHINE,
        &meta.creating_machine,
    );

    // COMT chunk header + body
    out.extend_from_slice(b"COMT");
    out.extend_from_slice(&(inner.len() as u64).to_be_bytes());
    out.extend_from_slice(&inner);
}

/// Emit a single Comment (within COMT). Layout per `comment_t` (14
/// bytes through count, then text + pad).
#[allow(clippy::too_many_arguments)]
fn emit_comment(
    out: &mut Vec<u8>,
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    comment_type: u16,
    comment_reference: u16,
    text: &str,
) {
    let text_bytes = text.as_bytes();
    let count = text_bytes.len() as u32;

    out.extend_from_slice(&year.to_be_bytes());
    out.push(month);
    out.push(day);
    out.push(hour);
    out.push(minute);
    out.extend_from_slice(&comment_type.to_be_bytes());
    out.extend_from_slice(&comment_reference.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(text_bytes);
    if count % 2 == 1 {
        out.push(0x00); // pad byte (NOT counted in count field)
    }
}

/// Emit the ID3 chunk: 12-byte chunk header (chunk_id="ID3 ",
/// chunk_data_size = CALC_CHUNK_SIZE(id3_len)) + the full ID3v2.4
/// tag bytes + optional pad if odd.
fn emit_id3_chunk(out: &mut Vec<u8>, id3_bytes: &[u8]) {
    let raw_size = id3_bytes.len() as u64;
    let chunk_data_size = ceil_odd(raw_size);

    out.extend_from_slice(b"ID3 ");
    out.extend_from_slice(&chunk_data_size.to_be_bytes());
    out.extend_from_slice(id3_bytes);
    if raw_size % 2 == 1 {
        out.push(0x00); // pad byte
    }
}

/// Render the complete DFF footer (DIIN + COMT + ID3 chunks). Output
/// bytes are byte-identical to sacd_extract's `dsdiff_close` non-
/// edit-master footer emission for the same input metadata.
pub fn render_dff_footer(meta: &DffMetadata) -> Vec<u8> {
    let mut out = Vec::<u8>::new();
    emit_diin(&mut out, meta);
    emit_comt(&mut out, meta);
    let id3_bytes = render_id3v24(&meta.id3);
    emit_id3_chunk(&mut out, &id3_bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::sha256_hex;

    // ============================================================
    //  ceil_odd
    // ============================================================

    #[test]
    fn ceil_odd_keeps_even_unchanged() {
        assert_eq!(ceil_odd(0), 0);
        assert_eq!(ceil_odd(4), 4);
        assert_eq!(ceil_odd(100), 100);
    }

    #[test]
    fn ceil_odd_rounds_odd_up() {
        assert_eq!(ceil_odd(1), 2);
        assert_eq!(ceil_odd(19), 20);
        assert_eq!(ceil_odd(83), 84);
        assert_eq!(ceil_odd(179), 180);
    }

    // ============================================================
    //  MARK chunk
    // ============================================================

    #[test]
    fn mark_chunk_matches_solo_monk_canonical_bytes() {
        // Solo Monk track 1: duration 02:28:31 → MARK chunk bytes
        // verified empirically against canonical 34-byte MARK chunk
        // (SHA-256 7c0206aa...60c14f43 from pre-step audit).
        let meta = solo_monk_dff_metadata();
        let mut buf = Vec::new();
        emit_mark(&mut buf, &meta);
        assert_eq!(buf.len(), 34);
        assert_eq!(
            sha256_hex(&buf),
            "7c0206aa33d00ae1e6996a2f2a8711cd343cca072093494e43222d7e60c14f43",
        );
    }

    #[test]
    fn mark_chunk_long_track_splits_minutes_into_hours() {
        // 70-minute track: hours=1, minutes=10, seconds=0, frames=0.
        let mut meta = solo_monk_dff_metadata();
        meta.duration_minutes_total = 70;
        meta.duration_seconds = 0;
        meta.duration_frames = 0;
        let mut buf = Vec::new();
        emit_mark(&mut buf, &meta);
        // Read hours u16 BE at offset 12.
        let hours = u16::from_be_bytes([buf[12], buf[13]]);
        assert_eq!(hours, 1);
        let minutes = buf[14];
        assert_eq!(minutes, 10);
    }

    // ============================================================
    //  DIAR / DITI text chunks
    // ============================================================

    #[test]
    fn diar_chunk_matches_solo_monk_canonical() {
        let mut buf = Vec::new();
        emit_text_chunk(&mut buf, b"DIAR", "THELONIOUS MONK");
        assert_eq!(buf.len(), 32); // 16 header + 15 text + 1 pad
        assert_eq!(
            sha256_hex(&buf),
            "cf4f2a45fb398b64015e475741235dbc6498edc72db8238679092a9e80937c51",
        );
    }

    #[test]
    fn diti_chunk_matches_solo_monk_canonical() {
        let mut buf = Vec::new();
        emit_text_chunk(&mut buf, b"DITI", "DINAH");
        assert_eq!(buf.len(), 22); // 16 header + 5 text + 1 pad
        assert_eq!(
            sha256_hex(&buf),
            "b4f75b0f3ccf3cbbb5c4aa210764f77e46f9b96619e70ca238339d9778142332",
        );
    }

    #[test]
    fn text_chunk_even_length_no_pad() {
        // Text of even length should NOT have a trailing pad byte.
        let mut buf = Vec::new();
        emit_text_chunk(&mut buf, b"DIAR", "AB"); // 2 chars (even)
        assert_eq!(buf.len(), 16 + 2); // no pad
                                       // chunk_data_size = 4 (count) + 2 (text) = 6 (even)
        let size = u64::from_be_bytes(buf[4..12].try_into().unwrap());
        assert_eq!(size, 6);
    }

    #[test]
    fn text_chunk_odd_length_has_pad_byte() {
        let mut buf = Vec::new();
        emit_text_chunk(&mut buf, b"DIAR", "X"); // 1 char (odd)
        assert_eq!(buf.len(), 16 + 1 + 1); // text + pad
                                           // chunk_data_size = 4 + 1 = 5 → rounded to 6 (even)
        let size = u64::from_be_bytes(buf[4..12].try_into().unwrap());
        assert_eq!(size, 6);
        assert_eq!(buf[17], 0x00); // pad byte
    }

    // ============================================================
    //  DIIN container
    // ============================================================

    #[test]
    fn diin_chunk_matches_solo_monk_canonical() {
        let meta = solo_monk_dff_metadata();
        let mut buf = Vec::new();
        emit_diin(&mut buf, &meta);
        assert_eq!(buf.len(), 100); // 12 header + 88 children
        assert_eq!(
            sha256_hex(&buf),
            "3e83b66c6b2c1551fdb8fb9110b0627c3211eef060ab4c4c7a314900d77bcbd4",
        );
    }

    #[test]
    fn diin_with_no_text_chunks_only_emits_mark() {
        let mut meta = solo_monk_dff_metadata();
        meta.diar = None;
        meta.diti = None;
        let mut buf = Vec::new();
        emit_diin(&mut buf, &meta);
        // DIIN header (12) + MARK (34) = 46 bytes
        assert_eq!(buf.len(), 46);
        // chunk_data_size = 34
        let size = u64::from_be_bytes(buf[4..12].try_into().unwrap());
        assert_eq!(size, 34);
    }

    // ============================================================
    //  Comment 1 / Comment 2
    // ============================================================

    #[test]
    fn comment_1_uses_1_indexed_month() {
        // Solo Monk: comment 1 has month=10 (October), 1-indexed.
        let meta = solo_monk_dff_metadata();
        let mut buf = Vec::new();
        emit_comt(&mut buf, &meta);
        // COMT body starts at offset 14 (12 header + 2 numcomments).
        // Comment 1 at offset 14. Month byte at offset 14+2 = 16.
        assert_eq!(buf[16], 10);
    }

    #[test]
    fn comment_2_uses_0_indexed_month() {
        // Solo Monk: comment 2 has tm_mon=4 (May, 0-indexed).
        let meta = solo_monk_dff_metadata();
        let mut buf = Vec::new();
        emit_comt(&mut buf, &meta);
        // Comment 1 starts at offset 14, occupies 50 bytes
        // (14 + 36 text, even, no pad). Comment 2 at offset 64.
        // Month byte at 64+2 = 66.
        assert_eq!(buf[66], 4);
    }

    // ============================================================
    //  COMT chunk
    // ============================================================

    #[test]
    fn comt_chunk_matches_solo_monk_canonical() {
        let meta = solo_monk_dff_metadata();
        let mut buf = Vec::new();
        emit_comt(&mut buf, &meta);
        assert_eq!(buf.len(), 162); // 12 header + 150 data
        assert_eq!(
            sha256_hex(&buf),
            "f7338123393b77e25ddedd583ddc6d9a00467d0f2923620aaf34bf68d05aeecc",
        );
    }

    // ============================================================
    //  ID3 chunk wrapper
    // ============================================================

    #[test]
    fn id3_chunk_wrapper_adds_pad_for_odd_id3_tag() {
        // PR 3b's render_id3v24 produces 179 bytes for Solo Monk.
        // The DFF wrapper rounds chunk_data_size to 180 (even) and
        // appends 1 pad byte.
        let id3_tag = vec![0x42u8; 179];
        let mut buf = Vec::new();
        emit_id3_chunk(&mut buf, &id3_tag);
        assert_eq!(buf.len(), 12 + 180); // 12 header + 179 data + 1 pad
        assert_eq!(&buf[0..4], b"ID3 ");
        let size = u64::from_be_bytes(buf[4..12].try_into().unwrap());
        assert_eq!(size, 180); // CALC_CHUNK_SIZE(179) = 180
        assert_eq!(buf[12 + 179], 0x00); // pad
    }

    #[test]
    fn id3_chunk_matches_solo_monk_canonical() {
        // Full ID3 chunk including the "ID3 " wrapper + embedded
        // PR 3b ID3 tag for Solo Monk.
        let meta = solo_monk_dff_metadata();
        let id3_bytes = render_id3v24(&meta.id3);
        let mut buf = Vec::new();
        emit_id3_chunk(&mut buf, &id3_bytes);
        assert_eq!(buf.len(), 192);
        assert_eq!(
            sha256_hex(&buf),
            "78dc020f701216ae2573336bc829195a1356783ccda3d7e16bcca2b7da2fd213",
        );
    }

    // ============================================================
    //  Full footer (byte-exact gate)
    // ============================================================

    #[test]
    fn render_dff_footer_matches_solo_monk_canonical() {
        let meta = solo_monk_dff_metadata();
        let out = render_dff_footer(&meta);
        assert_eq!(out.len(), 454);
        // Canonical SHA-256 from pre-step parsing of
        // /tmp/sacd-compare/c-ref-dff/SOLO MONK/01 - DINAH.dff
        // bytes [104720592, 104721046).
        assert_eq!(
            sha256_hex(&out),
            "2e84daf5560d10b602a319cb0b7ccca7ce2ac4d90d11e809bf3c1affd38b2d4d",
        );
    }

    // ============================================================
    //  Helpers
    // ============================================================

    /// Canonical Solo Monk track 1 DFF metadata. Empirically derived
    /// from `/tmp/sacd-compare/c-ref-dff/SOLO MONK/01 - DINAH.dff`
    /// during the PR 3c pre-step.
    fn solo_monk_dff_metadata() -> DffMetadata {
        DffMetadata {
            diar: Some("THELONIOUS MONK".into()),
            diti: Some("DINAH".into()),
            duration_minutes_total: 2,
            duration_seconds: 28,
            duration_frames: 31,
            disc_date_year: 1999,
            disc_date_month_1_indexed: 10,
            disc_date_day: 27,
            disc_or_album_title: "SOLO MONK".into(),
            creation_year: 2026,
            creation_month_0_indexed: 4, // tm_mon for May
            creation_day: 13,
            creation_hour: 18,
            creation_minute: 20,
            creating_machine: "SACD extract 0.3.9.3 \n0.3.9.3-dirty\nCopyright (c) 2010-2020 by respective authors.\n".into(),
            id3: solo_monk_id3_metadata(),
        }
    }

    fn solo_monk_id3_metadata() -> Id3Metadata {
        // Same values as in PR 3b's
        // render_solo_monk_track_1_matches_canonical_footer test.
        Id3Metadata {
            tit2: Some("DINAH".into()),
            talb: Some("SOLO MONK".into()),
            tpe1: Some("THELONIOUS MONK".into()),
            tpe2: None,
            txxx_performer: None,
            tcom: None,
            tsrc: Some("USSM19917805".into()),
            tpub: None,
            tcop: None,
            tpos: Some((1, 1)),
            tcon: Some("Other".into()),
            tyer: Some(1999),
            tdat: Some((10, 27)),
            trck: Some((1, 13)),
        }
    }
}

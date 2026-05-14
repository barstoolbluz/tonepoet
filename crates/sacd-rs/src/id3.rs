//! ID3v2.4 footer renderer for SACD-extract parity.
//!
//! Mirrors sacd_extract's `scarletbook_id3_tag_render` in
//! `id3_tag_mode = 4` (default): produces an ID3v2.4 tag with
//! UTF-8 text encoding for most frames, ISO-8859-1 for TYER and
//! TDAT, and the various libid3 quirks documented below.
//!
//! ## Goal: byte-exact match with sacd_extract's libid3 output
//!
//! This module exists to enable whole-file byte-exact comparison
//! between sacd-rs DSF output and `sacd_extract -s -t N`. The
//! verification fixture (Solo Monk track 1) has a 179-byte ID3v2.4
//! footer with SHA-256
//! `08c1e8fc66ae9de5eec34347a278fe34b6db2775dddeb29668259bbf59c090f5`.
//! Our renderer produces those exact bytes given the corresponding
//! [`Id3Metadata`].
//!
//! ## libid3 quirks replicated for byte-exact match
//!
//! 1. **TYER and TDAT use ISO-8859-1 encoding** (encoding byte
//!    `0x00`), not UTF-8 (`0x03`) like other text frames. The C
//!    `scarletbook_id3_tag_render` calls `id3_set_text` (which
//!    hardcodes ISO-8859-1) for these two frames, but
//!    `id3_set_text_wraper` (which respects mode 4) for everything
//!    else.
//!
//! 2. **TXXX description is `"PERFORMER"` uppercase**, not
//!    `"Performer"`. From libid3's `id3_set_text__performer_utf8`.
//!
//! 3. **TXXX value has a trailing null** (the ID3v2.4 spec doesn't
//!    require one — the frame boundary terminates the value — but
//!    libid3 appends one anyway due to its size accounting:
//!    `fr_raw_size = strlen("PERFORMER") + strlen(text) + 3`).
//!
//! 4. **Tag/frame size encoding is 8-8-8-7 bit packing**, NOT the
//!    spec's 7-7-7-7 synchsafe. libid3's `ID3_SET_SIZE28` macro
//!    expands to `[size>>23, size>>15, size>>7, size & 0x7F] &
//!    [0xFF, 0xFF, 0xFF, 0x7F]`. For sizes < 16384 (every realistic
//!    case), this is byte-identical to standard synchsafe (the
//!    high bits of bytes a/b/c are 0 either way). For sizes
//!    ≥ 16384, libid3 produces non-spec output; we mirror.
//!
//! 5. **TDAT byte order is "MMDD" (month-day)**, violating the
//!    ID3v2.3 spec which requires "DDMM" (day-month). The C uses
//!    `snprintf("%02d%02d", month, day)` — month first. We mirror.
//!
//! 6. **Tag header**: version=4, revision=0, flags=0x00. Frame
//!    flags: always 0x0000.
//!
//! 7. **Frame emission order is fixed**: TIT2, TALB, TPE1, TPE2,
//!    TXXX, TCOM, TSRC, TPUB, TCOP, TPOS, TCON, TYER, TDAT, TRCK
//!    (frames are omitted if the corresponding metadata is `None`).

/// Metadata for ID3v2.4 footer rendering. Each field corresponds
/// to exactly one ID3 frame; `None` means the frame is omitted from
/// the output.
///
/// The caller is responsible for any SACD-specific fallback
/// resolution (e.g., disc_artist fallback chain for TPE1) — this
/// struct represents the final, pre-resolved values to emit.
#[derive(Debug, Clone, Default)]
pub struct Id3Metadata {
    /// Track title (TIT2).
    pub tit2: Option<String>,
    /// Album title (TALB).
    pub talb: Option<String>,
    /// Lead performer/artist (TPE1). For sacd_extract parity, set
    /// from track_performer if present, else fall back through
    /// disc_artist/disc_artist_phonetic/album_artist/...
    pub tpe1: Option<String>,
    /// Band/orchestra (TPE2). Set from master_text.album_artist
    /// only (NOT from disc_artist).
    pub tpe2: Option<String>,
    /// Performer (TXXX with description "PERFORMER"). Emitted only
    /// when track-level performer text is set.
    pub txxx_performer: Option<String>,
    /// Composer (TCOM).
    pub tcom: Option<String>,
    /// ISRC (TSRC), 12 ASCII chars in country+owner+year+designation
    /// concatenation.
    pub tsrc: Option<String>,
    /// Publisher (TPUB).
    pub tpub: Option<String>,
    /// Copyright (TCOP).
    pub tcop: Option<String>,
    /// Disc number / set size (TPOS). Format "n/m".
    pub tpos: Option<(u16, u16)>,
    /// Genre name (TCON). Already mapped from SACD genre code via
    /// [`sacd_genre_to_id3_string`].
    pub tcon: Option<String>,
    /// 4-digit recording year (TYER, ISO-8859-1).
    pub tyer: Option<u16>,
    /// Recording month + day (TDAT, ISO-8859-1, **MMDD** order).
    pub tdat: Option<(u8, u8)>,
    /// Track number / total tracks (TRCK). Format "n/m".
    pub trck: Option<(u16, u16)>,
}

/// Encode a 32-bit size into 4 bytes using libid3's `ID3_SET_SIZE28`
/// macro. NOT spec-synchsafe — uses 8-8-8-7 bit packing instead of
/// 7-7-7-7. Coincidentally matches synchsafe for sizes < 16384 (every
/// realistic case), so it's byte-identical to spec output in
/// practice. Replicated here for byte-exact compatibility with
/// sacd_extract's libid3 output.
pub fn libid3_size28(size: u32) -> [u8; 4] {
    [
        ((size >> 23) & 0xFF) as u8,
        ((size >> 15) & 0xFF) as u8,
        ((size >> 7) & 0xFF) as u8,
        (size & 0x7F) as u8,
    ]
}

/// Map an SACD genre code (0..30) to its ID3 genre name string,
/// mirroring scarletbook_id3.c's `sacd_id3_genres[]` lookup
/// combined with libid3's `genre_table[]`. Codes outside [0, 30]
/// return "Other".
pub fn sacd_genre_to_id3_string(sacd_code: u8) -> &'static str {
    // sacd_id3_genres[sacd_code] → ID3 genre index. From the C
    // source in scarletbook_id3.c.
    const SACD_TO_ID3_INDEX: [u8; 31] = [
        12,  // 0  Not used        → Other
        12,  // 1  Not defined     → Other
        60,  // 2  Adult Cont.     → Top 40
        40,  // 3  Alt. Rock       → AlternRock
        12,  // 4  Children's      → Other
        32,  // 5  Classical       → Classical
        140, // 6  Contemp. Christian
        2,   // 7  Country         → Country
        3,   // 8  Dance           → Dance
        98,  // 9  Easy Listening
        109, // 10 Erotic          → Porn Groove
        80,  // 11 Folk            → Folk
        38,  // 12 Gospel          → Gospel
        7,   // 13 Hip Hop         → Hip-Hop
        8,   // 14 Jazz            → Jazz
        86,  // 15 Latin           → Latin
        77,  // 16 Musical         → Musical
        10,  // 17 New Age         → New Age
        103, // 18 Opera           → Opera
        104, // 19 Operetta        → Chamber Music
        13,  // 20 Pop Music       → Pop
        15,  // 21 RAP             → Rap
        16,  // 22 Reggae          → Reggae
        17,  // 23 Rock Music      → Rock
        14,  // 24 R&B             → R&B
        37,  // 25 Sound Effects   → Sound Clip
        24,  // 26 Sound Track     → Soundtrack
        101, // 27 Spoken Word     → Speech
        48,  // 28 World Music     → Ethnic
        0,   // 29 Blues           → Blues
        12,  // 30 Not used        → Other
    ];
    let id3_index = if (sacd_code as usize) < SACD_TO_ID3_INDEX.len() {
        SACD_TO_ID3_INDEX[sacd_code as usize]
    } else {
        12 // Other
    };
    id3_genre_name(id3_index)
}

/// ID3v1 genre names indexed by ID3 genre code. Reproduced from
/// libid3's `genre.dat`. Index 12 = "Other" is the fallback for
/// unknown codes.
fn id3_genre_name(idx: u8) -> &'static str {
    // Subset covering only the codes that scarletbook_id3 actually
    // emits (per SACD_TO_ID3_INDEX above). Other codes return "Other".
    match idx {
        0 => "Blues",
        2 => "Country",
        3 => "Dance",
        7 => "Hip-Hop",
        8 => "Jazz",
        10 => "New Age",
        12 => "Other",
        13 => "Pop",
        14 => "R&B",
        15 => "Rap",
        16 => "Reggae",
        17 => "Rock",
        24 => "Soundtrack",
        32 => "Classical",
        37 => "Sound Clip",
        38 => "Gospel",
        40 => "AlternRock",
        48 => "Ethnic",
        60 => "Top 40",
        77 => "Musical",
        80 => "Folk",
        86 => "Latin",
        98 => "Easy Listening",
        101 => "Speech",
        103 => "Opera",
        104 => "Chamber Music",
        109 => "Porn Groove",
        140 => "Contemporary Christian",
        _ => "Other",
    }
}

/// Render an ID3v2.4 tag from `meta`. Frames are emitted in the
/// fixed sacd_extract order, omitting those whose corresponding
/// metadata field is `None`.
///
/// Output is a `Vec<u8>` containing the complete tag (10-byte tag
/// header + frames). The returned bytes are byte-identical to
/// sacd_extract's libid3 output for the same metadata.
pub fn render_id3v24(meta: &Id3Metadata) -> Vec<u8> {
    // Accumulate frames first, then prepend the tag header with
    // the correct total size.
    let mut frames = Vec::<u8>::new();

    if let Some(ref s) = meta.tit2 {
        emit_text_frame(&mut frames, b"TIT2", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.talb {
        emit_text_frame(&mut frames, b"TALB", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tpe1 {
        emit_text_frame(&mut frames, b"TPE1", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tpe2 {
        emit_text_frame(&mut frames, b"TPE2", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.txxx_performer {
        emit_txxx_performer(&mut frames, s);
    }
    if let Some(ref s) = meta.tcom {
        emit_text_frame(&mut frames, b"TCOM", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tsrc {
        emit_text_frame(&mut frames, b"TSRC", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tpub {
        emit_text_frame(&mut frames, b"TPUB", s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tcop {
        emit_text_frame(&mut frames, b"TCOP", s, ENCODING_UTF8);
    }
    if let Some((n, m)) = meta.tpos {
        let s = format!("{}/{}", n, m);
        emit_text_frame(&mut frames, b"TPOS", &s, ENCODING_UTF8);
    }
    if let Some(ref s) = meta.tcon {
        emit_text_frame(&mut frames, b"TCON", s, ENCODING_UTF8);
    }
    if let Some(year) = meta.tyer {
        // libid3 quirk #1: TYER uses encoding 0x00 (ISO-8859-1),
        // not UTF-8. Content is "%04d" zero-padded.
        let s = format!("{:04}", year);
        emit_text_frame(&mut frames, b"TYER", &s, ENCODING_ISO_8859_1);
    }
    if let Some((month, day)) = meta.tdat {
        // libid3 quirk #1 + #5: TDAT uses encoding 0x00 AND format
        // is "MMDD" (month-day, violating ID3v2.3 spec which is
        // "DDMM"). scarletbook_id3.c: snprintf("%02d%02d", month, day).
        let s = format!("{:02}{:02}", month, day);
        emit_text_frame(&mut frames, b"TDAT", &s, ENCODING_ISO_8859_1);
    }
    if let Some((n, m)) = meta.trck {
        let s = format!("{}/{}", n, m);
        emit_text_frame(&mut frames, b"TRCK", &s, ENCODING_UTF8);
    }

    // Tag header: "ID3" + version=4 + revision=0 + flags=0 + size.
    let tag_data_size = frames.len() as u32;
    let size_bytes = libid3_size28(tag_data_size);
    let mut out = Vec::with_capacity(10 + frames.len());
    out.extend_from_slice(b"ID3");
    out.push(0x04); // version major
    out.push(0x00); // revision
    out.push(0x00); // flags
    out.extend_from_slice(&size_bytes);
    out.extend_from_slice(&frames);
    out
}

const ENCODING_ISO_8859_1: u8 = 0x00;
const ENCODING_UTF8: u8 = 0x03;

/// Emit a text frame (`TIT2`, `TALB`, etc.) with the given encoding
/// byte. Content layout: `[encoding][text bytes][null terminator]`.
///
/// Frame size in the 10-byte header = `text.len() + 2` (1 encoding
/// byte + 1 null). This matches libid3's `id3_set_text_utf8`
/// (`fr_raw_size = strlen(text) + 2`).
fn emit_text_frame(out: &mut Vec<u8>, frame_id: &[u8; 4], text: &str, encoding: u8) {
    let text_bytes = text.as_bytes();
    let fr_size = (text_bytes.len() + 2) as u32; // encoding + text + null
    out.extend_from_slice(frame_id);
    out.extend_from_slice(&libid3_size28(fr_size));
    out.extend_from_slice(&[0x00, 0x00]); // frame flags
    out.push(encoding);
    out.extend_from_slice(text_bytes);
    out.push(0x00); // null terminator
}

/// Emit a TXXX "User defined text information" frame with the
/// hardcoded description `"PERFORMER"` (uppercase per libid3) and
/// `text` as the value. Content layout:
/// `[encoding=0x03]["PERFORMER"][0x00][text bytes][0x00]`.
///
/// Frame size = `strlen("PERFORMER") + strlen(text) + 3` per
/// libid3's `id3_set_text__performer_utf8` (1 encoding + 9 desc +
/// 1 desc-null + N text + 1 value-null). The value-null is libid3
/// quirk #3 (ID3v2.4 spec doesn't require it).
fn emit_txxx_performer(out: &mut Vec<u8>, text: &str) {
    const DESC: &[u8] = b"PERFORMER"; // 9 bytes, uppercase per libid3
    let text_bytes = text.as_bytes();
    let fr_size = (1 + DESC.len() + 1 + text_bytes.len() + 1) as u32;
    out.extend_from_slice(b"TXXX");
    out.extend_from_slice(&libid3_size28(fr_size));
    out.extend_from_slice(&[0x00, 0x00]); // frame flags
    out.push(ENCODING_UTF8);
    out.extend_from_slice(DESC);
    out.push(0x00); // description null terminator
    out.extend_from_slice(text_bytes);
    out.push(0x00); // value null terminator (libid3 quirk)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    //  libid3_size28 unit tests
    // ============================================================

    #[test]
    fn libid3_size28_zero() {
        assert_eq!(libid3_size28(0), [0, 0, 0, 0]);
    }

    #[test]
    fn libid3_size28_small_values_match_standard_synchsafe() {
        // For sizes < 128, only the d byte is non-zero. Identical
        // to standard synchsafe.
        assert_eq!(libid3_size28(7), [0, 0, 0, 7]);
        assert_eq!(libid3_size28(127), [0, 0, 0, 127]);
    }

    #[test]
    fn libid3_size28_solo_monk_tag_size() {
        // Canonical fixture: Solo Monk track 1's tag data size is
        // 169 bytes. libid3 encodes this as [0, 0, 1, 0x29].
        // Confirmed empirically against
        // /tmp/sacd-compare/c-ref-dsf/SOLO MONK/01 - DINAH.dsf.
        assert_eq!(libid3_size28(169), [0, 0, 1, 0x29]);
    }

    #[test]
    fn libid3_size28_boundary_at_16384_diverges_from_synchsafe() {
        // For size = 16384 = 2^14, libid3 produces [0, 0, 0x80, 0]
        // (c byte has high bit set; NON-spec-synchsafe). Standard
        // synchsafe would produce [0, 1, 0, 0]. This is the libid3
        // quirk that practically never shows for real SACDs (no
        // frame has size ≥ 16384).
        assert_eq!(libid3_size28(16384), [0, 0, 0x80, 0]);
    }

    #[test]
    fn libid3_size28_128_byte_boundary() {
        // size = 128 = 2^7. libid3 and standard synchsafe both
        // produce [0, 0, 1, 0].
        assert_eq!(libid3_size28(128), [0, 0, 1, 0]);
    }

    // ============================================================
    //  Text frame encoding
    // ============================================================

    #[test]
    fn text_frame_utf8_layout() {
        let mut buf = Vec::new();
        emit_text_frame(&mut buf, b"TIT2", "DINAH", ENCODING_UTF8);
        // 10-byte header: "TIT2" + size(=7) + flags(0,0)
        assert_eq!(&buf[0..4], b"TIT2");
        assert_eq!(&buf[4..8], &libid3_size28(7)); // fr_size = 5 chars + 1 enc + 1 null
        assert_eq!(&buf[8..10], &[0x00, 0x00]);
        // Frame data: encoding + text + null
        assert_eq!(buf[10], ENCODING_UTF8);
        assert_eq!(&buf[11..16], b"DINAH");
        assert_eq!(buf[16], 0x00);
        assert_eq!(buf.len(), 17); // 10 header + 7 data
    }

    #[test]
    fn text_frame_iso_8859_1_uses_zero_encoding_byte() {
        let mut buf = Vec::new();
        emit_text_frame(&mut buf, b"TYER", "1999", ENCODING_ISO_8859_1);
        assert_eq!(buf[10], ENCODING_ISO_8859_1);
        assert_eq!(buf[10], 0x00, "TYER must use ISO-8859-1 (0x00), NOT UTF-8");
        assert_eq!(&buf[11..15], b"1999");
        assert_eq!(buf[15], 0x00);
    }

    #[test]
    fn text_frame_empty_string_still_emits_two_byte_data() {
        // Edge case: empty string still produces a valid frame
        // with 2 bytes of data (encoding + null).
        let mut buf = Vec::new();
        emit_text_frame(&mut buf, b"TIT2", "", ENCODING_UTF8);
        assert_eq!(buf.len(), 12); // 10 header + 2 data
        assert_eq!(&buf[4..8], &libid3_size28(2));
        assert_eq!(buf[10], ENCODING_UTF8);
        assert_eq!(buf[11], 0x00);
    }

    // ============================================================
    //  TXXX frame
    // ============================================================

    #[test]
    fn txxx_performer_uppercase_description() {
        let mut buf = Vec::new();
        emit_txxx_performer(&mut buf, "ARTIST NAME");
        // Frame header
        assert_eq!(&buf[0..4], b"TXXX");
        // Data: [enc=0x03]["PERFORMER"][0x00]["ARTIST NAME"][0x00]
        assert_eq!(buf[10], ENCODING_UTF8);
        assert_eq!(&buf[11..20], b"PERFORMER", "description MUST be uppercase");
        assert_eq!(buf[20], 0x00, "description null terminator");
        assert_eq!(&buf[21..32], b"ARTIST NAME");
        assert_eq!(buf[32], 0x00, "value null terminator (libid3 non-spec quirk)");
        assert_eq!(buf.len(), 33); // 10 header + 23 data
    }

    #[test]
    fn txxx_frame_size_includes_trailing_null() {
        // Frame size must include the libid3 non-spec trailing null
        // on the value. For text="X": fr_size = 1 + 9 + 1 + 1 + 1 = 13.
        let mut buf = Vec::new();
        emit_txxx_performer(&mut buf, "X");
        let fr_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        // For size 13, libid3_size28 produces [0, 0, 0, 13].
        assert_eq!(libid3_size28(13), [0, 0, 0, 13]);
        // The frame size bytes spell out [0, 0, 0, 13] in the
        // header.
        assert_eq!(&buf[4..8], &[0, 0, 0, 13]);
    }

    // ============================================================
    //  Tag header
    // ============================================================

    #[test]
    fn tag_header_for_empty_metadata() {
        // Empty metadata → no frames → tag is just the 10-byte
        // header with size=0.
        let out = render_id3v24(&Id3Metadata::default());
        assert_eq!(out.len(), 10);
        assert_eq!(&out[0..3], b"ID3");
        assert_eq!(out[3], 0x04);
        assert_eq!(out[4], 0x00);
        assert_eq!(out[5], 0x00);
        assert_eq!(&out[6..10], &[0, 0, 0, 0]);
    }

    // ============================================================
    //  Canonical Solo Monk fixture (byte-exact gate)
    // ============================================================

    #[test]
    fn render_solo_monk_track_1_matches_canonical_footer() {
        // The empirical canonical fixture from pre-step audit.
        // SHA-256: 08c1e8fc66ae9de5eec34347a278fe34b6db2775dddeb29668259bbf59c090f5
        // Total bytes: 179.
        // Source: /tmp/sacd-compare/c-ref-dsf/SOLO MONK/01 - DINAH.dsf
        // bytes [104726620, 104726799).
        let meta = Id3Metadata {
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
        };
        let out = render_id3v24(&meta);
        assert_eq!(out.len(), 179, "expected canonical 179-byte footer");

        // Hash assertion against the empirical fixture.
        use crate::test_util::sha256_hex;
        assert_eq!(
            sha256_hex(&out),
            "08c1e8fc66ae9de5eec34347a278fe34b6db2775dddeb29668259bbf59c090f5",
            "byte-exact match against sacd_extract's libid3 output",
        );
    }

    // ============================================================
    //  Genre mapping
    // ============================================================

    #[test]
    fn sacd_genre_code_0_maps_to_other() {
        // Solo Monk's SACD genre code maps to "Other".
        assert_eq!(sacd_genre_to_id3_string(0), "Other");
    }

    #[test]
    fn sacd_genre_code_14_maps_to_jazz() {
        // SACD code 14 (Jazz) → ID3 index 8 → "Jazz".
        assert_eq!(sacd_genre_to_id3_string(14), "Jazz");
    }

    #[test]
    fn sacd_genre_out_of_range_maps_to_other() {
        assert_eq!(sacd_genre_to_id3_string(99), "Other");
        assert_eq!(sacd_genre_to_id3_string(255), "Other");
    }
}

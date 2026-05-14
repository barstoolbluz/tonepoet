//! Philips DSDIFF (.dff) writer. Port of sacd-extract's
//! `libsacd/dsdiff.c` for the DSD-uncompressed, single-track,
//! no-edit-master path.
//!
//! ## DSDIFF file layout (MVP, no footer)
//!
//! ```text
//!  FRM8 (16B):  "FRM8" | chunk_data_size (u64 BE) | "DSD " (form_type)
//!    FVER (16B):  "FVER" | chunk_data_size=4 | version=0x01050000 (u32 BE)
//!    PROP (12B header + 4B property_type):  "PROP" | chunk_data_size | "SND "
//!      FS   (16B):  "FS  " | chunk_data_size=4 | sample_rate (u32 BE)
//!      CHNL (14B + 4N):  "CHNL" | chunk_data_size=2+4N | channel_count (u16 BE) | channel_ids[N]
//!      CMPR (32B):  "CMPR" | chunk_data_size=20 | "DSD " | count=14 | "not compressed" | 0x00 pad
//!      LSCO (14B):  "LSCO" | chunk_data_size=2 | loudspeaker_config (u16 BE)
//!    DSD-data (12B header):  "DSD " | chunk_data_size = audio_data_size_padded
//!    [audio payload]
//!    [0x00 pad byte iff payload is odd]
//! ```
//!
//! Header size depends only on channel count: `header_size = 136 + 4N`.
//! Stereo = 144 bytes, 6-channel = 160 bytes.
//!
//! ## Deviations from the DSDIFF spec
//!
//! 1. The DSDIFF spec says pad bytes are not counted in
//!    `chunk_data_size`. The C reference applies `CEIL_ODD_NUMBER` to
//!    every chunk_data_size value, rounding up to even. This
//!    affects CMPR (19 → 20) and the DSD-data chunk (raw audio
//!    bytes → padded). We match the C reference for byte-exact
//!    compatibility with sacd-extract output.
//! 2. The C reference uses `sprintf` with the `"C%03i"` format for
//!    fallback channel IDs when channel_count is not in {2, 5, 6}.
//!    `sprintf` writes a NUL terminator that overflows past the
//!    4-byte slot into the next channel_id. We write the 4 ASCII
//!    bytes without a NUL — spec-compliant. This path is dead for
//!    real SACDs (channel_count always 2/5/6).
//!
//! ## Audio payload
//!
//! DSDIFF stores DSD MSB-first; SACD ISO sectors store DSD MSB-first.
//! Same encoding. SACD's interleaved-byte-cycle-across-channels frame
//! layout is exactly DSDIFF's "clustered frame". Audio bytes flow
//! through `write_frame` unchanged — no bit-reversal, no demux.

use std::io::{self, Seek, SeekFrom, Write};

/// DSD64 sample rate: 64 × 44.1 kHz. SACDs are always DSD64.
pub const SACD_SAMPLING_FREQUENCY: u32 = 2_822_400;

const DSDIFF_VERSION: u32 = 0x0105_0000;
const CHUNK_HEADER_SIZE: u64 = 12;

// 4-byte chunk IDs and other markers. Written directly as bytes —
// see module doc on `MAKE_MARKER` LE convention.
const FRM8: &[u8; 4] = b"FRM8";
const FVER: &[u8; 4] = b"FVER";
const PROP: &[u8; 4] = b"PROP";
const FS: &[u8; 4] = b"FS  ";
const CHNL: &[u8; 4] = b"CHNL";
const CMPR: &[u8; 4] = b"CMPR";
const LSCO: &[u8; 4] = b"LSCO";
// "DSD " appears as FRM8.form_type, DSD-data.chunk_id, and
// CMPR.compression_type. Note trailing space.
const DSD: &[u8; 4] = b"DSD ";
const SND: &[u8; 4] = b"SND ";

const SLFT: &[u8; 4] = b"SLFT";
const SRGT: &[u8; 4] = b"SRGT";
const MLFT: &[u8; 4] = b"MLFT";
const MRGT: &[u8; 4] = b"MRGT";
// Three trailing spaces for "C", two for "LS"/"RS", one for "LFE".
const C_ID: &[u8; 4] = b"C   ";
const LS_ID: &[u8; 4] = b"LS  ";
const RS_ID: &[u8; 4] = b"RS  ";
const LFE_ID: &[u8; 4] = b"LFE ";

const CMPR_NAME: &[u8] = b"not compressed";
const CMPR_NAME_LEN: u8 = 14;

const LS_CONFIG_2_CHNL: u16 = 0;
const LS_CONFIG_5_CHNL: u16 = 3;
const LS_CONFIG_6_CHNL: u16 = 4;
const LS_CONFIG_UNDEFINED: u16 = 65535;

/// Compute the total header byte count for a given channel count.
/// All sub-chunk sizes are intrinsically even, so the result is
/// always even. Closed form: `136 + 4N`.
pub const fn header_size(channel_count: u8) -> u64 {
    136 + 4 * channel_count as u64
}

/// Streaming DSDIFF writer. Audio bytes are passed through unchanged
/// (no bit-reversal, no demux); the writer just appends them and
/// maintains the FRM8/DSD-data size fields in the header.
///
/// Construction writes a placeholder header; `finish()` rewrites the
/// header with the final sizes after seeking back.
pub struct DffWriter<W: Write + Seek> {
    writer: W,
    channel_count: u8,
    sample_rate: u32,
    audio_data_size: u64,
    /// Optional footer bytes (DIIN + COMT + ID3 chunks) to append
    /// after audio. Set via [`Self::set_footer_bytes`]; `finish()`
    /// writes them and updates `FRM8.chunk_data_size` to include
    /// the footer length.
    footer_bytes: Option<Vec<u8>>,
}

impl<W: Write + Seek> DffWriter<W> {
    /// Create a new writer and emit the placeholder header (with
    /// audio_data_size=0). Caller must invoke `finish()` to write
    /// the final header values; dropping without finishing leaves a
    /// file with zeroed sizes.
    pub fn new(mut writer: W, channel_count: u8, sample_rate: u32) -> io::Result<Self> {
        if channel_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "channel_count must be > 0",
            ));
        }
        writer.seek(SeekFrom::Start(0))?;
        let header = serialize_header(channel_count, sample_rate, 0);
        writer.write_all(&header)?;
        Ok(Self {
            writer,
            channel_count,
            sample_rate,
            audio_data_size: 0,
            footer_bytes: None,
        })
    }

    /// Set the footer bytes (DIIN + COMT + ID3 chunks) to append
    /// after audio. Pass the output of
    /// [`crate::dff_footer::render_dff_footer`]. Must be called
    /// before `finish()`.
    ///
    /// When set, `finish()` writes the bytes after the audio's
    /// optional odd-pad byte and updates `FRM8.chunk_data_size` to
    /// include the footer length, matching sacd_extract's
    /// non-edit-master default output.
    pub fn set_footer_bytes(&mut self, bytes: Vec<u8>) {
        self.footer_bytes = Some(bytes);
    }

    /// Append raw DSD audio bytes. The byte stream is assumed to be
    /// already in clustered-frame layout (byte-interleaved across
    /// channels, MSB-first within each byte) — i.e., exactly what
    /// the SACD ISO yields per frame.
    pub fn write_frame(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data)?;
        self.audio_data_size += data.len() as u64;
        Ok(())
    }

    /// Finalize: pad audio to even length if needed, write optional
    /// footer, then seek back and rewrite the FRM8 header with the
    /// final sizes (including footer length).
    pub fn finish(mut self) -> io::Result<()> {
        if self.audio_data_size % 2 == 1 {
            self.writer.write_all(&[0u8])?;
            self.audio_data_size += 1;
        }
        // Write footer after audio + pad. Footer is expected to be
        // even-byte-aligned (each chunk pads internally per
        // CALC_CHUNK_SIZE); we don't add a global pad after.
        let footer_size = self.footer_bytes.as_ref().map_or(0, |f| f.len() as u64);
        if let Some(ref footer) = self.footer_bytes {
            self.writer.write_all(footer)?;
        }
        self.writer.seek(SeekFrom::Start(0))?;
        let header = serialize_header_with_footer(
            self.channel_count,
            self.sample_rate,
            self.audio_data_size,
            footer_size,
        );
        self.writer.write_all(&header)?;
        Ok(())
    }
}

/// Build the complete header byte sequence for the given parameters.
/// `audio_data_size` must already reflect any pad byte (i.e. even).
/// The output length equals `header_size(channel_count)`.
/// Convenience wrapper for the no-footer case.
pub fn serialize_header(
    channel_count: u8,
    sample_rate: u32,
    audio_data_size: u64,
) -> Vec<u8> {
    serialize_header_with_footer(channel_count, sample_rate, audio_data_size, 0)
}

/// Build the complete header with explicit `footer_size`. Sets
/// `FRM8.chunk_data_size` to include the footer length per
/// dsdiff.c's `form_dsd_chunk->chunk_data_size =
/// CALC_CHUNK_SIZE(header_size + audio_data_size + footer_size - 12)`.
pub fn serialize_header_with_footer(
    channel_count: u8,
    sample_rate: u32,
    audio_data_size: u64,
    footer_size: u64,
) -> Vec<u8> {
    let total_header = header_size(channel_count);
    let mut buf = Vec::with_capacity(total_header as usize);

    // FRM8 (16 bytes): chunk_id, chunk_data_size, form_type.
    // chunk_data_size = header_size + audio_data_size + footer_size - 12.
    let frm8_size =
        total_header + audio_data_size + footer_size - CHUNK_HEADER_SIZE;
    buf.extend_from_slice(FRM8);
    buf.extend_from_slice(&frm8_size.to_be_bytes());
    buf.extend_from_slice(DSD);

    // FVER (16 bytes): chunk_id, chunk_data_size=4, version.
    buf.extend_from_slice(FVER);
    buf.extend_from_slice(&4u64.to_be_bytes());
    buf.extend_from_slice(&DSDIFF_VERSION.to_be_bytes());

    // PROP header (12 bytes) + property_type (4 bytes). The
    // chunk_data_size for PROP is computed below from the inner
    // chunk sizes.
    let chnl_payload = 2 + 4 * channel_count as u64;
    let chnl_total = CHUNK_HEADER_SIZE + chnl_payload;
    let prop_data_size = 4 // property_type
        + 16 // FS
        + chnl_total
        + 32 // CMPR (header 12 + payload 19 rounded to 20, total 32)
        + 14; // LSCO
    buf.extend_from_slice(PROP);
    buf.extend_from_slice(&prop_data_size.to_be_bytes());
    buf.extend_from_slice(SND);

    // FS (16 bytes): chunk_id, chunk_data_size=4, sample_rate.
    buf.extend_from_slice(FS);
    buf.extend_from_slice(&4u64.to_be_bytes());
    buf.extend_from_slice(&sample_rate.to_be_bytes());

    // CHNL (14 + 4N bytes).
    buf.extend_from_slice(CHNL);
    buf.extend_from_slice(&chnl_payload.to_be_bytes());
    buf.extend_from_slice(&(channel_count as u16).to_be_bytes());
    write_channel_ids(&mut buf, channel_count);

    // CMPR (32 bytes total): chunk_data_size=20 (CALC_CHUNK_SIZE
    // rounded up from 19 raw bytes — includes the pad byte per the
    // C reference's behavior).
    buf.extend_from_slice(CMPR);
    buf.extend_from_slice(&20u64.to_be_bytes());
    buf.extend_from_slice(DSD); // compression_type
    buf.push(CMPR_NAME_LEN); // count = 14
    buf.extend_from_slice(CMPR_NAME); // "not compressed"
    buf.push(0); // pad byte

    // LSCO (14 bytes): chunk_id, chunk_data_size=2, loudspeaker_config.
    buf.extend_from_slice(LSCO);
    buf.extend_from_slice(&2u64.to_be_bytes());
    buf.extend_from_slice(&loudspeaker_config(channel_count).to_be_bytes());

    // DSD-data chunk header (12 bytes). Body is the audio payload,
    // written separately via write_frame. chunk_data_size already
    // reflects the pad byte if any.
    buf.extend_from_slice(DSD);
    buf.extend_from_slice(&audio_data_size.to_be_bytes());

    debug_assert_eq!(buf.len() as u64, total_header);
    buf
}

fn write_channel_ids(buf: &mut Vec<u8>, channel_count: u8) {
    match channel_count {
        2 => {
            buf.extend_from_slice(SLFT);
            buf.extend_from_slice(SRGT);
        }
        5 => {
            buf.extend_from_slice(MLFT);
            buf.extend_from_slice(MRGT);
            buf.extend_from_slice(C_ID);
            buf.extend_from_slice(LS_ID);
            buf.extend_from_slice(RS_ID);
        }
        6 => {
            buf.extend_from_slice(MLFT);
            buf.extend_from_slice(MRGT);
            buf.extend_from_slice(C_ID);
            buf.extend_from_slice(LFE_ID);
            buf.extend_from_slice(LS_ID);
            buf.extend_from_slice(RS_ID);
        }
        n => {
            // Spec-compliant fallback: 4 ASCII bytes "C000", "C001",
            // ... — no NUL terminator. The C reference's sprintf
            // overflows by 1 byte; we deliberately diverge.
            for i in 0..n {
                let id = [b'C', b'0' + (i / 100), b'0' + ((i / 10) % 10), b'0' + (i % 10)];
                buf.extend_from_slice(&id);
            }
        }
    }
}

fn loudspeaker_config(channel_count: u8) -> u16 {
    match channel_count {
        2 => LS_CONFIG_2_CHNL,
        5 => LS_CONFIG_5_CHNL,
        6 => LS_CONFIG_6_CHNL,
        _ => LS_CONFIG_UNDEFINED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run(channel_count: u8, payload: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut w = DffWriter::new(&mut cursor, channel_count, SACD_SAMPLING_FREQUENCY)
                .unwrap();
            if !payload.is_empty() {
                w.write_frame(payload).unwrap();
            }
            w.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn read_u16_be(buf: &[u8], off: usize) -> u16 {
        u16::from_be_bytes(buf[off..off + 2].try_into().unwrap())
    }
    fn read_u32_be(buf: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
    }
    fn read_u64_be(buf: &[u8], off: usize) -> u64 {
        u64::from_be_bytes(buf[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn header_size_formula() {
        assert_eq!(header_size(2), 144);
        assert_eq!(header_size(3), 148);
        assert_eq!(header_size(5), 156);
        assert_eq!(header_size(6), 160);
    }

    #[test]
    fn header_byte_layout_for_stereo_dsd64() {
        // Empty audio so DSD.chunk_data_size = 0 and FRM8 size is
        // just the header contribution.
        let out = run(2, &[]);
        assert_eq!(out.len(), 144);

        // FRM8
        assert_eq!(&out[0..4], FRM8);
        assert_eq!(read_u64_be(&out, 4), 144 - 12); // 132
        assert_eq!(&out[12..16], DSD); // form_type

        // FVER
        assert_eq!(&out[16..20], FVER);
        assert_eq!(read_u64_be(&out, 20), 4);
        assert_eq!(read_u32_be(&out, 28), 0x0105_0000);

        // PROP
        assert_eq!(&out[32..36], PROP);
        assert_eq!(read_u64_be(&out, 36), 88); // 4 + 16 + 22 + 32 + 14
        assert_eq!(&out[44..48], SND);

        // FS
        assert_eq!(&out[48..52], FS);
        assert_eq!(read_u64_be(&out, 52), 4);
        assert_eq!(read_u32_be(&out, 60), 2_822_400);

        // CHNL
        assert_eq!(&out[64..68], CHNL);
        assert_eq!(read_u64_be(&out, 68), 2 + 4 * 2);
        assert_eq!(read_u16_be(&out, 76), 2);
        assert_eq!(&out[78..82], SLFT);
        assert_eq!(&out[82..86], SRGT);

        // CMPR
        assert_eq!(&out[86..90], CMPR);
        assert_eq!(read_u64_be(&out, 90), 20); // 19 raw → 20 padded
        assert_eq!(&out[98..102], DSD); // compression_type
        assert_eq!(out[102], 14); // count
        assert_eq!(&out[103..117], b"not compressed");
        assert_eq!(out[117], 0); // pad byte

        // LSCO
        assert_eq!(&out[118..122], LSCO);
        assert_eq!(read_u64_be(&out, 122), 2);
        assert_eq!(read_u16_be(&out, 130), LS_CONFIG_2_CHNL);

        // DSD-data header
        assert_eq!(&out[132..136], DSD);
        assert_eq!(read_u64_be(&out, 136), 0);
    }

    #[test]
    fn header_byte_layout_for_six_channel() {
        let out = run(6, &[]);
        assert_eq!(out.len(), 160);

        // CHNL: 14 + 24 bytes; channel_ids = MLFT, MRGT, C, LFE, LS, RS.
        assert_eq!(&out[64..68], CHNL);
        assert_eq!(read_u64_be(&out, 68), 2 + 4 * 6); // 26
        assert_eq!(read_u16_be(&out, 76), 6);
        assert_eq!(&out[78..82], MLFT);
        assert_eq!(&out[82..86], MRGT);
        assert_eq!(&out[86..90], C_ID);
        assert_eq!(&out[90..94], LFE_ID);
        assert_eq!(&out[94..98], LS_ID);
        assert_eq!(&out[98..102], RS_ID);

        // CMPR shifts: starts at 64 + (14 + 24) = 102.
        assert_eq!(&out[102..106], CMPR);
        assert_eq!(read_u64_be(&out, 106), 20);
        assert_eq!(out[133], 0); // CMPR pad byte at cmpr_start + 31

        // LSCO at 102 + 32 = 134.
        assert_eq!(&out[134..138], LSCO);
        assert_eq!(read_u16_be(&out, 146), LS_CONFIG_6_CHNL);

        // PROP.chunk_data_size for 6ch: 4 + 16 + 38 + 32 + 14 = 104.
        assert_eq!(read_u64_be(&out, 36), 104);

        // DSD-data header at 134 + 14 = 148; audio starts at 160.
        assert_eq!(&out[148..152], DSD);
        assert_eq!(read_u64_be(&out, 152), 0);
    }

    #[test]
    fn header_byte_layout_for_five_channel() {
        // Mirror of header_byte_layout_for_six_channel for the
        // 5-channel ITU-R BS.775 layout (L, R, C, Ls, Rs — no LFE).
        // Real SACDs with this configuration exist (mostly older
        // surround mixes that don't use the LFE channel).
        let out = run(5, &[]);
        // Header size: 136 + 4 * 5 = 156 bytes.
        assert_eq!(out.len(), 156);

        // CHNL: 14 (CHNL header+count) + 20 (5 channel_ids × 4) = 34
        // on disk; chunk_data_size = 2 + 4*5 = 22.
        assert_eq!(&out[64..68], CHNL);
        assert_eq!(read_u64_be(&out, 68), 2 + 4 * 5);
        assert_eq!(read_u16_be(&out, 76), 5);
        // Channel IDs: MLFT, MRGT, C, LS, RS (no LFE between C and LS).
        assert_eq!(&out[78..82], MLFT);
        assert_eq!(&out[82..86], MRGT);
        assert_eq!(&out[86..90], C_ID);
        assert_eq!(&out[90..94], LS_ID);
        assert_eq!(&out[94..98], RS_ID);

        // CMPR starts at 64 + (14 + 20) = 98.
        assert_eq!(&out[98..102], CMPR);
        assert_eq!(read_u64_be(&out, 102), 20);
        // CMPR pad byte at cmpr_start + 31 = 129.
        assert_eq!(out[129], 0);

        // LSCO at 98 + 32 = 130.
        assert_eq!(&out[130..134], LSCO);
        assert_eq!(read_u16_be(&out, 142), LS_CONFIG_5_CHNL);

        // PROP.chunk_data_size for 5ch: 4 + 16 + 34 + 32 + 14 = 100.
        assert_eq!(read_u64_be(&out, 36), 100);

        // DSD-data header at 130 + 14 = 144; audio starts at 156.
        assert_eq!(&out[144..148], DSD);
        assert_eq!(read_u64_be(&out, 148), 0);
    }

    #[test]
    fn audio_passes_through_unchanged_no_bit_reversal() {
        // A pattern that would be detectable if accidentally
        // bit-reversed (0xAA → 0x55) or demuxed.
        let payload: Vec<u8> = (0..200).map(|i| (i & 0xFF) as u8).collect();
        let out = run(2, &payload);
        let header = 144;
        assert_eq!(out.len(), header + payload.len());
        assert_eq!(&out[header..header + payload.len()], &payload[..]);
    }

    #[test]
    fn odd_audio_emits_pad_byte_and_padded_size() {
        // 7 bytes of audio → 1 pad byte → DSD.chunk_data_size = 8.
        let payload: Vec<u8> = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        let out = run(2, &payload);
        let header = 144usize;
        assert_eq!(out.len(), header + 7 + 1); // padded
        // Real audio bytes preserved.
        assert_eq!(&out[header..header + 7], &payload[..]);
        // Pad byte is 0x00.
        assert_eq!(out[header + 7], 0);
        // DSD-data.chunk_data_size = 8 (padded), not 7 (raw).
        assert_eq!(read_u64_be(&out, 136), 8);
        // FRM8.chunk_data_size = (144 + 8) - 12 = 140.
        assert_eq!(read_u64_be(&out, 4), 140);
    }

    #[test]
    fn cmpr_chunk_has_dsd_not_compressed_name_with_pad() {
        let out = run(2, &[]);
        // For stereo: CMPR starts at offset 86.
        let cmpr_start = 86;
        assert_eq!(&out[cmpr_start..cmpr_start + 4], CMPR);
        assert_eq!(read_u64_be(&out, cmpr_start + 4), 20);
        assert_eq!(&out[cmpr_start + 12..cmpr_start + 16], DSD); // compression_type
        assert_eq!(out[cmpr_start + 16], 14); // count
        assert_eq!(
            &out[cmpr_start + 17..cmpr_start + 31],
            b"not compressed",
        );
        // Pad byte at cmpr_start + 31.
        assert_eq!(out[cmpr_start + 31], 0);
    }

    #[test]
    fn unknown_channel_count_uses_c_numbered_ids() {
        let out = run(3, &[]);
        assert_eq!(out.len(), 148);

        // CHNL channel IDs at offsets 78, 82, 86.
        assert_eq!(&out[78..82], b"C000");
        assert_eq!(&out[82..86], b"C001");
        assert_eq!(&out[86..90], b"C002");
        // For N=3: CHNL ends at 64+(14+12)=90. CMPR at 90..122.
        // LSCO at 122..136, with loudspeaker_config u16 at 134.
        assert_eq!(&out[122..126], LSCO);
        assert_eq!(read_u16_be(&out, 134), LS_CONFIG_UNDEFINED);
    }

    #[test]
    fn empty_write_produces_header_only_file() {
        let out = run(2, &[]);
        assert_eq!(out.len(), 144);
        // DSD-data.chunk_data_size = 0.
        assert_eq!(read_u64_be(&out, 136), 0);
        // FRM8.chunk_data_size = 132.
        assert_eq!(read_u64_be(&out, 4), 132);
    }

    #[test]
    fn frm8_chunk_data_size_tracks_audio_growth() {
        // Even audio: no pad. FRM8 = 144 + A - 12.
        let payload: Vec<u8> = vec![0xAA; 100];
        let out = run(2, &payload);
        assert_eq!(out.len(), 244);
        assert_eq!(read_u64_be(&out, 4), 144 + 100 - 12);
        assert_eq!(read_u64_be(&out, 136), 100);
        // 6-channel with even audio.
        let out6 = run(6, &payload);
        assert_eq!(out6.len(), 160 + 100);
        assert_eq!(read_u64_be(&out6, 4), 160 + 100 - 12);
    }

    #[test]
    fn consecutive_write_frames_accumulate() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut w =
                DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            w.write_frame(&[0x01, 0x02, 0x03, 0x04]).unwrap();
            w.write_frame(&[0x05, 0x06]).unwrap();
            w.write_frame(&[0x07, 0x08, 0x09, 0x0A]).unwrap();
            w.finish().unwrap();
        }
        let out = cursor.into_inner();
        assert_eq!(out.len(), 144 + 10);
        assert_eq!(&out[144..154], &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A]);
        assert_eq!(read_u64_be(&out, 136), 10);
    }
}

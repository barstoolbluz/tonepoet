//! Kitty upload compression for the pinned ratatui-image 1.0.5 transport.
//!
//! Ratatui-image emits a one-shot Kitty graphics APC at the image-origin
//! buffer cell, followed by Unicode image placeholders. Only the APC upload
//! is changed here; the crate retains its image IDs, placement, and cache.
//! Unrecognized layouts and incompressible images use the original upload.

use std::io::Write;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use flate2::{write::ZlibEncoder, Compression};

const APC_START: &str = "\x1b_G";
const APC_END: &str = "\x1b\\";
const KITTY_CHUNK_BYTES: usize = 4096; // Base64 chunks are multiples of 4.
const MIN_RGB_BYTES: usize = 16 * 1024;
// Refuse work on exceptionally large buffer cells. The existing uncompressed
// path remains available, with no additional transient multi-hundred-MiB copy.
const MAX_RGB_BYTES: usize = 128 * 1024 * 1024;

fn parameter<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    header.split(',').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    })
}

/// Return a compressed replacement for a first-time Kitty upload, or `None`
/// to keep the original symbol unchanged. Non-Kitty redraws take the fast path.
/// The RFC 1950 zlib stream is compressed *before* Base64 encoding; `o=z` is
/// attached to the first APC only. Continuations retain the opening image ID
/// and quiet flag alongside Kitty's `m` flag.
pub(super) fn compress_kitty_upload(symbol: &str) -> Option<String> {
    let start = symbol.find(APC_START)?;
    let first = &symbol[start + APC_START.len()..];
    let first_end = first.find(';')?;
    let first_header = &first[..first_end];
    let format: usize = parameter(first_header, "f")?.parse().ok()?;
    let channels = match format { 24 => 3_usize, 32 => 4_usize, _ => return None };
    let width: usize = parameter(first_header, "s")?.parse().ok()?;
    let height: usize = parameter(first_header, "v")?.parse().ok()?;
    let expected = width.checked_mul(height)?.checked_mul(channels)?;
    if !(MIN_RGB_BYTES..=MAX_RGB_BYTES).contains(&expected) { return None; }
    if parameter(first_header, "o").is_some() { return None; } // Already transformed.

    // Decode each original 4 KiB chunk directly into the compressor, rather
    // than materializing another full RGB image beside ratatui-image's buffer.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    let mut offset = start;
    let mut received = 0_usize;
    loop {
        let rest = symbol.get(offset..)?.strip_prefix(APC_START)?;
        let separator = rest.find(';')?;
        let header = &rest[..separator];
        // ratatui-image 1.0.5 repeats q=2 and the image ID on *every*
        // continuation APC, not just m=. Permit those identity/quiet fields
        // only when they agree with the opening command; never swallow a
        // separate transmission, placement, or deletion into this stream.
        if offset != start {
            let mut seen_m = false;
            let mut seen_q = false;
            let mut seen_i = false;
            for field in header.split(',') {
                let (key, value) = field.split_once('=')?;
                match key {
                    "m" if !seen_m => seen_m = true,
                    "q" if !seen_q && parameter(first_header, "q") == Some(value) => {
                        seen_q = true;
                    }
                    "i" if !seen_i && parameter(first_header, "i") == Some(value) => {
                        seen_i = true;
                    }
                    _ => return None,
                }
            }
            if !seen_m { return None; }
        }
        let body = &rest[separator + 1..];
        let end = body.find(APC_END)?;
        let chunk = &body[..end];
        let decoded = STANDARD.decode(chunk).ok()?;
        received = received.checked_add(decoded.len())?;
        if received > expected { return None; }
        encoder.write_all(&decoded).ok()?;
        offset += APC_START.len() + separator + 1 + end + APC_END.len();
        let more = match parameter(header, "m") { Some("1") => true,
            Some("0") | None => false, _ => return None };
        if !more { break; }
    }
    if received != expected { return None; }
    let compressed = encoder.finish().ok()?;
    // Compression work already happened; do not send more bytes if the saving
    // is marginal (for example, noise-like scans). Keep the original APC then.
    if compressed.len().saturating_mul(100) >= received.saturating_mul(95) {
        return None;
    }
    let encoded = STANDARD.encode(compressed);
    let chunks: Vec<&str> = encoded.as_bytes().chunks(KITTY_CHUNK_BYTES)
        .map(std::str::from_utf8).collect::<Result<_, _>>().ok()?;
    if chunks.is_empty() { return None; }

    let first_parameters = first_header.split(',')
        .filter(|part| !part.starts_with("m=") && !part.starts_with("o="))
        .collect::<Vec<_>>().join(",");
    let mut output = String::with_capacity(symbol.len().min(4096)
        .saturating_add(encoded.len()).saturating_add(chunks.len() * 20));
    output.push_str(&symbol[..start]);
    for (index, chunk) in chunks.iter().enumerate() {
        output.push_str(APC_START);
        if index == 0 {
            output.push_str(&first_parameters);
            output.push_str(",o=z,");
        } else {
            // Keep the original transport's quiet and identity metadata on
            // continuation APCs. Omitting q=2 can elicit one terminal reply
            // per chunk on a slow SSH link.
            for key in ["q", "i"] {
                if let Some(value) = parameter(first_header, key) {
                    output.push_str(key);
                    output.push('=');
                    output.push_str(value);
                    output.push(',');
                }
            }
        }
        let last = index + 1 == chunks.len();
        output.push_str(if last { "m=0;" } else { "m=1;" });
        output.push_str(chunk);
        output.push_str(APC_END);
    }
    output.push_str(&symbol[offset..]); // Preserve original Kitty placeholders.
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    fn original_kitty_upload(rgb: &[u8], w: usize, h: usize) -> String {
        let encoded = STANDARD.encode(rgb);
        let chunks = encoded.as_bytes().chunks(KITTY_CHUNK_BYTES).collect::<Vec<_>>();
        let mut symbol = String::from("\x1b[0m");
        for (index, chunk) in chunks.iter().enumerate() {
            let more = if index + 1 == chunks.len() { 0 } else { 1 };
            if index == 0 {
                symbol.push_str(&format!("\x1b_Ga=T,f=24,s={w},v={h},i=42,m={more};"));
            } else {
                symbol.push_str(&format!("\x1b_Gm={more};"));
            }
            symbol.push_str(std::str::from_utf8(chunk).unwrap());
            symbol.push_str(APC_END);
        }
        symbol.push_str("\u{10eeee}placeholder");
        symbol
    }

    // This is the continuation-header layout emitted by ratatui-image 1.0.5
    // transmit_virtual. The simpler fixture above does not exercise it.
    fn ratatui_105_upload(rgb: &[u8], w: usize, h: usize) -> String {
        original_kitty_upload(rgb, w, h)
            .replace("a=T,f=24,", "a=T,q=2,f=24,")
            .replace("\x1b_Gm=", "\x1b_Gq=2,i=42,m=")
    }

    #[test]
    fn ratatui_105_continuations_compress_and_reject_mismatched_identity() {
        let rgb = vec![91_u8; 320 * 180 * 3];
        let original = ratatui_105_upload(&rgb, 320, 180);
        assert!(original.matches("\x1b_Gq=2,i=42,m=").count() > 1);
        let compressed = compress_kitty_upload(&original).expect("real upload must compress");
        assert!(compressed.contains("i=42,o=z,m="));
        assert_eq!(compressed_rgb(&compressed), rgb);
        assert!(compressed.ends_with("\u{10eeee}placeholder"));
        // A compressible but nontrivial pattern still needs multiple APCs.
        // Every compressed continuation must retain quiet/id metadata.
        let mut seed = 0x123456789abcdef0_u64;
        let tile = (0..(16 * 1024)).map(|_| {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            (seed.wrapping_mul(0x2545f4914f6cdd1d) >> 56) as u8
        }).collect::<Vec<_>>();
        let mut patterned = Vec::with_capacity(512 * 512 * 3);
        for i in 0..(512 * 512 * 3) { patterned.push(tile[i % tile.len()]); }
        let original_chunks = ratatui_105_upload(&patterned, 512, 512);
        let compressed_chunks = compress_kitty_upload(&original_chunks).unwrap();
        assert!(compressed_chunks.matches("\x1b_G").count() > 1);
        assert!(compressed_chunks.contains("\x1b_Gq=2,i=42,m="));
        assert_eq!(compressed_rgb(&compressed_chunks), patterned);
        // A different image ID or quiet flag in a continuation must fall
        // back untouched; it is not the same image's payload.
        assert!(compress_kitty_upload(&original.replacen(
            "\x1b_Gq=2,i=42,m=", "\x1b_Gq=2,i=43,m=", 1)).is_none());
        assert!(compress_kitty_upload(&original.replacen(
            "\x1b_Gq=2,i=42,m=", "\x1b_Gq=1,i=42,m=", 1)).is_none());
        assert!(compress_kitty_upload(&original.replacen(
            "\x1b_Gq=2,i=42,m=", "\x1b_Ga=d,q=2,i=42,m=", 1)).is_none());
    }

    fn compressed_rgb(symbol: &str) -> Vec<u8> {
        let mut encoded = String::new();
        let mut rest = symbol;
        while let Some(pos) = rest.find(APC_START) {
            let command = &rest[pos + APC_START.len()..];
            let (_, payload) = command.split_once(';').unwrap();
            let (chunk, later) = payload.split_once(APC_END).unwrap();
            encoded.push_str(chunk);
            rest = later;
        }
        let payload = STANDARD.decode(encoded).unwrap();
        let mut decoder = ZlibDecoder::new(payload.as_slice());
        let mut rgb = Vec::new();
        decoder.read_to_end(&mut rgb).unwrap();
        rgb
    }

    #[test]
    fn kitty_zlib_roundtrips_image_and_preserves_identity_and_placeholders() {
        let rgb = vec![17_u8; 320 * 180 * 3];
        let original = original_kitty_upload(&rgb, 320, 180);
        let compressed = compress_kitty_upload(&original).unwrap();
        assert!(compressed.starts_with("\x1b[0m\x1b_Ga=T,f=24,s=320,v=180,i=42,o=z,m="));
        assert!(compressed.ends_with("\u{10eeee}placeholder"));
        assert!(!original.contains("o=z"));
        assert_eq!(compressed_rgb(&compressed), rgb);
        assert!(compressed.len() < original.len() / 10,
            "repeated pixels must save most of the RGB upload");
    }

    #[test]
    fn kitty_zlib_falls_back_for_unknown_and_incompressible_uploads() {
        assert!(compress_kitty_upload("plain terminal text").is_none());
        assert!(compress_kitty_upload("\x1b_Ga=q,f=100;AAAA\x1b\\").is_none());
        // High-entropy RGB should not pay the wire cost of zlib framing.
        let mut seed = 0x9e3779b97f4a7c15_u64;
        let rgb = (0..(300 * 200 * 3)).map(|_| {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            (seed.wrapping_mul(0x2545f4914f6cdd1d) >> 56) as u8
        }).collect::<Vec<_>>();
        let original = original_kitty_upload(&rgb, 300, 200);
        assert!(compress_kitty_upload(&original).is_none());
    }
}

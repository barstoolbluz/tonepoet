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

// tmux requires DCS passthrough *around each Kitty APC*, not around the whole
// image cell (which also contains ordinary cursor control and Unicode image
// placeholders). The latter must reach tmux itself to remain pane-local.
const TMUX_DCS: &str = "\x1bPtmux;";

/// Wrap raw Kitty upload commands for the tmux server while retaining their
/// normal Unicode placeholder cells. Every command carries q=2, including
/// continuations; neither tmux nor the terminal should reply to the TUI.
/// An unrecognized command is a hard failure, not a raw passthrough attempt.
pub(super) fn wrap_tmux_kitty_apcs(symbol: &str) -> Result<Option<String>, ()> {
    if !symbol.contains(APC_START) && !symbol.contains(TMUX_DCS) { return Ok(None); }
    let bytes = symbol.as_bytes();
    let mut output = String::with_capacity(symbol.len().saturating_add(128));
    let mut pos = 0;
    let mut changed = false;
    while pos < bytes.len() {
        let dcs = symbol[pos..].find(TMUX_DCS).map(|offset| pos + offset);
        let apc = symbol[pos..].find(APC_START).map(|offset| pos + offset);
        // Already-wrapped commands are left unchanged. Scan escape-doubled
        // tmux bytes rather than treating the inner APC's ST as a DCS end.
        if let Some(start) = dcs.filter(|start| apc.is_none_or(|apc| *start < apc)) {
            output.push_str(&symbol[pos..start]);
            let end = tmux_dcs_end(symbol, start).ok_or(())?;
            if !quiet_tmux_kitty_dcs(symbol, start, end) { return Err(()); }
            output.push_str(&symbol[start..end]);
            pos = end;
            continue;
        }
        let Some(start) = apc else {
            output.push_str(&symbol[pos..]);
            break;
        };
        output.push_str(&symbol[pos..start]);
        let content = &symbol[start + APC_START.len()..];
        let separator = content.find(';').ok_or(())?;
        let header = &content[..separator];
        if header.is_empty() || header.len() > 256 || header.bytes().any(|byte|
            !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'=' | b',' | b'-'))
        {
            return Err(());
        }
        // Only stream uploads (not capability queries, file paths, or shared
        // memory) are valid here. These are the actions emitted by the pinned
        // ratatui-image Kitty upload path.
        if !safe_tmux_kitty_upload_header(header) { return Err(()); }
        let body = &content[separator + 1..];
        let end = body.find(APC_END).ok_or(())?;
        let payload = &body[..end];
        if payload.len() > 64 * 1024 || !payload.bytes().all(|byte|
            matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'='))
        {
            return Err(());
        }
        let mut quiet = false;
        let mut safe_header = String::with_capacity(header.len() + 4);
        for part in header.split(',') {
            if part.is_empty() { return Err(()); }
            if part.starts_with("q=") {
                if quiet { return Err(()); }
                quiet = true;
                if !safe_header.is_empty() { safe_header.push(','); }
                safe_header.push_str("q=2");
            } else {
                if !safe_header.is_empty() { safe_header.push(','); }
                safe_header.push_str(part);
            }
        }
        if !quiet { safe_header.push_str(",q=2"); }
        let mut raw_apc = String::with_capacity(safe_header.len() + payload.len() + 8);
        raw_apc.push_str(APC_START);
        raw_apc.push_str(&safe_header);
        raw_apc.push(';');
        raw_apc.push_str(payload);
        raw_apc.push_str(APC_END);
        output.push_str(TMUX_DCS);
        for character in raw_apc.chars() {
            if character == '\x1b' { output.push('\x1b'); }
            output.push(character);
        }
        output.push_str(APC_END);
        pos = start + APC_START.len() + separator + 1 + end + APC_END.len();
        changed = true;
    }
    Ok(changed.then_some(output))
}

fn tmux_dcs_end(symbol: &str, start: usize) -> Option<usize> {
    let bytes = symbol.as_bytes();
    let mut pos = start + TMUX_DCS.len();
    while pos < bytes.len() {
        if bytes[pos] == 0x1b {
            match *bytes.get(pos + 1)? {
                0x1b => { pos += 2; continue; }
                b'\\' => return Some(pos + 2),
                _ => return None,
            }
        }
        pos += 1;
    }
    None
}

fn safe_tmux_kitty_upload_header(header: &str) -> bool {
    if parameter(header, "t").is_some_and(|transport| transport != "d") {
        return false; // file/shm paths are not available over SSH
    }
    match parameter(header, "a") {
        Some("T" | "t") => true,
        None => parameter(header, "m").is_some(), // continuation chunk
        _ => false,
    }
}

/// A pinned image crate may itself wrap Kitty commands when TMUX is set.
/// Never trust that opaque DCS blindly: validate that it holds one quiet,
/// direct stream upload, without a reply-producing capability query.
fn quiet_tmux_kitty_dcs(symbol: &str, start: usize, end: usize) -> bool {
    let bytes = symbol.as_bytes();
    let interior = &bytes[start + TMUX_DCS.len()..end - APC_END.len()];
    if interior.len() > 64 * 1024 + 512 { return false; }
    let mut raw = Vec::with_capacity(interior.len());
    let mut pos = 0;
    while pos < interior.len() {
        if interior[pos] == 0x1b {
            if interior.get(pos + 1) != Some(&0x1b) { return false; }
            pos += 2;
            raw.push(0x1b);
        } else {
            raw.push(interior[pos]);
            pos += 1;
        }
    }
    let Ok(apc) = std::str::from_utf8(&raw) else { return false; };
    let Some(command) = apc.strip_prefix(APC_START).and_then(|part| part.strip_suffix(APC_END))
    else { return false; };
    let Some((header, payload)) = command.split_once(';') else { return false; };
    safe_tmux_kitty_upload_header(header)
        && header.split(',').filter(|part| part.starts_with("q=")).eq(["q=2"])
        && payload.len() <= 64 * 1024
        && payload.bytes().all(|byte| matches!(byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'='))
}

/// Only a fresh graphics upload needs a forwarding check. Ordinary redraws
/// may contain Unicode placeholders but no Kitty APC; do not spawn tmux for
/// those frames. Check before wrapping to avoid committing a one-shot upload
/// to a hidden pane with allow-passthrough=on.
pub(super) fn buffer_has_kitty_upload(buffer: &ratatui::buffer::Buffer) -> bool {
    buffer.content.iter().any(|cell|
        cell.symbol().contains(APC_START) || cell.symbol().contains(TMUX_DCS))
}

/// Called only when tmux capability was established. Every protocol-producing
/// widget, including Browse, metadata previews, and file-picker images, uses
/// this one final-buffer transport before ratatui sends output to the pane.
pub(super) fn wrap_tmux_kitty_buffer(buffer: &mut ratatui::buffer::Buffer) -> Result<(), ()> {
    for cell in &mut buffer.content {
        match wrap_tmux_kitty_apcs(cell.symbol()) {
            Ok(Some(wrapped)) => { cell.set_symbol(&wrapped); },
            Ok(None) => {},
            Err(()) => return Err(()),
        };
    }
    Ok(())
}


// Sixel is already supported by the pinned image crate. It may emit
// tmux-wrapped SIXEL (when tmux is detected), or a bare SIXEL command.
// Never forward a bare DCS directly to the outer emulator: wrap just that
// command, leaving cursor positioning / clearing visible to tmux.
const SIXEL_DCS: &str = "\x1bP";

fn sixel_header(s: &str) -> bool {
    // DCS P[0-9;]*q is the Sixel introducer; other DCS commands are not
    // image data. The original encoder controls all following payload bytes.
    let n = s.bytes().take(32)
        .take_while(|byte| matches!(*byte, b'0'..=b'9' | b';')).count();
    n < 32 && s.as_bytes().get(n) == Some(&b'q')
}

pub(super) fn buffer_has_sixel_upload(buffer: &ratatui::buffer::Buffer) -> bool {
    buffer.content.iter().any(|cell| cell.symbol().contains(SIXEL_DCS))
}

/// A Sixel payload is repeated on ordinary widget redraws. Identical pixels
/// at the same origin may use an already-fresh forwarding check; a changed
/// payload/origin requires a new worker inspection. No image data is copied.
pub(super) fn sixel_upload_fingerprint(buffer: &ratatui::buffer::Buffer) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut seen = false;
    for (index, cell) in buffer.content.iter().enumerate() {
        if cell.symbol().contains(SIXEL_DCS) {
            index.hash(&mut hasher);
            cell.symbol().hash(&mut hasher);
            seen = true;
        }
    }
    seen.then(|| hasher.finish())
}

/// Never send an unverified native image or Kitty placeholder to tmux. This
/// affects only the transient frame, not the protocol picker/cache: once the
/// worker replies, a single cache rebuild can perform the real upload.
pub(super) fn suppress_unverified_tmux_graphics(buffer: &mut ratatui::buffer::Buffer) {
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if symbol.contains(SIXEL_DCS) || symbol.contains(APC_START)
            || symbol.contains(TMUX_DCS) || symbol.contains('\u{10eeee}')
        {
            cell.set_symbol(" ");
        }
    }
}

/// Retain the pinned Sixel encoder and its clearing behavior. Reject a
/// malformed DCS before it reaches the terminal. Do not wrap cursor-control
/// sequences together with Sixel, since tmux must own their pane placement.
pub(super) fn wrap_tmux_sixel_buffer(buffer: &mut ratatui::buffer::Buffer) -> Result<(), ()> {
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if !symbol.contains(SIXEL_DCS) { continue; }
        if symbol.contains(TMUX_DCS) {
            let start = symbol.find(TMUX_DCS).ok_or(())?;
            let end = tmux_dcs_end(symbol, start).ok_or(())?;
            // The pinned Sixel renderer wraps its clear + Sixel graphics in
            // one DCS; do not double wrap this already-owned sequence.
            if symbol[end..].contains(SIXEL_DCS) ||
                !symbol[start + TMUX_DCS.len()..end].contains("\x1b\x1bP") ||
                !symbol[start + TMUX_DCS.len()..end].contains('q')
            { return Err(()); }
            continue;
        }
        let mut out = String::with_capacity(symbol.len().saturating_add(32));
        let mut rest = symbol;
        while let Some(pos) = rest.find(SIXEL_DCS) {
            out.push_str(&rest[..pos]);
            let body = &rest[pos + SIXEL_DCS.len()..];
            if !sixel_header(body) { return Err(()); }
            let end = body.find(APC_END).ok_or(())?;
            let command = &rest[pos..pos + SIXEL_DCS.len() + end + APC_END.len()];
            out.push_str(TMUX_DCS);
            for c in command.chars() {
                if c == '\x1b' { out.push('\x1b'); }
                out.push(c);
            }
            out.push_str(APC_END);
            rest = &body[end + APC_END.len()..];
        }
        out.push_str(rest);
        cell.set_symbol(&out);
    }
    Ok(())
}

#[cfg(test)]
mod tmux_tests {
    use super::*;

    #[test]
    fn sixel_identity_changes_only_with_payload_or_placement() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.get_mut(0, 0).set_symbol("\x1bPq#0~\x1b\\");
        let first = sixel_upload_fingerprint(&buf).unwrap();
        buf.get_mut(2, 0).set_symbol("new status text");
        assert_eq!(sixel_upload_fingerprint(&buf), Some(first),
            "ordinary text redraws do not invalidate an image upload");
        buf.get_mut(0, 0).set_symbol("\x1bPq#1~\x1b\\");
        let different = sixel_upload_fingerprint(&buf).unwrap();
        assert_ne!(different, first);
        buf.get_mut(0, 0).set_symbol(" ");
        buf.get_mut(1, 0).set_symbol("\x1bPq#0~\x1b\\");
        assert_ne!(sixel_upload_fingerprint(&buf), Some(first),
            "moving the origin requires another forwarding check");
    }

    #[test]
    fn an_unverified_image_is_suppressed_without_erasing_other_ui() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf.get_mut(0, 0).set_symbol("\x1bPq#0~\x1b\\");
        buf.get_mut(1, 0).set_symbol("\x1b_Ga=T,q=2,m=0;QUJD\x1b\\");
        buf.get_mut(2, 0).set_symbol("\u{10eeee}");
        buf.get_mut(3, 0).set_symbol("status");
        suppress_unverified_tmux_graphics(&mut buf);
        for x in 0..3 { assert_eq!(buf.get(x, 0).symbol(), " "); }
        assert_eq!(buf.get(3, 0).symbol(), "status");
        suppress_unverified_tmux_graphics(&mut buf);
        assert_eq!(buf.get(3, 0).symbol(), "status");
    }

    #[test]
    fn sixel_is_wrapped_without_hiding_tmux_owned_cursor_commands() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        buf.get_mut(0,0).set_symbol("\x1b[2K\x1bPq#0;2;100;0;0~\x1b\\");
        assert!(buffer_has_sixel_upload(&buf));
        wrap_tmux_sixel_buffer(&mut buf).unwrap();
        let symbol = buf.get(0,0).symbol();
        assert!(symbol.starts_with("\x1b[2K\x1bPtmux;"));
        assert!(symbol.ends_with("\x1b\x1b\\\x1b\\"));
    }

    #[test]
    fn prewrapped_sixel_is_not_double_wrapped() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        let symbol = "\x1bPtmux;\x1b\x1b[2K\x1b\x1bPq#0~\x1b\x1b\\\x1b\\";
        buf.get_mut(0,0).set_symbol(symbol);
        wrap_tmux_sixel_buffer(&mut buf).unwrap();
        assert_eq!(buf.get(0,0).symbol(), symbol);
        buf.get_mut(0,0).set_symbol("\x1bPnot-sixel\x1b\\");
        assert!(wrap_tmux_sixel_buffer(&mut buf).is_err());
    }

    #[test]
    fn wraps_every_chunk_while_retaining_placeholders_and_silencing_replies() {
        let input = "\x1b[0m\x1b_Ga=T,f=24,i=7,q=1,m=1;QUJD\x1b\\\x1b_Gi=7,m=0;REVG\x1b\\\u{10eeee}rest";
        let wrapped = wrap_tmux_kitty_apcs(input).unwrap().unwrap();
        assert_eq!(wrapped.matches(TMUX_DCS).count(), 2);
        assert_eq!(wrapped.matches("q=2").count(), 2);
        assert!(wrapped.starts_with("\x1b[0m"));
        assert!(wrapped.ends_with("\u{10eeee}rest"));
        assert!(wrapped.contains("\x1bPtmux;\x1b\x1b_Ga=T"));
        assert!(wrapped.contains("\x1b\x1b\\\x1b\\"));
        assert!(wrap_tmux_kitty_apcs(&wrapped).unwrap().is_none(),
            "a second transport pass must not double-wrap APCs");
        let unsafe_wrapped = wrapped.replacen("q=2", "q=1", 1);
        assert!(wrap_tmux_kitty_apcs(&unsafe_wrapped).is_err(),
            "already-wrapped commands must still suppress terminal replies");
    }

    #[test]
    fn forwarding_check_is_only_needed_for_a_first_upload_not_placeholders() {
        use ratatui::{buffer::Buffer, layout::Rect};
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer.get_mut(0, 0).set_symbol("\u{10eeee}");
        assert!(!buffer_has_kitty_upload(&buffer),
            "a normal redraw must not recheck tmux or retransmit an image");

        buffer.get_mut(1, 0).set_symbol("\x1b_Ga=T,q=2,m=0;QUJD\x1b\\");
        assert!(buffer_has_kitty_upload(&buffer),
            "a new one-shot upload needs a fresh forwarding check");
        buffer.get_mut(1, 0).set_symbol("\x1bPtmux;\x1b\x1b_Ga=T,q=2,m=0;QUJD\x1b\x1b\\\x1b\\");
        assert!(buffer_has_kitty_upload(&buffer),
            "already-wrapped Kitty streams need the same forwarding check");
    }

    #[test]
    fn malformed_kitty_commands_never_pass_through_raw() {
        for command in [
            "\x1b_Ga=T,q=1,q=0;QUJD\x1b\\",
            "\x1b_Ga=T;QUJD",
            "\x1b_Ga=T;ABC\x1b[31m\x1b\\",
            "\x1b_Ga=T;ABC!!\x1b\\",
            "\x1b_G;AAA\x1b\\",
        ] {
            assert!(wrap_tmux_kitty_apcs(command).is_err(), "{command:?}");
        }
        assert!(wrap_tmux_kitty_apcs("normal text\u{10eeee}").unwrap().is_none());
    }
}

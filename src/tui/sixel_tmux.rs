//! Pane-local, bounded Sixel passthrough for tmux.
//!
//! tmux 3.5a discards an input DCS above 1 MiB. More importantly, tmux
//! restores terminal state *between* passthroughs, so a Sixel DCS must never
//! be split across them. Partition the already-encoded bitmap on terminal
//! cell boundaries into independently terminated Sixel images, retaining
//! every original pixel mask and colour-register definition.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ratatui::buffer::Buffer;

const TMUX_DCS: &str = "\x1bPtmux;";
const DCS: &str = "\x1bP";
const ST: &str = "\x1b\\";
// Account for doubled ESC, tmux; and ST well below tmux's 1 MiB parser cap.
const TILE_LIMIT: usize = 512 * 1024;

#[derive(Clone, Default)]
struct Colours {
    definitions: BTreeMap<usize, String>,
    selected: Option<usize>,
}

/// Emit the Sixel columns belonging to an optional horizontal interval.
/// Always track colour state and the original x cursor, including skipped
/// columns. Sixel's `-` (six-pixel band advance) is handled by the caller.
fn band_columns(
    band: &str,
    bounds: Option<(usize, usize)>,
    state: &mut Colours,
    out: &mut String,
    mask: u8,
) -> Result<usize, ()> {
    let bytes = band.as_bytes();
    let mut i = 0;
    let mut x = 0_usize;
    let mut max_x = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b';') {
                    i += 1;
                }
                let token = band.get(start..i).ok_or(())?;
                let fields: Vec<_> = token[1..].split(';').collect();
                let index: usize = fields.first().ok_or(())?.parse().map_err(|_| ())?;
                match fields.len() {
                    1 => {},
                    5 => {
                        // Valid Sixel palettes are HLS (1) or RGB (2).
                        let space: usize = fields[1].parse().map_err(|_| ())?;
                        if space != 1 && space != 2 { return Err(()); }
                        for value in &fields[2..] {
                            value.parse::<usize>().map_err(|_| ())?;
                        }
                        state.definitions.insert(index, token.to_owned());
                    }
                    _ => return Err(()),
                }
                state.selected = Some(index);
                if bounds.is_some() { out.push_str(token); }
            }
            b'$' => {
                x = 0;
                i += 1;
                if bounds.is_some() { out.push('$'); }
            }
            b'!' => {
                i += 1;
                let first = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                let count: usize = band.get(first..i).ok_or(())?
                    .parse().map_err(|_| ())?;
                if count == 0 || !bytes.get(i).is_some_and(|byte| (b'?'..=b'~').contains(byte)) { return Err(()); }
                let pixel = (((bytes[i] - b'?') & mask) + b'?') as char;
                i += 1;
                let end = x.checked_add(count).ok_or(())?;
                if let Some((left, right)) = bounds {
                    let selected = end.min(right).saturating_sub(x.max(left));
                    if selected == 1 { out.push(pixel); }
                    else if selected > 1 { write!(out, "!{selected}{pixel}").map_err(|_| ())?; }
                }
                x = end;
                max_x = max_x.max(x);
            }
            b'?'..=b'~' => {
                if let Some((left, right)) = bounds {
                    if (left..right).contains(&x) {
                        out.push((((bytes[i] - b'?') & mask) + b'?') as char);
                    }
                }
                x = x.checked_add(1).ok_or(())?;
                max_x = max_x.max(x);
                i += 1;
            }
            // All other Sixel controls are unsupported in this pinned
            // encoder's raster stream. Never forward an unknown large DCS.
            _ => return Err(()),
        }
    }
    Ok(max_x)
}

struct Raster<'a> {
    header: &'a str,
    bands: Vec<&'a str>,
    ratio: (usize, usize),
    width: usize,
    height: usize,
}

fn parse_raster(command: &str) -> Result<Raster<'_>, ()> {
    let start = command.strip_prefix(DCS).ok_or(())?;
    let q = start.bytes().take(32)
        .take_while(|b| b.is_ascii_digit() || *b == b';').count();
    if q >= 32 || start.as_bytes().get(q) != Some(&b'q') { return Err(()); }
    let header = &command[..DCS.len() + q + 1];
    let content = command.strip_suffix(ST).ok_or(())?;
    let mut data = &content[header.len()..];
    let mut ratio = (1, 1);
    let mut dimensions = None;
    if let Some(after) = data.strip_prefix('"') {
        let len = after.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
        let fields: Vec<_> = after[..len].split(';').collect();
        if fields.len() != 4 { return Err(()); }
        let parsed: Vec<usize> = fields.iter()
            .map(|v| v.parse().map_err(|_| ())).collect::<Result<_, _>>()?;
        if parsed.iter().any(|n| *n == 0) { return Err(()); }
        ratio = (parsed[0], parsed[1]);
        dimensions = Some((parsed[2], parsed[3]));
        data = &after[len..];
    }
    let mut bands: Vec<_> = data.split('-').collect();
    if bands.last() == Some(&"") && bands.len() > 1 { bands.pop(); }
    if bands.is_empty() { return Err(()); }
    let (mut state, mut dummy) = (Colours::default(), String::new());
    let mut max_width = 0;
    for band in &bands {
        max_width = max_width.max(band_columns(band, None, &mut state, &mut dummy, 0x3f)?);
    }
    let (width, height) = dimensions.unwrap_or((max_width, bands.len().checked_mul(6).ok_or(())?));
    if width == 0 || height == 0 || width < max_width ||
        bands.len().saturating_sub(1).saturating_mul(6) >= height
    { return Err(()); }
    Ok(Raster { header, bands, ratio, width, height })
}

fn tile_command(
    raster: &Raster<'_>,
    bands: &[&str],
    before: &Colours,
    x: usize, width: usize,
    height: usize,
) -> Result<String, ()> {
    let mut body = String::new();
    write!(body, "\"{};{};{};{}", raster.ratio.0, raster.ratio.1, width, height)
        .map_err(|_| ())?;
    for definition in before.definitions.values() { body.push_str(definition); }
    if let Some(colour) = before.selected { write!(body, "#{colour}").map_err(|_| ())?; }
    let mut state = before.clone();
    for (i, band) in bands.iter().enumerate() {
        if i != 0 { body.push('-'); }
        // Raster attributes are not a clipping instruction in every sixel
        // implementation. Remove only padding bits *outside* the image's
        // declared final pixel row, without changing any in-image pixels.
        let remainder = height % 6;
        let mask = if i + 1 == bands.len() && remainder != 0 {
            ((1_u16 << remainder) - 1) as u8
        } else { 0x3f };
        band_columns(band, Some((x, x.checked_add(width).ok_or(())?)), &mut state, &mut body, mask)?;
    }
    let mut command = String::with_capacity(raster.header.len() + body.len() + ST.len());
    command.push_str(raster.header);
    command.push_str(&body);
    command.push_str(ST);
    Ok(command)
}

fn wrap_complete(out: &mut String, raw: &str) {
    out.push_str(TMUX_DCS);
    for ch in raw.chars() {
        if ch == '\x1b' { out.push('\x1b'); }
        out.push(ch);
    }
    out.push_str(ST);
}

fn encoded_len(raw: &str) -> usize {
    TMUX_DCS.len() + raw.len() + raw.bytes().filter(|b| *b == 0x1b).count() + ST.len()
}

fn unescape_tmux(body: &str) -> Result<String, ()> {
    let mut raw = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.next() != Some('\x1b') { return Err(()); }
        raw.push(ch);
    }
    Ok(raw)
}

/// A single tmux DCS ends at a non-doubled ESC backslash.
fn passthrough_end(symbol: &str, start: usize) -> Option<usize> {
    let bytes = symbol.as_bytes();
    let mut i = start + TMUX_DCS.len();
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if *bytes.get(i + 1)? == 0x1b { i += 2; continue; }
            if bytes[i + 1] == b'\\' { return Some(i + 2); }
            return None;
        }
        i += 1;
    }
    None
}

/// The newly produced transport is idempotent. Every passthrough must
/// independently contain a *complete* Sixel command, not a fragment as R8 did.
fn valid_tiled(symbol: &str) -> bool {
    let mut pos = 0;
    let mut packets = 0;
    while let Some(offset) = symbol[pos..].find(TMUX_DCS) {
        let start = pos + offset;
        if symbol[pos..start].contains(DCS) { return false; }
        let Some(end) = passthrough_end(symbol, start) else { return false; };
        if end - start > TILE_LIMIT { return false; }
        let Ok(raw) = unescape_tmux(&symbol[start + TMUX_DCS.len()..end - ST.len()])
            else { return false; };
        let Some(sixel_start) = raw.find(DCS) else { return false; };
        let nested = &raw[sixel_start..];
        if parse_raster(nested).is_err() { return false; }
        packets += 1;
        pos = end;
    }
    packets > 1 && !symbol[pos..].contains(DCS)
}

/// Returns None when no alteration is necessary. A small, already-wrapped
/// Sixel image keeps the pinned renderer's exact output, including clearing.
fn transform(symbol: &str, x: u16, y: u16, font: (u16, u16), available: (u16, u16))
    -> Result<Option<String>, ()>
{
    if !symbol.contains(DCS) { return Ok(None); }
    if symbol.matches(TMUX_DCS).count() > 1 {
        return valid_tiled(symbol).then_some(None).ok_or(());
    }
    let (raw, was_wrapped, outer_before, outer_after) = if let Some(start) = symbol.find(TMUX_DCS) {
        let end = passthrough_end(symbol, start).ok_or(())?;
        let body = &symbol[start + TMUX_DCS.len()..end - ST.len()];
        let raw = unescape_tmux(body)?;
        (raw, true, &symbol[..start], &symbol[end..])
    } else {
        (symbol.to_owned(), false, "", "")
    };
    let sixel_start = raw.find(DCS).ok_or(())?;
    let sixel_end = raw[sixel_start + DCS.len()..].find(ST).ok_or(())?
        + sixel_start + DCS.len() + ST.len();
    if raw[sixel_end..].contains(DCS) || outer_before.contains(DCS) || outer_after.contains(DCS) {
        return Err(());
    }
    let command = &raw[sixel_start..sixel_end];
    let prelude = &raw[..sixel_start];
    let suffix = &raw[sixel_end..];
    if !suffix.is_empty() { return Err(()); }
    // Validate even a small command before it reaches the outer terminal.
    let raster = parse_raster(command)?;
    if was_wrapped && symbol.len() <= TILE_LIMIT { return Ok(None); }
    if !was_wrapped && encoded_len(command) <= TILE_LIMIT {
        let mut output = String::from(prelude);
        wrap_complete(&mut output, command);
        return Ok(Some(output));
    }

    let (cw, ch) = (font.0 as usize, font.1 as usize);
    if cw == 0 || ch == 0 { return Err(()); }
    let max_width = usize::from(available.0);
    let max_height = usize::from(available.1);
    if raster.width > max_width.saturating_mul(cw) ||
        raster.height > max_height.saturating_mul(ch) { return Err(()); }

    // Each vertical partition begins at a *character-cell* boundary. Sixel
    // advances in bands of six pixels, so the least such positive interval
    // is lcm(6, cell_height). Horizontal splits are also cell-aligned.
    let groups_bands = ch / gcd(ch, 6);
    let mut output = String::from(outer_before);
    if !was_wrapped { output.push_str(prelude); }
    let mut current = Colours::default();
    let mut first = true;
    let mut band = 0;
    while band < raster.bands.len() {
        let end = (band + groups_bands).min(raster.bands.len());
        let top_pixels = band.checked_mul(6).ok_or(())?;
        if top_pixels >= raster.height { return Err(()); }
        let height = (end - band).checked_mul(6).ok_or(())?
            .min(raster.height - top_pixels);
        let bands = &raster.bands[band..end];
        let mut pending = vec![(0_usize, raster.width)];
        while let Some((left, right)) = pending.pop() {
            let tile = tile_command(&raster, bands, &current, left, right - left, height)?;
            let mut payload = String::new();
            if first && was_wrapped { payload.push_str(prelude); }
            payload.push_str(&tile);
            if encoded_len(&payload) > TILE_LIMIT {
                // Split along character columns, keeping both children in
                // display order. A single cell still too large is unsafe.
                let columns = (right - left).div_ceil(cw);
                if columns <= 1 { return Err(()); }
                let middle = left + (columns / 2) * cw;
                if middle >= right { return Err(()); }
                pending.push((middle, right));
                pending.push((left, middle));
                continue;
            }
            if !first {
                let col = usize::from(x) + left / cw + 1;
                let row = usize::from(y) + top_pixels / ch + 1;
                // CUP alone changes tmux's virtual cursor but may not move
                // the outer terminal before tty_cmd_rawstring(). ECH emits
                // a real pane-clamped tty operation at that position without
                // advancing the cursor (and the tile repaints the one cell).
                // This also keeps tiled images inside split/offset panes.
                write!(output, "\x1b[{row};{col}H\x1b[1X").map_err(|_| ())?;
            }
            wrap_complete(&mut output, &payload);
            first = false;
        }
        let mut ignored = String::new();
        for b in bands {
            band_columns(b, None, &mut current, &mut ignored, 0x3f)?;
        }
        band = end;
    }
    output.push_str(outer_after);
    Ok(Some(output))
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 { let t = a % b; a = b; b = t; }
    a
}

/// This runs once on a new native frame, after the existing tmux capability
/// preflight. No terminal queries or image decoding occur in this path.
pub(super) fn wrap_buffer(buffer: &mut Buffer, font: (u16, u16)) -> Result<(), ()> {
    if buffer.area.width == 0 { return Ok(()); }
    let (width, height) = (buffer.area.width, buffer.area.height);
    for (index, cell) in buffer.content.iter_mut().enumerate() {
        if !cell.symbol().contains(DCS) { continue; }
        let col = (index % usize::from(width)) as u16;
        let row = (index / usize::from(width)) as u16;
        let x = buffer.area.x.checked_add(col).ok_or(())?;
        let y = buffer.area.y.checked_add(row).ok_or(())?;
        if let Some(encoded) = transform(
            cell.symbol(), x, y, font,
            (width.saturating_sub(col), height.saturating_sub(row)),
        )? {
            cell.set_symbol(&encoded);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use std::fmt::Write as _;

    // Model the observable tmux boundary: each passthrough is forwarded as
    // one complete raw string; tmux may write arbitrary terminal state next.
    fn packets(symbol: &str) -> Vec<(usize, usize, String)> {
        let mut result = Vec::new();
        let mut pos = 0;
        let mut position = (1, 1);
        while let Some(offset) = symbol[pos..].find(TMUX_DCS) {
            let start = pos + offset;
            let before = &symbol[pos..start];
            if !result.is_empty() {
                assert!(before.ends_with("\x1b[1X"),
                    "each following tile must force a pane-local physical cursor update");
            }
            if let Some(move_end) = before.rfind("H\x1b[1X") {
                if let Some(move_start) = before[..move_end].rfind("\x1b[") {
                    let coords = &before[move_start + 2..move_end];
                    let (row, col) = coords.split_once(';').unwrap();
                    position = (row.parse().unwrap(), col.parse().unwrap());
                }
            }
            let end = passthrough_end(symbol, start).unwrap();
            assert!(end - start < 1_048_576, "tmux would discard this DCS");
            let raw = unescape_tmux(&symbol[start + TMUX_DCS.len()..end - ST.len()]).unwrap();
            let cmd = &raw[raw.find(DCS).unwrap()..];
            let raster = parse_raster(cmd).expect("each passthrough must be a complete Sixel image");
            assert!(raster.width > 0 && raster.height > 0);
            result.push((position.0, position.1, cmd.to_owned()));
            pos = end;
        }
        result
    }

    fn wrapped(raw: &str) -> String {
        let mut output = String::new();
        wrap_complete(&mut output, raw);
        output
    }

    #[test]
    fn large_viewer_tiles_are_complete_and_cell_aligned() {
        let mut image = String::from("\x1bPq\"1;1;2480;1520#0;2;100;0;0#1;2;0;100;0#2;2;0;0;100");
        for band in 0..254 {
            if band != 0 { image.push('-'); }
            image.push_str("#0");
            image.push_str(&"~".repeat(2480));
            image.push_str("$#1");
            image.push_str(&"A".repeat(2480));
            image.push_str("$#2");
            image.push_str(&"V".repeat(2480));
        }
        image.push_str(ST);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 248, 76));
        let source = wrapped(&format!("\x1b[248X\x1b[76B\x1b[76A{image}"));
        assert!(source.len() > 1_048_576);
        buffer.get_mut(0, 0).set_symbol(&source);
        wrap_buffer(&mut buffer, (10, 20)).unwrap();
        let emitted = buffer.get(0, 0).symbol().to_owned();
        let tiles = packets(&emitted);
        assert_eq!(tiles.len(), 26, "254 six-pixel bands in 10-band strips");
        for (n, (row, col, command)) in tiles.iter().enumerate() {
            assert_eq!(*row, 1 + 3 * n);
            assert_eq!(*col, 1);
            let raster = parse_raster(command).unwrap();
            assert_eq!(raster.width, 2480);
            assert_eq!(raster.height, if n == 25 { 20 } else { 60 });
            assert!(command.contains("#0;2;100;0;0"));
            assert!(command.contains("#1;2;0;100;0"));
            assert!(command.contains("#2;2;0;0;100"));
        }
        wrap_buffer(&mut buffer, (10, 20)).unwrap();
        assert_eq!(buffer.get(0, 0).symbol(), emitted, "repeated passes must not retile");
    }

    #[test]
    fn wide_noisy_band_group_is_split_horizontally_on_cell_boundaries() {
        // Force a single 60-pixel-high group over the packet limit. This
        // must split columns rather than downsample or divide sixel bytes.
        let mut image = String::from("\x1bPq\"1;1;2400;60");
        for colour in 0..32 {
            write!(image, "#{colour};2;{};50;60", colour * 3).unwrap();
        }
        for band in 0..10 {
            if band != 0 { image.push('-'); }
            for colour in 0..32 {
                write!(image, "#{colour}").unwrap();
                image.push_str(&"~".repeat(2400));
                image.push('$');
            }
        }
        image.push_str(ST);
        let source = wrapped(&image);
        assert!(source.len() > TILE_LIMIT);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 240, 3));
        buffer.get_mut(0, 0).set_symbol(&source);
        wrap_buffer(&mut buffer, (10, 20)).unwrap();
        let got = packets(buffer.get(0, 0).symbol());
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].0, got[0].1), (1, 1));
        assert_eq!((got[1].0, got[1].1), (1, 121));
        for (_, _, cmd) in &got {
            let raster = parse_raster(cmd).unwrap();
            assert_eq!(raster.width, 1200);
            assert_eq!(raster.height, 60);
            // 32 colour passes per band, exactly half the original columns.
            assert_eq!(raster.bands.len(), 10);
            assert_eq!(cmd.matches("#31").count(), 11); // palette + 10 passes
            assert!(cmd.contains("#1;2;3;50;60"));
        }
    }

    #[test]
    fn palette_and_active_colour_survive_band_boundaries() {
        let command = "\x1bPq\"1;1;20;120#7;2;15;55;95!20~-$!20A-!20~\x1b\\";
        let raster = parse_raster(command).unwrap();
        let mut state = Colours::default();
        let mut ignored = String::new();
        band_columns(raster.bands[0], None, &mut state, &mut ignored, 0x3f).unwrap();
        let tile = tile_command(&raster, &raster.bands[1..], &state, 0, 20, 12).unwrap();
        assert!(tile.starts_with("\x1bPq\"1;1;20;12#7;2;15;55;95#7"));
        assert!(tile.contains("!20A-!20~"));
    }


    /// Independent reference rasterizer for the synthetic control/data
    /// language exercised here. Decode masks into an indexed pixel canvas;
    /// compare the original full Sixel with four independently placed tiles.
    fn paint(command: &str, canvas: &mut [u32], stride: usize, x0: usize, y0: usize) {
        let bytes = command.as_bytes();
        let mut pos = command.find('q').unwrap() + 1;
        let (mut width, mut height) = (stride, canvas.len() / stride);
        let mut x = 0;
        let mut y = 0;
        let mut colour = 0;
        while pos < bytes.len() - ST.len() {
            match bytes[pos] {
                b'"' => {
                    pos += 1;
                    let begin = pos;
                    while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b';') {
                        pos += 1;
                    }
                    let parts: Vec<usize> = command[begin..pos].split(';')
                        .map(|v| v.parse().unwrap()).collect();
                    width = parts[2];
                    height = parts[3];
                }
                b'#' => {
                    pos += 1;
                    let begin = pos;
                    while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b';') {
                        pos += 1;
                    }
                    colour = command[begin..pos].split(';').next().unwrap()
                        .parse::<u32>().unwrap() + 1;
                }
                b'$' => { x = 0; pos += 1; }
                b'-' => { x = 0; y += 6; pos += 1; }
                b'!' => {
                    pos += 1;
                    let begin = pos;
                    while bytes[pos].is_ascii_digit() { pos += 1; }
                    let n: usize = command[begin..pos].parse().unwrap();
                    let pixel = bytes[pos] - b'?';
                    for dx in 0..n {
                        for bit in 0..6 {
                            if (pixel & (1 << bit)) != 0 && x + dx < width && y + bit < height {
                                canvas[(y0 + y + bit) * stride + x0 + x + dx] = colour;
                            }
                        }
                    }
                    x += n;
                    pos += 1;
                }
                b'?'..=b'~' => {
                    let pixel = bytes[pos] - b'?';
                    for bit in 0..6 {
                        if (pixel & (1 << bit)) != 0 && x < width && y + bit < height {
                            canvas[(y0 + y + bit) * stride + x0 + x] = colour;
                        }
                    }
                    x += 1;
                    pos += 1;
                }
                other => panic!("unexpected Sixel control {other}"),
            }
        }
    }

    #[test]
    fn independent_pixel_decode_matches_four_tiles_including_partial_bottom_band() {
        let raw = "\x1bPq\"1;1;20;14#3;2;10;20;30#4;2;80;60;40#3!20~$#4!10A-#3!20V$#4!10~-#4!20~\x1b\\";
        let raster = parse_raster(raw).unwrap();
        let mut expected = vec![0; 20 * 14];
        paint(raw, &mut expected, 20, 0, 0);
        let mut actual = vec![0; 20 * 14];
        let mut starting = Colours::default();
        for (group, first_band, band_count, y, h) in [(0, 0, 2, 0, 12), (1, 2, 1, 12, 2)] {
            if group == 1 {
                let mut scratch = String::new();
                for band in &raster.bands[..2] {
                    band_columns(band, None, &mut starting, &mut scratch, 0x3f).unwrap();
                }
            }
            for left in [0, 10] {
                let cmd = tile_command(&raster,
                    &raster.bands[first_band..first_band + band_count],
                    &starting, left, 10, h).unwrap();
                paint(&cmd, &mut actual, 20, left, y);
            }
        }
        assert_eq!(actual, expected,
            "Sixel cell-aligned tiles must preserve every original pixel and palette index");
    }

    #[test]
    fn short_preview_is_unchanged_and_second_large_image_is_independent() {
        let small = wrapped("\x1b[2K\x1bPq\"1;1;10;6#1~\x1b\\");
        let mut big = String::from("\x1bPq\"1;1;2000;120#1;2;100;0;0");
        for n in 0..20 {
            if n != 0 { big.push('-'); }
            for _ in 0..400 { big.push_str("#1!5~"); }
        }
        big.push_str(ST);
        let mut buf = Buffer::empty(Rect::new(0, 0, 240, 30));
        buf.get_mut(0, 0).set_symbol(&small);
        buf.get_mut(20, 8).set_symbol(&wrapped(&big));
        wrap_buffer(&mut buf, (10, 20)).unwrap();
        assert_eq!(buf.get(0, 0).symbol(), small);
        assert!(packets(buf.get(20, 8).symbol()).len() >= 1);
    }

    #[test]
    fn broken_r8_fragment_cannot_escape_as_sixel_or_text() {
        let piece1 = wrapped("\x1bPq#0!10~");
        let piece2 = wrapped("#0!10~\x1b\\");
        assert!(transform(&(piece1 + &piece2), 0, 0, (10, 20), (80, 24)).is_err());
        assert!(transform("\x1bPnot-sixel\x1b\\", 0, 0, (10, 20), (80, 24)).is_err());
    }
}

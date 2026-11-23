//! Responsive native extraction overlay (#88).
//! All viewports derive from the current terminal dimensions. The same logical
//! indices register the mouse targets and drive keyboard selection.

use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::convert::pipeline::native_extract::NativeWavContainer;
use crate::disc::{DiscFormat, DiscPresentation, PresentationId};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::disc_browser::{DiscBrowserState, ExtractFocus, ExtractPanel, ExtractOptionControl, TagSheetControl, NATIVE_TAG_FIELD_LABELS, duration_display, sample_rate_display};
use super::theme::{contrast_ratio, Theme};

fn clip(s: &str, width: u16) -> String {
    let w = width as usize;
    if w == 0 { return String::new(); }
    let n = s.chars().count();
    if n <= w { return s.to_owned(); }
    if w < 5 { return s.chars().take(w).collect(); }
    let first = (w - 1) / 2;
    let last = w - first - 1;
    format!("{}…{}", s.chars().take(first).collect::<String>(), s.chars().skip(n-last).collect::<String>())
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024u64.pow(4) { format!("{:.1} TiB",bytes as f64/1024f64.powi(4)) }
    else if bytes >= 1024u64.pow(3) { format!("{:.1} GiB",bytes as f64/1024f64.powi(3)) }
    else if bytes >= 1024u64.pow(2) { format!("{:.0} MiB",bytes as f64/1024f64.powi(2)) }
    else if bytes >= 1024 { format!("{:.1} KiB", bytes as f64 / 1024.0) }
    else { format!("{bytes} B") }
}

/// Clip names and path components from the end rather than erasing their middle.
fn clip_end(s: &str, width: u16) -> String {
    let width = width as usize;
    if s.chars().count() <= width { return s.into(); }
    if width == 0 { return String::new(); }
    if width == 1 { return "…".into(); }
    format!("{}…", s.chars().take(width - 1).collect::<String>())
}

/// Prefer the album-folder identity to the machine-specific path prefix.
/// A surviving final directory (often "Extracted") is retained ahead of
/// shortening the longer preceding disc-folder name.
fn folder_display_value(path: &Path) -> (String, Option<usize>) {
    let home = std::env::var_os("HOME")
        .or_else(||std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    if let Some(home) = home {
        if let Ok(tail) = path.strip_prefix(&home) {
            let prefix_chars = home.to_string_lossy().chars().count();
            let value = if tail.as_os_str().is_empty() { "~".to_string() }
                else { format!("~/{}", tail.display()) };
            return (value, Some(prefix_chars));
        }
    }
    (path.to_string_lossy().into_owned(), None)
}

fn folder_clip_end(text: &str, cells: usize) -> String {
    use super::display_width::{char_width, width};
    if width(text) <= cells { return text.to_owned(); }
    if cells == 0 { return String::new(); }
    let mut clipped = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > cells - 1 { break; }
        clipped.push(ch);
        used += w;
    }
    clipped.push('…');
    clipped
}

fn compact_folder_path(path: &Path, width: u16) -> String {
    let (value, _) = folder_display_value(path);
    if super::display_width::width(&value) <= width as usize { return value; }
    let home_prefix = value.starts_with("~/") || value.starts_with("~\\");
    let absolute = path.is_absolute();
    let mut pieces: Vec<&str> = value.split(|ch| ch == '/' || ch == '\\')
        .filter(|part| !part.is_empty() && *part != "~").collect();
    if pieces.is_empty() { return folder_clip_end(&value, width as usize); }
    let prefix = if home_prefix { "~/…/" } else if absolute { "/…/" } else { "…/" };
    while pieces.len() > 2 && super::display_width::width(&format!("{prefix}{}", pieces.join("/"))) > width as usize {
        pieces.remove(0);
    }
    let mut compact: Vec<String> = pieces.iter().map(|part| (*part).to_owned()).collect();
    if super::display_width::width(&format!("{prefix}{}", compact.join("/"))) <= width as usize {
        return format!("{prefix}{}", compact.join("/"));
    }
    // The parent is usually the disc name. Keep its beginning and the last
    // folder name whole while space permits; only then shorten that last name.
    for i in (0..compact.len()).rev().skip(1).chain(std::iter::once(compact.len()-1)) {
        let used = super::display_width::width(prefix) + compact.iter().enumerate()
            .filter(|(j, _)| *j != i).map(|(_, part)| super::display_width::width(part)).sum::<usize>()
            + compact.len().saturating_sub(1);
        let available = (width as usize).saturating_sub(used);
        compact[i] = folder_clip_end(&compact[i], available);
        if super::display_width::width(&format!("{prefix}{}",compact.join("/"))) <= width as usize {
            return format!("{prefix}{}",compact.join("/"));
        }
    }
    folder_clip_end(&format!("{prefix}{}",compact.join("/")),width as usize)
}

/// Convert a terminal-cell offset into a scalar position. A click inside a
/// double-width glyph lands at that glyph's leading edge, matching ratatui's
/// cell geometry without confusing a rendered column with a character index.
fn folder_char_at_cell(text: &str, cell: usize) -> usize {
    let mut used = 0;
    for (index, ch) in text.chars().enumerate() {
        let next = used + super::display_width::char_width(ch);
        if cell < next { return index; }
        used = next;
    }
    text.chars().count()
}

/// Translate an already-focused Folder preview click into the full-path
/// caret before opening the shared text editor. Once editing, all clicks,
/// word selection and dragging use TextInputState's own cell mapping.
/// Matching visible runs from the right keeps `~` and collapsed `…` from
/// shifting later path components.
pub(super) fn native_folder_preview_caret(path: &Path, field_width: u16, cell: usize) -> usize {
    let (value, home_prefix_chars) = folder_display_value(path);
    let preview = compact_folder_path(path, field_width);
    // Windows-style separators are rendered as '/' when components collapse.
    // This normalization does not change Unicode scalar positions.
    let full: Vec<char> = value.chars().map(|ch| if ch == '\\' { '/' } else { ch }).collect();
    let visible: Vec<char> = preview.chars().map(|ch| if ch == '\\' { '/' } else { ch }).collect();
    let mut positions = vec![None; visible.len()];
    let mut end = visible.len();
    let mut limit = full.len();
    while end > 0 {
        if visible[end - 1] == '…' { end -= 1; continue; }
        let begin = visible[..end].iter().rposition(|ch| *ch == '…').map_or(0, |i| i + 1);
        let run = &visible[begin..end];
        if run.len() <= limit {
            if let Some(at) = (0..=limit-run.len()).rev()
                .find(|at| &full[*at..*at+run.len()] == run) {
                for offset in 0..run.len() { positions[begin+offset] = Some(at+offset); }
                limit = at;
            }
        }
        end = begin;
    }
    let index = folder_char_at_cell(&preview, cell);
    let display_index = if index < positions.len() {
        positions[index]
            .or_else(|| positions[index+1..].iter().copied().flatten().next())
            .or_else(|| positions[..index].iter().rev().copied().flatten().next().map(|at|at+1))
            .unwrap_or(0)
    } else {
        positions.iter().rev().copied().flatten().next().map_or(0,|at|at+1)
    };
    let raw_index = match home_prefix_chars {
        Some(prefix) if display_index > 0 => prefix + display_index - 1,
        Some(prefix) if preview == "~" => prefix,
        _ => display_index,
    };
    raw_index.min(path.to_string_lossy().chars().count())
}

fn check(checked: bool) -> &'static str { if checked { "☑" } else { "☐" } }
fn program_check(s: &DiscBrowserState, i: usize) -> &'static str {
    if !s.is_extractable(i) { "×" } else { check(s.selected[i]) }
}

fn header_for(id: PresentationId, p: &DiscPresentation, index: usize) -> String {
    match id {
        PresentationId::SacdArea(_) => p.id.compact_label(),
        PresentationId::CueAlbum(_) | PresentationId::ArchiveFiles => p.label.clone(),
        PresentationId::DvdAudioGroup(n) => format!("Group {n}"),
        PresentationId::DvdVideoTitle { vts_number, title_number, .. } => format!("VTS {vts_number} · Title {title_number}"),
        PresentationId::BluRayTitle { playlist_number, display_angle, .. } =>
            format!("Playlist {playlist_number:05} · angle {}{}", display_angle.get(), if index == 0 { " · main title" } else { "" }),
        PresentationId::MatroskaStream { audio_ordinal, .. } |
        PresentationId::TransportStream { audio_ordinal, .. } =>
            format!("Audio stream {audio_ordinal}"),
    }
}

fn program_info(p: &DiscPresentation, format: DiscFormat) -> String {
    let codec = p.format.codec.as_deref().unwrap_or("unknown");
    let loss = if p.format.lossless { "lossless" } else { "lossy" };
    if format == DiscFormat::Archive {
        return format!("{} original archive files · no conversion",p.tracks.len());
    }
    if format == DiscFormat::Sacd {
        let dst = p.tracks.iter().any(|t| t.format_note.as_deref().unwrap_or("").contains("DST"));
        // The SACD mapper supplies layout labels (including 5.0) directly.
        // Keep the review's familiar numeric spellings for Stereo and 5.1.
        let channels = match (p.format.channel_layout.as_deref(),p.format.channels) {
            (Some("Stereo" | "stereo"), _) => "2.0".to_owned(),
            (Some("Multichannel" | "multichannel"), Some(5)) => "5.0".to_owned(),
            (Some("Multichannel" | "multichannel"), Some(6)) => "5.1".to_owned(),
            (Some(layout), _) if !layout.is_empty() => layout.to_owned(),
            _ => match p.format.channels {
                Some(2) => "2.0".to_owned(),
                Some(5) => "5.0".to_owned(),
                Some(6) => "5.1".to_owned(),
                Some(n) => format!("{n} ch"),
                None => "unknown".into(),
            },
        };
        return format!("{} · lossless · DSD64 · {channels}", if dst { "DST" } else { "DSD" });
    }
    let rate = p.format.sample_rate.map(sample_rate_display).unwrap_or_else(|| "probing".into());
    let depth = p.format.bit_depth.map_or_else(|| "probing".into(), |bits| format!("{bits}-bit"));
    let channels = p.format.channel_layout.clone().unwrap_or_else(||
        p.format.channels.map_or_else(|| "? ch".into(), |ch| format!("{ch} ch")));
    format!("{codec} · {loss} · {rate}/{depth} · {channels}")
}

fn space_for(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() { p = p.parent()?; }
    fs2::available_space(p).ok()
}

/// A local, palette-derived hierarchy. No theme has to maintain an extra
/// extraction-specific palette. The interpolation also works for light themes.
#[derive(Clone, Copy)]
struct ExtractColors {
    base: Color,
    band: Color,
    focused: Color,
    unfocused: Color,
    stripe_focused: Color,
    stripe_unfocused: Color,
    selection_muted: Color,
    soft: Color,
    label: Color,
    dim: Color,
    faint: Color,
}

fn blend_color(a: Color, b: Color, b_parts: u16, total: u16) -> Color {
    match (a, b) {
        (Color::Rgb(ar,ag,ab), Color::Rgb(br,bg,bb)) => {
            let c = |a:u8,b:u8| ((u16::from(a) * (total-b_parts)
                + u16::from(b) * b_parts) / total) as u8;
            Color::Rgb(c(ar,br),c(ag,bg),c(ab,bb))
        }
        _ => b,
    }
}

fn extract_colors(theme: Theme) -> ExtractColors {
    let band=theme.surface;
    let focused=blend_color(band,theme.selection_bg,1,4);
    let unfocused=theme.panel_bg;
    ExtractColors {
        base: theme.panel_bg, band, focused, unfocused,
        stripe_focused: blend_color(focused,theme.selection_bg,1,9),
        stripe_unfocused: blend_color(unfocused,band,1,3),
        selection_muted: blend_color(unfocused,theme.selection_bg,1,2),
        soft: blend_color(theme.value,theme.label,1,3),
        label: blend_color(theme.label,theme.value,1,4),
        dim: theme.label,
        faint: blend_color(theme.label,theme.panel_bg,1,3),
    }
}

/// Adjust only word-bearing text, retaining the theme's hue and original tier
/// wherever it already clears WCAG AA normal-text contrast (4.5:1).
/// Unlike dim numbering and decorative separators, these colors carry meaning.
fn readable_word(color: Color, surface: Color) -> Color {
    const MIN_CONTRAST: f64 = 4.5;
    if contrast_ratio(color, surface) >= MIN_CONTRAST { return color; }

    // Both poles are evaluated: dark themes generally need a lighter glyph,
    // while a light theme's selection bar can require a darker one. The
    // smaller adjustment preserves more of the palette's original identity.
    let mut best: Option<(u16, Color)> = None;
    for pole in [Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0)] {
        if contrast_ratio(pole, surface) < MIN_CONTRAST { continue; }
        let (mut lo, mut hi) = (0u16, 256u16);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if contrast_ratio(blend_color(color, pole, mid, 256), surface) >= MIN_CONTRAST {
                hi = mid;
            } else { lo = mid + 1; }
        }
        if best.is_none_or(|(amount, _)| lo < amount) {
            best = Some((lo, blend_color(color, pole, lo, 256)));
        }
    }
    best.map_or(color, |(_, adjusted)| adjusted)
}

#[derive(Clone, Copy)]
struct WordTiers {
    value: Color,
    soft: Color,
    label: Color,
}

fn word_tiers(theme: Theme, colors: ExtractColors, surface: Color) -> WordTiers {
    WordTiers {
        value: readable_word(theme.value, surface),
        soft: readable_word(colors.soft, surface),
        label: readable_word(colors.label, surface),
    }
}

fn fill(frame: &mut ratatui::Frame, area: Rect, color: Color) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(Block::default().style(Style::default().bg(color)),area);
    }
}

/// The divider owns its two side junctions; it never uses the final frame row.
fn divider(frame: &mut ratatui::Frame, popup: Rect, y: u16, theme: Theme,
    separator: Option<(u16, char)>) {
    if popup.width < 2 { return; }
    let mut line=vec!['─'; popup.width as usize];
    line[0]='├';
    line[popup.width as usize-1]='┤';
    if let Some((x,junction))=separator {
        if x>popup.x && x<popup.x+popup.width-1 {
            line[(x-popup.x) as usize]=junction;
        }
    }
    frame.render_widget(Paragraph::new(line.into_iter().collect::<String>())
        .style(Style::default().fg(theme.title).bg(theme.panel_bg)),
        Rect::new(popup.x,y,popup.width,1));
}

fn preferred_program_rows(state: &DiscBrowserState) -> u16 {
    let visible=state.contents.presentations.iter().enumerate()
        .filter(|(i,_)|state.is_visible_program(*i)).count();
    let rows=if matches!(state.contents.format,DiscFormat::Sacd|DiscFormat::DvdAudio) {
        2*visible
    } else { 2*visible + 1 };
    (rows + usize::from(!state.contents.suppressed.is_empty())).min(u16::MAX as usize) as u16
}

/// The title bar is separate from the three-sided body block. All child rows
/// are contained in `body_block.inner(body)`, so they cannot erase its corners.
/// The default occupies most of a roomy terminal, while respecting the
/// two-column and one-row margins. Extra space goes to the lists.
pub fn draw(
    frame: &mut ratatui::Frame,
    state: &DiscBrowserState,
    scratch: &Path,
    buttons: &mut ButtonRenderMap,
    theme: Theme,
) {
    draw_with_hover(frame, state, scratch, buttons, theme, None);
}

/// Render the active Extract window with its mouse hover target. The ordinary
/// entrypoint remains for drawing noninteractive parent windows and tests.
pub fn draw_with_hover(
    frame: &mut ratatui::Frame,
    state: &DiscBrowserState,
    scratch: &Path,
    buttons: &mut ButtonRenderMap,
    theme: Theme,
    hover: Option<TuiButton>,
) {
    let area=frame.size();
    if area.width<64 || area.height<20 {
        frame.render_widget(Paragraph::new("Make the terminal at least 64×20 to extract.")
            .alignment(Alignment::Center).style(Style::default().fg(theme.label)),
            Rect::new(area.x,area.y+area.height/2,area.width,1));
        return;
    }
    let wide=area.width>=140 && area.height>=36;
    let small=area.width<96 || area.height<30;
    let colors=extract_colors(theme);
    let header_height=if small {2} else {3};
    let output_count=state.output_rows().len() as u16;
    // A stop per setting, while the wide layout still fits four rows of
    // controls (the first four occupy two columns).
    // Three read-only helper lines (format, folder, resolved naming preview)
    // belong to the form but never take focus.
    let output_height=if small {1} else if wide {state.wide_output_height()}
        else {output_count.saturating_add(3)};
    let results_height=1;
    let footer_height=if small {1}else{2};
    let problems=state.blockers(scratch);
    // A blocker replaces the caption, rather than creating an extra form row.
    let warning_height=0u16;
    let item_count=state.selected_presentation().map_or(0,|p|p.tracks.len())
        .min(u16::MAX as usize) as u16;
    let pane_height=12u16.max(item_count.saturating_add(3))
        .max(preferred_program_rows(state).saturating_add(1));
    // Two frame rows (title and bottom) and three horizontal separators.
    let requested_height=2u16.saturating_add(header_height).saturating_add(3)
        .saturating_add(pane_height).saturating_add(output_height)
        .saturating_add(u16::from(!small)).saturating_add(warning_height)
        .saturating_add(results_height).saturating_add(footer_height);
    // A 25% increase can be imperceptible when the content-based window
    // already nearly fills the terminal. In larger terminals, give the lists
    // room to breathe instead of opening a compact content-sized popup.
    // Keep one terminal row visible above/below the window.
    let height=requested_height.saturating_add(requested_height.saturating_add(3)/4)
        .max((u32::from(area.height)*9/10) as u16)
        .min(area.height.saturating_sub(2));
    let longest_item=state.contents.presentations.iter()
        .flat_map(|p|p.tracks.iter()).filter_map(|t|t.title.as_deref())
        .map(|t|t.chars().count()).max().unwrap_or(0);
    let natural_width=(78usize + longest_item).clamp(96,150) as u16;
    // Prefer a substantial default width on spacious terminals. The extra
    // columns belong to the track pane, not to the program column.
    let width=natural_width.saturating_add(natural_width.saturating_add(3)/4)
        .max((u32::from(area.width)*9/10) as u16)
        .min(area.width.saturating_sub(4));
    let popup=if state.maximized { area } else { Rect::new(
        area.x+(area.width-width)/2,area.y+(area.height-height)/2,width,height,
    ) };
    state.form_wide.set(wide && popup.width.saturating_sub(6) >= 88);
    state.footer_extract_enabled.set(problems.is_empty());
    // The window is the foreground; Browse should remain context, not compete
    // for attention with its selection, scrollbar, and metadata headings.
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell=frame.buffer_mut().get_mut(x,y);
            let faded_bg=blend_color(cell.bg,colors.base,3,4);
            cell.set_bg(faded_bg);
            cell.set_fg(colors.faint);
        }
    }
    frame.render_widget(Clear,popup);
    let body=Rect::new(popup.x,popup.y+1,popup.width,popup.height.saturating_sub(1));
    let body_block=Block::default().borders(Borders::LEFT|Borders::RIGHT|Borders::BOTTOM)
        .style(Style::default().bg(colors.base))
        .border_style(Style::default().fg(theme.title));
    let inner=body_block.inner(body);
    fill(frame,body,colors.base);
    frame.render_widget(body_block,body);

    let source_name=state.source_path.file_name()
        .map(|v|v.to_string_lossy().into_owned())
        .unwrap_or_else(||state.contents.label.clone());
    let action=match state.contents.format {
        DiscFormat::Sacd|DiscFormat::DvdAudio=>"extract tracks",
        DiscFormat::DvdVideo|DiscFormat::BluRay=>"extract chapters",
        DiscFormat::Matroska|DiscFormat::TransportFile=>"extract audio",
        DiscFormat::CueImage=>"extract CUE tracks",
        DiscFormat::Archive=>"extract files",
    };
    let source_type=format!("{} {}",state.contents.format.name(),
        if state.source_path.is_dir() {"folder"}else if matches!(state.contents.format,
            DiscFormat::Matroska | DiscFormat::TransportFile | DiscFormat::Archive) {"file"} else {"ISO"});
    let indicator=if state.maximized {"▾"}else{"▸"};
    let presentation=state.selected_presentation()
        .map(|p|p.id.compact_label()).unwrap_or_else(||"no selection".into());
    let leading=format!("{indicator} {action}  [▾ {presentation} · {source_type}] ");
    let trailing=format!(" {source_name} ");
    frame.render_widget(Paragraph::new(super::draw_browse::browse_title_bar_line(
        popup.width as usize,&leading,&trailing,theme.title,theme,
        theme.panel_bg,None)),Rect::new(popup.x,popup.y,popup.width,1));
    buttons.record_button(TuiButton::DiscBrowserTitle,Rect::new(popup.x,popup.y,popup.width,1));
    buttons.record_button(TuiButton::DiscBrowserTitleSize,Rect::new(popup.x+1,popup.y,1,1));

    let chunks=Layout::default().direction(Direction::Vertical).constraints([
        Constraint::Length(header_height),Constraint::Length(1),
        Constraint::Min(1),Constraint::Length(1),
        Constraint::Length(output_height),Constraint::Length(u16::from(!small)),
        Constraint::Length(warning_height),Constraint::Length(results_height),
        Constraint::Length(1),Constraint::Length(footer_height),
    ]).split(inner);
    fill(frame,chunks[0],colors.band);
    let header_text=Rect::new(chunks[0].x+2,chunks[0].y,
        chunks[0].width.saturating_sub(4),chunks[0].height);
    draw_header(frame,state,header_text,small,theme);
    let mut pane_separator=None;
    if small {
        match state.focus {
            ExtractFocus::Programs=>draw_programs(frame,state,chunks[2],buttons,theme),
            ExtractFocus::Items=>draw_items(frame,state,chunks[2],buttons,theme,false),
            ExtractFocus::Output | ExtractFocus::Footer=>draw_output_with_hover(frame,state,chunks[2],buttons,theme,false,hover),
        }
    } else {
        let longest = state.contents.presentations.iter().enumerate().map(|(i, p)| {
            let headline = header_for(p.id, p, i).chars().count()
                + duration_display(p.total_duration_secs).chars().count()
                + p.tracks.len().to_string().len() + 18;
            let detail = program_info(p, state.contents.format).chars().count() + 8;
            headline.max(detail)
        }).max().unwrap_or(0).min(u16::MAX as usize) as u16;
        let pane_width=chunks[2].width;
        // Hold the natural program column while the additional width goes
        // entirely to tracks. Still adapt when the terminal is narrower.
        let base_pane_width=natural_width.saturating_sub(2).min(pane_width);
        let min_width=(u32::from(base_pane_width)*28/100) as u16;
        let max_width=(base_pane_width/2).min(pane_width/2);
        let program_width=longest.saturating_add(4)
            .clamp(min_width.min(max_width),max_width)
            .min(pane_width.saturating_sub(2));
        let panes=Layout::default().direction(Direction::Horizontal).constraints([
            Constraint::Length(program_width),Constraint::Length(1),Constraint::Min(1)
        ]).split(chunks[2]);
        pane_separator=Some(panes[1].x);
        draw_programs(frame,state,panes[0],buttons,theme);
        draw_items(frame,state,panes[2],buttons,theme,wide);
        for y in panes[1].y..panes[1].y+panes[1].height {
            frame.render_widget(Paragraph::new("│").style(Style::default()
                .fg(theme.title).bg(colors.base)),Rect::new(panes[1].x,y,1,1));
        }
    }
    if small {
        let program=match state.contents.format {
            DiscFormat::Sacd=>"Areas",DiscFormat::DvdAudio=>"Groups",
            DiscFormat::DvdVideo=>"Titles",DiscFormat::BluRay=>"Playlists",
            DiscFormat::Matroska|DiscFormat::TransportFile=>"Streams",
            DiscFormat::CueImage=>"Albums",
            DiscFormat::Archive=>"Archive",
        };
        let item=if state.contents.format == DiscFormat::Archive { "Files" }
            else if state.contents.format == DiscFormat::TransportFile { "Streams" }
            else if matches!(state.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay|DiscFormat::Matroska) {
                "Chapters" } else { "Tracks" };
        let tabs=[(program,ExtractFocus::Programs),(item,ExtractFocus::Items),
            ("Output",ExtractFocus::Output)];
        let mut line=Vec::new();
        let band_words=word_tiers(theme,colors,colors.band);
        for (i,(label,focus)) in tabs.iter().enumerate() {
            if i>0 {line.push(Span::styled("  ",Style::default().fg(colors.faint)));}
            let style=if state.focus==*focus {
                Style::default().fg(readable_word(theme.panel_bg,theme.tab_active)).bg(theme.tab_active)
                    .add_modifier(Modifier::BOLD)
            }else{Style::default().fg(band_words.label)};
            line.push(Span::styled(format!(" {label} "),style));
        }
        frame.render_widget(Paragraph::new(Line::from(line))
            .style(Style::default().bg(colors.band)),chunks[4]);
    } else { draw_output_with_hover(frame,state,chunks[4],buttons,theme,wide,hover); }
    draw_results(frame,state,scratch,chunks[7],problems.first().map(String::as_str),theme);
    fill(frame,chunks[5],colors.base);
    draw_footer(frame,state,chunks[9],buttons,small,problems.is_empty(),theme);
    // These dividers are the only widgets allowed to touch the body rails.
    divider(frame,popup,chunks[1].y,theme,pane_separator.map(|x|(x,'┬')));
    divider(frame,popup,chunks[3].y,theme,pane_separator.map(|x|(x,'┴')));
    divider(frame,popup,chunks[8].y,theme,None);
    if state.overwrite_open {
        if let Some(value) = buttons.button_rect(TuiButton::DiscBrowserOverwrite) {
            let anchor=Rect::new(value.x.saturating_sub(2),value.y,28,1);
            draw_native_overwrite_menu(frame,buttons,anchor,popup,state,theme);
        }
    }
    if let Some(panel)=state.panel { draw_panel(frame,state,panel,popup,buttons,theme,hover); }
}

fn short_genre(text: &str) -> (String, usize) {
    let values=text.split(|ch|matches!(ch,','|';'|'|'|'\n')).map(str::trim)
        .filter(|item|!item.is_empty()).collect::<Vec<_>>();
    (values.first().copied().unwrap_or("").to_string(),values.len().saturating_sub(1))
}

fn append_header_field(fields: &mut Vec<Span<'static>>, value: &str, color: Color, faint: Color) {
    if value.trim().is_empty() {return;}
    if !fields.is_empty() {fields.push(Span::styled("  ·  ",Style::default().fg(faint)));}
    fields.push(Span::styled(value.to_string(),Style::default().fg(color)));
}

fn sacd_details_line(details: &str, words: WordTiers, colors: ExtractColors) -> Line<'static> {
    let mut spans=Vec::new();
    for part in details.split('·').map(str::trim).filter(|part|!part.is_empty()) {
        if !spans.is_empty() {spans.push(Span::styled("  ·  ",Style::default().fg(colors.faint)));}
        let (label,value)=part.split_once(' ').unwrap_or((part,""));
        let display=match label {"Date"=>"Disc date", "Hybrid"=>"hybrid", _=>label};
        if !value.is_empty() && matches!(label,"Catalog"|"Publisher"|"Date") {
            spans.push(Span::styled(format!("{display} "),Style::default().fg(colors.dim)));
            spans.push(Span::styled(value.to_owned(),Style::default().fg(words.soft)));
        }else{
            spans.push(Span::styled(display.to_owned(),Style::default().fg(words.soft)));
            if !value.is_empty(){spans.push(Span::styled(format!(" {value}"),Style::default().fg(words.soft)));}
        }
    }
    Line::from(spans)
}

/// Clip the *rendered* styled line, not the source metadata. SACD detail
/// labels and separators expand during formatting, and truncating the raw
/// source can still overrun the frame. Preserve the existing text colors.
fn clip_styled_header_line(line: Line<'static>, cells: usize, ellipsis_color: Color) -> Line<'static> {
    if cells == 0 {return Line::default();}
    let text_width = line.spans.iter()
        .map(|span|super::display_width::width(span.content.as_ref()))
        .sum::<usize>();
    let clipped = text_width > cells;
    let budget = cells.saturating_sub(usize::from(clipped));
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        if used >= budget {break;}
        let mut part = String::new();
        for ch in span.content.chars() {
            let width = super::display_width::char_width(ch);
            if used + width > budget {break;}
            part.push(ch);
            used += width;
        }
        if !part.is_empty() {spans.push(Span::styled(part,span.style));}
    }
    if clipped {spans.push(Span::styled("…", Style::default().fg(ellipsis_color)));}
    Line::from(spans)
}

fn draw_header(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, small: bool, theme: Theme) {
    if r.width==0 || r.height==0 { return; }
    let colors=extract_colors(theme);
    let words=word_tiers(theme,colors,colors.band);
    fill(frame,r,colors.band);
    let title=s.contents.album_title.as_deref().unwrap_or(&s.contents.label);
    let artist=s.contents.album_artist.as_deref().unwrap_or("no disc text");
    let mut fields=Vec::new();
    append_header_field(&mut fields,title,words.value,colors.faint);
    append_header_field(&mut fields,artist,words.value,colors.faint);
    if let Some(genre)=s.contents.genre.as_deref() {
        let (first,extra)=short_genre(genre);
        if !first.is_empty() {
            append_header_field(&mut fields,&first,words.soft,colors.faint);
            if extra>0 {fields.push(Span::styled(format!(" +{extra}"),Style::default().fg(colors.dim)));}
        }
    }
    if let Some(year)=s.contents.year.as_deref() {
        append_header_field(&mut fields,year,words.soft,colors.faint);
    }
    let mut lines=vec![Line::from(fields)];
    if s.contents.format == DiscFormat::TransportFile {
        let explanation = "No chapters found: orphan file has no disc navigation; extract whole streams.";
        lines.push(clip_styled_header_line(Line::from(Span::styled(explanation,
            Style::default().fg(readable_word(theme.warning, colors.band)))),
            r.width.saturating_sub(2) as usize, colors.dim));
        // The compact header has two rows. Its explanation must remain
        // visible without a Diagnostics popup; the wide view also shows
        // the ordinary diagnostics summary on its third row.
        if small {
            frame.render_widget(Paragraph::new(lines).style(Style::default().bg(colors.band)),r);
            return;
        }
    }
    if !small && s.contents.format==DiscFormat::Sacd {
        let details=s.contents.presentations.first()
            .and_then(|p|p.extract.sacd_header_details.as_deref())
            .unwrap_or("SACD · native DSD64");
        // A formatted " · " separator uses more cells than the raw disc
        // metadata. Clip after styling, leaving two cells of right padding.
        lines.push(clip_styled_header_line(sacd_details_line(details,words,colors),
            r.width.saturating_sub(2) as usize,colors.dim));
    }
    let diagnostics=if s.contents.diagnostics.is_empty(){"none".to_owned()}
        else{s.contents.diagnostics.len().to_string()};
    let color=if s.contents.diagnostics.is_empty(){words.value}
        else{readable_word(theme.warning,colors.band)};
    let mut summary=Vec::new();
    if !(small && s.contents.format==DiscFormat::Sacd) {
        let protection=match s.protection_display().as_str() {
            "None" => "no encryption detected".to_owned(),
            other => other.to_owned(),
        };
        let reserved=if small {
            "Diagnostics  ".chars().count()+diagnostics.chars().count()
                +if s.contents.diagnostics.is_empty(){0}else{"  · d view".chars().count()}+2
        }else{0};
        let available=(r.width as usize).saturating_sub(reserved);
        if !small || available>"Protection  ".chars().count() {
            summary.push(Span::styled("Protection  ",Style::default().fg(words.label)));
            let value=if small {clip(&protection,available.saturating_sub(12) as u16)}
                else{protection};
            summary.push(Span::styled(format!("{value}  "),Style::default().fg(words.soft)));
        }
    }
    summary.push(Span::styled("Diagnostics  ",Style::default().fg(words.label)));
    summary.push(Span::styled(diagnostics,Style::default().fg(color)));
    if !s.contents.diagnostics.is_empty() {
        summary.push(Span::styled("  · ",Style::default().fg(colors.faint)));
        summary.push(Span::styled("d",Style::default().fg(theme.info)));
        summary.push(Span::styled(" view",Style::default().fg(words.label)));
    }
    lines.push(Line::from(summary));
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(colors.band)),r);
}

fn draw_programs(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, buttons: &mut ButtonRenderMap, theme: Theme) {
    let title = match s.contents.format { DiscFormat::Sacd=>"AREAS",DiscFormat::DvdAudio=>"GROUPS",DiscFormat::DvdVideo=>"TITLES",DiscFormat::BluRay=>"PLAYLISTS",
        DiscFormat::Matroska|DiscFormat::TransportFile=>"AUDIO STREAMS",
        DiscFormat::CueImage=>"CUE ALBUMS",DiscFormat::Archive=>"ARCHIVE CONTENTS" };
    let colors=extract_colors(theme);
    let pane_focused=s.focus==ExtractFocus::Programs;
    let pane_bg=if pane_focused{colors.focused}else{colors.unfocused};
    let pane_words=word_tiers(theme,colors,pane_bg);
    fill(frame,r,pane_bg);
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(format!("{title}  "),Style::default().fg(if pane_focused{
            readable_word(theme.header,pane_bg)}else{colors.faint})),
        Span::styled(format!("{} of {} checked{}",s.selected_program_count(),s.len(),
            if s.contents.suppressed.is_empty() { String::new() }
            else {format!(" · {} hidden",s.contents.suppressed.len())}),
            Style::default().fg(pane_words.label)),
    ])).style(Style::default().bg(pane_bg)),Rect::new(r.x+2,r.y,r.width.saturating_sub(4),1));
    let inner = Rect::new(r.x+2,r.y+1,r.width.saturating_sub(4),r.height.saturating_sub(1));
    if inner.width == 0 || inner.height == 0 { return; }
    let mut display_rows: Vec<(Option<usize>,String,bool,Option<usize>)> = Vec::new();
    // Keep all streams of the same authored title/playlist together. Blu-ray
    // presentation scores are global, so adjacent-source-index grouping loses
    // a second stream whenever another playlist ranks between them.
    let mut visited = vec![false; s.contents.presentations.len()];
    for group_start in 0..s.len() {
        if visited[group_start] || !s.is_visible_program(group_start) { continue; }
        let indices = (group_start..s.len())
            .chain(0..group_start)
            .filter(|i| !visited[*i] && s.is_visible_program(*i) && s.shared_program(group_start, *i))
            .collect::<Vec<_>>();
        for &i in &indices { visited[i] = true; }
        let first = &s.contents.presentations[group_start];
        let is_title = matches!(s.contents.format, DiscFormat::DvdVideo | DiscFormat::BluRay);
        if is_title {
            display_rows.push((None, format!("{}  {} · {} {}",
                header_for(first.id, first, group_start),
                duration_display(first.total_duration_secs), first.tracks.len(),
                if first.tracks.len() == 1 { "chapter" } else { "chapters" }), false, None));
            if first.extract.angle_count > 1 {
                display_rows.push((None, format!("  {} angles · angle ◂ {} ▸",
                    first.extract.angle_count, s.selected_angle_for(group_start)), false, Some(group_start)));
            }
        }
        for i in indices {
            let p = &s.contents.presentations[i];
            let checkmark = program_check(s, i);
            let name = if is_title {
                let language = p.extract.language.as_deref().unwrap_or("und");
                let role = p.extract.role.as_deref().unwrap_or("");
                format!("  {checkmark} {language} · {}{}",
                    program_info(p, s.contents.format),
                    if role.is_empty() { String::new() } else { format!(" · {role}") })
            } else {
                format!("{checkmark}  {}  {} · {} {}",
                    header_for(p.id,p,i),duration_display(p.total_duration_secs),p.tracks.len(),
                    if p.tracks.len() == 1 { "track" } else { "tracks" })
            };
            let reason = p.extract.ineligible_reason.as_deref().or_else(||
                s.probe_statuses.get(i).and_then(|status| status.as_ref())
                    .and_then(|result| result.as_ref().err()).map(String::as_str));
            let name = if let Some(reason) = reason {
                format!("{name}  · {reason}")
            } else { name };
            display_rows.push((Some(i),name,s.cursor==i,None));
            if !is_title {
                display_rows.push((None,format!("    {}",program_info(p,s.contents.format)),false,None));
            }
        }
    }
    if !s.contents.suppressed.is_empty() {
        display_rows.push((None,format!("{} hidden · h {} them",s.contents.suppressed.len(),if s.show_hidden {"hides"}else{"shows"}),false,None));
        if s.show_hidden { for p in &s.contents.suppressed {
            display_rows.push((None,format!("○ {} · {}",p.id.compact_label(),p.reason),false,None));
        }}
    }
    let cursor_row = display_rows.iter().position(|(i,_,_,_)| *i==Some(s.cursor)).unwrap_or(0);
    let max_scroll=display_rows.len().saturating_sub(inner.height as usize);
    let scroll = if cursor_row < s.scroll { cursor_row } else if cursor_row>=s.scroll+inner.height as usize {
        cursor_row+1-inner.height as usize } else { s.scroll }.min(max_scroll);
    for (offset,(index,label,focused,angle_owner)) in display_rows.iter()
        .skip(scroll).take(inner.height as usize).enumerate() {
        let y=inner.y+offset as u16;
        if index.is_none() && !s.contents.suppressed.is_empty()
            && label.starts_with(&format!("{} hidden",s.contents.suppressed.len())) {
            buttons.record_button(TuiButton::DiscBrowserHiddenToggle,
                Rect::new(inner.x,y,inner.width,1));
        }
        let active = s.focus==ExtractFocus::Programs && *focused;
        let bg=if active {theme.selection_bg}
            else if *focused {colors.selection_muted}
            else if offset%2==1 {if pane_focused{colors.stripe_focused}else{colors.stripe_unfocused}}
            else{pane_bg};
        frame.render_widget(Block::default().style(Style::default().bg(bg)),
            Rect::new(inner.x,y,inner.width,1));
        if let Some(i)=index {
            buttons.record_button(TuiButton::DiscBrowserStream(*i),Rect::new(inner.x,y,inner.width,1));
            // The one-cell box and the start of its caption toggle selection.
            // The rest of the row still focuses on a single click, as before.
            // Every row has a two-cell focus gutter; title and playlist
            // child streams add two cells before the checkbox.
            let indent = if matches!(s.contents.format, DiscFormat::DvdVideo | DiscFormat::BluRay) {
                4
            } else { 2 };
            let start = inner.x + indent;
            if start < inner.right() {
                buttons.record_button(TuiButton::DiscBrowserExpand(*i),
                    Rect::new(start, y, 6.min(inner.right() - start), 1));
            }
        }
        let words=word_tiers(theme,colors,bg);
        let mut text = label.as_str();
        let mut suffix = None;
        if let Some(i)=index {
            let p=&s.contents.presentations[*i];
            if matches!(s.contents.format,DiscFormat::Sacd|DiscFormat::DvdAudio) {
                let unit=if p.tracks.len()==1 {"track"} else {"tracks"};
                let tail=format!("{} · {} {unit}",duration_display(p.total_duration_secs),p.tracks.len());
                if let Some(base) = label.strip_suffix(&tail) {
                    suffix=Some(tail);
                    text=base.trim_end();
                }
            }
        } else if label.contains(" chapter") {
            // Title/playlist group headings align like SACD program rows.
            if let Some(split) = label.rfind("  ") {
                if label[split+2..].contains(" chapter") {
                    suffix=Some(label[split+2..].to_owned());
                    text=&label[..split];
                }
            }
        }
        let right_width = suffix.as_ref().map_or(0,|t|t.chars().count() as u16);
        let left_width=inner.width.saturating_sub(right_width + if right_width>0 {2}else{0});
        // Truncate before the fixed two-cell gap, never let a long area name
        // collide with its right-aligned duration.
        let visible_name=folder_clip_end(text,left_width.saturating_sub(2) as usize);
        let text=visible_name.as_str();
        let mut spans = Vec::new();
        spans.push(Span::styled(if *focused { "▌ " } else { "  " },
            Style::default().fg(if active{theme.info}else{colors.dim})));
        if let Some(mark) = text.find(|ch| matches!(ch, '☑' | '☐')) {
            spans.push(Span::styled(text[..mark].to_owned(),Style::default().fg(colors.dim)));
            let glyph = &text[mark..mark + '☑'.len_utf8()];
            spans.push(Span::styled(glyph.to_owned(),Style::default().fg(
                if glyph == "☑" { readable_word(theme.success, bg) } else { words.label })));
            spans.push(Span::styled(text[mark + '☑'.len_utf8()..].to_owned(),
                Style::default().fg(if active {words.value}
                    else if index.is_some_and(|i|!s.is_extractable(i)) {theme.error}
                    else {words.value})));
        } else {
            spans.push(Span::styled(text.to_owned(),Style::default().fg(if active{words.value}else{words.soft})));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            Rect::new(inner.x,y,left_width,1));
        if let Some(tail)=suffix {
            frame.render_widget(Paragraph::new(tail).alignment(Alignment::Right)
                .style(Style::default().bg(bg).fg(if active{words.value}else{words.soft})),
                Rect::new(inner.x+inner.width-right_width,y,right_width,1));
        }
        if let Some(owner)=angle_owner {
            for (symbol, button) in [('◂', TuiButton::DiscBrowserAnglePrev(*owner)),
                                     ('▸', TuiButton::DiscBrowserAngleNext(*owner))] {
                if let Some(col)=label.chars().position(|ch|ch==symbol) {
                    let offset=col as u16+2;
                    if offset<inner.width {
                        buttons.record_button(button, Rect::new(inner.x+offset,y,1,1));
                    }
                }
            }
        }
    }
}

fn draw_items(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect,
    buttons: &mut ButtonRenderMap, theme: Theme, wide: bool) {
    let Some(p)=s.selected_presentation() else { return; };
    let kind=if s.contents.format == DiscFormat::TransportFile { "WHOLE STREAM" }
        else if matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay|DiscFormat::Matroska) {
            "CHAPTERS" } else { "TRACKS" };
    let checked=s.items_checked.get(s.cursor).map_or(0,|v|v.iter().filter(|x|**x).count());
    let colors=extract_colors(theme);
    let pane_focused=s.focus==ExtractFocus::Items;
    let pane_bg=if pane_focused{colors.focused}else{colors.unfocused};
    let pane_words=word_tiers(theme,colors,pane_bg);
    fill(frame,r,pane_bg);
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(format!("{kind}  "),Style::default().fg(if pane_focused{
            readable_word(theme.header,pane_bg)}else{colors.faint})),
        Span::styled(format!("{checked} of {} checked · {}",p.tracks.len(),p.id.compact_label()),
            Style::default().fg(pane_words.label)),
    ])).style(Style::default().bg(pane_bg)),Rect::new(r.x+2,r.y,r.width.saturating_sub(4),1));
    let inner=Rect::new(r.x+2,r.y+1,r.width.saturating_sub(4),r.height.saturating_sub(1));
    if inner.width<12 || inner.height==0 { return; }
    let cursor=s.item_cursor.min(p.tracks.len().saturating_sub(1));
    let sacd_detail=(s.contents.format==DiscFormat::Sacd)
        .then(||p.tracks.get(cursor).and_then(|t|t.format_note.as_deref()))
        .flatten().filter(|note|note.contains("ISRC ")||note.contains("Composer ")
            ||note.contains("Songwriter ")||note.contains("Arranger ")||note.contains("Genre "));
    // Reserve a blank row before track details; pin details to the bottom
    // when there are more tracks than visible list rows.
    let detail_rows=if sacd_detail.is_some() && inner.height>=3 {2}else{0};
    let capacity=inner.height.saturating_sub(detail_rows).max(1);
    // When items remain below, the final visible row is a count, not a
    // truncated chapter. Scroll against the smaller selection viewport so
    // the focused row and its click target remain visible.
    let list_rows = if p.tracks.len() > capacity as usize && capacity > 1 {
        capacity - 1
    } else { capacity };
    let scroll = if cursor < s.item_scroll { cursor }
        else if cursor >= s.item_scroll + list_rows as usize {
            cursor + 1 - list_rows as usize
        } else { s.item_scroll }.min(p.tracks.len().saturating_sub(list_rows as usize));
    let remaining = p.tracks.len().saturating_sub(scroll);
    let more_below = remaining > capacity as usize;
    let visible = if more_below { capacity - 1 } else { capacity };
    let mut start_secs=Some(0.0_f64);
    for previous in p.tracks.iter().take(scroll) {
        start_secs=start_secs.and_then(|secs|previous.duration_secs.map(|d|secs+d));
    }
    let differing_performer=p.tracks.iter().any(|track|track.performer.as_deref()
        .is_some_and(|name|Some(name)!=s.contents.album_artist.as_deref()));
    // Measure the entire chosen program once; every title starts in one
    // column even when a later track's time crosses an hour boundary.
    let duration_width=p.tracks.iter().filter_map(|track| track.duration_secs)
        .map(duration_display).map(|text| text.chars().count())
        .max().unwrap_or(0).max(4).min(u16::MAX as usize - 1) as u16 + 1;
    for (offset,(i,track)) in p.tracks.iter().enumerate().skip(scroll)
        .take(visible as usize).enumerate() {
        let y=inner.y+offset as u16;
        let chosen=i==cursor;
        let focused=pane_focused && chosen;
        let bg=if focused{theme.selection_bg}
            else if chosen{colors.selection_muted}
            else if offset%2==1{if pane_focused{colors.stripe_focused}else{colors.stripe_unfocused}}
            else{pane_bg};
        frame.render_widget(Block::default().style(Style::default().bg(bg)),Rect::new(inner.x,y,inner.width,1));
        let style=Style::default().bg(bg);
        let words=word_tiers(theme,colors,bg);
        let mut x=inner.x;
        frame.render_widget(Paragraph::new(if chosen {"▌"}else{" "})
            .style(style.fg(if focused{theme.info}else{colors.dim})),
            Rect::new(x,y,1,1));
        x+=2;
        let checked=s.items_checked.get(s.cursor).and_then(|v|v.get(i))==Some(&true);
        frame.render_widget(Paragraph::new(check(checked)).style(style.fg(
            if checked { readable_word(theme.success, bg) } else { words.label })),
            Rect::new(x,y,1,1));
        let check_x=x;
        x+=2;
        let number_width=if track.number>99 {4}else{3};
        frame.render_widget(Paragraph::new(format!("{:02}",track.number))
            .style(style.fg(colors.dim)),Rect::new(x,y,number_width,1));
        x+=number_width;
        let duration=track.duration_secs.map(duration_display).unwrap_or_default();
        frame.render_widget(Paragraph::new(duration).alignment(Alignment::Right)
            .style(style.fg(if focused{words.value}else{words.soft})),
            Rect::new(x,y,duration_width.saturating_sub(1),1));
        x+=duration_width;
        let tail_width=if wide {9u16}else{0u16};
        let performer_width=if wide && differing_performer {
            ((inner.width.saturating_sub(32))/4).min(24)
        }else{0};
        let title_width=inner.x+inner.width;
        let title_width=title_width.saturating_sub(x+tail_width+performer_width);
        frame.render_widget(Paragraph::new(clip(track.title.as_deref().unwrap_or("Untitled"),title_width))
            .style(style.fg(words.value)),Rect::new(x,y,title_width,1));
        x+=title_width;
        if wide && performer_width>0 {
            frame.render_widget(Paragraph::new(clip(track.performer.as_deref().unwrap_or(""),performer_width))
                .style(style.fg(if focused{words.value}else{words.soft})),Rect::new(x,y,performer_width,1));
        }
        if wide {
            let start=start_secs.map(duration_display).unwrap_or_default();
            let right=inner.x+inner.width;
            frame.render_widget(Paragraph::new(start).alignment(Alignment::Right)
                .style(style.fg(if focused { words.value } else { colors.dim })),
                Rect::new(right.saturating_sub(8),y,8.min(inner.width),1));
        }
        start_secs=start_secs.and_then(|secs|track.duration_secs.map(|duration|secs+duration));
        buttons.record_button(TuiButton::DiscBrowserTrack(i),Rect::new(inner.x,y,inner.width,1));
        buttons.record_button(TuiButton::DiscBrowserTrackCheck(i),Rect::new(check_x,y,1,1));
    }
    if more_below {
        let hidden = remaining.saturating_sub(visible as usize);
        frame.render_widget(Paragraph::new(format!("    ↓ {hidden} more"))
            .style(Style::default().fg(pane_words.label).bg(pane_bg)),
            Rect::new(inner.x,inner.y + visible,inner.width,1));
    }
    if let Some(note)=sacd_detail {
        let y=if p.tracks.len()<=capacity as usize {
            inner.y+(p.tracks.len() as u16).saturating_add(1)
        }else{inner.y+inner.height-1};
        if y<inner.y+inner.height {
            // Track-scoped details always name the track, not just ISRC notes.
            let note=format!("{:02} {note}",p.tracks[cursor].number);
            frame.render_widget(Paragraph::new(clip_end(&note,inner.width))
                .style(Style::default().fg(pane_words.soft).bg(pane_bg)),Rect::new(inner.x,y,inner.width,1));
        }
    }
}


fn option_value(s: &DiscBrowserState, name: &str) -> String {
    match name {
        "Format" if s.contents.format==DiscFormat::Sacd => {
            let mut dsf = false;
            let mut dff = false;
            let mut dst_decoded = false;
            let mut dst_preserved = false;
            let mut native_dsd = false;
            // Match native_job's selected, extractable, nonempty programs and
            // its keep_dst -> dff extension decision. DST is tracked separately
            // only to explain what the selected output will contain.
            for (i,p) in s.contents.presentations.iter().enumerate() {
                if s.selected.get(i) != Some(&true) || !s.is_extractable(i) { continue; }
                let keep_dst = s.keep_dst.get(i) == Some(&true);
                for (j,track) in p.tracks.iter().enumerate() {
                    if s.items_checked.get(i).and_then(|v|v.get(j)) != Some(&true) { continue; }
                    if keep_dst || s.sacd_dff { dff = true; } else { dsf = true; }
                    let dst = p.format.codec.as_deref() == Some("DST")
                        || track.format_note.as_deref().unwrap_or("").contains("DST");
                    if dst {
                        if keep_dst { dst_preserved = true; } else { dst_decoded = true; }
                    } else { native_dsd = true; }
                }
            }
            match (dsf,dff) {
                (false,false) => "DSF · native DSD (no DST decode)".into(),
                (true,false) if dst_decoded && native_dsd => "DSF · native DSD / DST decoded".into(),
                (true,false) if dst_decoded => "DSF · DST decoded to DSD".into(),
                (true,false) => "DSF · native DSD (no DST decode)".into(),
                (false,true) if dst_preserved => "DFF · keep selected DST".into(),
                (false,true) if dst_decoded => "DFF · DST decoded to DSD".into(),
                (false,true) => "DFF · native DSD".into(),
                (true,true) if dst_decoded => "DSF / DFF · decode or keep DST by area".into(),
                (true,true) => "DSF / DFF · native DSD / keep selected DST".into(),
            }
        },
        "Format" if s.contents.format == DiscFormat::Archive =>
            "Original archive files · byte-for-byte copy".into(),
        "Format" if s.contents.format == DiscFormat::CueImage => {
            let formats: std::collections::BTreeSet<_> = s.cue_segments.iter().enumerate()
                .filter(|(i,_)|s.selected.get(*i)==Some(&true))
                .flat_map(|(_,segments)| segments.iter().map(|segment|segment.extension.as_str()))
                .collect();
            let lossless=s.cue_segments.iter().enumerate()
                .filter(|(i,_)|s.selected.get(*i)==Some(&true))
                .flat_map(|(_,segments)|segments.iter()).all(|segment|
                    !matches!(segment.encoder.as_str(),"libmp3lame"|"aac"|"libvorbis"|"libopus"));
            let lossy=s.cue_segments.iter().enumerate()
                .filter(|(i,_)|s.selected.get(*i)==Some(&true))
                .flat_map(|(_,segments)|segments.iter()).all(|segment|
                    matches!(segment.encoder.as_str(),"libmp3lame"|"aac"|"libvorbis"|"libopus"));
            let policy=if lossless {"lossless re-encode, exact samples"}
                else if lossy {"original compressed packets; no re-encode"}
                else {"lossless exact; lossy packets copied"};
            format!("{} · {policy}",formats.into_iter()
                .map(|ext|ext.to_uppercase()).collect::<Vec<_>>().join(" / "))
        },
        "Format" if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile) => {
            if s.file_per_program {
                let ext = s.contents.presentations.iter().enumerate()
                    .filter(|(i,_)| s.selected.get(*i)==Some(&true))
                    .map(|(_,p)| crate::convert::pipeline::native_extract::file_stream_copy_format(
                        s.contents.format, p.format.codec.as_deref().unwrap_or("unknown")).extension)
                    .collect::<std::collections::BTreeSet<_>>();
                let unpack = s.contents.format == DiscFormat::TransportFile &&
                    s.contents.presentations.iter().any(|p| matches!(p.format.codec.as_deref(),
                        Some("pcm_dvd" | "pcm_bluray")));
                format!("Native {} · {}", ext.into_iter()
                    .map(|x|format!(".{x}")).collect::<Vec<_>>().join(" / "),
                    if unpack { "lossless PCM unpack or packet copy" }
                    else { "copy original packets" })
            } else {
                "WAV · source depth if lossless; 24-bit if lossy (labelled)".into()
            }
        },
        "Format" => match s.wav_container {
            NativeWavContainer::Automatic => "WAV · automatic RIFF/RF64 · channel mask".into(),
            NativeWavContainer::Riff => "RIFF WAV · 4 GB limit".into(),
            NativeWavContainer::Rf64 => "RF64 WAV · large-file capable".into(),
            NativeWavContainer::W64 => "W64 · 64-bit sizes".into(),
        },
        "Files" if s.contents.format == DiscFormat::Archive =>
            "One original file per checked member, including non-audio".into(),
        "Files" if s.contents.format == DiscFormat::CueImage =>
            "One source-format file per selected CUE track".into(),
        "Files" if s.contents.format == DiscFormat::TransportFile =>
            "Copy complete audio streams · no chapters available".into(),
        "Files" if s.contents.format == DiscFormat::Matroska =>
            (if s.file_per_program { "one native file per audio stream" }
             else { "one WAV per selected chapter (native copy if chapterless)" }).into(),
        "Files" => (if s.file_per_program {
            if s.cue && s.native_cue_applicable() { "one per area/group/title + CUE" } else { "one per area/group/title" }
        } else if matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay) {
            "one per chapter"
        } else { "one per track" }).into(),
        "Tags" if s.contents.format == DiscFormat::Archive =>
            (if s.tags {"on · tags for filenames only; originals untouched"}
             else {"off · preserve original member names"}).into(),
        "Tags" if !s.native_tags_available() => "off for DFF · no standard tags".into(),
        "Tags" if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile) && s.tags => {
            // The selected carrier, not the input MKV, determines whether
            // metadata survives a whole-stream packet copy. Chapter WAVs
            // retain the existing native metadata writer.
            let mut no_text = std::collections::BTreeSet::new();
            let mut no_picture = std::collections::BTreeSet::new();
            for (i, presentation) in s.contents.presentations.iter().enumerate() {
                if s.selected.get(i) != Some(&true) || !s.is_extractable(i) { continue; }
                let copy = s.file_per_program || matches!(presentation.id,
                    crate::disc::PresentationId::MatroskaStream { chapter_count: 0, .. });
                if !copy { continue; }
                let carrier = crate::convert::pipeline::native_extract::file_stream_copy_format(
                    s.contents.format,
                    presentation.format.codec.as_deref().unwrap_or("unknown"));
                if !carrier.text_tags { no_text.insert(carrier.extension); }
                if s.tag_artwork && !carrier.artwork { no_picture.insert(carrier.extension); }
            }
            if no_text.is_empty() && no_picture.is_empty() {
                format!("on · {} fields · {}", s.tag_fields.count_ones(),
                    if s.tag_artwork { "artwork" } else { "no artwork" })
            } else {
                // Put the limitation first, before any potential screen
                // truncation. Do not imply native .ac3/.aac contains art.
                let mut value = "on where supported".to_owned();
                if !no_text.is_empty() {
                    value.push_str(&format!(" · no tags: {}",
                        no_text.into_iter().map(|ext|format!(".{ext}"))
                            .collect::<Vec<_>>().join("/")));
                }
                if !no_picture.is_empty() {
                    value.push_str(&format!(" · no artwork: {}",
                        no_picture.into_iter().map(|ext|format!(".{ext}"))
                            .collect::<Vec<_>>().join("/")));
                }
                value
            }
        },
        "Tags" => if s.tags { format!("on · {} fields · {}{}",
            s.tag_fields.count_ones(),if s.tag_artwork {"artwork"}else{"no artwork"},
            if s.selected_sacd_tag_carriers().1 {" · DFF drops tags"}else{""}) }
            else { "off · no metadata written".into() },
        "Downmix" => s.downmix.as_str().to_string(),
        "Assume decrypted" => if s.assume_decrypted {"on · already decrypted"} else {"off"}.into(),
        _ => String::new(),
    }
}

/// The wide form uses a stable three-row pair of controls followed by full
/// width Folder and Naming fields. Medium and small layouts retain the same
/// logical tab order and scroll it one row at a time.
fn output_cell(name: &str, index: usize, rows: &[&str], area: Rect, wide: bool, focus: usize) -> Option<Rect> {
    if rows.is_empty() || area.height == 0 || area.width <= 4 { return None; }
    let text = Rect::new(area.x+2, area.y, area.width-4, area.height);
    if wide && text.width >= 88 {
        let gutter = 3;
        let left_width = (text.width-gutter)*55/100;
        let right_x = text.x+left_width+gutter;
        let right_width = text.width-left_width-gutter;
        let (x, y, width) = match name {
            "Format" => (text.x,0,left_width),
            "Overwrite" => (right_x,0,right_width),
            "Keep DST" => (right_x,1,right_width),
            "Files" => (text.x,2,left_width),
            "Tags" => (right_x,2,right_width),
            "Folder" => (text.x,3,text.width),
            "Naming" => (text.x,5,text.width),
            "Downmix" => (text.x,7,text.width),
            "Assume decrypted" => (text.x,8,text.width),
            _ => return None,
        };
        return (y < text.height).then(|| Rect::new(x,text.y+y,width,1));
    }
    let visual = |i:usize| {
        i + usize::from(i > 0)
            + usize::from(i > rows.iter().position(|row| *row == "Folder").unwrap_or(rows.len()))
            + usize::from(i > rows.iter().position(|row| *row == "Naming").unwrap_or(rows.len()))
    };
    let visible = text.height as usize;
    let total = rows.len()+3;
    let focused = visual(focus.min(rows.len().saturating_sub(1)))
        + usize::from(rows.get(focus).is_some_and(|name|
            *name == "Folder" || *name == "Naming"));
    let scroll = focused.saturating_sub(visible.saturating_sub(1))
        .min(total.saturating_sub(visible));
    let position = visual(index);
    if position < scroll || position >= scroll+visible { return None; }
    Some(Rect::new(text.x,text.y+(position-scroll) as u16,text.width,1))
}

const OVERWRITE_OPTIONS: [&str;3] = [
    "Stop if a file exists", "Skip existing files", "Replace existing files",
];
const OVERWRITE_HELP_GLYPH: &str = "?"; // One-cell fallback for fonts without ⓘ.
const OVERWRITE_EXPLANATIONS: [&str;3] = [
    "Nothing is written if any target exists.",
    "Existing files stay. The rest are written.",
    "Existing files are overwritten.",
];

fn overwrite_index(s: &DiscBrowserState) -> usize {
    use crate::convert::pipeline::native_extract::NativeOverwritePolicy as P;
    match s.overwrite_policy {P::Fail=>0,P::Skip=>1,P::Replace=>2}
}

/// Each option owns exactly its printed cells; repeated clicks do not depend
/// on the selected value's width or on neighboring text.
fn radio_focus_bg(band: Color, theme: Theme) -> Color {
    blend_color(band,theme.selection_bg,1,8)
}

fn draw_native_radio(frame: &mut ratatui::Frame, buttons: &mut ButtonRenderMap,
    field: Rect, options: &[(&str,bool)], focused: bool, format: bool,
    theme: Theme, background: Color) -> u16 {
    let mut x = field.x;
    let limit = field.x.saturating_add(field.width);
    for (i,(label,selected)) in options.iter().enumerate() {
        let text = format!("{} {}", if *selected {"●"} else {"○"}, label);
        let width = text.chars().count() as u16;
        if x.saturating_add(width)>limit { break; }
        let active = focused && *selected;
        let bg = if active { radio_focus_bg(background, theme) } else { background };
        let color = if *selected {theme.value} else {theme.label};
        let style = Style::default().bg(bg).fg(readable_word(color,bg))
            .add_modifier(if active { Modifier::BOLD } else {Modifier::empty()});
        let rect=Rect::new(x,field.y,width,1);
        if active {
            let glyph=if *selected {"●"}else{"○"};
            frame.render_widget(Paragraph::new(Line::from(vec![
                Span::styled(glyph,style.fg(readable_word(theme.info,bg))),
                Span::styled(format!(" {label}"),style),
            ])),rect);
        } else {
            frame.render_widget(Paragraph::new(text).style(style),rect);
        }
        buttons.record_button(if format {TuiButton::DiscBrowserFormatChoice(i)}
            else {TuiButton::DiscBrowserFilesChoice(i)},rect);
        x=x.saturating_add(width+2);
    }
    x
}

fn draw_native_toggle(frame:&mut ratatui::Frame, buttons:&mut ButtonRenderMap,
    field:Rect, text:&str, enabled:bool, focused:bool, index:usize,
    theme:Theme, bg:Color) -> u16 {
    let text=format!("{} {text}",check(enabled));
    let width=(text.chars().count() as u16).min(field.width);
    if width==0 {return field.x;}
    let control_bg=if focused {theme.input_focused_bg}else{bg};
    let style=Style::default().bg(control_bg).fg(readable_word(
        if focused {theme.value}else{theme.label},control_bg));
    let rect=Rect::new(field.x,field.y,width,1);
    frame.render_widget(Paragraph::new(clip_end(&text,width)).style(style),rect);
    buttons.record_button(TuiButton::DiscBrowserToggle(index),rect);
    field.x.saturating_add(width)
}

fn draw_native_pill(frame:&mut ratatui::Frame, buttons:&mut ButtonRenderMap,
    rect:Rect, label:&str, target:TuiButton, focused:bool,hovered:bool,theme:Theme) {
    if rect.width==0 {return;}
    let bg=if focused {theme.pill_active_bg}
        else if hovered {theme.pill_neutral_hover_bg}else{theme.pill_neutral_bg};
    let fg=readable_word(if focused {theme.pill_active_fg}else{theme.value},bg);
    frame.render_widget(Paragraph::new(clip_end(label,rect.width)).style(Style::default()
        .bg(bg).fg(fg)),rect);
    buttons.record_button(target,rect);
}

/// Render the existing shared TextInputState with a theme-derived recessed
/// well. The selection and the block caret use separate surfaces. A field with
/// focus but no editor has neither a caret nor a selection.
fn draw_native_input(frame:&mut ratatui::Frame, rect:Rect, text:&str,
    editing:Option<&super::text_input::TextInputState>,theme:Theme) {
    if rect.width==0 {return;}
    let well=if theme.dark {
        blend_color(theme.panel_bg,Color::Black,1,4)
    }else{blend_color(theme.panel_bg,theme.value,1,6)};
    let normal=Style::default().fg(readable_word(theme.value,well)).bg(well);
    fill(frame,rect,well);
    let content=if let Some(editor)=editing {
        // Selected text uses the same high-contrast value foreground as an
        // ordinary field, but inverted. The tinted caret stays visible at
        // either end of the selection, including after Ctrl+A.
        let selection_bg=readable_word(theme.value,well);
        let caret_bg=if editor.has_selection() {
            // The palette tint can share the selection's luminance (notably
            // on light themes). Prefer the contrasting pole in that case so
            // the block caret does not disappear against adjacent selection.
            let tint=readable_word(theme.selection_bg,well);
            let pole=if theme.dark {Color::White}else{Color::Black};
            if contrast_ratio(pole,well)>=4.5
                && contrast_ratio(pole,selection_bg)>contrast_ratio(tint,selection_bg) {
                pole
            }else{tint}
        }else{selection_bg};
        Line::from(tui_file_picker::text_input_spans_with_visible_selection_caret(
            editor,rect.width as usize,normal,
            Style::default().fg(well).bg(selection_bg),
            Style::default().fg(well).bg(caret_bg),true))
    } else {
        Line::from(Span::styled(clip_end(text,rect.width),normal))
    };
    frame.render_widget(Paragraph::new(content).style(normal),rect);
}

/// A read-only help sheet anchored under the fixed-position cycler.
/// Its click shield absorbs clicks outside without activating underlying rows.
fn draw_native_overwrite_menu(frame:&mut ratatui::Frame, buttons:&mut ButtonRenderMap,
    anchor:Rect, container:Rect, s:&DiscBrowserState, theme:Theme) {
    if !s.overwrite_open {return;}
    const H:u16=6;
    let width=66u16.min(container.width.saturating_sub(4));
    if width<30 || container.height<H+2 {return;}
    let x=anchor.x.min(container.right().saturating_sub(width+2)).max(container.x+1);
    let below=anchor.bottom();
    let y=if below+H<=container.bottom().saturating_sub(1) {below}
        else {anchor.y.saturating_sub(H)};
    let rect=Rect::new(x,y,width,H);
    // Dismiss from anywhere outside this sheet, including the dimmed Browse
    // region beyond the dialog. The passive sheet target is recorded last.
    buttons.record_button(TuiButton::DiscBrowserOverwriteHelpOutside,frame.size());
    buttons.record_button(TuiButton::DiscBrowserOverwriteHelpPassive,rect);
    frame.render_widget(Clear,rect);
    frame.render_widget(Block::default().borders(Borders::ALL).style(Style::default()
        .bg(theme.dropdown_bg).fg(readable_word(theme.value,theme.dropdown_bg)))
        .border_style(Style::default().fg(theme.label)),rect);
    let heading="Overwrite · Esc or click outside to close";
    frame.render_widget(Paragraph::new(clip_end(heading,width.saturating_sub(2)))
        .style(Style::default().fg(theme.info).bg(theme.dropdown_bg)),
        Rect::new(x+1,y+1,width.saturating_sub(2),1));
    for (index,description) in OVERWRITE_EXPLANATIONS.iter().enumerate() {
        let label=format!("{}: {}",OVERWRITE_OPTIONS[index],description);
        frame.render_widget(Paragraph::new(clip_end(&label,width.saturating_sub(4)))
            .style(Style::default().fg(readable_word(theme.value,theme.dropdown_bg))
                .bg(theme.dropdown_bg)),Rect::new(x+2,y+2+index as u16,width.saturating_sub(4),1));
    }
}

#[cfg(test)]
fn draw_output(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,
    buttons:&mut ButtonRenderMap,theme:Theme,wide:bool) {
    draw_output_with_hover(frame,s,r,buttons,theme,wide,None);
}

fn draw_output_with_hover(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,
    buttons:&mut ButtonRenderMap,theme:Theme,wide:bool,hover:Option<TuiButton>) {
    let rows=s.output_rows();
    let colors=extract_colors(theme);
    let bg=if s.focus==ExtractFocus::Output {colors.focused}else{colors.band};
    s.form_wide.set(wide && r.width.saturating_sub(4) >= 88);
    fill(frame,r,bg);
    let mut format_note=None;
    let mut folder_note=None;
    const LABEL_WIDTH:u16=10;
    for (i,name) in rows.iter().enumerate() {
        let Some(cell)=output_cell(name,i,&rows,r,wide,s.output_cursor) else{continue;};
        let focused=s.focus==ExtractFocus::Output && s.output_cursor==i
            && s.output_row_focusable(i);
        let label_width=LABEL_WIDTH.min(cell.width);
        let labels=word_tiers(theme,colors,bg);
        let label=if *name=="Assume decrypted" {"Decrypt"} else {name};
        let label_focused = focused && !s.editing_destination
            && matches!(*name,"Folder"|"Naming");
        frame.render_widget(Paragraph::new(format!("{label} ")).style(Style::default()
            .bg(bg).fg(if label_focused {readable_word(theme.info,bg)}else{labels.label})),
            Rect::new(cell.x,cell.y,label_width,1));
        let field=Rect::new(cell.x+label_width,cell.y,cell.width.saturating_sub(label_width),1);
        if field.width==0 {continue;}
        match *name {
            "Format" => {
                if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile | DiscFormat::CueImage | DiscFormat::Archive) {
                    frame.render_widget(Paragraph::new(clip_end(&option_value(s,"Format"),field.width))
                        .style(Style::default().fg(readable_word(theme.value,bg)).bg(bg)),field);
                } else if s.contents.format==DiscFormat::Sacd {
                    let options=[("DSF",!s.sacd_dff),("DFF",s.sacd_dff)];
                    draw_native_radio(frame,buttons,field,&options,focused,true,theme,bg);
                }else{
                    let options=[("Automatic",s.wav_container==NativeWavContainer::Automatic),
                        ("WAV",s.wav_container==NativeWavContainer::Riff),
                        ("RF64",s.wav_container==NativeWavContainer::Rf64),
                        ("W64",s.wav_container==NativeWavContainer::W64)];
                    draw_native_radio(frame,buttons,field,&options,focused,true,theme,bg);
                }
                format_note=Some((cell.x+LABEL_WIDTH,cell.y+1,cell.width.saturating_sub(LABEL_WIDTH)));
            }
            "Overwrite" => {
                // Fixed 22-cell value: every arrow has a stable hitbox across
                // all three policies, including when repeatedly clicked.
                const VALUE_WIDTH:u16=22;
                const CONTROL_WIDTH:u16=28;
                if field.width<CONTROL_WIDTH {continue;}
                let well=if focused {theme.input_focused_bg}else{theme.input_unfocused_bg};
                fill(frame,Rect::new(field.x,field.y,26,1),well);
                let value=format!("{:<22}",OVERWRITE_OPTIONS[overwrite_index(s)]);
                let style=Style::default().fg(readable_word(theme.value,well)).bg(well);
                frame.render_widget(Paragraph::new("◂").style(Style::default()
                    .fg(if focused {theme.info}else{theme.label}).bg(well)),
                    Rect::new(field.x,field.y,1,1));
                frame.render_widget(Paragraph::new(value).style(style),
                    Rect::new(field.x+2,field.y,VALUE_WIDTH,1));
                frame.render_widget(Paragraph::new("▸").style(Style::default()
                    .fg(if focused {theme.info}else{theme.label}).bg(well)),
                    Rect::new(field.x+25,field.y,1,1));
                frame.render_widget(Paragraph::new(OVERWRITE_HELP_GLYPH).style(Style::default()
                    .fg(theme.info).bg(bg)),Rect::new(field.x+27,field.y,1,1));
                buttons.record_button(TuiButton::DiscBrowserOverwritePrev,Rect::new(field.x,field.y,1,1));
                buttons.record_button(TuiButton::DiscBrowserOverwrite,Rect::new(field.x+2,field.y,VALUE_WIDTH,1));
                buttons.record_button(TuiButton::DiscBrowserOverwriteNext,Rect::new(field.x+25,field.y,1,1));
                buttons.record_button(TuiButton::DiscBrowserOverwriteHelp,Rect::new(field.x+27,field.y,1,1));
            }
            "Keep DST" => {
                let enabled = s.contents.presentations.iter().enumerate().any(|(i,p)|
                    s.selected.get(i) == Some(&true)
                        && super::disc_browser_actions::presentation_has_dst(p)
                        && s.keep_dst.get(i) == Some(&true));
                let names=s.contents.presentations.iter().enumerate()
                    .filter(|(index,p)|s.selected.get(*index)==Some(&true)
                        && s.items_checked.get(*index).is_some_and(|items|items.iter().any(|yes|*yes))
                        && super::disc_browser_actions::presentation_has_dst(p))
                    .map(|(_,p)|p.id.compact_label()).collect::<Vec<_>>();
                let label=if names.is_empty() {"Keep DST in selected areas".to_owned()}
                    else {format!("Keep DST in {}",names.join(" and "))};
                draw_native_toggle(frame,buttons,field,&label,enabled,
                    focused,i,theme,bg);
            }
            "Files" => {
                let (item,unit)=match s.contents.format {
                    DiscFormat::Sacd=>("track","area"),DiscFormat::DvdAudio=>("track","group"),
                    DiscFormat::DvdVideo=>("chapter","title"),DiscFormat::BluRay=>("chapter","playlist"),
                    DiscFormat::Matroska=>("chapter","stream"),
                    DiscFormat::TransportFile=>("stream","stream"),
                    DiscFormat::CueImage=>("track","album"),
                    DiscFormat::Archive=>("file","archive"),
                };
                let individual=if s.contents.format==DiscFormat::Matroska {
                    "Split chapters".to_owned()
                }else{format!("One per {item}")};
                let grouped=if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile) {
                    "Copy stream".to_owned()
                }else{format!("One per {unit}")};
                if matches!(s.contents.format,DiscFormat::TransportFile | DiscFormat::CueImage | DiscFormat::Archive) {
                    frame.render_widget(Paragraph::new(clip_end(&option_value(s,"Files"),field.width))
                        .style(Style::default().fg(readable_word(theme.value,bg)).bg(bg)),field);
                } else {
                    let options=[(individual.as_str(),!s.file_per_program),(grouped.as_str(),s.file_per_program)];
                    draw_native_radio(frame,buttons,field,&options,focused,false,theme,bg);
                }
            }
            "Tags" => {
                let (dsf,dff)=s.selected_sacd_tag_carriers();
                let x=if dff && !dsf {field.x} else {
                    draw_native_toggle(frame,buttons,field,if s.contents.format==DiscFormat::Archive {"Use tags"}else{"Transfer"},s.tags,focused,i,theme,bg)
                };
                let edit=" Edit… ";
                let ew=edit.chars().count() as u16;
                if field.width>=ew {
                    let button_x=field.x+field.width-ew;
                    let summary=if dff && !dsf {"off for DFF".to_owned()}
                        else if s.tags && dff {"DSF only; DFF none".to_owned()}
                        else if s.tags {format!("{} fields · {}",s.tag_fields.count_ones(),
                            if s.tag_artwork {"artwork"}else{"no artwork"})}
                        else {"no tags".to_owned()};
                    if button_x>x+1 {
                        frame.render_widget(Paragraph::new(clip_end(&summary,button_x-x-1))
                            .style(Style::default().fg(if dff {theme.warning}
                                else {readable_word(theme.text_dim,bg)}).bg(bg)),
                            Rect::new(x+1,field.y,button_x-x-1,1));
                    }
                    draw_native_pill(frame,buttons,Rect::new(button_x,field.y,ew,1),edit,
                        TuiButton::DiscBrowserTagsEdit,focused,
                        hover==Some(TuiButton::DiscBrowserTagsEdit),theme);
                }
            }
            "Folder" => {
                let pill=" Browse… ";
                let pw=pill.chars().count() as u16;
                let input_width=field.width.saturating_sub(pw+2);
                let input=Rect::new(field.x,field.y,input_width,1);
                let editing=s.editing_destination && !s.editing_template;
                let value=compact_folder_path(&s.destination,input_width);
                draw_native_input(frame,input,&value,
                    editing.then_some(s.text_edit.as_ref()).flatten(),theme);
                folder_note=Some((cell.x+LABEL_WIDTH,cell.y+1,cell.width.saturating_sub(LABEL_WIDTH)));
                buttons.record_button(TuiButton::DiscBrowserFolderField,input);
                if field.width>=pw+3 {
                    draw_native_pill(frame,buttons,Rect::new(field.x+input_width+2,field.y,pw,1),
                        pill,TuiButton::DiscBrowserFolderBrowse,false,
                        hover==Some(TuiButton::DiscBrowserFolderBrowse),theme);
                }
            }
            "Naming" => {
                let editing=s.editing_destination && s.editing_template;
                let value=s.naming_template.as_str();
                let load=" Load template ▾ ";
                let custom=" Custom… ";
                let load_width=load.chars().count() as u16;
                let custom_width=custom.chars().count() as u16;
                let pills_width=load_width+custom_width+2;
                let show_pills=field.width>=pills_width+8;
                let input_width=if show_pills {field.width-pills_width} else {field.width};
                let input=Rect::new(field.x,field.y,input_width.min(field.width),1);
                draw_native_input(frame,input,value,
                    editing.then_some(s.text_edit.as_ref()).flatten(),theme);
                buttons.record_button(TuiButton::DiscBrowserNaming,input);
                if show_pills {
                    let first=Rect::new(field.x+input_width+1,field.y,load_width,1);
                    draw_native_pill(frame,buttons,first,load,
                        TuiButton::DiscBrowserNamingLoad,false,
                        hover==Some(TuiButton::DiscBrowserNamingLoad),theme);
                    let second=Rect::new(first.x+load_width+1,field.y,custom_width,1);
                    draw_native_pill(frame,buttons,second,custom,
                        TuiButton::DiscBrowserNamingCustom,false,
                        hover==Some(TuiButton::DiscBrowserNamingCustom),theme);
                }
            }
            "Downmix" => {
                // Reserve the longest policy name, regardless of the active
                // value. On narrow screens keep both arrows visible and clip
                // only the label; the mouse targets do not move as it cycles.
                let maximum = super::disc_browser_actions::NATIVE_DVDA_DOWNMIX_POLICIES.iter()
                    .map(|policy| super::display_width::width(policy.as_str()) as u16)
                    .max().unwrap_or(0);
                let well=if focused {theme.input_focused_bg}else{theme.input_unfocused_bg};
                buttons.record_button(TuiButton::DiscBrowserToggle(i),field);
                if field.width>=5 {
                    let value_width=maximum.min(field.width.saturating_sub(4));
                    let next_x=field.x+value_width+3;
                    fill(frame,Rect::new(field.x,field.y,value_width+4,1),well);
                    frame.render_widget(Paragraph::new("◂").style(Style::default()
                        .fg(if focused {theme.info}else{theme.label}).bg(well)),
                        Rect::new(field.x,field.y,1,1));
                    frame.render_widget(Paragraph::new(clip_end(s.downmix.as_str(),value_width))
                        .style(Style::default().fg(readable_word(theme.value,well)).bg(well)),
                        Rect::new(field.x+2,field.y,value_width,1));
                    frame.render_widget(Paragraph::new("▸").style(Style::default()
                        .fg(if focused {theme.info}else{theme.label}).bg(well)),
                        Rect::new(next_x,field.y,1,1));
                    buttons.record_button(TuiButton::DiscBrowserDownmixPrev,
                        Rect::new(field.x,field.y,1,1));
                    buttons.record_button(TuiButton::DiscBrowserDownmixNext,
                        Rect::new(next_x,field.y,1,1));
                } else {
                    frame.render_widget(Paragraph::new(clip_end(s.downmix.as_str(),field.width))
                        .style(Style::default().fg(readable_word(theme.value,well)).bg(well)),field);
                }
            }
            "Assume decrypted" => {
                let value=format!("{} Assume decrypted",check(s.assume_decrypted));
                let well=if focused {theme.input_focused_bg}else{theme.input_unfocused_bg};
                frame.render_widget(Paragraph::new(clip_end(&value,field.width))
                    .style(Style::default().fg(readable_word(theme.value,well)).bg(well)),field);
                buttons.record_button(TuiButton::DiscBrowserToggle(i),field);
            }
            _ => {},
        }
    }
    if let Some((x,y,width))=format_note {
        if y<r.y+r.height && width>0 {
            let (dsf,dff)=s.selected_sacd_tag_carriers();
            let (message,warning)=if s.contents.format==DiscFormat::Sacd {
                if dff && dsf {("DSF keeps tags; DFF has no standard tags",true)}
                else if dff && s.keep_dst.iter().any(|keep|*keep) {
                    ("DFF has no standard tags; DST kept",true)
                }
                else if dff {("DFF has no standard tags",true)}
                else {("DSF keeps tags and artwork",false)}
            } else if s.contents.format == DiscFormat::Archive {
                ("Archive members are copied without modification",false)
            } else if s.contents.format == DiscFormat::CueImage {
                ("Lossy packets copied; lossless sample-exact",false)
            } else if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile) {
                // File-backed WAV outputs choose RIFF/RF64 automatically for
                // both PCM stream copies and decoded chapter splits.
                ("WAV: automatic RF64 past 4 GiB",false)
            } else { match s.wav_container {
                NativeWavContainer::Automatic => ("Automatic: RF64 past 4 GiB",false),
                NativeWavContainer::Riff => ("WAV stops at 4 GiB",true),
                NativeWavContainer::Rf64 => ("RF64 handles large WAV files",false),
                NativeWavContainer::W64 => ("W64 Wave64 container",false),
            }};
            frame.render_widget(Paragraph::new(clip_end(message,width))
                .style(Style::default().bg(bg).fg(if warning {theme.warning}else{colors.soft})),
                Rect::new(x,y,width,1));
        }
    }
    if let Some((x,y,width))=folder_note {
        if y<r.y+r.height && width>0 {
            use super::disc_browser::DestinationFeedback as D;
            let layout=s.native_output_layout_hint();
            let message=match s.destination_feedback {
                Some(D::WillBeCreated) if layout=="Files go directly into this folder." =>
                    "This folder will be created; files go directly into it.".to_owned(),
                Some(D::WillBeCreated)=>format!("This folder will be created; {layout}"),
                Some(D::NotWritable)=>"not writable".to_owned(),
                Some(D::InvalidPath)=>"enter an absolute directory path".to_owned(),
                None=>layout,
            };
            let color=if matches!(s.destination_feedback,Some(D::NotWritable|D::InvalidPath)) {
                theme.error
            }else{colors.soft};
            frame.render_widget(Paragraph::new(clip_end(&message,width))
                .style(Style::default().bg(bg).fg(color)),Rect::new(x,y,width,1));
        }
    }
    // The worker-resolved destination remains authoritative. On overflow the
    // author's folder and filename take priority over the absolute root.
    if let Some(index)=rows.iter().position(|name|*name=="Naming") {
        if let Some(naming)=output_cell("Naming",index,&rows,r,wide,s.output_cursor) {
            let y=naming.y.saturating_add(1);
            if y<r.y+r.height && naming.width>2 {
                let template=if s.editing_destination && s.editing_template {
                    s.destination_input.as_str()
                } else { s.naming_template.as_str() };
                let invalid=crate::convert::pipeline::native_extract::unknown_native_naming_token(
                    template);
                // Neither a collision nor an invalid token may replace the
                // highlighted item's live destination path. The separate
                // result caption communicates the warning/blocker.
                let preview=s.native_preview_path_for_template(template)
                    .map(|path|super::template_builder::destination_path_preview(
                        &path,&s.destination,naming.width.saturating_sub(2) as usize,
                        || s.native_preview_naming_for_template_case(
                            template,s.capitalize_naming)))
                    .unwrap_or_else(|| "Select a playable track to preview its path".into());
                let color=if invalid.is_some() {theme.warning} else {colors.soft};
                super::template_builder::draw_naming_preview(frame,
                    Rect::new(naming.x,naming.y+1,naming.width,1),
                    &preview,Style::default().bg(bg).fg(color));
            }
        }
    }
}

/// A result caption, not a selectable setting. The only highlighted text
/// here is an extraction blocker. Working space is shown only when separate.
fn draw_results(frame:&mut ratatui::Frame,s:&DiscBrowserState,
    scratch:&Path,r:Rect,blocker:Option<&str>,theme:Theme) {
    if r.height==0 {return;}
    let items=s.selected_item_count();
    let programs=s.selected_program_count();
    let size=if s.contents.format==DiscFormat::TransportFile {
        s.selected_item_bytes().map_or_else(||"audio size unknown".into(),
            |(total,_,_)|format!("~{} audio",format_bytes(total)))
    } else {
        s.selected_item_bytes().map_or_else(||"pending".into(),|(total,_,_)|format_bytes(total))
    };
    let effective_items=if matches!(s.contents.format, DiscFormat::Matroska | DiscFormat::TransportFile)
        && s.file_per_program {programs}else{items};
    let kind=if s.contents.format==DiscFormat::Archive {
        if effective_items==1 {"file"}else{"files"}
    }else if s.contents.format==DiscFormat::TransportFile {
        if effective_items == 1 {"stream"}else{"streams"}
    } else { match (matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay|DiscFormat::Matroska),effective_items) {
        (true,1)=>"chapter",(true,_)=>"chapters",(false,1)=>"track",(false,_)=>"tracks",
    }};
    let group=match s.contents.format {
        DiscFormat::Sacd=>if programs==1 {"area"}else{"areas"},
        DiscFormat::DvdAudio=>if programs==1 {"group"}else{"groups"},
        DiscFormat::DvdVideo=>if programs==1 {"title"}else{"titles"},
        DiscFormat::BluRay=>if programs==1 {"playlist"}else{"playlists"},
        DiscFormat::Matroska|DiscFormat::TransportFile=>if programs==1 {"stream"}else{"streams"},
        DiscFormat::CueImage=>if programs==1 {"album"}else{"albums"},
        DiscFormat::Archive=>"archive",
    };
    let colors=extract_colors(theme);
    let words=word_tiers(theme,colors,colors.base);
    fill(frame,r,colors.base);
    let template=if s.editing_destination && s.editing_template {
        s.destination_input.as_str()
    } else { s.naming_template.as_str() };
    let unknown=crate::convert::pipeline::native_extract::unknown_native_naming_token(template);
    let caption=if let Some(blocker)=blocker {format!("⚠ {blocker}")}
        else if let Some(token)=unknown.as_deref() {
            format!("▲ Unknown naming token: {token}")
        } else {
        let log=if s.extraction_log {" + log"}else{""};
        let dest=space_for(&s.destination).map_or_else(||"unknown".into(),format_bytes);
        let base=format!("{items} {kind}{log} · {programs} {group} · {size}");
        if s.native_storage_shares_destination_volume(scratch) {
            format!("{base} · destination {dest} free")
        } else {
            let available=space_for(scratch).map_or_else(||"unknown".into(),format_bytes);
            let capacities=format!("destination {dest} free · working {available} free");
            let detailed=format!("{base} · {capacities}");
            let compact=format!("{items} {kind}{log} · {size} · {capacities}");
            // A popup may be narrower than the terminal. Remove optional
            // selection details before sacrificing either volume's capacity.
            let width=r.width.saturating_sub(4) as usize;
            if detailed.chars().count()<=width { detailed }
            else if compact.chars().count()<=width { compact }
            else { capacities }
        }
    };
    frame.render_widget(Paragraph::new(clip_end(&caption,r.width.saturating_sub(4)))
        .style(Style::default().bg(colors.base).fg(if blocker.is_some(){theme.error}
            else if unknown.is_some(){theme.warning}else{words.soft})),
        Rect::new(r.x+2,r.y,r.width.saturating_sub(4),1));
}

fn draw_footer(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,buttons:&mut ButtonRenderMap,small:bool,can_extract:bool,theme:Theme) {
    if r.height==0 {return;}
    let colors=extract_colors(theme);
    let words=word_tiers(theme,colors,colors.base);
    fill(frame,r,colors.base);
    if !small {
        let hints=super::disc_browser_actions::native_key_hints(s);
        // Keep the help key visible at the right margin, even when the other
        // contextual hints cannot all fit in a medium-width terminal.
        let help_index=hints.iter().position(|hint| *hint == "? all keys");
        let help_width=help_index.map_or(0,|i|hints[i].chars().count() as u16);
        let right=r.x.saturating_add(r.width).saturating_sub(2);
        let help_x=right.saturating_sub(help_width);
        let mut x=r.x+2;
        for (i,hint) in hints.iter().enumerate().filter(|(i,_)|Some(*i)!=help_index) {
            let token=format!("{}{}",if i==0 {""}else{" · "},hint);
            let width=token.chars().count() as u16;
            if x.saturating_add(width+3)>help_x { break; }
            let clickable=super::disc_browser_actions::native_hint_clickable(s,i);
            let key=token.find(' ').unwrap_or(token.len());
            let key_style=Style::default().fg(theme.info);
            // Interactive key hints use the info accent, never an underline.
            let spans=vec![Span::styled(token[..key].to_owned(),key_style),
                Span::styled(token[key..].to_owned(),Style::default().fg(words.label))];
            frame.render_widget(Paragraph::new(Line::from(spans)),Rect::new(x,r.y,width,1));
            if clickable {
                buttons.record_button(TuiButton::DiscBrowserHint(i),Rect::new(x,r.y,width,1));
            }
            x=x.saturating_add(width);
        }
        if help_x>=x && help_x+help_width<=right && help_width>0 {
            let index=help_index.expect("only a real help binding reserves space");
            frame.render_widget(Paragraph::new(hints[index])
                .style(Style::default().fg(theme.info)),
                Rect::new(help_x,r.y,help_width,1));
            buttons.record_button(TuiButton::DiscBrowserHint(index),
                Rect::new(help_x,r.y,help_width,1));
        }
    }
    let y=r.y+r.height-1;
    let chips=[("^x Extract",TuiButton::DiscBrowserExtract,can_extract,theme.chip_go),
        ("c Convert…",TuiButton::DiscBrowserConvert,!matches!(s.contents.format,DiscFormat::CueImage | DiscFormat::Archive),theme.tab_active),
        ("m Options",TuiButton::DiscBrowserOptions,true,theme.warning),
        ("Esc Close",TuiButton::DiscBrowserClose,true,theme.chip_dismiss)];
    let length:usize=chips.iter().map(|(txt,_,_,_)|txt.chars().count()+2).sum();
    let mut x=r.x+r.width.saturating_sub(length as u16)/2;
    for (index,(label,button,enabled,color)) in chips.into_iter().enumerate() {
        let text=format!(" {label} ");
        let width=text.chars().count() as u16;
        if x+width>r.x+r.width {break;}
        let active=enabled && s.focus==ExtractFocus::Footer && s.footer_cursor==index;
        let chip_bg=if !enabled {theme.text_dim}
            else if active {theme.pill_active_bg}else{color};
        let fg=if active {theme.pill_active_fg}else{theme.panel_bg};
        let style=Style::default().bg(chip_bg).fg(readable_word(fg,chip_bg))
            .add_modifier(if active {Modifier::BOLD}else{Modifier::empty()});
        frame.render_widget(Paragraph::new(text).style(style),Rect::new(x,y,width,1));
        if enabled { buttons.record_button(button,Rect::new(x,y,width,1)); }
        x=x.saturating_add(width+2);
    }
}

/// A real, modal Tags sheet: every displayed control has a matching mouse
/// target and exactly the same action as Space/Enter. The scroll is derived
/// from the active control; no filesystem work occurs during repaint.
#[cfg(test)]
fn draw_tags_sheet(frame:&mut ratatui::Frame, s:&DiscBrowserState, popup:Rect,
    buttons:&mut ButtonRenderMap, theme:Theme) {
    draw_tags_sheet_with_hover(frame,s,popup,buttons,theme,None);
}

fn draw_tags_sheet_with_hover(frame:&mut ratatui::Frame, s:&DiscBrowserState, popup:Rect,
    buttons:&mut ButtonRenderMap, theme:Theme,hover:Option<TuiButton>) {
    let width=popup.width.saturating_sub(2).min(82);
    let height=popup.height.saturating_sub(2).min(27);
    if width<12 || height<7 { return; }
    let area=Rect::new(popup.x+(popup.width-width)/2,popup.y+(popup.height-height)/2,width,height);
    // Attenuate the rendered Extract window before the sheet is painted. This
    // is presentation-only; no additional drawing or layout pass is needed.
    for y in popup.y..popup.y.saturating_add(popup.height) {
        for x in popup.x..popup.x.saturating_add(popup.width) {
            let cell=frame.buffer_mut().get_mut(x,y);
            cell.set_fg(blend_color(cell.fg,cell.bg,3,4));
        }
    }
    // Register after the main overlay's controls, so the sheet is truly modal.
    buttons.record_button(TuiButton::DiscBrowserTagSheetOutside,frame.size());
    buttons.record_button(TuiButton::DiscBrowserTagSheetPassive,area);
    frame.render_widget(Clear,area);
    fill(frame,area,theme.surface);
    let outline=Block::default().borders(Borders::ALL).style(Style::default()
        .bg(theme.surface).fg(readable_word(theme.value,theme.surface)))
        .border_style(Style::default().fg(theme.border));
    frame.render_widget(outline,area);
    let title=Rect::new(area.x+1,area.y+1,area.width.saturating_sub(2),1);
    fill(frame,title,theme.title);
    frame.render_widget(Paragraph::new(" ▸ Tags ").style(Style::default()
        .fg(readable_word(theme.panel_bg,theme.title)).bg(theme.title)
        .add_modifier(Modifier::BOLD)),title);

    let controls=s.tag_sheet_controls();
    // Tuple: focus control index, optional mouse target, label, foreground,
    // dark-field styling. Headings are not focus stops.
    let mut rows:Vec<(Option<usize>,Option<TuiButton>,String,Color,bool)>=Vec::new();
    let preview=s.tag_preview_for_highlight();
    let values=[&preview.title,&preview.artist,&preview.album,&preview.genre,
        &preview.year,&preview.composer,&preview.performer,&preview.catalog,&preview.publisher];
    for (index,control) in controls.iter().copied().enumerate() {
        use TagSheetControl as C;
        let target=Some(TuiButton::DiscBrowserTagSheetControl(index));
        let item=match control {
            C::Transfer=>format!("{} {}",check(s.tags),
                if s.contents.format==DiscFormat::Archive {"Use tags for filenames"}
                else {"Transfer tags"}),
            C::Source=>format!("Source     ▕ {}  ▾▏",
                s.tag_source_label(s.tag_source_index())),
            C::Field(i)=>{
                if i==0 {
                    let number=s.contents.presentations.get(s.cursor)
                        .and_then(|p|p.tracks.get(s.item_cursor)).map_or(1,|t|t.number);
                    let scope=if s.file_per_program { "values for area/group output".to_owned() }
                        else { format!("values for track {number:02}") };
                    rows.push((None,None,format!("FIELDS                     {scope}"),theme.header,false));
                }
                let value=if s.tag_preview_is_pending() { "from source on extraction" }
                    else { values.get(i).and_then(|v|v.as_deref()).unwrap_or("no value") };
                format!("{} {:<11} {}",check(s.tag_fields & (1u16<<i)!=0),
                    NATIVE_TAG_FIELD_LABELS[i],value)
            },
            C::All=>String::new(), // The two action pills share this row.
            C::None=>continue,
            C::Artwork=>{
                rows.push((None,None,"ARTWORK".to_owned(),theme.header,false));
                // The materializer's source artwork outranks a folder cover;
                // that evidence is not available until preparation. Avoid
                // naming a fallback as if it were the selected embedded image.
                let provenance=s.tag_preview_artwork.as_ref().map(|p|format!("source art preferred · {} folder fallback",
                    p.file_name().map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "cover".to_owned())))
                    .unwrap_or_else(||"source art preferred · no folder fallback".to_owned());
                format!("{} Embed artwork   {}",check(s.tag_artwork),provenance)
            },
            C::UseDsf=>"▲ DFF has no standard tags.".to_owned(),
            C::EditValues | C::MusicBrainz | C::Done=>continue,
        };
        let fg=if matches!(control,C::UseDsf) {theme.warning}
            else if matches!(control,C::Field(_) if s.tag_preview_is_pending()) {theme.text_dim}
            else if matches!(control,C::Field(i) if values[i].is_none()) {theme.text_dim}
            else {theme.value};
        rows.push((Some(index),target,item,fg,matches!(control,C::Source)));
        if matches!(control,C::Source) && s.tag_source_open {
            for i in 0..s.tag_source_option_count() {
                let text=s.tag_source_label(i);
                rows.push((Some(index),Some(TuiButton::DiscBrowserTagSheetSourceChoice(i)),
                    format!("  {} {text}",if s.tag_source_cursor==i {"●"}else{"○"}),theme.value,true));
            }
        }
    }
    let body=Rect::new(area.x+2,area.y+2,area.width.saturating_sub(4),area.height.saturating_sub(6));
    let layout_focus=if controls.get(s.panel_cursor)==Some(&TagSheetControl::None) {
        controls.iter().position(|item|*item==TagSheetControl::All).unwrap_or(s.panel_cursor)
    } else {s.panel_cursor};
    let active=if s.tag_source_open {
        rows.iter().position(|(_,target,_,_,_)| *target ==
            Some(TuiButton::DiscBrowserTagSheetSourceChoice(s.tag_source_cursor)))
    } else {
        rows.iter().position(|(focus,_,_,_,_)| *focus == Some(layout_focus))
    }.unwrap_or(0);
    let scroll=active.saturating_sub(body.height.saturating_sub(1) as usize)
        .min(rows.len().saturating_sub(body.height as usize));
    for (visible,(index,target,label,fg,field)) in rows.iter().skip(scroll)
        .take(body.height as usize).enumerate() {
        let rect=Rect::new(body.x,body.y+visible as u16,body.width,1);
        let control=index.and_then(|i|controls.get(i)).copied();
        if control==Some(TagSheetControl::All) {
            fill(frame,rect,theme.surface);
            let mut end=rect.x+rect.width;
            for (action,text) in [(TagSheetControl::None," None "),
                (TagSheetControl::All," All ")] {
                let Some(action_index)=controls.iter().position(|item|*item==action) else {continue;};
                let width=text.chars().count() as u16;
                if end<rect.x+width {break;}
                end-=width;
                let action_rect=Rect::new(end,rect.y,width,1);
                let focused=!s.tag_source_open && s.panel_cursor==action_index;
                let bg=if focused {theme.pill_active_bg}
                    else if hover==Some(TuiButton::DiscBrowserTagSheetControl(action_index)) {
                        theme.pill_neutral_hover_bg
                    } else {theme.pill_neutral_bg};
                let fg=if focused {theme.pill_active_fg}else{theme.value};
                frame.render_widget(Paragraph::new(text).style(Style::default()
                    .fg(readable_word(fg,bg)).bg(bg)),action_rect);
                buttons.record_button(TuiButton::DiscBrowserTagSheetControl(action_index),action_rect);
                end=end.saturating_sub(2);
            }
            continue;
        }
        if control==Some(TagSheetControl::UseDsf) {
            fill(frame,rect,theme.surface);
            let text=" Use DSF ";
            let width=(text.chars().count() as u16).min(rect.width);
            let action=Rect::new(rect.x+rect.width-width,rect.y,width,1);
            let focused=!s.tag_source_open && *index==Some(s.panel_cursor);
            let bg=if focused {theme.pill_active_bg}
                else if index.is_some_and(|i| hover==Some(TuiButton::DiscBrowserTagSheetControl(i))) {
                    theme.pill_neutral_hover_bg
                } else {theme.pill_neutral_bg};
            let fg=if focused {theme.pill_active_fg}else{theme.value};
            if action.x>rect.x {
                frame.render_widget(Paragraph::new(clip_end(label,action.x-rect.x))
                    .style(Style::default().fg(readable_word(theme.warning,theme.surface))
                    .bg(theme.surface)),Rect::new(rect.x,rect.y,action.x-rect.x,1));
            }
            frame.render_widget(Paragraph::new(text).style(Style::default()
                .fg(readable_word(fg,bg)).bg(bg)),action);
            if let Some(target)=target {buttons.record_button(*target,action);}
            continue;
        }
        let focused=match target {
            Some(TuiButton::DiscBrowserTagSheetSourceChoice(choice)) =>
                s.tag_source_open && *choice == s.tag_source_cursor,
            _ => !s.tag_source_open && *index == Some(s.panel_cursor),
        };
        let bg=if focused {theme.input_focused_bg}
            else if *field {theme.input_unfocused_bg}else{theme.surface};
        fill(frame,rect,bg);
        let fg=if focused {readable_word(theme.value,bg)}else{readable_word(*fg,bg)};
        frame.render_widget(Paragraph::new(clip_end(label,rect.width))
            .style(Style::default().fg(fg).bg(bg)
                .add_modifier(if focused {Modifier::BOLD}else{Modifier::empty()})),rect);
        if let Some(target)=target {buttons.record_button(*target,rect);}
    }
    let hint_y=area.y+area.height-3;
    let hint=Rect::new(area.x+2,hint_y,area.width.saturating_sub(4),1);
    frame.render_widget(Paragraph::new(clip_end("↑↓ move · Space toggle · Enter select · Esc close",hint.width))
        .style(Style::default().fg(readable_word(theme.text_dim,theme.surface))
        .bg(theme.surface)),hint);
    let footer=Rect::new(area.x+2,area.y+area.height-2,area.width.saturating_sub(4),1);
    let mut x=footer.x.saturating_add(footer.width);
    for (control,text) in [(TagSheetControl::Done," Done "),
        (TagSheetControl::MusicBrainz," MusicBrainz… "),
        (TagSheetControl::EditValues," Edit values… ")] {
        let Some(index)=controls.iter().position(|item|*item==control) else {continue;};
        let width=text.chars().count() as u16;
        if x<footer.x.saturating_add(width) {continue;}
        x=x.saturating_sub(width);
        let rect=Rect::new(x,footer.y,width,1);
        let focused=s.panel_cursor==index;
        let bg=if focused {theme.pill_active_bg}
            else if control==TagSheetControl::Done {theme.chip_go}
            else if hover==Some(TuiButton::DiscBrowserTagSheetControl(index)) {
                theme.pill_neutral_hover_bg
            } else {theme.pill_neutral_bg};
        let fg=if focused {theme.pill_active_fg}
            else if control==TagSheetControl::Done {theme.panel_bg}else{theme.value};
        frame.render_widget(Paragraph::new(text).style(Style::default()
            .fg(readable_word(fg,bg)).bg(bg)),rect);
        buttons.record_button(TuiButton::DiscBrowserTagSheetControl(index),rect);
        x=x.saturating_sub(2);
    }
}

/// These strings are the sole text authority for the Options controls.
/// Every choice has a separate keyboard stop and mouse hit target.
fn extract_option_display(s:&DiscBrowserState,control:ExtractOptionControl) -> (String,String) {
    use ExtractOptionControl as C;
    use crate::convert::pipeline::native_extract::{NativeCueBetweenGap as B,NativeCueFirstGap as F};
    let settings=&s.extract_settings;
    let mark=check;
    let radio=|on:bool| if on {"●"} else {"○"};
    match control {
        C::WriteLog => (format!("{} Write extraction.log",mark(s.extraction_log)),
            "in destination folder, beside the tracks".into()),
        C::Verify => (format!("{} Verify after extraction",mark(settings.verify_after)),
            "reads each file back and checks its audio".into()),
        C::Chapters => ("Chapters…".into(),format!("chapters.txt {} · for single-file output",
            if settings.write_chapters {"on"}else{"off"})),
        C::CueFiles => ("CUE files…".into(),format!("create {} · embed {} · pre {} · {} · {}",
            if s.cue {"on"}else{"off"},if settings.embed_cue {"on"}else{"off"},
            if settings.keep_preemphasis {"on"}else{"off"},
            match settings.between_gap {B::AppendPrevious=>"append",B::PrependNext=>"prepend",B::Drop=>"drop"},
            match settings.first_gap {F::PrependFirst=>"prepend",F::OwnTrack00=>"00",F::Drop=>"drop"})),
        C::CopyFolder => ("Copy folder contents…".into(),format!("{} of 10 types · for CUE images",
            settings.copy_folder.enabled_count())),
        C::SaveNext => (format!("{} Use these settings next time",mark(s.save_extract_defaults)),
            "saved when you press Extract".into()),
        C::WriteChapters => (format!("{} Write chapters.txt",mark(settings.write_chapters)),
            "beside the output file".into()),
        C::CreateCue => (format!("{} Create CUE file",mark(s.cue)),
            "lists where each track starts".into()),
        C::EmbedCue => (format!("{} Embed CUE in the file",mark(settings.embed_cue)),
            "stored inside the audio file itself".into()),
        C::KeepPreemphasis => (format!("{} Keep pre-emphasis flags",mark(settings.keep_preemphasis)),
            "marks tracks mastered with pre-emphasis".into()),
        C::BetweenAppend => (format!("{} Between tracks: Append to previous",radio(settings.between_gap==B::AppendPrevious)),
            "gap audio belongs to the preceding track".into()),
        C::BetweenPrepend => (format!("{} Between tracks: Prepend to next",radio(settings.between_gap==B::PrependNext)),
            "gap audio belongs to the following track".into()),
        C::BetweenDrop => (format!("{} Between tracks: Drop",radio(settings.between_gap==B::Drop)),
            "omit INDEX 00..INDEX 01 gap audio".into()),
        C::FirstPrepend => (format!("{} Before track 1: Prepend to track 1",radio(settings.first_gap==F::PrependFirst)),
            "include hidden pregap before track 1".into()),
        C::FirstOwn00 => (format!("{} Before track 1: Own track 00",radio(settings.first_gap==F::OwnTrack00)),
            "write hidden pregap as its own track when present".into()),
        C::FirstDrop => (format!("{} Before track 1: Drop",radio(settings.first_gap==F::Drop)),
            "omit initial pregap audio".into()),
        C::CopyArtwork => (format!("{} Artwork",mark(settings.copy_folder.artwork)),
            "image files beside the source CUE image".into()),
        C::CopyArtFolders => (format!("{} Art folders",mark(settings.copy_folder.art_folders)),
            "Artwork, Art, Scans, Covers, Booklet (any case)".into()),
        C::CopyLog => (format!("{} .log",mark(settings.copy_folder.log)),"source rip logs".into()),
        C::CopyCue => (format!("{} .cue",mark(settings.copy_folder.cue)),"original source cues".into()),
        C::CopyTxt => (format!("{} .txt",mark(settings.copy_folder.txt)),"text documents".into()),
        C::CopyNfo => (format!("{} .nfo",mark(settings.copy_folder.nfo)),"release information".into()),
        C::CopyMd => (format!("{} .md",mark(settings.copy_folder.md)),"Markdown notes".into()),
        C::CopyPdf => (format!("{} .pdf",mark(settings.copy_folder.pdf)),"PDF booklets".into()),
        C::CopyMd5 => (format!("{} .md5",mark(settings.copy_folder.md5)),"original checksums".into()),
        C::CopyOther => (format!("{} Everything else",mark(settings.copy_folder.other)),
            "other regular companion files and directories".into()),
        C::Done => ("Done".into(),"return to the parent window".into()),
    }
}

/// Visual vocabulary for the mockup-based Options forms. The existing ordered
/// control list remains the authority for keyboard focus and mouse actions.
#[derive(Clone,Copy)]
enum ExtractOptionVisual { Toggle, Button, Radio }

fn option_draw_text(frame:&mut ratatui::Frame,rect:Rect,text:&str,theme:Theme,color:Color) {
    if rect.width==0 || rect.height==0 {return;}
    frame.render_widget(Paragraph::new(clip_end(text,rect.width))
        .style(Style::default().bg(theme.dropdown_bg).fg(readable_word(color,theme.dropdown_bg))),rect);
}

/// Value and qualifier share the current wording but retain the mockup's
/// distinct value and muted-label tiers on both dark and light themes.
fn option_draw_summary(frame:&mut ratatui::Frame,rect:Rect,text:&str,theme:Theme) {
    if rect.width==0 || rect.height==0 {return;}
    let clipped=clip_end(text,rect.width);
    let (value,qualifier)=match clipped.split_once(" · for ") {
        Some((value,rest))=>(value.to_owned(),format!(" · for {rest}")),
        None=>(clipped,String::new()),
    };
    let surface=theme.dropdown_bg;
    let line=Line::from(vec![
        Span::styled(value,Style::default().fg(readable_word(theme.value,surface))),
        Span::styled(qualifier,Style::default().fg(readable_word(theme.label,surface))),
    ]);
    frame.render_widget(Paragraph::new(line).style(Style::default().bg(surface)),rect);
}

fn option_draw_control(frame:&mut ratatui::Frame,buttons:&mut ButtonRenderMap,
    choices:&[ExtractOptionControl],active:usize,hover:Option<TuiButton>,theme:Theme,
    control:ExtractOptionControl,rect:Rect,label:&str,visual:ExtractOptionVisual)
{
    if rect.width==0 || rect.height==0 {return;}
    let Some(index)=choices.iter().position(|item|*item==control) else {return;};
    let focused=index==active;
    let hovered=hover==Some(TuiButton::DiscBrowserOptionControl(index));
    let bg=if focused {theme.pill_active_bg}
        else if hovered {theme.pill_neutral_hover_bg}
        else if matches!(visual,ExtractOptionVisual::Button) {theme.pill_neutral_bg}
        else {theme.dropdown_bg};
    let fg=readable_word(if focused {theme.pill_active_fg}else{theme.value},bg);
    fill(frame,rect,bg);
    let clipped=clip_end(label,rect.width);
    let mut chars=clipped.chars();
    let first=chars.next();
    let mark=matches!(first,Some('☑'|'☐'|'●'|'○'));
    if mark && !matches!(visual,ExtractOptionVisual::Button) {
        let glyph=first.unwrap_or(' ');
        let mark_fg=if focused {fg}
            else if glyph=='☑' {readable_word(theme.success,bg)}
            else if glyph=='●' {readable_word(theme.info,bg)}
            else {readable_word(theme.label,bg)};
        let text=Line::from(vec![
            Span::styled(glyph.to_string(),Style::default().fg(mark_fg)),
            Span::styled(chars.collect::<String>(),Style::default().fg(fg)),
        ]);
        frame.render_widget(Paragraph::new(text).style(Style::default().bg(bg)),rect);
    } else {
        frame.render_widget(Paragraph::new(clipped).style(Style::default().bg(bg).fg(fg)),rect);
    }
    buttons.record_button(TuiButton::DiscBrowserOptionControl(index),rect);
}

fn option_section(frame:&mut ratatui::Frame,area:Rect,y:u16,text:&str,theme:Theme) {
    option_draw_text(frame,Rect::new(area.x+2,y,area.width.saturating_sub(4),1),
        text,theme,theme.header);
}

fn option_footer(frame:&mut ratatui::Frame,s:&DiscBrowserState,area:Rect,
    buttons:&mut ButtonRenderMap,choices:&[ExtractOptionControl],active:usize,
    hover:Option<TuiButton>,theme:Theme)
{
    let rule_y=area.bottom().saturating_sub(3);
    for x in area.x+1..area.right().saturating_sub(1) {
        let cell=frame.buffer_mut().get_mut(x,rule_y);
        cell.set_symbol("─");
        cell.set_fg(theme.border);
    }
    let y=area.bottom().saturating_sub(2);
    let hint=Rect::new(area.x+2,y,area.width.saturating_sub(14).min(10),1);
    option_draw_text(frame,hint,"? all keys",theme,theme.label);
    if hint.width>0 {
        option_draw_text(frame,Rect::new(hint.x,hint.y,1,1),"?",theme,theme.info);
        if let Some(index)=super::disc_browser_actions::native_key_hints(s)
            .iter().position(|label|*label=="? all keys") {
            buttons.record_button(TuiButton::DiscBrowserHint(index),hint);
        }
    }
    let rect=Rect::new(area.right().saturating_sub(9),y,6,1);
    option_draw_control(frame,buttons,choices,active,hover,theme,
        ExtractOptionControl::Done,rect," Done ",ExtractOptionVisual::Button);
    if let Some(index)=choices.iter().position(|item|*item==ExtractOptionControl::Done) {
        let selected=index==active;
        let bg=if selected {theme.pill_active_bg}else{theme.chip_go};
        fill(frame,rect,bg);
        frame.render_widget(Paragraph::new(" Done ").style(Style::default()
            .bg(bg).fg(readable_word(if selected {theme.pill_active_fg}else{theme.panel_bg},bg))),rect);
    }
}

fn draw_extract_options_compact(frame:&mut ratatui::Frame,s:&DiscBrowserState,
    panel:ExtractPanel,area:Rect,buttons:&mut ButtonRenderMap,
    choices:&[ExtractOptionControl],active:usize,hover:Option<TuiButton>,theme:Theme)
{
    use ExtractOptionControl as C;
    let x=area.x+2;
    let inner=area.width.saturating_sub(4);
    let y=area.y;
    match panel {
        ExtractPanel::Options => {
            option_section(frame,area,y+2,"OUTPUT",theme);
            for (control,offset) in [(C::WriteLog,3),(C::Verify,4),(C::Chapters,5),
                (C::CueFiles,6),(C::CopyFolder,8),(C::SaveNext,10)] {
                let row=y+offset;
                let (label,note)=extract_option_display(s,control);
                let style=if matches!(control,C::Chapters|C::CueFiles|C::CopyFolder) {
                    ExtractOptionVisual::Button
                } else {ExtractOptionVisual::Toggle};
                let short_label=if matches!(style,ExtractOptionVisual::Button) {
                    format!(" {label} ")
                } else {label};
                let cap=if matches!(style,ExtractOptionVisual::Button) {
                    (short_label.chars().count() as u16).min(inner)
                } else {inner.min(short_label.chars().count() as u16)};
                let target=Rect::new(x,row,cap,1);
                option_draw_control(frame,buttons,choices,active,hover,theme,
                    control,target,&short_label,style);
                if control==C::CueFiles {
                    let note_rect=Rect::new(x+1,row+1,inner.saturating_sub(1),1);
                    option_draw_text(frame,note_rect,&note,theme,theme.label);
                    if let Some(index)=choices.iter().position(|item|*item==control) {
                        buttons.record_button(TuiButton::DiscBrowserOptionControl(index),note_rect);
                    }
                } else if control==C::Chapters || control==C::CopyFolder {
                    let description=if control==C::Chapters {
                        format!("chapters {}",if s.extract_settings.write_chapters {"on"}else{"off"})
                    } else {format!("{}/10 types",s.extract_settings.copy_folder.enabled_count())};
                    let note_x=x+cap+1;
                    option_draw_text(frame,Rect::new(note_x,row,inner.saturating_sub(cap+1),1),
                        &description,theme,theme.label);
                }
            }
            option_section(frame,area,y+9,"DEFAULTS",theme);
        },
        ExtractPanel::Chapters => {
            option_draw_text(frame,Rect::new(x,y+1,inner,1),
                "Used when chapters from an ISO or container can't be kept in the output.",theme,theme.label);
            let (label,_)=extract_option_display(s,C::WriteChapters);
            option_draw_control(frame,buttons,choices,active,hover,theme,C::WriteChapters,
                Rect::new(x,y+3,inner.min(label.chars().count() as u16),1),
                &label,ExtractOptionVisual::Toggle);
            for (i,text) in ["beside the output file","marks where each chapter starts",
                "mkvmerge and MP4Box read it."].iter().enumerate() {
                option_draw_text(frame,Rect::new(x+2,y+4+i as u16,inner.saturating_sub(2),1),
                    text,theme,theme.label);
            }
        },
        ExtractPanel::CueFiles => {
            option_draw_text(frame,Rect::new(x,y+1,inner,1),
                "Creating a CUE file, and splitting a CUE image into tracks.",theme,theme.label);
            option_section(frame,area,y+2,"NEW CUE FILE",theme);
            for (control,row) in [(C::CreateCue,3),(C::EmbedCue,4),(C::KeepPreemphasis,5)] {
                let (label,_)=extract_option_display(s,control);
                option_draw_control(frame,buttons,choices,active,hover,theme,control,
                    Rect::new(x,y+row,inner.min(label.chars().count() as u16),1),
                    &label,ExtractOptionVisual::Toggle);
            }
            option_section(frame,area,y+6,"SPLITTING A CUE IMAGE",theme);
            let tight=area.height<18;
            if !tight {
                option_draw_text(frame,Rect::new(x,y+7,inner,1),
                    "Gaps are the audio between INDEX 00 and INDEX 01.",theme,theme.label);
            }
            for (control,row,label) in [
                (C::BetweenAppend,8,"Append to previous"),(C::BetweenPrepend,9,"Prepend to next"),
                (C::BetweenDrop,10,"Drop between tracks"),(C::FirstPrepend,11,"Prepend to track 1"),
                (C::FirstOwn00,12,"Own track 00"),(C::FirstDrop,13,"Drop before track 1")
            ] {
                let (marked,_)=extract_option_display(s,control);
                let glyph=marked.chars().next().unwrap_or('○');
                let display=format!("{glyph} {label}");
                let visible_row=row-u16::from(tight);
                option_draw_control(frame,buttons,choices,active,hover,theme,control,
                    Rect::new(x,y+visible_row,inner.min(display.chars().count() as u16),1),
                    &display,ExtractOptionVisual::Radio);
            }
        },
        ExtractPanel::CopyFolder => {
            option_draw_text(frame,Rect::new(x,y+1,inner,1),
                "Copied from a CUE image's folder into the destination folder.",theme,theme.label);
            let cols=2u16;
            let col_width=inner/cols;
            let copy_controls=&choices[..choices.len().saturating_sub(1)];
            for (i,control) in copy_controls.iter().enumerate() {
                let row=(i/cols as usize) as u16;
                let col=(i%cols as usize) as u16;
                let (label,_)=extract_option_display(s,*control);
                let rect=Rect::new(x+col*col_width,y+3+row,
                    (label.chars().count() as u16).min(if col==cols-1 {inner-col*col_width}else{col_width}),1);
                option_draw_control(frame,buttons,choices,active,hover,theme,*control,
                    rect,&label,ExtractOptionVisual::Toggle);
            }
            for (i,text) in ["Artwork: image files beside the CUE image.",
                "Art folders: Artwork, Art, Scans, Covers and Booklet, in any case.",
                ".cue and .md5 describe the source image, not the new tracks."].iter().enumerate() {
                let row=y+9+i as u16;
                if row<area.bottom().saturating_sub(3) {
                    option_draw_text(frame,Rect::new(x,row,inner,1),text,theme,theme.label);
                }
            }
        },
        _=>{},
    }
}

fn draw_extract_options_sheet(frame:&mut ratatui::Frame,s:&DiscBrowserState,
    panel:ExtractPanel,popup:Rect,buttons:&mut ButtonRenderMap,theme:Theme,
    hover:Option<TuiButton>)
{
    use ExtractOptionControl as C;
    use crate::convert::pipeline::native_extract::{NativeCueBetweenGap as B,NativeCueFirstGap as F};
    let width=popup.width.saturating_sub(2).min(108);
    let max_height=match panel {
        ExtractPanel::Options=>19,ExtractPanel::Chapters=>10,
        ExtractPanel::CueFiles=>25,ExtractPanel::CopyFolder=>14,_=>12,
    };
    let height=popup.height.saturating_sub(2).min(max_height);
    if width<22 || height<7 {return;}
    let area=Rect::new(popup.x+(popup.width-width)/2,popup.y+(popup.height-height)/2,width,height);
    for y in popup.y..popup.bottom() {
        for x in popup.x..popup.right() {
            let cell=frame.buffer_mut().get_mut(x,y);
            cell.set_fg(blend_color(cell.fg,cell.bg,3,4));
        }
    }
    buttons.record_button(TuiButton::DiscBrowserOptionOutside,frame.size());
    buttons.record_button(TuiButton::DiscBrowserOptionPassive,area);
    frame.render_widget(Clear,area);
    fill(frame,area,theme.dropdown_bg);
    // The mockup's solid title band replaces the top border; the frame
    // surrounds only the body, and the footer has its own separator.
    let body=Rect::new(area.x,area.y+1,area.width,area.height.saturating_sub(1));
    frame.render_widget(Block::default().borders(Borders::LEFT|Borders::RIGHT|Borders::BOTTOM)
        .style(Style::default().bg(theme.dropdown_bg))
        .border_style(Style::default().fg(theme.title)),body);
    let title=match panel {
        ExtractPanel::Options=>"▸ Options",
        ExtractPanel::Chapters=>"▸ Chapters",
        ExtractPanel::CueFiles=>"▸ CUE files",
        ExtractPanel::CopyFolder=>"▸ Copy folder contents",
        _=>unreachable!(),
    };
    let title_rect=Rect::new(area.x+1,area.y,area.width.saturating_sub(2),1);
    fill(frame,title_rect,theme.title);
    frame.render_widget(Paragraph::new(format!(" {title}")).style(Style::default()
        .bg(theme.title).fg(readable_word(theme.panel_bg,theme.title))),title_rect);
    let choices=s.option_controls();
    let active=s.panel_cursor.min(choices.len().saturating_sub(1));
    let full=match panel {
        ExtractPanel::Options=>area.width>=88 && area.height>=19,
        ExtractPanel::Chapters=>area.width>=80 && area.height>=10,
        ExtractPanel::CueFiles=>area.width>=100 && area.height>=25,
        ExtractPanel::CopyFolder=>area.width>=94 && area.height>=14,
        _=>false,
    };
    if !full {
        draw_extract_options_compact(frame,s,panel,area,buttons,&choices,active,hover,theme);
    } else {
        let x=area.x+2;
        let y=area.y;
        let inner=area.width.saturating_sub(4);
        let column=x+34;
        let note_width=area.right().saturating_sub(column+2);
        match panel {
            ExtractPanel::Options => {
                option_section(frame,area,y+2,"OUTPUT",theme);
                for (control,row) in [(C::WriteLog,3),(C::Verify,4)] {
                    let (label,_)=extract_option_display(s,control);
                    option_draw_control(frame,buttons,&choices,active,hover,theme,control,
                        Rect::new(x,y+row,label.chars().count() as u16,1),
                        &label,ExtractOptionVisual::Toggle);
                }
                option_draw_text(frame,Rect::new(column,y+3,note_width,1),
                    "in destination folder, beside the tracks",theme,theme.label);
                option_draw_text(frame,Rect::new(column,y+4,note_width,1),
                    "reads each file back and checks its audio",theme,theme.label);
                option_draw_text(frame,Rect::new(column,y+5,note_width,1),
                    "against a checksum taken while extracting",theme,theme.label);
                for (control,row) in [(C::Chapters,7),(C::CueFiles,9),(C::CopyFolder,11)] {
                    let (label,note)=extract_option_display(s,control);
                    let button_width=29u16;
                    let rect=Rect::new(x,y+row,button_width,1);
                    option_draw_control(frame,buttons,&choices,active,hover,theme,
                        control,rect,&format!(" {label}"),ExtractOptionVisual::Button);
                    let description=if control==C::CueFiles {
                        format!("{note} · for new CUEs and images")
                    } else {note};
                    let available=note_width as usize;
                    if description.chars().count()>available && control==C::CueFiles {
                        let prefix=description.split(" · for new CUEs and images").next().unwrap_or("");
                        option_draw_summary(frame,Rect::new(column,y+row,note_width,1),prefix,theme);
                        option_draw_text(frame,Rect::new(column,y+row+1,note_width,1),
                            "for new CUEs and images",theme,theme.label);
                    } else {
                        option_draw_summary(frame,Rect::new(column,y+row,note_width,1),
                            &description,theme);
                    }
                }
                option_section(frame,area,y+13,"DEFAULTS",theme);
                let (label,_)=extract_option_display(s,C::SaveNext);
                option_draw_control(frame,buttons,&choices,active,hover,theme,C::SaveNext,
                    Rect::new(x,y+14,label.chars().count() as u16,1),&label,ExtractOptionVisual::Toggle);
                option_draw_text(frame,Rect::new(column,y+14,note_width,1),
                    "saved when you press Extract",theme,theme.label);
            },
            ExtractPanel::Chapters => {
                option_draw_text(frame,Rect::new(x,y+1,inner,1),
                    "Used when chapters from an ISO or container can't be kept in the output.",theme,theme.value);
                let (label,_)=extract_option_display(s,C::WriteChapters);
                option_draw_control(frame,buttons,&choices,active,hover,theme,C::WriteChapters,
                    Rect::new(x,y+3,label.chars().count() as u16,1),&label,ExtractOptionVisual::Toggle);
                for (offset,text) in [(3,"beside the output file"),(4,"marks where each chapter starts"),
                    (5,"mkvmerge and MP4Box read it.")] {
                    option_draw_text(frame,Rect::new(column,y+offset,note_width,1),text,theme,theme.label);
                }
            },
            ExtractPanel::CueFiles => {
                option_draw_text(frame,Rect::new(x,y+1,inner,1),
                    "Creating a CUE file, and splitting a CUE image into tracks.",theme,theme.value);
                option_section(frame,area,y+3,"NEW CUE FILE",theme);
                for (control,row) in [(C::CreateCue,4),(C::EmbedCue,8),(C::KeepPreemphasis,11)] {
                    let (label,_)=extract_option_display(s,control);
                    option_draw_control(frame,buttons,&choices,active,hover,theme,control,
                        Rect::new(x,y+row,label.chars().count() as u16,1),&label,ExtractOptionVisual::Toggle);
                }
                option_draw_text(frame,Rect::new(column,y+4,note_width,1),
                    "lists where each track starts",theme,theme.label);
                for (row,text) in [(5,"For SACD ISOs and DFF files, and for any area or stream saved as one"),
                    (6,"file that cannot hold its own track list."),
                    (9,"For FLAC and WavPack saved as one file.")] {
                    option_draw_text(frame,Rect::new(x,y+row,inner,1),text,theme,theme.label);
                }
                option_draw_text(frame,Rect::new(column,y+8,note_width,1),
                    "stored inside the audio file itself",theme,theme.label);
                option_draw_text(frame,Rect::new(column,y+11,note_width,1),
                    "marks tracks mastered with pre-emphasis",theme,theme.label);
                option_draw_text(frame,Rect::new(column,y+12,note_width,1),
                    "from the source CUE or the rip log",theme,theme.label);
                option_section(frame,area,y+14,"SPLITTING A CUE IMAGE",theme);
                option_draw_text(frame,Rect::new(x,y+15,inner,1),
                    "Gaps are the audio between a track's INDEX 00 and INDEX 01.",theme,theme.label);
                for (row,title,options,summary) in [
                    (17,"Between tracks",[
                        (C::BetweenAppend,"Append to previous"),(C::BetweenPrepend,"Prepend to next"),(C::BetweenDrop,"Drop")],
                        match s.extract_settings.between_gap {
                            B::AppendPrevious=>"Albums play gaplessly.",
                            B::PrependNext=>"The gap starts the next track.",
                            B::Drop=>"The gap audio is omitted.",
                        }),
                    (20,"Before track 1",[
                        (C::FirstPrepend,"Prepend to track 1"),(C::FirstOwn00,"Own track 00"),(C::FirstDrop,"Drop")],
                        match s.extract_settings.first_gap {
                            F::PrependFirst=>"A hidden track becomes part of track 1.",
                            F::OwnTrack00=>"A hidden track before track 1 becomes track 00.",
                            F::Drop=>"The hidden-track audio is omitted.",
                        }),
                ] {
                    option_draw_text(frame,Rect::new(x,y+row,17,1),title,theme,theme.label);
                    let mut radio_x=x+18;
                    for (control,label) in options {
                        let (marked,_)=extract_option_display(s,control);
                        let glyph=marked.chars().next().unwrap_or('○');
                        let display=format!("{glyph} {label}");
                        let width=display.chars().count() as u16;
                        option_draw_control(frame,buttons,&choices,active,hover,theme,control,
                            Rect::new(radio_x,y+row,width,1),&display,ExtractOptionVisual::Radio);
                        radio_x=radio_x.saturating_add(width+2);
                    }
                    option_draw_text(frame,Rect::new(x+18,y+row+1,inner.saturating_sub(18),1),
                        summary,theme,theme.label);
                }
            },
            ExtractPanel::CopyFolder => {
                option_draw_text(frame,Rect::new(x,y+1,inner,1),
                    "Copied from a CUE image's folder into the destination folder.",theme,theme.value);
                let col_width=inner/4;
                for (i,control) in choices.iter().take(10).enumerate() {
                    let col=(i%4) as u16;
                    let row=(i/4) as u16;
                    let (label,_)=extract_option_display(s,*control);
                    option_draw_control(frame,buttons,&choices,active,hover,theme,*control,
                        Rect::new(x+col*col_width,y+3+row,
                            (label.chars().count() as u16).min(if col==3 {inner-col*col_width}else{col_width}),1),
                        &label,ExtractOptionVisual::Toggle);
                }
                for (i,text) in ["Artwork: image files beside the CUE image.",
                    "Art folders: Artwork, Art, Scans, Covers and Booklet, in any case.",
                    ".cue and .md5 describe the source image, not the new tracks."].iter().enumerate() {
                    option_draw_text(frame,Rect::new(x,y+7+i as u16,inner,1),
                        text,theme,theme.label);
                }
            },
            _=>unreachable!(),
        }
    }
    option_footer(frame,s,area,buttons,&choices,active,hover,theme);
}

fn draw_panel(frame:&mut ratatui::Frame,s:&DiscBrowserState,panel:ExtractPanel,popup:Rect,
    buttons:&mut ButtonRenderMap,theme:Theme,hover:Option<TuiButton>) {
    if panel == ExtractPanel::Tags {
        draw_tags_sheet_with_hover(frame,s,popup,buttons,theme,hover);
        return;
    }
    if matches!(panel,ExtractPanel::Options | ExtractPanel::Chapters | ExtractPanel::CueFiles | ExtractPanel::CopyFolder) {
        draw_extract_options_sheet(frame,s,panel,popup,buttons,theme,hover);
        return;
    }
    let max_w=popup.width.saturating_sub(8).min(75);
    let max_h=popup.height.saturating_sub(6).min(17);
    let area=Rect::new(popup.x+(popup.width-max_w)/2,popup.y+(popup.height-max_h)/2,max_w,max_h);
    frame.render_widget(Clear,area);
    frame.render_widget(Block::default().style(Style::default().bg(theme.panel_bg)),area);
    let title=match panel {
        ExtractPanel::Tags=>"Tags",ExtractPanel::Downmix=>"Downmix",
        ExtractPanel::AssumeDecrypted=>"Assume decrypted",ExtractPanel::Diagnostics=>"Diagnostics",
        ExtractPanel::Options | ExtractPanel::Chapters | ExtractPanel::CueFiles | ExtractPanel::CopyFolder=>unreachable!("Options use their dedicated sheet"),
        ExtractPanel::ConvertChoice=>"Convert selection",
        ExtractPanel::Keys=>"Extract keys",
    };
    let block=Block::default().borders(Borders::ALL).title(format!(" {title} "))
        .style(Style::default().bg(theme.panel_bg).fg(readable_word(theme.value,theme.panel_bg)))
        .border_style(Style::default().fg(theme.title));
    let inner=block.inner(area);
    frame.render_widget(block,area);
    let lines:Vec<String>=match panel {
        ExtractPanel::Tags=>unreachable!("Tags uses the dedicated sheet"),
        ExtractPanel::Downmix=>vec!["[ / ]  Cycle DVD-Audio downmix policy".into(),
            format!("Current: {}", option_value(s,"Downmix")),
            "none preserves every source channel".into()],
        ExtractPanel::AssumeDecrypted=>vec!["[ / ]  Toggle the already-decrypted assumption".into(),
            format!("Current: {}", option_value(s,"Assume decrypted")),
            "Enable only for a copy you know is already decrypted".into()],
        ExtractPanel::Diagnostics=>s.contents.diagnostics.iter().map(|d|format!("{d:?}")).collect(),
        ExtractPanel::Options | ExtractPanel::Chapters | ExtractPanel::CueFiles | ExtractPanel::CopyFolder => unreachable!("Options use their dedicated sheet"),
        ExtractPanel::ConvertChoice=>vec!["Enter  Convert highlighted program".into(),
            "e  Queue every checked program separately".into(),
            "Esc  Return to extraction".into()],
        ExtractPanel::Keys=>super::disc_browser_actions::native_key_help(s),
    };
    // The Keys sheet is ordinarily read-only. Only help opened from an
    // Options popup gains a mouse-accessible return action, preserving the
    // existing global Extract-key help behavior.
    let return_to_options=panel==ExtractPanel::Keys && s.option_help_return.is_some();
    let content=if return_to_options {
        Rect::new(inner.x,inner.y,inner.width,inner.height.saturating_sub(2))
    } else {inner};
    if lines.is_empty(){frame.render_widget(Paragraph::new("None"),content);}else{
        let rendered=lines.into_iter().skip(s.panel_cursor).take(content.height as usize)
            .map(|l|Line::from(clip(&l,content.width))).collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(rendered),content);
    }
    if return_to_options && inner.height>0 && inner.width>=6 {
        let done=Rect::new(inner.right()-6,inner.bottom()-1,6,1);
        fill(frame,done,theme.chip_go);
        frame.render_widget(Paragraph::new(" Done ").style(Style::default()
            .bg(theme.chip_go).fg(readable_word(theme.panel_bg,theme.chip_go))),done);
        buttons.record_button(TuiButton::DiscBrowserOptionOutside,done);
    }
}

#[cfg(test)]
mod extract_r3_render_tests {
    use super::*;
    use crate::disc::model::{
        AudioPresentationFormat, CopyProtectionSummary, DiscContents,
        DiscFormat, DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo,
        PresentationId, SacdAreaId,
    };
    use crate::disc::diagnostics::{DiscDiagnostic, DiagnosticScope, DiagnosticSeverity};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn r7_matroska_tags_row_identifies_unsupported_native_metadata() {
        let mut state = sacd_state(false, false);
        state.contents.format = DiscFormat::Matroska;
        state.contents.presentations[0].id = PresentationId::MatroskaStream {
            stream_index: 0, audio_ordinal: 1, chapter_count: 0,
        };
        state.file_per_program = true;
        state.tags = true;
        state.tag_artwork = true;
        state.contents.presentations[0].format.codec = Some("ac3".into());
        let text = option_value(&state, "Tags");
        assert!(text.starts_with("on where supported"), "{text}");
        assert!(text.contains("no tags: .ac3"), "{text}");
        assert!(text.contains("no artwork: .ac3"), "{text}");
        state.contents.presentations[0].format.codec = Some("flac".into());
        let text = option_value(&state, "Tags");
        assert!(text.contains("artwork"), "{text}");
        assert!(!text.contains("unavailable"), "{text}");
        state.contents.presentations[0].format.codec = Some("alac".into());
        assert!(option_value(&state, "Format").contains(".mka"));
        assert!(!option_value(&state, "Tags").contains("unavailable"));
    }

    fn sacd_state(diagnostics: bool, details: bool) -> DiscBrowserState {
        let mut extract = PresentationExtractInfo::default();
        if details { extract.sacd_header_details = Some("Catalog 12345 · Publisher Example · Hybrid".into()); }
        let contents = DiscContents {
            format: DiscFormat::Sacd,
            label: "SACD".into(), source_path: "disc.iso".into(),
            presentations: vec![DiscPresentation {
                id: PresentationId::SacdArea(SacdAreaId::Stereo), label: "DSD Stereo".into(),
                format: AudioPresentationFormat {
                    codec: Some("DSD".into()), sample_rate: Some(2_822_400),
                    bit_depth: Some(1), channels: Some(2), channel_layout: Some("stereo".into()),
                    lossless: true, provenance: FormatProvenance::TocHeader,
                },
                tracks: vec![DiscTrack { number: 1, title: Some("Track".into()),
                    performer: None, duration_secs: Some(1.0), format_note: None }],
                total_duration_secs: 1.0,
                album_title: None, album_artist: None, genre: None, year: None,
                extract,
            }],
            suppressed: vec![],
            copy_protection: CopyProtectionSummary { description: "None".into() },
            diagnostics: if diagnostics { vec![DiscDiagnostic {
                severity: DiagnosticSeverity::Warning, scope: DiagnosticScope::Disc,
                message: "sample diagnostic".into(),
            }] } else { vec![] },
            album_title: Some("Example Album".into()), album_artist: None,
            genre: None, year: None,
        };
        DiscBrowserState::new(contents, "disc.iso".into())
    }

    #[test]
    fn r6_program_and_track_boxes_use_one_cell_and_preserve_click_targets() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        let mut second = state.contents.presentations[0].tracks[0].clone();
        second.number = 2;
        second.title = Some("Second".into());
        state.contents.presentations[0].tracks.push(second);
        state.items_checked[0] = vec![true, false];

        for (selected, mark) in [(false, "☐"), (true, "☑")] {
            state.selected[0] = selected;
            let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
            let mut buttons = ButtonRenderMap::new();
            terminal.draw(|frame| draw_programs(frame, &state,
                Rect::new(2, 3, 104, 8), &mut buttons, theme)).unwrap();
            let target = TuiButton::DiscBrowserExpand(0);
            let hit = buttons.button_rect(target).expect("program checkbox hit target");
            assert_eq!(terminal.backend().buffer().get(hit.x, hit.y).symbol(), mark);
            assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(target));
            assert_eq!(buttons.find_button_at(hit.x + 2, hit.y), Some(target),
                "caption next to program checkbox remains clickable");
            assert_eq!(buttons.find_button_at(hit.right(), hit.y),
                Some(TuiButton::DiscBrowserStream(0)),
                "rest of program row still focuses without toggling");
        }

        // A secondary details row stays one cell right of its program heading.
        // Both SACD and DVD-Audio use the ordinary (non-title) layout.
        for format in [DiscFormat::Sacd, DiscFormat::DvdAudio] {
            let mut program = sacd_state(false, false);
            program.contents.format = format;
            if format == DiscFormat::DvdAudio {
                program.contents.presentations[0].id = PresentationId::DvdAudioGroup(1);
                program.contents.presentations[0].format.codec = Some("MLP".into());
            }
            let area = Rect::new(2, 3, 104, 8);
            let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
            let mut buttons = ButtonRenderMap::new();
            terminal.draw(|frame| draw_programs(frame, &program, area,
                &mut buttons, theme)).unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| (area.x..area.right()).map(|x|
                buffer.get(x, y).symbol().to_owned()).collect::<Vec<_>>().concat();
            let heading = header_for(program.contents.presentations[0].id,
                &program.contents.presentations[0], 0);
            let details = program_info(&program.contents.presentations[0], format);
            let heading_row = row(area.y + 1);
            let details_row = row(area.y + 2);
            let heading_at = heading_row.find(heading.as_str()).expect("program heading rendered");
            let details_at = details_row.find(details.as_str()).expect("program details rendered");
            let heading_col = heading_row[..heading_at].chars().count();
            let details_col = details_row[..details_at].chars().count();
            assert_eq!(details_col, heading_col + 1,
                "{format:?}: details remain one cell deeper than the heading");
        }

        state.focus = ExtractFocus::Items;
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame| draw_items(frame, &state,
            Rect::new(2, 3, 104, 8), &mut buttons, theme, true)).unwrap();
        for (index, mark) in [(0, "☑"), (1, "☐")] {
            let target = TuiButton::DiscBrowserTrackCheck(index);
            let hit = buttons.button_rect(target).expect("track checkbox hit target");
            assert_eq!(hit.width, 1, "one-cell track tick box");
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.get(hit.x, hit.y).symbol(), mark);
            assert_eq!(buffer.get(hit.x + 2, hit.y).symbol(), "0",
                "track numbers align after the narrow checkbox");
            assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(target));
            assert_eq!(buttons.find_button_at(hit.x + 2, hit.y),
                Some(TuiButton::DiscBrowserTrack(index)),
                "track caption keeps the row's existing click behavior");
        }

        let mut title = chapter_state(1);
        title.focus = ExtractFocus::Programs;
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame| draw_programs(frame, &title,
            Rect::new(2, 3, 104, 8), &mut buttons, theme)).unwrap();
        let target = TuiButton::DiscBrowserExpand(0);
        let hit = buttons.button_rect(target).expect("indented stream checkbox hit target");
        assert_eq!(terminal.backend().buffer().get(hit.x, hit.y).symbol(), "☑");
        assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(target));
        assert_eq!(buttons.find_button_at(hit.x + 2, hit.y), Some(target));
    }

    #[test]
    fn r6_options_and_tags_tick_boxes_keep_mouse_hits_on_glyph_and_caption() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        for (width, height) in [(72, 24), (154, 40)] {
            for panel in [ExtractPanel::Options, ExtractPanel::Chapters,
                ExtractPanel::CueFiles, ExtractPanel::CopyFolder] {
                state.panel = Some(panel);
                let choices = state.option_controls();
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut buttons = ButtonRenderMap::new();
                terminal.draw(|frame| draw(frame, &state, Path::new("/tmp"),
                    &mut buttons, theme)).unwrap();
                let mut tick_count = 0;
                for (index, control) in choices.iter().enumerate() {
                    let (text, _) = extract_option_display(&state, *control);
                    let Some(mark) = text.chars().next().filter(|mark|
                        matches!(*mark, '☐' | '☑')) else { continue; };
                    let target = TuiButton::DiscBrowserOptionControl(index);
                    let hit = buttons.button_rect(target).expect("option checkbox target");
                    assert!(hit.width >= 3, "{panel:?} checkbox caption has a hit target");
                    let expected = if mark == '☑' { "☑" } else { "☐" };
                    assert_eq!(terminal.backend().buffer().get(hit.x, hit.y).symbol(),
                        expected, "{panel:?} {control:?}");
                    assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(target));
                    assert_eq!(buttons.find_button_at(hit.x + 2, hit.y), Some(target));
                    tick_count += 1;
                }
                assert!(tick_count > 0, "{panel:?} has visible tick boxes");
            }
        }

        for transfer in [false, true] {
            state.tags = transfer;
            state.tag_fields = 0b010101010;
            state.panel = Some(ExtractPanel::Tags);
            let controls = state.tag_sheet_controls();
            let mut terminal = Terminal::new(TestBackend::new(132, 44)).unwrap();
            let mut buttons = ButtonRenderMap::new();
            terminal.draw(|frame| draw_tags_sheet(frame, &state,
                Rect::new(6, 5, 116, 33), &mut buttons, theme)).unwrap();
            let checked = if transfer {
                vec![TagSheetControl::Transfer, TagSheetControl::Field(0),
                    TagSheetControl::Field(1), TagSheetControl::Artwork]
            } else { vec![TagSheetControl::Transfer] };
            for control in checked {
                let index = controls.iter().position(|entry| *entry == control).unwrap();
                let target = TuiButton::DiscBrowserTagSheetControl(index);
                let hit = buttons.button_rect(target).expect("Tags checkbox target");
                let on = match control {
                    TagSheetControl::Transfer => transfer,
                    TagSheetControl::Field(i) => state.tag_fields & (1u16 << i) != 0,
                    TagSheetControl::Artwork => state.tag_artwork,
                    _ => unreachable!(),
                };
                assert_eq!(terminal.backend().buffer().get(hit.x, hit.y).symbol(), check(on));
                assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(target));
                assert_eq!(buttons.find_button_at(hit.x + 2, hit.y), Some(target));
            }
        }
    }

    fn render_header(state: &DiscBrowserState, width: u16, small: bool) -> (Vec<String>, Terminal<TestBackend>) {
        let height = if small { 2 } else { 3 };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw_header(f, state, Rect::new(0, 0, width, height),
            small, crate::tui::theme::theme_by_slug_or_default("tokyo-night"))).unwrap();
        let text = (0..height).map(|y| (0..width)
            .map(|x| terminal.backend().buffer().get(x, y).symbol().to_owned())
            .collect::<Vec<_>>().concat()).collect();
        (text, terminal)
    }

    #[test]
    fn sacd_header_retains_diagnostics_on_small_and_details_on_medium() {
        for details in [false, true] {
            for diagnostics in [false, true] {
                let state = sacd_state(diagnostics, details);
                let (small, terminal) = render_header(&state, 64, true);
                let expected = if diagnostics { "Diagnostics  1" } else { "Diagnostics  none" };
                assert!(small[1].contains(expected), "{small:?}");
                assert_eq!(small[1].contains("d view"), diagnostics, "{small:?}");
                if details { assert!(!small[1].contains("Catalog")); }
                if diagnostics {
                    let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
                    let x = small[1].find('1').expect("diagnostic count") as u16;
                    assert_eq!(terminal.backend().buffer().get(x, 1).fg, theme.warning);
                }
                let (medium, _) = render_header(&state, 96, false);
                assert!(medium[2].contains(expected));
                assert_eq!(medium[2].contains("d view"), diagnostics);
                if details { assert!(medium[1].contains("Catalog 12345")); }
            }
        }
    }

    #[test]
    fn themed_extraction_dialog_fills_the_surface_and_title_bar() {
        let state = sacd_state(false, false);
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width, height) in [(150, 40), (100, 30), (64, 20)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| {
                let mut buttons = ButtonRenderMap::new();
                draw(frame,&state,Path::new("/tmp"),&mut buttons,theme);
            }).unwrap();
            let buffer = terminal.backend().buffer();
            let title_y=(0..height).find(|y|(0..width).any(|x|buffer.get(x,*y).bg==theme.title))
                .expect("centered title bar");
            let left=(0..width).find(|x|buffer.get(*x,title_y).symbol()=="┌")
                .expect("unfilled left title corner");
            let right=(0..width).rev().find(|x|buffer.get(*x,title_y).symbol()=="┐")
                .expect("unfilled right title corner");
            assert_ne!(buffer.get(left,title_y).bg,theme.title);
            assert_ne!(buffer.get(right,title_y).bg,theme.title);
            assert_eq!(buffer.get(left+1,title_y).bg,theme.title);
            assert_eq!(buffer.get(right-1,title_y).bg,theme.title);
            assert_eq!(buffer.get(left+1,title_y).symbol(),"▸");
            let bottom_y=(title_y+1..height).find(|y|buffer.get(left,*y).symbol()=="└")
                .expect("closed bottom frame");
            assert_eq!(buffer.get(right,bottom_y).symbol(),"┘", "{width}x{height}");
            assert_eq!(buffer.get(left+2,title_y+1).bg,extract_colors(theme).band,
                "header band {width}x{height}");
            assert_eq!(buffer.get(left,title_y+1).fg,theme.title,
                "side rail {width}x{height}");
            let bottom_margin = height - bottom_y - 1;
            assert!(title_y>=1 && bottom_margin>=1,
                "dialog must leave a row above and below: {width}x{height}");
            assert!(title_y.abs_diff(bottom_margin)<=1,
                "dialog must remain centered even when the default size is capped: {width}x{height}");
            assert!(left>=2 && width-right-1>=2,
                "dialog must leave two columns at each side: {width}x{height}");
        }
    }

    #[test]
    fn r36_default_extraction_window_fills_roomy_terminals_without_maximizing() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        assert!(!state.maximized);
        for (width,height) in [(110u16,34u16),(154,40),(200,64),(240,80)] {
            let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
            terminal.draw(|f|draw(f,&state,Path::new("/tmp"),
                &mut ButtonRenderMap::new(),theme)).unwrap();
            let buffer=terminal.backend().buffer();
            let (left,top)=(0..height).find_map(|y|(0..width).find_map(|x|
                (buffer.get(x,y).symbol()=="┌").then_some((x,y))))
                .expect("default title corner");
            let right=(left+1..width).find(|x|buffer.get(*x,top).symbol()=="┐")
                .expect("default right title corner");
            let bottom=(top+1..height).find(|y|buffer.get(left,*y).symbol()=="└")
                .expect("default lower corner");
            let actual_width=right-left+1;
            let actual_height=bottom-top+1;
            assert!(actual_width>=width*9/10,
                "{width}x{height}: default popup width {actual_width} is too narrow");
            assert!(actual_height>=height*9/10,
                "{width}x{height}: default popup height {actual_height} is too short");
            assert!(left>=2 && width-right-1>=2,"{width}x{height}: horizontal margins");
            assert!(top>=1 && height-bottom-1>=1,"{width}x{height}: vertical margins");
            assert!(left.abs_diff(width-right-1)<=1,"{width}x{height}: x centering");
            assert!(top.abs_diff(height-bottom-1)<=1,"{width}x{height}: y centering");
        }
        state.maximized=true;
        let mut terminal=Terminal::new(TestBackend::new(200,64)).unwrap();
        terminal.draw(|f|draw(f,&state,Path::new("/tmp"),
            &mut ButtonRenderMap::new(),theme)).unwrap();
        let buf=terminal.backend().buffer();
        assert_eq!(buf.get(0,0).symbol(),"┌");
        assert_eq!(buf.get(199,0).symbol(),"┐");
        assert_eq!(buf.get(0,63).symbol(),"└");
        assert_eq!(buf.get(199,63).symbol(),"┘");
    }

    fn rendered_lines(state: &DiscBrowserState, width: u16, height: u16) -> (Vec<String>, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(width,height)).unwrap();
        terminal.draw(|frame| {
            let mut buttons = ButtonRenderMap::new();
            draw(frame,state,Path::new("/tmp"),&mut buttons,
                crate::tui::theme::theme_by_slug_or_default("tokyo-night"));
        }).unwrap();
        let text = (0..height).map(|y| (0..width)
            .map(|x| terminal.backend().buffer().get(x,y).symbol().to_owned())
            .collect::<Vec<_>>().concat()).collect();
        (text,terminal)
    }

    fn chapter_state(count: usize) -> DiscBrowserState {
        let mut contents = sacd_state(false,false).contents;
        contents.format = DiscFormat::DvdVideo;
        contents.presentations[0].id = PresentationId::dvd_video(1,1,0);
        contents.presentations[0].label = "DVD title".into();
        contents.presentations[0].tracks = (1..=count).map(|i|DiscTrack {
            number:i as u32, title: Some(format!("Chapter {i:02}")),
            performer:None, duration_secs:Some(90.0), format_note:None,
        }).collect();
        let mut state = DiscBrowserState::new(contents,"disc.iso".into());
        state.focus = ExtractFocus::Items;
        state
    }

    #[test]
    fn small_dialog_has_three_named_tabs_and_diagnostics_hint_only_when_present() {
        for format in [DiscFormat::Sacd,DiscFormat::DvdVideo] {
            for diagnostic in [false,true] {
                let mut state = sacd_state(diagnostic,false);
                state.contents.format=format;
                let (lines,_) = rendered_lines(&state,72,24);
                let joined = lines.join("\n");
                assert!(joined.contains("Output"),"{joined}");
                assert!(joined.contains(if format==DiscFormat::Sacd {"Areas"}else{"Titles"}),"{joined}");
                assert!(joined.contains(if format==DiscFormat::Sacd {"Tracks"}else{"Chapters"}),"{joined}");
                assert!(!joined.contains("Tab:"),"{joined}");
                assert_eq!(joined.contains("d view"),diagnostic,"{joined}");
            }
        }
    }

    #[test]
    fn narrow_dialog_preserves_diagnostics_and_readable_metadata() {
        let mut state = sacd_state(true, false);
        state.contents.format = DiscFormat::DvdVideo;
        state.contents.copy_protection.description =
            "Long CSS protection / region information ".repeat(5);
        let (header, _) = render_header(&state, 68, true);
        assert!(header[1].contains("Diagnostics  1"), "{header:?}");
        assert!(header[1].contains("d view"), "{header:?}");
        assert!(!header[0].contains("· ·"), "{header:?}");
        assert_eq!(format_bytes(705_600), "689.1 KiB");
        assert_eq!(format_bytes(2*1024u64.pow(4)), "2.0 TiB");

        let single_chapter = chapter_state(1);
        let (dialog, _) = rendered_lines(&single_chapter, 110, 34);
        let text = dialog.join("\n");
        assert!(text.contains("1 chapter"), "{text}");
        assert!(!text.contains("1 chapters"), "{text}");
    }

    #[test]
    fn overflowing_chapters_reserve_a_remaining_row_at_both_mockup_sizes() {
        for (width,height) in [(72,24),(110,34)] {
            let mut state = chapter_state(44);
            let (start,_) = rendered_lines(&state,width,height);
            assert!(start.join("\n").contains("more"),"{start:?}");
            state.item_cursor = 22;
            let (middle,_) = rendered_lines(&state,width,height);
            assert!(middle.join("\n").contains("Chapter 23"),"{middle:?}");
            assert!(middle.join("\n").contains("more"),"{middle:?}");
            state.item_cursor = 43;
            let (end,_) = rendered_lines(&state,width,height);
            assert!(end.join("\n").contains("Chapter 44"),"{end:?}");
            assert!(!end.join("\n").contains("more"),"{end:?}");
        }
    }

    #[test]
    fn working_space_label_reports_actual_scratch_and_destination_volumes() {
        let mut state = sacd_state(false, false);
        let destination = tempfile::tempdir().unwrap();
        state.destination = destination.path().join("extracted");
        let available = space_for(&state.destination).unwrap();
        let expected = format_bytes(available);
        // Display the actual working volume. The default may share the
        // destination filesystem, while explicit scratch can be elsewhere.
        for configured in [None, Some(destination.path()), Some(Path::new("/dev/shm"))] {
            let scratch = crate::tui::disc_browser_actions::native_working_root(&state, configured);
            let mut terminal = Terminal::new(TestBackend::new(170, 2)).unwrap();
            let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
            terminal.draw(|f| draw_results(f, &state, &scratch, Rect::new(0, 0, 170, 2), None, theme)).unwrap();
            let result = (0..170).map(|x| terminal.backend().buffer().get(x,0).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            assert!(result.contains(&format!("destination {expected} free")), "{result}");
            let scratch_free=space_for(&scratch).map_or_else(||"unknown".into(),format_bytes);
            if state.native_storage_shares_destination_volume(&scratch) {
                assert!(!result.contains(" · working "), "{result}");
            } else {
                assert!(result.contains(&format!("working {scratch_free} free")), "{result}");
            }
        }
    }
    #[test]
    fn track_performer_never_overwrites_title_or_right_aligned_start_time() {
        let mut state = sacd_state(false, false);
        state.contents.album_artist = Some("Various Artists".into());
        state.contents.presentations[0].tracks = vec![
            DiscTrack { number: 1, title: Some("Mountain Jam".into()),
                performer: Some("The Allman Brothers Band".into()),
                duration_secs: Some(130.0), format_note: None },
            DiscTrack { number: 2, title: Some("Short Song".into()),
                performer: Some("Guest Performer".into()),
                duration_secs: Some(600.0), format_note: None },
            DiscTrack { number: 3, title: Some("Closing Number".into()),
                performer: Some("Another Performer".into()),
                duration_secs: Some(15.0), format_note: None },
        ];
        state.items_checked[0] = vec![true; 3];
        for (width, height, show_performer) in [(240, 50, true), (154, 40, false),
            (110, 34, false), (92, 28, false)] {
            state.maximized = width == 240;
            // The compact popup shows one tab at a time; select Tracks.
            state.focus = if width < 96 || height < 30 { ExtractFocus::Items }
                else { ExtractFocus::Programs };
            let (lines, _terminal) = rendered_lines(&state, width, height);
            for (title, performer) in [("Mountain Jam", "The Allman Brothers Band"),
                ("Short Song", "Guest Performer"), ("Closing Number", "Another Performer")] {
                let line = lines.iter().find(|line| line.contains(title))
                    .unwrap_or_else(|| panic!("{width}x{height}: title {title} missing: {lines:?}"));
                if show_performer {
                    let title_end = line.find(title).unwrap() + title.len();
                    let performer_start = line.find(performer).expect("performer has its own column");
                    assert!(performer_start >= title_end, "{line}");
                } else if width < 140 {
                    assert!(!line.contains(performer), "compact view must prioritize title: {line}");
                }
            }
            if show_performer {
                let first = lines.iter().find(|line|line.contains("Mountain Jam")).unwrap();
                let second = lines.iter().find(|line|line.contains("Short Song")).unwrap();
                let third = lines.iter().find(|line|line.contains("Closing Number")).unwrap();
                let x1 = first[..first.rfind("0:00").unwrap() + "0:00".len()].chars().count();
                let x2 = second[..second.rfind("2:10").unwrap() + "2:10".len()].chars().count();
                let x3 = third[..third.rfind("12:10").unwrap() + "12:10".len()].chars().count();
                assert_eq!((x1,x2,x3),(x1,x1,x1),"start times must share one right edge");
            }
        }
        // When every performer matches the album artist, no separate
        // performer column is drawn even in a wide terminal.
        state.maximized = true;
        state.contents.album_artist = Some("The Allman Brothers Band".into());
        state.contents.presentations[0].tracks.iter_mut().for_each(|track|
            track.performer = Some("The Allman Brothers Band".into()));
        let (lines,_) = rendered_lines(&state,240,50);
        let row = lines.iter().find(|line|line.contains("Mountain Jam")).unwrap();
        assert!(!row.contains("The Allman Brothers Band"), "{row}");
    }

    #[test]
    fn result_caption_preserves_both_capacities_at_popup_widths() {
        let mut state = sacd_state(false, false);
        let temp = tempfile::tempdir().unwrap();
        state.destination = temp.path().join("out");
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let working = Path::new("/dev/shm");
        let mut terminal=Terminal::new(TestBackend::new(92,2)).unwrap();
        terminal.draw(|f|draw_results(f,&state,temp.path(),Rect::new(0,0,92,1),None,theme)).unwrap();
        let row = (0..92).map(|x|terminal.backend().buffer().get(x,0).symbol())
            .collect::<String>();
        assert!(!row.contains("working"), "shared storage: {row}");
        terminal.draw(|f|draw_results(f,&state,temp.path(),Rect::new(0,0,92,1),
            Some("Insufficient destination storage"),theme)).unwrap();
        let row = (0..92).map(|x|terminal.backend().buffer().get(x,0).symbol())
            .collect::<String>();
        assert!(row.contains("Insufficient destination storage"), "{row}");
        assert!(!row.contains(" · destination ") && !row.contains(" · working "), "{row}");
        if !working.exists() || state.native_storage_shares_destination_volume(working) {
            // The distinct-volume check requires a second mounted filesystem.
            return;
        }
        let destination_free = format_bytes(space_for(&state.destination).unwrap());
        let working_free = format_bytes(space_for(working).unwrap());
        for (width,height) in [(110,34),(96,30)] {
            let mut terminal = Terminal::new(TestBackend::new(width,height)).unwrap();
            terminal.draw(|frame| {
                let mut buttons = ButtonRenderMap::new();
                draw(frame,&state,working,&mut buttons,theme);
            }).unwrap();
            let rows = (0..height).map(|y|(0..width)
                .map(|x|terminal.backend().buffer().get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().concat()).collect::<Vec<_>>();
            let caption = rows.iter().find(|row|row.contains("destination "))
                .unwrap_or_else(||panic!("{width}x{height}: no result caption: {rows:?}"));
            assert!(caption.contains(&format!("destination {destination_free} free")),"{caption}");
            assert!(caption.contains(&format!("working {working_free} free")),"{caption}");
        }
    }

    #[test]
    fn visual_revision_focus_surfaces_output_and_genre_count() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let colors=extract_colors(theme);
        let mut state=sacd_state(false,false);
        state.contents.genre=Some("Prog; Rock; Art Rock; Jazz; Live".into());
        state.contents.album_artist=Some("Genesis".into());
        state.contents.presentations[0].tracks=(1..=8).map(|n|DiscTrack {
            number:n,title:Some(format!("Track {n:02}")),
            performer:Some("Genesis".into()),duration_secs:Some(300.0),format_note:None,
        }).collect();
        state.items_checked[0]=vec![true;8];
        state.focus=ExtractFocus::Items;
        let (lines,terminal)=rendered_lines(&state,154,36);
        let text=lines.join("\n");
        assert!(text.contains("Prog +4"),"{text}");
        assert!(text.contains("Overwrite"),"{text}");
        assert!(text.contains("Folder") && text.contains("Naming"),"{text}");
        let pane_y=lines.iter().position(|line|line.contains("AREAS") && line.contains("TRACKS"))
            .expect("both pane headings") as u16;
        let buf=terminal.backend().buffer();
        let left=lines[pane_y as usize][..lines[pane_y as usize].find("AREAS").unwrap()].chars().count() as u16;
        let right=lines[pane_y as usize][..lines[pane_y as usize].find("TRACKS").unwrap()].chars().count() as u16;
        assert_eq!(buf.get(left,pane_y).bg,colors.unfocused);
        assert_eq!(buf.get(right,pane_y).bg,colors.focused);
        let top=lines.iter().position(|line|line.contains("extract tracks"))
            .expect("title") as u16;
        let bottom=lines.iter().rposition(|line|line.contains('└'))
            .expect("closed bottom frame") as u16;
        let bottom_margin=36-bottom-1;
        assert!(top>=1 && bottom_margin>=1,
            "expanded dialog must leave the terminal margin");
        assert!(top.abs_diff(bottom_margin)<=1,
            "expanded dialog must remain vertically centered");
        assert!(!lines.iter().any(|line|line.contains("Track 01") &&
            line.contains("Genesis")),"artist performer column should be hidden");
        state.focus=ExtractFocus::Programs;
        let (other,terminal)=rendered_lines(&state,154,36);
        let pane_y=other.iter().position(|line|line.contains("AREAS") && line.contains("TRACKS"))
            .unwrap() as u16;
        let left=other[pane_y as usize][..other[pane_y as usize].find("AREAS").unwrap()].chars().count() as u16;
        let right=other[pane_y as usize][..other[pane_y as usize].find("TRACKS").unwrap()].chars().count() as u16;
        assert_eq!(terminal.backend().buffer().get(left,pane_y).bg,colors.focused);
        assert_eq!(terminal.backend().buffer().get(right,pane_y).bg,colors.unfocused);
    }

    #[test]
    fn wide_destination_keeps_template_and_all_dividers_have_junctions() {
        let mut state=sacd_state(false,false);
        state.destination=Path::new("/tmp/this-is-a-very-long-destination-folder-for-extraction")
            .to_path_buf();
        state.naming_template="{track} - {title}".into();
        let (lines,_)=rendered_lines(&state,154,36);
        let text=lines.join("\n");
        assert!(text.contains("{track} - {title}"),"{text}");
        assert!(text.contains("Overwrite"),"{text}");
        assert!(text.contains('┬'),"{text}");
        assert!(text.contains('┴'),"{text}");
        let bottom=lines.iter().find(|line|line.contains('└'))
            .expect("bottom border");
        assert!(bottom.contains('┘'),"{bottom}");
    }


    /// Locate a word in the actual terminal cells and check every glyph's
    /// foreground/background pair, not merely the requested palette colors.
    fn word_cells(
        buffer: &ratatui::buffer::Buffer,
        word: &str,
        context: &str,
    ) -> (u16, u16) {
        for y in 0..buffer.area.height {
            let line=(0..buffer.area.width)
                .map(|x|buffer.get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            if let Some(at)=line.find(word) {
                let start=line[..at].chars().count() as u16;
                for x in start..start+word.chars().count() as u16 {
                    let cell=buffer.get(x,y);
                    let ratio=contrast_ratio(cell.fg,cell.bg);
                    assert!(ratio>=4.5,
                        "{context}: {word:?} at ({x},{y}) has {ratio:.3}:1; fg={:?}, bg={:?}",
                        cell.fg,cell.bg);
                }
                return (start,y);
            }
        }
        panic!("{context}: word {word:?} absent from terminal");
    }

    #[test]
    fn r22_form_label_tiers_and_focused_controls_across_themes_and_viewports() {
        for slug in ["tokyo-night","gruvbox","solarized-dark","tokyo-night-day","everforest-light"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            for (width,height,wide) in [(72,24,false),(110,34,false),(154,40,true)] {
                let mut state=sacd_state(true,false);
                state.focus=ExtractFocus::Output;
                state.destination="out".into();
                state.naming_template="{track}-{title}".into();
                for index in 0..state.output_rows().len() {
                    state.output_cursor=index;
                    let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                    let mut buttons=ButtonRenderMap::new();
                    terminal.draw(|f|draw_output(f,&state,
                        Rect::new(1,3,width-2,state.output_rows().len() as u16+2),
                        &mut buttons,theme,wide)).unwrap();
                    let buffer=terminal.backend().buffer();
                    let name=state.output_rows()[index];
                    let context=format!("{slug} {width}x{height} selected={name}");
                    let (x,y)=word_cells(buffer,name,&context);
                    let region_bg=extract_colors(theme).focused;
                    assert_eq!(buffer.get(x,y).bg,region_bg,"{context}");
                    assert_eq!(buffer.get(x,y).fg,
                        if matches!(name,"Folder"|"Naming") { readable_word(theme.info,region_bg) }
                        else { word_tiers(theme,extract_colors(theme),region_bg).label },
                        "{context}: only focused unedited text-field labels use the info accent");
                    // Compact viewports scroll the form to keep the active
                    // control in view. Only assert labels actually rendered.
                    for (other_index,other) in state.output_rows().iter().enumerate() {
                        if *other != name && output_cell(other,other_index,&state.output_rows(),
                            Rect::new(1,3,width-2,state.output_rows().len() as u16+2),
                            wide,index).is_some() {
                            word_cells(buffer,other,&context);
                        }
                    }
                    if name=="Tags" {word_cells(buffer,"fields",&context);}
                    if name=="Naming" {word_cells(buffer,"{track}",&context);}
                    if wide {
                        word_cells(buffer,"Stop if a file exists",&context);
                        let rect=buttons.button_rect(TuiButton::DiscBrowserOverwrite)
                            .expect("cycler value visible");
                        let active=name=="Overwrite";
                        assert_eq!(buffer.get(rect.x+1,rect.y).bg,
                            if active {theme.input_focused_bg}else{theme.input_unfocused_bg},
                            "{context}: focus belongs only to the cycler value");
                    }
                }
                let state=sacd_state(true,false);
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                terminal.draw(|f|draw_header(f,&state,Rect::new(1,2,width-2,3),false,theme)).unwrap();
                let buffer=terminal.backend().buffer();
                for word in ["Example Album","Protection","Diagnostics"] {
                    word_cells(buffer,word,&format!("{slug} header"));
                }
            }
        }
    }

    #[test]
    fn r34_sacd_details_clip_after_label_expansion_and_leave_two_blank_cells() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.contents.presentations[0].extract.sacd_header_details=Some(format!(
            "Catalog AL-001 · Date 1999 · Publisher {} · Hybrid SACD", "長い出版社".repeat(60)));
        for width in [45,80,102] {
            let mut terminal=Terminal::new(TestBackend::new(130,12)).unwrap();
            let rect=Rect::new(3,2,width,3);
            terminal.draw(|frame|draw_header(frame,&state,rect,false,theme)).unwrap();
            let buffer=terminal.backend().buffer();
            for x in rect.right()-2..rect.right() {
                assert_eq!(buffer.get(x,rect.y+1).symbol()," ",
                    "second header line must leave two blank frame-side cells, width={width}");
            }
            let visible=(rect.x..rect.right()-2).map(|x|
                buffer.get(x,rect.y+1).symbol().to_owned()).collect::<String>();
            assert!(visible.contains("Catalog"),"{visible}");
            assert!(visible.contains('…'),"long publisher must truncate, width={width}: {visible}");
        }
    }

    #[test]
    fn one_and_multiple_item_program_tallies_end_at_the_program_pane_right_edge() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let area=Rect::new(4,5,48,16);
        for count in [1usize,2] {
            let mut sacd=sacd_state(false,false);
            if count==2 {
                let mut second=sacd.contents.presentations[0].tracks[0].clone();
                second.number=2;
                second.title=Some("Second".into());
                sacd.contents.presentations[0].tracks.push(second);
            }
            for (state,unit) in [(sacd,"track"),(chapter_state(count),"chapter")] {
                let mut terminal=Terminal::new(TestBackend::new(110,34)).unwrap();
                terminal.draw(|f|{
                    let mut buttons=ButtonRenderMap::new();
                    draw_programs(f,&state,area,&mut buttons,theme);
                }).unwrap();
                let buffer=terminal.backend().buffer();
                let suffix=format!("{count} {unit}{}",if count==1{""}else{"s"});
                let (_,y)=word_cells(buffer,&suffix,"right-aligned program total");
                let final_letter=suffix.chars().last().unwrap().to_string();
                assert_eq!(buffer.get(area.x+area.width-3,y).symbol(),final_letter.as_str(),
                    "{suffix} must end at the padded right edge of the program pane");
            }
        }
    }


    #[test]
    fn r16_sacd_format_summary_matches_the_native_job_output_extensions() {
        use crate::disc::model::SacdAreaId;
        let base=sacd_state(false,false).contents;
        let mut dst=base.clone();
        dst.presentations[0].format.codec=Some("DST".into());
        dst.presentations[0].tracks[0].format_note=Some("DST compressed".into());
        let mut mixed=base.clone();
        let mut second=dst.presentations[0].clone();
        second.id=PresentationId::SacdArea(SacdAreaId::MultiChannel);
        second.format.channels=Some(5);
        second.format.channel_layout=Some("5.0".into());
        mixed.presentations.push(second);

        for (contents,keep,expected,descriptor) in [
            (base.clone(),vec![false],"DSF","native DSD"),
            (dst.clone(),vec![false],"DSF","DST decoded"),
            (dst.clone(),vec![true],"DFF","keep selected DST"),
            (mixed.clone(),vec![false,true],"DSF / DFF","keep selected DST"),
        ] {
            let mut state=DiscBrowserState::new(contents,"disc.iso".into());
            state.keep_dst=keep;
            let planned=state.native_job("/tmp/extract-test".into());
            let extensions=planned.programs.iter().map(|p|
                if p.keep_dst {"DFF"} else {"DSF"}).collect::<std::collections::BTreeSet<_>>();
            let expected_set=expected.split(" / ").collect::<std::collections::BTreeSet<_>>();
            assert_eq!(extensions,expected_set,"{expected}: native job extensions");
            let summary=option_value(&state,"Format");
            assert_eq!(summary.split(" · ").next(),Some(expected),"{summary}");
            assert!(summary.contains(descriptor),"{summary}");
        }
        // An unchecked track must not contribute an extension even when its
        // area remains checked; native_job drops empty programs too.
        let mut partial=DiscBrowserState::new(mixed,"disc.iso".into());
        partial.keep_dst=vec![false,true];
        partial.items_checked[0][0]=false;
        let planned=partial.native_job("/tmp/extract-test".into());
        assert_eq!(planned.programs.len(),1);
        assert!(planned.programs[0].keep_dst);
        assert!(option_value(&partial,"Format").starts_with("DFF ·"));
    }

    #[test]
    fn r16_sacd_program_layout_shows_two_five_and_six_channels() {
        let mut state=sacd_state(false,false);
        for (channels,layout,expected) in [
            (2,"Stereo","2.0"), (5,"5.0","5.0"), (6,"5.1","5.1"),
            (5,"Multichannel","5.0"), (6,"Multichannel","5.1"),
        ] {
            state.contents.presentations[0].format.channels=Some(channels);
            state.contents.presentations[0].format.channel_layout=Some(layout.into());
            let program=&state.contents.presentations[0];
            let details=program_info(program,DiscFormat::Sacd);
            assert!(details.ends_with(&format!(" · {expected}")),"{details}");
            let (lines,_)=rendered_lines(&state,154,40);
            assert!(lines.join("\n").contains(&format!("DSD64 · {expected}")),
                "{channels} channels absent from rendered area");
        }
    }

    #[test]
    fn r22_form_controls_have_independent_focus_and_hitboxes() {
        let mut state=sacd_state(false,false);
        state.focus=ExtractFocus::Output;
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(72,24),(110,34),(154,40)] {
            let mut positions=Vec::new();
            for (name,target) in [("Overwrite",TuiButton::DiscBrowserOverwrite),
                ("Folder",TuiButton::DiscBrowserFolderField),
                ("Naming",TuiButton::DiscBrowserNaming)] {
                state.output_cursor=state.output_rows().iter().position(|row|*row==name).unwrap();
                let mut buttons=ButtonRenderMap::new();
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
                let rect=buttons.button_rect(target).expect("focused control is visible");
                assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(target),
                    "{width}x{height}: {target:?} at {rect:?}");
                positions.push(rect);
            }
            if width>=154 {
                assert_eq!(positions[2].y,positions[1].y+2);
                assert_eq!(positions[2].x,positions[1].x);
                assert!(positions[0].x>positions[1].x);
            }
        }
    }

    #[test]
    fn r34_overwrite_cycler_hitboxes_stay_fixed_as_values_change() {
        use crate::convert::pipeline::native_extract::NativeOverwritePolicy as Policy;
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (area,wide) in [(Rect::new(2,4,120,8),true),
            (Rect::new(2,4,80,10),false)] {
            let mut state=sacd_state(false,false);
            state.focus=ExtractFocus::Output;
            state.output_cursor=state.output_rows().iter().position(|name|*name=="Overwrite").unwrap();
            let mut positions=None;
            for policy in [Policy::Fail,Policy::Skip,Policy::Replace] {
                state.overwrite_policy=policy;
                let mut terminal=Terminal::new(TestBackend::new(130,20)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|f|draw_output(f,&state,area,&mut buttons,theme,wide)).unwrap();
                let targets=[TuiButton::DiscBrowserOverwritePrev,TuiButton::DiscBrowserOverwrite,
                    TuiButton::DiscBrowserOverwriteNext,TuiButton::DiscBrowserOverwriteHelp];
                let rects=targets.iter().map(|target| {
                    let rect=buttons.button_rect(*target).expect("cycler target must be visible");
                    assert!(rect.y>=area.y && rect.y<area.bottom());
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(*target));
                    rect
                }).collect::<Vec<_>>();
                assert_eq!(rects[1].width,22);
                assert_eq!(rects[2].x,rects[0].x+25);
                assert_eq!(rects[3].x,rects[2].x+2);
                if let Some(first)=&positions { assert_eq!(&rects,first); }
                else { positions=Some(rects); }
            }
        }
    }

    #[test]
    fn r34_overwrite_help_sheet_shields_underlying_form_clicks() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.focus=ExtractFocus::Output;
        state.output_cursor=state.output_rows().iter().position(|name|*name=="Overwrite").unwrap();
        state.overwrite_open=true;
        let mut terminal=Terminal::new(TestBackend::new(160,45)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|f|draw(f,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
        let sheet=buttons.button_rect(TuiButton::DiscBrowserOverwriteHelpPassive)
            .expect("help sheet must be visible");
        assert_eq!(buttons.find_button_at(sheet.x+1,sheet.y+1),
            Some(TuiButton::DiscBrowserOverwriteHelpPassive));
        let popup=buttons.button_rect(TuiButton::DiscBrowserOverwriteHelpOutside)
            .expect("the help sheet must shield the popup");
        assert!(sheet.x>=popup.x && sheet.y>=popup.y
            && sheet.right()<=popup.right() && sheet.bottom()<=popup.bottom());
        let arrow=buttons.button_rect(TuiButton::DiscBrowserOverwriteNext).unwrap();
        assert_eq!(buttons.find_button_at(arrow.x,arrow.y),
            Some(TuiButton::DiscBrowserOverwriteHelpOutside));
    }

    #[test]
    fn round_5_downmix_arrows_have_stable_positions_and_click_targets() {
        use crate::convert::pipeline::DvdaDownmixPolicy as Policy;
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.contents.format=DiscFormat::DvdAudio;
        state.contents.presentations[0].format.channels=Some(6);
        state.focus=ExtractFocus::Output;
        let index=state.output_rows().iter().position(|name|*name=="Downmix")
            .expect("DVD-A multichannel Downmix row");
        state.output_cursor=index;
        let max_value=crate::tui::disc_browser_actions::NATIVE_DVDA_DOWNMIX_POLICIES.iter()
            .map(|policy|policy.as_str().len() as u16).max().unwrap();
        for (area,wide) in [(Rect::new(2,3,150,10),true),
            (Rect::new(2,3,78,12),false),
            (Rect::new(2,3,36,12),false)] {
            let mut positions=None;
            for policy in [Policy::None,Policy::Auto,
                Policy::FooInputDvdaCompatible,Policy::FfmpegDefault] {
                state.downmix=policy;
                let mut terminal=Terminal::new(TestBackend::new(155,20)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|frame|draw_output(frame,&state,area,&mut buttons,theme,wide)).unwrap();
                let prev=buttons.button_rect(TuiButton::DiscBrowserDownmixPrev)
                    .expect("left Downmix arrow");
                let next=buttons.button_rect(TuiButton::DiscBrowserDownmixNext)
                    .expect("right Downmix arrow");
                let field=buttons.button_rect(TuiButton::DiscBrowserToggle(index))
                    .expect("Downmix value field");
                assert_eq!(next.x-prev.x,max_value.min(field.width-4)+3);
                assert!(next.right()<=field.right());
                assert_eq!(prev.y,next.y);
                assert_eq!(terminal.backend().buffer().get(prev.x,prev.y).symbol(),"◂");
                assert_eq!(terminal.backend().buffer().get(next.x,next.y).symbol(),"▸");
                assert_eq!(buttons.find_button_at(prev.x,prev.y),
                    Some(TuiButton::DiscBrowserDownmixPrev));
                assert_eq!(buttons.find_button_at(next.x,next.y),
                    Some(TuiButton::DiscBrowserDownmixNext));
                if field.width>=max_value+4 {
                    let rendered=(prev.x+2..prev.x+2+policy.as_str().len() as u16)
                        .map(|x|terminal.backend().buffer().get(x,prev.y).symbol().to_owned())
                        .collect::<Vec<_>>().concat();
                    assert_eq!(rendered,policy.as_str());
                }
                if let Some(previous)=positions { assert_eq!((prev,next),previous); }
                else {positions=Some((prev,next));}
            }
        }
    }

    #[test]
    fn r22_optional_dvda_controls_fit_the_wide_form() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.contents.format = DiscFormat::DvdAudio;
        state.contents.copy_protection.description = "CPPM MKB detected".into();
        state.focus = ExtractFocus::Output;
        assert_eq!(state.wide_output_height(), 9);
        let index = state.output_rows().iter().position(|name| *name == "Assume decrypted").unwrap();
        state.output_cursor = index;
        let mut buttons = ButtonRenderMap::new();
        let mut terminal = Terminal::new(TestBackend::new(155, 30)).unwrap();
        terminal.draw(|frame| draw_output(frame, &state, Rect::new(2, 3, 150, 10),
            &mut buttons, theme, true)).unwrap();
        let rect = buttons.button_rect(TuiButton::DiscBrowserToggle(index))
            .expect("the conditional row must stay visible at its fixed position");
        assert_eq!(rect.y, 11);
        state.contents.presentations[0].format.channels = Some(6);
        assert_eq!(state.wide_output_height(), 9);
        assert!(state.output_rows().contains(&"Downmix"));
    }

    #[test]
    fn round_2_naming_result_prefers_authored_name_without_paging() {
        use crate::tui::app::{TemplateBuilderFocus, TemplateBuilderState, TemplateTarget};
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.destination = std::path::PathBuf::from(
            "/home/user/Music/Extracted/A Carefully Organized Listening Library");
        state.contents.album_artist = Some("pink floyd and the very long ensemble name".into());
        state.contents.album_title = Some("wish you were here and other remarkable recordings".into());
        state.contents.year = Some("1975".into());
        state.contents.presentations[0].tracks[0].title = Some(
            "shine on you crazy diamond parts one through nine and a long bonus title".into());
        state.naming_template =
            "%ALBUM_ARTIST% - %ALBUM% (%YEAR%)/%TRACKNUMBER% - %TITLE%".into();
        state.focus = ExtractFocus::Output;
        state.output_cursor = state.output_rows().iter().position(|row| *row == "Naming").unwrap();
        for (width, height) in [(80u16, 24u16), (100, 30)] {
            for capitalize in [false, true] {
                state.capitalize_naming = capitalize;
                let full_path = state.native_preview_path_for_template_case(
                    &state.naming_template, capitalize).unwrap();
                let name = state.native_preview_naming_for_template_case(
                    &state.naming_template, capitalize).unwrap();
                assert!(full_path.starts_with(&state.destination));
                assert!(name.ends_with(".dsf"));
                assert!(!name.contains("/home/user/"));

                let display = crate::tui::template_builder::destination_path_preview(
                    &full_path, &state.destination, width.saturating_sub(8) as usize,
                    || Some(name.clone()));
                assert!(!display.starts_with("/home/user/Music/Extracted/"),
                    "a long destination should give priority to the generated name");
                let name_prefix = name.chars().take(12).collect::<String>();
                assert!(display.contains(name_prefix.as_str()));
                assert!(crate::tui::display_width::width(&display)
                    <= width.saturating_sub(8) as usize);

                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut builder = TemplateBuilderState::new(
                    TemplateTarget::Filename, &state.naming_template,
                    TemplateBuilderFocus::TemplateInput);
                builder.return_to_extract = Some(Box::new(state.clone()));
                builder.capitalize = capitalize;
                let popup = crate::tui::template_builder::builder_popup(
                    Rect::new(0, 0, width, height), &builder).unwrap();
                let mut buttons = ButtonRenderMap::new();
                terminal.draw(|frame|crate::tui::template_builder::draw_template_builder(
                    frame, &builder, &mut buttons, theme)).unwrap();
                let row_y = popup.y + 4;
                let row = (popup.x+1..popup.x+popup.width-1)
                    .map(|x| terminal.backend().buffer().get(x,row_y).symbol().to_owned())
                    .collect::<Vec<_>>().join("");
                assert!(row.contains(name_prefix.as_str()),
                    "Custom preview shows the name rather than destination: {row}");
                assert!(!row.contains("/home/user/"), "Custom must not show the destination");
                for button in [TuiButton::TemplateBuilderApply, TuiButton::TemplateBuilderSave,
                    TuiButton::TemplateBuilderSavedTemplates, TuiButton::TemplateBuilderClear] {
                    let rect = buttons.button_rect(button).expect("Custom footer must remain visible");
                    assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(button));
                }

                // Check the real Extract popup as well as its standalone Naming row:
                // responsive tabs and the rest of its footer retain their hitboxes.
                buttons.clear();
                terminal.draw(|frame|draw(frame, &state, Path::new("/tmp"), &mut buttons, theme)).unwrap();
                for button in [TuiButton::DiscBrowserConvert, TuiButton::DiscBrowserOptions,
                    TuiButton::DiscBrowserClose] {
                    let rect = buttons.button_rect(button).expect("Extract footer must remain visible");
                    assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(button));
                }
            }
        }
    }

    #[test]
    fn round_2_two_sacd_areas_keep_complete_authored_name_on_overflow() {
        // Use the same two-area SACD layout as the draft-folder regression.
        let mut contents = sacd_state(false, false).contents;
        contents.presentations[0].label = "Stereo".into();
        let mut multi = contents.presentations[0].clone();
        multi.id = PresentationId::SacdArea(SacdAreaId::MultiChannel);
        multi.label = "Multichannel".into();
        contents.presentations.push(multi);
        contents.album_title = Some("wish you were here".into());
        for presentation in &mut contents.presentations {
            presentation.tracks[0].title = Some("shine on you crazy diamond".into());
        }
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.destination = "/home/user/Music/Extracted/Long Collection".into();
        state.naming_template = "%ALBUM%/%TRACKNUMBER% - %TITLE%".into();
        state.cursor = 1;
        state.focus = ExtractFocus::Output;
        state.output_cursor = state.output_rows().iter().position(|row| *row == "Naming").unwrap();
        assert_eq!(state.native_job(std::path::PathBuf::new()).programs.len(), 2);

        use crate::tui::display_width::width;
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut uncased = None;
        for capitalize in [false, true] {
            state.capitalize_naming = capitalize;
            let path = state.native_preview_path().unwrap();
            let authored = state.native_preview_naming_for_template_case(
                &state.naming_template, capitalize).unwrap();
            if !capitalize { uncased = Some(authored.clone()); }
            else { assert_ne!(Some(&authored), uncased.as_ref()); }
            assert!(path.starts_with(state.destination.join("Multichannel")), "{}", path.display());
            assert!(path.ends_with(Path::new(&authored)), "{} vs {authored}", path.display());

            let relative = path.strip_prefix(&state.destination).unwrap().to_string_lossy();
            let cells = width(&authored) + 6;
            assert!(width(&relative) > cells);
            let visible = crate::tui::template_builder::destination_path_preview(
                &path, &state.destination, cells, || Some(authored.clone()));
            assert!(visible.ends_with(&authored), "{visible}");
            assert!(visible.contains(".../"), "{visible}");
            assert!(width(&visible) <= cells);

            // The actual Naming row uses the same overflow path. Its width
            // is chosen to fit the authored result but not the program folder.
            let area = Rect::new(2, 2, (cells + 6) as u16, 18);
            let rows = state.output_rows();
            let naming = output_cell("Naming", state.output_cursor, &rows, area, false,
                state.output_cursor).unwrap();
            assert_eq!(naming.width as usize - 2, cells);
            let mut terminal = Terminal::new(TestBackend::new(area.right() + 2, 23)).unwrap();
            terminal.draw(|frame| draw_output_with_hover(frame, &state, area,
                &mut ButtonRenderMap::new(), theme, false, None)).unwrap();
            let caption = (naming.x..naming.right())
                .map(|x| terminal.backend().buffer().get(x, naming.y + 1).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            assert!(caption.contains(&authored), "{caption}");
            assert!(caption.contains(".../"), "{caption}");
        }

        // An explicit area token removes automatic program folders; the
        // authored result remains the actual worker-relative path.
        let explicit = "%AREA%/%ALBUM%/%TRACKNUMBER% - %TITLE%";
        let actual = state.native_preview_path_for_template(explicit).unwrap();
        let authored = state.native_preview_naming_for_template_case(
            explicit, state.capitalize_naming).unwrap();
        assert_eq!(actual.strip_prefix(&state.destination).unwrap(), Path::new(&authored));
    }

    #[test]
    fn r27_naming_preview_uses_worker_path_and_both_shared_controls_have_hitboxes() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.destination = "/tmp/r27-album".into();
        state.naming_template = "%ARTIST%/%TRACKNUMBER% - %TITLE%.%EXTENSION%".into();
        state.focus = ExtractFocus::Output;
        state.output_cursor = state.output_rows().iter().position(|row|*row=="Naming").unwrap();
        let expected = state.native_output_paths();
        assert_eq!(expected.len(), 1);
        assert_eq!(state.native_preview_path().as_ref(), expected.first());
        let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
        let text = (0..44).map(|y|(0..160)
            .map(|x|terminal.backend().buffer().get(x,y).symbol().to_owned())
            .collect::<Vec<_>>().concat()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("→ /tmp/r27-album/Untitled/01 - Track.dsf"),"{text}");
        for button in [TuiButton::DiscBrowserNamingLoad,TuiButton::DiscBrowserNamingCustom] {
            let rect = buttons.button_rect(button).expect("shared naming control must be clickable");
            assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(button));
        }
    }

    #[test]
    fn r27_duplicate_selected_media_paths_block_extract_before_queueing() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.destination = "/tmp/r27-album".into();
        let duplicate = state.contents.presentations[0].tracks[0].clone();
        state.contents.presentations[0].tracks.push(duplicate);
        state.items_checked[0] = vec![true, true];
        state.naming_template = "%TITLE%".into();
        assert_eq!(state.native_duplicate_count(), 2);
        assert!(state.blockers(Path::new("/tmp")).iter().any(|p|
            p.contains("2 tracks would get the same name")));
        let (lines, _) = rendered_lines(&state, 160, 44);
        assert!(lines.join("\n").contains("2 tracks would get the same name"));
        assert!(lines.join("\n").contains("→ "), "the destination remains visible during a collision");
        let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
        terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),
            &mut ButtonRenderMap::new(),theme)).unwrap();
        assert!(!state.footer_extract_enabled.get(),"collision must disable Extract");
        state.naming_template = "%TRACKNUMBER% - %TITLE%".into();
        assert_eq!(state.native_duplicate_count(),0);
        state.items_checked[0][1] = false;
        state.naming_template = "%TITLE%".into();
        assert_eq!(state.native_duplicate_count(),0,"unchecked tracks cannot cause false collisions");
    }

    #[test]
    fn r30_tags_sheet_renders_actionable_controls_and_shields_underlying_window() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.tags = true;
        state.panel = Some(ExtractPanel::Tags);
        state.tag_fields = 0x7f;
        let mut terminal = Terminal::new(TestBackend::new(132, 44)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        let popup = Rect::new(6, 5, 116, 33);
        terminal.draw(|frame| draw_tags_sheet(frame, &state, popup, &mut buttons, theme)).unwrap();
        for control in [TagSheetControl::Transfer, TagSheetControl::Source,
            TagSheetControl::Field(0), TagSheetControl::Field(8), TagSheetControl::All,
            TagSheetControl::None, TagSheetControl::Artwork, TagSheetControl::EditValues,
            TagSheetControl::MusicBrainz, TagSheetControl::Done] {
            let index = state.tag_sheet_controls().iter().position(|item| *item == control).unwrap();
            let target = TuiButton::DiscBrowserTagSheetControl(index);
            let rect = buttons.button_rect(target)
                .unwrap_or_else(|| panic!("missing hitbox for {control:?}"));
            assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(target));
        }
        assert_eq!(buttons.find_button_at(0, 0), Some(TuiButton::DiscBrowserTagSheetOutside));
        let lines = (0..44).map(|y| (0..132).map(|x|
            terminal.backend().buffer().get(x, y).symbol().to_owned()
        ).collect::<Vec<_>>().concat()).collect::<Vec<_>>().join("\n");
        for label in ["Transfer tags", "Catalog", "Publisher", "no value",
            "Embed artwork", "Edit values", "MusicBrainz", "Done"] {
            assert!(lines.contains(label), "missing {label:?} from Tags sheet");
        }
        assert!(!lines.contains("1-7  Fields"));

        state.tag_source_open = true;
        buttons.clear();
        terminal.draw(|frame| draw_tags_sheet(frame, &state, popup, &mut buttons, theme)).unwrap();
        for index in 0..2 {
            let target = TuiButton::DiscBrowserTagSheetSourceChoice(index);
            let rect = buttons.button_rect(target).expect("source menu item hitbox");
            assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(target));
        }
    }

    #[test]
    fn r30c_sheet_pills_and_parent_dimming_work_in_both_themes_and_sizes() {
        for slug in ["tokyo-night", "tokyo-night-day"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            for (width,height,popup) in [(110,34,Rect::new(4,3,102,28)),
                (132,44,Rect::new(6,5,116,33))] {
                let mut state=sacd_state(false,false);
                state.tags=true;
                state.sacd_dff=true;
                state.panel=Some(ExtractPanel::Tags);
                let controls=state.tag_sheet_controls();
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                for control in [TagSheetControl::All,TagSheetControl::None,TagSheetControl::UseDsf] {
                    let index=controls.iter().position(|v|*v==control).unwrap();
                    state.panel_cursor=index;
                    let mut buttons=ButtonRenderMap::new();
                    terminal.draw(|frame| {
                        frame.render_widget(Paragraph::new("BACKGROUND EXTRACT WINDOW")
                            .style(Style::default().fg(theme.value).bg(theme.panel_bg)),
                            Rect::new(popup.x,popup.y,popup.width,1));
                        draw_tags_sheet(frame,&state,popup,&mut buttons,theme);
                    }).unwrap();
                    let hit=TuiButton::DiscBrowserTagSheetControl(index);
                    let rect=buttons.button_rect(hit).expect("focused pill hitbox");
                    let cell=terminal.backend().buffer().get(rect.x,rect.y);
                    assert_eq!(cell.bg,theme.pill_active_bg,"{slug} {width} {control:?}");
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(hit));
                    // Text outside the focused sheet is still the dimmed parent.
                    let under=terminal.backend().buffer().get(popup.x,popup.y);
                    assert_eq!(under.fg,blend_color(theme.value,theme.panel_bg,3,4));
                    if control==TagSheetControl::UseDsf {
                        assert_ne!(buttons.find_button_at(rect.x.saturating_sub(2),rect.y),Some(hit),
                            "the warning prose must not be a clickable button");
                    }
                    for other in [TagSheetControl::All,TagSheetControl::None,TagSheetControl::UseDsf] {
                        let other_index=controls.iter().position(|v|*v==other).unwrap();
                        let other_rect=buttons.button_rect(TuiButton::DiscBrowserTagSheetControl(other_index))
                            .expect("unfocused pill still visible");
                        if other != control {
                            assert_eq!(terminal.backend().buffer().get(other_rect.x,other_rect.y).bg,
                                theme.pill_neutral_bg,"unfocused action must retain button styling");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn neutral_extract_buttons_hover_without_changing_their_hit_regions() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.tags = true;
        let area = Rect::new(2, 2, 150, 10);
        let mut terminal = Terminal::new(TestBackend::new(154, 22)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame| draw_output_with_hover(frame, &state, area,
            &mut buttons, theme, true, Some(TuiButton::DiscBrowserNamingLoad))).unwrap();
        let hovered = buttons.button_rect(TuiButton::DiscBrowserNamingLoad)
            .expect("Load template visible");
        assert_eq!(buttons.find_button_at(hovered.x, hovered.y),
            Some(TuiButton::DiscBrowserNamingLoad));
        assert_eq!(terminal.backend().buffer().get(hovered.x, hovered.y).bg,
            theme.pill_neutral_hover_bg);
        let other = buttons.button_rect(TuiButton::DiscBrowserNamingCustom)
            .expect("Custom visible");
        assert_eq!(terminal.backend().buffer().get(other.x, other.y).bg,
            theme.pill_neutral_bg);

        state.panel = Some(ExtractPanel::Tags);
        let controls = state.tag_sheet_controls();
        let all = controls.iter().position(|c| *c == TagSheetControl::All).unwrap();
        let none = controls.iter().position(|c| *c == TagSheetControl::None).unwrap();
        state.panel_cursor = all;
        let popup = Rect::new(6, 5, 116, 33);
        let mut sheet_terminal = Terminal::new(TestBackend::new(132, 44)).unwrap();
        buttons.clear();
        sheet_terminal.draw(|frame| draw_tags_sheet_with_hover(frame, &state,
            popup, &mut buttons, theme,
            Some(TuiButton::DiscBrowserTagSheetControl(none)))).unwrap();
        let hovered = buttons.button_rect(TuiButton::DiscBrowserTagSheetControl(none))
            .expect("None pill visible");
        assert_eq!(buttons.find_button_at(hovered.x, hovered.y),
            Some(TuiButton::DiscBrowserTagSheetControl(none)));
        assert_eq!(sheet_terminal.backend().buffer().get(hovered.x, hovered.y).bg,
            theme.pill_neutral_hover_bg);
        let focused = buttons.button_rect(TuiButton::DiscBrowserTagSheetControl(all))
            .expect("All pill visible");
        assert_eq!(sheet_terminal.backend().buffer().get(focused.x, focused.y).bg,
            theme.pill_active_bg);
    }

    #[test]
    fn r30c_artwork_preview_names_source_preference_not_folder_as_final() {
        use crate::convert::pipeline::native_extract::native_artwork_path;
        use crate::convert::pipeline::types::AlbumMetadata;
        use crate::convert::pipeline::STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY;
        let temp=tempfile::tempdir().unwrap();
        let iso=temp.path().join("disc.iso");
        let folder_cover=temp.path().join("cover.png");
        let source_art=temp.path().join("source-art.jpg");
        std::fs::write(&iso,b"disc").unwrap();
        std::fs::write(&folder_cover,b"folder").unwrap();
        std::fs::write(&source_art,b"source").unwrap();
        let mut album=AlbumMetadata::default();
        album.extra.insert(STAGED_SOURCE_ARTWORK_PATH_EXTRA_KEY.into(),
            source_art.to_string_lossy().into_owned());
        assert_eq!(native_artwork_path(&album,&iso),Some(source_art));
        let mut state=sacd_state(false,false);
        state.tags=true;
        state.tag_preview_artwork=Some(folder_cover);
        state.panel=Some(ExtractPanel::Tags);
        let (lines,_)=rendered_lines(&state,132,44);
        let sheet=lines.join("\n");
        assert!(sheet.contains("source art preferred"),"{sheet}");
        assert!(sheet.contains("folder fallback"),"{sheet}");
    }

    #[test]
    fn r30_tags_sheet_hides_children_when_transfer_off_and_exposes_dff_recovery() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = sacd_state(false, false);
        state.panel = Some(ExtractPanel::Tags);
        state.tags = false;
        let mut terminal = Terminal::new(TestBackend::new(132, 44)).unwrap();
        let popup = Rect::new(6, 5, 116, 33);
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame| draw_tags_sheet(frame, &state, popup, &mut buttons, theme)).unwrap();
        assert_eq!(state.tag_sheet_controls().len(), 4);
        assert!(!state.tag_sheet_controls().contains(&TagSheetControl::Source));
        assert!(state.tag_sheet_controls().contains(&TagSheetControl::EditValues));
        state.tags = true;
        state.sacd_dff = true;
        assert!(!state.native_tags_available());
        assert!(state.tag_sheet_controls().contains(&TagSheetControl::UseDsf));
        buttons.clear();
        terminal.draw(|frame| draw_tags_sheet(frame, &state, popup, &mut buttons, theme)).unwrap();
        let index = state.tag_sheet_controls().iter().position(|control|
            *control == TagSheetControl::UseDsf).unwrap();
        let rect = buttons.button_rect(TuiButton::DiscBrowserTagSheetControl(index))
            .expect("DFF warning must have an actionable Use DSF control");
        assert_eq!(buttons.find_button_at(rect.x, rect.y),
            Some(TuiButton::DiscBrowserTagSheetControl(index)));
    }

    #[test]
    fn r28_collision_and_unknown_token_never_replace_live_path_preview() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.destination="/tmp/r28-preview".into();
        let mut second=state.contents.presentations[0].tracks[0].clone();
        second.number=2;
        second.title=Some("song".into());
        state.contents.presentations[0].tracks[0].title=Some("Song".into());
        state.contents.presentations[0].tracks.push(second);
        state.items_checked[0]=vec![true,true];
        state.naming_template="%TITLE%.%EXTENSION%".into();
        assert_eq!(state.native_duplicate_count(),2);
        for (cursor,spelling) in [(0,"Song"),(1,"song")] {
            state.item_cursor=cursor;
            let (lines,_) = rendered_lines(&state,160,44);
            let text=lines.join("\n");
            assert!(text.contains(&format!("→ /tmp/r28-preview/{spelling}.dsf")),"{text}");
            assert!(text.contains("2 tracks would get the same name"),"{text}");
            assert!(state.blockers(Path::new("/tmp")).iter().any(|b|
                b.contains("2 tracks would get the same name")));
            let mut terminal=Terminal::new(TestBackend::new(160,44)).unwrap();
            terminal.draw(|f|draw(f,&state,Path::new("/tmp"),
                &mut ButtonRenderMap::new(),theme)).unwrap();
            assert!(!state.footer_extract_enabled.get());
        }
        // While editing, the path reflects the draft, not a stale blocker.
        state.editing_destination=true;
        state.editing_template=true;
        state.destination_input="%TRACKNUMBER% - %TITLE%.%EXTENSION%".into();
        let (draft,_) = rendered_lines(&state,160,44);
        assert!(draft.join("\n").contains("→ /tmp/r28-preview/02 - song.dsf"));
        state.naming_template=state.destination_input.clone();
        state.editing_destination=false;
        state.editing_template=false;
        assert_eq!(state.native_duplicate_count(),0);
        let (resolved,_) = rendered_lines(&state,160,44);
        assert!(resolved.join("\n").contains("→ /tmp/r28-preview/02 - song.dsf"));
        assert!(!resolved.join("\n").contains("2 tracks would get the same name"));

        state.naming_template="%TITLE%/%MISSING%".into();
        let (unknown,_) = rendered_lines(&state,160,44);
        let text=unknown.join("\n");
        assert!(text.contains("→ /tmp/r28-preview/song/Untitled.dsf"),"{text}");
        assert!(text.contains("2 tracks would get the same name"),"{text}");
        // Resolve the collision independently. The unknown-token warning is
        // then visible while the live (amber) destination preview remains.
        state.items_checked[0][0]=false;
        let (unknown,terminal)=rendered_lines(&state,160,44);
        let text=unknown.join("\n");
        assert!(text.contains("Unknown naming token: %MISSING%"),"{text}");
        assert!(terminal.backend().buffer().content().iter().any(|cell|
            cell.fg==theme.warning && cell.symbol()=="→"),"preview must be amber");
    }

    #[test]
    fn r27_program_mode_detects_duplicate_continuous_output_paths() {
        let mut state = sacd_state(false, false);
        state.destination = "/tmp/r27-album".into();
        state.file_per_program = true;
        let duplicate = state.contents.presentations[0].clone();
        state.contents.presentations.push(duplicate);
        state.selected.push(true);
        state.keep_dst.push(false);
        state.items_checked.push(vec![true]);
        assert_eq!(state.native_output_paths().len(),2);
        assert_eq!(state.native_duplicate_count(),2,
            "the collision gate must use program-level paths in continuous mode");
    }

    #[test]
    fn r22_only_the_active_radio_option_has_focused_style() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        assert!(option_value(&state,"Format").contains("native DSD"));
        state.contents.presentations[0].tracks[0].format_note=Some("DST compressed".into());
        assert!(option_value(&state,"Format").contains("DST decoded"));
        state.keep_dst[0]=true;
        assert!(option_value(&state,"Format").contains("keep selected DST"));
        state.selected[0]=false;
        assert!(option_value(&state,"Format").contains("native DSD"));
        state.selected[0]=true;
        state.keep_dst[0]=false;
        let area=Rect::new(3,3,120,8);
        let mut terminal=Terminal::new(TestBackend::new(130,16)).unwrap();
        state.focus=ExtractFocus::Output;
        state.output_cursor=0;
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|f|draw_output(f,&state,area,&mut buttons,theme,true)).unwrap();
        let active=buttons.button_rect(TuiButton::DiscBrowserFormatChoice(0)).unwrap();
        let inactive=buttons.button_rect(TuiButton::DiscBrowserFormatChoice(1)).unwrap();
        let buf=terminal.backend().buffer();
        assert_eq!(buf.get(active.x,active.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
        assert_ne!(buf.get(inactive.x,inactive.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
        let (x,y)=word_cells(buf,"Format","radio label");
        assert_eq!(buf.get(x,y).bg,extract_colors(theme).focused);
        let (x,y)=word_cells(buf,"Files","other output label");
        assert_eq!(buf.get(x,y).bg,extract_colors(theme).focused);
        state.keep_dst[0]=true;
        // DST retention must not change the independent DSF/DFF radio.
        terminal.draw(|f|draw_output(f,&state,area,&mut ButtonRenderMap::new(),theme,true)).unwrap();
        assert_eq!(terminal.backend().buffer().get(active.x,active.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
        assert_ne!(terminal.backend().buffer().get(inactive.x,inactive.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
        state.sacd_dff=true;
        terminal.draw(|f|draw_output(f,&state,area,&mut ButtonRenderMap::new(),theme,true)).unwrap();
        assert_ne!(terminal.backend().buffer().get(active.x,active.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
        assert_eq!(terminal.backend().buffer().get(inactive.x,inactive.y).bg,radio_focus_bg(extract_colors(theme).focused,theme));
    }

    #[test]
    fn r32_abbreviated_folder_preview_click_maps_to_full_path_component() {
        let home=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from).unwrap_or_else(||"/tmp".into());
        let path=home.join("long-parent-folder-not-displayed")
            .join("another-hidden-directory").join("Extracted");
        let preview=compact_folder_path(&path,28);
        let byte=preview.find("Extracted").expect("last component visible");
        // The click mapper consumes terminal cells, not UTF-8 byte offsets.
        // A compact path contains multibyte ellipsis glyphs before this word.
        let column=crate::tui::display_width::width(&preview[..byte]);
        let caret=native_folder_preview_caret(&path,28,column);
        let tail=path.to_string_lossy().chars().skip(caret).collect::<String>();
        assert!(tail.starts_with("Extracted"),"{preview:?} caret mapped to {tail:?}");
    }

    #[test]
    fn r36_native_text_selection_and_block_caret_are_visible_in_every_builtin_theme() {
        for palette in crate::tui::theme::palettes() {
            let theme=Theme::from_palette(palette);
            let slug=palette.slug;
            let mut terminal=Terminal::new(TestBackend::new(36,8)).unwrap();
            let area=Rect::new(3,2,24,1);
            let well=if theme.dark {blend_color(theme.panel_bg,Color::Black,1,4)}
                else {blend_color(theme.panel_bg,theme.value,1,6)};
            let selection_bg=readable_word(theme.value,well);
            let mut editor=super::super::text_input::TextInputState::new_selected(
                "café_music".to_owned());
            terminal.draw(|f|draw_native_input(f,area,"ignored",Some(&editor),theme)).unwrap();
            let buffer=terminal.backend().buffer();
            let selected=buffer.get(area.x,area.y);
            assert_eq!(selected.bg,selection_bg,"{slug}: selected fill is the value text");
            assert_eq!(selected.fg,well,"{slug}: selected text is inverted");
            assert!(contrast_ratio(selected.bg,well)>=4.5,"{slug}: selection versus well");
            assert!(contrast_ratio(selected.fg,selected.bg)>=4.5,"{slug}: selected word");
            assert!(!selected.modifier.contains(Modifier::UNDERLINED));

            let caret_x=area.x+"café_music".chars().count() as u16;
            let selected_caret=buffer.get(caret_x,area.y);
            assert_eq!(selected_caret.symbol()," ","{slug}: select-all caret is a block");
            assert_eq!(selected_caret.fg,well,"{slug}: selected caret glyph foreground");
            assert!(contrast_ratio(selected_caret.bg,well)>=4.5,"{slug}: selected caret versus well");
            assert!(contrast_ratio(selected_caret.bg,selected.bg)>=1.4,
                "{slug}: selected caret must differ from adjacent highlight");
            let selected_caret_bg=selected_caret.bg;

            // Reverse drag/Shift+Left puts the caret on the first selected
            // character rather than after the selection.
            editor.select_all=false;
            editor.selection_anchor=Some(editor.text.len());
            editor.cursor=0;
            terminal.draw(|f|draw_native_input(f,area,"ignored",Some(&editor),theme)).unwrap();
            let buffer=terminal.backend().buffer();
            assert_eq!(buffer.get(area.x,area.y).bg,selected_caret_bg,"{slug}: reverse caret");
            assert_eq!(buffer.get(area.x+1,area.y).bg,selection_bg,"{slug}: reverse selection");

            editor.clear_selection();
            editor.cursor=editor.text.len();
            terminal.draw(|f|draw_native_input(f,area,"ignored",Some(&editor),theme)).unwrap();
            let buffer=terminal.backend().buffer();
            let caret=buffer.get(caret_x,area.y);
            assert_eq!(caret.symbol()," ","{slug}: end of text uses the block's empty cell");
            assert_eq!(caret.bg,selection_bg,"{slug}: ordinary block caret fill");
            terminal.draw(|f|draw_native_input(f,area,"café_music",None,theme)).unwrap();
            let buffer=terminal.backend().buffer();
            assert_eq!(buffer.get(area.x,area.y).bg,well,"{slug}: recessed well");
            assert_eq!(buffer.get(caret_x,area.y).bg,well,"{slug}: no caret without edit");
            let text=(area.x..area.right()).map(|x|buffer.get(x,area.y).symbol().to_owned())
                .collect::<String>();
            assert!(!text.contains('▕') && !text.contains('▏'),"{slug}: no edge marks");
        }
    }

    #[test]
    fn r36_folder_and_naming_render_select_all_using_the_shared_input_widget() {
        for slug in ["tokyo-night","tokyo-night-day"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            let well=if theme.dark {blend_color(theme.panel_bg,Color::Black,1,4)}
                else {blend_color(theme.panel_bg,theme.value,1,6)};
            let selected=readable_word(theme.value,well);
            for naming in [false,true] {
                let mut state=sacd_state(false,false);
                state.focus=ExtractFocus::Output;
                state.editing_destination=true;
                state.editing_template=naming;
                state.text_edit=Some(super::super::text_input::TextInputState::new_selected(
                    "AB".into()));
                let mut terminal=Terminal::new(TestBackend::new(160,36)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|f|draw_output(f,&state,Rect::new(3,3,144,16),
                    &mut buttons,theme,true)).unwrap();
                let field=buttons.button_rect(if naming {TuiButton::DiscBrowserNaming}
                    else {TuiButton::DiscBrowserFolderField}).expect("editable field hitbox");
                let buffer=terminal.backend().buffer();
                for x in field.x..field.x+2 {
                    assert_eq!(buffer.get(x,field.y).bg,selected,
                        "{slug}: {} selection",if naming {"Naming"}else{"Folder"});
                    assert_eq!(buffer.get(x,field.y).fg,well);
                }
                let caret=buffer.get(field.x+2,field.y);
                assert!(contrast_ratio(caret.bg,selected)>=1.4,
                    "{slug}: {} cursor beside highlighted text",if naming {"Naming"}else{"Folder"});
            }
        }
    }

    #[test]
    fn r32_radio_focus_has_theme_surface_not_underlined_selection() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.focus=ExtractFocus::Output;
        state.output_cursor=0;
        let mut terminal=Terminal::new(TestBackend::new(140,22)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|f|draw_output(f,&state,Rect::new(2,2,130,10),
            &mut buttons,theme,true)).unwrap();
        let selected=buttons.button_rect(TuiButton::DiscBrowserFormatChoice(0)).unwrap();
        let unselected=buttons.button_rect(TuiButton::DiscBrowserFormatChoice(1)).unwrap();
        let buf=terminal.backend().buffer();
        assert_ne!(buf.get(selected.x,selected.y).bg,buf.get(unselected.x,unselected.y).bg);
        assert_eq!(buf.get(selected.x,selected.y).fg,
            readable_word(theme.info,radio_focus_bg(extract_colors(theme).focused,theme)));
        for rect in [selected,unselected] {
            for x in rect.x..rect.right() {
                assert!(!buf.get(x,rect.y).modifier.contains(Modifier::UNDERLINED));
            }
        }
    }

    #[test]
    fn r36_roomy_default_and_full_terminal_maximize_have_title_hitboxes() {
        let mut state=sacd_state(false,false);
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut terminal=Terminal::new(TestBackend::new(240,50)).unwrap();
        let mut normal=None;
        terminal.draw(|f|{
            let mut buttons=ButtonRenderMap::new();
            draw(f,&state,Path::new("/tmp"),&mut buttons,theme);
            normal=buttons.button_rect(TuiButton::DiscBrowserTitle);
        }).unwrap();
        let normal=normal.unwrap();
        assert!(normal.width>=216 && normal.width<=236 && normal.x>=2 && normal.y>=1,
            "roomy default must be visibly larger while retaining margins: {normal:?}");
        let lines=(0..50).map(|y|(0..240)
            .map(|x|terminal.backend().buffer().get(x,y).symbol().to_owned())
            .collect::<Vec<_>>().concat()).collect::<Vec<_>>().join("\n");
        assert!(lines.contains("▸ extract tracks"));
        state.maximized=true;
        let mut maximum=None;
        terminal.draw(|f|{
            let mut buttons=ButtonRenderMap::new();
            draw(f,&state,Path::new("/tmp"),&mut buttons,theme);
            maximum=buttons.button_rect(TuiButton::DiscBrowserTitle);
        }).unwrap();
        assert_eq!(maximum.unwrap(),Rect::new(0,0,240,1));
        let buf=terminal.backend().buffer();
        assert_eq!(buf.get(1,0).symbol(),"▾");
        assert_eq!(buf.get(0,49).symbol(),"└");
        assert_eq!(buf.get(239,49).symbol(),"┘");
    }

    #[test]
    fn r15_folder_path_preserves_disc_identity_and_naming_separately() {
        let home=std::env::var_os("HOME").map(std::path::PathBuf::from)
            .unwrap_or_else(||std::path::PathBuf::from("/home/test"));
        let disc=home.join("temp/external/archive/audio-isos/sacd")
            .join("1973 - Genesis - Selling England By The Pound (2024, SACD, Analogue Productions)")
            .join("Extracted");
        let folder=compact_folder_path(&disc,86);
        assert!(folder.chars().count()<=86 && folder.contains("Selling England By The Pound"),
            "{folder}");
        assert!(folder.ends_with("/Extracted"),"{folder}");
        if std::env::var_os("HOME").is_some(){assert!(folder.starts_with("~/…/"),"{folder}");}
        let mut state=sacd_state(false,false);
        state.destination=disc;
        let (lines,_)=rendered_lines(&state,154,40);
        let text=lines.join("\n");
        assert!(text.contains("Folder") && text.contains("Naming"),"{text}");
        assert!(text.contains("%TRACKNUMBER% - %TITLE%"),"{text}");
        assert!(!text.contains(" / %TRACKNUMBER%"),"template must not resemble a path component");
        assert_eq!(format_bytes(479*1024*1024),"479 MiB");
        assert_eq!(format_bytes(2*1024*1024*1024),"2.0 GiB");
        let mut state=sacd_state(false,false);
        state.contents.presentations[0].tracks[0].title=Some("Dancing with the Moonlit Knight".into());
        state.contents.presentations[0].tracks[0].duration_secs=Some(484.0);
        state.focus=ExtractFocus::Items;
        let (lines,_)=rendered_lines(&state,150,40);
        assert!(lines.iter().any(|row|row.contains("01 8:04 Dancing")),"{lines:?}");
    }

    #[test]
    fn r23_dst_format_consequence_and_tag_availability_are_visible_in_buffer() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.contents.presentations[0].format.codec=Some("DST".into());
        state.contents.presentations[0].tracks[0].format_note=Some("DST compressed".into());
        state.keep_dst[0]=true;
        state.focus=ExtractFocus::Output;
        let area=Rect::new(2,3,146,7);
        let mut terminal=Terminal::new(TestBackend::new(152,14)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|frame|draw_output(frame,&state,area,&mut buttons,theme,true)).unwrap();
        {
            let buffer=terminal.backend().buffer();
            let line=|y| (area.x..area.x+area.width).map(|x|buffer.get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            let note=line(area.y+1);
            assert!(note.contains("DFF has no standard tags"),"{note}");
            let byte=note.find("DFF has").unwrap();
            let x=area.x+note[..byte].chars().count() as u16;
            assert_eq!(buffer.get(x,area.y+1).fg,theme.warning);
            let tags=line(area.y+2);
            assert!(tags.contains("off for DFF"),"{tags}");
        }
        let tags_index=state.output_rows().iter().position(|name|*name=="Tags").unwrap();
        assert!(buttons.button_rect(TuiButton::DiscBrowserToggle(tags_index)).is_none(),
            "the disabled DFF-only Transfer toggle must not be clickable");
        assert!(buttons.button_rect(TuiButton::DiscBrowserTagsEdit).is_some(),
            "the Tags sheet must remain accessible to offer Use DSF");

        // A second selected area producing DSF still accepts tags, and the
        // helper text makes the distinction explicit without a second sheet.
        state.contents.presentations.push(state.contents.presentations[0].clone());
        state.selected.push(true);
        state.items_checked.push(vec![true]);
        state.keep_dst.push(false);
        let mut mixed_buttons=ButtonRenderMap::new();
        terminal.draw(|frame|draw_output(frame,&state,area,&mut mixed_buttons,theme,true)).unwrap();
        let buffer=terminal.backend().buffer();
        let mixed=(area.x..area.x+area.width)
            .map(|x|buffer.get(x,area.y+1).symbol().to_owned()).collect::<Vec<_>>().concat();
        assert!(mixed.contains("DSF keeps tags; DFF has no standard tags"),"{mixed}");
        assert!(mixed_buttons.button_rect(TuiButton::DiscBrowserToggle(tags_index)).is_some());
    }

    #[test]
    fn r34_audit_d02_tags_summary_uses_text_dim_across_light_and_dark_themes() {
        for slug in ["tokyo-night", "tokyo-night-day"] {
            let theme = crate::tui::theme::theme_by_slug_or_default(slug);
            let mut state = sacd_state(false, false);
            state.focus = ExtractFocus::Output;
            state.tags = true;
            state.tag_fields = 0x7f; // Seven selected fields.
            state.tag_artwork = true;
            let area = Rect::new(2, 3, 146, 9);
            let mut terminal = Terminal::new(TestBackend::new(152, 14)).unwrap();
            let mut buttons = ButtonRenderMap::new();
            let find_summary = |buffer: &ratatui::buffer::Buffer, text: &str| {
                (area.y..area.bottom()).find_map(|y| {
                    let line = (area.x..area.right()).map(|x| buffer.get(x, y).symbol().to_owned())
                        .collect::<Vec<_>>().concat();
                    line.find(text).map(|at| (area.x + line[..at].chars().count() as u16, y))
                }).unwrap_or_else(|| panic!("{slug}: missing Tags summary {text:?}"))
            };
            terminal.draw(|frame| draw_output(frame, &state, area, &mut buttons, theme, true)).unwrap();
            let summary = "7 fields · artwork";
            let buffer = terminal.backend().buffer();
            let (x, y) = find_summary(buffer, summary);
            let bg = extract_colors(theme).focused;
            let fg = readable_word(theme.text_dim, bg);
            for offset in 0..summary.chars().count() as u16 {
                let cell = buffer.get(x + offset, y);
                assert_eq!((cell.fg, cell.bg), (fg, bg), "{slug} summary cell {offset}");
                assert!(contrast_ratio(cell.fg, cell.bg) >= 4.5);
            }
            assert!(buttons.button_rect(TuiButton::DiscBrowserTagsEdit).is_some());

            // A second DST area is DFF even when the radio remains DSF;
            // this mixed-format warning retains its established warning color.
            let mut multi = state.contents.presentations[0].clone();
            multi.id = PresentationId::SacdArea(SacdAreaId::MultiChannel);
            multi.format.codec = Some("DST".into());
            state.contents.presentations.push(multi);
            state.selected.push(true);
            state.items_checked.push(vec![true]);
            state.keep_dst.push(true);
            terminal.draw(|frame| draw_output(frame, &state, area, &mut buttons, theme, true)).unwrap();
            let warning = "DSF only; DFF none";
            let buffer = terminal.backend().buffer();
            let (x, y) = find_summary(buffer, warning);
            for offset in 0..warning.chars().count() as u16 {
                let cell = buffer.get(x + offset, y);
                assert_eq!(cell.fg, theme.warning, "{slug} warning cell {offset}");
            }
        }
    }

    #[test]
    fn r23_folder_feedback_and_footer_keyboard_focus_render_in_actual_cells() {
        use super::super::disc_browser::DestinationFeedback;
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let temp=tempfile::tempdir().unwrap();
        let mut state=sacd_state(false,false);
        state.focus=ExtractFocus::Output;
        state.destination=temp.path().join("NewAlbum");
        state.refresh_destination_feedback();
        assert_eq!(state.destination_feedback,Some(DestinationFeedback::WillBeCreated));
        let area=Rect::new(2,3,146,7);
        let mut terminal=Terminal::new(TestBackend::new(152,14)).unwrap();
        terminal.draw(|frame|draw_output(frame,&state,area,&mut ButtonRenderMap::new(),theme,true)).unwrap();
        {
            let buffer=terminal.backend().buffer();
            let folder_note=(area.x..area.x+area.width)
                .map(|x|buffer.get(x,area.y+4).symbol().to_owned()).collect::<Vec<_>>().concat();
            assert!(folder_note.contains("This folder will be created; "),"{folder_note}");
            assert!(!folder_note.contains(" · will be created"),"{folder_note}");
            if state.native_output_layout_hint()=="Files go directly into this folder." {
                assert!(folder_note.contains("This folder will be created; files go directly into it."),
                    "{folder_note}");
            }
        }

        let file=temp.path().join("not_a_directory");
        std::fs::write(&file,"ordinary file").unwrap();
        state.destination=file;
        state.refresh_destination_feedback();
        assert_eq!(state.destination_feedback,Some(DestinationFeedback::NotWritable));
        terminal.draw(|frame|draw_output(frame,&state,area,&mut ButtonRenderMap::new(),theme,true)).unwrap();
        {
            let buffer=terminal.backend().buffer();
            let folder_note=(area.x..area.x+area.width)
                .map(|x|buffer.get(x,area.y+4).symbol().to_owned()).collect::<Vec<_>>().concat();
            assert!(folder_note.contains("not writable"),"{folder_note}");
        }

        state.focus=ExtractFocus::Footer;
        state.footer_cursor=2;
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|frame|draw_footer(frame,&state,Rect::new(2,2,146,2),
            &mut buttons,false,true,theme)).unwrap();
        let more=buttons.button_rect(TuiButton::DiscBrowserOptions).unwrap();
        assert_eq!(terminal.backend().buffer().get(more.x+2,more.y).bg,theme.pill_active_bg);
    }
    #[test]
    fn extract_options_popup_is_keyboard_visible_and_mouse_shields_underlying_form() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(64u16,20u16),(72,24),(110,34),(154,40)] {
            let mut state=sacd_state(false,false);
            for panel in [ExtractPanel::Options,ExtractPanel::Chapters,
                ExtractPanel::CueFiles,ExtractPanel::CopyFolder] {
                state.panel=Some(panel);
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|f|draw(f,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
                for index in 0..state.option_controls().len() {
                    let target=TuiButton::DiscBrowserOptionControl(index);
                    let rect=buttons.button_rect(target)
                        .expect("Option must have a visible mouse target");
                    assert!(rect.width>0 && rect.height>0 && rect.right()<=width && rect.bottom()<=height,
                        "invisible option at {width}x{height} in {panel:?}");
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(target));
                }
                let passive=buttons.button_rect(TuiButton::DiscBrowserOptionPassive).unwrap();
                assert_eq!(buttons.find_button_at(passive.x+1,passive.y+1),
                    Some(TuiButton::DiscBrowserOptionPassive));
                let outside=buttons.button_rect(TuiButton::DiscBrowserOptionOutside).unwrap();
                assert!(passive.x>=outside.x && passive.y>=outside.y);
                assert_eq!(buttons.find_button_at(0,0),Some(TuiButton::DiscBrowserOptionOutside));
            }
        }
    }


    #[test]
    fn cd03_options_render_live_summaries_in_actual_terminal_cells() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(64u16,20u16),(72,24),(154,40)] {
            let mut state=sacd_state(false,false);
            state.panel=Some(ExtractPanel::Options);
            let render=|state:&DiscBrowserState| -> String {
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|frame|draw(frame,state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
                for index in 0..state.option_controls().len() {
                    let control=TuiButton::DiscBrowserOptionControl(index);
                    let rect=buttons.button_rect(control).expect("every Options control has a mouse target");
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(control));
                }
                (0..height).map(|y| (0..width).map(|x|
                    terminal.backend().buffer().get(x,y).symbol().to_owned())
                    .collect::<Vec<_>>().concat()).collect::<Vec<_>>().join("\n")
            };
            let initial=render(&state);
            assert!(initial.contains("chapters.txt on") || initial.contains("chapters on"),
                "{width}x{height}: {initial}");
            assert!(initial.contains("7/10 types") || initial.contains("7 of 10 types"),
                "{width}x{height}: {initial}");
            assert!(initial.contains("create on") && initial.contains("pre on") &&
                initial.contains("append"),"{width}x{height}: {initial}");
            state.extract_settings.keep_preemphasis=false;
            state.extract_settings.write_chapters=false;
            state.extract_settings.copy_folder.artwork=false;
            let changed=render(&state);
            assert!(changed.contains("pre off") &&
                (changed.contains("chapters.txt off") || changed.contains("chapters off")),
                "{width}x{height}: {changed}");
            assert!(changed.contains("6/10 types") || changed.contains("6 of 10 types"),
                "{width}x{height}: {changed}");
            assert!(changed.contains("append"),"gap choice disappeared: {changed}");
        }
    }

    #[test]
    fn r2d07_cue_summary_reflects_preemphasis_without_hiding_gap_choices() {
        let mut state=sacd_state(false,false);
        let original=extract_option_display(&state,ExtractOptionControl::CueFiles).1;
        assert!(original.contains("pre on") && original.contains("append") && original.contains("00"));
        state.extract_settings.keep_preemphasis=false;
        let changed=extract_option_display(&state,ExtractOptionControl::CueFiles).1;
        assert!(changed.contains("pre off") && changed.contains("append") && changed.contains("00"));
        state.extract_settings.keep_preemphasis=true;
        assert_eq!(extract_option_display(&state,ExtractOptionControl::CueFiles).1,original);
    }


    /// Locate exact visible cells, not byte indices in a UTF-8 screen dump.
    fn r4f_text_position(buffer:&ratatui::buffer::Buffer,width:u16,height:u16,
        text:&str) -> (u16,u16) {
        for y in 0..height {
            let row=(0..width).map(|x|buffer.get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            if let Some(byte)=row.find(text) {
                return (row[..byte].chars().count() as u16,y);
            }
        }
        panic!("no visible text {text:?}");
    }

    #[test]
    fn r4_options_mockup_has_sections_descriptions_buttons_and_separate_footer() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        state.panel=Some(ExtractPanel::Options);
        let mut terminal=Terminal::new(TestBackend::new(154,40)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
        let lines=(0..40).map(|y|(0..154).map(|x|
            terminal.backend().buffer().get(x,y).symbol().to_owned())
            .collect::<Vec<_>>().concat()).collect::<Vec<_>>();
        let row=|needle:&str|lines.iter().position(|line|line.contains(needle))
            .unwrap_or_else(||panic!("missing {needle:?}: {lines:#?}"));
        assert!(row("▸ Options")<row("OUTPUT"));
        assert!(row("OUTPUT")<row("Write extraction.log"));
        assert_eq!(row("Verify after extraction"),row("reads each file back and checks its audio"));
        assert_eq!(row("Write extraction.log"),row("in destination folder, beside the tracks"));
        assert_eq!(row("Chapters…"),row("chapters.txt on"));
        assert_eq!(row("CUE files…"),row("create on"));
        assert!(lines.iter().any(|line|line.contains("embed off") && line.contains("pre on")));
        assert!(lines.iter().any(|line|line.contains("7 of 10 types")));
        assert!(row("Copy folder contents…")<row("DEFAULTS"));
        assert!(row("DEFAULTS")<row("Use these settings next time"));
        assert_eq!(row("Use these settings next time"),row("saved when you press Extract"));
        assert!(row("? all keys")<40 && row("Done")<40);
        assert!(!lines.iter().any(|line|line.contains("↑↓ / Tab focus")));
        let chapter=buttons.button_rect(TuiButton::DiscBrowserOptionControl(2)).unwrap();
        assert_eq!(terminal.backend().buffer().get(chapter.x,chapter.y).bg,theme.pill_neutral_bg);
        let done=buttons.button_rect(TuiButton::DiscBrowserOptionControl(6)).unwrap();
        assert_eq!(terminal.backend().buffer().get(done.x,done.y).bg,theme.chip_go);
        assert_eq!(buttons.find_button_at(done.x,done.y),Some(TuiButton::DiscBrowserOptionControl(6)));
        let log=buttons.button_rect(TuiButton::DiscBrowserOptionControl(0)).unwrap();
        assert_eq!(terminal.backend().buffer().get(log.x,log.y).bg,theme.pill_active_bg);
        assert_eq!(terminal.backend().buffer().get(log.right()+3,log.y).bg,theme.dropdown_bg,
            "form focus must not highlight the whole row");
        let buffer=terminal.backend().buffer();
        let value_x=r4f_text_position(buffer,154,40,"in destination folder, beside the tracks").0;
        for description in ["reads each file back and checks its audio",
            "against a checksum taken while extracting", "chapters.txt on",
            "create on", "7 of 10 types", "saved when you press Extract"] {
            assert_eq!(r4f_text_position(buffer,154,40,description).0,value_x,
                "description column drifted: {description}");
        }
        let modal=buttons.button_rect(TuiButton::DiscBrowserOptionPassive).unwrap();
        // Blank rows between OUTPUT, the three popup controls, and DEFAULTS.
        for y in [row("against a checksum taken while extracting")+1,
            row("Chapters…")+1,row("CUE files…")+1,row("Copy folder contents…")+1] {
            assert!((modal.x+2..modal.right()-2).all(|x|
                buffer.get(x,y as u16).symbol()==" "),"group spacer missing at y={y}");
        }
        assert_eq!(done.right(),modal.right()-3,"Done must hug the footer's right edge");
        assert_eq!(done.y,modal.bottom()-2,"Done must sit on the footer row");
        let help=crate::tui::disc_browser_actions::native_key_hints(&state)
            .iter().position(|hint|*hint=="? all keys").unwrap();
        let hint=buttons.button_rect(TuiButton::DiscBrowserHint(help)).unwrap();
        assert_eq!(hint.y,done.y);
        assert_eq!(buttons.find_button_at(hint.x+2,hint.y),
            Some(TuiButton::DiscBrowserHint(help)));
    }

    #[test]
    fn r4_chapters_cue_and_copy_popups_draw_mockup_rows_and_controls() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=sacd_state(false,false);
        for panel in [ExtractPanel::Chapters,ExtractPanel::CueFiles,ExtractPanel::CopyFolder] {
            state.panel=Some(panel);
            state.panel_cursor=0;
            let mut terminal=Terminal::new(TestBackend::new(154,40)).unwrap();
            let mut buttons=ButtonRenderMap::new();
            terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
            let lines=(0..40).map(|y|(0..154).map(|x|
                terminal.backend().buffer().get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().concat()).collect::<Vec<_>>();
            let row=|needle:&str|lines.iter().position(|line|line.contains(needle))
                .unwrap_or_else(||panic!("{panel:?} missing {needle:?}: {lines:#?}"));
            match panel {
                ExtractPanel::Chapters => {
                    assert!(row("▸ Chapters")<row("Write chapters.txt"));
                    assert!(lines.iter().any(|line|line.contains("Used when chapters from an ISO or container can't be kept in the output.")));
                    assert_eq!(row("Write chapters.txt"),row("beside the output file"));
                    assert!(row("marks where each chapter starts")<row("mkvmerge and MP4Box read it."));
                },
                ExtractPanel::CueFiles => {
                    assert!(row("NEW CUE FILE")<row("Create CUE file"));
                    assert!(row("Create CUE file")<row("Embed CUE in the file"));
                    assert!(row("Embed CUE in the file")<row("Keep pre-emphasis flags"));
                    assert!(row("Keep pre-emphasis flags")<row("SPLITTING A CUE IMAGE"));
                    assert!(lines.iter().any(|line|line.contains("For SACD ISOs and DFF files")));
                    assert!(lines.iter().any(|line|line.contains("For FLAC and WavPack saved as one file.")));
                    assert!(lines.iter().any(|line|line.contains("Gaps are the audio between a track's INDEX 00 and INDEX 01.")));
                    let between=row("Between tracks");
                    assert!(lines[between].contains("Append to previous") &&
                        lines[between].contains("Prepend to next") && lines[between].contains("Drop"));
                    assert!(lines[between+1].contains("Albums play gaplessly."));
                    let first=row("Before track 1");
                    assert!(lines[first].contains("Prepend to track 1") &&
                        lines[first].contains("Own track 00") && lines[first].contains("Drop"));
                    assert!(lines[first+1].contains("A hidden track before track 1 becomes track 00."));
                    let group=|first:usize| (first..first+3).map(|i|
                        buttons.button_rect(TuiButton::DiscBrowserOptionControl(i)).unwrap())
                        .collect::<Vec<_>>();
                    let between=group(3);
                    let initial=group(6);
                    for radios in [&between,&initial] {
                        assert!(radios.iter().all(|r|r.y==radios[0].y));
                        assert!(radios.windows(2).all(|pair|
                            pair[0].right()<pair[1].x),"radios must be side by side with gaps");
                    }
                    assert_eq!(between[0].x,initial[0].x,
                        "radio groups must share a choice column");
                },
                ExtractPanel::CopyFolder => {
                    assert!(row("▸ Copy folder contents")<row("☑ Artwork"));
                    let first=row("☑ Artwork");
                    assert!(lines[first].contains("☑ Art folders") &&
                        lines[first].contains("☑ .log") && lines[first].contains("☐ .cue"));
                    let second=row("☑ .txt");
                    assert_eq!(second,first+1);
                    assert!(lines[second].contains("☑ .nfo") && lines[second].contains("☑ .md") &&
                        lines[second].contains("☑ .pdf"));
                    assert!(lines[second+1].contains("☐ .md5") &&
                        lines[second+1].contains("☐ Everything else"));
                    assert!(row("Artwork: image files beside the CUE image.")>second+1);
                    assert!(lines.iter().any(|line|line.contains("Art folders: Artwork, Art, Scans, Covers and Booklet, in any case.")));
                    assert!(lines.iter().any(|line|line.contains(".cue and .md5 describe the source image, not the new tracks.")));
                    let grid=(0..10).map(|index|
                        buttons.button_rect(TuiButton::DiscBrowserOptionControl(index)).unwrap())
                        .collect::<Vec<_>>();
                    assert!(grid[..4].windows(2).all(|r|r[0].x<r[1].x));
                    let spacing=grid[1].x-grid[0].x;
                    assert!(spacing>0);
                    assert_eq!(grid[2].x-grid[1].x,spacing);
                    assert_eq!(grid[3].x-grid[2].x,spacing);
                    assert!(grid[..4].iter().all(|r|r.y==grid[0].y));
                    for i in 0..4 {
                        assert_eq!(grid[i+4].x,grid[i].x,"second grid row lost its columns");
                        assert_eq!(grid[i+4].y,grid[i].y+1);
                    }
                    for i in 0..2 {
                        assert_eq!(grid[i+8].x,grid[i].x,"third grid row lost its columns");
                        assert_eq!(grid[i+8].y,grid[i].y+2);
                    }
                },
                _=>unreachable!(),
            }
            let done_index=state.option_controls().len()-1;
            let done=buttons.button_rect(TuiButton::DiscBrowserOptionControl(done_index)).unwrap();
            assert_eq!(terminal.backend().buffer().get(done.x,done.y).bg,theme.chip_go);
            assert!(lines[done.y as usize].contains("? all keys"));
            for index in 0..state.option_controls().len() {
                let target=TuiButton::DiscBrowserOptionControl(index);
                let rect=buttons.button_rect(target).expect("all options have mouse targets");
                assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(target));
            }
        }
    }

    #[test]
    fn r4_mockup_controls_remain_reachable_in_compact_terminals() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(64,20),(72,24)] {
            let mut state=sacd_state(false,false);
            for panel in [ExtractPanel::Options,ExtractPanel::Chapters,
                ExtractPanel::CueFiles,ExtractPanel::CopyFolder] {
                state.panel=Some(panel);
                let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                let mut buttons=ButtonRenderMap::new();
                terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
                for (index,_) in state.option_controls().iter().enumerate() {
                    let target=TuiButton::DiscBrowserOptionControl(index);
                    let rect=buttons.button_rect(target).expect("compact control has visible hit box");
                    assert!(rect.width>0 && rect.bottom()<=height && rect.right()<=width);
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(target));
                    if panel==ExtractPanel::CueFiles && (3..9).contains(&index) {
                        assert!(matches!(terminal.backend().buffer().get(rect.x,rect.y).symbol(),"●"|"○"),
                            "compact radio must remain visible in front of the footer rule");
                    }
                }
            }
        }
    }

    #[test]
    fn r4f_option_summary_value_and_qualifier_use_distinct_theme_tiers() {
        for slug in ["tokyo-night","one-light"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            let mut state=sacd_state(false,false);
            state.panel=Some(ExtractPanel::Options);
            let mut terminal=Terminal::new(TestBackend::new(154,40)).unwrap();
            let mut buttons=ButtonRenderMap::new();
            terminal.draw(|frame|draw(frame,&state,Path::new("/tmp"),&mut buttons,theme)).unwrap();
            let buffer=terminal.backend().buffer();
            for (value,qualifier) in [
                ("chapters.txt on","· for single-file output"),
                ("create on","· for new CUEs and images"),
                ("7 of 10 types","· for CUE images"),
            ] {
                let (value_x,value_y)=r4f_text_position(buffer,154,40,value);
                let (qualifier_x,qualifier_y)=r4f_text_position(buffer,154,40,qualifier);
                assert_eq!(value_y,qualifier_y,"{slug} summary unexpectedly wrapped");
                assert!(qualifier_x>value_x);
                let value_color=buffer.get(value_x,value_y).fg;
                let qualifier_color=buffer.get(qualifier_x,qualifier_y).fg;
                assert_eq!(value_color,readable_word(theme.value,theme.dropdown_bg),
                    "{slug} value tier for {value}");
                assert_eq!(qualifier_color,readable_word(theme.label,theme.dropdown_bg),
                    "{slug} qualifier tier for {qualifier}");
                assert_ne!(value_color,qualifier_color,
                    "{slug} summary must distinguish value from qualifier");
            }
        }
    }
}

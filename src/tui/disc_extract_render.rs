//! Responsive native extraction overlay (#88).
//! All viewports derive from the current terminal dimensions. The same logical
//! indices register the mouse targets and drive keyboard selection.

use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::convert::pipeline::native_extract::NativeWavContainer;
use crate::disc::{DiscFormat, DiscPresentation, PresentationId};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::disc_browser::{DiscBrowserState, ExtractFocus, ExtractPanel, duration_display, sample_rate_display};
use super::theme::Theme;

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
    if bytes >= 1024u64.pow(3) { format!("{:.1} GiB",bytes as f64/1024f64.powi(3)) }
    else if bytes >= 1024u64.pow(2) { format!("{:.1} MiB",bytes as f64/1024f64.powi(2)) }
    else if bytes >= 1024 { format!("{:.1} KiB", bytes as f64 / 1024.0) }
    else { format!("{bytes} B") }
}

fn check(checked: bool) -> &'static str { if checked { "[✓]" } else { "[ ]" } }
fn program_check(s: &DiscBrowserState, i: usize) -> &'static str {
    if !s.is_extractable(i) { "×" } else { check(s.selected[i]) }
}

fn header_for(id: PresentationId, p: &DiscPresentation, index: usize) -> String {
    match id {
        PresentationId::SacdArea(_) => p.id.compact_label(),
        PresentationId::DvdAudioGroup(n) => format!("Group {n}"),
        PresentationId::DvdVideoTitle { vts_number, title_number, .. } => format!("VTS {vts_number} · Title {title_number}"),
        PresentationId::BluRayTitle { playlist_number, display_angle, .. } =>
            format!("Playlist {playlist_number:05} · angle {}{}", display_angle.get(), if index == 0 { " · main title" } else { "" }),
    }
}

fn program_info(p: &DiscPresentation, format: DiscFormat) -> String {
    let codec = p.format.codec.as_deref().unwrap_or("unknown");
    let loss = if p.format.lossless { "lossless" } else { "lossy" };
    if format == DiscFormat::Sacd {
        let dst = p.tracks.iter().any(|t| t.format_note.as_deref().unwrap_or("").contains("DST"));
        return format!("{} · lossless · DSD64 · {}", if dst { "DST" } else { "DSD" }, p.format.channel_layout.as_deref().unwrap_or("unknown"));
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

fn rule(frame: &mut ratatui::Frame, area: Rect, theme: Theme) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(Paragraph::new("─".repeat(area.width as usize))
            .style(Style::default().fg(theme.title).bg(theme.panel_bg)), area);
    }
}

/// The single frame and its dividers belong to the dialog, not to individual
/// panes. Every region is derived from the current viewport on each draw.
pub fn draw(
    frame: &mut ratatui::Frame,
    state: &DiscBrowserState,
    scratch: &Path,
    buttons: &mut ButtonRenderMap,
    theme: Theme,
) {
    let area = frame.size();
    if area.width < 64 || area.height < 20 {
        frame.render_widget(Paragraph::new("Make the terminal at least 64×20 to extract.")
            .alignment(Alignment::Center).style(Style::default().fg(theme.label)),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1));
        return;
    }
    let wide = area.width >= 140 && area.height >= 36;
    let small = area.width < 96 || area.height < 30;
    let popup = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(Block::default().style(Style::default().bg(theme.panel_bg))
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(theme.title)), popup);

    let source_name = state.source_path.file_name()
        .map(|v| v.to_string_lossy().into_owned()).unwrap_or_else(|| state.contents.label.clone());
    let action = match state.contents.format {
        DiscFormat::Sacd | DiscFormat::DvdAudio => "extract tracks",
        DiscFormat::DvdVideo | DiscFormat::BluRay => "extract chapters",
    };
    let source_type = format!("{} {}", state.contents.format.name(),
        if state.source_path.is_dir() { "folder" } else { "ISO" });
    let tag_width = (source_type.chars().count() as u16 + 1).min(popup.width / 2);
    let title_style = Style::default().fg(theme.panel_bg).bg(theme.title).add_modifier(Modifier::BOLD);
    frame.render_widget(Block::default().style(title_style), Rect::new(popup.x,popup.y,popup.width,1));
    let left = Rect::new(popup.x+1,popup.y,popup.width.saturating_sub(tag_width+2),1);
    frame.render_widget(Paragraph::new(clip(&format!("▸ {action} · {source_name}"),left.width))
        .style(title_style),left);
    let right = Rect::new(popup.x+popup.width-tag_width-1,popup.y,tag_width,1);
    frame.render_widget(Paragraph::new(source_type).alignment(Alignment::Right)
        .style(title_style),right);

    let inner = Rect::new(popup.x+1,popup.y+1,popup.width-2,popup.height-2);
    let header_height = if small { 2 } else { 3 };
    let output_count = state.output_rows().len() as u16;
    let output_height = if small { 1 } else if wide { output_count.div_ceil(2).max(2) }
        else { output_count.max(1) };
    let results_height = if small { 1 } else { 2 };
    let footer_height = if small { 1 } else { 2 };
    let problems = state.blockers(scratch);
    let warning_height = u16::from(!problems.is_empty());
    let chunks = Layout::default().direction(Direction::Vertical).constraints([
        Constraint::Length(header_height), Constraint::Length(1),
        Constraint::Min(1), Constraint::Length(1),
        Constraint::Length(output_height), Constraint::Length(u16::from(!small)),
        Constraint::Length(warning_height), Constraint::Length(results_height),
        Constraint::Length(1), Constraint::Length(footer_height),
    ]).split(inner);
    draw_header(frame,state,chunks[0],small,theme);
    rule(frame,chunks[1],theme);
    if small {
        match state.focus {
            ExtractFocus::Programs => draw_programs(frame,state,chunks[2],buttons,theme),
            ExtractFocus::Items => draw_items(frame,state,chunks[2],buttons,theme,false),
            ExtractFocus::Output => draw_output(frame,state,chunks[2],buttons,theme,false),
        }
    } else {
        // The largest authored program label plus checkbox, duration and
        // count determines the preferred width, constrained by the mockup's
        // proportional bounds. No fixed 80-column assumption.
        let longest = state.contents.presentations.iter().enumerate().map(|(i,p)| {
            header_for(p.id,p,i).chars().count() + 7
                + duration_display(p.total_duration_secs).chars().count()
                + p.tracks.len().to_string().len() + 11
        }).max().unwrap_or(0) as u16;
        let pane_width = chunks[2].width;
        let min_width = (pane_width as u32 * 34 / 100) as u16;
        let max_width = pane_width / 2;
        let program_width = longest.clamp(min_width.min(max_width),max_width)
            .min(pane_width.saturating_sub(2));
        let panes = Layout::default().direction(Direction::Horizontal).constraints([
            Constraint::Length(program_width),Constraint::Length(1),Constraint::Min(1)
        ]).split(chunks[2]);
        draw_programs(frame,state,panes[0],buttons,theme);
        if panes[1].height > 0 {
            for y in panes[1].y..panes[1].y+panes[1].height {
                frame.render_widget(Paragraph::new("│").style(Style::default().fg(theme.title)),
                    Rect::new(panes[1].x,y,1,1));
            }
        }
        draw_items(frame,state,panes[2],buttons,theme,wide);
    }
    rule(frame,chunks[3],theme);
    if small {
        // The three tabs use the existing pane focus; no extra navigation
        // state, and no generic label which obscures the available panes.
        let program = match state.contents.format {
            DiscFormat::Sacd => "Areas",
            DiscFormat::DvdAudio => "Groups",
            DiscFormat::DvdVideo => "Titles",
            DiscFormat::BluRay => "Playlists",
        };
        let item = if matches!(state.contents.format, DiscFormat::DvdVideo | DiscFormat::BluRay) {
            "Chapters"
        } else { "Tracks" };
        let tabs = [(program, ExtractFocus::Programs),
            (item, ExtractFocus::Items), ("Output", ExtractFocus::Output)];
        let mut line = Vec::new();
        for (i, (label, focus)) in tabs.iter().enumerate() {
            if i != 0 { line.push(Span::styled("  ", Style::default().fg(theme.text_dim))); }
            let style = if state.focus == *focus {
                Style::default().fg(theme.panel_bg).bg(theme.tab_active).add_modifier(Modifier::BOLD)
            } else { Style::default().fg(theme.label) };
            line.push(Span::styled(format!(" {label} "), style));
        }
        frame.render_widget(Paragraph::new(Line::from(line))
            .style(Style::default().bg(theme.panel_bg)), chunks[4]);
    } else { draw_output(frame,state,chunks[4],buttons,theme,wide); }
    if let Some(problem) = problems.first() {
        frame.render_widget(Paragraph::new(clip(&format!("⚠ {problem}"),chunks[6].width))
            .style(Style::default().fg(theme.error)),chunks[6]);
    }
    draw_results(frame,state,scratch,chunks[7],small,theme);
    rule(frame,chunks[8],theme);
    draw_footer(frame,state,chunks[9],buttons,small,problems.is_empty(),theme);
    rule(frame,Rect::new(popup.x+1,popup.y+popup.height-1,popup.width-2,1),theme);
    if let Some(panel) = state.panel { draw_panel(frame,state,panel,popup,theme); }
}

fn draw_header(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, small: bool, theme: Theme) {
    if r.width == 0 || r.height == 0 { return; }
    // Do not emit empty separators when a disc has no genre or year.
    let subtitle = [
        Some(s.contents.album_title.as_deref().unwrap_or(&s.contents.label)),
        Some(s.contents.album_artist.as_deref().unwrap_or("no disc text")),
        s.contents.genre.as_deref(), s.contents.year.as_deref(),
    ].into_iter().flatten().filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>().join(" · ");
    let mut lines = vec![Line::from(Span::styled(clip(&subtitle, r.width),
        Style::default().fg(theme.value)))];
    if !small && s.contents.format == DiscFormat::Sacd {
        let details = s.contents.presentations.first()
            .and_then(|p| p.extract.sacd_header_details.as_deref())
            .unwrap_or("SACD · native DSD64");
        lines.push(Line::from(Span::styled(clip(details,r.width),
            Style::default().fg(theme.text_dim))));
    }
    let diagnostics = if s.contents.diagnostics.is_empty() { "none".to_owned() }
        else { s.contents.diagnostics.len().to_string() };
    let color = if s.contents.diagnostics.is_empty() { theme.value } else { theme.warning };
    let mut summary = Vec::new();
    if !(small && s.contents.format == DiscFormat::Sacd) {
        let protection = s.protection_display();
        // Reserve room for diagnostics and its shortcut, even for a long
        // CSS/AACS protection description on the compact two-line header.
        let reserved = if small {
            "Diagnostics  ".chars().count() + diagnostics.chars().count()
                + if s.contents.diagnostics.is_empty() { 0 } else { "  · d view".chars().count() }
                + 2
        } else { 0 };
        let available = (r.width as usize).saturating_sub(reserved);
        if !small || available > "Protection  ".chars().count() {
            summary.push(Span::styled("Protection  ",Style::default().fg(theme.label)));
            let value = if small {
                clip(&protection, available.saturating_sub("Protection  ".len()) as u16)
            } else { protection };
            summary.push(Span::styled(format!("{value}  "),
                Style::default().fg(theme.value)));
        }
    }
    summary.push(Span::styled("Diagnostics  ",Style::default().fg(theme.label)));
    summary.push(Span::styled(diagnostics,Style::default().fg(color)));
    if !s.contents.diagnostics.is_empty() {
        summary.push(Span::styled("  · d view",Style::default().fg(theme.info)));
    }
    lines.push(Line::from(summary));
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(theme.panel_bg)),r);
}

fn draw_programs(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, buttons: &mut ButtonRenderMap, theme: Theme) {
    let title = match s.contents.format { DiscFormat::Sacd=>"AREAS",DiscFormat::DvdAudio=>"GROUPS",DiscFormat::DvdVideo=>"TITLES",DiscFormat::BluRay=>"PLAYLISTS" };
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(format!("{title}  "),Style::default().fg(theme.header)),
        Span::styled(format!("{} of {} checked · {} hidden",s.selected_program_count(),
            s.len(),s.contents.suppressed.len()),Style::default().fg(theme.text_dim)),
    ])),Rect::new(r.x+1,r.y,r.width.saturating_sub(2),1));
    let inner = Rect::new(r.x+1,r.y+1,r.width.saturating_sub(2),r.height.saturating_sub(1));
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
            display_rows.push((Some(i),name,s.focus==ExtractFocus::Programs && s.cursor==i,None));
            if !is_title {
                display_rows.push((None,format!("      {}",program_info(p,s.contents.format)),false,None));
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
        let active = s.focus==ExtractFocus::Programs && *focused;
        let bg = if active { theme.selection_bg } else { theme.panel_bg };
        frame.render_widget(Block::default().style(Style::default().bg(bg)),
            Rect::new(inner.x,y,inner.width,1));
        if let Some(i)=index {
            buttons.record_button(TuiButton::DiscBrowserStream(*i),Rect::new(inner.x,y,inner.width,1));
            buttons.record_button(TuiButton::DiscBrowserExpand(*i),
                Rect::new(inner.x+2,y,4.min(inner.width.saturating_sub(2)),1));
        }
        let mut text = label.as_str();
        let mut suffix = None;
        if let Some(i)=index {
            let p=&s.contents.presentations[*i];
            if matches!(s.contents.format,DiscFormat::Sacd|DiscFormat::DvdAudio) {
                let tail=format!("{} · {} tracks",duration_display(p.total_duration_secs),p.tracks.len());
                if let Some(base) = label.strip_suffix(&tail) {
                    suffix=Some(tail);
                    text=base.trim_end();
                }
            }
        } else if label.contains(" chapters") {
            // Title/playlist group headings align like SACD program rows.
            if let Some(split) = label.rfind("  ") {
                if label[split+2..].contains(" chapters") {
                    suffix=Some(label[split+2..].to_owned());
                    text=&label[..split];
                }
            }
        }
        let right_width = suffix.as_ref().map_or(0,|t|t.chars().count() as u16);
        let left_width=inner.width.saturating_sub(right_width + u16::from(right_width>0));
        let mut spans = Vec::new();
        spans.push(Span::styled(if active { "▸ " } else { "  " },
            Style::default().fg(theme.info)));
        if let Some(mark) = text.find('[') {
            let prefix=&text[..mark];
            spans.push(Span::styled(prefix.to_owned(),Style::default().fg(theme.text_dim)));
            if text[mark..].starts_with("[✓]") || text[mark..].starts_with("[ ]") {
                spans.push(Span::styled("[",Style::default().fg(theme.text_dim)));
                spans.push(Span::styled(if text[mark..].starts_with("[✓]") {"✓"}else{" "},
                    Style::default().fg(theme.success)));
                spans.push(Span::styled("]",Style::default().fg(theme.text_dim)));
                let marker_bytes=if text[mark..].starts_with("[✓]") {5}else{3};
                spans.push(Span::styled(text[mark+marker_bytes..].to_owned(),
                    Style::default().fg(if index.is_some_and(|i|!s.is_extractable(i)) {theme.error}
                        else {theme.value})));
            } else {
                spans.push(Span::styled(text[mark..].to_owned(),Style::default().fg(theme.value)));
            }
        } else {
            spans.push(Span::styled(text.to_owned(),Style::default().fg(theme.text_dim)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            Rect::new(inner.x,y,left_width,1));
        if let Some(tail)=suffix {
            frame.render_widget(Paragraph::new(tail).alignment(Alignment::Right)
                .style(Style::default().bg(bg).fg(theme.text_dim)),
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
    let kind=if matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay) {
        "CHAPTERS" } else { "TRACKS" };
    let checked=s.items_checked.get(s.cursor).map_or(0,|v|v.iter().filter(|x|**x).count());
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(format!("{kind}  "),Style::default().fg(theme.header)),
        Span::styled(format!("{checked} of {} checked · {}",p.tracks.len(),p.id.compact_label()),
            Style::default().fg(theme.text_dim)),
    ])),Rect::new(r.x+1,r.y,r.width.saturating_sub(2),1));
    let inner=Rect::new(r.x+1,r.y+1,r.width.saturating_sub(2),r.height.saturating_sub(1));
    if inner.width<12 || inner.height==0 { return; }
    let cursor=s.item_cursor.min(p.tracks.len().saturating_sub(1));
    let sacd_detail=(s.contents.format==DiscFormat::Sacd)
        .then(||p.tracks.get(cursor).and_then(|t|t.format_note.as_deref()))
        .flatten().filter(|note|note.contains("ISRC ")||note.contains("Composer ")
            ||note.contains("Songwriter ")||note.contains("Arranger ")||note.contains("Genre "));
    let capacity = inner.height.saturating_sub(u16::from(sacd_detail.is_some())).max(1);
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
    for (offset,(i,track)) in p.tracks.iter().enumerate().skip(scroll)
        .take(visible as usize).enumerate() {
        let y=inner.y+offset as u16;
        let focused=s.focus==ExtractFocus::Items && i==cursor;
        let bg=if focused {theme.selection_bg}else{theme.panel_bg};
        frame.render_widget(Block::default().style(Style::default().bg(bg)),Rect::new(inner.x,y,inner.width,1));
        let style=Style::default().bg(bg);
        let mut x=inner.x;
        frame.render_widget(Paragraph::new(if focused {"▸"}else{" "}).style(style.fg(theme.info)),
            Rect::new(x,y,1,1));
        x+=2;
        let checked=s.items_checked.get(s.cursor).and_then(|v|v.get(i))==Some(&true);
        frame.render_widget(Paragraph::new(Line::from(vec![
            Span::styled("[",style.fg(theme.text_dim)),
            Span::styled(if checked {"✓"}else{" "},style.fg(theme.success)),
            Span::styled("]",style.fg(theme.text_dim)),
        ])).style(style),Rect::new(x,y,3,1));
        let check_x=x;
        x+=4;
        let number_width=if track.number>99 {4}else{3};
        frame.render_widget(Paragraph::new(format!("{:02}",track.number))
            .style(style.fg(theme.text_dim)),Rect::new(x,y,number_width,1));
        x+=number_width+1;
        let tail_width=if wide {15u16}else{8u16};
        let performer_width=if wide {((inner.width.saturating_sub(32))/4).min(24)}else{0};
        let title_width=inner.x+inner.width;
        let title_width=title_width.saturating_sub(x+tail_width+performer_width);
        frame.render_widget(Paragraph::new(clip(track.title.as_deref().unwrap_or("Untitled"),title_width))
            .style(style.fg(theme.value)),Rect::new(x,y,title_width,1));
        x+=title_width;
        if wide && performer_width>0 {
            frame.render_widget(Paragraph::new(clip(track.performer.as_deref().unwrap_or(""),performer_width))
                .style(style.fg(theme.text_dim)),Rect::new(x,y,performer_width,1));
            x+=performer_width;
        }
        let duration=track.duration_secs.map(duration_display).unwrap_or_default();
        let duration_width=if wide {7}else{8};
        frame.render_widget(Paragraph::new(duration).alignment(Alignment::Right)
            .style(style.fg(theme.text_dim)),Rect::new(x,y,duration_width,1));
        x+=duration_width;
        if wide {
            let start=start_secs.map(duration_display).unwrap_or_default();
            frame.render_widget(Paragraph::new(start).alignment(Alignment::Right)
                .style(style.fg(theme.text_dim)),Rect::new(x,y,8,1));
        }
        start_secs=start_secs.and_then(|secs|track.duration_secs.map(|duration|secs+duration));
        buttons.record_button(TuiButton::DiscBrowserTrack(i),Rect::new(inner.x,y,inner.width,1));
        buttons.record_button(TuiButton::DiscBrowserTrackCheck(i),Rect::new(check_x,y,3,1));
    }
    if more_below {
        let hidden = remaining.saturating_sub(visible as usize);
        frame.render_widget(Paragraph::new(format!("    ↓ {hidden} more"))
            .style(Style::default().fg(theme.text_dim).bg(theme.panel_bg)),
            Rect::new(inner.x,inner.y + visible,inner.width,1));
    }
    if let Some(note)=sacd_detail {
        let y=inner.y+capacity;
        if y<inner.y+inner.height {
            frame.render_widget(Paragraph::new(clip(note,inner.width))
                .style(Style::default().fg(theme.text_dim)),Rect::new(inner.x,y,inner.width,1));
        }
    }
}


fn option_value(s: &DiscBrowserState, name: &str) -> String {
    match name {
        "Format" if s.contents.format==DiscFormat::Sacd =>
            if s.keep_dst.iter().any(|v| *v) { "DSF · DST decoded; some areas keep DST in DFF".into() }
            else { "DSF · DST decoded to uncompressed DSD".into() },
        "Format" => match s.wav_container {
            NativeWavContainer::Automatic => "WAV · automatic RIFF/RF64 · channel mask".into(),
            NativeWavContainer::Riff => "RIFF WAV · 4 GB limit".into(),
            NativeWavContainer::Rf64 => "RF64 WAV · large-file capable".into(),
            NativeWavContainer::W64 => "W64 · 64-bit sizes".into(),
        },
        "Files" => (if s.file_per_program { if s.cue {"continuous per program + CUE"} else {"continuous per program"} } else if matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay) { "one per chapter" } else { "one per track" }).into(),
        "Tags" => if s.tags { format!("on · {} selected fields · artwork {}{}",
            s.tag_fields.count_ones(),if s.tag_artwork {"on"}else{"off"},
            if s.keep_dst.iter().any(|v|*v) {" · DFF drops tags"}else{""}) }
            else { "off · no metadata written".into() },
        "Destination" => format!("{} · {}",s.destination.display(),s.naming_template),
        "Downmix" => s.downmix.as_str().to_string(),
        "Assume decrypted" => if s.assume_decrypted {"on · already decrypted"} else {"off"}.into(),
        _ => String::new(),
    }
}

/// Stable logical index -> output cell mapping. All visible controls retain
/// their existing logical indices, independent of viewport and column count.
fn output_cell(index: usize, count: usize, area: Rect, wide: bool, focus: usize) -> Option<Rect> {
    if count == 0 || area.height == 0 || area.width == 0 { return None; }
    let columns = if wide && area.width >= 70 { 2usize } else { 1usize };
    let per_column = count.div_ceil(columns);
    let col = index / per_column;
    let row = index % per_column;
    let visible = area.height as usize;
    let focused_row = focus.min(count.saturating_sub(1)) % per_column;
    let scroll = focused_row.saturating_sub(visible.saturating_sub(1))
        .min(per_column.saturating_sub(visible));
    if row < scroll || row >= scroll + visible { return None; }
    let column_width = area.width / columns as u16;
    let x = area.x + col as u16 * column_width;
    let width = if col + 1 == columns { area.width - (x - area.x) } else { column_width };
    Some(Rect::new(x, area.y + (row - scroll) as u16, width, 1))
}

/// Highlight template tokens without granting the renderer any knowledge of
/// path expansion. Only the native job builder interprets those tokens.
fn value_spans(value: &str, theme: Theme) -> Vec<Span<'static>> {
    let mut spans=Vec::new();
    let mut rest=value;
    while let Some(start)=rest.find('{') {
        let Some(end)=rest[start..].find('}') else {break;};
        if start>0 {spans.push(Span::styled(rest[..start].to_owned(),
            Style::default().fg(theme.value)));}
        let length=start+end+1;
        spans.push(Span::styled(rest[start..length].to_owned(),
            Style::default().fg(theme.purple)));
        rest=&rest[length..];
    }
    if !rest.is_empty() {spans.push(Span::styled(rest.to_owned(),
        Style::default().fg(theme.value)));}
    spans
}

fn draw_output(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,
    buttons:&mut ButtonRenderMap,theme:Theme,wide:bool) {
    let rows=s.output_rows();
    for (i,name) in rows.iter().enumerate() {
        let Some(cell)=output_cell(i,rows.len(),r,wide,s.output_cursor) else {continue;};
        let selected=s.focus==ExtractFocus::Output && s.output_cursor==i;
        let bg=if selected {theme.selection_bg}else{theme.panel_bg};
        frame.render_widget(Block::default().style(Style::default().bg(bg)),cell);
        let label_width=if wide {13usize}else{18usize};
        let value_width=cell.width.saturating_sub(label_width as u16+2);
        let mut spans=vec![
            Span::styled(if selected {"▸"}else{" "},Style::default().fg(theme.info)),
            Span::styled(format!("{name:<label_width$} "),Style::default().fg(theme.label)),
        ];
        spans.extend(value_spans(&clip(&option_value(s,name),value_width),theme));
        frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),cell);
        buttons.record_button(TuiButton::DiscBrowserOutput(i),cell);
    }
}

fn draw_results(frame:&mut ratatui::Frame,s:&DiscBrowserState,
    scratch:&Path,r:Rect,small:bool,theme:Theme) {
    let items=s.selected_item_count();
    let programs=s.selected_program_count();
    let size=s.selected_item_bytes().map_or_else(||"pending".into(),|(total,_,_)|format_bytes(total));
    let kind=if matches!(s.contents.format,DiscFormat::DvdVideo|DiscFormat::BluRay) {
        "chapters" }else{"tracks"};
    let group=match s.contents.format {
        DiscFormat::Sacd=>if programs==1 {"area"}else{"areas"},
        DiscFormat::DvdAudio=>if programs==1 {"group"}else{"groups"},
        DiscFormat::DvdVideo=>if programs==1 {"title"}else{"titles"},
        DiscFormat::BluRay=>if programs==1 {"playlist"}else{"playlists"},
    };
    let main=format!("{items} {kind} · {programs} {group} · about {size}");
    let destination=space_for(&s.destination).map_or_else(||"unknown".into(),format_bytes);
    let available=space_for(scratch).map_or_else(||"unknown".into(),format_bytes);
    let space=if s.native_storage_shares_destination_volume(scratch) {
        let required=s.native_required_volume_bytes(false).map_or_else(||"pending".into(),format_bytes);
        format!("destination {destination} free · working storage needs {required}, {available} free (shared volume)")
    }else{
        let (target,work)=s.native_split_volume_bytes(false)
            .map_or_else(||("pending".into(),"pending".into()),|(target,work)|
                (format_bytes(target),format_bytes(work)));
        format!("destination needs {target}, {destination} free · working storage needs {work}, {available} free")
    };
    for (row,(label,value)) in [ ("Extract",main),("Space",space) ]
        .into_iter().take(if small {1}else{2}).enumerate() {
        let width=r.width.saturating_sub(10);
        frame.render_widget(Paragraph::new(Line::from(vec![
            Span::styled(format!("{label:<10}"),Style::default().fg(theme.label)),
            Span::styled(clip(&value,width),Style::default().fg(theme.text_dim)),
        ])).style(Style::default().bg(theme.panel_bg)),Rect::new(r.x,r.y+row as u16,r.width,1));
    }
}

fn draw_footer(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,buttons:&mut ButtonRenderMap,small:bool,can_extract:bool,theme:Theme) {
    if r.height==0 {return;}
    if !small {
        let hint = "↑↓ move · ←→ pane · Space check · a n i all none invert · ⇧↑↓ range · [ ] cycle · Tab focus · ↵ open · h hidden";
        frame.render_widget(Paragraph::new(Line::from(vec![
            Span::styled("↑↓",Style::default().fg(theme.info)),
            Span::styled(clip(&hint["↑↓".len()..],r.width.saturating_sub(2)),
                Style::default().fg(theme.text_dim)),
        ])),Rect::new(r.x,r.y,r.width,1));
    }
    let y=r.y+r.height-1;
    let chips=[("^x Extract",TuiButton::DiscBrowserExtract,can_extract,theme.chip_go),
        ("c Convert",TuiButton::DiscBrowserConvert,true,theme.tab_active),
        ("m Options",TuiButton::DiscBrowserMore,true,theme.warning),
        ("Esc Close",TuiButton::DiscBrowserClose,true,theme.chip_dismiss)];
    let length:usize=chips.iter().map(|(txt,_,_,_)|txt.chars().count()+2).sum();
    let mut x=r.x+r.width.saturating_sub(length as u16)/2;
    for (label,button,enabled,color) in chips {
        let text=format!(" {label} ");
        let width=text.chars().count() as u16;
        if x+width>r.x+r.width {break;}
        let style = if enabled { Style::default().bg(color).fg(theme.panel_bg) }
            else { Style::default().bg(theme.text_dim).fg(theme.panel_bg) };
        frame.render_widget(Paragraph::new(text).style(style),Rect::new(x,y,width,1));
        if enabled { buttons.record_button(button,Rect::new(x,y,width,1)); }
        x=x.saturating_add(width);
    }
    let _=s;
}

fn draw_panel(frame:&mut ratatui::Frame,s:&DiscBrowserState,panel:ExtractPanel,popup:Rect,theme:Theme) {
    let max_w=popup.width.saturating_sub(8).min(75);
    let max_h=popup.height.saturating_sub(6).min(17);
    let area=Rect::new(popup.x+(popup.width-max_w)/2,popup.y+(popup.height-max_h)/2,max_w,max_h);
    frame.render_widget(Clear,area);
    frame.render_widget(Block::default().style(Style::default().bg(theme.panel_bg)),area);
    let title=match panel {
        ExtractPanel::Format=>"Format",ExtractPanel::Files=>"Files",ExtractPanel::Tags=>"Tags",
        ExtractPanel::Destination=>"Destination",ExtractPanel::Downmix=>"Downmix",
        ExtractPanel::AssumeDecrypted=>"Assume decrypted",ExtractPanel::Diagnostics=>"Diagnostics",
        ExtractPanel::More=>"More",ExtractPanel::ConvertChoice=>"Convert selection",
    };
    let block=Block::default().borders(Borders::ALL).title(format!(" {title} "))
        .style(Style::default().bg(theme.panel_bg).fg(theme.value))
        .border_style(Style::default().fg(theme.title));
    let inner=block.inner(area);
    frame.render_widget(block,area);
    let lines:Vec<String>=match panel {
        ExtractPanel::Format=> {
            if s.contents.format==DiscFormat::Sacd { vec![
                "DSF (DST decoded) by default".into(),
                "k  Toggle Keep DST for highlighted DST area (DFF)".into(),
                "DFF cannot store tags".into(),
            ]} else {vec!["[ ] cycle: Automatic / RIFF / RF64 / W64".into(),
                format!("Current: {}",option_value(s,"Format")),
                "Audio is decoded, not resampled or gain-adjusted".into()] }
        },
        ExtractPanel::Files=>vec!["f  Toggle one file per track/chapter or program".into(),
            format!("Current: {}", option_value(s,"Files")),
            if s.contents.format == DiscFormat::Sacd {
                "CUE is not supported for DSF/DFF; continuous native DSD is available".into()
            } else { "c  Toggle CUE sheet for continuous PCM".into() },
            "CUE indexes round to 1/75 second; native audio stays sample-exact".into()],
        ExtractPanel::Tags=>vec![
            format!("t  Transfer tags: {}", if s.tags {"on"}else{"off"}),
            format!("s  Source: {}", match s.tag_source {
                crate::convert::pipeline::native_extract::NativeTagSource::DiscAndSidecar =>
                    if s.contents.format == DiscFormat::Sacd { "sidecar, then disc text" }
                    else { "disc/sidecar metadata" },
                crate::convert::pipeline::native_extract::NativeTagSource::AuthoredTrack =>
                    if s.contents.format == DiscFormat::Sacd { "disc text, then sidecar" }
                    else { "materialized track metadata" },
            }),
            format!("1-7  Fields: {} selected (title/artist/album/genre/year/composer/performer)",s.tag_fields.count_ones()),
            format!("a  Artwork: {} (source or direct-folder cover)",if s.tag_artwork {"on"}else{"off"}),
            if s.keep_dst.iter().any(|v|*v) {
                "DFF cannot hold tags; selected DST-preserving DFF tracks drop them".into()
            } else {"Tags apply to completed private files before publication".into()},
            "k  Keep tags: change selected DFF areas to DSF".into(),
            "e  Edit tags (existing disc-sidecar editor)".into(),
            "Esc  Back".into(),
        ],
        ExtractPanel::Destination=>vec![
            "p  Pick folder  ·  t  Edit naming template".into(),
            format!("Folder: {}", s.destination.display()),
            format!("Template: {}",s.naming_template),
            format!("s  Program subfolders: {}",if s.program_subfolders {"on"}else{"off"}),
            format!("o  Existing output: {:?}",s.overwrite_policy),
            format!("l  Extraction log: {}",if s.extraction_log {"on"}else{"off"}),
            "Tokens: {area} {group} {title} {playlist} {stream}".into(),
            "        {track} {chapter} and available tag fields".into(),
            if s.editing_destination {format!("> {}",s.destination_input)}
                else {"Only complete files appear in the destination".into()},
        ],
        ExtractPanel::Downmix=>vec!["[ / ]  Cycle DVD-Audio downmix policy".into(),
            format!("Current: {}", option_value(s,"Downmix")),
            "none preserves every source channel".into()],
        ExtractPanel::AssumeDecrypted=>vec!["[ / ]  Toggle the already-decrypted assumption".into(),
            format!("Current: {}", option_value(s,"Assume decrypted")),
            "Enable only for a copy you know is already decrypted".into()],
        ExtractPanel::Diagnostics=>s.contents.diagnostics.iter().map(|d|format!("{d:?}")).collect(),
        ExtractPanel::More=>{
            let mut actions = vec!["e  Edit tags (where supported)".into(),
                "b  MusicBrainz using existing lookup".into(),
                "h  Show / hide hidden programs".into(),
                "d  View diagnostics".into(),
                "c  Convert highlighted presentation".into()];
            if s.contents.format != DiscFormat::Sacd {
                actions.push("a  Analyze disc again".into());
            }
            if s.contents.format == DiscFormat::DvdAudio {
                actions.push(format!("g  Select groups: {}",match s.dvda_group_preset {
                    0=>"all",1=>"prefer stereo",2=>"prefer multichannel",_=>"highest resolution"
                }));
            }
            actions
        },
        ExtractPanel::ConvertChoice=>vec!["Enter  Convert highlighted program".into(),
            "e  Queue every checked program separately".into(),
            "Esc  Return to extraction".into()],
    };
    if lines.is_empty(){frame.render_widget(Paragraph::new("None"),inner);}else{
        let rendered=lines.into_iter().skip(s.panel_cursor).take(inner.height as usize).map(|l|Line::from(clip(&l,inner.width))).collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(rendered),inner);
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
            assert_eq!(buffer.get(2, 1).bg, theme.title, "title bar {width}x{height}");
            assert_eq!(buffer.get(2, 2).bg, theme.panel_bg,
                "dialog surface {width}x{height}");
            assert_eq!(buffer.get(1, 2).fg, theme.title, "outer rail {width}x{height}");
        }
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
            terminal.draw(|f| draw_results(f, &state, &scratch, Rect::new(0, 0, 170, 2), false, theme)).unwrap();
            let result = (0..170).map(|x| terminal.backend().buffer().get(x,1).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            if state.native_storage_shares_destination_volume(&scratch) {
                assert!(result.contains(&format!("destination {expected} free")), "{result}");
            } else {
                assert!(result.contains(&format!(", {expected} free")), "{result}");
            }
            let scratch_free=space_for(&scratch).map_or_else(||"unknown".into(),format_bytes);
            assert!(result.contains(&format!(", {scratch_free} free")), "{result}");
        }
    }
}

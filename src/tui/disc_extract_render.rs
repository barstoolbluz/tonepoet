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
    else { format!("{bytes} B") }
}

fn row_style(focused: bool, selected: bool, theme: Theme) -> Style {
    if focused && selected { Style::default().fg(theme.pill_active_fg).bg(theme.selection_bg).add_modifier(Modifier::BOLD) }
    else { Style::default().fg(theme.text) }
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

pub fn draw(
    frame: &mut ratatui::Frame,
    state: &DiscBrowserState,
    scratch: &Path,
    buttons: &mut ButtonRenderMap,
    theme: Theme,
) {
    let area = frame.size();
    if area.width < 64 || area.height < 20 {
        let label = "Make the terminal at least 64×20 to extract.";
        frame.render_widget(Paragraph::new(label).alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1));
        return;
    }
    let wide = area.width >= 140 && area.height >= 36;
    let small = area.width < 96 || area.height < 30;
    let popup = Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2);
    frame.render_widget(Clear, popup);
    let source_name = state.source_path.file_name()
        .map(|v| v.to_string_lossy().into_owned()).unwrap_or_else(|| state.contents.label.clone());
    let action = match state.contents.format {
        DiscFormat::Sacd | DiscFormat::DvdAudio => "extract tracks",
        DiscFormat::DvdVideo | DiscFormat::BluRay => "extract chapters",
    };
    let type_name = format!("{} {}", state.contents.format.name(), if state.source_path.is_dir() { "folder" } else { "ISO" });
    let title = clip(&format!("▸ {action} · {source_name}                            {type_name}"), popup.width - 2);
    let border = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.border))
        .title(Span::styled(title, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)));
    let inner = border.inner(popup);
    frame.render_widget(border, popup);

    // Reserve presentation rows first. Small terminals hide inactive panes
    // instead of shrinking the lists into unreadable two-column slivers.
    let header_height = if small { 2 } else { 3 };
    let footer_height = if small { 1 } else { 2 };
    let results_height = if small { 1 } else { 2 };
    // Wide output is two columns; medium stacks every row. Both reserve
    // enough height for DVD-Audio's conditional controls.
    let output_count = state.output_rows().len() as u16;
    let output_height = if small { 2 } else if wide { output_count.div_ceil(2) } else { output_count };
    let warning_height = if state.blockers(scratch).is_empty() { 0 } else { 1 };
    let chunks = Layout::default().direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), Constraint::Min(1),
            Constraint::Length(output_height), Constraint::Length(warning_height),
            Constraint::Length(results_height), Constraint::Length(footer_height),
        ]).split(inner);
    draw_header(frame, state, chunks[0], small, theme);
    if small {
        match state.focus {
            ExtractFocus::Programs => draw_programs(frame,state,chunks[1],buttons,theme),
            ExtractFocus::Items => draw_items(frame,state,chunks[1],buttons,theme,false),
            ExtractFocus::Output => draw_output(frame,state,chunks[1],buttons,theme,false),
        }
    } else {
        let prefix = (34u16.min(chunks[1].width.saturating_sub(1))).max(chunks[1].width * 34 / 100);
        let program_width = prefix.min(chunks[1].width * 50 / 100);
        let panes = Layout::default().direction(Direction::Horizontal)
            .constraints([Constraint::Length(program_width), Constraint::Min(1)]).split(chunks[1]);
        draw_programs(frame,state,panes[0],buttons,theme);
        draw_items(frame,state,panes[1],buttons,theme,wide);
    }
    if !small { draw_output(frame,state,chunks[2],buttons,theme,wide); }
    else {
        let tabs = match state.focus { ExtractFocus::Programs=>"AREAS / GROUPS / TITLES / PLAYLISTS", ExtractFocus::Items=>"TRACKS / CHAPTERS", ExtractFocus::Output=>"OUTPUT" };
        frame.render_widget(Paragraph::new(format!("Tab: {tabs}")),chunks[2]);
    }
    let problems = state.blockers(scratch);
    if let Some(problem) = problems.first() {
        frame.render_widget(Paragraph::new(clip(&format!("⚠ {problem}"), chunks[3].width))
            .style(Style::default().fg(theme.error)), chunks[3]);
    }
    draw_results(frame,state,chunks[4],small,theme);
    draw_footer(frame,state,chunks[5],buttons,small,problems.is_empty(),theme);
    if let Some(panel) = state.panel { draw_panel(frame,state,panel,popup,theme); }
}

fn draw_header(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, small: bool, theme: Theme) {
    let mut lines = vec![Line::from(clip(&format!("{} · {} · {} · {}",
        s.contents.album_title.as_deref().unwrap_or(&s.contents.label),
        s.contents.album_artist.as_deref().unwrap_or("no disc text"),
        s.contents.genre.as_deref().unwrap_or(""),
        s.contents.year.as_deref().unwrap_or("")),r.width))];
    if !small && s.contents.format == DiscFormat::Sacd {
        let details = s.contents.presentations.first()
            .and_then(|p| p.extract.sacd_header_details.as_deref())
            .unwrap_or("SACD · native DSD64");
        lines.push(Line::from(clip(details, r.width)));
    }
    let diagnostics = if s.contents.diagnostics.is_empty() { "none".to_string() }
        else { format!("{}", s.contents.diagnostics.len()) };
    let summary = if s.contents.format == DiscFormat::Sacd {
        // At 64x20 the second header row is the diagnostics affordance.
        // Authored SACD details remain visible on medium/wide terminals.
        format!("Diagnostics  {diagnostics}  · d view")
    } else {
        format!("Protection  {}    Diagnostics  {diagnostics}  · d view", s.protection_display())
    };
    let summary_color = if s.contents.diagnostics.is_empty() { theme.text_dim } else { theme.warning };
    lines.push(Line::from(Span::styled(clip(&summary, r.width),Style::default().fg(summary_color))));
    frame.render_widget(Paragraph::new(lines),r);
}

fn draw_programs(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, buttons: &mut ButtonRenderMap, theme: Theme) {
    let title = match s.contents.format { DiscFormat::Sacd=>"AREAS",DiscFormat::DvdAudio=>"GROUPS",DiscFormat::DvdVideo=>"TITLES",DiscFormat::BluRay=>"PLAYLISTS" };
    let header = format!("{title}  {} checked · {} hidden",s.selected_program_count(),s.contents.suppressed.len());
    let inner = Rect::new(r.x+1,r.y+1,r.width.saturating_sub(2),r.height.saturating_sub(2));
    frame.render_widget(Block::default().borders(Borders::ALL).title(header)
        .border_style(Style::default().fg(if s.focus==ExtractFocus::Programs {theme.info} else {theme.border_dim})),r);
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
            display_rows.push((None, format!("{}  {} · {} chapters",
                header_for(first.id, first, group_start),
                duration_display(first.total_duration_secs), first.tracks.len()), false, None));
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
                format!("{checkmark}  {}  {} · {} tracks",
                    header_for(p.id,p,i),duration_display(p.total_duration_secs),p.tracks.len())
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
    for (offset,(index,label,focused,angle_owner)) in display_rows.iter().skip(scroll).take(inner.height as usize).enumerate() {
        let y=inner.y+offset as u16;
        let mut style=row_style(s.focus==ExtractFocus::Programs,*focused,theme);
        if index.is_none() { style=style.fg(theme.text_dim); }
        if index.is_some_and(|i| !s.is_extractable(i)) && !*focused {
            style=style.fg(theme.error);
        }
        if let Some(i)=index {
            buttons.record_button(TuiButton::DiscBrowserStream(*i),Rect::new(inner.x,y,inner.width,1));
            buttons.record_button(TuiButton::DiscBrowserExpand(*i),Rect::new(inner.x,y,4.min(inner.width),1));
        }
        if let Some(owner) = angle_owner {
            for (symbol, button) in [('◂', TuiButton::DiscBrowserAnglePrev(*owner)),
                                     ('▸', TuiButton::DiscBrowserAngleNext(*owner))] {
                if let Some(col) = label.chars().position(|ch| ch == symbol) {
                    if col < inner.width as usize {
                        buttons.record_button(button, Rect::new(inner.x + col as u16, y, 1, 1));
                    }
                }
            }
        }
        frame.render_widget(Paragraph::new(clip(label,inner.width)).style(style),Rect::new(inner.x,y,inner.width,1));
    }
}

fn draw_items(frame: &mut ratatui::Frame, s: &DiscBrowserState, r: Rect, buttons: &mut ButtonRenderMap, theme: Theme, wide: bool) {
    let Some(p)=s.selected_presentation() else { return; };
    let kind=if s.contents.format==DiscFormat::DvdVideo || s.contents.format==DiscFormat::BluRay { "CHAPTERS" } else { "TRACKS" };
    let checked=s.items_checked.get(s.cursor).map_or(0,|v| v.iter().filter(|x| **x).count());
    let label=clip(&format!("{kind}  {checked} of {} checked · {}",p.tracks.len(),p.id.compact_label()),r.width.saturating_sub(2));
    frame.render_widget(Block::default().borders(Borders::ALL).title(label)
        .border_style(Style::default().fg(if s.focus==ExtractFocus::Items {theme.info} else {theme.border_dim})),r);
    let inner=Rect::new(r.x+1,r.y+1,r.width.saturating_sub(2),r.height.saturating_sub(2));
    if inner.width==0 || inner.height==0 { return; }
    let cursor=s.item_cursor.min(p.tracks.len().saturating_sub(1));
    // Show a conditional SACD detail line only when the disc actually has
    // extra per-track fields. No placeholder text on discs without disc text.
    let sacd_detail = (s.contents.format == DiscFormat::Sacd)
        .then(|| p.tracks.get(cursor).and_then(|t| t.format_note.as_deref()))
        .flatten()
        .filter(|note| note.contains("ISRC ") || note.contains("Composer ")
            || note.contains("Songwriter ") || note.contains("Arranger ")
            || note.contains("Genre "));
    let visible_rows = inner.height.saturating_sub(u16::from(sacd_detail.is_some())).max(1);
    let scroll = if cursor < s.item_scroll { cursor }
        else if cursor >= s.item_scroll+visible_rows as usize { cursor+1-visible_rows as usize }
        else { s.item_scroll }.min(p.tracks.len().saturating_sub(visible_rows as usize));
    let mut start_secs = Some(0.0_f64);
    for earlier in p.tracks.iter().take(scroll) {
        start_secs = start_secs.and_then(|start| earlier.duration_secs.map(|duration| start + duration));
    }
    for (offset,(i,track)) in p.tracks.iter().enumerate().skip(scroll).take(visible_rows as usize).enumerate() {
        let mut label=format!("{} {:02}  {}",check(s.items_checked.get(s.cursor).and_then(|v|v.get(i))==Some(&true)),
            track.number,track.title.as_deref().unwrap_or("Untitled"));
        if wide { if let Some(performer)=&track.performer { label.push_str(&format!(" · {performer}")); } }
        if let Some(secs)=track.duration_secs { label.push_str(&format!("   {}",duration_display(secs))); }
        if wide { if let Some(start)=start_secs { label.push_str(&format!("   {}",duration_display(start))); } }
        start_secs = start_secs.and_then(|start| track.duration_secs.map(|duration| start + duration));
        let y=inner.y+offset as u16;
        frame.render_widget(Paragraph::new(clip(&label,inner.width)).style(row_style(s.focus==ExtractFocus::Items,i==cursor,theme)),
            Rect::new(inner.x,y,inner.width,1));
        buttons.record_button(TuiButton::DiscBrowserTrack(i),Rect::new(inner.x,y,inner.width,1));
        buttons.record_button(TuiButton::DiscBrowserTrackCheck(i),Rect::new(inner.x,y,3.min(inner.width),1));
    }
    if let Some(note) = sacd_detail {
        let y=inner.y+visible_rows;
        if y < inner.y + inner.height {
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
        "Files" => if s.file_per_program { if s.cue {"continuous per program + CUE"} else {"continuous per program"} } else { "one per track/chapter" }.into(),
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

fn draw_output(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,buttons:&mut ButtonRenderMap,theme:Theme,wide:bool) {
    let rows = s.output_rows();
    for (i,name) in rows.iter().enumerate() {
        let Some(cell) = output_cell(i, rows.len(), r, wide, s.output_cursor) else { continue; };
        let label_width = if wide { 13 } else { 14 };
        let text = format!(" {:<label_width$} {}", name, option_value(s,name));
        let selected = s.focus==ExtractFocus::Output && s.output_cursor==i;
        frame.render_widget(Paragraph::new(clip(&text,cell.width)).style(row_style(s.focus==ExtractFocus::Output,selected,theme)),cell);
        buttons.record_button(TuiButton::DiscBrowserOutput(i),cell);
    }
}

fn draw_results(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,small:bool,theme:Theme) {
    let items=s.selected_item_count();
    let programs=s.selected_program_count();
    let (size,work)=match s.selected_item_bytes() {
        Some((total,largest,program))=>(format_bytes(total),
            format_bytes(s.native_required_volume_bytes(false).unwrap_or_else(||
                s.required_native_volume_bytes(total,largest,program)))),
        None=>("pending".into(),"pending".into()),
    };
    let kind=if s.contents.format==DiscFormat::DvdVideo || s.contents.format==DiscFormat::BluRay {"chapters"} else {"tracks"};
    let main=format!("Extract  {items} {kind} · {programs} streams · about {size}");
    // The native worker always stages on the destination volume, even if
    // configured scratch storage is on another filesystem. Both figures must
    // describe that same actual volume as the preflight blocker does.
    let dest=space_for(&s.destination).map_or_else(||"unknown".into(),format_bytes);
    let work_free=dest.clone();
    let space=format!("Space  destination {dest} free · working storage needs {work}, {work_free} free");
    let lines=if small {vec![Line::from(clip(&main,r.width))]}
        else {vec![Line::from(clip(&main,r.width)),Line::from(clip(&space,r.width))]};
    frame.render_widget(Paragraph::new(lines).style(Style::default().fg(theme.text_dim)),r);
}

fn draw_footer(frame:&mut ratatui::Frame,s:&DiscBrowserState,r:Rect,buttons:&mut ButtonRenderMap,small:bool,can_extract:bool,theme:Theme) {
    if r.height==0 {return;}
    if !small {
        frame.render_widget(Paragraph::new(clip("↑↓ move · ←→ pane · Space check · a n i all none invert · ⇧↑↓ range · [ ] cycle · Tab focus · ↵ open · h hidden",r.width))
            .style(Style::default().fg(theme.text_dim)),Rect::new(r.x,r.y,r.width,1));
    }
    let y=r.y+r.height-1;
    let chips=[("^x Extract",TuiButton::DiscBrowserExtract,can_extract,theme.chip_go),
        ("c Convert…",TuiButton::DiscBrowserConvert,true,theme.blue),
        ("m … more",TuiButton::DiscBrowserMore,true,theme.warning),
        ("Esc Close",TuiButton::DiscBrowserClose,true,theme.chip_dismiss)];
    let length:usize=chips.iter().filter(|(_,_,enabled,_)|*enabled).map(|(txt,_,_,_)|txt.chars().count()+2).sum();
    let mut x=r.x+r.width.saturating_sub(length as u16)/2;
    for (label,button,enabled,color) in chips {
        if !enabled {continue;}
        let text=format!(" {label} ");
        let width=text.chars().count() as u16;
        if x+width>r.x+r.width {break;}
        frame.render_widget(Paragraph::new(text).style(Style::default().bg(color).fg(theme.bg)),Rect::new(x,y,width,1));
        buttons.record_button(button,Rect::new(x,y,width,1));
        x=x.saturating_add(width);
    }
    let _=s;
}

fn draw_panel(frame:&mut ratatui::Frame,s:&DiscBrowserState,panel:ExtractPanel,popup:Rect,theme:Theme) {
    let max_w=popup.width.saturating_sub(8).min(75);
    let max_h=popup.height.saturating_sub(6).min(17);
    let area=Rect::new(popup.x+(popup.width-max_w)/2,popup.y+(popup.height-max_h)/2,max_w,max_h);
    frame.render_widget(Clear,area);
    let title=match panel {
        ExtractPanel::Format=>"Format",ExtractPanel::Files=>"Files",ExtractPanel::Tags=>"Tags",
        ExtractPanel::Destination=>"Destination",ExtractPanel::Downmix=>"Downmix",
        ExtractPanel::AssumeDecrypted=>"Assume decrypted",ExtractPanel::Diagnostics=>"Diagnostics",
        ExtractPanel::More=>"More",ExtractPanel::ConvertChoice=>"Convert selection",
    };
    let block=Block::default().borders(Borders::ALL).title(format!(" {title} "))
        .border_style(Style::default().fg(theme.info));
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
            "e  Edit folder  ·  t  Edit template".into(),
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
                assert!(small[1].contains("d view"), "{small:?}");
                if details { assert!(!small[1].contains("Catalog")); }
                if diagnostics {
                    let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
                    let x = small[1].find('1').expect("diagnostic count") as u16;
                    assert_eq!(terminal.backend().buffer().get(x, 1).fg, theme.warning);
                }
                let (medium, _) = render_header(&state, 96, false);
                assert!(medium[2].contains(expected) && medium[2].contains("d view"));
                if details { assert!(medium[1].contains("Catalog 12345")); }
            }
        }
    }

    #[test]
    fn working_space_label_reports_destination_volume_not_configured_scratch() {
        let mut state = sacd_state(false, false);
        let destination = tempfile::tempdir().unwrap();
        state.destination = destination.path().join("extracted");
        let available = space_for(&state.destination).unwrap();
        let expected = format_bytes(available);
        // The configured scratch path may be on another filesystem. Its
        // capacity must never enter the native extraction results display.
        for configured in [None, Some(destination.path()), Some(Path::new("/dev/shm"))] {
            let _scratch = crate::tui::disc_browser_actions::native_working_root(&state, configured);
            let mut terminal = Terminal::new(TestBackend::new(170, 2)).unwrap();
            let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
            terminal.draw(|f| draw_results(f, &state, Rect::new(0, 0, 170, 2), false, theme)).unwrap();
            let result = (0..170).map(|x| terminal.backend().buffer().get(x,1).symbol().to_owned())
                .collect::<Vec<_>>().concat();
            assert!(result.contains(&format!("destination {expected} free")), "{result}");
            assert!(result.contains(&format!(", {expected} free")), "{result}");
        }
    }
}

//! Read-only filesystem Properties, matching the compact card layout.
//! No probing, filesystem operations or database access occurs during drawing.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph}, Frame,
};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::directory_properties::{FolderPropertiesState, PropertiesFocus, PropertiesSubject};
use super::theme::Theme;
use crate::directory_accounting::{AccountingService, CachedMeasurement, DirectoryView, Freshness, ScanEnd, format_bytes, format_exact};

const LABEL_WIDTH: usize = 11;

/// Size from the terminal, not a fixed card. The normal-size floors reproduce
/// roughly 95 x 21 at 100 x 26 (about 25% above the former 76 x 17 card).
/// Maximized Properties grows by up to 40%, unlike the full-screen editor.
/// Both modes retain a border cell on small terminals.
pub fn popup_rect(screen: Rect, maximized: bool) -> Option<Rect> {
    if screen.width < 24 || screen.height < 9 { return None; }
    let cap_w = screen.width.saturating_sub(2);
    let cap_h = screen.height.saturating_sub(2);
    let normal_w = ((screen.width as u32 * 70 + 99) / 100) as u16;
    let normal_h = ((screen.height as u32 * 60 + 99) / 100) as u16;
    let normal_w = normal_w.max(95).min(cap_w);
    let normal_h = normal_h.max(21).min(cap_h);
    let (width, height) = if maximized {
        (((normal_w as u32 * 7 + 4) / 5).min(cap_w as u32) as u16,
         ((normal_h as u32 * 7 + 4) / 5).min(cap_h as u32) as u16)
    } else { (normal_w, normal_h) };
    Some(Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width, height,
    ))
}

fn timestamp(time: Option<std::time::SystemTime>) -> Option<String> {
    time.map(|value| {
        let dt: chrono::DateTime<chrono::Local> = value.into();
        dt.format("%Y-%m-%d %H:%M:%S %:z").to_string()
    })
}

fn measured_at(cached: &CachedMeasurement) -> String {
    let dt: chrono::DateTime<chrono::Local> = cached.measurement.finished.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn style(color: Color) -> Style { Style::default().fg(color) }
fn dim(value: impl Into<String>, theme: Theme) -> Span<'static> { Span::styled(value.into(), style(theme.text_muted)) }
fn bright(value: impl Into<String>, theme: Theme) -> Span<'static> { Span::styled(value.into(), style(theme.blue)) }

fn labeled(label: &str, value: Vec<Span<'static>>, theme: Theme) -> Line<'static> {
    let mut spans = Vec::with_capacity(value.len() + 1);
    spans.push(Span::styled(format!("{label:<LABEL_WIDTH$}"), style(theme.text_muted)));
    spans.extend(value);
    Line::from(spans)
}

fn text_row(label: &str, value: impl Into<String>, theme: Theme) -> Line<'static> {
    labeled(label, vec![bright(value, theme)], theme)
}

fn quantity(n: u128, singular: &str, plural: &str) -> String {
    format!("{} {}", format_exact(n), if n == 1 { singular } else { plural })
}

fn contents_line(files: u128, folders: u128, audio: u128, theme: Theme) -> Line<'static> {
    let mut values = vec![
        bright(quantity(files, "file", "files"), theme),
        dim(", ", theme),
        bright(quantity(folders, "folder", "folders"), theme),
        dim(" · ", theme),
    ];
    if audio == 0 { values.push(dim("no audio files", theme)); }
    else { values.push(bright(quantity(audio, "audio file", "audio files"), theme)); }
    labeled("Contains", values, theme)
}

fn status_line(state: &FolderPropertiesState, view: Option<&DirectoryView>, theme: Theme) -> Line<'static> {
    let Some(view) = view else { return Line::from(dim("           checking cached measurement…", theme)); };
    if let Some(cached) = &view.cached {
        let mut spans = vec![dim(format!("{:<LABEL_WIDTH$}cached {}", "", measured_at(cached)), theme)];
        let end_label = if view.running && state.cancelled { Some("cancelling…") }
            else if view.running { Some("recalculating…") }
            else if state.cancelled || matches!(view.last_end.as_ref(), Some(ScanEnd::Cancelled(_))) { Some("rescan cancelled") }
            else { match view.last_end.as_ref() {
                Some(ScanEnd::Failed { .. }) => Some("rescan failed"),
                Some(ScanEnd::Deferred(_)) => Some("scan deferred"),
                Some(ScanEnd::Changed { .. }) => Some("changed during scan"),
                Some(ScanEnd::Complete { saved: false, .. }) => Some("not saved"),
                _ => match cached.freshness {
                    Freshness::AsObserved => None,
                    Freshness::Unverified => Some("freshness unverified"),
                    Freshness::KnownInvalid => Some("known changes since scan"),
                },
            }};
        if let Some(label) = end_label {
            spans.push(dim(" · ", theme));
            spans.push(Span::styled(label.to_string(), style(theme.amber)));
        }
        return Line::from(spans);
    }
    if view.running {
        if state.cancelled {
            return Line::from(dim(format!("{:<LABEL_WIDTH$}cancelling…", ""), theme));
        }
        if let Some(p) = &view.progress {
            return Line::from(dim(format!("{:<LABEL_WIDTH$}{} files examined · partial", "", format_exact(p.files_examined)), theme));
        }
        return Line::from(dim(format!("{:<LABEL_WIDTH$}scanning…", ""), theme));
    }
    let message = match view.last_end.as_ref() {
        Some(ScanEnd::Cancelled(_)) => "scan cancelled",
        Some(ScanEnd::Deferred(_)) => "scan deferred",
        Some(ScanEnd::Failed { .. }) => "scan failed",
        Some(ScanEnd::Changed { .. }) => "changed during scan",
        _ if view.error.is_some() => "measurement unavailable",
        _ => "not yet calculated",
    };
    Line::from(dim(format!("{:<LABEL_WIDTH$}{message}", ""), theme))
}

fn display_location(path: &std::path::Path, skip: usize, max_width: usize) -> String {
    let full = path.to_string_lossy();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let display = home.and_then(|home| {
        path.strip_prefix(&home).ok().map(|tail| {
            if tail.as_os_str().is_empty() { "~".to_string() }
            else { format!("~/{}", tail.display()) }
        })
    }).unwrap_or_else(|| full.to_string());
    super::display_width::truncate_right(&display.chars().skip(skip).collect::<String>(), max_width)
}

fn content_lines(state: &FolderPropertiesState, view: Option<&DirectoryView>, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let name = state.path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| state.path.display().to_string());
    let name = super::display_width::truncate_right(&name, width);
    let subtype = match &state.subject {
        PropertiesSubject::Folder => "Folder · symlinks not followed",
        PropertiesSubject::File { is_symlink: true, .. } => "Symlink · target not followed",
        PropertiesSubject::File { .. } => "File · filesystem metadata",
    };
    let rule = || Line::from(Span::styled("─".repeat(width), style(theme.border_dim)));
    let mut lines = vec![
        Line::from(Span::styled(name, style(theme.text_bright).add_modifier(Modifier::BOLD))),
        Line::from(dim(subtype, theme)),
        rule(),
        text_row("Location", display_location(state.path.parent().unwrap_or(&state.path),
            state.horizontal_scroll, width.saturating_sub(LABEL_WIDTH)), theme),
        rule(),
    ];
    match &state.subject {
        PropertiesSubject::Folder => {
            if let Some(cached) = view.and_then(|v| v.cached.as_ref()) {
                let bytes = cached.measurement.counts.bytes;
                lines.push(labeled("Size", vec![bright(format_bytes(bytes), theme),
                    dim(format!(" ({} bytes)", format_exact(bytes)), theme)], theme));
            } else if let Some(progress) = view.filter(|v| v.running).and_then(|v| v.progress.as_ref()) {
                lines.push(labeled("Size", vec![bright(format_bytes(progress.bytes_examined), theme),
                    dim(" · partial", theme)], theme));
            } else { lines.push(text_row("Size", "unknown", theme)); }
            lines.push(status_line(state, view, theme));
            if let Some(cached) = view.and_then(|v| v.cached.as_ref()) {
                let c = &cached.measurement.counts;
                lines.push(contents_line(c.files, c.folders, c.audio, theme));
            } else if let Some(progress) = view.filter(|v| v.running).and_then(|v| v.progress.as_ref()) {
                lines.push(labeled("Contains", vec![bright(quantity(progress.files_examined, "file", "files"), theme),
                    dim(", ", theme), bright(quantity(progress.folders_examined, "folder", "folders"), theme),
                    dim(" · partial", theme)], theme));
            } else if let Some(local) = view.and_then(|v| v.local.as_ref()) {
                lines.push(labeled("Contains", vec![bright(quantity(local.files, "file", "files"), theme),
                    dim(", ", theme), bright(quantity(local.folders, "folder", "folders"), theme),
                    dim(" · one level", theme)], theme));
            } else { lines.push(text_row("Contains", "unknown", theme)); }
            lines.push(rule());
            let cached = view.and_then(|v| v.cached.as_ref());
            lines.push(labeled("Created", vec![cached.and_then(|c| timestamp(c.measurement.created))
                .map(|s| bright(s, theme)).unwrap_or_else(|| dim("– not recorded by this filesystem", theme))], theme));
            lines.push(labeled("Modified", vec![cached.and_then(|c| timestamp(c.measurement.modified))
                .map(|s| bright(s, theme)).unwrap_or_else(|| dim("– not yet measured", theme))], theme));
        }
        PropertiesSubject::File { size, modified, is_symlink } => {
            let size_text = if *is_symlink { "unknown · symlink not followed".to_string() }
                else { format!("{} ({} bytes)", format_bytes(*size as u128), format_exact(*size as u128)) };
            lines.push(text_row("Size", size_text, theme));
            lines.push(Line::default());
            lines.push(Line::default());
            lines.push(rule());
            lines.push(labeled("Created", vec![dim("– not recorded by this filesystem", theme)], theme));
            // Browse may have followed a symlink for classification. Its mtime
            // is not proof of the link's own mtime, even if the caller supplied
            // one. Do not show target metadata under a no-follow heading.
            let link_safe_modified = if *is_symlink { None } else { *modified };
            lines.push(labeled("Modified", vec![link_safe_modified.and_then(|t| timestamp(Some(t)))
                .map(|s| bright(s, theme)).unwrap_or_else(|| dim("– unavailable in listing", theme))], theme));
        }
    }
    lines
}

fn pill(label: &str, focused: bool, muted: bool, theme: Theme) -> Line<'static> {
    let bg = if muted { theme.pill_dim_bg } else { theme.purple };
    let fg = if muted { theme.text_bright } else { theme.pill_active_fg };
    let base = Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
    let label = label.trim();
    let mut chars = label.chars();
    let first = chars.next().unwrap_or(' ');
    let rest = chars.collect::<String>();
    let shortcut = if muted { base.add_modifier(Modifier::UNDERLINED) } else { base };
    let focused_style = if focused && muted { base.add_modifier(Modifier::ITALIC) } else { base };
    let padding = if muted { " " } else { "  " };
    Line::from(vec![
        Span::styled(padding, base),
        Span::styled(first.to_string(), shortcut),
        Span::styled(rest, focused_style),
        Span::styled(padding, base),
    ])
}

fn render_button(f: &mut Frame, map: &mut ButtonRenderMap, area: Rect, label: &str,
    id: TuiButton, focused: bool, muted: bool, theme: Theme) {
    if area.width == 0 || area.height == 0 { return; }
    f.render_widget(Paragraph::new(pill(label, focused, muted, theme)), area);
    map.record_button(id, area);
}

pub fn draw(f: &mut Frame, state: &FolderPropertiesState, service: &AccountingService,
    map: &mut ButtonRenderMap, theme: Theme) {
    let Some(area) = popup_rect(f.size(), state.maximized) else { return; };
    let view = if let Some(id) = state.request_id {
        service.view(&state.path).filter(|v| state.cancelled || v.subscription == Some(id))
    } else { None };
    let cancelling = state.is_folder() && state.cancelled && view.is_some_and(|v| v.running);
    let running = state.is_folder() && !state.cancelled && view.is_some_and(|v| v.running);
    let name = state.path.file_name().map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| state.path.display().to_string());
    let indicator = if state.maximized { "▾" } else { "▸" };
    let title = format!("{indicator} {} Properties", super::display_width::truncate_right(
        &name, area.width.saturating_sub(19) as usize));
    let block = super::draw::solid_title_block(area, title, theme.purple, theme)
        .style(Style::default().bg(theme.surface));
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    map.record_button(TuiButton::PropertiesTitle, Rect::new(
        area.x.saturating_add(1), area.y, area.width.saturating_sub(2), 1));

    let body_x = area.x.saturating_add(2);
    let body_w = area.width.saturating_sub(4);
    let body_y = area.y.saturating_add(1);
    let footer_y = area.bottom().saturating_sub(3);
    let body_h = footer_y.saturating_sub(body_y).saturating_sub(1);
    if body_w == 0 || body_h == 0 { return; }
    let lines = content_lines(state, view, theme, body_w as usize);
    let scroll = state.scroll.min(lines.len().saturating_sub(body_h as usize));
    f.render_widget(Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        Rect::new(body_x, body_y, body_w, body_h));

    // Recalculate/Cancel shares the Size row, away from its value; the mouse
    // hit region is *exactly* the painted pill, even in narrow terminals.
    if state.is_folder() && scroll <= 5 && 5 - scroll < body_h as usize {
        let label = if cancelling { "Cancelling…" } else if running { "Cancel" } else { "Recalculate" };
        let width = label.chars().count() as u16 + 2;
        if body_w >= width + 14 {
            let rect = Rect::new(body_x + body_w - width, body_y + 5 - scroll as u16, width, 1);
            if cancelling {
                // Cancellation is cooperative. Do not offer a no-op
                // Recalculate until the current worker has acknowledged it.
                f.render_widget(Paragraph::new(pill(label, false, true, theme)), rect);
            } else {
                let id = if running { TuiButton::PropertiesCancel } else { TuiButton::PropertiesRecalculate };
                render_button(f, map, rect, label, id, state.focus == PropertiesFocus::Recalculate, true, theme);
            }
        }
    }
    let edit = "Edit Metadata...";
    let edit_width = edit.chars().count() as u16 + 2;
    let ok_width = 6u16;
    if body_w >= edit_width + ok_width + 2 {
        render_button(f, map, Rect::new(body_x, footer_y, edit_width, 1), edit,
            TuiButton::PropertiesEditMetadata, state.focus == PropertiesFocus::EditMetadata, true, theme);
        render_button(f, map, Rect::new(body_x + body_w - ok_width, footer_y, ok_width, 1), "OK",
            TuiButton::PropertiesOk, state.focus == PropertiesFocus::Ok, false, theme);
    } else if body_w >= ok_width {
        render_button(f, map, Rect::new(body_x + body_w - ok_width, footer_y, ok_width, 1), "OK",
            TuiButton::PropertiesOk, state.focus == PropertiesFocus::Ok, false, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory_accounting::{CachedMeasurement, CompleteMeasurement, Counts, Freshness, ScanProgress};
    use crate::directory_accounting::model::Observation;
    use crate::tui::directory_properties::{PropertiesFocus, PropertiesSubject};
    use ratatui::{backend::TestBackend, Terminal};
    use std::{path::PathBuf, time::{Duration, SystemTime}};

    fn theme() -> Theme {
        crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug())
    }
    fn folder() -> FolderPropertiesState {
        FolderPropertiesState {
            path: PathBuf::from("/library/zeppelin/Led Zeppelin - Led Zeppelin DVD/disc 01"),
            subject: PropertiesSubject::Folder, request_id: Some(19), scroll: 0,
            horizontal_scroll: 0, cancelled: false, maximized: false, focus: PropertiesFocus::Ok,
        }
    }
    fn completed(state: &FolderPropertiesState) -> AccountingService {
        let mut service = AccountingService::new(None);
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_798_000_000);
        service.views.insert(state.path.clone(), DirectoryView {
            cached: Some(CachedMeasurement {
                measurement: CompleteMeasurement {
                    path: state.path.clone(),
                    counts: Counts { files: 31, folders: 1, audio: 0, bytes: 8_208_015_360, ..Counts::default() },
                    started: time, finished: time, created: None, modified: Some(time),
                    generation: 1, observation: Observation::AsObserved,
                },
                freshness: Freshness::AsObserved,
            }),
            subscription: Some(19),
            ..DirectoryView::default()
        });
        service
    }
    fn render(state: &FolderPropertiesState, service: &AccountingService, width: u16, height: u16)
        -> (String, ButtonRenderMap, Terminal<TestBackend>) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut map = ButtonRenderMap::new();
        terminal.draw(|f| draw(f, state, service, &mut map, theme())).unwrap();
        let rendered = (0..height).map(|y| {
            let mut line = String::new();
            for x in 0..width { line.push_str(terminal.backend().buffer().get(x, y).symbol()); }
            line
        }).collect::<Vec<_>>().join("\n");
        (rendered, map, terminal)
    }

    #[test]
    fn reference_properties_layout_orders_rows_and_draws_real_buttons() {
        let state = folder();
        let service = completed(&state);
        let (text, map, terminal) = render(&state, &service, 100, 26);
        for needle in [
            "disc 01 Properties", "disc 01", "Folder · symlinks not followed",
            "Location", "Size", "7.64 GiB", "8,208,015,360 bytes",
            "Contains", "31 files", "1 folder", "no audio files",
            "Created", "not recorded", "Modified", "Recalculate", "Edit Metadata...", "OK",
        ] {
            assert!(text.contains(needle), "missing {needle:?}: {text}");
        }
        let order = ["Folder ·", "Location", "Size", "Contains", "Created", "Modified"];
        for pair in order.windows(2) {
            assert!(text.find(pair[0]) < text.find(pair[1]), "wrong row order: {text}");
        }
        assert!(!text.contains("Exact bytes:"), "old scrolling debug layout must be gone");
        let rect = popup_rect(Rect::new(0, 0, 100, 26), false).unwrap();
        assert_eq!((rect.width, rect.height), (95, 21));
        let edit = map.button_rect(TuiButton::PropertiesEditMetadata).unwrap();
        let ok = map.button_rect(TuiButton::PropertiesOk).unwrap();
        let recalc = map.button_rect(TuiButton::PropertiesRecalculate).unwrap();
        assert_eq!(edit.y, ok.y);
        assert!(edit.right() < ok.x, "footer actions must be separate clickable targets");
        assert_eq!(recalc.y, rect.y + 6, "recalculate must sit beside Size");
        for (button, bounds) in map.recorded_buttons() {
            assert_eq!(map.find_button_at(bounds.x, bounds.y), Some(*button));
            assert!(bounds.x >= rect.x && bounds.right() <= rect.right());
        }
        // The frame uses the active theme for the frame and action button.
        let border = terminal.backend().buffer().get(rect.x, rect.y + 1);
        assert_eq!(border.fg, theme().purple);
        let ok_cell = terminal.backend().buffer().get(ok.x + 1, ok.y);
        assert_eq!(ok_cell.bg, theme().purple);
    }

    #[test]
    fn properties_geometry_scales_with_terminal_and_maximizes_without_fullscreen() {
        let normal = popup_rect(Rect::new(0, 0, 140, 38), false).unwrap();
        let large = popup_rect(Rect::new(0, 0, 140, 38), true).unwrap();
        assert_eq!((normal.width, normal.height), (98, 23));
        assert_eq!((large.width, large.height), (138, 33));
        assert!(large.width > normal.width && large.height > normal.height);
        assert_ne!(large, Rect::new(0, 0, 140, 38));
        for (w, h) in [(100, 26), (46, 12), (24, 9)] {
            let base = popup_rect(Rect::new(0, 0, w, h), false).unwrap();
            let max = popup_rect(Rect::new(0, 0, w, h), true).unwrap();
            for area in [base, max] {
                assert!(area.width <= w - 2 && area.height <= h - 2);
                assert!(area.right() <= w && area.bottom() <= h);
            }
            assert!(max.width >= base.width && max.height >= base.height);
        }
        assert!(popup_rect(Rect::new(0, 0, 23, 9), false).is_none());

        let mut state = folder();
        state.maximized = true;
        let (text, map, terminal) = render(&state, &completed(&state), 140, 38);
        assert!(text.contains("▾ disc 01 Properties"));
        for button in [TuiButton::PropertiesTitle, TuiButton::PropertiesOk,
            TuiButton::PropertiesEditMetadata, TuiButton::PropertiesRecalculate] {
            let hit = map.button_rect(button).expect("maximized Properties hitbox");
            assert_eq!(map.find_button_at(hit.x, hit.y), Some(button));
            assert!(hit.x >= large.x && hit.right() <= large.right());
            assert!(hit.y >= large.y && hit.bottom() <= large.bottom());
        }
        let bar = terminal.backend().buffer().get(large.x + 1, large.y);
        assert_eq!(bar.bg, theme().purple, "the title bar must be solid");
    }

    /// Location is the containing directory, not the path in the title.
    fn location_row(state: &FolderPropertiesState) -> String {
        let (text, _, _) = render(state, &AccountingService::new(None), 100, 26);
        text.lines().find(|line| line.contains("Location"))
            .expect("Properties Location row").to_string()
    }

    #[test]
    fn location_is_the_parent_of_a_folder_or_file_with_root_fallback() {
        let mut state = folder();
        let parent = state.path.parent().unwrap();
        let row = location_row(&state);
        assert!(row.contains(&display_location(parent, 0, 61)), "{row}");
        assert!(!row.contains("disc 01"), "the folder name belongs in the heading: {row}");

        state.path = PathBuf::from("/library/zeppelin/track.flac");
        state.subject = PropertiesSubject::File { size: 42, modified: None, is_symlink: false };
        let row = location_row(&state);
        assert!(row.contains(&display_location(state.path.parent().unwrap(), 0, 61)), "{row}");
        assert!(!row.contains("track.flac"), "the file name belongs in the heading: {row}");

        // Path::parent() returns None at filesystem roots: Location must still
        // have a usable value, rather than being empty or panicking.
        state.path = PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" });
        assert!(state.path.parent().is_none());
        let row = location_row(&state);
        assert!(row.contains(&display_location(&state.path, 0, 61)), "{row}");
    }

    #[test]
    fn location_retains_unicode_and_horizontal_scroll() {
        let mut state = folder();
        state.path = PathBuf::from("/library/Música/日本語/track.flac");
        state.subject = PropertiesSubject::File { size: 1, modified: None, is_symlink: false };
        state.horizontal_scroll = 3;
        // Inspect the spans rather than the TestBackend buffer: wide glyphs
        // reserve a second terminal cell which need not repeat the glyph.
        let lines = content_lines(&state, None, theme(), 72);
        let row = lines[3].spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert!(row.contains(&display_location(state.path.parent().unwrap(), 3, 61)), "{row}");
    }

    #[test]
    fn cached_cancelled_scan_is_never_displayed_as_a_partial_total() {
        let mut state = folder();
        state.cancelled = true;
        let mut service = completed(&state);
        service.views.get_mut(&state.path).unwrap().last_end =
            Some(ScanEnd::Cancelled(ScanProgress { files_examined: 2, ..ScanProgress::default() }));
        let (text, map, _) = render(&state, &service, 100, 26);
        assert!(text.contains("cached "));
        assert!(text.contains("rescan cancelled"));
        assert!(text.contains("7.64 GiB"));
        assert!(!text.contains("partial"));
        assert!(map.button_rect(TuiButton::PropertiesRecalculate).is_some());
    }

    #[test]
    fn running_scan_exposes_cancel_and_marks_numbers_partial() {
        let state = folder();
        let mut service = AccountingService::new(None);
        service.views.insert(state.path.clone(), DirectoryView {
            subscription: Some(19), running: true,
            progress: Some(ScanProgress {
                bytes_examined: 50_000, files_examined: 5, folders_examined: 2,
                ..ScanProgress::default()
            }),
            ..DirectoryView::default()
        });
        let (text, map, _) = render(&state, &service, 92, 26);
        assert!(text.contains("partial"), "running counts are not final: {text}");
        assert!(text.contains("Cancel"));
        assert!(map.button_rect(TuiButton::PropertiesCancel).is_some());
        assert!(map.button_rect(TuiButton::PropertiesRecalculate).is_none());
    }

    #[test]
    fn cancellation_in_progress_does_not_offer_an_ineffective_recalculate() {
        let mut state = folder();
        state.cancelled = true;
        let mut service = AccountingService::new(None);
        service.views.insert(state.path.clone(), DirectoryView {
            subscription: Some(19), running: true,
            progress: Some(ScanProgress { files_examined: 5, ..ScanProgress::default() }),
            ..DirectoryView::default()
        });
        let (text, map, _) = render(&state, &service, 92, 26);
        assert!(text.contains("cancelling"));
        assert!(map.button_rect(TuiButton::PropertiesRecalculate).is_none());
        assert!(map.button_rect(TuiButton::PropertiesCancel).is_none());
    }

    #[test]
    fn cold_cache_and_compact_terminal_preserve_exit_and_scroll() {
        let state = folder();
        let service = AccountingService::new(None);
        let (text, map, _) = render(&state, &service, 46, 12);
        assert!(text.contains("disc 01"));
        assert!(map.button_rect(TuiButton::PropertiesOk).is_some());
        assert!(map.button_rect(TuiButton::PropertiesEditMetadata).is_some());
        let mut scrolled = state;
        scrolled.scroll = 4;
        let (text, map, _) = render(&scrolled, &service, 46, 12);
        assert!(text.contains("unknown") || text.contains("checking cached"));
        assert!(text.contains("Contains") || text.contains("Created") || text.contains("Modified"));
        assert!(map.button_rect(TuiButton::PropertiesOk).is_some());
    }

    #[test]
    fn symlink_presentation_never_displays_a_target_timestamp() {
        let mut state = folder();
        let target_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Defense at the presentation boundary: do not trust metadata from
        // another caller that obtained a followed target's timestamp.
        state.subject = PropertiesSubject::File {
            size: 123_456, modified: Some(target_time), is_symlink: true,
        };
        state.request_id = None;
        let (text, map, _) = render(&state, &AccountingService::new(None), 100, 26);
        assert!(text.contains("Symlink · target not followed"), "{text}");
        assert!(text.contains("unknown · symlink not followed"), "{text}");
        assert!(text.contains("unavailable in listing"), "{text}");
        assert!(!text.contains(&timestamp(Some(target_time)).unwrap()));
        assert!(map.button_rect(TuiButton::PropertiesRecalculate).is_none());
    }

    #[test]
    fn file_properties_use_listing_facts_and_never_offer_directory_scan() {
        let mut state = folder();
        state.subject = PropertiesSubject::File { size: 123_456, modified: None, is_symlink: false };
        state.request_id = None;
        let (text, map, _) = render(&state, &AccountingService::new(None), 80, 24);
        assert!(text.contains("File · filesystem metadata"));
        assert!(text.contains("123,456 bytes"));
        assert!(text.contains("Modified"));
        assert!(map.button_rect(TuiButton::PropertiesRecalculate).is_none());
        assert!(map.button_rect(TuiButton::PropertiesEditMetadata).is_some());
    }
}

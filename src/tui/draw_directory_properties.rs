//! Adaptive read-only Folder Properties modal.
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph}, Frame,
};
use super::directory_properties::FolderPropertiesState;
use crate::directory_accounting::{AccountingService, Freshness, ScanEnd, format_bytes, format_exact};

fn date(time: Option<std::time::SystemTime>) -> String {
    time.map(|t| {
        let dt: chrono::DateTime<chrono::Local> = t.into();
        dt.format("%Y-%m-%d %H:%M:%S %:z").to_string()
    }).unwrap_or_else(|| "Unavailable on this filesystem".into())
}

pub fn draw(f: &mut Frame, state: &FolderPropertiesState, service: &AccountingService, theme: super::theme::Theme) {
    let parent = f.size();
    if parent.width < 8 || parent.height < 3 { return; }
    let width = parent.width.min(76).saturating_sub(if parent.width > 80 { 0 } else { 2 }).max(8);
    let height = parent.height.min(22).saturating_sub(if parent.height > 24 { 0 } else { 1 }).max(3);
    let area = Rect::new(parent.x + (parent.width - width) / 2,
        parent.y + (parent.height - height) / 2, width, height);
    // A superseded modal must not consume another subscription's results.
    let view = service.view(&state.path).filter(|v| state.cancelled || v.subscription == Some(state.request_id));
    let running = !state.cancelled && view.is_some_and(|v| v.running);
    let title = " Folder Properties ";
    let block = Block::default().borders(Borders::ALL).title(title)
        .border_style(Style::default().fg(theme.amber))
        .title_alignment(Alignment::Center);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 { return; }
    let mut rows: Vec<String> = Vec::new();
    let name = state.path.file_name().map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| state.path.display().to_string());
    rows.push(format!("Name: {name}"));
    rows.push(format!("Location: {}", state.path.display()));
    rows.push("Type: Directory (links not followed)".into());
    if let Some(view) = view {
        if state.cancelled {
            rows.push("Status: Scan cancellation requested".into());
        }
        if running {
            rows.push("Status: Calculating...".into());
            if let Some(p) = &view.progress {
                rows.push(format!("Examined: {} files, {} folders", format_exact(p.files_examined), format_exact(p.folders_examined)));
                rows.push(format!("Scanned so far: {} (NOT a total)", format_bytes(p.bytes_examined)));
            }
        }
        if let Some(cached) = &view.cached {
            let c = &cached.measurement.counts;
            let size_label = match cached.freshness {
                Freshness::AsObserved => "Size",
                Freshness::Unverified => "Cached size",
                Freshness::KnownInvalid => "Historical size",
            };
            rows.push(format!("{size_label}: {}", format_bytes(c.bytes)));
            rows.push(format!("Exact bytes: {}", format_exact(c.bytes)));
            rows.push(format!("Contains: {} files, {} folders", format_exact(c.files), format_exact(c.folders)));
            rows.push(format!("Audio files: {}", format_exact(c.audio)));
            rows.push(format!("Excluded links: {}", format_exact(c.excluded_links)));
            rows.push(format!("Created: {}", date(cached.measurement.created)));
            rows.push(format!("Modified: {}", date(cached.measurement.modified)));
            rows.push(format!("Measured: {}", date(Some(cached.measurement.finished))));
            let status = match cached.freshness {
                Freshness::AsObserved => "Completed (as observed; not a filesystem snapshot)",
                Freshness::Unverified => "Cached (freshness unverified)",
                Freshness::KnownInvalid => "Cached (known change; historical measurement)",
            };
            rows.push(format!("Status: {status}"));
        }
        if running {
            // Running status is deliberately first, including on recalculation
            // of a cached (potentially stale) historical measurement.
        } else if let Some(end) = &view.last_end {
            match end {
                ScanEnd::Complete { saved: false, persistence_error, .. } => rows.push(format!("Persistence: not saved ({})", persistence_error.as_deref().unwrap_or("unavailable"))),
                ScanEnd::Cancelled(p) => rows.push(format!("Status: Cancelled after {} files", format_exact(p.files_examined))),
                ScanEnd::Deferred(_) => rows.push("Status: Background scan deferred by metadata latency".into()),
                ScanEnd::Failed { reason, .. } => rows.push(format!("Status: Failed: {reason}")),
                ScanEnd::Changed { reason, .. } => rows.push(format!("Status: Changed during scan: {reason}")),
                _ => {},
            }
        } else if let Some(error) = &view.error {
            rows.push(format!("Status: Unavailable: {error}"));
        }
        if view.cached.is_none() && !running && !state.cancelled && view.last_end.is_none() {
            rows.push("Status: Not calculated".into());
        }
    } else {
        rows.push("Status: Checking cached measurement...".into());
    }
    // Long paths remain accessible by scrolling in a normal-sized terminal.
    let footer = if running { "Esc Close  C Cancel  R Recalculate  Arrows Scroll" }
        else { "Esc Close  R Recalculate  Arrows Scroll  Home Reset" };
    let body_rows = inner.height.saturating_sub(1) as usize;
    if body_rows > 0 {
        let max_scroll = rows.len().saturating_sub(body_rows);
        let start = state.scroll.min(max_scroll);
        let lines = rows.iter().skip(start).take(body_rows).map(|row| {
            Line::from(Span::styled(row.clone(), Style::default().fg(theme.text)))
        }).collect::<Vec<_>>();
        let body = Rect::new(inner.x, inner.y, inner.width, body_rows as u16);
        // Horizontal scroll applies to the full untruncated location line,
        // including paths too long to fit in the current terminal width.
        f.render_widget(Paragraph::new(lines)
            .scroll((0, state.horizontal_scroll.min(u16::MAX as usize) as u16)), body);
    }
    let footer_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    f.render_widget(Paragraph::new(footer).style(Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)), footer_area);
}

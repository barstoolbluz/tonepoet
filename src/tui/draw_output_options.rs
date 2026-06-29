//! Output options pane: dest path, folder/filename templates, merge mode, est. size (cyan border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatState, OutputOptionsField, OutputOptionsState};
use super::pill::render_pill_spans;
use super::probe::SourceInfo;
use crate::convert::formats::AudioFormat;

/// Draw the output options pane with cyan border
pub fn draw_output_options_pane(
    f: &mut Frame,
    area: Rect,
    opts: &OutputOptionsState,
    source_info: Option<&SourceInfo>,
    total_source_size: u64,
    format: &FormatState,
    focused: bool,
    maximized: bool,
    theme: super::theme::Theme,
) {
    if area.height < 5 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme.cyan
    } else {
        theme.text_dim
    };
    let w = area.width as usize;

    // Top border
    let top_line = output_options_title_line(border_color, w, maximized, theme);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let is_dest_focused = focused && opts.field_focus == OutputOptionsField::DestPath;
    let is_folder_focused = focused && opts.field_focus == OutputOptionsField::FolderTemplate;
    let is_file_focused = focused && opts.field_focus == OutputOptionsField::FilenameTemplate;
    let is_merge_focused = focused && opts.field_focus == OutputOptionsField::MergeMode;

    // Destination path
    let dest_display = opts
        .dest_path
        .as_ref()
        .map(shorten_path_for_display)
        .unwrap_or_else(|| "—".to_string());

    let dest_label_style = if is_dest_focused {
        theme.bright()
    } else {
        theme.muted()
    };
    let dest_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   dest        ", dest_label_style),
            Span::styled(dest_display, theme.bright()),
        ], theme);

    // Folder template
    let folder_label_style = if is_folder_focused {
        theme.bright()
    } else {
        theme.muted()
    };
    let load_pill = Span::styled(
        " load ",
        Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.amber)
            .add_modifier(Modifier::BOLD),
    );
    let build_pill = Span::styled(
        " custom ",
        Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.blue)
            .add_modifier(Modifier::BOLD),
    );
    let pill_width = 6 + 1 + 8; // " load " + gap + " custom "
    let folder_tmpl_max = w.saturating_sub(15 + pill_width + 4); // label + pills + borders + gap
    let folder_display = truncate_to(&opts.folder_template, folder_tmpl_max);
    let folder_row = template_row_with_pills(
        border_color,
        w,
        "   folder      ",
        &folder_display,
        folder_label_style,
        load_pill.clone(),
        build_pill.clone(), theme);

    // Filename template
    let file_label_style = if is_file_focused {
        theme.bright()
    } else {
        theme.muted()
    };
    let file_display = truncate_to(&opts.filename_template, folder_tmpl_max);
    let file_row = template_row_with_pills(
        border_color,
        w,
        "   filename    ",
        &file_display,
        file_label_style,
        load_pill,
        build_pill, theme);

    // Merge mode pills
    let merge_row = pill_row(
        border_color,
        w,
        "merge      ",
        "",
        &render_pill_spans(&opts.merge, is_merge_focused, theme),
        is_merge_focused, theme);

    // Estimated size
    let est_display = estimate_output_size(source_info, total_source_size, format)
        .unwrap_or_else(|| "—".to_string());
    let est_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   est. size   ", theme.muted()),
            Span::styled(est_display, theme.accent()),
        ], theme);

    let mut lines = vec![top_line];
    lines.push(dest_row);
    lines.push(folder_row);
    lines.push(file_row);
    lines.push(merge_row);
    lines.push(est_row);
    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}


/// Draw the collapsed output-options title bar.
pub fn draw_output_options_title_bar(f: &mut Frame, area: Rect, focused: bool, theme: super::theme::Theme) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused { theme.cyan } else { theme.text_dim };
    f.render_widget(
        Paragraph::new(vec![output_options_title_line(
            border_color,
            area.width as usize,
            false, theme)]),
        area,
    );
}

fn output_options_title_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    maximized: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let title = " output options ";
    let indicator = if maximized { "▾" } else { "▸" };
    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let left_spans = vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(format!(" {indicator}{title}"), bar_style),
    ];
    let right_spans = vec![
        Span::styled("a", Style::default().fg(theme.text_muted).bg(border_color)),
        Span::styled("dvanced ", bar_style),
        Span::styled("┐", theme.border(border_color)),
    ];
    let fixed_width = Line::from(left_spans.clone()).width()
        + Line::from(right_spans.clone()).width();
    let fill_count = width.saturating_sub(fixed_width);
    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(fill_count),
        bar_style,
    ));
    spans.extend(right_spans);
    Line::from(spans)
}

/// Build a bordered line with a label, pill spans, and optional suffix
fn pill_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    suffix: &'a str,
    pills: &[Span<'a>],
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused {
        theme.bright()
    } else {
        theme.muted()
    };

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
    ];
    spans.extend_from_slice(pills);

    if !suffix.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(suffix, theme.muted()));
    }

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));

    Line::from(spans)
}

/// Create a line with │ content ... │ border
fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
    theme: super::theme::Theme,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme.border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

/// Build a template row with trailing [load] [build] pills.
fn template_row_with_pills<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    template_display: &str,
    label_style: Style,
    load_pill: Span<'a>,
    build_pill: Span<'a>,
    theme: super::theme::Theme,
) -> Line<'a> {
    let load_w = load_pill.width();
    let build_w = build_pill.width();

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(label, label_style),
        Span::styled(template_display.to_string(), theme.text_style()),
    ];

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let pills_total = load_w + 1 + build_w; // pills + gap
    let padding = width.saturating_sub(content_width + pills_total + 1); // +1 for right border
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(load_pill);
    spans.push(Span::raw(" "));
    spans.push(build_pill);
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

/// Estimate the output file size based on source audio properties and target format.
fn estimate_output_size(
    info: Option<&SourceInfo>,
    total_source_size: u64,
    format: &FormatState,
) -> Option<String> {
    let info = info?;
    if info.duration_secs <= 0.0 || total_source_size == 0 {
        return None;
    }

    // For batch mode, scale cursor-file estimates to the full batch.
    let batch_scale = if info.file_size > 0 {
        total_source_size as f64 / info.file_size as f64
    } else {
        1.0
    };

    let target_format = format.format.selected_value();
    let bytes = match target_format {
        // DSD: 1-bit per sample at the DSD rate, scaled to batch
        AudioFormat::Dsf | AudioFormat::Dff => {
            let dsd_rate = *format.sample_rate.selected_value() as f64;
            let channels = info.channels as f64;
            (info.duration_secs * dsd_rate * channels / 8.0 * batch_scale) as u64
        }
        // Lossy: bitrate × duration / 8, scaled to batch
        AudioFormat::Mp3 => (320_000.0 * info.duration_secs / 8.0 * batch_scale) as u64,
        AudioFormat::Aac => (256_000.0 * info.duration_secs / 8.0 * batch_scale) as u64,
        AudioFormat::Opus => (128_000.0 * info.duration_secs / 8.0 * batch_scale) as u64,
        // Lossless: scale proportionally from source file size when possible,
        // otherwise fall back to raw PCM formula with compression estimate.
        _ => {
            let target_rate = *format.sample_rate.selected_value() as f64;
            let target_bits = format.bit_depth.selected_value().bits() as f64;
            let channels = info.channels as f64;
            let target_raw = info.duration_secs * target_rate * target_bits * channels / 8.0;

            let target_is_compressed = matches!(
                target_format,
                AudioFormat::Flac | AudioFormat::Alac | AudioFormat::WavPack
            );
            let is_container = info.format_name.starts_with("SACD");
            let is_uncompressed = info.codec.starts_with("pcm_");
            let can_scale = target_is_compressed
                && !is_container
                && !is_uncompressed
                && info.bit_depth.is_some()
                && info.sample_rate > 0;

            if can_scale {
                // Proportional scaling from total source size: preserves
                // actual compression ratio. Same settings → total output
                // size equals total source size.
                let source_rate = info.sample_rate as f64;
                let source_bits = info.bit_depth.unwrap() as f64;
                let scale = (target_rate * target_bits) / (source_rate * source_bits);
                (total_source_size as f64 * scale) as u64
            } else {
                // No source bit_depth or container format: use generic factor,
                // scaled to total batch.
                let factor = match target_format {
                    AudioFormat::Flac | AudioFormat::Alac | AudioFormat::WavPack => 0.6,
                    _ => 1.0,
                };
                (target_raw * factor * batch_scale) as u64
            }
        }
    };

    Some(format_size_estimate(bytes))
}

fn format_size_estimate(bytes: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    const KB: u64 = 1_024;
    if bytes >= GB {
        format!("~{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("~{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("~{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("~{} B", bytes)
    }
}

fn shorten_path_for_display(path: &std::path::PathBuf) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rest) = path.strip_prefix(home_path) {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest);
        }
    }
    path.display().to_string()
}

fn text_width(s: &str) -> usize {
    Line::from(s).width()
}

/// Truncate a string to at most `max_width` terminal cells, adding "..." if
/// truncated, without slicing the input at byte offsets.
fn truncate_to(s: &str, max_width: usize) -> String {
    if text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis = "...";
    let ellipsis_width = text_width(ellipsis);
    if max_width <= ellipsis_width {
        let mut out = String::new();
        for ch in s.chars() {
            let candidate = format!("{}{}", out, ch);
            if text_width(&candidate) > max_width {
                break;
            }
            out = candidate;
        }
        return out;
    }

    let mut out = String::new();
    for ch in s.chars() {
        let candidate = format!("{}{}", out, ch);
        if text_width(&candidate) + ellipsis_width > max_width {
            break;
        }
        out = candidate;
    }
    format!("{}{}", out, ellipsis)
}

//! Output options pane: dest path, folder/filename templates, merge mode, est. size (cyan border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{OutputOptionsField, OutputOptionsState};
use super::pill::render_pill_spans;
use super::theme;

/// Draw the output options pane with cyan border
pub fn draw_output_options_pane(
    f: &mut Frame,
    area: Rect,
    opts: &OutputOptionsState,
    focused: bool,
) {
    if area.height < 5 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme::CYAN
    } else {
        theme::TEXT_DIM
    };
    let w = area.width as usize;

    // Top border
    let title = " output options ";
    let adv_label = " advanced ";
    let dash_count = w.saturating_sub(2 + title.len() + adv_label.len() + 2);

    let top_line = Line::from(vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
        Span::styled("─".repeat(dash_count), theme::border(border_color)),
        Span::raw(" "),
        Span::styled("a", theme::muted()),
        Span::styled("dvanced", theme::border(border_color)),
        Span::styled(" ┐", theme::border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let is_dest_focused = focused && opts.field_focus == OutputOptionsField::DestPath;
    let is_folder_focused = focused && opts.field_focus == OutputOptionsField::FolderTemplate;
    let is_file_focused = focused && opts.field_focus == OutputOptionsField::FilenameTemplate;
    let is_merge_focused = focused && opts.field_focus == OutputOptionsField::MergeMode;

    // Destination path
    let dest_display = opts
        .dest_path
        .as_ref()
        .map(|p| {
            let s = p.display().to_string();
            if let Ok(home) = std::env::var("HOME") {
                if s.starts_with(&home) {
                    return format!("~{}", &s[home.len()..]);
                }
            }
            s
        })
        .unwrap_or_else(|| "—".to_string());

    let dest_label_style = if is_dest_focused {
        theme::bright()
    } else {
        theme::muted()
    };
    let dest_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   dest        ", dest_label_style),
            Span::styled(dest_display, theme::bright()),
        ],
    );

    // Folder template
    let folder_label_style = if is_folder_focused {
        theme::bright()
    } else {
        theme::muted()
    };
    let load_pill = Span::styled(
        " load ",
        Style::default()
            .fg(theme::PILL_ACTIVE_FG)
            .bg(theme::AMBER)
            .add_modifier(Modifier::BOLD),
    );
    let build_pill = Span::styled(
        " custom ",
        Style::default()
            .fg(theme::PILL_ACTIVE_FG)
            .bg(theme::BLUE)
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
        build_pill.clone(),
    );

    // Filename template
    let file_label_style = if is_file_focused {
        theme::bright()
    } else {
        theme::muted()
    };
    let file_display = truncate_to(&opts.filename_template, folder_tmpl_max);
    let file_row = template_row_with_pills(
        border_color,
        w,
        "   filename    ",
        &file_display,
        file_label_style,
        load_pill,
        build_pill,
    );

    // Merge mode pills
    let merge_row = pill_row(
        border_color,
        w,
        "merge      ",
        "",
        &render_pill_spans(&opts.merge, is_merge_focused),
        is_merge_focused,
    );

    // Estimated size
    let est_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   est. size   ", theme::muted()),
            Span::styled("—", theme::accent()),
        ],
    );

    let mut lines = vec![top_line];
    lines.push(dest_row);
    lines.push(folder_row);
    lines.push(file_row);
    lines.push(merge_row);
    lines.push(est_row);
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Build a bordered line with a label, pill spans, and optional suffix
fn pill_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    suffix: &'a str,
    pills: &[Span<'a>],
    focused: bool,
) -> Line<'a> {
    let label_style = if focused {
        theme::bright()
    } else {
        theme::muted()
    };

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
    ];
    spans.extend_from_slice(pills);

    if !suffix.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(suffix, theme::muted()));
    }

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));

    Line::from(spans)
}

/// Create a line with │ content ... │ border
fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme::border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
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
) -> Line<'a> {
    let load_w = load_pill.width();
    let build_w = build_pill.width();

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(label, label_style),
        Span::styled(template_display.to_string(), theme::text()),
    ];

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let pills_total = load_w + 1 + build_w; // pills + gap
    let padding = width.saturating_sub(content_width + pills_total + 2); // +2 for gap + border
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(load_pill);
    spans.push(Span::raw(" "));
    spans.push(build_pill);
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

/// Truncate a string to at most `max_chars` characters, adding "..." if truncated.
fn truncate_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars > 3 {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{}...", truncated)
    } else {
        s.chars().take(max_chars).collect()
    }
}

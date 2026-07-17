//! Output options pane: dest path, folder/filename templates, merge mode, est. size (cyan border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatState, OutputOptionsField, OutputOptionsState};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::inline_edit::{inline_cursor_col, render_inline_value};
use super::pill::{render_pill_spans, PillState};
use super::probe::SourceInfo;
use crate::convert::formats::AudioFormat;

/// Row offsets, relative to the output-options pane top, for the below-the-fold
/// conversion option pills. These are shared with mouse hit registration so the
/// rendered rows and clickable rows cannot drift independently.
pub const OUTPUT_OPTIONS_FORCE_ENCODE_ROW: u16 = 13;
pub const OUTPUT_OPTIONS_DISC_SUBFOLDERS_ROW: u16 = 14;
pub const OUTPUT_OPTIONS_WRITE_LOG_ROW: u16 = 15;
pub const OUTPUT_OPTIONS_ACTIONS_ROW: u16 = 18;

const OUTPUT_OPTIONS_TEMPLATE_LOAD_WIDTH: u16 = 6;
const OUTPUT_OPTIONS_TEMPLATE_BUILD_WIDTH: u16 = 8;
const OUTPUT_OPTIONS_TEMPLATE_GAP: u16 = 1;

/// Register Output Options mouse targets from the same row-offset constants used
/// by `draw_output_options_pane`. This is intentionally separate from drawing so
/// the app's standard second pass can populate `ButtonRenderMap` after layout is
/// known, without event-time coordinate reconstruction.
pub fn register_output_options_mouse_targets(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    opts: &OutputOptionsState,
    maximized: bool,
    show_actions: bool,
) {
    if area.height < 5 || area.width < 30 {
        return;
    }

    let content_x = area.x.saturating_add(1);
    let content_width = area.width.saturating_sub(2);
    if content_width == 0 {
        return;
    }
    let row_rect = |row: u16| Rect::new(content_x, area.y.saturating_add(row), content_width, 1);
    let row_visible = |row: u16| row < area.height.saturating_sub(1);

    if row_visible(1) {
        buttons.record_button(TuiButton::DestPathField, row_rect(1));
    }
    if row_visible(2) {
        buttons.record_button(TuiButton::FolderTemplateField, row_rect(2));
        register_output_options_template_buttons(buttons, area, 2, true);
    }
    if row_visible(3) {
        buttons.record_button(TuiButton::FilenameTemplateField, row_rect(3));
        register_output_options_template_buttons(buttons, area, 3, false);
    }
    if row_visible(4) {
        register_output_options_pills(
            buttons,
            area,
            4,
            "merge      ",
            &opts.merge,
            TuiButton::MergePill,
        );
    }

    if maximized && area.height >= 11 {
        if row_visible(8) {
            buttons.record_button(TuiButton::CompanionExtensionsField, row_rect(8));
        }
        if row_visible(9) {
            buttons.record_button(TuiButton::CompanionFoldersField, row_rect(9));
        }
        if area.height >= 12 && row_visible(10) {
            buttons.record_button(TuiButton::ExcludeFilesField, row_rect(10));
        }
    }

    if maximized && area.height >= 17 {
        register_output_options_pills(
            buttons,
            area,
            OUTPUT_OPTIONS_FORCE_ENCODE_ROW,
            "force enc ",
            &opts.force_encode,
            TuiButton::ForceEncodePill,
        );
        register_output_options_pills(
            buttons,
            area,
            OUTPUT_OPTIONS_DISC_SUBFOLDERS_ROW,
            "disc dirs ",
            &opts.disc_subfolders,
            TuiButton::DiscSubfoldersPill,
        );
        register_output_options_pills(
            buttons,
            area,
            OUTPUT_OPTIONS_WRITE_LOG_ROW,
            "write log  ",
            &opts.write_log,
            TuiButton::WriteLogPill,
        );
    }

    if show_actions && maximized && area.height >= 20 && row_visible(OUTPUT_OPTIONS_ACTIONS_ROW) {
        buttons.record_button(TuiButton::ActionsPipelineField, row_rect(OUTPUT_OPTIONS_ACTIONS_ROW));
    }
}

fn register_output_options_template_buttons(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    row: u16,
    folder_template: bool,
) {
    if row >= area.height.saturating_sub(1) {
        return;
    }
    let right_border_x = area.x.saturating_add(area.width).saturating_sub(1);
    let build_x = right_border_x.saturating_sub(OUTPUT_OPTIONS_TEMPLATE_BUILD_WIDTH);
    let load_x = build_x
        .saturating_sub(OUTPUT_OPTIONS_TEMPLATE_GAP)
        .saturating_sub(OUTPUT_OPTIONS_TEMPLATE_LOAD_WIDTH);
    if load_x <= area.x || build_x <= area.x {
        return;
    }
    let y = area.y.saturating_add(row);
    let (load_button, build_button) = if folder_template {
        (
            TuiButton::TemplateLoadFolderButton,
            TuiButton::TemplateBuildFolderButton,
        )
    } else {
        (
            TuiButton::TemplateLoadFilenameButton,
            TuiButton::TemplateBuildFilenameButton,
        )
    };
    // Register after the full-row field target so the visible pills win
    // overlapping hit tests. This mirrors the rendered right-aligned pill row.
    buttons.record_button(
        load_button,
        Rect::new(load_x, y, OUTPUT_OPTIONS_TEMPLATE_LOAD_WIDTH, 1),
    );
    buttons.record_button(
        build_button,
        Rect::new(build_x, y, OUTPUT_OPTIONS_TEMPLATE_BUILD_WIDTH, 1),
    );
}

fn register_output_options_pills<T>(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    row: u16,
    label: &str,
    state: &PillState<T>,
    button_for: impl Fn(usize) -> TuiButton,
) {
    if row >= area.height.saturating_sub(1) {
        return;
    }
    let y = area.y.saturating_add(row);
    let right = area.x.saturating_add(area.width).saturating_sub(1);
    let label_width = format!("   {}  ", label).chars().count() as u16;
    let mut x = area.x.saturating_add(1).saturating_add(label_width);
    for (index, option) in state.options.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(2);
        }
        let width = option.label.chars().count() as u16 + 2;
        if x >= right {
            break;
        }
        let visible_width = width.min(right.saturating_sub(x));
        buttons.record_button(button_for(index), Rect::new(x, y, visible_width, 1));
        x = x.saturating_add(width);
    }
}


/// Draw the output options pane and register its mouse targets in the standard
/// render-time `ButtonRenderMap` pass.
pub fn draw_output_options_pane_with_mouse_targets(
    f: &mut Frame,
    area: Rect,
    opts: &OutputOptionsState,
    source_info: Option<&SourceInfo>,
    total_source_size: u64,
    format: &FormatState,
    focused: bool,
    maximized: bool,
    show_actions: bool,
    buttons: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    draw_output_options_pane(
        f,
        area,
        opts,
        source_info,
        total_source_size,
        format,
        focused,
        maximized,
        show_actions,
        theme,
    );
    register_output_options_mouse_targets(buttons, area, opts, maximized, show_actions);
}



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
    show_actions: bool,
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
    let is_extensions_focused = focused && opts.field_focus == OutputOptionsField::CompanionExtensions;
    let is_folders_focused = focused && opts.field_focus == OutputOptionsField::CompanionFolders;
    let is_exclude_files_focused = focused && opts.field_focus == OutputOptionsField::ExcludeFiles;
    let is_force_encode_focused = focused && opts.field_focus == OutputOptionsField::ForceEncode;
    let is_disc_subfolders_focused = focused && opts.field_focus == OutputOptionsField::DiscSubfolders;
    let is_write_log_focused = focused && opts.field_focus == OutputOptionsField::WriteLog;
    let is_actions_focused = focused && opts.field_focus == OutputOptionsField::Actions;

    let is_editing = |field| opts.editing == Some(field);

    // Destination path
    let dest_display = opts
        .dest_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let dest_label_style = if is_dest_focused { theme.bright() } else { theme.muted() };
    let dest_value_w = w.saturating_sub(2 + "   dest        ".len());
    let dest_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   dest        ", dest_label_style),
            render_inline_value(
                &dest_display,
                is_editing(OutputOptionsField::DestPath),
                &opts.edit_input,
                is_dest_focused,
                dest_value_w,
                theme,
            ),
        ],
        theme,
    );

    // Folder template
    let folder_label_style = if is_folder_focused { theme.bright() } else { theme.muted() };
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
    let folder_row = template_row_with_value_span(
        border_color,
        w,
        "   folder      ",
        render_inline_value(
            &opts.folder_template,
            is_editing(OutputOptionsField::FolderTemplate),
            &opts.edit_input,
            is_folder_focused,
            folder_tmpl_max,
            theme,
        ),
        folder_label_style,
        load_pill.clone(),
        build_pill.clone(),
        theme,
    );

    // Filename template
    let file_label_style = if is_file_focused { theme.bright() } else { theme.muted() };
    let file_row = template_row_with_value_span(
        border_color,
        w,
        "   filename    ",
        render_inline_value(
            &opts.filename_template,
            is_editing(OutputOptionsField::FilenameTemplate),
            &opts.edit_input,
            is_file_focused,
            folder_tmpl_max,
            theme,
        ),
        file_label_style,
        load_pill,
        build_pill,
        theme,
    );

    // Merge mode pills
    let merge_row = pill_row(
        border_color,
        w,
        "merge      ",
        "",
        &render_pill_spans(&opts.merge, is_merge_focused, theme),
        is_merge_focused,
        theme,
    );

    // Estimated size
    let est_display = estimate_output_size(source_info, total_source_size, format)
        .unwrap_or_else(|| "—".to_string());
    let est_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   est. size   ", theme.muted()),
            Span::styled(est_display, theme.accent()),
        ],
        theme,
    );

    let mut lines = vec![top_line];
    lines.push(dest_row);
    lines.push(folder_row);
    lines.push(file_row);
    lines.push(merge_row);
    lines.push(est_row);

    if maximized && area.height >= 11 {
        lines.push(bordered_line(border_color, w, vec![], theme));
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   Companion files", output_options_section_header_style(theme))],
            theme,
        ));
        lines.push(field_row(
            border_color,
            w,
            "   extensions  ",
            &opts.companion_extensions,
            is_editing(OutputOptionsField::CompanionExtensions),
            &opts.edit_input,
            is_extensions_focused,
            theme,
        ));
        lines.push(field_row(
            border_color,
            w,
            "   folders     ",
            &opts.companion_folders,
            is_editing(OutputOptionsField::CompanionFolders),
            &opts.edit_input,
            is_folders_focused,
            theme,
        ));
        if area.height >= 12 {
            lines.push(field_row(
                border_color,
                w,
                "   exclude     ",
                &opts.companion_exclude_files,
                is_editing(OutputOptionsField::ExcludeFiles),
                &opts.edit_input,
                is_exclude_files_focused,
                theme,
            ));
        }
    }

    if maximized && area.height >= 17 {
        lines.push(bordered_line(border_color, w, vec![], theme));
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   Conversion", output_options_section_header_style(theme))],
            theme,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "force enc ",
            "",
            &render_pill_spans(&opts.force_encode, is_force_encode_focused, theme),
            is_force_encode_focused,
            theme,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "disc dirs ",
            "",
            &render_pill_spans(&opts.disc_subfolders, is_disc_subfolders_focused, theme),
            is_disc_subfolders_focused,
            theme,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "write log  ",
            "",
            &render_pill_spans(&opts.write_log, is_write_log_focused, theme),
            is_write_log_focused,
            theme,
        ));
    }

    if show_actions && maximized && area.height >= 20 {
        lines.push(bordered_line(border_color, w, vec![], theme));
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   Actions", output_options_section_header_style(theme))],
            theme,
        ));
        lines.push(actions_row(
            border_color,
            w,
            &output_options_actions_summary(&opts.actions),
            opts.actions.is_empty(),
            is_actions_focused,
            theme,
        ));
    }

    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    if focused {
        if let Some((row, label_w, value_w)) = output_options_edit_cursor(opts, maximized, area.height as usize, w) {
            let col = inline_cursor_col(&opts.edit_input, value_w);
            let cursor_x = area.x + 1 + label_w as u16 + col;
            let cursor_y = area.y + row as u16;
            if cursor_y < area.y + area.height && cursor_x < area.x + area.width.saturating_sub(1) {
                f.set_cursor(cursor_x, cursor_y);
            }
        }
    }
}



/// Section header styling for the Output Options pane.
///
/// This intentionally mirrors the app-wide `theme.header` token used for
/// section labels: dim text with bold emphasis. Keep this separate from
/// `theme.accent()` so informational section headers do not inherit cyan/amber
/// value styling.
fn output_options_section_header_style(theme: super::theme::Theme) -> Style {
    Style::default()
        .fg(theme.header)
        .add_modifier(Modifier::BOLD)
}


fn field_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    value: &str,
    editing: bool,
    input: &super::text_input::TextInputState,
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let value_w = width.saturating_sub(2 + label.len());
    bordered_line(
        border_color,
        width,
        vec![
            Span::styled(label, label_style),
            render_inline_value(value, editing, input, focused, value_w, theme),
        ],
        theme,
    )
}

pub fn output_options_actions_summary(
    actions: &crate::convert::pipeline::ActionPipeline,
) -> String {
    if actions.is_empty() {
        return "none".to_string();
    }
    let pre = actions.pre.len();
    let post = actions.post.len();
    match (pre, post) {
        (0, post) => format!("{post} post"),
        (pre, 0) => format!("{pre} pre"),
        (pre, post) => format!("{pre} pre · {post} post"),
    }
}

fn actions_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    summary: &str,
    empty: bool,
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let summary_style = if empty {
        Style::default().fg(theme.text_dim)
    } else if focused {
        theme.bright()
    } else {
        theme.accent()
    };
    bordered_line(
        border_color,
        width,
        vec![
            Span::styled("   pipeline    ", label_style),
            Span::styled("▸ ", theme.muted()),
            Span::styled(summary.to_string(), summary_style),
            Span::raw("          "),
            Span::styled("Enter/click edit", theme.muted()),
        ],
        theme,
    )
}

fn output_options_edit_cursor(
    opts: &OutputOptionsState,
    maximized: bool,
    area_height: usize,
    pane_width: usize,
) -> Option<(usize, usize, usize)> {
    let field = opts.editing?;
    let label_w = 15usize;
    match field {
        OutputOptionsField::DestPath => Some((1, label_w, pane_width.saturating_sub(2 + label_w))),
        OutputOptionsField::FolderTemplate => {
            let pill_width = 6 + 1 + 8;
            Some((2, label_w, pane_width.saturating_sub(15 + pill_width + 4)))
        }
        OutputOptionsField::FilenameTemplate => {
            let pill_width = 6 + 1 + 8;
            Some((3, label_w, pane_width.saturating_sub(15 + pill_width + 4)))
        }
        OutputOptionsField::CompanionExtensions if maximized && area_height >= 11 => {
            Some((8, label_w, pane_width.saturating_sub(2 + label_w)))
        }
        OutputOptionsField::CompanionFolders if maximized && area_height >= 11 => {
            Some((9, label_w, pane_width.saturating_sub(2 + label_w)))
        }
        OutputOptionsField::ExcludeFiles if maximized && area_height >= 12 => {
            Some((10, label_w, pane_width.saturating_sub(2 + label_w)))
        }
        _ => None,
    }
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

/// Build a template row with trailing [load] [custom] pills.
fn template_row_with_value_span<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    value_span: Span<'static>,
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
        value_span,
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
            let selected_rate = *format.sample_rate.selected_value();
            let dsd_rate = if selected_rate == crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL {
                info.sample_rate as f64
            } else {
                selected_rate as f64
            };
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
            let selected_rate = *format.sample_rate.selected_value();
            let target_rate = if selected_rate == crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL {
                info.sample_rate as f64
            } else {
                selected_rate as f64
            };
            let target_bits = if format.bit_depth.selected_value().is_source() {
                info.bit_depth? as f64
            } else {
                format.bit_depth.selected_value().bits() as f64
            };
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



#[cfg(test)]
mod output_options_companion_render_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;


    #[test]
    fn maximized_actions_row_renders_live_pipeline_summary() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut opts = OutputOptionsState::new();
        opts.field_focus = OutputOptionsField::Actions;
        opts.actions.post.push(crate::convert::pipeline::ConversionAction::CreateFolder(
            crate::convert::pipeline::CreateFolderAction {
                path: std::path::PathBuf::from("Logs"),
                continue_on_error: false,
            },
        ));
        let format = FormatState::new();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_output_options_pane(
                    frame,
                    Rect::new(0, 0, 80, 20),
                    &opts,
                    None,
                    0,
                    &format,
                    true,
                    true,
                    true,
                    theme,
                )
            })
            .expect("draw output options");

        let mut row = String::new();
        for x in 0..80 {
            row.push_str(terminal.backend().buffer().get(x, OUTPUT_OPTIONS_ACTIONS_ROW).symbol());
        }
        assert!(row.contains("pipeline"), "actions row should render pipeline label: {row}");
        assert!(row.contains("1 post"), "actions row should summarize post actions: {row}");
        assert!(row.contains("Enter/click edit"), "actions row should advertise edit affordance: {row}");
    }

    #[test]
    fn maximized_actions_row_registers_button_map_target_inside_pane_only() {
        let opts = OutputOptionsState::new();
        let mut buttons = ButtonRenderMap::new();
        register_output_options_mouse_targets(
            &mut buttons,
            Rect::new(10, 4, 80, 20),
            &opts,
            true,
            true,
        );

        assert_eq!(
            buttons.find_button_at(11, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            Some(TuiButton::ActionsPipelineField)
        );
        assert_eq!(
            buttons.find_button_at(9, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            None,
            "clicks outside the Output Options pane must not open the actions wizard"
        );
        assert_eq!(
            buttons.find_button_at(89, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            None,
            "right border must not be treated as the actions row"
        );
        assert_eq!(
            buttons.find_button_at(90, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            None,
            "outside cells must not be treated as the actions row"
        );
    }

    #[test]
    fn wrapped_output_options_draw_populates_button_map_for_actions_row() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let opts = OutputOptionsState::new();
        let area = Rect::new(10, 4, 80, 20);
        let format = FormatState::new();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut buttons = ButtonRenderMap::new();

        terminal
            .draw(|frame| {
                draw_output_options_pane_with_mouse_targets(
                    frame,
                    area,
                    &opts,
                    None,
                    0,
                    &format,
                    true,
                    true,
                    true,
                    &mut buttons,
                    theme,
                );
            })
            .expect("draw output options");

        assert_eq!(
            buttons.find_button_at(11, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            Some(TuiButton::ActionsPipelineField),
            "the production Output Options draw path must register the rendered Actions row"
        );
        assert_eq!(buttons.find_button_at(9, 4 + OUTPUT_OPTIONS_ACTIONS_ROW), None);
    }

    #[test]
    fn output_options_registration_includes_template_pills_with_concrete_targets() {
        let opts = OutputOptionsState::new();
        let mut buttons = ButtonRenderMap::new();
        let area = Rect::new(10, 4, 80, 20);

        register_output_options_mouse_targets(&mut buttons, area, &opts, true, true);

        let folder_y = area.y + 2;
        let filename_y = area.y + 3;
        let build_x = area.x + area.width - 1 - OUTPUT_OPTIONS_TEMPLATE_BUILD_WIDTH;
        let load_x = build_x - OUTPUT_OPTIONS_TEMPLATE_GAP - OUTPUT_OPTIONS_TEMPLATE_LOAD_WIDTH;

        assert_eq!(
            buttons.find_button_at(load_x, folder_y),
            Some(TuiButton::TemplateLoadFolderButton),
            "folder template load pill must be registered by the shared helper"
        );
        assert_eq!(
            buttons.find_button_at(build_x, folder_y),
            Some(TuiButton::TemplateBuildFolderButton),
            "folder template custom pill must be registered by the shared helper"
        );
        assert_eq!(
            buttons.find_button_at(load_x, filename_y),
            Some(TuiButton::TemplateLoadFilenameButton),
            "filename template load pill must be registered by the shared helper"
        );
        assert_eq!(
            buttons.find_button_at(build_x, filename_y),
            Some(TuiButton::TemplateBuildFilenameButton),
            "filename template custom pill must be registered by the shared helper"
        );
    }

    #[test]
    fn maximized_output_options_pill_targets_use_rendered_option_indices() {
        let opts = OutputOptionsState::new();
        let mut buttons = ButtonRenderMap::new();
        let area = Rect::new(10, 4, 80, 20);

        register_output_options_mouse_targets(&mut buttons, area, &opts, true, true);

        assert!(
            buttons
                .recorded_buttons()
                .iter()
                .any(|(button, _)| matches!(button, TuiButton::ForceEncodePill(index) if *index < opts.force_encode.options.len())),
            "registered force-encode pill targets must use concrete option indices"
        );
        assert!(
            buttons
                .recorded_buttons()
                .iter()
                .all(|(button, _)| !matches!(button, TuiButton::ForceEncodePill(usize::MAX) | TuiButton::DiscSubfoldersPill(usize::MAX) | TuiButton::WriteLogPill(usize::MAX))),
            "Output Options registration must not create synthetic full-row sentinel pill targets"
        );
    }

    #[test]
    fn non_maximized_output_options_does_not_register_below_fold_targets() {
        let opts = OutputOptionsState::new();
        let mut buttons = ButtonRenderMap::new();
        register_output_options_mouse_targets(
            &mut buttons,
            Rect::new(10, 4, 80, 20),
            &opts,
            false,
            true,
        );

        assert_eq!(
            buttons.find_button_at(11, 4 + OUTPUT_OPTIONS_ACTIONS_ROW),
            None,
            "non-maximized Output Options must not expose invisible Actions-row hitboxes"
        );
        assert_eq!(
            buttons.find_button_at(11, 4 + OUTPUT_OPTIONS_FORCE_ENCODE_ROW),
            None,
            "non-maximized Output Options must not expose invisible conversion-pill hitboxes"
        );
    }

    #[test]
    fn actions_summary_renders_none_for_empty_pipeline() {
        assert_eq!(
            output_options_actions_summary(&crate::convert::pipeline::ActionPipeline::default()),
            "none"
        );
    }

    #[test]
    fn maximized_companion_header_uses_header_style_not_accent_style() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let opts = OutputOptionsState::new();
        let format = FormatState::new();
        let backend = TestBackend::new(60, 11);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| {
                draw_output_options_pane(
                    frame,
                    Rect::new(0, 0, 60, 11),
                    &opts,
                    None,
                    0,
                    &format,
                    true,
                    true,
                    true,
                    theme,
                )
            })
            .expect("draw output options");

        // Rows: top, dest, folder, filename, merge, estimate, blank, header.
        // The 'C' in "   Companion files" starts at x=4 inside the border.
        let cell = terminal.backend().buffer().get(4, 7);
        assert_eq!(cell.symbol(), "C");
        assert_eq!(cell.fg, theme.header, "section header must use the theme.header color token");
        assert_ne!(cell.fg, theme.accent().fg.unwrap_or(theme.text_dim), "section header must not use accent/value styling");
        assert!(cell.modifier.contains(Modifier::BOLD), "section header must be bold like theme.header");
    }
}

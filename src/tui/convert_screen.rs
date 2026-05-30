//! Main convert screen layout: header + preset bar + source/metadata/format/output options + footer.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppState, ConvertFocus, ConvertLayout, SourceMode};
use super::button_map::{ButtonRenderMap, MetadataFieldKind, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::draw_metadata::{draw_metadata_pane, draw_metadata_title_bar};
use super::draw_output::{draw_format_pane, draw_format_title_bar};
use super::draw_output_options::{draw_output_options_pane, draw_output_options_title_bar};
use super::draw_preset_bar::draw_preset_bar;
use super::draw_source::{draw_source_pane, draw_source_title_bar};
use super::pill::PillState;
use super::theme;

/// Draw the full convert screen.
pub fn draw_convert_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let layout = app.convert.layout;
    let pane_constraint = |pane: ConvertFocus, default_height: u16| -> Constraint {
        match layout {
            ConvertLayout::Default => Constraint::Length(default_height),
            ConvertLayout::Maximized(maximized) if maximized == pane => Constraint::Fill(1),
            ConvertLayout::Maximized(_) => Constraint::Length(1),
        }
    };

    let source_h = super::draw_source::source_pane_height(&app.convert.source.mode, area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            pane_constraint(ConvertFocus::Source, source_h),
            pane_constraint(ConvertFocus::Metadata, 5),
            pane_constraint(ConvertFocus::Format, 10),
            pane_constraint(ConvertFocus::OutputOptions, 7),
            Constraint::Length(1),
            Constraint::Length(1),
            if layout == ConvertLayout::Default { Constraint::Min(0) } else { Constraint::Length(0) },
            Constraint::Length(2),
        ])
        .split(area);

    // Pass 1: draw with immutable state reads.
    draw_header(f, chunks[0]);
    {
        let preset = &app.preset;
        let buttons = &mut app.button_map;
        draw_preset_bar(f, chunks[2], preset, buttons);
    }

    let source_focused = app.convert.focus == ConvertFocus::Source;
    if app.convert.is_collapsed(ConvertFocus::Source) {
        draw_source_title_bar(f, chunks[4], &app.convert.source, source_focused);
    } else {
        draw_source_pane(
            f,
            chunks[4],
            &app.convert.source,
            source_focused,
            app.convert.is_maximized(ConvertFocus::Source),
        );
    }

    let metadata_focused = app.convert.focus == ConvertFocus::Metadata;
    if app.convert.is_collapsed(ConvertFocus::Metadata) {
        draw_metadata_title_bar(f, chunks[5], metadata_focused);
    } else {
        draw_metadata_pane(
            f,
            chunks[5],
            &app.convert.metadata,
            &app.convert.source.mode,
            metadata_focused,
            app.convert.is_maximized(ConvertFocus::Metadata),
        );
    }

    let format_focused = app.convert.focus == ConvertFocus::Format;
    if app.convert.is_collapsed(ConvertFocus::Format) {
        draw_format_title_bar(f, chunks[6], format_focused);
    } else {
        draw_format_pane(
            f,
            chunks[6],
            &app.convert.format,
            format_focused,
            app.convert.is_maximized(ConvertFocus::Format),
        );
    }

    let output_focused = app.convert.focus == ConvertFocus::OutputOptions;
    if app.convert.is_collapsed(ConvertFocus::OutputOptions) {
        draw_output_options_title_bar(f, chunks[7], output_focused);
    } else {
        draw_output_options_pane(
            f,
            chunks[7],
            &app.convert.output_options,
            app.convert.source.mode.current_info(),
            app.convert.source.mode.total_source_size(),
            &app.convert.format,
            output_focused,
            app.convert.is_maximized(ConvertFocus::OutputOptions),
        );
    }

    draw_convert_action_bar(f, chunks[9], &mut app.button_map);
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[11],
        app.current_screen,
        &mut app.button_map,
        status_msg,
    );

    // Pass 2: register buttons for the same frame.
    register_buttons(app, &chunks);
}

fn register_buttons(app: &mut AppState, chunks: &[Rect]) {
    register_source_buttons(app, chunks[4]);
    register_metadata_buttons(app, chunks[5]);
    register_format_buttons(app, chunks[6]);
    register_output_options_buttons(app, chunks[7]);
}

fn register_title_bar_buttons(buttons: &mut ButtonRenderMap, area: Rect, focus: ConvertFocus) {
    // Title-bar-only target: pane focus and double-click maximize/restore must
    // not cover content rows. Record Pane first; ButtonRenderMap searches in
    // reverse insertion order, so the narrower maximize and advanced controls
    // below take priority when their cells overlap this title-row target.
    buttons.record_button(TuiButton::Pane(focus), Rect::new(area.x, area.y, area.width, 1));
    register_maximize_toggle(buttons, area, focus);
    register_advanced_toggle(buttons, area, focus);
}

fn register_source_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::Source);
    if app.convert.is_collapsed(ConvertFocus::Source) || area.height < 2 {
        return;
    }

    let buttons = &mut app.button_map;
    let inner_w = area.width.saturating_sub(2);
    let in_batch = app.convert.source.mode.is_batch();
    let in_multi = app.convert.source.mode.is_multi_track();
    let has_source = !app.convert.source.mode.is_empty();
    let (button, label) = if in_batch || in_multi {
        (
            TuiButton::SourceExpandButton,
            super::draw_source::EXPAND_PILL_LABEL,
        )
    } else {
        (
            TuiButton::SourceBrowseButton,
            super::draw_source::BROWSE_PILL_LABEL,
        )
    };
    let pill_label_w = label.chars().count() as u16;
    let right_margin = 3u16;
    let pill_y = area.y + area.height.saturating_sub(2);
    if inner_w >= pill_label_w + right_margin {
        let pill_x = area.x + 1 + (inner_w - pill_label_w - right_margin);
        buttons.record_button(button, Rect::new(pill_x, pill_y, pill_label_w, 1));
        if has_source {
            let analyze_w = super::draw_source::ANALYZE_PILL_LABEL.chars().count() as u16;
            let analyze_x = pill_x.saturating_sub(analyze_w + 2);
            if analyze_x > area.x + 1 {
                buttons.record_button(
                    TuiButton::SourceAnalyzeButton,
                    Rect::new(analyze_x, pill_y, analyze_w, 1),
                );
            }
        }
    }
}

fn register_metadata_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::Metadata);
    if app.convert.is_collapsed(ConvertFocus::Metadata) || area.height < 2 {
        return;
    }

    let visible_rows = area.height.saturating_sub(2) as usize;
    app.button_map
        .record_metadata_file_list_visible_rows(visible_rows);

    match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => {
            register_metadata_file_rows(
                &mut app.button_map,
                area,
                paths.len(),
                *cursor,
                app.convert.metadata.file_scroll,
            );
        }
        SourceMode::MultiTrack { tracks, cursor, .. } => {
            register_metadata_file_rows(
                &mut app.button_map,
                area,
                tracks.len(),
                *cursor,
                app.convert.metadata.file_scroll,
            );
        }
        _ => register_metadata_fields(&mut app.button_map, area),
    }
}

fn register_format_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::Format);
    if app.convert.is_collapsed(ConvertFocus::Format) {
        return;
    }

    let state = &app.convert.format;
    let buttons = &mut app.button_map;
    let label_col = area.x + 17;
    register_pill_row(buttons, &state.format, area.y + 2, label_col, |i| TuiButton::FormatPill(i));

    if state.is_dsd_selected() {
        register_pill_row(buttons, &state.sample_rate, area.y + 3, label_col, |i| TuiButton::RatePill(i));
        register_pill_row(buttons, &state.noise_shaper, area.y + 5, label_col, |i| TuiButton::NoiseShaperPill(i));
        register_pill_row(buttons, &state.modulator_order, area.y + 6, label_col, |i| TuiButton::ModulatorOrderPill(i));
        register_pill_row(buttons, &state.conversion_preset, area.y + 7, label_col, |i| TuiButton::ConversionPresetPill(i));
    } else {
        register_pill_row(buttons, &state.sample_rate, area.y + 3, label_col, |i| TuiButton::RatePill(i));
        register_pill_row(buttons, &state.bit_depth, area.y + 4, label_col, |i| TuiButton::DepthPill(i));
        register_pill_row(buttons, &state.resampler, area.y + 5, label_col, |i| TuiButton::ResamplerPill(i));
        register_pill_row(buttons, &state.dither, area.y + 6, label_col, |i| TuiButton::DitherPill(i));
        register_pill_row(buttons, &state.replaygain, area.y + 7, label_col, |i| TuiButton::ReplayGainPill(i));
    }
}

fn register_output_options_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::OutputOptions);
    if app.convert.is_collapsed(ConvertFocus::OutputOptions) {
        return;
    }

    let state = &app.convert.output_options;
    let buttons = &mut app.button_map;
    let label_col = area.x + 17;
    register_pill_row(buttons, &state.merge, area.y + 4, label_col, |i| TuiButton::MergePill(i));

    let inner_x = area.x + 1;
    let inner_w = area.width.saturating_sub(2);
    buttons.record_button(TuiButton::DestPathField, Rect::new(inner_x, area.y + 1, inner_w, 1));
    let pill_zone = 6 + 1 + 8 + 1;
    let text_w = inner_w.saturating_sub(pill_zone);
    buttons.record_button(
        TuiButton::FolderTemplateField,
        Rect::new(inner_x, area.y + 2, text_w, 1),
    );
    buttons.record_button(
        TuiButton::FilenameTemplateField,
        Rect::new(inner_x, area.y + 3, text_w, 1),
    );
    let load_x = area.x + area.width.saturating_sub(pill_zone);
    buttons.record_button(TuiButton::TemplateLoadFolderButton, Rect::new(load_x, area.y + 2, 6, 1));
    buttons.record_button(TuiButton::TemplateBuildFolderButton, Rect::new(load_x + 7, area.y + 2, 8, 1));
    buttons.record_button(TuiButton::TemplateLoadFilenameButton, Rect::new(load_x, area.y + 3, 6, 1));
    buttons.record_button(TuiButton::TemplateBuildFilenameButton, Rect::new(load_x + 7, area.y + 3, 8, 1));
}

fn register_metadata_fields(buttons: &mut ButtonRenderMap, area: Rect) {
    let inner_x = area.x + 1;
    let inner_w = area.width.saturating_sub(2);
    let half_w = inner_w / 2;
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::Title),
        Rect::new(inner_x, area.y + 1, inner_w, 1),
    );
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::Artist),
        Rect::new(inner_x, area.y + 2, half_w, 1),
    );
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::Album),
        Rect::new(inner_x + half_w, area.y + 2, inner_w - half_w, 1),
    );
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::Genre),
        Rect::new(inner_x, area.y + 3, half_w, 1),
    );
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::Year),
        Rect::new(inner_x + half_w, area.y + 3, inner_w - half_w, 1),
    );
}

fn register_metadata_file_rows(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    len: usize,
    cursor: usize,
    scroll: usize,
) {
    let visible = area.height.saturating_sub(2) as usize;
    if len == 0 || visible == 0 {
        return;
    }
    let scroll = clamp_scroll(scroll, cursor, len, visible);
    let inner_x = area.x + 1;
    let inner_w = area.width.saturating_sub(2);
    for (visual, index) in (scroll..len).take(visible).enumerate() {
        buttons.record_button(
            TuiButton::MetadataFileRow(index),
            Rect::new(inner_x, area.y + 1 + visual as u16, inner_w, 1),
        );
    }
}

fn clamp_scroll(scroll: usize, cursor: usize, len: usize, visible: usize) -> usize {
    if len <= visible {
        return 0;
    }
    let max_scroll = len.saturating_sub(visible);
    let mut scroll = scroll.min(max_scroll);
    if cursor < scroll {
        scroll = cursor;
    } else if cursor >= scroll + visible {
        scroll = cursor + 1 - visible;
    }
    scroll.min(max_scroll)
}

fn register_advanced_toggle(buttons: &mut ButtonRenderMap, pane_area: Rect, focus: ConvertFocus) {
    if pane_area.width < 12 {
        return;
    }
    let x = pane_area.x + pane_area.width - 10;
    buttons.record_button(TuiButton::AdvancedToggle(focus), Rect::new(x, pane_area.y, 8, 1));
}

fn register_maximize_toggle(buttons: &mut ButtonRenderMap, pane_area: Rect, focus: ConvertFocus) {
    if pane_area.width < 4 {
        return;
    }
    buttons.record_button(
        TuiButton::MaximizeToggle(focus),
        Rect::new(pane_area.x + 2, pane_area.y, 1, 1),
    );
}

fn register_pill_row<T: Clone>(
    buttons: &mut ButtonRenderMap,
    state: &PillState<T>,
    y: u16,
    start_x: u16,
    make_button: impl Fn(usize) -> TuiButton,
) {
    let mut x = start_x;
    for (i, opt) in state.options.iter().enumerate() {
        if i > 0 {
            x += 2;
        }
        let pill_width = opt.label.len() as u16 + 2;
        buttons.record_button(make_button(i), Rect::new(x, y, pill_width, 1));
        x += pill_width;
    }
}

/// Draw the convert screen action bar with enqueue + enqueue+start pills, centered.
fn draw_convert_action_bar(f: &mut Frame, area: Rect, buttons: &mut ButtonRenderMap) {
    use super::draw_overlays::{footer_pill_pub, pill_gap_pub};

    let pills: &[(&str, TuiButton, Color)] = &[
        ("enqueue", TuiButton::SourceEnqueueButton, theme::GREEN),
        (
            "enqueue + start",
            TuiButton::SourceEnqueueStartButton,
            theme::BLUE,
        ),
    ];

    let total_width: u16 = pills
        .iter()
        .map(|(label, _, _)| label.len() as u16 + 2)
        .sum::<u16>()
        + (pills.len().saturating_sub(1) as u16);

    let left_pad = area.width.saturating_sub(total_width) / 2;
    let mut x = area.x + left_pad;
    let mut spans: Vec<Span> = vec![Span::raw(" ".repeat(left_pad as usize))];

    for (i, (label, btn, color)) in pills.iter().enumerate() {
        if i > 0 {
            spans.push(pill_gap_pub());
            x += 1;
        }
        let pill = footer_pill_pub(label, *color);
        let pill_width = label.len() as u16 + 2;
        buttons.record_button(*btn, Rect::new(x, area.y, pill_width, 1));
        x += pill_width;
        spans.push(pill);
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

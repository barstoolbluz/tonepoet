//! Main convert screen layout: header + preset bar + source/metadata/format/output options + footer.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppState, ConvertFocus, ConvertLayout, FormatField, FormatPaneRow, ResamplerChoice, SourceMode};
use super::button_map::{ButtonRenderMap, MetadataFieldKind, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::draw_metadata::{draw_metadata_pane, draw_metadata_title_bar};
use super::draw_output::{draw_format_pane, draw_format_title_bar};
use super::draw_output_options::{
    draw_output_options_pane, draw_output_options_title_bar,
    register_output_options_mouse_targets,
};
use super::draw_preset_bar::draw_preset_bar;
use super::draw_source::{draw_source_pane, draw_source_title_bar};
use super::pill::PillState;

/// Draw the full convert screen.
pub fn draw_convert_screen(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    let layout = app.convert.layout;
    let pane_constraint = |pane: ConvertFocus, default_height: u16| -> Constraint {
        match layout {
            ConvertLayout::Default => Constraint::Length(default_height),
            ConvertLayout::Maximized(maximized) if maximized == pane => Constraint::Fill(1),
            ConvertLayout::Maximized(_) => Constraint::Length(1),
        }
    };

    let source_h = super::draw_source::source_pane_height(&app.convert.source.mode, area.width);
    let format_h = (app.convert.format.pane_rows(false).len() as u16).saturating_add(3);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            pane_constraint(ConvertFocus::Source, source_h),
            pane_constraint(ConvertFocus::Metadata, 6),
            pane_constraint(ConvertFocus::Format, format_h),
            pane_constraint(ConvertFocus::OutputOptions, 7),
            Constraint::Length(1),
            Constraint::Length(1),
            if layout == ConvertLayout::Default { Constraint::Min(0) } else { Constraint::Length(0) },
            Constraint::Length(2),
        ])
        .split(area);

    // Pass 1: draw with immutable state reads.
    draw_header(f, chunks[0], theme);
    {
        let preset = &app.preset;
        let buttons = &mut app.button_map;
        draw_preset_bar(f, chunks[2], preset, buttons, theme);
    }

    let source_focused = app.convert.focus == ConvertFocus::Source;
    if app.convert.is_collapsed(ConvertFocus::Source) {
        draw_source_title_bar(f, chunks[4], &app.convert.source, source_focused, theme);
    } else {
        draw_source_pane(
            f,
            chunks[4],
            &app.convert.source,
            source_focused,
            app.convert.is_maximized(ConvertFocus::Source), theme);
    }

    let metadata_focused = app.convert.focus == ConvertFocus::Metadata;
    if app.convert.is_collapsed(ConvertFocus::Metadata) {
        draw_metadata_title_bar(f, chunks[5], metadata_focused, theme);
    } else {
        draw_metadata_pane(
            f,
            chunks[5],
            &app.convert.metadata,
            &app.convert.source.mode,
            metadata_focused,
            app.convert.is_maximized(ConvertFocus::Metadata), theme);
    }

    let format_focused = app.convert.focus == ConvertFocus::Format;
    if app.convert.is_collapsed(ConvertFocus::Format) {
        draw_format_title_bar(f, chunks[6], format_focused, theme);
    } else {
        draw_format_pane(
            f,
            chunks[6],
            &app.convert.format,
            format_focused,
            app.convert.is_maximized(ConvertFocus::Format), theme);
    }

    let output_focused = app.convert.focus == ConvertFocus::OutputOptions;
    if app.convert.is_collapsed(ConvertFocus::OutputOptions) {
        draw_output_options_title_bar(f, chunks[7], output_focused, theme);
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
            app.conversion_actions_ui_enabled(),
            theme,
        );
    }

    draw_convert_action_bar(f, chunks[9], &mut app.button_map, theme);
    let file_task_footer = app.file_task_footer_state();
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[11],
        app.current_screen,
        app.browse.tab_count(),
        &mut app.button_map,
        status_msg,
        file_task_footer,
        theme,
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

    // Stream pill click targets on the header line for multi-presentation discs.
    if let super::app::SourceMode::MultiTrack {
        disc_contents: Some(ref dc),
        ..
    } = app.convert.source.mode
    {
        if dc.presentations.len() >= 2 {
            let header_y = area.y + 1;
            let third = inner_w / 3;
            buttons.record_button(
                TuiButton::SourceStreamPrev,
                Rect::new(area.x + 1 + inner_w - third * 2, header_y, third, 1),
            );
            buttons.record_button(
                TuiButton::SourceStreamNext,
                Rect::new(area.x + 1 + inner_w - third, header_y, third, 1),
            );
        }
    }
}

fn register_metadata_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::Metadata);
    if app.convert.is_collapsed(ConvertFocus::Metadata) || area.height < 2 {
        return;
    }

    let list_area = area;
    let visible_rows = list_area.height.saturating_sub(2) as usize;
    app.button_map
        .record_metadata_file_list_visible_rows(visible_rows);

    match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => {
            app.button_map.record_button(
                TuiButton::MetadataField(MetadataFieldKind::AlbumArtist),
                Rect::new(list_area.x + 1, list_area.y + 1, list_area.width.saturating_sub(2), 1),
            );
            let shifted_area = Rect::new(
                list_area.x,
                list_area.y.saturating_add(1),
                list_area.width,
                list_area.height.saturating_sub(1),
            );
            register_metadata_file_rows(
                &mut app.button_map,
                shifted_area,
                paths.len(),
                *cursor,
                app.convert.metadata.file_scroll,
            );
        }
        SourceMode::MultiTrack { tracks, cursor, archive_preview: None, .. } => {
            app.button_map.record_button(
                TuiButton::MetadataField(MetadataFieldKind::AlbumArtist),
                Rect::new(list_area.x + 1, list_area.y + 1, list_area.width.saturating_sub(2), 1),
            );
            let shifted_area = Rect::new(
                list_area.x,
                list_area.y.saturating_add(1),
                list_area.width,
                list_area.height.saturating_sub(1),
            );
            register_metadata_file_rows(
                &mut app.button_map,
                shifted_area,
                tracks.len(),
                *cursor,
                app.convert.metadata.file_scroll,
            );
        }
        SourceMode::MultiTrack { tracks, cursor, .. } => {
            register_metadata_file_rows(
                &mut app.button_map,
                list_area,
                tracks.len(),
                *cursor,
                app.convert.metadata.file_scroll,
            );
        }
        _ => register_metadata_fields(&mut app.button_map, list_area),
    }

}

fn register_format_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::Format);
    if app.convert.is_collapsed(ConvertFocus::Format) {
        return;
    }

    let maximized = app.convert.is_maximized(ConvertFocus::Format);
    let rows = app.convert.format.pane_rows(maximized);
    let state = &app.convert.format;
    let buttons = &mut app.button_map;
    let label_col = area.x + 17;

    for (row_index, row) in rows.into_iter().enumerate() {
        let y = area.y.saturating_add(1).saturating_add(row_index as u16);
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        let FormatPaneRow::Field(field) = row else {
            continue;
        };
        match field {
            FormatField::Format => register_pill_row(buttons, &state.format, y, label_col, TuiButton::FormatPill),
            FormatField::SampleRate | FormatField::DsdRate => register_pill_row(buttons, &state.sample_rate, y, label_col, TuiButton::RatePill),
            FormatField::BitDepth => register_pill_row(buttons, &state.bit_depth, y, label_col, TuiButton::DepthPill),
            FormatField::Resampler => register_pill_row(buttons, &state.resampler, y, label_col, TuiButton::ResamplerPill),
            FormatField::Dither => register_pill_row(buttons, &state.dither, y, label_col, TuiButton::DitherPill),
            FormatField::ReplayGain => register_pill_row(buttons, &state.replaygain, y, label_col, TuiButton::ReplayGainPill),
            FormatField::NoiseShaper => register_pill_row(buttons, &state.noise_shaper, y, label_col, TuiButton::NoiseShaperPill),
            FormatField::ModulatorOrder => register_pill_row(buttons, &state.modulator_order, y, label_col, TuiButton::ModulatorOrderPill),
            FormatField::ConversionPreset => register_pill_row(buttons, &state.conversion_preset, y, label_col, TuiButton::ConversionPresetPill),
            FormatField::DsdPath => register_pill_row(buttons, &state.dsd_pathway, y, label_col, TuiButton::DsdPathPill),
            FormatField::DsdProfile => register_pill_row(buttons, &state.dsd_profile, y, label_col, TuiButton::DsdProfilePill),
            FormatField::DsdGain => register_enabled_pill_row(buttons, &state.dsd_gain_mode, y, label_col, TuiButton::DsdGainPill),
            FormatField::DsdGainScope => register_pill_row(buttons, &state.dsd_auto_gain_scope, y, label_col, TuiButton::DsdGainScopePill),
            FormatField::DsdTruePeakScan => register_pill_row(buttons, &state.dsd_true_peak_scan_mode, y, label_col, TuiButton::DsdTruePeakScanPill),
            FormatField::DsdGainDb => buttons.record_button(
                TuiButton::DsdGainDbField,
                ratatui::layout::Rect::new(area.x, y, area.width, 1),
            ),
            FormatField::DsdNormalizeTarget => buttons.record_button(
                TuiButton::DsdNormalizeTargetField,
                ratatui::layout::Rect::new(area.x, y, area.width, 1),
            ),
            FormatField::Container => {
                let containers = state.format.selected_value().available_containers();
                let mut x = label_col;
                for (index, container) in containers.iter().enumerate() {
                    let width = container.display_name.len() as u16 + 2;
                    if container.enabled {
                        buttons.record_button(
                            TuiButton::ContainerPill(index),
                            ratatui::layout::Rect::new(x, y, width, 1),
                        );
                    }
                    x = x.saturating_add(width);
                    if index + 1 < containers.len() {
                        x = x.saturating_add(1);
                    }
                }
                if matches!(
                    *state.format.selected_value(),
                    crate::convert::formats::AudioFormat::Flac
                        | crate::convert::formats::AudioFormat::Aac
                        | crate::convert::formats::AudioFormat::Opus
                        | crate::convert::formats::AudioFormat::Mp3
                        | crate::convert::formats::AudioFormat::WavPack
                ) {
                    let name = state.format.selected_value().name().to_lowercase();
                    let width = name.len() as u16 + 11;
                    if right_settings_pill_fits(area, x, width) {
                        let x = area.x + area.width.saturating_sub(width + 1);
                        buttons.record_button(
                            TuiButton::FormatSettingsButton,
                            ratatui::layout::Rect::new(x, y, width, 1),
                        );
                    }
                }
            }
            FormatField::ResampleQuality => {
                let mut x = label_col;
                let quality_choices = state.resample_quality_choices();
                for (index, (_, label)) in quality_choices.iter().enumerate() {
                    let width = label.len() as u16 + 2;
                    buttons.record_button(
                        TuiButton::ResampleQualityPill(index),
                        ratatui::layout::Rect::new(x, y, width, 1),
                    );
                    x = x.saturating_add(width);
                    if index + 1 < quality_choices.len() {
                        x = x.saturating_add(1);
                    }
                }
                let resampler_name = match *state.resampler.selected_value() {
                    ResamplerChoice::Ssrc => Some("ssrc"),
                    ResamplerChoice::Sox => Some("sox"),
                    ResamplerChoice::Soxr => Some("soxr"),
                    ResamplerChoice::None => None,
                };
                if let Some(name) = resampler_name {
                    let width = name.len() as u16 + 11;
                    if right_settings_pill_fits(area, x, width) {
                        let x = area.x + area.width.saturating_sub(width + 1);
                        buttons.record_button(
                            TuiButton::ResamplerSettingsButton,
                            ratatui::layout::Rect::new(x, y, width, 1),
                        );
                    }
                }
            }
        }
    }
}

fn right_settings_pill_fits(area: Rect, left_content_end_x: u16, pill_width: u16) -> bool {
    let left_content_width = left_content_end_x.saturating_sub(area.x);
    left_content_width
        .saturating_add(pill_width)
        .saturating_add(2)
        <= area.width
}

fn register_enabled_pill_row<T>(
    buttons: &mut ButtonRenderMap,
    state: &PillState<T>,
    y: u16,
    mut x: u16,
    button: impl Fn(usize) -> TuiButton,
) {
    for (index, option) in state.options.iter().enumerate().filter(|(_, option)| option.enabled) {
        let width = option.label.len() as u16 + 2;
        buttons.record_button(button(index), Rect::new(x, y, width, 1));
        x = x.saturating_add(width.saturating_add(2));
    }
}

fn register_output_options_buttons(app: &mut AppState, area: Rect) {
    register_title_bar_buttons(&mut app.button_map, area, ConvertFocus::OutputOptions);
    let maximized = app.convert.is_maximized(ConvertFocus::OutputOptions);
    app.button_map.record_output_options_layout(maximized, area.height);
    if app.convert.is_collapsed(ConvertFocus::OutputOptions) {
        return;
    }

    // Output Options concrete controls are registered through the same helper
    // that owns their rendered row constants. Keep this in the normal
    // Convert-screen second pass; do not recreate event-time coordinate
    // fallbacks, hidden render side channels, or pane-generic synthetic child
    // targets. Register after the title-bar target so concrete controls win
    // ButtonRenderMap overlap resolution.
    let show_actions = app.conversion_actions_ui_enabled();
    register_output_options_mouse_targets(
        &mut app.button_map,
        area,
        &app.convert.output_options,
        maximized,
        show_actions,
    );
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
    buttons.record_button(
        TuiButton::MetadataField(MetadataFieldKind::AlbumArtist),
        Rect::new(inner_x, area.y + 4, inner_w, 1),
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
fn draw_convert_action_bar(f: &mut Frame, area: Rect, buttons: &mut ButtonRenderMap, theme: super::theme::Theme) {
    use super::draw_overlays::{footer_pill_pub, pill_gap_pub};

    let pills: &[(&str, TuiButton, Color)] = &[
        ("enqueue", TuiButton::SourceEnqueueButton, theme.green),
        (
            "enqueue + start",
            TuiButton::SourceEnqueueStartButton,
            theme.blue,
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
        let pill = footer_pill_pub(label, *color, theme);
        let pill_width = label.len() as u16 + 2;
        buttons.record_button(*btn, Rect::new(x, area.y, pill_width, 1));
        x += pill_width;
        spans.push(pill);
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}


#[cfg(test)]
mod output_options_button_map_tests {
    use super::*;
    use super::super::draw_output_options::{
        OUTPUT_OPTIONS_DISC_SUBFOLDERS_ROW, OUTPUT_OPTIONS_FORCE_ENCODE_ROW,
        OUTPUT_OPTIONS_WRITE_LOG_ROW,
    };
    use crate::config::TonepoetConfig;
    use crate::tui::app::AppScreen;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn rendered_output_options_registers_below_fold_pill_hit_rows() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::OutputOptions;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::OutputOptions);

        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                draw_convert_screen(frame, Rect::new(0, 0, 100, 40), &mut app, theme);
            })
            .expect("draw convert screen");

        let dest_rect = app
            .button_map
            .find_button_rect(&TuiButton::DestPathField)
            .expect("destination field should be registered for visible output options pane");
        let pane_x = dest_rect.x.saturating_sub(1);
        let pane_y = dest_rect.y.saturating_sub(1);
        let label_col = pane_x + 17;

        assert_eq!(
            app.button_map
                .find_button_at(label_col, pane_y + OUTPUT_OPTIONS_FORCE_ENCODE_ROW),
            Some(TuiButton::ForceEncodePill(0)),
            "force-encode pill hit row must match the rendered force enc row",
        );
        assert_eq!(
            app.button_map
                .find_button_at(label_col, pane_y + OUTPUT_OPTIONS_DISC_SUBFOLDERS_ROW),
            Some(TuiButton::DiscSubfoldersPill(0)),
            "disc-dirs pill hit row must match the rendered disc dirs row",
        );
        assert_eq!(
            app.button_map
                .find_button_at(label_col, pane_y + OUTPUT_OPTIONS_WRITE_LOG_ROW),
            Some(TuiButton::WriteLogPill(0)),
            "write-log pill hit row must match the rendered write log row",
        );
    }
}


#[cfg(test)]
mod output_options_registration_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{AppScreen, OutputOptionsField};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn gated_convert_screen_hides_actions_row_and_hitbox() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        // Default config: the conversion-actions feature gate is OFF.
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        assert!(!app.conversion_actions_ui_enabled());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::OutputOptions;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::OutputOptions);
        app.button_map.clear();

        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw convert screen");

        assert!(
            app.button_map
                .find_button_rect(&super::TuiButton::ActionsPipelineField)
                .is_none(),
            "gated-off Actions row must not register a hitbox"
        );
        let buffer_text: String = (0..40)
            .map(|y| (0..100).map(|x| terminal.backend().buffer().get(x, y).symbol().to_string()).collect::<String>())
            .collect();
        assert!(
            !buffer_text.contains("   Actions"),
            "gated-off Actions section must not render"
        );
    }

    #[test]
    fn draw_convert_screen_registers_rendered_output_options_actions_row() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut config = TonepoetConfig::default();
        config.ui.show_conversion_actions = true;
        let mut app = AppState::new_for_test(config);
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::OutputOptions;
        app.convert.output_options.field_focus = OutputOptionsField::Actions;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::OutputOptions);
        app.button_map.clear();

        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw convert screen");

        assert!(
            app.button_map
                .find_button_rect(&TuiButton::ActionsPipelineField)
                .is_some(),
            "production draw_convert_screen/register_buttons path must register the rendered Actions row when it is actually rendered"
        );
    }

    #[test]
    fn draw_convert_screen_does_not_register_actions_row_when_maximized_pane_too_short() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::OutputOptions;
        app.convert.output_options.field_focus = OutputOptionsField::Actions;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::OutputOptions);
        app.button_map.clear();

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw convert screen");

        assert_eq!(app.button_map.output_options_layout(), Some((true, 13)));
        assert!(
            app.button_map
                .find_button_rect(&TuiButton::ActionsPipelineField)
                .is_none(),
            "a maximized Output Options pane shorter than the Actions row threshold must not register invisible Actions targets"
        );
    }

    #[test]
    fn draw_convert_screen_does_not_register_actions_row_when_output_options_collapsed() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::Source;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Source);
        app.button_map.clear();

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw convert screen");

        assert!(
            app.button_map
                .find_button_rect(&TuiButton::ActionsPipelineField)
                .is_none(),
            "collapsed Output Options pane must not receive invisible Actions-row targets"
        );
    }
}

#[cfg(test)]
mod format_render_registration_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::convert::formats::AudioFormat;
    use crate::tui::app::{AppScreen, DsdGainMode, FormatField, ResamplerChoice};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tonepoet_pipeline::enums::ResampleQuality;

    fn row_text(terminal: &Terminal<TestBackend>, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| terminal.backend().buffer().get(x, y).symbol().to_string())
            .collect()
    }

    fn rendered_row(terminal: &Terminal<TestBackend>, width: u16, height: u16, needle: &str) -> u16 {
        (0..height)
            .find(|&y| row_text(terminal, y, width).contains(needle))
            .unwrap_or_else(|| panic!("rendered row containing {needle:?} not found"))
    }

    #[test]
    fn dsd_to_pcm_render_keyboard_and_hit_map_share_one_dynamic_layout() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::Format;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
        app.convert.format.set_source_is_dsd(true);
        assert!(app.convert.format.format.select_value(&AudioFormat::Flac));
        assert!(app.convert.format.resampler.select_value(&ResamplerChoice::Sox));
        assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::Auto));
        app.convert.format.resample_quality = ResampleQuality::Ultra;
        app.convert.format.apply_format_constraints();

        let backend = TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("terminal");
        app.button_map.clear();
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw DSD-to-PCM format pane");

        let gain_scope_y = rendered_row(&terminal, WIDTH, HEIGHT, "gain scope");
        let auto_margin_y = rendered_row(&terminal, WIDTH, HEIGHT, "auto margin");
        let container_y = rendered_row(&terminal, WIDTH, HEIGHT, "container");
        let resample_quality_y = rendered_row(&terminal, WIDTH, HEIGHT, "insane");
        let gain_y = rendered_row(&terminal, WIDTH, HEIGHT, "DSD gain");

        let album_scope_rect = app
            .button_map
            .find_button_rect(&TuiButton::DsdGainScopePill(1))
            .expect("gain-scope album pill hit region");
        assert_eq!(album_scope_rect.y, gain_scope_y);
        assert_eq!(
            app.button_map.find_button_at(album_scope_rect.x, album_scope_rect.y),
            Some(TuiButton::DsdGainScopePill(1)),
        );
        assert!(crate::tui::format_interactions::handle_convert_format_button(
            &mut app.convert,
            TuiButton::DsdGainScopePill(1),
        ));
        assert_eq!(app.convert.format.dsd_gain_mode.selected_value(), &DsdGainMode::Auto);
        assert_eq!(app.convert.format.dsd_auto_gain_scope.selected, 1);
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::DsdNormalizeTargetField)
                .expect("auto-margin hit region")
                .y,
            auto_margin_y,
        );
        assert!(
            app.button_map.find_button_rect(&TuiButton::DsdGainDbField).is_none(),
            "manual-gain row is inactive in auto mode and must not render or receive a hit target"
        );
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::ContainerPill(0))
                .expect("container hit region")
                .y,
            container_y,
        );
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::ResampleQualityPill(5))
                .expect("insane resampling-quality hit region")
                .y,
            resample_quality_y,
        );

        let gain_text = row_text(&terminal, gain_y, WIDTH);
        assert!(gain_text.contains("disabled"));
        assert!(gain_text.contains("auto"));
        assert!(gain_text.contains("manual"));
        assert!(!gain_text.contains("reference"));
        assert!(!gain_text.contains("native"));
        assert!(!gain_text.contains("normalize"));

        let keyboard_rows = app.convert.format.visible_fields(true);
        assert!(keyboard_rows.contains(&FormatField::DsdGainScope));
        assert!(keyboard_rows.contains(&FormatField::DsdNormalizeTarget));
        assert!(keyboard_rows.contains(&FormatField::Container));
        assert!(keyboard_rows.contains(&FormatField::ResampleQuality));
        assert!(!keyboard_rows.contains(&FormatField::DsdGainDb));

        assert!(crate::tui::format_interactions::handle_convert_format_button(
            &mut app.convert,
            TuiButton::ContainerPill(1),
        ));
        assert_eq!(app.convert.format.field_focus, FormatField::Container);
        let clicked_container = app.convert.format.selected_container_index;
        app.convert.format.select_focused_next(None, None);
        assert_ne!(app.convert.format.selected_container_index, clicked_container);

        app.convert.format.resample_quality = ResampleQuality::Ultra;
        assert!(crate::tui::format_interactions::handle_convert_format_button(
            &mut app.convert,
            TuiButton::ResampleQualityPill(4),
        ));
        assert_eq!(app.convert.format.field_focus, FormatField::ResampleQuality);
        app.convert.format.select_focused_next(None, None);
        assert_eq!(app.convert.format.resample_quality, ResampleQuality::Insane);

        assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::Fixed));
        app.convert.format.apply_format_constraints();
        app.button_map.clear();
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("redraw manual DSD-to-PCM format pane");

        let gain_db_y = rendered_row(&terminal, WIDTH, HEIGHT, "gain dB");
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::DsdGainDbField)
                .expect("manual-gain hit region")
                .y,
            gain_db_y,
        );
        let all_text: String = (0..HEIGHT)
            .map(|y| row_text(&terminal, y, WIDTH))
            .collect();
        assert!(!all_text.contains("gain scope"));
        assert!(!all_text.contains("auto margin"));
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DsdGainScopePill(0))
            .is_none());
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DsdNormalizeTargetField)
            .is_none());
    }
}

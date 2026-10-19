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
use super::draw_output::{draw_format_pane, draw_format_title_bar, true_peak_custom_pill_offset, true_peak_info_offset};
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
            FormatField::OutputChannelLayout => register_pill_row(
                buttons, &state.output_channel_layout, y, label_col, TuiButton::ChannelLayoutPill,
            ),
            FormatField::RemoveDcOffset => {
                let labels = super::draw_output::dc_correction_labels(area.width as usize);
                let mut x = label_col;
                let pane_right = area.x.saturating_add(area.width).saturating_sub(1);
                for (index, label) in labels.iter().enumerate() {
                    let len = label.len() as u16;
                    if x.saturating_add(len) <= pane_right {
                        buttons.record_button(TuiButton::RemoveDcPill(index), Rect::new(x, y, len, 1));
                    }
                    x = x.saturating_add(len + 2);
                }
                // pill_row adds two cells before U+24D8; include glyph and
                // same-foreground guard cell in a two-cell target.
                if x.saturating_add(2) <= pane_right {
                    buttons.record_button(TuiButton::DcInfo, Rect::new(x, y, 2, 1));
                }
            }
            FormatField::DcManualValue => {
                if label_col < area.x.saturating_add(area.width).saturating_sub(1) {
                    buttons.record_button(TuiButton::DcManualValueField,
                        Rect::new(label_col, y, area.x + area.width - label_col - 1, 1));
                }
            }
            FormatField::Deemphasis => {
                buttons.record_button(TuiButton::DeemphasisPill(0), Rect::new(label_col, y, 5, 1));
                buttons.record_button(TuiButton::DeemphasisPill(1), Rect::new(label_col + 7, y, 4, 1));
                if state.deemphasis_evidence != super::app::ConvertDeemphasisEvidence::None {
                    // Two logical cells intentionally cover the U+24D8 glyph
                    // plus its same-style guard cell. That remains clickable
                    // whether the terminal renders the ambiguous-width glyph
                    // as one cell or two.
                    buttons.record_button(TuiButton::DeemphasisInfo, Rect::new(label_col + 13, y, 2, 1));
                }
            }
            FormatField::HdcdDecode => {
                if state.hdcd_is_actionable() {
                    buttons.record_button(TuiButton::HdcdPill(0), Rect::new(label_col, y, 5, 1));
                    buttons.record_button(TuiButton::HdcdPill(1), Rect::new(label_col + 7, y, 4, 1));
                }
                buttons.record_button(TuiButton::HdcdInfo, Rect::new(label_col + 13, y, 2, 1));
            }
            FormatField::ReplayGain => register_pill_row(buttons, &state.replaygain, y, label_col, TuiButton::ReplayGainPill),
            FormatField::PcmTruePeak => {
                let labels = super::draw_output::pcm_gain_pill_labels(area.width as usize);
                let pane_right = area.x.saturating_add(area.width).saturating_sub(1);
                let mut x = label_col;
                for (index, label) in labels.iter().enumerate() {
                    let len = label.len() as u16 + 2; // pill padding
                    if x.saturating_add(len) <= pane_right {
                        buttons.record_button(TuiButton::PcmTruePeakPill(index),
                            Rect::new(x, y, len, 1));
                    }
                    x = x.saturating_add(len + 2); // two-cell gap after each pill
                }
                // The common pill_row suffix places U+24D8 two cells after
                // the last pill; its guard cell is part of the same hitbox.
                if x.saturating_add(2) <= pane_right {
                    buttons.record_button(TuiButton::PcmGainInfo,
                        Rect::new(x, y, 2, 1));
                }
            }
            FormatField::PcmGainDb => buttons.record_button(
                TuiButton::PcmGainDbField,
                ratatui::layout::Rect::new(area.x, y, area.width, 1),
            ),
            FormatField::TruePeakScope => register_pill_row(
                buttons,
                &state.pcm_true_peak_scope,
                y,
                label_col,
                TuiButton::TruePeakScopePill,
            ),
            FormatField::PcmTruePeakScan => register_pill_row(
                buttons,
                &state.pcm_true_peak_scan_mode,
                y,
                label_col,
                TuiButton::PcmTruePeakScanPill,
            ),
            FormatField::PcmTruePeakTarget => {
                buttons.record_button(TuiButton::PcmTruePeakTargetField, Rect::new(area.x, y, area.width, 1));
                if let Some(offset) = true_peak_custom_pill_offset(
                    state.pcm_true_peak_target_dbtp,
                    state.true_peak_target_editing == Some(field),
                    area.width as usize,
                ) {
                    buttons.record_button(TuiButton::PcmTruePeakCustomPill, Rect::new(area.x + offset, y, 8, 1));
                }
                register_true_peak_info_button(
                    buttons, area, y, state.pcm_true_peak_target_dbtp,
                    state.true_peak_target_editing == Some(field),
                );
            }
            FormatField::NoiseShaper => register_pill_row(buttons, &state.noise_shaper, y, label_col, TuiButton::NoiseShaperPill),
            FormatField::ModulatorOrder => register_pill_row(buttons, &state.modulator_order, y, label_col, TuiButton::ModulatorOrderPill),
            FormatField::ConversionPreset => register_pill_row(buttons, &state.conversion_preset, y, label_col, TuiButton::ConversionPresetPill),
            FormatField::DsdPath => register_pill_row(buttons, &state.dsd_pathway, y, label_col, TuiButton::DsdPathPill),
            FormatField::DsdProfile => register_pill_row(buttons, &state.dsd_profile, y, label_col, TuiButton::DsdProfilePill),
            FormatField::DsdCustomReconstruction => register_pill_row(buttons, &state.dsd_custom_reconstruction, y, label_col, TuiButton::DsdCustomReconstructionPill),
            FormatField::DsdCustomLowpass => register_pill_row(buttons, &state.dsd_custom_lowpass, y, label_col, TuiButton::DsdCustomLowpassPill),
            FormatField::DsdCustomExportLevel => register_pill_row(buttons, &state.dsd_custom_export_level, y, label_col, TuiButton::DsdCustomExportLevelPill),
            FormatField::DsdGain => register_enabled_pill_row(buttons, &state.dsd_gain_mode, y, label_col, TuiButton::DsdGainPill),
            FormatField::DsdTruePeakScope => register_pill_row(buttons, &state.dsd_true_peak_scope, y, label_col, TuiButton::DsdTruePeakScopePill),
            FormatField::DsdTruePeakScan => register_pill_row(buttons, &state.dsd_true_peak_scan_mode, y, label_col, TuiButton::DsdTruePeakScanPill),
            FormatField::DsdGainDb => buttons.record_button(
                TuiButton::DsdGainDbField,
                ratatui::layout::Rect::new(area.x, y, area.width, 1),
            ),
            FormatField::DsdTruePeakTarget => {
                buttons.record_button(TuiButton::DsdTruePeakTargetField, Rect::new(area.x, y, area.width, 1));
                if let Some(offset) = true_peak_custom_pill_offset(
                    state.dsd_true_peak_target_dbtp,
                    state.true_peak_target_editing == Some(field),
                    area.width as usize,
                ) {
                    buttons.record_button(TuiButton::DsdTruePeakCustomPill, Rect::new(area.x + offset, y, 8, 1));
                }
                register_true_peak_info_button(
                    buttons, area, y, state.dsd_true_peak_target_dbtp,
                    state.true_peak_target_editing == Some(field),
                );
            }
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

// Concrete hit target is recorded after the whole-row target so the icon
// opens help instead of entering the value editor. Match the draw guard.
fn register_true_peak_info_button(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    y: u16,
    value: tonepoet_pipeline::DbNano,
    editing: bool,
) {
    if area.width < 30 || area.height < 6 {
        return; // Matches draw_format_pane's visibility guard.
    }
    if let Some(offset) = true_peak_info_offset(value, editing, area.width as usize) {
        buttons.record_button(
            TuiButton::TruePeakInfo,
            Rect::new(area.x + offset, y, 2, 1),
        );
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
    use crate::tui::app::{
        ActiveOverlay, AppScreen, ConvertDeemphasisEvidence, DsdGainMode, FormatField,
        ResamplerChoice,
    };
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

    fn rendered_symbol_x(
        terminal: &Terminal<TestBackend>,
        width: u16,
        y: u16,
        symbol: &str,
    ) -> u16 {
        (0..width)
            .find(|&x| terminal.backend().buffer().get(x, y).symbol() == symbol)
            .unwrap_or_else(|| panic!("rendered symbol {symbol:?} not found on row {y}"))
    }

    fn render_deemphasis_info_case(
        app: &mut AppState,
        width: u16,
        height: u16,
    ) -> Terminal<TestBackend> {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::Format;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
        app.button_map.clear();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), app, theme))
            .expect("draw de-emphasis info case");
        terminal
    }

    #[test]
    fn dc_info_hitbox_four_pills_and_readable_notice_at_terminal_widths() {
        use crate::tui::app::DcCorrectionMode;
        for width in [44u16, 52, 66, 96, 120] {
            for mode in [DcCorrectionMode::Off, DcCorrectionMode::AutoShift,
                DcCorrectionMode::HighPass, DcCorrectionMode::Manual] {
                let height = 50;
                let mut app = AppState::new_for_test(TonepoetConfig::default());
                assert!(app.convert.format.dc_correction.select_value(&mode));
                let terminal = render_deemphasis_info_case(&mut app, width, height);
                let row = rendered_row(&terminal, width, height, "DC offset");
                let glyph_x = rendered_symbol_x(&terminal, width, row, "ⓘ");
                let rect = app.button_map.find_button_rect(&TuiButton::DcInfo)
                    .expect("DC info must be a real hitbox");
                assert_eq!((rect.x, rect.y, rect.width), (glyph_x, row, 2));
                assert_eq!(app.button_map.find_button_at(glyph_x, row), Some(TuiButton::DcInfo));
                assert_eq!(app.button_map.find_button_at(glyph_x + 1, row), Some(TuiButton::DcInfo));
                for pill in 0..4 {
                    let control = TuiButton::RemoveDcPill(pill);
                    let rect = app.button_map.find_button_rect(&control)
                        .expect("each visible DC pill has an independent hitbox");
                    assert_eq!(rect.y, row);
                    assert!(rect.x + rect.width <= glyph_x);
                    assert_eq!(app.button_map.find_button_at(rect.x, row), Some(control));
                }
                let (tx, _rx) = tokio::sync::mpsc::channel(1);
                crate::tui::keybindings::handle_mouse(&mut app, MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: glyph_x, row, modifiers: KeyModifiers::NONE,
                }, &tx);
                assert!(matches!(app.active_overlay,
                    ActiveOverlay::Notice { ref title, ref message, scroll: 0 }
                        if title == "DC offset correction"
                        && message == crate::tui::keybindings::DC_CORRECTION_INFO_MESSAGE));
                if width == 96 && mode == DcCorrectionMode::Off {
                    let mut notice_terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    notice_terminal.draw(|frame|
                        crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
                    let displayed: String = (0..height)
                        .map(|y| row_text(&notice_terminal, y, width)).collect();
                    assert!(displayed.contains("DC offset correction"));
                    assert!(displayed.contains("Recommended unless"));
                    let tall_max_scroll = crate::tui::draw_overlays::notice_message_max_scroll(
                        ratatui::layout::Rect::new(0, 0, width, height),
                        crate::tui::keybindings::DC_CORRECTION_INFO_MESSAGE);
                    if tall_max_scroll == 0 {
                        assert!(displayed.contains("This setting does not fix hum"),
                            "a notice that fits must show its ending without scrolling");
                    } else {
                        // If help copy grows, it is still valid for this tall
                        // terminal to scroll; the last paragraph must remain reachable.
                        if let ActiveOverlay::Notice { scroll, .. } = &mut app.active_overlay {
                            *scroll = tall_max_scroll;
                        }
                        notice_terminal.draw(|frame|
                            crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
                        let final_page: String = (0..height)
                            .map(|y| row_text(&notice_terminal, y, width)).collect();
                        assert!(final_page.contains("This setting does not fix hum"));
                    }
                    // Scrolling is required only when the same content exceeds
                    // the visible body height. Exercise the actual overlay at
                    // a short terminal size, including its final paragraph.
                    let short_height = 24;
                    let max_scroll = crate::tui::draw_overlays::notice_message_max_scroll(
                        ratatui::layout::Rect::new(0, 0, width, short_height),
                        crate::tui::keybindings::DC_CORRECTION_INFO_MESSAGE);
                    assert!(max_scroll > 0, "short terminals must scroll");
                    if let ActiveOverlay::Notice { scroll, .. } = &mut app.active_overlay {
                        *scroll = max_scroll;
                    }
                    let mut short_terminal = Terminal::new(
                        TestBackend::new(width, short_height)).unwrap();
                    short_terminal.draw(|frame|
                        crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
                    let final_page: String = (0..short_height)
                        .map(|y| row_text(&short_terminal, y, width)).collect();
                    assert!(final_page.contains("This setting does not fix hum"),
                        "scrolling to the bottom must expose the last paragraph");
                }
            }
        }
    }

    #[test]
    fn dc_manual_edit_row_only_appears_in_manual_mode() {
        use crate::tui::app::DcCorrectionMode;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let _ = render_deemphasis_info_case(&mut app, 100, 50);
        assert!(app.button_map.find_button_rect(&TuiButton::DcManualValueField).is_none());
        assert!(app.convert.format.dc_correction.select_value(&DcCorrectionMode::Manual));
        let terminal = render_deemphasis_info_case(&mut app, 100, 50);
        let row = rendered_row(&terminal, 100, 50, "DC amount");
        let rect = app.button_map.find_button_rect(&TuiButton::DcManualValueField).unwrap();
        assert_eq!(rect.y, row);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        crate::tui::keybindings::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x, row, modifiers: KeyModifiers::NONE,
        }, &tx);
        assert_eq!(app.convert.format.true_peak_target_editing, Some(FormatField::DcManualValue));
    }

    #[test]
    fn catalog_deemphasis_info_hitbox_tracks_rendered_icon_and_opens_notice() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.convert.format.deemphasis_eligible = true;
        app.convert.format.deemphasis_evidence = ConvertDeemphasisEvidence::CatalogExact;
        app.convert.format.deemphasis_enabled = false;
        app.convert.format.deemphasis_overridden = false;

        let terminal = render_deemphasis_info_case(&mut app, WIDTH, HEIGHT);
        let row = rendered_row(
            &terminal,
            WIDTH,
            HEIGHT,
            "this pressing is known pre-emphasised",
        );
        let icon_x = rendered_symbol_x(&terminal, WIDTH, row, "ⓘ");
        assert_eq!(
            app.button_map.find_button_at(icon_x, row),
            Some(TuiButton::DeemphasisInfo),
            "the visible catalog advisory icon must own its screen cell",
        );
        assert_eq!(
            app.button_map.find_button_at(icon_x + 1, row),
            Some(TuiButton::DeemphasisInfo),
            "the same-style guard cell must remain part of the information hitbox",
        );
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DeemphasisPill(0))
            .is_some());
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DeemphasisPill(1))
            .is_some());

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        crate::tui::keybindings::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: icon_x,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Notice { ref title, ref message, .. }
                if title == "CD pre-emphasis"
                    && message == crate::tui::keybindings::DEEMPHASIS_INFO_MESSAGE
        ));
    }

    #[test]
    fn cue_flag_deemphasis_info_hitbox_tracks_rendered_icon_and_opens_notice() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.convert.format.deemphasis_eligible = true;
        app.convert.format.deemphasis_evidence = ConvertDeemphasisEvidence::CueFlag;
        app.convert.format.deemphasis_enabled = false;
        app.convert.format.deemphasis_overridden = false;

        let terminal = render_deemphasis_info_case(&mut app, WIDTH, HEIGHT);
        let row = rendered_row(
            &terminal,
            WIDTH,
            HEIGHT,
            "pre-emphasis flagged in the CUE",
        );
        let icon_x = rendered_symbol_x(&terminal, WIDTH, row, "ⓘ");
        assert_eq!(
            app.button_map.find_button_at(icon_x, row),
            Some(TuiButton::DeemphasisInfo),
            "the visible CUE advisory icon must own its screen cell",
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        crate::tui::keybindings::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: icon_x,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Notice { ref title, ref message, .. }
                if title == "CD pre-emphasis"
                    && message == crate::tui::keybindings::DEEMPHASIS_INFO_MESSAGE
        ));
    }

    #[test]
    fn explicit_tag_off_info_hitbox_tracks_rendered_icon_and_opens_notice() {
        const WIDTH: u16 = 140;
        const HEIGHT: u16 = 48;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.convert.format.deemphasis_eligible = true;
        app.convert.format.deemphasis_evidence = ConvertDeemphasisEvidence::ExplicitTag;
        app.convert.format.deemphasis_enabled = false;
        app.convert.format.deemphasis_overridden = true;

        let terminal = render_deemphasis_info_case(&mut app, WIDTH, HEIGHT);
        let row = rendered_row(
            &terminal,
            WIDTH,
            HEIGHT,
            "pre-emphasis detected: turn on to correct it.",
        );
        assert!(
            !row_text(&terminal, row, WIDTH).contains('⚠'),
            "R23 keeps the information glyph but drops the warning glyph",
        );
        let icon_x = rendered_symbol_x(&terminal, WIDTH, row, "ⓘ");
        assert_eq!(
            app.button_map.find_button_at(icon_x, row),
            Some(TuiButton::DeemphasisInfo),
            "the visible explicit-tag information icon must own its screen cell",
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        crate::tui::keybindings::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: icon_x,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Notice { ref title, ref message, .. }
                if title == "CD pre-emphasis"
                    && message == crate::tui::keybindings::DEEMPHASIS_INFO_MESSAGE
        ));
    }

    #[test]
    fn deemphasis_on_uses_corrected_guidance_and_keeps_info_popup_reachable() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.convert.format.deemphasis_eligible = true;
        app.convert.format.deemphasis_evidence = ConvertDeemphasisEvidence::ExplicitTag;
        app.convert.format.deemphasis_enabled = true;
        app.convert.format.deemphasis_overridden = false;

        let terminal = render_deemphasis_info_case(&mut app, WIDTH, HEIGHT);
        let row = rendered_row(
            &terminal,
            WIDTH,
            HEIGHT,
            "pre-emphasis will be corrected.",
        );
        let rendered = row_text(&terminal, row, WIDTH);
        assert!(
            !rendered.contains("turn on"),
            "an enabled control must not tell the user to enable it",
        );
        let icon_x = rendered_symbol_x(&terminal, WIDTH, row, "ⓘ");
        assert_eq!(
            app.button_map.find_button_at(icon_x, row),
            Some(TuiButton::DeemphasisInfo),
            "the information popup remains reachable while De-emphasis is On",
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        crate::tui::keybindings::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: icon_x,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Notice { ref title, ref message, .. }
                if title == "CD pre-emphasis"
                    && message == crate::tui::keybindings::DEEMPHASIS_INFO_MESSAGE
        ));
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
        assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::TruePeakGuard));
        app.convert.format.resample_quality = ResampleQuality::Ultra;
        app.convert.format.apply_format_constraints();

        let backend = TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("terminal");
        app.button_map.clear();
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw DSD-to-PCM format pane");

        let gain_scope_y = rendered_row(&terminal, WIDTH, HEIGHT, "TP scope");
        let true_peak_target_y = rendered_row(&terminal, WIDTH, HEIGHT, "TP target");
        let container_y = rendered_row(&terminal, WIDTH, HEIGHT, "container");
        let resample_quality_y = rendered_row(&terminal, WIDTH, HEIGHT, "insane");
        let gain_y = rendered_row(&terminal, WIDTH, HEIGHT, "DSD gain");

        let album_scope_rect = app
            .button_map
            .find_button_rect(&TuiButton::DsdTruePeakScopePill(1))
            .expect("gain-scope album pill hit region");
        assert_eq!(album_scope_rect.y, gain_scope_y);
        assert_eq!(
            app.button_map.find_button_at(album_scope_rect.x, album_scope_rect.y),
            Some(TuiButton::DsdTruePeakScopePill(1)),
        );
        assert!(crate::tui::format_interactions::handle_convert_format_button(
            &mut app.convert,
            TuiButton::DsdTruePeakScopePill(1),
        ));
        assert_eq!(app.convert.format.dsd_gain_mode.selected_value(), &DsdGainMode::TruePeakGuard);
        assert_eq!(app.convert.format.dsd_true_peak_scope.selected, 1);
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::DsdTruePeakTargetField)
                .expect("true-peak target hit region")
                .y,
            true_peak_target_y,
        );
        assert!(
            app.button_map.find_button_rect(&TuiButton::DsdGainDbField).is_none(),
            "fixed-gain row is inactive under certified true-peak policy and must not render or receive a hit target"
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
        assert!(gain_text.contains("off"));
        assert!(gain_text.contains("true-peak guard"));
        assert!(gain_text.contains("true-peak normalize"));
        assert!(gain_text.contains("fixed gain"));
        assert!(!gain_text.contains("auto"));

        let keyboard_rows = app.convert.format.visible_fields(true);
        assert!(keyboard_rows.contains(&FormatField::DsdTruePeakTarget));
        assert!(keyboard_rows.contains(&FormatField::DsdTruePeakScope));
        assert!(keyboard_rows.contains(&FormatField::DsdTruePeakScan));
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

        assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::FixedGain));
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
        assert!(!all_text.contains("TP scope"));
        assert!(!all_text.contains("TP target"));
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DsdTruePeakScopePill(0))
            .is_none());
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::DsdTruePeakTargetField)
            .is_none());
    }

    #[test]
    fn r30_pcm_and_dsd_custom_pills_have_real_hit_targets_and_inline_inputs() {
        use crossterm::event::{KeyCode, KeyEvent};
        use tonepoet_pipeline::{DbNano, TruePeakTargetOrigin};

        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        for dsd in [false, true] {
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Convert;
            app.convert.focus = ConvertFocus::Format;
            app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
            app.convert.format.set_source_is_dsd(dsd);
            assert!(app.convert.format.format.select_value(&AudioFormat::Flac));
            app.convert.format.apply_format_constraints();
            let (field, pill) = if dsd {
                assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::TruePeakGuard));
                (FormatField::DsdTruePeakTarget, TuiButton::DsdTruePeakCustomPill)
            } else {
                assert!(app.convert.format.pcm_gain_mode.select_value(&crate::tui::app::PcmGainMode::TruePeakGuard));
                (FormatField::PcmTruePeakTarget, TuiButton::PcmTruePeakCustomPill)
            };

            let backend = TestBackend::new(WIDTH, HEIGHT);
            let mut terminal = Terminal::new(backend).expect("terminal");
            app.button_map.clear();
            terminal.draw(|f| draw_convert_screen(f, f.size(), &mut app, theme))
                .expect("draw true-peak target");
            let y = rendered_row(&terminal, WIDTH, HEIGHT, "TP target");
            let rendered = row_text(&terminal, y, WIDTH);
            assert!(rendered.contains("-0.10 dBTP"), "default target must display at least two decimals: {rendered}");
            assert!(rendered.contains("custom"), "custom must be a visible pill: {rendered}");
            let rect = app.button_map.find_button_rect(&pill).expect("custom pill hit region");
            assert_eq!(rect.y, y);
            assert_eq!(rect.width, 8);
            assert_eq!(app.button_map.find_button_at(rect.x + 2, y), Some(pill));

            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            crate::tui::keybindings::handle_mouse(&mut app, MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 2, row: y, modifiers: KeyModifiers::NONE,
            }, &tx);
            assert_eq!(app.convert.format.true_peak_target_editing, Some(field));
            app.button_map.clear();
            terminal.draw(|f| draw_convert_screen(f, f.size(), &mut app, theme))
                .expect("draw active inline editor");
            assert!(row_text(&terminal, y, WIDTH).contains("custom"));
            assert!(app.button_map.find_button_rect(&pill).is_some(), "pill remains visible during editing");

            for ch in "0.0001".chars() {
                crate::tui::keybindings::handle_key(&mut app, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &tx);
            }
            crate::tui::keybindings::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
            assert_eq!(app.convert.format.true_peak_target_editing, None);
            let (value, origin) = if dsd {
                (app.convert.format.dsd_true_peak_target_dbtp, app.convert.format.dsd_true_peak_target_origin)
            } else {
                (app.convert.format.pcm_true_peak_target_dbtp, app.convert.format.pcm_true_peak_target_origin)
            };
            assert_eq!(value, DbNano(-100_000));
            assert_eq!(origin, TruePeakTargetOrigin::User);
        }
    }

    #[test]
    fn r32_pcm_and_dsd_true_peak_info_clicks_open_shared_notice_without_editing() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let theme = crate::tui::theme::theme_by_slug(
            crate::tui::theme::default_theme_slug(),
        )
        .expect("default theme");
        for dsd in [false, true] {
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Convert;
            app.convert.focus = ConvertFocus::Format;
            app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
            app.convert.format.set_source_is_dsd(dsd);
            assert!(app.convert.format.format.select_value(&AudioFormat::Flac));
            app.convert.format.apply_format_constraints();
            let target_field = if dsd {
                assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::TruePeakGuard));
                TuiButton::DsdTruePeakTargetField
            } else {
                assert!(app.convert.format.pcm_gain_mode.select_value(
                    &crate::tui::app::PcmGainMode::TruePeakGuard,
                ));
                TuiButton::PcmTruePeakTargetField
            };
            let initial_target = if dsd {
                app.convert.format.dsd_true_peak_target_dbtp
            } else {
                app.convert.format.pcm_true_peak_target_dbtp
            };
            let backend = TestBackend::new(WIDTH, HEIGHT);
            let mut terminal = Terminal::new(backend).expect("terminal");
            app.button_map.clear();
            terminal
                .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
                .expect("draw true-peak information row");

            let y = rendered_row(&terminal, WIDTH, HEIGHT, "TP target");
            let icon_x = rendered_symbol_x(&terminal, WIDTH, y, "\u{24d8}");
            let rendered = row_text(&terminal, y, WIDTH);
            assert!(!rendered.contains("0.25 dB steps"), "{rendered}");
            assert!(!rendered.contains(":set"), "{rendered}");
            let info_rect = app.button_map.find_button_rect(&TuiButton::TruePeakInfo)
                .expect("visible icon must have a hitbox");
            assert_eq!((info_rect.x, info_rect.y, info_rect.width), (icon_x, y, 2));
            for x in icon_x..icon_x + 2 {
                assert_eq!(app.button_map.find_button_at(x, y), Some(TuiButton::TruePeakInfo));
            }
            assert_eq!(app.button_map.find_button_at(icon_x + 2, y), Some(target_field));

            let (tx, _rx) = tokio::sync::mpsc::channel(1);
            crate::tui::keybindings::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: icon_x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
            assert!(matches!(
                app.active_overlay,
                ActiveOverlay::Notice { ref title, ref message, scroll }
                    if title == "True peak"
                        && message == crate::tui::keybindings::TRUE_PEAK_INFO_MESSAGE
                        && scroll == 0
            ));
            assert_eq!(app.convert.format.true_peak_target_editing, None);
            let target_after = if dsd {
                app.convert.format.dsd_true_peak_target_dbtp
            } else {
                app.convert.format.pcm_true_peak_target_dbtp
            };
            assert_eq!(target_after, initial_target, "help must not mutate the gain target");
        }
    }

    #[test]
    fn r32_true_peak_help_is_not_registered_when_target_row_is_hidden() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 48;
        let theme = crate::tui::theme::theme_by_slug(
            crate::tui::theme::default_theme_slug(),
        )
        .expect("default theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::Format;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
        app.convert.format.set_source_is_dsd(true);
        assert!(app.convert.format.format.select_value(&AudioFormat::Flac));
        assert!(app.convert.format.dsd_gain_mode.select_value(&DsdGainMode::FixedGain));
        app.convert.format.apply_format_constraints();
        let backend = TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("terminal");
        app.button_map.clear();
        terminal
            .draw(|frame| draw_convert_screen(frame, frame.size(), &mut app, theme))
            .expect("draw fixed-gain mode");
        assert!(app.button_map.find_button_rect(&TuiButton::TruePeakInfo).is_none());
        let all_text: String = (0..HEIGHT)
            .map(|y| row_text(&terminal, y, WIDTH))
            .collect();
        assert!(!all_text.contains("TP target"));
    }
}

#[cfg(test)]
mod pcm_gain_info_regressions {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{ActiveOverlay, AppScreen, PcmGainMode};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{backend::TestBackend, Terminal};

    fn render(app: &mut AppState, width: u16, height: u16) -> Terminal<TestBackend> {
        let theme = crate::tui::theme::theme_by_slug(
            crate::tui::theme::default_theme_slug(),
        ).expect("default theme");
        app.current_screen = AppScreen::Convert;
        app.convert.focus = ConvertFocus::Format;
        app.convert.layout = ConvertLayout::Maximized(ConvertFocus::Format);
        app.button_map.clear();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| draw_convert_screen(frame, frame.size(), app, theme))
            .expect("draw maximized Format pane");
        terminal
    }

    fn rendered_text(terminal: &Terminal<TestBackend>, width: u16, y: u16) -> String {
        (0..width).map(|x| terminal.backend().buffer().get(x, y).symbol().to_string())
            .collect()
    }

    #[test]
    fn pcm_gain_info_is_visible_clickable_and_does_not_overlap_pills_at_narrow_widths() {
        for width in [44u16, 51, 52, 63, 64, 80, 83, 84, 100, 120] {
            for mode in [PcmGainMode::Off, PcmGainMode::TruePeakGuard,
                PcmGainMode::TruePeakNormalize, PcmGainMode::FixedGain] {
                let height = 50;
                let mut app = AppState::new_for_test(TonepoetConfig::default());
                assert!(app.convert.format.pcm_gain_mode.select_value(&mode));
                app.convert.format.field_focus = FormatField::PcmTruePeak;
                let terminal = render(&mut app, width, height);
                let row = (0..height).find(|&y|
                    rendered_text(&terminal, width, y).contains("PCM gain"))
                    .expect("PCM gain row visible in maximized Format pane");
                let glyph_x = (0..width).find(|&x|
                    terminal.backend().buffer().get(x, row).symbol() == "ⓘ")
                    .expect("literal U+24D8 drawn even when pills extend beyond pane");
                let info_rect = app.button_map.find_button_rect(&TuiButton::PcmGainInfo)
                    .expect("visible PCM gain information hitbox");
                assert_eq!((info_rect.x, info_rect.y, info_rect.width), (glyph_x, row, 2));
                assert!(glyph_x > 17, "the information control belongs after the pills");
                for x in glyph_x..glyph_x + 2 {
                    assert_eq!(app.button_map.find_button_at(x, row), Some(TuiButton::PcmGainInfo));
                }
                for index in 0..4 {
                    let pill = TuiButton::PcmTruePeakPill(index);
                    let rect = app.button_map.find_button_rect(&pill).expect("original gain pill");
                    assert_eq!(rect.y, row);
                    assert!(rect.x + rect.width + 2 <= glyph_x,
                        "all four selectable pills must precede the information control");
                    assert_eq!(app.button_map.find_button_at(rect.x, row), Some(pill));
                }
                let last_pill = app.button_map.find_button_rect(
                    &TuiButton::PcmTruePeakPill(3)).unwrap();
                assert_eq!(glyph_x, last_pill.x + last_pill.width + 2,
                    "the suffix follows the final pill by the common two-cell gap");
                let prior_gain = *app.convert.format.pcm_gain_mode.selected_value();
                let prior_db = app.convert.format.pcm_fixed_gain_db;
                let prior_ceiling = app.convert.format.pcm_true_peak_target_dbtp;
                let prior_override = app.convert.format.pcm_gain_overridden;
                let (tx, _rx) = tokio::sync::mpsc::channel(8);
                crate::tui::keybindings::handle_mouse(&mut app, MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left), column: glyph_x + 1,
                    row, modifiers: KeyModifiers::NONE,
                }, &tx);
                assert!(matches!(&app.active_overlay, ActiveOverlay::Notice { title, message, scroll }
                    if title == "PCM gain"
                    && message == crate::tui::keybindings::PCM_GAIN_INFO_MESSAGE
                    && *scroll == 0));
                assert_eq!(*app.convert.format.pcm_gain_mode.selected_value(), prior_gain);
                assert_eq!(app.convert.format.pcm_fixed_gain_db, prior_db);
                assert_eq!(app.convert.format.pcm_true_peak_target_dbtp, prior_ceiling);
                assert_eq!(app.convert.format.pcm_gain_overridden, prior_override);
                crate::tui::keybindings::handle_key(&mut app,
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
                assert!(matches!(app.active_overlay, ActiveOverlay::None));
                assert_eq!(app.convert.format.field_focus, FormatField::PcmTruePeak);
                crate::tui::keybindings::handle_key(&mut app,
                    KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &tx);
                assert!(matches!(app.active_overlay, ActiveOverlay::Notice { .. }),
                    "focused PCM gain row also opens help by keyboard");
                crate::tui::keybindings::handle_key(&mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
                assert!(matches!(app.active_overlay, ActiveOverlay::None));
                assert_eq!(*app.convert.format.pcm_gain_mode.selected_value(), prior_gain);
            }
        }
    }

    #[test]
    fn pcm_gain_notice_is_readable_and_scrolls_to_fixed_gain_without_markdown() {
        const WIDTH: u16 = 80;
        const HEIGHT: u16 = 24;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.convert.format.field_focus = FormatField::PcmTruePeak;
        // Render the larger Format screen first, then use the actual Notice overlay renderer.
        let _ = render(&mut app, WIDTH, 50);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        crate::tui::keybindings::handle_key(&mut app,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), &tx);
        let message = crate::tui::keybindings::PCM_GAIN_INFO_MESSAGE;
        let headings = ["Off\n", "True-peak guard\n", "True-peak normalize\n", "Fixed gain\n"];
        let positions = headings.map(|heading| message.find(heading).expect("help heading"));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]),
            "all four headings must appear in the correct order");
        assert_eq!(message.matches("Choose this when").count(), 4);
        assert!(!message.contains("**"), "Notice uses plain-text headings, not Markdown");
        let max_scroll = crate::tui::draw_overlays::notice_message_max_scroll(
            Rect::new(0, 0, WIDTH, HEIGHT), message);
        assert!(max_scroll > 0, "the long help must scroll on a standard terminal");
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal.draw(|frame| crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
        let first_page = (0..HEIGHT)
            .map(|y| rendered_text(&terminal, WIDTH, y)).collect::<String>();
        assert!(first_page.contains("PCM gain"));
        assert!(first_page.contains("Controls whether"));
        // Scroll to the end through the same persistent Notice state consumed by the renderer.
        if let ActiveOverlay::Notice { scroll, .. } = &mut app.active_overlay {
            *scroll = max_scroll;
        } else {
            panic!("information popup must still be open");
        }
        terminal.draw(|frame| crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
        let last_page = (0..HEIGHT)
            .map(|y| rendered_text(&terminal, WIDTH, y)).collect::<String>();
        assert!(last_page.contains("Fixed gain"));
        assert!(last_page.contains("clipping"));
        assert!(app.button_map.find_button_rect(&TuiButton::OverlayCancel).is_some());
        crate::tui::keybindings::handle_key(&mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(app.convert.format.field_focus, FormatField::PcmTruePeak);
    }
}

//! Main convert screen layout: header + preset bar + source/metadata/format/output options + footer

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppState, ConvertFocus};
use super::button_map::{ButtonRenderMap, MetadataFieldKind, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::draw_metadata::draw_metadata_pane;
use super::draw_output::draw_format_pane;
use super::draw_output_options::draw_output_options_pane;
use super::draw_preset_bar::draw_preset_bar;
use super::draw_source::draw_source_pane;
use super::pill::PillState;
use super::theme;

/// Draw the full convert screen
pub fn draw_convert_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),  // header (ASCII art box)
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // preset bar
            Constraint::Length(1),  // blank
            Constraint::Length(6),  // source pane (path + format + duration + browse pill)
            Constraint::Length(5),  // metadata pane
            Constraint::Length(10), // format pane
            Constraint::Length(7),  // output options pane
            Constraint::Min(0),     // absorb extra vertical space
            Constraint::Length(1),  // convert action bar
            Constraint::Length(2),  // footer (tabs + context)
        ])
        .split(area);

    // Pass 1: Draw everything (immutable reads from app state)
    draw_header(f, chunks[0]);
    {
        let preset = &app.preset;
        let buttons = &mut app.button_map;
        draw_preset_bar(f, chunks[2], preset, buttons);
    }
    draw_source_pane(
        f,
        chunks[4],
        &app.convert.source,
        app.convert.focus == ConvertFocus::Source,
    );
    draw_metadata_pane(
        f,
        chunks[5],
        &app.convert.metadata,
        app.convert.focus == ConvertFocus::Metadata,
    );
    draw_format_pane(
        f,
        chunks[6],
        &app.convert.format,
        app.convert.focus == ConvertFocus::Format,
    );
    draw_output_options_pane(
        f,
        chunks[7],
        &app.convert.output_options,
        app.convert.focus == ConvertFocus::OutputOptions,
    );
    draw_convert_action_bar(f, chunks[9], &mut app.button_map);
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[10],
        app.current_screen,
        &mut app.button_map,
        status_msg,
    );

    // Pass 2: Register mouse button areas (mutable access to button_map)
    register_buttons(app, &chunks);
}

/// Register all clickable areas for mouse support.
/// Uses split borrows: reads from convert state + writes to button_map.
fn register_buttons(app: &mut AppState, chunks: &[Rect]) {
    let source_area = chunks[4];
    let metadata_area = chunks[5];
    let format_area = chunks[6];
    let output_options_area = chunks[7];

    // Pane focus areas
    app.button_map
        .record_button(TuiButton::Pane(ConvertFocus::Source), source_area);
    app.button_map
        .record_button(TuiButton::Pane(ConvertFocus::Metadata), metadata_area);
    app.button_map
        .record_button(TuiButton::Pane(ConvertFocus::Format), format_area);
    app.button_map.record_button(
        TuiButton::Pane(ConvertFocus::OutputOptions),
        output_options_area,
    );

    // Format pane pill buttons
    // Layout within format pane (10 lines):
    //   y+0: top border
    //   y+1: blank
    //   y+2: format pills
    //   y+3: blank
    //   y+4: sample rate pills
    //   y+5: bit depth pills
    //   y+6: dither pills
    //   y+7: replaygain pills
    //   y+8: blank
    //   y+9: bottom border
    {
        let state = &app.convert.format;
        let buttons = &mut app.button_map;
        let label_col = format_area.x + 17; // "│" + "   " + 11-char label + "  " = 17

        register_pill_row(
            buttons,
            &state.format,
            format_area.y + 2,
            label_col,
            format_area.width,
            |i| TuiButton::FormatPill(i),
        );
        register_pill_row(
            buttons,
            &state.sample_rate,
            format_area.y + 4,
            label_col,
            format_area.width,
            |i| TuiButton::RatePill(i),
        );
        register_pill_row(
            buttons,
            &state.bit_depth,
            format_area.y + 5,
            label_col,
            format_area.width,
            |i| TuiButton::DepthPill(i),
        );
        register_pill_row(
            buttons,
            &state.dither,
            format_area.y + 6,
            label_col,
            format_area.width,
            |i| TuiButton::DitherPill(i),
        );
        register_pill_row(
            buttons,
            &state.replaygain,
            format_area.y + 7,
            label_col,
            format_area.width,
            |i| TuiButton::ReplayGainPill(i),
        );
    }

    // Output options pane pill buttons
    // Layout within output options pane (7 lines):
    //   y+0: top border
    //   y+1: dest
    //   y+2: folder
    //   y+3: filename
    //   y+4: merge pills
    //   y+5: est. size
    //   y+6: bottom border
    {
        let state = &app.convert.output_options;
        let buttons = &mut app.button_map;
        let label_col = output_options_area.x + 17;

        register_pill_row(
            buttons,
            &state.merge,
            output_options_area.y + 4,
            label_col,
            output_options_area.width,
            |i| TuiButton::MergePill(i),
        );
    }

    // Tab bar buttons are registered by draw_footer() itself.

    // Advanced toggle buttons (top border of each pane)
    // The "a dvanced" text spans 8 chars at area.x + area.width - 10
    register_advanced_toggle(&mut app.button_map, source_area, ConvertFocus::Source);
    register_advanced_toggle(&mut app.button_map, metadata_area, ConvertFocus::Metadata);
    register_advanced_toggle(&mut app.button_map, format_area, ConvertFocus::Format);
    register_advanced_toggle(
        &mut app.button_map,
        output_options_area,
        ConvertFocus::OutputOptions,
    );

    // Output options editable text fields (rows 1-3 of the pane)
    {
        let buttons = &mut app.button_map;
        let inner_x = output_options_area.x + 1;
        let inner_w = output_options_area.width.saturating_sub(2);
        buttons.record_button(
            TuiButton::DestPathField,
            Rect::new(inner_x, output_options_area.y + 1, inner_w, 1),
        );
        // Template text fields (clickable area excludes the pills on the right)
        let pill_zone = 6 + 1 + 7 + 1; // " load " + gap + " build " + border
        let text_w = inner_w.saturating_sub(pill_zone);
        buttons.record_button(
            TuiButton::FolderTemplateField,
            Rect::new(inner_x, output_options_area.y + 2, text_w, 1),
        );
        buttons.record_button(
            TuiButton::FilenameTemplateField,
            Rect::new(inner_x, output_options_area.y + 3, text_w, 1),
        );
        // [load] and [build] pills at the right edge of folder/filename rows
        let load_x = output_options_area.x + output_options_area.width - pill_zone;
        buttons.record_button(
            TuiButton::TemplateLoadFolderButton,
            Rect::new(load_x, output_options_area.y + 2, 6, 1),
        );
        buttons.record_button(
            TuiButton::TemplateBuildFolderButton,
            Rect::new(load_x + 7, output_options_area.y + 2, 7, 1),
        );
        buttons.record_button(
            TuiButton::TemplateLoadFilenameButton,
            Rect::new(load_x, output_options_area.y + 3, 6, 1),
        );
        buttons.record_button(
            TuiButton::TemplateBuildFilenameButton,
            Rect::new(load_x + 7, output_options_area.y + 3, 7, 1),
        );
    }

    // Source pane pills (row 4 of the pane, right-aligned with 3-space
    // margin). Primary pill: browse/expand. Secondary: analyze (when loaded).
    {
        let buttons = &mut app.button_map;
        let inner_w = source_area.width.saturating_sub(2);
        let in_batch = app.convert.source.mode.is_batch();
        let has_source = !app.convert.source.mode.is_empty();
        let (button, label) = if in_batch {
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
        if inner_w as u16 >= pill_label_w + right_margin {
            let pill_x = source_area.x + 1 + (inner_w as u16 - pill_label_w - right_margin);
            buttons.record_button(
                button,
                Rect::new(pill_x, source_area.y + 4, pill_label_w, 1),
            );

            // Register analyze pill to the left of the primary pill.
            if has_source {
                let analyze_w = super::draw_source::ANALYZE_PILL_LABEL.chars().count() as u16;
                let gap = 2u16;
                let analyze_x = pill_x.saturating_sub(analyze_w + gap);
                if analyze_x > source_area.x + 1 {
                    buttons.record_button(
                        TuiButton::SourceAnalyzeButton,
                        Rect::new(analyze_x, source_area.y + 4, analyze_w, 1),
                    );
                }

                // Enqueue pill row (row 5 of the pane).
                let enq_start_w =
                    super::draw_source::ENQUEUE_START_PILL_LABEL.chars().count() as u16;
                let enq_w = super::draw_source::ENQUEUE_PILL_LABEL.chars().count() as u16;
                let enq_start_x =
                    source_area.x + 1 + (inner_w as u16).saturating_sub(enq_start_w + right_margin);
                let enq_x = enq_start_x.saturating_sub(enq_w + gap);
                let enq_row = source_area.y + 5;
                if enq_row < source_area.y + source_area.height {
                    buttons.record_button(
                        TuiButton::SourceEnqueueStartButton,
                        Rect::new(enq_start_x, enq_row, enq_start_w, 1),
                    );
                    if enq_x > source_area.x + 1 {
                        buttons.record_button(
                            TuiButton::SourceEnqueueButton,
                            Rect::new(enq_x, enq_row, enq_w, 1),
                        );
                    }
                }
            }
        }
    }

    // Metadata editable fields (rows 1-3 of the pane)
    // Row 1: title (full width)
    // Row 2: artist (left half) + album (right half)
    // Row 3: genre (left half) + year (right half)
    {
        let buttons = &mut app.button_map;
        let inner_x = metadata_area.x + 1;
        let inner_w = metadata_area.width.saturating_sub(2);
        let half_w = inner_w / 2;

        // Row 1: title (full width)
        buttons.record_button(
            TuiButton::MetadataField(MetadataFieldKind::Title),
            Rect::new(inner_x, metadata_area.y + 1, inner_w, 1),
        );
        // Row 2: artist (left half), album (right half)
        buttons.record_button(
            TuiButton::MetadataField(MetadataFieldKind::Artist),
            Rect::new(inner_x, metadata_area.y + 2, half_w, 1),
        );
        buttons.record_button(
            TuiButton::MetadataField(MetadataFieldKind::Album),
            Rect::new(inner_x + half_w, metadata_area.y + 2, inner_w - half_w, 1),
        );
        // Row 3: genre (left half), year (right half)
        buttons.record_button(
            TuiButton::MetadataField(MetadataFieldKind::Genre),
            Rect::new(inner_x, metadata_area.y + 3, half_w, 1),
        );
        buttons.record_button(
            TuiButton::MetadataField(MetadataFieldKind::Year),
            Rect::new(inner_x + half_w, metadata_area.y + 3, inner_w - half_w, 1),
        );
    }
}

/// Register an "advanced" toggle button at the top-right of a pane.
/// Text layout: "...─── a dvanced ┐"
/// Clickable "advanced" spans 8 chars starting at area.x + area.width - 10.
fn register_advanced_toggle(buttons: &mut ButtonRenderMap, pane_area: Rect, focus: ConvertFocus) {
    if pane_area.width < 12 {
        return;
    }
    let x = pane_area.x + pane_area.width - 10;
    let rect = Rect::new(x, pane_area.y, 8, 1);
    buttons.record_button(TuiButton::AdvancedToggle(focus), rect);
}

/// Register button areas for each pill in a row.
/// `start_x` is where the first pill begins (after the label).
fn register_pill_row<T: Clone>(
    buttons: &mut ButtonRenderMap,
    state: &PillState<T>,
    y: u16,
    start_x: u16,
    _max_width: u16,
    make_button: impl Fn(usize) -> TuiButton,
) {
    let mut x = start_x;
    for (i, opt) in state.options.iter().enumerate() {
        if i > 0 {
            x += 2; // gap between pills
        }
        let pill_width = opt.label.len() as u16 + 2; // " LABEL " padding
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
        + (pills.len().saturating_sub(1) as u16); // gaps

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

    let bar = Paragraph::new(Line::from(spans));
    f.render_widget(bar, area);
}

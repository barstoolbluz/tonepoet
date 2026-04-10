//! Main convert screen layout: header + preset bar + source/metadata/format/output options + footer

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

use super::app::{AppState, ConvertFocus};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::draw_header::draw_header;
use super::draw_preset_bar::draw_preset_bar;
use super::draw_source::draw_source_pane;
use super::draw_metadata::draw_metadata_pane;
use super::draw_output::draw_format_pane;
use super::draw_output_options::draw_output_options_pane;
use super::draw_footer::draw_footer;
use super::pill::PillState;

/// Draw the full convert screen
pub fn draw_convert_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),  // header (ASCII art box)
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // preset bar
            Constraint::Length(1),  // blank
            Constraint::Length(5),  // source pane (compact)
            Constraint::Length(5),  // metadata pane
            Constraint::Length(10), // format pane
            Constraint::Length(7),  // output options pane
            Constraint::Min(0),    // absorb extra vertical space
            Constraint::Length(2),  // footer (tabs + context)
        ])
        .split(area);

    // Pass 1: Draw everything (immutable reads from app state)
    draw_header(f, chunks[0]);
    draw_preset_bar(f, chunks[2], &app.preset);
    draw_source_pane(
        f, chunks[4], &app.convert.source,
        app.convert.focus == ConvertFocus::Source,
    );
    draw_metadata_pane(
        f, chunks[5], &app.convert.metadata,
        app.convert.focus == ConvertFocus::Metadata,
    );
    draw_format_pane(
        f, chunks[6], &app.convert.format,
        app.convert.focus == ConvertFocus::Format,
    );
    draw_output_options_pane(
        f, chunks[7], &app.convert.output_options,
        app.convert.focus == ConvertFocus::OutputOptions,
    );
    draw_footer(f, chunks[9], app.current_screen);

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
    let footer_area = chunks[9];

    // Pane focus areas
    app.button_map.record_button(TuiButton::Pane(ConvertFocus::Source), source_area);
    app.button_map.record_button(TuiButton::Pane(ConvertFocus::Metadata), metadata_area);
    app.button_map.record_button(TuiButton::Pane(ConvertFocus::Format), format_area);
    app.button_map.record_button(TuiButton::Pane(ConvertFocus::OutputOptions), output_options_area);

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

        register_pill_row(buttons, &state.format, format_area.y + 2, label_col, format_area.width, |i| TuiButton::FormatPill(i));
        register_pill_row(buttons, &state.sample_rate, format_area.y + 4, label_col, format_area.width, |i| TuiButton::RatePill(i));
        register_pill_row(buttons, &state.bit_depth, format_area.y + 5, label_col, format_area.width, |i| TuiButton::DepthPill(i));
        register_pill_row(buttons, &state.dither, format_area.y + 6, label_col, format_area.width, |i| TuiButton::DitherPill(i));
        register_pill_row(buttons, &state.replaygain, format_area.y + 7, label_col, format_area.width, |i| TuiButton::ReplayGainPill(i));
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

        register_pill_row(buttons, &state.merge, output_options_area.y + 4, label_col, output_options_area.width, |i| TuiButton::MergePill(i));
    }

    // Tab bar buttons (footer first row)
    {
        let tab_row_y = footer_area.y;
        let tab_count = 5u16;
        let tab_w = footer_area.width / tab_count;
        let buttons = &mut app.button_map;
        for i in 0..5 {
            let x = footer_area.x + i * tab_w;
            buttons.record_button(TuiButton::Tab(i as u8 + 1), Rect::new(x, tab_row_y, tab_w, 1));
        }
    }
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

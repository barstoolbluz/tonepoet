//! Browse screen: file browser with directory tree + info pane

use std::borrow::Cow;
use std::path::Path;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::{AppState, BrowseInfoFocus, BrowseInlineEditState, BrowseInlineEditTarget};
use super::browse::{BrowseColumn, BrowseDirectorySummaryColdWorkPolicy, BrowseEntry, BrowseOptionsMenu, BrowsePaneId, BrowseState, CachedInfo, EntryKind, FolderAudioSummary, FolderClassificationKind, FolderContentClassification, FormatFilter, SortBy, SortDir};
use super::button_map::{ButtonRenderMap, ScrollbarSurface, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::inline_edit::{
    inline_cursor_col, render_inline_value_with_embedded_cursor,
    render_text_input_value_with_style,
};
use super::probe::MetadataField;
use super::text_input::TextInputState;

/// Fixed column widths (inside the list border). Name is flexible.
const COL_SIZE_W: usize = 9;
const COL_DATE_W: usize = 12;
const COL_TYPE_W: usize = 8;
/// Prefix: cursor(2) + selection marker(1) + space(1).
const ROW_PREFIX: usize = 4;
const ROW_CURSOR_W: usize = 2;
const ROW_GUTTER_W: usize = 2;
/// Trailing space before the right border.
const ROW_TRAILING: usize = 2;
const MIN_NAME_W: usize = 8;
const COL_FORMAT_W: usize = 8;
const COL_CODEC_W: usize = 12;
const COL_SAMPLE_RATE_W: usize = 11;
const COL_CHANNELS_W: usize = 8;
const COL_DURATION_W: usize = 9;
const COL_ARTIST_W: usize = 16;
const COL_ALBUM_W: usize = 16;
const BROWSE_PATH_GO_LABEL: &str = " go ";
const BROWSE_PATH_GO_WIDTH: u16 = 5;

#[derive(Debug, Clone, Copy)]
struct BrowseColumnCell {
    column: BrowseColumn,
    width: usize,
}

fn column_fixed_width(column: BrowseColumn) -> usize {
    match column {
        BrowseColumn::Name => MIN_NAME_W,
        BrowseColumn::Size => COL_SIZE_W,
        BrowseColumn::Date => COL_DATE_W,
        BrowseColumn::Type => COL_TYPE_W,
        BrowseColumn::Format => COL_FORMAT_W,
        BrowseColumn::Codec => COL_CODEC_W,
        BrowseColumn::SampleRate => COL_SAMPLE_RATE_W,
        BrowseColumn::Channels => COL_CHANNELS_W,
        BrowseColumn::Duration => COL_DURATION_W,
        BrowseColumn::Artist => COL_ARTIST_W,
        BrowseColumn::Album => COL_ALBUM_W,
    }
}

fn column_right_aligned(column: BrowseColumn) -> bool {
    matches!(
        column,
        BrowseColumn::Size | BrowseColumn::SampleRate | BrowseColumn::Channels | BrowseColumn::Duration
    )
}

fn browse_column_layout(inner_width: usize, configured: &[BrowseColumn]) -> Vec<BrowseColumnCell> {
    let mut columns = Vec::new();
    for column in configured.iter().copied() {
        if !columns.contains(&column) {
            columns.push(column);
        }
    }
    if !columns.contains(&BrowseColumn::Name) {
        columns.insert(0, BrowseColumn::Name);
    }
    if columns.first() != Some(&BrowseColumn::Name) {
        columns.retain(|column| *column != BrowseColumn::Name);
        columns.insert(0, BrowseColumn::Name);
    }

    while columns.len() > 1 {
        let non_name_width: usize = columns
            .iter()
            .copied()
            .filter(|column| *column != BrowseColumn::Name)
            .map(column_fixed_width)
            .sum();
        let gaps = columns.len().saturating_sub(1);
        let needed = ROW_PREFIX + ROW_TRAILING + non_name_width + gaps + MIN_NAME_W;
        if needed <= inner_width {
            break;
        }
        columns.pop();
    }

    let non_name_width: usize = columns
        .iter()
        .copied()
        .filter(|column| *column != BrowseColumn::Name)
        .map(column_fixed_width)
        .sum();
    let gaps = columns.len().saturating_sub(1);
    let name_width = inner_width
        .saturating_sub(ROW_PREFIX + ROW_TRAILING + non_name_width + gaps)
        .max(1);

    columns
        .into_iter()
        .map(|column| BrowseColumnCell {
            column,
            width: if column == BrowseColumn::Name {
                name_width
            } else {
                column_fixed_width(column)
            },
        })
        .collect()
}

fn name_column_width(layout: &[BrowseColumnCell]) -> usize {
    layout
        .iter()
        .find(|cell| cell.column == BrowseColumn::Name)
        .map(|cell| cell.width)
        .unwrap_or(MIN_NAME_W)
}

/// Format the cached advisory without flattening its confidence.
fn browse_preemphasis_status_text(
    advisory: &super::preemphasis::PreemphasisAdvisory,
) -> String {
    let evidence = advisory
        .catalog
        .as_ref()
        .map(|catalog| format!("catalog {}", catalog.catalog_number))
        .unwrap_or_else(|| advisory.detail.clone());
    let verdict = match advisory.confidence {
        super::preemphasis::PreemphasisConfidence::Detected => "detected",
        super::preemphasis::PreemphasisConfidence::StrongCandidate => "strong candidate",
        super::preemphasis::PreemphasisConfidence::Possible => "possible",
        super::preemphasis::PreemphasisConfidence::NotDetected => "not detected",
        super::preemphasis::PreemphasisConfidence::Indeterminate => "not checked",
    };
    if evidence.trim().is_empty() {
        verdict.to_string()
    } else {
        format!("{verdict} ({evidence})")
    }
}

fn middle_truncate_tab_label(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars { return value.to_string(); }
    if max_chars <= 1 { return "…".chars().take(max_chars).collect(); }
    let keep = max_chars - 1;
    let left = (keep + 1) / 2;
    let right = keep / 2;
    let mut out: String = chars[..left].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - right..].iter());
    out
}

fn draw_browse_tab_strip(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let infos = app.browse.tab_infos();
    if infos.is_empty() {
        return;
    }

    // Record strip ownership first. More specific tab/control regions are
    // recorded later and win reverse hit-testing, leaving this as the target
    // for otherwise-empty row space (not the file list behind it).
    app.button_map.record_button(TuiButton::BrowseDirTabStrip, area);

    // Reserve actions before tabs, but never at the cost of the minimum tab
    // cell. Prefer descriptive labels; shed Reopen first, then compact New Tab
    // to the legacy '+' affordance only under real width pressure. Reopen is
    // always available from the strip context menu even when its button drops.
    let min_cell = 7usize;
    let min_tabs_w = min_cell as u16;
    let separator_w = 1u16;
    let full_reopen_w = 8u16; // " Reopen "
    let full_new_w = 9u16; // " New Tab "
    let compact_new_w = 3u16; // " + "
    let full_new = area.width >= min_tabs_w.saturating_add(separator_w + full_new_w);
    let new_w = if full_new {
        full_new_w
    } else if area.width >= min_tabs_w.saturating_add(compact_new_w) {
        compact_new_w
    } else {
        0
    };
    let show_reopen = app.browse.has_closed_tabs()
        && full_new
        && area.width
            >= min_tabs_w.saturating_add(
                separator_w + full_reopen_w + separator_w + full_new_w,
            );
    let controls_w = if new_w == 0 {
        0
    } else if show_reopen {
        separator_w + full_reopen_w + separator_w + new_w
    } else {
        separator_w + new_w
    };
    let tabs_w = area.width.saturating_sub(controls_w);
    if tabs_w == 0 {
        return;
    }

    let max_cell = 24usize;
    let capacity = ((tabs_w as usize) / min_cell).max(1);
    let visible_count = infos.len().min(capacity);
    let active = app.browse.active_tab_index().min(infos.len().saturating_sub(1));
    let mut start = active.saturating_sub(visible_count / 2);
    if start + visible_count > infos.len() {
        start = infos.len().saturating_sub(visible_count);
    }
    let end = (start + visible_count).min(infos.len());
    let hidden_left = start;
    let hidden_right = infos.len().saturating_sub(end);
    let indicator_w = (if hidden_left > 0 { 2 } else { 0 }) + (if hidden_right > 0 { 4 } else { 0 });
    let usable = tabs_w.saturating_sub(indicator_w);
    let cell_w = ((usable as usize) / visible_count.max(1)).clamp(min_cell, max_cell) as u16;
    let mut x = area.x;

    if hidden_left > 0 {
        let r = Rect::new(x, area.y, 2.min(area.right().saturating_sub(x)), 1);
        f.render_widget(Paragraph::new("‹ ").style(Style::default().fg(theme.text_dim)), r);
        x = x.saturating_add(r.width);
    }

    for info in &infos[start..end] {
        if x >= area.x.saturating_add(tabs_w) { break; }
        let width = cell_w.min(area.x.saturating_add(tabs_w).saturating_sub(x));
        if width == 0 { break; }
        let cell = Rect::new(x, area.y, width, 1);
        // The active tab is shown by its highlighted background/bold; a leading
        // marker glyph would be a redundant, confusing vertical bar. Keep a
        // single leading pad space for both states so cell widths are stable.
        let active_mark = " ";
        let loading = if info.loading { "◐" } else { "" };
        let selected = if info.has_selection { "•" } else { "" };
        let close = if width >= 6 { "[×]" } else { "" };
        let close_w = close.chars().count() as u16; // 3 or 0
        // Right-align the close affordance to the cell edge so the drawn [×]
        // sits exactly under its registered click region (below). A left-aligned
        // close floats next to short labels and never receives the click.
        let left_cols = width.saturating_sub(close_w) as usize;
        let fixed =
            active_mark.chars().count() + loading.chars().count() + selected.chars().count();
        let label_w = left_cols.saturating_sub(fixed).max(1);
        let label = middle_truncate_tab_label(&info.label, label_w);
        let left = truncate_to(&format!("{active_mark}{loading}{label}{selected}"), left_cols);
        let pad = left_cols.saturating_sub(super::display_width::width(&left));
        let text = format!("{left}{}{close}", " ".repeat(pad));
        let style = if info.active {
            Style::default().fg(theme.bg).bg(theme.tab_active).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.tab_inactive)
        };
        f.render_widget(Paragraph::new(text).style(style), cell);
        let body_w = width.saturating_sub(close_w);
        if body_w > 0 {
            app.button_map.record_button(TuiButton::BrowseDirTab(info.index), Rect::new(x, area.y, body_w, 1));
        }
        if close_w > 0 {
            app.button_map.record_button(TuiButton::BrowseDirTabClose(info.index), Rect::new(x + body_w, area.y, close_w, 1));
        }
        x = x.saturating_add(width);
    }

    if hidden_right > 0 && x < area.x.saturating_add(tabs_w) {
        let text = format!("›+{}", hidden_right);
        let width = (text.chars().count() as u16).min(area.x.saturating_add(tabs_w).saturating_sub(x));
        if width > 0 {
            f.render_widget(Paragraph::new(truncate_to(&text, width as usize)).style(Style::default().fg(theme.text_dim)), Rect::new(x, area.y, width, 1));
        }
    }

    if controls_w > 0 {
        let mut cx = area.x.saturating_add(tabs_w);
        // Buttons use the same high-contrast style as the file picker's tab
        // buttons (dark text on cyan) so they read unmistakably as buttons —
        // theme.surface was nearly the app background. Separators sit on the
        // plain background so each button reads as a distinct chip.
        let separator_style = Style::default().fg(theme.border_dim);
        let button_style = Style::default().fg(theme.bg).bg(theme.cyan);

        let leading_separator = Rect::new(cx, area.y, 1.min(area.right().saturating_sub(cx)), 1);
        if leading_separator.width > 0 {
            f.render_widget(Paragraph::new("│").style(separator_style), leading_separator);
            cx = cx.saturating_add(leading_separator.width);
        }

        if show_reopen {
            let reopen = Rect::new(
                cx,
                area.y,
                full_reopen_w.min(area.right().saturating_sub(cx)),
                1,
            );
            if reopen.width == full_reopen_w {
                f.render_widget(Paragraph::new(" Reopen ").style(button_style), reopen);
                app.button_map
                    .record_button(TuiButton::BrowseDirTabReopenClosed, reopen);
                cx = cx.saturating_add(reopen.width);
            }
            let separator = Rect::new(cx, area.y, 1.min(area.right().saturating_sub(cx)), 1);
            if separator.width > 0 {
                f.render_widget(Paragraph::new("│").style(separator_style), separator);
                cx = cx.saturating_add(separator.width);
            }
        }

        let add = Rect::new(cx, area.y, new_w.min(area.right().saturating_sub(cx)), 1);
        if add.width == new_w {
            let label = if full_new { " New Tab " } else { " + " };
            f.render_widget(Paragraph::new(label).style(button_style), add);
            app.button_map.record_button(TuiButton::BrowseDirTabNew, add);
        }
    }
}

/// Draw the full browse screen.
pub fn draw_browse_screen(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    app.browse.last_render_area = Some(area);

    // The directory tab strip only claims a row when there are 2+ tabs, so a
    // single-tab Browse view keeps its exact pre-tabs layout (no wasted row).
    let tab_strip_rows: u16 = if app.browse.tab_count() > 1 { 1 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),              // header banner
            Constraint::Length(5),              // toolbar + path bar (two boxes, shared middle border)
            Constraint::Length(tab_strip_rows), // directory tab strip (hidden at a single tab)
            Constraint::Min(10),                // three-pane browse content
            Constraint::Length(2),              // footer (screen tabs + context)
        ])
        .split(area);

    draw_header(f, chunks[0], theme);
    draw_browse_toolbar(f, chunks[1], app, theme);
    if tab_strip_rows > 0 {
        draw_browse_tab_strip(f, chunks[2], app, theme);
    }

    let content_chunks = browse_content_layout(chunks[3], &app.browse);
    let explore_area = content_chunks[0];
    let list_area = content_chunks[1];
    let info_area = content_chunks[2];

    let hover = app.hover_target;
    let inline_edit = app.browse_inline_edit.clone();
    let navigation_focus_active = app.browse_info_focus.is_none()
        && (!app.browse.search.active
            || app.browse.search.focus == super::browse::SearchFocus::Results);

    if app.browse.explore_enabled {
        if app.browse.explore_collapsed {
            draw_collapsed_pane(f, explore_area, BrowsePaneId::Explore, "explore", &mut app.button_map, theme);
        } else {
            draw_explore_pane(
                f,
                explore_area,
                &mut app.browse,
                inline_edit.as_ref(),
                &mut app.button_map,
                hover,
                navigation_focus_active,
                theme,
            );
        }
    }

    let list_scrollbar = draw_browse_list(
        f,
        list_area,
        &mut app.browse,
        inline_edit.as_ref(),
        hover,
        navigation_focus_active,
        theme,
    );
    let create_row_active = matches!(
        app.browse_inline_edit.as_ref().map(|state| &state.target),
        Some(crate::tui::app::BrowseInlineEditTarget::Create { dir, .. }) if dir == &app.browse.current_dir
    );
    register_browse_buttons(
        &mut app.button_map,
        list_area,
        &app.browse,
        inline_edit.as_ref(),
        create_row_active,
    );
    if let Some((track, thumb)) = list_scrollbar {
        app.button_map.record_button(
            TuiButton::ScrollbarTrack(ScrollbarSurface::BrowseList),
            track,
        );
        app.button_map.record_button(
            TuiButton::ScrollbarThumb(ScrollbarSurface::BrowseList),
            thumb,
        );
    }
    // The Browse pane is maximized/restored by double-clicking its title.
    // Do not also register the title glyph as a single-click toggle: Browse
    // never collapses, and a destructive single-click here violates the
    // documented interaction model.
    app.button_map.record_button(TuiButton::BrowsePaneTitle(BrowsePaneId::Browse), Rect::new(list_area.x, list_area.y, list_area.width, 1));

    if app.browse.info_enabled {
        if app.browse.info_collapsed {
            draw_collapsed_pane(f, info_area, BrowsePaneId::Info, "info", &mut app.button_map, theme);
        } else {
            draw_browse_info(
                f,
                info_area,
                &app.browse,
                inline_edit.as_ref(),
                app.browse_info_focus,
                &mut app.button_map,
                hover,
                theme,
            );
            app.button_map.record_button(TuiButton::BrowsePaneToggle(BrowsePaneId::Info), Rect::new(info_area.x + 1, info_area.y, 7.min(info_area.width.saturating_sub(2)), 1));
        }
    }

    let file_task_footer = app.file_task_footer_state();
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[4],
        app.current_screen,
        app.browse.tab_count(),
        &mut app.button_map,
        status_msg,
        file_task_footer,
        theme,
    );

    if app.browse.options_menu.is_open() {
        let anchor = app
            .button_map
            .button_rect(TuiButton::BrowseToolbarOptions)
            .unwrap_or_else(|| options_button_anchor_for_toolbar(chunks[1]));
        draw_options_menu(
            f,
            anchor,
            area,
            &app.browse,
            &app.config.performance.browsing.archive_listing,
            &mut app.button_map,
            app.hover_target,
            theme,
        );
    }
    if app.bookmarks.dropdown_open {
        if let Some(anchor) = app.button_map.button_rect(TuiButton::BrowseBookmarksToggle) {
            draw_bookmarks_dropdown(
                f,
                anchor,
                area,
                &mut app.bookmarks,
                &mut app.button_map,
                app.hover_target,
                theme,
            );
        } else {
            // The path-row affordance itself can disappear at extreme sizes.
            // Do not retain keyboard ownership for a dropdown that has neither
            // an anchor nor any visible hit targets.
            app.bookmarks.close_dropdown();
        }
    }
}

fn browse_content_layout(area: Rect, browse: &BrowseState) -> std::rc::Rc<[Rect]> {
    let constraints: Vec<Constraint> = match (
        browse.explore_enabled,
        browse.info_enabled,
        browse.explore_collapsed,
        browse.info_collapsed,
    ) {
        (false, false, _, _) => vec![Constraint::Length(0), Constraint::Percentage(100), Constraint::Length(0)],
        (false, true, _, true) => vec![Constraint::Length(0), Constraint::Min(40), Constraint::Length(3)],
        (false, true, _, false) => {
            let info_width = area.width / 3;
            vec![
                Constraint::Length(0),
                Constraint::Length(area.width.saturating_sub(info_width)),
                Constraint::Length(info_width),
            ]
        },
        (true, false, true, _) => vec![Constraint::Length(3), Constraint::Min(40), Constraint::Length(0)],
        (true, false, false, _) => vec![Constraint::Percentage(20), Constraint::Percentage(80), Constraint::Length(0)],
        (true, true, true, true) => vec![Constraint::Length(3), Constraint::Min(40), Constraint::Length(3)],
        (true, true, true, false) => vec![Constraint::Length(3), Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)],
        (true, true, false, true) => vec![Constraint::Percentage(20), Constraint::Min(40), Constraint::Length(3)],
        (true, true, false, false) => vec![Constraint::Percentage(20), Constraint::Percentage(50), Constraint::Percentage(30)],
    };
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area)
}

fn draw_browse_toolbar(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    if area.height < 5 || area.width < 20 {
        return;
    }

    // Two stacked boxes sharing a middle border line:
    //   Line 0: ┌──────────────────────────────────────┐
    //   Line 1: │ ‹ Back  › Fwd  ↑ Up  Refresh  ...   │
    //   Line 2: ├──────────────────────────────────────┤
    //   Line 3: │ path: ~/dir                     [Go] │
    //   Line 4: └──────────────────────────────────────┘
    let border_color = if app.browse.path_input.is_some() {
        theme.blue
    } else {
        theme.border_dim
    };
    let bs = theme.border(border_color);
    let w = area.width as usize;
    let inner_w = w.saturating_sub(2);

    // Line 0: top border
    let top = Line::from(Span::styled(format!("┌{}┐", "─".repeat(inner_w)), bs));
    f.render_widget(Paragraph::new(top), Rect::new(area.x, area.y, area.width, 1));

    // Line 1: side borders (button content rendered on top)
    f.render_widget(Paragraph::new("│").style(bs), Rect::new(area.x, area.y + 1, 1, 1));
    f.render_widget(Paragraph::new("│").style(bs), Rect::new(area.x + area.width - 1, area.y + 1, 1, 1));

    // Line 2: shared middle border
    let mid = Line::from(Span::styled(format!("├{}┤", "─".repeat(inner_w)), bs));
    f.render_widget(Paragraph::new(mid), Rect::new(area.x, area.y + 2, area.width, 1));

    // Line 3: side borders (path content rendered on top)
    f.render_widget(Paragraph::new("│").style(bs), Rect::new(area.x, area.y + 3, 1, 1));
    f.render_widget(Paragraph::new("│").style(bs), Rect::new(area.x + area.width - 1, area.y + 3, 1, 1));

    // Line 4: bottom border
    let bot = Line::from(Span::styled(format!("└{}┘", "─".repeat(inner_w)), bs));
    f.render_widget(Paragraph::new(bot), Rect::new(area.x, area.y + 4, area.width, 1));

    // Render toolbar buttons on line 1, inside the borders
    let mut x = area.x + 1;
    let y = area.y + 1;
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarBack, x, y, " ‹ Back ", app.browse.can_go_back(), theme);
    x = x.saturating_add(8);
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarForward, x, y, " › Fwd ", app.browse.can_go_forward(), theme);
    x = x.saturating_add(8);
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarUp, x, y, " ↑ Up ", app.browse.current_dir.parent().is_some(), theme);
    x = x.saturating_add(7);
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarRefresh, x, y, " Refresh ", true, theme);
    x = x.saturating_add(10);
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarOptions, x, y, " Options ▾ ", true, theme);
    x = x.saturating_add(12);
    draw_toolbar_button(f, &mut app.button_map, TuiButton::BrowseToolbarSearch, x, y, " Search ", true, theme);

    // Render path bar on line 3, inside the borders. Reserve explicit hit
    // rectangles for both trailing actions so the breadcrumb never overlaps them.
    let row_area = Rect::new(area.x + 1, area.y + 3, area.width.saturating_sub(2), 1);
    const BOOKMARK_WIDTH: u16 = 13;
    // One blank cell between Go and bookmarks so the two buttons read as
    // separate controls instead of one fused pill (user-requested spacer).
    const ACTION_GAP: u16 = 1;
    let action_width = BROWSE_PATH_GO_WIDTH
        .saturating_add(ACTION_GAP)
        .saturating_add(BOOKMARK_WIDTH);
    let path_area = Rect::new(
        row_area.x,
        row_area.y,
        row_area.width.saturating_sub(action_width),
        1,
    );
    draw_breadcrumb_inline(f, path_area, &app.browse, theme);
    app.button_map.record_button(TuiButton::BrowseBreadcrumb, path_area);
    if app.browse.path_input.is_some() {
        let prefix_width = (super::display_width::width(" path: ") as u16).min(path_area.width);
        let input_width = path_area
            .width
            .saturating_sub(prefix_width)
            .saturating_sub(1);
        if input_width > 0 {
            app.button_map.record_button(
                TuiButton::BrowsePathInlineEdit,
                Rect::new(
                    path_area.x.saturating_add(prefix_width),
                    path_area.y,
                    input_width,
                    1,
                ),
            );
        }
    }
    if row_area.width >= action_width {
        let go = Rect::new(
            row_area.right().saturating_sub(action_width),
            row_area.y,
            BROWSE_PATH_GO_WIDTH,
            1,
        );
        let bookmarks = Rect::new(
            go.right().saturating_add(ACTION_GAP),
            row_area.y,
            BOOKMARK_WIDTH,
            1,
        );
        f.render_widget(
            Paragraph::new(BROWSE_PATH_GO_LABEL).style(browse_toolbar_button_style(theme)),
            go,
        );
        f.render_widget(
            Paragraph::new(" bookmarks ▾ ").style(browse_toolbar_button_style(theme)),
            bookmarks,
        );
        app.button_map.record_button(TuiButton::BrowsePathGo, go);
        app.button_map.record_button(TuiButton::BrowseBookmarksToggle, bookmarks);
    }
}

fn draw_toolbar_button(
    f: &mut Frame,
    buttons: &mut ButtonRenderMap,
    button: TuiButton,
    x: u16,
    y: u16,
    label: &str,
    enabled: bool,
    theme: super::theme::Theme,
) {
    let width = super::display_width::width(label) as u16;
    let style = if enabled {
        browse_toolbar_button_style(theme)
    } else {
        browse_toolbar_button_disabled_style(theme)
    };
    f.render_widget(Paragraph::new(label).style(style), Rect::new(x, y, width, 1));
    if enabled {
        buttons.record_button(button, Rect::new(x, y, width, 1));
    }
}


fn browse_toolbar_button_style(theme: super::theme::Theme) -> Style {
    // Match the file-picker toolbar button styling. The Browse toolbar should
    // look like actionable buttons, not dim text sitting on the pane surface.
    Style::default().fg(theme.bg).bg(theme.cyan).add_modifier(Modifier::BOLD)
}

fn browse_toolbar_button_disabled_style(theme: super::theme::Theme) -> Style {
    Style::default().fg(theme.text_muted).bg(theme.border_dim)
}

fn draw_explore_pane(
    f: &mut Frame,
    area: Rect,
    browse: &mut BrowseState,
    inline_edit: Option<&BrowseInlineEditState>,
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
    navigation_focus_active: bool,
    theme: super::theme::Theme,
) {
    if area.height < 3 || area.width < 6 {
        return;
    }
    let border_color = theme.cyan;
    let w = area.width as usize;

    // Solid title bar (matches convert screen pane style)
    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let title = "▾ explore ";
    let title_w = super::display_width::width(&title);
    let dash_count = w.saturating_sub(2 + title_w);
    let top_line = Line::from(vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(title, bar_style),
        Span::styled(" ".repeat(dash_count), bar_style),
        Span::styled("┐", theme.border(border_color)),
    ]);
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let content_height = (area.height as usize).saturating_sub(2);
    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        content_height as u16,
    );

    f.render_widget(Paragraph::new(top_line), Rect::new(area.x, area.y, area.width, 1));
    for row in 0..content_height {
        let y = area.y + 1 + row as u16;
        f.render_widget(
            Paragraph::new("│").style(theme.border(border_color)),
            Rect::new(area.x, y, 1, 1),
        );
        f.render_widget(
            Paragraph::new("│").style(theme.border(border_color)),
            Rect::new(area.x + area.width - 1, y, 1, 1),
        );
    }
    f.render_widget(
        Paragraph::new(bot_line),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    buttons.record_button(
        TuiButton::BrowsePaneToggle(BrowsePaneId::Explore),
        Rect::new(area.x + 1, area.y, title_w as u16, 1),
    );

    let create = inline_edit.and_then(|state| match &state.target {
        BrowseInlineEditTarget::Create { dir, .. } => Some((dir, &state.input)),
        _ => None,
    });
    let tree_entry_capacity = effective_entry_capacity(inner.height as usize, create.is_some());
    browse.set_tree_visible_height(tree_entry_capacity);
    f.render_widget(Clear, inner);

    let rename = inline_edit.and_then(|state| match &state.target {
        BrowseInlineEditTarget::Rename { path } => Some((path, &state.input)),
        _ => None,
    });

    let start = browse.tree_scroll;
    let mut visual_row = 0usize;
    let mut rendered_nodes = 0usize;
    let mut absolute = start;
    let mut editor_cursor = None;
    while absolute < browse.tree_nodes.len()
        && rendered_nodes < tree_entry_capacity
        && visual_row < inner.height as usize
    {
        let node = &browse.tree_nodes[absolute];
        let row_area = Rect::new(inner.x, inner.y + visual_row as u16, inner.width, 1);
        let disclosure_x = inner
            .x
            .saturating_add((node.depth.saturating_mul(2)) as u16)
            .min(inner.x.saturating_add(inner.width.saturating_sub(1)));
        let row_hovered = hover == Some(TuiButton::BrowseTreeNode(absolute))
            || hover == Some(TuiButton::BrowseTreeDisclosure(absolute));

        if rename.is_some_and(|(path, _)| path == &node.path) {
            let input = rename.expect("rename predicate established").1;
            let glyph = if node.has_children {
                if node.expanded { "▾" } else { "▸" }
            } else {
                " "
            };
            let prefix = format!("{}{} ", "  ".repeat(node.depth), glyph);
            let prefix_width = super::display_width::width(&prefix);
            let input_width = (inner.width as usize).saturating_sub(prefix_width).max(1);
            let mut spans = vec![Span::styled(prefix, theme.text_style())];
            spans.extend(render_inline_value_with_embedded_cursor(input, input_width, theme));
            f.render_widget(Paragraph::new(Line::from(spans)), row_area);
            buttons.record_button(
                TuiButton::BrowseTreeInlineEdit,
                Rect::new(
                    row_area.x.saturating_add(prefix_width as u16),
                    row_area.y,
                    input_width as u16,
                    1,
                ),
            );
            editor_cursor = Some((
                inner.x.saturating_add(prefix_width as u16).saturating_add(
                    inline_cursor_col(input, input_width) as u16,
                ),
                row_area.y,
            ));
        } else {
            let line = render_browse_tree_node_line(
                node,
                absolute == browse.tree_cursor
                    && navigation_focus_active
                    && browse.tree_navigation_active(),
                row_hovered,
                theme,
            );
            f.render_widget(Paragraph::new(line), row_area);
            buttons.record_button(TuiButton::BrowseTreeNode(absolute), row_area);
            if node.has_children {
                buttons.record_button(
                    TuiButton::BrowseTreeDisclosure(absolute),
                    Rect::new(disclosure_x, row_area.y, 1, 1),
                );
            }
        }
        visual_row += 1;
        rendered_nodes += 1;

        if visual_row < inner.height as usize
            && create.is_some_and(|(dir, _)| dir == &node.path)
        {
            let input = create.expect("create predicate established").1;
            let prefix = format!("{}  + ", "  ".repeat(node.depth));
            let prefix_width = super::display_width::width(&prefix);
            let input_width = (inner.width as usize).saturating_sub(prefix_width).max(1);
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme.amber))];
            spans.extend(render_inline_value_with_embedded_cursor(input, input_width, theme));
            let create_area = Rect::new(inner.x, inner.y + visual_row as u16, inner.width, 1);
            f.render_widget(Paragraph::new(Line::from(spans)), create_area);
            buttons.record_button(
                TuiButton::BrowseTreeInlineEdit,
                Rect::new(
                    create_area.x.saturating_add(prefix_width as u16),
                    create_area.y,
                    input_width as u16,
                    1,
                ),
            );
            editor_cursor = Some((
                inner.x.saturating_add(prefix_width as u16).saturating_add(
                    inline_cursor_col(input, input_width) as u16,
                ),
                create_area.y,
            ));
            visual_row += 1;
        }
        absolute += 1;
    }

    if let Some((track, thumb)) = draw_vertical_scrollbar(
        f,
        Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height),
        browse.tree_nodes.len(),
        browse.tree_visible_height,
        browse.tree_scroll,
        theme,
    ) {
        buttons.record_button(
            TuiButton::ScrollbarTrack(ScrollbarSurface::BrowseTree),
            track,
        );
        buttons.record_button(
            TuiButton::ScrollbarThumb(ScrollbarSurface::BrowseTree),
            thumb,
        );
    }

    if let Some((x, y)) = editor_cursor {
        f.set_cursor(x.min(inner.x + inner.width.saturating_sub(1)), y);
    }
}

fn render_browse_tree_node_line(
    node: &super::browse::BrowseTreeNode,
    selected: bool,
    hovered: bool,
    theme: super::theme::Theme,
) -> Line<'static> {
    // Keep Browse tree row presentation behind a single adapter over the
    // file-picker TreeNode model. That avoids a second tree row model drifting
    // away from the picker while still allowing Browse-specific selection and
    // hover colors.
    let glyph = if node.has_children {
        if node.expanded { "▾" } else { "▸" }
    } else {
        " "
    };
    let indent = "  ".repeat(node.depth);
    let mut style = theme.text_style();
    if selected {
        style = style
            .bg(theme.selection_bg)
            .fg(theme.text_bright)
            .add_modifier(Modifier::BOLD);
    } else if hovered {
        style = style.bg(theme.surface);
    }
    Line::from(Span::styled(format!("{}{} {}", indent, glyph, node.name), style))
}

fn draw_collapsed_pane(
    f: &mut Frame,
    area: Rect,
    pane: BrowsePaneId,
    title: &str,
    buttons: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Fill the entire collapsed pane with a subtle background so it doesn't
    // blend into the screen background, but keep it muted (not the active
    // border color).
    let collapsed_bg = theme.surface;
    let fill_style = Style::default().bg(collapsed_bg);
    for row in 0..area.height {
        f.render_widget(
            Paragraph::new(" ".repeat(area.width as usize)).style(fill_style),
            Rect::new(area.x, area.y + row, area.width, 1),
        );
    }
    let block = Block::default().borders(Borders::ALL).border_style(theme.border(theme.border_dim));
    let inner = bordered_panel_inner(area);
    f.render_widget(block, area);
    let active_color = match pane {
        BrowsePaneId::Explore => theme.cyan,
        BrowsePaneId::Info => theme.amber,
        BrowsePaneId::Browse => theme.cyan,
    };
    let text_style = Style::default().fg(active_color).bg(collapsed_bg);
    let chars = std::iter::once('▸')
        .chain(std::iter::once(' '))
        .chain(title.chars())
        .collect::<Vec<_>>();
    for (row, ch) in chars.into_iter().enumerate().take(inner.height as usize) {
        let rect = Rect::new(inner.x, inner.y + row as u16, 1, 1);
        f.render_widget(Paragraph::new(ch.to_string()).style(text_style), rect);
    }
    buttons.record_button(TuiButton::BrowsePaneToggle(pane), area);
}

fn draw_options_menu(
    f: &mut Frame,
    anchor: Rect,
    screen_area: Rect,
    browse: &BrowseState,
    archive_listing_mode: &str,
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
    theme: super::theme::Theme,
) {
    let root_rows = options_root_rows(browse);
    let geometry = options_menu_geometry_for_area(
        anchor,
        screen_area,
        browse,
        archive_listing_mode,
    );
    let active_parent = active_options_parent_button(browse.options_menu);
    let root_selected = if browse.options_menu == BrowseOptionsMenu::Root {
        browse
            .options_menu_highlight
            .and_then(|index| root_rows.get(index))
            .and_then(|(_, button)| *button)
    } else {
        active_parent
    };

    render_options_menu_panel(
        f,
        geometry.root_area,
        "Options",
        &root_rows,
        buttons,
        hover,
        root_selected,
        theme,
    );

    if let (Some((title, submenu_rows)), Some(submenu_area)) = (
        options_submenu_rows(browse, archive_listing_mode),
        geometry.submenu_area,
    ) {
        let submenu_selected = browse
            .options_menu_highlight
            .and_then(|index| submenu_rows.get(index))
            .and_then(|(_, button)| *button);
        render_options_menu_panel(
            f,
            submenu_area,
            title,
            &submenu_rows,
            buttons,
            hover,
            submenu_selected,
            theme,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OptionsMenuGeometry {
    pub(super) root_area: Rect,
    pub(super) submenu_area: Option<Rect>,
}

impl OptionsMenuGeometry {
    pub(super) fn contains(self, x: u16, y: u16) -> bool {
        rect_contains(self.root_area, x, y)
            || match self.submenu_area {
                Some(area) => rect_contains(area, x, y),
                None => false,
            }
    }
}

pub(super) fn browse_toolbar_area_for_screen(screen_area: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(screen_area)[1]
}

pub(super) fn options_button_anchor_for_toolbar(toolbar_area: Rect) -> Rect {
    Rect::new(
        toolbar_area.x.saturating_add(34),
        toolbar_area.y.saturating_add(1),
        12.min(toolbar_area.width),
        1,
    )
}


pub(super) fn options_menu_geometry_for_area(
    anchor: Rect,
    screen_area: Rect,
    browse: &BrowseState,
    archive_listing_mode: &str,
) -> OptionsMenuGeometry {
    let root_rows = options_root_rows(browse);
    let root_width = options_menu_panel_width("Options", &root_rows, screen_area.width);
    let root_height = root_rows.len() as u16 + 2;
    let root_x = clamp_menu_x(anchor.x, root_width, screen_area);
    let preferred_y = anchor.y.saturating_add(anchor.height);
    let root_y = clamp_menu_y(preferred_y, root_height, screen_area);
    let root_area = Rect::new(root_x, root_y, root_width, root_height);
    let active_parent = active_options_parent_button(browse.options_menu);

    let submenu_area = options_submenu_rows(browse, archive_listing_mode).map(|(title, submenu_rows)| {
        let submenu_width = options_menu_panel_width(title, &submenu_rows, screen_area.width);
        let submenu_height = submenu_rows.len() as u16 + 2;
        let parent_row_index = active_parent
            .and_then(|active| {
                root_rows
                    .iter()
                    .position(|(_, button)| *button == Some(active))
            })
            .unwrap_or(0);
        let preferred_submenu_y = root_area
            .y
            .saturating_add(1)
            .saturating_add(parent_row_index as u16);
        options_submenu_area(
            root_area,
            submenu_width,
            submenu_height,
            preferred_submenu_y,
            screen_area,
        )
    });

    OptionsMenuGeometry {
        root_area,
        submenu_area,
    }
}

fn effective_entry_capacity(total_rows: usize, inline_create_active: bool) -> usize {
    total_rows.saturating_sub(usize::from(inline_create_active))
}

fn bookmark_dropdown_fits(screen_area: Rect) -> bool {
    screen_area.width >= 8 && screen_area.height >= 6
}

#[cfg(test)]
mod viewport_capacity_tests {
    use super::*;

    #[test]
    fn inline_create_reserves_one_authoritative_entry_row() {
        assert_eq!(effective_entry_capacity(10, false), 10);
        assert_eq!(effective_entry_capacity(10, true), 9);
        assert_eq!(effective_entry_capacity(0, true), 0);

        let visible = effective_entry_capacity(10, true);
        let final_offset = 100usize.saturating_sub(visible);
        assert_eq!(final_offset, 91);
        let metrics = tui_file_picker::ScrollbarMetrics::new(100, visible, final_offset, 10)
            .expect("scrollbar metrics");
        assert_eq!(metrics.max_offset, final_offset);
        assert_eq!(metrics.thumb_start + metrics.thumb_len, metrics.track_len);
    }

    #[test]
    fn dropdown_minimum_geometry_is_explicit() {
        assert!(!bookmark_dropdown_fits(Rect::new(0, 0, 7, 6)));
        assert!(!bookmark_dropdown_fits(Rect::new(0, 0, 8, 5)));
        assert!(bookmark_dropdown_fits(Rect::new(0, 0, 8, 6)));
    }
}

fn draw_bookmarks_dropdown(
    f: &mut Frame,
    anchor: Rect,
    screen_area: Rect,
    state: &mut super::bookmarks::BookmarksState,
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
    theme: super::theme::Theme,
) {
    // Never leave an invisible modal active. If the terminal cannot preserve
    // the dropdown's minimum geometry, close it and return keyboard ownership
    // to the underlying Browse surface.
    if !bookmark_dropdown_fits(screen_area) {
        state.close_dropdown();
        return;
    }

    let longest = state
        .entries
        .iter()
        .map(|entry| super::display_width::width(&entry.name).saturating_add(4))
        .max()
        .unwrap_or(18)
        .max(20);
    let width = longest.min(40).saturating_add(2) as u16;
    let width = width.min(screen_area.width);
    let max_bookmark_rows = screen_area.height.saturating_sub(5).min(10) as usize;
    state.set_dropdown_visible_rows(max_bookmark_rows);
    let visible_bookmarks = state.entries.len().min(max_bookmark_rows);
    let height = visible_bookmarks as u16 + 5;
    let x = clamp_menu_x(anchor.right().saturating_sub(width), width, screen_area);
    let y = clamp_menu_y(anchor.bottom(), height, screen_area);
    let area = Rect::new(x, y, width, height);

    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.green))
        .title(Span::styled(
            " Bookmarks ",
            Style::default().fg(theme.green).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let start = state.dropdown_scroll;
    let end = (start + max_bookmark_rows).min(state.entries.len());
    let mut targets = Vec::new();
    for (row, index) in (start..end).enumerate() {
        let entry = &state.entries[index];
        let selected = state.dropdown_selected == index;
        let target_status = state.target_status(&entry.path);
        let missing = target_status == Some(super::bookmarks::BookmarkTargetStatus::Missing);
        let unavailable =
            target_status == Some(super::bookmarks::BookmarkTargetStatus::Unavailable);
        let button = TuiButton::BrowseBookmarkDropdownRow(index);
        let hovered = hover == Some(button);
        let style = if selected || hovered {
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else if missing {
            Style::default()
                .fg(theme.destructive)
                .add_modifier(Modifier::DIM)
        } else if unavailable {
            Style::default().fg(theme.amber).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.text)
        };
        let prefix = if missing {
            " ! "
        } else if unavailable {
            " ? "
        } else if selected {
            " ▸ "
        } else {
            "   "
        };
        let row_area = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        let name = super::display_width::truncate_right(
            &entry.name,
            inner.width.saturating_sub(3) as usize,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(prefix, style), Span::styled(name, style)]))
                .style(style),
            row_area,
        );
        targets.push((button, row_area));
    }

    let separator_y = inner.y.saturating_add(visible_bookmarks as u16);
    f.render_widget(
        Paragraph::new("─".repeat(inner.width as usize)).style(Style::default().fg(theme.border_dim)),
        Rect::new(inner.x, separator_y, inner.width, 1),
    );
    let add_index = state.entries.len();
    let manage_index = add_index + 1;
    let add_area = Rect::new(inner.x, separator_y + 1, inner.width, 1);
    let manage_area = Rect::new(inner.x, separator_y + 2, inner.width, 1);
    draw_dropdown_action_row(
        f,
        add_area,
        " Bookmark this dir",
        state.dropdown_selected == add_index,
        hover == Some(TuiButton::BrowseBookmarkDropdownAdd),
        theme,
    );
    draw_dropdown_action_row(
        f,
        manage_area,
        " Manage bookmarks…",
        state.dropdown_selected == manage_index,
        hover == Some(TuiButton::BrowseBookmarkDropdownManage),
        theme,
    );

    for (button, rect) in targets {
        buttons.record_button(button, rect);
    }
    buttons.record_button(TuiButton::BrowseBookmarkDropdownAdd, add_area);
    buttons.record_button(TuiButton::BrowseBookmarkDropdownManage, manage_area);
}

fn draw_dropdown_action_row(
    f: &mut Frame,
    area: Rect,
    label: &str,
    selected: bool,
    hovered: bool,
    theme: super::theme::Theme,
) {
    let style = if selected || hovered {
        Style::default()
            .fg(theme.text_bright)
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.cyan)
    };
    f.render_widget(Paragraph::new(label).style(style), area);
}

fn draw_vertical_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    visible: usize,
    offset: usize,
    theme: super::theme::Theme,
) -> Option<(Rect, Rect)> {
    let metrics = tui_file_picker::ScrollbarMetrics::new(
        total,
        visible,
        offset,
        area.height as usize,
    )?;
    let track_lines = (0..area.height)
        .map(|_| Line::from(Span::styled("░", Style::default().fg(theme.text_dim))))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(track_lines), area);
    let thumb = Rect::new(
        area.x,
        area.y.saturating_add(metrics.thumb_start as u16),
        1,
        metrics.thumb_len as u16,
    );
    let thumb_lines = (0..metrics.thumb_len)
        .map(|_| Line::from(Span::styled("█", Style::default().fg(theme.title))))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(thumb_lines), thumb);
    Some((area, thumb))
}

fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

pub(super) fn options_root_rows(browse: &BrowseState) -> Vec<(String, Option<TuiButton>)> {
    vec![
        (
            (if browse.show_hidden { " ● Show hidden files" } else { " ○ Show hidden files" }).to_string(),
            Some(TuiButton::BrowseOptionsShowHidden),
        ),
        (" Layout                ▸".to_string(), Some(TuiButton::BrowseOptionsLayout)),
        (" Columns               ▸".to_string(), Some(TuiButton::BrowseOptionsColumns)),
        (" Default sort          ▸".to_string(), Some(TuiButton::BrowseOptionsSort)),
        (" Filter                ▸".to_string(), Some(TuiButton::BrowseOptionsFilter)),
        (
            " Archive listing mode  ▸".to_string(),
            Some(TuiButton::BrowseOptionsArchiveListing),
        ),
        (" ─────────────────────".to_string(), None),
        (
            " Save layout as default".to_string(),
            Some(TuiButton::BrowseOptionsSaveLayout),
        ),
        (
            " Restore defaults".to_string(),
            Some(TuiButton::BrowseOptionsRestoreDefaults),
        ),
    ]
}

pub(super) fn active_options_parent_button(menu: BrowseOptionsMenu) -> Option<TuiButton> {
    match menu {
        BrowseOptionsMenu::Layout => Some(TuiButton::BrowseOptionsLayout),
        BrowseOptionsMenu::Columns => Some(TuiButton::BrowseOptionsColumns),
        BrowseOptionsMenu::Sort => Some(TuiButton::BrowseOptionsSort),
        BrowseOptionsMenu::Filter => Some(TuiButton::BrowseOptionsFilter),
        BrowseOptionsMenu::ArchiveListing => Some(TuiButton::BrowseOptionsArchiveListing),
        BrowseOptionsMenu::Root | BrowseOptionsMenu::Closed => None,
    }
}

pub(super) fn options_submenu_rows(
    browse: &BrowseState,
    archive_listing_mode: &str,
) -> Option<(&'static str, Vec<(String, Option<TuiButton>)>)> {
    match browse.options_menu {
        BrowseOptionsMenu::Layout => Some((
            "Layout",
            vec![
                (
                    format!(" {} Show Explore pane", if browse.explore_enabled { "●" } else { "○" }),
                    Some(TuiButton::BrowseOptionsToggleExplore),
                ),
                (
                    format!(" {} Show Info pane", if browse.info_enabled { "●" } else { "○" }),
                    Some(TuiButton::BrowseOptionsToggleInfo),
                ),
            ],
        )),
        BrowseOptionsMenu::Columns => Some((
            "Columns",
            BrowseColumn::ALL
                .iter()
                .map(|column| {
                    let mark = if browse.columns.contains(column) { "☑" } else { "☐" };
                    (
                        format!(" {} {}", mark, column.label()),
                        Some(TuiButton::BrowseOptionsColumn(*column)),
                    )
                })
                .collect(),
        )),
        BrowseOptionsMenu::Sort => Some((
            "Default sort",
            SortBy::ALL
                .into_iter()
                .flat_map(|by| [(by, SortDir::Asc), (by, SortDir::Desc)])
                .map(|(by, dir)| {
                    let mark = if browse.default_sort_by == by && browse.default_sort_dir == dir {
                        "●"
                    } else {
                        "○"
                    };
                    let arrow = match dir {
                        SortDir::Asc => "↑",
                        SortDir::Desc => "↓",
                    };
                    (
                        format!(" {} {} {}", mark, by.display_label(), arrow),
                        Some(TuiButton::BrowseOptionsSortChoice(by, dir)),
                    )
                })
                .collect(),
        )),
        BrowseOptionsMenu::Filter => Some((
            "Filter",
            FormatFilter::menu_choices()
                .into_iter()
                .enumerate()
                .map(|(index, filter)| {
                    let mark = if browse.format_filter == filter { "●" } else { "○" };
                    (
                        format!(" {} {}", mark, filter.menu_label()),
                        Some(TuiButton::BrowseOptionsFilterChoice(index)),
                    )
                })
                .collect(),
        )),
        BrowseOptionsMenu::ArchiveListing => {
            let auto = archive_listing_choice_mark(archive_listing_mode, "auto");
            let always = archive_listing_choice_mark(archive_listing_mode, "always");
            let never = archive_listing_choice_mark(archive_listing_mode, "never");
            Some((
                "Archive listing",
                vec![
                    (
                        format!(" {} Auto (skip remote)", auto),
                        Some(TuiButton::BrowseOptionsArchiveChoice(0)),
                    ),
                    (
                        format!(" {} Always", always),
                        Some(TuiButton::BrowseOptionsArchiveChoice(1)),
                    ),
                    (
                        format!(" {} Never", never),
                        Some(TuiButton::BrowseOptionsArchiveChoice(2)),
                    ),
                ],
            ))
        }
        BrowseOptionsMenu::Root | BrowseOptionsMenu::Closed => None,
    }
}

fn options_menu_panel_width(title: &str, rows: &[(String, Option<TuiButton>)], outer_width: u16) -> u16 {
    let content_width = rows
        .iter()
        .map(|(row, _)| super::display_width::width(row))
        .max()
        .unwrap_or(12)
        .max(super::display_width::width(&title) + 4) as u16;
    content_width.saturating_add(2).min(40).min(outer_width.max(1))
}

fn clamp_menu_x(preferred_x: u16, width: u16, outer: Rect) -> u16 {
    let right = outer.x.saturating_add(outer.width);
    let max_x = right.saturating_sub(width);
    preferred_x.min(max_x).max(outer.x)
}

fn options_submenu_area(
    root_area: Rect,
    width: u16,
    height: u16,
    preferred_y: u16,
    outer: Rect,
) -> Rect {
    let y = clamp_menu_y(preferred_y, height, outer);
    let right_x = root_area.x.saturating_add(root_area.width);
    let outer_right = outer.x.saturating_add(outer.width);
    if right_x.saturating_add(width) <= outer_right {
        return Rect::new(right_x, y, width, height);
    }

    if root_area.x >= outer.x.saturating_add(width) {
        return Rect::new(root_area.x - width, y, width, height);
    }

    let right_candidate = clamp_menu_x(right_x, width, outer);
    let left_candidate = outer.x.max(root_area.x.saturating_sub(width));
    let right_overlap = horizontal_overlap(right_candidate, width, root_area.x, root_area.width);
    let left_overlap = horizontal_overlap(left_candidate, width, root_area.x, root_area.width);

    // Prefer the canonical right-side flyout on ties, but if the terminal is
    // too narrow for a non-overlapping flyout, choose the side that obscures
    // the smallest part of the root panel.
    let x = if left_overlap < right_overlap {
        left_candidate
    } else {
        right_candidate
    };
    Rect::new(x, y, width, height)
}

fn clamp_menu_y(preferred_y: u16, height: u16, outer: Rect) -> u16 {
    let bottom = outer.y.saturating_add(outer.height);
    let max_y = bottom.saturating_sub(height);
    preferred_y.min(max_y).max(outer.y)
}

fn horizontal_overlap(ax: u16, aw: u16, bx: u16, bw: u16) -> u16 {
    let a_end = ax.saturating_add(aw);
    let b_end = bx.saturating_add(bw);
    a_end.min(b_end).saturating_sub(ax.max(bx))
}

fn bordered_panel_inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn options_menu_row_hitbox(area: Rect, row_index: usize) -> Option<Rect> {
    let inner = bordered_panel_inner(area);
    if row_index >= inner.height as usize {
        return None;
    }
    Some(Rect::new(
        inner.x,
        inner.y.saturating_add(row_index as u16),
        inner.width,
        1,
    ))
}

fn archive_listing_choice_mark(config_value: &str, choice: &str) -> &'static str {
    let normalized = config_value.trim().to_ascii_lowercase();
    let current = match normalized.as_str() {
        "always" => "always",
        "never" => "never",
        _ => "auto",
    };
    if current == choice { "●" } else { "○" }
}

fn fit_menu_row(row: &str, width: u16) -> String {
    super::display_width::pad_or_truncate(row, width as usize, false)
}

fn render_options_menu_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[(String, Option<TuiButton>)],
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
    selected: Option<TuiButton>,
    theme: super::theme::Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(theme.border(theme.cyan))
        .style(Style::default().bg(theme.bg));
    let inner = bordered_panel_inner(area);

    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let lines = rows
        .iter()
        .map(|(row, button)| {
            let row_text = fit_menu_row(row, inner.width);
            let style = match button {
                Some(button) if hover == Some(*button) || selected == Some(*button) => Style::default()
                    .fg(theme.bg)
                    .bg(theme.blue)
                    .add_modifier(Modifier::BOLD),
                Some(_) => theme.text_style().bg(theme.bg),
                None => Style::default().fg(theme.border_dim).bg(theme.bg),
            };
            Line::from(Span::styled(row_text, style))
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme.bg)), inner);

    for (idx, (_, button)) in rows.iter().enumerate() {
        if let (Some(button), Some(hitbox)) = (button, options_menu_row_hitbox(area, idx)) {
            buttons.record_button(*button, hitbox);
        }
    }
}

pub(super) fn active_options_menu_rows(
    browse: &BrowseState,
    archive_listing_mode: &str,
) -> Option<Vec<(String, Option<TuiButton>)>> {
    match browse.options_menu {
        BrowseOptionsMenu::Closed => None,
        BrowseOptionsMenu::Root => Some(options_root_rows(browse)),
        _ => options_submenu_rows(browse, archive_listing_mode).map(|(_, rows)| rows),
    }
}

#[cfg(test)]
mod options_menu_tests {
    use super::*;

    #[test]
    fn layout_gives_recovered_explore_space_to_browse_pane() {
        let mut browse = BrowseState::new();
        browse.explore_enabled = false;
        browse.info_enabled = true;
        browse.explore_collapsed = false;
        browse.info_collapsed = false;

        for width in [99, 100, 101] {
            let chunks = browse_content_layout(Rect::new(0, 0, width, 10), &browse);
            let expected_info = width / 3;

            assert_eq!(chunks[0].width, 0);
            assert_eq!(chunks[1].width, width - expected_info);
            assert_eq!(chunks[2].width, expected_info);
            assert!(
                chunks[2].width <= width / 3,
                "info pane exceeded one-third cap at width {width}"
            );
        }
    }

    #[test]
    fn layout_gives_recovered_info_space_to_browse_pane() {
        let mut browse = BrowseState::new();
        browse.explore_enabled = true;
        browse.info_enabled = false;
        browse.explore_collapsed = false;
        browse.info_collapsed = false;

        let chunks = browse_content_layout(Rect::new(0, 0, 100, 10), &browse);

        assert_eq!(chunks[0].width, 20);
        assert_eq!(chunks[1].width, 80);
        assert_eq!(chunks[2].width, 0);
    }

    #[test]
    fn archive_listing_submenu_marks_current_mode() {
        let mut browse = BrowseState::new();
        browse.options_menu = BrowseOptionsMenu::ArchiveListing;

        let (_, rows) = options_submenu_rows(&browse, "never").expect("archive rows");

        assert!(rows[0].0.starts_with(" ○ Auto"));
        assert!(rows[1].0.starts_with(" ○ Always"));
        assert!(rows[2].0.starts_with(" ● Never"));
    }

    #[test]
    fn archive_listing_submenu_treats_unknown_mode_as_auto() {
        let mut browse = BrowseState::new();
        browse.options_menu = BrowseOptionsMenu::ArchiveListing;

        let (_, rows) = options_submenu_rows(&browse, "unexpected").expect("archive rows");

        assert!(rows[0].0.starts_with(" ● Auto"));
        assert!(rows[1].0.starts_with(" ○ Always"));
        assert!(rows[2].0.starts_with(" ○ Never"));
    }

    #[test]
    fn submenu_geometry_prefers_right_when_there_is_room() {
        let outer = Rect::new(0, 0, 100, 20);
        let root = Rect::new(30, 1, 24, 10);

        let submenu = options_submenu_area(root, 20, 8, root.y, outer);

        assert_eq!(submenu.x, root.x + root.width);
        assert_eq!(horizontal_overlap(submenu.x, submenu.width, root.x, root.width), 0);
    }

    #[test]
    fn submenu_geometry_flips_left_before_overlapping_root() {
        let outer = Rect::new(0, 0, 60, 20);
        let root = Rect::new(30, 1, 24, 10);

        let submenu = options_submenu_area(root, 20, 8, root.y, outer);

        assert_eq!(submenu.x + submenu.width, root.x);
        assert_eq!(horizontal_overlap(submenu.x, submenu.width, root.x, root.width), 0);
    }

    #[test]
    fn submenu_geometry_stays_in_bounds_when_overlap_is_unavoidable() {
        let outer = Rect::new(0, 0, 30, 20);
        let root = Rect::new(5, 1, 22, 10);

        let submenu = options_submenu_area(root, 20, 8, root.y, outer);

        assert!(submenu.x >= outer.x);
        assert!(submenu.x + submenu.width <= outer.x + outer.width);
    }

    #[test]
    fn submenu_geometry_anchors_to_parent_row() {
        let outer = Rect::new(0, 0, 100, 20);
        let root = Rect::new(30, 1, 24, 10);
        let preferred_y = root.y + 1 + 4;

        let submenu = options_submenu_area(root, 20, 8, preferred_y, outer);

        assert_eq!(submenu.y, preferred_y);
    }

    #[test]
    fn options_menu_geometry_places_layout_submenu_on_parent_row() {
        let mut browse = BrowseState::new();
        browse.options_menu = BrowseOptionsMenu::Layout;
        let screen = Rect::new(0, 0, 120, 30);
        let toolbar = browse_toolbar_area_for_screen(screen);

        let geometry = options_menu_geometry_for_area(
            options_button_anchor_for_toolbar(toolbar),
            screen,
            &browse,
            "auto",
        );
        let submenu = geometry.submenu_area.expect("layout submenu");

        assert_eq!(submenu.y, geometry.root_area.y + 2);
        assert!(geometry.contains(submenu.x + 1, submenu.y));
    }


    #[test]
    fn options_menu_geometry_respects_nonzero_browse_area_origin() {
        let mut browse = BrowseState::new();
        browse.options_menu = BrowseOptionsMenu::Layout;
        let screen = Rect::new(7, 3, 120, 30);
        let toolbar = browse_toolbar_area_for_screen(screen);

        let geometry = options_menu_geometry_for_area(
            options_button_anchor_for_toolbar(toolbar),
            screen,
            &browse,
            "auto",
        );
        let submenu = geometry.submenu_area.expect("layout submenu");

        assert!(geometry.root_area.x >= screen.x);
        assert_eq!(geometry.root_area.y, screen.y + 9);
        assert!(submenu.x >= screen.x);
        assert!(submenu.y >= screen.y);
        assert!(geometry.contains(submenu.x + 1, submenu.y));
    }

    #[test]
    fn submenu_geometry_clamps_to_terminal_bottom() {
        let outer = Rect::new(0, 0, 100, 12);
        let root = Rect::new(30, 1, 24, 10);
        let preferred_y = 10;

        let submenu = options_submenu_area(root, 20, 8, preferred_y, outer);

        assert_eq!(submenu.y, 4);
        assert!(submenu.y + submenu.height <= outer.y + outer.height);
    }

    #[test]
    fn options_menu_hitboxes_match_bordered_panel_inner_rows() {
        let panel = Rect::new(10, 4, 24, 7);

        assert_eq!(options_menu_row_hitbox(panel, 0), Some(Rect::new(11, 5, 22, 1)));
        assert_eq!(options_menu_row_hitbox(panel, 4), Some(Rect::new(11, 9, 22, 1)));
        assert_eq!(options_menu_row_hitbox(panel, 5), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchControlKind {
    Recursive,
    Mode,
    Sort,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchControlLayoutItem {
    kind: SearchControlKind,
    x: u16,
    width: u16,
    label: String,
}

#[derive(Debug, Clone, Copy)]
enum SearchControlLabelTier {
    Full,
    Compact,
    Tiny,
}

fn browse_search_rows(active: bool) -> u16 {
    if active { 2 } else { 0 }
}

fn browse_header_y(area: Rect, search_active: bool) -> u16 {
    area.y + 1 + browse_search_rows(search_active)
}

fn browse_search_input_y(area: Rect) -> u16 {
    area.y + 1
}

fn browse_search_controls_y(area: Rect) -> u16 {
    area.y + 2
}

pub(super) fn browse_entry_y_start(area: Rect, search_active: bool) -> u16 {
    area.y + 2 + browse_search_rows(search_active)
}

fn search_control_button(kind: SearchControlKind) -> TuiButton {
    match kind {
        SearchControlKind::Recursive => TuiButton::BrowseSearchRecursive,
        SearchControlKind::Mode => TuiButton::BrowseSearchMode,
        SearchControlKind::Sort => TuiButton::BrowseSearchSort,
        SearchControlKind::Audio => TuiButton::BrowseSearchAudioOnly,
    }
}

fn search_label_abbrev(label: &str) -> &str {
    match label {
        "filename" => "file",
        "relevance" => "rel",
        "extension" => "ext",
        other => other,
    }
}

fn first_label_char(label: &str) -> char {
    label.chars().next().unwrap_or('?')
}

fn search_control_labels_for_tier(
    tier: SearchControlLabelTier,
    recursive: bool,
    mode_label: &str,
    sort_label: &str,
    sort_dir: SortDir,
    audio_only: bool,
) -> Vec<(SearchControlKind, String)> {
    let sort_arrow = match sort_dir {
        SortDir::Asc => "▲",
        SortDir::Desc => "▼",
    };
    let mode_compact = search_label_abbrev(mode_label);
    let sort_compact = search_label_abbrev(sort_label);
    let mode_tiny = first_label_char(mode_label);

    match tier {
        SearchControlLabelTier::Full => vec![
            (
                SearchControlKind::Recursive,
                if recursive { " recursive ✓ " } else { " recursive " }.to_string(),
            ),
            (SearchControlKind::Mode, format!(" mode: {} ", mode_label)),
            (
                SearchControlKind::Sort,
                format!(" sort: {} {} ", sort_label, sort_arrow),
            ),
            (
                SearchControlKind::Audio,
                if audio_only { " audio ✓ " } else { " all files " }.to_string(),
            ),
        ],
        SearchControlLabelTier::Compact => vec![
            (
                SearchControlKind::Recursive,
                if recursive { " rec ✓ " } else { " rec " }.to_string(),
            ),
            (SearchControlKind::Mode, format!(" mode:{} ", mode_compact)),
            (
                SearchControlKind::Sort,
                format!(" sort:{} {} ", sort_compact, sort_arrow),
            ),
            (
                SearchControlKind::Audio,
                if audio_only { " audio ✓ " } else { " files " }.to_string(),
            ),
        ],
        SearchControlLabelTier::Tiny => vec![
            (
                SearchControlKind::Recursive,
                if recursive { " r✓ " } else { " r " }.to_string(),
            ),
            (SearchControlKind::Mode, format!(" m:{} ", mode_tiny)),
            (SearchControlKind::Sort, format!(" s{} ", sort_arrow)),
            (
                SearchControlKind::Audio,
                if audio_only { " a✓ " } else { " a " }.to_string(),
            ),
        ],
    }
}

fn search_control_label_width(label: &str) -> usize {
    super::display_width::width(label)
}

fn search_control_total_width(labels: &[(SearchControlKind, String)]) -> usize {
    labels
        .iter()
        .enumerate()
        .map(|(idx, (_, label))| {
            let gap = if idx > 0 { 1 } else { 0 };
            search_control_label_width(label) + gap
        })
        .sum()
}

fn place_search_control_labels(
    inner_width: usize,
    labels: Vec<(SearchControlKind, String)>,
    require_all: bool,
) -> Option<Vec<SearchControlLayoutItem>> {
    let mut items = Vec::new();
    let mut used = 0usize;

    for (idx, (kind, label)) in labels.into_iter().enumerate() {
        let gap = if idx > 0 { 1 } else { 0 };
        let label_width = search_control_label_width(&label);
        if used + gap + label_width > inner_width {
            if require_all {
                return None;
            }
            break;
        }
        used += gap;
        items.push(SearchControlLayoutItem {
            kind,
            x: used as u16,
            width: label_width as u16,
            label,
        });
        used += label_width;
    }

    Some(items)
}

fn search_control_row_layout(
    inner_width: usize,
    recursive: bool,
    mode_label: &str,
    sort_label: &str,
    sort_dir: SortDir,
    audio_only: bool,
) -> Vec<SearchControlLayoutItem> {
    for tier in [
        SearchControlLabelTier::Full,
        SearchControlLabelTier::Compact,
        SearchControlLabelTier::Tiny,
    ] {
        let labels = search_control_labels_for_tier(
            tier,
            recursive,
            mode_label,
            sort_label,
            sort_dir,
            audio_only,
        );
        if search_control_total_width(&labels) <= inner_width {
            return place_search_control_labels(inner_width, labels, true).unwrap_or_default();
        }
    }

    // Pathological widths cannot display every control. Keep the left-to-right
    // control order and register only fully visible controls; never allow text
    // or hitboxes to run into the border or adjacent panes.
    let labels = search_control_labels_for_tier(
        SearchControlLabelTier::Tiny,
        recursive,
        mode_label,
        sort_label,
        sort_dir,
        audio_only,
    );
    place_search_control_labels(inner_width, labels, false).unwrap_or_default()
}

fn search_control_style(
    kind: SearchControlKind,
    recursive: bool,
    audio_only: bool,
    theme: super::theme::Theme,
) -> Style {
    match kind {
        SearchControlKind::Recursive if recursive => Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.green)
            .add_modifier(Modifier::BOLD),
        SearchControlKind::Audio if audio_only => Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.green)
            .add_modifier(Modifier::BOLD),
        SearchControlKind::Recursive | SearchControlKind::Audio => {
            Style::default().fg(theme.text_dim).bg(theme.surface)
        }
        SearchControlKind::Mode | SearchControlKind::Sort => {
            Style::default().fg(theme.text_bright).bg(theme.surface)
        }
    }
}

fn clipped_inner_row_rect(area: Rect, y: u16, x_offset: u16, width: u16) -> Option<Rect> {
    let inner_left = area.x.saturating_add(1);
    let inner_right_exclusive = area.x.saturating_add(area.width.saturating_sub(1));
    let x = inner_left.saturating_add(x_offset);
    if x >= inner_right_exclusive {
        return None;
    }
    let end = x.saturating_add(width).min(inner_right_exclusive);
    if end <= x {
        return None;
    }
    Some(Rect::new(x, y, end - x, 1))
}

#[cfg(test)]
mod search_panel_geometry_tests {
    use super::*;

    fn assert_layout_inside(inner_width: usize, items: &[SearchControlLayoutItem]) {
        for item in items {
            assert!((item.x as usize) + (item.width as usize) <= inner_width);
        }
    }

    #[test]
    fn search_panel_row_order_keeps_header_attached_to_results() {
        let area = Rect::new(10, 4, 90, 20);

        assert_eq!(browse_search_input_y(area), 5);
        assert_eq!(browse_search_controls_y(area), 6);
        assert_eq!(browse_header_y(area, true), 7);
        assert_eq!(browse_entry_y_start(area, true), 8);

        assert!(browse_search_input_y(area) < browse_search_controls_y(area));
        assert!(browse_search_controls_y(area) < browse_header_y(area, true));
        assert!(browse_header_y(area, true) < browse_entry_y_start(area, true));
    }

    #[test]
    fn search_control_layout_matches_full_width_visual_order() {
        let items = search_control_row_layout(
            80,
            true,
            "filename",
            "relevance",
            SortDir::Asc,
            false,
        );

        assert_eq!(
            items.iter().map(|item| item.kind).collect::<Vec<_>>(),
            vec![
                SearchControlKind::Recursive,
                SearchControlKind::Mode,
                SearchControlKind::Sort,
                SearchControlKind::Audio,
            ]
        );
        assert_eq!(items[0].label, " recursive ✓ ");
        assert_eq!(items[1].label, " mode: filename ");
        assert_eq!(items[2].label, " sort: relevance ▲ ");
        assert_eq!(items[3].label, " all files ");
        assert_layout_inside(80, &items);
    }

    #[test]
    fn search_control_layout_compacts_before_clipping() {
        let items = search_control_row_layout(
            30,
            true,
            "filename",
            "relevance",
            SortDir::Desc,
            true,
        );

        assert_eq!(items.len(), 4);
        assert_eq!(items[0].label, " r✓ ");
        assert_eq!(items[1].label, " m:f ");
        assert_eq!(items[2].label, " s▼ ");
        assert_eq!(items[3].label, " a✓ ");
        assert_layout_inside(30, &items);
    }

    #[test]
    fn search_control_layout_never_overflows_narrow_rows() {
        for inner_width in 0..64 {
            let items = search_control_row_layout(
                inner_width,
                true,
                "filename",
                "relevance",
                SortDir::Asc,
                false,
            );
            assert_layout_inside(inner_width, &items);
        }
    }

    #[test]
    fn search_control_hitboxes_are_clipped_to_inner_panel() {
        let area = Rect::new(20, 3, 12, 8);
        let inner_width = area.width.saturating_sub(2) as usize;
        let items = search_control_row_layout(
            inner_width,
            true,
            "filename",
            "relevance",
            SortDir::Asc,
            true,
        );
        let y = browse_search_controls_y(area);

        for item in &items {
            let rect = clipped_inner_row_rect(area, y, item.x, item.width).expect("visible hitbox");
            assert!(rect.x >= area.x + 1);
            assert!(rect.x + rect.width <= area.x + area.width - 1);
            assert_eq!(rect.y, y);
            assert_eq!(rect.height, 1);
        }
    }
}

/// Register mouse click targets for the browse list: column headers,
/// individual entry rows, and a catch-all list area for scroll wheel routing.
fn register_browse_buttons(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    browse: &BrowseState,
    inline_edit: Option<&BrowseInlineEditState>,
    create_row_active: bool,
) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    // The whole list area (outer rect) is the scroll-wheel catch-all.
    buttons.record_button(TuiButton::BrowseList, area);

    let w = area.width as usize;
    let inner_w = w.saturating_sub(2);
    if inner_w <= ROW_PREFIX + ROW_TRAILING + MIN_NAME_W {
        return;
    }
    let columns = browse_column_layout(inner_w, &browse.columns);
    let name_width = name_column_width(&columns);
    let inline_rename_path = inline_edit.and_then(|state| match &state.target {
        BrowseInlineEditTarget::Rename { path } => Some(path),
        _ => None,
    });

    // Column x-offsets (relative to area.x). The header sits immediately above
    // result rows; an active search panel occupies the first two rows inside
    // the border.
    let search_rows = browse_search_rows(browse.search.active);
    let header_y = browse_header_y(area, browse.search.active);
    let mut x = area.x + 1 + ROW_PREFIX as u16;
    for cell in &columns {
        buttons.record_button(
            TuiButton::BrowseColumn(cell.column),
            Rect::new(x, header_y, cell.width as u16, 1),
        );
        x = x.saturating_add(cell.width as u16).saturating_add(1);
    }

    // Search toggle in the top border (right-aligned "search" label).
    {
        let search_label_w = if browse.search.active { 10u16 } else { 8u16 }; // " search ✓ " or " search "
        let search_x = area.x + area.width - search_label_w - 1;
        buttons.record_button(
            TuiButton::BrowseSearchToggle,
            Rect::new(search_x, area.y, search_label_w, 1),
        );
    }

    // Search panel controls (if active): input row first, then all option
    // controls grouped together on the second row. The helper is shared with
    // drawing so click targets cannot drift from the rendered geometry.
    if browse.search.active {
        buttons.record_button(
            TuiButton::BrowseSearchInput,
            Rect::new(
                area.x.saturating_add(4),
                browse_search_input_y(area),
                inner_w.saturating_sub(3) as u16,
                1,
            ),
        );
        let controls_y = browse_search_controls_y(area);
        for item in search_control_row_layout(
            inner_w,
            browse.search.recursive,
            browse.search.mode.label(),
            browse.search.sort.label(),
            browse.search.sort_dir,
            browse.search.audio_only,
        ) {
            if let Some(rect) = clipped_inner_row_rect(area, controls_y, item.x, item.width) {
                buttons.record_button(search_control_button(item.kind), rect);
            }
        }
    }

    if browse.filter_input.is_some() {
        buttons.record_button(
            TuiButton::BrowseFilterInput,
            Rect::new(
                area.x.saturating_add(4),
                area.y.saturating_add(area.height.saturating_sub(2)),
                inner_w.saturating_sub(4) as u16,
                1,
            ),
        );
    }

    // Entry rows: below header (and search panel if active), above bottom border.
    // Mirror the renderer: an active create prompt reserves one list row, so
    // no entry button may be registered where the create editor is drawn.
    let entry_y_start = browse_entry_y_start(area, browse.search.active);
    let content_height = (area.height as usize).saturating_sub(3 + search_rows as usize);
    let entry_capacity = content_height.saturating_sub(usize::from(create_row_active));
    let start = browse.scroll_offset;
    let end = (start + entry_capacity).min(browse.entries.len());
    if create_row_active && content_height > 0 {
        let create_row = (end - start).min(content_height - 1) as u16;
        let y = entry_y_start + create_row;
        buttons.record_button(
            TuiButton::BrowseCreateRow,
            Rect::new(area.x + 1, y, inner_w as u16, 1),
        );
        buttons.record_button(
            TuiButton::BrowseFileInlineEdit,
            Rect::new(
                area.x + 1 + ROW_PREFIX as u16,
                y,
                name_width as u16,
                1,
            ),
        );
    }
    for (row, i) in (start..end).enumerate() {
        let y = entry_y_start + row as u16;
        let row_rect = Rect::new(area.x + 1, y, inner_w as u16, 1);
        buttons.record_button(TuiButton::BrowseEntry(i), row_rect);
        // Record the gutter after the body so reverse hit-testing gives it
        // priority over the enclosing row hit target.
        buttons.record_button(
            TuiButton::BrowseEntryGutter(i),
            Rect::new(
                area.x + 1 + ROW_CURSOR_W as u16,
                y,
                ROW_GUTTER_W as u16,
                1,
            ),
        );
        if inline_rename_path.is_some_and(|path| path == &browse.entries[i].path) {
            buttons.record_button(
                TuiButton::BrowseFileInlineEdit,
                Rect::new(
                    area.x + 1 + ROW_PREFIX as u16,
                    y,
                    name_width as u16,
                    1,
                ),
            );
        }
    }
}

fn render_path_input_spans(
    input: &TextInputState,
    inner_width: usize,
    theme: super::theme::Theme,
) -> (Vec<Span<'static>>, u16) {
    let prefix = " path: ";
    let prefix_w = super::display_width::width(prefix);
    let input_max = inner_width
        .saturating_sub(prefix_w)
        .saturating_sub(1)
        .max(1);

    let mut spans = vec![Span::styled(prefix, Style::default().fg(theme.blue))];
    spans.extend(render_inline_value_with_embedded_cursor(input, input_max, theme));

    let cursor_col = prefix_w as u16 + inline_cursor_col(input, input_max);
    (spans, cursor_col)
}

/// Draw the path bar inline (no border — caller provides the containing border).
fn draw_breadcrumb_inline(f: &mut Frame, area: Rect, browse: &BrowseState, theme: super::theme::Theme) {
    if area.width < 10 {
        return;
    }

    // Editable path input mode.
    if let Some(ref input) = browse.path_input {
        let (spans, cursor_col) = render_path_input_spans(input, area.width as usize, theme);
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, area);

        let cursor_x = area.x + cursor_col;
        if cursor_x < area.x + area.width {
            f.set_cursor(cursor_x, area.y);
        }
        return;
    }

    // Read-only display mode
    let display = if let Some(ref arc) = browse.archive {
        let archive_name = arc
            .listing
            .archive_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let dirty = arc.staging.as_ref().is_some_and(|staging| staging.dirty);
        let marker = if dirty { " [modified]" } else { "" };
        if arc.inner_path.is_empty() {
            format!("{}:/{}", archive_name, marker)
        } else {
            format!("{}:/{}{}", archive_name, arc.inner_path, marker)
        }
    } else {
        let path_str = browse.current_dir.display().to_string();
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() && path_str.starts_with(&home) {
            format!("~{}", &path_str[home.len()..])
        } else {
            path_str
        }
    };

    let filter_suffix = if !browse.filter_text.is_empty() {
        format!("   filter: {}", browse.filter_text)
    } else {
        String::new()
    };

    let type_ahead_suffix = if browse.type_ahead_active() {
        format!("   jump: {}", browse.type_ahead_buffer)
    } else {
        String::new()
    };

    let prefix = " path: ";
    let prefix_w = super::display_width::width(prefix);
    let suffix_w = super::display_width::width(&filter_suffix)
        + super::display_width::width(&type_ahead_suffix);
    let path_max = (area.width as usize)
        .saturating_sub(prefix_w)
        .saturating_sub(suffix_w)
        .saturating_sub(1);
    let display_truncated = truncate_left(&display, path_max);

    let mut spans = vec![
        Span::styled(prefix, theme.muted()),
        Span::styled(display_truncated, theme.bright()),
    ];
    if !filter_suffix.is_empty() {
        spans.push(Span::styled(
            filter_suffix,
            Style::default().fg(theme.amber),
        ));
    }
    if !type_ahead_suffix.is_empty() {
        spans.push(Span::styled(
            type_ahead_suffix,
            Style::default().fg(theme.cyan),
        ));
    }

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);
}

/// Truncate a string from the LEFT to fit `max` chars, prepending `…` if cut.
/// Used so the end of paths (most contextual portion) stays visible.
fn truncate_left(s: &str, max: usize) -> String {
    super::display_width::truncate_left(s, max)
}

fn browse_scan_progress_text(browse: &BrowseState) -> Option<String> {
    browse.scan_pending.as_ref()?;
    let folder = browse
        .current_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| browse.current_dir.display().to_string());
    Some(format!(
        "Reading {}… ({})",
        folder, browse.scan_discovered_count
    ))
}

/// Draw the directory listing (left pane) with a sortable column header row.
/// Reserves an extra row for the live filter input when one is active.
fn draw_browse_list(
    f: &mut Frame,
    area: Rect,
    browse: &mut BrowseState,
    inline_edit: Option<&BrowseInlineEditState>,
    hover: Option<super::button_map::TuiButton>,
    navigation_focus_active: bool,
    theme: super::theme::Theme,
) -> Option<(Rect, Rect)> {
    if area.height < 4 || area.width < 20 {
        return None;
    }

    let border_color = theme.cyan;
    let w = area.width as usize;
    let inner_w = w.saturating_sub(2);

    // Top border with solid title bar
    let title = "▾ browse ";
    let search_label = if browse.search.active {
        " search ✓ "
    } else {
        " search "
    };
    let search_display_w = super::display_width::width(search_label);
    let title_w = super::display_width::width(&title);
    let fill_count = w.saturating_sub(1 + title_w + search_display_w + 1);

    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let search_style = if browse.search.active {
        Style::default()
            .fg(theme.green)
            .bg(border_color)
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        bar_style
    };
    let top_line = Line::from(vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(title, bar_style),
        Span::styled(" ".repeat(fill_count), bar_style),
        Span::styled(search_label, search_style),
        Span::styled("┐", theme.border(border_color)),
    ]);

    let bot_line = if let Some(progress) = browse_scan_progress_text(browse) {
        let available = w.saturating_sub(2);
        let decorated = format!(" {} ", progress);
        let label = super::display_width::truncate_right(&decorated, available);
        let label_w = super::display_width::width(&label);
        Line::from(vec![
            Span::styled("└", theme.border(border_color)),
            Span::styled(label, theme.muted()),
            Span::styled(
                "─".repeat(available.saturating_sub(label_w)),
                theme.border(border_color),
            ),
            Span::styled("┘", theme.border(border_color)),
        ])
    } else {
        Line::from(Span::styled(
            format!("└{}┘", "─".repeat(w.saturating_sub(2))),
            theme.border(border_color),
        ))
    };

    // Content rows = total - top border - header - bottom border
    // (-1 if filter row, -2 if search panel).
    let has_filter = browse.filter_input.is_some();
    let has_search = browse.search.active;
    let reserved = if has_search {
        5 // top border + header + 2 search rows + bottom border
    } else if has_filter {
        4
    } else {
        3
    };
    let content_height = (area.height as usize).saturating_sub(reserved);
    let create_input = inline_edit.and_then(|state| match &state.target {
        BrowseInlineEditTarget::Create { dir, .. } if dir == &browse.current_dir => {
            Some(&state.input)
        }
        _ => None,
    });
    let entry_capacity = effective_entry_capacity(content_height, create_input.is_some());
    browse.set_visible_height(entry_capacity);

    let column_layout = browse_column_layout(inner_w, &browse.columns);
    let name_w = name_column_width(&column_layout);

    let mut lines: Vec<Line> = Vec::with_capacity(area.height as usize);
    lines.push(top_line);

    // Search panel (2 rows when active): input first, then peer controls.
    if browse.search.active {
        // Row 1: full-width search input.
        // Layout: │ + " / "(3) + input(input_w) + │
        let input_w = inner_w.saturating_sub(3);
        let mut search_spans = vec![
            Span::styled("│", theme.border(border_color)),
            Span::styled(" / ", Style::default().fg(theme.amber)),
        ];
        search_spans.extend(render_text_input_value_with_style(
            &browse.search.input,
            input_w,
            browse.search.focus == super::browse::SearchFocus::Input,
            Style::default().fg(theme.text_bright).bg(theme.surface),
            theme,
        ));
        search_spans.push(Span::styled("│", theme.border(border_color)));
        lines.push(Line::from(search_spans));

        // Row 2: recursive + mode + sort + audio, all visibly clickable.
        // The shared layout helper progressively compacts labels and finally
        // omits trailing controls only when the pane is too narrow to display
        // every tiny pill without colliding with the right border.
        let control_items = search_control_row_layout(
            inner_w,
            browse.search.recursive,
            browse.search.mode.label(),
            browse.search.sort.label(),
            browse.search.sort_dir,
            browse.search.audio_only,
        );
        let mut row2_spans = vec![Span::styled("│", theme.border(border_color))];
        let mut used = 0usize;
        for item in &control_items {
            let x = item.x as usize;
            if x > used {
                row2_spans.push(Span::raw(" ".repeat(x - used)));
            }
            row2_spans.push(Span::styled(
                item.label.clone(),
                search_control_style(
                    item.kind,
                    browse.search.recursive,
                    browse.search.audio_only,
                    theme,
                ),
            ));
            used = x + item.width as usize;
        }
        if inner_w > used {
            row2_spans.push(Span::raw(" ".repeat(inner_w - used)));
        }
        row2_spans.push(Span::styled("│", theme.border(border_color)));
        lines.push(Line::from(row2_spans));
    }

    // Header row: after the search panel so the columns anchor to results.
    lines.push(render_header_row(
        border_color,
        w,
        &column_layout,
        browse.sort_by,
        browse.sort_dir,
        theme,
    ));

    let mut rename_cursor: Option<(usize, u16)> = None;

    if let Some(err) = &browse.error {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   {}", err),
                Style::default().fg(theme.destructive),
            )], theme));
        for _ in 1..content_height {
            lines.push(empty_bordered_line(border_color, w, theme));
        }
    } else if browse.entries.is_empty() {
        if let Some(input) = create_input {
            rename_cursor = Some((0, inline_cursor_col(input, name_w)));
            lines.push(render_browse_create_line(
                border_color,
                w,
                name_w,
                input,
                theme,
            ));
        } else {
            let msg = browse_scan_progress_text(browse)
                .map(|progress| format!("   {progress}"))
                .unwrap_or_else(|| "   (empty)".to_string());
            lines.push(bordered_line(
                border_color,
                w,
                vec![Span::styled(msg, theme.muted())],
                theme,
            ));
        }
        for _ in 1..content_height {
            lines.push(empty_bordered_line(border_color, w, theme));
        }
    } else {
        let start = browse.scroll_offset;
        // The same effective capacity drives rendering and BrowseState's
        // scrolling/cursor/scrollbar calculations. This keeps the final entry
        // reachable while an inline creation editor owns one visual row.
        let end = (start + entry_capacity).min(browse.entries.len());

        for i in start..end {
            let entry = &browse.entries[i];
            let is_selected = i == browse.selected_index
                && navigation_focus_active
                && browse.files_navigation_active();
            let is_checked = browse.is_multi_selected(&entry.path);
            let is_range_preview = browse.is_range_preview_index(i);
            let is_hovered =
                !is_selected && hover == Some(super::button_map::TuiButton::BrowseEntry(i));
            let inline_rename_input = inline_edit.and_then(|state| match &state.target {
                BrowseInlineEditTarget::Rename { path } if path == &entry.path => Some(&state.input),
                _ => None,
            });
            if let Some(input) = inline_rename_input {
                rename_cursor = Some((i - start, inline_cursor_col(input, name_w)));
            }
            lines.push(render_entry_line(
                border_color,
                w,
                &column_layout,
                browse,
                entry,
                inline_rename_input,
                is_selected,
                is_checked,
                is_range_preview,
                is_hovered, theme));
        }

        let mut rendered = end - start;
        if let Some(input) = create_input {
            if rendered < content_height {
                rename_cursor = Some((rendered, inline_cursor_col(input, name_w)));
                lines.push(render_browse_create_line(
                    border_color,
                    w,
                    name_w,
                    input,
                    theme,
                ));
                rendered += 1;
            }
        }
        for _ in rendered..content_height {
            lines.push(empty_bordered_line(border_color, w, theme));
        }
    }

    // Filter input row (just above the bottom border) when active.
    let mut filter_cursor: Option<u16> = None;
    if let Some(input) = &browse.filter_input {
        // Inside row layout: │ + " / " + <input view> + padding + │
        // Reserve 1 (left border) + 3 (" / ") + 2 (right padding + border) = 6
        let input_width = inner_w.saturating_sub(4); // " / " prefix takes 3 + 1 trailing space
        let (_, cursor_col_in_view) = input.view(input_width);
        filter_cursor = Some(cursor_col_in_view);

        let mut filter_spans = vec![
            Span::styled("│", theme.border(border_color)),
            Span::styled(" / ", Style::default().fg(theme.cyan)),
        ];
        filter_spans.extend(render_text_input_value_with_style(
            input,
            input_width,
            true,
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.input_focused_bg),
            theme,
        ));
        filter_spans.push(Span::raw(" "));
        filter_spans.push(Span::styled("│", theme.border(border_color)));
        lines.push(Line::from(filter_spans));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    // Position the terminal cursor inside the search input or filter input.
    if browse.search.active && browse.search.focus == super::browse::SearchFocus::Input {
        let input_w = inner_w.saturating_sub(3);
        let (_, cursor_col) = browse.search.input.view(input_w);
        let cursor_x = area.x + 1 + 3 + cursor_col; // border + " / " prefix
        let cursor_y = browse_search_input_y(area); // top border + first search row
        f.set_cursor(cursor_x, cursor_y);
    } else if let Some(col_in_view) = filter_cursor {
        let cursor_x = area.x + 1 + 3 + col_in_view;
        let cursor_y = area.y + area.height - 2;
        f.set_cursor(cursor_x, cursor_y);
    } else if let Some((row, col_in_view)) = rename_cursor {
        let cursor_x = area.x + 1 + ROW_PREFIX as u16 + col_in_view;
        let cursor_y = browse_entry_y_start(area, browse.search.active) + row as u16;
        f.set_cursor(cursor_x, cursor_y);
    }

    draw_vertical_scrollbar(
        f,
        Rect::new(
            area.right().saturating_sub(2),
            browse_entry_y_start(area, browse.search.active),
            1,
            content_height as u16,
        ),
        browse.entries.len(),
        browse.visible_height,
        browse.scroll_offset,
        theme,
    )
}


fn render_browse_create_line(
    border_color: ratatui::style::Color,
    width: usize,
    name_width: usize,
    input: &TextInputState,
    theme: super::theme::Theme,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(ROW_PREFIX))];
    spans.extend(render_inline_value_with_embedded_cursor(
        input,
        name_width,
        theme,
    ));
    bordered_line(border_color, width, spans, theme)
}

/// Render the column header row with sort indicator (▲/▼) on the active column.
fn normalize_bordered_spans_width(
    spans: &mut Vec<Span<'static>>,
    target_width: usize,
) {
    let Some(right_border) = spans.pop() else {
        return;
    };
    let content_target = target_width.saturating_sub(right_border.width());
    let mut normalized = Vec::with_capacity(spans.len() + 2);
    let mut remaining = content_target;
    for span in spans.drain(..) {
        if remaining == 0 {
            break;
        }
        let span_width = span.width();
        if span_width <= remaining {
            remaining -= span_width;
            normalized.push(span);
        } else {
            let fitted = super::display_width::fit_prefix(span.content.as_ref(), remaining);
            normalized.push(Span::styled(fitted, span.style));
            remaining = 0;
        }
    }
    if remaining > 0 {
        normalized.push(Span::raw(" ".repeat(remaining)));
    }
    normalized.push(right_border);
    *spans = normalized;
}

fn render_header_row(
    border_color: ratatui::style::Color,
    width: usize,
    columns: &[BrowseColumnCell],
    sort_by: SortBy,
    sort_dir: SortDir,
    theme: super::theme::Theme,
) -> Line<'static> {
    let arrow = match sort_dir {
        SortDir::Asc => "▲",
        SortDir::Desc => "▼",
    };

    let mut spans = Vec::with_capacity(3 + columns.len().saturating_mul(2));
    spans.push(Span::styled("│", theme.border(border_color)));
    spans.push(Span::raw(" ".repeat(ROW_PREFIX)));

    for (idx, cell) in columns.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        let is_active = sort_by == cell.column.sort_by();
        let style = if is_active {
            Style::default()
                .fg(theme.cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        let text = if is_active {
            format!("{} {}", cell.column.config_key(), arrow)
        } else {
            cell.column.config_key().to_string()
        };
        let display = pad_or_truncate(&text, cell.width, column_right_aligned(cell.column));
        spans.push(Span::styled(display, style));
    }

    spans.push(Span::raw(" ".repeat(ROW_TRAILING)));
    spans.push(Span::styled("│", theme.border(border_color)));

    // Symmetrically pad or trim before the right border.
    normalize_bordered_spans_width(&mut spans, width);

    Line::from(spans)
}

/// Render a single entry row using the active configured Browse columns.
fn render_entry_line(
    border_color: ratatui::style::Color,
    width: usize,
    columns: &[BrowseColumnCell],
    browse: &BrowseState,
    entry: &BrowseEntry,
    inline_rename_input: Option<&TextInputState>,
    is_selected: bool,
    is_checked: bool,
    is_range_preview: bool,
    is_hovered: bool,
    theme: super::theme::Theme,
) -> Line<'static> {
    // Cursor indicator
    let cursor = if is_selected { "▸ " } else { "  " };
    let cursor_style = if is_selected {
        Style::default().fg(theme.blue)
    } else {
        Style::default().fg(theme.text_dim)
    };

    // Multi-select gutter. Range preview is deliberately distinct from
    // committed marks while the marker remains visible in constrained themes.
    let check = if is_checked { "●" } else { " " };
    let mut check_style = if is_range_preview {
        Style::default().fg(theme.bg).bg(theme.selection_bg)
    } else if is_checked {
        Style::default().fg(theme.cyan)
    } else {
        Style::default().fg(theme.text_dim)
    };
    if is_range_preview {
        check_style = check_style.add_modifier(Modifier::BOLD);
    }

    let cached = cached_info_for_entry(browse, entry);
    let mut spans = Vec::with_capacity(7 + columns.len().saturating_mul(2));
    let mut inline_editor_span_range: Option<std::ops::Range<usize>> = None;
    spans.push(Span::styled("│", theme.border(border_color)));
    spans.push(Span::styled(cursor, cursor_style));
    spans.push(Span::styled(check, check_style));
    spans.push(Span::raw(" "));

    for (idx, cell) in columns.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        if cell.column == BrowseColumn::Name {
            if let Some(input) = inline_rename_input {
                let start = spans.len();
                spans.extend(render_inline_value_with_embedded_cursor(input, cell.width, theme));
                inline_editor_span_range = Some(start..spans.len());
            } else {
                let name_display = pad_or_truncate(&entry.name, cell.width, false);
                spans.push(Span::styled(name_display, entry_name_style(entry, is_selected, theme)));
            }
        } else {
            let value = entry_column_text(entry, cell.column, cached);
            let display = pad_or_truncate(value.as_ref(), cell.width, column_right_aligned(cell.column));
            spans.push(Span::styled(display, entry_column_style(entry, cell.column, cached, theme)));
        }
    }

    spans.push(Span::raw(" ".repeat(ROW_TRAILING)));
    spans.push(Span::styled("│", theme.border(border_color)));

    // Symmetrically pad or trim before the right border.
    normalize_bordered_spans_width(&mut spans, width);

    // Selected row gets a subtle bg highlight; hovered row gets a dimmer one.
    let bg = if is_selected {
        Some(theme.selection_bg)
    } else if is_range_preview {
        Some(theme.selection_bg)
    } else if is_hovered {
        Some(theme.hover_bg)
    } else {
        None
    };
    if let Some(bg_color) = bg {
        for (span_index, span) in spans.iter_mut().enumerate() {
            let inline_editor_owns_style = inline_editor_span_range
                .as_ref()
                .is_some_and(|range| range.contains(&span_index));
            if !inline_editor_owns_style && !matches!(span.content.as_ref(), "│") {
                span.style = span.style.bg(bg_color);
                if is_selected {
                    span.style = span
                        .style
                        .fg(theme.text_bright)
                        .add_modifier(Modifier::BOLD);
                }
            }
        }
    }

    Line::from(spans)
}

fn cached_info_for_entry<'a>(browse: &'a BrowseState, entry: &BrowseEntry) -> Option<&'a CachedInfo> {
    browse.valid_probe_for_entry(entry)
}

fn entry_name_style(
    entry: &BrowseEntry,
    is_selected: bool,
    theme: super::theme::Theme,
) -> Style {
    if entry.is_broken_symlink {
        return Style::default().fg(theme.destructive);
    }
    match &entry.kind {
        EntryKind::ParentDir => Style::default().fg(theme.text_muted),
        EntryKind::Directory => Style::default().fg(theme.blue),
        EntryKind::DvdAudioDir | EntryKind::DvdVideoDir | EntryKind::BlurayDir => {
            Style::default().fg(theme.purple)
        }
        EntryKind::AudioFile(_) => {
            if is_selected {
                Style::default()
                    .fg(theme.text_bright)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            }
        }
        EntryKind::Archive => Style::default().fg(theme.amber),
        EntryKind::SacdIso
        | EntryKind::DvdAudioIso
        | EntryKind::DvdVideoIso
        | EntryKind::BlurayIso => Style::default().fg(theme.purple),
        EntryKind::OtherFile => Style::default().fg(theme.text_dim),
    }
}

fn entry_column_style(
    entry: &BrowseEntry,
    column: BrowseColumn,
    cached: Option<&CachedInfo>,
    theme: super::theme::Theme,
) -> Style {
    let audio_column = is_audio_column(column);
    if audio_column && !entry.is_audio() {
        return Style::default().fg(theme.text_dim);
    }
    if audio_column && cached.is_none() {
        return Style::default().fg(theme.text_dim);
    }
    theme.muted()
}

fn entry_column_text<'a>(
    entry: &'a BrowseEntry,
    column: BrowseColumn,
    cached: Option<&'a CachedInfo>,
) -> Cow<'a, str> {
    match column {
        BrowseColumn::Name => Cow::Borrowed(entry.name.as_str()),
        BrowseColumn::Size => match &entry.kind {
            EntryKind::ParentDir | EntryKind::Directory => Cow::Borrowed(""),
            _ => Cow::Owned(size_str(entry.size)),
        },
        BrowseColumn::Date => Cow::Owned(entry.date_label()),
        BrowseColumn::Type => Cow::Owned(entry.type_label()),
        BrowseColumn::Format => audio_column_value(entry, cached, |info| {
            non_empty_str(info.source.format_name.as_str())
                .map(Cow::Borrowed)
                .or_else(|| Some(Cow::Owned(entry.type_label())))
        }),
        BrowseColumn::Codec => audio_column_value(entry, cached, |info| {
            non_empty_owned(info.source.codec_display())
        }),
        BrowseColumn::SampleRate => audio_column_value(entry, cached, |info| {
            (info.source.sample_rate > 0).then(|| Cow::Owned(info.source.sample_rate_display()))
        }),
        BrowseColumn::Channels => audio_column_value(entry, cached, |info| {
            (info.source.channels > 0).then(|| Cow::Owned(info.source.channels_display()))
        }),
        BrowseColumn::Duration => audio_column_value(entry, cached, |info| {
            (info.source.duration_secs.is_finite() && info.source.duration_secs > 0.0)
                .then(|| Cow::Owned(info.source.duration_display()))
        }),
        BrowseColumn::Artist => audio_column_value(entry, cached, |info| {
            info.metadata
                .artist
                .as_deref()
                .and_then(non_empty_str)
                .map(Cow::Borrowed)
        }),
        BrowseColumn::Album => audio_column_value(entry, cached, |info| {
            info.metadata
                .album
                .as_deref()
                .and_then(non_empty_str)
                .map(Cow::Borrowed)
        }),
    }
}

fn audio_column_value<'a, F>(
    entry: &BrowseEntry,
    cached: Option<&'a CachedInfo>,
    value: F,
) -> Cow<'a, str>
where
    F: FnOnce(&'a CachedInfo) -> Option<Cow<'a, str>>,
{
    if !entry.is_audio() {
        return Cow::Borrowed("—");
    }
    cached.and_then(value).unwrap_or(Cow::Borrowed("—"))
}

fn non_empty_str(value: &str) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn non_empty_owned<'a>(value: String) -> Option<Cow<'a, str>> {
    if value.trim().is_empty() {
        None
    } else {
        Some(Cow::Owned(value))
    }
}

fn is_audio_column(column: BrowseColumn) -> bool {
    matches!(
        column,
        BrowseColumn::Format
            | BrowseColumn::Codec
            | BrowseColumn::SampleRate
            | BrowseColumn::Channels
            | BrowseColumn::Duration
            | BrowseColumn::Artist
            | BrowseColumn::Album
    )
}

/// Pad a string to `width` chars, or truncate with ellipsis if too long.
/// `right_align` pads on the left when true.
fn pad_or_truncate(s: &str, width: usize, right_align: bool) -> String {
    super::display_width::pad_or_truncate(s, width, right_align)
}

/// Draw the info pane (right pane) showing details for the selected entry
/// Content returned by `entry_info_lines`: the visual lines plus a
/// mapping of which metadata fields appear at which line indices (for
/// registering click targets).
struct InfoContent {
    lines: Vec<Vec<Span<'static>>>,
    /// (field, line_index) for each clickable metadata row. Fields
    /// that are absent but have a "(click to add)" placeholder also
    /// appear here.
    meta_field_rows: Vec<(MetadataField, usize)>,
    /// Line index of the analyze pill (if present).
    analyze_pill_row: Option<usize>,
    /// Line index of the edit tags pill (if present).
    edit_tags_pill_row: Option<usize>,
    /// Line index of the Audio Streams pill (if present).
    audio_streams_pill_row: Option<usize>,
    /// Cursor position for an active inline metadata editor: (line, column).
    inline_cursor: Option<(usize, u16)>,
}

fn draw_browse_info(
    f: &mut Frame,
    area: Rect,
    browse: &BrowseState,
    inline_edit: Option<&BrowseInlineEditState>,
    info_focus: Option<BrowseInfoFocus>,
    buttons: &mut ButtonRenderMap,
    hover: Option<super::button_map::TuiButton>,
    theme: super::theme::Theme,
) {
    if area.height < 4 || area.width < 15 {
        return;
    }

    let border_color = theme.amber;
    let w = area.width as usize;

    // Solid title bar (matches convert screen pane style)
    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let title = "▾ info ";
    let title_w = super::display_width::width(&title);
    let fill_count = w.saturating_sub(2 + title_w);

    let top_line = Line::from(vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(title, bar_style),
        Span::styled(" ".repeat(fill_count), bar_style),
        Span::styled("┐", theme.border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let content_height = (area.height as usize).saturating_sub(2);

    let mut lines: Vec<Line> = vec![top_line];

    // Available width for content (inside borders, after the 3-space indent)
    let content_width = w.saturating_sub(2);
    let analyze_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoAnalyze);
    let edit_tags_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoEditTags);
    let audio_streams_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoAudioStreams);
    let info = if let Some(entry) = browse.selected_entry() {
        entry_info_lines(
            entry,
            browse,
            content_width,
            analyze_hovered,
            edit_tags_hovered,
            audio_streams_hovered,
            inline_edit,
            info_focus,
            theme)
    } else {
        InfoContent {
            lines: vec![vec![Span::styled("   (no selection)", theme.muted())]],
            meta_field_rows: Vec::new(),
            analyze_pill_row: None,
            edit_tags_pill_row: None,
            audio_streams_pill_row: None,
            inline_cursor: None,
        }
    };

    // Render content lines with border
    for line_spans in info.lines.iter().take(content_height) {
        lines.push(bordered_line(border_color, w, line_spans.clone(), theme));
    }

    // Fill remaining
    let rendered = info.lines.len().min(content_height);
    for _ in rendered..content_height {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    if let Some((line_idx, col)) = info.inline_cursor {
        if line_idx < content_height {
            f.set_cursor(area.x + 1 + col, area.y + 1 + line_idx as u16);
        }
    }

    // Register clickable metadata fields in the info pane. Only for
    // lines that fit within the visible content area.
    let info_y_start = area.y + 1; // below top border
    for (field, line_idx) in &info.meta_field_rows {
        if *line_idx < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoMeta(*field),
                Rect::new(
                    area.x + 1,
                    info_y_start + *line_idx as u16,
                    (w - 2) as u16,
                    1,
                ),
            );
        }
    }

    // Register pill buttons if they fit.
    if let Some(row) = info.analyze_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAnalyze,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.edit_tags_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoEditTags,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.audio_streams_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAudioStreams,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
}

/// Truncate a string to fit within `max_chars` columns, adding ellipsis if needed.
#[allow(dead_code)]
pub(crate) fn truncate_for_disc_overlay(s: &str, max_chars: usize) -> String { truncate_to(s, max_chars) }

fn truncate_to(s: &str, max_chars: usize) -> String {
    super::display_width::truncate_right(s, max_chars)
}

fn push_key_value_line(
    lines: &mut Vec<Vec<Span<'static>>>,
    key: &'static str,
    value: impl Into<String>,
    key_style: Style,
    value_style: Style,
) {
    lines.push(vec![
        Span::styled(format!("   {key:<8}"), key_style),
        Span::styled(value.into(), value_style),
    ]);
}

fn push_directory_metric_line(
    lines: &mut Vec<Vec<Span<'static>>>,
    key: &'static str,
    value: impl Into<String>,
    theme: super::theme::Theme,
) {
    lines.push(vec![
        Span::styled(format!("   {key:<12}"), theme.muted()),
        Span::styled(value.into(), theme.text_style()),
    ]);
}

fn directory_count_label(count: usize, singular: &str, plural: &str) -> String {
    format!("{} {}", count, if count == 1 { singular } else { plural })
}

fn append_size_label(mut label: String, size: u64) -> String {
    if size > 0 {
        label.push_str(&format!(" ({})", size_str(size)));
    }
    label
}

fn push_directory_stats_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    browse: &BrowseState,
    entry_path: &Path,
    entry_date_label: &str,
    theme: super::theme::Theme,
) -> bool {
    if let Some(stats) = browse.current_dir_stats() {
        push_directory_metric_line(
            lines,
            "folders",
            directory_count_label(stats.folder_count, "folder", "folders"),
            theme,
        );
        push_directory_metric_line(
            lines,
            "files",
            append_size_label(
                directory_count_label(stats.file_count, "file", "files"),
                stats.total_size,
            ),
            theme,
        );
        push_directory_metric_line(
            lines,
            "audio files",
            append_size_label(
                directory_count_label(stats.audio_count, "audio file", "audio files"),
                stats.audio_size,
            ),
            theme,
        );
        if !entry_date_label.is_empty() {
            push_directory_metric_line(lines, "updated", entry_date_label.to_string(), theme);
        }
        true
    } else if browse.dir_stats_pending.contains(entry_path) {
        lines.push(vec![
            Span::styled("   files       ", theme.muted()),
            Span::styled("computing...", Style::default().fg(theme.text_dim)),
        ]);
        true
    } else {
        false
    }
}

fn folder_audio_headline(
    audio: &FolderAudioSummary,
    browse: &BrowseState,
    include_probe_status: bool,
) -> String {
    let rollup = browse.folder_probe_rollup(audio);
    let mut parts = vec![format!(
        "{} {}",
        audio.track_count,
        if audio.track_count == 1 { "track" } else { "tracks" }
    )];

    if let Some(format) = audio.dominant_format_label() {
        if audio.is_mixed_format() {
            parts.push(format!("{format} + mixed formats"));
        } else {
            parts.push(format.to_string());
        }
    }

    if rollup.has_mixed_profiles() {
        parts.push("mixed rates".to_string());
    } else if let Some(profile) = rollup.dominant_profile_label() {
        parts.push(profile.to_string());
    } else if include_probe_status
        && audio.track_count > 0
        && browse.folder_audio_summary_probe_work_in_flight(audio)
    {
        parts.push("probing...".to_string());
    }

    parts.join(" · ")
}

fn push_folder_audio_detail_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    audio: &FolderAudioSummary,
    browse: &BrowseState,
    content_width: usize,
    theme: super::theme::Theme,
) {
    let rollup = browse.folder_probe_rollup(audio);
    if rollup.profile_counts.len() > 1 {
        let detail = rollup
            .profile_counts
            .iter()
            .map(|(profile, count)| format!("{count}x {profile}"))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(vec![
            Span::raw("   "),
            Span::styled(truncate_to(&detail, content_width.saturating_sub(3)), theme.text_style()),
        ]);
    }

    let mut tail = Vec::new();
    if rollup.total_duration_secs > 0.0 {
        tail.push(format!(
            "duration: {}",
            crate::tui::disc_browser::duration_display(rollup.total_duration_secs)
        ));
    }
    if let Some(stats) = browse.current_dir_stats() {
        if stats.total_size > 0 {
            tail.push(format!("size: {}", size_str(stats.total_size)));
        }
    }
    if rollup.probed_count > 0 && rollup.probed_count < audio.track_count {
        tail.push(format!(
            "{} unprobed",
            audio.track_count.saturating_sub(rollup.probed_count)
        ));
    }

    if !tail.is_empty() {
        lines.push(vec![
            Span::raw("   "),
            Span::styled(truncate_to(&tail.join(" · "), content_width.saturating_sub(3)), theme.muted()),
        ]);
    }
}

fn push_disc_probe_summary_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    browse: &BrowseState,
    source_path: &Path,
    entry_size: u64,
    content_width: usize,
    audio_streams_hovered: bool,
    audio_streams_pill_row: &mut Option<usize>,
    theme: super::theme::Theme,
) {
    let max_value_chars = content_width.saturating_sub(3);
    if let Some(contents) = browse
        .disc_probe_cache
        .get(source_path)
        .and_then(|cache| cache.contents_if_current(source_path))
    {
        for summary in crate::tui::disc_browser::disc_content_summary_lines(contents.as_ref()) {
            lines.push(vec![
                Span::raw("   "),
                Span::styled(truncate_to(&summary, max_value_chars), theme.text_style()),
            ]);
        }
        let copy_protection = contents.copy_protection.description.trim();
        if !copy_protection.eq_ignore_ascii_case("none") {
            lines.push(vec![
                Span::styled("   copy protection", theme.muted()),
                Span::raw(" "),
                Span::styled(
                    truncate_to(copy_protection, max_value_chars.saturating_sub(18)),
                    theme.text_style(),
                ),
            ]);
        }
        lines.push(vec![]);
        lines.push(vec![Span::styled("   streams:", theme.muted())]);
        for stream in crate::tui::disc_browser::disc_stream_summary_lines(contents.as_ref(), 6) {
            lines.push(vec![
                Span::raw("     "),
                Span::styled(truncate_to(&stream, max_value_chars.saturating_sub(2)), theme.text_style()),
            ]);
        }
        if contents.presentations.len() > 6 {
            lines.push(vec![Span::styled(
                format!("     ... and {} more", contents.presentations.len() - 6),
                theme.muted(),
            )]);
        }
        if !contents.presentations.is_empty() {
            lines.push(vec![]);
            let row = lines.len();
            let label = " audio streams ";
            let width = super::display_width::width(label);
            let pad = content_width.saturating_sub(width + 3);
            let bg = if audio_streams_hovered { theme.blue } else { theme.purple };
            lines.push(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(
                    label,
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            *audio_streams_pill_row = Some(row);
        }
    } else if let Some(error) = browse
        .disc_probe_cache
        .get(source_path)
        .and_then(|cache| cache.error_if_current(source_path))
    {
        lines.push(vec![
            Span::styled("   status  ", theme.muted()),
            Span::styled(truncate_to(error, max_value_chars.saturating_sub(10)), Style::default().fg(theme.destructive)),
        ]);
        lines.push(vec![Span::styled("   size    ", theme.muted()), Span::styled(size_str(entry_size), theme.text_style())]);
    } else {
        // Only claim analysis is happening when the reducer actually has a
        // worker in flight. In cached-only / after-descend modes the absence of
        // a cache entry is intentional, and rendering "Analyzing disc…" would
        // be a false progress indicator that can sit forever.
        let pending = browse.disc_probe_pending.contains(source_path);
        let status = if pending {
            "Analyzing disc…"
        } else if matches!(
            browse.directory_summary_cold_work_policy,
            BrowseDirectorySummaryColdWorkPolicy::CachedOnly
        ) {
            "disc summary not cached"
        } else if matches!(
            browse.directory_summary_cold_work_policy,
            BrowseDirectorySummaryColdWorkPolicy::AfterDescendOnly
        ) {
            "disc summary available after descend/scan"
        } else {
            "disc summary not cached"
        };
        lines.push(vec![Span::styled("   status  ", theme.muted()), Span::styled(status, theme.muted())]);
        lines.push(vec![Span::styled("   size    ", theme.muted()), Span::styled(size_str(entry_size), theme.text_style())]);
    }
}

fn push_folder_classification_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    classification: &FolderContentClassification,
    browse: &BrowseState,
    entry_path: &Path,
    entry_size: u64,
    entry_date_label: &str,
    content_width: usize,
    audio_streams_hovered: bool,
    audio_streams_pill_row: &mut Option<usize>,
    theme: super::theme::Theme,
) {
    match classification.kind {
        FolderClassificationKind::Album => {
            push_key_value_line(
                lines,
                "kind",
                "album folder",
                theme.muted(),
                theme.bold(theme.blue),
            );
            lines.push(vec![
                Span::raw("   "),
                Span::styled(
                    truncate_to(
                        &folder_audio_headline(&classification.audio, browse, true),
                        content_width.saturating_sub(3),
                    ),
                    theme.text_style(),
                ),
            ]);
            push_folder_audio_detail_lines(lines, &classification.audio, browse, content_width, theme);
        }
        FolderClassificationKind::Disc => {
            let marker = classification
                .disc_marker
                .map(|marker| marker.label())
                .or_else(|| {
                    classification
                        .units
                        .first()
                        .and_then(|unit| unit.disc_marker.map(|marker| marker.label()))
                })
                .unwrap_or("disc");
            push_key_value_line(
                lines,
                "kind",
                format!("{marker} disc folder"),
                theme.muted(),
                theme.bold(theme.purple),
            );
            let source_path = classification.disc_probe_source_path(entry_path);
            push_disc_probe_summary_lines(
                lines,
                browse,
                source_path,
                entry_size,
                content_width,
                audio_streams_hovered,
                audio_streams_pill_row,
                theme,
            );
        }
        FolderClassificationKind::MultiDisc => {
            push_key_value_line(
                lines,
                "kind",
                "multi-disc album",
                theme.muted(),
                theme.bold(theme.blue),
            );
            let disc_word = if classification.unit_count == 1 {
                "disc"
            } else {
                "discs"
            };
            let mut headline = format!("{} {disc_word}", classification.unit_count);
            if classification.audio.track_count > 0 {
                headline.push_str(" · ");
                headline.push_str(&folder_audio_headline(&classification.audio, browse, true));
            }
            lines.push(vec![
                Span::raw("   "),
                Span::styled(
                    truncate_to(&headline, content_width.saturating_sub(3)),
                    theme.text_style(),
                ),
            ]);
            push_folder_audio_detail_lines(lines, &classification.audio, browse, content_width, theme);
            if classification.audio.track_count > 0 && classification.units.len() <= 6 {
                for unit in &classification.units {
                    let unit_line = format!(
                        "{}: {}",
                        unit.name,
                        folder_audio_headline(&unit.audio, browse, false)
                    );
                    lines.push(vec![
                        Span::raw("   "),
                        Span::styled(
                            truncate_to(&unit_line, content_width.saturating_sub(3)),
                            theme.muted(),
                        ),
                    ]);
                }
            }
        }
        FolderClassificationKind::Collection => {
            if !push_directory_stats_lines(lines, browse, entry_path, entry_date_label, theme) {
                // Collections are an internal classification signal used to
                // choose follow-up work. Do not make "collection · many albums"
                // the primary user-facing summary for ordinary artist/tree
                // folders; without stats, fall back to the old neutral
                // directory label until stats are available.
                push_key_value_line(lines, "kind", "directory", theme.muted(), theme.text_style());
            }
        }
        FolderClassificationKind::Unknown => {
            if !push_directory_stats_lines(lines, browse, entry_path, entry_date_label, theme) {
                push_key_value_line(lines, "kind", "directory", theme.muted(), theme.text_style());
            }
        }
    }
}

/// Build content lines for the info pane based on the entry kind.
/// `content_width` is the width available inside the pane borders.
/// Returns `InfoContent` with both the visual lines and a mapping of
/// clickable metadata field positions.
fn entry_info_lines(
    entry: &BrowseEntry,
    browse: &BrowseState,
    content_width: usize,
    analyze_hovered: bool,
    edit_tags_hovered: bool,
    audio_streams_hovered: bool,
    inline_edit: Option<&BrowseInlineEditState>,
    info_focus: Option<BrowseInfoFocus>,
    theme: super::theme::Theme,
) -> InfoContent {
    // Maximum width for free-form text values: subtract the 3-space indent
    let max_value_chars = content_width.saturating_sub(3);

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut meta_field_rows: Vec<(MetadataField, usize)> = Vec::new();
    let mut inline_cursor: Option<(usize, u16)> = None;
    let archive_metadata_inline_disabled = browse.is_in_archive()
        && browse.active_archive_staging().is_none();
    // Pill rows set by branches that emit them after their content.
    // SacdIso is the only branch using this besides AudioFile (which
    // returns early), but the pattern generalises if more arms grow
    // pill rendering.
    let mut sacd_edit_tags_row: Option<usize> = None;
    let mut audio_streams_pill_row: Option<usize> = None;

    let inline_metadata_input = |field: MetadataField| -> Option<&TextInputState> {
        if archive_metadata_inline_disabled {
            return None;
        }
        inline_edit.and_then(|state| match &state.target {
            BrowseInlineEditTarget::Metadata { path, field: active_field }
                if path == &entry.path && *active_field == field => Some(&state.input),
            _ => None,
        })
    };
    let metadata_focused = |field: MetadataField| -> bool {
        !archive_metadata_inline_disabled
            && matches!(info_focus, Some(BrowseInfoFocus::Metadata(active)) if active == field)
    };
    let metadata_label_style = |field: MetadataField| {
        if metadata_focused(field) {
            Style::default()
                .fg(theme.text_bright)
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        }
    };
    let metadata_value_style = |field: MetadataField| {
        if metadata_focused(field) {
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.input_unfocused_bg)
        } else {
            theme.text_style()
        }
    };
    let metadata_placeholder_style = |field: MetadataField| {
        if metadata_focused(field) {
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.input_unfocused_bg)
        } else {
            Style::default().fg(theme.text_dim)
        }
    };

    let entry_date_label = entry.date_label();

    // Blank
    lines.push(vec![]);

    // Name section
    lines.push(vec![Span::styled("   name", theme.muted())]);
    lines.push(vec![
        Span::raw("   "),
        Span::styled(truncate_to(&entry.name, max_value_chars), theme.bright()),
    ]);
    lines.push(vec![]);

    match &entry.kind {
        EntryKind::ParentDir => {
            lines.push(vec![Span::styled("   parent directory", theme.muted())]);
        }
        EntryKind::Directory => {
            if let Some(classification) = browse.current_folder_classification() {
                push_folder_classification_lines(
                    &mut lines,
                    classification.as_ref(),
                    browse,
                    &entry.path,
                    entry.size,
                    &entry_date_label,
                    content_width,
                    audio_streams_hovered,
                    &mut audio_streams_pill_row,
                    theme,
                );
            } else {
                lines.push(vec![
                    Span::styled("   kind    ", theme.muted()),
                    Span::styled("directory", theme.text_style()),
                ]);
                if browse.folder_classification_pending_for(&entry.path) {
                    lines.push(vec![
                        Span::styled("   content ", theme.muted()),
                        Span::styled("classifying...", Style::default().fg(theme.text_dim)),
                    ]);
                }
                // Show directory stats if cached, or "computing..." if a stats
                // task is currently in flight for this directory.
                push_directory_stats_lines(&mut lines, browse, &entry.path, &entry_date_label, theme);
            }
        }
        EntryKind::AudioFile(fmt) => {
            #[allow(unused_assignments)]
            let mut analyze_row = 0usize;
            // Show cached probe info if available
            if let Some(cached) = browse.current_cached_info() {
                let info = &cached.source;
                lines.push(vec![
                    Span::styled("   format  ", theme.muted()),
                    Span::styled(info.format_name.clone(), theme.bold(theme.blue)),
                ]);
                lines.push(vec![
                    Span::styled("   codec   ", theme.muted()),
                    Span::styled(info.codec_display(), theme.text_style()),
                ]);
                if info.sample_rate > 0 {
                    lines.push(vec![
                        Span::styled("   rate    ", theme.muted()),
                        Span::styled(info.sample_rate_display(), theme.text_style()),
                    ]);
                }
                if info.channels > 0 {
                    lines.push(vec![
                        Span::styled("   channels", theme.muted()),
                        Span::raw(" "),
                        Span::styled(info.channels_display(), theme.text_style()),
                    ]);
                }
                if info.duration_secs > 0.0 {
                    lines.push(vec![
                        Span::styled("   duration", theme.muted()),
                        Span::raw(" "),
                        Span::styled(info.duration_display(), theme.text_style()),
                    ]);
                }
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(info.size_display(), theme.text_style()),
                ]);

                // Pre-emphasis — preserve typed confidence from the detector.
                if let Some(ref advisory) = cached.metadata.preemphasis_metadata {
                    let value = browse_preemphasis_status_text(advisory);
                    let style = match advisory.confidence {
                        super::preemphasis::PreemphasisConfidence::Detected => {
                            Style::default().fg(theme.destructive)
                        }
                        super::preemphasis::PreemphasisConfidence::StrongCandidate
                        | super::preemphasis::PreemphasisConfidence::Possible => {
                            Style::default().fg(theme.amber)
                        }
                        super::preemphasis::PreemphasisConfidence::NotDetected
                        | super::preemphasis::PreemphasisConfidence::Indeterminate => {
                            theme.muted()
                        }
                    };
                    lines.push(vec![
                        Span::styled("   pre-emph", theme.muted()),
                        Span::raw(" "),
                        Span::styled(
                            truncate_to(&value, max_value_chars.saturating_sub(11)),
                            style,
                        ),
                    ]);
                }

                // HDCD — shown if previously analyzed and detected.
                // "HDCD" in the value text rendered gold.
                if let Some(ref hdcd) = cached.metadata.hdcd_detail {
                    let val_max = max_value_chars.saturating_sub(11);
                    let mut spans = vec![Span::styled("   HDCD    ", theme.muted())];
                    if let Some(rest) = hdcd.strip_prefix("HDCD") {
                        spans.push(Span::styled(
                            "HDCD",
                            Style::default()
                                .fg(theme.amber)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ));
                        spans.push(Span::styled(
                            truncate_to(rest, val_max.saturating_sub(4)),
                            theme.text_style(),
                        ));
                    } else {
                        spans.push(Span::styled(truncate_to(hdcd, val_max), theme.text_style()));
                    }
                    lines.push(spans);
                }

                // ReplayGain / R128 — shown with technical info since
                // these are measurement data, not user-editable metadata.
                let meta = &cached.metadata;
                let has_rg = meta.rg_track_gain.is_some()
                    || meta.rg_album_gain.is_some()
                    || meta.rg_track_peak.is_some()
                    || meta.rg_album_peak.is_some();
                let has_r128 = meta.r128_track_gain.is_some() || meta.r128_album_gain.is_some();
                if has_rg || has_r128 {
                    lines.push(vec![]);
                    let label = match (has_rg, has_r128) {
                        (true, true) => "replaygain + r128",
                        (true, false) => {
                            match (meta.rg_track_gain.is_some(), meta.rg_album_gain.is_some()) {
                                (true, true) => "replaygain (track + album)",
                                (false, true) => "replaygain (album)",
                                _ => "replaygain (track)",
                            }
                        }
                        (false, true) => "r128",
                        _ => "loudness",
                    };
                    lines.push(vec![Span::styled(format!("   {}", label), theme.muted())]);

                    let rg_inline_max = max_value_chars.saturating_sub(11);
                    if let Some(g) = &meta.rg_track_gain {
                        lines.push(vec![
                            Span::styled("   tk gain ", theme.muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_track_peak {
                        lines.push(vec![
                            Span::styled("   tk peak ", theme.muted()),
                            Span::styled(truncate_to(p, rg_inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(g) = &meta.rg_album_gain {
                        lines.push(vec![
                            Span::styled("   al gain ", theme.muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_album_peak {
                        lines.push(vec![
                            Span::styled("   al peak ", theme.muted()),
                            Span::styled(truncate_to(p, rg_inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_track_gain {
                        lines.push(vec![
                            Span::styled("   r128 tk ", theme.muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_album_gain {
                        lines.push(vec![
                            Span::styled("   r128 al ", theme.muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme.text_style()),
                        ]);
                    }
                }

                // Analyze pill — after technical info + RG, before metadata.
                lines.push(vec![]);
                analyze_row = lines.len();
                let analyze_label = " analyze ";
                let analyze_w = super::display_width::width(analyze_label);
                let analyze_pad = content_width.saturating_sub(analyze_w + 3);
                let analyze_bg = if analyze_hovered {
                    theme.blue
                } else {
                    theme.purple
                };
                lines.push(vec![
                    Span::raw(" ".repeat(analyze_pad)),
                    Span::styled(
                        analyze_label,
                        Style::default()
                            .fg(theme.pill_active_fg)
                            .bg(analyze_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);

                // Metadata tags — always show the section (with placeholders
                // for absent fields) so users can click to add new tags.
                {
                    let inline_max = max_value_chars.saturating_sub(11);
                    lines.push(vec![]);

                    // Title: inline label + value (same layout as artist/album/genre/year).
                    let title_row = lines.len();
                    if let Some(input) = inline_metadata_input(MetadataField::Title) {
                        let mut row = vec![
                            Span::styled("   title   ", metadata_label_style(MetadataField::Title)),
                        ];
                        row.extend(render_inline_value_with_embedded_cursor(input, inline_max, theme));
                        lines.push(row);
                        inline_cursor = Some((title_row, 11 + inline_cursor_col(input, inline_max)));
                    } else if let Some(title) = &meta.title {
                        lines.push(vec![
                            Span::styled("   title   ", metadata_label_style(MetadataField::Title)),
                            Span::styled(truncate_to(title, inline_max), metadata_value_style(MetadataField::Title)),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   title   ", metadata_label_style(MetadataField::Title)),
                            Span::styled(
                                if archive_metadata_inline_disabled {
                                    "(use edit tags)"
                                } else if metadata_focused(MetadataField::Title) {
                                    "(type to add)"
                                } else {
                                    "(click to add)"
                                },
                                metadata_placeholder_style(MetadataField::Title),
                            ),
                        ]);
                    }
                    if !archive_metadata_inline_disabled {
                        meta_field_rows.push((MetadataField::Title, title_row));
                    }

                    // Artist: inline label + value. Clickable on the whole line.
                    let artist_row = lines.len();
                    if let Some(input) = inline_metadata_input(MetadataField::Artist) {
                        let mut row = vec![
                            Span::styled("   artist  ", metadata_label_style(MetadataField::Artist)),
                        ];
                        row.extend(render_inline_value_with_embedded_cursor(input, inline_max, theme));
                        lines.push(row);
                        inline_cursor = Some((artist_row, 11 + inline_cursor_col(input, inline_max)));
                    } else if let Some(artist) = &meta.artist {
                        lines.push(vec![
                            Span::styled("   artist  ", metadata_label_style(MetadataField::Artist)),
                            Span::styled(truncate_to(artist, inline_max), metadata_value_style(MetadataField::Artist)),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   artist  ", metadata_label_style(MetadataField::Artist)),
                            Span::styled(
                                if archive_metadata_inline_disabled {
                                    "(use edit tags)"
                                } else if metadata_focused(MetadataField::Artist) {
                                    "(type to add)"
                                } else {
                                    "(click to add)"
                                },
                                metadata_placeholder_style(MetadataField::Artist),
                            ),
                        ]);
                    }
                    if !archive_metadata_inline_disabled {
                        meta_field_rows.push((MetadataField::Artist, artist_row));
                    }

                    // Album
                    let album_row = lines.len();
                    if let Some(input) = inline_metadata_input(MetadataField::Album) {
                        let mut row = vec![
                            Span::styled("   album   ", metadata_label_style(MetadataField::Album)),
                        ];
                        row.extend(render_inline_value_with_embedded_cursor(input, inline_max, theme));
                        lines.push(row);
                        inline_cursor = Some((album_row, 11 + inline_cursor_col(input, inline_max)));
                    } else if let Some(album) = &meta.album {
                        lines.push(vec![
                            Span::styled("   album   ", metadata_label_style(MetadataField::Album)),
                            Span::styled(truncate_to(album, inline_max), metadata_value_style(MetadataField::Album)),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   album   ", metadata_label_style(MetadataField::Album)),
                            Span::styled(
                                if archive_metadata_inline_disabled {
                                    "(use edit tags)"
                                } else if metadata_focused(MetadataField::Album) {
                                    "(type to add)"
                                } else {
                                    "(click to add)"
                                },
                                metadata_placeholder_style(MetadataField::Album),
                            ),
                        ]);
                    }
                    if !archive_metadata_inline_disabled {
                        meta_field_rows.push((MetadataField::Album, album_row));
                    }

                    // Genre
                    let genre_row = lines.len();
                    if let Some(input) = inline_metadata_input(MetadataField::Genre) {
                        let mut row = vec![
                            Span::styled("   genre   ", metadata_label_style(MetadataField::Genre)),
                        ];
                        row.extend(render_inline_value_with_embedded_cursor(input, inline_max, theme));
                        lines.push(row);
                        inline_cursor = Some((genre_row, 11 + inline_cursor_col(input, inline_max)));
                    } else if let Some(genre) = &meta.genre {
                        lines.push(vec![
                            Span::styled("   genre   ", metadata_label_style(MetadataField::Genre)),
                            Span::styled(truncate_to(genre, inline_max), metadata_value_style(MetadataField::Genre)),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   genre   ", metadata_label_style(MetadataField::Genre)),
                            Span::styled(
                                if archive_metadata_inline_disabled {
                                    "(use edit tags)"
                                } else if metadata_focused(MetadataField::Genre) {
                                    "(type to add)"
                                } else {
                                    "(click to add)"
                                },
                                metadata_placeholder_style(MetadataField::Genre),
                            ),
                        ]);
                    }
                    if !archive_metadata_inline_disabled {
                        meta_field_rows.push((MetadataField::Genre, genre_row));
                    }

                    // Year
                    let year_row = lines.len();
                    if let Some(input) = inline_metadata_input(MetadataField::Year) {
                        let mut row = vec![
                            Span::styled("   year    ", metadata_label_style(MetadataField::Year)),
                        ];
                        row.extend(render_inline_value_with_embedded_cursor(input, inline_max, theme));
                        lines.push(row);
                        inline_cursor = Some((year_row, 11 + inline_cursor_col(input, inline_max)));
                    } else if let Some(year) = &meta.year {
                        lines.push(vec![
                            Span::styled("   year    ", metadata_label_style(MetadataField::Year)),
                            Span::styled(truncate_to(year, inline_max), metadata_value_style(MetadataField::Year)),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   year    ", metadata_label_style(MetadataField::Year)),
                            Span::styled(
                                if archive_metadata_inline_disabled {
                                    "(use edit tags)"
                                } else if metadata_focused(MetadataField::Year) {
                                    "(type to add)"
                                } else {
                                    "(click to add)"
                                },
                                metadata_placeholder_style(MetadataField::Year),
                            ),
                        ]);
                    }
                    if !archive_metadata_inline_disabled {
                        meta_field_rows.push((MetadataField::Year, year_row));
                    }
                }
            } else {
                // Not yet probed or probe failed — show basic info
                lines.push(vec![
                    Span::styled("   format  ", theme.muted()),
                    Span::styled(fmt.name().to_string(), theme.bold(theme.blue)),
                ]);
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(size_str(entry.size), theme.text_style()),
                ]);
                if let Some(archive_entry) = browse.archive_entry_for_path(&entry.path) {
                    if archive_entry.packed_size > 0 {
                        lines.push(vec![
                            Span::styled("   packed  ", theme.muted()),
                            Span::styled(size_str(archive_entry.packed_size), theme.text_style()),
                        ]);
                    }
                    lines.push(vec![
                        Span::styled("   archive ", theme.muted()),
                        Span::styled(
                            if archive_entry.encrypted { "encrypted" } else { "entry" },
                            theme.text_style(),
                        ),
                    ]);
                }

                // Analyze pill after basic info.
                lines.push(vec![]);
                let analyze_row_unprobed = lines.len();
                let a_label = " analyze ";
                let a_w = super::display_width::width(a_label);
                let a_pad = content_width.saturating_sub(a_w + 3);
                let a_bg = if analyze_hovered {
                    theme.blue
                } else {
                    theme.purple
                };
                lines.push(vec![
                    Span::raw(" ".repeat(a_pad)),
                    Span::styled(
                        a_label,
                        Style::default()
                            .fg(theme.pill_active_fg)
                            .bg(a_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                // Return early — no metadata to show.
                lines.push(vec![]);
                let et_row = lines.len();
                let et_label = " edit tags ";
                let et_w2 = super::display_width::width(et_label);
                let et_pad2 = content_width.saturating_sub(et_w2 + 3);
                let et_bg2 = if edit_tags_hovered {
                    theme.blue
                } else {
                    theme.purple
                };
                lines.push(vec![
                    Span::raw(" ".repeat(et_pad2)),
                    Span::styled(
                        et_label,
                        Style::default()
                            .fg(theme.pill_active_fg)
                            .bg(et_bg2)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                return InfoContent {
                    lines,
                    meta_field_rows,
                    analyze_pill_row: Some(analyze_row_unprobed),
                    edit_tags_pill_row: Some(et_row),
                    audio_streams_pill_row,
                    inline_cursor,
                };
            }

            // Edit tags pill — after metadata/RG section.
            lines.push(vec![]);
            let edit_tags_row = lines.len();
            let et_label = " edit tags ";
            let et_w = super::display_width::width(et_label);
            let et_pad = content_width.saturating_sub(et_w + 3);
            let et_bg = if edit_tags_hovered {
                theme.blue
            } else {
                theme.purple
            };
            lines.push(vec![
                Span::raw(" ".repeat(et_pad)),
                Span::styled(
                    et_label,
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(et_bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            return InfoContent {
                lines,
                meta_field_rows,
                analyze_pill_row: Some(analyze_row),
                edit_tags_pill_row: Some(edit_tags_row),
                audio_streams_pill_row,
                inline_cursor,
            };
        }
        EntryKind::DvdAudioIso
        | EntryKind::DvdAudioDir
        | EntryKind::DvdVideoIso
        | EntryKind::DvdVideoDir
        | EntryKind::BlurayIso
        | EntryKind::BlurayDir => {
            let disc_kind = match &entry.kind {
                EntryKind::DvdAudioIso => "DVD-Audio ISO",
                EntryKind::DvdAudioDir => "DVD-Audio directory",
                EntryKind::DvdVideoIso => "DVD-Video ISO",
                EntryKind::DvdVideoDir => "DVD-Video directory",
                EntryKind::BlurayIso => "Blu-ray ISO",
                EntryKind::BlurayDir => "Blu-ray directory",
                _ => unreachable!("disc info arm only handles disc entries"),
            };
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled(disc_kind, theme.bold(theme.purple)),
            ]);
            push_disc_probe_summary_lines(
                &mut lines,
                browse,
                &entry.path,
                entry.size,
                content_width,
                audio_streams_hovered,
                &mut audio_streams_pill_row,
                theme,
            );
        }
        EntryKind::Archive => {
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled("archive (7z)", theme.text_style()),
            ]);
            lines.push(vec![
                Span::styled("   size    ", theme.muted()),
                Span::styled(size_str(entry.size), theme.text_style()),
            ]);
        }
        EntryKind::SacdIso => {
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled("SACD ISO (ScarletBook)", theme.text_style()),
            ]);

            // SACD ISOs are native disc sources and must use the same compact
            // content/stream summary renderer as Blu-ray, DVD-Audio, and
            // DVD-Video sources. Keep the SACD-specific metadata/edit-tags UI
            // below, but do not bypass the shared disc probe cache path.
            push_disc_probe_summary_lines(
                &mut lines,
                browse,
                &entry.path,
                entry.size,
                content_width,
                audio_streams_hovered,
                &mut audio_streams_pill_row,
                theme,
            );

            if let Some(cached) = browse.current_cached_info() {
                let info = &cached.source;
                lines.push(vec![]);
                lines.push(vec![
                    Span::styled("   format  ", theme.muted()),
                    Span::styled(info.format_name.clone(), theme.bold(theme.purple)),
                ]);
                lines.push(vec![
                    Span::styled("   codec   ", theme.muted()),
                    Span::styled(info.codec_display(), theme.text_style()),
                ]);
                if info.sample_rate > 0 {
                    lines.push(vec![
                        Span::styled("   rate    ", theme.muted()),
                        Span::styled(info.sample_rate_display(), theme.text_style()),
                    ]);
                }
                if info.channels > 0 {
                    lines.push(vec![
                        Span::styled("   channels", theme.muted()),
                        Span::raw(" "),
                        Span::styled(info.channels_display(), theme.text_style()),
                    ]);
                }
                if info.duration_secs > 0.0 {
                    lines.push(vec![
                        Span::styled("   duration", theme.muted()),
                        Span::raw(" "),
                        Span::styled(info.duration_display(), theme.text_style()),
                    ]);
                }
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(info.size_display(), theme.text_style()),
                ]);

                // Album-level metadata block (from sidecar overlay when
                // present, ScarletBook fallback otherwise).
                let meta = &cached.metadata;
                let inline_max = max_value_chars.saturating_sub(11);
                let has_any = meta.album.is_some()
                    || meta.artist.is_some()
                    || meta.genre.is_some()
                    || meta.year.is_some()
                    || meta.catalog_number.is_some();
                if has_any {
                    lines.push(vec![]);
                    if let Some(s) = &meta.artist {
                        lines.push(vec![
                            Span::styled("   artist  ", metadata_label_style(MetadataField::Artist)),
                            Span::styled(truncate_to(s, inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(s) = &meta.album {
                        lines.push(vec![
                            Span::styled("   album   ", metadata_label_style(MetadataField::Album)),
                            Span::styled(truncate_to(s, inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(s) = &meta.genre {
                        lines.push(vec![
                            Span::styled("   genre   ", metadata_label_style(MetadataField::Genre)),
                            Span::styled(truncate_to(s, inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(s) = &meta.year {
                        lines.push(vec![
                            Span::styled("   year    ", metadata_label_style(MetadataField::Year)),
                            Span::styled(truncate_to(s, inline_max), theme.text_style()),
                        ]);
                    }
                    if let Some(s) = &meta.catalog_number {
                        lines.push(vec![
                            Span::styled("   catalog ", theme.muted()),
                            Span::styled(truncate_to(s, inline_max), theme.text_style()),
                        ]);
                    }
                }
            }

            // Edit-tags pill — parity with the AudioFile arm so SACD ISOs have
            // a clickable mouse path to the metadata editor. This remains
            // available even while the async disc/source probe is pending.
            lines.push(vec![]);
            let edit_tags_row = lines.len();
            let et_label = " edit tags ";
            let et_w = super::display_width::width(et_label);
            let et_pad = content_width.saturating_sub(et_w + 3);
            let et_bg = if edit_tags_hovered {
                theme.blue
            } else {
                theme.purple
            };
            lines.push(vec![
                Span::raw(" ".repeat(et_pad)),
                Span::styled(
                    et_label,
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(et_bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            sacd_edit_tags_row = Some(edit_tags_row);
        }
        EntryKind::OtherFile => {
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled("file", theme.text_style()),
            ]);
            lines.push(vec![
                Span::styled("   size    ", theme.muted()),
                Span::styled(size_str(entry.size), theme.text_style()),
            ]);
        }
    }

    // Symlink indicator (applies to all entry kinds).
    if entry.is_symlink {
        lines.push(vec![]);
        let (label, color) = if entry.is_broken_symlink {
            ("symlink (broken)", theme.destructive)
        } else {
            ("symlink", theme.amber)
        };
        lines.push(vec![
            Span::styled("   ", theme.muted()),
            Span::styled(label, Style::default().fg(color)),
        ]);
    }

    InfoContent {
        lines,
        meta_field_rows,
        analyze_pill_row: None,
        edit_tags_pill_row: sacd_edit_tags_row,
        audio_streams_pill_row,
        inline_cursor,
    }
}

/// Create a bordered line
fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
    theme: super::theme::Theme,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = Vec::with_capacity(content.len() + 3);
    spans.push(Span::styled("│", theme.border(border_color)));
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

fn empty_bordered_line(
    border_color: ratatui::style::Color,
    width: usize,
    theme: super::theme::Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled("│", theme.border(border_color)),
        Span::raw(" ".repeat(width.saturating_sub(2))),
        Span::styled("│", theme.border(border_color)),
    ])
}

#[cfg(test)]
mod browse_list_render_allocation_tests {
    use super::*;
    use crate::convert::formats::AudioFormat;
    use crate::tui::probe::{SourceInfo, SourceMetadata};
    use std::path::PathBuf;

    fn audio_entry() -> BrowseEntry {
        BrowseEntry::new(
            PathBuf::from("/tmp/track.flac"),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            1024,
            None,
        )
    }

    #[test]
    fn possible_catalog_advisory_is_never_rendered_as_detected() {
        let advisory = crate::tui::preemphasis::PreemphasisAdvisory {
            evidence: crate::tui::preemphasis::PreemphasisAdvisoryEvidence::Catalog,
            confidence: crate::tui::preemphasis::PreemphasisConfidence::Possible,
            catalog: Some(crate::tui::preemphasis::CatalogAdvisory {
                catalog_number: "35DP-150".to_string(),
                quality: crate::tui::preemphasis::catalog::CatalogMatchQuality::Exact,
                source: crate::tui::preemphasis::catalog::CatalogMatchSource::Folder,
                source_row: 1,
                source_catalog_cell: "35DP-150".to_string(),
            }),
            detail: "folder exact match".to_string(),
        };
        let rendered = browse_preemphasis_status_text(&advisory);
        assert_eq!(rendered, "possible (catalog 35DP-150)");
        assert!(!rendered.contains("detected"));
    }

    fn cached_info() -> CachedInfo {
        CachedInfo {
            source: SourceInfo {
                format_name: "FLAC".to_string(),
                codec: "PCM".to_string(),
                bit_depth: Some(24),
                sample_format_is_float: None,
                sample_rate: 96_000,
                channels: 2,
                channel_layout: "stereo".to_string(),
                duration_secs: 123.0,
                file_size: 1024,
            },
            metadata: SourceMetadata {
                artist: Some("Artist".to_string()),
                album: Some("Album".to_string()),
                ..SourceMetadata::default()
            },
        }
    }

    #[test]
    fn audio_columns_preserve_display_values_without_intermediate_clones() {
        let entry = audio_entry();
        let cached = cached_info();

        assert!(matches!(
            entry_column_text(&entry, BrowseColumn::Format, Some(&cached)),
            Cow::Borrowed("FLAC")
        ));
        assert!(matches!(
            entry_column_text(&entry, BrowseColumn::Artist, Some(&cached)),
            Cow::Borrowed("Artist")
        ));
        assert!(matches!(
            entry_column_text(&entry, BrowseColumn::Album, Some(&cached)),
            Cow::Borrowed("Album")
        ));
        assert_eq!(
            pad_or_truncate(
                entry_column_text(&entry, BrowseColumn::Artist, Some(&cached)).as_ref(),
                8,
                false,
            ),
            "Artist  "
        );
    }

    #[test]
    fn entry_rows_keep_right_border_at_exact_display_width() {
        let browse = BrowseState::new();
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let width = 40usize;
        let columns = [BrowseColumnCell {
            column: BrowseColumn::Name,
            width: width - 8,
        }];

        for name in [
            "Japan Epic 25 ・ 8P-5137",
            "日本語アルバム",
            "e\u{301}lan vital",
        ] {
            let entry = BrowseEntry::new(
                PathBuf::from(format!("/tmp/{name}")),
                name.to_string(),
                EntryKind::OtherFile,
                0,
                None,
            );
            let line = render_entry_line(
                theme.border_dim,
                width,
                &columns,
                &browse,
                &entry,
                None,
                false,
                false,
                false,
                false,
                theme,
            );
            assert_eq!(line.width(), width, "{name}");
            assert_eq!(
                line.spans.last().map(|span| span.content.as_ref()),
                Some("│"),
                "{name}"
            );
        }
    }

    #[test]
    fn selected_row_restyle_preserves_inline_editor_field_and_selection_styles() {
        let browse = BrowseState::new();
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let width = 40usize;
        let columns = [BrowseColumnCell {
            column: BrowseColumn::Name,
            width: width - 8,
        }];
        let entry = audio_entry();
        let input = TextInputState::new_selected("track.flac".to_string());

        let line = render_entry_line(
            theme.border_dim,
            width,
            &columns,
            &browse,
            &entry,
            Some(&input),
            true,
            false,
            false,
            false,
            theme,
        );

        assert!(line.spans.iter().any(|span| {
            span.style.bg == Some(theme.input_focused_bg)
        }), "inline editor field background must survive selected-row styling");
        assert!(line.spans.iter().any(|span| {
            span.style.bg == Some(theme.text_bright)
                && span.style.fg == Some(theme.bg)
        }), "inline editor selection must retain inverse-video styling");
    }

    #[test]
    fn placeholder_and_padding_output_remain_unchanged() {
        let other = BrowseEntry::new(
            PathBuf::from("/tmp/readme.txt"),
            "readme.txt".to_string(),
            EntryKind::OtherFile,
            12,
            None,
        );

        assert!(matches!(
            entry_column_text(&other, BrowseColumn::Artist, None),
            Cow::Borrowed("—")
        ));
        assert_eq!(
            pad_or_truncate(
                entry_column_text(&other, BrowseColumn::Artist, None).as_ref(),
                4,
                false,
            ),
            "—   "
        );
        assert_eq!(
            pad_or_truncate(entry_column_text(&other, BrowseColumn::Size, None).as_ref(), 6, true),
            "  12 B"
        );
    }
}

#[cfg(test)]
mod folder_classification_info_pane_tests {
    use super::*;
    use crate::disc::{DiscContents, SacdAreaId};
    use crate::disc::model::{
        AudioPresentationFormat, CopyProtectionSummary, DiscFormat, DiscPresentation,
        DiscTrack, FormatProvenance, PresentationId,
    };
    use crate::tui::browse::{
        FolderDiscMarkerKind, FolderUnitSummary, ProbeCacheIdentity,
    };
    use crate::tui::disc_browser::{disc_probe_fingerprint, DiscProbeCacheEntry};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn theme() -> crate::tui::theme::Theme {
        crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug())
    }

    fn flatten(info: &InfoContent) -> String {
        info.lines
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn audio_summary(paths: &[PathBuf], format: &str) -> FolderAudioSummary {
        let mut format_counts = BTreeMap::new();
        format_counts.insert(format.to_string(), paths.len());
        FolderAudioSummary {
            track_count: paths.len(),
            format_counts,
            file_paths: paths.to_vec(),
        }
    }

    fn directory_entry(path: &Path, name: &str, identity: ProbeCacheIdentity) -> BrowseEntry {
        BrowseEntry::new(
            path.to_path_buf(),
            name.to_string(),
            EntryKind::Directory,
            identity.size,
            identity.modified,
        )
    }

    fn render_classification(entry: BrowseEntry, classification: FolderContentClassification) -> (String, InfoContent) {
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity, classification);
        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        (flatten(&info), info)
    }

    #[test]
    fn album_classification_renders_track_format_and_probe_state() {
        let entry = BrowseEntry::new(
            PathBuf::from("/music/album"),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let audio = audio_summary(
            &[
                PathBuf::from("/music/album/01.flac"),
                PathBuf::from("/music/album/02.flac"),
            ],
            "FLAC",
        );
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Album,
            identity,
            audio,
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let (rendered, info) = render_classification(entry, classification);

        assert!(rendered.contains("album folder"));
        assert!(rendered.contains("2 tracks · FLAC"));
        assert!(!rendered.contains("probing..."));
        assert!(info.audio_streams_pill_row.is_none());
    }

    #[test]
    fn multidisc_classification_renders_disc_count_total_tracks_and_unit_rows() {
        let entry = BrowseEntry::new(
            PathBuf::from("/music/album"),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let disc1_audio = audio_summary(&[PathBuf::from("/music/album/Disc 1/01.flac")], "FLAC");
        let disc2_audio = audio_summary(&[PathBuf::from("/music/album/Disc 2/01.flac")], "FLAC");
        let mut total_formats = BTreeMap::new();
        total_formats.insert("FLAC".to_string(), 2);
        let total_audio = FolderAudioSummary {
            track_count: 2,
            format_counts: total_formats,
            file_paths: vec![
                PathBuf::from("/music/album/Disc 1/01.flac"),
                PathBuf::from("/music/album/Disc 2/01.flac"),
            ],
        };
        let units = vec![
            FolderUnitSummary {
                path: PathBuf::from("/music/album/Disc 1"),
                parent: PathBuf::from("/music/album"),
                name: "Disc 1".to_string(),
                disc_marker: None,
                audio: disc1_audio,
            },
            FolderUnitSummary {
                path: PathBuf::from("/music/album/Disc 2"),
                parent: PathBuf::from("/music/album"),
                name: "Disc 2".to_string(),
                disc_marker: None,
                audio: disc2_audio,
            },
        ];
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::MultiDisc,
            identity,
            audio: total_audio,
            units,
            unit_count: 2,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let (rendered, _) = render_classification(entry, classification);

        assert!(rendered.contains("multi-disc album"));
        assert!(rendered.contains("2 discs · 2 tracks · FLAC"));
        assert!(!rendered.contains("probing..."));
        assert!(rendered.contains("Disc 1: 1 track · FLAC"));
        assert!(rendered.contains("Disc 2: 1 track · FLAC"));
    }

    #[test]
    fn collection_classification_without_stats_renders_neutral_directory_label() {
        let entry = BrowseEntry::new(
            PathBuf::from("/music/artist"),
            "artist".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Collection,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 24,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let (rendered, _) = render_classification(entry, classification);

        assert!(rendered.contains("kind    directory"));
        assert!(!rendered.contains("collection · 24 albums"));
        assert!(!rendered.contains("probing..."));
        assert!(!rendered.contains("streams:"));
    }

    #[test]
    fn collection_classification_uses_old_style_directory_stats_as_primary_summary_when_available() {
        let entry = BrowseEntry::new(
            PathBuf::from("/music/artist"),
            "artist".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Collection,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 24,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity, classification);
        browse.insert_dir_stats_for_identity(
            entry.path.clone(),
            identity,
            crate::tui::browse::DirStats {
                folder_count: 24,
                file_count: 312,
                audio_count: 247,
                audio_size: 12_400_000_000,
                total_size: 14_100_000_000,
            },
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("folders     24 folders"));
        assert!(rendered.contains("files       312 files (13.1 GB)"));
        assert!(rendered.contains("audio files 247 audio files (11.5 GB)"));
        assert!(!rendered.contains("collection · 24 albums"));
        assert!(!rendered.contains("kind    collection"));
    }

    #[test]
    fn directory_only_collection_renders_old_style_stats_not_collection_label() {
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(86_400 * 10);
        let entry = BrowseEntry::new(
            PathBuf::from("/music/artist"),
            "artist".to_string(),
            EntryKind::Directory,
            0,
            Some(modified),
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Collection,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 0,
            collection_many: true,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity, classification);
        browse.insert_dir_stats_for_identity(
            entry.path.clone(),
            identity,
            crate::tui::browse::DirStats {
                folder_count: 18,
                file_count: 247,
                audio_count: 192,
                audio_size: 42_000_000_000,
                total_size: 45_000_000_000,
            },
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("folders     18 folders"));
        assert!(rendered.contains("files       247 files (41.9 GB)"));
        assert!(rendered.contains("audio files 192 audio files (39.1 GB)"));
        assert!(rendered.contains("updated"));
        assert!(!rendered.contains("collection · many albums"));
        assert!(!rendered.contains("kind    collection"));
    }


    fn bluray_layout(root: &Path) {
        let bdmv = root.join("BDMV");
        std::fs::create_dir_all(bdmv.join("PLAYLIST")).expect("playlist dir");
        std::fs::create_dir_all(bdmv.join("STREAM")).expect("stream dir");
        std::fs::write(bdmv.join("index.bdmv"), b"index").expect("index marker");
        std::fs::write(bdmv.join("MovieObject.bdmv"), b"movie object").expect("movie marker");
        std::fs::write(bdmv.join("PLAYLIST").join("00000.mpls"), b"playlist").expect("playlist");
        std::fs::write(bdmv.join("STREAM").join("00000.m2ts"), b"stream").expect("stream");
    }

    fn disc_presentation(index: u32, codec: &str, channels: u32, layout: &str) -> DiscPresentation {
        DiscPresentation {
            id: PresentationId::try_blu_ray_title(index, 0x1100 + index as u16, 0, 1)
                .expect("valid Blu-ray id"),
            label: format!("stream {index}"),
            format: AudioPresentationFormat {
                codec: Some(codec.to_string()),
                sample_rate: Some(96_000),
                bit_depth: Some(24),
                channels: Some(channels as u8),
                channel_layout: Some(layout.to_string()),
                lossless: true,
                provenance: FormatProvenance::IfoAttributes,
            },
            tracks: vec![DiscTrack {
                number: 1,
                title: None,
                performer: None,
                duration_secs: Some(60.0),
                format_note: None,
            }],
            total_duration_secs: 60.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    fn disc_contents(source_path: PathBuf) -> DiscContents {
        DiscContents {
            format: DiscFormat::BluRay,
            label: "BD".to_string(),
            source_path,
            presentations: vec![
                disc_presentation(1, "AC3", 6, "5.1"),
                disc_presentation(2, "DTS-HD MA", 6, "5.1"),
                disc_presentation(3, "TrueHD", 6, "5.1"),
                disc_presentation(4, "LPCM", 2, "stereo"),
                disc_presentation(5, "LPCM", 6, "5.1"),
                disc_presentation(6, "LPCM", 2, "stereo"),
                disc_presentation(7, "AC3", 2, "stereo"),
            ],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: "none".to_string() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    #[test]
    fn classified_disc_folder_renders_existing_disc_summary_streams_and_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("FRAGILE");
        std::fs::create_dir(&root).expect("disc root");
        bluray_layout(&root);
        let metadata = std::fs::metadata(&root).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let entry = directory_entry(&root, "FRAGILE", identity);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(FolderDiscMarkerKind::BluRay),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        let fingerprint = disc_probe_fingerprint(&root).expect("disc fingerprint");
        browse.disc_probe_cache.insert(
            root.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, disc_contents(root.clone())),
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Blu-ray disc folder"));
        assert!(rendered.contains("content: 7 audio streams · 7 tracks"));
        assert!(rendered.contains("streams:"));
        assert!(rendered.contains("LPCM 24-bit/96kHz stereo"));
        assert!(rendered.contains("... and 1 more"));
        assert!(!rendered.contains("copy protection"));
        assert!(info.audio_streams_pill_row.is_some());
    }

    #[test]
    fn classified_iso_folders_show_audio_streams_for_one_presentation_across_disc_markers() {
        let marker_cases = [
            (FolderDiscMarkerKind::DvdAudio, "dvda.iso"),
            (FolderDiscMarkerKind::DvdVideo, "dvdv.iso"),
            (FolderDiscMarkerKind::Sacd, "sacd.iso"),
            (FolderDiscMarkerKind::BluRay, "bluray.iso"),
        ];

        for (marker, file_name) in marker_cases {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().join("album");
            std::fs::create_dir(&root).expect("disc folder");
            let nested_iso = root.join(file_name);
            std::fs::write(&nested_iso, b"disc image fixture").expect("disc image");
            let metadata = std::fs::metadata(&root).expect("folder metadata");
            let identity = ProbeCacheIdentity::from_metadata(&metadata);
            let entry = directory_entry(&root, "album", identity);
            let classification = FolderContentClassification {
                kind: FolderClassificationKind::Disc,
                identity,
                audio: FolderAudioSummary::default(),
                units: vec![FolderUnitSummary {
                    path: nested_iso.clone(),
                    parent: root.clone(),
                    name: file_name.to_string(),
                    disc_marker: Some(marker),
                    audio: FolderAudioSummary::default(),
                }],
                unit_count: 1,
                collection_many: false,
                io_budget_exhausted: false,
                disc_marker: Some(marker),
                embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
                cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            };
            let mut contents = disc_contents(nested_iso.clone());
            contents.presentations.truncate(1);
            let presentation = contents
                .presentations
                .first_mut()
                .expect("single presentation fixture");
            match marker {
                FolderDiscMarkerKind::DvdAudio => {
                    contents.format = DiscFormat::DvdAudio;
                    presentation.id = PresentationId::DvdAudioGroup(1);
                    presentation.format.codec = Some("MLP".to_string());
                }
                FolderDiscMarkerKind::DvdVideo => {
                    contents.format = DiscFormat::DvdVideo;
                    presentation.id = PresentationId::dvd_video(1, 1, 0);
                    presentation.format.codec = Some("LPCM".to_string());
                }
                FolderDiscMarkerKind::Sacd => {
                    contents.format = DiscFormat::Sacd;
                    presentation.id = PresentationId::SacdArea(SacdAreaId::Stereo);
                    presentation.format.codec = Some("DSD".to_string());
                    presentation.format.sample_rate = Some(2_822_400);
                    presentation.format.bit_depth = Some(1);
                }
                FolderDiscMarkerKind::BluRay | FolderDiscMarkerKind::Iso => {}
            }

            let mut browse = BrowseState::new();
            browse.entries = vec![entry.clone()];
            browse.selected_index = 0;
            browse.insert_folder_classification_for_identity(
                root.clone(),
                identity,
                classification,
            );
            let fingerprint = disc_probe_fingerprint(&nested_iso).expect("disc fingerprint");
            browse.disc_probe_cache.insert(
                nested_iso.clone(),
                DiscProbeCacheEntry::from_success(fingerprint, contents),
            );

            let info = entry_info_lines(
                &entry,
                &browse,
                96,
                false,
                false,
                false,
                None,
                None,
                theme(),
            );
            let rendered = flatten(&info);

            assert!(
                info.audio_streams_pill_row.is_some(),
                "{marker:?} folder with one presentation must expose audio streams",
            );
            assert!(rendered.contains("content: 1 audio stream"));
        }
    }

    #[test]
    fn disc_folder_renders_copy_protection_only_when_not_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("PROTECTED");
        std::fs::create_dir(&root).expect("disc root");
        bluray_layout(&root);
        let metadata = std::fs::metadata(&root).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let entry = directory_entry(&root, "PROTECTED", identity);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(FolderDiscMarkerKind::BluRay),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };
        let mut contents = disc_contents(root.clone());
        contents.copy_protection.description = "  AACS  ".to_string();

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        let fingerprint = disc_probe_fingerprint(&root).expect("disc fingerprint");
        browse.disc_probe_cache.insert(
            root.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, contents),
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("copy protection AACS"));
    }

    #[test]
    fn disc_folder_suppresses_whitespace_padded_none_copy_protection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("UNPROTECTED");
        std::fs::create_dir(&root).expect("disc root");
        bluray_layout(&root);
        let metadata = std::fs::metadata(&root).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let entry = directory_entry(&root, "UNPROTECTED", identity);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(FolderDiscMarkerKind::BluRay),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };
        let mut contents = disc_contents(root.clone());
        contents.copy_protection.description = "  none  ".to_string();

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        let fingerprint = disc_probe_fingerprint(&root).expect("disc fingerprint");
        browse.disc_probe_cache.insert(
            root.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, contents),
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(!rendered.contains("copy protection"));
    }

    #[test]
    fn cached_only_classified_disc_without_cache_does_not_claim_analyzing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Cached Only Disc");
        std::fs::create_dir(&root).expect("disc root");
        bluray_layout(&root);
        let metadata = std::fs::metadata(&root).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let entry = directory_entry(&root, "Cached Only Disc", identity);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(FolderDiscMarkerKind::BluRay),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        browse.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Blu-ray disc folder"));
        assert!(rendered.contains("disc summary not cached"));
        assert!(
            !rendered.contains("Analyzing disc…"),
            "cached-only mode must not claim a worker is running when none is pending"
        );
        assert!(info.audio_streams_pill_row.is_none());
    }

    fn sacd_presentation(area: SacdAreaId, label: &str, channels: u32, layout: &str) -> DiscPresentation {
        DiscPresentation {
            id: PresentationId::SacdArea(area),
            label: label.to_string(),
            format: AudioPresentationFormat {
                codec: Some("DSD".to_string()),
                sample_rate: Some(2_822_400),
                bit_depth: Some(1),
                channels: Some(channels as u8),
                channel_layout: Some(layout.to_string()),
                lossless: true,
                provenance: FormatProvenance::IfoAttributes,
            },
            tracks: vec![DiscTrack {
                number: 1,
                title: None,
                performer: None,
                duration_secs: Some(60.0),
                format_note: None,
            }],
            total_duration_secs: 60.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    fn sacd_contents(source_path: PathBuf) -> DiscContents {
        DiscContents {
            format: DiscFormat::Sacd,
            label: "SACD".to_string(),
            source_path,
            presentations: vec![
                sacd_presentation(SacdAreaId::MultiChannel, "Multichannel", 6, "5.1"),
                sacd_presentation(SacdAreaId::Stereo, "Stereo", 2, "stereo"),
            ],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: "none".to_string() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    #[test]
    fn native_sacd_iso_renders_compact_disc_stream_summary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("album.iso");
        std::fs::write(&iso, b"synthetic sacd iso").expect("iso");
        let metadata = std::fs::metadata(&iso).expect("metadata");
        let entry = BrowseEntry::new(
            iso.clone(),
            "album.iso".to_string(),
            EntryKind::SacdIso,
            metadata.len(),
            metadata.modified().ok(),
        );

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let fingerprint = disc_probe_fingerprint(&iso).expect("disc fingerprint");
        browse.disc_probe_cache.insert(
            iso.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, sacd_contents(iso.clone())),
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            96,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("SACD ISO (ScarletBook)"));
        assert!(rendered.contains("content: 2 audio streams · 2 tracks"));
        assert!(rendered.contains("streams:"));
        assert!(rendered.contains("DSD 2.8MHz stereo"));
        assert!(rendered.contains("DSD 2.8MHz 5.1"));
        assert!(info.audio_streams_pill_row.is_some());
        assert!(info.edit_tags_pill_row.is_some());
    }
}

/// Format a file size for display
fn size_str(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_073_741_824.0 {
        format!("{:.1} GB", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod path_field_render_tests {
    use super::*;

    #[test]
    fn browse_tab_strip_draws_owned_labelled_controls_with_exact_close_hits() {
        use ratatui::{backend::TestBackend, Terminal};

        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");

        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.current_dir = a;
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );

        // Option B is a layout invariant: one tab does not register or consume
        // a strip row at all.
        app.button_map.clear();
        let single_area = Rect::new(0, 0, 100, 30);
        let backend = TestBackend::new(single_area.width, single_area.height);
        let mut terminal = Terminal::new(backend).expect("single-tab terminal");
        terminal
            .draw(|frame| draw_browse_screen(frame, single_area, &mut app, theme))
            .expect("draw single-tab Browse");
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::BrowseDirTabStrip)
            .is_none(), "single-tab Browse must not allocate a tab strip");

        assert!(app.browse.open_dir_in_new_tab(b, false));
        app.button_map.clear();
        let area = Rect::new(0, 0, 60, 1);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_browse_tab_strip(frame, area, &mut app, theme))
            .expect("draw strip");

        assert_eq!(
            app.button_map.find_button_rect(&TuiButton::BrowseDirTabStrip),
            Some(area),
            "the complete row must be owned before narrower hit targets are layered on top",
        );
        assert_eq!(
            app.button_map
                .find_button_rect(&TuiButton::BrowseDirTabClose(0))
                .expect("first close hit")
                .width,
            3,
            "[×] is a three-cell drawing and must own exactly three cells",
        );
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::BrowseDirTabNew)
            .is_some());

        let row = (0..area.width).fold(String::new(), |mut row, x| {
            row.push_str(terminal.backend().buffer().get(x, 0).symbol());
            row
        });
        assert!(row.contains("New Tab"), "wide strips label the new-tab button");
        assert!(!row.contains('⧉'), "the standalone Duplicate button is removed");

        // Width degradation must preserve a non-zero tab allocation and never
        // register a control beyond the one-line strip.
        app.button_map.clear();
        let narrow = Rect::new(0, 0, 10, 1);
        let backend = TestBackend::new(narrow.width, narrow.height);
        let mut terminal = Terminal::new(backend).expect("narrow terminal");
        terminal
            .draw(|frame| draw_browse_tab_strip(frame, narrow, &mut app, theme))
            .expect("draw narrow strip");
        let new_rect = app
            .button_map
            .find_button_rect(&TuiButton::BrowseDirTabNew)
            .expect("compact new-tab hit");
        assert_eq!(new_rect.width, 3);
        assert!(new_rect.right() <= narrow.right());
        assert!(app
            .button_map
            .find_button_rect(&TuiButton::BrowseDirTab(0))
            .is_some(), "width pressure must not zero out all tab cells");
    }

    #[test]
    fn browse_tab_close_glyph_sits_under_its_click_region() {
        use ratatui::{backend::TestBackend, Terminal};

        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("aa");
        let b = temp.path().join("bb");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");

        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.current_dir = a;
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        assert!(app.browse.open_dir_in_new_tab(b, false));

        app.button_map.clear();
        let area = Rect::new(0, 0, 60, 1);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw_browse_tab_strip(frame, area, &mut app, theme))
            .expect("draw strip");
        let buf = terminal.backend().buffer().clone();

        // The registered close region for every tab must actually contain the
        // drawn close glyph; otherwise a real click on the visible [×] misses.
        for i in 0..app.browse.tab_count() {
            if let Some(rect) = app.button_map.find_button_rect(&TuiButton::BrowseDirTabClose(i)) {
                let has_cross = (rect.x..rect.right()).any(|x| buf.get(x, 0).symbol() == "×");
                assert!(
                    has_cross,
                    "tab {i}: no close glyph under its click region {rect:?} — the [×] is drawn elsewhere"
                );
            }
        }
    }

    #[test]
    fn browse_scan_progress_renders_for_empty_and_populated_streams() {
        let mut browse = BrowseState::new();
        browse.current_dir = std::path::PathBuf::from("/music/album");
        let (handle, _cancel) = crate::tui::browse::ScanHandle::new(1);
        browse.scan_pending = Some(handle);

        assert_eq!(
            browse_scan_progress_text(&browse).as_deref(),
            Some("Reading album… (0)")
        );

        browse.entries.push(crate::tui::browse::BrowseEntry::new(
            browse.current_dir.join("track.flac"),
            "track.flac".to_string(),
            crate::tui::browse::EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            0,
            None,
        ));
        browse.scan_discovered_count = 42;
        assert_eq!(
            browse_scan_progress_text(&browse).as_deref(),
            Some("Reading album… (42)")
        );

        browse.scan_pending = None;
        assert!(browse_scan_progress_text(&browse).is_none());
    }

    #[test]
    fn browse_path_go_label_is_lowercase_without_changing_hit_width() {
        assert_eq!(BROWSE_PATH_GO_LABEL, " go ");
        // The label renders inside the 5-cell hit rect and has always been 4
        // cells wide (" Go " was too); the rect width is the unchanged part.
        assert_eq!(BROWSE_PATH_GO_WIDTH, 5);
        assert!(
            crate::tui::display_width::width(BROWSE_PATH_GO_LABEL)
                <= BROWSE_PATH_GO_WIDTH as usize,
        );
    }

    #[test]
    fn path_input_renderer_shows_partial_selection() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let mut input = TextInputState::new("Music/Album".to_string());
        input.selection_anchor = Some("Music/".len());
        input.cursor = "Music/Al".len();

        let (spans, cursor_col) = render_path_input_spans(&input, 32, theme);
        let rendered = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.starts_with(" path: Music/Album"));
        assert_eq!(cursor_col, crate::tui::display_width::width(" path: Music/Al") as u16);
        // P1-1 selection contrast: the inverse-video pair (theme.bg text on
        // a text_bright surface), not the old low-contrast selection_bg.
        assert!(spans.iter().any(|span| {
            span.content.as_ref().contains("Al")
                && span.style.bg == Some(theme.text_bright)
                && span.style.fg == Some(theme.bg)
        }));
    }
}

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
                &app.directory_accounting,
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
    Match,
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
        SearchControlKind::Match => TuiButton::BrowseSearchMatch,
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
    fuzzy: bool,
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
                SearchControlKind::Match,
                format!(" match: {} ", if fuzzy { "fuzzy" } else { "literal" }),
            ),
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
                SearchControlKind::Match,
                format!(" match:{} ", if fuzzy { "fuzzy" } else { "literal" }),
            ),
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
            (
                SearchControlKind::Match,
                if fuzzy { " f " } else { " l " }.to_string(),
            ),
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
    fuzzy: bool,
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
            fuzzy,
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
        fuzzy,
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
        SearchControlKind::Mode | SearchControlKind::Match | SearchControlKind::Sort => {
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
            false,
            "relevance",
            SortDir::Asc,
            false,
        );

        assert_eq!(
            items.iter().map(|item| item.kind).collect::<Vec<_>>(),
            vec![
                SearchControlKind::Recursive,
                SearchControlKind::Mode,
                SearchControlKind::Match,
                SearchControlKind::Sort,
                SearchControlKind::Audio,
            ]
        );
        assert_eq!(items[0].label, " recursive ✓ ");
        assert_eq!(items[1].label, " mode: filename ");
        assert_eq!(items[2].label, " match: literal ");
        assert_eq!(items[3].label, " sort: relevance ▲ ");
        assert_eq!(items[4].label, " all files ");
        assert_layout_inside(80, &items);
    }

    #[test]
    fn search_control_layout_compacts_before_clipping() {
        let items = search_control_row_layout(
            30,
            true,
            "filename",
            false,
            "relevance",
            SortDir::Desc,
            true,
        );

        assert_eq!(items.len(), 5);
        assert_eq!(items[0].label, " r✓ ");
        assert_eq!(items[1].label, " m:f ");
        assert_eq!(items[2].label, " l ");
        assert_eq!(items[3].label, " s▼ ");
        assert_eq!(items[4].label, " a✓ ");
        assert_layout_inside(30, &items);
    }

    #[test]
    fn search_control_layout_never_overflows_narrow_rows() {
        for inner_width in 0..64 {
            let items = search_control_row_layout(
                inner_width,
                true,
                "filename",
                false,
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
            false,
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
            browse.search.fuzzy,
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

        // Row 2: recursive + mode + match + sort + audio, all visibly clickable.
        // The shared layout helper progressively compacts labels and finally
        // omits trailing controls only when the pane is too narrow to display
        // every tiny pill without colliding with the right border.
        let control_items = search_control_row_layout(
            inner_w,
            browse.search.recursive,
            browse.search.mode.label(),
            browse.search.fuzzy,
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
#[derive(Clone)]
struct InfoContent {
    lines: Vec<Vec<Span<'static>>>,
    /// The untruncated first-row title, retained for optional two-line display.
    /// The four-row header stays fixed when late metadata changes this title.
    title: Option<String>,
    /// (field, line_index) for each clickable metadata row. Fields
    /// that are absent but have a "(click to add)" placeholder also
    /// appear here.
    meta_field_rows: Vec<(MetadataField, usize)>,
    /// Line index of the separate directory Refresh/Cancel block (if present).
    refresh_pill_row: Option<usize>,
    /// Line index of the analyze pill (if present).
    analyze_pill_row: Option<usize>,
    /// Line index of the edit tags pill (if present).
    edit_tags_pill_row: Option<usize>,
    /// Line index of the Audio Streams pill (if present).
    audio_streams_pill_row: Option<usize>,
    /// Cursor position for an active inline metadata editor: (line, column).
    inline_cursor: Option<(usize, u16)>,
}

/// Find a directory summary row by its first (label) span. A wrapped metric
/// occupies two physical rows: its label followed by an indented value row.
fn directory_info_metric_block(
    info: &InfoContent,
    label: &str,
) -> Option<(usize, Vec<Vec<Span<'static>>>)> {
    let start = info.lines.iter().position(|row| row.first()
        .is_some_and(|span| span.content.trim() == label))?;
    let mut end = start + 1;
    if info.lines.get(end).is_some_and(|row| row.len() > 1
        && row.first().is_some_and(|span| span.content == "   ")) {
        end += 1;
    }
    Some((start, info.lines[start..end].to_vec()))
}

/// In a compact pane, a wrapped count may fit on the label row if we drop
/// the alignment padding. Preserve the entire value (and its span styles):
/// only the fallback below may abbreviate a value on extremely small panes.
fn tighten_directory_metric_value(
    rows: &[Vec<Span<'static>>],
    content_width: usize,
) -> Option<Vec<Span<'static>>> {
    let label = rows.first()?.first()?;
    let value = rows.get(1)?.get(1..)?;
    let compact_label = format!("{} ", label.content.trim_end());
    let width = super::display_width::width(&compact_label)
        + value.iter().map(|span| super::display_width::width(span.content.as_ref())).sum::<usize>();
    if width > content_width { return None; }
    let mut line = vec![Span::styled(compact_label, label.style)];
    line.extend(value.iter().cloned());
    Some(line)
}

/// If the viewport genuinely cannot fit the required rows, abbreviate only
/// enough wrapped values to keep each label and the active action visible.
fn compact_directory_metric_value(
    rows: &[Vec<Span<'static>>],
    content_width: usize,
) -> Vec<Span<'static>> {
    let label = &rows[0][0];
    let compact_label = format!("{} ", label.content.trim_end());
    let mut line = vec![Span::styled(compact_label.clone(), label.style)];
    let mut remaining = content_width.saturating_sub(super::display_width::width(&compact_label));
    for span in rows[1].iter().skip(1) {
        if remaining == 0 { break; }
        let value = truncate_to(span.content.as_ref(), remaining);
        remaining = remaining.saturating_sub(super::display_width::width(&value));
        line.push(Span::styled(value, span.style));
    }
    line
}

/// Short Browse panes prioritize the four counted rows and the active action.
/// Retain optional summary values and then as many classification details as
/// fit. The full-height Info pane never calls this function.
fn compact_directory_info(
    mut info: InfoContent,
    content_height: usize,
    content_width: usize,
) -> InfoContent {
    let mut mandatory = ["Folders", "Files", "Audio files", "Total size"]
        .into_iter().filter_map(|label| directory_info_metric_block(&info, label))
        .collect::<Vec<_>>();
    if mandatory.len() != 4 { return info; }
    let action = info.refresh_pill_row;
    let action_rows = if action.is_some() { 1 } else { 0 };
    let mut required = mandatory.iter().map(|(_, rows)| rows.len()).sum::<usize>()
        + action_rows;
    // Before clipping any count, reclaim unused label-column padding in just
    // the wrapped rows. Prefer to fit the title as well as all four complete
    // values and the action; at 66x24 this tightens two unknown-count rows.
    let preferred_required = content_height.saturating_sub(usize::from(!info.lines.is_empty()));
    for (_, rows) in &mut mandatory {
        if required <= preferred_required { break; }
        if rows.len() > 1 {
            if let Some(tight) = tighten_directory_metric_value(rows, content_width) {
                *rows = vec![tight];
                required -= 1;
            }
        }
    }
    // Truly tiny viewports may still need shortened numbers; this fallback
    // never runs for the 66x24 all-unknown case.
    for (_, rows) in &mut mandatory {
        if required <= content_height { break; }
        if rows.len() > 1 {
            *rows = vec![compact_directory_metric_value(rows, content_width)];
            required -= 1;
        }
    }
    let mut remaining = content_height.saturating_sub(required);
    let mut selected = mandatory;
    // The title is the shared visual origin, not an optional directory
    // statistic. Counts and the active control still take precedence when a
    // genuinely tiny viewport cannot accommodate both.
    if remaining > 0 && !info.lines.is_empty() {
        selected.push((0, vec![info.lines[0].clone()]));
        remaining -= 1;
    }
    let show_blank = action.is_some() && remaining > 0;
    if show_blank { remaining -= 1; }
    // Optional metric blocks retain their original physical wrapping and order.
    for label in ["Disk free", "Updated", "Kind"] {
        if let Some(block) = directory_info_metric_block(&info, label) {
            if block.1.len() <= remaining {
                remaining -= block.1.len();
                selected.push(block);
            }
        }
    }
    if let Some(row) = action {
        if show_blank { selected.push((row - 1, vec![Vec::new()])); }
        selected.push((row, vec![info.lines[row].clone()]));
    }
    // All media views use compact_media_info instead. Keep the original
    // plain-directory supplemental-row policy and its mapped button index.
    let supplemental_start = action.map(|row| row + 1).or_else(|| {
        ["Updated", "Disk free", "Total size"].into_iter()
            .find_map(|label| directory_info_metric_block(&info, label)
                .map(|(start, rows)| start + rows.len()))
    }).unwrap_or(info.lines.len());
    for (row, spans) in info.lines.iter().enumerate().skip(supplemental_start) {
        if remaining == 0 { break; }
        selected.push((row, vec![spans.clone()]));
        remaining -= 1;
    }

    selected.sort_by_key(|(start, _)| *start);
    let mut compact = Vec::new();
    let mut row_mapping = Vec::new();
    for (start, rows) in selected {
        for (offset, line) in rows.into_iter().enumerate() {
            row_mapping.push((start + offset, compact.len()));
            compact.push(line);
        }
    }
    let remap = |original: Option<usize>| -> Option<usize> {
        original.and_then(|row| row_mapping.iter()
            .find(|(old, _)| *old == row).map(|(_, new)| *new))
    };
    info.refresh_pill_row = remap(info.refresh_pill_row);
    info.analyze_pill_row = remap(info.analyze_pill_row);
    info.edit_tags_pill_row = remap(info.edit_tags_pill_row);
    info.audio_streams_pill_row = remap(info.audio_streams_pill_row);
    info.meta_field_rows = info.meta_field_rows.into_iter()
        .filter_map(|(field, row)| remap(Some(row)).map(|new| (field, new)))
        .collect();
    info.inline_cursor = info.inline_cursor.and_then(|(row, col)|
        remap(Some(row)).map(|new| (new, col)));
    info.lines = compact;
    info
}

// A short Info pane cannot accommodate the reference header plus enriched
// disc rows and controls. Select its compact geometry before the probe cache
// fills, including the 14-row content pane at a 160x30 terminal.
const MEDIA_COMPACT_MAX_CONTENT_ROWS: usize = 14;

/// Compact media views anchor the title and first body row independently of
/// probe-cache detail. Wrapped fields remain atomic, and controls remain mapped
/// to their painted positions.
fn compact_media_info(mut info: InfoContent, height: usize) -> InfoContent {
    if height > MEDIA_COMPACT_MAX_CONTENT_ROWS && info.lines.len() <= height {
        return info;
    }
    let mut selected: Vec<usize> = Vec::new();
    let mut budget = height;
    let button = info.refresh_pill_row.or(info.audio_streams_pill_row);
    for row in [button, info.edit_tags_pill_row].into_iter().flatten() {
        if budget > 0 && !selected.contains(&row) {
            selected.push(row);
            budget -= 1;
        }
    }
    // The first row is the unlabelled title. Secondary header rows stay out
    // of compact views so cache enrichment cannot push the first metric down.
    if budget > 0 && !info.lines.is_empty() {
        selected.push(0);
        budget -= 1;
    }
    let body_start = 4.min(info.lines.len());
    let labels = ["Audio", "Audio files", "Discs", "Contents", "Size", "Modified",
        "Format", "Mixes", "Areas", "Protection", "Streams", "Video", "Content",
        "Duration", "Status"];
    // Compact push_media_row emits [indent, label] and then [indent, value].
    // Recognize both that form and the regular aligned label prefix, and do
    // not absorb the next compact label into the current wrapped field.
    let is_label_row = |row: &[Span<'static>], label: &str| {
        row.first().is_some_and(|span| span.content.trim() == label)
            || (row.first().is_some_and(|span| span.content == MEDIA_INDENT)
                && row.get(1).is_some_and(|span| span.content.trim() == label))
    };
    let mut add_group = |label: &str, selected: &mut Vec<usize>, budget: &mut usize| {
        if let Some(start) = (body_start..info.lines.len()).find(|&row|
            is_label_row(&info.lines[row], label)) {
            let mut end = start + 1;
            while end < info.lines.len() && !info.lines[end].is_empty()
                && info.lines[end].first().is_some_and(|span| span.content.trim().is_empty())
                && !labels.iter().any(|other| is_label_row(&info.lines[end], other))
                && Some(end) != button {
                end += 1;
            }
            if end - start <= *budget && !selected.contains(&start) {
                selected.extend(start..end);
                *budget -= end - start;
            }
        }
    };
    for label in labels.iter().copied() {
        if budget == 0 { break; }
        add_group(label, &mut selected, &mut budget);
    }
    // Preserve one blank row before the controls when space is available.
    if budget > 0 {
        if let Some(row) = button.filter(|row| *row > 0 && info.lines[row - 1].is_empty()) {
            selected.push(row - 1);
        }
    }
    selected.sort_unstable();
    selected.dedup();
    let remap = |old: Option<usize>| old.and_then(|row| selected.iter()
        .position(|value| *value == row));
    info.refresh_pill_row = remap(info.refresh_pill_row);
    info.audio_streams_pill_row = remap(info.audio_streams_pill_row);
    info.edit_tags_pill_row = remap(info.edit_tags_pill_row);
    info.analyze_pill_row = remap(info.analyze_pill_row);
    info.meta_field_rows.retain(|(_, row)| selected.contains(row));
    info.meta_field_rows = info.meta_field_rows.into_iter()
        .filter_map(|(field, row)| remap(Some(row)).map(|index| (field, index))).collect();
    info.inline_cursor = info.inline_cursor.and_then(|(row, col)|
        remap(Some(row)).map(|index| (index, col)));
    info.lines = selected.into_iter().map(|row| info.lines[row].clone()).collect();
    info
}

/// Split at a word boundary when it uses most of the first row, otherwise
/// break at a display-column boundary. The second line may be ellipsized: a
/// long title never takes space from accounting rows or action controls.
fn wrapped_info_title(title: &str, max_width: usize) -> (String, Option<String>) {
    let clean = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if super::display_width::width(&clean) <= max_width {
        return (clean, None);
    }
    let fitted = super::display_width::fit_prefix(&clean, max_width);
    let split = fitted.char_indices().rev()
        .find(|(offset, ch)| *offset > 0 && ch.is_whitespace()
            && super::display_width::width(&clean[..*offset]) >= max_width / 2)
        .map(|(offset, _)| offset)
        .unwrap_or(fitted.len());
    let (first, rest) = clean.split_at(split);
    (first.trim_end().to_string(), Some(truncate_to(rest.trim_start(), max_width)))
}

/// Use the existing blank fourth header row for a stationary continuation
/// slot, rather than inserting a row. Artist and edition move once with the
/// fixed full-height geometry, not when an async disc/tag probe completes.
/// Compact layouts keep their established one-row ellipsized titles and
/// prioritise labels, counts, and buttons over wrapping.
fn wrap_full_info_title(mut info: InfoContent, width: usize,
    theme: super::theme::Theme) -> InfoContent {
    if info.lines.len() < 4 || !info.lines[3].is_empty() {
        return info;
    }
    let Some(title) = info.title.as_deref() else { return info; };
    let max_width = width.saturating_sub(MEDIA_INDENT.len());
    if max_width == 0 { return info; }
    let (first, continuation) = wrapped_info_title(title, max_width);
    // The header renderer already owns these cells and their foreground style.
    info.lines[0] = vec![Span::raw(MEDIA_INDENT),
        Span::styled(first, theme.bold(theme.text_bright))];
    let artist = std::mem::take(&mut info.lines[1]);
    let descriptor = std::mem::replace(&mut info.lines[2], artist);
    info.lines[3] = descriptor;
    info.lines[1] = continuation.map(|line| vec![Span::raw(MEDIA_INDENT),
        Span::styled(line, theme.bold(theme.text_bright))]).unwrap_or_default();
    info
}

/// Return a painted pill's actual cell range. Refresh and Streams can share
/// one row; computing both positions from spans keeps click hitboxes exact.
fn info_pill_offset(spans: &[Span<'_>], caption: &str) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    for span in spans {
        let width = super::display_width::width(span.content.as_ref());
        if span.content == caption { return Some((offset, width)); }
        offset += width;
    }
    None
}

fn draw_browse_info(
    f: &mut Frame,
    area: Rect,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
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
    let refresh_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoRefresh);
    let info = if let Some(entry) = browse.selected_entry() {
        entry_info_lines(
            entry,
            browse,
            accounting,
            content_width,
            analyze_hovered,
            edit_tags_hovered,
            audio_streams_hovered,
            refresh_hovered,
            inline_edit,
            info_focus,
            theme)
    } else {
        InfoContent {
            lines: vec![vec![Span::styled("   (no selection)", theme.muted())]],
            title: None,
            meta_field_rows: Vec::new(),
            refresh_pill_row: None,
            analyze_pill_row: None,
            edit_tags_pill_row: None,
            audio_streams_pill_row: None,
            inline_cursor: None,
        }
    };

    // The full Info pane retains the reference's order and spacing. On a
    // compact Browse screen, optional wrapping must never scroll away one of
    // the four required accounting rows or the active Refresh/Cancel control.
    let physical_folder = browse.selected_entry().is_some_and(|entry|
        !browse.is_in_archive() &&
            (matches!(entry.kind, EntryKind::ParentDir) || entry.is_child_dir())
            && !entry.is_symlink);
    let media_selection = browse.selected_entry().is_some_and(|entry| {
        entry.is_disc_source() || entry.is_video_transport_file()
        || (matches!(entry.kind, EntryKind::Archive) && entry.path.extension()
            .and_then(|v| v.to_str()).is_some_and(|v| v.eq_ignore_ascii_case("iso")))
        || (entry.is_child_dir() && browse.current_folder_classification().is_some_and(|c|
            matches!(c.kind, FolderClassificationKind::Album | FolderClassificationKind::Disc |
                FolderClassificationKind::MultiDisc)))
    });
    let compact_media = media_selection && (content_height <= MEDIA_COMPACT_MAX_CONTENT_ROWS
        || info.lines.len() > content_height);
    let compact_directory = !media_selection && physical_folder
        && info.lines.len() > content_height;
    let info = if media_selection {
        compact_media_info(info, content_height)
    } else if compact_directory {
        compact_directory_info(info, content_height, content_width)
    } else { info };
    // Never reinterpret a compact layout as a four-row header: its rows are
    // reindexed and its hitboxes already refer to that compact ordering.
    let info = if compact_media || compact_directory { info } else {
        wrap_full_info_title(info, content_width, theme)
    };
    let start_row = 0;

    // Render content lines with border
    for line_spans in info.lines.iter().skip(start_row).take(content_height) {
        lines.push(bordered_line(border_color, w, line_spans.clone(), theme));
    }

    // Fill remaining
    let rendered = info.lines.len().saturating_sub(start_row).min(content_height);
    for _ in rendered..content_height {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    if let Some((line_idx, col)) = info.inline_cursor {
        if line_idx >= start_row && line_idx - start_row < content_height {
            f.set_cursor(area.x + 1 + col, area.y + 1 + (line_idx - start_row) as u16);
        }
    }

    // Register clickable metadata fields in the info pane. Only for
    // lines that fit within the visible content area.
    let info_y_start = area.y + 1; // below top border
    for (field, line_idx) in &info.meta_field_rows {
        if *line_idx >= start_row && *line_idx - start_row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoMeta(*field),
                Rect::new(
                    area.x + 1,
                    info_y_start + (*line_idx - start_row) as u16,
                    (w - 2) as u16,
                    1,
                ),
            );
        }
    }

    // The Refresh/Cancel pill is painted on the media action row, adjacent to
    // Streams when available. Locate it from its spans rather than assuming the
    // C4 directory indent of three columns.
    if let Some(row) = info.refresh_pill_row {
        if row >= start_row && row - start_row < content_height {
            let spans = &info.lines[row];
            let pill = if spans.iter().any(|s| s.content == " Cancel ") {
                " Cancel "
            } else { " Refresh " };
            if let Some((left, width)) = info_pill_offset(spans, pill) {
                buttons.record_button(TuiButton::BrowseInfoRefresh,
                    Rect::new(area.x + 1 + left as u16,
                        info_y_start + (row - start_row) as u16,
                        width as u16, 1));
            }
        }
    }
    // Register pill buttons if they fit.
    if let Some(row) = info.analyze_pill_row {
        if row >= start_row && row - start_row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAnalyze,
                Rect::new(area.x + 1, info_y_start + (row - start_row) as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.edit_tags_pill_row {
        if row >= start_row && row - start_row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoEditTags,
                Rect::new(area.x + 1, info_y_start + (row - start_row) as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.audio_streams_pill_row {
        if row >= start_row && row - start_row < content_height {
            if let Some(spans) = info.lines.get(row) {
                if let Some(pill) = spans.last().filter(|span| span.content == " Streams… ") {
                    let left = spans[..spans.len() - 1].iter()
                        .map(|span| super::display_width::width(span.content.as_ref()))
                        .sum::<usize>();
                    let width = super::display_width::width(pill.content.as_ref());
                    if left + width <= w.saturating_sub(2) {
                        buttons.record_button(
                            TuiButton::BrowseInfoAudioStreams,
                            Rect::new(area.x + 1 + left as u16,
                                info_y_start + (row - start_row) as u16, width as u16, 1),
                        );
                    }
                }
            }
        }
    }
}

/// Truncate a string to fit within `max_chars` columns, adding ellipsis if needed.
#[allow(dead_code)]
pub(crate) fn truncate_for_disc_overlay(s: &str, max_chars: usize) -> String { truncate_to(s, max_chars) }

fn truncate_to(s: &str, max_chars: usize) -> String {
    super::display_width::truncate_right(s, max_chars)
}

/// Plain-directory details share the accounting metrics' value column.
/// "Audio files" is always one of the four counted rows and is the longest
/// metric label, including when disk space and modification time are absent.
fn directory_metric_label_width(content_width: usize) -> usize {
    let max_label = "Audio files".len();
    let gap = if content_width >= 3 + max_label + 2 + 10 { 2 } else { 1 };
    max_label + gap
}

/// One label column for Kind, provisional Content, the four counted rows,
/// Disk free and Updated.
fn push_directory_metric_line(
    lines: &mut Vec<Vec<Span<'static>>>,
    key: &'static str,
    value: Vec<Span<'static>>,
    label_width: usize,
    theme: super::theme::Theme,
) {
    let mut row = vec![Span::styled(format!("   {key:label_width$}"), theme.muted())];
    row.extend(value);
    lines.push(row);
}

/// Keep provisional directory facts in the same column as the counted rows.
/// Narrow panes wrap a long value rather than painting over the right border.
fn push_directory_detail_line(
    lines: &mut Vec<Vec<Span<'static>>>,
    key: &'static str,
    value: &'static str,
    style: Style,
    content_width: usize,
    theme: super::theme::Theme,
) {
    let label_width = directory_metric_label_width(content_width);
    let value_width = super::display_width::width(value);
    if value_width <= content_width.saturating_sub(3 + label_width) {
        push_directory_metric_line(lines, key, vec![Span::styled(value, style)],
            label_width, theme);
    } else {
        push_directory_metric_line(lines, key, vec![], label_width, theme);
        lines.push(vec![Span::raw("   "),
            Span::styled(truncate_to(value, content_width.saturating_sub(3)), style)]);
    }
}

/// Binary units for directory contents and filesystem capacity. The general
/// file-list formatter is unchanged for compatibility with existing columns.
fn directory_size_str(bytes: u64) -> String {
    let bytes_f = bytes as f64;
    if bytes >= 1 << 40 {
        format!("{:.2} TiB", bytes_f / (1u64 << 40) as f64)
    } else if bytes >= 1 << 30 {
        format!("{:.2} GiB", bytes_f / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes_f / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.1} KiB", bytes_f / (1u64 << 10) as f64)
    } else {
        format!("{bytes} B")
    }
}

enum DirectoryMetricValue {
    Known(String),
    Unknown,
    Scanning(String),
    Disk(u64, u64),
}

fn directory_spinner() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() / 125;
    FRAMES[tick as usize % FRAMES.len()]
}

/// The action is a separate block below the summary, never a value for
/// Total size. The four counted rows exist even without measurements.
fn push_directory_stats_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    entry_path: &Path,
    entry_date_label: &str,
    content_width: usize,
    refresh_hovered: bool,
    refresh_focused: bool,
    theme: super::theme::Theme,
) -> Option<usize> {
    use crate::directory_accounting::{Freshness, format_bytes, format_exact};
    let view = accounting.view(entry_path);
    let running = view.is_some_and(|v| v.running) || accounting.scan_pending(entry_path);
    let progress = view.and_then(|v| v.progress.as_ref());
    let cached = view.and_then(|v| v.cached.as_ref())
        .filter(|v| v.freshness != Freshness::KnownInvalid);
    let local = view.and_then(|v| v.local);
    // Folders is the *immediate* child-directory count shown in the image.
    // Files and Audio files are recursive totals: a one-level preview does
    // not establish their values (nor the Total size). Their values are
    // unknown until a complete measurement is available, or provisional
    // while the exhaustive walker runs. Do not mix these scopes.
    let listing = (entry_path == browse.current_dir.as_path())
        .then(|| browse.current_directory_listing_counts()).flatten();
    let direct_folders = local.map(|counts| counts.folders)
        .or_else(|| listing.as_ref().map(|counts| counts.folders as u128));
    let completed = cached.map(|v| v.measurement.counts);

    let mut metrics: Vec<(&'static str, DirectoryMetricValue)> = Vec::new();
    let folders = if let Some(count) = direct_folders {
        DirectoryMetricValue::Known(format_exact(count))
    } else if running {
        DirectoryMetricValue::Scanning(format_exact(progress.map_or(0, |p| p.folders_discovered)))
    } else {
        DirectoryMetricValue::Unknown
    };
    metrics.push(("Folders", folders));
    for (label, completed_value, scanned_value) in [
        ("Files", completed.map(|counts| counts.files), progress.map(|p| p.files_examined)),
        ("Audio files", completed.map(|counts| counts.audio), progress.map(|p| p.audio_examined)),
    ] {
        let value = if running && completed_value.is_none() {
            DirectoryMetricValue::Scanning(format_exact(scanned_value.unwrap_or(0)))
        } else if let Some(value) = completed_value {
            DirectoryMetricValue::Known(format_exact(value))
        } else {
            DirectoryMetricValue::Unknown
        };
        metrics.push((label, value));
    }
    let total = if running && cached.is_none() {
        DirectoryMetricValue::Scanning(format!("{}…",
            format_bytes(progress.map_or(0, |p| p.bytes_examined))))
    } else if let Some(cached) = cached {
        DirectoryMetricValue::Known(format_bytes(cached.measurement.counts.bytes))
    } else {
        DirectoryMetricValue::Unknown
    };
    metrics.push(("Total size", total));
    if let Some((available, total)) = browse.current_filesystem_space() {
        metrics.push(("Disk free", DirectoryMetricValue::Disk(available, total)));
    }
    if !entry_date_label.is_empty() {
        metrics.push(("Updated", DirectoryMetricValue::Known(entry_date_label.into())));
    }
    let has_unknown = metrics.iter().any(|(label, value)|
        matches!(*label, "Files" | "Audio files" | "Total size")
            && matches!(value, DirectoryMetricValue::Unknown));

    // On a narrow terminal one inter-column space fits 'unknown' without
    // orphaning its value. Keep the image's two spaces at normal widths.
    let label_width = directory_metric_label_width(content_width);
    let available = content_width.saturating_sub(3 + label_width);
    for (label, value) in metrics {
        let mut spans: Vec<Span<'static>> = Vec::new();
        match value {
            DirectoryMetricValue::Known(value) => {
                spans.push(Span::styled(value, Style::default().fg(theme.cyan)));
            }
            DirectoryMetricValue::Unknown => {
                spans.push(Span::styled("unknown", Style::default().fg(theme.text_dim)));
            }
            DirectoryMetricValue::Scanning(value) => {
                spans.push(Span::styled(format!("{} ", directory_spinner()),
                    Style::default().fg(theme.purple)));
                spans.push(Span::styled(value, Style::default().fg(theme.cyan)));
            }
            DirectoryMetricValue::Disk(free, capacity) => {
                let size = if content_width < 35 {
                    format!("{}/{}", directory_size_str(free), directory_size_str(capacity))
                        .replace(" TiB", "T").replace(" GiB", "G")
                        .replace(" MiB", "M").replace(" KiB", "K").replace(" B", "B")
                } else {
                    format!("{} / {}", directory_size_str(free), directory_size_str(capacity))
                };
                // Used portion is solid blue; free portion is shaded. Unlike
                // a percent-only indicator, the fraction is calculated from
                // the same available/total units printed beside the bar.
                let slots = if content_width < 35 { 5usize } else { 12usize };
                let used = if capacity == 0 { 0 } else {
                    (((capacity.saturating_sub(free.min(capacity))) as u128 * slots as u128)
                        / capacity as u128) as usize
                };
                spans.push(Span::styled("█".repeat(used), Style::default().fg(theme.cyan)));
                spans.push(Span::styled("░".repeat(slots - used), Style::default().fg(theme.text_dim)));
                spans.push(Span::raw(if content_width < 35 { " " } else { "  " }));
                spans.push(Span::styled(size, Style::default().fg(theme.cyan)));
            }
        }
        let value_width = spans.iter().map(|span| super::display_width::width(span.content.as_ref())).sum::<usize>();
        if value_width <= available {
            push_directory_metric_line(lines, label, spans, label_width, theme);
        } else {
            // Do not silently truncate 'unknown', a running size, or the
            // volume capacity. Narrow panes wrap the *value* as a unit.
            push_directory_metric_line(lines, label, vec![], label_width, theme);
            let max_width = content_width.saturating_sub(3);
            let mut row = vec![Span::raw("   ")];
            let mut remaining = max_width;
            for span in spans {
                let text = truncate_to(span.content.as_ref(), remaining);
                remaining = remaining.saturating_sub(super::display_width::width(&text));
                row.push(Span::styled(text, span.style));
                if remaining == 0 { break; }
            }
            lines.push(row);
        }
    }
    if !running && !has_unknown { return None; }
    // One blank line below Updated (or Total size when optional rows are
    // absent). The control always aligns with the labels, not the value column.
    lines.push(Vec::new());
    let row = lines.len();
    let caption = if running { " Cancel " } else { " Refresh " };
    let _ = (refresh_hovered, refresh_focused); // Keep the purple block in both states.
    lines.push(vec![
        Span::raw("   "),
        Span::styled(caption, Style::default().fg(theme.bg).bg(theme.purple)
            .add_modifier(Modifier::BOLD)),
    ]);
    Some(row)
}

/// Display facts from cached tags or a disc probe. Missing fields are omitted.
struct BrowseMediaHeader {
    title: String,
    artist: Option<String>,
    year: Option<String>,
    catalog: Option<String>,
}

/// A conflicting album tag is not authoritative. Missing probe results do not
/// cause extra I/O from the renderer.
fn album_tag_consensus(
    browse: &BrowseState,
    audio: &FolderAudioSummary,
    field: fn(&super::probe::SourceMetadata) -> Option<&str>,
) -> Option<String> {
    let mut result: Option<String> = None;
    for path in &audio.file_paths {
        let Some(info) = browse.probe_info_for_path(path) else { continue };
        let Some(value) = field(&info.metadata).map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        if let Some(existing) = &result {
            if !existing.eq_ignore_ascii_case(value) { return None; }
        } else {
            result = Some(value.to_owned());
        }
    }
    result
}

fn media_header_for_entry(
    entry: &BrowseEntry,
    browse: &BrowseState,
    disc_contents: Option<&crate::disc::DiscContents>,
) -> Option<BrowseMediaHeader> {
    let classification = if matches!(entry.kind, EntryKind::Directory) && !browse.is_in_archive() {
        browse.current_folder_classification().filter(|c| matches!(c.kind,
            FolderClassificationKind::Album | FolderClassificationKind::Disc |
            FolderClassificationKind::MultiDisc))
    } else { None };
    if classification.is_none() && !entry.is_disc_source() && !entry.is_video_transport_file() {
        return None;
    }
    let fallback_title = if (entry.is_disc_source() || entry.is_video_transport_file())
        && !entry.is_child_dir() {
        entry.path.file_stem().and_then(|s| s.to_str())
            .filter(|s| !s.is_empty()).unwrap_or(&entry.name).to_owned()
    } else { entry.name.clone() };
    if entry.is_video_transport_file() {
        return Some(BrowseMediaHeader {
            title: fallback_title, artist: None, year: None, catalog: None,
        });
    }
    let disc = entry.is_disc_source() || classification.is_some_and(|c|
        c.kind == FolderClassificationKind::Disc);
    if disc {
        let meta = if matches!(entry.kind, EntryKind::SacdIso) {
            browse.current_cached_info().map(|cache| &cache.metadata)
        } else { None };
        return Some(BrowseMediaHeader {
            title: disc_contents.and_then(|c| c.album_title.as_deref())
                .or_else(|| meta.and_then(|m| m.album.as_deref()))
                .filter(|v| !v.trim().is_empty()).unwrap_or(&fallback_title).to_owned(),
            artist: disc_contents.and_then(|c| c.album_artist.as_deref())
                .or_else(|| meta.and_then(|m| m.artist.as_deref()))
                .filter(|v| !v.trim().is_empty()).map(str::to_owned),
            year: disc_contents.and_then(|c| c.year.as_deref())
                .or_else(|| meta.and_then(|m| m.year.as_deref()))
                .filter(|v| !v.trim().is_empty()).map(str::to_owned),
            catalog: meta.and_then(|m| m.catalog_number.as_deref())
                .filter(|v| !v.trim().is_empty()).map(str::to_owned),
        });
    }
    let audio = &classification?.audio;
    Some(BrowseMediaHeader {
        title: album_tag_consensus(browse, audio, |m| m.album.as_deref())
            .unwrap_or(fallback_title),
        artist: album_tag_consensus(browse, audio, |m| m.artist.as_deref()),
        year: album_tag_consensus(browse, audio, |m| m.year.as_deref()),
        catalog: album_tag_consensus(browse, audio, |m| m.catalog_number.as_deref()),
    })
}

/// Transport previews share the same value column and compact wrapping as
/// the album and disc rows. No alternate indent or label grid is needed.
fn push_media_field(
    lines: &mut Vec<Vec<Span<'static>>>,
    label: &'static str,
    value: impl AsRef<str>,
    content_width: usize,
    theme: super::theme::Theme,
) {
    push_media_row(lines, label, vec![Span::styled(value.as_ref().to_owned(),
        theme.text_style())], content_width, theme);
}

/// Render only the transport details collected by the asynchronous FFmpeg
/// probe. There is deliberately no Streams button: the disc browser operates
/// on complete disc sources, not individual VOB/M2TS transport files.
fn push_transport_stream_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    summary: &crate::tui::probe::TransportStreamSummary,
    content_width: usize,
    theme: super::theme::Theme,
) {
    let noun = |count: usize| if count == 1 { "stream" } else { "streams" };
    push_media_field(lines, "Content", format!("{} video {} · {} audio {}",
        summary.video_count, noun(summary.video_count),
        summary.audio_count, noun(summary.audio_count)), content_width, theme);

    for (index, video) in summary.videos.iter().enumerate() {
        let details = if video.width > 0 && video.height > 0 {
            format!("{} · {}×{}", video.codec, video.width, video.height)
        } else {
            video.codec.clone()
        };
        push_media_field(lines, "Video", format!("{}: {details}", index + 1), content_width, theme);
    }
    if summary.video_count > summary.videos.len() {
        push_media_field(lines, "Video", format!("… and {} more",
            summary.video_count - summary.videos.len()), content_width, theme);
    }
    for (index, audio) in summary.audios.iter().enumerate() {
        let mut details = vec![audio.codec.clone()];
        if !audio.channel_layout.is_empty() {
            details.push(audio.channel_layout.clone());
        }
        if audio.sample_rate > 0 {
            let khz = audio.sample_rate as f64 / 1000.0;
            details.push(if audio.sample_rate % 1000 == 0 {
                format!("{} kHz", audio.sample_rate / 1000)
            } else {
                format!("{khz:.1} kHz")
            });
        }
        push_media_field(lines, "Audio", format!("{}: {}", index + 1, details.join(" · ")),
            content_width, theme);
    }
    if summary.audio_count == 0 {
        push_media_field(lines, "Audio", "no audio streams", content_width, theme);
    } else if summary.audio_count > summary.audios.len() {
        push_media_field(lines, "Audio", format!("… and {} more",
            summary.audio_count - summary.audios.len()), content_width, theme);
    }
    if summary.duration_secs.is_finite() && summary.duration_secs > 0.0 {
        push_media_field(lines, "Duration",
            crate::tui::disc_browser::duration_display(summary.duration_secs),
            content_width, theme);
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

fn push_folder_classification_lines(
    lines: &mut Vec<Vec<Span<'static>>>,
    classification: &FolderContentClassification,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    entry_path: &Path,
    entry_size: u64,
    entry_date_label: &str,
    content_width: usize,
    cached_disc_contents: Option<&crate::disc::DiscContents>,
    audio_streams_hovered: bool,
    audio_streams_pill_row: &mut Option<usize>,
    refresh_hovered: bool,
    refresh_focused: bool,
    refresh_pill_row: &mut Option<usize>,
    is_symlink: bool,
    theme: super::theme::Theme,
) {
    match classification.kind {
        FolderClassificationKind::Album | FolderClassificationKind::Disc |
        FolderClassificationKind::MultiDisc => push_media_folder_layout(
            lines, classification, browse, accounting, entry_path, entry_date_label,
            content_width, cached_disc_contents, audio_streams_hovered,
            audio_streams_pill_row, refresh_hovered, refresh_pill_row, is_symlink, theme,
        ),
        FolderClassificationKind::Collection | FolderClassificationKind::Unknown => {
            push_directory_detail_line(lines, "Kind", "directory", theme.text_style(),
                content_width, theme);
            if !is_symlink {
                *refresh_pill_row = push_directory_stats_lines(
                    lines, browse, accounting, entry_path, entry_date_label, content_width,
                    refresh_hovered, refresh_focused, theme,
                );
            }
        }
    }
    let _ = entry_size;

}

// Media previews share a fixed value column with an explicit label/value gap.
// Plain directories retain their existing C4 metric formatter.
const MEDIA_INDENT: &str = " ";
const MEDIA_LABEL_COLUMNS: usize = 12;

fn media_accent(value: impl Into<String>, theme: super::theme::Theme) -> Span<'static> {
    Span::styled(value.into(), Style::default().fg(theme.cyan))
}

fn media_secondary(value: impl Into<String>, theme: super::theme::Theme) -> Span<'static> {
    Span::styled(value.into(), Style::default().fg(theme.text_muted))
}

fn media_count(count: usize, singular: &'static str, plural: &'static str,
    theme: super::theme::Theme) -> Vec<Span<'static>> {
    vec![media_accent(count.to_string(), theme),
        media_secondary(format!(" {}", if count == 1 { singular } else { plural }), theme)]
}

/// Render media values inside the Info pane. Normal panes retain a shared
/// value column. Narrow panes use a shorter label gap when an entire value
/// fits; otherwise the label is put above the value. Word breaks preserve
/// status text, while a count and its noun stay on the same physical line.
fn push_media_row(
    lines: &mut Vec<Vec<Span<'static>>>,
    label: &'static str,
    values: Vec<Span<'static>>,
    content_width: usize,
    theme: super::theme::Theme,
) {
    let prefix = format!("{}{label:<width$} ", MEDIA_INDENT, width = MEDIA_LABEL_COLUMNS);
    let prefix_width = super::display_width::width(&prefix);
    let value_width = content_width.saturating_sub(prefix_width);
    let width_of = |span: &Span<'static>| super::display_width::width(span.content.as_ref());
    let total_value_width: usize = values.iter().map(width_of).sum();
    // A number and its noun are one semantic value, not two wrap points.
    let is_count_noun = |count: &Span<'static>, noun: &Span<'static>| {
        !count.content.is_empty()
            && count.content.chars().all(|c| c.is_ascii_digit())
            && noun.content.starts_with(' ')
            && noun.content.trim().chars().all(|c| c.is_ascii_alphabetic() || c == ' ')
    };
    let first_value_width = values.first().map_or(0, width_of);
    let widest_count_noun = values.windows(2)
        .filter(|pair| is_count_noun(&pair[0], &pair[1]))
        .map(|pair| width_of(&pair[0]) + width_of(&pair[1]))
        .max().unwrap_or(0);
    let widest_value = first_value_width.max(widest_count_noun);

    // At width 22, "Modified  1970-01-01" fits on one line even though
    // the normal 12-column grid does not. Do not make Contents or Audio
    // harder to read by forcing their long values onto that short line.
    let tight_prefix = format!("{}{label}  ", MEDIA_INDENT);
    let tight_width = super::display_width::width(&tight_prefix);
    let tight = !label.is_empty() && widest_value > value_width
        && tight_width <= content_width
        && total_value_width <= content_width - tight_width;
    let compact = !tight && (content_width < prefix_width.saturating_add(4)
        || widest_value > value_width);
    if compact && !label.is_empty() {
        lines.push(vec![Span::raw(MEDIA_INDENT), Span::styled(
            truncate_to(label, content_width.saturating_sub(MEDIA_INDENT.len())), theme.muted())]);
    }
    let row_prefix = if compact { MEDIA_INDENT.to_string() }
        else if tight { tight_prefix } else { prefix };
    let row_prefix_width = super::display_width::width(&row_prefix);
    // Continuations belong to the current field, not a new labelled field.
    // Preserve the value column in inline layouts and the compact indent.
    let continuation_prefix = if compact { MEDIA_INDENT.to_string() }
        else { " ".repeat(row_prefix_width) };
    let available = content_width.saturating_sub(row_prefix_width);
    let mut row = vec![Span::styled(row_prefix.clone(), theme.muted())];
    let mut used = 0usize;

    // Group adjacent count/noun spans without losing their separate styles.
    let mut segments: Vec<Vec<Span<'static>>> = Vec::with_capacity(values.len());
    let mut iter = values.into_iter().peekable();
    while let Some(span) = iter.next() {
        if iter.peek().is_some_and(|noun| is_count_noun(&span, noun)) {
            segments.push(vec![span, iter.next().expect("peeked count noun")]);
        } else {
            segments.push(vec![span]);
        }
    }
    for (index, segment) in segments.iter().enumerate() {
        let segment_width: usize = segment.iter().map(width_of).sum();
        let separator = segment.len() == 1
            && matches!(segment[0].content.trim(), "·" | "/");
        if separator {
            let next_width = segments.get(index + 1).map_or(0, |next|
                next.iter().map(width_of).sum::<usize>());
            if used == 0 || next_width == 0
                || used + segment_width + next_width > available {
                // A separator belongs between complete values, never at the
                // end of one row or the beginning of the next.
                continue;
            }
        }
        if used > 0 && used + segment_width > available {
            lines.push(row);
            row = vec![Span::styled(continuation_prefix.clone(), theme.muted())];
            used = 0;
        }
        if segment_width <= available {
            for span in segment {
                let text = if used == 0 {
                    let text = span.content.trim_start();
                    text.strip_prefix("· ").or_else(|| text.strip_prefix("/ "))
                        .unwrap_or(text)
                } else { span.content.as_ref() };
                used += super::display_width::width(text);
                row.push(Span::styled(text.to_owned(), span.style));
            }
            continue;
        }
        // A single long styled status/description may contain several words.
        // Reflow at whitespace rather than permanently truncating its tail.
        // Only a word wider than the complete value row needs ellipsis.
        for span in segment {
            let mut rest = span.content.as_ref().to_owned();
            loop {
                if used == 0 {
                    let text = rest.trim_start();
                    rest = text.strip_prefix("· ").or_else(|| text.strip_prefix("/ "))
                        .unwrap_or(text).to_owned();
                }
                if rest.is_empty() { break; }
                let remaining = available.saturating_sub(used);
                let width = super::display_width::width(&rest);
                if width <= remaining {
                    row.push(Span::styled(rest, span.style));
                    used += width;
                    break;
                }
                let mut columns = 0usize;
                let mut break_at = None;
                for (byte, ch) in rest.char_indices() {
                    if byte > 0 && ch.is_whitespace() && columns <= remaining {
                        break_at = Some(byte);
                    }
                    columns += super::display_width::char_width(ch);
                    if columns > remaining { break; }
                }
                let break_at = break_at.filter(|byte| !rest[*byte..].trim().is_empty()
                    && super::display_width::width(rest[..*byte].trim_end()) <= remaining);
                if let Some(byte) = break_at {
                    let head = rest[..byte].trim_end();
                    if !head.is_empty() {
                        row.push(Span::styled(head.to_owned(), span.style));
                    }
                    lines.push(row);
                    row = vec![Span::styled(continuation_prefix.clone(), theme.muted())];
                    used = 0;
                    rest = rest[byte..].trim_start().to_owned();
                } else if used > 0 {
                    lines.push(row);
                    row = vec![Span::styled(continuation_prefix.clone(), theme.muted())];
                    used = 0;
                } else {
                    // The first word itself exceeds a complete value row.
                    // Abbreviate that word only; later words still get rows.
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    let head = truncate_to(&rest[..end], available);
                    used = super::display_width::width(&head);
                    row.push(Span::styled(head, span.style));
                    if rest[end..].trim().is_empty() { break; }
                    rest = rest[end..].trim_start().to_owned();
                    lines.push(row);
                    row = vec![Span::styled(continuation_prefix.clone(), theme.muted())];
                    used = 0;
                }
            }
        }
    }
    lines.push(row);
}

fn media_duration(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds <= 0.0 { return None; }
    let total = seconds.round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    Some(if hours > 0 { format!("{hours}h {minutes:02}m") }
        else if minutes > 0 { format!("{minutes}m {secs:02}s") }
        else { format!("{secs}s") })
}

fn media_duration_append(values: &mut Vec<Span<'static>>, seconds: Option<f64>,
    theme: super::theme::Theme) {
    if let Some(value) = seconds.and_then(media_duration) {
        values.push(media_secondary(" · ", theme));
        values.push(media_accent(value, theme));
    }
}

fn media_edition(name: &str, title: &str) -> Option<String> {
    // A suffix explicitly carried by the folder/ISO name is usable; a catalog
    // number or an inferred pressing is not evidence of an edition.
    let rest = name.get(title.len()..)?;
    if !name.get(..title.len())?.eq_ignore_ascii_case(title) { return None; }
    let suffix = rest.trim();
    let enclosed = suffix.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
        .or_else(|| suffix.strip_prefix('(').and_then(|v| v.strip_suffix(')')));
    enclosed.map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
}

fn media_header_descriptor(
    entry: &BrowseEntry,
    browse: &BrowseState,
    contents: Option<&crate::disc::DiscContents>,
) -> String {
    let classification = if entry.is_child_dir() && !browse.is_in_archive() {
        browse.current_folder_classification()
    } else { None };
    if entry.is_video_transport_file() {
        return if entry.path.extension().and_then(|v| v.to_str())
            .is_some_and(|v| v.eq_ignore_ascii_case("vob")) {
                "DVD-Video · VOB".to_string()
            } else { "Blu-ray · M2TS".to_string() };
    }
    if let Some(classification) = classification {
        if matches!(classification.kind, FolderClassificationKind::Album) {
            let audio = &classification.audio;
            return match (audio.dominant_format_label(), audio.is_mixed_format()) {
                (Some(label), false) => label.to_string(),
                (Some(label), true) => format!("{label} + mixed formats"),
                _ => "audio files".to_string(),
            };
        }
    }
    let disc_kind = contents.map(|v| v.format.name())
        .or_else(|| classification.and_then(|c| c.disc_marker.map(|m| m.label())))
        .unwrap_or_else(|| match entry.kind {
            EntryKind::SacdIso => "SACD",
            EntryKind::DvdAudioIso | EntryKind::DvdAudioDir => "DVD-Audio",
            EntryKind::DvdVideoIso | EntryKind::DvdVideoDir => "DVD-Video",
            EntryKind::BlurayIso | EntryKind::BlurayDir => "Blu-ray",
            _ => "disc",
        });
    let mut description = disc_kind.to_string();
    if disc_kind == "Blu-ray" {
        if let Some(p) = contents.and_then(|v| v.presentations.iter()
            .filter(|p| p.format.codec.as_deref().is_some_and(|s|
                s.eq_ignore_ascii_case("LPCM") || s.eq_ignore_ascii_case("PCM")))
            .max_by_key(|p| p.format.sample_rate.unwrap_or(0))) {
            let format = &p.format;
            if let (Some(bits), Some(rate)) = (format.bit_depth, format.sample_rate) {
                if rate % 1000 == 0 {
                    description.push_str(&format!(" {bits}-{} {}", rate / 1000,
                        format.codec.as_deref().unwrap_or("LPCM")));
                }
            }
        }
    }
    let is_iso = matches!(entry.kind, EntryKind::SacdIso | EntryKind::DvdAudioIso
        | EntryKind::DvdVideoIso | EntryKind::BlurayIso)
        || classification.is_some_and(|c| c.units.iter().any(|u|
            u.path.extension().and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("iso"))));
    if is_iso && !disc_kind.eq_ignore_ascii_case("ISO") { description.push_str(" · ISO"); }
    description
}

fn push_media_reference_header(
    lines: &mut Vec<Vec<Span<'static>>>,
    header: &BrowseMediaHeader,
    descriptor: &str,
    entry_name: &str,
    content_width: usize,
    theme: super::theme::Theme,
) {
    let max = content_width.saturating_sub(MEDIA_INDENT.len());
    lines.push(vec![Span::raw(MEDIA_INDENT),
        Span::styled(truncate_to(&header.title, max), theme.bold(theme.text_bright))]);
    if header.artist.is_some() || header.year.is_some() {
        let mut values = vec![Span::raw(MEDIA_INDENT)];
        if let Some(artist) = &header.artist {
            values.push(media_accent(artist, theme));
        }
        if let Some(year) = &header.year {
            if header.artist.is_some() { values.push(media_secondary(" · ", theme)); }
            values.push(media_secondary(year, theme));
        }
        // A long artist name must not paint through the right border.
        let mut left = max;
        let mut bounded = vec![Span::raw(MEDIA_INDENT)];
        for span in values.into_iter().skip(1) {
            let text = truncate_to(span.content.as_ref(), left);
            left = left.saturating_sub(super::display_width::width(&text));
            bounded.push(Span::styled(text, span.style));
        }
        lines.push(bounded);
    } else {
        lines.push(Vec::new());
    }
    let edition = media_edition(entry_name, &header.title);
    if edition.is_some() || !descriptor.is_empty() {
        let description = if let Some(edition) = edition {
            // The edition may already name the medium ("Japan SHM SACD").
            // Do not repeat it as "SACD · SACD · ISO".
            let medium = descriptor.split(" · ").next().unwrap_or(descriptor);
            let suffix = if edition.to_ascii_lowercase().ends_with(&medium.to_ascii_lowercase()) {
                descriptor.strip_prefix(medium).unwrap_or(descriptor)
                    .trim_start_matches(" · ")
            } else { descriptor };
            if suffix.is_empty() { edition } else { format!("{edition} · {suffix}") }
        } else { descriptor.to_string() };
        lines.push(vec![Span::raw(MEDIA_INDENT),
            media_secondary(truncate_to(&description, max), theme)]);
    } else {
        lines.push(Vec::new());
    }
    lines.push(Vec::new());
}

fn push_media_album_audio(
    lines: &mut Vec<Vec<Span<'static>>>,
    audio: &FolderAudioSummary,
    browse: &BrowseState,
    content_width: usize,
    label: &'static str,
    theme: super::theme::Theme,
) {
    let rollup = browse.folder_probe_rollup(audio);
    let noun = if label == "Audio" { "track" } else { "file" };
    let mut values = media_count(audio.track_count, noun,
        if label == "Audio" { "tracks" } else { "files" }, theme);
    if rollup.probed_count == audio.track_count {
        media_duration_append(&mut values, Some(rollup.total_duration_secs), theme);
    }
    push_media_row(lines, label, values, content_width, theme);
    if let Some(format) = audio.dominant_format_label() {
        let mut values = vec![media_accent(format.to_string(), theme)];
        if audio.is_mixed_format() { values.push(media_secondary(" · mixed formats", theme)); }
        if rollup.has_mixed_profiles() {
            values.push(media_secondary(" · mixed audio profiles", theme));
        } else if let Some(profile) = rollup.dominant_profile_label() {
            let profile = profile.split('/').collect::<Vec<_>>();
            if let Some(bits) = profile.first().filter(|s| !s.is_empty()) {
                values.push(media_secondary(" · ", theme));
                values.push(media_accent((*bits).to_string(), theme));
            }
            if let Some(rate) = profile.get(1).filter(|s| !s.is_empty()) {
                values.push(media_secondary(" / ", theme));
                values.push(media_accent((*rate).to_string(), theme));
            }
            if let Some(channels) = profile.get(2).filter(|s| !s.is_empty()) {
                values.push(media_secondary(" · ", theme));
                values.push(media_accent((*channels).to_string(), theme));
            }
        }
        if rollup.probed_count > 0 && rollup.probed_count < audio.track_count {
            values.push(media_secondary(format!(" ({} of {} probed)",
                rollup.probed_count, audio.track_count), theme));
        }
        push_media_row(lines, "Format", values, content_width, theme);
    }
}

fn media_disc_is_stereo(p: &crate::disc::DiscPresentation) -> bool {
    if let Some(layout) = &p.format.channel_layout {
        let layout = layout.to_ascii_lowercase();
        if layout.contains("stereo") || layout == "2.0" || layout == "2ch" { return true; }
    }
    matches!(p.format.channels, Some(2))
        || matches!(p.id, crate::disc::PresentationId::SacdArea(
            crate::disc::SacdAreaId::Stereo))
}

fn media_disc_stream_name(p: &crate::disc::DiscPresentation) -> String {
    let codec = p.format.codec.as_deref().unwrap_or(&p.label);
    let fmt = &p.format;
    let rate = fmt.sample_rate.filter(|r| *r > 0).map(|r| {
        if r % 1000 == 0 { (r / 1000).to_string() }
        else { format!("{:.1}", r as f64 / 1000.0) }
    });
    let quality = match (fmt.bit_depth, rate) {
        (Some(bits), Some(rate)) => format!(" {bits}/{rate}"),
        (None, Some(rate)) => format!(" {rate} kHz"),
        (Some(bits), None) => format!(" {bits}-bit"),
        _ => String::new(),
    };
    let layout = fmt.channel_layout.clone()
        .or_else(|| fmt.channels.map(|n| if n == 2 { "stereo".into() }
            else if n > 2 { "multichannel".into() } else { "mono".into() }));
    format!("{codec}{quality}{}", layout.map(|s| format!(" {s}")).unwrap_or_default())
}

/// Mirrors the reference's Audio/Mixes/Streams, or Audio/Format/Areas for
/// SACD. All values are derived from an already cached disc probe.
fn push_media_disc_audio(
    lines: &mut Vec<Vec<Span<'static>>>,
    contents: &crate::disc::DiscContents,
    content_width: usize,
    theme: super::theme::Theme,
) -> bool {
    use crate::disc::{DiscFormat, PresentationId, SacdAreaId};
    let streams = contents.presentations.len();
    let sacd = contents.format == DiscFormat::Sacd;
    let tracks = if sacd {
        // The stereo and multichannel areas may contain the same titles.
        contents.presentations.iter().map(|p| p.tracks.len()).max().unwrap_or(0)
    } else {
        contents.presentations.iter().map(|p| p.tracks.len()).sum()
    };
    let mut values = media_count(tracks, "track", "tracks", theme);
    if sacd {
        let duration = contents.presentations.iter()
            .map(|p| p.total_duration_secs).filter(|s| s.is_finite() && *s > 0.0)
            .fold(0.0f64, f64::max);
        media_duration_append(&mut values, Some(duration), theme);
    } else {
        values.push(media_secondary(" · ", theme));
        values.extend(media_count(streams, "stream", "streams", theme));
    }
    push_media_row(lines, "Audio", values, content_width, theme);
    if sacd {
        // Stereo is the reference's principal SACD format when both areas
        // are present; probe order must not arbitrarily select multichannel.
        if let Some(p) = contents.presentations.iter()
            .find(|p| matches!(p.id, PresentationId::SacdArea(SacdAreaId::Stereo)))
            .or_else(|| contents.presentations.first()) {
            let rate = p.format.sample_rate.filter(|v| *v > 0);
            let dsd = rate.and_then(|rate| (rate % 44_100 == 0).then_some(rate / 44_100))
                .map(|multiplier| format!("DSD{multiplier}"))
                .or_else(|| p.format.codec.clone()).unwrap_or_else(|| "DSD".into());
            let mut format = vec![media_accent(dsd, theme)];
            if let Some(rate) = rate {
                format.push(media_secondary(" · ", theme));
                format.push(media_accent(format!("{:.1} MHz", rate as f64 / 1_000_000.0), theme));
            }
            let layout = if media_disc_is_stereo(p) { "stereo" } else { "multichannel" };
            format.push(media_secondary(" · ", theme));
            format.push(media_accent(layout.to_string(), theme));
            push_media_row(lines, "Format", format, content_width, theme);
        }
        let stereo = contents.presentations.iter().any(|p|
            matches!(p.id, PresentationId::SacdArea(SacdAreaId::Stereo)));
        let multi = contents.presentations.iter().any(|p|
            matches!(p.id, PresentationId::SacdArea(SacdAreaId::MultiChannel)));
        push_media_row(lines, "Areas", vec![
            media_accent(format!("stereo {}", if stereo { "✓" } else { "—" }), theme),
            media_secondary(" · ", theme),
            media_secondary(format!("multichannel {}", if multi { "✓" } else { "—" }), theme),
        ], content_width, theme);
    } else {
        let stereo = contents.presentations.iter().filter(|p| media_disc_is_stereo(p)).count();
        let multi = streams.saturating_sub(stereo);
        let mut mixes = Vec::new();
        if stereo > 0 { mixes.extend(media_count(stereo, "stereo", "stereo", theme)); }
        if multi > 0 {
            if !mixes.is_empty() { mixes.push(media_secondary(" · ", theme)); }
            mixes.extend(media_count(multi, "multichannel", "multichannel", theme));
        }
        if !mixes.is_empty() { push_media_row(lines, "Mixes", mixes, content_width, theme); }
        let mut groups: Vec<(String, usize, bool, u32)> = Vec::new();
        for presentation in &contents.presentations {
            let description = media_disc_stream_name(presentation);
            if let Some((_, count, _, _)) = groups.iter_mut().find(|(name, _, _, _)| *name == description) {
                *count += 1;
            } else {
                let linear_pcm = presentation.format.codec.as_deref().is_some_and(|codec|
                    codec.eq_ignore_ascii_case("LPCM") || codec.eq_ignore_ascii_case("PCM"));
                groups.push((description, 1, linear_pcm,
                    presentation.format.sample_rate.unwrap_or(0)));
            }
        }
        // Show lossless LPCM and higher rates first, retaining a stable
        // secondary ordering. The Streams view still provides every mix.
        groups.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.0.cmp(&b.0)));
        let mut shown = 0;
        for (index, (description, count, _, _)) in groups.iter().take(2).enumerate() {
            let value = vec![media_secondary(format!("{count}× "), theme),
                media_accent(description.clone(), theme)];
            push_media_row(lines, if index == 0 { "Streams" } else { "" }, value,
                content_width, theme);
            shown += count;
        }
        if streams > shown {
            push_media_row(lines, "", vec![media_secondary(format!("+ {} more", streams - shown), theme)],
                content_width, theme);
        }
    }
    let protection = contents.copy_protection.description.trim();
    if !protection.is_empty() && (!sacd || !protection.eq_ignore_ascii_case("none")) {
        let description = if protection.eq_ignore_ascii_case("none") {
            "unencrypted".to_string()
        } else { protection.to_string() };
        push_media_row(lines, "Protection", vec![media_accent(description, theme)],
            content_width, theme);
    }
    streams > 0
}

fn push_media_contents_and_size(
    lines: &mut Vec<Vec<Span<'static>>>,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    path: &Path,
    classification: Option<&FolderContentClassification>,
    direct_file_size: Option<u64>,
    entry_date_label: &str,
    content_width: usize,
    theme: super::theme::Theme,
) -> bool {
    use crate::directory_accounting::{format_bytes, Freshness};
    let view = accounting.view(path);
    let running = direct_file_size.is_none()
        && (view.is_some_and(|v| v.running) || accounting.scan_pending(path));
    let progress = view.and_then(|v| v.progress.as_ref());
    let complete = view.and_then(|v| v.cached.as_ref())
        .filter(|v| direct_file_size.is_none() && v.freshness != Freshness::KnownInvalid)
        .map(|v| v.measurement.counts);
    let listing = (path == browse.current_dir.as_path())
        .then(|| browse.current_directory_listing_counts()).flatten();
    let local = view.and_then(|v| v.local);
    let folders = if direct_file_size.is_some() { None } else {
        local.map(|c| c.folders)
            .or_else(|| listing.map(|c| c.folders as u128))
    };
    let iso_units = classification.map_or(0, |classification|
        classification.units.iter().filter(|u|
            u.path.extension().and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("iso"))).count());
    let disc_image_count = if direct_file_size.is_some() { 1 }
        else if classification.is_some_and(|c| c.kind == FolderClassificationKind::Disc
            && c.units.is_empty()) { 0 }
        else { iso_units };
    let mut contents = Vec::new();
    // A one-level count can describe immediate contents even when no
    // complete recursive size measurement has finished. The Size row
    // still reports unknown until a complete result is available.
    let count = direct_file_size.map(|_| 1u128)
        .or_else(|| complete.map(|c| c.files))
        .or_else(|| local.map(|c| c.files))
        .or_else(|| listing.map(|c| c.files as u128));
    if let Some(count) = count {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        // When the only file is an ISO, put 'disc image' first, as in the
        // Blu-ray reference. Otherwise retain the file count and add the ISO
        // subset, as in the SACD reference. Never add subsets to totals.
        if !(disc_image_count > 0 && count == disc_image_count) {
            contents.extend(media_count(count, "file", "files", theme));
        }
    } else if running {
        contents.push(media_secondary("scanning…", theme));
    } else {
        contents.push(media_secondary("unknown", theme));
    }
    if disc_image_count > 0 {
        if !contents.is_empty() && count != Some(disc_image_count as u128) {
            contents.push(media_secondary(" · ", theme));
        } else if count == Some(disc_image_count as u128) { contents.clear(); }
        contents.extend(media_count(disc_image_count, "disc image", "disc images", theme));
    }
    if let Some(folders) = folders {
        if folders > 0 {
            if !contents.is_empty() { contents.push(media_secondary(" · ", theme)); }
            let count = usize::try_from(folders).unwrap_or(usize::MAX);
            contents.extend(media_count(count, "folder", "folders", theme));
        }
    }
    push_media_row(lines, "Contents", contents, content_width, theme);
    if classification.is_some_and(|c| c.kind == FolderClassificationKind::Album)
        && complete.is_some_and(|counts| counts.audio > 0) {
        let counts = complete.expect("checked above");
        let files = counts.files;
        let audio = counts.audio.min(files);
        let other = files.saturating_sub(audio);
        let mut breakdown = vec![media_accent(audio.to_string(), theme), media_secondary(" audio", theme)];
        breakdown.push(media_secondary(" · ", theme));
        breakdown.push(media_accent(other.to_string(), theme));
        breakdown.push(media_secondary(" other", theme));
        push_media_row(lines, "", breakdown, content_width, theme);
    }
    let value = if let Some(bytes) = direct_file_size {
        vec![media_accent(format_bytes(bytes as u128), theme)]
    } else if running {
        let current = format_bytes(progress.map_or(0, |p| p.bytes_examined));
        vec![media_accent(format!("{} {current}…", directory_spinner()), theme)]
    } else if let Some(cached) = complete {
        vec![media_accent(format_bytes(cached.bytes), theme)]
    } else {
        vec![media_secondary("unknown", theme)]
    };
    push_media_row(lines, "Size", value, content_width, theme);
    let modified = if entry_date_label.is_empty() {
        vec![media_secondary("unknown", theme)]
    } else {
        vec![media_accent(entry_date_label.to_string(), theme)]
    };
    push_media_row(lines, "Modified", modified, content_width, theme);
    running
}

fn push_media_actions(
    lines: &mut Vec<Vec<Span<'static>>>,
    content_width: usize,
    running: bool,
    show_refresh: bool,
    show_streams: bool,
    refresh_hovered: bool,
    streams_hovered: bool,
    refresh_pill_row: &mut Option<usize>,
    streams_pill_row: &mut Option<usize>,
    theme: super::theme::Theme,
) {
    if !show_refresh && !show_streams { return; }
    lines.push(Vec::new());
    let row_index = lines.len();
    let mut row = vec![Span::raw(MEDIA_INDENT)];
    let mut remaining = content_width.saturating_sub(MEDIA_INDENT.len());
    if show_refresh {
        let label = if running { " Cancel " } else { " Refresh " };
        let width = super::display_width::width(label);
        if remaining >= width {
            let bg = if refresh_hovered { theme.blue } else { theme.purple };
            row.push(Span::styled(label, Style::default()
                .fg(theme.pill_active_fg).bg(bg).add_modifier(Modifier::BOLD)));
            remaining -= width;
            *refresh_pill_row = Some(row_index);
        }
    }
    if show_streams {
        let label = " Streams… ";
        let width = super::display_width::width(label);
        let gap = if refresh_pill_row.is_some() { 2 } else { 0 };
        if remaining >= gap + width {
            if gap > 0 { row.push(Span::raw("  ")); }
            let bg = if streams_hovered { theme.blue } else { theme.purple };
            row.push(Span::styled(label, Style::default()
                .fg(theme.pill_active_fg).bg(bg).add_modifier(Modifier::BOLD)));
            *streams_pill_row = Some(row_index);
        }
    }
    lines.push(row);
}

fn push_media_folder_layout(
    lines: &mut Vec<Vec<Span<'static>>>,
    classification: &FolderContentClassification,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    entry_path: &Path,
    entry_date_label: &str,
    content_width: usize,
    cached_disc_contents: Option<&crate::disc::DiscContents>,
    audio_streams_hovered: bool,
    audio_streams_pill_row: &mut Option<usize>,
    refresh_hovered: bool,
    refresh_pill_row: &mut Option<usize>,
    is_symlink: bool,
    theme: super::theme::Theme,
) {
    let mut has_streams = false;
    match classification.kind {
        FolderClassificationKind::Album => push_media_album_audio(lines, &classification.audio,
            browse, content_width, "Audio", theme),
        FolderClassificationKind::Disc => {
            let path = classification.disc_probe_source_path(entry_path);
            if let Some(contents) = cached_disc_contents {
                has_streams = push_media_disc_audio(lines, contents, content_width, theme);
            } else {
                let status = browse.disc_probe_cache.get(path)
                    .and_then(|cache| cache.error_if_current(path))
                    .map(str::to_owned)
                    .unwrap_or_else(|| if browse.disc_probe_pending.contains(path) {
                        "Analyzing disc…".into()
                    } else { "disc metadata not cached".into() });
                push_media_row(lines, "Audio", vec![media_secondary(status, theme)],
                    content_width, theme);
            }
        }
        FolderClassificationKind::MultiDisc => {
            let has_iso = classification.units.iter().any(|u| u.disc_marker.is_some());
            if classification.audio.track_count > 0 {
                push_media_album_audio(lines, &classification.audio, browse, content_width,
                    if has_iso { "Audio files" } else { "Audio" }, theme);
            }
            push_media_row(lines, "Discs", media_count(classification.unit_count,
                "disc", "discs", theme), content_width, theme);
            // Filesystem enumeration order is unspecified. Sort the small
            // presentation list, not the cached classification or the scanner.
            let mut units = classification.units.iter().collect::<Vec<_>>();
            units.sort_by_cached_key(|unit| (unit.name.to_lowercase(), unit.path.clone()));
            for unit in units.iter().take(6) {
                // A direct child ISO should be shown by its real filename;
                // an ISO inside "Disc 1/" retains that directory's unit name.
                let display_name = if unit.path.parent() == Some(entry_path)
                    && unit.path.extension().and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("iso")) {
                    unit.path.file_name().and_then(|name| name.to_str())
                        .unwrap_or(&unit.name)
                } else { &unit.name };
                let description = if let Some(marker) = unit.disc_marker {
                    format!("{display_name} · {}", marker.label())
                } else {
                    format!("{display_name} · {} {}", unit.audio.track_count,
                        if unit.audio.track_count == 1 { "track" } else { "tracks" })
                };
                push_media_row(lines, "", vec![media_secondary(description, theme)], content_width, theme);
            }
            if classification.units.len() > 6 {
                push_media_row(lines, "", vec![media_secondary(
                    format!("+ {} more discs", classification.units.len() - 6), theme)],
                    content_width, theme);
            }
        }
        _ => return,
    }
    let running = push_media_contents_and_size(lines, browse, accounting, entry_path,
        Some(classification), None, entry_date_label, content_width, theme);
    push_media_actions(lines, content_width, running, !is_symlink, has_streams,
        refresh_hovered, audio_streams_hovered, refresh_pill_row,
        audio_streams_pill_row, theme);
}

/// Directly selected ISO files and disc-source directories use the same
/// presentation as a directory classified as containing an ISO. This draws
/// from already cached probe/accounting data; it starts no new work.
fn push_direct_disc_layout(
    lines: &mut Vec<Vec<Span<'static>>>,
    entry: &BrowseEntry,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    entry_date_label: &str,
    content_width: usize,
    cached_disc_contents: Option<&crate::disc::DiscContents>,
    audio_streams_hovered: bool,
    audio_streams_pill_row: &mut Option<usize>,
    refresh_hovered: bool,
    refresh_pill_row: &mut Option<usize>,
    theme: super::theme::Theme,
) {
    let source = entry.path.as_path();
    let has_streams = if let Some(contents) = cached_disc_contents {
        push_media_disc_audio(lines, contents, content_width, theme)
    } else {
        let status = browse.disc_probe_cache.get(source)
            .and_then(|cache| cache.error_if_current(source))
            .map(str::to_owned)
            .unwrap_or_else(|| if browse.disc_probe_pending.contains(source) {
                "Analyzing disc…".to_owned()
            } else if matches!(browse.directory_summary_cold_work_policy,
                BrowseDirectorySummaryColdWorkPolicy::AfterDescendOnly) {
                "disc summary available after descend/scan".to_owned()
            } else { "disc metadata not cached".to_owned() });
        push_media_row(lines, "Audio", vec![media_secondary(status, theme)], content_width, theme);
        false
    };
    let physical_dir = entry.is_child_dir();
    let running = push_media_contents_and_size(lines, browse, accounting, source,
        None, (!physical_dir).then_some(entry.size), entry_date_label, content_width, theme);
    push_media_actions(lines, content_width, running, physical_dir && !entry.is_symlink,
        has_streams, refresh_hovered, audio_streams_hovered, refresh_pill_row,
        audio_streams_pill_row, theme);
}

/// Build content lines for the info pane based on the entry kind.
/// `content_width` is the width available inside the pane borders.
/// Returns `InfoContent` with both the visual lines and a mapping of
/// clickable metadata field positions.
fn entry_info_lines(
    entry: &BrowseEntry,
    browse: &BrowseState,
    accounting: &crate::directory_accounting::AccountingService,
    content_width: usize,
    analyze_hovered: bool,
    edit_tags_hovered: bool,
    audio_streams_hovered: bool,
    refresh_hovered: bool,
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
    let mut refresh_pill_row: Option<usize> = None;
    let refresh_focused = info_focus == Some(BrowseInfoFocus::Refresh);

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

    let entry_date_label = if matches!(entry.kind, EntryKind::ParentDir) && !browse.is_in_archive() {
        browse.current_directory_modified()
            .map(|modified| {
                let dt: chrono::DateTime<chrono::Local> = modified.into();
                dt.format("%Y-%m-%d").to_string()
            })
            .unwrap_or_default()
    } else {
        entry.date_label()
    };

    // All selections start at the same cell with the same header height.
    // Classification and probe arrival may enrich text but cannot move the
    // body down or shift the title horizontally.
    let disc_source_path = if entry.is_disc_source() {
        Some(entry.path.as_path())
    } else if matches!(entry.kind, EntryKind::Directory) && !browse.is_in_archive() {
        browse.current_folder_classification().filter(|c|
            c.kind == FolderClassificationKind::Disc
        ).map(|c| c.disc_probe_source_path(&entry.path))
    } else { None };
    let cached_disc_contents = disc_source_path.and_then(|path|
        browse.disc_probe_cache.get(path)
            .and_then(|cache| cache.contents_if_current(path))
    );
    let display_name = if matches!(entry.kind, EntryKind::ParentDir) && !browse.is_in_archive() {
        browse.current_dir.file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| browse.current_dir.display().to_string())
    } else if matches!(entry.kind, EntryKind::Archive)
        && entry.path.extension().and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("iso")) {
        // ISOs start out as Archive until the cheap kind probe completes.
        // Use the same visible title before and after the promotion.
        entry.path.file_stem().and_then(|s| s.to_str())
            .unwrap_or(&entry.name).to_owned()
    } else { entry.name.clone() };
    let header = media_header_for_entry(entry, browse, cached_disc_contents.as_deref())
        .unwrap_or_else(|| BrowseMediaHeader {
            title: display_name.clone(), artist: None, year: None, catalog: None,
        });
    let descriptor = if entry.is_disc_source() || entry.is_video_transport_file()
        || (entry.is_child_dir() && !browse.is_in_archive()
            && browse.current_folder_classification().is_some_and(|c|
                matches!(c.kind, FolderClassificationKind::Album
                    | FolderClassificationKind::Disc | FolderClassificationKind::MultiDisc))) {
        media_header_descriptor(entry, browse, cached_disc_contents.as_deref())
    } else { String::new() };
    let edition_name = if entry.is_child_dir() { entry.name.as_str() }
        else { entry.path.file_stem().and_then(|s| s.to_str()).unwrap_or(&entry.name) };
    push_media_reference_header(&mut lines, &header, &descriptor, edition_name,
        content_width, theme);

    match &entry.kind {
        EntryKind::ParentDir => {
            if browse.is_in_archive() {
                lines.push(vec![Span::styled("   parent directory", theme.muted())]);
            } else {
                push_directory_detail_line(&mut lines, "Kind", "directory", theme.text_style(),
                    content_width, theme);
                refresh_pill_row = push_directory_stats_lines(
                    &mut lines, browse, accounting, &browse.current_dir, &entry_date_label,
                    content_width, refresh_hovered, refresh_focused, theme,
                );
            }
        }
        EntryKind::Directory if browse.is_in_archive() => {
            // Archive-member paths are logical, not filesystem directories.
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled("directory", theme.text_style()),
            ]);
        }
        EntryKind::Directory => {
            if let Some(classification) = browse.current_folder_classification() {
                push_folder_classification_lines(
                    &mut lines,
                    classification.as_ref(),
                    browse,
                    accounting,
                    &entry.path,
                    entry.size,
                    &entry_date_label,
                    content_width,
                    cached_disc_contents.as_deref(),
                    audio_streams_hovered,
                    &mut audio_streams_pill_row,
                    refresh_hovered,
                    refresh_focused,
                    &mut refresh_pill_row,
                    entry.is_symlink,
                    theme,
                );
            } else {
                push_directory_detail_line(&mut lines, "Kind", "directory", theme.text_style(),
                    content_width, theme);
                if !entry.is_symlink {
                    refresh_pill_row = push_directory_stats_lines(
                        &mut lines, browse, accounting, &entry.path, &entry_date_label,
                        content_width, refresh_hovered, refresh_focused, theme,
                    );
                }
                if browse.folder_classification_pending_for(&entry.path) {
                    push_directory_detail_line(&mut lines, "Content", "classifying...",
                        Style::default().fg(theme.text_dim), content_width, theme);
                }
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
                    title: Some(header.title.clone()),
                    meta_field_rows,
                    refresh_pill_row: None,
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
                title: Some(header.title.clone()),
                meta_field_rows,
                refresh_pill_row: None,
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
            push_direct_disc_layout(&mut lines, entry, browse, accounting,
                &entry_date_label, content_width, cached_disc_contents.as_deref(),
                audio_streams_hovered, &mut audio_streams_pill_row,
                refresh_hovered, &mut refresh_pill_row, theme);
        }
        EntryKind::Archive => {
            if entry.path.extension().and_then(|v| v.to_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("iso")) {
                // The cheap kind probe may not yet have promoted this ISO to a
                // native disc source. Hold its header and body position until
                // the effective kind arrives; do not invent stream metadata.
                push_media_row(&mut lines, "Audio", vec![media_secondary(
                    "disc format not identified", theme)], content_width, theme);
                push_media_contents_and_size(&mut lines, browse, accounting, &entry.path,
                    None, Some(entry.size), &entry_date_label, content_width, theme);
            } else {
                lines.push(vec![
                    Span::styled("   kind    ", theme.muted()),
                    Span::styled("archive (7z)", theme.text_style()),
                ]);
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(size_str(entry.size), theme.text_style()),
                ]);
            }
        }
        EntryKind::SacdIso => {
            push_direct_disc_layout(&mut lines, entry, browse, accounting,
                &entry_date_label, content_width, cached_disc_contents.as_deref(),
                audio_streams_hovered, &mut audio_streams_pill_row,
                refresh_hovered, &mut refresh_pill_row, theme);
            // Keep the SACD metadata editor even before the disc probe arrives.
            // The primary album/artist/year already live in the shared header.
            if let Some(cached) = browse.current_cached_info() {
                if let Some(genre) = &cached.metadata.genre {
                    push_media_row(&mut lines, "Genre", vec![media_accent(genre.clone(), theme)],
                        content_width, theme);
                }
                if let Some(catalog) = &cached.metadata.catalog_number {
                    push_media_row(&mut lines, "Catalog", vec![media_accent(catalog.clone(), theme)],
                        content_width, theme);
                }
            }
            lines.push(Vec::new());
            let row = lines.len();
            let bg = if edit_tags_hovered { theme.blue } else { theme.purple };
            lines.push(vec![Span::raw(MEDIA_INDENT), Span::styled(" edit tags ",
                Style::default().fg(theme.pill_active_fg).bg(bg)
                    .add_modifier(Modifier::BOLD))]);
            sacd_edit_tags_row = Some(row);
        }
        EntryKind::OtherFile => {
            if entry.is_video_transport_file() {
                // The header already describes DVD-Video/VOB or Blu-ray/M2TS.
                // Render the same column grid as the other media previews.
                if let Some(streams) = browse.current_cached_info()
                    .and_then(|cached| cached.transport.as_ref()) {
                    push_transport_stream_lines(&mut lines, streams, content_width, theme);
                } else {
                    let status = if browse.probe_pending.contains(&entry.path) {
                        "Inspecting streams…"
                    } else { "stream details not cached" };
                    push_media_field(&mut lines, "Status", status, content_width, theme);
                }
                push_media_row(&mut lines, "Size", vec![media_accent(
                    size_str(entry.size), theme)], content_width, theme);
                push_media_row(&mut lines, "Modified", vec![media_accent(
                    if entry_date_label.is_empty() { "unknown".into() }
                    else { entry_date_label.clone() }, theme)], content_width, theme);
            } else {
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
        title: Some(header.title),
        meta_field_rows,
        refresh_pill_row,
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
                compression_is_lossless: None,
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
            transport: None,
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
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
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
    fn media_fields_fit_minimum_info_pane_width_without_separators() {
        for width in [13usize, 14, 18, 22, 78] {
            let mut lines = Vec::new();
            push_media_field(&mut lines, "Probed time", "long unavailable value", width, theme());
            push_media_field(&mut lines, "Protection", "CPPM / CSS", width, theme());
            push_media_field(&mut lines, "Format", "FLAC", width, theme());
            for (line_no, row) in lines.iter().enumerate() {
                let painted_width = row.iter().map(|span|
                    crate::tui::display_width::width(span.content.as_ref())).sum::<usize>();
                assert!(painted_width <= width,
                    "width {width}, row {line_no} overran Info content: {painted_width}");
            }
        }
    }

    fn cache_flac_tag(
        browse: &mut BrowseState,
        path: PathBuf,
        album: Option<&str>,
        artist: Option<&str>,
        year: Option<&str>,
    ) {
        let metadata = crate::tui::probe::SourceMetadata {
            album: album.map(str::to_owned),
            artist: artist.map(str::to_owned),
            year: year.map(str::to_owned),
            ..Default::default()
        };
        browse.insert_probe_cache_for_test(path, Some(std::sync::Arc::new(CachedInfo {
            source: crate::tui::probe::SourceInfo {
                format_name: "FLAC".into(), codec: "FLAC".into(),
                bit_depth: Some(24), sample_format_is_float: Some(false),
                compression_is_lossless: Some(true), sample_rate: 96_000,
                channels: 2, channel_layout: "stereo".into(),
                duration_secs: 120.0, file_size: 0,
            },
            metadata,
            transport: None,
        })));
    }

    #[test]
    fn album_header_uses_consistent_cached_tags_and_falls_back_on_conflicts() {
        let entry = BrowseEntry::new(PathBuf::from("/music/folder"),
            "folder".into(), EntryKind::Directory, 0, None);
        let p1 = entry.path.join("01.flac");
        let p2 = entry.path.join("02.flac");
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let mut classification = FolderContentClassification {
            kind: FolderClassificationKind::Album,
            identity,
            audio: audio_summary(&[p1.clone(), p2.clone()], "FLAC"),
            units: Vec::new(), unit_count: 1, collection_many: false,
            io_budget_exhausted: false, disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: BTreeMap::new(),
        };
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity,
            classification.clone());
        cache_flac_tag(&mut browse, p1.clone(), Some("The Album"), Some("The Artist"), Some("1987"));
        cache_flac_tag(&mut browse, p2.clone(), Some("the album"), Some("THE ARTIST"), Some("1987"));

        let header = media_header_for_entry(&entry, &browse, None).expect("album header");
        assert_eq!(header.title, "The Album");
        assert_eq!(header.artist.as_deref(), Some("The Artist"));
        assert_eq!(header.year.as_deref(), Some("1987"));

        cache_flac_tag(&mut browse, p2, Some("Different Album"), Some("Another Artist"), None);
        let conflicting = media_header_for_entry(&entry, &browse, None).expect("album header");
        assert_eq!(conflicting.title, "folder", "do not invent a single album from disagreeing tags");
        assert!(conflicting.artist.is_none(), "a conflicting track artist is not an album artist");
        assert_eq!(conflicting.year.as_deref(), Some("1987"));

        classification.audio.file_paths.clear();
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity, classification);
        let unknown = media_header_for_entry(&entry, &browse, None).expect("album header");
        assert_eq!(unknown.title, "folder");
        assert!(unknown.artist.is_none());
        assert!(unknown.year.is_none());
    }

    fn transport_preview_info(
        name: &str,
        summary: crate::tui::probe::TransportStreamSummary,
    ) -> (String, InfoContent) {
        let path = PathBuf::from(format!("/tmp/{name}"));
        let entry = BrowseEntry::new(path.clone(), name.into(), EntryKind::OtherFile, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let mut info = CachedInfo::new(crate::tui::probe::SourceInfo {
            format_name: "M2TS".into(), codec: "DTS".into(), bit_depth: None,
            sample_format_is_float: None, compression_is_lossless: None,
            sample_rate: 48000, channels: 2, channel_layout: "stereo".into(),
            duration_secs: 123.0, file_size: 0,
        }, crate::tui::probe::SourceMetadata::default());
        info.transport = Some(summary);
        browse.insert_probe_cache_for_test(path, Some(std::sync::Arc::new(info)));
        let rendered = entry_info_lines(&entry, &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96, false, false, false, false, None, None, theme());
        (flatten(&rendered), rendered)
    }

    #[test]
    fn transport_previews_render_video_and_all_cached_audio_streams() {
        use crate::tui::probe::{TransportAudioStream, TransportStreamSummary, TransportVideoStream};
        let video = TransportVideoStream { codec: "H.264".into(), width: 1920, height: 1080 };
        let audio1 = TransportAudioStream { codec: "DTS".into(), sample_rate: 48000,
            channels: 6, channel_layout: "5.1".into() };
        let audio2 = TransportAudioStream { codec: "AC3".into(), sample_rate: 48000,
            channels: 2, channel_layout: "stereo".into() };

        // Blu-ray with a multichannel mix and stereo commentary.
        let (rendered, info) = transport_preview_info("movie.m2ts", TransportStreamSummary {
            video_count: 1, audio_count: 2, videos: vec![video.clone()],
            audios: vec![audio1, audio2.clone()], duration_secs: 123.0,
        });
        assert!(rendered.contains("Blu-ray · M2TS"), "{rendered}");
        assert!(rendered.contains("1 video stream") && rendered.contains("2 audio streams"), "{rendered}");
        assert!(rendered.contains("Video") && rendered.contains("H.264") && rendered.contains("1920×1080"), "{rendered}");
        assert!(rendered.contains("DTS · 5.1 · 48 kHz"), "{rendered}");
        assert!(rendered.contains("AC3 · stereo · 48 kHz"), "{rendered}");
        assert!(info.audio_streams_pill_row.is_none(), "transport preview is not a disc browser");

        // One audio stream, same bounded preview path.
        let (single, _) = transport_preview_info("clip.VOB", TransportStreamSummary {
            video_count: 1, audio_count: 1, videos: vec![video.clone()],
            audios: vec![audio2], duration_secs: 20.0,
        });
        assert!(single.contains("DVD-Video · VOB") && single.contains("1 audio stream"), "{single}");
        assert!(single.contains("AC3 · stereo · 48 kHz"), "{single}");
        assert!(!single.contains("no audio streams"));

        // Valid video-only menu VOB: completed probe is NOT 'not cached'.
        let (silent, _) = transport_preview_info("menu.vob", TransportStreamSummary {
            video_count: 1, audio_count: 0, videos: vec![video],
            audios: Vec::new(), duration_secs: 6.0,
        });
        assert!(silent.contains("1 video stream") && silent.contains("0 audio streams"), "{silent}");
        assert!(silent.contains("no audio streams"), "{silent}");
        assert!(!silent.contains("not cached") && !silent.contains("Probing audio"), "{silent}");

        let other = BrowseEntry::new(PathBuf::from("/tmp/readme.txt"),
            "readme.txt".into(), EntryKind::OtherFile, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![other.clone()];
        let plain = entry_info_lines(&other, &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96, false, false, false, false, None, None, theme());
        assert!(flatten(&plain).contains("file"));
        assert!(!flatten(&plain).contains("video stream"));
    }

    #[test]
    fn transport_preview_omits_missing_streams_and_bounds_lines_in_narrow_panes() {
        use crate::tui::probe::{TransportStreamSummary, TransportVideoStream};
        let summary = TransportStreamSummary {
            video_count: 9, audio_count: 0,
            videos: vec![TransportVideoStream { codec: "H.264".into(),
                width: 1920, height: 1080 }],
            audios: vec![], duration_secs: 1.0,
        };
        for width in [13usize, 18, 22, 78] {
            let mut lines = Vec::new();
            push_transport_stream_lines(&mut lines, &summary, width, theme());
            for line in &lines {
                let used = line.iter().map(|span|
                    crate::tui::display_width::width(span.content.as_ref())).sum::<usize>();
                assert!(used <= width, "width={width}, line={line:?}, used={used}");
            }
        }
        let (rendered, info) = transport_preview_info("many.m2ts", summary);
        assert!(rendered.contains("… and 8 more"), "{rendered}");
        assert!(info.audio_streams_pill_row.is_none());
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let (rendered, info) = render_classification(entry, classification);

        assert!(!rendered.contains("album folder"));
        assert!(rendered.contains("Audio") && rendered.contains("2 tracks"));
        assert!(rendered.contains("Format") && rendered.contains("FLAC"));
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let (rendered, _) = render_classification(entry, classification);

        assert!(!rendered.contains("multi-disc album"));
        assert!(rendered.contains("Discs") && rendered.contains("2"));
        assert!(rendered.contains("Audio") && rendered.contains("2 tracks"));
        assert!(rendered.contains("Format") && rendered.contains("FLAC"));
        assert!(!rendered.contains("probing..."));
        assert!(rendered.contains("Disc 1 · 1 track"));
        assert!(rendered.contains("Disc 2 · 1 track"));
    }

    #[test]
    fn numbered_iso_multidisc_renders_disc_names_without_zero_tracks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(root.join("Disc 1.iso"), b"").expect("disc 1 fixture");
        std::fs::write(root.join("Disc 2.iso"), b"").expect("disc 2 fixture");
        let entry = BrowseEntry::new(root.to_path_buf(), "Album".into(),
            EntryKind::Directory, 0, None);
        let mut classification = crate::tui::browse::classify_folder_content_blocking(
            root, ProbeCacheIdentity::from_entry(&entry));
        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.audio.track_count, 0);
        classification.units.reverse(); // Deliberately non-directory order.
        let (rendered, _) = render_classification(entry, classification);
        assert!(!rendered.contains("multi-disc album"), "{rendered}");
        assert!(rendered.contains("Discs") && rendered.contains("2"), "{rendered}");
        let first = rendered.find("Disc 1.iso · ISO").expect(&rendered);
        let second = rendered.find("Disc 2.iso · ISO").expect(&rendered);
        assert!(first < second, "ISO unit order: {rendered}");
        assert!(!rendered.contains("ISO · ISO"), "redundant ISO descriptor: {rendered}");
        assert!(!rendered.contains("Tracks") && !rendered.contains("0 tracks"), "{rendered}");
    }

    #[test]
    fn flac_and_iso_multidisc_labels_only_the_regular_audio_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::create_dir(root.join("Disc 1")).expect("disc 1 directory");
        std::fs::write(root.join("Disc 1").join("01.flac"), b"").expect("track fixture");
        std::fs::write(root.join("Disc 2.iso"), b"").expect("disc 2 image");
        let entry = BrowseEntry::new(root.to_path_buf(), "Album".into(),
            EntryKind::Directory, 0, None);
        let mut classification = crate::tui::browse::classify_folder_content_blocking(
            root, ProbeCacheIdentity::from_entry(&entry));
        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.audio.track_count, 1);
        classification.units.reverse(); // Mixed unit kinds must sort equally.
        let (rendered, _) = render_classification(entry, classification);
        assert!(rendered.contains("Audio files") && rendered.contains("1 file"), "{rendered}");
        let first = rendered.find("Disc 1 · 1 track").expect(&rendered);
        let second = rendered.find("Disc 2.iso · ISO").expect(&rendered);
        assert!(first < second, "mixed unit order: {rendered}");
        assert!(rendered.contains("Discs") && rendered.contains("2"), "{rendered}");
        assert!(!rendered.contains("Tracks") && !rendered.contains("1 tracks"), "{rendered}");
    }

    #[test]
    fn nested_iso_multidisc_uses_the_disc_directory_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        for n in 1..=2 {
            let folder = temp.path().join(format!("Disc {n}"));
            std::fs::create_dir(&folder).expect("disc directory");
            std::fs::write(folder.join("media.iso"), b"").expect("disc image");
        }
        let entry = BrowseEntry::new(temp.path().to_path_buf(), "Album".into(),
            EntryKind::Directory, 0, None);
        let classification = crate::tui::browse::classify_folder_content_blocking(
            temp.path(), ProbeCacheIdentity::from_entry(&entry));
        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        let (rendered, _) = render_classification(entry, classification);
        assert!(rendered.contains("Disc 1 · ISO") && rendered.contains("Disc 2 · ISO"),
            "{rendered}");
        assert!(!rendered.contains("media.iso · ISO"), "{rendered}");
    }

    #[test]
    fn multidisc_audio_files_count_preserves_right_border_at_compact_widths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let disc = root.join("Disc 1");
        std::fs::create_dir(&disc).expect("disc directory");
        for n in 1..=7 {
            std::fs::write(disc.join(format!("{n:02}.flac")), b"")
                .expect("audio fixture");
        }
        std::fs::write(root.join("Disc 2.iso"), b"").expect("ISO fixture");

        let entry = BrowseEntry::new(root.to_path_buf(), "Album".into(),
            EntryKind::Directory, 0, None);
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = crate::tui::browse::classify_folder_content_blocking(root, identity);
        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.audio.track_count, 7);
        assert!(classification.units.iter().any(|unit| unit.disc_marker.is_some()));

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(
            entry.path.clone(), identity, classification);
        let accounting = crate::directory_accounting::AccountingService::new(None);
        for width in [14usize, 18, 19, 20, 28] {
            let info = entry_info_lines(&entry, &browse, &accounting, width,
                false, false, false, false, None, None, theme());
            for (index, row) in info.lines.iter().enumerate() {
                let painted = row.iter().map(|span|
                    super::super::display_width::width(span.content.as_ref())).sum::<usize>();
                assert!(painted <= width,
                    "content width={width}, row={index}, painted={painted}: {row:?}");
                let bordered = bordered_line(theme().border_dim, width + 2, row.clone(), theme());
                assert_eq!(bordered.width(), width + 2,
                    "right border displaced at width={width}, row={index}");
                assert_eq!(bordered.spans.last().map(|s| s.content.as_ref()), Some("│"));
            }
            let rows = reference_rows(&info);
            let label_row = rows.iter().position(|line| line.contains("Audio files"))
                .unwrap_or_else(|| panic!("missing Audio files: {rows:?}"));
            if width <= 20 {
                assert_eq!(rows[label_row], " Audio files", "width={width}: {rows:?}");
                assert_eq!(rows[label_row + 1], " 7 files", "width={width}: {rows:?}");
            } else {
                assert_eq!(rows[label_row], " Audio files  7 files",
                    "width={width}: {rows:?}");
            }
        }
    }

    #[test]
    fn long_multidisc_sets_show_six_units_and_the_remainder() {
        let temp = tempfile::tempdir().expect("tempdir");
        for n in 1..=8 {
            std::fs::write(temp.path().join(format!("Disc {n}.iso")), b"")
                .expect("ISO fixture");
        }
        let entry = BrowseEntry::new(temp.path().to_path_buf(), "Album".into(),
            EntryKind::Directory, 0, None);
        let classification = crate::tui::browse::classify_folder_content_blocking(
            temp.path(), ProbeCacheIdentity::from_entry(&entry));
        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        let (rendered, _) = render_classification(entry, classification);
        assert!(rendered.contains("Discs") && rendered.contains("8"), "{rendered}");
        assert!(rendered.contains("+ 2 more discs"), "{rendered}");
        assert!(!rendered.contains("Tracks  0") && !rendered.contains("0 tracks"));
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let (rendered, _) = render_classification(entry, classification);

        let kind = rendered.lines().find(|row| row.contains("Kind"))
            .expect("Kind row");
        let folders = rendered.lines().find(|row| row.contains("Folders"))
            .expect("Folders row");
        assert_eq!(kind.find("directory"), folders.find("unknown"),
            "Kind and counted metrics share the same value column: {rendered}");
        assert!(rendered.starts_with(" artist\n\n\n\n"),
            "keep the header stationary across lazy classification: {rendered}");
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
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
                immediate: Some(crate::tui::browse::DirectoryImmediateCounts { folders: 3, files: 8, audio_files: 3 }),
                complete: true,
                file_count: 312,
                audio_count: 247,
                audio_size: 12_400_000_000,
                total_size: 14_100_000_000,
                ..Default::default()
            },
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Folders"));
        assert!(rendered.contains("Files"));
        assert!(rendered.contains("Audio files"));
        assert!(rendered.contains("Total size"));
        assert!(rendered.contains("Refresh"));
        assert!(rendered.contains("unknown"));
        assert!(!rendered.contains("13.13 GiB"));
        assert!(!rendered.contains("collection · 24 albums"));
        assert!(!rendered.contains("Kind    collection"));
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
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
                immediate: Some(crate::tui::browse::DirectoryImmediateCounts { folders: 2, files: 8, audio_files: 3 }),
                complete: true,
                file_count: 247,
                audio_count: 192,
                audio_size: 42_000_000_000,
                total_size: 45_000_000_000,
                ..Default::default()
            },
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Folders"));
        assert!(rendered.contains("Files"));
        assert!(rendered.contains("Audio files"));
        assert!(rendered.contains("Total size"));
        assert!(rendered.contains("Refresh"));
        assert!(rendered.contains("unknown"));
        assert!(!rendered.contains("41.91 GiB"));
        assert!(rendered.contains("Updated"));
        assert!(!rendered.contains("collection · many albums"));
        assert!(!rendered.contains("Kind    collection"));
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        let fingerprint = disc_probe_fingerprint(&root).expect("disc fingerprint");
        let mut disc = disc_contents(root.clone());
        disc.album_title = Some("Blu-ray Album".into());
        disc.album_artist = Some("Blu-ray Artist".into());
        disc.year = Some("2018".into());
        browse.disc_probe_cache.insert(
            root.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, disc),
        );

        let info = entry_info_lines(
            &entry,
            &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(!rendered.contains("Blu-ray disc folder"));
        assert!(rendered.contains("Blu-ray Album"));
        assert!(rendered.contains("Blu-ray Artist") && rendered.contains("2018"));
        assert!(rendered.contains("7 tracks") && rendered.contains("7 streams"));
        assert!(rendered.contains("Streams"));
        assert!(rendered.contains("LPCM 24/96"));
        assert!(rendered.contains("+ 4 more"));
        assert!(rendered.contains("Protection") && rendered.contains("unencrypted"));
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
                cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
                cue_file_reference_repairs: std::collections::BTreeMap::new(),
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
                    contents.copy_protection.description = "CPPM".into();
                    presentation.id = PresentationId::DvdAudioGroup(1);
                    presentation.format.codec = Some("MLP".to_string());
                }
                FolderDiscMarkerKind::DvdVideo => {
                    contents.format = DiscFormat::DvdVideo;
                    contents.copy_protection.description = "CSS".into();
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
                &crate::directory_accounting::AccountingService::new(None),
                96,
                false,
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
            // SACD areas describe overlapping programs, not independent
            // Blu-ray/DVD streams. Its reference layout reports the track
            // count and areas rather than manufacturing a stream count.
            if marker == FolderDiscMarkerKind::Sacd {
                assert!(rendered.contains("1 track") && rendered.contains("Areas"));
            } else {
                assert!(rendered.contains("1 stream"));
            }
            match marker {
                FolderDiscMarkerKind::DvdAudio => {
                    assert!(rendered.contains("MLP") && rendered.contains("CPPM"));
                }
                FolderDiscMarkerKind::DvdVideo => {
                    assert!(rendered.contains("LPCM") && rendered.contains("CSS"));
                }
                FolderDiscMarkerKind::Sacd => assert!(rendered.contains("DSD")),
                FolderDiscMarkerKind::BluRay => assert!(rendered.contains("AC3")),
                FolderDiscMarkerKind::Iso => unreachable!(),
            }
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
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
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Protection") && rendered.contains("AACS"));
    }

    #[test]
    fn disc_folder_labels_known_none_copy_protection_as_unencrypted() {
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
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
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Protection") && rendered.contains("unencrypted"));
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
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        browse.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        let info = entry_info_lines(
            &entry,
            &browse,
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("Cached Only Disc"));
        assert!(rendered.contains("disc metadata not cached"));
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
    fn streams_control_hitbox_covers_only_the_painted_button() {
        use ratatui::{backend::TestBackend, Terminal};
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("disc.iso");
        std::fs::write(&iso, b"synthetic disc iso").expect("iso fixture");
        let metadata = std::fs::metadata(&iso).expect("metadata");
        let entry = BrowseEntry::new(iso.clone(), "disc.iso".into(),
            EntryKind::BlurayIso, metadata.len(), metadata.modified().ok());
        let mut browse = BrowseState::new();
        browse.entries = vec![entry];
        browse.selected_index = 0;
        let fingerprint = disc_probe_fingerprint(&iso).expect("disc fingerprint");
        browse.disc_probe_cache.insert(iso.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, disc_contents(iso)));
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let mut terminal = Terminal::new(TestBackend::new(72, 36)).expect("terminal");
        let mut buttons = ButtonRenderMap::new();
        terminal.draw(|frame| {
            draw_browse_info(frame, Rect::new(0, 0, 72, 36), &browse,
                &accounting, None, None, &mut buttons, None, theme());
        }).expect("draw");
        let hit = buttons.find_button_rect(&TuiButton::BrowseInfoAudioStreams)
            .expect("Streams button should fit in tall Info pane");
        assert_eq!(hit.width, super::super::display_width::width(" Streams… ") as u16);
        assert_eq!(hit.x, 2, "direct ISO Streams is left-aligned after one indent");
        assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(TuiButton::BrowseInfoAudioStreams));
        assert_eq!(buttons.find_button_at(hit.x + hit.width - 1, hit.y),
            Some(TuiButton::BrowseInfoAudioStreams));
        assert_ne!(buttons.find_button_at(hit.x - 1, hit.y),
            Some(TuiButton::BrowseInfoAudioStreams), "left padding is not clickable");
    }

    // Three screenshot specifications: source data is fixture data, not an
    // assertion that TonePoet must manufacture absent metadata.
    fn reference_accounting(path: &Path, files: u128, audio: u128, folders: u128,
        bytes: u128) -> crate::directory_accounting::AccountingService {
        use crate::directory_accounting::{AccountingService, CachedMeasurement, Counts,
            CompleteMeasurement, Freshness};
        use crate::directory_accounting::model::Observation;
        let mut service = AccountingService::new(None);
        let time = SystemTime::UNIX_EPOCH;
        let counts = Counts { files, audio, folders, bytes, ..Default::default() };
        let view = service.views.entry(path.to_path_buf()).or_default();
        view.cached = Some(CachedMeasurement {
            measurement: CompleteMeasurement {
                path: path.to_path_buf(), counts,
                started: time, finished: time, created: None, modified: None,
                generation: 1, observation: Observation::AsObserved,
            },
            freshness: Freshness::AsObserved,
        });
        view.local = Some(Counts { folders, ..Default::default() });
        service
    }

    fn reference_disc_info(
        root: &Path,
        display_name: &str,
        file_name: &str,
        marker: FolderDiscMarkerKind,
        contents: DiscContents,
        accounting: &crate::directory_accounting::AccountingService,
    ) -> (InfoContent, BrowseState) {
        let entry = BrowseEntry::new(root.to_path_buf(), display_name.to_owned(),
            EntryKind::Directory, 0, None);
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let iso = root.join(file_name);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc, identity,
            audio: FolderAudioSummary::default(),
            units: vec![FolderUnitSummary {
                path: iso.clone(), parent: root.to_path_buf(),
                name: file_name.to_string(), disc_marker: Some(marker),
                audio: FolderAudioSummary::default(),
            }],
            unit_count: 1, collection_many: false, io_budget_exhausted: false,
            disc_marker: Some(marker),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: BTreeMap::new(),
        };
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.to_path_buf(), identity, classification);
        let fingerprint = disc_probe_fingerprint(&iso).expect("ISO fixture fingerprint");
        browse.disc_probe_cache.insert(iso.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, contents));
        let info = entry_info_lines(&entry, &browse, accounting, 78,
            false, false, false, false, None, None, theme());
        (info, browse)
    }

    fn reference_rows(info: &InfoContent) -> Vec<String> {
        info.lines.iter().map(|row| row.iter().map(|span| span.content.as_ref())
            .collect::<String>().trim_end().to_string()).collect()
    }

    fn reference_labels_in_order(info: &InfoContent, labels: &[&str]) {
        let mut last = None;
        for label in labels {
            let index = info.lines.iter().position(|line| line.first()
                .is_some_and(|span| span.content.trim() == *label))
                .unwrap_or_else(|| panic!("missing {label:?}: {:?}", reference_rows(info)));
            assert!(last.is_none_or(|old| index > old),
                "label {label:?} out of order: {:?}", reference_rows(info));
            last = Some(index);
        }
    }

    #[test]
    fn media_reference_rows_never_overrun_narrow_info_panes() {
        for width in [11usize, 13, 14, 18, 28, 78] {
            let mut lines = Vec::new();
            push_media_row(&mut lines, "Protection", vec![
                media_accent("CPPM · copy-protected long identifier", theme()),
            ], width, theme());
            push_media_row(&mut lines, "Streams", vec![
                media_secondary("12× ", theme()),
                media_accent("LPCM 24/192 multichannel", theme()),
            ], width, theme());
            let (mut refresh, mut streams) = (None, None);
            push_media_actions(&mut lines, width, false, true, true, false, false,
                &mut refresh, &mut streams, theme());
            for line in &lines {
                let len: usize = line.iter().map(|s|
                    super::super::display_width::width(s.content.as_ref())).sum();
                assert!(len <= width, "width {width}, line {line:?}, painted {len}");
            }
            assert!(refresh.is_some(), "Refresh is visible at width {width}");
            if let Some(row) = streams {
                assert_eq!(Some(row), refresh, "controls share a row");
            }
        }
    }

    #[test]
    fn compact_media_row_does_not_paint_values_on_a_label_only_row() {
        for width in [11usize, 13, 14] {
            let mut rows = Vec::new();
            push_media_row(&mut rows, "Protection", vec![
                media_accent("CPPM copy-protected", theme()),
            ], width, theme());
            assert_eq!(rows[0].iter().map(|s| s.content.as_ref())
                .collect::<String>(), " Protection");
            assert_eq!(rows[1][0].content.as_ref(), MEDIA_INDENT);
            assert!(rows.len() >= 2, "width={width}: {rows:?}");
            assert!(rows.iter().skip(1).any(|row| row.iter()
                .any(|span| span.content.contains("CPPM"))));
            for row in rows {
                let painted = row.iter().map(|s|
                    super::super::display_width::width(s.content.as_ref())).sum::<usize>();
                assert!(painted <= width, "width={width}, painted={painted}");
            }
        }
    }

    #[test]
    fn narrow_media_rows_keep_status_and_count_units_without_orphaning_the_date() {
        let mut lines = Vec::new();
        push_media_row(&mut lines, "Audio", vec![
            media_secondary("disc metadata not cached", theme()),
        ], 22, theme());
        let status = lines.iter().map(|row| row.iter()
            .map(|span| span.content.as_ref()).collect::<String>())
            .map(|row| row.trim().to_string()).collect::<Vec<_>>().join(" ");
        assert!(status.contains("disc metadata not cached"), "{status}");
        assert!(!status.contains('…'), "status was truncated: {status}");

        lines.clear();
        let mut contents = media_count(12, "file", "files", theme());
        contents.push(media_secondary(" · ", theme()));
        contents.extend(media_count(2, "folder", "folders", theme()));
        push_media_row(&mut lines, "Contents", contents, 22, theme());
        let rows = lines.iter().map(|row| row.iter()
            .map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![" Contents".to_string(),
            " 12 files · 2 folders".to_string()], "{rows:?}");

        lines.clear();
        push_media_row(&mut lines, "Modified", vec![
            media_accent("1970-01-01", theme()),
        ], 22, theme());
        assert_eq!(lines.len(), 1, "date should fit with its label: {lines:?}");
        assert!(lines[0].iter().map(|span| span.content.as_ref())
            .collect::<String>().contains("Modified  1970-01-01"));
        for row in lines {
            let width = row.iter().map(|span| super::super::display_width::width(
                span.content.as_ref())).sum::<usize>();
            assert!(width <= 22, "date row width {width}");
        }
    }

    #[test]
    fn wrapped_audio_count_keeps_a_single_label_and_both_values() {
        let mut values = media_count(64, "track", "tracks", theme());
        values.push(media_secondary(" · ", theme()));
        values.extend(media_count(12, "stream", "streams", theme()));
        let mut rows = Vec::new();
        push_media_row(&mut rows, "Audio", values, 24, theme());
        let painted = rows.iter().map(|row| row.iter()
            .map(|span| span.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        assert_eq!(painted, vec![" Audio        64 tracks".to_string(),
            "              12 streams".to_string()], "{painted:?}");
        assert_eq!(rows[0][0].content.trim(), "Audio");
        assert!(rows[1][0].content.trim().is_empty());
        assert_eq!(super::super::display_width::width(rows[1][0].content.as_ref()), 14);
        for row in &rows {
            let painted_width = row.iter().map(|span|
                super::super::display_width::width(span.content.as_ref())).sum::<usize>();
            assert!(painted_width <= 24, "{painted:?}");
        }
    }

    #[test]
    fn compact_cached_bluray_iso_keeps_wrapped_audio_stream_count() {
        let temp = tempfile::tempdir().unwrap();
        let iso = temp.path().join("movie.iso");
        std::fs::write(&iso, b"disc fingerprint").unwrap();
        let metadata = std::fs::metadata(&iso).unwrap();
        let entry = BrowseEntry::new(iso.clone(), "movie.iso".into(),
            EntryKind::BlurayIso, metadata.len(), metadata.modified().ok());
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let mut contents = disc_contents(iso.clone());
        // Twelve distinct presentations with 64 tracks in total.
        contents.presentations = (1..=12).map(|index| {
            let mut p = disc_presentation(index, "LPCM", 2, "stereo");
            p.tracks = vec![p.tracks[0].clone(); if index <= 4 { 6 } else { 5 }];
            p
        }).collect();
        let fingerprint = disc_probe_fingerprint(&iso).unwrap();
        browse.disc_probe_cache.insert(iso.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, contents));
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let full = entry_info_lines(&entry, &browse, &accounting, 24,
            false, false, false, false, None, None, theme());
        let compact = compact_media_info(full, 8);
        let rows = reference_rows(&compact);
        let audio_row = rows.iter().position(|row| row.trim_start().starts_with("Audio"))
            .expect("Audio row survives compaction");
        assert!(rows[audio_row].contains("64 tracks"), "{rows:?}");
        assert_eq!(rows.get(audio_row + 1).map(String::as_str),
            Some("              12 streams"), "{rows:?}");
        assert_eq!(rows.iter().filter(|row| row.trim_start().starts_with("Audio")).count(), 1);
        assert!(compact.audio_streams_pill_row.is_some(), "Streams control survives");
    }

    #[test]
    fn wrapped_flac_format_continuations_align_without_repeating_label() {
        let mut rows = Vec::new();
        push_media_row(&mut rows, "Format", vec![
            media_accent("FLAC", theme()), media_secondary(" · ", theme()),
            media_accent("24-bit", theme()), media_secondary(" / ", theme()),
            media_accent("96 kHz", theme()), media_secondary(" · ", theme()),
            media_accent("stereo", theme()),
        ], 34, theme());
        let painted = rows.iter().map(|row| row.iter()
            .map(|span| span.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        assert_eq!(rows.len(), 2, "{painted:?}");
        assert_eq!(rows[0][0].content.trim(), "Format");
        assert_eq!(rows[1][0].content.as_ref(), "              ");
        assert_eq!(painted[1], "              stereo");
        assert_eq!(painted.iter().filter(|row| row.contains("Format")).count(), 1);
        for row in &rows {
            let painted_width = row.iter().map(|span|
                super::super::display_width::width(span.content.as_ref())).sum::<usize>();
            assert!(painted_width <= 34, "{painted:?}");
        }
    }

    #[test]
    fn media_rows_leave_a_real_gap_and_share_one_value_column() {
        for label in ["Audio", "Audio files", "Format", "Protection", "Contents"] {
            let mut rows = Vec::new();
            push_media_row(&mut rows, label, media_count(7, "file", "files", theme()),
                78, theme());
            assert_eq!(rows.len(), 1, "{label}: {rows:?}");
            assert_eq!(rows[0].len(), 3);
            let prefix = rows[0][0].content.as_ref();
            assert_eq!(super::super::display_width::width(prefix), 14, "{label}");
            assert!(prefix.starts_with(' ') && prefix.ends_with(' '), "{label}: {prefix:?}");
            assert!(prefix.trim_end().ends_with(label), "{label}: {prefix:?}");
            assert_eq!(rows[0][1].content.as_ref(), "7");
            assert_eq!(rows[0][2].content.as_ref(), " files");
        }
    }

    #[test]
    fn all_entry_kinds_anchor_the_header_at_one_cell_with_four_fixed_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let album = tmp.path().join("Album");
        std::fs::create_dir(&album).unwrap();
        let entries = [
            BrowseEntry::new(album.clone(), "Album".into(), EntryKind::Directory, 0, None),
            BrowseEntry::new(tmp.path().join("track.flac"), "track.flac".into(),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac), 0, None),
            BrowseEntry::new(tmp.path().join("movie.iso"), "movie.iso".into(),
                EntryKind::BlurayIso, 0, None),
        ];
        let accounting = crate::directory_accounting::AccountingService::new(None);
        for entry in &entries {
            let mut browse = BrowseState::new();
            browse.entries = vec![entry.clone()];
            browse.selected_index = 0;
            let cold = entry_info_lines(entry, &browse, &accounting, 78,
                false, false, false, false, None, None, theme());
            assert_eq!(cold.lines[0][0].content.as_ref(), MEDIA_INDENT);
            assert_eq!(cold.lines[0][1].content.as_ref(),
                if entry.is_disc_source() { "movie" } else { entry.name.as_str() });
            assert!(cold.lines[1].is_empty() && cold.lines[3].is_empty());
            assert!(!cold.lines[4].is_empty(), "details begin after header");
        }
        let entry = &entries[0];
        let identity = ProbeCacheIdentity::from_entry(entry);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let cold = entry_info_lines(entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity,
            FolderContentClassification {
                kind: FolderClassificationKind::Album, identity,
                audio: FolderAudioSummary::default(), units: vec![],
                unit_count: 1, collection_many: false, io_budget_exhausted: false,
                disc_marker: None,
                embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
                cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
                cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
                cue_file_reference_repairs: BTreeMap::new(),
            });
        let warm = entry_info_lines(entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        assert_eq!(cold.lines[0][0].content, warm.lines[0][0].content);
        assert_eq!(cold.lines[0][1].content, warm.lines[0][1].content);
        assert!(warm.lines[3].is_empty());
        assert_eq!(warm.lines[4][0].content.trim(), "Audio");
    }

    #[test]
    fn direct_disc_kinds_use_media_rows_instead_of_legacy_kind_and_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let accounting = crate::directory_accounting::AccountingService::new(None);
        for kind in [EntryKind::DvdAudioIso, EntryKind::DvdVideoIso,
            EntryKind::BlurayIso, EntryKind::SacdIso] {
            let iso = tmp.path().join("album.iso");
            std::fs::write(&iso, b"disc fixture").unwrap();
            let meta = std::fs::metadata(&iso).unwrap();
            let entry = BrowseEntry::new(iso.clone(), "album.iso".into(), kind,
                meta.len(), meta.modified().ok());
            let mut browse = BrowseState::new();
            browse.entries = vec![entry.clone()];
            browse.selected_index = 0;
            let cold = entry_info_lines(&entry, &browse, &accounting, 78,
                false, false, false, false, None, None, theme());
            let rows = reference_rows(&cold);
            assert!(rows[0].starts_with(" album"), "{rows:?}");
            assert!(rows[3].is_empty());
            reference_labels_in_order(&cold, &["Audio", "Contents", "Size", "Modified"]);
            assert!(!rows.iter().any(|r| r.contains("Kind") || r.contains('─')),
                "{rows:?}");
            assert!(rows.iter().any(|r| r.contains("1 disc image")));
            assert!(cold.refresh_pill_row.is_none());
        }
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let root = tmp.path().join("DiscDir");
            std::fs::create_dir_all(&root).unwrap();
            let entry = BrowseEntry::new(root, "DiscDir".into(), kind, 0, None);
            let mut browse = BrowseState::new();
            browse.entries = vec![entry.clone()];
            browse.selected_index = 0;
            let info = entry_info_lines(&entry, &browse, &accounting, 78,
                false, false, false, false, None, None, theme());
            let rows = reference_rows(&info);
            reference_labels_in_order(&info, &["Audio", "Contents", "Size", "Modified"]);
            assert!(!rows.iter().any(|r| r.contains("Kind") || r.contains('─')),
                "{rows:?}");
            assert!(info.refresh_pill_row.is_some());
        }
    }

    #[test]
    fn iso_archive_promotion_keeps_title_and_first_metric_position() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = tmp.path().join("movie.iso");
        std::fs::write(&iso, b"disc fixture").unwrap();
        let metadata = std::fs::metadata(&iso).unwrap();
        let archive = BrowseEntry::new(iso.clone(), "movie.iso".into(),
            EntryKind::Archive, metadata.len(), metadata.modified().ok());
        let mut browse = BrowseState::new();
        browse.entries = vec![archive.clone()];
        browse.selected_index = 0;
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let cold = entry_info_lines(&archive, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        let promoted = BrowseEntry::new(iso, "movie.iso".into(),
            EntryKind::BlurayIso, metadata.len(), metadata.modified().ok());
        browse.entries[0] = promoted.clone();
        let warm = entry_info_lines(&promoted, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        assert_eq!(cold.lines[0][1].content.as_ref(), "movie");
        assert_eq!(cold.lines[0][1].content, warm.lines[0][1].content);
        assert!(cold.lines[3].is_empty() && warm.lines[3].is_empty());
        assert_eq!(cold.lines[4][0].content.trim(), "Audio");
        assert_eq!(warm.lines[4][0].content.trim(), "Audio");
        assert!(reference_rows(&cold).iter().any(|row| row.contains("1 disc image")));
        assert!(reference_rows(&warm).iter().any(|row| row.contains("1 disc image")));
    }

    #[test]
    fn direct_bluray_iso_probe_enriches_rows_without_moving_header() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = tmp.path().join("movie.iso");
        std::fs::write(&iso, b"disc fixture").unwrap();
        let meta = std::fs::metadata(&iso).unwrap();
        let entry = BrowseEntry::new(iso.clone(), "movie.iso".into(),
            EntryKind::BlurayIso, meta.len(), meta.modified().ok());
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let cold = entry_info_lines(&entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        let mut contents = disc_contents(iso.clone());
        contents.album_title = Some("Movie".into());
        contents.album_artist = Some("Artist".into());
        contents.year = Some("2026".into());
        let fingerprint = disc_probe_fingerprint(&iso).unwrap();
        browse.disc_probe_cache.insert(iso.clone(),
            DiscProbeCacheEntry::from_success(fingerprint, contents));
        let warm = entry_info_lines(&entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        assert_eq!(cold.lines[0][0].content, warm.lines[0][0].content);
        assert!(cold.lines[1].is_empty());
        assert!(reference_rows(&warm)[1].contains("Artist · 2026"));
        assert!(cold.lines[3].is_empty() && warm.lines[3].is_empty());
        assert_eq!(cold.lines[4][0].content.trim(), warm.lines[4][0].content.trim());
        reference_labels_in_order(&warm,
            &["Audio", "Mixes", "Streams", "Protection", "Contents", "Size", "Modified"]);
        assert!(warm.audio_streams_pill_row.is_some());
        assert!(warm.refresh_pill_row.is_none());
    }

    #[test]
    fn compact_bluray_iso_keeps_first_audio_row_at_one_with_and_without_probe() {
        use ratatui::{backend::TestBackend, Terminal};
        let temp = tempfile::tempdir().unwrap();
        let iso = temp.path().join("movie.iso");
        std::fs::write(&iso, b"disc fingerprint").unwrap();
        let metadata = std::fs::metadata(&iso).unwrap();
        let entry = BrowseEntry::new(iso.clone(), "movie.iso".into(),
            EntryKind::BlurayIso, metadata.len(), metadata.modified().ok());
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        let accounting = crate::directory_accounting::AccountingService::new(None);
        // A bordered 26x10 pane has exactly eight rows and 24 content cells.
        for populated in [false, true] {
            if populated {
                let fingerprint = disc_probe_fingerprint(&iso).unwrap();
                browse.disc_probe_cache.insert(iso.clone(),
                    DiscProbeCacheEntry::from_success(fingerprint, disc_contents(iso.clone())));
            }
            let full = entry_info_lines(&entry, &browse, &accounting, 24,
                false, false, false, false, None, None, theme());
            let compact = compact_media_info(full, 8);
            let rows = reference_rows(&compact);
            assert_eq!(rows[0], " movie", "populated={populated}: {rows:?}");
            assert!(rows[1].starts_with(" Audio"),
                "first detail moved with cache, populated={populated}: {rows:?}");
            assert!(rows.len() <= 8);

            let mut buttons = ButtonRenderMap::new();
            let mut terminal = Terminal::new(TestBackend::new(26, 10)).unwrap();
            terminal.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 26, 10),
                &browse, &accounting, None, None, &mut buttons, None, theme())).unwrap();
            let painted = (1..9).map(|y| (1..25).map(|x|
                terminal.backend().buffer().get(x, y).symbol().to_owned())
                .collect::<Vec<_>>().join("")).collect::<Vec<_>>();
            assert!(painted[0].starts_with(" movie"), "{painted:?}");
            assert!(painted[1].starts_with(" Audio"), "{painted:?}");
            if populated {
                let action = TuiButton::BrowseInfoAudioStreams;
                let rect = buttons.find_button_rect(&action)
                    .expect("Streams action after probe");
                assert_eq!(buttons.find_button_at(rect.x, rect.y), Some(action));
                assert!(rect.y > 0 && rect.bottom() < 10);
            } else {
                // Direct ISO files have no Refresh button before a probe.
                assert!(buttons.find_button_rect(&TuiButton::BrowseInfoRefresh).is_none());
            }
        }
    }

    #[test]
    fn short_bluray_iso_info_does_not_switch_layout_when_disc_cache_fills() {
        use ratatui::{backend::TestBackend, Terminal};
        let tmp = tempfile::tempdir().unwrap();
        let iso = tmp.path().join("movie.iso");
        std::fs::write(&iso, b"disc fingerprint").unwrap();
        let metadata = std::fs::metadata(&iso).unwrap();
        let entry = BrowseEntry::new(iso.clone(), "movie.iso".into(),
            EntryKind::BlurayIso, metadata.len(), metadata.modified().ok());
        let mut app = super::directory_info_presentation_tests::directory_layout_app(entry);
        let mut cold_info_titles: [Option<String>; 3] = [None, None, None];
        let mut cold_browse_titles: [Option<String>; 3] = [None, None, None];
        for populated in [false, true] {
            if populated {
                let fingerprint = disc_probe_fingerprint(&iso).unwrap();
                app.browse.disc_probe_cache.insert(iso.clone(),
                    DiscProbeCacheEntry::from_success(fingerprint, disc_contents(iso.clone())));
            }

            // Browse at 160x24, 160x29, and 160x30 must choose compact
            // geometry before and after the disc cache fills. The standalone
            // Info panes have eight, thirteen, and fourteen content rows.
            for (index, (pane_height, screen_height)) in
                [(10u16, 24u16), (15u16, 29u16), (16u16, 30u16)]
                    .into_iter().enumerate() {
                let mut buttons = ButtonRenderMap::new();
                let mut terminal = Terminal::new(TestBackend::new(48, pane_height)).unwrap();
                terminal.draw(|frame| draw_browse_info(frame,
                    Rect::new(0, 0, 48, pane_height), &app.browse,
                    &app.directory_accounting, None, None, &mut buttons,
                    None, theme())).unwrap();
                let rows = (1..(pane_height - 1)).map(|y| (1..47).map(|x|
                    terminal.backend().buffer().get(x, y).symbol().to_owned())
                    .collect::<Vec<_>>().join("")).collect::<Vec<_>>();
                assert!(rows[0].starts_with(" movie"),
                    "Info title, height={pane_height}, populated={populated}: {rows:?}");
                assert!(rows[1].starts_with(" Audio"),
                    "Info Audio moved, height={pane_height}, populated={populated}: {rows:?}");
                if let Some(cold) = &cold_info_titles[index] {
                    assert_eq!(&rows[0], cold, "Info title moved or changed");
                } else {
                    cold_info_titles[index] = Some(rows[0].clone());
                }
                if populated {
                    let rect = buttons.find_button_rect(&TuiButton::BrowseInfoAudioStreams)
                        .expect("cached ISO exposes Streams action");
                    assert!(rect.x > 0 && rect.right() < 48);
                    assert!(rect.y > 0 && rect.bottom() < pane_height);
                    assert!(rows[(rect.y - 1) as usize].contains("Streams…"));
                    assert_eq!(buttons.find_button_at(rect.x, rect.y),
                        Some(TuiButton::BrowseInfoAudioStreams));
                } else {
                    assert!(buttons.find_button_rect(&TuiButton::BrowseInfoAudioStreams).is_none());
                }

                // Verify the actual three-pane layout and mouse translation,
                // not just the standalone Info renderer.
                let (browse_rows, area, _) =
                    super::directory_info_presentation_tests::render_directory_browse_size(&mut app, 160, screen_height);
                assert_eq!((area.width, area.height), (48, pane_height));
                assert!(browse_rows[0].starts_with(" movie"),
                    "Browse title, height={screen_height}: {browse_rows:?}");
                assert!(browse_rows[1].starts_with(" Audio"),
                    "Browse Audio moved, height={screen_height}, populated={populated}: {browse_rows:?}");
                if let Some(cold) = &cold_browse_titles[index] {
                    assert_eq!(&browse_rows[0], cold, "Browse title moved or changed");
                } else {
                    cold_browse_titles[index] = Some(browse_rows[0].clone());
                }
                if populated {
                    let rect = app.button_map.find_button_rect(&TuiButton::BrowseInfoAudioStreams)
                        .expect("Browse Streams hitbox");
                    assert!(rect.x > area.x && rect.right() < area.right());
                    assert!(rect.y > area.y && rect.bottom() < area.bottom());
                    assert!(browse_rows[(rect.y - area.y - 1) as usize].contains("Streams…"));
                    assert_eq!(app.button_map.find_button_at(rect.x, rect.y),
                        Some(TuiButton::BrowseInfoAudioStreams));
                } else {
                    assert!(app.button_map.find_button_rect(
                        &TuiButton::BrowseInfoAudioStreams).is_none());
                }
            }

            // Tall panes retain the full four-row media reference header.
            let mut tall_buttons = ButtonRenderMap::new();
            let mut tall = Terminal::new(TestBackend::new(48, 36)).unwrap();
            tall.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 48, 36),
                &app.browse, &app.directory_accounting, None, None, &mut tall_buttons,
                None, theme())).unwrap();
            let first_audio = (1..35).find(|y| (1..47).map(|x|
                tall.backend().buffer().get(x, *y).symbol().to_owned())
                .collect::<Vec<_>>().join("").starts_with(" Audio"));
            assert_eq!(first_audio, Some(5),
                "tall Info pane must keep four header rows, populated={populated}");
        }
    }

    #[test]
    fn flac_reference_has_unlabelled_header_audio_format_contents_size_and_action() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("At Fillmore East (Japan Platinum SHM)");
        std::fs::create_dir(&root).unwrap();
        let entry = BrowseEntry::new(root.clone(),
            "At Fillmore East (Japan Platinum SHM)".into(), EntryKind::Directory, 0, None);
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let paths = (1..=7).map(|n| root.join(format!("{n:02}.flac")))
            .collect::<Vec<_>>();
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Album, identity,
            audio: audio_summary(&paths, "FLAC"), units: vec![],
            unit_count: 1, collection_many: false, io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: BTreeMap::new(),
        };
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(root.clone(), identity, classification);
        for path in paths {
            browse.insert_probe_cache_for_test(path, Some(std::sync::Arc::new(CachedInfo {
                source: crate::tui::probe::SourceInfo {
                    format_name: "FLAC".into(), codec: "FLAC".into(), bit_depth: Some(16),
                    sample_format_is_float: Some(false), compression_is_lossless: Some(true),
                    sample_rate: 44_100, channels: 2, channel_layout: "stereo".into(),
                    duration_secs: 4560.0 / 7.0, file_size: 0,
                },
                metadata: crate::tui::probe::SourceMetadata {
                    album: Some("At Fillmore East".into()),
                    artist: Some("The Allman Brothers Band".into()),
                    year: Some("1971".into()), ..Default::default()
                }, transport: None,
            })));
        }
        let accounting = reference_accounting(&root, 28, 7, 1,
            (584.9 * 1_048_576.0) as u128);
        let info = entry_info_lines(&entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        let rows = reference_rows(&info);
        assert_eq!(rows[0].trim(), "At Fillmore East");
        assert_eq!(rows[1].trim(), "The Allman Brothers Band · 1971");
        assert_eq!(rows[2].trim(), "Japan Platinum SHM · FLAC");
        assert!(rows[3].is_empty(), "blank line between header and audio");
        reference_labels_in_order(&info, &["Audio", "Format", "Contents", "Size", "Modified"]);
        assert!(rows.iter().any(|r| r.contains("7 tracks · 1h 16m")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("FLAC · 16-bit / 44.1 kHz · stereo")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("28 files · 1 folder")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("7 audio · 21 other")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("584.9 MiB")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("Kind") || r.contains("─")));
        assert!(info.refresh_pill_row.is_some(), "complete measurement still supports Refresh");
        assert!(info.audio_streams_pill_row.is_none());
        let labels = &info.lines[4];
        assert_eq!(labels[0].style.fg, Some(theme().text_muted));
        assert_eq!(info.lines[0][1].style.fg, Some(theme().text_bright));
        assert!(info.lines[0][1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(info.lines[1][1].style.fg, Some(theme().cyan));
    }

    #[test]
    fn sacd_reference_has_area_flags_one_disc_image_and_adjacent_buttons() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("At Fillmore East (Japan SHM SACD)");
        std::fs::create_dir(&root).unwrap();
        let iso = root.join("album.iso");
        std::fs::write(&iso, b"test disc").unwrap();
        let mut contents = sacd_contents(iso.clone());
        contents.presentations.truncate(1);
        contents.presentations[0] = sacd_presentation(SacdAreaId::Stereo, "Stereo", 2, "stereo");
        contents.presentations[0].tracks = (1..=7).map(|n| DiscTrack {
            number: n, title: None, performer: None,
            duration_secs: None, format_note: None,
        }).collect();
        contents.presentations[0].total_duration_secs = 4560.0;
        contents.album_title = Some("At Fillmore East".into());
        contents.album_artist = Some("The Allman Brothers Band".into());
        contents.year = Some("1971".into());
        let accounting = reference_accounting(&root, 4, 0, 0,
            (3.16 * 1_073_741_824.0) as u128);
        let (info, _) = reference_disc_info(&root,
            "At Fillmore East (Japan SHM SACD)", "album.iso",
            FolderDiscMarkerKind::Sacd, contents, &accounting);
        let rows = reference_rows(&info);
        assert_eq!(rows[0].trim(), "At Fillmore East");
        assert_eq!(rows[1].trim(), "The Allman Brothers Band · 1971");
        assert_eq!(rows[2].trim(), "Japan SHM SACD · ISO");
        reference_labels_in_order(&info, &["Audio", "Format", "Areas", "Contents", "Size", "Modified"]);
        assert!(rows.iter().any(|r| r.contains("7 tracks · 1h 16m")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("DSD64 · 2.8 MHz · stereo")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("stereo ✓ · multichannel —")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("4 files · 1 disc image")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("3.16 GiB")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("Protection") || r.contains("─")));
        assert_eq!(info.refresh_pill_row, info.audio_streams_pill_row);
        assert!(info.refresh_pill_row.is_some());
        let buttons = &info.lines[info.refresh_pill_row.unwrap()];
        assert_eq!(buttons.iter().filter(|s| s.content == " Refresh ").count(), 1);
        assert_eq!(buttons.iter().filter(|s| s.content == " Streams… ").count(), 1);
    }

    #[test]
    fn bluray_reference_has_grouped_streams_mixes_and_unencrypted_status() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Wish You Were Here: 50th Anniversary Edition");
        std::fs::create_dir(&root).unwrap();
        let iso = root.join("disc.iso");
        std::fs::write(&iso, b"test blu-ray").unwrap();
        let mut contents = disc_contents(iso.clone());
        contents.presentations.clear();
        for index in 0..12usize {
            let (codec, channels, layout, rate) = if index == 0 {
                ("LPCM", 2, "stereo", 192_000)
            } else if index <= 5 {
                ("LPCM", 2, "stereo", 96_000)
            } else if index <= 7 {
                ("DTS-HD MA", 2, "stereo", 48_000)
            } else {
                ("DTS-HD MA", 6, "5.1", 48_000)
            };
            let mut presentation = disc_presentation((index + 1) as u32, codec, channels, layout);
            presentation.format.sample_rate = Some(rate);
            presentation.tracks = (0..if index < 8 { 5 } else { 6 }).map(|n| DiscTrack {
                number: n + 1, title: None, performer: None,
                duration_secs: None, format_note: None,
            }).collect();
            contents.presentations.push(presentation);
        }
        // Two stereo formats and a multichannel mix. The actual mix count
        // is derived from the presentations, never assigned by the renderer.
        contents.album_title = Some("Wish You Were Here: 50th Anniversary Edition".into());
        contents.album_artist = Some("Pink Floyd".into());
        contents.year = Some("1975".into());
        let mut accounting = crate::directory_accounting::AccountingService::new(None);
        accounting.views.entry(root.clone()).or_default().local =
            Some(crate::directory_accounting::Counts {
                files: 1, folders: 1, ..Default::default()
            });
        let (info, _) = reference_disc_info(&root,
            "Wish You Were Here: 50th Anniversary Edition", "disc.iso",
            FolderDiscMarkerKind::BluRay, contents, &accounting);
        let rows = reference_rows(&info);
        assert_eq!(rows[0].trim(), "Wish You Were Here: 50th Anniversary Edition");
        assert_eq!(rows[1].trim(), "Pink Floyd · 1975");
        assert_eq!(rows[2].trim(), "Blu-ray 24-192 LPCM · ISO");
        reference_labels_in_order(&info, &["Audio", "Mixes", "Streams", "Protection", "Contents", "Size", "Modified"]);
        assert!(rows.iter().any(|r| r.contains("64 tracks · 12 streams")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("8 stereo · 4 multichannel")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("1× LPCM 24/192 stereo")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("5× LPCM 24/96 stereo")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("+ 6 more")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Protection") && r.contains("unencrypted")));
        assert!(rows.iter().any(|r| r.contains("1 disc image · 1 folder")), "{rows:?}");
        assert!(rows.iter().any(|r| r.trim().starts_with("Size") && r.contains("unknown")), "{rows:?}");
        assert_eq!(info.refresh_pill_row, info.audio_streams_pill_row);
    }

    #[test]
    fn media_unknown_and_scanning_views_keep_refresh_and_cancel_action() {
        let entry = BrowseEntry::new(PathBuf::from("/tmp/Album"), "Album".into(),
            EntryKind::Directory, 0, None);
        let kind = FolderClassificationKind::Album;
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = FolderContentClassification {
            kind, identity, audio: FolderAudioSummary::default(), units: vec![],
            unit_count: 1, collection_many: false, io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: BTreeMap::new(),
        };
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.insert_folder_classification_for_identity(entry.path.clone(), identity, classification);
        let mut accounting = crate::directory_accounting::AccountingService::new(None);
        let unknown = entry_info_lines(&entry, &browse, &accounting, 55,
            false, false, false, false, None, None, theme());
        assert!(reference_rows(&unknown).iter().any(|r| r.contains("Size") && r.contains("unknown")));
        assert!(unknown.refresh_pill_row.is_some());
        accounting.views.entry(entry.path.clone()).or_default().running = true;
        accounting.views.get_mut(&entry.path).unwrap().progress =
            Some(crate::directory_accounting::ScanProgress {
                files_examined: 4, bytes_examined: 4_194_304, ..Default::default()
            });
        let running = entry_info_lines(&entry, &browse, &accounting, 55,
            false, false, false, false, None, None, theme());
        assert!(reference_rows(&running).iter().any(|r| r.contains("4.0 MiB…")));
        assert!(reference_rows(&running)[running.refresh_pill_row.unwrap()].contains("Cancel"));
    }

    #[test]
    fn adjacent_media_buttons_have_separate_exact_hitboxes() {
        use ratatui::{backend::TestBackend, Terminal};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Disc");
        std::fs::create_dir(&root).unwrap();
        let iso = root.join("disc.iso");
        std::fs::write(&iso, b"probe fingerprint").unwrap();
        let contents = disc_contents(iso.clone());
        let mut accounting = reference_accounting(&root, 1, 0, 1, 0);
        let (_, browse) = reference_disc_info(&root, "Disc", "disc.iso",
            FolderDiscMarkerKind::BluRay, contents, &accounting);
        let mut buttons = ButtonRenderMap::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
        terminal.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 80, 32),
            &browse, &accounting, None, None, &mut buttons, None, theme())).unwrap();
        let refresh = buttons.find_button_rect(&TuiButton::BrowseInfoRefresh).unwrap();
        let streams = buttons.find_button_rect(&TuiButton::BrowseInfoAudioStreams).unwrap();
        assert_eq!(refresh.y, streams.y);
        assert_eq!(streams.x - refresh.right(), 2);
        assert_eq!(refresh.width as usize, super::super::display_width::width(" Refresh "));
        assert_eq!(streams.width as usize, super::super::display_width::width(" Streams… "));
        assert_eq!(buttons.find_button_at(refresh.x, refresh.y), Some(TuiButton::BrowseInfoRefresh));
        assert_eq!(buttons.find_button_at(streams.x, streams.y),
            Some(TuiButton::BrowseInfoAudioStreams));
        assert_eq!(buttons.find_button_at(refresh.right(), refresh.y), None);
        let line = (0..80).map(|x| terminal.backend().buffer().get(x, refresh.y).symbol().to_owned())
            .collect::<Vec<_>>().join("");
        assert!(line.contains("Refresh") && line.contains("Streams"), "{line}");

        // During a scan the same action changes to Cancel, without losing
        // its click region or moving Streams to a separate line.
        accounting.views.get_mut(&root).unwrap().running = true;
        accounting.views.get_mut(&root).unwrap().progress =
            Some(crate::directory_accounting::ScanProgress {
                bytes_examined: 1024, ..Default::default()
            });
        let mut running_buttons = ButtonRenderMap::new();
        terminal.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 80, 32),
            &browse, &accounting, None, None, &mut running_buttons, None,
            theme())).unwrap();
        let cancel = running_buttons.find_button_rect(&TuiButton::BrowseInfoRefresh).unwrap();
        let running_streams = running_buttons.find_button_rect(&TuiButton::BrowseInfoAudioStreams).unwrap();
        assert_eq!(cancel.width as usize, super::super::display_width::width(" Cancel "));
        assert_eq!(cancel.y, running_streams.y);
        assert_eq!(running_streams.x - cancel.right(), 2);
        assert_eq!(running_buttons.find_button_at(cancel.x, cancel.y),
            Some(TuiButton::BrowseInfoRefresh));
        let row = (0..80).map(|x| terminal.backend().buffer().get(x, cancel.y).symbol().to_owned())
            .collect::<Vec<_>>().join("");
        assert!(row.contains("Cancel") && row.contains("Streams"), "{row}");
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
            &crate::directory_accounting::AccountingService::new(None),
            96,
            false,
            false,
            false,
            false,
            None,
            None,
            theme(),
        );
        let rendered = flatten(&info);

        assert!(rendered.contains("SACD · ISO"), "{rendered}");
        assert!(rendered.contains("1 track"), "{rendered}");
        assert!(rendered.contains("DSD64") && rendered.contains("Areas"), "{rendered}");
        assert!(!rendered.contains("Kind") && !rendered.contains('─'), "{rendered}");
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


#[cfg(test)]
mod directory_info_presentation_tests {
    use super::*;
    use std::time::Duration;

    fn theme() -> crate::tui::theme::Theme {
        crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        )
    }

    fn render(entry: &BrowseEntry, browse: &BrowseState) -> String {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let info = entry_info_lines(entry, browse, &crate::directory_accounting::AccountingService::new(None), 100, false, false, false, false, None, None, theme);
        info.lines.iter().map(|spans| spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>().join("\n")
    }

    #[tokio::test]
    async fn parent_row_displays_current_directory_and_not_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = temp.path().join("Rock");
        std::fs::create_dir(&current).expect("rock");
        let child = current.join("Yes");
        std::fs::create_dir(&child).expect("yes");
        std::fs::write(current.join("one.flac"), b"abc").expect("audio");
        std::fs::write(current.join("cover.jpg"), b"d").expect("cover");
        std::fs::write(temp.path().join("parent-only.txt"), b"different").expect("parent-only");
        let parent_entry = BrowseEntry::new(temp.path().to_path_buf(), "..".into(), EntryKind::ParentDir, 0, None);
        let mut browse = BrowseState::new();
        browse.current_dir = current.clone();
        browse.entries = vec![parent_entry.clone()];
        browse.all_dirs = vec![BrowseEntry::new(child, "Yes".into(), EntryKind::Directory, 0, None)];
        browse.all_files = vec![
            BrowseEntry::new(current.join("one.flac"), "one.flac".into(),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac), 3, None),
            BrowseEntry::new(current.join("cover.jpg"), "cover.jpg".into(), EntryKind::OtherFile, 1, None),
        ];
        browse.publish_scanned_entries(Some(parent_entry.clone()), browse.all_dirs.clone(), browse.all_files.clone());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        browse.probe_current(&tx);
        // Debounce tested by forcing its clock in the Browse module tests;
        // here simply wait out the public policy interval.
        tokio::time::sleep(Duration::from_millis(190)).await;
        browse.check_probe_debounce(&tx);
        let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&current).expect("identity")
        );
        browse.insert_dir_stats_for_identity(current, identity,
            crate::tui::browse::DirStats { total_size: 4, complete: true, ..Default::default() });
        let text = render(&parent_entry, &browse);
        assert!(text.contains("Rock"));
        assert!(!text.contains("parent directory"));
        assert!(text.contains("Folders"));
        assert!(text.contains("Files"));
        assert!(text.contains("Audio files"));
        assert!(text.contains("Total size"));
        assert!(text.contains("Refresh"));
        assert!(!text.contains("4 B"));
        assert!(text.contains("Updated"));
    }

    #[test]
    fn incomplete_collection_size_is_never_published_as_total() {
        let entry = BrowseEntry::new(
            Path::new("/music/collection").to_path_buf(), "collection".into(), EntryKind::Directory, 0, None
        );
        let identity = crate::tui::browse::ProbeCacheIdentity::from_entry(&entry);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.insert_dir_stats_for_identity(entry.path.clone(), identity,
            crate::tui::browse::DirStats {
                folder_count: 100,
                immediate: Some(crate::tui::browse::DirectoryImmediateCounts {
                    folders: 3, files: 2, audio_files: 1,
                }),
                total_size: 2 * (1u64 << 40),
                complete: false,
                ..Default::default()
            });
        let text = render(&entry, &browse);
        assert!(text.contains("Folders"));
        assert!(text.contains("Files"));
        assert!(text.contains("Audio files"));
        assert!(text.contains("Total size"));
        assert!(text.contains("Refresh"));
        assert!(!text.contains("not calculated"));
        assert!(!text.contains("unavailable"));
        assert!(!text.contains("2.00 TiB"));
        assert!(!text.contains("(partial)"));
        assert!(!text.contains("Disk free"));
    }

    fn directory_info_with_service(
        entry: &BrowseEntry,
        browse: &BrowseState,
        accounting: &crate::directory_accounting::AccountingService,
    ) -> InfoContent {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        entry_info_lines(entry, browse, accounting, 100, false, false, false,
            false, None, None, theme)
    }

    fn flatten_lines(info: &InfoContent) -> Vec<String> {
        info.lines.iter().map(|row| row.iter()
            .map(|span| span.content.as_ref()).collect::<String>()).collect()
    }

    fn fake_measurement(path: &Path, bytes: u128) -> crate::directory_accounting::CachedMeasurement {
        use crate::directory_accounting::model::{CompleteMeasurement, Counts, Observation};
        let time = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86_400);
        crate::directory_accounting::CachedMeasurement {
            measurement: CompleteMeasurement {
                path: path.to_path_buf(),
                counts: Counts { bytes, ..Counts::default() },
                started: time, finished: time, created: None, modified: None,
                generation: 10, observation: Observation::AsObserved,
            },
            freshness: crate::directory_accounting::Freshness::Unverified,
        }
    }

    #[test]
    fn directory_metric_columns_align_and_unknown_counts_have_dim_rows() {
        use crate::directory_accounting::{AccountingService, Counts};
        let path = Path::new("/music/albums");
        let entry = BrowseEntry::new(path.to_path_buf(), "albums".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let mut service = AccountingService::new(None);
        let unknown = directory_info_with_service(&entry, &browse, &service);
        let text = flatten_lines(&unknown).join("\n");
        assert!(text.contains("Folders"));
        assert!(text.contains("Files"));
        assert!(text.contains("Audio files"));
        assert_eq!(text.matches("unknown").count(), 4);
        for label in ["Folders", "Files", "Audio files", "Total size"] {
            let row = unknown.lines.iter().find(|row|
                row.first().is_some_and(|span| span.content.trim() == label)).unwrap();
            assert_eq!(row[1].style.fg, Some(theme().text_dim));
        }
        assert!(!text.contains("not calculated"));
        assert!(!text.contains("unavailable"));
        assert!(!text.contains("Ctrl+P"));
        assert!(!text.contains("Disk free"));
        assert!(text.contains("Total size"));
        assert!(text.contains("Refresh"));
        assert!(unknown.refresh_pill_row.is_some());

        service.views.entry(path.to_path_buf()).or_default().local = Some(Counts {
            folders: 2, files: 12, audio: 9, ..Counts::default()
        });
        let known = directory_info_with_service(&entry, &browse, &service);
        let rows = known.lines.iter().filter(|row| row.len() == 2
            && row[0].content.starts_with("   ")).collect::<Vec<_>>();
        let row = rows.iter().find(|row| row[0].content.trim() == "Folders").unwrap();
        assert_eq!(row[0].content.chars().count(), 3 + "Audio files".len() + 2);
        assert_eq!(row[1].content.as_ref(), "2");
        assert!(row[0].content.ends_with("  "));
        // A one-level preview does not turn recursive Files/Audio into known values.
        assert_eq!(rows.iter().find(|row| row[0].content.trim() == "Files").unwrap()[1].content.as_ref(), "unknown");
        assert_eq!(rows.iter().find(|row| row[0].content.trim() == "Audio files").unwrap()[1].content.as_ref(), "unknown");
        // An immediate folder count of zero is a known value, not an omitted row.
        service.views.get_mut(path).unwrap().local = Some(Counts::default());
        let zero = directory_info_with_service(&entry, &browse, &service);
        let text = flatten_lines(&zero).join("\n");
        assert!(text.contains("Folders      0"));
        assert!(text.contains("Audio files  unknown"));
    }

    #[test]
    fn ordinary_directory_kind_content_and_metrics_share_value_column() {
        use crate::directory_accounting::{AccountingService, Counts};
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Collection");
        std::fs::create_dir(&folder).unwrap();
        let entry = BrowseEntry::new(folder.clone(), "Collection".into(),
            EntryKind::Directory, 0,
            Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86_400)));
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        browse.selected_index = 0;
        browse.mark_folder_classification_pending_for_test(folder.clone());
        let mut accounting = AccountingService::new(None);
        let view = accounting.views.entry(folder.clone()).or_default();
        view.local = Some(Counts { folders: 2, ..Counts::default() });
        view.cached = Some(fake_measurement(&folder, 1024));
        let mut full = entry_info_lines(&entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        // The actual Disk free producer uses this same metric-row formatter.
        // Add a representative disk row without filesystem-dependent statvfs.
        let mut disk_row = Vec::new();
        push_directory_metric_line(&mut disk_row, "Disk free",
            vec![Span::styled("8 GiB / 16 GiB", theme().text_style())],
            directory_metric_label_width(78), theme());
        let updated = directory_info_metric_block(&full, "Updated").unwrap().0;
        full.lines.splice(updated..updated, disk_row);
        let mut column = None;
        for label in ["Kind", "Folders", "Files", "Audio files", "Total size",
            "Disk free", "Updated", "Content"] {
            let row = full.lines.iter().find(|r| r.first()
                .is_some_and(|span| span.content.trim() == label))
                .unwrap_or_else(|| panic!("missing {label}: {:?}", flatten_lines(&full)));
            assert!(row.len() >= 2, "no value on {label}: {row:?}");
            let start = super::super::display_width::width(row[0].content.as_ref());
            assert_eq!(start, 16, "{label} starts at {start}, expected 16");
            if let Some(previous) = column { assert_eq!(start, previous, "{label}"); }
            column = Some(start);
        }

        // The same Kind column applies when browsing the parent row and a
        // classified non-media collection, not just an unclassified child.
        let parent = BrowseEntry::new(temp.path().to_path_buf(), "..".into(),
            EntryKind::ParentDir, 0, None);
        browse.current_dir = folder.clone();
        browse.entries = vec![parent.clone()];
        let parent_info = entry_info_lines(&parent, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        for label in ["Kind", "Folders", "Files", "Audio files", "Total size"] {
            let row = parent_info.lines.iter().find(|r| r.first()
                .is_some_and(|span| span.content.trim() == label)).unwrap();
            assert_eq!(super::super::display_width::width(row[0].content.as_ref()), 16);
        }
        browse.current_dir = temp.path().to_path_buf();
        browse.entries = vec![entry.clone()];
        browse.insert_folder_classification_for_identity(folder.clone(),
            crate::tui::browse::ProbeCacheIdentity::from_entry(&entry),
            example_classification(&entry, FolderClassificationKind::Collection));
        let classified = entry_info_lines(&entry, &browse, &accounting, 78,
            false, false, false, false, None, None, theme());
        let kind = classified.lines.iter().find(|r| r.first()
            .is_some_and(|span| span.content.trim() == "Kind")).unwrap();
        assert_eq!(super::super::display_width::width(kind[0].content.as_ref()), 16);

        // Wider alignment must not introduce a painted-width overflow on a
        // compact pane. Long provisional values use the metric continuation.
        let mut cold = BrowseState::new();
        cold.entries = vec![entry.clone()];
        cold.mark_folder_classification_pending_for_test(folder);
        let narrow = entry_info_lines(&entry, &cold, &accounting, 22,
            false, false, false, false, None, None, theme());
        for label in ["Kind", "Content"] {
            let start = narrow.lines.iter().position(|r| r.first()
                .is_some_and(|span| span.content.trim() == label)).unwrap();
            assert_eq!(narrow.lines[start].len(), 1, "{label} should wrap");
            assert!(flatten_lines(&narrow)[start + 1].trim().len() > 0);
            assert!(narrow.lines[start..=start + 1].iter().all(|r| r.iter()
                .map(|span| super::super::display_width::width(span.content.as_ref()))
                .sum::<usize>() <= 22));
        }
    }

    #[test]
    fn immediate_folders_stay_distinct_from_recursive_cached_folders() {
        use crate::directory_accounting::{AccountingService, Counts};
        let path = Path::new("/music/parent");
        let entry = BrowseEntry::new(path.to_path_buf(), "parent".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let mut service = AccountingService::new(None);
        let mut cached = fake_measurement(path, 1024);
        cached.measurement.counts = Counts {
            folders: 99, files: 80, audio: 70, bytes: 1024, ..Default::default()
        };
        service.views.entry(path.to_path_buf()).or_default().cached = Some(cached);
        let info = directory_info_with_service(&entry, &browse, &service);
        let rendered = flatten_lines(&info).join("\n");
        assert!(rendered.contains("Folders      unknown"));
        assert!(rendered.contains("Files        80"));
        assert!(rendered.contains("Audio files  70"));
        assert!(info.refresh_pill_row.is_none());
        service.views.get_mut(path).unwrap().local = Some(Counts {
            folders: 3, files: 2, audio: 1, ..Default::default()
        });
        let info = directory_info_with_service(&entry, &browse, &service);
        let rendered = flatten_lines(&info).join("\n");
        assert!(rendered.contains("Folders      3"));
        assert!(!rendered.contains("Folders      99"));
        assert!(rendered.contains("Files        80"));
        assert!(rendered.contains("Audio files  70"));
    }

    #[test]
    fn saved_total_is_indistinguishable_from_new_total_and_invalid_total_is_hidden() {
        use crate::directory_accounting::{AccountingService, Freshness};
        let path = Path::new("/music/catalogue");
        let entry = BrowseEntry::new(path.to_path_buf(), "catalogue".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let mut service = AccountingService::new(None);
        let saved = fake_measurement(path, 1024u128 * 1024 * 1024);
        service.views.entry(path.to_path_buf()).or_default().cached = Some(saved);
        let restored = directory_info_with_service(&entry, &browse, &service);
        let text = flatten_lines(&restored).join("\n");
        assert!(text.contains("1.00 GiB"));
        assert!(!text.contains("Refresh"));
        assert!(restored.refresh_pill_row.is_none());
        assert!(!text.contains("measurement"));
        assert!(!text.contains("historical"));
        assert!(!text.contains("measured at"));
        assert!(!text.contains("1970"));
        service.views.get_mut(path).unwrap().cached.as_mut().unwrap().freshness = Freshness::AsObserved;
        let observed = directory_info_with_service(&entry, &browse, &service);
        assert_eq!(flatten_lines(&restored), flatten_lines(&observed));

        let view = service.views.get_mut(path).unwrap();
        view.cached.as_mut().unwrap().freshness = Freshness::KnownInvalid;
        let stale = directory_info_with_service(&entry, &browse, &service);
        let text = flatten_lines(&stale).join("\n");
        assert!(!text.contains("1.00 GiB"));
        assert!(text.contains("Refresh"));
        assert!(stale.refresh_pill_row.is_some());

        service.views.get_mut(path).unwrap().running = true;
        let running = directory_info_with_service(&entry, &browse, &service);
        let text = flatten_lines(&running).join("\n");
        assert!(text.contains("0 B…"));
        assert!(text.contains("Cancel"));
        assert!(!text.contains("Refresh"));
        assert!(!text.contains("1.00 GiB"));
        assert!(running.refresh_pill_row.is_some());
    }

    #[test]
    fn running_progress_is_ephemeral_and_button_swaps_to_cancel() {
        use crate::directory_accounting::{AccountingService, ScanProgress};
        let path = Path::new("/music/big-album");
        let entry = BrowseEntry::new(path.to_path_buf(), "big-album".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let mut service = AccountingService::new(None);
        let view = service.views.entry(path.to_path_buf()).or_default();
        view.running = true;
        view.progress = Some(ScanProgress {
            folders_discovered: 8, files_examined: 77, audio_examined: 70,
            bytes_examined: 2 * 1024 * 1024 * 1024, ..Default::default()
        });
        let info = directory_info_with_service(&entry, &browse, &service);
        let content = flatten_lines(&info).join("\n");
        assert!(content.contains("77"));
        assert!(content.contains("70"));
        assert!(content.contains("2.00 GiB…"));
        assert!(content.contains("Cancel"));
        assert!(!content.contains("Refresh"));
        assert_eq!(info.lines[info.refresh_pill_row.unwrap() - 1].len(), 0);
        let spinning = info.lines.iter().filter(|row|
            row.iter().any(|span| span.style.fg == Some(theme().purple))).count();
        assert_eq!(spinning, 4);
        // If an independently known direct folder count exists, its row
        // remains stable while the other three values are still being tallied.
        service.views.get_mut(path).unwrap().running = false;
        service.views.get_mut(path).unwrap().local = Some(
            crate::directory_accounting::Counts { folders: 8, ..Default::default() });
        service.views.get_mut(path).unwrap().running = true;
        let stable = directory_info_with_service(&entry, &browse, &service);
        let folders_row = stable.lines.iter().find(|row|
            row.first().is_some_and(|span| span.content.trim() == "Folders")).unwrap();
        assert_eq!(folders_row[1].content.as_ref(), "8");
        assert!(!folders_row.iter().any(|span| span.style.fg == Some(theme().purple)));
        let stable_spinning = stable.lines.iter().filter(|row|
            row.iter().any(|span| span.style.fg == Some(theme().purple))).count();
        assert_eq!(stable_spinning, 3);
    }

    #[test]
    fn full_height_info_has_name_kind_unknown_values_and_separate_action() {
        let path = Path::new("/music/Long Artist - Album Collection");
        let entry = BrowseEntry::new(path.to_path_buf(),
            "Long Artist - Album Collection".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let info = directory_info_with_service(&entry, &browse,
            &crate::directory_accounting::AccountingService::new(None));
        let rows = flatten_lines(&info);
        let labels = ["Kind", "Folders", "Files", "Audio files", "Total size"];
        let positions: Vec<_> = labels.iter().map(|label| rows.iter().position(|row|
            row.contains(label)).unwrap()).collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        let action = info.refresh_pill_row.unwrap();
        assert!(action > *positions.last().unwrap());
        assert!(rows[action - 1].trim().is_empty());
        assert!(rows[action].contains(" Refresh "));
    }

    #[test]
    fn narrow_directory_info_keeps_refresh_pill_whole_and_focusable() {
        let path = Path::new("/music/collection");
        let entry = BrowseEntry::new(path.to_path_buf(), "collection".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let info = entry_info_lines(&entry, &browse, &accounting, 13,
            false, false, false, false, None, Some(BrowseInfoFocus::Refresh), theme);
        let rows = flatten_lines(&info);
        let row = info.refresh_pill_row.expect("narrow pane pill row");
        assert!(rows[row].contains(" Refresh "));
        assert!(rows.iter().any(|line| line.contains("Total size")));
        assert!(rows[row].len() <= 13, "pill fits narrow content area");
    }

    #[test]
    fn refresh_pill_hitbox_is_registered_only_when_visible() {
        use ratatui::{backend::TestBackend, Terminal};
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Child");
        std::fs::create_dir(&folder).unwrap();
        let entry = BrowseEntry::new(folder, "Child".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry];
        let mut buttons = ButtonRenderMap::new();
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let backend = TestBackend::new(70, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 70, 24),
            &browse, &accounting, None, None, &mut buttons, None, theme)).unwrap();
        let hit = buttons.find_button_rect(&TuiButton::BrowseInfoRefresh).expect("Refresh hit region");
        let row = (0..70).map(|x| terminal.backend().buffer().get(x, hit.y).symbol().to_string())
            .collect::<Vec<_>>().join("");
        assert!(row.contains("Refresh"));
        assert_eq!(buttons.find_button_at(hit.x, hit.y), Some(TuiButton::BrowseInfoRefresh));
    }

    // The full Browse layout leaves an eight-row Info content viewport at
    // 24 terminal rows. Test that layout, not a standalone tall Info pane.
    fn render_directory_browse_24(
        app: &mut AppState,
        width: u16,
    ) -> (Vec<String>, Rect, Option<Rect>) {
        use ratatui::{backend::TestBackend, Terminal};
        let screen = Rect::new(0, 0, width, 24);
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        app.button_map.clear();
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| draw_browse_screen(frame, screen, app, theme)).unwrap();
        let info_area = browse_content_layout(Rect::new(0, 12, width, 10), &app.browse)[2];
        let rows = (info_area.y + 1..info_area.bottom() - 1)
            .map(|y| (info_area.x + 1..info_area.right() - 1)
                .map(|x| terminal.backend().buffer().get(x, y).symbol().to_string())
                .collect::<Vec<_>>().join(""))
            .collect::<Vec<_>>();
        let hit = app.button_map.find_button_rect(&TuiButton::BrowseInfoRefresh);
        (rows, info_area, hit)
    }

    /// Use the actual three-pane Browse layout at arbitrary terminal heights.
    pub(super) fn render_directory_browse_size(
        app: &mut AppState,
        width: u16,
        height: u16,
    ) -> (Vec<String>, Rect, Option<Rect>) {
        use ratatui::{backend::TestBackend, Terminal};
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        app.button_map.clear();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw_browse_screen(
            frame, Rect::new(0, 0, width, height), app, theme,
        )).unwrap();
        // Single-tab Browse: 7 header + 5 toolbar + 2 footer.
        let content = Rect::new(0, 12, width, height.saturating_sub(14));
        let info_area = browse_content_layout(content, &app.browse)[2];
        let rows = (info_area.y + 1..info_area.bottom() - 1)
            .map(|y| (info_area.x + 1..info_area.right() - 1)
                .map(|x| terminal.backend().buffer().get(x, y).symbol().to_string())
                .collect::<Vec<_>>().join(""))
            .collect::<Vec<_>>();
        let hit = app.button_map.find_button_rect(&TuiButton::BrowseInfoRefresh);
        (rows, info_area, hit)
    }

    fn assert_complete_unknowns(rows: &[String], expected: usize) {
        for label in ["Folders", "Files", "Audio files", "Total size"] {
            assert!(rows.iter().any(|row| row.contains(label)),
                "missing {label}: {rows:?}");
        }
        assert_eq!(rows.join("\n").matches("unknown").count(), expected,
            "an unknown value was elided or clipped: {rows:?}");
    }

    #[test]
    fn sixty_six_by_twenty_four_keeps_every_cold_unknown_and_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Unmeasured");
        std::fs::create_dir(&folder).unwrap();
        let entry = BrowseEntry::new(folder.clone(), "Unmeasured".into(),
            EntryKind::Directory, 0, None);
        let mut app = directory_layout_app(entry);
        app.directory_accounting.views.get_mut(&folder).unwrap().local = None;
        let (rows, area, hit) = render_directory_browse_size(&mut app, 66, 24);
        assert_complete_unknowns(&rows, 4);
        assert_refresh_visible(&rows, area, hit, &app);

        // The one-level preview establishes Folders, not the recursive totals.
        app.directory_accounting.views.get_mut(&folder).unwrap().local =
            Some(crate::directory_accounting::Counts {
                folders: 7, ..Default::default()
            });
        let (rows, area, hit) = render_directory_browse_size(&mut app, 66, 24);
        assert_complete_unknowns(&rows, 3);
        assert!(rows.iter().any(|row| row.contains("Folders") && row.contains('7')),
            "direct folder count should remain whole: {rows:?}");
        assert_refresh_visible(&rows, area, hit, &app);

        let view = app.directory_accounting.views.get_mut(&folder).unwrap();
        view.running = true;
        view.progress = Some(crate::directory_accounting::ScanProgress {
            folders_discovered: 7, files_examined: 12, audio_examined: 9,
            bytes_examined: 2 * 1024 * 1024, ..Default::default()
        });
        let (rows, area, hit) = render_directory_browse_size(&mut app, 66, 24);
        assert_complete_unknowns(&rows, 0);
        assert!(rows.iter().any(|row| row.contains("Cancel")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("MiB…")), "{rows:?}");
        let hit = hit.expect("Cancel hitbox");
        assert_eq!(hit.width, 8);
        assert!(rows[(hit.y - area.y - 1) as usize].contains("Cancel"));
        assert_eq!(app.button_map.find_button_at(hit.x, hit.y),
            Some(TuiButton::BrowseInfoRefresh));
    }

    pub(super) fn directory_layout_app(entry: BrowseEntry) -> AppState {
        let path = entry.path.clone();
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.current_dir = path.parent().unwrap().to_path_buf();
        app.browse.entries = vec![entry];
        app.browse.selected_index = 0;
        app.browse.explore_enabled = true;
        app.browse.explore_collapsed = false;
        app.browse.info_enabled = true;
        app.browse.info_collapsed = false;
        app.directory_accounting.views.entry(path).or_default().local = Some(
            crate::directory_accounting::Counts {
                folders: 2, files: 12, audio: 9, ..Default::default()
            },
        );
        app
    }

    fn assert_refresh_visible(rows: &[String], info_area: Rect, hit: Option<Rect>, app: &AppState) {
        assert_eq!(rows.len(), 8, "24-row Browse has eight Info content rows");
        assert!(rows.iter().any(|row| row.contains("Refresh")),
            "Refresh must not fall below the viewport: {rows:?}");
        let hit = hit.expect("Refresh must have a registered mouse hitbox");
        assert_eq!(hit.width, 9, "mouse hitbox covers only the painted button");
        assert!(hit.x > info_area.x && hit.right() < info_area.right());
        assert!(hit.y > info_area.y && hit.bottom() < info_area.bottom());
        assert!(rows[(hit.y - info_area.y - 1) as usize].contains("Refresh"),
            "hitbox must align with the actually rendered pill");
        assert_eq!(app.button_map.find_button_at(hit.x, hit.y), Some(TuiButton::BrowseInfoRefresh));
    }

    #[test]
    fn directory_accounting_and_refresh_are_visible_at_80_by_24_even_after_counts() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("Collection");
        std::fs::create_dir(&child).unwrap();
        let entry = BrowseEntry::new(child, "Collection".into(), EntryKind::Directory, 0, None);
        for width in [66, 80, 120] {
            let mut app = directory_layout_app(entry.clone());
            let (rows, info, hit) = render_directory_browse_24(&mut app, width);
            assert_refresh_visible(&rows, info, hit, &app);
            for label in ["Folders", "Files", "Audio files", "Total size"] {
                assert!(rows.iter().any(|row| row.contains(label)),
                    "{label} must remain visible at width {width}: {rows:?}");
            }
            assert!(rows.iter().any(|row| row.trim() == "Refresh"),
                "button is separate from the metric value column: {rows:?}");
        }
    }

    /// The real Browse viewport is eight lines high at 80x24. Add the same
    /// two-line capacity reading produced by push_directory_stats_lines so
    /// compact selection is tested against physically wrapped optional rows.
    fn info_with_wrapped_disk_space(
        entry: &BrowseEntry,
        browse: &BrowseState,
        accounting: &crate::directory_accounting::AccountingService,
    ) -> InfoContent {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug());
        let mut info = entry_info_lines(entry, browse, accounting, 22,
            false, false, false, false, None, None, theme);
        let updated = directory_info_metric_block(&info, "Updated").unwrap().0;
        let disk = vec![
            vec![Span::styled("   Disk free   ", theme.muted())],
            vec![Span::raw("   "), Span::styled("█████░ 2.66T/16.30T",
                Style::default().fg(theme.cyan))],
        ];
        info.lines.splice(updated..updated, disk);
        if let Some(row) = &mut info.refresh_pill_row { *row += 2; }
        info
    }

    fn assert_counted_rows_present(info: &InfoContent) {
        let rows = flatten_lines(info);
        for label in ["Folders", "Files", "Audio files", "Total size"] {
            assert!(rows.iter().any(|row| row.contains(label)),
                "{label} must remain in the compact viewport: {rows:?}");
        }
    }

    #[test]
    fn eighty_by_twenty_four_keeps_all_metrics_with_disk_free_and_updated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Collection");
        std::fs::create_dir(&path).unwrap();
        let modified = Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400));
        let entry = BrowseEntry::new(path, "Collection".into(), EntryKind::Directory, 0, modified);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let full = info_with_wrapped_disk_space(&entry, &browse, &accounting);
        let full_rows = flatten_lines(&full);
        assert!(full_rows.iter().any(|r| r.contains("Disk free")));
        assert!(full_rows.iter().any(|r| r.contains("Updated")));
        let compact = compact_directory_info(full, 8, 22);
        assert!(compact.lines.len() <= 8);
        assert_counted_rows_present(&compact);
        let rows = flatten_lines(&compact);
        assert!(rows[0].starts_with(" Collection"), "stationary title: {rows:?}");
        assert!(rows.iter().any(|r| r.contains("Refresh")));
        assert!(compact.refresh_pill_row.is_some());
        assert_eq!(compact.lines[compact.refresh_pill_row.unwrap() - 1].len(), 0);
    }

    #[test]
    fn eighty_by_twenty_four_keeps_running_spinners_size_and_cancel() {
        use crate::directory_accounting::{AccountingService, Counts, ScanProgress};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Very Long Artist Name And Very Long Album Name");
        std::fs::create_dir(&path).unwrap();
        let modified = Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400));
        let entry = BrowseEntry::new(path.clone(),
            "Very Long Artist Name And Very Long Album Name".into(),
            EntryKind::Directory, 0, modified);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let mut accounting = AccountingService::new(None);
        let view = accounting.views.entry(path).or_default();
        view.local = Some(Counts { folders: 8, ..Default::default() });
        view.running = true;
        view.progress = Some(ScanProgress {
            folders_discovered: 8, files_examined: 77, audio_examined: 70,
            bytes_examined: 2 * 1024 * 1024 * 1024, ..Default::default()
        });
        let full = info_with_wrapped_disk_space(&entry, &browse, &accounting);
        let compact = compact_directory_info(full, 8, 22);
        assert!(compact.lines.len() <= 8);
        assert_counted_rows_present(&compact);
        let rows = flatten_lines(&compact);
        assert!(rows[0].starts_with(" Very Long Artist"), "stationary title: {rows:?}");
        assert!(rows.iter().any(|r| r.contains("Cancel")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("Refresh")));
        assert!(rows.iter().any(|r| r.contains("2.00 GiB…")), "{rows:?}");
        let spinner_rows = compact.lines.iter().filter(|row|
            row.iter().any(|span| span.style.fg == Some(theme().purple))).count();
        assert_eq!(spinner_rows, 3, "Files, Audio files and Total size are scanning");
        let button = compact.refresh_pill_row.unwrap();
        assert!(rows[button].contains("Cancel"));
    }

    #[test]
    fn title_wrap_reserves_one_header_row_without_moving_metrics_or_buttons() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Collection");
        std::fs::create_dir(&path).unwrap();
        let entry = BrowseEntry::new(path, "Collection".into(), EntryKind::Directory, 0, None);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let mut info = entry_info_lines(&entry, &browse, &accounting, 22,
            false, false, false, false, None, None, theme());
        let original_metrics = flatten_lines(&info).into_iter().skip(4).collect::<Vec<_>>();
        let refresh = info.refresh_pill_row;
        let height = info.lines.len() + 1;
        assert!(height <= MEDIA_COMPACT_MAX_CONTENT_ROWS,
            "plain directories can wrap if the full view fits even at a modest height");
        info.title = Some("At Fillmore East".into());
        let cold = wrap_full_info_title(info.clone(), 22, theme());
        assert!(flatten_lines(&cold)[1].is_empty(), "short title reserves the slot");
        info.title = Some("At Fillmore East New York 1971".into());
        let warm = wrap_full_info_title(info.clone(), 22, theme());
        let rows = flatten_lines(&warm);
        assert_eq!(rows[0].trim(), "At Fillmore East New");
        assert_eq!(rows[1].trim(), "York 1971");
        assert_eq!(warm.lines.len(), cold.lines.len());
        assert_eq!(warm.refresh_pill_row, refresh);
        assert_eq!(rows.into_iter().skip(4).collect::<Vec<_>>(), original_metrics);
        assert_eq!(flatten_lines(&cold).into_iter().skip(4).collect::<Vec<_>>(),
            original_metrics);
        let compact = compact_directory_info(info, 8, 22);
        assert!(compact.lines.len() <= 8);
        assert!(flatten_lines(&compact)[0].starts_with(" Collection"),
            "compact title remains single-line and truncated");
        assert!(compact.refresh_pill_row.is_some(), "compact action must remain");
    }

    #[test]
    fn long_flac_title_stays_wrapped_when_probe_overflows_info_viewport() {
        use ratatui::{backend::TestBackend, Terminal};

        let temp = tempfile::tempdir().unwrap();
        let name = "A Very Long Live Recording From Fillmore East In New York 1971.flac";
        let path = temp.path().join(name);
        std::fs::write(&path, b"FLAC fixture").unwrap();
        let entry = BrowseEntry::new(path.clone(), name.to_owned(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac), 12, None);
        let mut app = directory_layout_app(entry.clone());
        let mut cold_header: Option<Vec<String>> = None;

        for probed in [false, true] {
            if probed {
                app.browse.insert_probe_for_identity(path.clone(),
                    crate::tui::browse::ProbeCacheIdentity::from_entry(&entry),
                    Some(std::sync::Arc::new(CachedInfo {
                        source: crate::tui::probe::SourceInfo {
                            format_name: "FLAC".into(), codec: "FLAC".into(),
                            bit_depth: Some(24), sample_format_is_float: Some(false),
                            compression_is_lossless: Some(true), sample_rate: 96_000,
                            channels: 2, channel_layout: "stereo".into(),
                            duration_secs: 240.0, file_size: 12,
                        },
                        metadata: crate::tui::probe::SourceMetadata {
                            title: Some("Live recording".into()),
                            artist: Some("The performing artist".into()),
                            album: Some("The concert album".into()),
                            genre: Some("Jazz".into()), year: Some("1971".into()),
                            ..Default::default()
                        },
                        transport: None,
                    })));
            }

            let full = entry_info_lines(&entry, &app.browse, &app.directory_accounting,
                46, false, false, false, false, None, None, theme());
            assert_eq!(full.lines.len() > 14, probed,
                "the probe must cross the fourteen-row viewport boundary");
            let body = flatten_lines(&full)[4].clone();
            let (analyze, edit_tags, metadata_rows) = (full.analyze_pill_row,
                full.edit_tags_pill_row, full.meta_field_rows.clone());
            let original_len = full.lines.len();
            let wrapped = wrap_full_info_title(full, 46, theme());
            let wrapped_rows = flatten_lines(&wrapped);
            assert_eq!(wrapped_rows.len(), original_len);
            assert!(wrapped_rows[1].trim().contains("1971.flac"),
                "second title row disappeared after probe: {wrapped_rows:?}");
            assert_eq!(wrapped_rows[4], body, "the first body row moved");
            assert_eq!(wrapped.analyze_pill_row, analyze);
            assert_eq!(wrapped.edit_tags_pill_row, edit_tags);
            assert_eq!(wrapped.meta_field_rows, metadata_rows);

            let mut buttons = ButtonRenderMap::new();
            let mut terminal = Terminal::new(TestBackend::new(48, 16)).unwrap();
            terminal.draw(|frame| draw_browse_info(frame, Rect::new(0, 0, 48, 16),
                &app.browse, &app.directory_accounting, None, None,
                &mut buttons, None, theme())).unwrap();
            let rendered = (1..15).map(|y| (1..47).map(|x|
                terminal.backend().buffer().get(x, y).symbol().to_owned())
                .collect::<Vec<_>>().join("")).collect::<Vec<_>>();
            assert_eq!(rendered[0].trim(), wrapped_rows[0].trim());
            assert_eq!(rendered[1].trim(), wrapped_rows[1].trim());
            assert_eq!(rendered[4].trim(), body.trim());
            if let Some(header) = &cold_header {
                assert_eq!(&rendered[..2], header.as_slice(), "header changed after probe");
            } else {
                cold_header = Some(rendered[..2].to_vec());
            }
            if !probed {
                let rect = buttons.find_button_rect(&TuiButton::BrowseInfoAnalyze)
                    .expect("cold FLAC Analyze hitbox");
                assert_eq!(rect.y as usize, analyze.unwrap() + 1);
                assert_eq!(buttons.find_button_at(rect.x, rect.y),
                    Some(TuiButton::BrowseInfoAnalyze));
            }

            let (browse_rows, area, _) = render_directory_browse_size(&mut app, 160, 30);
            assert_eq!((area.width, area.height), (48, 16));
            assert_eq!(&browse_rows[..2], &rendered[..2]);
            assert_eq!(browse_rows[4], rendered[4]);
        }
    }

    #[test]
    fn wrapped_title_respects_display_columns_and_limits_to_two_rows() {
        for title in ["Wish You Were Here: 50th Anniversary Edition",
            "東京 音楽 日本語 作品", "e\u{301}lan vital remastered at Fillmore"] {
            let (first, second) = wrapped_info_title(title, 20);
            assert!(super::super::display_width::width(&first) <= 20, "{first}");
            if let Some(second) = second {
                assert!(super::super::display_width::width(&second) <= 20, "{second}");
            }
        }
        let (first, second) = wrapped_info_title("At Fillmore East", 30);
        assert_eq!(first, "At Fillmore East");
        assert!(second.is_none());
    }

    #[test]
    fn full_height_keeps_directory_accounting_below_stationary_title() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Long Name With Several Wrapped Words");
        std::fs::create_dir(&path).unwrap();
        let modified = Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400));
        let entry = BrowseEntry::new(path, "Long Name With Several Wrapped Words".into(),
            EntryKind::Directory, 0, modified);
        let mut browse = BrowseState::new();
        browse.entries = vec![entry.clone()];
        let accounting = crate::directory_accounting::AccountingService::new(None);
        let full = info_with_wrapped_disk_space(&entry, &browse, &accounting);
        let rows = flatten_lines(&full);
        assert!(rows[0].starts_with(" Long Name"), "{rows:?}");
        assert!(rows[1..4].iter().all(|row| row.is_empty()), "{rows:?}");
        let positions = ["Kind", "Folders", "Files", "Audio files", "Total size",
            "Disk free", "Updated", "Refresh"].iter().map(|label|
            rows.iter().position(|row| row.contains(label)).unwrap()).collect::<Vec<_>>();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(positions[0], 4);
        assert!(rows[positions[6] + 1].contains(entry.date_label().as_str()));
        assert!(rows[positions[7] - 1].trim().is_empty());
    }

    #[test]
    fn long_folder_and_date_do_not_displace_compact_metrics_or_hitbox() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("A Very Long Folder Name With Several Wrapped Words");
        std::fs::create_dir(&path).unwrap();
        let entry = BrowseEntry::new(path, "A Very Long Folder Name With Several Wrapped Words".into(),
            EntryKind::Directory, 0,
            Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400)));
        let mut app = directory_layout_app(entry);
        let (rows, info, hit) = render_directory_browse_24(&mut app, 80);
        assert_refresh_visible(&rows, info, hit, &app);
        for label in ["Folders", "Files", "Audio files", "Total size"] {
            assert!(rows.iter().any(|r| r.contains(label)),
                "{label} disappeared at 80x24: {rows:?}");
        }
    }

    #[test]
    fn compact_directory_keeps_title_at_66_and_80_for_counts_progress_and_flac_switch() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Collection");
        std::fs::create_dir(&path).unwrap();
        let entry = BrowseEntry::new(path.clone(), "Collection".into(),
            EntryKind::Directory, 0,
            Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86_400)));
        for width in [66u16, 80] {
            let mut app = directory_layout_app(entry.clone());
            let view = app.directory_accounting.views.get_mut(&path).unwrap();
            view.local = None;
            for stage in 0..4 {
                if stage == 1 {
                    app.directory_accounting.views.get_mut(&path).unwrap().local =
                        Some(crate::directory_accounting::Counts {
                            folders: 3, ..Default::default()
                        });
                } else if stage == 2 {
                    let view = app.directory_accounting.views.get_mut(&path).unwrap();
                    view.running = true;
                    view.progress = Some(crate::directory_accounting::ScanProgress {
                        folders_discovered: 3, files_examined: 7, audio_examined: 5,
                        bytes_examined: 2 * 1024 * 1024, ..Default::default()
                    });
                } else if stage == 3 {
                    let view = app.directory_accounting.views.get_mut(&path).unwrap();
                    view.running = false;
                    view.cached = Some(fake_measurement(&path, 2 * 1024 * 1024));
                }
                let (rows, area, hit) = render_directory_browse_24(&mut app, width);
                assert!(rows[0].starts_with(" Collection"),
                    "width={width}, stage={stage}, rows={rows:?}");
                for label in ["Folders", "Files", "Audio files", "Total size"] {
                    assert!(rows.iter().any(|r| r.contains(label)),
                        "width={width}, stage={stage}, missing {label}: {rows:?}");
                }
                if stage < 2 {
                    assert_refresh_visible(&rows, area, hit, &app);
                } else if stage == 2 {
                    let hit = hit.expect("Cancel hitbox");
                    assert!(rows[(hit.y - area.y - 1) as usize].contains("Cancel"));
                    assert_eq!(app.button_map.find_button_at(hit.x, hit.y),
                        Some(TuiButton::BrowseInfoRefresh));
                } else {
                    assert!(hit.is_none(), "complete measurement has no Refresh");
                }
            }
            // An unprobed FLAC selection shares the exact same title origin.
            let flac = BrowseEntry::new(path.join("track.flac"), "track.flac".into(),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac), 0, None);
            app.browse.entries = vec![flac];
            app.browse.selected_index = 0;
            let (rows, _, _) = render_directory_browse_24(&mut app, width);
            assert!(rows[0].starts_with(" track.flac"),
                "FLAC title shifted at width={width}: {rows:?}");
        }
    }

    #[test]
    fn scanning_cancel_hitbox_tracks_compacted_painted_button() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("A Very Long Folder Name With Several Wrapped Words");
        std::fs::create_dir(&path).unwrap();
        let entry = BrowseEntry::new(path.clone(),
            "A Very Long Folder Name With Several Wrapped Words".into(),
            EntryKind::Directory, 0,
            Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400)));
        let mut app = directory_layout_app(entry);
        let view = app.directory_accounting.views.get_mut(&path).unwrap();
        view.running = true;
        view.progress = Some(crate::directory_accounting::ScanProgress {
            files_examined: 77, audio_examined: 70,
            bytes_examined: 2 * 1024 * 1024 * 1024, ..Default::default()
        });
        let (rows, area, hit) = render_directory_browse_24(&mut app, 80);
        let hit = hit.expect("Cancel is visible");
        assert_eq!(hit.width, 8);
        assert!(hit.x > area.x && hit.right() < area.right());
        assert!(hit.y > area.y && hit.bottom() < area.bottom());
        assert!(rows[(hit.y - area.y - 1) as usize].contains("Cancel"));
        assert_eq!(app.button_map.find_button_at(hit.x, hit.y), Some(TuiButton::BrowseInfoRefresh));
        for label in ["Folders", "Files", "Audio files", "Total size"] {
            assert!(rows.iter().any(|r| r.contains(label)),
                "{label} disappeared while scanning: {rows:?}");
        }
        assert!(rows.iter().any(|r| r.contains("2.00 GiB…")));
    }

    fn example_classification(
        entry: &BrowseEntry,
        kind: FolderClassificationKind,
    ) -> FolderContentClassification {
        FolderContentClassification {
            kind,
            identity: crate::tui::browse::ProbeCacheIdentity::from_entry(entry),
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 2,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(crate::tui::browse::FolderDiscMarkerKind::DvdAudio),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: Default::default(),
        }
    }

    #[test]
    fn classified_album_and_disc_details_follow_visible_accounting() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("Album");
        std::fs::create_dir(&child).unwrap();
        let entry = BrowseEntry::new(child.clone(), "Album".into(), EntryKind::Directory, 0, None);
        for kind in [FolderClassificationKind::Album, FolderClassificationKind::Disc,
            FolderClassificationKind::MultiDisc] {
            let mut app = directory_layout_app(entry.clone());
            app.browse.insert_folder_classification_for_test(
                child.clone(), example_classification(&entry, kind),
            );
            let (rows, info, hit) = render_directory_browse_24(&mut app, 80);
            assert!(rows.iter().any(|row| row.contains("Audio") || row.contains("Discs")),
                "{kind:?}: {rows:?}");
            assert!(rows.iter().any(|row| row.contains("Size")), "{kind:?}: {rows:?}");
            let hit = hit.expect("media Refresh visible");
            assert!(hit.x > info.x && hit.right() < info.right());
            assert_eq!(app.button_map.find_button_at(hit.x, hit.y),
                Some(TuiButton::BrowseInfoRefresh));
        }
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let mut app = directory_layout_app(BrowseEntry::new(
                child.clone(), "Album".into(), kind, 0, None,
            ));
            let (rows, info, hit) = render_directory_browse_24(&mut app, 80);
            assert_refresh_visible(&rows, info, hit, &app);
        }
    }

    #[test]
    fn album_reference_layout_survives_compact_browse_height() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Album Collection");
        std::fs::create_dir(&folder).unwrap();
        let modified = Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86_400));
        let entry = BrowseEntry::new(folder.clone(), "Album Collection".into(),
            EntryKind::Directory, 0, modified);
        let mut app = directory_layout_app(entry.clone());
        let track = folder.join("track.flac");
        let mut classification = example_classification(&entry, FolderClassificationKind::Album);
        classification.audio.track_count = 1;
        classification.audio.format_counts.insert("FLAC".to_string(), 1);
        classification.audio.file_paths.push(track.clone());
        app.browse.insert_folder_classification_for_identity(
            folder, crate::tui::browse::ProbeCacheIdentity::from_entry(&entry), classification);
        app.browse.insert_probe_cache_for_test(track, Some(std::sync::Arc::new(
            crate::tui::browse::CachedInfo {
                source: crate::tui::probe::SourceInfo {
                    sample_format_is_float: Some(false), compression_is_lossless: Some(true),
                    format_name: "FLAC".into(), codec: "flac".into(), bit_depth: Some(16),
                    sample_rate: 44_100, channels: 2, channel_layout: "stereo".into(),
                    duration_secs: 90.0, file_size: 1234,
                }, metadata: crate::tui::probe::SourceMetadata::default(), transport: None,
            }
        )));
        let (drawn, area, hit) = render_directory_browse_size(&mut app, 80, 30);
        assert_eq!(drawn.len(), 14);
        assert!(drawn.iter().any(|row| row.contains("1 track")), "{drawn:?}");
        assert!(drawn.iter().any(|row| row.contains("Contents")), "{drawn:?}");
        assert!(drawn.iter().any(|row| row.contains("Size")), "{drawn:?}");
        assert_eq!(app.button_map.find_button_at(hit.unwrap().x, hit.unwrap().y),
            Some(TuiButton::BrowseInfoRefresh));
        assert_eq!(area.height, 16);

        let full = entry_info_lines(&entry, &app.browse, &app.directory_accounting,
            22, false, false, false, false, None, None, theme());
        assert!(flatten_lines(&full).join("\n").contains("1m 30s"));
        let compact = compact_media_info(full, 7);
        let rows = flatten_lines(&compact);
        assert!(rows.iter().any(|row| row.contains("1 track")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("Refresh")), "{rows:?}");
        assert!(compact.refresh_pill_row.is_some());
    }

    #[test]
    fn disc_probe_status_and_refresh_survive_compact_media_view() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Disc");
        std::fs::create_dir(&folder).unwrap();
        let entry = BrowseEntry::new(folder.clone(), "Disc".into(),
            EntryKind::Directory, 0, Some(std::time::SystemTime::UNIX_EPOCH
                + Duration::from_secs(86_400)));
        let mut app = directory_layout_app(entry.clone());
        app.browse.insert_folder_classification_for_identity(
            folder, crate::tui::browse::ProbeCacheIdentity::from_entry(&entry),
            example_classification(&entry, FolderClassificationKind::Disc));
        let full = entry_info_lines(&entry, &app.browse, &app.directory_accounting,
            22, false, false, false, false, None, None, theme());
        let full_rows = flatten_lines(&full);
        assert!(full_rows.iter().map(|row| row.trim())
            .collect::<Vec<_>>().join(" ").contains("disc metadata not cached"),
            "status must survive whitespace wrapping: {full_rows:?}");
        assert!(full_rows.iter().any(|row| row.contains("12 files · 2 folders")),
            "count+noun rows must not split: {full_rows:?}");
        assert!(full_rows.iter().any(|row| row.contains("Modified  1970-01-01")),
            "narrow date should fit on one row: {full_rows:?}");
        let compact = compact_media_info(full, 6);
        let rows = flatten_lines(&compact);
        assert!(rows.iter().any(|row| row.contains("Audio")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("Refresh")), "{rows:?}");
        assert!(compact.refresh_pill_row.is_some());
        assert!(compact.audio_streams_pill_row.is_none());
    }

    #[test]
    fn measured_total_fits_24_row_info_without_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("Collection");
        std::fs::create_dir(&child).unwrap();
        let mut app = directory_layout_app(BrowseEntry::new(
            child.clone(), "Collection".into(), EntryKind::Directory, 0, None,
        ));
        app.directory_accounting.views.get_mut(&child).unwrap().cached =
            Some(fake_measurement(&child, 1024u128 * 1024 * 1024));
        let (rows, _info, hit) = render_directory_browse_24(&mut app, 80);
        assert!(hit.is_none(), "completed total has no Refresh action");
        assert!(rows.iter().any(|row| row.contains("Total size")));
        assert!(rows.iter().any(|row| row.contains("1.00 GiB")),
            "complete value should wrap, not be clipped: {rows:?}");
        assert!(!rows.iter().any(|row| row.contains("Refresh")));
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlink_keeps_indicator_but_never_offers_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("real");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("linked");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let entry = BrowseEntry::new_with_symlink(
            link, "linked".into(), EntryKind::Directory, 0, None, true, false,
        );
        let mut app = directory_layout_app(entry.clone());
        let info = directory_info_with_service(&entry, &app.browse, &app.directory_accounting);
        assert!(info.refresh_pill_row.is_none());
        assert!(!flatten_lines(&info).iter().any(|row| row.contains("Refresh")));
        let (rows, _area, hit) = render_directory_browse_24(&mut app, 80);
        assert!(hit.is_none());
        assert!(!rows.iter().any(|row| row.contains("Refresh")));
        assert!(rows.iter().any(|row| row.contains("symlink")),
            "existing symlink indication should remain visible");

        let ordinary = BrowseEntry::new(
            target, "real".into(), EntryKind::Directory, 0, None,
        );
        let mut ordinary_app = directory_layout_app(ordinary);
        let (rows, info, hit) = render_directory_browse_24(&mut ordinary_app, 80);
        assert_refresh_visible(&rows, info, hit, &ordinary_app);
    }

    #[test]
    fn binary_size_formatter_does_not_call_gibibytes_gigabytes() {
        assert_eq!(directory_size_str(1u64 << 40), "1.00 TiB");
        assert_eq!(directory_size_str(1u64 << 30), "1.00 GiB");
        assert_eq!(directory_size_str(0), "0 B");
    }
    #[test]
    fn archive_navigation_rows_do_not_claim_filesystem_directory_stats() {
        let archive_path = std::path::PathBuf::from("/tmp/archive.zip");
        let mut browse = BrowseState::new();
        browse.archive = Some(crate::tui::browse::ArchiveBrowseState {
            listing: crate::tui::archive_listing::ArchiveListing {
                archive_path: archive_path.clone(),
                format: "zip".to_string(),
                physical_size: 1024,
                entries: Vec::new(),
            },
            inner_path: "inside".to_string(),
            password: None,
            staging: None,
        });
        let parent = BrowseEntry::new(
            archive_path.clone(), "..".into(), EntryKind::ParentDir, 0, None,
        );
        browse.entries = vec![parent.clone()];
        let parent_render = render(&parent, &browse);
        assert!(parent_render.contains("parent directory"));
        assert!(!parent_render.contains("Disk free"));
        assert!(!parent_render.contains("Contents size"));

        let child = BrowseEntry::new(
            archive_path.join("inside").join("folder"),
            "folder".into(), EntryKind::Directory, 0, None,
        );
        browse.entries = vec![child.clone()];
        let child_render = render(&child, &browse);
        assert!(child_render.contains("directory"));
        assert!(!child_render.contains("Disk free"));
        assert!(!child_render.contains("Contents size"));
    }

}

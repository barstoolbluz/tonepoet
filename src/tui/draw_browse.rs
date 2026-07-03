//! Browse screen: file browser with directory tree + info pane

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::{AppState, BrowseInfoFocus, BrowseInlineEditState, BrowseInlineEditTarget};
use super::browse::{BrowseColumn, BrowseEntry, BrowseOptionsMenu, BrowsePaneId, BrowseState, CachedInfo, EntryKind, FormatFilter, SortBy, SortDir};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::inline_edit::{inline_cursor_col, render_inline_value_with_embedded_cursor};
use super::probe::MetadataField;
use super::text_input::TextInputState;

/// Fixed column widths (inside the list border). Name is flexible.
const COL_SIZE_W: usize = 9;
const COL_DATE_W: usize = 12;
const COL_TYPE_W: usize = 8;
/// Prefix: cursor(2) + check(1) + space(1).
const ROW_PREFIX: usize = 4;
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

/// Draw the full browse screen
pub fn draw_browse_screen(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    app.browse.last_render_area = Some(area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7), // header banner
            Constraint::Length(5), // toolbar + path bar (two boxes, shared middle border)
            Constraint::Min(10),   // three-pane browse content
            Constraint::Length(2), // footer (tabs + context)
        ])
        .split(area);

    draw_header(f, chunks[0], theme);
    draw_browse_toolbar(f, chunks[1], app, theme);

    let content_chunks = browse_content_layout(chunks[2], &app.browse);
    let explore_area = content_chunks[0];
    let list_area = content_chunks[1];
    let info_area = content_chunks[2];

    let hover = app.hover_target;
    let inline_edit = app.browse_inline_edit.clone();

    if app.browse.explore_enabled {
        if app.browse.explore_collapsed {
            draw_collapsed_pane(f, explore_area, BrowsePaneId::Explore, "explore", &mut app.button_map, theme);
        } else {
            draw_explore_pane(f, explore_area, &mut app.browse, &mut app.button_map, hover, theme);
        }
    }

    draw_browse_list(f, list_area, &mut app.browse, inline_edit.as_ref(), hover, theme);
    register_browse_buttons(&mut app.button_map, list_area, &app.browse);
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

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[3],
        app.current_screen,
        &mut app.button_map,
        status_msg,
        theme,
    );

    if app.browse.options_menu.is_open() {
        draw_options_menu(
            f,
            chunks[1],
            area,
            &app.browse,
            &app.config.performance.browsing.archive_listing,
            &mut app.button_map,
            app.hover_target,
            theme,
        );
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

    // Render path bar on line 3, inside the borders
    let path_area = Rect::new(area.x + 1, area.y + 3, area.width.saturating_sub(2), 1);
    draw_breadcrumb_inline(f, path_area, &app.browse, theme);
    app.button_map.record_button(TuiButton::BrowseBreadcrumb, path_area);
    if path_area.width > 6 {
        let go = Rect::new(path_area.x + path_area.width - 5, path_area.y, 5, 1);
        f.render_widget(Paragraph::new(" Go ").style(browse_toolbar_button_style(theme)), go);
        app.button_map.record_button(TuiButton::BrowsePathGo, go);
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
    let width = label.chars().count() as u16;
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
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
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
    let title_w = title.chars().count();
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
    let inner = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), content_height as u16);

    // Render top border, side borders, and bottom border
    f.render_widget(Paragraph::new(top_line), Rect::new(area.x, area.y, area.width, 1));
    for row in 0..content_height {
        let y = area.y + 1 + row as u16;
        f.render_widget(Paragraph::new("│").style(theme.border(border_color)), Rect::new(area.x, y, 1, 1));
        f.render_widget(Paragraph::new("│").style(theme.border(border_color)), Rect::new(area.x + area.width - 1, y, 1, 1));
    }
    f.render_widget(Paragraph::new(bot_line), Rect::new(area.x, area.y + area.height - 1, area.width, 1));

    buttons.record_button(TuiButton::BrowsePaneToggle(BrowsePaneId::Explore), Rect::new(area.x + 1, area.y, title_w as u16, 1));

    browse.set_tree_visible_height(inner.height as usize);
    let start = browse.tree_scroll;
    let end = (start + inner.height as usize).min(browse.tree_nodes.len());
    let lines = browse.tree_nodes[start..end]
        .iter()
        .enumerate()
        .map(|(row, node)| {
            let absolute = start + row;
            render_browse_tree_node_line(
                node,
                absolute == browse.tree_cursor,
                hover == Some(TuiButton::BrowseTreeNode(absolute)),
                theme,
            )
        })
        .collect::<Vec<_>>();
    for (row, absolute) in (start..end).enumerate() {
        buttons.record_button(
            TuiButton::BrowseTreeNode(absolute),
            Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
        );
    }
    f.render_widget(Paragraph::new(lines), inner);
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
        style = style.bg(theme.selection_bg).fg(theme.bg);
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
    toolbar_area: Rect,
    screen_area: Rect,
    browse: &BrowseState,
    archive_listing_mode: &str,
    buttons: &mut ButtonRenderMap,
    hover: Option<TuiButton>,
    theme: super::theme::Theme,
) {
    let root_rows = options_root_rows(browse);
    let geometry = options_menu_geometry_for_area(
        toolbar_area,
        screen_area,
        browse,
        archive_listing_mode,
    );
    let active_parent = active_options_parent_button(browse.options_menu);

    render_options_menu_panel(
        f,
        geometry.root_area,
        "Options",
        &root_rows,
        buttons,
        hover,
        active_parent,
        theme,
    );

    if let (Some((title, submenu_rows)), Some(submenu_area)) = (
        options_submenu_rows(browse, archive_listing_mode),
        geometry.submenu_area,
    ) {
        render_options_menu_panel(
            f,
            submenu_area,
            title,
            &submenu_rows,
            buttons,
            hover,
            None,
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

pub(super) fn options_menu_geometry_for_area(
    toolbar_area: Rect,
    screen_area: Rect,
    browse: &BrowseState,
    archive_listing_mode: &str,
) -> OptionsMenuGeometry {
    let root_rows = options_root_rows(browse);
    let root_width = options_menu_panel_width("Options", &root_rows, toolbar_area.width);
    let root_height = root_rows.len() as u16 + 2;
    let preferred_x = toolbar_area.x.saturating_add(30);
    let root_y = toolbar_area.y.saturating_add(1);
    let root_x = clamp_menu_x(preferred_x, root_width, toolbar_area);
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

fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn options_root_rows(browse: &BrowseState) -> Vec<(String, Option<TuiButton>)> {
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

fn active_options_parent_button(menu: BrowseOptionsMenu) -> Option<TuiButton> {
    match menu {
        BrowseOptionsMenu::Layout => Some(TuiButton::BrowseOptionsLayout),
        BrowseOptionsMenu::Columns => Some(TuiButton::BrowseOptionsColumns),
        BrowseOptionsMenu::Sort => Some(TuiButton::BrowseOptionsSort),
        BrowseOptionsMenu::Filter => Some(TuiButton::BrowseOptionsFilter),
        BrowseOptionsMenu::ArchiveListing => Some(TuiButton::BrowseOptionsArchiveListing),
        BrowseOptionsMenu::Root | BrowseOptionsMenu::Closed => None,
    }
}

fn options_submenu_rows(
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
        .map(|(row, _)| row.chars().count())
        .max()
        .unwrap_or(12)
        .max(title.chars().count() + 4) as u16;
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
    let target = width as usize;
    let mut fitted = row.chars().take(target).collect::<String>();
    let current = fitted.chars().count();
    if current < target {
        fitted.push_str(&" ".repeat(target - current));
    }
    fitted
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

        let geometry = options_menu_geometry_for_area(toolbar, screen, &browse, "auto");
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

        let geometry = options_menu_geometry_for_area(toolbar, screen, &browse, "auto");
        let submenu = geometry.submenu_area.expect("layout submenu");

        assert!(geometry.root_area.x >= screen.x);
        assert_eq!(geometry.root_area.y, screen.y + 8);
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

fn browse_entry_y_start(area: Rect, search_active: bool) -> u16 {
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
    label.chars().count()
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
fn register_browse_buttons(buttons: &mut ButtonRenderMap, area: Rect, browse: &BrowseState) {
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

    // Entry rows: below header (and search panel if active), above bottom border.
    let entry_y_start = browse_entry_y_start(area, browse.search.active);
    let content_height = (area.height as usize).saturating_sub(3 + search_rows as usize);
    let start = browse.scroll_offset;
    let end = (start + content_height).min(browse.entries.len());
    for (row, i) in (start..end).enumerate() {
        let y = entry_y_start + row as u16;
        buttons.record_button(
            TuiButton::BrowseEntry(i),
            Rect::new(area.x + 1, y, (inner_w) as u16, 1),
        );
    }
}

fn render_path_input_spans(
    input: &TextInputState,
    inner_width: usize,
    theme: super::theme::Theme,
) -> (Vec<Span<'static>>, u16) {
    let prefix = " path: ";
    let prefix_w = prefix.chars().count();
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
    let prefix_w = prefix.chars().count();
    let suffix_w = filter_suffix.chars().count() + type_ahead_suffix.chars().count();
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
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        return s
            .chars()
            .rev()
            .take(max)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }
    let skip = count - (max - 1);
    let truncated: String = s.chars().skip(skip).collect();
    format!("…{}", truncated)
}

/// Draw the directory listing (left pane) with a sortable column header row.
/// Reserves an extra row for the live filter input when one is active.
fn draw_browse_list(
    f: &mut Frame,
    area: Rect,
    browse: &mut BrowseState,
    inline_edit: Option<&BrowseInlineEditState>,
    hover: Option<super::button_map::TuiButton>,
    theme: super::theme::Theme,
) {
    if area.height < 4 || area.width < 20 {
        return;
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
    let search_display_w = search_label.chars().count();
    let title_w = title.chars().count();
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

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

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
    browse.visible_height = content_height;

    let column_layout = browse_column_layout(inner_w, &browse.columns);
    let name_w = name_column_width(&column_layout);

    let mut lines: Vec<Line> = vec![top_line];

    // Search panel (2 rows when active): input first, then peer controls.
    if browse.search.active {
        // Row 1: full-width search input.
        // Layout: │ + " / "(3) + input(input_w) + │
        let input_w = inner_w.saturating_sub(3);
        let (view, _cursor_col) = browse.search.input.view(input_w);
        let view_len = view.chars().count();
        let padded = if view.is_empty() {
            " ".repeat(input_w.max(1))
        } else {
            let pad = input_w.saturating_sub(view_len);
            format!("{}{}", view, " ".repeat(pad))
        };
        lines.push(Line::from(vec![
            Span::styled("│", theme.border(border_color)),
            Span::styled(" / ", Style::default().fg(theme.amber)),
            Span::styled(
                padded,
                Style::default().fg(theme.text_bright).bg(theme.surface),
            ),
            Span::styled("│", theme.border(border_color)),
        ]));

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
            lines.push(bordered_line(border_color, w, vec![], theme));
        }
    } else if browse.entries.is_empty() {
        let msg = if browse.scan_pending.is_some() {
            "   Loading..."
        } else {
            "   (empty)"
        };
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(msg, theme.muted())], theme));
        for _ in 1..content_height {
            lines.push(bordered_line(border_color, w, vec![], theme));
        }
    } else {
        let start = browse.scroll_offset;
        let end = (start + content_height).min(browse.entries.len());

        for i in start..end {
            let entry = &browse.entries[i];
            let is_selected = i == browse.selected_index;
            let is_checked = browse.is_multi_selected(&entry.path);
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
                is_hovered, theme));
        }

        let rendered = end - start;
        for _ in rendered..content_height {
            lines.push(bordered_line(border_color, w, vec![], theme));
        }
    }

    // Filter input row (just above the bottom border) when active.
    let mut filter_cursor: Option<u16> = None;
    if let Some(input) = &browse.filter_input {
        // Inside row layout: │ + " / " + <input view> + padding + │
        // Reserve 1 (left border) + 3 (" / ") + 2 (right padding + border) = 6
        let input_width = inner_w.saturating_sub(4); // " / " prefix takes 3 + 1 trailing space
        let (visible, cursor_col_in_view) = input.view(input_width);
        filter_cursor = Some(cursor_col_in_view);

        let visible_w = visible.chars().count();
        let pad = input_width.saturating_sub(visible_w);
        lines.push(Line::from(vec![
            Span::styled("│", theme.border(border_color)),
            Span::styled(" / ", Style::default().fg(theme.cyan)),
            Span::styled(visible, Style::default().fg(theme.text_bright)),
            Span::raw(" ".repeat(pad)),
            Span::raw(" "),
            Span::styled("│", theme.border(border_color)),
        ]));
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
}

/// Render the column header row with sort indicator (▲/▼) on the active column.
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

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::raw(" ".repeat(ROW_PREFIX)),
    ];

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

    // Pad any shortfall to reach the right border cleanly (safety net).
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used < width {
        let pad = width - used;
        let last = spans.pop().unwrap();
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(last);
    }

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

    // Multi-select checkbox
    let check = if is_checked { "●" } else { " " };
    let check_style = if is_checked {
        Style::default().fg(theme.cyan)
    } else {
        Style::default().fg(theme.text_dim)
    };

    let cached = cached_info_for_entry(browse, entry);
    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(cursor, cursor_style),
        Span::styled(check, check_style),
        Span::raw(" "),
    ];

    for (idx, cell) in columns.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        if cell.column == BrowseColumn::Name {
            if let Some(input) = inline_rename_input {
                spans.extend(render_inline_value_with_embedded_cursor(input, cell.width, theme));
            } else {
                let name_display = pad_or_truncate(&entry.name, cell.width, false);
                spans.push(Span::styled(name_display, entry_name_style(entry, is_selected, theme)));
            }
        } else {
            let value = entry_column_text(entry, cell.column, cached);
            let display = pad_or_truncate(&value, cell.width, column_right_aligned(cell.column));
            spans.push(Span::styled(display, entry_column_style(entry, cell.column, cached, theme)));
        }
    }

    spans.push(Span::raw(" ".repeat(ROW_TRAILING)));
    spans.push(Span::styled("│", theme.border(border_color)));

    // Pad any shortfall before the right border (safety net on narrow widths).
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used < width {
        let pad = width - used;
        let last = spans.pop().unwrap();
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(last);
    }

    // Selected row gets a subtle bg highlight; hovered row gets a dimmer one.
    let bg = if is_selected {
        Some(theme.border_dim)
    } else if is_hovered {
        Some(theme.hover_bg)
    } else {
        None
    };
    if let Some(bg_color) = bg {
        for span in spans.iter_mut() {
            if !matches!(span.content.as_ref(), "│") {
                span.style = span.style.bg(bg_color);
            }
        }
    }

    Line::from(spans)
}

fn cached_info_for_entry<'a>(browse: &'a BrowseState, entry: &BrowseEntry) -> Option<&'a CachedInfo> {
    browse
        .probe_cache
        .get(&entry.path)
        .and_then(|cached| cached.as_ref().map(|info| info.as_ref()))
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
    if is_audio_column(column) && !entry.is_audio() {
        return Style::default().fg(theme.text_dim);
    }
    if is_audio_column(column) && cached.is_none() {
        return Style::default().fg(theme.text_dim);
    }
    theme.muted()
}

fn entry_column_text(
    entry: &BrowseEntry,
    column: BrowseColumn,
    cached: Option<&CachedInfo>,
) -> String {
    match column {
        BrowseColumn::Name => entry.name.clone(),
        BrowseColumn::Size => match &entry.kind {
            EntryKind::ParentDir | EntryKind::Directory => String::new(),
            _ => size_str(entry.size),
        },
        BrowseColumn::Date => entry.date_label(),
        BrowseColumn::Type => entry.type_label(),
        BrowseColumn::Format => audio_column_value(entry, cached, |info| {
            non_empty(info.source.format_name.clone()).or_else(|| Some(entry.type_label()))
        }),
        BrowseColumn::Codec => audio_column_value(entry, cached, |info| {
            non_empty(info.source.codec_display())
        }),
        BrowseColumn::SampleRate => audio_column_value(entry, cached, |info| {
            (info.source.sample_rate > 0).then(|| info.source.sample_rate_display())
        }),
        BrowseColumn::Channels => audio_column_value(entry, cached, |info| {
            (info.source.channels > 0).then(|| info.source.channels_display())
        }),
        BrowseColumn::Duration => audio_column_value(entry, cached, |info| {
            (info.source.duration_secs.is_finite() && info.source.duration_secs > 0.0)
                .then(|| info.source.duration_display())
        }),
        BrowseColumn::Artist => audio_column_value(entry, cached, |info| {
            info.metadata.artist.clone().and_then(non_empty)
        }),
        BrowseColumn::Album => audio_column_value(entry, cached, |info| {
            info.metadata.album.clone().and_then(non_empty)
        }),
    }
}

fn audio_column_value<F>(
    entry: &BrowseEntry,
    cached: Option<&CachedInfo>,
    value: F,
) -> String
where
    F: FnOnce(&CachedInfo) -> Option<String>,
{
    if !entry.is_audio() {
        return "—".to_string();
    }
    cached.and_then(value).unwrap_or_else(|| "—".to_string())
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
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
    let count = s.chars().count();
    if count == width {
        return s.to_string();
    }
    if count > width {
        if width < 2 {
            return s.chars().take(width).collect();
        }
        let truncated: String = s.chars().take(width - 1).collect();
        return format!("{}…", truncated);
    }
    let pad = width - count;
    if right_align {
        format!("{}{}", " ".repeat(pad), s)
    } else {
        format!("{}{}", s, " ".repeat(pad))
    }
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
    let title_w = title.chars().count();
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
    let count = s.chars().count();
    if count <= max_chars || max_chars < 2 {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars - 1).collect();
    format!("{}…", truncated)
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
            lines.push(vec![
                Span::styled("   kind    ", theme.muted()),
                Span::styled("directory", theme.text_style()),
            ]);
            // Show directory stats if cached, or "computing…" if a stats
            // task is currently in flight for this directory.
            if let Some(stats) = browse.current_dir_stats() {
                lines.push(vec![
                    Span::styled("   files   ", theme.muted()),
                    Span::styled(stats.file_count.to_string(), theme.text_style()),
                ]);
                if stats.audio_count > 0 {
                    lines.push(vec![
                        Span::styled("   audio   ", theme.muted()),
                        Span::styled(stats.audio_count.to_string(), theme.accent()),
                    ]);
                }
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(size_str(stats.total_size), theme.text_style()),
                ]);
            } else if browse.dir_stats_pending.contains(&entry.path) {
                lines.push(vec![
                    Span::styled("   files   ", theme.muted()),
                    Span::styled("computing…", Style::default().fg(theme.text_dim)),
                ]);
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

                // Pre-emphasis — show if metadata evidence detected.
                if let Some(ref pe) = cached.metadata.preemphasis_metadata {
                    lines.push(vec![
                        Span::styled("   pre-emph", theme.muted()),
                        Span::raw(" "),
                        Span::styled(
                            truncate_to(
                                &format!("detected ({})", pe),
                                max_value_chars.saturating_sub(11),
                            ),
                            Style::default().fg(theme.destructive),
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
                let analyze_w = analyze_label.chars().count();
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
                let a_w = a_label.chars().count();
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
                let et_w2 = et_label.chars().count();
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
            let et_w = et_label.chars().count();
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
            if let Some(contents) = browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.contents_if_current(&entry.path))
            {
                lines.push(vec![
                    Span::styled("   streams ", theme.muted()),
                    Span::styled(crate::tui::disc_browser::disc_summary(contents.as_ref()), theme.text_style()),
                ]);
                lines.push(vec![
                    Span::styled("   copy protection", theme.muted()),
                    Span::raw(" "),
                    Span::styled(
                        truncate_to(&contents.copy_protection.description, max_value_chars.saturating_sub(18)),
                        theme.text_style(),
                    ),
                ]);
                lines.push(vec![]);
                for (idx, presentation) in contents.presentations.iter().enumerate().take(6) {
                    lines.push(vec![
                        Span::styled("   ", theme.muted()),
                        Span::styled(
                            truncate_to(&crate::tui::disc_browser::presentation_summary(idx, presentation), max_value_chars),
                            theme.text_style(),
                        ),
                    ]);
                }
                if contents.presentations.len() > 6 {
                    lines.push(vec![Span::styled(
                        format!("   … {} more audio streams", contents.presentations.len() - 6),
                        theme.muted(),
                    )]);
                }
                if contents.presentations.len() >= 2 {
                    lines.push(vec![]);
                    let row = lines.len();
                    let label = " audio streams ";
                    let width = label.chars().count();
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
                    audio_streams_pill_row = Some(row);
                }
            } else if let Some(error) = browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.error_if_current(&entry.path))
            {
                lines.push(vec![
                    Span::styled("   status  ", theme.muted()),
                    Span::styled(truncate_to(error, max_value_chars.saturating_sub(10)), Style::default().fg(theme.destructive)),
                ]);
                lines.push(vec![Span::styled("   size    ", theme.muted()), Span::styled(size_str(entry.size), theme.text_style())]);
            } else {
                lines.push(vec![Span::styled("   status  ", theme.muted()), Span::styled("Analyzing disc…", theme.muted())]);
                lines.push(vec![Span::styled("   size    ", theme.muted()), Span::styled(size_str(entry.size), theme.text_style())]);
            }
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
            if let Some(cached) = browse.current_cached_info() {
                let info = &cached.source;
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

                // Album-level metadata block (from sidecar overlay
                // when present, ScarletBook fallback otherwise).
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

                if let Some(contents) = browse
                    .disc_probe_cache
                    .get(&entry.path)
                    .and_then(|cache| cache.contents_if_current(&entry.path))
                {
                    if contents.presentations.len() >= 2 {
                        lines.push(vec![]);
                        let row = lines.len();
                        let label = " audio streams ";
                        let width = label.chars().count();
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
                        audio_streams_pill_row = Some(row);
                    }
                }
                // SACD stream summary uses the shared DiscContents cache when available.
                // Edit-tags pill — parity with the AudioFile arm so
                // SACD ISOs have a clickable mouse path to the
                // metadata editor (keyboard via :tags, context menu
                // via right-click already exist).
                lines.push(vec![]);
                let edit_tags_row = lines.len();
                let et_label = " edit tags ";
                let et_w = et_label.chars().count();
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
            } else {
                // Not yet probed (async probe pending) — fall back to
                // size only, but still emit the edit-tags pill so
                // the mouse path stays available during the probe
                // window (matches AudioFile arm's behaviour).
                lines.push(vec![
                    Span::styled("   size    ", theme.muted()),
                    Span::styled(size_str(entry.size), theme.text_style()),
                ]);
                lines.push(vec![]);
                let edit_tags_row = lines.len();
                let et_label = " edit tags ";
                let et_w = et_label.chars().count();
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

    let mut spans = vec![Span::styled("│", theme.border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
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
        assert_eq!(cursor_col, (" path: Music/Al".chars().count()) as u16);
        assert!(spans.iter().any(|span| {
            span.content.as_ref().contains("Al")
                && span.style.bg == Some(theme.selection_bg)
                && span.style.fg == Some(theme.bg)
        }));
    }
}

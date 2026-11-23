//! Output options pane: destination, one Naming pattern, merge mode, est. size (cyan border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatState, MetadataState, MultiTrackEntry, OutputOptionsField, OutputOptionsState, PreviewTrack, SourceMode};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::inline_edit::{inline_cursor_col, render_inline_value};
use super::pill::{render_pill_spans, PillState};
use super::probe::{SourceInfo, SourceMetadata};
use std::path::Path;
use crate::convert::formats::AudioFormat;

/// Row offsets, relative to the output-options pane top, for the below-the-fold
/// conversion option pills. These are shared with mouse hit registration so the
/// rendered rows and clickable rows cannot drift independently.
pub const OUTPUT_OPTIONS_FORCE_ENCODE_ROW: u16 = 13;
pub const OUTPUT_OPTIONS_DISC_SUBFOLDERS_ROW: u16 = 14;
pub const OUTPUT_OPTIONS_WRITE_LOG_ROW: u16 = 15;
pub const OUTPUT_OPTIONS_PARTIAL_ROW: u16 = 16;
pub const OUTPUT_OPTIONS_IF_EXISTS_ROW: u16 = 17;
pub const OUTPUT_OPTIONS_ACTIONS_MIN_HEIGHT: u16 = 20;
const OUTPUT_OPTIONS_COMPACT_ACTIONS_ROW: u16 = 18;
pub const OUTPUT_OPTIONS_ACTIONS_ROW: u16 = 20;

/// A 20/21-row maximized pane fits the Actions field only without the two
/// decorative rows preceding its section. Taller panes retain that spacing.
/// Drawing, mouse registration, and tests all use this one placement rule.
fn actions_row_for_height(height: u16) -> Option<u16> {
    if height >= 22 {
        Some(OUTPUT_OPTIONS_ACTIONS_ROW)
    } else if height >= OUTPUT_OPTIONS_ACTIONS_MIN_HEIGHT {
        Some(OUTPUT_OPTIONS_COMPACT_ACTIONS_ROW)
    } else {
        None
    }
}

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
        buttons.record_button(TuiButton::FilenameTemplateField, row_rect(2));
        register_output_options_template_buttons(buttons, area, 2);
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

    if maximized && area.height >= 19 {
        register_output_options_pills(buttons, area, OUTPUT_OPTIONS_PARTIAL_ROW,
            "on failure ", &opts.partial, TuiButton::PartialPill);
        register_output_options_pills(buttons, area, OUTPUT_OPTIONS_IF_EXISTS_ROW,
            "if exists ", &opts.if_exists, TuiButton::IfExistsPill);
    }

    if show_actions && maximized {
        if let Some(row) = actions_row_for_height(area.height) {
            if row_visible(row) {
                buttons.record_button(TuiButton::ActionsPipelineField, row_rect(row));
            }
        }
    }
}

fn register_output_options_template_buttons(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    row: u16,
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
    let (load_button, build_button) = (
        TuiButton::TemplateLoadFilenameButton,
        TuiButton::TemplateBuildFilenameButton,
    );
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
    let label_width = super::display_width::width(&format!("   {}  ", label)) as u16;
    let mut x = area.x.saturating_add(1).saturating_add(label_width);
    for (index, option) in state.options.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(2);
        }
        let width = super::display_width::width(&option.label) as u16 + 2;
        if x >= right {
            break;
        }
        let visible_width = width.min(right.saturating_sub(x));
        buttons.record_button(button_for(index), Rect::new(x, y, visible_width, 1));
        x = x.saturating_add(width);
    }
}


/// In Batch, only AlbumArtist is editable in the metadata pane. The other
/// fields may still hold tags from the initially selected member; they must
/// not override the currently selected source's Naming preview or the
/// per-item admission projection.
pub(crate) fn convert_batch_naming_overrides(metadata: &MetadataState) -> MetadataState {
    MetadataState {
        album_artist_for_conversion: metadata.album_artist_for_conversion.clone(),
        ..MetadataState::default()
    }
}

/// A borrowed projection of the archive members that Convert will submit.
/// The materializer derives album metadata after filtering by track selection;
/// use the same membership and strict promotion rule on the redraw path.
/// No extra file reads or copies of artwork inventories are needed.
#[derive(Clone, Copy)]
pub(crate) struct ArchiveNamingAlbum<'a> {
    album: Option<&'a str>,
    album_artist: Option<&'a str>,
    first_artist: Option<&'a str>,
    year: Option<&'a str>,
    genre: Option<&'a str>,
    composer: Option<&'a str>,
    isrc: Option<&'a str>,
    catalog_number: Option<&'a str>,
}

fn common_selected_archive_tag<'a>(
    tracks: &'a [PreviewTrack],
    selected: &[bool],
    field: for<'m> fn(&'m SourceMetadata) -> Option<&'m str>,
) -> Option<&'a str> {
    let mut members = tracks.iter().zip(selected.iter())
        .filter_map(|(track, selected)| (*selected).then_some(&track.metadata));
    let first = field(members.next()?)?;
    if first.trim().is_empty() || !members.all(|metadata| field(metadata) == Some(first)) {
        None
    } else {
        Some(first)
    }
}

impl<'a> ArchiveNamingAlbum<'a> {
    fn from_selection(tracks: &'a [PreviewTrack], selected: &[bool]) -> Self {
        let artist = common_selected_archive_tag(tracks, selected,
            |metadata| metadata.artist.as_deref());
        let first_artist = tracks.iter().zip(selected.iter())
            .filter(|(_, selected)| **selected)
            .find_map(|(track, _)| track.metadata.artist.as_deref());
        Self {
            album: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.album.as_deref()),
            album_artist: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.album_artist.as_deref()).or(artist),
            first_artist,
            year: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.year.as_deref()),
            genre: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.genre.as_deref()),
            // COMPOSER and ISRC stay track-scoped in filenames. All three
            // fields require selected-track consensus in album folders;
            // CATALOG is album-scoped in filenames as well.
            composer: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.composer.as_deref()),
            isrc: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.isrc.as_deref()),
            catalog_number: common_selected_archive_tag(tracks, selected,
                |metadata| metadata.catalog_number.as_deref()),
        }
    }
}

/// Live Convert metadata for the read-only Naming preview. The actual planner
/// remains responsible for collisions, name limits, and disc subfolders.
#[derive(Clone, Copy)]
pub(crate) struct ConvertNamingPreview<'a> {
    pub source_metadata: &'a SourceMetadata,
    pub album_metadata: Option<&'a SourceMetadata>,
    pub archive_album: Option<ArchiveNamingAlbum<'a>>,
    pub selected_track: Option<&'a MultiTrackEntry>,
    pub archive_track: bool,
    pub album_title: Option<&'a str>,
    pub album_artist: Option<&'a str>,
    /// True only for existing, parsed multi-disc metadata; never inferred
    /// from the cursor number or an apparent directory name.
    pub multi_disc_proven: bool,
    pub overrides: &'a MetadataState,
    pub source_path: &'a Path,
}

impl<'a> ConvertNamingPreview<'a> {
    /// Borrow the already-probed source evidence shared by Convert's Naming
    /// preview and queue-admission folder policy. Neither caller probes files.
    pub(crate) fn from_source(
        source: &'a SourceMode,
        opts: &OutputOptionsState,
        overrides: &'a MetadataState,
    ) -> Option<Self> {
        let source_path = source.current_path()?;
        let source_metadata = source.current_metadata_ref()?;
        let (album_metadata, archive_album, selected_track, archive_track,
            album_title, album_artist, multi_disc_proven) = match source {
            SourceMode::MultiTrack {
                metadata, tracks, cursor, selected, album_title, album_artist,
                archive_preview, ..
            } => {
                let relevant = *opts.disc_subfolders.selected_value()
                    || opts.folder_template.contains("DISC")
                    || opts.filename_template.contains("DISC");
                let proven = relevant && archive_preview.as_ref().is_some_and(|archive| {
                    archive.tracks.iter().any(|entry| {
                        entry.metadata.disc_number.is_some_and(|n| n > 1)
                            || entry.metadata.disc_total.is_some_and(|n| n > 1)
                    })
                });
                let projected = archive_preview.as_ref().map(|archive| {
                    ArchiveNamingAlbum::from_selection(&archive.tracks, selected)
                });
                (Some(metadata), projected, tracks.get(*cursor), archive_preview.is_some(),
                    album_title.as_deref(), album_artist.as_deref(), proven)
            }
            _ => (None, None, None, false, None, None, false),
        };
        Some(Self {
            source_metadata, album_metadata, archive_album, selected_track, archive_track,
            album_title, album_artist, multi_disc_proven, overrides,
            source_path: source_path.as_path(),
        })
    }
}

/// An album-scoped optional prefix may render empty after conditional blocks
/// are evaluated. In that case select the existing flat-output policy *before*
/// admission; when it renders a real component, keep the album directory as
/// the planner's root for logs, companions, and Keep Both.
///
/// Unknown extra tags cannot be inferred from this lightweight probe model.
/// Preserve the established album policy rather than guessing they are empty.
pub(crate) fn optional_convert_album_folder_is_empty(
    opts: &OutputOptionsState,
    format: &FormatState,
    source_info: Option<&SourceInfo>,
    preview: Option<ConvertNamingPreview<'_>>,
) -> Option<bool> {
    let metadata_confirmed = source_info.is_some()
        || preview.is_some_and(|context| context.selected_track.is_some());
    optional_convert_album_folder_is_empty_with_evidence(
        opts, format, source_info, preview, metadata_confirmed,
    )
}

/// Batch admission has just read this particular file's tags, but does not
/// need a technical audio probe for optional YEAR/GENRE album prefixes.
/// Keep the result unknown for technical tokens whose source facts are absent.
pub(crate) fn optional_convert_album_folder_for_probed_tags(
    opts: &OutputOptionsState,
    format: &FormatState,
    preview: ConvertNamingPreview<'_>,
) -> Option<bool> {
    optional_convert_album_folder_is_empty_with_evidence(
        opts, format, None, Some(preview), true,
    )
}

fn optional_convert_album_folder_is_empty_with_evidence(
    opts: &OutputOptionsState,
    format: &FormatState,
    source_info: Option<&SourceInfo>,
    preview: Option<ConvertNamingPreview<'_>>,
    metadata_confirmed: bool,
) -> Option<bool> {
    if opts.folder_template.is_empty() {
        return Some(true);
    }
    if OutputOptionsState::has_stable_album_prefix(&opts.folder_template) {
        return Some(false);
    }
    let preview = preview?;
    let pattern = opts.conversion_folder_template();
    let known = ["ARTIST", "ALBUM_ARTIST", "ALBUM", "TITLE_EXTRA", "YEAR",
        "GENRE", "COMPOSER", "ISRC", "CATALOG", "FORMAT", "SAMPLERATE", "BITDEPTH", "EXT"];
    if pattern.split('%').skip(1).step_by(2).any(|token| !known.contains(&token)) {
        return None;
    }
    if !metadata_confirmed {
        // An unprobed cursor is not evidence that its optional tags are absent.
        return None;
    }
    if source_info.is_none() && (pattern.contains("%SAMPLERATE%")
        || pattern.contains("%BITDEPTH%")) {
        // Tag-only batch probes do not provide technical format facts.
        return None;
    }
    let folder = render_live_convert_template(
        &pattern, true, opts.folder_template_capitalize,
        format.selected_extension(), preview, source_info, format,
    );
    Some(sanitize_relative_preview(&folder).is_empty())
}

/// Convert's in-place preview uses the same cell-aware destination shortening
/// order as Extract. Before a source is available, the editor's fixed example
/// metadata provide an illustrative result. No filesystem operation occurs.
fn convert_naming_path_preview(
    opts: &OutputOptionsState,
    template: &str,
    format: &FormatState,
    available_cells: usize,
    source_info: Option<&SourceInfo>,
    preview: Option<ConvertNamingPreview<'_>>,
) -> String {
    let ext = format.selected_extension();
    let (folder_pattern, filename_pattern) =
        OutputOptionsState::split_naming_pattern(template);
    let render_segment = |pattern: &str, album_scope: bool| {
        if let Some(preview) = preview {
            render_live_convert_template(pattern, album_scope,
                opts.filename_template_capitalize, ext, preview, source_info, format)
        } else {
            super::template_builder::render_template_preview_with_case_and_extension(
                &pattern.replace("%EXTENSION%", "%EXT%"),
                opts.filename_template_capitalize, ext)
        }
    };
    // A rendered folder can be empty even when its conditional template is
    // nonempty. The planner discards that component rather than creating `/`.
    // `disc dirs` augments the track-level path, after any stable album
    // folder. The backend helper also suppresses duplicate explicit disc
    // routes. With no loaded source, the fixed example has no disc evidence.
    let track_pattern = crate::convert::formats::naming_template_with_disc_subfolder(
        filename_pattern, preview.is_some() && *opts.disc_subfolders.selected_value());
    let filename = sanitize_relative_preview(&render_segment(&track_pattern, false));
    let folder = if folder_pattern.is_empty() {
        String::new()
    } else {
        sanitize_relative_preview(&render_segment(folder_pattern, true))
    };
    let mut authored = if folder.trim_matches('/').is_empty() {
        filename
    } else {
        format!("{}/{}", folder.trim_matches('/'), filename.trim_start_matches('/'))
    };
    let suffix = format!(".{ext}");
    if !authored.rsplit('/').next().unwrap_or_default()
        .to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()) {
        authored.push_str(&suffix);
    }
    let root = opts.dest_path.as_deref();
    if let Some(root) = root {
        let path = root.join(&authored);
        super::template_builder::destination_path_preview(
            &path, root, available_cells, || Some(authored.clone()),
        )
    } else {
        super::template_builder::clip_result(&authored, available_cells)
    }
}

/// Match the planner's authored path-component cleanup without interpreting
/// a slash inside an expanded source tag as a template delimiter.
fn sanitize_relative_preview(rendered: &str) -> String {
    rendered.split('/').map(str::trim).filter(|part| !part.is_empty())
        .map(|part| if matches!(part, "." | "..") {"untitled"} else {part})
        .collect::<Vec<_>>().join("/")
}

/// Render a read-only approximation with the same metadata authority as the
/// conversion request. Ordinary Convert metadata-pane edits are not sent to
/// the planner, except for the album-artist override. Archive edits are already
/// reflected in the selected source track's metadata before queue admission.
/// Delegate conditional blocks and sanitization to the conversion logic; a
/// slash in a source tag must not become an authored path separator.
fn render_live_convert_template(
    template: &str,
    album_scope: bool,
    capitalize: bool,
    extension: &str,
    preview: ConvertNamingPreview<'_>,
    source_info: Option<&SourceInfo>,
    format: &FormatState,
) -> String {
    use std::collections::BTreeMap;
    let clean = crate::convert::pipeline::stages::conversion_action_sanitize_component;
    let src = preview.source_metadata;
    let album_src = preview.album_metadata.unwrap_or(src);
    let archive_album = preview.archive_album;
    let album_year = archive_album.map(|album| album.year)
        .unwrap_or_else(|| album_src.year.as_deref());
    let album_genre = archive_album.map(|album| album.genre)
        .unwrap_or_else(|| album_src.genre.as_deref());
    // The planner reads archive folder COMPOSER/ISRC from consensus album
    // extras, but reads filename COMPOSER/ISRC from the individual track.
    // CATALOG is album-scoped in both planner paths.
    let (composer, isrc, catalog) = if let Some(album) = archive_album {
        (
            if album_scope { album.composer } else { src.composer.as_deref() },
            if album_scope { album.isrc } else { src.isrc.as_deref() },
            album.catalog_number,
        )
    } else {
        (
            src.composer.as_deref().or(album_src.composer.as_deref()),
            src.isrc.as_deref(),
            album_src.catalog_number.as_deref().or(src.catalog_number.as_deref()),
        )
    };
    let track = preview.selected_track;
    let source_stem = preview.source_path.file_stem().and_then(|stem| stem.to_str())
        .unwrap_or("untitled");
    let choose = |source_value: Option<&str>, fallback: &str| {
        clean(source_value.unwrap_or(fallback))
    };
    let number = if preview.archive_track {
        // The archive materializer honors a stored TRACKNUMBER before the
        // member ordinal. CUE/SACD use the parsed track-list number instead.
        src.track_number.or_else(|| track.map(|entry| entry.number))
    } else {
        track.map(|entry| entry.number).or(src.track_number)
    }.unwrap_or(1);
    let track_artist = choose(track.and_then(|entry| entry.performer.as_deref())
        .or(src.artist.as_deref()).or(album_src.artist.as_deref()), "Unknown Artist");
    let album_artist_source = if let Some(album) = archive_album {
        // Match the archive planner: common album artist, then the first
        // selected track's artist when no album-wide value is available.
        album.album_artist.or(album.first_artist)
    } else {
        preview.album_artist.or(album_src.album_artist.as_deref())
            .or(src.album_artist.as_deref()).or(album_src.artist.as_deref())
            .or(Some(&track_artist))
    };
    let album_artist = choose(preview.overrides.album_artist_for_conversion.as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(album_artist_source), "Unknown Artist");
    // In Convert's established planner, ARTIST in the album folder means
    // album artist. ARTIST in the track filename means track performer.
    let artist = if album_scope { album_artist.clone() } else { track_artist };
    let album_name = if let Some(album) = archive_album {
        album.album
    } else {
        album_src.album.as_deref().or(preview.album_title).or(src.album.as_deref())
    };
    let mut album = choose(album_name, source_stem);
    let mut title_extra = String::new();
    if template.contains("%TITLE_EXTRA%") {
        if let Some((stem, qualifier)) =
            crate::convert::pipeline::stages::extract_title_extra(&album) {
            album = stem;
            title_extra = clean(&qualifier);
        }
    }
    let fallback_title = track.map(|_| format!("Track {number:02}"))
        .unwrap_or_else(|| source_stem.to_owned());
    let source_title = if preview.archive_track {
        src.title.as_deref().or_else(|| track.and_then(|entry| entry.title.as_deref()))
    } else {
        track.and_then(|entry| entry.title.as_deref()).or(src.title.as_deref())
    };
    let title = choose(source_title, &fallback_title);
    let disc_number = src.disc_number.or(album_src.disc_number).filter(|n| *n > 0);
    let disc_total = src.disc_total.or(album_src.disc_total).filter(|n| *n > 1);
    let proven_disc = preview.multi_disc_proven
        || disc_number.is_some_and(|n| n > 1) || disc_total.is_some();
    let sample_rate = (*format.sample_rate.selected_value()
        != crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL)
        .then_some(*format.sample_rate.selected_value())
        .or_else(|| source_info.map(|info| info.sample_rate));
    let rate = sample_rate.map(|hz| {
        if hz >= 2_822_400 && hz % 2_822_400 == 0 {
            format!("DSD{}", 64 * (hz / 2_822_400))
        } else if hz % 1000 == 0 {
            format!("{}kHz", hz / 1000)
        } else if hz % 100 == 0 {
            format!("{:.1}kHz", f64::from(hz) / 1000.0)
        } else {
            let mut text = format!("{:.3}", f64::from(hz) / 1000.0);
            while text.contains('.') && text.ends_with('0') { text.pop(); }
            if text.ends_with('.') { text.pop(); }
            format!("{text}kHz")
        }
    }).unwrap_or_default();
    let depth = if format.bit_depth.selected_value().is_source() {
        source_info.and_then(|info| info.bit_depth).map(|n| n.to_string())
    } else {
        Some(format.bit_depth.selected_value().bits().to_string())
    }.unwrap_or_default();
    // The planner renders YEAR from the first contiguous run of four ASCII
    // date digits, not from the entire DATE field (e.g. 2020-05-17 -> 2020).
    // Both planner scopes use album_metadata.date. A track's own DATE can
    // differ across a multi-track source and must not invent an album folder.
    let year = album_year.map(convert_preview_year_from_date).unwrap_or_default();
    let mut tokens = BTreeMap::new();
    for (key, value) in [
        ("ARTIST", artist.as_str()), ("ALBUM_ARTIST", album_artist.as_str()),
        ("ALBUM", album.as_str()), ("TITLE", title.as_str()),
        ("TITLE_EXTRA", title_extra.as_str()),
        ("GENRE", (if album_scope { album_genre } else { src.genre.as_deref().or(album_genre) })
            .unwrap_or("")),
        ("YEAR", year.as_str()),
        ("COMPOSER", composer.unwrap_or("")),
        ("ISRC", isrc.unwrap_or("")),
        ("CATALOG", catalog.unwrap_or("")),
        ("FORMAT", format.format.selected_value().name()),
        ("SAMPLERATE", rate.as_str()), ("BITDEPTH", depth.as_str()),
    ] {
        tokens.insert(key.to_owned(), if value.is_empty() {
            String::new()
        } else {clean(value)});
    }
    tokens.insert("TRACKNN".into(), format!("{number:02}"));
    tokens.insert("NN".into(), format!("{number:02}"));
    for key in ["TRACK", "TRACKN", "N"] {
        tokens.insert(key.to_owned(), number.to_string());
    }
    tokens.insert("TRACKNUMBER".into(), format!("{number:02}"));
    // Raw DISC falls back to one; the DISCNUMBER family and automatic disc
    // folder only resolve with positive disc identity and multi-disc proof.
    tokens.insert("DISC".into(), disc_number.unwrap_or(1).to_string());
    let scoped_disc = disc_number.filter(|_| proven_disc);
    tokens.insert("DISCNUMBER".into(), scoped_disc.map(|n| n.to_string()).unwrap_or_default());
    tokens.insert("NNDISCNUMBER".into(), scoped_disc.map(|n| format!("{n:02}")).unwrap_or_default());
    tokens.insert("NNNDISCNUMBER".into(), scoped_disc.map(|n| format!("{n:03}")).unwrap_or_default());
    tokens.insert("DISCTOTAL".into(), disc_total.map(|n| n.to_string()).unwrap_or_default());
    tokens.insert("DISC_FOLDER".into(), scoped_disc.map(|n| format!("disc {n:02}")).unwrap_or_default());
    tokens.insert("EXT".into(), String::new());
    tokens.insert("EXTENSION".into(), String::new());
    tokens.insert("PERFORMER".into(), artist);
    // The shared editor stores literal .%EXT% / .%EXTENSION% in its pattern.
    // In Convert the planner appends the selected container extension itself.
    let pattern = template.replace(".%EXTENSION%", "")
        .replace("%EXTENSION%", "").replace(".%EXT%", "").replace("%EXT%", "");
    let empty = BTreeMap::new();
    let conditioned = crate::convert::pipeline::stages::render_conditional_template_blocks(
        &pattern, &tokens, None, &empty);
    let mut result = String::new();
    let mut rest = conditioned.as_str();
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            result.push('%');
            result.push_str(rest);
            return result;
        };
        result.push_str(tokens.get(&rest[..end]).map(String::as_str).unwrap_or(""));
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    // Mirror path-component cleanup: separators belong only to the authored
    // template; an expanded metadata value has already been sanitized above.
    let result = sanitize_relative_preview(&result);
    let result = if capitalize {
        crate::convert::renaming::capitalize_naming_path(&result, Some(extension))
    } else {result};
    result
}

/// Match the conversion planner's DATE-to-YEAR interpretation. The
/// planner's extractor is private to its locked module.
fn convert_preview_year_from_date(date: &str) -> String {
    let mut run = String::new();
    for ch in date.chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
            if run.len() == 4 {
                return run;
            }
        } else {
            run.clear();
        }
    }
    String::new()
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



/// Draw the output options pane with cyan border.
pub fn draw_output_options_pane(
    f: &mut Frame, area: Rect, opts: &OutputOptionsState,
    source_info: Option<&SourceInfo>, total_source_size: u64,
    format: &FormatState, focused: bool, maximized: bool,
    show_actions: bool, theme: super::theme::Theme,
) {
    draw_output_options_pane_with_preview(f, area, opts, source_info,
        total_source_size, format, focused, maximized, show_actions, None, theme);
}

pub(crate) fn draw_output_options_pane_with_preview(
    f: &mut Frame,
    area: Rect,
    opts: &OutputOptionsState,
    source_info: Option<&SourceInfo>,
    total_source_size: u64,
    format: &FormatState,
    focused: bool,
    maximized: bool,
    show_actions: bool,
    preview: Option<ConvertNamingPreview<'_>>,
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
    let is_file_focused = focused && opts.field_focus == OutputOptionsField::FilenameTemplate;
    let is_merge_focused = focused && opts.field_focus == OutputOptionsField::MergeMode;
    let is_extensions_focused = focused && opts.field_focus == OutputOptionsField::CompanionExtensions;
    let is_folders_focused = focused && opts.field_focus == OutputOptionsField::CompanionFolders;
    let is_exclude_files_focused = focused && opts.field_focus == OutputOptionsField::ExcludeFiles;
    let is_force_encode_focused = focused && opts.field_focus == OutputOptionsField::ForceEncode;
    let is_disc_subfolders_focused = focused && opts.field_focus == OutputOptionsField::DiscSubfolders;
    let is_write_log_focused = focused && opts.field_focus == OutputOptionsField::WriteLog;
    let is_partial_focused = focused && opts.field_focus == OutputOptionsField::Partial;
    let is_if_exists_focused = focused && opts.field_focus == OutputOptionsField::IfExists;
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
    let mut dest_spans = vec![Span::styled("   dest        ", dest_label_style)];
    dest_spans.extend(render_inline_value(
        &dest_display,
        is_editing(OutputOptionsField::DestPath),
        &opts.edit_input,
        is_dest_focused,
        dest_value_w,
        theme,
    ));
    let dest_row = bordered_line(border_color, w, dest_spans, theme);

    // One authored Naming field; the existing backend still receives distinct
    // album-folder and filename segments at the conversion request boundary.
    let naming_label_style = if is_file_focused { theme.bright() } else { theme.muted() };
    let load_pill = Span::styled(
        " load ",
        Style::default().fg(theme.pill_active_fg).bg(theme.amber)
            .add_modifier(Modifier::BOLD),
    );
    let build_pill = Span::styled(
        " custom ",
        Style::default().fg(theme.pill_active_fg).bg(theme.blue)
            .add_modifier(Modifier::BOLD),
    );
    let pill_width = 6 + 1 + 8;
    let template_width = w.saturating_sub(15 + pill_width + 4);
    let template = if is_editing(OutputOptionsField::FilenameTemplate) {
        opts.edit_input.text.clone()
    } else {
        opts.naming_template()
    };
    let naming_row = template_row_with_value_span(
        border_color, w, "   naming      ",
        render_inline_value(
            &template,
            is_editing(OutputOptionsField::FilenameTemplate),
            &opts.edit_input,
            is_file_focused,
            template_width,
            theme,
        ),
        naming_label_style, load_pill, build_pill, theme,
    );
    let result = convert_naming_path_preview(opts, &template, format, w.saturating_sub(7),
        source_info, preview);
    let result_row = bordered_line(border_color, w,
        vec![Span::styled("   → ", theme.muted()),
             Span::styled(result, theme.accent())], theme);

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
    lines.push(naming_row);
    lines.push(result_row);
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

    if maximized && area.height >= 19 {
        lines.push(pill_row(
            border_color, w, "on failure ", "",
            &render_pill_spans(&opts.partial, is_partial_focused, theme),
            is_partial_focused, theme,
        ));
        lines.push(pill_row(
            border_color, w, "if exists ", "",
            &render_pill_spans(&opts.if_exists, is_if_exists_focused, theme),
            is_if_exists_focused, theme,
        ));
    }

    if show_actions && maximized {
        if let Some(row) = actions_row_for_height(area.height) {
            if row == OUTPUT_OPTIONS_ACTIONS_ROW {
                lines.push(bordered_line(border_color, w, vec![], theme));
                lines.push(bordered_line(
                    border_color,
                    w,
                    vec![Span::styled("   Actions", output_options_section_header_style(theme))],
                    theme,
                ));
            }
            lines.push(actions_row(
                border_color,
                w,
                &output_options_actions_summary(&opts.actions),
                opts.actions.is_empty(),
                is_actions_focused,
                theme,
            ));
        }
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
    let mut spans = vec![Span::styled(label, label_style)];
    spans.extend(render_inline_value(
        value, editing, input, focused, value_w, theme,
    ));
    bordered_line(border_color, width, spans, theme)
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
        OutputOptionsField::FilenameTemplate => {
            let pill_width = 6 + 1 + 8;
            Some((2, label_w, pane_width.saturating_sub(15 + pill_width + 4)))
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
    value_spans: Vec<Span<'static>>,
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
    ];
    spans.extend(value_spans);

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
                if !format.source_is_dsd {
                    return None;
                }
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
    fn pcm_source_with_source_relative_dsd_rate_has_no_false_size_estimate() {
        let info = SourceInfo {
            format_name: "FLAC".to_string(),
            codec: "flac".to_string(),
            bit_depth: Some(24),
            sample_format_is_float: None,
            compression_is_lossless: None,
            sample_rate: 96_000,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 60.0,
            file_size: 10_000_000,
        };
        let mut format = FormatState::new();
        format.source_is_dsd = true;
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();
        assert!(format
            .sample_rate
            .select_value(&crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL));

        // Model a source transition from DSD to PCM before the next constraint
        // pass. The estimator must never interpret 96 kHz PCM as a 1-bit rate.
        format.source_is_dsd = false;
        assert_eq!(estimate_output_size(Some(&info), info.file_size, &format), None);
    }

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
        for height in [20, 21, 22] {
            let backend = TestBackend::new(80, height);
            let mut terminal = Terminal::new(backend).expect("terminal");

            terminal
                .draw(|frame| {
                    draw_output_options_pane(
                        frame,
                        Rect::new(0, 0, 80, height),
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
            let offset = actions_row_for_height(height).expect("Actions row fits");
            for x in 0..80 {
                row.push_str(terminal.backend().buffer().get(x, offset).symbol());
            }
            assert!(row.contains("pipeline"), "height {height}: actions label: {row}");
            assert!(row.contains("1 post"), "height {height}: live summary: {row}");
            assert!(row.contains("Enter/click edit"), "height {height}: edit affordance: {row}");
            for (offset, expected) in [(OUTPUT_OPTIONS_PARTIAL_ROW, "on failure"),
                                       (OUTPUT_OPTIONS_IF_EXISTS_ROW, "if exists")] {
                let mut text = String::new();
                for x in 0..80 {
                    text.push_str(terminal.backend().buffer().get(x, offset).symbol());
                }
                assert!(text.contains(expected), "height {height}: {expected} missing: {text}");
                if offset == OUTPUT_OPTIONS_PARTIAL_ROW {
                    for label in ["fail conversion", "keep partial output"] {
                        assert!(text.contains(label),
                            "height {height}: source failure outcome {label} missing: {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn maximized_actions_row_registers_button_map_target_inside_pane_only() {
        let opts = OutputOptionsState::new();
        for height in [20, 21, 22] {
            let mut buttons = ButtonRenderMap::new();
            register_output_options_mouse_targets(
                &mut buttons,
                Rect::new(10, 4, 80, height),
                &opts,
                true,
                true,
            );

            let y = 4 + actions_row_for_height(height).expect("Actions row fits");
            assert_eq!(buttons.find_button_at(11, y), Some(TuiButton::ActionsPipelineField));
            assert_eq!(buttons.find_button_at(9, y), None, "outside left pane border");
            assert_eq!(buttons.find_button_at(89, y), None, "right pane border");
            assert_eq!(buttons.find_button_at(90, y), None, "outside right pane border");
        }
        assert!(actions_row_for_height(19).is_none());
        let mut short = ButtonRenderMap::new();
        register_output_options_mouse_targets(&mut short, Rect::new(10, 4, 80, 19), &opts, true, true);
        assert!(short.recorded_buttons().iter().all(|(button, _)|
            !matches!(button, TuiButton::ActionsPipelineField)),
            "too-short panes must not create invisible Actions targets");
    }

    #[test]
    fn wrapped_output_options_draw_populates_button_map_for_actions_row() {
        let theme = crate::tui::theme::theme_by_slug(crate::tui::theme::default_theme_slug())
            .expect("default theme");
        let opts = OutputOptionsState::new();
        let format = FormatState::new();
        for height in [20, 21, 22] {
            let area = Rect::new(10, 4, 80, height);
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

            let y = 4 + actions_row_for_height(height).expect("Actions row fits");
            assert_eq!(
                buttons.find_button_at(11, y), Some(TuiButton::ActionsPipelineField),
                "height {height}: production draw must register the rendered Actions row"
            );
            assert_eq!(buttons.find_button_at(9, y), None);
        }
    }

    #[test]
    fn convert_naming_preview_never_treats_a_source_tag_slash_as_a_folder() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let metadata = SourceMetadata {
            title: Some("Track".to_owned()),
            artist: Some("AC/DC".to_owned()),
            ..SourceMetadata::default()
        };
        let overrides = MetadataState::default();
        let context = ConvertNamingPreview {
            source_metadata: &metadata,
            album_metadata: None,
            archive_album: None,
            selected_track: None,
            archive_track: false,
            album_title: None,
            album_artist: None,
            multi_disc_proven: false,
            overrides: &overrides,
            source_path: Path::new("source.flac"),
        };
        opts.set_naming_template("%ARTIST% - %TITLE%", false);
        let flat = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 200, None, Some(context));
        assert_eq!(flat, "AC DC - Track.flac");
        assert!(!flat.contains('/'));

        opts.set_naming_template("%ARTIST%/%TITLE%", false);
        let nested = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 200, None, Some(context));
        assert_eq!(nested, "AC DC/Track.flac");
        assert_eq!(nested.matches('/').count(), 1);
        assert_eq!(sanitize_relative_preview("../some//album/../track"),
            "untitled/some/album/untitled/track");
    }

    #[test]
    fn convert_preview_uses_selected_multitrack_metadata_and_album_artist() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let album = SourceMetadata {
            album: Some("Songs from the Album".into()),
            artist: Some("First Track Artist".into()),
            ..SourceMetadata::default()
        };
        let entries = [
            MultiTrackEntry { number: 1, title: Some("Beginning".into()),
                performer: Some("Singer One".into()), duration_display: None },
            MultiTrackEntry { number: 3, title: Some("Finale".into()),
                performer: Some("Singer Three".into()), duration_display: None },
        ];
        let mut overrides = MetadataState::default();
        opts.set_naming_template("%ALBUM_ARTIST%/%TRACKNN% - %ARTIST% - %TITLE%", false);
        fn preview_for<'a>(album: &'a SourceMetadata,
            track: &'a MultiTrackEntry, overrides: &'a MetadataState) -> ConvertNamingPreview<'a> {
            ConvertNamingPreview {
                source_metadata: album,
                album_metadata: Some(album),
                archive_album: None,
                selected_track: Some(track),
                archive_track: false,
                album_title: Some("Songs from the Album"),
                album_artist: Some("Various Artists"),
                multi_disc_proven: false,
                overrides,
                source_path: Path::new("compilation.cue"),
            }
        }
        let first = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(preview_for(&album, &entries[0], &overrides)));
        assert_eq!(first, "Various Artists/01 - Singer One - Beginning.flac");
        let third = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(preview_for(&album, &entries[1], &overrides)));
        assert_eq!(third, "Various Artists/03 - Singer Three - Finale.flac");
        opts.set_naming_template("%ARTIST%/%TRACKNN% - %ARTIST% - %TITLE%", false);
        let folder_artist = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(preview_for(&album, &entries[1], &overrides)));
        assert_eq!(folder_artist, "Various Artists/03 - Singer Three - Finale.flac");
        // This local edit is not part of the ordinary CUE/SACD conversion request.
        overrides.title = Some("Edited Finale".into());
        let untransmitted = convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(preview_for(&album, &entries[1], &overrides)));
        assert_eq!(untransmitted, "Various Artists/03 - Singer Three - Finale.flac");

        // An archive member's stored TRACKNUMBER overrides its display
        // ordinal in the materializer; CUE/SACD use their parsed entry number.
        let archive_member = SourceMetadata {
            track_number: Some(7), title: Some("Tagged Track".into()),
            ..SourceMetadata::default()
        };
        opts.set_naming_template("%TRACKNN% - %TITLE%", false);
        let clean_overrides = MetadataState::default();
        let archive_context = ConvertNamingPreview {
            source_metadata: &archive_member,
            archive_track: true,
            ..preview_for(&album, &entries[1], &clean_overrides)
        };
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(archive_context)), "07 - Tagged Track.flac");
        // A committed archive edit is written into the selected source track,
        // not carried as an ordinary Convert request metadata override.
        let edited_archive_member = SourceMetadata {
            title: Some("Archive Edit".into()), year: Some("1984".into()),
            genre: Some("Ambient".into()), ..archive_member.clone()
        };
        let archive_edited = ConvertNamingPreview {
            source_metadata: &edited_archive_member, overrides: &overrides,
            ..archive_context
        };
        // The planner derives the album YEAR and GENRE independently of
        // this archive member's track tags. An album prefix remains empty
        // when the selected source has no common album value.
        for (pattern, expected) in [
            ("%TRACKNN% - %TITLE%", "07 - Archive Edit.flac"),
            ("%YEAR%/%TITLE%", "Archive Edit.flac"),
            ("%GENRE%/%TITLE%", "Archive Edit.flac"),
            ("%TITLE% - %GENRE%", "Archive Edit - Ambient.flac"),
        ] {
            opts.set_naming_template(pattern, false);
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, 160, None, Some(archive_edited)), expected);
        }
        // With an authoritative album value, the folder takes that value,
        // even when the selected archive member has different tags.
        let tagged_album = SourceMetadata {
            year: Some("1977".into()), genre: Some("Soul".into()), ..album.clone()
        };
        let archive_album = ConvertNamingPreview {
            album_metadata: Some(&tagged_album), ..archive_edited
        };
        for (pattern, expected) in [
            ("%YEAR%/%TITLE%", "1977/Archive Edit.flac"),
            ("%GENRE%/%TITLE%", "Soul/Archive Edit.flac"),
        ] {
            opts.set_naming_template(pattern, false);
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, 160, None, Some(archive_album)), expected);
        }
    }

    #[test]
    fn archive_selection_and_edits_keep_preview_folder_admission_and_planner_in_sync() {
        use crate::convert::pipeline::{AlbumMetadata, ExtractionProvenance,
            PreparedSource, PreparedTrack, SourceAudioCoding, SourceAudioDescriptor,
            SourceKind, TrackId, TrackMetadata, TrackSourceRef};
        use crate::convert::{ConversionItem, FileFormat};
        use crate::tui::app::{archive_preview_album_metadata,
            source_mode_from_archive_preview, ArchivePreview, PreviewTrack};
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let info = SourceInfo {
            format_name: "FLAC".into(), codec: "flac".into(),
            bit_depth: Some(24), sample_format_is_float: Some(false),
            compression_is_lossless: Some(true), sample_rate: 44_100,
            channels: 2, channel_layout: "stereo".into(),
            duration_secs: 1.0, file_size: 1024,
        };
        let format = FormatState::new();
        let config = crate::config::TonepoetConfig::default();
        let edits = MetadataState::default();
        let destination = tempfile::tempdir().expect("isolated destination");
        let root = destination.path().to_path_buf();
        let archive_path = PathBuf::from("/music/archive.zip");

        // The expected album values are independent of the TUI projection.
        // They represent the materializer's strict, post-selection promotion.
        for (case, second_year, second_genre, selected, edit_first,
            album_year, album_genre) in [
            ("missing", None, None, [true, true], false, None, None),
            ("matching", Some("1984"), Some("Ambient"), [true, true], false,
                Some("1984"), Some("Ambient")),
            ("conflicting", Some("1990"), Some("Rock"), [true, true], false,
                None, None),
            ("partial", Some("1990"), Some("Rock"), [true, false], false,
                Some("1984"), Some("Ambient")),
            // Start with missing tags, then simulate the existing metadata
            // editor applying the changes to the already-probed track.
            ("edited", Some("1990"), Some("Rock"), [true, false], true,
                Some("1984"), Some("Ambient")),
        ] {
            // Archive folder extras follow the materializer's all-selected
            // rule. Missing tags and disagreement both defeat promotion.
            let common_extra = matches!(case, "matching" | "partial" | "edited");
            let (second_composer, second_isrc, second_catalog) = match case {
                "missing" => (None, None, None),
                "matching" => (Some("Bach"), Some("AAA111"), Some("CAT-01")),
                _ => (Some("Mozart"), Some("BBB222"), Some("CAT-02")),
            };
            let first = SourceMetadata {
                title: Some("Archive Edit".into()),
                artist: Some("Singer".into()),
                album: Some("Album".into()), track_number: Some(7),
                year: (!edit_first).then_some("1984".into()),
                genre: (!edit_first).then_some("Ambient".into()),
                composer: Some("Bach".into()),
                isrc: Some("AAA111".into()),
                catalog_number: Some("CAT-01".into()),
                ..Default::default()
            };
            let second = SourceMetadata {
                title: Some("Second Track".into()),
                artist: Some("Singer".into()),
                album: Some("Album".into()),
                year: second_year.map(str::to_string),
                genre: second_genre.map(str::to_string),
                composer: second_composer.map(str::to_string),
                isrc: second_isrc.map(str::to_string),
                catalog_number: second_catalog.map(str::to_string),
                ..Default::default()
            };
            let tracks = [first, second].into_iter().enumerate()
                .map(|(idx, metadata)| PreviewTrack {
                    path: PathBuf::from(format!("/stage/track-{idx}.flac")),
                    original_name: format!("track-{idx}"),
                    info: info.clone(),
                    original_metadata: metadata.clone(),
                    metadata,
                }).collect::<Vec<_>>();
            let album_metadata = archive_preview_album_metadata(&tracks);
            // The source-pane album summary keeps the old missing-tag rule.
            // The conversion materializer and Naming projection require every
            // selected member to have the tag, so their folder paths can differ.
            if case == "missing" {
                assert_eq!(album_metadata.year.as_deref(), Some("1984"));
                assert_eq!(album_metadata.genre.as_deref(), Some("Ambient"));
            } else if case == "conflicting" {
                assert!(album_metadata.year.is_none(), "{case}: year");
                assert!(album_metadata.genre.is_none(), "{case}: genre");
            }
            let preview = ArchivePreview {
                staging_dir: PathBuf::from("/stage"),
                archive_path: archive_path.clone(),
                tracks, album_metadata,
                mount_lease: None,
            };
            let mut source = source_mode_from_archive_preview(preview);
            let SourceMode::MultiTrack {
                selected: flags, archive_preview: Some(archive), ..
            } = &mut source else { panic!("expected archive source") };
            *flags = selected.to_vec();
            if edit_first {
                archive.tracks[0].metadata.year = Some("1984".into());
                archive.tracks[0].metadata.genre = Some("Ambient".into());
                archive.tracks[0].metadata.title = Some("Archive Edit".into());
                // Deliberately leave the cached album snapshot stale: the
                // live preview and admission must use edited selected tracks.
            }
            let SourceMode::MultiTrack {
                archive_preview: Some(archive), ..
            } = &source else { unreachable!() };
            let prepared_tracks = archive.tracks.iter().enumerate()
                .zip(selected)
                .filter(|(_, wanted)| *wanted)
                .map(|((idx, track), _)| {
                    let ordinal = idx as u32 + 1;
                    PreparedTrack {
                        id: TrackId { source_ordinal: ordinal, disc_number: None,
                            track_number: track.metadata.track_number.unwrap_or(ordinal) },
                        source_ref: TrackSourceRef::StagedFile(track.path.clone()),
                        metadata: TrackMetadata {
                            title: track.metadata.title.clone(),
                            artist: track.metadata.artist.clone().into(),
                            date: track.metadata.year.clone(),
                            genre: track.metadata.genre.clone().into(),
                            composer: track.metadata.composer.clone().into(),
                            isrc: track.metadata.isrc.clone(),
                            // The archive tag reader preserves source text
                            // fields as lower-case planner extras.
                            extra: [
                                ("composer", track.metadata.composer.as_ref()),
                                ("isrc", track.metadata.isrc.as_ref()),
                                ("catalognumber", track.metadata.catalog_number.as_ref()),
                            ].into_iter().filter_map(|(key, value)| {
                                value.map(|value| (key.to_owned(), value.clone()))
                            }).collect(),
                            track_number: track.metadata.track_number,
                            ..Default::default()
                        },
                        expected_samples: Some(44_100), sample_rate: Some(44_100),
                        source_audio: SourceAudioDescriptor::from_scalar(
                            Some(44_100), Some(24), Some(SourceAudioCoding::Pcm)),
                        bit_depth: Some(24), warnings: Vec::new(),
                    }
                }).collect();
            let prepared = PreparedSource {
                container: archive_path.clone(), kind: SourceKind::Archive,
                tracks: prepared_tracks,
                album_metadata: AlbumMetadata {
                    album: Some("Album".into()),
                    album_artist: Some("Singer".into()).into(),
                    date: album_year.map(str::to_owned),
                    genre: album_genre.map(str::to_owned).into(),
                    // Independently model the materializer's promoted extras
                    // after selection, then compare the actual plan to the UI.
                    extra: if common_extra {
                        BTreeMap::from([
                            ("composer".into(), "Bach".into()),
                            ("isrc".into(), "AAA111".into()),
                            ("catalognumber".into(), "CAT-01".into()),
                        ])
                    } else { BTreeMap::new() },
                    total_tracks: selected.iter().filter(|&&value| value).count() as u32,
                    ..Default::default()
                },
                provenance: ExtractionProvenance {
                    source_kind: SourceKind::Archive,
                    source_sha256: None, tool_versions: Default::default(),
                    extracted_at: chrono::Utc::now(),
                },
            };
            for (pattern, expected_prefix, expect_flat) in [
                ("%YEAR%/%TITLE%", album_year, album_year.is_none()),
                ("%GENRE%/%TITLE%", album_genre, album_genre.is_none()),
                // These tokens are track-scoped. Their authored subfolders
                // remain even with flat_output=true (no extra album folder).
                ("%COMPOSER%/%TITLE%", Some("Bach"), true),
                ("%ISRC%/%TITLE%", Some("AAA111"), true),
                ("%CATALOG%/%TITLE%", common_extra.then_some("CAT-01"), !common_extra),
                ("%TRACKNN% - %TITLE%", None, true),
                ("%TITLE% - %GENRE%", None, true),
                ("%TITLE% - %COMPOSER%", None, true),
                ("%TITLE% - %ISRC%", None, true),
                ("%TITLE% - %CATALOG%", None, true),
            ] {
                let mut opts = OutputOptionsState::new();
                opts.dest_path = Some(root.clone());
                opts.set_naming_template(pattern, false);
                let context = ConvertNamingPreview::from_source(&source, &opts, &edits)
                    .expect("already-probed source");
                let displayed = convert_naming_path_preview(&opts, &opts.naming_template(),
                    &format, 512, source.current_info(), Some(context));
                let mut conversion = crate::tui::convert_actions::try_pills_to_options(
                    &format, &opts, &config).expect("valid Convert options");
                crate::tui::convert_actions::apply_optional_naming_folder_for_source(
                    &mut conversion, &opts, &format, &source, &edits);
                assert_eq!(conversion.flat_output, expect_flat,
                    "{case}: admission policy {pattern}");
                let settings = conversion.pipeline_settings.clone().unwrap();
                let item = ConversionItem::new(archive_path.clone(),
                    FileFormat::Audio(crate::convert::formats::AudioFormat::Flac), conversion);
                let mut request = crate::convert::pipeline::unified_request::build_pipeline_request_from_settings(
                    &item, settings).expect("valid planner request");
                request.naming.template = item.options.effective_naming_template("%NN% - %TITLE%");
                assert_eq!(request.naming.per_album_subdir, !expect_flat,
                    "{case}: album-folder request policy {pattern}");
                let plan = crate::convert::pipeline::stages::plan_outputs(&prepared, &request)
                    .expect("planner accepts archive");
                let planned = &plan.entries[0].final_path;
                assert_eq!(Path::new(&displayed), planned.as_path(),
                    "{case}: preview and planner {pattern}");
                let relative = planned.strip_prefix(&root).unwrap();
                if let Some(prefix) = expected_prefix {
                    assert!(relative.starts_with(prefix), "{case}: {pattern} folder");
                } else if pattern.contains('/') {
                    assert_eq!(relative.components().count(), 1,
                        "{case}: {pattern} has no album folder");
                }
                if case == "missing" && matches!(pattern,
                    "%COMPOSER%/%TITLE%" | "%ISRC%/%TITLE%") {
                    // The second selected member has no such track tag, and
                    // must not inherit a sibling's authored track directory.
                    let second = plan.entries[1].final_path.strip_prefix(&root).unwrap();
                    assert_eq!(second, Path::new("Second Track.flac"),
                        "{case}: empty track-scoped prefix {pattern}");
                }
                if pattern.starts_with("%TRACKNN%") {
                    assert_eq!(relative, Path::new("07 - Archive Edit.flac"),
                        "{case}: archive TRACKNUMBER survives");
                }
                if pattern.contains(" - %GENRE%") {
                    assert_eq!(relative, Path::new("Archive Edit - Ambient.flac"),
                        "{case}: track GENRE remains track-scoped");
                }
                if pattern.contains(" - %COMPOSER%") {
                    assert_eq!(relative, Path::new("Archive Edit - Bach.flac"),
                        "{case}: filename COMPOSER remains track-scoped");
                }
                if pattern.contains(" - %ISRC%") {
                    assert_eq!(relative, Path::new("Archive Edit - AAA111.flac"),
                        "{case}: filename ISRC remains track-scoped");
                }
                if pattern.contains(" - %CATALOG%") {
                    let expected = if common_extra { "Archive Edit - CAT-01.flac" }
                        else { "Archive Edit -.flac" };
                    assert_eq!(relative, Path::new(expected),
                        "{case}: filename CATALOG uses common album metadata");
                }
            }
            if case == "missing" {
                // The highlighted second member has no COMPOSER or ISRC.
                // Its filename must not inherit the first member's tags.
                let SourceMode::MultiTrack { cursor, .. } = &mut source
                    else { unreachable!() };
                *cursor = 1;
                for pattern in ["%TITLE% - %COMPOSER%", "%TITLE% - %ISRC%"] {
                    let mut opts = OutputOptionsState::new();
                    opts.dest_path = Some(root.clone());
                    opts.set_naming_template(pattern, false);
                    let context = ConvertNamingPreview::from_source(&source, &opts, &edits)
                        .expect("already-probed second member");
                    let displayed = convert_naming_path_preview(&opts, &opts.naming_template(),
                        &format, 512, source.current_info(), Some(context));
                    let mut converted = crate::tui::convert_actions::try_pills_to_options(
                        &format, &opts, &config).expect("valid Convert options");
                    crate::tui::convert_actions::apply_optional_naming_folder_for_source(
                        &mut converted, &opts, &format, &source, &edits);
                    assert!(converted.flat_output, "filename-only admission: {pattern}");
                    let settings = converted.pipeline_settings.clone().unwrap();
                    let item = ConversionItem::new(archive_path.clone(),
                        FileFormat::Audio(crate::convert::formats::AudioFormat::Flac), converted);
                    let mut request = crate::convert::pipeline::unified_request::build_pipeline_request_from_settings(
                        &item, settings).expect("planner request");
                    request.naming.template = item.options.effective_naming_template("%NN% - %TITLE%");
                    let plan = crate::convert::pipeline::stages::plan_outputs(&prepared, &request)
                        .expect("planner accepts both selected tracks");
                    let planned = &plan.entries[1].final_path;
                    assert_eq!(Path::new(&displayed), planned.as_path(),
                        "second member must use its own {pattern} metadata");
                    assert_eq!(displayed, "Second Track -.flac");
                }
            }
        }
    }

    #[test]
    fn single_flac_preview_paths_match_real_planner_for_album_artist_and_composer() {
        use crate::convert::pipeline::{AlbumMetadata, ExtractionProvenance, PreparedSource,
            PreparedTrack, SourceKind, TrackId, TrackMetadata, TrackSourceRef};
        use crate::convert::{ConversionItem, FileFormat};
        let format = FormatState::new();
        let temp = tempfile::tempdir().expect("temporary destination");
        let root = temp.path().join("destination");
        let path = Path::new("/music/input.flac");
        let metadata = SourceMetadata {
            title: Some("Title".into()), artist: Some("Solo Singer".into()),
            album_artist: Some("Various Artists".into()),
            composer: Some("A Composer".into()), ..Default::default()
        };
        let overrides = MetadataState::default();
        let context = ConvertNamingPreview {
            archive_album: None,
            source_metadata: &metadata, album_metadata: None, selected_track: None,
            archive_track: false, album_title: None, album_artist: None,
            multi_disc_proven: false, overrides: &overrides, source_path: path,
        };
        let source = PreparedSource {
            container: path.to_owned(), kind: SourceKind::SingleFile,
            tracks: vec![PreparedTrack {
                id: TrackId {source_ordinal: 1, disc_number: None, track_number: 1},
                source_ref: TrackSourceRef::StagedFile(path.to_owned()),
                metadata: TrackMetadata {
                    title: Some("Title".into()),
                    artist: Some("Solo Singer".into()).into(),
                    composer: Some("A Composer".into()).into(),
                    track_number: Some(1), ..Default::default()
                },
                expected_samples: Some(44_100), sample_rate: Some(44_100),
                source_audio: Default::default(), bit_depth: Some(24), warnings: Vec::new(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Album".into()),
                album_artist: Some("Various Artists".into()).into(),
                total_tracks: 1, ..Default::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile, source_sha256: None,
                tool_versions: Default::default(), extracted_at: chrono::Utc::now(),
            },
        };
        let mut opts = OutputOptionsState::new();
        for (pattern, expected) in [
            ("%ALBUM_ARTIST%/%TITLE%", "Various Artists/Title.flac"),
            ("%COMPOSER%/%TITLE%", "A Composer/Title.flac"),
        ] {
            opts.set_naming_template(pattern, false);
            let mut converted = crate::tui::convert_actions::try_pills_to_options(
                &format, &opts, &crate::config::TonepoetConfig::default()).unwrap();
            converted.output_dir = Some(root.clone());
            let settings = converted.pipeline_settings.clone().unwrap();
            let item = ConversionItem::new(path.to_owned(), FileFormat::Audio(
                crate::convert::formats::AudioFormat::Flac), converted);
            let request = crate::convert::pipeline::unified_request::build_pipeline_request_from_settings(
                &item, settings).expect("request");
            let plan = crate::convert::pipeline::stages::plan_outputs(&source, &request)
                .expect("real planned output");
            let actual = plan.entries[0].final_path.strip_prefix(&root)
                .expect("output under destination").to_string_lossy();
            assert_eq!(actual, expected, "actual planned output: {pattern}");
            for width in [80, 160] {
                let displayed = convert_naming_path_preview(&opts, &opts.naming_template(),
                    &format, width, None, Some(context));
                assert_eq!(displayed, actual, "preview: {pattern}, width={width}");
            }
        }
    }

    #[test]
    fn batch_cursor_preview_uses_selected_sidecar_year_and_genre() {
        use std::collections::{BTreeMap, HashSet};

        let dir = tempfile::tempdir().expect("temporary CUE album");
        let image = dir.path().join("sample.flac");
        let cue = dir.path().join("sample.cue");
        std::fs::write(&image, include_bytes!("../../tests/fixtures/silence.flac")).unwrap();
        let format = FormatState::new();
        let config = crate::config::TonepoetConfig::default();
        let overrides = MetadataState::default();
        let mut options = OutputOptionsState::new();
        for with_tags in [true, false] {
            std::fs::write(&cue, format!(
                "{}TITLE \"Cue Album\"\nFILE \"sample.flac\" FLAC\n\
                   TRACK 01 AUDIO\n    TITLE \"Opening\"\n    INDEX 01 00:00:00\n",
                if with_tags {"REM DATE 1973\nREM GENRE Jazz\n"} else {""},
            )).unwrap();
            let authority = crate::tui::app::resolve_convert_preview_authority(
                &image, &HashSet::new(), &BTreeMap::new(),
                &config.conversion.aggregate_metadata_target_priority, None,
            );
            assert_eq!(authority.cue_sidecar_override,
                Some(crate::convert::pipeline::CueSidecarPolicy::SidecarOnly));
            let authoritative = crate::tui::app::read_convert_naming_metadata_for_cue_decision(
                &image, &authority,
            ).expect("selected CUE metadata");
            let (_, cursor, notice) = crate::tui::app::probe_convert_source_for_message(
                &image, authority.cue_sidecar_override,
                authority.sidecar_cue_track_metadata.as_ref(),
            );
            assert!(notice.is_none(), "source probe notice: {notice:?}");
            assert_eq!(cursor.year, authoritative.year);
            assert_eq!(cursor.genre, authoritative.genre);
            assert_eq!(cursor.title.as_deref(), Some("Opening"));
            for disc_dirs in [false, true] {
                options.disc_subfolders.select_value(&disc_dirs);
                for (pattern, folder) in [
                    ("%YEAR%/%TITLE%", "1973"),
                    ("%GENRE%/%TITLE%", "Jazz"),
                ] {
                    options.set_naming_template(pattern, false);
                    for width in [80, 160] {
                        let context = ConvertNamingPreview {
                            archive_album: None,
                            source_metadata: &cursor, album_metadata: None,
                            selected_track: None, archive_track: false,
                            album_title: None, album_artist: None,
                            multi_disc_proven: false, overrides: &overrides,
                            source_path: &image,
                        };
                        let actual = convert_naming_path_preview(
                            &options, &options.naming_template(), &format,
                            width - 8, None, Some(context),
                        );
                        let expected = if with_tags {
                            format!("{folder}/Opening.flac")
                        } else {"Opening.flac".to_string()};
                        assert_eq!(actual, expected,
                            "{pattern}, CUE tags={with_tags}, disc dirs={disc_dirs}, width={width}");
                    }
                }
            }
        }
    }

    #[test]
    fn convert_single_flac_preview_retains_album_artist_and_composer() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let source = SourceMetadata {
            title: Some("Title".into()), artist: Some("Solo Singer".into()),
            album_artist: Some("Various Artists".into()),
            composer: Some("A Composer".into()), ..Default::default()
        };
        let overrides = MetadataState::default();
        fn ctx<'a>(source: &'a SourceMetadata, overrides: &'a MetadataState)
            -> ConvertNamingPreview<'a> {
            ConvertNamingPreview {
                archive_album: None,
                source_metadata: source, album_metadata: None, selected_track: None,
                archive_track: false, album_title: None, album_artist: None,
                multi_disc_proven: false, overrides, source_path: Path::new("input.flac"),
            }
        }
        for width in [80, 160] {
            opts.set_naming_template("%ALBUM_ARTIST%/%TITLE%", false);
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, width, None, Some(ctx(&source, &overrides))),
                "Various Artists/Title.flac");
            opts.set_naming_template("%COMPOSER%/%TITLE%", false);
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, width, None, Some(ctx(&source, &overrides))),
                "A Composer/Title.flac");
            opts.set_naming_template("%ALBUM_ARTIST%/%TITLE%", false);
            let edited_overrides = MetadataState {
                album_artist_for_conversion: Some("Edited Album Artist".into()),
                ..MetadataState::default()
            };
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, width, None, Some(ctx(&source, &edited_overrides))),
                "Edited Album Artist/Title.flac");
        }
        // A missing optional composer must not invent a folder.
        let without_composer = SourceMetadata {composer: None, ..source.clone()};
        let ctx_missing = ConvertNamingPreview {
            archive_album: None,
            source_metadata: &without_composer, album_metadata: None,
            selected_track: None, archive_track: false, album_title: None,
            album_artist: None, multi_disc_proven: false,
            overrides: &overrides, source_path: Path::new("input.flac"),
        };
        opts.set_naming_template("%COMPOSER%/%TITLE%", false);
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(ctx_missing)), "Title.flac");
    }

    #[test]
    fn optional_album_prefix_policy_matches_resolved_preview() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let metadata = SourceMetadata {
            title: Some("Second".into()), genre: Some("Jazz".into()),
            year: Some("2001".into()), ..Default::default()
        };
        let overrides = MetadataState::default();
        let ctx = ConvertNamingPreview {
            archive_album: None,
            source_metadata: &metadata, album_metadata: None,
            selected_track: None, archive_track: false,
            album_title: None, album_artist: None,
            multi_disc_proven: false, overrides: &overrides,
            source_path: Path::new("source.flac"),
        };
        let info = SourceInfo {
            format_name: "FLAC".into(), codec: "flac".into(),
            bit_depth: Some(24), sample_format_is_float: Some(false),
            compression_is_lossless: Some(true), sample_rate: 44_100,
            channels: 2, channel_layout: "stereo".into(),
            duration_secs: 1.0, file_size: 1024,
        };
        for pattern in ["%GENRE%/%TITLE%", "%YEAR%/%TITLE%", "{%YEAR%}/%TITLE%"] {
            opts.set_naming_template(pattern, false);
            assert_eq!(optional_convert_album_folder_is_empty(&opts, &format,
                Some(&info), Some(ctx)), Some(false), "present album metadata: {pattern}");
        }
        let missing = SourceMetadata { year: None, genre: None, ..metadata.clone() };
        let ctx_missing = ConvertNamingPreview { source_metadata: &missing, ..ctx };
        for pattern in ["%GENRE%/%TITLE%", "%YEAR%/%TITLE%", "{%YEAR%}/%TITLE%"] {
            opts.set_naming_template(pattern, false);
            assert_eq!(optional_convert_album_folder_is_empty(&opts, &format,
                Some(&info), Some(ctx_missing)), Some(true), "absent optional metadata: {pattern}");
        }
        // The local metadata pane can contain untransmitted edits. They must
        // not reverse the flat-output admission decision for absent tags.
        let local_edits = MetadataState {
            year: Some("1987".into()), genre: Some("Jazz".into()),
            ..MetadataState::default()
        };
        let local_ctx = ConvertNamingPreview { overrides: &local_edits, ..ctx_missing };
        for pattern in ["%GENRE%/%TITLE%", "%YEAR%/%TITLE%", "{%YEAR%}/%TITLE%"] {
            opts.set_naming_template(pattern, false);
            assert_eq!(optional_convert_album_folder_is_empty(&opts, &format,
                Some(&info), Some(local_ctx)), Some(true), "UI-only metadata: {pattern}");
            assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, 160, Some(&info), Some(local_ctx)), "Second.flac");
        }
        opts.set_naming_template("%ALBUM%/{%YEAR%}/%TITLE%", false);
        assert_eq!(optional_convert_album_folder_is_empty(&opts, &format,
            Some(&info), Some(ctx_missing)), Some(false), "stable album prefix");
    }

    #[test]
    fn batch_naming_ignores_first_cursor_year_and_genre() {
        let metadata = MetadataState {
            year: Some("2001".into()), genre: Some("Jazz".into()),
            album_artist_for_conversion: Some("Chosen Album Artist".into()),
            ..Default::default()
        };
        let overrides = convert_batch_naming_overrides(&metadata);
        assert!(overrides.year.is_none());
        assert!(overrides.genre.is_none());
        assert_eq!(overrides.album_artist_for_conversion.as_deref(),
            Some("Chosen Album Artist"));
        let mut opts = OutputOptionsState::new();
        opts.set_naming_template("{%YEAR%}/%TITLE%", false);
        let undated = SourceMetadata {title: Some("Undated".into()), ..Default::default()};
        let preview = ConvertNamingPreview {
            archive_album: None,
            source_metadata: &undated, album_metadata: None, selected_track: None,
            archive_track: false, album_title: None, album_artist: None,
            multi_disc_proven: false, overrides: &overrides,
            source_path: Path::new("undated.flac"),
        };
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &FormatState::new(), 72, None, Some(preview)), "Undated.flac");
    }

    #[test]
    fn convert_year_preview_uses_planner_date_extraction_not_local_edits() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let mut source = SourceMetadata {
            title: Some("Example".into()), year: Some("2020-05-17".into()),
            ..Default::default()
        };
        let mut overrides = MetadataState::default();
        for disc_dirs in [false, true] {
            opts.disc_subfolders.select_value(&disc_dirs);
            for pattern in ["%YEAR%/%TITLE%", "{%YEAR%}/%TITLE%"] {
                opts.set_naming_template(pattern, false);
                for (source_date, override_date, expected) in [
                    (Some("2020-05-17"), None, "2020"),
                    (Some("2020"), None, "2020"),
                    (Some("March 1971"), None, "1971"),
                    (None, None, ""),
                    (Some("2020-05-17"), Some("2005-11-01"), "2020"),
                    (None, Some("1984"), ""),
                    (Some("2020-05-17"), Some("missing"), "2020"),
                ] {
                    source.year = source_date.map(str::to_owned);
                    overrides.year = override_date.map(str::to_owned);
                    let preview = ConvertNamingPreview {
                        archive_album: None,
                        source_metadata: &source, album_metadata: None,
                        selected_track: None, archive_track: false,
                        album_title: None, album_artist: None,
                        multi_disc_proven: false, overrides: &overrides,
                        source_path: Path::new("Example.dsf"),
                    };
                    for width in [80, 160] {
                        let actual = convert_naming_path_preview(&opts, &opts.naming_template(),
                            &format, width - 8, None, Some(preview));
                        let expected = if expected.is_empty() {
                            "Example.flac".to_owned()
                        } else if pattern.starts_with('{') {
                            format!("{{{expected}}}/Example.flac")
                        } else {
                            format!("{expected}/Example.flac")
                        };
                        assert_eq!(actual, expected,
                            "pattern={pattern}, source={source_date:?}, override={override_date:?}, width={width}");
                    }
                }
            }
        }
    }

    #[test]
    fn single_flac_local_edits_do_not_change_preview_or_planned_output() {
        use crate::convert::pipeline::{AlbumMetadata, ExtractionProvenance, PreparedSource,
            PreparedTrack, SourceKind, TrackId, TrackMetadata, TrackSourceRef};
        use crate::convert::{ConversionItem, FileFormat};

        let temp = tempfile::tempdir().expect("temporary destination");
        let input = temp.path().join("input.flac");
        let root = temp.path().join("converted");
        let info = SourceInfo {
            format_name: "FLAC".into(), codec: "flac".into(),
            bit_depth: Some(24), sample_format_is_float: Some(false),
            compression_is_lossless: Some(true), sample_rate: 44_100,
            channels: 2, channel_layout: "stereo".into(),
            duration_secs: 1.0, file_size: 1024,
        };
        let mut tagged = SourceMetadata {
            title: Some("Finale".into()), artist: Some("Original Singer".into()),
            album: Some("Original Album".into()),
            track_number: Some(3), ..Default::default()
        };
        let mut edited = MetadataState {
            title: Some("Edited Title".into()), artist: Some("Edited Singer".into()),
            album: Some("Edited Album".into()), genre: Some("Jazz".into()),
            year: Some("1987".into()), ..Default::default()
        };
        let mut prepared = PreparedSource {
            container: input.clone(), kind: SourceKind::SingleFile,
            tracks: vec![PreparedTrack {
                id: TrackId { source_ordinal: 1, disc_number: None, track_number: 3 },
                source_ref: TrackSourceRef::StagedFile(input.clone()),
                metadata: TrackMetadata {
                    title: Some("Finale".into()),
                    artist: Some("Original Singer".into()).into(),
                    track_number: Some(3), ..Default::default()
                },
                expected_samples: Some(44_100), sample_rate: Some(44_100),
                source_audio: Default::default(), bit_depth: Some(24),
                warnings: Vec::new(),
            }],
            album_metadata: AlbumMetadata {
                album: Some("Original Album".into()),
                total_tracks: 1, ..Default::default()
            },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SingleFile, source_sha256: None,
                tool_versions: Default::default(), extracted_at: chrono::Utc::now(),
            },
        };
        let format = FormatState::new();
        let config = crate::config::TonepoetConfig::default();
        let mut opts = OutputOptionsState::new();
        opts.dest_path = Some(root.clone());

        // Compare the exact read-only result with a real planner invocation,
        // including the admission-time flat-output policy, for both absent
        // and present source tags while the local UI holds different values.
        let assert_preview_matches_plan = |
            opts: &OutputOptionsState,
            source: &SourceMode,
            edited: &MetadataState,
            prepared: &PreparedSource,
            expected: &str,
            flat: bool,
        | {
            let context = ConvertNamingPreview::from_source(source, opts, edited)
                .expect("selected source preview");
            let displayed = convert_naming_path_preview(opts, &opts.naming_template(),
                &format, 512, source.current_info(), Some(context));
            let mut converted = crate::tui::convert_actions::try_pills_to_options(
                &format, opts, &config).expect("valid Convert options");
            crate::tui::convert_actions::apply_optional_naming_folder_for_source(
                &mut converted, opts, &format, source, edited);
            assert_eq!(converted.flat_output, flat, "flat-output policy: {expected}");
            let settings = converted.pipeline_settings.clone().expect("pipeline settings");
            let item = ConversionItem::new(input.clone(), FileFormat::Audio(
                crate::convert::formats::AudioFormat::Flac), converted);
            let mut request = crate::convert::pipeline::unified_request::build_pipeline_request_from_settings(
                &item, settings).expect("conversion request");
            request.naming.template = item.options.effective_naming_template("%NN% - %TITLE%");
            let plan = crate::convert::pipeline::stages::plan_outputs(prepared, &request)
                .expect("real planned output");
            let planned = &plan.entries[0].final_path;
            assert_eq!(planned.strip_prefix(&root).expect("under destination"),
                Path::new(expected));
            assert_eq!(displayed, planned.to_string_lossy().as_ref(),
                "preview must match planner for {expected}");
        };

        let ordinary = SourceMode::Single {
            path: input.clone(), info: Some(info.clone()), metadata: tagged.clone(),
            probe_notice: None,
        };
        for (pattern, expected, flat) in [
            ("{%YEAR%}/%TITLE%", "Finale.flac", true),
            ("%GENRE%/%TITLE%", "Finale.flac", true),
            ("%ALBUM%/%TITLE%", "Original Album/Finale.flac", false),
            ("%ARTIST% - %TITLE%", "Original Singer - Finale.flac", true),
        ] {
            opts.set_naming_template(pattern, false);
            assert_preview_matches_plan(&opts, &ordinary, &edited, &prepared, expected, flat);
        }

        tagged.year = Some("1987".into());
        tagged.disc_number = Some(2);
        tagged.disc_total = Some(3);
        prepared.album_metadata.date = Some("1987".into());
        prepared.album_metadata.disc_number = Some(2);
        prepared.album_metadata.total_discs = Some(3);
        prepared.tracks[0].metadata.date = Some("1987".into());
        prepared.tracks[0].metadata.disc_number = Some(2);
        prepared.tracks[0].id.disc_number = Some(2);
        edited.year = Some("2001".into());
        let dated = SourceMode::Single {
            path: input.clone(), info: Some(info), metadata: tagged,
            probe_notice: None,
        };
        opts.disc_subfolders.select_value(&true);
        opts.set_naming_template("{%YEAR%}/%TRACKNN% - %TITLE%", false);
        assert_preview_matches_plan(&opts, &dated, &edited, &prepared,
            "{1987}/disc 02/03 - Finale.flac", false);
    }

    #[test]
    fn convert_preview_matches_disc_evidence_and_optional_folder_policy() {
        let mut opts = OutputOptionsState::new();
        let format = FormatState::new();
        let metadata = SourceMetadata {
            title: Some("Finale".into()),
            album: Some("Album".into()),
            track_number: Some(3),
            disc_number: Some(2),
            disc_total: Some(3),
            ..SourceMetadata::default()
        };
        let overrides = MetadataState::default();
        let context = ConvertNamingPreview {
            archive_album: None,
            source_metadata: &metadata, album_metadata: None,
            selected_track: None, archive_track: false,
            album_title: None, album_artist: None,
            multi_disc_proven: false, overrides: &overrides,
            source_path: Path::new("source.flac"),
        };
        opts.set_naming_template("%ALBUM%/%TRACKNN% - %TITLE%", false);
        for (enabled, expected) in [
            (false, "Album/03 - Finale.flac"),
            (true, "Album/disc 02/03 - Finale.flac"),
        ] {
            opts.disc_subfolders.select_value(&enabled);
            for width in [80, 160] {
                let result = convert_naming_path_preview(&opts, &opts.naming_template(),
                    &format, width - 7, None, Some(context));
                assert_eq!(result, expected, "disc dirs={enabled}, width={width}");
            }
        }
        opts.disc_subfolders.select_value(&false);
        opts.set_naming_template("%ALBUM%/%DISC%/%TITLE%", false);
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context)), "Album/2/Finale.flac");
        opts.set_naming_template("%ALBUM%/{%YEAR%}/%TRACKNN% - %TITLE%", false);
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context)), "Album/03 - Finale.flac");
        // Year evidence must come from the probed source, not the editable
        // Convert metadata pane. Preserve album-before-disc ordering.
        let with_year = SourceMetadata { year: Some("1987".into()), ..metadata.clone() };
        let context_with_year = ConvertNamingPreview { source_metadata: &with_year, ..context };
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context_with_year)), "Album/{1987}/03 - Finale.flac");
        opts.set_naming_template("{%YEAR%}/%TRACKNN% - %TITLE%", false);
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context)), "03 - Finale.flac");
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context_with_year)), "{1987}/03 - Finale.flac");
        opts.disc_subfolders.select_value(&true);
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context)), "disc 02/03 - Finale.flac");
        assert_eq!(convert_naming_path_preview(&opts, &opts.naming_template(),
            &format, 160, None, Some(context_with_year)), "{1987}/disc 02/03 - Finale.flac");

        opts.dest_path = Some(Path::new("/home/user/Music/Long Conversion Destination").into());
        let name = "disc 02/03 - Finale.flac";
        let full = format!("/home/user/Music/Long Conversion Destination/{name}");
        for width in [80, 160] {
            let preview = convert_naming_path_preview(&opts, &opts.naming_template(),
                &format, (width - 7) as usize, None, Some(context));
            assert!(preview.ends_with(name), "constructed name clipped at {width}: {preview}");
            if width == 160 { assert_eq!(preview, full); }
        }
    }

    #[test]
    fn convert_output_options_draws_one_naming_line_and_a_read_only_path() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut opts = OutputOptionsState::new();
        opts.set_naming_template("my album/%TRACKNN% - %TITLE%", true);
        opts.dest_path = Some(Path::new("/home/user/Music/Converted").to_path_buf());
        let format = FormatState::new();
        for cols in [80, 100, 160] {
            let mut terminal = Terminal::new(TestBackend::new(cols, 24)).unwrap();
            terminal.draw(|frame| draw_output_options_pane(
                frame, Rect::new(0, 0, cols, 15), &opts, None, 0,
                &format, true, false, true, theme,
            )).unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| (1..cols-1).map(|x| buffer.get(x,y).symbol().to_owned())
                .collect::<Vec<_>>().join("");
            assert!(row(2).contains("naming"));
            assert!(row(2).contains(" load ") && row(2).contains(" custom "));
            assert!(row(3).contains("→ ") && row(3).contains("My Album"),
                "naming preview at {cols}: {}", row(3));
            assert!(!row(3).contains(" load ") && !row(3).contains(" custom "));
            assert!(row(4).contains("merge"));
        }
    }

    #[test]
    fn output_options_registration_includes_template_pills_with_concrete_targets() {
        let opts = OutputOptionsState::new();
        let mut buttons = ButtonRenderMap::new();
        let area = Rect::new(10, 4, 80, 20);

        register_output_options_mouse_targets(&mut buttons, area, &opts, true, true);

        let naming_y = area.y + 2;
        let preview_y = area.y + 3;
        let build_x = area.x + area.width - 1 - OUTPUT_OPTIONS_TEMPLATE_BUILD_WIDTH;
        let load_x = build_x - OUTPUT_OPTIONS_TEMPLATE_GAP - OUTPUT_OPTIONS_TEMPLATE_LOAD_WIDTH;

        assert_eq!(
            buttons.find_button_at(load_x, naming_y),
            Some(TuiButton::TemplateLoadFilenameButton),
            "Naming uses the shared filename-template library"
        );
        assert_eq!(
            buttons.find_button_at(build_x, naming_y),
            Some(TuiButton::TemplateBuildFilenameButton),
            "Naming opens the shared custom editor"
        );
        assert_eq!(buttons.find_button_at(load_x, preview_y), None);
        assert_eq!(buttons.find_button_at(build_x, preview_y), None);
        assert!(buttons.recorded_buttons().iter().all(|(button, _)|
            !matches!(button, TuiButton::TemplateLoadFolderButton
                | TuiButton::TemplateBuildFolderButton
                | TuiButton::FolderTemplateField)));

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
        assert!(buttons.recorded_buttons().iter().any(|(button, rect)|
            matches!(button, TuiButton::PartialPill(index) if *index < opts.partial.options.len())
                && rect.y == area.y + OUTPUT_OPTIONS_PARTIAL_ROW),
            "the compact pane must expose concrete Partial targets");
        assert!(buttons.recorded_buttons().iter().any(|(button, rect)|
            matches!(button, TuiButton::IfExistsPill(index) if *index < opts.if_exists.options.len())
                && rect.y == area.y + OUTPUT_OPTIONS_IF_EXISTS_ROW),
            "the compact pane must expose concrete IfExists targets");
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
            buttons.find_button_at(11, 4 + actions_row_for_height(20).unwrap()),
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

//! Browse Info artwork: shallow album discovery, bounded asynchronous decoding,
//! and terminal-protocol state owned outside the immediate-mode draw pass.
//!
//! One discovery and one decoding job can be active at any time. Rapid cursor
//! motion is debounced; superseded results are discarded by context/source
//! identity, so large or remote albums cannot spawn unbounded worker threads.

use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageReader, Limits};
use lofty::picture::PictureType;
use ratatui::{buffer::Buffer, layout::Rect, style::Color};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::config::BrowseArtworkConfig;
use super::browse::{BrowseState, EntryKind, FolderClassificationKind};

const SELECTION_SETTLE: Duration = Duration::from_millis(130);
const MAX_DIRECTORY_ENTRIES: usize = 32_768;
const MAX_FOLDER_IMAGES: usize = 128;
const MAX_ENCODED_IMAGE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 12_000;
const MAX_DECODE_ALLOC: u64 = 96 * 1024 * 1024;
const MAX_PREVIEW_EDGE: u32 = 1_600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtworkContext {
    /// Info-pane discovery: one album/disc directory, no recursive traversal.
    Album { directory: PathBuf, selected_audio: Option<PathBuf> },
    /// Direct image View: decode this file and discover sibling image names only.
    SingleFile(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageVerticalPlacement {
    Center,
    Bottom,
}

impl ArtworkContext {
    /// Never follow Browse's synthetic `..`, archive members, or symlinks.
    /// A directory must already be classified as album/disc content; ordinary
    /// folders retain their full Info metrics instead of reserving image rows.
    /// This is a cached lookup only, never filesystem I/O during rendering.
    pub fn from_browse(browse: &BrowseState) -> Option<Self> {
        if browse.is_in_archive() { return None; }
        let entry = browse.selected_entry()?;
        if entry.is_symlink || matches!(entry.kind, EntryKind::ParentDir) { return None; }
        if entry.is_child_dir() && browse.current_folder_classification().is_some_and(|c|
            matches!(c.kind, FolderClassificationKind::Album | FolderClassificationKind::Disc
                | FolderClassificationKind::MultiDisc))
        {
            return Some(Self::Album { directory: entry.path.clone(), selected_audio: None });
        }
        if entry.is_audio() && entry.is_regular_file {
            return Some(Self::Album {
                directory: entry.path.parent()?.to_path_buf(),
                selected_audio: Some(entry.path.clone()),
            });
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtworkSource {
    // One cycling entry per album, regardless of the number of audio tracks.
    // Sharing the list avoids copying it when a decode worker is launched.
    Embedded(Arc<[PathBuf]>),
    File(PathBuf),
}

impl ArtworkSource {
    pub fn label(&self) -> String {
        match self {
            Self::Embedded(paths) => paths.first().map_or_else(
                || "Embedded artwork".to_string(),
                |path| format!("Embedded: {}", path.file_name().unwrap_or_default().to_string_lossy()),
            ),
            Self::File(path) => path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        }
    }
}

struct ScanResult {
    context: ArtworkContext,
    sources: Vec<ArtworkSource>,
    warning: Option<String>,
    generation: u64,
}

struct DecodedArtwork {
    image: DynamicImage,
    embedded_path: Option<PathBuf>,
}

struct DecodeResult {
    generation: u64,
    source: ArtworkSource,
    image: Result<DecodedArtwork, String>,
}

/// Non-clonable, session-only graphics state. This cache is deliberately not
/// stored with a Browse tab or in persistent configuration.
pub struct BrowseArtworkState {
    context: Option<ArtworkContext>,
    generation: u64,
    scan_generation: u64,
    decode_generation: u64,
    changed_at: Instant,
    scanned: Option<ArtworkContext>,
    scan_rx: Option<Receiver<ScanResult>>,
    sources: Vec<ArtworkSource>,
    cursor: usize,
    // Net key/click navigation while the worker-owned sibling scan is pending.
    // Apply once, modulo the final ordered list, without extra decode jobs.
    queued_gallery_offset: i64,
    failed: HashSet<usize>,
    decode_rx: Option<Receiver<DecodeResult>>,
    decoded_source: Option<ArtworkSource>,
    decoded_embedded_path: Option<PathBuf>,
    decoded: Option<DynamicImage>,
    protocol: Option<Box<dyn StatefulProtocol>>,
    desired_area: Rect,
    vertical_placement: ImageVerticalPlacement,
    protocol_area: Rect,
    protocol_generation: usize,
    retransmit_generation: usize,
    font_size: (u16, u16),
    prepared_protocol_has_fixed_origin: bool,
    // An inline single-click cycles; a subsequent double-click must open the
    // image that was under the first click, not the next image.
    inline_click_source: Option<ArtworkSource>,
    warning: Option<String>,
    pub popup_open: bool,
    pub popup_rect: Rect,
    // Native graphics may have altered terminal pixels outside the ratatui diff.
    // Restore only when terminal graphics are newly placed, invalidated, or
    // removed, not on every unrelated (often startup-driven) UI redraw.
    needs_background_restore: bool,
    // Alternate the invisible ink on each ACTUAL restoration. Frame parity
    // cannot guarantee a change when intervening frames don't repaint.
    background_restore_count: usize,
    // A context menu painted over native artwork may overwrite terminal pixels
    // without modifying Ratatui's image-origin cell. Restore once when that
    // menu closes or moves; never on hover or unrelated startup redraws.
    artwork_menu_occludes_image: bool,
    repaint_image_after_menu: bool,
    native_image_restore_count: usize,
}

impl Default for BrowseArtworkState {
    fn default() -> Self {
        Self {
            context: None, generation: 0, scan_generation: 0, decode_generation: 0, changed_at: Instant::now(), scanned: None,
            scan_rx: None, sources: Vec::new(), cursor: 0,
            queued_gallery_offset: 0, failed: HashSet::new(),
            decode_rx: None, decoded_source: None, decoded_embedded_path: None, decoded: None,
            protocol: None, desired_area: Rect::default(),
            vertical_placement: ImageVerticalPlacement::Center, protocol_area: Rect::default(),
            protocol_generation: 0, retransmit_generation: 0, font_size: (8, 12),
            prepared_protocol_has_fixed_origin: false,
            inline_click_source: None, warning: None, popup_open: false, popup_rect: Rect::default(),
            needs_background_restore: false, background_restore_count: 0,
            artwork_menu_occludes_image: false, repaint_image_after_menu: false,
            native_image_restore_count: 0,
        }
    }
}

impl BrowseArtworkState {
    pub fn observe(&mut self, context: Option<ArtworkContext>) {
        if self.context == context { return; }
        self.needs_background_restore |= self.decoded.is_some() || self.protocol.is_some();
        self.context = context;
        self.generation = self.generation.wrapping_add(1);
        self.changed_at = Instant::now();
        self.scanned = None;
        self.clear_selection();
        // Keep the in-flight receivers until they complete to cap concurrency.
        self.popup_open = false;
        self.warning = None;
    }

    fn clear_selection(&mut self) {
        self.sources.clear();
        self.cursor = 0;
        self.queued_gallery_offset = 0;
        self.artwork_menu_occludes_image = false;
        self.repaint_image_after_menu = false;
        self.native_image_restore_count = 0;
        self.failed.clear();
        self.decoded_source = None;
        self.decoded_embedded_path = None;
        self.decoded = None;
        self.protocol = None;
        self.protocol_area = Rect::default();
        self.inline_click_source = None;
    }

    /// User-initiated reload or discovery-policy change. Keep in-flight work
    /// owned, but reject its result if the policy has changed in the meantime.
    pub fn invalidate(&mut self) {
        self.needs_background_restore |= self.decoded.is_some() || self.protocol.is_some();
        self.generation = self.generation.wrapping_add(1);
        self.scanned = None;
        self.changed_at = Instant::now();
        self.clear_selection();
        self.popup_open = false;
        self.warning = None;
    }

    pub fn suspend(&mut self) {
        self.popup_open = false;
        self.desired_area = Rect::default();
        self.vertical_placement = ImageVerticalPlacement::Center;
        if self.context.is_some() {
            self.observe(None);
        }
    }

    pub fn take_background_restore(&mut self) -> bool {
        let needed = std::mem::take(&mut self.needs_background_restore);
        if needed {
            self.background_restore_count = self.background_restore_count.wrapping_add(1);
        }
        needed
    }

    pub fn background_restore_count(&self) -> usize {
        self.background_restore_count
    }

    pub fn invalidate_protocol(&mut self) {
        self.protocol = None;
        self.protocol_area = Rect::default();
    }

    /// Track the artwork menu independently of decode and protocol readiness.
    /// A menu opened during asynchronous decoding may cover the first image
    /// frame, and a replacement menu may open while restoration is pending.
    pub fn artwork_menu_opened(&mut self) {
        self.artwork_menu_occludes_image = true;
    }

    /// Called only when an artwork menu closes or changes position. Rebuild
    /// native protocol state, and force its origin command through the terminal
    /// diff on the next visible image frame. A protocol rebuild alone cannot
    /// restore Sixel/iTerm pixels when the origin buffer cell is unchanged.
    pub fn artwork_menu_finished(&mut self, native_image_still_visible: bool) {
        let menu_was_open = std::mem::take(&mut self.artwork_menu_occludes_image);
        if !menu_was_open || !native_image_still_visible || self.decoded.is_none() {
            return;
        }
        // A previous dismissal can already have invalidated the protocol.
        // Do not cancel its pending one-shot restoration when another menu
        // opens and closes before a frame can rebuild and render the image.
        if self.protocol.is_some() {
            self.invalidate_protocol();
            self.needs_background_restore = true;
            self.repaint_image_after_menu = true;
        }
    }

    pub fn set_desired_area(&mut self, area: Rect, placement: ImageVerticalPlacement) {
        self.desired_area = area;
        self.vertical_placement = placement;
    }

    /// A normal click retains Browse Info's existing cycle behavior. Remember
    /// the originally displayed source so double-click opens THAT artwork.
    pub fn inline_click(&mut self) -> bool {
        self.inline_click_source = self.sources.get(self.cursor).cloned();
        self.cycle(true)
    }

    pub fn open_inline_popup_on_double_click(&mut self) -> bool {
        let Some(original) = self.inline_click_source.take() else { return false; };
        if let Some(index) = self.sources.iter().position(|source| *source == original) {
            if index != self.cursor {
                self.cursor = index;
                self.generation = self.generation.wrapping_add(1);
                self.decoded_source = None;
                self.decoded_embedded_path = None;
                self.decoded = None;
            }
        }
        if !self.has_candidate() { return false; }
        self.popup_open = true;
        self.invalidate_protocol();
        true
    }

    /// A direct image popup is independent of Info's album selection, which
    /// may be None when the selected Browse entry is a PNG/JPEG/etc.
    pub fn is_single_file_popup(&self) -> bool {
        self.popup_open && matches!(&self.context, Some(ArtworkContext::SingleFile(_)))
    }

    pub fn open_file_popup(&mut self, path: PathBuf) {
        self.observe(Some(ArtworkContext::SingleFile(path.clone())));
        // Show the requested file immediately while a bounded, nonrecursive
        // worker enumerates other images in its directory for viewer cycling.
        self.scanned = None;
        self.changed_at = Instant::now() - SELECTION_SETTLE;
        self.sources = vec![ArtworkSource::File(path)];
        self.queued_gallery_offset = 0;
        self.popup_open = true;
        self.popup_rect = Rect::default();
        self.desired_area = Rect::default();
        self.vertical_placement = ImageVerticalPlacement::Center;
        self.invalidate_protocol();
    }

    pub fn has_image(&self) -> bool { self.decoded.is_some() }
    /// The popup's hit target and background must match the *displayed*
    /// pixels, not the larger viewport used to fit them. Both this and
    /// render_image use the same cell/pixel geometry and scaling policy.
    fn popup_image_area(&self, viewport: Rect, allow_upscale: bool) -> Option<Rect> {
        let image = self.decoded.as_ref()?;
        let rect = fitted_image_area(viewport, image.width(), image.height(), self.font_size,
            allow_upscale, ImageVerticalPlacement::Center);
        (rect.width > 0 && rect.height > 0).then_some(rect)
    }
    pub fn has_candidate(&self) -> bool { self.failed.len() < self.sources.len() }
    pub fn count(&self) -> usize { self.sources.len() }
    pub fn index(&self) -> usize { self.cursor }
    pub fn current_label(&self) -> Option<String> {
        if matches!(self.sources.get(self.cursor), Some(ArtworkSource::Embedded(_))) {
            if let Some(path) = &self.decoded_embedded_path {
                return Some(format!("Embedded: {}", path.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
        self.sources.get(self.cursor).map(ArtworkSource::label)
    }

    pub fn message(&self) -> &str {
        if self.context.is_none() { return ""; }
        if self.decoded.is_some() { return ""; }
        if self.scanned.is_none() || (self.scan_rx.is_some() && self.sources.is_empty()) {
            return if matches!(&self.context, Some(ArtworkContext::SingleFile(_))) {
                "Loading image…"
            } else { "Finding artwork…" };
        }
        if self.decode_rx.is_some() || !self.sources.is_empty() && self.failed.len() < self.sources.len() {
            return "Loading artwork…";
        }
        if matches!(&self.context, Some(ArtworkContext::Album { .. })) { return ""; }
        self.warning.as_deref().unwrap_or("No image available")
    }

    pub fn cycle(&mut self, forward: bool) -> bool {
        if self.sources.len() < 2 {
            // The requested image is available before its siblings. Preserve
            // *every* navigation step without enumerating the directory on the
            // UI thread or launching a decode for each intermediate image.
            if self.is_single_file_popup() && self.scanned != self.context {
                self.queued_gallery_offset = self.queued_gallery_offset.saturating_add(
                    if forward { 1 } else { -1 });
                return true;
            }
            return false;
        }
        let len = self.sources.len();
        for step in 1..len {
            let next = if forward { (self.cursor + step) % len }
                else { (self.cursor + len - (step % len)) % len };
            if !self.failed.contains(&next) {
                self.needs_background_restore |= self.decoded.is_some() || self.protocol.is_some();
                self.cursor = next;
                self.generation = self.generation.wrapping_add(1);
                self.decoded_source = None;
                self.decoded_embedded_path = None;
                self.decoded = None;
                self.protocol = None;
                self.protocol_area = Rect::default();
                return true;
            }
        }
        false
    }

    pub fn close_popup(&mut self) {
        let was_file_view = self.is_single_file_popup();
        self.needs_background_restore = true;
        self.popup_open = false;
        self.popup_rect = Rect::default();
        self.inline_click_source = None;
        self.artwork_menu_occludes_image = false;
        self.repaint_image_after_menu = false;
        self.native_image_restore_count = 0;
        self.protocol = None; // rebuild for the pane on the next prepare pass
        self.protocol_area = Rect::default();
        if was_file_view {
            // Restore Info's ordinary selection on the next draw. Reject any
            // file-view worker result that arrives after the popup closes.
            self.observe(None);
            self.desired_area = Rect::default();
        }
    }

    /// This is invoked before/after terminal draws, never within the renderer.
    /// Filesystem traversal and image decoding always run on worker threads.
    pub fn prepare(&mut self, picker: &mut Picker, picker_generation: usize,
                   kitty_generation: usize, config: &BrowseArtworkConfig) -> bool {
        let mut changed = self.poll_workers();
        if self.scan_rx.is_none() && self.scanned != self.context
            && self.context.is_some() && self.changed_at.elapsed() >= SELECTION_SETTLE
        {
            let context = self.context.as_ref().expect("tested").clone();
            let policy = config.clone().normalized();
            let (tx, rx) = mpsc::channel();
            self.scan_rx = Some(rx);
            let generation = self.generation;
            self.scan_generation = generation;
            thread::spawn(move || {
                let mut result = scan_directory(context, &policy);
                result.generation = generation;
                let _ = tx.send(result);
            });
            changed = true;
        }
        if self.decode_rx.is_none() && self.decoded.is_none()
            && !self.sources.is_empty() && self.failed.len() < self.sources.len()
        {
            if let Some(source) = self.sources.get(self.cursor).cloned() {
                let (tx, rx) = mpsc::channel();
                self.decode_rx = Some(rx);
                let generation = self.generation;
                self.decode_generation = generation;
                thread::spawn(move || {
                    let image = decode_source(&source);
                    let _ = tx.send(DecodeResult { generation, source, image });
                });
                changed = true;
            }
        }
        let font_size = picker.font_size;
        self.font_size = font_size;
        self.prepared_protocol_has_fixed_origin =
            picker.protocol_type != ratatui_image::picker::ProtocolType::Halfblocks
            && picker.protocol_type != ratatui_image::picker::ProtocolType::Kitty;
        if !self.prepared_protocol_has_fixed_origin {
            // Kitty encodes its ID in the placeholder. Halfblocks uses cell
            // foregrounds for real pixels. Neither may inherit Sixel/iTerm
            // origin-cell ink after a terminal protocol change.
            self.native_image_restore_count = 0;
        }
        if picker.protocol_type == ratatui_image::picker::ProtocolType::Halfblocks {
            self.repaint_image_after_menu = false;
        }
        let desired = self.decoded.as_ref().map_or(self.desired_area, |image| {
            fitted_image_area(self.desired_area, image.width(), image.height(), font_size,
                config.allow_upscale, self.vertical_placement)
        });
        let kitty = if picker.protocol_type == ratatui_image::picker::ProtocolType::Kitty {
            kitty_generation
        } else { 0 };
        if desired.width > 0 && desired.height > 0 && self.decoded.is_some()
            && (self.protocol.is_none() || self.protocol_area != desired
                || self.protocol_generation != picker_generation || self.retransmit_generation != kitty)
        {
            self.protocol = self.decoded.as_ref().map(|image| picker.new_resize_protocol(image.clone()));
            if picker.protocol_type != ratatui_image::picker::ProtocolType::Halfblocks {
                // A new native graphics placement may have modified the
                // terminal outside Ratatui's buffer. Restore it on this draw.
                self.needs_background_restore = true;
            }
            self.protocol_area = desired;
            self.protocol_generation = picker_generation;
            self.retransmit_generation = kitty;
            changed = true;
        }
        changed
    }

    fn poll_workers(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = self.scan_rx.take() {
            match rx.try_recv() {
                Ok(result) => {
                    if result.generation == self.generation && self.context.as_ref() == Some(&result.context) {
                        let direct_file = matches!(&result.context, ArtworkContext::SingleFile(_));
                        let selected = direct_file.then(|| self.sources.get(self.cursor).cloned())
                            .flatten();
                        self.scanned = Some(result.context);
                        self.sources = result.sources;
                        self.cursor = selected.as_ref().and_then(|source|
                            self.sources.iter().position(|candidate| candidate == source))
                            .unwrap_or(0);
                        self.failed.clear();
                        // Don't throw away the selected file's pending/decoded
                        // pixels when sibling discovery completes.
                        if !direct_file || self.decoded_source.as_ref() != self.sources.get(self.cursor) {
                            self.decoded = None;
                            self.decoded_source = None;
                            self.decoded_embedded_path = None;
                            self.protocol = None;
                        }
                        self.warning = result.warning;
                        if direct_file {
                            let offset = std::mem::take(&mut self.queued_gallery_offset);
                            if !self.sources.is_empty() {
                                let len = self.sources.len() as i64;
                                let next = (self.cursor as i64 + offset.rem_euclid(len))
                                    .rem_euclid(len) as usize;
                                if next != self.cursor {
                                    self.needs_background_restore |=
                                        self.decoded.is_some() || self.protocol.is_some();
                                    self.cursor = next;
                                    self.generation = self.generation.wrapping_add(1);
                                    self.decoded_source = None;
                                    self.decoded_embedded_path = None;
                                    self.decoded = None;
                                    self.invalidate_protocol();
                                }
                            }
                        }
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => self.scan_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    // A worker panic must never lead to an automatic retry loop.
                    if self.scan_generation == self.generation {
                        self.scanned = self.context.clone();
                        self.queued_gallery_offset = 0;
                        self.warning = Some("Artwork discovery failed".into());
                        changed = true;
                    }
                }
            }
        }
        if let Some(rx) = self.decode_rx.take() {
            match rx.try_recv() {
                Ok(result) => {
                    if result.generation == self.generation && self.sources.get(self.cursor) == Some(&result.source) {
                        match result.image {
                            Ok(decoded) => {
                                self.decoded_source = Some(result.source);
                                self.decoded_embedded_path = decoded.embedded_path;
                                self.decoded = Some(decoded.image);
                                self.protocol = None;
                            }
                            Err(error) => {
                                self.failed.insert(self.cursor);
                                self.warning = Some(error);
                                self.cycle(true);
                            }
                        }
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => self.decode_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    if self.decode_generation == self.generation {
                        self.failed.insert(self.cursor);
                        self.cycle(true);
                        self.warning = Some("Artwork decode worker failed".into());
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// The renderer merely records the geometry and paints prepared protocol
    /// state; no image file I/O, decode, or protocol construction occurs here.
    pub fn render_image(&mut self, f: &mut ratatui::Frame, area: Rect,
                        allow_upscale: bool, placement: ImageVerticalPlacement,
                        background: Color) -> bool {
        self.set_desired_area(area, placement);
        if self.decoded.is_none() || self.protocol.is_none() { return false; }
        let image = self.decoded.as_ref().expect("checked");
        let target = fitted_image_area(area, image.width(), image.height(), self.font_size,
            allow_upscale, placement);
        if target.width == 0 || target.height == 0 || self.protocol_area != target { return false; }
        // The image protocol needs the actual UI background when padding or
        // compositing source pixels, not the terminal's default background.
        let (r, g, b) = super::theme::rgb_tuple(background);
        let widget = ratatui_image::StatefulImage::new(Some(image::Rgb([r, g, b])));
        f.render_stateful_widget(widget, target,
            self.protocol.as_mut().expect("checked"));
        // The pinned image crate's other native types are Sixel and iTerm2;
        // unlike Kitty they have no fresh image ID to change the image-origin
        // cell after an otherwise byte-identical retransmission.
        let fixed_origin_protocol = self.prepared_protocol_has_fixed_origin;
        if self.repaint_image_after_menu {
            // Sixel/iTerm commands may remain byte-identical after a rebuild.
            // Kitty creates a fresh image ID on protocol invalidation, so its
            // placeholder colors must NOT be overridden with invisible ink.
            if fixed_origin_protocol {
                self.native_image_restore_count = self.native_image_restore_count
                    .wrapping_add(1).max(1);
            }
            self.repaint_image_after_menu = false;
        }
        if fixed_origin_protocol && self.native_image_restore_count != 0 {
            // Keep this same ink on subsequent ordinary redraws; reverting to
            // Color::Reset would retransmit the image a second, unrelated time.
            // Never change the native protocol's skipped image cells.
            let ink = background_repaint_ink(background, self.native_image_restore_count);
            f.buffer_mut().get_mut(target.x, target.y).set_fg(ink);
        }
        true
    }
}


/// Keep Ratatui's diff in sync with terminal graphics outside the fitted image.
/// Sixel/Kitty/iTerm can change actual terminal pixels without changing any
/// normal cell. Repainting blank theme-background cells is necessary even if
/// Ratatui still believes their contents are identical to the previous frame.
/// Alternating the *invisible* foreground on successive restoration passes
/// forces diff updates without changing the displayed color or touching text
/// or graphics-protocol cells. Both choices must be explicit RGB values.
fn background_repaint_ink(background: Color, restore_count: usize) -> Color {
    let (r, g, b) = super::theme::rgb_tuple(background);
    if restore_count % 2 == 0 {
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(if r == u8::MAX { r - 1 } else { r + 1 }, g, b)
    }
}

pub(crate) fn repaint_image_background(
    f: &mut ratatui::Frame, area: Rect, background: Color, restore_count: usize,
) {
    let ink = background_repaint_ink(background, restore_count);
    repaint_blank_background_cells(f.buffer_mut(), area, background, ink);
}

fn repaint_blank_background_cells(buffer: &mut Buffer, area: Rect, background: Color, ink: Color) {
    let area = area.intersection(buffer.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = buffer.get_mut(x, y);
            // Image widgets mark covered cells as skipped. Do not overwrite their
            // escape sequences or controls, and do not recolor other UI surfaces.
            if !cell.skip && cell.symbol() == " " && cell.bg == background {
                cell.set_fg(ink);
            }
        }
    }
}

pub fn percent_height(available: u16, percent: u8) -> u16 {
    if available == 0 { return 0; }
    let height = (u32::from(available) * u32::from(percent) + 50) / 100;
    height.max(1).min(u32::from(available)) as u16
}

/// Maintain the image aspect ratio in terminal *pixel* geometry, not merely
/// cell counts. Fit computes the size before rendering, so the actual image
/// remains centered even when only one dimension needs shrinking.
fn fitted_image_area(area: Rect, pixel_width: u32, pixel_height: u32,
                     font_size: (u16, u16), allow_upscale: bool,
                     placement: ImageVerticalPlacement) -> Rect {
    if area.width == 0 || area.height == 0 || pixel_width == 0 || pixel_height == 0 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let cell_w = pixel_width.div_ceil(u32::from(font_size.0.max(1)));
    let cell_h = pixel_height.div_ceil(u32::from(font_size.1.max(1)));
    let available_w = u32::from(area.width);
    let available_h = u32::from(area.height);
    // Scale as a rational fraction to avoid overflow, rounding drift and NaN.
    let (numerator, denominator): (u64, u64) =
        if !allow_upscale && cell_w <= available_w && cell_h <= available_h {
            (1, 1)
        } else if u64::from(available_w) * u64::from(cell_h)
            <= u64::from(available_h) * u64::from(cell_w)
        {
            (u64::from(available_w), u64::from(cell_w))
        } else {
            (u64::from(available_h), u64::from(cell_h))
        };
    let fit = |natural: u32, limit: u32| -> u16 {
        ((u64::from(natural) * numerator + denominator / 2) / denominator)
            .max(1).min(u64::from(limit)) as u16
    };
    let width = fit(cell_w, available_w);
    let height = fit(cell_h, available_h);
    let y = match placement {
        ImageVerticalPlacement::Center => area.y.saturating_add((area.height - height) / 2),
        ImageVerticalPlacement::Bottom => area.bottom().saturating_sub(height),
    };
    Rect::new(area.x.saturating_add((area.width - width) / 2), y, width, height)
}

pub fn fitted_without_upscale(area: Rect, pixel_width: u32, pixel_height: u32,
                              font_size: (u16, u16)) -> Rect {
    fitted_image_area(area, pixel_width, pixel_height, font_size, false,
        ImageVerticalPlacement::Center)
}

pub(crate) fn is_supported_image(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
    })
}

/// Read only direct entries. A supported image-name symlink is eligible only
/// when its target resolves to an ordinary file. Never traverse linked folders.
fn scan_directory(context: ArtworkContext, config: &BrowseArtworkConfig) -> ScanResult {
    let (directory, selected_audio) = match &context {
        ArtworkContext::Album { directory, selected_audio } => (directory, selected_audio),
        ArtworkContext::SingleFile(path) => {
            // This explicit View scans only sibling image names. Never scan
            // audio tags, descend into subfolders, or decode sibling images.
            return scan_image_siblings(context.clone(), path);
        }
    };
    let mut files = Vec::new();
    let mut audio = Vec::new();
    let mut warning = None;
    if !fs::symlink_metadata(directory).is_ok_and(|md| md.is_dir()) {
        return ScanResult {
            context, sources: Vec::new(), warning: Some("Album folder is unavailable".into()),
            generation: 0,
        };
    }
    match fs::read_dir(directory) {
        Ok(listing) => {
            for (index, result) in listing.enumerate() {
                if index >= MAX_DIRECTORY_ENTRIES {
                    warning = Some("Artwork scan capped in unusually large folder".into());
                    break;
                }
                let Ok(entry) = result else { continue; };
                let Ok(file_type) = entry.file_type() else { continue; };
                let path = entry.path();
                if is_supported_image(&path) {
                    // Resolve metadata only for image symlinks. Regular images
                    // keep the fast dirent path; linked directories are skipped.
                    if file_type.is_file() || (file_type.is_symlink()
                        && fs::metadata(&path).is_ok_and(|md| md.is_file()))
                    {
                        // Rank the bounded listing before truncation so a
                        // preferred cover does not depend on readdir order.
                        files.push(path);
                    }
                } else if file_type.is_file() && crate::convert::classify::is_audio_file_path(&path) {
                    audio.push(path);
                }
            }
        }
        Err(error) => warning = Some(format!("Cannot read album artwork: {error}")),
    }
    files.sort_by(|a, b| {
        let key = |path: &PathBuf| {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            let rank = config.filenames.iter().position(|v| v.eq_ignore_ascii_case(&name))
                .unwrap_or(usize::MAX);
            (rank, name)
        };
        key(a).cmp(&key(b))
    });
    if files.len() > MAX_FOLDER_IMAGES {
        files.truncate(MAX_FOLDER_IMAGES);
        warning = Some("Artwork list capped at 128 images".into());
    }
    audio.sort();
    // The selected track has first opportunity, but a track without artwork
    // must not hide valid embedded art on another track in the same album.
    if let Some(selected) = selected_audio {
        if let Some(position) = audio.iter().position(|path| path == selected) {
            let path = audio.remove(position);
            audio.insert(0, path);
        } else if selected.parent() == Some(directory.as_path())
            && fs::symlink_metadata(selected).is_ok_and(|md| md.is_file())
            && crate::convert::classify::is_audio_file_path(selected)
        {
            // The listing may have reached its entry cap before this track.
            audio.insert(0, selected.clone());
        }
    }
    // A single candidate searches the sorted tracks lazily at decode time.
    // It remains one cycling entry even when many tracks contain artwork.
    let embedded = (!audio.is_empty()).then(|| ArtworkSource::Embedded(Arc::from(audio)));
    let mut sources: Vec<ArtworkSource> = files.into_iter().map(ArtworkSource::File).collect();
    if let Some(embedded) = embedded {
        if config.prefer_embedded { sources.insert(0, embedded); }
        else { sources.push(embedded); }
    }
    ScanResult { context, sources, warning, generation: 0 }
}

/// A bounded, deterministic shallow listing for an explicitly opened image.
/// Keep the selected file in the list even if a huge directory hits the cap.
fn scan_image_siblings(context: ArtworkContext, selected: &Path) -> ScanResult {
    let mut files = Vec::new();
    let mut warning = None;
    if let Some(directory) = selected.parent() {
        match fs::read_dir(directory) {
            Ok(listing) => {
                for (index, result) in listing.enumerate() {
                    if index >= MAX_DIRECTORY_ENTRIES {
                        warning = Some("Image directory listing capped".into());
                        break;
                    }
                    let Ok(entry) = result else { continue; };
                    let path = entry.path();
                    if !is_supported_image(&path) { continue; }
                    let Ok(kind) = entry.file_type() else { continue; };
                    if kind.is_file() || (kind.is_symlink()
                        && fs::metadata(&path).is_ok_and(|md| md.is_file())) {
                        files.push(path);
                    }
                }
            }
            Err(error) => warning = Some(format!("Cannot list image directory: {error}")),
        }
    }
    // Sort all candidates before truncating so directory iteration order never
    // changes the cycle sequence; the selected entry is always retained.
    fn order(a: &PathBuf, b: &PathBuf) -> std::cmp::Ordering {
        a.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase()
            .cmp(&b.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase())
            .then_with(|| a.cmp(b))
    }
    files.sort_by(order);
    // Unlike album artwork discovery, the explicit image viewer includes all
    // image files found by the bounded directory scan (up to 32,768 entries).
    // Truncating the viewer at the album-artwork limit would silently exclude
    // most images in larger photography or artwork folders.
    // A renamed/removed file can still be visible in a stale Browse list; the
    // decoder revalidates it. Preserve the explicitly requested file so the
    // popup never silently changes to a different image.
    if !files.iter().any(|path| path == selected) {
        if files.len() == MAX_DIRECTORY_ENTRIES { files.pop(); }
        files.push(selected.to_path_buf());
        files.sort_by(order);
    }
    ScanResult {
        context,
        sources: files.into_iter().map(ArtworkSource::File).collect(),
        warning,
        generation: 0,
    }
}

fn decode_source(source: &ArtworkSource) -> Result<DecodedArtwork, String> {
    match source {
        ArtworkSource::File(path) => {
            // Only direct image-name links are eligible. A link replaced with
            // a missing target or directory between scanning and decoding
            // must fail without traversing anything else.
            if !is_supported_image(path) { return Err("Unsupported artwork filename".into()); }
            let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            if !(metadata.is_file() || (metadata.file_type().is_symlink()
                && fs::metadata(path).is_ok_and(|md| md.is_file())))
            {
                return Err("Artwork source is not a regular file".into());
            }
            let file = fs::File::open(path).map_err(|e| e.to_string())?;
            let opened = file.metadata().map_err(|e| e.to_string())?;
            if !opened.is_file() { return Err("Artwork source is not a regular file".into()); }
            if opened.len() > MAX_ENCODED_IMAGE_BYTES {
                return Err("Artwork image exceeds 24 MiB limit".into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_ENCODED_IMAGE_BYTES + 1).read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok(DecodedArtwork { image: decode_image_bytes(bytes)?, embedded_path: None })
        }
        ArtworkSource::Embedded(paths) => {
            // The worker tries candidates in order and stops on the first
            // image that really decodes, not merely the first picture tag.
            // Missing, unreadable, and corrupt pictures all permit fallback.
            let mut last_error = None;
            for path in paths.iter() {
                // Do not read audio symlinks even if they were replaced after
                // discovery. Image-file symlinks are handled separately above.
                if !fs::symlink_metadata(path).is_ok_and(|md| md.is_file()) { continue; }
                let bytes = super::probe::read_embedded_picture_bytes(path, PictureType::CoverFront)
                    .or_else(|_| super::probe::read_embedded_picture_bytes(path, PictureType::Other));
                match bytes.and_then(decode_image_bytes) {
                    Ok(image) => return Ok(DecodedArtwork {
                        image, embedded_path: Some(path.clone()),
                    }),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| "No usable embedded artwork in album".into()))
        }
    }
}

fn decode_image_bytes(bytes: Vec<u8>) -> Result<DynamicImage, String> {
    if bytes.len() as u64 > MAX_ENCODED_IMAGE_BYTES { return Err("Artwork exceeds 24 MiB limit".into()); }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?;
    if image.width() > MAX_PREVIEW_EDGE || image.height() > MAX_PREVIEW_EDGE {
        Ok(image.resize(MAX_PREVIEW_EDGE, MAX_PREVIEW_EDGE, image::imageops::FilterType::Triangle))
    } else { Ok(image) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_round_and_allow_full_height() {
        assert_eq!(percent_height(0, 28), 0);
        assert_eq!(percent_height(30, 28), 8);
        assert_eq!(percent_height(30, 100), 30);
        assert_eq!(percent_height(4, 1), 1);
    }

    #[test]
    fn downscale_uses_terminal_cell_dimensions_and_centers() {
        let area = Rect::new(10, 5, 40, 20);
        assert_eq!(fitted_without_upscale(area, 24, 24, (8, 12)), Rect::new(28, 14, 3, 2));
        assert_eq!(fitted_without_upscale(area, 9999, 9999, (8, 12)), Rect::new(15, 5, 30, 20));
    }

    #[test]
    fn fitting_maintains_aspect_ratio_and_respects_enlargement_policy() {
        let area = Rect::new(10, 5, 40, 20);
        assert_eq!(fitted_without_upscale(area, 560, 60, (8, 12)),
            Rect::new(10, 13, 40, 3));
        assert_eq!(fitted_without_upscale(area, 24, 24, (8, 12)),
            Rect::new(28, 14, 3, 2));
        assert_eq!(fitted_image_area(area, 24, 24, (8, 12), true, ImageVerticalPlacement::Center),
            Rect::new(15, 5, 30, 20));
    }

    #[test]
    fn discovery_prefers_embedded_but_cycles_only_direct_folder_images() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("track.flac"), b"audio").unwrap();
        fs::write(root.join("z.jpg"), b"image").unwrap();
        fs::write(root.join("folder.jpg"), b"image").unwrap();
        fs::write(root.join("cover.png"), b"image").unwrap();
        fs::create_dir(root.join("artwork")).unwrap();
        fs::write(root.join("artwork").join("cover.jpg"), b"image").unwrap();
        let ctx = ArtworkContext::Album { directory: root.to_path_buf(), selected_audio: None };
        let result = scan_directory(ctx.clone(), &BrowseArtworkConfig::default());
        assert_eq!(result.sources.len(), 4);
        assert!(matches!(&result.sources[0], ArtworkSource::Embedded(_)));
        assert_eq!(result.sources[1].label(), "cover.png");
        assert_eq!(result.sources[2].label(), "folder.jpg");
        assert_eq!(result.sources[3].label(), "z.jpg");
        let mut policy = BrowseArtworkConfig::default();
        policy.prefer_embedded = false;
        let result = scan_directory(ctx, &policy);
        assert!(matches!(result.sources.last(), Some(ArtworkSource::Embedded(_))));
    }

    #[test]
    fn custom_preference_order_and_case_matching_are_respected() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("front.JPG"), b"image").unwrap();
        fs::write(tmp.path().join("cover.png"), b"image").unwrap();
        fs::write(tmp.path().join("folder.jpg"), b"image").unwrap();
        let mut config = BrowseArtworkConfig::default();
        config.filenames = vec!["FOLDER.JPG".into(), "front.jpg".into()];
        let result = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: None,
        }, &config);
        let labels: Vec<_> = result.sources.iter().map(ArtworkSource::label).collect();
        assert_eq!(labels, vec!["folder.jpg".to_string(), "front.JPG".to_string(),
            "cover.png".to_string()]);
    }

    #[test]
    fn stale_decoding_results_never_replace_a_new_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = BrowseArtworkState::default();
        state.sources = vec![ArtworkSource::File(tmp.path().join("first.png")),
            ArtworkSource::File(tmp.path().join("second.png"))];
        let old_generation = state.generation;
        assert!(state.cycle(true));
        let (tx, rx) = mpsc::channel();
        tx.send(DecodeResult {
            generation: old_generation,
            source: ArtworkSource::File(tmp.path().join("first.png")),
            image: Ok(DecodedArtwork { image: DynamicImage::new_rgba8(10, 10), embedded_path: None }),
        }).unwrap();
        state.decode_rx = Some(rx);
        assert!(!state.poll_workers());
        assert!(state.decoded.is_none());
        assert_eq!(state.index(), 1);
    }

    #[test]
    fn preferred_filename_is_retained_after_candidate_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for index in 0..140 { fs::write(tmp.path().join(format!("a{index:04}.jpg")), b"x").unwrap(); }
        fs::write(tmp.path().join("cover.png"), b"x").unwrap();
        let result = scan_directory(ArtworkContext::Album { directory: tmp.path().into(), selected_audio: None },
                                    &BrowseArtworkConfig::default());
        assert_eq!(result.sources.len(), MAX_FOLDER_IMAGES);
        assert_eq!(result.sources[0].label(), "cover.png");
        assert!(result.warning.is_some());
    }

    #[test]
    fn stale_generation_is_rejected_after_policy_change() {
        let mut state = BrowseArtworkState::default();
        let tmp = tempfile::tempdir().unwrap();
        let context = ArtworkContext::Album { directory: tmp.path().into(), selected_audio: None };
        state.observe(Some(context.clone()));
        let stale = state.generation;
        state.invalidate();
        let (tx, rx) = mpsc::channel();
        tx.send(ScanResult { context, sources: vec![ArtworkSource::File(tmp.path().join("x.jpg"))],
            warning: None, generation: stale }).unwrap();
        state.scan_rx = Some(rx);
        assert!(!state.poll_workers());
        assert!(state.sources.is_empty());
        assert!(state.scanned.is_none());
    }

    #[test]
    fn real_png_decodes_and_popup_height_is_centered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cover.png");
        image::DynamicImage::new_rgba8(12, 16).save(&path).unwrap();
        let image = decode_source(&ArtworkSource::File(path)).unwrap();
        assert_eq!((image.image.width(), image.image.height()), (12, 16));
        assert_eq!(popup_rect(Rect::new(0, 0, 100, 40), 60), Rect::new(10, 8, 80, 24));
        assert_eq!(popup_rect(Rect::new(0, 0, 100, 40), 100), Rect::new(10, 0, 80, 40));
    }

    #[test]
    fn graphics_background_restore_preserves_text_images_and_other_surfaces() {
        let area = Rect::new(0, 0, 9, 3);
        let background = Color::Rgb(26, 27, 38);
        let other_background = Color::Rgb(40, 44, 52);
        let mut before = Buffer::empty(area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                before.get_mut(x, y).set_bg(background);
            }
        }
        before.get_mut(1, 1).set_symbol("T");
        before.get_mut(2, 1).set_skip(true); // native image area
        before.get_mut(3, 1).set_bg(other_background); // control surface

        let even = background_repaint_ink(background, 0);
        let odd = background_repaint_ink(background, 1);
        assert_ne!(even, Color::Reset);
        assert_ne!(odd, Color::Reset);
        assert_ne!(even, odd);
        assert_eq!(even, background);
        assert_eq!(odd, Color::Rgb(27, 27, 38));
        assert_eq!(background_repaint_ink(Color::Rgb(255, 0, 0), 1),
            Color::Rgb(254, 0, 0)); // keep the +/-1 adjustment in range

        // A new preview may first render on an odd OR an even frame, before
        // any foreground has been painted. Both must generate terminal writes.
        for first_frame in [0_usize, 1] {
            let mut previous = before.clone();
            // Also exercise consecutive frames while native artwork is active.
            for frame_count in first_frame..first_frame + 3 {
                let mut after = previous.clone();
                repaint_blank_background_cells(&mut after, area, background,
                    background_repaint_ink(background, frame_count));

                for (x, y) in [(1, 1), (2, 1), (3, 1)] {
                    assert_eq!(after.get(x, y), before.get(x, y),
                        "text, native images, and non-theme surfaces must not change");
                }
                assert_eq!(after.get(4, 1).bg, background);
                assert_ne!(after.get(4, 1).fg, Color::Reset);
                assert!(previous.diff(&after).iter().any(|(x, y, _)| (*x, *y) == (4, 1)),
                    "frame {frame_count} must repaint an initially reset blank cell");
                assert!(previous.diff(&after).iter().all(|(x, y, _)| {
                    let cell = after.get(*x, *y);
                    !cell.skip && cell.symbol() == " " && cell.bg == background
                }), "only blank theme-background cells may be repainted");
                previous = after;
            }
        }
    }

    #[test]
    fn native_background_repaint_is_on_demand_not_per_unrelated_frame() {
        use ratatui_image::picker::ProtocolType;

        let mut artwork = BrowseArtworkState::default();
        artwork.decoded = Some(DynamicImage::new_rgb8(80, 60));
        artwork.set_desired_area(Rect::new(0, 0, 30, 15), ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        let config = BrowseArtworkConfig::default();
        let background = Color::Rgb(26, 27, 38);

        assert!(artwork.prepare(&mut picker, 0, 0, &config));
        assert!(artwork.take_background_restore(), "first native placement must repaint");
        let first = artwork.background_restore_count();
        let first_ink = background_repaint_ink(background, first);
        assert!(!artwork.take_background_restore(), "restoration is consumed once");
        assert!(!artwork.prepare(&mut picker, 0, 0, &config),
            "unchanged artwork must not rebuild on unrelated Browse redraws");
        assert!(!artwork.take_background_restore(),
            "unrelated redraws must not repaint thousands of blank cells");

        assert!(artwork.prepare(&mut picker, 0, 1, &config),
            "an explicit Kitty retransmit must rebuild its protocol");
        assert!(artwork.take_background_restore());
        let second = artwork.background_restore_count();
        assert_ne!(background_repaint_ink(background, second), first_ink,
            "restorations must alternate even if rendered frame counts have the same parity");
        assert!(!artwork.take_background_restore());
        artwork.popup_open = true;
        artwork.close_popup();
        assert!(artwork.take_background_restore(), "popup teardown still repaints");
        assert_eq!(artwork.background_restore_count(), second + 1);
    }

    #[test]
    fn native_artwork_menu_restoration_forces_one_image_origin_diff() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        state.decoded = Some(DynamicImage::new_rgb8(24, 24));
        state.popup_open = true;
        let viewport = Rect::new(3, 2, 45, 24);
        let background = Color::Rgb(26, 27, 38);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        assert!(state.prepare(&mut picker, 0, 0, &config));
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let initial = terminal.backend().buffer().clone();
        let origin = (state.protocol_area.x, state.protocol_area.y);

        // No menu painted over the image: ordinary drawing and unrelated
        // context menus must not initiate image restoration.
        state.artwork_menu_finished(true);
        assert!(!state.repaint_image_after_menu);
        state.artwork_menu_opened();
        state.artwork_menu_finished(false); // e.g. viewer is closing
        assert!(!state.repaint_image_after_menu);
        assert_eq!(state.native_image_restore_count, 0);

        state.artwork_menu_opened();
        state.artwork_menu_finished(true);
        assert!(state.repaint_image_after_menu);
        assert!(state.protocol.is_none(), "reprepare the native protocol after occlusion");
        // A replacement menu can open before the next frame rebuilds the
        // protocol. Dismissing it must not discard the first restoration.
        state.artwork_menu_opened();
        state.artwork_menu_finished(true);
        assert!(state.repaint_image_after_menu,
            "replacement before redraw must preserve the pending restoration");
        assert!(state.protocol.is_none());
        assert!(state.prepare(&mut picker, 0, 0, &config));
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let restored = terminal.backend().buffer().clone();
        assert_ne!(initial.get(origin.0, origin.1).fg, restored.get(origin.0, origin.1).fg);
        assert!(initial.diff(&restored).iter().any(|(x, y, _)| (*x, *y) == origin),
            "Ratatui must emit the native image command at its origin");
        assert_eq!(state.native_image_restore_count, 1);
        assert!(!state.repaint_image_after_menu);

        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let ordinary_redraw = terminal.backend().buffer().clone();
        assert_eq!(ordinary_redraw.get(origin.0, origin.1).fg,
            restored.get(origin.0, origin.1).fg,
            "unrelated redraw must not toggle the image-origin ink again");

        // Replace the menu, allow an actual image frame to render under the
        // replacement, then dismiss it. The final dismissal must request a
        // second restoration even though the first was already consumed.
        state.artwork_menu_opened();
        state.artwork_menu_finished(true);
        state.artwork_menu_opened();
        assert!(state.prepare(&mut picker, 0, 0, &config));
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
            frame.render_widget(ratatui::widgets::Clear,
                Rect::new(origin.0 + 1, origin.1, 1, 1));
        }).unwrap();
        let covered_frame = terminal.backend().buffer().clone();
        assert!(!state.repaint_image_after_menu);
        state.artwork_menu_finished(true);
        assert!(state.repaint_image_after_menu,
            "dismissing the replacement menu must request another restoration");
        assert!(state.protocol.is_none());
        assert!(state.prepare(&mut picker, 0, 0, &config));
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let second_restore = terminal.backend().buffer().clone();
        assert_ne!(second_restore.get(origin.0, origin.1).fg,
            covered_frame.get(origin.0, origin.1).fg,
            "successive menu dismissals must each restore native image pixels");
        assert!(covered_frame.diff(&second_restore).iter()
            .any(|(x, y, _)| (*x, *y) == origin));
        assert_eq!(state.native_image_restore_count, 3,
            "each rendered menu restoration forces the origin exactly once");
        assert!(!state.repaint_image_after_menu);

        // Halfblocks pixels use the real cell foreground for their upper
        // pixel. Native-only diff forcing must not recolor that pixel if the
        // terminal protocol changes (for example on entering tmux).
        picker.protocol_type = ProtocolType::Halfblocks;
        state.invalidate_protocol();
        assert!(state.prepare(&mut picker, 1, 0, &config));
        assert_eq!(state.native_image_restore_count, 0);
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let halfblocks = terminal.backend().buffer();
        assert_eq!(halfblocks.get(origin.0, origin.1).fg, Color::Rgb(0, 0, 0),
            "native restoration ink must not overwrite Halfblocks pixel colors");
    }

    #[test]
    fn artwork_menu_opened_before_decode_restores_first_occluded_native_frame() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut still_loading = BrowseArtworkState::default();
        still_loading.artwork_menu_opened();
        still_loading.artwork_menu_finished(true);
        assert!(!still_loading.repaint_image_after_menu,
            "a menu dismissed before any image exists must not retransmit");
        assert!(!still_loading.take_background_restore());

        let mut state = BrowseArtworkState::default();
        state.popup_open = true;
        state.artwork_menu_opened();
        assert!(state.artwork_menu_occludes_image,
            "the menu must remain tracked while decoding has not completed");
        assert!(state.decoded.is_none());
        assert!(state.protocol.is_none());

        // Model the asynchronous decode completing before the menu closes.
        state.decoded = Some(DynamicImage::new_rgb8(160, 96));
        let viewport = Rect::new(3, 2, 45, 24);
        let background = Color::Rgb(26, 27, 38);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        assert!(state.prepare(&mut picker, 0, 0, &config));
        let origin = (state.protocol_area.x, state.protocol_area.y);
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
            frame.render_widget(ratatui::widgets::Clear,
                Rect::new(origin.0 + 1, origin.1, 2, 1));
        }).unwrap();
        let occluded = terminal.backend().buffer().clone();
        assert!(state.take_background_restore(), "the initial placement was prepared");
        assert!(!state.take_background_restore());

        state.artwork_menu_finished(true);
        assert!(state.repaint_image_after_menu);
        assert!(state.protocol.is_none());
        assert!(state.take_background_restore(),
            "dismissal of a menu opened before decode must schedule restoration");
        assert!(!state.take_background_restore());
        assert!(state.prepare(&mut picker, 0, 0, &config));
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let restored = terminal.backend().buffer().clone();
        assert!(occluded.diff(&restored).iter()
            .any(|(x, y, _)| (*x, *y) == origin),
            "restoration must retransmit the native command at the image origin");
        assert_eq!(state.native_image_restore_count, 1);
        assert!(!state.repaint_image_after_menu);

        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        assert_eq!(state.native_image_restore_count, 1,
            "ordinary redraws must not trigger a second menu restoration");
    }

    #[test]
    fn closing_artwork_view_never_retransmits_its_hidden_image() {
        let mut state = BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/pictures/cover.png"));
        state.decoded = Some(DynamicImage::new_rgb8(12, 12));
        state.artwork_menu_opened();
        state.close_popup();
        state.artwork_menu_finished(true);
        assert!(!state.repaint_image_after_menu);
        assert!(state.context.is_none());
        assert!(state.decoded.is_none());
    }

    #[test]
    fn image_close_requests_one_background_restoration() {
        let mut artwork = BrowseArtworkState::default();
        artwork.popup_open = true;
        artwork.close_popup();
        assert!(artwork.take_background_restore());
        assert!(!artwork.take_background_restore());
    }

    #[test]
    fn popup_background_and_mouse_hitbox_fit_image_without_a_caption() {
        use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};

        let mut app = super::super::app::AppState::new_for_test(
            crate::config::TonepoetConfig::default());
        app.browse_artwork.popup_open = true;
        app.browse_artwork.decoded = Some(DynamicImage::new_rgb8(160, 96));
        app.browse_artwork.font_size = (8, 12);
        app.browse_artwork.sources = vec![ArtworkSource::File(PathBuf::from("cover.png"))];
        let theme = app.theme;
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| {
            frame.render_widget(Paragraph::new("background marker"), Rect::new(0, 0, 20, 1));
            draw_popup(frame, &mut app, theme);
        }).unwrap();

        let bounds = popup_rect(Rect::new(0, 0, 100, 40), 60);
        let fitted = fitted_image_area(bounds, 160, 96, (8, 12), false,
            ImageVerticalPlacement::Center);
        assert_eq!(fitted, Rect::new(40, 16, 20, 8));
        assert_eq!(app.browse_artwork.popup_rect, fitted);
        let buffer = terminal.backend().buffer();
        let painted = (0..40_u16).map(|y| (0..100_u16)
            .map(|x| buffer.get(x, y).symbol().to_owned())
            .collect::<Vec<_>>().join("")).collect::<Vec<_>>().join("\n");
        assert!(painted.contains("background marker"),
            "outside-image screen content must not be covered by a popup panel");
        assert!(!painted.contains("cover.png"), "filename caption must be absent");
    }

    #[test]
    fn loading_viewer_uses_only_a_centered_status_line() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = super::super::app::AppState::new_for_test(
            crate::config::TonepoetConfig::default());
        app.browse_artwork.open_file_popup(PathBuf::from("cover.png"));
        let theme = app.theme;
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| draw_popup(frame, &mut app, theme)).unwrap();
        let status = app.browse_artwork.popup_rect;
        assert_eq!(status.height, 1);
        assert!(status.width < 80, "loading must not paint the maximum viewport");
        assert_eq!(status.y, 19);
    }

    #[test]
    fn embedded_art_is_found_after_first_track_without_a_picture() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("01 Intro.flac");
        let second = tmp.path().join("02 Song.flac");
        fs::write(&first, include_bytes!("../../tests/fixtures/silence.flac")).unwrap();
        fs::write(&second, include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac")).unwrap();
        image::DynamicImage::new_rgb8(8, 6).save(tmp.path().join("cover.jpg")).unwrap();
        let sources = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(sources.len(), 2); // One logical embedded image and one folder cover.
        assert!(matches!(&sources[0], ArtworkSource::Embedded(_)));
        let decoded = decode_source(&sources[0]).expect("later track has valid embedded art");
        assert_eq!(decoded.embedded_path.as_deref(), Some(second.as_path()));
        assert_eq!((decoded.image.width(), decoded.image.height()), (12, 16));
        assert_eq!(sources[1].label(), "cover.jpg");

        // A selected track that lacks artwork still searches the other tracks.
        let selected = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: Some(first),
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(decode_source(&selected[0]).unwrap().embedded_path.as_deref(), Some(second.as_path()));
    }

    #[test]
    fn corrupt_embedded_image_advances_to_next_track() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01 Corrupt.flac"),
            include_bytes!("../../tests/fixtures/browse_artwork/embedded_corrupt.flac")).unwrap();
        let good = tmp.path().join("02 Usable.flac");
        fs::write(&good,
            include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac")).unwrap();
        let sources = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(sources.len(), 1);
        let decoded = decode_source(&sources[0]).expect("skip corrupt picture");
        assert_eq!(decoded.embedded_path.as_deref(), Some(good.as_path()));
    }

    #[test]
    fn selected_track_with_art_is_tried_before_sorted_album_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("01 Track.flac");
        let second = tmp.path().join("02 Track.flac");
        let bytes = include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac");
        fs::write(&first, bytes).unwrap();
        fs::write(&second, bytes).unwrap();
        let sources = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: Some(second.clone()),
        }, &BrowseArtworkConfig::default()).sources;
        let decoded = decode_source(&sources[0]).unwrap();
        assert_eq!(decoded.embedded_path.as_deref(), Some(second.as_path()));
    }

    #[test]
    fn album_without_embedded_pictures_falls_back_to_folder_cover() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("01 Intro.flac"),
            include_bytes!("../../tests/fixtures/silence.flac")).unwrap();
        fs::write(tmp.path().join("02 Song.flac"),
            include_bytes!("../../tests/fixtures/silence.flac")).unwrap();
        image::DynamicImage::new_rgb8(7, 9).save(tmp.path().join("cover.jpg")).unwrap();
        let sources = scan_directory(ArtworkContext::Album {
            directory: tmp.path().into(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(sources.len(), 2);
        assert!(decode_source(&sources[0]).is_err());
        assert_eq!(sources[1].label(), "cover.jpg");
        let decoded = decode_source(&sources[1]).expect("fallback cover must be usable");
        assert_eq!((decoded.image.width(), decoded.image.height()), (7, 9));
    }

    #[test]
    fn info_image_bottom_placement_stays_fixed_as_height_allocation_grows() {
        // The 1080-pixel square reaches the Info pane's width ceiling.
        // The extra height at 75% must go ABOVE the image, not below it.
        let bottom = 70;
        let mut prior_height = 0;
        for height in [22, 38, 57] {
            let area = Rect::new(5, bottom - height, 80, height);
            let target = fitted_image_area(area, 1080, 1080, (12, 24), true,
                ImageVerticalPlacement::Bottom);
            assert_eq!(target.bottom(), bottom);
            assert!(target.height >= prior_height);
            assert!(target.width <= area.width);
            prior_height = target.height;
        }
        let popup = fitted_image_area(Rect::new(5, 5, 80, 57), 1080, 1080, (12, 24),
            true, ImageVerticalPlacement::Center);
        assert_eq!(popup.y, 5 + (57 - popup.height) / 2);
    }

    #[test]
    fn file_view_decodes_only_selected_image_and_restores_album_state_on_close() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("image.png");
        image::DynamicImage::new_rgba8(24, 18).save(&image_path).unwrap();
        fs::write(temp.path().join("unrelated.jpg"), b"not decoded").unwrap();

        let mut state = BrowseArtworkState::default();
        state.observe(Some(ArtworkContext::Album {
            directory: temp.path().to_path_buf(), selected_audio: None,
        }));
        let album_generation = state.generation;
        state.open_file_popup(image_path.clone());
        assert!(state.is_single_file_popup());
        assert!(state.generation > album_generation);
        assert!(state.scanned.is_none(), "sibling listing is deferred to a worker");
        assert!(state.scan_rx.is_none(), "opening View must not scan synchronously");
        assert_eq!(state.sources, vec![ArtworkSource::File(image_path.clone())]);
        assert!(!state.cycle(true), "a single image has nothing to cycle to");

        let mut picker = Picker::new((12, 24));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state.has_image() && Instant::now() < deadline {
            state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default());
            thread::sleep(Duration::from_millis(5));
        }
        assert!(state.has_image(), "direct image should decode in a bounded worker");
        assert_eq!(state.decoded.as_ref().map(|image| (image.width(), image.height())),
            Some((24, 18)));

        let file_generation = state.generation;
        state.close_popup();
        assert!(!state.popup_open);
        assert!(!state.is_single_file_popup());
        assert!(state.context.is_none());
        assert!(state.sources.is_empty());
        assert!(state.generation > file_generation);
    }

    #[test]
    fn stale_file_view_result_is_discarded_after_popup_closes() {
        let mut state = BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/pictures/front.png"));
        let stale = state.generation;
        let (tx, rx) = mpsc::channel();
        state.decode_rx = Some(rx);
        tx.send(DecodeResult {
            generation: stale,
            source: ArtworkSource::File(PathBuf::from("/pictures/front.png")),
            image: Err("stale decode error".to_string()),
        }).unwrap();
        state.close_popup();
        state.poll_workers();
        assert_eq!(state.message(), "");
        assert!(state.warning.is_none());
        assert!(state.context.is_none());
    }

    #[test]
    fn missing_embedded_art_is_silent_in_browse_artwork_state() {
        let mut state = BrowseArtworkState::default();
        state.observe(Some(ArtworkContext::Album {
            directory: PathBuf::from("/music/album"), selected_audio: None,
        }));
        state.scanned = state.context.clone();
        state.warning = Some("No embedded Other artwork found in '/music/album/track.flac'".into());
        assert_eq!(state.message(), "", "absence is not an Info-pane error");
    }

    #[test]
    fn inline_double_click_opens_the_original_image_not_the_cycled_successor() {
        let mut state = BrowseArtworkState::default();
        state.sources = vec![ArtworkSource::File(PathBuf::from("cover.png")),
            ArtworkSource::File(PathBuf::from("back.jpg"))];
        assert!(!state.open_inline_popup_on_double_click(), "needs a first click");
        assert!(state.inline_click(), "normal single-click cycles");
        assert_eq!(state.index(), 1);
        assert!(state.open_inline_popup_on_double_click());
        assert!(state.popup_open);
        assert_eq!(state.index(), 0, "double-click keeps the image originally clicked");
        state.close_popup();
        assert!(!state.popup_open);
    }

    #[test]
    fn direct_image_early_navigation_accumulates_every_step() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            DynamicImage::new_rgb8(12, 10).save(temp.path().join(name)).unwrap();
        }
        let path = temp.path().join("b.png");
        let context = ArtworkContext::SingleFile(path.clone());
        for (directions, expected) in [
            (vec![true], "c.png"),
            (vec![false], "a.png"),
            (vec![true, false], "b.png"),
            (vec![true, true], "a.png"),
            (vec![false, true, true], "c.png"),
        ] {
            let mut state = BrowseArtworkState::default();
            state.open_file_popup(path.clone());
            assert_eq!(state.count(), 1);
            for direction in directions.iter().copied() {
                assert!(state.cycle(direction), "early inputs are accepted");
            }
            assert_eq!(state.current_label().as_deref(), Some("b.png"),
                "do not navigate an incomplete sibling list");
            let mut result = scan_image_siblings(context.clone(), &path);
            result.generation = state.generation;
            let (tx, rx) = mpsc::channel();
            state.scan_rx = Some(rx);
            tx.send(result).unwrap();
            assert!(state.poll_workers());
            assert_eq!(state.count(), 3);
            assert_eq!(state.current_label().as_deref(), Some(expected),
                "deferred navigation {directions:?} must preserve all steps");
            assert_eq!(state.queued_gallery_offset, 0);
            state.close_popup();
            assert!(!state.popup_open);
        }
    }

    #[test]
    fn closing_direct_view_discards_pending_gallery_navigation_and_scan() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            DynamicImage::new_rgb8(12, 10).save(temp.path().join(name)).unwrap();
        }
        let path = temp.path().join("b.png");
        let context = ArtworkContext::SingleFile(path.clone());
        let mut state = BrowseArtworkState::default();
        state.open_file_popup(path.clone());
        assert!(state.cycle(true));
        assert!(state.cycle(true));
        assert_eq!(state.queued_gallery_offset, 2);
        let mut result = scan_image_siblings(context, &path);
        result.generation = state.generation;
        let (tx, rx) = mpsc::channel();
        state.scan_rx = Some(rx);
        tx.send(result).unwrap();
        state.close_popup();
        assert_eq!(state.queued_gallery_offset, 0);
        assert!(state.context.is_none());
        assert!(!state.poll_workers(), "a post-close scan is stale");
        assert!(state.sources.is_empty(), "stale results must not reopen a gallery");
    }

    #[test]
    fn direct_image_view_cycles_shallow_siblings_in_deterministic_order() {
        let temp = tempfile::tempdir().unwrap();
        let cover = temp.path().join("cover.png");
        for file in ["z.png", "Back.JPG", "cover.png"] {
            image::DynamicImage::new_rgb8(12, 10).save(temp.path().join(file)).unwrap();
        }
        fs::write(temp.path().join("track.flac"), b"audio").unwrap();
        fs::create_dir(temp.path().join("artwork")).unwrap();
        image::DynamicImage::new_rgb8(12, 10)
            .save(temp.path().join("artwork").join("hidden.png")).unwrap();
        let mut state = BrowseArtworkState::default();
        state.open_file_popup(cover.clone());
        assert_eq!(state.count(), 1, "the explicit file is available before a scan");
        let mut picker = Picker::new((12, 24));
        let deadline = Instant::now() + Duration::from_secs(5);
        while (state.count() != 3 || !state.has_image()) && Instant::now() < deadline {
            state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default());
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.count(), 3, "only direct supported images are cycled");
        assert_eq!(state.index(), 1, "discovery preserves the explicitly opened cover");
        assert_eq!(state.current_label().as_deref(), Some("cover.png"));
        assert!(state.has_image(), "the worker must retain already decoded pixels");
        assert!(state.cycle(true));
        assert_eq!(state.current_label().as_deref(), Some("z.png"));
        assert!(state.cycle(true));
        assert_eq!(state.current_label().as_deref(), Some("Back.JPG"));
        assert!(state.cycle(false));
        assert_eq!(state.current_label().as_deref(), Some("z.png"));
    }

    #[test]
    fn direct_gallery_includes_siblings_beyond_the_album_artwork_limit() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..140 {
            fs::write(temp.path().join(format!("{n:03}.png")), b"image").unwrap();
        }
        let selected = temp.path().join("139.png");
        let result = scan_image_siblings(ArtworkContext::SingleFile(selected.clone()), &selected);
        assert_eq!(result.sources.len(), 140, "a gallery is not an album-artwork list");
        assert!(result.sources.contains(&ArtworkSource::File(selected)));
        assert!(result.warning.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn direct_image_symlinks_decode_but_broken_and_directory_links_do_not() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let album = tmp.path().join("album");
        fs::create_dir(&album).unwrap();
        let shared = tmp.path().join("shared.jpg");
        image::DynamicImage::new_rgb8(18, 10).save(&shared).unwrap();
        symlink(&shared, album.join("cover.jpg")).unwrap();
        symlink(tmp.path().join("missing.png"), album.join("folder.png")).unwrap();
        let nested = album.join("artwork");
        fs::create_dir(&nested).unwrap();
        image::DynamicImage::new_rgb8(9, 9).save(nested.join("unreferenced.png")).unwrap();
        symlink(&nested, album.join("original_cover.jpg")).unwrap();
        image::DynamicImage::new_rgb8(6, 6).save(album.join("back.png")).unwrap();
        let sources = scan_directory(ArtworkContext::Album {
            directory: album.clone(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        let labels: Vec<_> = sources.iter().map(ArtworkSource::label).collect();
        assert_eq!(labels, vec!["cover.jpg", "back.png"]);
        assert_eq!(decode_source(&sources[0]).unwrap().image.width(), 18);
        let mut state = BrowseArtworkState::default();
        state.sources = sources;
        assert!(state.cycle(true));
        assert_eq!(state.current_label().as_deref(), Some("back.png"));
        assert!(state.cycle(true));
        assert_eq!(state.current_label().as_deref(), Some("cover.jpg"));

        // Refuse a symlink that changed into a directory after discovery.
        fs::remove_file(album.join("cover.jpg")).unwrap();
        symlink(&nested, album.join("cover.jpg")).unwrap();
        assert!(decode_source(&state.sources[0]).is_err());
    }

}

/// Maximum image viewport, always bounded by the terminal (including narrow
/// terminals and the maximum 100% height option). The *painted popup* uses
/// the image's fitted rectangle inside this viewport.
pub fn popup_rect(terminal: Rect, percent: u8) -> Rect {
    let height = percent_height(terminal.height, percent).max(5).min(terminal.height);
    let width = ((u32::from(terminal.width) * 80 + 50) / 100)
        .max(20).min(u32::from(terminal.width)) as u16;
    Rect::new(terminal.x.saturating_add((terminal.width - width) / 2),
              terminal.y.saturating_add((terminal.height - height) / 2), width, height)
}

/// A centered modal image view, rendered after the ordinary screen. The
/// configurable height controls the maximum image size, not a blank panel.
/// The same image protocol cache is used for inline and popup views.
pub fn draw_popup(f: &mut ratatui::Frame, app: &mut super::app::AppState,
                  theme: super::theme::Theme) {
    use ratatui::widgets::{Block, Clear, Paragraph};
    use ratatui::style::Style;

    let terminal = f.size();
    if terminal.width < 8 || terminal.height < 5 {
        app.browse_artwork.popup_rect = Rect::default();
        return;
    }
    let viewport = popup_rect(terminal, app.browse.artwork_config.popup_height_percent);
    let message_width = {
        let message = app.browse_artwork.message();
        let message = if message.is_empty() { "Loading image…" } else { message };
        super::display_width::width(message)
    };
    let rect = app.browse_artwork.popup_image_area(viewport,
        app.browse.artwork_config.allow_upscale).unwrap_or_else(|| {
            // While loading or on failure, show just a centered status line;
            // don't reserve a large empty canvas for a missing image.
            let width = message_width.min(viewport.width as usize).max(1) as u16;
            Rect::new(viewport.x + (viewport.width - width) / 2,
                viewport.y + (viewport.height - 1) / 2, width, 1)
        });
    app.browse_artwork.popup_rect = rect;
    f.render_widget(Clear, rect);
    // No border, label, caption row, or panel outside the image. The backing
    // color matches the application rather than the old oversized panel.
    f.render_widget(Block::default()
        .style(Style::default().bg(theme.bg)), rect);
    let displayed = app.browse_artwork.render_image(f, viewport,
        app.browse.artwork_config.allow_upscale, ImageVerticalPlacement::Center,
        theme.bg);
    if !displayed {
        let message = app.browse_artwork.message();
        let message = if message.is_empty() { "Loading image…" } else { message };
        f.render_widget(Paragraph::new(message).style(theme.muted()), rect);
    }
}

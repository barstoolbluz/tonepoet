//! Browse Info artwork: shallow album discovery, bounded asynchronous decoding,
//! and terminal-protocol state owned outside the immediate-mode draw pass.
//!
//! One discovery and one decoding job can be active at any time. Rapid cursor
//! motion is debounced; superseded results are discarded by context/source
//! identity, so large or remote albums cannot spawn unbounded worker threads.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Cursor, Seek};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageReader, Limits};
use lofty::picture::PictureType;
use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::StatefulWidget};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::config::BrowseArtworkConfig;
use super::browse::{BrowseState, EntryKind, FolderClassificationKind};
use super::info_artwork_cache::{InfoPreviewCache, Preview, SourceStamp, PREVIEW_EDGE, stamps_for_decoded};

const SELECTION_SETTLE: Duration = Duration::from_millis(130);
const MAX_DIRECTORY_ENTRIES: usize = 32_768;
const MAX_FOLDER_IMAGES: usize = 128;
// Input byte admission and decoded memory admission are separate. A compressed
// 100 MiB archive scan may expand to hundreds of MiB before resampling.
const MAX_ENCODED_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 32_768;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    image: Arc<DynamicImage>,
    embedded_path: Option<PathBuf>,
    // Set only for Info-pane requests. Never populated by the full-size viewer.
    preview_stamps: Option<Vec<SourceStamp>>,
    preview_cache_hit: bool,
}

struct DecodeResult {
    generation: u64,
    source: ArtworkSource,
    image: Result<DecodedArtwork, String>,
    cache_epoch: Option<u64>,
}

// Sixel/iTerm/Kitty resize+encode can take seconds for large artwork. A
// settings interaction must never do that work inside the terminal draw.
// The worker owns its protocol; only pre-rendered terminal cells cross back
// to the UI thread. Revision + geometry reject superseded encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EncodedFrameKey {
    revision: u64,
    area: Rect,
    protocol_generation: usize,
    retransmit_generation: usize,
}

struct EncodedFrame {
    key: EncodedFrameKey,
    cells: Buffer,
    // Kitty uploads image bytes once. Render this second buffer from the SAME
    // protocol after its first upload, so later frames contain only placement
    // placeholders. Sixel/iTerm2 continue to reuse the initial payload.
    placement_cells: Option<Buffer>,
    upload_drawn: bool,
}

// Only fixed-origin native protocols have replayable payloads. Identity excludes
// the transient viewport revision, but includes every input to the encoding.
#[derive(Clone)]
struct ViewerFrameIdentity {
    generation: u64,
    pixels: Weak<DynamicImage>,
    ground: Option<[u8; 3]>,
    font: (u16, u16),
    protocol: ratatui_image::picker::ProtocolType,
    area: Rect,
    picker_generation: usize,
}
impl PartialEq for ViewerFrameIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.pixels.ptr_eq(&other.pixels)
            && self.ground == other.ground && self.font == other.font
            && self.protocol == other.protocol && self.area == other.area
            && self.picker_generation == other.picker_generation
    }
}
impl Eq for ViewerFrameIdentity {}

// Prepare native transport cells off the UI thread. Foreground and alternate
// jobs share the same codec and placement path; Kitty is never replay-cached.
fn encode_native_frame(pixels: &DynamicImage, mut picker: Picker, key: EncodedFrameKey,
                       ground: [u8; 3], compress_kitty: bool) -> EncodedFrame {
    let started=Instant::now();
    let kitty = picker.protocol_type ==
        ratatui_image::picker::ProtocolType::Kitty;
    let mut protocol = picker.new_resize_protocol(pixels.clone());
    let area = Rect::new(0, 0, key.area.width, key.area.height);
    // The image paints over a theme-colored panel. Both
    // transmission and placement-only frames need that ground.
    let blank = || {
        let mut cells = Buffer::empty(area);
        for y in 0..area.height {
            for x in 0..area.width {
                cells.get_mut(x, y).set_bg(Color::Rgb(ground[0], ground[1], ground[2]));
            }
        }
        cells
    };
    let mut cells = blank();
    ratatui_image::StatefulImage::new(Some(image::Rgb(ground)))
        .render(area, &mut cells, &mut protocol);
    // StatefulProtocol sends Kitty's bytes on its first render
    // and only placeholders thereafter. Keep both renderings
    // rather than replaying the upload on every UI redraw.
    let placement_cells = if kitty {
        let mut placement = blank();
        ratatui_image::StatefulImage::new(Some(image::Rgb(ground)))
            .render(area, &mut placement, &mut protocol);
        Some(placement)
    } else { None };
    if compress_kitty {
        let origin = cells.get_mut(0, 0);
        if let Some(compressed) =
            super::kitty_transport::compress_kitty_upload(origin.symbol()) {
            origin.set_symbol(&compressed);
        }
    }
    if log::log_enabled!(log::Level::Debug) {
        let bytes:usize=(0..area.height).map(|y|(0..area.width).map(|x|cells.get(x,y).symbol().len()).sum::<usize>()).sum();
        log::debug!("artwork native encode: {}x{} cells, {} ms, {} buffer symbol bytes (not PTY bytes)",area.width,area.height,started.elapsed().as_millis(),bytes);
    }
    EncodedFrame { key, cells, placement_cells, upload_drawn: false }
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
    // Key failures by source, never by a cursor index that changes when an
    // asynchronous gallery scan finishes. A failed explicit file stays chosen.
    failed: HashMap<ArtworkSource, String>,
    viewer_decode_bounds: Option<(u32, u32)>,
    viewer_max_area: Rect,
    // Session-only fixed-origin native frames, keyed to decoded content and terminal geometry.
    alternate_frames: Vec<(ViewerFrameIdentity, EncodedFrame)>,
    warm_rx: Option<Receiver<EncodedFrame>>,
    warm_request: Option<ViewerFrameIdentity>,
    warm_failed: Option<ViewerFrameIdentity>,
    encode_viewer_request: Option<ViewerFrameIdentity>,
    /// RGB ground used for viewer alpha composition, not Info's cached pixels.
    viewer_background: Option<[u8; 3]>,
    decode_rx: Option<Receiver<DecodeResult>>,
    decoded_source: Option<ArtworkSource>,
    decoded_embedded_path: Option<PathBuf>,
    decoded: Option<Arc<DynamicImage>>,
    // Created on first Info request, not when disabled or in direct viewer use.
    preview_cache: Option<Arc<InfoPreviewCache>>,
    // AppState's test constructors request a memory-only implicit cache even
    // in integration tests, where the library is built without cfg(test).
    memory_only_preview_cache: bool,
    protocol: Option<Box<dyn StatefulProtocol>>,
    desired_area: Rect,
    vertical_placement: ImageVerticalPlacement,
    protocol_area: Rect,
    protocol_generation: usize,
    retransmit_generation: usize,
    font_size: (u16, u16),
    prepared_protocol_has_fixed_origin: bool,
    prepared_protocol_type: ratatui_image::picker::ProtocolType,
    kitty_upload_transport: bool,
    // An inline single-click cycles; a subsequent double-click must open the
    // image that was under the first click, not the next image.
    inline_click_source: Option<ArtworkSource>,
    warning: Option<String>,
    pub popup_open: bool,
    /// Full-screen uses the same async viewer and protocol cache, without a
    /// Browse frame behind it. This is transient; the opening policy is saved.
    pub fullscreen: bool,
    pub popup_rect: Rect,
    // Native graphics may have altered terminal pixels outside the ratatui diff.
    // Restore only when terminal graphics are newly placed, invalidated, or
    // removed, not on every unrelated (often startup-driven) UI redraw.
    needs_background_restore: bool,
    // Alternate the invisible ink on each ACTUAL restoration. Frame parity
    // cannot guarantee a change when intervening frames don't repaint.
    background_restore_count: usize,
    // A native image command can repaint terminal pixels AFTER the current
    // frame's blank-cell repair. Request one follow-up repair only after the
    // newly prepared protocol has actually been rendered. This closes the
    // first-placement race without restoring on every unrelated Browse frame.
    native_placement_awaiting_render: bool,
    post_native_repaint_redraw: bool,
    // A context menu painted over native artwork may overwrite terminal pixels
    // without modifying Ratatui's image-origin cell. Restore once when that
    // menu closes or moves; never on hover or unrelated startup redraws.
    artwork_menu_occludes_image: bool,
    repaint_image_after_menu: bool,
    native_image_restore_count: usize,
    // Enable asynchronous native encoding after artwork settings is opened.
    // The ordinary viewer follows its existing render path until then.
    asynchronous_after_settings: bool,
    settings_visible: bool,
    // The first draw after settings closes owns the authoritative viewport.
    // Do not schedule a native encode using the previous draw's dimensions.
    awaiting_viewport_after_settings: bool,
    encoding_revision: u64,
    encode_rx: Option<Receiver<EncodedFrame>>,
    encode_request: Option<EncodedFrameKey>,
    encode_failed: Option<EncodedFrameKey>,
    encoded_frame: Option<EncodedFrame>,
}

impl Default for BrowseArtworkState {
    fn default() -> Self {
        Self {
            context: None, generation: 0, scan_generation: 0, decode_generation: 0, changed_at: Instant::now(), scanned: None,
            scan_rx: None, sources: Vec::new(), cursor: 0,
            queued_gallery_offset: 0, failed: HashMap::new(), viewer_decode_bounds: None,
            viewer_max_area: Rect::default(), alternate_frames: Vec::new(),
            warm_rx: None, warm_request: None, warm_failed: None, encode_viewer_request: None,
            viewer_background: None, decode_rx: None, decoded_source: None, decoded_embedded_path: None, decoded: None,
            preview_cache: None, memory_only_preview_cache: false,
            protocol: None, desired_area: Rect::default(),
            vertical_placement: ImageVerticalPlacement::Center, protocol_area: Rect::default(),
            protocol_generation: 0, retransmit_generation: 0, font_size: (8, 12),
            prepared_protocol_has_fixed_origin: false,
            prepared_protocol_type: ratatui_image::picker::ProtocolType::Halfblocks,
            kitty_upload_transport: false,
            inline_click_source: None, warning: None, popup_open: false,
            fullscreen: false, popup_rect: Rect::default(),
            needs_background_restore: false, background_restore_count: 0,
            native_placement_awaiting_render: false, post_native_repaint_redraw: false,
            artwork_menu_occludes_image: false, repaint_image_after_menu: false,
            native_image_restore_count: 0,
            asynchronous_after_settings: false, settings_visible: false,
            awaiting_viewport_after_settings: false,
            encoding_revision: 0, encode_rx: None,
            encode_request: None, encode_failed: None, encoded_frame: None,
        }
    }
}

impl BrowseArtworkState {
    /// Test-state constructors call this before any artwork request. Explicit
    /// at_path() fixtures still qualify the persistent SQLite implementation.
    pub(super) fn use_memory_only_preview_cache(&mut self) {
        assert!(self.preview_cache.is_none(), "test cache policy must be selected before cache creation");
        self.memory_only_preview_cache = true;
    }

    fn preview_cache_handle(&mut self) -> Arc<InfoPreviewCache> {
        let memory_only = self.memory_only_preview_cache;
        Arc::clone(self.preview_cache.get_or_insert_with(|| Arc::new(
            if memory_only { InfoPreviewCache::memory_only() }
            else { InfoPreviewCache::new() }
        )))
    }

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
        self.fullscreen = false;
        self.warning = None;
    }

    fn clear_selection(&mut self) {
        self.sources.clear();
        self.cursor = 0;
        self.queued_gallery_offset = 0;
        self.artwork_menu_occludes_image = false;
        self.repaint_image_after_menu = false;
        self.native_image_restore_count = 0;
        self.native_placement_awaiting_render = false;
        self.post_native_repaint_redraw = false;
        self.failed.clear();
        self.viewer_decode_bounds = None;
        self.viewer_max_area = Rect::default();
        self.alternate_frames.clear();
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
        self.fullscreen = false;
        self.warning = None;
    }

    /// Settings apply while the image viewer remains open. A new discovery
    /// generation replaces old pixels without exiting the invoking view.
    pub fn invalidate_for_settings(&mut self) {
        let was_open = self.popup_open;
        let was_fullscreen = self.fullscreen;
        self.invalidate();
        self.popup_open = was_open;
        self.fullscreen = was_fullscreen;
    }

    pub fn clear_cache(&mut self) -> (u64, mpsc::Receiver<Result<(), String>>) {
        self.preview_cache_handle().clear()
    }

    pub fn latest_clear_id(&self) -> Option<u64> {
        self.preview_cache.as_ref().map(|cache| cache.latest_clear_id())
    }

    pub fn invalidate_cache_paths(&mut self, paths: &[PathBuf]) {
        if let Some(cache) = &self.preview_cache { cache.invalidate_paths(paths); }
        self.invalidate();
    }

    pub fn suspend(&mut self) {
        self.popup_open = false;
        self.fullscreen = false;
        self.desired_area = Rect::default();
        self.vertical_placement = ImageVerticalPlacement::Center;
        if self.context.is_some() {
            if let Some(cache) = &self.preview_cache { cache.cancel_pending(); }
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
        self.encoding_revision = self.encoding_revision.wrapping_add(1);
        self.encoded_frame = None;
        self.protocol = None;
        self.protocol_area = Rect::default();
        self.native_placement_awaiting_render = false;
        self.post_native_repaint_redraw = false;
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
        // A draw with valid geometry releases the post-settings scheduling
        // hold. A zero-sized viewport cannot authorize encoding old pixels.
        if area.width > 0 && area.height > 0 {
            self.awaiting_viewport_after_settings = false;
        }
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
        self.open_info_popup();
        true
    }

    /// Switching from an Info preview to View discards its pixels and returns
    /// to the original, uncached decoding path. An outstanding Info result is
    /// generation-obsolete; it may finish, but cannot populate the viewer.
    pub fn open_info_popup(&mut self) {
        self.popup_open = true;
        self.asynchronous_after_settings = true;
        self.viewer_max_area = Rect::default();
        self.alternate_frames.clear();
        self.fullscreen = false;
        self.popup_rect = Rect::default();
        self.desired_area = Rect::default(); // wait for viewer geometry before decoding
        self.viewer_decode_bounds = None;
        if matches!(self.context, Some(ArtworkContext::Album { .. })) {
            self.generation = self.generation.wrapping_add(1);
            self.decoded = None;
            self.decoded_source = None;
            self.decoded_embedded_path = None;
        }
        self.invalidate_protocol();
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
        self.asynchronous_after_settings = true;
        self.viewer_max_area = Rect::default();
        self.alternate_frames.clear();
        self.fullscreen = false;
        self.popup_rect = Rect::default();
        self.desired_area = Rect::default();
        self.vertical_placement = ImageVerticalPlacement::Center;
        self.invalidate_protocol();
    }

    // Retain only fixed-origin protocols. Kitty has one-shot image IDs and
    // must always use a new protocol after changing geometry.
    fn viewer_frame_identity(&self, area: Rect) -> Option<ViewerFrameIdentity> {
        if !self.popup_open || !self.prepared_protocol_has_fixed_origin { return None; }
        Some(ViewerFrameIdentity {
            generation: self.generation, pixels: Arc::downgrade(self.decoded.as_ref()?),
            ground: self.viewer_background, font: self.font_size,
            protocol: self.prepared_protocol_type, area,
            picker_generation: self.protocol_generation,
        })
    }

    fn cache_viewer_frame(&mut self, identity: ViewerFrameIdentity, frame: EncodedFrame) {
        // A changed source, theme, or terminal cannot resurrect a stale frame.
        if self.viewer_frame_identity(identity.area).as_ref() != Some(&identity) { return; }
        self.alternate_frames.retain(|(old, _)| old.area != identity.area);
        if self.alternate_frames.len() >= 2 { self.alternate_frames.remove(0); }
        self.alternate_frames.push((identity, frame));
    }

    fn remember_viewer_frame(&mut self) {
        let Some(identity) = self.viewer_frame_identity(self.protocol_area) else { return; };
        if let Some(frame) = self.encoded_frame.take() {
            if frame.key == self.encoded_frame_key() { self.cache_viewer_frame(identity, frame); }
        }
    }

    fn poll_warm_frame(&mut self) -> bool {
        let Some(rx) = self.warm_rx.take() else { return false; };
        match rx.try_recv() {
            Ok(frame) => {
                if let Some(identity) = self.warm_request.take() { self.cache_viewer_frame(identity, frame); }
                true
            }
            Err(TryRecvError::Empty) => { self.warm_rx = Some(rx); false }
            Err(TryRecvError::Disconnected) => { self.warm_failed = self.warm_request.take(); false }
        }
    }

    fn switch_viewport(&mut self, fullscreen: bool) {
        self.remember_viewer_frame();
        self.fullscreen = fullscreen;
        self.asynchronous_after_settings = true;
        self.popup_rect = Rect::default();
        self.desired_area = Rect::default();
        self.needs_background_restore = true;
        self.invalidate_protocol();
    }

    pub fn enter_fullscreen(&mut self) {
        if self.popup_open && !self.fullscreen { self.switch_viewport(true); }
    }

    pub fn toggle_fullscreen(&mut self) {
        if self.popup_open { self.switch_viewport(!self.fullscreen); }
    }

    /// Defer expensive native image encoding after the settings window opens.
    /// Keep the previous encoded frame only while its revision remains valid.
    pub fn artwork_settings_opened(&mut self) {
        self.asynchronous_after_settings = true;
        self.settings_visible = true;
    }

    pub fn set_artwork_settings_visible(&mut self, visible: bool) {
        if self.settings_visible && !visible {
            // Info and View both recalculate their artwork area during draw.
            self.awaiting_viewport_after_settings = true;
        }
        self.settings_visible = visible;
    }

    /// The first Kitty upload may be withheld by tmux preflight. It has not
    /// reached the terminal, so a subsequent draw must still offer that upload.
    pub(super) fn kitty_upload_withheld(&mut self) {
        let key = self.encoded_frame_key();
        if let Some(frame) = self.encoded_frame.as_mut()
            .filter(|frame| frame.key == key && frame.upload_drawn)
        {
            frame.upload_drawn = false;
            self.native_placement_awaiting_render = true;
            self.post_native_repaint_redraw = false;
        }
    }

    pub(super) fn edit_source(&self) -> Option<(PathBuf, bool)> {
        match self.decoded_source.as_ref()? {
            ArtworkSource::File(path) => Some((path.clone(), false)),
            ArtworkSource::Embedded(_) => self.decoded_embedded_path.clone().map(|p| (p, true)),
        }
    }

    pub(super) fn edit_image_finished(&mut self, wrote: bool) {
        self.set_artwork_settings_visible(false);
        self.needs_background_restore = true;
        self.alternate_frames.clear();
        if wrote {
            if let (Some(cache), Some((path, _))) = (&self.preview_cache, self.edit_source()) { cache.invalidate_paths(&[path]); }
            self.generation = self.generation.wrapping_add(1);
            self.failed.clear();
            self.decoded = None;
            self.decoded_source = None;
            self.decoded_embedded_path = None;
        }
        self.invalidate_protocol();
    }

    pub(super) fn set_edit_preview_pixels(&mut self, pixels: Arc<DynamicImage>, _background: Color) {
        self.generation = self.generation.wrapping_add(1);
        self.decoded = Some(pixels);
        self.fullscreen = true; // previews never enlarge
        self.asynchronous_after_settings = true;
        self.needs_background_restore = true;
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
    pub fn has_candidate(&self) -> bool {
        self.sources.iter().any(|source| !self.failed.contains_key(source))
    }
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
        // A selected failure takes priority over discovery status: the
        // sibling scan can still be in flight after the requested file fails.
        if self.popup_open {
            if let Some(message) = self.sources.get(self.cursor)
                .and_then(|source| self.failed.get(source)) {
                return message;
            }
        }
        if self.scanned.is_none() || (self.scan_rx.is_some() && self.sources.is_empty()) {
            return if matches!(&self.context, Some(ArtworkContext::SingleFile(_))) {
                "Loading image…"
            } else { "Finding artwork…" };
        }
        if self.decode_rx.is_some() || self.has_candidate() {
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
            // Album Info silently tries the next usable source if an empty
            // embedded picture tag was found. A deliberate viewer navigation
            // never skips a broken/oversized file: show its own error instead.
            if self.popup_open || !self.failed.contains_key(&self.sources[next]) {
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
        self.fullscreen = false;
        self.popup_rect = Rect::default();
        self.asynchronous_after_settings = false;
        self.settings_visible = false;
        self.awaiting_viewport_after_settings = false;
        self.encoded_frame = None;
        self.alternate_frames.clear();
        self.viewer_max_area = Rect::default();
        self.inline_click_source = None;
        self.artwork_menu_occludes_image = false;
        self.repaint_image_after_menu = false;
        self.native_image_restore_count = 0;
        self.native_placement_awaiting_render = false;
        self.post_native_repaint_redraw = false;
        self.viewer_decode_bounds = None;
        self.protocol = None; // rebuild for the pane on the next prepare pass
        self.protocol_area = Rect::default();
        if !was_file_view && matches!(self.context, Some(ArtworkContext::Album { .. })) {
            // The just-closed viewer used the original decode, not an Info
            // derivative. Resume the pane from its own cache on the next pass.
            self.generation = self.generation.wrapping_add(1);
            self.decoded_source = None;
            self.decoded_embedded_path = None;
            self.decoded = None;
        }
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
                   kitty_generation: usize, config: &BrowseArtworkConfig,
                   background: Color) -> bool {
        // Post-draw preparation asks the event loop for one prompt, non-clearing
        // draw. The second pass paints background cells after the terminal has
        // received its new Sixel/Kitty/iTerm image command.
        let post_render_repair = std::mem::take(&mut self.post_native_repaint_redraw);
        if post_render_repair {
            // Browse may already have consumed the first repair after the
            // native image rendered in that same frame. Re-arm it only AFTER
            // terminal.draw has completed, so the follow-up frame really
            // repaints cells the terminal's image command could have cleared.
            self.needs_background_restore = true;
        }
        let mut changed = post_render_repair;
        changed |= self.poll_workers();
        changed |= self.poll_encoded_frame();
        // Close / Apply can change a viewer height on the same input turn.
        // The event loop prepares before drawing, so its previous desired_area
        // must not spawn an expensive, immediately obsolete encoding job.
        if self.awaiting_viewport_after_settings { return changed; }
        let viewer_background = self.popup_open.then(|| {
            let (r, g, b) = super::theme::rgb_tuple(background);
            [r, g, b]
        });
        if self.viewer_background != viewer_background {
            self.viewer_background = viewer_background;
            if self.popup_open {
                // A viewer decoded for the old theme has literal RGB ground
                // pixels. Invalidate it before placing a protocol with a new
                // theme, including a decode still running on a worker.
                self.needs_background_restore = true;
                self.generation = self.generation.wrapping_add(1);
                self.decoded = None;
                self.decoded_source = None;
                self.decoded_embedded_path = None;
                self.invalidate_protocol();
                changed = true;
            }
        }
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
        let font_size = picker.font_size;
        self.font_size = font_size;
        // Decode once for the largest viewport this terminal can display.
        // A modal/full-screen toggle changes only terminal encoding geometry.
        let decode_area = if self.popup_open && self.desired_area.width > 0
            && self.desired_area.height > 0 && self.viewer_max_area.width > 0 {
            self.viewer_max_area
        } else { self.desired_area };
        let bounds = (
            u32::from(decode_area.width).saturating_mul(u32::from(font_size.0.max(1))),
            u32::from(decode_area.height).saturating_mul(u32::from(font_size.1.max(1))),
        );
        // The popup is measured on its first draw. Do not decode at an
        // arbitrary 1600-pixel fallback then immediately decode again.
        if self.popup_open && self.decoded.is_some()
            && self.viewer_decode_bounds.is_some_and(|old|
                bounds.0 > old.0 || bounds.1 > old.1)
        {
            self.needs_background_restore = true;
            self.generation = self.generation.wrapping_add(1);
            self.decoded = None;
            self.decoded_source = None;
            self.decoded_embedded_path = None;
            self.invalidate_protocol();
        }
        if self.decode_rx.is_none() && self.decoded.is_none()
            && (!self.popup_open || (bounds.0 > 0 && bounds.1 > 0))
            && self.sources.get(self.cursor).is_some_and(|s| !self.failed.contains_key(s))
        {
            if let Some(source) = self.sources.get(self.cursor).cloned() {
                let (tx, rx) = mpsc::channel();
                self.decode_rx = Some(rx);
                let generation = self.generation;
                self.decode_generation = generation;
                self.viewer_decode_bounds = self.popup_open.then_some(bounds);
                // Eligibility is decided by the Info consumer, not by the
                // source's File/Embedded variant. View never receives a cache.
                let info_request = !self.popup_open
                    && matches!(self.context, Some(ArtworkContext::Album { .. }));
                let cache = info_request.then(|| self.preview_cache_handle());
                let cache_epoch = cache.as_ref().map(|cache| cache.epoch());
                thread::spawn(move || {
                    let image = match (&cache, cache_epoch) {
                        (Some(cache), Some(epoch)) => {
                            if let Some((preview, hit)) = cache.lookup(&source, epoch) {
                                Ok(DecodedArtwork {
                                    image: preview.pixels,
                                    embedded_path: preview.embedded_path,
                                    preview_stamps: Some(preview.stamps),
                                    preview_cache_hit: hit,
                                })
                            } else {
                                decode_source_with_edge(&source, PREVIEW_EDGE).map(|mut decoded| {
                                    decoded.preview_stamps = stamps_for_decoded(
                                        &source, decoded.embedded_path.as_deref());
                                    decoded
                                })
                            }
                        }
                        _ => decode_source_with_bounds_and_background(
                            &source, bounds.0, bounds.1, viewer_background),
                        // The viewer decodes only the pixels that can appear
                        // inside the actual terminal's current viewport.
                    };
                    let _ = tx.send(DecodeResult { generation, source, image, cache_epoch });
                });
                changed = true;
            }
        }
        // `o=z` is part of the Kitty graphics specification. Terminals with
        // incomplete implementations can explicitly retain the native upload.
        self.kitty_upload_transport =
            picker.protocol_type == ratatui_image::picker::ProtocolType::Kitty
            && !matches!(std::env::var("TONEPOET_KITTY_ZLIB").as_deref(), Ok("0"));
        self.prepared_protocol_type = picker.protocol_type;
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
        // Full-screen never enlarges small source images, even when the
        // ordinary modal's enlargement preference is enabled.
        let allow_upscale = !self.fullscreen && config.allow_upscale;
        let desired = self.decoded.as_ref().map_or(self.desired_area, |image| {
            fitted_image_area(self.desired_area, image.width(), image.height(), font_size,
                allow_upscale, self.vertical_placement)
        });
        let kitty = if picker.protocol_type == ratatui_image::picker::ProtocolType::Kitty {
            kitty_generation
        } else { 0 };
        if desired.width > 0 && desired.height > 0 && self.decoded.is_some()
            && (self.protocol.is_none() || self.protocol_area != desired
                || self.protocol_generation != picker_generation || self.retransmit_generation != kitty)
        {
            self.encoding_revision = self.encoding_revision.wrapping_add(1);
            self.encoded_frame = None;
            self.protocol = self.decoded.as_ref().map(|image| picker.new_resize_protocol(image.as_ref().clone()));
            self.native_placement_awaiting_render =
                picker.protocol_type != ratatui_image::picker::ProtocolType::Halfblocks;
            if self.native_placement_awaiting_render {
                // The first pass repairs any stale terminal background. A
                // second pass is queued only once this image actually renders.
                self.needs_background_restore = true;
            }
            self.protocol_area = desired;
            self.protocol_generation = picker_generation;
            self.retransmit_generation = kitty;
            changed = true;
        }
        changed |= self.poll_warm_frame();
        let active_identity = self.viewer_frame_identity(desired);
        if self.encoded_frame.is_none() && self.protocol_area == desired {
            if let Some(position) = self.alternate_frames.iter().position(|(identity, _)|
                Some(identity) == active_identity.as_ref()) {
                let (_, mut frame) = self.alternate_frames.remove(position);
                frame.key = self.encoded_frame_key();
                frame.upload_drawn = false;
                self.encoded_frame = Some(frame);
                self.repaint_image_after_menu = true;
                changed = true;
            }
        }
        // One foreground job and one speculative alternate job, at most.
        // The alternate shares the largest-viewport decode and starts on the
        // first windowed encoding, not on the first full-screen key press.
        if self.asynchronous_after_settings && !self.settings_visible
            && picker.protocol_type != ratatui_image::picker::ProtocolType::Halfblocks
            && self.protocol.is_some() && self.decoded.is_some()
            && desired.width > 0 && desired.height > 0
        {
            let key = self.encoded_frame_key();
            if self.encoded_frame.as_ref().is_some_and(|frame| frame.key != key) {
                self.encoded_frame = None;
            }
            if self.encoded_frame.is_none() && self.encode_rx.is_none()
                && self.encode_failed != Some(key)
                && !(active_identity.is_some() && self.warm_request == active_identity)
            {
                let pixels = Arc::clone(self.decoded.as_ref().expect("checked"));
                let picker = picker.clone();
                let (r, g, b) = super::theme::rgb_tuple(background);
                let compress_kitty = self.kitty_upload_transport;
                let (tx, rx) = mpsc::channel();
                self.encode_rx = Some(rx);
                self.encode_request = Some(key);
                self.encode_viewer_request = active_identity.clone();
                thread::spawn(move || {
                    let _ = tx.send(encode_native_frame(&pixels, picker, key, [r, g, b], compress_kitty));
                });
            }
            // Do not warm Info, Edit Image's synthetic preview, or geometry
            // not yet established by the actual viewer draw. On an early f,
            // the existing warm job becomes the requested job, not a duplicate.
            if active_identity.is_some() && self.warm_rx.is_none()
                && self.viewer_max_area.width >= 8 && self.viewer_max_area.height >= 5 {
                let modal = popup_rect(self.viewer_max_area, config.popup_height_percent);
                let active_viewport = if self.fullscreen { self.viewer_max_area } else { modal };
                let alternate_viewport = if self.fullscreen { modal } else { self.viewer_max_area };
                if self.desired_area == active_viewport {
                    let pixels = self.decoded.as_ref().expect("checked");
                    let alternate = fitted_image_area(alternate_viewport, pixels.width(), pixels.height(),
                        font_size, self.fullscreen && config.allow_upscale, ImageVerticalPlacement::Center);
                    if alternate.width > 0 && alternate.height > 0 && alternate != desired {
                        let identity = self.viewer_frame_identity(alternate).expect("fixed viewer");
                        if self.encode_viewer_request.as_ref() != Some(&identity)
                            && self.warm_failed.as_ref() != Some(&identity)
                            && !self.alternate_frames.iter().any(|(cached, _)| cached == &identity) {
                            let pixels = Arc::clone(pixels);
                            let picker = picker.clone();
                            let ground = self.viewer_background.expect("viewer ground");
                            let key = EncodedFrameKey { area: alternate, ..key };
                            let (tx, rx) = mpsc::channel();
                            self.warm_rx = Some(rx);
                            self.warm_request = Some(identity);
                            thread::spawn(move || {
                                let _ = tx.send(encode_native_frame(&pixels, picker, key, ground, false));
                            });
                        }
                    }
                }
            }
        }
        changed
    }

    fn encoded_frame_key(&self) -> EncodedFrameKey {
        EncodedFrameKey {
            revision: self.encoding_revision,
            area: self.protocol_area,
            protocol_generation: self.protocol_generation,
            retransmit_generation: self.retransmit_generation,
        }
    }

    fn poll_encoded_frame(&mut self) -> bool {
        let Some(rx) = self.encode_rx.take() else { return false; };
        match rx.try_recv() {
            Ok(frame) => {
                self.encode_request = None;
                let identity = self.encode_viewer_request.take();
                if self.asynchronous_after_settings && frame.key == self.encoded_frame_key() {
                    self.encoded_frame = Some(frame);
                    return true;
                }
                if let Some(identity) = identity {
                    self.cache_viewer_frame(identity, frame);
                    return true;
                }
            }
            Err(TryRecvError::Empty) => { self.encode_rx = Some(rx); }
            Err(TryRecvError::Disconnected) => {
                // A failed worker leaves the image hidden. Do not immediately
                // respawn for the same artwork/geometry on every UI tick.
                self.encode_failed = self.encode_request.take();
                self.encode_viewer_request = None;
            }
        }
        false
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
                        // Discovery can reorder the cursor. Keep earlier
                        // source-bound decode failures for retained images.
                        self.failed.retain(|source, _| self.sources.contains(source));
                        // Don't throw away the selected file's pending/decoded
                        // pixels when sibling discovery completes.
                        if !direct_file || self.decoded_source.as_ref() != self.sources.get(self.cursor) {
                            self.decoded = None;
                            self.decoded_source = None;
                            self.decoded_embedded_path = None;
                            self.invalidate_protocol();
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
                                if let (Some(epoch), Some(stamps), Some(cache)) = (
                                    result.cache_epoch, decoded.preview_stamps,
                                    self.preview_cache.as_ref(),
                                ) {
                                    cache.admit(&result.source, epoch, Preview {
                                        pixels: Arc::clone(&decoded.image),
                                        embedded_path: decoded.embedded_path.clone(),
                                        stamps,
                                    }, decoded.preview_cache_hit);
                                }
                                self.decoded_source = Some(result.source);
                                self.decoded_embedded_path = decoded.embedded_path;
                                self.decoded = Some(decoded.image);
                                self.invalidate_protocol();
                            }
                            Err(error) => {
                                self.record_decode_failure(&result.source, error);
                            }
                        }
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => self.decode_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    if self.decode_generation == self.generation {
                        if let Some(source) = self.sources.get(self.cursor).cloned() {
                            self.record_decode_failure(&source,
                                "Artwork decode worker failed".into());
                        }
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn record_decode_failure(&mut self, source: &ArtworkSource, error: String) {
        self.decoded = None;
        self.decoded_source = None;
        self.decoded_embedded_path = None;
        self.invalidate_protocol();
        self.failed.insert(source.clone(), format!("Cannot display {}: {error}", source.label()));
        if !self.popup_open {
            // Info pane discovery has always preferred embedded art but
            // permitted missing/invalid embedded tags to fall back to files.
            // Keep that behavior; explicit popup navigation never substitutes.
            self.cycle(true);
        }
    }

    /// The renderer records geometry and paints prepared protocol state.
    /// After settings opens, native resize/encode is worker-owned; a pending
    /// worker must never fall back to a blocking image encode here.
    #[cfg(test)]
    pub(super) fn native_frame_ready(&self)->bool {
        self.decoded.is_some()&&self.protocol.is_some()&&self.protocol_area.width>0&&self.protocol_area.height>0
            &&(!self.asynchronous_after_settings||self.prepared_protocol_type==ratatui_image::picker::ProtocolType::Halfblocks
                ||self.encoded_frame.as_ref().is_some_and(|frame|frame.key==self.encoded_frame_key()))
    }

    pub fn render_image(&mut self, f: &mut ratatui::Frame, area: Rect,
                        allow_upscale: bool, placement: ImageVerticalPlacement,
                        background: Color) -> bool {
        self.set_desired_area(area, placement);
        if self.settings_visible { return false; }
        if self.decoded.is_none() || self.protocol.is_none() { return false; }
        let image = self.decoded.as_ref().expect("checked");
        let target = fitted_image_area(area, image.width(), image.height(), self.font_size,
            allow_upscale, placement);
        if target.width == 0 || target.height == 0 || self.protocol_area != target { return false; }
        // The image protocol needs the actual UI background when padding or
        // compositing source pixels, not the terminal's default background.
        let (r, g, b) = super::theme::rgb_tuple(background);
        let async_native = self.asynchronous_after_settings
            && self.prepared_protocol_type != ratatui_image::picker::ProtocolType::Halfblocks
            && !self.settings_visible;
        // When the async path is enabled, never fall back to synchronous
        // native encoding on a cache miss. That would stall Esc or Apply.
        if async_native {
            let key = self.encoded_frame_key();
            let Some(frame) = self.encoded_frame.as_mut()
                .filter(|frame| frame.key == key) else { return false; };
            let cells = if frame.upload_drawn {
                frame.placement_cells.as_ref().unwrap_or(&frame.cells)
            } else { &frame.cells };
            for y in 0..target.height {
                for x in 0..target.width {
                    f.buffer_mut().get_mut(target.x + x, target.y + y)
                        .clone_from(cells.get(x, y));
                }
            }
            frame.upload_drawn = true;
        } else {
            let widget = ratatui_image::StatefulImage::new(Some(image::Rgb([r, g, b])));
            f.render_stateful_widget(widget, target,
                self.protocol.as_mut().expect("checked"));
        }
        // ratatui-image 1.0.5 places its first Kitty upload and Unicode
        // placeholders in the image-origin cell. Transform that one-shot APC
        // without changing its ID, placement, cache, or subsequent redraws.
        // Failed/non-beneficial compression leaves the original bytes intact.
        if self.kitty_upload_transport && !async_native {
            let cell = f.buffer_mut().get_mut(target.x, target.y);
            if let Some(compressed) = super::kitty_transport::compress_kitty_upload(cell.symbol()) {
                cell.set_symbol(&compressed);
            }
        }
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
        if self.native_placement_awaiting_render {
            // Native graphics can overwrite the first blank-cell repaint as
            // their escape sequences are emitted. Force a DIFFERENT invisible
            // ink on the next frame; do not continuously repaint while idle.
            self.native_placement_awaiting_render = false;
            self.needs_background_restore = true;
            self.post_native_repaint_redraw = true;
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

#[cfg(test)]
fn decode_source(source: &ArtworkSource) -> Result<DecodedArtwork, String> {
    decode_source_with_edge(source, MAX_IMAGE_DIMENSION)
}

fn decode_source_with_edge(source: &ArtworkSource, edge: u32) -> Result<DecodedArtwork, String> {
    decode_source_with_bounds(source, edge, edge)
}

fn decode_source_with_bounds(source: &ArtworkSource, width: u32, height: u32)
    -> Result<DecodedArtwork, String>
{
    decode_source_with_bounds_and_background(source, width, height, None)
}

fn decode_source_with_bounds_and_background(
    source: &ArtworkSource, width: u32, height: u32, background: Option<[u8; 3]>,
) -> Result<DecodedArtwork, String> {
    match source {
        ArtworkSource::File(path) => {
            // Check both before and after opening; the source may change while
            // the nonrecursive scan is running. Never buffer a second 100 MiB
            // copy of a regular image file solely for format detection.
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
                return Err("Artwork exceeds 100 MiB encoded-size limit".into());
            }
            Ok(DecodedArtwork {
                image: Arc::new(decode_image_reader(BufReader::new(file), width, height, background)?),
                embedded_path: None, preview_stamps: None, preview_cache_hit: false,
            })
        }
        ArtworkSource::Embedded(paths) => {
            // An absent embedded picture is not a failing gallery file: scan
            // the next album source instead. The same safety limits apply to
            // each extracted embedded image.
            let mut last_error = None;
            for path in paths.iter() {
                if !fs::symlink_metadata(path).is_ok_and(|md| md.is_file()) { continue; }
                let bytes = super::probe::read_embedded_picture_bytes(path, PictureType::CoverFront)
                    .or_else(|_| super::probe::read_embedded_picture_bytes(path, PictureType::Other));
                match bytes.and_then(|data| decode_image_bytes_with_bounds(data, width, height, background)) {
                    Ok(image) => return Ok(DecodedArtwork {
                        image: Arc::new(image), embedded_path: Some(path.clone()),
                        preview_stamps: None, preview_cache_hit: false,
                    }),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| "No usable embedded artwork in album".into()))
        }
    }
}

fn decode_image_bytes_with_bounds(
    bytes: Vec<u8>, width: u32, height: u32, background: Option<[u8; 3]>,
) -> Result<DynamicImage, String> {
    if bytes.len() as u64 > MAX_ENCODED_IMAGE_BYTES {
        return Err("Artwork exceeds 100 MiB encoded-size limit".into());
    }
    decode_image_reader(Cursor::new(bytes), width, height, background)
}

/// Composite before downsampling: Sixel does not carry PNG alpha and its
/// palette quantizer otherwise uses hidden RGB values as visible edge ink.
/// Opaque images retain their existing decoding and resizing path.
pub(super) fn composite_viewer_alpha(image: DynamicImage, background: [u8; 3]) -> DynamicImage {
    if !image.color().has_alpha() { return image; }
    // PNG encoders often retain an RGBA channel even for wholly opaque art.
    // Preserve that image's original pixel format and resizing path.
    if let DynamicImage::ImageRgba8(pixels) = &image {
        if pixels.as_raw().chunks_exact(4).all(|pixel| pixel[3] == 255) {
            return image;
        }
    }
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bytes = rgba.into_raw();
    // Pack into the same allocation. The RGB write cursor always trails the
    // next RGBA read; even large originals need no second full-frame buffer.
    let mut output = 0;
    for offset in (0..bytes.len()).step_by(4) {
        let alpha = u32::from(bytes[offset + 3]);
        for channel in 0..3 {
            bytes[output + channel] = ((u32::from(bytes[offset + channel]) * alpha
                + u32::from(background[channel]) * (255 - alpha) + 127) / 255) as u8;
        }
        output += 3;
    }
    bytes.truncate(output);
    DynamicImage::ImageRgb8(image::RgbImage::from_raw(width, height, bytes)
        .expect("valid RGBA geometry yields packed RGB pixels"))
}

fn decode_image_reader<R: std::io::BufRead + Seek>(
    reader: R, width: u32, height: u32, background: Option<[u8; 3]>,
) -> Result<DynamicImage, String> {

    let mut reader = ImageReader::new(reader)
        .with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?;
    let image = if let Some(ground) = background {
        composite_viewer_alpha(image, ground)
    } else { image };
    let (width, height) = (width.max(1), height.max(1));
    if image.width() > width || image.height() > height {
        Ok(image.resize(width, height, image::imageops::FilterType::Triangle))
    } else { Ok(image) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_composites_transparent_png_before_resizing_without_white_or_edge_ink() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("transparent.png");
        let background = [17, 31, 47];
        let mut pixels = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 0, 0]));
        pixels.put_pixel(1, 1, image::Rgba([210, 70, 20, 255]));
        pixels.put_pixel(2, 1, image::Rgba([210, 70, 20, 128]));
        DynamicImage::ImageRgba8(pixels).save(&path).unwrap();

        let source = ArtworkSource::File(path);
        let decoded = decode_source_with_bounds_and_background(&source, 4, 4, Some(background))
            .expect("PNG with transparent surroundings");
        let rgb = decoded.image.to_rgb8();
        assert!(matches!(decoded.image.as_ref(), DynamicImage::ImageRgb8(_)));
        assert_eq!(rgb.get_pixel(0, 0).0, background, "fully transparent pixel");
        assert_eq!(rgb.get_pixel(3, 3).0, background, "transparent centre/edge");
        assert_eq!(rgb.get_pixel(1, 1).0, [210, 70, 20], "opaque artwork");
        assert_eq!(rgb.get_pixel(2, 1).0, [114, 51, 33], "half-alpha edge");

        let smaller = decode_source_with_bounds_and_background(&source, 2, 2, Some(background))
            .expect("downsample pre-composited artwork");
        assert_eq!((smaller.image.width(), smaller.image.height()), (2, 2));
        assert!(matches!(smaller.image.as_ref(), DynamicImage::ImageRgb8(_)));
    }

    #[test]
    fn artwork_settings_escape_close_and_apply_preserve_the_viewer() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        use crate::tui::{
            app::{ActiveOverlay, AppScreen, AppState},
            artwork_settings,
            button_map::TuiButton,
            context_menu::{execute_context_action, ContextAction},
            keybindings,
        };

        for fullscreen in [false, true] {
            // Esc key, Esc Close click, Apply click, Apply shortcut.
            for dismiss in 0..4 {
                let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
                app.current_screen = AppScreen::Browse;
                app.browse_artwork.open_file_popup(PathBuf::from("front.png"));
                if fullscreen { app.browse_artwork.enter_fullscreen(); }
                let (tx, _rx) = tokio::sync::mpsc::channel(1);
                execute_context_action(&mut app,
                    ContextAction::BrowseArtworkOpenSettings, &tx, false);
                assert!(matches!(app.active_overlay, ActiveOverlay::ArtworkSettings(_)));
                assert!(app.browse_artwork.asynchronous_after_settings,
                    "the production context action must enable non-blocking redraws");
                if dismiss == 1 || dismiss == 2 {
                    let config = app.browse.artwork_config.clone();
                    let theme = app.theme;
                    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
                    terminal.draw(|frame| {
                        if let ActiveOverlay::ArtworkSettings(settings) = &mut app.active_overlay {
                            artwork_settings::draw(frame, settings, &config,
                                &mut app.button_map, theme);
                        }
                    }).unwrap();
                    let target = if dismiss == 1 {
                        TuiButton::ArtworkClose
                    } else { TuiButton::ArtworkApply };
                    let hit = app.button_map.button_rect(target)
                        .expect("The dismissal control must have a mouse target");
                    keybindings::handle_mouse(&mut app, MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: hit.x + hit.width / 2, row: hit.y,
                        modifiers: KeyModifiers::NONE,
                    }, &tx);
                } else {
                    let key = if dismiss == 3 { KeyCode::Char('a') } else { KeyCode::Esc };
                    keybindings::handle_key(&mut app,
                        KeyEvent::new(key, KeyModifiers::NONE), &tx);
                }
                assert!(matches!(app.active_overlay, ActiveOverlay::None));
                assert!(app.browse_artwork.popup_open,
                    "dismissing settings must retain the viewer");
                assert_eq!(app.browse_artwork.fullscreen, fullscreen);
                assert!(app.browse_artwork.asynchronous_after_settings,
                    "the first viewer repaint must not block input on an encode");
            }
        }
    }

    #[test]
    fn settings_close_never_reenters_native_encode_during_draw() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let area = Rect::new(2, 2, 40, 24);
        let ground = Color::Rgb(26, 27, 38);
        let config = BrowseArtworkConfig::default();
        let mut state = BrowseArtworkState::default();
        state.popup_open = true;
        state.viewer_background = Some([26, 27, 38]);
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(160, 160)));
        state.set_desired_area(area, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        state.artwork_settings_opened();
        assert!(state.prepare(&mut picker, 0, 0, &config, ground));
        assert!(state.protocol.is_some());
        let viewport = state.protocol_area;
        let mut terminal = Terminal::new(TestBackend::new(60, 32)).unwrap();
        terminal.draw(|frame| {
            assert!(!state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Center, ground));
        }).unwrap();
        assert!(state.native_placement_awaiting_render,
            "the hidden image must not consume its native upload");

        state.set_artwork_settings_visible(false);
        // An encode cache miss must NOT run StatefulImage::render in UI draw.
        // The slow native Sixel renderer would consume the placement flag.
        terminal.draw(|frame| {
            assert!(!state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Center, ground));
        }).unwrap();
        assert!(state.native_placement_awaiting_render,
            "closing the window must defer native rendering to its worker");

        // Simulate the completed worker. The UI copies its already-encoded
        // cells without invoking the native resize/encode path.
        let mut cells = Buffer::empty(Rect::new(0, 0, viewport.width, viewport.height));
        cells.get_mut(0, 0).set_symbol("native-test");
        let key = state.encoded_frame_key();
        state.encoded_frame = Some(EncodedFrame {
            key, cells, placement_cells: None, upload_drawn: false,
        });
        terminal.draw(|frame| {
            assert!(state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Center, ground));
        }).unwrap();
        assert_eq!(terminal.backend().buffer().get(viewport.x, viewport.y).symbol(),
                   "native-test");
        assert!(!state.native_placement_awaiting_render);
        state.invalidate_protocol();
        assert!(state.encoded_frame.is_none(),
            "resize/source invalidation must reject the old native command");
    }

    // A long native image encode must not own the application's input loop.
    // Keep its sending end alive while driving the normal prepare/input/draw
    // sequence. A blocking receive would hang this test instead of drawing.
    #[test]
    fn pending_native_encode_does_not_block_settings_input_or_dismissal() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        use crate::tui::{
            app::{ActiveOverlay, AppScreen, AppState},
            button_map::TuiButton,
            context_menu::{execute_context_action, ContextAction},
            draw::draw_ui,
            keybindings,
        };

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        for fullscreen in [false, true] {
            // Esc, Close click, Apply click, Apply key, and nondismissal Tab.
            for action in 0..5 {
                let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
                app.current_screen = AppScreen::Browse;
                app.browse_artwork.popup_open = true;
                app.browse_artwork.fullscreen = fullscreen;
                app.browse_artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(240, 240)));
                let (r, g, b) = super::super::theme::rgb_tuple(app.theme.bg);
                app.browse_artwork.viewer_background = Some([r, g, b]);
                let screen = Rect::new(0, 0, 100, 40);
                let viewport = if fullscreen { screen } else {
                    popup_rect(screen, app.browse.artwork_config.popup_height_percent)
                };
                app.browse_artwork.set_desired_area(viewport, ImageVerticalPlacement::Center);
                app.prepare_image_preview_protocols();
                execute_context_action(&mut app, ContextAction::BrowseArtworkOpenSettings,
                    &tx, false);
                let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
                app.prepare_image_preview_protocols();
                terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
                assert!(matches!(app.active_overlay, ActiveOverlay::ArtworkSettings(_)));

                // Simulate a worker still encoding. Do not release its result
                // during any of the following production update/draw passes.
                let (send_later, pending) = mpsc::channel::<EncodedFrame>();
                // The producer deliberately cannot reply during the draw.
                // A generous deadlock watchdog only releases it if a future
                // blocking receive regresses into the input path.
                let (finished_tx, finished_rx) = mpsc::channel::<()>();
                let watchdog = std::thread::spawn(move || {
                    let blocked = finished_rx.recv_timeout(Duration::from_secs(10)).is_err();
                    drop(send_later);
                    blocked
                });
                let key = app.browse_artwork.encoded_frame_key();
                app.browse_artwork.encode_rx = Some(pending);
                app.browse_artwork.encode_request = Some(key);
                app.browse_artwork.encoded_frame = None;
                if action == 1 || action == 2 {
                    let button = if action == 1 { TuiButton::ArtworkClose }
                        else { TuiButton::ArtworkApply };
                    let hit = app.button_map.button_rect(button).unwrap();
                    keybindings::handle_mouse(&mut app, MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: hit.x + hit.width / 2, row: hit.y,
                        modifiers: KeyModifiers::NONE,
                    }, &tx);
                } else {
                    let key = match action {
                        0 => KeyCode::Esc,
                        3 => KeyCode::Char('a'),
                        _ => KeyCode::Tab,
                    };
                    keybindings::handle_key(&mut app,
                        KeyEvent::new(key, KeyModifiers::NONE), &tx);
                }
                let still_open = action == 4;
                assert_eq!(matches!(app.active_overlay, ActiveOverlay::ArtworkSettings(_)), still_open);
                // These are the same pre-draw / draw / post-draw steps as the
                // event loop; they must finish before the encoder replies.
                app.prepare_image_preview_protocols();
                terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
                app.prepare_image_preview_protocols();
                let visible = terminal.backend().buffer().content.iter()
                    .map(|cell| cell.symbol()).collect::<Vec<_>>().join("");
                assert_eq!(visible.contains("artwork settings"), still_open,
                    "settings visibility must reflect the input before the encode finishes");
                assert!(app.browse_artwork.popup_open);
                assert!(app.browse_artwork.encode_rx.is_some(),
                    "the result has intentionally not been delivered");
                let _ = finished_tx.send(());
                assert!(!watchdog.join().unwrap(),
                    "input/draw waited for an unfinished native encoder");
            }
        }
    }

    #[test]
    fn settings_close_defers_encoding_until_measured_viewport() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;
        use crate::tui::{app::{ActiveOverlay, AppScreen, AppState}, draw::draw_ui};

        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let ground = app.theme.bg;
        app.current_screen = AppScreen::Browse;
        app.browse_artwork.popup_open = true;
        let (r, g, b) = super::super::theme::rgb_tuple(app.theme.bg);
        app.browse_artwork.viewer_background = Some([r, g, b]);
        app.browse_artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(400, 400)));
        let screen = Rect::new(0, 0, 100, 40);
        let old_viewport = popup_rect(screen, 60);
        app.browse_artwork.set_desired_area(old_viewport, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        app.browse_artwork.prepare(&mut picker, 0, 0,
            &app.browse.artwork_config, ground);
        let old_target = app.browse_artwork.protocol_area;
        app.browse_artwork.artwork_settings_opened();
        app.active_overlay = ActiveOverlay::ArtworkSettings(Box::new(
            super::super::artwork_settings::ArtworkSettingsState::new(
                &app.browse.artwork_config, Rect::default())));
        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        // Height increases while artwork is hidden. The actual popup draw
        // records the new viewport, rather than the old view's coordinates.
        app.browse.artwork_config.popup_height_percent = 80;
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        let new_viewport = popup_rect(screen, 80);
        let expected_target = fitted_image_area(new_viewport, 400, 400, picker.font_size,
            app.browse.artwork_config.allow_upscale, ImageVerticalPlacement::Center);
        assert_ne!(old_target, expected_target, "the fixture must distinguish old and new encodes");
        assert_eq!(app.browse_artwork.desired_area, new_viewport);
        app.active_overlay = ActiveOverlay::None;
        app.browse_artwork.set_artwork_settings_visible(false);
        assert!(app.browse_artwork.awaiting_viewport_after_settings);
        app.browse_artwork.prepare(&mut picker, 0, 0,
            &app.browse.artwork_config, ground);
        assert!(app.browse_artwork.encode_request.is_none(),
            "pre-draw preparation must never enqueue an old viewport");
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        assert!(!app.browse_artwork.awaiting_viewport_after_settings);
        app.browse_artwork.prepare(&mut picker, 0, 0,
            &app.browse.artwork_config, ground);
        assert!(app.browse_artwork.encode_request.is_some(),
            "post-draw preparation must enqueue the measured viewport");
        assert_eq!(app.browse_artwork.encode_request.unwrap().area, expected_target);
        assert_eq!(app.browse_artwork.protocol_area, expected_target);
        assert_eq!(app.browse_artwork.desired_area, new_viewport);

        // A smaller terminal while settings is open is measured the same way.
        app.active_overlay = ActiveOverlay::ArtworkSettings(Box::new(
            super::super::artwork_settings::ArtworkSettingsState::new(
                &app.browse.artwork_config, Rect::default())));
        app.browse_artwork.set_artwork_settings_visible(true);
        let mut narrower = Terminal::new(TestBackend::new(90, 34)).unwrap();
        narrower.draw(|f| draw_ui(f, &mut app)).unwrap();
        assert_eq!(app.browse_artwork.desired_area, popup_rect(Rect::new(0, 0, 90, 34), 80));
    }

    #[test]
    fn smaller_viewport_keeps_a_suitable_decoded_image() {
        let mut state = BrowseArtworkState::default();
        state.popup_open = true;
        state.viewer_background = Some([26, 27, 38]);
        let pixels = Arc::new(DynamicImage::new_rgb8(400, 400));
        state.decoded = Some(Arc::clone(&pixels));
        state.viewer_decode_bounds = Some((800, 600));
        state.set_desired_area(Rect::new(4, 4, 50, 24), ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default(),
            Color::Rgb(26, 27, 38));
        assert!(Arc::ptr_eq(state.decoded.as_ref().expect("reused pixels"), &pixels));
        assert!(state.decode_rx.is_none(), "shrink must not restart the decoder");
    }

    #[test]
    fn pending_view_height_apply_skips_old_viewport_encode() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;
        use crate::tui::{
            app::{ActiveOverlay, AppScreen, AppState},
            context_menu::{execute_context_action, ContextAction},
            draw::draw_ui,
            keybindings,
            test_support::XdgConfigHomeGuard,
        };
        let _home = XdgConfigHomeGuard::new("tonepoet-artwork-apply-geometry");
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse_artwork.popup_open = true;
        app.browse_artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(400, 400)));
        let (r, g, b) = super::super::theme::rgb_tuple(app.theme.bg);
        app.browse_artwork.viewer_background = Some([r, g, b]);
        let screen = Rect::new(0, 0, 100, 40);
        let old_viewport = popup_rect(screen, 60);
        app.browse_artwork.set_desired_area(old_viewport, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        app.browse_artwork.prepare(&mut picker, 0, 0, &app.browse.artwork_config, app.theme.bg);
        let old_target = app.browse_artwork.protocol_area;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        execute_context_action(&mut app, ContextAction::BrowseArtworkOpenSettings,
            &tx, false);
        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        // SOURCE's first control is focused on entry. Move to View height,
        // edit 80, then Apply from the actual keybinding dispatch.
        for _ in 0..5 {
            keybindings::handle_key(&mut app,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &tx);
        }
        keybindings::handle_key(&mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        for digit in ['8', '0'] {
            keybindings::handle_key(&mut app,
                KeyEvent::new(KeyCode::Char(digit), KeyModifiers::NONE), &tx);
        }
        assert_eq!(app.browse.artwork_config.popup_height_percent, 60);
        keybindings::handle_key(&mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), &tx);
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(app.browse.artwork_config.popup_height_percent, 80);
        assert!(app.browse_artwork.popup_open);
        let saved = crate::config::TonepoetConfig::load().expect("saved Apply value");
        assert_eq!(saved.browsing.artwork.popup_height_percent, 80);
        app.browse_artwork.set_artwork_settings_visible(false);
        app.browse_artwork.prepare(&mut picker, 0, 0, &app.browse.artwork_config, app.theme.bg);
        assert!(app.browse_artwork.encode_request.is_none(),
            "Apply must not start an obsolete old-sized worker before drawing");
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        let new_viewport = popup_rect(screen, 80);
        let expected_target = fitted_image_area(new_viewport, 400, 400, picker.font_size,
            app.browse.artwork_config.allow_upscale, ImageVerticalPlacement::Center);
        assert_ne!(old_target, expected_target);
        assert_eq!(app.browse_artwork.desired_area, new_viewport);
        app.browse_artwork.prepare(&mut picker, 0, 0, &app.browse.artwork_config, app.theme.bg);
        assert_eq!(app.browse_artwork.encode_request.expect("new encode").area,
            expected_target);
    }

    #[test]
    fn kitty_worker_sends_upload_once_and_retries_withheld_upload() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let background = Color::Rgb(26, 27, 38);
        let config = BrowseArtworkConfig::default();
        let mut state = BrowseArtworkState::default();
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(192, 128)));
        let viewport = Rect::new(2, 2, 30, 16);
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        state.artwork_settings_opened();
        state.set_artwork_settings_visible(false);
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        assert!(state.prepare(&mut picker, 0, 0, &config, background));
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.encoded_frame.is_none() && Instant::now() < deadline {
            state.poll_encoded_frame();
            std::thread::sleep(Duration::from_millis(5));
        }
        let encoded = state.encoded_frame.as_ref().expect("native worker result");
        let first = encoded.cells.get(0, 0).symbol();
        let repeated = encoded.placement_cells.as_ref().expect("Kitty placement buffer")
            .get(0, 0).symbol();
        assert!(first.contains("\x1b_G"), "first frame contains Kitty upload");
        assert!(first.contains("o=z"), "the worker compresses the upload");
        assert!(!repeated.contains("\x1b_G"), "placement cannot re-upload pixels");
        assert!(first.ends_with(repeated), "image ID and placeholders are stable");

        let mut terminal = Terminal::new(TestBackend::new(45, 25)).unwrap();
        let target = state.protocol_area;
        // Model the actual tmux preflight decision: suppress the first upload
        // from the terminal buffer, then notify the prepared-frame owner.
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
            assert!(super::super::kitty_transport::buffer_has_kitty_upload(frame.buffer_mut()));
            super::super::kitty_transport::suppress_unverified_tmux_graphics(frame.buffer_mut());
            state.kitty_upload_withheld();
        }).unwrap();
        assert!(!terminal.backend().buffer().get(target.x, target.y)
            .symbol().contains("\x1b_G"), "withheld upload is absent from the sent frame");
        assert!(!state.encoded_frame.as_ref().unwrap().upload_drawn);

        // After verification, the FIRST accepted frame still uploads the
        // compressed image, and the following frame only places placeholders.
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        assert!(terminal.backend().buffer().get(target.x, target.y)
            .symbol().contains("\x1b_G"), "verification retry must contain image data");
        state.prepare(&mut picker, 0, 0, &config, background);
        assert!(state.encode_rx.is_none(), "background repair must reuse cached cells");
        let first_key = state.encoded_frame_key();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        assert!(!terminal.backend().buffer().get(target.x, target.y)
            .symbol().contains("\x1b_G"), "ordinary redraw must only place the image");
        assert_eq!(state.encoded_frame_key(), first_key,
            "background repaint must not restart Kitty encoding");

        // A genuine viewport change allocates a new protocol and permits
        // another upload, with a new image identity.
        let smaller_viewport = Rect::new(2, 2, 18, 9);
        state.set_desired_area(smaller_viewport, ImageVerticalPlacement::Center);
        state.prepare(&mut picker, 0, 0, &config, background);
        assert_ne!(state.encoded_frame_key(), first_key);
        assert!(state.encoded_frame.is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.encoded_frame.is_none() && Instant::now() < deadline {
            state.poll_encoded_frame();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.encoded_frame.is_some(), "new geometry is encoded");
        terminal.draw(|frame| {
            assert!(state.render_image(frame, smaller_viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, background));
        }).unwrap();
        let new_target = state.protocol_area;
        assert!(terminal.backend().buffer().get(new_target.x, new_target.y)
            .symbol().contains("\x1b_G"), "new viewport must transmit new pixels");
    }

    #[test]
    fn encoded_worker_rejects_superseded_geometry_and_bounds_retries() {
        let mut state = BrowseArtworkState::default();
        state.asynchronous_after_settings = true;
        state.protocol_area = Rect::new(4, 6, 24, 12);
        let old_key = state.encoded_frame_key();
        let (tx, rx) = mpsc::channel();
        state.encode_rx = Some(rx);
        state.encode_request = Some(old_key);
        state.invalidate_protocol();
        tx.send(EncodedFrame { key: old_key,
            cells: Buffer::empty(Rect::new(0, 0, 24, 12)),
            placement_cells: None, upload_drawn: false,
        }).unwrap();
        assert!(!state.poll_encoded_frame(), "stale encode is not a draw event");
        assert!(state.encoded_frame.is_none(), "stale native bytes must not display");
        assert!(state.encode_rx.is_none());

        let key = state.encoded_frame_key();
        let (tx, rx) = mpsc::channel::<EncodedFrame>();
        state.encode_rx = Some(rx);
        state.encode_request = Some(key);
        drop(tx);
        assert!(!state.poll_encoded_frame());
        assert_eq!(state.encode_failed, Some(key),
            "a failed encode must not trigger a new worker on every UI tick");
    }

    #[test]
    fn first_viewer_background_setup_preserves_later_decoded_pixels() {
        let mut state = BrowseArtworkState::default();
        state.popup_open = true;
        state.set_desired_area(Rect::new(0, 0, 24, 12), ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        let config = BrowseArtworkConfig::default();
        let background = Color::Rgb(26, 27, 38);

        // A real viewer prepares the background before its decode worker returns.
        assert!(state.prepare(&mut picker, 0, 0, &config, background));
        assert_eq!(state.viewer_background, Some([26, 27, 38]));
        assert!(state.decoded.is_none());

        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(24, 24)));
        let generation = state.generation;
        assert!(state.prepare(&mut picker, 0, 0, &config, background));
        assert_eq!(state.generation, generation);
        assert!(state.decoded.is_some());
        assert!(state.protocol.is_some());
    }

    #[test]
    fn viewer_background_change_invalidates_precomposited_pixels() {
        let mut state = BrowseArtworkState::default();
        let mut picker = Picker::new((8, 12));
        state.popup_open = true;
        state.viewer_background = Some([10, 20, 30]);
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(4, 4)));
        let before = state.generation;
        assert!(state.prepare(&mut picker, 1, 0,
            &BrowseArtworkConfig::default(), Color::Rgb(35, 45, 55)));
        assert!(state.decoded.is_none());
        assert_ne!(state.generation, before);
        assert_eq!(state.viewer_background, Some([35, 45, 55]));
    }

    #[test]
    fn opaque_viewer_inputs_preserve_existing_format_and_pixel_values() {
        let input = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2, 2, image::Rgb([150, 40, 10])));
        let output = composite_viewer_alpha(input.clone(), [12, 23, 34]);
        assert!(matches!(output, DynamicImage::ImageRgb8(_)));
        assert_eq!(input.as_bytes(), output.as_bytes());

        let opaque_rgba = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2, 2, image::Rgba([150, 40, 10, 255])));
        let output = composite_viewer_alpha(opaque_rgba.clone(), [12, 23, 34]);
        assert!(matches!(output, DynamicImage::ImageRgba8(_)));
        assert_eq!(opaque_rgba.as_bytes(), output.as_bytes());
    }

    #[test]
    fn test_app_constructors_keep_implicit_artwork_cache_in_memory() {
        let mut app = crate::tui::app::AppState::new_for_test(
            crate::config::TonepoetConfig::default());
        assert!(app.browse_artwork.memory_only_preview_cache);
        assert!(app.browse_artwork.preview_cache.is_none(), "remain lazy");
        assert!(app.browse_artwork.preview_cache_handle().is_memory_only_for_test());

        let root = tempfile::tempdir().unwrap();
        let mut app = crate::tui::app::AppState::new_for_test_with_db_path(
            crate::config::TonepoetConfig::default(), root.path().join("metadata.sqlite3"));
        assert!(app.browse_artwork.memory_only_preview_cache);
        assert!(app.browse_artwork.preview_cache.is_none(), "remain lazy");
        assert!(app.browse_artwork.preview_cache_handle().is_memory_only_for_test());
    }

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
            image: Ok(DecodedArtwork { image: Arc::new(DynamicImage::new_rgba8(10, 10)),
                embedded_path: None, preview_stamps: None, preview_cache_hit: false }),
            cache_epoch: None,
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
        artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(80, 60)));
        artwork.set_desired_area(Rect::new(0, 0, 30, 15), ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        let config = BrowseArtworkConfig::default();
        let background = Color::Rgb(26, 27, 38);

        assert!(artwork.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        assert!(artwork.take_background_restore(), "first native placement must repaint");
        let first = artwork.background_restore_count();
        let first_ink = background_repaint_ink(background, first);
        assert!(!artwork.take_background_restore(), "restoration is consumed once");
        assert!(!artwork.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)),
            "unchanged artwork must not rebuild on unrelated Browse redraws");
        assert!(!artwork.take_background_restore(),
            "unrelated redraws must not repaint thousands of blank cells");

        assert!(artwork.prepare(&mut picker, 0, 1, &config, Color::Rgb(0, 0, 0)),
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
    fn native_image_placement_gets_one_post_render_background_repair() {
        use ratatui::{backend::TestBackend, style::Style, widgets::Paragraph, Terminal};
        use ratatui_image::picker::ProtocolType;

        let background = Color::Rgb(26, 27, 38);
        let viewport = Rect::new(2, 2, 42, 24);
        let empty_cell = (59, 28); // Outside the image, inside the drawn screen.
        let config = BrowseArtworkConfig::default();

        // The popup is composited after Browse's background pass. The Info
        // preview is composited before it. Both need a post-image repair, for
        // each native protocol; Halfblocks must never request this redraw.
        for (protocol_type, popup) in [
            (ProtocolType::Sixel, false),
            (ProtocolType::Sixel, true),
            (ProtocolType::Kitty, false),
            (ProtocolType::Kitty, true),
            (ProtocolType::Iterm2, false),
            (ProtocolType::Iterm2, true),
        ] {
            let mut state = BrowseArtworkState::default();
            state.decoded = Some(Arc::new(DynamicImage::new_rgb8(40, 30)));
            state.popup_open = popup;
            // These pixels model a decode completed for the current viewer ground.
            state.viewer_background = popup.then_some([0, 0, 0]);
            let placement = if popup {
                ImageVerticalPlacement::Center
            } else {
                ImageVerticalPlacement::Bottom
            };
            state.set_desired_area(viewport, placement);
            let mut picker = Picker::new((8, 12));
            picker.protocol_type = protocol_type;
            assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
            assert!(state.native_placement_awaiting_render);
            // Preparing alone must not schedule a second render. The native
            // protocol might be suppressed by an overlay or zero-size area.
            assert!(!state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
            assert!(state.needs_background_restore);

            let mut terminal = Terminal::new(TestBackend::new(64, 32)).unwrap();
            terminal.draw(|frame| {
                let screen = frame.size();
                frame.render_widget(Paragraph::new("")
                    .style(Style::default().bg(background)), screen);
                if popup {
                    // Full-size images are drawn after Browse's repair.
                    assert!(state.take_background_restore());
                    repaint_image_background(frame, screen, background,
                        state.background_restore_count());
                }
                assert!(state.render_image(frame, viewport, config.allow_upscale,
                    placement, background));
                if !popup {
                    // Info previews are painted before Browse's repair.
                    assert!(state.take_background_restore());
                    repaint_image_background(frame, screen, background,
                        state.background_restore_count());
                }
            }).unwrap();
            let first = terminal.backend().buffer().clone();
            let origin = (state.protocol_area.x, state.protocol_area.y);
            assert!(state.post_native_repaint_redraw);
            assert!(!state.native_placement_awaiting_render);

            // The actual image command may have altered terminal pixels after
            // the first pass. The post-draw prepare must request another frame
            // without invalidating/re-encoding the image protocol.
            let prepared_before = state.protocol_area;
            assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
            assert_eq!(state.protocol_area, prepared_before);
            assert!(!state.post_native_repaint_redraw);
            assert!(state.needs_background_restore,
                "post-draw prepare must re-arm the repair even when the first draw consumed it");
            terminal.draw(|frame| {
                let screen = frame.size();
                frame.render_widget(Paragraph::new("")
                    .style(Style::default().bg(background)), screen);
                if popup {
                    // Full-size images are drawn after Browse's repair.
                    assert!(state.take_background_restore());
                    repaint_image_background(frame, screen, background,
                        state.background_restore_count());
                }
                assert!(state.render_image(frame, viewport, config.allow_upscale,
                    placement, background));
                if !popup {
                    // Info previews are painted before Browse's repair.
                    assert!(state.take_background_restore());
                    repaint_image_background(frame, screen, background,
                        state.background_restore_count());
                }
            }).unwrap();
            let second = terminal.backend().buffer().clone();
            let first_origin = first.get(origin.0, origin.1).symbol();
            let second_origin = second.get(origin.0, origin.1).symbol();
            if protocol_type == ProtocolType::Kitty {
                // In ratatui-image 1.0.5 the initial Kitty origin contains
                // an upload followed by colored Unicode placeholders. Later
                // frames keep only the placeholders. Equality would reject
                // the intended one-time upload, not a retransmission.
                assert!(first_origin.contains("\x1b_G"));
                assert!(!second_origin.contains("\x1b_G"),
                    "the follow-up must not upload Kitty image data again");
                assert!(first_origin.ends_with(second_origin),
                    "the follow-up must retain the original Kitty placeholders");
            } else {
                assert_eq!(first_origin, second_origin,
                    "the follow-up must not change the native image command");
            }
            assert_eq!(state.background_restore_count(), 2,
                "the first and post-render frame each need one real repair");
            assert_eq!(first.get(empty_cell.0, empty_cell.1).bg, background);
            assert_eq!(second.get(empty_cell.0, empty_cell.1).bg, background);
            assert_ne!(first.get(empty_cell.0, empty_cell.1).fg,
                second.get(empty_cell.0, empty_cell.1).fg);
            assert!(first.diff(&second).iter().any(|(x, y, _)|
                (*x, *y) == empty_cell),
                "post-image repair must force a terminal write to the blank region");
            assert!(!state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
            assert!(!state.take_background_restore(),
                "an unchanged image must not flood subsequent Browse redraws");
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new("")
                    .style(Style::default().bg(background)), frame.size());
                assert!(state.render_image(frame, viewport, config.allow_upscale,
                    placement, background));
                assert!(!state.take_background_restore());
            }).unwrap();
            assert_eq!(second_origin,
                terminal.backend().buffer().get(origin.0, origin.1).symbol(),
                "an ordinary third frame must not retransmit native graphics");
            assert_eq!(state.background_restore_count(), 2);
        }
    }

    #[test]
    fn post_render_background_repair_is_cancelled_when_image_closes() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/pictures/cover.png"));
        state.scanned = state.context.clone(); // No filesystem worker in this state test.
        state.viewer_background = Some([0, 0, 0]);
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(24, 24)));
        state.set_desired_area(Rect::new(2, 2, 20, 15), ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        let deadline=Instant::now()+std::time::Duration::from_secs(10);
        while !state.native_frame_ready()&&Instant::now()<deadline {state.prepare(&mut picker,0,0,&config,Color::Rgb(0,0,0));std::thread::sleep(std::time::Duration::from_millis(2));}
        assert!(state.native_frame_ready(),"native worker must complete");
        assert!(state.take_background_restore());
        let mut terminal = Terminal::new(TestBackend::new(40, 25)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, Rect::new(2, 2, 20, 15),
                config.allow_upscale, ImageVerticalPlacement::Center,
                Color::Rgb(26, 27, 38)));
        }).unwrap();
        assert!(state.post_native_repaint_redraw);
        state.close_popup();
        assert!(!state.post_native_repaint_redraw,
            "closing must not schedule another hidden-image render");
        assert!(!state.native_placement_awaiting_render);
        assert!(state.take_background_restore(),
            "closing still needs the ordinary one-shot background cleanup");
        assert!(!state.take_background_restore());
    }

    #[test]
    fn hidden_info_preview_cancels_pending_image_repair_but_cleans_old_pixels() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        state.observe(Some(ArtworkContext::Album {
            directory: PathBuf::from("/albums/old"), selected_audio: None,
        }));
        state.scanned = state.context.clone(); // Suppress filesystem workers.
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(32, 24)));
        let area = Rect::new(3, 2, 24, 16);
        state.set_desired_area(area, ImageVerticalPlacement::Bottom);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        let config = BrowseArtworkConfig::default();
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        let mut terminal = Terminal::new(TestBackend::new(40, 25)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Bottom, Color::Rgb(26, 27, 38)));
            assert!(state.take_background_restore());
        }).unwrap();
        assert!(state.post_native_repaint_redraw);

        // Layout changes can hide Info immediately after the image is drawn.
        // Its pending second image pass must be cancelled; removal still
        // requires one blank-background cleanup for the old terminal pixels.
        state.suspend();
        assert!(!state.post_native_repaint_redraw);
        assert!(!state.native_placement_awaiting_render);
        assert!(!state.has_image());
        assert!(state.take_background_restore());
        assert!(!state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        assert!(!state.take_background_restore());
    }

    #[test]
    fn new_info_selection_preserves_cleanup_and_schedules_its_own_repair() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        let area = Rect::new(2, 2, 24, 16);
        let config = BrowseArtworkConfig::default();
        state.observe(Some(ArtworkContext::Album {
            directory: PathBuf::from("/albums/one"), selected_audio: None,
        }));
        state.scanned = state.context.clone(); // Suppress filesystem workers.
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(32, 24)));
        state.set_desired_area(area, ImageVerticalPlacement::Bottom);
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        let mut terminal = Terminal::new(TestBackend::new(40, 25)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Bottom, Color::Rgb(26, 27, 38)));
            assert!(state.take_background_restore());
        }).unwrap();
        assert!(state.post_native_repaint_redraw);

        // Replacing artwork cancels the obsolete upload's second pass, but
        // keeps cleanup for the old pixels and prepares a fresh image.
        state.observe(Some(ArtworkContext::Album {
            directory: PathBuf::from("/albums/two"), selected_audio: None,
        }));
        state.scanned = state.context.clone();
        assert!(!state.post_native_repaint_redraw);
        assert!(state.needs_background_restore);
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(40, 30)));
        state.set_desired_area(area, ImageVerticalPlacement::Bottom);
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        terminal.draw(|frame| {
            assert!(state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Bottom, Color::Rgb(26, 27, 38)));
            assert!(state.take_background_restore());
        }).unwrap();
        assert!(state.post_native_repaint_redraw);
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        assert!(state.take_background_restore());
        assert!(!state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)),
            "unchanged replacement artwork must not schedule a third upload or repaint");
    }

    #[test]
    fn halfblocks_never_schedules_native_background_followup() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(24, 24)));
        let area = Rect::new(2, 2, 20, 15);
        state.set_desired_area(area, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Halfblocks;
        let config = BrowseArtworkConfig::default();
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        assert!(!state.native_placement_awaiting_render);
        assert!(!state.take_background_restore());
        let mut terminal = Terminal::new(TestBackend::new(40, 25)).unwrap();
        terminal.draw(|frame| {
            assert!(state.render_image(frame, area, config.allow_upscale,
                ImageVerticalPlacement::Center, Color::Rgb(26, 27, 38)));
        }).unwrap();
        assert!(!state.post_native_repaint_redraw);
        assert!(!state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        assert!(!state.take_background_restore());
    }

    #[test]
    fn native_artwork_menu_restoration_forces_one_image_origin_diff() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(24, 24)));
        state.popup_open = true;
        state.viewer_background = Some([0, 0, 0]);
        let viewport = Rect::new(3, 2, 45, 24);
        let background = Color::Rgb(26, 27, 38);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        assert!(state.prepare(&mut picker, 1, 0, &config, Color::Rgb(0, 0, 0)));
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
        // The worker received this ground when the popup was prepared.
        state.viewer_background = Some([0, 0, 0]);
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(160, 96)));
        let viewport = Rect::new(3, 2, 45, 24);
        let background = Color::Rgb(26, 27, 38);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
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
        state.decoded = Some(Arc::new(DynamicImage::new_rgb8(12, 12)));
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
        app.browse_artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(160, 96)));
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
        state.set_desired_area(Rect::new(0, 0, 70, 25), ImageVerticalPlacement::Center);
        assert!(state.is_single_file_popup());
        assert!(state.generation > album_generation);
        assert!(state.scanned.is_none(), "sibling listing is deferred to a worker");
        assert!(state.scan_rx.is_none(), "opening View must not scan synchronously");
        assert_eq!(state.sources, vec![ArtworkSource::File(image_path.clone())]);

        let mut picker = Picker::new((12, 24));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state.has_image() && Instant::now() < deadline {
            state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default(), Color::Rgb(0, 0, 0));
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
            image: Err("stale decode error".to_string()), cache_epoch: None,
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
    fn info_preview_uses_cache_but_inline_view_reloads_full_size_original() {
        let temp = tempfile::tempdir().unwrap();
        let picture = temp.path().join("cover.png");
        DynamicImage::new_rgba8(1300, 900).save(&picture).unwrap();
        let context = ArtworkContext::Album { directory: temp.path().to_path_buf(), selected_audio: None };
        let mut state = BrowseArtworkState::default();
        state.preview_cache = Some(Arc::new(InfoPreviewCache::at_path(temp.path().join("cache.sqlite3"))));
        state.observe(Some(context.clone()));
        state.scanned = Some(context);
        state.sources = vec![ArtworkSource::File(picture.clone())];
        let mut picker = Picker::new((8, 12));
        let mut load = |state: &mut BrowseArtworkState| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !state.has_image() && Instant::now() < deadline {
                state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default(), Color::Rgb(0, 0, 0));
                thread::sleep(Duration::from_millis(5));
            }
            assert!(state.has_image(), "decode worker did not complete");
        };
        load(&mut state);
        assert_eq!(state.decoded.as_ref().map(|image| image.width()), Some(PREVIEW_EDGE));
        assert!(state.preview_cache.is_some());
        state.open_info_popup();
        state.set_desired_area(Rect::new(0, 0, 170, 80), ImageVerticalPlacement::Center);
        assert!(state.decoded.is_none(), "viewer must not reuse a cached preview");
        load(&mut state);
        assert_eq!(state.decoded.as_ref().map(|image| image.width()), Some(1300));
        state.close_popup();
        load(&mut state);
        assert_eq!(state.decoded.as_ref().map(|image| image.width()), Some(PREVIEW_EDGE));
        assert!(picture.exists(), "original must remain untouched");
    }

    #[test]
    fn disabled_info_artwork_does_not_start_discovery_or_a_cache() {
        let mut app = crate::tui::app::AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.artwork_config.enabled = false;
        app.browse_artwork.observe(Some(ArtworkContext::Album {
            directory: PathBuf::from("/unvisited/album"), selected_audio: None,
        }));
        app.prepare_image_preview_protocols();
        assert!(app.browse_artwork.context.is_none());
        assert!(app.browse_artwork.preview_cache.is_none());
        assert!(app.browse_artwork.scan_rx.is_none());
        assert!(app.browse_artwork.decode_rx.is_none());
        let root = tempfile::tempdir().unwrap();
        app.browse_artwork.preview_cache = Some(Arc::new(InfoPreviewCache::at_path(root.path().join("cache.sqlite3"))));
        let (_generation, done) = app.browse_artwork.clear_cache();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    }

    #[test]
    fn clear_completion_identity_survives_browse_suspension() {
        let root = tempfile::tempdir().unwrap();
        let mut state = BrowseArtworkState::default();
        state.preview_cache = Some(Arc::new(InfoPreviewCache::at_path(
            root.path().join("previews.sqlite3"))));
        state.observe(Some(ArtworkContext::Album {
            directory: root.path().to_path_buf(), selected_audio: None,
        }));
        let (first_id, done) = state.clear_cache();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        state.suspend(); // Increments the preview epoch after the clear succeeds.
        assert_eq!(state.latest_clear_id(), Some(first_id));

        // A later clear is a new user command, not an ordinary invalidation.
        let (second_id, done) = state.clear_cache();
        assert!(second_id > first_id);
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        assert_eq!(state.latest_clear_id(), Some(second_id));
    }

    #[test]
    fn stale_preview_worker_after_clear_may_display_but_cannot_repopulate() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("cover.png");
        fs::write(&image_path, b"unchanged original").unwrap();
        let source = ArtworkSource::File(image_path);
        let mut state = BrowseArtworkState::default();
        state.sources = vec![source.clone()];
        let cache = Arc::new(InfoPreviewCache::at_path(temp.path().join("previews.sqlite3")));
        let old = cache.epoch();
        state.preview_cache = Some(Arc::clone(&cache));
        let (sender, receiver) = mpsc::channel();
        sender.send(DecodeResult { generation: state.generation, source: source.clone(),
            cache_epoch: Some(old), image: Ok(DecodedArtwork {
                image: Arc::new(DynamicImage::new_rgb8(32, 32)),
                embedded_path: None, preview_stamps: stamps_for_decoded(&source, None),
                preview_cache_hit: false,
            }) }).unwrap();
        state.decode_rx = Some(receiver);
        let (_new, clear_done) = state.clear_cache();
        assert!(state.poll_workers());
        assert!(state.decoded.is_some(), "clearing must not interfere with display");
        assert!(clear_done.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
        assert!(cache.lookup(&source, cache.epoch()).is_none());
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
        state.set_desired_area(Rect::new(0, 0, 70, 25), ImageVerticalPlacement::Center);
        assert_eq!(state.count(), 1, "the explicit file is available before a scan");
        let mut picker = Picker::new((12, 24));
        let deadline = Instant::now() + Duration::from_secs(5);
        while (state.count() != 3 || !state.has_image()) && Instant::now() < deadline {
            state.prepare(&mut picker, 0, 0, &BrowseArtworkConfig::default(), Color::Rgb(0, 0, 0));
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


    #[test]
    fn failed_explicit_view_keeps_filename_and_gallery_position() {
        let mut state = BrowseArtworkState::default();
        let first = ArtworkSource::File(PathBuf::from("/images/large.png"));
        let second = ArtworkSource::File(PathBuf::from("/images/small.png"));
        state.open_file_popup(PathBuf::from("/images/large.png"));
        let generation = state.generation;
        let (tx, rx) = mpsc::channel();
        state.decode_rx = Some(rx);
        tx.send(DecodeResult { generation, source: first.clone(),
            image: Err("decoded-memory budget exceeded".into()), cache_epoch: None }).unwrap();
        assert!(state.poll_workers());
        assert_eq!(state.current_label().as_deref(), Some("large.png"));
        assert!(state.message().contains("large.png"));
        assert!(state.message().contains("decoded-memory budget exceeded"));
        assert!(!state.has_image());
        // A delayed sibling scan may change the cursor index but must not
        // discard the failure or silently present the successful sibling.
        let (scan_tx, scan_rx) = mpsc::channel();
        state.scan_rx = Some(scan_rx);
        scan_tx.send(ScanResult { context: ArtworkContext::SingleFile(
            PathBuf::from("/images/large.png")), sources: vec![second.clone(), first.clone()],
            warning: None, generation }).unwrap();
        assert!(state.poll_workers());
        assert_eq!(state.current_label().as_deref(), Some("large.png"));
        assert!(state.message().contains("large.png"));
        assert!(state.cycle(false));
        assert_eq!(state.current_label().as_deref(), Some("small.png"));
        assert!(state.cycle(true));
        assert_eq!(state.current_label().as_deref(), Some("large.png"));
        assert!(state.message().contains("large.png"));
    }

    #[test]
    fn info_skips_missing_embedded_art_and_keeps_usable_folder_cover() {
        let mut state = BrowseArtworkState::default();
        let embedded = ArtworkSource::Embedded(Arc::from([
            PathBuf::from("/music/album/no-cover.flac")
        ]));
        let cover = ArtworkSource::File(PathBuf::from("/music/album/cover.png"));
        state.context = Some(ArtworkContext::Album {
            directory: PathBuf::from("/music/album"), selected_audio: None,
        });
        state.scanned = state.context.clone();
        state.sources = vec![embedded.clone(), cover.clone()];
        state.record_decode_failure(&embedded, "No usable embedded artwork in album".into());
        assert_eq!(state.cursor, 1);
        assert_eq!(state.sources[state.cursor], cover);
        assert!(!state.popup_open);
        assert_eq!(state.message(), "Loading artwork…");
    }

    #[test]
    fn viewer_accepts_over_24_mib_up_to_100_mib_and_rejects_oversize() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("scan.png");
        let handle = fs::File::create(&file).unwrap();
        handle.set_len(25 * 1024 * 1024).unwrap();
        let error = decode_source_with_bounds(&ArtworkSource::File(file.clone()), 800, 600)
            .err().expect("an empty sparse file has no valid image format");
        assert!(!error.contains("encoded-size limit"), "25 MiB passes byte admission");
        handle.set_len(MAX_ENCODED_IMAGE_BYTES + 1).unwrap();
        let error = decode_source_with_bounds(&ArtworkSource::File(file), 800, 600)
            .err().expect("oversized file is rejected");
        assert!(error.contains("100 MiB"));
    }

    #[test]
    fn decode_binds_large_original_to_requested_viewport() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("page.png");
        DynamicImage::new_rgb8(2000, 1000).save(&path).unwrap();
        let result = decode_source_with_bounds(&ArtworkSource::File(path), 1200, 720).unwrap();
        assert_eq!((result.image.width(), result.image.height()), (1200, 600));
    }


    #[test]
    fn failed_viewer_wraps_filename_and_reason_on_eighty_and_forty_columns() {
        use ratatui::{backend::TestBackend, Terminal};

        for columns in [80_u16, 40_u16] {
            let mut app = super::super::app::AppState::new_for_test(
                crate::config::TonepoetConfig::default());
            let file = PathBuf::from("/images/scan_100mb.png");
            let source = ArtworkSource::File(file.clone());
            app.browse_artwork.open_file_popup(file);
            app.browse_artwork.record_decode_failure(&source,
                "Source encoded-size limit exceeded after reading image headers; \
                 maximum supported image is 100 MiB; please choose a smaller scan".into());
            let theme = app.theme;
            let mut terminal = Terminal::new(TestBackend::new(columns, 24)).unwrap();
            terminal.draw(|frame| draw_popup(frame, &mut app, theme)).unwrap();
            let status = app.browse_artwork.popup_rect;
            assert!(status.height > 1 && status.width <= columns,
                "failure needs a bounded multiline area at {columns} columns");
            let buffer = terminal.backend().buffer();
            let display = (0..24_u16).map(|y| (0..columns)
                .map(|x| buffer.get(x, y).symbol().to_owned())
                .collect::<Vec<_>>().join("")).collect::<Vec<_>>().join("\n");
            assert!(display.contains("scan_100mb.png"), "filename hidden at {columns} columns");
            assert!(display.contains("100 MiB"), "size-limit explanation hidden at {columns} columns");
            assert!(display.contains("Cannot display"), "failure marker hidden at {columns} columns");
        }
    }

    #[test]
    fn fullscreen_round_trip_preserves_terminal_sized_decoded_pixels() {
        use ratatui_image::picker::ProtocolType;
        let mut state=BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/image/cover.png"));
        state.scanned=state.context.clone();
        state.viewer_max_area=Rect::new(0,0,100,40);
        state.viewer_decode_bounds=Some((800,480));
        state.viewer_background=Some([0,0,0]);
        let pixels=Arc::new(DynamicImage::new_rgb8(800,480));
        state.decoded=Some(pixels.clone());
        let generation=state.generation;
        let mut picker=Picker::new((8,12)); picker.protocol_type=ProtocolType::Halfblocks;
        for fullscreen in [false,true,false,true] {
            if state.fullscreen!=fullscreen {state.toggle_fullscreen();}
            let area=if fullscreen {Rect::new(0,0,100,40)} else {Rect::new(0,0,100,20)};
            state.set_desired_area(area,ImageVerticalPlacement::Center);
            state.prepare(&mut picker,0,0,&BrowseArtworkConfig::default(),Color::Rgb(0,0,0));
            assert_eq!(state.generation,generation);
            assert!(Arc::ptr_eq(state.decoded.as_ref().unwrap(),&pixels));
            assert!(state.decode_rx.is_none());
        }
    }

    #[test]
    fn first_fullscreen_transition_reuses_prewarmed_fixed_origin_frame() {
        use ratatui_image::picker::ProtocolType;
        let mut state = BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/image/cover.png"));
        state.scanned = state.context.clone();
        state.viewer_max_area = Rect::new(0, 0, 60, 24);
        state.viewer_decode_bounds = Some((480, 288));
        state.viewer_background = Some([0, 0, 0]);
        let pixels = Arc::new(DynamicImage::new_rgb8(480, 288));
        state.decoded = Some(Arc::clone(&pixels));
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Sixel;
        let config = BrowseArtworkConfig::default();
        let modal = popup_rect(state.viewer_max_area, config.popup_height_percent);
        state.set_desired_area(modal, ImageVerticalPlacement::Center);
        let background = Color::Rgb(0, 0, 0);
        state.prepare(&mut picker, 0, 0, &config, background);
        assert!(state.encode_rx.is_some());
        assert!(state.warm_request.is_some(), "full-screen encoding must start before f");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !(state.native_frame_ready() && state.alternate_frames.iter().any(|(id, _)| id.area == state.viewer_max_area))
            && Instant::now() < deadline {
            state.prepare(&mut picker, 0, 0, &config, background);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(state.native_frame_ready());
        assert!(state.alternate_frames.iter().any(|(id, _)| id.area == state.viewer_max_area));
        state.toggle_fullscreen();
        state.set_desired_area(state.viewer_max_area, ImageVerticalPlacement::Center);
        state.prepare(&mut picker, 0, 0, &config, background);
        assert!(state.native_frame_ready(), "first f must not launch another encode");
        assert!(state.encode_request.is_none());
        assert!(state.warm_request.is_none());
        assert!(Arc::ptr_eq(state.decoded.as_ref().unwrap(), &pixels));
    }

    #[test]
    fn immediate_first_fullscreen_transition_joins_pending_alternate_encode() {
        use ratatui::{backend::TestBackend,Terminal};
        use ratatui_image::picker::ProtocolType;
        let mut state=BrowseArtworkState::default();
        state.open_file_popup(PathBuf::from("/image/cover.png"));
        state.scanned=state.context.clone();
        state.viewer_max_area=Rect::new(0,0,60,24);
        state.viewer_decode_bounds=Some((480,288));
        state.viewer_background=Some([0,0,0]);
        let pixels=Arc::new(DynamicImage::new_rgb8(480,288));
        state.decoded=Some(Arc::clone(&pixels));
        let mut picker=Picker::new((8,12));picker.protocol_type=ProtocolType::Sixel;
        let config=BrowseArtworkConfig::default();let background=Color::Rgb(0,0,0);
        let full=state.viewer_max_area;let modal=popup_rect(full,config.popup_height_percent);
        state.set_desired_area(modal,ImageVerticalPlacement::Center);
        state.prepare(&mut picker,0,0,&config,background);
        // Retain the real alternate worker, but gate delivery to make the
        // early-f ordering deterministic even on very fast machines.
        let actual=state.warm_rx.take().expect("opening starts alternate encode");
        let requested=state.warm_request.clone().expect("alternate identity");
        assert_eq!(requested.area,full);
        let (release,pending)=mpsc::channel();state.warm_rx=Some(pending);
        let deadline=Instant::now()+Duration::from_secs(15);
        while !state.native_frame_ready()&&Instant::now()<deadline {
            state.prepare(&mut picker,0,0,&config,background);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(state.native_frame_ready(),"window must become visible before f");
        let mut terminal=Terminal::new(TestBackend::new(60,24)).unwrap();
        terminal.draw(|f|assert!(state.render_image(f,modal,config.allow_upscale,ImageVerticalPlacement::Center,background))).unwrap();
        let generation=state.generation;
        state.toggle_fullscreen();state.set_desired_area(full,ImageVerticalPlacement::Center);
        for _ in 0..3 {
            state.prepare(&mut picker,0,0,&config,background);
            assert!(state.fullscreen);assert_eq!(state.protocol_area,full);
            assert!(state.warm_request.as_ref()==Some(&requested));
            assert!(state.encode_rx.is_none()&&state.encode_request.is_none(),"early f must join the already-started encode");
            assert!(state.decode_rx.is_none());assert_eq!(state.generation,generation);
            assert!(Arc::ptr_eq(state.decoded.as_ref().unwrap(),&pixels));
        }
        // Deliver the real encoded frame, not a synthetic replacement.
        let frame=actual.recv_timeout(Duration::from_secs(15)).expect("alternate codec result");
        assert_eq!(frame.key.area,full);assert!(release.send(frame).is_ok());
        state.prepare(&mut picker,0,0,&config,background);
        assert!(state.native_frame_ready());assert!(state.warm_request.is_none());
        assert!(state.encode_rx.is_none()&&state.encode_request.is_none());
        assert_eq!(state.encoded_frame.as_ref().unwrap().key,state.encoded_frame_key());
        terminal.draw(|f|assert!(state.render_image(f,full,false,ImageVerticalPlacement::Center,background))).unwrap();
        // The completed windowed frame is still available for a return.
        state.toggle_fullscreen();state.set_desired_area(modal,ImageVerticalPlacement::Center);
        state.prepare(&mut picker,0,0,&config,background);
        assert!(state.native_frame_ready());assert!(state.encode_rx.is_none());
        terminal.draw(|f|assert!(state.render_image(f,modal,config.allow_upscale,ImageVerticalPlacement::Center,background))).unwrap();
    }

    #[test]
    fn fullscreen_native_workers_reuse_pixels_and_render_matching_geometry() {
        use ratatui::{backend::TestBackend,Terminal};use ratatui_image::picker::ProtocolType;
        for protocol in [ProtocolType::Sixel,ProtocolType::Kitty]{let mut state=BrowseArtworkState::default();state.open_file_popup(PathBuf::from("/image/cover.png"));state.scanned=state.context.clone();state.viewer_max_area=Rect::new(0,0,60,24);state.viewer_decode_bounds=Some((480,288));state.viewer_background=Some([0,0,0]);let pixels=Arc::new(DynamicImage::new_rgb8(480,288));state.decoded=Some(pixels.clone());let mut picker=Picker::new((8,12));picker.protocol_type=protocol;let config=BrowseArtworkConfig::default();let mut terminal=Terminal::new(TestBackend::new(60,24)).unwrap();let background=Color::Rgb(0,0,0);for fullscreen in [false,true,false,true]{if state.fullscreen!=fullscreen{state.toggle_fullscreen();}let area=if fullscreen{Rect::new(0,0,60,24)}else{Rect::new(0,0,60,12)};state.set_desired_area(area,ImageVerticalPlacement::Center);state.prepare(&mut picker,0,0,&config,background);let deadline=Instant::now()+Duration::from_secs(10);while !state.native_frame_ready()&&Instant::now()<deadline{state.prepare(&mut picker,0,0,&config,background);std::thread::sleep(Duration::from_millis(2));}assert!(state.native_frame_ready());assert!(Arc::ptr_eq(state.decoded.as_ref().unwrap(),&pixels));terminal.draw(|frame|{assert!(state.render_image(frame,area,config.allow_upscale,ImageVerticalPlacement::Center,background));}).unwrap();}}
    }

    #[test]
    fn fullscreen_uses_whole_terminal_and_never_enlarges_small_images() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut app = crate::tui::app::AppState::new_for_test(
            crate::config::TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.artwork_config.allow_upscale = true;
        app.browse_artwork.open_file_popup(PathBuf::from("/image/front.png"));
        app.browse_artwork.scanned = app.browse_artwork.context.clone();
        let (r, g, b) = crate::tui::theme::rgb_tuple(app.theme.bg);
        app.browse_artwork.viewer_background = Some([r, g, b]);
        app.browse_artwork.decoded = Some(Arc::new(DynamicImage::new_rgb8(24, 24)));
        app.browse_artwork.enter_fullscreen();
        app.image_picker = Picker::new((8, 12));
        app.image_picker.protocol_type = ProtocolType::Halfblocks;
        let screen = Rect::new(0, 0, 100, 40);
        app.browse_artwork.set_desired_area(screen, ImageVerticalPlacement::Center);
        assert!(app.prepare_image_preview_protocols());
        assert_eq!(app.browse_artwork.protocol_area, Rect::new(48, 19, 3, 2),
            "source pixels cap the fit even when normal View allows enlargement");

        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| frame.render_widget(
            ratatui::widgets::Paragraph::new("BROWSE_PREVIOUS_FRAME"),
            Rect::new(0, 0, 30, 1),
        )).unwrap();
        terminal.draw(|frame| crate::tui::draw::draw_ui(frame, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer().get(0, 0).bg, app.theme.bg,
            "letterboxing must have the application's background colour");
        assert_eq!(app.browse_artwork.popup_rect, screen,
            "full-screen pointer input includes any letterboxing");
        assert_eq!(app.browse_artwork.desired_area, screen,
            "full-screen must not reuse the configured popup-height viewport");
        let buf = terminal.backend().buffer();
        let text = (0..screen.height).flat_map(|y| (0..screen.width)
            .map(move |x| buf.get(x, y).symbol().to_owned()))
            .collect::<Vec<_>>().join("");
        assert!(!text.contains("browse") && !text.contains("tonepoet")
            && !text.contains("BROWSE_PREVIOUS_FRAME"),
            "the normal Browse chrome and prior frame must not appear behind the image");
        app.browse_artwork.close_popup();
        assert!(!app.browse_artwork.fullscreen);
    }

    #[test]
    fn kitty_image_upload_is_compressed_once_and_not_resent_on_redraw() {
        use ratatui::{backend::TestBackend, Terminal};
        use ratatui_image::picker::ProtocolType;

        let mut state = BrowseArtworkState::default();
        // A synthetic archival scan with nearby pixels correlated enough to
        // benefit from zlib, but larger than a thumbnail.
        let image = image::RgbImage::from_fn(1024, 768, |x, y| {
            image::Rgb([((x / 8) % 256) as u8, ((y / 8) % 256) as u8,
                (((x / 16) + (y / 16)) % 256) as u8])
        });
        state.decoded = Some(Arc::new(DynamicImage::ImageRgb8(image)));
        let viewport = Rect::new(2, 2, 70, 25);
        state.set_desired_area(viewport, ImageVerticalPlacement::Center);
        let mut picker = Picker::new((8, 12));
        picker.protocol_type = ProtocolType::Kitty;
        let config = BrowseArtworkConfig::default();
        assert!(state.prepare(&mut picker, 0, 0, &config, Color::Rgb(0, 0, 0)));
        let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
        let draw = |frame: &mut ratatui::Frame, artwork: &mut BrowseArtworkState| {
            assert!(artwork.render_image(frame, viewport, config.allow_upscale,
                ImageVerticalPlacement::Center, Color::Rgb(26, 27, 38)));
        };
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let first_origin = terminal.backend().buffer()
            .get(state.protocol_area.x, state.protocol_area.y).symbol().to_owned();
        assert!(first_origin.contains("\x1b_G"), "first frame must transmit Kitty image");
        assert!(first_origin.contains("o=z"), "Kitty upload should use zlib");
        // Compare actual transmitted bytes against the original uncompressed
        // payload at the *fitted* size, not the larger source image dimensions.
        let header = first_origin.split("\x1b_G").nth(1).unwrap()
            .split(';').next().unwrap();
        let header_number = |key: &str| -> usize {
            header.split(',').find_map(|field| field.split_once('=')
                .filter(|(name, _)| *name == key)
                .and_then(|(_, value)| value.parse::<usize>().ok())).unwrap()
        };
        let channels = if header_number("f") == 32 { 4 } else { 3 };
        let raw_bytes = header_number("s") * header_number("v") * channels;
        let uncompressed_wire_bytes = raw_bytes.div_ceil(3) * 4;
        eprintln!("Kitty actual upload {} bytes vs {} uncompressed Base64 bytes",
            first_origin.len(), uncompressed_wire_bytes);
        assert!(first_origin.len() < uncompressed_wire_bytes / 2,
            "representative scan should save at least half of its RGB upload");
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let second_origin = terminal.backend().buffer()
            .get(state.protocol_area.x, state.protocol_area.y).symbol().to_owned();
        assert!(!second_origin.contains("\x1b_G"), "ordinary redraw cannot retransmit RGB");
        assert!(first_origin.ends_with(&second_origin),
            "compression must not change cached Kitty placeholders");
    }

    #[test]
    fn valid_png_above_former_24_mib_limit_decodes_to_viewport() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archival_scan.png");
        // Deterministic high-entropy pixels, so the *actual valid PNG* is
        // larger than 24 MiB, rather than a sparse invalid image or metadata
        // padding masquerading as a high-resolution scan.
        let pixels = image::RgbImage::from_fn(3072, 3072, |x, y| {
            let mut n = ((x as u64) << 32) | y as u64;
            n ^= n >> 33;
            n = n.wrapping_mul(0xff51afd7ed558ccd);
            n ^= n >> 33;
            n = n.wrapping_mul(0xc4ceb9fe1a85ec53);
            n ^= n >> 33;
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        });
        pixels.save(&path).unwrap();
        let encoded_size = fs::metadata(&path).unwrap().len();
        assert!(encoded_size > 24 * 1024 * 1024,
            "PNG must exceed old limit, but was {encoded_size} bytes");
        assert!(encoded_size < MAX_ENCODED_IMAGE_BYTES);
        let decoded = decode_source_with_bounds(&ArtworkSource::File(path), 800, 600).unwrap();
        assert_eq!((decoded.image.width(), decoded.image.height()), (600, 600));
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

/// Failure text is kept separate from the one-line loading indicator.
/// The error gets a bounded, centered text area that can wrap on narrow TTYs.
fn popup_status_area(viewport: Rect, status: &str, failed: bool) -> Rect {
    let longest = status.lines().map(super::display_width::width).max().unwrap_or(1);
    let width = if failed { viewport.width } else {
        longest.min(viewport.width as usize).max(1) as u16
    };
    let height = if failed {
        let columns = usize::from(width.max(1));
        // Word wrapping can use more rows than character-count / columns:
        // leave a couple of rows for word boundaries, within the viewport.
        status.lines().map(|line| {
            super::display_width::width(line).max(1).div_ceil(columns)
        }).sum::<usize>().saturating_add(2)
            .min(viewport.height as usize).max(1) as u16
    } else { 1 };
    Rect::new(viewport.x + (viewport.width - width) / 2,
              viewport.y + (viewport.height - height) / 2, width, height)
}

/// A centered modal image view, rendered after the ordinary screen. The
/// configurable height controls the maximum image size, not a blank panel.
/// The same image protocol cache is used for inline and popup views.
pub fn draw_popup(f: &mut ratatui::Frame, app: &mut super::app::AppState,
                  theme: super::theme::Theme) {
    use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
    use ratatui::{layout::Alignment, style::Style};

    let terminal = f.size();
    app.browse_artwork.viewer_max_area = terminal;
    let fullscreen = app.browse_artwork.fullscreen;
    if terminal.width == 0 || terminal.height == 0
        || (!fullscreen && (terminal.width < 8 || terminal.height < 5))
    {
        app.browse_artwork.popup_rect = Rect::default();
        return;
    }
    let viewport = if fullscreen { terminal } else {
        popup_rect(terminal, app.browse.artwork_config.popup_height_percent)
    };
    let allow_upscale = !fullscreen && app.browse.artwork_config.allow_upscale;
    let source_failed = app.browse_artwork.sources.get(app.browse_artwork.cursor)
        .is_some_and(|source| app.browse_artwork.failed.contains_key(source));
    let transport_failed = app.tmux_image_transport_failed();
    let failure = source_failed || transport_failed;
    let message = if source_failed {
        app.browse_artwork.message()
    } else if transport_failed {
        "Cannot display native image: tmux graphics transport failed"
    } else {
        let message = app.browse_artwork.message();
        if message.is_empty() { "Loading image…" } else { message }
    };
    // A line break makes the reason easy to find when the selected filename
    // spans multiple columns, and keeps the existing message() API unchanged.
    let status = if failure { message.replacen(": ", ":\n", 1) } else { message.to_owned() };
    let rect = if transport_failed {
        popup_status_area(viewport, &status, true)
    } else {
        app.browse_artwork.popup_image_area(viewport,
            allow_upscale).unwrap_or_else(|| popup_status_area(viewport, &status, failure))
    };
    // In full-screen mode, the entire terminal owns pointer input, including
    // letterboxing around an image smaller than the display.
    app.browse_artwork.popup_rect = if fullscreen { terminal } else { rect };
    if let super::app::ActiveOverlay::ArtworkSettings(settings) = &mut app.active_overlay {
        settings.update_anchor(app.browse_artwork.popup_rect, terminal);
        // Record current dimensions even when settings hides the image. A
        // height edit or terminal resize must not leave the old viewport as
        // the next encoding request after dismissal.
        app.browse_artwork.set_desired_area(viewport, ImageVerticalPlacement::Center);
        return;
    }
    if matches!(app.active_overlay, super::app::ActiveOverlay::EditImage(_)) {
        app.browse_artwork.set_desired_area(viewport, ImageVerticalPlacement::Center);
        return;
    }
    f.render_widget(Clear, rect);
    // No border, label, caption row, or panel outside the image. The backing
    // color matches the application rather than the old oversized panel.
    f.render_widget(Block::default()
        .style(Style::default().bg(theme.bg)), rect);
    let displayed = !transport_failed && app.browse_artwork.render_image(f, viewport,
        allow_upscale, ImageVerticalPlacement::Center, theme.bg);
    if !displayed {
        f.render_widget(Paragraph::new(status).style(theme.muted())
            .alignment(Alignment::Center).wrap(Wrap { trim: false }), rect);
    }

}

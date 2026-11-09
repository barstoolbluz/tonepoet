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
use ratatui::layout::Rect;
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
pub struct ArtworkContext {
    pub directory: PathBuf,
    pub selected_audio: Option<PathBuf>,
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
            return Some(Self { directory: entry.path.clone(), selected_audio: None });
        }
        if entry.is_audio() && entry.is_regular_file {
            return Some(Self {
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
    failed: HashSet<usize>,
    decode_rx: Option<Receiver<DecodeResult>>,
    decoded_source: Option<ArtworkSource>,
    decoded_embedded_path: Option<PathBuf>,
    decoded: Option<DynamicImage>,
    protocol: Option<Box<dyn StatefulProtocol>>,
    desired_area: Rect,
    protocol_area: Rect,
    protocol_generation: usize,
    retransmit_generation: usize,
    font_size: (u16, u16),
    warning: Option<String>,
    pub popup_open: bool,
    pub popup_rect: Rect,
}

impl Default for BrowseArtworkState {
    fn default() -> Self {
        Self {
            context: None, generation: 0, scan_generation: 0, decode_generation: 0, changed_at: Instant::now(), scanned: None,
            scan_rx: None, sources: Vec::new(), cursor: 0, failed: HashSet::new(),
            decode_rx: None, decoded_source: None, decoded_embedded_path: None, decoded: None,
            protocol: None, desired_area: Rect::default(), protocol_area: Rect::default(),
            protocol_generation: 0, retransmit_generation: 0, font_size: (8, 12),
            warning: None, popup_open: false, popup_rect: Rect::default(),
        }
    }
}

impl BrowseArtworkState {
    pub fn observe(&mut self, context: Option<ArtworkContext>) {
        if self.context == context { return; }
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
        self.failed.clear();
        self.decoded_source = None;
        self.decoded_embedded_path = None;
        self.decoded = None;
        self.protocol = None;
        self.protocol_area = Rect::default();
    }

    /// User-initiated reload or discovery-policy change. Keep in-flight work
    /// owned, but reject its result if the policy has changed in the meantime.
    pub fn invalidate(&mut self) {
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
        if self.context.is_some() {
            self.observe(None);
        }
    }

    pub fn invalidate_protocol(&mut self) {
        self.protocol = None;
        self.protocol_area = Rect::default();
    }

    pub fn set_desired_area(&mut self, area: Rect) {
        self.desired_area = area;
    }

    pub fn has_image(&self) -> bool { self.decoded.is_some() }
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
        if self.scan_rx.is_some() || self.scanned.is_none() { return "Finding artwork…"; }
        if self.decode_rx.is_some() || !self.sources.is_empty() && self.failed.len() < self.sources.len() {
            return "Loading artwork…";
        }
        self.warning.as_deref().unwrap_or("No artwork in album folder")
    }

    pub fn cycle(&mut self, forward: bool) -> bool {
        if self.sources.len() < 2 { return false; }
        let len = self.sources.len();
        for step in 1..len {
            let next = if forward { (self.cursor + step) % len }
                else { (self.cursor + len - (step % len)) % len };
            if !self.failed.contains(&next) {
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
        self.popup_open = false;
        self.popup_rect = Rect::default();
        self.protocol = None; // rebuild for the pane on the next prepare pass
        self.protocol_area = Rect::default();
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
        let desired = self.decoded.as_ref().map_or(self.desired_area, |image| {
            fitted_image_area(self.desired_area, image.width(), image.height(), font_size,
                config.allow_upscale)
        });
        let kitty = if picker.protocol_type == ratatui_image::picker::ProtocolType::Kitty {
            kitty_generation
        } else { 0 };
        if desired.width > 0 && desired.height > 0 && self.decoded.is_some()
            && (self.protocol.is_none() || self.protocol_area != desired
                || self.protocol_generation != picker_generation || self.retransmit_generation != kitty)
        {
            self.protocol = self.decoded.as_ref().map(|image| picker.new_resize_protocol(image.clone()));
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
                        self.scanned = Some(result.context);
                        self.sources = result.sources;
                        self.cursor = 0;
                        self.failed.clear();
                        self.decoded = None;
                        self.decoded_source = None;
                        self.decoded_embedded_path = None;
                        self.protocol = None;
                        self.warning = result.warning;
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => self.scan_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    // A worker panic must never lead to an automatic retry loop.
                    if self.scan_generation == self.generation {
                        self.scanned = self.context.clone();
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
                        allow_upscale: bool) -> bool {
        self.set_desired_area(area);
        if self.decoded.is_none() || self.protocol.is_none() { return false; }
        let image = self.decoded.as_ref().expect("checked");
        let target = fitted_image_area(area, image.width(), image.height(), self.font_size,
            allow_upscale);
        if target.width == 0 || target.height == 0 || self.protocol_area != target { return false; }
        let widget = ratatui_image::StatefulImage::new(None);
        f.render_stateful_widget(widget, target,
            self.protocol.as_mut().expect("checked"));
        true
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
                     font_size: (u16, u16), allow_upscale: bool) -> Rect {
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
    Rect::new(area.x.saturating_add((area.width - width) / 2),
              area.y.saturating_add((area.height - height) / 2), width, height)
}

pub fn fitted_without_upscale(area: Rect, pixel_width: u32, pixel_height: u32,
                              font_size: (u16, u16)) -> Rect {
    fitted_image_area(area, pixel_width, pixel_height, font_size, false)
}

fn is_supported_image(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
    })
}

/// Read only direct entries. A supported image-name symlink is eligible only
/// when its target resolves to an ordinary file. Never traverse linked folders.
fn scan_directory(context: ArtworkContext, config: &BrowseArtworkConfig) -> ScanResult {
    let mut files = Vec::new();
    let mut audio = Vec::new();
    let mut warning = None;
    if !fs::symlink_metadata(&context.directory).is_ok_and(|md| md.is_dir()) {
        return ScanResult {
            context, sources: Vec::new(), warning: Some("Album folder is unavailable".into()),
            generation: 0,
        };
    }
    match fs::read_dir(&context.directory) {
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
    if let Some(selected) = &context.selected_audio {
        if let Some(position) = audio.iter().position(|path| path == selected) {
            let path = audio.remove(position);
            audio.insert(0, path);
        } else if selected.parent() == Some(context.directory.as_path())
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
        assert_eq!(fitted_image_area(area, 24, 24, (8, 12), true),
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
        let ctx = ArtworkContext { directory: root.to_path_buf(), selected_audio: None };
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
        let result = scan_directory(ArtworkContext {
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
        let result = scan_directory(ArtworkContext { directory: tmp.path().into(), selected_audio: None },
                                    &BrowseArtworkConfig::default());
        assert_eq!(result.sources.len(), MAX_FOLDER_IMAGES);
        assert_eq!(result.sources[0].label(), "cover.png");
        assert!(result.warning.is_some());
    }

    #[test]
    fn stale_generation_is_rejected_after_policy_change() {
        let mut state = BrowseArtworkState::default();
        let tmp = tempfile::tempdir().unwrap();
        let context = ArtworkContext { directory: tmp.path().into(), selected_audio: None };
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
    fn embedded_art_is_found_after_first_track_without_a_picture() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("01 Intro.flac");
        let second = tmp.path().join("02 Song.flac");
        fs::write(&first, include_bytes!("../../tests/fixtures/silence.flac")).unwrap();
        fs::write(&second, include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac")).unwrap();
        image::DynamicImage::new_rgb8(8, 6).save(tmp.path().join("cover.jpg")).unwrap();
        let sources = scan_directory(ArtworkContext {
            directory: tmp.path().into(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(sources.len(), 2); // One logical embedded image and one folder cover.
        assert!(matches!(&sources[0], ArtworkSource::Embedded(_)));
        let decoded = decode_source(&sources[0]).expect("later track has valid embedded art");
        assert_eq!(decoded.embedded_path.as_deref(), Some(second.as_path()));
        assert_eq!((decoded.image.width(), decoded.image.height()), (12, 16));
        assert_eq!(sources[1].label(), "cover.jpg");

        // A selected track that lacks artwork still searches the other tracks.
        let selected = scan_directory(ArtworkContext {
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
        let sources = scan_directory(ArtworkContext {
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
        let sources = scan_directory(ArtworkContext {
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
        let sources = scan_directory(ArtworkContext {
            directory: tmp.path().into(), selected_audio: None,
        }, &BrowseArtworkConfig::default()).sources;
        assert_eq!(sources.len(), 2);
        assert!(decode_source(&sources[0]).is_err());
        assert_eq!(sources[1].label(), "cover.jpg");
        let decoded = decode_source(&sources[1]).expect("fallback cover must be usable");
        assert_eq!((decoded.image.width(), decoded.image.height()), (7, 9));
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
        let sources = scan_directory(ArtworkContext {
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

/// A popup rectangle is always a subset of the terminal (including narrow
/// terminals and the maximum 100% height option).
pub fn popup_rect(terminal: Rect, percent: u8) -> Rect {
    let height = percent_height(terminal.height, percent).max(5).min(terminal.height);
    let width = ((u32::from(terminal.width) * 80 + 50) / 100)
        .max(20).min(u32::from(terminal.width)) as u16;
    Rect::new(terminal.x.saturating_add((terminal.width - width) / 2),
              terminal.y.saturating_add((terminal.height - height) / 2), width, height)
}

/// A centered modal image view, rendered after the ordinary screen. The
/// artwork occupies the configurable fraction of the full terminal height;
/// the same image protocol cache is used for the inline and popup views.
pub fn draw_popup(f: &mut ratatui::Frame, app: &mut super::app::AppState,
                  theme: super::theme::Theme) {
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    use ratatui::style::Style;

    let terminal = f.size();
    if terminal.width < 8 || terminal.height < 5 { return; }
    let rect = popup_rect(terminal, app.browse.artwork_config.popup_height_percent);
    app.browse_artwork.popup_rect = rect;
    f.render_widget(Clear, rect);
    f.render_widget(Block::default().borders(Borders::ALL)
        .title(" Album artwork · Esc to close ")
        .style(Style::default().fg(theme.text).bg(theme.panel_bg))
        .border_style(theme.border(theme.amber)), rect);
    let image_area = Rect::new(rect.x + 1, rect.y + 1,
        rect.width.saturating_sub(2), rect.height.saturating_sub(3));
    let displayed = app.browse_artwork.render_image(f, image_area,
        app.browse.artwork_config.allow_upscale);
    if !displayed {
        let caption = app.browse_artwork.message();
        f.render_widget(Paragraph::new(caption).style(theme.muted()), image_area);
    }
    let label = app.browse_artwork.current_label().unwrap_or_default();
    let caption = format!(" {}  ({}/{}) · ←/→ or click: next ",
        label, app.browse_artwork.index().saturating_add(1), app.browse_artwork.count());
    let caption = super::display_width::fit_prefix(&caption, rect.width.saturating_sub(2) as usize);
    f.render_widget(Paragraph::new(caption).style(theme.muted()),
        Rect::new(rect.x + 1, rect.bottom() - 2, rect.width.saturating_sub(2), 1));
}

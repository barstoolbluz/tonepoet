//! libbluray-sys backend adapter for Blu-ray Phase 0.
//!
//! This file is the only place Phase 0 should import libbluray-sys.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::{self, Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::time::Instant;

use libbluray_sys as ffi;

use super::bluray_backend::{
    bluray_source_stem, BluRayAudioCoding, BluRayAudioStreamKind, BlurayAudioStreamInfo,
    BlurayBackend, BlurayBackendCapability, BlurayChapterInfo, BlurayLpcmBitDepth,
    BlurayLpcmBitDepthProbeFailure, BlurayLpcmNotProbedReason, BlurayLpcmPidProbeFailure,
    BlurayLpcmPidProbeFailureReason, BlurayLpcmProbeStopReason, BlurayPtsContinuitySegment,
    BlurayStreamDecryptor, BlurayTitleInfo, BlurayTitleKey, ProbeDepth,
};
use super::bluray_utils::{
    bluray_audio_layout_from_libbluray_code, bluray_audio_rate_from_libbluray_code,
    BlurayLpcmPesProbe, BlurayLpcmPesProbeFailureReason,
};

const TITLES_ALL: u8 = 0;

// libbluray-sys generates bindings from libbluray/bluray.h with bindgen, so
// use those generated constants rather than duplicating libbluray enum values.
// If a future libbluray-sys release stops exposing these names cleanly, keep a
// local fallback table only with a comment that it mirrors libbluray's
// bd_event_e and bd_stream_type_e enums.
const BD_EVENT_ERROR: u32 = ffi::bd_event_e_BD_EVENT_ERROR as u32;
const BD_EVENT_READ_ERROR: u32 = ffi::bd_event_e_BD_EVENT_READ_ERROR as u32;
const BD_EVENT_ENCRYPTED: u32 = ffi::bd_event_e_BD_EVENT_ENCRYPTED as u32;
const BLURAY_STREAM_TYPE_AUDIO_LPCM: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_LPCM as u8;
const BLURAY_STREAM_TYPE_AUDIO_AC3: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_AC3 as u8;
const BLURAY_STREAM_TYPE_AUDIO_DTS: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_DTS as u8;
const BLURAY_STREAM_TYPE_AUDIO_TRUHD: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_TRUHD as u8;
const BLURAY_STREAM_TYPE_AUDIO_AC3PLUS: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_AC3PLUS as u8;
const BLURAY_STREAM_TYPE_AUDIO_DTSHD: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_DTSHD as u8;
const BLURAY_STREAM_TYPE_AUDIO_DTSHD_MASTER: u8 = ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_DTSHD_MASTER as u8;
const BLURAY_STREAM_TYPE_AUDIO_AC3PLUS_SECONDARY: u8 =
    ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_AC3PLUS_SECONDARY as u8;
const BLURAY_STREAM_TYPE_AUDIO_DTSHD_SECONDARY: u8 =
    ffi::bd_stream_type_e_BLURAY_STREAM_TYPE_AUDIO_DTSHD_SECONDARY as u8;
const TS_PACKET_SIZE: usize = 188;
const LPCM_PROBE_CHUNK_PACKETS: usize = 512;
/// Recommended bounded LPCM probe policy for callers that explicitly opt in to
/// reading title media bytes through `streams_with_probe_policy`. Plain
/// `streams()` does not use this budget; it is metadata-only.
const DEFAULT_LPCM_PROBE_LIMIT_BYTES: u64 = ProbeDepth::DEFAULT_MAX_BYTES;

pub struct BlurayBackendLibbluray;

impl BlurayBackendLibbluray {
    #[must_use]
    pub const fn default_lpcm_probe_policy() -> ProbeDepth {
        ProbeDepth::None
    }

    #[must_use]
    pub const fn bounded_lpcm_probe_policy() -> ProbeDepth {
        ProbeDepth::Bounded {
            max_bytes: DEFAULT_LPCM_PROBE_LIMIT_BYTES,
            max_duration: ProbeDepth::DEFAULT_MAX_DURATION,
        }
    }

    pub fn streams_with_probe_policy(
        disc: &BlurayDisc,
        title: BlurayTitleKey,
        policy: ProbeDepth,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
        streams_with_probe_policy_impl(disc, title, policy)
    }
}

/// Safe owner for a libbluray handle.
///
/// The handle kept on `BlurayDisc` is metadata-only after disc open. Each
/// `BlurayTitleSource` owns a separate libbluray handle, because libbluray has a
/// single active title and read cursor per `BLURAY*`.
pub struct BlurayDisc {
    metadata_handle: RefCell<BlurayHandle>,
    source_path: PathBuf,
    title_count: u32,
    _not_send_sync: PhantomData<Rc<()>>,
}

struct BlurayHandle {
    handle: NonNull<ffi::BLURAY>,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl Drop for BlurayHandle {
    fn drop(&mut self) {
        unsafe {
            ffi::bd_close(self.handle.as_ptr());
        }
    }
}

/// Read+Seek view over one selected libbluray title.
///
/// This type owns its `BLURAY*`; metadata queries and other title readers cannot
/// alter its selected title or stream position.
pub struct BlurayTitleSource {
    handle: BlurayHandle,
    title: BlurayTitleKey,
    angle_number: u8,
}

impl BlurayDisc {
    pub fn open(path: &Path) -> Result<Self, String> {
        let (handle, title_count) = BlurayHandle::open_loaded(path)?;
        Ok(Self {
            metadata_handle: RefCell::new(handle),
            source_path: path.to_path_buf(),
            title_count,
            _not_send_sync: PhantomData,
        })
    }

    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    #[must_use]
    pub fn title_count(&self) -> u32 {
        self.title_count
    }

    pub fn title_info(&self, index: u32) -> Result<BlurayTitleInfo, String> {
        let mut handle = self.metadata_handle.borrow_mut();
        let guard = handle.title_info_guard(index, 0, self.title_count)?;
        let info = guard.as_ref();
        Ok(title_info_from_ffi(info))
    }

    pub fn title_key(&self, index: u32) -> Result<BlurayTitleKey, String> {
        self.title_info(index).map(|info| info.key)
    }

    fn title_info_guard(&self, index: u32, angle_number: u8) -> Result<TitleInfoGuard, String> {
        let mut handle = self.metadata_handle.borrow_mut();
        handle.title_info_guard(index, angle_number, self.title_count)
    }

    fn open_title_handle(
        &self,
        title_index: u32,
        angle_number: u8,
    ) -> Result<BlurayHandle, String> {
        BlurayHandle::open_for_title(
            &self.source_path,
            title_index,
            angle_number,
            self.title_count,
        )
    }
}

impl BlurayHandle {
    fn open(path: &Path) -> Result<Self, String> {
        let path_c = path_to_cstring(path)?;
        let handle = unsafe { ffi::bd_open(path_c.as_ptr(), ptr::null()) };
        let handle = NonNull::new(handle)
            .ok_or_else(|| format!("libbluray bd_open failed for {}", path.display()))?;
        Ok(Self {
            handle,
            _not_send_sync: PhantomData,
        })
    }

    fn open_loaded(path: &Path) -> Result<(Self, u32), String> {
        let mut handle = Self::open(path)?;
        handle.init_event_queue();
        handle.check_events()?;
        let title_count = handle.load_title_list();
        handle.check_events()?;
        Ok((handle, title_count))
    }

    fn open_for_title(
        path: &Path,
        title_index: u32,
        angle_number: u8,
        expected_title_count: u32,
    ) -> Result<Self, String> {
        if title_index >= expected_title_count {
            return Err(format!(
                "Blu-ray title index {title_index} outside title count {expected_title_count}"
            ));
        }

        let (mut handle, title_count) = Self::open_loaded(path)?;
        if title_count != expected_title_count {
            return Err(format!(
                "Blu-ray title count changed while opening independent handle: expected {}, got {}",
                expected_title_count, title_count
            ));
        }
        handle.select_title_and_angle(title_index, angle_number, title_count)?;
        Ok(handle)
    }

    fn init_event_queue(&mut self) {
        unsafe {
            ffi::bd_get_event(self.handle.as_ptr(), ptr::null_mut());
        }
    }

    fn load_title_list(&mut self) -> u32 {
        unsafe { ffi::bd_get_titles(self.handle.as_ptr(), TITLES_ALL, 0) }
    }

    fn check_events(&mut self) -> Result<(), String> {
        drain_events(self.handle.as_ptr()).map_err(|err| err.to_string())
    }

    fn check_events_io(&mut self) -> io::Result<()> {
        drain_events(self.handle.as_ptr()).map_err(LibblurayEventError::into_io_error)
    }

    fn title_info_guard(
        &mut self,
        index: u32,
        angle_number: u8,
        title_count: u32,
    ) -> Result<TitleInfoGuard, String> {
        if index >= title_count {
            return Err(format!(
                "Blu-ray title index {index} outside title count {title_count}"
            ));
        }

        let raw = unsafe {
            ffi::bd_get_title_info(self.handle.as_ptr(), index, angle_number.into())
        };
        let ptr = complete_title_info_after_events(raw, index, angle_number, drain_events(self.handle.as_ptr()), |ptr| {
            unsafe {
                ffi::bd_free_title_info(ptr.as_ptr());
            }
        })?;
        Ok(TitleInfoGuard { ptr })
    }

    fn select_title_and_angle(
        &mut self,
        title_index: u32,
        angle_number: u8,
        title_count: u32,
    ) -> Result<(), String> {
        if title_index >= title_count {
            return Err(format!(
                "Blu-ray title index {title_index} outside title count {title_count}"
            ));
        }

        let selected = unsafe { ffi::bd_select_title(self.handle.as_ptr(), title_index) };
        let title_events = self.check_events();
        if selected <= 0 {
            return match title_events {
                Err(event) => Err(event),
                Ok(()) => Err(format!("libbluray bd_select_title({title_index}) failed")),
            };
        }
        title_events?;

        let angle = unsafe { ffi::bd_select_angle(self.handle.as_ptr(), angle_number.into()) };
        let angle_events = self.check_events();
        if angle <= 0 && angle_number != 0 {
            return match angle_events {
                Err(event) => Err(event),
                Ok(()) => Err(format!("libbluray bd_select_angle({angle_number}) failed")),
            };
        }
        angle_events
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let len: c_int = buf
            .len()
            .min(c_int::MAX as usize)
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "read buffer too large"))?;
        let result = unsafe { ffi::bd_read(self.handle.as_ptr(), buf.as_mut_ptr(), len) };
        complete_read_after_events(result, self.check_events_io())
    }

    fn seek(&mut self, absolute: u64) -> io::Result<u64> {
        let result = unsafe { ffi::bd_seek(self.handle.as_ptr(), absolute) };
        complete_position_after_events("bd_seek", result, self.check_events_io())
    }

    fn tell(&mut self) -> u64 {
        unsafe { ffi::bd_tell(self.handle.as_ptr()) as u64 }
    }

    fn title_size(&mut self) -> u64 {
        unsafe { ffi::bd_get_title_size(self.handle.as_ptr()) as u64 }
    }

    #[allow(dead_code)]
    fn seek_chapter(&mut self, chapter: u32) -> io::Result<u64> {
        let result = unsafe { ffi::bd_seek_chapter(self.handle.as_ptr(), chapter) };
        complete_position_after_events(
            &format!("bd_seek_chapter({chapter})"),
            result,
            self.check_events_io(),
        )
    }
}

struct TitleInfoGuard {
    ptr: NonNull<ffi::BLURAY_TITLE_INFO>,
}

fn complete_title_info_after_events<F>(
    raw: *mut ffi::BLURAY_TITLE_INFO,
    index: u32,
    angle_number: u8,
    title_events: Result<(), LibblurayEventError>,
    free_title_info: F,
) -> Result<NonNull<ffi::BLURAY_TITLE_INFO>, String>
where
    F: FnOnce(NonNull<ffi::BLURAY_TITLE_INFO>),
{
    let Some(ptr) = NonNull::new(raw) else {
        return match title_events {
            Err(event) => Err(event.to_string()),
            Ok(()) => Err(format!(
                "libbluray bd_get_title_info({index}, {angle_number}) returned NULL"
            )),
        };
    };

    if let Err(event) = title_events {
        free_title_info(ptr);
        return Err(event.to_string());
    }

    Ok(ptr)
}

impl TitleInfoGuard {
    fn as_ref(&self) -> &ffi::BLURAY_TITLE_INFO {
        unsafe { self.ptr.as_ref() }
    }
}

impl Drop for TitleInfoGuard {
    fn drop(&mut self) {
        unsafe {
            ffi::bd_free_title_info(self.ptr.as_ptr());
        }
    }
}

impl Read for BlurayTitleSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.read(buf)
    }
}

impl Seek for BlurayTitleSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let absolute = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => {
                let current = self.tell();
                checked_seek_target(current, delta)?
            }
            SeekFrom::End(delta) => {
                let size = self.title_size();
                checked_seek_target(size, delta)?
            }
        };

        self.handle.seek(absolute)
    }
}

impl BlurayTitleSource {
    #[must_use]
    pub fn title(&self) -> BlurayTitleKey {
        self.title
    }

    #[must_use]
    pub fn angle_number(&self) -> u8 {
        self.angle_number
    }

    #[allow(dead_code)]
    pub fn seek_chapter(&mut self, chapter: u32) -> io::Result<u64> {
        self.handle.seek_chapter(chapter)
    }

    fn tell(&mut self) -> u64 {
        self.handle.tell()
    }

    fn title_size(&mut self) -> u64 {
        self.handle.title_size()
    }
}

impl BlurayBackend for BlurayBackendLibbluray {
    type Disc = BlurayDisc;
    type TitleSource = BlurayTitleSource;

    fn open(path: &Path) -> Result<Self::Disc, String> {
        BlurayDisc::open(path)
    }

    fn disc_label(_disc: &Self::Disc, source: &Path) -> Option<String> {
        bluray_source_stem(source)
    }

    fn titles(disc: &Self::Disc) -> Result<Vec<BlurayTitleInfo>, String> {
        let mut titles = Vec::with_capacity(disc.title_count() as usize);
        for index in 0..disc.title_count() {
            titles.push(disc.title_info(index)?);
        }
        Ok(titles)
    }

    fn title_by_playlist(
        disc: &Self::Disc,
        playlist_number: u32,
    ) -> Result<BlurayTitleKey, String> {
        Self::titles(disc)?
            .into_iter()
            .find(|title| title.key.playlist_number() == playlist_number)
            .map(|title| title.key)
            .ok_or_else(|| format!("Blu-ray playlist {playlist_number:05} not found"))
    }

    fn chapters(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
    ) -> Result<Vec<BlurayChapterInfo>, String> {
        let guard = disc.title_info_guard(title.title_index(), angle_number)?;
        let info = guard.as_ref();
        if info.chapters.is_null() || info.chapter_count == 0 {
            return Ok(Vec::new());
        }

        let chapters = unsafe {
            std::slice::from_raw_parts(info.chapters, info.chapter_count as usize)
        };
        Ok(chapters
            .iter()
            .map(|chapter| {
                let duration = chapter.duration;
                BlurayChapterInfo {
                    chapter_number: chapter.idx.saturating_add(1),
                    start_pts_90k: chapter.start,
                    end_pts_90k: (duration > 0).then_some(chapter.start.saturating_add(duration)),
                    duration_pts_90k: (duration > 0).then_some(duration),
                    byte_offset: Some(chapter.offset),
                    clip_ref: Some(chapter.clip_ref),
                }
            })
            .collect())
    }

    fn streams(
        disc: &Self::Disc,
        title: BlurayTitleKey,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
        streams_with_probe_policy_impl(disc, title, ProbeDepth::None)
    }

    fn streams_with_probe_policy(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        policy: ProbeDepth,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
        streams_with_probe_policy_impl(disc, title, policy)
    }

    fn max_angle(disc: &Self::Disc, title: BlurayTitleKey) -> Result<u8, String> {
        let guard = disc.title_info_guard(title.title_index(), 0)?;
        Ok(guard.as_ref().angle_count)
    }

    fn open_title(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
        decryptor: Option<&mut dyn BlurayStreamDecryptor>,
    ) -> Result<Self::TitleSource, String> {
        if decryptor.is_some() {
            return Err("external Blu-ray decryptor hooks are Phase 6 work".to_string());
        }
        let handle = disc.open_title_handle(title.title_index(), angle_number)?;
        Ok(BlurayTitleSource {
            handle,
            title,
            angle_number,
        })
    }

    fn pts_continuity_segments(
        _source: &Self::TitleSource,
    ) -> Result<BlurayBackendCapability<Vec<BlurayPtsContinuitySegment>>, String> {
        Ok(BlurayBackendCapability::unsupported(
            "libbluray Phase 0 does not expose title PTS continuity segments",
        ))
    }
}

fn title_info_from_ffi(info: &ffi::BLURAY_TITLE_INFO) -> BlurayTitleInfo {
    let key = BlurayTitleKey::from_libbluray(info.idx, info.playlist);
    BlurayTitleInfo {
        key,
        playlist_number: info.playlist,
        duration_pts_90k: info.duration,
        angle_count: info.angle_count,
        chapter_count: info.chapter_count,
        clip_count: info.clip_count,
    }
}

fn audio_streams_from_title_info(
    info: &ffi::BLURAY_TITLE_INFO,
) -> Result<Vec<BlurayAudioStreamInfo>, String> {
    if info.clips.is_null() || info.clip_count == 0 {
        return Ok(Vec::new());
    }

    let clips = unsafe { std::slice::from_raw_parts(info.clips, info.clip_count as usize) };
    let mut streams = audio_streams_for_kind_from_clips(clips, BluRayAudioStreamKind::Primary)?;
    streams.extend(audio_streams_for_kind_from_clips(
        clips,
        BluRayAudioStreamKind::Secondary,
    )?);
    Ok(streams)
}

fn audio_streams_for_kind_from_clips(
    clips: &[ffi::BLURAY_CLIP_INFO],
    kind: BluRayAudioStreamKind,
) -> Result<Vec<BlurayAudioStreamInfo>, String> {
    let mut per_clip = Vec::with_capacity(clips.len());
    for (clip_index, clip) in clips.iter().enumerate() {
        per_clip.push(audio_stream_descriptors_from_clip(clip, clip_index, kind)?);
    }

    Ok(reconcile_clip_audio_descriptors(kind, &per_clip)?
        .into_iter()
        .map(ClipAudioStreamDescriptor::into_public)
        .collect())
}

fn audio_stream_descriptors_from_clip(
    clip: &ffi::BLURAY_CLIP_INFO,
    clip_index: usize,
    kind: BluRayAudioStreamKind,
) -> Result<Vec<ClipAudioStreamDescriptor>, String> {
    let (count, ptr) = match kind {
        BluRayAudioStreamKind::Primary => (clip.audio_stream_count, clip.audio_streams),
        BluRayAudioStreamKind::Secondary => (clip.sec_audio_stream_count, clip.sec_audio_streams),
    };

    if count == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(format!(
            "Blu-ray clip {} reports {} {} audio streams but libbluray returned a NULL stream table",
            clip_index, count, kind.label()
        ));
    }

    let streams = unsafe { std::slice::from_raw_parts(ptr, count as usize) };
    streams
        .iter()
        .enumerate()
        .map(|(index, stream)| audio_stream_descriptor_from_ffi(kind, index, stream))
        .collect()
}

fn reconcile_clip_audio_descriptors(
    kind: BluRayAudioStreamKind,
    per_clip: &[Vec<ClipAudioStreamDescriptor>],
) -> Result<Vec<ClipAudioStreamDescriptor>, String> {
    let Some(first_clip) = per_clip.first() else {
        return Ok(Vec::new());
    };

    for (clip_index, clip) in per_clip.iter().enumerate().skip(1) {
        if clip.len() != first_clip.len() {
            return Err(format!(
                "Blu-ray {} audio stream count changes across clips: clip 0 has {}, clip {} has {}; Phase 0 cannot represent clip-varying audio tables",
                kind.label(), first_clip.len(), clip_index, clip.len()
            ));
        }

        for (stream_index, (expected, actual)) in first_clip.iter().zip(clip.iter()).enumerate() {
            if !expected.matches_title_wide_stream(actual) {
                return Err(format!(
                    "Blu-ray {} audio stream {} changes across clips: clip 0 has {}, clip {} has {}; Phase 0 cannot represent clip-varying audio metadata",
                    kind.label(), stream_index + 1, expected.summary(), clip_index, actual.summary()
                ));
            }
        }
    }

    Ok(first_clip.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClipAudioStreamDescriptor {
    kind: BluRayAudioStreamKind,
    stream_index: u8,
    pid: u16,
    coding: BluRayAudioCoding,
    sample_rate: Option<u32>,
    channels: Option<u8>,
    channel_layout: Option<String>,
    language: Option<String>,
    raw_coding_type: u8,
    raw_format: u8,
    raw_rate: u8,
    raw_lang: [u8; 4],
}

impl ClipAudioStreamDescriptor {
    fn matches_title_wide_stream(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.stream_index == other.stream_index
            && self.pid == other.pid
            && self.raw_coding_type == other.raw_coding_type
            && self.raw_format == other.raw_format
            && self.raw_rate == other.raw_rate
            && self.raw_lang == other.raw_lang
    }

    fn summary(&self) -> String {
        format!(
            "pid=0x{:04x} coding=0x{:02x} format=0x{:02x} rate=0x{:02x} lang={}",
            self.pid,
            self.raw_coding_type,
            self.raw_format,
            self.raw_rate,
            self.language.as_deref().unwrap_or("und")
        )
    }

    fn into_public(self) -> BlurayAudioStreamInfo {
        let bit_depth = if self.coding == BluRayAudioCoding::Lpcm {
            BlurayLpcmBitDepth::NotProbed {
                reason: BlurayLpcmNotProbedReason::PrimaryProbeNotRun,
            }
        } else {
            BlurayLpcmBitDepth::NotApplicable
        };

        BlurayAudioStreamInfo {
            kind: self.kind,
            pid: self.pid,
            stream_index: self.stream_index,
            coding: self.coding,
            sample_rate: self.sample_rate,
            bit_depth,
            channels: self.channels,
            channel_layout: self.channel_layout,
            language: self.language,
        }
    }
}

fn audio_stream_descriptor_from_ffi(
    kind: BluRayAudioStreamKind,
    index: usize,
    stream: &ffi::BLURAY_STREAM_INFO,
) -> Result<ClipAudioStreamDescriptor, String> {
    let Some(coding) = audio_coding_from_stream_type(stream.coding_type) else {
        return Err(format!(
            "unsupported Blu-ray {} audio coding type 0x{:02x} for stream {} pid 0x{:04x}",
            kind.label(),
            stream.coding_type,
            index + 1,
            stream.pid
        ));
    };
    let (channels, channel_layout) = bluray_audio_layout_from_libbluray_code(stream.format);
    Ok(ClipAudioStreamDescriptor {
        kind,
        pid: stream.pid,
        stream_index: index.min(u8::MAX as usize) as u8,
        coding,
        sample_rate: bluray_audio_rate_from_libbluray_code(stream.rate),
        channels,
        channel_layout,
        language: lang_from_libbluray(stream.lang),
        raw_coding_type: stream.coding_type,
        raw_format: stream.format,
        raw_rate: stream.rate,
        raw_lang: stream.lang,
    })
}

fn audio_coding_from_stream_type(coding_type: u8) -> Option<BluRayAudioCoding> {
    match coding_type {
        BLURAY_STREAM_TYPE_AUDIO_LPCM => Some(BluRayAudioCoding::Lpcm),
        BLURAY_STREAM_TYPE_AUDIO_AC3 => Some(BluRayAudioCoding::Ac3),
        BLURAY_STREAM_TYPE_AUDIO_DTS => Some(BluRayAudioCoding::Dts),
        BLURAY_STREAM_TYPE_AUDIO_TRUHD => Some(BluRayAudioCoding::TrueHd),
        BLURAY_STREAM_TYPE_AUDIO_AC3PLUS => Some(BluRayAudioCoding::Eac3),
        BLURAY_STREAM_TYPE_AUDIO_DTSHD => Some(BluRayAudioCoding::DtsHd),
        BLURAY_STREAM_TYPE_AUDIO_DTSHD_MASTER => Some(BluRayAudioCoding::DtsHdMaster),
        BLURAY_STREAM_TYPE_AUDIO_AC3PLUS_SECONDARY => Some(BluRayAudioCoding::Eac3),
        BLURAY_STREAM_TYPE_AUDIO_DTSHD_SECONDARY => Some(BluRayAudioCoding::DtsHd),
        _ => None,
    }
}

fn streams_with_probe_policy_impl(
    disc: &BlurayDisc,
    title: BlurayTitleKey,
    policy: ProbeDepth,
) -> Result<Vec<BlurayAudioStreamInfo>, String> {
    let guard = disc.title_info_guard(title.title_index(), 0)?;
    let info = guard.as_ref();
    let mut streams = audio_streams_from_title_info(info)?;

    initialize_lpcm_probe_statuses(&mut streams, &policy);

    let primary_lpcm_pids: HashSet<u16> = streams
        .iter()
        .filter(|stream| {
            stream.kind == BluRayAudioStreamKind::Primary
                && stream.coding == BluRayAudioCoding::Lpcm
                && !stream.bit_depth.is_probed()
        })
        .map(|stream| stream.pid)
        .collect();

    if !primary_lpcm_pids.is_empty() && !matches!(policy, ProbeDepth::None) {
        let report = probe_lpcm_headers_for_title(disc, title, 0, &primary_lpcm_pids, &policy)?;
        apply_lpcm_probe_report(&mut streams, &report);
    }

    Ok(streams)
}

fn probe_lpcm_headers_for_title(
    disc: &BlurayDisc,
    title: BlurayTitleKey,
    angle_number: u8,
    pids: &HashSet<u16>,
    policy: &ProbeDepth,
) -> Result<LpcmProbeReport, String> {
    debug_assert!(!matches!(policy, ProbeDepth::None));
    let handle = disc.open_title_handle(title.title_index(), angle_number)?;
    let mut source = BlurayTitleSource {
        handle,
        title,
        angle_number,
    };
    source
        .seek(SeekFrom::Start(0))
        .map_err(|err| err.to_string())?;

    Ok(read_lpcm_probe_window(&mut source, pids, policy))
}

#[derive(Debug, Clone)]
struct LpcmProbeReport {
    headers: HashMap<u16, super::bluray_utils::BlurayLpcmPesHeader>,
    scanned_bytes: u64,
    missing_pids: Vec<u16>,
    pid_failures: Vec<BlurayLpcmPidProbeFailure>,
    completion: LpcmProbeCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LpcmProbeCompletion {
    AllTargetsFound,
    Stopped(BlurayLpcmProbeStopReason),
}

fn read_lpcm_probe_window<R: Read>(
    reader: &mut R,
    pids: &HashSet<u16>,
    policy: &ProbeDepth,
) -> LpcmProbeReport {
    let mut probe = BlurayLpcmPesProbe::new(pids.iter().copied());
    let mut buf = vec![0u8; TS_PACKET_SIZE * LPCM_PROBE_CHUNK_PACKETS];
    let mut scanned_bytes = 0u64;
    let mut stop_reason = None;
    let started_at = Instant::now();

    while !probe.is_complete() {
        match policy {
            ProbeDepth::None => {
                stop_reason = Some(BlurayLpcmProbeStopReason::ByteLimit);
                break;
            }
            ProbeDepth::Bounded {
                max_bytes,
                max_duration,
            } => {
                if scanned_bytes >= *max_bytes {
                    stop_reason = Some(BlurayLpcmProbeStopReason::ByteLimit);
                    break;
                }
                if started_at.elapsed() >= *max_duration {
                    stop_reason = Some(BlurayLpcmProbeStopReason::TimeLimit);
                    break;
                }
            }
            ProbeDepth::Exhaustive => {}
        }

        let read_len = match policy {
            ProbeDepth::Bounded { max_bytes, .. } => {
                let remaining = max_bytes.saturating_sub(scanned_bytes);
                buf.len().min(remaining.min(usize::MAX as u64) as usize)
            }
            ProbeDepth::Exhaustive => buf.len(),
            ProbeDepth::None => 0,
        };
        if read_len == 0 {
            stop_reason = Some(BlurayLpcmProbeStopReason::ByteLimit);
            break;
        }

        let count = match reader.read(&mut buf[..read_len]) {
            Ok(0) => {
                stop_reason = Some(BlurayLpcmProbeStopReason::EndOfTitle);
                break;
            }
            Ok(count) => count,
            Err(err) => {
                stop_reason = Some(BlurayLpcmProbeStopReason::ReadError {
                    message: err.to_string(),
                });
                break;
            }
        };
        scanned_bytes = scanned_bytes.saturating_add(count as u64);
        probe.feed(&buf[..count]);
    }

    if !probe.is_complete() {
        if let ProbeDepth::Bounded { max_duration, .. } = policy {
            if stop_reason.is_none() && started_at.elapsed() >= *max_duration {
                stop_reason = Some(BlurayLpcmProbeStopReason::TimeLimit);
            }
        }
    }

    let mut missing_pids: Vec<u16> = pids
        .iter()
        .copied()
        .filter(|pid| !probe.found().contains_key(pid))
        .collect();
    missing_pids.sort_unstable();
    let pid_failures = missing_pids
        .iter()
        .copied()
        .map(|pid| BlurayLpcmPidProbeFailure {
            pid,
            reason: pid_failure_reason_from_probe(probe.failure_reason(pid)),
        })
        .collect();
    let headers = probe.into_found();

    let completion = if missing_pids.is_empty() {
        LpcmProbeCompletion::AllTargetsFound
    } else {
        LpcmProbeCompletion::Stopped(
            stop_reason.unwrap_or(BlurayLpcmProbeStopReason::EndOfTitle),
        )
    };

    LpcmProbeReport {
        headers,
        scanned_bytes,
        missing_pids,
        pid_failures,
        completion,
    }
}

fn pid_failure_reason_from_probe(
    reason: BlurayLpcmPesProbeFailureReason,
) -> BlurayLpcmPidProbeFailureReason {
    match reason {
        BlurayLpcmPesProbeFailureReason::PesStartNotFound => {
            BlurayLpcmPidProbeFailureReason::PesStartNotFound
        }
        BlurayLpcmPesProbeFailureReason::LpcmSubheaderIncomplete => {
            BlurayLpcmPidProbeFailureReason::LpcmSubheaderIncomplete
        }
        BlurayLpcmPesProbeFailureReason::InvalidPesPrefix => {
            BlurayLpcmPidProbeFailureReason::InvalidPesPrefix
        }
        BlurayLpcmPesProbeFailureReason::InvalidLpcmHeader { message } => {
            BlurayLpcmPidProbeFailureReason::InvalidLpcmHeader { message }
        }
    }
}

fn bit_depth_failure_from_stop(
    stop_reason: &BlurayLpcmProbeStopReason,
    pid_failures: Vec<BlurayLpcmPidProbeFailure>,
) -> BlurayLpcmBitDepthProbeFailure {
    match stop_reason {
        BlurayLpcmProbeStopReason::ByteLimit => {
            BlurayLpcmBitDepthProbeFailure::ByteLimit {
                missing_pids: pid_failures,
            }
        }
        BlurayLpcmProbeStopReason::TimeLimit => {
            BlurayLpcmBitDepthProbeFailure::TimeLimit {
                missing_pids: pid_failures,
            }
        }
        BlurayLpcmProbeStopReason::EndOfTitle => {
            BlurayLpcmBitDepthProbeFailure::EndOfTitle {
                missing_pids: pid_failures,
            }
        }
        BlurayLpcmProbeStopReason::ReadError { message } => {
            BlurayLpcmBitDepthProbeFailure::ReadError {
                message: message.clone(),
                missing_pids: pid_failures,
            }
        }
    }
}

fn initialize_lpcm_probe_statuses(streams: &mut [BlurayAudioStreamInfo], policy: &ProbeDepth) {
    for stream in streams {
        stream.bit_depth = if stream.coding != BluRayAudioCoding::Lpcm {
            BlurayLpcmBitDepth::NotApplicable
        } else {
            match stream.kind {
                BluRayAudioStreamKind::Primary => match policy {
                    ProbeDepth::None => BlurayLpcmBitDepth::NotProbed {
                        reason: BlurayLpcmNotProbedReason::ProbePolicyNone,
                    },
                    ProbeDepth::Bounded { .. } | ProbeDepth::Exhaustive => {
                        BlurayLpcmBitDepth::NotProbed {
                            reason: BlurayLpcmNotProbedReason::PrimaryProbeNotRun,
                        }
                    }
                },
                BluRayAudioStreamKind::Secondary => BlurayLpcmBitDepth::NotProbed {
                    reason: BlurayLpcmNotProbedReason::SecondaryStreamNotInMainTransport,
                },
            }
        };
    }
}

fn apply_lpcm_probe_report(streams: &mut [BlurayAudioStreamInfo], report: &LpcmProbeReport) {
    for stream in streams {
        if stream.kind != BluRayAudioStreamKind::Primary || stream.coding != BluRayAudioCoding::Lpcm
        {
            continue;
        }

        if let Some(header) = report.headers.get(&stream.pid) {
            stream.sample_rate = Some(header.sample_rate);
            stream.bit_depth = BlurayLpcmBitDepth::Probed {
                bit_depth: header.bit_depth,
                scanned_bytes: report.scanned_bytes,
            };
            stream.channels = Some(header.channels);
            stream.channel_layout = Some(header.channel_layout.to_string());
        } else {
            let stop_reason = match &report.completion {
                LpcmProbeCompletion::AllTargetsFound => &BlurayLpcmProbeStopReason::EndOfTitle,
                LpcmProbeCompletion::Stopped(stop_reason) => stop_reason,
            };
            let pid_failures = report
                .pid_failures
                .iter()
                .filter(|failure| failure.pid == stream.pid)
                .cloned()
                .collect();
            stream.bit_depth = BlurayLpcmBitDepth::ProbeFailed {
                bytes_scanned: report.scanned_bytes,
                reason: bit_depth_failure_from_stop(stop_reason, pid_failures),
            };
        }
    }
}

fn checked_seek_target(base: u64, delta: i64) -> io::Result<u64> {
    if delta >= 0 {
        base.checked_add(delta as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))
    } else {
        base.checked_sub(delta.unsigned_abs())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))
    }
}

fn complete_read_after_events(
    raw_result: c_int,
    event_result: io::Result<()>,
) -> io::Result<usize> {
    if raw_result < 0 {
        return Err(event_result
            .err()
            .unwrap_or_else(|| io::Error::new(io::ErrorKind::Other, "libbluray bd_read failed")));
    }
    event_result?;
    Ok(raw_result as usize)
}

fn complete_position_after_events(
    operation: &str,
    raw_result: i64,
    event_result: io::Result<()>,
) -> io::Result<u64> {
    if raw_result < 0 {
        return Err(event_result.err().unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::Other, format!("libbluray {operation} failed"))
        }));
    }
    event_result?;
    Ok(raw_result as u64)
}

#[cfg(unix)]
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;

    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("path contains interior NUL byte: {}", path.display()))
}

#[cfg(not(unix))]
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    let path = path.as_os_str().to_str().ok_or_else(|| {
        format!("path is not valid UTF-8 and cannot be passed to libbluray: {}", path.display())
    })?;

    CString::new(path)
        .map_err(|_| format!("path contains interior NUL byte: {}", path.display()))
}

fn lang_from_libbluray(lang: [u8; 4]) -> Option<String> {
    let bytes: Vec<u8> = lang
        .into_iter()
        .take_while(|byte| *byte != 0)
        .filter(|byte| byte.is_ascii_alphabetic())
        .collect();
    if bytes.len() == 3 {
        String::from_utf8(bytes).ok()
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LibblurayEvent {
    event: u32,
    param: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LibblurayEventError {
    event: u32,
    param: u32,
    name: Option<String>,
}

impl LibblurayEventError {
    fn new(event: u32, param: u32) -> Self {
        Self {
            event,
            param,
            name: event_name(event),
        }
    }

    fn into_io_error(self) -> io::Error {
        let kind = match self.event {
            BD_EVENT_READ_ERROR => io::ErrorKind::UnexpectedEof,
            BD_EVENT_ENCRYPTED => io::ErrorKind::PermissionDenied,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, self.to_string())
    }
}

impl std::fmt::Display for LibblurayEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.event {
            BD_EVENT_ERROR => write!(
                f,
                "libbluray event {} param {}",
                self.name.as_deref().unwrap_or("ERROR"),
                self.param
            ),
            BD_EVENT_READ_ERROR => write!(
                f,
                "libbluray event {} param {}",
                self.name.as_deref().unwrap_or("READ_ERROR"),
                self.param
            ),
            BD_EVENT_ENCRYPTED => write!(
                f,
                "Blu-ray stream is encrypted and not usable in Phase 0"
            ),
            _ => write!(
                f,
                "libbluray fatal event {} param {}",
                self.name.as_deref().unwrap_or("UNKNOWN"),
                self.param
            ),
        }
    }
}

impl std::error::Error for LibblurayEventError {}

fn drain_events(handle: *mut ffi::BLURAY) -> Result<(), LibblurayEventError> {
    loop {
        let mut raw_event = std::mem::MaybeUninit::<ffi::BD_EVENT>::uninit();
        let has_event = unsafe { ffi::bd_get_event(handle, raw_event.as_mut_ptr()) };
        if has_event == 0 {
            return Ok(());
        }
        let raw_event = unsafe { raw_event.assume_init() };
        if let Some(error) = event_error(LibblurayEvent {
            event: raw_event.event,
            param: raw_event.param,
        }) {
            return Err(error);
        }
    }
}

#[cfg(test)]
fn drain_queued_events<I>(events: I) -> Result<(), LibblurayEventError>
where
    I: IntoIterator<Item = LibblurayEvent>,
{
    for event in events {
        if let Some(error) = event_error(event) {
            return Err(error);
        }
    }
    Ok(())
}

fn event_error(event: LibblurayEvent) -> Option<LibblurayEventError> {
    match event.event {
        BD_EVENT_ERROR | BD_EVENT_READ_ERROR | BD_EVENT_ENCRYPTED => {
            Some(LibblurayEventError::new(event.event, event.param))
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn event_name(event: u32) -> Option<String> {
    match event {
        BD_EVENT_ERROR => Some("ERROR".to_string()),
        BD_EVENT_READ_ERROR => Some("READ_ERROR".to_string()),
        BD_EVENT_ENCRYPTED => Some("ENCRYPTED".to_string()),
        _ => event_name_from_libbluray(event),
    }
}

#[allow(dead_code)]
fn event_name_from_libbluray(event: u32) -> Option<String> {
    let ptr = unsafe { ffi::bd_event_name(event) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_bd_audio_coding_types() {
        assert_eq!(audio_coding_from_stream_type(0x80), Some(BluRayAudioCoding::Lpcm));
        assert_eq!(audio_coding_from_stream_type(0x81), Some(BluRayAudioCoding::Ac3));
        assert_eq!(audio_coding_from_stream_type(0x82), Some(BluRayAudioCoding::Dts));
        assert_eq!(audio_coding_from_stream_type(0x83), Some(BluRayAudioCoding::TrueHd));
        assert_eq!(audio_coding_from_stream_type(0x84), Some(BluRayAudioCoding::Eac3));
        assert_eq!(audio_coding_from_stream_type(0x85), Some(BluRayAudioCoding::DtsHd));
        assert_eq!(audio_coding_from_stream_type(0x86), Some(BluRayAudioCoding::DtsHdMaster));
        assert_eq!(audio_coding_from_stream_type(0xa1), Some(BluRayAudioCoding::Eac3));
        assert_eq!(audio_coding_from_stream_type(0xa2), Some(BluRayAudioCoding::DtsHd));
        assert_eq!(audio_coding_from_stream_type(0x03), None);
    }

    #[cfg(unix)]
    #[test]
    fn path_to_cstring_preserves_non_utf8_unix_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let os_path = OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'i', b's', b'o',
        ]);
        let path = PathBuf::from(os_path);
        let c_path = path_to_cstring(&path).unwrap();

        assert_eq!(c_path.as_bytes(), path.as_os_str().as_bytes());
    }

    #[test]
    fn parses_libbluray_language_triplet() {
        assert_eq!(lang_from_libbluray(*b"eng\0"), Some("eng".to_string()));
        assert_eq!(lang_from_libbluray(*b"und\0"), Some("und".to_string()));
        assert_eq!(lang_from_libbluray([0, 0, 0, 0]), None);
    }

    #[test]
    fn checked_seek_rejects_negative_underflow() {
        assert_eq!(checked_seek_target(10, -5).unwrap(), 5);
        assert!(checked_seek_target(10, -11).is_err());
    }


    #[test]
    fn reconciles_matching_multi_clip_audio_descriptors() {
        let descriptor = test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1100);
        let result = reconcile_clip_audio_descriptors(
            BluRayAudioStreamKind::Primary,
            &[vec![descriptor.clone()], vec![descriptor.clone()]],
        )
        .expect("matching descriptors should reconcile");
        assert_eq!(result, vec![descriptor]);
    }

    #[test]
    fn rejects_clip_varying_audio_pid() {
        let result = reconcile_clip_audio_descriptors(
            BluRayAudioStreamKind::Primary,
            &[
                vec![test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1100)],
                vec![test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1101)],
            ],
        );
        let err = result.expect_err("pid changes cannot be represented by one title-wide stream");
        assert!(err.contains("changes across clips"));
    }

    #[test]
    fn rejects_clip_varying_secondary_audio_count() {
        let result = reconcile_clip_audio_descriptors(
            BluRayAudioStreamKind::Secondary,
            &[
                vec![test_descriptor(BluRayAudioStreamKind::Secondary, 0, 0x1a00)],
                Vec::new(),
            ],
        );
        let err = result.expect_err(
            "stream count changes cannot be represented by one title-wide table",
        );
        assert!(err.contains("stream count changes"));
    }


    #[test]
    fn libbluray_pts_segments_are_explicitly_unsupported() {
        let result = BlurayBackendCapability::<Vec<BlurayPtsContinuitySegment>>::unsupported(
            "libbluray Phase 0 does not expose title PTS continuity segments",
        );
        assert!(!result.is_supported());
        match result {
            BlurayBackendCapability::Unsupported { reason } => {
                assert!(reason.contains("does not expose"));
            }
            BlurayBackendCapability::Supported { .. } => panic!("expected unsupported capability"),
        }
    }

    #[test]
    fn lpcm_probe_finds_all_pids_before_cap() {
        let pids = HashSet::from([0x1100, 0x1101]);
        let packet_a = test_ts_packet(
            0x1100,
            true,
            0,
            &test_lpcm_pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]),
        );
        let packet_b = test_ts_packet(
            0x1101,
            true,
            1,
            &test_lpcm_pes_prefix([0, 0, (9 << 4) | 4, 3 << 6]),
        );
        let data = [packet_a.as_slice(), packet_b.as_slice()].concat();
        let mut reader = std::io::Cursor::new(data);

        let report = read_lpcm_probe_window(
            &mut reader,
            &pids,
            &ProbeDepth::Bounded {
                max_bytes: (TS_PACKET_SIZE * 8) as u64,
                max_duration: std::time::Duration::from_secs(60),
            },
        );

        assert_eq!(report.scanned_bytes, (TS_PACKET_SIZE * 2) as u64);
        assert_eq!(report.completion, LpcmProbeCompletion::AllTargetsFound);
        assert!(report.missing_pids.is_empty());
        assert_eq!(report.headers.get(&0x1100).unwrap().bit_depth, 16);
        assert_eq!(report.headers.get(&0x1101).unwrap().bit_depth, 24);
    }

    #[test]
    fn lpcm_probe_reports_missing_pid_when_cap_hits() {
        let pids = HashSet::from([0x1100, 0x1101]);
        let packet = test_ts_packet(
            0x1100,
            true,
            0,
            &test_lpcm_pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]),
        );
        let padding = [0u8; TS_PACKET_SIZE];
        let data = [packet.as_slice(), padding.as_slice()].concat();
        let mut reader = std::io::Cursor::new(data);

        let report = read_lpcm_probe_window(
            &mut reader,
            &pids,
            &ProbeDepth::Bounded {
                max_bytes: (TS_PACKET_SIZE * 2) as u64,
                max_duration: std::time::Duration::from_secs(60),
            },
        );

        assert_eq!(report.scanned_bytes, (TS_PACKET_SIZE * 2) as u64);
        assert_eq!(report.missing_pids, vec![0x1101]);
        assert_eq!(
            report.pid_failures,
            vec![BlurayLpcmPidProbeFailure {
                pid: 0x1101,
                reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
            }]
        );
        assert_eq!(
            report.completion,
            LpcmProbeCompletion::Stopped(BlurayLpcmProbeStopReason::ByteLimit)
        );
    }

    #[test]
    fn lpcm_probe_reports_eof_before_all_pids_appear() {
        let pids = HashSet::from([0x1100, 0x1101]);
        let data = test_ts_packet(
            0x1100,
            true,
            0,
            &test_lpcm_pes_prefix([0, 0, (3 << 4) | 1, 1 << 6]),
        );
        let mut reader = std::io::Cursor::new(data);

        let report = read_lpcm_probe_window(
            &mut reader,
            &pids,
            &ProbeDepth::Bounded {
                max_bytes: (TS_PACKET_SIZE * 8) as u64,
                max_duration: std::time::Duration::from_secs(60),
            },
        );

        assert_eq!(report.scanned_bytes, TS_PACKET_SIZE as u64);
        assert_eq!(report.missing_pids, vec![0x1101]);
        assert_eq!(
            report.pid_failures,
            vec![BlurayLpcmPidProbeFailure {
                pid: 0x1101,
                reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
            }]
        );
        assert_eq!(
            report.completion,
            LpcmProbeCompletion::Stopped(BlurayLpcmProbeStopReason::EndOfTitle)
        );
    }

    #[test]
    fn lpcm_probe_reports_read_error_without_failing_stream_enumeration() {
        let pids = HashSet::from([0x1100]);
        let mut reader = FailingReader::new("synthetic probe read failure");

        let report = read_lpcm_probe_window(&mut reader, &pids, &ProbeDepth::Exhaustive);

        assert_eq!(report.scanned_bytes, 0);
        assert_eq!(report.missing_pids, vec![0x1100]);
        assert_eq!(
            report.pid_failures,
            vec![BlurayLpcmPidProbeFailure {
                pid: 0x1100,
                reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
            }]
        );
        match report.completion {
            LpcmProbeCompletion::Stopped(BlurayLpcmProbeStopReason::ReadError { message }) => {
                assert!(message.contains("synthetic probe read failure"));
            }
            other => panic!("expected read-error stop reason, got {other:?}"),
        }
    }

    #[test]
    fn lpcm_probe_depth_none_performs_no_reads() {
        let pids = HashSet::from([0x1100]);
        let mut reader = CountingReader::new(vec![0u8; TS_PACKET_SIZE]);

        let report = read_lpcm_probe_window(&mut reader, &pids, &ProbeDepth::None);

        assert_eq!(reader.read_calls, 0);
        assert_eq!(report.scanned_bytes, 0);
        assert_eq!(report.missing_pids, vec![0x1100]);
    }

    #[test]
    fn streams_default_policy_is_metadata_only() {
        assert_eq!(ProbeDepth::default(), ProbeDepth::None);
        assert_eq!(
            BlurayBackendLibbluray::default_lpcm_probe_policy(),
            ProbeDepth::None
        );
        assert_eq!(
            BlurayBackendLibbluray::bounded_lpcm_probe_policy(),
            ProbeDepth::Bounded {
                max_bytes: ProbeDepth::DEFAULT_MAX_BYTES,
                max_duration: ProbeDepth::DEFAULT_MAX_DURATION,
            }
        );
        assert_eq!(ProbeDepth::DEFAULT_MAX_BYTES, 256 * 1024 * 1024);
        assert_eq!(ProbeDepth::DEFAULT_MAX_DURATION, std::time::Duration::from_secs(3));
    }

    #[test]
    fn primary_lpcm_streams_default_to_not_probed_for_metadata_only_streams() {
        let mut streams = vec![test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1100)
            .into_public()];

        initialize_lpcm_probe_statuses(&mut streams, &ProbeDepth::None);

        assert_eq!(
            streams[0].bit_depth,
            BlurayLpcmBitDepth::NotProbed {
                reason: BlurayLpcmNotProbedReason::ProbePolicyNone,
            }
        );
    }

    #[test]
    fn lpcm_probe_report_marks_failed_bit_depth_with_parser_reason() {
        let mut streams = vec![test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1100)
            .into_public()];
        initialize_lpcm_probe_statuses(
            &mut streams,
            &ProbeDepth::Bounded {
                max_bytes: 4096,
                max_duration: std::time::Duration::from_secs(60),
            },
        );
        let report = LpcmProbeReport {
            headers: HashMap::new(),
            scanned_bytes: 4096,
            missing_pids: vec![0x1100],
            pid_failures: vec![BlurayLpcmPidProbeFailure {
                pid: 0x1100,
                reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
            }],
            completion: LpcmProbeCompletion::Stopped(BlurayLpcmProbeStopReason::ByteLimit),
        };

        apply_lpcm_probe_report(&mut streams, &report);

        assert_eq!(
            streams[0].bit_depth,
            BlurayLpcmBitDepth::ProbeFailed {
                bytes_scanned: 4096,
                reason: BlurayLpcmBitDepthProbeFailure::ByteLimit {
                    missing_pids: vec![BlurayLpcmPidProbeFailure {
                        pid: 0x1100,
                        reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
                    }],
                },
            }
        );
    }

    #[test]
    fn lpcm_probe_report_records_probed_bit_depth() {
        let mut streams = vec![test_descriptor(BluRayAudioStreamKind::Primary, 0, 0x1100)
            .into_public()];
        let packet = test_ts_packet(
            0x1100,
            true,
            0,
            &test_lpcm_pes_prefix([0, 0, (3 << 4) | 1, 3 << 6]),
        );
        let mut reader = std::io::Cursor::new(packet);
        let report = read_lpcm_probe_window(
            &mut reader,
            &HashSet::from([0x1100]),
            &ProbeDepth::Exhaustive,
        );

        apply_lpcm_probe_report(&mut streams, &report);

        assert_eq!(
            streams[0].bit_depth,
            BlurayLpcmBitDepth::Probed {
                bit_depth: 24,
                scanned_bytes: TS_PACKET_SIZE as u64,
            }
        );
    }

    #[test]
    fn secondary_lpcm_streams_are_explicitly_not_probed() {
        let mut streams = vec![test_descriptor(BluRayAudioStreamKind::Secondary, 0, 0x1a00)
            .into_public()];

        initialize_lpcm_probe_statuses(&mut streams, &ProbeDepth::Exhaustive);

        assert_eq!(
            streams[0].bit_depth,
            BlurayLpcmBitDepth::NotProbed {
                reason: BlurayLpcmNotProbedReason::SecondaryStreamNotInMainTransport,
            }
        );
    }

    #[test]
    fn lpcm_probe_reports_incomplete_subheader_for_started_pid() {
        let pids = HashSet::from([0x1100]);
        let packet = test_ts_packet(0x1100, true, 0, &[0x00, 0x00, 0x01, 0xbd, 0x00]);
        let mut reader = std::io::Cursor::new(packet);

        let report = read_lpcm_probe_window(
            &mut reader,
            &pids,
            &ProbeDepth::Bounded {
                max_bytes: TS_PACKET_SIZE as u64,
                max_duration: std::time::Duration::from_secs(60),
            },
        );

        assert_eq!(
            report.pid_failures,
            vec![BlurayLpcmPidProbeFailure {
                pid: 0x1100,
                reason: BlurayLpcmPidProbeFailureReason::LpcmSubheaderIncomplete,
            }]
        );
    }

    #[test]
    fn lpcm_probe_reports_invalid_lpcm_header_for_reserved_codes() {
        let pids = HashSet::from([0x1100]);
        let packet = test_ts_packet(
            0x1100,
            true,
            0,
            &test_lpcm_pes_prefix([0, 0, (2 << 4) | 1, 1 << 6]),
        );
        let mut reader = std::io::Cursor::new(packet);

        let report = read_lpcm_probe_window(
            &mut reader,
            &pids,
            &ProbeDepth::Bounded {
                max_bytes: TS_PACKET_SIZE as u64,
                max_duration: std::time::Duration::from_secs(60),
            },
        );

        match &report.pid_failures[0].reason {
            BlurayLpcmPidProbeFailureReason::InvalidLpcmHeader { message } => {
                assert!(message.contains("reserved Blu-ray LPCM channel code"));
            }
            other => panic!("expected invalid LPCM header, got {other:?}"),
        }
    }

    #[test]
    fn maps_libbluray_events_to_io_errors() {
        let read_error = LibblurayEventError::new(BD_EVENT_READ_ERROR, 9).into_io_error();
        assert_eq!(read_error.kind(), io::ErrorKind::UnexpectedEof);

        let encrypted = LibblurayEventError::new(BD_EVENT_ENCRYPTED, 0).into_io_error();
        assert_eq!(encrypted.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn title_info_completion_prefers_queued_event_on_null() {
        let err = complete_title_info_after_events(
            std::ptr::null_mut(),
            3,
            0,
            Err(LibblurayEventError::new(BD_EVENT_READ_ERROR, 12)),
            |_| panic!("null title info must not be freed"),
        )
        .unwrap_err();

        assert!(err.contains("READ_ERROR"));
    }

    #[test]
    fn title_info_completion_reports_null_without_event() {
        let err = complete_title_info_after_events(
            std::ptr::null_mut(),
            3,
            0,
            Ok(()),
            |_| panic!("null title info must not be freed"),
        )
        .unwrap_err();

        assert!(err.contains("bd_get_title_info(3, 0) returned NULL"));
    }

    #[test]
    fn title_info_completion_frees_successful_pointer_when_event_follows() {
        let raw = NonNull::<ffi::BLURAY_TITLE_INFO>::dangling().as_ptr();
        let freed = std::cell::Cell::new(false);

        let err = complete_title_info_after_events(
            raw,
            1,
            0,
            Err(LibblurayEventError::new(BD_EVENT_ENCRYPTED, 0)),
            |ptr| {
                assert_eq!(ptr.as_ptr(), raw);
                freed.set(true);
            },
        )
        .unwrap_err();

        assert!(freed.get());
        assert!(err.contains("ENCRYPTED"));
    }

    #[test]
    fn drains_mock_event_queue_until_error_event() {
        let result = drain_queued_events([
            LibblurayEvent { event: 5, param: 1 },
            LibblurayEvent { event: BD_EVENT_READ_ERROR, param: 42 },
            LibblurayEvent { event: BD_EVENT_ERROR, param: 99 },
        ]);
        let err = result.expect_err("read error should stop the queue drain");
        assert_eq!(err.event, BD_EVENT_READ_ERROR);
        assert_eq!(err.param, 42);
    }

    #[test]
    fn read_success_returns_queued_event_error() {
        let event_result = drain_queued_events([LibblurayEvent {
            event: BD_EVENT_READ_ERROR,
            param: 7,
        }])
        .map_err(LibblurayEventError::into_io_error);

        let err = complete_read_after_events(188, event_result).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(err.to_string().contains("READ_ERROR"));
    }

    #[test]
    fn read_failure_prefers_queued_event_error() {
        let event_result = drain_queued_events([LibblurayEvent {
            event: BD_EVENT_ENCRYPTED,
            param: 0,
        }])
        .map_err(LibblurayEventError::into_io_error);

        let err = complete_read_after_events(-1, event_result).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn seek_success_returns_queued_event_error() {
        let event_result = drain_queued_events([LibblurayEvent {
            event: BD_EVENT_ERROR,
            param: 3,
        }])
        .map_err(LibblurayEventError::into_io_error);

        let err = complete_position_after_events("bd_seek", 4096, event_result).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("ERROR"));
    }

    #[test]
    fn chapter_seek_failure_prefers_queued_event_error() {
        let event_result = drain_queued_events([LibblurayEvent {
            event: BD_EVENT_ENCRYPTED,
            param: 0,
        }])
        .map_err(LibblurayEventError::into_io_error);

        let err =
            complete_position_after_events("bd_seek_chapter(2)", -1, event_result).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    struct FailingReader {
        message: &'static str,
    }

    impl FailingReader {
        fn new(message: &'static str) -> Self {
            Self { message }
        }
    }

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, self.message))
        }
    }

    struct CountingReader {
        data: std::io::Cursor<Vec<u8>>,
        read_calls: usize,
    }

    impl CountingReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data: std::io::Cursor::new(data),
                read_calls: 0,
            }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.read_calls += 1;
            self.data.read(buf)
        }
    }

    fn test_lpcm_pes_prefix(lpcm_header: [u8; 4]) -> Vec<u8> {
        let mut pes = vec![
            0x00, 0x00, 0x01, 0xbd, // PES start code + private stream id
            0x00, 0x00, // unspecified PES packet length for probing purposes
            0x80, 0x80, 0x05, // marker flags + five-byte optional header
            0x21, 0x00, 0x01, 0x00, 0x01, // placeholder PTS bytes
        ];
        pes.extend_from_slice(&lpcm_header);
        pes
    }

    fn test_ts_packet(
        pid: u16,
        payload_unit_start: bool,
        continuity_counter: u8,
        payload: &[u8],
    ) -> [u8; TS_PACKET_SIZE] {
        assert!(payload.len() <= 184);
        let mut packet = [0xff; TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        if payload_unit_start {
            packet[1] |= 0x40;
        }
        packet[2] = pid as u8;
        packet[3] = continuity_counter & 0x0f;

        if payload.len() == 184 {
            packet[3] |= 0x10;
            packet[4..].copy_from_slice(payload);
        } else {
            packet[3] |= 0x30;
            let adaptation_len = 183 - payload.len();
            packet[4] = adaptation_len as u8;
            let payload_offset = 5 + adaptation_len;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        }

        packet
    }

    fn test_descriptor(
        kind: BluRayAudioStreamKind,
        stream_index: u8,
        pid: u16,
    ) -> ClipAudioStreamDescriptor {
        ClipAudioStreamDescriptor {
            kind,
            stream_index,
            pid,
            coding: BluRayAudioCoding::Lpcm,
            sample_rate: Some(48_000),
            channels: Some(2),
            channel_layout: Some("stereo".to_string()),
            language: Some("eng".to_string()),
            raw_coding_type: BLURAY_STREAM_TYPE_AUDIO_LPCM,
            raw_format: 0x03,
            raw_rate: 0x01,
            raw_lang: *b"eng\0",
        }
    }
}

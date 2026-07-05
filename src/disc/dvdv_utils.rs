//! DVD-Video source detection and `DiscContents` mapping glue.
//!
//! Hybrid DVD-Audio/DVD-Video discs must stay on the DVD-Audio path. These
//! helpers therefore reject any source that the DVD-Audio detector accepts, and
//! also reject ISO images whose `AUDIO_TS/` inventory is non-empty.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use dvdvideo::disc::{DvdFile, DvdFileKind};
use dvdvideo::ifo::{AudioCodingMode, AudioStreamAttr, DvdTitleEntry, TtSrpt, VmgIfo, DVD_SECTOR};
use dvdvideo::{DvdDisc, VtsIfo};

use crate::convert::pipeline::dvda_demux::{
    parse_private_stream_1_packets_with_mode, DvdaPcmSubHeader, DvdaSubHeaderMode,
    DvdaSubstreamKind,
};

const VMG_MAGIC: &[u8; 12] = b"DVDVIDEO-VMG";

/// Check whether an ISO file contains a DVD-Video disc.
#[must_use]
pub fn is_dvdv_iso(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if crate::disc::dvda_utils::is_dvda_iso(path) {
        return false;
    }

    let Ok(disc) = DvdDisc::open(path) else {
        return false;
    };

    // A non-empty AUDIO_TS inventory indicates a hybrid or DVD-Audio disc. The
    // DVD-Audio pipeline owns that case even when VIDEO_TS is also valid.
    disc.audio_ts_files.is_empty()
}

/// Check whether a directory contains a DVD-Video disc root.
#[must_use]
pub fn is_dvdv_directory(path: &Path) -> bool {
    let Some((root, ifo)) = dvdv_directory_root_and_ifo(path) else {
        return false;
    };
    if crate::disc::dvda_utils::is_dvda_directory(&root) {
        return false;
    }
    if !audio_ts_absent_or_empty(&root) {
        return false;
    }
    ifo_has_vmg_magic(&ifo)
}

/// Check whether a path is any supported DVD-Video source.
#[must_use]
pub fn is_dvdv_source(path: &Path) -> bool {
    if path.is_file() {
        is_dvdv_iso(path)
    } else if path.is_dir() {
        is_dvdv_directory(path)
    } else {
        false
    }
}

/// Return the DVD root for a directory source.
///
/// Accepts either the disc root (`.../DISC/VIDEO_TS/VIDEO_TS.IFO`) or the
/// `VIDEO_TS` directory itself (`.../DISC/VIDEO_TS/VIDEO_TS.IFO`).
#[must_use]
pub fn dvdv_directory_root(path: &Path) -> Option<PathBuf> {
    dvdv_directory_root_and_ifo(path).map(|(root, _)| root)
}

/// Return the concrete `VIDEO_TS` directory for a directory source.
#[must_use]
pub fn dvdv_video_ts_dir(path: &Path) -> Option<PathBuf> {
    dvdv_directory_root_and_ifo(path).and_then(|(_, ifo)| ifo.parent().map(Path::to_path_buf))
}

/// Open either an ISO/block-device DVD-Video source or a filesystem DVD root.
pub fn open_dvdv_source(path: &Path) -> Result<DvdDisc, String> {
    if path.is_file() {
        if !is_dvdv_iso(path) {
            return Err(format!("Not a DVD-Video ISO: {}", path.display()));
        }
        return DvdDisc::open(path)
            .map_err(|err| format!("DVD-Video open failed for '{}': {err}", path.display()));
    }

    if path.is_dir() {
        let root = dvdv_directory_root(path).ok_or_else(|| {
            format!("Not a DVD-Video directory source: {}", path.display())
        })?;
        if crate::disc::dvda_utils::is_dvda_directory(&root) || !audio_ts_absent_or_empty(&root) {
            return Err(format!(
                "{} is a hybrid/DVD-Audio source; DVD-Audio handling must take precedence",
                root.display()
            ));
        }
        return open_dvdv_directory(&root);
    }

    Err(format!("Not a DVD-Video source: {}", path.display()))
}

/// Parse all VTS IFOs for either an ISO/block device or a filesystem DVD root.
pub fn parse_vts_ifos_for_source(path: &Path, disc: &DvdDisc) -> Result<Vec<(u8, VtsIfo)>, String> {
    let title_entries = parse_vmg_title_entries_for_source(path, disc)?;

    if path.is_dir() {
        let root = dvdv_directory_root(path).ok_or_else(|| {
            format!("Not a DVD-Video directory source: {}", path.display())
        })?;
        let video_ts = dvdv_video_ts_dir(path).ok_or_else(|| {
            format!("Not a DVD-Video VIDEO_TS directory: {}", path.display())
        })?;
        let mut out = Vec::new();
        for vts_number in 1..=disc.title_set_count {
            let Some(ifo_file) = disc.vtsi(vts_number) else {
                continue;
            };
            let ifo_path = resolve_child_case_insensitive(&video_ts, &ifo_file.name)
                .unwrap_or_else(|| video_ts.join(&ifo_file.name));
            let buf = fs::read(&ifo_path).map_err(|err| {
                format!("failed to read DVD-Video IFO '{}': {err}", ifo_path.display())
            })?;
            match VtsIfo::parse(&buf, vts_number) {
                Ok(mut vts) => {
                    vts.apply_vmg_title_entries(&title_entries);
                    apply_lpcm_packet_overrides_for_directory_source(
                        path,
                        disc,
                        vts_number,
                        &mut vts.audio_streams,
                    );
                    out.push((vts_number, vts));
                }
                Err(err) => log::warn!(
                    "Skipping DVD-Video VTS {} in {}: {}",
                    vts_number,
                    root.display(),
                    err
                ),
            }
        }
        return Ok(out);
    }

    let mut reader = File::open(path)
        .map_err(|err| format!("DVD-Video ISO open failed for '{}': {err}", path.display()))?;
    let mut out = Vec::new();
    for vts_number in 1..=disc.title_set_count {
        match disc.parse_vts(&mut reader, vts_number) {
            Ok(mut vts) => {
                vts.apply_vmg_title_entries(&title_entries);
                apply_lpcm_packet_overrides_for_iso_source(
                    &reader,
                    disc,
                    vts_number,
                    &mut vts.audio_streams,
                );
                out.push((vts_number, vts));
            }
            Err(err) => log::warn!(
                "Skipping DVD-Video VTS {} in {}: {}",
                vts_number,
                path.display(),
                err
            ),
        }
    }
    Ok(out)
}


const DVDV_LPCM_PROBE_SECTORS: usize = 500;
const DVDV_LPCM_SUBSTREAM_BASE: u8 = 0xA0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DvdVideoLpcmProbeFormat {
    sample_frequency_code: u8,
    sample_frequency: u32,
    quantization_code: u8,
    bit_depth: u32,
    channels: u8,
}

fn apply_lpcm_packet_overrides_for_directory_source(
    source: &Path,
    disc: &DvdDisc,
    vts_number: u8,
    audio_streams: &mut [AudioStreamAttr],
) {
    let Some(vob_file) = disc.vts_title_vob(vts_number, 1) else {
        return;
    };
    let Some(vob_path) = directory_video_ts_file_path(source, &vob_file.name) else {
        return;
    };

    apply_lpcm_packet_overrides_with_probe(vts_number, audio_streams, |stream, substream_id| {
        let result = File::open(&vob_path)
            .and_then(|mut reader| probe_lpcm_format_from_reader(&mut reader, substream_id));
        match &result {
            Ok(None) => log::debug!(
                "DVD-Video VTS {} LPCM stream {}: no packet sub-header found in first {} sectors of {}",
                vts_number,
                stream.stream_index,
                DVDV_LPCM_PROBE_SECTORS,
                vob_path.display()
            ),
            Err(err) => log::debug!(
                "DVD-Video VTS {} LPCM stream {}: packet sub-header probe failed for {}: {}",
                vts_number,
                stream.stream_index,
                vob_path.display(),
                err
            ),
            Ok(Some(_)) => {}
        }
        result
    });
}

fn apply_lpcm_packet_overrides_for_iso_source(
    reader: &File,
    disc: &DvdDisc,
    vts_number: u8,
    audio_streams: &mut [AudioStreamAttr],
) {
    let Some(vob_file) = disc.vts_title_vob(vts_number, 1) else {
        return;
    };
    let vob_offset = u64::from(vob_file.lba).saturating_mul(DVD_SECTOR as u64);

    apply_lpcm_packet_overrides_with_probe(vts_number, audio_streams, |stream, substream_id| {
        let result = probe_lpcm_format_from_iso_vob(reader, vob_offset, substream_id);
        match &result {
            Ok(None) => log::debug!(
                "DVD-Video VTS {} LPCM stream {}: no packet sub-header found in first {} sectors at LBA {}",
                vts_number,
                stream.stream_index,
                DVDV_LPCM_PROBE_SECTORS,
                vob_file.lba
            ),
            Err(err) => log::debug!(
                "DVD-Video VTS {} LPCM stream {}: packet sub-header probe failed at LBA {}: {}",
                vts_number,
                stream.stream_index,
                vob_file.lba,
                err
            ),
            Ok(Some(_)) => {}
        }
        result
    });
}

fn apply_lpcm_packet_overrides_with_probe<F>(
    vts_number: u8,
    audio_streams: &mut [AudioStreamAttr],
    mut probe: F,
) where
    F: FnMut(&AudioStreamAttr, u8) -> std::io::Result<Option<DvdVideoLpcmProbeFormat>>,
{
    for stream in audio_streams.iter_mut() {
        if stream.coding_mode != AudioCodingMode::Lpcm {
            continue;
        }

        let substream_id = DVDV_LPCM_SUBSTREAM_BASE.saturating_add(stream.stream_index);
        if let Ok(Some(format)) = probe(stream, substream_id) {
            apply_lpcm_packet_override(vts_number, stream, format);
        }
    }
}

fn probe_lpcm_format_from_iso_vob(
    reader: &File,
    vob_offset: u64,
    substream_id: u8,
) -> std::io::Result<Option<DvdVideoLpcmProbeFormat>> {
    let mut probe_reader = reader.try_clone()?;
    let original_position = probe_reader.stream_position()?;

    let probe_result = (|| {
        probe_reader.seek(SeekFrom::Start(vob_offset))?;
        probe_lpcm_format_from_reader(&mut probe_reader, substream_id)
    })();

    let restore_result = probe_reader.seek(SeekFrom::Start(original_position));
    match (probe_result, restore_result) {
        (Ok(format), Ok(_)) => Ok(format),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Ok(_)) => Err(err),
        (Err(err), Err(restore_err)) => {
            log::debug!(
                "DVD-Video LPCM ISO probe failed and original reader position restore also failed: {}",
                restore_err
            );
            Err(err)
        }
    }
}

fn probe_lpcm_format_from_reader<R: Read>(
    reader: &mut R,
    substream_id: u8,
) -> std::io::Result<Option<DvdVideoLpcmProbeFormat>> {
    let mut sector = [0_u8; DVD_SECTOR];
    for _ in 0..DVDV_LPCM_PROBE_SECTORS {
        match reader.read_exact(&mut sector) {
            Ok(()) => {
                if let Some(format) = probe_lpcm_format_from_sector(&sector, substream_id) {
                    return Ok(Some(format));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

fn probe_lpcm_format_from_sector(
    sector: &[u8; DVD_SECTOR],
    substream_id: u8,
) -> Option<DvdVideoLpcmProbeFormat> {
    let packets = match parse_private_stream_1_packets_with_mode(
        sector,
        DvdaSubHeaderMode::DvdVideo,
    ) {
        Ok(packets) => packets,
        Err(err) => {
            log::debug!("DVD-Video LPCM packet probe skipped sector: {}", err);
            return None;
        }
    };

    for packet in packets {
        if packet.sub_header.stream_id != substream_id {
            continue;
        }
        if packet.sub_header.kind() != DvdaSubstreamKind::Pcm {
            continue;
        }
        let Some(pcm) = packet.sub_header.pcm else {
            continue;
        };
        if let Some(format) = lpcm_probe_format_from_pcm_sub_header(pcm) {
            return Some(format);
        }
    }

    None
}

fn lpcm_probe_format_from_pcm_sub_header(pcm: DvdaPcmSubHeader) -> Option<DvdVideoLpcmProbeFormat> {
    Some(DvdVideoLpcmProbeFormat {
        sample_frequency_code: pcm.group1_sample_rate_code,
        sample_frequency: pcm.group1_sample_rate?,
        quantization_code: pcm.group1_bits_code,
        bit_depth: pcm.group1_bits?,
        channels: pcm.channel_count?,
    })
}

fn apply_lpcm_packet_override(
    vts_number: u8,
    stream: &mut AudioStreamAttr,
    format: DvdVideoLpcmProbeFormat,
) {
    let old = (
        stream.sample_frequency_code,
        stream.sample_frequency,
        stream.quantization_code,
        stream.bit_depth,
        stream.channels,
    );
    let new = (
        format.sample_frequency_code,
        Some(format.sample_frequency),
        format.quantization_code,
        Some(format.bit_depth),
        format.channels,
    );

    if old == new {
        return;
    }

    log::warn!(
        "DVD-Video VTS {} LPCM stream {}: overriding IFO audio attributes {:?}/{:?}/{}ch with packet sub-header {}/{}bit/{}ch",
        vts_number,
        stream.stream_index,
        stream.sample_frequency,
        stream.bit_depth,
        stream.channels,
        format.sample_frequency,
        format.bit_depth,
        format.channels
    );

    stream.sample_frequency_code = format.sample_frequency_code;
    stream.sample_frequency = Some(format.sample_frequency);
    stream.quantization_code = format.quantization_code;
    stream.bit_depth = Some(format.bit_depth);
    stream.channels = format.channels;
}

fn parse_vmg_title_entries_for_source(path: &Path, disc: &DvdDisc) -> Result<Vec<DvdTitleEntry>, String> {
    if path.is_dir() {
        let video_ts = dvdv_video_ts_dir(path).ok_or_else(|| {
            format!("Not a DVD-Video directory source: {}", path.display())
        })?;
        let vmgi_path = resolve_child_case_insensitive(&video_ts, "VIDEO_TS.IFO").ok_or_else(|| {
            format!("DVD-Video VMG IFO is missing from {}", video_ts.display())
        })?;
        let buf = fs::read(&vmgi_path).map_err(|err| {
            format!("failed to read DVD-Video VMG IFO '{}': {err}", vmgi_path.display())
        })?;
        let mat = VmgIfo::parse(&buf).map_err(|err| {
            format!("failed to parse DVD-Video VMG IFO '{}': {err}", vmgi_path.display())
        })?;
        let tt_off = (mat.tt_srpt_sector as usize)
            .checked_mul(DVD_SECTOR)
            .ok_or_else(|| format!("DVD-Video VMG TT_SRPT sector overflow in {}", vmgi_path.display()))?;
        let tt_buf = buf.get(tt_off..).ok_or_else(|| {
            format!("DVD-Video VMG TT_SRPT offset is past end of {}", vmgi_path.display())
        })?;
        let tt = TtSrpt::parse(tt_buf).map_err(|err| {
            format!("failed to parse DVD-Video VMG TT_SRPT '{}': {err}", vmgi_path.display())
        })?;
        return Ok(tt.entries);
    }

    let mut reader = File::open(path)
        .map_err(|err| format!("DVD-Video ISO open failed for '{}': {err}", path.display()))?;
    disc.parse_vmg_tt_srpt(&mut reader)
        .map(|tt| tt.entries)
        .map_err(|err| format!("failed to parse DVD-Video VMG TT_SRPT in '{}': {err}", path.display()))
}

/// Filesystem path for a VIDEO_TS member of a directory-backed source.
#[must_use]
pub fn directory_video_ts_file_path(source: &Path, file_name: &str) -> Option<PathBuf> {
    let video_ts = dvdv_video_ts_dir(source)?;
    Some(
        resolve_child_case_insensitive(&video_ts, file_name)
            .unwrap_or_else(|| video_ts.join(file_name)),
    )
}

/// Map a DVD-Video source into the unified disc browsing model.
pub fn map_dvdv_source(path: &Path) -> Result<crate::disc::DiscContents, String> {
    let disc = open_dvdv_source(path)?;
    let vts_ifos = parse_vts_ifos_for_source(path, &disc)?;
    let mut contents = crate::disc::dvdv_mapper::map_dvdv_disc(&disc, &vts_ifos, path);
    overlay_dvdv_sidecar_metadata(path, &mut contents);
    Ok(contents)
}

/// Load the DVD-Video TOML sidecar (if present) and overlay metadata onto
/// the disc contents so browse/convert views show tagged track titles and
/// album information before conversion starts.
fn overlay_dvdv_sidecar_metadata(source: &Path, contents: &mut crate::disc::DiscContents) {
    let sidecars = match crate::tui::command::load_dvdv_metadata_sidecar_presentations(source) {
        Ok(Some((_, sidecars))) => sidecars,
        Ok(None) => return,
        Err(err) => {
            match crate::tui::command::dvdv_metadata_sidecar_path_for_source(source) {
                Ok(sidecar_path) => log::warn!(
                    "Failed to load DVD-Video metadata sidecar {} while browsing {}: {}",
                    sidecar_path.display(),
                    source.display(),
                    err
                ),
                Err(path_err) => log::warn!(
                    "Failed to resolve DVD-Video metadata sidecar path while browsing {}: {}; load error: {}",
                    source.display(),
                    path_err,
                    err
                ),
            }
            return;
        }
    };

    let mut disc_album_title = contents.album_title.clone();
    let mut disc_album_artist = contents.album_artist.clone();
    let mut disc_genre = contents.genre.clone();
    let mut disc_year = contents.year.clone();

    for presentation in &mut contents.presentations {
        let Some(current_identity) = dvdv_browse_presentation_identity(presentation) else {
            continue;
        };
        let mut matching = sidecars.iter().filter(|sidecar| {
            let Some(ref stored_identity) = sidecar.source.presentation else {
                return false;
            };
            crate::tui::command::dvdv_presentation_identity_compatible(
                Some(stored_identity),
                Some(&current_identity),
            )
        });
        let Some(sidecar) = matching.next() else {
            continue;
        };
        if matching.next().is_some() {
            log::warn!(
                "Ignoring ambiguous DVD-Video metadata sidecar entries for VTS {} title {} audio stream {} while browsing {}",
                current_identity.vts_number,
                current_identity.title_number,
                current_identity.audio_stream_index,
                source.display()
            );
            continue;
        }

        if let Some(v) = dvdv_sidecar_value(&sidecar.album, &["ALBUM", "album"]) {
            presentation.album_title = Some(v.to_owned());
            disc_album_title = Some(v.to_owned());
        }
        if let Some(v) = dvdv_sidecar_value(
            &sidecar.album,
            &["ALBUMARTIST", "album_artist", "ARTIST", "artist"],
        ) {
            presentation.album_artist = Some(v.to_owned());
            disc_album_artist = Some(v.to_owned());
        }
        if let Some(v) = dvdv_sidecar_value(&sidecar.album, &["GENRE", "genre"]) {
            presentation.genre = Some(v.to_owned());
            disc_genre = Some(v.to_owned());
        }
        if let Some(v) = dvdv_sidecar_value(&sidecar.album, &["DATE", "date", "YEAR", "year"]) {
            presentation.year = Some(v.to_owned());
            disc_year = Some(v.to_owned());
        }

        for sidecar_track in &sidecar.tracks {
            let matching_track_index = sidecar_track
                .source_chapter
                .and_then(|chapter| {
                    presentation
                        .tracks
                        .iter()
                        .position(|track| track.number == u32::from(chapter))
                })
                .or_else(|| {
                    presentation
                        .tracks
                        .iter()
                        .position(|track| track.number == sidecar_track.number as u32)
                });
            let Some(track_index) = matching_track_index else {
                continue;
            };
            let disc_track = &mut presentation.tracks[track_index];
            if let Some(title) = dvdv_sidecar_value(&sidecar_track.tags, &["TITLE", "title"]) {
                disc_track.title = Some(title.to_owned());
            }
            if let Some(artist) = dvdv_sidecar_value(
                &sidecar_track.tags,
                &["ARTIST", "artist", "PERFORMER", "performer"],
            ) {
                disc_track.performer = Some(artist.to_owned());
            }
        }
    }

    contents.album_title = disc_album_title;
    contents.album_artist = disc_album_artist;
    contents.genre = disc_genre;
    contents.year = disc_year;
}

fn dvdv_browse_presentation_identity(
    presentation: &crate::disc::model::DiscPresentation,
) -> Option<crate::tui::command::DvdVideoPresentationIdentity> {
    let (vts_number, title_number, audio_stream_index) = presentation.id.dvd_video_parts()?;
    let durations = presentation
        .tracks
        .iter()
        .map(|track| track.duration_secs)
        .collect::<Option<Vec<_>>>();
    Some(crate::tui::command::DvdVideoPresentationIdentity {
        vts_number,
        title_number,
        audio_stream_index,
        angle_number: dvdv_browse_presentation_angle_number(presentation),
        track_count: Some(presentation.tracks.len()),
        duration_fingerprint: durations
            .as_deref()
            .map(crate::tui::command::dvdv_track_duration_fingerprint_from_secs),
    })
}

fn dvdv_browse_presentation_angle_number(
    presentation: &crate::disc::model::DiscPresentation,
) -> Option<u8> {
    // The current browse model materializes only the default angle, and older
    // `PresentationId` values do not carry the title angle count. The mapper
    // annotates multi-angle titles on every track, so reproduce the materializer
    // identity semantics here: single-angle title => sparse angle, multi-angle
    // default preview => explicit angle 1. This keeps an angle-less TOML sidecar
    // from appearing in the preview when conversion would reject it.
    presentation
        .tracks
        .iter()
        .any(|track| track.format_note.as_deref().is_some_and(|note| note.contains("Default angle")))
        .then_some(1)
}

fn dvdv_sidecar_value<'a>(
    values: &'a std::collections::BTreeMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter().find_map(|key| values.get(*key).map(String::as_str))
}

fn open_dvdv_directory(root: &Path) -> Result<DvdDisc, String> {
    let video_ts = dvdv_video_ts_dir(root).ok_or_else(|| {
        format!("Not a DVD-Video VIDEO_TS directory: {}", root.display())
    })?;
    let mut video_ts_files = Vec::new();
    let entries = fs::read_dir(&video_ts).map_err(|err| {
        format!("failed to read DVD-Video directory '{}': {err}", video_ts.display())
    })?;

    for entry in entries {
        let entry = entry.map_err(|err| {
            format!("failed to read DVD-Video directory entry in '{}': {err}", video_ts.display())
        })?;
        let path = entry.path();
        let meta = entry.metadata().map_err(|err| {
            format!("failed to stat DVD-Video file '{}': {err}", path.display())
        })?;
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()).map(str::to_string) else {
            continue;
        };
        let Some(kind) = classify_video_ts_name(&name) else {
            continue;
        };
        video_ts_files.push(build_directory_dvd_file(name, kind, meta.len()));
    }

    video_ts_files.sort_by_key(|file| (file.title_set, file.vob_index, sort_kind_priority(file.kind)));
    if !video_ts_files.iter().any(|file| file.kind == DvdFileKind::Vmgi) {
        return Err(format!("VIDEO_TS.IFO is missing from {}", video_ts.display()));
    }

    let title_set_count = video_ts_files
        .iter()
        .map(|file| file.title_set)
        .filter(|title_set| *title_set > 0)
        .max()
        .unwrap_or(0);
    let volume_id = root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("DVDVIDEO")
        .to_string();

    Ok(DvdDisc {
        volume_id,
        title_set_count,
        video_ts_files,
        audio_ts_files: Vec::new(),
    })
}

fn build_directory_dvd_file(name: String, kind: DvdFileKind, size: u64) -> DvdFile {
    let (title_set, vob_index) = match kind {
        DvdFileKind::Vmgi | DvdFileKind::VmgMenu | DvdFileKind::VmgiBup => (0, 0),
        DvdFileKind::Vtsi(ts) | DvdFileKind::VtsMenu(ts) | DvdFileKind::VtsiBup(ts) => (ts, 0),
        DvdFileKind::VtsTitle { ts, vob } => (ts, vob),
    };
    DvdFile {
        kind,
        name,
        // Directory-backed VOBs are read by filesystem path, not ISO LBA.
        lba: 0,
        size,
        title_set,
        vob_index,
    }
}

fn classify_video_ts_name(name: &str) -> Option<DvdFileKind> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "VIDEO_TS.IFO" => Some(DvdFileKind::Vmgi),
        "VIDEO_TS.VOB" => Some(DvdFileKind::VmgMenu),
        "VIDEO_TS.BUP" => Some(DvdFileKind::VmgiBup),
        _ => parse_vts_file_name(&upper),
    }
}

fn parse_vts_file_name(upper: &str) -> Option<DvdFileKind> {
    let rest = upper.strip_prefix("VTS_")?;
    if rest.len() != 8 || rest.as_bytes().get(2) != Some(&b'_') || rest.as_bytes().get(4) != Some(&b'.') {
        return None;
    }
    let ts = rest.get(0..2)?.parse::<u8>().ok()?;
    let vob = rest.get(3..4)?.parse::<u8>().ok()?;
    let ext = rest.get(5..)?;
    if !(1..=99).contains(&ts) {
        return None;
    }
    match (vob, ext) {
        (0, "IFO") => Some(DvdFileKind::Vtsi(ts)),
        (0, "VOB") => Some(DvdFileKind::VtsMenu(ts)),
        (0, "BUP") => Some(DvdFileKind::VtsiBup(ts)),
        (1..=9, "VOB") => Some(DvdFileKind::VtsTitle { ts, vob }),
        _ => None,
    }
}

fn sort_kind_priority(kind: DvdFileKind) -> u8 {
    match kind {
        DvdFileKind::Vmgi | DvdFileKind::Vtsi(_) => 0,
        DvdFileKind::VmgMenu | DvdFileKind::VtsMenu(_) => 1,
        DvdFileKind::VtsTitle { .. } => 2,
        DvdFileKind::VmgiBup | DvdFileKind::VtsiBup(_) => 3,
    }
}

fn dvdv_directory_root_and_ifo(path: &Path) -> Option<(PathBuf, PathBuf)> {
    if !path.is_dir() {
        return None;
    }

    if path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.eq_ignore_ascii_case("VIDEO_TS"))
        .unwrap_or(false)
    {
        let ifo = resolve_child_case_insensitive(path, "VIDEO_TS.IFO")?;
        if ifo.is_file() {
            let root = path.parent().unwrap_or(path).to_path_buf();
            return Some((root, ifo));
        }
    }

    let video_ts = resolve_child_case_insensitive(path, "VIDEO_TS")?;
    if !video_ts.is_dir() {
        return None;
    }
    let ifo = resolve_child_case_insensitive(&video_ts, "VIDEO_TS.IFO")?;
    if ifo.is_file() {
        Some((path.to_path_buf(), ifo))
    } else {
        None
    }
}

/// Resolve a child entry by DVD name without assuming canonical uppercase names.
///
/// Many copied DVD filesystems preserve lowercase or mixed-case names on
/// case-sensitive hosts. Prefer an exact match when present, then fall back to
/// ASCII case-insensitive matching because DVD-Video file names are ASCII by
/// specification.
fn resolve_child_case_insensitive(parent: &Path, wanted: &str) -> Option<PathBuf> {
    let exact = parent.join(wanted);
    if exact.exists() {
        return Some(exact);
    }

    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .map(|candidate| candidate.eq_ignore_ascii_case(wanted))
            .unwrap_or(false)
        {
            return Some(entry.path());
        }
    }
    None
}

fn ifo_has_vmg_magic(path: &Path) -> bool {
    let mut magic = [0u8; 12];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map(|()| magic == *VMG_MAGIC)
        .unwrap_or(false)
}

fn audio_ts_absent_or_empty(root: &Path) -> bool {
    let Some(audio_ts) = resolve_child_case_insensitive(root, "AUDIO_TS") else {
        return true;
    };
    let Ok(meta) = fs::metadata(&audio_ts) else {
        return true;
    };
    if !meta.is_dir() {
        return false;
    }
    fs::read_dir(audio_ts)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("dvdv-utils-{label}-{}-{nanos}", std::process::id()))
    }


    #[test]
    fn dvdv_sidecar_overlay_uses_internal_tag_keys_from_toml_parser() {
        use crate::disc::model::{
            AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscFormat,
            DiscPresentation, DiscTrack, FormatProvenance, PresentationId,
        };

        let root = unique_dir("overlay-internal-keys");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("FILLMORE_EAST.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("FILLMORE_EAST.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album_artist = "Neil Young & Crazy Horse"
album = "Live at the Fillmore East"
genre = "Rock"
date = "1971"
[[presentations.tracks]]
number = 1
source_title = 1
source_chapter = 1
title = "Everybody Knows This Is Nowhere"
artist = "Neil Young & Crazy Horse"
[[presentations.tracks]]
number = 2
source_title = 1
source_chapter = 2
title = "Winterlong"
artist = "Neil Young"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = DiscContents {
            format: DiscFormat::DvdVideo,
            label: "Fixture".to_string(),
            source_path: iso.clone(),
            presentations: vec![DiscPresentation {
                id: PresentationId::dvd_video(1, 1, 0),
                label: "VTS 01 Title 01 Stream 1".to_string(),
                format: AudioPresentationFormat {
                    codec: Some("LPCM".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("Stereo".to_string()),
                    lossless: true,
                    provenance: FormatProvenance::IfoAttributes,
                },
                tracks: vec![
                    DiscTrack {
                        number: 1,
                        title: Some("Chapter 1".to_string()),
                        performer: None,
                        duration_secs: Some(60.0),
                        format_note: None,
                    },
                    DiscTrack {
                        number: 2,
                        title: Some("Chapter 2".to_string()),
                        performer: None,
                        duration_secs: Some(61.0),
                        format_note: None,
                    },
                ],
                total_duration_secs: 121.0,
                album_title: None,
                album_artist: None,
                genre: None,
                year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        };

        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.album_title.as_deref(), Some("Live at the Fillmore East"));
        assert_eq!(presentation.album_artist.as_deref(), Some("Neil Young & Crazy Horse"));
        assert_eq!(presentation.genre.as_deref(), Some("Rock"));
        assert_eq!(presentation.year.as_deref(), Some("1971"));
        assert_eq!(contents.album_title.as_deref(), Some("Live at the Fillmore East"));
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Everybody Knows This Is Nowhere"));
        assert_eq!(presentation.tracks[0].performer.as_deref(), Some("Neil Young & Crazy Horse"));
        assert_eq!(presentation.tracks[1].title.as_deref(), Some("Winterlong"));
        assert_eq!(presentation.tracks[1].performer.as_deref(), Some("Neil Young"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_sidecar_overlay_prefers_source_chapter_over_logical_track_number() {
        use crate::disc::model::{
            AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscFormat,
            DiscPresentation, DiscTrack, FormatProvenance, PresentationId,
        };

        let root = unique_dir("overlay-source-chapter");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("ODD_NUMBERING.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("ODD_NUMBERING.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album = "Authored Chapter Fixture"
[[presentations.tracks]]
number = 10
source_title = 1
source_chapter = 1
title = "Authored Chapter One"
[[presentations.tracks]]
number = 20
source_title = 1
source_chapter = 2
title = "Authored Chapter Two"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = DiscContents {
            format: DiscFormat::DvdVideo,
            label: "Fixture".to_string(),
            source_path: iso.clone(),
            presentations: vec![DiscPresentation {
                id: PresentationId::dvd_video(1, 1, 0),
                label: "VTS 01 Title 01 Stream 1".to_string(),
                format: AudioPresentationFormat {
                    codec: Some("LPCM".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("Stereo".to_string()),
                    lossless: true,
                    provenance: FormatProvenance::IfoAttributes,
                },
                tracks: vec![
                    DiscTrack {
                        number: 1,
                        title: Some("Chapter 1".to_string()),
                        performer: None,
                        duration_secs: Some(60.0),
                        format_note: None,
                    },
                    DiscTrack {
                        number: 2,
                        title: Some("Chapter 2".to_string()),
                        performer: None,
                        duration_secs: Some(61.0),
                        format_note: None,
                    },
                ],
                total_duration_secs: 121.0,
                album_title: None,
                album_artist: None,
                genre: None,
                year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        };

        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Authored Chapter One"));
        assert_eq!(presentation.tracks[1].title.as_deref(), Some("Authored Chapter Two"));

        let _ = fs::remove_dir_all(root);
    }


    fn dvdv_test_contents_with_track_notes(
        iso: PathBuf,
        track_notes: Vec<Option<String>>,
    ) -> crate::disc::model::DiscContents {
        use crate::disc::model::{
            AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscFormat,
            DiscPresentation, DiscTrack, FormatProvenance, PresentationId,
        };
        let tracks = track_notes
            .into_iter()
            .enumerate()
            .map(|(idx, format_note)| DiscTrack {
                number: idx.saturating_add(1) as u32,
                title: Some(format!("Chapter {}", idx + 1)),
                performer: None,
                duration_secs: Some(60.0 + idx as f64),
                format_note,
            })
            .collect::<Vec<_>>();
        DiscContents {
            format: DiscFormat::DvdVideo,
            label: "Fixture".to_string(),
            source_path: iso,
            presentations: vec![DiscPresentation {
                id: PresentationId::dvd_video(1, 1, 0),
                label: "VTS 01 Title 01 Stream 1".to_string(),
                format: AudioPresentationFormat {
                    codec: Some("LPCM".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("Stereo".to_string()),
                    lossless: true,
                    provenance: FormatProvenance::IfoAttributes,
                },
                total_duration_secs: tracks.iter().filter_map(|track| track.duration_secs).sum(),
                tracks,
                album_title: None,
                album_artist: None,
                genre: None,
                year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary { description: String::new() },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }


    #[test]
    fn dvdv_sidecar_overlay_replaces_existing_disc_level_album_fields() {
        let root = unique_dir("overlay-replaces-disc-album-fields");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("REPLACE_ALBUM.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("REPLACE_ALBUM.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album = "Sidecar Album"
album_artist = "Sidecar Artist"
genre = "Sidecar Genre"
date = "1971"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "Sidecar Chapter One"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = dvdv_test_contents_with_track_notes(iso.clone(), vec![None, None]);
        contents.album_title = Some("Probe Album".to_string());
        contents.album_artist = Some("Probe Artist".to_string());
        contents.genre = Some("Probe Genre".to_string());
        contents.year = Some("2000".to_string());

        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        assert_eq!(contents.album_title.as_deref(), Some("Sidecar Album"));
        assert_eq!(contents.album_artist.as_deref(), Some("Sidecar Artist"));
        assert_eq!(contents.genre.as_deref(), Some("Sidecar Genre"));
        assert_eq!(contents.year.as_deref(), Some("1971"));

        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        assert_eq!(contents.album_title.as_deref(), Some("Sidecar Album"));
        assert_eq!(contents.album_artist.as_deref(), Some("Sidecar Artist"));
        assert_eq!(contents.genre.as_deref(), Some("Sidecar Genre"));
        assert_eq!(contents.year.as_deref(), Some("1971"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_sidecar_overlay_rejects_duration_fingerprint_mismatch() {
        let root = unique_dir("overlay-fingerprint-mismatch");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("STALE.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("STALE.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
duration_fingerprint = "dvdv-ms-v1:2:stale"
[presentations.album]
album = "Stale Metadata"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "Wrong Chapter One"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = dvdv_test_contents_with_track_notes(iso.clone(), vec![None, None]);
        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.album_title, None);
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Chapter 1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_sidecar_overlay_ignores_ambiguous_duplicate_presentations() {
        let root = unique_dir("overlay-duplicate-presentations");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("DUPLICATE.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("DUPLICATE.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "first"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album = "First Match"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "First Chapter One"

[[presentations]]
id = "second"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album = "Second Match"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "Second Chapter One"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = dvdv_test_contents_with_track_notes(iso.clone(), vec![None, None]);
        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.album_title, None);
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Chapter 1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_sidecar_overlay_rejects_angle_less_sidecar_for_multi_angle_preview() {
        let root = unique_dir("overlay-angleless-multi-angle");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("MULTI_ANGLE.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("MULTI_ANGLE.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
track_count = 2
[presentations.album]
album = "Angle-Less Metadata"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "Angle-Less Chapter One"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = dvdv_test_contents_with_track_notes(
            iso.clone(),
            vec![
                Some("Default angle 1 of 2".to_string()),
                Some("Default angle 1 of 2".to_string()),
            ],
        );
        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.album_title, None);
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Chapter 1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_sidecar_overlay_accepts_explicit_angle_one_for_multi_angle_preview() {
        let root = unique_dir("overlay-explicit-angle-one");
        fs::create_dir_all(&root).expect("create fixture dir");
        let iso = root.join("MULTI_ANGLE_OK.ISO");
        fs::write(&iso, b"not a real iso; only used for sidecar path resolution")
            .expect("write source fixture");
        fs::write(
            root.join("MULTI_ANGLE_OK.ISO.dvdvideo.metadata.toml"),
            r#"schema_version = 1
format = "tonepoet-dvdvideo-metadata"

[[presentations]]
id = "vts1-title1-stream0-angle1"
[presentations.source]
vts = 1
title = 1
audio_stream = 0
angle = 1
track_count = 2
[presentations.album]
album = "Angle One Metadata"
[[presentations.tracks]]
number = 1
source_chapter = 1
title = "Angle One Chapter One"
"#,
        )
        .expect("write TOML sidecar");

        let mut contents = dvdv_test_contents_with_track_notes(
            iso.clone(),
            vec![
                Some("Default angle 1 of 2".to_string()),
                Some("Default angle 1 of 2".to_string()),
            ],
        );
        overlay_dvdv_sidecar_metadata(&iso, &mut contents);
        let presentation = &contents.presentations[0];
        assert_eq!(presentation.album_title.as_deref(), Some("Angle One Metadata"));
        assert_eq!(presentation.tracks[0].title.as_deref(), Some("Angle One Chapter One"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvdv_directory_detection_accepts_lowercase_video_ts_names() {
        let root = unique_dir("lowercase-video-ts");
        let video_ts = root.join("video_ts");
        fs::create_dir_all(&video_ts).expect("create lowercase VIDEO_TS");
        fs::write(video_ts.join("video_ts.ifo"), VMG_MAGIC).expect("write lowercase VMG IFO");

        assert!(dvdv_directory_root(&root).is_some());
        assert!(dvdv_video_ts_dir(&root)
            .expect("VIDEO_TS dir")
            .ends_with("video_ts"));
        assert!(directory_video_ts_file_path(&root, "VIDEO_TS.IFO")
            .expect("VMG IFO")
            .ends_with("video_ts.ifo"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dvdv_directory_detection_rejects_lowercase_nonempty_audio_ts_hybrid() {
        let root = unique_dir("lowercase-hybrid");
        let video_ts = root.join("video_ts");
        let audio_ts = root.join("audio_ts");
        fs::create_dir_all(&video_ts).expect("create VIDEO_TS");
        fs::create_dir_all(&audio_ts).expect("create AUDIO_TS");
        fs::write(video_ts.join("video_ts.ifo"), VMG_MAGIC).expect("write VMG IFO");
        fs::write(audio_ts.join("audio_ts.ifo"), b"DVD-Audio marker").expect("write audio marker");

        assert!(!is_dvdv_directory(&root));

        let _ = fs::remove_dir_all(&root);
    }

    fn lpcm_stream(
        sample_frequency_code: u8,
        quantization_code: u8,
        channels: u8,
    ) -> AudioStreamAttr {
        AudioStreamAttr {
            coding_mode: AudioCodingMode::Lpcm,
            multichannel_extension: false,
            audio_type: 0,
            sample_frequency_code,
            sample_frequency: match sample_frequency_code {
                0 => Some(48_000),
                1 => Some(96_000),
                _ => None,
            },
            quantization_code,
            bit_depth: match quantization_code {
                0 => Some(16),
                1 => Some(20),
                2 => Some(24),
                _ => None,
            },
            channels,
            language: Some("en".to_string()),
            stream_index: 0,
        }
    }

    fn dvd_video_pcm_header(
        sample_frequency_code: u8,
        sample_frequency: Option<u32>,
        quantization_code: u8,
        bit_depth: Option<u32>,
        channel_count: Option<u8>,
    ) -> DvdaPcmSubHeader {
        DvdaPcmSubHeader {
            first_audio_frame: 0,
            group1_bits_code: quantization_code,
            group2_bits_code: 0,
            group1_sample_rate_code: sample_frequency_code,
            group2_sample_rate_code: 0,
            group1_bits: bit_depth,
            group2_bits: None,
            group1_sample_rate: sample_frequency,
            group2_sample_rate: None,
            channel_count,
            channel_assignment: 1,
            cci: 0,
        }
    }

    fn lpcm_stream_with_index(
        stream_index: u8,
        sample_frequency_code: u8,
        quantization_code: u8,
        channels: u8,
    ) -> AudioStreamAttr {
        let mut stream = lpcm_stream(sample_frequency_code, quantization_code, channels);
        stream.stream_index = stream_index;
        stream
    }

    fn non_lpcm_stream(coding_mode: AudioCodingMode, stream_index: u8) -> AudioStreamAttr {
        AudioStreamAttr {
            coding_mode,
            multichannel_extension: false,
            audio_type: 0,
            sample_frequency_code: 0,
            sample_frequency: Some(48_000),
            quantization_code: 0,
            bit_depth: None,
            channels: 6,
            language: Some("en".to_string()),
            stream_index,
        }
    }

    fn ac3_stream(stream_index: u8) -> AudioStreamAttr {
        non_lpcm_stream(AudioCodingMode::Ac3, stream_index)
    }

    fn dts_stream(stream_index: u8) -> AudioStreamAttr {
        non_lpcm_stream(AudioCodingMode::Dts, stream_index)
    }

    fn probed_format(
        sample_frequency_code: u8,
        sample_frequency: u32,
        quantization_code: u8,
        bit_depth: u32,
        channels: u8,
    ) -> DvdVideoLpcmProbeFormat {
        DvdVideoLpcmProbeFormat {
            sample_frequency_code,
            sample_frequency,
            quantization_code,
            bit_depth,
            channels,
        }
    }

    fn dvd_video_lpcm_format_byte(
        sample_frequency_code: u8,
        quantization_code: u8,
        channels: u8,
    ) -> u8 {
        (quantization_code << 6) | ((sample_frequency_code & 0x03) << 4) | ((channels - 1) & 0x07)
    }

    fn sector_with_dvd_video_lpcm_packet(
        substream_id: u8,
        sample_frequency_code: u8,
        quantization_code: u8,
        channels: u8,
    ) -> [u8; DVD_SECTOR] {
        let mut sector = [0_u8; DVD_SECTOR];
        sector[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
        sector[13] = 0;

        let packet_offset = 14;
        let sub_header = [
            substream_id,
            0x04,
            0x00,
            0x58,
            0x03,
            dvd_video_lpcm_format_byte(sample_frequency_code, quantization_code, channels),
            0x7F,
        ];
        let payload = [0xAA, 0xBB, 0xCC, 0xDD];
        let pes_len = 3 + sub_header.len() + payload.len();

        sector[packet_offset..packet_offset + 4]
            .copy_from_slice(&[0x00, 0x00, 0x01, 0xBD]);
        sector[packet_offset + 4..packet_offset + 6]
            .copy_from_slice(&(pes_len as u16).to_be_bytes());
        sector[packet_offset + 6] = 0x80;
        sector[packet_offset + 7] = 0x80;
        sector[packet_offset + 8] = 0;
        let sub_header_offset = packet_offset + 9;
        sector[sub_header_offset..sub_header_offset + sub_header.len()]
            .copy_from_slice(&sub_header);
        let payload_offset = sub_header_offset + sub_header.len();
        sector[payload_offset..payload_offset + payload.len()].copy_from_slice(&payload);
        sector
    }

    fn write_temp_file(label: &str, bytes: &[u8]) -> PathBuf {
        let dir = unique_dir(label);
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("fixture.bin");
        fs::write(&path, bytes).expect("write temp fixture");
        path
    }

    fn write_directory_and_iso_parity_fixtures(
        label: &str,
        sectors: &[[u8; DVD_SECTOR]],
        iso_lba: u32,
    ) -> (PathBuf, PathBuf, u32) {
        let root = unique_dir(label);
        let video_ts = root.join("VIDEO_TS");
        fs::create_dir_all(&video_ts).expect("create VIDEO_TS");
        fs::write(video_ts.join("VIDEO_TS.IFO"), VMG_MAGIC).expect("write VMG IFO");

        let mut vob_bytes = Vec::with_capacity(sectors.len() * DVD_SECTOR);
        for sector in sectors {
            vob_bytes.extend_from_slice(sector);
        }
        // Use lowercase on disk while callers ask for the canonical DVD name.
        // This keeps the parity test sensitive to directory VOB name resolution.
        fs::write(video_ts.join("vts_01_1.vob"), &vob_bytes).expect("write directory VOB");

        let iso_path = root.join("fixture.iso");
        let vob_offset = iso_lba as usize * DVD_SECTOR;
        let mut iso_bytes = vec![0_u8; vob_offset + vob_bytes.len()];
        iso_bytes[vob_offset..vob_offset + vob_bytes.len()].copy_from_slice(&vob_bytes);
        fs::write(&iso_path, iso_bytes).expect("write ISO-like fixture");

        (root, iso_path, iso_lba)
    }

    fn disc_with_title_vob(name: &str, lba: u32) -> DvdDisc {
        DvdDisc {
            volume_id: "TEST".to_string(),
            title_set_count: 1,
            video_ts_files: vec![DvdFile {
                kind: DvdFileKind::VtsTitle { ts: 1, vob: 1 },
                name: name.to_string(),
                lba,
                size: DVD_SECTOR as u64,
                title_set: 1,
                vob_index: 1,
            }],
            audio_ts_files: Vec::new(),
        }
    }

    fn disc_without_title_vob() -> DvdDisc {
        DvdDisc {
            volume_id: "TEST".to_string(),
            title_set_count: 1,
            video_ts_files: Vec::new(),
            audio_ts_files: Vec::new(),
        }
    }

    struct CountingReader {
        bytes: Vec<u8>,
        offset: usize,
        read_calls: usize,
        bytes_read: usize,
    }

    impl CountingReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                offset: 0,
                read_calls: 0,
                bytes_read: 0,
            }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.read_calls += 1;
            if self.offset >= self.bytes.len() {
                return Ok(0);
            }
            let n = out.len().min(self.bytes.len() - self.offset);
            out[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
            self.offset += n;
            self.bytes_read += n;
            Ok(n)
        }
    }

    #[test]
    fn lpcm_probe_format_uses_demux_pcm_sub_header_fields() {
        let format = lpcm_probe_format_from_pcm_sub_header(dvd_video_pcm_header(
            1,
            Some(96_000),
            2,
            Some(24),
            Some(2),
        ))
        .expect("complete DVD-Video LPCM packet format");

        assert_eq!(format.sample_frequency_code, 1);
        assert_eq!(format.sample_frequency, 96_000);
        assert_eq!(format.quantization_code, 2);
        assert_eq!(format.bit_depth, 24);
        assert_eq!(format.channels, 2);
    }

    #[test]
    fn lpcm_probe_format_requires_demux_channel_count() {
        assert!(lpcm_probe_format_from_pcm_sub_header(dvd_video_pcm_header(
            1,
            Some(96_000),
            2,
            Some(24),
            None,
        ))
        .is_none());
    }

    #[test]
    fn probe_uses_dvd_video_demux_parser_for_matching_substream() {
        let sector = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);

        let format = probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE)
            .expect("matching DVD-Video LPCM substream should probe");

        assert_eq!(format, probed_format(1, 96_000, 2, 24, 2));
    }

    #[test]
    fn probe_accepts_seven_channel_dvd_video_lpcm_packet() {
        let sector = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 0, 2, 7);

        let format = probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE)
            .expect("7-channel DVD-Video LPCM substream should probe");

        assert_eq!(format.channels, 7);
        assert_eq!(format.bit_depth, 24);
        assert_eq!(format.sample_frequency, 48_000);
    }

    #[test]
    fn probe_accepts_eight_channel_dvd_video_lpcm_packet() {
        let sector = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 0, 2, 8);

        let format = probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE)
            .expect("8-channel DVD-Video LPCM substream should probe");

        assert_eq!(format.channels, 8);
        assert_eq!(format.bit_depth, 24);
        assert_eq!(format.sample_frequency, 48_000);
    }

    #[test]
    fn probe_ignores_other_lpcm_substreams() {
        let sector = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE + 1, 1, 2, 2);

        assert!(probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE).is_none());
        assert_eq!(
            probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE + 1),
            Some(probed_format(1, 96_000, 2, 24, 2))
        );
    }

    #[test]
    fn probe_returns_none_when_packet_is_after_500_reader_sectors() {
        let mut bytes = vec![0_u8; DVD_SECTOR * (DVDV_LPCM_PROBE_SECTORS + 1)];
        let packet = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);
        let packet_offset = DVD_SECTOR * DVDV_LPCM_PROBE_SECTORS;
        bytes[packet_offset..packet_offset + DVD_SECTOR].copy_from_slice(&packet);
        let mut reader = CountingReader::new(bytes);

        assert!(matches!(
            probe_lpcm_format_from_reader(&mut reader, DVDV_LPCM_SUBSTREAM_BASE),
            Ok(None)
        ));
        assert_eq!(reader.read_calls, DVDV_LPCM_PROBE_SECTORS);
        assert_eq!(reader.bytes_read, DVD_SECTOR * DVDV_LPCM_PROBE_SECTORS);
    }

    #[test]
    fn probe_finds_packet_in_500th_reader_sector() {
        let mut bytes = vec![0_u8; DVD_SECTOR * DVDV_LPCM_PROBE_SECTORS];
        let packet = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);
        let packet_offset = DVD_SECTOR * (DVDV_LPCM_PROBE_SECTORS - 1);
        bytes[packet_offset..packet_offset + DVD_SECTOR].copy_from_slice(&packet);
        let mut reader = CountingReader::new(bytes);

        let format = probe_lpcm_format_from_reader(&mut reader, DVDV_LPCM_SUBSTREAM_BASE)
            .expect("reader probe should not fail")
            .expect("500th sector is inside scan limit");

        assert_eq!(format, probed_format(1, 96_000, 2, 24, 2));
        assert_eq!(reader.read_calls, DVDV_LPCM_PROBE_SECTORS);
        assert_eq!(reader.bytes_read, DVD_SECTOR * DVDV_LPCM_PROBE_SECTORS);
    }

    #[test]
    fn probe_does_not_decode_subheader_past_pes_end() {
        let sector = sector_with_short_private_stream_and_lpcm_like_bytes_after_end();

        assert!(probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE).is_none());
    }

    #[test]
    fn probe_restores_iso_reader_position_after_success() {
        let mut bytes = vec![0_u8; DVD_SECTOR * 2];
        let packet = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);
        bytes[DVD_SECTOR..DVD_SECTOR * 2].copy_from_slice(&packet);
        let path = write_temp_file("iso-position-success", &bytes);
        let mut reader = File::open(&path).expect("open ISO fixture");
        reader.seek(SeekFrom::Start(17)).expect("seek before probe");

        let format = probe_lpcm_format_from_iso_vob(
            &reader,
            DVD_SECTOR as u64,
            DVDV_LPCM_SUBSTREAM_BASE,
        )
        .expect("probe succeeds")
        .expect("matching packet");

        assert_eq!(format, probed_format(1, 96_000, 2, 24, 2));
        assert_eq!(reader.stream_position().expect("reader position"), 17);
        let _ = fs::remove_dir_all(path.parent().expect("temp parent"));
    }

    #[test]
    fn probe_restores_iso_reader_position_after_miss() {
        let bytes = vec![0_u8; DVD_SECTOR * DVDV_LPCM_PROBE_SECTORS];
        let path = write_temp_file("iso-position-miss", &bytes);
        let mut reader = File::open(&path).expect("open ISO fixture");
        reader.seek(SeekFrom::Start(19)).expect("seek before probe");

        assert!(matches!(
            probe_lpcm_format_from_iso_vob(&reader, 0, DVDV_LPCM_SUBSTREAM_BASE),
            Ok(None)
        ));
        assert_eq!(reader.stream_position().expect("reader position"), 19);
        let _ = fs::remove_dir_all(path.parent().expect("temp parent"));
    }

    #[cfg(unix)]
    #[test]
    fn probe_restores_iso_reader_position_after_io_error() {
        let dir = unique_dir("iso-position-io-error");
        fs::create_dir_all(&dir).expect("create temp dir");
        let mut reader = File::open(&dir).expect("open directory as File");
        reader.seek(SeekFrom::Start(0)).expect("seek before probe");

        let result = probe_lpcm_format_from_iso_vob(&reader, 0, DVDV_LPCM_SUBSTREAM_BASE);

        assert!(result.is_err());
        assert_eq!(reader.stream_position().expect("reader position"), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_full_rate_depth_channels_and_raw_codes() {
        let mut streams = vec![lpcm_stream(0, 2, 3)];

        apply_lpcm_packet_overrides_with_probe(1, &mut streams, |_stream, substream_id| {
            assert_eq!(substream_id, DVDV_LPCM_SUBSTREAM_BASE);
            Ok(Some(probed_format(1, 96_000, 2, 24, 2)))
        });

        assert_eq!(streams[0].sample_frequency_code, 1);
        assert_eq!(streams[0].sample_frequency, Some(96_000));
        assert_eq!(streams[0].quantization_code, 2);
        assert_eq!(streams[0].bit_depth, Some(24));
        assert_eq!(streams[0].channels, 2);
    }

    #[test]
    fn overrides_channels_only() {
        let mut streams = vec![lpcm_stream(0, 0, 1)];

        apply_lpcm_packet_overrides_with_probe(1, &mut streams, |_stream, substream_id| {
            assert_eq!(substream_id, DVDV_LPCM_SUBSTREAM_BASE);
            Ok(Some(probed_format(0, 48_000, 0, 16, 2)))
        });

        assert_eq!(streams[0].sample_frequency_code, 0);
        assert_eq!(streams[0].sample_frequency, Some(48_000));
        assert_eq!(streams[0].quantization_code, 0);
        assert_eq!(streams[0].bit_depth, Some(16));
        assert_eq!(streams[0].channels, 2);
    }

    #[test]
    fn does_not_change_stream_when_ifo_and_packet_agree() {
        let mut streams = vec![lpcm_stream(1, 2, 2)];
        let original = streams[0].clone();

        apply_lpcm_packet_overrides_with_probe(1, &mut streams, |_stream, _substream_id| {
            Ok(Some(probed_format(1, 96_000, 2, 24, 2)))
        });

        assert_eq!(streams[0], original);
    }

    #[test]
    fn does_not_call_probe_for_non_lpcm_streams() {
        let mut streams = vec![ac3_stream(0), dts_stream(1)];
        let original = streams.clone();
        let mut probe_calls = 0;

        apply_lpcm_packet_overrides_with_probe(1, &mut streams, |_stream, _substream_id| {
            probe_calls += 1;
            panic!("non-LPCM stream must not trigger VOB I/O/probing");
        });

        assert_eq!(probe_calls, 0);
        assert_eq!(streams, original);
    }

    #[test]
    fn keeps_ifo_when_vts_has_no_title_vob() {
        let root = unique_dir("no-title-vob");
        let mut streams = vec![lpcm_stream(0, 0, 1)];
        let original = streams[0].clone();
        let disc = disc_without_title_vob();

        apply_lpcm_packet_overrides_for_directory_source(&root, &disc, 1, &mut streams);

        assert_eq!(streams[0], original);
    }

    #[test]
    fn probes_multiple_lpcm_streams_by_substream_id() {
        let mut streams = vec![
            lpcm_stream_with_index(0, 0, 0, 1),
            lpcm_stream_with_index(1, 0, 0, 1),
        ];
        let mut seen_substreams = Vec::new();

        apply_lpcm_packet_overrides_with_probe(1, &mut streams, |_stream, substream_id| {
            seen_substreams.push(substream_id);
            match substream_id {
                0xA0 => Ok(Some(probed_format(1, 96_000, 2, 24, 2))),
                0xA1 => Ok(Some(probed_format(0, 48_000, 0, 16, 2))),
                other => panic!("unexpected substream {:#04x}", other),
            }
        });

        assert_eq!(seen_substreams, vec![0xA0, 0xA1]);
        assert_eq!(streams[0].sample_frequency, Some(96_000));
        assert_eq!(streams[0].bit_depth, Some(24));
        assert_eq!(streams[0].channels, 2);
        assert_eq!(streams[1].sample_frequency, Some(48_000));
        assert_eq!(streams[1].bit_depth, Some(16));
        assert_eq!(streams[1].channels, 2);
    }

    #[test]
    fn source_probes_multiple_lpcm_streams_independently_by_substream_id() {
        let stream0_packet = sector_with_dvd_video_lpcm_packet(0xA0, 1, 2, 2);
        let stream1_packet = sector_with_dvd_video_lpcm_packet(0xA1, 0, 0, 2);
        let iso_lba = 4_u32;
        let (root, iso_path, _) = write_directory_and_iso_parity_fixtures(
            "multiple-lpcm-source-integration",
            &[stream0_packet, stream1_packet],
            iso_lba,
        );
        let directory_disc = disc_with_title_vob("VTS_01_1.VOB", 0);
        let iso_disc = disc_with_title_vob("VTS_01_1.VOB", iso_lba);

        let mut directory_streams = vec![
            lpcm_stream_with_index(0, 0, 0, 1),
            lpcm_stream_with_index(1, 1, 2, 3),
        ];
        apply_lpcm_packet_overrides_for_directory_source(
            &root,
            &directory_disc,
            1,
            &mut directory_streams,
        );

        let iso_reader = File::open(&iso_path).expect("open ISO-like fixture");
        let mut iso_streams = vec![
            lpcm_stream_with_index(0, 0, 0, 1),
            lpcm_stream_with_index(1, 1, 2, 3),
        ];
        apply_lpcm_packet_overrides_for_iso_source(&iso_reader, &iso_disc, 1, &mut iso_streams);

        for streams in [&directory_streams, &iso_streams] {
            assert_eq!(streams[0].sample_frequency_code, 1);
            assert_eq!(streams[0].sample_frequency, Some(96_000));
            assert_eq!(streams[0].quantization_code, 2);
            assert_eq!(streams[0].bit_depth, Some(24));
            assert_eq!(streams[0].channels, 2);

            assert_eq!(streams[1].sample_frequency_code, 0);
            assert_eq!(streams[1].sample_frequency, Some(48_000));
            assert_eq!(streams[1].quantization_code, 0);
            assert_eq!(streams[1].bit_depth, Some(16));
            assert_eq!(streams[1].channels, 2);
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_source_probe_reads_vts_title_vob_1() {
        let root = unique_dir("directory-probe");
        let video_ts = root.join("VIDEO_TS");
        fs::create_dir_all(&video_ts).expect("create VIDEO_TS");
        fs::write(video_ts.join("VIDEO_TS.IFO"), VMG_MAGIC).expect("write VMG IFO");
        let packet = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);
        fs::write(video_ts.join("VTS_01_1.VOB"), packet).expect("write title VOB");
        let disc = disc_with_title_vob("VTS_01_1.VOB", 0);
        let mut streams = vec![lpcm_stream(0, 2, 3)];

        apply_lpcm_packet_overrides_for_directory_source(&root, &disc, 1, &mut streams);

        assert_eq!(streams[0].sample_frequency, Some(96_000));
        assert_eq!(streams[0].bit_depth, Some(24));
        assert_eq!(streams[0].channels, 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn iso_source_probe_seeks_to_vts_title_vob_lba() {
        let lba = 2_u32;
        let mut bytes = vec![0_u8; DVD_SECTOR * 3];
        let packet = sector_with_dvd_video_lpcm_packet(DVDV_LPCM_SUBSTREAM_BASE, 1, 2, 2);
        let packet_offset = lba as usize * DVD_SECTOR;
        bytes[packet_offset..packet_offset + DVD_SECTOR].copy_from_slice(&packet);
        let path = write_temp_file("iso-lba-probe", &bytes);
        let reader = File::open(&path).expect("open ISO fixture");
        let disc = disc_with_title_vob("VTS_01_1.VOB", lba);
        let mut streams = vec![lpcm_stream(0, 2, 3)];

        apply_lpcm_packet_overrides_for_iso_source(&reader, &disc, 1, &mut streams);

        assert_eq!(streams[0].sample_frequency, Some(96_000));
        assert_eq!(streams[0].bit_depth, Some(24));
        assert_eq!(streams[0].channels, 2);
        let _ = fs::remove_dir_all(path.parent().expect("temp parent"));
    }

    #[test]
    fn directory_and_iso_probes_return_same_lpcm_packet_format() {
        let target_substream = DVDV_LPCM_SUBSTREAM_BASE;
        let expected = probed_format(1, 96_000, 2, 24, 2);
        let packet = sector_with_dvd_video_lpcm_packet(target_substream, 1, 2, 2);
        let miss = [0_u8; DVD_SECTOR];
        let iso_lba = 3_u32;
        let (root, iso_path, _) = write_directory_and_iso_parity_fixtures(
            "source-parity",
            &[miss, packet],
            iso_lba,
        );

        let directory_vob_path = directory_video_ts_file_path(&root, "VTS_01_1.VOB")
            .expect("directory source should resolve title VOB name");
        let mut directory_reader = File::open(&directory_vob_path).expect("open directory VOB");
        let directory_format = probe_lpcm_format_from_reader(&mut directory_reader, target_substream)
            .expect("directory probe should not fail")
            .expect("directory probe should find LPCM packet");

        let iso_reader = File::open(&iso_path).expect("open ISO-like fixture");
        let iso_format = probe_lpcm_format_from_iso_vob(
            &iso_reader,
            u64::from(iso_lba) * DVD_SECTOR as u64,
            target_substream,
        )
        .expect("ISO probe should not fail")
        .expect("ISO probe should find LPCM packet at VOB LBA");

        assert_eq!(directory_format, expected);
        assert_eq!(iso_format, expected);
        assert_eq!(directory_format, iso_format);

        let _ = fs::remove_dir_all(&root);
    }

    fn sector_with_short_private_stream_and_lpcm_like_bytes_after_end() -> [u8; DVD_SECTOR] {
        let mut sector = [0_u8; DVD_SECTOR];
        sector[..4].copy_from_slice(&[0x00, 0x00, 0x01, 0xBA]);
        sector[13] = 0;

        let packet_offset = 14;
        sector[packet_offset..packet_offset + 4].copy_from_slice(&[0x00, 0x00, 0x01, 0xBD]);
        sector[packet_offset + 4..packet_offset + 6].copy_from_slice(&3_u16.to_be_bytes());
        sector[packet_offset + 6] = 0x80;
        sector[packet_offset + 7] = 0x80;
        sector[packet_offset + 8] = 0;

        // The declared Private Stream 1 body ends immediately before this byte.
        // These bytes intentionally look like a valid DVD-Video LPCM sub-header,
        // but they do not belong to the declared packet.
        let fake_sub_header_offset = packet_offset + 9;
        sector[fake_sub_header_offset..fake_sub_header_offset + 7].copy_from_slice(&[
            DVDV_LPCM_SUBSTREAM_BASE,
            0x04,
            0x00,
            0x58,
            0x03,
            0x91,
            0x7F,
        ]);

        sector
    }

    #[test]
    fn lpcm_probe_keeps_ifo_when_no_packet_is_found() {
        let empty_sector = [0_u8; DVD_SECTOR];
        let mut reader = &empty_sector[..];
        assert!(matches!(
            probe_lpcm_format_from_reader(&mut reader, DVDV_LPCM_SUBSTREAM_BASE),
            Ok(None)
        ));
    }

    #[test]
    fn lpcm_probe_ignores_lpcm_like_bytes_after_declared_private_stream_end() {
        let sector = sector_with_short_private_stream_and_lpcm_like_bytes_after_end();

        assert!(probe_lpcm_format_from_sector(&sector, DVDV_LPCM_SUBSTREAM_BASE).is_none());

        let mut reader = &sector[..];
        assert!(matches!(
            probe_lpcm_format_from_reader(&mut reader, DVDV_LPCM_SUBSTREAM_BASE),
            Ok(None)
        ));
    }

    #[test]
    fn lpcm_override_updates_rate_depth_channels_and_raw_codes() {
        let mut stream = lpcm_stream(0, 2, 3);
        apply_lpcm_packet_override(
            1,
            &mut stream,
            DvdVideoLpcmProbeFormat {
                sample_frequency_code: 1,
                sample_frequency: 96_000,
                quantization_code: 2,
                bit_depth: 24,
                channels: 2,
            },
        );

        assert_eq!(stream.sample_frequency_code, 1);
        assert_eq!(stream.sample_frequency, Some(96_000));
        assert_eq!(stream.quantization_code, 2);
        assert_eq!(stream.bit_depth, Some(24));
        assert_eq!(stream.channels, 2);
    }

    #[test]
    fn lpcm_override_can_update_channels_only() {
        let mut stream = lpcm_stream(0, 0, 1);
        apply_lpcm_packet_override(
            1,
            &mut stream,
            DvdVideoLpcmProbeFormat {
                sample_frequency_code: 0,
                sample_frequency: 48_000,
                quantization_code: 0,
                bit_depth: 16,
                channels: 2,
            },
        );

        assert_eq!(stream.sample_frequency, Some(48_000));
        assert_eq!(stream.bit_depth, Some(16));
        assert_eq!(stream.channels, 2);
    }

}


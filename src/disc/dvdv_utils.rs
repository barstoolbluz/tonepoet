//! DVD-Video source detection and `DiscContents` mapping glue.
//!
//! Hybrid DVD-Audio/DVD-Video discs must stay on the DVD-Audio path. These
//! helpers therefore reject any source that the DVD-Audio detector accepts, and
//! also reject ISO images whose `AUDIO_TS/` inventory is non-empty.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use dvdvideo::disc::{DvdFile, DvdFileKind};
use dvdvideo::ifo::{DvdTitleEntry, TtSrpt, VmgIfo, DVD_SECTOR};
use dvdvideo::{DvdDisc, VtsIfo};

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
    Ok(crate::disc::dvdv_mapper::map_dvdv_disc(
        &disc,
        &vts_ifos,
        path,
    ))
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("dvdv-utils-{label}-{}-{nanos}", std::process::id()))
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
}


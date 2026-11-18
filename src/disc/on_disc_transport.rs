//! Resolve a selected transport file to the disc navigation that owns it.
//!
//! The file is an entry point, not the extraction source. Blu-ray playlists
//! and DVD-Video titles can cross file boundaries; their existing materializers
//! need the *disc root*. An unrelated/orphan transport file is deliberately not
//! treated as a disc. Orphans are mapped to standalone audio streams.

use std::path::{Path, PathBuf};

use super::model::DiscContents;

/// Cheap, extension-only test for the explicit context-menu entry point.
/// Structural validation and media I/O always run in the background probe.
#[must_use]
pub fn is_transport_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("m2ts") || extension.eq_ignore_ascii_case("vob")
        })
}

enum DiscFileLocation {
    Bluray { root: PathBuf, clip_id: String },
    DvdVideo { root: PathBuf, file_name: String },
}

fn disc_root(parent: &Path) -> Option<&Path> {
    parent.parent().map(|root| {
        if root.as_os_str().is_empty() { Path::new(".") } else { root }
    })
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name()
        .and_then(|part| part.to_str())
        .is_some_and(|part| part.eq_ignore_ascii_case(name))
}

/// Only navigation, not copied directory names, establishes disc ownership.
/// Inspect the adjacent DVD IFO/BUP files or Blu-ray navigation tables without
/// reading media or traversing the disc. Once any marker is present, Phase A
/// owns validation (including malformed/incomplete navigation failures).
fn directory_has_marker(directory: &Path, predicate: impl Fn(&str) -> bool) -> bool {
    let Ok(mut entries) = std::fs::read_dir(directory) else {
        // A directory that cannot be inspected cannot establish the absence
        // of navigation. Never turn an inaccessible authored disc into an
        // apparently independent stream.
        return directory.is_dir();
    };
    entries.any(|entry| {
        match entry {
            Ok(entry) => entry.file_name().to_str().is_some_and(&predicate),
            Err(_) => true, // unreadable navigation inventory is not an orphan
        }
    })
}

fn bluray_markers_in(directory: &Path) -> bool {
    directory_has_marker(directory, |name| {
        name.eq_ignore_ascii_case("index.bdmv")
            || name.eq_ignore_ascii_case("MovieObject.bdmv")
    }) || directory_has_marker_in_child(directory, "PLAYLIST", "mpls")
        || directory_has_marker_in_child(directory, "CLIPINF", "clpi")
}

fn has_bluray_navigation(bdmv: &Path) -> bool {
    if bluray_markers_in(bdmv) { return true; }
    // An incomplete authored disc can retain backup navigation after its
    // primary marker was lost. Check this one standard adjacent directory.
    let Ok(entries) = std::fs::read_dir(bdmv) else { return bdmv.is_dir(); };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name().to_str().is_some_and(|name| name.eq_ignore_ascii_case("BACKUP"))
            && bluray_markers_in(&entry.path())
    })
}

fn directory_has_marker_in_child(bdmv: &Path, child: &str, extension: &str) -> bool {
    // Case-insensitive lookup matches Phase A's disc layout recognition.
    let Ok(children) = std::fs::read_dir(bdmv) else { return bdmv.is_dir(); };
    children.filter_map(Result::ok).any(|entry| {
        entry.file_name().to_str().is_some_and(|name| name.eq_ignore_ascii_case(child))
            && directory_has_marker(&entry.path(), |name| {
                name.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
            })
    })
}

/// Common classifier for both discovery and the worker's launch-time check.
/// A media-only VIDEO_TS or BDMV/STREAM copy is a standalone file, even when
/// the copied directories retain their optical-disc names.
pub(crate) fn has_disc_location(path: &Path) -> bool {
    let Some(parent) = path.parent() else { return false; };
    if named(parent, "VIDEO_TS") {
        return directory_has_marker(parent, |name| {
            name.rsplit_once('.').is_some_and(|(_, ext)| {
                ext.eq_ignore_ascii_case("ifo") || ext.eq_ignore_ascii_case("bup")
            })
        });
    }
    named(parent, "STREAM") && parent.parent()
        .filter(|bdmv| named(bdmv, "BDMV"))
        .is_some_and(has_bluray_navigation)
}

fn locate(path: &Path) -> Result<DiscFileLocation, String> {
    if !path.is_file() {
        return Err(format!("Not a regular on-disc transport file: {}", path.display()));
    }
    let directory = path.parent().ok_or_else(|| format!(
        "No disc directory around {}", path.display()
    ))?;
    let name = path.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| format!("Unsupported transport filename: {}", path.display()))?;

    if name.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("m2ts")) {
        let bdmv = directory.parent().filter(|parent| named(parent, "BDMV"));
        let root = bdmv.filter(|_| named(directory, "STREAM"))
            .and_then(disc_root)
            .ok_or_else(|| format!(
                "{} is not in a Blu-ray BDMV/STREAM directory; standalone-file extraction is a later phase",
                path.display()
            ))?;
        if !super::bluray_utils::is_bluray_directory(root) {
            return Err(format!(
                "The BDMV navigation and playlist structure for {} is missing or incomplete",
                path.display()
            ));
        }
        let clip_id = path.file_stem().and_then(|value| value.to_str())
            .filter(|value| value.len() == 5 && value.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| format!(
                "{} has no five-digit Blu-ray clip identifier", path.display()
            ))?;
        return Ok(DiscFileLocation::Bluray {
            root: root.to_path_buf(), clip_id: clip_id.to_string(),
        });
    }

    if name.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("vob")) {
        let root = disc_root(directory).filter(|_| named(directory, "VIDEO_TS"))
            .ok_or_else(|| format!(
                "{} is not in a DVD VIDEO_TS directory; standalone-file extraction is a later phase",
                path.display()
            ))?;
        if !super::dvdv_utils::has_dvdv_directory_navigation(root) {
            return Err(format!(
                "The DVD-Video IFO navigation for {} is missing or incomplete",
                path.display()
            ));
        }
        return Ok(DiscFileLocation::DvdVideo {
            root: root.to_path_buf(), file_name: name.to_string(),
        });
    }
    Err(format!("Not an M2TS or VOB file: {}", path.display()))
}

/// Discover the authored playlists/titles for this file and map only their
/// audio presentations. Keep the complete title/playlist and its chapter
/// ordering: an authored program may use several transport files.
pub fn probe_on_disc_transport(path: &Path) -> Result<DiscContents, String> {
    if !has_disc_location(path) {
        return super::matroska_utils::map_orphan_transport_file(path);
    }
    match locate(path)? {
        DiscFileLocation::Bluray { root, clip_id } => {
            super::bluray_utils::map_bluray_source_for_clip(&root, &clip_id)
        }
        DiscFileLocation::DvdVideo { root, file_name } => {
            super::dvdv_utils::map_dvdv_source_for_vob(&root, &file_name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m2ts_disc_root_is_discovered_without_media_probe() {
        let temp = tempfile::tempdir().unwrap();
        let bdmv = temp.path().join("BluRay").join("bdmv");
        std::fs::create_dir_all(bdmv.join("playLIST")).unwrap();
        std::fs::create_dir_all(bdmv.join("stream")).unwrap();
        for name in ["index.BDMV", "MovieObject.bdmv"] {
            std::fs::write(bdmv.join(name), []).unwrap();
        }
        std::fs::write(bdmv.join("playLIST/00003.MPLS"), []).unwrap();
        let clip = bdmv.join("stream/00042.M2TS");
        std::fs::write(&clip, []).unwrap();
        assert!(has_disc_location(&clip));
        match locate(&clip).unwrap() {
            DiscFileLocation::Bluray { root, clip_id } => {
                assert_eq!(root, temp.path().join("BluRay"));
                assert_eq!(clip_id, "00042");
            }
            _ => panic!("wrong disc type"),
        }
    }

    #[test]
    fn vob_disc_root_is_discovered_without_media_probe() {
        let temp = tempfile::tempdir().unwrap();
        let video_ts = temp.path().join("DVD").join("video_ts");
        std::fs::create_dir_all(&video_ts).unwrap();
        std::fs::write(video_ts.join("video_ts.ifo"), b"DVDVIDEO-VMG").unwrap();
        let vob = video_ts.join("vts_02_2.vOb");
        std::fs::write(&vob, []).unwrap();
        assert!(has_disc_location(&vob));
        match locate(&vob).unwrap() {
            DiscFileLocation::DvdVideo { root, file_name } => {
                assert_eq!(root, temp.path().join("DVD"));
                assert_eq!(file_name, "vts_02_2.vOb");
            }
            _ => panic!("wrong disc type"),
        }
    }

    #[test]
    fn explicit_vob_on_hybrid_disc_uses_video_navigation_not_disc_classification() {
        let temp = tempfile::tempdir().unwrap();
        let video_ts = temp.path().join("VIDEO_TS");
        let audio_ts = temp.path().join("AUDIO_TS");
        std::fs::create_dir(&video_ts).unwrap();
        std::fs::create_dir(&audio_ts).unwrap();
        std::fs::write(video_ts.join("VIDEO_TS.IFO"), b"DVDVIDEO-VMG").unwrap();
        std::fs::write(audio_ts.join("AUDIO_TS.IFO"), b"DVDAUDIO-AMG").unwrap();
        let vob = video_ts.join("VTS_01_2.VOB");
        std::fs::write(&vob, vec![0u8; 2048]).unwrap();
        assert!(has_disc_location(&vob));

        assert!(crate::disc::dvda_utils::is_dvda_directory(temp.path()));
        assert!(!crate::disc::dvdv_utils::is_dvdv_directory(temp.path()));
        match locate(&vob).unwrap() {
            DiscFileLocation::DvdVideo { root, file_name } => {
                assert_eq!(root, temp.path());
                assert_eq!(file_name, "VTS_01_2.VOB");
            }
            _ => panic!("explicit VOB must use VIDEO_TS"),
        }

        // A misleading .IFO filename is not sufficient to admit navigation.
        std::fs::write(video_ts.join("VIDEO_TS.IFO"), b"not a DVD-Video IFO").unwrap();
        assert!(locate(&vob).is_err());
    }

    #[test]
    fn orphaned_files_and_non_disc_names_do_not_claim_disc_navigation() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["00042.m2ts", "VTS_01_1.VOB"] {
            let path = temp.path().join(name);
            std::fs::write(&path, []).unwrap();
            assert!(locate(&path).is_err());
        }
        assert!(is_transport_path(Path::new("00042.M2TS")));
        assert!(is_transport_path(Path::new("VTS_01_1.vOb")));
        assert!(!is_transport_path(Path::new("song.flac")));
    }

    #[test]
    fn copied_media_only_disc_subtrees_are_orphans() {
        let temp = tempfile::tempdir().unwrap();
        let bluray = temp.path().join("BDMV/STREAM/00001.m2ts");
        std::fs::create_dir_all(bluray.parent().unwrap()).unwrap();
        std::fs::write(&bluray, b"transport stream fixture").unwrap();
        assert!(!has_disc_location(&bluray));

        let dvd = temp.path().join("VIDEO_TS/VTS_01_1.VOB");
        std::fs::create_dir_all(dvd.parent().unwrap()).unwrap();
        std::fs::write(&dvd, b"transport stream fixture").unwrap();
        assert!(!has_disc_location(&dvd));
    }

    #[test]
    fn malformed_adjacent_navigation_retains_phase_a_errors() {
        let temp = tempfile::tempdir().unwrap();
        let bluray = temp.path().join("BDMV/STREAM/00001.m2ts");
        std::fs::create_dir_all(bluray.parent().unwrap()).unwrap();
        std::fs::write(&bluray, b"not a valid playlist").unwrap();
        let bdmv = bluray.parent().unwrap().parent().unwrap();
        std::fs::write(bdmv.join("Index.BDMV"), b"broken navigation").unwrap();
        assert!(has_disc_location(&bluray));
        assert!(probe_on_disc_transport(&bluray).unwrap_err().contains("navigation"));
        std::fs::remove_file(bdmv.join("Index.BDMV")).unwrap();
        std::fs::create_dir_all(bdmv.join("playLIST")).unwrap();
        std::fs::write(bdmv.join("playLIST/00001.MPLS"), b"broken playlist").unwrap();
        assert!(has_disc_location(&bluray));
        std::fs::remove_file(bdmv.join("playLIST/00001.MPLS")).unwrap();
        std::fs::create_dir_all(bdmv.join("CLIPINF")).unwrap();
        std::fs::write(bdmv.join("CLIPINF/00001.CLPI"), b"broken clip info").unwrap();
        assert!(has_disc_location(&bluray));
        std::fs::remove_file(bdmv.join("CLIPINF/00001.CLPI")).unwrap();
        std::fs::create_dir_all(bdmv.join("BACKUP")).unwrap();
        std::fs::write(bdmv.join("BACKUP/MovieObject.bdmv"), b"broken backup").unwrap();
        assert!(has_disc_location(&bluray));

        let dvd = temp.path().join("VIDEO_TS/VTS_01_1.VOB");
        std::fs::create_dir_all(dvd.parent().unwrap()).unwrap();
        std::fs::write(&dvd, b"not a valid title").unwrap();
        std::fs::write(dvd.parent().unwrap().join("vts_01_0.IfO"), b"broken IFO").unwrap();
        assert!(has_disc_location(&dvd));
        assert!(probe_on_disc_transport(&dvd).unwrap_err().contains("IFO navigation"));
        std::fs::remove_file(dvd.parent().unwrap().join("vts_01_0.IfO")).unwrap();
        std::fs::write(dvd.parent().unwrap().join("VIDEO_TS.BUP"), b"broken backup").unwrap();
        assert!(has_disc_location(&dvd));
    }

}

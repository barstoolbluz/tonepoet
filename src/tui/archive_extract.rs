//! Archive-file admission for the shared native Extract window.
//!
//! The existing 7z materializer handles archive formats, including nested
//! TAR compression. A private, disposable extraction supplies file identities
//! and tags for naming. No archive content is written to the destination by
//! the preview, and no output file is ever decoded or modified.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;
use lofty::file::TaggedFileExt;
use lofty::tag::{Accessor, ItemKey};

use crate::convert::pipeline::native_extract::NativeArchiveMember;
use crate::disc::{
    AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscExtractTagFields,
    DiscFormat, DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo,
    PresentationId,
};

#[derive(Debug)]
pub struct ArchiveExtractPreview {
    pub contents: DiscContents,
    pub members: Vec<NativeArchiveMember>,
    pub source_length: u64,
    pub source_modified: Option<std::time::SystemTime>,
    pub password: Option<String>,
}

pub(crate) fn safe_archive_relative_path(relative: &Path) -> Result<(),String> {
    // TAR writers commonly prefix safe member paths with `./`. Accept that
    // spelling, but never `..`, absolute roots or platform path prefixes.
    let mut seen_file=false;
    for component in relative.components() {
        match component {
            Component::CurDir if !seen_file => {},
            Component::Normal(_) => seen_file=true,
            _ => return Err(format!("Archive entry has an unsafe path: {}",relative.display())),
        }
    }
    if !seen_file {return Err(format!("Archive entry is not a file path: {}",relative.display()));}
    // Do not interpret Windows drive paths or backslash separators as
    // ordinary Unix names. Colons elsewhere are legitimate Unix filenames.
    if relative.components().enumerate().any(|(index,part)|{
        let text=part.as_os_str().to_string_lossy();
        let bytes=text.as_bytes();
        text.contains('\\') || text.contains('\0') ||
            (index==0 && bytes.len()>=2 && bytes[0].is_ascii_alphabetic() && bytes[1]==b':')
    }) {
        return Err(format!("Archive entry is not a safe relative path: {}",relative.display()));
    }
    Ok(())
}

pub(crate) fn regular_archive_file(root: &Path, relative: &Path) -> Result<PathBuf,String> {
    safe_archive_relative_path(relative)?;
    let mut current=root.to_path_buf();
    for part in relative.components() {
        current.push(part.as_os_str());
        let info=fs::symlink_metadata(&current).map_err(|error|
            format!("Archive member unavailable '{}': {error}",current.display()))?;
        if info.file_type().is_symlink() {
            return Err(format!("Archive member contains a symlink: {}",relative.display()));
        }
    }
    if !fs::symlink_metadata(&current).is_ok_and(|meta|meta.file_type().is_file()) {
        return Err(format!("Archive member is not a regular file: {}",relative.display()));
    }
    Ok(current)
}

fn is_audio(path: &Path) -> bool {
    let extension=path.extension().and_then(|ext|ext.to_str()).unwrap_or("")
        .to_ascii_lowercase();
    crate::convert::classify::SUPPORTED_AUDIO_FILE_EXTENSIONS.contains(&extension.as_str())
        || extension == "mka" // Archive extraction also accepts tagged Matroska audio.
}

fn read_naming_tags(path: &Path) -> (DiscExtractTagFields, Option<u32>) {
    let Ok(tagged)=lofty::read_from_path(path) else {return (DiscExtractTagFields::default(),None)};
    let Some(tag)=tagged.primary_tag().or_else(||tagged.first_tag()) else {
        return (DiscExtractTagFields::default(),None);
    };
    let number=tag.track().filter(|number|*number>0);
    (DiscExtractTagFields {
        title:tag.title().map(|s|s.to_string()),
        artist:tag.artist().map(|s|s.to_string()),
        album:tag.album().map(|s|s.to_string()),
        genre:tag.genre().map(|s|s.to_string()),
        year:tag.year().map(|year|year.to_string()),
        composer:tag.get_string(&ItemKey::Composer).map(str::to_owned),
        performer:tag.get_string(&ItemKey::Performer).map(str::to_owned),
        catalog:tag.get_string(&ItemKey::CatalogNumber).map(str::to_owned),
        publisher:tag.get_string(&ItemKey::Publisher).map(str::to_owned),
    },number)
}

fn files_in_staging(root: &Path) -> Result<Vec<(PathBuf,u64)>,String> {
    let mut files=Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry=entry.map_err(|error|format!("Read archive contents: {error}"))?;
        if entry.path()==root {continue;}
        if entry.file_type().is_symlink() || (!entry.file_type().is_dir() && !entry.file_type().is_file()) {
            return Err(format!("Archive contains a link or special file: {}",entry.path().display()));
        }
        if !entry.file_type().is_file() {continue;}
        let relative=entry.path().strip_prefix(root).map_err(|error|
            format!("Archive entry escaped staging: {error}"))?.to_path_buf();
        safe_archive_relative_path(&relative)?;
        let size=entry.metadata().map_err(|error|format!("Inspect archive entry: {error}"))?.len();
        files.push((relative,size));
    }
    files.sort_by(|left,right|left.0.cmp(&right.0));
    if files.is_empty() {return Err("Archive has no regular files to extract".into());}
    let mut names=HashSet::new();
    for (path,_) in &files {
        if !names.insert(path.to_string_lossy().to_lowercase()) {
            return Err(format!("Archive includes names that collide on case-insensitive filesystems: {}",
                path.display()));
        }
    }
    Ok(files)
}

pub async fn inspect(path: &Path, passwords: Vec<String>, cancel: CancellationToken)
    -> Result<ArchiveExtractPreview,String>
{
    let metadata=fs::symlink_metadata(path).map_err(|error|
        format!("Cannot inspect archive '{}': {error}",path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("Archive source must be a regular, non-symlink file".into());
    }
    let source_length=metadata.len();
    let source_modified=metadata.modified().ok();
    let (listing,password)=crate::tui::archive_listing::list_archive_with_password_candidates(
        path,&passwords,crate::tui::archive_listing::ArchiveListingOptions::default(),
        cancel.clone()).await?;
    for entry in &listing.entries {
        let raw=Path::new(&entry.path);
        if entry.is_dir && matches!(entry.path.as_str(),"."|"./") {continue;}
        safe_archive_relative_path(raw)?;
    }
    let stage=tempfile::Builder::new().prefix("tonepoet-archive-inspect-")
        .tempdir().map_err(|error|format!("Create archive preview storage: {error}"))?;
    let runner=crate::convert::pipeline::tool::RealToolRunner::new(Default::default());
    crate::convert::pipeline::materializer_archive::extract_archive_to_staging(
        path,stage.path(),"archive-native-preview",password.as_deref(),
        &runner,None,&cancel).await.map_err(|error|format!("Unpack archive for naming: {error}"))?;
    if cancel.is_cancelled() {return Err("Archive preview cancelled".into());}
    let paths=tokio::task::spawn_blocking({
        let root=stage.path().to_path_buf();
        move ||files_in_staging(&root)
    }).await.map_err(|error|format!("Archive preview listing task: {error}"))??;
    // Metadata reads stay off the event reducer; the private tree is alive
    // throughout the blocking task. No tag writer ever touches these files.
    let stage_path=stage.path().to_path_buf();
    let (members,tracks,tags)=tokio::task::spawn_blocking(move ||{
        let mut members=Vec::new();
        let mut tracks=Vec::new();
        let mut tags=Vec::new();
        for (index,(relative,size)) in paths.into_iter().enumerate() {
            let number=u32::try_from(index+1).map_err(|_|"Archive contains too many files")?;
            let file=regular_archive_file(&stage_path,&relative)?;
            let audio=is_audio(&relative);
            let (tag,tag_track_number)=if audio {read_naming_tags(&file)}
                else {(DiscExtractTagFields::default(),None)};
            let tagged_audio=audio &&
                (tag!=DiscExtractTagFields::default() || tag_track_number.is_some());
            let basename=relative.file_name().unwrap_or_default().to_string_lossy();
            let label=tag.title.clone().unwrap_or_else(||relative.to_string_lossy().into_owned());
            tracks.push(DiscTrack {number,title:Some(label),performer:tag.artist.clone(),
                duration_secs:None,format_note:Some(basename.into_owned())});
            members.push(NativeArchiveMember{number,relative_path:relative,size,tagged_audio,
                tag_track_number});
            tags.push(tag);
        }
        Ok::<_,String>((members,tracks,tags))
    }).await.map_err(|error|format!("Archive tag-read task: {error}"))??;
    let after=fs::symlink_metadata(path).map_err(|error|
        format!("Archive source disappeared during preview: {error}"))?;
    if !after.file_type().is_file() || after.file_type().is_symlink() ||
        after.len()!=source_length || after.modified().ok()!=source_modified {
        return Err("Archive changed during Extract preview; reopen it".into());
    }
    let label=path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let presentation=DiscPresentation { id:PresentationId::ArchiveFiles,
        label:label.clone(),format:AudioPresentationFormat{codec:Some("Original files".into()),
            sample_rate:None,bit_depth:None,channels:None,channel_layout:None,
            lossless:true,provenance:FormatProvenance::Unknown},
        total_duration_secs:0.0,tracks,extract:PresentationExtractInfo {
            authored_track_tags:tags,..Default::default()},
        album_title:Some(label.clone()),album_artist:None,genre:None,year:None};
    Ok(ArchiveExtractPreview {
        contents:DiscContents{format:DiscFormat::Archive,label:label.clone(),
            source_path:path.to_path_buf(),presentations:vec![presentation],
            suppressed:Vec::new(),copy_protection:CopyProtectionSummary {
                description:"Original archive members, byte-for-byte".into()},
            diagnostics:Vec::new(),album_title:Some(label),album_artist:None,
            genre:None,year:None},
        members,source_length,source_modified,password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_unsafe_archive_relative_names() {
        for path in ["../escape","/absolute","folder/../escape","C:\\danger.txt",
            "album\\sneaky.mp3","C:/escape.mp3"] {
            assert!(safe_archive_relative_path(Path::new(path)).is_err(),"{path}");
        }
        assert!(safe_archive_relative_path(Path::new("./Disk 1/cover.jpg")).is_ok());
        assert!(safe_archive_relative_path(Path::new("Disk 1/cover.jpg")).is_ok());
        assert!(safe_archive_relative_path(Path::new(".hidden/album.cue")).is_ok());
        #[cfg(unix)]
        assert!(safe_archive_relative_path(Path::new("Artist: Live/track.flac")).is_ok());
    }

    #[test]
    fn archive_inventory_includes_non_audio_and_preserves_relative_directories() {
        let staging=tempfile::tempdir().unwrap();
        fs::create_dir_all(staging.path().join("Disc 1")).unwrap();
        fs::write(staging.path().join("cover.jpg"),b"cover").unwrap();
        fs::write(staging.path().join("Disc 1/album.cue"),b"cue").unwrap();
        fs::write(staging.path().join("Disc 1/track.flac"),b"audio").unwrap();
        let files=files_in_staging(staging.path()).unwrap();
        assert_eq!(files,vec![
            (PathBuf::from("Disc 1/album.cue"),3),
            (PathBuf::from("Disc 1/track.flac"),5),
            (PathBuf::from("cover.jpg"),5),
        ]);
    }

    #[cfg(unix)]
    #[test]
    fn archive_inventory_refuses_symlink_members() {
        use std::os::unix::fs::symlink;
        let staging=tempfile::tempdir().unwrap();
        fs::write(staging.path().join("cover.jpg"),b"cover").unwrap();
        symlink(staging.path().join("cover.jpg"),staging.path().join("link.jpg")).unwrap();
        assert!(files_in_staging(staging.path()).is_err());
    }


    #[test]
    fn archive_audio_names_follow_the_shared_classifier() {
        for extension in crate::convert::classify::SUPPORTED_AUDIO_FILE_EXTENSIONS {
            let path=PathBuf::from(format!("track.{extension}"));
            assert!(is_audio(&path),"missed canonical audio extension {extension}");
        }
        for extension in ["oga","mp4","aifc","bwf","m4r","alac","caf","mpc"] {
            assert!(is_audio(Path::new(&format!("track.{extension}"))));
        }
        assert!(is_audio(Path::new("track.MKA")));
        assert!(is_audio(Path::new("track.OGa")));
        for non_audio in ["cover.jpg","disc.cue","notes.txt"] {
            assert!(!is_audio(Path::new(non_audio)));
        }
    }

    #[test]
    fn archive_tag_reader_includes_full_naming_field_set() {
        // A real tagged FLAC, not a synthetic NativeTagValues projection.
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("Skipping archive tag fixture: ffmpeg unavailable");
            return;
        }
        let root=tempfile::tempdir().unwrap();
        let track=root.path().join("movement.flac");
        let result=std::process::Command::new("ffmpeg")
            .args(["-hide_banner","-loglevel","error","-nostdin","-y",
                "-f","lavfi","-i","sine=frequency=440:sample_rate=44100:duration=1",
                "-c:a","flac","-metadata","title=Movement 1",
                "-metadata","artist=Debussy",
                "-metadata","composer=Claude Debussy",
                "-metadata","performer=Alice Example",
                "-metadata","CATALOGNUMBER=CAT-42",
                "-metadata","publisher=Test Records"])
            .arg(&track).output().unwrap();
        assert!(result.status.success(),"ffmpeg fixture: {}",
            String::from_utf8_lossy(&result.stderr));
        let (tags,_) = read_naming_tags(&track);
        assert_eq!(tags.title.as_deref(),Some("Movement 1"));
        assert_eq!(tags.composer.as_deref(),Some("Claude Debussy"));
        assert_eq!(tags.performer.as_deref(),Some("Alice Example"));
        assert_eq!(tags.catalog.as_deref(),Some("CAT-42"));
        assert_eq!(tags.publisher.as_deref(),Some("Test Records"));
    }

}

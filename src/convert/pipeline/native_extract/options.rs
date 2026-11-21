//! Extraction-only output companions. The native file task owns admission,
//! publication, cancellation and recovery; this module supplies bounded
//! serialization and selected companion copying without a second writer.

use super::*;
use lofty::config::WriteOptions;
use lofty::file::AudioFile;
use lofty::prelude::TaggedFileExt;
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem};

/// Simple chapter format is understood by both mkvmerge and MP4Box -chap.
/// The source timeline is measured on the audio clock, not guessed from an
/// estimated duration. Only the presentation title is sanitized for lines.
pub(super) fn chapter_text(starts: &[(String,u64)], rate: u32) -> Result<String,String> {
    if rate == 0 {return Err("Cannot write chapters without a sample rate".into());}
    if starts.is_empty() {return Err("Cannot write empty chapter table".into());}
    if starts.windows(2).any(|pair|pair[0].1>=pair[1].1) {
        return Err("Chapters are not an ordered timeline".into());
    }
    let mut out=String::new();
    for (index,(title,sample)) in starts.iter().enumerate() {
        let ms=(u128::from(*sample)*1000+u128::from(rate/2))/u128::from(rate);
        let seconds=ms/1000;
        let name=title.replace('\r', " ").replace('\n', " ").trim().to_owned();
        out.push_str(&format!("CHAPTER{:02}={:02}:{:02}:{:02}.{:03}\n",
            index+1, seconds/3600,(seconds/60)%60,seconds%60,ms%1000));
        out.push_str(&format!("CHAPTER{:02}NAME={}\n",index+1,name));
    }
    Ok(out)
}

/// CUE indexes round to 75-frame positions, as required by the CUE format.
/// Raw elementary carriers use FILE ... WAVE: CUE has no AAC/AC3/DTS type.
pub(super) fn cue_text(file_name: &str, starts:&[(String,u64,bool)], rate:u32)
    -> Result<String,String> {
    if rate==0 || starts.is_empty() || starts.len()>99 || starts[0].1!=0 ||
        starts.windows(2).any(|pair|pair[0].1>=pair[1].1) {
        return Err("Invalid CUE clock or track starts".into());
    }
    let file_name=file_name.replace('"',"'").replace('\r'," ").replace('\n'," ");
    let cue_type=if file_name.to_ascii_lowercase().ends_with(".mp3") {"MP3"}else{"WAVE"};
    let mut out=format!("FILE \"{file_name}\" {cue_type}\n");
    for (i,(name,sample,pre)) in starts.iter().enumerate() {
        let frames=(u128::from(*sample)*75+u128::from(rate/2))/u128::from(rate);
        out.push_str(&format!("  TRACK {:02} AUDIO\n",i+1));
        out.push_str(&format!("    TITLE \"{}\"\n",name.replace('"',"'").replace('\r'," ").replace('\n'," ")));
        if *pre {out.push_str("    FLAGS PRE\n");}
        out.push_str(&format!("    INDEX 01 {:02}:{:02}:{:02}\n",
            frames/4500,(frames/75)%60,frames%75));
    }
    Ok(out)
}

pub(super) fn stage_text(temp: &Path, name:&str, content:&str) -> Result<PathBuf,String> {
    let path=temp.join(name);
    let mut out=OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e|format!("Create private extraction sidecar: {e}"))?;
    out.write_all(content.as_bytes()).and_then(|()|out.sync_all())
        .map_err(|e|format!("Finish private extraction sidecar: {e}"))?;
    Ok(path)
}

pub(super) fn embed_cue(path: &Path, text: &str) -> Result<(),String> {
    let extension=path.extension().and_then(|e|e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !matches!(extension.as_str(),"flac"|"wv") {return Ok(());}
    let mut tagged=lofty::read_from_path(path)
        .map_err(|e|format!("Read {} for embedded CUE: {e}",path.display()))?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tagged.primary_tag_type()));
    }
    let tag=tagged.primary_tag_mut().ok_or("Embedded CUE has no writable tag")?;
    let key=ItemKey::Unknown("CUESHEET".into());
    tag.remove_key(&key);
    tag.insert_unchecked(TagItem::new(key,ItemValue::Text(text.trim().to_owned())));
    tagged.save_to_path(path,WriteOptions::default())
        .map_err(|e|format!("Write embedded CUE to {}: {e}",path.display()))?;
    Ok(())
}

pub(super) fn write_preemphasis_tag(path:&Path) -> Result<(),String> {
    if path.extension().and_then(|e|e.to_str())
        .is_some_and(|e|e.eq_ignore_ascii_case("wav")) {
        // PRE_EMPHASIS is not a RIFF INFO field. Use the established
        // authoritative ID3v2-in-RIFF writer, which retains the PCM payload.
        return crate::tui::probe::write_all_tag_value_lists_to_id3v2_with_cancel(
            path, &[(ItemKey::Unknown("PRE_EMPHASIS".into()), vec!["1".into()])], None)
            .map(|_|());
    }
    let mut tagged=lofty::read_from_path(path)
        .map_err(|e|format!("Read {} for pre-emphasis tag: {e}",path.display()))?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(tagged.primary_tag_type()));
    }
    let tag=tagged.primary_tag_mut().ok_or("Pre-emphasis carrier has no tag")?;
    let key=ItemKey::Unknown("PRE_EMPHASIS".into());
    tag.remove_key(&key);
    tag.insert_unchecked(TagItem::new(key,ItemValue::Text("1".into())));
    tagged.save_to_path(path,WriteOptions::default())
        .map_err(|e|format!("Write pre-emphasis metadata {}: {e}",path.display()))
}

fn artwork_type(path:&Path) -> bool {
    path.extension().and_then(|ext|ext.to_str()).is_some_and(|ext|
        matches!(ext.to_ascii_lowercase().as_str(),
            "png"|"jpg"|"jpeg"|"webp"|"gif"|"bmp"|"tif"|"tiff"|"avif"|"heic"))
}

fn artwork_folder(path:&Path) -> bool {
    path.file_name().and_then(|s|s.to_str()).is_some_and(|name|
        matches!(name.to_ascii_lowercase().as_str(),
            "artwork"|"art"|"scans"|"covers"|"booklet"))
}

fn includes_companion(path:&Path, directory:bool, settings:&NativeCopyFolderOptions) -> bool {
    if directory {
        if artwork_folder(path) {return settings.art_folders;}
        return settings.other;
    }
    if artwork_type(path) {return settings.artwork;}
    let ext=path.extension().and_then(|s|s.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "log"=>settings.log,"cue"=>settings.cue,"txt"=>settings.txt,
        "nfo"=>settings.nfo,"md"=>settings.md,"pdf"=>settings.pdf,
        "md5"=>settings.md5,_=>settings.other,
    }
}

/// Copy only the explicitly selected companions, never a symlink and never
/// an extraction output or an existing destination file. Even the user's
/// Replace policy cannot make a copied companion overwrite another artifact.
pub(super) fn copy_cue_folder_contents(job:&NativeExtractionJob, temp:&Path,
    cancel:&CancellationToken, controls:&NativeExtractionControl,
    published:&mut Vec<PathBuf>) -> Result<(),String> {
    if job.format!=DiscFormat::CueImage || job.options.copy_folder.enabled_count()==0 {return Ok(());}
    // An admitted CUE can live outside its referenced audio directory. Copy
    // companions beside the actual images, never just beside the sheet.
    let mut roots=BTreeSet::new();
    for segment in &job.cue_segments {
        let folder=segment.image.parent().ok_or("CUE audio image has no directory")?;
        roots.insert(fs::canonicalize(folder).map_err(|e|
            format!("Resolve CUE image companion folder {}: {e}",folder.display()))?);
    }
    if roots.is_empty() {return Ok(());}
    let source_cue_parent=if job.source.is_dir() {Some(job.source.as_path())}
        else {job.source.parent()};
    let source_cue_parent=source_cue_parent.and_then(|p|fs::canonicalize(p).ok());
    let common=if roots.len()>1 {
        // Keep relative album layout for multiple image folders. When the
        // source sheet is elsewhere, the deepest common image ancestor gives
        // disjoint, deterministic paths without naming collisions.
        let mut prefix=source_cue_parent.filter(|p|roots.iter().all(|r|r.starts_with(p)))
            .unwrap_or_else(||roots.iter().next().unwrap().clone());
        for r in &roots {
            while !r.starts_with(&prefix) {
                if !prefix.pop() {return Err("CUE image folders have no shared root".into());}
            }
        }
        Some(prefix)
    } else {None};
    let destination=fs::canonicalize(&job.destination).ok();
    let reserved=job.programs.iter().flat_map(|program|
        native_program_media_paths(job,program)).chain(native_log_targets(job))
        .map(|path|native_destination_collision_key(&path))
        .collect::<HashSet<_>>();
    let mut stack=Vec::new();
    for root in &roots {
        let prefix=common.as_ref().map(|common|root.strip_prefix(common)
            .map(Path::to_path_buf).map_err(|_|"CUE image root escaped common ancestor"))
            .transpose()?.unwrap_or_default();
        let mut entries=fs::read_dir(root).map_err(|e|format!("List CUE companions: {e}"))?
            .map(|entry|entry.map(|e|e.path())).collect::<Result<Vec<_>,_>>()
            .map_err(|e|format!("Read CUE companion entry: {e}"))?;
        entries.sort();
        for source in entries {
            let metadata=fs::symlink_metadata(&source).map_err(|e|
                format!("Inspect CUE companion type: {e}"))?;
            if !metadata.file_type().is_symlink() &&
                includes_companion(&source,metadata.is_dir(),&job.options.copy_folder) {
                stack.push((source,root.clone(),prefix.clone(),0usize));
            }
        }
    }
    stack.reverse();
    let mut ordinal=0usize;
    while let Some((source,root,prefix,depth))=stack.pop() {
        if cancel.is_cancelled() {return Err("Companion copying cancelled".into());}
        if depth>64 {return Err("CUE companion folder nesting exceeds 64 levels".into());}
        let metadata=fs::symlink_metadata(&source)
            .map_err(|e|format!("Inspect companion {}: {e}",source.display()))?;
        if metadata.file_type().is_symlink() {continue;}
        let relative=source.strip_prefix(&root).map_err(|_|"Companion escaped image folder")?;
        let resolved=fs::canonicalize(&source)
            .map_err(|e|format!("Resolve CUE companion {}: {e}",source.display()))?;
        if !resolved.starts_with(&root) {
            return Err("Companion escaped CUE image folder".into());
        }
        // Never scan back into the extraction destination.
        if destination.as_ref().is_some_and(|dest|
            resolved==*dest || resolved.starts_with(dest)) {continue;}
        if metadata.is_dir() {
            let mut entries=fs::read_dir(&source)
                .map_err(|e|format!("List companion folder: {e}"))?
                .map(|entry|entry.map(|e|e.path())).collect::<Result<Vec<_>,_>>()
                .map_err(|e|format!("Read companion folder entry: {e}"))?;
            entries.sort();
            for path in entries.into_iter().rev() {
                stack.push((path,root.clone(),prefix.clone(),depth+1));
            }
            continue;
        }
        if !metadata.is_file() {continue;}
        let target=job.destination.join(prefix.join(relative));
        let target_key=native_destination_collision_key(&target);
        if reserved.contains(&target_key) || fs::symlink_metadata(&target).is_ok() ||
            published.iter().any(|path|native_destination_collision_key(path)==target_key) {
            continue;
        }
        let complete=temp.join(format!("copied-companion-{ordinal}"));
        ordinal+=1;
        fs::copy(&source,&complete)
            .map_err(|e|format!("Copy companion {}: {e}",source.display()))?;
        let cloned=fs::metadata(&complete).map_err(|e|format!("Stat copied companion: {e}"))?;
        if cloned.len()!=metadata.len() || fs::metadata(&source).ok().is_none_or(|m|
            m.len()!=metadata.len() || m.modified().ok()!=metadata.modified().ok()) {
            return Err(format!("CUE companion changed during copying: {}",source.display()));
        }
        let mut companion_job=job.clone();
        companion_job.overwrite_policy=NativeOverwritePolicy::Fail;
        controls.set_attempted_output(target.clone());
        publish_finished_file_controlled(&companion_job,&complete,&target,cancel,cancel,controls)?;
        controls.clear_attempted_output();
        published.push(target);
        let _=fs::remove_file(&complete);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn common_chapter_format_and_cue_rounding() {
        let rows=vec![("First".into(),0),("Second".into(),44_100)];
        let chapters=chapter_text(&rows,44_100).unwrap();
        assert!(chapters.contains("CHAPTER01=00:00:00.000"));
        assert!(chapters.contains("CHAPTER02=00:00:01.000"));
        assert!(chapters.contains("CHAPTER02NAME=Second"));
        let cue=cue_text("audio.ac3",&[("First".into(),0,true),("Second".into(),44_100,false)],44_100).unwrap();
        assert!(cue.contains("FILE \"audio.ac3\" WAVE"));
        assert!(cue.contains("FLAGS PRE"));
        assert!(cue.contains("INDEX 01 00:01:00"));
        let delayed=chapter_text(&[("Late".into(),22_050)],44_100).unwrap();
        assert!(delayed.contains("CHAPTER01=00:00:00.500"));
    }
    #[test]
    fn companion_defaults_match_the_checked_boxes() {
        let options=NativeCopyFolderOptions::default();
        assert_eq!(options.enabled_count(),7);
        assert!(!includes_companion(Path::new("Album.cue"),false,&options));
        assert!(includes_companion(Path::new("notes.pdf"),false,&options));
        assert!(includes_companion(Path::new("Scans"),true,&options));
    }

    #[test]
    fn r2d05_companions_come_from_audio_image_folder_and_keep_output_ownership() {
        let temp=tempfile::tempdir().unwrap();
        let album=temp.path().join("Album");
        let audio=album.join("Audio");
        fs::create_dir_all(&audio).unwrap();
        let cue=album.join("album.cue");fs::write(&cue,b"cuesheet").unwrap();
        let source=audio.join("disc.flac");fs::write(&source,b"original input").unwrap();
        fs::write(audio.join("cover.jpg"),b"artwork").unwrap();
        fs::write(audio.join("rip.log"),b"EAC log").unwrap();
        fs::write(audio.join("original.cue"),b"unchecked cue").unwrap();
        fs::write(audio.join("original.md5"),b"unchecked checksum").unwrap();
        fs::write(audio.join("extraction.log"),b"do not overwrite extraction log").unwrap();
        let destination=temp.path().join("destination");
        let mut job=super::super::tests::fixture_job(&destination,temp.path());
        job.format=DiscFormat::CueImage;
        job.source=cue.clone();
        job.extraction_log=true;
        job.program_subfolders=false;
        job.programs[0].id=PresentationId::CueAlbum(1);
        let segment=NativeCueSegment {
            presentation:job.programs[0].id,number:1,cue_path:cue.clone(),
            image:source.clone(),start_sample:0,end_sample:10,index00_sample:None,
            index01_sample:0,pre_emphasis:false,hidden_track_published:false,
            appended_gap:None,total_samples:10,sample_rate:44_100,
            extension:"flac".into(),encoder:"flac".into(),muxer:"flac".into(),
            sample_bits:Some(16),channels:2,source_length:14,source_modified:None,
            cue_length:8,cue_modified:None,source_bit_rate:None,
            packet_cut_samples:Vec::new(),packet_part_index:0,
        };
        job.cue_segments.push(segment.clone());
        let private=temp.path().join("scratch");fs::create_dir_all(&private).unwrap();
        let mut published=Vec::new();
        copy_cue_folder_contents(&job,&private,&CancellationToken::new(),
            &NativeExtractionControl::default(),&mut published).unwrap();
        assert_eq!(fs::read(destination.join("cover.jpg")).unwrap(),b"artwork");
        assert_eq!(fs::read(destination.join("rip.log")).unwrap(),b"EAC log");
        assert!(!destination.join("original.cue").exists());
        assert!(!destination.join("original.md5").exists());
        assert!(!destination.join("extraction.log").exists());
        // Two physical image folders retain separate relative locations.
        let second=album.join("Other");fs::create_dir_all(&second).unwrap();
        let image2=second.join("disc2.flac");fs::write(&image2,b"other input").unwrap();
        fs::write(second.join("cover.jpg"),b"other artwork").unwrap();
        job.cue_segments.push(NativeCueSegment {image:image2,..segment});
        job.destination=temp.path().join("multi");
        fs::create_dir_all(job.destination.join("Audio")).unwrap();
        fs::write(job.destination.join("Audio/cover.jpg"),b"existing user art").unwrap();
        published.clear();
        copy_cue_folder_contents(&job,&private,&CancellationToken::new(),
            &NativeExtractionControl::default(),&mut published).unwrap();
        assert_eq!(fs::read(job.destination.join("Audio/cover.jpg")).unwrap(),b"existing user art");
        assert_eq!(fs::read(job.destination.join("Other/cover.jpg")).unwrap(),b"other artwork");
        assert!(!job.destination.join("Audio/extraction.log").exists());
    }

    #[test]
    fn r2d03_wav_preemphasis_overlay_preserves_pcm_and_is_readable() {
        let temp=tempfile::tempdir().unwrap();
        let wav=temp.path().join("track.wav");
        let payload=vec![0x40u8;96_000];
        let mut bytes=b"RIFF".to_vec();bytes.extend_from_slice(&(36u32+payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());bytes.extend_from_slice(&96_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);fs::write(&wav,bytes).unwrap();
        assert!(crate::tui::preemphasis::metadata::check_pre_flag_tag_evidence(&wav).is_none());
        write_preemphasis_tag(&wav).unwrap();
        assert!(crate::tui::preemphasis::metadata::check_pre_flag_tag_evidence(&wav).is_some());
        // The audio image is read by the real WAV decoder, not compared by
        // fixed offsets that can move after adding an ID3v2 chunk.
        let decoded=std::process::Command::new("ffmpeg")
            .args(["-hide_banner","-nostdin","-loglevel","error","-i"])
            .arg(&wav).args(["-map","0:a:0","-c:a","pcm_s16le","-f","s16le","-"])
            .output();
        if let Ok(result)=decoded {
            assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
            assert_eq!(result.stdout,payload);
        }
    }

}

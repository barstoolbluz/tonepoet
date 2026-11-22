//! Unmodified archive-member extraction through the durable native file task.
//!
//! 7z unpacks into private working storage. Each selected *regular* member is
//! copied without interpreting its format, then handed to the existing atomic
//! publisher. Metadata affects output names only; it never touches file bytes.

use super::*;
use std::io::Read;
use crate::tui::archive_extract::{regular_archive_file, safe_archive_relative_path};

pub(super) fn proposed_archive_destination(
    job: &NativeExtractionJob, program: &NativeProgramChoice, track: &NativeTrackChoice,
) -> PathBuf {
    let Some(member) = job.archive_members.iter().find(|item| item.number == track.number) else {
        return job.destination.join("invalid-archive-selection");
    };
    let relative = &member.relative_path;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let original_name = relative.file_name().unwrap_or_default();
    if !job.transfer_tags || !member.tagged_audio {
        return job.destination.join(parent).join(original_name);
    }
    let extension = relative.extension().and_then(|value| value.to_str()).unwrap_or("");
    let mut named_track=track.clone();
    named_track.number=member.tag_track_number.filter(|number|*number>0).unwrap_or(track.number);
    // The list label may be a source-relative path. When TITLE is absent,
    // native naming must fall back to the basename alone, without its suffix.
    named_track.name=relative.file_stem().unwrap_or(original_name).to_string_lossy().into_owned();
    let mut named_program=program.clone();
    let fields=job.tag_fields;
    let filter=|tags:&mut NativeTagValues| {
        if fields & (1<<0)==0 {tags.title=None;}
        if fields & (1<<1)==0 {tags.artist=None;}
        if fields & (1<<2)==0 {tags.album=None;}
        if fields & (1<<3)==0 {tags.genre=None;}
        if fields & (1<<4)==0 {tags.year=None;}
        if fields & (1<<5)==0 {tags.composer=None;}
        if fields & (1<<6)==0 {tags.performer=None;}
        if fields & (1<<7)==0 {tags.catalog=None;}
        if fields & (1<<8)==0 {tags.publisher=None;}
    };
    filter(&mut named_track.tags);
    filter(&mut named_program.tags);
    // The shared %CATALOG% resolver uses disc identity. Supply this archive
    // member's own catalog only for this naming call; no other source changes.
    let mut identity=job.disc_identity.clone();
    identity.catalog=named_track.tags.catalog.clone();
    let mut stem = render_naming_template_for_extension(
        &job.naming_template, &named_program, &named_track, extension, &identity,
    );
    if job.capitalize_naming {
        stem = crate::convert::renaming::capitalize_naming_path(&stem, Some(extension));
    }
    job.destination.join(parent).join(with_native_extension(stem, extension))
}

pub(super) fn validate_archive_job(job: &NativeExtractionJob) -> Result<(), String> {
    if job.programs.len() != 1 || job.programs[0].id != PresentationId::ArchiveFiles {
        return Err("Archive extraction requires one archive-file selection".into());
    }
    if job.file_per_program || job.cue || job.downmix.is_active() || job.assume_decrypted
        || job.sacd_dff || job.programs[0].keep_dst || job.program_subfolders {
        return Err("Archive extraction copies original members, without disc or conversion options".into());
    }
    let info = fs::symlink_metadata(&job.source)
        .map_err(|error| format!("Cannot inspect archive source: {error}"))?;
    if !info.file_type().is_file() || info.file_type().is_symlink() ||
        info.len() != job.archive_length || info.modified().ok() != job.archive_modified {
        return Err("Archive changed after preview; reopen Extract".into());
    }
    if job.archive_members.len() != job.programs[0].tracks.len() {
        return Err("Archive members do not match selected files".into());
    }
    let mut numbers = HashSet::new();
    let mut paths = HashSet::new();
    for member in &job.archive_members {
        safe_archive_relative_path(&member.relative_path)?;
        if !numbers.insert(member.number) ||
            !paths.insert(member.relative_path.to_string_lossy().to_lowercase()) {
            return Err("Archive selection repeats an entry".into());
        }
        let track = job.programs[0].tracks.iter().find(|track| track.number == member.number)
            .ok_or("Archive member has no selected file")?;
        if track.estimated_bytes != Some(member.size) {
            return Err("Archive member size differs from preview".into());
        }
    }
    Ok(())
}

pub(super) async fn probe_selected_sources(
    job: &NativeExtractionJob, cancel: &CancellationToken,
) -> Result<NativeProbeFacts, String> {
    if cancel.is_cancelled() { return Err("Archive preflight cancelled".into()); }
    validate_archive_job(job)?;
    Ok(NativeProbeFacts::default())
}

pub(super) async fn execute_inner(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    controls: Arc<NativeExtractionControl>,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    validate_job(&job)?;
    controls.execution_started.store(true, Ordering::Release);
    let work_root = native_working_storage(&job.working_root, &job.destination)?;
    fs::create_dir_all(&work_root).map_err(|error|format!("Cannot create private working storage: {error}"))?;
    let temporary = tempfile::Builder::new().prefix("tonepoet-archive-extract-")
        .tempdir_in(&work_root).map_err(|error|format!("Private extraction storage: {error}"))?;
    let extracted = temporary.path().join("unpacked");
    fs::create_dir(&extracted).map_err(|error|format!("Archive staging: {error}"))?;
    let runner = RealToolRunner::new(HashMap::new()).with_native_pause(controls.tool_pause());
    let program = &job.programs[0];
    let total = program.tracks.len();
    let estimated_bytes_total = program.tracks.iter()
        .try_fold(0_u64,|total,track|total.checked_add(track.estimated_bytes?));
    let mut published = Vec::new();
    let mut done = 0_usize;
    let mut skipped = 0_usize;
    let mut estimated_bytes_done = 0_u64;
    let mut estimated_bytes_published = 0_u64;
    let mut estimated_bytes_skipped = 0_u64;
    let mut emit = |current:String,current_file:Option<PathBuf>,current_bytes:Option<u64>,
        done:usize,skipped:usize,bytes_done:u64,bytes_published:u64,bytes_skipped:u64,
        published_paths:Vec<PathBuf>| {
        on_progress(NativeExtractionProgress {done,total,published:published_paths.len(),
            published_paths,skipped,current,current_file,current_estimated_bytes:current_bytes,
            estimated_bytes_done:bytes_done,estimated_bytes_published:bytes_published,
            estimated_bytes_skipped:bytes_skipped,estimated_bytes_total});
    };
    emit("Unpacking archive in private storage".into(),None,None,0,0,0,0,0,Vec::new());
    controls.watch_tree(extracted.clone(),"Unpacking archive".into(),None);
    crate::convert::pipeline::materializer_archive::extract_archive_to_staging(
        &job.source,&extracted,"native-archive-extract",job.archive_password.as_deref(),
        &runner,None,&cancel,
    ).await.map_err(|error|format!("Cannot unpack archive: {error}"))?;
    controls.clear_watch();
    if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
    validate_archive_job(&job)?;

    for (index,track) in program.tracks.iter().enumerate() {
        if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
        let member = job.archive_members.iter().find(|item|item.number == track.number)
            .ok_or("Archived member disappeared from selection")?;
        let target = proposed_archive_destination(&job,program,track);
        if job.overwrite_policy == NativeOverwritePolicy::Skip && fs::symlink_metadata(&target).is_ok() {
            controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                .with_reason(NativeSkipReason::ExistingFile))?;
            done += 1;
            skipped += 1;
            estimated_bytes_done = estimated_bytes_done.saturating_add(member.size);
            estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(member.size);
            emit(format!("Skipped {}",track.name),None,None,done,skipped,
                estimated_bytes_done,estimated_bytes_published,estimated_bytes_skipped,published.clone());
            continue;
        }
        let item_cancel = controls.begin_item(&cancel);
        controls.set_attempted_output(target.clone());
        let completed = temporary.path().join(format!("completed-{index}"));
        let result: Result<(),String> = async {
            controls.wait_ready(&cancel,&item_cancel).await?;
            let original = regular_archive_file(&extracted,&member.relative_path)?;
            let original_size = fs::metadata(&original).map_err(|error|
                format!("Inspect archived file '{}': {error}",member.relative_path.display()))?.len();
            if original_size != member.size {
                return Err(format!("Archive entry changed size: {}",member.relative_path.display()));
            }
            controls.watch_file(completed.clone(),format!("Copying {}",track.name),Some(member.size),false);
            emit(format!("Copying {}",track.name),Some(completed.clone()),Some(member.size),
                done,skipped,estimated_bytes_done,estimated_bytes_published,estimated_bytes_skipped,published.clone());
            let source = original;
            let target_file = completed.clone();
            let controls_for_copy = Arc::clone(&controls);
            let copy_cancel = cancel.clone();
            let skip_cancel = item_cancel.clone();
            let verify = job.options.verify_after && native_is_audio_target(&target);
            let captured = tokio::task::spawn_blocking(move || -> Result<Option<[u8;32]>,String> {
                let mut input = File::open(&source).map_err(|e|format!("Open archived file: {e}"))?;
                let mut output = OpenOptions::new().write(true).create_new(true)
                    .open(&target_file).map_err(|e|format!("Stage archived file: {e}"))?;
                let mut digest = verify.then(Sha256::new);
                let mut buffer = [0_u8; 256*1024];
                loop {
                    if controls_for_copy.wait_blocking(&copy_cancel,&skip_cancel) {
                        return Err("Archive copy interrupted".into());
                    }
                    let read = input.read(&mut buffer).map_err(|e|format!("Read archived member: {e}"))?;
                    if read == 0 {break;}
                    output.write_all(&buffer[..read]).map_err(|e|format!("Stage archived member: {e}"))?;
                    if let Some(digest)=digest.as_mut() {digest.update(&buffer[..read]);}
                }
                output.sync_all().map_err(|e|format!("Sync archived member: {e}"))?;
                Ok(digest.map(|digest|digest.finalize().into()))
            }).await.map_err(|error|format!("Archived-file copy task: {error}"))??;
            if let Some(digest) = captured {
                controls.record_identity_fingerprint(&completed,digest);
            }
            if fs::metadata(&completed).map_err(|error|format!("Copied file missing: {error}"))?.len()!=member.size {
                return Err(format!("Archived member copy length mismatch: {}",member.relative_path.display()));
            }
            controls.wait_ready(&cancel,&item_cancel).await?;
            publish_finished_file_controlled(&job,&completed,&target,&cancel,&item_cancel,&controls)?;
            published.push(target);
            estimated_bytes_published = estimated_bytes_published.saturating_add(member.size);
            emit(format!("Published {}",track.name),None,None,done,skipped,
                estimated_bytes_done,estimated_bytes_published,estimated_bytes_skipped,published.clone());
            Ok(())
        }.await;
        controls.clear_watch();
        let requested_skip = controls.finish_item();
        if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
        if requested_skip && result.is_err() {
            controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                .with_reason(NativeSkipReason::RequestedByUser))?;
            skipped += 1;
            estimated_bytes_skipped = estimated_bytes_skipped.saturating_add(member.size);
        } else {result?;}
        controls.clear_attempted_output();
        done += 1;
        estimated_bytes_done = estimated_bytes_done.saturating_add(member.size);
        emit(track.name.clone(),None,None,done,skipped,estimated_bytes_done,
            estimated_bytes_published,estimated_bytes_skipped,published.clone());
    }
    if job.extraction_log {
        for (index,log_target) in native_log_targets(&job).into_iter().enumerate() {
            let log = temporary.path().join(format!("extraction-receipt-{index}.log"));
            let content = native_log_content_for_index(&job,index,&published,
                &controls.skipped_receipts(),&[],NativeLogOutcome::Complete,None);
            let mut writer = OpenOptions::new().create_new(true).write(true).open(&log)
                .map_err(|error|format!("Cannot stage extraction log: {error}"))?;
            writer.write_all(content.as_bytes()).and_then(|()|writer.sync_all())
                .map_err(|error|format!("Cannot finish extraction log: {error}"))?;
            publish_finished_file_controlled(&job,&log,&log_target,&cancel,&cancel,&controls)?;
            published.push(log_target);
            emit("Published extraction log".into(),None,None,done,skipped,estimated_bytes_done,
                estimated_bytes_published,estimated_bytes_skipped,published.clone());
        }
    }
    Ok(NativeExtractionReport{published,skipped})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive_job() -> (tempfile::TempDir, NativeExtractionJob) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("archive.zip");
        fs::write(&source, b"archive-test-source").unwrap();
        let info = fs::symlink_metadata(&source).unwrap();
        let mut job = super::super::tests::fixture_job(&root.path().join("Extracted"), root.path());
        job.format = DiscFormat::Archive;
        job.source = source;
        job.archive_length = info.len();
        job.archive_modified = info.modified().ok();
        job.programs[0].id = PresentationId::ArchiveFiles;
        job.programs[0].tracks[0].estimated_bytes = Some(10);
        job.archive_members.push(NativeArchiveMember {
            number: 1, relative_path: PathBuf::from("Disc 1/01 - original.mp3"),
            size: 10, tagged_audio: true, tag_track_number: Some(7),
        });
        job.program_subfolders = false;
        job.naming_template = "%TRACKNUMBER% - %TITLE%".into();
        job.programs[0].tracks[0].tags.title = Some("From embedded tags".into());
        (root, job)
    }

    #[test]
    fn archive_targets_keep_original_paths_when_tag_naming_is_off() {
        let (_root, mut job) = archive_job();
        validate_job(&job).unwrap();
        let program = &job.programs[0];
        let track = &program.tracks[0];
        assert_eq!(proposed_archive_destination(&job, program, track),
            job.destination.join("Disc 1/01 - original.mp3"));
        job.transfer_tags = true;
        let renamed = proposed_archive_destination(&job, &job.programs[0],
            &job.programs[0].tracks[0]);
        assert_eq!(renamed.parent(), Some(job.destination.join("Disc 1").as_path()));
        assert_eq!(renamed.file_name().unwrap().to_string_lossy(),
            "07 - From embedded tags.mp3");
        job.archive_members[0].tagged_audio = false;
        assert_eq!(proposed_archive_destination(&job, &job.programs[0],
            &job.programs[0].tracks[0]), job.destination.join("Disc 1/01 - original.mp3"));
    }

    #[test]
    fn archive_selection_binds_source_snapshot_member_size_and_safe_paths() {
        let (_root, mut job) = archive_job();
        validate_job(&job).unwrap();
        job.archive_members[0].relative_path = PathBuf::from("../outside.mp3");
        assert!(validate_job(&job).is_err());
        job.archive_members[0].relative_path = PathBuf::from("Disc 1/01 - original.mp3");
        job.archive_members[0].size += 1;
        assert!(validate_job(&job).is_err());
        job.archive_members[0].size -= 1;
        job.archive_length += 1;
        assert!(validate_job(&job).is_err());
    }

    #[test]
    fn archive_extended_tag_tokens_respect_each_field_mask() {
        let (_root,mut job)=archive_job();
        let track=&mut job.programs[0].tracks[0];
        track.tags.title=Some("Movement 1".into());
        track.tags.composer=Some("Claude Debussy".into());
        track.tags.performer=Some("Alice Example".into());
        track.tags.catalog=Some("CAT-42".into());
        track.tags.publisher=Some("Test Records".into());
        job.transfer_tags=true;
        job.tag_fields=0x1ff;
        for (template,expected,bit) in [
            ("%COMPOSER% - %TITLE%","Claude Debussy - Movement 1.mp3",5),
            ("%PERFORMER%","Alice Example.mp3",6),
            ("%CATALOG%","CAT-42.mp3",7),
            ("%TITLE% - %COMPOSER%","Movement 1 - Claude Debussy.mp3",5),
        ] {
            job.naming_template=template.into();
            let named=proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
            assert_eq!(named.file_name().unwrap().to_string_lossy(),expected);
            job.tag_fields &= !(1<<bit);
            let suppressed=proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
            assert!(!suppressed.file_name().unwrap().to_string_lossy().contains(match bit {
                5=>"Claude Debussy",6=>"Alice Example",7=>"CAT-42",_=>unreachable!(),
            }),"{template}");
            job.tag_fields |= 1<<bit;
        }
        job.programs[0].tracks[0].tags.catalog=None;
        job.naming_template="%CATALOG%".into();
        let missing=proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        assert!(!missing.file_name().unwrap().to_string_lossy().contains("CAT-42"));
    }

    #[test]
    fn archive_missing_title_uses_basename_not_display_path() {
        let (_root,mut job)=archive_job();
        job.archive_members[0].relative_path=PathBuf::from("Disc 1/01 - Prelude.flac");
        job.programs[0].tracks[0].name="Disc 1/01 - Prelude.flac".into();
        job.programs[0].tracks[0].tags.title=None;
        job.programs[0].tracks[0].tags.artist=Some("An artist".into());
        job.transfer_tags=true;
        job.tag_fields=0x1ff;
        job.naming_template="%TITLE%".into();
        let destination=proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]);
        assert_eq!(destination,job.destination.join("Disc 1/01 - Prelude.flac"));
        job.programs[0].tracks[0].tags.title=Some("Actual Title".into());
        assert_eq!(proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Disc 1/Actual Title.flac"));
        job.transfer_tags=false;
        assert_eq!(proposed_archive_destination(&job,&job.programs[0],&job.programs[0].tracks[0]),
            job.destination.join("Disc 1/01 - Prelude.flac"));
    }

    #[tokio::test]
    async fn real_zip_preserves_renamed_audio_bytes_and_non_audio_sidecars() {
        let available=|binary:&str|std::process::Command::new(binary).arg("-h").output().is_ok();
        if !available("ffmpeg") || !available("zip") ||
            (!available("7zz") && !available("7z")) {
            eprintln!("Skipping real archive fixture: ffmpeg, zip or 7z unavailable");
            return;
        }
        let root=tempfile::tempdir().unwrap();
        let source_dir=root.path().join("album");
        fs::create_dir(&source_dir).unwrap();
        for (file,codec,number,title) in [
            ("sample.oga","libvorbis","7","Movement 1"),
            ("other.mp4","aac","9","Interlude"),
            ("older.mp3","libmp3lame","11","Finale"),
        ] {
            let path=source_dir.join(file);
            let track_key=if file.ends_with(".oga") {"TRACKNUMBER"} else {"track"};
            let cmd=std::process::Command::new("ffmpeg")
                .args(["-hide_banner","-loglevel","error","-nostdin","-y",
                    "-f","lavfi","-i","sine=frequency=440:sample_rate=48000:duration=1",
                    "-c:a",codec,"-metadata",&format!("title={title}"),
                    "-metadata",&format!("{track_key}={number}")])
                .arg(&path).output().unwrap();
            assert!(cmd.status.success(),"Fixture {file}: {}",
                String::from_utf8_lossy(&cmd.stderr));
        }
        let flac=std::process::Command::new("ffmpeg")
            .args(["-hide_banner","-loglevel","error","-nostdin","-y",
                "-f","lavfi","-i","sine=frequency=330:sample_rate=48000:duration=1",
                "-c:a","flac","-metadata","title=Movement 1",
                "-metadata","TRACKNUMBER=4","-metadata","composer=Claude Debussy",
                "-metadata","performer=Alice Example",
                "-metadata","CATALOGNUMBER=CAT-42","-metadata","publisher=Test Records"])
            .arg(source_dir.join("movement.flac")).output().unwrap();
        assert!(flac.status.success(),"FLAC fixture: {}",
            String::from_utf8_lossy(&flac.stderr));
        let untagged=std::process::Command::new("ffmpeg")
            .args(["-hide_banner","-loglevel","error","-nostdin","-y",
                "-f","lavfi","-i","sine=frequency=220:sample_rate=48000:duration=1",
                "-map_metadata","-1","-c:a","pcm_s16le"])
            .arg(source_dir.join("untagged.wav")).output().unwrap();
        assert!(untagged.status.success(),"WAV fixture: {}",
            String::from_utf8_lossy(&untagged.stderr));
        fs::write(source_dir.join("cover.jpg"),b"exact cover art").unwrap();
        let archive=root.path().join("album.zip");
        let zipped=std::process::Command::new("zip").current_dir(&source_dir)
            .args(["-q", "-r"]).arg(&archive).args(["."]).output().unwrap();
        assert!(zipped.status.success(),"zip: {}",String::from_utf8_lossy(&zipped.stderr));
        let preview=crate::tui::archive_extract::inspect(
            &archive,vec![],CancellationToken::new()).await.unwrap();
        let mut job=super::super::tests::fixture_job(&root.path().join("output"),root.path());
        job.format=DiscFormat::Archive;
        job.source=archive;
        job.archive_length=preview.source_length;
        job.archive_modified=preview.source_modified;
        job.archive_password=preview.password;
        job.archive_members=preview.members;
        job.programs[0].id=PresentationId::ArchiveFiles;
        let choices=preview.contents.presentations[0].tracks.iter().enumerate()
            .map(|(index,track)| {
                let tags=&preview.contents.presentations[0].extract.authored_track_tags[index];
                NativeTrackChoice {number:track.number,
                    name:track.title.clone().unwrap_or_default(),
                    estimated_bytes:Some(job.archive_members[index].size),duration_secs:None,
                    tags:NativeTagValues {title:tags.title.clone(),artist:tags.artist.clone(),
                        album:tags.album.clone(),genre:tags.genre.clone(),year:tags.year.clone(),
                        composer:tags.composer.clone(),performer:tags.performer.clone(),
                        catalog:tags.catalog.clone(),publisher:tags.publisher.clone()},
                }
            }).collect();
        job.programs[0].tracks=choices;
        job.program_subfolders=false;
        job.round4_layout=true;
        job.transfer_tags=true;
        job.tag_fields=0x1ff;
        job.naming_template="%TRACKNUMBER% - %TITLE%".into();
        let planned=job.programs[0].tracks.iter().map(|track|
            proposed_archive_destination(&job,&job.programs[0],track)).collect::<Vec<_>>();
        let report=super::super::execute(job.clone(),CancellationToken::new(),|_|{})
            .await.unwrap();
        assert_eq!(report.published.len(),6);
        assert_eq!(report.published.len(),planned.len());
        for (track,output) in job.programs[0].tracks.iter().zip(planned) {
            assert!(report.published.contains(&output));
            let member=job.archive_members.iter().find(|m|m.number==track.number).unwrap();
            let original=source_dir.join(&member.relative_path);
            assert_eq!(fs::read(&output).unwrap(),fs::read(&original).unwrap(),"{output:?}");
            let suffix=member.relative_path.extension().unwrap().to_string_lossy();
            let name=output.file_name().unwrap().to_string_lossy();
            match suffix.as_ref() {
                "jpg" => assert_eq!(name,"cover.jpg"),
                "oga" => assert_eq!(name,"07 - Movement 1.oga"),
                "mp4" => assert_eq!(name,"09 - Interlude.mp4"),
                "mp3" => assert_eq!(name,"11 - Finale.mp3"),
                "flac" => assert_eq!(name,"04 - Movement 1.flac"),
                "wav" => assert_eq!(name,"untagged.wav"),
                _ => unreachable!(),
            }
        }
        // Exercise each extended field through the real archive publisher,
        // not just its destination planner. No audio bytes may change.
        let flac_member=job.archive_members.iter().find(|member|
            member.relative_path.extension().and_then(|ext|ext.to_str())==Some("flac")).unwrap();
        let flac_track=job.programs[0].tracks.iter()
            .find(|track|track.number==flac_member.number).unwrap();
        for (index,(template,name,bit,tag_value)) in [
            ("%COMPOSER% - %TITLE%","Claude Debussy - Movement 1.flac",5,"Claude Debussy"),
            ("%PERFORMER%","Alice Example.flac",6,"Alice Example"),
            ("%CATALOG%","CAT-42.flac",7,"CAT-42"),
        ].into_iter().enumerate() {
            let mut selected=job.clone();
            selected.destination=root.path().join(format!("field-{index}"));
            selected.archive_members=vec![flac_member.clone()];
            selected.programs[0].tracks=vec![flac_track.clone()];
            selected.naming_template=template.into();
            let output=proposed_archive_destination(&selected,&selected.programs[0],
                &selected.programs[0].tracks[0]);
            assert_eq!(output.file_name().unwrap().to_string_lossy(),name);
            let report=super::super::execute(selected.clone(),CancellationToken::new(),|_|{})
                .await.unwrap();
            assert_eq!(report.published,vec![output.clone()]);
            assert_eq!(fs::read(&output).unwrap(),
                fs::read(source_dir.join(&flac_member.relative_path)).unwrap());
            selected.tag_fields &= !(1<<bit);
            let suppressed=proposed_archive_destination(&selected,&selected.programs[0],
                &selected.programs[0].tracks[0]);
            assert!(!suppressed.file_name().unwrap().to_string_lossy().contains(tag_value));
        }
    }

}

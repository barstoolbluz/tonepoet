//! CUE-authoritative extraction through the existing native file task.
//!
//! CUE indexes identify decoded *sample* frames, never approximate seek
//! timestamps. Every selected track is encoded into private scratch using the
//! original image carrier, then independently published with native overwrite,
//! cancellation, skip and journal semantics. This route does not use Convert.

use super::*;

fn segment_for<'a>(job: &'a NativeExtractionJob, id: PresentationId, number: u32)
    -> Result<&'a NativeCueSegment, String>
{
    job.cue_segments.iter().find(|s|s.presentation==id && s.number==number)
        .ok_or_else(|| format!("Missing CUE segment {id:?}/{number}"))
}

fn verify_source(path: &Path, length: u64,
    modified: Option<std::time::SystemTime>) -> Result<(), String>
{
    let metadata = fs::symlink_metadata(path)
        .map_err(|error|format!("CUE source disappeared: {}: {error}",path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() ||
        metadata.len() != length || metadata.modified().ok() != modified {
        return Err(format!("CUE source changed after selection: {}",path.display()));
    }
    Ok(())
}

fn verify_segment(segment: &NativeCueSegment) -> Result<(), String> {
    verify_source(&segment.image,segment.source_length,segment.source_modified)?;
    verify_source(&segment.cue_path,segment.cue_length,segment.cue_modified)?;
    if segment.sample_rate == 0 || segment.start_sample >= segment.end_sample ||
        segment.end_sample > segment.total_samples || segment.extension.is_empty() ||
        segment.encoder.is_empty() || segment.muxer.is_empty() || segment.channels == 0 {
        return Err("Invalid CUE track geometry or missing source encoder".into());
    }
    Ok(())
}

fn output_overwrites_input(target: &Path, inputs: &[&Path]) -> bool {
    let output_key = native_destination_collision_key(target);
    let resolved = fs::canonicalize(target).ok();
    let parent = target.parent().and_then(|parent|fs::canonicalize(parent).ok());
    inputs.iter().any(|input| {
        native_destination_collision_key(input) == output_key ||
        fs::canonicalize(input).ok().is_some_and(|source| {
            resolved.as_ref() == Some(&source) || parent.as_ref().is_some_and(|parent|
                target.file_name().is_some_and(|name| parent.join(name) == source))
        })
    })
}

pub(super) fn validate_cue_job(job: &NativeExtractionJob) -> Result<(), String> {
    if !job.format.eq(&DiscFormat::CueImage) { return Err("Not a CUE extraction".into()); }
    if job.cue || job.file_per_program || job.sacd_dff || job.assume_decrypted ||
        job.downmix.is_active() || job.programs.iter().any(|p|p.keep_dst) {
        return Err("CUE extraction uses split tracks and source-format encoding only".into());
    }
    let selected: usize = job.programs.iter().map(|program|program.tracks.len()).sum();
    if selected == 0 || selected != job.cue_segments.len() {
        return Err("CUE geometry does not match checked tracks".into());
    }
    if job.cue_input_paths.is_empty() || job.cue_segments.iter().any(|s|
        !job.cue_input_paths.contains(&s.image) || !job.cue_input_paths.contains(&s.cue_path)) {
        return Err("CUE job lacks authoritative input protection".into());
    }
    let mut seen = BTreeSet::new();
    for segment in &job.cue_segments {
        if !seen.insert((segment.presentation.compact_label(),segment.number)) {
            return Err("Duplicate CUE segment".into());
        }
        let program = job.programs.iter().find(|p|p.id == segment.presentation)
            .ok_or("CUE segment belongs to an unselected album")?;
        let track = program.tracks.iter().find(|track|track.number == segment.number)
            .ok_or("CUE segment belongs to an unchecked track")?;
        verify_segment(segment)?;
        // A recovered/modified job may point at a different image than at
        // admission. The operation must not become an implicit file overwrite.
        let output = proposed_destination(job,program,track);
        let inputs: Vec<_> = job.cue_input_paths.iter().map(PathBuf::as_path).collect();
        if output_overwrites_input(&output,&inputs) {
            return Err(format!("CUE extraction would overwrite source: {}",output.display()));
        }
    }
    Ok(())
}

pub(super) async fn probe_selected_sources(job: &NativeExtractionJob,
    cancel: &CancellationToken) -> Result<NativeProbeFacts, String>
{
    if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
    validate_cue_job(job)?;
    let segment = job.cue_segments.first().ok_or("No selected CUE segments")?;
    let program = job.programs.iter().find(|p|p.id==segment.presentation)
        .ok_or("Missing CUE presentation")?;
    Ok(NativeProbeFacts {bit_depth:program.source_depth,
        sample_rate:program.source_rate,channels:program.source_channels})
}

fn utf8_path(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(||format!(
        "FFmpeg cannot address non-UTF8 CUE path: {}",path.display()))
}

fn track_tags(job: &NativeExtractionJob, program: &NativeProgramChoice,
    track: &NativeTrackChoice) -> NativeTagValues
{
    let authored = program.authored_tags.as_ref().map(|all| {
        all.tracks.get(&track.number).cloned().unwrap_or_default().prioritized(&all.album)
    }).unwrap_or_default();
    match job.tag_source {
        NativeTagSource::AuthoredTrack => authored.prioritized(&track.tags).prioritized(&program.tags),
        NativeTagSource::DiscAndSidecar | NativeTagSource::MusicBrainz =>
            track.tags.prioritized(&program.tags).prioritized(&authored),
    }
}

fn encode_command(job: &NativeExtractionJob, program: &NativeProgramChoice,
    track: &NativeTrackChoice, segment: &NativeCueSegment, output: &Path)
    -> Result<ToolCommand,String>
{
    let mut args = vec!["-hide_banner","-nostdin","-loglevel","error","-xerror","-y"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    args.extend(["-i".into(),utf8_path(&segment.image)?,
        "-map".into(),"0:a:0".into(),"-vn".into(),"-sn".into(),"-dn".into(),
        "-map_metadata".into(),"-1".into(),"-map_chapters".into(),"-1".into(),
        "-af".into(),format!("atrim=start_sample={}:end_sample={},asetpts=PTS-STARTPTS",
            segment.start_sample,segment.end_sample),
        "-ar".into(),segment.sample_rate.to_string(),
        "-c:a".into(),segment.encoder.clone()]);
    if segment.muxer == "wav" {
        // Large split WAVs must remain writable without a 4 GiB RIFF ceiling.
        args.extend(["-rf64".into(),"auto".into()]);
    }
    if segment.encoder == "flac" && segment.sample_bits.unwrap_or(16)>16 {
        // FLAC can encode 24-bit integers from FFmpeg's s32 planar input.
        args.extend(["-sample_fmt".into(),"s32".into(),
            "-bits_per_raw_sample".into(),"24".into()]);
    }
    if job.transfer_tags {
        let tags = track_tags(job,program,track);
        let fields = [
            (0,"title",tags.title.as_ref()),(1,"artist",tags.artist.as_ref()),
            (2,"album",tags.album.as_ref()),(3,"genre",tags.genre.as_ref()),
            (4,"date",tags.year.as_ref()),(5,"composer",tags.composer.as_ref()),
            (6,"performer",tags.performer.as_ref()),(7,"catalog",tags.catalog.as_ref()),
            (8,"publisher",tags.publisher.as_ref()),
        ];
        for (bit,name,value) in fields {
            if job.tag_fields & (1<<bit) != 0 {
                if let Some(value)=value.filter(|v|!v.trim().is_empty()) {
                    args.extend(["-metadata".into(),format!("{name}={value}")]);
                }
            }
        }
        args.extend(["-metadata".into(),format!("track={}",track.number)]);
    }
    args.extend(["-f".into(),segment.muxer.clone(),utf8_path(output)?]);
    Ok(ToolCommand {binary:ToolBinary::Ffmpeg,args,secret_args:Vec::new(),cwd:None,
        environment_policy:tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env:Vec::new(),timeout:Duration::from_secs(6*60*60)})
}

pub(super) async fn execute_inner(
    job: NativeExtractionJob, cancel: CancellationToken,
    controls: Arc<NativeExtractionControl>,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport,String> {
    validate_job(&job)?;
    controls.execution_started.store(true,Ordering::Release);
    let work = native_working_storage(&job.working_root,&job.destination)?;
    fs::create_dir_all(&work).map_err(|e|format!("Create CUE working storage: {e}"))?;
    let temp = tempfile::Builder::new().prefix("tonepoet-cue-").tempdir_in(&work)
        .map_err(|e|format!("Private CUE storage: {e}"))?;
    let runner = RealToolRunner::new(HashMap::new()).with_native_pause(controls.tool_pause());
    let total: usize = job.programs.iter().map(|p|p.tracks.len()).sum();
    let estimated_bytes_total = job.programs.iter().flat_map(|p|&p.tracks)
        .try_fold(0u64,|sum,track|sum.checked_add(track.estimated_bytes?));
    let mut published = Vec::new();
    let (mut done,mut skipped) = (0usize,0usize);
    let (mut bytes_done,mut bytes_skipped,mut bytes_published) = (0u64,0u64,0u64);
    let mut emit = |current: String,current_file: Option<PathBuf>,estimate:Option<u64>,
        done:usize,skipped:usize,published:&Vec<PathBuf>,
        bytes_done:u64,bytes_skipped:u64,bytes_published:u64| {
        on_progress(NativeExtractionProgress {done,total,published:published.len(),
            published_paths:published.clone(),skipped,current,current_file,
            current_estimated_bytes:estimate,estimated_bytes_done:bytes_done,
            estimated_bytes_skipped:bytes_skipped,estimated_bytes_published:bytes_published,
            estimated_bytes_total});
    };
    emit("Checking selected CUE tracks".into(),None,None,done,skipped,&published,
        bytes_done,bytes_skipped,bytes_published);
    for (pi,program) in job.programs.iter().enumerate() {
        for track in &program.tracks {
            if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
            let segment = segment_for(&job,program.id,track.number)?;
            let target = proposed_destination(&job,program,track);
            let estimated = track.estimated_bytes.unwrap_or(0);
            let item = controls.begin_item(&cancel);
            controls.set_attempted_output(target.clone());
            if job.overwrite_policy == NativeOverwritePolicy::Skip &&
                fs::symlink_metadata(&target).is_ok() {
                controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                    .with_reason(NativeSkipReason::ExistingFile))?;
                done+=1; skipped+=1;
                bytes_done=bytes_done.saturating_add(estimated);
                bytes_skipped=bytes_skipped.saturating_add(estimated);
                controls.finish_item(); controls.clear_attempted_output();
                emit(format!("Skipped existing {}",track.name),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
                continue;
            }
            verify_segment(segment)?;
            let completed = temp.path().join(format!("cue-{pi}-{}.{}",track.number,segment.extension));
            controls.watch_file(completed.clone(),format!("Encoding {}",track.name),
                track.estimated_bytes,false);
            emit(format!("Encoding {}",track.name),Some(completed.clone()),track.estimated_bytes,
                done,skipped,&published,bytes_done,bytes_skipped,bytes_published);
            let result: Result<(),String> = async {
                runner.run(encode_command(&job,program,track,segment,&completed)?,&item)
                    .await.map_err(|error|format!("CUE re-encoding: {error}"))?;
                controls.wait_ready(&cancel,&item).await?;
                let meta = fs::metadata(&completed).map_err(|error|
                    format!("CUE output was not encoded: {error}"))?;
                if meta.len()==0 { return Err("CUE encoder produced an empty file".into()); }
                if matches!(segment.extension.as_str(),"flac"|"wav"|"aiff"|"wv") {
                    // Lossless carriers must have the exact decoded CUE frame
                    // count; a successful encoder exit alone is not proof.
                    let (frames,rate) = crate::tui::accuraterip::probe_sample_count(&completed)?;
                    if rate != segment.sample_rate || frames != segment.end_sample-segment.start_sample {
                        return Err(format!("Encoded CUE frame count mismatch for {}: expected {}, got {}",
                            track.name,segment.end_sample-segment.start_sample,frames));
                    }
                }
                if job.transfer_tags && job.tag_artwork &&
                    matches!(segment.extension.as_str(),"flac"|"mp3"|"m4a"|"ogg"|"opus") {
                    if let Some(artwork) = native_artwork_path(&AlbumMetadata::default(),&segment.cue_path) {
                        let completed_copy = completed.clone();
                        tokio::task::spawn_blocking(move ||
                            write_completed_native_metadata(&completed_copy,&[],Some(&artwork)))
                            .await.map_err(|error|format!("CUE artwork embedding task: {error}"))??;
                    }
                }
                File::open(&completed).and_then(|file|file.sync_all())
                    .map_err(|error|format!("Sync encoded CUE track: {error}"))?;
                verify_segment(segment)?;
                controls.wait_ready(&cancel,&item).await?;
                // Recheck source aliases immediately before Replace. The
                // destination may have acquired a symlink since preflight.
                let inputs = job.cue_input_paths.iter().map(PathBuf::as_path)
                    .collect::<Vec<_>>();
                if output_overwrites_input(&target,&inputs) {
                    return Err(format!("CUE extraction would overwrite source: {}",target.display()));
                }
                publish_finished_file_controlled(&job,&completed,&target,&cancel,&item,&controls)?;
                let _ = fs::remove_file(&completed);
                published.push(target.clone());
                bytes_published=bytes_published.saturating_add(estimated);
                emit(format!("Published {}",track.name),None,None,done,skipped,&published,
                    bytes_done,bytes_skipped,bytes_published);
                Ok(())
            }.await;
            controls.clear_watch();
            let user_skipped = controls.finish_item();
            if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
            if user_skipped && result.is_err() {
                controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                    .with_reason(NativeSkipReason::RequestedByUser))?;
                done+=1; skipped+=1;
                bytes_done=bytes_done.saturating_add(estimated);
                bytes_skipped=bytes_skipped.saturating_add(estimated);
                controls.clear_attempted_output();
                emit(format!("Skipped {}",track.name),None,None,done,skipped,&published,
                    bytes_done,bytes_skipped,bytes_published);
                continue;
            }
            result?;
            controls.clear_attempted_output();
            done+=1;
            bytes_done=bytes_done.saturating_add(estimated);
            emit(format!("Completed {}",track.name),None,None,done,skipped,&published,
                bytes_done,bytes_skipped,bytes_published);
        }
    }
    if job.extraction_log {
        for (index,target) in native_log_targets(&job).into_iter().enumerate() {
            let content = native_log_content_for_index(&job,index,&published,
                &controls.skipped_receipts(),&[],NativeLogOutcome::Complete,None);
            publish_native_recovery_sidecar(&job,&target,&content,&cancel,&controls)?;
            published.push(target);
            emit("Published extraction log".into(),None,None,done,skipped,&published,
                bytes_done,bytes_skipped,bytes_published);
        }
    }
    Ok(NativeExtractionReport {published,skipped})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cue_range_cannot_be_empty_or_past_eof() {
        let s = NativeCueSegment { presentation:PresentationId::CueAlbum(1),number:1,
            cue_path:PathBuf::from("none"),image:PathBuf::from("none"),start_sample:200,
            end_sample:200,total_samples:300,sample_rate:44100,extension:"flac".into(),
            encoder:"flac".into(),muxer:"flac".into(),sample_bits:Some(16),channels:2,
            source_length:0,source_modified:None,cue_length:0,cue_modified:None};
        assert!(verify_segment(&s).is_err());
    }

    #[test]
    fn replace_never_targets_a_source_even_through_a_parent_alias() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Album.flac");
        fs::write(&source,b"source-must-survive").unwrap();
        assert!(output_overwrites_input(&source,&[source.as_path()]));
        assert!(output_overwrites_input(&temp.path().join("./Album.flac"),
            &[source.as_path()]));
        assert!(!output_overwrites_input(&temp.path().join("track.flac"),
            &[source.as_path()]));
    }
}

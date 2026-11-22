//! CUE-authoritative extraction through the existing native file task.
//!
//! CUE indexes identify sample-clock boundaries. Lossless tracks are trimmed
//! and encoded in their original format; lossy tracks copy complete compressed
//! packets, with boundaries rounded to packet edges. Both routes stage privately
//! and share native publication, cancellation and skip controls, not Convert.

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
    if segment.index00_sample.is_some_and(|start|start>segment.index01_sample)
        || segment.index01_sample>=segment.total_samples {
        return Err("Invalid CUE INDEX 00/01 snapshot".into());
    }
    if let Some(extra)=&segment.appended_gap {
        verify_source(&extra.image,extra.source_length,extra.source_modified)?;
        if extra.end_sample==0 || extra.end_sample>=extra.total_samples ||
            extra.image==segment.image || is_lossy(segment) {
            return Err("Invalid CUE cross-image appended interval".into());
        }
    }
    if is_lossy(segment) {
        if segment.packet_part_index > segment.packet_cut_samples.len() ||
            segment.packet_cut_samples.iter().any(|cut|*cut==0 || *cut>=segment.total_samples) ||
            segment.packet_cut_samples.windows(2).any(|pair|pair[0]>=pair[1]) {
            return Err("Invalid lossless-packet CUE partition".into());
        }
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
        !job.cue_input_paths.contains(&s.image) || !job.cue_input_paths.contains(&s.cue_path) ||
        s.appended_gap.as_ref().is_some_and(|extra|!job.cue_input_paths.contains(&extra.image))) {
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

/// Lossy CUE images are split by FFmpeg's packet segmenter, never by
/// decoding and re-encoding. A single sequential pass over each image assigns
/// every source audio packet to exactly one part. Verification compares every
/// demuxed compressed packet byte-for-byte before any part can be published.
fn is_lossy(segment: &NativeCueSegment) -> bool {
    matches!(segment.encoder.as_str(), "libmp3lame"|"aac"|"libvorbis"|"libopus")
}

fn packet_segment_paths(segment: &NativeCueSegment, work: &Path, ordinal: usize)
    -> Vec<PathBuf>
{
    (0..=segment.packet_cut_samples.len()).map(|index|
        work.join(format!("cue-packets-{ordinal}-{index:06}.{}",segment.extension))).collect()
}

fn packet_segment_command(segment: &NativeCueSegment, work: &Path, ordinal: usize)
    -> Result<ToolCommand,String>
{
    let mut args: Vec<String> = ["-hide_banner","-nostdin","-loglevel","error","-xerror","-y"]
        .into_iter().map(str::to_owned).collect();
    args.extend(["-i".into(),utf8_path(&segment.image)?,"-map".into(),"0:a:0".into(),
        "-vn".into(),"-sn".into(),"-dn".into(),"-map_metadata".into(),"-1".into(),
        "-c:a".into(),"copy".into()]);
    if segment.packet_cut_samples.is_empty() {
        // The segment muxer defaults to a two-second split without explicit
        // boundaries. One-track CUE images need a direct, single-file remux.
        args.extend(["-f".into(),segment.muxer.clone()]);
        if segment.extension == "mp3" {
            args.extend(["-write_xing".into(),"0".into()]);
        }
        args.push(utf8_path(&packet_segment_paths(segment,work,ordinal)[0])?);
    } else {
        let cuts = segment.packet_cut_samples.iter().map(|sample|format!("{:.9}",
            *sample as f64 / f64::from(segment.sample_rate)))
            .collect::<Vec<_>>().join(",");
        args.extend(["-f".into(),"segment".into(),"-segment_times".into(),cuts,
            "-reset_timestamps".into(),"1".into(),
            "-segment_format".into(),segment.muxer.clone()]);
        // The MP3 muxer otherwise inserts a new Xing frame into each output.
        if segment.extension == "mp3" {
            args.extend(["-segment_format_options".into(),"write_xing=0".into()]);
        }
        args.push(utf8_path(&work.join(format!("cue-packets-{ordinal}-%06d.{}",
            segment.extension)))?);
    }
    Ok(ToolCommand {binary:ToolBinary::Ffmpeg,args,secret_args:Vec::new(),cwd:None,
        environment_policy:tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env:Vec::new(),timeout:Duration::from_secs(6*60*60)})
}

fn packet_input(path: &Path) -> Result<(ffmpeg_next::format::context::Input,usize),String> {
    crate::tui::probe::ensure_ffmpeg_init_pub();
    let input = ffmpeg_next::format::input(path)
        .map_err(|error|format!("Cannot inspect compressed packets of {}: {error}",path.display()))?;
    let index = input.streams().best(ffmpeg_next::media::Type::Audio)
        .ok_or_else(||format!("No audio packets in {}",path.display()))?.index();
    Ok((input,index))
}

#[cfg(test)]
fn verify_packet_partition(source: &Path, parts: &[PathBuf]) -> Result<(),String> {
    verify_packet_partition_receipts(source,parts,false).map(|_|())
}

/// This is the existing source/segmenter packet-identity pass. When Verify is
/// on, its audio-packet digests become producer receipts for the track copies.
/// They are taken before final metadata work and before final publication.
fn verify_packet_partition_receipts(source: &Path, parts: &[PathBuf],
    capture: bool) -> Result<Vec<[u8;32]>,String> {
    let (mut original,source_index)=packet_input(source)?;
    let mut packets=original.packets().filter(|(stream,_)|stream.index()==source_index);
    let mut receipts=Vec::new();
    for part in parts {
        let (mut output,index)=packet_input(part)?;
        let mut count=0usize;
        let mut digest=capture.then(Sha256::new);
        for (_,packet) in output.packets().filter(|(stream,_)|stream.index()==index) {
            let (_,expected)=packets.next().ok_or_else(||format!(
                "Packet-copy split added audio packets: {}",part.display()))?;
            if expected.data()!=packet.data() {
                return Err(format!("Packet-copy split changed audio packet payload: {}",
                    part.display()));
            }
            if let Some(digest)=digest.as_mut() {
                let payload=packet.data().ok_or_else(||format!(
                    "Empty compressed audio packet: {}",part.display()))?;
                digest.update((payload.len() as u64).to_le_bytes());
                digest.update(payload);
            }
            count+=1;
        }
        if count==0 {return Err(format!("CUE packet-cut produced an empty audio part: {}",
            part.display()));}
        if let Some(digest)=digest {receipts.push(digest.finalize().into());}
    }
    if packets.next().is_some() {
        return Err(format!("Packet-copy split lost source audio packets: {}",source.display()));
    }
    Ok(receipts)
}

/// Read back compressed packet payloads without depending on ID3/Vorbis/MP4
/// tag bytes or headers. Both source partitioning and destination readback
/// use the same length-delimited packet fingerprint.
pub(super) fn packet_audio_fingerprint(path: &Path,
    checkpoint: &mut dyn FnMut() -> std::io::Result<()>,
) -> Result<[u8;32],String> {
    let (mut input,index)=packet_input(path)?;
    let mut digest=Sha256::new();
    let mut count=0;
    for (_,packet) in input.packets().filter(|(stream,_)|stream.index()==index) {
        checkpoint().map_err(|e|format!("Packet verification interrupted: {e}"))?;
        let payload=packet.data().ok_or_else(||format!(
            "Missing compressed packet data in {}",path.display()))?;
        digest.update((payload.len() as u64).to_le_bytes());
        digest.update(payload);
        count+=1;
    }
    if count==0 {return Err(format!("No audio packets in {}",path.display()));}
    Ok(digest.finalize().into())
}

fn verify_packet_file_equal(expected: &Path, actual: &Path) -> Result<(),String> {
    let (mut left,li)=packet_input(expected)?;
    let (mut right,ri)=packet_input(actual)?;
    let mut source=left.packets().filter(|(stream,_)|stream.index()==li);
    let mut output=right.packets().filter(|(stream,_)|stream.index()==ri);
    loop {
        match (source.next(),output.next()) {
            (None,None)=>return Ok(()),
            (Some((_,a)),Some((_,b))) if a.data()==b.data()=>{},
            _=>return Err(format!("Tagging altered compressed audio packets in {}",
                actual.display())),
        }
    }
}

fn ape_wav_encoder(bits: Option<u32>) -> Result<&'static str,String> {
    match bits {
        Some(8) => Ok("pcm_u8"),
        Some(16) => Ok("pcm_s16le"),
        Some(24) => Ok("pcm_s24le"),
        Some(32) => Ok("pcm_s32le"),
        _ => Err("Cannot preserve this APE sample depth in the mac WAV input".into()),
    }
}

/// `mac` accepts a seekable WAV input, not decoded PCM on stdin. The input
/// WAV lives in the same private extraction workdir as the final APE. Manage
/// its external process explicitly because ToolBinary is intentionally a
/// closed, Reference-qualified set and cannot be extended for this one path.
#[allow(unsafe_code)] // libc::kill pause/resume signals for the mac child
async fn run_mac_ape(input: &Path, output: &Path, cancel: &CancellationToken,
    item: &CancellationToken, controls: &NativeExtractionControl) -> Result<(),String>
{
    let mut command = tokio::process::Command::new("mac");
    command.arg(input).arg(output).arg("-c2000")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()
        .map_err(|error|format!("Start Monkey's Audio 'mac' encoder: {error}"))?;
    #[cfg(unix)]
    let pid = child.id().and_then(|pid|i32::try_from(pid).ok());
    #[cfg(unix)]
    let mut stopped = false;
    let timeout = tokio::time::sleep(Duration::from_secs(6*60*60));
    tokio::pin!(timeout);
    let result = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|error|
                format!("Wait for mac APE encoder: {error}")),
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                controls.tool_pause().acknowledge(false);
                return Err("Extraction cancelled during APE encoding".into());
            }
            _ = item.cancelled() => {
                let _ = child.kill().await;
                controls.tool_pause().acknowledge(false);
                return Err("Current APE track skipped".into());
            }
            _ = &mut timeout => {
                let _ = child.kill().await;
                controls.tool_pause().acknowledge(false);
                return Err("mac APE encoder exceeded its six-hour timeout".into());
            }
            _ = tokio::time::sleep(Duration::from_millis(40)) => {
                #[cfg(unix)]
                if let Some(pid) = pid {
                    let pause = controls.is_paused();
                    if pause != stopped {
                        let sig = if pause { libc::SIGSTOP } else { libc::SIGCONT };
                        if unsafe { libc::kill(pid,sig) } == 0 {
                            stopped = pause;
                            controls.tool_pause().acknowledge(pause);
                        }
                    }
                }
            }
        }
    };
    #[cfg(unix)]
    if stopped {
        if let Some(pid) = pid { unsafe { libc::kill(pid,libc::SIGCONT); } }
    }
    controls.tool_pause().acknowledge(false);
    let status = result?;
    if !status.success() {
        return Err(format!("mac APE encoder failed with status {status}"));
    }
    Ok(())
}

fn encode_command(job: &NativeExtractionJob, program: &NativeProgramChoice,
    track: &NativeTrackChoice, segment: &NativeCueSegment, output: &Path,
    hash_output: Option<&Path>) -> Result<ToolCommand,String>
{
    let mut args = vec!["-hide_banner","-nostdin","-loglevel","error","-xerror","-y"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    let (encoder,muxer) = if segment.encoder == "mac" {
        (ape_wav_encoder(segment.sample_bits)?.to_string(),"wav")
    } else { (segment.encoder.clone(),segment.muxer.as_str()) };
    args.extend(["-i".into(),utf8_path(&segment.image)?]);
    let split=if hash_output.is_some() {",asplit=2[audio][tap]"}else{"[combined]"};
    let map=if hash_output.is_some() {"[audio]"}else{"[combined]"};
    if let Some(extra)=&segment.appended_gap {
        args.extend(["-i".into(),utf8_path(&extra.image)?,
            "-filter_complex".into(),format!(
                "[0:a:0]atrim=start_sample={}:end_sample={},asetpts=PTS-STARTPTS[one];\
                 [1:a:0]atrim=start_sample=0:end_sample={},asetpts=PTS-STARTPTS[two];\
                 [one][two]concat=n=2:v=0:a=1{split}",
                segment.start_sample,segment.end_sample,extra.end_sample),
            "-map".into(),map.into()]);
    } else if hash_output.is_some() {
        args.extend(["-filter_complex".into(),format!(
            "[0:a:0]atrim=start_sample={}:end_sample={},asetpts=PTS-STARTPTS,asplit=2[audio][tap]",
                segment.start_sample,segment.end_sample),
            "-map".into(),"[audio]".into()]);
    } else {
        args.extend(["-map".into(),"0:a:0".into(),
            "-af".into(),format!("atrim=start_sample={}:end_sample={},asetpts=PTS-STARTPTS",
                segment.start_sample,segment.end_sample)]);
    }
    args.extend(["-vn".into(),"-sn".into(),"-dn".into(),
        "-map_metadata".into(),"-1".into(),"-map_chapters".into(),"-1".into(),
        "-ar".into(),segment.sample_rate.to_string(),"-c:a".into(),encoder]);
    if muxer == "wav" {
        // Large split WAVs must remain writable without a 4 GiB RIFF ceiling.
        args.extend(["-rf64".into(),"auto".into()]);
    }
    if segment.encoder == "flac" && segment.sample_bits.unwrap_or(16)>16 {
        // FLAC can encode 24-bit integers from FFmpeg's s32 planar input.
        args.extend(["-sample_fmt".into(),"s32".into(),
            "-bits_per_raw_sample".into(),"24".into()]);
    }
    if segment.encoder == "tta" && segment.sample_bits == Some(24) {
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
    args.extend(["-f".into(),muxer.to_string(),utf8_path(output)?]);
    if let Some(path)=hash_output {
        // The hash muxer consumes the *same filter output* as the encoder.
        // FFmpeg computes this digest as frames are produced, before any
        // metadata rewrite or publication can affect the completed file.
        args.extend(["-map".into(),"[tap]".into(),"-c:a".into(),"pcm_f64le".into(),
            "-f".into(),"hash".into(),"-hash".into(),"SHA256".into(),
            utf8_path(path)?]);
    }
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
    let total: usize = job.programs.iter().map(|p|p.tracks.len()+
        usize::from(native_cue_hidden_target(&job,p).is_some())).sum();
    let estimated_bytes_total = job.programs.iter().flat_map(|p|&p.tracks)
        .try_fold(0u64,|sum,track|sum.checked_add(track.estimated_bytes?));
    let mut published = Vec::new();
    // One single-pass segmenter execution per distinct compressed image.
    let mut packet_cache: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut packet_receipts: BTreeMap<PathBuf, Vec<[u8;32]>> = BTreeMap::new();
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
        let mut items: Vec<(NativeTrackChoice,NativeCueSegment,PathBuf)> = Vec::new();
        if let Some(target)=native_cue_hidden_target(&job,program) {
            let first=segment_for(&job,program.id,1)?;
            let hidden=NativeCueSegment {number:0,start_sample:0,
                end_sample:first.index01_sample,pre_emphasis:false,
                // The later-image pregap belongs only to its real predecessor.
                appended_gap:None,packet_part_index:0,..first.clone()};
            items.push((NativeTrackChoice {number:0,name:"Hidden Track".into(),
                tags:NativeTagValues::default(),duration_secs:Some(
                    first.index01_sample as f64/f64::from(first.sample_rate)),
                estimated_bytes:None},hidden,target));
        }
        for track in &program.tracks {
            let segment=segment_for(&job,program.id,track.number)?.clone();
            items.push((track.clone(),segment,proposed_destination(&job,program,track)));
        }
        for (track,segment,target) in &items {
            if cancel.is_cancelled() {return Err("Extraction cancelled".into());}
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
            let producer_hash=(job.options.verify_after && !is_lossy(segment)).then(||
                temp.path().join(format!("cue-{pi}-{}-audio.sha256",track.number)));
            controls.watch_file(completed.clone(),format!("Preparing {}",track.name),
                track.estimated_bytes,false);
            emit(format!("Preparing {}",track.name),Some(completed.clone()),track.estimated_bytes,
                done,skipped,&published,bytes_done,bytes_skipped,bytes_published);
            let result: Result<(),String> = async {
                if is_lossy(segment) {
                    if !packet_cache.contains_key(&segment.image) {
                        let ordinal=packet_cache.len();
                        let parts=packet_segment_paths(segment,temp.path(),ordinal);
                        runner.run(packet_segment_command(segment,temp.path(),ordinal)?,&item)
                            .await.map_err(|error|format!("Lossy CUE packet-copy split refused: {error}"))?;
                        controls.wait_ready(&cancel,&item).await?;
                        if !parts.iter().all(|path|path.is_file()) {
                            return Err("Lossy CUE packet-copy split did not produce every part; image refused".into());
                        }
                        let original=segment.image.clone();
                        let check=parts.clone();
                        let verify=job.options.verify_after;
                        let receipts=tokio::task::spawn_blocking(move ||
                            verify_packet_partition_receipts(&original,&check,verify))
                            .await.map_err(|error|format!("Packet-verification task: {error}"))??;
                        if verify {packet_receipts.insert(segment.image.clone(),receipts);}
                        packet_cache.insert(segment.image.clone(),parts);
                    }
                    let part=packet_cache.get(&segment.image)
                        .and_then(|parts|parts.get(segment.packet_part_index))
                        .ok_or("Missing copied CUE packet part")?;
                    fs::copy(part,&completed).map_err(|error|
                        format!("Copy compressed packets into track: {error}"))?;
                    if job.options.verify_after {
                        let digest=packet_receipts.get(&segment.image)
                            .and_then(|receipts|receipts.get(segment.packet_part_index))
                            .copied().ok_or("Missing producer-time packet fingerprint")?;
                        controls.record_packet_fingerprint(&completed,digest);
                    }
                } else if segment.encoder == "mac" {
                    let wav = temp.path().join(format!("cue-{pi}-{}-ape-input.wav",track.number));
                    controls.watch_file(wav.clone(),format!("Decoding {} for APE",track.name),
                        track.estimated_bytes,false);
                    runner.run(encode_command(&job,program,track,segment,&wav,
                        producer_hash.as_deref())?,&item)
                        .await.map_err(|error|format!("CUE APE PCM stage: {error}"))?;
                    controls.wait_ready(&cancel,&item).await?;
                    controls.watch_file(completed.clone(),format!("Encoding {} as APE",track.name),
                        track.estimated_bytes,false);
                    let encode = run_mac_ape(&wav,&completed,&cancel,&item,&controls).await;
                    let _ = fs::remove_file(&wav);
                    encode?;
                } else {
                    runner.run(encode_command(&job,program,track,segment,&completed,
                        producer_hash.as_deref())?,&item)
                        .await.map_err(|error|format!("CUE re-encoding: {error}"))?;
                }
                controls.wait_ready(&cancel,&item).await?;
                if let Some(hash)=producer_hash.as_deref() {
                    controls.record_audio_fingerprint(&completed,
                        native_audio_hash_receipt(hash)?,"pcm_f64le");
                }
                let meta = fs::metadata(&completed).map_err(|error|
                    format!("CUE output was not encoded: {error}"))?;
                if meta.len()==0 { return Err("CUE encoder produced an empty file".into()); }
                if matches!(segment.extension.as_str(),"flac"|"wav"|"aiff"|"wv"|"ape"|"tta") {
                    // Lossless carriers must have the exact decoded CUE frame
                    // count; a successful encoder exit alone is not proof.
                    let (frames,rate) = crate::tui::accuraterip::probe_sample_count(&completed)?;
                    let expected=(segment.end_sample-segment.start_sample)
                        +segment.appended_gap.as_ref().map_or(0,|extra|extra.end_sample);
                    if rate != segment.sample_rate || frames != expected {
                        return Err(format!("Encoded CUE frame count mismatch for {}: expected {}, got {}",
                            track.name,expected,frames));
                    }
                }
                if is_lossy(segment) && job.transfer_tags && segment.extension != "aac" {
                    // Raw ADTS AAC has no ordinary writable tag container.
                    // Never fail an otherwise exact packet copy merely to
                    // attempt embedded metadata on that carrier.
                    let mut fields=selected_tag_items(&track_tags(&job,program,track),job.tag_fields);
                    fields.push((lofty::tag::ItemKey::TrackNumber,Some(track.number.to_string())));
                    let output=completed.clone();
                    tokio::task::spawn_blocking(move ||
                        write_completed_native_metadata(&output,&fields,None))
                        .await.map_err(|error|format!("CUE packet-copy tag task: {error}"))??;
                }
                if segment.encoder == "mac" && job.transfer_tags {
                    let mut fields = selected_tag_items(&track_tags(&job,program,track),job.tag_fields);
                    fields.push((lofty::tag::ItemKey::TrackNumber,Some(track.number.to_string())));
                    let ape = completed.clone();
                    let art = if job.tag_artwork {
                        native_artwork_path(&AlbumMetadata::default(),&segment.cue_path)
                    } else { None };
                    tokio::task::spawn_blocking(move ||
                        write_completed_native_metadata(&ape,&fields,art.as_deref()))
                        .await.map_err(|error|format!("CUE APE tag task: {error}"))??;
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
                // Pre-emphasis is independent of ordinary metadata-transfer
                // preferences. Only positively marked tracks carry the tag.
                if job.options.keep_preemphasis && segment.pre_emphasis &&
                    matches!(segment.extension.as_str(),
                        "wav"|"flac"|"wv"|"ape"|"tta"|"mp3"|"m4a"|"ogg"|"opus") {
                    let output=completed.clone();
                    tokio::task::spawn_blocking(move ||options::write_preemphasis_tag(&output))
                        .await.map_err(|e|format!("Pre-emphasis metadata task: {e}"))??;
                }
                if is_lossy(segment) {
                    let original=packet_cache.get(&segment.image)
                        .and_then(|parts|parts.get(segment.packet_part_index))
                        .ok_or("Missing original packet part after tagging")?.clone();
                    let written=completed.clone();
                    tokio::task::spawn_blocking(move ||verify_packet_file_equal(&original,&written))
                        .await.map_err(|error|format!("Packet recheck task: {error}"))??;
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
    options::copy_cue_folder_contents(&job,temp.path(),&cancel,&controls,&mut published)?;
    emit("Copied selected CUE companions".into(),None,None,done,skipped,&published,
        bytes_done,bytes_skipped,bytes_published);
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
            end_sample:200,index00_sample:None,index01_sample:200,pre_emphasis:false,
            hidden_track_published:false,appended_gap:None,total_samples:300,sample_rate:44100,extension:"flac".into(),
            encoder:"flac".into(),muxer:"flac".into(),sample_bits:Some(16),channels:2,
            source_length:0,source_modified:None,cue_length:0,cue_modified:None,
            source_bit_rate:None,packet_cut_samples:Vec::new(),packet_part_index:0};
        assert!(verify_segment(&s).is_err());
    }

    #[test]
    fn lossy_split_command_never_encodes_or_filters_audio() {
        let segment=NativeCueSegment {
            presentation:PresentationId::CueAlbum(1),number:1,
            cue_path:"a.cue".into(),image:"a.mp3".into(),start_sample:0,
            end_sample:48_000,index00_sample:None,index01_sample:0,pre_emphasis:false,
            hidden_track_published:false,appended_gap:None,total_samples:96_000,sample_rate:48_000,
            extension:"mp3".into(),encoder:"libmp3lame".into(),muxer:"mp3".into(),
            sample_bits:None,channels:2,source_length:0,source_modified:None,
            cue_length:0,cue_modified:None,source_bit_rate:None,
            packet_cut_samples:vec![48_000],packet_part_index:0,
        };
        let command=packet_segment_command(&segment,Path::new("/tmp"),0).unwrap();
        let args=command.args;
        assert!(args.windows(2).any(|part|part==["-c:a","copy"]));
        assert!(args.windows(2).any(|part|part==["-segment_times","1.000000000"]));
        assert!(!args.iter().any(|arg|arg=="-af" || arg=="libmp3lame"));
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

    #[test]
    fn one_track_packet_command_is_direct_copy_for_all_lossy_carriers() {
        for (extension,muxer) in [
            ("mp3","mp3"),("m4a","ipod"),("aac","adts"),
            ("ogg","ogg"),("opus","opus"),
        ] {
            let segment=NativeCueSegment {
                presentation:PresentationId::CueAlbum(1),number:1,
                cue_path:"album.cue".into(),image:PathBuf::from(format!("image.{extension}")),
                start_sample:0,end_sample:288_000,index00_sample:None,index01_sample:0,pre_emphasis:false,
                hidden_track_published:false,appended_gap:None,total_samples:288_000,sample_rate:48_000,
                extension:extension.into(),encoder:"not-used".into(),muxer:muxer.into(),
                sample_bits:None,channels:2,source_length:0,source_modified:None,
                cue_length:0,cue_modified:None,source_bit_rate:None,
                packet_cut_samples:vec![],packet_part_index:0,
            };
            let args=packet_segment_command(&segment,Path::new("/tmp"),5).unwrap().args;
            assert!(args.windows(2).any(|pair|pair==["-c:a","copy"]),"{extension}");
            assert!(args.windows(2).any(|pair|pair==["-f",muxer]),"{extension}");
            assert!(!args.iter().any(|arg|arg=="segment" || arg=="-segment_times" ||
                arg=="-reset_timestamps" || arg=="-af" || arg=="not-used"),"{extension}");
            let expected=Path::new("/tmp").join(format!("cue-packets-5-000000.{extension}"));
            assert_eq!(args.last().map(String::as_str),expected.to_str());
            if extension=="mp3" {
                assert!(args.windows(2).any(|pair|pair==["-write_xing","0"]));
            } else {assert!(!args.iter().any(|arg|arg=="-write_xing"));}
            assert_eq!(packet_segment_paths(&segment,Path::new("/tmp"),5).len(),1);
        }
    }

    #[tokio::test]
    async fn r2d01_two_wav_images_preserve_cross_file_pregap_samples_for_all_gap_modes() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("Skipping real two-image CUE test: ffmpeg unavailable");
            return;
        }
        fn make_wav(path:&Path, samples:&[i16]) {
            let pcm=samples.iter().flat_map(|sample|sample.to_le_bytes()).collect::<Vec<_>>();
            let mut wav=b"RIFF".to_vec();
            wav.extend_from_slice(&(36u32+pcm.len() as u32).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&48_000u32.to_le_bytes());
            wav.extend_from_slice(&96_000u32.to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
            wav.extend_from_slice(&pcm);
            fs::write(path,wav).unwrap();
        }
        fn decode_pcm(path:&Path)->Vec<i16> {
            let output=std::process::Command::new("ffmpeg")
                .args(["-hide_banner","-loglevel","error","-nostdin","-i"])
                .arg(path).args(["-map","0:a:0","-c:a","pcm_s16le","-f","s16le","-"])
                .output().unwrap();
            assert!(output.status.success(),"{}: {}",path.display(),
                String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout.len()%2,0);
            output.stdout.chunks_exact(2)
                .map(|bytes|i16::from_le_bytes([bytes[0],bytes[1]])).collect()
        }
        let root=tempfile::tempdir().unwrap();
        let first=root.path().join("side-a.wav");
        let second=root.path().join("side-b.wav");
        let cue=root.path().join("album.cue");
        let a=vec![1234i16;48_000];
        let b=[vec![-2345i16;96_000],vec![3456i16;48_000]].concat();
        make_wav(&first,&a);make_wav(&second,&b);
        fs::write(&cue,"FILE \"side-a.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\nFILE \"side-b.wav\" WAVE\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n").unwrap();
        let cue_meta=fs::metadata(&cue).unwrap();
        let mut all=Vec::new();
        for (number,image,total,index00,index01) in [
            (1u32,&first,48_000u64,None,0u64),
            (2u32,&second,144_000u64,Some(0u64),96_000u64),
        ] {
            let meta=fs::metadata(image).unwrap();
            all.push(NativeCueSegment {
                presentation:PresentationId::CueAlbum(1),number,
                cue_path:cue.clone(),image:image.clone(),start_sample:index01,
                end_sample:total,index00_sample:index00,index01_sample:index01,
                pre_emphasis:false,hidden_track_published:false,appended_gap:None,
                total_samples:total,sample_rate:48_000,extension:"wav".into(),
                encoder:"pcm_s16le".into(),muxer:"wav".into(),sample_bits:Some(16),
                channels:1,source_length:meta.len(),source_modified:meta.modified().ok(),
                cue_length:cue_meta.len(),cue_modified:cue_meta.modified().ok(),
                source_bit_rate:None,packet_cut_samples:Vec::new(),packet_part_index:0,
            });
        }
        for (mode,label) in [
            (NativeCueBetweenGap::AppendPrevious,"append"),
            (NativeCueBetweenGap::PrependNext,"prepend"),
            (NativeCueBetweenGap::Drop,"drop"),
        ] {
            let mut job=super::super::tests::fixture_job(
                &root.path().join(label),root.path());
            job.format=DiscFormat::CueImage;
            job.source=cue.clone();
            job.programs[0].id=PresentationId::CueAlbum(1);
            job.programs[0].source_rate=Some(48_000);
            job.programs[0].source_depth=Some(16);
            job.programs[0].source_channels=Some(1);
            job.programs[0].source_codec="pcm_s16le".into();
            job.programs[0].tracks=vec![
                NativeTrackChoice {number:1,name:"First".into(),tags:NativeTagValues::default(),
                    duration_secs:None,estimated_bytes:None},
                NativeTrackChoice {number:2,name:"Second".into(),tags:NativeTagValues::default(),
                    duration_secs:None,estimated_bytes:None},
            ];
            job.program_subfolders=false;
            job.round4_layout=true;
            job.naming_template="%TITLE%".into();
            job.cue_input_paths=vec![cue.clone(),first.clone(),second.clone()];
            job.options.between_gap=mode;
            job.options.verify_after=true;
            job.cue_segments=all.clone();
            configure_cue_gap_segments(&all,&mut job.cue_segments,&job.options).unwrap();
            let report=super::super::execute(job,CancellationToken::new(),|_|{})
                .await.unwrap_or_else(|err|panic!("{label}: {err}"));
            assert_eq!(report.published.len(),2,"{label} (no track 00 at second FILE)");
            let one=decode_pcm(&report.published[0]);
            let two=decode_pcm(&report.published[1]);
            let gap=&b[..96_000];
            let content=&b[96_000..];
            let (expected_one,expected_two) = match mode {
                NativeCueBetweenGap::AppendPrevious => ([a.as_slice(),gap].concat(),content.to_vec()),
                NativeCueBetweenGap::PrependNext => (a.clone(),b.clone()),
                NativeCueBetweenGap::Drop => (a.clone(),content.to_vec()),
            };
            assert_eq!(one,expected_one,"{label}: first track");
            assert_eq!(two,expected_two,"{label}: second track");
            if mode!=NativeCueBetweenGap::Drop {
                assert_eq!([one,two].concat(),[a.as_slice(),b.as_slice()].concat(),
                    "{label}: bit-exact reconstruction of both source images");
            }
        }
    }

    #[tokio::test]
    async fn real_cue_extraction_copies_all_packets_for_single_track_and_three_cuts() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("Skipping real lossy CUE extraction fixture: ffmpeg unavailable");
            return;
        }
        let root=tempfile::tempdir().unwrap();
        for (extension,encoder,muxer) in [
            ("mp3","libmp3lame","mp3"), ("m4a","aac","ipod"),
            ("aac","aac","adts"), ("ogg","libvorbis","ogg"),
            ("opus","libopus","opus"),
        ] {
            let input=root.path().join(format!("source.{extension}"));
            let fixture=std::process::Command::new("ffmpeg")
                .args(["-hide_banner","-loglevel","error","-nostdin","-y",
                    "-f","lavfi","-i","sine=frequency=440:sample_rate=48000:duration=4.5",
                    "-ac","2","-c:a",encoder,"-b:a","128k"])
                .arg(&input).output().unwrap();
            assert!(fixture.status.success(),"{extension} fixture: {}",
                String::from_utf8_lossy(&fixture.stderr));
            let cue=root.path().join(format!("album.{extension}.cue"));
            fs::write(&cue,format!("FILE \"source.{extension}\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n")).unwrap();
            let (samples,rate)=crate::tui::accuraterip::probe_sample_count(&input).unwrap();
            assert!(samples>2*rate as u64);
            let image_meta=fs::metadata(&input).unwrap();
            let cue_meta=fs::metadata(&cue).unwrap();
            let segment=NativeCueSegment {
                presentation:PresentationId::CueAlbum(1),number:1,
                cue_path:cue.clone(),image:input.clone(),
                start_sample:0,end_sample:samples,index00_sample:None,index01_sample:0,pre_emphasis:false,
                hidden_track_published:false,appended_gap:None,total_samples:samples,sample_rate:rate,
                extension:extension.into(),encoder:encoder.into(),muxer:muxer.into(),
                sample_bits:None,channels:2,source_length:image_meta.len(),
                source_modified:image_meta.modified().ok(),cue_length:cue_meta.len(),
                cue_modified:cue_meta.modified().ok(),source_bit_rate:None,
                packet_cut_samples:vec![],packet_part_index:0,
            };
            let mut job=super::super::tests::fixture_job(
                &root.path().join(format!("output-{extension}")),root.path());
            job.format=DiscFormat::CueImage;
            job.source=cue.clone();
            job.programs[0].id=segment.presentation;
            job.programs[0].is_lossless=false;
            job.programs[0].source_rate=Some(rate);
            job.programs[0].source_depth=None;
            job.programs[0].source_codec=encoder.into();
            job.programs[0].tracks[0].name="Single".into();
            job.cue_segments=vec![segment.clone()];
            job.cue_input_paths=vec![cue.clone(),input.clone()];
            job.program_subfolders=false;
            job.round4_layout=true;
            job.naming_template="%TITLE%".into();
            // MP3 also exercises the packet-extraction-time verify receipt.
            job.options.verify_after=extension=="mp3";
            let report=super::super::execute(job.clone(),CancellationToken::new(),|_|{})
                .await.unwrap_or_else(|error|panic!("{extension}: {error}"));
            assert_eq!(report.published.len(),1,"{extension}");
            assert_eq!(report.published[0].extension().and_then(|ext|ext.to_str()),Some(extension));
            verify_packet_partition(&input,&report.published).unwrap();
            // Three explicit boundaries still use the original single-pass
            // segment muxer. Exercise the actual worker for this MP3 case.
            if extension=="mp3" {
                let mut four=job;
                four.destination=root.path().join("four-tracks");
                let cuts=vec![rate as u64,2*rate as u64,3*rate as u64];
                let mut start=0;
                four.programs[0].tracks.clear();
                four.cue_segments.clear();
                for (index,end) in cuts.iter().copied().chain([samples]).enumerate() {
                    let number=(index+1) as u32;
                    four.programs[0].tracks.push(NativeTrackChoice {
                        number,name:format!("Part {number}"),tags:NativeTagValues::default(),
                        duration_secs:None,estimated_bytes:None,
                    });
                    four.cue_segments.push(NativeCueSegment {
                        number,start_sample:start,end_sample:end,
                        packet_cut_samples:cuts.clone(),packet_part_index:index,..segment.clone()
                    });
                    start=end;
                }
                let report=super::super::execute(four,CancellationToken::new(),|_|{})
                    .await.unwrap();
                assert_eq!(report.published.len(),4);
                verify_packet_partition(&input,&report.published).unwrap();
            }
        }
    }

}

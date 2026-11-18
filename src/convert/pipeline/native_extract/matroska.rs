//! Native MKV/MKA extraction (#91 Phase B).
//!
//! Copy mode demuxes one selected audio stream into its supported native
//! carrier with `-c:a copy`: no decoder or encoder sees the audio packets.
//! Chapter mode decodes the stream once to source-depth (or lossy 24-bit) PCM,
//! then copies
//! sample-aligned ranges to standalone WAV/RF64 files. Each completed file is
//! published through the existing native file-task journal and atomic writer.
//! No material, manifest or output is written beside the source.

use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::convert::chapter_structure::{normalize_embedded_chapters, read_embedded_chapters};
use crate::disc::matroska_utils::{map_matroska_file, map_orphan_transport_file};
use crate::convert::chapter_structure::EmbeddedChapterTrack;

fn command(args: Vec<String>) -> ToolCommand {
    ToolCommand {
        binary: ToolBinary::Ffmpeg,
        args,
        secret_args: Vec::new(),
        cwd: None,
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        env: Vec::new(),
        timeout: Duration::from_secs(6 * 60 * 60),
    }
}

fn utf8_path(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned)
        .ok_or_else(|| format!("FFmpeg cannot address non-UTF8 path: {}", path.display()))
}

fn source_index(program: &NativeProgramChoice) -> Result<u32, String> {
    match program.id {
        PresentationId::MatroskaStream { stream_index, .. } |
        PresentationId::TransportStream { stream_index, .. } => Ok(stream_index),
        _ => Err("The selected program is not a file-backed audio stream".into()),
    }
}

fn copied_stream_tags(job: &NativeExtractionJob, program: &NativeProgramChoice) -> NativeTagValues {
    let authored = program.authored_tags.as_ref().map(|tags| tags.album.clone())
        .unwrap_or_default();
    match job.tag_source {
        NativeTagSource::AuthoredTrack => authored.prioritized(&program.tags),
        // No implicit external tag-file discovery occurs on the file-backed
        // source. A MusicBrainz choice is available only after the user saves
        // values through the existing Tags flow.
        NativeTagSource::DiscAndSidecar | NativeTagSource::MusicBrainz =>
            program.tags.prioritized(&authored),
    }
}

fn chapter_tags(job: &NativeExtractionJob, program: &NativeProgramChoice,
    chapter: &NativeTrackChoice) -> NativeTagValues {
    let authored = program.authored_tags.as_ref().map(|tags| {
        tags.tracks.get(&chapter.number).cloned().unwrap_or_default()
            .prioritized(&tags.album)
    }).unwrap_or_default();
    match job.tag_source {
        NativeTagSource::AuthoredTrack => authored.prioritized(&chapter.tags).prioritized(&program.tags),
        NativeTagSource::DiscAndSidecar | NativeTagSource::MusicBrainz =>
            chapter.tags.prioritized(&program.tags).prioritized(&authored),
    }
}

/// Override FFmpeg's implicit container/stream-tag propagation: a disabled
/// Tags control must not silently copy metadata into a new audio file.
fn push_metadata_args(args: &mut Vec<String>, tags: &NativeTagValues, mask: u16) {
    let entries = [
        (0, "title", tags.title.as_ref()),
        (1, "artist", tags.artist.as_ref()),
        (2, "album", tags.album.as_ref()),
        (3, "genre", tags.genre.as_ref()),
        (4, "date", tags.year.as_ref()),
        (5, "composer", tags.composer.as_ref()),
        (6, "performer", tags.performer.as_ref()),
        (7, "catalog", tags.catalog.as_ref()),
        (8, "publisher", tags.publisher.as_ref()),
    ];
    for (bit, key, value) in entries {
        if mask & (1 << bit) != 0 {
            if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                args.extend(["-metadata".into(), format!("{key}={value}")]);
            }
        }
    }
}

/// Matroska ALAC can omit packet durations while retaining a per-track
/// DURATION tag. A plain remux then advertises only the final packet's PTS,
/// not its end. This tag describes the absolute stream presentation endpoint,
/// not a duration relative to the first packet. FFmpeg's setts filter repairs
/// the final packet without decoding audio; ordinary packets still end at the
/// next PTS. No container-wide endpoint is guessed when the tag is absent.
fn alac_duration_bsf(source: &Path, stream_index: u32) -> Option<String> {
    let input = ffmpeg_next::format::input(source).ok()?;
    let stream = input.streams().find(|s| s.index() == stream_index as usize)?;
    let metadata = stream.metadata();
    let duration = metadata.get("DURATION")?;
    let mut parts = duration.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    if parts.next().is_some() || minutes >= 60 || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    let total = hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds;
    let tb = stream.time_base();
    if tb.numerator() <= 0 || tb.denominator() <= 0 { return None; }
    let ticks = (total * f64::from(tb.denominator()) /
        f64::from(tb.numerator())).round();
    if !ticks.is_finite() || ticks < 1.0 || ticks > i64::MAX as f64 { return None; }
    // Escaped commas are required by FFmpeg's filter argument parser.
    Some(format!("setts=duration=if(eq(NEXT_PTS\\,NOPTS)\\,{}-PTS\\,NEXT_PTS-PTS)",
        ticks as i64))
}

fn copy_command(job: &NativeExtractionJob, program: &NativeProgramChoice,
    target: &Path) -> Result<ToolCommand, String> {
    let stream = source_index(program)?;
    let carrier = file_stream_copy_format(job.format, &program.source_codec);
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-xerror",
        "-y"].into_iter().map(str::to_owned).collect();
    // ALAC's final-packet repair uses the source's absolute presentation end.
    // Without -copyts, FFmpeg can shift an audio-only stream's leading delay
    // to zero while retaining its old DURATION endpoint.
    if program.source_codec.as_str() == "alac" {
        args.push("-copyts".into());
    }
    args.push("-i".into());
    args.push(utf8_path(&job.source)?);
    // MPEG transport LPCM is framed differently from ordinary WAV PCM.
    // FFmpeg has no packet-copy WAV muxer for it; decode losslessly at the
    // probed source precision. Never silently downshift 24-bit source to 16.
    let audio_codec = if job.format == DiscFormat::TransportFile &&
        native_pcm_unpack_required(&program.source_codec) {
        let depth = program.source_depth.ok_or_else(||
            format!("{}: source LPCM precision is unknown", program.label))?;
        match depth {
            1..=16 => "pcm_s16le",
            17..=24 => "pcm_s24le",
            25..=32 => "pcm_s32le",
            _ => return Err(format!("Unsupported transport LPCM precision: {depth} bits")),
        }
    } else { "copy" };
    args.extend(["-map".into(), format!("0:{stream}"), "-vn".into(), "-sn".into(),
        "-dn".into(), "-c:a".into(), audio_codec.into(),
        "-map_metadata".into(), "-1".into(),
        "-map_metadata:s:a:0".into(), "-1".into(),
        // A loose transport file cannot supply authored chapter navigation.
        "-map_chapters".into(),
        if job.format == DiscFormat::Matroska && carrier.chapters { "0" } else { "-1" }.into()]);
    // MP3 Xing generation from Matroska packets can erroneously skip an
    // entire decoder frame on playback; a plain MP3 packet copy preserves
    // the original decoded samples, although encoder padding may remain.
    if carrier.muxer == "mp3" {
        args.extend(["-write_xing".into(), "0".into()]);
    }
    if program.source_codec.as_str() == "alac" {
        if let Some(filter) = alac_duration_bsf(&job.source, stream) {
            args.extend(["-bsf:a".into(), filter]);
        }
    }
    if job.transfer_tags && carrier.text_tags {
        push_metadata_args(&mut args, &copied_stream_tags(job, program), job.tag_fields);
        // FFmpeg -attach is a Matroska-specific attachment mechanism.
        // FLAC/MP3 pictures are written to completed private files by the
        // existing native metadata writer instead.
        if job.tag_artwork && carrier.muxer == "matroska" && carrier.artwork {
            if let Some(art) = native_artwork_path(&AlbumMetadata::default(), &job.source) {
                let mime = art.extension().and_then(|ext| ext.to_str()).and_then(|ext| {
                    if ext.eq_ignore_ascii_case("png") { Some("image/png") }
                    else if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
                        Some("image/jpeg")
                    } else { None }
                });
                if let Some(mime) = mime {
                    args.extend(["-attach".into(), utf8_path(&art)?,
                        "-metadata:s:t:0".into(), format!("mimetype={mime}")]);
                }
            }
        }
    }
    // FFmpeg's WAV muxer defaults to -rf64 never. Allow it to reserve ds64
    // space and finalize a true RF64 header if this seekable private copy
    // exceeds the 32-bit RIFF size limit, without changing the PCM packets.
    if carrier.muxer == "wav" {
        args.extend(["-rf64".into(), "auto".into()]);
    }
    args.extend(["-f".into(), carrier.muxer.into(), utf8_path(target)?]);
    Ok(command(args))
}

#[derive(Debug, Clone)]
struct ChapterPcm {
    codec: &'static str,
    raw_format: &'static str,
    bits: u16,
    valid_bits: u16,
    sample_bytes: u16,
    floating: bool,
    /// The embedded WAV INFO comment survives when extraction.log is off.
    lossy_codec: Option<String>,
}

fn chapter_pcm(presentation: &crate::disc::DiscPresentation) -> Result<ChapterPcm, String> {
    let format = &presentation.format;
    let codec = format.codec.as_deref().unwrap_or("unknown");
    let floating = matches!(codec, "pcm_f32le" | "pcm_f32be" |
        "pcm_f64le" | "pcm_f64be");
    let bits = if !format.lossless {24} else if floating {
        if codec.starts_with("pcm_f64") {64} else {32}
    } else {format.bit_depth.unwrap_or(32)};
    let (codec, raw_format, storage, floating) = match (bits, floating) {
        (8, false) => ("pcm_u8", "u8", 8, false),
        (9..=16, false) => ("pcm_s16le", "s16le", 16, false),
        (17..=24, false) => ("pcm_s24le", "s24le", 24, false),
        (25..=32, false) => ("pcm_s32le", "s32le", 32, false),
        (33..=64, false) => ("pcm_s64le", "s64le", 64, false),
        (32, true) => ("pcm_f32le", "f32le", 32, true),
        (64, true) => ("pcm_f64le", "f64le", 64, true),
        _ => return Err(format!("Unsupported lossless Matroska PCM depth {bits}")),
    };
    Ok(ChapterPcm { codec, raw_format, bits: storage, valid_bits: bits as u16,
        sample_bytes: (storage / 8) as u16, floating,
        lossy_codec: (!format.lossless).then(|| format.codec.clone()
            .unwrap_or_else(|| "unknown".into())),
    })
}

fn decode_command(source: &Path, stream: u32, raw: &Path,
    spec: &ChapterPcm) -> Result<ToolCommand, String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-xerror",
        "-y", "-i"].into_iter().map(str::to_owned).collect();
    args.push(utf8_path(source)?);
    args.extend(["-map".into(), format!("0:{stream}"), "-vn".into(), "-sn".into(),
        "-dn".into(), "-map_metadata".into(), "-1".into(),
        "-c:a".into(), spec.codec.into(), "-f".into(), spec.raw_format.into(),
        utf8_path(raw)?]);
    Ok(command(args))
}

/// Opus packet timestamps include decoder pre-skip, which can put the
/// packet start before the first retained PCM sample even when it is positive.
/// Read a bounded set of packets to find the first retained decoded frame.
/// A short Opus packet can be entirely consumed by pre-skip and yield no PCM.
/// Use FFmpeg's own presentation clock for the first retained sample;
/// chapter splitting already requires FFmpeg to decode the selected stream.
fn opus_decoded_start(source: &Path, stream_index: u32) -> Result<i64, String> {
    let output = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", &stream_index.to_string(),
            "-show_frames", "-read_intervals", "%+#8",
            "-show_entries", "frame=best_effort_timestamp",
            "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(source)
        .output().map_err(|e| format!("Probe first decoded Opus frame: {e}"))?;
    if !output.status.success() {
        return Err(format!("Probe first decoded Opus frame failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|e| format!("Read first decoded Opus timestamp: {e}"))?;
    text.lines().map(str::trim).find(|line| !line.is_empty())
        .ok_or("The Opus stream has no decoded audio frame")?
        .parse::<i64>().map_err(|e| format!("Invalid decoded Opus timestamp: {e}"))
}

/// Raw FFmpeg PCM begins with the stream's first decoded sample, not with
/// silence spanning the MKV's earlier video timeline. Chapter markers use the
/// container timeline, so subtract this origin before addressing raw frames.
fn stream_origin_samples(source: &Path, stream_index: u32, rate: u32) -> Result<i128, String> {
    let input = ffmpeg_next::format::input(source)
        .map_err(|e| format!("Read Matroska audio start time: {e}"))?;
    let stream = input.streams().find(|stream| stream.index() == stream_index as usize)
        .ok_or("Selected Matroska audio stream is missing")?;
    if stream.parameters().medium() != ffmpeg_next::media::Type::Audio {
        return Err("Selected Matroska stream is no longer audio".into());
    }
    // For Opus, FFmpeg has already discarded pre-skip samples when writing
    // raw PCM. The first decoded frame's timestamp, not the encoded packet
    // start, applies to both negative pre-roll and positively delayed Opus.
    let start = if stream.parameters().id() == ffmpeg_next::codec::Id::OPUS {
        opus_decoded_start(source, stream_index)?
    } else {
        stream.start_time()
    };
    if start == i64::MIN { return Ok(0); } // AV_NOPTS_VALUE: preserve untimed-stream behavior
    let time_base = stream.time_base();
    let num = time_base.numerator();
    let den = time_base.denominator();
    if num <= 0 || den <= 0 || rate == 0 {
        return Err("Invalid Matroska audio timestamp clock".into());
    }
    let numerator = i128::from(start)
        .checked_mul(i128::from(num))
        .and_then(|v| v.checked_mul(i128::from(rate)))
        .ok_or("Matroska audio start timestamp overflow")?;
    let denominator = i128::from(den);
    // Nearest-frame rounding is the same as chapter normalization; it also
    // preserves a negative preroll origin instead of treating it as zero.
    let rounded = if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    };
    Ok(rounded)
}

/// Preserve only verified speaker assignments. A 5.1 count alone cannot tell
/// a side-surround arrangement from a rear-surround arrangement.
fn chapter_channel_mask(layout: Option<&str>, channels: u16) -> u32 {
    let source_mask = layout.and_then(|label| {
        verified_layout_mask(label, channels).or_else(|| {
            let digits = label.strip_prefix("0x")?;
            let mask = u32::from_str_radix(digits, 16).ok()?;
            (mask != 0 && mask.count_ones() == u32::from(channels)).then_some(mask)
        })
    });
    source_mask.unwrap_or(match channels { 1 => 4, 2 => 3, _ => 0 })
}

fn checked_bytes(samples: u64, channels: u16, sample_bytes: u16) -> Result<u64, String> {
    samples.checked_mul(u64::from(channels))
        .and_then(|bytes| bytes.checked_mul(u64::from(sample_bytes)))
        .ok_or_else(|| "Decoded Matroska sample offset exceeds addressable storage".into())
}

/// Standard WAVEFORMATEXTENSIBLE PCM header, preserving valid source bits,
/// speaker positions, and RF64 sizes. Float PCM uses the IEEE_FLOAT GUID.
/// Lossy sources receive a durable ICMT provenance label in the WAV itself.
fn write_pcm_header(writer: &mut impl Write, rate: u32, channels: u16,
    mask: u32, samples: u64, spec: &ChapterPcm) -> Result<(), String> {
    let length = checked_bytes(samples, channels, spec.sample_bytes)?;
    let align = channels.checked_mul(spec.sample_bytes).ok_or("PCM WAV alignment overflow")?;
    let byte_rate = rate.checked_mul(u32::from(align)).ok_or("PCM WAV byte-rate overflow")?;
    let note = spec.lossy_codec.as_ref().map(|codec| {
        let text = format!("Decoded from lossy {codec} source; 24-bit PCM");
        let mut bytes = text.into_bytes();
        bytes.push(0);
        if bytes.len() % 2 != 0 {bytes.push(0);}
        bytes
    });
    // The fact chunk is kept for compatibility with existing float-WAV files.
    // Its sample count is required for non-PCM; optional for integer PCM.
    let extra = note.as_ref().map_or(0u64, |bytes| 20 + bytes.len() as u64);
    let header_size = 80u64.checked_add(extra).ok_or("WAV header length overflow")?;
    let padded_length = length.checked_add(length % 2)
        .ok_or("WAV data padding overflow")?;
    let riff = padded_length.checked_add(header_size - 8)
        .is_some_and(|size| size <= u64::from(u32::MAX));
    if riff {
        writer.write_all(b"RIFF").map_err(|e|e.to_string())?;
        writer.write_all(&((padded_length + header_size - 8) as u32).to_le_bytes())
            .map_err(|e|e.to_string())?;
        writer.write_all(b"WAVE").map_err(|e|e.to_string())?;
    } else {
        writer.write_all(b"RF64\xff\xff\xff\xffWAVEds64")
            .map_err(|e|e.to_string())?;
        writer.write_all(&28u32.to_le_bytes()).map_err(|e|e.to_string())?;
        writer.write_all(&padded_length.checked_add(header_size + 28)
            .ok_or("RF64 length overflow")?.to_le_bytes())
            .map_err(|e|e.to_string())?;
        writer.write_all(&length.to_le_bytes()).map_err(|e|e.to_string())?;
        writer.write_all(&samples.to_le_bytes()).map_err(|e|e.to_string())?;
        writer.write_all(&0u32.to_le_bytes()).map_err(|e|e.to_string())?;
    }
    writer.write_all(b"fmt ").map_err(|e|e.to_string())?;
    writer.write_all(&40u32.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&0xfffeu16.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&channels.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&rate.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&byte_rate.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&align.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&spec.bits.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&22u16.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&spec.valid_bits.to_le_bytes()).map_err(|e|e.to_string())?;
    writer.write_all(&mask.to_le_bytes()).map_err(|e|e.to_string())?;
    const PCM_GUID: [u8; 16] = [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
    const FLOAT_GUID: [u8; 16] = [3, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
    writer.write_all(if spec.floating {&FLOAT_GUID} else {&PCM_GUID})
        .map_err(|e|e.to_string())?;
    writer.write_all(b"fact\x04\0\0\0").map_err(|e|e.to_string())?;
    writer.write_all(&u32::try_from(samples).unwrap_or(u32::MAX).to_le_bytes())
        .map_err(|e|e.to_string())?;
    if let Some(bytes) = note {
        writer.write_all(b"LIST").map_err(|e|e.to_string())?;
        writer.write_all(&(12u32 + bytes.len() as u32).to_le_bytes())
            .map_err(|e|e.to_string())?;
        writer.write_all(b"INFOICMT").map_err(|e|e.to_string())?;
        writer.write_all(&(bytes.len() as u32).to_le_bytes()).map_err(|e|e.to_string())?;
        writer.write_all(&bytes).map_err(|e|e.to_string())?;
    }
    writer.write_all(b"data").map_err(|e|e.to_string())?;
    writer.write_all(&(if riff {length as u32} else {u32::MAX}).to_le_bytes())
        .map_err(|e|e.to_string())?;
    Ok(())
}

fn write_pcm_chapter(raw: &Path, output: &Path, rate: u32, channels: u16,
    mask: u32, start: u64, samples: u64, spec: &ChapterPcm,
    job_cancel: &CancellationToken, item_cancel: &CancellationToken,
    controls: &NativeExtractionControl,
) -> Result<(), String> {
    if channels == 0 || rate == 0 || samples == 0 {
        return Err("Invalid rate, channels or sample count for Matroska chapter WAV".into());
    }
    let offset = checked_bytes(start, channels, spec.sample_bytes)?;
    let length = checked_bytes(samples, channels, spec.sample_bytes)?;
    let mut input = File::open(raw).map_err(|e| format!("Open decoded audio: {e}"))?;
    let size = input.metadata().map_err(|e|e.to_string())?.len();
    if offset.checked_add(length).is_none_or(|end|end > size) {
        return Err("Chapter's sample range exceeds the decoded stream".into());
    }
    input.seek(SeekFrom::Start(offset)).map_err(|e|format!("Seek decoded chapter: {e}"))?;
    let mut output = OpenOptions::new().create_new(true).write(true).open(output)
        .map_err(|e|format!("Create private chapter WAV: {e}"))?;
    write_pcm_header(&mut output, rate, channels, mask, samples, spec)?;
    let mut remaining = length;
    let mut buf = [0u8; 128 * 1024];
    while remaining > 0 {
        if controls.wait_blocking(job_cancel, item_cancel) {
            return Err("Matroska chapter extraction stopped".into());
        }
        let n = usize::try_from(remaining.min(buf.len() as u64)).unwrap();
        input.read_exact(&mut buf[..n]).map_err(|e|format!("Read decoded chapter: {e}"))?;
        output.write_all(&buf[..n]).map_err(|e|format!("Write chapter WAV: {e}"))?;
        remaining -= n as u64;
    }
    if length % 2 != 0 {
        output.write_all(&[0]).map_err(|e| format!("Pad odd WAV data chunk: {e}"))?;
    }
    output.sync_all().map_err(|e|format!("Synchronize chapter WAV: {e}"))?;
    Ok(())
}

async fn inspected_source(job: &NativeExtractionJob, cancel: &CancellationToken)
    -> Result<(crate::disc::DiscContents, Vec<crate::convert::chapter_structure::RawEmbeddedChapter>), String> {
    if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
    let path = job.source.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mapped = if crate::disc::matroska_utils::is_matroska_path(&path) {
            map_matroska_file(&path)?
        } else { map_orphan_transport_file(&path)? };
        let chapters = if mapped.format == DiscFormat::Matroska {
            read_embedded_chapters(&path)?
        } else { Vec::new() };
        Ok::<_, String>((mapped, chapters))
    }).await.map_err(|e| format!("Matroska probe task: {e}"))??;
    if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
    Ok(result)
}

pub(super) async fn probe_selected_sources(job: &NativeExtractionJob,
    cancel: &CancellationToken) -> Result<NativeProbeFacts, String> {
    let (mapped, _) = inspected_source(job, cancel).await?;
    let program = job.programs.first().ok_or("No selected Matroska stream")?;
    let presentation = mapped.presentations.iter().find(|p| p.id == program.id)
        .ok_or("Matroska stream or chapter inventory changed since selection")?;
    Ok(NativeProbeFacts { bit_depth: presentation.format.bit_depth,
        sample_rate: presentation.format.sample_rate, channels: presentation.format.channels })
}

fn selected_ranges(chapters: &[EmbeddedChapterTrack], requested: &[NativeTrackChoice],
    sample_frames: u64, audio_origin: i128) -> Result<Vec<(u32, u64, u64)>, String> {
    requested.iter().map(|track| {
        let chapter = chapters.get(track.number.checked_sub(1).ok_or("Chapter zero")? as usize)
            .filter(|chapter| chapter.ordinal == track.number)
            .ok_or_else(|| format!("Chapter {} no longer exists", track.number))?;
        let nominal_start = chapter.boundary.start_sample;
        let nominal_end = nominal_start.checked_add(chapter.boundary.samples)
            .ok_or("Chapter sample range overflow")?;
        // Chapters are authored in file presentation time, but the raw PCM
        // starts at the selected audio stream's own origin. Clamp to the
        // decoded extent without adding missing silence or extending an
        // explicitly bounded final chapter to audio EOF.
        let extent = i128::from(sample_frames);
        let start = (i128::from(nominal_start) - audio_origin).clamp(0, extent) as u64;
        let end = (i128::from(nominal_end) - audio_origin).clamp(0, extent) as u64;
        // No overlap is a valid selection: for example, video chapter 1
        // can precede the first audio packet. Report zero frames so the
        // caller can skip only that chapter and publish the others.
        Ok((track.number, start, end - start))
    }).collect()
}

/// CUE indexes are authored chapter starts expressed on the native file's
/// audio clock. A delayed stream does not gain synthetic silence when copied
/// to an elementary file. As with ordinary CUE, indexes round to 1/75 s.
fn native_stream_cue(job: &NativeExtractionJob, program: &NativeProgramChoice,
    chapters: &[EmbeddedChapterTrack], rate: u32, source_index: u32,
    audio_target: &Path) -> Result<String, String> {
    if rate == 0 { return Err("CUE needs a known audio sample rate".into()); }
    let origin = stream_origin_samples(&job.source, source_index, rate)?;
    let format = matroska_copy_format(&program.source_codec);
    let cue_type = if format.extension == "mp3" { "MP3" } else { "WAVE" };
    let filename = audio_target.file_name().ok_or("CUE audio has no filename")?
        .to_string_lossy().replace('"', "'");
    let mut rows = vec![format!("FILE \"{filename}\" {cue_type}"),
        "REM Indexes rounded to nearest 1/75 s; encoded audio unchanged".into()];
    let mut count = 0usize;
    for chapter in chapters {
        let end = i128::from(chapter.boundary.start_sample)
            .checked_add(i128::from(chapter.boundary.samples))
            .ok_or("CUE chapter sample overflow")?;
        if end <= origin { continue; } // no audio in this authored chapter
        count += 1;
        if count > 99 { return Err("CUE supports at most 99 audible chapters".into()); }
        let start = (i128::from(chapter.boundary.start_sample) - origin).max(0) as u128;
        let frame = start.checked_mul(75).ok_or("CUE index overflow")?
            .checked_add(u128::from(rate/2)).ok_or("CUE index overflow")? / u128::from(rate);
        let mins = frame / 4500;
        let seconds = (frame / 75) % 60;
        let frames = frame % 75;
        let title = chapter.title.as_deref().unwrap_or("Untitled")
            .replace('"', "'").replace('\r', " ").replace('\n', " ");
        rows.push(format!("  TRACK {count:02} AUDIO"));
        rows.push(format!("    TITLE \"{title}\""));
        rows.push(format!("    INDEX 01 {mins:02}:{seconds:02}:{frames:02}"));
    }
    if count == 0 {return Err("Selected Matroska chapters contain no audio for CUE".into());}
    Ok(rows.join("\n") + "\n")
}

fn check_source_unchanged(path: &Path, length: u64,
    modified: Option<std::time::SystemTime>) -> Result<(), String> {
    let current = fs::metadata(path)
        .map_err(|e| format!("Recheck Matroska source: {e}"))?;
    if current.len() != length || current.modified().ok() != modified {
        return Err("Matroska input changed during extraction; stop before publishing further files".into());
    }
    Ok(())
}

pub(super) async fn execute_inner(
    job: NativeExtractionJob,
    cancel: CancellationToken,
    controls: Arc<NativeExtractionControl>,
    mut on_progress: impl FnMut(NativeExtractionProgress),
) -> Result<NativeExtractionReport, String> {
    validate_job(&job)?;
    controls.execution_started.store(true, Ordering::Release);
    let work = native_working_storage(&job.working_root, &job.destination)?;
    fs::create_dir_all(&work).map_err(|e| format!("File stream working storage: {e}"))?;
    let temp = tempfile::Builder::new().prefix("tonepoet-stream-").tempdir_in(&work)
        .map_err(|e| format!("Private Matroska storage: {e}"))?;
    let runner = RealToolRunner::new(HashMap::new()).with_native_pause(controls.tool_pause());
    let initial_metadata = fs::metadata(&job.source)
        .map_err(|e| format!("Inspect Matroska input: {e}"))?;
    let initial_modified = initial_metadata.modified().ok();
    let initial_length = initial_metadata.len();
    let (mapped, raw_chapters) = inspected_source(&job, &cancel).await?;
    check_source_unchanged(&job.source, initial_length, initial_modified)?;
    let total = job.programs.iter().map(|p| p.tracks.len()).sum();
    let estimated_bytes_total = job.programs.iter().flat_map(|p| &p.tracks)
        .try_fold(0u64, |sum, track| sum.checked_add(track.estimated_bytes?));
    let mut done = 0;
    let mut skipped = 0;
    let mut published = Vec::new();
    let mut bytes_done = 0u64;
    let mut bytes_skipped = 0u64;
    let mut bytes_published = 0u64;
    let mut emit = |current: String, current_file: Option<PathBuf>, estimate: Option<u64>,
        done: usize, skipped: usize, published: &Vec<PathBuf>,
        bytes_done: u64, bytes_skipped: u64, bytes_published: u64| {
        on_progress(NativeExtractionProgress { done, total, published: published.len(),
            published_paths: published.clone(), skipped, current, current_file,
            current_estimated_bytes: estimate, estimated_bytes_done: bytes_done,
            estimated_bytes_skipped: bytes_skipped, estimated_bytes_published: bytes_published,
            estimated_bytes_total });
    };
    emit("Inspecting selected file audio streams".into(), None, None,
        done, skipped, &published, bytes_done, bytes_skipped, bytes_published);

    for (pi, program) in job.programs.iter().enumerate() {
        if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
        let presentation = mapped.presentations.iter().find(|p| p.id == program.id)
            .ok_or_else(|| format!("{}: stream/chapter inventory changed since selection", program.label))?;
        if program.source_rate.is_some_and(|rate| Some(rate) != presentation.format.sample_rate)
            || program.source_channels.is_some_and(|channels| Some(channels) != presentation.format.channels)
            || presentation.format.codec.as_deref().unwrap_or("unknown") != program.source_codec.as_str()
            || (job.format == DiscFormat::TransportFile &&
                native_pcm_unpack_required(&program.source_codec) &&
                presentation.format.bit_depth != program.source_depth) {
            return Err(format!("{}: source audio format changed since selection",program.label));
        }
        let chapter_count = match program.id {
            PresentationId::MatroskaStream { chapter_count, .. } => chapter_count,
            PresentationId::TransportStream { .. } => 0,
            _ => return Err("Non-file-stream program in file-stream job".into()),
        };
        let copy = job.file_per_program || chapter_count == 0;
        let group_target = proposed_destination_for_program(&job, program);
        let expected = program.tracks.iter().filter_map(|track| track.estimated_bytes)
            .fold(0u64, u64::saturating_add);
        if copy {
            let target = if job.file_per_program { group_target.clone() }
                else { proposed_destination(&job, program, &program.tracks[0]) };
            let item = controls.begin_item(&cancel);
            controls.set_attempted_output(target.clone());
            if job.overwrite_policy == NativeOverwritePolicy::Skip && fs::symlink_metadata(&target).is_ok() {
                controls.record_skipped(if job.file_per_program { NativeSkippedSelection::program(program) }
                    else { NativeSkippedSelection::track(program, 1) }
                    .with_reason(NativeSkipReason::ExistingFile))?;
                done += program.tracks.len(); skipped += program.tracks.len();
                bytes_done = bytes_done.saturating_add(expected);
                bytes_skipped = bytes_skipped.saturating_add(expected);
                controls.finish_item(); controls.clear_attempted_output();
                emit(format!("Skipped existing {}", program.label), None, None, done, skipped,
                    &published, bytes_done, bytes_skipped, bytes_published);
                continue;
            }
            if fs::symlink_metadata(&target).is_ok() && job.overwrite_policy == NativeOverwritePolicy::Fail {
                return Err(format!("Destination already exists: {}", target.display()));
            }
            let carrier = file_stream_copy_format(job.format, &program.source_codec);
            let complete = temp.path().join(format!("stream-{pi}.{}", carrier.extension));
            controls.watch_file(complete.clone(), format!("Copying {}", program.label),
                Some(expected), false);
            emit(format!("Copying unchanged {}", program.label), Some(complete.clone()),
                Some(expected), done, skipped, &published, bytes_done, bytes_skipped, bytes_published);
            let result: Result<(), String> = async {
                runner.run(copy_command(&job, program, &complete)?, &item)
                    .await.map_err(|e| format!("Native audio stream extraction: {e}"))?;
                controls.wait_ready(&cancel, &item).await?;
                let output = fs::metadata(&complete).map_err(|e| format!("Copied stream missing: {e}"))?;
                if output.len() < 64 { return Err("Audio stream extraction produced no usable file".into()); }
                // Native FLAC pictures and MP3 ID3 APIC cannot use the
                // Matroska -attach path. Embed only an explicitly selected
                // cover, on the completed private copy before publication.
                // The established writer preserves the compressed audio.
                if job.transfer_tags && job.tag_artwork && carrier.artwork
                    && carrier.muxer != "matroska" {
                    if let Some(artwork) = native_artwork_path(
                        &AlbumMetadata::default(), &job.source) {
                        let private_copy = complete.clone();
                        tokio::task::spawn_blocking(move ||
                            write_completed_native_metadata(&private_copy, &[], Some(&artwork)))
                            .await.map_err(|error| format!("Native artwork task: {error}"))??;
                    }
                }
                File::open(&complete).and_then(|f| f.sync_all())
                    .map_err(|e|format!("Sync copied Matroska stream: {e}"))?;
                check_source_unchanged(&job.source, initial_length, initial_modified)?;
                let cue_file = if job.cue && matroska_cue_for_program(&job, program) {
                    let rate = presentation.format.sample_rate
                        .ok_or("CUE needs a known source sample rate")?;
                    let chapters = normalize_embedded_chapters(&raw_chapters, rate)?;
                    let text = native_stream_cue(&job,program,&chapters,rate,
                        source_index(program)?,&target)?;
                    let staged = temp.path().join(format!("stream-{pi}.cue"));
                    let mut output = OpenOptions::new().create_new(true).write(true).open(&staged)
                        .map_err(|e|format!("Create private Matroska CUE: {e}"))?;
                    output.write_all(text.as_bytes()).and_then(|()|output.sync_all())
                        .map_err(|e|format!("Finish private Matroska CUE: {e}"))?;
                    controls.record_prepared_cue(&target.with_extension("cue"),&text)?;
                    Some(staged)
                } else {None};
                publish_finished_file_controlled(&job,&complete,&target,&cancel,&item,&controls)?;
                // One completed private file per stream: bounded scratch usage.
                let _ = fs::remove_file(&complete);
                published.push(target.clone());
                bytes_published = bytes_published.saturating_add(expected);
                emit(format!("Published {}",program.label),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
                if let Some(staged) = cue_file {
                    controls.wait_ready(&cancel,&item).await?;
                    let cue_target = target.with_extension("cue");
                    controls.set_attempted_output(cue_target.clone());
                    publish_finished_file_controlled(&job,&staged,&cue_target,
                        &cancel,&item,&controls)?;
                    let _ = fs::remove_file(&staged);
                    published.push(cue_target);
                    emit(format!("Published CUE for {}",program.label),None,None,done,skipped,
                        &published,bytes_done,bytes_skipped,bytes_published);
                }
                Ok(())
            }.await;
            controls.clear_watch();
            let requested_skip = controls.finish_item();
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if requested_skip && result.is_err() {
                controls.record_skipped(if job.file_per_program { NativeSkippedSelection::program(program) }
                    else { NativeSkippedSelection::track(program, 1) }
                    .with_reason(NativeSkipReason::RequestedByUser))?;
                done += program.tracks.len(); skipped += program.tracks.len();
                bytes_done = bytes_done.saturating_add(expected);
                bytes_skipped = bytes_skipped.saturating_add(expected);
                controls.clear_attempted_output();
                emit(format!("Skipped {}",program.label),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
                continue;
            }
            result?;
            controls.clear_attempted_output();
            done += program.tracks.len();
            bytes_done = bytes_done.saturating_add(expected);
            emit(format!("Completed {}",program.label),None,None,done,skipped,
                &published,bytes_done,bytes_skipped,bytes_published);
            continue;
        }

        // A chapter split reuses one decoded raw stream, regardless of the
        // number of selected chapter outputs. This is linear in audio length.
        let rate = presentation.format.sample_rate.ok_or_else(||
            format!("{}: unknown sample rate, choose Copy stream",program.label))?;
        let channels = presentation.format.channels.filter(|n| *n != 0)
            .ok_or_else(||format!("{}: unknown channel count, choose Copy stream",program.label))?;
        let chapters = normalize_embedded_chapters(&raw_chapters, rate)?;
        let channel_mask = chapter_channel_mask(presentation.format.channel_layout.as_deref(),
            u16::from(channels));
        if chapters.len() != chapter_count as usize {
            return Err(format!("{}: embedded chapter table changed",program.label));
        }
        // Do not decode if every selected destination already exists and the
        // user explicitly requested Skip.
        let mut pending = Vec::new();
        for track in &program.tracks {
            let target = proposed_destination(&job, program, track);
            if job.overwrite_policy == NativeOverwritePolicy::Skip && fs::symlink_metadata(&target).is_ok() {
                controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                    .with_reason(NativeSkipReason::ExistingFile))?;
                done += 1; skipped += 1;
                bytes_done = bytes_done.saturating_add(track.estimated_bytes.unwrap_or(0));
                bytes_skipped = bytes_skipped.saturating_add(track.estimated_bytes.unwrap_or(0));
                emit(format!("Skipped existing {}",track.name),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
            } else { pending.push((track,target)); }
        }
        if pending.is_empty() { continue; }
        // Resolve the decoded origin only when a chapter actually needs
        // extraction. An idempotent Skip rerun needs neither probe nor decode.
        // The Opus origin may require one external first-frame probe. Keep
        // process I/O off the async file-task executor's worker thread.
        let origin_source = job.source.clone();
        let origin_stream = source_index(program)?;
        let audio_origin = tokio::task::spawn_blocking(move ||
            stream_origin_samples(&origin_source, origin_stream, rate))
            .await.map_err(|e| format!("Matroska audio origin task: {e}"))??;
        let spec = chapter_pcm(presentation)?;
        let raw = temp.path().join(format!("decoded-{pi}.{}", spec.raw_format));
        let decode_item = controls.begin_item(&cancel);
        controls.watch_file(raw.clone(),format!("Decoding {} once",program.label),None,false);
        emit(format!("Decoding {} for selected chapters",program.label),Some(raw.clone()),None,
            done,skipped,&published,bytes_done,bytes_skipped,bytes_published);
        let decoded = runner.run(decode_command(&job.source,source_index(program)?,&raw,&spec)?,&decode_item)
            .await.map_err(|e|format!("Decode Matroska audio to private PCM: {e}"));
        controls.clear_watch();
        let decode_skipped = controls.finish_item();
        if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
        if decode_skipped && decoded.is_err() {
            for (track,_) in &pending {
                controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                    .with_reason(NativeSkipReason::RequestedByUser))?;
                done += 1; skipped += 1;
                bytes_done = bytes_done.saturating_add(track.estimated_bytes.unwrap_or(0));
                bytes_skipped = bytes_skipped.saturating_add(track.estimated_bytes.unwrap_or(0));
            }
            emit(format!("Skipped {}",program.label),None,None,done,skipped,
                &published,bytes_done,bytes_skipped,bytes_published);
            continue;
        }
        decoded?;
        let decoded_bytes = fs::metadata(&raw).map_err(|e|format!("Private decoded stream missing: {e}"))?.len();
        let align = u64::from(channels) * u64::from(spec.sample_bytes);
        if decoded_bytes == 0 || decoded_bytes % align != 0 {
            return Err("Matroska decoded stream contains no complete sample frames".into());
        }
        let ranges: HashMap<u32, (u64, u64)> =
            selected_ranges(&chapters, &program.tracks, decoded_bytes / align, audio_origin)?
                .into_iter().map(|(number, start, frames)| (number, (start, frames)))
                .collect();
        for (track, target) in pending {
            let (start, frames) = *ranges.get(&track.number)
                .ok_or_else(|| format!("Chapter {} has no decoded sample range", track.number))?;
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if frames == 0 {
                // This chapter lies wholly outside the decoded audio. No
                // empty WAV is published, but a durable skip receipt prevents
                // Recovery from repeatedly attempting this same selection.
                controls.record_skipped(NativeSkippedSelection::track(program, track.number)
                    .with_reason(NativeSkipReason::NoAudioInChapter))?;
                done += 1; skipped += 1;
                let estimate = track.estimated_bytes.unwrap_or(0);
                bytes_done = bytes_done.saturating_add(estimate);
                bytes_skipped = bytes_skipped.saturating_add(estimate);
                emit(format!("Skipped {} (no audio)", track.name), None, None, done, skipped,
                    &published, bytes_done, bytes_skipped, bytes_published);
                continue;
            }
            let item = controls.begin_item(&cancel);
            controls.set_attempted_output(target.clone());
            let completed = temp.path().join(format!("chapter-{pi}-{}.wav",track.number));
            controls.watch_file(completed.clone(),format!("Writing {}",track.name),track.estimated_bytes,false);
            emit(format!("Writing {}",track.name),Some(completed.clone()),track.estimated_bytes,
                done,skipped,&published,bytes_done,bytes_skipped,bytes_published);
            let result: Result<(),String> = async {
                let reader = raw.clone(); let writer = completed.clone();
                let cancel_copy = cancel.clone(); let item_copy = item.clone();
                let controls_copy = Arc::clone(&controls);
                let chapter_spec = spec.clone();
                tokio::task::spawn_blocking(move || write_pcm_chapter(&reader,&writer,
                    rate,u16::from(channels),channel_mask,start,frames,&chapter_spec,
                    &cancel_copy,&item_copy,&controls_copy))
                    .await.map_err(|e|format!("Matroska chapter writer task: {e}"))??;
                controls.wait_ready(&cancel,&item).await?;
                if job.transfer_tags {
                    let values = chapter_tags(&job,program,track);
                    let entries = selected_tag_items(&values,job.tag_fields);
                    let artwork = if job.tag_artwork {
                        native_artwork_path(&AlbumMetadata::default(),&job.source)
                    } else {None};
                    if !entries.is_empty() || artwork.is_some() {
                        let completed_copy = completed.clone();
                        tokio::task::spawn_blocking(move ||
                            write_completed_native_metadata(&completed_copy,&entries,artwork.as_deref()))
                            .await.map_err(|e|format!("Matroska chapter metadata task: {e}"))??;
                    }
                }
                controls.wait_ready(&cancel,&item).await?;
                check_source_unchanged(&job.source, initial_length, initial_modified)?;
                publish_finished_file_controlled(&job,&completed,&target,&cancel,&item,&controls)?;
                // Bound scratch use to one decoded stream and one chapter,
                // not the cumulative size of an entire audiobook/album.
                let _ = fs::remove_file(&completed);
                published.push(target);
                bytes_published = bytes_published.saturating_add(track.estimated_bytes.unwrap_or(0));
                emit(format!("Published {}",track.name),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
                Ok(())
            }.await;
            controls.clear_watch();
            let user_skipped = controls.finish_item();
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            if user_skipped && result.is_err() {
                controls.record_skipped(NativeSkippedSelection::track(program,track.number)
                    .with_reason(NativeSkipReason::RequestedByUser))?;
                done += 1; skipped += 1;
                bytes_done = bytes_done.saturating_add(track.estimated_bytes.unwrap_or(0));
                bytes_skipped = bytes_skipped.saturating_add(track.estimated_bytes.unwrap_or(0));
                controls.clear_attempted_output();
                emit(format!("Skipped {}",track.name),None,None,done,skipped,
                    &published,bytes_done,bytes_skipped,bytes_published);
                continue;
            }
            result?;
            controls.clear_attempted_output();
            done += 1;
            bytes_done = bytes_done.saturating_add(track.estimated_bytes.unwrap_or(0));
            emit(format!("Completed {}",track.name),None,None,done,skipped,
                &published,bytes_done,bytes_skipped,bytes_published);
        }
        fs::remove_file(&raw).map_err(|e|format!("Remove private decoded Matroska audio: {e}"))?;
        check_source_unchanged(&job.source, initial_length, initial_modified)?;
    }
    if job.extraction_log {
        for (index,target) in native_log_targets(&job).into_iter().enumerate() {
            let text = native_log_content_for_index(&job,index,&published,
                &controls.skipped_receipts(),&[],NativeLogOutcome::Complete,None);
            publish_native_recovery_sidecar(&job,&target,&text,&cancel,&controls)?;
            published.push(target);
            emit("Published extraction log".into(),None,None,done,skipped,
                &published,bytes_done,bytes_skipped,bytes_published);
        }
    }
    Ok(NativeExtractionReport { published, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn float64_spec() -> ChapterPcm {
        ChapterPcm {codec:"pcm_f64le", raw_format:"f64le", bits:64,
            valid_bits:64, sample_bytes:8, floating:true, lossy_codec:None}
    }

    /// Locate RIFF/RF64 sample data without assuming a fixed header size.
    fn wave_payload(bytes: &[u8]) -> &[u8] {
        let mut off = if bytes.starts_with(b"RF64") {48} else {12};
        while off + 8 <= bytes.len() {
            let kind = &bytes[off..off+4];
            let size = u32::from_le_bytes(bytes[off+4..off+8].try_into().unwrap()) as usize;
            if kind == b"data" {
                let end = if bytes.starts_with(b"RF64") { bytes.len() } else {
                    off + 8 + size
                };
                return &bytes[off+8..end];
            }
            off += 8 + size + size%2;
        }
        panic!("Missing WAV data chunk")
    }


    #[test]
    fn sample_aligned_float64_wav_has_correct_header_and_payload() {
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("stream.raw");
        let samples: Vec<f64> = (0..40).map(|v| f64::from(v)/100.0).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s|s.to_le_bytes()).collect();
        fs::write(&raw,&bytes).unwrap();
        let wav = temp.path().join("chapter.wav");
        let spec = float64_spec();
        write_pcm_chapter(&raw,&wav,48000,2,0x3,5,7,&spec,&CancellationToken::new(),
            &CancellationToken::new(),&NativeExtractionControl::default()).unwrap();
        let content = fs::read(&wav).unwrap();
        assert_eq!(&content[..4], b"RIFF");
        assert_eq!(&content[8..12], b"WAVE");
        assert_eq!(&content[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(content[4..8].try_into().unwrap()) as usize + 8, content.len());
        assert_eq!(u16::from_le_bytes(content[20..22].try_into().unwrap()), 0xfffe);
        assert_eq!(&content[44..48], &[3,0,0,0]);
        assert_eq!(&content[80..], &bytes[5*16..12*16]);
    }

    #[test]
    fn rf64_header_records_true_64_bit_lengths_without_allocating_gigabytes() {
        // 2 channels of float64 PCM and >4 GiB of sample data. The writer
        // must use a real ds64 data count, not a wrapped RIFF length.
        let samples = u64::from(u32::MAX) / 16 + 10;
        let length = checked_bytes(samples, 2, 8).unwrap();
        let mut header = Vec::new();
        write_pcm_header(&mut header, 48_000, 2, 0x3, samples,&float64_spec()).unwrap();
        assert_eq!(header.len(), 116);
        assert_eq!(&header[0..4], b"RF64");
        assert_eq!(&header[12..16], b"ds64");
        assert_eq!(u64::from_le_bytes(header[20..28].try_into().unwrap()), length + 108);
        assert_eq!(u64::from_le_bytes(header[28..36].try_into().unwrap()), length);
        assert_eq!(u64::from_le_bytes(header[36..44].try_into().unwrap()), samples);
        assert_eq!(&header[108..112], b"data");
        assert_eq!(&header[112..116], &u32::MAX.to_le_bytes());
    }

    #[test]
    fn replace_policy_never_overwrites_matroska_input() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination");
        let mut job = super::super::tests::fixture_job(&destination, temp.path());
        job.format = DiscFormat::Matroska;
        job.file_per_program = true;
        job.program_subfolders = false;
        job.programs[0].id = PresentationId::MatroskaStream {
            stream_index: 0,
            audio_ordinal: 1,
            chapter_count: 0,
        };
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        let output = proposed_destination_for_program(&job, &job.programs[0]);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"source data must survive").unwrap();
        job.source = output.clone();
        let error = validate_job(&job).unwrap_err();
        assert!(error.contains("own input"), "{error}");
        assert_eq!(fs::read(&output).unwrap().as_slice(), b"source data must survive");
    }

    #[test]
    fn copy_command_is_packet_passthrough_and_only_one_audio_stream() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = super::super::tests::fixture_job(temp.path(),temp.path());
        job.format = DiscFormat::Matroska;
        job.source = temp.path().join("source.mkv");
        let mut program = job.programs[0].clone();
        program.id = PresentationId::MatroskaStream {
            stream_index: 3, audio_ordinal: 2, chapter_count: 2,
        };
        let args = copy_command(&job,&program,&temp.path().join("copy.mka")).unwrap().args;
        assert!(args.windows(2).any(|words|words == ["-map","0:3"]));
        assert!(args.windows(2).any(|words|words == ["-c:a","copy"]));
        assert!(!args.iter().any(|arg|arg == "-c:v"));
        program.source_codec = "ac3".into();
        let args = copy_command(&job,&program,&temp.path().join("copy.ac3")).unwrap().args;
        assert!(args.windows(2).any(|words|words==["-f","ac3"]));
        assert!(args.windows(2).any(|words|words==["-map_chapters","-1"]));
        program.source_codec = "mp3".into();
        let args = copy_command(&job,&program,&temp.path().join("copy.mp3")).unwrap().args;
        assert!(args.windows(2).any(|words|words==["-write_xing","0"]));
        assert!(!args.iter().any(|arg| arg == "-copyts"));
        program.source_codec = "alac".into();
        let args = copy_command(&job,&program,&temp.path().join("copy.mka")).unwrap().args;
        assert!(args.windows(2).any(|words|words==["-f","matroska"]));
        assert!(args.windows(2).any(|words|words==["-map_chapters","0"]));
        assert!(args.windows(2).any(|words|words==["-copyts","-i"]));
        assert!(!args.iter().any(|arg|arg=="-use_editlist"));
        // Only the PCM carrier uses the WAV muxer and automatic RF64.
        program.source_codec = "pcm_s16le".into();
        let args = copy_command(&job,&program,&temp.path().join("copy.wav")).unwrap().args;
        assert!(args.windows(2).any(|words| words == ["-c:a", "copy"]));
        assert!(args.windows(2).any(|words| words == ["-rf64", "auto"]));
        assert!(args.windows(2).any(|words| words == ["-f", "wav"]));
        program.source_codec = "flac".into();
        let args = copy_command(&job,&program,&temp.path().join("copy.flac")).unwrap().args;
        assert!(!args.iter().any(|arg| arg == "-rf64"));
    }

    #[tokio::test]
    async fn r9_pcm_whole_stream_copy_uses_automatic_rf64_and_preserves_samples() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("ffprobe").arg("-version").output().is_err() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("original.mka");
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=431:duration=0.25:sample_rate=48000",
            "-c:a", "pcm_s16le", "-f", "matroska",
        ]).arg(&input).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}",
            String::from_utf8_lossy(&fixture.stderr));
        let contents = map_matroska_file(&input).unwrap();
        assert_eq!(contents.presentations[0].format.codec.as_deref(), Some("pcm_s16le"));
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, input.clone());
        state.tags = false;
        state.extraction_log = false;
        state.destination = temp.path().join("export");
        let job = state.native_job(temp.path().join("scratch"));
        assert_eq!(job.programs[0].source_codec, "pcm_s16le");
        let report = execute_with_controls(job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(report.published.len(), 1);
        let wav = &report.published[0];
        assert_eq!(wav.extension().and_then(|ext|ext.to_str()), Some("wav"));
        let header = fs::read(wav).unwrap();
        // A small output is ordinary RIFF; FFmpeg reserves a JUNK/ds64 slot
        // that its WAV muxer rewrites as RF64 when the 32-bit limit is crossed.
        assert_eq!(&header[..4], b"RIFF");
        assert_eq!(&header[12..16], b"JUNK");
        let mut reference = Vec::new();
        for source in [&input, wav] {
            let pcm = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error",
                "-i",
            ]).arg(source).args([
                "-map", "0:a:0", "-vn", "-sn", "-dn", "-f", "s16le", "-",
            ]).output().unwrap();
            assert!(pcm.status.success(), "decode: {}",
                String::from_utf8_lossy(&pcm.stderr));
            reference.push(pcm.stdout);
        }
        assert_eq!(reference[0].len(), 12_000 * 2);
        assert_eq!(reference[0], reference[1]);
    }

    /// Tests the complete source-to-publication path, including audio stream
    /// 1 after video stream 0, recovery-style skip, and a sample-exact chapter.
    /// Skip only when the external fixture tools are not installed.
    #[tokio::test]
    async fn full_stream_passthrough_and_selected_chapter_split_are_recoverable() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("ffprobe").arg("-version").output().is_err() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("fixture.mkv");
        let chapters = temp.path().join("chapters.txt");
        fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second\n").unwrap();
        let result = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=s=16x16:r=25:d=1",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1:sample_rate=48000",
            "-f", "lavfi", "-i", "sine=frequency=550:duration=1:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&chapters).args([
            "-map", "0:v:0", "-map", "1:a:0", "-map", "2:a:0",
            "-map_chapters", "3", "-c:v", "ffv1", "-c:a", "flac",
        ]).arg(&source).output().unwrap();
        assert!(result.status.success(), "fixture: {}", String::from_utf8_lossy(&result.stderr));
        let contents = map_matroska_file(&source).unwrap();
        assert_eq!(contents.presentations.len(), 2);
        assert_eq!(contents.presentations[0].id, PresentationId::MatroskaStream {
            stream_index: 1, audio_ordinal: 1, chapter_count: 2,
        });
        assert_eq!(contents.presentations[1].id, PresentationId::MatroskaStream {
            stream_index: 2, audio_ordinal: 2, chapter_count: 2,
        });
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.selected[1] = false;
        state.tags = false;
        state.destination = temp.path().join("copies");
        let mut copy_job = state.native_job(temp.path().join("scratch"));
        assert!(copy_job.file_per_program);
        assert_eq!(copy_job.programs.len(), 1);
        let first_copy = execute_with_controls(copy_job.clone(), CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(first_copy.published.len(), 1);
        let copied = &first_copy.published[0];
        assert_eq!(copied.extension().and_then(|ext| ext.to_str()), Some("flac"));
        let info = Command::new("ffprobe").args([
            "-v", "error", "-show_entries", "stream=codec_name,codec_type",
            "-of", "compact",
        ]).arg(copied).output().unwrap();
        assert!(info.status.success());
        let streams = String::from_utf8_lossy(&info.stdout);
        assert!(streams.contains("codec_name=flac"), "{streams}");
        assert!(!streams.contains("codec_type=video"), "{streams}");
        // An already-published selection is retained unmodified by Skip.
        let original_copy = fs::read(copied).unwrap();
        copy_job.overwrite_policy = NativeOverwritePolicy::Skip;
        let second_copy = execute_with_controls(copy_job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert!(second_copy.published.is_empty());
        assert_eq!(second_copy.skipped, 2);
        assert_eq!(fs::read(copied).unwrap(), original_copy);

        state.file_per_program = false;
        state.destination = temp.path().join("chapters-out");
        state.items_checked[0] = vec![true, false];
        let split_job = state.native_job(temp.path().join("scratch"));
        assert_eq!(split_job.programs[0].tracks.len(), 1);
        let split = execute_with_controls(split_job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(split.published.len(), 1);
        let wav = fs::read(&split.published[0]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wave_payload(&wav).len(), 24_000 * 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()),16);
        let raw = temp.path().join("reference.s16le");
        let reference = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:1", "-vn", "-sn", "-dn", "-map_metadata", "-1",
            "-c:a", "pcm_s16le", "-f", "s16le",
        ]).arg(&raw).output().unwrap();
        assert!(reference.status.success());
        let original_pcm = fs::read(&raw).unwrap();
        assert_eq!(wave_payload(&wav), &original_pcm[..24_000 * 2],
            "chapter extraction must not resample or re-encode the decoded PCM");
        assert_eq!(fs::read_dir(&state.destination).unwrap().count(), 1,
            "nothing beyond the selected chapter may be published");
    }

    fn requested_chapter(number: u32) -> NativeTrackChoice {
        NativeTrackChoice {
            number, name: format!("Chapter {number}"), tags: NativeTagValues::default(),
            estimated_bytes: None, duration_secs: None,
        }
    }

    #[test]
    fn audio_stream_origin_and_authored_final_end_define_split_ranges() {
        use crate::convert::chapter_structure::RawEmbeddedChapter;
        let chapter = |start, end| RawEmbeddedChapter {
            title: None, start, end, time_base_num: 1, time_base_den: 1_000,
        };
        let chapters = normalize_embedded_chapters(&[
            chapter(0, 1_000), chapter(1_000, 2_000),
        ], 48_000).unwrap();
        // An audio stream beginning 0.5s after the first video frame has
        // only half a second in chapter 1, then a full second in chapter 2.
        assert_eq!(selected_ranges(&chapters,
            &[requested_chapter(1), requested_chapter(2)], 72_000, 24_000).unwrap(),
            vec![(1, 0, 24_000), (2, 24_000, 48_000)]);
        // The final chapter is explicitly bounded. It must not own an extra
        // 0.5s simply because decoding produced a full second of samples.
        let single = normalize_embedded_chapters(&[chapter(0, 500)], 48_000).unwrap();
        assert_eq!(selected_ranges(&single, &[requested_chapter(1)], 48_000, 0).unwrap(),
            vec![(1, 0, 24_000)]);
        // A negative audio origin (preroll) also uses the presentation clock.
        assert_eq!(selected_ranges(&single, &[requested_chapter(1)], 50_400, -2_400).unwrap(),
            vec![(1, 2_400, 24_000)]);
        assert_eq!(chapter_channel_mask(None, 6), 0);
        assert_eq!(chapter_channel_mask(Some("5.1(side)"), 6), 0x60f);
        assert_eq!(chapter_channel_mask(Some("5.1(back)"), 6), 0x3f);
    }

    #[tokio::test]
    async fn delayed_audio_is_split_on_container_chapter_boundaries() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("delayed.mkv");
        let metadata = temp.path().join("chapters.ffmeta");
        fs::write(&metadata, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Early\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=2000\ntitle=Late\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=s=16x16:r=25:d=2",
            "-itsoffset", "0.5", "-f", "lavfi", "-i",
            "sine=frequency=1000:duration=1.5:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&metadata).args([
            "-map", "0:v:0", "-map", "1:a:0", "-map_chapters", "2",
            "-c:v", "ffv1", "-c:a", "flac",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));
        assert_eq!(stream_origin_samples(&source, 1, 48_000).unwrap(), 24_000);
        let contents = map_matroska_file(&source).unwrap();
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let result = execute_with_controls(job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(result.published.len(), 2);
        let first = fs::read(&result.published[0]).unwrap();
        let second = fs::read(&result.published[1]).unwrap();
        assert_eq!(wave_payload(&first).len(), 24_000 * 2);
        assert_eq!(wave_payload(&second).len(), 48_000 * 2);
        let reference = temp.path().join("reference.s16le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:1", "-vn", "-sn", "-dn", "-c:a", "pcm_s16le",
            "-f", "s16le",
        ]).arg(&reference).output().unwrap();
        assert!(decoded.status.success());
        let pcm = fs::read(&reference).unwrap();
        assert_eq!(pcm.len(), 72_000 * 2);
        assert_eq!(wave_payload(&first), &pcm[..24_000 * 2]);
        assert_eq!(wave_payload(&second), &pcm[24_000 * 2..]);
    }

    #[tokio::test]
    async fn authored_half_second_end_and_explicit_5_1_side_layout_survive_wav() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("six-channel.mka");
        let metadata = temp.path().join("chapters.ffmeta");
        fs::write(&metadata, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Half second\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "anullsrc=channel_layout=5.1(side):sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&metadata).args([
            "-map", "0:a:0", "-map_chapters", "1", "-t", "1", "-c:a", "flac",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));
        let contents = map_matroska_file(&source).unwrap();
        assert_eq!(contents.presentations[0].format.channel_layout.as_deref(), Some("5.1(side)"));
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source);
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let result = execute_with_controls(job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(result.published.len(), 1);
        let wav = fs::read(&result.published[0]).unwrap();
        assert_eq!(wave_payload(&wav).len(), 24_000 * 6 * 2);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 0x60f);
        let check = Command::new("ffprobe").args([
            "-v", "error", "-show_entries", "stream=channels,channel_layout", "-of", "default",
        ]).arg(&result.published[0]).output().unwrap();
        assert!(check.status.success());
        let info = String::from_utf8_lossy(&check.stdout);
        assert!(info.contains("channels=6") && info.contains("channel_layout=5.1(side)"), "{info}");
    }

    #[test]
    fn chapters_before_audio_have_zero_frames_without_invalidating_later_ranges() {
        use crate::convert::chapter_structure::RawEmbeddedChapter;
        let chapter = |start, end| RawEmbeddedChapter {
            title: None, start, end, time_base_num: 1, time_base_den: 1_000,
        };
        let chapters = normalize_embedded_chapters(&[
            chapter(0, 1_000), chapter(1_000, 3_000),
        ], 48_000).unwrap();
        assert_eq!(selected_ranges(&chapters,
            &[requested_chapter(1), requested_chapter(2)], 96_000, 48_000).unwrap(),
            vec![(1, 0, 0), (2, 0, 96_000)]);
    }

    #[tokio::test]
    async fn empty_video_only_chapter_is_skipped_and_following_audio_is_published() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("audio-after-video.mkv");
        let metadata = temp.path().join("chapters.ffmeta");
        fs::write(&metadata, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=No audio\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=3000\ntitle=Audio\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=s=16x16:r=25:d=3",
            "-itsoffset", "1", "-f", "lavfi", "-i",
            "sine=frequency=1000:duration=2:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&metadata).args([
            "-map", "0:v:0", "-map", "1:a:0", "-map_chapters", "2",
            "-c:v", "ffv1", "-c:a", "flac",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));
        assert_eq!(stream_origin_samples(&source, 1, 48_000).unwrap(), 48_000);
        let contents = map_matroska_file(&source).unwrap();
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let controls = Arc::new(NativeExtractionControl::default());
        let result = execute_with_controls(job.clone(), CancellationToken::new(),
            Arc::clone(&controls), |_| {}).await.unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(result.published.len(), 1);
        let skipped = controls.skipped_receipts();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].track_number, Some(1));
        assert_eq!(skipped[0].reason, Some(NativeSkipReason::NoAudioInChapter));
        let receipt = render_native_extraction_log(&job, &result.published, &skipped);
        assert!(receipt.contains("Skipped: chapter contains no audio"), "{receipt}");
        let wav = fs::read(&result.published[0]).unwrap();
        assert_eq!(wave_payload(&wav).len(), 96_000 * 2);
        let reference = temp.path().join("reference.s16le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:1", "-vn", "-sn", "-dn", "-c:a", "pcm_s16le", "-f", "s16le",
        ]).arg(&reference).output().unwrap();
        assert!(decoded.status.success());
        let pcm = fs::read(reference).unwrap();
        assert_eq!(wave_payload(&wav), pcm.as_slice());
        assert_eq!(fs::read_dir(&state.destination).unwrap().count(), 1,
            "an empty chapter must not publish an empty WAV");
    }

    #[tokio::test]
    async fn opus_preroll_does_not_shift_chapter_samples_or_shorten_final_chapter() {
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains("libopus") { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("opus.mka");
        let metadata = temp.path().join("chapters.ffmeta");
        fs::write(&metadata, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=1200:duration=1:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&metadata).args([
            "-map", "0:a:0", "-map_chapters", "1", "-c:a", "libopus", "-b:a", "128k",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));
        let demux = ffmpeg_next::format::input(&source).unwrap();
        assert!(demux.streams().next().unwrap().start_time() < 0,
            "fixture must contain a negative Opus pre-roll packet timestamp");
        assert_eq!(stream_origin_samples(&source, 0, 48_000).unwrap(), 0);
        let contents = map_matroska_file(&source).unwrap();
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let result = execute_with_controls(job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(result.skipped, 0);
        assert_eq!(result.published.len(), 2);
        let first = fs::read(&result.published[0]).unwrap();
        let second = fs::read(&result.published[1]).unwrap();
        assert_eq!(wave_payload(&first).len(), 24_000 * 3);
        assert_eq!(wave_payload(&second).len(), 24_000 * 3);
        assert!(String::from_utf8_lossy(&first).contains("Decoded from lossy opus source"));
        let reference = temp.path().join("reference.s24le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:0", "-vn", "-sn", "-dn", "-c:a", "pcm_s24le", "-f", "s24le",
        ]).arg(&reference).output().unwrap();
        assert!(decoded.status.success());
        let pcm = fs::read(reference).unwrap();
        assert_eq!(pcm.len(), 48_000 * 3);
        assert_eq!(wave_payload(&first), &pcm[..24_000 * 3]);
        assert_eq!(wave_payload(&second), &pcm[24_000 * 3..]);
    }

    /// A 2.5 ms Opus packet holds 120 samples, all of which can be pre-skip.
    /// Reading just one compressed packet yields no retained decoded frame,
    /// but the one-second stream is valid and must split into two WAVs.
    #[tokio::test]
    async fn short_frame_opus_preskip_still_exports_both_chapters() {
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains("libopus")
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("short-opus.mka");
        let chapters = temp.path().join("chapters.ffmeta");
        fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=1200:duration=1:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&chapters).args([
            "-map", "0:a:0", "-map_chapters", "1", "-c:a", "libopus",
            "-b:a", "128k", "-frame_duration", "2.5",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));
        let demux = ffmpeg_next::format::input(&source).unwrap();
        assert!(demux.streams().next().unwrap().start_time() < 0,
            "fixture must contain Opus decoder pre-skip");
        assert_eq!(stream_origin_samples(&source, 0, 48_000).unwrap(), 0);

        let contents = map_matroska_file(&source).unwrap();
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let result = execute_with_controls(job, CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(result.skipped, 0);
        assert_eq!(result.published.len(), 2);
        let first = fs::read(&result.published[0]).unwrap();
        let second = fs::read(&result.published[1]).unwrap();
        assert_eq!(wave_payload(&first).len(), 24_000 * 3);
        assert_eq!(wave_payload(&second).len(), 24_000 * 3);
        assert!(String::from_utf8_lossy(&first).contains("Decoded from lossy opus source"));

        let reference = temp.path().join("normal-decode.s24le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:0", "-vn", "-sn", "-dn", "-c:a", "pcm_s24le",
            "-f", "s24le",
        ]).arg(&reference).output().unwrap();
        assert!(decoded.status.success(), "decode: {}", String::from_utf8_lossy(&decoded.stderr));
        let pcm = fs::read(&reference).unwrap();
        assert_eq!(pcm.len(), 48_000 * 3);
        assert_eq!(wave_payload(&first), &pcm[..24_000 * 3]);
        assert_eq!(wave_payload(&second), &pcm[24_000 * 3..]);
    }

    #[tokio::test]
    async fn positive_opus_start_skips_empty_chapter_and_preserves_decoded_pcm() {
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains("libopus")
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("delayed-opus.mka");
        let chapters = temp.path().join("chapters.ffmeta");
        fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=No audio\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=2600\ntitle=Audio\n").unwrap();
        let fixture = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
            "-itsoffset", "0.5", "-f", "lavfi", "-i",
            "sine=frequency=880:duration=2:sample_rate=48000",
            "-f", "ffmetadata", "-i",
        ]).arg(&chapters).args([
            "-map", "0:a:0", "-map_chapters", "1", "-c:a", "libopus", "-b:a", "128k",
        ]).arg(&source).output().unwrap();
        assert!(fixture.status.success(), "fixture: {}", String::from_utf8_lossy(&fixture.stderr));

        let demux = ffmpeg_next::format::input(&source).unwrap();
        let stream = demux.streams().next().unwrap();
        // This is the failure condition in R3: the packet start precedes the
        // chapter boundary although the decoded stream has no samples there.
        assert_eq!(stream.time_base().numerator(), 1);
        assert_eq!(stream.time_base().denominator(), 1000);
        assert!(stream.start_time() > 0 && stream.start_time() < 500);
        let origin = stream_origin_samples(&source, 0, 48_000).unwrap();
        assert!(origin >= 24_000, "decoded PCM must start at/after 0.5s: {origin}");

        let contents = map_matroska_file(&source).unwrap();
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("split");
        let job = state.native_job(temp.path().join("scratch"));
        let controls = Arc::new(NativeExtractionControl::default());
        let result = execute_with_controls(job.clone(), CancellationToken::new(),
            Arc::clone(&controls), |_| {}).await.unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(result.published.len(), 1);
        let skipped = controls.skipped_receipts();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].track_number, Some(1));
        assert_eq!(skipped[0].reason, Some(NativeSkipReason::NoAudioInChapter));
        let receipt = render_native_extraction_log(&job, &result.published, &skipped);
        assert!(receipt.contains("Skipped: chapter contains no audio"), "{receipt}");
        assert_eq!(fs::read_dir(&state.destination).unwrap().count(), 1,
            "the audio-free chapter must not publish a WAV");

        let wav = fs::read(&result.published[0]).unwrap();
        assert_eq!(wave_payload(&wav).len(), 96_000 * 3);
        let reference = temp.path().join("normal-decode.s24le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i",
        ]).arg(&source).args([
            "-map", "0:0", "-vn", "-sn", "-dn", "-c:a", "pcm_s24le",
            "-f", "s24le",
        ]).arg(&reference).output().unwrap();
        assert!(decoded.status.success());
        let pcm = fs::read(reference).unwrap();
        assert_eq!(pcm.len(), 96_000 * 3);
        assert_eq!(wave_payload(&wav), pcm.as_slice(),
            "the second chapter must contain every retained Opus sample exactly once");
    }


    #[test]
    fn r6_native_carriers_and_cue_eligibility_follow_codec() {
        let mapping = [
            ("flac", "flac", "flac", false),
            ("ac3", "ac3", "ac3", false),
            ("eac3", "eac3", "eac3", false),
            ("aac", "aac", "adts", false),
            ("mp3", "mp3", "mp3", false),
            ("opus", "mka", "matroska", true),
            ("vorbis", "mka", "matroska", true),
            ("truehd", "thd", "truehd", false),
            ("alac", "mka", "matroska", true),
            ("ape", "mka", "matroska", true),
        ];
        for (codec, ext, muxer, chapters) in mapping {
            let carrier = matroska_copy_format(codec);
            assert_eq!(carrier.extension, ext, "{codec}");
            assert_eq!(carrier.muxer, muxer, "{codec}");
            assert_eq!(carrier.chapters, chapters, "{codec}");
        }
    }

    #[test]
    fn r6_odd_byte_pcm24_chapter_is_padded_outside_data_chunk() {
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("one.s24le");
        let wav = temp.path().join("one.wav");
        fs::write(&raw, [1,2,3]).unwrap();
        let spec = ChapterPcm {codec:"pcm_s24le",raw_format:"s24le",bits:24,
            valid_bits:24,sample_bytes:3,floating:false,lossy_codec:Some("aac".into())};
        write_pcm_chapter(&raw,&wav,48_000,1,4,0,1,&spec,
            &CancellationToken::new(),&CancellationToken::new(),
            &NativeExtractionControl::default()).unwrap();
        let file = fs::read(&wav).unwrap();
        assert_eq!(wave_payload(&file), &[1u8,2,3]);
        assert_eq!(file.last(),Some(&0));
        assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize + 8,
            file.len());
        assert!(String::from_utf8_lossy(&file).contains("Decoded from lossy aac source"));
    }

    #[tokio::test]
    async fn r6_native_ac3_stream_cue_is_opt_in_and_packet_preserving() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.mka");
        let chapters = temp.path().join("chapters.ffmeta");
        fs::write(&chapters,";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Second\n").unwrap();
        let create = Command::new("ffmpeg").args([
            "-hide_banner","-nostdin","-loglevel","error","-y",
            "-f","lavfi","-i","sine=frequency=440:duration=1:sample_rate=48000",
            "-f","ffmetadata","-i",
        ]).arg(&chapters).args([
            "-map","0:a:0","-map_chapters","1","-c:a","ac3","-b:a","192k",
        ]).arg(&source).output().unwrap();
        assert!(create.status.success(),"{}",String::from_utf8_lossy(&create.stderr));
        let contents = map_matroska_file(&source).unwrap();
        assert_eq!(contents.presentations[0].format.codec.as_deref(),Some("ac3"));
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.tags = false;
        state.destination = temp.path().join("without-cue");
        assert!(state.matroska_cue_available());
        assert!(state.output_rows().contains(&"CUE"));
        let preview = state.native_preview_path().unwrap();
        assert_eq!(preview.extension().and_then(|v|v.to_str()),Some("ac3"));
        let job = state.native_job(temp.path().join("scratch"));
        assert!(!job.cue);
        let first = execute_with_controls(job,CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_|{}).await.unwrap();
        assert_eq!(first.published.len(),1);
        assert_eq!(first.published[0].extension().and_then(|v|v.to_str()),Some("ac3"));
        assert!(!first.published[0].with_extension("cue").exists());
        let packet_hashes = |path: &Path| {
            let output = Command::new("ffprobe").args([
                "-v","error","-select_streams","a:0","-show_packets",
                "-show_data_hash","sha256","-show_entries","packet=data_hash",
                "-of","default=noprint_wrappers=1:nokey=1",
            ]).arg(path).output().unwrap();
            assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
            output.stdout
        };
        assert_eq!(packet_hashes(&source),packet_hashes(&first.published[0]));

        state.cue = true;
        state.destination = temp.path().join("with-cue");
        let job = state.native_job(temp.path().join("scratch"));
        assert!(job.cue);
        validate_job(&job).unwrap();
        let result = execute_with_controls(job,CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_|{}).await.unwrap();
        assert_eq!(result.published.len(),2);
        let cue = result.published.iter().find(|p|p.extension().is_some_and(|x|x=="cue")).unwrap();
        let text = fs::read_to_string(cue).unwrap();
        assert!(text.contains("FILE \"Audio stream 1.ac3\" WAVE"),"{text}");
        assert!(text.contains("TRACK 01 AUDIO") && text.contains("TRACK 02 AUDIO"),"{text}");
        assert!(text.contains("INDEX 01 00:00:38"),"{text}");
    }

    #[tokio::test]
    async fn r6_flac_24_bit_chapter_preserves_source_integer_precision() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("24bit.mka");
        let metadata = temp.path().join("chapters.txt");
        fs::write(&metadata,";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Half\n").unwrap();
        let create = Command::new("ffmpeg").args([
            "-hide_banner","-nostdin","-loglevel","error","-y",
            "-f","lavfi","-i","sine=frequency=440:duration=1:sample_rate=48000",
            "-f","ffmetadata","-i",
        ]).arg(&metadata).args([
            "-map","0:a:0","-map_chapters","1","-c:a","flac",
            "-sample_fmt","s32","-bits_per_raw_sample","24",
        ]).arg(&source).output().unwrap();
        assert!(create.status.success(),"{}",String::from_utf8_lossy(&create.stderr));
        let contents = map_matroska_file(&source).unwrap();
        assert_eq!(contents.presentations[0].format.bit_depth,Some(24));
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents,source.clone());
        state.file_per_program = false;
        state.tags = false;
        state.destination = temp.path().join("chapters-out");
        let job = state.native_job(temp.path().join("scratch"));
        let result = execute_with_controls(job,CancellationToken::new(),
            Arc::new(NativeExtractionControl::default()), |_|{}).await.unwrap();
        assert_eq!(result.published.len(),1);
        let file = fs::read(&result.published[0]).unwrap();
        assert_eq!(u16::from_le_bytes(file[34..36].try_into().unwrap()),24);
        assert_eq!(wave_payload(&file).len(),24_000*3);
        let raw = temp.path().join("reference.s24le");
        let decoded = Command::new("ffmpeg").args([
            "-hide_banner","-nostdin","-loglevel","error","-y","-i",
        ]).arg(&source).args([
            "-map","0:a:0","-vn","-sn","-dn","-c:a","pcm_s24le","-f","s24le",
        ]).arg(&raw).output().unwrap();
        assert!(decoded.status.success());
        let pcm = fs::read(&raw).unwrap();
        assert_eq!(wave_payload(&file), &pcm[..24_000*3]);
    }


    #[tokio::test]
    async fn r7_native_flac_and_mp3_embed_selected_cover_without_changing_audio_packets() {
        use lofty::file::TaggedFileExt;
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains("libmp3lame")
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let cover = temp.path().join("cover.jpg");
        image::RgbImage::from_pixel(16, 16, image::Rgb([50, 140, 210]))
            .save(&cover).unwrap();
        for codec in ["flac", "libmp3lame"] {
            let source = temp.path().join(format!("{codec}.mka"));
            let create = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=1:sample_rate=48000",
                "-map", "0:a:0", "-c:a", codec,
            ]).arg(&source).output().unwrap();
            assert!(create.status.success(), "{}", String::from_utf8_lossy(&create.stderr));
            let packet_hashes = |path: &Path| -> Vec<String> {
                let output = Command::new("ffprobe").args([
                    "-v", "error", "-select_streams", "a:0", "-show_packets",
                    "-show_data_hash", "sha256", "-show_entries", "packet=data_hash",
                    "-of", "default=noprint_wrappers=1:nokey=1",
                ]).arg(path).output().unwrap();
                assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                // Matroska MP3 may carry decoder Skip Samples side data that
                // raw MP3 cannot represent. Compare compressed packet hashes,
                // not the additional human-readable side-data fields.
                String::from_utf8(output.stdout).unwrap().lines()
                    .filter(|line| line.starts_with("SHA256:"))
                    .map(str::to_owned).collect()
            };
            let source_packets = packet_hashes(&source);
            let contents = map_matroska_file(&source).unwrap();
            for (tags, artwork, expected_picture) in [
                (true, true, true), (true, false, false), (false, true, false),
            ] {
                let mut state = crate::tui::disc_browser::DiscBrowserState::new(
                    contents.clone(), source.clone());
                state.tags = tags;
                state.tag_artwork = artwork;
                state.destination = temp.path().join(format!(
                    "{codec}-tags-{tags}-art-{artwork}"));
                let job = state.native_job(temp.path().join("scratch"));
                let result = execute_with_controls(job, CancellationToken::new(),
                    Arc::new(NativeExtractionControl::default()), |_|{}).await.unwrap();
                assert_eq!(result.published.len(), 1, "{codec}, tags={tags}, art={artwork}");
                let file = &result.published[0];
                assert_eq!(file.extension().and_then(|ext|ext.to_str()),
                    Some(if codec == "flac" { "flac" } else { "mp3" }));
                assert_eq!(packet_hashes(file), source_packets,
                    "the native metadata update may not re-encode audio");
                let tagged = lofty::read_from_path(file).unwrap();
                let has_cover = tagged.tags().iter().any(|tag| !tag.pictures().is_empty());
                assert_eq!(has_cover, expected_picture,
                    "{codec}, tags={tags}, artwork={artwork}");
                // No unrequested CUE or artwork sidecar appears in the destination.
                assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
            }
        }
    }

    /// Matroska's DURATION is a presentation endpoint, not a duration to add
    /// to an ALAC stream's initial PTS. Exercise both FFmpeg's default
    /// audio-only PTS normalization and a later stream behind video stream 0.
    #[tokio::test]
    async fn r8_alac_delayed_copy_preserves_stream_origin_endpoint_and_packets() {
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains(" alac ")
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let chapters = temp.path().join("chapters.ffmeta");
        fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=1500\ntitle=Second\n").unwrap();
        let audio_times = |path: &Path| -> (f64, f64) {
            let output = Command::new("ffprobe").args([
                "-v", "error", "-select_streams", "a:0", "-show_entries",
                "stream=start_time:stream_tags=DURATION",
                "-of", "default=noprint_wrappers=1",
            ]).arg(path).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let text = String::from_utf8(output.stdout).unwrap();
            let start: f64 = text.lines().find_map(|line| line.strip_prefix("start_time="))
                .unwrap().parse().unwrap();
            let end = text.lines().find_map(|line| line.strip_prefix("TAG:DURATION="))
                .unwrap();
            let parts: Vec<_> = end.split(':').collect();
            assert_eq!(parts.len(), 3);
            let end = parts[0].parse::<f64>().unwrap() * 3600.0
                + parts[1].parse::<f64>().unwrap() * 60.0
                + parts[2].parse::<f64>().unwrap();
            (start, end)
        };
        let packet_hashes = |path: &Path| -> Vec<String> {
            let output = Command::new("ffprobe").args([
                "-v", "error", "-select_streams", "a:0", "-show_packets",
                "-show_data_hash", "sha256", "-show_entries", "packet=data_hash",
                "-of", "default=noprint_wrappers=1:nokey=1",
            ]).arg(path).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let hashes: Vec<_> = String::from_utf8(output.stdout).unwrap().lines()
                .filter(|line| line.starts_with("SHA256:"))
                .map(str::to_owned).collect();
            assert!(!hashes.is_empty());
            hashes
        };
        let decoded = |path: &Path| -> Vec<u8> {
            let output = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-i",
            ]).arg(path).args([
                "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", "pcm_s32le",
                "-f", "s32le", "-",
            ]).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            output.stdout
        };
        let chapter_times = |path: &Path| -> Vec<String> {
            let output = Command::new("ffprobe").args([
                "-v", "error", "-show_chapters", "-show_entries",
                "chapter=start_time,end_time", "-of", "csv=p=0",
            ]).arg(path).output().unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().lines()
                .map(|line| line.split(',').take(2).collect::<Vec<_>>().join(","))
                .collect()
        };
        for with_video in [false, true] {
            let source = temp.path().join(if with_video { "video-first.mkv" }
                else { "audio-only.mka" });
            let mut ffmpeg = Command::new("ffmpeg");
            ffmpeg.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
            if with_video {
                ffmpeg.args(["-f", "lavfi", "-i", "color=s=32x32:r=10:d=2"]);
            }
            ffmpeg.args(["-itsoffset", "0.5", "-f", "lavfi", "-i",
                "sine=frequency=770:duration=1:sample_rate=48000",
                "-f", "ffmetadata", "-i"]).arg(&chapters);
            if with_video {
                ffmpeg.args(["-map", "0:v:0", "-map", "1:a:0",
                    "-map_chapters", "2", "-c:v", "mpeg4", "-c:a", "alac"]);
            } else {
                ffmpeg.args(["-map", "0:a:0", "-map_chapters", "1", "-c:a", "alac"]);
            }
            let fixture = ffmpeg.arg(&source).output().unwrap();
            assert!(fixture.status.success(), "{}", String::from_utf8_lossy(&fixture.stderr));
            let (origin, endpoint) = audio_times(&source);
            assert!((origin - 0.5).abs() <= 0.001);
            assert!((endpoint - 1.5).abs() <= 0.001);
            let contents = map_matroska_file(&source).unwrap();
            assert_eq!(contents.presentations.len(), 1);
            let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
            state.tags = false;
            state.destination = temp.path().join(if with_video { "out-video" } else { "out-audio" });
            assert!(!state.matroska_cue_available());
            let job = state.native_job(temp.path().join("scratch"));
            assert!(!job.cue);
            validate_job(&job).unwrap();
            let cmd = copy_command(&job, &job.programs[0],
                &temp.path().join("probe-only.mka")).unwrap();
            assert!(cmd.args.windows(2).any(|args| args == ["-copyts", "-i"]));
            assert!(cmd.args.windows(2).any(|args| args[0] == "-bsf:a"
                && args[1].contains("1500-PTS") && !args[1].contains("STARTPTS+")));
            let result = execute_with_controls(job, CancellationToken::new(),
                Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
            assert_eq!(result.published.len(), 1);
            let native = &result.published[0];
            assert_eq!(native.extension().and_then(|ext| ext.to_str()), Some("mka"));
            assert!(!native.with_extension("cue").exists());
            assert_eq!(fs::read_dir(native.parent().unwrap()).unwrap().count(), 1);
            let (out_origin, out_end) = audio_times(native);
            assert!((out_origin - origin).abs() <= 0.001,
                "audio start changed: {out_origin} vs {origin}");
            assert!((out_end - endpoint).abs() <= 0.001,
                "audio endpoint changed: {out_end} vs {endpoint}");
            assert_eq!(packet_hashes(native), packet_hashes(&source),
                "ALAC packets must not be re-encoded");
            let reference = decoded(&source);
            assert_eq!(reference.len(), 48_000 * 4);
            assert_eq!(decoded(native), reference, "ALAC PCM sample loss");
            let chapters_before = chapter_times(&source);
            assert_eq!(chapters_before.len(), 2);
            assert_eq!(chapter_times(native), chapters_before,
                "chapter presentation times must remain authored");
        }
    }

    #[tokio::test]
    async fn r7_alac_mka_copy_preserves_samples_duration_and_authored_chapters() {
        use std::process::Command;
        let Ok(encoders) = Command::new("ffmpeg").arg("-encoders").output() else { return; };
        if !String::from_utf8_lossy(&encoders.stdout).contains(" alac ")
            || Command::new("ffprobe").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let chapters = temp.path().join("chapters.ffmeta");
        fs::write(&chapters, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=250\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=250\nEND=500\ntitle=Second\n").unwrap();
        for (duration, frames) in [("0.5", 24_000usize), ("1", 48_000), ("2", 96_000)] {
            let source = temp.path().join(format!("alac-{duration}.mka"));
            let create = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i",
            ]).arg(format!("sine=frequency=770:duration={duration}:sample_rate=48000"))
                .args(["-f", "ffmetadata", "-i"]).arg(&chapters)
                .args(["-map", "0:a:0", "-map_chapters", "1", "-c:a", "alac"])
                .arg(&source).output().unwrap();
            assert!(create.status.success(), "{}", String::from_utf8_lossy(&create.stderr));
            let contents = map_matroska_file(&source).unwrap();
            let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
            state.tags = false;
            state.destination = temp.path().join(format!("out-{duration}"));
            assert!(!state.matroska_cue_available(), "MKA retains embedded chapters");
            let job = state.native_job(temp.path().join("scratch"));
            assert!(!job.cue);
            validate_job(&job).unwrap();
            let arguments = copy_command(&job, &job.programs[0],
                &temp.path().join("probe-only.mka")).unwrap().args;
            assert!(arguments.windows(2).any(|args|
                args[0] == "-bsf:a" && args[1].contains("NEXT_PTS")),
                "the per-track Matroska duration must repair ALAC's final packet");
            let result = execute_with_controls(job, CancellationToken::new(),
                Arc::new(NativeExtractionControl::default()), |_|{}).await.unwrap();
            assert_eq!(result.published.len(), 1);
            let native = &result.published[0];
            assert_eq!(native.extension().and_then(|ext|ext.to_str()), Some("mka"));
            assert!(!native.with_extension("cue").exists());
            let pcm = |input: &Path| -> Vec<u8> {
                let out = Command::new("ffmpeg").args([
                    "-hide_banner", "-nostdin", "-loglevel", "error", "-i",
                ]).arg(input).args([
                    "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", "pcm_s32le",
                    "-f", "s32le", "-",
                ]).output().unwrap();
                assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
                out.stdout
            };
            let reference = pcm(&source);
            assert_eq!(reference.len(), frames * 4);
            assert_eq!(pcm(native), reference, "ALAC packets must retain every decoded sample");
            let probe_duration = |path: &Path| -> f64 {
                let output = Command::new("ffprobe").args([
                    "-v", "error", "-show_entries", "format=duration",
                    "-of", "default=noprint_wrappers=1:nokey=1",
                ]).arg(path).output().unwrap();
                assert!(output.status.success());
                String::from_utf8(output.stdout).unwrap().trim().parse().unwrap()
            };
            let expected = frames as f64 / 48_000.0;
            assert!((probe_duration(native) - expected).abs() <= 0.003,
                "retained MKA timeline must match decoded samples");
            let output = Command::new("ffprobe").args([
                "-v", "error", "-show_chapters", "-show_entries", "chapter=id",
                "-of", "csv=p=0",
            ]).arg(native).output().unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2,
                "ALAC fallback must retain authored chapter markers");
        }
    }

    #[test]
    fn standalone_transport_replace_must_never_overwrite_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = super::super::tests::fixture_job(&temp.path().join("destination"), temp.path());
        job.format = DiscFormat::TransportFile;
        job.file_per_program = true;
        job.cue = false;
        job.programs[0].id = PresentationId::TransportStream {
            stream_index: 0, audio_ordinal: 1,
        };
        job.programs[0].tracks.truncate(1);
        job.programs[0].source_codec = "ac3".into();
        job.overwrite_policy = NativeOverwritePolicy::Replace;
        let output = proposed_destination_for_program(&job, &job.programs[0]);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        job.source = temp.path().join("orphan.m2ts");
        fs::write(&job.source, b"source data must survive").unwrap();
        // On a filesystem without symlinks the same-source canonical test
        // cannot be exercised; ordinary output-path validation remains intact.
        #[cfg(unix)] {
            std::os::unix::fs::symlink(&job.source, &output).unwrap();
            let error = validate_job(&job).unwrap_err();
            assert!(error.contains("own input"), "{error}");
            assert_eq!(fs::read(&job.source).unwrap().as_slice(), b"source data must survive");
        }
    }

    #[test]
    fn orphan_transport_copy_commands_are_chapterless_and_stream_specific() {
        let temp = tempfile::tempdir().unwrap();
        let mut job = super::super::tests::fixture_job(temp.path(), temp.path());
        job.format = DiscFormat::TransportFile;
        job.file_per_program = true;
        job.cue = false;
        job.source = temp.path().join("orphan.M2TS");
        let mut program = job.programs[0].clone();
        program.id = PresentationId::TransportStream { stream_index: 3, audio_ordinal: 2 };
        program.source_codec = "ac3".into();
        let args = copy_command(&job, &program, &temp.path().join("stream.ac3")).unwrap().args;
        assert!(args.windows(2).any(|words| words == ["-map", "0:3"]));
        assert!(args.windows(2).any(|words| words == ["-c:a", "copy"]));
        assert!(args.windows(2).any(|words| words == ["-map_chapters", "-1"]));
        assert!(args.windows(2).any(|words| words == ["-f", "ac3"]));
        // The new LPCM WAV rule must not change Phase B's Matroska
        // packet-copy carrier mapping for this unusual codec.
        assert_eq!(matroska_copy_format("pcm_dvd").extension, "mka");
        assert_eq!(file_stream_copy_format(DiscFormat::TransportFile, "pcm_dvd").extension, "wav");
        program.source_codec = "pcm_dvd".into();
        program.source_depth = Some(16);
        let args = copy_command(&job, &program, &temp.path().join("stream.wav")).unwrap().args;
        assert!(args.windows(2).any(|words| words == ["-c:a", "pcm_s16le"]));
        assert!(args.windows(2).any(|words| words == ["-rf64", "auto"]));
        program.source_depth = None;
        assert!(copy_command(&job, &program, &temp.path().join("stream.wav")).is_err());
    }

    #[tokio::test]
    async fn orphan_m2ts_vob_two_streams_publish_only_requested_native_audio() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        for (label, file, muxer) in [
            ("flat-m2ts", "orphan.m2ts", "mpegts"),
            ("flat-vob", "orphan.vob", "vob"),
            ("copied-m2ts", "BDMV/STREAM/00001.m2ts", "mpegts"),
            ("copied-vob", "VIDEO_TS/VTS_01_1.vob", "vob"),
        ] {
            let source = temp.path().join(file);
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            let generated = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=0.4:sample_rate=48000",
                "-f", "lavfi", "-i", "sine=frequency=660:duration=0.4:sample_rate=48000",
                "-map", "0:a:0", "-map", "1:a:0", "-c:a", "ac3", "-b:a", "192k", "-f", muxer,
            ]).arg(&source).output().unwrap();
            assert!(generated.status.success(), "fixture: {}",
                String::from_utf8_lossy(&generated.stderr));
            let original = fs::read(&source).unwrap();
            assert!(!crate::disc::on_disc_transport::has_disc_location(&source));
            let contents = crate::tui::disc_browser::probe_disc_contents(&source).unwrap();
            assert!(contents.diagnostics.iter().any(|d| d.message.contains("No chapters found")));
            let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
            state.tags = false;
            state.extraction_log = false;
            state.destination = temp.path().join(format!("out-{label}"));
            let job = state.native_job(temp.path().join("scratch"));
            assert_eq!(job.format, DiscFormat::TransportFile);
            assert_eq!(job.programs.len(), 2);
            assert!(job.file_per_program);
            validate_job(&job).unwrap();
            let report = execute_with_controls(job.clone(), CancellationToken::new(),
                Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
            assert_eq!(report.published.len(), 2);
            assert_eq!(report.skipped, 0);
            assert_eq!(fs::read(&source).unwrap(), original);
            // No unrequested CUE, log, marker or staging file is allowed in
            // the destination. Count the recursively published leaf files.
            let leaves = report.published.iter().map(|p| p.canonicalize().unwrap())
                .collect::<std::collections::HashSet<_>>();
            let mut directories = vec![job.destination.clone()];
            let mut on_disk = std::collections::HashSet::new();
            while let Some(dir) = directories.pop() {
                for entry in fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() { directories.push(entry.path()); }
                    else { on_disk.insert(entry.path().canonicalize().unwrap()); }
                }
            }
            assert_eq!(on_disk, leaves);
            for (index, copied) in report.published.iter().enumerate() {
                assert_eq!(copied.extension().and_then(|e|e.to_str()), Some("ac3"));
                let reference = temp.path().join(format!("reference-{label}-{index}.ac3"));
                let copied_cmd = Command::new("ffmpeg").args([
                    "-hide_banner", "-nostdin", "-loglevel", "error", "-xerror", "-y",
                    "-i",
                ]).arg(&source).args([
                    "-map", &format!("0:{index}"), "-vn", "-sn", "-dn", "-c:a", "copy",
                    "-map_metadata", "-1", "-map_metadata:s:a:0", "-1", "-map_chapters", "-1",
                    "-f", "ac3",
                ]).arg(&reference).output().unwrap();
                assert!(copied_cmd.status.success(), "reference copy: {}",
                    String::from_utf8_lossy(&copied_cmd.stderr));
                assert_eq!(fs::read(copied).unwrap(), fs::read(reference).unwrap());
            }
            let mut skip = job.clone();
            skip.overwrite_policy = NativeOverwritePolicy::Skip;
            let second = execute_with_controls(skip, CancellationToken::new(),
                Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
            assert_eq!(second.published.len(), 0);
            assert_eq!(second.skipped, 2);
            assert_eq!(fs::read(&source).unwrap(), original);
        }
    }

    #[tokio::test]
    async fn orphan_vob_lpcm_unpacks_without_losing_decoded_samples() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { return; }
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("orphan.vob");
        let generated = Command::new("ffmpeg").args([
            "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-f", "lavfi",
            "-i", "sine=frequency=750:duration=0.6:sample_rate=48000",
            "-c:a", "pcm_dvd", "-f", "vob",
        ]).arg(&source).output().unwrap();
        assert!(generated.status.success());
        let contents = crate::tui::disc_browser::probe_disc_contents(&source).unwrap();
        assert_eq!(contents.presentations[0].format.codec.as_deref(), Some("pcm_dvd"));
        assert_eq!(contents.presentations[0].format.bit_depth, Some(16));
        let mut state = crate::tui::disc_browser::DiscBrowserState::new(contents, source.clone());
        state.tags = false;
        state.destination = temp.path().join("pcm-output");
        let report = execute_with_controls(state.native_job(temp.path().join("scratch")),
            CancellationToken::new(), Arc::new(NativeExtractionControl::default()), |_| {}).await.unwrap();
        assert_eq!(report.published.len(), 1);
        assert_eq!(report.published[0].extension().and_then(|ext|ext.to_str()), Some("wav"));
        let mut pcm = Vec::new();
        for path in [&source, &report.published[0]] {
            let decoded = Command::new("ffmpeg").args([
                "-hide_banner", "-nostdin", "-loglevel", "error", "-i",
            ]).arg(path).args(["-map", "0:a:0", "-f", "s16le", "-"]).output().unwrap();
            assert!(decoded.status.success(), "decode: {}",
                String::from_utf8_lossy(&decoded.stderr));
            pcm.push(decoded.stdout);
        }
        assert!(!pcm[0].is_empty());
        assert_eq!(pcm[0], pcm[1]);
    }

}

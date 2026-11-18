//! Phase D CUE extraction admission. The *shared* split-CUE folder selector
//! decides which sheets are authoritative; this module only projects its
//! admitted members into sample-accurate, independently executable tracks.
//!
//! The `combine` parameter is intentional. A later extract-window setting can
//! select self-contained CUE authority without adding a second grouping policy.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use crate::convert::split_cue_album::{
    self as split, SplitCueAdmissionMember, SplitCueAlbumGroupingDecision,
    SplitCueFolderSelection, SplitCueFolderPreference,
};
use crate::convert::pipeline::native_extract::NativeCueSegment;
use crate::disc::{
    AudioPresentationFormat, CopyProtectionSummary, DiscContents, DiscFormat,
    DiscPresentation, DiscTrack, FormatProvenance, PresentationExtractInfo,
    PresentationId, DiscExtractTagFields,
};

#[derive(Debug)]
pub struct CueExtractPreview {
    pub contents: DiscContents,
    /// Aligned to `contents.presentations` and each presentation's tracks.
    pub segments: Vec<Vec<NativeCueSegment>>,
}

fn supported_format(path: &Path, codec: &str, depth: Option<u32>, float: Option<bool>)
    -> Result<(String, String, String, Option<u32>), String>
{
    let ext = path.extension().and_then(|value| value.to_str())
        .unwrap_or_default().to_ascii_lowercase();
    // A filename extension alone does not establish the carrier. Never
    // silently change a mislabeled source into a different encoded format.
    let codec_expected = match ext.as_str() {
        "flac" => Some("flac"), "wv" => Some("wavpack"),
        "mp3" => Some("mp3"), "opus" => Some("opus"),
        _ => None,
    };
    if codec_expected.is_some_and(|expected|!codec.eq_ignore_ascii_case(expected)) {
        return Err(format!("CUE image extension and decoded codec disagree: {} ({codec})",
            path.display()));
    }
    let result: (&str, &str, &str, Option<u32>) = match ext.as_str() {
        "flac" if depth.is_some_and(|bits|bits > 24) =>
            return Err("FLAC encoder cannot retain source samples above 24 bits".into()),
        "flac" => ("flac", "flac", "flac", depth),
        "wav" | "wave" => ("wav", if float == Some(true) {
            if depth == Some(64) {"pcm_f64le"} else {"pcm_f32le"}
        } else if depth == Some(8) {"pcm_u8"}
        else if depth.unwrap_or(16) > 24 {"pcm_s32le"}
        else if depth.unwrap_or(16) > 16 {"pcm_s24le"} else {"pcm_s16le"}, "wav", depth),
        "aif" | "aiff" => ("aiff", if float == Some(true) {
            if depth == Some(64) {"pcm_f64be"} else {"pcm_f32be"}
        } else if depth == Some(8) {"pcm_s8"}
        else if depth.unwrap_or(16) > 24 {"pcm_s32be"}
        else if depth.unwrap_or(16) > 16 {"pcm_s24be"} else {"pcm_s16be"}, "aiff", depth),
        "wv" => ("wv", "wavpack", "wv", depth),
        "mp3" => ("mp3", "libmp3lame", "mp3", depth),
        "m4a" if codec.eq_ignore_ascii_case("alac") => ("m4a", "alac", "ipod", depth),
        "m4a" | "aac" if codec.eq_ignore_ascii_case("aac") =>
            ("m4a", "aac", "ipod", depth),
        "ogg" if codec.eq_ignore_ascii_case("vorbis") =>
            ("ogg", "libvorbis", "ogg", depth),
        "ogg" if codec.eq_ignore_ascii_case("opus") =>
            ("ogg", "libopus", "ogg", depth),
        "opus" => ("opus", "libopus", "opus", depth),
        _ => return Err(format!("Cannot re-encode '{}' as its original format ({codec}); \
            choose an image with a supported encoder", path.display())),
    };
    Ok((result.0.into(), result.1.into(), result.2.into(), result.3))
}

fn check_regular_snapshot(path: &Path) -> Result<(u64, Option<std::time::SystemTime>), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error|
        format!("Inspect CUE input '{}': {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("CUE input must be a regular, non-symlink file: {}", path.display()));
    }
    Ok((metadata.len(), metadata.modified().ok()))
}

/// Build the CUE member list from the shared folder choice; explicit CUE
/// selection resolves only genuine alternative-source ambiguity. No UI/worker
/// ever guesses a synthetic album by filename suffixes or track counts.
fn members_for_source(source: &Path, combine: bool) -> Result<Vec<SplitCueAdmissionMember>, String> {
    if !combine && source.is_file() {
        return split::admit_split_cue_member(source).map(|member|vec![member])
            .map_err(|err|err.to_string());
    }
    let folder = if source.is_dir() { source } else {
        source.parent().ok_or("Selected CUE has no parent directory")?
    };
    let candidates = split::split_cue_candidate_paths(&[folder.to_path_buf()]);
    if candidates.is_empty() { return Err("This folder has no direct CUE sheets".into()); }
    let chosen = split::select_split_cue_folder_members(&candidates, None);
    let chosen = match chosen {
        SplitCueFolderSelection::Selected { members, .. } if source.is_dir() ||
            members.iter().any(|m| m.cue_path == source) => return Ok(members),
        // A specifically clicked CUE is authoritative when the automatic
        // folder choice would otherwise select an alternative description.
        SplitCueFolderSelection::Selected { .. } | SplitCueFolderSelection::NeedsChoice { .. }
            if source.is_file() => split::select_split_cue_folder_members_with_preference(
                &candidates, Some(SplitCueFolderPreference::Cue(source))),
        other => other,
    };
    match chosen {
        SplitCueFolderSelection::Selected { members, .. } if !members.is_empty() => Ok(members),
        SplitCueFolderSelection::NeedsChoice { .. } => Err(
            "Several CUE sheets describe the same image. Open the desired CUE sheet directly".into()),
        SplitCueFolderSelection::NoViable { rejected } => Err(rejected.first()
            .map(ToString::to_string).unwrap_or_else(||"No valid CUE/image pair".into())),
        _ => Err("No CUE/image pair admitted by the shared album policy".into()),
    }
}

fn sheet_title(member: &SplitCueAdmissionMember) -> String {
    member.sheet.title.clone().unwrap_or_else(||member.cue_path.file_stem()
        .map(|value|value.to_string_lossy().into_owned()).unwrap_or_else(||"CUE album".into()))
}

fn album_groups(members: &[SplitCueAdmissionMember], combine: bool,
    previous: Option<&SplitCueAlbumGroupingDecision>) -> Result<Vec<Vec<SplitCueAdmissionMember>>, String>
{
    if !combine || members.len() <= 1 {
        return Ok(members.iter().cloned().map(|m|vec![m]).collect());
    }
    let paths: Vec<_> = members.iter().map(|member|member.cue_path.clone()).collect();
    let titles: Vec<_> = members.iter().map(sheet_title).collect();
    let key = split::grouping_key_from_paths(&paths);
    // A cached editor/queue decision is authoritative for this exact set.
    // Without one, use their shared non-network title/ambiguous ladder.
    let decision = previous.filter(|decision| {
        let found: Vec<_> = decision.groups.iter().flatten().cloned().collect();
        split::grouping_key_from_paths(&found) == key
    }).cloned().or_else(|| split::decide_with_toc_evidence(&paths,&titles,None,None))
        .ok_or("CUE grouping spans multiple folders")?;
    let mut remaining: BTreeMap<PathBuf,SplitCueAdmissionMember> = members.iter()
        .cloned().map(|m|{
            let key = fs::canonicalize(&m.cue_path).unwrap_or_else(|_|m.cue_path.clone());
            (key,m)
        }).collect();
    let mut groups = Vec::new();
    for group in decision.groups {
        let mut selected = Vec::new();
        for path in group {
            let member = remaining.remove(&path).ok_or("CUE group includes a non-admitted sheet")?;
            selected.push(member);
        }
        if !selected.is_empty() { groups.push(selected); }
    }
    if !remaining.is_empty() { return Err("CUE group omitted an admitted sheet".into()); }
    Ok(groups)
}

/// Probe off the UI thread. Admission, title/TOC grouping and INDEX geometry
/// are independent operations so combining can be turned off in later UI.
pub fn inspect(source: &Path, combine: bool,
    cached_decisions: &[SplitCueAlbumGroupingDecision]) -> Result<CueExtractPreview, String>
{
    let members = members_for_source(source,combine)?;
    let admitted_key = split::grouping_key_from_paths(&members.iter()
        .map(|m|m.cue_path.clone()).collect::<Vec<_>>());
    let previous = cached_decisions.iter().find(|decision|{
        let paths = decision.groups.iter().flatten().cloned().collect::<Vec<_>>();
        split::grouping_key_from_paths(&paths) == admitted_key
    });
    let groups = album_groups(&members, combine, previous)?;
    let mut presentations = Vec::new();
    let mut segments_by_program = Vec::new();
    for (group_index, group) in groups.into_iter().enumerate() {
        let id = PresentationId::CueAlbum(u32::try_from(group_index+1).map_err(|_|"Too many CUE albums")?);
        let album_name = if group.len() > 1 {
            split::meaningful_common_cue_album_prefix(&group.iter().map(sheet_title).collect::<Vec<_>>())
                .unwrap_or_else(||sheet_title(&group[0]))
        } else {sheet_title(&group[0])};
        let album_artist = group[0].sheet.performer.clone();
        let album_genre = group[0].sheet.genre.clone();
        let album_year = group[0].sheet.date.clone();
        let album_catalog = group[0].sheet.catalog.clone();
        let mut media_cache: BTreeMap<PathBuf, (u64, u32, crate::tui::probe::SourceInfo,
            u64,Option<std::time::SystemTime>)> = BTreeMap::new();
        let mut tracks = Vec::new();
        let mut segments = Vec::new();
        let mut authored = Vec::new();
        for member in &group {
            let (cue_length,cue_modified) = check_regular_snapshot(&member.cue_path)?;
            for image in &member.referenced_audio {
                if !media_cache.contains_key(image) {
                    let (len,modified) = check_regular_snapshot(image)?;
                    let (samples,rate) = crate::tui::accuraterip::probe_sample_count(image)?;
                    let info = crate::tui::probe::probe_audio(image)?;
                    if samples == 0 || rate == 0 || info.sample_rate != rate || info.channels == 0 {
                        return Err(format!("Invalid CUE audio clock/extent: {}", image.display()));
                    }
                    let _ = supported_format(image,&info.codec,info.bit_depth,info.sample_format_is_float)?;
                    media_cache.insert(image.clone(),(samples,rate,info,len,modified));
                }
            }
            for (j,track) in member.sheet.tracks.iter().enumerate() {
                let image = &member.track_audio_paths[j];
                let (samples,rate,info,len,modified) = media_cache.get(image)
                    .ok_or("CUE track has no admitted image")?;
                let index = track.index01_frames.ok_or("CUE track is missing INDEX 01")?;
                let start = u64::try_from(u128::from(index)*u128::from(*rate)/75)
                    .map_err(|_|"CUE track start overflows sample clock")?;
                let next_index = member.track_audio_paths.iter().enumerate().skip(j+1)
                    .find(|(_,next)| *next == image)
                    .and_then(|(i,_)|member.sheet.tracks[i].index01_frames);
                let end = if let Some(index)=next_index {
                    u64::try_from(u128::from(index)*u128::from(*rate)/75)
                        .map_err(|_|"CUE track end overflows sample clock")?
                } else {*samples};
                if start >= end || end > *samples {
                    return Err(format!("CUE indexes exceed or overlap audio in {}", image.display()));
                }
                let (extension,encoder,muxer,depth)=supported_format(image,
                    &info.codec,info.bit_depth,info.sample_format_is_float)?;
                let number = u32::try_from(tracks.len()+1).map_err(|_|"Too many CUE tracks")?;
                let title = track.title.clone().unwrap_or_else(||format!("Track {:02}",track.number));
                let artist = track.performer.clone().or_else(||member.sheet.performer.clone());
                tracks.push(DiscTrack {number,title:Some(title.clone()),performer:artist.clone(),
                    duration_secs:Some((end-start) as f64 / f64::from(*rate)),format_note:Some(extension.to_uppercase())});
                authored.push(DiscExtractTagFields { title:track.title.clone(),artist:artist.clone(),
                    album:Some(album_name.clone()),genre:member.sheet.genre.clone(),
                    year:member.sheet.date.clone(),performer:track.performer.clone(),
                    catalog:member.sheet.catalog.clone(),..Default::default() });
                segments.push(NativeCueSegment {presentation:id,number,
                    cue_path:member.cue_path.clone(),image:image.clone(),start_sample:start,
                    end_sample:end,total_samples:*samples,sample_rate:*rate,extension,encoder,muxer,
                    sample_bits:depth,channels:u8::try_from(info.channels)
                        .map_err(|_|"Too many CUE audio channels")?,
                    source_length:*len,source_modified:*modified,
                    cue_length,cue_modified});
            }
        }
        if tracks.is_empty() { return Err("CUE has no extractable tracks".into()); }
        let first = media_cache.values().next().ok_or("CUE has no audio")?;
        // Mixed-format albums retain per-track carriers in `segments`; the
        // presentation summary is descriptive rather than an encoding policy.
        let uniform = media_cache.values().all(|(_,rate,info,_,_)| *rate == first.1 &&
            info.codec == first.2.codec && info.channels == first.2.channels);
        let format = AudioPresentationFormat {
            codec: if uniform {Some(first.2.codec.clone())} else {Some("mixed source formats".into())},
            sample_rate: if uniform {Some(first.1)} else {None},
            bit_depth: if uniform {first.2.bit_depth} else {None},
            channels: if uniform {u8::try_from(first.2.channels).ok()} else {None},
            channel_layout: if uniform {Some(first.2.channel_layout.clone())} else {None},
            lossless: media_cache.values().all(|(_,_,info,_,_)|
                info.compression_is_lossless.unwrap_or(false)),
            provenance:FormatProvenance::Unknown,
        };
        let duration = tracks.iter().filter_map(|t|t.duration_secs).sum();
        presentations.push(DiscPresentation { id, label: album_name.clone(),format,tracks,
            total_duration_secs:duration,extract:PresentationExtractInfo {
                authored_album_tags:DiscExtractTagFields { album:Some(album_name.clone()),
                    artist:album_artist.clone(), genre:album_genre.clone(),year:album_year.clone(),
                    catalog:album_catalog, ..Default::default() },
                authored_track_tags:authored,..Default::default()
            }, album_title:Some(album_name), album_artist, genre:album_genre,year:album_year });
        segments_by_program.push(segments);
    }
    let name = if presentations.len() == 1 { presentations[0].label.clone() }
        else { source.file_name().unwrap_or_default().to_string_lossy().into_owned() };
    Ok(CueExtractPreview { contents:DiscContents {
        format:DiscFormat::CueImage,label:name.clone(),source_path:source.to_path_buf(),
        presentations,suppressed:Vec::new(),
        copy_protection:CopyProtectionSummary {description:"None (CUE image)".into()},
        diagnostics:Vec::new(), album_title:Some(name),album_artist:None,genre:None,year:None,
    }, segments:segments_by_program })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_are_never_silently_substituted() {
        assert_eq!(supported_format(Path::new("side.flac"),"flac",Some(24),None)
            .unwrap().1,"flac");
        assert_eq!(supported_format(Path::new("side.wav"),"pcm_s24le",Some(24),Some(false))
            .unwrap().1,"pcm_s24le");
        assert!(supported_format(Path::new("side.ape"),"ape",Some(16),None).is_err());
        assert!(supported_format(Path::new("misnamed.flac"),"pcm_s16le",Some(16),None).is_err());
    }

    fn write_stereo_pcm_wav(path: &Path, sample_rate: u32, frames: u32) {
        let data_bytes = frames.checked_mul(4).unwrap();
        let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_bytes.to_le_bytes());
        bytes.resize(44 + data_bytes as usize, 0);
        fs::write(path,bytes).unwrap();
    }

    #[test]
    fn related_sheets_share_one_album_and_can_be_separated_without_new_grouping() {
        let dir = tempfile::tempdir().unwrap();
        for side in ["A", "B"] {
            let image = format!("side-{side}.wav");
            write_stereo_pcm_wav(&dir.path().join(&image), 48_000, 144_000);
            let cue = format!("TITLE \"Album Side {side}\"\nPERFORMER \"Artist\"\n\
                FILE \"{image}\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"First {side}\"\n    INDEX 01 00:00:00\n\
                  TRACK 02 AUDIO\n    TITLE \"Second {side}\"\n    INDEX 01 00:01:00\n");
            fs::write(dir.path().join(format!("Album Side {side}.cue")),cue).unwrap();
        }
        let selected = dir.path().join("Album Side A.cue");
        let joined = inspect(&selected,true,&[]).unwrap();
        assert_eq!(joined.contents.presentations.len(),1,
            "shared split-CUE policy should combine these related sides");
        assert_eq!(joined.contents.presentations[0].tracks.len(),4);
        let segments = &joined.segments[0];
        assert_eq!(segments.iter().map(|s|s.number).collect::<Vec<_>>(),vec![1,2,3,4]);
        assert_eq!(segments.iter().map(|s|(s.start_sample,s.end_sample))
            .collect::<Vec<_>>(),vec![(0,48_000),(48_000,144_000),
                (0,48_000),(48_000,144_000)]);
        assert!(segments.iter().all(|s|s.extension == "wav" && s.sample_rate == 48_000));

        let separate = inspect(&selected,false,&[]).unwrap();
        assert_eq!(separate.contents.presentations.len(),1);
        assert_eq!(separate.contents.presentations[0].tracks.len(),2);
        assert_eq!(separate.segments[0][0].start_sample,0);
        assert_eq!(separate.segments[0][1].end_sample,144_000);
    }
}

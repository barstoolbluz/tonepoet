//! Read-only RF64 metadata adapter for Lofty 0.21, which accepts RIFF/WAVE but
//! rejects the RF64 signature. This builds a bounded in-memory *metadata view*,
//! never an audio replacement or a candidate for serialization. Audio payloads
//! (including multi-GB `data` chunks) are skipped using the ds64 data size.
//!
//! Keeping the real RIFF tag chunks lets the existing Lofty INFO/ID3 readers,
//! canonical editor mapping, and artwork handling remain the only tag authority.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

// Lofty's normal metadata read already loads artwork into memory. Cap the
// exceptional path instead of trusting RF64 chunk sizes as allocation requests.
const MAX_CHUNK_BYTES: u64 = 32 * 1024 * 1024;
const MAX_VIEW_BYTES: usize = 64 * 1024 * 1024;

/// Test the *file signature*, not a `.wav`/`.rf64` suffix. Called only after
/// the ordinary Lofty reader refuses a source, keeping healthy RIFF reads on
/// their original zero-extra-I/O path.
pub(super) fn has_rf64_magic(path: &Path) -> bool {
    let Ok(mut source) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    source.read_exact(&mut magic).is_ok() && &magic == b"RF64"
}

fn metadata_view(path: &Path) -> Result<Vec<u8>, String> {
    let mut source = File::open(path).map_err(|e| format!("open RF64 '{}': {e}", path.display()))?;
    let file_len = source.metadata()
        .map_err(|e| format!("stat RF64 '{}': {e}", path.display()))?.len();
    let mut signature = [0u8; 12];
    source.read_exact(&mut signature)
        .map_err(|e| format!("read RF64 header '{}': {e}", path.display()))?;
    if &signature[..4] != b"RF64" || &signature[8..12] != b"WAVE" {
        return Err("RF64 file does not contain a WAVE header".to_string());
    }

    // Reserve RIFF header; every retained chunk is copied in source order.
    // Add a tiny placeholder `data` *after* all tag chunks: Lofty does not
    // need the original samples to parse INFO, ID3v2 or embedded artwork.
    let mut view = b"RIFF\0\0\0\0WAVE".to_vec();
    let mut offset = 12u64;
    let mut data_size_64 = None;
    let mut saw_fmt = false;
    let mut saw_data = false;
    while offset.checked_add(8).is_some_and(|next| next <= file_len) {
        source.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek RF64 chunk: {e}"))?;
        let mut header = [0u8; 8];
        source.read_exact(&mut header)
            .map_err(|e| format!("read RF64 chunk: {e}"))?;
        let id = &header[..4];
        let size_32 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let body = offset + 8;
        if id == b"ds64" {
            if size_32 < 28 || data_size_64.is_some()
                || body.checked_add(u64::from(size_32)).is_none_or(|end| end > file_len)
            {
                return Err("invalid or duplicate RF64 ds64 chunk".to_string());
            }
            let mut ds64 = [0u8; 28];
            source.read_exact(&mut ds64)
                .map_err(|e| format!("read RF64 ds64: {e}"))?;
            // ds64: riffSize(u64), dataSize(u64), sampleCount(u64), tableLength(u32).
            // Extended-size non-audio chunks are deliberately not accepted:
            // they cannot fit in the bounded metadata-only RIFF projection.
            data_size_64 = Some(u64::from_le_bytes(ds64[8..16].try_into().unwrap()));
        }
        let size = if size_32 == u32::MAX {
            if id != b"data" {
                return Err("RF64 metadata chunk uses unsupported 64-bit chunk length".to_string());
            }
            data_size_64.ok_or("RF64 data chunk lacks preceding ds64 size")?
        } else {
            u64::from(size_32)
        };
        let end = body.checked_add(size).ok_or("RF64 chunk length overflow")?;
        if end > file_len {
            return Err(format!("truncated RF64 {:?} chunk", String::from_utf8_lossy(id)));
        }
        if id == b"data" {
            saw_data = true;
        } else if id != b"ds64" && id != b"JUNK" && id != b"PAD " && id != b"FLLR" {
            if size > MAX_CHUNK_BYTES || view.len().saturating_add(8 + size as usize + 1) > MAX_VIEW_BYTES {
                return Err("RF64 non-audio metadata exceeds bounded read limit".to_string());
            }
            if id == b"fmt " {
                saw_fmt = true;
            }
            // Use the exact source bytes, including both RIFF/INFO and ID3
            // containers. Don't reinterpret tags here or invent metadata.
            view.extend_from_slice(&header);
            let start = view.len();
            view.resize(start + size as usize, 0);
            source.seek(SeekFrom::Start(body))
                .map_err(|e| format!("seek RF64 metadata: {e}"))?;
            source.read_exact(&mut view[start..])
                .map_err(|e| format!("read RF64 metadata: {e}"))?;
            if size & 1 != 0 { view.push(0); }
        }
        offset = end.checked_add(size & 1).ok_or("RF64 chunk alignment overflow")?;
        if offset > file_len {
            return Err("truncated RF64 chunk padding".to_string());
        }
    }
    if offset != file_len {
        return Err("trailing truncated RF64 chunk header".to_string());
    }
    if data_size_64.is_none() || !saw_fmt || !saw_data {
        return Err("RF64 missing ds64, fmt or data chunk".to_string());
    }
    // A tiny placeholder is sufficient for WAVE type identification and
    // property parsing; no audio bytes from the source are read or exposed.
    view.extend_from_slice(b"data\x00\x01\x00\x00");
    view.resize(view.len() + 256, 0);
    let riff_size = u32::try_from(view.len() - 8)
        .map_err(|_| "RF64 metadata view is too large for RIFF".to_string())?;
    view[4..8].copy_from_slice(&riff_size.to_le_bytes());
    Ok(view)
}

pub(super) fn read_tags(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    let view = metadata_view(path)?;
    // `guess_file_type` fails with io::Error and `read` with LoftyError, so
    // they cannot chain through `and_then`.
    let probe = lofty::probe::Probe::new(std::io::Cursor::new(view))
        .guess_file_type()
        .map_err(|e| format!("parse RF64 metadata '{}': {e}", path.display()))?;
    probe
        .read()
        .map_err(|e| format!("parse RF64 metadata '{}': {e}", path.display()))
}

/// Small, real RIFF/RF64 audio carriers for integration tests of the distinct
/// metadata consumers. Both variants contain byte-identical ID3 tag chunks.
#[cfg(test)]
pub(crate) mod reader_test_fixture {
    use std::path::Path;
    use id3::TagLike as _;

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() & 1 != 0 {
            out.push(0);
        }
        out
    }

    pub(crate) fn write_tagged_wav(
        path: &Path,
        rf64: bool,
        title: Option<&str>,
        track: Option<u32>,
        pre_emphasis: Option<&str>,
    ) {
        let mut tags = id3::Tag::new();
        if let Some(title) = title {
            tags.set_title(title);
            tags.set_artist("RF64 Test Artist");
            tags.set_album("RF64 Test Album");
            tags.set_date_recorded(id3::Timestamp {
                year: 1994,
                month: None,
                day: None,
                hour: None,
                minute: None,
                second: None,
            });
        }
        if let Some(track) = track {
            tags.set_track(track);
        }
        if let Some(value) = pre_emphasis {
            tags.add_frame(id3::frame::ExtendedText {
                description: "PRE_EMPHASIS".to_owned(),
                value: value.to_owned(),
            });
        }

        let fmt = chunk(b"fmt ", &[1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0x10, 0xB1, 2, 0, 4, 0, 16, 0]);
        let pcm = [0u8; 16];
        let mut wav = if rf64 {
            let mut bytes = b"RF64\xff\xff\xff\xffWAVE".to_vec();
            let mut ds64 = [0u8; 28];
            ds64[8..16].copy_from_slice(&(pcm.len() as u64).to_le_bytes());
            bytes.extend(chunk(b"ds64", &ds64));
            bytes
        } else {
            b"RIFF\0\0\0\0WAVE".to_vec()
        };
        wav.extend(fmt);
        if title.is_some() || track.is_some() || pre_emphasis.is_some() {
            let mut encoded = Vec::new();
            tags.write_to(&mut encoded, id3::Version::Id3v24)
                .expect("encode ID3 WAV tag fixture");
            wav.extend(chunk(b"id3 ", &encoded));
        }
        wav.extend(chunk(b"data", &pcm));

        if rf64 {
            let riff_len = wav.len() as u64 - 8;
            wav[20..28].copy_from_slice(&riff_len.to_le_bytes());
        } else {
            let riff_len = wav.len() as u32 - 8;
            wav[4..8].copy_from_slice(&riff_len.to_le_bytes());
        }
        std::fs::write(path, wav).expect("write WAV metadata test fixture");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;
    use std::io::Write;

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() & 1 != 0 { bytes.push(0); }
        bytes
    }

    fn example_rf64(data: &[u8], tags_after_data: bool) -> Vec<u8> {
        let mut info = b"INFO".to_vec();
        info.extend(chunk(b"INAM", b"Reel Around The Fountain\0"));
        info.extend(chunk(b"IART", b"The Smiths\0"));
        let info = chunk(b"LIST", &info);
        let fmt = chunk(b"fmt ", &[1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0x10, 0xB1, 2, 0, 4, 0, 16, 0]);
        let mut bytes = b"RF64\xff\xff\xff\xffWAVE".to_vec();
        let mut ds64 = [0u8; 28];
        ds64[8..16].copy_from_slice(&(data.len() as u64).to_le_bytes());
        bytes.extend(chunk(b"ds64", &ds64));
        bytes.extend(fmt);
        if !tags_after_data { bytes.extend(&info); }
        bytes.extend_from_slice(b"data\xff\xff\xff\xff");
        bytes.extend_from_slice(data);
        if data.len() & 1 != 0 { bytes.push(0); }
        if tags_after_data { bytes.extend(info); }
        let riff_size = bytes.len() as u64 - 8;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }

    #[test]
    fn rf64_info_tags_read_before_and_after_data_without_audio_copy() {
        let dir = tempfile::tempdir().unwrap();
        for after in [false, true] {
            let path = dir.path().join(if after { "after.wav" } else { "before.rf64" });
            let bytes = example_rf64(&[0u8; 4096], after);
            File::create(&path).unwrap().write_all(&bytes).unwrap();
            assert!(has_rf64_magic(&path));
            let view = metadata_view(&path).unwrap();
            assert!(view.len() < 1024, "audio samples must not be copied");
            assert!(!view.windows(4).any(|four| four == b"RF64"));
            let tags = read_tags(&path).unwrap();
            let tag = tags.primary_tag().or_else(|| tags.first_tag()).unwrap();
            assert_eq!(tag.title().as_deref(), Some("Reel Around The Fountain"));
            assert_eq!(tag.artist().as_deref(), Some("The Smiths"));
            let displayed = super::super::read_metadata(&path).unwrap();
            assert_eq!(displayed.title.as_deref(), Some("Reel Around The Fountain"));
            assert_eq!(displayed.artist.as_deref(), Some("The Smiths"));
            let repeated = super::super::read_metadata(&path).unwrap();
            assert_eq!(repeated.title, displayed.title, "RF64 reads must be idempotent");
            assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes.len() as u64,
                "metadata reads must never rewrite the original carrier");
        }
    }

    #[test]
    fn rf64_embedded_id3_artwork_is_returned_by_the_existing_lofty_reader() {
        use id3::frame::{Picture, PictureType};
        use id3::TagLike as _;
        // A valid 1x1 PNG. The metadata adapter must not decode/recompress it.
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
            0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
            0x00, 0x05, 0x00, 0x01, 0xa5, 0xf6, 0x45, 0x40,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
            0xae, 0x42, 0x60, 0x82,
        ];
        let mut physical = id3::Tag::new();
        physical.add_frame(Picture {
            mime_type: "image/png".to_string(),
            picture_type: PictureType::CoverFront,
            description: String::new(),
            data: png.to_vec(),
        });
        let mut encoded = Vec::new();
        physical.write_to(&mut encoded, id3::Version::Id3v24).unwrap();
        let mut bytes = example_rf64(&[0u8; 8], true);
        bytes.extend(chunk(b"id3 ", &encoded));
        let riff_size = bytes.len() as u64 - 8;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artwork.wav");
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        let tagged = read_tags(&path).unwrap();
        assert!(tagged.tags().iter().any(|tag| tag.pictures().iter()
            .any(|picture| picture.pic_type() == lofty::picture::PictureType::CoverFront
                && picture.data() == png.as_slice())));
        let artwork = super::super::read_embedded_picture_bytes(
            &path, lofty::picture::PictureType::CoverFront,
        ).unwrap();
        assert_eq!(artwork, png);
    }

    #[test]
    fn sparse_multigigabyte_rf64_metadata_is_bounded_to_metadata_size() {
        // A true >4-GiB source without allocating/writing >4 GiB of samples.
        // Catch accidental regression to `read_to_end` or an O(audio size)
        // adapter without turning the test into a heavyweight I/O benchmark.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi-gigabyte.wav");
        let mut file = File::create(&path).unwrap();
        let mut bytes = example_rf64(&[], false);
        let data_header = bytes.windows(8).position(|w| w == b"data\xff\xff\xff\xff").unwrap();
        bytes.truncate(data_header + 8);
        let length = (1u64 << 32) + 512;
        let riff_size = bytes.len() as u64 + length - 8;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());
        bytes[28..36].copy_from_slice(&length.to_le_bytes());
        file.write_all(&bytes).unwrap();
        file.set_len(bytes.len() as u64 + length).unwrap();
        drop(file);

        let view = metadata_view(&path).unwrap();
        assert!(view.len() < 1024);
        let tag = read_tags(&path).unwrap();
        assert!(tag.tags().iter().any(|tag| tag.title().as_deref() == Some("Reel Around The Fountain")));
    }

    #[test]
    fn rf64_refuses_incomplete_ds64_and_truncated_audio_without_allocating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.wav");
        let mut bytes = example_rf64(&[0u8; 8], false);
        bytes[28..36].copy_from_slice(&u64::MAX.to_le_bytes());
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        assert!(metadata_view(&path).is_err());
        bytes = example_rf64(&[0u8; 8], false);
        bytes[12..16].copy_from_slice(b"JUNK");
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        assert!(metadata_view(&path).is_err());
    }
}

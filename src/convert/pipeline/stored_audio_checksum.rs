//! Read container-declared audio MD5 without decoding or hashing the file.
//!
//! These values are encoder-authored metadata, not proof that decoding was
//! correct. Absence, malformed metadata, and unsupported formats are silent:
//! no fabricated digest is written into the human conversion log.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tonepoet_pipeline::AudioFormat;

/// A container-embedded digest, already qualified for human presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredAudioMd5 {
    pub kind: &'static str,
    pub hex: String,
}

pub(crate) fn inspect_stored_audio_md5(
    path: &Path,
    format: &AudioFormat,
    wavpack_hybrid: bool,
) -> Option<StoredAudioMd5> {
    let mut file = File::open(path).ok()?;
    let (kind, digest) = match format {
        AudioFormat::Flac => ("FLAC STREAMINFO", flac_streaminfo_md5(&mut file)?),
        // Hybrid without its correction file does not carry a lossless PCM
        // identity, even if an MD5-like item is encountered in the metadata.
        AudioFormat::WavPack if !wavpack_hybrid => {
            ("WavPack", wavpack_declared_md5(&mut file)?)
        }
        _ => return None,
    };
    Some(StoredAudioMd5 {
        kind,
        hex: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}

fn flac_streaminfo_md5<R: Read + Seek>(reader: &mut R) -> Option<[u8; 16]> {
    let stream_start = crate::metadata_persistence::detect_flac_stream_offset(reader).ok()??;
    reader.seek(SeekFrom::Start(stream_start.checked_add(4)?)).ok()?;
    let mut metadata_header = [0u8; 4];
    reader.read_exact(&mut metadata_header).ok()?;
    let block_len = u32::from_be_bytes([
        0, metadata_header[1], metadata_header[2], metadata_header[3],
    ]);
    if metadata_header[0] & 0x7f != 0 || block_len != 34 {
        return None;
    }
    let mut streaminfo = [0u8; 34];
    reader.read_exact(&mut streaminfo).ok()?;
    let mut digest = [0u8; 16];
    digest.copy_from_slice(&streaminfo[18..34]);
    // FLAC specifies zero as "unset", not an MD5 of silent PCM.
    (digest != [0u8; 16]).then_some(digest)
}

// WavPack4/5 metadata: two-byte header, optional 2-byte extended size;
// payload length in two-byte words, with an optional odd-byte flag. The
// trailing MD5 is typically in the final block, so move by checked block
// extents instead of reading/decoding the (potentially enormous) bitstream.
fn wavpack_declared_md5<R: Read + Seek>(reader: &mut R) -> Option<[u8; 16]> {
    const HEADER_SIZE: u64 = 32;
    const LARGE: u8 = 0x80;
    const ODD: u8 = 0x40;
    const UNIQUE: u8 = 0x3f;
    const MD5: u8 = 0x26;
    const ALT_MD5: u8 = 0x29;
    const MAX_BLOCKS: usize = 1_000_000;

    let file_len = reader.seek(SeekFrom::End(0)).ok()?;
    let mut block_start = 0u64;
    let mut found: Option<[u8; 16]> = None;
    let mut blocks = 0;
    while block_start.checked_add(HEADER_SIZE)? <= file_len && blocks < MAX_BLOCKS {
        reader.seek(SeekFrom::Start(block_start)).ok()?;
        let mut header = [0u8; 32];
        reader.read_exact(&mut header).ok()?;
        if &header[..4] != b"wvpk" {
            // APEv2/ID3 trailing tags are not WavPack blocks. A first-block
            // mismatch is invalid; after valid blocks, trailing tags are fine.
            return if blocks > 0 { found } else { None };
        }
        let ck_size = u32::from_le_bytes(header[4..8].try_into().ok()?) as u64;
        let version = u16::from_le_bytes(header[8..10].try_into().ok()?);
        if ck_size < 24 || !(0x0402..0x0600).contains(&version) {
            return None;
        }
        let block_end = block_start.checked_add(ck_size)?.checked_add(8)?;
        if block_end > file_len {
            return None;
        }
        // The WavPack HYBRID_FLAG is defined in the block flags, even if the
        // caller's settings were obtained from an older request.
        let flags = u32::from_le_bytes(header[24..28].try_into().ok()?);
        if flags & 0x8 != 0 {
            return None;
        }
        blocks += 1;
        let mut offset = block_start + HEADER_SIZE;
        while offset < block_end {
            if block_end - offset < 2 {
                return None;
            }
            reader.seek(SeekFrom::Start(offset)).ok()?;
            let mut meta = [0u8; 2];
            reader.read_exact(&mut meta).ok()?;
            offset += 2;
            let mut length = u64::from(meta[1]) * 2;
            if meta[0] & LARGE != 0 {
                if block_end - offset < 2 {
                    return None;
                }
                let mut extra = [0u8; 2];
                reader.read_exact(&mut extra).ok()?;
                offset += 2;
                length = length.checked_add(u64::from(extra[0]) << 9)?
                    .checked_add(u64::from(extra[1]) << 17)?;
            }
            if (meta[0] & ODD != 0 && length == 0) || length > block_end - offset {
                return None;
            }
            let data_len = length - u64::from(meta[0] & ODD != 0);
            if matches!(meta[0] & UNIQUE, MD5 | ALT_MD5) && data_len == 16 {
                let mut digest = [0u8; 16];
                reader.read_exact(&mut digest).ok()?;
                if let Some(previous) = found {
                    if previous != digest { return None; }
                }
                found = Some(digest);
            }
            offset = offset.checked_add(length)?;
        }
        block_start = block_end;
    }
    if blocks == MAX_BLOCKS || blocks == 0 { None } else { found }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn streaminfo(digest: [u8; 16]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&[0x80, 0, 0, 34]);
        bytes.extend_from_slice(&[0; 18]);
        bytes.extend_from_slice(&digest);
        bytes
    }

    #[test]
    fn flac_reads_only_stored_streaminfo_and_ignores_unset_digest() {
        let digest = [0x8c; 16];
        assert_eq!(flac_streaminfo_md5(&mut Cursor::new(streaminfo(digest))), Some(digest));
        assert_eq!(flac_streaminfo_md5(&mut Cursor::new(streaminfo([0; 16]))), None);
        let mut malformed = streaminfo(digest);
        malformed[7] = 33;
        assert_eq!(flac_streaminfo_md5(&mut Cursor::new(malformed)), None);
        assert_eq!(flac_streaminfo_md5(&mut Cursor::new(b"not flac")), None);
    }

    fn block(meta_id: u8, digest: &[u8], odd: bool, hybrid: bool) -> Vec<u8> {
        let padded = digest.len() + usize::from(odd);
        assert_eq!(padded % 2, 0);
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(b"wvpk");
        bytes[8..10].copy_from_slice(&0x410u16.to_le_bytes());
        if hybrid { bytes[24] |= 8; }
        bytes.extend_from_slice(&[meta_id | if odd { 0x40 } else { 0 }, (padded / 2) as u8]);
        bytes.extend_from_slice(digest);
        if odd { bytes.push(0); }
        let ck = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&ck.to_le_bytes());
        bytes
    }

    #[test]
    fn wavpack_reads_md5_from_later_block_not_audio_bits() {
        let mut bytes = block(0x0a, &[0xa5; 16], false, false);
        let expected = [0x37; 16];
        bytes.extend(block(0x26, &expected, false, false));
        bytes.extend_from_slice(b"APETAGEX");
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(bytes)), Some(expected));
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(block(0x0a, &[0x42; 16], false, false))), None);
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(block(0x29, &expected, false, false))), Some(expected));
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(block(0x26, &expected, false, true))), None);
    }

    #[test]
    fn wavpack_refuses_malformed_metadata_and_conflicting_checksums() {
        let mut bytes = block(0x26, &[0x37; 16], false, false);
        bytes.extend(block(0x26, &[0x38; 16], false, false));
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(bytes)), None);
        let mut truncated = block(0x26, &[0x37; 16], false, false);
        truncated.truncate(40);
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(truncated)), None);
        let mut bad_size = block(0x26, &[0x37; 16], false, false);
        bad_size[33] = 250;
        assert_eq!(wavpack_declared_md5(&mut Cursor::new(bad_size)), None);
    }

    #[test]
    fn on_disk_filter_avoids_mislabeling_non_audio_file_md5() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audio.flac");
        std::fs::write(&path, streaminfo([0xab; 16])).unwrap();
        let md5 = inspect_stored_audio_md5(&path, &AudioFormat::Flac, false).unwrap();
        assert_eq!(md5.kind, "FLAC STREAMINFO");
        assert_eq!(md5.hex, "abababababababababababababababab");
        assert!(inspect_stored_audio_md5(&path, &AudioFormat::Alac, false).is_none());
    }
}

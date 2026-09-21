//! Recognition of legacy ID3-wrapped native FLAC files.
//!
//! Some older rippers prepend an ID3v2 tag and append an ID3v1 `TAG` block
//! around an otherwise valid native FLAC stream. FFmpeg can decode the FLAC
//! audio correctly and then report the trailing ID3v1 bytes as invalid FLAC
//! frame data. The helpers here authorize treating that *post-extent* decoder
//! error as EOF only when the wrapper shape and STREAMINFO sample extent are
//! both independently verified.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const ID3V2_HEADER_LEN: u64 = 10;
const ID3V1_LEN: u64 = 128;
const FLAC_MARKER: &[u8; 4] = b"fLaC";
const STREAMINFO_LEN: usize = 34;

/// Verified legacy wrapper information for a native FLAC stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WrappedFlacExtent {
    /// Per-channel sample-frame count declared by FLAC STREAMINFO.
    pub(crate) sample_frames: u64,
}

/// Return the declared FLAC extent only for the exact legacy envelope this
/// compatibility path is intended to tolerate: ID3v2 prefix + native FLAC +
/// ID3v1 trailer. Ordinary FLACs and ambiguous/malformed wrappers return
/// `Ok(None)` and therefore retain normal decoder-error handling.
pub(crate) fn wrapped_flac_extent(path: &Path) -> io::Result<Option<WrappedFlacExtent>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < ID3V2_HEADER_LEN + 4 + 4 + STREAMINFO_LEN as u64 + ID3V1_LEN {
        return Ok(None);
    }

    let mut id3v2 = [0_u8; ID3V2_HEADER_LEN as usize];
    file.read_exact(&mut id3v2)?;
    if &id3v2[..3] != b"ID3" {
        return Ok(None);
    }
    let Some(tag_payload_len) = synchsafe_u28(&id3v2[6..10]) else {
        return Ok(None);
    };
    let footer_len = if id3v2[5] & 0x10 != 0 { 10_u64 } else { 0_u64 };
    let prefix_len = ID3V2_HEADER_LEN
        .checked_add(u64::from(tag_payload_len))
        .and_then(|value| value.checked_add(footer_len))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ID3v2 extent overflow"))?;
    if prefix_len + 4 + 4 + STREAMINFO_LEN as u64 + ID3V1_LEN > len {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(prefix_len))?;
    let mut marker = [0_u8; 4];
    file.read_exact(&mut marker)?;
    if &marker != FLAC_MARKER {
        return Ok(None);
    }

    let mut block_header = [0_u8; 4];
    file.read_exact(&mut block_header)?;
    let block_type = block_header[0] & 0x7f;
    let block_len = (u32::from(block_header[1]) << 16)
        | (u32::from(block_header[2]) << 8)
        | u32::from(block_header[3]);
    if block_type != 0 || block_len as usize != STREAMINFO_LEN {
        return Ok(None);
    }
    let mut streaminfo = [0_u8; STREAMINFO_LEN];
    file.read_exact(&mut streaminfo)?;
    let packed = u64::from_be_bytes(streaminfo[10..18].try_into().expect("fixed STREAMINFO slice"));
    let sample_frames = packed & 0x0f_ffff_ffff;
    if sample_frames == 0 {
        // FLAC permits an unknown total-samples field. It cannot authorize the
        // narrow post-extent EOF exception because there is no exact extent.
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(ID3V1_LEN as i64)))?;
    let mut tag = [0_u8; 3];
    file.read_exact(&mut tag)?;
    if &tag != b"TAG" {
        return Ok(None);
    }

    Ok(Some(WrappedFlacExtent { sample_frames }))
}

fn synchsafe_u28(bytes: &[u8]) -> Option<u32> {
    let [a, b, c, d]: [u8; 4] = bytes.try_into().ok()?;
    if [a, b, c, d].iter().any(|value| value & 0x80 != 0) {
        return None;
    }
    Some(
        (u32::from(a) << 21)
            | (u32::from(b) << 14)
            | (u32::from(c) << 7)
            | u32::from(d),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn wrapped_fixture(sample_frames: u64) -> Vec<u8> {
        assert!(sample_frames > 0 && sample_frames < (1_u64 << 36));
        let mut bytes = Vec::new();
        // Empty ID3v2.4 prefix.
        bytes.extend_from_slice(&[b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0x80, 0, 0, STREAMINFO_LEN as u8]);
        let mut streaminfo = [0_u8; STREAMINFO_LEN];
        // 44.1 kHz, 2 channels, 16-bit, requested total sample count.
        let packed = (44_100_u64 << 44) | (1_u64 << 41) | (15_u64 << 36) | sample_frames;
        streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());
        bytes.extend_from_slice(&streaminfo);
        bytes.extend_from_slice(&[0_u8; 16]);
        let mut id3v1 = [0_u8; ID3V1_LEN as usize];
        id3v1[..3].copy_from_slice(b"TAG");
        bytes.extend_from_slice(&id3v1);
        bytes
    }

    #[test]
    fn recognizes_exact_wrapped_flac_and_declared_extent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrapped.flac");
        File::create(&path)
            .unwrap()
            .write_all(&wrapped_fixture(48_000))
            .unwrap();
        assert_eq!(
            wrapped_flac_extent(&path).unwrap(),
            Some(WrappedFlacExtent { sample_frames: 48_000 })
        );
    }

    #[test]
    fn refuses_missing_id3v1_or_unknown_streaminfo_extent() {
        let dir = tempfile::tempdir().unwrap();
        let no_trailer = dir.path().join("no-trailer.flac");
        let mut bytes = wrapped_fixture(48_000);
        let trailer = bytes.len() - ID3V1_LEN as usize;
        bytes[trailer..trailer + 3].copy_from_slice(b"BAD");
        File::create(&no_trailer).unwrap().write_all(&bytes).unwrap();
        assert_eq!(wrapped_flac_extent(&no_trailer).unwrap(), None);

        let unknown = dir.path().join("unknown.flac");
        File::create(&unknown)
            .unwrap()
            .write_all(&wrapped_fixture(1))
            .unwrap();
        // Zero just the 36-bit total-samples field while preserving the upper
        // sample-rate/channel/bit-depth bits.
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&unknown).unwrap();
        file.seek(SeekFrom::Start(10 + 4 + 4 + 10)).unwrap();
        let mut packed = [0_u8; 8];
        file.read_exact(&mut packed).unwrap();
        let value = u64::from_be_bytes(packed) & !0x0f_ffff_ffff;
        file.seek(SeekFrom::Start(10 + 4 + 4 + 10)).unwrap();
        file.write_all(&value.to_be_bytes()).unwrap();
        assert_eq!(wrapped_flac_extent(&unknown).unwrap(), None);
    }

    #[test]
    fn synchsafe_parser_rejects_high_bits() {
        assert_eq!(synchsafe_u28(&[0, 0, 1, 0]), Some(128));
        assert_eq!(synchsafe_u28(&[0x80, 0, 0, 0]), None);
    }
}

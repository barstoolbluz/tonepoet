//! Recognition of legacy ID3-wrapped native FLAC files.
//!
//! Some older rippers prepend an ID3v2 tag and append an ID3v1 `TAG` block
//! around an otherwise valid native FLAC stream. FFmpeg can decode the FLAC
//! audio correctly and then report the trailing ID3v1 bytes as invalid FLAC
//! frame data. A stream copy may discard the ID3v2 prefix while retaining the
//! trailer, so decode authorization follows the file actually being read, not
//! the shape of the original source.
//!
//! The helpers here authorize treating a *post-extent* decoder error as EOF
//! only when a terminal ID3v1 block and the STREAMINFO sample extent are both
//! independently verified. Publication cleanup remains narrower: it is only
//! offered when the conversion source has the full legacy ID3v2 + FLAC + ID3v1
//! envelope.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlacEnvelopeLayout {
    file_len: u64,
    flac_offset: u64,
    sample_frames: Option<u64>,
    has_id3v1_trailer: bool,
}

/// Shared decode guard for a native FLAC stream whose trailing ID3v1 bytes may
/// make FFmpeg report a post-audio decoder error.
///
/// The compatibility rule is deliberately narrow: an error is accepted as EOF
/// only after the independently verified STREAMINFO extent has been decoded.
/// The same guard also prevents a decoder from producing samples beyond that
/// extent and verifies exact completion before callers accept the decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WrappedFlacDecodeGuard {
    declared_sample_frames: Option<u64>,
}

impl WrappedFlacDecodeGuard {
    /// Inspect `path` only when the caller is performing a complete-file decode.
    /// Seeked or bounded decodes must pass `whole_file = false`; they cannot prove
    /// that an error occurred after the terminal STREAMINFO sample.
    pub(crate) fn for_path(path: &Path, whole_file: bool) -> io::Result<Self> {
        if !whole_file {
            return Ok(Self::default());
        }
        Ok(Self {
            declared_sample_frames: decode_guard_flac_extent(path)?
                .map(|extent| extent.sample_frames),
        })
    }

    #[must_use]
    pub(crate) const fn declared_sample_frames(self) -> Option<u64> {
        self.declared_sample_frames
    }

    /// Return true only for the exact post-extent error the compatibility path
    /// authorizes. Callers use this for demux, packet-submit, flush, and receive
    /// failures; ordinary FFmpeg EOF remains ordinary EOF.
    #[must_use]
    pub(crate) const fn accepts_post_extent_error(self, decoded_frames: u64) -> bool {
        matches!(self.declared_sample_frames, Some(expected) if expected == decoded_frames)
    }

    /// Advance a decoded sample-frame count without allowing a verified wrapped
    /// FLAC to exceed its STREAMINFO extent.
    pub(crate) fn checked_advance(
        self,
        decoded_frames: u64,
        frame_samples: u64,
    ) -> Result<u64, WrappedFlacDecodeError> {
        let next = decoded_frames
            .checked_add(frame_samples)
            .ok_or(WrappedFlacDecodeError::FrameCountOverflow)?;
        if let Some(expected) = self.declared_sample_frames {
            if next > expected {
                return Err(WrappedFlacDecodeError::ExceededExtent { expected, next });
            }
        }
        Ok(next)
    }

    /// Require exact completion for a verified wrapped FLAC. Ordinary inputs have
    /// no declared extent here and therefore pass unchanged.
    pub(crate) fn validate_complete(
        self,
        decoded_frames: u64,
    ) -> Result<(), WrappedFlacDecodeError> {
        if let Some(expected) = self.declared_sample_frames {
            if decoded_frames != expected {
                return Err(WrappedFlacDecodeError::IncompleteExtent {
                    expected,
                    decoded: decoded_frames,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrappedFlacDecodeError {
    FrameCountOverflow,
    ExceededExtent { expected: u64, next: u64 },
    IncompleteExtent { expected: u64, decoded: u64 },
}

impl std::fmt::Display for WrappedFlacDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::FrameCountOverflow => f.write_str("decoded sample-frame count overflow"),
            Self::ExceededExtent { expected, next } => write!(
                f,
                "ID3-wrapped FLAC decoder exceeded STREAMINFO extent: expected at most {expected} frames, decoded frame would reach {next}"
            ),
            Self::IncompleteExtent { expected, decoded } => write!(
                f,
                "ID3-wrapped FLAC decoded extent mismatch: STREAMINFO declares {expected} frames, decoded {decoded}"
            ),
        }
    }
}

/// Return the declared FLAC extent only for the exact legacy envelope this
/// compatibility path is intended to tolerate: ID3v2 prefix + native FLAC +
/// ID3v1 trailer. Ordinary FLACs and ambiguous/malformed wrappers return
/// `Ok(None)` and therefore retain normal decoder-error handling.
///
/// Production goes through [`WrappedFlacDecodeGuard::for_path`]; this narrow
/// recognizer is kept for the tests that pin the exact source envelope.
#[cfg(test)]
pub(crate) fn wrapped_flac_extent(path: &Path) -> io::Result<Option<WrappedFlacExtent>> {
    let Some(layout) = inspect_flac_envelope(path)? else {
        return Ok(None);
    };
    if layout.flac_offset == 0 || !layout.has_id3v1_trailer {
        return Ok(None);
    }
    let Some(sample_frames) = layout.sample_frames else {
        return Ok(None);
    };
    Ok(Some(WrappedFlacExtent {
        sample_frames,
    }))
}

/// Return the byte range that should be retained when publishing a FLAC
/// converted from an exact legacy ID3v2 + FLAC + ID3v1 source.
///
/// This is deliberately provenance-gated by the source. The converted output
/// may still have both wrappers, or FFmpeg may already have dropped the ID3v2
/// prefix while retaining the terminal ID3v1 block. A terminal block is
/// stripped only when its complete 128 bytes match the source trailer, so an
/// unrelated terminal `TAG` signature is never silently removed.
pub(crate) fn wrapped_flac_normalization_range(
    source: &Path,
    converted: &Path,
) -> io::Result<Option<Range<u64>>> {
    let Some(source_layout) = inspect_flac_envelope(source)? else {
        return Ok(None);
    };
    if source_layout.flac_offset == 0 || !source_layout.has_id3v1_trailer {
        return Ok(None);
    }

    let Some(converted_layout) = inspect_flac_envelope(converted)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "converted output is not a structurally recognizable native FLAC: {}",
                converted.display()
            ),
        ));
    };

    let mut end = converted_layout.file_len;
    if converted_layout.has_id3v1_trailer {
        let source_tag = read_id3v1_trailer(source, source_layout.file_len)?;
        let converted_tag = read_id3v1_trailer(converted, converted_layout.file_len)?;
        if source_tag != converted_tag {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "converted FLAC has an unexpected terminal ID3v1 block: {}",
                    converted.display()
                ),
            ));
        }
        end = end.checked_sub(ID3V1_LEN).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "ID3v1 trailer extent underflow")
        })?;
    }

    let start = converted_layout.flac_offset;
    if start == 0 && end == converted_layout.file_len {
        return Ok(None);
    }
    if end <= start {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "converted FLAC wrapper bounds are invalid: {}..{} in {}",
                start,
                end,
                converted.display()
            ),
        ));
    }
    Ok(Some(start..end))
}

fn decode_guard_flac_extent(path: &Path) -> io::Result<Option<WrappedFlacExtent>> {
    let Some(layout) = inspect_flac_envelope(path)? else {
        return Ok(None);
    };
    if !layout.has_id3v1_trailer {
        return Ok(None);
    }
    let Some(sample_frames) = layout.sample_frames else {
        return Ok(None);
    };
    Ok(Some(WrappedFlacExtent {
        sample_frames,
    }))
}

fn inspect_flac_envelope(path: &Path) -> io::Result<Option<FlacEnvelopeLayout>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let minimum_flac_len = 4 + 4 + STREAMINFO_LEN as u64;
    if file_len < minimum_flac_len {
        return Ok(None);
    }

    let mut prefix_header = [0_u8; ID3V2_HEADER_LEN as usize];
    let prefix_probe_len = usize::try_from(file_len.min(ID3V2_HEADER_LEN))
        .expect("bounded ID3v2 prefix probe fits usize");
    file.read_exact(&mut prefix_header[..prefix_probe_len])?;
    let flac_offset = if prefix_probe_len == ID3V2_HEADER_LEN as usize
        && &prefix_header[..3] == b"ID3"
    {
        let Some(tag_payload_len) = synchsafe_u28(&prefix_header[6..10]) else {
            return Ok(None);
        };
        let footer_len = if prefix_header[5] & 0x10 != 0 {
            ID3V2_HEADER_LEN
        } else {
            0
        };
        ID3V2_HEADER_LEN
            .checked_add(u64::from(tag_payload_len))
            .and_then(|value| value.checked_add(footer_len))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ID3v2 extent overflow"))?
    } else {
        0
    };
    if flac_offset
        .checked_add(minimum_flac_len)
        .map_or(true, |minimum_end| minimum_end > file_len)
    {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(flac_offset))?;
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
    let declared_sample_frames = packed & 0x0f_ffff_ffff;
    // FLAC permits zero for an unknown total-samples field. Structural
    // publication cleanup does not need a decoded-sample claim, but the decode
    // guard does and therefore rejects `None` at its own boundary.
    let sample_frames = (declared_sample_frames != 0).then_some(declared_sample_frames);

    let has_id3v1_trailer = if file_len >= ID3V1_LEN {
        file.seek(SeekFrom::End(-(ID3V1_LEN as i64)))?;
        let mut tag = [0_u8; 3];
        file.read_exact(&mut tag)?;
        &tag == b"TAG"
    } else {
        false
    };

    Ok(Some(FlacEnvelopeLayout {
        file_len,
        flac_offset,
        sample_frames,
        has_id3v1_trailer,
    }))
}

fn read_id3v1_trailer(path: &Path, file_len: u64) -> io::Result<[u8; ID3V1_LEN as usize]> {
    if file_len < ID3V1_LEN {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("FLAC is too short to contain an ID3v1 trailer: {}", path.display()),
        ));
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(file_len - ID3V1_LEN))?;
    let mut trailer = [0_u8; ID3V1_LEN as usize];
    file.read_exact(&mut trailer)?;
    Ok(trailer)
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
    fn decode_guard_accepts_errors_only_at_verified_extent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrapped.flac");
        File::create(&path)
            .unwrap()
            .write_all(&wrapped_fixture(48_000))
            .unwrap();

        let guard = WrappedFlacDecodeGuard::for_path(&path, true).unwrap();
        assert_eq!(guard.declared_sample_frames(), Some(48_000));
        assert!(!guard.accepts_post_extent_error(47_999));
        assert!(guard.accepts_post_extent_error(48_000));
        assert_eq!(guard.checked_advance(47_000, 1_000).unwrap(), 48_000);
        assert!(matches!(
            guard.checked_advance(48_000, 1),
            Err(WrappedFlacDecodeError::ExceededExtent {
                expected: 48_000,
                next: 48_001
            })
        ));
        assert!(guard.validate_complete(48_000).is_ok());
        assert!(matches!(
            guard.validate_complete(47_999),
            Err(WrappedFlacDecodeError::IncompleteExtent {
                expected: 48_000,
                decoded: 47_999
            })
        ));
    }

    #[test]
    fn decode_guard_recognizes_stream_copy_that_dropped_only_id3v2_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream-copy.flac");
        let bytes = wrapped_fixture(48_000);
        File::create(&path)
            .unwrap()
            .write_all(&bytes[ID3V2_HEADER_LEN as usize..])
            .unwrap();

        assert_eq!(
            wrapped_flac_extent(&path).unwrap(),
            None,
            "the exact source-wrapper recognizer must remain provenance-narrow"
        );
        let guard = WrappedFlacDecodeGuard::for_path(&path, true).unwrap();
        assert_eq!(guard.declared_sample_frames(), Some(48_000));
        assert!(guard.accepts_post_extent_error(48_000));
        assert!(!guard.accepts_post_extent_error(47_999));
    }

    #[test]
    fn normalization_range_strips_source_prefix_and_matching_trailer_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.flac");
        let wrapped = wrapped_fixture(48_000);
        File::create(&source).unwrap().write_all(&wrapped).unwrap();

        let copied = dir.path().join("copied.flac");
        File::create(&copied).unwrap().write_all(&wrapped).unwrap();
        assert_eq!(
            wrapped_flac_normalization_range(&source, &copied).unwrap(),
            Some(ID3V2_HEADER_LEN..wrapped.len() as u64 - ID3V1_LEN)
        );

        let stream_copy = dir.path().join("stream-copy.flac");
        let trailer_only = &wrapped[ID3V2_HEADER_LEN as usize..];
        File::create(&stream_copy)
            .unwrap()
            .write_all(trailer_only)
            .unwrap();
        assert_eq!(
            wrapped_flac_normalization_range(&source, &stream_copy).unwrap(),
            Some(0..trailer_only.len() as u64 - ID3V1_LEN)
        );

        let clean = dir.path().join("clean.flac");
        let clean_bytes = &wrapped
            [ID3V2_HEADER_LEN as usize..wrapped.len() - ID3V1_LEN as usize];
        File::create(&clean).unwrap().write_all(clean_bytes).unwrap();
        assert_eq!(
            wrapped_flac_normalization_range(&source, &clean).unwrap(),
            None
        );
    }

    #[test]
    fn normalization_refuses_unrelated_terminal_tag_block() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.flac");
        let wrapped = wrapped_fixture(48_000);
        File::create(&source).unwrap().write_all(&wrapped).unwrap();

        let converted = dir.path().join("converted.flac");
        let mut altered = wrapped[ID3V2_HEADER_LEN as usize..].to_vec();
        let trailer = altered.len() - ID3V1_LEN as usize;
        altered[trailer + 3] = 0x7f;
        File::create(&converted).unwrap().write_all(&altered).unwrap();

        let error = wrapped_flac_normalization_range(&source, &converted)
            .expect_err("mismatched terminal ID3v1 bytes must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unexpected terminal ID3v1 block"));
    }

    #[test]
    fn decode_guard_is_inert_for_non_whole_file_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrapped.flac");
        File::create(&path)
            .unwrap()
            .write_all(&wrapped_fixture(48_000))
            .unwrap();

        let guard = WrappedFlacDecodeGuard::for_path(&path, false).unwrap();
        assert_eq!(guard.declared_sample_frames(), None);
        assert!(!guard.accepts_post_extent_error(48_000));
        assert_eq!(guard.checked_advance(48_000, 1).unwrap(), 48_001);
        assert!(guard.validate_complete(0).is_ok());
    }

    #[test]
    fn synchsafe_parser_rejects_high_bits() {
        assert_eq!(synchsafe_u28(&[0, 0, 1, 0]), Some(128));
        assert_eq!(synchsafe_u28(&[0x80, 0, 0, 0]), None);
    }
}

//! `dvd://` URI scheme — opens a DVD-Video ISO image or block device
//! and surfaces it to `oxideav_core::SourceRegistry`.
//!
//! Supported URI forms:
//!
//! - `dvd:///abs/path/to/disc.iso` — open the file.
//! - `dvd:///dev/sr0` — open a block device (Unix).
//! - `dvd://` — Phase 2 (auto-detect a mounted DVD by walking
//!   `/Volumes`, `/media`, `/mnt` and probing each candidate for
//!   `VIDEO_TS/`). Currently returns `Unsupported`.
//!
//! Phase 1 surfaces the disc as a typed `DvdDiscSource`: a thin
//! wrapper that carries the parsed [`DvdDisc`] enumeration plus the
//! underlying file handle for byte-range reads. The reason we don't
//! materialise the first VOB as a `BytesSource` (as the Blu-ray
//! source does for the longest HDMV title) is that VOBs are MPEG-2
//! Program Streams with DVD-specific nav-pack overlays: the
//! pipeline needs to know it's a DVD before consuming bytes so it
//! can route through a DVD-aware demuxer in Phase 2. For now the
//! source driver makes the disc *discoverable* but the actual
//! playback bridge is the Phase 2 deliverable.

use std::path::{Path, PathBuf};

use crate::disc::DvdDisc;
use crate::error::{Error, Result};

/// Parsed `dvd://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DvdUri {
    /// `dvd://` — auto-detect (Phase 2).
    AutoDetect,
    /// `dvd:///abs/path` — explicit file or block-device path.
    Path(PathBuf),
}

/// Parse a `dvd://...` URI string.
pub fn parse_dvd_uri(uri: &str) -> Result<DvdUri> {
    let rest = uri
        .strip_prefix("dvd://")
        .or_else(|| uri.strip_prefix("dvd:"))
        .ok_or(Error::NotDvdVideo("not a dvd:// URI"))?;
    if rest.is_empty() || rest == "/" {
        return Ok(DvdUri::AutoDetect);
    }
    let path = if let Some(p) = rest.strip_prefix('/') {
        if p.starts_with('/') {
            PathBuf::from(p)
        } else {
            PathBuf::from(format!("/{p}"))
        }
    } else {
        PathBuf::from(rest)
    };
    Ok(DvdUri::Path(path))
}

/// Wrapper carrying the disc enumeration + the open file handle for
/// byte-range reads. Phase 2 will add an `open_vob_reader` helper.
#[derive(Debug)]
pub struct DvdDiscSource {
    pub disc: DvdDisc,
    path: PathBuf,
}

impl DvdDiscSource {
    /// Open a DVD-Video disc from a file or block-device path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let disc = DvdDisc::open(&path)?;
        Ok(Self { disc, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// oxideav framework integration removed — not needed for tonepoet.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auto_detect() {
        assert_eq!(parse_dvd_uri("dvd://").unwrap(), DvdUri::AutoDetect);
        assert_eq!(parse_dvd_uri("dvd:").unwrap(), DvdUri::AutoDetect);
        assert_eq!(parse_dvd_uri("dvd:///").unwrap(), DvdUri::AutoDetect);
    }

    #[test]
    fn parse_absolute_path() {
        assert_eq!(
            parse_dvd_uri("dvd:///tmp/disc.iso").unwrap(),
            DvdUri::Path(PathBuf::from("/tmp/disc.iso"))
        );
        assert_eq!(
            parse_dvd_uri("dvd:///dev/sr0").unwrap(),
            DvdUri::Path(PathBuf::from("/dev/sr0"))
        );
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert!(parse_dvd_uri("file:///x").is_err());
        assert!(parse_dvd_uri("http://example/").is_err());
    }
}

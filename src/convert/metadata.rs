use anyhow::{Context, Result};
use metaflac::Tag;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlacMetadata {
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub date: Option<String>,
    pub year: Option<String>,
    pub track_number: Option<u32>,
    pub total_tracks: Option<u32>,
    pub disc_number: Option<u32>,
    pub total_discs: Option<u32>,
    pub comment: Option<String>,
    pub genre: Option<String>,
}

impl FlacMetadata {
    pub fn new() -> Self {
        Self {
            artist: None,
            album_artist: None,
            album: None,
            title: None,
            date: None,
            year: None,
            track_number: None,
            total_tracks: None,
            disc_number: None,
            total_discs: None,
            comment: None,
            genre: None,
        }
    }

    pub fn get_display_artist(&self) -> Option<&String> {
        self.album_artist.as_ref().or(self.artist.as_ref())
    }
}

pub fn extract_metadata_from_flac(file_path: &Path) -> Result<FlacMetadata> {
    let tag = Tag::read_from_path(file_path)
        .with_context(|| format!("Failed to read FLAC metadata from {:?}", file_path))?;

    let mut metadata = FlacMetadata::new();

    if let Some(vorbis) = tag.vorbis_comments() {
        // artist() returns Option<&Vec<String>>, so we need the first element
        metadata.artist = vorbis
            .artist()
            .and_then(|v| v.first())
            .map(|s| s.to_string());

        // album_artist() returns Option<&Vec<String>>
        metadata.album_artist = vorbis
            .album_artist()
            .and_then(|v| v.first())
            .map(|s| s.to_string());

        metadata.album = vorbis
            .album()
            .and_then(|v| v.first())
            .map(|s| s.to_string());

        metadata.title = vorbis
            .title()
            .and_then(|v| v.first())
            .map(|s| s.to_string());

        // get() returns Option<&Vec<String>>
        if let Some(date_vec) = vorbis.get("DATE") {
            if let Some(date) = date_vec.first() {
                metadata.date = Some(date.to_string());
                if date.len() >= 4 {
                    metadata.year = Some(date[..4].to_string());
                }
            }
        }

        if metadata.year.is_none() {
            if let Some(year_vec) = vorbis.get("YEAR") {
                if let Some(year) = year_vec.first() {
                    metadata.year = Some(year.to_string());
                }
            }
        }

        metadata.track_number = vorbis.track();
        metadata.total_tracks = vorbis.total_tracks();

        if let Some(disc_vec) = vorbis.get("DISCNUMBER") {
            if let Some(disc) = disc_vec.first() {
                if let Ok(disc_num) = disc.parse::<u32>() {
                    metadata.disc_number = Some(disc_num);
                }
            }
        }

        if let Some(total_vec) = vorbis.get("TOTALDISCS") {
            if let Some(total) = total_vec.first() {
                if let Ok(total_discs) = total.parse::<u32>() {
                    metadata.total_discs = Some(total_discs);
                }
            }
        }

        // comments are stored in COMMENT field
        if let Some(comment_vec) = vorbis.get("COMMENT") {
            metadata.comment = comment_vec.first().map(|s| s.to_string());
        }

        metadata.genre = vorbis
            .genre()
            .and_then(|v| v.first())
            .map(|s| s.to_string());
    }

    Ok(metadata)
}

pub fn extract_year_from_flac_files(files: &[impl AsRef<Path>]) -> Option<String> {
    for file in files {
        if let Ok(metadata) = extract_metadata_from_flac(file.as_ref()) {
            if let Some(year) = metadata.year {
                return Some(year);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_creation() {
        let metadata = FlacMetadata::new();
        assert!(metadata.artist.is_none());
        assert!(metadata.album.is_none());
        assert!(metadata.year.is_none());
    }

    #[test]
    fn test_display_artist() {
        let mut metadata = FlacMetadata::new();
        metadata.artist = Some("Artist".to_string());
        assert_eq!(metadata.get_display_artist(), Some(&"Artist".to_string()));

        metadata.album_artist = Some("Album Artist".to_string());
        assert_eq!(
            metadata.get_display_artist(),
            Some(&"Album Artist".to_string())
        );
    }
}

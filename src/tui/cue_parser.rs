//! Minimal CUE sheet parser for the bulk rename wizard.
//!
//! Extracts track number, title, performer, and file reference from a
//! CUE sheet. Handles both single-image (one FILE + many TRACKs) and
//! track-by-track (one FILE per TRACK) layouts.

use std::path::Path;

/// A parsed CUE sheet.
#[derive(Debug, Clone, Default)]
pub struct CueSheet {
    /// Album-level title (from a TITLE before the first TRACK).
    pub title: Option<String>,
    /// Album-level performer.
    pub performer: Option<String>,
    /// Tracks in order.
    pub tracks: Vec<CueTrack>,
}

/// A single track from a CUE sheet.
#[derive(Debug, Clone, Default)]
pub struct CueTrack {
    /// Track number (1-based).
    pub number: u32,
    /// Track title from TITLE inside the TRACK block.
    pub title: Option<String>,
    /// Track performer (falls back to album performer if absent).
    pub performer: Option<String>,
    /// FILE reference associated with this track. For single-image CUEs,
    /// all tracks share the same file; for track-by-track, each has its own.
    pub file: Option<String>,
}

/// Parse a CUE sheet from a file path.
pub fn parse_cue_file(path: &Path) -> Result<CueSheet, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read CUE file: {}", e))?;
    Ok(parse_cue(&content))
}

/// Parse CUE sheet content from a string.
pub fn parse_cue(content: &str) -> CueSheet {
    let mut sheet = CueSheet::default();

    // State: are we inside a TRACK block?
    let mut current_track: Option<CueTrack> = None;
    // The most recent FILE line (used for both single-image and per-track).
    let mut current_file: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        // FILE "filename.ext" WAVE
        if let Some(file) = parse_file_line(trimmed) {
            current_file = Some(file);
            // If we're inside a track that doesn't have a file yet,
            // associate it. Otherwise it'll be picked up by the next TRACK.
            if let Some(ref mut track) = current_track {
                if track.file.is_none() {
                    track.file = current_file.clone();
                }
            }
            continue;
        }

        // TRACK NN AUDIO
        if let Some(num) = parse_track_line(trimmed) {
            // Commit the previous track (if any).
            if let Some(track) = current_track.take() {
                sheet.tracks.push(track);
            }
            current_track = Some(CueTrack {
                number: num,
                file: current_file.clone(),
                ..Default::default()
            });
            continue;
        }

        // TITLE "..."
        if let Some(title) = parse_quoted_field(trimmed, "TITLE") {
            if let Some(ref mut track) = current_track {
                track.title = Some(title);
            } else {
                sheet.title = Some(title);
            }
            continue;
        }

        // PERFORMER "..."
        if let Some(performer) = parse_quoted_field(trimmed, "PERFORMER") {
            if let Some(ref mut track) = current_track {
                track.performer = Some(performer);
            } else {
                sheet.performer = Some(performer);
            }
            continue;
        }

        // Other lines (REM, INDEX, FLAGS, etc.) are ignored.
    }

    // Commit the last track.
    if let Some(track) = current_track {
        sheet.tracks.push(track);
    }

    // For tracks without an explicit performer, inherit album performer.
    if let Some(ref album_performer) = sheet.performer {
        for track in &mut sheet.tracks {
            if track.performer.is_none() {
                track.performer = Some(album_performer.clone());
            }
        }
    }

    sheet
}

/// Parse a `FILE "filename" WAVE` line, returning the filename.
fn parse_file_line(line: &str) -> Option<String> {
    let line = line.strip_prefix("FILE")?;
    let line = line.trim_start();
    extract_quoted(line)
}

/// Parse a `TRACK NN AUDIO` line, returning the track number.
fn parse_track_line(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("TRACK")?.trim_start();
    let num_end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..num_end].parse().ok()
}

/// Parse a line like `TITLE "Some Title"` or `PERFORMER "Name"`.
fn parse_quoted_field(line: &str, keyword: &str) -> Option<String> {
    let rest = line.strip_prefix(keyword)?.trim_start();
    extract_quoted(rest)
}

/// Extract a double-quoted string from the start of `s`.
fn extract_quoted(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_image_cue() {
        let cue = r#"
PERFORMER "Miles Davis"
TITLE "Kind of Blue"
FILE "album.wav" WAVE
  TRACK 01 AUDIO
    TITLE "So What"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Freddie Freeloader"
    INDEX 01 09:22:00
  TRACK 03 AUDIO
    TITLE "Blue in Green"
    PERFORMER "Bill Evans"
    INDEX 01 19:14:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.title.as_deref(), Some("Kind of Blue"));
        assert_eq!(sheet.performer.as_deref(), Some("Miles Davis"));
        assert_eq!(sheet.tracks.len(), 3);

        assert_eq!(sheet.tracks[0].number, 1);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("So What"));
        assert_eq!(sheet.tracks[0].performer.as_deref(), Some("Miles Davis")); // inherited
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("album.wav"));

        assert_eq!(sheet.tracks[2].number, 3);
        assert_eq!(sheet.tracks[2].title.as_deref(), Some("Blue in Green"));
        assert_eq!(sheet.tracks[2].performer.as_deref(), Some("Bill Evans")); // overridden
    }

    #[test]
    fn track_by_track_cue() {
        let cue = r#"
PERFORMER "Artist"
TITLE "Album"
FILE "01 - First.flac" WAVE
  TRACK 01 AUDIO
    TITLE "First Song"
    INDEX 01 00:00:00
FILE "02 - Second.flac" WAVE
  TRACK 02 AUDIO
    TITLE "Second Song"
    INDEX 01 00:00:00
"#;
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file.as_deref(), Some("01 - First.flac"));
        assert_eq!(sheet.tracks[1].file.as_deref(), Some("02 - Second.flac"));
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("First Song"));
        assert_eq!(sheet.tracks[1].title.as_deref(), Some("Second Song"));
    }

    #[test]
    fn minimal_cue() {
        let cue = "TRACK 01 AUDIO\n  TITLE \"Only Track\"\n";
        let sheet = parse_cue(cue);
        assert_eq!(sheet.tracks.len(), 1);
        assert_eq!(sheet.tracks[0].title.as_deref(), Some("Only Track"));
        assert!(sheet.performer.is_none());
    }

    #[test]
    fn empty_cue() {
        let sheet = parse_cue("");
        assert!(sheet.tracks.is_empty());
    }

    #[test]
    fn track_number_parsing() {
        assert_eq!(parse_track_line("TRACK 01 AUDIO"), Some(1));
        assert_eq!(parse_track_line("TRACK 12 AUDIO"), Some(12));
        assert_eq!(parse_track_line("TRACK 1 AUDIO"), Some(1));
        assert_eq!(parse_track_line("  TRACK 05 AUDIO"), None); // leading whitespace stripped by caller
    }

    #[test]
    fn quoted_extraction() {
        assert_eq!(extract_quoted("\"hello world\" extra"), Some("hello world".to_string()));
        assert_eq!(extract_quoted("no quotes"), None);
        assert_eq!(extract_quoted("\"\""), Some("".to_string()));
    }
}

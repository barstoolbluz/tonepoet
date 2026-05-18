//! Template-based filename resolution for the bulk rename wizard.
//!
//! Resolves placeholders like `%ARTIST%`, `%TRACKNN%`, `%TITLE%` against
//! a file's metadata to produce a new filename. Reusable across template
//! mode, CUE mode (for further formatting), and saved rename conventions.

use super::probe::SourceMetadata;

/// Resolve a rename template against metadata + file extension.
///
/// Supported placeholders:
/// - `%N%`       — track number, raw (e.g. `3`)
/// - `%NN%`      — track number, 0-padded 2-digit (`03`)
/// - `%NNN%`     — track number, 0-padded 3-digit (`003`)
/// - `%TITLE%`   — title tag (fallback: original filename stem)
/// - `%ARTIST%`  — artist tag (fallback: `"Unknown Artist"`)
/// - `%ALBUM%`   — album tag (fallback: `"Unknown Album"`)
/// - `%YEAR%`    — year tag (fallback: empty string, placeholder removed)
/// - `%GENRE%`   — genre tag (fallback: empty string)
/// - `%CATALOG%` — catalog number tag (fallback: empty string)
/// - `%EXT%`     — original file extension without dot (e.g. `flac`)
///
/// Unrecognised `%FOO%` tokens are left as-is (pass-through).
/// Empty fallbacks: when a tag is absent and the fallback is `""`, the
/// placeholder AND any immediately-adjacent ` - ` or ` ()` separators
/// are NOT auto-cleaned — the user should edit per-line in the wizard
/// if needed. Keeping the literal output makes the preview honest.
pub fn resolve_template(
    template: &str,
    meta: &SourceMetadata,
    original_stem: &str,
    extension: &str,
) -> String {
    let mut result = template.to_string();

    // Track number variants
    if let Some(n) = meta.track_number {
        result = result.replace("%N%", &n.to_string());
        result = result.replace("%NN%", &format!("{:02}", n));
        result = result.replace("%NNN%", &format!("{:03}", n));
    } else {
        result = result.replace("%N%", "");
        result = result.replace("%NN%", "");
        result = result.replace("%NNN%", "");
    }

    // Text fields
    result = result.replace("%TITLE%", meta.title.as_deref().unwrap_or(original_stem));
    result = result.replace(
        "%ARTIST%",
        meta.artist.as_deref().unwrap_or("Unknown Artist"),
    );
    result = result.replace("%ALBUM%", meta.album.as_deref().unwrap_or("Unknown Album"));
    result = result.replace("%YEAR%", meta.year.as_deref().unwrap_or(""));
    result = result.replace("%GENRE%", meta.genre.as_deref().unwrap_or(""));
    result = result.replace("%CATALOG%", meta.catalog_number.as_deref().unwrap_or(""));
    result = result.replace("%EXT%", extension);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        year: Option<&str>,
        track: Option<u32>,
    ) -> SourceMetadata {
        SourceMetadata {
            title: title.map(|s| s.to_string()),
            artist: artist.map(|s| s.to_string()),
            album: album.map(|s| s.to_string()),
            year: year.map(|s| s.to_string()),
            track_number: track,
            ..SourceMetadata::default()
        }
    }

    #[test]
    fn basic_template() {
        let meta = meta_with(
            Some("Kind of Blue"),
            Some("Miles Davis"),
            None,
            Some("1959"),
            Some(3),
        );
        let result = resolve_template("%NN% - %TITLE% (%YEAR%)", &meta, "original", "flac");
        assert_eq!(result, "03 - Kind of Blue (1959)");
    }

    #[test]
    fn missing_track_number() {
        let meta = meta_with(Some("Song"), None, None, None, None);
        let result = resolve_template("%NN% - %TITLE%", &meta, "original", "flac");
        assert_eq!(result, " - Song");
    }

    #[test]
    fn title_fallback_to_stem() {
        let meta = meta_with(None, None, None, None, Some(1));
        let result = resolve_template("%NN% - %TITLE%", &meta, "my_song", "flac");
        assert_eq!(result, "01 - my_song");
    }

    #[test]
    fn artist_fallback() {
        let meta = meta_with(Some("Song"), None, None, None, Some(1));
        let result = resolve_template("%ARTIST% - %TITLE%", &meta, "x", "flac");
        assert_eq!(result, "Unknown Artist - Song");
    }

    #[test]
    fn extension_placeholder() {
        let meta = meta_with(Some("Song"), None, None, None, Some(1));
        let result = resolve_template("%NN% - %TITLE%.%EXT%", &meta, "x", "opus");
        assert_eq!(result, "01 - Song.opus");
    }

    #[test]
    fn three_digit_track() {
        let meta = meta_with(Some("Track"), None, None, None, Some(7));
        let result = resolve_template("%NNN% %TITLE%", &meta, "x", "flac");
        assert_eq!(result, "007 Track");
    }

    #[test]
    fn unrecognised_placeholder_passthrough() {
        let meta = meta_with(Some("Song"), None, None, None, None);
        let result = resolve_template("%FOO% - %TITLE%", &meta, "x", "flac");
        assert_eq!(result, "%FOO% - Song");
    }

    #[test]
    fn catalog_number() {
        let mut meta = meta_with(Some("Song"), None, None, None, Some(1));
        meta.catalog_number = Some("ABC-1234".to_string());
        let result = resolve_template("%CATALOG% %NN% %TITLE%", &meta, "x", "flac");
        assert_eq!(result, "ABC-1234 01 Song");
    }
}

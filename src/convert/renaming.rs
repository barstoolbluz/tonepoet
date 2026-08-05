use anyhow::{bail, Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::labels::detect_pressing_info;
use super::metadata::{extract_metadata_from_flac, extract_year_from_flac_files};

lazy_static! {
    static ref NON_CAPITALIZED_WORDS: Vec<&'static str> = vec![
        "a", "an", "the", "and", "but", "or", "for", "nor", "on", "at",
        "to", "from", "by", "of", "as", "about", "in", "up", "with"
    ];

    // Special cases that should preserve their capitalization
    static ref SPECIAL_CASES: HashMap<String, &'static str> = {
        let mut m = HashMap::new();
        // Band names and acronyms
        m.insert("ac/dc".to_string(), "AC/DC");
        m.insert("acdc".to_string(), "AC/DC");
        m.insert("ac-dc".to_string(), "AC/DC");
        m.insert("esg".to_string(), "ESG");
        m.insert("rem".to_string(), "R.E.M.");
        m.insert("r.e.m.".to_string(), "R.E.M.");
        m.insert("csny".to_string(), "CSNY");
        m.insert("elo".to_string(), "ELO");
        m.insert("abba".to_string(), "ABBA");
        m.insert("inxs".to_string(), "INXS");
        m.insert("nwa".to_string(), "N.W.A");
        m.insert("n.w.a".to_string(), "N.W.A");
        m.insert("omg".to_string(), "OMG");
        m.insert("uk".to_string(), "UK");
        m.insert("usa".to_string(), "USA");
        m.insert("ussr".to_string(), "USSR");
        m.insert("nyc".to_string(), "NYC");
        m.insert("la".to_string(), "LA");
        m.insert("dj".to_string(), "DJ");
        m.insert("mc".to_string(), "MC");
        m.insert("mg".to_string(), "MG"); // Booker T & the MG's
        m.insert("tv".to_string(), "TV");
        m.insert("mtv".to_string(), "MTV");
        m.insert("bbc".to_string(), "BBC");
        m.insert("zz".to_string(), "ZZ");  // for ZZ Top
        m.insert("xrcd".to_string(), "XRCD");
        m.insert("xrcd2".to_string(), "XRCD2");
        m.insert("xrcd24".to_string(), "XRCD24");
        m.insert("jp".to_string(), "JP");
        m.insert("lp".to_string(), "LP");

        // Roman numerals
        m.insert("ii".to_string(), "II");
        m.insert("iii".to_string(), "III");
        m.insert("iv".to_string(), "IV");
        m.insert("v".to_string(), "V");
        m.insert("vi".to_string(), "VI");
        m.insert("vii".to_string(), "VII");
        m.insert("viii".to_string(), "VIII");
        m.insert("ix".to_string(), "IX");
        m.insert("x".to_string(), "X");
        m.insert("xi".to_string(), "XI");
        m.insert("xii".to_string(), "XII");
        m.insert("xiii".to_string(), "XIII");
        m.insert("xiv".to_string(), "XIV");
        m.insert("xv".to_string(), "XV");

        m
    };

    // Pattern to detect likely acronyms (all caps, 2-5 letters)
    static ref ACRONYM_PATTERN: Regex = Regex::new(r"^[A-Z]{2,5}$").unwrap();

    // Pattern to detect roman numerals
    static ref ROMAN_NUMERAL_PATTERN: Regex = Regex::new(r"^M{0,4}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3})$").unwrap();
}

pub fn apply_folder_renaming(
    extract_path: &Path,
    audio_files: &[impl AsRef<Path>],
    custom_name: Option<String>,
    target_format: Option<&super::formats::AudioFormat>,
) -> Result<PathBuf> {
    // If custom name is provided, use it directly
    if let Some(name) = custom_name {
        let new_path = extract_path.join(&name);
        return Ok(new_path);
    }

    // Get the current folder name
    let current_folder = extract_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Failed to get folder name")?;

    // Extract metadata from FLAC files
    let year = extract_year_from_flac_files(audio_files).unwrap_or_else(|| "Unknown".to_string());

    // Get artist and album from first FLAC file
    let (artist, album) = if !audio_files.is_empty() {
        if let Ok(metadata) = extract_metadata_from_flac(audio_files[0].as_ref()) {
            let artist = metadata
                .get_display_artist()
                .cloned()
                .unwrap_or_else(|| "Unknown Artist".to_string());
            let mut album = metadata
                .album
                .unwrap_or_else(|| "Unknown Album".to_string());

            // Strip pressing info from album if present
            // Album might be: "Album Name (Pressing Info) [Uploader]"
            // We want just: "Album Name"
            if let Some(pos) = album.find(" (") {
                album = album[..pos].to_string();
            }

            (artist, album)
        } else {
            extract_artist_album_from_folder_name(current_folder)
        }
    } else {
        extract_artist_album_from_folder_name(current_folder)
    };

    // Detect pressing information
    let pressing_info = detect_pressing_info(current_folder, Some(&year));

    // Extract uploader from folder name (usually in square brackets at the end)
    let uploader = extract_uploader_from_folder_name(current_folder);

    // Build the new folder name
    // Apply title case to artist and album names
    let artist_cased = capitalize_section(&artist);
    let album_cased = capitalize_section(&album);

    // Handle filesystem-unsafe characters
    // AC/DC becomes ACDC, other slashes become dashes
    let artist_safe = sanitize_for_filesystem(&artist_cased);
    let album_safe = sanitize_for_filesystem(&album_cased);

    // Get format name for folder - use target format if provided, otherwise default to FLAC
    let format_name = if let Some(format) = target_format {
        format.name()
    } else {
        "FLAC" // Default for backward compatibility
    };

    let new_folder_name = format!(
        "{} - {} ({}) [{}] {{{}}} [{}]",
        artist_safe, album_safe, year, format_name, pressing_info.pressing_info, uploader
    );

    // Create the new path
    let parent = extract_path
        .parent()
        .context("Failed to get parent directory")?;
    let new_path = parent.join(&new_folder_name);

    // Check if renaming is needed
    if extract_path != new_path {
        // Check for conflicts
        if new_path.exists() {
            // Append timestamp to make unique
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let new_folder_name = format!(
                "{} - {} ({}) [FLAC] {{{}}} [{} {}]",
                artist_safe, album_safe, year, pressing_info.pressing_info, uploader, timestamp
            );
            let new_path = parent.join(&new_folder_name);

            fs::rename(extract_path, &new_path).with_context(|| {
                format!(
                    "Failed to rename folder from {:?} to {:?}",
                    extract_path, new_path
                )
            })?;

            Ok(new_path)
        } else {
            fs::rename(extract_path, &new_path).with_context(|| {
                format!(
                    "Failed to rename folder from {:?} to {:?}",
                    extract_path, new_path
                )
            })?;

            Ok(new_path)
        }
    } else {
        Ok(extract_path.to_path_buf())
    }
}

pub fn sanitize_for_filesystem(name: &str) -> String {
    // Special case for AC/DC - remove the slash
    if name == "AC/DC" {
        return "ACDC".to_string();
    }

    // Replace other problematic characters
    name.chars()
        .map(|c| match c {
            '/' => '-',  // Other slashes become dashes
            '\\' => '-', // Backslashes become dashes
            ':' => '-',  // Colons become dashes
            '*' => '_',  // Asterisks become underscores
            '?' => '_',  // Question marks become underscores
            '"' => '\'', // Double quotes become single quotes
            '<' => '(',  // Less than becomes open paren
            '>' => ')',  // Greater than becomes close paren
            '|' => '-',  // Pipe becomes dash
            c => c,
        })
        .collect()
}

fn extract_artist_album_from_folder_name(folder_name: &str) -> (String, String) {
    // Try to match pattern: Artist - Album (pressing info) [source]
    let full_match = Regex::new(r"^(.+?) - (.+?) \(.*\)").unwrap();
    if let Some(caps) = full_match.captures(folder_name) {
        let mut artist = caps.get(1).unwrap().as_str().to_string();
        let mut album = caps.get(2).unwrap().as_str().to_string();

        // Convert ACDC back to AC/DC for display
        if artist == "ACDC" {
            artist = "AC/DC".to_string();
        }
        if album.contains("ACDC") {
            album = album.replace("ACDC", "AC/DC");
        }

        return (artist, album);
    }

    // Try to match pattern: Artist - Album [source]
    let partial_match = Regex::new(r"^(.+?) - (.+?) \[").unwrap();
    if let Some(caps) = partial_match.captures(folder_name) {
        let mut artist = caps.get(1).unwrap().as_str().to_string();
        let mut album = caps.get(2).unwrap().as_str().to_string();

        // Convert ACDC back to AC/DC for display
        if artist == "ACDC" {
            artist = "AC/DC".to_string();
        }
        if album.contains("ACDC") {
            album = album.replace("ACDC", "AC/DC");
        }

        return (artist, album);
    }

    // Try simple pattern: Artist - Album
    if let Some(pos) = folder_name.find(" - ") {
        let mut artist = folder_name[..pos].to_string();
        let album = folder_name[pos + 3..].to_string();

        // Convert ACDC back to AC/DC for display
        if artist == "ACDC" {
            artist = "AC/DC".to_string();
        }

        return (artist, album);
    }

    ("Unknown Artist".to_string(), folder_name.to_string())
}

fn extract_uploader_from_folder_name(folder_name: &str) -> String {
    // Look for content in square brackets at the end
    let re = Regex::new(r"\[([^\]]+)\]$").unwrap();
    if let Some(caps) = re.captures(folder_name) {
        return caps.get(1).unwrap().as_str().to_string();
    }

    // Common uploaders
    if folder_name.contains("PBThal") {
        return "PBThal".to_string();
    }

    // Default to PBThal since these are all PBThal archives
    "PBThal".to_string()
}

pub fn rename_audio_files(folder_path: &Path) -> Result<Vec<PathBuf>> {
    let mut renamed_files = Vec::new();

    // Supported audio extensions
    let audio_extensions = ["flac", "mp3", "opus", "ogg", "m4a", "aac", "wav", "wv"];

    log::info!("rename_audio_files called for folder: {:?}", folder_path);
    log::info!("Attempting to read directory...");

    let entries = match fs::read_dir(folder_path) {
        Ok(e) => {
            log::info!("Successfully opened directory for reading");
            e
        }
        Err(e) => {
            log::error!("Failed to read directory {:?}: {}", folder_path, e);
            return Err(e.into());
        }
    };

    let mut entry_count = 0;
    // Process all audio files in the folder
    for entry_result in entries {
        entry_count += 1;
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                log::error!("Error reading entry #{}: {}", entry_count, e);
                continue;
            }
        };

        let path = entry.path();
        log::info!("Entry #{}: {:?}", entry_count, path);

        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            log::info!("  Extension: {}", ext);
            let is_file = path.is_file();
            let ext_matches = audio_extensions.contains(&ext);
            log::info!("  Is file: {}, Extension matches: {}", is_file, ext_matches);

            if is_file && ext_matches {
                log::info!("Processing audio file: {:?}", path);
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .context("Failed to get file name")?;

                // Try to match pattern: XX - Track Title.ext
                // Use dynamic regex that captures the extension
                let pattern = format!(r"^(\d+) - (.+)\.{}$", regex::escape(ext));
                let track_match = Regex::new(&pattern).unwrap();

                if let Some(caps) = track_match.captures(file_name) {
                    let track_number = caps.get(1).unwrap().as_str();
                    let title = caps.get(2).unwrap().as_str();

                    log::info!(
                        "Matched pattern for: {}, track: {}, title: {}",
                        file_name,
                        track_number,
                        title
                    );

                    // Capitalize the title properly
                    let new_title = capitalize_title(title);

                    // Pad track number to 2 digits
                    let padded_track = format!("{:02}", track_number.parse::<u32>().unwrap_or(0));

                    let new_file_name = format!("{} - {}.{}", padded_track, new_title, ext);

                    if new_file_name != file_name {
                        log::info!("Renaming: {} -> {}", file_name, new_file_name);
                        let new_path = folder_path.join(&new_file_name);
                        fs::rename(&path, &new_path).with_context(|| {
                            format!("Failed to rename file from {:?} to {:?}", path, new_path)
                        })?;
                        renamed_files.push(new_path);
                    } else {
                        log::info!("No rename needed: {}", file_name);
                        renamed_files.push(path);
                    }
                } else if ext == "flac" {
                    // For FLAC files, try to extract from metadata as fallback
                    if let Ok(metadata) = extract_metadata_from_flac(&path) {
                        if let (Some(track), Some(ref title)) =
                            (metadata.track_number, metadata.title.as_ref())
                        {
                            let padded_track = format!("{:02}", track);
                            let new_title = capitalize_title(title);
                            let new_file_name = format!("{} - {}.{}", padded_track, new_title, ext);

                            let new_path = folder_path.join(&new_file_name);
                            if new_path != path {
                                fs::rename(&path, &new_path).with_context(|| {
                                    format!(
                                        "Failed to rename file from {:?} to {:?}",
                                        path, new_path
                                    )
                                })?;
                                renamed_files.push(new_path);
                            } else {
                                renamed_files.push(path);
                            }
                        } else {
                            renamed_files.push(path);
                        }
                    } else {
                        renamed_files.push(path);
                    }
                } else {
                    // For non-FLAC files that don't match pattern, keep as-is
                    log::info!("File doesn't match pattern, keeping as-is: {:?}", path);
                    renamed_files.push(path);
                }
            } else {
                log::info!("  Skipping non-audio file or directory");
            }
        } else {
            log::info!("  No extension found");
        }
    }

    log::info!("Processed {} total directory entries", entry_count);
    log::info!(
        "rename_audio_files completed: {} files processed",
        renamed_files.len()
    );
    Ok(renamed_files)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTransform {
    Title,
    Upper,
    Lower,
}

/// Apply a Unicode-preserving case-only transform. Title case delegates to the
/// same punctuation-aware naming logic used by metadata fix-caps; upper/lower
/// use Rust's full Unicode mappings and therefore never transliterate or drop
/// non-ASCII characters.
pub fn transform_case(value: &str, transform: CaseTransform) -> String {
    match transform {
        CaseTransform::Title => capitalize_title(value),
        CaseTransform::Upper => value.to_uppercase(),
        CaseTransform::Lower => value.to_lowercase(),
    }
}

pub fn capitalize_title(title: &str) -> String {
    log::info!("capitalize_title input: '{}'", title);

    // Parenthetical sections are independent title-case sections, but the
    // original punctuation and spacing are retained byte-for-byte. A case-only
    // transform must never silently insert, remove, or reorder tokens.
    let re = Regex::new(r"(\([^)]*\))").expect("parenthetical regex is valid");
    let mut result = String::with_capacity(title.len());
    let mut last_end = 0;
    for cap in re.find_iter(title) {
        result.push_str(&capitalize_section(&title[last_end..cap.start()]));
        let paren_content = cap.as_str();
        if paren_content.len() > 2 {
            // Pass the delimiters through with the content: the section
            // normalizer's designator carve-outs (e.g. "(US") depend on
            // seeing the opening punctuation on the first token.
            result.push_str(&capitalize_section(paren_content));
        } else {
            result.push_str(paren_content);
        }
        last_end = cap.end();
    }
    result.push_str(&capitalize_section(&title[last_end..]));

    log::info!("capitalize_title output: '{}'", result);
    result
}

pub fn capitalize_section(section: &str) -> String {
    // Normalize all-caps prose token-by-token so normal words such as THE and
    // BAND do not masquerade as acronyms. Retain an explicit uppercase US
    // parenthetical designator from the original token; lowercasing the whole
    // section first would make `(US ...)` indistinguishable from the pronoun
    // "us" and either corrupt the designator or over-capitalize normal prose.
    let normalize_all_caps = section.len() > 1
        && section
            .chars()
            .all(|ch| !ch.is_alphabetic() || ch.is_uppercase());
    let mut ranges = Vec::new();
    let mut token_start = None;
    for (index, ch) in section.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                ranges.push(start..index);
            }
        } else if token_start.is_none() {
            token_start = Some(index);
        }
    }
    if let Some(start) = token_start {
        ranges.push(start..section.len());
    }

    let mut result = String::with_capacity(section.len());
    let mut copied_through = 0;
    for (index, range) in ranges.iter().enumerate() {
        result.push_str(&section[copied_through..range.start]);
        let original_word = &section[range.clone()];
        let normalized_word;
        let word = if normalize_all_caps
            && !(word_has_opening_prefix(original_word) && word_core(original_word) == "US")
        {
            normalized_word = lowercase_word_core(original_word);
            normalized_word.as_str()
        } else {
            original_word
        };
        let after_ampersand = (index > 0
            && section[ranges[index - 1].clone()]
                .chars()
                .any(|ch| ch == '&'))
            || word_affixes(word).0.chars().any(|ch| ch == '&');
        let first_in_delimited_section = word_has_opening_prefix(word);
        let core_lower = word_core(word).to_lowercase();
        let transformed = if index == 0
            || index + 1 == ranges.len()
            || after_ampersand
            || first_in_delimited_section
            || !NON_CAPITALIZED_WORDS.contains(&core_lower.as_str())
        {
            capitalize_word(word)
        } else {
            lowercase_word_core(word)
        };
        result.push_str(&transformed);
        copied_through = range.end;
    }
    result.push_str(&section[copied_through..]);
    result
}

fn word_affixes(word: &str) -> (&str, &str, &str) {
    let Some(core_start) = word
        .char_indices()
        .find_map(|(index, ch)| ch.is_alphanumeric().then_some(index))
    else {
        return (word, "", "");
    };
    let core_end = word
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_alphanumeric().then_some(index + ch.len_utf8()))
        .unwrap_or(core_start);
    (&word[..core_start], &word[core_start..core_end], &word[core_end..])
}

fn word_core(word: &str) -> &str {
    word_affixes(word).1
}

fn word_has_opening_prefix(word: &str) -> bool {
    let prefix = word_affixes(word).0;
    prefix
        .chars()
        .any(|ch| matches!(ch, '(' | '[' | '{' | '"' | '\'' | '“' | '‘'))
}

fn lowercase_word_core(word: &str) -> String {
    let (prefix, core, suffix) = word_affixes(word);
    if core.is_empty() {
        return word.to_string();
    }
    format!("{prefix}{}{suffix}", core.to_lowercase())
}

fn capitalize_word(word: &str) -> String {
    let (prefix, core, suffix) = word_affixes(word);
    if core.is_empty() {
        return word.to_string();
    }
    format!("{prefix}{}{suffix}", capitalize_word_core(core))
}

fn capitalize_word_core(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }

    // Ampersands are punctuation, not token separators. Preserve them exactly
    // while treating every following component as the start of a name/title
    // component. This covers both spaced input ("& The") and real-world
    // compact spellings ("&The" / "Kool&The") without inserting spaces.
    if let Some(pos) = word.find('&') {
        let (first, rest_with_ampersand) = word.split_at(pos);
        if let Some(after_ampersand) = rest_with_ampersand.strip_prefix('&') {
            return format!(
                "{}&{}",
                capitalize_word_core(first),
                capitalize_word_core(after_ampersand)
            );
        }
    }

    let word_lower = word.to_lowercase();
    if let Some(special) = SPECIAL_CASES.get(&word_lower) {
        return special.to_string();
    }

    let has_letter_dot_pattern = word
        .chars()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0].is_alphabetic() && pair[1] == '.');
    if has_letter_dot_pattern && word.chars().filter(|ch| ch.is_alphabetic()).count() >= 2 {
        return word.to_uppercase();
    }

    if word.chars().count() >= 2
        && word.chars().count() <= 5
        && word
            .chars()
            .all(|ch| ch.is_uppercase() || !ch.is_alphabetic())
    {
        return word.to_string();
    }

    if ROMAN_NUMERAL_PATTERN.is_match(&word.to_uppercase()) {
        return word.to_uppercase();
    }

    if let Some(pos) = word.find('\'') {
        let (first, rest) = word.split_at(pos);
        return format!("{}{}", capitalize_word_core(first), rest);
    }

    if let Some(pos) = word.find('-') {
        let (first, rest_with_hyphen) = word.split_at(pos);
        if let Some(after_hyphen) = rest_with_hyphen.strip_prefix('-') {
            return format!(
                "{}-{}",
                capitalize_word_core(first),
                capitalize_word_core(after_hyphen)
            );
        }
    }

    if let Some(pos) = word.find('/') {
        let (first, rest_with_slash) = word.split_at(pos);
        if let Some(after_slash) = rest_with_slash.strip_prefix('/') {
            return format!(
                "{}/{}",
                capitalize_word_core(first),
                capitalize_word_core(after_slash)
            );
        }
    }

    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first
            .to_uppercase()
            .chain(chars.as_str().to_lowercase().chars())
            .collect(),
    }
}

/// Set album tag for a file based on its format
fn set_album_tag(file_path: &Path, album: &str) -> Result<()> {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .context("File has no extension")?;

    match ext {
        "flac" => {
            // Use metaflac command
            let output = Command::new("metaflac")
                .arg("--remove-tag=ALBUM")
                .arg(format!("--set-tag=ALBUM={}", album))
                .arg(file_path)
                .output()
                .context("Failed to execute metaflac")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("metaflac failed: {}", stderr);
            }
            Ok(())
        }
        "wv" => {
            // Use wvtag command
            let output = Command::new("wvtag")
                .arg("-q")
                .arg("-w")
                .arg(format!("ALBUM={}", album))
                .arg(file_path)
                .output()
                .context("Failed to execute wvtag")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("wvtag failed: {}", stderr);
            }
            Ok(())
        }
        "mp3" | "m4a" | "aac" => {
            // Use FFmpeg in-place with temp file
            let temp_path = file_path.with_extension(format!("tmp.{}", ext));

            let output = Command::new("ffmpeg")
                .arg("-nostdin")
                .arg("-i")
                .arg(file_path)
                .arg("-c")
                .arg("copy")
                .arg("-metadata")
                .arg(format!("album={}", album))
                .arg("-y")
                .arg(&temp_path)
                .output()
                .context("Failed to execute ffmpeg")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = fs::remove_file(&temp_path); // Cleanup
                bail!("ffmpeg failed: {}", stderr);
            }

            // Replace original with updated file
            fs::rename(&temp_path, file_path).context("Failed to replace original file")?;
            Ok(())
        }
        "opus" => {
            // Use opustags command
            let output = Command::new("opustags")
                .arg("--delete")
                .arg("ALBUM")
                .arg("-s")
                .arg(format!("ALBUM={}", album))
                .arg("--in-place")
                .arg(file_path)
                .output()
                .context("Failed to execute opustags")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("opustags failed: {}", stderr);
            }
            Ok(())
        }
        _ => {
            // Unsupported format, skip silently
            Ok(())
        }
    }
}

/// Set title tag for a file based on its format
fn set_title_tag(file_path: &Path, title: &str) -> Result<()> {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .context("File has no extension")?;

    match ext {
        "flac" => {
            let output = Command::new("metaflac")
                .arg("--remove-tag=TITLE")
                .arg(format!("--set-tag=TITLE={}", title))
                .arg(file_path)
                .output()
                .context("Failed to execute metaflac")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("metaflac failed: {}", stderr);
            }
            Ok(())
        }
        "wv" => {
            let output = Command::new("wvtag")
                .arg("-q")
                .arg("-w")
                .arg(format!("TITLE={}", title))
                .arg(file_path)
                .output()
                .context("Failed to execute wvtag")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("wvtag failed: {}", stderr);
            }
            Ok(())
        }
        "mp3" | "m4a" | "aac" => {
            let temp_path = file_path.with_extension(format!("tmp.{}", ext));

            let output = Command::new("ffmpeg")
                .arg("-nostdin")
                .arg("-i")
                .arg(file_path)
                .arg("-c")
                .arg("copy")
                .arg("-metadata")
                .arg(format!("title={}", title))
                .arg("-y")
                .arg(&temp_path)
                .output()
                .context("Failed to execute ffmpeg")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = fs::remove_file(&temp_path);
                bail!("ffmpeg failed: {}", stderr);
            }

            fs::rename(&temp_path, file_path).context("Failed to replace original file")?;
            Ok(())
        }
        "opus" => {
            let output = Command::new("opustags")
                .arg("--delete")
                .arg("TITLE")
                .arg("-s")
                .arg(format!("TITLE={}", title))
                .arg("--in-place")
                .arg(file_path)
                .output()
                .context("Failed to execute opustags")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("opustags failed: {}", stderr);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Updates album tags based on the folder name pattern for all supported audio formats.
/// Folder should be: Artist - Album (Year) [Format] {Pressing Info 24-96} [Uploader]
/// Album tag becomes: Album (Pressing Info / 24-96) [Uploader]
pub fn update_album_tags(folder_path: &Path) -> Result<usize> {
    let mut tags_updated = 0;

    // Extract components from folder name
    let folder_name = folder_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Failed to get folder name")?;

    // Pattern: Artist - Album (Year) [FORMAT] {Pressing Info 24-96} [Uploader]
    // Make [FORMAT] flexible to match any format (FLAC, WavPack, MP3, AAC, Opus)
    let pattern =
        Regex::new(r"^(.+?) - (.+?) \((\d{4})\) \[([^\]]+)\] \{(.+?)\s+24-96\} \[(.+?)\]$")
            .unwrap();

    log::info!("🏷️ Album tagging: folder_name = '{}'", folder_name);
    log::info!(
        "🏷️ Pattern match result: {:?}",
        pattern.captures(folder_name).is_some()
    );

    if let Some(caps) = pattern.captures(folder_name) {
        log::info!(
            "🏷️ Pattern matched! Album: {}, Format: {}, Pressing: {}, Uploader: {}",
            caps.get(2).unwrap().as_str(),
            caps.get(4).unwrap().as_str(),
            caps.get(5).unwrap().as_str(),
            caps.get(6).unwrap().as_str()
        );
        let album_title = caps.get(2).unwrap().as_str();
        let _format = caps.get(4).unwrap().as_str(); // Captured but not used (for pattern matching only)
        let pressing_info = caps.get(5).unwrap().as_str();
        let uploader = caps.get(6).unwrap().as_str();

        // Apply title case to album title (matching convert.sh behavior)
        let album_title_cased = capitalize_section(album_title);

        // Format the new album tag: Album (Pressing Info / 24-96) [Uploader]
        let new_album_tag = format!(
            "{} ({} / 24-96) [{}]",
            album_title_cased, pressing_info, uploader
        );

        // Process all audio files recursively in the folder and subdirectories
        tags_updated = update_album_tags_recursive(folder_path, &new_album_tag)?;
    }

    Ok(tags_updated)
}

/// Helper function to recursively update album tags in all supported audio files
fn update_album_tags_recursive(dir: &Path, new_album_tag: &str) -> Result<usize> {
    let mut tags_updated = 0;

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Recurse into subdirectories
            tags_updated += update_album_tags_recursive(&path, new_album_tag)?;
        } else if path.is_file() {
            let ext = path.extension().and_then(|s| s.to_str());

            // Only process supported audio formats
            if matches!(
                ext,
                Some("flac") | Some("wv") | Some("mp3") | Some("m4a") | Some("aac") | Some("opus")
            ) {
                // Use helper function instead of metaflac library
                if let Err(e) = set_album_tag(&path, new_album_tag) {
                    log::warn!("Failed to set album tag for {:?}: {}", path, e);
                    // Continue processing other files
                } else {
                    tags_updated += 1;
                }
            }
        }
    }

    Ok(tags_updated)
}

/// Updates title tags based on the filenames after renaming for all supported audio formats.
/// This ensures metadata consistency with the renamed files.
///
/// This is the third step in the renaming workflow:
/// 1. Folder renaming (apply_folder_renaming)
/// 2. File renaming (rename_audio_files)
/// 3. Metadata retagging (update_title_tags) <- this function
pub fn update_title_tags(folder_path: &Path) -> Result<usize> {
    log::info!(
        "🏷️ Title tagging: folder_path = '{}'",
        folder_path.display()
    );
    update_title_tags_recursive(folder_path)
}

/// Helper function to recursively update title tags in all supported audio files
fn update_title_tags_recursive(dir: &Path) -> Result<usize> {
    let mut tags_updated = 0;

    // Process all audio files in the folder
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        log::info!(
            "🏷️ Checking entry: {} (is_file: {}, ext: {:?})",
            path.display(),
            path.is_file(),
            path.extension().and_then(|s| s.to_str())
        );

        if path.is_dir() {
            // Recurse into subdirectories
            tags_updated += update_title_tags_recursive(&path)?;
        } else if path.is_file() {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .context("Failed to get file name")?;

            // Extract title from filename pattern: XX - Title.ext
            // Match pattern for any supported audio extension
            let track_match = Regex::new(r"^\d+ - (.+)\.(flac|wv|mp3|m4a|aac|opus)$").unwrap();
            if let Some(caps) = track_match.captures(file_name) {
                let new_title = caps.get(1).unwrap().as_str();
                log::info!(
                    "🏷️ Found title in filename '{}': '{}'",
                    file_name,
                    new_title
                );

                // Use helper function instead of metaflac library
                if let Err(e) = set_title_tag(&path, new_title) {
                    log::warn!("Failed to set title tag for {:?}: {}", path, e);
                } else {
                    tags_updated += 1;
                }
            }
        }
    }

    Ok(tags_updated)
}

/// Apply all tagging operations to a folder (both album and title tags)
/// This is a convenience function that combines update_album_tags and update_title_tags
pub fn apply_all_tags(folder_path: &Path) -> Result<(usize, usize)> {
    let album_count = update_album_tags(folder_path)?;
    let title_count = update_title_tags(folder_path)?;
    Ok((album_count, title_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_artist_album() {
        let (artist, album) =
            extract_artist_album_from_folder_name("Pink Floyd - The Wall (UK) [PBThal]");
        assert_eq!(artist, "Pink Floyd");
        assert_eq!(album, "The Wall");

        let (artist, album) = extract_artist_album_from_folder_name("Led Zeppelin - IV");
        assert_eq!(artist, "Led Zeppelin");
        assert_eq!(album, "IV");
    }

    #[test]
    fn test_extract_uploader() {
        let uploader = extract_uploader_from_folder_name("Artist - Album [PBThal]");
        assert_eq!(uploader, "PBThal");

        let uploader = extract_uploader_from_folder_name("Artist - Album (Info) [Uploader123]");
        assert_eq!(uploader, "Uploader123");
    }

    #[test]
    fn test_capitalize_title() {
        assert_eq!(
            capitalize_title("the dark side of the moon"),
            "The Dark Side of the Moon"
        );
        assert_eq!(capitalize_title("don't stop me now"), "Don't Stop Me Now");
        assert_eq!(capitalize_title("rock and roll"), "Rock and Roll");
        // "with" is a preposition and should be lowercase in the middle of a title (title case rules)
        assert_eq!(
            capitalize_title("lucy in the sky with diamonds"),
            "Lucy in the Sky with Diamonds"
        );
    }

    #[test]
    fn test_capitalize_word() {
        assert_eq!(capitalize_word("hello"), "Hello");
        assert_eq!(capitalize_word("don't"), "Don't");
        assert_eq!(capitalize_word("rock-and-roll"), "Rock-And-Roll");
        assert_eq!(capitalize_word(""), "");
    }

    #[test]
    fn capitalize_after_ampersand_across_entry_points() {
        for input in ["Booker T & the MG's", "Neil Young & the Shocking Pinks"] {
            let expected = input.replace("& the", "& The");
            assert_eq!(capitalize_section(input), expected);
            assert_eq!(capitalize_title(input), expected);
        }
        for input in [
            "Kool & The Gang, Emergency, 1984",
            "Kool &The Gang, Emergency, 1984",
            "Kool&The Gang, Emergency, 1984",
        ] {
            assert_eq!(capitalize_section(input), input);
            assert_eq!(capitalize_title(input), input);
        }
        assert_eq!(
            capitalize_title("KOOL & THE GANG, EMERGENCY, 1984"),
            "Kool & The Gang, Emergency, 1984"
        );
        assert_eq!(
            capitalize_title("KOOL&THE GANG, EMERGENCY, 1984"),
            "Kool&The Gang, Emergency, 1984"
        );
        assert_eq!(
            capitalize_title("Jack and the Beanstalk"),
            "Jack and the Beanstalk"
        );
        assert_eq!(
            capitalize_title("Dark Side of the Moon"),
            "Dark Side of the Moon"
        );
        assert_eq!(
            capitalize_section("Booker T & (the MG's)"),
            "Booker T & (The MG's)"
        );
        assert_eq!(
            capitalize_section("Booker T &(the MG's)"),
            "Booker T &(The MG's)"
        );
    }

    #[test]
    fn all_caps_us_pronoun_does_not_become_a_country_designator() {
        assert_eq!(capitalize_section("TELL US WHY"), "Tell Us Why");
        assert_eq!(capitalize_title("TELL US WHY"), "Tell Us Why");
        assert_eq!(capitalize_section("(US LP)"), "(US LP)");
        assert_eq!(capitalize_title("(US LP)"), "(US LP)");
    }

    #[test]
    fn parenthetical_designators_and_prefixed_words_are_case_only() {
        let exact = "(Japan P-11356 Promo LP / 32-192)";
        assert_eq!(capitalize_title(exact), exact);
        assert_eq!(capitalize_section(exact), exact);
        assert_eq!(
            capitalize_title("(Foo Foo LP / 24-96)"),
            "(Foo Foo LP / 24-96)"
        );
        assert_eq!(
            capitalize_section("[japan] \"promo\" '(foo)'"),
            "[Japan] \"Promo\" '(Foo)'"
        );
        assert_eq!(capitalize_title("(US LP)"), "(US LP)");
        assert_eq!(capitalize_section("(JP LP)"), "(JP LP)");
    }

    #[test]
    fn capitalization_preserves_unicode_and_every_noncase_character() {
        let input = "(japan  P-11356\tpromo LP / 32-192) — blue öyster cult";
        let output = capitalize_section(input);
        let strip_case = |value: &str| {
            value
                .chars()
                .filter(|ch| !ch.is_alphabetic())
                .collect::<String>()
        };
        assert_eq!(strip_case(&output), strip_case(input));
        assert!(output.contains("Öyster"));
        assert!(output.contains("P-11356"));
        assert!(output.contains("32-192"));
        assert!(output.contains("Japan  P-11356\tPromo"));
    }

}

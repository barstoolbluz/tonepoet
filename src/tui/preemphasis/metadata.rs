//! Metadata-based pre-emphasis evidence detection.
//!
//! Checks audio file tags, CUE files, and EAC/XLD log files for
//! explicit pre-emphasis indicators. This tier is authoritative —
//! if metadata says pre-emphasis is present, we trust it.

use std::fs;
use std::path::Path;

/// Evidence source for pre-emphasis detection.
#[derive(Debug, Clone)]
pub enum PreemphasisEvidence {
    /// PRE_EMPHASIS or PRE-EMPHASIS tag found in the audio file.
    Tag,
    /// COMMENT tag mentions "pre-emphasis" or similar.
    CommentTag,
    /// FLAGS PRE found in a CUE file in the same directory.
    CueFile,
    /// Pre-emphasis mentioned in an EAC/XLD log file.
    LogFile,
}

impl PreemphasisEvidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::CommentTag => "comment tag",
            Self::CueFile => "CUE file",
            Self::LogFile => "log file",
        }
    }
}

/// Check audio file tags for pre-emphasis indicators via lofty.
/// Returns the evidence type if found.
pub fn check_tag_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;

    let tagged = lofty::read_from_path(audio_path).ok()?;
    for tag in tagged.tags() {
        // Check for explicit PRE_EMPHASIS or PRE-EMPHASIS tag.
        let pe_keys = [
            ItemKey::Unknown("PRE_EMPHASIS".to_string()),
            ItemKey::Unknown("PRE-EMPHASIS".to_string()),
            ItemKey::Unknown("PREEMPHASIS".to_string()),
            ItemKey::Unknown("pre_emphasis".to_string()),
            ItemKey::Unknown("pre-emphasis".to_string()),
            ItemKey::Unknown("preemphasis".to_string()),
        ];
        for key in &pe_keys {
            if let Some(val) = tag.get_string(key) {
                let v = val.trim().to_ascii_lowercase();
                if v == "1" || v == "yes" || v == "true" {
                    return Some(PreemphasisEvidence::Tag);
                }
            }
        }

        // Check COMMENT tag for "pre-emphasis" / "pre emphasis" mentions.
        if let Some(comment) = tag.get_string(&ItemKey::Comment) {
            let lower = comment.to_ascii_lowercase();
            if lower.contains("pre-emphasis") || lower.contains("pre emphasis")
                || lower.contains("preemphasis")
            {
                return Some(PreemphasisEvidence::CommentTag);
            }
        }
    }
    None
}

/// Check CUE and log files in the directory for pre-emphasis evidence.
/// Also checks parent directory (for disc 01/disc 02 layouts).
pub fn check_file_evidence(audio_path: &Path) -> Option<PreemphasisEvidence> {
    let dir = match audio_path.parent() {
        Some(d) => d,
        None => return None,
    };

    // Check this directory and the parent (for disc 01/02 layouts).
    for search_dir in [Some(dir), dir.parent()].iter().flatten() {
        let entries = match fs::read_dir(search_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            if ext == "cue" {
                if check_cue_file_for_preemphasis(&path) {
                    return Some(PreemphasisEvidence::CueFile);
                }
            } else if ext == "log" {
                if check_log_file_for_preemphasis(&path) {
                    return Some(PreemphasisEvidence::LogFile);
                }
            }
        }
    }
    None
}

/// Parse a CUE file looking for FLAGS PRE. Returns true if any track
/// has the pre-emphasis flag set.
fn check_cue_file_for_preemphasis(cue_path: &Path) -> bool {
    let content = match fs::read_to_string(cue_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content.lines().any(|line| {
        let upper = line.trim().to_ascii_uppercase();
        upper.contains("FLAGS") && upper.contains("PRE")
    })
}

/// Check an EAC/XLD log file for pre-emphasis mentions.
fn check_log_file_for_preemphasis(log_path: &Path) -> bool {
    let content = match fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let lower = content.to_ascii_lowercase();
    lower.contains("pre-emphasis") || lower.contains("preemphasis")
        || lower.contains("pre emphasis")
}

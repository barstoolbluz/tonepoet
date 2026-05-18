//! 7z extraction output parsing.

use std::path::Path;

use crate::convert::pipeline::progress::streaming::ProbeUpdate;

#[derive(Debug, Clone, Default)]
pub struct ArchiveProgressProbe {
    extracted_files: u32,
    total_files: Option<u32>,
}

impl ArchiveProgressProbe {
    pub fn new(total_files: Option<u32>) -> Self {
        Self {
            extracted_files: 0,
            total_files,
        }
    }

    pub fn parse_line(&mut self, line: &str) -> Option<ProbeUpdate> {
        let member = parse_extracting_member(line)?;
        self.extracted_files = self.extracted_files.saturating_add(1);
        let display_name = Path::new(member)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(member)
            .to_string();
        match self.total_files.filter(|total| *total > 0) {
            Some(total) => {
                let progress = (self.extracted_files as f32 / total as f32).clamp(0.0, 1.0);
                Some(ProbeUpdate::measured(
                    progress,
                    "archive-extract-file".to_string(),
                    format!(
                        "Extracting archive item {} of {}: {}",
                        self.extracted_files, total, display_name
                    ),
                ))
            }
            None => Some(ProbeUpdate::unknown(
                "archive-extract-file".to_string(),
                format!("Extracting {display_name}"),
            )),
        }
    }
}

pub fn parse_extracting_member(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let member = trimmed
        .strip_prefix("Extracting  ")
        .or_else(|| trimmed.strip_prefix("Extracting "))?;
    let member = member.trim();
    if member.is_empty() {
        None
    } else {
        Some(member)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_archive_member_name() {
        assert_eq!(
            parse_extracting_member("Extracting  album/01 - So What.flac"),
            Some("album/01 - So What.flac")
        );
        assert_eq!(parse_extracting_member("Everything is Ok"), None);
    }

    #[test]
    fn file_count_progress_is_measured_when_total_known() {
        let mut probe = ArchiveProgressProbe::new(Some(4));
        let update = probe
            .parse_line("Extracting  album/01 - So What.flac")
            .expect("parsed");
        assert!((update.progress() - 0.25).abs() < 0.001);
        assert!(update.message().contains("01 - So What.flac"));
    }

    #[test]
    fn unknown_progress_reports_member_when_total_missing() {
        let mut probe = ArchiveProgressProbe::new(None);
        let update = probe
            .parse_line("Extracting  album/01 - So What.flac")
            .expect("parsed");
        assert!(update.is_unknown());
        assert!(update.message().contains("01 - So What.flac"));
    }
}

// SPDX-License-Identifier: GPL-2.0-or-later
//! Corpus validation helpers for DSF/DSDIFF reader hardening.
//!
//! These helpers deliberately live in the library rather than only in tests so
//! tonepoet can run the same acceptance sweep over private SACD/DSD corpora in
//! CI or release qualification. The harness does not special-case individual
//! files: every input is opened through the same strict streaming readers and
//! returns a structured report.

use crate::dsd_file::inspect::{DsdCompression, DsdContainerFormat};
use crate::dsd_file::ops::{validate_dsd_stream, DsdSourceKind, DsdValidationFailureKind, DsdValidationMode, DsdValidationOptions, DsdValidationReport};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Options for validating a set of DSF/DFF/DST files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdCorpusValidationOptions {
    pub validation: DsdValidationOptions,
    /// When true, every corpus entry must decode/validate successfully.
    pub require_all_success: bool,
    /// When true, at least one DSDIFF/DST file must be present and fully
    /// decoded. This is useful for DST reader acceptance gates.
    pub require_dst_coverage: bool,
    /// When true, at least one DSF and one DSDIFF/DSD file must be present.
    pub require_uncompressed_file_coverage: bool,
}

impl Default for DsdCorpusValidationOptions {
    fn default() -> Self {
        Self {
            validation: DsdValidationOptions::default(),
            require_all_success: true,
            require_dst_coverage: true,
            require_uncompressed_file_coverage: false,
        }
    }
}

impl DsdCorpusValidationOptions {
    pub fn header_only() -> Self {
        Self {
            validation: DsdValidationOptions::header_only(),
            ..Self::default()
        }
    }

    pub fn stream_payloads() -> Self {
        Self {
            validation: DsdValidationOptions::stream_payloads(),
            ..Self::default()
        }
    }

    pub fn decode_dst() -> Self {
        Self::default()
    }

    pub fn allow_failures(mut self) -> Self {
        self.require_all_success = false;
        self
    }

    pub fn require_all_file_kinds(mut self) -> Self {
        self.require_uncompressed_file_coverage = true;
        self.require_dst_coverage = true;
        self
    }
}

/// One file's corpus-validation result.
#[derive(Debug, Clone)]
pub struct DsdCorpusEntryReport {
    pub path: PathBuf,
    pub report: DsdValidationReport,
}

/// Aggregate corpus-validation result.
#[derive(Debug, Clone)]
pub struct DsdCorpusValidationReport {
    pub entries: Vec<DsdCorpusEntryReport>,
    pub files_seen: u64,
    pub files_succeeded: u64,
    pub files_failed: u64,
    pub dsf_files: u64,
    pub dsdiff_dsd_files: u64,
    pub dsdiff_dst_files: u64,
    pub dst_frames_seen: u64,
    pub dstc_checked_frames: u64,
    pub dstc_missing_frames: u64,
    pub dstc_no_crc_frames: u64,
    pub dstc_present_unchecked_frames: u64,
    pub dstc_passed_frames: u64,
    pub dstc_failed_frames: u64,
    pub dstc_malformed_frames: u64,
    pub decoded_dsd_bytes: u64,
    pub encoded_dst_bytes: u64,
    pub failures: Vec<DsdCorpusAcceptanceFailure>,
}

impl DsdCorpusValidationReport {
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn compression_ratio_observed(&self) -> Option<f64> {
        if self.encoded_dst_bytes == 0 {
            None
        } else {
            Some(self.decoded_dsd_bytes as f64 / self.encoded_dst_bytes as f64)
        }
    }
}

/// Acceptance-gate failures for corpus-level DSD reader evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DsdCorpusAcceptanceFailure {
    EmptyCorpus,
    IoOpen { path: PathBuf, message: String },
    FileValidationFailed { path: PathBuf, kind: DsdValidationFailureKind, message: String },
    MissingDstCoverage,
    MissingDsfCoverage,
    MissingDsdiffDsdCoverage,
    DstCoverageNotDecoded,
}

/// Validate explicit corpus paths. Directories are intentionally not expanded
/// here; callers should decide their traversal policy and pass the exact files
/// admitted to the corpus.
pub fn validate_dsd_corpus_paths<I, P>(
    paths: I,
    options: DsdCorpusValidationOptions,
) -> DsdCorpusValidationReport
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut out = DsdCorpusValidationReport {
        entries: Vec::new(),
        files_seen: 0,
        files_succeeded: 0,
        files_failed: 0,
        dsf_files: 0,
        dsdiff_dsd_files: 0,
        dsdiff_dst_files: 0,
        dst_frames_seen: 0,
        dstc_checked_frames: 0,
        dstc_missing_frames: 0,
        dstc_no_crc_frames: 0,
        dstc_present_unchecked_frames: 0,
        dstc_passed_frames: 0,
        dstc_failed_frames: 0,
        dstc_malformed_frames: 0,
        decoded_dsd_bytes: 0,
        encoded_dst_bytes: 0,
        failures: Vec::new(),
    };

    for path in paths {
        let path = path.as_ref().to_path_buf();
        out.files_seen += 1;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(err) => {
                out.files_failed += 1;
                out.failures.push(DsdCorpusAcceptanceFailure::IoOpen {
                    path,
                    message: io_error_message(err),
                });
                continue;
            }
        };
        let report = validate_dsd_stream(file, options.validation.clone());
        if let Some(kind) = report.source_kind {
            match kind {
                DsdSourceKind::Dsf => out.dsf_files += 1,
                DsdSourceKind::DsdiffDsd => out.dsdiff_dsd_files += 1,
                DsdSourceKind::DsdiffDst => out.dsdiff_dst_files += 1,
            }
        }
        out.dst_frames_seen += report.dst_frames_seen;
        out.dstc_checked_frames += report.dstc_checked_frames;
        out.dstc_missing_frames += report.dstc_missing_frames;
        out.dstc_no_crc_frames += report.dstc_no_crc_frames;
        out.dstc_present_unchecked_frames += report.dstc_present_unchecked_frames;
        out.dstc_passed_frames += report.dstc_passed_frames;
        out.dstc_failed_frames += report.dstc_failed_frames;
        out.dstc_malformed_frames += report.dstc_malformed_frames;
        out.decoded_dsd_bytes += report.decoded_dsd_bytes;
        out.encoded_dst_bytes += report.encoded_dst_bytes;

        if report.is_success() {
            out.files_succeeded += 1;
        } else {
            out.files_failed += 1;
            if options.require_all_success {
                if let Some(failure) = report.failures.first() {
                    out.failures.push(DsdCorpusAcceptanceFailure::FileValidationFailed {
                        path: path.clone(),
                        kind: failure.kind,
                        message: failure.message.clone(),
                    });
                } else {
                    out.failures.push(DsdCorpusAcceptanceFailure::FileValidationFailed {
                        path: path.clone(),
                        kind: DsdValidationFailureKind::Read,
                        message: "validation did not reach EOF".to_string(),
                    });
                }
            }
        }
        out.entries.push(DsdCorpusEntryReport { path, report });
    }

    if out.files_seen == 0 {
        out.failures.push(DsdCorpusAcceptanceFailure::EmptyCorpus);
    }
    if options.require_dst_coverage && out.dsdiff_dst_files == 0 {
        out.failures.push(DsdCorpusAcceptanceFailure::MissingDstCoverage);
    }
    if options.require_dst_coverage
        && options.validation.mode != DsdValidationMode::DecodeDst
        && out.dsdiff_dst_files > 0
    {
        out.failures.push(DsdCorpusAcceptanceFailure::DstCoverageNotDecoded);
    }
    if options.require_uncompressed_file_coverage && out.dsf_files == 0 {
        out.failures.push(DsdCorpusAcceptanceFailure::MissingDsfCoverage);
    }
    if options.require_uncompressed_file_coverage && out.dsdiff_dsd_files == 0 {
        out.failures.push(DsdCorpusAcceptanceFailure::MissingDsdiffDsdCoverage);
    }
    out
}

fn io_error_message(err: io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => format!("{} (os error {})", err, code),
        None => err.to_string(),
    }
}

/// True if the validation report is for a decoded DSDIFF/DST file.
pub fn report_has_decoded_dst_coverage(report: &DsdValidationReport) -> bool {
    matches!(report.source_kind, Some(DsdSourceKind::DsdiffDst))
        && matches!(report.info.as_ref().map(|i| (i.format, i.compression)), Some((DsdContainerFormat::Dsdiff, DsdCompression::Dst)))
        && report.dst_frames_seen > 0
        && report.decoded_dsd_bytes > 0
        && report.dstc_checked_frames + report.dstc_missing_frames == report.dst_frames_seen
        && report.failures.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dff_dst_writer::{DffDstWriter, SACD_SAMPLING_FREQUENCY};
    use crate::dff_writer::DffWriter;
    use crate::dst::{dst_interleaved_frame_len, encode_uncompressed_frame_interleaved};
    use std::fs;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("sacd_rs_{}_{}_{}.dff", name, std::process::id(), nanos))
    }

    #[test]
    fn corpus_harness_requires_dst_coverage() {
        let payload = vec![0xaa, 0x55, 0x12, 0x34];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&payload).unwrap();
            writer.finish().unwrap();
        }
        let path = unique_path("no_dst");
        fs::write(&path, cursor.into_inner()).unwrap();
        let report = validate_dsd_corpus_paths([path.clone()], DsdCorpusValidationOptions::decode_dst());
        let _ = fs::remove_file(path);
        assert!(!report.is_success());
        assert!(report.failures.iter().any(|f| matches!(f, DsdCorpusAcceptanceFailure::MissingDstCoverage)));
    }

    #[test]
    fn corpus_harness_accepts_decoded_dst_coverage() {
        let frame_len = dst_interleaved_frame_len(2).unwrap();
        let decoded = vec![0xaa; frame_len];
        let encoded = encode_uncompressed_frame_interleaved(&decoded, 2).unwrap();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_encoded_frame(&encoded, &decoded).unwrap();
            writer.finish().unwrap();
        }
        let path = unique_path("dst");
        fs::write(&path, cursor.into_inner()).unwrap();
        let report = validate_dsd_corpus_paths([path.clone()], DsdCorpusValidationOptions::decode_dst());
        let _ = fs::remove_file(path);
        assert!(report.is_success(), "unexpected corpus failures: {:?}", report.failures);
        assert_eq!(report.dsdiff_dst_files, 1);
        assert_eq!(report.dst_frames_seen, 1);
        assert_eq!(report.dstc_checked_frames, 1);
        assert!(report_has_decoded_dst_coverage(&report.entries[0].report));
    }
}

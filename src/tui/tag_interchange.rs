//! Text-tag interchange primitives shared by Browse and the metadata editor.
//!
//! The format is deliberately small and fail-closed: one upper-case field key,
//! one encoded value per following line, and one byte-empty line between
//! blocks. Empty values use `~`; literal all-tilde values gain one leading
//! tilde. Values containing line breaks are not representable and are omitted
//! by serialization rather than being silently altered.

use std::fmt;

use super::probe::{MetadataStoredValueCollapse, TagEntry};

/// Picker-local filter for tag transfer. Keeping this separate from the global
/// Audio filter prevents `.cue` visibility from changing unrelated pickers.
pub(crate) fn tag_transfer_picker_filter() -> tui_file_picker::FilePickerFilter {
    let mut extensions = crate::convert::classify::SUPPORTED_AUDIO_FILE_EXTENSIONS
        .iter()
        .map(|extension| (*extension).to_string())
        .collect::<Vec<_>>();
    extensions.push("cue".to_string());
    tui_file_picker::FilePickerFilter::Custom {
        label: "Audio and CUE".to_string(),
        extensions,
    }
}

/// Carrier selected at the transfer boundary. CUE carriers retain both their
/// policy-selected text and the concrete write authority needed for TOCTOU
/// revalidation immediately before a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarCueWriteMethod {
    /// Rewrite only the authored sidecar. This is the explicit `.cue`
    /// contract and the only supported target shape for synthetic multi-part
    /// image albums this round.
    SidecarOnly,
    /// The sidecar is the sole writable metadata authority because every
    /// referenced audio carrier was classified as metadata-unsupported by the
    /// existing lofty/probe path. Transfer reads must not reopen those
    /// carriers for field data, and transfer writes must report their embedded
    /// tag surfaces as blocked/unsupported while still committing the sidecar.
    UnsupportedCarriersSidecarOnly,
    /// Target-only authority for cue-less unsupported carriers. Classification
    /// captures the exact sidecar state so execution cannot silently adopt a
    /// sidecar created or changed by another writer after classification.
    UnsupportedCarriersCreateOrRewriteSidecarOnly {
        /// `None` means the path was absent. `Some(bytes)` is the exact
        /// structurally-invalid placeholder observed during classification.
        expected_original: Option<Vec<u8>>,
    },
    /// Write full Files-dimension tags to each one-file-per-track member, then
    /// rewrite the CUE-capped sidecar only after every member write succeeds.
    PerFileAndSidecar,
}

impl SidecarCueWriteMethod {
    fn is_unsupported_authority(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedCarriersSidecarOnly
                | Self::UnsupportedCarriersCreateOrRewriteSidecarOnly { .. }
        )
    }

    fn materialization_expected_original(&self) -> Option<&Option<Vec<u8>>> {
        match self {
            Self::UnsupportedCarriersCreateOrRewriteSidecarOnly { expected_original } => {
                Some(expected_original)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddedCueCarrier {
    pub(crate) image_path: std::path::PathBuf,
    pub(crate) cue_text: String,
    pub(crate) sheet: crate::convert::cue_parser::CueSheet,
    /// A multi-FILE embedded CUESHEET may be read as a source, but one
    /// member image cannot authoritatively rewrite sibling references.
    pub(crate) multi_file_read_only: bool,
}

#[derive(Debug, Clone)]
pub enum TransferCarrier {
    Files { paths: Vec<std::path::PathBuf> },
    SidecarCue {
        cue_path: std::path::PathBuf,
        /// Distinct referenced member images in first-reference order. For an
        /// unsupported explicit-file selection this is the selected subset;
        /// `track_audio_paths` remains the complete admitted CUE ownership map.
        image_paths: Vec<std::path::PathBuf>,
        /// Member ownership aligned to authored TRACK-number order.
        track_audio_paths: Vec<std::path::PathBuf>,
        role: crate::convert::split_cue_album::SplitCueMemberRole,
        write_method: SidecarCueWriteMethod,
        // Exact CUE snapshot/template admitted by classification. Existing
        // sidecar rewrites revalidate the live snapshot; target-only
        // materialization composes metadata onto this FILE/TRACK structure.
        cue_text: String,
        sheet: crate::convert::cue_parser::CueSheet,
    },
    /// One explicitly selected image with a usable embedded CUESHEET.
    EmbeddedCue {
        image_path: std::path::PathBuf,
        #[allow(dead_code)]
        cue_text: String,
        sheet: crate::convert::cue_parser::CueSheet,
        multi_file_read_only: bool,
    },
    /// Ordered aggregate representation selected from a directory, or from
    /// multiple explicitly selected images that all carry usable embedded CUEs.
    EmbeddedCues { carriers: Vec<EmbeddedCueCarrier> },
    /// One folder-level logical selection containing independently resolved
    /// metadata groups and/or genuinely uncovered ordinary files. Components
    /// retain deterministic directory order, while each CUE component retains
    /// its authored track order. Classification flattens nested aggregates and
    /// never constructs an empty aggregate.
    Aggregate { carriers: Vec<TransferCarrier> },
}

/// Availability facts consumed by ordered aggregate-target resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AggregateMetadataAvailability {
    pub individual_files: bool,
    pub sidecar_cue: bool,
    pub embedded_cue: bool,
}

/// Resolve the first configured target accepted by a caller-supplied probe.
pub fn resolve_aggregate_metadata_target_by(
    priority: &[crate::config::AggregateMetadataTarget],
    mut is_available: impl FnMut(crate::config::AggregateMetadataTarget) -> bool,
) -> Option<crate::config::AggregateMetadataTarget> {
    crate::config::normalized_aggregate_metadata_target_priority(priority)
        .into_iter()
        .find(|target| is_available(*target))
}

/// Resolve the first available target in normalized configured order.
pub fn resolve_aggregate_metadata_target(
    priority: &[crate::config::AggregateMetadataTarget],
    availability: AggregateMetadataAvailability,
) -> Option<crate::config::AggregateMetadataTarget> {
    resolve_aggregate_metadata_target_by(priority, |target| match target {
        crate::config::AggregateMetadataTarget::IndividualFiles => availability.individual_files,
        crate::config::AggregateMetadataTarget::SidecarCue => availability.sidecar_cue,
        crate::config::AggregateMetadataTarget::EmbeddedCue => availability.embedded_cue,
    })
}

impl TransferCarrier {
    pub(crate) fn dimension(&self) -> TransferDimension {
        match self {
            Self::Files { paths } => TransferDimension::Files(paths.len()),
            Self::SidecarCue {
                image_paths,
                track_audio_paths,
                write_method,
                ..
            } if write_method.is_unsupported_authority() => TransferDimension::Tracks(
                unsupported_sidecar_selected_track_indices_unchecked(
                    image_paths,
                    track_audio_paths,
                )
                .len(),
            ),
            Self::SidecarCue { sheet, .. } | Self::EmbeddedCue { sheet, .. } => {
                TransferDimension::Tracks(sheet.tracks.len())
            }
            Self::EmbeddedCues { carriers } => TransferDimension::Tracks(
                carriers
                    .iter()
                    .map(|carrier| carrier.sheet.tracks.len())
                    .sum(),
            ),
            Self::Aggregate { carriers } => TransferDimension::Tracks(
                carriers.iter().map(TransferCarrier::count).sum(),
            ),
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.dimension().count()
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Files { .. } => "files",
            Self::SidecarCue { .. } => "sidecar CUE",
            Self::EmbeddedCue { .. } | Self::EmbeddedCues { .. } => "embedded CUE",
            Self::Aggregate { .. } => "aggregate metadata",
        }
    }

    pub(crate) fn authored_track_numbers(&self) -> Option<Vec<u32>> {
        match self {
            Self::Files { .. } => None,
            Self::SidecarCue {
                image_paths,
                track_audio_paths,
                write_method,
                sheet,
                ..
            } if write_method.is_unsupported_authority() => {
                let selected = unsupported_sidecar_selected_track_indices_unchecked(
                    image_paths,
                    track_audio_paths,
                );
                let mut numbers = sheet
                    .tracks
                    .iter()
                    .map(|track| track.number)
                    .collect::<Vec<_>>();
                numbers.sort_unstable();
                Some(
                    selected
                        .into_iter()
                        .filter_map(|index| numbers.get(index).copied())
                        .collect(),
                )
            }
            Self::SidecarCue { sheet, .. } | Self::EmbeddedCue { sheet, .. } => {
                let mut numbers = sheet
                    .tracks
                    .iter()
                    .map(|track| track.number)
                    .collect::<Vec<_>>();
                numbers.sort_unstable();
                Some(numbers)
            }
            Self::EmbeddedCues { carriers } => Some(
                carriers
                    .iter()
                    .flat_map(|carrier| carrier.sheet.tracks.iter().map(|track| track.number))
                    .collect(),
            ),
            Self::Aggregate { carriers } => {
                let mut numbers = Vec::new();
                for carrier in carriers {
                    numbers.extend(carrier.authored_track_numbers()?);
                }
                Some(numbers)
            }
        }
    }

    /// Stable within one process and complete enough to revalidate a prepared
    /// folder target before dispatch. CUE writers still perform their stronger
    /// content-specific snapshot checks immediately before publication.
    pub(crate) fn classification_identity(&self) -> String {
        format!("{self:?}")
    }

    fn write_operation_count(&self) -> usize {
        match self {
            Self::Files { paths } => paths.len(),
            Self::SidecarCue {
                track_audio_paths,
                write_method: SidecarCueWriteMethod::PerFileAndSidecar,
                ..
            } => track_audio_paths.len().saturating_add(1),
            Self::SidecarCue {
                image_paths,
                write_method,
                ..
            } if write_method.is_unsupported_authority() => image_paths.len().saturating_add(1),
            Self::SidecarCue { .. } | Self::EmbeddedCue { .. } => 1,
            Self::EmbeddedCues { carriers } => carriers.len(),
            Self::Aggregate { carriers } => carriers
                .iter()
                .map(TransferCarrier::write_operation_count)
                .sum(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDimension {
    Files(usize),
    Tracks(usize),
}

impl TransferDimension {
    pub(crate) fn count(self) -> usize {
        match self {
            Self::Files(count) | Self::Tracks(count) => count,
        }
    }

    fn is_tracks(self) -> bool {
        matches!(self, Self::Tracks(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstTrackCollapseEligibility {
    Forbidden,
    /// The target is the file-dimensional metadata surface of one audio image
    /// and that surface carries a CUESHEET anchor. This is the only shape in
    /// which collapsing multiple source positions to the first track is
    /// semantically authorized.
    SingleImageWithCuesheet,
}

impl FirstTrackCollapseEligibility {
    fn permits(self) -> bool {
        matches!(self, Self::SingleImageWithCuesheet)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBlock {
    pub key: String,
    /// One ordered list per source position. Scalar tag-block lines decode to
    /// zero-or-one-value lists; the v1 list marker preserves repeated values.
    pub values: Vec<super::probe::MetadataFieldValues>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldBlockValueMode {
    Broadcast,
    Positional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkippedFieldReason {
    InvalidKey,
    DuplicateKey,
    MultilineValue,
    MissingValues,
}

impl fmt::Display for SkippedFieldReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey => write!(f, "field key is not representable"),
            Self::DuplicateKey => write!(f, "duplicate field key not representable"),
            Self::MultilineValue => write!(f, "multi-line value not representable"),
            Self::MissingValues => write!(f, "field has no per-file values"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedField {
    pub key: String,
    pub reason: SkippedFieldReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldBlockSerialization {
    pub text: String,
    pub keys: Vec<String>,
    pub skipped: Vec<SkippedField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldBlockParseError {
    Empty,
    BareCarriageReturn,
    EmptyBlock { block: usize },
    InvalidKey { block: usize, key: String },
    MissingValues { block: usize, key: String },
}

impl fmt::Display for FieldBlockParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "no tag blocks found"),
            Self::BareCarriageReturn => write!(f, "tag blocks contain a bare carriage return"),
            Self::EmptyBlock { block } => write!(f, "tag block {} is empty", block + 1),
            Self::InvalidKey { block, key } => write!(
                f,
                "tag block {} has invalid key {:?}; keys must match [A-Z0-9_]+",
                block + 1,
                key
            ),
            Self::MissingValues { block, key } => write!(
                f,
                "tag block {} ({}) has no value lines",
                block + 1,
                key
            ),
        }
    }
}

impl std::error::Error for FieldBlockParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBlockCountError {
    pub key: String,
    pub value_count: usize,
    pub target_count: usize,
}

impl fmt::Display for FieldBlockCountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} has {} value{} for {} file{}",
            self.key,
            self.value_count,
            if self.value_count == 1 { "" } else { "s" },
            self.target_count,
            if self.target_count == 1 { "" } else { "s" }
        )
    }
}

impl std::error::Error for FieldBlockCountError {}

pub fn is_field_block_key(line: &str) -> bool {
    !line.is_empty()
        && line
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn encode_value(value: &str) -> String {
    if value.is_empty() {
        return "~".to_string();
    }
    if value.bytes().all(|byte| byte == b'~') {
        let mut encoded = String::with_capacity(value.len() + 1);
        encoded.push('~');
        encoded.push_str(value);
        return encoded;
    }
    value.to_string()
}

fn decode_value(line: &str) -> String {
    if line == "~" {
        return String::new();
    }
    if line.len() >= 2 && line.bytes().all(|byte| byte == b'~') {
        return line[1..].to_string();
    }
    line.to_string()
}


const MULTI_VALUE_LINE_PREFIX: &str = "@tonepoet-mv1:";

fn encode_field_position(values: &super::probe::MetadataFieldValues) -> String {
    if values.value_count() <= 1 && !values.as_str().starts_with(MULTI_VALUE_LINE_PREFIX) {
        return encode_value(values.as_str());
    }
    let encoded = serde_json::to_string(&values.to_texts())
        .expect("serializing a vector of strings to JSON cannot fail");
    format!("{MULTI_VALUE_LINE_PREFIX}{encoded}")
}

fn decode_field_position(line: &str) -> super::probe::MetadataFieldValues {
    if let Some(encoded) = line.strip_prefix(MULTI_VALUE_LINE_PREFIX) {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(encoded) {
            return super::probe::MetadataFieldValues::from_stored_texts(values);
        }
    }
    super::probe::MetadataFieldValues::from_scalar(decode_value(line))
}

pub fn serialize_tag_entries<'a>(
    entries: impl IntoIterator<Item = &'a TagEntry>,
) -> FieldBlockSerialization {
    let mut blocks = Vec::new();
    let mut keys = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for entry in entries {
        if entry.is_binary {
            continue;
        }
        let key = entry.display_key.clone();
        if !is_field_block_key(&key) {
            skipped.push(SkippedField {
                key: entry.display_key.clone(),
                reason: SkippedFieldReason::InvalidKey,
            });
            continue;
        }
        if entry
            .per_file_values
            .iter()
            .any(|values| {
                values
                    .values()
                    .iter()
                    .any(|value| value.text.contains('\n') || value.text.contains('\r'))
            })
        {
            skipped.push(SkippedField {
                key,
                reason: SkippedFieldReason::MultilineValue,
            });
            continue;
        }

        let mut block = String::new();
        block.push_str(&key);
        for value in &entry.per_file_values {
            block.push('\n');
            block.push_str(&encode_field_position(value));
        }
        // Synthetic rows with no positional values cannot form a valid block.
        if entry.per_file_values.is_empty() {
            skipped.push(SkippedField {
                key,
                reason: SkippedFieldReason::MissingValues,
            });
            continue;
        }
        if !seen.insert(key.clone()) {
            skipped.push(SkippedField {
                key,
                reason: SkippedFieldReason::DuplicateKey,
            });
            continue;
        }
        keys.push(key);
        blocks.push(block);
    }

    FieldBlockSerialization {
        text: blocks.join("\n\n"),
        keys,
        skipped,
    }
}

pub fn parse_field_blocks(input: &str) -> Result<Vec<FieldBlock>, FieldBlockParseError> {
    if input.is_empty() {
        return Err(FieldBlockParseError::Empty);
    }
    let mut normalized = input.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(FieldBlockParseError::BareCarriageReturn);
    }

    // A single final line terminator is ordinary text-file framing. A second
    // one would be a trailing empty block and is rejected.
    if normalized.ends_with('\n') {
        normalized.pop();
        if normalized.ends_with('\n') {
            return Err(FieldBlockParseError::EmptyBlock { block: 1 });
        }
    }
    if normalized.is_empty() {
        return Err(FieldBlockParseError::Empty);
    }

    let mut parsed = Vec::new();
    for (block_index, raw_block) in normalized.split("\n\n").enumerate() {
        if raw_block.is_empty() {
            return Err(FieldBlockParseError::EmptyBlock { block: block_index });
        }
        let mut lines = raw_block.split('\n');
        let key = lines.next().unwrap_or_default();
        if !is_field_block_key(key) {
            return Err(FieldBlockParseError::InvalidKey {
                block: block_index,
                key: key.to_string(),
            });
        }
        let values = lines.map(decode_field_position).collect::<Vec<_>>();
        if values.is_empty() {
            return Err(FieldBlockParseError::MissingValues {
                block: block_index,
                key: key.to_string(),
            });
        }
        parsed.push(FieldBlock {
            key: key.to_string(),
            values,
        });
    }

    if parsed.is_empty() {
        Err(FieldBlockParseError::Empty)
    } else {
        Ok(parsed)
    }
}

pub fn validate_block_count(
    block: &FieldBlock,
    target_count: usize,
) -> Result<FieldBlockValueMode, FieldBlockCountError> {
    if block.values.len() == 1 {
        return Ok(FieldBlockValueMode::Broadcast);
    }
    if block.values.len() == target_count {
        return Ok(FieldBlockValueMode::Positional);
    }
    Err(FieldBlockCountError {
        key: block.key.clone(),
        value_count: block.values.len(),
        target_count,
    })
}

pub fn value_for_target(
    block: &FieldBlock,
    mode: FieldBlockValueMode,
    target_index: usize,
) -> Option<&super::probe::MetadataFieldValues> {
    match mode {
        FieldBlockValueMode::Broadcast => block.values.first(),
        FieldBlockValueMode::Positional => block.values.get(target_index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::probe::RowScope;

    #[test]
    fn aggregate_target_resolver_honors_order_and_normalizes_partial_config() {
        use crate::config::AggregateMetadataTarget::{
            EmbeddedCue, IndividualFiles, SidecarCue,
        };

        let all = AggregateMetadataAvailability {
            individual_files: true,
            sidecar_cue: true,
            embedded_cue: true,
        };
        assert_eq!(
            resolve_aggregate_metadata_target(&[IndividualFiles, SidecarCue, EmbeddedCue], all),
            Some(IndividualFiles),
        );
        assert_eq!(
            resolve_aggregate_metadata_target(&[EmbeddedCue, SidecarCue, IndividualFiles], all),
            Some(EmbeddedCue),
        );
        assert_eq!(
            resolve_aggregate_metadata_target(
                &[SidecarCue, SidecarCue],
                AggregateMetadataAvailability {
                    individual_files: true,
                    sidecar_cue: false,
                    embedded_cue: false,
                },
            ),
            Some(IndividualFiles),
            "missing targets are appended in the stable default order",
        );
        assert_eq!(
            resolve_aggregate_metadata_target(&[], all),
            Some(SidecarCue),
            "an empty configured list normalizes to the stable default order",
        );
        assert_eq!(
            resolve_aggregate_metadata_target(
                &[],
                AggregateMetadataAvailability::default(),
            ),
            None,
        );
    }

    fn entry(key: &str, values: &[&str]) -> TagEntry {
        TagEntry {
            display_key: key.to_string(),
            item_key: lofty::tag::ItemKey::Unknown(key.to_string()),
            value: values.first().copied().unwrap_or_default().to_string(),
            original: values.first().copied().unwrap_or_default().to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            row_scope: RowScope::File,
            per_file_stored_value_counts: vec![1; values.len()],
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(values.iter().map(|value| (*value).to_string()).collect()),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(values.iter().map(|value| (*value).to_string()).collect()),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    #[test]
    fn field_blocks_round_trip_empty_tildes_whitespace_and_crlf() {
        let entries = vec![
            entry("TITLE", &["", "~", "~~", "  "]),
            entry("ARTIST", &["Genesis", "Genesis", "Genesis", "Genesis"]),
        ];
        let serialized = serialize_tag_entries(entries.iter());
        assert_eq!(
            serialized.text,
            "TITLE\n~\n~~\n~~~\n  \n\nARTIST\nGenesis\nGenesis\nGenesis\nGenesis"
        );
        let crlf = serialized.text.replace('\n', "\r\n");
        assert_eq!(
            parse_field_blocks(&crlf).unwrap(),
            vec![
                FieldBlock {
                    key: "TITLE".to_string(),
                    values: crate::tui::probe::metadata_field_values_from_scalars(vec!["".to_string(), "~".to_string(), "~~".to_string(), "  ".to_string()]),
                },
                FieldBlock {
                    key: "ARTIST".to_string(),
                    values: crate::tui::probe::metadata_field_values_from_scalars(
                        vec!["Genesis".to_string(); 4],
                    ),
                },
            ]
        );
    }

    #[test]
    fn field_blocks_preserve_repeated_values_order_duplicates_and_prefix_collisions() {
        let mut repeated = entry("COMPOSER", &["placeholder"]);
        repeated.per_file_values[0] = super::super::probe::MetadataFieldValues::from_stored_texts([
            "Alice",
            "Alice",
            "Bob; Carol",
            "@tonepoet-mv1:literal",
        ]);
        repeated.per_file_originals = repeated.per_file_values.clone();
        repeated.value = repeated.per_file_values[0].as_str().to_string();
        repeated.original = repeated.value.clone();

        let serialized = serialize_tag_entries(std::iter::once(&repeated));
        assert!(serialized.text.starts_with("COMPOSER\n@tonepoet-mv1:["));
        let parsed = parse_field_blocks(&serialized.text).expect("parse repeated-value field block");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].values, repeated.per_file_values);
        assert_eq!(
            parsed[0].values[0].to_texts(),
            vec!["Alice", "Alice", "Bob; Carol", "@tonepoet-mv1:literal"],
        );

        let legacy = parse_field_blocks("COMPOSER\nAlice; Bob")
            .expect("legacy scalar field block stays readable");
        assert_eq!(legacy[0].values[0].to_texts(), vec!["Alice; Bob"]);
    }

    #[test]
    fn serializer_skips_multiline_values_without_altering_them() {
        let entries = vec![entry("COMMENT", &["line one\nline two"]), entry("TITLE", &["Duke"])];
        let serialized = serialize_tag_entries(entries.iter());
        assert_eq!(serialized.text, "TITLE\nDuke");
        assert_eq!(serialized.skipped.len(), 1);
        assert_eq!(serialized.skipped[0].key, "COMMENT");
        assert_eq!(serialized.skipped[0].reason, SkippedFieldReason::MultilineValue);
    }

    #[test]
    fn serializer_fails_closed_on_noncanonical_or_duplicate_keys() {
        let entries = vec![
            entry("title", &["lowercase"]),
            entry("TITLE", &["first"]),
            entry("TITLE", &["second"]),
        ];
        let serialized = serialize_tag_entries(entries.iter());
        assert_eq!(serialized.text, "TITLE\nfirst");
        assert_eq!(serialized.skipped.len(), 2);
        assert_eq!(serialized.skipped[0].reason, SkippedFieldReason::InvalidKey);
        assert_eq!(serialized.skipped[1].reason, SkippedFieldReason::DuplicateKey);
    }

    #[test]
    fn parser_rejects_malformed_blocks_and_count_mismatches() {
        assert!(matches!(parse_field_blocks(""), Err(FieldBlockParseError::Empty)));
        assert!(matches!(
            parse_field_blocks("Title\nDuke"),
            Err(FieldBlockParseError::InvalidKey { .. })
        ));
        assert!(matches!(
            parse_field_blocks("TITLE"),
            Err(FieldBlockParseError::MissingValues { .. })
        ));
        assert!(matches!(
            parse_field_blocks("TITLE\nDuke\n\n"),
            Err(FieldBlockParseError::EmptyBlock { .. })
        ));
        let block = parse_field_blocks("TRACKNUMBER\n1\n2\n3").unwrap().remove(0);
        assert_eq!(
            validate_block_count(&block, 2).unwrap_err().to_string(),
            "TRACKNUMBER has 3 values for 2 files"
        );
    }

    #[test]
    fn serializer_parser_identity_on_serialized_subset() {
        let entries = vec![
            entry("TITLE", &["Behind the Lines", "Duchess"]),
            entry("COMMENT", &["", "~~~"]),
        ];
        let serialized = serialize_tag_entries(entries.iter());
        let parsed = parse_field_blocks(&serialized.text).unwrap();
        assert_eq!(parsed[0].values, entries[0].per_file_values);
        assert_eq!(parsed[1].values, entries[1].per_file_values);
    }

    #[test]
    fn field_block_round_trip_property_over_generated_values() {
        const ATOMS: &[&str] = &["", "~", "~~", " ", "  ", "Duke", "Ö", "東京", "a_b-9"];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for case in 0..1_024 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let count = ((seed >> 60) as usize % 8) + 1;
            let mut values = Vec::with_capacity(count);
            for index in 0..count {
                seed ^= seed.rotate_left(17).wrapping_add(index as u64);
                let left = ATOMS[(seed as usize) % ATOMS.len()];
                let right = ATOMS[((seed >> 11) as usize) % ATOMS.len()];
                values.push(format!("{left}{right}{case}"));
            }
            if case % 17 == 0 {
                values[0].clear();
            } else if case % 19 == 0 {
                values[0] = "~".repeat((case % 7) + 1);
            }
            let source = TagEntry {
                display_key: "CUSTOM_1".to_string(),
                item_key: lofty::tag::ItemKey::Unknown("CUSTOM_1".to_string()),
                value: values[0].clone(),
                original: values[0].clone(),
                is_binary: false,
                is_mixed: values.windows(2).any(|pair| pair[0] != pair[1]),
                has_multiple_stored_values: false,
                row_scope: RowScope::File,
                per_file_stored_value_counts: vec![1; values.len()],
                per_file_values: crate::tui::probe::metadata_field_values_from_scalars(values.clone()),
                per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(values.clone()),
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            };
            let serialized = serialize_tag_entries(std::iter::once(&source));
            let parsed = parse_field_blocks(&serialized.text).expect("generated round trip");
            assert_eq!(parsed, vec![FieldBlock {
                key: "CUSTOM_1".to_string(),
                values: crate::tui::probe::metadata_field_values_from_scalars(values),
            }]);
        }
    }

    fn editor_with_files(file_count: usize, entries: Vec<TagEntry>) -> super::super::app::MetadataEditorState {
        super::super::app::MetadataEditorState::for_files(
            (0..file_count)
                .map(|index| std::path::PathBuf::from(format!("/tmp/{:02}.flac", index + 1)))
                .collect(),
            entries,
            (0..file_count).map(|index| format!("{:02}", index + 1)).collect(),
            super::super::app::MetadataTechnicalDetails::default(),
        )
    }

    #[test]
    fn block_apply_prevalidates_all_counts_and_pins_success_wording() {
        let mut state = editor_with_files(12, Vec::new());
        let blocks = vec![
            FieldBlock {
                key: "TITLE".to_string(),
                values: crate::tui::probe::metadata_field_values_from_scalars(vec!["Duke".to_string()]),
            },
            FieldBlock {
                key: "TRACKNUMBER".to_string(),
                values: crate::tui::probe::metadata_field_values_from_scalars(
                    (1..=12).map(|value| value.to_string()).collect::<Vec<String>>(),
                ),
            },
        ];
        let report = apply_field_blocks_to_editor(&mut state, &blocks).expect("valid blocks");
        assert_eq!(
            report.success_status(12),
            "applied TITLE (broadcast to 12 files), TRACKNUMBER (positional) — review before save"
        );
        assert_eq!(state.active_surface().entries.len(), 2);
        assert_eq!(
            state.active_surface().entries[0].item_key,
            lofty::tag::ItemKey::TrackTitle,
            "new standard rows must keep their typed writer identity"
        );

        let before = state
            .active_surface()
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.display_key.clone(),
                    entry.item_key.clone(),
                    entry.value.clone(),
                    entry.per_file_values.clone(),
                )
            })
            .collect::<Vec<_>>();
        let invalid = vec![
            FieldBlock {
                key: "ARTIST".to_string(),
                values: crate::tui::probe::metadata_field_values_from_scalars(vec!["Genesis".to_string()]),
            },
            FieldBlock {
                key: "DISCNUMBER".to_string(),
                values: crate::tui::probe::metadata_field_values_from_scalars(vec!["1".to_string(), "2".to_string()]),
            },
        ];
        assert_eq!(
            apply_field_blocks_to_editor(&mut state, &invalid).unwrap_err(),
            "DISCNUMBER has 2 values for 12 files"
        );
        let after = state
            .active_surface()
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.display_key.clone(),
                    entry.item_key.clone(),
                    entry.value.clone(),
                    entry.per_file_values.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(after, before, "count failure must apply nothing");
    }

    fn stored_list_entry(key: &str, slots: &[&[&str]]) -> TagEntry {
        let mut entry = entry(key, &vec!["placeholder"; slots.len()]);
        entry.per_file_values = slots
            .iter()
            .map(|values| {
                crate::tui::probe::MetadataFieldValues::from_stored_texts(values.iter().copied())
            })
            .collect();
        entry.per_file_originals = entry.per_file_values.clone();
        entry.per_file_stored_value_counts = entry
            .per_file_values
            .iter()
            .map(crate::tui::probe::MetadataFieldValues::value_count)
            .collect();
        entry.has_multiple_stored_values = entry
            .per_file_stored_value_counts
            .iter()
            .any(|count| *count > 1);
        entry.value = entry
            .per_file_values
            .first()
            .map(|values| values.as_str().to_string())
            .unwrap_or_default();
        entry.original = entry.value.clone();
        entry
    }

    #[test]
    fn block_apply_reports_stored_list_cardinality_reduction_and_keeps_provenance() {
        let mut state = editor_with_files(
            1,
            vec![stored_list_entry("COMPOSER", &[&["Alice", "Bob"]])],
        );
        let blocks = vec![FieldBlock {
            key: "COMPOSER".to_string(),
            values: vec![crate::tui::probe::MetadataFieldValues::from_stored_texts([
                "Carol",
            ])],
        }];

        let report = apply_field_blocks_to_editor(&mut state, &blocks).expect("apply COMPOSER");
        assert_eq!(
            report.collapsed_fields,
            vec![MetadataStoredValueCollapse {
                display_key: "COMPOSER".to_string(),
                slots: vec![0],
            }]
        );
        assert_eq!(
            report.success_status(1),
            "applied COMPOSER (broadcast to 1 file), warning: COMPOSER stored-value count reduced on carrier 1 — review before save"
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_stored_value_counts,
            vec![2],
            "tag-block application must preserve original stored-cardinality provenance"
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_values[0].to_texts(),
            vec!["Carol"]
        );
    }

    #[test]
    fn block_apply_does_not_warn_for_equal_or_greater_list_cardinality_or_exact_revert() {
        let original = stored_list_entry("COMPOSER", &[&["Alice", "Bob"]]);

        for replacement in [
            vec!["Bob", "Alice"],
            vec!["Alice", "Bob", "Carol"],
            vec!["Alice", "Bob"],
        ] {
            let mut state = editor_with_files(1, vec![original.clone()]);
            let blocks = vec![FieldBlock {
                key: "COMPOSER".to_string(),
                values: vec![crate::tui::probe::MetadataFieldValues::from_stored_texts(
                    replacement,
                )],
            }];
            let report = apply_field_blocks_to_editor(&mut state, &blocks)
                .expect("apply non-reducing COMPOSER block");
            assert!(
                report.collapsed_fields.is_empty(),
                "equal-cardinality edits, growth, and exact reverts must not warn"
            );
        }
    }

    #[test]
    fn block_apply_new_field_never_reports_stored_list_collapse() {
        let mut state = editor_with_files(2, Vec::new());
        let blocks = vec![FieldBlock {
            key: "COMPOSER".to_string(),
            values: vec![
                crate::tui::probe::MetadataFieldValues::from_stored_texts(["Alice"]),
                crate::tui::probe::MetadataFieldValues::from_stored_texts(["Bob", "Carol"]),
            ],
        }];

        let report = apply_field_blocks_to_editor(&mut state, &blocks).expect("create COMPOSER");
        assert!(report.collapsed_fields.is_empty());
    }

    #[test]
    fn block_apply_reports_only_slots_below_their_original_stored_count() {
        let mut state = editor_with_files(
            3,
            vec![stored_list_entry(
                "COMPOSER",
                &[
                    &["Alice", "Bob"],
                    &["Carol", "Dave"],
                    &["Eve", "Frank", "Grace"],
                ],
            )],
        );
        let blocks = vec![FieldBlock {
            key: "COMPOSER".to_string(),
            values: vec![
                crate::tui::probe::MetadataFieldValues::from_stored_texts(["Solo"]),
                crate::tui::probe::MetadataFieldValues::from_stored_texts(["Carol", "Delta"]),
                crate::tui::probe::MetadataFieldValues::from_stored_texts(["Grace", "Eve"]),
            ],
        }];

        let report = apply_field_blocks_to_editor(&mut state, &blocks).expect("apply COMPOSER");
        assert_eq!(report.collapsed_fields.len(), 1);
        assert_eq!(report.collapsed_fields[0].display_key, "COMPOSER");
        assert_eq!(report.collapsed_fields[0].slots, vec![0, 2]);
        assert!(report.success_status(3).contains("carriers 1, 3"));
    }

    #[test]
    fn editor_apply_rejects_ambiguous_duplicate_target_rows_before_mutation() {
        let mut state = editor_with_files(
            1,
            vec![entry("TITLE", &["First"]), entry("title", &["Second"])],
        );
        let before = state
            .active_surface()
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.display_key.clone(),
                    entry.item_key.clone(),
                    entry.value.clone(),
                    entry.per_file_values.clone(),
                )
            })
            .collect::<Vec<_>>();
        let blocks = vec![FieldBlock {
            key: "TITLE".to_string(),
            values: crate::tui::probe::metadata_field_values_from_scalars(vec!["Replacement".to_string()]),
        }];

        assert_eq!(
            apply_field_blocks_to_editor(&mut state, &blocks).unwrap_err(),
            "metadata editor contains duplicate field TITLE; resolve it before applying tag blocks"
        );
        let after = state
            .active_surface()
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.display_key.clone(),
                    entry.item_key.clone(),
                    entry.value.clone(),
                    entry.per_file_values.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(after, before);
    }

    #[test]
    fn transfer_plan_broadcasts_scalars_but_never_numbering() {
        let source = vec![
            entry("TITLE", &["Duke"]),
            entry("TRACKNUMBER", &["1"]),
            entry("DISCTOTAL", &["2"]),
        ];
        let plan = plan_transfer_values(
            &source,
            1,
            3,
            super::super::app::TagTransferScope::All,
        )
        .expect("1-to-N plan");
        assert_eq!(plan.fields.len(), 1);
        assert_eq!(plan.fields[0].canonical_key, "TITLE");
        assert_eq!(plan.fields[0].values, vec!["Duke".to_string(); 3]);
        assert_eq!(
            plan.skipped_numbering_keys,
            vec!["TRACKNUMBER".to_string(), "DISCTOTAL".to_string()]
        );
    }

    #[test]
    fn transfer_plan_preserves_positional_traversal_order_and_fails_mismatch() {
        let source = vec![entry(
            "TITLE",
            &["Behind the Lines", "Duchess", "Guide Vocal"],
        )];
        let plan = plan_transfer_values(
            &source,
            3,
            3,
            super::super::app::TagTransferScope::All,
        )
        .expect("N-to-N plan");
        assert_eq!(
            plan.fields[0].values,
            vec![
                "Behind the Lines".to_string(),
                "Duchess".to_string(),
                "Guide Vocal".to_string(),
            ],
            "the planner must not sort either side after bounded traversal"
        );
        assert_eq!(
            plan_transfer_values(
                &source,
                3,
                2,
                super::super::app::TagTransferScope::All,
            )
            .unwrap_err(),
            "tag transfer requires 1 source or equal source/target counts; got 3 sources and 2 targets"
        );
    }

    #[test]
    fn transfer_plan_rejects_duplicate_or_structurally_short_sources_before_writes() {
        let duplicate = vec![entry("TITLE", &["A"]), entry("title", &["B"])];
        assert_eq!(
            plan_transfer_values(
                &duplicate,
                1,
                1,
                super::super::app::TagTransferScope::All,
            )
            .unwrap_err(),
            "tag transfer source contains duplicate field TITLE"
        );

        let short = vec![entry("TITLE", &["A", "B"])];
        assert_eq!(
            plan_transfer_values(
                &short,
                3,
                3,
                super::super::app::TagTransferScope::All,
            )
            .unwrap_err(),
            // A dimension-mismatched source entry is treated as Track-scoped by
            // effective_row_scope and filtered by selection, so the plan is
            // empty — the honest outcome; the per-position guard remains a
            // defensive backstop for constructible shapes.
            "tag transfer source contains no applicable text fields"
        );
    }

    fn flac_block(block_type: u8, last: bool, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + data.len());
        out.push((if last { 0x80 } else { 0 }) | block_type);
        let len = data.len() as u32;
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.extend_from_slice(data);
        out
    }

    fn vorbis_block_body(comments: &[(&str, &str)]) -> Vec<u8> {
        let vendor = b"tonepoet-transfer-test";
        let mut out = Vec::new();
        out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        out.extend_from_slice(vendor);
        out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for (name, value) in comments {
            let comment = format!("{name}={value}");
            out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            out.extend_from_slice(comment.as_bytes());
        }
        out
    }

    const APE_NUMBERING_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata_persistence/ape.wv"
    ));

    fn write_test_flac(path: &std::path::Path, title: &str) {
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&flac_block(0, false, &[0; 34]));
        bytes.extend_from_slice(&flac_block(
            4,
            false,
            &vorbis_block_body(&[("TITLE", title)]),
        ));
        bytes.extend_from_slice(&flac_block(1, true, &[0; 4096]));
        bytes.extend((0..4096).map(|index| (index % 251) as u8));
        std::fs::write(path, bytes).expect("write synthetic FLAC transfer target");
    }

    fn merged_value(path: &std::path::Path, key: &str) -> Option<String> {
        super::super::probe::read_all_tags_merged(&[path.to_path_buf()])
            .expect("read transfer result")
            .into_iter()
            .find(|entry| entry.display_key == key)
            .map(|entry| entry.value)
    }

    #[test]
    fn transfer_route_writes_native_flac_and_dsf_and_reports_file_progress() {
        let temp = tempfile::tempdir().expect("transfer route tempdir");
        let flac = temp.path().join("target.flac");
        let dsf = temp.path().join("target.dsf");
        write_test_flac(&flac, "Old FLAC");
        crate::dsf_tags::write_test_dsf_fixture(&dsf, None).expect("write DSF target");

        let progress = std::sync::Mutex::new(Vec::new());
        let on_progress = |completed: usize, total: usize, path: &std::path::Path| {
            progress
                .lock()
                .expect("progress lock")
                .push((completed, total, path.to_path_buf()));
        };
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();
        let report = execute_tag_transfer_from_entries(
            &[
                entry("TITLE", &["Duke"]),
                entry("TRACKNUMBER", &["1"]),
            ],
            1,
            &[flac.clone(), dsf.clone()],
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Standard,
            &cancel,
            Some(&on_progress),
        )
        .expect("1-to-N transfer route");

        assert_eq!(report.written, 2);
        assert_eq!(report.skipped_numbering_keys, vec!["TRACKNUMBER"]);
        assert_eq!(merged_value(&flac, "TITLE").as_deref(), Some("Duke"));
        assert_eq!(
            crate::dsf_tags::read(&dsf)
                .expect("read DSF transfer result")
                .first("TITLE"),
            Some("Duke")
        );
        assert_eq!(
            *progress.lock().expect("progress lock"),
            vec![(1, 2, flac), (2, 2, dsf)]
        );
    }

    #[test]
    fn transfer_route_is_positional_for_n_to_n_and_fails_n_to_m_before_io() {
        let temp = tempfile::tempdir().expect("positional transfer tempdir");
        let first = temp.path().join("01.dsf");
        let second = temp.path().join("02.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&first, None).expect("write first DSF");
        crate::dsf_tags::write_test_dsf_fixture(&second, None).expect("write second DSF");
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();

        let report = execute_tag_transfer_from_entries(
            &[entry("TITLE", &["Behind the Lines", "Duchess"])],
            2,
            &[first.clone(), second.clone()],
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Standard,
            &cancel,
            None,
        )
        .expect("N-to-N transfer route");
        assert_eq!(report.written, 2);
        assert_eq!(
            crate::dsf_tags::read(&first).expect("first DSF").first("TITLE"),
            Some("Behind the Lines")
        );
        assert_eq!(
            crate::dsf_tags::read(&second).expect("second DSF").first("TITLE"),
            Some("Duchess")
        );

        let error = execute_tag_transfer_from_entries(
            &[entry("TITLE", &["A", "B"])],
            2,
            &[
                std::path::PathBuf::from("/must/not/read/one.dsf"),
                std::path::PathBuf::from("/must/not/read/two.dsf"),
                std::path::PathBuf::from("/must/not/read/three.dsf"),
            ],
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Standard,
            &cancel,
            None,
        )
        .expect_err("N-to-M route must fail before target I/O");
        assert_eq!(
            error,
            "tag transfer requires 1 source or equal source/target counts; got 2 sources and 3 targets"
        );
    }

    #[test]
    fn mixed_wavpack_transfer_empty_slot_deletes_native_fallback_target_field() {
        // Strong-mode writes consult the metadata journal DB; isolate
        // XDG dirs (and serialize with other env-redirecting tests) so a
        // concurrent guard user cannot swap the journal path mid-write.
        let _xdg = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-mixed-wavpack-transfer",
        );
        let temp = tempfile::tempdir().expect("mixed WavPack transfer tempdir");
        let first = temp.path().join("01-first.wv");
        let second = temp.path().join("02-second.wv");
        std::fs::write(&first, APE_NUMBERING_FIXTURE).expect("write first WavPack target");
        std::fs::write(&second, APE_NUMBERING_FIXTURE).expect("write second WavPack target");
        // The shared fixture has no TITLE; seed one on the delete target so
        // the empty slot performs a real deletion (a written change).
        super::super::probe::write_all_tags(
            &first,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Doomed Title".to_string()))],
        )
        .expect("seed TITLE on first target");
        super::super::probe::inject_invalid_ape_key_item_for_test(
            &first,
            "&год".as_bytes(),
            b"1977",
        )
        .expect("force native fallback for first target");
        super::super::probe::inject_invalid_ape_key_item_for_test(
            &second,
            "&год".as_bytes(),
            b"1977",
        )
        .expect("force native fallback for second target");

        let cancel = super::super::probe::MetadataWriteCancelFlag::new();
        let report = execute_tag_transfer_from_entries(
            &[entry("TITLE", &["", "Second title"])],
            2,
            &[first.clone(), second.clone()],
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("mixed native-fallback WavPack transfer");

        assert_eq!(
            report.failed,
            Vec::new(),
            "mixed transfer must not fail any target"
        );
        assert_eq!(report.written, 2);
        // File-level absence proof (the merged reader may synthesize
        // canonical placeholder rows).
        assert!(
            !super::super::probe::native_ape_has_item_for_test(&first, "TITLE")
                .expect("parse first target"),
            "deleted TITLE item must be absent from the rewritten APEv2 tag"
        );
        assert_eq!(merged_value(&second, "TITLE").as_deref(), Some("Second title"));
    }

    #[test]
    fn transfer_from_paths_reads_source_and_writes_target_in_strong_mode() {
        let temp = tempfile::tempdir().expect("path transfer tempdir");
        let source = temp.path().join("source.dsf");
        let target = temp.path().join("target.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&source, None).expect("write source DSF");
        crate::dsf_tags::write_test_dsf_fixture(&target, None).expect("write target DSF");
        super::super::probe::write_all_tags_for_transfer_at_verification(
            &source,
            &[(lofty::tag::ItemKey::TrackTitle, Some("Source title".to_string()))],
            None,
            tui_file_picker::VerificationMode::Strong,
        )
        .expect("seed source metadata through transfer writer seam");

        let cancel = super::super::probe::MetadataWriteCancelFlag::new();
        let report = execute_tag_transfer_from_paths(
            std::slice::from_ref(&source),
            std::slice::from_ref(&target),
            super::super::app::TagTransferScope::Canonical,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("path-based transfer route");
        assert_eq!(report.written, 1);
        assert_eq!(
            crate::dsf_tags::read(&target)
                .expect("read path-transfer target")
                .first("TITLE"),
            Some("Source title")
        );
    }

    #[test]
    fn transfer_route_forwards_target_diff_cancel_and_verification_to_writer() {
        let temp = tempfile::tempdir().expect("writer route tempdir");
        let target = temp.path().join("target.dsf");
        crate::dsf_tags::write_test_dsf_fixture(&target, None).expect("write DSF target");
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();

        for verification in [
            tui_file_picker::VerificationMode::Standard,
            tui_file_picker::VerificationMode::Strong,
        ] {
            let mut observed = Vec::new();
            let report = execute_tag_transfer_from_entries_with_writer(
                &[entry("TITLE", &["Duke"])],
                1,
                std::slice::from_ref(&target),
                super::super::app::TagTransferScope::All,
                verification,
                &cancel,
                None,
                |path, changes, routed_cancel, routed_verification| {
                    observed.push((
                        path.to_path_buf(),
                        changes.to_vec(),
                        routed_cancel.is_some(),
                        routed_verification,
                    ));
                    Ok(super::super::probe::MetadataWriteCommitReport::default())
                },
            )
            .expect("spy transfer route");

            assert_eq!(report.written, 1);
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].0, target);
            assert!(observed[0].2);
            assert_eq!(observed[0].3, verification);
            assert_eq!(
                observed[0].1,
                vec![(lofty::tag::ItemKey::TrackTitle, Some(crate::tui::probe::MetadataFieldValues::from_scalar("Duke")), false)]
            );
        }
    }

    #[test]
    fn transfer_picker_filter_is_canonical_audio_plus_cue_without_widening_global_audio() {
        let transfer = tag_transfer_picker_filter();
        let actual_extensions = match &transfer {
            tui_file_picker::FilePickerFilter::Custom { extensions, .. } => extensions
                .iter()
                .map(|extension| extension.to_ascii_lowercase())
                .collect::<std::collections::BTreeSet<_>>(),
            other => panic!("transfer filter must be explicit canonical composition: {other:?}"),
        };
        let mut expected_extensions = crate::convert::classify::SUPPORTED_AUDIO_FILE_EXTENSIONS
            .iter()
            .map(|extension| (*extension).to_string())
            .collect::<std::collections::BTreeSet<_>>();
        expected_extensions.insert("cue".to_string());
        assert_eq!(
            actual_extensions, expected_extensions,
            "transfer filter must equal canonical audio coverage plus only CUE"
        );

        for extension in crate::convert::classify::SUPPORTED_AUDIO_FILE_EXTENSIONS {
            let path = std::path::PathBuf::from(format!("track.{extension}"));
            assert!(
                crate::convert::classify::is_audio_file_path(&path),
                "canonical source must classify {extension} as audio"
            );
            assert!(
                transfer.accepts_path(&path, false),
                "transfer picker must admit canonical audio extension {extension}"
            );
        }
        assert!(transfer.accepts_path(std::path::Path::new("album.CUE"), false));
        assert!(!transfer.accepts_path(std::path::Path::new("notes.txt"), false));
        assert!(!tui_file_picker::FilePickerFilter::Audio
            .accepts_path(std::path::Path::new("album.cue"), false));
    }

    #[test]
    fn cue_rows_sort_gapped_track_numbers_positionally_and_map_fields() {
        let sheet = crate::convert::cue_parser::CueSheet {
            title: Some("Album".to_string()),
            performer: Some("Album Artist".to_string()),
            date: Some("1980".to_string()),
            genre: Some("Rock".to_string()),
            catalog: Some("CAT-7".to_string()),
            tracks: vec![
                crate::convert::cue_parser::CueTrack {
                    number: 7,
                    title: Some("Seven".to_string()),
                    performer: Some("Artist Seven".to_string()),
                    isrc: Some("ISRC00000007".to_string()),
                    ..Default::default()
                },
                crate::convert::cue_parser::CueTrack {
                    number: 2,
                    title: Some("Two".to_string()),
                    performer: Some("Artist Two".to_string()),
                    isrc: Some("ISRC00000002".to_string()),
                    ..Default::default()
                },
            ],
        };

        let entries = cue_sheet_transfer_entries(&sheet);
        let by_key = |key: &str| {
            entries
                .iter()
                .find(|entry| entry.display_key == key)
                .expect("CUE field")
        };
        assert_eq!(by_key("TITLE").per_file_values, vec!["Two", "Seven"]);
        assert_eq!(
            by_key("ARTIST").per_file_values,
            vec!["Artist Two", "Artist Seven"]
        );
        assert_eq!(
            by_key("ISRC").per_file_values,
            vec!["ISRC00000002", "ISRC00000007"]
        );
        assert_eq!(by_key("ALBUM").per_file_values, vec!["Album", "Album"]);
        assert!(entries
            .iter()
            .all(|entry| matches!(entry.row_scope, RowScope::Track)));
    }

    #[test]
    fn track_dimension_plans_cover_n_to_n_mismatch_collapse_and_one_file_skip() {
        let track_source = vec![entry("TITLE", &["First", "Second"])];
        let positional = plan_transfer_values_for_dimensions(
            &track_source,
            TransferDimension::Tracks(2),
            TransferDimension::Tracks(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("track positional plan");
        assert_eq!(positional.fields[0].values, vec!["First", "Second"]);

        let tracks_to_files = plan_transfer_values_for_dimensions(
            &track_source,
            TransferDimension::Tracks(2),
            TransferDimension::Files(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("track carrier to files positional plan");
        assert_eq!(tracks_to_files.fields[0].values, vec!["First", "Second"]);

        let files_to_tracks = plan_transfer_values_for_dimensions(
            &track_source,
            TransferDimension::Files(2),
            TransferDimension::Tracks(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("files to track carrier positional plan");
        assert_eq!(files_to_tracks.fields[0].values, vec!["First", "Second"]);

        assert_eq!(
            plan_transfer_values_for_dimensions(
                &track_source,
                TransferDimension::Tracks(2),
                TransferDimension::Tracks(3),
                super::super::app::TagTransferScope::All,
            )
            .unwrap_err(),
            "tag transfer carrier dimensions do not match: 2 source positions and 3 target positions"
        );

        assert!(plan_transfer_values_for_dimensions(
            &track_source,
            TransferDimension::Tracks(2),
            TransferDimension::Files(1),
            super::super::app::TagTransferScope::All,
        )
        .expect_err("ordinary single-file target must not collapse track values")
        .contains("do not match"));

        let collapsed = plan_transfer_values_for_dimensions_with_collapse(
            &track_source,
            TransferDimension::Tracks(2),
            TransferDimension::Files(1),
            super::super::app::TagTransferScope::All,
            FirstTrackCollapseEligibility::SingleImageWithCuesheet,
        )
        .expect("track-carrier to single-image collapse");
        assert!(collapsed.first_track_collapse);
        assert_eq!(collapsed.fields[0].values, vec!["First"]);

        assert!(plan_transfer_values_for_dimensions(
            &track_source,
            TransferDimension::Files(2),
            TransferDimension::Files(1),
            super::super::app::TagTransferScope::All,
        )
        .expect_err("ordinary single-file target must not collapse file values")
        .contains("equal source/target counts"));

        let file_collapse = plan_transfer_values_for_dimensions_with_collapse(
            &track_source,
            TransferDimension::Files(2),
            TransferDimension::Files(1),
            super::super::app::TagTransferScope::All,
            FirstTrackCollapseEligibility::SingleImageWithCuesheet,
        )
        .expect("multi-file to single-image collapse");
        assert!(file_collapse.first_track_collapse);
        assert_eq!(file_collapse.fields[0].values, vec!["First"]);

        let file_source = vec![
            entry("ALBUM", &["Duke"]),
            entry("TITLE", &["Single file title"]),
        ];
        let into_tracks = plan_transfer_values_for_dimensions(
            &file_source,
            TransferDimension::Files(1),
            TransferDimension::Tracks(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("one file into track carrier");
        assert_eq!(into_tracks.fields.len(), 1);
        assert_eq!(into_tracks.fields[0].canonical_key, "ALBUM");
        assert_eq!(into_tracks.fields[0].values, vec!["Duke", "Duke"]);
        assert!(into_tracks
            .skipped_fields
            .iter()
            .any(|message| message.starts_with("TITLE skipped:")));
    }

    #[test]
    fn first_track_collapse_requires_single_image_and_nonempty_cuesheet_evidence() {
        let source = vec![entry("TITLE", &["First", "Second"])];
        let mut ordinary = editor_with_files(1, Vec::new());
        assert_eq!(
            metadata_editor_first_track_collapse_eligibility(
                &ordinary,
                TransferDimension::Files(1)
            ),
            FirstTrackCollapseEligibility::Forbidden
        );
        assert!(apply_transfer_entries_to_editor_with_dimension(
            &mut ordinary,
            &source,
            TransferDimension::Files(2),
            super::super::app::TagTransferScope::All,
        )
        .expect_err("ordinary single file must reject multi-source collapse")
        .contains("equal source/target counts"));

        let cuesheet = concat!(
            "FILE \"image.flac\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 01 03:00:00\n",
        );
        let mut single_image = editor_with_files(1, vec![entry("CUESHEET", &[cuesheet])]);
        assert_eq!(
            metadata_editor_first_track_collapse_eligibility(
                &single_image,
                TransferDimension::Files(1)
            ),
            FirstTrackCollapseEligibility::SingleImageWithCuesheet
        );
        let report = apply_transfer_entries_to_editor_with_dimension(
            &mut single_image,
            &source,
            TransferDimension::Files(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("single image with CUESHEET may collapse to first track");
        assert!(report.first_track_collapse);
        let title = single_image
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("collapsed title row");
        assert_eq!(title.per_file_values, vec!["First"]);
    }

    #[test]
    fn cue_album_fields_use_first_source_value_with_explicit_warning() {
        let source = vec![
            entry("ALBUM", &["First Album", "Second Album"]),
            entry("TITLE", &["Track One", "Track Two"]),
        ];
        let plan = plan_transfer_values_for_dimensions(
            &source,
            TransferDimension::Files(2),
            TransferDimension::Tracks(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("file rows into CUE tracks");

        let album = plan
            .fields
            .iter()
            .find(|field| field.canonical_key == "ALBUM")
            .expect("album field");
        assert_eq!(album.values, vec!["First Album", "First Album"]);
        let title = plan
            .fields
            .iter()
            .find(|field| field.canonical_key == "TITLE")
            .expect("track title field");
        assert_eq!(title.values, vec!["Track One", "Track Two"]);
        assert_eq!(
            plan.cardinality_warnings,
            vec!["ALBUM is album-scoped in CUE; used the first source value"]
        );
    }

    #[test]
    fn cue_target_field_cap_excludes_songwriter_isrc_numbering_and_unknowns() {
        let source = vec![
            entry("TITLE", &["One", "Two"]),
            entry("ARTIST", &["A", "B"]),
            entry("ALBUM", &["Album", "Album"]),
            entry("SONGWRITER", &["Writer", "Writer"]),
            entry("ISRC", &["X", "Y"]),
            entry("TRACKNUMBER", &["1", "2"]),
            entry("COMMENT", &["No", "No"]),
        ];
        let plan = plan_transfer_values_for_dimensions(
            &source,
            TransferDimension::Tracks(2),
            TransferDimension::Tracks(2),
            super::super::app::TagTransferScope::All,
        )
        .expect("CUE cap plan");
        assert_eq!(
            plan.fields
                .iter()
                .map(|field| field.canonical_key.as_str())
                .collect::<Vec<_>>(),
            vec!["TITLE", "ARTIST", "ALBUM"]
        );
        assert!(plan
            .skipped_fields
            .iter()
            .any(|message| message.contains("SONGWRITER excluded")));
        assert!(plan
            .skipped_fields
            .iter()
            .any(|message| message.contains("ISRC skipped")));
        assert!(plan
            .skipped_fields
            .iter()
            .any(|message| message.contains("TRACKNUMBER skipped")));
        assert!(plan
            .skipped_fields
            .iter()
            .any(|message| message.contains("COMMENT skipped")));
    }

    #[test]
    fn embedded_cue_metadata_target_applicability_matches_existing_writer_routes() {
        assert!(embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.flac")
        ));
        assert!(embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.wv")
        ));
        assert!(embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.ogg")
        ));
        assert!(embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.ape")
        ));
        assert!(!embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.mpc")
        ));
        assert!(!embedded_cue_metadata_target_is_writable(
            std::path::Path::new("disc.dff")
        ));
    }

    #[test]
    fn embedded_cue_writes_fail_closed_for_read_only_targets() {
        let source = vec![entry("TITLE", &["One", "Two"])];
        let sheet = crate::convert::cue_parser::CueSheet {
            tracks: vec![
                crate::convert::cue_parser::CueTrack {
                    number: 1,
                    index01_frames: Some(0),
                    ..Default::default()
                },
                crate::convert::cue_parser::CueTrack {
                    number: 2,
                    index01_frames: Some(75),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let target = TransferCarrier::EmbeddedCue {
            image_path: std::path::PathBuf::from("image.mpc"),
            cue_text: concat!(
                "FILE \"image.wav\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n",
                "    INDEX 01 00:01:00\n",
            )
            .to_string(),
            sheet,
            multi_file_read_only: false,
        };
        let error = execute_tag_transfer_to_cue(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Standard,
            &super::super::probe::MetadataWriteCancelFlag::new(),
            None,
        )
        .expect_err("read-only embedded writes must fail closed");
        assert_eq!(
            error,
            "embedded CUE write is not supported for this audio carrier",
        );
    }

    #[test]
    fn sidecar_cue_transfer_route_preserves_structure_and_is_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("album.flac");
        let cue = temp.path().join("album.cue");
        std::fs::write(&image, b"audio").expect("image");
        let original = concat!(
            "REM COMMENT \"leave this alone\"\r\n",
            "TITLE \"Old Album\"\r\n",
            "FILE \"album.flac\" WAVE\r\n",
            "  TRACK 01 AUDIO\r\n",
            "    TITLE \"Old One\"\r\n",
            "    FLAGS PRE\r\n",
            "    INDEX 01 00:00:00\r\n",
            "  TRACK 02 AUDIO\r\n",
            "    TITLE \"Old Two\"\r\n",
            "    INDEX 01 03:00:00\r\n",
        );
        std::fs::write(&cue, original).expect("cue");
        let target = TransferCarrier::SidecarCue {
            cue_path: cue.clone(),
            image_paths: vec![image.clone()],
            track_audio_paths: vec![image; 2],
            role: crate::convert::split_cue_album::SplitCueMemberRole::SyntheticAlbumPart,
            write_method: SidecarCueWriteMethod::SidecarOnly,
            cue_text: original.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(original),
        };
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();
        let source = vec![
            entry("ALBUM", &["New Album", "New Album"]),
            entry("TITLE", &["New One", "New Two"]),
            entry("ARTIST", &["Artist One", "Artist Two"]),
        ];

        let first = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("sidecar CUE transfer");
        assert_eq!(first.written, 1);
        assert_eq!(first.failed.len(), 0);
        let first_status = first.status();
        assert!(first_status.starts_with("Wrote 3 fields to sidecar CUE"));
        assert!(first_status.contains(&cue.display().to_string()));
        assert!(first_status.contains("1 rewritten, 0 unchanged, 0 failed"));
        let rewritten = std::fs::read_to_string(&cue).expect("rewritten cue");
        assert!(rewritten.contains("REM COMMENT \"leave this alone\"\r\n"));
        assert!(rewritten.contains("FILE \"album.flac\" WAVE\r\n"));
        assert!(rewritten.contains("FLAGS PRE\r\n"));
        assert!(rewritten.contains("INDEX 01 03:00:00\r\n"));
        assert!(rewritten.contains("TITLE \"New Album\"\r\n"));
        assert!(rewritten.contains("TITLE \"New One\"\r\n"));
        assert!(rewritten.contains("PERFORMER \"Artist Two\"\r\n"));

        let before_second = std::fs::read(&cue).expect("before second transfer");
        let second = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("idempotent sidecar CUE transfer");
        assert_eq!(second.unchanged, 1);
        let second_status = second.status();
        assert!(second_status.starts_with("Sidecar CUE"));
        assert!(second_status.contains(&cue.display().to_string()));
        assert!(second_status.contains("already matched 3 fields"));
        assert!(second_status.contains("0 rewritten, 1 unchanged, 0 failed"));
        assert!(!second_status.starts_with("Wrote"));
        assert_eq!(std::fs::read(&cue).expect("after second transfer"), before_second);
    }

    #[test]
    fn unsupported_carrier_sidecar_read_uses_cue_without_reopening_audio() {
        let cue_text = concat!(
            "PERFORMER \"Cue Artist\"\n",
            "TITLE \"Cue Album\"\n",
            "FILE \"01.dff\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Cue One\"\n",
            "    INDEX 01 00:00:00\n",
            "FILE \"02.dff\" WAVE\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Cue Two\"\n",
            "    INDEX 01 00:00:00\n",
        );
        let carrier = TransferCarrier::SidecarCue {
            cue_path: std::path::PathBuf::from("/does/not/need/to/exist/album.cue"),
            image_paths: vec![
                std::path::PathBuf::from("/does/not/exist/01.dff"),
                std::path::PathBuf::from("/does/not/exist/02.dff"),
            ],
            track_audio_paths: vec![
                std::path::PathBuf::from("/does/not/exist/01.dff"),
                std::path::PathBuf::from("/does/not/exist/02.dff"),
            ],
            role: crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar,
            write_method: SidecarCueWriteMethod::UnsupportedCarriersSidecarOnly,
            cue_text: cue_text.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(cue_text),
        };

        let entries = read_transfer_carrier_entries(
            &carrier,
            super::super::app::TagTransferScope::All,
            &super::super::probe::MetadataWriteCancelFlag::new(),
        )
        .expect("CUE authority must not attempt to open unsupported carriers");

        let title = entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("TITLE from CUE");
        assert_eq!(title.per_file_values, vec!["Cue One", "Cue Two"]);
        let album = entries
            .iter()
            .find(|entry| entry.display_key == "ALBUM")
            .expect("ALBUM from CUE");
        assert_eq!(album.per_file_values, vec!["Cue Album", "Cue Album"]);
    }

    #[test]
    fn unsupported_sidecar_projection_includes_every_track_owned_by_selected_image() {
        let first = std::path::PathBuf::from("/does/not/exist/side-a.dff");
        let second = std::path::PathBuf::from("/does/not/exist/side-b.dff");
        let cue_text = r#"FILE "side-a.dff" WAVE
  TRACK 01 AUDIO
    TITLE "Side A One"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Side A Two"
    INDEX 01 03:00:00
FILE "side-b.dff" WAVE
  TRACK 03 AUDIO
    TITLE "Side B One"
    INDEX 01 00:00:00
"#;
        let carrier = TransferCarrier::SidecarCue {
            cue_path: std::path::PathBuf::from("/does/not/exist/album.cue"),
            image_paths: vec![first.clone()],
            track_audio_paths: vec![first.clone(), first, second],
            role: crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar,
            write_method: SidecarCueWriteMethod::UnsupportedCarriersSidecarOnly,
            cue_text: cue_text.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(cue_text),
        };

        assert_eq!(carrier.dimension(), TransferDimension::Tracks(2));
        assert_eq!(carrier.authored_track_numbers(), Some(vec![1, 2]));
        let entries = read_transfer_carrier_entries(
            &carrier,
            super::super::app::TagTransferScope::All,
            &super::super::probe::MetadataWriteCancelFlag::new(),
        )
        .expect("selected physical image must project every CUE track it owns");
        let title = entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("projected TITLE");
        assert_eq!(title.per_file_values, vec!["Side A One", "Side A Two"]);
    }

    #[test]
    fn unsupported_carrier_sidecar_write_blocks_audio_and_commits_only_cue() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("01.dff");
        let second = temp.path().join("02.dff");
        let cue = temp.path().join("album.cue");
        let first_bytes = b"not-a-real-dff-but-must-remain-byte-identical";
        let second_bytes = b"also-not-a-real-dff-and-must-remain-byte-identical";
        std::fs::write(&first, first_bytes).expect("first carrier");
        std::fs::write(&second, second_bytes).expect("second carrier");
        let original = concat!(
            "PERFORMER \"Old Artist\"\n",
            "TITLE \"Old Album\"\n",
            "FILE \"01.dff\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Old One\"\n",
            "    INDEX 01 00:00:00\n",
            "FILE \"02.dff\" WAVE\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Old Two\"\n",
            "    INDEX 01 00:00:00\n",
        );
        std::fs::write(&cue, original).expect("cue fixture");
        let target = TransferCarrier::SidecarCue {
            cue_path: cue.clone(),
            image_paths: vec![first.clone(), second.clone()],
            track_audio_paths: vec![first.clone(), second.clone()],
            role: crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar,
            write_method: SidecarCueWriteMethod::UnsupportedCarriersSidecarOnly,
            cue_text: original.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(original),
        };
        let source = vec![
            entry("ALBUM", &["New Album", "New Album"]),
            entry("TITLE", &["New One", "New Two"]),
            entry("ARTIST", &["New Artist", "New Artist"]),
        ];
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();

        let first_report = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("unsupported carriers must route writes to the sidecar");

        assert_eq!(first_report.written, 1);
        assert!(first_report.failed.is_empty());
        assert_eq!(first_report.blocked.len(), 2);
        assert_eq!(first_report.written_paths, vec![cue.clone()]);
        assert_eq!(first_report.target_paths, vec![cue.clone()]);
        assert_eq!(std::fs::read(&first).expect("first unchanged"), first_bytes);
        assert_eq!(std::fs::read(&second).expect("second unchanged"), second_bytes);
        let rewritten = std::fs::read_to_string(&cue).expect("rewritten cue");
        assert!(rewritten.contains("TITLE \"New Album\""));
        assert!(rewritten.contains("TITLE \"New One\""));
        assert!(rewritten.contains("TITLE \"New Two\""));
        let status = first_report.status();
        assert!(status.contains("sidecar CUE"));
        assert!(status.contains("2 carrier tag writes blocked/unsupported"));
        assert!(!status.contains("to 2 files"));

        let before_second = std::fs::read(&cue).expect("cue before idempotent retry");
        let second_report = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("idempotent sidecar retry");
        assert_eq!(second_report.written, 0);
        assert_eq!(second_report.unchanged, 1);
        assert_eq!(second_report.blocked.len(), 2);
        assert_eq!(std::fs::read(&cue).expect("cue after retry"), before_second);
        assert_eq!(std::fs::read(&first).expect("first still unchanged"), first_bytes);
        assert_eq!(std::fs::read(&second).expect("second still unchanged"), second_bytes);
    }

    #[test]
    fn embedded_flac_cue_transfer_round_trips_and_is_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("album.flac");
        let original = concat!(
            "REM COMMENT \"leave this alone\"\n",
            "TITLE \"Old Album\"\n",
            "FILE \"album.flac\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Old One\"\n",
            "    FLAGS PRE\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Old Two\"\n",
            "    INDEX 01 03:00:00\n",
        );
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&flac_block(0, false, &[0; 34]));
        bytes.extend_from_slice(&flac_block(
            4,
            false,
            &vorbis_block_body(&[("TITLE", "Image"), ("CUESHEET", original)]),
        ));
        bytes.extend_from_slice(&flac_block(1, true, &[0; 4096]));
        bytes.extend((0..4096).map(|index| (index % 251) as u8));
        std::fs::write(&image, bytes).expect("synthetic FLAC image");

        let target = TransferCarrier::EmbeddedCue {
            image_path: image.clone(),
            cue_text: original.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(original),
            multi_file_read_only: false,
        };
        let source = vec![
            entry("ALBUM", &["New Album", "New Album"]),
            entry("TITLE", &["New One", "New Two"]),
            entry("ARTIST", &["Artist One", "Artist Two"]),
        ];
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();

        let first = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("embedded FLAC CUE transfer");
        assert_eq!(first.written, 1);
        let first_status = first.status();
        assert!(first_status.starts_with("Wrote 3 fields to embedded CUE in"));
        assert!(first_status.contains(&image.display().to_string()));
        assert!(first_status.contains("1 rewritten, 0 unchanged, 0 failed"));
        let rewritten = super::super::probe::read_all_tags_merged(&[image.clone()])
            .expect("read embedded transfer result")
            .into_iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
            .and_then(|entry| entry.per_file_values.into_iter().next())
            .expect("embedded CUESHEET");
        assert!(rewritten.contains("REM COMMENT \"leave this alone\""));
        assert!(rewritten.contains("FILE \"album.flac\" WAVE"));
        assert!(rewritten.contains("FLAGS PRE"));
        assert!(rewritten.contains("INDEX 01 03:00:00"));
        assert!(rewritten.contains("TITLE \"New Album\""));
        assert!(rewritten.contains("TITLE \"New One\""));
        assert!(rewritten.contains("PERFORMER \"Artist Two\""));

        let before_second = std::fs::read(&image).expect("before second transfer");
        let second = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("idempotent embedded FLAC CUE transfer");
        assert_eq!(second.unchanged, 1);
        let second_status = second.status();
        assert!(second_status.starts_with("Embedded CUE in"));
        assert!(second_status.contains(&image.display().to_string()));
        assert!(second_status.contains("already matched 3 fields"));
        assert!(second_status.contains("0 rewritten, 1 unchanged, 0 failed"));
        assert!(!second_status.starts_with("Wrote"));
        assert_eq!(std::fs::read(&image).expect("after second transfer"), before_second);
    }

    #[test]
    fn embedded_wavpack_cue_transfer_uses_existing_metadata_writer() {
        let _xdg = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-embedded-wavpack-cue-transfer",
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("disc.wv");
        std::fs::write(&image, APE_NUMBERING_FIXTURE).expect("WavPack fixture");
        let original = concat!(
            "TITLE \"Old Album\"\n",
            "FILE \"disc.wv\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Old One\"\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Old Two\"\n",
            "    INDEX 01 03:00:00\n",
        );
        super::super::probe::write_all_tags(
            &image,
            &[(
                lofty::tag::ItemKey::Unknown("CUESHEET".to_string()),
                Some(original.to_string()),
            )],
        )
        .expect("seed WavPack embedded CUESHEET");

        let target = TransferCarrier::EmbeddedCue {
            image_path: image.clone(),
            cue_text: original.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(original),
            multi_file_read_only: false,
        };
        let report = execute_tag_transfer_from_entries_to_carrier(
            &[
                entry("ALBUM", &["New Album", "New Album"]),
                entry("TITLE", &["New One", "New Two"]),
            ],
            TransferDimension::Tracks(2),
            Some(&[1, 2]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &super::super::probe::MetadataWriteCancelFlag::new(),
            None,
        )
        .expect("embedded WavPack CUE transfer");
        assert_eq!(report.written, 1);
        assert!(report.failed.is_empty());

        let rewritten = super::super::probe::read_all_tags_merged(&[image])
            .expect("read WavPack transfer result")
            .into_iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
            .and_then(|entry| entry.per_file_values.into_iter().next())
            .expect("WavPack embedded CUESHEET");
        assert!(rewritten.contains("TITLE \"New Album\""));
        assert!(rewritten.contains("TITLE \"New One\""));
        assert!(rewritten.contains("TITLE \"New Two\""));
    }

    #[test]
    fn embedded_cue_set_transfer_writes_each_carrier_without_crossing_sheet_boundaries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut carriers = Vec::new();
        for (name, first_track) in [("disc-1.flac", 1_u32), ("disc-2.flac", 1_u32)] {
            let image = temp.path().join(name);
            let cue = format!(
                "TITLE \"Old Album\"\nFILE \"{name}\" FLAC\n  TRACK {first_track:02} AUDIO\n    TITLE \"Old First\"\n    INDEX 01 00:00:00\n  TRACK {:02} AUDIO\n    TITLE \"Old Second\"\n    INDEX 01 00:00:03\n",
                first_track + 1,
            );
            let mut bytes = b"fLaC".to_vec();
            bytes.extend_from_slice(&flac_block(0, false, &[0; 34]));
            bytes.extend_from_slice(&flac_block(
                4,
                false,
                &vorbis_block_body(&[("TITLE", "Image"), ("CUESHEET", cue.as_str())]),
            ));
            bytes.extend_from_slice(&flac_block(1, true, &[0; 4096]));
            bytes.extend((0..4096).map(|index| (index % 251) as u8));
            std::fs::write(&image, bytes).expect("synthetic FLAC image");
            carriers.push(EmbeddedCueCarrier {
                image_path: image,
                cue_text: cue.clone(),
                sheet: crate::convert::cue_parser::parse_cue(&cue),
                multi_file_read_only: false,
            });
        }
        let target = TransferCarrier::EmbeddedCues {
            carriers: carriers.clone(),
        };
        let source = vec![
            entry("ALBUM", &["New Album", "New Album", "New Album", "New Album"]),
            entry("TITLE", &["D1 One", "D1 Two", "D2 One", "D2 Two"]),
        ];
        let report = execute_tag_transfer_from_entries_to_carrier(
            &source,
            TransferDimension::Tracks(4),
            Some(&[1, 2, 3, 4]),
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &super::super::probe::MetadataWriteCancelFlag::new(),
            None,
        )
        .expect("embedded CUE set transfer");
        assert_eq!(report.written, 2);
        assert_eq!(report.target_paths.len(), 2);
        assert!(report.status().contains("2 embedded CUE carriers"));

        for (index, carrier) in carriers.iter().enumerate() {
            let cue = super::super::probe::read_all_tags_merged(&[carrier.image_path.clone()])
                .expect("read embedded transfer result")
                .into_iter()
                .find(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
                .and_then(|entry| entry.per_file_values.into_iter().next())
                .expect("embedded CUESHEET");
            assert!(cue.contains("TITLE \"New Album\""));
            if index == 0 {
                assert!(cue.contains("TITLE \"D1 One\""));
                assert!(cue.contains("TITLE \"D1 Two\""));
                assert!(!cue.contains("D2 One"));
            } else {
                assert!(cue.contains("TITLE \"D2 One\""));
                assert!(cue.contains("TITLE \"D2 Two\""));
                assert!(!cue.contains("D1 One"));
            }
        }
    }

    #[test]
    fn sidecar_write_re_admission_detects_target_identity_and_complete_track_geometry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("album.flac");
        let replacement_image = temp.path().join("replacement.flac");
        let cue = temp.path().join("album.cue");
        std::fs::write(&image, b"audio").expect("image");
        std::fs::write(&replacement_image, b"audio").expect("replacement image");
        let original = concat!(
            "FILE \"album.flac\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 01 03:00:00\n",
        );
        let expected_sheet = crate::convert::cue_parser::parse_cue(original);
        std::fs::write(&cue, original).expect("cue");
        validate_sidecar_transfer_snapshot(
            &cue,
            &std::fs::read_to_string(&cue).expect("read cue snapshot"),
            &[image.clone(), image.clone()],
            &expected_sheet,
        )
            .expect("unchanged target must re-admit");

        std::fs::write(
            &cue,
            concat!(
                "FILE \"replacement.flac\" WAVE\n",
                "  TRACK 01 AUDIO\n",
                "    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n",
                "    INDEX 01 03:00:00\n",
            ),
        )
        .expect("retarget cue");
        assert!(validate_sidecar_transfer_snapshot(
            &cue,
            &std::fs::read_to_string(&cue).expect("read cue snapshot"),
            &[image.clone(), image.clone()],
            &expected_sheet,
        )
            .expect_err("retargeted cue must refuse")
            .contains("no longer resolves to the expected per-track image set"));

        // Same-count variants trip the geometry check; the added-track
        // variant trips the earlier count/ownership check — a different
        // (equally fail-closed) refusal wording.
        for (changed, expected_refusal) in [
            (
                concat!(
                    "FILE \"album.flac\" WAVE\n",
                    "  TRACK 01 AUDIO\n",
                    "    INDEX 01 00:00:00\n",
                    "  TRACK 02 AUDIO\n",
                    "    INDEX 01 04:00:00\n",
                ),
                "changed track structure after classification",
            ),
            (
                concat!(
                    "FILE \"album.flac\" WAVE\n",
                    "  TRACK 01 AUDIO\n",
                    "    INDEX 01 00:00:00\n",
                    "  TRACK 03 AUDIO\n",
                    "    INDEX 01 03:00:00\n",
                ),
                "changed track structure after classification",
            ),
            (
                concat!(
                    "FILE \"album.flac\" WAVE\n",
                    "  TRACK 01 AUDIO\n",
                    "    INDEX 01 00:00:00\n",
                    "  TRACK 02 AUDIO\n",
                    "    INDEX 01 03:00:00\n",
                    "  TRACK 03 AUDIO\n",
                    "    INDEX 01 06:00:00\n",
                ),
                "changed track/image ownership after classification",
            ),
        ] {
            std::fs::write(&cue, changed).expect("change track geometry");
            assert!(validate_sidecar_transfer_snapshot(
            &cue,
            &std::fs::read_to_string(&cue).expect("read cue snapshot"),
            &[image.clone(), image.clone()],
            &expected_sheet,
        )
                .expect_err("any geometry change must refuse")
                .contains(expected_refusal));
        }
    }

    #[test]
    fn files_to_tracks_pairing_requires_the_exact_authored_number_sequence() {
        let paths = vec![
            std::path::PathBuf::from("/album/02.flac"),
            std::path::PathBuf::from("/album/03.flac"),
        ];
        let entries = vec![entry("TRACKNUMBER", &["2", "3"])];

        assert_eq!(
            corroborate_file_track_order(&paths, &entries, &[1, 2]).unwrap_err(),
            "file order and track numbers disagree; renumber or rename before transferring"
        );

        let warning = corroborate_file_track_order(
            &[
                std::path::PathBuf::from("/album/alpha.flac"),
                std::path::PathBuf::from("/album/beta.flac"),
            ],
            &[entry("TITLE", &["Alpha", "Beta"])],
            &[1, 2],
        )
        .expect("missing numbering is an honestly disclosed positional fallback")
        .expect("fallback warning");
        assert!(warning.contains("paired by filename order"));
    }

    #[test]
    fn editor_snapshot_retains_authored_cue_track_numbers_for_files_pairing() {
        let cue = concat!(
            "FILE \"b.flac\" WAVE\n",
            "  TRACK 07 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "FILE \"a.flac\" WAVE\n",
            "  TRACK 03 AUDIO\n",
            "    INDEX 01 00:00:00\n",
        );
        assert_eq!(
            editor_snapshot_authored_track_numbers(
                &[entry("CUESHEET", &[cue])],
                TransferDimension::Tracks(2),
            ),
            Some(vec![3, 7])
        );
        assert_eq!(
            editor_snapshot_authored_track_numbers(
                &[entry("CUESHEET", &[cue])],
                TransferDimension::Files(2),
            ),
            None
        );
    }

    #[test]
    fn embedded_target_refusals_surface_before_confirmation() {
        let source = vec![entry("TITLE", &["One", "Two"])];
        let sheet = crate::convert::cue_parser::parse_cue(concat!(
            "FILE \"album.wav\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n",
            "    INDEX 01 03:00:00\n",
        ));
        let read_only = TransferCarrier::EmbeddedCue {
            image_path: std::path::PathBuf::from("/album/album.mpc"),
            cue_text: String::new(),
            sheet: sheet.clone(),
            multi_file_read_only: false,
        };
        assert_eq!(
            preview_tag_transfer(
                &source,
                TransferDimension::Tracks(2),
                &read_only,
                super::super::app::TagTransferScope::All,
            )
            .unwrap_err(),
            "embedded CUE write is not supported for this audio carrier"
        );

        let multi_file = TransferCarrier::EmbeddedCue {
            image_path: std::path::PathBuf::from("/album/album.flac"),
            cue_text: String::new(),
            sheet,
            multi_file_read_only: true,
        };
        assert!(preview_tag_transfer(
            &source,
            TransferDimension::Tracks(2),
            &multi_file,
            super::super::app::TagTransferScope::All,
        )
        .unwrap_err()
        .contains("read-only transfer sources"));
    }

    #[test]
    fn metadata_sidecar_member_failure_leaves_sidecar_byte_identical() {
        let temp = tempfile::tempdir().expect("multi-FILE transfer tempdir");
        let first = temp.path().join("01.flac");
        let second = temp.path().join("02.flac");
        let alias = temp.path().join("02-alias.flac");
        let cue = temp.path().join("album.cue");
        write_test_flac(&first, "Old One");
        write_test_flac(&second, "Old Two");
        std::fs::hard_link(&second, &alias).expect("create hardlink refusal fixture");
        let cue_text = concat!(
            "FILE \"01.flac\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"Old One\"\n",
            "    INDEX 01 00:00:00\n",
            "FILE \"02.flac\" WAVE\n",
            "  TRACK 02 AUDIO\n",
            "    TITLE \"Old Two\"\n",
            "    INDEX 01 00:00:00\n",
        );
        std::fs::write(&cue, cue_text).expect("write multi-FILE CUE");
        let before = std::fs::read(&cue).expect("snapshot sidecar");
        let target = TransferCarrier::SidecarCue {
            cue_path: cue.clone(),
            image_paths: vec![first.clone(), second.clone()],
            track_audio_paths: vec![first.clone(), second.clone()],
            role: crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar,
            write_method: SidecarCueWriteMethod::PerFileAndSidecar,
            cue_text: cue_text.to_string(),
            sheet: crate::convert::cue_parser::parse_cue(cue_text),
        };
        let cancel = super::super::probe::MetadataWriteCancelFlag::new();
        let report = execute_tag_transfer_from_entries_to_carrier(
            &[entry("TITLE", &["New One", "New Two"])],
            TransferDimension::Files(2),
            None,
            &target,
            super::super::app::TagTransferScope::All,
            tui_file_picker::VerificationMode::Strong,
            &cancel,
            None,
        )
        .expect("member failure is reported in-band");

        assert_eq!(merged_value(&first, "TITLE").as_deref(), Some("New One"));
        assert_eq!(merged_value(&second, "TITLE").as_deref(), Some("Old Two"));
        assert!(report.failed.iter().any(|(path, _)| path == &second));
        assert!(report.failed.iter().any(|(path, reason)| {
            path == &cue && reason.contains("sidecar left unchanged")
        }));
        assert_eq!(std::fs::read(&cue).expect("re-read sidecar"), before);
    }

    #[test]
    fn aggregate_preview_slices_positions_without_crossing_representation_boundaries() {
        let cue_text = concat!(
            "TITLE \"Album\"\n",
            "FILE \"image.flac\" FLAC\n",
            "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            "  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n",
        );
        let target = TransferCarrier::Aggregate {
            carriers: vec![
                TransferCarrier::SidecarCue {
                    cue_path: std::path::PathBuf::from("/tmp/album.cue"),
                    image_paths: vec![std::path::PathBuf::from("/tmp/image.flac")],
                    track_audio_paths: vec![
                        std::path::PathBuf::from("/tmp/image.flac"),
                        std::path::PathBuf::from("/tmp/image.flac"),
                    ],
                    role: crate::convert::split_cue_album::SplitCueMemberRole::SyntheticAlbumPart,
                    write_method: SidecarCueWriteMethod::SidecarOnly,
                    cue_text: cue_text.to_string(),
                    sheet: crate::convert::cue_parser::parse_cue(cue_text),
                },
                TransferCarrier::Files {
                    paths: vec![std::path::PathBuf::from("/tmp/bonus.flac")],
                },
            ],
        };
        let source = vec![entry("TITLE", &["Cue One", "Cue Two", "Bonus"])];
        assert_eq!(target.dimension(), TransferDimension::Tracks(3));
        assert_eq!(
            preview_tag_transfer(
                &source,
                TransferDimension::Tracks(3),
                &target,
                super::super::app::TagTransferScope::All,
            )
            .expect("aggregate preview"),
            2,
            "the CUE and uncovered-file components each receive their own positional plan"
        );
        assert_eq!(
            preview_tag_transfer_fanout(
                &source,
                TransferDimension::Tracks(3),
                &target,
                super::super::app::TagTransferScope::All,
            )
            .expect("aggregate fanout preview"),
            (2, None)
        );
        assert!(preview_tag_transfer(
            &source,
            TransferDimension::Tracks(2),
            &target,
            super::super::app::TagTransferScope::All,
        )
        .unwrap_err()
        .contains("2 source positions and 3 target positions"));
    }
}

#[derive(Debug, Clone, Default)]
pub struct TagTransferReport {
    pub source_count: usize,
    pub target_count: usize,
    pub written: usize,
    pub unchanged: usize,
    pub written_fields: usize,
    pub source_carrier: Option<String>,
    pub target_carrier: Option<String>,
    pub target_paths: Vec<std::path::PathBuf>,
    pub first_track_collapse: bool,
    pub written_paths: Vec<std::path::PathBuf>,
    pub failed: Vec<(std::path::PathBuf, String)>,
    /// Per-carrier writes that were intentionally not attempted because the
    /// selected metadata representation proved the carrier cannot hold tags.
    /// These are informational, not transfer failures: a sidecar/other
    /// authoritative representation may still have committed successfully.
    pub blocked: Vec<(std::path::PathBuf, String)>,
    pub skipped_numbering_keys: Vec<String>,
    pub skipped_fields: Vec<String>,
    pub cardinality_warnings: Vec<String>,
    pub durability_warnings: Vec<String>,
}

impl TagTransferReport {
    fn cue_target_description(&self, target_carrier: &str) -> String {
        if target_carrier == "embedded CUE" && self.target_paths.len() > 1 {
            return format!("{} embedded CUE carriers", self.target_paths.len());
        }
        let path = self
            .target_paths
            .first()
            .map(|path| format!("'{}'", path.display()))
            .unwrap_or_else(|| "<unknown target>".to_string());
        match target_carrier {
            "embedded CUE" => format!("embedded CUE in {path}"),
            "sidecar CUE" => format!("sidecar CUE {path}"),
            other => format!("{other} {path}"),
        }
    }

    pub fn status(&self) -> String {
        let mut status = if let Some(target_carrier) = self.target_carrier.as_deref() {
            if target_carrier == "aggregate metadata" {
                format!(
                    "Transferred {} field application{} across {} logical position{} ({} written, {} unchanged, {} failed)",
                    self.written_fields,
                    if self.written_fields == 1 { "" } else { "s" },
                    self.target_count,
                    if self.target_count == 1 { "" } else { "s" },
                    self.written,
                    self.unchanged,
                    self.failed.len(),
                )
            } else if target_carrier == "files" {
                format!(
                    "Transferred {} field{} to {} file{} ({} written, {} unchanged, {} failed)",
                    self.written_fields,
                    if self.written_fields == 1 { "" } else { "s" },
                    self.target_count,
                    if self.target_count == 1 { "" } else { "s" },
                    self.written,
                    self.unchanged,
                    self.failed.len(),
                )
            } else {
                let target = self.cue_target_description(target_carrier);
                if self.written > 0 {
                    format!(
                        "Wrote {} field{} to {} ({} tracks; {} rewritten, {} unchanged, {} failed)",
                        self.written_fields,
                        if self.written_fields == 1 { "" } else { "s" },
                        target,
                        self.target_count,
                        self.written,
                        self.unchanged,
                        self.failed.len(),
                    )
                } else if self.unchanged > 0 {
                    let sentence_target = match target_carrier {
                        "embedded CUE" => target.replacen("embedded", "Embedded", 1),
                        "sidecar CUE" => target.replacen("sidecar", "Sidecar", 1),
                        _ => target,
                    };
                    format!(
                        "{} already matched {} field{} ({} tracks; {} rewritten, {} unchanged, {} failed)",
                        sentence_target,
                        self.written_fields,
                        if self.written_fields == 1 { "" } else { "s" },
                        self.target_count,
                        self.written,
                        self.unchanged,
                        self.failed.len(),
                    )
                } else {
                    format!(
                        "Failed to write {} field{} to {} ({} tracks; {} rewritten, {} unchanged, {} failed)",
                        self.written_fields,
                        if self.written_fields == 1 { "" } else { "s" },
                        target,
                        self.target_count,
                        self.written,
                        self.unchanged,
                        self.failed.len(),
                    )
                }
            }
        } else {
            format!(
                "Transferred tags to {} file{} ({} written, {} unchanged, {} failed)",
                self.target_count,
                if self.target_count == 1 { "" } else { "s" },
                self.written,
                self.unchanged,
                self.failed.len(),
            )
        };
        if !self.skipped_numbering_keys.is_empty() {
            status.push_str(&format!(
                "; 1-to-N skipped {}",
                self.skipped_numbering_keys.join(", ")
            ));
        }
        if !self.cardinality_warnings.is_empty() {
            status.push_str(&format!(
                "; {} cardinality warning{}",
                self.cardinality_warnings.len(),
                if self.cardinality_warnings.len() == 1 { "" } else { "s" }
            ));
        }
        if self.first_track_collapse {
            status.push_str("; wrote first-track values to single image");
        }
        if !self.skipped_fields.is_empty() {
            status.push_str(&format!("; {}", self.skipped_fields.join("; ")));
        }
        if !self.blocked.is_empty() {
            status.push_str(&format!(
                "; {} carrier tag write{} blocked/unsupported",
                self.blocked.len(),
                if self.blocked.len() == 1 { "" } else { "s" }
            ));
        }
        if let Some(source_carrier) = self.source_carrier.as_deref() {
            status.push_str(&format!("; source {source_carrier}"));
        }
        if !self.durability_warnings.is_empty() {
            status.push_str(&format!(
                "; {} durability warning{}",
                self.durability_warnings.len(),
                if self.durability_warnings.len() == 1 { "" } else { "s" }
            ));
        }
        status
    }
}

fn is_transfer_numbering_key(key: &str) -> bool {
    matches!(
        key,
        "TRACKNUMBER" | "TRACKTOTAL" | "DISCNUMBER" | "DISCTOTAL"
    )
}

fn transfer_entry_selected(
    entry: &TagEntry,
    scope: super::app::TagTransferScope,
    source_dimension: TransferDimension,
) -> bool {
    if entry.is_binary {
        return false;
    }
    if matches!(source_dimension, TransferDimension::Files(file_count) if entry.is_track_scoped(file_count)) {
        return false;
    }
    match scope {
        super::app::TagTransferScope::Canonical => {
            super::context_menu::tag_entry_matches_copy_selection(
                entry,
                super::context_menu::TagCopySelection::CanonicalOnly,
            )
        }
        super::app::TagTransferScope::All => true,
    }
}

fn is_cue_per_track_field(key: &str) -> bool {
    matches!(key, "TITLE" | "ARTIST" | "TRACKNUMBER" | "ISRC")
}

fn cue_target_key(key: &str) -> Option<&'static str> {
    match key {
        "TITLE" => Some("TITLE"),
        "ARTIST" => Some("PERFORMER"),
        "ALBUM" => Some("TITLE"),
        "ALBUMARTIST" => Some("PERFORMER"),
        "DATE" => Some("REM DATE"),
        "GENRE" => Some("REM GENRE"),
        "CATALOGNUMBER" => Some("CATALOG"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct PlannedTransferField {
    canonical_key: String,
    item_key: lofty::tag::ItemKey,
    values: Vec<super::probe::MetadataFieldValues>,
}

#[derive(Debug, Clone, Default)]
struct TransferValuePlan {
    fields: Vec<PlannedTransferField>,
    skipped_numbering_keys: Vec<String>,
    skipped_fields: Vec<String>,
    cardinality_warnings: Vec<String>,
    first_track_collapse: bool,
}

fn canonical_transfer_item_key(
    canonical_key: &str,
    source_item_key: &lofty::tag::ItemKey,
) -> lofty::tag::ItemKey {
    match super::probe::item_key_for_new_editor_row(canonical_key) {
        lofty::tag::ItemKey::Unknown(_) => source_item_key.clone(),
        typed => typed,
    }
}

fn plan_transfer_values_for_unified_cue_editor(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    scope: super::app::TagTransferScope,
    dimensions: super::probe::UnifiedCueDimensions,
    target_scopes: &std::collections::HashMap<String, super::probe::RowScope>,
) -> Result<TransferValuePlan, String> {
    let source_count = source_dimension.count();
    if source_count == 0 {
        return Err("tag transfer has no source positions".to_string());
    }
    if dimensions.files == 0 || dimensions.tracks == 0 {
        return Err("unified CUE target has no file or logical-track positions".to_string());
    }

    let mut plan = TransferValuePlan::default();
    let mut seen = std::collections::BTreeSet::new();
    for entry in source_entries {
        if !transfer_entry_selected(entry, scope, source_dimension) {
            continue;
        }
        let canonical_key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if !seen.insert(canonical_key.clone()) {
            return Err(format!(
                "tag transfer source contains duplicate field {canonical_key}"
            ));
        }
        if canonical_key == "SONGWRITER" {
            plan.skipped_fields
                .push("SONGWRITER excluded from CUE transfer this round".to_string());
            continue;
        }
        if canonical_key == "ISRC" {
            plan.skipped_fields
                .push("ISRC skipped: CUE writeback is read-only this round".to_string());
            continue;
        }
        if canonical_key == "TRACKNUMBER" {
            plan.skipped_fields
                .push("TRACKNUMBER skipped: CUE TRACK numbers are structural".to_string());
            continue;
        }
        if cue_target_key(&canonical_key).is_none() {
            plan.skipped_fields.push(format!(
                "{} skipped: not representable by the CUE field cap",
                canonical_key
            ));
            continue;
        }

        let declared_scope = target_scopes
            .get(&canonical_key)
            .copied()
            .unwrap_or_else(|| {
                if canonical_key == "ARTIST" {
                    super::probe::RowScope::Track
                } else {
                    super::probe::RowScope::File
                }
            });
        let shape = super::probe::unified_cue_row_shape(&canonical_key, declared_scope)
            .ok_or_else(|| format!("{canonical_key} has no unified-CUE row shape"))?;
        let target_count = shape.dimension(dimensions);

        let source_values = (0..source_count)
            .map(|source_index| {
                entry
                    .per_file_values
                    .get(source_index)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "{} has no source value at position {}",
                            canonical_key,
                            source_index + 1
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let all_source_same = source_values
            .windows(2)
            .all(|pair| pair[0] == pair[1]);

        let values = match shape.scope {
            super::probe::RowScope::File => {
                if matches!(source_dimension, TransferDimension::Files(_))
                    && source_count == target_count
                {
                    // Preserve legitimate per-carrier album values only when
                    // the source itself is file-dimensional. Equal cardinality
                    // alone is not enough: a track-dimensional source with the
                    // same count has no defined track->target-carrier mapping.
                    source_values.clone()
                } else if source_count == 1 || all_source_same {
                    vec![source_values[0].clone(); target_count]
                } else {
                    // Album-scoped values cannot be mapped positionally when
                    // source and target carrier shapes differ.  The value is
                    // semantically singular at album scope, so use a stable
                    // winner instead of rejecting a legitimate transfer.
                    plan.cardinality_warnings.push(format!(
                        "{} had conflicting album-scoped source values across {} positions; used the first source value for {} target carrier{}",
                        canonical_key,
                        source_count,
                        target_count,
                        if target_count == 1 { "" } else { "s" },
                    ));
                    vec![source_values[0].clone(); target_count]
                }
            }
            super::probe::RowScope::Track => {
                if matches!(source_dimension, TransferDimension::Files(1)) {
                    plan.skipped_fields.push(format!(
                        "{} skipped: one file cannot broadcast a per-track CUE field",
                        canonical_key
                    ));
                    continue;
                }
                if source_count != target_count {
                    return Err(format!(
                        "tag transfer carrier dimensions do not match for per-track {}: {} source positions and {} logical tracks",
                        canonical_key, source_count, target_count
                    ));
                }
                source_values.clone()
            }
        };

        if values.iter().any(|values| values.value_count() > 1) {
            plan.skipped_fields.push(format!(
                "{} skipped: CUE cannot represent repeated values without loss",
                canonical_key
            ));
            continue;
        }

        plan.fields.push(PlannedTransferField {
            item_key: canonical_transfer_item_key(&canonical_key, &entry.item_key),
            canonical_key,
            values,
        });
    }

    if plan.fields.is_empty()
        && plan.skipped_numbering_keys.is_empty()
        && plan.skipped_fields.is_empty()
    {
        return Err("tag transfer source contains no applicable text fields".to_string());
    }
    Ok(plan)
}

fn plan_transfer_values_for_dimensions(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    target_dimension: TransferDimension,
    scope: super::app::TagTransferScope,
) -> Result<TransferValuePlan, String> {
    plan_transfer_values_for_dimensions_with_collapse(
        source_entries,
        source_dimension,
        target_dimension,
        scope,
        FirstTrackCollapseEligibility::Forbidden,
    )
}

fn plan_transfer_values_for_dimensions_with_collapse(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    target_dimension: TransferDimension,
    scope: super::app::TagTransferScope,
    collapse_eligibility: FirstTrackCollapseEligibility,
) -> Result<TransferValuePlan, String> {
    let source_count = source_dimension.count();
    let target_count = target_dimension.count();
    if source_count == 0 {
        return Err("tag transfer has no source positions".to_string());
    }
    if target_count == 0 {
        return Err("tag transfer target has no positions".to_string());
    }

    let first_track_collapse = collapse_eligibility.permits()
        && matches!(target_dimension, TransferDimension::Files(1))
        && source_count > 1;
    let file_broadcast = matches!(source_dimension, TransferDimension::Files(1))
        && matches!(target_dimension, TransferDimension::Files(count) if count > 1);
    let one_file_to_tracks = matches!(source_dimension, TransferDimension::Files(1))
        && target_dimension.is_tracks();

    let cardinality_valid = match (source_dimension, target_dimension) {
        (TransferDimension::Files(1), TransferDimension::Files(_)) => true,
        (TransferDimension::Files(1), TransferDimension::Tracks(_)) => true,
        (TransferDimension::Files(source), TransferDimension::Files(1)) => {
            source == 1 || first_track_collapse
        }
        (TransferDimension::Files(source), TransferDimension::Files(target)) => source == target,
        (TransferDimension::Files(source), TransferDimension::Tracks(target)) => source == target,
        (TransferDimension::Tracks(source), TransferDimension::Files(1)) => {
            source == 1 || first_track_collapse
        }
        (TransferDimension::Tracks(source), TransferDimension::Files(target)) => source == target,
        (TransferDimension::Tracks(source), TransferDimension::Tracks(target)) => source == target,
    };
    if !cardinality_valid {
        return match (source_dimension, target_dimension) {
            (TransferDimension::Files(_), TransferDimension::Files(_)) => Err(format!(
                "tag transfer requires 1 source or equal source/target counts; got {} sources and {} targets",
                source_count, target_count
            )),
            _ => Err(format!(
                "tag transfer carrier dimensions do not match: {} source positions and {} target positions",
                source_count, target_count
            )),
        };
    }

    let mut plan = TransferValuePlan {
        first_track_collapse,
        ..TransferValuePlan::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    for entry in source_entries {
        if !transfer_entry_selected(entry, scope, source_dimension) {
            continue;
        }
        let canonical_key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if !seen.insert(canonical_key.clone()) {
            return Err(format!(
                "tag transfer source contains duplicate field {canonical_key}"
            ));
        }
        if file_broadcast && is_transfer_numbering_key(&canonical_key) {
            plan.skipped_numbering_keys.push(canonical_key);
            continue;
        }
        if one_file_to_tracks && is_cue_per_track_field(&canonical_key) {
            plan.skipped_fields.push(format!(
                "{} skipped: one file cannot broadcast a per-track CUE field",
                canonical_key
            ));
            continue;
        }
        if target_dimension.is_tracks() {
            if canonical_key == "SONGWRITER" {
                plan.skipped_fields
                    .push("SONGWRITER excluded from CUE transfer this round".to_string());
                continue;
            }
            if canonical_key == "ISRC" {
                plan.skipped_fields
                    .push("ISRC skipped: CUE writeback is read-only this round".to_string());
                continue;
            }
            if canonical_key == "TRACKNUMBER" {
                plan.skipped_fields
                    .push("TRACKNUMBER skipped: CUE TRACK numbers are structural".to_string());
                continue;
            }
            if cue_target_key(&canonical_key).is_none() {
                plan.skipped_fields.push(format!(
                    "{} skipped: not representable by the CUE field cap",
                    canonical_key
                ));
                continue;
            }
        }

        let mut values = Vec::with_capacity(target_count);
        let mut warned_stored_sources = std::collections::BTreeSet::new();
        for target_index in 0..target_count {
            let source_index = if first_track_collapse || file_broadcast || one_file_to_tracks {
                0
            } else {
                target_index
            };
            let value = entry.per_file_values.get(source_index).ok_or_else(|| {
                format!(
                    "{} has no source value at position {}",
                    canonical_key,
                    source_index + 1
                )
            })?;
            let stored_count = entry.stored_value_count_for_slot(source_index);
            if stored_count > value.value_count()
                && warned_stored_sources.insert(source_index)
            {
                plan.cardinality_warnings.push(format!(
                    "{} source {} exposes {} list values for {} stored instances",
                    canonical_key,
                    source_index + 1,
                    value.value_count(),
                    stored_count
                ));
            }
            values.push(value.clone());
        }
        if target_dimension.is_tracks()
            && values.iter().any(|values| values.value_count() > 1)
        {
            plan.skipped_fields.push(format!(
                "{} skipped: CUE cannot represent repeated values without loss",
                canonical_key
            ));
            continue;
        }
        if target_dimension.is_tracks()
            && !is_cue_per_track_field(&canonical_key)
            && values.windows(2).any(|pair| pair[0] != pair[1])
        {
            let first = values[0].clone();
            values.fill(first);
            plan.cardinality_warnings.push(format!(
                "{} is album-scoped in CUE; used the first source value",
                canonical_key
            ));
        }
        plan.fields.push(PlannedTransferField {
            item_key: canonical_transfer_item_key(&canonical_key, &entry.item_key),
            canonical_key,
            values,
        });
    }

    if plan.fields.is_empty()
        && plan.skipped_numbering_keys.is_empty()
        && plan.skipped_fields.is_empty()
    {
        return Err("tag transfer source contains no applicable text fields".to_string());
    }
    Ok(plan)
}

#[cfg(test)] // superseded by the prepared/classified round-7 flow; exercised by regression tests
fn plan_transfer_values(
    source_entries: &[TagEntry],
    source_count: usize,
    target_count: usize,
    scope: super::app::TagTransferScope,
) -> Result<TransferValuePlan, String> {
    plan_transfer_values_for_dimensions(
        source_entries,
        TransferDimension::Files(source_count),
        TransferDimension::Files(target_count),
        scope,
    )
}


fn cue_transfer_entry<V>(
    display_key: &str,
    item_key: lofty::tag::ItemKey,
    values: Vec<V>,
) -> Option<TagEntry>
where
    V: Into<super::probe::MetadataFieldValues>,
{
    let values: Vec<super::probe::MetadataFieldValues> = values.into_iter().map(Into::into).collect();
    if values.is_empty() || values.iter().all(|value| value.as_str().is_empty()) {
        return None;
    }
    let is_mixed = values.windows(2).any(|pair| pair[0] != pair[1]);
    let value = if is_mixed {
        "<multiple values>".to_string()
    } else {
        values
            .first()
            .map(|value| value.as_str().to_string())
            .unwrap_or_default()
    };
    let stored_value_counts = values
        .iter()
        .map(super::probe::MetadataFieldValues::value_count)
        .collect();
    Some(TagEntry {
        display_key: display_key.to_string(),
        item_key,
        value: value.clone(),
        original: value,
        is_binary: false,
        is_mixed,
        has_multiple_stored_values: values.iter().any(|value| value.value_count() > 1),
        row_scope: super::probe::RowScope::Track,
        per_file_stored_value_counts: stored_value_counts,
        per_file_originals: values.clone(),
        per_file_values: values,
        mb_proposed_value: None,
        mb_proposed_per_file: None,
    })
}

/// Convert a parsed CUE into transfer rows. Track pairing is positional after
/// sorting by authored TRACK number; gapped and non-one-based numbers are
/// intentionally accepted. CUESHEET itself is never surfaced as a field.
pub(crate) fn cue_sheet_transfer_entries(
    sheet: &crate::convert::cue_parser::CueSheet,
) -> Vec<TagEntry> {
    let mut tracks = sheet.tracks.iter().collect::<Vec<_>>();
    tracks.sort_by_key(|track| track.number);
    let track_count = tracks.len();
    let repeated = |value: &Option<String>| {
        vec![value.clone().unwrap_or_default(); track_count]
    };
    let mut entries = Vec::new();
    let candidates = [
        cue_transfer_entry(
            "TITLE",
            lofty::tag::ItemKey::TrackTitle,
            tracks
                .iter()
                .map(|track| track.title.clone().unwrap_or_default())
                .collect(),
        ),
        cue_transfer_entry(
            "ARTIST",
            lofty::tag::ItemKey::TrackArtist,
            tracks
                .iter()
                .map(|track| track.performer.clone().unwrap_or_default())
                .collect(),
        ),
        cue_transfer_entry(
            "ISRC",
            lofty::tag::ItemKey::Isrc,
            tracks
                .iter()
                .map(|track| track.isrc.clone().unwrap_or_default())
                .collect(),
        ),
        cue_transfer_entry("ALBUM", lofty::tag::ItemKey::AlbumTitle, repeated(&sheet.title)),
        cue_transfer_entry(
            "ALBUMARTIST",
            lofty::tag::ItemKey::AlbumArtist,
            repeated(&sheet.performer),
        ),
        cue_transfer_entry("DATE", lofty::tag::ItemKey::Year, repeated(&sheet.date)),
        cue_transfer_entry("GENRE", lofty::tag::ItemKey::Genre, repeated(&sheet.genre)),
        cue_transfer_entry(
            "CATALOGNUMBER",
            lofty::tag::ItemKey::CatalogNumber,
            repeated(&sheet.catalog),
        ),
    ];
    entries.extend(candidates.into_iter().flatten());
    entries
}

fn embedded_cue_set_transfer_entries(carriers: &[EmbeddedCueCarrier]) -> Vec<TagEntry> {
    let specs = [
        ("TITLE", lofty::tag::ItemKey::TrackTitle),
        ("ARTIST", lofty::tag::ItemKey::TrackArtist),
        ("ISRC", lofty::tag::ItemKey::Isrc),
        ("ALBUM", lofty::tag::ItemKey::AlbumTitle),
        ("ALBUMARTIST", lofty::tag::ItemKey::AlbumArtist),
        ("DATE", lofty::tag::ItemKey::Year),
        ("GENRE", lofty::tag::ItemKey::Genre),
        ("CATALOGNUMBER", lofty::tag::ItemKey::CatalogNumber),
    ];
    specs
        .into_iter()
        .filter_map(|(display_key, item_key)| {
            let mut values = Vec::new();
            for carrier in carriers {
                let track_count = carrier.sheet.tracks.len();
                let local = cue_sheet_transfer_entries(&carrier.sheet);
                if let Some(entry) = local
                    .iter()
                    .find(|entry| entry.display_key.eq_ignore_ascii_case(display_key))
                {
                    values.extend(entry.per_file_values.iter().cloned());
                } else {
                    values.extend(
                        std::iter::repeat(super::probe::MetadataFieldValues::default())
                            .take(track_count),
                    );
                }
            }
            cue_transfer_entry(display_key, item_key, values)
        })
        .collect()
}


fn transfer_value_summary(values: &[super::probe::MetadataFieldValues]) -> (String, bool) {
    let is_mixed = values.windows(2).any(|pair| pair[0] != pair[1]);
    let value = if is_mixed {
        "<multiple values>".to_string()
    } else {
        values
            .first()
            .map(|value| value.as_str().to_string())
            .unwrap_or_default()
    };
    (value, is_mixed)
}

fn normalize_transfer_entry(entry: &mut TagEntry) {
    let (value, is_mixed) = transfer_value_summary(&entry.per_file_values);
    let (original, _) = transfer_value_summary(&entry.per_file_originals);
    entry.value = value;
    entry.original = original;
    entry.is_mixed = is_mixed;
    entry.has_multiple_stored_values = entry
        .per_file_stored_value_counts
        .iter()
        .any(|count| *count > 1);
    entry.row_scope = super::probe::RowScope::Track;
    entry.mb_proposed_value = None;
    entry.mb_proposed_per_file = None;
}

/// Merge independently read carrier segments into one logical positional
/// transfer surface. A field absent from a segment contributes empty values for
/// that segment, matching the ordinary merged-file reader's absent-tag
/// semantics without allowing one representation to absorb another.
fn merge_transfer_entry_segments(
    segments: Vec<(Vec<TagEntry>, usize)>,
) -> Result<Vec<TagEntry>, String> {
    use std::collections::{BTreeMap, BTreeSet};

    let total = segments.iter().map(|(_, count)| *count).sum::<usize>();
    if total == 0 {
        return Err("tag transfer aggregate has no source positions".to_string());
    }

    let mut ordered_keys = Vec::new();
    let mut templates = BTreeMap::<String, TagEntry>::new();
    let mut normalized_segments = Vec::with_capacity(segments.len());
    for (entries, count) in segments {
        let mut seen = BTreeSet::new();
        let mut local = BTreeMap::new();
        for entry in entries {
            let key = super::probe::canonical_metadata_display_key(&entry.display_key);
            if !seen.insert(key.clone()) {
                return Err(format!(
                    "tag transfer source contains duplicate field {key}"
                ));
            }
            if entry.per_file_values.len() != count {
                return Err(format!(
                    "tag transfer source field {key} has {} values for {count} positions",
                    entry.per_file_values.len()
                ));
            }
            if !entry.per_file_originals.is_empty()
                && entry.per_file_originals.len() != count
            {
                return Err(format!(
                    "tag transfer source field {key} has {} original values for {count} positions",
                    entry.per_file_originals.len()
                ));
            }
            if !templates.contains_key(&key) {
                ordered_keys.push(key.clone());
                templates.insert(key.clone(), entry.clone());
            }
            local.insert(key, entry);
        }
        normalized_segments.push((local, count));
    }

    let mut merged = Vec::with_capacity(ordered_keys.len());
    for key in ordered_keys {
        let mut entry = templates
            .remove(&key)
            .ok_or_else(|| "internal error: aggregate transfer template missing".to_string())?;
        entry.per_file_values.clear();
        entry.per_file_originals.clear();
        entry.per_file_stored_value_counts.clear();
        for (local, count) in &normalized_segments {
            if let Some(segment) = local.get(&key) {
                entry
                    .per_file_values
                    .extend(segment.per_file_values.iter().cloned());
                if segment.per_file_originals.is_empty() {
                    entry
                        .per_file_originals
                        .extend(segment.per_file_values.iter().cloned());
                } else {
                    entry
                        .per_file_originals
                        .extend(segment.per_file_originals.iter().cloned());
                }
                for index in 0..*count {
                    entry
                        .per_file_stored_value_counts
                        .push(segment.stored_value_count_for_slot(index));
                }
            } else {
                entry
                    .per_file_values
                    .extend(std::iter::repeat_with(crate::tui::probe::MetadataFieldValues::default).take(*count));
                entry
                    .per_file_originals
                    .extend(std::iter::repeat_with(crate::tui::probe::MetadataFieldValues::default).take(*count));
                entry
                    .per_file_stored_value_counts
                    .extend(std::iter::repeat(0).take(*count));
            }
        }
        normalize_transfer_entry(&mut entry);
        merged.push(entry);
    }
    Ok(merged)
}

fn slice_transfer_entries(
    entries: &[TagEntry],
    start: usize,
    count: usize,
) -> Result<Vec<TagEntry>, String> {
    let end = start
        .checked_add(count)
        .ok_or_else(|| "tag transfer aggregate position overflow".to_string())?;
    entries
        .iter()
        .map(|entry| {
            if entry.per_file_values.len() < end {
                return Err(format!(
                    "tag transfer source field {} has {} values but aggregate segment requires positions {} through {}",
                    super::probe::canonical_metadata_display_key(&entry.display_key),
                    entry.per_file_values.len(),
                    start + 1,
                    end
                ));
            }
            let mut sliced = entry.clone();
            sliced.per_file_values = entry.per_file_values[start..end].to_vec();
            sliced.per_file_originals = if entry.per_file_originals.len() >= end {
                entry.per_file_originals[start..end].to_vec()
            } else {
                sliced.per_file_values.clone()
            };
            sliced.per_file_stored_value_counts = (start..end)
                .map(|index| entry.stored_value_count_for_slot(index))
                .collect();
            normalize_transfer_entry(&mut sliced);
            Ok(sliced)
        })
        .collect()
}

fn unsupported_sidecar_selected_track_indices_unchecked(
    selected_image_paths: &[std::path::PathBuf],
    track_audio_paths: &[std::path::PathBuf],
) -> Vec<usize> {
    let selected = selected_image_paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    track_audio_paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| selected.contains(path).then_some(index))
        .collect()
}

fn unsupported_sidecar_selected_track_indices(
    selected_image_paths: &[std::path::PathBuf],
    track_audio_paths: &[std::path::PathBuf],
    sheet: &crate::convert::cue_parser::CueSheet,
) -> Result<Vec<usize>, String> {
    if selected_image_paths.is_empty() {
        return Err(
            "tag transfer unsupported-sidecar selection contains no audio carriers".to_string(),
        );
    }
    if track_audio_paths.len() != sheet.tracks.len() {
        return Err(
            "tag transfer sidecar track/image ownership cardinality mismatch".to_string(),
        );
    }
    let selected = selected_image_paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if selected.len() != selected_image_paths.len() {
        return Err(
            "tag transfer unsupported-sidecar selection contains duplicate audio carriers"
                .to_string(),
        );
    }
    let mut matched = std::collections::BTreeSet::new();
    let mut indices = Vec::new();
    for (index, path) in track_audio_paths.iter().enumerate() {
        if selected.contains(path) {
            matched.insert(path.clone());
            indices.push(index);
        }
    }
    if matched != selected {
        let missing = selected
            .difference(&matched)
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "tag transfer unsupported-sidecar selection is not fully represented by the admitted CUE: {missing}"
        ));
    }
    if indices.is_empty() {
        return Err(
            "tag transfer unsupported-sidecar selection maps to no authored CUE tracks".to_string(),
        );
    }
    Ok(indices)
}

fn project_transfer_entries(
    entries: &[TagEntry],
    indices: &[usize],
) -> Result<Vec<TagEntry>, String> {
    entries
        .iter()
        .map(|entry| {
            let mut projected = entry.clone();
            projected.per_file_values = indices
                .iter()
                .map(|index| {
                    entry.per_file_values.get(*index).cloned().ok_or_else(|| {
                        format!(
                            "tag transfer source field {} has no CUE value at authored position {}",
                            super::probe::canonical_metadata_display_key(&entry.display_key),
                            index + 1
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            projected.per_file_originals = indices
                .iter()
                .map(|index| {
                    entry
                        .per_file_originals
                        .get(*index)
                        .cloned()
                        .or_else(|| entry.per_file_values.get(*index).cloned())
                        .ok_or_else(|| {
                            format!(
                                "tag transfer source field {} has no original CUE value at authored position {}",
                                super::probe::canonical_metadata_display_key(&entry.display_key),
                                index + 1
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            projected.per_file_stored_value_counts = indices
                .iter()
                .map(|index| entry.stored_value_count_for_slot(*index))
                .collect();
            normalize_transfer_entry(&mut projected);
            Ok(projected)
        })
        .collect()
}

fn aggregate_source_segment(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    target_dimension: TransferDimension,
    start: usize,
    count: usize,
    aggregate_count: usize,
) -> Result<(Vec<TagEntry>, TransferDimension), String> {
    match source_dimension {
        TransferDimension::Files(1) => Ok((source_entries.to_vec(), source_dimension)),
        TransferDimension::Files(source_count) | TransferDimension::Tracks(source_count)
            if source_count == aggregate_count =>
        {
            if target_dimension.count() != count {
                return Err(format!(
                    "tag transfer aggregate segment has {} positions but its carrier has {}",
                    count,
                    target_dimension.count()
                ));
            }
            let segment_dimension = match source_dimension {
                TransferDimension::Files(_) => TransferDimension::Files(count),
                TransferDimension::Tracks(_) => TransferDimension::Tracks(count),
            };
            Ok((
                slice_transfer_entries(source_entries, start, count)?,
                segment_dimension,
            ))
        }
        _ => Err(format!(
            "tag transfer carrier dimensions do not match: {} source positions and {} target positions",
            source_dimension.count(),
            aggregate_count
        )),
    }
}


fn expand_image_entries_to_cue_tracks(
    file_entries: Vec<TagEntry>,
    image_paths: &[std::path::PathBuf],
    track_audio_paths: &[std::path::PathBuf],
) -> Result<Vec<TagEntry>, String> {
    let mut image_index = std::collections::BTreeMap::new();
    for (index, path) in image_paths.iter().enumerate() {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if image_index.insert(key, index).is_some() {
            return Err(format!(
                "tag transfer synthetic CUE contains duplicate image '{}'",
                path.display()
            ));
        }
    }
    let mut track_indices = Vec::with_capacity(track_audio_paths.len());
    for path in track_audio_paths {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        track_indices.push(*image_index.get(&key).ok_or_else(|| {
            format!(
                "tag transfer synthetic CUE track references unclassified image '{}'",
                path.display()
            )
        })?);
    }

    let mut expanded = Vec::new();
    for mut entry in file_entries {
        let key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if key == "CUESHEET" || is_cue_per_track_field(&key) {
            continue;
        }
        if entry.per_file_values.len() != image_paths.len() {
            return Err(format!(
                "tag transfer image field {key} has {} values for {} images",
                entry.per_file_values.len(),
                image_paths.len()
            ));
        }
        let source_values = entry.per_file_values.clone();
        let source_originals = entry.per_file_originals.clone();
        let source_counts = (0..image_paths.len())
            .map(|index| entry.stored_value_count_for_slot(index))
            .collect::<Vec<_>>();
        entry.per_file_values = track_indices
            .iter()
            .map(|index| source_values[*index].clone())
            .collect();
        entry.per_file_originals = track_indices
            .iter()
            .map(|index| {
                source_originals
                    .get(*index)
                    .cloned()
                    .unwrap_or_else(|| source_values[*index].clone())
            })
            .collect();
        entry.per_file_stored_value_counts = track_indices
            .iter()
            .map(|index| source_counts[*index])
            .collect();
        normalize_transfer_entry(&mut entry);
        expanded.push(entry);
    }
    Ok(expanded)
}

fn overlay_cue_entries_on_file_entries(
    file_entries: Vec<TagEntry>,
    cue_entries: Vec<TagEntry>,
    count: usize,
) -> Result<Vec<TagEntry>, String> {
    let mut merged = merge_transfer_entry_segments(vec![(file_entries, count)])?;
    let mut by_key = merged
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            (
                super::probe::canonical_metadata_display_key(&entry.display_key),
                index,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for cue in cue_entries {
        let key = super::probe::canonical_metadata_display_key(&cue.display_key);
        if cue.per_file_values.len() != count {
            return Err(format!(
                "tag transfer CUE field {key} has {} values for {count} tracks",
                cue.per_file_values.len()
            ));
        }
        if let Some(index) = by_key.get(&key).copied() {
            let entry = &mut merged[index];
            entry.item_key = cue.item_key.clone();
            for position in 0..count {
                let value = &cue.per_file_values[position];
                if value.is_empty() {
                    continue;
                }
                entry.per_file_values[position] = value.clone();
                entry.per_file_originals[position] = value.clone();
                entry.per_file_stored_value_counts[position] =
                    cue.stored_value_count_for_slot(position).max(1);
            }
            normalize_transfer_entry(entry);
        } else {
            let mut cue = cue;
            normalize_transfer_entry(&mut cue);
            by_key.insert(key, merged.len());
            merged.push(cue);
        }
    }
    Ok(merged)
}

pub(crate) fn read_transfer_carrier_entries(
    carrier: &TransferCarrier,
    scope: super::app::TagTransferScope,
    cancel: &super::probe::MetadataWriteCancelFlag,
) -> Result<Vec<TagEntry>, String> {
    match carrier {
        TransferCarrier::Files { paths } => read_transfer_source_entries(paths, scope, cancel),
        TransferCarrier::SidecarCue {
            image_paths,
            track_audio_paths,
            role,
            write_method,
            sheet,
            ..
        } => {
            let cue_entries = cue_sheet_transfer_entries(sheet)
                .into_iter()
                .filter(|entry| match scope {
                    super::app::TagTransferScope::Canonical => {
                        super::context_menu::tag_entry_matches_copy_selection(
                            entry,
                            super::context_menu::TagCopySelection::CanonicalOnly,
                        )
                    }
                    super::app::TagTransferScope::All => true,
                })
                .collect::<Vec<_>>();
            if track_audio_paths.len() != sheet.tracks.len() {
                return Err(
                    "tag transfer sidecar track/image ownership cardinality mismatch".to_string(),
                );
            }
            if write_method.is_unsupported_authority() {
                // Classification already established that the physical audio
                // carriers cannot provide metadata through lofty. The CUE is
                // therefore the complete readable metadata authority for this
                // transfer representation. Explicit file selections may be a
                // strict subset of the album, so expose only the authored CUE
                // tracks owned by the selected unsupported carriers. The audio
                // files are never reopened for field data.
                let selected_track_indices = unsupported_sidecar_selected_track_indices(
                    image_paths,
                    track_audio_paths,
                    sheet,
                )?;
                return project_transfer_entries(&cue_entries, &selected_track_indices);
            }
            match role {
                crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar => {
                    let file_entries =
                        read_transfer_source_entries(track_audio_paths, scope, cancel)?;
                    overlay_cue_entries_on_file_entries(
                        file_entries,
                        cue_entries,
                        sheet.tracks.len(),
                    )
                }
                crate::convert::split_cue_album::SplitCueMemberRole::SyntheticAlbumPart => {
                    let file_entries =
                        read_transfer_source_entries(image_paths, scope, cancel)?;
                    let track_entries = expand_image_entries_to_cue_tracks(
                        file_entries,
                        image_paths,
                        track_audio_paths,
                    )?;
                    overlay_cue_entries_on_file_entries(
                        track_entries,
                        cue_entries,
                        sheet.tracks.len(),
                    )
                }
            }
        }
        TransferCarrier::EmbeddedCue { sheet, .. } => {
            let entries = cue_sheet_transfer_entries(sheet);
            Ok(entries
                .into_iter()
                .filter(|entry| match scope {
                    super::app::TagTransferScope::Canonical => {
                        super::context_menu::tag_entry_matches_copy_selection(
                            entry,
                            super::context_menu::TagCopySelection::CanonicalOnly,
                        )
                    }
                    super::app::TagTransferScope::All => true,
                })
                .collect())
        }
        TransferCarrier::EmbeddedCues { carriers } => {
            let entries = embedded_cue_set_transfer_entries(carriers);
            Ok(entries
                .into_iter()
                .filter(|entry| match scope {
                    super::app::TagTransferScope::Canonical => {
                        super::context_menu::tag_entry_matches_copy_selection(
                            entry,
                            super::context_menu::TagCopySelection::CanonicalOnly,
                        )
                    }
                    super::app::TagTransferScope::All => true,
                })
                .collect())
        }
        TransferCarrier::Aggregate { carriers } => {
            if carriers.is_empty() {
                return Err("internal error: aggregate transfer carrier is empty".to_string());
            }
            let mut segments = Vec::with_capacity(carriers.len());
            for child in carriers {
                segments.push((
                    read_transfer_carrier_entries(child, scope, cancel)?,
                    child.count(),
                ));
            }
            merge_transfer_entry_segments(segments)
        }
    }
}

pub(crate) fn read_transfer_source_entries(
    source_paths: &[std::path::PathBuf],
    scope: super::app::TagTransferScope,
    cancel: &super::probe::MetadataWriteCancelFlag,
) -> Result<Vec<TagEntry>, String> {
    let merged = super::probe::read_all_tags_merged_with_metadata_cancellable_for_operation(
        source_paths,
        || cancel.is_cancelled(),
        "tag transfer cancelled while reading source metadata",
    )?;
    let source_failures = merged
        .metadata_errors
        .iter()
        .enumerate()
        .filter_map(|(index, issue)| {
            issue
                .as_ref()
                .filter(|issue| issue.blocks_metadata_use())
                .map(|issue| (index, issue))
        })
        .collect::<Vec<_>>();
    if !source_failures.is_empty() {
        let (index, issue) = source_failures[0];
        return Err(format!(
            "tag transfer source read failed for '{}' ({} of {} sources unreadable): {}",
            source_paths
                .get(index)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| format!("source {}", index + 1)),
            source_failures.len(),
            source_paths.len(),
            issue.reason
        ));
    }
    Ok(merged
        .entries
        .into_iter()
        .filter(|entry| transfer_entry_selected(entry, scope, TransferDimension::Files(source_paths.len())))
        .collect())
}

fn parse_transfer_track_number(value: &str) -> Option<u32> {
    value
        .trim()
        .split_once('/')
        .map(|(number, _)| number)
        .unwrap_or_else(|| value.trim())
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0)
}

fn carrier_authored_track_numbers(carrier: &TransferCarrier) -> Option<Vec<u32>> {
    carrier.authored_track_numbers()
}

fn corroborate_file_track_order(
    paths: &[std::path::PathBuf],
    entries: &[TagEntry],
    expected_track_numbers: &[u32],
) -> Result<Option<String>, String> {
    if paths.len() != expected_track_numbers.len() {
        return Err(
            "internal error: file/track corroboration cardinality mismatch".to_string(),
        );
    }
    let tracknumber = entries.iter().find(|entry| {
        super::probe::canonical_metadata_display_key(&entry.display_key) == "TRACKNUMBER"
    });
    let numbers = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let tagged = tracknumber
                .and_then(|entry| entry.per_file_values.get(index))
                .map(super::probe::MetadataFieldValues::as_str)
                .filter(|value| !value.trim().is_empty());
            match tagged {
                // A present TRACKNUMBER is authoritative, including an
                // invalid value: do not conceal it with a filename fallback.
                Some(value) => parse_transfer_track_number(value),
                None => crate::convert::processor::strict_track_number_from_dispatch_path(path)
                    .or_else(|| crate::tui::preemphasis::metadata::leading_track_number(path)),
            }
        })
        .collect::<Vec<_>>();

    if numbers.iter().all(Option::is_some) {
        let ordered = numbers
            .iter()
            .map(|number| number.expect("all numbers checked above"))
            .collect::<Vec<_>>();
        if ordered != expected_track_numbers {
            return Err(
                "file order and track numbers disagree; renumber or rename before transferring"
                    .to_string(),
            );
        }
        Ok(None)
    } else {
        Ok(Some(
            "paired by filename order; no complete track-number sequence was available to corroborate the pairing"
                .to_string(),
        ))
    }
}

pub(crate) fn corroborate_source_pairing(
    source: &TransferCarrier,
    target: &TransferCarrier,
    source_entries: &[TagEntry],
) -> Result<Option<String>, String> {
    match (source, target) {
        (
            TransferCarrier::Files { paths },
            TransferCarrier::SidecarCue { .. }
            | TransferCarrier::EmbeddedCue { .. }
            | TransferCarrier::EmbeddedCues { .. },
        ) if paths.len() == target.count() && paths.len() > 1 => {
            let expected = carrier_authored_track_numbers(target)
                .ok_or_else(|| "internal error: CUE target has no authored track order".to_string())?;
            corroborate_file_track_order(paths, source_entries, &expected)
        }
        _ => Ok(None),
    }
}

#[cfg(test)] // superseded by the prepared/classified round-7 flow; exercised by regression tests
pub(crate) fn execute_tag_transfer_from_entries(
    source_entries: &[TagEntry],
    source_count: usize,
    target_paths: &[std::path::PathBuf],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    execute_tag_transfer_from_entries_with_writer(
        source_entries,
        source_count,
        target_paths,
        scope,
        verification,
        cancel,
        progress,
        |path, changes, cancel, verification| {
            super::probe::write_tag_value_lists_for_transfer_at_verification(
                path,
                changes,
                cancel,
                verification,
            )
        },
    )
}

#[cfg(test)] // superseded by the prepared/classified round-7 flow; exercised by regression tests
fn execute_tag_transfer_from_entries_with_writer<F>(
    source_entries: &[TagEntry],
    source_count: usize,
    target_paths: &[std::path::PathBuf],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
    writer: F,
) -> Result<TagTransferReport, String>
where
    F: FnMut(
        &std::path::Path,
        &[(lofty::tag::ItemKey, Option<super::probe::MetadataFieldValues>, bool)],
        Option<&super::probe::MetadataWriteCancelFlag>,
        tui_file_picker::VerificationMode,
    ) -> Result<super::probe::MetadataWriteCommitReport, String>,
{
    execute_tag_transfer_to_files_with_writer(
        source_entries,
        TransferDimension::Files(source_count),
        None,
        target_paths,
        scope,
        verification,
        cancel,
        progress,
        writer,
    )
}

fn execute_tag_transfer_to_files_with_writer<F>(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    source_track_numbers: Option<&[u32]>,
    target_paths: &[std::path::PathBuf],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
    mut writer: F,
) -> Result<TagTransferReport, String>
where
    F: FnMut(
        &std::path::Path,
        &[(lofty::tag::ItemKey, Option<super::probe::MetadataFieldValues>, bool)],
        Option<&super::probe::MetadataWriteCancelFlag>,
        tui_file_picker::VerificationMode,
    ) -> Result<super::probe::MetadataWriteCommitReport, String>,
{
    let value_plan = plan_transfer_values_for_dimensions(
        source_entries,
        source_dimension,
        TransferDimension::Files(target_paths.len()),
        scope,
    )?;
    if cancel.is_cancelled() {
        return Err("tag transfer cancelled before target read".to_string());
    }

    let target_merged =
        super::probe::read_all_tags_merged_with_metadata_cancellable_for_operation(
            target_paths,
            || cancel.is_cancelled(),
            "tag transfer cancelled while reading target metadata",
        )?;

    let target_pairing_warning = if source_dimension.is_tracks()
        && source_dimension.count() == target_paths.len()
        && target_paths.len() > 1
    {
        match source_track_numbers {
            Some(expected) => {
                corroborate_file_track_order(target_paths, &target_merged.entries, expected)?
            }
            None => Some(
                "paired by filename order; the track-dimensional source did not expose authored track numbers for corroboration"
                    .to_string(),
            ),
        }
    } else {
        None
    };

    let mut report = TagTransferReport {
        source_count: source_dimension.count(),
        target_count: target_paths.len(),
        written_fields: value_plan.fields.len(),
        source_carrier: Some(if source_dimension.is_tracks() {
            "CUE tracks".to_string()
        } else {
            "files".to_string()
        }),
        target_carrier: Some("files".to_string()),
        target_paths: target_paths.to_vec(),
        first_track_collapse: value_plan.first_track_collapse,
        skipped_numbering_keys: value_plan.skipped_numbering_keys,
        skipped_fields: value_plan.skipped_fields,
        cardinality_warnings: value_plan.cardinality_warnings,
        ..TagTransferReport::default()
    };
    if let Some(warning) = target_pairing_warning {
        report.cardinality_warnings.push(warning);
    }

    let mut target_by_key = std::collections::HashMap::new();
    for entry in &target_merged.entries {
        let key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if target_by_key.insert(key.clone(), entry).is_some() {
            return Err(format!(
                "tag transfer target metadata contains duplicate field {key}"
            ));
        }
    }

    for (target_index, target_path) in target_paths.iter().enumerate() {
        if cancel.is_cancelled() {
            report.failed.push((
                target_path.clone(),
                "tag transfer cancelled before writing this file".to_string(),
            ));
            if let Some(progress) = progress {
                progress(target_index + 1, target_paths.len(), target_path);
            }
            continue;
        }
        if let Some(issue) = target_merged
            .metadata_errors
            .get(target_index)
            .and_then(|issue| issue.as_ref())
            .filter(|issue| issue.blocks_metadata_use())
        {
            report
                .failed
                .push((target_path.clone(), issue.reason.clone()));
            if let Some(progress) = progress {
                progress(target_index + 1, target_paths.len(), target_path);
            }
            continue;
        }

        let backend = crate::metadata_persistence::metadata_backend_for_path(target_path)?;
        let mut changes = Vec::new();
        for field in &value_plan.fields {
            let source_value = &field.values[target_index];
            let target_entry = target_by_key.get(&field.canonical_key);
            let target_value = target_entry
                .and_then(|entry| entry.per_file_values.get(target_index))
                .cloned()
                .unwrap_or_default();
            let target_existed = target_entry
                .is_some_and(|entry| entry.stored_value_count_for_slot(target_index) > 0);
            if source_value == &target_value {
                continue;
            }
            if super::probe::metadata_field_is_set_valued(&field.canonical_key)
                && source_value.value_count() > 1
                && !backend.supports_repeated_field(&field.canonical_key)
            {
                report.cardinality_warnings.push(format!(
                    "{}: {} has {} values but {} cannot round-trip repeated instances; the legacy scalar projection will be written",
                    target_path.display(),
                    field.canonical_key,
                    source_value.value_count(),
                    backend.label(),
                ));
            }
            changes.push((
                field.item_key.clone(),
                Some(source_value.clone()),
                target_existed,
            ));
        }
        if changes.is_empty() {
            report.unchanged += 1;
            if let Some(progress) = progress {
                progress(target_index + 1, target_paths.len(), target_path);
            }
            continue;
        }

        match writer(target_path, &changes, Some(cancel), verification) {
            Ok(commit) => {
                report.written += 1;
                report.written_paths.push(target_path.clone());
                report.durability_warnings.extend(
                    commit
                        .durability_warnings
                        .into_iter()
                        .map(|warning| format!("{}: {}", target_path.display(), warning)),
                );
            }
            Err(reason) => report.failed.push((target_path.clone(), reason)),
        }
        if let Some(progress) = progress {
            progress(target_index + 1, target_paths.len(), target_path);
        }
    }

    Ok(report)
}

fn cue_replacement_value<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    if value.contains('\r') || value.contains('\n') || value.contains('"') {
        return Err(format!(
            "{} cannot be written to CUE losslessly because it contains a line break or double quote",
            field
        ));
    }
    Ok(value)
}

fn overlay_projected_transfer_plan_on_cue_sheet(
    projected_plan: &TransferValuePlan,
    target_sheet: &crate::convert::cue_parser::CueSheet,
    selected_track_indices: &[usize],
) -> Result<TransferValuePlan, String> {
    let target_count = target_sheet.tracks.len();
    if selected_track_indices.is_empty() {
        return Err("tag transfer projected CUE target has no selected tracks".to_string());
    }
    if selected_track_indices
        .iter()
        .any(|index| *index >= target_count)
    {
        return Err("tag transfer projected CUE target index is out of bounds".to_string());
    }
    if projected_plan
        .fields
        .iter()
        .any(|field| field.values.len() != selected_track_indices.len())
    {
        return Err(
            "tag transfer projected CUE field cardinality does not match the selected tracks"
                .to_string(),
        );
    }

    let current_entries = cue_sheet_transfer_entries(target_sheet);
    let current_by_key = current_entries
        .iter()
        .map(|entry| {
            (
                super::probe::canonical_metadata_display_key(&entry.display_key),
                entry,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut full_plan = TransferValuePlan {
        skipped_numbering_keys: projected_plan.skipped_numbering_keys.clone(),
        skipped_fields: projected_plan.skipped_fields.clone(),
        cardinality_warnings: projected_plan.cardinality_warnings.clone(),
        first_track_collapse: projected_plan.first_track_collapse,
        ..TransferValuePlan::default()
    };
    for field in &projected_plan.fields {
        let mut values = if let Some(current) = current_by_key.get(&field.canonical_key) {
            if current.per_file_values.len() != target_count {
                return Err(format!(
                    "tag transfer CUE field {} has {} values for {} authored tracks",
                    field.canonical_key,
                    current.per_file_values.len(),
                    target_count
                ));
            }
            current.per_file_values.clone()
        } else {
            // `cue_sheet_transfer_entries` intentionally omits fields whose
            // complete current value is empty. Treat that as an all-empty full
            // sheet so a projected write can introduce a representable
            // per-track field without manufacturing values for other tracks.
            vec![super::probe::MetadataFieldValues::default(); target_count]
        };
        for (projected_index, target_index) in
            selected_track_indices.iter().copied().enumerate()
        {
            values[target_index] = field.values[projected_index].clone();
        }

        // CUE album fields have one physical value shared by every track. A
        // strict subset cannot change that value without also changing the
        // metadata observed by unselected carriers. Preserve the album value
        // and report the field as skipped when the projected overlay would
        // require mixed values. Per-track fields remain independently writable.
        if !is_cue_per_track_field(&field.canonical_key)
            && values.windows(2).any(|pair| pair[0] != pair[1])
        {
            full_plan.skipped_fields.push(format!(
                "{} skipped: album-scoped CUE metadata cannot be changed by a partial carrier selection without affecting unselected tracks",
                field.canonical_key
            ));
            continue;
        }
        full_plan.fields.push(PlannedTransferField {
            canonical_key: field.canonical_key.clone(),
            item_key: field.item_key.clone(),
            values,
        });
    }
    Ok(full_plan)
}

fn cue_metadata_replacement_text(
    value_plan: &TransferValuePlan,
    target_sheet: &crate::convert::cue_parser::CueSheet,
) -> Result<String, String> {
    let target_count = target_sheet.tracks.len();
    let field = |key: &str| {
        value_plan
            .fields
            .iter()
            .find(|field| field.canonical_key == key)
    };
    let mut lines = Vec::new();
    let mut push_album_quoted = |key: &str, cue_key: &str| -> Result<(), String> {
        let Some(value) = field(key).and_then(|field| field.values.first()) else {
            return Ok(());
        };
        if value.is_empty() {
            return Ok(());
        }
        lines.push(format!(
            "{} \"{}\"",
            cue_key,
            cue_replacement_value(value.as_str(), key)?
        ));
        Ok(())
    };
    push_album_quoted("ALBUM", "TITLE")?;
    push_album_quoted("ALBUMARTIST", "PERFORMER")?;
    drop(push_album_quoted);
    if let Some(value) = field("DATE").and_then(|field| field.values.first()) {
        if !value.is_empty() {
            lines.push(format!(
                "REM DATE \"{}\"",
                cue_replacement_value(value.as_str(), "DATE")?
            ));
        }
    }
    if let Some(value) = field("GENRE").and_then(|field| field.values.first()) {
        if !value.is_empty() {
            lines.push(format!(
                "REM GENRE \"{}\"",
                cue_replacement_value(value.as_str(), "GENRE")?
            ));
        }
    }
    if let Some(value) = field("CATALOGNUMBER").and_then(|field| field.values.first()) {
        if !value.is_empty() {
            lines.push(format!(
                "CATALOG {}",
                cue_replacement_value(value.as_str(), "CATALOGNUMBER")?
            ));
        }
    }

    let mut target_tracks = target_sheet.tracks.iter().collect::<Vec<_>>();
    target_tracks.sort_by_key(|track| track.number);
    for (index, track) in target_tracks.iter().enumerate() {
        lines.push(format!("  TRACK {:02} AUDIO", track.number));
        for (key, cue_key) in [("TITLE", "TITLE"), ("ARTIST", "PERFORMER")] {
            let Some(value) = field(key).and_then(|field| field.values.get(index)) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            lines.push(format!(
                "    {} \"{}\"",
                cue_key,
                cue_replacement_value(value.as_str(), key)?
            ));
        }
    }
    if target_tracks.len() != target_count {
        return Err("internal error: CUE target track ordering changed cardinality".to_string());
    }
    Ok(lines.join("\n"))
}

fn transfer_path_identity(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn validate_sidecar_transfer_snapshot(
    cue_path: &std::path::Path,
    current_cue_text: &str,
    expected_track_audio_paths: &[std::path::PathBuf],
    expected_sheet: &crate::convert::cue_parser::CueSheet,
) -> Result<(), String> {
    let current_sheet = crate::convert::cue_parser::parse_cue(current_cue_text);
    let parent = cue_path.parent().ok_or_else(|| {
        format!(
            "sidecar CUE '{}' has no parent directory; left unchanged",
            cue_path.display()
        )
    })?;
    if current_sheet.tracks.len() != expected_track_audio_paths.len() {
        return Err(format!(
            "sidecar CUE '{}' changed track/image ownership after classification; left unchanged",
            cue_path.display()
        ));
    }
    let mut current_track_paths = Vec::with_capacity(current_sheet.tracks.len());
    let mut seen_track_numbers = std::collections::BTreeSet::new();
    for track in &current_sheet.tracks {
        if track.number == 0 || !seen_track_numbers.insert(track.number) {
            return Err(format!(
                "sidecar CUE '{}' has invalid or duplicate authored TRACK numbers; left unchanged",
                cue_path.display()
            ));
        }
        let file_ref = track.file.as_deref().ok_or_else(|| {
            format!(
                "sidecar CUE '{}' track {} no longer has a FILE reference; left unchanged",
                cue_path.display(),
                track.number
            )
        })?;
        let resolved = super::accuraterip::resolve_cue_file_reference(parent, file_ref)
            .ok_or_else(|| {
                format!(
                    "sidecar CUE '{}' no longer resolves FILE reference '{}'; left unchanged",
                    cue_path.display(),
                    file_ref
                )
            })?;
        current_track_paths.push((track.number, transfer_path_identity(&resolved)));
    }
    current_track_paths.sort_by_key(|(track_number, _)| *track_number);
    let current_track_paths = current_track_paths
        .into_iter()
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    let expected_track_paths = expected_track_audio_paths
        .iter()
        .map(|path| transfer_path_identity(path))
        .collect::<Vec<_>>();
    if current_track_paths != expected_track_paths {
        return Err(format!(
            "sidecar CUE '{}' no longer resolves to the expected per-track image set; left unchanged",
            cue_path.display()
        ));
    }
    if !cue_track_geometry_matches(expected_sheet, &current_sheet) {
        return Err(format!(
            "sidecar CUE '{}' changed track structure after classification; left unchanged",
            cue_path.display()
        ));
    }
    Ok(())
}

fn cue_track_geometry_matches(
    expected: &crate::convert::cue_parser::CueSheet,
    current: &crate::convert::cue_parser::CueSheet,
) -> bool {
    let mut expected_tracks = expected.tracks.iter().collect::<Vec<_>>();
    let mut current_tracks = current.tracks.iter().collect::<Vec<_>>();
    expected_tracks.sort_by_key(|track| track.number);
    current_tracks.sort_by_key(|track| track.number);
    expected_tracks.len() == current_tracks.len()
        && expected_tracks
            .iter()
            .zip(current_tracks.iter())
            .all(|(expected, current)| {
                expected.number == current.number
                    && expected.index01_frames == current.index01_frames
            })
}

fn revalidate_embedded_transfer_target(
    image_path: &std::path::Path,
    expected_sheet: &crate::convert::cue_parser::CueSheet,
    cancel: &super::probe::MetadataWriteCancelFlag,
) -> Result<String, String> {
    let merged = super::probe::read_all_tags_merged_with_metadata_cancellable_for_operation(
        &[image_path.to_path_buf()],
        || cancel.is_cancelled(),
        "tag transfer cancelled while re-admitting embedded CUE target",
    )?;
    if let Some(issue) = merged
        .metadata_errors
        .first()
        .and_then(|issue| issue.as_ref())
        .filter(|issue| issue.blocks_metadata_use())
    {
        return Err(format!(
            "embedded CUE target '{}' could not be re-read immediately before write; left unchanged: {}",
            image_path.display(),
            issue.reason,
        ));
    }
    let current_text = merged
        .entries
        .iter()
        .find(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
        .and_then(|entry| entry.per_file_values.first())
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.as_str().to_string())
        .ok_or_else(|| {
            format!(
                "embedded CUE target '{}' no longer carries a CUESHEET; left unchanged",
                image_path.display(),
            )
        })?;
    let current_sheet = crate::convert::cue_parser::parse_cue(&current_text);
    if !cue_track_geometry_matches(expected_sheet, &current_sheet) {
        return Err(format!(
            "embedded CUE target '{}' changed track structure after classification; left unchanged",
            image_path.display(),
        ));
    }
    Ok(current_text)
}

fn write_sidecar_transfer(
    cue_path: &std::path::Path,
    track_audio_paths: &[std::path::PathBuf],
    sheet: &crate::convert::cue_parser::CueSheet,
    classified_cue_text: &str,
    replacement: &str,
    materialization_expected_original: Option<&Option<Vec<u8>>>,
) -> Result<Option<super::probe::MetadataWriteCommitReport>, String> {
    let outcome = match materialization_expected_original {
        Some(expected_original) => {
            // A target-only sidecar starts from the FILE/TRACK structure built
            // during classification. Overlay only the requested metadata, then
            // validate the exact FILE-bearing text that will be published.
            // This keeps carrier ownership single-sourced and closes the gap
            // where a metadata-only replacement was validated before FILE
            // references could exist.
            let materialized = crate::convert::cue_parser::compose_cue_metadata_replacement(
                classified_cue_text,
                replacement,
            )?;
            validate_sidecar_transfer_snapshot(
                cue_path,
                &materialized,
                track_audio_paths,
                sheet,
            )?;
            match expected_original {
                None => crate::convert::cue_parser::create_cue_sidecar_from_cuesheet(
                    cue_path,
                    &materialized,
                ),
                Some(expected_original) => {
                    crate::convert::cue_parser::replace_invalid_cue_sidecar_from_cuesheet_if_unchanged(
                        cue_path,
                        &materialized,
                        expected_original,
                    )
                }
            }
        }
        None => crate::convert::cue_parser::rewrite_cue_sidecar_metadata_from_cuesheet_validated(
            cue_path,
            replacement,
            |_raw, current_cue_text| {
                // Existing sidecars already own their FILE/TRACK structure.
                // Validate the exact snapshot consumed by the byte-preserving
                // mutator, not the intentionally FILE-less metadata overlay.
                validate_sidecar_transfer_snapshot(
                    cue_path,
                    current_cue_text,
                    track_audio_paths,
                    sheet,
                )
            },
        ),
    }?;
    Ok(match outcome {
        crate::convert::cue_parser::CueSidecarWritebackOutcome::Unchanged => None,
        crate::convert::cue_parser::CueSidecarWritebackOutcome::Rewritten { .. } => {
            Some(super::probe::MetadataWriteCommitReport::default())
        }
        crate::convert::cue_parser::CueSidecarWritebackOutcome::RewrittenUtf8Fallback {
            source_encoding,
        } => Some(super::probe::MetadataWriteCommitReport {
            durability_warnings: vec![format!(
                "sidecar CUE encoding changed from {source_encoding} to UTF-8 because the requested metadata was not representable losslessly"
            )],
        }),
    })
}

fn preflight_sidecar_transfer_snapshot(
    cue_path: &std::path::Path,
    track_audio_paths: &[std::path::PathBuf],
    sheet: &crate::convert::cue_parser::CueSheet,
) -> Result<(), String> {
    let raw = std::fs::read(cue_path).map_err(|error| {
        format!(
            "read sidecar CUE '{}' before member writes: {error}",
            cue_path.display()
        )
    })?;
    let current = crate::convert::cue_parser::decode_cue_bytes_for_path(&raw, cue_path)
        .map_err(|error| {
            format!(
                "decode sidecar CUE '{}' before member writes: {error}",
                cue_path.display()
            )
        })?;
    validate_sidecar_transfer_snapshot(cue_path, &current, track_audio_paths, sheet)
}

/// Whether the carrier has an existing metadata writer that can persist an
/// embedded CUESHEET. Aggregate transfer and editor resolution share this gate
/// so a representation cannot win priority and then fail categorically on save.
pub(crate) fn embedded_cue_metadata_target_is_writable(path: &std::path::Path) -> bool {
    matches!(
        crate::metadata_persistence::metadata_persistence_route_for_path(path),
        crate::metadata_persistence::MetadataPersistenceRoute::NativeFlacVorbis
            | crate::metadata_persistence::MetadataPersistenceRoute::NativeDsfId3
            | crate::metadata_persistence::MetadataPersistenceRoute::WavPackApeDispatch
            | crate::metadata_persistence::MetadataPersistenceRoute::Lofty
    )
}

fn validate_embedded_cue_transfer_target(
    carrier: &EmbeddedCueCarrier,
) -> Result<(), String> {
    if carrier.multi_file_read_only {
        return Err(
            "multi-FILE embedded CUESHEET carriers are read-only transfer sources; a single member cannot authoritatively rewrite sibling references"
                .to_string(),
        );
    }
    if !embedded_cue_metadata_target_is_writable(&carrier.image_path) {
        return Err("embedded CUE write is not supported for this audio carrier".to_string());
    }
    Ok(())
}

fn write_embedded_cue_transfer(
    carrier: &EmbeddedCueCarrier,
    replacement: &str,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
) -> Result<Option<super::probe::MetadataWriteCommitReport>, String> {
    let current_cue_text = revalidate_embedded_transfer_target(
        &carrier.image_path,
        &carrier.sheet,
        cancel,
    )?;
    if current_cue_text != carrier.cue_text {
        log::debug!(
            "embedded CUE text changed after transfer classification for '{}'; composing against the revalidated current text",
            carrier.image_path.display(),
        );
    }
    let composed = crate::convert::cue_parser::compose_cue_metadata_replacement(
        &current_cue_text,
        replacement,
    )?;
    if composed == current_cue_text {
        return Ok(None);
    }

    let report = match crate::metadata_persistence::metadata_persistence_route_for_path(
        &carrier.image_path,
    ) {
        crate::metadata_persistence::MetadataPersistenceRoute::NativeFlacVorbis => {
            super::probe::write_embedded_cuesheet_for_transfer_at_verification(
                &carrier.image_path,
                &current_cue_text,
                composed,
                Some(cancel),
                verification,
            )
        }
        crate::metadata_persistence::MetadataPersistenceRoute::NativeDsfId3
        | crate::metadata_persistence::MetadataPersistenceRoute::WavPackApeDispatch
        | crate::metadata_persistence::MetadataPersistenceRoute::Lofty => {
            super::probe::write_all_tags_for_transfer_at_verification(
                &carrier.image_path,
                &[(
                    lofty::tag::ItemKey::Unknown("CUESHEET".to_string()),
                    Some(composed),
                )],
                Some(cancel),
                verification,
            )
        }
        crate::metadata_persistence::MetadataPersistenceRoute::ReadOnlyApeFamily
        | crate::metadata_persistence::MetadataPersistenceRoute::UnsupportedDff => Err(
            "embedded CUE write is not supported for this audio carrier".to_string(),
        ),
    }?;
    Ok(Some(report))
}

fn transfer_value_plan_slice(
    plan: &TransferValuePlan,
    start: usize,
    len: usize,
) -> Result<TransferValuePlan, String> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| "internal error: embedded CUE carrier range overflow".to_string())?;
    let mut fields = Vec::with_capacity(plan.fields.len());
    for field in &plan.fields {
        let values = field.values.get(start..end).ok_or_else(|| {
            format!(
                "internal error: embedded CUE field '{}' has inconsistent carrier cardinality",
                field.canonical_key
            )
        })?;
        fields.push(PlannedTransferField {
            canonical_key: field.canonical_key.clone(),
            item_key: field.item_key.clone(),
            values: values.to_vec(),
        });
    }
    Ok(TransferValuePlan {
        fields,
        first_track_collapse: plan.first_track_collapse,
        ..TransferValuePlan::default()
    })
}

fn execute_tag_transfer_to_embedded_cues(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    carriers: &[EmbeddedCueCarrier],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    if carriers.is_empty() {
        return Err("internal error: embedded CUE carrier set is empty".to_string());
    }
    // Aggregate transfer is intentionally all-or-nothing at admission: no
    // member is mutated unless every carrier is writable at dispatch time.
    for carrier in carriers {
        validate_embedded_cue_transfer_target(carrier)?;
    }
    let target_dimension = TransferDimension::Tracks(
        carriers
            .iter()
            .map(|carrier| carrier.sheet.tracks.len())
            .sum(),
    );
    let plan = plan_transfer_values_for_dimensions(
        source_entries,
        source_dimension,
        target_dimension,
        scope,
    )?;
    let mut report = TagTransferReport {
        source_count: source_dimension.count(),
        target_count: target_dimension.count(),
        written_fields: plan.fields.len(),
        source_carrier: Some(if source_dimension.is_tracks() {
            "CUE tracks".to_string()
        } else {
            "files".to_string()
        }),
        target_carrier: Some("embedded CUE".to_string()),
        target_paths: carriers
            .iter()
            .map(|carrier| carrier.image_path.clone())
            .collect(),
        first_track_collapse: plan.first_track_collapse,
        skipped_numbering_keys: plan.skipped_numbering_keys.clone(),
        skipped_fields: plan.skipped_fields.clone(),
        cardinality_warnings: plan.cardinality_warnings.clone(),
        ..TagTransferReport::default()
    };

    let total = carriers.len();
    let mut offset = 0usize;
    for (index, carrier) in carriers.iter().enumerate() {
        let track_count = carrier.sheet.tracks.len();
        if cancel.is_cancelled() {
            report.failed.push((
                carrier.image_path.clone(),
                "tag transfer cancelled before embedded CUE write".to_string(),
            ));
        } else {
            let local_plan = transfer_value_plan_slice(&plan, offset, track_count)?;
            let replacement = cue_metadata_replacement_text(&local_plan, &carrier.sheet)?;
            match write_embedded_cue_transfer(carrier, &replacement, verification, cancel) {
                Ok(Some(commit)) => {
                    report.written += 1;
                    report.written_paths.push(carrier.image_path.clone());
                    report.durability_warnings.extend(commit.durability_warnings);
                }
                Ok(None) => report.unchanged += 1,
                Err(error) => report.failed.push((carrier.image_path.clone(), error)),
            }
        }
        offset += track_count;
        if let Some(progress) = progress {
            progress(index + 1, total, &carrier.image_path);
        }
    }
    Ok(report)
}

fn validate_transfer_target_for_write(target: &TransferCarrier) -> Result<(), String> {
    match target {
        TransferCarrier::EmbeddedCue {
            image_path,
            cue_text,
            sheet,
            multi_file_read_only,
        } => validate_embedded_cue_transfer_target(&EmbeddedCueCarrier {
            image_path: image_path.clone(),
            cue_text: cue_text.clone(),
            sheet: sheet.clone(),
            multi_file_read_only: *multi_file_read_only,
        }),
        TransferCarrier::EmbeddedCues { carriers } => {
            if carriers.is_empty() {
                return Err("internal error: embedded CUE carrier set is empty".to_string());
            }
            for carrier in carriers {
                validate_embedded_cue_transfer_target(carrier)?;
            }
            Ok(())
        }
        TransferCarrier::Aggregate { carriers } => {
            if carriers.is_empty() {
                return Err("internal error: aggregate transfer carrier is empty".to_string());
            }
            for carrier in carriers {
                validate_transfer_target_for_write(carrier)?;
            }
            Ok(())
        }
        TransferCarrier::Files { .. } | TransferCarrier::SidecarCue { .. } => Ok(()),
    }
}

fn append_transfer_report(target: &mut TagTransferReport, mut child: TagTransferReport) {
    target.written = target.written.saturating_add(child.written);
    target.unchanged = target.unchanged.saturating_add(child.unchanged);
    target.written_fields = target.written_fields.saturating_add(child.written_fields);
    target.first_track_collapse |= child.first_track_collapse;
    target.target_paths.append(&mut child.target_paths);
    target.written_paths.append(&mut child.written_paths);
    target.failed.append(&mut child.failed);
    target.blocked.append(&mut child.blocked);
    target
        .skipped_numbering_keys
        .append(&mut child.skipped_numbering_keys);
    target.skipped_fields.append(&mut child.skipped_fields);
    target
        .cardinality_warnings
        .append(&mut child.cardinality_warnings);
    target
        .durability_warnings
        .append(&mut child.durability_warnings);
}

fn execute_tag_transfer_to_aggregate(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    source_track_numbers: Option<&[u32]>,
    carriers: &[TransferCarrier],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    if carriers.is_empty() {
        return Err("internal error: aggregate transfer carrier is empty".to_string());
    }
    let aggregate_count = carriers.iter().map(TransferCarrier::count).sum::<usize>();
    validate_transfer_target_for_write(&TransferCarrier::Aggregate {
        carriers: carriers.to_vec(),
    })?;

    let mut report = TagTransferReport {
        source_count: source_dimension.count(),
        target_count: aggregate_count,
        source_carrier: Some(if source_dimension.is_tracks() {
            "CUE tracks".to_string()
        } else {
            "files".to_string()
        }),
        target_carrier: Some("aggregate metadata".to_string()),
        ..TagTransferReport::default()
    };
    let total_operations = carriers
        .iter()
        .map(TransferCarrier::write_operation_count)
        .sum::<usize>();
    let mut position_offset = 0usize;
    let mut operation_offset = 0usize;
    for carrier in carriers {
        let count = carrier.count();
        let (segment_entries, segment_dimension) = aggregate_source_segment(
            source_entries,
            source_dimension,
            carrier.dimension(),
            position_offset,
            count,
            aggregate_count,
        )?;
        let segment_track_numbers = match source_track_numbers {
            Some(numbers) if source_dimension.count() == aggregate_count => {
                let end = position_offset
                    .checked_add(count)
                    .ok_or_else(|| "tag transfer aggregate track-number overflow".to_string())?;
                Some(numbers.get(position_offset..end).ok_or_else(|| {
                    "tag transfer source track-number cardinality does not match its aggregate dimension"
                        .to_string()
                })?)
            }
            Some(numbers) if source_dimension.count() == 1 => Some(numbers),
            _ => None,
        };
        let mapped_progress = |completed: usize, _total: usize, path: &std::path::Path| {
            if let Some(progress) = progress {
                progress(operation_offset + completed, total_operations, path);
            }
        };
        let child = execute_tag_transfer_from_entries_to_carrier(
            &segment_entries,
            segment_dimension,
            segment_track_numbers,
            carrier,
            scope,
            verification,
            cancel,
            Some(&mapped_progress),
        )?;
        append_transfer_report(&mut report, child);
        position_offset += count;
        operation_offset += carrier.write_operation_count();
    }
    Ok(report)
}

fn execute_tag_transfer_to_cue(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    _source_track_numbers: Option<&[u32]>,
    target: &TransferCarrier,
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    if let TransferCarrier::EmbeddedCues { carriers } = target {
        return execute_tag_transfer_to_embedded_cues(
            source_entries,
            source_dimension,
            carriers,
            scope,
            verification,
            cancel,
            progress,
        );
    }
    if let TransferCarrier::EmbeddedCue {
        image_path,
        cue_text,
        sheet,
        multi_file_read_only,
    } = target
    {
        validate_embedded_cue_transfer_target(&EmbeddedCueCarrier {
            image_path: image_path.clone(),
            cue_text: cue_text.clone(),
            sheet: sheet.clone(),
            multi_file_read_only: *multi_file_read_only,
        })?;
    }
    let target_dimension = target.dimension();
    let mut cue_value_plan = plan_transfer_values_for_dimensions(
        source_entries,
        source_dimension,
        target_dimension,
        scope,
    )?;
    let (target_sheet, target_path) = match target {
        TransferCarrier::SidecarCue { cue_path, sheet, .. } => (sheet, cue_path.as_path()),
        TransferCarrier::EmbeddedCue { image_path, sheet, .. } => (sheet, image_path.as_path()),
        TransferCarrier::Files { .. }
        | TransferCarrier::EmbeddedCues { .. }
        | TransferCarrier::Aggregate { .. } => {
            return Err("internal error: CUE writer received unsupported carrier".to_string())
        }
    };
    if let TransferCarrier::SidecarCue {
        image_paths,
        track_audio_paths,
        write_method,
        sheet,
        ..
    } = target
    {
        if write_method.is_unsupported_authority() {
            let selected_track_indices = unsupported_sidecar_selected_track_indices(
                image_paths,
                track_audio_paths,
                sheet,
            )?;
            cue_value_plan = overlay_projected_transfer_plan_on_cue_sheet(
                &cue_value_plan,
                sheet,
                &selected_track_indices,
            )?;
        }
    }
    let replacement = cue_metadata_replacement_text(&cue_value_plan, target_sheet)?;

    if let TransferCarrier::SidecarCue {
        cue_path,
        image_paths,
        track_audio_paths,
        write_method,
        cue_text,
        sheet,
        ..
    } = target
    {
        if write_method.is_unsupported_authority() {
            if cancel.is_cancelled() {
                return Err("tag transfer cancelled before sidecar CUE write".to_string());
            }
            let total = image_paths.len().saturating_add(1);
            let mut report = TagTransferReport {
                source_count: source_dimension.count(),
                target_count: target_dimension.count(),
                written_fields: cue_value_plan.fields.len(),
                source_carrier: Some(if source_dimension.is_tracks() {
                    "CUE tracks".to_string()
                } else {
                    "files".to_string()
                }),
                target_carrier: Some("sidecar CUE".to_string()),
                target_paths: vec![cue_path.clone()],
                first_track_collapse: cue_value_plan.first_track_collapse,
                skipped_numbering_keys: cue_value_plan.skipped_numbering_keys,
                skipped_fields: cue_value_plan.skipped_fields,
                cardinality_warnings: cue_value_plan.cardinality_warnings,
                blocked: image_paths
                    .iter()
                    .cloned()
                    .map(|path| {
                        (
                            path,
                            "embedded tag write blocked: metadata format unsupported; sidecar CUE is authoritative"
                                .to_string(),
                        )
                    })
                    .collect(),
                ..TagTransferReport::default()
            };

            for (index, path) in image_paths.iter().enumerate() {
                if let Some(progress) = progress {
                    progress(index + 1, total, path);
                }
            }
            if cancel.is_cancelled() {
                return Err("tag transfer cancelled before sidecar CUE write".to_string());
            }
            match write_sidecar_transfer(
                cue_path,
                track_audio_paths,
                sheet,
                cue_text,
                &replacement,
                write_method.materialization_expected_original(),
            ) {
                Ok(Some(commit)) => {
                    report.written = 1;
                    report.written_paths.push(cue_path.clone());
                    report.durability_warnings.extend(commit.durability_warnings);
                }
                Ok(None) => report.unchanged = 1,
                Err(error) => report.failed.push((cue_path.clone(), error)),
            }
            if let Some(progress) = progress {
                progress(total, total, cue_path);
            }
            return Ok(report);
        }
    }

    if let TransferCarrier::SidecarCue {
        cue_path,
        track_audio_paths,
        role,
        write_method: SidecarCueWriteMethod::PerFileAndSidecar,
        cue_text,
        sheet,
        ..
    } = target
    {
        if *role != crate::convert::split_cue_album::SplitCueMemberRole::MetadataSidecar {
            return Err(
                "internal error: per-file CUE fan-out requested for a synthetic album part"
                    .to_string(),
            );
        }
        if track_audio_paths.len() != sheet.tracks.len() {
            return Err(
                "internal error: per-file CUE fan-out track/image cardinality mismatch"
                    .to_string(),
            );
        }
        // Validate the complete sidecar ownership/geometry snapshot before
        // mutating any member. The validated rewrite below repeats the same
        // check immediately before commit, closing the intervening race.
        preflight_sidecar_transfer_snapshot(cue_path, track_audio_paths, sheet)?;
        let target_track_numbers = target
            .authored_track_numbers()
            .ok_or_else(|| "internal error: CUE target has no authored track order".to_string())?;
        let combined_total = track_audio_paths.len() + 1;
        let mapped_progress = |completed: usize, _total: usize, path: &std::path::Path| {
            if let Some(progress) = progress {
                progress(completed, combined_total, path);
            }
        };
        let mut report = execute_tag_transfer_to_files_with_writer(
            source_entries,
            source_dimension,
            Some(&target_track_numbers),
            track_audio_paths,
            scope,
            verification,
            cancel,
            Some(&mapped_progress),
            |path, changes, cancel, verification| {
                super::probe::write_tag_value_lists_for_transfer_at_verification(
                    path,
                    changes,
                    cancel,
                    verification,
                )
            },
        )?;
        let member_failures = report.failed.len();
        report.target_carrier = Some("files + sidecar CUE".to_string());
        report.target_count = sheet.tracks.len();
        report.target_paths.insert(0, cue_path.clone());
        report.written_fields += cue_value_plan.fields.len();
        report.skipped_numbering_keys
            .extend(cue_value_plan.skipped_numbering_keys.clone());
        report.skipped_fields
            .extend(cue_value_plan.skipped_fields.clone());
        report.cardinality_warnings
            .extend(cue_value_plan.cardinality_warnings.clone());

        if member_failures > 0 {
            report.failed.push((
                cue_path.clone(),
                format!(
                    "sidecar left unchanged ({member_failures} member write(s) failed)"
                ),
            ));
            if let Some(progress) = progress {
                progress(combined_total, combined_total, cue_path);
            }
            return Ok(report);
        }
        if cancel.is_cancelled() {
            report.failed.push((
                cue_path.clone(),
                "tag transfer cancelled after member writes; sidecar left unchanged".to_string(),
            ));
            if let Some(progress) = progress {
                progress(combined_total, combined_total, cue_path);
            }
            return Ok(report);
        }
        match write_sidecar_transfer(
            cue_path,
            track_audio_paths,
            sheet,
            cue_text,
            &replacement,
            None,
        ) {
            Ok(Some(commit)) => {
                report.written += 1;
                report.written_paths.push(cue_path.clone());
                report.durability_warnings.extend(commit.durability_warnings);
            }
            Ok(None) => report.unchanged += 1,
            Err(error) => report.failed.push((cue_path.clone(), error)),
        }
        if let Some(progress) = progress {
            progress(combined_total, combined_total, cue_path);
        }
        return Ok(report);
    }

    let mut report = TagTransferReport {
        source_count: source_dimension.count(),
        target_count: target_dimension.count(),
        written_fields: cue_value_plan.fields.len(),
        source_carrier: Some(if source_dimension.is_tracks() {
            "CUE tracks".to_string()
        } else {
            "files".to_string()
        }),
        target_carrier: Some(target.label().to_string()),
        target_paths: vec![target_path.to_path_buf()],
        first_track_collapse: cue_value_plan.first_track_collapse,
        skipped_numbering_keys: cue_value_plan.skipped_numbering_keys,
        skipped_fields: cue_value_plan.skipped_fields,
        cardinality_warnings: cue_value_plan.cardinality_warnings,
        ..TagTransferReport::default()
    };

    if cancel.is_cancelled() {
        return Err("tag transfer cancelled before CUE write".to_string());
    }
    let write_result = match target {
        TransferCarrier::SidecarCue {
            cue_path,
            track_audio_paths,
            write_method,
            cue_text,
            sheet,
            ..
        } => write_sidecar_transfer(
            cue_path,
            track_audio_paths,
            sheet,
            cue_text,
            &replacement,
            write_method.materialization_expected_original(),
        ),
        TransferCarrier::EmbeddedCue {
            image_path,
            cue_text,
            sheet,
            multi_file_read_only,
        } => write_embedded_cue_transfer(
            &EmbeddedCueCarrier {
                image_path: image_path.clone(),
                cue_text: cue_text.clone(),
                sheet: sheet.clone(),
                multi_file_read_only: *multi_file_read_only,
            },
            &replacement,
            verification,
            cancel,
        ),
        TransferCarrier::Files { .. }
        | TransferCarrier::EmbeddedCues { .. }
        | TransferCarrier::Aggregate { .. } => unreachable!(),
    };

    match write_result {
        Ok(Some(commit)) => {
            report.written = 1;
            report.written_paths.push(target_path.to_path_buf());
            report.durability_warnings.extend(commit.durability_warnings);
        }
        Ok(None) => report.unchanged = 1,
        Err(error) => report.failed.push((target_path.to_path_buf(), error)),
    }
    if let Some(progress) = progress {
        progress(1, 1, target_path);
    }
    Ok(report)
}

pub(crate) fn execute_tag_transfer_from_entries_to_carrier(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    source_track_numbers: Option<&[u32]>,
    target: &TransferCarrier,
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    match target {
        TransferCarrier::Files { paths } => execute_tag_transfer_to_files_with_writer(
            source_entries,
            source_dimension,
            source_track_numbers,
            paths,
            scope,
            verification,
            cancel,
            progress,
            |path, changes, cancel, verification| {
                super::probe::write_tag_value_lists_for_transfer_at_verification(
                    path,
                    changes,
                    cancel,
                    verification,
                )
            },
        ),
        TransferCarrier::SidecarCue { .. }
        | TransferCarrier::EmbeddedCue { .. }
        | TransferCarrier::EmbeddedCues { .. } => {
            execute_tag_transfer_to_cue(
                source_entries,
                source_dimension,
                source_track_numbers,
                target,
                scope,
                verification,
                cancel,
                progress,
            )
        }
        TransferCarrier::Aggregate { carriers } => execute_tag_transfer_to_aggregate(
            source_entries,
            source_dimension,
            source_track_numbers,
            carriers,
            scope,
            verification,
            cancel,
            progress,
        ),
    }
}

pub(crate) fn preview_tag_transfer(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    target: &TransferCarrier,
    scope: super::app::TagTransferScope,
) -> Result<usize, String> {
    validate_transfer_target_for_write(target)?;
    if let TransferCarrier::Aggregate { carriers } = target {
        let aggregate_count = target.count();
        let mut offset = 0usize;
        let mut field_count = 0usize;
        for carrier in carriers {
            let count = carrier.count();
            let (entries, dimension) = aggregate_source_segment(
                source_entries,
                source_dimension,
                carrier.dimension(),
                offset,
                count,
                aggregate_count,
            )?;
            field_count = field_count.saturating_add(preview_tag_transfer(
                &entries,
                dimension,
                carrier,
                scope,
            )?);
            offset += count;
        }
        return Ok(field_count);
    }
    Ok(plan_transfer_values_for_dimensions(
        source_entries,
        source_dimension,
        target.dimension(),
        scope,
    )?
    .fields
    .len())
}

pub(crate) fn preview_tag_transfer_fanout(
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    target: &TransferCarrier,
    scope: super::app::TagTransferScope,
) -> Result<(usize, Option<(usize, usize)>), String> {
    if let TransferCarrier::Aggregate { carriers } = target {
        validate_transfer_target_for_write(target)?;
        let aggregate_count = target.count();
        let mut offset = 0usize;
        let mut total = 0usize;
        for carrier in carriers {
            let count = carrier.count();
            let (entries, dimension) = aggregate_source_segment(
                source_entries,
                source_dimension,
                carrier.dimension(),
                offset,
                count,
                aggregate_count,
            )?;
            total = total.saturating_add(
                preview_tag_transfer_fanout(&entries, dimension, carrier, scope)?.0,
            );
            offset += count;
        }
        return Ok((total, None));
    }

    let cue_or_file_count = preview_tag_transfer(
        source_entries,
        source_dimension,
        target,
        scope,
    )?;
    if let TransferCarrier::SidecarCue {
        track_audio_paths,
        write_method: SidecarCueWriteMethod::PerFileAndSidecar,
        ..
    } = target
    {
        let file_count = plan_transfer_values_for_dimensions(
            source_entries,
            source_dimension,
            TransferDimension::Files(track_audio_paths.len()),
            scope,
        )?
        .fields
        .len();
        Ok((file_count + cue_or_file_count, Some((file_count, cue_or_file_count))))
    } else {
        Ok((cue_or_file_count, None))
    }
}

pub(crate) fn execute_tag_transfer_between_carriers(
    source: &TransferCarrier,
    target: &TransferCarrier,
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    let source_entries = read_transfer_carrier_entries(source, scope, cancel)?;
    let source_pairing_warning = corroborate_source_pairing(source, target, &source_entries)?;
    let source_track_numbers = carrier_authored_track_numbers(source);
    let mut report = execute_tag_transfer_from_entries_to_carrier(
        &source_entries,
        source.dimension(),
        source_track_numbers.as_deref(),
        target,
        scope,
        verification,
        cancel,
        progress,
    )?;
    report.source_carrier = Some(source.label().to_string());
    if let Some(warning) = source_pairing_warning {
        report.cardinality_warnings.push(warning);
    }
    Ok(report)
}

#[cfg(test)] // superseded by the prepared/classified round-7 flow; exercised by regression tests
pub(crate) fn execute_tag_transfer_from_paths(
    source_paths: &[std::path::PathBuf],
    target_paths: &[std::path::PathBuf],
    scope: super::app::TagTransferScope,
    verification: tui_file_picker::VerificationMode,
    cancel: &super::probe::MetadataWriteCancelFlag,
    progress: Option<&(dyn Fn(usize, usize, &std::path::Path) + Send + Sync)>,
) -> Result<TagTransferReport, String> {
    let source_entries = read_transfer_source_entries(source_paths, scope, cancel)?;
    execute_tag_transfer_from_entries(
        &source_entries,
        source_paths.len(),
        target_paths,
        scope,
        verification,
        cancel,
        progress,
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldBlockApplyReport {
    pub applied: Vec<(String, FieldBlockValueMode)>,
    pub skipped_track_scoped: Vec<String>,
    pub collapsed_fields: Vec<MetadataStoredValueCollapse>,
}

impl FieldBlockApplyReport {
    pub fn success_status(&self, file_count: usize) -> String {
        let mut parts = self
            .applied
            .iter()
            .map(|(key, mode)| match mode {
                FieldBlockValueMode::Broadcast => format!(
                    "{} (broadcast to {} file{})",
                    key,
                    file_count,
                    if file_count == 1 { "" } else { "s" }
                ),
                FieldBlockValueMode::Positional => format!("{} (positional)", key),
            })
            .collect::<Vec<_>>();
        if !self.skipped_track_scoped.is_empty() {
            parts.push(format!(
                "skipped track-scoped {}",
                self.skipped_track_scoped.join(", ")
            ));
        }
        for collapsed in &self.collapsed_fields {
            let carriers = collapsed
                .slots
                .iter()
                .map(|slot| (slot + 1).to_string())
                .collect::<Vec<_>>();
            parts.push(format!(
                "warning: {} stored-value count reduced on carrier{} {}",
                collapsed.display_key,
                if carriers.len() == 1 { "" } else { "s" },
                carriers.join(", ")
            ));
        }
        format!("applied {} — review before save", parts.join(", "))
    }
}

pub(crate) fn apply_field_blocks_to_editor(
    state: &mut super::app::MetadataEditorState,
    blocks: &[FieldBlock],
) -> Result<FieldBlockApplyReport, String> {
    let file_count = state.active_surface().paths.len();
    if file_count == 0 {
        return Err("metadata editor has no file targets".to_string());
    }
    let mut target_rows = std::collections::HashMap::new();
    for (index, entry) in state.active_surface().entries.iter().enumerate() {
        let key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if target_rows.insert(key.clone(), index).is_some() {
            return Err(format!(
                "metadata editor contains duplicate field {key}; resolve it before applying tag blocks"
            ));
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut plans = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !is_field_block_key(&block.key) {
            return Err(format!("{} is not a valid tag-block key", block.key));
        }
        if !seen.insert(block.key.clone()) {
            return Err(format!("{} appears more than once in the tag blocks", block.key));
        }
        let mode = validate_block_count(block, file_count).map_err(|error| error.to_string())?;
        let row = target_rows.get(&block.key).copied();
        let track_scoped = row
            .and_then(|index| state.active_surface().entries.get(index))
            .is_some_and(|entry| entry.is_track_scoped(file_count));
        plans.push((block, mode, row, track_scoped));
    }

    let mut report = FieldBlockApplyReport::default();
    for (block, mode, existing_row, track_scoped) in plans {
        if track_scoped {
            report.skipped_track_scoped.push(block.key.clone());
            continue;
        }
        let values = (0..file_count)
            .map(|index| {
                value_for_target(block, mode, index)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let all_same = values.windows(2).all(|pair| pair[0] == pair[1]);
        let display = if all_same {
            values
                .first()
                .map(|values| values.as_str().to_string())
                .unwrap_or_default()
        } else {
            "<multiple values>".to_string()
        };

        if let Some(row) = existing_row {
            let collapse_slots = state.active_surface().entries[row]
                .stored_list_collapse_slots(values.iter().enumerate());
            if !collapse_slots.is_empty() {
                report.collapsed_fields.push(MetadataStoredValueCollapse {
                    display_key: block.key.clone(),
                    slots: collapse_slots,
                });
            }
            let surface = state.active_surface_mut();
            let entry = &mut surface.entries[row];
            entry.value = display;
            entry.is_mixed = !all_same;
            entry.per_file_values = values;
            entry.mb_proposed_value = None;
            entry.mb_proposed_per_file = None;
            surface.deleted.retain(|deleted| *deleted != row);
        } else {
            state.active_surface_mut().entries.push(TagEntry {
                display_key: block.key.clone(),
                item_key: super::probe::item_key_for_new_editor_row(&block.key),
                value: display,
                original: String::new(),
                is_binary: false,
                is_mixed: !all_same,
                has_multiple_stored_values: false,
                row_scope: super::probe::RowScope::File,
                per_file_stored_value_counts: vec![0; file_count],
                per_file_values: values,
                per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec![String::new(); file_count]),
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
        }
        report.applied.push((block.key.clone(), mode));
    }

    state.recompute_active_dirty();
    Ok(report)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTransferApplyReport {
    pub applied: Vec<(String, FieldBlockValueMode)>,
    pub skipped_numbering_keys: Vec<String>,
    pub skipped_track_scoped: Vec<String>,
    pub skipped_fields: Vec<String>,
    pub first_track_collapse: bool,
    pub cardinality_warnings: Vec<String>,
}

impl EditorTransferApplyReport {
    pub fn success_status(&self, target_count: usize) -> String {
        let mut parts = self
            .applied
            .iter()
            .map(|(key, mode)| match mode {
                FieldBlockValueMode::Broadcast => format!(
                    "{} (broadcast to {} position{})",
                    key,
                    target_count,
                    if target_count == 1 { "" } else { "s" }
                ),
                FieldBlockValueMode::Positional => format!("{} (positional)", key),
            })
            .collect::<Vec<_>>();
        if !self.skipped_numbering_keys.is_empty() {
            parts.push(format!(
                "skipped 1-to-N numbering {}",
                self.skipped_numbering_keys.join(", ")
            ));
        }
        if !self.skipped_track_scoped.is_empty() {
            parts.push(format!(
                "skipped track-scoped {}",
                self.skipped_track_scoped.join(", ")
            ));
        }
        parts.extend(self.skipped_fields.iter().cloned());
        if self.first_track_collapse {
            parts.push("used first-track values for the single-image editor".to_string());
        }
        if !self.cardinality_warnings.is_empty() {
            parts.push(format!(
                "{} cardinality warning{}",
                self.cardinality_warnings.len(),
                if self.cardinality_warnings.len() == 1 { "" } else { "s" }
            ));
        }
        format!("transferred {} into editor — review before save", parts.join(", "))
    }
}

pub(crate) fn metadata_editor_transfer_dimension(
    state: &super::app::MetadataEditorState,
) -> TransferDimension {
    let surface = state.active_surface();
    let track_count = surface
        .entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            !surface.deleted.contains(index)
                && matches!(entry.row_scope, super::probe::RowScope::Track)
        })
        .map(|(_, entry)| entry.per_file_values.len())
        .max()
        .unwrap_or(0);
    if track_count > 0 {
        TransferDimension::Tracks(track_count)
    } else {
        TransferDimension::Files(surface.paths.len())
    }
}

fn metadata_editor_first_track_collapse_eligibility(
    state: &super::app::MetadataEditorState,
    target_dimension: TransferDimension,
) -> FirstTrackCollapseEligibility {
    let surface = state.active_surface();
    let single_image = matches!(target_dimension, TransferDimension::Files(1))
        && surface.paths.len() == 1;
    let has_cuesheet = surface.entries.iter().enumerate().any(|(index, entry)| {
        !surface.deleted.contains(&index)
            && entry.display_key.eq_ignore_ascii_case("CUESHEET")
            && !entry
                .per_file_values
                .first()
                .map(super::probe::MetadataFieldValues::as_str)
                .unwrap_or(entry.value.as_str())
                .trim()
                .is_empty()
    });
    if single_image && has_cuesheet {
        FirstTrackCollapseEligibility::SingleImageWithCuesheet
    } else {
        FirstTrackCollapseEligibility::Forbidden
    }
}

pub(crate) fn metadata_editor_transfer_snapshot(
    state: &super::app::MetadataEditorState,
) -> (Vec<TagEntry>, TransferDimension) {
    let surface = state.active_surface();
    let dimension = metadata_editor_transfer_dimension(state);
    let entries = surface
        .entries
        .iter()
        .enumerate()
        .filter(|(index, _)| !surface.deleted.contains(index))
        .map(|(_, entry)| entry.clone())
        .collect();
    (entries, dimension)
}

pub(crate) fn editor_snapshot_authored_track_numbers(
    entries: &[TagEntry],
    dimension: TransferDimension,
) -> Option<Vec<u32>> {
    let TransferDimension::Tracks(expected_count) = dimension else {
        return None;
    };
    let cue_text = entries
        .iter()
        .find(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
        .and_then(|entry| {
            entry
                .per_file_values
                .first()
                .map(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .or_else(|| (!entry.value.trim().is_empty()).then_some(entry.value.as_str()))
        })?;
    let sheet = crate::convert::cue_parser::parse_cue(cue_text);
    let mut numbers = sheet
        .tracks
        .iter()
        .map(|track| track.number)
        .collect::<Vec<_>>();
    numbers.sort_unstable();
    let unique_and_valid = numbers.len() == expected_count
        && numbers.iter().all(|number| *number > 0)
        && numbers.windows(2).all(|pair| pair[0] != pair[1]);
    unique_and_valid.then_some(numbers)
}

pub(crate) fn metadata_editor_unsaved_edit_count(
    state: &super::app::MetadataEditorState,
) -> usize {
    let surface = state.active_surface();
    let mut changed = surface.deleted.iter().copied().collect::<std::collections::BTreeSet<_>>();
    for (index, entry) in surface.entries.iter().enumerate() {
        if entry.value != entry.original
            || entry.per_file_values != entry.per_file_originals
            || entry.mb_proposed_value.is_some()
            || entry.mb_proposed_per_file.is_some()
        {
            changed.insert(index);
        }
    }
    changed.len()
}

pub(crate) fn apply_transfer_entries_to_editor_with_dimension(
    state: &mut super::app::MetadataEditorState,
    source_entries: &[TagEntry],
    source_dimension: TransferDimension,
    scope: super::app::TagTransferScope,
) -> Result<EditorTransferApplyReport, String> {
    let target_dimension = metadata_editor_transfer_dimension(state);
    let target_count = target_dimension.count();
    if target_count == 0 {
        return Err("metadata editor has no transfer target positions".to_string());
    }

    let mut target_rows = std::collections::HashMap::new();
    let mut target_scopes = std::collections::HashMap::new();
    for (index, entry) in state.active_surface().entries.iter().enumerate() {
        let key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if target_rows.insert(key.clone(), index).is_some() {
            return Err(format!(
                "metadata editor contains duplicate field {key}; resolve it before transferring tags"
            ));
        }
        target_scopes.insert(key, entry.row_scope);
    }

    let presentation_dimension = state.active_surface().paths.len();
    let cue_dimensions = state
        .active_surface()
        .cue_album_synthetic_sheet
        .as_ref()
        .map(|sheet| super::probe::UnifiedCueDimensions {
            files: sheet.audio_paths.len(),
            tracks: sheet.track_sources.len(),
            presentation: presentation_dimension,
        });
    let value_plan = if let Some(dimensions) = cue_dimensions {
        plan_transfer_values_for_unified_cue_editor(
            source_entries,
            source_dimension,
            scope,
            dimensions,
            &target_scopes,
        )?
    } else {
        plan_transfer_values_for_dimensions_with_collapse(
            source_entries,
            source_dimension,
            target_dimension,
            scope,
            metadata_editor_first_track_collapse_eligibility(state, target_dimension),
        )?
    };

    let mut report = EditorTransferApplyReport {
        skipped_numbering_keys: value_plan.skipped_numbering_keys,
        skipped_fields: value_plan.skipped_fields,
        first_track_collapse: value_plan.first_track_collapse,
        cardinality_warnings: value_plan.cardinality_warnings,
        ..EditorTransferApplyReport::default()
    };

    for PlannedTransferField {
        canonical_key,
        item_key,
        values,
    } in value_plan.fields
    {
        let existing_scope = target_scopes
            .get(&canonical_key)
            .copied()
            .unwrap_or(super::probe::RowScope::Track);
        let (row_scope, field_target_count) = if let Some(dimensions) = cue_dimensions {
            let shape = super::probe::unified_cue_row_shape(&canonical_key, existing_scope)
                .ok_or_else(|| format!("{canonical_key} has no unified-CUE target shape"))?;
            (shape.scope, shape.dimension(dimensions))
        } else {
            (
                if target_dimension.is_tracks() {
                    super::probe::RowScope::Track
                } else {
                    super::probe::RowScope::File
                },
                target_count,
            )
        };
        if values.len() != field_target_count {
            return Err(format!(
                "internal tag-transfer shape error: {} projected {} values for a target dimension of {}",
                canonical_key,
                values.len(),
                field_target_count,
            ));
        }
        let mode = if source_dimension.count() == 1 && field_target_count > 1 {
            FieldBlockValueMode::Broadcast
        } else {
            FieldBlockValueMode::Positional
        };
        let all_same = values.windows(2).all(|pair| pair[0] == pair[1]);
        let display = if all_same {
            values
                .first()
                .map(|values| values.as_str().to_string())
                .unwrap_or_default()
        } else {
            "<multiple values>".to_string()
        };
        if let Some(row) = target_rows.get(&canonical_key).copied() {
            let surface = state.active_surface_mut();
            let entry = &mut surface.entries[row];
            entry.value = display;
            entry.is_mixed = !all_same;
            entry.row_scope = row_scope;
            entry.per_file_values = values;
            entry.mb_proposed_value = None;
            entry.mb_proposed_per_file = None;
            surface.deleted.retain(|deleted| *deleted != row);
        } else {
            let key = canonical_key.clone();
            state.active_surface_mut().entries.push(TagEntry {
                display_key: key.clone(),
                item_key,
                value: display,
                original: String::new(),
                is_binary: false,
                is_mixed: !all_same,
                has_multiple_stored_values: false,
                row_scope,
                per_file_stored_value_counts: Vec::new(),
                per_file_values: values,
                per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec![String::new(); field_target_count]),
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
            target_rows.insert(key, state.active_surface().entries.len() - 1);
        }
        report.applied.push((canonical_key, mode));
    }

    if cue_dimensions.is_some() {
        report.cardinality_warnings.extend(
            super::keybindings::cue_album_reconcile_row_shapes(state.active_surface_mut()),
        );
    }
    state.recompute_active_dirty();
    Ok(report)
}

pub(crate) fn metadata_editor_transfer_fingerprint(
    state: &super::app::MetadataEditorState,
) -> u64 {
    fn feed(hash: &mut u64, bytes: &[u8]) {
        const FNV_PRIME: u64 = 1_099_511_628_211;
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
        *hash ^= 0xff;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }

    let surface = state.active_surface();
    let mut hash = 14_695_981_039_346_656_037_u64;
    feed(&mut hash, format!("{:?}", surface.id).as_bytes());
    for path in &surface.paths {
        feed(&mut hash, path.to_string_lossy().as_bytes());
    }
    for (index, entry) in surface.entries.iter().enumerate() {
        feed(&mut hash, index.to_string().as_bytes());
        feed(&mut hash, entry.display_key.as_bytes());
        feed(&mut hash, format!("{:?}", entry.item_key).as_bytes());
        feed(&mut hash, format!("{:?}", entry.row_scope).as_bytes());
        feed(&mut hash, entry.value.as_bytes());
        feed(&mut hash, entry.original.as_bytes());
        if let Some(value) = entry.mb_proposed_value.as_deref() {
            feed(&mut hash, value.as_bytes());
        } else {
            feed(&mut hash, b"<no-mb-proposal>");
        }
        if let Some(values) = entry.mb_proposed_per_file.as_ref() {
            for value in values {
                feed(&mut hash, value.as_bytes());
            }
        } else {
            feed(&mut hash, b"<no-mb-per-file-proposal>");
        }
        for value in &entry.per_file_values {
            feed(&mut hash, value.as_bytes());
        }
        for value in &entry.per_file_originals {
            feed(&mut hash, value.as_bytes());
        }
        for count in &entry.per_file_stored_value_counts {
            feed(&mut hash, count.to_string().as_bytes());
        }
        feed(
            &mut hash,
            &[
                u8::from(entry.is_binary),
                u8::from(entry.is_mixed),
                u8::from(entry.has_multiple_stored_values),
            ],
        );
    }
    for deleted in &surface.deleted {
        feed(&mut hash, deleted.to_string().as_bytes());
    }
    hash
}

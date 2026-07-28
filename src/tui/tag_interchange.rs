//! Text-tag interchange primitives shared by Browse and the metadata editor.
//!
//! The format is deliberately small and fail-closed: one upper-case field key,
//! one encoded value per following line, and one byte-empty line between
//! blocks. Empty values use `~`; literal all-tilde values gain one leading
//! tilde. Values containing line breaks are not representable and are omitted
//! by serialization rather than being silently altered.

use std::fmt;

use super::probe::TagEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBlock {
    pub key: String,
    pub values: Vec<String>,
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
            .any(|value| value.contains('\n') || value.contains('\r'))
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
            block.push_str(&encode_value(value));
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
        let values = lines.map(decode_value).collect::<Vec<_>>();
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

pub fn value_for_target<'a>(
    block: &'a FieldBlock,
    mode: FieldBlockValueMode,
    target_index: usize,
) -> Option<&'a str> {
    match mode {
        FieldBlockValueMode::Broadcast => block.values.first().map(String::as_str),
        FieldBlockValueMode::Positional => block.values.get(target_index).map(String::as_str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::probe::RowScope;

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
            per_file_values: values.iter().map(|value| (*value).to_string()).collect(),
            per_file_originals: values.iter().map(|value| (*value).to_string()).collect(),
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
                    values: vec!["", "~", "~~", "  "]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
                FieldBlock {
                    key: "ARTIST".to_string(),
                    values: vec!["Genesis".to_string(); 4],
                },
            ]
        );
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
                per_file_values: values.clone(),
                per_file_originals: values.clone(),
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            };
            let serialized = serialize_tag_entries(std::iter::once(&source));
            let parsed = parse_field_blocks(&serialized.text).expect("generated round trip");
            assert_eq!(parsed, vec![FieldBlock {
                key: "CUSTOM_1".to_string(),
                values,
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
                values: vec!["Duke".to_string()],
            },
            FieldBlock {
                key: "TRACKNUMBER".to_string(),
                values: (1..=12).map(|value| value.to_string()).collect(),
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
                values: vec!["Genesis".to_string()],
            },
            FieldBlock {
                key: "DISCNUMBER".to_string(),
                values: vec!["1".to_string(), "2".to_string()],
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
            values: vec!["Replacement".to_string()],
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
                vec![(lofty::tag::ItemKey::TrackTitle, Some("Duke".to_string()))]
            );
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TagTransferReport {
    pub source_count: usize,
    pub target_count: usize,
    pub written: usize,
    pub unchanged: usize,
    pub written_paths: Vec<std::path::PathBuf>,
    pub failed: Vec<(std::path::PathBuf, String)>,
    pub skipped_numbering_keys: Vec<String>,
    pub cardinality_warnings: Vec<String>,
    pub durability_warnings: Vec<String>,
}

impl TagTransferReport {
    pub fn status(&self) -> String {
        let mut status = format!(
            "Transferred tags to {} file{} ({} written, {} unchanged, {} failed)",
            self.target_count,
            if self.target_count == 1 { "" } else { "s" },
            self.written,
            self.unchanged,
            self.failed.len(),
        );
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
    source_count: usize,
) -> bool {
    if entry.is_binary || entry.is_track_scoped(source_count) {
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

#[derive(Debug, Clone)]
struct PlannedTransferField {
    canonical_key: String,
    item_key: lofty::tag::ItemKey,
    values: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct TransferValuePlan {
    fields: Vec<PlannedTransferField>,
    skipped_numbering_keys: Vec<String>,
    cardinality_warnings: Vec<String>,
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

fn plan_transfer_values(
    source_entries: &[TagEntry],
    source_count: usize,
    target_count: usize,
    scope: super::app::TagTransferScope,
) -> Result<TransferValuePlan, String> {
    if source_count == 0 {
        return Err("tag transfer has no source files".to_string());
    }
    if target_count == 0 {
        return Err("tag transfer target contains no audio files".to_string());
    }
    if source_count != 1 && source_count != target_count {
        return Err(format!(
            "tag transfer requires 1 source or equal source/target counts; got {} sources and {} targets",
            source_count, target_count
        ));
    }

    let broadcast = source_count == 1 && target_count > 1;
    let mut plan = TransferValuePlan::default();
    let mut seen = std::collections::BTreeSet::new();
    for entry in source_entries {
        if !transfer_entry_selected(entry, scope, source_count) {
            continue;
        }
        let canonical_key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if !seen.insert(canonical_key.clone()) {
            return Err(format!(
                "tag transfer source contains duplicate field {canonical_key}"
            ));
        }
        if broadcast && is_transfer_numbering_key(&canonical_key) {
            plan.skipped_numbering_keys.push(canonical_key);
            continue;
        }

        let mut values = Vec::with_capacity(target_count);
        for target_index in 0..target_count {
            let source_index = if source_count == 1 { 0 } else { target_index };
            let value = entry.per_file_values.get(source_index).ok_or_else(|| {
                format!(
                    "{} has no source value at position {}",
                    canonical_key,
                    source_index + 1
                )
            })?;
            let stored_count = entry.stored_value_count_for_slot(source_index);
            if stored_count > 1 {
                plan.cardinality_warnings.push(format!(
                    "{} source {} collapsed {} stored values to its display value",
                    canonical_key,
                    source_index + 1,
                    stored_count
                ));
            }
            values.push(value.clone());
        }
        plan.fields.push(PlannedTransferField {
            item_key: canonical_transfer_item_key(&canonical_key, &entry.item_key),
            canonical_key,
            values,
        });
    }

    if plan.fields.is_empty() && plan.skipped_numbering_keys.is_empty() {
        return Err("tag transfer source contains no applicable text fields".to_string());
    }
    Ok(plan)
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
        .filter_map(|(index, issue)| issue.as_ref().map(|issue| (index, issue)))
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
        .filter(|entry| transfer_entry_selected(entry, scope, source_paths.len()))
        .collect())
}

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
            super::probe::write_all_tags_for_transfer_at_verification(
                path,
                changes,
                cancel,
                verification,
            )
        },
    )
}

fn execute_tag_transfer_from_entries_with_writer<F>(
    source_entries: &[TagEntry],
    source_count: usize,
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
        &[(lofty::tag::ItemKey, Option<String>)],
        Option<&super::probe::MetadataWriteCancelFlag>,
        tui_file_picker::VerificationMode,
    ) -> Result<super::probe::MetadataWriteCommitReport, String>,
{
    let value_plan = plan_transfer_values(
        source_entries,
        source_count,
        target_paths.len(),
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

    let mut report = TagTransferReport {
        source_count,
        target_count: target_paths.len(),
        skipped_numbering_keys: value_plan.skipped_numbering_keys,
        cardinality_warnings: value_plan.cardinality_warnings,
        ..TagTransferReport::default()
    };

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
        {
            report
                .failed
                .push((target_path.clone(), issue.reason.clone()));
            if let Some(progress) = progress {
                progress(target_index + 1, target_paths.len(), target_path);
            }
            continue;
        }

        let mut changes = Vec::new();
        for field in &value_plan.fields {
            let source_value = &field.values[target_index];
            let target_value = target_by_key
                .get(&field.canonical_key)
                .and_then(|entry| entry.per_file_values.get(target_index))
                .map(String::as_str)
                .unwrap_or("");
            if source_value == target_value {
                continue;
            }
            changes.push((field.item_key.clone(), Some(source_value.clone())));
        }
        if changes.is_empty() {
            report.unchanged += 1;
            if let Some(progress) = progress {
                progress(target_index + 1, target_paths.len(), target_path);
            }
            continue;
        }

        match writer(
            target_path,
            &changes,
            Some(cancel),
            verification,
        ) {
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
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>();
        let all_same = values.windows(2).all(|pair| pair[0] == pair[1]);
        let display = if all_same {
            values.first().cloned().unwrap_or_default()
        } else {
            "<multiple values>".to_string()
        };

        if let Some(row) = existing_row {
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
                per_file_originals: vec![String::new(); file_count],
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
    pub cardinality_warnings: Vec<String>,
}

impl EditorTransferApplyReport {
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

pub(crate) fn metadata_editor_transfer_snapshot(
    state: &super::app::MetadataEditorState,
) -> (Vec<TagEntry>, usize) {
    let surface = state.active_surface();
    let source_count = surface.paths.len();
    let entries = surface
        .entries
        .iter()
        .enumerate()
        .filter(|(index, _)| !surface.deleted.contains(index))
        .map(|(_, entry)| entry.clone())
        .collect();
    (entries, source_count)
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

pub(crate) fn apply_transfer_entries_to_editor(
    state: &mut super::app::MetadataEditorState,
    source_entries: &[TagEntry],
    source_count: usize,
    scope: super::app::TagTransferScope,
) -> Result<EditorTransferApplyReport, String> {
    let target_count = state.active_surface().paths.len();
    if source_count == 0 {
        return Err("tag transfer has no source files".to_string());
    }
    if target_count == 0 {
        return Err("metadata editor has no file targets".to_string());
    }
    if source_count != 1 && source_count != target_count {
        return Err(format!(
            "tag transfer requires 1 source or equal source/target counts; got {} sources and {} editor targets",
            source_count, target_count
        ));
    }

    #[derive(Debug)]
    struct Plan {
        key: String,
        item_key: lofty::tag::ItemKey,
        mode: FieldBlockValueMode,
        existing_row: Option<usize>,
        values: Vec<String>,
        cardinality_warnings: Vec<String>,
    }

    let mut target_rows = std::collections::HashMap::new();
    for (index, entry) in state.active_surface().entries.iter().enumerate() {
        let key = super::probe::canonical_metadata_display_key(&entry.display_key);
        if target_rows.insert(key.clone(), index).is_some() {
            return Err(format!(
                "metadata editor contains duplicate field {key}; resolve it before transferring tags"
            ));
        }
    }

    let broadcast = source_count == 1 && target_count > 1;
    let mut report = EditorTransferApplyReport::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut plans = Vec::new();
    for source_entry in source_entries {
        if !transfer_entry_selected(source_entry, scope, source_count) {
            continue;
        }
        let key = super::probe::canonical_metadata_display_key(&source_entry.display_key);
        if !seen.insert(key.clone()) {
            return Err(format!("tag transfer source contains duplicate field {key}"));
        }
        if broadcast && is_transfer_numbering_key(&key) {
            report.skipped_numbering_keys.push(key);
            continue;
        }
        let existing_row = target_rows.get(&key).copied();
        if existing_row
            .and_then(|index| state.active_surface().entries.get(index))
            .is_some_and(|entry| entry.is_track_scoped(target_count))
        {
            report.skipped_track_scoped.push(key);
            continue;
        }
        let mode = if source_count == 1 {
            FieldBlockValueMode::Broadcast
        } else {
            FieldBlockValueMode::Positional
        };
        let mut values = Vec::with_capacity(target_count);
        let mut warnings = Vec::new();
        for target_index in 0..target_count {
            let source_index = if source_count == 1 { 0 } else { target_index };
            let Some(value) = source_entry.per_file_values.get(source_index) else {
                return Err(format!(
                    "{} has no source value at position {}",
                    key,
                    source_index + 1
                ));
            };
            let stored_count = source_entry.stored_value_count_for_slot(source_index);
            if stored_count > 1 {
                warnings.push(format!(
                    "{} source {} collapsed {} stored values to its display value",
                    key,
                    source_index + 1,
                    stored_count
                ));
            }
            values.push(value.clone());
        }
        let item_key = canonical_transfer_item_key(&key, &source_entry.item_key);
        plans.push(Plan {
            key,
            item_key,
            mode,
            existing_row,
            values,
            cardinality_warnings: warnings,
        });
    }

    for plan in plans {
        let all_same = plan.values.windows(2).all(|pair| pair[0] == pair[1]);
        let display = if all_same {
            plan.values.first().cloned().unwrap_or_default()
        } else {
            "<multiple values>".to_string()
        };
        if let Some(row) = plan.existing_row {
            let surface = state.active_surface_mut();
            let entry = &mut surface.entries[row];
            entry.value = display;
            entry.is_mixed = !all_same;
            entry.per_file_values = plan.values;
            entry.mb_proposed_value = None;
            entry.mb_proposed_per_file = None;
            surface.deleted.retain(|deleted| *deleted != row);
        } else {
            state.active_surface_mut().entries.push(TagEntry {
                display_key: plan.key.clone(),
                item_key: plan.item_key,
                value: display,
                original: String::new(),
                is_binary: false,
                is_mixed: !all_same,
                has_multiple_stored_values: false,
                row_scope: super::probe::RowScope::File,
                per_file_stored_value_counts: vec![0; target_count],
                per_file_values: plan.values,
                per_file_originals: vec![String::new(); target_count],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
        }
        report.applied.push((plan.key, plan.mode));
        report.cardinality_warnings.extend(plan.cardinality_warnings);
    }

    if report.applied.is_empty()
        && report.skipped_numbering_keys.is_empty()
        && report.skipped_track_scoped.is_empty()
    {
        return Err("tag transfer source contains no applicable text fields".to_string());
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

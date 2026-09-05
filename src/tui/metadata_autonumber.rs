//! Metadata-editor numbering and count population.
//!
//! This module deliberately owns both the pure numbering rules and the
//! metadata-editor mutation boundary.  Context menus, command mode, and the
//! custom preview overlay all dispatch through the same functions so their
//! behavior cannot drift.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::app::MetadataEditorState;
use super::text_input::TextInputState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberingTarget {
    Track,
    Disc,
}

impl NumberingTarget {
    pub fn display_key(self) -> &'static str {
        match self {
            Self::Track => "TRACKNUMBER",
            Self::Disc => "DISCNUMBER",
        }
    }

    pub fn title(self) -> &'static str {
        self.display_key()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberingScheme {
    N,
    NN,
    NOverNN,
    NNOverNN,
    SN,
    SNN,
}

impl NumberingScheme {
    pub const ALL: [Self; 6] = [
        Self::N,
        Self::NN,
        Self::NOverNN,
        Self::NNOverNN,
        Self::SN,
        Self::SNN,
    ];

    pub const IMMEDIATE: [Self; 4] = [Self::N, Self::NN, Self::NOverNN, Self::NNOverNN];

    pub fn label(self) -> &'static str {
        match self {
            Self::N => "N",
            Self::NN => "NN",
            Self::NOverNN => "N/NN",
            Self::NNOverNN => "NN/NN",
            Self::SN => "SN",
            Self::SNN => "SNN",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_uppercase().as_str() {
            "N" => Some(Self::N),
            "NN" => Some(Self::NN),
            "N/NN" => Some(Self::NOverNN),
            "NN/NN" => Some(Self::NNOverNN),
            "SN" => Some(Self::SN),
            "SNN" => Some(Self::SNN),
            _ => None,
        }
    }

    pub fn is_side(self) -> bool {
        matches!(self, Self::SN | Self::SNN)
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|candidate| *candidate == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let idx = Self::ALL.iter().position(|candidate| *candidate == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoPopulateTarget {
    TrackTotal,
    DiscTotal,
    DiscNumber,
}

impl AutoPopulateTarget {
    pub fn parse(input: &str) -> Option<Self> {
        let normalized = input
            .trim()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match normalized.as_str() {
            "totaltracks" | "tracktotal" => Some(Self::TrackTotal),
            "totaldiscs" | "disctotal" => Some(Self::DiscTotal),
            "discnumber" | "disc" => Some(Self::DiscNumber),
            _ => None,
        }
    }

    fn display_key(self) -> &'static str {
        match self {
            Self::TrackTotal => "TRACKTOTAL",
            Self::DiscTotal => "DISCTOTAL",
            Self::DiscNumber => "DISCNUMBER",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationReport {
    pub field: String,
    pub changed: usize,
    pub unchanged: usize,
    pub blocked: usize,
    pub collapsed_slots: usize,
}

impl MutationReport {
    pub fn status(&self, operation: &str) -> String {
        let mut status = format!(
            "{} {}: {} changed",
            operation, self.field, self.changed
        );
        if self.unchanged > 0 {
            status.push_str(&format!(", {} unchanged", self.unchanged));
        }
        if self.blocked > 0 {
            status.push_str(&format!(", {} read-only", self.blocked));
        }
        if self.collapsed_slots > 0 {
            status.push_str(&format!(
                "; warning: {} source carrier{} collapsed multiple stored values",
                self.collapsed_slots,
                if self.collapsed_slots == 1 { "" } else { "s" },
            ));
        }
        status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SideDerivationSource {
    EmbeddedCue,
    SidecarCue,
    ExistingTag,
    Filename,
}

impl SideDerivationSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::EmbeddedCue => "embedded cue",
            Self::SidecarCue => "sidecar cue",
            Self::ExistingTag => "existing tags",
            Self::Filename => "filename",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedSideNumber {
    pub prefix: String,
    pub sequence: usize,
    pub source: SideDerivationSource,
}

/// Per-row side-number assignment carried into the custom overlay.  A
/// `derived_sequence` is authoritative evidence from the ordered resolver;
/// `None` means the preview must generate a deterministic sequence for this
/// prefix.  Manual prefix edits deliberately clear both source fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideNumberAssignment {
    pub prefix: String,
    pub derived_sequence: Option<usize>,
    pub source: Option<SideDerivationSource>,
}

/// Ordered source resolver.  The cue variants are intentional seams: they are
/// represented in the policy now, while their readers remain out of scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideNumberResolver {
    pub sources: Vec<SideDerivationSource>,
}

impl Default for SideNumberResolver {
    fn default() -> Self {
        Self {
            sources: vec![
                SideDerivationSource::EmbeddedCue,
                SideDerivationSource::SidecarCue,
                SideDerivationSource::ExistingTag,
                SideDerivationSource::Filename,
            ],
        }
    }
}

impl SideNumberResolver {
    pub fn resolve(
        &self,
        state: &MetadataEditorState,
        target: NumberingTarget,
        slot: usize,
    ) -> Option<DerivedSideNumber> {
        for source in &self.sources {
            let candidate = match source {
                // Deliberately unimplemented in this bounded round.  Keeping
                // these explicit avoids baking filename precedence into UI code.
                SideDerivationSource::EmbeddedCue | SideDerivationSource::SidecarCue => None,
                SideDerivationSource::ExistingTag => existing_tag_side_number(state, target, slot),
                SideDerivationSource::Filename => state
                    .active_surface()
                    .paths
                    .get(slot)
                    .and_then(|path| side_number_from_filename(path)),
            };
            if let Some(mut derived) = candidate {
                derived.source = *source;
                return Some(derived);
            }
        }
        None
    }
}

fn canonical_entry_index(state: &MetadataEditorState, key: &str) -> Option<usize> {
    state
        .active_surface()
        .entries
        .iter()
        .position(|entry| super::probe::canonical_metadata_display_key(&entry.display_key) == key)
}

fn existing_tag_side_number(
    state: &MetadataEditorState,
    target: NumberingTarget,
    slot: usize,
) -> Option<DerivedSideNumber> {
    let entry_idx = canonical_entry_index(state, target.display_key())?;
    let value = state
        .active_surface()
        .entries
        .get(entry_idx)?
        .per_file_values
        .get(slot)?;
    parse_side_number(value)
}

/// Recognize an explicit ASCII side label followed immediately by a positive
/// decimal sequence. The label may contain one to eight letters for custom or
/// existing-tag values. Filename inference applies the narrower one-letter
/// anchoring policy in [`side_number_from_filename`].
pub fn parse_side_number(input: &str) -> Option<DerivedSideNumber> {
    let trimmed = input.trim();
    let mut chars = trimmed.char_indices().peekable();
    let mut prefix_end = 0usize;
    let mut prefix_len = 0usize;
    while let Some(&(idx, ch)) = chars.peek() {
        if !ch.is_ascii_alphabetic() {
            break;
        }
        chars.next();
        prefix_end = idx + ch.len_utf8();
        prefix_len += 1;
    }
    if prefix_len == 0 || prefix_len > 8 {
        return None;
    }

    let digit_start = prefix_end;
    let mut digit_end = digit_start;
    while let Some(&(idx, ch)) = chars.peek() {
        if !ch.is_ascii_digit() {
            break;
        }
        chars.next();
        digit_end = idx + ch.len_utf8();
    }
    if digit_end == digit_start {
        return None;
    }

    let tail = trimmed[digit_end..].trim_start();
    let valid_tail = if tail.is_empty() {
        true
    } else if let Some(total) = tail.strip_prefix('/') {
        total
            .trim()
            .parse::<usize>()
            .ok()
            .is_some_and(|value| value > 0)
    } else {
        tail.starts_with('-')
            || tail.starts_with('_')
            || tail.starts_with('.')
            || tail.starts_with('–')
            || tail.starts_with('—')
    };
    if !valid_tail {
        return None;
    }

    let sequence = trimmed[digit_start..digit_end].parse::<usize>().ok()?;
    if sequence == 0 {
        return None;
    }
    Some(DerivedSideNumber {
        prefix: trimmed[..prefix_end].to_ascii_uppercase(),
        sequence,
        source: SideDerivationSource::ExistingTag,
    })
}

/// Continue one explicit numbering seed while preserving its lexical notation.
/// This is the pure continuation primitive used by inline `!` expansion;
/// structure/boundary decisions remain with the metadata editor.
pub fn continue_numbering_seed(seed: &str, increment: usize) -> Result<String, String> {
    let trimmed = seed.trim();
    if trimmed.is_empty() {
        return Err("numbering continuation needs a non-empty seed".to_string());
    }

    if let Some(side) = parse_side_number(trimmed) {
        let prefix_len = side.prefix.len();
        let raw_prefix = &trimmed[..prefix_len];
        let rest = &trimmed[prefix_len..];
        let digit_len = rest.bytes().take_while(|byte| byte.is_ascii_digit()).count();
        if digit_len == 0 || !rest[digit_len..].is_empty() {
            return Err(format!("cannot continue numbering from seed '{trimmed}'"));
        }
        let scheme = match digit_len {
            1 => NumberingScheme::SN,
            2 => NumberingScheme::SNN,
            _ => return Err(format!("cannot continue numbering from seed '{trimmed}'")),
        };
        let next = side
            .sequence
            .checked_add(increment)
            .ok_or_else(|| "numbering continuation overflowed".to_string())?;
        return Ok(format_side_numbering_value(
            scheme,
            raw_prefix,
            next,
            next,
        ));
    }

    let (number_text, total) = match trimmed.split_once('/') {
        Some((number, total)) => {
            if total.is_empty()
                || !total.bytes().all(|byte| byte.is_ascii_digit())
                || total.parse::<usize>().ok().filter(|value| *value > 0).is_none()
            {
                return Err(format!("cannot continue numbering from seed '{trimmed}'"));
            }
            (number, Some(total.parse::<usize>().unwrap_or_default()))
        }
        None => (trimmed, None),
    };
    if number_text.is_empty() || !number_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("cannot continue numbering from seed '{trimmed}'"));
    }
    let number = number_text
        .parse::<usize>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| format!("cannot continue numbering from seed '{trimmed}'"))?;
    let next = number
        .checked_add(increment)
        .ok_or_else(|| "numbering continuation overflowed".to_string())?;
    let padded = match number_text.len() {
        1 => false,
        2 => true,
        _ => return Err(format!("cannot continue numbering from seed '{trimmed}'")),
    };
    let scheme = match (padded, total.is_some()) {
        (false, false) => NumberingScheme::N,
        (true, false) => NumberingScheme::NN,
        (false, true) => NumberingScheme::NOverNN,
        (true, true) => NumberingScheme::NNOverNN,
    };
    let total_or_maximum = if let Some(total) = total {
        if next > total {
            return Err(format!(
                "cannot continue numbering past declared total {total} from seed '{trimmed}'"
            ));
        }
        total
    } else {
        next
    };
    Ok(format_non_side_numbering_value(
        scheme,
        next,
        total_or_maximum,
    ))
}

/// Continue a side-style seed onto a later declared partition, resetting the
/// numeric sequence while advancing the seed's alphabetic side prefix. The
/// caller supplies the declared partition offset; this helper never invents a
/// boundary on its own.
pub fn continue_side_numbering_seed(
    seed: &str,
    prefix_increment: usize,
    sequence: usize,
) -> Result<String, String> {
    let trimmed = seed.trim();
    let side = parse_side_number(trimmed)
        .ok_or_else(|| format!("cannot continue side numbering from seed '{trimmed}'"))?;
    let prefix_len = side.prefix.len();
    let raw_prefix = &trimmed[..prefix_len];
    let rest = &trimmed[prefix_len..];
    let digit_len = rest.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    if digit_len == 0 || !rest[digit_len..].is_empty() || sequence == 0 {
        return Err(format!("cannot continue side numbering from seed '{trimmed}'"));
    }
    let scheme = match digit_len {
        1 => NumberingScheme::SN,
        2 => NumberingScheme::SNN,
        _ => return Err(format!("cannot continue side numbering from seed '{trimmed}'")),
    };
    let prefix = advance_ascii_side_prefix(raw_prefix, prefix_increment)?;
    Ok(format_side_numbering_value(
        scheme,
        &prefix,
        sequence,
        sequence,
    ))
}

fn advance_ascii_side_prefix(prefix: &str, increment: usize) -> Result<String, String> {
    if prefix.is_empty() || !prefix.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(format!("invalid side prefix '{prefix}'"));
    }
    if increment == 0 {
        return Ok(prefix.to_string());
    }

    // Spreadsheet-style alphabetic succession gives the familiar A -> B ->
    // ... -> Z -> AA sequence without imposing a one-letter-only policy.
    let lowercase = prefix.bytes().all(|byte| byte.is_ascii_lowercase());
    let mut ordinal = 0u64;
    for byte in prefix.bytes() {
        let digit = u64::from(byte.to_ascii_uppercase() - b'A' + 1);
        ordinal = ordinal
            .checked_mul(26)
            .and_then(|value| value.checked_add(digit))
            .ok_or_else(|| "side prefix continuation overflowed".to_string())?;
    }
    ordinal = ordinal
        .checked_add(u64::try_from(increment).unwrap_or(u64::MAX))
        .ok_or_else(|| "side prefix continuation overflowed".to_string())?;

    let mut reversed = Vec::new();
    while ordinal > 0 {
        ordinal -= 1;
        reversed.push((ordinal % 26) as u8);
        ordinal /= 26;
    }
    if reversed.len() > 8 {
        return Err("side prefix continuation exceeds the 8-letter numbering limit".to_string());
    }
    reversed.reverse();
    Ok(reversed
        .into_iter()
        .map(|digit| {
            let byte = (if lowercase { b'a' } else { b'A' }) + digit;
            char::from(byte)
        })
        .collect())
}

pub fn side_number_from_filename(path: &Path) -> Option<DerivedSideNumber> {
    let stem = path.file_stem()?.to_str()?;
    let mut chars = stem.chars();
    let side = chars.next()?;
    let first_digit = chars.next()?;
    if !side.is_ascii_alphabetic() || !first_digit.is_ascii_digit() {
        return None;
    }

    let mut derived = parse_side_number(stem)?;
    // Filename inference is intentionally narrower than explicit/custom side
    // labels: one leading side letter avoids treating stems such as
    // `trackA01` as side-number evidence.
    if derived.prefix.len() != 1 {
        return None;
    }
    derived.source = SideDerivationSource::Filename;
    Some(derived)
}

fn decimal_width(total: usize) -> usize {
    total.max(1).to_string().len().max(2)
}

fn format_non_side_numbering_value(
    scheme: NumberingScheme,
    sequence: usize,
    total: usize,
) -> String {
    let width = decimal_width(total);
    match scheme {
        NumberingScheme::N => sequence.to_string(),
        NumberingScheme::NN => format!("{sequence:0width$}"),
        NumberingScheme::NOverNN => format!("{sequence}/{total:0width$}"),
        NumberingScheme::NNOverNN => {
            format!("{sequence:0width$}/{total:0width$}")
        }
        NumberingScheme::SN | NumberingScheme::SNN => unreachable!(),
    }
}

fn format_side_numbering_value(
    scheme: NumberingScheme,
    prefix: &str,
    sequence: usize,
    maximum: usize,
) -> String {
    match scheme {
        NumberingScheme::SN => format!("{prefix}{sequence}"),
        NumberingScheme::SNN => {
            let width = decimal_width(maximum);
            format!("{prefix}{sequence:0width$}")
        }
        _ => unreachable!(),
    }
}

pub fn format_numbering_values(
    scheme: NumberingScheme,
    count: usize,
    prefixes: Option<&[String]>,
) -> Result<Vec<String>, String> {
    format_numbering_values_with_sequences(scheme, count, prefixes, None)
}

fn format_numbering_values_with_sequences(
    scheme: NumberingScheme,
    count: usize,
    prefixes: Option<&[String]>,
    derived_sequences: Option<&[Option<usize>]>,
) -> Result<Vec<String>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if !scheme.is_side() {
        return Ok((1..=count)
            .map(|sequence| format_non_side_numbering_value(scheme, sequence, count))
            .collect());
    }

    let prefixes = prefixes.ok_or_else(|| "side numbering requires prefixes".to_string())?;
    if prefixes.len() != count {
        return Err(format!(
            "side prefix dimension mismatch: {} prefixes for {} rows",
            prefixes.len(), count
        ));
    }
    if prefixes.iter().any(|prefix| !valid_prefix(prefix)) {
        return Err("side prefixes must contain 1-8 ASCII letters".to_string());
    }
    let generated_sequences;
    let derived_sequences = match derived_sequences {
        Some(sequences) => {
            if sequences.len() != count {
                return Err(format!(
                    "side sequence dimension mismatch: {} sequences for {} rows",
                    sequences.len(), count
                ));
            }
            sequences
        }
        None => {
            generated_sequences = vec![None; count];
            generated_sequences.as_slice()
        }
    };

    // Reserve every source-derived sequence before assigning generated rows,
    // including evidence that appears later in editor order.  This preserves
    // shuffled/non-contiguous source numbering and prevents generated rows from
    // colliding with authoritative values.
    let mut used: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for (slot, (prefix, sequence)) in prefixes.iter().zip(derived_sequences).enumerate() {
        let Some(sequence) = *sequence else {
            continue;
        };
        if sequence == 0 {
            return Err(format!("side sequence for row {} must be positive", slot + 1));
        }
        if !used.entry(prefix.as_str()).or_default().insert(sequence) {
            return Err(format!(
                "duplicate source-derived side sequence {prefix}{sequence}"
            ));
        }
    }

    let mut next_generated: BTreeMap<&str, usize> = BTreeMap::new();
    let mut assigned = Vec::with_capacity(count);
    for (prefix, derived) in prefixes.iter().zip(derived_sequences) {
        let sequence = if let Some(sequence) = *derived {
            sequence
        } else {
            let next = next_generated.entry(prefix.as_str()).or_insert(1);
            let group_used = used.entry(prefix.as_str()).or_default();
            while group_used.contains(next) {
                *next += 1;
            }
            let sequence = *next;
            group_used.insert(sequence);
            *next += 1;
            sequence
        };
        assigned.push(sequence);
    }

    let mut maximum_by_prefix: BTreeMap<&str, usize> = BTreeMap::new();
    for (prefix, sequence) in prefixes.iter().zip(&assigned) {
        maximum_by_prefix
            .entry(prefix.as_str())
            .and_modify(|maximum| *maximum = (*maximum).max(*sequence))
            .or_insert(*sequence);
    }

    Ok(prefixes
        .iter()
        .zip(assigned)
        .map(|(prefix, sequence)| {
            format_side_numbering_value(
                scheme,
                prefix,
                sequence,
                *maximum_by_prefix.get(prefix.as_str()).unwrap_or(&1),
            )
        })
        .collect())
}

fn valid_prefix(prefix: &str) -> bool {
    let len = prefix.len();
    (1..=8).contains(&len) && prefix.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn normalize_prefix(input: &str) -> Result<String, String> {
    let prefix = input.trim().to_ascii_uppercase();
    if valid_prefix(&prefix) {
        Ok(prefix)
    } else {
        Err("prefix must contain 1-8 ASCII letters".to_string())
    }
}

fn scheme_representation(
    scheme: NumberingScheme,
) -> crate::metadata_persistence::MetadataNumberingRepresentation {
    use crate::metadata_persistence::MetadataNumberingRepresentation;

    match scheme {
        NumberingScheme::N => MetadataNumberingRepresentation::PlainUnsigned,
        NumberingScheme::NOverNN | NumberingScheme::NNOverNN => {
            MetadataNumberingRepresentation::NumericFraction
        }
        NumberingScheme::NN => MetadataNumberingRepresentation::PaddedUnsigned,
        NumberingScheme::SN | NumberingScheme::SNN => MetadataNumberingRepresentation::Lexical
    }
}

fn carrier_label(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_else(|| "extensionless file".to_string())
}

#[derive(Debug, Clone)]
struct NumberingCarrier {
    label: String,
    persistence_label: &'static str,
    capabilities: crate::metadata_persistence::MetadataNumberingCapabilities,
}

#[derive(Debug, Clone)]
struct NumberingTargetContext {
    entry_idx: usize,
    writable_slots: Vec<usize>,
    carriers: Vec<NumberingCarrier>,
    capabilities: crate::metadata_persistence::MetadataNumberingCapabilities,
}

impl NumberingTargetContext {
    fn require(
        &self,
        display_key: &str,
        representation: crate::metadata_persistence::MetadataNumberingRepresentation,
    ) -> Result<(), String> {
        if self.capabilities.supports(representation) {
            return Ok(());
        }

        let incompatible = self
            .carriers
            .iter()
            .filter(|carrier| !carrier.capabilities.supports(representation))
            .map(|carrier| format!("{} ({})", carrier.label, carrier.persistence_label))
            .collect::<BTreeSet<_>>();
        let requirement = match representation {
            crate::metadata_persistence::MetadataNumberingRepresentation::PlainUnsigned => {
                "plain unsigned numbering values"
            }
            crate::metadata_persistence::MetadataNumberingRepresentation::PaddedUnsigned => {
                "padded unsigned numbering values"
            }
            crate::metadata_persistence::MetadataNumberingRepresentation::NumericFraction => {
                "numeric fraction numbering values"
            }
            crate::metadata_persistence::MetadataNumberingRepresentation::Lexical => {
                "lexical numbering values"
            }
        };
        Err(format!(
            "{display_key} requires {requirement}; incompatible carrier{}: {}",
            if incompatible.len() == 1 { "" } else { "s" },
            incompatible.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

fn numbering_uses_textual_sidecar(state: &MetadataEditorState) -> bool {
    // Parsed optical-disc presentations persist through their dedicated
    // metadata sidecars/metabase rather than through the repeated virtual
    // source path exposed per track. CUE albums are intentionally *not*
    // included here: numbering fields are ordinary audio-tag metadata there,
    // and the generated CUE projection does not losslessly represent them.
    state.active_surface().technical_details.disc.is_some()
}

fn numbering_target_context(
    state: &MetadataEditorState,
    display_key: &str,
) -> Result<NumberingTargetContext, String> {
    if state.read_only {
        return Err("metadata editor is read-only".to_string());
    }
    let entry_idx = canonical_entry_index(state, display_key)
        .ok_or_else(|| format!("metadata editor has no {display_key} field"))?;
    if let Some(reason) =
        super::keybindings::metadata_editor_unpersistable_per_track_reason(state, entry_idx)
    {
        return Err(reason);
    }

    let surface = state.active_surface();
    let entry = &surface.entries[entry_idx];
    if entry.is_track_scoped(surface.paths.len()) {
        return Err(format!(
            "cannot auto-number per-track {display_key} on an embedded CUE surface; \
             CUE TRACK numbers are positional"
        ));
    }
    if entry.per_file_values.len() != surface.paths.len() {
        return Err(format!(
            "{display_key} has {} values for {} file carriers",
            entry.per_file_values.len(),
            surface.paths.len()
        ));
    }

    let textual_sidecar = numbering_uses_textual_sidecar(state);
    let writable_slots = if textual_sidecar {
        // Disc metadata sidecars are the persistence authority. The source image
        // may itself be an ISO, directory, or read-only audio carrier, none of
        // which describes the numbering representation accepted by the
        // sidecar. `state.read_only` above is the authoritative write gate.
        (0..surface.paths.len()).collect::<Vec<_>>()
    } else {
        (0..surface.paths.len())
            .filter(|slot| super::keybindings::metadata_editor_slot_is_writable(state, *slot))
            .collect::<Vec<_>>()
    };
    if writable_slots.is_empty() {
        return Err("no writable files in this metadata editor session".to_string());
    }

    let mut capabilities: Option<
        crate::metadata_persistence::MetadataNumberingCapabilities,
    > = None;
    let mut carriers = Vec::with_capacity(writable_slots.len());
    for slot in &writable_slots {
        let path = surface
            .paths
            .get(*slot)
            .ok_or_else(|| format!("missing metadata carrier path for slot {}", slot + 1))?;
        let (carrier_capabilities, persistence_label) = if textual_sidecar {
            (
                crate::metadata_persistence::MetadataNumberingCapabilities::TEXTUAL,
                "metadata sidecar",
            )
        } else {
            let capability =
                crate::metadata_persistence::metadata_numbering_capability_for_path(path)?;
            (capability.capabilities, capability.backend.label())
        };
        capabilities = Some(match capabilities {
            Some(current) => current.intersection(carrier_capabilities),
            None => carrier_capabilities,
        });
        carriers.push(NumberingCarrier {
            label: if textual_sidecar {
                "sidecar".to_string()
            } else {
                carrier_label(path)
            },
            persistence_label,
            capabilities: carrier_capabilities,
        });
    }
    let capabilities = capabilities
        .ok_or_else(|| "no writable metadata carriers were classified".to_string())?;

    Ok(NumberingTargetContext {
        entry_idx,
        writable_slots,
        carriers,
        capabilities,
    })
}

fn require_numbering_representation(
    state: &MetadataEditorState,
    display_key: &str,
    representation: crate::metadata_persistence::MetadataNumberingRepresentation,
) -> Result<(usize, Vec<usize>), String> {
    let context = numbering_target_context(state, display_key)?;
    context.require(display_key, representation)?;
    Ok((context.entry_idx, context.writable_slots))
}

/// Validate the complete editor target before exposing or applying a scheme.
/// The persistence backend owns representation capabilities; mixed selections
/// use their intersection and unknown backends fail closed.
pub fn numbering_scheme_capability(
    state: &MetadataEditorState,
    target: NumberingTarget,
    scheme: NumberingScheme,
) -> Result<(), String> {
    require_numbering_representation(
        state,
        target.display_key(),
        scheme_representation(scheme),
    )
    .map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberingMenuEligibility {
    pub immediate: Vec<NumberingScheme>,
    pub custom: bool,
}

/// Compute every row-menu choice from one execution-grade target snapshot.
/// Generic carriers are probed once, then each scheme is filtered by the same
/// representation and proposed-value checks used at the mutation boundary.
pub fn numbering_menu_eligibility(
    state: &MetadataEditorState,
    target: NumberingTarget,
) -> Result<NumberingMenuEligibility, String> {
    let context = numbering_target_context(state, target.display_key())?;
    let count = row_dimension(state, context.entry_idx);
    let mut immediate = Vec::new();
    for scheme in NumberingScheme::IMMEDIATE {
        if !context.capabilities.supports(scheme_representation(scheme)) {
            continue;
        }
        let values = format_numbering_values(scheme, count, None)?;
        if values_have_writable_effect(
            state,
            context.entry_idx,
            &context.writable_slots,
            &values,
            true,
        ) {
            immediate.push(scheme);
        }
    }
    Ok(NumberingMenuEligibility {
        immediate,
        custom: context
            .capabilities
            .supports(crate::metadata_persistence::MetadataNumberingRepresentation::Lexical),
    })
}

fn values_have_writable_effect(
    state: &MetadataEditorState,
    entry_idx: usize,
    writable_slots: &[usize],
    values: &[String],
    restore_deleted: bool,
) -> bool {
    let entry = &state.active_surface().entries[entry_idx];
    writable_slots.iter().any(|slot| {
        entry
            .per_file_values
            .get(*slot)
            .zip(values.get(*slot))
            .is_some_and(|(current, proposed)| current != proposed)
    }) || (restore_deleted
        && state.active_surface().deleted.contains(&entry_idx)
        && !writable_slots.is_empty())
}

/// Presentation eligibility for an immediate scheme.  It uses the exact same
/// capability validation and proposed values as execution, then suppresses an
/// idempotent no-op from menus. Direct execution remains safely idempotent.
pub fn numbering_scheme_has_useful_effect(
    state: &MetadataEditorState,
    target: NumberingTarget,
    scheme: NumberingScheme,
) -> Result<bool, String> {
    let (entry_idx, writable_slots) = require_numbering_representation(
        state,
        target.display_key(),
        scheme_representation(scheme),
    )?;
    let count = row_dimension(state, entry_idx);
    let values = if scheme.is_side() {
        values_from_assignments(scheme, count, &default_side_assignments(state, target))?
    } else {
        format_numbering_values(scheme, count, None)?
    };
    Ok(values_have_writable_effect(
        state,
        entry_idx,
        &writable_slots,
        &values,
        true,
    ))
}

fn row_dimension(state: &MetadataEditorState, entry_idx: usize) -> usize {
    state
        .active_surface()
        .entries
        .get(entry_idx)
        .map(|entry| entry.per_file_values.len())
        .unwrap_or(0)
}

fn apply_values(
    state: &mut MetadataEditorState,
    display_key: &str,
    values: Vec<String>,
    restore_deleted: bool,
) -> Result<MutationReport, String> {
    if state.read_only {
        return Err("metadata editor is read-only".to_string());
    }
    let entry_idx = canonical_entry_index(state, display_key)
        .ok_or_else(|| format!("metadata editor has no {} field", display_key))?;
    if let Some(reason) = super::keybindings::metadata_editor_unpersistable_per_track_reason(
        state,
        entry_idx,
    ) {
        return Err(reason);
    }

    let dim = row_dimension(state, entry_idx);
    if dim != values.len() {
        return Err(format!(
            "{} dimension changed while numbering ({} rows, {} values)",
            display_key,
            dim,
            values.len()
        ));
    }

    let path_count = state.active_surface().paths.len();
    let file_scoped = state
        .active_surface()
        .entries
        .get(entry_idx)
        .is_some_and(|entry| !entry.is_track_scoped(path_count) && dim == path_count);
    let writable: Vec<bool> = if file_scoped && numbering_uses_textual_sidecar(state) {
        vec![true; dim]
    } else if file_scoped {
        (0..dim)
            .map(|slot| super::keybindings::metadata_editor_slot_is_writable(state, slot))
            .collect()
    } else {
        vec![true; dim]
    };

    let collapsed_slots = state.active_surface().entries[entry_idx]
        .stored_value_collapse_slots(
            values
                .iter()
                .enumerate()
                .filter(|(slot, _)| writable.get(*slot).copied().unwrap_or(false))
                .map(|(slot, value)| (slot, value.as_str())),
        )
        .len();

    let mut changed = 0usize;
    let mut unchanged = 0usize;
    let mut blocked = 0usize;
    {
        let entry = &mut state.active_surface_mut().entries[entry_idx];
        for (slot, replacement) in values.into_iter().enumerate() {
            if !writable.get(slot).copied().unwrap_or(false) {
                blocked += 1;
                continue;
            }
            let Some(current) = entry.per_file_values.get_mut(slot) else {
                continue;
            };
            if current.as_str() == replacement {
                unchanged += 1;
            } else {
                current.replace_scalar(replacement);
                changed += 1;
            }
        }
        super::keybindings::metadata_editor_recompute_entry_display(entry);
    }
    if restore_deleted && changed.saturating_add(unchanged) > 0 {
        state.active_surface_mut().deleted.retain(|idx| *idx != entry_idx);
    }
    state.recompute_active_dirty();

    Ok(MutationReport {
        field: display_key.to_string(),
        changed,
        unchanged,
        blocked,
        collapsed_slots,
    })
}

pub fn default_side_assignments(
    state: &MetadataEditorState,
    target: NumberingTarget,
) -> Vec<SideNumberAssignment> {
    let count = canonical_entry_index(state, target.display_key())
        .map(|idx| row_dimension(state, idx))
        .unwrap_or_else(|| state.active_surface().paths.len());
    let resolver = SideNumberResolver::default();
    (0..count)
        .map(|slot| {
            if let Some(derived) = resolver.resolve(state, target, slot) {
                SideNumberAssignment {
                    prefix: derived.prefix,
                    derived_sequence: Some(derived.sequence),
                    source: Some(derived.source),
                }
            } else {
                // Product rule: the manual surface starts at A but never infers
                // later sides. The sequence is generated only for preview/apply.
                SideNumberAssignment {
                    prefix: "A".to_string(),
                    derived_sequence: None,
                    source: None,
                }
            }
        })
        .collect()
}

fn values_from_assignments(
    scheme: NumberingScheme,
    count: usize,
    assignments: &[SideNumberAssignment],
) -> Result<Vec<String>, String> {
    let prefixes = assignments
        .iter()
        .map(|assignment| assignment.prefix.clone())
        .collect::<Vec<_>>();
    let sequences = assignments
        .iter()
        .map(|assignment| assignment.derived_sequence)
        .collect::<Vec<_>>();
    format_numbering_values_with_sequences(
        scheme,
        count,
        scheme.is_side().then_some(prefixes.as_slice()),
        scheme.is_side().then_some(sequences.as_slice()),
    )
}

pub fn apply_numbering(
    state: &mut MetadataEditorState,
    target: NumberingTarget,
    scheme: NumberingScheme,
    explicit_prefix: Option<&str>,
) -> Result<MutationReport, String> {
    numbering_scheme_capability(state, target, scheme)?;
    let entry_idx = canonical_entry_index(state, target.display_key())
        .ok_or_else(|| format!("metadata editor has no {} field", target.display_key()))?;
    let count = row_dimension(state, entry_idx);
    let values = if scheme.is_side() {
        let mut assignments = default_side_assignments(state, target);
        if let Some(prefix) = explicit_prefix {
            let prefix = normalize_prefix(prefix)?;
            for assignment in &mut assignments {
                assignment.prefix = prefix.clone();
                assignment.derived_sequence = None;
                assignment.source = None;
            }
        } else {
            let derived_count = assignments
                .iter()
                .filter(|assignment| assignment.source.is_some())
                .count();
            if derived_count > 0 && derived_count < count {
                return Err(format!(
                    "side derivation resolved {derived_count} of {count} rows; use an explicit prefix or Custom… to resolve the remainder"
                ));
            }
        }
        values_from_assignments(scheme, count, &assignments)?
    } else {
        format_numbering_values(scheme, count, None)?
    };
    apply_values(state, target.display_key(), values, true)
}

pub fn apply_numbering_with_assignments(
    state: &mut MetadataEditorState,
    target: NumberingTarget,
    scheme: NumberingScheme,
    assignments: &[SideNumberAssignment],
) -> Result<MutationReport, String> {
    numbering_scheme_capability(state, target, scheme)?;
    let entry_idx = canonical_entry_index(state, target.display_key())
        .ok_or_else(|| format!("metadata editor has no {} field", target.display_key()))?;
    let count = row_dimension(state, entry_idx);
    if assignments.len() != count {
        return Err(format!(
            "side assignment dimension mismatch: {} assignments for {} rows",
            assignments.len(), count
        ));
    }
    let values = if scheme.is_side() {
        values_from_assignments(scheme, count, assignments)?
    } else {
        format_numbering_values(scheme, count, None)?
    };
    apply_values(state, target.display_key(), values, true)
}

fn parse_positive_component(value: &str) -> Option<usize> {
    let head = value.trim().split('/').next()?.trim();
    let parsed = head.parse::<usize>().ok()?;
    (parsed > 0).then_some(parsed)
}

/// Semantic track count for Auto-populate. The storage dimension of a row is
/// deliberately not used as the source of truth: one carrier can represent
/// many tracks (single-image CUE, ISO), while a unified CUE surface can have a
/// track-row dimension distinct from its file/save dimension.
fn semantic_track_count(state: &MetadataEditorState) -> usize {
    let surface = state.active_surface();
    let disc_count = surface
        .technical_details
        .disc
        .as_ref()
        .map(|disc| disc.track_count)
        .unwrap_or(0);
    let cue_count = surface
        .cue_album_synthetic_sheet
        .as_ref()
        .map(|sheet| sheet.track_sources.len())
        .unwrap_or(0);
    let explicit_track_rows = surface
        .entries
        .iter()
        .filter(|entry| matches!(entry.row_scope, super::probe::RowScope::Track))
        .map(|entry| entry.per_file_values.len())
        .max()
        .unwrap_or(0);

    disc_count
        .max(cue_count)
        .max(explicit_track_rows)
        .max(surface.file_labels.len())
        .max(surface.paths.len())
        .max(1)
}

fn cue_structural_disc_numbers(state: &MetadataEditorState, dim: usize) -> Option<Vec<usize>> {
    let surface = state.active_surface();
    let sheet = surface.cue_album_synthetic_sheet.as_ref()?;
    if dim != surface.paths.len() {
        return None;
    }

    // CUE paths are physical-disc authorities. Preserve their declared order
    // and map each save carrier back to the CUE that owns its tracks. This
    // handles one-image, multi-file, and aggregate multi-CUE albums without
    // deriving disc identity from filenames or directory names.
    let mut cue_order = Vec::<&Path>::new();
    for cue_path in &sheet.cue_paths {
        if !cue_order.iter().any(|existing| *existing == cue_path.as_path()) {
            cue_order.push(cue_path.as_path());
        }
    }
    for source in &sheet.track_sources {
        if !cue_order
            .iter()
            .any(|existing| *existing == source.cue_path.as_path())
        {
            cue_order.push(source.cue_path.as_path());
        }
    }
    if cue_order.is_empty() {
        return None;
    }

    surface
        .paths
        .iter()
        .map(|path| {
            let mut owner = None;
            for source in &sheet.track_sources {
                if source.audio_path != *path {
                    continue;
                }
                let disc = cue_order
                    .iter()
                    .position(|cue| *cue == source.cue_path.as_path())?
                    + 1;
                match owner {
                    None => owner = Some(disc),
                    Some(existing) if existing == disc => {}
                    Some(_) => return None,
                }
            }
            owner
        })
        .collect()
}

/// Parse an explicit conventional disc-directory name.
///
/// This is intentionally narrower than general filename/disc heuristics: the
/// plain-file Auto-populate fallback may use only structural directory
/// evidence, never mutable tag values or fuzzy album-name inference.
fn explicit_disc_directory_number(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_str()?.trim().to_ascii_lowercase();
    for prefix in ["disc", "disk", "cd"] {
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let digits = rest.trim_start_matches(|ch: char| matches!(ch, ' ' | '-' | '_'));
        if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let number = digits.parse::<usize>().ok()?;
        if number > 0 {
            return Some(number);
        }
    }
    None
}

fn common_parent_directory(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut common = paths.first()?.parent()?.to_path_buf();
    for path in &paths[1..] {
        let parent = path.parent()?;
        while !parent.starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    Some(common)
}

/// Derive disc assignments for an ordinary file-backed editor surface only
/// when the selected files provide coherent, explicit directory structure.
///
/// Supported shapes are either one explicitly numbered common directory
/// (`.../Disc 2/*.flac`) or sibling numbered disc directories immediately
/// below one non-root album ancestor (`.../album/Disc 1/...`,
/// `.../album/Disc 2/...`). Anything ambiguous falls back to the historical
/// one-disc behavior in `semantic_disc_numbers`.
fn plain_file_structural_disc_numbers(paths: &[PathBuf], dim: usize) -> Option<Vec<usize>> {
    if paths.len() != dim || paths.is_empty() {
        return None;
    }

    let common = common_parent_directory(paths)?;
    if let Some(number) = explicit_disc_directory_number(&common) {
        return Some(vec![number; dim]);
    }

    // A filesystem root is not meaningful album identity. Rejecting it also
    // keeps unrelated trees such as `/Disc 7/album/...` and `/Disk_8/album/...`
    // from being mistaken for one multi-disc album merely because `/` is
    // their only common ancestor.
    if common.file_name().is_none() {
        return None;
    }

    let numbers = paths
        .iter()
        .map(|path| {
            let parent = path.parent()?;
            let relative_parent = parent.strip_prefix(&common).ok()?;
            let disc_dir = relative_parent.components().next()?.as_os_str();
            explicit_disc_directory_number(Path::new(disc_dir))
        })
        .collect::<Option<Vec<_>>>()?;

    // Sibling-directory inference is useful only when it actually proves a
    // multi-disc structure. One-disc selections are handled either by the
    // explicitly numbered common-directory case above or by the conservative
    // fallback below.
    if numbers.iter().copied().collect::<BTreeSet<_>>().len() < 2 {
        return None;
    }

    Some(numbers)
}

/// Disc assignment for the active editor surface. A parsed disc presentation
/// is one physical disc even though it exposes one virtual path slot per
/// track. CUE-backed surfaces derive disc grouping from their parsed CUE
/// authorities. Plain file-backed surfaces may also derive disc grouping from
/// coherent explicit numbered disc directories; otherwise they remain one
/// logical disc. Existing DISCNUMBER values are mutable edit state and are
/// therefore never used as structural evidence by Auto-populate.
fn semantic_disc_numbers(state: &MetadataEditorState, dim: usize) -> Vec<usize> {
    let surface = state.active_surface();
    if surface.technical_details.disc.is_some() {
        return vec![1; dim];
    }
    if let Some(numbers) = cue_structural_disc_numbers(state, dim) {
        return numbers;
    }
    if surface.cue_source.is_some() || surface.pending_sidecar_cue_creation {
        return vec![1; dim];
    }

    if let Some(numbers) = plain_file_structural_disc_numbers(&surface.paths, dim) {
        return numbers;
    }

    // Without coherent structural directory evidence, a plain file-backed
    // surface is one logical disc. Existing DISCNUMBER values are edit state,
    // not structural evidence for Auto-populate: using a stale per-track
    // series here is the exact failure mode this operation must eliminate.
    vec![1; dim]
}

fn semantic_disc_total(state: &MetadataEditorState, dim: usize) -> usize {
    semantic_disc_numbers(state, dim)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .len()
        .max(1)
}

/// Whether the DISCNUMBER row has enough evidence for Auto-populate to be
/// advertised in the row context menu.
///
/// Execution deliberately does not use this gate: direct Auto-populate must
/// retain the repair behavior that collapses an unstructured/stale disc series
/// to one logical disc. The menu, however, should not propose a guessed `1`
/// when neither the source structure nor the field itself provides disc
/// evidence.
fn disc_number_offer_has_evidence(state: &MetadataEditorState, entry_idx: usize) -> bool {
    let surface = state.active_surface();
    let dim = row_dimension(state, entry_idx);

    let has_structural_evidence = surface.technical_details.disc.is_some()
        || surface.cue_source.is_some()
        || surface.cue_album_synthetic_sheet.is_some()
        || surface.pending_sidecar_cue_creation
        || plain_file_structural_disc_numbers(&surface.paths, dim).is_some();
    if has_structural_evidence {
        return true;
    }

    let entry = &surface.entries[entry_idx];
    entry
        .per_file_values
        .iter()
        .map(|value| value.as_str())
        .chain(entry.per_file_originals.iter().map(|value| value.as_str()))
        .chain(std::iter::once(entry.value.as_str()))
        .chain(std::iter::once(entry.original.as_str()))
        .any(|value| parse_positive_component(value).is_some())
}

#[derive(Debug, Clone)]
struct AutoPopulatePlan {
    entry_idx: usize,
    writable_slots: Vec<usize>,
    values: Vec<String>,
    restore_deleted: bool,
}

impl AutoPopulatePlan {
    fn has_useful_effect(&self, state: &MetadataEditorState) -> bool {
        values_have_writable_effect(
            state,
            self.entry_idx,
            &self.writable_slots,
            &self.values,
            self.restore_deleted,
        )
    }
}

fn plan_auto_populate(
    state: &MetadataEditorState,
    target: AutoPopulateTarget,
) -> Result<AutoPopulatePlan, String> {
    let key = target.display_key();
    let (entry_idx, writable_slots) = require_numbering_representation(
        state,
        key,
        crate::metadata_persistence::MetadataNumberingRepresentation::PlainUnsigned,
    )?;
    let dim = row_dimension(state, entry_idx);
    let (values, restore_deleted) = match target {
        AutoPopulateTarget::TrackTotal => {
            let count = semantic_track_count(state);
            (vec![count.to_string(); dim], true)
        }
        AutoPopulateTarget::DiscNumber => (
            semantic_disc_numbers(state, dim)
                .into_iter()
                .map(|number| number.to_string())
                .collect(),
            true,
        ),
        AutoPopulateTarget::DiscTotal => {
            let total = semantic_disc_total(state, dim);
            (vec![total.to_string(); dim], true)
        }
    };
    Ok(AutoPopulatePlan {
        entry_idx,
        writable_slots,
        values,
        restore_deleted,
    })
}

/// Return whether Auto populate should be offered and has at least one
/// writable, persistence-safe effect. DISCNUMBER additionally requires source
/// or field evidence before the menu advertises the operation; direct
/// execution intentionally remains available for repair workflows.
pub fn auto_populate_has_useful_effect(
    state: &MetadataEditorState,
    target: AutoPopulateTarget,
) -> Result<bool, String> {
    let plan = plan_auto_populate(state, target)?;
    if target == AutoPopulateTarget::DiscNumber
        && !disc_number_offer_has_evidence(state, plan.entry_idx)
    {
        return Ok(false);
    }
    Ok(plan.has_useful_effect(state))
}

pub fn auto_populate(
    state: &mut MetadataEditorState,
    target: AutoPopulateTarget,
) -> Result<MutationReport, String> {
    let plan = plan_auto_populate(state, target)?;
    apply_values(
        state,
        target.display_key(),
        plan.values,
        plan.restore_deleted,
    )
}

#[derive(Debug, Clone)]
pub struct AutoNumberOverlayState {
    pub target: NumberingTarget,
    pub scheme: NumberingScheme,
    pub cursor: usize,
    pub scroll: usize,
    pub selected: BTreeSet<usize>,
    pub selection_anchor: Option<usize>,
    pub assignments: Vec<SideNumberAssignment>,
    pub current_values: Vec<String>,
    pub labels: Vec<String>,
    pub prefix_input: Option<TextInputState>,
}

impl AutoNumberOverlayState {
    pub fn new(state: &MetadataEditorState, target: NumberingTarget) -> Result<Self, String> {
        let entry_idx = canonical_entry_index(state, target.display_key())
            .ok_or_else(|| format!("metadata editor has no {} field", target.display_key()))?;
        let entry = &state.active_surface().entries[entry_idx];
        let count = entry.per_file_values.len();
        numbering_scheme_capability(state, target, NumberingScheme::SNN)?;
        let assignments = default_side_assignments(state, target);
        let labels = (0..count)
            .map(|slot| {
                state
                    .active_surface()
                    .paths
                    .get(slot)
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                    .or_else(|| state.active_surface().file_labels.get(slot).cloned())
                    .unwrap_or_else(|| format!("Track {:02}", slot + 1))
            })
            .collect();
        Ok(Self {
            target,
            scheme: NumberingScheme::SNN,
            cursor: 0,
            scroll: 0,
            selected: BTreeSet::new(),
            selection_anchor: None,
            assignments,
            current_values: entry
                .per_file_values
                .iter()
                .map(|value| value.as_str().to_string())
                .collect(),
            labels,
            prefix_input: None,
        })
    }

    pub fn len(&self) -> usize {
        self.current_values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.current_values.is_empty()
    }

    pub fn preview_values(&self) -> Result<Vec<String>, String> {
        values_from_assignments(self.scheme, self.len(), &self.assignments)
    }

    pub fn source_summary(&self) -> String {
        let has_manual = self
            .assignments
            .iter()
            .any(|assignment| assignment.source.is_none());
        let mut distinct = self
            .assignments
            .iter()
            .filter_map(|assignment| assignment.source)
            .collect::<BTreeSet<_>>();
        match (has_manual, distinct.len()) {
            (true, 0) => "manual".to_string(),
            (false, 1) => distinct
                .pop_first()
                .map(|source| source.label().to_string())
                .unwrap_or_else(|| "manual".to_string()),
            _ => "mixed sources".to_string(),
        }
    }

    pub fn move_cursor(&mut self, delta: isize, extend_selection: bool) {
        if self.is_empty() {
            self.cursor = 0;
            return;
        }
        let old = self.cursor;
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(step)
        } else {
            self.cursor.saturating_add(step).min(self.len() - 1)
        };
        if extend_selection {
            let anchor = *self.selection_anchor.get_or_insert(old);
            self.selected.clear();
            for slot in anchor.min(self.cursor)..=anchor.max(self.cursor) {
                self.selected.insert(slot);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    pub fn toggle_current_selection(&mut self) {
        if self.is_empty() {
            return;
        }
        if !self.selected.insert(self.cursor) {
            self.selected.remove(&self.cursor);
        }
        self.selection_anchor = Some(self.cursor);
    }

    pub fn selected_or_current(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            (!self.is_empty()).then_some(self.cursor).into_iter().collect()
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn common_selected_prefix(&self) -> Option<&str> {
        let targets = self.selected_or_current();
        let first = targets
            .first()
            .and_then(|slot| self.assignments.get(*slot))?
            .prefix
            .as_str();
        targets
            .iter()
            .all(|slot| {
                self.assignments
                    .get(*slot)
                    .is_some_and(|assignment| assignment.prefix == first)
            })
            .then_some(first)
    }

    pub fn selected_prefix_display(&self) -> String {
        if self.is_empty() {
            "A".to_string()
        } else {
            self.common_selected_prefix()
                .map(str::to_string)
                .unwrap_or_else(|| "<mixed>".to_string())
        }
    }

    pub fn begin_prefix_edit(&mut self) {
        let seed = self
            .common_selected_prefix()
            .map(str::to_string)
            .unwrap_or_default();
        self.prefix_input = Some(TextInputState::new_selected(seed));
    }

    pub fn commit_prefix_edit(&mut self) -> Result<usize, String> {
        let Some(input) = self.prefix_input.as_ref() else {
            return Ok(0);
        };
        let prefix = normalize_prefix(&input.text)?;
        self.prefix_input = None;
        let targets = self.selected_or_current();
        for slot in &targets {
            if let Some(assignment) = self.assignments.get_mut(*slot) {
                assignment.prefix = prefix.clone();
                assignment.derived_sequence = None;
                assignment.source = None;
            }
        }
        Ok(targets.len())
    }

    pub fn ensure_cursor_visible(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll.saturating_add(visible_rows) {
            self.scroll = self.cursor.saturating_sub(visible_rows - 1);
        }
        self.scroll = self.scroll.min(self.len().saturating_sub(visible_rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_preserves_seed_notation_without_inventing_a_side() {
        for (seed, increment, expected) in [
            ("1", 3usize, "4"),
            ("01", 3, "04"),
            ("A1", 3, "A4"),
            ("A01", 3, "A04"),
            ("01/12", 3, "04/12"),
        ] {
            assert_eq!(
                continue_numbering_seed(seed, increment).expect("valid continuation seed"),
                expected,
                "{seed} + {increment}"
            );
        }
        assert_eq!(
            continue_side_numbering_seed("A01", 1, 1).expect("declared next side"),
            "B01"
        );
        assert_eq!(
            continue_side_numbering_seed("Z1", 1, 1).expect("alphabetic rollover"),
            "AA1"
        );
        assert!(continue_numbering_seed("Side A", 1).is_err());
    }
    use crate::tui::app::{
        CueAlbumSyntheticSheet, CueAlbumTrackSource, DiscTechnicalDetails, MetadataCueSource,
        MetadataTechnicalDetails,
    };
    use crate::tui::probe::{RowScope, TagEntry};
    use lofty::tag::ItemKey;
    use std::path::PathBuf;

    fn entry(display_key: &str, item_key: ItemKey, values: &[&str]) -> TagEntry {
        let per_file_values = values.iter().map(|value| (*value).to_string()).collect::<Vec<_>>();
        let is_mixed = per_file_values.windows(2).any(|window| window[0] != window[1]);
        TagEntry {
            row_scope: RowScope::File,
            display_key: display_key.to_string(),
            item_key,
            value: if is_mixed {
                "<multiple values>".to_string()
            } else {
                per_file_values
                    .first()
                    .map(|values| values.as_str().to_string())
                    .unwrap_or_default()
            },
            original: if is_mixed {
                "<multiple values>".to_string()
            } else {
                per_file_values
                    .first()
                    .map(|values| values.as_str().to_string())
                    .unwrap_or_default()
            },
            is_binary: false,
            is_mixed,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: vec![1; values.len()],
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(per_file_values.clone()),
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(per_file_values),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn state(paths: &[&str], entries: Vec<TagEntry>) -> MetadataEditorState {
        MetadataEditorState::for_files(
            paths.iter().map(PathBuf::from).collect(),
            entries,
            paths.iter().map(|path| (*path).to_string()).collect(),
            MetadataTechnicalDetails::default(),
        )
    }

    fn track_entry(display_key: &str, item_key: ItemKey, values: &[&str]) -> TagEntry {
        let mut entry = entry(display_key, item_key, values);
        entry.row_scope = RowScope::Track;
        entry
    }

    fn disc_state(label: &str, track_count: usize) -> MetadataEditorState {
        let paths = vec![PathBuf::from("disc.iso"); track_count];
        let values = vec![""; track_count];
        MetadataEditorState::for_files(
            paths,
            vec![
                entry("TRACKTOTAL", ItemKey::TrackTotal, &values),
                entry("DISCNUMBER", ItemKey::DiscNumber, &values),
                entry("DISCTOTAL", ItemKey::DiscTotal, &values),
            ],
            (1..=track_count).map(|n| format!("Track {n:02}")).collect(),
            MetadataTechnicalDetails::from_disc(DiscTechnicalDetails {
                presentation_label: label.to_string(),
                track_count,
                ..DiscTechnicalDetails::default()
            }),
        )
    }

    fn block_slot(state: &mut MetadataEditorState, slot: usize, reason: &str) {
        let paths = state.active_surface().paths.clone();
        let files = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let mut details = crate::tui::app::MetadataFileDetails::default();
                details.file_facts.path = path;
                if index == slot {
                    details.file_facts.write_eligibility =
                        crate::tui::app::FileWriteEligibility::Blocked {
                            reason: reason.to_string(),
                        };
                }
                details
            })
            .collect();
        state.active_surface_mut().technical_details.files = files;
    }

    #[test]
    fn numeric_schemes_match_the_seventeen_track_contract() {
        let n = format_numbering_values(NumberingScheme::N, 17, None).unwrap();
        let nn = format_numbering_values(NumberingScheme::NN, 17, None).unwrap();
        let n_total = format_numbering_values(NumberingScheme::NOverNN, 17, None).unwrap();
        let nn_total = format_numbering_values(NumberingScheme::NNOverNN, 17, None).unwrap();
        assert_eq!(n.first().map(String::as_str), Some("1"));
        assert_eq!(n.last().map(String::as_str), Some("17"));
        assert_eq!(nn.first().map(String::as_str), Some("01"));
        assert_eq!(nn.last().map(String::as_str), Some("17"));
        assert_eq!(n_total.first().map(String::as_str), Some("1/17"));
        assert_eq!(n_total.last().map(String::as_str), Some("17/17"));
        assert_eq!(nn_total.first().map(String::as_str), Some("01/17"));
        assert_eq!(nn_total.last().map(String::as_str), Some("17/17"));
    }

    #[test]
    fn numeric_schemes_widen_at_three_digits() {
        let values = format_numbering_values(NumberingScheme::NNOverNN, 100, None).unwrap();
        assert_eq!(values[0], "001/100");
        assert_eq!(values[98], "099/100");
        assert_eq!(values[99], "100/100");
    }

    #[test]
    fn side_numbering_resets_per_prefix_without_inventing_groups() {
        let prefixes = vec!["A", "A", "B", "B", "B"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let values = format_numbering_values(NumberingScheme::SNN, 5, Some(&prefixes)).unwrap();
        assert_eq!(values, ["A01", "A02", "B01", "B02", "B03"]);
    }

    #[test]
    fn filename_parser_accepts_side_prefix_and_rejects_embedded_letters() {
        let parsed = side_number_from_filename(&PathBuf::from("A01 - Come Together.flac")).unwrap();
        assert_eq!(parsed.prefix, "A");
        assert_eq!(parsed.sequence, 1);
        assert!(side_number_from_filename(&PathBuf::from("trackA01.flac")).is_none());
        assert!(side_number_from_filename(&PathBuf::from("01 - Come Together.flac")).is_none());
        assert!(parse_side_number("A01/not-a-total").is_none());
        assert_eq!(parse_side_number("A01/17").unwrap().sequence, 1);
        assert_eq!(parse_side_number("SIDE01").unwrap().prefix, "SIDE");
        assert!(side_number_from_filename(&PathBuf::from("SIDE01.flac")).is_none());
    }

    #[test]
    fn numeric_only_menu_exposes_plain_numbering_only_when_it_changes_values() {
        let changing = state(
            &["track.dsf"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["9"])],
        );
        let eligibility = numbering_menu_eligibility(&changing, NumberingTarget::Track)
            .expect("DSF should expose its proven plain-unsigned capability");
        assert_eq!(eligibility.immediate, vec![NumberingScheme::N]);
        assert!(!eligibility.custom);

        let already_numbered = state(
            &["track.dsf"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        let eligibility = numbering_menu_eligibility(&already_numbered, NumberingTarget::Track)
            .expect("idempotent DSF eligibility should remain valid");
        assert!(eligibility.immediate.is_empty());
        assert!(!eligibility.custom);
    }

    #[test]
    fn ape_menu_exposes_plain_padded_and_fraction_schemes_but_not_custom() {
        let state = state(
            &["track.wv", "track-2.wv"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["9", "9"])],
        );
        let eligibility = numbering_menu_eligibility(&state, NumberingTarget::Track)
            .expect("WavPack/APEv2 should expose its proven numeric spelling capabilities");

        assert_eq!(
            eligibility.immediate,
            vec![
                NumberingScheme::N,
                NumberingScheme::NN,
                NumberingScheme::NOverNN,
                NumberingScheme::NNOverNN,
            ]
        );
        assert!(!eligibility.custom);
        assert!(numbering_scheme_capability(
            &state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
        )
        .unwrap_err()
        .contains("requires lexical numbering values"));
    }

    #[test]
    fn side_prefix_validation_rejects_digits_and_path_punctuation() {
        for prefix in ["B2", "../B", "B-C", ""] {
            assert!(format_numbering_values(
                NumberingScheme::SNN,
                1,
                Some(&[prefix.to_string()]),
            )
            .is_err());
        }
    }

    #[test]
    fn resolver_order_is_explicit_and_reorderable() {
        let resolver = SideNumberResolver::default();
        assert_eq!(
            resolver.sources,
            vec![
                SideDerivationSource::EmbeddedCue,
                SideDerivationSource::SidecarCue,
                SideDerivationSource::ExistingTag,
                SideDerivationSource::Filename,
            ]
        );
    }

    #[test]
    fn existing_side_tag_wins_over_conflicting_filename_evidence() {
        let state = state(
            &["A01 - One.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["B07"])],
        );

        let assignments = default_side_assignments(&state, NumberingTarget::Track);

        assert_eq!(
            assignments,
            [SideNumberAssignment {
                prefix: "B".to_string(),
                derived_sequence: Some(7),
                source: Some(SideDerivationSource::ExistingTag),
            }]
        );
    }

    #[test]
    fn filename_derivation_falls_back_only_to_manual_a() {
        let state = state(
            &["A01 - One.flac", "B01 - Two.flac", "No Side.flac"],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1"],
            )],
        );
        let assignments = default_side_assignments(&state, NumberingTarget::Track);
        assert_eq!(
            assignments,
            [
                SideNumberAssignment {
                    prefix: "A".to_string(),
                    derived_sequence: Some(1),
                    source: Some(SideDerivationSource::Filename),
                },
                SideNumberAssignment {
                    prefix: "B".to_string(),
                    derived_sequence: Some(1),
                    source: Some(SideDerivationSource::Filename),
                },
                SideNumberAssignment {
                    prefix: "A".to_string(),
                    derived_sequence: None,
                    source: None,
                },
            ]
        );
    }

    #[test]
    fn custom_overlay_prefills_the_full_side_numbering_fixture() {
        let mut paths = Vec::new();
        for track in 1..=6 {
            paths.push(format!("A{track:02} - Side A {track}.flac"));
        }
        for track in 1..=11 {
            paths.push(format!("B{track:02} - Side B {track}.flac"));
        }
        let path_refs = paths.iter().map(String::as_str).collect::<Vec<_>>();
        let values = vec!["01"; paths.len()];
        let state = state(
            &path_refs,
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &values)],
        );

        let overlay = AutoNumberOverlayState::new(&state, NumberingTarget::Track).unwrap();
        let preview = overlay.preview_values().unwrap();

        assert_eq!(preview[0], "A01");
        assert_eq!(preview[5], "A06");
        assert_eq!(preview[6], "B01");
        assert_eq!(preview[16], "B11");
        assert!(overlay.assignments.iter().all(|assignment| {
            assignment.source == Some(SideDerivationSource::Filename)
                && assignment.derived_sequence.is_some()
        }));
    }

    #[test]
    fn custom_prefix_assignment_renumbers_only_the_selected_side() {
        let state = state(
            &["One.flac", "Two.flac", "Three.flac", "Four.flac"],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1", "1"],
            )],
        );
        let mut overlay =
            AutoNumberOverlayState::new(&state, NumberingTarget::Track).unwrap();
        overlay.selected.extend([2, 3]);
        overlay.prefix_input = Some(TextInputState::new("b".to_string()));

        assert_eq!(overlay.commit_prefix_edit().unwrap(), 2);
        assert_eq!(
            overlay.preview_values().unwrap(),
            ["A01", "A02", "B01", "B02"]
        );
        assert_eq!(overlay.assignments[2].source, None);
        assert_eq!(overlay.assignments[2].derived_sequence, None);
        assert_eq!(overlay.assignments[3].source, None);
        assert_eq!(overlay.assignments[3].derived_sequence, None);
    }

    #[test]
    fn prefix_edit_selects_the_existing_value_for_replacement() {
        let state = state(
            &["One.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        let mut overlay = AutoNumberOverlayState::new(&state, NumberingTarget::Track).unwrap();

        overlay.begin_prefix_edit();

        let input = overlay.prefix_input.as_ref().unwrap();
        assert_eq!(input.text, "A");
        assert!(input.select_all);
        assert_eq!(input.selection_range(), Some(0..1));
    }

    #[test]
    fn mixed_selection_displays_mixed_and_starts_with_an_empty_replacement() {
        let state = state(
            &["A01 - One.flac", "B01 - Two.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1", "1"])],
        );
        let mut overlay = AutoNumberOverlayState::new(&state, NumberingTarget::Track).unwrap();
        overlay.selected.extend([0, 1]);

        assert_eq!(overlay.selected_prefix_display(), "<mixed>");
        overlay.begin_prefix_edit();
        assert_eq!(overlay.prefix_input.as_ref().unwrap().text, "");
    }

    #[test]
    fn headless_side_numbering_rejects_partial_derivation() {
        let mut state = state(
            &["A01 - One.flac", "No Side.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1", "1"])],
        );

        let error = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
            None,
        )
        .unwrap_err();

        assert!(error.contains("resolved 1 of 2 rows"));
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["1", "1"]
        );
    }

    #[test]
    fn side_preview_preserves_shuffled_noncontiguous_source_sequences() {
        let state = state(
            &[
                "B03 - Three.flac",
                "A07 - Seven.flac",
                "B01 - One.flac",
                "A02 - Two.flac",
            ],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1", "1"],
            )],
        );

        let overlay = AutoNumberOverlayState::new(&state, NumberingTarget::Track).unwrap();

        assert_eq!(
            overlay.preview_values().unwrap(),
            ["B03", "A07", "B01", "A02"]
        );
        assert_eq!(
            overlay
                .assignments
                .iter()
                .map(|assignment| assignment.derived_sequence)
                .collect::<Vec<_>>(),
            [Some(3), Some(7), Some(1), Some(2)]
        );
    }

    #[test]
    fn generated_side_sequences_skip_all_reserved_source_values() {
        let assignments = vec![
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: None,
                source: None,
            },
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: Some(1),
                source: Some(SideDerivationSource::Filename),
            },
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: Some(3),
                source: Some(SideDerivationSource::Filename),
            },
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: None,
                source: None,
            },
        ];

        assert_eq!(
            values_from_assignments(NumberingScheme::SNN, 4, &assignments).unwrap(),
            ["A02", "A01", "A03", "A04"]
        );
    }

    #[test]
    fn duplicate_source_sequences_are_rejected_instead_of_renumbered() {
        let assignments = vec![
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: Some(1),
                source: Some(SideDerivationSource::Filename),
            },
            SideNumberAssignment {
                prefix: "A".to_string(),
                derived_sequence: Some(1),
                source: Some(SideDerivationSource::ExistingTag),
            },
        ];

        let error = values_from_assignments(NumberingScheme::SNN, 2, &assignments).unwrap_err();
        assert!(error.contains("duplicate source-derived side sequence A1"));
    }

    #[test]
    fn numeric_carrier_allows_plain_numbers_but_rejects_richer_schemes() {
        let state = state(
            &["one.dsf"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        assert!(numbering_scheme_capability(
            &state,
            NumberingTarget::Track,
            NumberingScheme::N,
        )
        .is_ok());
        for scheme in [
            NumberingScheme::NN,
            NumberingScheme::NOverNN,
            NumberingScheme::NNOverNN,
            NumberingScheme::SN,
            NumberingScheme::SNN,
        ] {
            let error = numbering_scheme_capability(
                &state,
                NumberingTarget::Track,
                scheme,
            )
            .unwrap_err();
            assert!(
                error.contains("requires padded unsigned numbering values")
                    || error.contains("requires lexical numbering values")
                    || error.contains("requires numeric fraction numbering values")
            );
        }
    }

    #[test]
    fn tui_capability_queries_the_authoritative_backend_and_fails_closed() {
        let text_state = state(
            &["track.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        assert!(numbering_scheme_capability(
            &text_state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
        )
        .is_ok());

        let unknown_state = state(
            &["track.unknown"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        let error = numbering_scheme_capability(
            &unknown_state,
            NumberingTarget::Track,
            NumberingScheme::N,
        )
        .unwrap_err();
        assert!(error.contains("cannot determine metadata numbering capabilities"));
    }

    #[test]
    fn mixed_carriers_reject_side_numbering_before_any_mutation() {
        let mut state = state(
            &["one.flac", "two.dsf"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1", "1"])],
        );

        let error = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
            Some("A"),
        )
        .unwrap_err();

        assert!(error.contains(".dsf"));
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["1", "1"]
        );
        assert!(!state.active_surface().dirty);
    }

    #[test]
    fn unsupported_and_unknown_carriers_fail_closed_for_all_unproven_values() {
        let dff = state(
            &["one.dff"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        assert!(numbering_scheme_capability(
            &dff,
            NumberingTarget::Track,
            NumberingScheme::N,
        )
        .unwrap_err()
        .contains("unsupported"));

        let unknown = state(
            &["one.audio"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1"])],
        );
        for scheme in [NumberingScheme::N, NumberingScheme::SNN] {
            assert!(numbering_scheme_capability(&unknown, NumberingTarget::Track, scheme)
                .unwrap_err()
                .contains("cannot determine metadata numbering capabilities"));
        }
    }

    #[test]
    fn shared_mutation_engine_updates_values_and_dirty_state() {
        let mut state = state(
            &["one.flac", "two.flac", "three.flac"],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1"],
            )],
        );
        let report = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::NN,
            None,
        )
        .unwrap();
        assert_eq!(report.changed, 3);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["01", "02", "03"]
        );
        assert!(state.active_surface().dirty);
    }

    #[test]
    fn total_population_uses_the_row_dimension() {
        let mut state = state(
            &["one.flac", "two.flac", "three.flac"],
            vec![entry(
                "TRACKTOTAL",
                ItemKey::TrackTotal,
                &["", "", ""],
            )],
        );
        auto_populate(&mut state, AutoPopulateTarget::TrackTotal).unwrap();
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["3", "3", "3"]
        );
    }

    #[test]
    fn single_image_cue_tracktotal_uses_semantic_track_rows_not_carrier_count() {
        let mut state = state(
            &["album.flac"],
            vec![
                entry("TRACKTOTAL", ItemKey::TrackTotal, &[""]),
                track_entry(
                    "TITLE",
                    ItemKey::TrackTitle,
                    &["One", "Two", "Three", "Four", "Five"],
                ),
            ],
        );

        auto_populate(&mut state, AutoPopulateTarget::TrackTotal).unwrap();

        assert_eq!(state.active_surface().entries[0].per_file_values, ["5"]);
    }

    #[test]
    fn multi_file_cue_tracktotal_uses_track_count_and_audio_persistence() {
        let mut state = state(
            &["one.flac", "two.flac", "three.flac"],
            vec![entry(
                "TRACKTOTAL",
                ItemKey::TrackTotal,
                &["", "", ""],
            )],
        );
        state.active_surface_mut().cue_source =
            Some(MetadataCueSource::Sidecar(PathBuf::from("album.cue")));

        auto_populate(&mut state, AutoPopulateTarget::TrackTotal).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["3", "3", "3"]
        );
    }

    #[test]
    fn aggregate_cue_disc_population_uses_cue_authority_grouping() {
        let paths = ["d1t1.flac", "d1t2.flac", "d2t1.flac", "d2t2.flac"];
        let mut state = state(
            &paths,
            vec![
                entry("DISCNUMBER", ItemKey::DiscNumber, &["", "", "", ""]),
                entry("DISCTOTAL", ItemKey::DiscTotal, &["", "", "", ""]),
            ],
        );
        let cue1 = PathBuf::from("disc1.cue");
        let cue2 = PathBuf::from("disc2.cue");
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: vec![cue1.clone(), cue2.clone()],
            audio_paths: paths.iter().map(PathBuf::from).collect(),
            track_sources: paths
                .iter()
                .enumerate()
                .map(|(index, path)| CueAlbumTrackSource {
                    cue_path: if index < 2 { cue1.clone() } else { cue2.clone() },
                    audio_path: PathBuf::from(path),
                    local_track_index: index % 2,
                    original_track_number: (index % 2 + 1) as u32,
                    file_ref: (*path).to_string(),
                    index00_frames: None,
                    index01_frames: Some(0),
                    index00_sample: None,
                    index01_sample: None,
                    isrc: None,
                    album_user_metadata: Default::default(),
                    user_metadata: Default::default(),
                    tonepoet_metadata_present: false,
                    directives: Vec::new(),
                })
                .collect(),
            album_title: Some("Album".to_string()),
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
            user_metadata: Default::default(),
            program_sample_rate: None,
            program_total_samples: None,
        });
        state.active_surface_mut().cue_source = Some(MetadataCueSource::Sidecar(cue1));

        auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
        auto_populate(&mut state, AutoPopulateTarget::DiscTotal).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["1", "1", "2", "2"]
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_values,
            ["2", "2", "2", "2"]
        );
    }

    #[test]
    fn iso_disc_auto_population_uses_true_track_count_and_one_disc() {
        for label in ["SACD Stereo", "DVD-A Group 1"] {
            let mut state = disc_state(label, 6);

            assert!(auto_populate_has_useful_effect(
                &state,
                AutoPopulateTarget::TrackTotal
            )
            .unwrap(), "{label}");
            auto_populate(&mut state, AutoPopulateTarget::TrackTotal).unwrap();
            auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
            auto_populate(&mut state, AutoPopulateTarget::DiscTotal).unwrap();

            assert_eq!(
                state.active_surface().entries[0].per_file_values,
                vec!["6"; 6],
                "{label}"
            );
            assert_eq!(
                state.active_surface().entries[1].per_file_values,
                vec!["1"; 6],
                "{label}: DISCNUMBER must be constant per physical disc"
            );
            assert_eq!(
                state.active_surface().entries[2].per_file_values,
                vec!["1"; 6],
                "{label}: one source image is one disc"
            );
        }
    }

    #[test]
    fn applying_the_same_numbering_scheme_twice_is_idempotent() {
        let mut state = state(
            &["one.flac", "two.flac", "three.flac"],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1"],
            )],
        );

        let first = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::NN,
            None,
        )
        .unwrap();
        let second = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::NN,
            None,
        )
        .unwrap();

        assert_eq!(first.changed, 3);
        assert_eq!(second.changed, 0);
        assert_eq!(second.unchanged, 3);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["01", "02", "03"]
        );
    }

    #[test]
    fn headless_side_numbering_defaults_to_a_only_when_no_side_is_derivable() {
        let mut state = state(
            &["One.flac", "Two.flac", "Three.flac"],
            vec![entry(
                "TRACKNUMBER",
                ItemKey::TrackNumber,
                &["1", "1", "1"],
            )],
        );

        apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
            None,
        )
        .unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["A01", "A02", "A03"]
        );
    }

    #[test]
    fn disc_population_replaces_a_per_track_series_with_one_logical_disc() {
        let mut state = state(
            &[
                "/album/Disc 9/one.flac",
                "/album/Disk_9/two.flac",
                "/album/CD-9/three.flac",
            ],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["1", "2", "3"])],
        );

        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(report.changed, 2);
        assert_eq!(report.unchanged, 1);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["1", "1", "1"]
        );
        assert_eq!(state.active_surface().entries[0].value, "1");
        assert!(!state.active_surface().entries[0].is_mixed);
    }

    #[test]
    fn explicit_disc_directory_parser_is_narrow_and_conventional() {
        for (name, expected) in [
            ("Disc1", 1),
            ("disc 2", 2),
            ("DISC-03", 3),
            ("Disc_4", 4),
            ("Disk 5", 5),
            ("CD6", 6),
            ("cd 07", 7),
        ] {
            assert_eq!(
                explicit_disc_directory_number(Path::new(name)),
                Some(expected),
                "{name}"
            );
        }

        for name in [
            "Disc",
            "Disc 0",
            "Disc 1 Bonus",
            "Side 1",
            "d01",
            "Discography 2",
        ] {
            assert_eq!(
                explicit_disc_directory_number(Path::new(name)),
                None,
                "{name}"
            );
        }
    }

    #[test]
    fn plain_file_album_disc_directories_drive_disc_number_and_total() {
        let mut state = state(
            &[
                "/album/Disc 1/01.flac",
                "/album/Disc 1/02.flac",
                "/album/Disc 2/01.flac",
                "/album/Disc 2/02.flac",
            ],
            vec![
                entry("DISCNUMBER", ItemKey::DiscNumber, &["", "", "", ""]),
                entry("DISCTOTAL", ItemKey::DiscTotal, &["", "", "", ""]),
            ],
        );

        auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
        auto_populate(&mut state, AutoPopulateTarget::DiscTotal).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["1", "1", "2", "2"]
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_values,
            ["2", "2", "2", "2"]
        );
    }

    #[test]
    fn plain_file_album_compact_cd_directories_drive_disc_number_and_total() {
        let mut state = state(
            &[
                "/album/CD1/01.flac",
                "/album/CD1/02.flac",
                "/album/CD2/01.flac",
                "/album/CD2/02.flac",
            ],
            vec![
                entry("DISCNUMBER", ItemKey::DiscNumber, &["", "", "", ""]),
                entry("DISCTOTAL", ItemKey::DiscTotal, &["", "", "", ""]),
            ],
        );

        auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
        auto_populate(&mut state, AutoPopulateTarget::DiscTotal).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["1", "1", "2", "2"]
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_values,
            ["2", "2", "2", "2"]
        );
    }

    #[test]
    fn plain_files_in_one_explicit_disc_directory_keep_that_disc_number() {
        let mut state = state(
            &["/album/Disc-2/01.flac", "/album/Disc-2/02.flac"],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );

        auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["2", "2"]
        );
    }

    #[test]
    fn flat_plain_folder_still_repairs_stale_per_track_disc_numbers_to_one_disc() {
        let mut state = state(
            &[
                "/album/01.flac",
                "/album/02.flac",
                "/album/03.flac",
            ],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["1", "2", "3"])],
        );

        auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["1", "1", "1"]
        );
    }

    #[test]
    fn disc_population_ignores_folder_names_and_defaults_one_disc() {
        let mut state = state(
            &[
                "/Disc 7/album/Disc 1/one.flac",
                "/Disk_8/album/CD-2/two.flac",
            ],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );

        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(report.changed, 2);
        assert_eq!(report.unchanged, 0);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["1", "1"]
        );
    }

    #[test]
    fn disc_total_comes_from_source_structure_not_stale_disc_tags() {
        let mut state = state(
            &["one.flac", "two.flac", "three.flac", "four.flac"],
            vec![
                entry("DISCNUMBER", ItemKey::DiscNumber, &["1", "1", "2", "0"]),
                entry("DISCTOTAL", ItemKey::DiscTotal, &["", "", "", ""]),
            ],
        );

        auto_populate(&mut state, AutoPopulateTarget::DiscTotal).unwrap();

        assert_eq!(
            state.active_surface().entries[1].per_file_values,
            vec!["1", "1", "1", "1"]
        );
    }

    #[test]
    fn disc_population_without_explicit_evidence_uses_one_constant_disc() {
        let mut state = state(
            &["/album/one.flac", "/album/two.flac"],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );
        state.active_surface_mut().deleted.push(0);
        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
        assert_eq!(report.changed, 2);
        assert_eq!(report.unchanged, 0);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["1", "1"]
        );
        assert!(
            state.active_surface().deleted.is_empty(),
            "successful auto-population must restore a deleted standard field"
        );
    }

    #[test]
    fn auto_populate_eligibility_requires_a_writable_effect() {
        let no_evidence = state(
            &["one.flac", "two.flac"],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );
        assert!(!auto_populate_has_useful_effect(
            &no_evidence,
            AutoPopulateTarget::DiscNumber,
        )
        .unwrap());

        let eligible = state(
            &["one.flac", "two.flac"],
            vec![entry("TRACKTOTAL", ItemKey::TrackTotal, &["", ""])],
        );
        assert!(auto_populate_has_useful_effect(
            &eligible,
            AutoPopulateTarget::TrackTotal,
        )
        .unwrap());
    }

    #[test]
    fn mixed_writable_and_read_only_slots_intersect_only_writable_carriers() {
        let mut state = state(
            &["one.flac", "two.dsf"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["1", "1"])],
        );
        block_slot(&mut state, 1, "read-only fixture");

        assert!(numbering_scheme_capability(
            &state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
        )
        .is_ok());
        let report = apply_numbering(
            &mut state,
            NumberingTarget::Track,
            NumberingScheme::SNN,
            Some("A"),
        )
        .unwrap();
        assert_eq!(report.changed, 1);
        assert_eq!(report.blocked, 1);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["A01", "1"]
        );
    }

    #[test]
    fn no_writable_slots_reject_direct_numbering_and_population() {
        let mut numbering = state(
            &["one.flac"],
            vec![entry("TRACKNUMBER", ItemKey::TrackNumber, &["9"])],
        );
        block_slot(&mut numbering, 0, "read-only fixture");
        assert!(apply_numbering(
            &mut numbering,
            NumberingTarget::Track,
            NumberingScheme::N,
            None,
        )
        .unwrap_err()
        .contains("no writable files"));
        assert_eq!(numbering.active_surface().entries[0].per_file_values, ["9"]);

        let mut total = state(
            &["one.flac"],
            vec![entry("TRACKTOTAL", ItemKey::TrackTotal, &[""])],
        );
        block_slot(&mut total, 0, "read-only fixture");
        assert!(auto_populate(&mut total, AutoPopulateTarget::TrackTotal)
            .unwrap_err()
            .contains("no writable files"));
        assert_eq!(total.active_surface().entries[0].per_file_values, [""]);
    }

    #[test]
    fn auto_populate_fails_closed_for_unknown_carriers_before_mutation() {
        let mut state = state(
            &["one.unknown"],
            vec![entry("TRACKTOTAL", ItemKey::TrackTotal, &[""])],
        );
        let error = auto_populate(&mut state, AutoPopulateTarget::TrackTotal).unwrap_err();
        assert!(error.contains("cannot determine metadata numbering capabilities"));
        assert_eq!(state.active_surface().entries[0].per_file_values, [""]);
        assert!(!state.active_surface().dirty);
    }
}

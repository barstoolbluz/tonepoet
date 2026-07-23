//! Metadata-editor numbering and count population.
//!
//! This module deliberately owns both the pure numbering rules and the
//! metadata-editor mutation boundary.  Context menus, command mode, and the
//! custom preview overlay all dispatch through the same functions so their
//! behavior cannot drift.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
        let width = decimal_width(count);
        return Ok((1..=count)
            .map(|n| match scheme {
                NumberingScheme::N => n.to_string(),
                NumberingScheme::NN => format!("{n:0width$}"),
                NumberingScheme::NOverNN => format!("{n}/{count:0width$}"),
                NumberingScheme::NNOverNN => {
                    format!("{n:0width$}/{count:0width$}")
                }
                NumberingScheme::SN | NumberingScheme::SNN => unreachable!(),
            })
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
        .map(|(prefix, sequence)| match scheme {
            NumberingScheme::SN => format!("{prefix}{sequence}"),
            NumberingScheme::SNN => {
                let width = decimal_width(*maximum_by_prefix.get(prefix.as_str()).unwrap_or(&1));
                format!("{prefix}{sequence:0width$}")
            }
            _ => unreachable!(),
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
        NumberingScheme::NN | NumberingScheme::SN | NumberingScheme::SNN => {
            MetadataNumberingRepresentation::Lexical
        }
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
    backend: crate::metadata_persistence::MetadataPersistenceBackend,
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
            .filter(|carrier| {
                !carrier
                    .backend
                    .numbering_capabilities()
                    .supports(representation)
            })
            .map(|carrier| format!("{} ({})", carrier.label, carrier.backend.label()))
            .collect::<BTreeSet<_>>();
        let requirement = match representation {
            crate::metadata_persistence::MetadataNumberingRepresentation::PlainUnsigned => {
                "plain unsigned numbering values"
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

    let writable_slots = (0..surface.paths.len())
        .filter(|slot| super::keybindings::metadata_editor_slot_is_writable(state, *slot))
        .collect::<Vec<_>>();
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
        let capability =
            crate::metadata_persistence::metadata_numbering_capability_for_path(path)?;
        capabilities = Some(match capabilities {
            Some(current) => current.intersection(capability.capabilities),
            None => capability.capabilities,
        });
        carriers.push(NumberingCarrier {
            label: carrier_label(path),
            backend: capability.backend,
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
    let writable: Vec<bool> = if file_scoped {
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
            if *current == replacement {
                unchanged += 1;
            } else {
                *current = replacement;
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
    let current = state.active_surface().entries[entry_idx]
        .per_file_values
        .clone();
    let (values, restore_deleted) = match target {
        AutoPopulateTarget::TrackTotal => (vec![dim.to_string(); dim], true),
        AutoPopulateTarget::DiscNumber => {
            // Existing tags are the only implemented source in the ordered
            // policy for disc numbers. Cue sources remain explicit seams; path
            // ancestry is intentionally not treated as metadata evidence.
            let originals = &state.active_surface().entries[entry_idx].per_file_originals;
            let mut derived_writable = false;
            let values = (0..dim)
                .map(|slot| {
                    let derived = originals
                        .get(slot)
                        .and_then(|value| parse_positive_component(value));
                    if derived.is_some()
                        && writable_slots.binary_search(&slot).is_ok()
                    {
                        derived_writable = true;
                    }
                    derived
                        .map(|number| number.to_string())
                        .unwrap_or_else(|| current.get(slot).cloned().unwrap_or_default())
                })
                .collect::<Vec<_>>();
            (values, derived_writable)
        }
        AutoPopulateTarget::DiscTotal => {
            let distinct = canonical_entry_index(state, "DISCNUMBER")
                .map(|disc_idx| {
                    state.active_surface().entries[disc_idx]
                        .per_file_values
                        .iter()
                        .filter_map(|value| parse_positive_component(value))
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            if distinct.is_empty() {
                (current, false)
            } else {
                (vec![distinct.len().to_string(); dim], true)
            }
        }
    };
    Ok(AutoPopulatePlan {
        entry_idx,
        writable_slots,
        values,
        restore_deleted,
    })
}

/// Return whether Auto populate has at least one writable, persistence-safe
/// effect. The plan is the same one consumed by execution, so menu state cannot
/// drift from mutation semantics.
pub fn auto_populate_has_useful_effect(
    state: &MetadataEditorState,
    target: AutoPopulateTarget,
) -> Result<bool, String> {
    plan_auto_populate(state, target).map(|plan| plan.has_useful_effect(state))
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
            current_values: entry.per_file_values.clone(),
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
    use crate::tui::app::MetadataTechnicalDetails;
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
                per_file_values.first().cloned().unwrap_or_default()
            },
            original: if is_mixed {
                "<multiple values>".to_string()
            } else {
                per_file_values.first().cloned().unwrap_or_default()
            },
            is_binary: false,
            is_mixed,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: vec![1; values.len()],
            per_file_originals: per_file_values.clone(),
            per_file_values,
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
                error.contains("requires lexical numbering values")
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
    fn disc_population_uses_only_existing_tag_evidence() {
        let mut disc_entry = entry("DISCNUMBER", ItemKey::DiscNumber, &["1", "2", "3"]);
        disc_entry.per_file_values = vec![String::new(), String::new(), String::new()];
        disc_entry.value.clear();
        disc_entry.is_mixed = false;
        let mut state = state(
            &[
                "/album/Disc 9/one.flac",
                "/album/Disk_9/two.flac",
                "/album/CD-9/three.flac",
            ],
            vec![disc_entry],
        );

        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(report.changed, 3);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn disc_population_ignores_folder_names_at_every_ancestor() {
        let mut state = state(
            &[
                "/Disc 7/album/Disc 1/one.flac",
                "/Disk_8/album/CD-2/two.flac",
            ],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );

        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();

        assert_eq!(report.changed, 0);
        assert_eq!(report.unchanged, 2);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["", ""]
        );
    }

    #[test]
    fn disc_total_counts_distinct_positive_disc_numbers() {
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
            vec!["2", "2", "2", "2"]
        );
    }

    #[test]
    fn disc_population_without_explicit_evidence_is_a_noop() {
        let mut state = state(
            &["/album/one.flac", "/album/two.flac"],
            vec![entry("DISCNUMBER", ItemKey::DiscNumber, &["", ""])],
        );
        state.active_surface_mut().deleted.push(0);
        let report = auto_populate(&mut state, AutoPopulateTarget::DiscNumber).unwrap();
        assert_eq!(report.changed, 0);
        assert_eq!(report.unchanged, 2);
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            vec!["", ""]
        );
        assert_eq!(
            state.active_surface().deleted,
            vec![0],
            "no-evidence auto-population must be a true no-op"
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

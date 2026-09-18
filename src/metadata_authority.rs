//! Shared aggregate-metadata authority selection.
//!
//! Callers establish semantic viability for the content scope they are
//! resolving. This module applies the user's normalized configured priority to
//! those viability facts. Keeping the ordering decision here prevents editor,
//! queue, and conversion paths from inventing private sidecar/embedded/file
//! precedence rules.

use crate::config::AggregateMetadataTarget;

/// Viability facts consumed by aggregate metadata-authority resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AggregateMetadataAvailability {
    pub individual_files: bool,
    pub sidecar_cue: bool,
    pub embedded_cue: bool,
}

/// Resolve the first configured target accepted by a caller-supplied viability
/// probe. Omitted/duplicate configured entries inherit the normalization rules
/// in `config::normalized_aggregate_metadata_target_priority`.
pub fn resolve_aggregate_metadata_target_by(
    priority: &[AggregateMetadataTarget],
    mut is_viable: impl FnMut(AggregateMetadataTarget) -> bool,
) -> Option<AggregateMetadataTarget> {
    crate::config::normalized_aggregate_metadata_target_priority(priority)
        .into_iter()
        .find(|target| is_viable(*target))
}

/// Resolve the first viable representation in normalized configured order.
pub fn resolve_aggregate_metadata_target(
    priority: &[AggregateMetadataTarget],
    availability: AggregateMetadataAvailability,
) -> Option<AggregateMetadataTarget> {
    resolve_aggregate_metadata_target_by(priority, |target| match target {
        AggregateMetadataTarget::IndividualFiles => availability.individual_files,
        AggregateMetadataTarget::SidecarCue => availability.sidecar_cue,
        AggregateMetadataTarget::EmbeddedCue => availability.embedded_cue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AggregateMetadataTarget::{EmbeddedCue, IndividualFiles, SidecarCue};

    #[test]
    fn configured_priority_orders_only_viable_representations() {
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
            resolve_aggregate_metadata_target(&[], AggregateMetadataAvailability::default()),
            None,
        );
    }

    #[test]
    fn every_presence_mask_obeys_each_complete_priority_permutation() {
        let priorities = [
            [IndividualFiles, SidecarCue, EmbeddedCue],
            [IndividualFiles, EmbeddedCue, SidecarCue],
            [SidecarCue, IndividualFiles, EmbeddedCue],
            [SidecarCue, EmbeddedCue, IndividualFiles],
            [EmbeddedCue, IndividualFiles, SidecarCue],
            [EmbeddedCue, SidecarCue, IndividualFiles],
        ];

        for priority in priorities {
            for mask in 0u8..8 {
                let availability = AggregateMetadataAvailability {
                    individual_files: mask & 0b001 != 0,
                    sidecar_cue: mask & 0b010 != 0,
                    embedded_cue: mask & 0b100 != 0,
                };
                let expected = priority.iter().copied().find(|target| match target {
                    IndividualFiles => availability.individual_files,
                    SidecarCue => availability.sidecar_cue,
                    EmbeddedCue => availability.embedded_cue,
                });
                assert_eq!(
                    resolve_aggregate_metadata_target(&priority, availability),
                    expected,
                    "priority={priority:?} availability={availability:?}",
                );
            }
        }
    }

    #[test]
    fn sidecar_and_embedded_are_peers_after_individual_files_become_nonviable() {
        let image_scope = AggregateMetadataAvailability {
            individual_files: false,
            sidecar_cue: true,
            embedded_cue: true,
        };
        assert_eq!(
            resolve_aggregate_metadata_target(
                &[IndividualFiles, SidecarCue, EmbeddedCue],
                image_scope,
            ),
            Some(SidecarCue),
        );
        assert_eq!(
            resolve_aggregate_metadata_target(
                &[IndividualFiles, EmbeddedCue, SidecarCue],
                image_scope,
            ),
            Some(EmbeddedCue),
        );
    }
}

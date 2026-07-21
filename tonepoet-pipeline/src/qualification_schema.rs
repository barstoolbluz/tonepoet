//! Strongly typed machine-evidence schemas shared by qualification producers
//! and runtime release-certification consumers.
//!
//! Policy manifests remain append-only JSON. These report-only schemas keep
//! construction and validation on one Rust representation so a future policy
//! change cannot silently update one side while leaving the other stale.

use crate::{
    REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
    REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
    REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
    REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX,
    REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
/// One real-tool observation in the contiguous streamed-WAV capacity scan.
pub struct ReferenceStreamedWavBoundaryObservationV2 {
    /// Mono sample frames declared by the sparse W64 source.
    pub sample_frames: u64,
    /// Float64 audio bytes represented by `sample_frames`.
    pub audio_payload_bytes: u64,
    /// RIFF size field emitted by the pinned unseekable WAV writer.
    pub observed_riff_size_field: u32,
    /// Data-chunk size field emitted by the pinned unseekable WAV writer.
    pub observed_data_size_field: u32,
    /// Structurally correct RIFF size for the complete carrier.
    pub structural_riff_size: u64,
    /// Whether `structural_riff_size` fits the 32-bit RIFF field.
    pub structural_riff_size_representable: bool,
    /// Whether both observed size fields exactly describe the complete carrier.
    pub header_fields_exact: bool,
    /// Planner outcome, either `accepted` or `rejected`.
    pub planner_admission: String,
    /// Stable planner error code for a rejected observation.
    pub planner_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
/// Frozen observation at the first payload whose data field wraps modulo 2^32.
pub struct ReferenceStreamedWavDataWrapWitnessV2 {
    /// Mono sample frames declared by the sparse W64 source.
    pub sample_frames: u64,
    /// Float64 audio bytes represented by `sample_frames`.
    pub audio_payload_bytes: u64,
    /// RIFF size field emitted by the pinned writer.
    pub observed_riff_size_field: u32,
    /// Wrapped data-chunk size field emitted by the pinned writer.
    pub observed_data_size_field: u32,
    /// Mathematically expected payload size modulo 2^32.
    pub expected_modulo_data_size_field: u32,
    /// Must remain false: the wrapped fields are not streaming sentinels.
    pub wrapped_header_is_sentinel: bool,
    /// Must remain false: header capture does not prove complete consumption.
    pub consumer_completeness_claim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
/// Complete policy-v12 real-tool evidence for the streamed Float64 WAV limit.
pub struct ReferenceStreamedWavCapacityEvidenceV2 {
    /// Qualification outcome; canonical evidence requires `passed`.
    pub status: String,
    /// Versioned evidence-contract identifier.
    pub contract: String,
    /// Sparse source container used to declare large logical sample counts.
    pub sparse_source_container: String,
    /// Qualification fixture sample rate.
    pub sample_rate_hz: u32,
    /// Qualification fixture channel count.
    pub channels: u16,
    /// Streamed sample encoding.
    pub sample_encoding: String,
    /// Bytes per interleaved sample value.
    pub bytes_per_sample: u64,
    /// Largest value representable in the RIFF size field.
    pub riff_size_field_max: u64,
    /// Bytes added to audio payload when calculating the RIFF size field.
    pub riff_size_overhead_bytes: u64,
    /// Policy-v12 maximum audio payload before frame alignment.
    pub max_audio_payload_bytes: u64,
    /// Conservative output-frame reserve used by planner admission.
    pub duration_guard_frames: u64,
    /// Complete streamed WAV header length observed before audio bytes.
    pub stream_header_bytes: u64,
    /// Largest frame-aligned carrier admitted by policy v12.
    pub accepted_edge: ReferenceStreamedWavBoundaryObservationV2,
    /// Immediately following frame-aligned carrier rejected by policy v12.
    pub first_policy_rejected_edge: ReferenceStreamedWavBoundaryObservationV2,
    /// Contiguous frame-aligned observations through the data-wrap witness.
    pub transition_scan: Vec<ReferenceStreamedWavBoundaryObservationV2>,
    /// Frame offset of the first observed decrease in the RIFF size field.
    pub first_observed_riff_wrap_offset_frames: u64,
    /// Frozen 4 GiB + one Float64 sample data-wrap observation.
    pub data_wrap_witness: ReferenceStreamedWavDataWrapWitnessV2,
    /// Stable planner error code required for every rejected observation.
    pub error_code: String,
}

impl ReferenceStreamedWavCapacityEvidenceV2 {
    /// Versioned evidence-contract identifier.
    pub const CONTRACT: &'static str = "tonepoet-reference-streamed-wav-capacity/v2";
    /// Stable planner error code for a carrier above the policy-v12 limit.
    pub const ERROR_CODE: &'static str = "DSD-REF-P0-025";
    /// Sample rate used by the sparse real-tool fixture.
    pub const SAMPLE_RATE_HZ: u32 = 48_000;
    /// Channel count used by the sparse real-tool fixture.
    pub const CHANNELS: u16 = 1;
    /// Complete bytes preceding the streamed WAV audio payload.
    pub const STREAM_HEADER_BYTES: u64 = REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES + 8;
    /// First payload whose data size wraps after one complete 2^32-byte cycle.
    pub const DATA_WRAP_PAYLOAD_BYTES: u64 = (1_u64 << 32) + REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;

    /// Return the largest whole-frame payload admitted by policy v12.
    pub fn largest_frame_aligned_admitted_payload() -> u64 {
        REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
            / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            * REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
    }

    /// Return the number of contiguous frame observations required by v2.
    pub fn expected_transition_count() -> u64 {
        (Self::DATA_WRAP_PAYLOAD_BYTES - Self::largest_frame_aligned_admitted_payload())
            / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            + 1
    }

    /// Validate the complete v2 F6 report contract against compiled policy-v12
    /// constants and the relationships between every contiguous edge probe.
    /// Exact defective writer fields remain report data and are immutably bound
    /// by the promoted report hash; this method validates their topology rather
    /// than guessing an unqualified writer-overflow formula.
    pub fn is_canonical_v12(&self) -> bool {
        let accepted_payload = Self::largest_frame_aligned_admitted_payload();
        let observation_is_canonical =
            |value: &ReferenceStreamedWavBoundaryObservationV2, index: u64| {
                let payload = accepted_payload.checked_add(
                    index.checked_mul(REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE)?,
                )?;
                let structural_riff_size =
                    payload.checked_add(REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES)?;
                let structural_representable =
                    structural_riff_size <= REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX;
                let exact = structural_representable
                    && value.observed_riff_size_field
                        == u32::try_from(structural_riff_size).ok()?
                    && value.observed_data_size_field == u32::try_from(payload).ok()?;
                Some(
                    value.sample_frames == payload / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
                        && value.audio_payload_bytes == payload
                        && value.structural_riff_size == structural_riff_size
                        && value.structural_riff_size_representable == structural_representable
                        && value.header_fields_exact == exact
                        && if index == 0 {
                            value.planner_admission == "accepted"
                                && value.planner_error_code.is_none()
                                && exact
                        } else {
                            value.planner_admission == "rejected"
                                && value.planner_error_code.as_deref() == Some(Self::ERROR_CODE)
                        },
                )
            };

        let scan_is_canonical = u64::try_from(self.transition_scan.len()).ok()
            == Some(Self::expected_transition_count())
            && self
                .transition_scan
                .iter()
                .enumerate()
                .all(|(index, value)| {
                    u64::try_from(index)
                        .ok()
                        .and_then(|index| observation_is_canonical(value, index))
                        == Some(true)
                });
        let observed_wrap_offset = self
            .transition_scan
            .windows(2)
            .position(|pair| {
                pair[1].observed_riff_size_field < pair[0].observed_riff_size_field
            })
            .and_then(|index| u64::try_from(index + 1).ok());
        let accepted_matches = observation_is_canonical(&self.accepted_edge, 0) == Some(true)
            && self
                .transition_scan
                .first()
                .is_some_and(|value| value == &self.accepted_edge);
        let first_rejected_matches =
            observation_is_canonical(&self.first_policy_rejected_edge, 1) == Some(true)
                && !self
                    .first_policy_rejected_edge
                    .structural_riff_size_representable
                && !self.first_policy_rejected_edge.header_fields_exact
                && self
                    .transition_scan
                    .get(1)
                    .is_some_and(|value| value == &self.first_policy_rejected_edge);
        let data_wrap = self.transition_scan.last();

        self.status == "passed"
            && self.contract == Self::CONTRACT
            && self.sparse_source_container == "w64"
            && self.sample_rate_hz == Self::SAMPLE_RATE_HZ
            && self.channels == Self::CHANNELS
            && self.sample_encoding == "pcm_f64le"
            && self.bytes_per_sample == REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            && self.riff_size_field_max == REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX
            && self.riff_size_overhead_bytes == REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES
            && self.max_audio_payload_bytes == REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
            && self.duration_guard_frames == REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES
            && self.stream_header_bytes == Self::STREAM_HEADER_BYTES
            && scan_is_canonical
            && accepted_matches
            && first_rejected_matches
            && observed_wrap_offset == Some(self.first_observed_riff_wrap_offset_frames)
            && self.first_observed_riff_wrap_offset_frames >= 1
            && data_wrap.is_some_and(|value| {
                value.sample_frames == self.data_wrap_witness.sample_frames
                    && value.audio_payload_bytes == self.data_wrap_witness.audio_payload_bytes
                    && value.observed_riff_size_field
                        == self.data_wrap_witness.observed_riff_size_field
                    && value.observed_data_size_field
                        == self.data_wrap_witness.observed_data_size_field
            })
            && self.data_wrap_witness.sample_frames
                == Self::DATA_WRAP_PAYLOAD_BYTES / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            && self.data_wrap_witness.audio_payload_bytes == Self::DATA_WRAP_PAYLOAD_BYTES
            && self.data_wrap_witness.observed_riff_size_field == 58
            && self.data_wrap_witness.observed_data_size_field == 8
            && self.data_wrap_witness.expected_modulo_data_size_field == 8
            && !self.data_wrap_witness.wrapped_header_is_sentinel
            && !self.data_wrap_witness.consumer_completeness_claim
            && self.error_code == Self::ERROR_CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_canonical_evidence() -> ReferenceStreamedWavCapacityEvidenceV2 {
        let accepted_payload =
            ReferenceStreamedWavCapacityEvidenceV2::largest_frame_aligned_admitted_payload();
        let mut transition_scan = Vec::new();
        for index in 0..ReferenceStreamedWavCapacityEvidenceV2::expected_transition_count() {
            let payload = accepted_payload + index * REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
            let structural_riff_size =
                payload + REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES;
            let admitted = index == 0;
            let (observed_riff_size_field, observed_data_size_field) = if admitted {
                (
                    u32::try_from(structural_riff_size).expect("accepted RIFF size fits u32"),
                    u32::try_from(payload).expect("accepted data size fits u32"),
                )
            } else if payload == ReferenceStreamedWavCapacityEvidenceV2::DATA_WRAP_PAYLOAD_BYTES {
                (58, 8)
            } else {
                (
                    u32::try_from(index).expect("synthetic index fits u32"),
                    u32::try_from(payload & u64::from(u32::MAX))
                        .expect("synthetic modulo data size fits u32"),
                )
            };
            transition_scan.push(ReferenceStreamedWavBoundaryObservationV2 {
                sample_frames: payload / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
                audio_payload_bytes: payload,
                observed_riff_size_field,
                observed_data_size_field,
                structural_riff_size,
                structural_riff_size_representable: admitted,
                header_fields_exact: admitted,
                planner_admission: if admitted { "accepted" } else { "rejected" }.to_string(),
                planner_error_code: (!admitted).then(|| {
                    ReferenceStreamedWavCapacityEvidenceV2::ERROR_CODE.to_string()
                }),
            });
        }
        let accepted_edge = transition_scan[0].clone();
        let first_policy_rejected_edge = transition_scan[1].clone();
        let data_wrap_witness = {
            let data_wrap = transition_scan.last().expect("data-wrap observation");
            ReferenceStreamedWavDataWrapWitnessV2 {
                sample_frames: data_wrap.sample_frames,
                audio_payload_bytes: data_wrap.audio_payload_bytes,
                observed_riff_size_field: data_wrap.observed_riff_size_field,
                observed_data_size_field: data_wrap.observed_data_size_field,
                expected_modulo_data_size_field: 8,
                wrapped_header_is_sentinel: false,
                consumer_completeness_claim: false,
            }
        };
        ReferenceStreamedWavCapacityEvidenceV2 {
            status: "passed".to_string(),
            contract: ReferenceStreamedWavCapacityEvidenceV2::CONTRACT.to_string(),
            sparse_source_container: "w64".to_string(),
            sample_rate_hz: ReferenceStreamedWavCapacityEvidenceV2::SAMPLE_RATE_HZ,
            channels: ReferenceStreamedWavCapacityEvidenceV2::CHANNELS,
            sample_encoding: "pcm_f64le".to_string(),
            bytes_per_sample: REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
            riff_size_field_max: REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX,
            riff_size_overhead_bytes: REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
            max_audio_payload_bytes: REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
            duration_guard_frames: REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
            stream_header_bytes: ReferenceStreamedWavCapacityEvidenceV2::STREAM_HEADER_BYTES,
            accepted_edge,
            first_policy_rejected_edge,
            transition_scan,
            first_observed_riff_wrap_offset_frames: 1,
            data_wrap_witness,
            error_code: ReferenceStreamedWavCapacityEvidenceV2::ERROR_CODE.to_string(),
        }
    }

    #[test]
    fn v2_boundary_constants_are_exact_and_frame_aligned() {
        assert_eq!(ReferenceStreamedWavCapacityEvidenceV2::STREAM_HEADER_BYTES, 66);
        assert_eq!(
            ReferenceStreamedWavCapacityEvidenceV2::largest_frame_aligned_admitted_payload(),
            4_294_967_232,
        );
        assert_eq!(ReferenceStreamedWavCapacityEvidenceV2::expected_transition_count(), 10);
        assert_eq!(
            ReferenceStreamedWavCapacityEvidenceV2::DATA_WRAP_PAYLOAD_BYTES,
            4_294_967_304,
        );
    }

    #[test]
    fn v2_schema_accepts_only_a_contiguous_canonical_boundary() {
        let canonical = synthetic_canonical_evidence();
        assert!(canonical.is_canonical_v12());

        let mut discontinuous = canonical.clone();
        discontinuous.transition_scan[4].audio_payload_bytes +=
            REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
        assert!(!discontinuous.is_canonical_v12());

        let mut altered_edge = canonical.clone();
        altered_edge.accepted_edge.observed_data_size_field -= 1;
        assert!(!altered_edge.is_canonical_v12());

        let mut false_completeness = canonical;
        false_completeness
            .data_wrap_witness
            .consumer_completeness_claim = true;
        assert!(!false_completeness.is_canonical_v12());
    }

    #[test]
    fn v2_schema_rejects_unknown_root_and_nested_fields() {
        let canonical = synthetic_canonical_evidence();
        let mut root = serde_json::to_value(&canonical).expect("serialize canonical evidence");
        root.as_object_mut()
            .expect("capacity evidence is an object")
            .insert("future_unbound_claim".to_string(), serde_json::Value::Bool(true));
        assert!(
            serde_json::from_value::<ReferenceStreamedWavCapacityEvidenceV2>(root).is_err()
        );

        let mut nested = serde_json::to_value(&canonical).expect("serialize canonical evidence");
        nested["accepted_edge"]
            .as_object_mut()
            .expect("accepted edge is an object")
            .insert("future_unbound_claim".to_string(), serde_json::Value::Bool(true));
        assert!(
            serde_json::from_value::<ReferenceStreamedWavCapacityEvidenceV2>(nested).is_err()
        );
    }
}

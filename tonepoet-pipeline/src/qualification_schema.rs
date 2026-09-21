//! Strongly typed machine-evidence schemas shared by qualification producers
//! and runtime release-certification consumers.
//!
//! Policy manifests remain append-only JSON. These report-only schemas keep
//! construction and validation on one Rust representation so a future policy
//! change cannot silently update one side while leaving the other stale.

use crate::{
    REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
    REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
    REFERENCE_STREAMED_WAV_HEADER_BYTES,
    REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
    REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX,
    REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
};

// Historical policy-v12 constants are frozen independently of the active policy.
// V12 misidentified the 58-byte streamed header as 66 bytes and therefore used
// a 58-byte RIFF-size overhead instead of the measured 50-byte contribution.
const V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES: u64 = 58;
const V12_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES: u64 =
    REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX - V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES;
const V12_STREAMED_WAV_STREAM_HEADER_BYTES: u64 = 66;

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
    pub const STREAM_HEADER_BYTES: u64 = V12_STREAMED_WAV_STREAM_HEADER_BYTES;
    /// First payload whose data size wraps after one complete 2^32-byte cycle.
    pub const DATA_WRAP_PAYLOAD_BYTES: u64 = (1_u64 << 32) + REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;

    /// Return the largest whole-frame payload admitted by policy v12.
    pub fn largest_frame_aligned_admitted_payload() -> u64 {
        V12_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
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
                    payload.checked_add(V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES)?;
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
            && self.riff_size_overhead_bytes == V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES
            && self.max_audio_payload_bytes == V12_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
/// Complete policy-v13 real-tool evidence for the streamed Float64 WAV limit.
pub struct ReferenceStreamedWavCapacityEvidenceV3 {
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
    /// Policy-v13 maximum audio payload before frame alignment.
    pub max_audio_payload_bytes: u64,
    /// Conservative output-frame reserve used by planner admission.
    pub duration_guard_frames: u64,
    /// Complete streamed WAV header length observed before audio bytes.
    pub stream_header_bytes: u64,
    /// Largest frame-aligned carrier admitted by policy v13.
    pub accepted_edge: ReferenceStreamedWavBoundaryObservationV2,
    /// Immediately following frame-aligned carrier rejected by policy v13.
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

impl ReferenceStreamedWavCapacityEvidenceV3 {
    /// Versioned evidence-contract identifier.
    pub const CONTRACT: &'static str = "tonepoet-reference-streamed-wav-capacity/v3";
    /// Stable planner error code for a carrier above the policy-v13 limit.
    pub const ERROR_CODE: &'static str = "DSD-REF-P0-025";
    /// Sample rate used by the sparse real-tool fixture.
    pub const SAMPLE_RATE_HZ: u32 = 48_000;
    /// Channel count used by the sparse real-tool fixture.
    pub const CHANNELS: u16 = 1;
    /// Complete bytes preceding the streamed WAV audio payload.
    pub const STREAM_HEADER_BYTES: u64 = REFERENCE_STREAMED_WAV_HEADER_BYTES;
    /// First payload whose data size wraps after one complete 2^32-byte cycle.
    pub const DATA_WRAP_PAYLOAD_BYTES: u64 = (1_u64 << 32) + REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;

    /// Return the largest whole-frame payload admitted by policy v13.
    pub fn largest_frame_aligned_admitted_payload() -> u64 {
        REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
            / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            * REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
    }

    /// Return the number of contiguous frame observations required by v3.
    pub fn expected_transition_count() -> u64 {
        (Self::DATA_WRAP_PAYLOAD_BYTES - Self::largest_frame_aligned_admitted_payload())
            / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
            + 1
    }

    /// Validate the complete v3 F7 report contract against compiled policy-v13
    /// constants and the relationships between every contiguous edge probe.
    /// Exact defective writer fields remain report data and are immutably bound
    /// by the promoted report hash; this method validates their topology rather
    /// than guessing an unqualified writer-overflow formula.
    pub fn is_canonical_v13(&self) -> bool {
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


/// Active common-model Reference qualification schema.
pub const REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION: u32 = 17;
/// Source-controlled unpromoted common-model candidate manifest.
pub const REFERENCE_COMMON_CANDIDATE_MANIFEST_PATH: &str =
    "qualification/dsd_reference_common_v17_candidate.json";
/// Source-controlled qualification report. The checked-in report remains `not_run`
/// until the exact candidate closure completes every release gate.
pub const REFERENCE_COMMON_QUALIFICATION_REPORT_PATH: &str =
    "qualification/dsd_reference_common_v17_report.json";
/// Source-controlled release certification. The checked-in certification remains
/// `not_run` until it is generated from the matching completed report.
pub const REFERENCE_COMMON_RELEASE_CERTIFICATION_PATH: &str =
    "qualification/dsd_reference_common_v17_certification.json";
/// Stable identity of the consolidated Reference execution model.
pub const REFERENCE_COMMON_EXECUTION_MODEL: &str = "tonepoet-reference-common-model/v1";
/// Exact accepted Phase-4 predecessor archive digest.
pub const REFERENCE_ACCEPTED_PHASE4_SHA256: &str = "2f34c5e74af049f132b9bfd0eb27752af7039de29973f9a2a6805c93748e019c";
/// Exact authoritative design handoff digest.
pub const REFERENCE_DESIGN_HANDOFF_SHA256: &str = "cfda6bd32495e21124f73198a4d52a8ff304adb28cd16b16d3c389e6b567bbd5";
/// Digest of the inherited v16 evidence retained as historical context.
pub const REFERENCE_INHERITED_V16_EVIDENCE_SHA256: &str = "cbea231eb727598ac547dc7346c5ab9a0f6182aeaacdc3e205ec916517ae2b53";
/// Certified in-process peak-observer identity used by Reference gain and acceptance.
pub const REFERENCE_CERTIFIED_OBSERVER_ID: &str = "tonepoet-true-peak:fast066v2_reference/certified_peak_meter/v1";
/// Named certified finite reconstruction target.
pub const REFERENCE_CERTIFIED_RECONSTRUCTION: &str = "hq1024_v1";
/// Certified finite-target endpoint policy.
pub const REFERENCE_CERTIFIED_EDGE_POLICY: &str = "repeat_endpoints";
/// Search tier admitted for qualified Reference observation.
pub const REFERENCE_CERTIFIED_SCAN_TIER: &str = "reference";
/// Certificate component that has ceiling authority.
pub const REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT: &str = "finite_interval_upper";
/// Stable common typed planner identity.
pub const REFERENCE_COMMON_PLANNER_ID: &str = "tonepoet-pipeline:semantic_plan/reference_common/v1";
/// Stable common executor identity.
pub const REFERENCE_COMMON_EXECUTOR_ID: &str = "tonepoet:reference_common_executor/v1";
/// Complete-reader authority for protected Float64 Wave64.
pub const REFERENCE_R64_READER_ID: &str = "tonepoet:reference_r64_exact_w64_complete_reader/v1";
/// Complete-reader authority for terminal QPCM.
pub const REFERENCE_QPCM_READER_ID: &str = "tonepoet:reference_qpcm_exact_complete_reader/v1";
/// Package-sample identity authority retained from the sealed Reference policy.
pub const REFERENCE_PACKAGE_IDENTITY_ID: &str = "tonepoet:reference_package_identity/v16";
/// Phase-4 native metadata/ReplayGain mutation route used by Reference where admitted.
pub const REFERENCE_METADATA_MUTATION_ID: &str = "tonepoet:reference_metadata_mutation/phase4_native/v1";
/// Post-mutation decoded-sample identity authority.
pub const REFERENCE_POST_METADATA_IDENTITY_ID: &str = "tonepoet:reference_post_metadata_sample_identity/v16";
/// Reference terminal-realization authority using the common linear error calculus.
pub const REFERENCE_TERMINAL_ID: &str = "tonepoet:reference_terminal_realization/hq1024_linear_error/v1";
/// Candidate declaration: concrete numerical source digest is supplied by the build closure.
pub const REFERENCE_NUMERICAL_SOURCE_BINDING: &str =
    "build-bound:TONEPOET_TRUE_PEAK_SOURCE_SHA256/v1";
/// Candidate declaration: concrete common Reference source digest is supplied by the build closure.
pub const REFERENCE_COMMON_SOURCE_BINDING: &str =
    "build-bound:TONEPOET_REFERENCE_COMMON_SOURCE_SHA256/v1";
/// Candidate declaration: the concrete target dispatch is measured at runtime.
pub const REFERENCE_SIMD_BINDING: &str =
    "runtime-bound:reference_runtime_dispatch_digest/same-graph-avx-qualified/v1";
/// Candidate declaration: the exact compiler/build identity is supplied by the root build script.
pub const REFERENCE_COMPILER_BUILD_BINDING: &str =
    "build-bound:TONEPOET_COMPILER_BUILD_CLOSURE/v1";

/// Certified observer closure recorded by candidate qualification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCertifiedObserverClosureV1 {
    /// Concrete implementation identity.
    pub implementation_identity: String,
    /// Named certified reconstruction target.
    pub reconstruction: String,
    /// Edge policy used by the finite reconstruction.
    pub edge_policy: String,
    /// Search tier.
    pub scan_tier: String,
    /// Conservative certificate endpoint used as authority.
    pub authority_endpoint: String,
    /// Whether the complete-reader contract is mandatory.
    pub complete_reader_required: bool,
    /// Whether the unproved constant-prefix shortcut is admitted.
    pub constant_prefix_optimization_admitted: bool,
}

impl ReferenceCertifiedObserverClosureV1 {
    /// Active Reference certified-observer closure.
    #[must_use]
    pub fn current() -> Self {
        Self {
            implementation_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            complete_reader_required: true,
            constant_prefix_optimization_admitted: false,
        }
    }

    /// True only for the exact active finite-target contract.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self == &Self::current()
    }
}

/// Evidence-bearing implementation closure that must match between candidate
/// characterization and shipped production Reference execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCommonPrimitiveClosureV1 {
    /// Policy identity bound into the qualification closure.
    pub policy_identity: String,
    /// Semantic-plan identity bound into the qualification closure.
    pub semantic_plan_identity: String,
    /// Numerical implementation identity bound into the qualification closure.
    pub numerical_implementation: String,
    /// Common implementation source bound into the qualification closure.
    pub common_implementation_source: String,
    /// Certified reconstruction authority bound into the qualification closure.
    pub certified_reconstruction: String,
    /// Edge-handling policy bound into the qualification closure.
    pub edge_policy: String,
    /// Certified scan tier bound into the qualification closure.
    pub scan_tier: String,
    /// Reader/decode implementation bound into the qualification closure.
    pub reader_decode_implementation: String,
    /// Tool identity and version closure bound into the qualification.
    pub tool_identity_version_closure: String,
    /// Reconstruction profile bound into the qualification closure.
    pub reconstruction_profile: String,
    /// Terminal implementation identity bound into the qualification closure.
    pub terminal_implementation: String,
    /// Dither and quantization policy bound into the qualification closure.
    pub dither_quantization_policy: String,
    /// Metadata-writer route bound into the qualification closure.
    pub metadata_writer_route: String,
    /// Post-mutation checks bound into the qualification closure.
    pub post_mutation_checks: String,
    /// SIMD implementation identity bound into the qualification closure.
    pub simd_implementation: String,
    /// Compiler/build closure bound into the qualification.
    pub compiler_build_closure: String,
}

impl ReferenceCommonPrimitiveClosureV1 {
    /// Canonical declarative closure for the Phase-5 candidate. Concrete build,
    /// source, tool, ABI, and runtime-dispatch identities are added to the
    /// characterized runtime closure fingerprint by the shipping executable.
    #[must_use]
    pub fn current_declaration() -> Self {
        Self {
            policy_identity: "sox_ng_14_8_0_1_v17+common-v17-candidate".to_string(),
            semantic_plan_identity: REFERENCE_COMMON_PLANNER_ID.to_string(),
            numerical_implementation: REFERENCE_NUMERICAL_SOURCE_BINDING.to_string(),
            common_implementation_source: REFERENCE_COMMON_SOURCE_BINDING.to_string(),
            certified_reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            reader_decode_implementation: format!(
                "{REFERENCE_R64_READER_ID}+{REFERENCE_QPCM_READER_ID}"
            ),
            tool_identity_version_closure:
                "sox-ng-14.8.0.1+ffmpeg-qualified-v16+phase4-native-metadata".to_string(),
            reconstruction_profile: "sealed-reference-profile-matrix-v16".to_string(),
            terminal_implementation: REFERENCE_TERMINAL_ID.to_string(),
            dither_quantization_policy: "sealed-reference-terminal-depth-policy-v16".to_string(),
            metadata_writer_route: REFERENCE_METADATA_MUTATION_ID.to_string(),
            post_mutation_checks: REFERENCE_POST_METADATA_IDENTITY_ID.to_string(),
            simd_implementation: REFERENCE_SIMD_BINDING.to_string(),
            compiler_build_closure: REFERENCE_COMPILER_BUILD_BINDING.to_string(),
        }
    }
}

/// Candidate characterization counts and exact closure identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCandidateCharacterizationV1 {
    /// Number of positive qualification cases.
    pub positive_case_count: u64,
    /// Number of expected-negative qualification cases.
    pub expected_negative_case_count: u64,
    /// SHA-256 digest of the qualified runtime closure.
    pub runtime_closure_fingerprint_sha256: String,
}

/// Source-controlled candidate manifest. Candidate status is descriptive only;
/// it cannot discharge production admission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCommonQualificationV1 {
    /// Qualification schema version.
    pub schema_version: u32,
    /// Execution-model identity for this qualification artifact.
    pub execution_model: String,
    /// Recorded qualification or gate status.
    pub status: String,
    /// SHA-256 digest of the accepted Phase 4 artifact.
    pub accepted_phase4_sha256: String,
    /// SHA-256 digest of the design handoff artifact.
    pub design_handoff_sha256: String,
    /// SHA-256 digest of the inherited v16 evidence.
    pub inherited_v16_evidence_sha256: String,
    /// Certified observer identity recorded by the qualification.
    pub observer: ReferenceCertifiedObserverClosureV1,
    /// Qualified primitive closure.
    pub closure: ReferenceCommonPrimitiveClosureV1,
}

impl ReferenceCommonQualificationV1 {
    /// Validate immutable source-level identity of a candidate manifest.
    pub fn validate_candidate_manifest(&self) -> Result<(), String> {
        if self.schema_version != REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION
            || self.execution_model != REFERENCE_COMMON_EXECUTION_MODEL
            || self.status != "candidate"
            || self.accepted_phase4_sha256 != REFERENCE_ACCEPTED_PHASE4_SHA256
            || self.design_handoff_sha256 != REFERENCE_DESIGN_HANDOFF_SHA256
            || self.inherited_v16_evidence_sha256 != REFERENCE_INHERITED_V16_EVIDENCE_SHA256
            || !self.observer.is_current()
            || self.closure != ReferenceCommonPrimitiveClosureV1::current_declaration()
        {
            return Err("Reference common-model candidate manifest does not match the compiled qualification contract".to_string());
        }
        Ok(())
    }
}

/// One required release gate result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceReleaseGateResultV1 {
    /// Release gate name.
    pub gate: String,
    /// Recorded qualification or gate status.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// SHA-256 digest of evidence attached to this gate.
    pub evidence_sha256: Option<String>,
}

/// Completed or source-controlled not-run qualification report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceQualificationReportV1 {
    /// Qualification schema version.
    pub schema_version: u32,
    /// Execution-model identity for this qualification artifact.
    pub execution_model: String,
    /// Recorded qualification or gate status.
    pub status: String,
    /// SHA-256 digest of the qualified candidate manifest.
    pub candidate_manifest_sha256: String,
    /// SHA-256 digest of the qualified runtime closure.
    pub runtime_closure_fingerprint_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// SHA-256 digest of the metadata-mutation closure.
    pub metadata_mutation_closure_fingerprint_sha256: Option<String>,
    /// Number of positive qualification cases.
    pub positive_case_count: u64,
    /// Number of expected-negative qualification cases.
    pub expected_negative_case_count: u64,
    /// Recorded release-gate results.
    pub gates: Vec<ReferenceReleaseGateResultV1>,
}

/// Release certification binding a completed report to the exact candidate and
/// shipped runtime closure.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceReleaseCertificationV1 {
    /// Qualification schema version.
    pub schema_version: u32,
    /// Execution-model identity for this qualification artifact.
    pub execution_model: String,
    /// Recorded qualification or gate status.
    pub status: String,
    /// Recorded release-certification outcome.
    pub outcome: String,
    /// SHA-256 digest of the qualified candidate manifest.
    pub candidate_manifest_sha256: String,
    /// SHA-256 digest of the qualification report.
    pub qualification_report_sha256: String,
    /// SHA-256 digest of the qualified runtime closure.
    pub runtime_closure_fingerprint_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// SHA-256 digest of the metadata-mutation closure.
    pub metadata_mutation_closure_fingerprint_sha256: Option<String>,
    /// Number of positive qualification cases.
    pub positive_case_count: u64,
    /// Number of expected-negative qualification cases.
    pub expected_negative_case_count: u64,
    /// Recorded release-gate results.
    pub gates: Vec<ReferenceReleaseGateResultV1>,
}

/// Exact Phase-5 release-gate names. Extra/missing/duplicate gates are rejected.
pub const REFERENCE_REQUIRED_RELEASE_GATES: [&str; 12] = [
    "Q01",
    "Q02",
    "GAIN08",
    "GAIN09",
    "wave64_integrity_60_cell",
    "complete_reader",
    "package_identity",
    "post_metadata_identity",
    "timeout_cancellation_resource",
    "ordinary_workspace_regression",
    "real_tool_candidate_qualification",
    "paired_performance_resource",
];

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Compute the canonical SHA-256 hex digest of exact bytes.
#[must_use]
pub fn reference_sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_completed_gate_set(gates: &[ReferenceReleaseGateResultV1]) -> Result<(), String> {
    if gates.len() != REFERENCE_REQUIRED_RELEASE_GATES.len() {
        return Err("Reference release report has the wrong gate count".to_string());
    }
    let mut seen = std::collections::BTreeSet::new();
    for gate in gates {
        if !REFERENCE_REQUIRED_RELEASE_GATES.contains(&gate.gate.as_str())
            || !seen.insert(gate.gate.as_str())
        {
            return Err(format!("Reference release report has an unknown or duplicate gate {}", gate.gate));
        }
        if gate.status != "passed"
            || gate.evidence_sha256.as_deref().map(is_sha256_hex) != Some(true)
        {
            return Err(format!("Reference release gate {} is not backed by passed SHA-256 evidence", gate.gate));
        }
    }
    Ok(())
}

impl ReferenceQualificationReportV1 {
    /// Fail-only production preflight for source-controlled Phase-5 evidence.
    /// Success means only that the report is structurally complete and bound to
    /// the exact candidate; runtime closure identity is validated after current
    /// tool/build attestation.
    pub fn validate_promotion_preflight(&self, candidate_bytes: &[u8]) -> Result<(), String> {
        if self.schema_version != REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION
            || self.execution_model != REFERENCE_COMMON_EXECUTION_MODEL
            || self.status != "passed"
            || self.candidate_manifest_sha256 != reference_sha256_hex(candidate_bytes)
            || !is_sha256_hex(&self.runtime_closure_fingerprint_sha256)
            || self
                .metadata_mutation_closure_fingerprint_sha256
                .as_deref()
                .map(is_sha256_hex)
                != Some(true)
            || self.positive_case_count == 0
            || self.expected_negative_case_count == 0
        {
            return Err("Reference qualification report is incomplete or does not bind the exact candidate/required closure variants".to_string());
        }
        validate_completed_gate_set(&self.gates)
    }

    /// Validate a completed qualification report against the exact core runtime
    /// closure and, when the real pipeline will run metadata mutation, the
    /// attested metadata-mutation closure. Every completed release records both
    /// variants so one qualification can activate either admitted route.
    pub fn validate_completed(
        &self,
        candidate_bytes: &[u8],
        runtime_closure_fingerprint_sha256: &str,
        metadata_mutation_closure_fingerprint_sha256: Option<&str>,
    ) -> Result<(), String> {
        self.validate_promotion_preflight(candidate_bytes)?;
        if self.runtime_closure_fingerprint_sha256 != runtime_closure_fingerprint_sha256
            || metadata_mutation_closure_fingerprint_sha256.is_some_and(|fingerprint| {
                self.metadata_mutation_closure_fingerprint_sha256.as_deref() != Some(fingerprint)
            })
        {
            return Err("Reference qualification report does not bind the running runtime closure variant".to_string());
        }
        Ok(())
    }
}

impl ReferenceReleaseCertificationV1 {
    /// Fail-only production preflight for source-controlled certification.
    /// This validates completed status, candidate/report binding, both declared
    /// closure variants, counts, and release gates without accepting a running
    /// runtime closure.
    pub fn validate_promotion_preflight(
        &self,
        candidate_bytes: &[u8],
        report_bytes: &[u8],
        report: &ReferenceQualificationReportV1,
    ) -> Result<(), String> {
        if self.schema_version != REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION
            || self.execution_model != REFERENCE_COMMON_EXECUTION_MODEL
            || self.status != "passed"
            || self.outcome != "qualified"
            || self.candidate_manifest_sha256 != reference_sha256_hex(candidate_bytes)
            || self.qualification_report_sha256 != reference_sha256_hex(report_bytes)
            || self.runtime_closure_fingerprint_sha256 != report.runtime_closure_fingerprint_sha256
            || self.metadata_mutation_closure_fingerprint_sha256
                != report.metadata_mutation_closure_fingerprint_sha256
            || self.positive_case_count != report.positive_case_count
            || self.expected_negative_case_count != report.expected_negative_case_count
            || self.positive_case_count == 0
            || self.expected_negative_case_count == 0
            || self.gates != report.gates
        {
            return Err("Reference release certification does not bind the completed matching qualification evidence".to_string());
        }
        report.validate_promotion_preflight(candidate_bytes)?;
        validate_completed_gate_set(&self.gates)
    }

    /// Validate release certification against exact candidate/report bytes and
    /// the closure variant used by the running production implementation.
    pub fn validate_completed(
        &self,
        candidate_bytes: &[u8],
        report_bytes: &[u8],
        report: &ReferenceQualificationReportV1,
        runtime_closure_fingerprint_sha256: &str,
        metadata_mutation_closure_fingerprint_sha256: Option<&str>,
    ) -> Result<(), String> {
        self.validate_promotion_preflight(candidate_bytes, report_bytes, report)?;
        if self.runtime_closure_fingerprint_sha256 != runtime_closure_fingerprint_sha256
            || metadata_mutation_closure_fingerprint_sha256.is_some_and(|fingerprint| {
                self.metadata_mutation_closure_fingerprint_sha256.as_deref() != Some(fingerprint)
            })
        {
            return Err("Reference release certification does not bind the running runtime closure variant".to_string());
        }
        report.validate_completed(
            candidate_bytes,
            runtime_closure_fingerprint_sha256,
            metadata_mutation_closure_fingerprint_sha256,
        )
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
                payload + V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES;
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
            riff_size_overhead_bytes: V12_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
            max_audio_payload_bytes: V12_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
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

    fn synthetic_canonical_evidence_v3() -> ReferenceStreamedWavCapacityEvidenceV3 {
        let accepted_payload =
            ReferenceStreamedWavCapacityEvidenceV3::largest_frame_aligned_admitted_payload();
        let mut transition_scan = Vec::new();
        for index in 0..ReferenceStreamedWavCapacityEvidenceV3::expected_transition_count() {
            let payload = accepted_payload + index * REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
            let structural_riff_size =
                payload + REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES;
            let admitted = index == 0;
            let (observed_riff_size_field, observed_data_size_field) = if admitted {
                (
                    u32::try_from(structural_riff_size).expect("accepted RIFF size fits u32"),
                    u32::try_from(payload).expect("accepted data size fits u32"),
                )
            } else if payload == ReferenceStreamedWavCapacityEvidenceV3::DATA_WRAP_PAYLOAD_BYTES {
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
                    ReferenceStreamedWavCapacityEvidenceV3::ERROR_CODE.to_string()
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
        ReferenceStreamedWavCapacityEvidenceV3 {
            status: "passed".to_string(),
            contract: ReferenceStreamedWavCapacityEvidenceV3::CONTRACT.to_string(),
            sparse_source_container: "w64".to_string(),
            sample_rate_hz: ReferenceStreamedWavCapacityEvidenceV3::SAMPLE_RATE_HZ,
            channels: ReferenceStreamedWavCapacityEvidenceV3::CHANNELS,
            sample_encoding: "pcm_f64le".to_string(),
            bytes_per_sample: REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE,
            riff_size_field_max: REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX,
            riff_size_overhead_bytes: REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
            max_audio_payload_bytes: REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES,
            duration_guard_frames: REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
            stream_header_bytes: ReferenceStreamedWavCapacityEvidenceV3::STREAM_HEADER_BYTES,
            accepted_edge,
            first_policy_rejected_edge,
            transition_scan,
            first_observed_riff_wrap_offset_frames: 1,
            data_wrap_witness,
            error_code: ReferenceStreamedWavCapacityEvidenceV3::ERROR_CODE.to_string(),
        }
    }

    #[test]
    fn v3_boundary_constants_are_exact_and_frame_aligned() {
        assert_eq!(REFERENCE_STREAMED_WAV_HEADER_BYTES, 58);
        assert_eq!(ReferenceStreamedWavCapacityEvidenceV3::STREAM_HEADER_BYTES, 58);
        assert_eq!(REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES, 50);
        assert_eq!(
            REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES,
            REFERENCE_STREAMED_WAV_HEADER_BYTES - 8,
        );
        assert_eq!(REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES, 4_294_967_245);
        assert_eq!(
            ReferenceStreamedWavCapacityEvidenceV3::largest_frame_aligned_admitted_payload(),
            4_294_967_240,
        );
        assert_eq!(ReferenceStreamedWavCapacityEvidenceV3::expected_transition_count(), 9);
        assert_eq!(
            ReferenceStreamedWavCapacityEvidenceV3::DATA_WRAP_PAYLOAD_BYTES,
            4_294_967_304,
        );
    }

    #[test]
    fn v3_schema_accepts_only_the_corrected_contiguous_boundary() {
        let canonical = synthetic_canonical_evidence_v3();
        assert!(canonical.is_canonical_v13());

        let mut stale_header = canonical.clone();
        stale_header.stream_header_bytes = 66;
        assert!(!stale_header.is_canonical_v13());

        let mut stale_overhead = canonical.clone();
        stale_overhead.riff_size_overhead_bytes = 58;
        assert!(!stale_overhead.is_canonical_v13());

        let mut discontinuous = canonical;
        discontinuous.transition_scan[4].audio_payload_bytes +=
            REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
        assert!(!discontinuous.is_canonical_v13());
    }

    #[test]
    fn v3_schema_rejects_unknown_root_and_nested_fields() {
        let canonical = synthetic_canonical_evidence_v3();
        let mut root = serde_json::to_value(&canonical).expect("serialize canonical evidence");
        root.as_object_mut()
            .expect("capacity evidence is an object")
            .insert("future_unbound_claim".to_string(), serde_json::Value::Bool(true));
        assert!(
            serde_json::from_value::<ReferenceStreamedWavCapacityEvidenceV3>(root).is_err()
        );

        let mut nested = serde_json::to_value(&canonical).expect("serialize canonical evidence");
        nested["accepted_edge"]
            .as_object_mut()
            .expect("accepted edge is an object")
            .insert("future_unbound_claim".to_string(), serde_json::Value::Bool(true));
        assert!(
            serde_json::from_value::<ReferenceStreamedWavCapacityEvidenceV3>(nested).is_err()
        );
    }

    fn phase5_candidate() -> ReferenceCommonQualificationV1 {
        ReferenceCommonQualificationV1 {
            schema_version: REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION,
            execution_model: REFERENCE_COMMON_EXECUTION_MODEL.to_string(),
            status: "candidate".to_string(),
            accepted_phase4_sha256: REFERENCE_ACCEPTED_PHASE4_SHA256.to_string(),
            design_handoff_sha256: REFERENCE_DESIGN_HANDOFF_SHA256.to_string(),
            inherited_v16_evidence_sha256: REFERENCE_INHERITED_V16_EVIDENCE_SHA256.to_string(),
            observer: ReferenceCertifiedObserverClosureV1::current(),
            closure: ReferenceCommonPrimitiveClosureV1::current_declaration(),
        }
    }

    fn completed_gate_set() -> Vec<ReferenceReleaseGateResultV1> {
        REFERENCE_REQUIRED_RELEASE_GATES
            .iter()
            .enumerate()
            .map(|(index, gate)| ReferenceReleaseGateResultV1 {
                gate: (*gate).to_string(),
                status: "passed".to_string(),
                evidence_sha256: Some(format!("{index:064x}")),
            })
            .collect()
    }

    #[test]
    fn phase5_candidate_rejects_closure_wording_drift() {
        let canonical = phase5_candidate();
        canonical
            .validate_candidate_manifest()
            .expect("canonical Phase-5 candidate declaration is accepted");

        let mut numerical_drift = canonical.clone();
        numerical_drift.closure.numerical_implementation =
            "tonepoet-true-peak:version-only".to_string();
        assert!(numerical_drift.validate_candidate_manifest().is_err());

        let mut simd_drift = canonical;
        simd_drift.closure.simd_implementation = "simd-enabled".to_string();
        assert!(simd_drift.validate_candidate_manifest().is_err());
    }

    #[test]
    fn phase5_status_edit_cannot_manually_promote_not_run_evidence() {
        let candidate_bytes = b"phase5-candidate";
        let runtime_fingerprint = "a".repeat(64);
        let report = ReferenceQualificationReportV1 {
            schema_version: REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION,
            execution_model: REFERENCE_COMMON_EXECUTION_MODEL.to_string(),
            status: "passed".to_string(),
            candidate_manifest_sha256: String::new(),
            runtime_closure_fingerprint_sha256: String::new(),
            metadata_mutation_closure_fingerprint_sha256: None,
            positive_case_count: 0,
            expected_negative_case_count: 0,
            gates: Vec::new(),
        };
        assert!(report
            .validate_completed(candidate_bytes, &runtime_fingerprint, None)
            .is_err());

        let report_bytes = serde_json::to_vec(&report).expect("serialize tampered report");
        let certification = ReferenceReleaseCertificationV1 {
            schema_version: REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION,
            execution_model: REFERENCE_COMMON_EXECUTION_MODEL.to_string(),
            status: "passed".to_string(),
            outcome: "qualified".to_string(),
            candidate_manifest_sha256: String::new(),
            qualification_report_sha256: String::new(),
            runtime_closure_fingerprint_sha256: String::new(),
            metadata_mutation_closure_fingerprint_sha256: None,
            positive_case_count: 0,
            expected_negative_case_count: 0,
            gates: Vec::new(),
        };
        assert!(certification
            .validate_completed(
                candidate_bytes,
                &report_bytes,
                &report,
                &runtime_fingerprint,
                None,
            )
            .is_err());
    }

    #[test]
    fn phase5_completed_evidence_is_bound_to_candidate_report_and_runtime_closure() {
        let candidate_bytes = b"phase5-candidate";
        let runtime_fingerprint = "b".repeat(64);
        let metadata_fingerprint = "d".repeat(64);
        let gates = completed_gate_set();
        let report = ReferenceQualificationReportV1 {
            schema_version: REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION,
            execution_model: REFERENCE_COMMON_EXECUTION_MODEL.to_string(),
            status: "passed".to_string(),
            candidate_manifest_sha256: reference_sha256_hex(candidate_bytes),
            runtime_closure_fingerprint_sha256: runtime_fingerprint.clone(),
            metadata_mutation_closure_fingerprint_sha256: Some(metadata_fingerprint.clone()),
            positive_case_count: 17,
            expected_negative_case_count: 11,
            gates: gates.clone(),
        };
        report
            .validate_completed(candidate_bytes, &runtime_fingerprint, None)
            .expect("matching completed report validates for metadata-disabled runtime");
        report
            .validate_completed(
                candidate_bytes,
                &runtime_fingerprint,
                Some(&metadata_fingerprint),
            )
            .expect("matching completed report validates for metadata-enabled runtime");
        assert!(report
            .validate_completed(candidate_bytes, &"c".repeat(64), None)
            .is_err());

        let report_bytes = serde_json::to_vec(&report).expect("serialize completed report");
        let certification = ReferenceReleaseCertificationV1 {
            schema_version: REFERENCE_COMMON_QUALIFICATION_SCHEMA_VERSION,
            execution_model: REFERENCE_COMMON_EXECUTION_MODEL.to_string(),
            status: "passed".to_string(),
            outcome: "qualified".to_string(),
            candidate_manifest_sha256: reference_sha256_hex(candidate_bytes),
            qualification_report_sha256: reference_sha256_hex(&report_bytes),
            runtime_closure_fingerprint_sha256: runtime_fingerprint.clone(),
            metadata_mutation_closure_fingerprint_sha256: Some(metadata_fingerprint.clone()),
            positive_case_count: report.positive_case_count,
            expected_negative_case_count: report.expected_negative_case_count,
            gates,
        };
        certification
            .validate_completed(
                candidate_bytes,
                &report_bytes,
                &report,
                &runtime_fingerprint,
                Some(&metadata_fingerprint),
            )
            .expect("matching certification validates");

        let mut report_drift = report.clone();
        report_drift.positive_case_count += 1;
        assert!(certification
            .validate_completed(
                candidate_bytes,
                &report_bytes,
                &report_drift,
                &runtime_fingerprint,
                Some(&metadata_fingerprint),
            )
            .is_err());
    }
}

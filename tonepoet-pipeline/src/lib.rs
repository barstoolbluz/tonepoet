//! Pure command-planning crate for tonepoet audio conversion.
//!
//! The crate owns the unified conversion type system and converts
//! already-probed source facts plus target settings into deterministic command
//! descriptions. It does not spawn processes, probe files, read configuration,
//! or perform filesystem writes.

pub mod dsd_album_gain;
pub mod dsd_reference;
pub mod enums;
pub mod error;
pub mod mapping;
pub mod plan;
pub mod plugins;
pub mod qualification_schema;
pub mod semantic_plan;
pub mod ssrc_binary64;
pub mod settings;
pub mod source;
pub mod tools;
pub mod w64;
pub mod fingerprint;

pub use dsd_album_gain::*;
pub use dsd_reference::*;
pub use enums::*;
pub use error::{PlanningError, Result};
pub use mapping::*;
pub use plan::{
    CommandEnvironmentPolicy, MetadataPlanEffect,
    plan_conversion, plan_conversion_with_registry, plan_topology,
    stage_a_selected_vs_emitted_diagnostics,
    stage_a_selected_vs_emitted_diagnostics_with_registry,
    selects_reference_dsd_to_pcm, ConversionPlan, Finalization,
    InputSource, OutputSink, PlanAction, PlanContext, PlanOperation, PlanParticipantId, PlanRequest,
    PlanScope, PlanScopeId, PlanStep, StageALoweringProvenance,
    StageASelectedVsEmittedRecord, PlannedCommand, PlannedCommandPipeline, TopologyPlan,
};
pub use qualification_schema::*;
pub use semantic_plan::*;
pub use ssrc_binary64::*;
pub use plugins::{
    FfmpegPlugin, FlacPlugin, MetaflacPlugin, SoxPlugin, SsrcPlugin,
};
pub use settings::*;
pub use source::{SourceFrameExtent, SourceInfo, SourceRepresentationKind};
pub use tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolRegistry, ToolSupport};
pub use w64::*;
pub use fingerprint::{
    common_execution_plan_fingerprint_v1, common_semantic_plan_fingerprint_v1, settings_and_effects_fingerprint, settings_fingerprint,
    CommonExecutionPlanFingerprintV1, CommonSemanticPlanFingerprintV1, SettingsFingerprint, DSD_ALBUM_GAIN_FINGERPRINT_FIELD_PATHS,
    PCM_TRUE_PEAK_FINGERPRINT_FIELD_PATHS, SETTINGS_FINGERPRINT_FIELD_COUNT,
    SETTINGS_FINGERPRINT_FIELD_PATHS,
    SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT, SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS,
};

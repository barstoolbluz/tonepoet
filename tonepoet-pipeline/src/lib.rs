//! Pure command-planning crate for tonepoet audio conversion.
//!
//! The crate owns the unified conversion type system and converts
//! already-probed source facts plus target settings into deterministic command
//! descriptions. It does not spawn processes, probe files, read configuration,
//! or perform filesystem writes.

pub mod dsd_reference;
pub mod enums;
pub mod error;
pub mod mapping;
pub mod plan;
pub mod plugins;
pub mod qualification_schema;
pub mod settings;
pub mod source;
pub mod tools;
pub mod fingerprint;

pub use dsd_reference::*;
pub use enums::*;
pub use error::{PlanningError, Result};
pub use mapping::*;
pub use plan::{
    CommandEnvironmentPolicy, MetadataPlanEffect,
    plan_conversion, plan_conversion_with_registry, plan_topology,
    selects_reference_dsd_to_pcm, ConversionPlan, Finalization,
    InputSource, OutputSink, PlanAction, PlanContext, PlanOperation, PlanRequest, PlanStep,
    PlannedCommand, PlannedCommandPipeline, PlannedExecutionStep, TopologyPlan,
};
pub use qualification_schema::*;
pub use plugins::{
    FfmpegPlugin, FlacPlugin, LoudgainPlugin, MetaflacPlugin, SoxPlugin, SsrcPlugin,
};
pub use settings::*;
pub use source::{SourceInfo, SourceRepresentationKind};
pub use tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolRegistry, ToolSupport};
pub use fingerprint::{
    settings_fingerprint, SettingsFingerprint, SETTINGS_FINGERPRINT_FIELD_COUNT,
    SETTINGS_FINGERPRINT_FIELD_PATHS, SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT,
    SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS,
};

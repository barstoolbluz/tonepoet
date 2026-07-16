//! Pure command-planning crate for tonepoet audio conversion.
//!
//! The crate owns the unified conversion type system and converts
//! already-probed source facts plus target settings into deterministic command
//! descriptions. It does not spawn processes, probe files, read configuration,
//! or perform filesystem writes.

pub mod enums;
pub mod error;
pub mod mapping;
pub mod plan;
pub mod plugins;
pub mod settings;
pub mod source;
pub mod tools;
pub mod fingerprint;

pub use enums::*;
pub use error::{PlanningError, Result};
pub use mapping::*;
pub use plan::{
    MetadataPlanEffect,
    plan_conversion, plan_conversion_with_registry, plan_topology, ConversionPlan, Finalization,
    InputSource, OutputSink, PlanAction, PlanContext, PlanOperation, PlanRequest, PlanStep,
    PlannedCommand, TopologyPlan,
};
pub use plugins::{
    FfmpegPlugin, FlacPlugin, LoudgainPlugin, MetaflacPlugin, SoxPlugin, SsrcPlugin,
};
pub use settings::*;
pub use source::{SourceInfo, SourceRepresentationKind};
pub use tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolRegistry, ToolSupport};
pub use fingerprint::{settings_fingerprint, SettingsFingerprint, SETTINGS_FINGERPRINT_FIELD_COUNT, SETTINGS_FINGERPRINT_FIELD_PATHS};

//! Tool plugin trait and deterministic registry.

use crate::enums::PreferredTool;
use crate::error::{PlanningError, Result};
use crate::plan::{MetadataPlanEffect, PlanContext, PlanStep, PlannedCommand};
use std::collections::BTreeSet;
use std::fmt;

/// Binary/tool identity used in planned commands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ToolIdentifier {
    /// FFmpeg binary.
    Ffmpeg,
    /// SoX binary.
    Sox,
    /// SSRC binary.
    Ssrc,
    /// loudgain binary.
    Loudgain,
    /// metaflac binary.
    Metaflac,
    /// FLAC command-line binary for native decode verification.
    Flac,
    /// Caller-defined tool name.
    Custom(String),
}

impl ToolIdentifier {
    /// Program name for process execution.
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Sox => "sox",
            Self::Ssrc => "ssrc",
            Self::Loudgain => "loudgain",
            Self::Metaflac => "metaflac",
            Self::Flac => "flac",
            Self::Custom(name) => name.as_str(),
        }
    }

    /// True when this identifier matches the user's preference.
    #[must_use]
    pub fn matches_preference(&self, preference: &PreferredTool) -> bool {
        match (self, preference) {
            (_, PreferredTool::Auto) => false,
            (Self::Ffmpeg, PreferredTool::Ffmpeg)
            | (Self::Sox, PreferredTool::Sox)
            | (Self::Ssrc, PreferredTool::Ssrc) => true,
            (Self::Custom(name), PreferredTool::Custom(preferred)) => name == preferred,
            _ => false,
        }
    }
}

impl fmt::Display for ToolIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program())
    }
}

/// Capability score returned by a plugin for a logical step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolSupport {
    score: u8,
}

impl ToolSupport {
    /// Unsupported operation.
    pub const UNSUPPORTED: Self = Self { score: 0 };
    /// Supported fallback implementation.
    pub const FALLBACK: Self = Self { score: 25 };
    /// Supported normal implementation.
    pub const SUPPORTED: Self = Self { score: 50 };
    /// Preferred implementation for the operation.
    pub const PREFERRED: Self = Self { score: 75 };
    /// Canonical implementation for the operation.
    pub const CANONICAL: Self = Self { score: 100 };

    /// Construct a custom score in the supported range.
    #[must_use]
    pub const fn new(score: u8) -> Self {
        Self { score }
    }

    /// True if the plugin supports the operation.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.score > 0
    }

    /// Numeric score.
    #[must_use]
    pub const fn score(self) -> u8 {
        self.score
    }
}

/// Metadata policy behavior for a selected plugin and logical step.
///
/// The planner uses this after deterministic plugin selection so metadata
/// preservation decisions reflect the tool that will actually run, not a
/// topology-time guess. Custom plugins can return `WritesRequestedPolicy`
/// when their encoder applies [`crate::settings::MetadataSettings`] itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataDisposition {
    /// The operation does not write tags/artwork according to the requested policy.
    DoesNotWrite,
    /// The operation writes exactly the requested tags/artwork policy.
    WritesRequestedPolicy,
}

impl MetadataDisposition {
    /// True when a later metadata-transfer step would be redundant.
    #[must_use]
    pub const fn writes_requested_policy(self) -> bool {
        matches!(self, Self::WritesRequestedPolicy)
    }
}

/// Pure command-builder plugin.
///
/// Implementations must not read files, inspect the environment, spawn
/// processes, or use randomness. They receive already-probed facts and return
/// an argv vector only.
pub trait ToolPlugin: Send + Sync {
    /// Stable tool identifier.
    fn id(&self) -> ToolIdentifier;

    /// Capability score for a planned logical step.
    fn supports(&self, context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport;

    /// Return the typed planner-owned metadata effect for this logical step.
    ///
    /// This is the authoritative planner signal used for metadata-step pruning
    /// and orchestration satisfaction. Implementations must report only effects
    /// they actually produce for the selected step; callers must not infer these
    /// facts by parsing command-line arguments.
    fn metadata_effect(
        &self,
        _context: &PlanContext<'_>,
        _step: &PlanStep,
    ) -> MetadataPlanEffect {
        MetadataPlanEffect::none()
    }

    /// Report whether this plugin writes the requested metadata/artwork policy for this step.
    ///
    /// Deprecated compatibility hook. New planner/orchestrator code should use
    /// [`ToolPlugin::metadata_effect`] so distinct metadata obligations cannot
    /// collapse into a coarse yes/no disposition.
    fn metadata_disposition(
        &self,
        _context: &PlanContext<'_>,
        _step: &PlanStep,
    ) -> MetadataDisposition {
        MetadataDisposition::DoesNotWrite
    }

    /// Build a command for the given logical step.
    fn build_command(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<PlannedCommand>;
}

/// Registry of deterministic tool plugins.
pub struct ToolRegistry {
    plugins: Vec<Box<dyn ToolPlugin>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_builtin_tools()
    }
}

impl ToolRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Create a registry containing built-in FFmpeg, SoX, SSRC, loudgain, and metaflac plugins.
    #[must_use]
    pub fn with_builtin_tools() -> Self {
        let mut registry = Self::empty();
        registry
            .register(Box::new(crate::plugins::FfmpegPlugin))
            .expect("unique built-in plugin");
        registry
            .register(Box::new(crate::plugins::SoxPlugin))
            .expect("unique built-in plugin");
        registry
            .register(Box::new(crate::plugins::SsrcPlugin))
            .expect("unique built-in plugin");
        registry
            .register(Box::new(crate::plugins::LoudgainPlugin))
            .expect("unique built-in plugin");
        registry
            .register(Box::new(crate::plugins::MetaflacPlugin))
            .expect("unique built-in plugin");
        registry
            .register(Box::new(crate::plugins::FlacPlugin))
            .expect("unique built-in plugin");
        registry
    }

    /// Register a plugin. Duplicate IDs are rejected to keep selection deterministic.
    pub fn register(&mut self, plugin: Box<dyn ToolPlugin>) -> Result<()> {
        let new_id = plugin.id();
        if self.plugins.iter().any(|existing| existing.id() == new_id) {
            return Err(PlanningError::RegistryError {
                reason: format!("duplicate plugin id {new_id}"),
            });
        }
        self.plugins.push(plugin);
        self.plugins.sort_by_key(|plugin| plugin.id());
        Ok(())
    }

    /// Return the registered tool IDs.
    #[must_use]
    pub fn tool_ids(&self) -> BTreeSet<ToolIdentifier> {
        self.plugins.iter().map(|plugin| plugin.id()).collect()
    }

    /// Return the selected plugin id for a step without building the command.
    pub fn selected_tool_id(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> Result<ToolIdentifier> {
        Ok(self.select_plugin(context, step)?.id())
    }

    /// Return the selected plugin's typed metadata effect for a logical step.
    pub fn metadata_effect_for_step(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> Result<MetadataPlanEffect> {
        let plugin = self.select_plugin(context, step)?;
        Ok(plugin.metadata_effect(context, step))
    }

    /// Return the selected plugin's metadata behavior for a logical step.
    pub fn metadata_disposition_for_step(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> Result<MetadataDisposition> {
        let plugin = self.select_plugin(context, step)?;
        Ok(plugin.metadata_disposition(context, step))
    }

    /// Build a command for one logical step using deterministic plugin selection.
    pub fn build_command(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> Result<PlannedCommand> {
        let plugin = self.select_plugin(context, step)?;
        plugin.build_command(context, step)
    }

    fn select_plugin(&self, context: &PlanContext<'_>, step: &PlanStep) -> Result<&dyn ToolPlugin> {
        let preference = &context.request.settings.preferred_tool;

        let mut supported: Vec<(&dyn ToolPlugin, ToolSupport)> = self
            .plugins
            .iter()
            .map(|plugin| (plugin.as_ref(), plugin.supports(context, step)))
            .filter(|(_, support)| support.is_supported())
            .collect();

        if supported.is_empty() {
            return Err(PlanningError::NoPluginForOperation {
                operation: step.operation.label().to_string(),
                preferred_tool: preference.clone(),
            });
        }

        if !matches!(preference, PreferredTool::Auto) {
            let mut preferred: Vec<(&dyn ToolPlugin, ToolSupport)> = supported
                .iter()
                .copied()
                .filter(|(plugin, _)| plugin.id().matches_preference(preference))
                .collect();
            if !preferred.is_empty() {
                preferred.sort_by(
                    |(left_plugin, left_support), (right_plugin, right_support)| {
                        right_support
                            .score()
                            .cmp(&left_support.score())
                            .then_with(|| left_plugin.id().cmp(&right_plugin.id()))
                    },
                );
                return Ok(preferred[0].0);
            }
        }

        supported.sort_by(
            |(left_plugin, left_support), (right_plugin, right_support)| {
                right_support
                    .score()
                    .cmp(&left_support.score())
                    .then_with(|| left_plugin.id().cmp(&right_plugin.id()))
            },
        );
        Ok(supported[0].0)
    }
}

//! Structured execution evidence for conversion reporting.
//!
//! Execution paths record compact, presentation-independent receipts here.
//! `conversion.log` and the portable execution JSON are projections of these
//! records; neither is allowed to rediscover performed audio processing from
//! settings, command-token searches, or endpoint comparisons.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tonepoet_pipeline::{
    plan_topology, plan_typed, stage_a_selected_vs_emitted_diagnostics, DecisionKind, EffectIntent,
    PlanOperation, PlanRequest, PlanningOutcome, RegisteredUnaryEffect,
    ResolvedOperationParameters, TopologyPlan, TypedPlanNode,
};

use super::tool::{CommandRecord, ProcessExit, RetainedPcmScalarPump};

pub const PORTABLE_EXECUTION_SCHEMA: &str = "tonepoet.execution/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum EvidenceValue {
    Text(String),
    Bool(bool),
    Integer(i64),
    Unsigned(u64),
    Decimal(f64),
    SampleRateHz(u32),
    BitDepth(String),
    Db(f64),
    Percent(f64),
}

impl EvidenceValue {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Decimal(value) => trim_decimal(*value),
            Self::SampleRateHz(value) => format!("{value} Hz"),
            Self::BitDepth(value) => value.clone(),
            Self::Db(value) => format!("{} dB", trim_decimal(*value)),
            Self::Percent(value) => format!("{}%", trim_decimal(*value)),
        }
    }
}

fn trim_decimal(value: f64) -> String {
    let mut rendered = format!("{value:.6}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationParameter {
    pub name: String,
    pub value: EvidenceValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl OperationParameter {
    #[must_use]
    pub fn new(name: impl Into<String>, value: EvidenceValue) -> Self {
        Self {
            name: name.into(),
            value,
            unit: None,
        }
    }

    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    #[must_use]
    pub fn render(&self) -> String {
        match self.unit.as_deref() {
            Some(unit) if !unit.is_empty() => format!("{} {}", self.value.render(), unit),
            _ => self.value.render(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthority {
    User,
    Preset,
    AutomaticPolicy,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionBackend {
    External {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Native {
        implementation: String,
    },
    Composite {
        description: String,
    },
    Unknown,
}

impl ExecutionBackend {
    #[must_use]
    pub fn external(tool: impl Into<String>) -> Self {
        Self::External {
            tool: tool.into(),
            version: None,
        }
    }

    #[must_use]
    pub fn native(implementation: impl Into<String>) -> Self {
        Self::Native {
            implementation: implementation.into(),
        }
    }

    #[must_use]
    pub fn human_label(&self) -> String {
        match self {
            Self::External { tool, version } => version
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|version| format!("{tool} {version}"))
                .unwrap_or_else(|| tool.clone()),
            Self::Native { implementation } => implementation.clone(),
            Self::Composite { description } => description.clone(),
            Self::Unknown => "backend not recorded".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Completed,
    Failed,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OperationDomain {
    #[default]
    AudioSignal,
    Artifact,
    Analysis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OperationLineage {
    #[default]
    DeliveredAudio,
    Analysis,
    DiscardedAttempt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceArtifactRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl EvidenceArtifactRef {
    #[must_use]
    pub fn signal(id: u32) -> Self {
        Self {
            id: format!("signal-{id}"),
            path: None,
        }
    }

    #[must_use]
    pub fn path(role: impl Into<String>, path: &Path) -> Self {
        Self {
            id: role.into(),
            path: Some(path.to_path_buf()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    /// Open semantic kind. New operation kinds are intentionally not an enum so
    /// a future operation remains serializable/renderable before a specialized
    /// human renderer exists.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<EvidenceArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<EvidenceArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<OperationParameter>,
    pub backend: ExecutionBackend,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocation_indices: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_ids: Vec<String>,
    #[serde(default)]
    pub domain: OperationDomain,
    pub status: OperationStatus,
    #[serde(default)]
    pub lineage: OperationLineage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl OperationRecord {
    #[must_use]
    pub fn completed(
        id: impl Into<String>,
        kind: impl Into<String>,
        name: impl Into<String>,
        backend: ExecutionBackend,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            parameters: Vec::new(),
            backend,
            invocation_indices: Vec::new(),
            decision_ids: Vec::new(),
            observation_ids: Vec::new(),
            domain: OperationDomain::AudioSignal,
            status: OperationStatus::Completed,
            lineage: OperationLineage::DeliveredAudio,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<EvidenceValue>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub authority: DecisionAuthority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub id: String,
    pub kind: String,
    pub statement: String,
    pub status: VerificationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocation_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeDomain {
    OriginalSourceStorage,
    SelectedSourceExtent,
    WorkingArtifact,
    StagedArtifact,
    DeliveredArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeEvidence {
    pub domain: SizeDomain,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackExecutionEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<ObservationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<VerificationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes: Vec<SizeEvidence>,
}

impl TrackExecutionEvidence {
    pub fn append(&mut self, mut other: Self, invocation_offset: usize) {
        if invocation_offset != 0 {
            for operation in &mut other.operations {
                for index in &mut operation.invocation_indices {
                    *index = index.saturating_add(invocation_offset);
                }
            }
            for verification in &mut other.verifications {
                for index in &mut verification.invocation_indices {
                    *index = index.saturating_add(invocation_offset);
                }
            }
        }
        self.operations.extend(other.operations);
        self.observations.extend(other.observations);
        self.decisions.extend(other.decisions);
        self.verifications.extend(other.verifications);
        self.sizes.extend(other.sizes);
    }

    pub fn mark_discarded_attempt(&mut self) {
        for operation in &mut self.operations {
            if operation.lineage == OperationLineage::DeliveredAudio {
                operation.lineage = OperationLineage::DiscardedAttempt;
            }
        }
    }

    #[must_use]
    pub fn delivered_operations(&self) -> impl Iterator<Item = &OperationRecord> {
        self.operations.iter().filter(|operation| {
            operation.lineage == OperationLineage::DeliveredAudio
                && operation.status == OperationStatus::Completed
        })
    }

    #[must_use]
    pub fn discarded_attempt_operations(&self) -> impl Iterator<Item = &OperationRecord> {
        self.operations.iter().filter(|operation| {
            operation.lineage == OperationLineage::DiscardedAttempt
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableInvocation {
    pub binary: String,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub environment: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ProcessExit>,
    pub elapsed_millis: u64,
}

impl From<&CommandRecord> for PortableInvocation {
    fn from(command: &CommandRecord) -> Self {
        Self {
            binary: command.binary.default_name().to_string(),
            argv: command.sanitized_args.clone(),
            cwd: command.cwd.clone(),
            environment_policy: command.environment_policy,
            environment: command.environment.clone(),
            environment_keys: command.env_keys.clone(),
            exit: command.exit,
            elapsed_millis: duration_millis(command.elapsed),
        }
    }
}

#[must_use]
pub fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[must_use]
pub fn render_operation_generic(operation: &OperationRecord) -> Vec<String> {
    let suffix = match operation.status {
        OperationStatus::Completed => String::new(),
        OperationStatus::Failed => " — failed".to_string(),
        OperationStatus::Incomplete => " — incomplete".to_string(),
    };
    let mut lines = vec![format!(
        "{} — {}{}",
        operation.name,
        operation.backend.human_label(),
        suffix,
    )];
    for parameter in &operation.parameters {
        lines.push(format!("  {}: {}", parameter.name, parameter.render()));
    }
    if let Some(detail) = operation.detail.as_deref().filter(|value| !value.is_empty()) {
        lines.push(format!("  {detail}"));
    }
    lines
}

#[must_use]
pub fn render_operation_with_context(
    evidence: &TrackExecutionEvidence,
    operation: &OperationRecord,
) -> Vec<String> {
    let mut lines = render_operation_generic(operation);
    for decision_id in &operation.decision_ids {
        if let Some(decision) = evidence.decisions.iter().find(|item| &item.id == decision_id) {
            let authority = match decision.authority {
                DecisionAuthority::User => "user",
                DecisionAuthority::Preset => "preset",
                DecisionAuthority::AutomaticPolicy => "automatic policy",
                DecisionAuthority::Unknown => "unknown authority",
            };
            lines.push(format!("  Selected by {authority}: {}", decision.summary));
        }
    }
    for observation_id in &operation.observation_ids {
        if let Some(observation) = evidence
            .observations
            .iter()
            .find(|item| &item.id == observation_id)
        {
            match observation.value.as_ref() {
                Some(value) => lines.push(format!(
                    "  Evidence: {}={} ({})",
                    observation.name,
                    value.render(),
                    observation.source,
                )),
                None => lines.push(format!(
                    "  Evidence: {} ({})",
                    observation.name, observation.source,
                )),
            }
        }
    }
    lines
}

#[must_use]
pub fn render_track_processing(evidence: &TrackExecutionEvidence) -> Vec<String> {
    let mut lines = Vec::new();
    for operation in evidence
        .delivered_operations()
        .filter(|operation| operation.domain == OperationDomain::AudioSignal)
    {
        lines.extend(render_operation_with_context(evidence, operation));
    }
    lines
}

#[must_use]
pub fn render_track_artifact_work(evidence: &TrackExecutionEvidence) -> Vec<String> {
    let mut lines = Vec::new();
    for operation in evidence
        .delivered_operations()
        .filter(|operation| operation.domain == OperationDomain::Artifact)
    {
        lines.extend(render_operation_with_context(evidence, operation));
    }
    lines
}

#[must_use]
pub fn render_discarded_attempt_processing(evidence: &TrackExecutionEvidence) -> Vec<String> {
    let mut lines = Vec::new();
    for operation in evidence.discarded_attempt_operations() {
        lines.extend(render_operation_with_context(evidence, operation));
    }
    lines
}

#[must_use]
pub fn render_track_verifications(evidence: &TrackExecutionEvidence) -> Vec<String> {
    evidence
        .verifications
        .iter()
        .map(|verification| {
            let status = match verification.status {
                VerificationStatus::Passed => "passed",
                VerificationStatus::Failed => "failed",
                VerificationStatus::Incomplete => "incomplete",
            };
            format!("{} — {status}", verification.statement)
        })
        .collect()
}

/// Project the typed planner contract into completed semantic evidence only
/// after the executor has successfully realized the command chain.
///
/// The planner remains the semantic authority; commands are used only to bind
/// selected typed operations to the invocation indices that physically realized
/// them. No command text is parsed to infer an effect or transform.
pub fn completed_plan_evidence(
    request: &PlanRequest,
    invocations: &[CommandRecord],
) -> Result<TrackExecutionEvidence, String> {
    let typed = match plan_typed(request).map_err(|error| error.to_string())? {
        PlanningOutcome::Ready(plan) => plan,
        PlanningOutcome::NeedFacts(facts) => {
            return Err(format!("typed plan still needs facts: {facts:?}"));
        }
        PlanningOutcome::Refused(refusal) => {
            return Err(format!("typed plan refused: {}: {}", refusal.code, refusal.reason));
        }
    };
    let diagnostics = stage_a_selected_vs_emitted_diagnostics(request)
        .map_err(|error| error.to_string())?;

    let mut evidence = TrackExecutionEvidence::default();
    for (node_index, node) in typed.nodes.iter().enumerate() {
        match node {
            TypedPlanNode::Operation {
                operation,
                input_signal,
                output_signal,
                candidates,
                selected_candidate,
                resolved_parameters,
            } => {
                if matches!(operation, PlanOperation::Verify { .. }) {
                    let indices = diagnostics
                        .iter()
                        .filter(|item| item.typed_node_index == node_index)
                        .filter(|item| item.matches_selected_realization())
                        .map(|item| item.emitted_step_index)
                        .filter(|index| *index < invocations.len())
                        .collect();
                    evidence.verifications.push(VerificationRecord {
                        id: format!("plan-node-{node_index}"),
                        kind: operation.label().to_string(),
                        statement: "Planner-requested terminal verification completed".to_string(),
                        status: VerificationStatus::Passed,
                        invocation_indices: indices,
                    });
                    continue;
                }

                let selected = candidates.get(*selected_candidate);
                let backend = selected
                    .and_then(|candidate| candidate.tool.as_ref())
                    .map(|tool| ExecutionBackend::external(tool.program()))
                    .unwrap_or(ExecutionBackend::Unknown);
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    operation.label(),
                    operation_human_name(operation),
                    backend,
                );
                if let Some(signal) = input_signal {
                    record.inputs.push(EvidenceArtifactRef::signal(signal.0));
                }
                if let Some(signal) = output_signal {
                    record.outputs.push(EvidenceArtifactRef::signal(signal.0));
                }
                record.parameters = operation_parameters(operation, resolved_parameters);
                record.domain = operation_domain(operation);
                let node_diagnostics = diagnostics
                    .iter()
                    .filter(|item| item.typed_node_index == node_index)
                    .collect::<Vec<_>>();
                let emitted_indices = node_diagnostics
                    .iter()
                    .copied()
                    .filter(|item| item.matches_selected_realization())
                    .map(|item| item.emitted_step_index)
                    .collect::<Vec<_>>();
                record.invocation_indices = emitted_indices
                    .iter()
                    .copied()
                    .filter(|index| *index < invocations.len())
                    .collect();
                if let Some(candidate) = selected {
                    apply_selected_terminal_realization(&mut record, candidate);
                    if let Some(tonepoet_pipeline::SelectedTerminalRealization::Pcm(realization)) =
                        candidate.contract.terminal_realization.as_ref()
                    {
                        // Stage-A is an observer, not execution authority. Compound terminals
                        // deliberately compare a typed preterminal owner with a package command,
                        // so a selected-vs-emitted mismatch can be expected for the package
                        // anchor (notably WavPack hybrid). Use the diagnostic's emitted index
                        // only as a coordinate anchor; structural topology validation below
                        // establishes the preterminal/package pair that physically ran.
                        let stage_a_anchor_indices = node_diagnostics
                            .iter()
                            .map(|item| item.emitted_step_index)
                            .collect::<Vec<_>>();
                        if let Some(indices) = compound_terminal_invocation_indices(
                            request,
                            realization,
                            &stage_a_anchor_indices,
                        )? {
                            record.invocation_indices = indices;
                        }
                    }
                }
                evidence.operations.push(record);
            }
            TypedPlanNode::ApplyEffect {
                input,
                output,
                instance,
                lowering,
                ..
            } => {
                let mut record = completed_effect_operation(
                    format!("effect-{}", instance.id.0),
                    instance,
                    lowering.tool.program(),
                );
                record.inputs.push(EvidenceArtifactRef::signal(input.0));
                record.outputs.push(EvidenceArtifactRef::signal(output.0));
                evidence.operations.push(record);
            }
            TypedPlanNode::ApplyGain {
                input,
                output,
                policy,
                decision,
            } => {
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    "apply_gain",
                    "Waveform gain",
                    ExecutionBackend::Unknown,
                );
                record.inputs.push(EvidenceArtifactRef::signal(input.0));
                record.outputs.push(EvidenceArtifactRef::signal(output.0));
                record.parameters.push(OperationParameter::new(
                    "Policy",
                    EvidenceValue::Text(format!("{policy:?}")),
                ));
                if let Some(decision) = decision {
                    record.decision_ids.push(format!("decision-{}", decision.0));
                }
                evidence.operations.push(record);
            }
            TypedPlanNode::ExportDsdLevel {
                input,
                output,
                level,
                gain_db,
            } => {
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    "dsd_export_level",
                    "DSD reconstruction level export",
                    ExecutionBackend::native("TonePoet DSD realization"),
                );
                record.inputs.push(EvidenceArtifactRef::signal(input.0));
                record.outputs.push(EvidenceArtifactRef::signal(output.0));
                record.parameters.push(OperationParameter::new(
                    "Level",
                    EvidenceValue::Text(format!("{level:?}")),
                ));
                record.parameters.push(OperationParameter::new(
                    "Gain",
                    EvidenceValue::Text(format!("{gain_db:?}")),
                ));
                evidence.operations.push(record);
            }
            TypedPlanNode::ReferenceTerminalRealization {
                input,
                output,
                decision,
                sample_contract,
            } => {
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    "reference_terminal_realization",
                    "Qualified Reference terminal realization",
                    ExecutionBackend::native("TonePoet qualified Reference path"),
                );
                record.inputs.push(EvidenceArtifactRef::signal(input.0));
                record.outputs.push(EvidenceArtifactRef::signal(output.0));
                record.decision_ids.push(format!("decision-{}", decision.0));
                record.parameters.push(OperationParameter::new(
                    "Sample contract",
                    EvidenceValue::Text(format!("{sample_contract:?}")),
                ));
                evidence.operations.push(record);
            }
            TypedPlanNode::PackageOutput { input, output, .. } => {
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    "package_output",
                    "Package output",
                    ExecutionBackend::Unknown,
                );
                record.inputs.push(EvidenceArtifactRef::signal(input.0));
                record.outputs.push(EvidenceArtifactRef::signal(output.0));
                record.domain = OperationDomain::Artifact;
                evidence.operations.push(record);
            }
            TypedPlanNode::MutateArtifact { .. } => {
                let mut record = OperationRecord::completed(
                    format!("plan-node-{node_index}"),
                    "metadata_mutation",
                    "Metadata mutation",
                    ExecutionBackend::Unknown,
                );
                record.domain = OperationDomain::Artifact;
                evidence.operations.push(record);
            }
            TypedPlanNode::VerifyArtifact { .. } => {
                evidence.verifications.push(VerificationRecord {
                    id: format!("plan-node-{node_index}"),
                    kind: "verify_artifact".to_string(),
                    statement: "Artifact verification completed".to_string(),
                    status: VerificationStatus::Passed,
                    invocation_indices: Vec::new(),
                });
            }
            TypedPlanNode::Decide(decision) => {
                evidence.decisions.push(DecisionRecord {
                    id: format!("decision-{}", decision.id.0),
                    kind: decision_kind_name(&decision.kind).to_string(),
                    summary: decision_summary(&decision.kind),
                    authority: DecisionAuthority::AutomaticPolicy,
                    observation_ids: decision
                        .observations
                        .iter()
                        .map(|observation| format!("observation-{}", observation.observation.0))
                        .collect(),
                });
            }
            // A typed observation node is a request for a measurement, not proof
            // that the measurement actually ran. Actual measurements are added
            // by their execution/verification owners instead of being invented
            // from the plan.
            TypedPlanNode::Observe(_)
            | TypedPlanNode::DecodeSourceForProcessing { .. }
            | TypedPlanNode::DecodeArtifactForObservation { .. } => {}
        }
    }

    let decision_observations: Vec<(String, Vec<String>)> = evidence
        .decisions
        .iter()
        .map(|decision| (decision.id.clone(), decision.observation_ids.clone()))
        .collect();
    for operation in &mut evidence.operations {
        for decision_id in operation.decision_ids.clone() {
            if let Some((_, observations)) = decision_observations
                .iter()
                .find(|(id, _)| id == &decision_id)
            {
                for observation_id in observations {
                    if !operation.observation_ids.contains(observation_id) {
                        operation.observation_ids.push(observation_id.clone());
                    }
                }
            }
        }
    }

    Ok(evidence)
}


/// Project the subset of a typed effect-aware plan that this pre-terminal
/// registered-effect realizer has just completed. The realizer supplies the
/// physical invocation indices as execution receipts; this projector never
/// rediscovers linkage from command text.
pub fn completed_registered_effects_from_plan(
    request: &PlanRequest,
    effects: &[EffectIntent],
    effect_invocation_indices: &[usize],
    resampler_invocation_index: Option<usize>,
) -> Result<TrackExecutionEvidence, String> {
    let typed = match tonepoet_pipeline::plan_typed_with_effects(request, effects)
        .map_err(|error| error.to_string())?
    {
        PlanningOutcome::Ready(plan) => plan,
        PlanningOutcome::NeedFacts(facts) => {
            return Err(format!("effect-aware typed plan still needs facts: {facts:?}"));
        }
        PlanningOutcome::Refused(refusal) => {
            return Err(format!("effect-aware typed plan refused: {}: {}", refusal.code, refusal.reason));
        }
    };
    let mut evidence = TrackExecutionEvidence::default();
    let mut effect_bindings = effect_invocation_indices.iter().copied();
    let mut projected_effects = 0_usize;
    let mut projected_resamplers = 0_usize;
    for (node_index, node) in typed.nodes.iter().enumerate() {
        match node {
            TypedPlanNode::ApplyEffect { input, output, instance, lowering, .. } => {
                let invocation_index = effect_bindings.next().ok_or_else(|| {
                    format!(
                        "registered-effect execution completed effect {} without a physical invocation receipt",
                        instance.id.0,
                    )
                })?;
                let mut operation = completed_effect_operation(
                    format!("effect-{}", instance.id.0),
                    instance,
                    lowering.tool.program(),
                );
                operation.inputs.push(EvidenceArtifactRef::signal(input.0));
                operation.outputs.push(EvidenceArtifactRef::signal(output.0));
                operation.invocation_indices.push(invocation_index);
                evidence.operations.push(operation);
                projected_effects += 1;
            }
            TypedPlanNode::Operation {
                operation: operation @ PlanOperation::ResamplePcm { .. },
                input_signal,
                output_signal,
                candidates,
                selected_candidate,
                resolved_parameters,
            } => {
                let invocation_index = resampler_invocation_index.ok_or_else(|| {
                    "registered-effect execution completed a resampler without a physical invocation receipt".to_string()
                })?;
                let selected = candidates.get(*selected_candidate);
                let backend = selected
                    .and_then(|candidate| candidate.tool.as_ref())
                    .map(|tool| ExecutionBackend::external(tool.program()))
                    .unwrap_or(ExecutionBackend::Unknown);
                let mut record = OperationRecord::completed(
                    format!("effect-plan-node-{node_index}"),
                    operation.label(),
                    operation_human_name(operation),
                    backend,
                );
                if let Some(input) = input_signal {
                    record.inputs.push(EvidenceArtifactRef::signal(input.0));
                }
                if let Some(output) = output_signal {
                    record.outputs.push(EvidenceArtifactRef::signal(output.0));
                }
                record.parameters = operation_parameters(operation, resolved_parameters);
                if let Some(candidate) = selected {
                    apply_selected_terminal_realization(&mut record, candidate);
                }
                record.invocation_indices.push(invocation_index);
                evidence.operations.push(record);
                projected_resamplers += 1;
            }
            _ => {}
        }
    }
    if effect_bindings.next().is_some() || projected_effects != effect_invocation_indices.len() {
        return Err(format!(
            "registered-effect execution supplied {} effect invocation receipt(s) for {} projected effect(s)",
            effect_invocation_indices.len(), projected_effects,
        ));
    }
    match (projected_resamplers, resampler_invocation_index) {
        (0, None) | (1, Some(_)) => {}
        (0, Some(_)) => {
            return Err("registered-effect execution supplied a resampler invocation receipt for a plan without a resampler".to_string());
        }
        (count, _) => {
            return Err(format!(
                "registered-effect evidence projection found {count} resampler nodes; expected at most one",
            ));
        }
    }
    Ok(evidence)
}

/// Project typed semantic evidence for a failed ordinary execution attempt.
/// Only operations that have a structured selected-vs-emitted mapping to an
/// invocation that was actually attempted are retained. This deliberately
/// refuses to infer semantics from argv or planner descriptions.
pub fn attempt_plan_evidence(
    request: &PlanRequest,
    invocations: &[CommandRecord],
) -> Result<TrackExecutionEvidence, String> {
    let mut evidence = completed_plan_evidence(request, invocations)?;
    retain_attempted_execution_evidence(&mut evidence, invocations);
    Ok(evidence)
}

fn retain_attempted_execution_evidence(
    evidence: &mut TrackExecutionEvidence,
    invocations: &[CommandRecord],
) {
    evidence.operations.retain_mut(|operation| {
        if operation.invocation_indices.is_empty() {
            return false;
        }
        let mut saw_attempt = false;
        let mut saw_incomplete = false;
        let mut saw_failed = false;
        for index in &operation.invocation_indices {
            let Some(invocation) = invocations.get(*index) else {
                // A mapped later invocation that was never reached makes an
                // already-started multi-invocation operation incomplete.
                saw_incomplete = true;
                continue;
            };
            saw_attempt = true;
            match invocation.exit {
                Some(ProcessExit::Code(0)) => {}
                Some(_) => saw_failed = true,
                None => saw_incomplete = true,
            }
        }
        if !saw_attempt {
            return false;
        }
        operation.status = if saw_failed {
            OperationStatus::Failed
        } else if saw_incomplete {
            OperationStatus::Incomplete
        } else {
            OperationStatus::Completed
        };
        operation.lineage = OperationLineage::DiscardedAttempt;
        true
    });
    evidence.verifications.retain_mut(|verification| {
        if verification.invocation_indices.is_empty() {
            return false;
        }
        let mut saw_attempt = false;
        let mut saw_incomplete = false;
        let mut saw_failed = false;
        for index in &verification.invocation_indices {
            let Some(invocation) = invocations.get(*index) else {
                saw_incomplete = true;
                continue;
            };
            saw_attempt = true;
            match invocation.exit {
                Some(ProcessExit::Code(0)) => {}
                Some(_) => saw_failed = true,
                None => saw_incomplete = true,
            }
        }
        if !saw_attempt {
            return false;
        }
        verification.status = if saw_failed {
            VerificationStatus::Failed
        } else if saw_incomplete {
            VerificationStatus::Incomplete
        } else {
            VerificationStatus::Passed
        };
        true
    });
    evidence.sizes.clear();
}

/// Failed execution still retains typed evidence when the semantic authority
/// can be reconstructed. Generic invocation evidence is only the fallback when
/// the typed projection itself is unavailable.
#[must_use]
pub fn attempt_plan_evidence_or_fallback(
    request: &PlanRequest,
    invocations: &[CommandRecord],
) -> TrackExecutionEvidence {
    match attempt_plan_evidence(request, invocations) {
        Ok(evidence) => evidence,
        Err(error) => {
            let mut evidence = discarded_attempt_evidence_from_invocations(invocations);
            evidence.verifications.push(VerificationRecord {
                id: "semantic-attempt-evidence-projection".to_string(),
                kind: "semantic_evidence_projection".to_string(),
                statement: format!("Typed attempt evidence incomplete: {error}"),
                status: VerificationStatus::Incomplete,
                invocation_indices: Vec::new(),
            });
            evidence
        }
    }
}

/// Semantic projection failure must not cause a performed command to be
/// silently reinterpreted from its text. The portable record still carries the
/// structured invocation transcript; the semantic evidence is explicitly
/// marked incomplete.
#[must_use]
pub fn completed_plan_evidence_or_fallback(
    request: &PlanRequest,
    invocations: &[CommandRecord],
) -> TrackExecutionEvidence {
    match completed_plan_evidence(request, invocations) {
        Ok(evidence) => evidence,
        Err(error) => TrackExecutionEvidence {
            verifications: vec![VerificationRecord {
                id: "semantic-evidence-projection".to_string(),
                kind: "semantic_evidence_projection".to_string(),
                statement: format!("Semantic execution evidence incomplete: {error}"),
                status: VerificationStatus::Incomplete,
                invocation_indices: Vec::new(),
            }],
            ..TrackExecutionEvidence::default()
        },
    }
}

/// Failure records without a surviving typed semantic plan retain physical
/// execution evidence without guessing semantics from command text.
#[must_use]
pub fn discarded_attempt_evidence_from_invocations(
    invocations: &[CommandRecord],
) -> TrackExecutionEvidence {
    let operations = invocations
        .iter()
        .enumerate()
        .filter(|(_, invocation)| invocation.exit.is_some())
        .map(|(index, invocation)| {
            let status = if invocation_completed(invocation) {
                OperationStatus::Completed
            } else {
                OperationStatus::Failed
            };
            OperationRecord {
                id: format!("attempt-invocation-{index}"),
                kind: "external_invocation".to_string(),
                name: "External processing invocation".to_string(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                parameters: Vec::new(),
                backend: ExecutionBackend::external(invocation.binary.default_name()),
                invocation_indices: vec![index],
                decision_ids: Vec::new(),
                observation_ids: Vec::new(),
                domain: OperationDomain::AudioSignal,
                status,
                lineage: OperationLineage::DiscardedAttempt,
                detail: None,
            }
        })
        .collect();
    TrackExecutionEvidence {
        operations,
        ..TrackExecutionEvidence::default()
    }
}

#[must_use]
pub fn completed_effect_operation(
    id: impl Into<String>,
    effect: &EffectIntent,
    backend: &str,
) -> OperationRecord {
    let (kind, name, parameters) = match &effect.effect {
        RegisteredUnaryEffect::SoxHighPass { frequency_hz }
        | RegisteredUnaryEffect::FfmpegHighPass { frequency_hz } => (
            "high_pass",
            "High-pass filter",
            vec![OperationParameter::new(
                "Cutoff",
                EvidenceValue::SampleRateHz(*frequency_hz),
            )],
        ),
        RegisteredUnaryEffect::SoxLowPass { frequency_hz }
        | RegisteredUnaryEffect::FfmpegLowPass { frequency_hz } => (
            "low_pass",
            "Low-pass filter",
            vec![OperationParameter::new(
                "Cutoff",
                EvidenceValue::SampleRateHz(*frequency_hz),
            )],
        ),
        RegisteredUnaryEffect::SoxSamplePeakNormalize { target_dbfs } => (
            "sample_peak_normalize",
            "Sample-peak normalization",
            vec![OperationParameter::new(
                "Target",
                EvidenceValue::Text(format!("{target_dbfs:?}")),
            )],
        ),
        RegisteredUnaryEffect::CdDeemphasis => (
            "cd_deemphasis",
            "CD de-emphasis",
            Vec::new(),
        ),
    };
    let mut record = OperationRecord::completed(id, kind, name, ExecutionBackend::external(backend));
    record.parameters = parameters;
    record
}

pub fn record_native_scalar_materialization(
    evidence: &mut TrackExecutionEvidence,
    pump: &RetainedPcmScalarPump,
) {
    if let Some(operation) = evidence.operations.iter_mut().find(|operation| {
        operation.kind == "apply_gain"
            && operation.status == OperationStatus::Completed
            && operation.lineage == OperationLineage::DeliveredAudio
    }) {
        operation.backend = ExecutionBackend::native("TonePoet Float64 scalar materializer");
        if !operation.parameters.iter().any(|parameter| parameter.name == "Gain") {
            operation.parameters.push(OperationParameter::new(
                "Gain",
                EvidenceValue::Text(format!("{:?}", pump.gain_db)),
            ));
        }
        return;
    }

    let mut operation = OperationRecord::completed(
        "native-scalar-materialization",
        "apply_gain",
        "Waveform gain",
        ExecutionBackend::native("TonePoet Float64 scalar materializer"),
    );
    operation.inputs.push(EvidenceArtifactRef::path(
        "retained-pcm-input",
        &pump.input_path,
    ));
    operation.parameters.push(OperationParameter::new(
        "Gain",
        EvidenceValue::Text(format!("{:?}", pump.gain_db)),
    ));
    evidence.operations.push(operation);
}

pub fn record_native_scalar_attempt_materialization(
    evidence: &mut TrackExecutionEvidence,
    pump: &RetainedPcmScalarPump,
) {
    if let Some(operation) = evidence.operations.iter_mut().find(|operation| {
        operation.kind == "apply_gain" && operation.lineage == OperationLineage::DiscardedAttempt
    }) {
        operation.backend = ExecutionBackend::native("TonePoet Float64 scalar pump");
        if !operation.parameters.iter().any(|parameter| parameter.name == "Gain") {
            operation.parameters.push(OperationParameter::new(
                "Gain",
                EvidenceValue::Text(format!("{:?}", pump.gain_db)),
            ));
        }
        return;
    }

    // The native scalar materializer has no external invocation receipt of its
    // own. On an error return we know it was part of the attempted route, but
    // the ordinary command transcript is not sufficient authority to claim
    // that the in-process pump itself completed. Preserve the semantic/backend
    // truth conservatively without inventing a command association.
    let mut operation = OperationRecord::completed(
        "native-scalar-attempt",
        "apply_gain",
        "Waveform gain",
        ExecutionBackend::native("TonePoet Float64 scalar pump"),
    );
    operation.inputs.push(EvidenceArtifactRef::path(
        "retained-pcm-input",
        &pump.input_path,
    ));
    operation.parameters.push(OperationParameter::new(
        "Gain",
        EvidenceValue::Text(format!("{:?}", pump.gain_db)),
    ));
    operation.status = OperationStatus::Incomplete;
    operation.lineage = OperationLineage::DiscardedAttempt;
    evidence.operations.push(operation);
}

#[must_use]
pub fn invocation_completed(invocation: &CommandRecord) -> bool {
    matches!(invocation.exit, Some(ProcessExit::Code(0)))
}

fn operation_domain(operation: &PlanOperation) -> OperationDomain {
    match operation {
        PlanOperation::MetadataTransfer { .. } | PlanOperation::StoreSourceAudioMd5 { .. } => {
            OperationDomain::Artifact
        }
        _ => OperationDomain::AudioSignal,
    }
}

fn operation_human_name(operation: &PlanOperation) -> &'static str {
    match operation {
        PlanOperation::DecodeToPcm { .. } => "Decode to PCM",
        PlanOperation::ResamplePcm { .. } => "Sample-rate conversion",
        PlanOperation::EncodePcm { .. } => "PCM encoding / terminal realization",
        PlanOperation::EncodeLossy { .. } => "Lossy encoding",
        PlanOperation::PcmToDsd { .. } => "PCM-to-DSD conversion",
        PlanOperation::DsdToPcm { .. } => "DSD-to-PCM conversion",
        PlanOperation::DsdRateChange { .. } => "DSD rate conversion",
        PlanOperation::MetadataTransfer { .. } => "Metadata transfer",
        PlanOperation::StoreSourceAudioMd5 { .. } => "Store source-audio MD5",
        PlanOperation::Verify { .. } => "Terminal verification",
    }
}

fn compound_terminal_invocation_indices(
    request: &PlanRequest,
    realization: &tonepoet_pipeline::SelectedPcmTerminalRealization,
    emitted_anchor_indices: &[usize],
) -> Result<Option<Vec<usize>>, String> {
    use tonepoet_pipeline::PcmTerminalRealizationKind as Kind;

    let preterminal_kind = match realization.kind {
        Kind::SsrcPreterminalFfmpegPackage | Kind::SsrcPreterminalSoxPackage => {
            CompoundPreterminalKind::Ssrc
        }
        Kind::SoxPreterminalFfmpegPackage | Kind::SoxPreterminalWavPackHybrid => {
            CompoundPreterminalKind::Sox
        }
        Kind::FfmpegPreterminalWavPackHybrid => CompoundPreterminalKind::Ffmpeg,
        _ => return Ok(None),
    };

    let TopologyPlan::Execute { steps, .. } =
        plan_topology(request).map_err(|error| error.to_string())?
    else {
        return Err("compound terminal realization unexpectedly lowered as passthrough".to_string());
    };

    let package_indices = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match &step.operation {
            PlanOperation::EncodePcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                ..
            } if target_format == &realization.target_format
                && target_rate_hz == &realization.target_rate_hz
                && target_bit_depth == &realization.target_bit_depth => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();
    if package_indices.len() != 1 {
        return Err(format!(
            "compound terminal evidence found {} structured package steps; expected one",
            package_indices.len(),
        ));
    }
    let package_index = package_indices[0];
    let package = &steps[package_index];
    let package_input = package.input.as_path().ok_or_else(|| {
        "compound terminal package has no path-backed structured input".to_string()
    })?;

    let preterminal_indices = steps
        .iter()
        .enumerate()
        .take(package_index)
        .filter_map(|(index, step)| {
            if step.output.as_path() != Some(package_input) {
                return None;
            }
            let matches_kind = match (preterminal_kind, &step.operation) {
                (
                    CompoundPreterminalKind::Ssrc,
                    PlanOperation::ResamplePcm {
                        target_bit_depth: Some(target_bit_depth),
                        ..
                    },
                ) => target_bit_depth == &realization.target_bit_depth,
                (
                    CompoundPreterminalKind::Sox | CompoundPreterminalKind::Ffmpeg,
                    PlanOperation::EncodePcm {
                        target_format: tonepoet_pipeline::AudioFormat::Wav,
                        target_bit_depth,
                        apply_processing: true,
                        ..
                    },
                ) => target_bit_depth == &realization.target_bit_depth,
                _ => false,
            };
            matches_kind.then_some(index)
        })
        .collect::<Vec<_>>();
    if preterminal_indices.len() != 1 {
        return Err(format!(
            "compound terminal evidence found {} structured preterminal producers; expected one",
            preterminal_indices.len(),
        ));
    }
    let preterminal_index = preterminal_indices[0];
    if preterminal_index + 1 != package_index {
        return Err(
            "compound terminal preterminal and package are no longer adjacent structured steps"
                .to_string(),
        );
    }
    if emitted_anchor_indices.len() != 1 {
        return Err(format!(
            "compound terminal evidence found {} Stage-A emitted anchors; expected one",
            emitted_anchor_indices.len(),
        ));
    }
    let topology_anchor = match preterminal_kind {
        CompoundPreterminalKind::Ssrc => preterminal_index,
        CompoundPreterminalKind::Sox | CompoundPreterminalKind::Ffmpeg => package_index,
    };
    let emitted_anchor = emitted_anchor_indices[0];
    let map_topology_index = |index: usize| -> Result<usize, String> {
        if emitted_anchor >= topology_anchor {
            index
                .checked_add(emitted_anchor - topology_anchor)
                .ok_or_else(|| "compound terminal emitted-index offset overflow".to_string())
        } else {
            index
                .checked_sub(topology_anchor - emitted_anchor)
                .ok_or_else(|| "compound terminal emitted-index offset underflow".to_string())
        }
    };

    Ok(Some(vec![
        map_topology_index(preterminal_index)?,
        map_topology_index(package_index)?,
    ]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompoundPreterminalKind {
    Ssrc,
    Sox,
    Ffmpeg,
}

fn apply_selected_terminal_realization(
    record: &mut OperationRecord,
    candidate: &tonepoet_pipeline::PhysicalCandidate,
) {
    let Some(tonepoet_pipeline::SelectedTerminalRealization::Pcm(realization)) =
        candidate.contract.terminal_realization.as_ref()
    else {
        return;
    };
    apply_pcm_terminal_realization(record, realization);
}

fn apply_pcm_terminal_realization(
    record: &mut OperationRecord,
    realization: &tonepoet_pipeline::SelectedPcmTerminalRealization,
) {
    use tonepoet_pipeline::PcmTerminalRealizationKind as Kind;
    record.backend = match realization.kind {
        Kind::SsrcPreterminalFfmpegPackage => ExecutionBackend::Composite {
            description: "SSRC terminal sample realization + FFmpeg packaging".to_string(),
        },
        Kind::SsrcPreterminalSoxPackage => ExecutionBackend::Composite {
            description: "SSRC terminal sample realization + SoX packaging".to_string(),
        },
        Kind::SoxPreterminalFfmpegPackage => ExecutionBackend::Composite {
            description: "SoX terminal sample realization + FFmpeg packaging".to_string(),
        },
        Kind::SoxPreterminalWavPackHybrid => ExecutionBackend::Composite {
            description: "SoX terminal sample realization + WavPack hybrid packaging".to_string(),
        },
        Kind::FfmpegPreterminalWavPackHybrid => ExecutionBackend::Composite {
            description: "FFmpeg/SoXR terminal sample realization + WavPack hybrid packaging".to_string(),
        },
        _ => record.backend.clone(),
    };

    record.parameters.extend([
        OperationParameter::new(
            "Terminal realization",
            EvidenceValue::Text(format!("{:?}", realization.kind)),
        ),
        OperationParameter::new(
            "Terminal input precision",
            EvidenceValue::BitDepth(match &realization.input_precision {
                tonepoet_pipeline::StoragePrecision::Pcm(depth) => format!("{depth:?}"),
                other => format!("{other:?}"),
            }),
        ),
        OperationParameter::new(
            "Terminal input value domain",
            EvidenceValue::Text(format!("{:?}", realization.input_value_domain)),
        ),
        OperationParameter::new(
            "Terminal output format",
            EvidenceValue::Text(format!("{:?}", realization.target_format)),
        ),
        OperationParameter::new(
            "Terminal output precision",
            EvidenceValue::BitDepth(format!("{:?}", realization.target_bit_depth)),
        ),
        OperationParameter::new(
            "Terminal sample/quantization owner",
            EvidenceValue::Text(terminal_sample_owner(realization.kind).to_string()),
        ),
        OperationParameter::new(
            "Dither owner",
            EvidenceValue::Text(terminal_dither_owner(realization.dither_owner).to_string()),
        ),
        OperationParameter::new(
            "Effective terminal dither",
            EvidenceValue::Text(
                realization
                    .effective_dither
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "None".to_string()),
            ),
        ),
    ]);
    if let Some(rate) = realization.target_rate_hz {
        record.parameters.push(OperationParameter::new(
            "Terminal output rate",
            EvidenceValue::SampleRateHz(rate),
        ));
    }
    if let Some(ssrc) = realization.ssrc_dither.as_ref() {
        if let Some(id) = ssrc.dither_id {
            record.parameters.push(OperationParameter::new(
                "SSRC native dither id",
                EvidenceValue::Unsigned(u64::from(id)),
            ));
        }
        record.parameters.push(OperationParameter::new(
            "SSRC PDF",
            EvidenceValue::Text(
                ssrc.pdf_type
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|| "None".to_string()),
            ),
        ));
        record.parameters.push(OperationParameter::new(
            "SSRC dither origin",
            EvidenceValue::Text(format!("{:?}", ssrc.origin)),
        ));
    }
}

fn terminal_sample_owner(kind: tonepoet_pipeline::PcmTerminalRealizationKind) -> &'static str {
    use tonepoet_pipeline::PcmTerminalRealizationKind as Kind;
    match kind {
        Kind::SsrcDirectWav
        | Kind::SsrcPreterminalFfmpegPackage
        | Kind::SsrcPreterminalSoxPackage => "SSRC",
        Kind::SoxDirect
        | Kind::SoxPreterminalFfmpegPackage
        | Kind::SoxPreterminalWavPackHybrid => "SoX",
        Kind::FfmpegDirect => "FFmpeg",
        Kind::FfmpegPreterminalWavPackHybrid => "FFmpeg/SoXR",
        Kind::NativeWavPackHybridPackage => "native WavPack hybrid packager",
    }
}

fn terminal_dither_owner(owner: tonepoet_pipeline::PcmTerminalDitherOwner) -> &'static str {
    use tonepoet_pipeline::PcmTerminalDitherOwner as Owner;
    match owner {
        Owner::None => "none",
        Owner::SelectedTerminal => "selected terminal backend",
        Owner::SoxPreterminal => "SoX preterminal",
        Owner::FfmpegPreterminal => "FFmpeg/SoXR preterminal",
        Owner::SsrcResampler => "SSRC resampler",
    }
}

fn operation_parameters(
    operation: &PlanOperation,
    resolved: &ResolvedOperationParameters,
) -> Vec<OperationParameter> {
    let mut parameters = Vec::new();
    match operation {
        PlanOperation::DecodeToPcm { bit_depth } => parameters.push(OperationParameter::new(
            "Output precision",
            EvidenceValue::BitDepth(format!("{bit_depth:?}")),
        )),
        PlanOperation::ResamplePcm {
            target_rate_hz,
            target_bit_depth,
            profile,
            brick_wall,
        } => {
            parameters.push(OperationParameter::new(
                "Target rate",
                EvidenceValue::SampleRateHz(*target_rate_hz),
            ));
            if let Some(depth) = target_bit_depth {
                parameters.push(OperationParameter::new(
                    "Target precision",
                    EvidenceValue::BitDepth(format!("{depth:?}")),
                ));
            }
            if let Some(profile) = profile {
                parameters.push(OperationParameter::new(
                    "Profile",
                    EvidenceValue::Text(format!("{profile:?}")),
                ));
            }
            parameters.push(OperationParameter::new(
                "Brick-wall",
                EvidenceValue::Bool(*brick_wall),
            ));
        }
        PlanOperation::EncodePcm {
            target_format,
            target_rate_hz,
            target_bit_depth,
            apply_processing,
        } => {
            parameters.push(OperationParameter::new(
                "Format",
                EvidenceValue::Text(format!("{target_format:?}")),
            ));
            if let Some(rate) = target_rate_hz {
                parameters.push(OperationParameter::new(
                    "Target rate",
                    EvidenceValue::SampleRateHz(*rate),
                ));
            }
            parameters.push(OperationParameter::new(
                "Target precision",
                EvidenceValue::BitDepth(format!("{target_bit_depth:?}")),
            ));
            parameters.push(OperationParameter::new(
                "Terminal processing",
                EvidenceValue::Bool(*apply_processing),
            ));
        }
        PlanOperation::EncodeLossy {
            target_format,
            target_rate_hz,
            apply_processing,
        } => {
            parameters.push(OperationParameter::new(
                "Format",
                EvidenceValue::Text(format!("{target_format:?}")),
            ));
            if let Some(rate) = target_rate_hz {
                parameters.push(OperationParameter::new(
                    "Target rate",
                    EvidenceValue::SampleRateHz(*rate),
                ));
            }
            parameters.push(OperationParameter::new(
                "Terminal processing",
                EvidenceValue::Bool(*apply_processing),
            ));
        }
        PlanOperation::PcmToDsd { target_format, target_rate, filter } => {
            parameters.push(OperationParameter::new("Format", EvidenceValue::Text(format!("{target_format:?}"))));
            parameters.push(OperationParameter::new("DSD rate", EvidenceValue::Text(format!("{target_rate:?}"))));
            parameters.push(OperationParameter::new("Filter", EvidenceValue::Text(format!("{filter:?}"))));
        }
        PlanOperation::DsdToPcm { target_format, target_rate_hz, target_bit_depth, lowpass } => {
            parameters.push(OperationParameter::new("Format", EvidenceValue::Text(format!("{target_format:?}"))));
            parameters.push(OperationParameter::new("Target rate", EvidenceValue::SampleRateHz(*target_rate_hz)));
            parameters.push(OperationParameter::new("Target precision", EvidenceValue::BitDepth(format!("{target_bit_depth:?}"))));
            parameters.push(OperationParameter::new("Low-pass", EvidenceValue::Text(format!("{lowpass:?}"))));
        }
        PlanOperation::DsdRateChange { target_format, target_rate, lowpass } => {
            parameters.push(OperationParameter::new("Format", EvidenceValue::Text(format!("{target_format:?}"))));
            parameters.push(OperationParameter::new("DSD rate", EvidenceValue::Text(format!("{target_rate:?}"))));
            parameters.push(OperationParameter::new("Low-pass", EvidenceValue::Text(format!("{lowpass:?}"))));
        }
        PlanOperation::MetadataTransfer { transfer_tags, preserve_artwork, .. } => {
            parameters.push(OperationParameter::new("Tags", EvidenceValue::Bool(*transfer_tags)));
            parameters.push(OperationParameter::new("Artwork", EvidenceValue::Bool(*preserve_artwork)));
        }
        PlanOperation::StoreSourceAudioMd5 { .. } | PlanOperation::Verify { .. } => {}
    }

    match resolved {
        ResolvedOperationParameters::None => {}
        ResolvedOperationParameters::ResampleSsrc { effective_profile, effective_attenuation_db, effective_output_depth, computation_precision, effective_dither, authority_reason, .. } => {
            parameters.push(OperationParameter::new("SSRC profile", EvidenceValue::Text(format!("{effective_profile:?}"))));
            if let Some(db) = effective_attenuation_db {
                parameters.push(OperationParameter::new("Attenuation", EvidenceValue::Db(f64::from(*db))));
            }
            parameters.push(OperationParameter::new("Output precision", EvidenceValue::BitDepth(format!("{effective_output_depth:?}"))));
            parameters.push(OperationParameter::new("Computation precision", EvidenceValue::Text(format!("{computation_precision:?}"))));
            parameters.push(OperationParameter::new("Dither", EvidenceValue::Text(format!("{effective_dither:?}"))));
            parameters.push(OperationParameter::new("Authority", EvidenceValue::Text(format!("{authority_reason:?}"))));
        }
        ResolvedOperationParameters::ResampleSox { quality, effective_bandwidth_pct, effective_sinc_passband_hz, effective_dither, .. } => {
            parameters.push(OperationParameter::new("Quality", EvidenceValue::Text(format!("{quality:?}"))));
            if let Some(value) = effective_bandwidth_pct { parameters.push(OperationParameter::new("Bandwidth", EvidenceValue::Percent(f64::from(*value)))); }
            if let Some(value) = effective_sinc_passband_hz { parameters.push(OperationParameter::new("Sinc passband", EvidenceValue::Decimal(f64::from(*value))).with_unit("Hz")); }
            if let Some(value) = effective_dither { parameters.push(OperationParameter::new("Dither", EvidenceValue::Text(format!("{value:?}")))); }
        }
        ResolvedOperationParameters::ResampleSoxr { effective_precision, effective_cutoff, effective_phase, effective_dither, .. } => {
            parameters.push(OperationParameter::new("Precision", EvidenceValue::Unsigned(u64::from(*effective_precision))));
            parameters.push(OperationParameter::new("Cutoff", EvidenceValue::Decimal(f64::from(*effective_cutoff))));
            if let Some(value) = effective_phase { parameters.push(OperationParameter::new("Phase", EvidenceValue::Unsigned(u64::from(*value)))); }
            if let Some(value) = effective_dither { parameters.push(OperationParameter::new("Dither", EvidenceValue::Text(format!("{value:?}")))); }
        }
        ResolvedOperationParameters::DsdToPcm { effective_sinc, effective_dither, .. } => {
            if let Some(value) = effective_sinc { parameters.push(OperationParameter::new("Sinc", EvidenceValue::Text(format!("{value:?}")))); }
            if let Some(value) = effective_dither { parameters.push(OperationParameter::new("Dither", EvidenceValue::Text(format!("{value:?}")))); }
        }
        ResolvedOperationParameters::PcmToDsd { effective_sinc, effective_gain_compensation, .. } => {
            if let Some(value) = effective_sinc { parameters.push(OperationParameter::new("Sinc", EvidenceValue::Text(format!("{value:?}")))); }
            parameters.push(OperationParameter::new("Gain compensation", EvidenceValue::Text(format!("{effective_gain_compensation:?}"))));
        }
        ResolvedOperationParameters::DsdRateChange { effective_from_sinc, .. } => {
            if let Some(value) = effective_from_sinc { parameters.push(OperationParameter::new("Sinc", EvidenceValue::Text(format!("{value:?}")))); }
        }
        ResolvedOperationParameters::EncodeFlac { effective_dither, .. }
        | ResolvedOperationParameters::EncodeWavPack { effective_dither, .. }
        | ResolvedOperationParameters::EncodePcm { effective_dither } => {
            if let Some(value) = effective_dither { parameters.push(OperationParameter::new("Dither", EvidenceValue::Text(format!("{value:?}")))); }
        }
        ResolvedOperationParameters::EncodeMp3 { .. }
        | ResolvedOperationParameters::EncodeAac { .. }
        | ResolvedOperationParameters::EncodeOpus { .. } => {}
    }
    parameters
}

fn decision_kind_name(kind: &DecisionKind) -> &'static str {
    match kind {
        DecisionKind::TruePeakGain { .. } => "true_peak_gain",
        DecisionKind::ReferenceGain { .. } => "reference_gain",
        DecisionKind::ReplayGainProjection { .. } => "replay_gain",
    }
}

fn decision_summary(kind: &DecisionKind) -> String {
    match kind {
        DecisionKind::TruePeakGain { .. } => "Resolved true-peak gain policy".to_string(),
        DecisionKind::ReferenceGain { .. } => "Resolved qualified Reference gain policy".to_string(),
        DecisionKind::ReplayGainProjection { .. } => "Resolved ReplayGain projection".to_string(),
    }
}


#[derive(Debug, Clone, Serialize)]
pub struct PortableExecutionRecord {
    pub schema: &'static str,
    pub job_id: String,
    pub item_id: String,
    pub source_container: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<PortableTrackExecutionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prior_attempts: Vec<PriorAttemptSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<PortableDeliveryRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub postprocessing: Vec<PortablePostprocessingRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<PortableActionPhaseRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortableTrackExecutionRecord {
    pub track_id: String,
    pub outcome: String,
    #[serde(default)]
    pub execution_evidence: TrackExecutionEvidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocations: Vec<PortableInvocation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortableDeliveryRecord {
    pub path: PathBuf,
    pub role: String,
    pub bytes: u64,
}

/// Consequential post-processing facts that are already authoritative at the
/// pipeline stage boundary. These remain distinct from delivered-audio
/// operations because metadata and ReplayGain tag work are not interchangeable
/// with waveform transformation.
#[derive(Debug, Clone, Serialize)]
pub struct PortablePostprocessingRecord {
    pub kind: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortableActionPhaseRecord {
    pub phase: String,
    pub recovery_required: bool,
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<PortableActionRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortableActionRecord {
    pub index: usize,
    pub kind: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<PortableActionOperationRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortableActionOperationRecord {
    pub id: String,
    pub summary: String,
    pub status: String,
}

#[must_use]
pub fn portable_execution_record(
    report: &super::types::PipelineReport,
) -> PortableExecutionRecord {
    let records: Vec<&super::types::TrackRecord> = match &report.outcome {
        super::types::AlbumOutcome::Complete { tracks, .. } => tracks.iter().collect(),
        super::types::AlbumOutcome::Partial { successful, failed, .. }
        | super::types::AlbumOutcome::Blocked { successful, failed, .. } => {
            successful.iter().chain(failed.iter()).collect()
        }
    };
    let tracks = records
        .into_iter()
        .map(|record| {
            let outcome = match &record.outcome {
                super::types::TrackOutcome::Ok => "success".to_string(),
                super::types::TrackOutcome::Err(error) => format!("failure: {error}"),
                super::types::TrackOutcome::Blocked(reason) => format!("blocked: {reason}"),
            };
            PortableTrackExecutionRecord {
                track_id: portable_track_id(&record.track_id),
                outcome,
                execution_evidence: record.execution_evidence.clone(),
                invocations: record.commands.iter().map(PortableInvocation::from).collect(),
            }
        })
        .collect();

    let deliveries = report
        .published
        .as_ref()
        .into_iter()
        .flat_map(|album| album.entries.iter())
        .map(|entry| PortableDeliveryRecord {
            path: entry.final_path.clone(),
            role: match &entry.role {
                super::types::PublishRole::Audio => "audio".to_string(),
                super::types::PublishRole::Sidecar(kind) => format!("sidecar:{kind:?}"),
            },
            bytes: entry.bytes,
        })
        .collect();

    let stage_records: &[super::types::StageRecord] = match &report.outcome {
        super::types::AlbumOutcome::Complete { stages, .. }
        | super::types::AlbumOutcome::Partial { stages, .. }
        | super::types::AlbumOutcome::Blocked { stages, .. } => stages,
    };
    let postprocessing = stage_records
        .iter()
        .filter_map(|record| {
            let kind = match record.stage {
                super::types::PipelineStage::Metadata => "metadata",
                super::types::PipelineStage::ReplayGain => "replaygain",
                _ => return None,
            };
            let (status, detail) = match &record.outcome {
                super::types::StageOutcome::Ok => ("completed", None),
                super::types::StageOutcome::OkWithDetail(detail) => {
                    ("completed", Some(detail.clone()))
                }
                super::types::StageOutcome::NotRequested => ("not_requested", None),
                super::types::StageOutcome::Skipped => ("skipped", None),
                super::types::StageOutcome::SkippedWithReason(reason) => {
                    ("skipped", Some(reason.clone()))
                }
                super::types::StageOutcome::Failed(error) => {
                    ("failed", Some(error.clone()))
                }
            };
            Some(PortablePostprocessingRecord {
                kind: kind.to_string(),
                status: status.to_string(),
                detail,
            })
        })
        .collect();

    let actions = report
        .action_reports
        .iter()
        .map(|phase| PortableActionPhaseRecord {
            phase: phase
                .phase
                .map(|phase| format!("{phase:?}"))
                .unwrap_or_else(|| "unknown".to_string()),
            recovery_required: phase.recovery_required,
            cancelled: phase.cancelled,
            actions: phase
                .actions
                .iter()
                .map(|action| PortableActionRecord {
                    index: action.index,
                    kind: action.kind.clone(),
                    status: format!("{:?}", action.status),
                    error: action.error.clone(),
                    operations: action
                        .operations
                        .iter()
                        .map(|operation| PortableActionOperationRecord {
                            id: operation.operation_id.clone(),
                            summary: operation.summary.clone(),
                            status: format!("{:?}", operation.status),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    PortableExecutionRecord {
        schema: PORTABLE_EXECUTION_SCHEMA,
        job_id: report.request.job_id.clone(),
        item_id: report.request.item_id.clone(),
        source_container: report.request.container.clone(),
        tracks,
        prior_attempts: report.prior_attempts.clone(),
        deliveries,
        postprocessing,
        actions,
    }
}

fn portable_track_id(track_id: &super::types::TrackId) -> String {
    match track_id.disc_number {
        Some(disc) => format!("disc-{disc}:track-{}:source-{}", track_id.track_number, track_id.source_ordinal),
        None => format!("track-{}:source-{}", track_id.track_number, track_id.source_ordinal),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorAttemptTrackSummary {
    pub track_id: String,
    pub outcome: String,
    #[serde(default)]
    pub execution_evidence: TrackExecutionEvidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocations: Vec<PortableInvocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorAttemptSummary {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<PriorAttemptTrackSummary>,
}

const MAX_PRIOR_ATTEMPT_TRACKS: usize = 64;
const MAX_PRIOR_ATTEMPT_INVOCATIONS_PER_TRACK: usize = 64;
const MAX_PRIOR_ATTEMPT_OPERATIONS_PER_TRACK: usize = 64;
const MAX_PRIOR_ATTEMPT_FACTS_PER_TRACK: usize = 64;
const MAX_PRIOR_ATTEMPTS: usize = 2;

fn bounded_prior_attempt_evidence(source: &TrackExecutionEvidence) -> TrackExecutionEvidence {
    let mut evidence = source.clone();
    evidence.operations.truncate(MAX_PRIOR_ATTEMPT_OPERATIONS_PER_TRACK);
    evidence.observations.truncate(MAX_PRIOR_ATTEMPT_FACTS_PER_TRACK);
    evidence.decisions.truncate(MAX_PRIOR_ATTEMPT_FACTS_PER_TRACK);
    evidence.verifications.truncate(MAX_PRIOR_ATTEMPT_FACTS_PER_TRACK);
    evidence.sizes.truncate(MAX_PRIOR_ATTEMPT_FACTS_PER_TRACK);
    evidence
}

#[must_use]
pub fn summarize_prior_attempt(
    report: &super::types::PipelineReport,
    reason: impl Into<String>,
) -> PriorAttemptSummary {
    let records: Vec<&super::types::TrackRecord> = match &report.outcome {
        super::types::AlbumOutcome::Complete { tracks: records, .. } => records.iter().collect(),
        super::types::AlbumOutcome::Partial { successful, failed, .. }
        | super::types::AlbumOutcome::Blocked { successful, failed, .. } => {
            successful.iter().chain(failed.iter()).collect()
        }
    };
    summarize_track_records_prior_attempt(records, reason)
}

#[must_use]
pub fn summarize_track_records_prior_attempt<'a>(
    records: impl IntoIterator<Item = &'a super::types::TrackRecord>,
    reason: impl Into<String>,
) -> PriorAttemptSummary {
    let mut tracks = Vec::new();
    for record in records.into_iter().take(MAX_PRIOR_ATTEMPT_TRACKS) {
        let outcome = match &record.outcome {
            super::types::TrackOutcome::Ok => "success".to_string(),
            super::types::TrackOutcome::Err(error) => format!("failure: {error}"),
            super::types::TrackOutcome::Blocked(reason) => format!("blocked: {reason}"),
        };
        tracks.push(PriorAttemptTrackSummary {
            track_id: portable_track_id(&record.track_id),
            outcome,
            execution_evidence: bounded_prior_attempt_evidence(&record.execution_evidence),
            invocations: record
                .commands
                .iter()
                .take(MAX_PRIOR_ATTEMPT_INVOCATIONS_PER_TRACK)
                .map(PortableInvocation::from)
                .collect(),
        });
    }
    PriorAttemptSummary {
        reason: reason.into(),
        tracks,
    }
}

pub fn append_prior_attempt_bounded(
    attempts: &mut Vec<PriorAttemptSummary>,
    attempt: PriorAttemptSummary,
) {
    if attempts.len() >= MAX_PRIOR_ATTEMPTS {
        attempts.remove(0);
    }
    attempts.push(attempt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_future_operation_is_visible_without_special_renderer() {
        let mut evidence = TrackExecutionEvidence::default();
        let mut operation = OperationRecord::completed(
            "future-1",
            "future_spectral_sculpt",
            "Future spectral sculpt",
            ExecutionBackend::native("tonepoet::future_dsp"),
        );
        operation.parameters.push(OperationParameter::new(
            "Intensity",
            EvidenceValue::Percent(37.5),
        ));
        evidence.operations.push(operation);

        let rendered = render_track_processing(&evidence).join("\n");
        assert!(rendered.contains("Future spectral sculpt"));
        assert!(rendered.contains("Intensity: 37.5%"));
        assert!(rendered.contains("tonepoet::future_dsp"));

        let json = serde_json::to_string(&evidence).expect("serialize execution evidence");
        assert!(json.contains("future_spectral_sculpt"));
        assert!(json.contains("future_dsp"));
    }

    #[test]
    fn discarded_attempt_is_not_rendered_as_delivered_processing() {
        let mut evidence = TrackExecutionEvidence::default();
        evidence.operations.push(OperationRecord::completed(
            "op-1",
            "resample_pcm",
            "Sample-rate conversion",
            ExecutionBackend::external("ssrc"),
        ));
        evidence.mark_discarded_attempt();
        assert!(render_track_processing(&evidence).is_empty());
        assert!(render_discarded_attempt_processing(&evidence)
            .join("\n")
            .contains("Sample-rate conversion"));
    }


    fn effect_projection_request(source_rate_hz: u32, target_rate_hz: u32) -> PlanRequest {
        let mut settings = tonepoet_pipeline::PipelineSettings::default();
        settings.target_format = tonepoet_pipeline::AudioFormat::Flac;
        settings.target_sample_rate = tonepoet_pipeline::RateTarget::PcmHz(target_rate_hz);
        settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(
            tonepoet_pipeline::PcmBitDepth::Int24,
        );
        PlanRequest {
            input_path: PathBuf::from("input.wav"),
            output_path: PathBuf::from("output.flac"),
            source: tonepoet_pipeline::SourceInfo {
                format: tonepoet_pipeline::AudioFormat::Wav,
                codec: tonepoet_pipeline::AudioCodec::PcmSigned,
                sample_rate_hz: Some(source_rate_hz),
                bit_depth: Some(tonepoet_pipeline::PcmBitDepth::Int24),
                true_source_depth: Some(tonepoet_pipeline::PcmBitDepth::Int24),
                source_representation: tonepoet_pipeline::SourceRepresentationKind::Pcm,
                sample_kind: Some(tonepoet_pipeline::SampleKind::SignedInteger),
                channels: Some(2),
                duration: Some(Duration::from_secs(60)),
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: tonepoet_pipeline::PlanScope::track("execution-evidence-test"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    #[test]
    fn registered_effect_receipts_bind_one_fused_and_resample_invocations() {
        let same_rate = effect_projection_request(48_000, 48_000);
        let one_effect = tonepoet_pipeline::explicit_effect_sequence([
            RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
        ]);
        let one = completed_registered_effects_from_plan(&same_rate, &one_effect, &[3], None)
            .expect("one-effect evidence");
        assert_eq!(one.operations.len(), 1);
        assert_eq!(one.operations[0].invocation_indices, vec![3]);
        let one_json = portable_test_track_json(&one, 4);
        assert!(one_json.contains("\"invocation_indices\":[3]"));

        let fused_effects = tonepoet_pipeline::explicit_effect_sequence([
            RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            RegisteredUnaryEffect::SoxLowPass { frequency_hz: 20_000 },
        ]);
        let fused = completed_registered_effects_from_plan(&same_rate, &fused_effects, &[7, 7], None)
            .expect("fused-effect evidence");
        let effect_records = fused
            .operations
            .iter()
            .filter(|operation| matches!(operation.kind.as_str(), "high_pass" | "low_pass"))
            .collect::<Vec<_>>();
        assert_eq!(effect_records.len(), 2);
        assert!(effect_records
            .iter()
            .all(|operation| operation.invocation_indices == vec![7]));
        let fused_json = portable_test_track_json(&fused, 8);
        assert_eq!(fused_json.matches("\"invocation_indices\":[7]").count(), 2);

        let rate_change = effect_projection_request(96_000, 44_100);
        let rate_effect = tonepoet_pipeline::explicit_effect_sequence([
            RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
        ]);
        let with_resample = completed_registered_effects_from_plan(
            &rate_change,
            &rate_effect,
            &[11],
            Some(10),
        )
        .expect("effect plus resample evidence");
        let effect = with_resample
            .operations
            .iter()
            .find(|operation| operation.kind == "high_pass")
            .expect("effect record");
        let resample = with_resample
            .operations
            .iter()
            .find(|operation| operation.kind == "resample_pcm")
            .expect("resample record");
        assert_eq!(effect.invocation_indices, vec![11]);
        assert_eq!(resample.invocation_indices, vec![10]);
        let rate_json = portable_test_track_json(&with_resample, 12);
        assert!(rate_json.contains("\"invocation_indices\":[11]"));
        assert!(rate_json.contains("\"invocation_indices\":[10]"));
    }

    fn portable_test_track_json(evidence: &TrackExecutionEvidence, invocation_count: usize) -> String {
        let invocations = (0..invocation_count)
            .map(|_| PortableInvocation::from(&test_invocation(Some(ProcessExit::Code(0)))))
            .collect();
        serde_json::to_string(&PortableTrackExecutionRecord {
            track_id: "track-1:source-1".to_string(),
            outcome: "success".to_string(),
            execution_evidence: evidence.clone(),
            invocations,
        })
        .expect("serialize portable track evidence")
    }

    fn test_invocation(exit: Option<ProcessExit>) -> CommandRecord {
        CommandRecord {
            description: Some("test invocation".to_string()),
            binary: super::super::tool::ToolBinary::Ffmpeg,
            sanitized_args: Vec::new(),
            cwd: None,
            environment_policy: Default::default(),
            environment: Default::default(),
            env_keys: Vec::new(),
            exit,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn portable_invocation_preserves_control_characters_as_json_escaped_data() {
        let mut command = test_invocation(Some(ProcessExit::Code(0)));
        command.sanitized_args = vec!["arg\nnext".to_string()];

        let portable = PortableInvocation::from(&command);
        assert_eq!(portable.argv, vec!["arg\nnext".to_string()]);
        let json = serde_json::to_string(&portable).expect("serialize portable invocation");
        assert!(json.contains("arg\\nnext"), "{json}");
    }

    fn float_source_terminal_request(
        source_depth: tonepoet_pipeline::PcmBitDepth,
        target_format: tonepoet_pipeline::AudioFormat,
    ) -> PlanRequest {
        let mut settings = tonepoet_pipeline::PipelineSettings::default();
        settings.target_format = target_format.clone();
        settings.target_sample_rate = tonepoet_pipeline::RateTarget::Source;
        settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Source;
        settings.dither_type = tonepoet_pipeline::DitherType::None;
        settings.dither_explicit = false;
        settings.metadata.transfer_tags = false;
        settings.metadata.preserve_artwork = false;
        PlanRequest {
            input_path: PathBuf::from("input.wav"),
            output_path: PathBuf::from(format!("output.{}", target_format.extension())),
            source: tonepoet_pipeline::SourceInfo {
                format: tonepoet_pipeline::AudioFormat::Wav,
                codec: tonepoet_pipeline::AudioCodec::PcmFloat,
                sample_rate_hz: Some(192_000),
                bit_depth: Some(source_depth),
                true_source_depth: Some(source_depth),
                source_representation: tonepoet_pipeline::SourceRepresentationKind::Pcm,
                sample_kind: Some(tonepoet_pipeline::SampleKind::Float),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: tonepoet_pipeline::PlanScope::track("execution-evidence-terminal-test"),
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    fn sox_preterminal_ffmpeg_package_request() -> PlanRequest {
        let mut request = float_source_terminal_request(
            tonepoet_pipeline::PcmBitDepth::Float64,
            tonepoet_pipeline::AudioFormat::Alac,
        );
        request.settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(
            tonepoet_pipeline::PcmBitDepth::Int24,
        );
        request.settings.dither_type = tonepoet_pipeline::DitherType::SlopedTpdf;
        request.settings.dither_explicit = true;
        request
    }

    fn command_indices_for_sox_ffmpeg_compound(request: &PlanRequest) -> (usize, usize, usize) {
        let plan = tonepoet_pipeline::plan_conversion(request)
            .expect("SoX-preterminal + FFmpeg-package route must plan");
        let tonepoet_pipeline::PlanAction::Execute { commands, .. } = plan.action else {
            panic!("compound terminal route must execute")
        };
        let package_index = commands
            .iter()
            .position(|command| command.tool == tonepoet_pipeline::ToolIdentifier::Ffmpeg)
            .expect("FFmpeg package command");
        let package_input = commands[package_index].input.as_path();
        let preterminal_index = commands
            .iter()
            .enumerate()
            .find_map(|(index, command)| {
                (index < package_index
                    && command.tool == tonepoet_pipeline::ToolIdentifier::Sox
                    && command.output.as_path() == package_input)
                    .then_some(index)
            })
            .expect("SoX preterminal feeding FFmpeg package");
        (preterminal_index, package_index, commands.len())
    }

    #[test]
    fn compound_terminal_links_preterminal_and_package_invocations() {
        let request = sox_preterminal_ffmpeg_package_request();
        let (preterminal_index, package_index, command_count) =
            command_indices_for_sox_ffmpeg_compound(&request);
        let invocations = (0..command_count)
            .map(|_| test_invocation(Some(ProcessExit::Code(0))))
            .collect::<Vec<_>>();

        let evidence = completed_plan_evidence(&request, &invocations)
            .expect("completed compound terminal evidence");
        let terminal = evidence
            .operations
            .iter()
            .find(|operation| {
                matches!(
                    &operation.backend,
                    ExecutionBackend::Composite { description }
                        if description == "SoX terminal sample realization + FFmpeg packaging"
                )
            })
            .expect("compound terminal operation");
        assert_eq!(
            terminal.invocation_indices,
            vec![preterminal_index, package_index],
        );

        let portable = portable_test_track_json(&evidence, command_count);
        assert!(portable.contains(&format!(
            "\"invocation_indices\":[{preterminal_index},{package_index}]"
        )));
    }

    #[test]
    fn compound_terminal_package_failure_keeps_both_links_and_failed_status() {
        let request = sox_preterminal_ffmpeg_package_request();
        let (preterminal_index, package_index, command_count) =
            command_indices_for_sox_ffmpeg_compound(&request);
        let mut invocations = (0..command_count)
            .map(|_| test_invocation(Some(ProcessExit::Code(0))))
            .collect::<Vec<_>>();
        invocations[package_index].exit = Some(ProcessExit::Code(1));

        let evidence = attempt_plan_evidence(&request, &invocations)
            .expect("failed compound terminal attempt evidence");
        let terminal = evidence
            .operations
            .iter()
            .find(|operation| {
                matches!(
                    &operation.backend,
                    ExecutionBackend::Composite { description }
                        if description == "SoX terminal sample realization + FFmpeg packaging"
                )
            })
            .expect("failed compound terminal operation");
        assert_eq!(
            terminal.invocation_indices,
            vec![preterminal_index, package_index],
        );
        assert_eq!(terminal.status, OperationStatus::Failed);
        assert_eq!(terminal.lineage, OperationLineage::DiscardedAttempt);
    }

    #[test]
    fn undithered_wavpack_hybrid_separates_quantization_owner_from_dither_owner() {
        let mut request = float_source_terminal_request(
            tonepoet_pipeline::PcmBitDepth::Float32,
            tonepoet_pipeline::AudioFormat::WavPack,
        );
        request.settings.wavpack.hybrid = true;

        let plan = tonepoet_pipeline::plan_conversion(&request)
            .expect("Float32 Source WavPack-hybrid route must plan");
        let tonepoet_pipeline::PlanAction::Execute { commands, .. } = plan.action else {
            panic!("WavPack-hybrid route must execute")
        };
        let invocations = (0..commands.len())
            .map(|_| test_invocation(Some(ProcessExit::Code(0))))
            .collect::<Vec<_>>();
        let evidence = completed_plan_evidence(&request, &invocations)
            .expect("undithered WavPack-hybrid evidence");
        let rendered = render_track_processing(&evidence).join("\n");
        assert!(rendered.contains("Terminal sample/quantization owner: SoX"));
        assert!(rendered.contains("Dither owner: none"));
        assert!(rendered.contains("Effective terminal dither: None"));
        assert!(!rendered.contains("Dither/quantization owner"));

        let portable = portable_test_track_json(&evidence, commands.len());
        assert!(portable.contains("Terminal sample/quantization owner"));
        assert!(portable.contains("Dither owner"));
        assert!(!portable.contains("Dither/quantization owner"));
    }

    #[test]
    fn partial_attempt_keeps_completed_and_failed_typed_operations_only() {
        let mut evidence = TrackExecutionEvidence::default();
        for (id, index) in [("first", 0_usize), ("second", 1), ("never", 2)] {
            let mut operation = OperationRecord::completed(
                id,
                format!("{id}_kind"),
                format!("{id} operation"),
                ExecutionBackend::external("ffmpeg"),
            );
            operation.invocation_indices.push(index);
            evidence.operations.push(operation);
        }
        let mut partial = OperationRecord::completed(
            "partial",
            "partial_kind",
            "partial multi-command operation",
            ExecutionBackend::external("ffmpeg"),
        );
        partial.invocation_indices.extend([0, 2]);
        evidence.operations.push(partial);

        let invocations = vec![
            test_invocation(Some(ProcessExit::Code(0))),
            test_invocation(Some(ProcessExit::Code(1))),
        ];

        retain_attempted_execution_evidence(&mut evidence, &invocations);

        assert_eq!(evidence.operations.len(), 3);
        assert_eq!(evidence.operations[0].status, OperationStatus::Completed);
        assert_eq!(evidence.operations[1].status, OperationStatus::Failed);
        assert_eq!(
            evidence
                .operations
                .iter()
                .find(|operation| operation.id == "partial")
                .expect("partly attempted operation")
                .status,
            OperationStatus::Incomplete,
        );
        assert!(evidence
            .operations
            .iter()
            .all(|operation| operation.lineage == OperationLineage::DiscardedAttempt));
        assert!(evidence.operations.iter().all(|operation| operation.id != "never"));
    }

    fn terminal_realization_fixture(
        kind: tonepoet_pipeline::PcmTerminalRealizationKind,
        selected_tool: tonepoet_pipeline::ToolIdentifier,
        target_format: tonepoet_pipeline::AudioFormat,
        dither: Option<tonepoet_pipeline::DitherType>,
        dither_owner: tonepoet_pipeline::PcmTerminalDitherOwner,
        ssrc_dither: Option<tonepoet_pipeline::plugins::ResolvedSsrcDither>,
    ) -> tonepoet_pipeline::SelectedPcmTerminalRealization {
        tonepoet_pipeline::SelectedPcmTerminalRealization {
            kind,
            selected_tool,
            input_precision: tonepoet_pipeline::StoragePrecision::Pcm(
                tonepoet_pipeline::PcmBitDepth::Float64,
            ),
            input_value_domain: tonepoet_pipeline::ValueDomain::FiniteFloating,
            target_format,
            target_rate_hz: Some(44_100),
            target_bit_depth: tonepoet_pipeline::PcmBitDepth::Int24,
            wavpack_hybrid: false,
            effective_dither: dither,
            ssrc_dither,
            dither_owner,
        }
    }

    fn projected_terminal_record(
        backend: &str,
        realization: &tonepoet_pipeline::SelectedPcmTerminalRealization,
    ) -> OperationRecord {
        let mut record = OperationRecord::completed(
            "terminal",
            "encode_pcm",
            "PCM encoding / terminal realization",
            ExecutionBackend::external(backend),
        );
        apply_pcm_terminal_realization(&mut record, realization);
        record
    }

    #[test]
    fn terminal_realization_evidence_preserves_physical_quantization_truth() {
        use tonepoet_pipeline::PcmTerminalDitherOwner as Owner;
        use tonepoet_pipeline::PcmTerminalRealizationKind as Kind;

        let direct_ffmpeg = projected_terminal_record(
            "ffmpeg",
            &terminal_realization_fixture(
                Kind::FfmpegDirect,
                tonepoet_pipeline::ToolIdentifier::Ffmpeg,
                tonepoet_pipeline::AudioFormat::Wav,
                Some(tonepoet_pipeline::DitherType::Tpdf),
                Owner::SelectedTerminal,
                None,
            ),
        );
        assert!(matches!(
            direct_ffmpeg.backend,
            ExecutionBackend::External { ref tool, .. } if tool == "ffmpeg"
        ));
        let direct_text = render_track_processing(&TrackExecutionEvidence {
            operations: vec![direct_ffmpeg],
            ..TrackExecutionEvidence::default()
        })
        .join("\n");
        assert!(direct_text.contains("Terminal input precision: Float64"));
        assert!(direct_text.contains("Terminal output precision: Int24"));
        assert!(direct_text.contains("Terminal sample/quantization owner: FFmpeg"));
        assert!(direct_text.contains("Dither owner: selected terminal backend"));

        let compound = projected_terminal_record(
            "ffmpeg",
            &terminal_realization_fixture(
                Kind::SoxPreterminalFfmpegPackage,
                tonepoet_pipeline::ToolIdentifier::Ffmpeg,
                tonepoet_pipeline::AudioFormat::Alac,
                Some(tonepoet_pipeline::DitherType::SlopedTpdf),
                Owner::SoxPreterminal,
                None,
            ),
        );
        let compound_evidence = TrackExecutionEvidence {
            operations: vec![compound],
            ..TrackExecutionEvidence::default()
        };
        let compound_text = render_track_processing(&compound_evidence).join("\n");
        assert!(compound_text.contains("SoX terminal sample realization + FFmpeg packaging"));
        assert!(compound_text.contains("Terminal sample/quantization owner: SoX"));
        assert!(compound_text.contains("Dither owner: SoX preterminal"));
        assert!(compound_text.contains("Effective terminal dither: SlopedTpdf"));
        let compound_portable = PortableTrackExecutionRecord {
            track_id: "track-1:source-1".to_string(),
            outcome: "success".to_string(),
            execution_evidence: compound_evidence.clone(),
            invocations: Vec::new(),
        };
        let compound_json = serde_json::to_string(&compound_portable)
            .expect("serialize terminal portable evidence");
        assert!(compound_json.contains("SoX preterminal"));
        assert!(compound_json.contains("Float64"));
        assert!(compound_json.contains("Int24"));

        let ssrc = projected_terminal_record(
            "ssrc",
            &terminal_realization_fixture(
                Kind::SsrcDirectWav,
                tonepoet_pipeline::ToolIdentifier::Ssrc,
                tonepoet_pipeline::AudioFormat::Wav,
                Some(tonepoet_pipeline::DitherType::Tpdf),
                Owner::SsrcResampler,
                Some(tonepoet_pipeline::plugins::ResolvedSsrcDither {
                    requested_global: tonepoet_pipeline::DitherType::Tpdf,
                    dither_id: Some(1),
                    pdf_type: Some(tonepoet_pipeline::SsrcPdfType::Triangular),
                    origin: tonepoet_pipeline::plugins::SsrcDitherOrigin::GlobalExact,
                    availability: tonepoet_pipeline::plugins::SsrcDitherAvailability::Active,
                }),
            ),
        );
        let ssrc_text = render_track_processing(&TrackExecutionEvidence {
            operations: vec![ssrc],
            ..TrackExecutionEvidence::default()
        })
        .join("\n");
        assert!(ssrc_text.contains("Dither owner: SSRC resampler"));
        assert!(ssrc_text.contains("SSRC native dither id: 1"));
        assert!(ssrc_text.contains("SSRC PDF: Triangular"));

        let no_dither = projected_terminal_record(
            "sox",
            &terminal_realization_fixture(
                Kind::SoxDirect,
                tonepoet_pipeline::ToolIdentifier::Sox,
                tonepoet_pipeline::AudioFormat::Wav,
                None,
                Owner::None,
                None,
            ),
        );
        let no_dither_text = render_track_processing(&TrackExecutionEvidence {
            operations: vec![no_dither],
            ..TrackExecutionEvidence::default()
        })
        .join("\n");
        assert!(no_dither_text.contains("Terminal sample/quantization owner: SoX"));
        assert!(no_dither_text.contains("Dither owner: none"));
        assert!(no_dither_text.contains("Effective terminal dither: None"));
        assert!(!no_dither_text.contains("Dither/quantization owner"));
    }
}

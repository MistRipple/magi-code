use crate::{
    WorkerExecutionCheckpointCursor, WorkerExecutionIntent, WorkerExecutionStepKind,
    WorkerExecutionTrace, WorkerExecutorFailureDetail, WorkerExecutorRequest, WorkerStage,
};
use magi_core::{SessionId, VerificationStatus, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::fmt;

pub(super) const LOCAL_PROCESS_PROTOCOL_VERSION: &str = "worker-shadow-local-process-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionMode {
    ShadowLoopback,
    #[default]
    LocalProcess,
}

impl WorkerExecutionMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ShadowLoopback => "shadow-loopback",
            Self::LocalProcess => "local-process",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessExecutorAffinity {
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub strict_session: bool,
    pub strict_workspace: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessExecutorStageMatrix {
    pub execute: bool,
    pub review: bool,
    pub verify: bool,
    pub repair: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LocalProcessExecutorProcessModel {
    ShadowLoopback,
    #[default]
    OneShotSubprocess,
    PersistentProcess,
}

impl LocalProcessExecutorProcessModel {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ShadowLoopback => "shadow-loopback",
            Self::OneShotSubprocess => "one-shot-subprocess",
            Self::PersistentProcess => "persistent-process",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessExecutorDescriptor {
    pub process_model: LocalProcessExecutorProcessModel,
    pub reuse_scope: WorkerExecutionBindingScope,
    pub parallelism_scope: WorkerExecutionParallelismScope,
    pub lease_state: WorkerExecutionLeaseState,
    pub binding_lifecycle: WorkerExecutionBindingLifecycle,
    pub process_lifecycle: WorkerExecutionProcessLifecycle,
    pub max_parallelism: usize,
    pub executor_instance_id: Option<String>,
    pub executor_lease_id: Option<String>,
}

impl Default for LocalProcessExecutorDescriptor {
    fn default() -> Self {
        Self {
            process_model: LocalProcessExecutorProcessModel::OneShotSubprocess,
            reuse_scope: WorkerExecutionBindingScope::None,
            parallelism_scope: WorkerExecutionParallelismScope::Executor,
            lease_state: WorkerExecutionLeaseState::None,
            binding_lifecycle: WorkerExecutionBindingLifecycle::None,
            process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
            max_parallelism: 1,
            executor_instance_id: None,
            executor_lease_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionReusePolicy {
    #[default]
    NotRequired,
    Preferred,
    Required,
}

impl WorkerExecutionReusePolicy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotRequired => "not-required",
            Self::Preferred => "preferred",
            Self::Required => "required",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionBindingScope {
    #[default]
    None,
    Session,
    Workspace,
}

impl WorkerExecutionBindingScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Session => "session",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionLeaseState {
    #[default]
    None,
    Requested,
    Active,
    Released,
    Expired,
}

impl WorkerExecutionLeaseState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Requested => "requested",
            Self::Active => "active",
            Self::Released => "released",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionBindingLifecycle {
    #[default]
    None,
    Requested,
    Bound,
    Released,
}

impl WorkerExecutionBindingLifecycle {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Requested => "requested",
            Self::Bound => "bound",
            Self::Released => "released",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionProcessLifecycle {
    #[default]
    OneShot,
    Reusable,
    Persistent,
}

impl WorkerExecutionProcessLifecycle {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Reusable => "reusable",
            Self::Persistent => "persistent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkerExecutionParallelismScope {
    #[default]
    Executor,
    Session,
    Workspace,
}

impl WorkerExecutionParallelismScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Executor => "executor",
            Self::Session => "session",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutionProfile {
    pub reuse_policy: WorkerExecutionReusePolicy,
    pub binding_scope: WorkerExecutionBindingScope,
    pub lease_state: WorkerExecutionLeaseState,
    pub binding_lifecycle: WorkerExecutionBindingLifecycle,
    pub process_lifecycle: WorkerExecutionProcessLifecycle,
    pub requested_process_model: Option<LocalProcessExecutorProcessModel>,
    pub requested_parallelism: usize,
}

impl Default for WorkerExecutionProfile {
    fn default() -> Self {
        Self {
            reuse_policy: WorkerExecutionReusePolicy::NotRequired,
            binding_scope: WorkerExecutionBindingScope::None,
            lease_state: WorkerExecutionLeaseState::None,
            binding_lifecycle: WorkerExecutionBindingLifecycle::None,
            process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
            requested_process_model: None,
            requested_parallelism: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerExecutorFailureLayer {
    Transport,
    Protocol,
    RemoteBusiness,
}

impl WorkerExecutorFailureLayer {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Protocol => "protocol",
            Self::RemoteBusiness => "remote-business",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutorFailure {
    pub layer: WorkerExecutorFailureLayer,
    pub message: String,
    pub detail: Option<WorkerExecutorFailureDetail>,
}

impl WorkerExecutorFailure {
    pub(super) fn transport(message: impl Into<String>) -> Self {
        Self {
            layer: WorkerExecutorFailureLayer::Transport,
            message: message.into(),
            detail: None,
        }
    }

    pub(super) fn protocol(message: impl Into<String>) -> Self {
        Self {
            layer: WorkerExecutorFailureLayer::Protocol,
            message: message.into(),
            detail: None,
        }
    }

    pub(crate) fn remote_business(message: impl Into<String>) -> Self {
        Self {
            layer: WorkerExecutorFailureLayer::RemoteBusiness,
            message: message.into(),
            detail: None,
        }
    }

    pub(crate) fn remote_business_with_detail(
        message: impl Into<String>,
        detail: WorkerExecutorFailureDetail,
    ) -> Self {
        Self {
            layer: WorkerExecutorFailureLayer::RemoteBusiness,
            message: message.into(),
            detail: Some(detail),
        }
    }
}

impl fmt::Display for WorkerExecutorFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.layer.label(), self.message)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessExecutorCapability {
    pub executor_id: String,
    pub executor_version: String,
    pub execution_mode: WorkerExecutionMode,
    pub protocol_version: String,
    pub supports_probe: bool,
    pub supports_execute: bool,
    pub supports_review: bool,
    pub supports_verify: bool,
    pub supports_repair: bool,
    pub affinity: LocalProcessExecutorAffinity,
    pub stage_matrix: LocalProcessExecutorStageMatrix,
    pub descriptor: LocalProcessExecutorDescriptor,
    pub supported_step_kinds: Vec<WorkerExecutionStepKind>,
}

impl LocalProcessExecutorCapability {
    fn lease_state_satisfies(
        requested: WorkerExecutionLeaseState,
        effective: WorkerExecutionLeaseState,
    ) -> bool {
        match requested {
            WorkerExecutionLeaseState::None => true,
            WorkerExecutionLeaseState::Requested => matches!(
                effective,
                WorkerExecutionLeaseState::Requested | WorkerExecutionLeaseState::Active
            ),
            WorkerExecutionLeaseState::Active => effective == WorkerExecutionLeaseState::Active,
            WorkerExecutionLeaseState::Released => effective == WorkerExecutionLeaseState::Released,
            WorkerExecutionLeaseState::Expired => effective == WorkerExecutionLeaseState::Expired,
        }
    }

    fn binding_lifecycle_satisfies(
        requested: WorkerExecutionBindingLifecycle,
        effective: WorkerExecutionBindingLifecycle,
    ) -> bool {
        match requested {
            WorkerExecutionBindingLifecycle::None => true,
            WorkerExecutionBindingLifecycle::Requested => matches!(
                effective,
                WorkerExecutionBindingLifecycle::Requested | WorkerExecutionBindingLifecycle::Bound
            ),
            WorkerExecutionBindingLifecycle::Bound => {
                effective == WorkerExecutionBindingLifecycle::Bound
            }
            WorkerExecutionBindingLifecycle::Released => {
                effective == WorkerExecutionBindingLifecycle::Released
            }
        }
    }

    fn process_lifecycle_satisfies(
        requested: WorkerExecutionProcessLifecycle,
        effective: WorkerExecutionProcessLifecycle,
    ) -> bool {
        match requested {
            WorkerExecutionProcessLifecycle::OneShot => {
                effective == WorkerExecutionProcessLifecycle::OneShot
            }
            WorkerExecutionProcessLifecycle::Reusable => matches!(
                effective,
                WorkerExecutionProcessLifecycle::Reusable
                    | WorkerExecutionProcessLifecycle::Persistent
            ),
            WorkerExecutionProcessLifecycle::Persistent => {
                effective == WorkerExecutionProcessLifecycle::Persistent
            }
        }
    }

    pub(super) fn failure_detail(
        &self,
        profile: Option<&WorkerExecutionProfile>,
        required_step_kinds: Vec<WorkerExecutionStepKind>,
        missing_step_kinds: Vec<WorkerExecutionStepKind>,
    ) -> WorkerExecutorFailureDetail {
        WorkerExecutorFailureDetail {
            executor_id: Some(self.executor_id.clone()),
            executor_version: Some(self.executor_version.clone()),
            executor_instance_id: self.descriptor.executor_instance_id.clone(),
            executor_lease_id: self.descriptor.executor_lease_id.clone(),
            requested_execution_profile: profile.cloned(),
            requested_lease_state: profile.map(|value| value.lease_state),
            requested_binding_lifecycle: profile.map(|value| value.binding_lifecycle),
            requested_process_lifecycle: profile.map(|value| value.process_lifecycle),
            effective_process_model: Some(self.descriptor.process_model),
            effective_lease_state: Some(self.descriptor.lease_state),
            effective_binding_lifecycle: Some(self.descriptor.binding_lifecycle),
            effective_process_lifecycle: Some(self.descriptor.process_lifecycle),
            effective_reuse_scope: Some(self.descriptor.reuse_scope),
            effective_parallelism_scope: Some(self.descriptor.parallelism_scope),
            required_step_kinds,
            supported_step_kinds: self.supported_step_kinds.clone(),
            missing_step_kinds,
        }
    }

    pub fn supports_stage(&self, stage: WorkerStage) -> bool {
        match stage {
            WorkerStage::Execute => self.supports_execute && self.stage_matrix.execute,
            WorkerStage::Review => self.supports_review && self.stage_matrix.review,
            WorkerStage::Verify => self.supports_verify && self.stage_matrix.verify,
            WorkerStage::Repair | WorkerStage::Finish => {
                self.supports_repair && self.stage_matrix.repair
            }
        }
    }

    pub fn supports_context(
        &self,
        session_id: &Option<SessionId>,
        workspace_id: &Option<WorkspaceId>,
    ) -> Result<(), WorkerExecutorFailure> {
        if self.affinity.strict_session && self.affinity.session_id.as_ref() != session_id.as_ref()
        {
            return Err(WorkerExecutorFailure::remote_business(format!(
                "executor {} {} session affinity mismatch: expected {}, got {}",
                self.executor_id,
                self.executor_version,
                self.affinity
                    .session_id
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string()),
                session_id
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            )));
        }
        if self.affinity.strict_workspace
            && self.affinity.workspace_id.as_ref() != workspace_id.as_ref()
        {
            return Err(WorkerExecutorFailure::remote_business(format!(
                "executor {} {} workspace affinity mismatch: expected {}, got {}",
                self.executor_id,
                self.executor_version,
                self.affinity
                    .workspace_id
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string()),
                workspace_id
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            )));
        }
        Ok(())
    }

    pub fn supports_profile(
        &self,
        profile: &WorkerExecutionProfile,
    ) -> Result<(), WorkerExecutorFailure> {
        if matches!(profile.reuse_policy, WorkerExecutionReusePolicy::Required)
            && self.descriptor.reuse_scope == WorkerExecutionBindingScope::None
        {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} does not support reusable session binding",
                    self.executor_id, self.executor_version
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        if profile.binding_scope != WorkerExecutionBindingScope::None
            && self.descriptor.reuse_scope != WorkerExecutionBindingScope::None
            && profile.binding_scope != self.descriptor.reuse_scope
        {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} binding scope mismatch: requested {}, supported {}",
                    self.executor_id,
                    self.executor_version,
                    profile.binding_scope.label(),
                    self.descriptor.reuse_scope.label()
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        if let Some(process_model) = profile.requested_process_model {
            if self.descriptor.process_model != process_model {
                return Err(WorkerExecutorFailure::remote_business_with_detail(
                    format!(
                        "executor {} {} process model mismatch: expected {}, got {}",
                        self.executor_id,
                        self.executor_version,
                        process_model.label(),
                        self.descriptor.process_model.label()
                    ),
                    self.failure_detail(Some(profile), Vec::new(), Vec::new()),
                ));
            }
        }
        if profile.requested_parallelism > self.descriptor.max_parallelism {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} parallelism mismatch: requested {}, supported {}",
                    self.executor_id,
                    self.executor_version,
                    profile.requested_parallelism,
                    self.descriptor.max_parallelism
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        if self.descriptor.lease_state != WorkerExecutionLeaseState::None
            && profile.lease_state != WorkerExecutionLeaseState::None
            && !Self::lease_state_satisfies(profile.lease_state, self.descriptor.lease_state)
        {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} lease state mismatch: requested {}, supported {}",
                    self.executor_id,
                    self.executor_version,
                    profile.lease_state.label(),
                    self.descriptor.lease_state.label()
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        if self.descriptor.binding_lifecycle != WorkerExecutionBindingLifecycle::None
            && profile.binding_lifecycle != WorkerExecutionBindingLifecycle::None
            && !Self::binding_lifecycle_satisfies(
                profile.binding_lifecycle,
                self.descriptor.binding_lifecycle,
            )
        {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} binding lifecycle mismatch: requested {}, supported {}",
                    self.executor_id,
                    self.executor_version,
                    profile.binding_lifecycle.label(),
                    self.descriptor.binding_lifecycle.label()
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        if !Self::process_lifecycle_satisfies(
            profile.process_lifecycle,
            self.descriptor.process_lifecycle,
        ) {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} process lifecycle mismatch: requested {}, supported {}",
                    self.executor_id,
                    self.executor_version,
                    profile.process_lifecycle.label(),
                    self.descriptor.process_lifecycle.label()
                ),
                self.failure_detail(Some(profile), Vec::new(), Vec::new()),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalProcessExecutorHealthStatus {
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessExecutorHealth {
    pub status: LocalProcessExecutorHealthStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessProbeRequest {
    pub executor_request: Option<WorkerExecutorRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProcessProbeResponse {
    pub capability: LocalProcessExecutorCapability,
    pub health: LocalProcessExecutorHealth,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessExecutionRequest {
    pub executor_request: WorkerExecutorRequest,
    pub intent: WorkerExecutionIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessExecutionResponse {
    pub trace: WorkerExecutionTrace,
    pub next_step_index: usize,
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessReviewRequest {
    pub executor_request: WorkerExecutorRequest,
    pub intent: WorkerExecutionIntent,
    pub prior_trace: Option<WorkerExecutionTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessReviewResponse {
    pub trace: WorkerExecutionTrace,
    pub review_summary: String,
    pub next_step_index: usize,
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessVerifyRequest {
    pub executor_request: WorkerExecutorRequest,
    pub intent: WorkerExecutionIntent,
    pub prior_trace: Option<WorkerExecutionTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessVerifyResponse {
    pub trace: WorkerExecutionTrace,
    pub verification_status: VerificationStatus,
    pub verify_summary: String,
    pub next_step_index: usize,
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessRepairRequest {
    pub executor_request: WorkerExecutorRequest,
    pub intent: WorkerExecutionIntent,
    pub prior_trace: Option<WorkerExecutionTrace>,
    pub repair_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_cursor: Option<WorkerExecutionCheckpointCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessRepairResponse {
    pub trace: WorkerExecutionTrace,
    pub repair_summary: String,
    pub next_step_index: usize,
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LocalProcessProtocolRequestKind {
    Probe(LocalProcessProbeRequest),
    Execute(LocalProcessExecutionRequest),
    Review(LocalProcessReviewRequest),
    Verify(LocalProcessVerifyRequest),
    Repair(LocalProcessRepairRequest),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessProtocolRequest {
    pub request_id: String,
    pub kind: LocalProcessProtocolRequestKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LocalProcessProtocolResponseKind {
    Probe(LocalProcessProbeResponse),
    Execute(LocalProcessExecutionResponse),
    Review(LocalProcessReviewResponse),
    Verify(LocalProcessVerifyResponse),
    Repair(LocalProcessRepairResponse),
    Error(WorkerExecutorFailure),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProcessProtocolResponse {
    pub request_id: String,
    pub kind: LocalProcessProtocolResponseKind,
}

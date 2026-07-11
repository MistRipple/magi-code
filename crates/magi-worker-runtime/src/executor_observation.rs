use crate::{
    DeterministicWorkerExecutor, EventCategory, EventContext, LocalProcessExecutorHealthStatus,
    LocalProcessExecutorProcessModel, WorkerExecutionBindingLifecycle, WorkerExecutionBindingScope,
    WorkerExecutionIntent, WorkerExecutionLeaseState, WorkerExecutionMode,
    WorkerExecutionParallelismScope, WorkerExecutionProcessLifecycle, WorkerExecutionReusePolicy,
    WorkerExecutionStepKind, WorkerExecutor, WorkerExecutorFailure, WorkerExecutorFailureLayer,
    WorkerExecutorKind, WorkerExecutorProbe, WorkerExecutorRequest, WorkerRuntime, WorkerStage,
};
use magi_core::{TaskId, UtcMillis, WorkerId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerExecutorObservationStatus {
    Ready,
    Degraded,
    Unavailable,
}

impl WorkerExecutorObservationStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

const EXECUTOR_HEALTH_READY_DETAIL: &str = "executor ready";
const EXECUTOR_HEALTH_DEGRADED_DETAIL: &str = "executor degraded";
const EXECUTOR_HEALTH_UNAVAILABLE_DETAIL: &str = "executor unavailable";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutorObservation {
    pub worker_id: WorkerId,
    pub task_id: Option<TaskId>,
    pub requested_stage: Option<WorkerStage>,
    pub request_id: Option<String>,
    pub request_source: Option<String>,
    pub requested_reuse_policy: Option<WorkerExecutionReusePolicy>,
    pub requested_binding_scope: Option<WorkerExecutionBindingScope>,
    pub requested_lease_state: Option<WorkerExecutionLeaseState>,
    pub requested_binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    pub requested_process_lifecycle: Option<WorkerExecutionProcessLifecycle>,
    pub requested_process_model: Option<LocalProcessExecutorProcessModel>,
    pub requested_parallelism: Option<usize>,
    pub requested_step_kinds: Vec<WorkerExecutionStepKind>,
    pub executor_kind: WorkerExecutorKind,
    pub observation_status: WorkerExecutorObservationStatus,
    pub executor_id: Option<String>,
    pub executor_version: Option<String>,
    pub executor_instance_id: Option<String>,
    pub executor_lease_id: Option<String>,
    pub execution_mode: Option<WorkerExecutionMode>,
    pub protocol_version: Option<String>,
    pub process_model: Option<LocalProcessExecutorProcessModel>,
    pub lease_state: Option<WorkerExecutionLeaseState>,
    pub binding_lifecycle: Option<WorkerExecutionBindingLifecycle>,
    pub process_lifecycle: Option<WorkerExecutionProcessLifecycle>,
    pub reuse_scope: Option<WorkerExecutionBindingScope>,
    pub parallelism_scope: Option<WorkerExecutionParallelismScope>,
    pub max_parallelism: Option<usize>,
    pub strict_session_affinity: Option<bool>,
    pub strict_workspace_affinity: Option<bool>,
    pub supported_step_kinds: Vec<WorkerExecutionStepKind>,
    pub health_status: Option<crate::LocalProcessExecutorHealthStatus>,
    pub health_detail: Option<String>,
    pub failure_layer: Option<WorkerExecutorFailureLayer>,
    pub failure_message: Option<String>,
    pub observed_at: UtcMillis,
}

impl WorkerRuntime {
    pub fn default_execution_intent(
        &self,
        worker_id: &WorkerId,
        task_id: &TaskId,
    ) -> WorkerExecutionIntent {
        DeterministicWorkerExecutor::default_intent(worker_id.clone(), task_id.clone())
    }

    pub fn executor(&self) -> Arc<dyn WorkerExecutor> {
        Arc::clone(&self.executor)
    }

    pub fn executor_kind(&self) -> WorkerExecutorKind {
        self.executor.executor_kind()
    }

    pub fn executor_probe(&self) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        self.executor.probe()
    }

    pub fn executor_probe_for(
        &self,
        request: Option<&WorkerExecutorRequest>,
    ) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        self.executor.probe_for_request(request)
    }

    pub fn observe_executor_probe(
        &self,
        worker_id: &WorkerId,
        task_id: Option<TaskId>,
        requested_stage: Option<WorkerStage>,
        request: Option<&WorkerExecutorRequest>,
        probe_result: &Result<WorkerExecutorProbe, WorkerExecutorFailure>,
    ) -> WorkerExecutorObservation {
        let record = match probe_result {
            Ok(probe) => self.observation_from_probe(
                worker_id,
                task_id.clone(),
                requested_stage,
                request,
                probe,
            ),
            Err(error) => self.observation_from_error(
                worker_id,
                task_id.clone(),
                requested_stage,
                request,
                error,
            ),
        };
        self.executor_observations
            .write()
            .expect("worker executor observation write lock poisoned")
            .push(record.clone());
        if let Some(task_id) = task_id.clone() {
            let worker_id = worker_id.clone();
            let requested_stage = requested_stage.unwrap_or(WorkerStage::Execute);
            let observed_at = record.observed_at;
            let executor_lease_id = record.executor_lease_id.clone();
            let binding_lifecycle = record
                .binding_lifecycle
                .or(record.requested_binding_lifecycle);
            self.upsert_branch_snapshot(&task_id, |existing| crate::WorkerRuntimeBranchSnapshot {
                task_id: task_id.clone(),
                worker_id: worker_id.clone(),
                stage: requested_stage,
                lease_id: executor_lease_id
                    .clone()
                    .or_else(|| existing.and_then(|snapshot| snapshot.lease_id.clone())),
                execution_intent_ref: existing
                    .and_then(|snapshot| snapshot.execution_intent_ref.clone()),
                binding_lifecycle: binding_lifecycle
                    .or_else(|| existing.and_then(|snapshot| snapshot.binding_lifecycle)),
                checkpoint_cursor: existing
                    .and_then(|snapshot| snapshot.checkpoint_cursor.clone())
                    .or({
                        Some(crate::WorkerExecutionCheckpointCursor {
                            checkpoint_stage: requested_stage,
                            next_step_index: 0,
                            checkpoint_at: observed_at,
                            resume_mode: crate::WorkerCheckpointResumeMode::StageRestart,
                            resume_token: None,
                        })
                    }),
            });
        }
        self.publish_with_category(
            "worker.executor.observed",
            EventCategory::Audit,
            EventContext {
                task_id: task_id.clone(),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": task_id.as_ref().map(ToString::to_string),
                "requested_stage": record.requested_stage.map(|stage| stage.label().to_string()),
                "request_id": record.request_id,
                "request_source": record.request_source,
                "requested_reuse_policy": record
                    .requested_reuse_policy
                    .map(|policy| policy.label().to_string()),
                "requested_binding_scope": record
                    .requested_binding_scope
                    .map(|scope| scope.label().to_string()),
                "requested_lease_state": record
                    .requested_lease_state
                    .map(|state| state.label().to_string()),
                "requested_binding_lifecycle": record
                    .requested_binding_lifecycle
                    .map(|state| state.label().to_string()),
                "requested_process_lifecycle": record
                    .requested_process_lifecycle
                    .map(|state| state.label().to_string()),
                "requested_process_model": record
                    .requested_process_model
                    .map(|mode| mode.label().to_string()),
                "requested_parallelism": record.requested_parallelism,
                "requested_step_kinds": record
                    .requested_step_kinds
                    .iter()
                    .map(WorkerExecutionStepKind::label)
                    .collect::<Vec<_>>(),
                "executor_kind": format!("{:?}", record.executor_kind),
                "observation_status": record.observation_status.label(),
                "executor_id": record.executor_id,
                "executor_version": record.executor_version,
                "executor_instance_id": record.executor_instance_id,
                "executor_lease_id": record.executor_lease_id,
                "execution_mode": record.execution_mode.map(|mode| mode.label().to_string()),
                "protocol_version": record.protocol_version,
                "process_model": record.process_model.map(|mode| mode.label().to_string()),
                "lease_state": record.lease_state.map(|state| state.label().to_string()),
                "binding_lifecycle": record
                    .binding_lifecycle
                    .map(|state| state.label().to_string()),
                "process_lifecycle": record
                    .process_lifecycle
                    .map(|state| state.label().to_string()),
                "reuse_scope": record.reuse_scope.map(|scope| scope.label().to_string()),
                "parallelism_scope": record.parallelism_scope.map(|scope| scope.label().to_string()),
                "max_parallelism": record.max_parallelism,
                "strict_session_affinity": record.strict_session_affinity,
                "strict_workspace_affinity": record.strict_workspace_affinity,
                "supported_step_kinds": record
                    .supported_step_kinds
                    .iter()
                    .map(WorkerExecutionStepKind::label)
                    .collect::<Vec<_>>(),
                "health_status": record.health_status.map(|status| format!("{:?}", status)),
                "health_detail": record.health_detail,
                "failure_layer": record.failure_layer.map(|layer| layer.label().to_string()),
                "failure_message": record.failure_message,
                "observed_at": record.observed_at.0,
            }),
        );
        record
    }

    pub fn executor_observations(&self) -> Vec<WorkerExecutorObservation> {
        self.executor_observations
            .read()
            .expect("worker executor observation read lock poisoned")
            .clone()
    }

    fn observation_from_probe(
        &self,
        worker_id: &WorkerId,
        task_id: Option<TaskId>,
        requested_stage: Option<WorkerStage>,
        request: Option<&WorkerExecutorRequest>,
        probe: &WorkerExecutorProbe,
    ) -> WorkerExecutorObservation {
        WorkerExecutorObservation {
            worker_id: worker_id.clone(),
            task_id,
            requested_stage,
            request_id: request.map(|value| value.request_id.clone()),
            request_source: request.map(|value| value.request_source.clone()),
            requested_reuse_policy: request
                .map(|value| value.requested_execution_profile.reuse_policy),
            requested_binding_scope: request
                .map(|value| value.requested_execution_profile.binding_scope),
            requested_lease_state: request.map(|value| value.requested_lease_state),
            requested_binding_lifecycle: request.map(|value| value.requested_binding_lifecycle),
            requested_process_lifecycle: request.map(|value| value.requested_process_lifecycle),
            requested_process_model: request
                .and_then(|value| value.requested_execution_profile.requested_process_model),
            requested_parallelism: request
                .map(|value| value.requested_execution_profile.requested_parallelism),
            requested_step_kinds: request
                .map(|value| value.required_step_kinds.clone())
                .unwrap_or_default(),
            executor_kind: probe.executor_kind,
            observation_status: match probe.health.status {
                LocalProcessExecutorHealthStatus::Healthy => WorkerExecutorObservationStatus::Ready,
                LocalProcessExecutorHealthStatus::Degraded => {
                    WorkerExecutorObservationStatus::Degraded
                }
                LocalProcessExecutorHealthStatus::Unavailable => {
                    WorkerExecutorObservationStatus::Unavailable
                }
            },
            executor_id: Some(probe.executor_id.clone()),
            executor_version: Some(probe.executor_version.clone()),
            executor_instance_id: probe.capability.descriptor.executor_instance_id.clone(),
            executor_lease_id: probe.capability.descriptor.executor_lease_id.clone(),
            execution_mode: Some(probe.capability.execution_mode),
            protocol_version: Some(probe.capability.protocol_version.clone()),
            process_model: Some(probe.capability.descriptor.process_model),
            lease_state: Some(probe.capability.descriptor.lease_state),
            binding_lifecycle: Some(probe.capability.descriptor.binding_lifecycle),
            process_lifecycle: Some(probe.capability.descriptor.process_lifecycle),
            reuse_scope: Some(probe.capability.descriptor.reuse_scope),
            parallelism_scope: Some(probe.capability.descriptor.parallelism_scope),
            max_parallelism: Some(probe.capability.descriptor.max_parallelism),
            strict_session_affinity: Some(probe.capability.affinity.strict_session),
            strict_workspace_affinity: Some(probe.capability.affinity.strict_workspace),
            supported_step_kinds: probe.capability.supported_step_kinds.clone(),
            health_status: Some(probe.health.status),
            health_detail: Some(public_executor_health_detail(probe.health.status).to_string()),
            failure_layer: None,
            failure_message: None,
            observed_at: UtcMillis::now(),
        }
    }

    fn observation_from_error(
        &self,
        worker_id: &WorkerId,
        task_id: Option<TaskId>,
        requested_stage: Option<WorkerStage>,
        request: Option<&WorkerExecutorRequest>,
        error: &WorkerExecutorFailure,
    ) -> WorkerExecutorObservation {
        WorkerExecutorObservation {
            worker_id: worker_id.clone(),
            task_id,
            requested_stage,
            request_id: request.map(|value| value.request_id.clone()),
            request_source: request.map(|value| value.request_source.clone()),
            requested_reuse_policy: request
                .map(|value| value.requested_execution_profile.reuse_policy),
            requested_binding_scope: request
                .map(|value| value.requested_execution_profile.binding_scope),
            requested_lease_state: request.map(|value| value.requested_lease_state),
            requested_binding_lifecycle: request.map(|value| value.requested_binding_lifecycle),
            requested_process_lifecycle: request.map(|value| value.requested_process_lifecycle),
            requested_process_model: request
                .and_then(|value| value.requested_execution_profile.requested_process_model),
            requested_parallelism: request
                .map(|value| value.requested_execution_profile.requested_parallelism),
            requested_step_kinds: request
                .map(|value| value.required_step_kinds.clone())
                .unwrap_or_default(),
            executor_kind: self.executor_kind(),
            observation_status: WorkerExecutorObservationStatus::Unavailable,
            executor_id: error
                .detail
                .as_ref()
                .and_then(|detail| detail.executor_id.clone()),
            executor_version: error
                .detail
                .as_ref()
                .and_then(|detail| detail.executor_version.clone()),
            executor_instance_id: error
                .detail
                .as_ref()
                .and_then(|detail| detail.executor_instance_id.clone()),
            executor_lease_id: error
                .detail
                .as_ref()
                .and_then(|detail| detail.executor_lease_id.clone()),
            execution_mode: None,
            protocol_version: None,
            process_model: None,
            lease_state: error
                .detail
                .as_ref()
                .and_then(|detail| detail.effective_lease_state),
            binding_lifecycle: error
                .detail
                .as_ref()
                .and_then(|detail| detail.effective_binding_lifecycle),
            process_lifecycle: error
                .detail
                .as_ref()
                .and_then(|detail| detail.effective_process_lifecycle),
            reuse_scope: error
                .detail
                .as_ref()
                .and_then(|detail| detail.effective_reuse_scope),
            parallelism_scope: error
                .detail
                .as_ref()
                .and_then(|detail| detail.effective_parallelism_scope),
            max_parallelism: None,
            strict_session_affinity: None,
            strict_workspace_affinity: None,
            supported_step_kinds: error
                .detail
                .as_ref()
                .map(|detail| detail.supported_step_kinds.clone())
                .unwrap_or_default(),
            health_status: None,
            health_detail: None,
            failure_layer: Some(error.layer),
            failure_message: Some(error.public_summary().to_string()),
            observed_at: UtcMillis::now(),
        }
    }
}

fn public_executor_health_detail(status: LocalProcessExecutorHealthStatus) -> &'static str {
    match status {
        LocalProcessExecutorHealthStatus::Healthy => EXECUTOR_HEALTH_READY_DETAIL,
        LocalProcessExecutorHealthStatus::Degraded => EXECUTOR_HEALTH_DEGRADED_DETAIL,
        LocalProcessExecutorHealthStatus::Unavailable => EXECUTOR_HEALTH_UNAVAILABLE_DETAIL,
    }
}

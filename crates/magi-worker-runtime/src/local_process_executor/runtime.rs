use super::loopback::loopback_capability;
use super::types::{
    LocalProcessExecutionRequest, LocalProcessExecutionResponse, LocalProcessExecutorCapability,
    LocalProcessExecutorProcessModel, LocalProcessProbeRequest, LocalProcessProtocolRequest,
    LocalProcessProtocolRequestKind, LocalProcessProtocolResponse,
    LocalProcessProtocolResponseKind, LocalProcessRepairRequest, LocalProcessRepairResponse,
    LocalProcessReviewRequest, LocalProcessReviewResponse, LocalProcessVerifyRequest,
    LocalProcessVerifyResponse, WorkerExecutionBindingLifecycle, WorkerExecutionBindingScope,
    WorkerExecutionLeaseState, WorkerExecutionProcessLifecycle, WorkerExecutorFailure,
};
use crate::{
    WorkerExecutor, WorkerExecutionFinalReport, WorkerExecutionIntent,
    WorkerExecutionStepKind, WorkerExecutionTrace, WorkerExecutorKind, WorkerExecutorProbe,
    WorkerExecutorRequest, WorkerStage,
};
use magi_core::{TaskResultKind, TerminationReason, UtcMillis, VerificationStatus};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LocalProcessExecutorConfig {
    pub executable: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
struct LocalProcessExecutorLeaseState {
    next_lease_sequence: u64,
    leases: BTreeMap<String, LocalProcessExecutorLeaseRecord>,
}

#[derive(Clone, Debug)]
struct LocalProcessExecutorLeaseRecord {
    lease_id: String,
    executor_instance_id: Option<String>,
    last_request_id: String,
    acquisition_count: usize,
}

#[derive(Clone, Debug)]
pub struct LocalProcessWorkerExecutor {
    config: LocalProcessExecutorConfig,
    lease_state: Arc<RwLock<LocalProcessExecutorLeaseState>>,
}

impl LocalProcessWorkerExecutor {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            config: LocalProcessExecutorConfig {
                executable: executable.into(),
                ..LocalProcessExecutorConfig::default()
            },
            lease_state: Arc::new(RwLock::new(LocalProcessExecutorLeaseState::default())),
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.config.args = args;
        self
    }

    pub fn with_working_directory(mut self, working_directory: impl Into<PathBuf>) -> Self {
        self.config.working_directory = Some(working_directory.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env.insert(key.into(), value.into());
        self
    }

    pub fn cargo_loopback() -> Self {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("worker-runtime crate should live under crates/")
            .to_path_buf();
        Self::new("cargo")
            .with_args(vec![
                "run".to_string(),
                "--quiet".to_string(),
                "-p".to_string(),
                "magi-worker-runtime".to_string(),
                "--bin".to_string(),
                "local_worker_executor".to_string(),
            ])
            .with_working_directory(workspace_root)
    }

    fn request_id(prefix: &str) -> String {
        format!("{prefix}-{}", UtcMillis::now().0)
    }

    pub(super) fn execute_step_kinds_env() -> Vec<WorkerExecutionStepKind> {
        match std::env::var("MAGI_LOCAL_WORKER_SUPPORTED_STEP_KINDS") {
            Ok(raw) => {
                let mut kinds = Vec::new();
                for token in raw
                    .split(',')
                    .map(|token| token.trim())
                    .filter(|token| !token.is_empty())
                {
                    let kind = match token {
                        "builtin-tool-invocation" | "builtin" => {
                            Some(WorkerExecutionStepKind::BuiltinToolInvocation)
                        }
                        "skill-dispatch" | "skill" => Some(WorkerExecutionStepKind::SkillDispatch),
                        "final-report" | "final" => Some(WorkerExecutionStepKind::FinalReport),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        if !kinds.contains(&kind) {
                            kinds.push(kind);
                        }
                    }
                }
                if kinds.is_empty() {
                    vec![
                        WorkerExecutionStepKind::BuiltinToolInvocation,
                        WorkerExecutionStepKind::SkillDispatch,
                        WorkerExecutionStepKind::FinalReport,
                    ]
                } else {
                    kinds
                }
            }
            Err(_) => vec![
                WorkerExecutionStepKind::BuiltinToolInvocation,
                WorkerExecutionStepKind::SkillDispatch,
                WorkerExecutionStepKind::FinalReport,
            ],
        }
    }

    pub(super) fn env_bool(name: &str, default: bool) -> bool {
        match std::env::var(name) {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => default,
            },
            Err(_) => default,
        }
    }

    pub(super) fn env_string(name: &str, default: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| default.to_string())
    }

    pub(super) fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    fn binding_key_for_request(
        request: &WorkerExecutorRequest,
    ) -> Result<Option<String>, WorkerExecutorFailure> {
        match request.requested_execution_profile.binding_scope {
            WorkerExecutionBindingScope::None => Ok(None),
            WorkerExecutionBindingScope::Session => request
                .session_id
                .as_ref()
                .map(|session_id| Some(format!("session:{}", session_id)))
                .ok_or_else(|| {
                    WorkerExecutorFailure::remote_business(
                        "session binding requires session_id in executor request",
                    )
                }),
            WorkerExecutionBindingScope::Workspace => request
                .workspace_id
                .as_ref()
                .map(|workspace_id| Some(format!("workspace:{}", workspace_id)))
                .ok_or_else(|| {
                    WorkerExecutorFailure::remote_business(
                        "workspace binding requires workspace_id in executor request",
                    )
                }),
        }
    }

    pub(super) fn capability_for_request_static(
        request: Option<&WorkerExecutorRequest>,
    ) -> Result<LocalProcessExecutorCapability, WorkerExecutorFailure> {
        let mut capability = loopback_capability();
        let Some(request) = request else {
            return Ok(capability);
        };

        let binding_key = Self::binding_key_for_request(request)?;
        if capability.descriptor.reuse_scope != WorkerExecutionBindingScope::None
            && request.requested_execution_profile.binding_scope
                == capability.descriptor.reuse_scope
            && binding_key.is_some()
        {
            let binding_key = binding_key.expect("binding key checked above");
            let binding_suffix = binding_key.replace(':', "-");
            capability.descriptor.executor_lease_id = Some(
                capability
                    .descriptor
                    .executor_lease_id
                    .clone()
                    .unwrap_or_else(|| {
                        format!("{}-{binding_suffix}-lease", capability.executor_id)
                    }),
            );
            capability.descriptor.lease_state = match request.requested_lease_state {
                WorkerExecutionLeaseState::Released => WorkerExecutionLeaseState::Released,
                WorkerExecutionLeaseState::Expired => WorkerExecutionLeaseState::Expired,
                WorkerExecutionLeaseState::Requested | WorkerExecutionLeaseState::Active => {
                    WorkerExecutionLeaseState::Active
                }
                WorkerExecutionLeaseState::None => WorkerExecutionLeaseState::None,
            };
            capability.descriptor.binding_lifecycle = match request.requested_binding_lifecycle {
                WorkerExecutionBindingLifecycle::Released => {
                    WorkerExecutionBindingLifecycle::Released
                }
                WorkerExecutionBindingLifecycle::Requested
                | WorkerExecutionBindingLifecycle::Bound => WorkerExecutionBindingLifecycle::Bound,
                WorkerExecutionBindingLifecycle::None => WorkerExecutionBindingLifecycle::None,
            };
        }

        capability.descriptor.process_lifecycle = match capability.descriptor.process_model {
            LocalProcessExecutorProcessModel::PersistentProcess => {
                match request.requested_process_lifecycle {
                    WorkerExecutionProcessLifecycle::Reusable => {
                        WorkerExecutionProcessLifecycle::Reusable
                    }
                    WorkerExecutionProcessLifecycle::Persistent => {
                        WorkerExecutionProcessLifecycle::Persistent
                    }
                    WorkerExecutionProcessLifecycle::OneShot => {
                        WorkerExecutionProcessLifecycle::Persistent
                    }
                }
            }
            _ => WorkerExecutionProcessLifecycle::OneShot,
        };

        Ok(capability)
    }

    fn apply_runtime_lease_state(
        &self,
        mut capability: LocalProcessExecutorCapability,
        request: Option<&WorkerExecutorRequest>,
    ) -> Result<LocalProcessExecutorCapability, WorkerExecutorFailure> {
        let Some(request) = request else {
            return Ok(capability);
        };

        if capability.descriptor.process_model
            != LocalProcessExecutorProcessModel::PersistentProcess
        {
            return Ok(capability);
        }

        let binding_key = Self::binding_key_for_request(request)?;
        let Some(binding_key) = binding_key else {
            return Ok(capability);
        };

        if capability.descriptor.reuse_scope == WorkerExecutionBindingScope::None
            || request.requested_execution_profile.binding_scope
                != capability.descriptor.reuse_scope
        {
            return Ok(capability);
        }

        let mut state = self
            .lease_state
            .write()
            .expect("local process executor lease state lock poisoned");

        if matches!(
            request.requested_lease_state,
            WorkerExecutionLeaseState::Released | WorkerExecutionLeaseState::Expired
        ) || matches!(
            request.requested_binding_lifecycle,
            WorkerExecutionBindingLifecycle::Released
        ) {
            let removed = state.leases.remove(&binding_key);
            capability.descriptor.executor_lease_id =
                removed.as_ref().map(|record| record.lease_id.clone());
            capability.descriptor.executor_instance_id = removed
                .as_ref()
                .and_then(|record| record.executor_instance_id.clone())
                .or_else(|| capability.descriptor.executor_instance_id.clone());
            capability.descriptor.lease_state =
                if request.requested_lease_state == WorkerExecutionLeaseState::Expired {
                    WorkerExecutionLeaseState::Expired
                } else {
                    WorkerExecutionLeaseState::Released
                };
            capability.descriptor.binding_lifecycle = WorkerExecutionBindingLifecycle::Released;
            return Ok(capability);
        }

        let instance_id = capability
            .descriptor
            .executor_instance_id
            .clone()
            .unwrap_or_else(|| format!("{}-persistent-instance", capability.executor_id));
        let next_sequence = {
            state.next_lease_sequence += 1;
            state.next_lease_sequence
        };
        let binding_suffix = binding_key.replace(':', "-");
        let record = state.leases.entry(binding_key.clone()).or_insert_with(|| {
            LocalProcessExecutorLeaseRecord {
                lease_id: format!(
                    "{}-{binding_suffix}-lease-{next_sequence}",
                    capability.executor_id
                ),
                executor_instance_id: Some(instance_id.clone()),
                last_request_id: request.request_id.clone(),
                acquisition_count: 0,
            }
        });
        record.last_request_id = request.request_id.clone();
        record.acquisition_count += 1;
        capability.descriptor.executor_instance_id = record.executor_instance_id.clone();
        capability.descriptor.executor_lease_id = Some(record.lease_id.clone());
        capability.descriptor.lease_state = WorkerExecutionLeaseState::Active;
        capability.descriptor.binding_lifecycle = WorkerExecutionBindingLifecycle::Bound;
        Ok(capability)
    }

    pub(super) fn validate_executor_request(
        capability: &LocalProcessExecutorCapability,
        request: &WorkerExecutorRequest,
    ) -> Result<(), WorkerExecutorFailure> {
        Self::binding_key_for_request(request).map_err(|error| {
            WorkerExecutorFailure::remote_business_with_detail(
                error.message,
                capability.failure_detail(
                    Some(&request.requested_execution_profile),
                    request.required_step_kinds.clone(),
                    Vec::new(),
                ),
            )
        })?;
        capability.supports_profile(&request.requested_execution_profile)?;
        if !capability.supports_stage(request.requested_stage) {
            return Err(WorkerExecutorFailure::remote_business_with_detail(
                format!(
                    "executor {} {} does not support {}",
                    capability.executor_id,
                    capability.executor_version,
                    request.requested_stage.label()
                ),
                capability.failure_detail(
                    Some(&request.requested_execution_profile),
                    request.required_step_kinds.clone(),
                    Vec::new(),
                ),
            ));
        }
        capability.supports_context(&request.session_id, &request.workspace_id)?;
        let required_step_kinds = request.required_step_kinds.clone();
        let missing_step_kinds: Vec<_> = required_step_kinds
            .iter()
            .copied()
            .filter(|kind| !capability.supported_step_kinds.contains(kind))
            .collect();
        if missing_step_kinds.is_empty() {
            return Ok(());
        }
        Err(WorkerExecutorFailure::remote_business_with_detail(
            format!(
                "executor {} {} missing required steps: {}",
                capability.executor_id,
                capability.executor_version,
                missing_step_kinds
                    .iter()
                    .map(WorkerExecutionStepKind::label)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            capability.failure_detail(
                Some(&request.requested_execution_profile),
                required_step_kinds,
                missing_step_kinds,
            ),
        ))
    }

    fn exchange(
        &self,
        request: &LocalProcessProtocolRequest,
    ) -> Result<LocalProcessProtocolResponse, WorkerExecutorFailure> {
        let mut command = Command::new(&self.config.executable);
        command.args(&self.config.args);
        if let Some(working_directory) = &self.config.working_directory {
            command.current_dir(working_directory);
        }
        for (key, value) in &self.config.env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| {
            WorkerExecutorFailure::transport(format!("spawn local worker executor failed: {error}"))
        })?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            WorkerExecutorFailure::transport("local worker executor stdin unavailable")
        })?;
        let request_json = serde_json::to_vec(request).map_err(|error| {
            WorkerExecutorFailure::protocol(format!(
                "serialize local worker request failed: {error}"
            ))
        })?;
        stdin.write_all(&request_json).map_err(|error| {
            WorkerExecutorFailure::transport(format!("write local worker request failed: {error}"))
        })?;
        drop(stdin);

        let output = child.wait_with_output().map_err(|error| {
            WorkerExecutorFailure::transport(format!("wait local worker executor failed: {error}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(WorkerExecutorFailure::transport(if stderr.is_empty() {
                format!("local worker executor exited with status {}", output.status)
            } else {
                format!(
                    "local worker executor exited with status {}: {stderr}",
                    output.status
                )
            }));
        }

        let response: LocalProcessProtocolResponse = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                WorkerExecutorFailure::protocol(format!(
                    "decode local worker response failed: {error}"
                ))
            })?;
        if response.request_id != request.request_id {
            return Err(WorkerExecutorFailure::protocol(format!(
                "local worker response request_id mismatch: expected {}, got {}",
                request.request_id, response.request_id
            )));
        }
        match &response.kind {
            LocalProcessProtocolResponseKind::Error(error) => Err(error.clone()),
            _ => Ok(response),
        }
    }

    fn execute_request(
        &self,
        request: &LocalProcessExecutionRequest,
    ) -> Result<LocalProcessExecutionResponse, WorkerExecutorFailure> {
        let protocol_request = LocalProcessProtocolRequest {
            request_id: Self::request_id("execute"),
            kind: LocalProcessProtocolRequestKind::Execute(request.clone()),
        };
        match self.exchange(&protocol_request)?.kind {
            LocalProcessProtocolResponseKind::Execute(response) => Ok(response),
            other => Err(WorkerExecutorFailure::protocol(format!(
                "local worker execute returned unexpected response: {other:?}"
            ))),
        }
    }

    fn review_request(
        &self,
        request: &LocalProcessReviewRequest,
    ) -> Result<LocalProcessReviewResponse, WorkerExecutorFailure> {
        let protocol_request = LocalProcessProtocolRequest {
            request_id: Self::request_id("review"),
            kind: LocalProcessProtocolRequestKind::Review(request.clone()),
        };
        match self.exchange(&protocol_request)?.kind {
            LocalProcessProtocolResponseKind::Review(response) => Ok(response),
            other => Err(WorkerExecutorFailure::protocol(format!(
                "local worker review returned unexpected response: {other:?}"
            ))),
        }
    }

    fn verify_request(
        &self,
        request: &LocalProcessVerifyRequest,
    ) -> Result<LocalProcessVerifyResponse, WorkerExecutorFailure> {
        let protocol_request = LocalProcessProtocolRequest {
            request_id: Self::request_id("verify"),
            kind: LocalProcessProtocolRequestKind::Verify(request.clone()),
        };
        match self.exchange(&protocol_request)?.kind {
            LocalProcessProtocolResponseKind::Verify(response) => Ok(response),
            other => Err(WorkerExecutorFailure::protocol(format!(
                "local worker verify returned unexpected response: {other:?}"
            ))),
        }
    }

    fn repair_request(
        &self,
        request: &LocalProcessRepairRequest,
    ) -> Result<LocalProcessRepairResponse, WorkerExecutorFailure> {
        let protocol_request = LocalProcessProtocolRequest {
            request_id: Self::request_id("repair"),
            kind: LocalProcessProtocolRequestKind::Repair(request.clone()),
        };
        match self.exchange(&protocol_request)?.kind {
            LocalProcessProtocolResponseKind::Repair(response) => Ok(response),
            other => Err(WorkerExecutorFailure::protocol(format!(
                "local worker repair returned unexpected response: {other:?}"
            ))),
        }
    }

    fn probe_request(
        &self,
        request: Option<&WorkerExecutorRequest>,
    ) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        let protocol_request = LocalProcessProtocolRequest {
            request_id: Self::request_id("probe"),
            kind: LocalProcessProtocolRequestKind::Probe(LocalProcessProbeRequest {
                executor_request: request.cloned(),
            }),
        };
        match self.exchange(&protocol_request)?.kind {
            LocalProcessProtocolResponseKind::Probe(response) => {
                let capability = self.apply_runtime_lease_state(response.capability, request)?;
                Ok(WorkerExecutorProbe {
                    executor_id: capability.executor_id.clone(),
                    executor_version: capability.executor_version.clone(),
                    executor_kind: WorkerExecutorKind::LocalProcess,
                    capability,
                    health: response.health,
                })
            }
            other => Err(WorkerExecutorFailure::protocol(format!(
                "local worker probe returned unexpected response: {other:?}"
            ))),
        }
    }
}

impl WorkerExecutor for LocalProcessWorkerExecutor {
    fn execute(&self, intent: &WorkerExecutionIntent) -> WorkerExecutionTrace {
        self.execute_checked(intent)
            .unwrap_or_else(|error| WorkerExecutionTrace {
                worker_id: intent.worker_id.clone(),
                task_id: intent.task_id.clone(),
                tool_invocations: Vec::new(),
                skill_dispatches: Vec::new(),
                final_report: WorkerExecutionFinalReport {
                    summary: format!(
                        "local process execution failed [{}]: {}",
                        error.layer.label(),
                        error.message
                    ),
                    result_kind: Some(TaskResultKind::Failure),
                    termination_reason: Some(TerminationReason::Failed),
                    verification_status: VerificationStatus::Failed,
                },
            })
    }

    fn execute_checked(
        &self,
        intent: &WorkerExecutionIntent,
    ) -> Result<WorkerExecutionTrace, WorkerExecutorFailure> {
        let executor_request = intent.executor_request(WorkerStage::Execute, "execute");
        let probe = self.probe_request(Some(&executor_request))?;
        probe.supports_request(&executor_request)?;
        Ok(self
            .execute_request(&LocalProcessExecutionRequest {
                executor_request,
                intent: intent.clone(),
                checkpoint_cursor: None,
            })?
            .trace)
    }

    fn execute_from_checkpoint(
        &self,
        intent: &WorkerExecutionIntent,
        checkpoint_cursor: Option<&crate::WorkerExecutionCheckpointCursor>,
    ) -> Result<crate::WorkerExecutionProgress, WorkerExecutorFailure> {
        let executor_request = intent.executor_request(WorkerStage::Execute, "execute");
        let probe = self.probe_request(Some(&executor_request))?;
        probe.supports_request(&executor_request)?;
        let response = self.execute_request(&LocalProcessExecutionRequest {
            executor_request,
            intent: intent.clone(),
            checkpoint_cursor: checkpoint_cursor.cloned(),
        })?;
        Ok(crate::WorkerExecutionProgress {
            trace: response.trace,
            next_step_index: response.next_step_index,
            completed: response.completed,
            checkpoint_cursor: Some(crate::WorkerExecutionCheckpointCursor {
                checkpoint_stage: WorkerStage::Execute,
                next_step_index: response.next_step_index,
                checkpoint_at: magi_core::UtcMillis::now(),
                resume_mode: crate::WorkerCheckpointResumeMode::StepCheckpoint,
                resume_token: None,
            }),
        })
    }

    fn probe(&self) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        self.probe_request(None)
    }

    fn probe_for_request(
        &self,
        request: Option<&WorkerExecutorRequest>,
    ) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        self.probe_request(request)
    }

    fn review(
        &self,
        intent: &WorkerExecutionIntent,
        prior_trace: Option<&WorkerExecutionTrace>,
    ) -> Result<(WorkerExecutionTrace, String), WorkerExecutorFailure> {
        let executor_request = intent.executor_request(WorkerStage::Review, "review");
        let probe = self.probe_request(Some(&executor_request))?;
        probe.supports_request(&executor_request)?;
        let response = self.review_request(&LocalProcessReviewRequest {
            executor_request,
            intent: intent.clone(),
            prior_trace: prior_trace.cloned(),
            checkpoint_cursor: None,
        })?;
        Ok((response.trace, response.review_summary))
    }

    fn verify(
        &self,
        intent: &WorkerExecutionIntent,
        prior_trace: Option<&WorkerExecutionTrace>,
    ) -> Result<(WorkerExecutionTrace, VerificationStatus, String), WorkerExecutorFailure> {
        let executor_request = intent.executor_request(WorkerStage::Verify, "verify");
        let probe = self.probe_request(Some(&executor_request))?;
        probe.supports_request(&executor_request)?;
        let response = self.verify_request(&LocalProcessVerifyRequest {
            executor_request,
            intent: intent.clone(),
            prior_trace: prior_trace.cloned(),
            checkpoint_cursor: None,
        })?;
        Ok((
            response.trace,
            response.verification_status,
            response.verify_summary,
        ))
    }

    fn repair(
        &self,
        intent: &WorkerExecutionIntent,
        prior_trace: Option<&WorkerExecutionTrace>,
        repair_reason: &str,
    ) -> Result<(WorkerExecutionTrace, String), WorkerExecutorFailure> {
        let executor_request = intent.executor_request(WorkerStage::Repair, "repair");
        let probe = self.probe_request(Some(&executor_request))?;
        probe.supports_request(&executor_request)?;
        let response = self.repair_request(&LocalProcessRepairRequest {
            executor_request,
            intent: intent.clone(),
            prior_trace: prior_trace.cloned(),
            repair_reason: repair_reason.to_string(),
            checkpoint_cursor: None,
        })?;
        Ok((response.trace, response.repair_summary))
    }

    fn executor_kind(&self) -> WorkerExecutorKind {
        WorkerExecutorKind::LocalProcess
    }
}

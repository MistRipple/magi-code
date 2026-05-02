use super::runtime::LocalProcessWorkerExecutor;
use super::types::{
    LOCAL_PROCESS_PROTOCOL_VERSION, LocalProcessExecutionResponse, LocalProcessExecutorAffinity,
    LocalProcessExecutorCapability, LocalProcessExecutorDescriptor, LocalProcessExecutorHealth,
    LocalProcessExecutorHealthStatus, LocalProcessExecutorProcessModel,
    LocalProcessExecutorStageMatrix, LocalProcessProbeResponse, LocalProcessProtocolRequest,
    LocalProcessProtocolRequestKind, LocalProcessProtocolResponse,
    LocalProcessProtocolResponseKind, LocalProcessRepairResponse, LocalProcessReviewResponse,
    LocalProcessVerifyResponse, WorkerExecutionBindingLifecycle, WorkerExecutionBindingScope,
    WorkerExecutionLeaseState, WorkerExecutionMode, WorkerExecutionParallelismScope,
    WorkerExecutionProcessLifecycle, WorkerExecutorFailure,
};
use crate::{
    WorkerCheckpointResumeMode, WorkerExecutionCheckpointCursor, WorkerExecutionFinalReport,
    WorkerExecutionIntent, WorkerExecutionIntentStep, WorkerExecutionTrace, WorkerStage,
    WorkerToolInvocation, derive_final_report,
};
use magi_bridge_client::BridgeDispatchRuntime;
use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::GovernanceService;
use magi_skill_runtime::SkillDispatchRuntime;
use magi_tool_runtime::ToolRegistry;
use std::io::{Read, Write};
use std::sync::Arc;

pub fn execute_intent_with_shadow_drivers(intent: &WorkerExecutionIntent) -> WorkerExecutionTrace {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let skill_dispatch_runtime =
        SkillDispatchRuntime::new(tool_registry.clone(), BridgeDispatchRuntime::new());
    execute_intent_with_drivers(intent, &tool_registry, &skill_dispatch_runtime)
}

pub fn execute_intent_step_with_shadow_drivers(
    intent: &WorkerExecutionIntent,
    step_index: usize,
) -> Result<(WorkerExecutionTrace, WorkerExecutionCheckpointCursor, bool), WorkerExecutorFailure> {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(governance, event_bus);
    tool_registry.register_default_builtins();
    let skill_dispatch_runtime =
        SkillDispatchRuntime::new(tool_registry.clone(), BridgeDispatchRuntime::new());
    execute_intent_step_with_drivers(intent, step_index, &tool_registry, &skill_dispatch_runtime)
}

pub fn execute_intent_with_drivers(
    intent: &WorkerExecutionIntent,
    tool_registry: &ToolRegistry,
    skill_dispatch_runtime: &SkillDispatchRuntime,
) -> WorkerExecutionTrace {
    let context = magi_tool_runtime::ToolExecutionContext {
        worker_id: Some(intent.worker_id.clone()),
        task_id: Some(intent.task_id.clone()),
        session_id: intent.session_id.clone(),
        workspace_id: intent.workspace_id.clone(),
        working_directory: None,
    };

    let mut tool_invocations = Vec::new();
    let mut skill_dispatches = Vec::new();
    let mut final_report: Option<WorkerExecutionFinalReport> = None;

    for step in &intent.steps {
        match step {
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id,
                tool_name,
                tool_kind,
                input,
                approval_requirement,
                risk_level,
                ..
            } => {
                let output = tool_registry.execute_with_policy(
                    magi_tool_runtime::ToolExecutionInput {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        tool_kind: tool_kind.clone(),
                        input: input.clone(),
                        approval_requirement: *approval_requirement,
                        risk_level: *risk_level,
                    },
                    context.clone(),
                    &magi_tool_runtime::ToolExecutionPolicy::default(),
                );
                tool_invocations.push(WorkerToolInvocation {
                    worker_id: intent.worker_id.clone(),
                    task_id: intent.task_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    status: output.status,
                    observed_at: UtcMillis::now(),
                });
            }
            WorkerExecutionIntentStep::SkillDispatch {
                tool_call_id,
                tool_name,
                plan,
                payload,
                approval_requirement,
                risk_level,
                working_directory,
                ..
            } => {
                let outcome = skill_dispatch_runtime.dispatch_observed(
                    plan,
                    magi_skill_runtime::SkillDispatchInput {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        binding_id: plan
                            .bridge_dispatch_plan
                            .bindings
                            .first()
                            .map(|binding| binding.binding_id.clone()),
                        payload: payload.clone(),
                        approval_requirement: *approval_requirement,
                        risk_level: *risk_level,
                        context: context.clone(),
                        working_directory: working_directory.clone(),
                    },
                );
                skill_dispatches.push(outcome.observation);
            }
            WorkerExecutionIntentStep::FinalReport(report) => {
                final_report = Some(report.clone());
            }
        }
    }

    let final_report =
        final_report.unwrap_or_else(|| derive_final_report(&tool_invocations, &skill_dispatches));
    WorkerExecutionTrace {
        worker_id: intent.worker_id.clone(),
        task_id: intent.task_id.clone(),
        tool_invocations,
        skill_dispatches,
        final_report,
    }
}

pub fn execute_intent_step_with_drivers(
    intent: &WorkerExecutionIntent,
    step_index: usize,
    tool_registry: &ToolRegistry,
    skill_dispatch_runtime: &SkillDispatchRuntime,
) -> Result<(WorkerExecutionTrace, WorkerExecutionCheckpointCursor, bool), WorkerExecutorFailure> {
    let context = magi_tool_runtime::ToolExecutionContext {
        worker_id: Some(intent.worker_id.clone()),
        task_id: Some(intent.task_id.clone()),
        session_id: intent.session_id.clone(),
        workspace_id: intent.workspace_id.clone(),
        working_directory: None,
    };
    let step = intent.steps.get(step_index).ok_or_else(|| {
        WorkerExecutorFailure::remote_business(format!(
            "checkpoint step index {} 超出 intent steps 范围",
            step_index
        ))
    })?;
    let mut tool_invocations = Vec::new();
    let mut skill_dispatches = Vec::new();
    let final_report = match step {
        WorkerExecutionIntentStep::BuiltinToolInvocation {
            tool_call_id,
            tool_name,
            tool_kind,
            input,
            approval_requirement,
            risk_level,
            ..
        } => {
            let output = tool_registry.execute_with_policy(
                magi_tool_runtime::ToolExecutionInput {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_kind: tool_kind.clone(),
                    input: input.clone(),
                    approval_requirement: *approval_requirement,
                    risk_level: *risk_level,
                },
                context.clone(),
                &magi_tool_runtime::ToolExecutionPolicy::default(),
            );
            tool_invocations.push(WorkerToolInvocation {
                worker_id: intent.worker_id.clone(),
                task_id: intent.task_id.clone(),
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                status: output.status,
                observed_at: UtcMillis::now(),
            });
            WorkerExecutionFinalReport {
                summary: format!("step {} completed", step_index),
                result_kind: None,
                termination_reason: None,
                verification_status: magi_core::VerificationStatus::Pending,
            }
        }
        WorkerExecutionIntentStep::SkillDispatch {
            tool_call_id,
            tool_name,
            plan,
            payload,
            approval_requirement,
            risk_level,
            working_directory,
            ..
        } => {
            let outcome = skill_dispatch_runtime.dispatch_observed(
                plan,
                magi_skill_runtime::SkillDispatchInput {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    binding_id: plan
                        .bridge_dispatch_plan
                        .bindings
                        .first()
                        .map(|binding| binding.binding_id.clone()),
                    payload: payload.clone(),
                    approval_requirement: *approval_requirement,
                    risk_level: *risk_level,
                    context: context.clone(),
                    working_directory: working_directory.clone(),
                },
            );
            skill_dispatches.push(outcome.observation);
            WorkerExecutionFinalReport {
                summary: format!("step {} completed", step_index),
                result_kind: None,
                termination_reason: None,
                verification_status: magi_core::VerificationStatus::Pending,
            }
        }
        WorkerExecutionIntentStep::FinalReport(report) => report.clone(),
    };
    let next_step_index = step_index + 1;
    let completed = next_step_index >= intent.steps.len();
    Ok((
        WorkerExecutionTrace {
            worker_id: intent.worker_id.clone(),
            task_id: intent.task_id.clone(),
            tool_invocations,
            skill_dispatches,
            final_report,
        },
        WorkerExecutionCheckpointCursor {
            checkpoint_stage: WorkerStage::Execute,
            next_step_index,
            checkpoint_at: UtcMillis::now(),
            resume_mode: WorkerCheckpointResumeMode::StepCheckpoint,
            resume_token: None,
        },
        completed,
    ))
}

pub(super) fn loopback_capability() -> LocalProcessExecutorCapability {
    let session_id = std::env::var("MAGI_LOCAL_WORKER_SESSION_ID")
        .ok()
        .and_then(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(SessionId::new(value))
            }
        });
    let workspace_id = std::env::var("MAGI_LOCAL_WORKER_WORKSPACE_ID")
        .ok()
        .and_then(|value| {
            if value.trim().is_empty() {
                None
            } else {
                Some(WorkspaceId::new(value))
            }
        });
    let process_model = {
        let process_model = LocalProcessWorkerExecutor::env_string(
            "MAGI_LOCAL_WORKER_PROCESS_MODEL",
            "one-shot-subprocess",
        )
        .trim()
        .to_ascii_lowercase();
        match process_model.as_str() {
            "shadow-loopback" | "shadow" => LocalProcessExecutorProcessModel::ShadowLoopback,
            "persistent-process" | "persistent" => {
                LocalProcessExecutorProcessModel::PersistentProcess
            }
            _ => LocalProcessExecutorProcessModel::OneShotSubprocess,
        }
    };
    let default_reuse_scope = match process_model {
        LocalProcessExecutorProcessModel::PersistentProcess => WorkerExecutionBindingScope::Session,
        LocalProcessExecutorProcessModel::OneShotSubprocess
        | LocalProcessExecutorProcessModel::ShadowLoopback => WorkerExecutionBindingScope::None,
    };
    let reuse_scope = {
        let value = LocalProcessWorkerExecutor::env_string(
            "MAGI_LOCAL_WORKER_REUSE_SCOPE",
            default_reuse_scope.label(),
        )
        .trim()
        .to_ascii_lowercase();
        match value.as_str() {
            "workspace" => WorkerExecutionBindingScope::Workspace,
            "session" => WorkerExecutionBindingScope::Session,
            _ => WorkerExecutionBindingScope::None,
        }
    };
    let parallelism_scope = {
        let value = LocalProcessWorkerExecutor::env_string(
            "MAGI_LOCAL_WORKER_PARALLELISM_SCOPE",
            "executor",
        )
        .trim()
        .to_ascii_lowercase();
        match value.as_str() {
            "workspace" => WorkerExecutionParallelismScope::Workspace,
            "session" => WorkerExecutionParallelismScope::Session,
            _ => WorkerExecutionParallelismScope::Executor,
        }
    };
    let executor_id = LocalProcessWorkerExecutor::env_string(
        "MAGI_LOCAL_WORKER_EXECUTOR_ID",
        "shadow-local-process-worker-executor",
    );
    let executor_instance_id = std::env::var("MAGI_LOCAL_WORKER_INSTANCE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            if process_model == LocalProcessExecutorProcessModel::PersistentProcess {
                Some(format!("{executor_id}-instance-1"))
            } else {
                None
            }
        });
    let executor_lease_id = std::env::var("MAGI_LOCAL_WORKER_LEASE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(None);
    LocalProcessExecutorCapability {
        executor_id,
        executor_version: LocalProcessWorkerExecutor::env_string(
            "MAGI_LOCAL_WORKER_EXECUTOR_VERSION",
            "worker-shadow-local-process-executor-v2",
        ),
        execution_mode: {
            let execution_mode = LocalProcessWorkerExecutor::env_string(
                "MAGI_LOCAL_WORKER_EXECUTION_MODE",
                "local-process",
            )
            .trim()
            .to_ascii_lowercase();
            match execution_mode.as_str() {
                "shadow-loopback" | "shadow" => WorkerExecutionMode::ShadowLoopback,
                _ => WorkerExecutionMode::LocalProcess,
            }
        },
        protocol_version: LOCAL_PROCESS_PROTOCOL_VERSION.to_string(),
        supports_probe: LocalProcessWorkerExecutor::env_bool(
            "MAGI_LOCAL_WORKER_SUPPORTS_PROBE",
            true,
        ),
        supports_execute: LocalProcessWorkerExecutor::env_bool(
            "MAGI_LOCAL_WORKER_SUPPORTS_EXECUTE",
            true,
        ),
        supports_review: LocalProcessWorkerExecutor::env_bool(
            "MAGI_LOCAL_WORKER_SUPPORTS_REVIEW",
            true,
        ),
        supports_verify: LocalProcessWorkerExecutor::env_bool(
            "MAGI_LOCAL_WORKER_SUPPORTS_VERIFY",
            true,
        ),
        supports_repair: LocalProcessWorkerExecutor::env_bool(
            "MAGI_LOCAL_WORKER_SUPPORTS_REPAIR",
            true,
        ),
        affinity: LocalProcessExecutorAffinity {
            session_id,
            workspace_id,
            strict_session: LocalProcessWorkerExecutor::env_bool(
                "MAGI_LOCAL_WORKER_SESSION_STRICT",
                std::env::var("MAGI_LOCAL_WORKER_SESSION_ID").is_ok(),
            ),
            strict_workspace: LocalProcessWorkerExecutor::env_bool(
                "MAGI_LOCAL_WORKER_WORKSPACE_STRICT",
                std::env::var("MAGI_LOCAL_WORKER_WORKSPACE_ID").is_ok(),
            ),
        },
        stage_matrix: LocalProcessExecutorStageMatrix {
            execute: LocalProcessWorkerExecutor::env_bool("MAGI_LOCAL_WORKER_STAGE_EXECUTE", true),
            review: LocalProcessWorkerExecutor::env_bool("MAGI_LOCAL_WORKER_STAGE_REVIEW", true),
            verify: LocalProcessWorkerExecutor::env_bool("MAGI_LOCAL_WORKER_STAGE_VERIFY", true),
            repair: LocalProcessWorkerExecutor::env_bool("MAGI_LOCAL_WORKER_STAGE_REPAIR", true),
        },
        descriptor: LocalProcessExecutorDescriptor {
            process_model,
            reuse_scope,
            parallelism_scope,
            lease_state: WorkerExecutionLeaseState::None,
            binding_lifecycle: WorkerExecutionBindingLifecycle::None,
            process_lifecycle: match process_model {
                LocalProcessExecutorProcessModel::PersistentProcess => {
                    WorkerExecutionProcessLifecycle::Persistent
                }
                _ => WorkerExecutionProcessLifecycle::OneShot,
            },
            max_parallelism: LocalProcessWorkerExecutor::env_usize(
                "MAGI_LOCAL_WORKER_MAX_PARALLELISM",
                1,
            ),
            executor_instance_id,
            executor_lease_id,
        },
        supported_step_kinds: LocalProcessWorkerExecutor::execute_step_kinds_env(),
    }
}

fn loopback_health() -> LocalProcessExecutorHealth {
    LocalProcessExecutorHealth {
        status: LocalProcessExecutorHealthStatus::Healthy,
        detail: "local worker executor loopback ready".to_string(),
    }
}

fn write_protocol_response(response: &LocalProcessProtocolResponse) -> Result<(), String> {
    let response_json = serde_json::to_string(response)
        .map_err(|error| format!("serialize local worker response failed: {error}"))?;
    std::io::stdout()
        .write_all(response_json.as_bytes())
        .map_err(|error| format!("write local worker response failed: {error}"))?;
    Ok(())
}

pub fn run_local_worker_executor_stdio() -> Result<(), String> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|error| format!("read local worker request failed: {error}"))?;

    let request = match serde_json::from_str::<LocalProcessProtocolRequest>(&buffer) {
        Ok(request) => request,
        Err(error) => {
            let response = LocalProcessProtocolResponse {
                request_id: "invalid-request".to_string(),
                kind: LocalProcessProtocolResponseKind::Error(WorkerExecutorFailure::protocol(
                    format!("parse local worker request failed: {error}"),
                )),
            };
            return write_protocol_response(&response);
        }
    };

    let response = match request.kind {
        LocalProcessProtocolRequestKind::Probe(probe_request) => {
            let capability = match LocalProcessWorkerExecutor::capability_for_request_static(
                probe_request.executor_request.as_ref(),
            ) {
                Ok(capability) => capability,
                Err(error) => {
                    let response = LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    };
                    return write_protocol_response(&response);
                }
            };
            if let Some(executor_request) = probe_request.executor_request.as_ref() {
                if let Err(error) = LocalProcessWorkerExecutor::validate_executor_request(
                    &capability,
                    executor_request,
                ) {
                    LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    }
                } else {
                    LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Probe(LocalProcessProbeResponse {
                            capability,
                            health: loopback_health(),
                        }),
                    }
                }
            } else {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Probe(LocalProcessProbeResponse {
                        capability,
                        health: loopback_health(),
                    }),
                }
            }
        }
        LocalProcessProtocolRequestKind::Execute(execute) => {
            let capability = match LocalProcessWorkerExecutor::capability_for_request_static(Some(
                &execute.executor_request,
            )) {
                Ok(capability) => capability,
                Err(error) => {
                    let response = LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    };
                    return write_protocol_response(&response);
                }
            };
            if !capability.supports_stage(WorkerStage::Execute) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(
                        WorkerExecutorFailure::remote_business("executor does not support execute"),
                    ),
                }
            } else if execute.intent.steps.is_empty() {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(
                        WorkerExecutorFailure::remote_business("execution intent missing steps"),
                    ),
                }
            } else if let Err(error) = LocalProcessWorkerExecutor::validate_executor_request(
                &capability,
                &execute.executor_request,
            ) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(error),
                }
            } else {
                let step_index = execute
                    .checkpoint_cursor
                    .as_ref()
                    .map(|cursor| cursor.next_step_index)
                    .unwrap_or(0);
                let executed = execute_intent_step_with_shadow_drivers(&execute.intent, step_index);
                let (trace, checkpoint_cursor, completed) = match executed {
                    Ok(result) => result,
                    Err(error) => {
                        return write_protocol_response(&LocalProcessProtocolResponse {
                            request_id: request.request_id,
                            kind: LocalProcessProtocolResponseKind::Error(error),
                        });
                    }
                };
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Execute(
                        LocalProcessExecutionResponse {
                            trace,
                            next_step_index: checkpoint_cursor.next_step_index,
                            completed,
                        },
                    ),
                }
            }
        }
        LocalProcessProtocolRequestKind::Review(review) => {
            let capability = match LocalProcessWorkerExecutor::capability_for_request_static(Some(
                &review.executor_request,
            )) {
                Ok(capability) => capability,
                Err(error) => {
                    let response = LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    };
                    return write_protocol_response(&response);
                }
            };
            if !capability.supports_stage(WorkerStage::Review) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(
                        WorkerExecutorFailure::remote_business("executor does not support review"),
                    ),
                }
            } else if let Err(error) = LocalProcessWorkerExecutor::validate_executor_request(
                &capability,
                &review.executor_request,
            ) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(error),
                }
            } else {
                let trace = execute_intent_with_shadow_drivers(&review.intent);
                let review_summary = trace.final_report.summary.clone();
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Review(LocalProcessReviewResponse {
                        trace,
                        review_summary,
                        next_step_index: 1,
                        completed: true,
                    }),
                }
            }
        }
        LocalProcessProtocolRequestKind::Verify(verify) => {
            let capability = match LocalProcessWorkerExecutor::capability_for_request_static(Some(
                &verify.executor_request,
            )) {
                Ok(capability) => capability,
                Err(error) => {
                    let response = LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    };
                    return write_protocol_response(&response);
                }
            };
            if !capability.supports_stage(WorkerStage::Verify) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(
                        WorkerExecutorFailure::remote_business("executor does not support verify"),
                    ),
                }
            } else if let Err(error) = LocalProcessWorkerExecutor::validate_executor_request(
                &capability,
                &verify.executor_request,
            ) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(error),
                }
            } else {
                let trace = execute_intent_with_shadow_drivers(&verify.intent);
                let verification_status = trace.final_report.verification_status;
                let verify_summary = trace.final_report.summary.clone();
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Verify(LocalProcessVerifyResponse {
                        trace,
                        verification_status,
                        verify_summary,
                        next_step_index: 1,
                        completed: true,
                    }),
                }
            }
        }
        LocalProcessProtocolRequestKind::Repair(repair) => {
            let capability = match LocalProcessWorkerExecutor::capability_for_request_static(Some(
                &repair.executor_request,
            )) {
                Ok(capability) => capability,
                Err(error) => {
                    let response = LocalProcessProtocolResponse {
                        request_id: request.request_id,
                        kind: LocalProcessProtocolResponseKind::Error(error),
                    };
                    return write_protocol_response(&response);
                }
            };
            if !capability.supports_stage(WorkerStage::Repair) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(
                        WorkerExecutorFailure::remote_business("executor does not support repair"),
                    ),
                }
            } else if let Err(error) = LocalProcessWorkerExecutor::validate_executor_request(
                &capability,
                &repair.executor_request,
            ) {
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Error(error),
                }
            } else {
                let trace = execute_intent_with_shadow_drivers(&repair.intent);
                let repair_summary = format!(
                    "repair for: {} — {}",
                    repair.repair_reason, trace.final_report.summary
                );
                LocalProcessProtocolResponse {
                    request_id: request.request_id,
                    kind: LocalProcessProtocolResponseKind::Repair(LocalProcessRepairResponse {
                        trace,
                        repair_summary,
                        next_step_index: 1,
                        completed: true,
                    }),
                }
            }
        }
    };

    write_protocol_response(&response)
}

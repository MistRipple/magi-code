use std::sync::Arc;

use magi_bridge_client::BridgeBindingDispatchPlan;
use magi_core::{
    ApprovalRequirement, ExecutionResultStatus, RiskLevel, SessionId, TaskId, TaskResultKind,
    TerminationReason, ToolCallId, VerificationStatus, WorkerId, WorkspaceId,
};
use magi_event_bus::InMemoryEventBus;
use magi_governance::ToolKind;
use magi_skill_runtime::{
    SkillDispatchRoute, SkillDispatchStatus, SkillToolRoutingSummary, SkillToolRuntimePlan,
};
use magi_tool_runtime::ToolExecutionPolicy;
use magi_worker_runtime::{
    LocalProcessExecutorDescriptor, LocalProcessExecutorHealth, LocalProcessExecutorHealthStatus,
    LocalProcessExecutorProcessModel, LocalProcessProbeResponse, LocalProcessProtocolResponse,
    LocalProcessProtocolResponseKind, LocalProcessWorkerExecutor, ShadowWorkerExecutor,
    WorkerCheckpointResumeMode, WorkerExecutionBindingLifecycle, WorkerExecutionBindingScope,
    WorkerExecutionCheckpointCursor, WorkerExecutionFinalReport, WorkerExecutionIntent,
    WorkerExecutionIntentStep, WorkerExecutionLeaseState, WorkerExecutionMode,
    WorkerExecutionParallelismScope, WorkerExecutionProcessLifecycle, WorkerExecutionProfile,
    WorkerExecutionReusePolicy, WorkerExecutionStepKind, WorkerExecutorFailureLayer,
    WorkerLoopAction, WorkerLoopOutcomeKind, WorkerRuntime, WorkerStage,
};

fn worker_id(value: &str) -> WorkerId {
    WorkerId::new(value.to_string())
}

fn task_id(value: &str) -> TaskId {
    TaskId::new(value.to_string())
}

fn builtin_skill_plan(tool_name: &str) -> SkillToolRuntimePlan {
    SkillToolRuntimePlan {
        skill_ids: vec!["local-process-skill".to_string()],
        tool_policy: ToolExecutionPolicy::default(),
        routing: SkillToolRoutingSummary {
            requested_builtin_tools: vec![tool_name.to_string()],
            requested_bridge_tool_names: Vec::new(),
            requested_bridge_binding_ids: Vec::new(),
            denied_requested_tools: Vec::new(),
        },
        prompt_injections: Vec::new(),
        custom_tool_bindings: Vec::new(),
        bridge_dispatch_plan: BridgeBindingDispatchPlan {
            source_skill_ids: vec!["local-process-skill".to_string()],
            bindings: Vec::new(),
        },
    }
}

#[test]
fn local_process_executor_can_run_execute_chain() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let runtime = WorkerRuntime::new(bus)
        .with_executor(Arc::new(LocalProcessWorkerExecutor::cargo_loopback()));
    let worker_id = worker_id("worker-local-process");
    let task_id = task_id("todo-local-process");
    runtime.register_worker(worker_id.clone());
    runtime.register_execution_intent(WorkerExecutionIntent {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("local-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"subprocess\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::SkillDispatch {
                tool_call_id: ToolCallId::new("local-skill-1".to_string()),
                tool_name: "process_inspect".to_string(),
                plan: builtin_skill_plan("process_inspect"),
                payload: "{\"mode\":\"subprocess-skill\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                working_directory: None,
                route: SkillDispatchRoute::Builtin,
                binding_id: None,
                detail: "subprocess skill dispatch".to_string(),
                status: SkillDispatchStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "local process completed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    });

    let loop_controller = runtime.loop_controller();
    loop_controller.enqueue_action(WorkerLoopAction::Execute {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
    });

    let outcome = loop_controller
        .step()
        .expect("execute outcome should exist");
    assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);
    assert_eq!(
        outcome.report.expect("final report missing").summary,
        "local process completed"
    );

    let summary = runtime.summary();
    assert_eq!(summary.tool_call_count, 1);
    assert_eq!(summary.skill_dispatch_count, 1);
    assert_eq!(summary.report_count, 1);

    let snapshot = runtime.snapshot_for_task(&task_id);
    assert_eq!(snapshot.tool_invocations.len(), 1);
    assert_eq!(snapshot.skill_dispatches.len(), 1);
}

#[test]
fn local_process_executor_resumes_from_checkpoint_without_replaying_completed_steps() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let runtime = WorkerRuntime::new(bus)
        .with_executor(Arc::new(LocalProcessWorkerExecutor::cargo_loopback()));
    let worker_id = worker_id("worker-local-process-resume");
    let task_id = task_id("todo-local-process-resume");
    runtime.register_worker(worker_id.clone());
    runtime.register_execution_intent(WorkerExecutionIntent {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("resume-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"resume-step-1\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::SkillDispatch {
                tool_call_id: ToolCallId::new("resume-skill-1".to_string()),
                tool_name: "process_inspect".to_string(),
                plan: builtin_skill_plan("process_inspect"),
                payload: "{\"mode\":\"resume-step-2\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                working_directory: None,
                route: SkillDispatchRoute::Builtin,
                binding_id: None,
                detail: "resume from checkpoint step 2".to_string(),
                status: SkillDispatchStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "local process resumed from checkpoint".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    });
    runtime.record_branch_checkpoint(
        &task_id,
        &worker_id,
        WorkerStage::Execute,
        None,
        Some(format!("worker-intent-{task_id}")),
        Some(WorkerExecutionBindingLifecycle::Requested),
        Some(WorkerExecutionCheckpointCursor {
            checkpoint_stage: WorkerStage::Execute,
            next_step_index: 1,
            checkpoint_at: magi_core::UtcMillis::now(),
            resume_mode: WorkerCheckpointResumeMode::StepCheckpoint,
            resume_token: None,
        }),
    );

    let loop_controller = runtime.loop_controller();
    loop_controller.enqueue_action(WorkerLoopAction::Execute {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
    });

    let outcome = loop_controller
        .step()
        .expect("execute outcome should exist");
    assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);
    assert_eq!(
        outcome.report.expect("final report missing").summary,
        "local process resumed from checkpoint"
    );

    let summary = runtime.summary();
    assert_eq!(summary.tool_call_count, 0, "已完成 step 不应被重放");
    assert_eq!(
        summary.skill_dispatch_count, 1,
        "恢复后只应执行剩余 skill step"
    );
    assert_eq!(summary.report_count, 1);

    let snapshot = runtime.snapshot_for_task(&task_id);
    assert!(
        snapshot.tool_invocations.is_empty(),
        "首个 tool step 不应重复执行"
    );
    assert_eq!(snapshot.skill_dispatches.len(), 1);
    assert_eq!(
        snapshot.skill_dispatches[0].tool_call_id.to_string(),
        "resume-skill-1"
    );

    let branch_snapshot = runtime
        .branch_snapshot_for_task(&task_id)
        .expect("branch snapshot should remain queryable");
    assert_eq!(branch_snapshot.stage, WorkerStage::Finish);
    assert!(
        branch_snapshot.checkpoint_cursor.is_none(),
        "执行完成后 checkpoint 应被清空"
    );

    let durable_snapshot = runtime.durable_snapshot();
    assert_eq!(durable_snapshot.branches.len(), 1);
    assert!(
        durable_snapshot.branches[0].checkpoint_cursor.is_none(),
        "durable snapshot 不应保留已完成 branch 的 checkpoint"
    );
}

#[test]
fn local_process_executor_probe_reports_capability_and_health() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let probe = executor.probe().expect("probe should succeed");
    assert_eq!(probe.executor_id, "shadow-local-process-worker-executor");
    assert_eq!(
        probe.executor_version,
        "worker-shadow-local-process-executor-v2"
    );
    assert_eq!(
        probe.executor_kind,
        magi_worker_runtime::WorkerExecutorKind::LocalProcess
    );
    assert_eq!(
        probe.capability.execution_mode,
        WorkerExecutionMode::LocalProcess
    );
    assert!(probe.capability.supports_probe);
    assert!(probe.capability.supports_execute);
    assert!(probe.capability.supports_review);
    assert!(probe.capability.supports_verify);
    assert!(probe.capability.supports_repair);
    assert!(probe.capability.affinity.session_id.is_none());
    assert!(probe.capability.affinity.workspace_id.is_none());
    assert!(!probe.capability.affinity.strict_session);
    assert!(!probe.capability.affinity.strict_workspace);
    assert!(probe.capability.stage_matrix.execute);
    assert!(probe.capability.stage_matrix.review);
    assert!(probe.capability.stage_matrix.verify);
    assert!(probe.capability.stage_matrix.repair);
    assert_eq!(
        probe.capability.descriptor.process_model,
        LocalProcessExecutorProcessModel::OneShotSubprocess
    );
    assert_eq!(
        probe.capability.descriptor.reuse_scope,
        WorkerExecutionBindingScope::None
    );
    assert_eq!(
        probe.capability.descriptor.parallelism_scope,
        WorkerExecutionParallelismScope::Executor
    );
    assert_eq!(probe.capability.descriptor.max_parallelism, 1);
    assert!(probe.capability.descriptor.executor_instance_id.is_none());
    assert!(probe.capability.descriptor.executor_lease_id.is_none());
    assert!(
        probe
            .capability
            .supported_step_kinds
            .contains(&WorkerExecutionStepKind::BuiltinToolInvocation)
    );
    assert!(
        probe
            .capability
            .supported_step_kinds
            .contains(&WorkerExecutionStepKind::SkillDispatch)
    );
    assert!(
        probe
            .capability
            .supported_step_kinds
            .contains(&WorkerExecutionStepKind::FinalReport)
    );
    assert_eq!(
        probe.health.status,
        LocalProcessExecutorHealthStatus::Healthy
    );
}

#[test]
fn local_process_executor_exposes_identity_and_step_capability_override() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_EXECUTOR_ID", "shadow-local-process-test")
        .with_env(
            "MAGI_LOCAL_WORKER_EXECUTOR_VERSION",
            "worker-shadow-local-process-test-v9",
        )
        .with_env("MAGI_LOCAL_WORKER_SUPPORTED_STEP_KINDS", "final-report")
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_MAX_PARALLELISM", "3");

    let probe = executor.probe().expect("probe should succeed");
    assert_eq!(probe.executor_id, "shadow-local-process-test");
    assert_eq!(
        probe.executor_version,
        "worker-shadow-local-process-test-v9"
    );
    assert_eq!(
        probe.capability.supported_step_kinds,
        vec![WorkerExecutionStepKind::FinalReport]
    );
    assert_eq!(
        probe.capability.descriptor.process_model,
        LocalProcessExecutorProcessModel::PersistentProcess
    );
    assert_eq!(
        probe.capability.descriptor.reuse_scope,
        WorkerExecutionBindingScope::Session
    );
    assert_eq!(
        probe.capability.descriptor.parallelism_scope,
        WorkerExecutionParallelismScope::Executor
    );
    assert_eq!(probe.capability.descriptor.max_parallelism, 3);
    assert_eq!(
        probe.capability.descriptor.executor_instance_id.as_deref(),
        Some("shadow-local-process-test-instance-1")
    );
    assert!(probe.capability.descriptor.executor_lease_id.is_none());
    assert_eq!(
        probe.capability.descriptor.lease_state,
        WorkerExecutionLeaseState::None
    );
    assert_eq!(
        probe.capability.descriptor.binding_lifecycle,
        WorkerExecutionBindingLifecycle::None
    );
    assert_eq!(
        probe.capability.descriptor.process_lifecycle,
        WorkerExecutionProcessLifecycle::Persistent
    );
}

#[test]
fn local_process_executor_rejects_affinity_mismatch() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_SESSION_ID", "session-affine")
        .with_env("MAGI_LOCAL_WORKER_WORKSPACE_ID", "workspace-affine");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-affinity-mismatch"),
            task_id: task_id("todo-affinity-mismatch"),
            session_id: Some(SessionId::new("session-other")),
            workspace_id: Some(WorkspaceId::new("workspace-other")),
            execution_profile: WorkerExecutionProfile::default(),
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        })
        .expect_err("affinity mismatch should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(error.message.contains("affinity mismatch"));
}

#[test]
fn local_process_executor_rejects_missing_step_capability() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_SUPPORTED_STEP_KINDS", "final-report");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-missing-step-cap"),
            task_id: task_id("todo-missing-step-cap"),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            steps: vec![
                WorkerExecutionIntentStep::BuiltinToolInvocation {
                    tool_call_id: ToolCallId::new("missing-step-tool-1".to_string()),
                    tool_name: "process_inspect".to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: "{\"mode\":\"shadow\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    status: ExecutionResultStatus::Succeeded,
                },
                WorkerExecutionIntentStep::SkillDispatch {
                    tool_call_id: ToolCallId::new("missing-step-skill-1".to_string()),
                    tool_name: "process_inspect".to_string(),
                    plan: builtin_skill_plan("process_inspect"),
                    payload: "{\"mode\":\"shadow-skill\"}".to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                    working_directory: None,
                    route: SkillDispatchRoute::Builtin,
                    binding_id: None,
                    detail: "missing step capability".to_string(),
                    status: SkillDispatchStatus::Succeeded,
                },
                WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                }),
            ],
        })
        .expect_err("missing step capability should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(error.message.contains("missing required steps"));
    assert!(
        error
            .detail
            .as_ref()
            .expect("failure detail missing")
            .missing_step_kinds
            .contains(&WorkerExecutionStepKind::BuiltinToolInvocation)
    );
}

#[test]
fn worker_runtime_loop_rejects_missing_step_capability_before_fallback() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let runtime = WorkerRuntime::new(bus).with_executor(Arc::new(
        LocalProcessWorkerExecutor::cargo_loopback()
            .with_env("MAGI_LOCAL_WORKER_SUPPORTED_STEP_KINDS", "final-report"),
    ));
    let worker_id = worker_id("worker-loop-reject");
    let task_id = task_id("todo-loop-reject");
    runtime.register_worker(worker_id.clone());
    runtime.register_execution_intent(WorkerExecutionIntent {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("reject-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"reject\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::SkillDispatch {
                tool_call_id: ToolCallId::new("reject-skill-1".to_string()),
                tool_name: "process_inspect".to_string(),
                plan: builtin_skill_plan("process_inspect"),
                payload: "{\"mode\":\"reject-skill\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                working_directory: None,
                route: SkillDispatchRoute::Builtin,
                binding_id: None,
                detail: "reject capability".to_string(),
                status: SkillDispatchStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "should not execute".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    });

    let loop_controller = runtime.loop_controller();
    loop_controller.enqueue_action(WorkerLoopAction::Execute {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
    });

    let outcome = loop_controller
        .step()
        .expect("execute outcome should exist");
    assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Rejected);
    assert!(
        outcome
            .rejection_reason
            .expect("rejection reason missing")
            .contains("executor capability insufficient")
    );
    assert!(runtime.snapshot_for_task(&task_id).reports.is_empty());
}

#[test]
fn local_process_executor_reports_remote_business_failures() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-empty-steps"),
            task_id: task_id("todo-empty-steps"),
            session_id: None,
            workspace_id: None,
            execution_profile: WorkerExecutionProfile::default(),
            steps: Vec::new(),
        })
        .expect_err("empty execute intent should be rejected by remote business layer");
    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
}

#[test]
fn local_process_executor_reports_protocol_failures() {
    let executor = LocalProcessWorkerExecutor::new("sh")
        .with_args(vec![
            "-c".to_string(),
            "cat >/dev/null; printf '%s' \"$WORKER_RESPONSE\"".to_string(),
        ])
        .with_env("WORKER_RESPONSE", "not-json");
    let error = executor
        .probe()
        .expect_err("invalid stdout should be protocol failure");
    assert_eq!(error.layer, WorkerExecutorFailureLayer::Protocol);
}

#[test]
fn local_process_executor_reports_transport_failures() {
    let executor = LocalProcessWorkerExecutor::new("/path/does/not/exist/worker-executor");
    let error = executor
        .probe()
        .expect_err("missing executable should be transport failure");
    assert_eq!(error.layer, WorkerExecutorFailureLayer::Transport);
}

#[test]
fn local_process_executor_rejects_mismatched_request_id() {
    let response = serde_json::to_string(&LocalProcessProtocolResponse {
        request_id: "probe-123".to_string(),
        kind: LocalProcessProtocolResponseKind::Probe(LocalProcessProbeResponse {
            capability: magi_worker_runtime::LocalProcessExecutorCapability {
                executor_id: "shadow-local-process-worker-executor".to_string(),
                executor_version: "worker-shadow-local-process-executor-v2".to_string(),
                execution_mode: WorkerExecutionMode::LocalProcess,
                protocol_version: "worker-shadow-local-process-v1".to_string(),
                supports_probe: true,
                supports_execute: true,
                supports_review: false,
                supports_verify: false,
                supports_repair: false,
                affinity: magi_worker_runtime::LocalProcessExecutorAffinity::default(),
                stage_matrix: magi_worker_runtime::LocalProcessExecutorStageMatrix {
                    execute: true,
                    review: false,
                    verify: false,
                    repair: false,
                },
                descriptor: LocalProcessExecutorDescriptor {
                    process_model: LocalProcessExecutorProcessModel::OneShotSubprocess,
                    reuse_scope: WorkerExecutionBindingScope::None,
                    parallelism_scope: WorkerExecutionParallelismScope::Executor,
                    lease_state: WorkerExecutionLeaseState::None,
                    binding_lifecycle: WorkerExecutionBindingLifecycle::None,
                    process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
                    max_parallelism: 1,
                    executor_instance_id: None,
                    executor_lease_id: None,
                },
                supported_step_kinds: vec![
                    WorkerExecutionStepKind::BuiltinToolInvocation,
                    WorkerExecutionStepKind::SkillDispatch,
                    WorkerExecutionStepKind::FinalReport,
                ],
            },
            health: LocalProcessExecutorHealth {
                status: LocalProcessExecutorHealthStatus::Unavailable,
                detail: "loopback unhealthy".to_string(),
            },
        }),
    })
    .expect("response serialization should succeed");
    let executor = LocalProcessWorkerExecutor::new("sh")
        .with_args(vec![
            "-c".to_string(),
            "cat >/dev/null; printf '%s' \"$WORKER_RESPONSE\"".to_string(),
        ])
        .with_env("WORKER_RESPONSE", response);
    let error = executor
        .probe()
        .expect_err("mismatched request id should be protocol failure");
    assert_eq!(error.layer, WorkerExecutorFailureLayer::Protocol);
}

#[test]
fn local_process_executor_rejects_missing_reusable_session_support() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_SUPPORTS_REUSABLE_SESSION", "false");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-reusable-session"),
            task_id: task_id("todo-reusable-session"),
            session_id: Some(SessionId::new("session-reusable")),
            workspace_id: None,
            execution_profile: WorkerExecutionProfile {
                reuse_policy: WorkerExecutionReusePolicy::Required,
                binding_scope: WorkerExecutionBindingScope::Session,
                lease_state: WorkerExecutionLeaseState::Requested,
                binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
                process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
                requested_process_model: Some(LocalProcessExecutorProcessModel::OneShotSubprocess),
                requested_parallelism: 1,
            },
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        })
        .expect_err("missing reusable session support should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(error.message.contains("reusable session"));
}

#[test]
fn local_process_executor_rejects_parallelism_overflow() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_MAX_PARALLELISM", "1");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-parallelism-overflow"),
            task_id: task_id("todo-parallelism-overflow"),
            session_id: Some(SessionId::new("session-parallelism")),
            workspace_id: Some(WorkspaceId::new("workspace-parallelism")),
            execution_profile: WorkerExecutionProfile {
                reuse_policy: WorkerExecutionReusePolicy::Preferred,
                binding_scope: WorkerExecutionBindingScope::Workspace,
                lease_state: WorkerExecutionLeaseState::Requested,
                binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
                process_lifecycle: WorkerExecutionProcessLifecycle::OneShot,
                requested_process_model: Some(LocalProcessExecutorProcessModel::OneShotSubprocess),
                requested_parallelism: 2,
            },
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        })
        .expect_err("parallelism overflow should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(error.message.contains("parallelism mismatch"));
}

#[test]
fn local_process_executor_rejects_binding_scope_mismatch() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-binding-scope"),
            task_id: task_id("todo-binding-scope"),
            session_id: Some(SessionId::new("session-binding")),
            workspace_id: Some(WorkspaceId::new("workspace-binding")),
            execution_profile: WorkerExecutionProfile {
                reuse_policy: WorkerExecutionReusePolicy::Required,
                binding_scope: WorkerExecutionBindingScope::Workspace,
                lease_state: WorkerExecutionLeaseState::Requested,
                binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
                process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
                requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
                requested_parallelism: 1,
            },
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        })
        .expect_err("binding scope mismatch should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(error.message.contains("binding scope mismatch"));
    let detail = error.detail.expect("binding scope failure detail missing");
    assert_eq!(
        detail.effective_reuse_scope,
        Some(WorkerExecutionBindingScope::Session)
    );
    assert_eq!(
        detail
            .requested_execution_profile
            .expect("requested profile missing")
            .binding_scope,
        WorkerExecutionBindingScope::Workspace
    );
}

#[test]
fn local_process_executor_rejects_session_binding_without_session_context() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session");

    let error = executor
        .execute_checked(&WorkerExecutionIntent {
            worker_id: worker_id("worker-missing-session-context"),
            task_id: task_id("todo-missing-session-context"),
            session_id: None,
            workspace_id: Some(WorkspaceId::new("workspace-binding")),
            execution_profile: WorkerExecutionProfile {
                reuse_policy: WorkerExecutionReusePolicy::Required,
                binding_scope: WorkerExecutionBindingScope::Session,
                lease_state: WorkerExecutionLeaseState::Requested,
                binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
                process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
                requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
                requested_parallelism: 1,
            },
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "should not run".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        })
        .expect_err("session binding without session_id should be rejected");

    assert_eq!(error.layer, WorkerExecutorFailureLayer::RemoteBusiness);
    assert!(
        error
            .message
            .contains("session binding requires session_id")
    );
}

#[test]
fn worker_runtime_records_active_lease_for_persistent_session_binding() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let runtime = WorkerRuntime::new(bus).with_executor(Arc::new(
        LocalProcessWorkerExecutor::cargo_loopback()
            .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
            .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session"),
    ));
    let worker_id = worker_id("worker-persistent-lease");
    let task_id = task_id("todo-persistent-lease");
    let session_id = SessionId::new("session-persistent-lease");
    runtime.register_worker(worker_id.clone());
    runtime.register_execution_intent(WorkerExecutionIntent {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
        session_id: Some(session_id.clone()),
        workspace_id: None,
        execution_profile: WorkerExecutionProfile {
            reuse_policy: WorkerExecutionReusePolicy::Required,
            binding_scope: WorkerExecutionBindingScope::Session,
            lease_state: WorkerExecutionLeaseState::Requested,
            binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
            process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
            requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
            requested_parallelism: 1,
        },
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "persistent lease path".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    });

    let loop_controller = runtime.loop_controller();
    loop_controller.enqueue_action(WorkerLoopAction::Execute {
        worker_id: worker_id.clone(),
        task_id: task_id.clone(),
    });
    let outcome = loop_controller
        .step()
        .expect("execute outcome should exist");
    assert_eq!(outcome.kind, WorkerLoopOutcomeKind::Applied);

    let observations = runtime.executor_observations();
    let observation = observations
        .last()
        .expect("persistent lease observation should exist");
    assert_eq!(
        observation.requested_lease_state,
        Some(WorkerExecutionLeaseState::Requested)
    );
    assert_eq!(
        observation.requested_binding_lifecycle,
        Some(WorkerExecutionBindingLifecycle::Requested)
    );
    assert_eq!(
        observation.requested_process_lifecycle,
        Some(WorkerExecutionProcessLifecycle::Persistent)
    );
    assert_eq!(
        observation.lease_state,
        Some(WorkerExecutionLeaseState::Active)
    );
    assert_eq!(
        observation.binding_lifecycle,
        Some(WorkerExecutionBindingLifecycle::Bound)
    );
    assert_eq!(
        observation.process_lifecycle,
        Some(WorkerExecutionProcessLifecycle::Persistent)
    );
    assert_eq!(
        observation.executor_instance_id.as_deref(),
        Some("shadow-local-process-worker-executor-instance-1")
    );
    assert_eq!(
        observation.executor_lease_id.as_deref(),
        Some("shadow-local-process-worker-executor-session-session-persistent-lease-lease-1")
    );
}

#[test]
fn persistent_executor_reuses_same_lease_for_same_session_binding() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session");
    let intent = WorkerExecutionIntent {
        worker_id: worker_id("worker-reuse-same-session"),
        task_id: task_id("todo-reuse-same-session"),
        session_id: Some(SessionId::new("session-reuse-same")),
        workspace_id: None,
        execution_profile: WorkerExecutionProfile {
            reuse_policy: WorkerExecutionReusePolicy::Required,
            binding_scope: WorkerExecutionBindingScope::Session,
            lease_state: WorkerExecutionLeaseState::Requested,
            binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
            process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
            requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
            requested_parallelism: 1,
        },
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "reuse same session".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    };

    let request_a = intent.executor_request(magi_worker_runtime::WorkerStage::Execute, "probe-a");
    let request_b = intent.executor_request(magi_worker_runtime::WorkerStage::Execute, "probe-b");
    let probe_a = executor
        .probe_for_request(Some(&request_a))
        .expect("first probe should succeed");
    let probe_b = executor
        .probe_for_request(Some(&request_b))
        .expect("second probe should succeed");

    assert_eq!(
        probe_a.capability.descriptor.executor_instance_id,
        probe_b.capability.descriptor.executor_instance_id
    );
    assert_eq!(
        probe_a.capability.descriptor.executor_lease_id,
        probe_b.capability.descriptor.executor_lease_id
    );
    assert_eq!(
        probe_b.capability.descriptor.lease_state,
        WorkerExecutionLeaseState::Active
    );
    assert_eq!(
        probe_b.capability.descriptor.binding_lifecycle,
        WorkerExecutionBindingLifecycle::Bound
    );
}

#[test]
fn persistent_executor_allocates_distinct_leases_for_distinct_session_bindings() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session");
    let base_profile = WorkerExecutionProfile {
        reuse_policy: WorkerExecutionReusePolicy::Required,
        binding_scope: WorkerExecutionBindingScope::Session,
        lease_state: WorkerExecutionLeaseState::Requested,
        binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
        process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
        requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
        requested_parallelism: 1,
    };

    let request_a = WorkerExecutionIntent {
        worker_id: worker_id("worker-session-a"),
        task_id: task_id("todo-session-a"),
        session_id: Some(SessionId::new("session-a")),
        workspace_id: None,
        execution_profile: base_profile.clone(),
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "a".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    }
    .executor_request(magi_worker_runtime::WorkerStage::Execute, "probe-a");
    let request_b = WorkerExecutionIntent {
        worker_id: worker_id("worker-session-b"),
        task_id: task_id("todo-session-b"),
        session_id: Some(SessionId::new("session-b")),
        workspace_id: None,
        execution_profile: base_profile,
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "b".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    }
    .executor_request(magi_worker_runtime::WorkerStage::Execute, "probe-b");

    let probe_a = executor
        .probe_for_request(Some(&request_a))
        .expect("session a probe should succeed");
    let probe_b = executor
        .probe_for_request(Some(&request_b))
        .expect("session b probe should succeed");

    assert_ne!(
        probe_a.capability.descriptor.executor_lease_id,
        probe_b.capability.descriptor.executor_lease_id
    );
}

#[test]
fn persistent_executor_releases_lease_and_reallocates_on_next_request() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback()
        .with_env("MAGI_LOCAL_WORKER_PROCESS_MODEL", "persistent-process")
        .with_env("MAGI_LOCAL_WORKER_REUSE_SCOPE", "session");
    let mut profile = WorkerExecutionProfile {
        reuse_policy: WorkerExecutionReusePolicy::Required,
        binding_scope: WorkerExecutionBindingScope::Session,
        lease_state: WorkerExecutionLeaseState::Requested,
        binding_lifecycle: WorkerExecutionBindingLifecycle::Requested,
        process_lifecycle: WorkerExecutionProcessLifecycle::Persistent,
        requested_process_model: Some(LocalProcessExecutorProcessModel::PersistentProcess),
        requested_parallelism: 1,
    };
    let build_request = |request_source: &str, profile: WorkerExecutionProfile| {
        WorkerExecutionIntent {
            worker_id: worker_id("worker-release"),
            task_id: task_id("todo-release"),
            session_id: Some(SessionId::new("session-release")),
            workspace_id: None,
            execution_profile: profile,
            steps: vec![WorkerExecutionIntentStep::FinalReport(
                WorkerExecutionFinalReport {
                    summary: "release".to_string(),
                    result_kind: Some(TaskResultKind::Success),
                    termination_reason: Some(TerminationReason::Completed),
                    verification_status: VerificationStatus::Passed,
                },
            )],
        }
        .executor_request(magi_worker_runtime::WorkerStage::Execute, request_source)
    };

    let first = executor
        .probe_for_request(Some(&build_request("acquire-1", profile.clone())))
        .expect("initial acquire should succeed");
    let first_lease = first
        .capability
        .descriptor
        .executor_lease_id
        .clone()
        .expect("initial lease should exist");

    profile.lease_state = WorkerExecutionLeaseState::Released;
    profile.binding_lifecycle = WorkerExecutionBindingLifecycle::Released;
    let released = executor
        .probe_for_request(Some(&build_request("release", profile.clone())))
        .expect("release should succeed");
    assert_eq!(
        released.capability.descriptor.lease_state,
        WorkerExecutionLeaseState::Released
    );

    profile.lease_state = WorkerExecutionLeaseState::Requested;
    profile.binding_lifecycle = WorkerExecutionBindingLifecycle::Requested;
    let reacquired = executor
        .probe_for_request(Some(&build_request("acquire-2", profile)))
        .expect("reacquire should succeed");
    let second_lease = reacquired
        .capability
        .descriptor
        .executor_lease_id
        .clone()
        .expect("reacquired lease should exist");

    assert_ne!(first_lease, second_lease);
    assert_eq!(
        reacquired.capability.descriptor.lease_state,
        WorkerExecutionLeaseState::Active
    );
}

#[test]
fn local_process_executor_can_run_review_through_subprocess() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let intent = WorkerExecutionIntent {
        worker_id: worker_id("worker-review-subprocess"),
        task_id: task_id("todo-review-subprocess"),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("review-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"review\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "review stage passed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    };

    let (trace, summary) = executor
        .review(&intent, None)
        .expect("review through subprocess should succeed");
    assert_eq!(trace.worker_id, worker_id("worker-review-subprocess"));
    assert_eq!(trace.tool_invocations.len(), 1);
    assert!(!summary.is_empty());
}

#[test]
fn local_process_executor_can_run_verify_through_subprocess() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let intent = WorkerExecutionIntent {
        worker_id: worker_id("worker-verify-subprocess"),
        task_id: task_id("todo-verify-subprocess"),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("verify-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"verify\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "verify stage passed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    };

    let (trace, status, summary) = executor
        .verify(&intent, None)
        .expect("verify through subprocess should succeed");
    assert_eq!(trace.worker_id, worker_id("worker-verify-subprocess"));
    assert_eq!(trace.tool_invocations.len(), 1);
    assert_eq!(status, VerificationStatus::Passed);
    assert!(!summary.is_empty());
}

#[test]
fn local_process_executor_can_run_repair_through_subprocess() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let intent = WorkerExecutionIntent {
        worker_id: worker_id("worker-repair-subprocess"),
        task_id: task_id("todo-repair-subprocess"),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![
            WorkerExecutionIntentStep::BuiltinToolInvocation {
                tool_call_id: ToolCallId::new("repair-tool-1".to_string()),
                tool_name: "process_inspect".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "{\"mode\":\"repair\"}".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                status: ExecutionResultStatus::Succeeded,
            },
            WorkerExecutionIntentStep::FinalReport(WorkerExecutionFinalReport {
                summary: "repair stage passed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            }),
        ],
    };

    let (trace, summary) = executor
        .repair(&intent, None, "verification failed: timeout exceeded")
        .expect("repair through subprocess should succeed");
    assert_eq!(trace.worker_id, worker_id("worker-repair-subprocess"));
    assert_eq!(trace.tool_invocations.len(), 1);
    assert!(summary.contains("repair"));
}

#[test]
fn local_process_executor_review_verify_repair_with_prior_trace() {
    let executor = LocalProcessWorkerExecutor::cargo_loopback();
    let intent = WorkerExecutionIntent {
        worker_id: worker_id("worker-prior-trace"),
        task_id: task_id("todo-prior-trace"),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "prior trace test".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    };

    // Execute first to get a prior trace
    let execute_trace = executor.execute(&intent);
    assert_eq!(execute_trace.worker_id, worker_id("worker-prior-trace"));

    // Review with prior trace
    let (review_trace, review_summary) = executor
        .review(&intent, Some(&execute_trace))
        .expect("review with prior trace should succeed");
    assert_eq!(review_trace.worker_id, worker_id("worker-prior-trace"));
    assert!(!review_summary.is_empty());

    // Verify with prior trace
    let (verify_trace, verify_status, verify_summary) = executor
        .verify(&intent, Some(&review_trace))
        .expect("verify with prior trace should succeed");
    assert_eq!(verify_trace.worker_id, worker_id("worker-prior-trace"));
    assert_eq!(verify_status, VerificationStatus::Passed);
    assert!(!verify_summary.is_empty());

    // Repair with prior trace
    let (repair_trace, repair_summary) = executor
        .repair(&intent, Some(&verify_trace), "test repair reason")
        .expect("repair with prior trace should succeed");
    assert_eq!(repair_trace.worker_id, worker_id("worker-prior-trace"));
    assert!(repair_summary.contains("repair"));
}

#[test]
fn worker_runtime_loop_can_run_review_verify_repair_stages() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let runtime = WorkerRuntime::new(bus)
        .with_executor(Arc::new(LocalProcessWorkerExecutor::cargo_loopback()));
    let wid = worker_id("worker-full-lifecycle");
    let tid = task_id("todo-full-lifecycle");
    runtime.register_worker(wid.clone());
    runtime.register_execution_intent(WorkerExecutionIntent {
        worker_id: wid.clone(),
        task_id: tid.clone(),
        session_id: None,
        workspace_id: None,
        execution_profile: WorkerExecutionProfile::default(),
        steps: vec![WorkerExecutionIntentStep::FinalReport(
            WorkerExecutionFinalReport {
                summary: "full lifecycle test".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
            },
        )],
    });

    let loop_controller = runtime.loop_controller();

    // Execute stage
    loop_controller.enqueue_action(WorkerLoopAction::Execute {
        worker_id: wid.clone(),
        task_id: tid.clone(),
    });
    let execute_outcome = loop_controller
        .step()
        .expect("execute outcome should exist");
    assert_eq!(execute_outcome.kind, WorkerLoopOutcomeKind::Applied);

    // Review stage
    loop_controller.enqueue_action(WorkerLoopAction::Review {
        worker_id: wid.clone(),
        summary: "review after execute".to_string(),
    });
    let review_outcome = loop_controller.step().expect("review outcome should exist");
    assert_eq!(review_outcome.kind, WorkerLoopOutcomeKind::Applied);

    // Verify stage
    loop_controller.enqueue_action(WorkerLoopAction::Verify {
        worker_id: wid.clone(),
        verification_status: VerificationStatus::Passed,
        summary: "verification passed".to_string(),
    });
    let verify_outcome = loop_controller.step().expect("verify outcome should exist");
    assert_eq!(verify_outcome.kind, WorkerLoopOutcomeKind::Applied);

    // Repair stage
    loop_controller.enqueue_action(WorkerLoopAction::Repair {
        worker_id: wid.clone(),
        summary: "repair after verify".to_string(),
    });
    let repair_outcome = loop_controller.step().expect("repair outcome should exist");
    assert_eq!(repair_outcome.kind, WorkerLoopOutcomeKind::Applied);

    let summary = runtime.summary();
    assert!(summary.report_count >= 4);
}

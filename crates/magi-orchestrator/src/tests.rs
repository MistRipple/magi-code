
use super::*;
use crate::task_store::TaskStore;
use magi_core::{ApprovalRequirement, RiskLevel, Task, TaskKind, ToolCallId, WorkerId};
use magi_event_bus::InMemoryEventBus;
use magi_governance::{DecisionPhase, GovernanceService, WorkerControlKind};
use magi_skill_runtime::{SkillDispatchRoute, SkillDispatchRuntime};
use magi_tool_runtime::{ToolExecutionSummary, ToolRegistry};
use magi_worker_runtime::{
    LocalProcessExecutorCapability, LocalProcessExecutorHealth,
    LocalProcessExecutorHealthStatus, LocalProcessWorkerExecutor, ShadowWorkerExecutor,
    SkillDispatchSummary, WorkerExecutionFinalReport, WorkerExecutionReport,
    WorkerExecutionStepKind, WorkerExecutionTrace, WorkerExecutorFailure, WorkerExecutorKind,
    WorkerExecutorProbe, WorkerRuntime, WorkerSkillDispatchObservation, WorkerStage,
};
use std::sync::{Arc, Mutex};

fn dispatch_target(decision: DispatchDecision) -> TaskExecutionTarget {
    let mission_id = decision.mission_id.clone();
    TaskExecutionTarget {
        mission_id,
        root_task_id: root_task_id_for_mission(&decision.mission_id),
        task_id: decision.task_id,
        requested_worker_id: None,
        recovery_id: None,
        execution_chain_ref: None,
    }
}

fn root_task_id_for_mission(mission_id: &MissionId) -> TaskId {
    TaskId::new(format!("task-root-{}", mission_id.as_str()))
}

fn build_execution_runtime_with_task_store(
    service: &OrchestratorService,
    worker_runtime: WorkerRuntime,
    tool_registry: ToolRegistry,
    skill_runtime: SkillDispatchRuntime,
) -> (OrchestratedExecutionRuntime, Arc<TaskStore>) {
    let task_store = Arc::new(TaskStore::new());
    let runtime = service
        .execution_runtime(worker_runtime, tool_registry, skill_runtime)
        .with_task_store(Arc::clone(&task_store));
    (runtime, task_store)
}

fn seed_action_tasks(
    task_store: &TaskStore,
    mission_id: &MissionId,
    mission_title: &str,
    tasks: &[(TaskId, &str, TaskStatus)],
) -> TaskId {
    let root_task_id = root_task_id_for_mission(mission_id);
    let now = UtcMillis::now();
    task_store.insert_task(Task {
        task_id: root_task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id.clone(),
        parent_task_id: None,
        kind: TaskKind::Objective,
        title: mission_title.to_string(),
        goal: mission_title.to_string(),
        status: TaskStatus::Running,
        dependency_ids: Vec::new(),
        required_children: tasks.iter().map(|(task_id, _, _)| task_id.clone()).collect(),
        policy_snapshot: None,
        executor_binding: None,
        context_refs: Vec::new(),
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        repair_count: 0,
        decision_payload: None,
        created_at: now,
        updated_at: now,
    });
    for (task_id, title, status) in tasks {
        task_store.insert_task(Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: TaskKind::Action,
            title: (*title).to_string(),
            goal: (*title).to_string(),
            status: status.clone(),
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            context_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            repair_count: 0,
            decision_payload: None,
            created_at: now,
            updated_at: now,
        });
    }
    root_task_id
}

fn direct_execution_target(mission_id: &MissionId, task_id: &TaskId) -> TaskExecutionTarget {
    TaskExecutionTarget {
        mission_id: mission_id.clone(),
        root_task_id: root_task_id_for_mission(mission_id),
        task_id: task_id.clone(),
        requested_worker_id: None,
        recovery_id: None,
        execution_chain_ref: None,
    }
}

fn worker_summary(skill_dispatch_count: usize) -> WorkerRuntimeSummary {
    WorkerRuntimeSummary {
        total_workers: 1,
        active_workers: 1,
        finished_workers: 0,
        failed_workers: 0,
        report_count: 1,
        tool_call_count: 0,
        skill_dispatch_count,
        executor_observation_count: 0,
        latest_executor_status: None,
        governance_count: 0,
        governance_summary: WorkerGovernanceSummary::default(),
        skill_dispatch_summary: SkillDispatchSummary {
            total_dispatches: skill_dispatch_count,
            builtin_dispatches: skill_dispatch_count,
            bridge_dispatches: 0,
            succeeded_dispatches: skill_dispatch_count,
            rejected_dispatches: 0,
            failed_dispatches: 0,
        },
    }
}

fn skill_dispatch_observation(task_id: &TaskId) -> WorkerSkillDispatchObservation {
    WorkerSkillDispatchObservation {
        worker_id: WorkerId::new("worker-1"),
        task_id: task_id.clone(),
        tool_call_id: ToolCallId::new("tool-call-1"),
        tool_name: "search_text".to_string(),
        route: Some(SkillDispatchRoute::Builtin),
        binding_id: None,
        status: magi_skill_runtime::SkillDispatchStatus::Succeeded,
        detail: "ok".to_string(),
        observed_at: UtcMillis::now(),
    }
}

fn governance_observation(
    task_id: &TaskId,
    action: WorkerControlKind,
    outcome: magi_governance::GovernanceOutcome,
    allowed: bool,
    requires_approval: bool,
    phase: DecisionPhase,
    reason: Option<&str>,
) -> WorkerGovernanceObservation {
    WorkerGovernanceObservation {
        worker_id: WorkerId::new("worker-1"),
        task_id: Some(task_id.clone()),
        action,
        decision: GovernanceDecision {
            outcome,
            allowed,
            requires_approval,
            phase,
            threshold: RiskLevel::Medium,
            reason: reason.map(|value| value.to_string()),
        },
        observed_at: UtcMillis::now(),
    }
}

fn build_recovery_execution_fixture(
    with_recovery_support: bool,
) -> (
    OrchestratedExecutionRuntime,
    RecoveryResumeInput,
    WorkerId,
    TaskId,
) {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let governance = Arc::new(GovernanceService::default());
    let service =
        OrchestratorService::with_governance(Arc::clone(&event_bus), Arc::clone(&governance));
    let session_store = Arc::new(magi_session_store::SessionStore::new());
    let workspace_store = Arc::new(magi_workspace::WorkspaceStore::new());

    let mission_id = MissionId::new("mission-recovery-hook");
    let task_id = TaskId::new("todo-recovery-hook");
    let session_id = SessionId::new("session-recovery-hook");
    let workspace_id = WorkspaceId::new("workspace-recovery-hook");
    let worker_id = WorkerId::new("worker-recovery-hook");

    session_store
        .create_session(session_id.clone(), "session")
        .expect("session should be creatable");
    session_store.bind_execution_ownership(
        session_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-1".to_string()),
        },
    );

    workspace_store
        .register(
            workspace_id.clone(),
            magi_core::AbsolutePath::new("/Users/xie/code/magi"),
        )
        .expect("workspace should be creatable");
    let recovery_handle = workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-1".to_string()),
        },
        "snapshot-recovery-hook",
        "recovery-recovery-hook",
        Some("resume".to_string()),
    );
    workspace_store
        .mark_recovery_ready(&recovery_handle.recovery_id)
        .expect("recovery should be ready");

    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));
    let task_store = Arc::new(TaskStore::new());
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Blocked)],
    );
    let execution_runtime = if with_recovery_support {
        service
            .execution_runtime_with_recovery_support(
            worker_runtime,
            tool_registry,
            skill_runtime,
            session_store,
            workspace_store.clone(),
        )
            .with_task_store(Arc::clone(&task_store))
    } else {
        service
            .execution_runtime(worker_runtime, tool_registry, skill_runtime)
            .with_task_store(Arc::clone(&task_store))
    };

    let recovery_input = workspace_store
        .build_recovery_resume_input(&recovery_handle.recovery_id)
        .expect("recovery input should be buildable");

    (execution_runtime, recovery_input, worker_id, task_id)
}

#[test]
fn execution_overview_exports_context_consumption_into_runtime_read_model() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(event_bus.clone());
    let control = service.control_plane();

    let mission_id = MissionId::new("mission-context");
    let assignment_id = AssignmentId::new("assignment-context");
    let task_id = TaskId::new("todo-context");

    control
        .execute(OrchestratorCommand::CreateMission {
            mission_id: mission_id.clone(),
            title: "mission".to_string(),
        })
        .expect("mission should be created");
    control
        .execute(OrchestratorCommand::AddAssignment {
            mission_id: mission_id.clone(),
            assignment_id: assignment_id.clone(),
            title: "assignment".to_string(),
        })
        .expect("assignment should be added");
    control
        .execute(OrchestratorCommand::CreateTask {
            mission_id: mission_id.clone(),
            assignment_id,
            task_id: task_id.clone(),
            title: "task".to_string(),
        })
        .expect("task should be added");

    let knowledge_store = magi_knowledge_store::KnowledgeStore::new();
    knowledge_store.ingest_code_index(magi_knowledge_store::CodeIndexIngestion {
        knowledge_id: "kb-code-ctx-1".to_string(),
        title: "Parse manifest".to_string(),
        content: "Parses manifest with governed code index output.".to_string(),
        tags: vec!["parser".to_string()],
        source_ref: Some("knowledge://parser".to_string()),
        updated_at: UtcMillis::now(),
        source: magi_knowledge_store::CodeIndexSource {
            path: "src/parser.rs".to_string(),
            language: Some("rust".to_string()),
            repo_ref: Some("repo".to_string()),
            commit_ref: Some("commit".to_string()),
            start_line: Some(10),
            end_line: Some(42),
            symbol: Some(magi_knowledge_store::CodeIndexSymbol {
                name: "parse_manifest".to_string(),
                kind: magi_knowledge_store::CodeSymbolKind::Function,
                container: None,
                signature: Some("fn parse_manifest(input: &str)".to_string()),
            }),
        },
        audit: Some(magi_knowledge_store::KnowledgeAuditLink {
            audit_event_id: "audit-knowledge-context-1".to_string(),
            trail_ref: Some("audit/trails/knowledge-context.json".to_string()),
            sequence: Some(7),
        }),
        governance: Some(magi_knowledge_store::KnowledgeGovernanceLink {
            outcome: magi_knowledge_store::KnowledgeGovernanceOutcome::Allowed,
            policy_refs: vec!["policy.knowledge.read".to_string()],
            rationale: Some("allowed for runtime context".to_string()),
            audit_event_id: Some("audit-knowledge-context-1".to_string()),
        }),
    });

    let session_id = magi_core::SessionId::new("session-context");
    let memory_store = magi_memory_store::MemoryStore::new();
    memory_store.apply_extraction(magi_memory_store::MemoryExtractionApplyRequest {
        extraction_id: "extract-context-1".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("timeline://context".to_string()),
        summary: "Extracted runtime context memories".to_string(),
        memories: vec![
            magi_memory_store::ExtractedMemory {
                memory_id: "mem-context-2".to_string(),
                layer: magi_memory_store::MemoryLayer::Durable,
                content: "Second memory".to_string(),
                created_at: UtcMillis::now(),
            },
            magi_memory_store::ExtractedMemory {
                memory_id: "mem-context-1".to_string(),
                layer: magi_memory_store::MemoryLayer::Durable,
                content: "First memory".to_string(),
                created_at: UtcMillis::now(),
            },
        ],
        created_at: UtcMillis::now(),
    });
    assert!(
        memory_store
            .verify_extraction_linkage("extract-context-1")
            .expect("memory extraction verification should exist")
            .is_consistent
    );

    let runtime = magi_context_runtime::ContextRuntime::new(knowledge_store, memory_store);
    let context_result = runtime.assemble(
        &magi_context_runtime::ContextBudget {
            max_turns: 4,
            max_knowledge: 2,
            max_memory: 3,
            max_shared_items: 2,
            max_file_summaries: 2,
        },
        magi_context_runtime::ContextAssemblyInput {
            recent_turns: vec!["latest turn".to_string()],
            knowledge_query: magi_knowledge_store::KnowledgeQuery {
                kind: Some(magi_knowledge_store::KnowledgeKind::CodeIndex),
                text: Some("parse_manifest".to_string()),
                tags: vec!["parser".to_string()],
                limit: 5,
            },
            memory_query: magi_memory_store::MemoryQuery {
                session_id: session_id.clone(),
                layer: Some(magi_memory_store::MemoryLayer::Durable),
                limit: 5,
            },
            shared_context: vec![magi_context_runtime::SharedContextItem {
                item_id: "shared-1".to_string(),
                title: "Shared".to_string(),
                content: "shared context".to_string(),
            }],
            file_summaries: vec![magi_context_runtime::FileSummaryItem {
                absolute_path: "/tmp/parser.rs".to_string(),
                summary: "parser summary".to_string(),
            }],
        },
    );
    let context_summary = MissionContextSummary::from_context_assembly(&context_result);

    let overview = match control
        .execute(OrchestratorCommand::BuildMissionExecutionOverview {
            mission_id: mission_id.clone(),
            worker_summary: worker_summary(0),
            tool_summary: ToolExecutionSummary::default(),
            skill_dispatch_observations: vec![],
            governance_observations: vec![],
            context_summary: Some(context_summary.clone()),
        })
        .expect("overview should be built")
    {
        OrchestratorCommandResult::MissionExecutionOverviewBuilt { overview } => overview,
        other => panic!("unexpected result: {other:?}"),
    };

    let overview_context = overview
        .context_summary
        .as_ref()
        .expect("context summary should be exported");
    assert_eq!(overview_context.used_turns, 1);
    assert_eq!(overview_context.used_knowledge, 1);
    assert_eq!(overview_context.used_memory, 2);
    assert_eq!(overview_context.code_index_knowledge_count, 1);
    assert_eq!(overview_context.audited_knowledge_count, 1);
    assert_eq!(overview_context.governed_knowledge_count, 1);
    assert_eq!(overview_context.extracted_memory_count, 2);
    assert_eq!(
        overview_context.knowledge_source_paths,
        vec!["src/parser.rs".to_string()]
    );
    assert_eq!(
        overview_context.memory_extraction_refs,
        vec!["extract-context-1".to_string()]
    );

    let read_model = event_bus.runtime_read_model_input();
    let mission_entry = read_model
        .details
        .missions
        .iter()
        .find(|entry| entry.mission_id == mission_id.to_string())
        .expect("mission entry should exist");
    assert_eq!(mission_entry.context_used_turn_count, 1);
    assert_eq!(mission_entry.context_used_knowledge_count, 1);
    assert_eq!(mission_entry.context_used_memory_count, 2);
    assert_eq!(mission_entry.context_code_index_knowledge_count, 1);
    assert_eq!(mission_entry.context_audited_knowledge_count, 1);
    assert_eq!(mission_entry.context_governed_knowledge_count, 1);
    assert_eq!(mission_entry.context_extracted_memory_count, 2);
    assert_eq!(
        mission_entry.context_knowledge_ids,
        vec!["kb-code-ctx-1".to_string()]
    );
    assert_eq!(
        mission_entry.context_knowledge_source_paths,
        vec!["src/parser.rs".to_string()]
    );
    assert_eq!(
        mission_entry.context_memory_ids,
        vec!["mem-context-1".to_string(), "mem-context-2".to_string()]
    );
    assert_eq!(
        mission_entry.context_memory_extraction_refs,
        vec!["extract-context-1".to_string()]
    );
    assert_eq!(read_model.overview.diagnostics.context_mission_count, 1);
    assert_eq!(read_model.overview.diagnostics.context_used_knowledge_count, 1);
    assert_eq!(read_model.overview.diagnostics.context_used_memory_count, 2);
    assert_eq!(
        read_model
            .overview
            .diagnostics
            .context_code_index_knowledge_count,
        1
    );
    assert_eq!(
        read_model
            .overview
            .diagnostics
            .context_extracted_memory_count,
        2
    );
}

#[test]
fn control_plane_aggregates_governance_summaries_by_layer() {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let service = OrchestratorService::new(event_bus);
    let control = service.control_plane();

    let mission_id = MissionId::new("mission-5");
    let assignment_one = AssignmentId::new("assignment-5a");
    let assignment_two = AssignmentId::new("assignment-5b");
    let task_one = TaskId::new("todo-5a-1");
    let task_two = TaskId::new("todo-5a-2");
    let task_three = TaskId::new("todo-5b-1");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_one.clone(),
        title: "assignment-a".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_two.clone(),
        title: "assignment-b".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_one.clone(),
        task_id: task_one.clone(),
        title: "todo-a-1".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_one.clone(),
        task_id: task_two.clone(),
        title: "todo-a-2".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_two.clone(),
        task_id: task_three.clone(),
        title: "todo-b-1".to_string(),
    });

    let governance_observations = vec![
        governance_observation(
            &task_one,
            WorkerControlKind::Execute,
            magi_governance::GovernanceOutcome::Allowed,
            true,
            false,
            DecisionPhase::WorkerControl,
            None,
        ),
        governance_observation(
            &task_one,
            WorkerControlKind::Review,
            magi_governance::GovernanceOutcome::Blocked,
            false,
            false,
            DecisionPhase::WorkerControl,
            Some("blocked"),
        ),
        governance_observation(
            &task_two,
            WorkerControlKind::Execute,
            magi_governance::GovernanceOutcome::NeedsApproval,
            false,
            true,
            DecisionPhase::ApprovalPolicy,
            Some("approval"),
        ),
        governance_observation(
            &task_two,
            WorkerControlKind::RepairRetry,
            magi_governance::GovernanceOutcome::Allowed,
            true,
            false,
            DecisionPhase::WorkerControl,
            Some("repair retry"),
        ),
        governance_observation(
            &task_three,
            WorkerControlKind::Fail,
            magi_governance::GovernanceOutcome::Rejected,
            false,
            false,
            DecisionPhase::WorkerControl,
            Some("rejected"),
        ),
    ];

    let overview = match control
        .execute(OrchestratorCommand::BuildMissionExecutionOverview {
            mission_id,
            worker_summary: worker_summary(0),
            tool_summary: ToolExecutionSummary::default(),
            skill_dispatch_observations: vec![],
            governance_observations,
            context_summary: None,
        })
        .expect("overview should be built")
    {
        OrchestratorCommandResult::MissionExecutionOverviewBuilt { overview } => overview,
        other => panic!("unexpected result: {other:?}"),
    };

    assert_eq!(overview.governance_summary.total_checks, 5);
    assert_eq!(overview.governance_summary.allowed, 2);
    assert_eq!(overview.governance_summary.needs_approval, 1);
    assert_eq!(overview.governance_summary.rejected, 1);
    assert_eq!(overview.governance_summary.blocked, 1);
    assert_eq!(overview.governance_summary.repair_retry, 1);

    assert_eq!(overview.assignment_governance_summaries.len(), 2);
    assert_eq!(
        overview.assignment_governance_summaries[0]
            .governance_summary
            .total_checks,
        4
    );
    assert_eq!(
        overview.assignment_governance_summaries[0]
            .governance_summary
            .allowed,
        2
    );
    assert_eq!(
        overview.assignment_governance_summaries[0]
            .governance_summary
            .needs_approval,
        1
    );
    assert_eq!(
        overview.assignment_governance_summaries[0]
            .governance_summary
            .blocked,
        1
    );
    assert_eq!(
        overview.assignment_governance_summaries[1]
            .governance_summary
            .total_checks,
        1
    );
    assert_eq!(
        overview.assignment_governance_summaries[1]
            .governance_summary
            .rejected,
        1
    );

    assert_eq!(overview.task_governance_summaries.len(), 3);
    assert_eq!(
        overview.task_governance_summaries[0]
            .governance_summary
            .blocked,
        1
    );
    assert_eq!(
        overview.task_governance_summaries[1]
            .governance_summary
            .repair_retry,
        1
    );
    assert_eq!(
        overview.task_governance_summaries[2]
            .governance_summary
            .rejected,
        1
    );
}

#[test]
fn resume_dispatch_decision_prefers_blocked_task_when_recovery_target_is_implicit() {
    let mission_id = MissionId::new("mission-implicit-resume");
    let blocked_task_id = TaskId::new("todo-blocked");
    let pending_task_id = TaskId::new("todo-pending");
    let task_store = TaskStore::new();
    let root_task_id = seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[
            (blocked_task_id.clone(), "blocked", TaskStatus::Blocked),
            (pending_task_id, "pending", TaskStatus::Ready),
        ],
    );

    let recovery_input = RecoveryResumeInput {
        recovery_id: "recovery-implicit".to_string(),
        snapshot_id: "snapshot-implicit".to_string(),
        ownership: magi_core::ExecutionOwnership {
            mission_id: Some(mission_id.clone()),
            task_id: None,
            ..Default::default()
        },
        diagnostic_summary: Some("resume".to_string()),
        created_at: UtcMillis::now(),
        updated_at: UtcMillis::now(),
    };

    let target = recovery_planner::build_recovery_target(&task_store, &recovery_input)
        .expect("resume target should be built from task graph");

    assert_eq!(target.mission_id, mission_id);
    assert_eq!(target.root_task_id, root_task_id);
    assert_eq!(target.task_id, blocked_task_id);
}

#[test]
fn recovery_consume_entry_can_execute_worker_and_sync_sidecars() {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let governance = Arc::new(GovernanceService::default());
    let service =
        OrchestratorService::with_governance(Arc::clone(&event_bus), Arc::clone(&governance));
    let session_store = Arc::new(magi_session_store::SessionStore::new());
    let workspace_store = Arc::new(magi_workspace::WorkspaceStore::new());

    let mission_id = MissionId::new("mission-recovery");
    let task_id = TaskId::new("todo-recovery");
    let session_id = SessionId::new("session-recovery");
    let workspace_id = WorkspaceId::new("workspace-recovery");
    let worker_id = WorkerId::new("worker-recovery");

    session_store
        .create_session(session_id.clone(), "session")
        .expect("session should be creatable");
    session_store.bind_execution_ownership(
        session_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-1".to_string()),
        },
    );

    workspace_store
        .register(
            workspace_id.clone(),
            magi_core::AbsolutePath::new("/Users/xie/code/magi"),
        )
        .expect("workspace should be creatable");
    let recovery_handle = workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-1".to_string()),
        },
        "snapshot-recovery",
        "recovery-recovery",
        Some("resume".to_string()),
    );
    workspace_store
        .mark_recovery_ready(&recovery_handle.recovery_id)
        .expect("recovery should be ready");

    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let task_store = Arc::new(TaskStore::new());
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Blocked)],
    );
    let execution_runtime = service
        .execution_runtime_with_recovery_support(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
            session_store.clone(),
            workspace_store.clone(),
        )
        .with_task_store(Arc::clone(&task_store));

    let recovery_input = workspace_store
        .build_recovery_resume_input(&recovery_handle.recovery_id)
        .expect("recovery input should be buildable");
    let result = execution_runtime
        .execute_recovery(recovery_input, worker_id, None)
        .expect("recovery should execute");

    assert_eq!(result.target.task_id, task_id);
    assert_eq!(result.dispatch.overview.mission.completed_tasks, 1);
    assert_eq!(result.dispatch.overview.skill_dispatch_summary.total_dispatches, 1);
    assert_eq!(
        result
            .session_sidecar
            .as_ref()
            .map(|sidecar| sidecar.current_status.clone()),
        Some(magi_session_store::SessionExecutionSidecarStatus::Resumed)
    );
    assert_eq!(
        result
            .session_sidecar
            .as_ref()
            .and_then(|sidecar| sidecar.recovery_ref.clone())
            .as_deref(),
        Some("recovery-recovery")
    );
    assert_eq!(
        result
            .workspace_recovery
            .as_ref()
            .map(|handle| handle.current_status.clone()),
        Some(magi_workspace::RecoveryStatus::Consumed)
    );
    assert_eq!(result.mission_snapshot.completed_tasks, 1);
}

#[test]
fn recovery_execution_prefers_requested_worker_id_over_stored_ownership_worker() {
    let (execution_runtime, recovery_input, _, _) = build_recovery_execution_fixture(true);
    let override_worker_id = WorkerId::new("worker-recovery-override");

    let result = execution_runtime
        .execute_recovery(recovery_input, override_worker_id.clone(), None)
        .expect("recovery should execute with override worker");

    assert_eq!(result.target.requested_worker_id, Some(override_worker_id.clone()));
    assert_eq!(result.dispatch.intent.worker_id, override_worker_id);
    assert_eq!(
        result
            .session_sidecar
            .as_ref()
            .and_then(|sidecar| sidecar.ownership.worker_id.clone()),
        result.target.requested_worker_id
    );
    assert_eq!(
        result
            .workspace_recovery
            .as_ref()
            .and_then(|recovery| recovery.ownership.worker_id.clone()),
        result.target.requested_worker_id
    );
}

#[test]
fn execution_runtime_execute_recovery_then_runs_hook_only_after_success() {
    let (execution_runtime, recovery_input, worker_id, task_id) =
        build_recovery_execution_fixture(true);

    let hook_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_log_capture = Arc::clone(&hook_log);
    let result = execution_runtime
        .execute_recovery_then(recovery_input, worker_id, None, move |result| {
            hook_log_capture
                .lock()
                .expect("hook log lock should hold")
                .push(result.recovery_input.recovery_id.clone());
        })
        .expect("recovery should execute");

    assert_eq!(result.target.task_id, task_id);
    assert_eq!(result.dispatch.overview.mission.completed_tasks, 1);
    assert_eq!(
        hook_log.lock().expect("hook log lock should hold").as_slice(),
        &[result.recovery_input.recovery_id.clone()]
    );
}

#[test]
fn execution_runtime_execute_recovery_then_skips_hook_when_recovery_support_missing() {
    let (execution_runtime, recovery_input, worker_id, _) = build_recovery_execution_fixture(false);

    let hook_calls = Arc::new(Mutex::new(0usize));
    let hook_calls_capture = Arc::clone(&hook_calls);
    let error = execution_runtime
        .execute_recovery_then(recovery_input, worker_id, None, move |_| {
            let mut calls = hook_calls_capture
                .lock()
                .expect("hook calls lock should hold");
            *calls += 1;
        })
        .expect_err("missing recovery support should reject execution before hook");

    match error {
        OrchestratorCommandError::RecoverySupportUnavailable { missing } => {
            assert!(missing.contains("workspace_store"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(*hook_calls.lock().expect("hook calls lock should hold"), 0);
}

#[test]
fn execution_runtime_execute_recovery_rejects_prepared_input_before_sidecar_mutation() {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let governance = Arc::new(GovernanceService::default());
    let service =
        OrchestratorService::with_governance(Arc::clone(&event_bus), Arc::clone(&governance));
    let control = service.control_plane();
    let session_store = Arc::new(magi_session_store::SessionStore::new());
    let workspace_store = Arc::new(magi_workspace::WorkspaceStore::new());

    let mission_id = MissionId::new("mission-recovery-prepared");
    let assignment_id = AssignmentId::new("assignment-recovery-prepared");
    let task_id = TaskId::new("todo-recovery-prepared");
    let session_id = SessionId::new("session-recovery-prepared");
    let workspace_id = WorkspaceId::new("workspace-recovery-prepared");
    let worker_id = WorkerId::new("worker-recovery-prepared");

    session_store
        .create_session(session_id.clone(), "session")
        .expect("session should be creatable");
    session_store.bind_execution_ownership(
        session_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-prepared".to_string()),
        },
    );

    workspace_store
        .register(
            workspace_id.clone(),
            magi_core::AbsolutePath::new("/Users/xie/code/magi"),
        )
        .expect("workspace should be creatable");
    let recovery_handle = workspace_store.prepare_recovery_entry(
        workspace_id.clone(),
        magi_core::ExecutionOwnership {
            session_id: Some(session_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: Some("chain-prepared".to_string()),
        },
        "snapshot-recovery-prepared",
        "recovery-prepared",
        Some("resume".to_string()),
    );

    let recovery_input = magi_core::RecoveryResumeInput {
        recovery_id: recovery_handle.recovery_id.clone(),
        snapshot_id: recovery_handle.snapshot_id.clone(),
        ownership: recovery_handle.ownership.clone(),
        diagnostic_summary: recovery_handle.diagnostic_summary.clone(),
        created_at: recovery_handle.created_at,
        updated_at: recovery_handle.updated_at,
    };

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let execution_runtime = service.execution_runtime_with_recovery_support(
        WorkerRuntime::new_compare(Arc::clone(&event_bus)),
        tool_registry.clone(),
        SkillDispatchRuntime::new(
            tool_registry,
            magi_bridge_client::BridgeDispatchRuntime::new(),
        ),
        session_store.clone(),
        workspace_store.clone(),
    );

    let error = execution_runtime
        .execute_recovery(recovery_input, worker_id, None)
        .expect_err("prepared recovery should fail before mutation");

    match error {
        OrchestratorCommandError::RecoverySupportUnavailable { missing } => {
            assert!(missing.contains("还未进入 Ready"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let sidecar = session_store
        .execution_sidecar_export(&session_id)
        .expect("session sidecar should remain available");
    assert_eq!(
        sidecar.current_status,
        magi_session_store::SessionExecutionSidecarStatus::Bound
    );
    assert!(sidecar.recovery_ref.is_none());

    let recovery_export = workspace_store
        .recovery_sidecar_export("recovery-prepared")
        .expect("recovery export should still exist");
    assert_eq!(recovery_export.current_status, magi_workspace::RecoveryStatus::Prepared);
}

#[test]
fn control_plane_can_apply_governance_decisions() {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let service = OrchestratorService::with_governance(
        event_bus,
        Arc::new(magi_governance::GovernanceService::default()),
    );
    let control = service.control_plane();

    let mission_id = MissionId::new("mission-4");
    let assignment_id = AssignmentId::new("assignment-4");
    let task_id = TaskId::new("todo-4");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let blocked_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-4")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::Execute,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: true,
        reason: Some("blocked".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: blocked_request,
        })
        .expect("blocked governance decision should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            mission,
            decision,
            disposition,
        } => {
            assert_eq!(decision.outcome, magi_governance::GovernanceOutcome::Blocked);
            assert_eq!(disposition, GovernanceDisposition::Blocked);
            assert_eq!(mission.assignments[0].tasks[0].status, TaskStatus::Blocked);
            assert_eq!(mission.status, MissionLifecycleStatus::Running);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let approval_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-4")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::Execute,
        risk_level: RiskLevel::High,
        approval_requirement: ApprovalRequirement::Required,
        retry_count: 0,
        blocked: false,
        reason: Some("approval".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: approval_request,
        })
        .expect("approval governance decision should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            decision,
            disposition,
            ..
        } => {
            assert_eq!(
                decision.outcome,
                magi_governance::GovernanceOutcome::NeedsApproval
            );
            assert_eq!(disposition, GovernanceDisposition::NeedsApproval);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let retry_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-4")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::RepairRetry,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 1,
        blocked: false,
        reason: Some("repair retry".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: retry_request,
        })
        .expect("repair retry governance decision should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            decision,
            disposition,
            mission,
        } => {
            assert_eq!(decision.outcome, magi_governance::GovernanceOutcome::Allowed);
            assert_eq!(disposition, GovernanceDisposition::RepairRetryScheduled);
            assert_eq!(mission.assignments[0].tasks[0].status, TaskStatus::Running);
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn execution_runtime_can_run_dispatch_through_worker_loop() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));
    let (execution_runtime, task_store) =
        build_execution_runtime_with_task_store(&service, worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-exec");
    let task_id = TaskId::new("todo-exec");
    seed_action_tasks(&task_store, &mission_id, "mission", &[(task_id.clone(), "task", TaskStatus::Ready)]);

    let result = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-exec"),
            Some(SessionId::new("session-exec")),
            Some(WorkspaceId::new("workspace-exec")),
            None,
        )
        .expect("execution should run");

    assert_eq!(result.target.task_id, task_id);
    assert_eq!(result.intent.task_id, task_id);
    assert!(result.outcome.report.is_some());
    assert_eq!(result.overview.mission.completed_tasks, 1);
    assert_eq!(result.overview.skill_dispatch_summary.total_dispatches, 1);
    assert_eq!(result.overview.skill_dispatch_summary.builtin_dispatches, 1);
    assert_eq!(
        result.overview.assignment_skill_dispatch_summaries[0]
            .skill_dispatch_summary
            .total_dispatches,
        1
    );
    assert_eq!(
        result.overview.task_skill_dispatch_summaries[0]
            .skill_dispatch_summary
            .total_dispatches,
        1
    );
    assert_eq!(result.overview.tool_summary.total_invocations, 2);
    assert!(!result.overview.running_task_ids.contains(&task_id));
}

#[test]
fn execution_runtime_automatically_assembles_context_summary_when_configured() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));

    let knowledge_store = magi_knowledge_store::KnowledgeStore::new();
    knowledge_store.ingest_code_index(magi_knowledge_store::CodeIndexIngestion {
        knowledge_id: "kb-exec-context-1".to_string(),
        title: "Mission parser refresh".to_string(),
        content: "Fix manifest parser for execution runtime context.".to_string(),
        tags: vec!["parser".to_string(), "runtime".to_string()],
        source_ref: Some("knowledge://execution-context".to_string()),
        updated_at: UtcMillis::now(),
        source: magi_knowledge_store::CodeIndexSource {
            path: "src/parser.rs".to_string(),
            language: Some("rust".to_string()),
            repo_ref: Some("repo".to_string()),
            commit_ref: Some("commit".to_string()),
            start_line: Some(12),
            end_line: Some(48),
            symbol: Some(magi_knowledge_store::CodeIndexSymbol {
                name: "refresh_manifest_parser".to_string(),
                kind: magi_knowledge_store::CodeSymbolKind::Function,
                container: None,
                signature: Some("fn refresh_manifest_parser(input: &str)".to_string()),
            }),
        },
        audit: Some(magi_knowledge_store::KnowledgeAuditLink {
            audit_event_id: "audit-exec-context-1".to_string(),
            trail_ref: Some("audit/trails/execution-context.json".to_string()),
            sequence: Some(3),
        }),
        governance: Some(magi_knowledge_store::KnowledgeGovernanceLink {
            outcome: magi_knowledge_store::KnowledgeGovernanceOutcome::Allowed,
            policy_refs: vec!["policy.knowledge.read".to_string()],
            rationale: Some("allowed for execution runtime context".to_string()),
            audit_event_id: Some("audit-exec-context-1".to_string()),
        }),
    });

    let session_id = SessionId::new("session-exec-context");
    let workspace_id = WorkspaceId::new("workspace-exec-context");
    let memory_store = magi_memory_store::MemoryStore::new();
    memory_store.apply_extraction(magi_memory_store::MemoryExtractionApplyRequest {
        extraction_id: "extract-exec-context-1".to_string(),
        session_id: session_id.clone(),
        source_ref: Some("timeline://execution-context".to_string()),
        summary: "Remember parser refresh constraints".to_string(),
        memories: vec![magi_memory_store::ExtractedMemory {
            memory_id: "mem-exec-context-1".to_string(),
            layer: magi_memory_store::MemoryLayer::Durable,
            content: "Parser refresh needs audit-safe migration.".to_string(),
            created_at: UtcMillis::now(),
        }],
        created_at: UtcMillis::now(),
    });
    assert!(
        memory_store
            .verify_extraction_linkage("extract-exec-context-1")
            .expect("memory extraction verification should exist")
            .is_consistent
    );

    let context_runtime = magi_context_runtime::ContextRuntime::new(knowledge_store, memory_store);
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );
    let execution_runtime = execution_runtime.with_context_runtime(
            context_runtime,
            ExecutionContextConfig {
                budget: magi_context_runtime::ContextBudget {
                    max_turns: 4,
                    max_knowledge: 3,
                    max_memory: 3,
                    max_shared_items: 2,
                    max_file_summaries: 2,
                },
                project_key: Some("project-exec-context".to_string()),
            },
        );

    let mission_id = MissionId::new("mission-exec-context");
    let task_id = TaskId::new("todo-exec-context");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "Mission parser refresh",
        &[(task_id.clone(), "Fix manifest parser", TaskStatus::Ready)],
    );

    let result = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-exec-context"),
            Some(session_id),
            Some(workspace_id),
            None,
        )
        .expect("execution should run");

    let context_summary = result
        .overview
        .context_summary
        .as_ref()
        .expect("context summary should be assembled automatically");
    assert_eq!(context_summary.used_knowledge, 1);
    assert_eq!(context_summary.used_memory, 1);
    assert_eq!(context_summary.code_index_knowledge_count, 1);
    assert_eq!(context_summary.audited_knowledge_count, 1);
    assert_eq!(context_summary.governed_knowledge_count, 1);
    assert_eq!(context_summary.extracted_memory_count, 1);
    assert_eq!(
        context_summary.knowledge_source_paths,
        vec!["src/parser.rs".to_string()]
    );
    assert_eq!(
        context_summary.memory_extraction_refs,
        vec!["extract-exec-context-1".to_string()]
    );

    let read_model = event_bus.runtime_read_model_input();
    let mission_entry = read_model
        .details
        .missions
        .iter()
        .find(|entry| entry.mission_id == mission_id.to_string())
        .expect("mission entry should exist");
    assert_eq!(mission_entry.context_used_knowledge_count, 1);
    assert_eq!(mission_entry.context_used_memory_count, 1);
    assert_eq!(mission_entry.context_code_index_knowledge_count, 1);
    assert_eq!(mission_entry.context_audited_knowledge_count, 1);
    assert_eq!(mission_entry.context_governed_knowledge_count, 1);
    assert_eq!(mission_entry.context_extracted_memory_count, 1);
    assert_eq!(
        mission_entry.context_knowledge_source_paths,
        vec!["src/parser.rs".to_string()]
    );
    assert_eq!(
        mission_entry.context_memory_extraction_refs,
        vec!["extract-exec-context-1".to_string()]
    );
}

#[test]
fn execution_runtime_execute_dispatch_then_runs_hook_only_after_success() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));
    let (execution_runtime, task_store) =
        build_execution_runtime_with_task_store(&service, worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-dispatch-hook-success");
    let task_id = TaskId::new("todo-dispatch-hook-success");
    seed_action_tasks(&task_store, &mission_id, "mission", &[(task_id.clone(), "task", TaskStatus::Ready)]);

    let hook_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_log_capture = Arc::clone(&hook_log);
    let result = execution_runtime
        .execute_dispatch_then(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-dispatch-hook-success"),
            Some(SessionId::new("session-dispatch-hook-success")),
            Some(WorkspaceId::new("workspace-dispatch-hook-success")),
            None,
            move |result| {
                hook_log_capture
                    .lock()
                    .expect("hook log lock should hold")
                    .push(result.target.task_id.to_string());
            },
        )
        .expect("execution should run");

    assert_eq!(result.target.task_id, task_id);
    assert_eq!(
        hook_log.lock().expect("hook log lock should hold").as_slice(),
        &[task_id.to_string()]
    );
}

#[test]
fn execution_runtime_execute_dispatch_with_writebacks_persists_memory_extraction_on_success() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));
    let (execution_runtime, task_store) =
        build_execution_runtime_with_task_store(&service, worker_runtime, tool_registry, skill_runtime);
    let memory_store = magi_memory_store::MemoryStore::new();

    let mission_id = MissionId::new("mission-dispatch-writeback-success");
    let task_id = TaskId::new("todo-dispatch-writeback-success");
    let session_id = SessionId::new("session-dispatch-writeback-success");
    seed_action_tasks(&task_store, &mission_id, "mission", &[(task_id.clone(), "task", TaskStatus::Ready)]);

    let writebacks = ExecutionWritebackPlans::from_optional_memory_extraction(Some(
        magi_memory_store::MemoryExtractionApplyRequest {
            extraction_id: "extract-dispatch-writeback-success".to_string(),
            session_id: session_id.clone(),
            source_ref: Some("timeline://dispatch-writeback".to_string()),
            summary: "dispatch writeback".to_string(),
            memories: vec![magi_memory_store::ExtractedMemory {
                memory_id: "mem-dispatch-writeback-success".to_string(),
                layer: magi_memory_store::MemoryLayer::Durable,
                content: "dispatch writeback content".to_string(),
                created_at: UtcMillis(42),
            }],
            created_at: UtcMillis(42),
        },
    ));

    let result = execution_runtime
        .execute_dispatch_with_writebacks(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-dispatch-writeback-success"),
            Some(session_id),
            Some(WorkspaceId::new("workspace-dispatch-writeback-success")),
            None,
            memory_store.clone(),
            writebacks,
        )
        .expect("execution should run");

    assert_eq!(result.target.task_id, task_id);
    assert!(memory_store
        .verify_extraction_linkage("extract-dispatch-writeback-success")
        .expect("dispatch writeback should persist extraction linkage")
        .is_consistent);
}

#[test]
fn execution_runtime_can_run_dispatch_through_local_process_executor() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new(Arc::clone(&event_bus))
        .with_executor(Arc::new(LocalProcessWorkerExecutor::cargo_loopback()));
    let (execution_runtime, task_store) =
        build_execution_runtime_with_task_store(&service, worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-local-exec");
    let task_id = TaskId::new("todo-local-exec");
    seed_action_tasks(&task_store, &mission_id, "mission", &[(task_id.clone(), "task", TaskStatus::Ready)]);

    let result = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-local-exec"),
            Some(SessionId::new("session-local-exec")),
            Some(WorkspaceId::new("workspace-local-exec")),
            None,
        )
        .expect("local process execution should run");

    assert_eq!(result.overview.mission.completed_tasks, 1);
    assert_eq!(result.overview.worker_summary.tool_call_count, 1);
    assert_eq!(result.overview.tool_summary.total_invocations, 1);
    assert_eq!(result.overview.skill_dispatch_summary.total_dispatches, 1);
    assert!(result.outcome.report.is_some());
}

#[derive(Clone)]
struct UnhealthyLocalExecutor;

impl ShadowWorkerExecutor for UnhealthyLocalExecutor {
    fn execute(&self, intent: &WorkerExecutionIntent) -> WorkerExecutionTrace {
        WorkerExecutionTrace {
            worker_id: intent.worker_id.clone(),
            task_id: intent.task_id.clone(),
            tool_invocations: Vec::new(),
            skill_dispatches: Vec::new(),
            final_report: WorkerExecutionFinalReport {
                summary: "should not execute".to_string(),
                result_kind: Some(TaskResultKind::Failure),
                termination_reason: Some(TerminationReason::Failed),
                verification_status: VerificationStatus::Failed,
            },
        }
    }

    fn probe(&self) -> Result<WorkerExecutorProbe, WorkerExecutorFailure> {
        Ok(WorkerExecutorProbe {
            executor_id: "shadow-local-process-worker-executor".to_string(),
            executor_version: "worker-shadow-local-process-executor-v2".to_string(),
            executor_kind: WorkerExecutorKind::LocalProcess,
            capability: LocalProcessExecutorCapability {
                executor_id: "shadow-local-process-worker-executor".to_string(),
                executor_version: "worker-shadow-local-process-executor-v2".to_string(),
                execution_mode: magi_worker_runtime::WorkerExecutionMode::LocalProcess,
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
                descriptor: magi_worker_runtime::LocalProcessExecutorDescriptor {
                    process_model:
                        magi_worker_runtime::LocalProcessExecutorProcessModel::OneShotSubprocess,
                    reuse_scope: magi_worker_runtime::WorkerExecutionBindingScope::None,
                    parallelism_scope: magi_worker_runtime::WorkerExecutionParallelismScope::Executor,
                    lease_state: magi_worker_runtime::WorkerExecutionLeaseState::None,
                    binding_lifecycle: magi_worker_runtime::WorkerExecutionBindingLifecycle::None,
                    process_lifecycle:
                        magi_worker_runtime::WorkerExecutionProcessLifecycle::OneShot,
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
                detail: "executor unhealthy".to_string(),
            },
        })
    }

    fn executor_kind(&self) -> WorkerExecutorKind {
        WorkerExecutorKind::LocalProcess
    }
}

#[test]
fn execution_runtime_rejects_unhealthy_local_process_executor_before_execute() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let control = service.control_plane();
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let execution_runtime = service.execution_runtime(worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-unhealthy-exec");
    let assignment_id = AssignmentId::new("assignment-unhealthy-exec");
    let task_id = TaskId::new("todo-unhealthy-exec");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let dispatch = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };

    let error = execution_runtime
        .execute_dispatch(
            dispatch_target(dispatch),
            WorkerId::new("worker-unhealthy-exec"),
            Some(SessionId::new("session-unhealthy-exec")),
            Some(WorkspaceId::new("workspace-unhealthy-exec")),
            None,
        )
        .expect_err("unhealthy executor should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert!(reason.contains("not healthy"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn execution_runtime_execute_dispatch_then_skips_hook_on_failure() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let control = service.control_plane();
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let execution_runtime = service.execution_runtime(worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-dispatch-hook-failure");
    let assignment_id = AssignmentId::new("assignment-dispatch-hook-failure");
    let task_id = TaskId::new("todo-dispatch-hook-failure");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let dispatch = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };

    let hook_calls = Arc::new(Mutex::new(0usize));
    let hook_calls_capture = Arc::clone(&hook_calls);
    let error = execution_runtime
        .execute_dispatch_then(
            dispatch_target(dispatch),
            WorkerId::new("worker-dispatch-hook-failure"),
            Some(SessionId::new("session-dispatch-hook-failure")),
            Some(WorkspaceId::new("workspace-dispatch-hook-failure")),
            None,
            move |_| {
                let mut calls = hook_calls_capture
                    .lock()
                    .expect("hook calls lock should hold");
                *calls += 1;
            },
        )
        .expect_err("unhealthy executor should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert!(reason.contains("not healthy"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(*hook_calls.lock().expect("hook calls lock should hold"), 0);
}

#[test]
fn execution_runtime_execute_dispatch_with_writebacks_skips_writeback_on_failure() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let control = service.control_plane();
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let execution_runtime = service.execution_runtime(worker_runtime, tool_registry, skill_runtime);
    let memory_store = magi_memory_store::MemoryStore::new();

    let mission_id = MissionId::new("mission-dispatch-writeback-failure");
    let assignment_id = AssignmentId::new("assignment-dispatch-writeback-failure");
    let task_id = TaskId::new("todo-dispatch-writeback-failure");
    let session_id = SessionId::new("session-dispatch-writeback-failure");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let dispatch = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };

    let writebacks = ExecutionWritebackPlans::from_optional_memory_extraction(Some(
        magi_memory_store::MemoryExtractionApplyRequest {
            extraction_id: "extract-dispatch-writeback-failure".to_string(),
            session_id,
            source_ref: Some("timeline://dispatch-writeback-failure".to_string()),
            summary: "dispatch writeback failure".to_string(),
            memories: vec![magi_memory_store::ExtractedMemory {
                memory_id: "mem-dispatch-writeback-failure".to_string(),
                layer: magi_memory_store::MemoryLayer::Durable,
                content: "dispatch writeback should not persist".to_string(),
                created_at: UtcMillis(52),
            }],
            created_at: UtcMillis(52),
        },
    ));

    let error = execution_runtime
        .execute_dispatch_with_writebacks(
            dispatch_target(dispatch),
            WorkerId::new("worker-dispatch-writeback-failure"),
            Some(SessionId::new("session-dispatch-writeback-failure")),
            Some(WorkspaceId::new("workspace-dispatch-writeback-failure")),
            None,
            memory_store.clone(),
            writebacks,
        )
        .expect_err("unhealthy executor should be rejected before writeback");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert!(reason.contains("not healthy"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(memory_store
        .extraction_linkage("extract-dispatch-writeback-failure")
        .is_none());
}

#[test]
fn execution_runtime_rejects_local_process_executor_missing_step_capability_before_execute() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(
        LocalProcessWorkerExecutor::cargo_loopback()
            .with_env("MAGI_LOCAL_WORKER_SUPPORTED_STEP_KINDS", "final-report"),
    ));
    let (execution_runtime, task_store) =
        build_execution_runtime_with_task_store(&service, worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-step-capability");
    let task_id = TaskId::new("todo-step-capability");
    seed_action_tasks(&task_store, &mission_id, "mission", &[(task_id.clone(), "task", TaskStatus::Ready)]);

    let error = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-step-capability"),
            Some(SessionId::new("session-step-capability")),
            Some(WorkspaceId::new("workspace-step-capability")),
            None,
        )
        .expect_err("missing step capability should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert!(reason.contains("missing required steps"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn execution_runtime_rejects_local_process_executor_affinity_mismatch_before_execute() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let control = service.control_plane();
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(
        LocalProcessWorkerExecutor::cargo_loopback()
            .with_env("MAGI_LOCAL_WORKER_SESSION_ID", "session-affine")
            .with_env("MAGI_LOCAL_WORKER_WORKSPACE_ID", "workspace-affine"),
    ));
    let execution_runtime = service.execution_runtime(worker_runtime, tool_registry, skill_runtime);

    let mission_id = MissionId::new("mission-affinity-capability");
    let assignment_id = AssignmentId::new("assignment-affinity-capability");
    let task_id = TaskId::new("todo-affinity-capability");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    let dispatch = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };

    let error = execution_runtime
        .execute_dispatch(
            dispatch_target(dispatch),
            WorkerId::new("worker-affinity-capability"),
            Some(SessionId::new("session-other")),
            Some(WorkspaceId::new("workspace-other")),
            None,
        )
        .expect_err("affinity mismatch should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert!(reason.contains("affinity mismatch"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn multi_task_execution_with_mixed_outcomes_aggregates_in_overview() {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let control = service.control_plane();
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();

    let mission_id = MissionId::new("mission-multi");
    let assignment_a = AssignmentId::new("assignment-multi-a");
    let assignment_b = AssignmentId::new("assignment-multi-b");
    let task_a1 = TaskId::new("todo-multi-a1");
    let task_a2 = TaskId::new("todo-multi-a2");
    let task_b1 = TaskId::new("todo-multi-b1");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_a.clone(),
        title: "assignment-a".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_b.clone(),
        title: "assignment-b".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_a.clone(),
        task_id: task_a1.clone(),
        title: "todo-a1".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_a.clone(),
        task_id: task_a2.clone(),
        title: "todo-a2".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_b.clone(),
        task_id: task_b1.clone(),
        title: "todo-b1".to_string(),
    });

    // Dispatch and apply success report for task_a1
    let dispatch_a1 = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch a1 should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };
    assert_eq!(dispatch_a1.task_id, task_a1);
    match control
        .execute(OrchestratorCommand::ApplyWorkerReport {
            report: WorkerExecutionReport {
                worker_id: WorkerId::new("worker-multi-1"),
                task_id: task_a1.clone(),
                stage: WorkerStage::Finish,
                summary: "completed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
                created_at: UtcMillis::now(),
            },
        })
        .expect("task_a1 success report should apply")
    {
        OrchestratorCommandResult::WorkerReportApplied { mission } => {
            let completed = mission
                .assignments
                .iter()
                .flat_map(|assignment| assignment.tasks.iter())
                .filter(|task| task.status == TaskStatus::Completed)
                .count();
            assert_eq!(completed, 1);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Dispatch and apply success report for task_a2
    let dispatch_a2 = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch a2 should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };
    assert_eq!(dispatch_a2.task_id, task_a2);
    match control
        .execute(OrchestratorCommand::ApplyWorkerReport {
            report: WorkerExecutionReport {
                worker_id: WorkerId::new("worker-multi-2"),
                task_id: task_a2.clone(),
                stage: WorkerStage::Finish,
                summary: "completed".to_string(),
                result_kind: Some(TaskResultKind::Success),
                termination_reason: Some(TerminationReason::Completed),
                verification_status: VerificationStatus::Passed,
                created_at: UtcMillis::now(),
            },
        })
        .expect("task_a2 success report should apply")
    {
        OrchestratorCommandResult::WorkerReportApplied { mission } => {
            let completed = mission
                .assignments
                .iter()
                .flat_map(|assignment| assignment.tasks.iter())
                .filter(|task| task.status == TaskStatus::Completed)
                .count();
            assert_eq!(completed, 2);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Dispatch task_b1 via control, then apply a failure report manually
    let dispatch_b1 = match control
        .execute(OrchestratorCommand::DispatchNextTask {
            mission_id: mission_id.clone(),
        })
        .expect("dispatch b1 should exist")
    {
        OrchestratorCommandResult::TaskDispatchPlanned { decision } => decision,
        other => panic!("unexpected result: {other:?}"),
    };
    assert_eq!(dispatch_b1.task_id, task_b1);
    let failure_report = WorkerExecutionReport {
        worker_id: WorkerId::new("worker-multi-3"),
        task_id: task_b1.clone(),
        stage: WorkerStage::Finish,
        summary: "failed".to_string(),
        result_kind: Some(TaskResultKind::Failure),
        termination_reason: Some(TerminationReason::Failed),
        verification_status: VerificationStatus::Failed,
        created_at: UtcMillis::now(),
    };
    match control
        .execute(OrchestratorCommand::ApplyWorkerReport {
            report: failure_report,
        })
        .expect("failure report should be applied")
    {
        OrchestratorCommandResult::WorkerReportApplied { mission } => {
            assert_eq!(mission.status, MissionLifecycleStatus::Failed);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Build final overview with aggregated observations
    let skill_observations = vec![
        skill_dispatch_observation(&task_a1),
        skill_dispatch_observation(&task_a2),
    ];
    let governance_observations = vec![
        governance_observation(
            &task_a1,
            WorkerControlKind::Execute,
            magi_governance::GovernanceOutcome::Allowed,
            true,
            false,
            DecisionPhase::WorkerControl,
            None,
        ),
        governance_observation(
            &task_b1,
            WorkerControlKind::Execute,
            magi_governance::GovernanceOutcome::Blocked,
            false,
            false,
            DecisionPhase::WorkerControl,
            Some("blocked after failure"),
        ),
    ];

    let overview = match control
        .execute(OrchestratorCommand::BuildMissionExecutionOverview {
            mission_id: mission_id.clone(),
            worker_summary: worker_summary(2),
            tool_summary: ToolExecutionSummary {
                total_invocations: 4,
                successful_invocations: 3,
                failed_invocations: 1,
                blocked_invocations: 0,
            },
            skill_dispatch_observations: skill_observations,
            governance_observations,
            context_summary: None,
        })
        .expect("overview should be built")
    {
        OrchestratorCommandResult::MissionExecutionOverviewBuilt { overview } => overview,
        other => panic!("unexpected result: {other:?}"),
    };

    // Mission-level assertions
    assert_eq!(overview.mission.total_tasks, 3);
    assert_eq!(overview.mission.completed_tasks, 2);
    assert_eq!(overview.mission.failed_tasks, 1);
    assert_eq!(overview.mission.total_assignments, 2);
    assert!(overview.running_task_ids.is_empty());

    // Tool summary
    assert_eq!(overview.tool_summary.total_invocations, 4);
    assert_eq!(overview.tool_summary.successful_invocations, 3);
    assert_eq!(overview.tool_summary.failed_invocations, 1);

    // Skill dispatch aggregation
    assert_eq!(overview.skill_dispatch_summary.total_dispatches, 2);
    assert_eq!(overview.skill_dispatch_summary.builtin_dispatches, 2);
    assert_eq!(overview.skill_dispatch_summary.succeeded_dispatches, 2);

    // Per-assignment skill dispatch
    assert_eq!(overview.assignment_skill_dispatch_summaries.len(), 2);
    let assign_a_skill = overview
        .assignment_skill_dispatch_summaries
        .iter()
        .find(|summary| summary.assignment_id == assignment_a)
        .expect("assignment-a skill summary should exist");
    assert_eq!(assign_a_skill.skill_dispatch_summary.total_dispatches, 2);
    let assign_b_skill = overview
        .assignment_skill_dispatch_summaries
        .iter()
        .find(|summary| summary.assignment_id == assignment_b)
        .expect("assignment-b skill summary should exist");
    assert_eq!(assign_b_skill.skill_dispatch_summary.total_dispatches, 0);

    // Per-task skill dispatch
    assert_eq!(overview.task_skill_dispatch_summaries.len(), 3);

    // Governance aggregation
    assert_eq!(overview.governance_summary.total_checks, 2);
    assert_eq!(overview.governance_summary.allowed, 1);
    assert_eq!(overview.governance_summary.blocked, 1);

    // Per-assignment governance
    assert_eq!(overview.assignment_governance_summaries.len(), 2);
    let assign_a_gov = overview
        .assignment_governance_summaries
        .iter()
        .find(|summary| summary.assignment_id == assignment_a)
        .expect("assignment-a governance summary should exist");
    assert_eq!(assign_a_gov.governance_summary.allowed, 1);
    let assign_b_gov = overview
        .assignment_governance_summaries
        .iter()
        .find(|summary| summary.assignment_id == assignment_b)
        .expect("assignment-b governance summary should exist");
    assert_eq!(assign_b_gov.governance_summary.blocked, 1);
}

#[test]
fn governance_block_then_allow_resumes_and_dispatches_task() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let governance = Arc::new(GovernanceService::default());
    let service = OrchestratorService::with_governance(
        Arc::clone(&event_bus),
        Arc::clone(&governance),
    );
    let control = service.control_plane();

    let mission_id = MissionId::new("mission-gov-block-allow");
    let assignment_id = AssignmentId::new("assignment-gov-block-allow");
    let task_id = TaskId::new("todo-gov-block-allow");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    // First: apply governance block (blocked=true)
    let blocked_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-gov-1")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::Execute,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: true,
        reason: Some("external dependency unavailable".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: blocked_request,
        })
        .expect("blocked governance should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            mission,
            decision,
            disposition,
        } => {
            assert_eq!(disposition, GovernanceDisposition::Blocked);
            assert_eq!(decision.outcome, magi_governance::GovernanceOutcome::Blocked);
            assert_eq!(
                mission.assignments[0].tasks[0].status,
                TaskStatus::Blocked
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Second: apply governance allow (blocked=false, low risk)
    let allow_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-gov-1")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::Execute,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 0,
        blocked: false,
        reason: Some("dependency now available".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: allow_request,
        })
        .expect("allow governance should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            mission,
            decision,
            disposition,
        } => {
            assert_eq!(disposition, GovernanceDisposition::Allowed);
            assert_eq!(decision.outcome, magi_governance::GovernanceOutcome::Allowed);
            assert_eq!(
                mission.assignments[0].tasks[0].status,
                TaskStatus::Running
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let mission = service
        .missions()
        .into_iter()
        .find(|mission| mission.mission_id == mission_id)
        .expect("mission should remain queryable after governance allow");
    assert_eq!(mission.assignments[0].tasks[0].status, TaskStatus::Running);
}

#[test]
fn repair_retry_with_prior_count_schedules_and_dispatches() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let governance = Arc::new(GovernanceService::default());
    let service = OrchestratorService::with_governance(
        Arc::clone(&event_bus),
        Arc::clone(&governance),
    );
    let control = service.control_plane();

    let mission_id = MissionId::new("mission-repair-retry");
    let assignment_id = AssignmentId::new("assignment-repair-retry");
    let task_id = TaskId::new("todo-repair-retry");

    let _ = control.execute(OrchestratorCommand::CreateMission {
        mission_id: mission_id.clone(),
        title: "mission".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::AddAssignment {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        title: "assignment".to_string(),
    });
    let _ = control.execute(OrchestratorCommand::CreateTask {
        mission_id: mission_id.clone(),
        assignment_id: assignment_id.clone(),
        task_id: task_id.clone(),
        title: "task".to_string(),
    });

    // Apply RepairRetry with retry_count=1 (avoids rejection — retry_count=0 would be Rejected)
    let retry_request = WorkerControlRequest {
        worker_id: Some(WorkerId::new("worker-repair")),
        mission_id: Some(mission_id.clone()),
        assignment_id: Some(assignment_id.clone()),
        task_id: Some(task_id.clone()),
        action: WorkerControlKind::RepairRetry,
        risk_level: RiskLevel::Low,
        approval_requirement: ApprovalRequirement::None,
        retry_count: 1,
        blocked: false,
        reason: Some("repair retry after first failure".to_string()),
    };
    match control
        .execute(OrchestratorCommand::ApplyGovernanceDecision {
            request: retry_request,
        })
        .expect("repair retry governance should apply")
    {
        OrchestratorCommandResult::GovernanceDecisionApplied {
            mission,
            decision,
            disposition,
        } => {
            assert_eq!(disposition, GovernanceDisposition::RepairRetryScheduled);
            assert_eq!(decision.outcome, magi_governance::GovernanceOutcome::Allowed);
            assert_eq!(
                mission.assignments[0].tasks[0].status,
                TaskStatus::Running
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let mission = service
        .missions()
        .into_iter()
        .find(|mission| mission.mission_id == mission_id)
        .expect("mission should remain queryable after repair retry");
    assert_eq!(mission.assignments[0].tasks[0].status, TaskStatus::Running);
}

// ========== Dispatch Batch Tests ==========

mod dispatch_batch_tests {
    use crate::dispatch::*;
    use std::collections::HashSet;

    fn make_contract(name: &str, deps: &[&str]) -> DispatchTaskContract {
        DispatchTaskContract {
            task_title: name.to_string(),
            ownership: "general".to_string(),
            mode: "implement".to_string(),
            context: vec![],
            scope_hint: vec![],
            files: vec![],
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            collaboration_contracts: DispatchCollaborationContracts::default(),
        }
    }

    fn make_result(summary: &str) -> DispatchResult {
        DispatchResult {
            success: true,
            summary: summary.to_string(),
            full_summary: None,
            modified_files: None,
            created_files: None,
            errors: None,
            blocking_issue: None,
            token_usage: None,
        }
    }

    #[test]
    fn batch_register_and_get() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w2", make_contract("task2", &["t1"])).unwrap();

        assert_eq!(batch.size(), 2);
        assert!(batch.get_entry("t1").is_some());
        assert!(batch.get_entry("t2").is_some());
        assert!(batch.get_entry("t3").is_none());
    }

    #[test]
    fn batch_duplicate_register_returns_err() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        let r = batch.register("t1", "w2", make_contract("task1_dup", &[]));
        assert!(r.is_err());
        assert_eq!(batch.size(), 1);
        assert_eq!(batch.get_entry("t1").unwrap().worker, "w1");
    }

    #[test]
    fn batch_status_transitions() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        assert_eq!(batch.get_entry("t1").unwrap().status, DispatchStatus::Pending);

        batch.mark_running("t1");
        assert_eq!(batch.get_entry("t1").unwrap().status, DispatchStatus::Running);

        batch.mark_completed("t1", make_result("done"));
        assert_eq!(batch.get_entry("t1").unwrap().status, DispatchStatus::Completed);
        assert!(batch.get_entry("t1").unwrap().status.is_terminal());
    }

    #[test]
    fn batch_get_ready_tasks_respects_deps() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w1", make_contract("task2", &["t1"])).unwrap();
        batch.register("t3", "w2", make_contract("task3", &[])).unwrap();

        let ready = batch.get_ready_tasks();
        let ready_ids: HashSet<&str> = ready.iter().map(|e| e.task_id.as_str()).collect();
        assert!(ready_ids.contains("t1"));
        assert!(ready_ids.contains("t3"));
        assert!(!ready_ids.contains("t2"));

        batch.mark_running("t1");
        batch.mark_completed("t1", make_result("done"));

        let ready2 = batch.get_ready_tasks();
        assert!(ready2.iter().any(|e| e.task_id == "t2"));
    }

    #[test]
    fn batch_isolation_same_worker_serial() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w1", make_contract("task2", &[])).unwrap();
        batch.register("t3", "w2", make_contract("task3", &[])).unwrap();

        batch.mark_running("t1");

        let ready = batch.get_ready_tasks_isolated();
        let ready_ids: HashSet<&str> = ready.iter().map(|e| e.task_id.as_str()).collect();
        assert!(!ready_ids.contains("t2"));
        assert!(ready_ids.contains("t3"));
    }

    #[test]
    fn batch_topological_sort() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w1", make_contract("task2", &["t1"])).unwrap();
        batch.register("t3", "w1", make_contract("task3", &["t2"])).unwrap();

        let sorted = batch.topological_sort().unwrap();
        let pos: std::collections::HashMap<&str, usize> = sorted
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        assert!(pos["t1"] < pos["t2"]);
        assert!(pos["t2"] < pos["t3"]);
    }

    #[test]
    fn batch_cancel_all() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w2", make_contract("task2", &[])).unwrap();

        batch.mark_running("t1");
        batch.cancel_all("test cancel");

        assert_eq!(batch.get_entry("t2").unwrap().status, DispatchStatus::Cancelled);
    }

    #[test]
    fn batch_summary() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w2", make_contract("task2", &[])).unwrap();

        batch.mark_running("t1");
        batch.mark_completed("t1", make_result("ok"));

        let summary = batch.summary();
        assert_eq!(summary.total, 2);
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.pending, 1);
    }

    #[test]
    fn batch_phase_transitions() {
        let mut batch = DispatchBatch::new(Some("b1"));
        assert_eq!(batch.phase(), BatchPhase::Active);
        assert!(batch.transition_to(BatchPhase::Summarizing).is_ok());
        assert_eq!(batch.phase(), BatchPhase::Summarizing);
        assert!(batch.transition_to(BatchPhase::Archived).is_ok());
        assert!(batch.transition_to(BatchPhase::Active).is_err());
    }

    #[test]
    fn batch_depth_limit() {
        let mut batch = DispatchBatch::new(Some("b1"));
        batch.register("t1", "w1", make_contract("task1", &[])).unwrap();
        batch.register("t2", "w1", make_contract("task2", &["t1"])).unwrap();
        batch.register("t3", "w1", make_contract("task3", &["t2"])).unwrap();

        assert!(batch.validate_depth_limit(3).is_ok());
        assert!(batch.validate_depth_limit(10).is_ok());
        assert!(batch.validate_depth_limit(1).is_err());
    }
}

// ========== Dispatch Idempotency Tests ==========

mod dispatch_idempotency_tests {
    use crate::dispatch::{
        DispatchIdempotencyClaimInput, DispatchIdempotencyStatus, DispatchIdempotencyStore,
    };

    fn claim_input(key: &str, task_id: &str) -> DispatchIdempotencyClaimInput {
        DispatchIdempotencyClaimInput {
            key: key.to_string(),
            session_id: "s1".to_string(),
            mission_id: "m1".to_string(),
            task_id: task_id.to_string(),
            worker: "w1".to_string(),
            ownership: "general".to_string(),
            mode: "implement".to_string(),
            task_name: "test".to_string(),
            routing_reason: "direct".to_string(),
            degraded: false,
            status: DispatchIdempotencyStatus::Dispatched,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn claim_new_record() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        let result = store.claim_or_get(claim_input("k1", "t1"));
        assert!(result.claimed);
        assert_eq!(result.record.key, "k1");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn claim_duplicate_returns_existing() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        let r1 = store.claim_or_get(claim_input("k1", "t1"));
        assert!(r1.claimed);

        let r2 = store.claim_or_get(claim_input("k1", "t2"));
        assert!(!r2.claimed);
        assert_eq!(r2.record.task_id, "t1");
    }

    #[test]
    fn resolve_by_key_and_task_id() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        store.claim_or_get(claim_input("k1", "t1"));

        assert!(store.resolve_by_key("k1").is_some());
        assert!(store.resolve_by_task_id("t1").is_some());
        assert!(store.resolve_by_key("k2").is_none());
    }

    #[test]
    fn update_status() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        store.claim_or_get(claim_input("k1", "t1"));

        let updated = store.update_status_by_task_id("t1", DispatchIdempotencyStatus::Completed);
        assert!(updated.is_some());
        assert_eq!(
            updated.unwrap().status,
            DispatchIdempotencyStatus::Completed
        );
    }

    #[test]
    fn remove_by_task_id() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        store.claim_or_get(claim_input("k1", "t1"));
        assert_eq!(store.len(), 1);

        assert!(store.remove_by_task_id("t1"));
        assert_eq!(store.len(), 0);
        assert!(store.resolve_by_key("k1").is_none());
    }

    #[test]
    fn clear_all() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        store.claim_or_get(claim_input("k1", "t1"));
        store.claim_or_get(claim_input("k2", "t2"));
        assert_eq!(store.len(), 2);

        store.clear();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn empty_key_returns_none() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        assert!(store.resolve_by_key("").is_none());
        assert!(store.resolve_by_key("  ").is_none());
        assert!(store.resolve_by_task_id("").is_none());
    }

    #[test]
    fn ownership_and_mode_normalization() {
        let mut store = DispatchIdempotencyStore::new(None, None);
        let mut input = claim_input("k1", "t1");
        input.ownership = "  ".to_string();
        input.mode = "".to_string();
        let result = store.claim_or_get(input);
        assert_eq!(result.record.ownership, "general");
        assert_eq!(result.record.mode, "implement");
    }
}

// ========== Dispatch Routing Tests ==========

mod dispatch_routing_tests {
    use crate::dispatch::DispatchRoutingService;
    use std::collections::{HashMap, HashSet};

    fn make_routing() -> DispatchRoutingService {
        let workers = vec!["w1".to_string(), "w2".to_string(), "w3".to_string()];
        let mut fallback = HashMap::new();
        fallback.insert("w1".to_string(), vec!["w2".to_string(), "w3".to_string()]);
        fallback.insert("w2".to_string(), vec!["w1".to_string()]);
        DispatchRoutingService::new(workers, fallback, 5000)
    }

    #[test]
    fn resolve_preferred_available() {
        let mut router = make_routing();
        let result = router.resolve_execution_worker("w1", None, None, false);
        assert!(result.ok);
        assert_eq!(result.selected_worker.as_deref(), Some("w1"));
        assert!(!result.degraded);
    }

    #[test]
    fn resolve_fallback_when_busy() {
        let mut router = make_routing();
        let busy: HashSet<String> = ["w1".to_string()].into_iter().collect();
        let result = router.resolve_execution_worker("w1", Some(&busy), None, true);
        assert!(result.ok);
        assert_eq!(result.selected_worker.as_deref(), Some("w2"));
        assert!(result.degraded);
    }

    #[test]
    fn fail_when_busy_no_fallback_allowed() {
        let mut router = make_routing();
        let busy: HashSet<String> = ["w1".to_string()].into_iter().collect();
        let result = router.resolve_execution_worker("w1", Some(&busy), None, false);
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    #[test]
    fn runtime_unavailable_triggers_fallback() {
        let mut router = make_routing();
        router.mark_worker_runtime_unavailable("w1", "rate limit");
        let result = router.resolve_execution_worker("w1", None, None, true);
        assert!(result.ok);
        assert_eq!(result.selected_worker.as_deref(), Some("w2"));
        assert!(result.degraded);
    }

    #[test]
    fn clear_runtime_unavailable() {
        let mut router = make_routing();
        router.mark_worker_runtime_unavailable("w1", "test");
        router.clear_worker_runtime_unavailable("w1");
        let result = router.resolve_execution_worker("w1", None, None, false);
        assert!(result.ok);
        assert_eq!(result.selected_worker.as_deref(), Some("w1"));
    }

    #[test]
    fn should_mark_runtime_unavailable_patterns() {
        assert!(DispatchRoutingService::should_mark_runtime_unavailable("rate limit exceeded"));
        assert!(DispatchRoutingService::should_mark_runtime_unavailable("Unauthorized"));
        assert!(DispatchRoutingService::should_mark_runtime_unavailable("503 service unavailable"));
        assert!(DispatchRoutingService::should_mark_runtime_unavailable("connection timeout"));
        assert!(!DispatchRoutingService::should_mark_runtime_unavailable(""));
        assert!(!DispatchRoutingService::should_mark_runtime_unavailable("normal error message"));
    }

    #[test]
    fn excluded_workers_skipped() {
        let mut router = make_routing();
        let excluded: HashSet<String> = ["w1".to_string()].into_iter().collect();
        let result = router.resolve_execution_worker("w1", None, Some(&excluded), true);
        assert!(result.ok);
        assert_eq!(result.selected_worker.as_deref(), Some("w2"));
        assert!(result.degraded);
    }
}

// ========== Dispatch Completion Queue Tests ==========

mod dispatch_completion_queue_tests {
    use crate::dispatch::DispatchCompletionQueue;
    use std::collections::HashSet;

    #[test]
    fn push_and_drain_all() {
        let mut queue = DispatchCompletionQueue::new();
        queue.push("t1", "w1", "completed", "done", vec![], None);
        queue.push("t2", "w2", "failed", "error", vec![], Some(vec!["err".to_string()]));

        assert_eq!(queue.len(), 2);

        let results = queue.drain_all();
        assert_eq!(results.len(), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn push_dedup() {
        let mut queue = DispatchCompletionQueue::new();
        queue.push("t1", "w1", "completed", "done", vec![], None);
        queue.push("t1", "w1", "completed", "done again", vec![], None);

        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn drain_for_targets() {
        let mut queue = DispatchCompletionQueue::new();
        queue.push("t1", "w1", "completed", "done1", vec![], None);
        queue.push("t2", "w2", "completed", "done2", vec![], None);
        queue.push("t3", "w3", "completed", "done3", vec![], None);

        let targets: HashSet<String> = ["t1".to_string(), "t3".to_string()].into_iter().collect();
        let matched = queue.drain_for_targets(&targets, None);

        assert_eq!(matched.len(), 2);
        assert_eq!(queue.len(), 1);

        let remaining = queue.drain_all();
        assert_eq!(remaining[0].task_id, "t2");
    }

    #[test]
    fn reset_clears_everything() {
        let mut queue = DispatchCompletionQueue::new();
        queue.push("t1", "w1", "completed", "done", vec![], None);
        queue.reset();
        assert!(queue.is_empty());

        queue.push("t1", "w1", "completed", "done again", vec![], None);
        assert_eq!(queue.len(), 1);
    }
}

// ========== Risk Policy Tests ==========

mod risk_policy_tests {
    use crate::risk_policy::*;
    use magi_core::RiskLevel;

    #[test]
    fn zero_files_low_risk() {
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "simple change",
            analysis: None,
            feature_contract: None,
            sub_task_count: 0,
            target_files: &[],
            acceptance_criteria_count: 0,
            failure_rate: None,
        });
        assert_eq!(assessment.level, RiskLevel::Low);
        assert_eq!(assessment.path, RiskPath::Light);
        assert!(!assessment.hard_stop);
    }

    #[test]
    fn many_files_high_risk() {
        let files: Vec<String> = (0..10).map(|i| format!("mod{i}/file.rs")).collect();
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "修改 API 接口的大型重构",
            analysis: None,
            feature_contract: None,
            sub_task_count: 0,
            target_files: &files,
            acceptance_criteria_count: 0,
            failure_rate: None,
        });
        assert_eq!(assessment.level, RiskLevel::High);
        assert_eq!(assessment.path, RiskPath::Full);
        assert!(assessment.hard_stop);
        assert_eq!(assessment.verification, VerificationLevel::Full);
    }

    #[test]
    fn interface_change_detected() {
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "修改 API 接口的请求字段",
            analysis: None,
            feature_contract: None,
            sub_task_count: 1,
            target_files: &["src/api.rs".to_string()],
            acceptance_criteria_count: 0,
            failure_rate: None,
        });
        assert!(assessment.signals.contains(&"interface_change".to_string()));
        assert!(assessment.score >= 7);
    }

    #[test]
    fn config_change_detected() {
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "update deps",
            analysis: None,
            feature_contract: None,
            sub_task_count: 0,
            target_files: &["Cargo.toml".to_string()],
            acceptance_criteria_count: 0,
            failure_rate: None,
        });
        assert!(assessment.signals.contains(&"config_or_dependency_change".to_string()));
    }

    #[test]
    fn failure_rate_increases_risk() {
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "fix",
            analysis: None,
            feature_contract: None,
            sub_task_count: 0,
            target_files: &["src/a.rs".to_string()],
            acceptance_criteria_count: 0,
            failure_rate: Some(0.5),
        });
        assert!(assessment.signals.iter().any(|s| s.starts_with("failure_rate")));
    }

    #[test]
    fn unknown_file_scope_penalized() {
        let assessment = evaluate_risk(&RiskPolicyInput {
            prompt: "do something",
            analysis: None,
            feature_contract: None,
            sub_task_count: 3,
            target_files: &[],
            acceptance_criteria_count: 0,
            failure_rate: None,
        });
        assert!(assessment.signals.contains(&"unknown_file_scope".to_string()));
    }
}

// ========== Verification Policy Tests ==========

mod verification_policy_tests {
    use crate::verification_policy::*;

    #[test]
    fn standard_mode_default() {
        let mode = resolve_verification_mode(&VerificationModeInput {
            task_title: Some("implement auth feature"),
            goal: None,
            analysis: None,
            acceptance: None,
            constraints: None,
            context: None,
        });
        assert_eq!(mode, VerificationMode::Standard);
    }

    #[test]
    fn smoke_mode_with_smoke_and_low_cost() {
        let mode = resolve_verification_mode(&VerificationModeInput {
            task_title: Some("smoke test 简单验证"),
            goal: None,
            analysis: None,
            acceptance: None,
            constraints: None,
            context: None,
        });
        assert_eq!(mode, VerificationMode::Smoke);
    }

    #[test]
    fn smoke_mode_with_smoke_and_quick() {
        let mode = resolve_verification_mode(&VerificationModeInput {
            task_title: Some("快速验证 快速完成"),
            goal: None,
            analysis: None,
            acceptance: None,
            constraints: None,
            context: None,
        });
        assert_eq!(mode, VerificationMode::Smoke);
    }

    #[test]
    fn empty_input_returns_standard() {
        let mode = resolve_verification_mode(&VerificationModeInput {
            task_title: None,
            goal: None,
            analysis: None,
            acceptance: None,
            constraints: None,
            context: None,
        });
        assert_eq!(mode, VerificationMode::Standard);
    }

    #[test]
    fn is_smoke_helper() {
        assert!(is_smoke_verification_input(&VerificationModeInput {
            task_title: Some("烟测 简单"),
            goal: None,
            analysis: None,
            acceptance: None,
            constraints: None,
            context: None,
        }));
    }

    #[test]
    fn smoke_overrides_disable_compile_and_test() {
        let overrides = resolve_verification_config_overrides(VerificationMode::Smoke);
        assert!(overrides.is_some());
        let config = overrides.unwrap();
        assert!(!config.compile_check);
        assert!(!config.lint_check);
        assert!(!config.test_check);
        assert!(config.ide_check);
    }

    #[test]
    fn standard_overrides_returns_none() {
        let overrides = resolve_verification_config_overrides(VerificationMode::Standard);
        assert!(overrides.is_none());
    }
}

mod plan_ledger_tests {
    use crate::plan_ledger::*;

    fn draft_input(session: &str, turn: &str) -> CreatePlanDraftInput {
        CreatePlanDraftInput {
            session_id: session.to_string(),
            turn_id: turn.to_string(),
            mission_id: None,
            mode: PlanMode::Standard,
            prompt: "实现用户认证".to_string(),
            summary: None,
            analysis: None,
            acceptance_criteria: None,
            constraints: None,
            risk_level: None,
            formatted_plan: None,
        }
    }

    fn item_input(item_id: &str, title: &str, worker: &str) -> DispatchPlanItemInput {
        DispatchPlanItemInput {
            item_id: item_id.to_string(),
            title: title.to_string(),
            worker: worker.to_string(),
            category: None,
            depends_on: None,
            scope_hints: None,
            target_files: None,
            requires_modification: None,
        }
    }

    #[test]
    fn create_draft_and_query() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        assert_eq!(plan.status, PlanStatus::Draft);
        assert_eq!(plan.version, 1);
        assert_eq!(plan.summary, "实现用户认证");

        let loaded = svc.get_plan("s1", &plan.plan_id);
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().plan_id, plan.plan_id);
    }

    #[test]
    fn create_duplicate_turn_supersedes_previous() {
        let mut svc = PlanLedgerService::new();
        let p1 = svc.create_draft(draft_input("s1", "t1"));
        let p2 = svc.create_draft(draft_input("s1", "t1"));

        assert_eq!(p2.version, 2);
        assert_eq!(p2.parent_plan_id, Some(p1.plan_id.clone()));

        let loaded_p1 = svc.get_plan("s1", &p1.plan_id).unwrap();
        assert_eq!(loaded_p1.status, PlanStatus::Superseded);
    }

    #[test]
    fn approve_and_reject_flow() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));

        let approved = svc.approve("s1", &plan.plan_id, None, Some("看起来不错")).unwrap();
        assert_eq!(approved.status, PlanStatus::Approved);
        assert_eq!(approved.review.as_ref().unwrap().status, PlanReviewStatus::Approved);

        let mut svc2 = PlanLedgerService::new();
        let plan2 = svc2.create_draft(draft_input("s1", "t2"));
        let rejected = svc2.reject("s1", &plan2.plan_id, None, Some("方案不合理")).unwrap();
        assert_eq!(rejected.status, PlanStatus::Rejected);
        assert_eq!(rejected.review.as_ref().unwrap().reason.as_deref(), Some("方案不合理"));
    }

    #[test]
    fn mark_executing_and_finalize() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None).unwrap();
        let executing = svc.mark_executing("s1", &plan.plan_id).unwrap();
        assert_eq!(executing.status, PlanStatus::Executing);

        let finalized = svc.finalize("s1", &plan.plan_id, PlanStatus::Completed).unwrap();
        assert_eq!(finalized.status, PlanStatus::Completed);
        assert_eq!(finalized.runtime.acceptance.summary, PlanAcceptanceSummary::Passed);
    }

    #[test]
    fn terminal_plan_rejects_mutations() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None);
        svc.mark_executing("s1", &plan.plan_id);
        svc.finalize("s1", &plan.plan_id, PlanStatus::Completed);

        assert!(svc.update_summary("s1", &plan.plan_id, "新摘要").is_none());
        assert!(svc.mark_executing("s1", &plan.plan_id).is_none());
    }

    #[test]
    fn recovery_protection_blocks_terminal() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None);
        svc.mark_executing("s1", &plan.plan_id);
        svc.set_recovery_protection("s1", &plan.plan_id, true);

        let _result = svc.finalize("s1", &plan.plan_id, PlanStatus::Failed);
        let loaded = svc.get_plan("s1", &plan.plan_id).unwrap();
        assert_eq!(loaded.status, PlanStatus::Executing);
    }

    #[test]
    fn recovery_resume_from_terminal() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None);
        svc.mark_executing("s1", &plan.plan_id);
        svc.set_recovery_protection("s1", &plan.plan_id, true);

        svc.force_status_for_test("s1", &plan.plan_id, PlanStatus::Failed);

        let resumed = svc.mark_executing("s1", &plan.plan_id).unwrap();
        assert_eq!(resumed.status, PlanStatus::Executing);
        assert!(!resumed.recovery_protected);
    }

    #[test]
    fn upsert_dispatch_item_creates_and_updates() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));

        let updated = svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("item-1", "编写接口", "worker-a")).unwrap();
        assert_eq!(updated.items.len(), 1);
        assert_eq!(updated.items[0].title, "编写接口");
        assert!(updated.status == PlanStatus::Approved);

        let updated2 = svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("item-1", "修改接口", "worker-b")).unwrap();
        assert_eq!(updated2.items.len(), 1);
        assert_eq!(updated2.items[0].title, "修改接口");
        assert_eq!(updated2.items[0].owner, "worker-b");
    }

    #[test]
    fn assignment_status_updates_plan_status() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a1", "任务A", "w1"));
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a2", "任务B", "w2"));
        svc.mark_executing("s1", &plan.plan_id);

        svc.update_assignment_status("s1", &plan.plan_id, "a1", PlanItemStatus::Completed);
        let after_one = svc.get_plan("s1", &plan.plan_id).unwrap();
        assert_eq!(after_one.status, PlanStatus::Executing);

        svc.update_assignment_status("s1", &plan.plan_id, "a2", PlanItemStatus::Completed);
        let after_all = svc.get_plan("s1", &plan.plan_id).unwrap();
        assert_eq!(after_all.status, PlanStatus::Completed);
    }

    #[test]
    fn partial_completion_on_mixed_results() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a1", "任务A", "w1"));
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a2", "任务B", "w2"));
        svc.mark_executing("s1", &plan.plan_id);

        svc.update_assignment_status("s1", &plan.plan_id, "a1", PlanItemStatus::Completed);
        svc.update_assignment_status("s1", &plan.plan_id, "a2", PlanItemStatus::Failed);
        let result = svc.get_plan("s1", &plan.plan_id).unwrap();
        assert_eq!(result.status, PlanStatus::PartiallyCompleted);
    }

    #[test]
    fn task_status_drives_item_progress() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a1", "任务A", "w1"));
        svc.mark_executing("s1", &plan.plan_id);

        svc.update_task_status("s1", &plan.plan_id, "a1", "todo-1", "pending");
        svc.update_task_status("s1", &plan.plan_id, "a1", "todo-2", "pending");

        svc.update_task_status("s1", &plan.plan_id, "a1", "todo-1", "completed");
        let result = svc.get_plan("s1", &plan.plan_id).unwrap();
        let item = &result.items[0];
        assert_eq!(item.progress, 50.0);
        assert_eq!(item.task_ids.len(), 2);

        svc.update_task_status("s1", &plan.plan_id, "a1", "todo-2", "completed");
        let result2 = svc.get_plan("s1", &plan.plan_id).unwrap();
        assert_eq!(result2.items[0].progress, 100.0);
        assert_eq!(result2.items[0].status, PlanItemStatus::Completed);
    }

    #[test]
    fn start_and_complete_attempt() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None);
        svc.mark_executing("s1", &plan.plan_id);

        let started = svc.start_attempt("s1", &plan.plan_id, PlanAttemptStartInput {
            scope: PlanAttemptScope::Orchestrator,
            target_id: None,
            assignment_id: None,
            task_id: None,
            reason: Some("开始执行".to_string()),
        }).unwrap();
        assert_eq!(started.attempts.len(), 1);
        assert_eq!(started.attempts[0].status, PlanAttemptStatus::Inflight);

        let completed = svc.complete_latest_attempt("s1", &plan.plan_id, PlanAttemptCompleteInput {
            scope: PlanAttemptScope::Orchestrator,
            target_id: None,
            assignment_id: None,
            task_id: None,
            status: PlanAttemptStatus::Succeeded,
            error: None,
            evidence_ids: None,
        }).unwrap();
        assert_eq!(completed.attempts[0].status, PlanAttemptStatus::Succeeded);
        assert!(completed.attempts[0].ended_at.is_some());
    }

    #[test]
    fn bind_mission_and_query_by_mission() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.bind_mission("s1", &plan.plan_id, "mission-42");

        let found = svc.get_latest_plan_by_mission("s1", "mission-42", false);
        assert!(found.is_some());
        assert_eq!(found.unwrap().plan_id, plan.plan_id);

        let not_found = svc.get_latest_plan_by_mission("s1", "mission-99", false);
        assert!(not_found.is_none());
    }

    #[test]
    fn list_plans_and_active_plan() {
        let mut svc = PlanLedgerService::new();
        let p1 = svc.create_draft(draft_input("s1", "t1"));
        let p2 = svc.create_draft(draft_input("s1", "t2"));

        let all = svc.list_plans("s1", 10);
        assert_eq!(all.len(), 2);

        let active = svc.get_active_plan("s1");
        assert!(active.is_some());

        svc.approve("s1", &p2.plan_id, None, None);
        svc.mark_executing("s1", &p2.plan_id);
        svc.finalize("s1", &p2.plan_id, PlanStatus::Completed);

        let active2 = svc.get_active_plan("s1");
        assert!(active2.is_some());
        assert_eq!(active2.unwrap().plan_id, p1.plan_id);
    }

    #[test]
    fn format_plan_display() {
        let mut svc = PlanLedgerService::new();
        let mut input = draft_input("s1", "t1");
        input.summary = Some("测试计划".to_string());
        input.analysis = Some("分析内容".to_string());
        input.constraints = Some(vec!["约束1".to_string()]);
        input.acceptance_criteria = Some(vec!["验收标准1".to_string()]);
        let plan = svc.create_draft(input);
        svc.upsert_dispatch_item("s1", &plan.plan_id, item_input("a1", "任务A", "worker-1"));
        let loaded = svc.get_plan("s1", &plan.plan_id).unwrap();
        let display = PlanLedgerService::format_plan_for_display(loaded);
        assert!(display.contains("测试计划"));
        assert!(display.contains("分析内容"));
        assert!(display.contains("约束1"));
        assert!(display.contains("验收标准1"));
        assert!(display.contains("[worker-1] 任务A"));
    }

    #[test]
    fn reconcile_by_missions() {
        let mut svc = PlanLedgerService::new();
        let p1 = svc.create_draft(draft_input("s1", "t1"));
        svc.bind_mission("s1", &p1.plan_id, "m1");
        svc.approve("s1", &p1.plan_id, None, None);
        svc.mark_executing("s1", &p1.plan_id);

        let count = svc.reconcile_by_missions("s1", &[("m1", "completed")]);
        assert_eq!(count, 1);

        let loaded = svc.get_plan("s1", &p1.plan_id).unwrap();
        assert!(loaded.status.is_terminal());
    }

    #[test]
    fn runtime_review_state_update() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None);
        svc.mark_executing("s1", &plan.plan_id);

        let updated = svc.update_runtime_review("s1", &plan.plan_id, ReviewState::Running, Some(1)).unwrap();
        assert_eq!(updated.runtime.review.state, ReviewState::Running);
        assert_eq!(updated.runtime.review.round, 1);
        assert!(updated.runtime.review.last_reviewed_at.is_some());
    }

    #[test]
    fn runtime_acceptance_update() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));

        let updated = svc.update_runtime_acceptance(
            "s1",
            &plan.plan_id,
            Some(vec![
                AcceptanceCriterion { description: "编译通过".to_string(), met: true },
                AcceptanceCriterion { description: "测试通过".to_string(), met: false },
            ]),
            None,
        ).unwrap();
        assert_eq!(updated.runtime.acceptance.summary, PlanAcceptanceSummary::Partial);
        assert_eq!(updated.runtime.acceptance.criteria.len(), 2);
    }
}

mod auto_learning_tests {
    use crate::auto_learning::*;

    #[test]
    fn preference_miner_correction() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(
            &["不要添加注释", "必须使用中文"],
            &["好的"],
        );
        assert!(result.preferences.len() >= 2);
        let correction = result.preferences.iter().find(|p| p.pattern.contains("注释")).unwrap();
        assert_eq!(correction.category, preference_miner::PreferenceCategory::Constraint);
        assert!(correction.confidence >= 0.7);
    }

    #[test]
    fn preference_miner_mandatory() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(
            &["务必先跑测试再提交"],
            &[],
        );
        assert!(!result.preferences.is_empty());
        let mandatory = &result.preferences[0];
        assert!(mandatory.pattern.contains("先跑测试再提交"));
    }

    #[test]
    fn preference_miner_format() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(
            &["请用中文回复"],
            &[],
        );
        let format_pref = result.preferences.iter().find(|p| p.pattern.contains("中文"));
        assert!(format_pref.is_some());
    }

    #[test]
    fn preference_miner_style() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(
            &["简单点说"],
            &[],
        );
        let style_pref = result.preferences.iter().find(|p| p.pattern.contains("简洁"));
        assert!(style_pref.is_some());
    }

    #[test]
    fn preference_miner_repetition() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(
            &["继续", "继续", "继续", "继续"],
            &["好的", "好的", "好的"],
        );
        let workflow = result.preferences.iter().find(|p| p.category == preference_miner::PreferenceCategory::Workflow);
        assert!(workflow.is_some());
    }

    #[test]
    fn preference_miner_empty_input() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(&[], &[]);
        assert!(result.preferences.is_empty());
        assert!(!result.has_new_high_signal);
    }

    #[test]
    fn consolidation_basic() {
        let mut svc = MemoryConsolidationService::new(None);
        assert!(!svc.should_consolidate());

        for i in 0..5 {
            svc.add_entry(memory_consolidation::RawMemoryInput {
                content: format!("架构设计决策 {i}"),
                citations: vec!["session:s1".to_string()],
                created_at: now_millis(),
                confidence: 0.8,
            });
        }

        assert!(svc.should_consolidate());
        let result = svc.consolidate();
        assert_eq!(result.processed_entries, 5);
        assert!(result.updated_topics > 0);
        assert_eq!(result.forgotten_entries, 0);
    }

    #[test]
    fn consolidation_dedup() {
        let mut svc = MemoryConsolidationService::new(None);
        let now = now_millis();

        for _ in 0..5 {
            svc.add_entry(memory_consolidation::RawMemoryInput {
                content: "使用 Rust 重写核心模块".to_string(),
                citations: vec!["session:s1".to_string()],
                created_at: now,
                confidence: 0.8,
            });
        }

        let result = svc.consolidate();
        assert_eq!(result.processed_entries, 5);
    }

    #[test]
    fn auto_learning_capture() {
        let mut mgr = AutoLearningManager::new();
        let input = AutoLearningCaptureInput {
            session_id: "s1".to_string(),
            mission_id: Some("m1".to_string()),
            request_id: None,
            turn_id: Some("t1".to_string()),
            final_status: "completed".to_string(),
            runtime_reason: None,
            errors: Vec::new(),
            delivery_summary: Some("完成用户认证".to_string()),
        };

        let raw = mgr.capture(
            &input,
            &["请实现用户认证"],
            &["好的，我来实现"],
            "完成用户认证功能",
            vec!["使用 JWT 方案".to_string()],
            vec!["JWT 适合无状态认证".to_string()],
            vec![],
        );

        assert_eq!(raw.session_id, "s1");
        assert_eq!(raw.final_status, "completed");
        assert_eq!(mgr.raw_memory_count(), 1);
        assert!(mgr.pending_consolidation_count() > 0);
    }

    #[test]
    fn auto_learning_consolidation_trigger() {
        let mut mgr = AutoLearningManager::new();
        let input = AutoLearningCaptureInput {
            session_id: "s1".to_string(),
            mission_id: None,
            request_id: None,
            turn_id: Some("t1".to_string()),
            final_status: "completed".to_string(),
            runtime_reason: None,
            errors: Vec::new(),
            delivery_summary: None,
        };

        for i in 0..3 {
            mgr.capture(
                &input,
                &["继续"],
                &["好的"],
                &format!("摘要 {i}"),
                vec![format!("决策 {i}")],
                vec![format!("学习 {i}")],
                vec![format!("警告 {i}")],
            );
        }

        let result = mgr.try_consolidate();
        assert!(result.is_some());
        assert!(result.unwrap().processed_entries > 0);
    }

    fn now_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

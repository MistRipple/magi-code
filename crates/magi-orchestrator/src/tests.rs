use super::*;
use crate::task_store::TaskStore;
use magi_core::{
    AccessProfile, RiskLevel, Task, TaskKind, TaskPolicy, TaskResultKind, TaskStatus, TaskTier,
    TerminationReason, ToolCallId, VerificationStatus, WorkerId,
};
use magi_event_bus::InMemoryEventBus;
use magi_governance::{DecisionPhase, GovernanceDecision, GovernanceService, WorkerControlKind};
use magi_skill_runtime::{SkillDispatchRoute, SkillDispatchRuntime};
use magi_tool_runtime::{ToolExecutionSummary, ToolRegistry};
use magi_worker_runtime::{
    LocalProcessExecutorCapability, LocalProcessExecutorHealth, LocalProcessExecutorHealthStatus,
    LocalProcessWorkerExecutor, SkillDispatchSummary, WorkerExecutionFinalReport,
    WorkerExecutionStepKind, WorkerExecutionTrace, WorkerExecutor, WorkerExecutorFailure,
    WorkerExecutorKind, WorkerExecutorProbe, WorkerGovernanceObservation, WorkerRuntime,
    WorkerSkillDispatchObservation,
};
use std::sync::{Arc, Mutex};

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
        kind: TaskKind::LocalAgent,
        title: mission_title.to_string(),
        goal: mission_title.to_string(),
        status: TaskStatus::Running,
        dependency_ids: Vec::new(),
        required_children: tasks
            .iter()
            .map(|(task_id, _, _)| task_id.clone())
            .collect(),
        policy_snapshot: None,
        executor_binding: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: magi_core::TaskRuntimePayload::default(),
        created_at: now,
        updated_at: now,
    });
    for (task_id, title, status) in tasks {
        task_store.insert_task(Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(root_task_id.clone()),
            kind: TaskKind::LocalAgent,
            title: (*title).to_string(),
            goal: (*title).to_string(),
            status: *status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        });
    }
    root_task_id
}

fn test_task_policy(
    access_profile: AccessProfile,
    command_mode: &str,
    allowed_tools: Vec<String>,
    denied_tools: Vec<String>,
) -> TaskPolicy {
    TaskPolicy {
        autonomy_level: "assisted".to_string(),
        access_profile,
        allowed_tools,
        denied_tools,
        allowed_paths: vec!["/tmp/worker-allowed".to_string()],
        denied_paths: vec!["/tmp/worker-denied".to_string()],
        network_mode: "default".to_string(),
        command_mode: command_mode.to_string(),
        retry_limit: 0,
        validation_profile: None,
        checkpoint_mode: "none".to_string(),
        task_tier: TaskTier::ExecutionChain,
        background_allowed: false,
        escalation_conditions: Vec::new(),
    }
}

fn seed_task_hierarchy(
    task_store: &TaskStore,
    mission_id: &MissionId,
    mission_title: &str,
    root_status: TaskStatus,
    tasks: &[(TaskId, Option<TaskId>, &str, TaskStatus)],
) -> TaskId {
    let root_task_id = root_task_id_for_mission(mission_id);
    let now = UtcMillis::now();
    let mut child_map: std::collections::HashMap<TaskId, Vec<TaskId>> =
        std::collections::HashMap::new();
    for (task_id, parent_task_id, _, _) in tasks {
        let parent = parent_task_id
            .clone()
            .unwrap_or_else(|| root_task_id.clone());
        child_map.entry(parent).or_default().push(task_id.clone());
    }

    task_store.insert_task(Task {
        task_id: root_task_id.clone(),
        mission_id: mission_id.clone(),
        root_task_id: root_task_id.clone(),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: mission_title.to_string(),
        goal: mission_title.to_string(),
        status: root_status,
        dependency_ids: Vec::new(),
        required_children: child_map.get(&root_task_id).cloned().unwrap_or_default(),
        policy_snapshot: None,
        executor_binding: None,
        knowledge_refs: Vec::new(),
        workspace_scope: None,
        write_scope: None,
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        evidence_refs: Vec::new(),
        retry_count: 0,
        runtime_payload: magi_core::TaskRuntimePayload::default(),
        created_at: now,
        updated_at: now,
    });

    for (task_id, parent_task_id, title, status) in tasks {
        let parent = parent_task_id
            .clone()
            .unwrap_or_else(|| root_task_id.clone());
        task_store.insert_task(Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_task_id.clone(),
            parent_task_id: Some(parent.clone()),
            kind: TaskKind::LocalAgent,
            title: (*title).to_string(),
            goal: (*title).to_string(),
            status: *status,
            dependency_ids: Vec::new(),
            required_children: child_map.get(task_id).cloned().unwrap_or_default(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: magi_core::TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        });
    }

    root_task_id
}

struct TaskProjectionOverviewFixture<'a> {
    mission_id: &'a MissionId,
    root_task_id: &'a TaskId,
    worker_summary: WorkerRuntimeSummary,
    tool_summary: ToolExecutionSummary,
    skill_dispatch_observations: &'a [WorkerSkillDispatchObservation],
    governance_observations: &'a [WorkerGovernanceObservation],
    context_summary: Option<ExecutionContextSummary>,
}

fn build_task_projection_overview(
    service: &OrchestratorService,
    task_store: &TaskStore,
    fixture: TaskProjectionOverviewFixture<'_>,
) -> ExecutionOverview {
    let TaskProjectionOverviewFixture {
        mission_id,
        root_task_id,
        worker_summary,
        tool_summary,
        skill_dispatch_observations,
        governance_observations,
        context_summary,
    } = fixture;
    service
        .build_execution_overview_from_task_projection(
            task_store,
            execution_overview::ExecutionOverviewProjectionInput {
                target: &TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: root_task_id.clone(),
                    task_id: root_task_id.clone(),
                    requested_worker_id: None,
                    recovery_id: None,
                    execution_chain_ref: None,
                },
                session_id: None,
                workspace_id: None,
                worker_summary,
                tool_summary,
                skill_dispatch_observations,
                governance_observations,
                context_summary,
            },
        )
        .expect("task projection overview should be built")
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

#[test]
fn execution_overview_exports_context_consumption_into_runtime_read_model() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(event_bus.clone());
    let task_store = TaskStore::new();

    let mission_id = MissionId::new("mission-context");
    let task_id = TaskId::new("todo-context");
    let root_task_id = seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
                workspace_id: None,
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
    let context_summary = ExecutionContextSummary::from_context_assembly(&context_result);

    let overview = build_task_projection_overview(
        &service,
        &task_store,
        TaskProjectionOverviewFixture {
            mission_id: &mission_id,
            root_task_id: &root_task_id,
            worker_summary: worker_summary(0),
            tool_summary: ToolExecutionSummary::default(),
            skill_dispatch_observations: &[],
            governance_observations: &[],
            context_summary: Some(context_summary.clone()),
        },
    );

    let overview_context = overview
        .context_summary
        .as_ref()
        .expect("context summary should be exported");
    assert_eq!(overview_context.used_turns, 1);
    assert_eq!(overview_context.used_knowledge, 1);
    assert_eq!(overview_context.used_memory, 2);
    assert_eq!(overview_context.recent_turn_resolved_count, 1);
    assert_eq!(overview_context.recent_turn_retained_count, 1);
    assert_eq!(overview_context.recent_turn_provided_source_count, 1);
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
    assert_eq!(
        overview_context.shared_context_ids,
        vec!["shared-1".to_string()]
    );
    assert_eq!(
        overview_context.file_summary_paths,
        vec!["/tmp/parser.rs".to_string()]
    );

    let read_model = event_bus.runtime_read_model_input();
    let mission_entry = read_model
        .details
        .execution_groups
        .iter()
        .find(|entry| entry.mission_id == mission_id.to_string())
        .expect("execution group entry should exist");
    assert_eq!(mission_entry.context_used_turn_count, 1);
    assert_eq!(mission_entry.context_used_knowledge_count, 1);
    assert_eq!(mission_entry.context_used_memory_count, 2);
    assert_eq!(mission_entry.context_recent_turn_resolved_count, 1);
    assert_eq!(mission_entry.context_recent_turn_retained_count, 1);
    assert_eq!(mission_entry.context_recent_turn_provided_source_count, 1);
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
    assert_eq!(
        mission_entry.context_shared_context_ids,
        vec!["shared-1".to_string()]
    );
    assert_eq!(
        mission_entry.context_file_summary_paths,
        vec!["/tmp/parser.rs".to_string()]
    );
    assert_eq!(
        read_model
            .overview
            .diagnostics
            .context_execution_group_count,
        1
    );
    assert_eq!(
        read_model.overview.diagnostics.context_used_knowledge_count,
        1
    );
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
fn agent_run_projection_aggregates_governance_summaries_by_layer() {
    let event_bus = Arc::new(InMemoryEventBus::new(16));
    let service = OrchestratorService::new(event_bus);
    let task_store = TaskStore::new();

    let mission_id = MissionId::new("mission-5");
    let assignment_one = AssignmentId::new("assignment-5a");
    let assignment_two = AssignmentId::new("assignment-5b");
    let task_one = TaskId::new(assignment_one.as_str());
    let task_two = TaskId::new("todo-5a-2");
    let task_three = TaskId::new(assignment_two.as_str());
    let root_task_id = seed_task_hierarchy(
        &task_store,
        &mission_id,
        "mission",
        TaskStatus::Failed,
        &[
            (task_one.clone(), None, "todo-a-1", TaskStatus::Completed),
            (
                task_two.clone(),
                Some(task_one.clone()),
                "todo-a-2",
                TaskStatus::Completed,
            ),
            (task_three.clone(), None, "todo-b-1", TaskStatus::Failed),
        ],
    );

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

    let overview = build_task_projection_overview(
        &service,
        &task_store,
        TaskProjectionOverviewFixture {
            mission_id: &mission_id,
            root_task_id: &root_task_id,
            worker_summary: worker_summary(0),
            tool_summary: ToolExecutionSummary::default(),
            skill_dispatch_observations: &[],
            governance_observations: &governance_observations,
            context_summary: None,
        },
    );

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
    let task_one_summary = overview
        .task_governance_summaries
        .iter()
        .find(|summary| summary.task_id == task_one)
        .expect("task one governance summary should exist");
    assert_eq!(task_one_summary.governance_summary.blocked, 1);
    let task_two_summary = overview
        .task_governance_summaries
        .iter()
        .find(|summary| summary.task_id == task_two)
        .expect("task two governance summary should exist");
    assert_eq!(task_two_summary.governance_summary.repair_retry, 1);
    let task_three_summary = overview
        .task_governance_summaries
        .iter()
        .find(|summary| summary.task_id == task_three)
        .expect("task three governance summary should exist");
    assert_eq!(task_three_summary.governance_summary.rejected, 1);
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
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-exec");
    let task_id = TaskId::new("todo-exec");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
    assert_eq!(result.overview.runtime_snapshot.completed_tasks, 1);
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

    let snapshot = event_bus.snapshot();
    for event_type in [
        "worker.report.applied",
        "worker.skill_dispatch.applied",
        "mission.execution.overview",
    ] {
        let event = snapshot
            .recent_events
            .iter()
            .find(|event| event.event_type == event_type)
            .unwrap_or_else(|| panic!("{event_type} event should be published"));
        assert_eq!(
            event.session_id.as_ref().map(|id| id.as_str()),
            Some("session-exec")
        );
        assert_eq!(
            event.workspace_id.as_ref().map(|id| id.as_str()),
            Some("workspace-exec")
        );
        assert_eq!(event.payload["session_id"].as_str(), Some("session-exec"));
        assert_eq!(
            event.payload["workspace_id"].as_str(),
            Some("workspace-exec")
        );
    }
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

    let session_id = SessionId::new("session-exec-context");
    let workspace_id = WorkspaceId::new("workspace-exec-context");
    let knowledge_store = magi_knowledge_store::KnowledgeStore::new();
    knowledge_store.ingest_code_index_in_workspace(
        workspace_id.clone(),
        magi_knowledge_store::CodeIndexIngestion {
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
        },
    );
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
        &[(task_id.clone(), "Fix manifest parser", TaskStatus::Pending)],
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
        .execution_groups
        .iter()
        .find(|entry| entry.mission_id == mission_id.to_string())
        .expect("execution group entry should exist");
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
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-dispatch-hook-success");
    let task_id = TaskId::new("todo-dispatch-hook-success");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
        hook_log
            .lock()
            .expect("hook log lock should hold")
            .as_slice(),
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
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );
    let memory_store = magi_memory_store::MemoryStore::new();

    let mission_id = MissionId::new("mission-dispatch-writeback-success");
    let task_id = TaskId::new("todo-dispatch-writeback-success");
    let session_id = SessionId::new("session-dispatch-writeback-success");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
        .execute_dispatch_with_writebacks(DispatchWritebackRequest {
            target: direct_execution_target(&mission_id, &task_id),
            worker_id: WorkerId::new("worker-dispatch-writeback-success"),
            session_id: Some(session_id),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-writeback-success")),
            skill_plan: None,
            memory_store: memory_store.clone(),
            writebacks,
        })
        .expect("execution should run");

    assert_eq!(result.target.task_id, task_id);
    assert!(
        memory_store
            .verify_extraction_linkage("extract-dispatch-writeback-success")
            .expect("dispatch writeback should persist extraction linkage")
            .is_consistent
    );
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
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-local-exec");
    let task_id = TaskId::new("todo-local-exec");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

    let result = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-local-exec"),
            Some(SessionId::new("session-local-exec")),
            Some(WorkspaceId::new("workspace-local-exec")),
            None,
        )
        .expect("local process execution should run");

    assert_eq!(result.overview.runtime_snapshot.completed_tasks, 1);
    assert_eq!(result.overview.worker_summary.tool_call_count, 1);
    assert_eq!(result.overview.tool_summary.total_invocations, 1);
    assert_eq!(result.overview.skill_dispatch_summary.total_dispatches, 1);
    assert!(result.outcome.report.is_some());
}

#[test]
fn execution_intent_inherits_task_tool_policy() {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime = WorkerRuntime::new_compare(Arc::clone(&event_bus));
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );
    let mission_id = MissionId::new("mission-worker-policy");
    let task_id = TaskId::new("task-worker-policy");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );
    let mut task = task_store
        .get_task(&task_id)
        .expect("seeded task should exist");
    task.policy_snapshot = Some(test_task_policy(
        AccessProfile::FullAccess,
        "read_only",
        vec!["process_inspect".to_string()],
        vec!["file_remove".to_string()],
    ));
    task_store.insert_task(task);

    let intent = execution_runtime
        .build_execution_intent(
            &direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-policy"),
            Some(SessionId::new("session-worker-policy")),
            Some(WorkspaceId::new("workspace-worker-policy")),
            None,
        )
        .expect("execution intent should build");

    assert_eq!(intent.tool_policy.access_profile, AccessProfile::FullAccess);
    assert_eq!(
        intent.tool_policy.effective_access_profile(),
        AccessProfile::ReadOnly
    );
    assert_eq!(
        intent.tool_policy.allowed_tool_names,
        vec!["process_inspect".to_string()]
    );
    assert_eq!(
        intent.tool_policy.denied_tool_names,
        vec!["file_remove".to_string()]
    );
    assert_eq!(
        intent.tool_policy.allowed_paths,
        vec!["/tmp/worker-allowed".to_string()]
    );

    let skill_policy = intent
        .steps
        .iter()
        .find_map(|step| match step {
            magi_worker_runtime::WorkerExecutionIntentStep::SkillDispatch { plan, .. } => {
                Some(&plan.tool_policy)
            }
            _ => None,
        })
        .expect("intent should contain skill dispatch step");
    assert_eq!(skill_policy.access_profile, AccessProfile::FullAccess);
    assert_eq!(
        skill_policy.effective_access_profile(),
        AccessProfile::ReadOnly
    );
    assert_eq!(
        skill_policy.allowed_tool_names,
        vec!["process_inspect".to_string()]
    );
    assert_eq!(
        skill_policy.denied_tool_names,
        vec!["file_remove".to_string()]
    );
}

#[derive(Clone)]
struct UnhealthyLocalExecutor;

impl WorkerExecutor for UnhealthyLocalExecutor {
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
            executor_id: "local-process-worker-executor".to_string(),
            executor_version: "worker-local-process-executor-v2".to_string(),
            executor_kind: WorkerExecutorKind::LocalProcess,
            capability: LocalProcessExecutorCapability {
                executor_id: "local-process-worker-executor".to_string(),
                executor_version: "worker-local-process-executor-v2".to_string(),
                execution_mode: magi_worker_runtime::WorkerExecutionMode::LocalProcess,
                protocol_version: "worker-local-process-v1".to_string(),
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
                    parallelism_scope:
                        magi_worker_runtime::WorkerExecutionParallelismScope::Executor,
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
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-unhealthy-exec");
    let task_id = TaskId::new("todo-unhealthy-exec");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

    let error = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-unhealthy-exec"),
            Some(SessionId::new("session-unhealthy-exec")),
            Some(WorkspaceId::new("workspace-unhealthy-exec")),
            None,
        )
        .expect_err("unhealthy executor should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert_eq!(reason, "executor unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn execution_runtime_execute_dispatch_then_skips_hook_on_failure() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-dispatch-hook-failure");
    let task_id = TaskId::new("todo-dispatch-hook-failure");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

    let hook_calls = Arc::new(Mutex::new(0usize));
    let hook_calls_capture = Arc::clone(&hook_calls);
    let error = execution_runtime
        .execute_dispatch_then(
            direct_execution_target(&mission_id, &task_id),
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
            assert_eq!(reason, "executor unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(*hook_calls.lock().expect("hook calls lock should hold"), 0);
}

#[test]
fn execution_runtime_execute_dispatch_with_writebacks_skips_writeback_on_failure() {
    let event_bus = Arc::new(InMemoryEventBus::new(32));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let governance = Arc::new(GovernanceService::default());
    let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
    tool_registry.register_default_builtins();
    let skill_runtime = SkillDispatchRuntime::new(
        tool_registry.clone(),
        magi_bridge_client::BridgeDispatchRuntime::new(),
    );
    let worker_runtime =
        WorkerRuntime::new(Arc::clone(&event_bus)).with_executor(Arc::new(UnhealthyLocalExecutor));
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );
    let memory_store = magi_memory_store::MemoryStore::new();

    let mission_id = MissionId::new("mission-dispatch-writeback-failure");
    let task_id = TaskId::new("todo-dispatch-writeback-failure");
    let session_id = SessionId::new("session-dispatch-writeback-failure");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
        .execute_dispatch_with_writebacks(DispatchWritebackRequest {
            target: direct_execution_target(&mission_id, &task_id),
            worker_id: WorkerId::new("worker-dispatch-writeback-failure"),
            session_id: Some(SessionId::new("session-dispatch-writeback-failure")),
            workspace_id: Some(WorkspaceId::new("workspace-dispatch-writeback-failure")),
            skill_plan: None,
            memory_store: memory_store.clone(),
            writebacks,
        })
        .expect_err("unhealthy executor should be rejected before writeback");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert_eq!(reason, "executor unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(
        memory_store
            .extraction_linkage("extract-dispatch-writeback-failure")
            .is_none()
    );
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
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-step-capability");
    let task_id = TaskId::new("todo-step-capability");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

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
            assert_eq!(reason, "executor capability insufficient");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn execution_runtime_rejects_local_process_executor_affinity_mismatch_before_execute() {
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
            .with_env("MAGI_LOCAL_WORKER_SESSION_ID", "session-affine")
            .with_env("MAGI_LOCAL_WORKER_WORKSPACE_ID", "workspace-affine"),
    ));
    let (execution_runtime, task_store) = build_execution_runtime_with_task_store(
        &service,
        worker_runtime,
        tool_registry,
        skill_runtime,
    );

    let mission_id = MissionId::new("mission-affinity-capability");
    let task_id = TaskId::new("todo-affinity-capability");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "mission",
        &[(task_id.clone(), "task", TaskStatus::Pending)],
    );

    let error = execution_runtime
        .execute_dispatch(
            direct_execution_target(&mission_id, &task_id),
            WorkerId::new("worker-affinity-capability"),
            Some(SessionId::new("session-other")),
            Some(WorkspaceId::new("workspace-other")),
            None,
        )
        .expect_err("affinity mismatch should be rejected before execute");

    match error {
        OrchestratorCommandError::WorkerExecutorUnavailable { reason } => {
            assert_eq!(reason, "executor capability insufficient");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn multi_task_execution_with_mixed_outcomes_aggregates_in_overview() {
    let event_bus = Arc::new(InMemoryEventBus::new(64));
    let service = OrchestratorService::new(Arc::clone(&event_bus));
    let task_store = TaskStore::new();

    let mission_id = MissionId::new("mission-multi");
    let assignment_a = AssignmentId::new("assignment-multi-a");
    let assignment_b = AssignmentId::new("assignment-multi-b");
    let task_a1 = TaskId::new(assignment_a.as_str());
    let task_a2 = TaskId::new("todo-multi-a2");
    let task_b1 = TaskId::new(assignment_b.as_str());
    let root_task_id = seed_task_hierarchy(
        &task_store,
        &mission_id,
        "mission",
        TaskStatus::Failed,
        &[
            (task_a1.clone(), None, "todo-a1", TaskStatus::Completed),
            (
                task_a2.clone(),
                Some(task_a1.clone()),
                "todo-a2",
                TaskStatus::Completed,
            ),
            (task_b1.clone(), None, "todo-b1", TaskStatus::Failed),
        ],
    );

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

    let overview = build_task_projection_overview(
        &service,
        &task_store,
        TaskProjectionOverviewFixture {
            mission_id: &mission_id,
            root_task_id: &root_task_id,
            worker_summary: worker_summary(2),
            tool_summary: ToolExecutionSummary {
                total_invocations: 4,
                successful_invocations: 3,
                failed_invocations: 1,
                blocked_invocations: 0,
            },
            skill_dispatch_observations: &skill_observations,
            governance_observations: &governance_observations,
            context_summary: None,
        },
    );

    // Mission-level assertions
    assert_eq!(overview.runtime_snapshot.total_tasks, 4);
    assert_eq!(overview.runtime_snapshot.completed_tasks, 2);
    assert_eq!(overview.runtime_snapshot.failed_tasks, 2);
    assert_eq!(overview.runtime_snapshot.total_assignments, 2);
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
        assert!(
            assessment
                .signals
                .contains(&"config_or_dependency_change".to_string())
        );
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
        assert!(
            assessment
                .signals
                .iter()
                .any(|s| s.starts_with("failure_rate"))
        );
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
        assert!(
            assessment
                .signals
                .contains(&"unknown_file_scope".to_string())
        );
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

        let approved = svc
            .approve("s1", &plan.plan_id, None, Some("看起来不错"))
            .unwrap();
        assert_eq!(approved.status, PlanStatus::Approved);
        assert_eq!(
            approved.review.as_ref().unwrap().status,
            PlanReviewStatus::Approved
        );

        let mut svc2 = PlanLedgerService::new();
        let plan2 = svc2.create_draft(draft_input("s1", "t2"));
        let rejected = svc2
            .reject("s1", &plan2.plan_id, None, Some("方案不合理"))
            .unwrap();
        assert_eq!(rejected.status, PlanStatus::Rejected);
        assert_eq!(
            rejected.review.as_ref().unwrap().reason.as_deref(),
            Some("方案不合理")
        );
    }

    #[test]
    fn mark_executing_and_finalize() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));
        svc.approve("s1", &plan.plan_id, None, None).unwrap();
        let executing = svc.mark_executing("s1", &plan.plan_id).unwrap();
        assert_eq!(executing.status, PlanStatus::Executing);

        let finalized = svc
            .finalize("s1", &plan.plan_id, PlanStatus::Completed)
            .unwrap();
        assert_eq!(finalized.status, PlanStatus::Completed);
        assert_eq!(
            finalized.runtime.acceptance.summary,
            PlanAcceptanceSummary::Passed
        );
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

        let updated = svc
            .upsert_dispatch_item(
                "s1",
                &plan.plan_id,
                item_input("item-1", "编写接口", "worker-a"),
            )
            .unwrap();
        assert_eq!(updated.items.len(), 1);
        assert_eq!(updated.items[0].title, "编写接口");
        assert!(updated.status == PlanStatus::Approved);

        let updated2 = svc
            .upsert_dispatch_item(
                "s1",
                &plan.plan_id,
                item_input("item-1", "修改接口", "worker-b"),
            )
            .unwrap();
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

        let started = svc
            .start_attempt(
                "s1",
                &plan.plan_id,
                PlanAttemptStartInput {
                    scope: PlanAttemptScope::Orchestrator,
                    target_id: None,
                    assignment_id: None,
                    task_id: None,
                    reason: Some("开始执行".to_string()),
                },
            )
            .unwrap();
        assert_eq!(started.attempts.len(), 1);
        assert_eq!(started.attempts[0].status, PlanAttemptStatus::Inflight);

        let completed = svc
            .complete_latest_attempt(
                "s1",
                &plan.plan_id,
                PlanAttemptCompleteInput {
                    scope: PlanAttemptScope::Orchestrator,
                    target_id: None,
                    assignment_id: None,
                    task_id: None,
                    status: PlanAttemptStatus::Succeeded,
                    error: None,
                    evidence_ids: None,
                },
            )
            .unwrap();
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

        let updated = svc
            .update_runtime_review("s1", &plan.plan_id, ReviewState::Running, Some(1))
            .unwrap();
        assert_eq!(updated.runtime.review.state, ReviewState::Running);
        assert_eq!(updated.runtime.review.round, 1);
        assert!(updated.runtime.review.last_reviewed_at.is_some());
    }

    #[test]
    fn runtime_acceptance_update() {
        let mut svc = PlanLedgerService::new();
        let plan = svc.create_draft(draft_input("s1", "t1"));

        let updated = svc
            .update_runtime_acceptance(
                "s1",
                &plan.plan_id,
                Some(vec![
                    AcceptanceCriterion {
                        description: "编译通过".to_string(),
                        met: true,
                    },
                    AcceptanceCriterion {
                        description: "测试通过".to_string(),
                        met: false,
                    },
                ]),
                None,
            )
            .unwrap();
        assert_eq!(
            updated.runtime.acceptance.summary,
            PlanAcceptanceSummary::Partial
        );
        assert_eq!(updated.runtime.acceptance.criteria.len(), 2);
    }
}

mod auto_learning_tests {
    use crate::auto_learning::*;

    #[test]
    fn preference_miner_correction() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(&["不要添加注释", "必须使用中文"], &["好的"]);
        assert!(result.preferences.len() >= 2);
        let correction = result
            .preferences
            .iter()
            .find(|p| p.pattern.contains("注释"))
            .unwrap();
        assert_eq!(
            correction.category,
            preference_miner::PreferenceCategory::Constraint
        );
        assert!(correction.confidence >= 0.7);
    }

    #[test]
    fn preference_miner_mandatory() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(&["务必先跑测试再提交"], &[]);
        assert!(!result.preferences.is_empty());
        let mandatory = &result.preferences[0];
        assert!(mandatory.pattern.contains("先跑测试再提交"));
    }

    #[test]
    fn preference_miner_format() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(&["请用中文回复"], &[]);
        let format_pref = result
            .preferences
            .iter()
            .find(|p| p.pattern.contains("中文"));
        assert!(format_pref.is_some());
    }

    #[test]
    fn preference_miner_style() {
        let miner = PreferenceMiner::new();
        let result = miner.mine_from_conversation(&["简单点说"], &[]);
        let style_pref = result
            .preferences
            .iter()
            .find(|p| p.pattern.contains("简洁"));
        assert!(style_pref.is_some());
    }

    #[test]
    fn preference_miner_repetition() {
        let miner = PreferenceMiner::new();
        let result = miner
            .mine_from_conversation(&["继续", "继续", "继续", "继续"], &["好的", "好的", "好的"]);
        let workflow = result
            .preferences
            .iter()
            .find(|p| p.category == preference_miner::PreferenceCategory::Workflow);
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
            AutoLearningCaptureContent {
                user_messages: &["请实现用户认证"],
                assistant_messages: &["好的，我来实现"],
                summary: "完成用户认证功能",
                decisions: vec!["使用 JWT 方案".to_string()],
                learnings: vec!["JWT 适合无状态认证".to_string()],
                warnings: vec![],
            },
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
            let summary = format!("摘要 {i}");
            mgr.capture(
                &input,
                AutoLearningCaptureContent {
                    user_messages: &["继续"],
                    assistant_messages: &["好的"],
                    summary: &summary,
                    decisions: vec![format!("决策 {i}")],
                    learnings: vec![format!("学习 {i}")],
                    warnings: vec![format!("警告 {i}")],
                },
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

#[test]
fn task_store_remove_mission_removes_tasks_leases_and_checkpoints_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let task_store = TaskStore::new();
    let mission_id = MissionId::new("mission-delete");
    let root_task_id = TaskId::new("task-root-delete");
    let child_task_id = TaskId::new("task-child-delete");
    seed_action_tasks(
        &task_store,
        &mission_id,
        "delete mission",
        &[(child_task_id.clone(), "child", TaskStatus::Running)],
    );
    task_store
        .grant_lease(
            &child_task_id,
            &root_task_id,
            &WorkerId::new("worker-delete"),
            "executor",
            60_000,
        )
        .expect("lease should be granted");
    let checkpoint_count = Arc::new(AtomicUsize::new(0));
    let observed_checkpoint_count = checkpoint_count.clone();
    task_store.set_checkpoint_callback(Box::new(move |_| {
        observed_checkpoint_count.fetch_add(1, Ordering::SeqCst);
    }));

    let removed = task_store.remove_tasks_by_mission(&mission_id);

    assert_eq!(removed.len(), 2);
    assert!(task_store.get_tasks_by_mission(&mission_id).is_empty());
    assert!(task_store.get_task(&root_task_id).is_none());
    assert!(task_store.get_task(&child_task_id).is_none());
    assert_eq!(
        task_store.checkpoint()["leases"].as_array().unwrap().len(),
        0
    );
    assert_eq!(checkpoint_count.load(Ordering::SeqCst), 1);
}

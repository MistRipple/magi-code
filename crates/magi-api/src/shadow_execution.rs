use magi_bridge_client::ModelInvocationRequest;
use magi_bridge_client::SHADOW_MODEL_PROVIDER;
#[cfg(test)]
use magi_core::SessionId;
use magi_core::{
    ExecutionOwnership, ExecutorBinding, MissionId, Task, TaskExecutionTarget, TaskId, TaskKind,
    TaskStatus, UtcMillis, WorkerId,
};
use magi_event_bus::{EventContext, task_events};
use magi_orchestrator::ExecutionWritebackPlans;
use magi_session_store::{
    ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
    ActiveExecutionTurn, ActiveExecutionTurnItem, ActiveExecutionTurnLane,
};

#[cfg(test)]
use crate::dto::SessionActionRequestDto;
use crate::{
    errors::ApiError,
    state::ApiState,
    task_execution::{DispatchSubmissionRequest, ShadowTaskExecutionPlan},
};

pub(crate) struct ShadowTaskGraphSubmission {
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub active_execution_chain: Option<ActiveExecutionChain>,
}

pub(crate) fn run_shadow_dispatch_submission(
    state: &ApiState,
    request: &DispatchSubmissionRequest,
) -> Result<ShadowTaskGraphSubmission, ApiError> {
    let _ = state.shadow_execution_pipeline().ok_or_else(|| {
        ApiError::internal_assembly(
            "执行 shadow dispatch 失败",
            "shadow execution pipeline 未配置",
        )
    })?;
    let task_store = state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("执行 shadow dispatch 失败", "task_store 未配置")
    })?;

    let accepted_at = request.accepted_at;
    let session_id = &request.session_id;
    let entry_id = request.entry_id.as_str();
    let trimmed_text = request.trimmed_text.as_deref();

    let mission_id = MissionId::new(format!("mission-session-action-{}", accepted_at.0));
    let worker_id = WorkerId::new(format!("worker-session-action-{}", accepted_at.0));

    let obj_task_id = TaskId::new(format!("task-obj-{}", accepted_at.0));
    let act_task_id = TaskId::new(format!("task-act-{}", accepted_at.0));

    let now = UtcMillis::now();
    let objective = make_shadow_task(
        obj_task_id.clone(),
        mission_id.clone(),
        obj_task_id.clone(),
        None,
        TaskKind::Objective,
        request.mission_title.clone(),
        trimmed_text.unwrap_or("").to_string(),
        TaskStatus::Running,
        now,
        Some("architect"),
    );
    task_store.insert_task(objective);

    let action_goal = trimmed_text.unwrap_or("").to_string();
    let action = make_shadow_task(
        act_task_id.clone(),
        mission_id.clone(),
        obj_task_id.clone(),
        Some(obj_task_id.clone()),
        TaskKind::Action,
        request.task_title.clone(),
        action_goal,
        TaskStatus::Ready,
        now,
        Some(
            request
                .target_role
                .as_deref()
                .unwrap_or_else(|| infer_dispatch_task_role(request.skill_name.as_deref())),
        ),
    );
    task_store.insert_task(action);

    let mut total_task_count = 2usize;
    let mut sub_task_ids: Vec<TaskId> = Vec::new();
    if request.deep_task {
        if let Some(sub_actions) = decompose_mission(state, trimmed_text, &now) {
            for (i, sub_title) in sub_actions.iter().enumerate() {
                let sub_task_id = TaskId::new(format!("task-sub-{}-{}", accepted_at.0, i));
                let sub_action = make_shadow_task(
                    sub_task_id.clone(),
                    mission_id.clone(),
                    obj_task_id.clone(),
                    Some(obj_task_id.clone()),
                    TaskKind::Action,
                    sub_title.clone(),
                    sub_title.clone(),
                    TaskStatus::Ready,
                    now,
                    Some("integration-dev"),
                );
                task_store.insert_task(sub_action);
                sub_task_ids.push(sub_task_id);
                total_task_count += 1;
            }
        }
    }

    let event = task_events::task_graph_created_event(
        mission_id.as_str(),
        obj_task_id.as_str(),
        total_task_count,
    )
    .with_context(EventContext {
        mission_id: Some(mission_id.clone()),
        task_id: Some(obj_task_id.clone()),
        ..EventContext::default()
    });
    let _ = state.event_bus.publish(event);

    let workspace_id = request.workspace_id.clone();
    let execution_chain_ref = Some(format!("session-action-chain-{}", accepted_at.0));
    let ownership = ExecutionOwnership {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        mission_id: Some(mission_id.clone()),
        task_id: Some(act_task_id.clone()),
        worker_id: Some(worker_id.clone()),
        execution_chain_ref: execution_chain_ref.clone(),
        ..ExecutionOwnership::default()
    };
    state.shadow_task_execution_registry().insert(
        act_task_id.clone(),
        ShadowTaskExecutionPlan::Dispatch {
            target: TaskExecutionTarget {
                mission_id: mission_id.clone(),
                root_task_id: obj_task_id.clone(),
                task_id: act_task_id.clone(),
                requested_worker_id: Some(worker_id.clone()),
                recovery_id: None,
                execution_chain_ref: execution_chain_ref.clone(),
            },
            worker_id: worker_id.clone(),
            lane_id: None,
            lane_seq: None,
            is_primary: true,
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            ownership: ownership.clone(),
            writebacks: ExecutionWritebackPlans::from_session_action_input(
                magi_orchestrator::DispatchMemoryExtractionInput {
                    accepted_at,
                    session_id: session_id,
                    timeline_entry_id: entry_id,
                    text: trimmed_text,
                    skill_name: request.skill_name.as_deref(),
                    deep_task: request.deep_task,
                },
            ),
            use_tools: request.deep_task,
            skill_name: request.skill_name.clone(),
        },
    );

    for sub_task_id in &sub_task_ids {
        let sub_ownership = ExecutionOwnership {
            session_id: Some(request.session_id.clone()),
            workspace_id: workspace_id.clone(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(TaskId::new(sub_task_id.as_str())),
            worker_id: Some(worker_id.clone()),
            execution_chain_ref: execution_chain_ref.clone(),
            ..ExecutionOwnership::default()
        };
        state.shadow_task_execution_registry().insert(
            sub_task_id.clone(),
            ShadowTaskExecutionPlan::Dispatch {
                target: TaskExecutionTarget {
                    mission_id: mission_id.clone(),
                    root_task_id: obj_task_id.clone(),
                    task_id: TaskId::new(sub_task_id.as_str()),
                    requested_worker_id: Some(worker_id.clone()),
                    recovery_id: None,
                    execution_chain_ref: execution_chain_ref.clone(),
                },
                worker_id: worker_id.clone(),
                lane_id: Some(format!("lane-{}", sub_task_id)),
                lane_seq: Some(
                    sub_task_ids
                        .iter()
                        .position(|candidate| candidate == sub_task_id)
                        .unwrap_or(0)
                        + 1,
                ),
                is_primary: false,
                session_id: request.session_id.clone(),
                workspace_id: workspace_id.clone(),
                ownership: sub_ownership,
                writebacks: ExecutionWritebackPlans::default(),
                use_tools: true,
                skill_name: request.skill_name.clone(),
            },
        );
    }

    let mut branches = vec![ActiveExecutionBranch {
        task_id: act_task_id.clone(),
        worker_id: worker_id.clone(),
        stage: "execute".to_string(),
        lease_id: None,
        execution_intent_ref: None,
        binding_lifecycle: None,
        checkpoint_stage: Some("execute".to_string()),
        next_step_index: Some(0),
        checkpoint_at: Some(now),
        resume_mode: Some("stage-restart".to_string()),
        resume_token: None,
        use_tools: request.deep_task,
        skill_name: request.skill_name.clone(),
        is_primary: true,
    }];
    let role_for_task = |task_id: &TaskId| {
        state
            .task_store()
            .and_then(|store| store.get_task(task_id))
            .and_then(|task| task.executor_binding.map(|binding| binding.target_role))
            .map(|role| role.trim().to_string())
            .filter(|role| !role.is_empty())
    };
    for sub_task_id in &sub_task_ids {
        branches.push(ActiveExecutionBranch {
            task_id: sub_task_id.clone(),
            worker_id: worker_id.clone(),
            stage: "execute".to_string(),
            lease_id: None,
            execution_intent_ref: None,
            binding_lifecycle: None,
            checkpoint_stage: Some("execute".to_string()),
            next_step_index: Some(0),
            checkpoint_at: Some(now),
            resume_mode: Some("stage-restart".to_string()),
            resume_token: None,
            use_tools: true,
            skill_name: request.skill_name.clone(),
            is_primary: false,
        });
    }
    let mut current_turn = ActiveExecutionTurn {
        turn_id: format!("turn-session-action-{}", accepted_at.0),
        turn_seq: accepted_at.0,
        accepted_at,
        status: "accepted".to_string(),
        user_message: trimmed_text.map(str::to_string),
        items: vec![ActiveExecutionTurnItem {
            item_id: format!("turn-item-phase-{}", accepted_at.0),
            item_seq: 1,
            lane_id: None,
            lane_seq: None,
            kind: "assistant_phase".to_string(),
            status: "pending".to_string(),
            source: "orchestrator".to_string(),
            title: Some("任务理解".to_string()),
            content: Some("已接收请求，正在整理执行步骤。".to_string()),
            task_id: Some(act_task_id.clone()),
            worker_id: Some(worker_id.clone()),
            role_id: role_for_task(&act_task_id),
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_result: None,
            tool_error: None,
            thread_visible: true,
            worker_visible: false,
        }],
        worker_lanes: sub_task_ids
            .iter()
            .enumerate()
            .map(|(index, sub_task_id)| ActiveExecutionTurnLane {
                lane_id: format!("lane-{}", sub_task_id),
                lane_seq: index + 1,
                task_id: sub_task_id.clone(),
                worker_id: worker_id.clone(),
                role_id: role_for_task(sub_task_id),
                title: state
                    .task_store()
                    .and_then(|store| store.get_task(sub_task_id))
                    .map(|task| task.title)
                    .unwrap_or_else(|| sub_task_id.to_string()),
                is_primary: false,
            })
            .collect(),
    };
    for (index, sub_task_id) in sub_task_ids.iter().enumerate() {
        let lane_id = format!("lane-{}", sub_task_id);
        let lane_title = current_turn
            .worker_lanes
            .iter()
            .find(|lane| lane.task_id == *sub_task_id)
            .map(|lane| lane.title.clone())
            .unwrap_or_else(|| sub_task_id.to_string());
        current_turn.items.push(ActiveExecutionTurnItem {
            item_id: format!("turn-item-worker-spawned-{}-{}", accepted_at.0, index),
            item_seq: index + 2,
            lane_id: Some(lane_id),
            lane_seq: Some(index + 1),
            kind: "worker_spawned".to_string(),
            status: "pending".to_string(),
            source: worker_id.to_string(),
            title: Some(lane_title.clone()),
            content: Some(format!("已为 {} 创建执行分支。", lane_title)),
            task_id: Some(sub_task_id.clone()),
            worker_id: Some(worker_id.clone()),
            role_id: role_for_task(sub_task_id),
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_result: None,
            tool_error: None,
            thread_visible: false,
            worker_visible: true,
        });
    }
    current_turn.normalize();
    Ok(ShadowTaskGraphSubmission {
        root_task_id: obj_task_id.clone(),
        action_task_id: act_task_id.clone(),
        active_execution_chain: Some(ActiveExecutionChain {
            session_id: request.session_id.clone(),
            mission_id,
            root_task_id: obj_task_id,
            execution_chain_ref: execution_chain_ref
                .expect("dispatch execution chain ref should exist"),
            workspace_id,
            active_branch_task_ids: branches
                .iter()
                .map(|branch| branch.task_id.clone())
                .collect(),
            active_worker_bindings: branches
                .iter()
                .map(|branch| branch.worker_id.clone())
                .collect(),
            branches,
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at,
                entry_id: entry_id.to_string(),
                trimmed_text: trimmed_text.map(str::to_string),
                deep_task: request.deep_task,
                skill_name: request.skill_name.clone(),
            },
            current_turn: Some(current_turn),
        }),
    })
}

#[cfg(test)]
pub(crate) fn run_shadow_session_action(
    state: &ApiState,
    request: &SessionActionRequestDto,
    accepted_at: UtcMillis,
    session_id: &SessionId,
    entry_id: &str,
    trimmed_text: Option<&str>,
) -> Result<ShadowTaskGraphSubmission, ApiError> {
    let mission_title = request.mission_title(trimmed_text);
    run_shadow_dispatch_submission(
        state,
        &DispatchSubmissionRequest {
            accepted_at,
            session_id: session_id.clone(),
            workspace_id: request
                .requested_workspace_id()
                .map(magi_core::WorkspaceId::new),
            entry_id: entry_id.to_string(),
            created_session: false,
            mission_title: mission_title.clone(),
            task_title: format!("执行: {mission_title}"),
            trimmed_text: trimmed_text.map(str::to_string),
            deep_task: request.deep_task,
            skill_name: request.skill_name.clone(),
            target_role: None,
        },
    )
}

fn decompose_mission(
    state: &ApiState,
    prompt: Option<&str>,
    _now: &UtcMillis,
) -> Option<Vec<String>> {
    let prompt_text = prompt.filter(|s| !s.is_empty())?;
    let client = state.model_bridge_client()?;
    let request = ModelInvocationRequest {
        provider: SHADOW_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "请将以下任务分解为 2-5 个具体的子任务。每行一个子任务标题，不要编号，不要额外说明。\n\n任务：{}",
            prompt_text
        ),
        messages: None,
        tools: None,
    };
    let response = client.invoke(request).ok()?;
    if !response.ok {
        return None;
    }
    let sub_tasks = parse_decomposition_response(&response.payload, prompt_text);
    if sub_tasks.is_empty() {
        None
    } else {
        Some(sub_tasks)
    }
}

fn parse_decomposition_response(response: &str, original_prompt: &str) -> Vec<String> {
    let mut tasks = Vec::new();
    for line in response.lines() {
        let Some(task_title) = normalize_decomposition_line(line, original_prompt) else {
            continue;
        };
        if tasks.iter().any(|existing| existing == &task_title) {
            continue;
        }
        tasks.push(task_title);
        if tasks.len() >= 5 {
            break;
        }
    }
    tasks
}

fn normalize_decomposition_line(line: &str, original_prompt: &str) -> Option<String> {
    let mut value = line.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix("shadow-model::") {
        value = rest.trim();
    }
    value = value
        .trim_start_matches(|ch: char| ch == '-' || ch == '*' || ch == '•' || ch.is_whitespace())
        .trim();
    let numbered = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .and_then(|(index, ch)| {
            if index > 0 && (ch == '.' || ch == ')' || ch == '、') {
                Some(value[index + ch.len_utf8()..].trim())
            } else {
                None
            }
        });
    if let Some(rest) = numbered {
        value = rest;
    }

    let lower = value.to_ascii_lowercase();
    let original_prompt = original_prompt.trim();
    if value.starts_with("请将以下任务分解")
        || value.starts_with("每行一个子任务")
        || value.starts_with("不要编号")
        || value.starts_with("不要额外说明")
        || value.starts_with("只返回")
        || lower.starts_with("task:")
        || value.starts_with("任务：")
        || value.starts_with("目标:")
        || value.starts_with("目标：")
        || (!original_prompt.is_empty() && value == original_prompt)
    {
        return None;
    }

    let value = value
        .trim_matches(|ch| ch == '"' || ch == '\'' || ch == '`')
        .trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Build a Task with sensible defaults for shadow execution entries.
fn make_shadow_task(
    task_id: TaskId,
    mission_id: MissionId,
    root_task_id: TaskId,
    parent_task_id: Option<TaskId>,
    kind: TaskKind,
    title: String,
    goal: String,
    status: TaskStatus,
    now: UtcMillis,
    target_role: Option<&str>,
) -> Task {
    Task {
        task_id,
        mission_id,
        root_task_id,
        parent_task_id,
        kind,
        title,
        goal,
        status,
        dependency_ids: Vec::new(),
        required_children: Vec::new(),
        policy_snapshot: None,
        executor_binding: target_role.map(|role| ExecutorBinding {
            target_role: role.to_string(),
            capability_requirements: Vec::new(),
            parallelism_group: None,
            exclusive_scope: None,
            worker_selector: None,
        }),
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
    }
}

fn infer_dispatch_task_role(skill_name: Option<&str>) -> &'static str {
    let Some(skill_name) = skill_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return "integration-dev";
    };

    let skill = skill_name.to_ascii_lowercase();
    if skill.contains("front") || skill.contains("ui") {
        "frontend-dev"
    } else if skill.contains("back") || skill.contains("api") || skill.contains("server") {
        "backend-dev"
    } else if skill.contains("review") || skill.contains("audit") {
        "reviewer"
    } else if skill.contains("test") || skill.contains("qa") || skill.contains("verify") {
        "test-engineer"
    } else if skill.contains("doc") || skill.contains("write") {
        "doc-writer"
    } else if skill.contains("debug") || skill.contains("fix") || skill.contains("bug") {
        "debugger"
    } else if skill.contains("data") || skill.contains("etl") || skill.contains("metric") {
        "data-engineer"
    } else if skill.contains("devops") || skill.contains("infra") || skill.contains("deploy") {
        "devops-engineer"
    } else if skill.contains("security") || skill.contains("auth") || skill.contains("sec") {
        "security-analyst"
    } else if skill.contains("arch") || skill.contains("design") {
        "architect"
    } else {
        "integration-dev"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_core::SessionId;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_memory_store::MemoryStore;
    use magi_orchestrator::{OrchestratorService, task_store::TaskStore};
    use magi_session_store::SessionStore;
    use magi_skill_runtime::SkillDispatchRuntime;
    use magi_tool_runtime::ToolRegistry;
    use magi_worker_runtime::WorkerRuntime;
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    use crate::dto::SessionActionRequestDto;

    /// Build a minimal ApiState with a shadow execution pipeline and task store.
    fn build_test_state() -> (ApiState, Arc<TaskStore>) {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        let task_store = Arc::new(TaskStore::new());

        let state = ApiState::new(
            "test-shadow",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store)
        .with_task_store(Arc::clone(&task_store));

        (state, task_store)
    }

    // -----------------------------------------------------------------------
    // Test 1: Session action creates Task Graph entries when TaskStore is configured
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_creates_task_graph_entries() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-graph-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionActionRequestDto {
            session_id: None,
            workspace_id: None,
            text: Some("Hello world".to_string()),
            skill_name: None,
            deep_task: false,
            images: Vec::new(),
        };

        let result = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-1",
            Some("Hello world"),
        )
        .expect("shadow session action should create task graph");

        // Verify Objective task was created
        let obj_task_id = result.root_task_id;
        let obj = task_store.get_task(&obj_task_id);
        assert!(obj.is_some(), "Objective task should exist in TaskStore");
        let obj = obj.unwrap();
        assert_eq!(obj.kind, TaskKind::Objective);
        assert_eq!(obj.title, "Hello world");
        assert_eq!(obj.status, TaskStatus::Running);

        // Verify Action task was created
        let act_task_id = result.action_task_id;
        let act = task_store.get_task(&act_task_id);
        assert!(act.is_some(), "Action task should exist in TaskStore");
        let act = act.unwrap();
        assert_eq!(act.kind, TaskKind::Action);
        assert_eq!(act.title, "执行: Hello world");
        assert_eq!(act.goal, "Hello world");
        assert_eq!(act.parent_task_id, Some(obj_task_id.clone()));
        assert_eq!(act.root_task_id, obj_task_id.clone());
        assert_eq!(act.status, TaskStatus::Ready);

        let children = task_store.get_children(&obj_task_id);
        assert_eq!(children.len(), 1);
        let child_ids: Vec<&str> = children.iter().map(|t| t.task_id.as_str()).collect();
        assert!(child_ids.contains(&act_task_id.as_str()));
    }

    // -----------------------------------------------------------------------
    // Test 2: Task status reflects dispatch outcome (failure path)
    // -----------------------------------------------------------------------

    #[test]
    fn session_action_registers_shadow_execution_plan() {
        let (state, task_store) = build_test_state();

        let session_id = SessionId::new("session-task-status-test");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionActionRequestDto {
            session_id: None,
            workspace_id: None,
            text: Some("Run a failing action".to_string()),
            skill_name: None,
            deep_task: false,
            images: Vec::new(),
        };

        let submission = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-2",
            Some("Run a failing action"),
        )
        .expect("shadow session action should register a task execution plan");

        let obj = task_store
            .get_task(&submission.root_task_id)
            .expect("objective task should exist");
        let act = task_store
            .get_task(&submission.action_task_id)
            .expect("action task should exist");

        assert_eq!(obj.status, TaskStatus::Running);
        assert_eq!(act.status, TaskStatus::Ready);
        assert!(
            state
                .shadow_task_execution_registry()
                .remove(&submission.action_task_id)
                .is_some(),
            "action task should have a registered execution plan",
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: No TaskStore configured — behavior unchanged (backward compat)
    // -----------------------------------------------------------------------

    #[test]
    fn no_task_store_is_rejected() {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let orchestrator = OrchestratorService::new(Arc::clone(&event_bus));
        let session_store = Arc::new(SessionStore::new());
        let workspace_store = Arc::new(WorkspaceStore::new());
        let memory_store = MemoryStore::new();

        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let execution_runtime = orchestrator.execution_runtime(
            WorkerRuntime::new_compare(Arc::clone(&event_bus)),
            tool_registry.clone(),
            SkillDispatchRuntime::new(
                tool_registry,
                magi_bridge_client::BridgeDispatchRuntime::new(),
            ),
        );

        // No with_task_store — TaskStore is None
        let state = ApiState::new(
            "test-no-task-store",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::clone(&workspace_store),
            governance,
        )
        .with_shadow_execution_pipeline(orchestrator, execution_runtime, memory_store);

        assert!(state.task_store().is_none());

        let session_id = SessionId::new("session-no-store");
        state
            .session_store
            .create_session(session_id.clone(), "test session")
            .expect("session should be creatable");

        let accepted_at = UtcMillis::now();
        let request = SessionActionRequestDto {
            session_id: None,
            workspace_id: None,
            text: Some("test".to_string()),
            skill_name: None,
            deep_task: false,
            images: Vec::new(),
        };

        let result = run_shadow_session_action(
            &state,
            &request,
            accepted_at,
            &session_id,
            "entry-3",
            Some("test"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn decomposition_parser_rejects_prompt_echo_lines() {
        let response = "shadow-model::请将以下任务分解为 2-5 个具体的子任务。每行一个子任务标题，不要编号，不要额外说明。\n\n任务：请分析并拆分这个复杂任务";

        let tasks = parse_decomposition_response(response, "请分析并拆分这个复杂任务");

        assert!(tasks.is_empty(), "提示词回显不能被当成子任务标题");
    }

    #[test]
    fn decomposition_parser_accepts_clean_numbered_lines() {
        let response = "1. 分析目标与约束\n2) 制定执行步骤\n- 汇总结果";

        let tasks = parse_decomposition_response(response, "请分析并拆分这个复杂任务");

        assert_eq!(
            tasks,
            vec![
                "分析目标与约束".to_string(),
                "制定执行步骤".to_string(),
                "汇总结果".to_string(),
            ]
        );
    }
}

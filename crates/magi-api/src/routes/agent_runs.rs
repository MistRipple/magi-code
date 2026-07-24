use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use magi_core::{
    AccessProfile, AgentRunProjection, MissionId, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, Task, TaskId,
    TaskKind, TaskStatus, TaskTier, public_task_output_refs,
};
use magi_session_store::{ActiveExecutionChain, ExecutionThread, ORCHESTRATOR_ROLE_ID};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::session_scope::{
    SessionWorkspaceScope, parse_session_id, require_session_workspace_scope,
};
use crate::{
    errors::ApiError,
    routes::settings::{load_registry_engines, resolve_registry_agents},
    session_continue::active_execution_branch_is_continue_recoverable,
    state::ApiState,
};

const AGENT_RESULT_MAX_CHARS: usize = 8000;

pub fn routes() -> Router<ApiState> {
    Router::new().route(
        "/agent-runs/projection/{root_task_id}",
        get(get_agent_run_projection),
    )
}

fn require_task_store(
    state: &ApiState,
) -> Result<&magi_orchestrator::task_store::TaskStore, ApiError> {
    state.task_store().ok_or_else(|| {
        ApiError::internal_assembly("任务存储未配置", "task_store is not configured")
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionScopedTaskQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
}

#[derive(Clone, Debug)]
struct SessionTaskScope {
    workspace: SessionWorkspaceScope,
    mission_id: Option<MissionId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentProjectionResultDto {
    final_text: String,
    output_ref_count: usize,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentProjectionDto {
    agent_run_id: String,
    parent_task_id: String,
    root_task_id: String,
    display_name: String,
    goal: String,
    role: String,
    engine_id: Option<String>,
    model: Option<String>,
    model_source: String,
    status: String,
    status_label: String,
    lifecycle: String,
    access_profile: String,
    parallelism_group: Option<String>,
    worker_id: Option<String>,
    thread_id: Option<String>,
    execution_chain_ref: Option<String>,
    /// 子代理任务被调度接受的权威起点；不从前端消息时间戳推断。
    started_at: magi_core::UtcMillis,
    /// 子代理进入终态的权威时刻；运行中保持为空。
    completed_at: Option<magi_core::UtcMillis>,
    /// 从任务起点到终态的固定耗时；运行中保持为空。
    response_duration_ms: Option<u64>,
    updated_at: magi_core::UtcMillis,
    result: Option<AgentProjectionResultDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentRunProjectionResponseDto {
    #[serde(flatten)]
    projection: AgentRunProjection,
    session_id: String,
    workspace_id: String,
    workspace_path: String,
    agents: Vec<AgentProjectionDto>,
}

#[derive(Clone, Debug, Default)]
struct AgentModelBinding {
    engine_id: Option<String>,
    model: Option<String>,
    model_source: String,
}

fn require_session_task_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
) -> Result<SessionTaskScope, ApiError> {
    let workspace = require_session_workspace_scope(
        state,
        session_id_value,
        workspace_id_value,
        workspace_path_value,
        "读取代理运行投影",
    )?;
    let ownership = state
        .session_store
        .execution_ownership(&workspace.session_id);
    Ok(SessionTaskScope {
        workspace,
        mission_id: ownership.and_then(|ownership| ownership.mission_id),
    })
}

fn require_session_task(
    state: &ApiState,
    session_id_value: Option<&str>,
    workspace_id_value: Option<&str>,
    workspace_path_value: Option<&str>,
    task_id: &TaskId,
) -> Result<SessionTaskScope, ApiError> {
    let scope = require_session_task_scope(
        state,
        session_id_value,
        workspace_id_value,
        workspace_path_value,
    )?;
    let mission_id = scope
        .mission_id
        .clone()
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有活跃任务".to_string()))?;
    let store = require_task_store(state)?;
    let task = store
        .get_task(task_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", task_id.as_str()))?;
    if task.mission_id != mission_id {
        return Err(ApiError::InvalidInput(format!(
            "任务 {} 不属于当前会话 {}",
            task_id, scope.workspace.session_id
        )));
    }
    Ok(scope)
}

fn public_task_for_api(mut task: Task) -> Task {
    task.output_refs = public_task_output_refs(task.status, &task.output_refs);
    if let Some(policy) = task.policy_snapshot.as_mut() {
        policy.task_tier = TaskTier::ExecutionChain;
    }
    task
}

fn public_agent_run_projection_for_api(mut projection: AgentRunProjection) -> AgentRunProjection {
    projection.root_task = public_task_for_api(projection.root_task);
    projection.tasks = projection
        .tasks
        .into_iter()
        .map(public_task_for_api)
        .collect();
    projection.execution_mode = "execution_chain".to_string();
    projection
}

fn agent_read_model_for_projection(
    projection: &AgentRunProjection,
    chain: Option<&ActiveExecutionChain>,
    session_threads: &[ExecutionThread],
    model_bindings: &HashMap<String, AgentModelBinding>,
) -> Vec<AgentProjectionDto> {
    let branches_by_task = chain
        .map(|chain| {
            chain
                .branches
                .iter()
                .map(|branch| (branch.task_id.clone(), branch))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let execution_chain_ref = chain.map(|chain| chain.execution_chain_ref.clone());
    let tasks_by_id = projection
        .tasks
        .iter()
        .map(|task| (task.task_id.clone(), task))
        .collect::<HashMap<_, _>>();
    let mut consumed_task_ids = HashSet::new();
    let mut agents = Vec::new();

    for thread in session_threads.iter().filter(|thread| {
        thread.role_id != ORCHESTRATOR_ROLE_ID
            && thread
                .handled_task_ids
                .iter()
                .any(|task_id| tasks_by_id.contains_key(task_id))
    }) {
        for task_id in &thread.handled_task_ids {
            let Some(task) = tasks_by_id.get(task_id).copied() else {
                continue;
            };
            if !is_agent_projection_task(task) || !consumed_task_ids.insert(task.task_id.clone()) {
                continue;
            }
            agents.push(agent_projection_from_task(
                task,
                branches_by_task.get(&task.task_id).copied(),
                Some(thread),
                execution_chain_ref.as_deref(),
                model_bindings,
            ));
        }
    }

    for task in projection
        .tasks
        .iter()
        .filter(|task| is_agent_projection_task(task) && !consumed_task_ids.contains(&task.task_id))
    {
        agents.push(agent_projection_from_task(
            task,
            branches_by_task.get(&task.task_id).copied(),
            None,
            execution_chain_ref.as_deref(),
            model_bindings,
        ));
    }

    agents.sort_by(|left, right| {
        left.started_at
            .0
            .cmp(&right.started_at.0)
            .then_with(|| left.agent_run_id.cmp(&right.agent_run_id))
    });
    agents
}

fn is_agent_projection_task(task: &Task) -> bool {
    task.parent_task_id.is_some()
        && (task.executor_binding_target_role().is_some() || task.kind == TaskKind::LocalAgent)
}

fn agent_projection_from_task(
    task: &Task,
    branch: Option<&magi_session_store::ActiveExecutionBranch>,
    thread: Option<&ExecutionThread>,
    execution_chain_ref: Option<&str>,
    model_bindings: &HashMap<String, AgentModelBinding>,
) -> AgentProjectionDto {
    let role = thread
        .map(|thread| thread.role_id.as_str())
        .or_else(|| task.executor_binding_target_role())
        .unwrap_or("agent")
        .to_string();
    let model_binding = model_bindings
        .get(&role)
        .cloned()
        .unwrap_or_else(|| AgentModelBinding {
            model_source: "unconfigured".to_string(),
            ..AgentModelBinding::default()
        });
    let (status, lifecycle) = agent_runtime_status(task);
    let completed_at = matches!(task.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed)
        .then_some(task.updated_at);
    let response_duration_ms = completed_at
        .map(|completed_at| completed_at.0.saturating_sub(task.created_at.0));
    AgentProjectionDto {
        agent_run_id: task.task_id.to_string(),
        parent_task_id: task
            .parent_task_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        root_task_id: task.root_task_id.to_string(),
        display_name: task.title.clone(),
        goal: task.goal.clone(),
        role,
        engine_id: model_binding.engine_id,
        model: model_binding.model,
        model_source: model_binding.model_source,
        status: status.to_string(),
        status_label: task_status_label(task.status).to_string(),
        lifecycle: lifecycle.to_string(),
        access_profile: task
            .policy_snapshot
            .as_ref()
            .map(|policy| policy.access_profile.as_str())
            .unwrap_or(AccessProfile::Restricted.as_str())
            .to_string(),
        parallelism_group: task
            .executor_binding_parallelism_group()
            .map(ToString::to_string),
        worker_id: thread
            .map(|thread| thread.worker_instance_id.to_string())
            .or_else(|| branch.map(|branch| branch.worker_id.to_string())),
        thread_id: thread
            .map(|thread| thread.thread_id.to_string())
            .or_else(|| branch.map(|branch| branch.thread_id.to_string())),
        execution_chain_ref: execution_chain_ref.map(ToString::to_string),
        started_at: task.created_at,
        completed_at,
        response_duration_ms,
        updated_at: thread
            .map(|thread| thread.last_used_at)
            .unwrap_or(task.updated_at),
        result: agent_projection_result(task, thread),
    }
}

fn agent_runtime_status(task: &Task) -> (&'static str, &'static str) {
    (task_status_slug(task.status), agent_lifecycle(task))
}

fn agent_model_bindings_for_state(state: &ApiState) -> HashMap<String, AgentModelBinding> {
    let engines = load_registry_engines(state);
    let model_by_engine_id = engines
        .into_iter()
        .filter_map(|engine| {
            let engine_id = engine
                .get("id")
                .or_else(|| engine.get("engineId"))?
                .as_str()?
                .trim()
                .to_string();
            if engine_id.is_empty() {
                return None;
            }
            let model = engine
                .get("llm")
                .and_then(|llm| llm.get("model"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            Some((engine_id, model))
        })
        .collect::<HashMap<_, _>>();
    resolve_registry_agents(state)
        .into_iter()
        .filter_map(|agent| {
            let role = agent.get("templateId")?.as_str()?.trim().to_string();
            if role.is_empty() {
                return None;
            }
            let engine_id = agent
                .get("engineId")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            let model_source = if engine_id.is_some() {
                "engine".to_string()
            } else {
                "inherited_orchestrator".to_string()
            };
            let model = engine_id
                .as_ref()
                .and_then(|engine_id| model_by_engine_id.get(engine_id))
                .cloned()
                .flatten();
            Some((
                role,
                AgentModelBinding {
                    engine_id,
                    model,
                    model_source,
                },
            ))
        })
        .collect()
}

fn task_status_slug(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
}

fn task_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "排队中",
        TaskStatus::Running => "运行中",
        TaskStatus::Completed => "已完成",
        TaskStatus::Failed => "失败",
        TaskStatus::Killed => "已终止",
    }
}

fn agent_lifecycle(task: &Task) -> &'static str {
    match task.status {
        TaskStatus::Pending => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Killed => "killed",
        TaskStatus::Failed
            if task
                .output_refs
                .iter()
                .any(|output| output == TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT) =>
        {
            "degraded"
        }
        TaskStatus::Failed => "failed",
    }
}

fn agent_projection_result(
    task: &Task,
    thread: Option<&ExecutionThread>,
) -> Option<AgentProjectionResultDto> {
    if !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    ) {
        return None;
    }
    let raw = thread
        .and_then(latest_assistant_text_from_thread)
        .or_else(|| {
            task.output_refs
                .iter()
                .rev()
                .find_map(|output| text_from_output_ref(output))
        })
        .unwrap_or_else(|| match task.status {
            TaskStatus::Completed => "代理未返回可展示输出".to_string(),
            TaskStatus::Killed => "代理任务被终止".to_string(),
            _ => "代理任务执行失败".to_string(),
        });
    let (final_text, truncated) = truncate_agent_result_text(&raw);
    Some(AgentProjectionResultDto {
        final_text,
        output_ref_count: task.output_refs.len(),
        truncated,
    })
}

fn latest_assistant_text_from_thread(thread: &ExecutionThread) -> Option<String> {
    thread
        .message_history
        .iter()
        .rev()
        .find(|message| message.role.trim().eq_ignore_ascii_case("assistant"))
        .and_then(|message| message.content.as_deref())
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(ToString::to_string)
}

fn text_from_output_ref(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Some(trimmed.to_string());
    };
    text_from_structured_output(&value).or_else(|| Some(trimmed.to_string()))
}

fn text_from_structured_output(value: &serde_json::Value) -> Option<String> {
    value
        .get("blocks")
        .and_then(|blocks| blocks.as_array())
        .and_then(|blocks| {
            blocks.iter().rev().find_map(|block| {
                let block_type = block.get("type").and_then(|value| value.as_str())?;
                if block_type != "text" {
                    return None;
                }
                block
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
        })
        .or_else(|| {
            value
                .get("result")
                .and_then(|result| {
                    result
                        .get("final_text")
                        .or_else(|| result.get("finalText"))
                        .and_then(|value| value.as_str())
                })
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
        })
}

fn truncate_agent_result_text(value: &str) -> (String, bool) {
    let trimmed = value.trim();
    if trimmed.chars().count() <= AGENT_RESULT_MAX_CHARS {
        return (trimmed.to_string(), false);
    }
    let mut output = trimmed
        .chars()
        .take(AGENT_RESULT_MAX_CHARS)
        .collect::<String>();
    output.push('…');
    (output, true)
}

async fn get_agent_run_projection(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<AgentRunProjectionResponseDto>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    let scope = require_session_task(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        &root_id,
    )?;
    let mut projection = store
        .build_agent_run_projection(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    apply_authoritative_runner_status(&state, &root_id, &mut projection);
    apply_recoverable_chain_summary(
        &state,
        query.session_id.as_deref(),
        &root_id,
        &mut projection,
    )?;
    let projection = public_agent_run_projection_for_api(projection);
    let active_chain = state
        .session_store
        .active_execution_chain(&scope.workspace.session_id)
        .filter(|chain| chain.root_task_id == root_id);
    let agent_model_bindings = agent_model_bindings_for_state(&state);
    let session_threads = state
        .session_store
        .thread_registry_snapshot(&scope.workspace.session_id);
    let agents = agent_read_model_for_projection(
        &projection,
        active_chain.as_ref(),
        &session_threads,
        &agent_model_bindings,
    );
    Ok(Json(agent_run_projection_response(
        projection, &scope, agents,
    )))
}

fn agent_run_projection_response(
    projection: AgentRunProjection,
    scope: &SessionTaskScope,
    agents: Vec<AgentProjectionDto>,
) -> AgentRunProjectionResponseDto {
    AgentRunProjectionResponseDto {
        projection,
        session_id: scope.workspace.session_id.to_string(),
        workspace_id: scope.workspace.workspace_id.to_string(),
        workspace_path: scope.workspace.workspace_path.clone(),
        agents,
    }
}

fn apply_recoverable_chain_summary(
    state: &ApiState,
    session_id_value: Option<&str>,
    root_task_id: &TaskId,
    projection: &mut AgentRunProjection,
) -> Result<(), ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let Some(chain) = state.session_store.active_execution_chain(&session_id) else {
        projection.has_recoverable_chain = false;
        projection.recoverable_branch_count = 0;
        return Ok(());
    };
    if &chain.root_task_id != root_task_id {
        projection.has_recoverable_chain = false;
        projection.recoverable_branch_count = 0;
        return Ok(());
    }
    let worker_runtime_handle = state
        .execution_pipeline()
        .map(|pipeline| pipeline.execution_runtime.worker_runtime());
    let count = chain
        .branches
        .iter()
        .filter(|branch| {
            active_execution_branch_is_continue_recoverable(
                worker_runtime_handle,
                state.task_store(),
                &chain,
                branch,
            )
        })
        .count();
    projection.has_recoverable_chain = count > 0;
    projection.recoverable_branch_count = count;
    Ok(())
}

fn apply_authoritative_runner_status(
    state: &ApiState,
    root_task_id: &TaskId,
    projection: &mut AgentRunProjection,
) {
    let Some(snapshot) = state
        .runner_manager()
        .and_then(|manager| manager.status(root_task_id.as_str()))
    else {
        return;
    };
    if let Some(status) = normalize_runner_status(&snapshot.status) {
        projection.runner_status = status.to_string();
    }
}

fn normalize_runner_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => Some("running"),
        "completed" => Some("completed"),
        "error" => Some("error"),
        "killed" => Some("killed"),
        "pending" => Some("pending"),
        "idle" => Some("idle"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApiState;
    use magi_core::{
        AbsolutePath, ExecutionOwnership, SessionId, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, UtcMillis,
        WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{ExecutionThreadStatus, SessionStore};
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn build_state() -> ApiState {
        let event_bus = Arc::new(InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let workspace_store = Arc::new(WorkspaceStore::default());
        let governance = Arc::new(GovernanceService::default());
        ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            workspace_store,
            governance,
        )
        .with_task_store(Arc::new(TaskStore::new()))
    }

    fn test_task(task_id: &str, mission_id: &MissionId) -> Task {
        let task_id = TaskId::new(task_id);
        Task {
            task_id: task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: task_id,
            parent_task_id: None,
            kind: TaskKind::LocalAgent,
            title: "测试任务".to_string(),
            goal: "验证 workspace 绑定".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: Some("/tmp/magi-task-workspace-a".to_string()),
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: Default::default(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }
    }

    #[test]
    fn public_task_for_api_redacts_internal_failed_output_refs() {
        let mission_id = MissionId::new("mission-task-redacted");
        let mut task = test_task("task-redacted", &mission_id);
        task.status = TaskStatus::Failed;
        task.output_refs =
            vec!["LLM invocation failed: provider transport failed: timed out".to_string()];

        let public_task = public_task_for_api(task);

        assert_eq!(
            public_task.output_refs,
            vec![TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT.to_string()]
        );
    }

    #[test]
    fn public_agent_run_projection_for_api_keeps_user_failure_refs() {
        let mission_id = MissionId::new("mission-task-user-failure");
        let mut root = test_task("task-user-failure-root", &mission_id);
        root.status = TaskStatus::Failed;
        root.output_refs = vec!["测试失败：断言不匹配".to_string()];
        let projection = AgentRunProjection {
            root_task: root.clone(),
            tasks: vec![root],
            running_tasks: Vec::new(),
            pending_tasks: Vec::new(),
            completed_tasks: Vec::new(),
            failed_tasks: vec![TaskId::new("task-user-failure-root")],
            killed_tasks: Vec::new(),
            progress_summary: Default::default(),
            aggregate_status: TaskStatus::Failed,
            display_status: "失败".to_string(),
            execution_mode: "execution_chain".to_string(),
            runner_status: "error".to_string(),
            has_recoverable_chain: false,
            recoverable_branch_count: 0,
        };

        let public_projection = public_agent_run_projection_for_api(projection);

        assert_eq!(
            public_projection.root_task.output_refs,
            vec!["测试失败：断言不匹配".to_string()]
        );
        assert_eq!(
            public_projection.tasks[0].output_refs,
            vec!["测试失败：断言不匹配".to_string()]
        );
    }

    #[test]
    fn completed_agent_task_cannot_be_reported_as_running_by_active_thread() {
        let mission_id = MissionId::new("mission-agent-terminal-status");
        let session_id = SessionId::new("session-agent-terminal-status");
        let mut task = test_task("task-agent-terminal-status", &mission_id);
        task.status = TaskStatus::Completed;
        task.parent_task_id = Some(TaskId::new("task-agent-terminal-parent"));
        let thread = ExecutionThread {
            thread_id: magi_core::ThreadId::new("thread-agent-terminal-status"),
            session_id,
            mission_id,
            role_id: "reviewer".to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-agent-terminal-status"),
            status: ExecutionThreadStatus::Active,
            created_at: UtcMillis(1),
            last_used_at: UtcMillis(2),
            handled_task_ids: vec![task.task_id.clone()],
            message_history: Vec::new(),
        };
        let projection =
            agent_projection_from_task(&task, None, Some(&thread), None, &HashMap::new());

        assert_eq!(projection.status, "completed");
        assert_eq!(projection.lifecycle, "completed");
    }

    #[test]
    fn agent_read_model_exposes_agent_runtime_identity_and_result() {
        let session_id = SessionId::new("session-agent-read-model");
        let mission_id = MissionId::new("mission-agent-read-model");
        let root_id = TaskId::new("task-agent-root");
        let child_id = TaskId::new("task-agent-child");
        let mut root = test_task(root_id.as_str(), &mission_id);
        root.root_task_id = root_id.clone();
        let mut child = test_task(child_id.as_str(), &mission_id);
        child.root_task_id = root_id.clone();
        child.parent_task_id = Some(root_id.clone());
        child.title = "登录流程审查员".to_string();
        child.goal = "审查登录模块的异常中断风险".to_string();
        child.status = TaskStatus::Completed;
        child.executor_binding = Some(
            magi_core::TaskExecutorBinding::for_role("reviewer")
                .with_parallelism_group(Some("review-wave".to_string())),
        );
        child.policy_snapshot = Some(magi_core::TaskPolicy {
            autonomy_level: "guided".to_string(),
            access_profile: AccessProfile::ReadOnly,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            read_only_paths: Vec::new(),
            network_mode: "disabled".to_string(),
            command_mode: "restricted".to_string(),
            retry_limit: 0,
            validation_profile: None,
            checkpoint_mode: "none".to_string(),
            task_tier: magi_core::TaskTier::ExecutionChain,
            background_allowed: false,
            escalation_conditions: Vec::new(),
        });
        child.output_refs = vec![
            serde_json::json!({
                "blocks": [
                    { "type": "text", "content": "旧 task output，不应覆盖 thread transcript。" }
                ]
            })
            .to_string(),
        ];
        child.created_at = UtcMillis(10);
        child.updated_at = UtcMillis(20);
        let projection = AgentRunProjection {
            root_task: root,
            tasks: vec![child],
            running_tasks: Vec::new(),
            pending_tasks: Vec::new(),
            completed_tasks: vec![child_id.clone()],
            failed_tasks: Vec::new(),
            killed_tasks: Vec::new(),
            progress_summary: Default::default(),
            aggregate_status: TaskStatus::Completed,
            display_status: "完成".to_string(),
            execution_mode: "execution_chain".to_string(),
            runner_status: "completed".to_string(),
            has_recoverable_chain: false,
            recoverable_branch_count: 0,
        };
        let chain = ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: root_id.clone(),
            execution_chain_ref: "chain-agent-read-model".to_string(),
            workspace_id: None,
            active_branch_task_ids: vec![child_id.clone()],
            active_worker_bindings: vec![magi_core::WorkerId::new("worker-agent-child")],
            branches: vec![magi_session_store::ActiveExecutionBranch {
                task_id: child_id.clone(),
                worker_id: magi_core::WorkerId::new("worker-agent-child"),
                stage: "execute".to_string(),
                lease_id: None,
                execution_intent_ref: None,
                binding_lifecycle: None,
                checkpoint_stage: Some("execute".to_string()),
                next_step_index: Some(0),
                checkpoint_at: Some(UtcMillis(11)),
                resume_mode: Some("stage-restart".to_string()),
                resume_token: None,
                use_tools: true,
                skill_name: None,
                is_primary: false,
                thread_id: magi_core::ThreadId::new("thread-agent-child"),
            }],
            recovery_ref: None,
            dispatch_context: magi_session_store::ActiveExecutionDispatchContext {
                accepted_at: UtcMillis(1),
                entry_id: "entry-agent-read-model".to_string(),
                trimmed_text: Some("测试代理读模型".to_string()),
                skill_name: None,
            },
            current_turn: None,
        };
        let session_threads = vec![ExecutionThread {
            thread_id: magi_core::ThreadId::new("thread-agent-child"),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: "reviewer".to_string(),
            worker_instance_id: magi_core::WorkerId::new("worker-agent-child"),
            status: ExecutionThreadStatus::Idle,
            created_at: UtcMillis(9),
            last_used_at: UtcMillis(21),
            handled_task_ids: vec![child_id.clone()],
            message_history: vec![magi_session_store::ThreadChatMessage {
                role: "assistant".to_string(),
                content: Some("代理完成：thread transcript 是权威结果。".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
        }];

        let mut model_bindings = HashMap::new();
        model_bindings.insert(
            "reviewer".to_string(),
            AgentModelBinding {
                engine_id: Some("engine-reviewer".to_string()),
                model: Some("gpt-reviewer".to_string()),
                model_source: "engine".to_string(),
            },
        );

        let agents = agent_read_model_for_projection(
            &projection,
            Some(&chain),
            &session_threads,
            &model_bindings,
        );

        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.agent_run_id, "task-agent-child");
        assert_eq!(agent.display_name, "登录流程审查员");
        assert_eq!(agent.role, "reviewer");
        assert_eq!(agent.engine_id.as_deref(), Some("engine-reviewer"));
        assert_eq!(agent.model.as_deref(), Some("gpt-reviewer"));
        assert_eq!(agent.model_source, "engine");
        assert_eq!(agent.lifecycle, "completed");
        assert_eq!(agent.started_at, UtcMillis(10));
        assert_eq!(agent.completed_at, Some(UtcMillis(20)));
        assert_eq!(agent.response_duration_ms, Some(10));
        assert_eq!(agent.access_profile, "read_only");
        assert_eq!(agent.parallelism_group.as_deref(), Some("review-wave"));
        assert_eq!(agent.worker_id.as_deref(), Some("worker-agent-child"));
        assert_eq!(agent.thread_id.as_deref(), Some("thread-agent-child"));
        assert_eq!(
            agent.execution_chain_ref.as_deref(),
            Some("chain-agent-read-model")
        );
        assert_eq!(
            agent
                .result
                .as_ref()
                .map(|result| result.final_text.as_str()),
            Some("代理完成：thread transcript 是权威结果。")
        );
    }

    #[test]
    fn agent_run_projection_response_serializes_scope_and_agents_as_first_class_fields() {
        let session_id = SessionId::new("session-projection-response");
        let workspace_id = WorkspaceId::new("workspace-projection-response");
        let mission_id = MissionId::new("mission-projection-response");
        let root = test_task("agent-run-response-root", &mission_id);
        let projection = AgentRunProjection {
            root_task: root.clone(),
            tasks: vec![root],
            running_tasks: vec![TaskId::new("agent-run-response-root")],
            pending_tasks: Vec::new(),
            completed_tasks: Vec::new(),
            failed_tasks: Vec::new(),
            killed_tasks: Vec::new(),
            progress_summary: Default::default(),
            aggregate_status: TaskStatus::Running,
            display_status: "运行中".to_string(),
            execution_mode: "execution_chain".to_string(),
            runner_status: "running".to_string(),
            has_recoverable_chain: false,
            recoverable_branch_count: 0,
        };
        let scope = SessionTaskScope {
            workspace: SessionWorkspaceScope {
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                workspace_path: "/tmp/projection-response".to_string(),
            },
            mission_id: Some(mission_id),
        };
        let agents = vec![AgentProjectionDto {
            agent_run_id: "task-agent-response".to_string(),
            parent_task_id: "agent-run-response-root".to_string(),
            root_task_id: "agent-run-response-root".to_string(),
            display_name: "响应代理".to_string(),
            goal: "验证响应 DTO".to_string(),
            role: "reviewer".to_string(),
            engine_id: None,
            model: None,
            model_source: "inherited_orchestrator".to_string(),
            status: "running".to_string(),
            status_label: "运行中".to_string(),
            lifecycle: "running".to_string(),
            access_profile: "read_only".to_string(),
            parallelism_group: None,
            worker_id: Some("worker-response".to_string()),
            thread_id: Some("thread-response".to_string()),
            execution_chain_ref: Some("chain-response".to_string()),
            started_at: UtcMillis(1),
            completed_at: None,
            response_duration_ms: None,
            updated_at: UtcMillis(2),
            result: None,
        }];

        let value = serde_json::to_value(agent_run_projection_response(projection, &scope, agents))
            .expect("response should serialize");

        assert_eq!(value["sessionId"].as_str(), Some(session_id.as_str()));
        assert_eq!(value["workspaceId"].as_str(), Some(workspace_id.as_str()));
        assert_eq!(
            value["workspacePath"].as_str(),
            Some("/tmp/projection-response")
        );
        assert_eq!(
            value["root_task"]["task_id"].as_str(),
            Some("agent-run-response-root")
        );
        assert_eq!(
            value["agents"][0]["agentRunId"].as_str(),
            Some("task-agent-response")
        );
        assert_eq!(
            value["agents"][0]["modelSource"].as_str(),
            Some("inherited_orchestrator")
        );
        assert_eq!(value["agents"][0]["accessProfile"], "read_only");
        assert!(value["agents"][0]["completedAt"].is_null());
        assert!(value["agents"][0]["responseDurationMs"].is_null());
        assert!(value["agents"][0].get("accessMode").is_none());
    }

    #[test]
    fn agent_model_bindings_resolve_registry_engine_id_model_contract() {
        let state = build_state();
        state
            .settings_store
            .set_section(
                "engines",
                serde_json::json!([
                    {
                        "id": "glm-5-1",
                        "displayName": "GLM-5.1",
                        "llm": {
                            "baseUrl": "http://localhost:8317/",
                            "apiKey": "test-key",
                            "model": "glm-5.1"
                        }
                    }
                ]),
            )
            .unwrap();
        state
            .settings_store
            .set_section(
                "agents",
                serde_json::json!([
                    {
                        "templateId": "explorer",
                        "engineId": "glm-5-1",
                        "order": 1
                    }
                ]),
            )
            .unwrap();

        let bindings = agent_model_bindings_for_state(&state);
        let explorer = bindings
            .get("explorer")
            .expect("explorer binding should be resolved");

        assert_eq!(explorer.engine_id.as_deref(), Some("glm-5-1"));
        assert_eq!(explorer.model.as_deref(), Some("glm-5.1"));
        assert_eq!(explorer.model_source, "engine");
    }

    #[test]
    fn require_session_task_rejects_cross_workspace_scope() {
        let state = build_state();
        let workspace_a = WorkspaceId::new("workspace-task-a");
        let workspace_b = WorkspaceId::new("workspace-task-b");
        let session_id = SessionId::new("session-task-a");
        let mission_id = MissionId::new("mission-task-a");
        let task_id = TaskId::new("root-task-a");

        state
            .workspace_registry
            .register(
                workspace_a.clone(),
                AbsolutePath::new("/tmp/magi-task-workspace-a"),
            )
            .expect("workspace a should register");
        state
            .workspace_registry
            .register(
                workspace_b.clone(),
                AbsolutePath::new("/tmp/magi-task-workspace-b"),
            )
            .expect("workspace b should register");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "任务会话",
                Some(workspace_a.to_string()),
            )
            .expect("session should create");
        state.session_store.bind_execution_ownership(
            session_id.clone(),
            ExecutionOwnership {
                session_id: Some(session_id.clone()),
                workspace_id: Some(workspace_a.clone()),
                mission_id: Some(mission_id.clone()),
                ..ExecutionOwnership::default()
            },
        );
        state
            .task_store()
            .expect("task store should exist")
            .insert_task(test_task(task_id.as_str(), &mission_id));

        let err = require_session_task(
            &state,
            Some(session_id.as_str()),
            Some(workspace_b.as_str()),
            None,
            &task_id,
        )
        .expect_err("cross workspace task request must be rejected");
        let message = format!("{err:?}");
        assert!(
            message.contains("不属于 workspace"),
            "unexpected error: {message}"
        );

        let scope = require_session_task(
            &state,
            Some(session_id.as_str()),
            Some(workspace_a.as_str()),
            None,
            &task_id,
        )
        .expect("matching workspace should pass");
        assert_eq!(scope.workspace.workspace_id, workspace_a);
        assert_eq!(scope.workspace.session_id, session_id);
        assert!(
            scope
                .workspace
                .workspace_path
                .ends_with("magi-task-workspace-a")
        );

        let scope = require_session_task(
            &state,
            Some(session_id.as_str()),
            Some("workspace-stale-query"),
            Some("/tmp/magi-task-workspace-a"),
            &task_id,
        )
        .expect("registered workspacePath should resolve stale workspace id");
        assert_eq!(scope.workspace.workspace_id, workspace_a);
        assert_eq!(scope.workspace.session_id, session_id);
    }
}

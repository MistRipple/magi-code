use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use magi_core::{
    AccessProfile, MissionId, TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT, Task, TaskId, TaskKind,
    TaskProjection, TaskStatus, public_task_output_refs,
};
use magi_session_store::{ActiveExecutionChain, ActiveExecutionTurn};
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

const DEFAULT_SESSION_TASK_HISTORY_LIMIT: usize = 12;
const MAX_SESSION_TASK_HISTORY_LIMIT: usize = 50;
const AGENT_RESULT_MAX_CHARS: usize = 8000;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/tasks/session-history", get(get_session_task_history))
        .route("/tasks/projection/{root_task_id}", get(get_task_projection))
        .route("/tasks/{task_id}", get(get_task))
        .route(
            "/tasks/{root_task_id}/delivery-package",
            get(get_delivery_package),
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
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTaskHistoryResponse {
    session_id: String,
    workspace_id: String,
    workspace_path: String,
    items: Vec<SessionTaskHistoryItemDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTaskHistoryItemDto {
    root_task: Task,
    runner_status: String,
    display_status: String,
    execution_mode: String,
    active: bool,
    archived: bool,
    restartable: bool,
    updated_at: magi_core::UtcMillis,
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
    task_id: String,
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
    access_mode: String,
    parallelism_group: Option<String>,
    worker_id: Option<String>,
    thread_id: Option<String>,
    execution_chain_ref: Option<String>,
    started_at: magi_core::UtcMillis,
    updated_at: magi_core::UtcMillis,
    result: Option<AgentProjectionResultDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskProjectionResponseDto {
    #[serde(flatten)]
    projection: TaskProjection,
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
        "读取任务投影",
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

fn push_session_history_root(
    store: &magi_orchestrator::task_store::TaskStore,
    task_id: &TaskId,
    seen: &mut HashSet<TaskId>,
    roots: &mut Vec<TaskId>,
) {
    let Some(task) = store.get_task(task_id) else {
        return;
    };
    if seen.insert(task.root_task_id.clone()) {
        roots.push(task.root_task_id);
    }
}

fn session_task_history_root_ids(
    state: &ApiState,
    session_id: &magi_core::SessionId,
    store: &magi_orchestrator::task_store::TaskStore,
) -> Vec<TaskId> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    if let Some(sidecar) = state.session_store.runtime_sidecar(session_id) {
        if let Some(chain) = sidecar.active_execution_chain.as_ref() {
            push_session_history_root(store, &chain.root_task_id, &mut seen, &mut roots);
        }
        if let Some(turn) = sidecar.current_turn.as_ref() {
            for item in &turn.items {
                if let Some(task_id) = item.task_id.as_ref() {
                    push_session_history_root(store, task_id, &mut seen, &mut roots);
                }
            }
        }
    }

    for turn in state.session_store.canonical_turns_for_session(session_id) {
        for item in turn.items {
            if let Some(task_id) = item.worker.and_then(|worker| worker.task_id) {
                push_session_history_root(store, &task_id, &mut seen, &mut roots);
            }
        }
    }

    roots
}

fn active_session_root_task_id(
    state: &ApiState,
    session_id: &magi_core::SessionId,
) -> Option<TaskId> {
    state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| {
            sidecar
                .active_execution_chain
                .map(|chain| chain.root_task_id)
        })
}

fn task_status_from_turn_status(status: &str) -> TaskStatus {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "succeeded" | "success" => TaskStatus::Completed,
        "failed" | "error" | "blocked" => TaskStatus::Failed,
        "cancelled" | "canceled" | "killed" => TaskStatus::Killed,
        "running" | "executing" | "repairing" | "verifying" => TaskStatus::Running,
        _ => TaskStatus::Pending,
    }
}

fn runner_status_from_task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "error",
        TaskStatus::Killed => "killed",
    }
}

fn public_task_for_api(mut task: Task) -> Task {
    task.output_refs = public_task_output_refs(task.status, &task.output_refs);
    task
}

fn public_task_projection_for_api(mut projection: TaskProjection) -> TaskProjection {
    projection.root_task = public_task_for_api(projection.root_task);
    projection.tasks = projection
        .tasks
        .into_iter()
        .map(public_task_for_api)
        .collect();
    projection
}

fn agent_read_model_for_projection(
    projection: &TaskProjection,
    chain: Option<&ActiveExecutionChain>,
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
    let mut agents = projection
        .tasks
        .iter()
        .filter(|task| task.parent_task_id.is_some())
        .filter(|task| {
            task.executor_binding_target_role().is_some() || task.kind == TaskKind::LocalAgent
        })
        .map(|task| {
            let branch = branches_by_task.get(&task.task_id);
            let role = task
                .executor_binding_target_role()
                .unwrap_or("agent")
                .to_string();
            let model_binding =
                model_bindings
                    .get(&role)
                    .cloned()
                    .unwrap_or_else(|| AgentModelBinding {
                        model_source: "unconfigured".to_string(),
                        ..AgentModelBinding::default()
                    });
            AgentProjectionDto {
                task_id: task.task_id.to_string(),
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
                status: task_status_slug(task.status).to_string(),
                status_label: task_status_label(task.status).to_string(),
                lifecycle: agent_lifecycle(task).to_string(),
                access_mode: task
                    .policy_snapshot
                    .as_ref()
                    .map(|policy| policy.access_profile.as_str())
                    .unwrap_or(AccessProfile::Restricted.as_str())
                    .to_string(),
                parallelism_group: task
                    .executor_binding_parallelism_group()
                    .map(ToString::to_string),
                worker_id: branch.map(|branch| branch.worker_id.to_string()),
                thread_id: branch.map(|branch| branch.thread_id.to_string()),
                execution_chain_ref: execution_chain_ref.clone(),
                started_at: task.created_at,
                updated_at: task.updated_at,
                result: agent_projection_result(task),
            }
        })
        .collect::<Vec<_>>();
    agents.sort_by(|left, right| {
        left.started_at
            .0
            .cmp(&right.started_at.0)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    agents
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

fn agent_projection_result(task: &Task) -> Option<AgentProjectionResultDto> {
    if !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    ) {
        return None;
    }
    let raw = task
        .output_refs
        .iter()
        .rev()
        .find_map(|output| text_from_output_ref(output))
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

fn synthetic_history_task_from_turn(
    session_id: &magi_core::SessionId,
    turn: &ActiveExecutionTurn,
    task_id: &TaskId,
) -> Task {
    let title = turn
        .user_message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            turn.items.iter().find_map(|item| {
                item.title
                    .as_deref()
                    .or(item.content.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| task_id.to_string());
    let status = task_status_from_turn_status(&turn.status);
    Task {
        task_id: task_id.clone(),
        mission_id: MissionId::new(format!("mission-history-{}", session_id)),
        root_task_id: task_id.clone(),
        parent_task_id: None,
        kind: TaskKind::LocalAgent,
        title: title.clone(),
        goal: title,
        status,
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
        runtime_payload: Default::default(),
        created_at: turn.accepted_at,
        updated_at: turn.completed_at.unwrap_or(turn.accepted_at),
    }
}

fn runner_status_for_projection(
    state: &ApiState,
    root_id: &TaskId,
    projection: &mut TaskProjection,
) {
    apply_authoritative_runner_status(state, root_id, projection);
}

async fn get_session_task_history(
    State(state): State<ApiState>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<SessionTaskHistoryResponse>, ApiError> {
    let store = require_task_store(&state)?;
    let scope = require_session_task_scope(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let session_id = scope.workspace.session_id.clone();
    let active_root_task_id = active_session_root_task_id(&state, &session_id);
    let mut items = Vec::new();
    let mut visible_roots = HashSet::new();

    for root_id in session_task_history_root_ids(&state, &session_id, store) {
        let Some(mut projection) = store.build_projection(&root_id) else {
            continue;
        };
        visible_roots.insert(root_id.clone());
        runner_status_for_projection(&state, &root_id, &mut projection);
        let active = active_root_task_id.as_ref() == Some(&root_id);
        let updated_at = projection
            .tasks
            .iter()
            .map(|task| task.updated_at)
            .max()
            .unwrap_or(projection.root_task.updated_at);
        items.push(SessionTaskHistoryItemDto {
            root_task: public_task_for_api(projection.root_task.clone()),
            runner_status: projection.runner_status,
            display_status: projection.display_status,
            execution_mode: projection.execution_mode,
            active,
            archived: !active,
            restartable: !matches!(
                projection.root_task.status,
                TaskStatus::Running | TaskStatus::Pending
            ),
            updated_at,
        });
    }

    if let Some(turn) = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
    {
        for task_id in turn.items.iter().filter_map(|item| item.task_id.as_ref()) {
            if visible_roots.contains(task_id) || store.get_task(task_id).is_some() {
                continue;
            }
            visible_roots.insert(task_id.clone());
            let task = synthetic_history_task_from_turn(&session_id, &turn, task_id);
            let runner_status = runner_status_from_task_status(task.status).to_string();
            items.push(SessionTaskHistoryItemDto {
                root_task: public_task_for_api(task.clone()),
                runner_status,
                display_status: "已完成".to_string(),
                execution_mode: "session_turn".to_string(),
                active: false,
                archived: true,
                restartable: false,
                updated_at: task.updated_at,
            });
        }
    }

    items.sort_by(|left, right| {
        right.updated_at.0.cmp(&left.updated_at.0).then_with(|| {
            right
                .root_task
                .task_id
                .as_str()
                .cmp(left.root_task.task_id.as_str())
        })
    });
    let limit = query
        .limit
        .unwrap_or(DEFAULT_SESSION_TASK_HISTORY_LIMIT)
        .clamp(1, MAX_SESSION_TASK_HISTORY_LIMIT);
    items.truncate(limit);

    Ok(Json(SessionTaskHistoryResponse {
        session_id: session_id.to_string(),
        workspace_id: scope.workspace.workspace_id.to_string(),
        workspace_path: scope.workspace.workspace_path,
        items,
    }))
}

async fn get_task_projection(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<TaskProjectionResponseDto>, ApiError> {
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
        .build_projection(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    apply_authoritative_runner_status(&state, &root_id, &mut projection);
    apply_recoverable_chain_summary(
        &state,
        query.session_id.as_deref(),
        &root_id,
        &mut projection,
    )?;
    let projection = public_task_projection_for_api(projection);
    let active_chain = state
        .session_store
        .active_execution_chain(&scope.workspace.session_id)
        .filter(|chain| chain.root_task_id == root_id);
    let agent_model_bindings = agent_model_bindings_for_state(&state);
    let agents =
        agent_read_model_for_projection(&projection, active_chain.as_ref(), &agent_model_bindings);
    Ok(Json(task_projection_response(projection, &scope, agents)))
}

fn task_projection_response(
    projection: TaskProjection,
    scope: &SessionTaskScope,
    agents: Vec<AgentProjectionDto>,
) -> TaskProjectionResponseDto {
    TaskProjectionResponseDto {
        projection,
        session_id: scope.workspace.session_id.to_string(),
        workspace_id: scope.workspace.workspace_id.to_string(),
        workspace_path: scope.workspace.workspace_path.clone(),
        agents,
    }
}

fn attach_task_scope(value: &mut serde_json::Value, scope: &SessionTaskScope) {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "sessionId".to_string(),
            serde_json::Value::String(scope.workspace.session_id.to_string()),
        );
        object.insert(
            "workspaceId".to_string(),
            serde_json::Value::String(scope.workspace.workspace_id.to_string()),
        );
        object.insert(
            "workspacePath".to_string(),
            serde_json::Value::String(scope.workspace.workspace_path.clone()),
        );
    }
}

fn apply_recoverable_chain_summary(
    state: &ApiState,
    session_id_value: Option<&str>,
    root_task_id: &TaskId,
    projection: &mut TaskProjection,
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
    projection: &mut TaskProjection,
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

async fn get_task(
    State(state): State<ApiState>,
    Path(task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let id = TaskId::new(&task_id);
    let scope = require_session_task(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        &id,
    )?;
    let task = store
        .get_task(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &task_id))?;
    let task = public_task_for_api(task);
    let mut value = serde_json::to_value(&task)
        .map_err(|err| ApiError::internal_assembly("序列化任务失败", err))?;
    attach_task_scope(&mut value, &scope);
    Ok(Json(value))
}

async fn get_delivery_package(
    State(state): State<ApiState>,
    Path(root_task_id): Path<String>,
    Query(query): Query<SessionScopedTaskQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = require_task_store(&state)?;
    let root_id = TaskId::new(&root_task_id);
    let scope = require_session_task(
        &state,
        query.session_id.as_deref(),
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
        &root_id,
    )?;

    let mut package = store
        .build_delivery_package(&root_id)
        .ok_or_else(|| ApiError::not_found("任务不存在", &root_task_id))?;
    attach_task_scope(&mut package, &scope);

    Ok(Json(package))
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
    use magi_session_store::SessionStore;
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
    fn public_task_projection_for_api_keeps_user_failure_refs() {
        let mission_id = MissionId::new("mission-task-user-failure");
        let mut root = test_task("task-user-failure-root", &mission_id);
        root.status = TaskStatus::Failed;
        root.output_refs = vec!["测试失败：断言不匹配".to_string()];
        let projection = TaskProjection {
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

        let public_projection = public_task_projection_for_api(projection);

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
                    { "type": "text", "content": "代理完成：发现 2 个风险点。" }
                ]
            })
            .to_string(),
        ];
        child.created_at = UtcMillis(10);
        child.updated_at = UtcMillis(20);
        let projection = TaskProjection {
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
            session_id,
            mission_id,
            root_task_id: root_id,
            execution_chain_ref: "chain-agent-read-model".to_string(),
            workspace_id: None,
            active_branch_task_ids: vec![child_id.clone()],
            active_worker_bindings: vec![magi_core::WorkerId::new("worker-agent-child")],
            branches: vec![magi_session_store::ActiveExecutionBranch {
                task_id: child_id,
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

        let mut model_bindings = HashMap::new();
        model_bindings.insert(
            "reviewer".to_string(),
            AgentModelBinding {
                engine_id: Some("engine-reviewer".to_string()),
                model: Some("gpt-reviewer".to_string()),
                model_source: "engine".to_string(),
            },
        );

        let agents = agent_read_model_for_projection(&projection, Some(&chain), &model_bindings);

        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.task_id, "task-agent-child");
        assert_eq!(agent.display_name, "登录流程审查员");
        assert_eq!(agent.role, "reviewer");
        assert_eq!(agent.engine_id.as_deref(), Some("engine-reviewer"));
        assert_eq!(agent.model.as_deref(), Some("gpt-reviewer"));
        assert_eq!(agent.model_source, "engine");
        assert_eq!(agent.lifecycle, "completed");
        assert_eq!(agent.access_mode, "read_only");
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
            Some("代理完成：发现 2 个风险点。")
        );
    }

    #[test]
    fn task_projection_response_serializes_scope_and_agents_as_first_class_fields() {
        let session_id = SessionId::new("session-projection-response");
        let workspace_id = WorkspaceId::new("workspace-projection-response");
        let mission_id = MissionId::new("mission-projection-response");
        let root = test_task("task-projection-response-root", &mission_id);
        let projection = TaskProjection {
            root_task: root.clone(),
            tasks: vec![root],
            running_tasks: vec![TaskId::new("task-projection-response-root")],
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
            task_id: "task-agent-response".to_string(),
            parent_task_id: "task-projection-response-root".to_string(),
            root_task_id: "task-projection-response-root".to_string(),
            display_name: "响应代理".to_string(),
            goal: "验证响应 DTO".to_string(),
            role: "reviewer".to_string(),
            engine_id: None,
            model: None,
            model_source: "inherited_orchestrator".to_string(),
            status: "running".to_string(),
            status_label: "运行中".to_string(),
            lifecycle: "running".to_string(),
            access_mode: "read_only".to_string(),
            parallelism_group: None,
            worker_id: Some("worker-response".to_string()),
            thread_id: Some("thread-response".to_string()),
            execution_chain_ref: Some("chain-response".to_string()),
            started_at: UtcMillis(1),
            updated_at: UtcMillis(2),
            result: None,
        }];

        let value = serde_json::to_value(task_projection_response(projection, &scope, agents))
            .expect("response should serialize");

        assert_eq!(value["sessionId"].as_str(), Some(session_id.as_str()));
        assert_eq!(value["workspaceId"].as_str(), Some(workspace_id.as_str()));
        assert_eq!(
            value["workspacePath"].as_str(),
            Some("/tmp/projection-response")
        );
        assert_eq!(
            value["root_task"]["task_id"].as_str(),
            Some("task-projection-response-root")
        );
        assert_eq!(
            value["agents"][0]["taskId"].as_str(),
            Some("task-agent-response")
        );
        assert_eq!(
            value["agents"][0]["modelSource"].as_str(),
            Some("inherited_orchestrator")
        );
    }

    #[test]
    fn agent_model_bindings_resolve_registry_engine_id_model_contract() {
        let state = build_state();
        state.settings_store.set_section(
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
        );
        state.settings_store.set_section(
            "agents",
            serde_json::json!([
                {
                    "templateId": "explorer",
                    "engineId": "glm-5-1",
                    "order": 1
                }
            ]),
        );

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

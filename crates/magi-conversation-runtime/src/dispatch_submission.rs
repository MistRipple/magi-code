//! Task System v2 — M17a：派发提交载体（DispatchSubmissionRequest /
//! DispatchSubmissionAccepted）从 magi-api/task_execution.rs 下沉到
//! conversation-runtime。
//!
//! 这两个 DTO 与 ApiState / ApiError 无任何运行期耦合，是 v2 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变；M17b 继续把"接受派发提交"这段只依赖 SessionStore / TaskStore /
//! TaskExecutionRegistry 的流程下沉到这里；magi-api 仅保留 run_dispatch_submission 与
//! runner 启动桥接。

use magi_core::{DomainError, SessionId, TaskId, UtcMillis, WorkspaceId};
use magi_orchestrator::task_store::TaskStore;
use magi_session_store::{SessionStore, TimelineEntryKind};

use crate::task_execution_registry::TaskExecutionRegistry;
use crate::task_graph_builder::{TaskGraphSubmission, cleanup_task_tree};

#[derive(Clone, Debug)]
pub struct DispatchSubmissionRequest {
    pub accepted_at: UtcMillis,
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub entry_id: String,
    pub timeline_message: String,
    pub created_session: bool,
    pub mission_title: String,
    pub task_title: String,
    pub trimmed_text: Option<String>,
    pub execution_goal: Option<String>,
    pub skill_name: Option<String>,
    pub target_role: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DispatchSubmissionAccepted {
    pub session_id: SessionId,
    pub entry_id: String,
    pub accepted_at: UtcMillis,
    pub created_session: bool,
    pub root_task_id: TaskId,
    pub action_task_id: TaskId,
    pub runner_started: bool,
}

#[derive(Debug)]
pub enum DispatchSubmissionAcceptError {
    Conflict { message: String },
    Internal { message: String },
}

impl DispatchSubmissionAcceptError {
    pub fn from_store_error(error: DomainError) -> Self {
        match error {
            DomainError::InvalidState { message } if message.contains("active current_turn") => {
                Self::Conflict { message }
            }
            other => Self::Internal {
                message: other.to_string(),
            },
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Conflict { message } | Self::Internal { message } => message,
        }
    }
}

pub fn ensure_dispatch_submission_acceptance_available(
    session_store: &SessionStore,
    request: &DispatchSubmissionRequest,
) -> Result<(), DispatchSubmissionAcceptError> {
    session_store
        .ensure_current_turn_acceptance_available(&request.session_id)
        .map_err(DispatchSubmissionAcceptError::from_store_error)
}

pub fn cleanup_rejected_dispatch(
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    graph: &TaskGraphSubmission,
) {
    if let Some(chain) = graph.active_execution_chain.as_ref() {
        for branch in &chain.branches {
            let _ = execution_registry.remove(&branch.task_id);
        }
    }
    if let Some(task_store) = task_store {
        cleanup_task_tree(task_store, &graph.root_task_id);
    }
}

pub fn accept_dispatch_submission(
    session_store: &SessionStore,
    task_store: Option<&TaskStore>,
    execution_registry: &TaskExecutionRegistry,
    request: DispatchSubmissionRequest,
    graph: TaskGraphSubmission,
) -> Result<DispatchSubmissionAccepted, DispatchSubmissionAcceptError> {
    if let Some(active_execution_chain) = graph.active_execution_chain.clone() {
        let accept_result = session_store.accept_active_execution_chain_with_timeline_entry(
            request.session_id.clone(),
            request.entry_id.clone(),
            TimelineEntryKind::UserMessage,
            request.timeline_message.clone(),
            request.accepted_at,
            active_execution_chain,
        );
        if let Err(error) = accept_result {
            cleanup_rejected_dispatch(task_store, execution_registry, &graph);
            return Err(DispatchSubmissionAcceptError::from_store_error(error));
        }
    }

    Ok(DispatchSubmissionAccepted {
        session_id: request.session_id,
        entry_id: request.entry_id,
        accepted_at: request.accepted_at,
        created_session: request.created_session,
        root_task_id: graph.root_task_id,
        action_task_id: graph.action_task_id,
        runner_started: false,
    })
}

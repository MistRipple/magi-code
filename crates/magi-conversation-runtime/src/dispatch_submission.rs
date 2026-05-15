//! Task System v2 — M17a：派发提交载体（DispatchSubmissionRequest /
//! DispatchSubmissionAccepted）从 magi-api/task_execution.rs 下沉到
//! conversation-runtime。
//!
//! 这两个 DTO 与 ApiState / ApiError 无任何运行期耦合，是 v2 dispatch 流程的
//! "请求 → 接受" 一次性数据载体。magi-api 通过 `pub use` 重导出维持外部
//! import 路径不变；后续 M17b 的 submit_dispatch_submission / drive_task_graph
//! 整体下沉时一并消化最终的薄壳。

use magi_core::{SessionId, TaskId, UtcMillis, WorkspaceId};

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

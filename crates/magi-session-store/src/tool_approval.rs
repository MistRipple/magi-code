//! 审批事实属于 canonical 工具 item；进程内等待通道不承担授权事实。
use magi_core::{SessionId, TaskId, UtcMillis};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 用户审批的最长等待时间。过期后不会执行原始工具调用，也不会写入拒绝记忆；
/// 后续新的模型调用可以重新请求同一组参数的审批。
pub const TOOL_APPROVAL_TTL_MILLIS: u64 = 5 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    AllowOnce,
    AllowForTurn,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolApproval {
    pub approval_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub reason: String,
    pub requested_at: UtcMillis,
    /// 由子代理发起时的代理身份，供审批界面说明是谁在请求；主线请求为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<ApprovalRequestAgent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestAgent {
    pub role: String,
    pub title: String,
}

impl ApprovalRequestAgent {
    /// 子代理任务的身份；主线任务返回 `None`。
    pub fn for_task(task: &magi_core::Task) -> Option<Self> {
        task.parent_task_id.as_ref()?;
        Some(Self {
            role: task
                .executor_binding_target_role()
                .unwrap_or("agent")
                .to_string(),
            title: task.title.clone(),
        })
    }
}

pub const TOOL_APPROVAL_METADATA_KEY: &str = "toolApproval";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedToolApproval {
    pub approval_id: String,
    pub decision: ToolApprovalDecision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalExecutionClaim {
    pub task_id: TaskId,
    pub turn_id: String,
    pub item_id: String,
    pub tool_call_id: String,
    pub started_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableToolApproval {
    pub request: PendingToolApproval,
    pub normalized_arguments: String,
    pub execution_root: Option<PathBuf>,
    pub policy_snapshot: serde_json::Value,
    pub decision: Option<ToolApprovalDecision>,
    pub decided_at: Option<UtcMillis>,
    pub cancelled_at: Option<UtcMillis>,
    pub execution: Option<ApprovalExecutionClaim>,
}

impl DurableToolApproval {
    pub fn denied_result(&self) -> Option<serde_json::Value> {
        (self.decision==Some(ToolApprovalDecision::Deny)).then(||serde_json::json!({
            "tool":self.request.tool_name,"status":"rejected","error_code":"tool_approval_denied",
            "error":"用户拒绝了本次工具操作","approval_id":self.request.approval_id,
            "execution_started":false,"retryable_with_same_arguments":false,
        }))
    }
    pub fn is_pending(&self, now: UtcMillis) -> bool {
        self.decision.is_none() && self.cancelled_at.is_none() && !self.is_expired(now)
    }
    pub fn is_expired(&self, now: UtcMillis) -> bool {
        self.request
            .requested_at
            .0
            .saturating_add(TOOL_APPROVAL_TTL_MILLIS)
            <= now.0
    }
}

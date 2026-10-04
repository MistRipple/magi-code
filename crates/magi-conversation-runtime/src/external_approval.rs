//! 外部（MCP）工具调用的人工审批等待。
//!
//! 复用 `ToolApprovalRegistry` 与 `tool.approval.requested` 事件，但存活判据不是“会话当前 turn
//! 仍在运行”，而是调用方提供的 `is_alive`（令牌仍有效且连接未断）。
//!
//! 每次调用使用独立的合成 `turn_id`（`external:<token_id>:<call_id>`），因此：
//! - 拒绝记忆只作用于这一次调用，用户之后改变主意可以重新发起；
//! - `AllowForTurn` 只对这一次调用生效，等同 `AllowOnce`；
//! - 调用结束后立即清理该 turn 的全部授权状态。

use std::time::{Duration, Instant};

use magi_core::{ExecutionResultStatus, SessionId, TaskId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};

use crate::tool_approval::{
    PendingToolApproval, TOOL_APPROVAL_TTL_MILLIS, ToolApprovalDecision, ToolApprovalRegistry,
    ToolApprovalRequestOutcome, expired_tool_approval_result, rejected_tool_approval_result,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct ExternalApprovalRequest<'a> {
    pub session_id: &'a SessionId,
    pub workspace_id: Option<&'a WorkspaceId>,
    pub token_id: &'a str,
    pub client_name: &'a str,
    pub token_prefix: &'a str,
    pub tool_call_id: &'a str,
    pub tool_name: &'a str,
    pub arguments_json: &'a str,
    /// 展示给用户的操作摘要（例如“写入 src/a.rs”）。不得包含文件正文或令牌。
    pub summary: &'a str,
    /// 等待上限；超过审批注册表 TTL 时按 TTL 截断。
    pub timeout: Duration,
}

/// 一个令牌下全部外部审批共用的任务标识，吊销令牌时按它整体取消。
pub fn external_approval_task_id(token_id: &str) -> TaskId {
    TaskId::new(format!("external:{token_id}"))
}

fn external_turn_id(token_id: &str, tool_call_id: &str) -> String {
    format!("external:{token_id}:{tool_call_id}")
}

/// 取消某令牌下所有待审批（令牌吊销、连接断开时调用）。等待方会以“已取消”收口。
pub fn cancel_external_approvals(
    registry: &ToolApprovalRegistry,
    session_id: &SessionId,
    token_id: &str,
) {
    registry.remove_task(session_id, &external_approval_task_id(token_id));
}

fn failure(
    request: &ExternalApprovalRequest<'_>,
    approval_id: &str,
    status: ExecutionResultStatus,
    code: &str,
    error: &str,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": request.tool_name,
            "status": match status {
                ExecutionResultStatus::Cancelled => "cancelled",
                ExecutionResultStatus::Rejected => "rejected",
                _ => "failed",
            },
            "error_code": code,
            "error": error,
            "approval_id": approval_id,
        })
        .to_string(),
        status,
    )
}

/// 发起审批并阻塞等待。`Ok(())` 表示用户允许，可以带 `approval_granted=true` 执行；
/// `Err` 为终态结果（拒绝、超时、取消、运行时故障），原始操作未执行。
pub fn await_external_tool_approval(
    event_bus: &InMemoryEventBus,
    registry: &ToolApprovalRegistry,
    request: &ExternalApprovalRequest<'_>,
    is_alive: &dyn Fn() -> bool,
) -> Result<(), (String, ExecutionResultStatus)> {
    let turn_id = external_turn_id(request.token_id, request.tool_call_id);
    let approval_id = format!("tool-approval-{turn_id}");
    let result = run_approval(
        event_bus,
        registry,
        request,
        is_alive,
        &turn_id,
        &approval_id,
    );
    registry.remove_turn(request.session_id, &turn_id);
    result
}

fn run_approval(
    event_bus: &InMemoryEventBus,
    registry: &ToolApprovalRegistry,
    request: &ExternalApprovalRequest<'_>,
    is_alive: &dyn Fn() -> bool,
    turn_id: &str,
    approval_id: &str,
) -> Result<(), (String, ExecutionResultStatus)> {
    if !is_alive() {
        return Err(failure(
            request,
            approval_id,
            ExecutionResultStatus::Cancelled,
            "tool_approval_cancelled",
            "连接已失效，待授权操作未执行",
        ));
    }
    let pending = PendingToolApproval {
        approval_id: approval_id.to_string(),
        session_id: request.session_id.clone(),
        task_id: external_approval_task_id(request.token_id),
        turn_id: turn_id.to_string(),
        tool_call_id: request.tool_call_id.to_string(),
        tool_name: request.tool_name.to_string(),
        reason: request.summary.to_string(),
        requested_at: UtcMillis::now(),
    };
    let waiter = match registry.request_with_arguments(pending, request.arguments_json) {
        Ok(ToolApprovalRequestOutcome::Pending(waiter)) => waiter,
        Ok(ToolApprovalRequestOutcome::AlreadyAllowed) => return Ok(()),
        Ok(ToolApprovalRequestOutcome::PreviouslyDenied) => {
            return Err(rejected_tool_approval_result(
                request.tool_name,
                approval_id,
                true,
            ));
        }
        Err(error) => {
            return Err(failure(
                request,
                approval_id,
                ExecutionResultStatus::Failed,
                "tool_approval_runtime_failed",
                &error,
            ));
        }
    };

    let _ = event_bus.publish(
        EventEnvelope::domain(
            magi_core::EventId::unique("event-tool-approval-requested"),
            "tool.approval.requested",
            serde_json::json!({
                "session_id": request.session_id,
                "workspace_id": request.workspace_id,
                "task_id": external_approval_task_id(request.token_id),
                "turn_id": turn_id,
                "tool_call_id": request.tool_call_id,
                "tool_name": request.tool_name,
                "approval_id": approval_id,
                "external": true,
                "client_name": request.client_name,
                "token_prefix": request.token_prefix,
                "summary": request.summary,
            }),
        )
        .with_context(EventContext {
            workspace_id: request.workspace_id.cloned(),
            session_id: Some(request.session_id.clone()),
            ..EventContext::default()
        }),
    );

    let deadline = Instant::now()
        + request
            .timeout
            .min(Duration::from_millis(TOOL_APPROVAL_TTL_MILLIS));
    loop {
        match waiter.decision_rx.recv_timeout(POLL_INTERVAL) {
            Ok(ToolApprovalDecision::AllowOnce | ToolApprovalDecision::AllowForTurn) => {
                if !is_alive() {
                    return Err(failure(
                        request,
                        approval_id,
                        ExecutionResultStatus::Cancelled,
                        "tool_approval_cancelled",
                        "连接已失效，待授权操作未执行",
                    ));
                }
                return Ok(());
            }
            Ok(ToolApprovalDecision::Deny) => {
                return Err(rejected_tool_approval_result(
                    request.tool_name,
                    approval_id,
                    false,
                ));
            }
            Err(error) => {
                if !is_alive() {
                    registry.cancel(approval_id);
                    return Err(failure(
                        request,
                        approval_id,
                        ExecutionResultStatus::Cancelled,
                        "tool_approval_cancelled",
                        "令牌已吊销或连接已断开，待授权操作未执行",
                    ));
                }
                if registry.is_expired(request.session_id, approval_id) {
                    return Err(expired_tool_approval_result(request.tool_name, approval_id));
                }
                if matches!(error, std::sync::mpsc::RecvTimeoutError::Disconnected) {
                    // 通道被外部清理（例如按令牌整体取消）但令牌仍存活：终态取消。
                    return Err(failure(
                        request,
                        approval_id,
                        ExecutionResultStatus::Cancelled,
                        "tool_approval_cancelled",
                        "授权请求已被取消，原始操作未执行",
                    ));
                }
                if Instant::now() >= deadline {
                    registry.cancel(approval_id);
                    return Err(failure(
                        request,
                        approval_id,
                        ExecutionResultStatus::Rejected,
                        "tool_approval_timeout",
                        "等待用户授权超时，原始操作未执行",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn request<'a>(session: &'a SessionId, timeout: Duration) -> ExternalApprovalRequest<'a> {
        ExternalApprovalRequest {
            session_id: session,
            workspace_id: None,
            token_id: "tok1",
            client_name: "cursor",
            token_prefix: "magi_mcp_ab12",
            tool_call_id: "call-1",
            tool_name: "file_write",
            arguments_json: r#"{"path":"a.txt"}"#,
            summary: "写入 a.txt",
            timeout,
        }
    }

    fn resolve_soon(
        registry: &ToolApprovalRegistry,
        session: &SessionId,
        decision: ToolApprovalDecision,
    ) -> std::thread::JoinHandle<()> {
        let registry = registry.clone();
        let session = session.clone();
        std::thread::spawn(move || {
            let approval_id = "tool-approval-external:tok1:call-1";
            for _ in 0..100 {
                if registry.resolve(&session, approval_id, decision).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    }

    #[test]
    fn allow_returns_ok_and_cleans_up_state() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let handle = resolve_soon(&registry, &session, ToolApprovalDecision::AllowOnce);
        let result = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(5)),
            &|| true,
        );
        handle.join().unwrap();
        assert!(result.is_ok());
        assert!(registry.pending_for_session(&session).is_empty());
    }

    #[test]
    fn deny_is_rejected_and_does_not_block_a_later_retry() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let handle = resolve_soon(&registry, &session, ToolApprovalDecision::Deny);
        let (payload, status) = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(5)),
            &|| true,
        )
        .unwrap_err();
        handle.join().unwrap();
        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("tool_approval_denied"));

        // 同一 call_id 再次发起：拒绝记忆已随 turn 清理，可以再次进入审批并被允许。
        let handle = resolve_soon(&registry, &session, ToolApprovalDecision::AllowOnce);
        let retry = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(5)),
            &|| true,
        );
        handle.join().unwrap();
        assert!(retry.is_ok());
    }

    #[test]
    fn timeout_is_rejected_and_leaves_nothing_pending() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let (payload, status) = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_millis(250)),
            &|| true,
        )
        .unwrap_err();
        assert_eq!(status, ExecutionResultStatus::Rejected);
        assert!(payload.contains("tool_approval_timeout"));
        assert!(registry.pending_for_session(&session).is_empty());
    }

    #[test]
    fn dead_connection_cancels_waiting_approval() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let alive = Arc::new(AtomicBool::new(true));
        let flag = alive.clone();
        let killer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(false, Ordering::SeqCst);
        });
        let (payload, status) = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(10)),
            &|| alive.load(Ordering::SeqCst),
        )
        .unwrap_err();
        killer.join().unwrap();
        assert_eq!(status, ExecutionResultStatus::Cancelled);
        assert!(payload.contains("tool_approval_cancelled"));
        assert!(registry.pending_for_session(&session).is_empty());
    }

    #[test]
    fn revoking_a_token_cancels_its_pending_approvals() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let canceller = {
            let registry = registry.clone();
            let session = session.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    if !registry.pending_for_session(&session).is_empty() {
                        cancel_external_approvals(&registry, &session, "tok1");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        let (_, status) = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(10)),
            &|| true,
        )
        .unwrap_err();
        canceller.join().unwrap();
        assert_eq!(status, ExecutionResultStatus::Cancelled);
    }

    #[test]
    fn already_dead_connection_never_creates_an_approval() {
        let bus = InMemoryEventBus::new(8);
        let registry = ToolApprovalRegistry::default();
        let session = SessionId::new("ext-session");
        let (_, status) = await_external_tool_approval(
            &bus,
            &registry,
            &request(&session, Duration::from_secs(1)),
            &|| false,
        )
        .unwrap_err();
        assert_eq!(status, ExecutionResultStatus::Cancelled);
        assert!(registry.pending_for_session(&session).is_empty());
    }
}

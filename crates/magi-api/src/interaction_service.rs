//! HTTP 与 App Server 共用的交互业务入口；持久决定仍由 SessionStore 拥有。
use crate::{errors::ApiError, state::ApiState};
use magi_conversation_runtime::{PendingToolApproval, ToolApprovalDecision, UserQuestionResponse};
use magi_core::{EventId, SessionId};
use magi_event_bus::{EventContext, EventEnvelope};
use serde_json::json;

pub(crate) fn pending_approvals(
    state: &ApiState,
    session_id: &SessionId,
) -> Result<Vec<PendingToolApproval>, ApiError> {
    let mut pending = state
        .session_store
        .pending_tool_approvals(session_id)
        .map_err(|error| ApiError::internal_assembly("读取工具审批失败", error))?;
    for request in state
        .conversation_registry
        .tool_approvals()
        .pending_for_session(session_id)
    {
        if request.task_id.as_str().starts_with("external:")
            && !pending
                .iter()
                .any(|pending| pending.approval_id == request.approval_id)
        {
            pending.push(request);
        }
    }
    pending.sort_by(|a, b| {
        a.requested_at
            .cmp(&b.requested_at)
            .then(a.approval_id.cmp(&b.approval_id))
    });
    Ok(pending)
}

pub(crate) fn resolve_approval(
    state: &ApiState,
    session_id: &SessionId,
    approval_id: &str,
    decision: ToolApprovalDecision,
) -> Result<magi_app_server_protocol::ResolveToolApprovalResult, ApiError> {
    if approval_id.is_empty() {
        return Err(ApiError::InvalidInput("approvalId 不能为空".to_string()));
    }
    let registry = state.conversation_registry.tool_approvals();
    let pending = if registry
        .pending_for_session(session_id)
        .iter()
        .any(|pending| pending.approval_id == approval_id)
    {
        registry.resolve(session_id, approval_id, decision)
    } else {
        // canonical 交互入口按原 Task/Turn 与当前拥有方核对恢复资格。
        // 已转交代理可以在主线关闭或归档后恢复，不能再要求主线 ready 指针。
        let pending = state
            .session_store
            .pending_tool_approvals(session_id)
            .map_err(|error| ApiError::internal_assembly("读取审批失败", error))?
            .into_iter()
            .find(|pending| pending.approval_id == approval_id)
            .ok_or_else(|| ApiError::Conflict("工具授权请求已过期、取消或已经处理".into()))?;
        state
            .session_store
            .resolve_tool_approvals(session_id, &[approval_id.into()], decision)
            .map(|_| pending)
            .map_err(|error| match error {
                magi_core::DomainError::Persistence { .. } => {
                    format!("工具审批持久化失败：{error}")
                }
                error => error.to_string(),
            })
    }
    .map_err(|message| {
        if message.starts_with("工具审批持久化失败") {
            ApiError::internal_assembly("提交工具审批失败", message)
        } else {
            ApiError::Conflict(message)
        }
    })?;
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-tool-approval-resolved"),
            "tool.approval.resolved",
            json!({
                "session_id": session_id,
                "task_id": pending.task_id,
                "turn_id": pending.turn_id,
                "tool_call_id": pending.tool_call_id,
                "tool_name": pending.tool_name,
                "approval_id": pending.approval_id,
                "decision": decision,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            task_id: Some(pending.task_id),
            ..EventContext::default()
        }),
    );
    use magi_app_server_protocol::generated::ToolApprovalDecision as WireDecision;
    let decision = match decision {
        ToolApprovalDecision::AllowOnce => WireDecision::AllowOnce,
        ToolApprovalDecision::AllowForTurn => WireDecision::AllowForTurn,
        ToolApprovalDecision::Deny => WireDecision::Deny,
    };
    Ok(magi_app_server_protocol::ResolveToolApprovalResult {
        session_id: session_id.to_string(),
        approval_id: approval_id.into(),
        decision,
        status: "resolved".into(),
    })
}

pub(crate) fn resolve_question(
    state: &ApiState,
    session_id: &SessionId,
    question_id: &str,
    response: UserQuestionResponse,
) -> Result<magi_app_server_protocol::ResolveUserQuestionResult, ApiError> {
    if question_id.is_empty() {
        return Err(ApiError::InvalidInput("questionId 不能为空".to_string()));
    }
    let outcome = match &response {
        UserQuestionResponse::Answered { .. } => "answered",
        UserQuestionResponse::Skipped => "skipped",
    };
    // 回答与题目对不上是调用方的输入错误；问题已经收口（轮次结束、已回答）才是冲突。
    let registry = state.conversation_registry.user_questions();
    let pending = if registry
        .pending_for_session(session_id)
        .iter()
        .any(|pending| pending.question_id == question_id)
    {
        registry.resolve(session_id, question_id, response)
    } else {
        // 与 pending 投影使用同一 canonical 准入；独立代理不依赖主线
        // 当前指针。持久答复只结算原交互，不启动 Runner 或新的模型请求。
        state
            .session_store
            .resolve_user_question(session_id, question_id, response)
            .map(|question| question.request)
            .map_err(|error| match error {
                magi_core::DomainError::Persistence { .. } => {
                    format!("提问答复持久化失败：{error}")
                }
                error => error.to_string(),
            })
    }
    .map_err(|message| {
        if message.starts_with("提问答复持久化失败") {
            ApiError::internal_assembly("提交提问答复失败", message)
        } else if message.contains("这个问题已经处理过") {
            ApiError::Conflict(message)
        } else {
            ApiError::InvalidInput(message)
        }
    })?;
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("event-user-question-resolved"),
            "user.question.resolved",
            json!({
                "session_id": session_id,
                "task_id": pending.task_id,
                "turn_id": pending.turn_id,
                "tool_call_id": pending.tool_call_id,
                "question_id": pending.question_id,
                "outcome": outcome,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            task_id: pending.task_id,
            ..EventContext::default()
        }),
    );
    Ok(magi_app_server_protocol::ResolveUserQuestionResult {
        session_id: session_id.to_string(),
        question_id: question_id.into(),
        status: "resolved".into(),
    })
}

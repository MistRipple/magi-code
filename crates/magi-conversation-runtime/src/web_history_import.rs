//! 把 ChatGPT Web 已保存对话里的一轮往返（用户消息 + 助手回复）导入为 Magi canonical 的已完成 Turn。
//!
//! 已保存模式的历史只沿 **Web → Magi** 方向同步（W2、W13）：Magi 只把缺失的可见消息
//! 增量写入 canonical，用于展示和切换本地模型后的上下文，绝不反向写回 Web。
//! 走与普通 Turn 相同的 Coordinator + canonical sink，不开第二条写入路径。

use std::collections::HashMap;

use magi_core::{SessionId, ThreadId, UtcMillis, WorkspaceId};
use magi_session_store::{
    ActiveExecutionTurn, ActiveExecutionTurnItem, SessionStore, TimelineEntryInput,
    TimelineEntryKind,
};

use crate::session_turn_coordinator::{
    CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus, ExecutionProfile,
    SessionTurnCoordinator, TurnAdmission,
};
use crate::session_writeback::{CanonicalTurnEventSink, append_session_turn_item_for_turn};
use crate::turn_contract::TurnCommand;

/// 一轮已同步的远端往返。
#[derive(Clone, Debug)]
pub struct RemoteWebExchange {
    pub user_text: String,
    pub assistant_text: String,
    /// 该往返里最后一条消息在 ChatGPT 侧的稳定 id（用于增量同步指针）。
    pub last_remote_id: Option<String>,
}

fn user_item(
    turn_id: &str,
    text: &str,
    attempt_id: &str,
    thread_id: ThreadId,
) -> ActiveExecutionTurnItem {
    ActiveExecutionTurnItem {
        item_id: format!("{turn_id}-user"),
        item_seq: 1,
        kind: "user_message".to_string(),
        status: "completed".to_string(),
        source: "user".to_string(),
        title: None,
        content: Some(text.to_string()),
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: None,
        tool_name: None,
        tool_status: None,
        tool_arguments: None,
        tool_result: None,
        tool_error: None,
        request_id: Some(format!("request-{turn_id}")),
        user_message_id: None,
        placeholder_message_id: None,
        metadata: HashMap::from([
            (
                "executionProfile".to_string(),
                serde_json::json!("conversation"),
            ),
            (
                "requestId".to_string(),
                serde_json::json!(format!("request-{turn_id}")),
            ),
            (
                "requestFingerprint".to_string(),
                serde_json::json!(format!("fingerprint-{turn_id}")),
            ),
            ("attemptId".to_string(), serde_json::json!(attempt_id)),
            // 标记这是从 ChatGPT Web 同步来的镜像，不是用户在 Magi 里发出的消息。
            ("webRemoteSynced".to_string(), serde_json::json!(true)),
        ]),
        timeline_entry_id: Some(format!("timeline-{turn_id}")),
        source_thread_id: thread_id,
    }
}

/// 把一轮远端往返写成已完成的 Turn。`turn_id` 必须唯一（调用方用远端消息 id 派生）。
pub fn import_remote_web_exchange(
    session_store: &SessionStore,
    coordinator: &SessionTurnCoordinator,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    turn_id: &str,
    turn_seq: u64,
    exchange: &RemoteWebExchange,
) -> Result<(), String> {
    let accepted_at = UtcMillis::now();
    let thread_id = session_store
        .ensure_session_mission(session_id, accepted_at, || {
            magi_core::MissionId::new(format!("mission-{session_id}"))
        })
        .1;
    let attempt = match coordinator
        .execute_command(
            session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.to_string(),
                request_id: format!("request-{turn_id}"),
                request_fingerprint: format!("fingerprint-{turn_id}"),
                profile: ExecutionProfile::Conversation,
            }),
        )
        .map_err(|error| format!("无法开始同步 Turn：{error:?}"))?
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => return Err(format!("同步 Turn 未被受理：{other:?}")),
    };
    let item = user_item(
        turn_id,
        &exchange.user_text,
        &attempt.attempt_id,
        thread_id.clone(),
    );
    let mut turn = ActiveExecutionTurn {
        turn_id: turn_id.to_string(),
        turn_seq,
        accepted_at,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: Some(exchange.user_text.clone()),
        items: vec![item],
    };
    turn.normalize();
    let sink = CanonicalTurnEventSink::for_store(session_store, None);
    sink.accept_conversation_turn_with_timeline_entry(
        session_id.clone(),
        workspace_id.cloned(),
        TimelineEntryInput::new(
            format!("timeline-{turn_id}"),
            TimelineEntryKind::UserMessage,
            &exchange.user_text,
            accepted_at,
        ),
        turn,
    )
    .map_err(|error| format!("同步 Turn 写入失败：{error}"))?;
    for status in [
        CoordinatorTurnStatus::Preparing,
        CoordinatorTurnStatus::Running,
    ] {
        coordinator
            .execute_command(
                session_id,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status,
                },
            )
            .map_err(|error| format!("同步 Turn 状态推进失败：{error:?}"))?;
    }
    let mut assistant = crate::session_writeback::session_turn_item(
        "assistant_final",
        "completed",
        None,
        Some(exchange.assistant_text.clone()),
        Some(format!("{turn_id}-assistant")),
        thread_id,
    );
    assistant.source = "orchestrator".to_string();
    assistant
        .metadata
        .insert("webRemoteSynced".to_string(), serde_json::json!(true));
    append_session_turn_item_for_turn(session_store, session_id, Some(turn_id), assistant, None)?;
    coordinator
        .execute_command(
            session_id,
            TurnCommand::Finish {
                attempt,
                status: CoordinatorTurnStatus::Completed,
            },
        )
        .map_err(|error| format!("同步 Turn 收口失败：{error:?}"))?;
    sink.set_status_domain(session_id, Some(turn_id), "completed")
        .map_err(|error| format!("同步 Turn 终态写入失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_exchange_becomes_a_completed_turn_with_both_messages() {
        let store = SessionStore::new();
        let coordinator = SessionTurnCoordinator::default();
        let session_id = SessionId::new("session-import");
        store
            .create_session(session_id.clone(), "imported")
            .expect("session should create");
        import_remote_web_exchange(
            &store,
            &coordinator,
            &session_id,
            None,
            "turn-web-import-1",
            1,
            &RemoteWebExchange {
                user_text: "你好".to_string(),
                assistant_text: "你好，有什么可以帮你？".to_string(),
                last_remote_id: Some("m2".to_string()),
            },
        )
        .expect("import should succeed");
        let turns = store.canonical_turns_for_session(&session_id);
        let turn = turns
            .iter()
            .find(|turn| turn.turn_id == "turn-web-import-1")
            .expect("turn");
        assert_eq!(
            turn.status,
            magi_session_store::CanonicalTurnStatus::Completed
        );
        let text: Vec<_> = turn
            .items
            .iter()
            .filter_map(|item| item.content.clone())
            .collect();
        assert!(text.iter().any(|t| t == "你好"));
        assert!(text.iter().any(|t| t.contains("有什么可以帮你")));
        // 导入不产生进行中的 Turn：之后用户可以正常在这个会话里发送消息。
        assert!(
            store
                .runtime_sidecar(&session_id)
                .and_then(|s| s.current_turn)
                .is_none_or(|t| {
                    !matches!(t.status.as_str(), "running" | "accepted" | "pending")
                })
        );
    }
}

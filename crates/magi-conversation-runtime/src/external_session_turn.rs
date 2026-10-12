//! 外部会话的活动段 Turn：外部 MCP 客户端没有“用户消息 → 模型回复”，但它在 Magi 里是一个真正的会话，
//! 调用必须作为普通工具项写进 canonical 时间线，和内部对话、GPT Web 同一套事实、同一套展示。
//!
//! canonical 的工具项必须挂在一个 Turn 上，所以把一段连续的外部活动收成一个 Turn：
//! 第一次调用打开 Turn（开场是一条系统提示，说明是哪个客户端），之后的调用都写进它，
//! 空闲一段时间或持续太久后收口为 completed，下一次调用再开新 Turn。
//! 走与普通 Turn 相同的 Coordinator + canonical sink（同 `web_history_import`），不开第二条写入路径。

use std::collections::HashMap;

use magi_core::{SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::{
    ActiveExecutionTurn, SessionStore, TimelineEntryInput, TimelineEntryKind,
};

use crate::session_turn_coordinator::{
    CoordinatorAdmission, CoordinatorCommandResult, CoordinatorTurnStatus, ExecutionProfile,
    SessionTurnCoordinator, TurnAdmission,
};
use crate::session_writeback::{
    CanonicalTurnEventSink, publish_current_session_turn_item_event, session_turn_item,
};
use crate::turn_contract::TurnCommand;

/// 外部会话 Turn 的 id 前缀；据此区分“活动段 Turn”与其他 Turn。
pub const EXTERNAL_TURN_ID_PREFIX: &str = "external-turn-";

fn opener_item_id(turn_id: &str) -> String {
    format!("{turn_id}-opener")
}

/// 打开一个活动段 Turn，开场是一条系统提示（`client_name` 是外部客户端的名称）。
#[allow(clippy::too_many_arguments)]
pub fn open_external_turn(
    session_store: &SessionStore,
    coordinator: &SessionTurnCoordinator,
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    client_name: &str,
    turn_id: &str,
    turn_seq: u64,
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
        .map_err(|error| format!("无法开始外部会话 Turn：{error:?}"))?
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => return Err(format!("外部会话 Turn 未被受理：{other:?}")),
    };
    let mut opener = session_turn_item(
        "assistant_phase",
        "completed",
        None,
        Some(format!("来自 {client_name} 的外部调用")),
        Some(opener_item_id(turn_id)),
        thread_id,
    );
    opener.source = "system".to_string();
    opener.metadata = HashMap::from([
        (
            "noticeKind".to_string(),
            serde_json::json!("external_client"),
        ),
        ("clientName".to_string(), serde_json::json!(client_name)),
    ]);
    opener.timeline_entry_id = Some(format!("timeline-{turn_id}"));
    let mut turn = ActiveExecutionTurn {
        turn_id: turn_id.to_string(),
        turn_seq,
        accepted_at,
        completed_at: None,
        status: "accepted".to_string(),
        user_message: None,
        items: vec![opener],
    };
    turn.normalize();
    CanonicalTurnEventSink::for_store(session_store, None)
        .accept_conversation_turn_with_timeline_entry(
            session_id.clone(),
            workspace_id.cloned(),
            TimelineEntryInput::new(
                format!("timeline-{turn_id}"),
                TimelineEntryKind::SystemNote,
                client_name,
                accepted_at,
            ),
            turn,
        )
        .map_err(|error| format!("外部会话 Turn 写入失败：{error}"))?;
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
            .map_err(|error| format!("外部会话 Turn 状态推进失败：{error:?}"))?;
    }
    CanonicalTurnEventSink::for_store(session_store, None)
        .set_status_domain(session_id, Some(turn_id), "running")
        .map_err(|error| format!("外部会话 Turn 运行状态写入失败：{error}"))?;
    let workspace = workspace_id.cloned();
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        &workspace,
        &opener_item_id(turn_id),
        None,
    )
}

/// 收口活动段 Turn 为 completed。Turn 已经不是当前 Turn（例如 daemon 重启后会话被收口过）时静默成功。
pub fn close_external_turn(
    session_store: &SessionStore,
    coordinator: &SessionTurnCoordinator,
    event_bus: &InMemoryEventBus,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    turn_id: &str,
) -> Result<(), String> {
    let is_current = session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| turn.turn_id == turn_id);
    if !is_current {
        return Ok(());
    }
    if let Ok(attempt) = coordinator.current_attempt(session_id, turn_id) {
        coordinator
            .execute_command(
                session_id,
                TurnCommand::Finish {
                    attempt,
                    status: CoordinatorTurnStatus::Completed,
                },
            )
            .map_err(|error| format!("外部会话 Turn 收口失败：{error:?}"))?;
    }
    CanonicalTurnEventSink::for_store(session_store, None)
        .set_status_domain(session_id, Some(turn_id), "completed")
        .map_err(|error| format!("外部会话 Turn 终态写入失败：{error}"))?;
    let workspace = workspace_id.cloned();
    publish_current_session_turn_item_event(
        event_bus,
        session_store,
        session_id,
        &workspace,
        &opener_item_id(turn_id),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_writeback::ExternalToolItemWriter;
    use magi_core::ExecutionResultStatus;
    use magi_session_store::CanonicalTurnStatus;

    #[test]
    fn external_calls_land_in_one_activity_turn_that_can_be_closed_and_reopened() {
        let store = SessionStore::new();
        let coordinator = SessionTurnCoordinator::default();
        let bus = InMemoryEventBus::new(32);
        let session_id = SessionId::new("external-tool-token-1");
        let workspace_id = WorkspaceId::new("ws-1");
        store
            .create_external_tool_session(session_id.clone(), "Cursor", workspace_id.to_string())
            .expect("external session");

        open_external_turn(
            &store,
            &coordinator,
            &bus,
            &session_id,
            Some(&workspace_id),
            "Cursor",
            "external-turn-1",
            1,
        )
        .expect("open");
        let workspace = Some(workspace_id.clone());
        let writer = ExternalToolItemWriter {
            session_store: &store,
            event_bus: &bus,
            session_id: &session_id,
            workspace_id: &workspace,
            turn_id: Some("external-turn-1"),
            origin: None,
        };
        writer
            .started("call-1", "file_read", "{\"path\":\"a.txt\"}")
            .unwrap();
        writer
            .finished(
                "call-1",
                "file_read",
                "{\"path\":\"a.txt\"}",
                "hello",
                ExecutionResultStatus::Succeeded,
            )
            .unwrap();
        writer
            .finished(
                "call-2",
                "file_read",
                "{\"path\":\"../x\"}",
                "路径超出工作区范围",
                ExecutionResultStatus::Rejected,
            )
            .unwrap();

        close_external_turn(
            &store,
            &coordinator,
            &bus,
            &session_id,
            Some(&workspace_id),
            "external-turn-1",
        )
        .expect("close");
        let turns = store.canonical_turns_for_session(&session_id);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, CanonicalTurnStatus::Completed);
        let tools = turns[0]
            .items
            .iter()
            .filter(|item| item.tool.is_some())
            .count();
        assert_eq!(tools, 2, "两次调用都在同一个 Turn 里");
        assert!(
            turns[0]
                .items
                .iter()
                .any(|item| item.content.as_deref() == Some("来自 Cursor 的外部调用")),
            "开场系统提示带客户端名称"
        );
        // 重复收口是无害的；之后可以再开新的活动段。
        close_external_turn(
            &store,
            &coordinator,
            &bus,
            &session_id,
            Some(&workspace_id),
            "external-turn-1",
        )
        .expect("close twice");
        open_external_turn(
            &store,
            &coordinator,
            &bus,
            &session_id,
            Some(&workspace_id),
            "Cursor",
            "external-turn-2",
            2,
        )
        .expect("reopen");
        assert_eq!(store.canonical_turns_for_session(&session_id).len(), 2);
    }
}

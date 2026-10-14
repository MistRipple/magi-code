//! 外部会话的活动段 Turn 管理：外部 MCP 客户端的调用按“活动段”写进它自己外部会话的 canonical 时间线。
//!
//! 第一次调用打开一个 Turn，之后的调用都写进它；没有在途调用且空闲一段时间，或持续太久，
//! 就收口为 completed，下一次调用再开新的。Turn 的打开与收口是 `magi-conversation-runtime::
//! external_session_turn` 的事，这里只负责“什么时候开、什么时候关”和在途调用计数。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use magi_conversation_runtime::external_session_turn::{
    EXTERNAL_TURN_ID_PREFIX, close_external_turn, open_external_turn,
};
use magi_conversation_runtime::session_turn_finalize::current_turn_status_is_terminal;
use magi_core::{EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::SessionKind;

use crate::state::ApiState;

/// 没有在途调用且空闲这么久，活动段收口。
const IDLE_CLOSE: Duration = Duration::from_secs(30);
/// 一个活动段最长持续时间（在途调用结束后才会收口）。
const MAX_TURN: Duration = Duration::from_secs(15 * 60);

struct OpenTurn {
    turn_id: String,
    workspace_id: WorkspaceId,
    opened_ms: u64,
    last_ms: u64,
    in_flight: usize,
}

pub(crate) struct ExternalTurns {
    open: Mutex<HashMap<SessionId, OpenTurn>>,
    maintenance_started: AtomicBool,
    idle: Duration,
    max: Duration,
}

fn now_ms() -> u64 {
    UtcMillis::now().0
}

impl ExternalTurns {
    pub(crate) fn new() -> Self {
        Self::with_limits(IDLE_CLOSE, MAX_TURN)
    }

    pub(crate) fn with_limits(idle: Duration, max: Duration) -> Self {
        Self {
            open: Mutex::new(HashMap::new()),
            maintenance_started: AtomicBool::new(false),
            idle,
            max,
        }
    }

    /// 一次外部调用开始：确保该会话有打开的活动段 Turn，返回它的 id，并把这次调用计为在途。
    /// 调用结束时必须配对调用 `end_call`。
    pub(crate) fn begin_call(
        &self,
        state: &ApiState,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        client_name: &str,
    ) -> Result<String, String> {
        let mut open = self.open.lock().expect("external turns poisoned");
        let now = now_ms();
        let current_turn = state
            .session_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.current_turn);
        if let Some(tracked) = open.get_mut(session_id) {
            let still_current = current_turn.as_ref().is_some_and(|turn| {
                turn.turn_id == tracked.turn_id && !current_turn_status_is_terminal(&turn.status)
            });
            let within_limit =
                tracked.in_flight > 0 || now.saturating_sub(tracked.opened_ms) < millis(self.max);
            if still_current && within_limit {
                tracked.in_flight += 1;
                tracked.last_ms = now;
                let turn_id = tracked.turn_id.clone();
                drop(open);
                publish_changed(state, session_id);
                return Ok(turn_id);
            }
            let stale = open.remove(session_id).expect("tracked turn exists");
            let _ = close_turn(state, session_id, Some(&stale.workspace_id), &stale.turn_id);
        } else if let Some(turn) = current_turn.as_ref()
            && !current_turn_status_is_terminal(&turn.status)
        {
            // 没有登记的未收口 Turn（例如 daemon 异常退出前留下的）：先收口再开新的。
            let _ = close_turn(state, session_id, Some(workspace_id), &turn.turn_id);
        }
        let seq = state
            .session_store
            .canonical_turns_for_session(session_id)
            .len() as u64
            + 1;
        let turn_id = format!("{EXTERNAL_TURN_ID_PREFIX}{now}-{seq}");
        open_external_turn(
            &state.session_store,
            state.conversation_registry.turn_coordinator(),
            &state.event_bus,
            session_id,
            Some(workspace_id),
            client_name,
            &turn_id,
            seq,
        )?;
        open.insert(
            session_id.clone(),
            OpenTurn {
                turn_id: turn_id.clone(),
                workspace_id: workspace_id.clone(),
                opened_ms: now,
                last_ms: now,
                in_flight: 1,
            },
        );
        drop(open);
        self.spawn_closer(state.clone(), session_id.clone(), turn_id.clone());
        publish_changed(state, session_id);
        Ok(turn_id)
    }

    /// 与 `begin_call` 配对：这次调用结束，开始计空闲时间。
    pub(crate) fn end_call(&self, state: &ApiState, session_id: &SessionId) {
        if let Some(tracked) = self
            .open
            .lock()
            .expect("external turns poisoned")
            .get_mut(session_id)
        {
            tracked.in_flight = tracked.in_flight.saturating_sub(1);
            tracked.last_ms = now_ms();
        }
        publish_changed(state, session_id);
    }

    /// 空闲或超时后收口活动段。没有 tokio 运行时（同步测试）时不启动，由下一次调用或关闭流程收口。
    fn spawn_closer(&self, state: ApiState, session_id: SessionId, turn_id: String) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let tick = (self.idle / 4).clamp(Duration::from_millis(20), Duration::from_secs(5));
        let (idle, max) = (millis(self.idle), millis(self.max));
        handle.spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let turns = &state.mcp_service.external_turns;
                let mut open = turns.open.lock().expect("external turns poisoned");
                let Some(tracked) = open.get(&session_id) else {
                    return;
                };
                if tracked.turn_id != turn_id {
                    return;
                }
                let now = now_ms();
                let idle_long_enough = now.saturating_sub(tracked.last_ms) >= idle;
                let too_long = now.saturating_sub(tracked.opened_ms) >= max;
                if tracked.in_flight == 0 && (idle_long_enough || too_long) {
                    let tracked = open.remove(&session_id).expect("tracked turn exists");
                    drop(open);
                    let _ = close_turn(&state, &session_id, Some(&tracked.workspace_id), &turn_id);
                    return;
                }
            }
        });
    }

    /// 这个外部会话里在途的调用数（含等待人工确认的）。
    pub(crate) fn in_flight(&self, session_id: &SessionId) -> usize {
        self.open
            .lock()
            .expect("external turns poisoned")
            .get(session_id)
            .map_or(0, |tracked| tracked.in_flight)
    }

    /// 清理保留期限之外的外部会话：`retention_days` 天内没有活动、也没有打开的活动段的会话整体删除
    /// （连同变更账本等会话资源）；之后该客户端再次调用会建立新的外部会话。返回清理的数量。
    pub(crate) async fn purge_expired(&self, state: &ApiState) -> usize {
        let days = u64::from(state.mcp_service.retention().retention_days);
        if days == 0 {
            return 0;
        }
        let cutoff = now_ms().saturating_sub(days.saturating_mul(86_400_000));
        let expired = state
            .session_store
            .sessions()
            .into_iter()
            .filter(|session| session.kind == SessionKind::ExternalTool)
            .filter(|session| session.updated_at.0 < cutoff)
            .filter(|session| self.in_flight(&session.session_id) == 0)
            .map(|session| session.session_id)
            .collect::<Vec<_>>();
        let mut purged = 0;
        for session_id in expired {
            match state.delete_session_and_resources(&session_id).await {
                Ok(()) => purged += 1,
                Err(error) => tracing::warn!(%session_id, ?error, "清理过期外部会话失败"),
            }
        }
        purged
    }

    /// 启动一次后台维护：收口遗留的活动段，并定期清理过期的外部会话。
    pub(crate) fn start_maintenance(&self, state: &ApiState) {
        if self.maintenance_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.maintenance_started.store(false, Ordering::SeqCst);
            return;
        };
        let state = state.clone();
        handle.spawn(async move {
            let turns = &state.mcp_service.external_turns;
            turns.settle_all(&state);
            loop {
                turns.purge_expired(&state).await;
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    /// 清理某令牌（缺省全部）的外部会话。返回清理的数量。
    pub(crate) async fn clear_sessions(&self, state: &ApiState, token_id: Option<&str>) -> usize {
        let targets = state
            .session_store
            .sessions()
            .into_iter()
            .filter(|session| session.kind == SessionKind::ExternalTool)
            .map(|session| session.session_id)
            .filter(|session_id| {
                token_id.is_none_or(|token_id| {
                    *session_id == crate::mcp_service::external_session_id(token_id)
                })
            })
            .collect::<Vec<_>>();
        let mut cleared = 0;
        for session_id in targets {
            self.open
                .lock()
                .expect("external turns poisoned")
                .remove(&session_id);
            match state.delete_session_and_resources(&session_id).await {
                Ok(()) => cleared += 1,
                Err(error) => tracing::warn!(%session_id, ?error, "清理外部会话失败"),
            }
        }
        cleared
    }

    /// 收口所有外部会话里未结束的活动段（daemon 启动时与关闭前）。返回收口的数量。
    pub(crate) fn settle_all(&self, state: &ApiState) -> usize {
        self.open.lock().expect("external turns poisoned").clear();
        let mut settled = 0;
        for session in state.session_store.sessions() {
            if session.kind != SessionKind::ExternalTool {
                continue;
            }
            let Some(turn) = state
                .session_store
                .runtime_sidecar(&session.session_id)
                .and_then(|sidecar| sidecar.current_turn)
            else {
                continue;
            };
            if current_turn_status_is_terminal(&turn.status) {
                continue;
            }
            let workspace_id = session.workspace_id.clone().map(WorkspaceId::new);
            if close_turn(
                state,
                &session.session_id,
                workspace_id.as_ref(),
                &turn.turn_id,
            )
            .is_ok()
            {
                settled += 1;
            }
        }
        settled
    }
}

/// 通知界面某个外部会话有变化（有新调用、调用结束）：侧栏据此刷新入口。
/// 事件对所有事件流可见（不管当前打开的是哪个工作区）；调用本身的内容走会话的 canonical 事件。
fn publish_changed(state: &ApiState, session_id: &SessionId) {
    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| session.workspace_id)
        .map(WorkspaceId::new);
    let _ = state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique("mcp-activity"),
            "mcp.activity",
            serde_json::json!({ "session_id": session_id.as_str() }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id,
            ..EventContext::default()
        }),
    );
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

fn close_turn(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    turn_id: &str,
) -> Result<(), String> {
    close_external_turn(
        &state.session_store,
        state.conversation_registry.turn_coordinator(),
        &state.event_bus,
        session_id,
        workspace_id,
        turn_id,
    )
}

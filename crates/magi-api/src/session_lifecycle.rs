//! 会话生命周期的领域动作：用户界面（HTTP 路由）与插件网关共用，路由只负责把结果组装成界面 DTO。
use crate::{
    ApiError, ApiState,
    scope_binding::{self, SessionScope},
    session_title,
};
use magi_core::{EventId, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use serde_json::json;

/// 新会话是否成为用户当前选中的会话。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreationSelection {
    /// 用户界面的新建：新会话成为当前会话。
    SelectAsCurrent,
    /// 其他发起方（插件网关）：创建会话，当前会话保持不变，是否打开由用户决定。
    LeaveCurrent,
}

/// 创建一个空的用户会话并提交：记录、持久化、`session.created` 事件在同一流程里收口，
/// 持久化失败时回滚新会话。
///
/// 调用方必须持有导航锁（`guard` 是这一点的证明）：创建与回滚都会读写当前会话指针。
pub(crate) async fn create_session(
    state: &ApiState,
    _navigation_guard: &tokio::sync::MutexGuard<'_, ()>,
    scope: &SessionScope,
    selection: CreationSelection,
) -> Result<(SessionId, Option<WorkspaceId>), ApiError> {
    let workspace_id = scope.workspace_id();
    let previous_current_session_id = state.session_store.current_session_id();
    let session_id = scope_binding::new_session_id();
    match selection {
        CreationSelection::SelectAsCurrent => scope_binding::create_session_in_scope(
            state,
            &session_id,
            session_title::NEW_SESSION_PLACEHOLDER_TITLE,
            workspace_id.as_ref(),
            UtcMillis::now(),
            None,
        )?,
        CreationSelection::LeaveCurrent => scope_binding::create_unselected_session_in_scope(
            state,
            &session_id,
            session_title::NEW_SESSION_PLACEHOLDER_TITLE,
            workspace_id.as_ref(),
            UtcMillis::now(),
        )?,
    }
    // 新建会话要用完整 projection 提交这次实体创建，让旧 projection、sidecar 和 current
    // 在同一个持久化事务中收口；导航专用写入只适合已有实体，不能把新实体的回滚留在旧文件之外。
    if let Err(error) =
        state.persist_session_projection_for_sessions_for_api(std::slice::from_ref(&session_id))
    {
        let original_message = error.message().to_string();
        // 没有选中新会话时当前指针从未改动，删除一个非当前会话不能带替代指针。
        let restore_current = match selection {
            CreationSelection::SelectAsCurrent => previous_current_session_id,
            CreationSelection::LeaveCurrent => None,
        };
        return Err(
            match state
                .rollback_created_session_after_navigation_lock(&session_id, restore_current)
                .await
            {
                Ok(()) => error,
                Err(rollback_error) => ApiError::internal_assembly(
                    "创建会话失败且回滚失败",
                    format!("{original_message}；回滚失败: {rollback_error:?}"),
                ),
            },
        );
    }
    publish_session_directory_event(state, "session.created", &session_id, workspace_id.as_ref());
    Ok((session_id, workspace_id))
}

pub(crate) fn publish_session_directory_event(
    state: &ApiState,
    event_type: &str,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
) {
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::unique(format!("event-{event_type}-{session_id}")),
            event_type,
            json!({
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.map(WorkspaceId::to_string),
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.cloned(),
            ..EventContext::default()
        }),
    );
}

/// 重命名会话：标题事件进日志、记录与 projection 更新、`session.title.updated` 事件，
/// 用户界面（HTTP 路由）与插件网关共用，路由只负责把结果组装成界面 DTO。
///
/// 调用方必须持有该会话的生命周期锁并已解析好作用域（`workspace_id` 来自作用域）。
/// 只有标题实际变化才向界面发布更新事件；返回的 `event_seq` 对应本次请求被接受的那条日志事件。
pub(crate) fn rename_session(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<WorkspaceId>,
    title: &str,
) -> Result<magi_session_store::SessionTitleCommit, ApiError> {
    let current = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let outcome = state.rename_session_with_persistence_for_api(session_id, title);
    // 标题事件进日志即成立：即使随后 projection 写入失败（错误照常返回），内存标题已变，
    // 界面与插件也必须得到通知，否则它们停留在旧标题。
    let now = state
        .session_store
        .session(session_id)
        .map(|record| record.title)
        .unwrap_or_else(|| current.title.clone());
    if current.title != now {
        session_title::publish_session_title_updated(state, session_id, workspace_id, &now);
    }
    let commit = outcome?;
    Ok(commit)
}

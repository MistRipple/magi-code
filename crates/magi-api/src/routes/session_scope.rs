use crate::{errors::ApiError, state::ApiState};
use magi_core::{SessionId, WorkspaceId};
use magi_session_store::SessionRecord;

pub(super) fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
}

pub(super) fn require_session_record_in_workspace(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: Option<&str>,
) -> Result<SessionRecord, ApiError> {
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    require_session_workspace_match(&session, requested_workspace_id)?;
    Ok(session)
}

pub(super) fn require_current_session_record_in_workspace(
    state: &ApiState,
    requested_workspace_id: Option<&str>,
) -> Result<SessionRecord, ApiError> {
    let session = state
        .session_store
        .current_session()
        .ok_or_else(|| ApiError::InvalidInput("当前没有活动 session".to_string()))?;
    require_session_workspace_match(&session, requested_workspace_id)?;
    Ok(session)
}

pub(super) fn session_workspace_id(
    state: &ApiState,
    session: &SessionRecord,
) -> Option<WorkspaceId> {
    session
        .workspace_id
        .as_deref()
        .map(WorkspaceId::new)
        .or_else(|| {
            state
                .session_store
                .execution_ownership(&session.session_id)
                .and_then(|ownership| ownership.workspace_id)
        })
}

pub(super) fn resolve_session_workspace_binding(
    state: &ApiState,
    session: &SessionRecord,
    requested_workspace_id: Option<&WorkspaceId>,
) -> Result<Option<WorkspaceId>, ApiError> {
    let bound_workspace_id = session_workspace_id(state, session);

    if let (Some(requested_workspace_id), Some(bound_workspace_id)) =
        (requested_workspace_id, bound_workspace_id.as_ref())
        && requested_workspace_id != bound_workspace_id
    {
        return Err(session_workspace_mismatch(
            &session.session_id,
            requested_workspace_id.as_str(),
        ));
    }

    Ok(requested_workspace_id.cloned().or(bound_workspace_id))
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn require_session_workspace_match(
    session: &SessionRecord,
    requested_workspace_id: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(requested_workspace_id) = trimmed_non_empty(requested_workspace_id)
        && session.workspace_id.as_deref() != Some(requested_workspace_id)
    {
        return Err(session_workspace_mismatch(
            &session.session_id,
            requested_workspace_id,
        ));
    }
    Ok(())
}

fn session_workspace_mismatch(session_id: &SessionId, workspace_id: &str) -> ApiError {
    ApiError::InvalidInput(format!(
        "会话 {} 不属于 workspace {}",
        session_id, workspace_id
    ))
}

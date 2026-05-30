use crate::{errors::ApiError, state::ApiState};
use magi_core::{SessionId, WorkspaceId};
use magi_session_store::SessionRecord;

#[derive(Clone, Debug)]
pub(super) struct SessionWorkspaceScope {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub workspace_path: String,
}

pub(super) fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
}

pub(super) fn require_workspace_id(value: Option<&str>) -> Result<WorkspaceId, ApiError> {
    value
        .map(str::trim)
        .filter(|workspace_id| !workspace_id.is_empty())
        .map(WorkspaceId::new)
        .ok_or_else(|| ApiError::InvalidInput("workspaceId 不能为空".to_string()))
}

pub(super) fn require_registered_workspace_id(
    state: &ApiState,
    value: Option<&str>,
) -> Result<WorkspaceId, ApiError> {
    let workspace_id = require_workspace_id(value)?;
    if state
        .workspace_root_path(&Some(workspace_id.clone()))
        .is_none()
    {
        return Err(ApiError::not_found(
            "workspace 不存在",
            workspace_id.as_str(),
        ));
    }
    Ok(workspace_id)
}

pub(super) fn require_session_workspace_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    requested_workspace_id: Option<&str>,
    action: &str,
) -> Result<SessionWorkspaceScope, ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let requested_workspace_id = require_workspace_id(requested_workspace_id)?;
    let session = state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let bound_workspace_id = state
        .session_store
        .execution_ownership(&session_id)
        .and_then(|ownership| ownership.workspace_id)
        .or_else(|| session_workspace_id(state, &session))
        .ok_or_else(|| ApiError::InvalidInput(format!("当前会话未绑定 workspace，不能{action}")))?;
    if bound_workspace_id != requested_workspace_id {
        return Err(session_workspace_mismatch(
            &session_id,
            requested_workspace_id.as_str(),
        ));
    }
    let workspace_path = state
        .workspace_root_path(&Some(bound_workspace_id.clone()))
        .ok_or_else(|| ApiError::not_found("workspace 不存在", bound_workspace_id.as_str()))?
        .to_string_lossy()
        .to_string();
    Ok(SessionWorkspaceScope {
        session_id,
        workspace_id: bound_workspace_id,
        workspace_path,
    })
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
    require_session_workspace_match(state, &session, requested_workspace_id)?;
    Ok(session)
}

pub(super) fn session_workspace_id(
    state: &ApiState,
    session: &SessionRecord,
) -> Option<WorkspaceId> {
    state.session_workspace_id(session)
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
    state: &ApiState,
    session: &SessionRecord,
    requested_workspace_id: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(requested_workspace_id) = trimmed_non_empty(requested_workspace_id)
        && session_workspace_id(state, session)
            .as_ref()
            .map(|workspace_id| workspace_id.as_str())
            != Some(requested_workspace_id)
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

use crate::{dto::SessionScopeKindDto, errors::ApiError, state::ApiState};
use magi_core::{SessionId, WorkspaceId};
use magi_session_store::SessionRecord;
use magi_tool_runtime::ToolExecutionContext;
use std::path::PathBuf;

/// 已验证的会话请求作用域。
///
/// 它把 session 与 Personal | Workspace 绑定为同一个事实交给任务类路由，
/// 从类型上禁止个人会话被错误地压缩为一个伪造的 workspace。
#[derive(Clone, Debug)]
pub(super) struct SessionRequestScope {
    pub session_id: SessionId,
    pub scope: SessionScope,
}

impl SessionRequestScope {
    pub(super) fn workspace_id(&self) -> Option<WorkspaceId> {
        self.scope.workspace_id()
    }

    pub(super) fn workspace_path(&self) -> Option<String> {
        match &self.scope {
            SessionScope::Personal => None,
            SessionScope::Workspace(binding) => Some(binding.workspace_path.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RegisteredWorkspaceBinding {
    pub workspace_id: WorkspaceId,
    pub workspace_path: String,
}

/// 会话的唯一作用域：要么绑定一个已注册项目，要么是 Magi 自己管理执行目录的个人会话。
///
/// 个人会话绝不伪装成 workspace；调用方必须显式消费这个枚举，不能把 `None` 当作
/// “缺参数”再去补默认项目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionScope {
    Personal,
    Workspace(RegisteredWorkspaceBinding),
}

impl SessionScope {
    pub(crate) fn workspace_id(&self) -> Option<WorkspaceId> {
        match self {
            Self::Personal => None,
            Self::Workspace(binding) => Some(binding.workspace_id.clone()),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct OptionalSessionWorkspaceScope {
    session_id: Option<SessionId>,
    workspace_id: Option<WorkspaceId>,
    workspace_path: Option<PathBuf>,
}

impl OptionalSessionWorkspaceScope {
    pub(super) fn scope_kind(&self) -> SessionScopeKindDto {
        if self.workspace_id.is_some() {
            SessionScopeKindDto::Workspace
        } else {
            SessionScopeKindDto::Personal
        }
    }

    pub(super) fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub(super) fn workspace_id(&self) -> Option<&WorkspaceId> {
        self.workspace_id.as_ref()
    }

    pub(super) fn tool_context(&self) -> ToolExecutionContext {
        ToolExecutionContext {
            session_id: self.session_id.clone(),
            workspace_id: self.workspace_id.clone(),
            working_directory: self.workspace_path.clone(),
            ..ToolExecutionContext::default()
        }
    }

    pub(super) fn workspace_id_string(&self) -> Option<String> {
        self.workspace_id.as_ref().map(ToString::to_string)
    }

    pub(super) fn workspace_path_string(&self) -> Option<String> {
        self.workspace_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
    }
}

pub(super) fn parse_session_id(value: Option<&str>) -> Result<SessionId, ApiError> {
    let session_id = value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("sessionId 不能为空".to_string()))?;
    Ok(SessionId::new(session_id))
}

pub(super) fn registered_workspace_path(
    state: &ApiState,
    workspace_id: &WorkspaceId,
) -> Result<String, ApiError> {
    state
        .workspace_root_path(&Some(workspace_id.clone()))
        .map(|path| path.to_string_lossy().to_string())
        .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))
}

pub(super) fn require_registered_workspace_binding(
    state: &ApiState,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<RegisteredWorkspaceBinding, ApiError> {
    let requested_workspace_id = trimmed_non_empty(requested_workspace_id);
    let requested_workspace_path = trimmed_non_empty(requested_workspace_path);
    if requested_workspace_id.is_none() && requested_workspace_path.is_none() {
        return Err(ApiError::InvalidInput("workspaceId 不能为空".to_string()));
    }
    let workspace_id = state
        .resolve_workspace_id_from_request(
            requested_workspace_id.map(WorkspaceId::new),
            requested_workspace_path,
        )
        .ok_or_else(|| {
            ApiError::not_found(
                "workspace 不存在",
                requested_workspace_id
                    .or(requested_workspace_path)
                    .unwrap_or_default(),
            )
        })?;
    let workspace_path = registered_workspace_path(state, &workspace_id)?;
    Ok(RegisteredWorkspaceBinding {
        workspace_id,
        workspace_path,
    })
}

pub(super) fn resolve_session_scope(
    state: &ApiState,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<SessionScope, ApiError> {
    let requested_workspace_id = trimmed_non_empty(requested_workspace_id);
    let requested_workspace_path = trimmed_non_empty(requested_workspace_path);
    if requested_workspace_id.is_none() && requested_workspace_path.is_none() {
        return Ok(SessionScope::Personal);
    }
    require_registered_workspace_binding(state, requested_workspace_id, requested_workspace_path)
        .map(SessionScope::Workspace)
}

/// 为已存在会话解析请求作用域。
///
/// 未附带项目绑定的新请求属于个人作用域；但已存在的项目会话不能因为调用方
/// 漏掉 binding 而被重解释为个人会话。会话记录是唯一归属事实，不猜测当前项目。
pub(super) fn resolve_existing_session_scope(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<SessionScope, ApiError> {
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let has_workspace_binding = trimmed_non_empty(requested_workspace_id).is_some()
        || trimmed_non_empty(requested_workspace_path).is_some();
    if !has_workspace_binding && session_workspace_id(state, &session).is_some() {
        return Err(ApiError::InvalidInput(
            "workspaceId 不能为空：项目会话必须提供 workspaceId 或 workspacePath".to_string(),
        ));
    }
    let scope = resolve_session_scope(state, requested_workspace_id, requested_workspace_path)?;
    require_session_record_in_scope(state, session_id, &scope)?;
    Ok(scope)
}

/// 解析由 HTTP 合同显式声明的会话作用域。
///
/// `scope` 不是缺省值：个人和项目请求都必须声明自己的边界，避免空 workspace
/// 在后续重构中再次被解释成“猜一个当前项目”。
pub(super) fn resolve_explicit_session_scope(
    state: &ApiState,
    requested_scope: SessionScopeKindDto,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<SessionScope, ApiError> {
    match requested_scope {
        SessionScopeKindDto::Personal => {
            if trimmed_non_empty(requested_workspace_id).is_some()
                || trimmed_non_empty(requested_workspace_path).is_some()
            {
                return Err(ApiError::InvalidInput(
                    "个人会话不能携带 workspace 绑定".to_string(),
                ));
            }
            Ok(SessionScope::Personal)
        }
        SessionScopeKindDto::Workspace => require_registered_workspace_binding(
            state,
            requested_workspace_id,
            requested_workspace_path,
        )
        .map(SessionScope::Workspace),
    }
}

/// 验证由 HTTP 请求明确声明的会话归属。
pub(super) fn require_session_request_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    requested_scope: SessionScopeKindDto,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<SessionRequestScope, ApiError> {
    let session_id = parse_session_id(session_id_value)?;
    let scope = resolve_explicit_session_scope(
        state,
        requested_scope,
        requested_workspace_id,
        requested_workspace_path,
    )?;
    require_session_record_in_scope(state, &session_id, &scope)?;
    Ok(SessionRequestScope { session_id, scope })
}

pub(super) fn require_session_record_in_scope(
    state: &ApiState,
    session_id: &SessionId,
    scope: &SessionScope,
) -> Result<SessionRecord, ApiError> {
    let session = state
        .session_store
        .session(session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let expected_workspace_id = scope.workspace_id();
    if session_workspace_id(state, &session) != expected_workspace_id {
        return match expected_workspace_id {
            Some(workspace_id) => Err(session_workspace_mismatch(
                session_id,
                workspace_id.as_str(),
            )),
            None => Err(ApiError::InvalidInput(format!(
                "会话 {} 不属于个人会话",
                session_id
            ))),
        };
    }
    Ok(session)
}

pub(super) fn resolve_optional_session_workspace_scope(
    state: &ApiState,
    session_id_value: Option<&str>,
    requested_workspace_id_value: Option<&str>,
    requested_workspace_path_value: Option<&str>,
) -> Result<OptionalSessionWorkspaceScope, ApiError> {
    let requested_workspace_path = trimmed_non_empty(requested_workspace_path_value);
    let requested_workspace_id = state.resolve_workspace_id_from_request(
        trimmed_non_empty(requested_workspace_id_value).map(WorkspaceId::new),
        requested_workspace_path,
    );
    let session_id = trimmed_non_empty(session_id_value).map(SessionId::new);
    let workspace_id = match session_id.as_ref() {
        Some(session_id) => {
            let session = state
                .session_store
                .session(session_id)
                .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
            resolve_session_workspace_binding(state, &session, requested_workspace_id.as_ref())?
        }
        None => requested_workspace_id,
    };
    let workspace_path = workspace_id
        .as_ref()
        .and_then(|workspace_id| state.workspace_root_path(&Some(workspace_id.clone())));

    Ok(OptionalSessionWorkspaceScope {
        session_id,
        workspace_id,
        workspace_path,
    })
}

pub(super) fn resolve_optional_explicit_session_scope(
    state: &ApiState,
    requested_scope: SessionScopeKindDto,
    session_id_value: Option<&str>,
    requested_workspace_id_value: Option<&str>,
    requested_workspace_path_value: Option<&str>,
) -> Result<OptionalSessionWorkspaceScope, ApiError> {
    let resolved_scope = resolve_explicit_session_scope(
        state,
        requested_scope,
        requested_workspace_id_value,
        requested_workspace_path_value,
    )?;
    let session_id = trimmed_non_empty(session_id_value).map(SessionId::new);
    if let Some(session_id) = session_id.as_ref() {
        require_session_record_in_scope(state, session_id, &resolved_scope)?;
    }
    let (workspace_id, workspace_path) = match resolved_scope {
        SessionScope::Personal => (None, None),
        SessionScope::Workspace(binding) => (
            Some(binding.workspace_id),
            Some(PathBuf::from(binding.workspace_path)),
        ),
    };
    Ok(OptionalSessionWorkspaceScope {
        session_id,
        workspace_id,
        workspace_path,
    })
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

fn session_workspace_mismatch(session_id: &SessionId, workspace_id: &str) -> ApiError {
    ApiError::InvalidInput(format!(
        "会话 {} 不属于 workspace {}",
        session_id, workspace_id
    ))
}

//! 插件会话控制端口：共享宿主的读写与查询请求进入 daemon 服务层的唯一入口。
//!
//! 端口不实现业务，只把请求交给 App Server 与 HTTP 路由共用的服务函数（创建不抢占当前选择、
//! 轮次提交、停止、队列管理、重命名与标题日志事件、终端与工作树控制等），
//! 并用会话自己的归属解析作用域，调用方不能指定工作区。
use crate::{ApiError, ApiState, turn_service};
use magi_core::SessionId;
use std::sync::Arc;

/// 在 daemon 装配 `ApiState` 之后创建，交给插件平台。
pub fn plugin_session_control(state: &ApiState) -> Arc<PluginSessionControl> {
    Arc::new(PluginSessionControl {
        state: state.clone(),
    })
}

pub struct PluginSessionControl {
    state: ApiState,
}

/// 页面代用户发起的一条输入（宿主已从 DSH 的内容块转换）。
#[derive(Debug)]
pub struct PluginPrompt {
    /// 业务请求身份，同一身份重复提交按轮次提交的回放语义处理，不重复接纳。
    pub request_id: String,
    /// 是否作为引导追加到当前进行中的轮次；否则按普通输入提交（空闲即开始，进行中即排队）。
    pub steer: bool,
    pub text: String,
    pub images: Vec<PluginPromptImage>,
}

/// 对一个尚未被领取的排队输入的修改。排队输入的身份是队列自己的 `queueId`。
#[derive(Debug)]
pub enum PluginQueueAction {
    /// 只替换文本，图片、引用、命令、目标模式、访问档位等沿用原输入。
    Edit {
        text: String,
    },
    Remove,
    /// 把输入补充到当前进行中的轮次（引导）。
    Steer,
}

#[derive(Debug)]
pub struct PluginPromptImage {
    pub name: Option<String>,
    pub media_type: String,
    /// 标准 base64（不含 `data:` 前缀）。
    pub data: String,
}

impl PluginSessionControl {
    /// 创建一个空的用户会话，不改写用户当前选中的会话（是否打开由用户决定）。
    ///
    /// 作用域只由 `workspace_id` 决定：给出已注册的工作区即为项目会话，否则为个人会话；
    /// 创建目录总是该作用域的目录，调用方不能另指目录或预分配会话身份。
    pub async fn create(&self, workspace_id: Option<&str>) -> Result<SessionId, ApiError> {
        let navigation_guard = self.state.lock_session_navigation().await;
        let scope = match workspace_id {
            Some(workspace_id) => crate::scope_binding::resolve_explicit_session_scope(
                &self.state,
                crate::dto::SessionScopeKindDto::Workspace,
                Some(workspace_id),
                None,
            )?,
            None => crate::scope_binding::resolve_explicit_session_scope(
                &self.state,
                crate::dto::SessionScopeKindDto::Personal,
                None,
                None,
            )?,
        };
        let (session_id, _) = crate::session_lifecycle::create_session(
            &self.state,
            &navigation_guard,
            &scope,
            crate::session_lifecycle::CreationSelection::LeaveCurrent,
        )
        .await?;
        Ok(session_id)
    }

    /// 以用户的名义向会话提交一条输入，走与界面、App Server 相同的 `TurnService::submit`。
    ///
    /// 作用域取自会话记录；引导绑定提交时观察到的进行中轮次，没有进行中的轮次则失败，
    /// 不把引导降级成普通输入。
    pub async fn prompt(
        &self,
        session_id: &SessionId,
        prompt: PluginPrompt,
    ) -> Result<(), ApiError> {
        let request_id = prompt.request_id.trim().to_owned();
        if request_id.is_empty() {
            return Err(ApiError::InvalidInput("输入需要非空的 requestId".into()));
        }
        let record = self
            .state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let expected_turn_id = if prompt.steer {
            let sidecar = self.state.session_store.runtime_sidecar(session_id);
            let turn = sidecar
                .as_ref()
                .and_then(|sidecar| sidecar.current_turn.as_ref())
                .filter(|turn| crate::session_activity::current_turn_status_is_active(&turn.status))
                .ok_or_else(|| ApiError::Conflict("会话没有进行中的轮次，无法引导".into()))?;
            Some(turn.turn_id.clone())
        } else {
            None
        };
        let images: Vec<serde_json::Value> = prompt
            .images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                serde_json::json!({
                    "name": image.name.clone().unwrap_or_else(|| format!("image-{}", index + 1)),
                    "dataUrl": format!("data:{};base64,{}", image.media_type, image.data),
                })
            })
            .collect();
        let request: crate::dto::SessionTurnRequestDto =
            serde_json::from_value(serde_json::json!({
                "sessionId": session_id.as_str(),
                "scope": if record.workspace_id.is_some() { "workspace" } else { "personal" },
                "workspaceId": record.workspace_id,
                "text": prompt.text,
                "images": images,
                "requestId": request_id,
                "steerCurrentTurn": prompt.steer,
                "expectedTurnId": expected_turn_id,
            }))
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        // 同一业务请求串行提交，重复提交才能按回放语义识别。
        let _request_lock = self.state.lock_app_server_request(&request_id).await;
        crate::turn_service::TurnService::new(self.state.clone())
            .submit(request)
            .await
            .map(|_| ())
    }

    /// 修改会话里一个排队输入，走界面与 App Server 共用的 `input_service`（提交锁、修订校验、回放语义不变）。
    pub async fn update_queue(
        &self,
        session_id: &SessionId,
        queue_id: &str,
        action: PluginQueueAction,
    ) -> Result<(), ApiError> {
        use crate::input_service as queue;
        use serde_json::json;
        let record = self
            .state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let scope = |extra: serde_json::Value| {
            let mut value = json!({
                "sessionId": session_id.as_str(),
                "workspaceId": record.workspace_id,
            });
            value
                .as_object_mut()
                .expect("object")
                .extend(extra.as_object().cloned().unwrap_or_default());
            value
        };
        let invalid = |error: serde_json::Error| ApiError::InvalidInput(error.to_string());
        let snapshot = queue::get_session_turn_queue(
            self.state.clone(),
            serde_json::from_value(scope(json!({}))).map_err(invalid)?,
        )
        .await?;
        let item = snapshot
            .queued_turns
            .iter()
            .find(|item| item.queue_id == queue_id)
            .ok_or_else(|| ApiError::not_found("排队输入不存在", queue_id))?;
        let revision = item.revision;
        match action {
            PluginQueueAction::Remove => {
                let request = serde_json::from_value(scope(json!({
                    "queueId": queue_id, "expectedRevision": revision,
                })))
                .map_err(invalid)?;
                queue::remove_session_turn_queue_item(self.state.clone(), request).await?;
            }
            PluginQueueAction::Steer => {
                let sidecar = self.state.session_store.runtime_sidecar(session_id);
                let turn = sidecar
                    .as_ref()
                    .and_then(|sidecar| sidecar.current_turn.as_ref())
                    .filter(|turn| {
                        crate::session_activity::current_turn_status_is_active(&turn.status)
                    })
                    .ok_or_else(|| ApiError::Conflict("会话没有进行中的轮次，无法引导".into()))?;
                let request = serde_json::from_value(scope(json!({
                    "queueId": queue_id, "expectedRevision": revision,
                    "expectedTurnId": turn.turn_id,
                })))
                .map_err(invalid)?;
                queue::guide_session_turn_queue_item(self.state.clone(), request).await?;
            }
            PluginQueueAction::Edit { text } => {
                if text.trim().is_empty() {
                    return Err(ApiError::InvalidInput("编辑后的输入不能为空".into()));
                }
                // 修改接口替换整条输入：文本之外的内容从原输入原样带回。
                let original = serde_json::to_value(item).map_err(invalid)?;
                let mut replacement = json!({
                    "scope": if record.workspace_id.is_some() { "workspace" } else { "personal" },
                    "sessionId": session_id.as_str(),
                    "workspaceId": record.workspace_id,
                    "text": text,
                });
                for key in [
                    "workspacePath",
                    "sourceFolderId",
                    "skillName",
                    "command",
                    "pluginCommandId",
                    "goalMode",
                    "accessProfile",
                    "images",
                    "contextReferences",
                    "browserAnnotationRefs",
                    "browserNodeSelections",
                ] {
                    if let Some(value) = original.get(key).filter(|value| !value.is_null()) {
                        replacement[key] = value.clone();
                    }
                }
                let request = serde_json::from_value(scope(json!({
                    "queueId": queue_id, "expectedRevision": revision, "request": replacement,
                })))
                .map_err(invalid)?;
                queue::update_session_turn_queue_item(self.state.clone(), request).await?;
            }
        }
        Ok(())
    }

    /// 重命名用户会话，返回被接受的标题与它在会话事件日志里的序号。
    /// 与界面的重命名共用同一个领域动作；没有事件日志（序号缺失）时拒绝，不用别的水位代替。
    pub async fn rename(
        &self,
        session_id: &SessionId,
        title: &str,
    ) -> Result<(String, u64), ApiError> {
        let _lifecycle_guard = self.state.lock_session_lifecycle(session_id).await;
        let record = self
            .state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        // 没有事件日志就没有序号可返回：先拒绝，不能改完再报错。
        if !self.state.session_store.has_session_event_writer() {
            return Err(ApiError::internal_assembly(
                "重命名会话失败",
                "会话事件日志不可用，标题没有序号",
            ));
        }
        let workspace_id = self.state.session_workspace_id(&record);
        let commit =
            crate::session_lifecycle::rename_session(&self.state, session_id, workspace_id, title)?;
        let seq = commit.event_seq.ok_or_else(|| {
            ApiError::internal_assembly("重命名会话失败", "会话事件日志不可用，标题没有序号")
        })?;
        Ok((commit.session.title, seq))
    }

    /// 停止会话当前的轮次。只作用于用户会话；会话没有进行中的轮次时与界面的停止一样是无害的空操作。
    pub async fn cancel(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let record = self
            .state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        turn_service::interrupt_session_turn(
            &self.state,
            session_id,
            record.workspace_id.as_deref(),
            None,
        )
        .await
        .map(|_| ())
    }

    /// 把会话级命令生命周期事件（`command/run`、`command/done`）写入事件日志，返回会话内事件序号。
    /// 只作用于用户会话；没有事件日志时明确失败。
    pub async fn append_command_event(
        &self,
        session_id: &SessionId,
        event: magi_session_store::SessionCommandEvent,
    ) -> Result<u64, ApiError> {
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        if !self.state.session_store.has_session_event_writer() {
            return Err(ApiError::internal_assembly(
                "记录命令事件失败",
                "会话事件日志不可用",
            ));
        }
        let seq = self
            .state
            .session_store
            .record_session_command_event(session_id, event)
            .map_err(|error| match error {
                magi_core::DomainError::NotFound { .. } => {
                    ApiError::session_not_found(session_id.as_str())
                }
                magi_core::DomainError::Validation { message }
                | magi_core::DomainError::InvalidState { message } => {
                    ApiError::InvalidInput(message)
                }
                other => ApiError::internal_assembly("记录命令事件失败", other),
            })?
            .ok_or_else(|| ApiError::internal_assembly("记录命令事件失败", "会话事件日志不可用"))?;
        Ok(seq)
    }

    /// 切换会话（及其活动目标）的访问档位，走 daemon 唯一事实源。
    pub async fn set_access_profile(
        &self,
        session_id: &SessionId,
        access_profile: magi_core::AccessProfile,
    ) -> Result<magi_core::AccessProfile, ApiError> {
        let _guard = self.state.lock_session_turn_commit(session_id).await;
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        if self
            .state
            .session_store
            .current_unfinished_goal(session_id)
            .is_some()
        {
            self.state
                .session_store
                .set_active_goal_access_profile(session_id, access_profile)
                .map_err(|error| ApiError::Conflict(error.to_string()))?;
            self.state
                .persist_session_state_checkpoint("plugin_access_profile_updated")?;
        }
        Ok(access_profile)
    }

    /// 列出用户会话的全部目标记录。
    pub async fn list_goals(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<magi_session_store::SessionGoal>, ApiError> {
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        Ok(self.state.session_store.goals_for_session(session_id))
    }

    /// 按 goalId 读取用户会话的单个目标。
    pub async fn get_goal(
        &self,
        session_id: &SessionId,
        goal_id: &str,
    ) -> Result<Option<magi_session_store::SessionGoal>, ApiError> {
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        Ok(self
            .state
            .session_store
            .goals_for_session(session_id)
            .into_iter()
            .find(|goal| goal.goal_id.as_str() == goal_id))
    }

    /// 为用户会话创建目标，复用 daemon 唯一的 SessionStore::create_goal 与持久化检查点。
    pub async fn create_goal(
        &self,
        session_id: &SessionId,
        objective: &str,
        access_profile: magi_core::AccessProfile,
    ) -> Result<magi_session_store::SessionGoal, ApiError> {
        let _guard = self.state.lock_session_turn_commit(session_id).await;
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let (_, thread_id) = self.state.session_store.ensure_session_mission(
            session_id,
            magi_core::UtcMillis::now(),
            || {
                magi_core::MissionId::new(format!(
                    "mission-{}-{}",
                    session_id,
                    magi_core::UtcMillis::now().0
                ))
            },
        );
        let turn_id = format!("plugin-goal-{}", magi_core::UtcMillis::now().0);
        let goal = self
            .state
            .session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                objective,
                access_profile,
            )
            .map_err(|error| match error {
                magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
                other => ApiError::Conflict(other.to_string()),
            })?;
        self.state
            .persist_session_state_checkpoint("plugin_goal_created")?;
        Ok(goal)
    }

    /// 更新用户会话的目标正文。
    pub async fn update_goal(
        &self,
        session_id: &SessionId,
        goal_id: &str,
        objective: &str,
    ) -> Result<magi_session_store::SessionGoal, ApiError> {
        let _guard = self.state.lock_session_turn_commit(session_id).await;
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let goal = self
            .state
            .session_store
            .update_goal_objective_if_revision(
                session_id,
                &magi_core::GoalId::new(goal_id),
                objective,
                None,
            )
            .map_err(|error| match error {
                magi_core::DomainError::Validation { message } => ApiError::InvalidInput(message),
                other => ApiError::Conflict(other.to_string()),
            })?;
        self.state
            .persist_session_state_checkpoint("plugin_goal_updated")?;
        Ok(goal)
    }

    /// 删除用户会话的指定目标及其绑定计划。
    pub async fn delete_goal(&self, session_id: &SessionId, goal_id: &str) -> Result<(), ApiError> {
        let _guard = self.state.lock_session_turn_commit(session_id).await;
        self.state
            .session_store
            .session(session_id)
            .filter(|record| record.kind.is_user())
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let current = self
            .state
            .session_store
            .goals_for_session(session_id)
            .into_iter()
            .find(|goal| goal.goal_id.as_str() == goal_id)
            .ok_or_else(|| ApiError::Conflict(format!("目标不存在：{goal_id}")))?;
        self.state
            .session_store
            .clear_goal_with_plan(session_id, &current.goal_id, current.control_revision, None)
            .map_err(|error| ApiError::Conflict(error.to_string()))?;
        self.state
            .persist_session_state_checkpoint("plugin_goal_cleared")?;
        Ok(())
    }

    /// 读取 daemon 权威 Git/Worktree 绑定上下文（与会话代码上下文同一事实源）。
    pub async fn list_worktrees(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
        let contexts = match session_id {
            Some(id) => self.state.session_code_contexts.for_session(id.as_str()),
            None => self.state.session_code_contexts.all(),
        };
        let mut out = Vec::new();
        for ctx in contexts {
            if let Ok(val) = serde_json::to_value(&ctx) {
                out.push(val);
            }
        }
        Ok(out)
    }

    /// 读取 Magi 原生 MCP 配置与当前在线连接状态（只读，不含明文凭据）。
    pub async fn list_mcp_servers(&self) -> Result<Vec<serde_json::Value>, ApiError> {
        let configured = self.state.settings_store.get_section("mcpServers");
        let connected: std::collections::BTreeSet<String> = self
            .state
            .mcp_connections()
            .read()
            .expect("mcp connections lock")
            .keys()
            .cloned()
            .collect();
        let mut out = Vec::new();
        if let Some(items) = configured.as_array() {
            for item in items {
                let name = item
                    .get("name")
                    .or_else(|| item.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let transport = item
                    .get("transport")
                    .or_else(|| item.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("stdio");
                let enabled = item
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                out.push(serde_json::json!({
                    "name": name,
                    "transport": transport,
                    "enabled": enabled,
                    "connected": connected.contains(name),
                    "source": "magi-daemon",
                }));
            }
        } else if let Some(obj) = configured.as_object() {
            for (name, item) in obj {
                let transport = item
                    .get("transport")
                    .or_else(|| item.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("stdio");
                let enabled = item
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                out.push(serde_json::json!({
                    "name": name,
                    "transport": transport,
                    "enabled": enabled,
                    "connected": connected.contains(name),
                    "source": "magi-daemon",
                }));
            }
        }
        Ok(out)
    }

    /// 读取 daemon 权威 `BrowserAuthority` 快照（逻辑 Tab 与宿主连接状态）。
    pub async fn browser_snapshot(&self) -> Result<serde_json::Value, ApiError> {
        let authority = self
            .state
            .browser_authority
            .lock()
            .expect("browser authority lock")
            .snapshot();
        let serialized = serde_json::to_value(&authority)
            .map_err(|error| ApiError::internal_assembly("序列化浏览器快照失败", error))?;
        Ok(serde_json::json!({
            "available": true,
            "authority": serialized,
        }))
    }

    /// 列出 Magi 原生 PTY 终端实例（可按会话过滤）。
    pub async fn terminal_list(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
        let sessions = self.state.terminal_sessions.list(session_id);
        Ok(sessions
            .into_iter()
            .map(|term| terminal_session_summary(&term))
            .collect())
    }

    /// 创建或打开受控 Magi 原生 PTY 终端（直接复用 `TerminalSessionManager` 唯一拥有者）。
    pub async fn terminal_create(
        &self,
        session_id: &SessionId,
        terminal_tab_id: &str,
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<serde_json::Value, ApiError> {
        let tab_id = terminal_tab_id.trim();
        if tab_id.is_empty() || tab_id.len() > 128 {
            return Err(ApiError::InvalidInput("terminalTabId 无效".into()));
        }
        let session = self
            .state
            .session_store
            .session(session_id)
            .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
        let execution_root = cwd
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                session
                    .initial_working_directory_ref
                    .as_deref()
                    .filter(|p| !p.trim().is_empty())
                    .map(std::path::PathBuf::from)
            })
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });
        let binding = crate::terminal_runtime::TerminalBinding {
            terminal_tab_id: tab_id.to_string(),
            workspace_id: session.workspace_id.map(|id| id.as_str().to_string()),
            source_folder_id: None,
            execution_root,
            session_id: session_id.as_str().to_string(),
        };
        let size = portable_pty::PtySize {
            cols: cols.clamp(2, 512),
            rows: rows.clamp(2, 256),
            pixel_width: 0,
            pixel_height: 0,
        };
        let term = self
            .state
            .terminal_sessions
            .open_or_create(binding, size)
            .map_err(|error| ApiError::Conflict(error.to_string()))?;
        Ok(terminal_session_summary(&term))
    }

    /// 向 Magi 原生 PTY 终端写入输入。
    pub async fn terminal_write(
        &self,
        terminal_tab_id: &str,
        data: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let term = self
            .state
            .terminal_sessions
            .get(terminal_tab_id)
            .ok_or_else(|| ApiError::NotFound(format!("终端不存在: {terminal_tab_id}")))?;
        term.write_input(data.as_bytes())
            .map_err(|error| ApiError::Conflict(error.to_string()))?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// 调整 Magi 原生 PTY 终端尺寸。
    pub async fn terminal_resize(
        &self,
        terminal_tab_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<serde_json::Value, ApiError> {
        let term = self
            .state
            .terminal_sessions
            .get(terminal_tab_id)
            .ok_or_else(|| ApiError::NotFound(format!("终端不存在: {terminal_tab_id}")))?;
        term.resize(portable_pty::PtySize {
            cols: cols.clamp(2, 512),
            rows: rows.clamp(2, 256),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// 读取 Magi 原生 PTY 终端当前滚动快照与生命周期状态。
    pub async fn terminal_read(
        &self,
        terminal_tab_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let term = self
            .state
            .terminal_sessions
            .get(terminal_tab_id)
            .ok_or_else(|| ApiError::NotFound(format!("终端不存在: {terminal_tab_id}")))?;
        let snap = term.snapshot();
        Ok(serde_json::json!({
            "id": term.binding().terminal_tab_id,
            "sessionId": term.binding().session_id,
            "sequence": snap.sequence,
            "output": String::from_utf8_lossy(&snap.output),
            "lifecycle": terminal_lifecycle_json(&snap.lifecycle),
        }))
    }

    /// 关闭 Magi 原生 PTY 终端。
    pub async fn terminal_close(
        &self,
        terminal_tab_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let closed = self.state.terminal_sessions.close_by_id(terminal_tab_id);
        Ok(serde_json::json!({ "closed": closed }))
    }

    /// 经原生 `magi-git::GitService` 创建 Git 工作树。
    pub async fn worktree_create(
        &self,
        repo_dir: &str,
        path: &str,
        branch: Option<&str>,
        base: Option<&str>,
        detached: bool,
    ) -> Result<serde_json::Value, ApiError> {
        let repo = std::path::PathBuf::from(repo_dir.trim());
        let target = std::path::PathBuf::from(path.trim());
        if repo.as_os_str().is_empty() || target.as_os_str().is_empty() {
            return Err(ApiError::InvalidInput("repoDir 和 path 不能为空".into()));
        }
        let branch_opt = branch
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string);
        let options = magi_git::WorktreeCreateOptions {
            path: target,
            base: base
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .unwrap_or("HEAD")
                .to_string(),
            create_branch: !detached && branch_opt.is_some(),
            branch: if detached { None } else { branch_opt },
            detached,
            precondition: magi_git::GitPrecondition::default(),
        };
        let service = magi_git::GitService::new();
        let entry = service
            .worktree_create(&repo, options)
            .await
            .map_err(|error| ApiError::Conflict(format!("创建 Git worktree 失败：{error}")))?;
        serde_json::to_value(&entry)
            .map_err(|error| ApiError::internal_assembly("序列化 worktree 失败", error))
    }

    /// 经原生 `magi-git::GitService` 移除 Git 工作树。
    pub async fn worktree_remove(
        &self,
        repo_dir: &str,
        path: &str,
        force: bool,
    ) -> Result<serde_json::Value, ApiError> {
        let repo = std::path::PathBuf::from(repo_dir.trim());
        let target = std::path::PathBuf::from(path.trim());
        if repo.as_os_str().is_empty() || target.as_os_str().is_empty() {
            return Err(ApiError::InvalidInput("repoDir 和 path 不能为空".into()));
        }
        let options = magi_git::WorktreeRemoveOptions {
            path: target,
            force,
            confirm_force: force,
            precondition: magi_git::GitPrecondition::default(),
        };
        let service = magi_git::GitService::new();
        let remaining = service
            .worktree_remove(&repo, options)
            .await
            .map_err(|error| ApiError::Conflict(format!("移除 Git worktree 失败：{error}")))?;
        let serialized = serde_json::to_value(&remaining)
            .map_err(|error| ApiError::internal_assembly("序列化 worktree 列表失败", error))?;
        Ok(serde_json::json!({
            "removed": true,
            "worktrees": serialized,
        }))
    }

    /// 读取可派发子代理角色的模型绑定只读投影（供 `ctx.subagentModelSelection`）。
    pub async fn subagent_model_selection(&self) -> Result<serde_json::Value, ApiError> {
        let raw_roles = self.state.settings_store.get_section("agentRoles");
        let spawnable: Vec<serde_json::Value> = self
            .state
            .agent_role_registry
            .all()
            .into_iter()
            .filter(|role| {
                self.state
                    .agent_role_registry
                    .is_spawnable_agent_role(&role.id)
            })
            .map(|role| {
                serde_json::json!({
                    "roleId": role.id,
                    "displayName": role.display_name,
                    "description": role.description,
                })
            })
            .collect();
        Ok(serde_json::json!({
            "roles": spawnable,
            "bindings": raw_roles,
        }))
    }

    /// 读取已配置/已连接 MCP 服务器的资源投影（供 `ctx.mcpResources`）。
    pub async fn mcp_resources(&self) -> Result<Vec<serde_json::Value>, ApiError> {
        let servers = self.list_mcp_servers().await?;
        let mut out = Vec::new();
        for server in servers {
            let name = server
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            out.push(serde_json::json!({
                "server": name,
                "uri": format!("mcp://{name}"),
                "connected": server.get("connected").and_then(serde_json::Value::as_bool).unwrap_or(false),
                "transport": server.get("transport").and_then(serde_json::Value::as_str).unwrap_or("stdio"),
            }));
        }
        Ok(out)
    }
}

fn terminal_lifecycle_json(
    lifecycle: &crate::terminal_runtime::TerminalLifecycle,
) -> serde_json::Value {
    match lifecycle {
        crate::terminal_runtime::TerminalLifecycle::Running => {
            serde_json::json!({ "status": "running" })
        }
        crate::terminal_runtime::TerminalLifecycle::Exited { exit_code, signal } => {
            serde_json::json!({ "status": "exited", "exitCode": exit_code, "signal": signal })
        }
        crate::terminal_runtime::TerminalLifecycle::Failed { message } => {
            serde_json::json!({ "status": "failed", "message": message })
        }
        crate::terminal_runtime::TerminalLifecycle::Closed => {
            serde_json::json!({ "status": "closed" })
        }
    }
}

fn terminal_session_summary(term: &crate::terminal_runtime::TerminalSession) -> serde_json::Value {
    let binding = term.binding();
    let snap = term.snapshot();
    serde_json::json!({
        "id": binding.terminal_tab_id,
        "sessionId": binding.session_id,
        "workspaceId": binding.workspace_id,
        "cwd": binding.execution_root.to_string_lossy(),
        "sequence": snap.sequence,
        "lifecycle": terminal_lifecycle_json(&snap.lifecycle),
        "backend": "magi-pty",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::SessionStore;
    use magi_workspace::WorkspaceStore;

    fn state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::new(TaskStore::new()))
    }

    /// 只记录标题事件的内存日志：其余事务不属于这些测试。
    #[derive(Default)]
    struct TitleLog(std::sync::Mutex<Vec<(SessionId, String)>>);

    impl magi_session_store::SessionEventWriter for TitleLog {
        fn append_model_attempt_stream(
            &self,

            _: &magi_core::SessionId,

            _: &str,

            _: u64,

            _: magi_session_store::ModelAttemptStream,
        ) -> magi_core::DomainResult<u64> {
            Err(magi_core::DomainError::Persistence {
                message: "attempt streams are not part of this test".into(),
            })
        }
        fn append_model_request(
            &self,
            _: &magi_core::SessionId,
            _: &str,
            _: u64,
            _: magi_session_store::ModelRequestSnapshot,
        ) -> magi_core::DomainResult<Option<u64>> {
            Err(magi_core::DomainError::Persistence {
                message: "model requests are not part of this test".into(),
            })
        }
        fn append_session_transaction(
            &self,
            _: &SessionId,
            _: &[magi_session_store::CanonicalTurnMutation],
            _: &[magi_session_store::SessionInputMutation],
            _: Option<(
                &magi_session_store::SessionAcceptanceRecord,
                Option<&magi_core::Task>,
            )>,
        ) -> magi_core::DomainResult<()> {
            Ok(())
        }

        fn append_session_title(
            &self,
            session_id: &SessionId,
            title: &str,
        ) -> magi_core::DomainResult<u64> {
            let mut log = self.0.lock().unwrap();
            log.push((session_id.clone(), title.to_string()));
            Ok(10 + log.len() as u64)
        }
    }

    #[tokio::test]
    async fn rename_returns_the_title_event_sequence_and_announces_the_change() {
        let state = state();
        let log = Arc::new(TitleLog::default());
        state
            .session_store
            .install_session_event_writer(log.clone());
        let session_id = SessionId::new("renamed");
        state
            .session_store
            .create_session(session_id.clone(), "旧标题")
            .unwrap();
        let control = plugin_session_control(&state);

        let (title, seq) = control.rename(&session_id, "  新标题 ").await.unwrap();
        assert_eq!((title.as_str(), seq), ("新标题", 11));
        assert_eq!(
            state.session_store.session(&session_id).unwrap().title,
            "新标题"
        );
        assert_eq!(
            *log.0.lock().unwrap(),
            vec![(session_id.clone(), "新标题".to_string())]
        );
        // 同一标题再次提交：新的序号，日志里多一条。
        let (_, again) = control.rename(&session_id, "新标题").await.unwrap();
        assert_eq!(again, 12);
    }

    #[tokio::test]
    async fn rename_refuses_unknown_sessions_invalid_titles_and_a_missing_log() {
        let state = state();
        let session_id = SessionId::new("rename-refusals");
        state
            .session_store
            .create_session(session_id.clone(), "旧标题")
            .unwrap();
        let control = plugin_session_control(&state);

        // 没有事件日志时没有序号可返回：拒绝，标题保持原样之外不做别的（记录仍按存储语义更新）。
        let error = control.rename(&session_id, "新标题").await.unwrap_err();
        assert!(error.message().contains("没有序号"), "{error:?}");
        // 拒绝发生在改动之前：标题和事件都没有变。
        assert_eq!(
            state.session_store.session(&session_id).unwrap().title,
            "旧标题"
        );

        state
            .session_store
            .install_session_event_writer(Arc::new(TitleLog::default()));
        assert!(matches!(
            control.rename(&SessionId::new("nope"), "x").await,
            Err(ApiError::SessionNotFound(_))
        ));
        for invalid in ["", "   ", "第一行\n第二行"] {
            assert!(
                matches!(
                    control.rename(&session_id, invalid).await,
                    Err(ApiError::InvalidInput(_)) | Err(ApiError::InternalAssemblyError(_))
                ),
                "{invalid:?}"
            );
        }
    }

    #[tokio::test]
    async fn cancel_resolves_the_scope_of_a_personal_session_and_is_harmless_when_idle() {
        let state = state();
        let session_id = SessionId::new("personal");
        state
            .session_store
            .create_session(session_id.clone(), "个人会话")
            .unwrap();
        let control = plugin_session_control(&state);
        control
            .cancel(&session_id)
            .await
            .expect("空闲会话的停止是无害的");
    }

    #[tokio::test]
    async fn cancel_takes_the_workspace_scope_from_the_session_not_from_the_caller() {
        let state = state();
        let root =
            std::env::temp_dir().join(format!("plugin-session-control-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace_id = magi_core::WorkspaceId::new("w-control");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                magi_core::AbsolutePath::new(root.display().to_string()),
            )
            .unwrap();
        let session_id = SessionId::new("in-workspace");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "项目会话",
                Some(workspace_id.to_string()),
            )
            .unwrap();
        plugin_session_control(&state)
            .cancel(&session_id)
            .await
            .expect("项目会话的作用域由会话记录解析");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn create_makes_an_unselected_session_and_announces_it() {
        let state = state();
        let shown = SessionId::new("shown");
        state
            .session_store
            .create_session(shown.clone(), "用户正在看")
            .unwrap();
        let mut events = state.event_bus.subscribe();
        let control = plugin_session_control(&state);

        let created = control.create(None).await.expect("个人会话可以创建");
        let record = state.session_store.session(&created).expect("会话已提交");
        assert!(record.kind.is_user());
        assert_eq!(record.workspace_id, None);
        assert_eq!(
            state.session_store.current_session_id(),
            Some(shown),
            "用户当前选中的会话没有被抢占"
        );
        let mut announced = false;
        while let Ok(event) = events.try_recv() {
            announced |= event.event_type == "session.created"
                && event.session_id.as_ref() == Some(&created);
        }
        assert!(announced, "创建要发布 session.created，宿主据此更新目录");
    }

    #[tokio::test]
    async fn create_in_a_workspace_uses_the_registered_workspace_and_refuses_unknown_ones() {
        let state = state();
        let root =
            std::env::temp_dir().join(format!("plugin-session-create-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace_id = magi_core::WorkspaceId::new("w-create");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                magi_core::AbsolutePath::new(root.display().to_string()),
            )
            .unwrap();
        let control = plugin_session_control(&state);
        let created = control.create(Some("w-create")).await.unwrap();
        let record = state.session_store.session(&created).unwrap();
        assert_eq!(record.workspace_id.as_deref(), Some("w-create"));
        assert!(
            record.initial_working_directory_ref.is_some(),
            "创建目录取自项目"
        );
        assert!(control.create(Some("unknown")).await.is_err());
        assert_eq!(state.session_store.sessions().len(), 1, "失败不留下会话");
        let _ = std::fs::remove_dir_all(root);
    }

    fn prompt(request_id: &str, text: &str, steer: bool) -> PluginPrompt {
        PluginPrompt {
            request_id: request_id.into(),
            steer,
            text: text.into(),
            images: vec![],
        }
    }

    #[tokio::test]
    async fn prompt_submits_user_input_through_the_turn_service_and_replays_by_request_id() {
        let state = state();
        let session_id = SessionId::new("prompted");
        state
            .session_store
            .create_session(session_id.clone(), "提示")
            .unwrap();
        let control = plugin_session_control(&state);

        control
            .prompt(&session_id, prompt("req-1", "你好", false))
            .await
            .expect("普通输入进入轮次提交");
        let turns = state.session_store.canonical_turns_for_session(&session_id);
        assert_eq!(turns.len(), 1);
        let user = turns[0]
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.kind,
                    magi_session_store::CanonicalTurnItemKind::UserMessage
                )
            })
            .expect("用户消息已提交");
        assert_eq!(user.content.as_deref(), Some("你好"));
        // 同一 requestId 的重复提交按回放处理，不再接纳第二轮。
        control
            .prompt(&session_id, prompt("req-1", "你好", false))
            .await
            .expect("回放仍是成功");
        assert_eq!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .len(),
            1
        );
        // 此 fixture 没有 dispatcher：等待后台失败收口，不留下悬挂的执行。
        let turn_id = turns[0].turn_id.clone();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state
                .turn_coordinator()
                .current_attempt(&session_id, &turn_id)
                .is_ok()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("后台执行应当收口");
    }

    #[tokio::test]
    async fn prompt_refuses_missing_identity_unknown_sessions_and_steering_an_idle_session() {
        let state = state();
        let session_id = SessionId::new("idle");
        state
            .session_store
            .create_session(session_id.clone(), "空闲")
            .unwrap();
        let control = plugin_session_control(&state);
        assert!(
            control
                .prompt(&session_id, prompt("  ", "x", false))
                .await
                .is_err()
        );
        assert!(
            control
                .prompt(&SessionId::new("missing"), prompt("r", "x", false))
                .await
                .is_err()
        );
        let error = control
            .prompt(&session_id, prompt("r", "引导", true))
            .await
            .expect_err("没有进行中的轮次不能引导");
        assert!(format!("{error:?}").contains("引导"), "{error:?}");
        assert!(
            state
                .session_store
                .canonical_turns_for_session(&session_id)
                .is_empty(),
            "失败的引导不留下轮次"
        );
    }

    fn queued_turn(
        session_id: &SessionId,
        workspace_id: &magi_core::WorkspaceId,
        queue_id: &str,
    ) -> crate::state::QueuedRegularSessionTurn {
        let request: crate::dto::SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "scope": "workspace", "sessionId": session_id.as_str(), "workspaceId": workspace_id.as_str(),
            "text": format!("queued {queue_id}"), "goalMode": true,
            "images": [{"name": "keep.png", "dataUrl": "data:image/png;base64,AA=="}],
            "requestId": format!("request-{queue_id}"),
            "userMessageId": format!("user-{queue_id}"),
            "placeholderMessageId": format!("assistant-{queue_id}"),
        }))
        .unwrap();
        crate::state::QueuedRegularSessionTurn {
            plugin_message: None,
            project_directories: None,
            request,
            request_fingerprint: None,
            requested_workspace_id: Some(workspace_id.clone()),
            accepted_at: magi_core::UtcMillis(100),
            route: crate::dto::SessionTurnRouteDto::Chat,
            task_title: None,
            execution_goal: None,
            required_tool_chain: Vec::new(),
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            session_id: session_id.clone(),
            workspace_id: Some(workspace_id.clone()),
            queue_id: queue_id.to_string(),
            retry_count: 0,
        }
    }

    fn queued_fixture() -> (ApiState, SessionId, std::path::PathBuf) {
        let state = state();
        let root = std::env::temp_dir().join(format!(
            "plugin-session-queue-{}-{}",
            std::process::id(),
            magi_core::UtcMillis::now().0
        ));
        std::fs::create_dir_all(&root).unwrap();
        let workspace_id = magi_core::WorkspaceId::new("w-queue");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                magi_core::AbsolutePath::new(root.display().to_string()),
            )
            .unwrap();
        let session_id = SessionId::new("queued-session");
        state
            .session_store
            .create_session_for_workspace(session_id.clone(), "队列", Some("w-queue".into()))
            .unwrap();
        state
            .enqueue_regular_session_turn(queued_turn(&session_id, &workspace_id, "first"))
            .unwrap();
        state
            .enqueue_regular_session_turn(queued_turn(&session_id, &workspace_id, "second"))
            .unwrap();
        (state, session_id, root)
    }

    #[tokio::test]
    async fn queue_edit_replaces_only_the_text_and_remove_takes_the_item_out() {
        let (state, session_id, root) = queued_fixture();
        let control = plugin_session_control(&state);
        let before = state
            .session_store
            .input_for_id(&session_id, "first")
            .unwrap();

        control
            .update_queue(
                &session_id,
                "first",
                PluginQueueAction::Edit {
                    text: "改过的文本".into(),
                },
            )
            .await
            .expect("编辑排队输入");
        let queue = crate::input_service::session_turn_queue_response(&state, &session_id);
        let first = queue
            .queued_turns
            .iter()
            .find(|item| item.queue_id == "first")
            .unwrap();
        assert_eq!(first.text.as_deref(), Some("改过的文本"));
        assert_eq!(first.images[0].name, "keep.png", "图片沿用原输入");
        assert!(first.goal_mode, "目标模式沿用原输入");
        assert_eq!(first.queue_position, 1, "身份与先后顺序不变");
        let after = state
            .session_store
            .input_for_id(&session_id, "first")
            .unwrap();
        assert_eq!(after.revision, before.revision + 1);

        control
            .update_queue(&session_id, "first", PluginQueueAction::Remove)
            .await
            .expect("移除排队输入");
        let queue = crate::input_service::session_turn_queue_response(&state, &session_id);
        assert_eq!(
            queue
                .queued_turns
                .iter()
                .map(|item| item.queue_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn queue_changes_refuse_unknown_items_empty_edits_and_steering_without_a_turn() {
        let (state, session_id, root) = queued_fixture();
        let control = plugin_session_control(&state);
        assert!(
            control
                .update_queue(&session_id, "nope", PluginQueueAction::Remove)
                .await
                .is_err()
        );
        assert!(
            control
                .update_queue(
                    &session_id,
                    "first",
                    PluginQueueAction::Edit { text: "  ".into() }
                )
                .await
                .is_err()
        );
        let error = control
            .update_queue(&session_id, "first", PluginQueueAction::Steer)
            .await
            .expect_err("没有进行中的轮次不能引导");
        assert!(format!("{error:?}").contains("引导"), "{error:?}");
        assert!(
            control
                .update_queue(
                    &SessionId::new("missing"),
                    "first",
                    PluginQueueAction::Remove
                )
                .await
                .is_err()
        );
        let queue = crate::input_service::session_turn_queue_response(&state, &session_id);
        assert_eq!(queue.queued_turns.len(), 2, "失败的修改不改变队列");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancel_refuses_unknown_sessions() {
        let control = plugin_session_control(&state());
        let error = control
            .cancel(&SessionId::new("missing"))
            .await
            .expect_err("未知会话必须失败");
        assert!(format!("{error:?}").contains("missing"), "{error:?}");
    }
}

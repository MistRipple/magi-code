use super::config::DaemonError;
use magi_event_bus::AuditUsageLedgerSnapshot;
use magi_knowledge_store::KnowledgeState;
use magi_session_store::{SessionDurableState, SessionExecutionSidecarStoreState, SessionStore};
use magi_worker_runtime::{WorkerRuntime, WorkerRuntimeDurableSnapshot};
use magi_workspace::{WorkspaceDurableState, WorkspaceRecoverySidecarStoreState, WorkspaceStore};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::warn;

#[derive(Clone, Debug)]
pub(crate) struct StateRepository {
    state_root: PathBuf,
}

impl StateRepository {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self { state_root }
    }

    pub(crate) fn session_durable_state_path(&self) -> PathBuf {
        self.state_root.join("sessions.json")
    }

    pub(crate) fn load_session_durable_state(&self) -> Result<SessionDurableState, DaemonError> {
        self.read_session_durable_state_or_default(self.session_durable_state_path())
    }

    /// 从指定工作区的 .magi/sessions.json 加载会话
    pub(crate) fn load_workspace_session_state(
        &self,
        workspace_root: &Path,
    ) -> Result<SessionDurableState, DaemonError> {
        let path = workspace_root.join(".magi").join("sessions.json");
        if path.exists() {
            self.read_session_durable_state_or_default(path)
        } else {
            Ok(SessionDurableState::default())
        }
    }

    fn load_global_session_state(&self) -> Result<SessionDurableState, DaemonError> {
        let path = self.session_durable_state_path();
        if !path.exists() {
            return Ok(SessionDurableState::default());
        }

        let durable = self.load_session_durable_state()?;
        let (mut global_state, workspace_states) = durable.partition_by_workspace();
        let rejected_workspace_session_count: usize = workspace_states
            .values()
            .map(|state| state.sessions.len())
            .sum();
        if rejected_workspace_session_count > 0 {
            global_state.clear_current_session_if_owned_by_workspace_states(&workspace_states);
            warn!(
                rejected_workspace_session_count,
                "清理全局 sessions.json 中错误归属的 workspace 会话；workspace 会话必须只从工作区 .magi/sessions.json 加载"
            );
            if global_state.is_empty() {
                let _ = fs::remove_file(&path);
            } else {
                self.save_session_durable_state(&global_state)?;
            }
        }
        Ok(global_state)
    }

    /// 遍历所有工作区加载会话，合并为统一的 SessionDurableState。
    /// 全局 sessions.json 只承载未绑定 workspace 的会话；workspace 绑定会话必须归属到
    /// 对应工作区的 .magi/sessions.json。
    pub(crate) fn load_sessions_from_workspaces(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<SessionDurableState, DaemonError> {
        // 从全局未绑定会话 + 各工作区 .magi/sessions.json 合并加载
        let mut merged = self.load_global_session_state()?;
        for (_, root_path) in workspace_roots {
            let ws_state = self.load_workspace_session_state(root_path)?;
            merged.append_state(ws_state);
        }
        Ok(merged)
    }

    /// 保存会话到指定工作区的 .magi/sessions.json
    pub(crate) fn save_workspace_session_state(
        &self,
        workspace_root: &Path,
        state: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        let path = workspace_root.join(".magi").join("sessions.json");
        self.write_json_atomically(path, state)
    }

    pub(crate) fn save_session_durable_state(
        &self,
        state: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.session_durable_state_path(), state)
    }

    pub(crate) fn session_sidecars_path(&self) -> PathBuf {
        self.state_root.join("session-sidecars.json")
    }

    pub(crate) fn load_session_sidecars(
        &self,
    ) -> Result<SessionExecutionSidecarStoreState, DaemonError> {
        self.read_json_or_default(self.session_sidecars_path())
    }

    pub(crate) fn save_session_sidecars(
        &self,
        state: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.session_sidecars_path(), state)
    }

    pub(crate) fn load_workspace_durable_state(
        &self,
    ) -> Result<WorkspaceDurableState, DaemonError> {
        self.read_json_or_default(self.state_root.join("workspaces.json"))
    }

    pub(crate) fn save_workspace_durable_state(
        &self,
        state: &WorkspaceDurableState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.state_root.join("workspaces.json"), state)
    }

    pub(crate) fn worker_runtime_snapshot_path(&self) -> PathBuf {
        self.state_root.join("worker-runtime.json")
    }

    pub(crate) fn load_worker_runtime_snapshot(
        &self,
    ) -> Result<WorkerRuntimeDurableSnapshot, DaemonError> {
        self.read_json_or_default(self.worker_runtime_snapshot_path())
    }

    pub(crate) fn save_worker_runtime_snapshot(
        &self,
        snapshot: &WorkerRuntimeDurableSnapshot,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.worker_runtime_snapshot_path(), snapshot)
    }

    pub(crate) fn workspace_recovery_sidecars_path(&self) -> PathBuf {
        self.state_root.join("workspace-recovery-sidecars.json")
    }

    pub(crate) fn load_workspace_recovery_sidecars(
        &self,
    ) -> Result<WorkspaceRecoverySidecarStoreState, DaemonError> {
        self.read_json_or_default(self.workspace_recovery_sidecars_path())
    }

    pub(crate) fn save_workspace_recovery_sidecars(
        &self,
        state: &WorkspaceRecoverySidecarStoreState,
    ) -> Result<(), DaemonError> {
        self.write_json_atomically(self.workspace_recovery_sidecars_path(), state)
    }

    pub(crate) fn audit_usage_ledger_path(&self) -> PathBuf {
        self.state_root.join("audit-usage-ledger.json")
    }

    pub(crate) fn load_audit_usage_ledger(&self) -> Result<AuditUsageLedgerSnapshot, DaemonError> {
        self.read_json_or_default(self.audit_usage_ledger_path())
    }

    pub(crate) fn knowledge_state_path(&self) -> PathBuf {
        self.state_root.join("knowledge.json")
    }

    pub(crate) fn load_knowledge_state(&self) -> Result<KnowledgeState, DaemonError> {
        self.read_json_or_default(self.knowledge_state_path())
    }

    pub(crate) fn save_knowledge_state(&self, state: &KnowledgeState) -> Result<(), DaemonError> {
        self.write_json_atomically(self.knowledge_state_path(), state)
    }

    fn read_json_or_default<T>(&self, path: PathBuf) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        if !path.exists() {
            return Ok(T::default());
        }
        self.read_json_value_or_default(path)
    }

    fn read_session_durable_state_or_default(
        &self,
        path: PathBuf,
    ) -> Result<SessionDurableState, DaemonError> {
        if !path.exists() {
            return Ok(SessionDurableState::default());
        }
        let content = fs::read_to_string(&path)?;
        let mut value = match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(value) => value,
            Err(error) => return self.backup_stale_json(path, error),
        };
        migrate_session_goal_state(&mut value);
        match serde_json::from_value(value) {
            Ok(value) => Ok(value),
            Err(error) => self.backup_stale_json(path, error),
        }
    }

    fn backup_stale_json<T>(
        &self,
        path: PathBuf,
        error: serde_json::Error,
    ) -> Result<T, DaemonError>
    where
        T: Default,
    {
        let backup_path = stale_backup_path(&path);
        warn!(
            path = %path.display(),
            backup_path = %backup_path.display(),
            error = %error,
            "影子状态文件与当前 schema 不兼容，已转存并回退到默认状态"
        );
        fs::rename(&path, &backup_path)?;
        Ok(T::default())
    }

    fn read_json_value_or_default<T>(&self, path: PathBuf) -> Result<T, DaemonError>
    where
        T: Default + for<'de> serde::Deserialize<'de>,
    {
        let content = fs::read_to_string(&path)?;
        match serde_json::from_str(&content) {
            Ok(value) => Ok(value),
            Err(error) => self.backup_stale_json(path, error),
        }
    }

    fn write_json_atomically<T>(&self, path: PathBuf, value: &T) -> Result<(), DaemonError>
    where
        T: serde::Serialize,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_vec_pretty(value)?;
        magi_core::fs_atomic::write_atomic(&path, content)?;
        Ok(())
    }
}

fn migrate_session_goal_state(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let latest = {
        let Some(goals) = object
            .get_mut("goals")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return;
        };
        let mut latest = HashMap::<String, (u64, String, bool, bool)>::new();
        for goal in goals.iter_mut() {
            let Some(goal) = goal.as_object_mut() else {
                continue;
            };
            goal.remove("consecutiveFailureTurns");
            let Some(session_id) = goal.get("sessionId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(goal_id) = goal.get("goalId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let updated_at = goal
                .get("updatedAt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let status = goal
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let cleared = status == "cleared";
            let unfinished = matches!(
                status,
                "active" | "paused" | "blocked" | "usage_limited" | "budget_limited"
            );
            let entry = latest.entry(session_id.to_string()).or_insert((
                updated_at,
                goal_id.to_string(),
                cleared,
                unfinished,
            ));
            if updated_at > entry.0 || (updated_at == entry.0 && goal_id > entry.1.as_str()) {
                *entry = (updated_at, goal_id.to_string(), cleared, unfinished);
            }
        }
        goals.retain(|goal| {
            let Some(goal) = goal.as_object() else {
                return false;
            };
            let Some(session_id) = goal.get("sessionId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some(goal_id) = goal.get("goalId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            latest
                .get(session_id)
                .is_some_and(|(_, latest_goal_id, cleared, _)| {
                    !cleared && latest_goal_id == goal_id
                })
        });
        latest
    };

    for plans_key in ["plans", "todo_lists"] {
        let Some(plans) = object
            .get_mut(plans_key)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        plans.retain_mut(|plan| {
            let Some(plan) = plan.as_object_mut() else {
                return false;
            };
            let Some(session_id) = plan.get("sessionId").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Some((_, goal_id, cleared, unfinished)) = latest.get(session_id) else {
                return true;
            };
            if *cleared {
                return false;
            }
            if *unfinished || plan.get("goalId").is_some() {
                plan.insert(
                    "goalId".to_string(),
                    serde_json::Value::String(goal_id.clone()),
                );
            }
            true
        });
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeSidecarFlushReport {
    pub(crate) session_sidecars_flushed: bool,
    pub(crate) workspace_recovery_sidecars_flushed: bool,
    pub(crate) worker_runtime_snapshot_flushed: bool,
}

#[derive(Clone)]
pub(crate) struct RuntimeSidecarPersistence {
    state_repository: StateRepository,
    session_store: Arc<SessionStore>,
    workspace_store: Arc<WorkspaceStore>,
    worker_runtime: WorkerRuntime,
}

impl RuntimeSidecarPersistence {
    pub(crate) fn new(
        state_repository: StateRepository,
        session_store: Arc<SessionStore>,
        workspace_store: Arc<WorkspaceStore>,
        worker_runtime: WorkerRuntime,
    ) -> Self {
        Self {
            state_repository,
            session_store,
            workspace_store,
            worker_runtime,
        }
    }

    pub(crate) fn worker_runtime_snapshot_dirty(&self) -> bool {
        self.worker_runtime.durable_snapshot_dirty()
    }

    fn save_session_durable_state(&self) -> Result<(), DaemonError> {
        self.session_store.persist_durable_state_with(|durable| {
            let (mut global_state, mut workspace_states) = durable.partition_by_workspace();
            for workspace in self.workspace_store.workspaces() {
                let workspace_id = workspace.workspace_id.to_string();
                let workspace_state = workspace_states.remove(&workspace_id).unwrap_or_default();
                self.state_repository.save_workspace_session_state(
                    workspace.native_root_path().as_path(),
                    &workspace_state,
                )?;
            }

            let orphan_session_count: usize = workspace_states
                .values()
                .map(|state| state.sessions.len())
                .sum();
            if orphan_session_count > 0 {
                global_state.clear_current_session_if_owned_by_workspace_states(&workspace_states);
                warn!(
                    orphan_session_count,
                    "跳过未注册 workspace 的会话持久化；workspace 绑定会话必须写入对应工作区状态"
                );
            }

            let global_path = self.state_repository.session_durable_state_path();
            if global_state.is_empty() {
                match fs::remove_file(&global_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            } else {
                self.state_repository
                    .save_session_durable_state(&global_state)?;
            }
            Ok(())
        })
    }

    pub(crate) fn flush_runtime_sidecars(&self) -> Result<RuntimeSidecarFlushReport, DaemonError> {
        let worker_runtime_snapshot_flushed =
            self.worker_runtime
                .flush_durable_snapshot_with(|snapshot| {
                    self.state_repository.save_worker_runtime_snapshot(snapshot)
                })?;
        let session_sidecars_flushed =
            self.session_store.flush_execution_sidecars_with(|state| {
                self.state_repository.save_session_sidecars(state)?;
                self.save_session_durable_state()
            })?;
        let workspace_recovery_sidecars_flushed =
            self.workspace_store.flush_recovery_sidecars_with(|state| {
                self.state_repository
                    .save_workspace_recovery_sidecars(state)
            })?;
        Ok(RuntimeSidecarFlushReport {
            session_sidecars_flushed,
            workspace_recovery_sidecars_flushed,
            worker_runtime_snapshot_flushed,
        })
    }

    pub(crate) fn flush_session_sidecars(&self) -> Result<bool, DaemonError> {
        self.session_store.flush_execution_sidecars_with(|state| {
            self.state_repository.save_session_sidecars(state)
        })
    }
}

fn stale_backup_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "state.json".to_string());
    file_name.push_str(".stale");
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus, PlanState, SessionId,
        SessionLifecycleStatus, TaskId, ThreadId, UtcMillis, WorkerId,
    };
    use magi_session_store::{
        ExecutionThread, ExecutionThreadStatus, NotificationRecord, NotificationScope,
        SessionDurableState, SessionPlan, SessionRecord, ThreadChatMessage,
        ThreadContextCheckpoint, TimelineEntry, TimelineEntryKind,
    };
    use std::collections::HashMap;

    #[test]
    fn legacy_cleared_goal_migrates_to_empty_current_slot() {
        let mut value = serde_json::json!({
            "goals": [
                {
                    "sessionId": "session-migrated-goal",
                    "goalId": "goal-complete",
                    "status": "complete",
                    "updatedAt": 10,
                    "consecutiveFailureTurns": 2
                },
                {
                    "sessionId": "session-migrated-goal",
                    "goalId": "goal-cleared",
                    "status": "cleared",
                    "updatedAt": 20,
                    "consecutiveFailureTurns": 0
                }
            ],
            "todo_lists": [{
                "sessionId": "session-migrated-goal",
                "planId": "plan-cleared"
            }]
        });

        migrate_session_goal_state(&mut value);

        assert_eq!(value["goals"], serde_json::json!([]));
        assert_eq!(value["todo_lists"], serde_json::json!([]));
    }

    #[test]
    fn legacy_goal_history_collapses_to_latest_goal_and_binds_plan() {
        let mut value = serde_json::json!({
            "goals": [
                {
                    "sessionId": "session-migrated-goal",
                    "goalId": "goal-old",
                    "status": "complete",
                    "updatedAt": 10,
                    "consecutiveFailureTurns": 2
                },
                {
                    "sessionId": "session-migrated-goal",
                    "goalId": "goal-current",
                    "status": "active",
                    "updatedAt": 20,
                    "consecutiveFailureTurns": 1
                }
            ],
            "plans": [{
                "sessionId": "session-migrated-goal",
                "planId": "plan-current"
            }]
        });

        migrate_session_goal_state(&mut value);

        assert_eq!(value["goals"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["goals"][0]["goalId"], "goal-current");
        assert!(value["goals"][0].get("consecutiveFailureTurns").is_none());
        assert_eq!(value["plans"][0]["goalId"], "goal-current");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            UtcMillis::now().0
        ));
        fs::create_dir_all(&path).expect("temp dir should create");
        path
    }

    fn session_incident(
        notification_id: &str,
        workspace_id: &str,
        session_id: &SessionId,
        message: &str,
        created_at: UtcMillis,
    ) -> NotificationRecord {
        NotificationRecord {
            notification_id: notification_id.to_string(),
            scope: NotificationScope::Session,
            workspace_id: Some(workspace_id.to_string()),
            session_id: Some(session_id.clone()),
            kind: "incident".to_string(),
            level: Some("error".to_string()),
            title: None,
            message: message.to_string(),
            detail: None,
            error_code: None,
            failure_stage: None,
            task_id: None,
            request_id: None,
            source: Some("test".to_string()),
            created_at,
            handled: false,
            action_required: true,
            count_unread: true,
            fingerprint: notification_id.to_string(),
            occurrence_count: 1,
            resolved: false,
        }
    }

    #[test]
    fn load_sessions_from_workspaces_merges_history_and_thread_registry() {
        let state_root = unique_temp_dir("magi-persistence-state");
        let workspace_root = unique_temp_dir("magi-persistence-workspace");
        let repository = StateRepository::new(state_root.clone());
        let session_id = SessionId::new("session-persisted-timeline");
        let now = UtcMillis::now();
        let workspace_state = SessionDurableState {
            current_session_id: Some(session_id.clone()),
            sessions: vec![SessionRecord {
                session_id: session_id.clone(),
                title: "持久化会话".to_string(),
                status: SessionLifecycleStatus::Active,
                created_at: now,
                updated_at: now,
                message_count: Some(1),
                workspace_id: Some("workspace-persisted".to_string()),
                last_completed_at: None,
                last_viewed_at: None,
            }],
            timeline: vec![TimelineEntry {
                entry_id: "timeline-persisted-user".to_string(),
                session_id: session_id.clone(),
                kind: TimelineEntryKind::UserMessage,
                message: "恢复后的用户消息".to_string(),
                occurred_at: now,
            }],
            canonical_turns: vec![],
            notifications: vec![session_incident(
                "notification-persisted",
                "workspace-persisted",
                &session_id,
                "恢复后的异常",
                now,
            )],
            goals: vec![],
            plans: vec![SessionPlan {
                plan_id: PlanId::new("plan-persisted"),
                session_id: session_id.clone(),
                goal_id: None,
                revision: 1,
                language: "zh-CN".to_string(),
                state: PlanState::Active,
                items: vec![PlanItem::new(
                    PlanItemId::new("restore-plan"),
                    "恢复目标任务清单",
                    PlanItemStatus::InProgress,
                )],
                task_bindings: HashMap::new(),
                task_statuses: HashMap::new(),
                updated_at: now,
            }],
            thread_registry: vec![ExecutionThread {
                thread_id: ThreadId::new("thread-persisted"),
                session_id: session_id.clone(),
                mission_id: MissionId::new("mission-persisted"),
                role_id: "executor".to_string(),
                worker_instance_id: WorkerId::new("worker-persisted"),
                status: ExecutionThreadStatus::Active,
                created_at: now,
                last_used_at: now,
                observed_context_window_tokens: Some(32_000),
                handled_task_ids: vec![TaskId::new("task-persisted")],
                message_history: vec![ThreadChatMessage {
                    role: "tool".to_string(),
                    content: Some("exit code 1: persisted tool error".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call-persisted".to_string()),
                    provider_context: Vec::new(),
                }],
            }],
            thread_context_checkpoints: vec![ThreadContextCheckpoint {
                thread_id: ThreadId::new("thread-persisted"),
                checkpoint_id: "checkpoint-persisted".to_string(),
                source_message_count: 1,
                summary_message: ThreadChatMessage {
                    role: "system".to_string(),
                    content: Some("恢复后的上下文检查点".to_string()),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    provider_context: Vec::new(),
                },
                reason: "context_window_pressure".to_string(),
                original_token_estimate: 32_000,
                checkpoint_token_estimate: 4_000,
                created_at: now,
                file_fact_versions: Vec::new(),
            }],
        };
        repository
            .save_workspace_session_state(&workspace_root, &workspace_state)
            .expect("workspace session state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[(
                "workspace-persisted".to_string(),
                workspace_root.clone(),
            )])
            .expect("workspace session state should load");

        assert_eq!(merged.sessions.len(), 1);
        assert_eq!(merged.timeline.len(), 1);
        assert_eq!(merged.notifications.len(), 1);
        assert_eq!(merged.plans.len(), 1);
        assert_eq!(merged.plans[0].items[0].title, "恢复目标任务清单");
        assert_eq!(merged.thread_registry.len(), 1);
        assert_eq!(merged.thread_context_checkpoints.len(), 1);
        assert_eq!(
            merged.thread_context_checkpoints[0].checkpoint_id,
            "checkpoint-persisted"
        );
        assert_eq!(
            merged.thread_registry[0].thread_id.as_str(),
            "thread-persisted"
        );
        assert_eq!(
            merged.thread_registry[0].observed_context_window_tokens,
            Some(32_000)
        );
        assert_eq!(
            merged.thread_registry[0].message_history[0]
                .content
                .as_deref(),
            Some("exit code 1: persisted tool error")
        );
        assert_eq!(merged.current_session_id, Some(session_id));

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn load_sessions_from_workspaces_cleans_workspace_bound_sessions_from_global_state() {
        let state_root = unique_temp_dir("magi-persistence-unknown-workspace");
        let repository = StateRepository::new(state_root.clone());
        let session_id = SessionId::new("session-unknown-workspace");
        let now = UtcMillis::now();

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id: session_id.clone(),
                    title: "未知工作区会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(1),
                    workspace_id: Some("workspace-missing".to_string()),
                    last_completed_at: None,
                    last_viewed_at: None,
                }],
                timeline: vec![TimelineEntry {
                    entry_id: "timeline-unknown-workspace".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "未知工作区会话消息".to_string(),
                    occurred_at: now,
                }],
                canonical_turns: vec![],
                notifications: vec![session_incident(
                    "notification-unknown-workspace",
                    "workspace-missing",
                    &session_id,
                    "未知工作区异常",
                    now,
                )],
                goals: vec![],
                plans: vec![],
                thread_registry: vec![],
                thread_context_checkpoints: vec![],
            })
            .expect("invalid global session state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[])
            .expect("workspace-bound global sessions should be discarded");

        assert!(merged.sessions.is_empty());
        assert!(merged.timeline.is_empty());
        assert!(merged.notifications.is_empty());
        assert_eq!(merged.current_session_id, None);

        assert!(
            !repository.session_durable_state_path().exists(),
            "全局 sessions.json 不能继续保留 workspace-bound 孤儿会话"
        );

        let _ = fs::remove_dir_all(state_root);
    }

    #[test]
    fn load_sessions_from_workspaces_does_not_migrate_global_workspace_sessions() {
        let state_root = unique_temp_dir("magi-persistence-no-global-migration");
        let workspace_root = unique_temp_dir("magi-persistence-no-global-migration-workspace");
        let repository = StateRepository::new(state_root.clone());
        let session_id = SessionId::new("session-global-workspace-bound");
        let now = UtcMillis::now();

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id: session_id.clone(),
                    title: "全局旧布局工作区会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(1),
                    workspace_id: Some("workspace-registered".to_string()),
                    last_completed_at: None,
                    last_viewed_at: None,
                }],
                timeline: vec![TimelineEntry {
                    entry_id: "timeline-global-workspace-bound".to_string(),
                    session_id: session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "全局旧布局消息".to_string(),
                    occurred_at: now,
                }],
                canonical_turns: vec![],
                notifications: vec![],
                goals: vec![],
                plans: vec![],
                thread_registry: vec![],
                thread_context_checkpoints: vec![],
            })
            .expect("invalid global session state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[(
                "workspace-registered".to_string(),
                workspace_root.clone(),
            )])
            .expect("registered workspace must not receive global workspace-bound sessions");
        assert!(
            merged.sessions.is_empty(),
            "workspace-bound sessions in global state must not be loaded"
        );

        let workspace_state = repository
            .load_workspace_session_state(&workspace_root)
            .expect("workspace session state should load");
        assert!(
            workspace_state.sessions.is_empty(),
            "旧全局布局不能迁移写入工作区 sessions.json"
        );
        assert!(
            !repository.session_durable_state_path().exists(),
            "清理后不能继续保留只包含 workspace-bound 会话的全局 sessions.json"
        );

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn load_sessions_from_workspaces_preserves_global_unbound_sessions() {
        let state_root = unique_temp_dir("magi-persistence-global-session");
        let workspace_root = unique_temp_dir("magi-persistence-global-workspace");
        let repository = StateRepository::new(state_root.clone());
        let now = UtcMillis::now();
        let global_session_id = SessionId::new("session-global-unbound");
        let workspace_session_id = SessionId::new("session-workspace-bound");

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(global_session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id: global_session_id.clone(),
                    title: "全局会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(1),
                    workspace_id: None,
                    last_completed_at: None,
                    last_viewed_at: None,
                }],
                timeline: vec![TimelineEntry {
                    entry_id: "timeline-global-session".to_string(),
                    session_id: global_session_id.clone(),
                    kind: TimelineEntryKind::UserMessage,
                    message: "全局未绑定消息".to_string(),
                    occurred_at: now,
                }],
                canonical_turns: vec![],
                notifications: vec![],
                goals: vec![],
                plans: vec![],
                thread_registry: vec![],
                thread_context_checkpoints: vec![],
            })
            .expect("global session durable state should save");

        repository
            .save_workspace_session_state(
                &workspace_root,
                &SessionDurableState {
                    current_session_id: Some(workspace_session_id.clone()),
                    sessions: vec![SessionRecord {
                        session_id: workspace_session_id.clone(),
                        title: "工作区会话".to_string(),
                        status: SessionLifecycleStatus::Active,
                        created_at: now,
                        updated_at: now,
                        message_count: Some(2),
                        workspace_id: Some("workspace-bound".to_string()),
                        last_completed_at: None,
                        last_viewed_at: None,
                    }],
                    timeline: vec![TimelineEntry {
                        entry_id: "timeline-workspace-session".to_string(),
                        session_id: workspace_session_id.clone(),
                        kind: TimelineEntryKind::UserMessage,
                        message: "工作区绑定消息".to_string(),
                        occurred_at: now,
                    }],
                    canonical_turns: vec![],
                    notifications: vec![],
                    goals: vec![],
                    plans: vec![],
                    thread_registry: vec![],
                    thread_context_checkpoints: vec![],
                },
            )
            .expect("workspace session durable state should save");

        let merged = repository
            .load_sessions_from_workspaces(&[(
                "workspace-bound".to_string(),
                workspace_root.clone(),
            )])
            .expect("session durable states should merge");

        assert_eq!(merged.sessions.len(), 2);
        assert!(merged.sessions.iter().any(
            |session| session.session_id == global_session_id && session.workspace_id.is_none()
        ));
        assert!(merged.sessions.iter().any(|session| {
            session.session_id == workspace_session_id
                && session.workspace_id.as_deref() == Some("workspace-bound")
        }));
        assert_eq!(merged.current_session_id, Some(global_session_id));
        assert_eq!(merged.timeline.len(), 2);

        let _ = fs::remove_dir_all(state_root);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn load_session_sidecars_ignores_session_durable_state_without_sidecar_file() {
        let state_root = unique_temp_dir("magi-persistence-session-sidecar-missing");
        let repository = StateRepository::new(state_root.clone());
        let now = UtcMillis::now();
        let session_id = SessionId::new("session-no-sidecar-file");

        repository
            .save_session_durable_state(&SessionDurableState {
                current_session_id: Some(session_id.clone()),
                sessions: vec![SessionRecord {
                    session_id,
                    title: "普通会话".to_string(),
                    status: SessionLifecycleStatus::Active,
                    created_at: now,
                    updated_at: now,
                    message_count: Some(0),
                    workspace_id: None,
                    last_completed_at: None,
                    last_viewed_at: None,
                }],
                timeline: vec![],
                canonical_turns: vec![],
                notifications: vec![],
                goals: vec![],
                plans: vec![],
                thread_registry: vec![],
                thread_context_checkpoints: vec![],
            })
            .expect("global session durable state should save");

        let sidecars = repository
            .load_session_sidecars()
            .expect("session sidecar loader should read only the sidecar file");
        assert!(sidecars.runtime_sidecars.is_empty());
        assert!(repository.session_durable_state_path().exists());

        let _ = fs::remove_dir_all(state_root);
    }
}

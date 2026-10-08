//! 会话级隔离工作副本的运行时服务。
//!
//! 隔离会话在 Magi 管理的私有目录里读写自己的工作副本，不会与同一工作区里的其它会话互相覆盖；
//! 改动记录在会话自己的变更账本里，用户确认后再合并回主工作区。机制本身（建立副本、三方合并）
//! 在 `magi-session-isolation`，这里负责把它接进会话生命周期：何时建立、会话的执行根目录、
//! 变更账本、清理，以及合并时的并发约束。
use crate::errors::ApiError;
use crate::state::ApiState;
use magi_core::{DomainError, SessionId, SessionLifecycleStatus, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_isolation::{
    IsolationOrigin, MergeOutcome, MergePlan, MergeSelection, SessionIsolation, apply_merge,
    create_isolated_copy, plan_merge,
};
use std::path::{Path, PathBuf};

const ISOLATIONS_FILE: &str = "session-isolations.json";
const ISOLATIONS_DIR: &str = "session-isolations";

impl ApiState {
    pub(crate) fn session_isolation(&self, session_id: &SessionId) -> Option<SessionIsolation> {
        self.session_isolations.get(session_id.as_str())
    }

    /// 隔离会话的执行根目录；非隔离会话返回 `None`。
    pub(crate) fn session_isolation_root(&self, session_id: &SessionId) -> Option<PathBuf> {
        self.session_isolation(session_id)
            .map(|isolation| isolation.root)
    }

    /// 持久化隔离登记，daemon 重启后会话继续使用同一个副本。
    pub(crate) fn persist_session_isolations(&self) -> Result<(), ApiError> {
        let Some(persistence) = self.runtime_persistence() else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        persistence.save_json(
            &state_root.join(ISOLATIONS_FILE),
            &self.session_isolations.all(),
        )
    }

    pub(crate) fn restore_session_isolations_from(&self, state_root: &Path) {
        let path = state_root.join(ISOLATIONS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<SessionIsolation>>(&bytes) {
                Ok(isolations) => {
                    let total = isolations.len();
                    let restored = self.session_isolations.restore(isolations);
                    if restored < total {
                        tracing::warn!(
                            dropped = total - restored,
                            "部分隔离副本目录已不存在，对应会话回到主工作区运行"
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "忽略无法解析的会话隔离登记"
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %path.display(),
                error = %error,
                "忽略无法读取的会话隔离登记"
            ),
        }
    }

    fn isolation_copy_path(&self, session_id: &SessionId, source_root: &Path) -> PathBuf {
        let state_root = self
            .runtime_persistence()
            .and_then(crate::state::RuntimeStatePersistence::state_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::temp_dir().join("magi"));
        let name = source_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "workspace".to_string());
        state_root
            .join(ISOLATIONS_DIR)
            .join(session_id.as_str())
            .join(name)
    }

    fn publish_isolation_event(
        &self,
        event_name: &str,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
        payload: serde_json::Value,
    ) {
        let mut body = serde_json::json!({
            "session_id": session_id,
            "workspace_id": workspace_id,
        });
        if let (Some(target), Some(extra)) = (body.as_object_mut(), payload.as_object()) {
            target.extend(extra.clone());
        }
        let _ = self.event_bus.publish(
            EventEnvelope::domain(
                magi_core::EventId::unique(format!("{event_name}-{session_id}")),
                event_name,
                body,
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.cloned(),
                ..EventContext::default()
            }),
        );
    }

    /// 让会话在隔离副本里运行。幂等：已经隔离则直接返回现有登记。
    ///
    /// 约束：会话当前没有进行中的轮次；它在主工作区里没有未处理的变更（切换后账本改为记录
    /// 副本里的改动，主工作区里的待处理变更会失去归属）；没有仍在运行的子代理 worktree。
    pub(crate) async fn enable_session_isolation(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        origin: IsolationOrigin,
    ) -> Result<SessionIsolation, ApiError> {
        if let Some(existing) = self.session_isolation(session_id) {
            return Ok(existing);
        }
        let source_root = self
            .workspace_root_path(&Some(workspace_id.clone()))
            .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))?;
        let source_root = std::fs::canonicalize(&source_root).unwrap_or(source_root);
        match self
            .session_store
            .ensure_current_turn_acceptance_available(session_id)
        {
            Ok(()) => {}
            Err(DomainError::CurrentTurnConflict { .. }) => {
                return Err(ApiError::conflict(
                    "会话正在执行，结束后才能切换到隔离副本",
                    session_id.as_str(),
                ));
            }
            Err(error) => {
                return Err(ApiError::internal_assembly("检查会话执行状态失败", error));
            }
        }
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        if let Some(snapshot) = self.snapshot_manager.get_session(session_id.as_str()) {
            let pending = snapshot
                .pending_changes()
                .map_err(|error| ApiError::internal_assembly("读取会话变更失败", error))?;
            if !pending.is_empty() {
                return Err(ApiError::conflict(
                    "会话在主工作区里还有未处理的变更，先批准或回退后再切换到隔离副本",
                    session_id.as_str(),
                ));
            }
        }
        if let Some(context) = self.session_code_contexts.get(session_id.as_str())
            && context
                .agent_worktrees
                .iter()
                .any(|worktree| worktree.active)
        {
            return Err(ApiError::conflict(
                "会话还有子代理在使用独立 worktree，结束后才能切换到隔离副本",
                session_id.as_str(),
            ));
        }

        let destination = self.isolation_copy_path(session_id, &source_root);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ApiError::internal_assembly("创建隔离副本目录失败", error))?;
        }
        let copy_source = source_root.clone();
        let copy_destination = destination.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            create_isolated_copy(&copy_source, &copy_destination)
        })
        .await
        .map_err(|error| ApiError::internal_assembly("建立隔离副本任务失败", error))?
        .map_err(|error| {
            let _ = std::fs::remove_dir_all(&destination);
            ApiError::internal_assembly("建立隔离副本失败", error)
        })?;

        let isolation = SessionIsolation {
            session_id: session_id.as_str().to_string(),
            workspace_id: workspace_id.as_str().to_string(),
            source_root: source_root.clone(),
            root: destination.clone(),
            origin,
            strategy: outcome.strategy,
            linked_dirs: outcome.linked_dirs,
            git_available: outcome.git_available,
            created_at_ms: UtcMillis::now().0,
        };
        // 先登记再启动账本：`ensure_snapshot_session` 依据登记把主工作区路径换成副本路径。
        self.session_isolations.insert(isolation.clone());
        if let Err(error) = self
            .start_isolated_snapshot_session(session_id, &isolation)
            .await
        {
            self.session_isolations.remove(session_id.as_str());
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(destination)).await;
            return Err(error);
        }
        // 副本里的 Git 与主工作区的 Git 是两回事：移除会话原有的主工作区 Git 上下文与执行租约。
        self.release_session_git_execution_lease(session_id);
        if self
            .session_code_contexts
            .get(session_id.as_str())
            .is_some()
        {
            self.session_code_contexts.remove(session_id.as_str());
            let _ = self.persist_session_git_contexts();
        }
        self.persist_session_isolations()?;
        tracing::info!(
            session_id = %session_id,
            strategy = ?isolation.strategy,
            root = %isolation.root.display(),
            "会话已切换到隔离副本"
        );
        self.publish_isolation_event(
            "session.isolation.changed",
            session_id,
            Some(workspace_id),
            serde_json::json!({ "enabled": true }),
        );
        Ok(isolation)
    }

    async fn start_isolated_snapshot_session(
        &self,
        session_id: &SessionId,
        isolation: &SessionIsolation,
    ) -> Result<(), ApiError> {
        // 切换前主工作区上的账本整个替换为副本上的账本（上面已确认没有待处理变更）。
        let _ = self
            .snapshot_manager
            .drop_session(session_id.as_str())
            .await;
        self.snapshot_manager
            .start_session(session_id.as_str().to_string(), isolation.root.clone())
            .await
            .map(|_| ())
            .map_err(|error| ApiError::internal_assembly("启动隔离副本变更账本失败", error))
    }

    /// 丢弃隔离副本，会话回到主工作区运行。副本里尚未合并的改动随之丢失。
    pub(crate) async fn discard_session_isolation(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ApiError> {
        let Some(isolation) = self.session_isolation(session_id) else {
            return Ok(());
        };
        if let Err(DomainError::CurrentTurnConflict { .. }) = self
            .session_store
            .ensure_current_turn_acceptance_available(session_id)
        {
            return Err(ApiError::conflict(
                "会话正在执行，结束后才能取消隔离",
                session_id.as_str(),
            ));
        }
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        self.remove_isolation_resources(session_id, &isolation)
            .await;
        self.persist_session_isolations()?;
        let workspace_id = WorkspaceId::new(isolation.workspace_id.clone());
        self.publish_isolation_event(
            "session.isolation.changed",
            session_id,
            Some(&workspace_id),
            serde_json::json!({ "enabled": false }),
        );
        Ok(())
    }

    /// 启动后回收没有人再用的隔离副本，返回回收的数量：
    ///
    /// - 登记着、但会话已经不存在（会话在守护进程停止期间被删除）的条目；
    /// - 目录还在、但不在登记里的副本（建立到一半崩溃、登记文件丢失）。
    ///
    /// 登记的清理是同步的；目录可能很大，放到后台线程里删除，不拖慢启动。
    pub fn reclaim_orphan_session_isolations(&self) -> usize {
        let mut stale_dirs: std::collections::BTreeSet<PathBuf> = Default::default();
        for isolation in self.session_isolations.all() {
            let session_id = SessionId::new(isolation.session_id.clone());
            if self.session_store.session(&session_id).is_none() {
                self.session_isolations.remove(&isolation.session_id);
                stale_dirs.insert(
                    isolation
                        .root
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| isolation.root.clone()),
                );
            }
        }
        if let Some(state_root) = self
            .runtime_persistence()
            .and_then(crate::state::RuntimeStatePersistence::state_root)
        {
            let base = state_root.join(ISOLATIONS_DIR);
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.filter_map(Result::ok) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if entry.path().is_dir() && !self.session_isolations.contains(&name) {
                        stale_dirs.insert(entry.path());
                    }
                }
            }
        }
        let reclaimed = stale_dirs.len();
        if reclaimed > 0 {
            if let Err(error) = self.persist_session_isolations() {
                tracing::warn!(?error, "持久化隔离登记清理结果失败");
            }
            tracing::info!(count = reclaimed, "回收没有人再用的会话隔离副本");
            std::thread::spawn(move || {
                for dir in stale_dirs {
                    if let Err(error) = std::fs::remove_dir_all(&dir)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %dir.display(), %error, "删除孤儿隔离副本失败");
                    }
                }
            });
        }
        reclaimed
    }

    /// 会话被删除时回收隔离副本。
    pub(crate) async fn cleanup_session_isolation(&self, session_id: &SessionId) {
        let Some(isolation) = self.session_isolation(session_id) else {
            return;
        };
        self.remove_isolation_resources(session_id, &isolation)
            .await;
        if let Err(error) = self.persist_session_isolations() {
            tracing::warn!(session_id = %session_id, ?error, "持久化隔离副本清理失败");
        }
    }

    async fn remove_isolation_resources(
        &self,
        session_id: &SessionId,
        isolation: &SessionIsolation,
    ) {
        self.session_isolations.remove(session_id.as_str());
        let _ = self
            .snapshot_manager
            .drop_session(session_id.as_str())
            .await;
        // 目录里有指回主工作区的符号链接（node_modules 等）；`remove_dir_all` 只删链接本身。
        let session_dir = isolation
            .root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| isolation.root.clone());
        let removal =
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(session_dir)).await;
        if let Ok(Err(error)) = removal
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(session_id = %session_id, %error, "删除隔离副本目录失败");
        }
    }

    /// 会话工具的 cwd / 权限根目录覆盖：隔离会话用它的副本，个人会话用 Magi 管理的私有目录，
    /// 其它会话（直接在主工作区运行）没有覆盖。
    pub(crate) fn session_execution_root_override(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        if let Some(root) = self.session_isolation_root(session_id) {
            return Some(root);
        }
        workspace_id
            .is_none()
            .then(|| self.personal_session_execution_root_path(session_id))
    }

    fn require_isolation(&self, session_id: &SessionId) -> Result<SessionIsolation, ApiError> {
        self.session_isolation(session_id)
            .ok_or_else(|| ApiError::InvalidInput("会话没有使用隔离副本".to_string()))
    }

    async fn isolated_snapshot(
        &self,
        session_id: &SessionId,
        isolation: &SessionIsolation,
    ) -> Result<std::sync::Arc<magi_snapshot::SnapshotSession>, ApiError> {
        let snapshot = self
            .ensure_snapshot_session(session_id, &isolation.source_root)
            .await?;
        snapshot
            .reconcile_async()
            .await
            .map_err(|error| ApiError::internal_assembly("刷新隔离副本变更失败", error))?;
        Ok(snapshot)
    }

    /// 预览把隔离副本的改动合并回主工作区会发生什么。
    pub(crate) async fn isolation_merge_plan(
        &self,
        session_id: &SessionId,
    ) -> Result<MergePlan, ApiError> {
        let isolation = self.require_isolation(session_id)?;
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let snapshot = self.isolated_snapshot(session_id, &isolation).await?;
        let source_root = isolation.source_root.clone();
        tokio::task::spawn_blocking(move || plan_merge(&snapshot, &source_root))
            .await
            .map_err(|error| ApiError::internal_assembly("计算合并计划任务失败", error))?
            .map_err(|error| ApiError::internal_assembly("计算合并计划失败", error))
    }

    /// 把隔离副本的改动合并回主工作区。
    pub(crate) async fn isolation_merge_apply(
        &self,
        session_id: &SessionId,
        selection: MergeSelection,
    ) -> Result<MergeOutcome, ApiError> {
        // 执行过程中也允许合并：与共享模式下批准变更一致，长时间运行的目标可以边做边合并。
        let isolation = self.require_isolation(session_id)?;
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let snapshot = self.isolated_snapshot(session_id, &isolation).await?;
        let source_root = isolation.source_root.clone();
        let outcome =
            tokio::task::spawn_blocking(move || apply_merge(&snapshot, &source_root, &selection))
                .await
                .map_err(|error| ApiError::internal_assembly("合并隔离副本任务失败", error))?
                .map_err(|error| ApiError::internal_assembly("合并隔离副本失败", error))?;
        let workspace_id = WorkspaceId::new(isolation.workspace_id.clone());
        self.publish_isolation_event(
            "session.isolation.merged",
            session_id,
            Some(&workspace_id),
            serde_json::json!({
                "applied": outcome.applied.len(),
                "already_applied": outcome.already_applied.len(),
                "unresolved": outcome.unresolved.len(),
                "failed": outcome.failed.len(),
            }),
        );
        Ok(outcome)
    }

    /// 同一工作区里另一个正在执行轮次、且运行在主工作区的会话。
    fn session_blocking_shared_workspace(
        &self,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
    ) -> Option<SessionId> {
        self.session_store
            .sessions()
            .into_iter()
            .filter(|session| {
                session.session_id != *session_id
                    && session.status == SessionLifecycleStatus::Active
                    && session.workspace_id.as_deref() == Some(workspace_id.as_str())
                    && !self
                        .session_isolations
                        .contains(session.session_id.as_str())
            })
            .find(|session| {
                matches!(
                    self.session_store
                        .ensure_current_turn_acceptance_available(&session.session_id),
                    Err(DomainError::CurrentTurnConflict { .. })
                )
            })
            .map(|session| session.session_id)
    }

    /// 同一工作区里另一个会话正在主工作区执行时，让这一轮自动改在隔离副本里运行。
    ///
    /// 只在这一轮要使用工具、会话本身还没有隔离、且切换的前置条件满足时才会发生；
    /// 任何一步不满足都返回 `None`，这一轮仍在主工作区运行（由执行租约排队），绝不因此失败。
    pub(crate) async fn isolate_on_workspace_contention(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        will_use_tools: bool,
    ) -> Option<SessionIsolation> {
        let workspace_id = workspace_id.as_ref()?;
        if !will_use_tools {
            return None;
        }
        if let Some(existing) = self.session_isolation(session_id) {
            return Some(existing);
        }
        let blocking = self.session_blocking_shared_workspace(session_id, workspace_id)?;
        match self
            .enable_session_isolation(
                session_id,
                workspace_id,
                IsolationOrigin::Contention {
                    blocking_session_id: blocking.as_str().to_string(),
                },
            )
            .await
        {
            Ok(isolation) => Some(isolation),
            Err(error) => {
                tracing::info!(
                    session_id = %session_id,
                    blocking_session_id = %blocking,
                    error = %error.message(),
                    "工作区被其它会话占用，但这一轮无法自动隔离，改为排队等待"
                );
                None
            }
        }
    }
}

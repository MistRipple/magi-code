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
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const ISOLATIONS_FILE: &str = "session-isolations.json";
const ISOLATIONS_DIR: &str = "session-isolations";

impl ApiState {
    /// 单一源目录解析入口：已经进入隔离的会话必须有对应副本，缺失映射和
    /// 不可用副本均拒绝；不能因为旧 session-only 查询返回 None 而写回源目录。
    pub(crate) fn session_execution_root_for_source(
        &self,
        session_id: &SessionId,
        source: &Path,
    ) -> Result<PathBuf, ApiError> {
        let source = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        let Some(isolation) = self
            .session_isolations
            .get_for_root(session_id.as_str(), &source)
        else {
            return if self.session_isolations.contains(session_id.as_str()) {
                Err(ApiError::Conflict(
                    "源文件夹缺少会话隔离映射，不能回落原目录".into(),
                ))
            } else {
                Ok(source)
            };
        };
        let expected_workspace = self
            .session_store
            .session(session_id)
            .and_then(|session| session.workspace_id);
        if expected_workspace.as_deref() != Some(isolation.workspace_id.as_str())
            || !isolation.linked_dirs.is_empty()
            || !isolation.root.is_dir()
            || std::fs::symlink_metadata(&isolation.root)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(ApiError::Conflict(
                "会话原隔离副本不可用、归属变化或仍含旧共享依赖链接，不能回落源目录".into(),
            ));
        }
        Ok(isolation.root)
    }

    pub(crate) fn session_isolation(&self, session_id: &SessionId) -> Option<SessionIsolation> {
        self.session_isolations.get(session_id.as_str())
    }

    /// 持久化隔离登记，daemon 重启后会话继续使用同一个副本。
    pub(crate) fn persist_session_isolations(&self) -> Result<(), ApiError> {
        let Some(persistence) = self.runtime_persistence() else {
            return Ok(());
        };
        let Some(state_root) = persistence.state_root() else {
            return Ok(());
        };
        self.session_isolations.persist_with(|records| {
            persistence.save_json(&state_root.join(ISOLATIONS_FILE), &records)
        })
    }

    pub(crate) fn restore_session_isolations_from(
        &self,
        persistence: &crate::state::RuntimeStatePersistence,
    ) {
        let Some(state_root) = persistence.state_root() else {
            return;
        };
        let path = state_root.join(ISOLATIONS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<SessionIsolation>>(&bytes) {
                Ok(isolations) => {
                    let total = isolations.len();
                    let mut isolations = isolations;
                    let before_migration = isolations.clone();
                    let mut migrated = 0usize;
                    for isolation in &mut isolations {
                        if isolation.linked_dirs.is_empty() {
                            continue;
                        }
                        let managed_root = state_root.join(ISOLATIONS_DIR).canonicalize();
                        let known_source = self
                            .workspace_registry
                            .project(&WorkspaceId::new(&isolation.workspace_id))
                            .is_some_and(|project| {
                                isolation.source_root.canonicalize().is_ok_and(|expected| {
                                    project.source_folder_roots().iter().any(|source| {
                                        source.canonicalize().is_ok_and(|source| source == expected)
                                    })
                                })
                            });
                        if !known_source
                            || !managed_root.ok().is_some_and(|managed| {
                                let mut parts = Path::new(&isolation.session_id).components();
                                if !matches!(parts.next(), Some(std::path::Component::Normal(_)))
                                    || parts.next().is_some()
                                {
                                    return false;
                                }
                                isolation.root.canonicalize().is_ok_and(|root| {
                                    root.parent()
                                        == Some(managed.join(&isolation.session_id).as_path())
                                })
                            })
                            || self
                                .session_store
                                .session(&SessionId::new(&isolation.session_id))
                                .and_then(|session| session.workspace_id)
                                .as_deref()
                                != Some(&isolation.workspace_id)
                        {
                            tracing::warn!(session_id = %isolation.session_id, "旧隔离副本归属无法验证，保留原绑定并拒绝迁移");
                            continue;
                        }
                        match magi_session_isolation::materialize_legacy_shared_directories(
                            isolation,
                        ) {
                            Ok(()) => {
                                isolation.linked_dirs.clear();
                                migrated += 1;
                            }
                            Err(error) => tracing::warn!(
                                session_id = %isolation.session_id,
                                ?error,
                                "旧隔离副本共享目录未能独立迁移，继续拒绝该副本执行"
                            ),
                        }
                    }
                    if migrated > 0 {
                        let saved = persistence.save_json(&path, &isolations);
                        if let Err(error) = saved {
                            tracing::warn!(
                                ?error,
                                "独立目录迁移登记未能持久化，保留旧绑定等待重试"
                            );
                            isolations = before_migration;
                            migrated = 0;
                        }
                    }
                    let unavailable = isolations
                        .iter()
                        .filter(|isolation| !isolation.root.is_dir())
                        .count();
                    self.session_isolations.restore(isolations);
                    tracing::info!(
                        total,
                        unavailable,
                        migrated,
                        "已恢复会话隔离映射；不可用副本保留原绑定并拒绝执行"
                    );
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
        let key = Sha256::digest(
            magi_core::HostPath::from_path(
                std::fs::canonicalize(source_root).unwrap_or_else(|_| source_root.to_path_buf()),
            )
            .to_path_ref()
            .as_str()
            .as_bytes(),
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
        state_root
            .join(ISOLATIONS_DIR)
            .join(session_id.as_str())
            .join(format!("{name}-{key}"))
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

    fn validate_isolation_boundary(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) -> Result<(), ApiError> {
        if let Some(turn_id) = expected_turn_id {
            let turn = self
                .session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)
                .filter(|turn| !turn.status.is_terminal())
                .filter(|_| {
                    self.session_store
                        .runtime_sidecar(session_id)
                        .and_then(|sidecar| sidecar.current_turn)
                        .is_some_and(|current| current.turn_id == turn_id)
                })
                .ok_or_else(|| {
                    ApiError::conflict("隔离准备的目标 Turn 已关闭或变化", session_id.as_str())
                })?;
            if turn.items.iter().any(|item| {
                item.tool.as_ref().is_some_and(|tool| {
                    magi_tool_runtime::BuiltinToolName::from_name(&tool.name).is_none_or(
                        |builtin| {
                            builtin.requires_workspace_execution_preparation(
                                &tool
                                    .arguments
                                    .as_ref()
                                    .map(serde_json::Value::to_string)
                                    .unwrap_or_default(),
                            )
                        },
                    )
                })
            }) {
                return Err(ApiError::conflict(
                    "本轮已开始使用工作区，不能中途切换执行目录",
                    session_id.as_str(),
                ));
            }
            Ok(())
        } else {
            self.session_store
                .ensure_current_turn_acceptance_available(session_id)
                .map_err(|error| match error {
                    DomainError::CurrentTurnConflict { .. } => ApiError::conflict(
                        "会话正在执行，结束后才能切换到隔离副本",
                        session_id.as_str(),
                    ),
                    error => ApiError::internal_assembly("检查会话执行状态失败", error),
                })
        }
    }

    /// 同一次授权的全部目录一起复制、持久提交和发布；失败不暴露不完整的执行映射。
    pub(crate) async fn enable_session_isolation_set_at_boundary(
        &self,
        session_id: &SessionId,
        binding: &magi_core::ProjectDirectoryBinding,
        origin: IsolationOrigin,
        expected_turn_id: Option<&str>,
    ) -> Result<Vec<SessionIsolation>, ApiError> {
        let validate = || {
            if self
                .session_store
                .session(session_id)
                .and_then(|session| session.workspace_id)
                .as_deref()
                != Some(binding.workspace_id.as_str())
            {
                return Err(ApiError::Conflict("会话项目归属已变化".into()));
            }
            self.workspace_registry
                .validate_directory_binding(binding)
                .map_err(|error| ApiError::Conflict(error.to_string()))?;
            self.validate_isolation_boundary(session_id, expected_turn_id)
        };
        validate()?;
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        validate()?;
        let existing = self.session_isolations.for_session(session_id.as_str());
        if !existing.is_empty() {
            let mut selected = Vec::new();
            for grant in &binding.source_folders {
                let source = self
                    .workspace_registry
                    .resolve_bound_source_directory(binding, Some(&grant.source_folder_id))
                    .map_err(|error| ApiError::Conflict(error.to_string()))?;
                self.session_execution_root_for_source(session_id, &source)?;
                selected.push(
                    self.session_isolations
                        .get_for_source(session_id.as_str(), &source)
                        .ok_or_else(|| ApiError::Conflict("隔离会话缺少原授权目录映射".into()))?,
                );
            }
            return Ok(selected);
        }
        for snapshot in self
            .snapshot_manager
            .sessions_for_session(session_id.as_str())
        {
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
        if self
            .session_code_contexts
            .for_session(session_id.as_str())
            .iter()
            .any(|context| {
                context.agent_worktrees.iter().any(|worktree| {
                    worktree.active
                        || (!worktree.applied
                            && (worktree.path.exists() || !worktree.changed_paths.is_empty()))
                })
            })
        {
            return Err(ApiError::conflict(
                "会话还有运行中或未处理的子代理 worktree 成果，先处理后才能切换到隔离副本",
                session_id.as_str(),
            ));
        }

        let mut sources = Vec::new();
        for grant in &binding.source_folders {
            let source = self
                .workspace_registry
                .resolve_bound_source_directory(binding, Some(&grant.source_folder_id))
                .map_err(|error| ApiError::Conflict(error.to_string()))?;
            sources.push((
                source.clone(),
                self.isolation_copy_path(session_id, &source),
            ));
        }
        let session = session_id.to_string();
        let workspace = binding.workspace_id.to_string();
        let copies = tokio::task::spawn_blocking(move || {
            let mut created: Vec<SessionIsolation> = Vec::new();
            for (source_root, destination) in sources {
                let copy = (|| {
                    if let Some(parent) = destination.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| {
                            ApiError::internal_assembly("创建隔离副本父目录失败", error)
                        })?;
                    }
                    let outcome = create_isolated_copy(&source_root, &destination)
                        .map_err(|error| ApiError::internal_assembly("建立隔离副本失败", error))?;
                    Ok::<_, ApiError>(SessionIsolation {
                        session_id: session.clone(),
                        workspace_id: workspace.clone(),
                        source_root,
                        root: destination,
                        origin: origin.clone(),
                        strategy: outcome.strategy,
                        linked_dirs: outcome.linked_dirs,
                        git_available: outcome.git_available,
                        created_at_ms: UtcMillis::now().0,
                    })
                })();
                match copy {
                    Ok(copy) => created.push(copy),
                    Err(error) => {
                        for copy in &created {
                            let _ = std::fs::remove_dir_all(&copy.root);
                        }
                        return Err(error);
                    }
                }
            }
            Ok(created)
        })
        .await
        .map_err(|error| ApiError::internal_assembly("建立整组隔离副本任务失败", error))??;
        let cleanup = || {
            for copy in &copies {
                let _ = std::fs::remove_dir_all(&copy.root);
            }
        };
        // 复制时不持有控制面锁；取消持有提交锁并等待 dispatcher，不能反向等待。
        let turn_guard = self.lock_session_turn_commit(session_id);
        tokio::pin!(turn_guard);
        let _turn_guard = loop {
            tokio::select! {
                guard = &mut turn_guard => break guard,
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    let boundary = if expected_turn_id.is_some() { validate() } else {
                        Err(ApiError::conflict("会话控制状态正在提交，请稍后重试隔离", session_id.as_str()))
                    };
                    if let Err(error) = boundary { cleanup(); return Err(error); }
                }
            }
        };
        if let Err(error) = validate() {
            cleanup();
            return Err(error);
        }
        let persisted = self
            .session_isolations
            .insert_many_with_persistence(&copies, |records| {
                match self.runtime_persistence().and_then(|persistence| {
                    persistence.state_root().map(|root| (persistence, root))
                }) {
                    Some((persistence, root)) => {
                        persistence.save_json(&root.join(ISOLATIONS_FILE), &records)
                    }
                    None => Ok(()),
                }
            });
        if let Err(error) = persisted {
            cleanup();
            return Err(error);
        }
        // 登记成功后保留完整映射；账本准备失败可沿原副本重试，不删除用户已经可见的成果。
        for isolation in &copies {
            self.start_isolated_snapshot_session(session_id, isolation)
                .await?;
            if let Some(context) = self
                .session_code_contexts
                .get_for_path(session_id.as_str(), &isolation.source_root)
            {
                self.release_session_git_execution_lease_for_repository(
                    session_id,
                    &context.git.git_common_dir,
                );
                self.session_code_contexts
                    .remove_for_root(session_id.as_str(), &context.execution_root);
            }
        }
        self.persist_session_git_contexts()?;
        self.publish_isolation_event("session.isolation.changed", session_id, Some(&binding.workspace_id),
            serde_json::json!({ "enabled": true, "source_folder_ids": binding.source_folders.iter().map(|source| &source.source_folder_id).collect::<Vec<_>>() }));
        Ok(copies)
    }

    async fn start_isolated_snapshot_session(
        &self,
        session_id: &SessionId,
        isolation: &SessionIsolation,
    ) -> Result<(), ApiError> {
        // 切换前主工作区上的账本整个替换为副本上的账本（上面已确认没有待处理变更）。
        let _ = self
            .snapshot_manager
            .drop_session_for_workspace(session_id.as_str(), &isolation.source_root)
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
        let isolations = self.session_isolations.for_session(session_id.as_str());
        if isolations.is_empty() {
            return Ok(());
        }
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
        let turn_guard = self.lock_session_turn_commit(session_id);
        tokio::pin!(turn_guard);
        let _turn_guard = tokio::select! {
            guard = &mut turn_guard => guard,
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                return Err(ApiError::conflict("会话控制状态正在提交，请稍后重试取消隔离", session_id.as_str()));
            }
        };
        self.validate_isolation_boundary(session_id, None)?;
        if self
            .session_code_contexts
            .for_session(session_id.as_str())
            .iter()
            .any(|context| {
                context
                    .agent_worktrees
                    .iter()
                    .any(|worktree| worktree.active)
            })
        {
            return Err(ApiError::conflict(
                "会话仍有运行中的子代理，不能丢弃隔离副本",
                session_id.as_str(),
            ));
        }
        for isolation in &isolations {
            self.remove_isolation_resources(session_id, isolation).await;
        }
        self.persist_session_isolations()?;
        let workspace_id = WorkspaceId::new(isolations[0].workspace_id.clone());
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
                self.session_isolations
                    .remove_for_source(&isolation.session_id, &isolation.source_root);
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
        let isolations = self.session_isolations.for_session(session_id.as_str());
        if isolations.is_empty() {
            return;
        }
        for isolation in &isolations {
            self.remove_isolation_resources(session_id, isolation).await;
        }
        if let Err(error) = self.persist_session_isolations() {
            tracing::warn!(session_id = %session_id, ?error, "持久化隔离副本清理失败");
        }
    }

    async fn remove_isolation_resources(
        &self,
        session_id: &SessionId,
        isolation: &SessionIsolation,
    ) {
        self.session_isolations
            .remove_for_source(session_id.as_str(), &isolation.source_root);
        let _ = self
            .snapshot_manager
            .drop_session_for_workspace(session_id.as_str(), &isolation.root)
            .await;
        // 目录里有指回主工作区的符号链接（node_modules 等）；`remove_dir_all` 只删链接本身。
        let session_dir = isolation.root.clone();
        let removal =
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(session_dir)).await;
        if let Ok(Err(error)) = removal
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(session_id = %session_id, %error, "删除隔离副本目录失败");
        }
    }

    fn require_isolation(&self, session_id: &SessionId) -> Result<SessionIsolation, ApiError> {
        self.session_isolation(session_id)
            .ok_or_else(|| ApiError::InvalidInput("会话没有使用隔离副本".to_string()))
    }

    fn require_isolation_for_source(
        &self,
        session_id: &SessionId,
        source: Option<&Path>,
    ) -> Result<SessionIsolation, ApiError> {
        let isolation = match source {
            Some(source) => self
                .session_isolations
                .get_for_root(session_id.as_str(), source)
                .ok_or_else(|| ApiError::InvalidInput("源文件夹没有隔离副本".into()))?,
            None => self.require_isolation(session_id)?,
        };
        let workspace = WorkspaceId::new(&isolation.workspace_id);
        let project = self
            .workspace_registry
            .project(&workspace)
            .ok_or_else(|| ApiError::InvalidInput("隔离副本所属项目已移除".into()))?;
        let source = std::fs::canonicalize(&isolation.source_root)
            .map_err(|error| ApiError::internal_assembly("隔离副本原目录不可用", error))?;
        if !project
            .source_folder_roots()
            .iter()
            .any(|root| std::fs::canonicalize(root).is_ok_and(|root| root == source))
        {
            return Err(ApiError::Conflict("隔离副本源目录已不属于原项目".into()));
        }
        self.session_execution_root_for_source(session_id, &source)?;
        Ok(isolation)
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

    pub(crate) async fn isolation_merge_plan_for_source(
        &self,
        session_id: &SessionId,
        source: Option<&Path>,
    ) -> Result<MergePlan, ApiError> {
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let isolation = self.require_isolation_for_source(session_id, source)?;
        let snapshot = self.isolated_snapshot(session_id, &isolation).await?;
        let source_root = isolation.source_root.clone();
        tokio::task::spawn_blocking(move || plan_merge(&snapshot, &source_root))
            .await
            .map_err(|error| ApiError::internal_assembly("计算合并计划任务失败", error))?
            .map_err(|error| ApiError::internal_assembly("计算合并计划失败", error))
    }

    pub(crate) async fn isolation_merge_apply_for_source(
        &self,
        session_id: &SessionId,
        source: Option<&Path>,
        selection: MergeSelection,
    ) -> Result<MergeOutcome, ApiError> {
        // 执行过程中也允许合并：与共享模式下批准变更一致，长时间运行的目标可以边做边合并。
        let _sync_guard = self.lock_session_change_sync(session_id).await;
        let isolation = self.require_isolation_for_source(session_id, source)?;
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
                "source_root": isolation.source_root,
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
    /// 只在首次工作区工具的准备边界、会话本身还没有隔离、且切换的前置条件满足时才会发生；
    /// 任何一步不满足都返回 `None`，这一轮仍在主工作区运行（由执行租约排队），绝不因此失败。
    pub(crate) async fn isolate_on_workspace_contention(
        &self,
        session_id: &SessionId,
        binding: &magi_core::ProjectDirectoryBinding,
        turn_id: &str,
    ) -> Option<Vec<SessionIsolation>> {
        if self.session_isolations.contains(session_id.as_str()) {
            return Some(self.session_isolations.for_session(session_id.as_str()));
        }
        let blocking = self.session_blocking_shared_workspace(session_id, &binding.workspace_id)?;
        match self
            .enable_session_isolation_set_at_boundary(
                session_id,
                binding,
                IsolationOrigin::Contention {
                    blocking_session_id: blocking.as_str().to_string(),
                },
                Some(turn_id),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_harness::MagiTurnHarness;

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_shared_copy_materializes_during_restore_and_persists_before_execution() {
        let harness = MagiTurnHarness::new_task("legacy copy recovery");
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let source = base.join("source");
        let state_root = base.join("state");
        let copy = state_root.join("session-isolations/legacy-session/copy");
        std::fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
        std::fs::write(source.join("node_modules/pkg/file.js"), "source dependency").unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(copy.join("result.txt"), "unmerged output").unwrap();
        std::os::unix::fs::symlink(source.join("node_modules"), copy.join("node_modules")).unwrap();
        let workspace = WorkspaceId::new("legacy-project");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace.clone(), source.clone())
            .unwrap();
        let session = SessionId::new("legacy-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(session.clone(), "old copy", Some(workspace.to_string()))
            .unwrap();
        let original = SessionIsolation {
            session_id: session.to_string(),
            workspace_id: workspace.to_string(),
            source_root: source.clone(),
            root: copy.clone(),
            origin: IsolationOrigin::Manual,
            strategy: magi_session_isolation::CloneStrategy::Copy,
            linked_dirs: vec!["node_modules".into()],
            git_available: false,
            created_at_ms: 1,
        };
        let file = state_root.join(ISOLATIONS_FILE);
        std::fs::write(&file, serde_json::to_vec(&vec![original.clone()]).unwrap()).unwrap();
        let persistence = std::sync::Arc::new(crate::state::RuntimeStatePersistence::new(
            &state_root,
            state_root.join("workspaces.json"),
            state_root.join("knowledge.json"),
        ));
        let restored = harness
            .state
            .clone()
            .with_runtime_persistence(persistence.clone());
        assert_eq!(
            restored
                .session_execution_root_for_source(&session, &source)
                .unwrap(),
            copy
        );
        let records: Vec<SessionIsolation> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(records[0].linked_dirs.is_empty());
        std::fs::write(copy.join("node_modules/pkg/file.js"), "copy dependency").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("node_modules/pkg/file.js")).unwrap(),
            "source dependency"
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("result.txt")).unwrap(),
            "unmerged output"
        );
        let restarted = restored.with_runtime_persistence(persistence.clone());
        assert!(
            restarted
                .session_isolations
                .get(session.as_str())
                .unwrap()
                .linked_dirs
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("node_modules/pkg/file.js")).unwrap(),
            "copy dependency"
        );
        let outside = base.join("outside-copy");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(source.join("node_modules"), outside.join("node_modules"))
            .unwrap();
        let mut invalid = original;
        invalid.root = outside.clone();
        std::fs::write(&file, serde_json::to_vec(&vec![invalid]).unwrap()).unwrap();
        restarted.restore_session_isolations_from(&persistence);
        assert!(
            restarted
                .session_execution_root_for_source(&session, &source)
                .is_err()
        );
        assert!(
            std::fs::symlink_metadata(outside.join("node_modules"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn project_isolation_set_is_atomic_persistent_and_merges_each_same_name_root() {
        let harness = MagiTurnHarness::new_task("multi isolation");
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().canonicalize().unwrap();
        let sources = [parent.join("a/shared"), parent.join("b/shared")];
        for (index, source) in sources.iter().enumerate() {
            std::fs::create_dir_all(source).unwrap();
            std::fs::write(source.join("same.txt"), format!("original {index}")).unwrap();
        }
        let workspace = WorkspaceId::new("isolation-set");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace.clone(), sources[0].clone())
            .unwrap();
        crate::routes::test_project_fixtures::add_source(
            &harness.state,
            workspace.as_str(),
            "second",
            &sources[1],
        );
        let session = SessionId::new("isolation-set-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(session.clone(), "multi", Some(workspace.to_string()))
            .unwrap();
        let binding = harness
            .state
            .workspace_registry
            .capture_directory_binding(&workspace, None, None)
            .unwrap();
        let destinations = sources
            .iter()
            .map(|source| harness.state.isolation_copy_path(&session, source))
            .collect::<Vec<_>>();
        assert_ne!(destinations[0], destinations[1], "同名来源不能共用副本路径");
        std::fs::create_dir_all(&destinations[1]).unwrap();
        std::fs::write(destinations[1].join("user.txt"), "keep").unwrap();
        assert!(
            harness
                .state
                .enable_session_isolation_set_at_boundary(
                    &session,
                    &binding,
                    IsolationOrigin::Manual,
                    None
                )
                .await
                .is_err()
        );
        assert!(!harness.state.session_isolations.contains(session.as_str()));
        assert!(
            !destinations[0].exists(),
            "第二目录创建失败必须清理本次第一目录"
        );
        assert_eq!(
            std::fs::read_to_string(destinations[1].join("user.txt")).unwrap(),
            "keep"
        );
        std::fs::remove_dir_all(&destinations[1]).unwrap();

        let state_root = harness
            .state
            .runtime_persistence()
            .unwrap()
            .state_root()
            .unwrap();
        let registry = state_root.join(ISOLATIONS_FILE);
        std::fs::create_dir(&registry).unwrap();
        assert!(
            harness
                .state
                .enable_session_isolation_set_at_boundary(
                    &session,
                    &binding,
                    IsolationOrigin::Manual,
                    None
                )
                .await
                .is_err()
        );
        assert!(!harness.state.session_isolations.contains(session.as_str()));
        assert!(
            destinations.iter().all(|destination| !destination.exists()),
            "登记写盘失败不能发布或保留自有半成品"
        );
        std::fs::remove_dir(&registry).unwrap();

        let copies = harness
            .state
            .enable_session_isolation_set_at_boundary(
                &session,
                &binding,
                IsolationOrigin::Manual,
                None,
            )
            .await
            .unwrap();
        assert_eq!(copies.len(), 2);
        let durable: Vec<SessionIsolation> =
            serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        assert_eq!(durable.len(), 2);
        harness.state.session_isolations.restore(durable);
        for (index, source) in sources.iter().enumerate() {
            let execution = harness
                .state
                .session_execution_root_for_source(&session, source)
                .unwrap();
            std::fs::write(execution.join("same.txt"), format!("changed {index}")).unwrap();
        }
        assert!(
            harness
                .state
                .isolation_merge_plan_for_source(&session, None)
                .await
                .is_err(),
            "无目录身份的旧合入入口不能任取一根"
        );
        let plan = harness
            .state
            .isolation_merge_plan_for_source(&session, Some(&sources[0]))
            .await
            .unwrap();
        assert_eq!(plan.entries.len(), 1);
        let merged = harness
            .state
            .isolation_merge_apply_for_source(
                &session,
                Some(&sources[0]),
                MergeSelection::default(),
            )
            .await
            .unwrap();
        assert_eq!(merged.applied, vec!["same.txt"]);
        assert_eq!(
            std::fs::read_to_string(sources[0].join("same.txt")).unwrap(),
            "changed 0"
        );
        assert_eq!(
            std::fs::read_to_string(sources[1].join("same.txt")).unwrap(),
            "original 1"
        );
        std::fs::write(sources[1].join("same.txt"), "concurrent user").unwrap();
        let conflict = harness
            .state
            .isolation_merge_apply_for_source(
                &session,
                Some(&sources[1]),
                MergeSelection::default(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.unresolved, vec!["same.txt"]);
        assert_eq!(
            std::fs::read_to_string(sources[1].join("same.txt")).unwrap(),
            "concurrent user"
        );
        assert!(
            copies.iter().all(|copy| copy.root.is_dir()),
            "部分合入保留所有未采用成果"
        );
        harness.state.cleanup_session_isolation(&session).await;
    }

    #[tokio::test]
    async fn multi_source_isolation_resolution_restart_and_cleanup_keep_other_copies() {
        use crate::state::RuntimeStatePersistence;
        use magi_event_bus::InMemoryEventBus;
        use magi_governance::GovernanceService;
        use magi_session_store::SessionStore;
        use magi_workspace::WorkspaceStore;
        use std::sync::Arc;
        let source_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let sources = [
            source_dir.path().join("a/shared"),
            source_dir.path().join("b/shared"),
        ];
        let copies = [
            state_dir.path().join("session-isolations/session/a"),
            state_dir.path().join("session-isolations/session/b"),
        ];
        for source in &sources {
            std::fs::create_dir_all(source).unwrap();
            std::fs::write(source.join("same.txt"), "original").unwrap();
        }
        let sessions = Arc::new(SessionStore::new());
        let projects = Arc::new(WorkspaceStore::new());
        let session = SessionId::new("session");
        let workspace = WorkspaceId::new("multi-isolation");
        projects
            .register_native_path(workspace.clone(), sources[0].clone())
            .unwrap();
        sessions
            .create_session_for_workspace(session.clone(), "multi", Some(workspace.to_string()))
            .unwrap();
        let make_state = || {
            ApiState::new(
                "test",
                Arc::new(InMemoryEventBus::new(32)),
                sessions.clone(),
                projects.clone(),
                Arc::new(GovernanceService::default()),
            )
            .with_snapshot_manager(Arc::new(
                crate::snapshot_lifecycle::managed_snapshot_manager(
                    state_dir.path().into(),
                    sessions.clone(),
                ),
            ))
            .with_runtime_persistence(Arc::new(RuntimeStatePersistence::new(
                state_dir.path(),
                state_dir.path().join("workspaces.json"),
                state_dir.path().join("knowledge.json"),
            )))
        };
        let state = make_state();
        let project = crate::routes::test_project_fixtures::add_source(
            &state,
            workspace.as_str(),
            "second",
            &sources[1],
        );
        for (source, copy) in sources.iter().zip(&copies) {
            std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
            let outcome = create_isolated_copy(source, copy).unwrap();
            state.session_isolations.insert(SessionIsolation {
                session_id: session.to_string(),
                workspace_id: workspace.to_string(),
                source_root: std::fs::canonicalize(source).unwrap(),
                root: std::fs::canonicalize(copy).unwrap(),
                origin: IsolationOrigin::Manual,
                strategy: outcome.strategy,
                linked_dirs: outcome.linked_dirs,
                git_available: outcome.git_available,
                created_at_ms: 1,
            });
            state
                .ensure_snapshot_session(&session, source)
                .await
                .unwrap();
        }
        assert!(
            state.session_isolation(&session).is_none(),
            "旧单根入口不得选一份副本"
        );
        std::fs::write(copies[1].join("same.txt"), "second-copy").unwrap();
        for (id, copy) in [project.primary_source_folder_id.as_str(), "second"]
            .into_iter()
            .zip(&copies)
        {
            let resolved = crate::scope_binding::resolve_project_source_directory(
                &state,
                &workspace,
                Some(&session),
                Some(id),
            )
            .unwrap();
            assert_eq!(
                resolved.execution_root,
                std::fs::canonicalize(copy).unwrap()
            );
        }
        state.persist_session_isolations().unwrap();
        state
            .snapshot_manager
            .archive_session(session.as_str())
            .await
            .unwrap();
        let restarted = make_state();
        assert_eq!(
            restarted
                .session_isolations
                .for_session(session.as_str())
                .len(),
            2
        );
        let snapshot = restarted
            .ensure_snapshot_session(&session, &sources[1])
            .await
            .unwrap();
        snapshot.reconcile().unwrap();
        let pending = snapshot.pending_changes().unwrap();
        assert_eq!(pending[0].original_content.as_deref(), Some("original"));
        assert_eq!(pending[0].preview_content.as_deref(), Some("second-copy"));
        #[cfg(unix)]
        {
            let retained = copies[1].with_extension("retained");
            std::fs::rename(&copies[1], &retained).unwrap();
            std::os::unix::fs::symlink(&sources[1], &copies[1]).unwrap();
            assert!(
                restarted
                    .session_execution_root_for_source(&session, &sources[1])
                    .is_err(),
                "副本被替换为源目录链接后不能继续写回源目录"
            );
            std::fs::remove_file(&copies[1]).unwrap();
            std::fs::rename(retained, &copies[1]).unwrap();
        }
        let first = restarted
            .session_isolations
            .get_for_source(session.as_str(), &sources[0])
            .unwrap();
        restarted.remove_isolation_resources(&session, &first).await;
        assert!(!copies[0].exists());
        assert!(copies[1].exists(), "删除一个源副本不能删除同会话的其它副本");
        assert!(
            restarted
                .session_execution_root_for_source(&session, &sources[0])
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(sources[1].join("same.txt")).unwrap(),
            "original"
        );
        let second = restarted
            .session_isolations
            .get_for_source(session.as_str(), &sources[1])
            .unwrap();
        restarted
            .remove_isolation_resources(&session, &second)
            .await;
        assert!(!copies[1].exists());
        assert!(!restarted.session_isolations.contains(session.as_str()));
    }

    #[tokio::test]
    async fn cancelled_isolation_preparation_exits_while_cancellation_holds_the_turn_lock() {
        let harness = MagiTurnHarness::new_task("不会执行");
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "base").unwrap();
        let workspace = WorkspaceId::new("cancel-isolation-workspace");
        harness
            .state
            .workspace_registry
            .register_native_path(workspace.clone(), root.path().to_path_buf())
            .unwrap();
        let session = SessionId::new("cancel-isolation-session");
        harness
            .state
            .session_store
            .create_session_for_workspace(session.clone(), "取消隔离", Some(workspace.to_string()))
            .unwrap();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &harness.state.session_store,
            harness.state.turn_coordinator(),
            &session,
            "cancel-isolation-turn",
            1,
            UtcMillis::now(),
            "running",
            "准备资源",
        );
        let guard = harness.state.lock_session_turn_commit(&session).await;
        let destination = harness.state.isolation_copy_path(&session, root.path());
        let state = harness.state.clone();
        let target = session.clone();
        let preparation = tokio::spawn(async move {
            state
                .enable_session_isolation_set_at_boundary(
                    &target,
                    &state
                        .workspace_registry
                        .capture_directory_binding(&workspace, None, None)
                        .unwrap(),
                    IsolationOrigin::Contention {
                        blocking_session_id: "busy".into(),
                    },
                    Some("cancel-isolation-turn"),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !destination.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("隔离副本应已开始复制");
        harness
            .state
            .turn_event_sink()
            .interrupt_turn_by_user(&session)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), preparation)
            .await
            .expect("取消持锁等待资源回收时，资源准备不能反向等待该锁")
            .unwrap();
        assert!(result.is_err());
        assert!(harness.state.session_isolation(&session).is_none());
        assert!(!destination.exists());
        drop(guard);
    }
}

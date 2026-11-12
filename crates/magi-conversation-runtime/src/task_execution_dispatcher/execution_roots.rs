use super::*;

impl LlmTaskDispatcher {
    pub(super) fn resolve_task_source_root(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        if let Some(binding) = task
            .executor_binding
            .as_ref()
            .and_then(|executor| executor.project_directories.as_ref())
        {
            return binding
                .grant(None)
                .ok()
                .and_then(|grant| magi_core::HostPath::from_path_ref(&grant.source_path_ref).ok())
                .map(magi_core::HostPath::into_path_buf);
        }
        workspace_id
            .as_ref()
            .and_then(|_| self.resolve_workspace_root_path(session_id, workspace_id))
    }

    pub(super) fn resolve_workspace_root_path(
        &self,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
    ) -> Option<PathBuf> {
        let source_root = workspace_id.as_ref().and_then(|workspace_id| {
            self.workspace_registry
                .as_ref()?
                .resolve_source_folder(workspace_id, None)
                .ok()
                .map(|(_, root)| root)
        });
        if workspace_id.is_some() && self.workspace_registry.is_some() && source_root.is_none() {
            return None;
        }
        if let Some(context) =
            self.session_code_contexts
                .as_ref()
                .and_then(|registry| match source_root.as_deref() {
                    Some(root) => registry.get_for_path(session_id.as_str(), root),
                    None => registry.get(session_id.as_str()),
                })
        {
            return Some(context.execution_root);
        }
        source_root
    }

    /// 子代理永不直接复用主会话 live worktree：只读任务拿 detached worktree，
    /// 可写任务拿唯一临时 branch + 独立 worktree。两者都从直接父任务当前工作树快照出发。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_task_execution_root(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        execution_root: Option<&PathBuf>,
        is_sidechain: bool,
        worker_id: Option<&WorkerId>,
    ) -> Result<Option<PathBuf>, String> {
        let main_root = execution_root
            .cloned()
            .or_else(|| self.resolve_task_source_root(task, session_id, workspace_id));
        if workspace_id.is_some() && main_root.is_none() {
            return Err("项目执行缺少明确源文件夹绑定，不能选择任意仓库".into());
        }
        if !is_sidechain {
            return Ok(main_root);
        }
        // 个人会话没有项目 Git 注册表；子代理和显式转交沿原执行计划继承目录。
        if workspace_id.is_none() && main_root.is_some() {
            return Ok(main_root);
        }
        let context = self.session_code_contexts.as_ref().and_then(|registry| {
            main_root
                .as_deref()
                .and_then(|root| registry.get_for_path(session_id.as_str(), root))
        });
        if let Some(context) = context.as_ref()
            && let Some(coordinator) = self.workspace_git_coordinator.as_ref()
        {
            coordinator
                .begin_execution(session_id.as_str(), &context.git.git_common_dir)
                .map_err(|error| error.to_string())?;
        }
        let Some(registry) = self.session_code_contexts.as_ref() else {
            if task.agent_run_transfer().is_some() {
                return Err("转交代理缺少原执行目录注册表，不能恢复本地执行".into());
            }
            return Ok(main_root);
        };
        let Some(context) = context else {
            if task.agent_run_transfer().is_some() {
                return Err("转交代理缺少原执行目录上下文，不能回退到主线目录".into());
            }
            // 非 Git workspace 没有 SessionGitContext，只能沿用普通 workspace 语义。
            return Ok(main_root);
        };
        let git_service = self
            .git_service
            .as_ref()
            .ok_or_else(|| "Git service 未注入，无法隔离子代理".to_string())?;
        if let Some(existing) = context.agent_worktrees.iter().find(|worktree| {
            worktree.task_id == task.task_id.as_str()
                && worktree.active
                && worktree.lease_id.as_deref() == Some(lease_id.as_str())
        }) {
            if existing.path.is_dir() {
                return Ok(Some(existing.path.clone()));
            }
            return Err(format!(
                "执行租约 {} 对应的 agent worktree 不可用",
                lease_id
            ));
        }
        if task.agent_activation().is_some() {
            // daemon 重启会撤销旧 lease，但保留 Agent 的 worktree 目录。恢复时
            // 必须复用这份原目录，并验证它仍属于同一 repository；不能从主线 HEAD
            // 重新创建一份看似干净的副本，悄悄丢掉代理留下的文件和分支。
            let previous_allocations = context
                .agent_worktrees
                .iter()
                .filter(|worktree| worktree.task_id == task.task_id.as_str())
                .collect::<Vec<_>>();
            if previous_allocations.len() > 1 {
                return Err("代理存在多份历史 worktree，不能猜测恢复来源".into());
            }
            if let Some(previous) = previous_allocations.first().copied() {
                let worker = worker_id.ok_or("恢复代理 worktree 缺少当前 Worker")?;
                let store = self.pipeline.task_store.as_ref();
                let current = store
                    .get_active_lease(&task.task_id)
                    .filter(|lease| &lease.lease_id == lease_id)
                    .ok_or("恢复代理 worktree 当前租约已失效")?;
                if previous.active
                    || current.root_task_id != task.root_task_id
                    || previous.worker_id != worker.as_str()
                {
                    return Err("代理原 worktree 仍占用或当前租约归属变化".into());
                }
                previous
                    .lease_id
                    .as_deref()
                    .and_then(|id| {
                        self.pipeline
                            .task_store
                            .as_ref()
                            .get_lease(&LeaseId::new(id))
                    })
                    .filter(|lease| {
                        lease.task_id == task.task_id
                            && lease.root_task_id == task.root_task_id
                            && lease.role == current.role
                            && lease.lease_status
                                != magi_orchestrator::task_store::TaskLeaseState::Active
                    })
                    .ok_or("原代理 worktree 缺少已结算的来源租约，拒绝恢复")?;
                let mode = if task
                    .policy_snapshot
                    .as_ref()
                    .map(magi_core::TaskPolicy::effective_access_profile)
                    .unwrap_or_default()
                    == AccessProfile::ReadOnly
                {
                    magi_git::AgentWorktreeMode::ReadOnly
                } else {
                    magi_git::AgentWorktreeMode::Writable
                };
                if mode != previous.mode {
                    return Err("代理恢复工具范围与原 worktree 模式不一致".into());
                }
                block_on_git(git_service.recover_agent_worktree(
                    &context.git.worktree_path,
                    &context.git.git_common_dir,
                    previous,
                ))
                .map_err(|error| format!("恢复代理 worktree 失败: {error}"))?;
                registry
                    .reattach_agent_worktree_for_root(
                        session_id.as_str(),
                        &context.execution_root,
                        previous,
                        lease_id.as_str(),
                        worker.as_str(),
                    )
                    .map_err(|error| format!("恢复代理 worktree 记录失败: {error}"))?;
                if let Some(persist) = self.session_state_persist_callback.as_deref()
                    && let Err(error) = persist("agent_worktree_reattached")
                {
                    let _ = registry.release_agent_worktree_for_lease_for_root(
                        session_id.as_str(),
                        &context.execution_root,
                        task.task_id.as_str(),
                        lease_id.as_str(),
                    );
                    return Err(format!("恢复代理 worktree 保存失败: {error}"));
                }
                return Ok(Some(previous.path.clone()));
            }
            let current_turn = self.execution_registry.turn_id(&task.task_id);
            let used_local_tools = self
                .session_store
                .canonical_turns_for_session(session_id)
                .iter()
                .filter(|turn| Some(turn.turn_id.as_str()) != current_turn.as_deref())
                .flat_map(|turn| &turn.items)
                .filter(|item| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        == Some(&task.task_id)
                })
                .filter_map(|item| item.tool.as_ref())
                .any(|tool| {
                    tool_requires_execution_resources(
                        &tool.name,
                        &tool
                            .arguments
                            .as_ref()
                            .map(serde_json::Value::to_string)
                            .unwrap_or_else(|| "{}".into()),
                        true,
                    )
                });
            if task.agent_run_transfer().is_some() || used_local_tools {
                return Err("代理缺少原本地执行 worktree 记录，拒绝从当前主线重建基线".into());
            }
        }
        if context.has_external_drift() {
            return Err(format!(
                "session Git context 已漂移，拒绝为子代理 {} 分配 worktree",
                task.task_id
            ));
        }
        let Some(base_head) = context.git.base_head.clone() else {
            return Err("当前 Git session 没有 base HEAD，无法隔离子代理".to_string());
        };
        let workspace_key = workspace_id
            .as_ref()
            .map(|value| sanitize_git_path_component(value.as_str()))
            .unwrap_or_else(|| "workspace".to_string());
        let session_key = sanitize_git_path_component(session_id.as_str());
        let task_key = sanitize_git_path_component(task.task_id.as_str());
        let allocation_nonce = UtcMillis::now().0;
        use sha2::{Digest, Sha256};
        let repository_key = magi_core::HostPath::from_path(context.execution_root.clone());
        let repository_key = magi_core::hex_lower(Sha256::digest(
            repository_key.to_path_ref().as_str().as_bytes(),
        ));
        let path = self
            .agent_worktree_root
            .join(workspace_key)
            .join(session_key)
            .join(&repository_key)
            .join(format!("{task_key}-{allocation_nonce}"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建 agent worktree 父目录失败: {error}"))?;
        }
        let access_profile = task
            .policy_snapshot
            .as_ref()
            .map(magi_core::TaskPolicy::effective_access_profile)
            .unwrap_or_default();
        let mode = if access_profile == AccessProfile::ReadOnly {
            magi_git::AgentWorktreeMode::ReadOnly
        } else {
            magi_git::AgentWorktreeMode::Writable
        };
        let branch = (mode == magi_git::AgentWorktreeMode::Writable).then(|| {
            format!(
                "magi/agent/{task_key}-{}-{allocation_nonce}",
                &repository_key[..12]
            )
        });
        // 首次执行时读取直接父代理当前 worktree，含其尚未提交改动；主线不共享子代理 index。
        let source = task.parent_task_id.as_ref().and_then(|id| {
            context
                .agent_worktrees
                .iter()
                .find(|worktree| worktree.task_id == id.as_str())
        });
        if (task
            .parent_task_id
            .as_ref()
            .is_some_and(|id| id != &task.root_task_id)
            && source.is_none())
            || source.is_some_and(|source| !source.active || !source.path.is_dir())
        {
            return Err("父代理执行资源已释放，拒绝建立嵌套 worktree".into());
        }
        if let Some(source) = source {
            let store = self.pipeline.task_store.as_ref();
            let current_parent = task
                .parent_task_id
                .as_ref()
                .and_then(|id| store.get_task(id));
            if current_parent.as_ref().is_none_or(|parent| {
                parent.status != magi_core::TaskStatus::Running
                    || parent.root_task_id != task.root_task_id
                    || parent.mission_id != task.mission_id
                    || store.get_active_lease(&parent.task_id).is_none_or(|lease| {
                        source.lease_id.as_deref() != Some(lease.lease_id.as_str())
                    })
            }) {
                return Err("父代理执行租约已失效，拒绝使用旧 worktree 建立快照".into());
            }
        }
        let source_path = source.map_or(context.git.worktree_path.as_path(), |source| {
            source.path.as_path()
        });
        let source_head = source.map_or(base_head.as_str(), |source| source.base_head.as_str());
        let base_head = block_on_git(git_service.snapshot_worktree(source_path, source_head))
            .map_err(|error| format!("生成子代理基线快照失败: {error}"))?;
        let created = block_on_git(git_service.worktree_create(
            &context.git.worktree_path,
            magi_git::WorktreeCreateOptions {
                path: path.clone(),
                base: base_head.clone(),
                branch: branch.clone(),
                create_branch: branch.is_some(),
                detached: branch.is_none(),
                precondition: context.precondition(),
            },
        ))
        .map_err(|error| format!("创建 agent 隔离 worktree 失败: {error}"))?;
        registry
            .register_agent_worktree_for_root(
                session_id.as_str(),
                &context.execution_root,
                magi_git::AgentWorktreeContext {
                    task_id: task.task_id.to_string(),
                    lease_id: Some(lease_id.to_string()),
                    worker_id: worker_id.map(ToString::to_string).unwrap_or_default(),
                    path: created.path.clone(),
                    mode,
                    base_head: base_head.clone(),
                    branch: created.branch.clone(),
                    active: true,
                    result_head: None,
                    changed_paths: Vec::new(),
                    applied: false,
                },
            )
            .map_err(|error| error.to_string())?;
        if let Some(persist) = self.session_state_persist_callback.as_deref() {
            persist("agent_worktree_allocated")?;
        }
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "agent-worktree-allocated-{}-{}",
                    task.task_id, allocation_nonce
                )),
                "agent.git.worktree.allocated",
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "task_id": task.task_id,
                    "worker_id": worker_id,
                    "mode": mode,
                    "base_head": base_head,
                    "branch": created.branch,
                    "worktree_path": created.path,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
        Ok(Some(path))
    }

    /// TaskPolicy 持久化的是逻辑 workspace 边界；子代理进入隔离 worktree 后，
    /// 同一边界必须投影到当前执行实例的物理根目录。这里只替换 workspace 内路径，
    /// 显式授权的外部引用路径保持不变，避免把整个 agent worktree 上级目录放入白名单。
    pub(super) fn task_for_execution_root(
        task: &magi_core::Task,
        workspace_identity_root: Option<&PathBuf>,
        execution_root: Option<&PathBuf>,
    ) -> magi_core::Task {
        let mut projected = task.clone();
        let (Some(identity_root), Some(execution_root), Some(policy)) = (
            workspace_identity_root,
            execution_root,
            projected.policy_snapshot.as_mut(),
        ) else {
            return projected;
        };
        if identity_root == execution_root {
            return projected;
        }

        policy.allowed_paths = project_policy_paths_to_execution_root(
            &policy.allowed_paths,
            identity_root,
            execution_root,
        );
        policy.denied_paths = project_policy_paths_to_execution_root(
            &policy.denied_paths,
            identity_root,
            execution_root,
        );
        policy.read_only_paths = project_policy_paths_to_execution_root(
            &policy.read_only_paths,
            identity_root,
            execution_root,
        );
        projected
    }

    /// 子代理模型调用结束后立即结束 worktree 的 active 生命周期。
    ///
    /// 可写 worktree 中的改动由运行时提交到代理分支并记录产出（分支、提交、改动路径），
    /// 随后安全移除目录；分支保留，供直接父任务通过 agent_apply 应用。只读 worktree 干净时
    /// 移除；存在未提交改动或安全移除失败时保留目录与分配记录，但标记为 inactive，
    /// 避免后续任务误认为仍由 worker 占用。绝不使用 force。
    ///
    /// 返回可写代理的产出描述，由调用方写入子任务终态的 output_refs。
    pub(super) fn finalize_agent_worktree(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        is_sidechain: bool,
    ) -> Option<serde_json::Value> {
        if !is_sidechain {
            return None;
        }
        let registry = self.session_code_contexts.as_ref()?;
        let contexts = registry.for_session(session_id.as_str());
        let mut outputs = Vec::new();
        for context in contexts {
            if let Some(output) = self.finalize_agent_worktree_for_context(
                task,
                lease_id,
                session_id,
                workspace_id,
                &context,
            ) {
                outputs.push(output);
            }
        }
        match outputs.len() {
            0 => None,
            1 => outputs.pop(),
            _ => Some(serde_json::json!({
                "agent_workspaces": outputs.into_iter().map(|output| output["agent_workspace"].clone()).collect::<Vec<_>>()
            })),
        }
    }

    pub(super) fn finalize_agent_worktree_for_context(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context: &magi_git::SessionCodeContext,
    ) -> Option<serde_json::Value> {
        let registry = self.session_code_contexts.as_ref()?;
        let git_service = self.git_service.as_ref()?;

        let allocation = context
            .agent_worktrees
            .iter()
            .find(|worktree| {
                worktree.task_id == task.task_id.as_str()
                    && worktree.active
                    && worktree.lease_id.as_deref() == Some(lease_id.as_str())
            })
            .cloned()?;

        let mut workspace_output = None;
        let mut commit_error = None;
        if allocation.mode == magi_git::AgentWorktreeMode::Writable && allocation.path.exists() {
            match self.commit_agent_worktree_output(
                task,
                lease_id,
                session_id,
                &context.execution_root,
                &allocation,
            ) {
                Ok(output) => workspace_output = output,
                Err(error) => commit_error = Some(error),
            }
        }
        let (cleanup_status, retained, cleanup_error) = if !allocation.path.exists() {
            ("already_missing", false, None)
        } else if let Some(error) = commit_error {
            ("retained_commit_failed", true, Some(error))
        } else {
            match block_on_git(git_service.observe(&allocation.path)) {
                Ok(observation) if observation.dirty.has_uncommitted => (
                    "retained_dirty",
                    true,
                    Some(format!(
                        "子代理 worktree 含未提交改动：staged={} unstaged={} untracked={} conflicted={}",
                        observation.dirty.staged,
                        observation.dirty.unstaged,
                        observation.dirty.untracked,
                        observation.dirty.conflicted_paths.len()
                    )),
                ),
                Ok(observation) => match (|| -> Result<(), String> {
                    if allocation.mode == magi_git::AgentWorktreeMode::ReadOnly {
                        registry
                            .record_agent_worktree_result_for_root(
                                session_id.as_str(),
                                &context.execution_root,
                                task.task_id.as_str(),
                                lease_id.as_str(),
                                observation.head.ok_or("只读代理目录缺少 HEAD，不能清理")?,
                                vec![],
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    // 删除目录前先保存可恢复 commit；不能只在删除后的异步进度
                    // checkpoint 里保存，否则崩溃会同时丢失目录和恢复证据。
                    if let Some(persist) = self.session_state_persist_callback.as_deref() {
                        persist("agent_worktree_result_saved")?;
                    }
                    block_on_git(git_service.worktree_remove(
                        &context.git.worktree_path,
                        magi_git::WorktreeRemoveOptions {
                            path: allocation.path.clone(),
                            force: false,
                            confirm_force: false,
                            precondition: context.precondition(),
                        },
                    ))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                })() {
                    Ok(_) => ("removed", false, None),
                    Err(error) => ("retained_cleanup_failed", true, Some(error)),
                },
                Err(error) => ("retained_observation_failed", true, Some(error.to_string())),
            }
        };

        if let Err(error) = registry.release_agent_worktree_for_lease_for_root(
            session_id.as_str(),
            &context.execution_root,
            task.task_id.as_str(),
            lease_id.as_str(),
        ) {
            tracing::warn!(
                session_id = %session_id,
                task_id = %task.task_id,
                %error,
                "结束 agent worktree 生命周期失败"
            );
            return workspace_output;
        }
        if let Some(persist) = self.session_state_persist_callback.as_deref()
            && let Err(error) = persist("agent_worktree_released")
        {
            tracing::error!(
                session_id = %session_id,
                task_id = %task.task_id,
                %error,
                "agent worktree 生命周期状态持久化失败"
            );
        }
        if retained {
            tracing::warn!(
                session_id = %session_id,
                task_id = %task.task_id,
                worktree_path = %allocation.path.display(),
                cleanup_status,
                cleanup_error = cleanup_error.as_deref().unwrap_or_default(),
                "子代理 worktree 已结束执行但保留目录"
            );
        }
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::unique(format!("agent-worktree-released-{}", task.task_id)),
                "agent.git.worktree.released",
                serde_json::json!({
                    "session_id": session_id,
                    "workspace_id": workspace_id,
                    "task_id": task.task_id,
                    "lease_id": lease_id,
                    "worker_id": allocation.worker_id,
                    "mode": allocation.mode,
                    "base_head": allocation.base_head,
                    "branch": allocation.branch,
                    "worktree_path": allocation.path,
                    "cleanup_status": cleanup_status,
                    "retained": retained,
                    "cleanup_error": cleanup_error,
                }),
            )
            .with_context(EventContext {
                session_id: Some(session_id.clone()),
                workspace_id: workspace_id.clone(),
                mission_id: Some(task.mission_id.clone()),
                task_id: Some(task.task_id.clone()),
                ..EventContext::default()
            }),
        );
        workspace_output
    }

    /// 把可写代理 worktree 的改动提交到代理分支并记录产出；没有任何改动时返回 `None`。
    pub(super) fn commit_agent_worktree_output(
        &self,
        task: &magi_core::Task,
        lease_id: &LeaseId,
        session_id: &SessionId,
        repository_root: &Path,
        allocation: &magi_git::AgentWorktreeContext,
    ) -> Result<Option<serde_json::Value>, String> {
        let (Some(registry), Some(git_service)) = (
            self.session_code_contexts.as_ref(),
            self.git_service.as_ref(),
        ) else {
            return Ok(None);
        };
        block_on_git(git_service.commit_all(
            &allocation.path,
            &format!("magi agent: {}", task.title.trim()),
        ))
        .map_err(|error| format!("提交子代理改动失败: {error}"))?;
        let head = block_on_git(git_service.observe_ref(&allocation.path))
            .map_err(|error| format!("读取子代理分支 HEAD 失败: {error}"))?
            .head
            .ok_or_else(|| "子代理分支没有 HEAD".to_string())?;
        let changed_paths =
            block_on_git(git_service.changed_paths(&allocation.path, &allocation.base_head, &head))
                .map_err(|error| format!("统计子代理改动失败: {error}"))?;
        registry
            .record_agent_worktree_result_for_root(
                session_id.as_str(),
                repository_root,
                task.task_id.as_str(),
                lease_id.as_str(),
                head.clone(),
                changed_paths.clone(),
            )
            .map_err(|error| error.to_string())?;
        if changed_paths.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::json!({
            "agent_workspace": {
                "repository_root": repository_root,
                "branch": allocation.branch,
                "base": allocation.base_head,
                "head": head,
                "changed_paths": changed_paths,
            }
        })))
    }
}

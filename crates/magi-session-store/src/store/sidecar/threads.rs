use super::*;

impl SessionStore {
    /// 注册新 thread；重复的 `thread_id` 表示恢复/装配状态冲突，必须显式失败。
    pub fn register_thread(&self, thread: ExecutionThread) -> DomainResult<()> {
        let session_id = thread.session_id.clone();
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            if state
                .thread_registry
                .iter()
                .any(|existing| existing.thread_id == thread.thread_id)
            {
                return Err(DomainError::InvalidState {
                    message: format!("thread {} 已注册，拒绝重复注册", thread.thread_id),
                });
            }
            state.thread_registry.push(thread);
        }
        self.mark_sidecar_dirty_for_session(&session_id, SessionSidecarFlushReason::RegisterThread);
        Ok(())
    }

    /// 只删除本次恢复创建的 worker thread。
    ///
    /// 返回 `None` 表示该 thread 已经被同一回滚流程删除；如果 thread 仍存在但
    /// 任一归属字段不匹配，则返回错误，禁止把其他执行链的历史 thread 当成恢复产物删除。
    pub fn remove_recovery_thread_if_owned(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        mission_id: &MissionId,
        task_id: &TaskId,
        worker_id: &WorkerId,
    ) -> DomainResult<Option<ExecutionThread>> {
        let removed = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(index) = state
                .thread_registry
                .iter()
                .position(|thread| &thread.thread_id == thread_id)
            else {
                return Ok(None);
            };
            let thread = &state.thread_registry[index];
            if thread.session_id != *session_id
                || thread.mission_id != *mission_id
                || thread.worker_instance_id != *worker_id
                || thread.role_id == ORCHESTRATOR_ROLE_ID
                || !thread
                    .handled_task_ids
                    .iter()
                    .any(|handled| handled == task_id)
            {
                return Err(DomainError::InvalidState {
                    message: format!("thread {} 不属于本次恢复创建的任务 {}", thread_id, task_id),
                });
            }
            state.thread_registry.remove(index)
        };
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RemoveThread);
        Ok(Some(removed))
    }

    /// 仅在 thread 仍保持指定快照时删除它，并同步删除其上下文检查点。
    ///
    /// materialize 失败时只能回收本次装配创建的 thread；快照校验把删除边界
    /// 固定在本次装配写入的状态，避免误删已经被 runner 接管的 thread。
    pub fn remove_thread_if_current(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        expected: &ExecutionThread,
    ) -> DomainResult<Option<ExecutionThread>> {
        let removed = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(index) = state
                .thread_registry
                .iter()
                .position(|thread| &thread.thread_id == thread_id)
            else {
                return Ok(None);
            };
            let thread = &state.thread_registry[index];
            if &thread.session_id != session_id || thread != expected {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "thread {} 已被其他执行修改，拒绝回收 materialize thread",
                        thread_id
                    ),
                });
            }
            state
                .thread_context_checkpoints
                .retain(|checkpoint| &checkpoint.thread_id != thread_id);
            state.thread_registry.remove(index)
        };
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RemoveThread);
        Ok(Some(removed))
    }

    /// 回滚一次尚未启动 runner 的主线 thread 激活。
    ///
    /// 只有当前 thread 仍等于本次 materialize 写入后的完整快照时才允许恢复，
    /// 避免覆盖已经被后续执行使用的 thread 状态。
    pub fn restore_thread_after_materialization(
        &self,
        session_id: &SessionId,
        thread_id: &ThreadId,
        expected_current: &ExecutionThread,
        original: ExecutionThread,
    ) -> DomainResult<()> {
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let Some(thread) = state
                .thread_registry
                .iter_mut()
                .find(|thread| &thread.thread_id == thread_id)
            else {
                return Err(DomainError::NotFound { entity: "thread" });
            };
            if thread.session_id != *session_id || thread != expected_current {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "thread {} 已被其他执行修改，拒绝回滚 materialize 状态",
                        thread_id
                    ),
                });
            }
            *thread = original;
        }
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RestoreThread);
        Ok(())
    }

    /// 激活 thread 并返回变更后的快照；thread 不存在时显式失败。
    pub fn activate_thread_checked(
        &self,
        thread_id: &ThreadId,
        task_id: &TaskId,
        now: UtcMillis,
    ) -> DomainResult<ExecutionThread> {
        let updated = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let thread = state
                .thread_registry
                .iter_mut()
                .find(|thread| &thread.thread_id == thread_id)
                .ok_or(DomainError::NotFound { entity: "thread" })?;
            thread.status = ExecutionThreadStatus::Active;
            thread.last_used_at = now;
            if !thread.handled_task_ids.iter().any(|id| id == task_id) {
                thread.handled_task_ids.push(task_id.clone());
            }
            thread.clone()
        };
        self.mark_sidecar_dirty_for_session(
            &updated.session_id,
            SessionSidecarFlushReason::RegisterThread,
        );
        Ok(updated)
    }

    /// 将处理指定 task 且仍为 `Active` 的 thread 原子收口为 `Idle`。
    pub fn mark_task_threads_idle(&self, task_id: &TaskId, now: UtcMillis) -> usize {
        let (settled, session_ids) = {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let mut settled = 0;
            let mut session_ids = Vec::new();
            for thread in state.thread_registry.iter_mut().filter(|thread| {
                thread.status == ExecutionThreadStatus::Active
                    && thread
                        .handled_task_ids
                        .iter()
                        .any(|handled| handled == task_id)
            }) {
                thread.status = ExecutionThreadStatus::Idle;
                thread.last_used_at = now;
                settled += 1;
                session_ids.push(thread.session_id.clone());
            }
            session_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            session_ids.dedup();
            (settled, session_ids)
        };
        if settled > 0 {
            for session_id in session_ids {
                self.mark_sidecar_dirty_for_session(
                    &session_id,
                    SessionSidecarFlushReason::SettleThread,
                );
            }
        }
        settled
    }

    /// 只读快照：用于测试与调试。
    pub fn thread_registry_snapshot(&self, session_id: &SessionId) -> Vec<ExecutionThread> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_registry
            .iter()
            .filter(|thread| &thread.session_id == session_id)
            .cloned()
            .collect()
    }

    /// 查找 session 的 orchestrator 主线 thread。
    ///
    /// 该 thread 由 `ensure_session_mission` 在 session 首次接收 user 输入时
    /// spawn，与 session 共享生命周期。所有归属主线的 item 都以此 thread_id
    /// 作为 `source_thread_id` 锚点。
    pub fn orchestrator_thread_for_session(
        &self,
        session_id: &SessionId,
    ) -> Option<ExecutionThread> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_registry
            .iter()
            .find(|thread| {
                &thread.session_id == session_id && thread.role_id == ORCHESTRATOR_ROLE_ID
            })
            .cloned()
    }

    /// 确保 session 拥有 mission 并 spawn 对应的 orchestrator thread。
    ///
    /// 复用顺序：
    /// 1. session 已存在 orchestrator thread → 返回其 `(mission_id, thread_id)`
    /// 2. session runtime sidecar 已绑定 mission（来自 recovery / 前次 dispatch 的 ownership）
    ///    → 使用该 mission_id，并 spawn orchestrator thread
    /// 3. 否则调用 `mission_id_factory` 生成新 mission_id 并 spawn orchestrator thread
    ///
    /// 此方法是 session 进入"任意工作态"（聊天 / 任务派发 / 运行时 followup）的唯一入口，
    /// 保证"同 session 同 mission 同 orchestrator thread"的不变量。
    pub fn ensure_session_mission(
        &self,
        session_id: &SessionId,
        now: UtcMillis,
        mission_id_factory: impl FnOnce() -> MissionId,
    ) -> (MissionId, ThreadId) {
        let (mission_id, thread_id, _) =
            self.ensure_session_mission_with_created(session_id, now, mission_id_factory);
        (mission_id, thread_id)
    }

    /// 与 `ensure_session_mission` 相同，但返回本次调用是否创建了主线 thread，
    /// 供跨 store materialize 事务记录准确的回滚归属。
    pub fn ensure_session_mission_with_created(
        &self,
        session_id: &SessionId,
        now: UtcMillis,
        mission_id_factory: impl FnOnce() -> MissionId,
    ) -> (MissionId, ThreadId, bool) {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if let Some(thread) = state.thread_registry.iter().find(|thread| {
            &thread.session_id == session_id && thread.role_id == ORCHESTRATOR_ROLE_ID
        }) {
            return (thread.mission_id.clone(), thread.thread_id.clone(), false);
        }
        let existing_mission = state
            .execution_sidecar_store
            .runtime_sidecar(session_id)
            .and_then(|sidecar| sidecar.ownership.mission_id);
        let mission_id = existing_mission.unwrap_or_else(mission_id_factory);
        let thread_id = ThreadId::new(format!("thread-orchestrator-{}", session_id));
        state.thread_registry.push(ExecutionThread {
            thread_id: thread_id.clone(),
            session_id: session_id.clone(),
            mission_id: mission_id.clone(),
            role_id: ORCHESTRATOR_ROLE_ID.to_string(),
            worker_instance_id: WorkerId::new(format!("worker-orchestrator-{}", session_id)),
            status: ExecutionThreadStatus::Idle,
            created_at: now,
            last_used_at: now,
            observed_context_window_tokens: None,
            handled_task_ids: Vec::new(),
        });
        drop(state);
        self.mark_sidecar_dirty_for_session(session_id, SessionSidecarFlushReason::RegisterThread);
        (mission_id, thread_id, true)
    }

    /// 依据 `source_thread_id` 判定 item 的可见性目的地。返回值是"主线"还是
    /// "task 详情"，由 thread 的 `role_id` 决定：
    /// - 该 thread 是 session 的 orchestrator thread → `Main`
    /// - 其他 thread → `TaskDetail { role_id, worker_id }`
    ///
    /// 约束：传入的 `source_thread_id` 必须是本 session 已注册 thread；
    /// 否则返回 `None`，调用方按"未知来源"处理（通常只出现在 P6 之前遗留的
    /// canonical turn，新写入路径不会走到）。
    pub fn resolve_thread_visibility(
        &self,
        session_id: &SessionId,
        source_thread_id: &ThreadId,
    ) -> Option<ThreadVisibility> {
        let state = self.state.read().expect("session state read lock poisoned");
        let thread = state.thread_registry.iter().find(|thread| {
            &thread.session_id == session_id && &thread.thread_id == source_thread_id
        })?;
        if thread.role_id == ORCHESTRATOR_ROLE_ID {
            Some(ThreadVisibility::Main)
        } else {
            Some(ThreadVisibility::TaskDetail {
                role_id: thread.role_id.clone(),
                worker_id: thread.worker_instance_id.clone(),
            })
        }
    }

    /// 为已核验来源的恢复线程读取 canonical 投影，不读取缓存 transcript。
    pub fn canonical_thread_message_history(
        &self,
        thread: &ExecutionThread,
    ) -> DomainResult<Vec<ThreadChatMessage>> {
        let state = self.state.read().expect("session state read lock poisoned");
        project_thread_message_history(&state, thread, None)
    }

    /// 在同一锁内读取 canonical 历史和检查点，摘要视图只从这个快照派生。
    pub fn thread_context_snapshot(
        &self,
        thread_id: &ThreadId,
    ) -> DomainResult<ThreadContextSnapshot> {
        let state = self.state.read().expect("session state read lock poisoned");
        let thread = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .ok_or(DomainError::NotFound { entity: "thread" })?;
        Ok(ThreadContextSnapshot {
            thread_id: thread.thread_id.clone(),
            session_id: thread.session_id.clone(),
            messages: project_thread_message_history(&state, thread, None)?,
            checkpoint: state
                .thread_context_checkpoints
                .iter()
                .find(|checkpoint| &checkpoint.thread_id == thread_id)
                .cloned(),
            context_window_tokens: thread.observed_context_window_tokens,
        })
    }

    pub fn thread_context_checkpoint(
        &self,
        thread_id: &ThreadId,
    ) -> Option<ThreadContextCheckpoint> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .thread_context_checkpoints
            .iter()
            .find(|checkpoint| &checkpoint.thread_id == thread_id)
            .cloned()
    }

    pub fn thread_context_window_tokens(&self, thread_id: &ThreadId) -> Option<u64> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .and_then(|thread| thread.observed_context_window_tokens)
    }

    pub fn record_thread_context_window_tokens(
        &self,
        thread_id: &ThreadId,
        context_window_tokens: u64,
        now: UtcMillis,
    ) {
        if context_window_tokens == 0 {
            return;
        }
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let session_id = state
            .thread_registry
            .iter()
            .find(|thread| &thread.thread_id == thread_id)
            .map(|thread| thread.session_id.clone());
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| &thread.thread_id == thread_id)
        {
            thread.observed_context_window_tokens = Some(context_window_tokens);
            thread.last_used_at = now;
        }
        drop(state);
        if let Some(session_id) = session_id {
            self.mark_sidecar_dirty_for_session(
                &session_id,
                SessionSidecarFlushReason::RegisterThread,
            );
        }
    }

    /// 安装上下文检查点并显式报告目标 thread 是否存在。
    pub fn install_thread_context_checkpoint_checked(
        &self,
        thread_id: &ThreadId,
        checkpoint: ThreadContextCheckpoint,
        now: UtcMillis,
    ) -> DomainResult<()> {
        let snapshot = self.thread_context_snapshot(thread_id)?;
        if &checkpoint.thread_id != thread_id
            || !self.install_thread_context_checkpoint_if_current(&snapshot, checkpoint, now)
        {
            return Err(DomainError::InvalidState {
                message: "上下文检查点来源、范围、指纹或代际与 canonical 快照不匹配".into(),
            });
        }
        Ok(())
    }

    /// 恢复摘要沿用来源快照，只有源、目标仍然有效且共享同一历史前缀时才能提交。
    pub fn inherit_thread_context_checkpoint(
        &self,
        source: &ThreadContextSnapshot,
        destination: &ThreadContextSnapshot,
        now: UtcMillis,
    ) -> DomainResult<()> {
        if source.session_id != destination.session_id || source.thread_id == destination.thread_id
        {
            return Err(DomainError::InvalidState {
                message: "恢复摘要必须属于同一会话的不同线程".into(),
            });
        }
        let Some(checkpoint) = source.checkpoint().filter(|checkpoint| {
            checkpoint.matches_history(source.messages())
                && checkpoint.matches_history(destination.messages())
        }) else {
            return Ok(());
        };
        let inherited = ThreadContextCheckpoint {
            thread_id: destination.thread_id.clone(),
            ..checkpoint.clone()
        };
        if !self.install_thread_context_checkpoint(destination, inherited, Some(source), now) {
            return Err(DomainError::InvalidState {
                message: "恢复摘要的来源或目标快照已变化，拒绝安装".into(),
            });
        }
        Ok(())
    }

    /// 安装时核对完整 canonical 历史、检查点身份和窗口；同长度改写也会拒绝。
    pub fn install_thread_context_checkpoint_if_current(
        &self,
        snapshot: &ThreadContextSnapshot,
        checkpoint: ThreadContextCheckpoint,
        now: UtcMillis,
    ) -> bool {
        self.install_thread_context_checkpoint(snapshot, checkpoint, None, now)
    }

    pub(super) fn install_thread_context_checkpoint(
        &self,
        snapshot: &ThreadContextSnapshot,
        checkpoint: ThreadContextCheckpoint,
        source: Option<&ThreadContextSnapshot>,
        now: UtcMillis,
    ) -> bool {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !thread_context_snapshot_is_current(&state, snapshot)
            || source.is_some_and(|source| !thread_context_snapshot_is_current(&state, source))
            || checkpoint.thread_id != snapshot.thread_id
            || !checkpoint.matches_history(&snapshot.messages)
            || checkpoint.generation
                <= snapshot
                    .checkpoint
                    .as_ref()
                    .map(|value| value.generation)
                    .unwrap_or_default()
        {
            return false;
        }
        if let Some(existing) = state
            .thread_context_checkpoints
            .iter_mut()
            .find(|value| value.thread_id == snapshot.thread_id)
        {
            *existing = checkpoint;
        } else {
            state.thread_context_checkpoints.push(checkpoint);
        }
        if let Some(thread) = state
            .thread_registry
            .iter_mut()
            .find(|thread| thread.thread_id == snapshot.thread_id)
        {
            thread.last_used_at = now;
        }
        drop(state);
        self.mark_sidecar_dirty_for_session(
            &snapshot.session_id,
            SessionSidecarFlushReason::RegisterThread,
        );
        true
    }

    /// 只清除读取时的检查点，不能删除并发安装的新摘要。
    pub fn clear_thread_context_checkpoint_if_current(
        &self,
        snapshot: &mut ThreadContextSnapshot,
    ) -> bool {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !thread_context_snapshot_is_current(&state, snapshot) {
            return false;
        }
        state
            .thread_context_checkpoints
            .retain(|value| value.thread_id != snapshot.thread_id);
        let changed = snapshot.checkpoint.take().is_some();
        drop(state);
        if changed {
            self.mark_sidecar_dirty_for_session(
                &snapshot.session_id,
                SessionSidecarFlushReason::RegisterThread,
            );
        }
        true
    }
}

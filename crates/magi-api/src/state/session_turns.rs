use super::*;

impl ApiState {
    pub(crate) async fn lock_app_server_request(
        &self,
        request_id: &str,
    ) -> AppServerRequestLockGuard {
        let request_id = request_id.trim().to_string();
        debug_assert!(!request_id.is_empty());
        let lock = {
            let mut registry = self
                .app_server_request_locks
                .lock()
                .expect("app server request lock registry poisoned");
            let entry =
                registry
                    .entry(request_id.clone())
                    .or_insert_with(|| AppServerRequestLockEntry {
                        lock: Arc::new(tokio::sync::Mutex::new(())),
                        references: 0,
                    });
            entry.references += 1;
            Arc::clone(&entry.lock)
        };
        let mut lease = AppServerRequestLockGuard {
            registry: Arc::clone(&self.app_server_request_locks),
            request_id,
            _guard: None,
        };
        // lease 在等待期间已经持有引用；若这个 future 被取消，Drop 会回收
        // 引用计数，不会把已取消的 requestId 永久留在注册表中。
        lease._guard = Some(lock.lock_owned().await);
        lease
    }

    /// 旧文件只读取迁移一次；完成前保留原文件，崩溃重试按稳定 input 身份去重。
    pub fn restore_regular_session_turn_queues(&self) -> Result<usize, ApiError> {
        if let Some(root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|p| p.state_root())
        {
            let path = root.join("session-turn-queue.json");
            match fs::read(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(ApiError::internal_assembly("读取旧输入队列失败", error)),
                Ok(bytes) => {
                    let turns: Vec<QueuedRegularSessionTurn> = serde_json::from_slice(&bytes)
                        .map_err(|error| {
                            ApiError::internal_assembly("解析旧输入队列失败", error)
                        })?;
                    // 先完整校验再迁移，防止损坏文件留下部分输入。
                    let mut normalized = Vec::new();
                    let mut identities = HashSet::new();
                    let mut requests = HashSet::new();
                    for mut turn in turns {
                        turn.normalize_identity();
                        if !identities.insert((turn.session_id.clone(), turn.queue_id.clone())) {
                            return Err(ApiError::Conflict(
                                "旧输入队列存在重复身份，保留原文件供核对".into(),
                            ));
                        }
                        if turn.request_fingerprint.is_none() {
                            turn.request_fingerprint = Some(
                                turn.request
                                    .request_fingerprint()
                                    .map_err(ApiError::InvalidInput)?,
                            );
                        }
                        if !requests.insert(turn.request.request_id().expect("normalized request"))
                        {
                            return Err(ApiError::Conflict(
                                "旧输入队列存在重复 requestId，保留原文件供核对".into(),
                            ));
                        }
                        if !matches!(
                            turn.route,
                            crate::dto::SessionTurnRouteDto::Chat
                                | crate::dto::SessionTurnRouteDto::Execute
                        ) {
                            return Err(ApiError::Conflict(
                                "旧输入队列投递类型无效，保留原文件供核对".into(),
                            ));
                        }
                        if self.session_store.session(&turn.session_id).is_none() {
                            return Err(ApiError::Conflict(format!(
                                "旧排队输入所属会话不存在: {}",
                                turn.session_id
                            )));
                        }
                        normalized.push(turn);
                    }
                    let backup = root.join("session-turn-queue.migrated.json");
                    if backup.exists()
                        && fs::read(&backup).map_err(|error| {
                            ApiError::internal_assembly("读取迁移备份失败", error)
                        })? != bytes
                    {
                        return Err(ApiError::Conflict(
                            "旧队列迁移备份冲突，原文件已保留".into(),
                        ));
                    }
                    for turn in normalized {
                        self.enqueue_regular_session_turn(turn)?;
                    }
                    fs::rename(&path, &backup).map_err(|error| {
                        ApiError::internal_assembly("保留旧队列迁移备份失败", error)
                    })?;
                }
            }
        }
        for input in self.session_store.all_session_inputs() {
            if input.state == magi_session_store::SessionInputState::Pending {
                let turn: QueuedRegularSessionTurn = serde_json::from_value(input.payload.clone())
                    .map_err(|error| ApiError::internal_assembly("持久输入请求损坏", error))?;
                if turn.session_id != input.session_id
                    || turn.queue_id != input.input_id
                    || turn.request.request_id().as_deref() != Some(input.request_id.as_str())
                    || turn.request.user_message_id().as_deref()
                        != Some(input.user_message_id.as_str())
                    || turn.request_fingerprint.as_deref()
                        != Some(input.request_fingerprint.as_str())
                    || turn.request.interrupt_turn_id.as_deref()
                        != input
                            .interruption
                            .as_ref()
                            .map(|value| value.turn_id.as_str())
                {
                    return Err(ApiError::Conflict(
                        "持久输入与原始请求身份不一致，拒绝执行".into(),
                    ));
                }
            }
        }
        let inputs = self.session_store.all_session_inputs();
        for mut input in inputs {
            if input.state == magi_session_store::SessionInputState::Pending
                && !self
                    .session_store
                    .session(&input.session_id)
                    .is_some_and(|s| s.status == SessionLifecycleStatus::Active)
            {
                let revision = input.revision;
                input.revision += 1;
                input.state = magi_session_store::SessionInputState::Cancelled;
                self.session_store
                    .commit_input(input, Some(revision))
                    .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
            }
        }
        Ok(self
            .session_store
            .all_session_inputs()
            .iter()
            .filter(|input| input.state == magi_session_store::SessionInputState::Pending)
            .count())
    }

    pub(crate) fn has_active_session_tool_execution(&self, session_id: &SessionId) -> bool {
        self.tool_registry.as_ref().is_some_and(|registry| {
            registry.has_active_executions(&ToolExecutionContextQuery {
                session_id: Some(session_id.clone()),
                ..Default::default()
            })
        })
    }

    /// 构造一次性 Turn 事实写回边界。SessionStore 的 canonical mutation 必须
    /// 通过该 sink 完成，事件发布只能发生在 mutation 成功之后。
    pub fn turn_event_sink(&self) -> CanonicalTurnEventSink<'_> {
        CanonicalTurnEventSink::new(
            self.session_store.as_ref(),
            self.event_bus.as_ref(),
            self.task_store(),
        )
    }

    pub fn with_session_turn_dispatcher(mut self, dispatcher: Arc<LlmTaskDispatcher>) -> Self {
        let execution = Arc::downgrade(&dispatcher);
        let resources = self.execution_resources.clone();
        self.task_execution_registry
            .set_agent_subtree_settler(move |session, tasks| {
                let queries = tasks
                    .iter()
                    .map(|task| ToolExecutionContextQuery {
                        session_id: Some(session.clone()),
                        task_id: Some(task.clone()),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                for query in &queries {
                    resources.cancel(
                        query.clone(),
                        BrowserLeaseEndReason::TaskFinished,
                        UtcMillis::now(),
                    );
                }
                let execution = execution
                    .upgrade()
                    .ok_or("代理执行器已不可用，无法核对线程退出")?;
                // 这是取消结算的观察期限，不限制任务的正常执行时长。
                execution
                    .wait_for_task_dispatches_blocking(tasks, std::time::Duration::from_secs(30))?;
                if resources
                    .tool_registry
                    .read()
                    .expect("execution resource tool registry lock poisoned")
                    .as_ref()
                    .is_some_and(|registry| {
                        queries
                            .iter()
                            .any(|query| registry.has_active_executions(query))
                    })
                {
                    return Err("代理仍有未退出的工具或进程，取消已接纳但资源尚未结算".into());
                }
                Ok(())
            });
        self.session_turn_dispatcher = Some(dispatcher);
        self
    }

    pub fn session_turn_dispatcher(&self) -> Option<&Arc<LlmTaskDispatcher>> {
        self.session_turn_dispatcher.as_ref()
    }

    pub fn turn_coordinator(&self) -> &Arc<magi_conversation_runtime::SessionTurnCoordinator> {
        &self.turn_coordinator
    }

    /// 从 SessionStore 的 canonical Turn projection 恢复 Coordinator 的轻量身份索引。
    ///
    /// canonical event 是重启后的唯一事实来源；这里只恢复 requestId、fingerprint、
    /// profile、attempt 和生命周期状态，不复制 Turn 正文，也不会自动重新发起上游
    /// Provider 请求。活动 Turn 的执行是否可继续由各 profile 的恢复调度器决定。
    pub fn restore_turn_coordinator_from_session_store(&self) -> usize {
        let mut restored = 0;
        for session_id in self.session_store.session_index() {
            let mut turns = self.session_store.canonical_turns_for_session(&session_id);
            // 先恢复最新 Turn：Coordinator 只允许一个 active 槽位，旧的异常
            // 非终态事实不能覆盖当前 canonical 轮次。终态 Turn 仍会全部进入
            // request replay 索引，供重试返回原 receipt。
            turns.sort_by(|left, right| {
                right
                    .turn_seq
                    .cmp(&left.turn_seq)
                    .then_with(|| right.accepted_at.0.cmp(&left.accepted_at.0))
                    .then_with(|| right.turn_id.cmp(&left.turn_id))
            });
            for turn in turns {
                let Some(command) =
                    SessionTurnCoordinator::recover_command_for_canonical_turn(&session_id, &turn)
                else {
                    continue;
                };
                if matches!(
                    self.turn_coordinator.execute_command(&session_id, command),
                    Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
                ) {
                    restored += 1;
                }
            }
        }
        restored
    }

    pub(crate) async fn lock_session_turn(
        &self,
        session_id: &SessionId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .session_turn_locks
                .lock()
                .expect("session turn locks should hold");
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// 项目管理同时冻结已有会话的接纳边界。只尝试取得锁，不能在持有导航锁时
    /// 等待执行结算（执行结算可能需要导航），也不能阻塞其它项目的控制面。
    pub(crate) fn try_lock_session_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTurnCommitGuard, ApiError> {
        let lock = self
            .session_turn_locks
            .lock()
            .expect("session turn locks should hold")
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.try_lock_owned().map_err(|_| {
            ApiError::Conflict("项目会话正在提交控制状态，请稍后重试项目修改".into())
        })?;
        Ok(SessionTurnCommitGuard {
            _session_turn_guard: guard,
        })
    }

    pub(crate) async fn lock_session_navigation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.session_navigation_lock.lock().await
    }

    pub(super) fn project_directory_lock(&self, id: &WorkspaceId) -> Arc<tokio::sync::RwLock<()>> {
        self.project_directory_locks
            .lock()
            .expect("project directory locks")
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
    }

    pub(crate) fn try_project_directory_admission(
        &self,
        id: &WorkspaceId,
    ) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, ApiError> {
        self.project_directory_lock(id)
            .try_read_owned()
            .map_err(|_| ApiError::Conflict("项目源文件夹正在修改，请稍后重试".into()))
    }

    pub(crate) fn try_project_directory_edit(
        &self,
        id: &WorkspaceId,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, ApiError> {
        self.project_directory_lock(id)
            .try_write_owned()
            .map_err(|_| ApiError::Conflict("项目请求正在接纳或目录正在修改，请稍后重试".into()))
    }

    /// 取得指定 session 的 Turn 提交锁。
    ///
    /// 该锁不包含全局导航锁。导航只在修改当前会话指针或创建新会话的短提交中
    /// 显式取得，避免后台执行生命周期把所有会话切换串行化。
    pub(crate) async fn lock_session_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> SessionTurnCommitGuard {
        let session_turn_guard = self.lock_session_turn(session_id).await;
        SessionTurnCommitGuard {
            _session_turn_guard: session_turn_guard,
        }
    }

    /// 在调用方已经持有 session Turn 锁后取得 Runner 生命周期锁。
    ///
    /// 该入口只允许被已持有 session Turn 锁的路径调用，避免 Continue、
    /// 新会话回滚等流程再次获取 Turn 锁而形成自锁。
    pub(crate) async fn lock_runner_lifecycle_after_turn_commit(
        &self,
        session_id: &SessionId,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let manager = self.runner_manager()?.clone();
        Some(manager.lock_session_lifecycle(session_id).await)
    }

    /// 取得完整执行生命周期提交锁：session Turn -> Runner lifecycle。
    ///
    /// 这里明确不取得全局导航锁。导航锁只属于当前会话指针的短提交，不能被
    /// 后台 preparation、Runner 启动或模型执行带出控制面。
    pub(crate) async fn lock_session_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> SessionLifecycleCommitGuard {
        let session_turn_guard = self.lock_session_turn(session_id).await;
        let runner_lifecycle_guard = self
            .runner_manager()
            .cloned()
            .map(|manager| async move { manager.lock_session_lifecycle(session_id).await });
        let runner_lifecycle_guard = match runner_lifecycle_guard {
            Some(lock) => Some(lock.await),
            None => None,
        };
        SessionLifecycleCommitGuard {
            _session_turn_guard: session_turn_guard,
            _runner_lifecycle_guard: runner_lifecycle_guard,
        }
    }

    /// 回滚一个尚未成功接纳首条消息的新会话。
    ///
    /// 调用方必须已经持有 navigation 锁。新会话创建会自动改写 current，
    /// 因此删除资源后必须显式恢复创建前的 current，并将删除与恢复作为同一次
    /// 完整 projection 提交持久化。创建提交可能已经把新 session projection 写入磁盘，
    /// 回滚必须同时清理该 projection。Turn 与 Runner 锁在这里按统一顺序补齐。
    pub(crate) async fn rollback_created_session_after_navigation_lock(
        &self,
        session_id: &SessionId,
        previous_current_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let _session_turn_guard = self.lock_session_turn(session_id).await;
        let _runner_lifecycle_guard = self
            .lock_runner_lifecycle_after_turn_commit(session_id)
            .await;
        self.delete_session_and_resources_after_lifecycle_lock(
            session_id,
            previous_current_session_id,
        )
        .await
    }

    pub(crate) async fn lock_session_change_sync(
        &self,
        session_id: &SessionId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .session_change_sync_locks
                .lock()
                .expect("session change sync locks should hold");
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    pub(crate) fn enqueue_regular_session_turn(
        &self,
        mut turn: QueuedRegularSessionTurn,
    ) -> Result<usize, ApiError> {
        use magi_session_store::{SessionInput, SessionInputState};
        turn.normalize_identity();
        let fingerprint = match &turn.request_fingerprint {
            Some(fingerprint) => fingerprint.clone(),
            None => turn
                .request
                .request_fingerprint()
                .map_err(ApiError::InvalidInput)?,
        };
        turn.request_fingerprint = Some(fingerprint.clone());
        let request_id = turn.request.request_id().expect("normalized request ID");
        let user_message_id = turn
            .request
            .user_message_id()
            .expect("normalized user item ID");
        let existing = self
            .session_store
            .inputs_for_session(&turn.session_id)
            .into_iter()
            .find(|input| input.input_id == turn.queue_id || input.request_id == request_id);
        if let Some(existing) = existing {
            let compatible_binding = matches!(
                existing.state,
                magi_session_store::SessionInputState::Attached
                    | magi_session_store::SessionInputState::Consumed
            );
            if (existing.input_id != turn.queue_id && !compatible_binding)
                || existing.session_id != turn.session_id
                || existing.request_id != request_id
                || existing.user_message_id != user_message_id
                || existing.request_fingerprint != fingerprint
            {
                return Err(ApiError::Conflict("输入身份或请求指纹冲突".into()));
            }
            return Ok(self.queued_regular_session_turn_count(&turn.session_id));
        }
        let bound = self
            .session_store
            .canonical_turns_for_session(&turn.session_id)
            .into_iter()
            .find(|canonical| {
                crate::turn_service::canonical_turn_matches_queued_regular_turn(canonical, &turn)
            });
        let active = self
            .session_store
            .session(&turn.session_id)
            .is_some_and(|s| s.status == SessionLifecycleStatus::Active);
        let interruption = turn.request.interrupt_turn_id.as_ref().map(|turn_id| {
            magi_session_store::InputInterruption {
                turn_id: turn_id.clone(),
                root_task_id: self
                    .session_store
                    .active_execution_chain(&turn.session_id)
                    .filter(|chain| chain.current_turn_id.as_ref() == Some(turn_id))
                    .map(|chain| chain.root_task_id),
                settled_at: None,
                error: None,
            }
        });
        self.session_store
            .commit_input(
                SessionInput {
                    input_id: turn.queue_id.clone(),
                    session_id: turn.session_id.clone(),
                    request_id,
                    user_message_id,
                    request_fingerprint: fingerprint,
                    accepted_at: turn.accepted_at,
                    revision: 1,
                    state: if bound.is_some() {
                        SessionInputState::Attached
                    } else if active {
                        SessionInputState::Pending
                    } else {
                        SessionInputState::Cancelled
                    },
                    attached_turn_id: bound.map(|turn| turn.turn_id),
                    consumption: None,
                    interruption,
                    payload: serde_json::to_value(&turn)
                        .map_err(|error| ApiError::internal_assembly("编码输入失败", error))?,
                },
                None,
            )
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(self
            .queued_regular_session_turns(&turn.session_id)
            .iter()
            .position(|queued| queued.queue_id == turn.queue_id)
            .map_or(0, |position| position + 1))
    }

    pub(crate) fn peek_next_regular_session_turn(
        &self,
        session_id: &SessionId,
    ) -> Option<QueuedRegularSessionTurn> {
        self.queued_regular_session_turns(session_id)
            .into_iter()
            .find(QueuedRegularSessionTurn::requires_execution)
    }

    pub(crate) fn queued_regular_session_turns(
        &self,
        session_id: &SessionId,
    ) -> Vec<QueuedRegularSessionTurn> {
        let mut inputs = self.session_store.inbox_snapshot(session_id).0;
        // 明确的停止并改做先于普通下一轮输入；原 FIFO 输入继续保留。
        inputs.sort_by_key(|input| {
            (
                input.interruption.is_none(),
                input.accepted_at,
                input.input_id.clone(),
            )
        });
        inputs
            .into_iter()
            .map(|input| {
                serde_json::from_value(input.payload).expect("validated queued input payload")
            })
            .collect()
    }

    pub(crate) fn queued_regular_session_turn_for_request_id(
        &self,
        request_id: &str,
    ) -> Option<(QueuedRegularSessionTurn, usize)> {
        self.queued_regular_session_ids()
            .into_iter()
            .find_map(|session_id| {
                self.queued_regular_session_turns(&session_id)
                    .into_iter()
                    .enumerate()
                    .find(|(_, turn)| {
                        turn.request.request_id().as_deref() == Some(request_id.trim())
                    })
                    .map(|(index, turn)| (turn, index + 1))
            })
    }

    pub(crate) fn remove_regular_session_turn(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<bool, ApiError> {
        let Some(mut input) = self
            .session_store
            .inputs_for_session(session_id)
            .into_iter()
            .find(|input| input.input_id == queue_id)
        else {
            return Ok(false);
        };
        // 已绑定的输入不能再被取回编辑或删除。
        if matches!(
            input.state,
            magi_session_store::SessionInputState::Attached
                | magi_session_store::SessionInputState::Consumed
        ) {
            return Err(ApiError::Conflict("输入已被领取，不能取回或删除".into()));
        }
        if input.state != magi_session_store::SessionInputState::Pending {
            return Ok(false);
        }
        let revision = input.revision;
        input.revision += 1;
        input.state = magi_session_store::SessionInputState::Cancelled;
        self.session_store
            .commit_input(input, Some(revision))
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(true)
    }

    pub(crate) fn record_regular_session_turn_retry(
        &self,
        session_id: &SessionId,
        queue_id: &str,
    ) -> Result<u8, ApiError> {
        let mut input = self
            .session_store
            .inputs_for_session(session_id)
            .into_iter()
            .find(|input| {
                input.input_id == queue_id
                    && input.state == magi_session_store::SessionInputState::Pending
            })
            .ok_or_else(|| ApiError::Conflict("输入已被领取或撤销".into()))?;
        let mut turn: QueuedRegularSessionTurn = serde_json::from_value(input.payload.clone())
            .map_err(|error| ApiError::internal_assembly("解码输入失败", error))?;
        turn.retry_count = turn.retry_count.saturating_add(1);
        let retry_count = turn.retry_count;
        let revision = input.revision;
        input.revision += 1;
        input.payload = serde_json::to_value(turn)
            .map_err(|error| ApiError::internal_assembly("编码输入失败", error))?;
        self.session_store
            .commit_input(input, Some(revision))
            .map_err(|error| ApiError::internal_assembly("提交输入事务失败", error))?;
        Ok(retry_count)
    }

    pub(crate) fn queued_regular_session_turn_count(&self, session_id: &SessionId) -> usize {
        self.queued_regular_session_turns(session_id)
            .iter()
            .filter(|turn| turn.requires_execution())
            .count()
    }

    pub(crate) fn queued_regular_session_ids(&self) -> Vec<SessionId> {
        self.session_store.pending_input_session_ids()
    }

    pub(crate) fn clear_regular_session_turn_queue_for_lifecycle(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, ApiError> {
        let queued = self.queued_regular_session_turns(session_id);
        for turn in &queued {
            self.remove_regular_session_turn(session_id, &turn.queue_id)?;
        }
        Ok(queued.len())
    }

    /// 会话删除的唯一资源回收入口。先停止并等待后台 runner，再删除所有运行态与
    /// 持久化事实，最后删除 SessionStore 主记录，避免任何组件保留孤儿状态。
    pub async fn delete_session_and_resources(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ApiError> {
        let _lifecycle_guard = self.lock_session_lifecycle(session_id).await;
        let replacement = self.replacement_session_for_delete(session_id);
        self.delete_session_and_resources_after_lifecycle_lock(session_id, replacement)
            .await
    }

    /// 在调用方已经持有 `navigation -> Turn -> Runner lifecycle` 后删除 session。
    ///
    /// 该入口让删除路由可以把替代 current 选择和最终持久化继续放在同一个
    /// navigation 临界区内；不能从未持有完整生命周期锁的路径调用。
    pub(crate) async fn delete_session_and_resources_after_lifecycle_lock(
        &self,
        session_id: &SessionId,
        replacement_session_id: Option<SessionId>,
    ) -> Result<(), ApiError> {
        let mut personal_root = self
            .session_store
            .session(session_id)
            .filter(|session| self.session_workspace_id(session).is_none())
            .map(|_| self.personal_session_execution_root_path(session_id));
        let job_outputs = self
            .session_store
            .background_jobs_for_session(session_id)
            .map_err(|error| ApiError::internal_assembly("读取会话 Job 输出归属失败", error))?;
        let manager = self.runner_manager();
        crate::turn_service::cancel_active_session_turn_for_lifecycle(self, session_id);
        if let Some(manager) = manager {
            manager
                .unbind_session_after_lifecycle_lock(session_id)
                .await
                .map_err(|error| ApiError::internal_assembly("删除会话时停止执行失败", error))?;
        }
        self.close_browser_session_for_magi_session(session_id)
            .await?;
        self.remove_browser_artifacts_for_session(session_id);
        self.terminal_sessions
            .close_for_session(session_id.as_str());
        self.cleanup_session_isolation(session_id).await;
        if personal_root.is_none() {
            self.cleanup_session_git_resources(session_id).await;
        }
        self.settings_store
            .remove_session(session_id)
            .map_err(crate::errors::settings_persistence_error)?;
        self.clear_regular_session_turn_queue_for_lifecycle(session_id)?;

        let mut mission_ids = HashSet::new();
        if let Some(thread) = self
            .session_store
            .orchestrator_thread_for_session(session_id)
        {
            mission_ids.insert(thread.mission_id);
        }
        if let Some(ownership) = self.session_store.execution_ownership(session_id)
            && let Some(mission_id) = ownership.mission_id
        {
            mission_ids.insert(mission_id);
        }
        if let Some(sidecar) = self.session_store.runtime_sidecar(session_id)
            && let Some(chain) = sidecar.active_execution_chain
        {
            mission_ids.insert(chain.mission_id);
        }

        let mut task_ids = self
            .session_store
            .execution_task_ids_for_session(session_id)
            .into_iter()
            .collect::<HashSet<_>>();
        task_ids.extend(self.task_execution_registry.remove_session(session_id));
        if let Some(task_store) = self.task_store() {
            for task_id in task_ids.clone() {
                if let Some(task) = task_store.get_task(&task_id) {
                    mission_ids.insert(task.mission_id);
                }
            }
            for mission_id in mission_ids {
                let removed = task_store
                    .remove_tasks_by_mission(&mission_id)
                    .map_err(|error| {
                        ApiError::internal_assembly("删除会话任务 checkpoint 失败", error)
                    })?;
                task_ids.extend(removed.into_iter().map(|task| task.task_id));
            }
            for task_id in task_ids.clone() {
                task_store.remove_task(&task_id).map_err(|error| {
                    ApiError::internal_assembly("删除会话残留任务 checkpoint 失败", error)
                })?;
            }
        }
        self.conversation_registry.remove_session(session_id);
        self.session_store
            .delete_session_with_persistence(
                session_id,
                replacement_session_id.as_ref(),
                |durable, sidecars| {
                    if let Some(persist) = &self.session_projection_persist {
                        persist(
                            durable,
                            sidecars,
                            SessionProjectionPersistMode::Sessions {
                                session_ids: replacement_session_id.iter().cloned().collect(),
                            },
                        )
                    } else {
                        Ok(())
                    }
                },
            )
            .map_err(|error| match error {
                magi_session_store::SessionMutationTransactionError::Domain(error) => {
                    ApiError::internal_assembly("删除会话失败", error)
                }
                magi_session_store::SessionMutationTransactionError::Persistence(error) => error,
            })?;
        if let Some(root) = self
            .runtime_persistence
            .as_ref()
            .and_then(|persistence| persistence.state_root())
        {
            for job in job_outputs {
                let directory = root.join("job-output").join(&job.job_id);
                if job.stdout_path != directory.join("stdout.log")
                    || job.stderr_path != directory.join("stderr.log")
                {
                    continue;
                }
                if let Err(error) = fs::remove_dir_all(&directory)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%session_id, job_id = %job.job_id, %error, "删除会话 Job 输出失败");
                }
            }
        }
        // 只在会话删除已持久提交后回收自有执行目录。失败不能先删除用户成果。
        if personal_root.is_some() && !self.cleanup_session_git_resources(session_id).await {
            personal_root = None;
        }
        if let Some(root) = personal_root {
            let result = tokio::task::spawn_blocking(move || fs::remove_dir_all(root)).await;
            if let Ok(Err(error)) = result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%session_id, %error, "删除个人会话文件失败");
            }
        }
        self.session_turn_locks
            .lock()
            .expect("session turn locks should hold")
            .remove(session_id);
        self.session_change_sync_locks
            .lock()
            .expect("session change sync locks should hold")
            .remove(session_id);
        Ok(())
    }

    /// 删除一个启动时无法加载的会话。它没有进入会话状态，没有运行态、任务或锁需要回收，
    /// 只需移除持久化文件与会话私有数据；调用方负责持有该会话的生命周期锁。
    pub(crate) async fn discard_unavailable_session_after_lifecycle_lock(
        &self,
        session_id: &SessionId,
    ) -> Result<magi_session_store::UnavailableSession, ApiError> {
        let discard = self.unavailable_session_discard.as_ref().ok_or_else(|| {
            ApiError::internal_assembly("删除不可用会话失败", "daemon 未注入不可用会话清理入口")
        })?;
        let entry = discard(session_id)?;
        self.session_store.forget_unavailable_session(session_id);
        self.settings_store
            .remove_session(session_id)
            .map_err(crate::errors::settings_persistence_error)?;
        // 无项目会话的私有执行目录：与正常删除同一规则，dirty/占用的 Git 资源保留成果。
        if entry.workspace_id.is_none() && self.cleanup_session_git_resources(session_id).await {
            let root = self.personal_session_execution_root_path(session_id);
            let result = tokio::task::spawn_blocking(move || fs::remove_dir_all(root)).await;
            if let Ok(Err(error)) = result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%session_id, %error, "删除不可用个人会话文件失败");
            }
        }
        Ok(entry)
    }

    pub(super) fn replacement_session_for_delete(
        &self,
        session_id: &SessionId,
    ) -> Option<SessionId> {
        if self.session_store.current_session_id().as_ref() != Some(session_id) {
            return None;
        }
        let target_workspace = self
            .session_store
            .session(session_id)
            .and_then(|session| session.workspace_id);
        self.session_store
            .sessions()
            .into_iter()
            .filter(|session| {
                &session.session_id != session_id
                    && session.workspace_id == target_workspace
                    && session.kind.is_user()
            })
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
            })
            .map(|session| session.session_id)
    }
}

use super::*;

impl ApiState {
    pub fn terminate_managed_processes(&self) -> usize {
        self.managed_process_group.terminate_all()
    }

    /// 任务（含子任务）进入终态：释放它自己的进程、浏览器租约与默认浏览器目标，
    /// 不等整轮结束，避免子代理结束后仍占着页面阻塞主代理。
    pub fn release_finished_task_resources(&self, session_id: &SessionId, task_id: &TaskId) {
        // 已排队但从未进入模型循环的代理也会收到 TaskStore 终态通知，
        // 不能只依赖执行栈退出时回收其初始 assignment。
        self.conversation_registry
            .close_task_signal_channel(session_id, task_id);
        self.cancel_execution_resources(
            Some(session_id),
            None,
            Some(task_id),
            BrowserLeaseEndReason::TaskFinished,
        );
        let executor = crate::browser_tool_runtime::browser_executor_key_for_task(task_id);
        let created_tabs = match self
            .mutate_browser_authority(|authority| Ok(authority.forget_agent_tabs(&executor)))
        {
            Ok(created_tabs) => created_tabs,
            Err(error) => {
                tracing::warn!(%session_id, %task_id, ?error, "清理任务浏览器默认目标失败");
                return;
            }
        };
        // 子代理新开的页面是中间产物，随子任务结束关闭，避免标签页累积到上限；
        // 主线的页面属于用户可继续查看的结果，保留到用户显式关闭。
        let is_child_task = self
            .task_store()
            .and_then(|task_store| task_store.get_task(task_id))
            .is_some_and(|task| task.parent_task_id.is_some());
        if !is_child_task || created_tabs.is_empty() {
            return;
        }
        let state = self.clone();
        spawn_session_turn_work("magi-browser-child-tab-close", async move {
            for tab_id in created_tabs {
                if let Err(error) =
                    crate::browser_tool_runtime::close_browser_tab(&state, &tab_id).await
                {
                    tracing::warn!(%tab_id, ?error, "关闭子任务新开的浏览器标签页失败");
                }
            }
        });
    }

    pub fn cancel_execution_resources(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
        task_id: Option<&TaskId>,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        self.execution_resources.cancel(
            ToolExecutionContextQuery {
                session_id: session_id.cloned(),
                workspace_id: workspace_id.cloned(),
                task_id: task_id.cloned(),
                worker_id: None,
                taskless_only: false,
            },
            reason,
            UtcMillis::now(),
        )
    }

    pub(crate) fn cancel_conversation_execution_resources(
        &self,
        session_id: &SessionId,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        self.execution_resources.cancel(
            ToolExecutionContextQuery::for_taskless_session(session_id),
            reason,
            UtcMillis::now(),
        )
    }

    /// 按 TaskStore 当前拥有方的执行范围取消资源；已转交子树及同会话的其他执行
    /// 由各自拥有方结算。Session 关闭使用 session 级资源入口，覆盖全部拥有方。
    /// 即使原 Task 已被收口或移除，仍结算调用方明确指定的 root 身份。
    pub(crate) fn cancel_task_execution_scope_resources(
        &self,
        session_id: &SessionId,
        root_task_id: &TaskId,
        reason: BrowserLeaseEndReason,
    ) -> ExecutionResourceCancellationReport {
        let mut task_ids = self
            .task_store()
            .map(|store| store.collect_execution_subtree_ids(root_task_id))
            .unwrap_or_default();
        if !task_ids.contains(root_task_id) {
            task_ids.push(root_task_id.clone());
        }
        let mut report = ExecutionResourceCancellationReport::default();
        for task_id in task_ids {
            let cancelled =
                self.cancel_execution_resources(Some(session_id), None, Some(&task_id), reason);
            report.tool_execution_count = report
                .tool_execution_count
                .saturating_add(cancelled.tool_execution_count);
            report.browser_lease_count = report
                .browser_lease_count
                .saturating_add(cancelled.browser_lease_count);
        }
        report
    }

    /// Goal 控制版本是现有执行撤销事实。修改/暂停/清除后，主动结算其已转交的
    /// 执行范围与资源；不触及 Session 拥有的分支，也不从主轮当前指针猜测归属。
    pub(crate) async fn settle_revoked_goal_agent_runs(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, ApiError> {
        let Some(store) = self.task_store.clone() else {
            return Ok(0);
        };
        let goals = self.session_store.goals_for_session(session_id);
        let roots = TaskExecutionRegistry::revoked_goal_agent_roots(
            &store,
            &self.session_store,
            session_id,
        );
        let mut errors = Vec::new();
        for root in &roots {
            let manager = self.runner_manager();
            let _restart_guard = match manager {
                Some(manager) => Some(manager.lock_for_restart(root.task_id.as_str()).await),
                None => None,
            };
            let goal_is_paused = root.agent_run_transfer().is_some_and(|transfer| {
                matches!(&transfer.owner, magi_core::AgentRunOwner::Goal { goal_id, .. }
                    if goals.iter().any(|goal| &goal.goal_id == goal_id && goal.status == magi_session_store::GoalStatus::Paused))
            });
            let end_reason = if goal_is_paused {
                BrowserLeaseEndReason::GoalPaused
            } else {
                BrowserLeaseEndReason::OwnerReleased
            };
            self.cancel_task_execution_scope_resources(session_id, &root.task_id, end_reason);
            let registry = self.task_execution_registry.clone();
            let execution_store = store.clone();
            let execution_session = session_id.clone();
            let execution_root = root.task_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                registry.settle_revoked_goal_agent_run(
                    &execution_store,
                    &execution_session,
                    &execution_root,
                )
            })
            .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => errors.push(error),
                Err(error) => errors.push(format!("目标代理结算线程退出：{error}")),
            }

            if let Some(manager) = manager {
                manager.quiesce_for_restart(root.task_id.as_str()).await;
            } else if let Some(dispatcher) = self.session_turn_dispatcher() {
                dispatcher.wait_for_quiesce(&root.task_id).await;
            }
            let ids = store.collect_execution_subtree_ids(&root.task_id);
            if ids.iter().filter_map(|id| store.get_task(id)).any(|task| {
                matches!(
                    task.status,
                    magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                )
            }) || self.tool_registry().is_some_and(|registry| {
                ids.iter().any(|id| {
                    registry.has_active_executions(&ToolExecutionContextQuery {
                        session_id: Some(session_id.clone()),
                        task_id: Some(id.clone()),
                        ..Default::default()
                    })
                })
            }) {
                errors.push(format!("目标代理 {} 尚未完成资源结算", root.task_id));
            }
        }
        if errors.is_empty() {
            Ok(roots.len())
        } else {
            Err(ApiError::Conflict(format!(
                "目标已更新，部分代理结算失败：{}",
                errors.join("；")
            )))
        }
    }
}

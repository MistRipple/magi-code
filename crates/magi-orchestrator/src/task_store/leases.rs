use super::*;

impl TaskStore {
    pub(super) fn validate_lease_owner(
        tasks: &HashMap<TaskId, Task>,
        lease: &TaskLease,
    ) -> DomainResult<()> {
        Self::validate_task_root(tasks, &lease.task_id, &lease.root_task_id)
    }

    pub(super) fn validate_lease_contract(
        tasks: &HashMap<TaskId, Task>,
        lease: &TaskLease,
    ) -> DomainResult<()> {
        Self::validate_lease_owner(tasks, lease)?;
        if lease.lease_status == TaskLeaseState::Active {
            let task = tasks
                .get(&lease.task_id)
                .ok_or(DomainError::NotFound { entity: "Task" })?;
            if task.status != TaskStatus::Running {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 的任务 {} 必须处于 Running，当前为 {:?}",
                        lease.lease_id, lease.task_id, task.status
                    ),
                });
            }
        }
        Ok(())
    }

    pub(super) fn build_lease(
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> TaskLease {
        let now = UtcMillis::now();
        let counter = LEASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let lease_id = LeaseId::new(format!("lease-{}-{}", now.0, counter));
        TaskLease {
            lease_id,
            task_id: task_id.clone(),
            root_task_id: root_task_id.clone(),
            worker_id: worker_id.clone(),
            role: role.to_string(),
            granted_at: now,
            expires_at: UtcMillis(now.0.saturating_add(duration_ms)),
            heartbeat_at: now,
            lease_status: TaskLeaseState::Active,
        }
    }

    /// 插入已有租约事实。
    ///
    /// 这是恢复/测试建 fixture 的底层入口，不负责推进任务状态。运行时必须使用
    /// `grant_lease_and_start_task`，避免把租约和任务状态拆成两个事实提交。
    pub fn insert_lease(&self, lease: TaskLease) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::validate_lease_contract(&tasks, &lease)?;
        let leases = self.leases.read().expect("leases read lock poisoned");
        if leases.contains_key(&lease.lease_id) {
            return Err(DomainError::AlreadyExists { entity: "Lease" });
        }
        if lease.lease_status == TaskLeaseState::Active
            && leases.values().any(|existing| {
                existing.task_id == lease.task_id && existing.lease_status == TaskLeaseState::Active
            })
        {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 已有活跃租约，禁止重复持有", lease.task_id),
            });
        }
        drop(leases);
        let lease_id = lease.lease_id.clone();
        self.leases
            .write()
            .expect("leases write lock poisoned")
            .insert(lease_id, lease);
        Ok(())
    }

    /// 获取指定任务的活跃租约。
    pub fn get_active_lease(&self, task_id: &TaskId) -> Option<TaskLease> {
        self.leases
            .read()
            .expect("leases read lock poisoned")
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
            .cloned()
    }

    /// 执行资格同时属于本次租约和仍活跃的父链。父任务终态的级联清理可能尚未
    /// 运行，不能仅凭子任务仍持有 Active lease 继续执行。已转交子树只检查到
    /// 已验证的转交根；Session/Goal 与 canonical Turn 准入继续由运行时检查。
    pub fn execution_lease_is_current(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let leases = self.leases.read().expect("leases read lock poisoned");
        let Some(lease) = leases.get(lease_id).filter(|lease| {
            &lease.task_id == task_id && lease.lease_status == TaskLeaseState::Active
        }) else {
            return false;
        };
        let Some(task) = tasks.get(task_id).filter(|task| {
            task.status == TaskStatus::Running && task.root_task_id == lease.root_task_id
        }) else {
            return false;
        };
        Self::ancestor_chain_is_active(task, &tasks, false)
    }

    /// 获取租约历史，用于恢复审计和终态断言。
    pub fn get_lease(&self, lease_id: &LeaseId) -> Option<TaskLease> {
        self.leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
            .cloned()
    }

    /// 为任务授予新的执行租约。如果任务已有活跃租约则返回 None。
    pub fn grant_lease(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> Option<TaskLease> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if Self::validate_task_root(&tasks, task_id, root_task_id).is_err() {
            return None;
        }
        if tasks
            .get(task_id)
            .is_none_or(|task| task.status != TaskStatus::Running)
        {
            return None;
        }
        let mut leases = self.leases.write().expect("leases write lock poisoned");

        if let Some(active_lease) = leases
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
        {
            if active_lease.root_task_id != *root_task_id {
                return None;
            }
            if Self::validate_lease_contract(&tasks, active_lease).is_err() {
                return None;
            }
            return None;
        }

        let lease = Self::build_lease(task_id, root_task_id, worker_id, role, duration_ms);
        let lease_id = lease.lease_id.clone();

        leases.insert(lease_id, lease.clone());
        Some(lease)
    }

    /// 在一个 TaskStore 事务中授予租约并把任务推进到 `Running`。
    ///
    /// 返回 `Ok(None)` 表示扫描后任务已被领取、已结束或父链已不可调度。
    /// 这些竞争不应使其他可运行分支失败；归属错误或 checkpoint 错误仍返回错误。
    pub fn grant_lease_and_start_task(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        self.grant_lease_and_start_task_inner(
            task_id,
            root_task_id,
            None,
            worker_id,
            role,
            duration_ms,
        )
    }

    /// Runner 按执行拥有方领取，扫描与转交/取消竞争时在同一 mutation 锁下复核。
    /// Lease 的 root_task_id 始终保留原审计 root，不能改成当前执行拥有方。
    pub fn claim_task_for_execution_scope(
        &self,
        task_id: &TaskId,
        scope_root: &TaskId,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        let task = self
            .get_task(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        self.grant_lease_and_start_task_inner(
            task_id,
            &task.root_task_id,
            Some(scope_root),
            worker_id,
            role,
            duration_ms,
        )
    }

    pub(super) fn grant_lease_and_start_task_inner(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        scope_root: Option<&TaskId>,
        worker_id: &WorkerId,
        role: &str,
        duration_ms: u64,
    ) -> DomainResult<Option<TaskLease>> {
        let transaction_started_at = Instant::now();
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        Self::validate_task_root(&tasks, task_id, root_task_id)?;
        if let Some(scope) = scope_root
            && Self::execution_scope_root_in_tasks(&tasks, task_id)? != *scope
        {
            return Ok(None);
        }
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if task.status != TaskStatus::Pending {
            return Ok(None);
        }
        // Runner 扫描与这里的事务之间父任务可能已结束，必须在授予前重新检查。
        if !Self::ancestor_chain_allows_dispatch_inner(task, &tasks) {
            return Ok(None);
        }

        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        if let Some(active_lease) = leases
            .values()
            .find(|lease| lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active)
            .cloned()
        {
            Self::validate_lease_owner(&tasks, &active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "任务 {} 的活跃租约 {} 不属于 root task {}",
                        task_id, active_lease.lease_id, root_task_id
                    ),
                });
            }
            return Ok(None);
        }

        let lease = Self::build_lease(task_id, root_task_id, worker_id, role, duration_ms);
        leases.insert(lease.lease_id.clone(), lease.clone());
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        task.status = TaskStatus::Running;
        task.updated_at = UtcMillis::now();
        let cloned_task = task.clone();
        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        tracing::info!(
            target: "magi.performance",
            task_id = %task_id,
            root_task_id = %root_task_id,
            elapsed_ms = transaction_started_at.elapsed().as_millis() as u64,
            stage = "task_lease_checkpoint_returned",
            "conversation response timing"
        );
        self.commit_maps(tasks, leases, mission_index);
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, TaskStatus::Running, cloned_task);
        tracing::info!(
            target: "magi.performance",
            task_id = %task_id,
            root_task_id = %root_task_id,
            elapsed_ms = transaction_started_at.elapsed().as_millis() as u64,
            stage = "task_lease_transaction_completed",
            "conversation response timing"
        );
        Ok(Some(lease))
    }

    /// 撤销活跃租约。
    pub fn revoke_lease(&self, task_id: &TaskId, lease_id: &LeaseId) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        let Some(lease) = leases.get_mut(lease_id) else {
            return false;
        };
        if lease.task_id != *task_id
            || lease.lease_status != TaskLeaseState::Active
            || Self::validate_lease_contract(&tasks, lease).is_err()
        {
            return false;
        }
        lease.lease_status = TaskLeaseState::Revoked;
        true
    }

    /// 在一个 TaskStore 事务中完成指定租约和任务。
    ///
    /// 不存在、已经失效的租约或已关闭的父链返回 `Ok(false)`，用于丢弃迟到结果。合同校验
    /// 与 checkpoint 错误会原样返回，绝不能通过读取任务内存状态猜测错误类型。
    pub fn complete_lease_and_task(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: &LeaseId,
        attempt: TaskCompletionAttempt,
    ) -> DomainResult<bool> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        let Some(lease) = leases.get(lease_id).cloned() else {
            return Ok(false);
        };
        if lease.lease_status != TaskLeaseState::Active {
            return Ok(false);
        }
        if lease.task_id != *task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于任务 {}", lease_id, task_id),
            });
        }
        if lease.root_task_id != *root_task_id {
            return Err(DomainError::InvalidState {
                message: format!("租约 {} 不属于 root task {}", lease_id, root_task_id),
            });
        }
        Self::validate_task_root(&tasks, task_id, root_task_id)?;
        Self::validate_lease_contract(&tasks, &lease)?;
        // 执行循环的检查与结果提交不是同一事务；父任务可能在结果返回途中结束。
        // 原 Task/lease 留给既有级联终止入口结算，不能把迟到成功写成新的终态。
        if tasks
            .get(task_id)
            .is_none_or(|task| !Self::ancestor_chain_is_active(task, &tasks, false))
        {
            return Ok(false);
        }
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        Self::apply_completion(task, attempt)?;
        let cloned_task = task.clone();
        leases
            .get_mut(lease_id)
            .ok_or(DomainError::NotFound { entity: "Lease" })?
            .lease_status = TaskLeaseState::Completed;
        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, TaskStatus::Completed, cloned_task);
        Ok(true)
    }

    /// 原子撤销指定租约并把任务推进到 `Failed` 或 `Killed`。
    ///
    /// `lease_id` 为 `None` 只允许没有活跃租约的 Pending 任务执行，适用于尚未派发
    /// 的任务被终止；Running 任务必须携带调用方观察到的租约 ID，避免误收口新执行轮。
    pub fn revoke_lease_and_set_task_terminal(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            root_task_id,
            lease_id,
            status,
            output_refs,
            false,
            None,
        )
    }

    /// 用户取消语义：在 TaskStore mutation lock 内读取并撤销当前活跃租约，
    /// 避免 Runner 在调用方“先读 lease、再写终态”的窗口里刚好授予新 lease。
    pub fn revoke_active_lease_and_set_task_terminal(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            root_task_id,
            None,
            status,
            output_refs,
            true,
            None,
        )
    }

    /// 过期扫描与转交/续租竞争时，只有仍属于本执行范围且确实过期的原租约可失败。
    pub fn expire_lease_for_execution_scope(
        &self,
        scope_root: &TaskId,
        task_id: &TaskId,
        lease_id: &LeaseId,
    ) -> DomainResult<bool> {
        let task = self
            .get_task(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        self.revoke_lease_and_set_task_terminal_inner(
            task_id,
            &task.root_task_id,
            Some(lease_id),
            TaskStatus::Failed,
            vec![format!("任务 {task_id} 租约已过期")],
            false,
            Some(scope_root),
        )
    }

    /// 在一次 TaskStore mutation 内收口整棵任务树。子任务列表、租约和终态使用同一
    /// 快照，避免调用方先扫描再逐项取消期间新任务/租约穿过取消屏障。损坏的单个
    /// 节点会被报告，但不会阻止其余合法节点在同一 checkpoint 中结算。
    pub fn revoke_active_leases_and_set_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            root_task_id,
            status,
            output_refs,
            SubtreeTerminalSelection::Audit,
        )
    }

    /// 终止当前拥有方的执行范围，不跨越已提交的转交边界。会话关闭等整体销毁
    /// 仍使用完整子树入口，以免留下已转交执行。
    pub fn revoke_active_leases_and_set_execution_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            root_task_id,
            status,
            output_refs,
            SubtreeTerminalSelection::Execution,
        )
    }

    /// 父终态、执行拥有方和后代结算在同一 mutation 中检查；不使用 Runner 的旧扫描
    /// 结果决定取消范围，也不跨过已提交的转交边界。
    pub fn revoke_orphaned_execution_descendants(&self, scope_root: &TaskId) -> DomainResult<bool> {
        self.set_subtree_terminal(
            scope_root,
            TaskStatus::Killed,
            Vec::new(),
            SubtreeTerminalSelection::OrphanedExecution,
        )
    }

    /// Runner 报告不可运行后，候选可能已完成或转交。只有仍属于该拥有方的活跃
    /// 且尚未取得执行租约的候选才能触发失败；检查与终态提交不能分开，否则会
    /// 误停仍正常执行的主线。
    pub fn fail_unrunnable_execution_scope(
        &self,
        scope_root: &TaskId,
        candidates: &[TaskId],
        output_refs: Vec<String>,
    ) -> DomainResult<bool> {
        self.set_subtree_terminal(
            scope_root,
            TaskStatus::Failed,
            output_refs,
            SubtreeTerminalSelection::ExecutionIfActive(candidates),
        )
    }

    pub(super) fn set_subtree_terminal(
        &self,
        root_task_id: &TaskId,
        status: TaskStatus,
        output_refs: Vec<String>,
        selection: SubtreeTerminalSelection<'_>,
    ) -> DomainResult<bool> {
        if !matches!(status, TaskStatus::Failed | TaskStatus::Killed) {
            return Err(DomainError::InvalidState {
                message: "子树批量结算只能写入 Failed 或 Killed 终态".into(),
            });
        }
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let audit_root_id = tasks
            .get(root_task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?
            .root_task_id
            .clone();
        let execution_only = !matches!(selection, SubtreeTerminalSelection::Audit);
        let mut task_ids = collect_subtree_ids_in_scope(&tasks, root_task_id, execution_only);
        let leases = self.leases.read().expect("leases read lock poisoned");
        match selection {
            SubtreeTerminalSelection::ExecutionIfActive(candidates) => {
                if !task_ids.iter().any(|id| {
                    candidates.contains(id)
                        && tasks
                            .get(id)
                            .is_some_and(|task| !is_terminal_status(task.status))
                        && !leases.values().any(|lease| {
                            lease.task_id == *id && lease.lease_status == TaskLeaseState::Active
                        })
                }) {
                    return Ok(false);
                }
            }
            SubtreeTerminalSelection::OrphanedExecution => {
                // 遍历保证父节点先于后代；一次扫描传播终态祖先，不反复扫描每棵子树。
                // 已转交节点及其后代不在当前 scope 中，不能被原 Runner 清理。
                let mut closed_branches = HashSet::new();
                task_ids.retain(|id| {
                    let task = &tasks[id];
                    let orphaned = task
                        .parent_task_id
                        .as_ref()
                        .is_some_and(|parent| closed_branches.contains(parent));
                    if orphaned || is_terminal_status(task.status) {
                        closed_branches.insert(id.clone());
                    }
                    orphaned
                });
            }
            SubtreeTerminalSelection::Audit | SubtreeTerminalSelection::Execution => {}
        }
        if task_ids.is_empty() {
            return Ok(false);
        }
        let candidate_tasks = tasks.clone();
        drop(tasks);
        let mut tasks = candidate_tasks;
        let candidate_leases = leases.clone();
        drop(leases);
        let mut leases = candidate_leases;
        let mut errors = Vec::new();
        let mut changes = Vec::new();
        for task_id in task_ids {
            match Self::prepare_task_terminal_in_maps(
                &mut tasks,
                &mut leases,
                &task_id,
                &audit_root_id,
                None,
                status,
                output_refs.clone(),
                true,
            ) {
                Ok(Some((old_status, task))) => changes.push((task_id, old_status, task)),
                Ok(None) => {}
                Err(error) => errors.push(format!("任务 {task_id}: {error}")),
            }
        }
        if !changes.is_empty() {
            let mission_index = self
                .mission_index
                .read()
                .expect("mission index lock poisoned")
                .clone();
            let snapshot =
                Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![audit_root_id]);
            self.fire_checkpoint(&snapshot)?;
            self.commit_maps(tasks, leases, mission_index);
        }
        drop(_mutation_guard);
        for (task_id, old_status, task) in &changes {
            self.emit_status_change(task_id, *old_status, status, task.clone());
        }
        if errors.is_empty() {
            Ok(!changes.is_empty())
        } else {
            Err(DomainError::InvalidState {
                message: errors.join("；"),
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn revoke_lease_and_set_task_terminal_inner(
        &self,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
        discover_active_lease: bool,
        expiry_scope: Option<&TaskId>,
    ) -> DomainResult<bool> {
        if !matches!(status, TaskStatus::Failed | TaskStatus::Killed) {
            return Err(DomainError::InvalidState {
                message: format!("任务终态只能是 Failed 或 Killed，收到 {:?}", status),
            });
        }
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        if let Some(scope) = expiry_scope
            && (Self::execution_scope_root_in_tasks(&tasks, task_id)? != *scope
                || lease_id.and_then(|id| leases.get(id)).is_none_or(|lease| {
                    lease.lease_status != TaskLeaseState::Active
                        || lease.expires_at >= UtcMillis::now()
                }))
        {
            return Ok(false);
        }
        let Some((old_status, cloned_task)) = Self::prepare_task_terminal_in_maps(
            &mut tasks,
            &mut leases,
            task_id,
            root_task_id,
            lease_id,
            status,
            output_refs,
            discover_active_lease,
        )?
        else {
            return Ok(false);
        };
        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![cloned_task.root_task_id.clone()],
        );
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        drop(_mutation_guard);
        self.emit_status_change(task_id, old_status, status, cloned_task);
        Ok(true)
    }

    /// 只在候选 maps 中准备终态；单任务与整子树取消共用同一租约/状态校验。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_task_terminal_in_maps(
        tasks: &mut HashMap<TaskId, Task>,
        leases: &mut HashMap<LeaseId, TaskLease>,
        task_id: &TaskId,
        root_task_id: &TaskId,
        lease_id: Option<&LeaseId>,
        status: TaskStatus,
        output_refs: Vec<String>,
        discover_active_lease: bool,
    ) -> DomainResult<Option<(TaskStatus, Task)>> {
        Self::validate_task_root(tasks, task_id, root_task_id)?;
        let active_leases = leases
            .values()
            .filter(|lease| {
                lease.task_id == *task_id && lease.lease_status == TaskLeaseState::Active
            })
            .cloned()
            .collect::<Vec<_>>();
        for active_lease in &active_leases {
            Self::validate_lease_contract(tasks, active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 不属于 root task {}",
                        active_lease.lease_id, root_task_id
                    ),
                });
            }
        }
        if discover_active_lease && active_leases.len() > 1 {
            return Err(DomainError::InvalidState {
                message: format!("任务 {} 同时拥有多个活跃租约，不能安全取消", task_id),
            });
        }
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if is_terminal_status(task.status) {
            if !active_leases.is_empty() {
                return Err(DomainError::InvalidState {
                    message: format!("终态任务 {} 不能拥有活跃租约，必须先修复任务投影", task_id),
                });
            }
            return Ok(None);
        }
        let requested_lease = if discover_active_lease {
            active_leases.first().cloned()
        } else {
            match lease_id {
                Some(expected) => {
                    let Some(lease) = leases.get(expected).cloned() else {
                        return Ok(None);
                    };
                    if lease.task_id != *task_id {
                        return Err(DomainError::InvalidState {
                            message: format!("租约 {} 不属于任务 {}", expected, task_id),
                        });
                    }
                    Self::validate_lease_contract(tasks, &lease)?;
                    if lease.root_task_id != *root_task_id {
                        return Err(DomainError::InvalidState {
                            message: format!("租约 {} 不属于 root task {}", expected, root_task_id),
                        });
                    }
                    if lease.lease_status != TaskLeaseState::Active {
                        return Ok(None);
                    }
                    Some(lease)
                }
                None => None,
            }
        };
        let active_lease_id = active_leases.first().map(|lease| lease.lease_id.clone());
        if let Some(active_lease) = active_lease_id
            .as_ref()
            .and_then(|active_id| leases.get(active_id))
        {
            Self::validate_lease_contract(tasks, active_lease)?;
            if active_lease.root_task_id != *root_task_id {
                return Err(DomainError::InvalidState {
                    message: format!(
                        "活跃租约 {} 不属于 root task {}",
                        active_lease.lease_id, root_task_id
                    ),
                });
            }
        }
        match (requested_lease.as_ref(), active_lease_id.as_ref()) {
            (Some(expected), Some(actual)) if expected.lease_id != *actual => {
                return Ok(None);
            }
            (Some(_), None) => {
                return Ok(None);
            }
            (None, Some(_)) => {
                return Err(DomainError::InvalidState {
                    message: format!("任务 {} 仍有活跃租约，必须指定租约 ID", task_id),
                });
            }
            // 没有活跃租约的 Running 任务是重启或 runner 崩溃后的陈旧执行态。
            // 用户终止和恢复收敛必须能够把它写成终态；此时不存在需要撤销的
            // 新租约，因此不会误伤另一轮执行。
            (None, None) if task.status == TaskStatus::Running => {}
            _ => {}
        }

        if requested_lease.is_some() && task.status != TaskStatus::Running {
            return Err(DomainError::InvalidState {
                message: format!("带租约的任务 {} 必须处于 Running", task_id),
            });
        }

        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let old_status = task.status;
        Self::apply_terminal_status(task, status, output_refs)?;
        let cloned_task = task.clone();
        if let Some(expected) = requested_lease.as_ref() {
            leases
                .get_mut(&expected.lease_id)
                .ok_or(DomainError::NotFound { entity: "Lease" })?
                .lease_status = TaskLeaseState::Revoked;
        }
        Ok(Some((old_status, cloned_task)))
    }

    /// 更新当前执行作用域内活跃租约的心跳时间。
    pub fn heartbeat_lease_for_execution_scope(
        &self,
        scope_root: &TaskId,
        task_id: &TaskId,
        lease_id: &LeaseId,
    ) -> bool {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if !Self::execution_scope_root_in_tasks(&tasks, task_id)
            .is_ok_and(|root| root == *scope_root)
        {
            return false;
        }
        let Some(lease) = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .get(lease_id)
            .cloned()
        else {
            return false;
        };
        if lease.task_id != *task_id
            || lease.lease_status != TaskLeaseState::Active
            || Self::validate_lease_contract(&tasks, &lease).is_err()
        {
            return false;
        }
        drop(tasks);
        let mut leases = self.leases.write().expect("leases write lock poisoned");
        let Some(lease) = leases.get_mut(lease_id) else {
            return false;
        };
        if lease.task_id != *task_id || lease.lease_status != TaskLeaseState::Active {
            return false;
        }
        let now = UtcMillis::now();
        let lease_ttl_ms = lease.expires_at.0.saturating_sub(lease.granted_at.0);
        lease.heartbeat_at = now;
        lease.expires_at = UtcMillis(now.0.saturating_add(lease_ttl_ms));
        true
    }

    /// 扫描当前执行范围中已过期的活跃租约，不修改状态。
    pub fn collect_expired_leases(&self, scope_root: &TaskId) -> Vec<(TaskId, LeaseId)> {
        self.collect_leases_for_execution_scope(scope_root, true)
    }

    /// 获取当前执行范围中未过期的活跃租约。
    pub fn collect_active_leases(&self, scope_root: &TaskId) -> Vec<(TaskId, LeaseId)> {
        self.collect_leases_for_execution_scope(scope_root, false)
    }

    /// 仅为当前调度拥有方收集租约，原 root 不能再为已转交执行续租或报告过期。
    pub(super) fn collect_leases_for_execution_scope(
        &self,
        scope_root: &TaskId,
        expired: bool,
    ) -> Vec<(TaskId, LeaseId)> {
        let now = UtcMillis::now();
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let ids = collect_subtree_ids_in_scope(&tasks, scope_root, true)
            .into_iter()
            .collect::<HashSet<_>>();
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|lease| {
                lease.lease_status == TaskLeaseState::Active
                    && (lease.expires_at < now) == expired
                    && ids.contains(&lease.task_id)
            })
            .map(|lease| (lease.task_id.clone(), lease.lease_id.clone()))
            .collect()
    }

    /// 获取所有活跃租约（不区分 root_task_id）。
    /// 仅在 reconcile / checkpoint restore 等全量收敛场景使用。
    pub fn collect_all_active_leases(&self) -> Vec<(TaskId, LeaseId)> {
        let leases = self.leases.read().expect("leases read lock poisoned");
        leases
            .values()
            .filter(|l| l.lease_status == TaskLeaseState::Active)
            .map(|l| (l.task_id.clone(), l.lease_id.clone()))
            .collect()
    }

    /// 在进程重启后收敛易失执行态。
    ///
    /// 本轮只保证逻辑续接，不恢复进程内瞬时现场，因此 restore 后的 Active lease
    /// 仍然不能被视为真实运行态。TaskStore 自身的 Pending/Running 与租约事实
    /// 决定受影响的 execution scope，不依赖 sidecar 或旧 Worker 阶段快照。
    pub fn reconcile_volatile_runtime_after_restore(&self) -> DomainResult<(usize, usize)> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let mut leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        for lease in leases.values() {
            Self::validate_lease_contract(&tasks, lease)?;
        }
        let active_leases = leases
            .values()
            .filter(|lease| lease.lease_status == TaskLeaseState::Active)
            .map(|lease| (lease.task_id.clone(), lease.lease_id.clone()))
            .collect::<Vec<_>>();
        let interrupted_task_ids = tasks
            .values()
            .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Running))
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        if active_leases.is_empty() && interrupted_task_ids.is_empty() {
            return Ok((0, 0));
        }

        let mut affected_roots = HashSet::new();
        let mut affected_scopes = HashSet::new();
        for task_id in active_leases
            .iter()
            .map(|(task_id, _)| task_id)
            .chain(interrupted_task_ids.iter())
        {
            let scope = Self::execution_scope_root_in_tasks(&tasks, task_id)?;
            affected_roots.insert(tasks[task_id].root_task_id.clone());
            affected_scopes.insert(scope);
        }
        for (task_id, lease_id) in &active_leases {
            if let Some(lease) = leases.get_mut(lease_id)
                && lease.task_id == *task_id
                && lease.lease_status == TaskLeaseState::Active
            {
                lease.lease_status = TaskLeaseState::Revoked;
            }
        }
        let now = UtcMillis::now();
        let mut failed_count = 0usize;
        for scope in &affected_scopes {
            for task_id in collect_subtree_ids_in_scope(&tasks, scope, true) {
                if let Some(task) = tasks.get_mut(&task_id)
                    && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                {
                    Self::apply_terminal_status(task, TaskStatus::Failed, Vec::new())?;
                    task.updated_at = now;
                    failed_count += 1;
                }
            }
        }

        let mission_index = self
            .mission_index
            .read()
            .expect("mission_index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, affected_roots);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, mission_index);
        self.notify_status_change();
        Ok((active_leases.len(), failed_count))
    }

    // ------------------------------------------------------------------
    // Checkpoint / Restore
    // ------------------------------------------------------------------
}

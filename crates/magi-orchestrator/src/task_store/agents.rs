use super::*;

impl TaskStore {
    /// 根据 Task 中的不可变 activation 来源读取会话拥有的代理，不另存身份注册表。
    pub fn agent_heads_for_session(&self, session_id: &magi_core::SessionId) -> Vec<Task> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let mut heads = std::collections::BTreeMap::<String, &Task>::new();
        for task in tasks.values() {
            let Some(activation) = task
                .agent_activation()
                .filter(|binding| &binding.session_id == session_id)
            else {
                continue;
            };
            let entry = heads.entry(activation.agent_id.to_string()).or_insert(task);
            if activation.generation > entry.agent_activation().expect("agent task").generation {
                *entry = task;
            }
        }
        heads.into_values().cloned().collect()
    }

    /// 两层代理试点以真实父链校验，不从展示名称推断深度。
    pub fn validate_agent_delegation(&self, parent: &Task) -> DomainResult<()> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::validate_agent_delegation_inner(&tasks, parent)
    }

    /// 根代理可随会话进入新的主线 Turn；嵌套代理只能由同一父 Agent 的新 activation 继续。
    pub fn agent_resume_parent_matches(&self, source: &Task, parent: &Task) -> bool {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        Self::same_agent_owner(
            source.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
            parent,
        )
    }

    pub(super) fn same_agent_owner(previous_parent: Option<&Task>, parent: &Task) -> bool {
        let Some(previous) = previous_parent else {
            return false;
        };
        if previous.parent_task_id.is_none() || parent.parent_task_id.is_none() {
            return previous.parent_task_id.is_none() && parent.parent_task_id.is_none();
        }
        match (previous.agent_activation(), parent.agent_activation()) {
            (Some(previous), Some(current)) => {
                previous.agent_id == current.agent_id && previous.session_id == current.session_id
            }
            _ => false,
        }
    }

    pub(super) fn validate_agent_delegation_inner(
        tasks: &HashMap<TaskId, Task>,
        parent: &Task,
    ) -> DomainResult<()> {
        let latest = tasks
            .get(&parent.task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        if !matches!(latest.status, TaskStatus::Pending | TaskStatus::Running)
            || latest.parent_task_id != parent.parent_task_id
            || latest.root_task_id != parent.root_task_id
            || latest.mission_id != parent.mission_id
            || latest.delegation_allowed() != parent.delegation_allowed()
        {
            return Err(DomainError::InvalidState {
                message: "派发父任务已失效或委派授权已变化".into(),
            });
        }
        if parent.parent_task_id.is_none() && parent.root_task_id == parent.task_id {
            return Ok(());
        }
        let root = tasks
            .get(&parent.root_task_id)
            .ok_or(DomainError::NotFound { entity: "RootTask" })?;
        if !parent.delegation_allowed()
            || parent.parent_task_id.as_ref() != Some(&root.task_id)
            || root.parent_task_id.is_some()
            || root.task_id != root.root_task_id
            || root.mission_id != parent.mission_id
            || (root.status != TaskStatus::Running && latest.agent_run_transfer().is_none())
            || root.executor_binding_target_role() != Some("coordinator")
            || !Self::ancestor_chain_is_active(latest, tasks, false)
        {
            return Err(DomainError::InvalidState {
                message: "嵌套派发需要主线显式委派，且最多两层代理".into(),
            });
        }
        Ok(())
    }

    pub(super) fn validate_agent_activation_insert(
        tasks: &HashMap<TaskId, Task>,
        task: &Task,
    ) -> DomainResult<()> {
        Self::validate_project_directory_inheritance(
            task,
            task.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
        )?;
        if task.agent_run_transfer().is_some() {
            return Err(DomainError::InvalidState {
                message: "创建任务不能携带已转交状态，必须从显式转交事务接纳".into(),
            });
        }
        let owner = task
            .parent_task_id
            .as_ref()
            .map(|parent| Self::agent_run_transfer_in_tasks(tasks, parent))
            .transpose()?
            .flatten()
            .map(|(root, _)| &root.task_id);
        // 转交后创建的新任务同样不能重新建立跨拥有方依赖。
        for dependency in task.dependency_ids.iter().chain(&task.required_children) {
            let dependency_owner = Self::agent_run_transfer_in_tasks(tasks, dependency)?
                .map(|(root, _)| &root.task_id);
            if owner != dependency_owner {
                return Err(DomainError::InvalidState {
                    message: "新任务不能建立跨转交子树的调度或完成依赖".into(),
                });
            }
        }
        for candidate in tasks.values() {
            if candidate
                .dependency_ids
                .iter()
                .chain(&candidate.required_children)
                .any(|id| id == &task.task_id)
                && Self::agent_run_transfer_in_tasks(tasks, &candidate.task_id)?
                    .map(|(root, _)| &root.task_id)
                    != owner
            {
                return Err(DomainError::InvalidState {
                    message: "任务身份被另一拥有方预留为调度或完成依赖".into(),
                });
            }
        }
        let Some(binding) = task.agent_activation() else {
            if task.delegation_allowed() || owner.is_some() {
                return Err(DomainError::InvalidState {
                    message: "受委派或已转交子树中的代理必须有持久 activation 身份".into(),
                });
            }
            return Ok(());
        };
        binding.validate(&task.task_id)?;
        let parent = task
            .parent_task_id
            .as_ref()
            .and_then(|id| tasks.get(id))
            .ok_or_else(|| DomainError::InvalidState {
                message: "Agent activation 缺少父任务".into(),
            })?;
        if !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
            || parent.root_task_id != task.root_task_id
            || parent.mission_id != task.mission_id
        {
            return Err(DomainError::InvalidState {
                message: "Agent activation 父任务已失效或作用域不匹配".into(),
            });
        }
        if parent.parent_task_id.is_some() || task.delegation_allowed() {
            Self::validate_agent_delegation_inner(tasks, parent)?;
            if (task.delegation_allowed()
                && (parent.parent_task_id.is_some()
                    || parent.executor_binding_target_role() != Some("coordinator")))
                || task.workspace_scope != parent.workspace_scope
                || task.write_scope != parent.write_scope
                || task.policy_snapshot != parent.policy_snapshot
            {
                return Err(DomainError::InvalidState {
                    message: "嵌套任务不能增加委派深度或改变父任务授权边界".into(),
                });
            }
        }
        if let Some((root, transfer)) = Self::agent_run_transfer_in_tasks(tasks, &parent.task_id)? {
            Self::validate_agent_transfer_subtree(
                tasks.values().chain(std::iter::once(task)),
                root,
                transfer,
            )?;
        }
        let prior = tasks
            .values()
            .filter(|candidate| {
                candidate
                    .agent_activation()
                    .is_some_and(|source| source.agent_id == binding.agent_id)
            })
            .max_by_key(|candidate| candidate.agent_activation().expect("agent task").generation);
        match (prior, &binding.previous_task_id) {
            (None, None) if binding.generation == 1 => Ok(()),
            (Some(prior), Some(previous)) => {
                let source = prior.agent_activation().expect("agent task");
                if prior.task_id != *previous
                    || prior.status != TaskStatus::Completed
                    || source.generation.checked_add(1) != Some(binding.generation)
                    || source.session_id != binding.session_id
                    || source.lifetime != binding.lifetime
                    || matches!(source.lifetime, magi_core::AgentLifetime::Turn)
                    || (task.delegation_allowed() && !prior.delegation_allowed())
                    || prior.mission_id != task.mission_id
                    || !Self::same_agent_owner(
                        prior.parent_task_id.as_ref().and_then(|id| tasks.get(id)),
                        parent,
                    )
                    || prior.workspace_scope != task.workspace_scope
                    || prior.write_scope != task.write_scope
                    || prior.executor_binding_target_role() != task.executor_binding_target_role()
                    || task
                        .executor_binding_capability_ids()
                        .iter()
                        .any(|capability| {
                            !prior.executor_binding_capability_ids().contains(capability)
                        })
                {
                    return Err(DomainError::InvalidState {
                        message: "Agent 已有更新/活动 activation 或来源作用域冲突".into(),
                    });
                }
                Ok(())
            }
            _ => Err(DomainError::InvalidState {
                message: "Agent activation 来源缺失或代际冲突".into(),
            }),
        }
    }

    pub(super) fn validate_project_directory_inheritance(
        task: &Task,
        parent: Option<&Task>,
    ) -> DomainResult<()> {
        let binding = task
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.project_directories.as_ref());
        let parent_binding = parent
            .and_then(|task| task.executor_binding.as_ref())
            .and_then(|binding| binding.project_directories.as_ref());
        if let Some(binding) = binding {
            binding.validate()?;
        }
        match (binding, parent_binding, task.parent_task_id.as_ref()) {
            (Some(child), Some(parent), Some(_)) => child.validate_inherited_from(parent),
            (None, None, _) | (Some(_), None, None) => Ok(()),
            _ => Err(DomainError::InvalidState {
                message: "子任务缺少父目录授权或试图自行扩大项目范围".into(),
            }),
        }
    }

    /// 显式转交只改变直接子树根的拥有方，不迁移 Task/lease 或复制后代状态。
    /// API 还须在会话提交锁下校验来源 Turn、Goal 控制版本及资源可独立运行。
    pub fn transfer_agent_subtree(
        &self,
        parent_id: &TaskId,
        child_id: &TaskId,
        transfer: magi_core::AgentRunTransfer,
    ) -> DomainResult<bool> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let parent = tasks.get(parent_id).ok_or(DomainError::NotFound {
            entity: "ParentTask",
        })?;
        let child = tasks.get(child_id).ok_or(DomainError::NotFound {
            entity: "AgentTask",
        })?;
        Self::validate_agent_transfer_binding(parent, child, &transfer)?;
        if let Some(existing) = child.agent_run_transfer() {
            if existing == &transfer {
                return Ok(false);
            }
            return Err(DomainError::InvalidState {
                message: "子树已经转交，不能改写原拥有方或来源".into(),
            });
        }
        if parent.status != TaskStatus::Running
            || !matches!(child.status, TaskStatus::Pending | TaskStatus::Running)
            || transfer.transferred_at < child.created_at
        {
            return Err(DomainError::InvalidState {
                message: "转交需要父任务仍在运行、子树根尚未结束".into(),
            });
        }
        if tasks.values().any(|candidate| {
            candidate
                .agent_run_transfer()
                .is_some_and(|existing| existing.operation_id == transfer.operation_id)
        }) {
            return Err(DomainError::InvalidState {
                message: "转交 operation_id 已用于其他子树".into(),
            });
        }
        Self::validate_agent_transfer_subtree(tasks.values(), child, &transfer)?;
        let root_id = parent.root_task_id.clone();
        let child = tasks.get_mut(child_id).expect("child checked");
        child
            .executor_binding
            .as_mut()
            .expect("activation binding checked")
            .agent_run_transfer = Some(transfer);
        child.updated_at = UtcMillis::now();
        let leases = self
            .leases
            .read()
            .expect("leases read lock poisoned")
            .clone();
        let index = self
            .mission_index
            .read()
            .expect("mission index read lock poisoned")
            .clone();
        let snapshot = Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![root_id]);
        self.fire_checkpoint(&snapshot)?;
        self.commit_maps(tasks, leases, index);
        self.notify_status_change();
        Ok(true)
    }

    pub(super) fn validate_agent_transfer_binding(
        parent: &Task,
        child: &Task,
        transfer: &magi_core::AgentRunTransfer,
    ) -> DomainResult<()> {
        let activation = child
            .agent_activation()
            .ok_or_else(|| DomainError::InvalidState {
                message: "转交子树需要持久 Agent 身份".into(),
            })?;
        transfer.validate(activation)?;
        if parent.parent_task_id.is_some()
            || parent.root_task_id != parent.task_id
            || parent.executor_binding_target_role() != Some("coordinator")
            || child.parent_task_id.as_ref() != Some(&parent.task_id)
            || child.root_task_id != parent.root_task_id
            || child.mission_id != parent.mission_id
            || transfer.transferred_by != parent.task_id
            || child.kind != magi_core::TaskKind::LocalAgent
        {
            return Err(DomainError::InvalidState {
                message: "只有主线能转交自己的直接子代理，不能改变任务树归属".into(),
            });
        }
        Ok(())
    }

    /// 创建与重载读取相同的转交合同。共享计划和跨拥有方的调度依赖不能随子树留在后台。
    pub(super) fn validate_agent_transfer_subtree<'a>(
        tasks: impl Iterator<Item = &'a Task>,
        child: &Task,
        transfer: &magi_core::AgentRunTransfer,
    ) -> DomainResult<()> {
        let tasks = tasks.collect::<Vec<_>>();
        let mut children = HashMap::<&TaskId, Vec<&Task>>::new();
        for task in &tasks {
            if let Some(parent) = &task.parent_task_id {
                children.entry(parent).or_default().push(task);
            }
        }
        let mut subtree = vec![child];
        let mut ids = HashSet::new();
        let mut cursor = 0;
        while cursor < subtree.len() {
            let task = subtree[cursor];
            if !ids.insert(task.task_id.clone())
                || task.root_task_id != child.root_task_id
                || task.mission_id != child.mission_id
                || task.kind != magi_core::TaskKind::LocalAgent
                || task.workspace_scope != child.workspace_scope
                || task.write_scope != child.write_scope
                || task
                    .agent_activation()
                    .is_none_or(|activation| activation.session_id != transfer.session_id)
                || task
                    .executor_binding
                    .as_ref()
                    .is_some_and(|binding| binding.plan_item_id.is_some())
                || (task.task_id != child.task_id && task.agent_run_transfer().is_some())
            {
                return Err(DomainError::InvalidState {
                    message: "子树存在循环、异域任务、共享计划或重复转交，不能独立运行".into(),
                });
            }
            if let Some(descendants) = children.get(&task.task_id) {
                subtree.extend(descendants.iter().copied());
            }
            cursor += 1;
        }
        if transfer.transferred_at < child.created_at
            || tasks.iter().any(|task| {
                task.dependency_ids
                    .iter()
                    .chain(&task.required_children)
                    .any(|dependency| ids.contains(&task.task_id) != ids.contains(dependency))
            })
        {
            return Err(DomainError::InvalidState {
                message: "子树转交时间无效或存在跨拥有方的调度/完成依赖，必须先结算依赖".into(),
            });
        }
        Ok(())
    }

    /// 只读返回现有父链上的转交根 Task，包含原 activation 与唯一转交记录。
    /// 无转交与缺失/损坏的查询来源分开返回，调用方不得将查询失败当作原父轮授权。
    pub fn agent_run_transfer_for_task(&self, task_id: &TaskId) -> DomainResult<Option<Task>> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        if !tasks.contains_key(task_id) {
            return Err(DomainError::NotFound {
                entity: "AgentTask",
            });
        }
        Self::agent_run_transfer_in_tasks(&tasks, task_id)
            .map(|boundary| boundary.map(|(root, _)| root.clone()))
    }

    pub(super) fn agent_run_transfer_in_tasks<'a>(
        tasks: &'a HashMap<TaskId, Task>,
        task_id: &TaskId,
    ) -> DomainResult<Option<(&'a Task, &'a magi_core::AgentRunTransfer)>> {
        // 未创建的依赖项可以没有拥有方；公开执行查询另外要求来源 Task 存在。
        let Some(original) = tasks.get(task_id) else {
            return Ok(None);
        };
        let mut current = original;
        let mut seen = HashSet::new();
        loop {
            if current.root_task_id != original.root_task_id
                || current.mission_id != original.mission_id
                || !seen.insert(current.task_id.clone())
            {
                return Err(DomainError::InvalidState {
                    message: "Agent 转交查询的父链循环或作用域不匹配".into(),
                });
            }
            if let Some(transfer) = current.agent_run_transfer() {
                return Ok(Some((current, transfer)));
            }
            let Some(parent) = &current.parent_task_id else {
                return Ok(None);
            };
            current = tasks.get(parent).ok_or_else(|| DomainError::InvalidState {
                message: "Agent 转交查询的来源父任务缺失".into(),
            })?;
        }
    }

    /// 显式继续同一转交范围；只重开调用方已核对的中断任务，不改写原父链或完成结果。
    pub fn reopen_transferred_agent_scope(
        &self,
        root: &TaskId,
        expected: &[Task],
    ) -> DomainResult<()> {
        let guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks lock poisoned").clone();
        let leases = self.leases.read().expect("leases lock poisoned").clone();
        if expected.first().is_none_or(|task| &task.task_id != root)
            || tasks
                .get(root)
                .is_none_or(|task| task.agent_run_transfer().is_none())
        {
            return Err(DomainError::InvalidState {
                message: "独立继续需要原转交子树根".into(),
            });
        }
        let ids = collect_subtree_ids_in_scope(&tasks, root, true);
        if ids.iter().any(|id| {
            tasks.get(id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) || leases
                .values()
                .any(|lease| &lease.task_id == id && lease.lease_status == TaskLeaseState::Active)
        }) {
            return Err(DomainError::InvalidState {
                message: "代理子树仍有活动任务或租约，不能继续".into(),
            });
        }
        let mut changed = Vec::new();
        for source in expected {
            let task = tasks.get(&source.task_id).ok_or(DomainError::NotFound {
                entity: "AgentTask",
            })?;
            if !ids.contains(&source.task_id)
                || task.status != TaskStatus::Failed
                || task.updated_at != source.updated_at
                || task.retry_count != source.retry_count
                || task.agent_activation() != source.agent_activation()
            {
                return Err(DomainError::InvalidState {
                    message: "代理恢复来源已变化".into(),
                });
            }
            let task = tasks.get_mut(&source.task_id).expect("validated task");
            task.retry_count =
                task.retry_count
                    .checked_add(1)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "恢复代际溢出".into(),
                    })?;
            task.status = TaskStatus::Pending;
            task.updated_at = UtcMillis(
                UtcMillis::now()
                    .0
                    .max(source.updated_at.0.saturating_add(1)),
            );
            changed.push(task.clone());
        }
        let index = self
            .mission_index
            .read()
            .expect("mission index lock poisoned")
            .clone();
        self.fire_checkpoint(&Self::snapshot_from_maps_with_dirty_roots(
            &tasks,
            &leases,
            vec![expected[0].root_task_id.clone()],
        ))?;
        self.commit_maps(tasks, leases, index);
        drop(guard);
        for task in changed {
            self.emit_status_change(
                &task.task_id.clone(),
                TaskStatus::Failed,
                TaskStatus::Pending,
                task,
            );
        }
        Ok(())
    }

    pub fn append_agent_context_access(
        &self,
        task_id: &TaskId,
        record: AgentContextAccessRecord,
    ) -> DomainResult<()> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get_mut(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let dirty_root_id = task.root_task_id.clone();
        let TaskRuntimePayload::AgentContext { accesses, .. } = &mut task.runtime_payload else {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 没有 agent context package"),
            });
        };
        accesses.push(record);
        task.updated_at = UtcMillis::now();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(())
    }

    /// 原子提交补充、访问回执与 revision，并在同一锁内复查接纳状态。
    /// Pending -> Running 与关闭边界不能把“已接纳”补充遗落在旧启动快照里。
    pub fn deliver_agent_context_supplement(
        &self,
        task_id: &TaskId,
        expected_status: TaskStatus,
        supplement: AgentContextSupplement,
        access: AgentContextAccessRecord,
    ) -> DomainResult<AgentContextPackage> {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        let mut tasks = self.tasks.read().expect("tasks read lock poisoned").clone();
        let task = tasks
            .get(task_id)
            .ok_or(DomainError::NotFound { entity: "Task" })?;
        let parent = tasks
            .get(&supplement.author_task_id)
            .ok_or(DomainError::NotFound {
                entity: "parent Task",
            })?;
        if !matches!(expected_status, TaskStatus::Pending | TaskStatus::Running)
            || task.status != expected_status
            || !matches!(parent.status, TaskStatus::Pending | TaskStatus::Running)
            || task.parent_task_id.as_ref() != Some(&parent.task_id)
            || task.root_task_id != parent.root_task_id
            || task.mission_id != parent.mission_id
            || task.workspace_scope != parent.workspace_scope
            || task.write_scope != parent.write_scope
        {
            return Err(DomainError::InvalidState {
                message: "代理接纳状态或父任务作用域已变化，请刷新后重新投递".into(),
            });
        }
        let task = tasks.get_mut(task_id).expect("validated task exists");
        let dirty_root_id = task.root_task_id.clone();
        let TaskRuntimePayload::AgentContext { package, accesses } = &mut task.runtime_payload
        else {
            return Err(DomainError::InvalidState {
                message: format!("任务 {task_id} 没有 agent context package"),
            });
        };
        if package.parent_task_id != supplement.author_task_id
            || access.operation != magi_core::AgentContextAccessOperation::Send
            || access.query.as_deref() != Some(supplement.message.as_str())
            || package
                .supplements
                .iter()
                .any(|entry| entry.supplement_id == supplement.supplement_id)
        {
            return Err(DomainError::InvalidState {
                message: "代理补充身份或发送回执冲突".into(),
            });
        }
        package.revision =
            package
                .revision
                .checked_add(1)
                .ok_or_else(|| DomainError::InvalidState {
                    message: "代理上下文 revision 溢出".into(),
                })?;
        package.updated_at = supplement.created_at;
        package.supplements.push(supplement);
        accesses.push(access);
        task.updated_at = package.updated_at;
        let package = package.as_ref().clone();
        let leases = self.leases.read().expect("leases read lock poisoned");
        let snapshot =
            Self::snapshot_from_maps_with_dirty_roots(&tasks, &leases, vec![dirty_root_id]);
        self.fire_checkpoint(&snapshot)?;
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        Ok(package)
    }

    /// 构建代理运行视图。
    pub fn build_agent_run_projection(&self, root_task_id: &TaskId) -> Option<AgentRunProjection> {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let children = child_index_from_tasks(&tasks);

        let root_task = tasks.get(root_task_id)?.clone();

        let mut ordered_task_ids: Vec<TaskId> = Vec::new();
        let mut stack: Vec<TaskId> = vec![root_task_id.clone()];
        let mut visited: HashSet<TaskId> = HashSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if tasks.contains_key(&current) {
                ordered_task_ids.push(current.clone());
                if let Some(child_ids) = children.get(&current) {
                    for child_id in child_ids.iter().rev() {
                        stack.push(child_id.clone());
                    }
                }
            }
        }
        let all_tasks: Vec<&Task> = ordered_task_ids
            .iter()
            .filter_map(|task_id| tasks.get(task_id))
            .collect();
        let projection_tasks: Vec<Task> = all_tasks.iter().map(|task| (*task).clone()).collect();
        let active_tasks: Vec<&Task> = all_tasks
            .iter()
            .copied()
            .filter(|task| task.status != TaskStatus::Killed)
            .collect();

        let running_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .map(|t| t.task_id.clone())
            .collect();

        let pending_tasks: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .map(|t| t.task_id.clone())
            .collect();

        let completed_task_ids: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.task_id.clone())
            .collect();

        let failed_task_ids: Vec<TaskId> = active_tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .map(|t| t.task_id.clone())
            .collect();

        let killed_task_ids: Vec<TaskId> = all_tasks
            .iter()
            .copied()
            .filter(|t| t.status == TaskStatus::Killed)
            .map(|t| t.task_id.clone())
            .collect();

        let total_tasks = active_tasks.len() as u32;
        let pending_count = pending_tasks.len() as u32;
        let completed_count = completed_task_ids.len() as u32;
        let settled_tasks = active_tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
                )
            })
            .count() as u32;
        let failed_count = failed_task_ids.len() as u32;
        let killed_count = killed_task_ids.len() as u32;
        let running_count = running_tasks.len() as u32;

        let aggregate_status = root_task.status;

        let execution_mode = "execution_chain".to_string();
        let runner_status = match aggregate_status {
            TaskStatus::Running => "running".to_string(),
            TaskStatus::Completed => "completed".to_string(),
            TaskStatus::Failed => "error".to_string(),
            TaskStatus::Killed => "killed".to_string(),
            TaskStatus::Pending => "pending".to_string(),
        };
        let display_status = match root_task.status {
            TaskStatus::Killed => "已终止".to_string(),
            TaskStatus::Failed => "主线任务执行失败，等待用户处理".to_string(),
            TaskStatus::Completed if failed_count > 0 => {
                format!("主线已完成，{} 项子任务已降级处理", failed_count)
            }
            TaskStatus::Completed => "全部完成".to_string(),
            TaskStatus::Pending => "待启动".to_string(),
            TaskStatus::Running if failed_count > 0 => {
                format!("主线仍在执行，正在接管 {} 项失败子任务", failed_count)
            }
            TaskStatus::Running => {
                let pct = (settled_tasks as f32 / total_tasks.max(1) as f32 * 100.0).round() as u32;
                if running_count > 0 {
                    format!("{}% 已完成，{} 项执行中", pct, running_count)
                } else if pending_count > 0 {
                    format!("{}% 已完成，{} 项待执行", pct, pending_count)
                } else {
                    format!("{}% 已完成，主线汇总中", pct)
                }
            }
        };

        Some(AgentRunProjection {
            root_task,
            tasks: projection_tasks,
            running_tasks,
            pending_tasks,
            completed_tasks: completed_task_ids,
            failed_tasks: failed_task_ids,
            killed_tasks: killed_task_ids,
            progress_summary: ProgressSummary {
                total_tasks,
                pending_tasks: pending_count,
                running_tasks: running_count,
                completed_tasks: completed_count,
                settled_tasks,
                failed_tasks: failed_count,
                killed_tasks: killed_count,
            },
            aggregate_status,
            display_status,
            execution_mode,
            runner_status,
            has_recoverable_chain: false,
            recoverable_branch_count: 0,
        })
    }

    /// Activation 可跨 root；只在完整恢复集合上校验来源，不能用单 root 增量快照裁决。
    pub(super) fn validate_agent_history(checkpoint: &TaskStoreCheckpoint) -> io::Result<()> {
        let mut generations = HashSet::new();
        let mut transfer_operations = HashSet::new();
        let all = checkpoint
            .tasks
            .iter()
            .map(|task| (&task.task_id, task))
            .collect::<HashMap<_, _>>();
        for task in &checkpoint.tasks {
            Self::validate_project_directory_inheritance(
                task,
                task.parent_task_id
                    .as_ref()
                    .and_then(|id| all.get(id).copied()),
            )
            .map_err(|error| Self::invalid_projection(error.to_string()))?;
            if task.delegation_allowed() && task.agent_activation().is_none() {
                return Err(Self::invalid_projection(
                    "受委派代理缺少持久 activation 身份",
                ));
            }
            let Some(binding) = task.agent_activation() else {
                if task.agent_run_transfer().is_some() {
                    return Err(Self::invalid_projection("转交子树缺少 Agent activation"));
                }
                continue;
            };
            if let Some(transfer) = task.agent_run_transfer() {
                let parent = task
                    .parent_task_id
                    .as_ref()
                    .and_then(|id| all.get(id))
                    .ok_or_else(|| Self::invalid_projection("转交子树缺少原父任务"))?;
                Self::validate_agent_transfer_binding(parent, task, transfer)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
                Self::validate_agent_transfer_subtree(checkpoint.tasks.iter(), task, transfer)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
                if !transfer_operations.insert(transfer.operation_id.clone()) {
                    return Err(Self::invalid_projection("子树转交 operation_id 重复"));
                }
            }
            if task
                .parent_task_id
                .as_ref()
                .and_then(|id| all.get(id))
                .is_none_or(|parent| {
                    parent.root_task_id != task.root_task_id || parent.mission_id != task.mission_id
                })
            {
                return Err(Self::invalid_projection(
                    "Agent activation 父任务归属不匹配",
                ));
            }
            if let Some(parent) = task.parent_task_id.as_ref().and_then(|id| all.get(id)) {
                if task.delegation_allowed()
                    && (parent.parent_task_id.is_some()
                        || parent.executor_binding_target_role() != Some("coordinator")
                        || parent.root_task_id != parent.task_id
                        || task.policy_snapshot != parent.policy_snapshot
                        || task.workspace_scope != parent.workspace_scope
                        || task.write_scope != parent.write_scope)
                {
                    return Err(Self::invalid_projection("代理委派来源无效"));
                }
                if parent.parent_task_id.is_some() {
                    let root = all
                        .get(&task.root_task_id)
                        .ok_or_else(|| Self::invalid_projection("嵌套代理 root 缺失"))?;
                    if !parent.delegation_allowed()
                        || task.delegation_allowed()
                        || parent.parent_task_id.as_ref() != Some(&root.task_id)
                        || root.parent_task_id.is_some()
                        || root.root_task_id != root.task_id
                        || root.executor_binding_target_role() != Some("coordinator")
                        || task.policy_snapshot != parent.policy_snapshot
                        || task.workspace_scope != parent.workspace_scope
                        || task.write_scope != parent.write_scope
                    {
                        return Err(Self::invalid_projection("嵌套代理授权或深度无效"));
                    }
                }
            }
            if !generations.insert((binding.agent_id.clone(), binding.generation)) {
                return Err(Self::invalid_projection("Agent activation generation 重复"));
            }
            if let Some(previous_id) = &binding.previous_task_id {
                let previous = all
                    .get(previous_id)
                    .ok_or_else(|| Self::invalid_projection("Agent activation 来源缺失"))?;
                let source = previous
                    .agent_activation()
                    .ok_or_else(|| Self::invalid_projection("Agent activation 来源无身份"))?;
                if source.agent_id != binding.agent_id
                    || source.session_id != binding.session_id
                    || source.lifetime != binding.lifetime
                    || source.generation.checked_add(1) != Some(binding.generation)
                    || (task.delegation_allowed() && !previous.delegation_allowed())
                    || previous.mission_id != task.mission_id
                    || !Self::same_agent_owner(
                        previous
                            .parent_task_id
                            .as_ref()
                            .and_then(|id| all.get(id))
                            .copied(),
                        all.get(task.parent_task_id.as_ref().expect("validated parent"))
                            .expect("validated parent"),
                    )
                    || previous.status != TaskStatus::Completed
                    || matches!(source.lifetime, magi_core::AgentLifetime::Turn)
                    || previous.workspace_scope != task.workspace_scope
                    || previous.write_scope != task.write_scope
                    || previous.executor_binding_target_role()
                        != task.executor_binding_target_role()
                    || task
                        .executor_binding_capability_ids()
                        .iter()
                        .any(|capability| {
                            !previous
                                .executor_binding_capability_ids()
                                .contains(capability)
                        })
                {
                    return Err(Self::invalid_projection("Agent activation 来源不匹配"));
                }
            }
        }
        Ok(())
    }
}

use super::*;

impl TaskStore {
    pub(super) fn snapshot_from_maps(
        tasks: &HashMap<TaskId, Task>,
        leases: &HashMap<LeaseId, TaskLease>,
    ) -> TaskStoreSnapshot {
        let mut tasks = tasks.values().cloned().collect::<Vec<_>>();
        let mut leases = leases.values().cloned().collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
        leases.sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        TaskStoreSnapshot {
            tasks,
            leases,
            changed_root_ids: Vec::new(),
        }
    }

    pub(super) fn snapshot_from_maps_with_dirty_roots(
        tasks: &HashMap<TaskId, Task>,
        leases: &HashMap<LeaseId, TaskLease>,
        dirty_roots: impl IntoIterator<Item = TaskId>,
    ) -> TaskStoreSnapshot {
        let mut snapshot = Self::snapshot_from_maps(tasks, leases);
        snapshot.changed_root_ids = dirty_roots.into_iter().collect();
        snapshot
            .changed_root_ids
            .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        snapshot.changed_root_ids.dedup();
        snapshot
    }

    pub(super) fn snapshot_locked(&self) -> TaskStoreSnapshot {
        let tasks = self.tasks.read().expect("tasks read lock poisoned");
        let leases = self.leases.read().expect("leases read lock poisoned");
        // 空 changed_root_ids 明确表示完整快照，供周期性 checkpoint 和恢复写入使用。
        Self::snapshot_from_maps(&tasks, &leases)
    }

    pub(super) fn commit_maps(
        &self,
        tasks: HashMap<TaskId, Task>,
        leases: HashMap<LeaseId, TaskLease>,
        mission_index: HashMap<MissionId, Vec<TaskId>>,
    ) {
        *self.tasks.write().expect("tasks write lock poisoned") = tasks;
        *self.leases.write().expect("leases write lock poisoned") = leases;
        *self
            .mission_index
            .write()
            .expect("mission_index write lock poisoned") = mission_index;
    }

    pub(super) fn snapshot_locked_with_mutation_guard(&self) -> TaskStoreSnapshot {
        let _mutation_guard = self
            .mutation_lock
            .lock()
            .expect("task mutation lock poisoned");
        self.snapshot_locked()
    }

    pub fn snapshot(&self) -> TaskStoreSnapshot {
        self.snapshot_locked_with_mutation_guard()
    }

    // ------------------------------------------------------------------
    // G8.5: Dynamic dependency management
    // ------------------------------------------------------------------

    /// 将同一内存快照写入一个不可变 generation，最后以 manifest 原子切换作为唯一提交点。
    pub fn checkpoint_to_projection_directory(&self, dir: &Path) -> io::Result<usize> {
        let snapshot = {
            let _mutation_guard = self
                .mutation_lock
                .lock()
                .map_err(|_| io::Error::other("task mutation lock poisoned"))?;
            self.snapshot_locked()
        };
        Self::checkpoint_snapshot_to_projection_directory(&snapshot, dir)
    }

    /// 将候选任务事实快照写入 projection。供 checkpoint 回调使用，避免回调重新读取
    /// 可能已经变化的 TaskStore。
    pub fn checkpoint_snapshot_to_projection_directory(
        snapshot: &TaskStoreSnapshot,
        dir: &Path,
    ) -> io::Result<usize> {
        let checkpoint_started_at = Instant::now();
        let changed_root_count = snapshot.changed_root_ids.len();
        Self::validate_snapshot(snapshot)?;
        let _writer = TASK_PROJECTION_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("task projection writer lock poisoned"))?;
        fs::create_dir_all(dir)?;
        let generations_dir = dir.join(TASK_PROJECTION_GENERATIONS_DIR);
        fs::create_dir_all(&generations_dir)?;

        let previous_manifest = Self::read_projection_manifest_if_present(dir)?;
        let mut generation = previous_manifest
            .as_ref()
            .map(|manifest| manifest.generation)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("task projection generation 已耗尽"))?;
        while generations_dir
            .join(Self::generation_directory_name(generation))
            .exists()
        {
            generation = generation
                .checked_add(1)
                .ok_or_else(|| io::Error::other("task projection generation 已耗尽"))?;
        }

        let mut roots = HashMap::<TaskId, (Vec<Task>, Vec<TaskLease>)>::new();
        for task in &snapshot.tasks {
            roots
                .entry(task.root_task_id.clone())
                .or_default()
                .0
                .push(task.clone());
        }
        for lease in &snapshot.leases {
            roots
                .entry(lease.root_task_id.clone())
                .or_default()
                .1
                .push(lease.clone());
        }
        for (tasks, leases) in roots.values_mut() {
            tasks.sort_by(|left, right| {
                left.created_at
                    .0
                    .cmp(&right.created_at.0)
                    .then_with(|| left.task_id.as_str().cmp(right.task_id.as_str()))
            });
            leases.sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        }
        #[cfg(test)]
        if let Some(hook) = TASK_PROJECTION_AFTER_SNAPSHOT_HOOK
            .lock()
            .expect("task projection test hook lock poisoned")
            .clone()
        {
            hook(dir);
        }

        let mut projections = roots
            .into_iter()
            .map(|(root_task_id, (tasks, leases))| TaskRootProjection {
                schema_version: TASK_PROJECTION_SCHEMA_VERSION,
                generation,
                root_task_id,
                tasks,
                leases,
            })
            .collect::<Vec<_>>();
        projections
            .sort_by(|left, right| left.root_task_id.as_str().cmp(right.root_task_id.as_str()));
        let dirty_roots = (!snapshot.changed_root_ids.is_empty()).then(|| {
            snapshot
                .changed_root_ids
                .iter()
                .cloned()
                .collect::<HashSet<_>>()
        });
        if let Some(dirty_roots) = dirty_roots.as_ref() {
            let dirty_projections = projections
                .iter()
                .filter(|projection| dirty_roots.contains(&projection.root_task_id))
                .cloned()
                .collect::<Vec<_>>();
            Self::validate_projection_set(&dirty_projections)?;
        } else {
            Self::validate_projection_set(&projections)?;
        }

        let sequence = TASK_PROJECTION_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_generation_dir = generations_dir.join(format!(
            ".generation-{generation:020}-{}-{sequence}.tmp",
            std::process::id()
        ));
        fs::create_dir(&temporary_generation_dir)?;
        // 每次任务状态变更都会产生一个新 generation，但绝大多数 root projection
        // 并没有变化。复用上一 generation 的不变文件，只为变化的 root 重新序列化并
        // 写入新文件；最后仍通过 manifest 原子切换，保留崩溃恢复和完整快照语义。
        let previous_generation_dir = previous_manifest.as_ref().and_then(|manifest| {
            let path = generations_dir.join(Self::generation_directory_name(manifest.generation));
            path.is_dir().then_some(path)
        });
        let write_result = (|| -> io::Result<Vec<TaskProjectionManifestRoot>> {
            let mut manifest_roots = Vec::with_capacity(projections.len());
            for projection in &projections {
                let file_name = Self::projection_file_name(projection.root_task_id.as_str());
                let destination = temporary_generation_dir.join(&file_name);
                let source = previous_generation_dir
                    .as_ref()
                    .map(|directory| directory.join(&file_name));
                let previous_root = previous_manifest.as_ref().and_then(|manifest| {
                    manifest
                        .roots
                        .iter()
                        .find(|root| root.file_name == file_name)
                });
                let is_dirty = dirty_roots
                    .as_ref()
                    .is_none_or(|roots| roots.contains(&projection.root_task_id));
                if !is_dirty
                    && let Some(previous_root) = previous_root
                    && let Some(source) = source.as_deref()
                    && fs::hard_link(source, &destination).is_ok()
                {
                    manifest_roots.push(TaskProjectionManifestRoot {
                        root_task_id: projection.root_task_id.clone(),
                        file_name,
                        generation: previous_root.generation,
                        content_hash: previous_root.content_hash.clone(),
                    });
                    continue;
                }

                let content = serde_json::to_vec_pretty(projection).map_err(io::Error::other)?;
                let content_hash = Self::projection_content_hash(projection)?;
                let source_identity = Self::reuse_projection_file_if_unchanged(
                    source.as_deref(),
                    &destination,
                    previous_root,
                    &content_hash,
                )?;
                if source_identity.is_none() {
                    Self::write_new_file_synced(&destination, &content)?;
                }
                let (root_generation, root_hash) =
                    source_identity.unwrap_or((generation, content_hash));
                manifest_roots.push(TaskProjectionManifestRoot {
                    root_task_id: projection.root_task_id.clone(),
                    file_name,
                    generation: root_generation,
                    content_hash: root_hash,
                });
            }
            Self::sync_directory(&temporary_generation_dir)?;
            Ok(manifest_roots)
        })();
        let manifest_roots = match write_result {
            Ok(roots) => roots,
            Err(error) => {
                Self::remove_stale_generation(&temporary_generation_dir, &generations_dir)?;
                return Err(error);
            }
        };

        let generation_dir = generations_dir.join(Self::generation_directory_name(generation));
        fs::rename(&temporary_generation_dir, &generation_dir)?;
        Self::sync_directory(&generations_dir)?;

        let manifest = TaskProjectionManifest {
            schema_version: TASK_PROJECTION_SCHEMA_VERSION,
            generation,
            roots: manifest_roots,
        };
        let manifest_content = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        magi_core::fs_atomic::write_atomic(
            &dir.join(TASK_PROJECTION_MANIFEST_FILE),
            manifest_content,
        )?;
        Self::sync_directory(dir)?;

        Self::cleanup_stale_generations(&generations_dir, &generation_dir)?;
        if changed_root_count > 0 {
            tracing::info!(
                target: "magi.performance",
                changed_root_count,
                task_count = snapshot.tasks.len(),
                lease_count = snapshot.leases.len(),
                projection_count = projections.len(),
                elapsed_ms = checkpoint_started_at.elapsed().as_millis() as u64,
                stage = "task_projection_checkpoint_completed",
                "conversation response timing"
            );
        }
        Ok(projections.len())
    }

    /// 复用上一 generation 中内容完全相同的 root projection。
    ///
    /// generation 目录只读且最终会整体重命名，因此可以安全建立硬链接；硬链接不
    /// 可用时退回到普通写入，由调用方继续执行原子目录提交。返回 root 文件的来源
    /// generation 和业务事实指纹，供 manifest 精确记录。
    pub(super) fn reuse_projection_file_if_unchanged(
        source: Option<&Path>,
        destination: &Path,
        previous_root: Option<&TaskProjectionManifestRoot>,
        content_hash: &str,
    ) -> io::Result<Option<(u64, String)>> {
        let (Some(source), Some(previous_root)) = (source, previous_root) else {
            return Ok(None);
        };
        if previous_root.content_hash != content_hash || fs::hard_link(source, destination).is_err()
        {
            return Ok(None);
        }
        Ok(Some((previous_root.generation, content_hash.to_string())))
    }

    pub(super) fn projection_content_hash(projection: &TaskRootProjection) -> io::Result<String> {
        let mut normalized = projection.clone();
        normalized.generation = 0;
        let content = serde_json::to_vec(&normalized).map_err(io::Error::other)?;
        let hash = content.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        Ok(format!("{hash:016x}"))
    }

    /// Restore a TaskStore from a root-task projection directory.
    ///
    /// Returns `Ok(None)` when the directory has not been created.
    pub fn restore_from_projection_directory(dir: &Path) -> io::Result<Option<Self>> {
        if !dir.exists() {
            return Ok(None);
        }
        let Some(manifest) = Self::read_projection_manifest_if_present(dir)? else {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(None);
        };
        let generation_dir = dir
            .join(TASK_PROJECTION_GENERATIONS_DIR)
            .join(Self::generation_directory_name(manifest.generation));
        if !generation_dir.is_dir() {
            return Err(Self::invalid_projection(format!(
                "manifest 指向的 generation 不存在: {}",
                manifest.generation
            )));
        }

        let mut expected_files = HashSet::new();
        let mut expected_roots = HashSet::new();
        let mut projections = Vec::with_capacity(manifest.roots.len());
        for root in &manifest.roots {
            if root.root_task_id.as_str().is_empty()
                || !expected_roots.insert(root.root_task_id.clone())
            {
                return Err(Self::invalid_projection("manifest 包含空或重复 rootTaskId"));
            }
            let expected_file_name = Self::projection_file_name(root.root_task_id.as_str());
            if root.file_name != expected_file_name
                || !expected_files.insert(root.file_name.clone())
            {
                return Err(Self::invalid_projection(format!(
                    "manifest 文件名与 rootTaskId 不一致: {}",
                    root.root_task_id
                )));
            }
            let content = fs::read(generation_dir.join(&root.file_name))?;
            let projection = Self::deserialize_projection_strict(&content)?;
            let content_hash_matches =
                Self::projection_content_hash(&projection)? == root.content_hash;
            if projection.schema_version != TASK_PROJECTION_SCHEMA_VERSION
                || root.generation == 0
                || projection.generation != root.generation
                || projection.root_task_id != root.root_task_id
                || !content_hash_matches
            {
                return Err(Self::invalid_projection(format!(
                    "task projection 元数据与 manifest 不一致: {}",
                    root.root_task_id
                )));
            }
            projections.push(projection);
        }
        let actual_files = fs::read_dir(&generation_dir)?
            .map(|entry| {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    return Err(Self::invalid_projection("generation 中包含非文件条目"));
                }
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Self::invalid_projection("generation 文件名不是 UTF-8"))
            })
            .collect::<io::Result<HashSet<_>>>()?;
        if actual_files != expected_files {
            return Err(Self::invalid_projection(
                "generation 文件集合与 manifest 不一致",
            ));
        }

        let checkpoint = Self::validate_projection_set(&projections)?;
        Ok(Some(Self::restore_checked(checkpoint)?))
    }

    /// 返回 manifest 已提交的 generation。accepted WAL 用它判断某条接纳记录是否已经
    /// 被后续完整 task checkpoint 覆盖。
    pub fn committed_projection_generation(dir: &Path) -> io::Result<Option<u64>> {
        if !dir.exists() {
            return Ok(None);
        }
        if !dir.join(TASK_PROJECTION_MANIFEST_FILE).exists() {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(None);
        }
        Self::read_projection_manifest(dir).map(|manifest| Some(manifest.generation))
    }

    /// 判断已提交 manifest 是否包含指定根任务。该判断只依据 manifest 提交事实，
    /// 不读取当前内存 TaskStore，也不以 task 是否存在推断 accepted WAL 状态。
    pub fn committed_projection_contains_root(
        dir: &Path,
        root_task_id: &TaskId,
    ) -> io::Result<bool> {
        if !dir.exists() {
            return Ok(false);
        }
        let Some(manifest) = Self::read_projection_manifest_if_present(dir)? else {
            Self::discard_uncommitted_projection_directory(dir)?;
            return Ok(false);
        };
        Ok(manifest
            .roots
            .iter()
            .any(|root| root.root_task_id == *root_task_id))
    }

    pub(super) fn restore_checked(checkpoint: TaskStoreCheckpoint) -> io::Result<Self> {
        Self::validate_checkpoint(&checkpoint)?;
        Self::validate_agent_history(&checkpoint)?;
        let store = Self::new();
        {
            let mut tasks = store.tasks.write().expect("tasks write lock poisoned");
            let mut mission_index = store
                .mission_index
                .write()
                .expect("mission_index write lock poisoned");
            for task in checkpoint.tasks {
                mission_index
                    .entry(task.mission_id.clone())
                    .or_default()
                    .push(task.task_id.clone());
                tasks.insert(task.task_id.clone(), task);
            }
        }
        let mut lease_map = store.leases.write().expect("leases write lock poisoned");
        for lease in checkpoint.leases {
            lease_map.insert(lease.lease_id.clone(), lease);
        }
        let max_counter = lease_map
            .keys()
            .filter_map(|id| id.as_str().rsplit('-').next())
            .filter_map(|part| part.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        drop(lease_map);
        let current = LEASE_COUNTER.load(Ordering::Relaxed);
        if max_counter >= current {
            LEASE_COUNTER.store(max_counter + 1, Ordering::Relaxed);
        }
        Ok(store)
    }

    pub(super) fn deserialize_projection_strict(content: &[u8]) -> io::Result<TaskRootProjection> {
        serde_json::from_slice(content).map_err(|error| {
            Self::invalid_projection(format!("解析 task projection 失败: {error}"))
        })
    }

    pub(super) fn validate_projection_set(
        projections: &[TaskRootProjection],
    ) -> io::Result<TaskStoreCheckpoint> {
        let mut checkpoint = TaskStoreCheckpoint::default();
        let mut roots = HashSet::new();
        for projection in projections {
            if projection.schema_version != TASK_PROJECTION_SCHEMA_VERSION
                || projection.generation == 0
            {
                return Err(Self::invalid_projection(
                    "task projection schema 或 generation 不合法",
                ));
            }
            if projection.root_task_id.as_str().is_empty()
                || !roots.insert(projection.root_task_id.clone())
            {
                return Err(Self::invalid_projection(
                    "task projection 的 rootTaskId 为空或重复",
                ));
            }
            if projection.tasks.is_empty() {
                return Err(Self::invalid_projection(format!(
                    "root projection 不得为空: {}",
                    projection.root_task_id
                )));
            }
            if !projection.tasks.iter().any(|task| {
                task.task_id == projection.root_task_id
                    && task.root_task_id == projection.root_task_id
            }) {
                return Err(Self::invalid_projection(format!(
                    "root projection 缺少 root task: {}",
                    projection.root_task_id
                )));
            }
            if projection
                .tasks
                .iter()
                .any(|task| task.root_task_id != projection.root_task_id)
                || projection
                    .leases
                    .iter()
                    .any(|lease| lease.root_task_id != projection.root_task_id)
            {
                return Err(Self::invalid_projection(format!(
                    "task 或 lease 归属错误: {}",
                    projection.root_task_id
                )));
            }
            checkpoint.tasks.extend(projection.tasks.iter().cloned());
            checkpoint.leases.extend(projection.leases.iter().cloned());
        }
        // Projection 文件按 root 分组存储，恢复时不能依赖 manifest 的 root 顺序。
        // 全量 checkpoint 的公共表示始终按稳定 ID 排序，确保 checkpoint 比较和后续
        // 重启恢复比较的是同一份 canonical 表示，而不是 HashMap/分组遍历顺序。
        checkpoint
            .tasks
            .sort_by(|left, right| left.task_id.as_str().cmp(right.task_id.as_str()));
        checkpoint
            .leases
            .sort_by(|left, right| left.lease_id.as_str().cmp(right.lease_id.as_str()));
        Self::validate_checkpoint(&checkpoint)?;
        Self::validate_agent_history(&checkpoint)?;
        Ok(checkpoint)
    }

    pub(super) fn validate_checkpoint(checkpoint: &TaskStoreCheckpoint) -> io::Result<()> {
        let mut task_ids = HashSet::new();
        let mut tasks_by_id = HashMap::new();
        for task in &checkpoint.tasks {
            if task.task_id.as_str().is_empty()
                || task.root_task_id.as_str().is_empty()
                || !task_ids.insert(task.task_id.clone())
            {
                return Err(Self::invalid_projection(
                    "taskId/rootTaskId 为空或 taskId 重复",
                ));
            }
            if let Some(binding) = task.agent_activation() {
                binding
                    .validate(&task.task_id)
                    .map_err(|error| Self::invalid_projection(error.to_string()))?;
            }
            tasks_by_id.insert(task.task_id.clone(), task);
        }
        for task in &checkpoint.tasks {
            let Some(root) = tasks_by_id.get(&task.root_task_id) else {
                return Err(Self::invalid_projection(format!(
                    "task 引用不存在的 rootTaskId: {}",
                    task.root_task_id
                )));
            };
            if root.task_id != root.root_task_id {
                return Err(Self::invalid_projection(format!(
                    "root task 自身归属不合法: {}",
                    root.task_id
                )));
            }
        }

        let mut lease_ids = HashSet::new();
        let mut active_task_ids = HashSet::new();
        for lease in &checkpoint.leases {
            if lease.lease_id.as_str().is_empty() || !lease_ids.insert(lease.lease_id.clone()) {
                return Err(Self::invalid_projection("leaseId 为空或重复"));
            }
            let Some(task) = tasks_by_id.get(&lease.task_id) else {
                return Err(Self::invalid_projection(format!(
                    "lease 引用不存在的 taskId: {}",
                    lease.task_id
                )));
            };
            let Some(root) = tasks_by_id.get(&lease.root_task_id) else {
                return Err(Self::invalid_projection(format!(
                    "lease 引用不存在的 rootTaskId: {}",
                    lease.root_task_id
                )));
            };
            if root.task_id != root.root_task_id
                || task.root_task_id != lease.root_task_id
                || task.mission_id != root.mission_id
            {
                return Err(Self::invalid_projection(format!(
                    "lease 与 task/root 的归属不一致: {}",
                    lease.lease_id
                )));
            }
            if lease.lease_status == TaskLeaseState::Active {
                if !active_task_ids.insert(lease.task_id.clone()) {
                    return Err(Self::invalid_projection(format!(
                        "任务 {} 存在多个活跃租约",
                        lease.task_id
                    )));
                }
                if task.status != TaskStatus::Running {
                    return Err(Self::invalid_projection(format!(
                        "活跃租约 {} 的任务 {} 必须处于 Running，当前为 {:?}",
                        lease.lease_id, lease.task_id, task.status
                    )));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_snapshot(snapshot: &TaskStoreSnapshot) -> io::Result<()> {
        if snapshot.changed_root_ids.is_empty() {
            return Self::validate_checkpoint(&TaskStoreCheckpoint {
                tasks: snapshot.tasks.clone(),
                leases: snapshot.leases.clone(),
            });
        }
        // 状态变更已经在 TaskStore mutation lock 下完成结构校验；增量 checkpoint
        // 只需验证受影响 root 的完整子树和租约，不能因为无关历史任务数量增长而
        // 阻塞当前 Turn。恢复入口仍会对完整 projection 做全量校验。
        let dirty_roots = snapshot.changed_root_ids.iter().collect::<HashSet<_>>();
        Self::validate_checkpoint(&TaskStoreCheckpoint {
            tasks: snapshot
                .tasks
                .iter()
                .filter(|task| dirty_roots.contains(&task.root_task_id))
                .cloned()
                .collect(),
            leases: snapshot
                .leases
                .iter()
                .filter(|lease| dirty_roots.contains(&lease.root_task_id))
                .cloned()
                .collect(),
        })
    }

    pub(super) fn read_projection_manifest_if_present(
        dir: &Path,
    ) -> io::Result<Option<TaskProjectionManifest>> {
        let path = dir.join(TASK_PROJECTION_MANIFEST_FILE);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_projection_manifest(dir).map(Some)
    }

    pub(super) fn discard_uncommitted_projection_directory(dir: &Path) -> io::Result<()> {
        let generations_dir = dir.join(TASK_PROJECTION_GENERATIONS_DIR);
        if generations_dir.exists() {
            fs::remove_dir_all(&generations_dir)?;
        }
        let has_other_entries = fs::read_dir(dir)?.next().transpose()?.is_some();
        if !has_other_entries {
            let _ = fs::remove_dir(dir);
        }
        Ok(())
    }

    pub(super) fn read_projection_manifest(dir: &Path) -> io::Result<TaskProjectionManifest> {
        let content = fs::read(dir.join(TASK_PROJECTION_MANIFEST_FILE))?;
        let manifest: TaskProjectionManifest =
            serde_json::from_slice(&content).map_err(|error| {
                Self::invalid_projection(format!("解析 task projection manifest 失败: {error}"))
            })?;
        if manifest.schema_version != TASK_PROJECTION_SCHEMA_VERSION || manifest.generation == 0 {
            return Err(Self::invalid_projection(
                "task projection manifest 版本或 generation 不合法",
            ));
        }
        Ok(manifest)
    }

    pub(super) fn generation_directory_name(generation: u64) -> String {
        format!("generation-{generation:020}")
    }

    pub(super) fn write_new_file_synced(path: &Path, content: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(content)?;
        file.sync_all()
    }

    pub(super) fn cleanup_stale_generations(
        generations_dir: &Path,
        current: &Path,
    ) -> io::Result<()> {
        let mut removed = false;
        for entry in fs::read_dir(generations_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path == current {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
            removed = true;
        }
        if removed {
            Self::sync_directory(generations_dir)?;
        }
        Ok(())
    }

    pub(super) fn remove_stale_generation(path: &Path, generations_dir: &Path) -> io::Result<()> {
        if path.exists() {
            fs::remove_dir_all(path)?;
            Self::sync_directory(generations_dir)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
        fs::File::open(path)?.sync_all()
    }

    #[cfg(not(unix))]
    pub(super) fn sync_directory(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn invalid_projection(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    pub(super) fn projection_file_name(value: &str) -> String {
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        format!("{encoded}.json")
    }
}

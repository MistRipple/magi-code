use super::*;

impl StateRepository {
    pub(crate) fn save_session_projection_state(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        self.save_session_projection_parts(durable, sidecars, &workspace_roots, None, false)
    }

    /// 只提交指定 session 的 durable projection。
    ///
    /// canonical event 已经按 session 独立落盘；sidecar flush 只需更新发生过
    /// 运行态变更的 session 文件，并维护全局 current/meta 文件。完整 snapshot
    /// 仍由 `save_session_projection_state` 保留给关机和显式一致性操作。
    pub(crate) fn save_session_projection_state_for_sessions(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        session_ids: &[SessionId],
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        let changed = session_ids.iter().cloned().collect::<HashSet<_>>();
        self.save_session_projection_parts(
            durable,
            sidecars,
            &workspace_roots,
            Some(&changed),
            false,
        )
    }

    pub(crate) fn save_session_projection_partial_state_with_roots(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        session_ids: &[SessionId],
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(), DaemonError> {
        let changed = session_ids.iter().cloned().collect::<HashSet<_>>();
        self.save_session_projection_parts(durable, sidecars, workspace_roots, Some(&changed), true)
    }

    /// 持久化导航状态时只提交当前指针，以及缺失的目标会话 projection。
    ///
    /// 导航不改变任何会话事实，因此不能为了保存一个指针重新遍历、重放并序列化
    /// 全部历史会话。新建 materialized session 没有 projection 时才在这里补写该
    /// 单个会话；已有 projection 则保持原文件不动。
    pub(crate) fn save_session_navigation_state(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        target_session_id: Option<&SessionId>,
    ) -> Result<(), DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut cache = self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned");
        let mut event_cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        // 导航只提交一个当前指针（以及在目标会话 projection 缺失时补一份）。缓存里保存着全部会话的
        // projection 全文与对话事件，绝不能为此整体克隆一份再替换：那会让每次切换会话的代价与全部
        // 会话历史的总量成正比（本仓库实测 300 MB 级，单次导航 1–2 秒纯用于克隆与释放）。
        // 这里只记录很小的待应用变更，等事务提交成功后再就地写入缓存；失败时缓存原样不动。
        let mut pending_snapshot: Option<(SessionId, (PathBuf, ContentDigest))> = None;
        let mut rebuilt_event_cache = None;
        let mut writes = Vec::new();

        match (durable.current_session_id.as_ref(), target_session_id) {
            (Some(current), Some(target)) if current != target => {
                return Err(DaemonError::internal(format!(
                    "导航目标与 current session 不一致: {target} != {current}"
                )));
            }
            (Some(current), None) => {
                return Err(DaemonError::internal(format!(
                    "草稿导航仍保留 current session 指针: {current}"
                )));
            }
            _ => {}
        }

        if let Some(session_id) = target_session_id {
            let session = durable
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
                .ok_or_else(|| {
                    DaemonError::internal(format!(
                        "导航目标 session 不存在，拒绝写入 current 指针: {session_id}"
                    ))
                })?;
            let path = self.session_projection_path(
                session_id,
                session.workspace_id.as_deref(),
                &workspace_roots,
            )?;
            if !path.exists() {
                // 只有需要补建缺失的 projection 时才切出目标会话的完整状态。
                let target = durable.durable_state_for_session(session_id);
                let sidecar = sidecars
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| sidecar.session_id == *session_id)
                    .cloned();
                // 极少发生：补建缺失的 projection 需要构造对话事件，只有这时才克隆事件缓存。
                let mut next_event_cache = event_cache.clone();
                let (path, content) = self.build_session_projection_content(
                    &target,
                    sidecar,
                    &workspace_roots,
                    session_id,
                    &mut next_event_cache,
                    false,
                )?;
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content: content.clone(),
                });
                pending_snapshot = Some((session_id.clone(), (path, ContentDigest::of(&content))));
                rebuilt_event_cache = Some(next_event_cache);
            }
        }

        let current_path = self.state_root.join("session-current.json");
        let current_content = serde_json::to_vec_pretty(&durable.current_session_id)
            .map_err(DaemonError::from)
            .and_then(|content| {
                String::from_utf8(content).map_err(|error| {
                    DaemonError::internal(format!("session current state 不是 UTF-8: {error}"))
                })
            })?;
        let current_digest = ContentDigest::of(&current_content);
        if cache
            .global
            .as_ref()
            .map(|(_, previous)| previous != &current_digest)
            .unwrap_or(true)
        {
            writes.push(SessionProjectionWrite {
                path: current_path.clone(),
                content: current_content,
            });
        }
        let next_global = (current_path, current_digest);

        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!("session-navigation-{}", magi_core::UtcMillis::now().0),
            writes,
            removals: Vec::new(),
        };
        self.commit_session_projection_transaction_locked(&transaction, &workspace_roots)?;
        self.ensure_state_layout_marker_locked()?;
        // 事务已提交：就地应用两处小变更，不替换整份缓存。
        if let Some((session_id, snapshot)) = pending_snapshot {
            cache.snapshots.insert(session_id, snapshot);
        }
        cache.global = Some(next_global);
        if let Some(next_event_cache) = rebuilt_event_cache {
            *event_cache = next_event_cache;
        }
        Ok(())
    }

    /// 把 projection 中的 canonical turn 快照转换为事件日志的初始写入序列。
    pub(super) fn initial_canonical_mutations(
        mut turns: Vec<magi_session_store::CanonicalTurn>,
    ) -> Vec<CanonicalTurnMutation> {
        turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        let mut mutations = Vec::with_capacity(turns.len());
        for next in turns {
            if next.status == magi_session_store::CanonicalTurnStatus::Superseded {
                // 最终快照没有记录 Superseded 必须经过 Cancelled 的中间事件；
                // 初始化 canonical event 时补齐这段状态机路径。
                let mut cancelled = next.clone();
                cancelled.status = magi_session_store::CanonicalTurnStatus::Cancelled;
                mutations.push(CanonicalTurnMutation {
                    previous: None,
                    next: cancelled.clone(),
                });
                mutations.push(CanonicalTurnMutation {
                    previous: Some(cancelled),
                    next,
                });
            } else {
                mutations.push(CanonicalTurnMutation {
                    previous: None,
                    next,
                });
            }
        }
        mutations
    }

    pub(super) fn import_workspace_projection_events(
        &self,
        snapshot: &mut SessionProjectionSnapshot,
        projection_path: &Path,
        original_projection_content: &str,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<(SessionConversationProjection, String), DaemonError> {
        let session = snapshot
            .durable
            .sessions
            .first()
            .expect("validated session projection must contain one session");
        let session_id = session.session_id.clone();
        let mutations = Self::initial_canonical_mutations(
            snapshot
                .durable
                .canonical_turns
                .iter()
                .filter(|turn| turn.session_id == session_id)
                .cloned()
                .collect(),
        );
        if mutations.is_empty() {
            return Err(DaemonError::internal(format!(
                "workspace projection 缺少可导入的 canonical turn: {}",
                projection_path.display()
            )));
        }

        let event_root = self.session_event_root(&session_id);
        if event_root.exists() {
            return Err(DaemonError::internal(format!(
                "workspace projection 导入要求 canonical event 根不存在: {}",
                event_root.display()
            )));
        }
        let empty_projection = SessionConversationProjection::load(&event_root, &session_id)?;
        let prepared = empty_projection.prepare_transaction_write(
            &event_root,
            &session_id,
            &mutations,
            &snapshot
                .durable
                .inputs
                .iter()
                .cloned()
                .map(|next| SessionInputMutation {
                    previous: None,
                    next,
                })
                .collect::<Vec<_>>(),
        )?;
        let original_event_seq = snapshot.canonical_event_seq;
        snapshot.canonical_event_seq = prepared.projection.last_event_seq();
        Self::validate_cached_canonical_projection(
            &snapshot.durable.canonical_turns,
            snapshot.canonical_event_seq,
            &prepared.projection,
            projection_path,
        )?;
        let projection_content = if snapshot.canonical_event_seq == original_event_seq {
            original_projection_content.to_string()
        } else {
            serde_json::to_vec_pretty(snapshot)
                .map_err(DaemonError::from)
                .and_then(|content| {
                    String::from_utf8(content).map_err(|error| {
                        DaemonError::internal(format!(
                            "workspace session projection 不是 UTF-8: {error}"
                        ))
                    })
                })?
        };
        let mut writes = vec![SessionProjectionWrite {
            path: prepared.path,
            content: prepared.content,
        }];
        if snapshot.canonical_event_seq != original_event_seq {
            writes.push(SessionProjectionWrite {
                path: projection_path.to_path_buf(),
                content: projection_content.clone(),
            });
        }
        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!(
                "workspace-session-import-{}-{}",
                magi_core::UtcMillis::now().0,
                session_id
            ),
            writes,
            removals: Vec::new(),
        };
        self.commit_session_projection_transaction_locked(&transaction, workspace_roots)?;
        Ok((prepared.projection, projection_content))
    }

    pub(crate) fn load_session_projections(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        self.load_session_projections_scoped(workspace_roots, false)
    }

    pub(crate) fn load_project_session_projections(
        &self,
        project: &magi_workspace::WorkspaceRecord,
    ) -> Result<
        (
            SessionDurableState,
            SessionExecutionSidecarStoreState,
            Vec<UnavailableSession>,
        ),
        DaemonError,
    > {
        // 热导入使用独立读取缓存和同一持久写锁。SessionStore 校验冲突失败时，
        // 不能先覆盖其它会话的缓存；后续正常写入会从同一事件日志按需加载。
        let mut reader = Self::new(self.state_root.clone());
        reader.write_lock = self.write_lock.clone();
        let (durable, sidecars) = reader.load_session_projections_scoped(
            &[(project.workspace_id.to_string(), project.native_root_path())],
            true,
        )?;
        Ok((durable, sidecars, reader.unavailable_sessions()))
    }

    pub(super) fn load_session_projections_scoped(
        &self,
        workspace_roots: &[(String, PathBuf)],
        project_only: bool,
    ) -> Result<(SessionDurableState, SessionExecutionSidecarStoreState), DaemonError> {
        self.recover_session_projection_transaction(workspace_roots)?;
        let _project_read_guard = project_only.then(|| {
            self.write_lock
                .lock()
                .expect("state repository write lock poisoned")
        });
        self.restore_quarantined_session_events(workspace_roots)?;
        let mut durable = SessionDurableState::default();
        let mut sidecars = SessionExecutionSidecarStoreState::default();
        let mut cache = SessionProjectionCache::default();
        let mut event_cache = HashMap::new();
        let mut event_accepted_submissions = Vec::new();
        let mut loaded_snapshots = HashSet::<magi_core::SessionId>::new();
        let mut unavailable = Vec::<UnavailableSession>::new();
        let workspace_root_by_id = workspace_roots.iter().cloned().collect::<HashMap<_, _>>();
        let mut roots = if project_only {
            Vec::new()
        } else {
            vec![(String::new(), self.state_root.clone())]
        };
        roots.extend(workspace_roots.iter().cloned());

        for (workspace_id, _workspace_root) in &roots {
            let projection_root = if workspace_id.is_empty() {
                self.session_projection_root()
            } else {
                self.project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-projections")
            };
            if projection_root.exists() {
                for entry in fs::read_dir(&projection_root)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    // 单个会话的投影或事件日志损坏只让该会话不可用：原文件保留在原处，
                    // 记录原因供界面提示，其余会话照常启动。
                    let loaded = (|| -> Result<_, DaemonError> {
                        let mut content = fs::read_to_string(&path)?;
                        let mut snapshot: SessionProjectionSnapshot =
                            serde_json::from_str(&content).map_err(|error| {
                                DaemonError::internal(format!(
                                    "解析 session projection 失败 {}: {error}",
                                    path.display()
                                ))
                            })?;
                        let session_id = Self::validate_session_projection(&snapshot, &path)?;
                        let session = snapshot
                            .durable
                            .sessions
                            .first()
                            .expect("projection validation requires one session");
                        match (workspace_id.is_empty(), session.workspace_id.as_deref()) {
                            (true, Some(actual_workspace_id)) => {
                                return Err(DaemonError::internal(format!(
                                    "全局 session projection 包含 workspace 归属，拒绝自动移动: {} ({actual_workspace_id})",
                                    path.display()
                                )));
                            }
                            (false, Some(actual_workspace_id))
                                if actual_workspace_id == workspace_id => {}
                            (false, actual_workspace_id) => {
                                return Err(DaemonError::internal(format!(
                                    "workspace session projection 归属与扫描根不一致: expected={workspace_id}, actual={} ({})",
                                    actual_workspace_id.unwrap_or("<none>"),
                                    path.display()
                                )));
                            }
                            (true, None) => {}
                        }
                        let expected_path = self.session_projection_path(
                            &session_id,
                            session.workspace_id.as_deref(),
                            &workspace_root_by_id,
                        )?;
                        if path != expected_path {
                            return Err(DaemonError::internal(format!(
                                "session projection 不在规范归属路径，拒绝自动移动: {} != {}",
                                path.display(),
                                expected_path.display()
                            )));
                        }
                        if loaded_snapshots.contains(&session_id) {
                            let previous_path = cache
                                .snapshots
                                .get(&session_id)
                                .map(|(cached_path, _)| cached_path.display().to_string())
                                .unwrap_or_else(|| "<unknown>".to_string());
                            return Err(DaemonError::internal(format!(
                                "session {} 存在重复 projection，拒绝自动移动或删除: {} 与 {}，规范路径为 {}",
                                session_id,
                                previous_path,
                                path.display(),
                                expected_path.display()
                            )));
                        }
                        let event_root = self.session_event_root(&session_id);
                        let event_root_existed = event_root.exists();
                        let mut event_projection =
                            SessionConversationProjection::load(&event_root, &session_id)?;
                        if snapshot.canonical_event_seq > event_projection.last_event_seq() {
                            if !workspace_id.is_empty()
                                && !event_root_existed
                                && event_projection.last_event_seq() == 0
                                && !snapshot.durable.canonical_turns.is_empty()
                            {
                                let original_projection_content = content.clone();
                                (event_projection, content) = self
                                    .import_workspace_projection_events(
                                        &mut snapshot,
                                        &path,
                                        &original_projection_content,
                                        &workspace_root_by_id,
                                    )?;
                                warn!(
                                    %session_id,
                                    projection = %path.display(),
                                    canonical_event_seq = snapshot.canonical_event_seq,
                                    "已将可移植工作区 session projection 导入当前 canonical event 状态根"
                                );
                            } else {
                                return Err(DaemonError::internal(format!(
                                    "session projection 的 canonical event 游标超前: {} > {} ({})",
                                    snapshot.canonical_event_seq,
                                    event_projection.last_event_seq(),
                                    path.display()
                                )));
                            }
                        }
                        Self::validate_cached_canonical_projection(
                            &snapshot.durable.canonical_turns,
                            snapshot.canonical_event_seq,
                            &event_projection,
                            &path,
                        )?;
                        snapshot.durable.canonical_turns =
                            event_projection.canonical_turns().to_vec();
                        snapshot.durable.inputs = event_projection.inputs().to_vec();
                        // canonical event 是唯一事实源；sidecar 只是执行恢复缓存。
                        // event 游标相等只说明 durable canonical 已同步，不能证明 sidecar
                        // 没有停留在较早的活动快照，因此每次恢复都从事件结果重建它。
                        if let Some(sidecar) = snapshot.sidecar.as_mut() {
                            sidecar
                                .project(event_projection.canonical_turns())
                                .map_err(|error| {
                                    DaemonError::internal(format!(
                                        "canonical event 无法重建 sidecar projection {}: {error}",
                                        path.display()
                                    ))
                                })?;
                        }
                        Ok((session_id, snapshot, content, event_projection))
                    })();
                    let (session_id, snapshot, content, event_projection) = match loaded {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            unavailable.push(Self::unavailable_session_from_path(
                                &path,
                                (!workspace_id.is_empty()).then(|| workspace_id.clone()),
                                &error,
                            ));
                            continue;
                        }
                    };
                    if let Some(sidecar) = snapshot.sidecar {
                        sidecars.upsert_runtime_sidecar(sidecar);
                    }
                    durable.append_state_without_current(snapshot.durable);
                    for record in event_projection.accepted_submissions() {
                        Self::merge_event_acceptance_into_projection(
                            &mut durable,
                            &mut sidecars,
                            record,
                        );
                    }
                    event_accepted_submissions.extend(
                        event_projection.accepted_submissions().iter().cloned().map(
                            |mut record| {
                                // event segment 已经恢复了 session/sidecar，只有 task
                                // checkpoint 仍需要在启动后按 TaskStore 代际收敛。
                                record.session_checkpointed = true;
                                record
                            },
                        ),
                    );
                    loaded_snapshots.insert(session_id.clone());
                    event_cache.insert(session_id.clone(), event_projection);
                    cache
                        .snapshots
                        .insert(session_id, (path, ContentDigest::of(&content)));
                }
            }

            if !workspace_id.is_empty() {
                let meta_path = self
                    .project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-workspace-meta.json");
                if meta_path.exists() {
                    let content = fs::read_to_string(&meta_path)?;
                    let meta: SessionDurableState = serde_json::from_str(&content)?;
                    durable.notifications.extend(meta.notifications);
                    let digest = ContentDigest::of(&content);
                    cache
                        .workspace_meta
                        .insert(workspace_id.clone(), (meta_path, digest));
                }
            }
        }

        // accepted event 可能已经 durable，但对应的 session projection 尚未写入。
        // 事件目录本身是权威源，必须从目录发现并恢复这类 session，不能把它误判成
        // 没有归属的孤立目录后丢弃。
        let event_parent = self.state_root.join("session-events");
        if !project_only && event_parent.exists() {
            let mut event_roots = fs::read_dir(&event_parent)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?;
            event_roots.sort();
            for event_root in event_roots {
                if !event_root.is_dir() {
                    return Err(DaemonError::internal(format!(
                        "canonical event 根目录包含非 session 目录: {}",
                        event_root.display()
                    )));
                }
                // 投影已判定不可用的会话保留原事件目录，不再按 event-only 会话恢复。
                if unavailable
                    .iter()
                    .any(|session| self.session_event_root(&session.session_id) == event_root)
                {
                    continue;
                }
                let loaded = (|| -> Result<_, DaemonError> {
                    let session_id =
                        SessionConversationProjection::read_session_id_from_root(&event_root)?;
                    if self.session_event_root(&session_id) != event_root {
                        return Err(DaemonError::internal(format!(
                            "canonical event 目录与 session 归属不一致: {}",
                            event_root.display()
                        )));
                    }
                    if event_cache.contains_key(&session_id) {
                        return Ok(None);
                    }
                    let event_projection =
                        SessionConversationProjection::load(&event_root, &session_id)?;
                    if event_projection.accepted_submissions().is_empty() {
                        return Ok(Some((session_id, event_projection, None)));
                    }
                    // 先只读校验并在空容器里构建 sidecar，确认整个会话可恢复后才提交到
                    // 启动状态，避免损坏会话留下半合并的事实。
                    let mut staged_durable = SessionDurableState::default();
                    let mut staged_sidecars = SessionExecutionSidecarStoreState::default();
                    let mut accepted = Vec::new();
                    for record in event_projection.accepted_submissions() {
                        let mut record = record.clone();
                        record.session_checkpointed = true;
                        Self::merge_event_acceptance_into_projection(
                            &mut staged_durable,
                            &mut staged_sidecars,
                            &record,
                        );
                        accepted.push(record);
                    }
                    let mut new_turns = Vec::new();
                    for turn in event_projection.canonical_turns() {
                        match durable
                            .canonical_turns
                            .iter()
                            .find(|existing| existing.turn_id == turn.turn_id)
                        {
                            Some(existing) if existing != turn => {
                                return Err(DaemonError::internal(format!(
                                    "canonical event 与 session projection 的 turn 冲突: {}",
                                    turn.turn_id
                                )));
                            }
                            Some(_) => {}
                            None => new_turns.push(turn.clone()),
                        }
                    }
                    let sidecar = staged_sidecars.runtime_sidecar(&session_id).ok_or_else(
                        || {
                            DaemonError::internal(format!(
                                "accepted canonical event 缺少 event-only sidecar projection: {}",
                                event_root.display()
                            ))
                        },
                    )?;
                    sidecar.project(event_projection.canonical_turns())
                    .map_err(|error| {
                        DaemonError::internal(format!(
                            "canonical event 无法重建 event-only sidecar projection {}: {error}",
                            event_root.display()
                        ))
                    })?;
                    Ok(Some((
                        session_id,
                        event_projection,
                        Some((accepted, new_turns, sidecar)),
                    )))
                })();
                match loaded {
                    Ok(None) => {}
                    Ok(Some((session_id, event_projection, staged))) => {
                        // 没有 accepted 事实时无法从事件本身重建 session 元数据；最终由
                        // coverage 校验报告该目录确实是损坏的孤立事件日志。
                        if let Some((accepted, new_turns, sidecar)) = staged {
                            for record in &accepted {
                                Self::merge_event_acceptance_into_projection(
                                    &mut durable,
                                    &mut sidecars,
                                    record,
                                );
                            }
                            durable.canonical_turns.extend(new_turns);
                            sidecars.upsert_runtime_sidecar(sidecar);
                            event_accepted_submissions.extend(accepted);
                        }
                        durable
                            .inputs
                            .retain(|input| input.session_id != session_id);
                        durable
                            .inputs
                            .extend(event_projection.inputs().iter().cloned());
                        event_cache.insert(session_id, event_projection);
                    }
                    Err(error) => unavailable.push(Self::unavailable_session_from_path(
                        &event_root,
                        None,
                        &error,
                    )),
                }
            }
        }

        let current_path = self.state_root.join("session-current.json");
        if !project_only && current_path.exists() {
            let content = fs::read_to_string(&current_path)?;
            let current: Option<magi_core::SessionId> =
                serde_json::from_str(&content).map_err(|error| {
                    DaemonError::internal(format!(
                        "解析 session current state 失败 {}: {error}",
                        current_path.display()
                    ))
                })?;
            durable.current_session_id = current;
            cache.global = Some((current_path, ContentDigest::of(&content)));
        }

        let app_meta_path = self.state_root.join("session-app-meta.json");
        if !project_only && app_meta_path.exists() {
            let content = fs::read_to_string(&app_meta_path)?;
            let meta: SessionDurableState = serde_json::from_str(&content)?;
            durable.notifications.extend(meta.notifications);
            cache.app_meta = Some((app_meta_path, ContentDigest::of(&content)));
        }

        // 同一会话已从规范副本成功恢复时，多余的问题副本只保留告警日志，不再把会话
        // 报告为不可用。
        unavailable.retain(|session| {
            !durable
                .sessions
                .iter()
                .any(|loaded| loaded.session_id == session.session_id)
        });
        // current 指向的会话已被判定不可用时只清除指针；其余会话照常启动。
        if durable.current_session_id.as_ref().is_some_and(|current| {
            unavailable
                .iter()
                .any(|session| &session.session_id == current)
        }) {
            durable.current_session_id = None;
        }
        if let Some(current_session_id) = durable.current_session_id.as_ref()
            && !durable
                .sessions
                .iter()
                .any(|session| &session.session_id == current_session_id)
        {
            return Err(DaemonError::internal(format!(
                "session current 指向不存在的 session: {current_session_id}"
            )));
        }

        // 回合事实已交给 SessionStore；事件缓存此后只保留游标与 accepted 记录，
        // 会话再次写入时才从事件目录重放常驻。
        for projection in event_cache.values_mut() {
            projection.release_canonical_turns();
        }
        *self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned") = cache;
        *self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned") = event_cache;
        *self
            .event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned") = event_accepted_submissions;
        *self
            .unavailable_sessions
            .lock()
            .expect("unavailable sessions lock poisoned") = unavailable;
        Ok((durable, sidecars))
    }

    /// 删除一个启动时无法加载的会话：投影文件与 canonical 事件目录在同一个持久事务里移除，
    /// 随后从不可用列表摘掉。只处理本次加载记录在案的会话，路径取自记录，不接受调用方给定的路径。
    pub(crate) fn discard_unavailable_session(
        &self,
        session_id: &magi_core::SessionId,
    ) -> Result<UnavailableSession, DaemonError> {
        let workspace_roots = self.workspace_projection_roots()?;
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut unavailable = self
            .unavailable_sessions
            .lock()
            .expect("unavailable sessions lock poisoned");
        let index = unavailable
            .iter()
            .position(|session| &session.session_id == session_id)
            .ok_or_else(|| DaemonError::internal(format!("会话不在不可用列表中: {session_id}")))?;
        let entry = unavailable[index].clone();
        // 加载时只在内存里清除了指向该会话的 current；磁盘指针仍在。会话文件删除后
        // 若指针还指着它，下一次启动会因“current 指向不存在的 session”整体失败，
        // 因此在同一个事务里把指针改写为空。
        let current_path = self.state_root.join("session-current.json");
        let points_at_session = match fs::read_to_string(&current_path) {
            Ok(content) => serde_json::from_str::<Option<magi_core::SessionId>>(&content)
                .is_ok_and(|current| current.as_ref() == Some(session_id)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let current_cleared = String::from("null");
        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!(
                "discard-unavailable-session-{}",
                magi_core::UtcMillis::now().0
            ),
            writes: if points_at_session {
                vec![SessionProjectionWrite {
                    path: current_path.clone(),
                    content: current_cleared.clone(),
                }]
            } else {
                Vec::new()
            },
            removals: vec![
                SessionProjectionRemoval {
                    path: PathBuf::from(&entry.source_path),
                    kind: SessionProjectionRemovalKind::File,
                },
                SessionProjectionRemoval {
                    path: self.session_event_root(session_id),
                    kind: SessionProjectionRemovalKind::Directory,
                },
            ],
        };
        self.commit_session_projection_transaction_locked(&transaction, &workspace_roots)?;
        if points_at_session {
            self.session_projection_cache
                .lock()
                .expect("session projection cache lock poisoned")
                .global = Some((current_path, ContentDigest::of(&current_cleared)));
        }
        self.session_event_cache
            .lock()
            .expect("session event cache lock poisoned")
            .remove(session_id);
        unavailable.remove(index);
        Ok(entry)
    }

    /// 最近一次加载中被判定不可用的会话。
    pub(crate) fn unavailable_sessions(&self) -> Vec<UnavailableSession> {
        self.unavailable_sessions
            .lock()
            .expect("unavailable sessions lock poisoned")
            .clone()
    }

    pub(super) fn unavailable_session_from_path(
        path: &Path,
        workspace_id: Option<String>,
        error: &DaemonError,
    ) -> UnavailableSession {
        let storage_key = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let session = UnavailableSession {
            session_id: magi_core::SessionId::new(Self::decode_session_storage_key(storage_key)),
            workspace_id,
            source_path: path.display().to_string(),
            reason: error.to_string(),
        };
        warn!(
            session_id = %session.session_id,
            path = %session.source_path,
            reason = %session.reason,
            "会话持久化文件损坏，已标记为不可用并保留原文件，其余会话继续启动"
        );
        session
    }

    /// `session_projection_file_name` 的逆变换：还原 `%XX` 转义的会话 ID。
    pub(super) fn decode_session_storage_key(storage_key: &str) -> String {
        let bytes = storage_key.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%'
                && let Some(byte) = storage_key
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                decoded.push(byte);
                index += 3;
                continue;
            }
            decoded.push(bytes[index]);
            index += 1;
        }
        String::from_utf8_lossy(&decoded).into_owned()
    }

    pub(super) fn merge_event_acceptance_into_projection(
        durable: &mut SessionDurableState,
        sidecars: &mut SessionExecutionSidecarStoreState,
        record: &AcceptedSubmissionRecord,
    ) {
        match durable
            .sessions
            .iter_mut()
            .find(|session| session.session_id == record.session.session.session_id)
        {
            Some(existing) if record.session.session.updated_at.0 > existing.updated_at.0 => {
                *existing = record.session.session.clone();
            }
            Some(_) => {}
            None => durable.sessions.push(record.session.session.clone()),
        }
        if !durable
            .timeline
            .iter()
            .any(|entry| entry.entry_id == record.session.timeline_entry.entry_id)
        {
            durable.timeline.push(record.session.timeline_entry.clone());
        }
        let should_update_sidecar = sidecars
            .runtime_sidecar(&record.session.sidecar.session_id)
            .is_none_or(|existing| record.session.sidecar.updated_at.0 > existing.updated_at.0);
        if should_update_sidecar {
            sidecars.upsert_runtime_sidecar(record.session.sidecar.clone());
        }
    }

    /// 校验事件目录都有已恢复的 session 归属。首次 accepted 持久化可能在事件写入后、
    /// projection 写入前崩溃；此时事件中的 accepted 事实会先把 session 恢复进内存，再调用
    /// 本方法确认该目录确实有归属。
    ///
    /// 没有任何归属的事件目录（典型来源：用户删除了工作区目录，其下会话的 projection 随之消失，
    /// 事件日志却留在全局 `session-events`）**不阻止启动**：整目录移到 `orphaned-session-events/`
    /// 隔离保留（只移动，不删除），启动日志给出告警。工作区本身按空白工作区加载。
    pub(crate) fn validate_session_event_log_coverage(
        &self,
        durable: &SessionDurableState,
    ) -> Result<(), DaemonError> {
        let event_parent = self.state_root.join("session-events");
        if !event_parent.exists() {
            return Ok(());
        }
        let expected = durable
            .sessions
            .iter()
            .map(|session| {
                (
                    self.session_event_root(&session.session_id),
                    session.session_id.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let event_cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        // 不可用会话的事件目录是需要保留的原始数据，既不属于已恢复会话，也不能被当成
        // 孤立日志移走。
        let unavailable_roots = self
            .unavailable_sessions()
            .iter()
            .flat_map(|session| {
                [
                    self.session_event_root(&session.session_id),
                    PathBuf::from(&session.source_path),
                ]
            })
            .collect::<HashSet<_>>();
        let mut unowned = Vec::new();
        for entry in fs::read_dir(&event_parent)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                return Err(DaemonError::internal(format!(
                    "canonical event 根目录包含非 session 目录: {}",
                    path.display()
                )));
            }
            if unavailable_roots.contains(&path) {
                continue;
            }
            let Some(session_id) = expected.get(&path) else {
                unowned.push(path);
                continue;
            };
            if !event_cache.contains_key(session_id) {
                return Err(DaemonError::internal(format!(
                    "canonical event 目录未包含在本次恢复结果中: {}",
                    path.display()
                )));
            }
        }
        drop(event_cache);
        self.quarantine_unowned_session_events(unowned)
    }

    /// 被隔离的无归属事件日志所在目录。
    pub(super) fn orphaned_session_events_root(&self) -> PathBuf {
        self.state_root.join("orphaned-session-events")
    }

    /// 工作区目录复原后，把它的会话事件日志从隔离区移回原位。
    ///
    /// 会话投影（全局或工作区 `.magi/session-projections`）重新出现，而同名事件日志只在隔离区时，
    /// 说明这个会话的数据又回来了：必须在加载投影之前归位，否则投影会因缺少 canonical 事件而无法
    /// 加载。只移动、不覆盖；`session-events` 下已有同名目录时保持原样。
    pub(super) fn restore_quarantined_session_events(
        &self,
        workspace_roots: &[(String, PathBuf)],
    ) -> Result<(), DaemonError> {
        let quarantine_root = self.orphaned_session_events_root();
        if !quarantine_root.is_dir() {
            return Ok(());
        }
        let event_parent = self.state_root.join("session-events");
        let mut projection_roots = vec![self.session_projection_root()];
        projection_roots.extend(workspace_roots.iter().map(|(id, _)| {
            self.project_storage_root(id)
                .join(".magi")
                .join("session-projections")
        }));
        for projection_root in projection_roots {
            let Ok(entries) = fs::read_dir(&projection_root) else {
                continue;
            };
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let quarantined = quarantine_root.join(stem);
                let target = event_parent.join(stem);
                if !quarantined.is_dir() || target.exists() {
                    continue;
                }
                fs::create_dir_all(&event_parent)?;
                fs::rename(&quarantined, &target)?;
                Self::sync_parent_directory(&quarantined);
                Self::sync_parent_directory(&target);
                tracing::info!(
                    session_events = %stem.to_string_lossy(),
                    "会话数据已复原，事件日志已从隔离区归位"
                );
            }
        }
        Ok(())
    }

    /// 把没有任何归属的事件目录整体移到 `orphaned-session-events/`（只移动，不删除）。
    pub(super) fn quarantine_unowned_session_events(
        &self,
        unowned: Vec<PathBuf>,
    ) -> Result<(), DaemonError> {
        if unowned.is_empty() {
            return Ok(());
        }
        let quarantine_root = self.orphaned_session_events_root();
        fs::create_dir_all(&quarantine_root)?;
        for path in unowned {
            let Some(name) = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
            else {
                continue;
            };
            let mut target = quarantine_root.join(&name);
            if target.exists() {
                target = quarantine_root.join(format!("{name}-{}", magi_core::UtcMillis::now().0));
            }
            fs::rename(&path, &target)?;
            Self::sync_parent_directory(&path);
            Self::sync_parent_directory(&target);
            tracing::warn!(
                session_events = %name,
                quarantined_to = %target.display(),
                "会话事件日志没有对应的会话（工作区或会话数据已被删除），已隔离保留，不影响启动"
            );
        }
        Ok(())
    }

    pub(crate) fn workspace_projection_roots(
        &self,
    ) -> Result<HashMap<String, PathBuf>, DaemonError> {
        let state = self.load_workspace_durable_state()?;
        let mut roots = HashMap::new();
        for workspace in state.workspaces {
            // 工作区稳定身份只在启动恢复时校验；这里位于每次保存与会话导航的热路径。
            roots.insert(
                workspace.workspace_id.to_string(),
                workspace.native_root_path(),
            );
        }
        Ok(roots)
    }

    pub(super) fn session_projection_path(
        &self,
        session_id: &magi_core::SessionId,
        workspace_id: Option<&str>,
        workspace_roots: &HashMap<String, PathBuf>,
    ) -> Result<PathBuf, DaemonError> {
        let projection_root = match workspace_id {
            None => self.session_projection_root(),
            Some(workspace_id) => {
                if !workspace_roots.contains_key(workspace_id) {
                    return Err(DaemonError::internal(format!(
                        "session {session_id} 引用了未注册 workspace，拒绝回落到全局目录: {workspace_id}"
                    )));
                }
                self.project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-projections")
            }
        };
        Ok(projection_root.join(Self::session_projection_file_name(session_id)))
    }

    pub(super) fn validate_session_projection(
        snapshot: &SessionProjectionSnapshot,
        path: &Path,
    ) -> Result<magi_core::SessionId, DaemonError> {
        if snapshot.durable.current_session_id.is_some() || snapshot.durable.sessions.len() != 1 {
            return Err(DaemonError::internal(format!(
                "session projection 必须只包含一个 session 且不能包含 current 指针: {}",
                path.display()
            )));
        }
        let session_id = snapshot.durable.sessions[0].session_id.clone();
        let scoped = snapshot
            .durable
            .inputs
            .iter()
            .all(|input| input.session_id == session_id && input.validate_snapshot().is_ok())
            && snapshot
                .durable
                .timeline
                .iter()
                .all(|entry| entry.session_id == session_id)
            && snapshot
                .durable
                .canonical_turns
                .iter()
                .all(|turn| turn.session_id == session_id)
            && snapshot
                .durable
                .notifications
                .iter()
                .all(|notification| notification.session_id.as_ref() == Some(&session_id))
            && snapshot
                .durable
                .goals
                .iter()
                .all(|goal| goal.session_id == session_id)
            && snapshot
                .durable
                .plans
                .iter()
                .all(|plan| plan.session_id == session_id)
            && snapshot
                .durable
                .thread_registry
                .iter()
                .all(|thread| thread.session_id == session_id);
        if !scoped {
            return Err(DaemonError::internal(format!(
                "session projection 混入其他 session 的事实: {}",
                path.display()
            )));
        }
        let thread_ids = snapshot
            .durable
            .thread_registry
            .iter()
            .map(|thread| &thread.thread_id)
            .collect::<HashSet<_>>();
        if snapshot
            .durable
            .thread_context_checkpoints
            .iter()
            .any(|checkpoint| !thread_ids.contains(&checkpoint.thread_id))
        {
            return Err(DaemonError::internal(format!(
                "session projection 包含无归属 thread 的上下文检查点: {}",
                path.display()
            )));
        }
        if let Some(sidecar) = snapshot.sidecar.as_ref() {
            let ownership_matches = sidecar
                .ownership
                .session_id
                .as_ref()
                .is_none_or(|owner| owner == &session_id);
            let chain_matches = sidecar
                .active_execution_chain
                .as_ref()
                .is_none_or(|chain| chain.session_id == session_id);
            if sidecar.session_id != session_id || !ownership_matches || !chain_matches {
                return Err(DaemonError::internal(format!(
                    "session projection 的 sidecar 归属不一致: {}",
                    path.display()
                )));
            }
        }
        Ok(session_id)
    }

    /// 只保留事件日志负责记录的 Turn 事实；目标状态推导出的标注在恢复时重新计算，
    /// 不参与缓存与事件重放的一致性比较。
    pub(super) fn event_fact_turns(
        turns: &[magi_session_store::CanonicalTurn],
    ) -> Result<serde_json::Value, DaemonError> {
        let mut value = serde_json::to_value(turns).map_err(DaemonError::from)?;
        for turn in value.as_array_mut().into_iter().flatten() {
            if let Some(metadata) = turn
                .get_mut("metadata")
                .and_then(serde_json::Value::as_object_mut)
            {
                for key in GOAL_DERIVED_TURN_METADATA_KEYS {
                    metadata.remove(key);
                }
            }
        }
        Ok(value)
    }

    pub(super) fn validate_cached_canonical_projection(
        cached_turns: &[magi_session_store::CanonicalTurn],
        cached_event_seq: u64,
        replayed: &SessionConversationProjection,
        path: &Path,
    ) -> Result<(), DaemonError> {
        if cached_event_seq == replayed.last_event_seq() {
            let cached = Self::event_fact_turns(cached_turns)?;
            let authoritative = Self::event_fact_turns(replayed.canonical_turns())?;
            if let Some(difference) = first_json_difference(&cached, &authoritative, "$") {
                return Err(DaemonError::internal(format!(
                    "session projection 与相同游标的 canonical 事件重放结果不一致（{difference}）: {}",
                    path.display()
                )));
            }
            return Ok(());
        }

        for cached_turn in cached_turns {
            let replayed_turn = replayed
                .canonical_turns()
                .iter()
                .find(|turn| turn.turn_id == cached_turn.turn_id)
                .ok_or_else(|| {
                    DaemonError::internal(format!(
                        "session projection 包含事件日志中不存在的 turn {}: {}",
                        cached_turn.turn_id,
                        path.display()
                    ))
                })?;
            replayed_turn
                .validate_update_from(cached_turn)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "session projection 不能推进到 canonical 事件结果 {}: {error}",
                        path.display()
                    ))
                })?;
            for cached_item in &cached_turn.items {
                let replayed_item = replayed_turn
                    .items
                    .iter()
                    .find(|item| item.item_id == cached_item.item_id)
                    .ok_or_else(|| {
                        DaemonError::internal(format!(
                            "session projection 包含事件日志中不存在的 item {}: {}",
                            cached_item.item_id,
                            path.display()
                        ))
                    })?;
                replayed_item
                    .validate_update_from(cached_item)
                    .map_err(|error| {
                        DaemonError::internal(format!(
                            "session projection item 不能推进到 canonical 事件结果 {}: {error}",
                            path.display()
                        ))
                    })?;
            }
        }
        Ok(())
    }

    pub(super) fn build_session_projection_content(
        &self,
        durable: &SessionDurableState,
        sidecar: Option<SessionRuntimeSidecarState>,
        workspace_roots: &HashMap<String, PathBuf>,
        session_id: &SessionId,
        event_cache: &mut HashMap<SessionId, SessionConversationProjection>,
        allow_canonical_event_advance: bool,
    ) -> Result<(PathBuf, String), DaemonError> {
        let path = self.session_projection_path(
            session_id,
            durable
                .sessions
                .first()
                .and_then(|session| session.workspace_id.as_deref()),
            workspace_roots,
        )?;
        let mut next_durable = durable.clone();
        let event_root = self.session_event_root(session_id);
        let mut event_projection = match event_cache.get(session_id) {
            Some(projection) => projection.clone(),
            None => SessionConversationProjection::load(&event_root, session_id)?,
        };
        // 已释放回合的会话自载入以来没有追加过任何事件（追加会先重放常驻），SessionStore 里的
        // 回合与事件重放结果等价，直接使用 `durable` 携带的回合，不必为了保存而重放整段历史。
        if event_projection.is_resident() {
            let memory_canonical =
                serde_json::to_value(&next_durable.canonical_turns).map_err(DaemonError::from)?;
            let authoritative = serde_json::to_value(event_projection.canonical_turns())
                .map_err(DaemonError::from)?;
            if !allow_canonical_event_advance
                && !json_values_semantically_equal(&memory_canonical, &authoritative)
            {
                return Err(DaemonError::internal(format!(
                    "session projection 不能反向生成 canonical 事实: {session_id}"
                )));
            }
            next_durable.canonical_turns = event_projection.canonical_turns().to_vec();
            next_durable.inputs = event_projection.inputs().to_vec();
            event_projection.touch();
        }
        let snapshot = SessionProjectionSnapshot {
            canonical_event_seq: event_projection.last_event_seq(),
            durable: next_durable,
            sidecar,
        };
        Self::validate_session_projection(&snapshot, &path)?;
        let content = serde_json::to_vec_pretty(&snapshot).map_err(DaemonError::from)?;
        let content = String::from_utf8(content).map_err(|error| {
            DaemonError::internal(format!("session projection 不是 UTF-8: {error}"))
        })?;
        event_cache.insert(session_id.clone(), event_projection);
        Ok((path, content))
    }

    pub(super) fn save_session_projection_parts(
        &self,
        durable: &SessionDurableState,
        sidecars: &SessionExecutionSidecarStoreState,
        workspace_roots: &HashMap<String, PathBuf>,
        changed_session_ids: Option<&HashSet<SessionId>>,
        partial_snapshot: bool,
    ) -> Result<(), DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut cache = self
            .session_projection_cache
            .lock()
            .expect("session projection cache lock poisoned");
        let mut event_cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let mut next_cache = if partial_snapshot {
            let snapshots = changed_session_ids
                .into_iter()
                .flat_map(|session_ids| session_ids.iter())
                .filter_map(|session_id| {
                    cache
                        .snapshots
                        .get(session_id)
                        .cloned()
                        .map(|snapshot| (session_id.clone(), snapshot))
                })
                .collect();
            SessionProjectionCache {
                snapshots,
                pending_removals: cache.pending_removals.clone(),
                pending_event_removals: cache.pending_event_removals.clone(),
                global: cache.global.clone(),
                app_meta: cache.app_meta.clone(),
                workspace_meta: cache.workspace_meta.clone(),
            }
        } else {
            cache.clone()
        };
        let mut next_event_cache = if partial_snapshot {
            changed_session_ids
                .into_iter()
                .flat_map(|session_ids| session_ids.iter())
                .filter_map(|session_id| {
                    event_cache
                        .get(session_id)
                        .cloned()
                        .map(|projection| (session_id.clone(), projection))
                })
                .collect()
        } else {
            event_cache.clone()
        };
        let mut writes = Vec::<SessionProjectionWrite>::new();
        let mut removals = Vec::<SessionProjectionRemoval>::new();
        let mut sidecar_by_session = HashMap::new();
        for sidecar in &sidecars.runtime_sidecars {
            if partial_snapshot
                && changed_session_ids
                    .is_some_and(|session_ids| !session_ids.contains(&sidecar.session_id))
            {
                continue;
            }
            if sidecar_by_session
                .insert(sidecar.session_id.clone(), sidecar.clone())
                .is_some()
            {
                return Err(DaemonError::internal(format!(
                    "session {} 存在重复 sidecar",
                    sidecar.session_id
                )));
            }
        }
        let mut retained_ids = HashSet::new();
        for session in &durable.sessions {
            if !retained_ids.insert(session.session_id.clone()) {
                return Err(DaemonError::internal(format!(
                    "拒绝持久化重复 session projection: {}",
                    session.session_id
                )));
            }
        }
        if !partial_snapshot
            && let Some(orphan_sidecar_id) = sidecar_by_session
                .keys()
                .find(|session_id| !retained_ids.contains(*session_id))
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化无 session 归属的 sidecar: {orphan_sidecar_id}"
            )));
        }

        if !partial_snapshot
            && let Some(current_session_id) = durable.current_session_id.as_ref()
            && !retained_ids.contains(current_session_id)
        {
            return Err(DaemonError::internal(format!(
                "拒绝持久化悬空 session current 指针: {current_session_id}"
            )));
        }

        for session in &durable.sessions {
            if changed_session_ids
                .is_some_and(|session_ids| !session_ids.contains(&session.session_id))
            {
                continue;
            }
            let session_id = session.session_id.clone();
            let path = self.session_projection_path(
                &session_id,
                session.workspace_id.as_deref(),
                workspace_roots,
            )?;
            let previous = cache
                .snapshots
                .get(&session_id)
                .filter(|(previous_path, _)| previous_path == &path)
                .map(|(_, digest)| digest.clone());
            let session_durable = durable.durable_state_for_session(&session_id);
            let (built_path, content) = self.build_session_projection_content(
                &session_durable,
                sidecar_by_session.get(&session_id).cloned(),
                workspace_roots,
                &session_id,
                &mut next_event_cache,
                partial_snapshot,
            )?;
            debug_assert_eq!(built_path, path);
            let digest = ContentDigest::of(&content);
            if previous.as_ref() != Some(&digest) {
                writes.push(SessionProjectionWrite {
                    path: path.clone(),
                    content,
                });
            }
            if let Some((old_path, _)) = next_cache.snapshots.get(&session_id)
                && old_path != &path
            {
                return Err(DaemonError::internal(format!(
                    "session projection 归属路径发生变化，拒绝自动移动或删除: {} -> {}",
                    old_path.display(),
                    path.display()
                )));
            }
            next_cache
                .snapshots
                .insert(session_id, (path.clone(), digest));
        }

        if !partial_snapshot {
            let stale_ids = next_cache
                .snapshots
                .iter()
                .filter(|(session_id, _)| !retained_ids.contains(session_id))
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            for session_id in stale_ids {
                if let Some((path, _)) = next_cache.snapshots.remove(&session_id) {
                    next_cache.pending_removals.insert(path);
                }
                next_event_cache.remove(&session_id);
                next_cache
                    .pending_event_removals
                    .insert(self.session_event_root(&session_id));
            }
        }

        // 增量运行态 checkpoint 只拥有被标记 session 的 projection。它的 durable
        // 快照是在释放 SessionStore 锁后传入的，`current_session_id` 可能已经被用户
        // 导航更新；如果在这里照旧写全局 current 指针，就会用旧快照覆盖用户刚选中的
        // 会话。current 指针只由完整投影和导航事务写入，增量 checkpoint 保持原文件和
        // 缓存不变。
        if !partial_snapshot {
            let current_path = self.state_root.join("session-current.json");
            let current_content = serde_json::to_vec_pretty(&durable.current_session_id)
                .map_err(DaemonError::from)?;
            let current_content = String::from_utf8(current_content).map_err(|error| {
                DaemonError::internal(format!("session current state 不是 UTF-8: {error}"))
            })?;
            let current_digest = ContentDigest::of(&current_content);
            if next_cache
                .global
                .as_ref()
                .map(|(_, previous)| previous != &current_digest)
                .unwrap_or(true)
            {
                writes.push(SessionProjectionWrite {
                    path: current_path.clone(),
                    content: current_content,
                });
            }
            next_cache.global = Some((current_path, current_digest));
        }

        if !partial_snapshot {
            let app_meta = SessionDurableState {
                notifications: durable
                    .notifications
                    .iter()
                    .filter(|notification| {
                        matches!(
                            notification.scope,
                            magi_session_store::NotificationScope::App
                        )
                    })
                    .cloned()
                    .collect(),
                ..SessionDurableState::default()
            };
            let app_meta_path = self.state_root.join("session-app-meta.json");
            let app_meta_content =
                serde_json::to_vec_pretty(&app_meta).map_err(DaemonError::from)?;
            let app_meta_content = String::from_utf8(app_meta_content).map_err(|error| {
                DaemonError::internal(format!("session app meta 不是 UTF-8: {error}"))
            })?;
            let app_meta_digest = ContentDigest::of(&app_meta_content);
            if next_cache
                .app_meta
                .as_ref()
                .map(|(_, previous)| previous != &app_meta_digest)
                .unwrap_or(true)
            {
                writes.push(SessionProjectionWrite {
                    path: app_meta_path.clone(),
                    content: app_meta_content,
                });
            }
            next_cache.app_meta = Some((app_meta_path, app_meta_digest));
        }

        if !partial_snapshot {
            for workspace_id in workspace_roots.keys() {
                let meta = SessionDurableState {
                    notifications: durable
                        .notifications
                        .iter()
                        .filter(|notification| {
                            notification.workspace_id.as_deref() == Some(workspace_id.as_str())
                                && matches!(
                                    notification.scope,
                                    magi_session_store::NotificationScope::Workspace
                                )
                        })
                        .cloned()
                        .collect(),
                    ..SessionDurableState::default()
                };
                let path = self
                    .project_storage_root(workspace_id)
                    .join(".magi")
                    .join("session-workspace-meta.json");
                let content = serde_json::to_vec_pretty(&meta).map_err(DaemonError::from)?;
                let content = String::from_utf8(content).map_err(|error| {
                    DaemonError::internal(format!("workspace meta 不是 UTF-8: {error}"))
                })?;
                let digest = ContentDigest::of(&content);
                if next_cache
                    .workspace_meta
                    .get(workspace_id)
                    .map(|(_, previous)| previous != &digest)
                    .unwrap_or(true)
                {
                    writes.push(SessionProjectionWrite {
                        path: path.clone(),
                        content,
                    });
                }
                next_cache
                    .workspace_meta
                    .insert(workspace_id.clone(), (path, digest));
            }
        }

        let pending_removals = next_cache
            .pending_removals
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for path in pending_removals {
            removals.push(SessionProjectionRemoval {
                path,
                kind: SessionProjectionRemovalKind::File,
            });
        }

        let pending_event_removals = next_cache
            .pending_event_removals
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for path in pending_event_removals {
            removals.push(SessionProjectionRemoval {
                path,
                kind: SessionProjectionRemovalKind::Directory,
            });
        }

        let transaction = SessionProjectionTransaction {
            schema_version: SESSION_PROJECTION_TRANSACTION_SCHEMA_VERSION,
            transaction_id: format!("session-projection-{}", magi_core::UtcMillis::now().0),
            writes,
            removals,
        };
        self.commit_session_projection_transaction_locked(&transaction, workspace_roots)?;
        self.ensure_state_layout_marker_locked()?;

        next_cache.pending_removals.clear();
        next_cache.pending_event_removals.clear();
        if partial_snapshot {
            for (session_id, snapshot) in next_cache.snapshots {
                cache.snapshots.insert(session_id, snapshot);
            }
            cache.pending_removals = next_cache.pending_removals;
            cache.pending_event_removals = next_cache.pending_event_removals;
            cache.global = next_cache.global;
            for (session_id, projection) in next_event_cache {
                event_cache.insert(session_id, projection);
            }
            trim_resident_event_projections(&mut event_cache);
        } else {
            *cache = next_cache;
            trim_resident_event_projections(&mut next_event_cache);
            *event_cache = next_event_cache;
        }

        Ok(())
    }

    pub(super) fn session_projection_file_name(session_id: &magi_core::SessionId) -> String {
        let mut encoded = String::new();
        for byte in session_id.as_str().bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        format!("{encoded}.json")
    }

    /// 返回 canonical 事件中尚未完全收敛的 accepted 事实。
    ///
    /// accepted 事实与 canonical event 写在同一个事件事务里，session 侧在事件提交时即已
    /// durable；这里只按已提交的 task manifest 判断 task 侧是否已经 checkpoint。
    pub(crate) fn load_accepted_submissions(
        &self,
    ) -> Result<Vec<AcceptedSubmissionRecord>, DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let committed_generation =
            TaskStore::committed_projection_generation(&self.task_store_projection_path())?
                .unwrap_or(0);
        let mut records = self
            .event_accepted_submissions
            .lock()
            .expect("event accepted submission cache lock poisoned")
            .clone();
        for record in &mut records {
            // Conversation acceptance 没有 Task projection；其 canonical event 本身
            // 已经是完整 durable 事实，不能等待不存在的 task manifest。
            let Some(task) = record.task.as_ref() else {
                record.task_checkpointed = true;
                continue;
            };
            if !record.task_checkpointed
                && (committed_generation > record.task_projection_generation_at_acceptance
                    || TaskStore::committed_projection_contains_root(
                        &self.task_store_projection_path(),
                        &task.root_task_id,
                    )?)
            {
                record.task_checkpointed = true;
            }
        }
        records.retain(|record| !(record.session_checkpointed && record.task_checkpointed));
        Ok(records)
    }

    pub(crate) fn session_projection_root(&self) -> PathBuf {
        self.state_root.join("session-projections")
    }

    pub(crate) fn session_event_root(&self, session_id: &magi_core::SessionId) -> PathBuf {
        self.state_root
            .join("session-events")
            .join(Self::session_projection_file_name(session_id).trim_end_matches(".json"))
    }

    pub(crate) fn canonical_event_next_sequence(
        &self,
        session_id: &magi_core::SessionId,
    ) -> Result<u64, DaemonError> {
        let _write_guard = self
            .write_lock
            .lock()
            .expect("state repository write lock poisoned");
        let mut cache = self
            .session_event_cache
            .lock()
            .expect("session event cache lock poisoned");
        let event_root = self.session_event_root(session_id);
        // 只需要事件游标，已释放回合的投影同样有效。
        if !cache.contains_key(session_id) {
            let projection = SessionConversationProjection::load(&event_root, session_id)?;
            cache.insert(session_id.clone(), projection);
        }
        let next_sequence = cache.get(session_id).map_or(1, |projection| {
            projection.last_event_seq().saturating_add(1).max(1)
        });
        Ok(next_sequence)
    }
}

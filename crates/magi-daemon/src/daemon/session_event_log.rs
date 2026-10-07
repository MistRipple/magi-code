use super::config::DaemonError;
use super::persistence::AcceptedSubmissionRecord;
use magi_core::{SessionId, UtcMillis};
use magi_session_store::{
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnEvent, CanonicalTurnEventKind,
    CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnMutation, CanonicalTurnStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const EVENT_TRANSACTION_SCHEMA_VERSION: u32 = 1;
const EVENT_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
/// 自上一个检查点以来累计这么多个事务后，写入新检查点并截断被覆盖的事务文件。
const CHECKPOINT_TRANSACTION_INTERVAL: usize = 128;
const CHECKPOINT_FILE_PREFIX: &str = "checkpoint-";

/// 事件目录内部的压缩形式：重放到 `last_event_seq`（总是某个事务末尾）后的完整结果。
/// 事件目录仍是唯一权威源，检查点只替代被它覆盖的那段事务（见
/// docs/durable-log-compaction-design.md §2）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalEventCheckpoint {
    schema_version: u32,
    session_id: SessionId,
    last_event_seq: u64,
    canonical_turns: Vec<CanonicalTurn>,
    accepted_submissions: Vec<AcceptedSubmissionRecord>,
}

/// 事务与检查点共有的会话身份字段；读取归属时不解析整份内容。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventFileSessionIdentity {
    session_id: SessionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalEventTransaction {
    schema_version: u32,
    transaction_id: String,
    session_id: SessionId,
    first_event_seq: u64,
    last_event_seq: u64,
    events: Vec<CanonicalTurnEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acceptance: Option<AcceptedSubmissionRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedCanonicalEventWrite {
    pub(crate) path: PathBuf,
    pub(crate) content: String,
    pub(crate) projection: SessionConversationProjection,
}

/// canonical 对话事实的重放结果。projection 文件只缓存该结果；events 目录才是权威源。
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionConversationProjection {
    session_id: Option<SessionId>,
    last_event_seq: u64,
    canonical_turns: Vec<CanonicalTurn>,
    accepted_submissions: Vec<AcceptedSubmissionRecord>,
    /// 已释放回合：只保留事件游标与 accepted 记录。回合事实在 SessionStore 里另有一份，
    /// 不活跃的会话没必要在这里再常驻一份完整副本；再次写入该会话前必须从事件目录重放。
    released: bool,
    /// 最近一次使用的单调序号，用于只让最近活跃的少数会话保持驻留。
    last_touched: u64,
    /// 最近检查点之后已提交的事务数，达到阈值时写入新检查点。
    transactions_since_checkpoint: usize,
}

static TOUCH_CLOCK: AtomicU64 = AtomicU64::new(1);

impl SessionConversationProjection {
    /// 只读取首个 event segment 的 session 归属，不重放整个事件目录。
    ///
    /// 启动阶段需要先判断 event 目录是否已经由 projection 拥有。这个判断不能为了
    /// 读取一个身份字段而重放整段历史，否则每次 daemon 启动都会把同一批 event
    /// 解析两次，历史越大，服务可用时间越长。
    pub(crate) fn read_session_id_from_root(event_root: &Path) -> Result<SessionId, DaemonError> {
        let mut paths = fs::read_dir(event_root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
        paths.sort();
        let path = paths.first().ok_or_else(|| {
            DaemonError::internal(format!(
                "canonical event 目录为空，无法确定 session 归属: {}",
                event_root.display()
            ))
        })?;
        let identity: EventFileSessionIdentity =
            serde_json::from_slice(&fs::read(path)?).map_err(|error| {
                DaemonError::internal(format!(
                    "解析 canonical event 文件归属失败 {}: {error}",
                    path.display()
                ))
            })?;
        Ok(identity.session_id)
    }

    pub(crate) fn load(event_root: &Path, session_id: &SessionId) -> Result<Self, DaemonError> {
        let mut projection = Self {
            session_id: Some(session_id.clone()),
            ..Self::default()
        };
        if !event_root.exists() {
            return Ok(projection);
        }

        let (checkpoint_paths, transaction_paths) = event_file_paths(event_root)?;
        let mut checkpoint_seq = 0;
        if let Some(path) = checkpoint_paths.last() {
            let checkpoint: CanonicalEventCheckpoint = serde_json::from_slice(&fs::read(path)?)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "解析 canonical event checkpoint 失败 {}: {error}",
                        path.display()
                    ))
                })?;
            if checkpoint.schema_version != EVENT_CHECKPOINT_SCHEMA_VERSION
                || &checkpoint.session_id != session_id
                || checkpoint.last_event_seq == 0
                || path.file_name() != checkpoint_file_name(checkpoint.last_event_seq).file_name()
            {
                return Err(DaemonError::internal(format!(
                    "canonical event checkpoint 不合法: {}",
                    path.display()
                )));
            }
            for turn in &checkpoint.canonical_turns {
                validate_turn(turn, session_id)?;
            }
            checkpoint_seq = checkpoint.last_event_seq;
            projection.last_event_seq = checkpoint.last_event_seq;
            projection.canonical_turns = checkpoint.canonical_turns;
            projection.accepted_submissions = checkpoint.accepted_submissions;
        }

        let mut seen_event_ids = HashSet::new();
        for (last_event_seq, path) in transaction_paths {
            // 截断中途崩溃会留下已被检查点覆盖的事务；它们的事实已在检查点内，直接跳过。
            if last_event_seq <= checkpoint_seq {
                continue;
            }
            let transaction: CanonicalEventTransaction = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "解析 canonical event transaction 失败 {}: {error}",
                        path.display()
                    ))
                })?;
            projection.validate_transaction(&transaction, session_id, &path)?;
            for event in transaction.events {
                projection.validate_next_event(&event, session_id, &mut seen_event_ids)?;
                projection.apply_event(event)?;
            }
            if let Some(acceptance) = transaction.acceptance.as_ref() {
                projection.validate_acceptance(acceptance, session_id, &path)?;
                if projection.accepted_submissions.iter().any(|existing| {
                    existing.session.session.session_id == acceptance.session.session.session_id
                        && existing.session.canonical_turn.turn_id
                            == acceptance.session.canonical_turn.turn_id
                }) {
                    return Err(DaemonError::internal(format!(
                        "canonical event transaction 重复 accepted turn: {}",
                        path.display()
                    )));
                }
                projection.accepted_submissions.push(acceptance.clone());
            }
            projection.transactions_since_checkpoint += 1;
        }
        projection.touch();
        Ok(projection)
    }

    /// 在一个原子 segment 内提交一次真实 SessionStore mutation。
    pub(crate) fn append_transaction(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> Result<Self, DaemonError> {
        self.append_transaction_inner(event_root, session_id, mutations, None)
    }

    /// 将 accepted 事实与 canonical 事件写入同一个 event segment。
    ///
    /// event segment 是本次提交唯一的 durable 边界：accepted session、sidecar 和 task
    /// 恢复信息不再先写一个 accepted journal、再写 canonical event，从而避免一次发送
    /// 产生两次同步磁盘写入和两个可能分裂的提交点。
    pub(crate) fn append_transaction_with_acceptance(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: AcceptedSubmissionRecord,
    ) -> Result<Self, DaemonError> {
        self.append_transaction_inner(event_root, session_id, mutations, Some(acceptance))
    }

    fn append_transaction_inner(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
    ) -> Result<Self, DaemonError> {
        let (mut next, transaction) =
            self.plan_transaction_inner(event_root, session_id, mutations, acceptance)?;
        let Some(transaction) = transaction else {
            return Ok(next);
        };
        fs::create_dir_all(event_root)?;
        let path = event_root.join(transaction_file_name(
            transaction.first_event_seq,
            transaction.last_event_seq,
        ));
        if path.exists() {
            let existing: CanonicalEventTransaction = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|error| {
                    DaemonError::internal(format!(
                        "解析已存在 canonical event transaction 失败 {}: {error}",
                        path.display()
                    ))
                })?;
            if existing != transaction {
                return Err(DaemonError::internal(format!(
                    "canonical event transaction 序号冲突: {}",
                    path.display()
                )));
            }
        } else {
            magi_core::fs_atomic::write_atomic(
                &path,
                serde_json::to_vec_pretty(&transaction).map_err(DaemonError::from)?,
            )?;
            next.transactions_since_checkpoint += 1;
        }
        if next.transactions_since_checkpoint >= CHECKPOINT_TRANSACTION_INTERVAL {
            // 事务已经提交；检查点只是压缩，失败时保留计数，下一次提交再试。
            match next.write_checkpoint(event_root, session_id) {
                Ok(()) => next.transactions_since_checkpoint = 0,
                Err(error) => tracing::warn!(
                    %session_id,
                    error = %error,
                    "写入 canonical event checkpoint 失败，保留事务文件待下次压缩"
                ),
            }
        }
        Ok(next)
    }

    /// 原子写入覆盖到 `last_event_seq` 的检查点，再删除被它覆盖的事务与旧检查点。
    ///
    /// 检查点先于删除落盘，任意时刻崩溃都不丢事实；删除失败的遗留文件在加载时被
    /// 跳过，并在下一次检查点时清理。
    fn write_checkpoint(
        &self,
        event_root: &Path,
        session_id: &SessionId,
    ) -> Result<(), DaemonError> {
        let checkpoint = CanonicalEventCheckpoint {
            schema_version: EVENT_CHECKPOINT_SCHEMA_VERSION,
            session_id: session_id.clone(),
            last_event_seq: self.last_event_seq,
            canonical_turns: self.canonical_turns.clone(),
            accepted_submissions: self.accepted_submissions.clone(),
        };
        magi_core::fs_atomic::write_atomic(
            &event_root.join(checkpoint_file_name(self.last_event_seq)),
            serde_json::to_vec(&checkpoint).map_err(DaemonError::from)?,
        )?;
        let (checkpoint_paths, transaction_paths) = event_file_paths(event_root)?;
        let covered = transaction_paths
            .into_iter()
            .filter(|(last_event_seq, _)| *last_event_seq <= self.last_event_seq)
            .map(|(_, path)| path);
        let superseded = checkpoint_paths.into_iter().filter(|path| {
            path.file_name() != checkpoint_file_name(self.last_event_seq).file_name()
        });
        for path in covered.chain(superseded) {
            if let Err(error) = fs::remove_file(&path) {
                tracing::warn!(
                    %session_id,
                    path = %path.display(),
                    error = %error,
                    "删除已被检查点覆盖的 canonical event 文件失败"
                );
            }
        }
        Ok(())
    }

    /// 只规划 canonical event segment，不直接写盘。
    ///
    /// workspace projection 首次接入新的 daemon 状态根时，调用方必须把该 segment
    /// 与 projection 游标更新放入同一个可恢复事务，不能在两个目录分别提交。
    pub(crate) fn prepare_transaction_write(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> Result<PreparedCanonicalEventWrite, DaemonError> {
        let (projection, transaction) =
            self.plan_transaction_inner(event_root, session_id, mutations, None)?;
        let transaction = transaction.ok_or_else(|| {
            DaemonError::internal("canonical event 导入没有产生可提交事件".to_string())
        })?;
        let path = event_root.join(transaction_file_name(
            transaction.first_event_seq,
            transaction.last_event_seq,
        ));
        let content = serde_json::to_vec_pretty(&transaction)
            .map_err(DaemonError::from)
            .and_then(|content| {
                String::from_utf8(content).map_err(|error| {
                    DaemonError::internal(format!(
                        "canonical event transaction 不是 UTF-8: {error}"
                    ))
                })
            })?;
        Ok(PreparedCanonicalEventWrite {
            path,
            content,
            projection,
        })
    }

    fn plan_transaction_inner(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
    ) -> Result<(Self, Option<CanonicalEventTransaction>), DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入前必须先从事件目录重放: {session_id}"
            )));
        }
        if mutations.is_empty() {
            return Err(DaemonError::internal(
                "canonical event transaction 不能为空".to_string(),
            ));
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|stored| stored != session_id)
        {
            return Err(DaemonError::internal(format!(
                "canonical projection session 不匹配: {} != {}",
                self.session_id
                    .as_ref()
                    .map(SessionId::as_str)
                    .unwrap_or_default(),
                session_id
            )));
        }

        let mut next = self.clone();
        next.session_id = Some(session_id.clone());
        let mut events = Vec::new();
        for mutation in mutations {
            next.plan_mutation(session_id, mutation, &mut events)?;
        }
        if events.is_empty() {
            if acceptance.is_some() {
                return Err(DaemonError::internal(
                    "accepted canonical event transaction 没有产生事件".to_string(),
                ));
            }
            return Ok((next, None));
        }

        if let Some(acceptance) = acceptance.as_ref() {
            next.validate_acceptance(acceptance, session_id, event_root)?;
            next.accepted_submissions.push(acceptance.clone());
        }

        let first_event_seq = events
            .first()
            .expect("non-empty canonical event transaction")
            .event_seq;
        let last_event_seq = events
            .last()
            .expect("non-empty canonical event transaction")
            .event_seq;
        let transaction = CanonicalEventTransaction {
            schema_version: EVENT_TRANSACTION_SCHEMA_VERSION,
            transaction_id: transaction_id(session_id, first_event_seq, last_event_seq),
            session_id: session_id.clone(),
            first_event_seq,
            last_event_seq,
            events,
            acceptance,
        };
        Ok((next, Some(transaction)))
    }

    pub(crate) fn last_event_seq(&self) -> u64 {
        self.last_event_seq
    }

    /// 回合是否常驻内存。释放后的投影只能用于读取游标与 accepted 记录。
    pub(crate) fn is_resident(&self) -> bool {
        !self.released
    }

    pub(crate) fn release_canonical_turns(&mut self) {
        self.canonical_turns = Vec::new();
        self.released = true;
    }

    pub(crate) fn touch(&mut self) {
        self.last_touched = TOUCH_CLOCK.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn last_touched(&self) -> u64 {
        self.last_touched
    }

    /// 只有常驻的投影才持有回合。读取已释放投影的回合是调用方的编程错误：
    /// 静默返回空列表会让写入路径基于错误的前置状态计算事件。
    pub(crate) fn canonical_turns(&self) -> &[CanonicalTurn] {
        assert!(
            !self.released,
            "canonical projection 的回合已释放，读取前必须重放常驻"
        );
        &self.canonical_turns
    }

    pub(crate) fn accepted_submissions(&self) -> &[AcceptedSubmissionRecord] {
        &self.accepted_submissions
    }

    fn plan_mutation(
        &mut self,
        session_id: &SessionId,
        mutation: &CanonicalTurnMutation,
        events: &mut Vec<CanonicalTurnEvent>,
    ) -> Result<(), DaemonError> {
        let mut next_turn = mutation.next.clone();
        next_turn.normalize();
        validate_turn(&next_turn, session_id)?;
        let current = self
            .canonical_turns
            .iter()
            .find(|turn| turn.turn_id == next_turn.turn_id)
            .cloned();
        let mut expected = mutation.previous.clone();
        if let Some(expected) = expected.as_mut() {
            expected.normalize();
        }
        if current != expected {
            if current.as_ref() == Some(&next_turn) {
                return Ok(());
            }
            return Err(DaemonError::internal(format!(
                "canonical mutation 前置 projection 不一致: {}",
                next_turn.turn_id
            )));
        }

        match current {
            None => {
                let mut started = turn_shell(&next_turn);
                started.status = CanonicalTurnStatus::Pending;
                started.completed_at = None;
                started.response_duration_ms = None;
                started.usage = None;
                self.push_planned_event(
                    session_id,
                    CanonicalTurnEventKind::TurnStarted,
                    Some(started),
                    None,
                    next_turn.accepted_at,
                    events,
                )?;
                for item in &next_turn.items {
                    self.push_planned_event(
                        session_id,
                        CanonicalTurnEventKind::TurnItemUpsert,
                        None,
                        Some(item.clone()),
                        item.updated_at,
                        events,
                    )?;
                }
                let current_shell = self
                    .canonical_turns
                    .iter()
                    .find(|turn| turn.turn_id == next_turn.turn_id)
                    .map(turn_shell)
                    .expect("turn started event was applied");
                let final_shell = turn_shell(&next_turn);
                if current_shell != final_shell {
                    self.push_planned_event(
                        session_id,
                        turn_event_kind(final_shell.status),
                        Some(final_shell),
                        None,
                        turn_occurred_at(&next_turn),
                        events,
                    )?;
                }
            }
            Some(previous) => {
                next_turn
                    .validate_update_from(&previous)
                    .map_err(|error| DaemonError::internal(error.to_string()))?;
                if let Some(removed) = previous.items.iter().find(|previous_item| {
                    !next_turn
                        .items
                        .iter()
                        .any(|item| item.item_id == previous_item.item_id)
                }) {
                    return Err(DaemonError::internal(format!(
                        "canonical turn {} 不能删除 item {}",
                        next_turn.turn_id, removed.item_id
                    )));
                }
                for item in &next_turn.items {
                    let previous_item = previous
                        .items
                        .iter()
                        .find(|previous_item| previous_item.item_id == item.item_id);
                    if let Some(previous_item) = previous_item {
                        item.validate_update_from(previous_item)
                            .map_err(|error| DaemonError::internal(error.to_string()))?;
                    }
                    if previous_item != Some(item) {
                        self.push_planned_event(
                            session_id,
                            CanonicalTurnEventKind::TurnItemUpsert,
                            None,
                            Some(item.clone()),
                            item.updated_at,
                            events,
                        )?;
                    }
                }
                if turn_shell(&previous) != turn_shell(&next_turn) {
                    self.push_planned_event(
                        session_id,
                        turn_event_kind(next_turn.status),
                        Some(turn_shell(&next_turn)),
                        None,
                        turn_occurred_at(&next_turn),
                        events,
                    )?;
                }
            }
        }

        let applied = self
            .canonical_turns
            .iter()
            .find(|turn| turn.turn_id == next_turn.turn_id)
            .ok_or_else(|| {
                DaemonError::internal(format!(
                    "canonical mutation 未生成 turn: {}",
                    next_turn.turn_id
                ))
            })?;
        if applied != &next_turn {
            return Err(DaemonError::internal(format!(
                "canonical mutation 事件无法完整重建 turn: {}",
                next_turn.turn_id
            )));
        }
        Ok(())
    }

    fn push_planned_event(
        &mut self,
        session_id: &SessionId,
        kind: CanonicalTurnEventKind,
        turn: Option<CanonicalTurn>,
        item: Option<CanonicalTurnItem>,
        occurred_at: UtcMillis,
        events: &mut Vec<CanonicalTurnEvent>,
    ) -> Result<(), DaemonError> {
        let (turn_id, turn_seq) = match (turn.as_ref(), item.as_ref()) {
            (Some(turn), None) => (turn.turn_id.clone(), turn.turn_seq),
            (None, Some(item)) => (item.turn_id.clone(), item.turn_seq),
            _ => {
                return Err(DaemonError::internal(
                    "canonical event 必须且只能携带 turn 或 item".to_string(),
                ));
            }
        };
        let event_seq = self.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.to_string(),
            event_id: event_id(session_id, event_seq),
            event_seq,
            kind,
            session_id: session_id.clone(),
            turn_id,
            turn_seq,
            occurred_at,
            turn,
            item,
        };
        self.apply_event(event.clone())?;
        events.push(event);
        Ok(())
    }

    fn validate_transaction(
        &self,
        transaction: &CanonicalEventTransaction,
        session_id: &SessionId,
        path: &Path,
    ) -> Result<(), DaemonError> {
        let expected_first = self.last_event_seq.saturating_add(1);
        let valid = transaction.schema_version == EVENT_TRANSACTION_SCHEMA_VERSION
            && &transaction.session_id == session_id
            && !transaction.events.is_empty()
            && transaction.first_event_seq == expected_first
            && transaction
                .events
                .first()
                .is_some_and(|event| event.event_seq == transaction.first_event_seq)
            && transaction
                .events
                .last()
                .is_some_and(|event| event.event_seq == transaction.last_event_seq)
            && transaction.transaction_id
                == transaction_id(
                    session_id,
                    transaction.first_event_seq,
                    transaction.last_event_seq,
                )
            && path.file_name()
                == transaction_file_name(transaction.first_event_seq, transaction.last_event_seq)
                    .file_name();
        if !valid {
            return Err(DaemonError::internal(format!(
                "canonical event transaction 不合法: {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn validate_acceptance(
        &self,
        acceptance: &AcceptedSubmissionRecord,
        session_id: &SessionId,
        path: &Path,
    ) -> Result<(), DaemonError> {
        if acceptance.session.session.session_id != *session_id
            || acceptance.session.timeline_entry.session_id != *session_id
            || acceptance.session.canonical_turn.session_id != *session_id
            || acceptance.session.sidecar.session_id != *session_id
            || acceptance
                .session
                .sidecar
                .current_turn
                .as_ref()
                .is_none_or(|turn| turn.turn_id != acceptance.session.canonical_turn.turn_id)
            || !self
                .canonical_turns
                .iter()
                .any(|turn| turn == &acceptance.session.canonical_turn)
        {
            return Err(DaemonError::internal(format!(
                "accepted 事实与 canonical event 不一致: {}",
                path.display()
            )));
        }
        if let Some(superseded_turn) = acceptance.session.superseded_turn.as_ref()
            && !self
                .canonical_turns
                .iter()
                .any(|turn| turn == superseded_turn)
        {
            return Err(DaemonError::internal(format!(
                "accepted 事实引用不存在的 superseded turn: {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn validate_next_event(
        &self,
        event: &CanonicalTurnEvent,
        session_id: &SessionId,
        seen_event_ids: &mut HashSet<String>,
    ) -> Result<(), DaemonError> {
        let expected_seq = self.last_event_seq.saturating_add(1);
        if event.schema_version != CANONICAL_TURN_SCHEMA_VERSION
            || &event.session_id != session_id
            || event.event_seq != expected_seq
            || event.event_id != event_id(session_id, expected_seq)
            || !seen_event_ids.insert(event.event_id.clone())
        {
            return Err(DaemonError::internal(format!(
                "canonical turn event 序列不合法: {}",
                event.event_id
            )));
        }
        Ok(())
    }

    fn apply_event(&mut self, event: CanonicalTurnEvent) -> Result<(), DaemonError> {
        match event.kind {
            CanonicalTurnEventKind::TurnStarted => {
                let turn = event_turn_shell(&event)?;
                if turn.status != CanonicalTurnStatus::Pending
                    || self
                        .canonical_turns
                        .iter()
                        .any(|existing| existing.turn_id == turn.turn_id)
                {
                    return Err(DaemonError::internal(format!(
                        "turn_started 事件状态或身份不合法: {}",
                        event.event_id
                    )));
                }
                self.canonical_turns.push(turn);
            }
            CanonicalTurnEventKind::TurnItemUpsert => {
                let item = event.item.clone().ok_or_else(|| {
                    DaemonError::internal(format!("item 事件缺少 item: {}", event.event_id))
                })?;
                if event.turn.is_some()
                    || item.session_id != event.session_id
                    || item.turn_id != event.turn_id
                    || item.turn_seq != event.turn_seq
                {
                    return Err(DaemonError::internal(format!(
                        "canonical item 与事件身份不一致: {}",
                        event.event_id
                    )));
                }
                validate_item(&item)?;
                let turn = self
                    .canonical_turns
                    .iter_mut()
                    .find(|turn| turn.turn_id == event.turn_id)
                    .ok_or_else(|| {
                        DaemonError::internal(format!(
                            "canonical item 事件引用未知 turn: {}",
                            event.turn_id
                        ))
                    })?;
                match turn
                    .items
                    .iter()
                    .position(|existing| existing.item_id == item.item_id)
                {
                    Some(index) => {
                        item.validate_update_from(&turn.items[index])
                            .map_err(|error| DaemonError::internal(error.to_string()))?;
                        turn.items[index] = item;
                    }
                    None => {
                        if turn
                            .items
                            .iter()
                            .any(|existing| existing.item_seq == item.item_seq)
                            || turn
                                .items
                                .iter()
                                .map(|existing| existing.item_seq)
                                .max()
                                .is_some_and(|max_seq| item.item_seq <= max_seq)
                        {
                            return Err(DaemonError::internal(format!(
                                "canonical item 不是严格追加: {}",
                                item.item_id
                            )));
                        }
                        turn.items.push(item);
                    }
                }
                turn.normalize();
            }
            CanonicalTurnEventKind::TurnUpdated
            | CanonicalTurnEventKind::TurnCompleted
            | CanonicalTurnEventKind::TurnSuperseded => {
                let shell = event_turn_shell(&event)?;
                let expected_kind = turn_event_kind(shell.status);
                if expected_kind != event.kind {
                    return Err(DaemonError::internal(format!(
                        "canonical turn event kind 与状态不一致: {}",
                        event.event_id
                    )));
                }
                let existing = self
                    .canonical_turns
                    .iter_mut()
                    .find(|turn| turn.turn_id == shell.turn_id)
                    .ok_or_else(|| {
                        DaemonError::internal(format!(
                            "canonical turn 更新引用未知 turn: {}",
                            shell.turn_id
                        ))
                    })?;
                let items = existing.items.clone();
                let mut next = shell;
                next.items = items;
                next.validate_update_from(existing)
                    .map_err(|error| DaemonError::internal(error.to_string()))?;
                *existing = next;
            }
        }
        self.canonical_turns.sort_by(|left, right| {
            left.turn_seq
                .cmp(&right.turn_seq)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        self.last_event_seq = event.event_seq;
        Ok(())
    }
}

fn event_turn_shell(event: &CanonicalTurnEvent) -> Result<CanonicalTurn, DaemonError> {
    let turn = event
        .turn
        .clone()
        .ok_or_else(|| DaemonError::internal(format!("turn 事件缺少 turn: {}", event.event_id)))?;
    if event.item.is_some()
        || !turn.items.is_empty()
        || turn.session_id != event.session_id
        || turn.turn_id != event.turn_id
        || turn.turn_seq != event.turn_seq
    {
        return Err(DaemonError::internal(format!(
            "canonical turn shell 与事件身份不一致: {}",
            event.event_id
        )));
    }
    Ok(turn)
}

fn validate_turn(turn: &CanonicalTurn, session_id: &SessionId) -> Result<(), DaemonError> {
    if &turn.session_id != session_id || turn.turn_id.trim().is_empty() {
        return Err(DaemonError::internal(
            "canonical turn session 或 turnId 不合法".to_string(),
        ));
    }
    let mut ids = HashSet::new();
    let mut seqs = HashSet::new();
    for item in &turn.items {
        if item.session_id != turn.session_id
            || item.turn_id != turn.turn_id
            || item.turn_seq != turn.turn_seq
            || !ids.insert(item.item_id.as_str())
            || !seqs.insert(item.item_seq)
        {
            return Err(DaemonError::internal(format!(
                "canonical turn {} 的 item 身份不合法",
                turn.turn_id
            )));
        }
        validate_item(item)?;
    }
    Ok(())
}

fn validate_item(item: &CanonicalTurnItem) -> Result<(), DaemonError> {
    if item.item_id.trim().is_empty() || item.updated_at.0 < item.created_at.0 {
        return Err(DaemonError::internal(format!(
            "canonical item {} 的时间或身份不合法",
            item.item_id
        )));
    }
    if (item.kind == CanonicalTurnItemKind::ToolCall) != item.tool.is_some() {
        return Err(DaemonError::internal(format!(
            "canonical item {} 的 tool payload 与 kind 不一致",
            item.item_id
        )));
    }
    Ok(())
}

fn turn_shell(turn: &CanonicalTurn) -> CanonicalTurn {
    let mut shell = turn.clone();
    shell.items.clear();
    shell
}

fn turn_event_kind(status: CanonicalTurnStatus) -> CanonicalTurnEventKind {
    if status == CanonicalTurnStatus::Superseded {
        CanonicalTurnEventKind::TurnSuperseded
    } else if status.is_terminal() {
        CanonicalTurnEventKind::TurnCompleted
    } else {
        CanonicalTurnEventKind::TurnUpdated
    }
}

fn turn_occurred_at(turn: &CanonicalTurn) -> UtcMillis {
    turn.completed_at
        .or_else(|| {
            turn.items
                .iter()
                .map(|item| item.updated_at)
                .max_by_key(|timestamp| timestamp.0)
        })
        .unwrap_or(turn.accepted_at)
}

fn event_id(session_id: &SessionId, event_seq: u64) -> String {
    format!("{}:{event_seq:020}", session_id.as_str())
}

fn transaction_id(session_id: &SessionId, first: u64, last: u64) -> String {
    format!("{}:{first:020}:{last:020}", session_id.as_str())
}

fn transaction_file_name(first: u64, last: u64) -> PathBuf {
    PathBuf::from(format!("{first:020}-{last:020}.json"))
}

fn checkpoint_file_name(last_event_seq: u64) -> PathBuf {
    PathBuf::from(format!("{CHECKPOINT_FILE_PREFIX}{last_event_seq:020}.json"))
}

type EventFilePaths = (Vec<PathBuf>, Vec<(u64, PathBuf)>);

/// 列出事件目录中的检查点（按序号排序）与事务（`(last_event_seq, path)`，按首序号排序）。
/// 其余文件名一律视为目录损坏。
fn event_file_paths(event_root: &Path) -> Result<EventFilePaths, DaemonError> {
    let mut checkpoints = Vec::new();
    let mut transactions = Vec::new();
    for entry in fs::read_dir(event_root)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        if let Some(seq) = stem.strip_prefix(CHECKPOINT_FILE_PREFIX) {
            if seq.len() != 20 || seq.parse::<u64>().is_err() {
                return Err(DaemonError::internal(format!(
                    "canonical event checkpoint 文件名不合法: {}",
                    path.display()
                )));
            }
            checkpoints.push(path);
            continue;
        }
        let last = stem
            .split_once('-')
            .filter(|(first, last)| first.len() == 20 && last.len() == 20)
            .and_then(|(first, last)| first.parse::<u64>().ok().and(last.parse::<u64>().ok()))
            .ok_or_else(|| {
                DaemonError::internal(format!(
                    "canonical event transaction 文件名不合法: {}",
                    path.display()
                ))
            })?;
        transactions.push((last, path));
    }
    checkpoints.sort();
    transactions.sort_by(|left, right| left.1.cmp(&right.1));
    Ok((checkpoints, transactions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::ThreadId;
    use magi_session_store::{
        CanonicalTurnItemStatus, CanonicalTurnVisibility, CanonicalWorkerRef,
    };
    use std::collections::HashMap;

    fn item(
        session_id: &SessionId,
        item_id: &str,
        item_seq: usize,
        kind: CanonicalTurnItemKind,
        content: &str,
    ) -> CanonicalTurnItem {
        CanonicalTurnItem {
            session_id: session_id.clone(),
            turn_id: "turn-1".to_string(),
            turn_seq: 1,
            item_id: item_id.to_string(),
            item_seq,
            kind,
            created_at: UtcMillis(10),
            status: CanonicalTurnItemStatus::Completed,
            item_version: None,
            updated_at: UtcMillis(10 + item_seq as u64),
            title: None,
            content: Some(content.to_string()),
            blocks: Vec::new(),
            tool: None,
            worker: None::<CanonicalWorkerRef>,
            source_thread_id: ThreadId::new("thread-main"),
            visibility: CanonicalTurnVisibility::default(),
            metadata: HashMap::new(),
        }
    }

    fn turn(session_id: &SessionId, status: CanonicalTurnStatus) -> CanonicalTurn {
        CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "turn-1".to_string(),
            turn_seq: 1,
            accepted_at: UtcMillis(10),
            completed_at: status.is_terminal().then_some(UtcMillis(20)),
            status,
            response_duration_ms: status.is_terminal().then_some(10),
            usage: None,
            items: vec![
                item(
                    session_id,
                    "user-1",
                    1,
                    CanonicalTurnItemKind::UserMessage,
                    "hello",
                ),
                item(
                    session_id,
                    "assistant-1",
                    2,
                    CanonicalTurnItemKind::AssistantText,
                    "world",
                ),
            ],
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn transaction_replays_incremental_items_and_turn_status() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-events");
        let completed = turn(&session_id, CanonicalTurnStatus::Completed);
        let projection = SessionConversationProjection::default()
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: completed.clone(),
                }],
            )
            .expect("transaction should append");
        assert_eq!(projection.last_event_seq(), 4);

        let replayed = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("events should replay");
        assert_eq!(replayed.canonical_turns(), &[completed]);
    }

    fn running_turn_with_items(session_id: &SessionId, assistant_items: usize) -> CanonicalTurn {
        let mut running = turn(session_id, CanonicalTurnStatus::Running);
        running.items.truncate(1);
        for index in 0..assistant_items {
            running.items.push(item(
                session_id,
                &format!("assistant-{index}"),
                index + 2,
                CanonicalTurnItemKind::AssistantText,
                "chunk",
            ));
        }
        running
    }

    /// 逐个事务追加 assistant item，返回最终投影。
    fn append_item_transactions(
        root: &Path,
        session_id: &SessionId,
        transactions: usize,
    ) -> SessionConversationProjection {
        let mut projection = SessionConversationProjection::default();
        let mut previous = None;
        for index in 0..transactions {
            let next = running_turn_with_items(session_id, index);
            projection = projection
                .append_transaction(
                    root,
                    session_id,
                    &[CanonicalTurnMutation {
                        previous: previous.clone(),
                        next: next.clone(),
                    }],
                )
                .expect("transaction should append");
            previous = Some(next);
        }
        projection
    }

    fn event_file_names(root: &Path) -> Vec<String> {
        let mut names = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn checkpoint_replaces_covered_transactions_and_replays_identically() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-checkpoint");
        let projection =
            append_item_transactions(temp.path(), &session_id, CHECKPOINT_TRANSACTION_INTERVAL);
        let checkpoint_seq = projection.last_event_seq();
        assert_eq!(
            event_file_names(temp.path()),
            vec![checkpoint_file_name(checkpoint_seq).display().to_string()],
            "达到阈值后只保留检查点"
        );
        assert_eq!(projection.transactions_since_checkpoint, 0);

        let replayed = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("checkpoint should load");
        assert_eq!(replayed.last_event_seq(), checkpoint_seq);
        assert_eq!(replayed.canonical_turns(), projection.canonical_turns());
        assert_eq!(
            SessionConversationProjection::read_session_id_from_root(temp.path()).unwrap(),
            session_id
        );

        // 检查点之后的增量事务照常追加，加载时只重放这一段。
        let previous = projection.canonical_turns()[0].clone();
        let next = running_turn_with_items(&session_id, CHECKPOINT_TRANSACTION_INTERVAL);
        let advanced = replayed
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(previous),
                    next,
                }],
            )
            .expect("append after checkpoint");
        assert_eq!(event_file_names(temp.path()).len(), 2);
        let reloaded = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("checkpoint plus tail should load");
        assert_eq!(reloaded.canonical_turns(), advanced.canonical_turns());
        assert_eq!(reloaded.transactions_since_checkpoint, 1);
    }

    #[test]
    fn transactions_left_by_interrupted_truncation_are_skipped() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-checkpoint-leftover");
        let full = tempfile::tempdir().expect("tempdir should create");
        // 同样的提交在另一目录不截断地保留全部事务，用来模拟删除前崩溃的遗留文件。
        let projection =
            append_item_transactions(temp.path(), &session_id, CHECKPOINT_TRANSACTION_INTERVAL);
        append_item_transactions(
            full.path(),
            &session_id,
            CHECKPOINT_TRANSACTION_INTERVAL - 1,
        );
        for name in event_file_names(full.path()) {
            fs::copy(full.path().join(&name), temp.path().join(&name)).unwrap();
        }

        let replayed = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("covered leftovers should be skipped");
        assert_eq!(replayed.canonical_turns(), projection.canonical_turns());
    }

    #[test]
    fn transaction_straddling_checkpoint_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-checkpoint-straddle");
        // 首个事务包含 turn_started 与 item 多个事件；检查点落在它中间即为跨界。
        append_item_transactions(temp.path(), &session_id, 3);
        assert!(event_file_names(temp.path())[0].starts_with(&format!("{:020}-", 1)));
        assert!(!event_file_names(temp.path())[0].ends_with(&format!("{:020}.json", 1)));
        SessionConversationProjection::load(temp.path(), &session_id)
            .expect("without checkpoint the log is valid");
        let checkpoint = CanonicalEventCheckpoint {
            schema_version: EVENT_CHECKPOINT_SCHEMA_VERSION,
            session_id: session_id.clone(),
            last_event_seq: 1,
            canonical_turns: Vec::new(),
            accepted_submissions: Vec::new(),
        };
        fs::write(
            temp.path().join(checkpoint_file_name(1)),
            serde_json::to_vec(&checkpoint).unwrap(),
        )
        .unwrap();
        let error = SessionConversationProjection::load(temp.path(), &session_id)
            .expect_err("事务跨越检查点边界必须拒绝");
        assert!(error.to_string().contains("transaction 不合法"), "{error}");
    }

    #[test]
    fn terminal_turn_shell_keeps_request_identity_without_items() {
        let session_id = SessionId::new("session-terminal-shell-request");
        let mut completed = turn(&session_id, CanonicalTurnStatus::Completed);
        completed.metadata.insert(
            "requestId".to_string(),
            serde_json::json!("request-terminal-shell"),
        );

        let shell = turn_shell(&completed);

        assert!(shell.items.is_empty(), "终态事件仍应使用轻量 turn shell");
        assert_eq!(
            shell.metadata.get("requestId"),
            Some(&serde_json::json!("request-terminal-shell")),
            "shell 必须保留用于前端收敛处理态的 Turn 级 requestId"
        );
    }

    #[test]
    fn event_gap_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-gap");
        SessionConversationProjection::default()
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: turn(&session_id, CanonicalTurnStatus::Completed),
                }],
            )
            .expect("transaction should append");
        let path = temp.path().join(transaction_file_name(1, 4));
        fs::rename(&path, temp.path().join(transaction_file_name(2, 5)))
            .expect("transaction should move");
        SessionConversationProjection::load(temp.path(), &session_id)
            .expect_err("event gap must fail");
    }

    #[test]
    fn item_removal_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-removal");
        let original = turn(&session_id, CanonicalTurnStatus::Running);
        let projection = SessionConversationProjection::default()
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: original.clone(),
                }],
            )
            .expect("original should append");
        let mut changed = original.clone();
        changed.items.pop();
        projection
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(original),
                    next: changed,
                }],
            )
            .expect_err("item removal must fail");
    }

    #[test]
    fn immutable_item_identity_tampering_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-tamper");
        let original = turn(&session_id, CanonicalTurnStatus::Running);
        let projection = SessionConversationProjection::default()
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: original.clone(),
                }],
            )
            .expect("original should append");
        let mut changed = original.clone();
        changed.items[0].item_seq = 9;
        projection
            .append_transaction(
                temp.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(original),
                    next: changed,
                }],
            )
            .expect_err("immutable item identity must fail");
    }
}

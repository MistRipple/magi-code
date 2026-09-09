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
};

const EVENT_TRANSACTION_SCHEMA_VERSION: u32 = 1;

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

/// canonical 对话事实的重放结果。projection 文件只缓存该结果；events 目录才是权威源。
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionConversationProjection {
    session_id: Option<SessionId>,
    last_event_seq: u64,
    canonical_turns: Vec<CanonicalTurn>,
    accepted_submissions: Vec<AcceptedSubmissionRecord>,
}

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
        let transaction: CanonicalEventTransaction = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| {
                DaemonError::internal(format!(
                    "解析 canonical event transaction 失败 {}: {error}",
                    path.display()
                ))
            })?;
        Ok(transaction.session_id)
    }

    pub(crate) fn load(event_root: &Path, session_id: &SessionId) -> Result<Self, DaemonError> {
        let mut projection = Self {
            session_id: Some(session_id.clone()),
            ..Self::default()
        };
        if !event_root.exists() {
            return Ok(projection);
        }

        let mut paths = fs::read_dir(event_root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
        paths.sort();

        let mut seen_event_ids = HashSet::new();
        for path in paths {
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
        }
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
            return Ok(next);
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
        fs::create_dir_all(event_root)?;
        let path = event_root.join(transaction_file_name(first_event_seq, last_event_seq));
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
        }
        Ok(next)
    }

    pub(crate) fn last_event_seq(&self) -> u64 {
        self.last_event_seq
    }

    pub(crate) fn canonical_turns(&self) -> &[CanonicalTurn] {
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

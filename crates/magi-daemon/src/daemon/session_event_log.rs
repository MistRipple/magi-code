use super::config::DaemonError;
use super::persistence::AcceptedSubmissionRecord;
use magi_core::{SessionId, UtcMillis};
use magi_session_store::{
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnEvent, CanonicalTurnEventKind,
    CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnMutation, CanonicalTurnStatus,
    SessionInput, SessionInputMutation, SessionInputState,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

mod history;
mod observations;
pub(super) use history::EventHistoryRead;
use history::HistorySegment;
pub(super) use observations::CommittedObservationPage;

const EVENT_TRANSACTION_SCHEMA_VERSION: u32 = 1;
const EVENT_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
/// 自上一个检查点以来累计这么多个事务后，写入新检查点并截断被覆盖的事务文件。
const CHECKPOINT_TRANSACTION_INTERVAL: usize = 128;
const CHECKPOINT_FILE_PREFIX: &str = "checkpoint-";

/// 事件目录内部的压缩形式：重放到 `last_event_seq`（总是某个事务末尾）后的完整结果。
/// 事件目录仍是唯一权威源，检查点只替代被它覆盖的那段事务。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalEventCheckpoint {
    schema_version: u32,
    session_id: SessionId,
    last_event_seq: u64,
    canonical_turns: Vec<CanonicalTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inputs: Vec<SessionInput>,
    accepted_submissions: Vec<AcceptedSubmissionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history_segments: Vec<HistorySegment>,
    /// 检查点覆盖范围内最后一条标题事件的标题；没有则不写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// 检查点覆盖范围内最后一条模型请求记录的完整值与摘要，供后续去重和检验，不存历史正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_model_request: Option<LatestModelRequestProjection>,
}

/// 投影和检查点里保留的最新一次模型请求正文与摘要，用于同一会话后续步骤去重。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LatestModelRequestProjection {
    system_prompt_sha256: String,
    system_prompt: String,
    header_sha256: String,
    header: magi_session_store::ModelRequestHeader,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<u64>,
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

impl CanonicalEventTransaction {
    fn matches_header(&self, session_id: &SessionId, first: u64) -> bool {
        self.schema_version == EVENT_TRANSACTION_SCHEMA_VERSION
            && &self.session_id == session_id
            && self.first_event_seq == first
            && !self.events.is_empty()
            && self.last_event_seq.checked_sub(first) == Some((self.events.len() - 1) as u64)
            && self.transaction_id == transaction_id(session_id, first, self.last_event_seq)
    }
}

fn event_identity_matches(event: &CanonicalTurnEvent, session_id: &SessionId, seq: u64) -> bool {
    event.schema_version == CANONICAL_TURN_SCHEMA_VERSION
        && &event.session_id == session_id
        && event.event_seq == seq
        && event.event_id == event_id(session_id, seq)
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
    inputs: Vec<SessionInput>,
    accepted_submissions: Vec<AcceptedSubmissionRecord>,
    /// 已释放回合：只保留事件游标与 accepted 记录。回合事实在 SessionStore 里另有一份，
    /// 不活跃的会话没必要在这里再常驻一份完整副本；再次写入该会话前必须从事件目录重放。
    released: bool,
    /// 最近一次使用的单调序号，用于只让最近活跃的少数会话保持驻留。
    last_touched: u64,
    /// 最近检查点之后已提交的事务数，达到阈值时写入新检查点。
    transactions_since_checkpoint: usize,
    checkpoint_seq: u64,
    history_segments: Vec<HistorySegment>,
    /// 日志里最后一条 `SessionTitleChanged` 的标题。没有这类事件（从未改名或旧会话）
    /// 时为 `None`，会话记录的标题不被覆盖。释放回合不影响它。
    title: Option<String>,
    /// 日志里最后一条 `ModelRequestRecorded` 的最新值与摘要；仅保留一条供继续去重。
    latest_model_request: Option<LatestModelRequestProjection>,
}

static TOUCH_CLOCK: AtomicU64 = AtomicU64::new(1);

impl SessionConversationProjection {
    pub(super) fn open_history_page(
        &self,
        event_root: &Path,
        after: u64,
        through: Option<u64>,
    ) -> Result<EventHistoryRead, DaemonError> {
        let through = through.unwrap_or(self.last_event_seq);
        if after > through || through > self.last_event_seq {
            return Err(DaemonError::internal("会话历史游标超出已提交水位"));
        }
        let first_retained = self
            .history_segments
            .first()
            .map_or(self.checkpoint_seq.saturating_add(1), |segment| {
                segment.first_event_seq
            });
        let mut read = EventHistoryRead::new(after, through, first_retained);
        let Some(first) = read.next_sequence() else {
            return Ok(read);
        };
        // Only the ranges named by the committed checkpoint are visible. Open
        // the selected source while the caller holds the repository write lock.
        if first <= self.checkpoint_seq {
            let segment = self
                .history_segments
                .iter()
                .find(|segment| segment.contains(first))
                .ok_or_else(|| DaemonError::internal("找不到已提交的历史封存范围"))?;
            read.open(*segment, &segment.path(event_root), true)?;
        } else {
            let (_, transactions) = event_file_paths(event_root)?;
            let (segment, path) = transactions
                .into_iter()
                .find(|(segment, _)| segment.contains(first))
                .ok_or_else(|| DaemonError::internal("找不到已提交的历史事务"))?;
            read.open(segment, &path, false)?;
        }
        Ok(read)
    }

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
            history::validate_index(&checkpoint.history_segments, checkpoint_seq)?;
            projection.checkpoint_seq = checkpoint_seq;
            projection.history_segments = checkpoint.history_segments;
            projection.last_event_seq = checkpoint.last_event_seq;
            projection.canonical_turns = checkpoint.canonical_turns;
            projection.accepted_submissions = checkpoint.accepted_submissions;
            projection.title = checkpoint.title;
            if let Some(latest) = checkpoint.latest_model_request {
                if magi_session_store::sha256_hex_bytes(latest.system_prompt.as_bytes())
                    != latest.system_prompt_sha256
                    || latest.header.sha256_hex() != latest.header_sha256
                    || latest.header.config.provider.trim().is_empty()
                    || latest.header.config.model.trim().is_empty()
                {
                    return Err(DaemonError::internal(format!(
                        "canonical event checkpoint 的 latestModelRequest 摘要不合法: {}",
                        path.display()
                    )));
                }
                projection.latest_model_request = Some(latest);
            }
            for input in checkpoint.inputs {
                input
                    .validate_snapshot()
                    .map_err(|error| DaemonError::internal(error.to_string()))?;
                if input.session_id != *session_id
                    || projection.inputs.iter().any(|existing| {
                        existing.input_id == input.input_id
                            || existing.request_id == input.request_id
                    })
                {
                    return Err(DaemonError::internal("checkpoint 输入归属或身份冲突"));
                }
                validate_input_binding(&input, &projection.canonical_turns)?;
                projection.inputs.push(input);
            }
        }

        let mut seen_event_ids = HashSet::new();
        for (range, path) in transaction_paths {
            // 截断中途崩溃会留下已被检查点覆盖的事务；它们的事实已在检查点内，直接跳过。
            if range.last_event_seq <= checkpoint_seq {
                continue;
            }
            let transaction = read_transaction(&path)?;
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
    #[cfg(test)]
    fn append_transaction(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
    ) -> Result<Self, DaemonError> {
        self.append_session_transaction(event_root, session_id, mutations, &[], None)
            .map(|(projection, _)| projection)
    }

    pub(crate) fn append_session_transaction(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        mutations: &[CanonicalTurnMutation],
        inputs: &[SessionInputMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
    ) -> Result<(Self, Vec<CanonicalTurnEvent>), DaemonError> {
        let (next, transaction) = self
            .plan_transaction_inner(event_root, session_id, mutations, inputs, acceptance, false)?;
        let Some(transaction) = transaction else {
            return Ok((next, Vec::new()));
        };
        next.commit_transaction(event_root, session_id, transaction)
    }

    /// 会话标题变更作为单事件事务提交。标题事件不属于任何 Turn，不需要回合常驻，
    /// 所以已释放回合的投影也可以直接追加（释放状态原样保留）。
    pub(crate) fn append_session_title(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        title: &str,
    ) -> Result<(Self, CanonicalTurnEvent), DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入标题前必须先从事件目录重放: {session_id}"
            )));
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|stored| stored != session_id)
        {
            return Err(DaemonError::internal(format!(
                "canonical projection session 不匹配: {session_id}"
            )));
        }
        let mut next = self.clone();
        next.session_id = Some(session_id.clone());
        let seq = next.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(session_id, seq),
            event_seq: seq,
            kind: CanonicalTurnEventKind::SessionTitleChanged,
            session_id: session_id.clone(),
            turn_id: String::new(),
            turn_seq: 0,
            occurred_at: UtcMillis::now(),
            turn: None,
            item: None,
            input: None,
            model_step: None,
            title: Some(title.to_string()),
            attempt_stream: None,
            model_request: None,
            command_event: None,
        };
        next.apply_event(event.clone())?;
        let transaction = CanonicalEventTransaction {
            schema_version: EVENT_TRANSACTION_SCHEMA_VERSION,
            transaction_id: transaction_id(session_id, seq, seq),
            session_id: session_id.clone(),
            first_event_seq: seq,
            last_event_seq: seq,
            events: vec![event.clone()],
            acceptance: None,
        };
        let (next, _) = next.commit_transaction(event_root, session_id, transaction)?;
        Ok((next, event))
    }

    /// 会话级命令生命周期事件（`command/run`、`command/done`）作为单事件事务提交。
    /// 与标题事件一样不属于任何 Turn（`turn_id` 为空、`turn_seq` 为 0），提交前要求投影已常驻以便安全触发检查点。
    pub(crate) fn append_session_command_event(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        command_event: magi_session_store::SessionCommandEvent,
    ) -> Result<(Self, CanonicalTurnEvent), DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入命令事件前必须先从事件目录重放: {session_id}"
            )));
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|stored| stored != session_id)
        {
            return Err(DaemonError::internal(format!(
                "canonical projection session 不匹配: {session_id}"
            )));
        }
        let mut next = self.clone();
        next.session_id = Some(session_id.clone());
        let seq = next.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(session_id, seq),
            event_seq: seq,
            kind: CanonicalTurnEventKind::CommandEventRecorded,
            session_id: session_id.clone(),
            turn_id: String::new(),
            turn_seq: 0,
            occurred_at: UtcMillis::now(),
            turn: None,
            item: None,
            input: None,
            model_step: None,
            title: None,
            attempt_stream: None,
            model_request: None,
            command_event: Some(command_event),
        };
        next.apply_event(event.clone())?;
        let transaction = CanonicalEventTransaction {
            schema_version: EVENT_TRANSACTION_SCHEMA_VERSION,
            transaction_id: transaction_id(session_id, seq, seq),
            session_id: session_id.clone(),
            first_event_seq: seq,
            last_event_seq: seq,
            events: vec![event.clone()],
            acceptance: None,
        };
        let (next, _) = next.commit_transaction(event_root, session_id, transaction)?;
        Ok((next, event))
    }

    /// 一次模型调用尝试的流记录作为单事件事务提交。事件带回合身份并按它校验，所以要求回合常驻。
    pub(crate) fn append_model_attempt_stream(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        turn_id: &str,
        turn_seq: u64,
        stream: magi_session_store::ModelAttemptStream,
    ) -> Result<(Self, CanonicalTurnEvent), DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入流记录前必须先从事件目录重放: {session_id}"
            )));
        }
        let mut next = self.clone();
        next.session_id = Some(session_id.clone());
        let seq = next.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(session_id, seq),
            event_seq: seq,
            kind: CanonicalTurnEventKind::ModelAttemptStreamRecorded,
            session_id: session_id.clone(),
            turn_id: turn_id.to_string(),
            turn_seq,
            occurred_at: UtcMillis::now(),
            turn: None,
            item: None,
            input: None,
            model_step: None,
            title: None,
            attempt_stream: Some(stream),
            model_request: None,
            command_event: None,
        };
        next.apply_event(event.clone())?;
        let transaction = CanonicalEventTransaction {
            schema_version: EVENT_TRANSACTION_SCHEMA_VERSION,
            transaction_id: transaction_id(session_id, seq, seq),
            session_id: session_id.clone(),
            first_event_seq: seq,
            last_event_seq: seq,
            events: vec![event.clone()],
            acceptance: None,
        };
        let (next, _) = next.commit_transaction(event_root, session_id, transaction)?;
        Ok((next, event))
    }

    /// 一次模型步骤首次提交的请求快照（系统提示与请求头），与上一条按内容摘要去重后作为单事件事务落盘。
    /// 序列化后超过 `MAX_MODEL_REQUEST_RECORD_BYTES`（4 MiB）时放弃写入并告警，返回 `Ok(None)`，不截断。
    pub(crate) fn append_model_request(
        &self,
        event_root: &Path,
        session_id: &SessionId,
        turn_id: &str,
        turn_seq: u64,
        snapshot: magi_session_store::ModelRequestSnapshot,
    ) -> Result<Option<(Self, CanonicalTurnEvent)>, DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入请求记录前必须先从事件目录重放: {session_id}"
            )));
        }
        let system_prompt_sha256 = snapshot.system_prompt_sha256();
        let header_sha256 = snapshot.header_sha256();
        let same_system = self
            .latest_model_request
            .as_ref()
            .is_some_and(|prev| prev.system_prompt_sha256 == system_prompt_sha256);
        let same_header = self
            .latest_model_request
            .as_ref()
            .is_some_and(|prev| prev.header_sha256 == header_sha256);
        let record = magi_session_store::ModelRequestRecord {
            step_id: snapshot.step_id,
            system_prompt_sha256,
            system_prompt: (!same_system).then_some(snapshot.system_prompt),
            header_sha256,
            header: (!same_header).then_some(snapshot.header),
            context_window: snapshot.context_window,
            redacted: snapshot.redacted,
        };
        let payload_size = serde_json::to_vec(&record)?.len();
        if payload_size > magi_session_store::MAX_MODEL_REQUEST_RECORD_BYTES {
            tracing::warn!(
                %session_id,
                turn_id,
                step_id = %record.step_id,
                bytes = payload_size,
                limit = magi_session_store::MAX_MODEL_REQUEST_RECORD_BYTES,
                "模型请求记录超过体积上限，已放弃写入"
            );
            return Ok(None);
        }
        let mut next = self.clone();
        next.session_id = Some(session_id.clone());
        let seq = next.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(session_id, seq),
            event_seq: seq,
            kind: CanonicalTurnEventKind::ModelRequestRecorded,
            session_id: session_id.clone(),
            turn_id: turn_id.to_string(),
            turn_seq,
            occurred_at: UtcMillis::now(),
            turn: None,
            item: None,
            input: None,
            model_step: None,
            title: None,
            attempt_stream: None,
            model_request: Some(record),
            command_event: None,
        };
        next.apply_event(event.clone())?;
        let transaction = CanonicalEventTransaction {
            schema_version: EVENT_TRANSACTION_SCHEMA_VERSION,
            transaction_id: transaction_id(session_id, seq, seq),
            session_id: session_id.clone(),
            first_event_seq: seq,
            last_event_seq: seq,
            events: vec![event.clone()],
            acceptance: None,
        };
        let (next, _) = next.commit_transaction(event_root, session_id, transaction)?;
        Ok(Some((next, event)))
    }

    /// 把已在 `self` 上重放过的事务写成事件文件，必要时写检查点。
    fn commit_transaction(
        self,
        event_root: &Path,
        session_id: &SessionId,
        transaction: CanonicalEventTransaction,
    ) -> Result<(Self, Vec<CanonicalTurnEvent>), DaemonError> {
        let mut next = self;
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
        Ok((next, transaction.events))
    }

    /// 先封存新增事务，再以检查点一次提交回放水位与封存范围。
    ///
    /// 检查点先于删除落盘，任意时刻崩溃都不丢事实；删除失败的遗留文件在加载时被
    /// 跳过，并在下一次检查点时清理。
    fn write_checkpoint(
        &mut self,
        event_root: &Path,
        session_id: &SessionId,
    ) -> Result<(), DaemonError> {
        let (checkpoint_paths, transaction_paths) = event_file_paths(event_root)?;
        let segment = HistorySegment::new(self.checkpoint_seq + 1, self.last_event_seq)?;
        segment.archive(event_root, &transaction_paths)?;
        let mut history_segments = self.history_segments.clone();
        history_segments.push(segment);
        let checkpoint = CanonicalEventCheckpoint {
            schema_version: EVENT_CHECKPOINT_SCHEMA_VERSION,
            session_id: session_id.clone(),
            last_event_seq: self.last_event_seq,
            canonical_turns: self.canonical_turns.clone(),
            inputs: self.inputs.clone(),
            accepted_submissions: self.accepted_submissions.clone(),
            history_segments,
            title: self.title.clone(),
            latest_model_request: self.latest_model_request.clone(),
        };
        magi_core::fs_atomic::write_atomic(
            &event_root.join(checkpoint_file_name(self.last_event_seq)),
            serde_json::to_vec(&checkpoint).map_err(DaemonError::from)?,
        )?;
        self.checkpoint_seq = self.last_event_seq;
        self.history_segments = checkpoint.history_segments;
        segment.remove_failed_attempts(event_root);
        let covered = transaction_paths
            .into_iter()
            .filter(|(range, _)| range.last_event_seq <= self.last_event_seq)
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
        inputs: &[SessionInputMutation],
    ) -> Result<PreparedCanonicalEventWrite, DaemonError> {
        let (projection, transaction) =
            self.plan_transaction_inner(event_root, session_id, mutations, inputs, None, true)?;
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
        inputs: &[SessionInputMutation],
        acceptance: Option<AcceptedSubmissionRecord>,
        importing: bool,
    ) -> Result<(Self, Option<CanonicalEventTransaction>), DaemonError> {
        if self.released {
            return Err(DaemonError::internal(format!(
                "canonical projection 的回合已释放，写入前必须先从事件目录重放: {session_id}"
            )));
        }
        if mutations.is_empty() && inputs.is_empty() {
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
        for mutation in inputs {
            let previous = next
                .inputs
                .iter()
                .find(|input| input.input_id == mutation.next.input_id)
                .cloned();
            if previous != mutation.previous {
                return Err(DaemonError::internal("Input 事务前置状态冲突"));
            }
            let seq = next.last_event_seq.saturating_add(1);
            let event = CanonicalTurnEvent {
                schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
                event_id: event_id(session_id, seq),
                event_seq: seq,
                kind: if importing {
                    CanonicalTurnEventKind::InputImported
                } else {
                    CanonicalTurnEventKind::InputUpdated
                },
                session_id: session_id.clone(),
                turn_id: String::new(),
                turn_seq: 0,
                occurred_at: mutation.next.accepted_at,
                turn: None,
                item: None,
                input: Some(mutation.next.clone()),
                model_step: None,
                title: None,
                attempt_stream: None,
                model_request: None,
                command_event: None,
            };
            next.apply_event(event.clone())?;
            events.push(event);
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

    pub(crate) fn inputs(&self) -> &[SessionInput] {
        &self.inputs
    }

    pub(crate) fn last_event_seq(&self) -> u64 {
        self.last_event_seq
    }

    /// 日志里最后一次标题变更的标题；没有标题事件时为 `None`。
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// 日志里最后一次模型请求记录的摘要与路由，用于在 daemon 重启后给在线观察播种，
    /// 使在线增量推导与全量历史重放保持一致。
    pub(crate) fn latest_model_request_state(
        &self,
    ) -> Option<crate::native_plugin_platform::session_observation::LatestModelRequestState> {
        self.latest_model_request.as_ref().map(|latest| {
            crate::native_plugin_platform::session_observation::LatestModelRequestState {
                header_sha256: latest.header_sha256.clone(),
                system_prompt_sha256: latest.system_prompt_sha256.clone(),
                provider: latest.header.config.provider.clone(),
                model: latest.header.config.model.clone(),
                context_window: latest.context_window,
            }
        })
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
                started.model_steps.clear();
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
                for step in &next_turn.model_steps {
                    self.push_planned_step_event(session_id, &next_turn, step, events)?;
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
                for step in &next_turn.model_steps {
                    if previous
                        .model_steps
                        .iter()
                        .find(|old| old.step_id == step.step_id)
                        != Some(step)
                    {
                        self.push_planned_step_event(session_id, &next_turn, step, events)?;
                    }
                }
                if turn_header(&previous) != turn_header(&next_turn) {
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

    fn push_planned_step_event(
        &mut self,
        session_id: &SessionId,
        turn: &CanonicalTurn,
        step: &magi_session_store::ModelStep,
        events: &mut Vec<CanonicalTurnEvent>,
    ) -> Result<(), DaemonError> {
        let seq = self.last_event_seq.saturating_add(1);
        let event = CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(session_id, seq),
            event_seq: seq,
            kind: CanonicalTurnEventKind::ModelStepUpdated,
            session_id: session_id.clone(),
            turn_id: turn.turn_id.clone(),
            turn_seq: turn.turn_seq,
            occurred_at: step.settled_at.unwrap_or(step.started_at),
            turn: None,
            item: None,
            input: None,
            model_step: Some(step.clone()),
            title: None,
            attempt_stream: None,
            model_request: None,
            command_event: None,
        };
        self.apply_event(event.clone())?;
        events.push(event);
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
            input: None,
            model_step: None,
            title: None,
            attempt_stream: None,
            model_request: None,
            command_event: None,
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
        let valid = transaction.matches_header(session_id, expected_first)
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
                .current_turn_id
                .as_deref()
                .is_none_or(|turn_id| turn_id != acceptance.session.canonical_turn.turn_id)
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
        if !event_identity_matches(event, session_id, expected_seq)
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
        if !matches!(
            event.kind,
            CanonicalTurnEventKind::InputUpdated | CanonicalTurnEventKind::InputImported
        ) && event.input.is_some()
        {
            return Err(DaemonError::internal("Turn 事件不能携带 Input"));
        }
        if event.kind != CanonicalTurnEventKind::ModelStepUpdated && event.model_step.is_some() {
            return Err(DaemonError::internal("非 Step 事件不能携带 modelStep"));
        }
        if event.kind != CanonicalTurnEventKind::SessionTitleChanged && event.title.is_some() {
            return Err(DaemonError::internal("非标题事件不能携带 title"));
        }
        if event.kind != CanonicalTurnEventKind::ModelAttemptStreamRecorded
            && event.attempt_stream.is_some()
        {
            return Err(DaemonError::internal("非流记录事件不能携带 attemptStream"));
        }
        if event.kind != CanonicalTurnEventKind::ModelRequestRecorded
            && event.model_request.is_some()
        {
            return Err(DaemonError::internal("非请求记录事件不能携带 modelRequest"));
        }
        if event.kind != CanonicalTurnEventKind::CommandEventRecorded
            && event.command_event.is_some()
        {
            return Err(DaemonError::internal("非命令事件不能携带 commandEvent"));
        }
        match event.kind {
            CanonicalTurnEventKind::CommandEventRecorded => {
                let recorded = event
                    .command_event
                    .as_ref()
                    .ok_or_else(|| DaemonError::internal("命令事件缺少 commandEvent"))?;
                if event.turn.is_some()
                    || event.item.is_some()
                    || event.input.is_some()
                    || event.model_step.is_some()
                    || event.title.is_some()
                    || event.attempt_stream.is_some()
                    || event.model_request.is_some()
                    || !event.turn_id.is_empty()
                    || event.turn_seq != 0
                {
                    return Err(DaemonError::internal("命令事件身份无效"));
                }
                match recorded {
                    magi_session_store::SessionCommandEvent::Run {
                        command_id, name, ..
                    } => {
                        if command_id.trim().is_empty() || name.trim().is_empty() {
                            return Err(DaemonError::internal(
                                "command/run 缺少 commandId 或 name",
                            ));
                        }
                    }
                    magi_session_store::SessionCommandEvent::Done {
                        command_id,
                        outcome,
                        text,
                        source_event_seq,
                    } => {
                        if command_id.trim().is_empty() {
                            return Err(DaemonError::internal("command/done 缺少 commandId"));
                        }
                        if *outcome == magi_session_store::SessionCommandOutcome::Error {
                            if text.as_deref().is_none_or(|t| t.trim().is_empty()) {
                                return Err(DaemonError::internal(
                                    "command/done 错误结果缺少非空 text",
                                ));
                            }
                            if source_event_seq.is_some() {
                                return Err(DaemonError::internal(
                                    "command/done 错误结果不能携带 sourceEventSeq",
                                ));
                            }
                        }
                    }
                }
            }
            CanonicalTurnEventKind::ModelRequestRecorded => {
                let recorded = event
                    .model_request
                    .as_ref()
                    .ok_or_else(|| DaemonError::internal("请求记录事件缺少 modelRequest"))?;
                if event.turn.is_some()
                    || event.item.is_some()
                    || event.input.is_some()
                    || event.model_step.is_some()
                    || event.title.is_some()
                    || event.attempt_stream.is_some()
                    || recorded.step_id.trim().is_empty()
                {
                    return Err(DaemonError::internal("请求记录事件身份无效"));
                }
                let turn = self
                    .canonical_turns
                    .iter()
                    .find(|turn| turn.turn_id == event.turn_id)
                    .ok_or_else(|| DaemonError::internal("请求记录事件引用未知 Turn"))?;
                if turn.turn_seq != event.turn_seq {
                    return Err(DaemonError::internal("请求记录事件引用身份不同的 Turn"));
                }
                if !turn
                    .model_steps
                    .iter()
                    .any(|step| step.step_id == recorded.step_id)
                {
                    return Err(DaemonError::internal("请求记录事件引用未知的 Step"));
                }
                let resolved_system = match recorded.system_prompt.as_ref() {
                    Some(text) => {
                        if magi_session_store::sha256_hex_bytes(text.as_bytes())
                            != recorded.system_prompt_sha256
                        {
                            return Err(DaemonError::internal("请求记录系统提示摘要不匹配"));
                        }
                        text.clone()
                    }
                    None => {
                        let prev = self.latest_model_request.as_ref().ok_or_else(|| {
                            DaemonError::internal("请求记录去重省略系统提示但缺少前序记录")
                        })?;
                        if prev.system_prompt_sha256 != recorded.system_prompt_sha256 {
                            return Err(DaemonError::internal("请求记录去重系统提示摘要不匹配"));
                        }
                        prev.system_prompt.clone()
                    }
                };
                let resolved_header = match recorded.header.as_ref() {
                    Some(header) => {
                        if header.config.provider.trim().is_empty()
                            || header.config.model.trim().is_empty()
                        {
                            return Err(DaemonError::internal("请求头缺少 provider 或 model"));
                        }
                        if header.sha256_hex() != recorded.header_sha256 {
                            return Err(DaemonError::internal("请求记录请求头摘要不匹配"));
                        }
                        header.clone()
                    }
                    None => {
                        let prev = self.latest_model_request.as_ref().ok_or_else(|| {
                            DaemonError::internal("请求记录去重省略请求头但缺少前序记录")
                        })?;
                        if prev.header_sha256 != recorded.header_sha256 {
                            return Err(DaemonError::internal("请求记录去重请求头摘要不匹配"));
                        }
                        prev.header.clone()
                    }
                };
                self.latest_model_request = Some(LatestModelRequestProjection {
                    system_prompt_sha256: recorded.system_prompt_sha256.clone(),
                    system_prompt: resolved_system,
                    header_sha256: recorded.header_sha256.clone(),
                    header: resolved_header,
                    context_window: recorded.context_window,
                });
            }
            CanonicalTurnEventKind::ModelAttemptStreamRecorded => {
                // 流记录不改变内存投影，只验证它指向一次真实存在的尝试：同一回合身份、
                // 存在该步骤与尝试，且不携带其他事实。
                let recorded = event
                    .attempt_stream
                    .as_ref()
                    .ok_or_else(|| DaemonError::internal("流记录事件缺少 attemptStream"))?;
                if event.turn.is_some()
                    || event.item.is_some()
                    || event.input.is_some()
                    || event.model_step.is_some()
                    || event.title.is_some()
                    || recorded.stream.is_empty()
                {
                    return Err(DaemonError::internal("流记录事件身份无效"));
                }
                let turn = self
                    .canonical_turns
                    .iter()
                    .find(|turn| turn.turn_id == event.turn_id)
                    .ok_or_else(|| DaemonError::internal("流记录事件引用未知 Turn"))?;
                if turn.turn_seq != event.turn_seq {
                    return Err(DaemonError::internal("流记录事件引用身份不同的 Turn"));
                }
                let known = turn
                    .model_steps
                    .iter()
                    .find(|step| step.step_id == recorded.step_id)
                    .is_some_and(|step| {
                        step.attempts
                            .iter()
                            .any(|attempt| attempt.attempt_id == recorded.attempt_id)
                    });
                if !known {
                    return Err(DaemonError::internal("流记录事件引用未知的 Step 尝试"));
                }
            }
            CanonicalTurnEventKind::SessionTitleChanged => {
                let title = event
                    .title
                    .as_deref()
                    .filter(|title| !title.trim().is_empty())
                    .ok_or_else(|| DaemonError::internal("标题事件缺少 title"))?;
                if event.turn.is_some()
                    || event.item.is_some()
                    || event.input.is_some()
                    || event.model_step.is_some()
                    || !event.turn_id.is_empty()
                    || event.turn_seq != 0
                {
                    return Err(DaemonError::internal("标题事件身份无效"));
                }
                self.title = Some(title.to_string());
            }
            CanonicalTurnEventKind::ModelStepUpdated => {
                let step = event
                    .model_step
                    .ok_or_else(|| DaemonError::internal("Step 事件缺少 modelStep"))?;
                step.validate_snapshot()
                    .map_err(|error| DaemonError::internal(error.to_string()))?;
                if event.turn.is_some() || event.item.is_some() || event.input.is_some() {
                    return Err(DaemonError::internal("Step 事件不能携带其他事实"));
                }
                let turn = self
                    .canonical_turns
                    .iter_mut()
                    .find(|turn| turn.turn_id == event.turn_id)
                    .ok_or_else(|| DaemonError::internal("Step 事件引用未知 Turn"))?;
                if turn.turn_seq != event.turn_seq {
                    return Err(DaemonError::internal("Step 事件引用身份不同的 Turn"));
                }
                let terminal = turn.status.is_terminal() || turn.completed_at.is_some();
                match turn
                    .model_steps
                    .iter_mut()
                    .find(|old| old.step_id == step.step_id)
                {
                    Some(old) => {
                        if terminal {
                            step.validate_terminal_settlement_from(old)
                                .map_err(|error| DaemonError::internal(error.to_string()))?;
                        }
                        step.validate_update_from(old)
                            .map_err(|error| DaemonError::internal(error.to_string()))?;
                        *old = step;
                    }
                    None if terminal => {
                        return Err(DaemonError::internal("终态 Turn 不能接纳新 Step"));
                    }
                    None => turn.model_steps.push(step),
                }
            }

            CanonicalTurnEventKind::InputUpdated | CanonicalTurnEventKind::InputImported => {
                let input = event
                    .input
                    .clone()
                    .ok_or_else(|| DaemonError::internal("Input 事件缺少输入"))?;
                if event.turn.is_some()
                    || event.item.is_some()
                    || !event.turn_id.is_empty()
                    || event.turn_seq != 0
                    || input.session_id != event.session_id
                {
                    return Err(DaemonError::internal("Input 事件身份无效"));
                }
                let previous = self
                    .inputs
                    .iter()
                    .find(|existing| existing.input_id == input.input_id);
                if event.kind == CanonicalTurnEventKind::InputImported {
                    if previous.is_some() {
                        return Err(DaemonError::internal("不能导入已存在的输入"));
                    }
                    input
                        .validate_snapshot()
                        .map_err(|error| DaemonError::internal(error.to_string()))?;
                } else {
                    input
                        .validate_update_from(previous)
                        .map_err(|error| DaemonError::internal(error.to_string()))?;
                }
                if self.inputs.iter().any(|existing| {
                    existing.input_id != input.input_id && existing.request_id == input.request_id
                }) {
                    return Err(DaemonError::internal("Input requestId 重复"));
                }
                validate_input_binding(&input, &self.canonical_turns)?;
                match self
                    .inputs
                    .iter_mut()
                    .find(|existing| existing.input_id == input.input_id)
                {
                    Some(existing) => *existing = input,
                    None => self.inputs.push(input),
                }
            }
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

fn validate_input_binding(
    input: &SessionInput,
    turns: &[CanonicalTurn],
) -> Result<(), DaemonError> {
    if matches!(
        input.state,
        SessionInputState::Attached | SessionInputState::Consumed
    ) && !turns.iter().any(|turn| {
        Some(&turn.turn_id) == input.attached_turn_id.as_ref()
            && turn.items.iter().any(|item| {
                item.kind == CanonicalTurnItemKind::UserMessage
                    && item
                        .metadata
                        .get("requestId")
                        .and_then(|value| value.as_str())
                        == Some(input.request_id.as_str())
                    && item
                        .metadata
                        .get("userMessageId")
                        .and_then(|value| value.as_str())
                        == Some(input.user_message_id.as_str())
            })
    }) {
        return Err(DaemonError::internal("Input 绑定缺少用户 item"));
    }
    Ok(())
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
    turn.validate_model_steps()
        .map_err(|error| DaemonError::internal(error.to_string()))?;
    Ok(turn)
}

fn validate_turn(turn: &CanonicalTurn, session_id: &SessionId) -> Result<(), DaemonError> {
    if &turn.session_id != session_id || turn.turn_id.trim().is_empty() {
        return Err(DaemonError::internal(
            "canonical turn session 或 turnId 不合法".to_string(),
        ));
    }
    turn.validate_model_steps()
        .map_err(|error| DaemonError::internal(error.to_string()))?;
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
    item.background_job()
        .map_err(|error| DaemonError::internal(error.to_string()))?;
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

fn turn_header(turn: &CanonicalTurn) -> CanonicalTurn {
    let mut header = turn_shell(turn);
    header.model_steps.clear();
    header
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

fn read_transaction(path: &Path) -> Result<CanonicalEventTransaction, DaemonError> {
    serde_json::from_reader(std::io::BufReader::new(fs::File::open(path)?)).map_err(|error| {
        DaemonError::internal(format!(
            "解析 canonical event transaction 失败 {}: {error}",
            path.display()
        ))
    })
}

type EventFilePaths = (Vec<PathBuf>, Vec<(HistorySegment, PathBuf)>);

/// 列出事件目录中的检查点（按序号排序）与事务（按首序号排序）。
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
        let (first, last) = stem
            .split_once('-')
            .filter(|(first, last)| first.len() == 20 && last.len() == 20)
            .and_then(|(first, last)| Some((first.parse::<u64>().ok()?, last.parse::<u64>().ok()?)))
            .ok_or_else(|| {
                DaemonError::internal(format!(
                    "canonical event transaction 文件名不合法: {}",
                    path.display()
                ))
            })?;
        transactions.push((HistorySegment::new(first, last)?, path));
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

    #[test]
    fn input_binding_replays_atomically_and_survives_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let session = SessionId::new("session-input-atomic");
        let pending = SessionInput {
            input_id: "input-atomic".into(),
            session_id: session.clone(),
            request_id: "request-atomic".into(),
            user_message_id: "user-1".into(),
            request_fingerprint: "fingerprint-atomic".into(),
            accepted_at: UtcMillis(10),
            revision: 1,
            state: SessionInputState::Pending,
            attached_turn_id: None,
            consumption: None,
            interruption: None,
            payload: serde_json::json!({"text":"hello"}),
        };
        let empty = SessionConversationProjection::load(root.path(), &session).unwrap();
        let queued = empty
            .append_session_transaction(
                root.path(),
                &session,
                &[],
                &[SessionInputMutation {
                    previous: None,
                    next: pending.clone(),
                }],
                None,
            )
            .unwrap()
            .0;
        assert_eq!(
            SessionConversationProjection::load(root.path(), &session)
                .unwrap()
                .inputs(),
            std::slice::from_ref(&pending)
        );
        let mut accepted = turn(&session, CanonicalTurnStatus::Pending);
        accepted.items[0]
            .metadata
            .insert("requestId".into(), serde_json::json!(pending.request_id));
        accepted.items[0].metadata.insert(
            "userMessageId".into(),
            serde_json::json!(pending.user_message_id),
        );
        let mut bound = pending.clone();
        bound.revision = 2;
        bound.state = SessionInputState::Attached;
        bound.attached_turn_id = Some(accepted.turn_id.clone());
        let turn_mutation = CanonicalTurnMutation {
            previous: None,
            next: accepted.clone(),
        };
        let input_mutation = SessionInputMutation {
            previous: Some(pending),
            next: bound.clone(),
        };
        // Turn 与 Input 的任意前置条件失败都不能留下半个事务。
        let mut invalid = input_mutation.clone();
        invalid.next.revision = 99;
        assert!(
            queued
                .append_session_transaction(
                    root.path(),
                    &session,
                    std::slice::from_ref(&turn_mutation),
                    &[invalid],
                    None
                )
                .is_err()
        );
        let before = SessionConversationProjection::load(root.path(), &session).unwrap();
        assert!(before.canonical_turns().is_empty());
        assert_eq!(before.inputs()[0].state, SessionInputState::Pending);
        let (mut committed, events) = queued
            .append_session_transaction(
                root.path(),
                &session,
                &[turn_mutation],
                &[input_mutation],
                None,
            )
            .unwrap();
        let user = events
            .iter()
            .find(|event| {
                event
                    .item
                    .as_ref()
                    .is_some_and(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            })
            .expect("committed user input must have a persistent event identity");
        assert_eq!(user.event_seq, queued.last_event_seq() + 2);
        assert_eq!(events.last().unwrap().event_seq, committed.last_event_seq());
        // 模拟 event 已落盘，内存/其他 projection 尚未更新便重启。
        let replayed = SessionConversationProjection::load(root.path(), &session).unwrap();
        assert_eq!(replayed.canonical_turns(), std::slice::from_ref(&accepted));
        assert_eq!(replayed.inputs(), std::slice::from_ref(&bound));
        committed.write_checkpoint(root.path(), &session).unwrap();
        assert_eq!(
            SessionConversationProjection::load(root.path(), &session)
                .unwrap()
                .inputs(),
            &[bound]
        );

        let mut completed = accepted.clone();
        completed.status = CanonicalTurnStatus::Completed;
        completed.completed_at = Some(UtcMillis(20));
        let advanced = committed
            .append_transaction(
                root.path(),
                &session,
                &[CanonicalTurnMutation {
                    previous: Some(accepted),
                    next: completed,
                }],
            )
            .unwrap();
        let open = |after, through| {
            SessionConversationProjection::load(root.path(), &session)?.open_history_page(
                root.path(),
                after,
                through,
            )
        };
        let page = |after, through, limit| {
            serde_json::to_value(
                CommittedObservationPage::read_for_scope(
                    &session,
                    after,
                    through,
                    limit,
                    4096,
                    crate::native_plugin_platform::session_observation::ObservationScope::Main,
                    open,
                )
                .unwrap(),
            )
            .unwrap()
        };
        let first = page(0, None, 1);
        let through = advanced.last_event_seq();
        assert_eq!(first["through"], through);
        assert_eq!(first["nextAfter"], queued.last_event_seq() + 1);
        assert_eq!(first["events"][0]["seq"], 0);
        assert_eq!(first["events"][0]["observation"]["kind"], "turnStarted");
        let rest = page(first["nextAfter"].as_u64().unwrap(), Some(through), 20);
        assert_eq!(rest["events"].as_array().unwrap().len(), 2);
        assert_eq!(rest["events"][0]["seq"], 1);
        assert_eq!(rest["events"][0]["sourceSequence"], user.event_seq);
        assert_eq!(
            rest["events"][0]["observation"],
            serde_json::json!({"kind":"textInput","messageId":"user-1","text":"hello"})
        );
        assert_eq!(rest["events"][1]["seq"], 2);
        assert_eq!(rest["events"][1]["sourceSequence"], through);
        assert_eq!(rest["events"][1]["observation"]["kind"], "turnEnded");
        assert!(rest["nextAfter"].is_null());
    }

    #[test]
    fn history_pages_never_split_the_observations_of_one_committed_event() {
        let root = tempfile::tempdir().unwrap();
        let session = SessionId::new("session-step-pages");
        let main = "thread-main";
        let fixture = serde_json::json!({
            "sessionId": session, "turnId": "turn-1", "turnSeq": 1, "acceptedAt": 1,
            "status": "completed", "completedAt": 9,
            "items": [
                {"sessionId": session, "turnId": "turn-1", "turnSeq": 1, "itemId": "user-1",
                 "itemSeq": 0, "kind": "user_message", "createdAt": 1, "status": "completed",
                 "updatedAt": 1, "content": "hi", "sourceThreadId": main, "metadata": {}},
                {"sessionId": session, "turnId": "turn-1", "turnSeq": 1, "itemId": "assistant-1",
                 "itemSeq": 1, "kind": "assistant_text", "createdAt": 2, "status": "completed",
                 "updatedAt": 3, "content": "好", "sourceThreadId": main,
                 "metadata": {"modelStepId": "step-1"}},
            ],
            "modelSteps": [{
                "stepId": "step-1", "taskId": null, "threadId": main, "checkpointGeneration": 0,
                "requestFingerprint": "a".repeat(64), "provider": "p", "messageCount": 1,
                "toolCount": 0, "inputRequestIds": [], "startedAt": 2, "settledAt": null,
                "status": "started", "responseFingerprint": null,
                "attempts": [{"attemptId": "step-1:attempt:1", "startedAt": 2,
                              "settledAt": null, "status": "started", "transport": null}],
                "binding": {"model": "m", "provider": "p", "bindingRevision": 1,
                            "settingsRevision": 1, "usageContext": null},
                "maxOutputTokens": null, "goalId": null, "usageTokens": null,
                "goalTokensAfter": null, "executorCapabilities": null,
                "purpose": "execution", "promptFrame": null, "cacheEpoch": 0,
            }],
            "metadata": {},
        });
        let turn: CanonicalTurn = serde_json::from_value(fixture).unwrap();
        SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: turn,
                }],
            )
            .unwrap();
        let open = |after, through| {
            SessionConversationProjection::load(root.path(), &session)?.open_history_page(
                root.path(),
                after,
                through,
            )
        };
        let read = |after, through, limit| {
            serde_json::to_value(
                CommittedObservationPage::read_for_scope(
                    &session,
                    after,
                    through,
                    limit,
                    1 << 20,
                    crate::native_plugin_platform::session_observation::ObservationScope::Main,
                    open,
                )
                .unwrap(),
            )
            .unwrap()
        };
        let full = read(0, None, 1000);
        let kinds = |page: &serde_json::Value| -> Vec<String> {
            page["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["observation"]["kind"].as_str().unwrap().to_string())
                .collect()
        };
        let all = kinds(&full);
        assert!(all.contains(&"stepStarted".to_string()), "{all:?}");
        assert!(all.contains(&"stepEnded".to_string()), "{all:?}");
        // 助手消息与它收口的步骤来自同一个事件，共享 sourceSequence。
        let events = full["events"].as_array().unwrap();
        let message = events
            .iter()
            .position(|event| event["observation"]["kind"] == "assistantMessage")
            .unwrap();
        assert_eq!(events[message]["observation"]["step"], 1);
        assert_eq!(events[message + 1]["observation"]["kind"], "stepEnded");
        assert_eq!(
            events[message]["sourceSequence"],
            events[message + 1]["sourceSequence"]
        );
        // 任意页大小：逐页读完与一次读完一致，且没有事件的观察被拆到两页。
        for limit in 1..=4 {
            let through = full["through"].as_u64().unwrap();
            let mut after = 0;
            let mut collected = Vec::new();
            let mut pages = 0;
            loop {
                let page = read(after, Some(through), limit);
                let batch = page["events"].as_array().unwrap().clone();
                if let (Some(last), Some(next)) = (collected.last(), batch.first()) {
                    let last: &serde_json::Value = last;
                    assert!(
                        next["sourceSequence"].as_u64() > last["sourceSequence"].as_u64(),
                        "limit {limit}: 一个事件的观察被拆到两页"
                    );
                }
                collected.extend(batch);
                pages += 1;
                match page["nextAfter"].as_u64() {
                    Some(next) => after = next,
                    None => break,
                }
                assert!(pages < 50);
            }
            assert_eq!(collected, *events, "limit {limit}");
        }
    }

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
            model_steps: Vec::new(),
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
    fn background_job_facts_replay_after_the_origin_turn_is_terminal() {
        let root = tempfile::tempdir().unwrap();
        let session = SessionId::new("job-events");
        let mut running = turn(&session, CanonicalTurnStatus::Running);
        let mut tool_item = item(&session, "job-tool", 3, CanonicalTurnItemKind::ToolCall, "");
        tool_item.tool = Some(magi_session_store::CanonicalToolCall {
            call_id: "job-call".into(),
            name: "shell_exec".into(),
            arguments: Some(serde_json::json!({"command":"publish","background":true})),
            result: Some(
                serde_json::json!({"status":"succeeded","running":true,"job_id":"job-events-1"}),
            ),
            error: None,
        });
        tool_item.worker = Some(CanonicalWorkerRef {
            task_id: Some(magi_core::TaskId::new("job-task")),
            worker_id: None,
            role_id: None,
            title: None,
        });
        let mut job = magi_tool_runtime::BackgroundJob {
            job_id: "job-events-1".into(),
            session_id: session.clone(),
            workspace_id: None,
            task_id: Some(magi_core::TaskId::new("job-task")),
            turn_id: running.turn_id.clone(),
            item_id: tool_item.item_id.clone(),
            tool_call_id: "job-call".into(),
            revision: 1,
            command: "publish".into(),
            cwd: root.path().display().to_string(),
            status: magi_tool_runtime::BackgroundJobStatus::Starting,
            started_at: UtcMillis(10),
            settled_at: None,
            exit_code: None,
            stdout_path: root.path().join("stdout.log"),
            stderr_path: root.path().join("stderr.log"),
        };
        tool_item.metadata.insert(
            magi_tool_runtime::BACKGROUND_JOB_METADATA_KEY.into(),
            serde_json::to_value(&job).unwrap(),
        );
        running.items.push(tool_item);
        let mut projection = SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: running.clone(),
                }],
            )
            .unwrap();
        let mut terminal = running.clone();
        terminal.status = CanonicalTurnStatus::Completed;
        terminal.completed_at = Some(UtcMillis(30));
        projection = projection
            .append_transaction(
                root.path(),
                &session,
                &[CanonicalTurnMutation {
                    previous: Some(running),
                    next: terminal.clone(),
                }],
            )
            .unwrap();
        let mut settled = terminal.clone();
        job.revision += 1;
        job.status = magi_tool_runtime::BackgroundJobStatus::OutcomeUnknown;
        job.settled_at = Some(UtcMillis(31));
        settled.items.last_mut().unwrap().metadata.insert(
            magi_tool_runtime::BACKGROUND_JOB_METADATA_KEY.into(),
            serde_json::to_value(&job).unwrap(),
        );
        projection = projection
            .append_transaction(
                root.path(),
                &session,
                &[CanonicalTurnMutation {
                    previous: Some(terminal),
                    next: settled.clone(),
                }],
            )
            .unwrap();
        let restored = SessionConversationProjection::load(root.path(), &session).unwrap();
        assert_eq!(restored.canonical_turns(), &[settled]);
        assert_eq!(restored.canonical_turns(), projection.canonical_turns());
        assert_eq!(
            restored.canonical_turns()[0]
                .items
                .last()
                .unwrap()
                .background_job()
                .unwrap()
                .unwrap(),
            job
        );
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
    fn title_events_are_sequenced_session_facts_that_replay_and_survive_checkpoints() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-title-events");
        let mut projection = append_item_transactions(root.path(), &session_id, 2);
        let seq_before = projection.last_event_seq();
        assert_eq!(projection.title(), None);

        let (next, first) = projection
            .append_session_title(root.path(), &session_id, "第一个标题")
            .unwrap();
        assert_eq!(first.event_seq, seq_before + 1);
        assert_eq!(first.kind, CanonicalTurnEventKind::SessionTitleChanged);
        assert_eq!((first.turn_id.as_str(), first.turn_seq), ("", 0));
        projection = next;
        let (next, second) = projection
            .append_session_title(root.path(), &session_id, "第二个标题")
            .unwrap();
        assert_eq!(second.event_seq, seq_before + 2);
        projection = next;
        assert_eq!(projection.title(), Some("第二个标题"));

        // 重放得到同一个标题与水位，回合不受影响。
        let replayed = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert_eq!(replayed.title(), Some("第二个标题"));
        assert_eq!(replayed.last_event_seq(), seq_before + 2);
        assert_eq!(replayed.canonical_turns(), projection.canonical_turns());

        // 释放回合不丢标题；检查点覆盖标题事件后仍能重放出同一个标题。
        let mut released = projection.clone();
        released.release_canonical_turns();
        assert_eq!(released.title(), Some("第二个标题"));
        let mut checkpointed = projection.clone();
        checkpointed
            .write_checkpoint(root.path(), &session_id)
            .unwrap();
        let after_checkpoint =
            SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert_eq!(after_checkpoint.title(), Some("第二个标题"));
        assert_eq!(after_checkpoint.last_event_seq(), seq_before + 2);
        // 检查点之后的新标题继续生效。
        let (after, third) = after_checkpoint
            .append_session_title(root.path(), &session_id, "第三个标题")
            .unwrap();
        assert_eq!(third.event_seq, seq_before + 3);
        assert_eq!(after.title(), Some("第三个标题"));
        let again = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert_eq!(again.title(), Some("第三个标题"));
    }

    #[test]
    fn a_released_projection_cannot_take_a_title_event_until_replayed() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-title-released");
        let mut projection = append_item_transactions(root.path(), &session_id, 1);
        projection.release_canonical_turns();
        // 释放后的投影没有回合：此时提交可能写出空回合的检查点，必须拒绝。
        assert!(
            projection
                .append_session_title(root.path(), &session_id, "标题")
                .is_err()
        );
        // 重放常驻后可以提交，回合保持完整。
        let resident = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        let (next, _) = resident
            .append_session_title(root.path(), &session_id, "标题")
            .unwrap();
        assert_eq!(next.canonical_turns().len(), 1);
    }

    fn started_step_turn(session: &SessionId) -> CanonicalTurn {
        let main = "thread-main";
        serde_json::from_value(serde_json::json!({
            "sessionId": session, "turnId": "turn-1", "turnSeq": 1, "acceptedAt": 1,
            "status": "running",
            "items": [{
                "sessionId": session, "turnId": "turn-1", "turnSeq": 1, "itemId": "user-1",
                "itemSeq": 0, "kind": "user_message", "createdAt": 1, "status": "completed",
                "updatedAt": 1, "content": "hi", "sourceThreadId": main, "metadata": {}}],
            "modelSteps": [{
                "stepId": "step-1", "taskId": null, "threadId": main, "checkpointGeneration": 0,
                "requestFingerprint": "a".repeat(64), "provider": "p", "messageCount": 1,
                "toolCount": 0, "inputRequestIds": [], "startedAt": 2, "settledAt": null,
                "status": "started", "responseFingerprint": null,
                "attempts": [{"attemptId": "step-1:attempt:1", "startedAt": 2,
                              "settledAt": null, "status": "started", "transport": null}],
                "binding": {"model": "m", "provider": "p", "bindingRevision": 1,
                            "settingsRevision": 1, "usageContext": null},
                "maxOutputTokens": null, "goalId": null, "usageTokens": null,
                "goalTokensAfter": null, "executorCapabilities": null,
                "purpose": "execution", "promptFrame": null, "cacheEpoch": 0,
            }],
            "metadata": {},
        }))
        .unwrap()
    }

    fn attempt_stream(step: &str, attempt: &str) -> magi_session_store::ModelAttemptStream {
        magi_session_store::ModelAttemptStream {
            step_id: step.into(),
            attempt_id: attempt.into(),
            stream: vec![serde_json::json!({"type": "chunk", "time": 5,
                "chunk": {"type": "block-start", "index": 0, "blockType": "text"}})],
        }
    }

    #[test]
    fn attempt_streams_are_logged_replayed_and_survive_checkpoints_without_entering_the_projection()
    {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-attempt-stream");
        let projection = SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: started_step_turn(&session_id),
                }],
            )
            .unwrap();
        let turns_before = projection.canonical_turns().to_vec();
        let seq_before = projection.last_event_seq();
        let (next, event) = projection
            .append_model_attempt_stream(
                root.path(),
                &session_id,
                "turn-1",
                1,
                attempt_stream("step-1", "step-1:attempt:1"),
            )
            .unwrap();
        assert_eq!(
            event.kind,
            CanonicalTurnEventKind::ModelAttemptStreamRecorded
        );
        assert_eq!(event.event_seq, seq_before + 1);
        assert_eq!((event.turn_id.as_str(), event.turn_seq), ("turn-1", 1));
        // 流记录不改变回合与步骤。
        assert_eq!(next.canonical_turns(), turns_before.as_slice());
        // 重放与检查点后水位一致、回合不变；事件在历史里（封存或松散）仍可读。
        let replayed = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert_eq!(replayed.last_event_seq(), seq_before + 1);
        assert_eq!(replayed.canonical_turns(), turns_before.as_slice());
        let mut checkpointed = next.clone();
        checkpointed
            .write_checkpoint(root.path(), &session_id)
            .unwrap();
        let after = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert_eq!(after.last_event_seq(), seq_before + 1);
        assert_eq!(after.canonical_turns(), turns_before.as_slice());
    }

    #[test]
    fn attempt_streams_must_point_at_a_real_attempt_of_a_resident_turn() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-attempt-stream-invalid");
        let projection = SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: started_step_turn(&session_id),
                }],
            )
            .unwrap();
        let append = |turn: &str, seq: u64, stream| {
            projection.append_model_attempt_stream(root.path(), &session_id, turn, seq, stream)
        };
        assert!(append("turn-1", 1, attempt_stream("step-1", "step-1:attempt:1")).is_ok());
        assert!(append("turn-1", 1, attempt_stream("step-x", "step-1:attempt:1")).is_err());
        assert!(append("turn-1", 1, attempt_stream("step-1", "step-1:attempt:9")).is_err());
        assert!(append("turn-1", 7, attempt_stream("step-1", "step-1:attempt:1")).is_err());
        assert!(append("turn-9", 1, attempt_stream("step-1", "step-1:attempt:1")).is_err());
        let mut empty = attempt_stream("step-1", "step-1:attempt:1");
        empty.stream.clear();
        assert!(append("turn-1", 1, empty).is_err());
        let mut released = projection.clone();
        released.release_canonical_turns();
        assert!(
            released
                .append_model_attempt_stream(
                    root.path(),
                    &session_id,
                    "turn-1",
                    1,
                    attempt_stream("step-1", "step-1:attempt:1"),
                )
                .is_err()
        );
    }

    #[test]
    fn failed_attempt_replays_as_assistant_attempt_before_retried_assistant_message() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-attempt-retry-replay");
        let turn_initial = started_step_turn(&session_id);
        let projection = SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: turn_initial.clone(),
                }],
            )
            .unwrap();
        let failed_stream = vec![
            serde_json::json!({"type": "chunk", "time": 10, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}),
            serde_json::json!({"type": "text-chunks", "time0": 11, "index": 0, "dt": [], "texts": ["半截"]}),
            serde_json::json!({"type": "chunk", "time": 20, "chunk": {
                "type": "finish",
                "reason": {"kind": "error", "failure": {"message": "503", "code": "provider_http_503", "status": 503}}
            }}),
        ];
        let (projection, _) = projection
            .append_model_attempt_stream(
                root.path(),
                &session_id,
                "turn-1",
                1,
                magi_session_store::ModelAttemptStream {
                    step_id: "step-1".into(),
                    attempt_id: "step-1:attempt:1".into(),
                    stream: failed_stream.clone(),
                },
            )
            .unwrap();
        // 第一次尝试结算为失败，再开始第二次尝试并落盘成功流 + 助手消息。
        let mut turn_retry = turn_initial.clone();
        turn_retry.model_steps[0].attempts[0].status =
            magi_session_store::ModelStepStatus::OutcomeUnknown;
        turn_retry.model_steps[0].attempts[0].settled_at = Some(UtcMillis(21));
        turn_retry.model_steps[0].attempts.push(
            serde_json::from_value(serde_json::json!({
                "attemptId": "step-1:attempt:2", "startedAt": 25,
                "settledAt": null, "status": "started", "transport": null,
            }))
            .unwrap(),
        );
        let projection = projection
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(turn_initial),
                    next: turn_retry.clone(),
                }],
            )
            .unwrap();
        let succeeded_stream = vec![
            serde_json::json!({"type": "chunk", "time": 30, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}),
            serde_json::json!({"type": "text-chunks", "time0": 31, "index": 0, "dt": [], "texts": ["成功"]}),
            serde_json::json!({"type": "chunk", "time": 32, "chunk": {"type": "block-end", "index": 0, "block": {"type": "text", "text": "成功"}}}),
            serde_json::json!({"type": "chunk", "time": 33, "chunk": {"type": "finish", "reason": {"kind": "stop"}}}),
        ];
        let (projection, _) = projection
            .append_model_attempt_stream(
                root.path(),
                &session_id,
                "turn-1",
                1,
                magi_session_store::ModelAttemptStream {
                    step_id: "step-1".into(),
                    attempt_id: "step-1:attempt:2".into(),
                    stream: succeeded_stream.clone(),
                },
            )
            .unwrap();
        let mut turn_done = turn_retry.clone();
        turn_done.items.push(
            serde_json::from_value(serde_json::json!({
                "sessionId": session_id, "turnId": "turn-1", "turnSeq": 1, "itemId": "assistant-1",
                "itemSeq": 1, "kind": "assistant_text", "createdAt": 34, "status": "completed",
                "updatedAt": 34, "content": "成功", "sourceThreadId": "thread-main",
                "metadata": {"modelStepId": "step-1"}
            }))
            .unwrap(),
        );
        let mut projection = projection
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(turn_retry),
                    next: turn_done,
                }],
            )
            .unwrap();
        projection
            .write_checkpoint(root.path(), &session_id)
            .unwrap();
        let page = CommittedObservationPage::read_for_scope(
            &session_id,
            0,
            None,
            64,
            1024 * 1024,
            crate::native_plugin_platform::session_observation::ObservationScope::Main,
            |after, through| projection.open_history_page(root.path(), after, through),
        )
        .unwrap();
        let value = serde_json::to_value(&page).unwrap();
        let events = value["events"].as_array().unwrap();
        let kinds: Vec<&str> = events
            .iter()
            .map(|entry| entry["observation"]["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "turnStarted",
                "stepStarted",
                "assistantAttempt",
                "assistantMessage",
                "stepEnded"
            ]
        );
        assert_eq!(
            events[2]["observation"]["stream"],
            serde_json::json!(failed_stream)
        );
        assert_eq!(
            events[3]["observation"]["stream"],
            serde_json::json!(succeeded_stream)
        );
    }

    #[test]
    fn model_request_records_dedup_change_checkpoint_and_enforce_size_limit() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-model-request");
        let mut turn1 = started_step_turn(&session_id);
        let step2 = serde_json::from_value(serde_json::json!({
            "stepId": "step-2", "taskId": null, "threadId": "thread-main", "checkpointGeneration": 0,
            "requestFingerprint": "b".repeat(64), "provider": "p", "messageCount": 2,
            "toolCount": 1, "inputRequestIds": [], "startedAt": 10, "settledAt": null,
            "status": "started", "responseFingerprint": null,
            "attempts": [{"attemptId": "step-2:attempt:1", "startedAt": 10,
                          "settledAt": null, "status": "started", "transport": null}],
            "binding": {"model": "m", "provider": "p", "bindingRevision": 1,
                        "settingsRevision": 1, "usageContext": null},
            "maxOutputTokens": null, "goalId": null, "usageTokens": null,
            "goalTokensAfter": null, "executorCapabilities": null,
            "purpose": "execution", "promptFrame": null, "cacheEpoch": 0,
        }))
        .unwrap();
        let step3 = serde_json::from_value(serde_json::json!({
            "stepId": "step-3", "taskId": null, "threadId": "thread-main", "checkpointGeneration": 0,
            "requestFingerprint": "c".repeat(64), "provider": "p", "messageCount": 3,
            "toolCount": 1, "inputRequestIds": [], "startedAt": 20, "settledAt": null,
            "status": "started", "responseFingerprint": null,
            "attempts": [{"attemptId": "step-3:attempt:1", "startedAt": 20,
                          "settledAt": null, "status": "started", "transport": null}],
            "binding": {"model": "m2", "provider": "p", "bindingRevision": 2,
                        "settingsRevision": 2, "usageContext": null},
            "maxOutputTokens": null, "goalId": null, "usageTokens": null,
            "goalTokensAfter": null, "executorCapabilities": null,
            "purpose": "execution", "promptFrame": null, "cacheEpoch": 1,
        }))
        .unwrap();
        turn1.model_steps.push(step2);
        turn1.model_steps.push(step3);
        let projection = SessionConversationProjection::default()
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: None,
                    next: turn1,
                }],
            )
            .unwrap();

        let header1 = magi_session_store::ModelRequestHeader {
            config: magi_session_store::ModelRequestCallConfig {
                provider: "p".into(),
                model: "m".into(),
                reasoning_effort: Some("low".into()),
                temperature: None,
                max_tokens: Some(4096),
                stop: Vec::new(),
            },
            tools: vec![magi_session_store::ModelRequestToolSchema {
                name: "shell_exec".into(),
                description: "Run shell".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }],
        };
        let snap1 = magi_session_store::ModelRequestSnapshot {
            step_id: "step-1".into(),
            system_prompt: "You are Magi.".into(),
            header: header1.clone(),
            context_window: Some(128_000),
            redacted: false,
        };
        let (projection, ev1) = projection
            .append_model_request(root.path(), &session_id, "turn-1", 1, snap1)
            .unwrap()
            .expect("first request recorded");
        assert!(ev1.model_request.as_ref().unwrap().system_prompt.is_some());
        assert!(ev1.model_request.as_ref().unwrap().header.is_some());

        // 写检查点后重新加载：step-2 相同系统提示和请求头应按检查点投影去重！
        let mut checkpointed = projection;
        checkpointed
            .write_checkpoint(root.path(), &session_id)
            .unwrap();
        let reloaded = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        let snap2 = magi_session_store::ModelRequestSnapshot {
            step_id: "step-2".into(),
            system_prompt: "You are Magi.".into(),
            header: header1.clone(),
            context_window: Some(128_000),
            redacted: false,
        };
        let (reloaded, ev2) = reloaded
            .append_model_request(root.path(), &session_id, "turn-1", 1, snap2)
            .unwrap()
            .expect("deduped request recorded");
        assert!(ev2.model_request.as_ref().unwrap().system_prompt.is_none());
        assert!(ev2.model_request.as_ref().unwrap().header.is_none());

        // step-3 变更模型与系统提示：写出新正文。
        let mut header3 = header1;
        header3.config.model = "m2".into();
        let snap3 = magi_session_store::ModelRequestSnapshot {
            step_id: "step-3".into(),
            system_prompt: "You are Magi v2.".into(),
            header: header3,
            context_window: Some(200_000),
            redacted: true,
        };
        let (reloaded, ev3) = reloaded
            .append_model_request(root.path(), &session_id, "turn-1", 1, snap3)
            .unwrap()
            .expect("changed request recorded");
        assert_eq!(
            ev3.model_request.as_ref().unwrap().system_prompt.as_deref(),
            Some("You are Magi v2.")
        );
        assert!(ev3.model_request.as_ref().unwrap().redacted);

        // 超过 4 MiB 上限的请求记录直接放弃返回 Ok(None)，水位不变。
        let seq_before_oversized = reloaded.last_event_seq();
        let oversized = magi_session_store::ModelRequestSnapshot {
            step_id: "step-3".into(),
            system_prompt: "x".repeat(magi_session_store::MAX_MODEL_REQUEST_RECORD_BYTES + 1024),
            header: magi_session_store::ModelRequestHeader {
                config: magi_session_store::ModelRequestCallConfig {
                    provider: "p".into(),
                    model: "m2".into(),
                    reasoning_effort: None,
                    temperature: None,
                    max_tokens: None,
                    stop: Vec::new(),
                },
                tools: Vec::new(),
            },
            context_window: None,
            redacted: false,
        };
        assert!(
            reloaded
                .append_model_request(root.path(), &session_id, "turn-1", 1, oversized)
                .unwrap()
                .is_none()
        );
        assert_eq!(reloaded.last_event_seq(), seq_before_oversized);

        // 历史重放应准确产出：step-1(initial + context + system), step-2(去重无观察), step-3(change + context + system)。
        let page = CommittedObservationPage::read_for_scope(
            &session_id,
            0,
            None,
            64,
            1024 * 1024,
            crate::native_plugin_platform::session_observation::ObservationScope::Main,
            |after, through| reloaded.open_history_page(root.path(), after, through),
        )
        .unwrap();
        let value = serde_json::to_value(&page).unwrap();
        let req_obs: Vec<_> = value["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["observation"].clone())
            .filter(|obs| {
                matches!(
                    obs["kind"].as_str(),
                    Some("requestHeader" | "requestContext" | "systemMessage")
                )
            })
            .collect();
        assert_eq!(req_obs.len(), 6);
        assert_eq!(req_obs[0]["kind"], "requestHeader");
        assert_eq!(req_obs[0]["reason"], "initial");
        assert_eq!(req_obs[1]["kind"], "requestContext");
        assert_eq!(req_obs[1]["model"], "m");
        assert_eq!(req_obs[2]["kind"], "systemMessage");
        assert_eq!(req_obs[2]["text"], "You are Magi.");
        assert_eq!(req_obs[3]["kind"], "requestHeader");
        assert_eq!(req_obs[3]["reason"], "change");
        assert_eq!(req_obs[4]["kind"], "requestContext");
        assert_eq!(req_obs[4]["model"], "m2");
        assert_eq!(req_obs[5]["kind"], "systemMessage");
        assert_eq!(req_obs[5]["text"], "You are Magi v2.");
    }

    #[test]
    fn title_events_reject_turn_identity_and_missing_titles() {
        let session_id = SessionId::new("session-title-invalid");
        let base = |title: Option<&str>, turn_id: &str, turn_seq: u64| CanonicalTurnEvent {
            schema_version: CANONICAL_TURN_SCHEMA_VERSION.into(),
            event_id: event_id(&session_id, 1),
            event_seq: 1,
            kind: CanonicalTurnEventKind::SessionTitleChanged,
            session_id: session_id.clone(),
            turn_id: turn_id.into(),
            turn_seq,
            occurred_at: UtcMillis(1),
            turn: None,
            item: None,
            input: None,
            model_step: None,
            title: title.map(str::to_string),
            attempt_stream: None,
            model_request: None,
            command_event: None,
        };
        let apply = |event| SessionConversationProjection::default().apply_event(event);
        assert!(apply(base(Some("标题"), "", 0)).is_ok());
        assert!(apply(base(None, "", 0)).is_err());
        assert!(apply(base(Some("  "), "", 0)).is_err());
        assert!(apply(base(Some("标题"), "turn-1", 0)).is_err());
        assert!(apply(base(Some("标题"), "", 3)).is_err());
        // 其他事件不得携带标题。
        let mut other = base(Some("标题"), "", 0);
        other.kind = CanonicalTurnEventKind::InputUpdated;
        assert!(apply(other).is_err());
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
            vec![
                checkpoint_file_name(checkpoint_seq).display().to_string(),
                "history".into()
            ],
            "达到阈值后保留检查点与封存历史"
        );
        assert_eq!(projection.transactions_since_checkpoint, 0);

        let replayed = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("checkpoint should load");
        let segment = HistorySegment::new(1, checkpoint_seq).unwrap();
        assert_eq!(replayed.history_segments, vec![segment]);
        let archived = archived_transactions(temp.path(), segment);
        assert_eq!(archived.len(), CHECKPOINT_TRANSACTION_INTERVAL);
        assert!(
            archived
                .iter()
                .all(|transaction| transaction.session_id == session_id)
        );
        assert_eq!(
            archived
                .iter()
                .flat_map(|transaction| transaction.events.iter().map(|event| event.event_seq))
                .collect::<Vec<_>>(),
            (1..=checkpoint_seq).collect::<Vec<_>>(),
        );
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
        assert_eq!(event_file_names(temp.path()).len(), 3);
        let reloaded = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("checkpoint plus tail should load");
        assert_eq!(reloaded.canonical_turns(), advanced.canonical_turns());
        assert_eq!(reloaded.transactions_since_checkpoint, 1);

        assert!(
            reloaded
                .open_history_page(temp.path(), 0, Some(advanced.last_event_seq() + 1))
                .is_err()
        );
    }

    fn archived_transactions(
        root: &Path,
        segment: HistorySegment,
    ) -> Vec<CanonicalEventTransaction> {
        let file = fs::File::open(segment.path(root)).unwrap();
        let source = std::io::BufReader::new(flate2::read::GzDecoder::new(file));
        serde_json::Deserializer::from_reader(source)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn failed_history_write_keeps_original_transactions_and_retries_from_the_same_boundary() {
        use std::io::Read;

        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-archive-retry");
        let archive_dir = HistorySegment::new(1, 1).unwrap().path(root.path());
        let archive_dir = archive_dir.parent().unwrap();
        fs::write(archive_dir, b"block archive directory creation").unwrap();
        let projection =
            append_item_transactions(root.path(), &session_id, CHECKPOINT_TRANSACTION_INTERVAL);
        assert!(projection.history_segments.is_empty());
        let (checkpoints, transactions) = event_file_paths(root.path()).unwrap();
        assert!(checkpoints.is_empty());
        assert_eq!(transactions.len(), CHECKPOINT_TRANSACTION_INTERVAL);
        let mut original = Vec::new();
        for (_, path) in &transactions {
            original.extend(fs::read(path).unwrap());
            original.push(b'\n');
        }

        fs::remove_file(archive_dir).unwrap();
        // Emulate interruption after sealing and before the checkpoint commits.
        let provisional = HistorySegment::new(1, projection.last_event_seq()).unwrap();
        provisional.archive(root.path(), &transactions).unwrap();
        let reopened = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        assert!(reopened.history_segments.is_empty());
        let next = reopened
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(reopened.canonical_turns()[0].clone()),
                    next: running_turn_with_items(&session_id, CHECKPOINT_TRANSACTION_INTERVAL),
                }],
            )
            .unwrap();
        let segment = HistorySegment::new(1, next.last_event_seq()).unwrap();
        assert_eq!(next.history_segments, [segment]);
        assert!(!provisional.path(root.path()).exists());
        assert!(event_file_paths(root.path()).unwrap().1.is_empty());
        let decoder =
            flate2::read::GzDecoder::new(fs::File::open(segment.path(root.path())).unwrap());
        let mut archived_prefix = vec![0; original.len()];
        let mut decoder = decoder;
        decoder.read_exact(&mut archived_prefix).unwrap();
        assert_eq!(archived_prefix, original);
        assert_eq!(
            archived_transactions(root.path(), segment).len(),
            CHECKPOINT_TRANSACTION_INTERVAL + 1
        );
    }

    #[test]
    fn history_after_an_existing_checkpoint_preserves_the_original_sequence_floor() {
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new("session-history-floor");
        let previous = append_item_transactions(root.path(), &session_id, 1);
        let compacted_through = previous.last_event_seq();
        let checkpoint = CanonicalEventCheckpoint {
            schema_version: EVENT_CHECKPOINT_SCHEMA_VERSION,
            session_id: session_id.clone(),
            last_event_seq: compacted_through,
            canonical_turns: previous.canonical_turns().to_vec(),
            inputs: Vec::new(),
            accepted_submissions: Vec::new(),
            history_segments: Vec::new(),
            title: None,
            latest_model_request: None,
        };
        fs::write(
            root.path().join(checkpoint_file_name(compacted_through)),
            serde_json::to_vec(&checkpoint).unwrap(),
        )
        .unwrap();
        let reopened = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        let mut next = reopened
            .append_transaction(
                root.path(),
                &session_id,
                &[CanonicalTurnMutation {
                    previous: Some(reopened.canonical_turns()[0].clone()),
                    next: running_turn_with_items(&session_id, 1),
                }],
            )
            .unwrap();
        next.write_checkpoint(root.path(), &session_id).unwrap();
        let reopened = SessionConversationProjection::load(root.path(), &session_id).unwrap();
        let retained =
            HistorySegment::new(compacted_through + 1, reopened.last_event_seq()).unwrap();
        assert_eq!(reopened.history_segments, [retained]);
        let transactions = archived_transactions(root.path(), retained);
        assert_eq!(transactions[0].first_event_seq, compacted_through + 1);
        assert_eq!(reopened.canonical_turns(), next.canonical_turns());

        let read = reopened.open_history_page(root.path(), 0, None).unwrap();
        assert_eq!(read.first_retained, compacted_through + 1);
    }

    #[test]
    fn history_read_pins_its_source_during_checkpoint_commit() {
        let root = tempfile::tempdir().unwrap();
        let session = SessionId::new("session-history-open-file");
        let mut projection = append_item_transactions(root.path(), &session, 1);
        let read = projection.open_history_page(root.path(), 0, None).unwrap();
        let (range, path) = event_file_paths(root.path()).unwrap().1.pop().unwrap();
        let transaction = read_transaction(&path).unwrap();
        projection.write_checkpoint(root.path(), &session).unwrap();
        assert!(!path.exists());

        assert_eq!(read.through, range.last_event_seq);
        let (opened_range, mut transactions) = read.transactions().unwrap();
        assert_eq!(opened_range, range);
        assert_eq!(transactions.next().unwrap().unwrap(), transaction);
        assert!(transactions.next().is_none());
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
            inputs: Vec::new(),
            accepted_submissions: Vec::new(),
            history_segments: Vec::new(),
            title: None,
            latest_model_request: None,
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

    #[test]
    fn command_events_persist_across_checkpoints_and_project_observations() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-commands");
        let projection = SessionConversationProjection::default();
        let (projection, run_event) = projection
            .append_session_command_event(
                temp.path(),
                &session_id,
                magi_session_store::SessionCommandEvent::Run {
                    command_id: "cmd-1".into(),
                    name: "ping".into(),
                    args: Some(" world".into()),
                },
            )
            .expect("command/run should append");
        assert_eq!(run_event.event_seq, 1);
        assert_eq!(run_event.kind, CanonicalTurnEventKind::CommandEventRecorded);

        let (mut projection, done_event) = projection
            .append_session_command_event(
                temp.path(),
                &session_id,
                magi_session_store::SessionCommandEvent::Done {
                    command_id: "cmd-1".into(),
                    outcome: magi_session_store::SessionCommandOutcome::Success,
                    text: Some("pong:world".into()),
                    source_event_seq: None,
                },
            )
            .expect("command/done should append");
        assert_eq!(done_event.event_seq, 2);

        projection
            .write_checkpoint(temp.path(), &session_id)
            .expect("checkpoint should write");
        let reloaded = SessionConversationProjection::load(temp.path(), &session_id)
            .expect("projection should reload after checkpoint");
        assert_eq!(reloaded.last_event_seq(), 2);

        let page = CommittedObservationPage::read_for_scope(
            &session_id,
            0,
            None,
            64,
            1024 * 1024,
            crate::native_plugin_platform::session_observation::ObservationScope::Main,
            |after, through| reloaded.open_history_page(temp.path(), after, through),
        )
        .expect("observations should replay");
        let value = serde_json::to_value(&page).expect("json");
        let events = value["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0]["observation"],
            serde_json::json!({
                "kind": "commandRun",
                "commandId": "cmd-1",
                "name": "ping",
                "args": " world",
            })
        );
        assert_eq!(
            events[1]["observation"],
            serde_json::json!({
                "kind": "commandDone",
                "commandId": "cmd-1",
                "outcome": "success",
                "text": "pong:world",
            })
        );

        // 非法命令事件（错误结果缺少 text）必须拒绝。
        reloaded
            .append_session_command_event(
                temp.path(),
                &session_id,
                magi_session_store::SessionCommandEvent::Done {
                    command_id: "cmd-2".into(),
                    outcome: magi_session_store::SessionCommandOutcome::Error,
                    text: Some("  ".into()),
                    source_event_seq: None,
                },
            )
            .expect_err("error command/done without non-empty text must fail");
    }

    #[test]
    fn title_events_project_session_title_observations() {
        let temp = tempfile::tempdir().expect("tempdir should create");
        let session_id = SessionId::new("session-titles");
        let projection = SessionConversationProjection::default();
        let (projection, event) = projection
            .append_session_title(temp.path(), &session_id, "Renamed Session")
            .expect("title should append");
        assert_eq!(event.event_seq, 1);
        assert_eq!(event.kind, CanonicalTurnEventKind::SessionTitleChanged);

        let page = CommittedObservationPage::read_for_scope(
            &session_id,
            0,
            None,
            64,
            1024 * 1024,
            crate::native_plugin_platform::session_observation::ObservationScope::Main,
            |after, through| projection.open_history_page(temp.path(), after, through),
        )
        .expect("main observations should replay");
        let value = serde_json::to_value(&page).expect("json");
        let events = value["events"].as_array().expect("events array");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]["observation"],
            serde_json::json!({
                "kind": "sessionTitle",
                "title": "Renamed Session",
            })
        );

        let subagent_page = CommittedObservationPage::read_for_scope(
            &session_id,
            0,
            None,
            64,
            1024 * 1024,
            crate::native_plugin_platform::session_observation::ObservationScope::Subagent {
                task_id: "child-1",
            },
            |after, through| projection.open_history_page(temp.path(), after, through),
        )
        .expect("subagent observations should replay");
        let subagent_value = serde_json::to_value(&subagent_page).expect("json");
        assert_eq!(
            subagent_value["events"]
                .as_array()
                .expect("subagent events array")
                .len(),
            0
        );
    }
}

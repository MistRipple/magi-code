use magi_core::ThreadId;
use magi_session_store::{
    AgentTurnBinding, CanonicalTurn, CanonicalTurnEvent, CanonicalTurnEventKind, CanonicalTurnItem,
    CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus, ModelStepPurpose,
    ModelTransportOutcome, SESSION_COMMAND_METADATA_KEY, SessionInput, SessionInputState,
    TOOL_RESULT_COMMITTED_METADATA_KEY,
};
use serde_json::{Value, json};

/// 观察投影的作用域：主线用户对话，或父会话日志中按 `task_id` 投影出的只读子代理轨迹。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationScope<'a> {
    Main,
    Subagent { task_id: &'a str },
}

/// 用于把重放/检查点里的最新请求摘要播种给在线观察，确保 daemon 重启后的增量观察
/// 与全量重放保持一致（不会把后续步骤的工具变更误判为首次 `request/header`，也不会重复发 `request/context`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LatestModelRequestState {
    pub(crate) header_sha256: String,
    pub(crate) system_prompt_sha256: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) context_window: Option<u64>,
}

/// 单会话跨事务保留的观察推导状态：
/// 1. 待被助手消息领走的成功尝试流记录（按 `(回合, 步骤)`）；
/// 2. 已观察到的最新请求头、系统提示摘要与路由上下文，用于计算 `request/header`（`initial`/`change`）、
///    `request/context` 与 `system/message` 变更。
///
/// 在线通知（按会话持有一份）与历史重放（按一次读取持有一份）共用同一推导。
#[derive(Default)]
pub(crate) struct PendingAttemptStreams {
    streams: std::collections::HashMap<(String, String), Vec<Value>>,
    seen_request: bool,
    latest_header_sha256: Option<String>,
    latest_system_prompt_sha256: Option<String>,
    latest_route: Option<(String, String, Option<u64>)>,
}

impl PendingAttemptStreams {
    pub(crate) fn seed_latest_request(&mut self, latest: Option<&LatestModelRequestState>) {
        if self.seen_request {
            return;
        }
        if let Some(latest) = latest {
            self.seen_request = true;
            self.latest_header_sha256 = Some(latest.header_sha256.clone());
            self.latest_system_prompt_sha256 = Some(latest.system_prompt_sha256.clone());
            self.latest_route = Some((
                latest.provider.clone(),
                latest.model.clone(),
                latest.context_window,
            ));
        }
    }

    fn insert(&mut self, turn_id: &str, step_id: &str, stream: Vec<Value>) {
        self.streams
            .insert((turn_id.to_string(), step_id.to_string()), stream);
    }

    fn take(&mut self, turn_id: &str, step_id: &str) -> Option<Vec<Value>> {
        self.streams
            .remove(&(turn_id.to_string(), step_id.to_string()))
    }

    fn forget_turn(&mut self, turn_id: &str) {
        self.streams.retain(|(turn, _), _| turn != turn_id);
    }
}

/// Project the observations of an atomic native transaction for the main session thread.
/// Live notification and history replay share the exact same inputs and derivation.
pub(crate) fn committed_observations<'a>(
    events: &'a [CanonicalTurnEvent],
    before_turns: &'a [CanonicalTurn],
    before_inputs: &'a [SessionInput],
    after_turns: &'a [CanonicalTurn],
    pending: &'a mut PendingAttemptStreams,
) -> impl Iterator<Item = (&'a CanonicalTurnEvent, Value)> + 'a {
    committed_observations_for_scope(
        events,
        before_turns,
        before_inputs,
        after_turns,
        pending,
        ObservationScope::Main,
    )
}

/// 列出本次事务触及到的全部子代理 `task_id`（保持出现顺序、去重），供在线通知向子代理订阅者推送。
pub(crate) fn touched_subagent_task_ids(
    events: &[CanonicalTurnEvent],
    after_turns: &[CanonicalTurn],
) -> Vec<String> {
    let mut ids = Vec::new();
    for event in events {
        if let Some(pos) = turn_position(event, after_turns)
            && let Some(binding) = after_turns[pos].agent_turn_binding().ok().flatten()
        {
            let task_id = binding.task_id.as_str();
            if !ids.iter().any(|existing: &String| existing == task_id) {
                ids.push(task_id.to_string());
            }
        }
    }
    ids
}

/// Project the observations of an atomic native transaction for either the main session
/// or one read-only subagent task projected from the parent session's canonical event log.
pub(crate) fn committed_observations_for_scope<'a>(
    events: &'a [CanonicalTurnEvent],
    before_turns: &'a [CanonicalTurn],
    before_inputs: &'a [SessionInput],
    after_turns: &'a [CanonicalTurn],
    pending: &'a mut PendingAttemptStreams,
    scope: ObservationScope<'a>,
) -> impl Iterator<Item = (&'a CanonicalTurnEvent, Value)> + 'a {
    events.iter().flat_map(move |event| {
        let scoped_turn = || {
            let (number, turn, thread_id) = scoped_turn_position(event, after_turns, scope)?;
            let previous = turn_position(event, before_turns).map(|pos| &before_turns[pos]);
            Some((previous, number, turn, thread_id))
        };
        let mut observations = Vec::new();
        match event.kind {
            CanonicalTurnEventKind::TurnStarted => {
                if scope == ObservationScope::Main
                    && let Some(pos) = turn_position(event, after_turns)
                    && turn_position(event, before_turns).is_none()
                    && let Some(binding) = after_turns[pos].agent_turn_binding().ok().flatten()
                    && binding.previous_turn_id.is_none()
                {
                    let sub_turn = &after_turns[pos];
                    observations.push(json!({
                        "kind": "subagentCatalog",
                        "childId": binding.task_id.as_str(),
                        "childCreatedAt": sub_turn.accepted_at.0,
                        "mode": "one-shot",
                        "label": subagent_label(sub_turn, &binding),
                    }));
                }
                if let Some((previous, number, turn, thread_id)) = scoped_turn() {
                    if matches!(scope, ObservationScope::Subagent { .. })
                        && previous.is_none()
                        && let Some(binding) = turn.agent_turn_binding().ok().flatten()
                        && binding.previous_turn_id.is_none()
                    {
                        observations.push(json!({
                            "kind": "subagentDescriptor",
                            "version": 3,
                            "mode": "one-shot",
                            "provider": "magi",
                            "label": subagent_label(turn, &binding),
                        }));
                    }
                    observations.extend(committed_turn_boundary(
                        event, previous, number, turn, &thread_id,
                    ));
                }
            }
            CanonicalTurnEventKind::TurnCompleted => {
                pending.forget_turn(&event.turn_id);
                if let Some((previous, number, turn, thread_id)) = scoped_turn()
                    && let Some(ended) =
                        committed_turn_boundary(event, previous, number, turn, &thread_id)
                {
                    // 回合结束先收口还开着的步骤：DSH 的 `turn/end` 之前不留未关闭的步骤。
                    observations.extend(
                        open_steps_for_thread(turn, &thread_id)
                            .into_iter()
                            .map(|step| json!({"kind": "stepEnded", "turn": number, "step": step})),
                    );
                    observations.push(ended);
                }
            }
            CanonicalTurnEventKind::ModelRequestRecorded => {
                if let Some(recorded) = event.model_request.as_ref()
                    && let Some((_, number, turn, thread_id)) = scoped_turn()
                    && let Some(step) = step_number_for_thread(turn, &thread_id, &recorded.step_id)
                {
                    // 1. request/header：首次或请求头摘要变化时发出；去重省略（header=None）表示未变化。
                    if let Some(header) = recorded.header.as_ref()
                        && pending.latest_header_sha256.as_deref()
                            != Some(recorded.header_sha256.as_str())
                    {
                        let reason =
                            if !pending.seen_request && pending.latest_header_sha256.is_none() {
                                "initial"
                            } else {
                                "change"
                            };
                        observations.push(json!({
                            "kind": "requestHeader",
                            "header": header,
                            "reason": reason,
                        }));
                    }
                    // 2. request/context：路由或上下文窗口变化时发出（即使 header 摘要未变但 contextWindow 变化）。
                    let next_route = if let Some(header) = recorded.header.as_ref() {
                        Some((
                            header.config.provider.clone(),
                            header.config.model.clone(),
                            recorded.context_window,
                        ))
                    } else {
                        pending.latest_route.as_ref().map(|(provider, model, _)| {
                            (provider.clone(), model.clone(), recorded.context_window)
                        })
                    };
                    if let Some(route) = next_route
                        && pending.latest_route.as_ref() != Some(&route)
                    {
                        let mut ctx = json!({
                            "kind": "requestContext",
                            "provider": route.0,
                            "model": route.1,
                        });
                        if let Some(window) = route.2 {
                            ctx["contextWindow"] = json!(window);
                        }
                        observations.push(ctx);
                        pending.latest_route = Some(route);
                    }
                    // 3. system/message：首次（非空）或系统提示变化时发出；去重省略表示未变化。
                    if let Some(text) = recorded.system_prompt.as_ref() {
                        let changed = pending.latest_system_prompt_sha256.as_deref()
                            != Some(recorded.system_prompt_sha256.as_str());
                        if changed && (pending.seen_request || !text.is_empty()) {
                            observations.push(json!({
                                "kind": "systemMessage",
                                "turn": number,
                                "step": step,
                                "messageId": format!("system-{}", recorded.step_id),
                                "text": text,
                            }));
                        }
                    }
                    pending.seen_request = true;
                    pending.latest_header_sha256 = Some(recorded.header_sha256.clone());
                    pending.latest_system_prompt_sha256 =
                        Some(recorded.system_prompt_sha256.clone());
                }
            }
            CanonicalTurnEventKind::ModelAttemptStreamRecorded => {
                if let Some(recorded) = event.attempt_stream.as_ref()
                    && let Some((_, number, turn, thread_id)) = scoped_turn()
                    && let Some(step) = step_number_for_thread(turn, &thread_id, &recorded.step_id)
                {
                    if is_failed_attempt_stream(&recorded.stream) {
                        observations.push(json!({
                            "kind": "assistantAttempt",
                            "turn": number,
                            "step": step,
                            "stream": recorded.stream,
                        }));
                    } else {
                        pending.insert(&event.turn_id, &recorded.step_id, recorded.stream.clone());
                    }
                }
            }
            CanonicalTurnEventKind::SessionTitleChanged => {
                if scope == ObservationScope::Main
                    && let Some(title) = event.title.as_deref().filter(|t| !t.trim().is_empty())
                {
                    observations.push(json!({
                        "kind": "sessionTitle",
                        "title": title,
                    }));
                }
            }
            CanonicalTurnEventKind::CommandEventRecorded => {
                if scope == ObservationScope::Main
                    && let Some(recorded) = event.command_event.as_ref()
                {
                    match recorded {
                        magi_session_store::SessionCommandEvent::Run {
                            command_id,
                            name,
                            args,
                        } => {
                            let mut payload = json!({
                                "kind": "commandRun",
                                "commandId": command_id,
                                "name": name,
                            });
                            if let Some(args) = args {
                                payload["args"] = json!(args);
                            }
                            observations.push(payload);
                        }
                        magi_session_store::SessionCommandEvent::Done {
                            command_id,
                            outcome,
                            text,
                            source_event_seq,
                        } => {
                            let outcome_str = match outcome {
                                magi_session_store::SessionCommandOutcome::Success => "success",
                                magi_session_store::SessionCommandOutcome::Error => "error",
                            };
                            let mut payload = json!({
                                "kind": "commandDone",
                                "commandId": command_id,
                                "outcome": outcome_str,
                            });
                            if let Some(text) = text {
                                payload["text"] = json!(text);
                            }
                            if let Some(seq) = source_event_seq {
                                payload["sourceEventSeq"] = json!(seq);
                            }
                            observations.push(payload);
                        }
                    }
                }
            }
            CanonicalTurnEventKind::ModelStepUpdated => {
                if let Some((previous, number, turn, thread_id)) = scoped_turn()
                    && let Some(step) = committed_step_start(event, previous, turn, &thread_id)
                {
                    observations.push(json!({"kind": "stepStarted", "turn": number, "step": step}));
                }
            }
            CanonicalTurnEventKind::TurnItemUpsert => {
                if let Some(item) = event.item.as_ref() {
                    match item.kind {
                        CanonicalTurnItemKind::ToolCall => {
                            if let Some((previous, number, turn, thread_id)) = scoped_turn() {
                                observations.extend(committed_model_tool_event(
                                    event, previous, number, turn, &thread_id,
                                ));
                                observations.extend(committed_step_end(
                                    item, previous, number, turn, &thread_id,
                                ));
                            }
                        }
                        CanonicalTurnItemKind::UserMessage => {
                            if scope == ObservationScope::Main {
                                observations.extend(committed_text_input(
                                    event,
                                    events,
                                    before_inputs,
                                ));
                            }
                        }
                        CanonicalTurnItemKind::AssistantText => {
                            if let Some((previous, number, turn, thread_id)) = scoped_turn() {
                                observations.extend(committed_assistant_message(
                                    event, previous, number, turn, &thread_id, pending,
                                ));
                                observations.extend(committed_step_end(
                                    item, previous, number, turn, &thread_id,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        observations.into_iter().map(move |value| (event, value))
    })
}

fn subagent_label(turn: &CanonicalTurn, binding: &AgentTurnBinding) -> String {
    turn.items
        .iter()
        .find_map(|item| {
            item.worker
                .as_ref()
                .and_then(|worker| worker.title.as_deref())
                .map(str::trim)
                .filter(|title| !title.is_empty())
        })
        .unwrap_or(binding.role_id.as_str())
        .to_string()
}

fn scoped_turn_thread_id(turn: &CanonicalTurn, scope: ObservationScope<'_>) -> Option<ThreadId> {
    match scope {
        ObservationScope::Main => {
            if !observes_main_turn(turn) {
                return None;
            }
            turn.items
                .iter()
                .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
                .map(|item| item.source_thread_id.clone())
        }
        ObservationScope::Subagent { task_id } => turn
            .agent_turn_binding()
            .ok()
            .flatten()
            .filter(|binding| binding.task_id.as_str() == task_id)
            .map(|binding| binding.thread_id),
    }
}

fn scoped_turn_position<'a>(
    event: &CanonicalTurnEvent,
    turns: &'a [CanonicalTurn],
    scope: ObservationScope<'_>,
) -> Option<(usize, &'a CanonicalTurn, ThreadId)> {
    let index = turn_position(event, turns)?;
    let turn = &turns[index];
    let thread_id = scoped_turn_thread_id(turn, scope)?;
    let number = turns[..=index]
        .iter()
        .filter(|candidate| scoped_turn_thread_id(candidate, scope).is_some())
        .count();
    Some((number, turn, thread_id))
}

/// 判断一条已提交尝试流是否以 `error` 或 `aborted` 结束（即没有产生助手消息的尝试）。
fn is_failed_attempt_stream(stream: &[Value]) -> bool {
    stream
        .iter()
        .rev()
        .find(|record| {
            record.get("type").and_then(Value::as_str) == Some("chunk")
                && record.pointer("/chunk/type").and_then(Value::as_str) == Some("finish")
        })
        .and_then(|record| record.pointer("/chunk/reason/kind").and_then(Value::as_str))
        .is_some_and(|kind| matches!(kind, "error" | "aborted"))
}

/// 解析主线 `Execution` 模型步骤的 1-based `(turn, step)` 编号；非主线或非 `Execution` 步骤返回 `None`。
pub(crate) fn main_execution_step_position(
    turns: &[CanonicalTurn],
    turn_id: &str,
    step_id: &str,
) -> Option<(usize, usize)> {
    let position = turns.iter().position(|turn| turn.turn_id == turn_id)?;
    let turn = &turns[position];
    let thread_id = scoped_turn_thread_id(turn, ObservationScope::Main)?;
    let number = turns[..=position]
        .iter()
        .filter(|candidate| scoped_turn_thread_id(candidate, ObservationScope::Main).is_some())
        .count();
    let step = step_number_for_thread(turn, &thread_id, step_id)?;
    Some((number, step))
}

#[derive(Debug)]
struct ActiveLiveAttempt {
    attempt_id: String,
    turn: usize,
    step: usize,
    next_index: u64,
    degraded: bool,
}

/// 单会话非持久化实时助手流追踪器。
/// 保证每条尝试的 `chunk` 序号从 0 严格连续；传输队列满或超帧后立即置 `degraded` 并发 `overflow`，
/// 不再向宿主发送跳号的 `chunk`。
#[derive(Default, Debug)]
pub(crate) struct LiveAttemptTracker {
    active: Option<ActiveLiveAttempt>,
}

impl LiveAttemptTracker {
    pub(crate) fn on_start(
        &mut self,
        attempt_id: &str,
        turn: usize,
        step: usize,
        after_event_seq: u64,
    ) -> Vec<Value> {
        let mut frames = Vec::new();
        if let Some(previous) = self.active.take()
            && !previous.degraded
        {
            frames.push(json!({
                "kind": "abandon",
                "attemptId": previous.attempt_id,
                "index": previous.next_index,
            }));
        }
        self.active = Some(ActiveLiveAttempt {
            attempt_id: attempt_id.to_string(),
            turn,
            step,
            next_index: 0,
            degraded: false,
        });
        frames.push(json!({
            "kind": "start",
            "attemptId": attempt_id,
            "turn": turn,
            "step": step,
            "afterEventSeq": after_event_seq,
        }));
        frames
    }

    pub(crate) fn on_chunk(&mut self, attempt_id: &str, time: u64, chunk: Value) -> Option<Value> {
        let active = self.active.as_mut()?;
        if active.attempt_id != attempt_id || active.degraded {
            return None;
        }
        let index = active.next_index;
        active.next_index = active.next_index.saturating_add(1);
        Some(json!({
            "kind": "chunk",
            "attemptId": attempt_id,
            "index": index,
            "time": time,
            "chunk": chunk,
        }))
    }

    pub(crate) fn on_abandon(&mut self, attempt_id: &str) -> Option<Value> {
        let matches = self
            .active
            .as_ref()
            .is_some_and(|active| active.attempt_id == attempt_id);
        if !matches {
            return None;
        }
        let active = self.active.take()?;
        if active.degraded {
            return None;
        }
        Some(json!({
            "kind": "abandon",
            "attemptId": active.attempt_id,
            "index": active.next_index,
        }))
    }

    /// 传输队列满或帧超限时标记当前尝试受损，并产出一次轻量 `overflow` 帧通知宿主结束实时流。
    pub(crate) fn mark_delivery_failed(&mut self, attempt_id: &str) -> Option<Value> {
        let active = self.active.as_mut()?;
        if active.attempt_id != attempt_id || active.degraded {
            return None;
        }
        active.degraded = true;
        Some(json!({
            "kind": "overflow",
            "attemptId": attempt_id,
        }))
    }

    /// 持久观察落盘后清理已结算或随回合关闭的活跃尝试跟踪。
    pub(crate) fn on_committed_observation(&mut self, observation: &Value) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let kind = observation.get("kind").and_then(Value::as_str);
        let turn = observation
            .get("turn")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let step = observation
            .get("step")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let settled = match kind {
            Some("assistantMessage" | "assistantAttempt") => {
                turn == Some(active.turn) && step == Some(active.step)
            }
            Some("turnEnded") => turn == Some(active.turn),
            _ => false,
        };
        if settled {
            self.active = None;
        }
    }
}

/// 指定线程内 `Execution` 执行步骤的编号（从 1 起），主线与子代理共用同一规则。
fn step_number_for_thread(
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
    step_id: &str,
) -> Option<usize> {
    turn.model_steps
        .iter()
        .filter(|step| {
            &step.thread_id == thread_id && step.purpose == Some(ModelStepPurpose::Execution)
        })
        .position(|step| step.step_id == step_id)
        .map(|position| position + 1)
}

/// 首次出现的执行步骤开始：模型请求在发送前已冻结并提交，这是步骤的开始事实。
fn committed_step_start(
    event: &CanonicalTurnEvent,
    previous: Option<&CanonicalTurn>,
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
) -> Option<usize> {
    let step = event.model_step.as_ref()?;
    if previous.is_some_and(|turn| {
        turn.model_steps
            .iter()
            .any(|old| old.step_id == step.step_id)
    }) {
        return None;
    }
    step_number_for_thread(turn, thread_id, &step.step_id)
}

/// 步骤“完成”：模型响应已终态落盘，且它请求的每个工具调用都有已提交的结果。
/// 没有请求工具的响应在落盘时即完成。失败、取消或结果未知的调用不会让步骤自然完成，
/// 由回合结束时收口。
fn step_completed_for_thread(turn: &CanonicalTurn, thread_id: &ThreadId, step_id: &str) -> bool {
    let Some(response) = turn.items.iter().find(|item| {
        item.kind == CanonicalTurnItemKind::AssistantText
            && &item.source_thread_id == thread_id
            && item.status == CanonicalTurnItemStatus::Completed
            && item.metadata.get("modelStepId").and_then(Value::as_str) == Some(step_id)
            && item
                .metadata
                .get("assistantOutputKind")
                .and_then(Value::as_str)
                != Some("error")
    }) else {
        return false;
    };
    let Some(calls) = response.metadata.get("toolCalls") else {
        return true;
    };
    let Some(calls) = calls.as_array() else {
        return false;
    };
    calls.iter().all(|call| {
        let Some(id) = call.get("id").and_then(Value::as_str) else {
            return false;
        };
        turn.items.iter().any(|item| {
            &item.source_thread_id == thread_id
                && item.tool.as_ref().is_some_and(|tool| tool.call_id == id)
                && item
                    .metadata
                    .get(TOOL_RESULT_COMMITTED_METADATA_KEY)
                    .and_then(Value::as_bool)
                    == Some(true)
        })
    })
}

/// 本次条目写入使条目所属步骤从未完成变为完成。
fn committed_step_end(
    item: &CanonicalTurnItem,
    previous: Option<&CanonicalTurn>,
    number: usize,
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
) -> Option<Value> {
    let step_id = item.metadata.get("modelStepId")?.as_str()?;
    let step = step_number_for_thread(turn, thread_id, step_id)?;
    let was_completed =
        previous.is_some_and(|turn| step_completed_for_thread(turn, thread_id, step_id));
    (step_completed_for_thread(turn, thread_id, step_id) && !was_completed)
        .then(|| json!({"kind": "stepEnded", "turn": number, "step": step}))
}

/// 回合结束时仍未自然完成的步骤编号（按编号升序）。
fn open_steps_for_thread(turn: &CanonicalTurn, thread_id: &ThreadId) -> Vec<usize> {
    turn.model_steps
        .iter()
        .filter(|step| {
            &step.thread_id == thread_id && step.purpose == Some(ModelStepPurpose::Execution)
        })
        .enumerate()
        .filter(|(_, step)| !step_completed_for_thread(turn, thread_id, &step.step_id))
        .map(|(position, _)| position + 1)
        .collect()
}

fn committed_text_input(
    event: &CanonicalTurnEvent,
    events: &[CanonicalTurnEvent],
    before_inputs: &[SessionInput],
) -> Option<Value> {
    let item = event.item.as_ref()?;
    if item.status != CanonicalTurnItemStatus::Completed
        || item.metadata.contains_key("pluginMessage")
        || item.metadata.contains_key(SESSION_COMMAND_METADATA_KEY)
        || item.metadata.contains_key("pluginCommandId")
        || !item.blocks.is_empty()
        || item
            .metadata
            .get("images")
            .and_then(Value::as_array)
            .is_some_and(|images| !images.is_empty())
    {
        return None;
    }
    let text = item.content.as_deref().filter(|text| !text.is_empty())?;
    let request_id = item.metadata.get("requestId")?.as_str()?;
    let message_id = item.metadata.get("userMessageId")?.as_str()?;
    let input = events
        .iter()
        .filter_map(|event| {
            (event.kind == CanonicalTurnEventKind::InputUpdated)
                .then_some(event.input.as_ref())
                .flatten()
        })
        .find(|input| {
            input.state == SessionInputState::Attached
                && input.session_id == event.session_id
                && input.attached_turn_id.as_deref() == Some(&event.turn_id)
                && input
                    .payload
                    .get("plugin_message")
                    .is_none_or(Value::is_null)
                && input.request_id == request_id
                && input.user_message_id == message_id
        })?;
    if before_inputs
        .iter()
        .find(|previous| previous.input_id == input.input_id)
        .is_some_and(|previous| previous.state != SessionInputState::Pending)
    {
        return None;
    }
    Some(json!({ "kind": "textInput", "messageId": message_id, "text": text }))
}

fn observes_main_turn(turn: &CanonicalTurn) -> bool {
    !turn.metadata.contains_key("agentExecution")
        && !turn.is_session_command()
        && turn
            .items
            .iter()
            .any(|item| item.kind == CanonicalTurnItemKind::UserMessage)
        && !turn
            .items
            .iter()
            .any(|item| item.metadata.contains_key("pluginCommandId"))
}

fn committed_turn_boundary(
    event: &CanonicalTurnEvent,
    previous: Option<&CanonicalTurn>,
    number: usize,
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
) -> Option<Value> {
    if event.kind == CanonicalTurnEventKind::TurnStarted {
        return Some(json!({"kind": "turnStarted", "turn": number}));
    }
    // A later write into a finished turn can produce a terminal snapshot. The
    // durable transition, rather than that snapshot kind alone, ends a turn.
    if previous.is_some_and(|previous| previous.status.is_terminal())
        || event.turn.as_ref()?.status != turn.status
    {
        return None;
    }
    let outcome = match turn.status {
        CanonicalTurnStatus::Completed => json!({"kind": "completed"}),
        CanonicalTurnStatus::Cancelled if turn.was_interrupted_by_user() => {
            json!({"kind": "userInterrupted"})
        }
        CanonicalTurnStatus::Failed => {
            let failure = turn.items.iter().rev().find(|item| {
                &item.source_thread_id == thread_id
                    && item
                        .metadata
                        .get("assistantOutputKind")
                        .and_then(Value::as_str)
                        == Some("error")
            })?;
            let diagnostic = failure.metadata.get("modelFailure");
            let code = diagnostic
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str);
            let message = diagnostic
                .and_then(|value| value.get("summary"))
                .and_then(Value::as_str)
                .or(failure.content.as_deref())?;
            json!({"kind": "failed", "message": message, "code": code})
        }
        _ => return None,
    };
    Some(json!({"kind": "turnEnded", "turn": number, "outcome": outcome}))
}

fn turn_position(event: &CanonicalTurnEvent, turns: &[CanonicalTurn]) -> Option<usize> {
    turns
        .binary_search_by(|turn| {
            turn.turn_seq
                .cmp(&event.turn_seq)
                .then_with(|| turn.turn_id.cmp(&event.turn_id))
        })
        .ok()
}

/// 主线模型响应的最终文本（含它请求的工具调用）：条目首次到达终态时发布一次。
///
/// 事实全部来自已提交的条目与模型步骤：步骤编号与工具事件同法（主线执行步骤的序号），
/// 模型身份取步骤绑定。缺任何一项（没有步骤、没有模型名、工具调用记录不完整）都不发布，
/// 不用占位值补。`stream` 取该步骤已提交的尝试流记录（没有记录就不带）；思考内容是另一个条目，
/// 不并入这条消息。
fn committed_assistant_message(
    event: &CanonicalTurnEvent,
    previous: Option<&CanonicalTurn>,
    turn_number: usize,
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
    pending: &mut PendingAttemptStreams,
) -> Option<Value> {
    let item = event.item.as_ref()?;
    if item.status != CanonicalTurnItemStatus::Completed
        || item
            .metadata
            .get("assistantOutputKind")
            .and_then(Value::as_str)
            == Some("error")
    {
        return None;
    }
    let was_terminal = previous
        .and_then(|turn| {
            turn.items
                .iter()
                .find(|candidate| candidate.item_id == item.item_id)
        })
        .is_some_and(|previous| previous.status.is_terminal());
    if was_terminal {
        return None;
    }
    if &item.source_thread_id != thread_id {
        return None;
    }
    let step_id = item.metadata.get("modelStepId")?.as_str()?;
    let steps: Vec<_> = turn
        .model_steps
        .iter()
        .filter(|step| {
            step.thread_id == item.source_thread_id
                && step.purpose == Some(ModelStepPurpose::Execution)
        })
        .collect();
    let position = steps.iter().position(|step| step.step_id == step_id)?;
    let binding = steps[position].binding.as_ref()?;
    let model = binding.model.as_deref().filter(|model| !model.is_empty())?;
    let provider = binding
        .provider
        .as_deref()
        .filter(|provider| !provider.is_empty())
        .unwrap_or(steps[position].provider.as_str());
    let mut calls = Vec::new();
    if let Some(recorded) = item.metadata.get("toolCalls") {
        for call in recorded.as_array()? {
            calls.push(json!({
                "id": call.get("id")?.as_str().filter(|id| !id.is_empty())?,
                "name": call.pointer("/function/name")?.as_str().filter(|name| !name.is_empty())?,
                "arguments": call.pointer("/function/arguments")?.as_str()?,
            }));
        }
    }
    let text = item.content.as_deref().unwrap_or_default();
    if text.is_empty() && calls.is_empty() {
        return None;
    }
    let mut observation = json!({
        "kind": "assistantMessage", "turn": turn_number, "step": position + 1,
        "messageId": item.item_id, "text": text, "toolCalls": calls,
        "provider": provider, "model": model,
    });
    if let Some(usage) = step_usage(steps[position]) {
        observation["usage"] = usage;
    }
    if let Some(stream) = pending.take(&event.turn_id, step_id) {
        observation["stream"] = json!(stream);
    }
    Some(observation)
}

/// 本步骤成功响应的用量：取最后一次成功传输回执（逐次回执是用量的权威来源，且在响应条目
/// 提交前已落盘）。口径与用量统计一致（`normalize_usage_delta`）：输入只含未命中缓存的部分，
/// 缓存读写单列，与 DSH 的互不重叠计数相同。没有成功回执或回执没有用量时不给，不估算。
fn step_usage(step: &magi_session_store::ModelStep) -> Option<Value> {
    let usage = step
        .attempts
        .iter()
        .rev()
        .find(|attempt| {
            attempt.transport.as_ref().is_some_and(|transport| {
                transport.outcome == Some(ModelTransportOutcome::Succeeded)
            })
        })?
        .transport
        .as_ref()?
        .usage
        .as_ref()?;
    let normalized = magi_usage_authority::costing::normalize_usage_delta(
        &magi_usage_authority::types::UsageEventUsageDelta {
            raw_input_tokens: usage.input_tokens,
            raw_output_tokens: usage.output_tokens,
            context_window_tokens: None,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cache_read_included_in_input: usage.cache_read_included_in_input,
        },
    );
    let mut value = json!({
        "inputTokens": normalized.net_input_tokens,
        "outputTokens": normalized.net_output_tokens,
    });
    if normalized.cache_observed_call_count == 1 && usage.cache_read_tokens.is_some() {
        value["cacheReadTokens"] = json!(normalized.cache_read_tokens);
    }
    if usage.cache_write_tokens.is_some() {
        value["cacheWriteTokens"] = json!(normalized.cache_write_tokens);
    }
    Some(value)
}

fn committed_model_tool_event(
    event: &CanonicalTurnEvent,
    previous: Option<&CanonicalTurn>,
    turn_number: usize,
    turn: &CanonicalTurn,
    thread_id: &ThreadId,
) -> Option<Value> {
    let item = event.item.as_ref()?;
    let tool = item.tool.as_ref()?;
    if &item.source_thread_id != thread_id {
        return None;
    }
    let step_id = item.metadata.get("modelStepId")?.as_str()?;
    let step_number = turn
        .model_steps
        .iter()
        .filter(|step| {
            step.thread_id == item.source_thread_id
                && step.purpose == Some(ModelStepPurpose::Execution)
        })
        .position(|step| step.step_id == step_id)?
        + 1;
    let previous = previous.and_then(|turn| {
        turn.items
            .iter()
            .find(|candidate| candidate.item_id == item.item_id)
    });
    match (previous, item.status) {
        (None, CanonicalTurnItemStatus::Running) => {
            // Reuse the argument string in the committed model response.
            // The tool item carries parsed JSON for display.
            let arguments = turn
                .items
                .iter()
                .filter(|response| {
                    response.kind == CanonicalTurnItemKind::AssistantText
                        && response.source_thread_id == item.source_thread_id
                        && response.metadata.get("modelStepId").and_then(Value::as_str)
                            == Some(step_id)
                })
                .filter_map(|response| response.metadata.get("toolCalls")?.as_array())
                .flatten()
                .find(|call| {
                    call.get("id").and_then(Value::as_str) == Some(tool.call_id.as_str())
                        && call.pointer("/function/name").and_then(Value::as_str)
                            == Some(tool.name.as_str())
                })?
                .pointer("/function/arguments")?
                .as_str()?;
            Some(
                json!({"kind": "toolCall", "turn": turn_number, "step": step_number,
                "callId": tool.call_id, "name": tool.name, "arguments": arguments}),
            )
        }
        (Some(previous), CanonicalTurnItemStatus::Completed | CanonicalTurnItemStatus::Failed)
            if !previous.status.is_terminal()
                && item
                    .metadata
                    .get(TOOL_RESULT_COMMITTED_METADATA_KEY)
                    .and_then(Value::as_bool)
                    == Some(true)
                && !previous
                    .metadata
                    .contains_key(TOOL_RESULT_COMMITTED_METADATA_KEY) =>
        {
            let result = tool.result.as_ref()?;
            let content = match result {
                Value::String(text) => text.clone(),
                _ => result.to_string(),
            };
            let failed = item.status == CanonicalTurnItemStatus::Failed;
            let mut observation = json!({"kind": "toolResult", "turn": turn_number, "step": step_number,
                "callId": tool.call_id, "messageId": item.item_id, "content": content, "failed": failed});
            if failed && let Some(code) = result.get("error_code").and_then(Value::as_str) {
                observation["error"] = json!({"name": "ToolError", "code": code});
                if let Some(reason) = result.get("error").and_then(Value::as_str) {
                    observation["error"]["reason"] = json!(reason);
                }
            }
            Some(observation)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_THREAD: &str = "thread-main";

    fn item(id: &str, kind: &str, status: &str, thread: &str, metadata: Value) -> Value {
        json!({
            "sessionId": "session-1", "turnId": "turn-1", "turnSeq": 1,
            "itemId": id, "itemSeq": 0, "kind": kind, "createdAt": 1, "status": status,
            "updatedAt": 1, "sourceThreadId": thread, "metadata": metadata,
        })
    }

    fn step(id: &str, thread: &str, model: Option<&str>) -> Value {
        json!({
            "stepId": id, "taskId": null, "threadId": thread, "checkpointGeneration": 0,
            "requestFingerprint": "fp", "provider": "step-provider", "messageCount": 1,
            "toolCount": 0, "inputRequestIds": [], "startedAt": 1, "settledAt": 2,
            "status": "response_received", "responseFingerprint": null, "attempts": [],
            "binding": model.map(|model| json!({
                "model": model, "provider": "bound-provider",
                "bindingRevision": 1, "settingsRevision": 1, "usageContext": null,
            })),
            "maxOutputTokens": null, "goalId": null, "usageTokens": null,
            "goalTokensAfter": null, "executorCapabilities": null,
            "purpose": "execution", "promptFrame": null, "cacheEpoch": 0,
        })
    }

    fn turn(items: Vec<Value>, steps: Vec<Value>) -> CanonicalTurn {
        serde_json::from_value(json!({
            "sessionId": "session-1", "turnId": "turn-1", "turnSeq": 1, "acceptedAt": 1,
            "status": "running", "items": items, "modelSteps": steps, "metadata": {},
        }))
        .expect("turn fixture")
    }

    fn upsert(item: &Value) -> CanonicalTurnEvent {
        serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1", "eventId": "event-1", "eventSeq": 1,
            "kind": "turn_item_upsert", "sessionId": "session-1", "turnId": "turn-1",
            "turnSeq": 1, "occurredAt": 1, "item": item,
        }))
        .expect("event fixture")
    }

    fn user() -> Value {
        item(
            "user-1",
            "user_message",
            "completed",
            MAIN_THREAD,
            json!({}),
        )
    }

    fn assistant(status: &str, text: &str, metadata: Value) -> Value {
        let mut value = item(
            "assistant-1",
            "assistant_text",
            status,
            MAIN_THREAD,
            metadata,
        );
        value["content"] = json!(text);
        value
    }

    fn observe(
        before: &[CanonicalTurn],
        after: &[CanonicalTurn],
        event: CanonicalTurnEvent,
    ) -> Vec<Value> {
        committed_observations(
            std::slice::from_ref(&event),
            before,
            &[],
            after,
            &mut PendingAttemptStreams::default(),
        )
        .map(|(_, observation)| observation)
        .filter(|observation| observation["kind"] == "assistantMessage")
        .collect()
    }

    fn observe_all(
        before: &[CanonicalTurn],
        after: &[CanonicalTurn],
        event: CanonicalTurnEvent,
    ) -> Vec<Value> {
        committed_observations(
            std::slice::from_ref(&event),
            before,
            &[],
            after,
            &mut PendingAttemptStreams::default(),
        )
        .map(|(_, observation)| observation)
        .collect()
    }

    fn kinds(observations: &[Value]) -> Vec<(String, u64)> {
        observations
            .iter()
            .map(|value| {
                (
                    value["kind"].as_str().unwrap().to_string(),
                    value["step"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    }

    fn step_event(step: &Value) -> CanonicalTurnEvent {
        serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1", "eventId": "event-s", "eventSeq": 1,
            "kind": "model_step_updated", "sessionId": "session-1", "turnId": "turn-1",
            "turnSeq": 1, "occurredAt": 1, "modelStep": step,
        }))
        .expect("step event fixture")
    }

    fn tool_item(id: &str, call_id: &str, step_id: &str, committed: bool) -> Value {
        let mut value = item(
            id,
            "tool_call",
            if committed { "completed" } else { "running" },
            MAIN_THREAD,
            if committed {
                json!({"modelStepId": step_id, TOOL_RESULT_COMMITTED_METADATA_KEY: true})
            } else {
                json!({"modelStepId": step_id})
            },
        );
        value["tool"] = json!({"callId": call_id, "name": "shell_exec", "result": "ok"});
        value
    }

    fn final_turn(message: Value) -> CanonicalTurn {
        turn(
            vec![user(), message],
            vec![
                step("step-other", "thread-worker", Some("other-model")),
                step("step-1", MAIN_THREAD, Some("model-a")),
            ],
        )
    }

    #[test]
    fn assistant_text_is_published_once_with_step_and_model_identity() {
        let message = assistant("completed", "你好", json!({"modelStepId": "step-1"}));
        let after = [final_turn(message.clone())];
        let observed = observe(&[], &after, upsert(&message));
        assert_eq!(
            observed,
            vec![json!({
                "kind": "assistantMessage", "turn": 1, "step": 1,
                "messageId": "assistant-1", "text": "你好", "toolCalls": [],
                "provider": "bound-provider", "model": "model-a",
            })]
        );
        // 终态条目的重复写入不再发布第二次。
        assert!(observe(&after, &after, upsert(&message)).is_empty());
    }

    fn step_with_receipt(usage: Value, outcome: &str) -> Value {
        let mut value = step("step-1", MAIN_THREAD, Some("model-a"));
        value["attempts"] = json!([
            // 先失败后成功的重试：只取成功的那一次。
            {"attemptId": "step-1:attempt:1", "startedAt": 1, "settledAt": 1, "status": "started",
             "transport": {"requestFingerprint": "a".repeat(64), "outcome": "failed",
                           "usage": {"inputTokens": 999, "outputTokens": 999,
                                     "cacheReadIncludedInInput": false}}},
            {"attemptId": "step-1:attempt:2", "startedAt": 2, "settledAt": 2, "status": "started",
             "transport": {"requestFingerprint": "a".repeat(64), "outcome": outcome, "usage": usage}},
        ]);
        value
    }

    fn observed_usage(usage: Value, outcome: &str) -> Option<Value> {
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let after = turn(
            vec![user(), message.clone()],
            vec![step_with_receipt(usage, outcome)],
        );
        observe(&[], &[after], upsert(&message))
            .pop()
            .and_then(|value| value.get("usage").cloned())
    }

    #[test]
    fn usage_uses_the_successful_receipt_in_the_disjoint_token_convention() {
        // 输入含缓存命中：未命中输入 = 输入 - 命中，命中与写入单列。
        assert_eq!(
            observed_usage(
                json!({"inputTokens": 30, "outputTokens": 9, "cacheReadTokens": 20,
                       "cacheWriteTokens": 2, "cacheReadIncludedInInput": true}),
                "succeeded"
            ),
            Some(json!({"inputTokens": 10, "outputTokens": 9,
                        "cacheReadTokens": 20, "cacheWriteTokens": 2}))
        );
        // 输入不含缓存：原样。
        assert_eq!(
            observed_usage(
                json!({"inputTokens": 30, "outputTokens": 9, "cacheReadTokens": 20,
                       "cacheReadIncludedInInput": false}),
                "succeeded"
            ),
            Some(json!({"inputTokens": 30, "outputTokens": 9, "cacheReadTokens": 20}))
        );
        // 没有缓存字段：只给输入输出。
        assert_eq!(
            observed_usage(
                json!({"inputTokens": 5, "outputTokens": 1, "cacheReadIncludedInInput": false}),
                "succeeded"
            ),
            Some(json!({"inputTokens": 5, "outputTokens": 1}))
        );
    }

    fn stream_event(step_id: &str, stream: Value) -> CanonicalTurnEvent {
        serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1", "eventId": "event-st", "eventSeq": 1,
            "kind": "model_attempt_stream_recorded", "sessionId": "session-1",
            "turnId": "turn-1", "turnSeq": 1, "occurredAt": 1,
            "attemptStream": {"stepId": step_id, "attemptId": "a1", "stream": stream},
        }))
        .expect("stream event fixture")
    }

    fn run(
        pending: &mut PendingAttemptStreams,
        before: &[CanonicalTurn],
        after: &[CanonicalTurn],
        event: CanonicalTurnEvent,
    ) -> Vec<Value> {
        committed_observations(std::slice::from_ref(&event), before, &[], after, pending)
            .map(|(_, observation)| observation)
            .collect()
    }

    #[test]
    fn the_assistant_message_takes_the_attempt_stream_committed_before_it_exactly_once() {
        let records = json!([{"type": "chunk", "time": 5,
            "chunk": {"type": "block-start", "index": 0, "blockType": "text"}}]);
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let turn = final_turn(message.clone());
        let mut pending = PendingAttemptStreams::default();
        // 流记录本身不产生观察，只被保留。
        assert!(
            run(
                &mut pending,
                &[],
                std::slice::from_ref(&turn),
                stream_event("step-1", records.clone())
            )
            .is_empty()
        );
        let first = run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            upsert(&message),
        );
        let observed = first
            .iter()
            .find(|value| value["kind"] == "assistantMessage")
            .unwrap();
        assert_eq!(observed["stream"], records);
        // 领走后不再有：同一步骤的重复写入也不重复发布流。
        let again = run(
            &mut pending,
            std::slice::from_ref(&turn),
            std::slice::from_ref(&turn),
            upsert(&message),
        );
        assert!(again.iter().all(|value| value.get("stream").is_none()));
    }

    #[test]
    fn failed_and_aborted_attempt_streams_emit_assistant_attempt_before_later_success() {
        let failed_stream = json!([
            {"type": "chunk", "time": 10, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}},
            {"type": "text-chunks", "time0": 11, "index": 0, "dt": [], "texts": ["半截"]},
            {"type": "chunk", "time": 20, "chunk": {
                "type": "finish",
                "reason": {
                    "kind": "error",
                    "failure": {"message": "provider rejected request", "code": "provider_http_503", "status": 503}
                }
            }},
        ]);
        let aborted_stream = json!([
            {"type": "chunk", "time": 30, "chunk": {
                "type": "finish",
                "reason": {
                    "kind": "aborted",
                    "failure": {"message": "model invocation cancelled", "code": "CANCELLED"}
                }
            }},
        ]);
        let succeeded_stream = json!([
            {"type": "chunk", "time": 40, "chunk": {"type": "block-start", "index": 0, "blockType": "text"}},
            {"type": "text-chunks", "time0": 41, "index": 0, "dt": [], "texts": ["好"]},
            {"type": "chunk", "time": 42, "chunk": {"type": "block-end", "index": 0, "block": {"type": "text", "text": "好"}}},
            {"type": "chunk", "time": 43, "chunk": {"type": "finish", "reason": {"kind": "stop"}}},
        ]);
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let turn = turn(
            vec![user(), message.clone()],
            vec![
                step("step-1", MAIN_THREAD, Some("model-a")),
                step("step-w", "thread-worker", Some("model-w")),
            ],
        );
        let mut pending = PendingAttemptStreams::default();
        // 工作线程的失败尝试不发布。
        assert!(
            run(
                &mut pending,
                &[],
                std::slice::from_ref(&turn),
                stream_event("step-w", failed_stream.clone())
            )
            .is_empty()
        );
        // 尝试 1（失败）立刻发出 assistantAttempt，不进待配对。
        let first_attempt = run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            stream_event("step-1", failed_stream.clone()),
        );
        assert_eq!(
            first_attempt,
            vec![json!({
                "kind": "assistantAttempt",
                "turn": 1,
                "step": 1,
                "stream": failed_stream,
            })]
        );
        // 尝试 2（取消）也立刻发出 assistantAttempt。
        let second_attempt = run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            stream_event("step-1", aborted_stream.clone()),
        );
        assert_eq!(
            second_attempt,
            vec![json!({
                "kind": "assistantAttempt",
                "turn": 1,
                "step": 1,
                "stream": aborted_stream,
            })]
        );
        // 尝试 3（成功）进待配对，随后的 assistantMessage 领走成功流，不受前面失败流污染。
        assert!(
            run(
                &mut pending,
                &[],
                std::slice::from_ref(&turn),
                stream_event("step-1", succeeded_stream.clone())
            )
            .is_empty()
        );
        let observed = run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            upsert(&message),
        );
        let assistant_msg = observed
            .iter()
            .find(|value| value["kind"] == "assistantMessage")
            .unwrap();
        assert_eq!(assistant_msg["stream"], succeeded_stream);
    }

    #[test]
    fn streams_of_worker_threads_and_finished_turns_are_not_kept() {
        let records = json!([{"type": "chunk", "time": 5, "chunk": {"type": "finish"}}]);
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let turn = turn(
            vec![user(), message.clone()],
            vec![
                step("step-1", MAIN_THREAD, Some("model-a")),
                step("step-w", "thread-worker", Some("model-w")),
            ],
        );
        let mut pending = PendingAttemptStreams::default();
        // 工作线程的步骤不属于主线：流记录不进入待配对。
        run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            stream_event("step-w", records.clone()),
        );
        run(
            &mut pending,
            &[],
            std::slice::from_ref(&turn),
            stream_event("step-1", records.clone()),
        );
        // 回合结束丢弃没被领走的流记录。
        let mut ended = turn.clone();
        ended.status = CanonicalTurnStatus::Completed;
        let mut shell = ended.clone();
        shell.items.clear();
        shell.model_steps.clear();
        let finish: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1", "eventId": "event-e", "eventSeq": 2,
            "kind": "turn_completed", "sessionId": "session-1", "turnId": "turn-1",
            "turnSeq": 1, "occurredAt": 1, "turn": shell,
        }))
        .unwrap();
        run(
            &mut pending,
            std::slice::from_ref(&turn),
            std::slice::from_ref(&ended),
            finish,
        );
        let late = run(
            &mut pending,
            &[],
            std::slice::from_ref(&ended),
            upsert(&message),
        );
        assert!(late.iter().all(|value| value.get("stream").is_none()));
    }

    #[test]
    fn usage_is_absent_without_a_successful_receipt_and_never_estimated() {
        let usage = json!({"inputTokens": 5, "outputTokens": 1, "cacheReadIncludedInInput": false});
        assert_eq!(observed_usage(usage.clone(), "failed"), None);
        assert_eq!(observed_usage(usage, "cancelled"), None);
        // 完全没有回执（默认夹具）。
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let observed = observe(&[], &[final_turn(message.clone())], upsert(&message));
        assert!(observed[0].get("usage").is_none());
    }

    #[test]
    fn assistant_tool_calls_come_from_recorded_calls_and_count_empty_text() {
        let calls = json!([{"id": "call-1", "type": "function",
            "function": {"name": "shell_exec", "arguments": "{\"command\":\"ls\"}"}}]);
        let message = assistant(
            "completed",
            "",
            json!({"modelStepId": "step-1", "toolCalls": calls}),
        );
        let observed = observe(&[], &[final_turn(message.clone())], upsert(&message));
        assert_eq!(observed[0]["text"], "");
        assert_eq!(
            observed[0]["toolCalls"],
            json!([{"id": "call-1", "name": "shell_exec", "arguments": "{\"command\":\"ls\"}"}])
        );
    }

    #[test]
    fn assistant_message_is_withheld_without_complete_facts() {
        let cases = [
            // 流式未结束
            assistant("running", "x", json!({"modelStepId": "step-1"})),
            // 没有步骤关联
            assistant("completed", "x", json!({})),
            // 没有任何内容
            assistant("completed", "", json!({"modelStepId": "step-1"})),
            // 错误结果文本不是模型消息
            assistant(
                "completed",
                "x",
                json!({"modelStepId": "step-1", "assistantOutputKind": "error"}),
            ),
            // 工具调用记录不完整
            assistant(
                "completed",
                "x",
                json!({"modelStepId": "step-1", "toolCalls": [{"id": "c"}]}),
            ),
            // 步骤不存在
            assistant("completed", "x", json!({"modelStepId": "missing"})),
        ];
        for message in cases {
            let after = [final_turn(message.clone())];
            assert!(
                observe(&[], &after, upsert(&message)).is_empty(),
                "{message}"
            );
        }
    }

    #[test]
    fn assistant_message_requires_model_binding_and_main_thread() {
        let message = assistant("completed", "x", json!({"modelStepId": "step-1"}));
        let unbound = turn(
            vec![user(), message.clone()],
            vec![step("step-1", MAIN_THREAD, None)],
        );
        assert!(observe(&[], &[unbound], upsert(&message)).is_empty());

        let mut worker = assistant("completed", "x", json!({"modelStepId": "step-w"}));
        worker["sourceThreadId"] = json!("thread-worker");
        let after = turn(
            vec![user(), worker.clone()],
            vec![step("step-w", "thread-worker", Some("model-w"))],
        );
        assert!(observe(&[], &[after], upsert(&worker)).is_empty());
    }

    #[test]
    fn a_new_main_step_starts_once_with_its_number() {
        let first = step("step-1", MAIN_THREAD, Some("model-a"));
        let second = step("step-2", MAIN_THREAD, Some("model-a"));
        let worker = step("step-w", "thread-worker", Some("model-w"));
        let after = turn(
            vec![user()],
            vec![first.clone(), second.clone(), worker.clone()],
        );
        let before = turn(vec![user()], vec![first.clone()]);
        // 第二个主线步骤首次出现：编号 2。
        assert_eq!(
            kinds(&observe_all(
                std::slice::from_ref(&before),
                std::slice::from_ref(&after),
                step_event(&second)
            )),
            [("stepStarted".to_string(), 2)]
        );
        // 已记录的步骤再次更新不重复开始。
        assert!(
            observe_all(
                std::slice::from_ref(&after),
                std::slice::from_ref(&after),
                step_event(&second)
            )
            .is_empty()
        );
        // 工作线程的步骤不属于主线。
        assert!(observe_all(&[before], &[after], step_event(&worker)).is_empty());
    }

    #[test]
    fn a_step_without_tools_ends_when_its_response_completes() {
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let running = assistant("running", "好", json!({"modelStepId": "step-1"}));
        let before = turn(
            vec![user(), running],
            vec![step("step-1", MAIN_THREAD, Some("model-a"))],
        );
        let after = final_turn(message.clone());
        let observed = observe_all(&[before], &[after], upsert(&message));
        assert_eq!(
            kinds(&observed),
            [
                ("assistantMessage".to_string(), 1),
                ("stepEnded".to_string(), 1)
            ]
        );
    }

    #[test]
    fn a_step_with_tools_ends_only_after_every_result_is_committed() {
        let calls = json!([
            {"id": "c1", "type": "function", "function": {"name": "shell_exec", "arguments": "{}"}},
            {"id": "c2", "type": "function", "function": {"name": "shell_exec", "arguments": "{}"}},
        ]);
        let response = assistant(
            "completed",
            "",
            json!({"modelStepId": "step-1", "toolCalls": calls}),
        );
        let steps = vec![step("step-1", MAIN_THREAD, Some("model-a"))];
        let turn_with = |first: bool, second: bool| {
            turn(
                vec![
                    user(),
                    response.clone(),
                    tool_item("t1", "c1", "step-1", first),
                    tool_item("t2", "c2", "step-1", second),
                ],
                steps.clone(),
            )
        };
        // 响应落盘时两个工具都还没有结果：步骤未完成。
        let started = turn_with(false, false);
        let observed = observe_all(&[], std::slice::from_ref(&started), upsert(&response));
        assert_eq!(kinds(&observed), [("assistantMessage".to_string(), 1)]);
        // 第一个结果提交：仍有一个未完成。
        let one_done = turn_with(true, false);
        let first = tool_item("t1", "c1", "step-1", true);
        let observed = observe_all(&[started], std::slice::from_ref(&one_done), upsert(&first));
        assert!(observed.iter().all(|value| value["kind"] != "stepEnded"));
        // 最后一个结果提交：先发工具结果，再收口步骤。
        let all_done = turn_with(true, true);
        let last = tool_item("t2", "c2", "step-1", true);
        let observed = observe_all(&[one_done], &[all_done], upsert(&last));
        assert_eq!(
            kinds(&observed),
            [("toolResult".to_string(), 1), ("stepEnded".to_string(), 1)]
        );
    }

    #[test]
    fn the_turn_end_closes_steps_that_never_completed_naturally() {
        // 步骤 1 自然完成，步骤 2 只有开始记录（失败或被中断）。
        let message = assistant("completed", "好", json!({"modelStepId": "step-1"}));
        let steps = vec![
            step("step-1", MAIN_THREAD, Some("model-a")),
            step("step-2", MAIN_THREAD, Some("model-a")),
        ];
        let mut ended = turn(vec![user(), message], steps);
        ended.status = CanonicalTurnStatus::Completed;
        let mut shell = ended.clone();
        shell.items.clear();
        shell.model_steps.clear();
        let event: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1", "eventId": "event-e", "eventSeq": 1,
            "kind": "turn_completed", "sessionId": "session-1", "turnId": "turn-1",
            "turnSeq": 1, "occurredAt": 1, "turn": shell,
        }))
        .expect("turn event fixture");
        let mut before = ended.clone();
        before.status = CanonicalTurnStatus::Running;
        let observed = observe_all(&[before], &[ended], event);
        assert_eq!(
            kinds(&observed),
            [("stepEnded".to_string(), 2), ("turnEnded".to_string(), 0)]
        );
    }

    #[test]
    fn seeded_pending_matches_full_replay_after_restart() {
        let header_v1 = magi_session_store::ModelRequestHeader {
            config: magi_session_store::ModelRequestCallConfig {
                provider: "openai".to_string(),
                model: "gpt-4.1".to_string(),
                temperature: None,
                max_tokens: Some(4096),
                reasoning_effort: None,
                stop: Vec::new(),
            },
            tools: Vec::new(),
        };
        let mut header_v2 = header_v1.clone();
        header_v2
            .tools
            .push(magi_session_store::ModelRequestToolSchema {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object"}),
            });
        let sys_sha = magi_session_store::sha256_hex_bytes(b"You are Magi.");
        let turn_after = turn(
            vec![user()],
            vec![
                step("step-1", MAIN_THREAD, Some("gpt-4.1")),
                step("step-2", MAIN_THREAD, Some("gpt-4.1")),
            ],
        );
        let event_v2: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1",
            "eventId": "event-req-2",
            "eventSeq": 2,
            "kind": "model_request_recorded",
            "sessionId": "session-1",
            "turnId": "turn-1",
            "turnSeq": 1,
            "occurredAt": 2,
            "modelRequest": {
                "stepId": "step-2",
                "systemPromptSha256": sys_sha,
                "headerSha256": header_v2.sha256_hex(),
                "header": header_v2,
                "contextWindow": 128000,
                "redacted": false,
            }
        }))
        .expect("model request event fixture");

        // 重启后新 PendingAttemptStreams 用检查点里的 v1 播种。
        let mut seeded = PendingAttemptStreams::default();
        seeded.seed_latest_request(Some(&LatestModelRequestState {
            header_sha256: header_v1.sha256_hex(),
            system_prompt_sha256: sys_sha,
            provider: "openai".to_string(),
            model: "gpt-4.1".to_string(),
            context_window: Some(128000),
        }));
        let observed: Vec<Value> = committed_observations(
            std::slice::from_ref(&event_v2),
            std::slice::from_ref(&turn_after),
            &[],
            std::slice::from_ref(&turn_after),
            &mut seeded,
        )
        .map(|(_, value)| value)
        .collect();
        // 只改了 tools，provider/model/contextWindow 与系统提示均未变：
        // 只应发 requestHeader(change)，不重复发 requestContext，也不把 reason 误标为 initial。
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["kind"], "requestHeader");
        assert_eq!(observed[0]["reason"], "change");
    }

    #[test]
    fn live_attempt_tracker_enforces_contiguous_indices_backpressure_and_settlement() {
        // 1. 主线步骤与子代理过滤。
        let worker_step = step("step-worker", "thread-worker", Some("gpt-4.1"));
        let mut compaction = step("step-compact", MAIN_THREAD, Some("gpt-4.1"));
        compaction["purpose"] = json!("context_compaction");
        let main_step = step("step-main", MAIN_THREAD, Some("gpt-4.1"));
        let turns = vec![turn(vec![user()], vec![worker_step, compaction, main_step])];
        assert_eq!(
            main_execution_step_position(&turns, "turn-1", "step-worker"),
            None
        );
        assert_eq!(
            main_execution_step_position(&turns, "turn-1", "step-compact"),
            None
        );
        assert_eq!(
            main_execution_step_position(&turns, "turn-1", "step-main"),
            Some((1, 1))
        );

        // 2. 正常连续 chunk 与持久结算后静默忽略迟到 abandon。
        let mut tracker = LiveAttemptTracker::default();
        assert_eq!(
            tracker.on_start("step-1:attempt:1", 1, 1, 10),
            vec![json!({
                "kind": "start",
                "attemptId": "step-1:attempt:1",
                "turn": 1,
                "step": 1,
                "afterEventSeq": 10,
            })]
        );
        assert_eq!(
            tracker.on_chunk(
                "step-1:attempt:1",
                1000,
                json!({"type": "block-start", "index": 0, "blockType": "text"})
            ),
            Some(json!({
                "kind": "chunk",
                "attemptId": "step-1:attempt:1",
                "index": 0,
                "time": 1000,
                "chunk": {"type": "block-start", "index": 0, "blockType": "text"},
            }))
        );
        assert_eq!(
            tracker.on_chunk(
                "step-1:attempt:1",
                1010,
                json!({"type": "text-delta", "index": 0, "text": "你"})
            ),
            Some(json!({
                "kind": "chunk",
                "attemptId": "step-1:attempt:1",
                "index": 1,
                "time": 1010,
                "chunk": {"type": "text-delta", "index": 0, "text": "你"},
            }))
        );
        tracker.on_committed_observation(&json!({
            "kind": "assistantMessage",
            "turn": 1,
            "step": 1,
        }));
        assert_eq!(tracker.on_abandon("step-1:attempt:1"), None);

        // 3. 投递失败（背压）后产生一次 overflow 并停止后续 chunk，不跳号。
        let _ = tracker.on_start("step-1:attempt:2", 1, 1, 12);
        assert!(
            tracker
                .on_chunk("step-1:attempt:2", 2000, json!({"type": "text-delta"}))
                .is_some()
        );
        assert_eq!(
            tracker.mark_delivery_failed("step-1:attempt:2"),
            Some(json!({"kind": "overflow", "attemptId": "step-1:attempt:2"}))
        );
        assert_eq!(tracker.mark_delivery_failed("step-1:attempt:2"), None);
        assert_eq!(
            tracker.on_chunk("step-1:attempt:2", 2010, json!({"type": "text-delta"})),
            None
        );
        assert_eq!(tracker.on_abandon("step-1:attempt:2"), None);

        // 4. 上一个未降级尝试未结算就被新尝试取代时，先自动发 abandon 再发新 start。
        let _ = tracker.on_start("step-2:attempt:1", 1, 2, 15);
        assert!(
            tracker
                .on_chunk("step-2:attempt:1", 3000, json!({"type": "text-delta"}))
                .is_some()
        );
        assert_eq!(
            tracker.on_start("step-2:attempt:2", 1, 2, 16),
            vec![
                json!({"kind": "abandon", "attemptId": "step-2:attempt:1", "index": 1}),
                json!({
                    "kind": "start",
                    "attemptId": "step-2:attempt:2",
                    "turn": 1,
                    "step": 2,
                    "afterEventSeq": 16,
                })
            ]
        );
    }

    #[test]
    fn subagent_turns_emit_parent_catalog_and_isolated_readonly_subagent_history() {
        let parent_turn = turn(vec![user()], vec![]);
        let sub_binding = json!({
            "taskId": "task-sub-1",
            "rootTaskId": "task-root",
            "parentTaskId": "task-root",
            "parentTurnId": "turn-1",
            "threadId": "thread-sub-1",
            "missionId": "mission-1",
            "workerId": "worker-1",
            "roleId": "researcher",
        });
        let sub_turn_start: CanonicalTurn = serde_json::from_value(json!({
            "sessionId": "session-1",
            "turnId": "turn-agent-task-sub-1",
            "turnSeq": 2,
            "acceptedAt": 2000,
            "status": "running",
            "items": [],
            "modelSteps": [],
            "metadata": { "agentExecution": sub_binding },
        }))
        .expect("subagent start turn fixture");
        let sub_start_event: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1",
            "eventId": "event-sub-start",
            "eventSeq": 2,
            "kind": "turn_started",
            "sessionId": "session-1",
            "turnId": "turn-agent-task-sub-1",
            "turnSeq": 2,
            "occurredAt": 2000,
            "turn": sub_turn_start,
        }))
        .expect("subagent start event fixture");

        let after_start = vec![parent_turn.clone(), sub_turn_start.clone()];
        let mut main_pending = PendingAttemptStreams::default();
        let main_obs: Vec<Value> = committed_observations(
            std::slice::from_ref(&sub_start_event),
            std::slice::from_ref(&parent_turn),
            &[],
            &after_start,
            &mut main_pending,
        )
        .map(|(_, value)| value)
        .collect();
        assert_eq!(
            main_obs,
            vec![json!({
                "kind": "subagentCatalog",
                "childId": "task-sub-1",
                "childCreatedAt": 2000,
                "mode": "one-shot",
                "label": "researcher",
            })]
        );

        let mut sub_pending = PendingAttemptStreams::default();
        let sub_obs: Vec<Value> = committed_observations_for_scope(
            std::slice::from_ref(&sub_start_event),
            std::slice::from_ref(&parent_turn),
            &[],
            &after_start,
            &mut sub_pending,
            ObservationScope::Subagent {
                task_id: "task-sub-1",
            },
        )
        .map(|(_, value)| value)
        .collect();
        assert_eq!(
            sub_obs,
            vec![
                json!({
                    "kind": "subagentDescriptor",
                    "version": 3,
                    "mode": "one-shot",
                    "provider": "magi",
                    "label": "researcher",
                }),
                json!({"kind": "turnStarted", "turn": 1}),
            ]
        );

        // 子代理执行一步并回复，只在子代理作用域产出，主线作用域完全隔离。
        let mut sub_step = step("sub-step-1", "thread-sub-1", Some("gpt-4.1"));
        sub_step["taskId"] = json!("task-sub-1");
        let sub_reply = json!({
            "sessionId": "session-1", "turnId": "turn-agent-task-sub-1", "turnSeq": 2,
            "itemId": "sub-msg-1", "itemSeq": 1, "kind": "assistant_text",
            "createdAt": 2010, "updatedAt": 2010, "status": "completed",
            "content": "调研完成", "sourceThreadId": "thread-sub-1",
            "worker": { "taskId": "task-sub-1", "roleId": "researcher", "title": "代码调研" },
            "metadata": { "modelStepId": "sub-step-1" },
        });
        let sub_turn_after_reply: CanonicalTurn = serde_json::from_value(json!({
            "sessionId": "session-1",
            "turnId": "turn-agent-task-sub-1",
            "turnSeq": 2,
            "acceptedAt": 2000,
            "status": "running",
            "items": [sub_reply],
            "modelSteps": [sub_step],
            "metadata": { "agentExecution": sub_binding },
        }))
        .expect("subagent reply turn fixture");
        let sub_reply_event: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1",
            "eventId": "event-sub-reply",
            "eventSeq": 3,
            "kind": "turn_item_upsert",
            "sessionId": "session-1",
            "turnId": "turn-agent-task-sub-1",
            "turnSeq": 2,
            "occurredAt": 2010,
            "item": sub_reply,
        }))
        .expect("subagent reply event fixture");
        let after_reply = vec![parent_turn.clone(), sub_turn_after_reply];
        assert!(
            committed_observations(
                std::slice::from_ref(&sub_reply_event),
                &after_start,
                &[],
                &after_reply,
                &mut main_pending,
            )
            .next()
            .is_none(),
            "subagent assistant messages must never leak into parent main observations"
        );
        let sub_reply_obs: Vec<Value> = committed_observations_for_scope(
            std::slice::from_ref(&sub_reply_event),
            &after_start,
            &[],
            &after_reply,
            &mut sub_pending,
            ObservationScope::Subagent {
                task_id: "task-sub-1",
            },
        )
        .map(|(_, value)| value)
        .collect();
        assert_eq!(
            sub_reply_obs
                .iter()
                .map(|v| v["kind"].as_str())
                .collect::<Vec<_>>(),
            vec![Some("assistantMessage"), Some("stepEnded")]
        );
        assert_eq!(sub_reply_obs[0]["turn"], 1);
        assert_eq!(sub_reply_obs[0]["step"], 1);
        assert_eq!(sub_reply_obs[0]["text"], "调研完成");

        // 穿插子代理 Turn 后，主线下一回合仍连续编号为 turn 2（不跳号）。
        let mut main_turn_2 = turn(vec![user()], vec![]);
        main_turn_2.turn_id = "turn-2".into();
        main_turn_2.turn_seq = 3;
        let main_turn_2_event: CanonicalTurnEvent = serde_json::from_value(json!({
            "schemaVersion": "canonical-turn.v1",
            "eventId": "event-main-2",
            "eventSeq": 4,
            "kind": "turn_started",
            "sessionId": "session-1",
            "turnId": "turn-2",
            "turnSeq": 3,
            "occurredAt": 3000,
            "turn": main_turn_2,
        }))
        .expect("main turn 2 event fixture");
        let after_main_2 = vec![after_reply[0].clone(), after_reply[1].clone(), main_turn_2];
        let main_turn_2_obs: Vec<Value> = committed_observations(
            std::slice::from_ref(&main_turn_2_event),
            &after_reply,
            &[],
            &after_main_2,
            &mut main_pending,
        )
        .map(|(_, value)| value)
        .collect();
        assert_eq!(
            main_turn_2_obs,
            vec![json!({"kind": "turnStarted", "turn": 2})]
        );
    }
}

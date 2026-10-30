//! 子代理终态回执。
//!
//! 终态与最终答复从子任务和状态一起提交的 `output_refs` 构造：完成时它是
//! `{ "blocks": [工具调用记录..., { "type": "text", "content": 最终答复 }] }`，
//! 失败或终止时是公开的失败原因。主线等待结果不再读取运行期线程快照，避免把
//! 中间过程文字当成最终结论。外部执行器失败/取消的部分证据由调用方从原
//! canonical Step/item 补充；它不改变 Task 结果，也不授予恢复执行资格。
//!
//! 执行事实（实际运行的命令及退出码、改动过的文件、工具失败次数）由运行时从工具
//! 调用记录与标准执行器证据提取，不依赖子代理在最终答复里自述。

use std::collections::BTreeSet;

use magi_core::{
    Task, TaskStatus, classify_public_task_failure, public_task_failure_is_degraded,
    public_task_output_refs,
};
use serde_json::{Value, json};

/// 一次等待或取消回执中所有代理最终答复共享的展示上限（字节），不限制执行。
pub(crate) const AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES: usize = 32 * 1024;
/// 回执中最多列出的命令条数，保留最近的命令。
const RECEIPT_MAX_COMMANDS: usize = 12;
/// 回执中最多列出的改动文件数。
const RECEIPT_MAX_FILES: usize = 40;
const RECEIPT_COMMAND_MAX_CHARS: usize = 200;
const EXECUTOR_OUTPUT_MAX_BYTES: usize = 8 * 1024;
const AGENT_UNAVAILABLE_PUBLIC_TEXT: &str =
    "代理当前不可用，主线需核对已有结果并接管或重新评估委派。";

/// 子代理实际执行过的命令事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandFact {
    pub command: String,
    pub exit_code: Option<i64>,
    pub status: String,
    pub executor: Option<String>,
}

/// 从工具调用记录提取的执行事实。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentActivity {
    pub tool_calls: usize,
    pub failed_tool_calls: usize,
    pub commands: Vec<CommandFact>,
    pub files_changed: BTreeSet<String>,
    pub external_commands: usize,
}

impl AgentActivity {
    fn to_json(&self) -> Value {
        let omitted_commands = self.commands.len().saturating_sub(RECEIPT_MAX_COMMANDS);
        let commands = self
            .commands
            .iter()
            .skip(omitted_commands)
            .map(|command| {
                json!({
                    "command": command.command,
                    "exit_code": command.exit_code,
                    "status": command.status,
                    "source": if command.executor.is_some() { "executor_owned" } else { "magi_tool" },
                    "executor": command.executor,
                })
            })
            .collect::<Vec<_>>();
        let files = self
            .files_changed
            .iter()
            .take(RECEIPT_MAX_FILES)
            .cloned()
            .collect::<Vec<_>>();
        json!({
            "tool_calls": self.tool_calls,
            "failed_tool_calls": self.failed_tool_calls,
            "external_commands": self.external_commands,
            "commands": commands,
            "omitted_commands": omitted_commands,
            "files_changed": files,
            "omitted_files": self.files_changed.len().saturating_sub(RECEIPT_MAX_FILES),
        })
    }
}

/// 子代理终态回执。最终答复的展示长度由调用方统一分配预算后再写入 JSON。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentReceipt {
    payload: Value,
    final_text: String,
}

impl AgentReceipt {
    /// 子任务未进入终态时返回 `None`。
    pub(crate) fn from_terminal_task(child: &Task) -> Option<Self> {
        let role = child.executor_binding_target_role().unwrap_or("agent");
        let base = |status: &str, child_status: &str| {
            json!({
                "status": status,
                "child_status": child_status,
                "child_task_id": child.task_id.to_string(),
                "activation": child.agent_activation(),
                "role": role,
                "title": child.title,
                "assignment": {
                    "title": child.title,
                    "goal": child.goal,
                    "role": role,
                },
            })
        };
        match child.status {
            TaskStatus::Pending | TaskStatus::Running => None,
            TaskStatus::Completed => {
                let (final_text, mut activity, workspace) = completed_output(&child.output_refs);
                let executor_receipts =
                    external_executor_receipts(&child.evidence_refs, &mut activity);
                let mut payload = base("completed", "completed");
                payload["activity"] = activity.to_json();
                if !executor_receipts.is_empty() {
                    payload["executor_receipts"] = Value::Array(executor_receipts);
                }
                if let Some(workspace) = workspace {
                    let instruction = json!(
                        "这些改动已提交到代理分支，尚未进入直接父任务的工作树。直接父任务需要采用时调用 agent_apply(task_id)；嵌套产出逐级合入，不采用时无需处理。"
                    );
                    if workspace.is_array() {
                        payload["workspaces"] = workspace;
                        payload["workspace_instruction"] = instruction;
                    } else {
                        let mut workspace = workspace;
                        workspace["instruction"] = instruction;
                        payload["workspace"] = workspace;
                    }
                }
                Some(Self {
                    payload,
                    final_text: final_text.unwrap_or_else(|| "代理未返回最终答复".to_string()),
                })
            }
            TaskStatus::Failed => {
                let public_refs = public_task_output_refs(TaskStatus::Failed, &child.output_refs);
                let error = public_refs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "代理任务执行失败".to_string());
                if public_refs
                    .iter()
                    .any(|output| public_task_failure_is_degraded(output))
                {
                    let mut payload = base("degraded", "failed");
                    payload["failure_stage"] = json!("dispatch");
                    payload["error_code"] = json!("agent_unavailable");
                    payload["fallback_mode"] = json!("mainline_or_reassign");
                    payload["error"] = json!("代理当前不可用");
                    payload["instruction"] =
                        json!(magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE);
                    return Some(Self {
                        payload,
                        final_text: AGENT_UNAVAILABLE_PUBLIC_TEXT.to_string(),
                    });
                }
                let failure = classify_public_task_failure(&error);
                let mut payload = base("failed", "failed");
                payload["failure_stage"] = json!(failure.failure_stage);
                payload["error_code"] = json!(failure.error_code);
                payload["fallback_mode"] = json!(failure.fallback_mode);
                payload["error"] = json!(error);
                payload["instruction"] =
                    json!(magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE);
                let final_text = public_refs.join("\n\n");
                Some(Self {
                    payload,
                    final_text,
                })
            }
            TaskStatus::Killed => {
                let mut payload = base("failed", "killed");
                payload["failure_stage"] = json!("cancellation");
                payload["error_code"] = json!("agent_killed");
                payload["fallback_mode"] = json!("mainline_or_reassign");
                payload["error"] = json!("代理任务被终止");
                payload["instruction"] =
                    json!(magi_tool_runtime::AGENT_DELEGATION_RECOVERY_GUIDANCE);
                let final_text = child
                    .output_refs
                    .iter()
                    .map(|output| output.trim())
                    .filter(|output| !output.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Some(Self {
                    payload,
                    final_text,
                })
            }
        }
    }

    pub(crate) fn child_status(&self) -> &str {
        self.payload["child_status"].as_str().unwrap_or_default()
    }

    pub(crate) fn with_executor_failures(
        mut self,
        failures: Vec<(String, magi_core::ExecutorFailureEvidence)>,
    ) -> Self {
        let mut activity = AgentActivity::default();
        let mut evidence_refs = Vec::new();
        let mut sources = Vec::new();
        for (step, evidence) in failures {
            sources
                .push(json!({"model_step_id":step,"resources_settled":evidence.resources_settled}));
            evidence_refs.extend(evidence.receipts.into_iter().map(|receipt| {
                serde_json::to_string(&magi_core::TaskCompletionEvidence::ExternalExecutor {
                    model_step_id: step.clone(),
                    receipt,
                })
                .expect("executor evidence serializes")
            }));
        }
        if !sources.is_empty() {
            self.payload["executor_failures"] = Value::Array(sources);
            let failures = external_executor_receipts(&evidence_refs, &mut activity)
                .into_iter()
                .map(|mut receipt| {
                    // 这些证据保存在原 canonical Step/item，不在 Task.evidence_refs。
                    // 临时投影数组的位置不能成为父任务读取原证据的索引。
                    receipt
                        .as_object_mut()
                        .expect("receipt object")
                        .remove("evidence_ref_index");
                    receipt["evidence_source"] = json!("canonical_model_step");
                    receipt
                })
                .collect::<Vec<_>>();
            if let Some(existing) = self
                .payload
                .get_mut("executor_receipts")
                .and_then(Value::as_array_mut)
            {
                existing.extend(failures);
            } else {
                self.payload["executor_receipts"] = Value::Array(failures);
            }
            if self.payload.get("activity").is_some() {
                self.payload["executor_failure_activity"] = activity.to_json();
            } else {
                self.payload["activity"] = activity.to_json();
            }
        }
        self
    }

    /// 失败原因来自 Task；失败前的工具活动仍从原 canonical item 读取。
    pub(crate) fn from_terminal_task_in_session(
        child: &Task,
        store: &magi_session_store::SessionStore,
        session: &magi_core::SessionId,
    ) -> Option<Self> {
        let mut receipt = Self::from_terminal_task(child)?;
        if child.status != TaskStatus::Completed {
            let items = store
                .thread_registry_snapshot(session)
                .into_iter()
                .find(|thread| thread.handled_task_ids.contains(&child.task_id))
                .map(|thread| {
                    store.canonical_tool_calls_for_task(session, &child.task_id, &thread.thread_id)
                })
                .unwrap_or_default();
            receipt = receipt.with_canonical_tool_activity(&items);
        }
        Some(
            receipt
                .with_executor_failures(store.executor_failures_for_task(session, &child.task_id)),
        )
    }

    fn with_canonical_tool_activity(
        mut self,
        items: &[magi_session_store::CanonicalTurnItem],
    ) -> Self {
        use magi_session_store::CanonicalTurnItemStatus;
        let mut activity = AgentActivity::default();
        let mut unconfirmed = Vec::new();
        for item in items {
            let Some(tool) = item.tool.as_ref() else {
                continue;
            };
            observe_tool_call(
                &mut activity,
                &json!({
                    "status": item.status,
                    "name": tool.name,
                    "arguments": tool.arguments,
                    "result": tool.result,
                }),
            );
            let known_not_started = tool.result.as_ref().is_some_and(|result| {
                result.get("execution_started").and_then(Value::as_bool) == Some(false)
            });
            if !known_not_started
                && matches!(
                    item.status,
                    CanonicalTurnItemStatus::Running
                        | CanonicalTurnItemStatus::Cancelled
                        | CanonicalTurnItemStatus::Indeterminate
                )
            {
                unconfirmed.push(json!({
                    "turn_id": item.turn_id, "item_id": item.item_id,
                    "call_id": tool.call_id, "tool": tool.name, "status": item.status,
                }));
            }
        }
        if activity.tool_calls > 0 {
            self.payload["activity"] = activity.to_json();
            self.payload["activity"]["evidence_source"] = json!("canonical_tool_items");
            self.payload["activity"]["unconfirmed_calls"] = json!(
                unconfirmed
                    .iter()
                    .take(RECEIPT_MAX_COMMANDS)
                    .collect::<Vec<_>>()
            );
            self.payload["activity"]["omitted_unconfirmed_calls"] =
                json!(unconfirmed.len().saturating_sub(RECEIPT_MAX_COMMANDS));
        }
        self
    }

    /// 写入按预算截取后的最终答复，生成模型与前端共同消费的回执 JSON。
    fn into_json(mut self, budget_bytes: usize) -> Value {
        let original_bytes = self.final_text.len();
        let (final_text, truncated) = truncate_head_tail(
            &self.final_text,
            budget_bytes,
            "完整答复可在该代理的详情面板查看",
        );
        self.payload["result"] = json!({
            "final_text": final_text,
            "truncated": truncated,
            "original_bytes": original_bytes,
        });
        self.payload
    }
}

/// 把一组回执按共享预算渲染成 JSON：短答复完整保留，剩余预算平均分给较长答复。
pub(crate) fn render_receipts(
    receipts: Vec<AgentReceipt>,
    total_budget_bytes: usize,
) -> Vec<Value> {
    let lengths = receipts
        .iter()
        .map(|receipt| receipt.final_text.len())
        .collect::<Vec<_>>();
    let budgets = allocate_budget(&lengths, total_budget_bytes);
    receipts
        .into_iter()
        .zip(budgets)
        .map(|(receipt, budget)| receipt.into_json(budget))
        .collect()
}

/// 水位分配：按长度从短到长依次满足，每次最多拿剩余预算的平均份额。
fn allocate_budget(lengths: &[usize], total: usize) -> Vec<usize> {
    let mut order = (0..lengths.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| lengths[index]);
    let mut budgets = vec![0; lengths.len()];
    let mut remaining = total;
    for (position, &index) in order.iter().enumerate() {
        let share = remaining / (order.len() - position);
        let granted = lengths[index].min(share);
        budgets[index] = granted;
        remaining -= granted;
    }
    budgets
}

/// 超出预算时保留开头约三分之二和结尾约三分之一，中间标注省略的字节数。
fn truncate_head_tail(text: &str, budget_bytes: usize, hint: &str) -> (String, bool) {
    if text.len() <= budget_bytes {
        return (text.to_string(), false);
    }
    let head_end = floor_char_boundary(text, budget_bytes * 2 / 3);
    let tail_start = ceil_char_boundary(text, text.len() - (budget_bytes - budget_bytes * 2 / 3));
    let omitted = tail_start.saturating_sub(head_end);
    (
        format!(
            "{}\n…（中间省略 {omitted} 字节；{hint}）…\n{}",
            &text[..head_end],
            &text[tail_start..]
        ),
        true,
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// 完成态 output_refs 中的最终答复、工具执行事实和可写代理的工作区产出。
fn completed_output(output_refs: &[String]) -> (Option<String>, AgentActivity, Option<Value>) {
    let mut activity = AgentActivity::default();
    let mut final_text = None;
    let mut workspaces = Vec::new();
    for output in output_refs {
        let parsed = serde_json::from_str::<Value>(output).ok();
        if let Some(output) = parsed
            .as_ref()
            .and_then(|parsed| parsed.get("agent_workspace"))
        {
            workspaces.push(output.clone());
            continue;
        }
        if let Some(outputs) = parsed
            .as_ref()
            .and_then(|parsed| parsed.get("agent_workspaces"))
            .and_then(Value::as_array)
        {
            workspaces.extend(outputs.iter().cloned());
            continue;
        }
        let Some(blocks) =
            parsed.and_then(|parsed| parsed.get("blocks").and_then(Value::as_array).cloned())
        else {
            if let Some(text) = non_empty(output) {
                final_text = Some(text);
            }
            continue;
        };
        for block in &blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block
                        .get("content")
                        .and_then(Value::as_str)
                        .and_then(non_empty)
                    {
                        final_text = Some(text);
                    }
                }
                Some("tool_call") => {
                    if let Some(tool_call) = block.get("toolCall") {
                        observe_tool_call(&mut activity, tool_call);
                    }
                }
                _ => {}
            }
        }
    }
    let workspace = match workspaces.len() {
        0 => None,
        1 => workspaces.pop(),
        _ => Some(Value::Array(workspaces)),
    };
    (final_text, activity, workspace)
}

/// 只投影 TaskStore 与终态一并提交的标准证据，不解析 CLI 私有事件或模型自述。
fn external_executor_receipts(
    evidence_refs: &[String],
    activity: &mut AgentActivity,
) -> Vec<Value> {
    let mut receipts = Vec::new();
    let mut output_bytes = EXECUTOR_OUTPUT_MAX_BYTES;
    let mut command_slots = RECEIPT_MAX_COMMANDS;
    for (index, evidence) in evidence_refs.iter().enumerate() {
        let Ok(magi_core::TaskCompletionEvidence::ExternalExecutor {
            model_step_id,
            receipt,
        }) = serde_json::from_str(evidence)
        else {
            continue;
        };
        if model_step_id.trim().is_empty() || receipt.validate().is_err() {
            continue;
        }
        activity.external_commands += receipt.commands.len();
        activity
            .commands
            .extend(receipt.commands.iter().map(|command| {
                CommandFact {
                    command: command
                        .command
                        .chars()
                        .take(RECEIPT_COMMAND_MAX_CHARS)
                        .collect(),
                    exit_code: command.exit_code,
                    status: match command.status {
                        magi_core::ExecutorCommandStatus::Succeeded => "succeeded",
                        magi_core::ExecutorCommandStatus::Failed => "failed",
                        magi_core::ExecutorCommandStatus::OutcomeUnknown => "outcome_unknown",
                    }
                    .into(),
                    executor: Some(receipt.executor.clone()),
                }
            }));
        let omitted = receipt.commands.len().saturating_sub(command_slots);
        let commands = receipt.commands.iter().skip(omitted).map(|command| {
            let allowance = output_bytes.min(1024);
            let (output, truncated) = truncate_head_tail(&command.output, allowance,
                "完整输出保留在原执行证据中");
            output_bytes = output_bytes.saturating_sub(allowance.min(command.output.len()));
            command_slots = command_slots.saturating_sub(1);
            json!({
                "command":command.command.chars().take(RECEIPT_COMMAND_MAX_CHARS).collect::<String>(),
                "command_truncated":command.command.chars().count() > RECEIPT_COMMAND_MAX_CHARS,
                "status":command.status, "exit_code":command.exit_code,
                "output":output, "output_truncated":truncated, "original_output_bytes":command.output.len(),
            })
        }).collect::<Vec<_>>();
        receipts.push(
            json!({"executor":receipt.executor.chars().take(100).collect::<String>(),
            "execution_id":receipt.execution_id.chars().take(200).collect::<String>(),
            "model_step_id":model_step_id, "evidence_ref_index":index,
            "commands":commands,"omitted_commands":omitted}),
        );
    }
    receipts
}

fn observe_tool_call(activity: &mut AgentActivity, tool_call: &Value) {
    activity.tool_calls += 1;
    let status = tool_call
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let succeeded = status == "success" || status == "succeeded" || status == "completed";
    if !succeeded {
        activity.failed_tool_calls += 1;
    }
    let name = tool_call
        .get("name")
        .and_then(Value::as_str)
        .map(crate::task_helpers::canonical_tool_call_name)
        .unwrap_or_default();
    let arguments = tool_call.get("arguments").cloned().unwrap_or(Value::Null);
    let result = match tool_call.get("result") {
        Some(Value::String(result)) => serde_json::from_str::<Value>(result).unwrap_or(Value::Null),
        Some(result) => result.clone(),
        None => Value::Null,
    };
    let argument = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .and_then(non_empty)
    };
    match name.as_str() {
        "shell_exec" => {
            if let Some(command) = argument("command") {
                activity.commands.push(CommandFact {
                    command: command.chars().take(RECEIPT_COMMAND_MAX_CHARS).collect(),
                    exit_code: result.get("exit_code").and_then(Value::as_i64),
                    status,
                    executor: None,
                });
            }
        }
        _ if !succeeded => {}
        "file_write" | "file_patch" | "file_remove" | "file_mkdir" => {
            activity.files_changed.extend(argument("path"));
        }
        "file_copy" => {
            activity.files_changed.extend(argument("destination"));
        }
        "file_move" => {
            activity.files_changed.extend(argument("source"));
            activity.files_changed.extend(argument("destination"));
        }
        "apply_patch" => {
            if let Some(patch) = argument("patch") {
                activity
                    .files_changed
                    .extend(patch.lines().filter_map(|line| {
                        [
                            "*** Add File: ",
                            "*** Update File: ",
                            "*** Delete File: ",
                            "*** Move to: ",
                        ]
                        .iter()
                        .find_map(|prefix| line.strip_prefix(prefix))
                        .and_then(non_empty)
                    }));
            }
        }
        _ => {}
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{MissionId, TaskId, TaskKind, TaskRuntimePayload, UtcMillis};

    #[test]
    fn failed_and_killed_agent_receipts_include_partial_external_evidence() {
        for status in [TaskStatus::Failed, TaskStatus::Killed] {
            let child = task(status, vec!["execution stopped".into()]);
            let evidence = magi_core::ExecutorFailureEvidence {
                receipts: vec![magi_core::ExecutorReceipt {
                    executor: "fixture".into(),
                    execution_id: "external-run".into(),
                    commands: vec![magi_core::ExecutorCommandEvidence {
                        command: "inspect".into(),
                        exit_code: None,
                        status: magi_core::ExecutorCommandStatus::OutcomeUnknown,
                        output: "partial".into(),
                    }],
                }],
                usage: None,
                items: vec![],
                resources_settled: true,
            };
            let receipt = AgentReceipt::from_terminal_task(&child)
                .unwrap()
                .with_executor_failures(vec![("original-step".into(), evidence)])
                .into_json(8192);
            assert_eq!(
                receipt["child_status"],
                if status == TaskStatus::Failed {
                    "failed"
                } else {
                    "killed"
                }
            );
            assert_eq!(receipt["activity"]["tool_calls"], 0);
            assert_eq!(receipt["activity"]["external_commands"], 1);
            assert_eq!(
                receipt["executor_receipts"][0]["model_step_id"],
                "original-step"
            );
            assert_eq!(
                receipt["executor_receipts"][0]["commands"][0]["status"],
                "outcome_unknown"
            );
            assert!(receipt["executor_receipts"][0]["commands"][0]["exit_code"].is_null());
            assert_eq!(receipt["executor_failures"][0]["resources_settled"], true);
            assert_eq!(
                receipt["executor_receipts"][0]["evidence_source"],
                "canonical_model_step"
            );
            assert!(
                receipt["executor_receipts"][0]
                    .get("evidence_ref_index")
                    .is_none()
            );
        }
    }

    fn task(status: TaskStatus, output_refs: Vec<String>) -> Task {
        Task {
            task_id: TaskId::new("task-receipt"),
            mission_id: MissionId::new("mission-receipt"),
            root_task_id: TaskId::new("task-root"),
            parent_task_id: Some(TaskId::new("task-root")),
            kind: TaskKind::LocalAgent,
            title: "回执测试".to_string(),
            goal: "验证回执".to_string(),
            status,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            completion_contract: magi_core::TaskCompletionContract::default(),
            recovery_checkpoint: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs,
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(1),
        }
    }

    fn tool_block(name: &str, arguments: Value, status: &str, result: Value) -> Value {
        json!({
            "type": "tool_call",
            "toolCall": {
                "id": format!("call-{name}"),
                "name": name,
                "arguments": arguments,
                "status": status,
                "result": result.to_string(),
            }
        })
    }

    #[test]
    fn failed_and_cancelled_receipts_keep_scoped_canonical_tool_effects_after_restore() {
        use magi_core::{SessionId, ThreadId, WorkerId};
        use magi_session_store::{
            CanonicalTurn, ExecutionThread, ExecutionThreadStatus, SessionStore, SessionStoreState,
        };
        let session = SessionId::new("native-partial-session");
        let thread = ThreadId::new("native-partial-thread");
        let make_turn = |session: &SessionId,
                         thread: &ThreadId,
                         task: &str,
                         suffix: &str|
         -> CanonicalTurn {
            let turn_id = format!("partial-{suffix}");
            let tools = [
                (
                    "file_write",
                    "completed",
                    json!({"path":"result.txt"}),
                    json!({"status":"succeeded"}),
                ),
                (
                    "shell_exec",
                    "completed",
                    json!({"command":"node check.mjs"}),
                    json!({"status":"succeeded","exit_code":0}),
                ),
                (
                    "shell_exec",
                    "indeterminate",
                    json!({"command":"publish once"}),
                    json!({"status":"outcome_unknown"}),
                ),
                (
                    "file_remove",
                    "cancelled",
                    json!({"path":"unconfirmed.txt"}),
                    Value::Null,
                ),
                (
                    "file_remove",
                    "cancelled",
                    json!({"path":"never-executed.txt"}),
                    json!({"status":"cancelled","execution_started":false}),
                ),
            ];
            serde_json::from_value(json!({
                "sessionId":session,"turnId":turn_id,"turnSeq":1,"acceptedAt":1,
                "completedAt":2,"status":"interrupted","metadata":{},"modelSteps":[],
                "items":tools.into_iter().enumerate().map(|(index,(name,status,arguments,result))| json!({
                    "sessionId":session,"turnId":turn_id,"turnSeq":1,"itemId":format!("tool-{index}"),
                    "itemSeq":index+1,"kind":"tool_call","createdAt":1,"updatedAt":2,"status":status,
                    "sourceThreadId":thread,"worker":{"taskId":task},
                    "tool":{"callId":format!("call-{index}"),"name":name,"arguments":arguments,"result":result},
                })).collect::<Vec<_>>(),
            })).unwrap()
        };
        let child = task(TaskStatus::Failed, vec!["model invocation failed".into()]);
        let sessions = SessionStore::from_state(SessionStoreState {
            canonical_turns: vec![
                make_turn(&session, &thread, child.task_id.as_str(), "original"),
                make_turn(&session, &thread, "sibling-task", "sibling"),
                make_turn(
                    &session,
                    &ThreadId::new("other-thread"),
                    child.task_id.as_str(),
                    "other-thread",
                ),
                make_turn(
                    &SessionId::new("other-session"),
                    &thread,
                    child.task_id.as_str(),
                    "other-session",
                ),
            ],
            thread_registry: vec![ExecutionThread {
                thread_id: thread.clone(),
                session_id: session.clone(),
                mission_id: child.mission_id.clone(),
                role_id: "executor".into(),
                worker_instance_id: WorkerId::new("native-partial-worker"),
                status: ExecutionThreadStatus::Idle,
                created_at: UtcMillis(1),
                last_used_at: UtcMillis(2),
                observed_context_window_tokens: None,
                handled_task_ids: vec![child.task_id.clone()],
                message_history: vec![],
            }],
            ..Default::default()
        });
        let sessions = SessionStore::from_persisted_parts(
            serde_json::from_slice(&serde_json::to_vec(&sessions.durable_state()).unwrap())
                .unwrap(),
            sessions.execution_sidecar_store_state(),
        )
        .unwrap();
        let before = sessions.canonical_turns_for_session(&session);
        for status in [TaskStatus::Failed, TaskStatus::Killed] {
            let child = Task {
                status,
                ..child.clone()
            };
            let receipt = AgentReceipt::from_terminal_task_in_session(&child, &sessions, &session)
                .unwrap()
                .into_json(4096);
            assert_eq!(receipt["activity"]["tool_calls"], 5);
            assert_eq!(receipt["activity"]["files_changed"], json!(["result.txt"]));
            assert_eq!(receipt["activity"]["commands"][0]["exit_code"], 0);
            assert_eq!(
                receipt["activity"]["commands"][1]["status"],
                "indeterminate"
            );
            assert!(receipt["activity"]["commands"][1]["exit_code"].is_null());
            assert_eq!(
                receipt["activity"]["unconfirmed_calls"][0]["turn_id"],
                "partial-original"
            );
            assert_eq!(
                receipt["activity"]["unconfirmed_calls"][0]["call_id"],
                "call-2"
            );
            assert_eq!(
                receipt["activity"]["unconfirmed_calls"]
                    .as_array()
                    .unwrap()
                    .len(),
                2,
                "取消不能证明没有副作用，只有明确未执行的调用才排除于待核对列表"
            );
            assert_eq!(
                receipt["activity"]["evidence_source"],
                "canonical_tool_items"
            );
        }
        let completed = Task {
            status: TaskStatus::Completed,
            output_refs: vec![json!({"blocks":[tool_block("file_write",json!({"path":"result.txt"}),"success",json!({"status":"succeeded"}))]}).to_string()],
            ..child
        };
        let receipt = AgentReceipt::from_terminal_task_in_session(&completed, &sessions, &session)
            .unwrap()
            .into_json(4096);
        assert_eq!(
            receipt["activity"]["tool_calls"], 1,
            "完成态仍使用原 Task 终态证据，不能重复叠加 canonical 活动"
        );
        assert_eq!(sessions.canonical_turns_for_session(&session), before);
    }

    #[test]
    fn completed_receipt_extracts_final_text_and_execution_facts() {
        let output = json!({
            "blocks": [
                {"type": "text", "content": "我先看一下文件"},
                tool_block("shell_exec", json!({"command": "cargo test -p demo"}), "failed", json!({"exit_code": 101})),
                tool_block("file_patch", json!({"path": "src/lib.rs"}), "success", json!({})),
                tool_block("apply_patch", json!({"patch": "*** Begin Patch\n*** Add File: src/new.rs\n+fn a() {}\n*** End Patch"}), "success", json!({})),
                tool_block("file_write", json!({"path": "src/rejected.rs"}), "failed", json!({})),
                tool_block("shell_exec", json!({"command": "cargo test -p demo"}), "success", json!({"exit_code": 0})),
                {"type": "text", "content": "修复完成，测试通过。"},
            ]
        });
        let receipt = AgentReceipt::from_terminal_task(&task(
            TaskStatus::Completed,
            vec![output.to_string()],
        ))
        .expect("终态任务应生成回执");
        let rendered = render_receipts(vec![receipt], 1024).remove(0);

        assert_eq!(rendered["result"]["final_text"], "修复完成，测试通过。");
        assert_eq!(rendered["activity"]["tool_calls"], 5);
        assert_eq!(rendered["activity"]["failed_tool_calls"], 2);
        assert_eq!(rendered["activity"]["commands"][0]["exit_code"], 101);
        assert_eq!(rendered["activity"]["commands"][1]["exit_code"], 0);
        assert_eq!(
            rendered["activity"]["files_changed"],
            json!(["src/lib.rs", "src/new.rs"]),
            "失败的写入不算改动"
        );
    }

    #[test]
    fn external_commands_are_evidence_with_bounded_output_and_never_native_calls() {
        let mut child = task(TaskStatus::Completed, vec!["检查完成".into()]);
        let commands = (0..20)
            .map(|index| magi_core::ExecutorCommandEvidence {
                command: format!("inspect {index}"),
                exit_code: if index == 19 { None } else { Some(0) },
                status: if index == 19 {
                    magi_core::ExecutorCommandStatus::OutcomeUnknown
                } else {
                    magi_core::ExecutorCommandStatus::Succeeded
                },
                output: "正文".repeat(4000),
            })
            .collect::<Vec<_>>();
        child.evidence_refs = vec![
            serde_json::to_string(&magi_core::TaskCompletionEvidence::ExternalExecutor {
                model_step_id: "step-external".into(),
                receipt: magi_core::ExecutorReceipt {
                    executor: "fixture_executor".into(),
                    execution_id: "external-thread".into(),
                    commands,
                },
            })
            .unwrap(),
        ];
        let before = child.evidence_refs.clone();
        let receipt = render_receipts(
            vec![AgentReceipt::from_terminal_task(&child).unwrap()],
            1024,
        )
        .remove(0);
        assert_eq!(receipt["result"]["final_text"], "检查完成");
        assert_eq!(receipt["activity"]["tool_calls"], 0);
        assert_eq!(receipt["activity"]["external_commands"], 20);
        assert_eq!(receipt["activity"]["omitted_commands"], 8);
        let evidence = &receipt["executor_receipts"][0];
        assert_eq!(evidence["model_step_id"], "step-external");
        assert_eq!(evidence["omitted_commands"], 8);
        let commands = evidence["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 12);
        assert_eq!(commands.last().unwrap()["status"], "outcome_unknown");
        assert!(commands.last().unwrap()["exit_code"].is_null());
        assert!(
            commands
                .iter()
                .all(|command| command["output_truncated"] == true)
        );
        assert!(
            commands
                .iter()
                .map(|command| command["output"].as_str().unwrap().len())
                .sum::<usize>()
                < EXECUTOR_OUTPUT_MAX_BYTES + 2048
        );
        assert_eq!(before, child.evidence_refs, "展示裁剪不能改写持久证据");
        assert!(serde_json::from_str::<Task>(&serde_json::to_string(&child).unwrap()).is_ok());
    }

    #[test]
    fn model_text_cannot_create_executor_evidence() {
        let output = json!({"executor_receipt":{
            "executor":"fixture_executor", "execution_id":"invented", "commands":[
                {"command":"cargo test", "exit_code":0,"status":"succeeded","output":"passed"}
            ]
        }})
        .to_string();
        let child = task(TaskStatus::Completed, vec![output.clone()]);
        let result = render_receipts(
            vec![AgentReceipt::from_terminal_task(&child).unwrap()],
            4096,
        )
        .remove(0);
        assert_eq!(result["result"]["final_text"], output);
        assert_eq!(result["activity"]["external_commands"], 0);
        assert!(
            result.get("executor_receipts").is_none(),
            "模型自述不是可信执行器证据"
        );
    }

    #[test]
    fn completed_receipt_reports_unapplied_workspace_output() {
        let receipt = AgentReceipt::from_terminal_task(&task(
            TaskStatus::Completed,
            vec![
                "实现完成".to_string(),
                json!({"agent_workspace": {
                    "branch": "magi/agent/task-1",
                    "base": "base-commit",
                    "head": "head-commit",
                    "changed_paths": ["src/lib.rs"],
                }})
                .to_string(),
            ],
        ))
        .expect("应生成回执");
        let rendered = render_receipts(vec![receipt], 1024).remove(0);
        assert_eq!(rendered["result"]["final_text"], "实现完成");
        assert_eq!(rendered["workspace"]["branch"], "magi/agent/task-1");
        assert_eq!(
            rendered["workspace"]["changed_paths"],
            json!(["src/lib.rs"])
        );
        assert!(
            rendered["workspace"]["instruction"]
                .as_str()
                .is_some_and(|text| text.contains("agent_apply"))
        );
    }

    #[test]
    fn completed_receipt_preserves_each_repository_without_overwriting_the_final_answer() {
        let outputs = ["/repo-a", "/repo-b"].map(|root| {
            json!({
                    "repository_root": root, "branch": format!("agent{root}"),
                    "base": "base", "head": "result", "changed_paths": ["src/index.ts"],
            })
        });
        let child = task(
            TaskStatus::Completed,
            vec![
                "两个仓库处理完成".into(),
                json!({"agent_workspaces": outputs}).to_string(),
            ],
        );
        let rendered = render_receipts(
            vec![AgentReceipt::from_terminal_task(&child).unwrap()],
            1024,
        )
        .remove(0);
        assert_eq!(rendered["result"]["final_text"], "两个仓库处理完成");
        assert_eq!(rendered["workspaces"].as_array().unwrap().len(), 2);
        assert_eq!(rendered["workspaces"][0]["repository_root"], "/repo-a");
        assert_eq!(rendered["workspaces"][1]["repository_root"], "/repo-b");
        assert!(
            rendered["workspace_instruction"]
                .as_str()
                .unwrap()
                .contains("agent_apply")
        );
    }

    #[test]
    fn running_task_has_no_receipt() {
        assert!(AgentReceipt::from_terminal_task(&task(TaskStatus::Running, Vec::new())).is_none());
    }

    #[test]
    fn shared_budget_keeps_short_answers_whole_and_splits_the_rest() {
        assert_eq!(
            allocate_budget(&[100, 10_000, 10_000], 10_100),
            vec![100, 5_000, 5_000]
        );
        assert_eq!(allocate_budget(&[10, 20], 1_000), vec![10, 20]);
    }

    #[test]
    fn every_long_answer_keeps_its_opening_and_ending() {
        let long = |marker: &str| {
            format!(
                "{marker}开头结论。{}{marker}结尾风险。",
                "中间推理过程。".repeat(2_000)
            )
        };
        let receipts = ["甲", "乙", "丙"]
            .iter()
            .map(|marker| {
                AgentReceipt::from_terminal_task(&task(TaskStatus::Completed, vec![long(marker)]))
                    .expect("应生成回执")
            })
            .collect::<Vec<_>>();
        let rendered = render_receipts(receipts, AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES);
        for (marker, receipt) in ["甲", "乙", "丙"].iter().zip(&rendered) {
            let text = receipt["result"]["final_text"].as_str().expect("应有答复");
            assert!(text.starts_with(&format!("{marker}开头结论")));
            assert!(text.ends_with(&format!("{marker}结尾风险。")));
            assert_eq!(receipt["result"]["truncated"], true);
        }
        let total = rendered
            .iter()
            .map(|receipt| receipt["result"]["final_text"].as_str().unwrap().len())
            .sum::<usize>();
        assert!(total <= AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES + 3 * 200);
    }

    #[test]
    fn killed_receipt_carries_cancellation_reason() {
        let receipt = AgentReceipt::from_terminal_task(&task(
            TaskStatus::Killed,
            vec!["主线取消：方向错误".to_string()],
        ))
        .expect("应生成回执");
        assert_eq!(receipt.child_status(), "killed");
        let rendered = render_receipts(vec![receipt], 1024).remove(0);
        assert_eq!(rendered["result"]["final_text"], "主线取消：方向错误");
    }
}

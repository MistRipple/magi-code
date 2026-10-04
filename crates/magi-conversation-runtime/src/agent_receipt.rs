//! 子代理终态回执。
//!
//! 回执只从子任务进入终态时与状态一起提交的 `output_refs` 构造：完成时它是
//! `{ "blocks": [工具调用记录..., { "type": "text", "content": 最终答复 }] }`，
//! 失败或终止时是公开的失败原因。主线等待结果不再读取运行期线程快照，避免把
//! 中间过程文字当成最终结论。
//!
//! 执行事实（实际运行的命令及退出码、改动过的文件、工具失败次数）由运行时从工具
//! 调用记录提取，不依赖子代理在最终答复里自述。

use std::collections::BTreeSet;

use magi_core::{
    Task, TaskStatus, classify_public_task_failure, public_task_failure_is_degraded,
    public_task_output_refs,
};
use serde_json::{Value, json};

/// 一次 `agent_wait` 中所有代理最终答复共享的展示预算（字节）。
pub(crate) const AGENT_WAIT_FINAL_TEXT_BUDGET_BYTES: usize = 32 * 1024;
/// 回执中最多列出的命令条数，保留最近的命令。
const RECEIPT_MAX_COMMANDS: usize = 12;
/// 回执中最多列出的改动文件数。
const RECEIPT_MAX_FILES: usize = 40;
const RECEIPT_COMMAND_MAX_CHARS: usize = 200;
const AGENT_UNAVAILABLE_PUBLIC_TEXT: &str = "代理当前不可用，主线需要改派或接管。";

/// 子代理实际执行过的命令事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandFact {
    pub command: String,
    pub exit_code: Option<i64>,
    pub status: String,
}

/// 从工具调用记录提取的执行事实。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentActivity {
    pub tool_calls: usize,
    pub failed_tool_calls: usize,
    pub commands: Vec<CommandFact>,
    pub files_changed: BTreeSet<String>,
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
                let (final_text, activity) = completed_output(&child.output_refs);
                let mut payload = base("completed", "completed");
                payload["activity"] = activity.to_json();
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
                    payload["instruction"] = json!(
                        "代理当前不可用。请不要停止任务：优先改派其他可用角色继续；如果没有必要继续派发，则由主线根据已有上下文直接推进。"
                    );
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
                payload["instruction"] = json!(
                    "判断该代理的失败是否影响结论：可补救时改派其他角色或由主线接管，不要把单个代理失败当作整体失败。"
                );
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

    /// 写入按预算截取后的最终答复，生成模型与前端共同消费的回执 JSON。
    fn into_json(mut self, budget_bytes: usize) -> Value {
        let original_bytes = self.final_text.len();
        let (final_text, truncated) = truncate_head_tail(&self.final_text, budget_bytes);
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
fn truncate_head_tail(text: &str, budget_bytes: usize) -> (String, bool) {
    if text.len() <= budget_bytes {
        return (text.to_string(), false);
    }
    let head_end = floor_char_boundary(text, budget_bytes * 2 / 3);
    let tail_start = ceil_char_boundary(text, text.len() - (budget_bytes - budget_bytes * 2 / 3));
    let omitted = tail_start.saturating_sub(head_end);
    (
        format!(
            "{}\n…（中间省略 {omitted} 字节；完整答复可在该代理的详情面板查看）…\n{}",
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

/// 完成态 output_refs 中的最终答复与工具执行事实。
fn completed_output(output_refs: &[String]) -> (Option<String>, AgentActivity) {
    let mut activity = AgentActivity::default();
    let mut final_text = None;
    for output in output_refs {
        let Some(blocks) = serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|parsed| parsed.get("blocks").and_then(Value::as_array).cloned())
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
    (final_text, activity)
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
    let result = tool_call
        .get("result")
        .and_then(Value::as_str)
        .and_then(|result| serde_json::from_str::<Value>(result).ok())
        .unwrap_or(Value::Null);
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

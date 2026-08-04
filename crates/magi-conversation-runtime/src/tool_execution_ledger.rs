//! 单个任务轮次内的工具执行账本。
//!
//! 模型可以重复提出相同的 function call，但模型请求不是工具执行授权。账本在
//! 调度边界统一约束实际执行次数：只读幂等调用复用同轮成功结果；用户明确要求
//! 某工具只调用一次时，后续调用返回结构化预算结果而不再触发外部副作用。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use magi_bridge_client::ChatToolCall;
use magi_core::ExecutionResultStatus;
use magi_session_store::ThreadChatMessage;
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};
use serde_json::Value;

use crate::{
    canonical_tool_call_name, context_authority::CurrentFileFact,
    tool_result_utils::infer_tool_call_status,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolExecutionLedger {
    successful_idempotent_calls: BTreeMap<ToolCallFingerprint, String>,
    current_file_facts: BTreeMap<PathBuf, CurrentFileFact>,
    workspace_root_path: Option<PathBuf>,
    successful_recovered_side_effect_calls: BTreeMap<ToolCallFingerprint, String>,
    interrupted_non_idempotent_calls: BTreeMap<ToolCallFingerprint, String>,
    executed_call_counts: BTreeMap<String, usize>,
    explicit_call_budgets: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ToolCallFingerprint {
    tool_name: String,
    canonical_arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallExecutionDecision {
    Execute {
        fingerprint: Option<ToolCallFingerprint>,
    },
    Reuse {
        result: String,
    },
    ReuseAfterExecution {
        source_index: usize,
        fingerprint: ToolCallFingerprint,
    },
    BudgetExhausted {
        result: String,
    },
    RecoveryBlocked {
        result: String,
    },
}

impl ToolCallExecutionDecision {
    pub(crate) fn immediate_result(&self) -> Option<(String, ExecutionResultStatus)> {
        match self {
            Self::Reuse { result } | Self::BudgetExhausted { result } => {
                Some((result.clone(), ExecutionResultStatus::Succeeded))
            }
            Self::RecoveryBlocked { result } => {
                Some((result.clone(), ExecutionResultStatus::Failed))
            }
            Self::Execute { .. } | Self::ReuseAfterExecution { .. } => None,
        }
    }
}

impl ToolExecutionLedger {
    pub(crate) fn for_task_goal(goal: &str) -> Self {
        Self {
            explicit_call_budgets: explicit_single_call_budgets(goal),
            ..Self::default()
        }
    }

    /// 从当前 task thread 的持久化消息恢复执行账本。
    ///
    /// 工具调用先于实际执行写入 thread，成功结果随后写入。因此恢复时：已有成功
    /// 结果的调用直接复用；没有结果的非只读调用必须先检查实际状态，不能自动重放。
    pub(crate) fn from_thread_history(
        goal: &str,
        history: &[ThreadChatMessage],
        tool_registry: Option<&ToolRegistry>,
    ) -> Self {
        let mut ledger = Self::for_task_goal(goal);
        let current_turn_start = history
            .iter()
            .rposition(|message| message.role == "user")
            .map(|index| index.saturating_add(1))
            .unwrap_or_default();
        let mut calls = BTreeMap::<String, (usize, ChatToolCall)>::new();
        let mut results = BTreeMap::<String, String>::new();

        for (message_index, message) in history.iter().enumerate() {
            if message.role == "assistant" {
                for call in &message.tool_calls {
                    calls.insert(
                        call.id.clone(),
                        (
                            message_index,
                            ChatToolCall {
                                id: call.id.clone(),
                                kind: call.kind.clone(),
                                function: magi_bridge_client::ChatToolFunction {
                                    name: call.function.name.clone(),
                                    arguments: call.function.arguments.clone(),
                                },
                            },
                        ),
                    );
                }
            }
            if message.role == "tool"
                && let (Some(call_id), Some(result)) =
                    (message.tool_call_id.as_deref(), message.content.as_deref())
            {
                results.insert(call_id.to_string(), result.to_string());
            }
        }

        for (call_id, (message_index, tool_call)) in calls {
            let canonical_name = canonical_tool_call_name(&tool_call.function.name);
            let result = results.get(&call_id);
            let belongs_to_current_turn = message_index >= current_turn_start;
            if result.is_some_and(|result| tool_result_is_interrupted_not_started(result)) {
                continue;
            }
            if belongs_to_current_turn {
                *ledger
                    .executed_call_counts
                    .entry(canonical_name.clone())
                    .or_default() += 1;
            }

            let Some(result) = result else {
                if belongs_to_current_turn
                    && !is_idempotent_read_tool(&canonical_name, tool_registry)
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .interrupted_non_idempotent_calls
                        .insert(fingerprint, interrupted_call_result(&canonical_name));
                }
                continue;
            };

            if tool_result_is_interrupted(result) {
                if belongs_to_current_turn
                    && tool_result_is_interrupted_unknown(result)
                    && !is_idempotent_read_tool(&canonical_name, tool_registry)
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .interrupted_non_idempotent_calls
                        .insert(fingerprint, interrupted_call_result(&canonical_name));
                }
            } else if infer_tool_call_status(result) == "success" {
                if is_idempotent_read_tool(&canonical_name, tool_registry) {
                    if reusable_result_is_current(&canonical_name, result)
                        && let Some(fingerprint) =
                            tool_call_fingerprint(&tool_call, &canonical_name)
                    {
                        ledger
                            .successful_idempotent_calls
                            .insert(fingerprint, result.clone());
                    }
                } else if belongs_to_current_turn
                    && let Some(fingerprint) = tool_call_fingerprint(&tool_call, &canonical_name)
                {
                    ledger
                        .successful_recovered_side_effect_calls
                        .insert(fingerprint, result.clone());
                }
            }
        }

        ledger
    }

    pub(crate) fn with_current_file_facts(
        mut self,
        facts: &[CurrentFileFact],
        workspace_root_path: Option<&Path>,
    ) -> Self {
        self.workspace_root_path = workspace_root_path.map(normalize_existing_path);
        for fact in facts {
            let path = normalize_existing_path(Path::new(&fact.absolute_path));
            self.current_file_facts.insert(path, fact.clone());
        }
        self
    }

    /// 为一个模型响应生成执行决策。相同只读调用在同一响应内只允许一个
    /// 真实执行代表；跨响应则直接复用已经成功的结果。
    pub(crate) fn plan(
        &self,
        tool_calls: &[ChatToolCall],
        tool_registry: Option<&ToolRegistry>,
    ) -> Vec<ToolCallExecutionDecision> {
        let mut first_pending_by_fingerprint = BTreeMap::<ToolCallFingerprint, usize>::new();
        let mut planned_call_counts = self.executed_call_counts.clone();

        tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| {
                let canonical_name = canonical_tool_call_name(&tool_call.function.name);
                let fingerprint = idempotent_fingerprint(tool_call, &canonical_name, tool_registry);

                if canonical_name == "file_read"
                    && let Some(result) = self.reuse_current_file_fact(tool_call)
                {
                    return ToolCallExecutionDecision::Reuse { result };
                }

                if let Some(fingerprint) = fingerprint.as_ref()
                    && let Some(result) = self.successful_idempotent_calls.get(fingerprint)
                {
                    return ToolCallExecutionDecision::Reuse {
                        result: reused_result(
                            &canonical_name,
                            result,
                            "duplicate_idempotent_call",
                            "本轮已复用相同只读工具的成功结果，未再次执行。",
                        ),
                    };
                }

                if let Some(fingerprint) = tool_call_fingerprint(tool_call, &canonical_name)
                    && let Some(result) = self
                        .successful_recovered_side_effect_calls
                        .get(&fingerprint)
                {
                    return ToolCallExecutionDecision::Reuse {
                        result: reused_result(
                            &canonical_name,
                            result,
                            "completed_before_recovery",
                            "相同外部操作已在中断前成功完成，本次恢复直接继承原结果，未重复执行。",
                        ),
                    };
                }

                if let Some(fingerprint) = tool_call_fingerprint(tool_call, &canonical_name)
                    && let Some(result) = self.interrupted_non_idempotent_calls.get(&fingerprint)
                {
                    return ToolCallExecutionDecision::RecoveryBlocked {
                        result: result.clone(),
                    };
                }

                if let Some(limit) = self.explicit_call_budgets.get(&canonical_name)
                    && planned_call_counts
                        .get(&canonical_name)
                        .copied()
                        .unwrap_or_default()
                        >= *limit
                {
                    return ToolCallExecutionDecision::BudgetExhausted {
                        result: budget_exhausted_result(&canonical_name, *limit),
                    };
                }

                if let Some(fingerprint) = fingerprint {
                    if let Some(source_index) = first_pending_by_fingerprint.get(&fingerprint) {
                        return ToolCallExecutionDecision::ReuseAfterExecution {
                            source_index: *source_index,
                            fingerprint,
                        };
                    }
                    first_pending_by_fingerprint.insert(fingerprint.clone(), index);
                    *planned_call_counts.entry(canonical_name).or_default() += 1;
                    return ToolCallExecutionDecision::Execute {
                        fingerprint: Some(fingerprint),
                    };
                }

                *planned_call_counts.entry(canonical_name).or_default() += 1;
                ToolCallExecutionDecision::Execute { fingerprint: None }
            })
            .collect()
    }

    pub(crate) fn record_execution(
        &mut self,
        tool_call: &ChatToolCall,
        fingerprint: Option<&ToolCallFingerprint>,
        result: &(String, ExecutionResultStatus),
    ) {
        let canonical_name = canonical_tool_call_name(&tool_call.function.name);
        *self
            .executed_call_counts
            .entry(canonical_name.clone())
            .or_default() += 1;
        if matches!(result.1, ExecutionResultStatus::Succeeded)
            && !is_idempotent_read_tool(&canonical_name, None)
        {
            self.successful_idempotent_calls
                .retain(|fingerprint, _| fingerprint.tool_name != "file_read");
            self.current_file_facts.clear();
        }
        if matches!(result.1, ExecutionResultStatus::Succeeded)
            && let Some(fingerprint) = fingerprint
        {
            self.successful_idempotent_calls
                .insert(fingerprint.clone(), result.0.clone());
        }
        if canonical_name == "file_read"
            && matches!(result.1, ExecutionResultStatus::Succeeded)
            && let Some(fact) = current_file_fact_from_result(&result.0)
        {
            self.current_file_facts.insert(
                normalize_existing_path(Path::new(&fact.absolute_path)),
                fact,
            );
        }
    }

    pub(crate) fn execute_batch_with(
        &mut self,
        tool_calls: &[ChatToolCall],
        tool_registry: Option<&ToolRegistry>,
        mut execute: impl FnMut(&[ChatToolCall]) -> Vec<(String, ExecutionResultStatus)>,
    ) -> Vec<(String, ExecutionResultStatus)> {
        let decisions = self.plan(tool_calls, tool_registry);
        let execution_indices = decisions
            .iter()
            .enumerate()
            .filter_map(|(index, decision)| {
                matches!(decision, ToolCallExecutionDecision::Execute { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let execution_calls = execution_indices
            .iter()
            .map(|index| tool_calls[*index].clone())
            .collect::<Vec<_>>();
        let executed_results = execute(&execution_calls);
        let mut results = vec![None; tool_calls.len()];
        for (execution_index, result) in execution_indices.iter().zip(executed_results) {
            let ToolCallExecutionDecision::Execute { fingerprint } = &decisions[*execution_index]
            else {
                unreachable!("only execute decisions are dispatched");
            };
            self.record_execution(&tool_calls[*execution_index], fingerprint.as_ref(), &result);
            results[*execution_index] = Some(result);
        }

        let mut fallback_indices = Vec::new();
        for (index, decision) in decisions.iter().enumerate() {
            if let Some(result) = decision.immediate_result() {
                results[index] = Some(result);
                continue;
            }
            match decision {
                ToolCallExecutionDecision::ReuseAfterExecution {
                    source_index,
                    fingerprint,
                } => {
                    let Some(source_result) = results[*source_index].as_ref() else {
                        unreachable!("duplicate source must execute before its reuse decision");
                    };
                    if let Some(reused) =
                        self.reuse_after_execution(&tool_calls[index], fingerprint, source_result)
                    {
                        results[index] = Some(reused);
                    } else {
                        fallback_indices.push(index);
                    }
                }
                ToolCallExecutionDecision::Execute { .. } => {}
                ToolCallExecutionDecision::Reuse { .. }
                | ToolCallExecutionDecision::BudgetExhausted { .. }
                | ToolCallExecutionDecision::RecoveryBlocked { .. } => {
                    unreachable!("immediate decisions are handled before deferred execution")
                }
            }
        }

        if !fallback_indices.is_empty() {
            let fallback_calls = fallback_indices
                .iter()
                .map(|index| tool_calls[*index].clone())
                .collect::<Vec<_>>();
            let fallback_results = execute(&fallback_calls);
            for (index, result) in fallback_indices.into_iter().zip(fallback_results) {
                let ToolCallExecutionDecision::ReuseAfterExecution { fingerprint, .. } =
                    &decisions[index]
                else {
                    unreachable!("only failed duplicate calls are retried");
                };
                self.record_execution(&tool_calls[index], Some(fingerprint), &result);
                results[index] = Some(result);
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                result.unwrap_or_else(|| {
                    (
                        serde_json::json!({
                            "tool": tool_calls[index].function.name,
                            "status": "failed",
                            "error_code": "tool_execution_missing_result",
                            "error": "工具执行未返回结果"
                        })
                        .to_string(),
                        ExecutionResultStatus::Failed,
                    )
                })
            })
            .collect()
    }

    pub(crate) fn reuse_after_execution(
        &self,
        tool_call: &ChatToolCall,
        fingerprint: &ToolCallFingerprint,
        source_result: &(String, ExecutionResultStatus),
    ) -> Option<(String, ExecutionResultStatus)> {
        if !matches!(source_result.1, ExecutionResultStatus::Succeeded) {
            return None;
        }
        let source = self.successful_idempotent_calls.get(fingerprint)?;
        Some((
            reused_result(
                &canonical_tool_call_name(&tool_call.function.name),
                source,
                "duplicate_idempotent_call",
                "本轮已复用相同只读工具的成功结果，未再次执行。",
            ),
            ExecutionResultStatus::Succeeded,
        ))
    }

    fn reuse_current_file_fact(&self, tool_call: &ChatToolCall) -> Option<String> {
        let arguments = serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?;
        let requested_path = arguments.get("path").and_then(Value::as_str)?;
        let requested_path = PathBuf::from(requested_path);
        let absolute_path = if requested_path.is_absolute() {
            requested_path
        } else {
            self.workspace_root_path.as_ref()?.join(requested_path)
        };
        let fact = self
            .current_file_facts
            .get(&normalize_existing_path(&absolute_path))?;
        if !magi_snapshot::path_content_hash(Path::new(&fact.absolute_path))
            .is_ok_and(|actual_hash| actual_hash == fact.content_hash)
        {
            return None;
        }
        let adapted = adapt_file_read_result(&fact.result, &arguments)?;
        Some(reused_result(
            "file_read",
            &adapted,
            "current_session_file_fact",
            "文件内容未变化，已复用当前会话中的文件事实，未再次执行读取。",
        ))
    }
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn current_file_fact_from_result(result: &str) -> Option<CurrentFileFact> {
    let payload = serde_json::from_str::<Value>(result).ok()?;
    if payload.get("tool").and_then(Value::as_str) != Some("file_read")
        || infer_tool_call_status(result) != "success"
    {
        return None;
    }
    let absolute_path = payload.get("path").and_then(Value::as_str)?.to_string();
    let content_hash = payload
        .get("content_hash")
        .and_then(Value::as_str)?
        .to_string();
    let summary = payload
        .get("content")
        .and_then(Value::as_str)
        .or_else(|| payload.get("summary").and_then(Value::as_str))
        .unwrap_or("文件读取成功")
        .chars()
        .take(8_000)
        .collect();
    Some(CurrentFileFact {
        absolute_path,
        content_hash,
        result: result.to_string(),
        summary,
    })
}

fn adapt_file_read_result(source_result: &str, arguments: &Value) -> Option<String> {
    let mut source = serde_json::from_str::<Value>(source_result).ok()?;
    if source.get("mode").and_then(Value::as_str) != Some("file") {
        return Some(source.to_string());
    }
    let Some(requested_max_bytes) = arguments
        .get("max_bytes")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
    else {
        return (!source
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(true))
        .then(|| source.to_string());
    };
    let content = source.get("content").and_then(Value::as_str)?;
    let source_bytes = source
        .get("bytes_read")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(content.len());
    let source_truncated = source
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if source_truncated && source_bytes < requested_max_bytes {
        return None;
    }
    let content_bytes = content.as_bytes();
    let adapted_len = requested_max_bytes.min(content_bytes.len());
    let adapted_content = String::from_utf8_lossy(&content_bytes[..adapted_len]).to_string();
    let file_size = source
        .get("file_size_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(adapted_len as u64);
    let path = source
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    source["max_bytes"] = requested_max_bytes.into();
    source["bytes_read"] = adapted_len.into();
    source["content"] = adapted_content.into();
    source["truncated"] = (file_size > requested_max_bytes as u64).into();
    source["summary"] = if file_size > requested_max_bytes as u64 {
        format!(
            "已从会话文件事实中复用 {} 的前 {} 字节",
            path, requested_max_bytes
        )
        .into()
    } else {
        format!("已从会话文件事实中复用文件 {path}").into()
    };
    Some(source.to_string())
}

fn reusable_result_is_current(tool_name: &str, result: &str) -> bool {
    if tool_name != "file_read" {
        return true;
    }
    let Ok(payload) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    let Some(path) = payload.get("path").and_then(Value::as_str) else {
        return false;
    };
    let Some(expected_hash) = payload.get("content_hash").and_then(Value::as_str) else {
        return false;
    };
    magi_snapshot::path_content_hash(Path::new(path))
        .is_ok_and(|actual_hash| actual_hash == expected_hash)
}

fn idempotent_fingerprint(
    tool_call: &ChatToolCall,
    canonical_name: &str,
    tool_registry: Option<&ToolRegistry>,
) -> Option<ToolCallFingerprint> {
    if !is_idempotent_read_tool(canonical_name, tool_registry) {
        return None;
    }
    tool_call_fingerprint(tool_call, canonical_name)
}

fn tool_call_fingerprint(
    tool_call: &ChatToolCall,
    canonical_name: &str,
) -> Option<ToolCallFingerprint> {
    let arguments = serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?;
    Some(ToolCallFingerprint {
        tool_name: canonical_name.to_string(),
        canonical_arguments: canonical_json(&arguments),
    })
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            let rendered = entries
                .into_iter()
                .map(|(key, value)| format!("{}:{}", serde_json::json!(key), canonical_json(value)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{rendered}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}

fn is_idempotent_read_tool(tool_name: &str, tool_registry: Option<&ToolRegistry>) -> bool {
    BuiltinToolName::from_name(tool_name).is_some_and(|tool| tool.is_idempotent_read_operation())
        || tool_registry.is_some_and(|registry| registry.is_idempotent_read_tool(tool_name))
}

fn explicit_single_call_budgets(goal: &str) -> BTreeMap<String, usize> {
    let normalized = goal.trim().to_ascii_lowercase();
    let mut budgets = BTreeMap::new();

    for tool in BuiltinToolName::ALL {
        let name = tool.as_str();
        let chinese = [
            format!("只调用一次 {name}"),
            format!("仅调用一次 {name}"),
            format!("只使用一次 {name}"),
            format!("仅使用一次 {name}"),
            format!("{name} 只调用一次"),
            format!("{name} 仅调用一次"),
        ];
        let english = [
            format!("only call {name} once"),
            format!("call {name} only once"),
            format!("call {name} exactly once"),
            format!("only use {name} once"),
            format!("use {name} only once"),
            format!("use {name} exactly once"),
        ];
        if chinese.iter().any(|pattern| goal.contains(pattern))
            || english.iter().any(|pattern| normalized.contains(pattern))
        {
            budgets.insert(name.to_string(), 1);
        }
    }
    budgets
}

fn reused_result(tool_name: &str, source_result: &str, reason: &str, message: &str) -> String {
    let source_result = serde_json::from_str::<Value>(source_result)
        .unwrap_or_else(|_| Value::String(source_result.to_string()));
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "reused",
        "reason": reason,
        "message": message,
        "source_result": source_result,
    })
    .to_string()
}

fn budget_exhausted_result(tool_name: &str, limit: usize) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "skipped",
        "reason": "tool_call_budget_exhausted",
        "message": format!("用户已要求本轮 {tool_name} 最多调用 {limit} 次；预算已用尽，请基于已有结果继续回答。"),
    })
    .to_string()
}

fn interrupted_call_result(tool_name: &str) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "interrupted",
        "execution": "blocked",
        "reason": "interrupted_tool_outcome_unknown",
        "message": "上次执行在结果持久化前中断，无法确认该非只读操作是否已生效。请先检查当前状态，再决定是否使用不同参数重新执行；系统不会自动重放同一调用。",
    })
    .to_string()
}

fn tool_result_is_interrupted_unknown(result: &str) -> bool {
    tool_result_has_interrupted_execution(result, "unknown")
}

fn tool_result_is_interrupted_not_started(result: &str) -> bool {
    tool_result_has_interrupted_execution(result, "not_started")
}

fn tool_result_has_interrupted_execution(result: &str, execution: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("interrupted")
        && value.get("execution").and_then(Value::as_str) == Some(execution)
}

fn tool_result_is_interrupted(result: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("interrupted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;
    use magi_session_store::{ThreadChatToolCall, ThreadChatToolFunction};

    fn call(id: &str, name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }
    }

    fn persisted_tool_history(call: &ChatToolCall, result: String) -> Vec<ThreadChatMessage> {
        vec![
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: call.id.clone(),
                    kind: call.kind.clone(),
                    function: ThreadChatToolFunction {
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some(result),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call.id.clone()),
                provider_context: Vec::new(),
            },
        ]
    }

    #[test]
    fn restores_file_read_only_when_content_hash_is_current() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("facts.txt");
        std::fs::write(&path, "stable fact").expect("write fixture");
        let content_hash = magi_snapshot::path_content_hash(&path).expect("hash fixture");
        let file_read = call(
            "call-file-read",
            "file_read",
            &serde_json::json!({"path": path}).to_string(),
        );
        let result = serde_json::json!({
            "tool": "file_read",
            "status": "succeeded",
            "path": path,
            "content_hash": content_hash,
            "content": "stable fact"
        })
        .to_string();
        let history = persisted_tool_history(&file_read, result);
        let ledger = ToolExecutionLedger::from_thread_history("继续", &history, None);
        assert!(matches!(
            ledger.plan(std::slice::from_ref(&file_read), None)[0],
            ToolCallExecutionDecision::Reuse { .. }
        ));

        std::fs::write(&path, "changed fact").expect("change fixture");
        let ledger = ToolExecutionLedger::from_thread_history("继续", &history, None);
        assert!(matches!(
            ledger.plan(&[file_read], None)[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }

    #[test]
    fn successful_write_invalidates_file_read_fact_in_same_turn() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("facts.txt");
        std::fs::write(&path, "stable fact").expect("write fixture");
        let file_read = call(
            "call-file-read",
            "file_read",
            &serde_json::json!({"path": path}).to_string(),
        );
        let mut ledger = ToolExecutionLedger::default();
        let read_plan = ledger.plan(std::slice::from_ref(&file_read), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &read_plan[0] else {
            panic!("first file read must execute");
        };
        let content_hash = magi_snapshot::path_content_hash(&path).expect("hash fixture");
        ledger.record_execution(
            &file_read,
            fingerprint.as_ref(),
            &(
                serde_json::json!({
                    "tool": "file_read",
                    "status": "succeeded",
                    "path": path,
                    "content_hash": content_hash,
                    "content": "stable fact"
                })
                .to_string(),
                ExecutionResultStatus::Succeeded,
            ),
        );
        let file_write = call(
            "call-file-write",
            "file_write",
            &serde_json::json!({"path": path, "content": "changed fact"}).to_string(),
        );
        ledger.record_execution(
            &file_write,
            None,
            &("ok".to_string(), ExecutionResultStatus::Succeeded),
        );
        assert!(matches!(
            ledger.plan(&[file_read], None)[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }

    #[test]
    fn reuses_successful_read_only_call_across_model_rounds() {
        let mut ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
        let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("first read call must execute");
        };
        let result = (
            r#"{"tool":"web_search","status":"succeeded","results":["Rust"]}"#.to_string(),
            ExecutionResultStatus::Succeeded,
        );
        ledger.record_execution(&first, fingerprint.as_ref(), &result);

        let repeat = call("call-2", "web_search", r#"{"query":"Rust"}"#);
        assert!(matches!(
            ledger.plan(&[repeat], None)[0],
            ToolCallExecutionDecision::Reuse { .. }
        ));
    }

    #[test]
    fn get_goal_is_never_reused_across_state_mutations() {
        let mut ledger = ToolExecutionLedger::for_task_goal("推进 Goal");
        let first = call("call-goal-1", "get_goal", "{}");
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("first get_goal must execute");
        };
        assert!(fingerprint.is_none());
        ledger.record_execution(
            &first,
            None,
            &(
                r#"{"tool":"get_goal","status":"ok","goal":null,"plan":null}"#.to_string(),
                ExecutionResultStatus::Succeeded,
            ),
        );

        let repeat = call("call-goal-2", "get_goal", "{}");
        assert!(matches!(
            ledger.plan(&[repeat], None)[0],
            ToolCallExecutionDecision::Execute { fingerprint: None }
        ));
    }

    #[test]
    fn treats_object_key_order_as_the_same_idempotent_call() {
        let ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
        let calls = [
            call(
                "call-1",
                "web_search",
                r#"{"query":"Rust","locale":"zh-CN"}"#,
            ),
            call(
                "call-2",
                "web_search",
                r#"{"locale":"zh-CN","query":"Rust"}"#,
            ),
        ];
        let plan = ledger.plan(&calls, None);
        assert!(matches!(
            plan[1],
            ToolCallExecutionDecision::ReuseAfterExecution {
                source_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn honors_explicit_single_call_budget_for_different_arguments() {
        let mut ledger =
            ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
        let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("first call must execute");
        };
        ledger.record_execution(
            &first,
            fingerprint.as_ref(),
            &("ok".to_string(), ExecutionResultStatus::Succeeded),
        );

        let second = call("call-2", "web_search", r#"{"query":"Cargo"}"#);
        assert!(matches!(
            ledger.plan(&[second], None)[0],
            ToolCallExecutionDecision::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn applies_single_call_budget_within_one_model_response() {
        let ledger =
            ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
        let calls = [
            call("call-1", "web_search", r#"{"query":"Rust"}"#),
            call("call-2", "web_search", r#"{"query":"Cargo"}"#),
        ];
        let plan = ledger.plan(&calls, None);
        assert!(matches!(plan[0], ToolCallExecutionDecision::Execute { .. }));
        assert!(matches!(
            plan[1],
            ToolCallExecutionDecision::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn additional_call_instruction_does_not_create_single_call_budget() {
        let ledger = ToolExecutionLedger::for_task_goal(
            "先调用 shell_exec 探查；权限失败时再调用一次 shell_exec 验证止损。",
        );
        let calls = [
            call("call-1", "shell_exec", r#"{"command":"pwd"}"#),
            call("call-2", "shell_exec", r#"{"command":"printf ok"}"#),
        ];

        assert!(
            ledger
                .plan(&calls, None)
                .iter()
                .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. }))
        );
    }

    #[test]
    fn never_deduplicates_write_or_process_operations() {
        let ledger = ToolExecutionLedger::for_task_goal("执行写入");
        let calls = [
            call("call-1", "file_write", r#"{"path":"a","content":"x"}"#),
            call("call-2", "file_write", r#"{"path":"a","content":"x"}"#),
        ];
        assert!(
            ledger
                .plan(&calls, None)
                .iter()
                .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. }))
        );
    }

    #[test]
    fn restores_successful_read_call_from_persisted_thread_history() {
        let history = vec![
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: "call-read".to_string(),
                    kind: "function".to_string(),
                    function: ThreadChatToolFunction {
                        name: "web_search".to_string(),
                        arguments: r#"{"query":"Rust"}"#.to_string(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some(
                    r#"{"tool":"web_search","status":"succeeded","results":["Rust"]}"#.to_string(),
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-read".to_string()),
                provider_context: Vec::new(),
            },
        ];

        let ledger = ToolExecutionLedger::from_thread_history("搜索 Rust", &history, None);
        assert!(matches!(
            ledger.plan(
                &[call("call-repeat", "web_search", r#"{"query":"Rust"}"#)],
                None
            )[0],
            ToolCallExecutionDecision::Reuse { .. }
        ));
    }

    #[test]
    fn blocks_replaying_unknown_write_call_after_interruption() {
        let history = vec![ThreadChatMessage {
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![ThreadChatToolCall {
                id: "call-write".to_string(),
                kind: "function".to_string(),
                function: ThreadChatToolFunction {
                    name: "file_write".to_string(),
                    arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        }];

        let ledger = ToolExecutionLedger::from_thread_history("写入文件", &history, None);
        let decision = ledger
            .plan(
                &[call(
                    "call-write-repeat",
                    "file_write",
                    r#"{"path":"a.txt","content":"x"}"#,
                )],
                None,
            )
            .remove(0);
        assert!(matches!(
            decision,
            ToolCallExecutionDecision::RecoveryBlocked { .. }
        ));
        assert_eq!(
            decision
                .immediate_result()
                .expect("blocked decision should resolve immediately")
                .1,
            ExecutionResultStatus::Failed
        );
    }

    #[test]
    fn reuses_successful_write_call_restored_from_thread_history() {
        let history = vec![
            ThreadChatMessage {
                role: "assistant".to_string(),
                content: None,
                images: Vec::new(),
                tool_calls: vec![ThreadChatToolCall {
                    id: "call-write".to_string(),
                    kind: "function".to_string(),
                    function: ThreadChatToolFunction {
                        name: "file_write".to_string(),
                        arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
                    },
                }],
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            ThreadChatMessage {
                role: "tool".to_string(),
                content: Some(
                    r#"{"tool":"file_write","status":"succeeded","changed_paths":["a.txt"]}"#
                        .to_string(),
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call-write".to_string()),
                provider_context: Vec::new(),
            },
        ];

        let ledger = ToolExecutionLedger::from_thread_history("写入文件", &history, None);
        let decision = ledger
            .plan(
                &[call(
                    "call-write-repeat",
                    "file_write",
                    r#"{"path":"a.txt","content":"x"}"#,
                )],
                None,
            )
            .remove(0);
        let ToolCallExecutionDecision::Reuse { result } = decision else {
            panic!("恢复后相同写操作必须复用已完成结果");
        };
        let payload: Value = serde_json::from_str(&result).expect("reuse result should be json");
        assert_eq!(payload["execution"], "reused");
        assert_eq!(payload["reason"], "completed_before_recovery");
    }

    #[test]
    fn new_user_turn_does_not_reuse_prior_side_effect_or_call_budget() {
        let mut history = vec![ThreadChatMessage {
            role: "user".to_string(),
            content: Some("写入文件".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }];
        let write = call(
            "call-write-prior-turn",
            "file_write",
            r#"{"path":"a.txt","content":"x"}"#,
        );
        history.extend(persisted_tool_history(
            &write,
            r#"{"tool":"file_write","status":"succeeded","changed_paths":["a.txt"]}"#.to_string(),
        ));
        history.push(ThreadChatMessage {
            role: "user".to_string(),
            content: Some("再次写入文件".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });

        let ledger = ToolExecutionLedger::from_thread_history(
            "只调用一次 file_write，再次写入文件",
            &history,
            None,
        );
        assert!(matches!(
            ledger.plan(
                &[call(
                    "call-write-current-turn",
                    "file_write",
                    r#"{"path":"a.txt","content":"x"}"#,
                )],
                None,
            )[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }

    #[test]
    fn new_user_turn_reuses_current_file_fact_but_invalidates_changed_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("facts.txt");
        std::fs::write(&path, "stable fact").expect("write fixture");
        let file_read = call(
            "call-read-prior-turn",
            "file_read",
            &serde_json::json!({"path": path}).to_string(),
        );
        let mut history = vec![ThreadChatMessage {
            role: "user".to_string(),
            content: Some("读取事实".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }];
        history.extend(persisted_tool_history(
            &file_read,
            serde_json::json!({
                "tool": "file_read",
                "status": "succeeded",
                "path": path,
                "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
                "content": "stable fact"
            })
            .to_string(),
        ));
        history.push(ThreadChatMessage {
            role: "user".to_string(),
            content: Some("继续使用事实".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });

        let ledger = ToolExecutionLedger::from_thread_history("继续使用事实", &history, None);
        assert!(matches!(
            ledger.plan(std::slice::from_ref(&file_read), None)[0],
            ToolCallExecutionDecision::Reuse { .. }
        ));

        std::fs::write(&path, "changed fact").expect("change fixture");
        let ledger = ToolExecutionLedger::from_thread_history("继续使用事实", &history, None);
        assert!(matches!(
            ledger.plan(&[file_read], None)[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }

    #[test]
    fn new_task_reuses_session_file_fact_with_different_preview_size() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("Cargo.toml");
        let content = "[workspace]\nresolver = \"2\"\n";
        std::fs::write(&path, content).expect("write fixture");
        let fact = CurrentFileFact {
            absolute_path: path.display().to_string(),
            content_hash: magi_snapshot::path_content_hash(&path).expect("hash fixture"),
            result: serde_json::json!({
                "tool": "file_read",
                "status": "succeeded",
                "mode": "file",
                "path": path,
                "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
                "file_size_bytes": content.len(),
                "max_bytes": 4096,
                "bytes_read": content.len(),
                "truncated": false,
                "encoding": "utf-8-lossy",
                "content": content,
                "summary": "已读取文件"
            })
            .to_string(),
            summary: content.to_string(),
        };
        let call = call(
            "call-current-fact",
            "file_read",
            r#"{"path":"Cargo.toml","max_bytes":12}"#,
        );
        let ledger = ToolExecutionLedger::for_task_goal("继续使用文件事实")
            .with_current_file_facts(&[fact], Some(directory.path()));
        let ToolCallExecutionDecision::Reuse { result } = &ledger.plan(&[call], None)[0] else {
            panic!("新 task 必须复用未变化的会话文件事实");
        };
        let payload: Value = serde_json::from_str(result).expect("reuse result json");
        assert_eq!(payload["reason"], "current_session_file_fact");
        assert_eq!(payload["source_result"]["content"], "[workspace]\n");
        assert_eq!(payload["source_result"]["max_bytes"], 12);
        assert_eq!(payload["source_result"]["truncated"], true);
    }

    #[test]
    fn new_task_does_not_reuse_session_file_fact_after_external_change() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("facts.txt");
        std::fs::write(&path, "stable fact").expect("write fixture");
        let fact = CurrentFileFact {
            absolute_path: path.display().to_string(),
            content_hash: magi_snapshot::path_content_hash(&path).expect("hash fixture"),
            result: serde_json::json!({
                "tool": "file_read",
                "status": "succeeded",
                "mode": "file",
                "path": path,
                "content_hash": magi_snapshot::path_content_hash(&path).expect("hash fixture"),
                "file_size_bytes": 11,
                "max_bytes": 4096,
                "bytes_read": 11,
                "truncated": false,
                "content": "stable fact"
            })
            .to_string(),
            summary: "stable fact".to_string(),
        };
        std::fs::write(&path, "changed fact").expect("change fixture");
        let ledger = ToolExecutionLedger::for_task_goal("继续使用文件事实")
            .with_current_file_facts(&[fact], Some(directory.path()));
        assert!(matches!(
            ledger.plan(
                &[call(
                    "call-changed-fact",
                    "file_read",
                    r#"{"path":"facts.txt"}"#,
                )],
                None,
            )[0],
            ToolCallExecutionDecision::Execute { .. }
        ));
    }
}

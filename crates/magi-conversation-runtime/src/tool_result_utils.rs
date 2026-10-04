//! 任务系统 — 工具调用结果的状态/摘要标准化。
//!
//! runtime 内部的 writeback / round 实现直接访问这些纯函数。

use magi_core::ExecutionResultStatus;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const TOOL_EXECUTION_FAILED_PUBLIC_ERROR: &str = "工具执行失败，请稍后重试";
pub const TOOL_SAFETY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "安全防护要求确认该操作，授权后将继续当前调用";
/// 模型可见的单个工具结果上限。完整结果仍由审计、UI 和恢复状态保存。
pub const MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES: usize = 12 * 1024;
/// 模型视图中历史工具结果总预算的下限。
///
/// 单条结果上限只能阻止一个大文件拖垮请求；长时间的只读探索会累积很多
/// 小结果，仍然把每一轮请求推向上下文上限。这里把模型视图中的工具结果
/// 做一次确定性总量收敛，完整结果继续保留在 thread/audit/UI 中。
pub const MODEL_VISIBLE_TOOL_HISTORY_MIN_BYTES: usize = 48 * 1024;
/// 历史工具结果最多占用上下文窗口的比例（按约 4 字节/token 换算）。
const MODEL_VISIBLE_TOOL_HISTORY_WINDOW_PERCENT: u64 = 25;

/// 按当前模型上下文窗口计算历史工具结果的总预算。
///
/// 预算与窗口成比例：大窗口模型保留更多近期文件与搜索结果，避免上下文尚空时
/// 就截断旧结果导致重复读取；超出部分由上下文压缩处理，而不是固定常数。
pub fn model_visible_tool_history_budget_bytes(context_window_tokens: u64) -> usize {
    let proportional_tokens =
        context_window_tokens.saturating_mul(MODEL_VISIBLE_TOOL_HISTORY_WINDOW_PERCENT) / 100;
    usize::try_from(proportional_tokens.saturating_mul(4))
        .unwrap_or(usize::MAX)
        .max(MODEL_VISIBLE_TOOL_HISTORY_MIN_BYTES)
}
const MODEL_VISIBLE_TOOL_RESULT_FIELD_MAX_BYTES: usize = 2 * 1024;
const MODEL_VISIBLE_TOOL_RESULT_MARKER: &str = "...[model output truncated]...";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicToolError {
    pub error_code: &'static str,
    pub error: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicToolFailure {
    pub summary: String,
    pub detail: String,
}

/// 判断工具结果是否明确声明“相同调用不可重试”，并把它提升为当前任务的终止信号。
///
/// 权限/策略拒绝不是普通的模型纠错失败。若只把拒绝结果继续交回模型，模型可能
/// 改换工具名或参数反复尝试同一个不可能成功的副作用，导致一轮任务长时间空转。
/// 只有结果携带明确的 `retryable_with_same_arguments=false` 契约时才在这里终止，
/// 不影响需要用户授权的 NeedsApproval 流程和可恢复的普通工具失败。
pub fn non_retryable_tool_failure(
    tool_name: &str,
    result: &str,
    status: ExecutionResultStatus,
) -> Option<DeterministicToolFailure> {
    if status != ExecutionResultStatus::Rejected {
        return None;
    }
    let payload = serde_json::from_str::<serde_json::Value>(result).ok()?;
    if payload
        .get("retryable_with_same_arguments")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
    {
        return None;
    }
    let error_code = payload
        .get("error_code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool_policy_rejected");
    // 用户拒绝或授权过期是本次调用的结局，不是策略阻止：结果交给模型改用其他方式
    // 继续，相同调用由拒绝记忆拦截，不应让整轮失败。
    if crate::tool_approval::is_tool_approval_decision_code(error_code) {
        return None;
    }
    let access_profile = payload
        .get("access_profile")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("当前任务策略");
    let required_access_profile = payload
        .get("required_access_profile")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let error = payload
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("该工具已被当前策略阻止");
    let recovery = required_access_profile
        .map(|profile| format!("如需继续，请将访问模式切换为 {profile} 后重新发送任务。"))
        .unwrap_or_else(|| "请改用当前允许的工具或调整任务范围后重新发送。".to_string());
    Some(DeterministicToolFailure {
        summary: format!("{tool_name} 已被当前访问策略阻止，已停止继续重试。"),
        detail: format!(
            "工具：{tool_name}\n错误码：{error_code}\n访问模式：{access_profile}\n原因：{error}\n{recovery}"
        ),
    })
}

/// 单轮对话允许的模型调用轮数上限；任务轮次与对话轮次共用。
pub(crate) const MAX_MODEL_ROUNDS_PER_TURN: usize = 200;
/// 没有任务策略时同一失败调用允许的重试次数（共尝试 retry + 1 次）。
pub(crate) const DEFAULT_TOOL_RETRY_LIMIT: u32 = 1;

/// 达到轮数上限时以明确原因结束本轮，避免模型陷入无限循环。
pub(crate) fn model_round_limit_failure(round: usize) -> Option<DeterministicToolFailure> {
    (round >= MAX_MODEL_ROUNDS_PER_TURN).then(|| DeterministicToolFailure {
        summary: format!("本轮已达到 {MAX_MODEL_ROUNDS_PER_TURN} 次模型调用上限，已停止继续执行。"),
        detail: format!(
            "本轮已连续进行 {MAX_MODEL_ROUNDS_PER_TURN} 次模型调用仍未完成，可能陷入了重复操作。请检查进展后拆分任务或补充说明再继续。"
        ),
    })
}

/// 工具参数的规范形式：JSON 按键排序、去除空白差异；无法解析时取去除首尾空白的原文。
pub(crate) fn normalized_tool_arguments(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.trim().to_string();
    };
    serde_json::to_string(&canonicalize_json(&value))
        .unwrap_or_else(|_| arguments.trim().to_string())
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        _ => value.clone(),
    }
}

/// 重复失败检测：按“工具 + 规范化参数 + 错误”计数。其他调用的成功不会清零计数；
/// 只有同一调用本身成功才说明它不再失败。
#[derive(Clone, Debug, Default)]
pub struct DeterministicToolFailureTracker {
    observations: BTreeMap<String, usize>,
}

impl DeterministicToolFailureTracker {
    pub fn observe(
        &mut self,
        tool_name: &str,
        arguments: &str,
        result: &str,
        status: ExecutionResultStatus,
        retry_limit: u32,
    ) -> Option<DeterministicToolFailure> {
        let call_key = format!(
            "{tool_name}\u{1f}{}\u{1f}",
            normalized_tool_arguments(arguments)
        );
        if status == ExecutionResultStatus::Succeeded {
            self.observations
                .retain(|key, _| !key.starts_with(&call_key));
            return None;
        }
        if !matches!(
            status,
            ExecutionResultStatus::Failed
                | ExecutionResultStatus::Rejected
                | ExecutionResultStatus::NeedsApproval
        ) {
            return None;
        }
        let payload = serde_json::from_str::<serde_json::Value>(result).ok();
        let error_code = payload
            .as_ref()
            .and_then(|payload| payload.get("error_code"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|code| !code.is_empty())
            .unwrap_or("tool_execution_failed");
        let access_profile = payload
            .as_ref()
            .and_then(|payload| payload.get("access_profile"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let key = format!("{call_key}{error_code}\u{1f}{access_profile}");
        let observations = self.observations.entry(key).or_default();
        *observations = observations.saturating_add(1);
        let max_attempts = retry_limit.saturating_add(1) as usize;
        if *observations < max_attempts {
            return None;
        }
        let error = payload
            .as_ref()
            .and_then(|payload| payload.get("error"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(result);
        Some(DeterministicToolFailure {
            summary: format!(
                "{tool_name} 使用相同参数连续失败 {max_attempts} 次，已停止重复执行。"
            ),
            detail: format!(
                "工具：{tool_name}\n错误码：{error_code}\n访问模式：{access_profile}\n原因：{error}\n相同参数和错误重复出现，继续调用不会改变结果；请修正参数、启动依赖服务或由用户调整访问模式后恢复任务。"
            ),
        })
    }
}

pub fn tool_result_execution_status(result: &str) -> ExecutionResultStatus {
    let explicit = serde_json::from_str::<Value>(result)
        .ok()
        .and_then(|payload| payload.get("status")?.as_str().map(str::to_ascii_lowercase));
    match explicit.as_deref() {
        Some("succeeded" | "success" | "ok" | "completed" | "degraded") => {
            ExecutionResultStatus::Succeeded
        }
        Some("rejected" | "blocked" | "denied" | "forbidden") => ExecutionResultStatus::Rejected,
        Some("needs_approval" | "needsapproval") => ExecutionResultStatus::NeedsApproval,
        Some("cancelled" | "canceled" | "aborted" | "killed") => ExecutionResultStatus::Cancelled,
        Some("failed" | "error" | "timeout" | "timed_out") => ExecutionResultStatus::Failed,
        Some("indeterminate") => ExecutionResultStatus::Indeterminate,
        _ if infer_tool_call_status(result) == "success" => ExecutionResultStatus::Succeeded,
        _ => ExecutionResultStatus::Failed,
    }
}

/// 运行时只有在明确声明“尚未产生副作用、可用同一参数继续”时，才允许授权后
/// 重新进入执行入口。缺少该契约的 NeedsApproval 必须作为运行时协议错误处理。
pub(crate) fn approval_resume_is_safe(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|payload| {
            payload
                .get("approval_resume_safe")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false)
}

pub(crate) fn approval_resume_contract_failure(tool_name: &str) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "failed",
            "error_code": "tool_approval_resume_contract_invalid",
            "error": "工具未声明可安全恢复，已阻止授权后重复执行",
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

pub fn safety_gate_public_error(status: ExecutionResultStatus) -> PublicToolError {
    match status {
        ExecutionResultStatus::NeedsApproval => PublicToolError {
            error_code: "tool_safety_needs_approval",
            error: TOOL_SAFETY_NEEDS_APPROVAL_PUBLIC_ERROR,
        },
        ExecutionResultStatus::Rejected => PublicToolError {
            error_code: "tool_safety_rejected",
            error: "该操作已被安全防护阻止",
        },
        _ => PublicToolError {
            error_code: "tool_safety_failed",
            error: "该操作暂不可用",
        },
    }
}

/// 工具调用在实际执行前因中断而放弃时的唯一结果合同。
///
/// 批次执行器在中断后跳过剩余调用、恢复历史为缺失结果补齐时都使用这份 payload；
/// 执行账本按 `execution: not_started` 识别它，不把它计为已执行或可能已生效。
pub(crate) fn tool_interrupted_before_execution_payload(tool_name: &str) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "interrupted",
        "execution": "not_started",
        "reason": "task_interrupted_before_tool_execution_started",
        "message": "本次工具调用在实际执行前已中断，尚未产生外部副作用；如仍有必要，可以重新调用。",
    })
    .to_string()
}

pub(crate) fn tool_interrupted_before_execution_result(
    tool_name: &str,
) -> (String, ExecutionResultStatus) {
    (
        tool_interrupted_before_execution_payload(tool_name),
        ExecutionResultStatus::Cancelled,
    )
}

/// 工具调用当前记录的结果是否仍是“等待授权”：等待中的调用尚未执行，批准后会先改写为
/// running 再执行，因此中断恢复时不能把它当成“可能已经执行”。
pub(crate) fn tool_payload_is_awaiting_approval(payload: &Value) -> bool {
    payload.get("status").and_then(Value::as_str) == Some("awaiting_approval")
}

pub(crate) fn tool_result_is_awaiting_approval(result: &str) -> bool {
    serde_json::from_str::<Value>(result)
        .is_ok_and(|value| tool_payload_is_awaiting_approval(&value))
}

pub(crate) fn tool_result_is_interrupted_not_started(result: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result) else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("interrupted")
        && value.get("execution").and_then(Value::as_str) == Some("not_started")
}

pub fn tool_execution_failed_result(tool_name: &str) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool_name,
            "status": "failed",
            "error_code": "tool_execution_failed",
            "error": TOOL_EXECUTION_FAILED_PUBLIC_ERROR,
        })
        .to_string(),
        ExecutionResultStatus::Failed,
    )
}

pub fn turn_item_status_for_tool_result(status: ExecutionResultStatus) -> &'static str {
    match status {
        ExecutionResultStatus::Succeeded => "completed",
        ExecutionResultStatus::NeedsApproval => "awaiting_approval",
        ExecutionResultStatus::Cancelled => "cancelled",
        ExecutionResultStatus::Failed | ExecutionResultStatus::Rejected => "failed",
        ExecutionResultStatus::Indeterminate => "indeterminate",
    }
}

pub fn infer_tool_call_status(result: &str) -> &'static str {
    let parsed = serde_json::from_str::<serde_json::Value>(result).ok();
    let mut explicit_success = false;
    let mut explicit_degraded = false;
    if let Some(status) = parsed
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
    {
        match status.to_ascii_lowercase().as_str() {
            "error" | "failed" | "blocked" | "rejected" | "needs_approval" | "needsapproval"
            | "timeout" | "timed_out" => return "error",
            "cancelled" | "canceled" | "killed" | "aborted" => return "cancelled",
            "succeeded" | "success" | "ok" | "completed" => explicit_success = true,
            "degraded" => {
                explicit_success = true;
                explicit_degraded = true;
            }
            _ => {}
        }
    }
    if explicit_degraded {
        return "success";
    }
    if parsed
        .as_ref()
        .and_then(|v| v.get("ok"))
        .and_then(|v| v.as_bool())
        .is_some_and(|ok| !ok)
    {
        return "error";
    }
    if parsed.as_ref().and_then(|v| v.get("error")).is_some() {
        return "error";
    }
    if explicit_success {
        return "success";
    }
    let lowered = result.to_ascii_lowercase();
    if [
        "blocked",
        "rejected",
        "denied",
        "forbidden",
        "not allowed",
        "risk policy blocked",
        "restricted access blocked",
        "风险策略拦截",
        "已被拒绝",
        "被拒绝",
        "被阻断",
        "不允许",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return "error";
    }
    "success"
}

pub fn summarize_tool_result(result: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result) {
        for key in ["summary", "message", "error"] {
            if let Some(value) = parsed.get(key).and_then(|value| value.as_str()) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    if result.len() <= 120 {
        return result.to_string();
    }
    let mut end = 120;
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &result[..end])
}

pub fn model_visible_tool_result(result: &str, status: ExecutionResultStatus) -> String {
    if result.len() <= MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES {
        return result.to_string();
    }

    let original_bytes = result.len();
    let mut envelope = Map::new();
    envelope.insert(
        "execution_status".to_string(),
        Value::String(status.wire_label().to_string()),
    );
    envelope.insert("model_truncated".to_string(), Value::Bool(true));
    envelope.insert(
        "original_bytes".to_string(),
        Value::from(original_bytes as u64),
    );

    if let Ok(parsed) = serde_json::from_str::<Value>(result)
        && let Some(object) = parsed.as_object()
    {
        for key in [
            "tool",
            "status",
            "error_code",
            "summary",
            "message",
            "path",
            "content_hash",
            "exit_code",
            "file_size_bytes",
            "bytes_read",
            "truncated",
            "original_token_count",
            "omitted_bytes",
        ] {
            if let Some(value) = object.get(key) {
                let bounded = value
                    .as_str()
                    .map(|text| {
                        Value::String(truncate_utf8_middle(
                            text,
                            MODEL_VISIBLE_TOOL_RESULT_FIELD_MAX_BYTES,
                        ))
                    })
                    .unwrap_or_else(|| value.clone());
                envelope.insert(key.to_string(), bounded);
            }
        }
        if let Some(error) = object.get("error").and_then(Value::as_str) {
            envelope.insert(
                "error".to_string(),
                Value::String(truncate_utf8_middle(
                    error,
                    MODEL_VISIBLE_TOOL_RESULT_FIELD_MAX_BYTES,
                )),
            );
        }
        let preview = ["content", "stdout", "stderr", "output"]
            .into_iter()
            .find_map(|key| object.get(key).and_then(Value::as_str))
            .unwrap_or(result);
        envelope.insert(
            "preview".to_string(),
            Value::String(truncate_utf8_middle(
                preview,
                MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES / 2,
            )),
        );
    }

    if !envelope.contains_key("preview") {
        envelope.insert(
            "preview".to_string(),
            Value::String(truncate_utf8_middle(
                result,
                MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES / 2,
            )),
        );
    }

    let encoded = serde_json::to_string(&envelope).unwrap_or_else(|_| {
        format!(
            "{{\"execution_status\":\"{}\",\"model_truncated\":true,\"original_bytes\":{},\"preview\":{}}}",
            status.wire_label(),
            original_bytes,
            serde_json::to_string(&truncate_utf8_middle(
                result,
                MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES / 2,
            ))
            .unwrap_or_else(|_| "\"[unavailable]\"".to_string())
        )
    });
    if encoded.len() <= MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES {
        encoded
    } else {
        serde_json::json!({
            "execution_status": status.wire_label(),
            "model_truncated": true,
            "original_bytes": original_bytes,
            "preview": truncate_utf8_middle(result, MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES / 3),
        })
        .to_string()
    }
}

/// 对一组已经按单条上限裁剪过的工具结果继续施加总预算。
///
/// 最近结果优先保留完整内容；较早结果降为结构化事实摘要。该函数不调用
/// 模型、不修改持久化结果，调用方只应把返回值用于下一次模型请求的上下文视图。
pub fn bound_model_visible_tool_history(results: &[String], budget_bytes: usize) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    let visible = results
        .iter()
        .map(|result| model_visible_tool_result(result, tool_result_execution_status(result)))
        .collect::<Vec<_>>();
    let total = visible.iter().map(String::len).sum::<usize>();
    if total <= budget_bytes {
        return visible;
    }

    // 至少给最近一批结果留出一半预算；它们最可能直接决定下一步动作。
    let recent_budget = budget_bytes / 2;
    let mut recent_start = visible.len();
    let mut recent_bytes = 0usize;
    for (index, result) in visible.iter().enumerate().rev() {
        if recent_start < visible.len() && recent_bytes.saturating_add(result.len()) > recent_budget
        {
            break;
        }
        recent_start = index;
        recent_bytes = recent_bytes.saturating_add(result.len());
    }
    let old_count = recent_start;
    if old_count == 0 {
        return vec![truncate_history_result(&visible[0], budget_bytes)];
    }
    let old_budget = budget_bytes.saturating_sub(recent_bytes);
    let per_old_budget = old_budget / old_count;
    let mut bounded = Vec::with_capacity(visible.len());
    for (index, result) in visible.into_iter().enumerate() {
        if index < recent_start {
            bounded.push(compact_historical_tool_result(&result, per_old_budget));
        } else {
            bounded.push(result);
        }
    }
    bounded
}

fn compact_historical_tool_result(result: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let parsed = serde_json::from_str::<Value>(result).ok();
    let mut envelope = Map::new();
    envelope.insert("history_compacted".to_string(), Value::Bool(true));
    if let Some(object) = parsed.as_ref().and_then(Value::as_object) {
        for key in [
            "tool",
            "status",
            "error_code",
            "path",
            "content_hash",
            "file_size_bytes",
            "exit_code",
            "summary",
            "message",
            "error",
        ] {
            if let Some(value) = object.get(key) {
                let bounded = value
                    .as_str()
                    .map(|text| {
                        Value::String(truncate_utf8_middle(
                            text,
                            MODEL_VISIBLE_TOOL_RESULT_FIELD_MAX_BYTES,
                        ))
                    })
                    .unwrap_or_else(|| value.clone());
                envelope.insert(key.to_string(), bounded);
            }
        }
    }
    let encoded = serde_json::to_string(&envelope)
        .unwrap_or_else(|_| "{\"history_compacted\":true}".to_string());
    truncate_history_result(&encoded, max_bytes)
}

fn truncate_history_result(result: &str, max_bytes: usize) -> String {
    if result.len() <= max_bytes {
        return result.to_string();
    }
    // 保持 JSON 可解析，避免 tool_result 配对在 bridge 协议边界失效。
    let minimal = "{\"history_compacted\":true}";
    if minimal.len() <= max_bytes {
        return minimal.to_string();
    }
    if max_bytes >= 2 {
        "{}".to_string()
    } else {
        "0".to_string()
    }
}

fn truncate_utf8_middle(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes <= MODEL_VISIBLE_TOOL_RESULT_MARKER.len() {
        return MODEL_VISIBLE_TOOL_RESULT_MARKER
            .chars()
            .take(max_bytes)
            .collect();
    }
    let available = max_bytes - MODEL_VISIBLE_TOOL_RESULT_MARKER.len();
    let mut head_bytes = available / 2;
    while head_bytes > 0 && !value.is_char_boundary(head_bytes) {
        head_bytes -= 1;
    }
    let mut tail_bytes = available.saturating_sub(head_bytes);
    while tail_bytes > 0 && !value.is_char_boundary(value.len() - tail_bytes) {
        tail_bytes -= 1;
    }
    format!(
        "{}{}{}",
        &value[..head_bytes],
        MODEL_VISIBLE_TOOL_RESULT_MARKER,
        &value[value.len().saturating_sub(tail_bytes)..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indeterminate_tool_result_stays_unconfirmed_end_to_end() {
        let status = tool_result_execution_status(
            r#"{"status":"indeterminate","error_code":"browser_host_request_indeterminate"}"#,
        );
        assert_eq!(status, ExecutionResultStatus::Indeterminate);
        assert_eq!(status.wire_label(), "indeterminate");
        assert_eq!(turn_item_status_for_tool_result(status), "indeterminate");
    }

    #[test]
    fn status_labels_are_stable() {
        assert_eq!(ExecutionResultStatus::Succeeded.wire_label(), "succeeded");
        assert_eq!(
            turn_item_status_for_tool_result(ExecutionResultStatus::NeedsApproval),
            "awaiting_approval"
        );
        assert_eq!(
            turn_item_status_for_tool_result(ExecutionResultStatus::Cancelled),
            "cancelled"
        );
    }

    #[test]
    fn tool_result_execution_status_preserves_recovery_semantics() {
        assert_eq!(
            tool_result_execution_status(r#"{"status":"needs_approval"}"#),
            ExecutionResultStatus::NeedsApproval
        );
        assert_eq!(
            tool_result_execution_status(r#"{"status":"rejected"}"#),
            ExecutionResultStatus::Rejected
        );
        assert_eq!(
            tool_result_execution_status(r#"{"status":"succeeded"}"#),
            ExecutionResultStatus::Succeeded
        );
    }

    #[test]
    fn infer_tool_call_status_prefers_status_field() {
        assert_eq!(infer_tool_call_status(r#"{"status":"failed"}"#), "error");
        assert_eq!(infer_tool_call_status(r#"{"status":"blocked"}"#), "error");
        assert_eq!(
            infer_tool_call_status(r#"{"status":"cancelled"}"#),
            "cancelled"
        );
        assert_eq!(
            infer_tool_call_status(r#"{"status":"needs_approval"}"#),
            "error"
        );
        assert_eq!(
            infer_tool_call_status(r#"{"status":"ok","error":"boom"}"#),
            "error"
        );
        assert_eq!(infer_tool_call_status(r#"{"status":"ok"}"#), "success");
        assert_eq!(
            infer_tool_call_status(r#"{"status":"degraded","error":"代理不可用"}"#),
            "success"
        );
        assert_eq!(
            infer_tool_call_status("高风险工具已被当前风险策略拦截: shell_exec"),
            "error"
        );
    }

    #[test]
    fn summarize_tool_result_prefers_structured_summary() {
        let summary = summarize_tool_result(
            r#"{"status":"succeeded","summary":"命令执行成功","stdout":"large body"}"#,
        );

        assert_eq!(summary, "命令执行成功");
    }

    #[test]
    fn summarize_tool_result_truncates_long_payloads() {
        let summary = summarize_tool_result(&"x".repeat(130));

        assert_eq!(summary.chars().count(), 121);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn model_visible_tool_result_keeps_success_payload() {
        let result = r#"{"status":"succeeded","stdout":"ok"}"#;

        assert_eq!(
            model_visible_tool_result(result, ExecutionResultStatus::Succeeded),
            result
        );
    }

    #[test]
    fn model_visible_tool_result_keeps_structured_error_for_recovery() {
        let result = r#"{"status":"needs_approval","error_code":"tool_policy_needs_approval","error":"该操作需要你的确认，授权后将继续当前调用","access_profile":"restricted","required_access_profile":"full_access"}"#;

        assert_eq!(
            model_visible_tool_result(result, ExecutionResultStatus::NeedsApproval),
            result
        );
    }

    #[test]
    fn model_visible_tool_result_keeps_file_patch_recovery_details() {
        let result = r#"{"status":"failed","error_code":"file_patch_no_match","error":"目标内容与当前文件不匹配，请重新读取文件后再修改","errors":["patch[0]: old_string 未在文件中找到"]}"#;

        assert_eq!(
            model_visible_tool_result(result, ExecutionResultStatus::Failed),
            result
        );
    }

    #[test]
    fn model_visible_tool_result_bounds_large_structured_payload() {
        let result = serde_json::json!({
            "tool": "shell_exec",
            "status": "succeeded",
            "stdout": "前缀".to_string() + &"x".repeat(50_000) + "后缀",
            "content_hash": "sha256:test-content",
            "exit_code": 0,
        })
        .to_string();

        let visible = model_visible_tool_result(&result, ExecutionResultStatus::Succeeded);
        assert!(visible.len() <= MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES);
        let parsed: Value = serde_json::from_str(&visible).expect("裁剪结果必须保持 JSON");
        assert_eq!(parsed["tool"], "shell_exec");
        assert_eq!(parsed["status"], "succeeded");
        assert_eq!(parsed["content_hash"], "sha256:test-content");
        assert_eq!(parsed["model_truncated"], true);
        assert_eq!(parsed["original_bytes"], result.len());
        assert!(
            parsed["preview"]
                .as_str()
                .unwrap()
                .contains("model output truncated")
        );
    }

    #[test]
    fn tool_history_budget_is_proportional_to_the_context_window() {
        assert_eq!(model_visible_tool_history_budget_bytes(256_000), 256_000);
        assert_eq!(
            model_visible_tool_history_budget_bytes(1_000_000),
            1_000_000
        );
        assert_eq!(
            model_visible_tool_history_budget_bytes(16_000),
            MODEL_VISIBLE_TOOL_HISTORY_MIN_BYTES
        );
    }

    #[test]
    fn bound_model_visible_tool_history_applies_a_total_budget_and_keeps_recent_results() {
        let results = (0..80)
            .map(|index| {
                serde_json::json!({
                    "tool": "file_read",
                    "status": "succeeded",
                    "path": format!("src/file-{index}.rs"),
                    "content_hash": format!("sha256:{index:064}"),
                    "content": "x".repeat(8_000),
                })
                .to_string()
            })
            .collect::<Vec<_>>();

        let bounded =
            bound_model_visible_tool_history(&results, MODEL_VISIBLE_TOOL_HISTORY_MIN_BYTES);

        assert!(
            bounded.iter().map(String::len).sum::<usize>() <= MODEL_VISIBLE_TOOL_HISTORY_MIN_BYTES
        );
        assert!(
            bounded
                .last()
                .is_some_and(|result| result.contains("content"))
        );
        assert!(
            bounded
                .first()
                .is_some_and(|result| result.contains("history_compacted"))
        );
        assert!(
            bounded
                .iter()
                .all(|result| { serde_json::from_str::<Value>(result).is_ok() })
        );
    }

    #[test]
    fn deterministic_policy_failure_stops_after_second_observation() {
        let mut tracker = DeterministicToolFailureTracker::default();
        let result = r#"{"status":"needs_approval","error_code":"tool_policy_needs_approval","error":"需要完全访问","access_profile":"restricted"}"#;

        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"printf test"}"#,
                    result,
                    ExecutionResultStatus::NeedsApproval,
                    1,
                )
                .is_none()
        );
        let failure = tracker
            .observe(
                "shell_exec",
                r#"{"command":"printf test"}"#,
                result,
                ExecutionResultStatus::NeedsApproval,
                1,
            )
            .expect("第二次相同策略失败必须止损");
        assert!(failure.summary.contains("停止重复执行"));
        assert!(failure.detail.contains("tool_policy_needs_approval"));
    }

    #[test]
    fn successful_tool_call_resets_deterministic_failure_observations() {
        let mut tracker = DeterministicToolFailureTracker::default();
        let result = r#"{"status":"rejected","error_code":"tool_policy_rejected","error":"不可用","access_profile":"read_only"}"#;

        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"printf test"}"#,
                    result,
                    ExecutionResultStatus::Rejected,
                    1,
                )
                .is_none()
        );
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"printf test"}"#,
                    r#"{"status":"succeeded"}"#,
                    ExecutionResultStatus::Succeeded,
                    1,
                )
                .is_none()
        );
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"printf test"}"#,
                    result,
                    ExecutionResultStatus::Rejected,
                    1,
                )
                .is_none()
        );
    }

    #[test]
    fn identical_failed_tool_call_stops_at_task_retry_limit() {
        let mut tracker = DeterministicToolFailureTracker::default();
        let result = r#"{"status":"failed","error_code":"browser_navigation_failed","error":"connection refused","recoverable":true}"#;
        let arguments = r#"{"url":"http://127.0.0.1:4174/"}"#;

        assert!(
            tracker
                .observe(
                    "browser_navigate",
                    arguments,
                    result,
                    ExecutionResultStatus::Failed,
                    1,
                )
                .is_none()
        );
        let failure = tracker
            .observe(
                "browser_navigate",
                arguments,
                result,
                ExecutionResultStatus::Failed,
                1,
            )
            .expect("任务 retry_limit=1 时第二次相同失败必须止损");
        assert!(failure.summary.contains("连续失败 2 次"));
        assert!(failure.detail.contains("browser_navigation_failed"));
    }

    #[test]
    fn repeated_failure_is_counted_by_normalized_arguments_and_not_reset_by_other_successes() {
        let mut tracker = DeterministicToolFailureTracker::default();
        let failed = r#"{"status":"failed","error_code":"shell_exit_nonzero","error":"exit 1"}"#;
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"npm test"}"#,
                    failed,
                    ExecutionResultStatus::Failed,
                    1,
                )
                .is_none()
        );
        // 同名工具的另一条命令成功，不能清零 npm test 的失败计数。
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"command":"ls"}"#,
                    r#"{"status":"succeeded"}"#,
                    ExecutionResultStatus::Succeeded,
                    1,
                )
                .is_none()
        );
        // 只改了空白的相同调用仍算重复失败。
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{ "command" : "npm test" }"#,
                    failed,
                    ExecutionResultStatus::Failed,
                    1,
                )
                .is_some(),
            "相同调用的重复失败必须在上限内停止"
        );
    }

    #[test]
    fn model_round_limit_stops_turn_at_shared_cap() {
        assert!(model_round_limit_failure(MAX_MODEL_ROUNDS_PER_TURN - 1).is_none());
        let failure = model_round_limit_failure(MAX_MODEL_ROUNDS_PER_TURN)
            .expect("达到轮数上限必须以明确原因结束本轮");
        assert!(failure.summary.contains("模型调用上限"));
    }

    #[test]
    fn non_retryable_policy_rejection_stops_without_waiting_for_duplicate_calls() {
        let result = r#"{
            "status":"rejected",
            "error_code":"tool_policy_rejected",
            "error":"该工具在当前访问模式下不可用",
            "access_profile":"read_only",
            "required_access_profile":"full_access",
            "retryable_with_same_arguments":false
        }"#;

        let failure =
            non_retryable_tool_failure("shell_exec", result, ExecutionResultStatus::Rejected)
                .expect("明确不可重试的权限拒绝必须立即终止任务");
        assert!(failure.summary.contains("停止继续重试"));
        assert!(failure.detail.contains("切换为 full_access"));
    }

    #[test]
    fn retryable_or_approval_results_do_not_trigger_terminal_policy_failure() {
        let needs_approval = r#"{
            "status":"needs_approval",
            "error_code":"tool_policy_needs_approval",
            "retryable_with_same_arguments":false
        }"#;
        assert!(
            non_retryable_tool_failure(
                "shell_exec",
                needs_approval,
                ExecutionResultStatus::NeedsApproval,
            )
            .is_none()
        );

        let retryable_rejection = r#"{
            "status":"rejected",
            "error_code":"tool_approval_denied",
            "retryable_with_same_arguments":true
        }"#;
        assert!(
            non_retryable_tool_failure(
                "shell_exec",
                retryable_rejection,
                ExecutionResultStatus::Rejected,
            )
            .is_none()
        );
    }
}

//! 任务系统 — 工具调用结果的状态/摘要标准化。
//!
//! runtime 内部的 writeback / round 实现直接访问这些纯函数。

use magi_core::ExecutionResultStatus;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const TOOL_EXECUTION_FAILED_PUBLIC_ERROR: &str = "工具执行失败，请稍后重试";
pub const TOOL_SAFETY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "安全防护已在受限访问下拦截该操作，请切换为完全访问权限后重试";
/// 模型可见的单个工具结果上限。完整结果仍由审计、UI 和恢复状态保存。
pub const MODEL_VISIBLE_TOOL_RESULT_MAX_BYTES: usize = 12 * 1024;
/// 单轮模型上下文中所有历史工具结果的总预算。
///
/// 单条结果上限只能阻止一个大文件拖垮请求；长时间的只读探索会累积很多
/// 小结果，仍然把每一轮请求推向上下文上限。这里把模型视图中的工具结果
/// 做一次确定性总量收敛，完整结果继续保留在 thread/audit/UI 中。
pub const MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES: usize = 48 * 1024;
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
        if status == ExecutionResultStatus::Succeeded {
            let prefix = format!("{tool_name}\u{1f}");
            self.observations.retain(|key, _| !key.starts_with(&prefix));
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
        let key = format!("{tool_name}\u{1f}{arguments}\u{1f}{error_code}\u{1f}{access_profile}");
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

pub fn tool_execution_status_label(status: ExecutionResultStatus) -> &'static str {
    match status {
        ExecutionResultStatus::Succeeded => "succeeded",
        ExecutionResultStatus::Failed => "failed",
        ExecutionResultStatus::Rejected => "rejected",
        ExecutionResultStatus::NeedsApproval => "needs_approval",
        ExecutionResultStatus::Cancelled => "cancelled",
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
        _ if infer_tool_call_status(result) == "success" => ExecutionResultStatus::Succeeded,
        _ => ExecutionResultStatus::Failed,
    }
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
        ExecutionResultStatus::NeedsApproval => "failed",
        ExecutionResultStatus::Failed
        | ExecutionResultStatus::Rejected
        | ExecutionResultStatus::Cancelled => "failed",
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
            "error" | "failed" | "blocked" | "rejected" | "cancelled" | "canceled"
            | "needs_approval" | "needsapproval" | "timeout" | "timed_out" | "killed"
            | "aborted" => return "error",
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
        Value::String(tool_execution_status_label(status).to_string()),
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
            tool_execution_status_label(status),
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
            "execution_status": tool_execution_status_label(status),
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
pub fn bound_model_visible_tool_history(results: &[String]) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    let visible = results
        .iter()
        .map(|result| model_visible_tool_result(result, tool_result_execution_status(result)))
        .collect::<Vec<_>>();
    let total = visible.iter().map(String::len).sum::<usize>();
    if total <= MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES {
        return visible;
    }

    // 至少给最近一批结果留出一半预算；它们最可能直接决定下一步动作。
    let recent_budget = MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES / 2;
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
        return vec![truncate_history_result(
            &visible[0],
            MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES,
        )];
    }
    let old_budget = MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES.saturating_sub(recent_bytes);
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
    fn status_labels_are_stable() {
        assert_eq!(
            tool_execution_status_label(ExecutionResultStatus::Succeeded),
            "succeeded"
        );
        assert_eq!(
            turn_item_status_for_tool_result(ExecutionResultStatus::NeedsApproval),
            "failed"
        );
        assert_eq!(
            turn_item_status_for_tool_result(ExecutionResultStatus::Cancelled),
            "failed"
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
        let result = r#"{"status":"needs_approval","error_code":"tool_policy_needs_approval","error":"受限访问已拦截该操作，请切换为完全访问权限后重试","access_profile":"restricted","required_access_profile":"full_access"}"#;

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

        let bounded = bound_model_visible_tool_history(&results);

        assert!(
            bounded.iter().map(String::len).sum::<usize>() <= MODEL_VISIBLE_TOOL_HISTORY_MAX_BYTES
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
}

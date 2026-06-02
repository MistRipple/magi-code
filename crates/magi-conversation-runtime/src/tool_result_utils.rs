//! 任务系统 — 工具调用结果的状态/摘要标准化。
//!
//! runtime 内部的 writeback / round 实现直接访问这些纯函数。

use magi_core::ExecutionResultStatus;

pub const TOOL_EXECUTION_FAILED_PUBLIC_ERROR: &str = "工具执行失败，请稍后重试";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicToolError {
    pub error_code: &'static str,
    pub error: &'static str,
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

pub fn safety_gate_public_error(status: ExecutionResultStatus) -> PublicToolError {
    match status {
        ExecutionResultStatus::NeedsApproval => PublicToolError {
            error_code: "tool_safety_needs_approval",
            error: "该操作触发安全防护，需要批准后执行",
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
        "requires approval",
        "needs approval",
        "人工审批",
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
            infer_tool_call_status("高风险工具必须人工审批: shell_exec"),
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
}

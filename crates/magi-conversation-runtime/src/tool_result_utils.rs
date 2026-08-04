//! 任务系统 — 工具调用结果的状态/摘要标准化。
//!
//! runtime 内部的 writeback / round 实现直接访问这些纯函数。

use magi_core::ExecutionResultStatus;
use std::collections::BTreeMap;

pub const TOOL_EXECUTION_FAILED_PUBLIC_ERROR: &str = "工具执行失败，请稍后重试";
pub const TOOL_SAFETY_NEEDS_APPROVAL_PUBLIC_ERROR: &str =
    "安全防护已在受限访问下拦截该操作，请切换为完全访问权限后重试";

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
        result: &str,
        status: ExecutionResultStatus,
    ) -> Option<DeterministicToolFailure> {
        if status == ExecutionResultStatus::Succeeded {
            let prefix = format!("{tool_name}\u{1f}");
            self.observations.retain(|key, _| !key.starts_with(&prefix));
            return None;
        }
        if !matches!(
            status,
            ExecutionResultStatus::Rejected | ExecutionResultStatus::NeedsApproval
        ) {
            return None;
        }
        let payload = serde_json::from_str::<serde_json::Value>(result).ok()?;
        let error_code = payload.get("error_code")?.as_str()?.trim();
        if !matches!(
            error_code,
            "tool_policy_rejected"
                | "tool_policy_needs_approval"
                | "tool_safety_rejected"
                | "tool_safety_needs_approval"
                | "skill_tool_policy_rejected"
                | "skill_tool_needs_approval"
        ) {
            return None;
        }
        let access_profile = payload
            .get("access_profile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let key = format!("{tool_name}\u{1f}{error_code}\u{1f}{access_profile}");
        let observations = self.observations.entry(key).or_default();
        *observations = observations.saturating_add(1);
        if *observations < 2 {
            return None;
        }
        let error = payload
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("工具调用被确定性策略拒绝");
        Some(DeterministicToolFailure {
            summary: format!("{tool_name} 在当前运行权限下重复被拒绝，已停止重复执行。"),
            detail: format!(
                "工具：{tool_name}\n错误码：{error_code}\n访问模式：{access_profile}\n原因：{error}\n相同权限条件下重复调用不会成功；需要改用当前权限允许的工具，或由用户调整访问模式后恢复任务。"
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

pub fn model_visible_tool_result(result: &str, _status: ExecutionResultStatus) -> String {
    result.to_string()
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
    fn deterministic_policy_failure_stops_after_second_observation() {
        let mut tracker = DeterministicToolFailureTracker::default();
        let result = r#"{"status":"needs_approval","error_code":"tool_policy_needs_approval","error":"需要完全访问","access_profile":"restricted"}"#;

        assert!(
            tracker
                .observe("shell_exec", result, ExecutionResultStatus::NeedsApproval)
                .is_none()
        );
        let failure = tracker
            .observe("shell_exec", result, ExecutionResultStatus::NeedsApproval)
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
                .observe("shell_exec", result, ExecutionResultStatus::Rejected)
                .is_none()
        );
        assert!(
            tracker
                .observe(
                    "shell_exec",
                    r#"{"status":"succeeded"}"#,
                    ExecutionResultStatus::Succeeded,
                )
                .is_none()
        );
        assert!(
            tracker
                .observe("shell_exec", result, ExecutionResultStatus::Rejected)
                .is_none()
        );
    }
}

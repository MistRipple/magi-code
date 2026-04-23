use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentTask {
    pub task_name: String,
    pub ownership_hint: String,
    pub mode_hint: String,
    pub goal: String,
    pub acceptance: Vec<String>,
    pub constraints: Vec<String>,
    pub context: Vec<String>,
    pub requires_modification: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignmentDispatchPayload {
    pub mission_title: Option<String>,
    pub tasks: Vec<AssignmentTask>,
}

#[derive(Clone, Debug)]
pub struct AssignmentDispatchRequest {
    pub id: String,
    pub payload: AssignmentDispatchPayload,
    pub raw_json: String,
}

#[derive(Clone, Debug)]
pub enum AssignmentDispatchDecision {
    None,
    Dispatch {
        request: AssignmentDispatchRequest,
        stripped_text: String,
    },
    BlockedTerminalHandoff {
        request: AssignmentDispatchRequest,
        stripped_text: String,
    },
}

static DISPATCH_NARRATIVE_BOUNDARY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?:^|[：:\s])(?:我将安排|我会安排|我将派发|我会派发|接下来安排|继续安排)")
            .unwrap(),
        Regex::new(r"分派给.{0,20}Worker").unwrap(),
        Regex::new(r"(?:编排|任务).{0,8}(?:已派发|派发完成)").unwrap(),
        Regex::new(r"派发.{0,20}Worker").unwrap(),
        Regex::new(r"并行执行中").unwrap(),
        Regex::new(r"等待\s*Worker").unwrap(),
        Regex::new(r"当前状态").unwrap(),
        Regex::new(r"返回结果后").unwrap(),
        Regex::new(r"完成后我会").unwrap(),
        Regex::new(r"汇总所有\s*Worker").unwrap(),
        Regex::new(r"验证文件是否正确写入").unwrap(),
        Regex::new(r"协作完成[:：]?$").unwrap(),
    ]
});

static DISPATCH_JSON_FRAGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""tasks"\s*:|"mission_title"\s*:|"task_name"\s*:|"ownership_hint"\s*:"#).unwrap()
});

static FENCED_JSON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```json\s*([\s\S]*?)```").unwrap());

pub fn decide_assignment_dispatch(
    text: &str,
    request_id: Option<&str>,
    round: u32,
    terminal_handoff_active: bool,
) -> AssignmentDispatchDecision {
    let request = match extract_assignment_dispatch(text, request_id, round) {
        Some(r) => r,
        None => return AssignmentDispatchDecision::None,
    };

    let stripped = strip_dispatch_text(text);

    if terminal_handoff_active {
        return AssignmentDispatchDecision::BlockedTerminalHandoff {
            request,
            stripped_text: stripped,
        };
    }

    AssignmentDispatchDecision::Dispatch {
        request,
        stripped_text: stripped,
    }
}

fn extract_assignment_dispatch(
    text: &str,
    request_id: Option<&str>,
    round: u32,
) -> Option<AssignmentDispatchRequest> {
    let json_candidate = extract_json_candidate(text)?;
    let parsed: Value = serde_json::from_str(&json_candidate).ok()?;

    if !is_valid_dispatch_payload(&parsed) {
        return None;
    }

    let payload: AssignmentDispatchPayload = serde_json::from_value(parsed).ok()?;

    let req_part = request_id
        .map(|s| s.replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_"))
        .unwrap_or_else(|| "request".to_string());

    let id = format!(
        "structured_assignment_dispatch_{}_round_{}",
        req_part, round
    );

    Some(AssignmentDispatchRequest {
        id,
        payload,
        raw_json: json_candidate,
    })
}

fn extract_json_candidate(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(caps) = FENCED_JSON.captures(trimmed) {
        let inner = caps.get(1)?.as_str().trim();
        if !inner.is_empty() {
            return Some(inner.to_string());
        }
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if serde_json::from_str::<Value>(trimmed).is_ok() {
            return Some(trimmed.to_string());
        }
    }

    let json_offset = find_line_started_json_offset(text);
    if json_offset >= 0 {
        let candidate = text[json_offset as usize..].trim();
        if candidate.starts_with('{') && candidate.ends_with('}') {
            if serde_json::from_str::<Value>(candidate).is_ok() {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

fn is_valid_dispatch_payload(value: &Value) -> bool {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return false,
    };

    let tasks = match obj.get("tasks").and_then(|t| t.as_array()) {
        Some(t) if !t.is_empty() => t,
        _ => return false,
    };

    tasks.iter().all(|task| {
        let t = match task.as_object() {
            Some(o) => o,
            None => return false,
        };
        t.get("task_name").and_then(|v| v.as_str()).is_some()
            && t.get("ownership_hint").and_then(|v| v.as_str()).is_some()
            && t.get("mode_hint").and_then(|v| v.as_str()).is_some()
            && t.get("goal").and_then(|v| v.as_str()).is_some()
            && t.get("acceptance").and_then(|v| v.as_array()).is_some()
            && t.get("constraints").and_then(|v| v.as_array()).is_some()
            && t.get("context").and_then(|v| v.as_array()).is_some()
            && t.get("requires_modification")
                .and_then(|v| v.as_bool())
                .is_some()
    })
}

fn find_line_started_json_offset(text: &str) -> i64 {
    let mut offset: usize = 0;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('{') {
            let start_offset = offset + (line.len() - trimmed.len());
            return start_offset as i64;
        }
        offset += line.len() + 1;
    }
    -1
}

fn find_payload_offset(text: &str) -> Option<usize> {
    let fence_offset = find_line_started_fence_offset(text);
    if let Some(foff) = fence_offset {
        let suffix = &text[foff..];
        if DISPATCH_JSON_FRAGMENT.is_match(suffix) {
            return Some(foff);
        }
        return None;
    }

    let json_offset = find_line_started_json_offset(text);
    if json_offset >= 0 {
        let suffix = &text[json_offset as usize..];
        if DISPATCH_JSON_FRAGMENT.is_match(suffix) {
            return Some(json_offset as usize);
        }
    }

    None
}

fn find_line_started_fence_offset(text: &str) -> Option<usize> {
    let mut offset: usize = 0;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let start_offset = offset + (line.len() - trimmed.len());
            return Some(start_offset);
        }
        offset += line.len() + 1;
    }
    None
}

fn is_narrative_boundary_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('|') && trimmed.ends_with('|') {
        return true;
    }
    DISPATCH_NARRATIVE_BOUNDARY
        .iter()
        .any(|p| p.is_match(trimmed))
}

fn find_narrative_boundary_offset(text: &str) -> Option<usize> {
    let mut offset: usize = 0;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if is_narrative_boundary_line(trimmed) {
            let start_offset = offset + (line.len() - trimmed.len());
            return Some(start_offset);
        }
        offset += line.len() + 1;
    }
    None
}

fn normalize_visible_prefix(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut pending_blank = false;
    let normalized = text.replace("\r\n", "\n");

    for raw_line in normalized.split('\n') {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            if !lines.is_empty() {
                pending_blank = true;
            }
            continue;
        }
        if pending_blank {
            lines.push("");
            pending_blank = false;
        }
        lines.push(line);
    }

    lines.join("\n").trim().to_string()
}

pub fn strip_dispatch_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let payload_offset = match find_payload_offset(text) {
        Some(off) => off,
        None => return text.to_string(),
    };

    let prefix = &text[..payload_offset];
    let boundary = find_narrative_boundary_offset(prefix);
    let visible = match boundary {
        Some(off) => &prefix[..off],
        None => prefix,
    };

    normalize_visible_prefix(visible)
}

pub fn strip_dispatch_preview_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    if let Some(fence_offset) = find_line_started_fence_offset(text) {
        let suffix = &text[fence_offset..];
        if DISPATCH_JSON_FRAGMENT.is_match(suffix) || suffix.trim_start().starts_with('{') {
            return normalize_visible_prefix(&text[..fence_offset]);
        }
    }

    let json_offset = find_line_started_json_offset(text);
    if json_offset >= 0 {
        let suffix = &text[json_offset as usize..];
        if DISPATCH_JSON_FRAGMENT.is_match(suffix) || suffix.trim_start().starts_with('{') {
            return normalize_visible_prefix(&text[..json_offset as usize]);
        }
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dispatch_json() -> String {
        r#"{
            "mission_title": "实现用户认证",
            "tasks": [{
                "task_name": "实现 JWT 验证",
                "ownership_hint": "backend",
                "mode_hint": "implement",
                "goal": "实现 JWT token 验证中间件",
                "acceptance": ["通过单元测试", "验证 token 过期"],
                "constraints": ["使用 jsonwebtoken crate"],
                "context": ["现有 auth 模块在 crates/auth/"],
                "requires_modification": true
            }]
        }"#
        .to_string()
    }

    #[test]
    fn extract_dispatch_from_fenced_json() {
        let text = format!(
            "我将安排以下任务：\n```json\n{}\n```",
            sample_dispatch_json()
        );
        let result = decide_assignment_dispatch(&text, Some("req-1"), 1, false);
        match result {
            AssignmentDispatchDecision::Dispatch {
                request,
                stripped_text,
            } => {
                assert_eq!(request.payload.tasks.len(), 1);
                assert_eq!(request.payload.tasks[0].task_name, "实现 JWT 验证");
                assert!(!stripped_text.contains("```json"));
            }
            _ => panic!("expected Dispatch"),
        }
    }

    #[test]
    fn extract_dispatch_from_raw_json() {
        let json = sample_dispatch_json();
        let result = decide_assignment_dispatch(&json, None, 2, false);
        match result {
            AssignmentDispatchDecision::Dispatch { request, .. } => {
                assert!(request.id.contains("round_2"));
                assert_eq!(
                    request.payload.mission_title.as_deref(),
                    Some("实现用户认证")
                );
            }
            _ => panic!("expected Dispatch"),
        }
    }

    #[test]
    fn returns_none_for_non_dispatch_text() {
        let text = "这是普通的回复文本，没有任何 JSON。";
        let result = decide_assignment_dispatch(text, None, 1, false);
        assert!(matches!(result, AssignmentDispatchDecision::None));
    }

    #[test]
    fn blocks_during_terminal_handoff() {
        let json = sample_dispatch_json();
        let result = decide_assignment_dispatch(&json, None, 1, true);
        assert!(matches!(
            result,
            AssignmentDispatchDecision::BlockedTerminalHandoff { .. }
        ));
    }

    #[test]
    fn rejects_invalid_payload_missing_required_fields() {
        let text = r#"{"tasks": [{"task_name": "test"}]}"#;
        let result = decide_assignment_dispatch(text, None, 1, false);
        assert!(matches!(result, AssignmentDispatchDecision::None));
    }

    #[test]
    fn rejects_empty_tasks() {
        let text = r#"{"tasks": []}"#;
        let result = decide_assignment_dispatch(text, None, 1, false);
        assert!(matches!(result, AssignmentDispatchDecision::None));
    }

    #[test]
    fn strip_removes_narrative_boundary() {
        let json = sample_dispatch_json();
        let text = format!("分析完成。\n我将安排以下任务：\n```json\n{}\n```", json);
        let stripped = strip_dispatch_text(&text);
        assert_eq!(stripped, "分析完成。");
    }

    #[test]
    fn strip_preserves_text_without_dispatch() {
        let text = "这是普通回复，没有 dispatch 内容。";
        let stripped = strip_dispatch_text(text);
        assert_eq!(stripped, text);
    }

    #[test]
    fn extract_handles_json_with_prefix_text() {
        let json = sample_dispatch_json();
        let text = format!("经过分析，我决定派发以下任务：\n{}", json);
        let result = decide_assignment_dispatch(&text, Some("test-req"), 3, false);
        match result {
            AssignmentDispatchDecision::Dispatch { request, .. } => {
                assert_eq!(request.payload.tasks.len(), 1);
            }
            _ => panic!("expected Dispatch"),
        }
    }
}

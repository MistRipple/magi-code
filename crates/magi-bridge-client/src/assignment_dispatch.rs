//! 派发负载残留清洗工具。
//!
//! P3 之前此模块同时承载两套契约：LLM JSON 派发决策器（`decide_assignment_dispatch`/
//! `AssignmentDispatchPayload` 等）与文本清洗器（`strip_dispatch_text` 等）。派发决策
//! 链路实际从未被上游接入（只有单测），Magi 运行期走的是 `task_plan_tool` 结构化
//! 契约。保留死代码会继续误导文档和 LLM 输出，因此 P3 将其彻底移除，只保留仍在被
//! `magi-api::prompt_utils` 消费的文本清洗器：当旧模型偶发在可见内容里漏出 JSON
//! 片段时，这些函数负责把它们从用户视图中抹掉。
//!
//! 若未来需要重新引入"LLM 自主派发"协议，应作为 P6 的 thread 原语下沉实现，而不是
//! 在此文件里复活。
use regex::Regex;
use std::sync::LazyLock;

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

    const SAMPLE_PAYLOAD: &str = r#"{
        "mission_title": "实现用户认证",
        "tasks": [{
            "task_name": "实现 JWT 验证",
            "ownership_hint": "backend"
        }]
    }"#;

    #[test]
    fn strip_removes_narrative_boundary_before_payload() {
        let text = format!(
            "分析完成。\n我将安排以下任务：\n```json\n{}\n```",
            SAMPLE_PAYLOAD
        );
        let stripped = strip_dispatch_text(&text);
        assert_eq!(stripped, "分析完成。");
    }

    #[test]
    fn strip_preserves_text_without_payload() {
        let text = "这是普通回复，没有 dispatch 内容。";
        assert_eq!(strip_dispatch_text(text), text);
    }

    #[test]
    fn preview_removes_partial_payload_when_fence_opened() {
        let text = format!("正在规划任务...\n```json\n{}", SAMPLE_PAYLOAD);
        let stripped = strip_dispatch_preview_text(&text);
        assert_eq!(stripped, "正在规划任务...");
    }

    #[test]
    fn preview_is_noop_for_short_text() {
        let text = "生成回复中…";
        assert_eq!(strip_dispatch_preview_text(text), text);
    }
}

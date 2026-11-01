//! 模型可见历史的唯一投影。
//!
//! 提供方缓存按渲染后的前缀匹配，所以“实时循环里发给模型的历史”和“回合边界从
//! canonical 状态重建的历史”必须是同一个函数的结果。这里拥有三件事：
//! 1. 单条工具结果的模型可见形态（事实标注 → 单条上限裁剪），实时与重建共用；
//! 2. 文件读取事实的有效性校验（重建时按工作区当前内容判定 current/stale）；
//! 3. 工具结果总预算：只作用于“原始结果序列”，输出是它的纯函数，不在上一轮的输出上叠加。
//!
//! 实时循环的 `messages` 保存原始（仅经单条裁剪）结果，每次构造请求时再投影出视图；
//! 禁止把投影结果写回原始序列，否则降级边界会按已降级的体积重新计算，与重建结果分叉。

use crate::tool_result_utils::{
    bound_model_visible_tool_history, model_visible_tool_history_budget_bytes,
    model_visible_tool_result,
};
use magi_bridge_client::ChatMessage;
use magi_session_store::ThreadChatMessage;
use std::path::Path;

/// 带工具结果正文的消息。实时 `ChatMessage` 与持久化的 `ThreadChatMessage` 共用同一投影。
pub(crate) trait ToolResultSlot {
    fn tool_result(&self) -> Option<&str>;
    fn set_tool_result(&mut self, content: String);
}

impl ToolResultSlot for ChatMessage {
    fn tool_result(&self) -> Option<&str> {
        (self.role == "tool")
            .then_some(self.content.as_deref())
            .flatten()
    }
    fn set_tool_result(&mut self, content: String) {
        self.content = Some(content);
    }
}

impl ToolResultSlot for ThreadChatMessage {
    fn tool_result(&self) -> Option<&str> {
        (self.role == "tool")
            .then_some(self.content.as_deref())
            .flatten()
    }
    fn set_tool_result(&mut self, content: String) {
        self.content = Some(content);
    }
}

pub(crate) use magi_session_store::HistoryProjection as ProjectionReport;

/// 刚读取的文件结果在创建时就是“当前”事实，直接带上与历史校验相同的 `fact_state` 标注。
///
/// 回合边界重建历史时 [`refresh_file_facts`] 会给未变化的读取结果补上同一标注；
/// 如果实时消息创建时不带它，下一回合同一条结果的字节就变了，其后的整段历史缓存全部失效。
pub(crate) fn with_current_file_fact(content: String) -> String {
    let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&content) else {
        return content;
    };
    if payload.get("tool").and_then(serde_json::Value::as_str) != Some("file_read")
        || payload
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || payload
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return content;
    }
    payload["fact_state"] = serde_json::Value::String("current".to_string());
    payload.to_string()
}

/// 实时工具结果交给模型的形态：先补上与历史校验相同的事实标注，再做单条上限裁剪。
///
/// 次序必须与下一回合重建历史时一致（先 [`refresh_file_facts`]，后裁剪），
/// 否则大文件读取的裁剪外壳里 `original_bytes` 等字段会不同，该条及其后的整段历史缓存失效。
pub(crate) fn project_tool_result(raw: &str, status: magi_core::ExecutionResultStatus) -> String {
    model_visible_tool_result(&with_current_file_fact(raw.to_string()), status)
}

/// 按工作区当前内容校验历史中的文件读取事实，返回失效条数。
pub(crate) fn refresh_file_facts(history: &mut [ThreadChatMessage]) -> usize {
    let mut stale = 0usize;
    for message in history.iter_mut().filter(|message| message.role == "tool") {
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        if payload.get("tool").and_then(serde_json::Value::as_str) != Some("file_read") {
            continue;
        }
        let path = payload
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let expected_hash = payload
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let is_current =
            path.as_deref()
                .zip(expected_hash.as_deref())
                .is_some_and(|(path, expected_hash)| {
                    magi_snapshot::path_content_hash(Path::new(path))
                        .is_ok_and(|actual_hash| actual_hash == expected_hash)
                });
        if is_current {
            payload["fact_state"] = serde_json::Value::String("current".to_string());
            message.content = Some(payload.to_string());
            continue;
        }
        stale += 1;
        message.content = Some(
            serde_json::json!({
                "tool": "file_read",
                "status": "stale",
                "fact_state": "stale",
                "path": path,
                "previous_content_hash": expected_hash,
                "reason": "workspace_content_changed",
                "message": "该文件读取事实已因工作区内容变化失效；如当前任务仍依赖它，必须重新读取。"
            })
            .to_string(),
        );
    }
    stale
}

/// 对工具结果序列施加总预算。输入必须是原始（仅经单条裁剪）结果，输出是它的纯函数。
pub(crate) fn bound_tool_results<M: ToolResultSlot>(
    messages: &mut [M],
    context_window_tokens: u64,
) -> ProjectionReport {
    let indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| message.tool_result().map(|_| index))
        .collect::<Vec<_>>();
    let results = indices
        .iter()
        .filter_map(|index| messages[*index].tool_result().map(str::to_string))
        .collect::<Vec<_>>();
    let bounded = bound_model_visible_tool_history(
        &results,
        model_visible_tool_history_budget_bytes(context_window_tokens),
    );
    let mut report = ProjectionReport::default();
    for ((index, original), result) in indices.into_iter().zip(&results).zip(bounded) {
        if &result != original && result.contains("\"history_compacted\"") {
            report.demoted_tool_results += 1;
        }
        messages[index].set_tool_result(result);
    }
    report
}

/// 历史中已失效的文件读取事实条数。标注由 [`refresh_file_facts`] 写入，只在本模块解释。
fn count_stale_file_facts<M: ToolResultSlot>(messages: &[M]) -> usize {
    messages
        .iter()
        .filter_map(ToolResultSlot::tool_result)
        .filter(|content| content.contains("\"fact_state\":\"stale\""))
        .count()
}

/// 实时请求视图：在原始序列的副本上投影，原始序列保持不变。
pub(crate) fn bounded_request_history(
    messages: &[ChatMessage],
    context_window_tokens: u64,
) -> (Vec<ChatMessage>, ProjectionReport) {
    let mut view = messages.to_vec();
    let mut report = bound_tool_results(&mut view, context_window_tokens);
    report.stale_file_facts = count_stale_file_facts(messages);
    (view, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::ExecutionResultStatus;

    /// 工作区里真实存在的文件，使重建时的事实校验判为 current，与实时标注一致。
    struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("magi-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        /// 返回持久化的原始结果文本。
        fn raw_read(&self, index: usize, content_len: usize) -> String {
            let file = self.dir.join(format!("file-{index}.txt"));
            let body = "x".repeat(content_len);
            std::fs::write(&file, &body).unwrap();
            serde_json::json!({
                "tool": "file_read",
                "status": "succeeded",
                "path": file.to_string_lossy(),
                "content_hash": magi_snapshot::path_content_hash(&file).unwrap(),
                "content": body,
            })
            .to_string()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn chat_tool_message(index: usize, content: String) -> ChatMessage {
        ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".into(),
            content: Some(content),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(format!("call-{index}")),
            provider_context: Vec::new(),
        }
    }

    fn thread_tool_message(index: usize, raw: String) -> ThreadChatMessage {
        ThreadChatMessage {
            input_request_id: None,
            role: "tool".into(),
            content: Some(raw),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(format!("call-{index}")),
            provider_context: Vec::new(),
        }
    }

    fn contents<M: ToolResultSlot>(messages: &[M]) -> Vec<Option<String>> {
        messages
            .iter()
            .map(|message| message.tool_result().map(str::to_string))
            .collect()
    }

    /// 实时视图（原始序列的投影）与回合边界重建（持久化原文经校验后投影）在每一步都逐字节相同，
    /// 覆盖未超预算、刚超预算、多次块边界前移，以及单条结果超过上限被裁剪的情形。
    #[test]
    fn live_view_equals_rebuilt_view_at_every_append() {
        let fixture = Fixture::new("projection-parity");
        let window = 32_000;
        let mut raw_history = Vec::new();
        let mut live = Vec::new();
        let mut previous: Vec<Option<String>> = Vec::new();
        let (mut rewrites, mut demoted_steps) = (0usize, 0usize);
        for index in 0..150 {
            // 每第 7 条是超过单条上限的大结果。
            let len = if index % 7 == 6 {
                90_000
            } else {
                5_000 + (index % 5) * 700
            };
            let raw = fixture.raw_read(index, len);
            live.push(chat_tool_message(
                index,
                project_tool_result(&raw, ExecutionResultStatus::Succeeded),
            ));
            raw_history.push(thread_tool_message(index, raw));

            let (view, _) = bounded_request_history(&live, window);
            let mut rebuilt = raw_history.clone();
            refresh_file_facts(&mut rebuilt);
            let report = bound_tool_results(&mut rebuilt, window);
            demoted_steps += usize::from(report.demoted_tool_results > 0);

            let view = contents(&view);
            assert_eq!(
                view,
                contents(&rebuilt),
                "第 {index} 次追加后实时视图与重建视图不一致"
            );
            if !previous.is_empty() && view[..previous.len()] != previous[..] {
                rewrites += 1;
            }
            previous = view;
        }
        assert!(
            demoted_steps > 100,
            "fixture must exercise the over-budget path"
        );
        // 前移只发生在块边界，不是每次追加都改写。
        assert!(
            rewrites * 3 < demoted_steps,
            "rewrites={rewrites} demoted_steps={demoted_steps}"
        );
    }

    #[test]
    fn projection_never_mutates_the_raw_sequence() {
        let fixture = Fixture::new("projection-pure");
        let live = (0..40)
            .map(|i| {
                chat_tool_message(
                    i,
                    project_tool_result(
                        &fixture.raw_read(i, 6_000),
                        ExecutionResultStatus::Succeeded,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let before = contents(&live);
        let (view, _) = bounded_request_history(&live, 32_000);
        assert_eq!(contents(&live), before);
        assert!(
            contents(&view)
                .iter()
                .flatten()
                .any(|content| content.contains("history_compacted"))
        );
    }

    #[test]
    fn report_counts_demoted_results_and_stale_facts() {
        let fixture = Fixture::new("projection-report");
        let mut history = (0..40)
            .map(|i| thread_tool_message(i, fixture.raw_read(i, 6_000)))
            .collect::<Vec<_>>();
        assert_eq!(refresh_file_facts(&mut history), 0);
        assert!(bound_tool_results(&mut history, 32_000).demoted_tool_results > 0);

        std::fs::write(fixture.dir.join("file-0.txt"), "changed").unwrap();
        let mut history = vec![thread_tool_message(0, fixture.raw_read(0, 100))];
        std::fs::write(fixture.dir.join("file-0.txt"), "changed").unwrap();
        assert_eq!(refresh_file_facts(&mut history), 1);
    }
}

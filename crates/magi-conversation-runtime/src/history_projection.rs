//! 模型可见历史的唯一投影。
//!
//! 提供方缓存按渲染后的前缀匹配，所以“实时循环里发给模型的历史”和“回合边界从
//! canonical 状态重建的历史”必须是同一个函数的结果。这里拥有三件事：
//! 1. 单条工具结果的模型可见形态（事实标注 → 单条上限裁剪），实时与重建共用；
//! 2. 文件读取事实的有效性校验（重建时按工作区当前内容判定 current/stale）；
//! 3. 工具结果总预算：只作用于“原始结果序列”，输出是它的纯函数，不在上一轮的输出上叠加；
//! 4. 受信扩展对已装配历史的受控改写（`extension_rewrite`）：同样是原始序列的纯函数，
//!    并产出记录供缓存归因，见 [`apply_extension_rewrites`]。
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

/// 扩展提出改写所用的钩子；其余钩子不允许改写历史。
pub use magi_extension_host::EXTENSION_REWRITE_HOOKS;

/// 扩展对已装配历史提出的一次改写提案（已通过 ExtensionBus 逐项调用，尚未裁决）。
#[derive(Clone, Debug)]
pub struct ExtensionRewriteProposal {
    pub plugin_id: String,
    pub contribution_id: String,
    pub hook: String,
    /// 被替换的区间 [from, to)，下标相对改写前的装配历史。
    pub from: usize,
    pub to: usize,
    pub replacement: Vec<ChatMessage>,
}

/// 提案被拒绝的原因。任何一项违反都拒绝整批提案，不做部分应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionRewriteRejection {
    /// 钩子不在允许改写历史的集合内。
    HookNotAllowed(String),
    /// 插件没有被用户授权 `contextRewrite`。未授权的提案不降级为未声明的改写。
    NotAuthorized(String),
    /// 区间为空、越界，或落在不可改写的前缀（系统段、压缩检查点边界之前）。
    InvalidRange { from: usize, to: usize },
    /// 区间互相重叠。
    Overlap,
    /// 被替换的消息不是可改写的纯对话文本（系统/项目/Skill/引用来源、工具调用与结果、
    /// 带图片或提供方私有上下文的消息都不可改写）。
    ProtectedMessage(usize),
    /// 替换内容不是非空的纯 user/assistant 文本。
    InvalidReplacement,
}

fn digest_of(messages: &[ChatMessage]) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(messages).expect("ChatMessage 必须可序列化");
    magi_core::hex_lower(Sha256::digest(bytes))
}

fn plain_conversation_text(message: &ChatMessage) -> bool {
    matches!(message.role.as_str(), "user" | "assistant")
        && message.context_origin == magi_bridge_client::ChatMessageOrigin::Conversation
        && message.tool_calls.is_empty()
        && message.tool_call_id.is_none()
        && message.images.is_empty()
        && message.provider_context.is_empty()
}

/// 扩展改写的唯一投影。输入是原始装配历史，输出是新的模型可见副本和生效的改写记录；
/// canonical 日志与输入序列不变，输出是 (输入, 授权, 提案) 的纯函数，因此实时视图与
/// 回合边界/重启后从持久化事实重建的视图逐字节一致。
///
/// `rewritable` 是可改写区域（其之前是系统段与压缩检查点边界，其之后是当前输入）。被改写的
/// 区间只能是纯 user/assistant 文本，因此工具调用与结果的配对天然不会被切断或丢失。
pub fn apply_extension_rewrites(
    messages: &[ChatMessage],
    rewritable: std::ops::Range<usize>,
    is_authorized: &dyn Fn(&str) -> bool,
    proposals: &[ExtensionRewriteProposal],
) -> Result<
    (
        Vec<ChatMessage>,
        Vec<magi_session_store::ExtensionRewriteRecord>,
    ),
    ExtensionRewriteRejection,
> {
    let mut ordered: Vec<&ExtensionRewriteProposal> = proposals.iter().collect();
    ordered.sort_by_key(|proposal| (proposal.from, proposal.to));
    let mut previous_end: Option<usize> = None;
    for proposal in &ordered {
        if !EXTENSION_REWRITE_HOOKS.contains(&proposal.hook.as_str()) {
            return Err(ExtensionRewriteRejection::HookNotAllowed(
                proposal.hook.clone(),
            ));
        }
        if !is_authorized(&proposal.plugin_id) {
            return Err(ExtensionRewriteRejection::NotAuthorized(
                proposal.plugin_id.clone(),
            ));
        }
        if proposal.from >= proposal.to
            || proposal.to > messages.len()
            || proposal.from < rewritable.start
            || proposal.to > rewritable.end
        {
            return Err(ExtensionRewriteRejection::InvalidRange {
                from: proposal.from,
                to: proposal.to,
            });
        }
        if previous_end.is_some_and(|end| proposal.from < end) {
            return Err(ExtensionRewriteRejection::Overlap);
        }
        if let Some(index) =
            (proposal.from..proposal.to).find(|i| !plain_conversation_text(&messages[*i]))
        {
            return Err(ExtensionRewriteRejection::ProtectedMessage(index));
        }
        if proposal.replacement.is_empty()
            || !proposal.replacement.iter().all(|m| {
                plain_conversation_text(m)
                    && m.content.as_deref().is_some_and(|c| !c.trim().is_empty())
            })
        {
            return Err(ExtensionRewriteRejection::InvalidReplacement);
        }
        previous_end = Some(proposal.to);
    }
    let mut view = Vec::with_capacity(messages.len());
    let mut records = Vec::with_capacity(ordered.len());
    let mut cursor = 0;
    for proposal in ordered {
        view.extend_from_slice(&messages[cursor..proposal.from]);
        view.extend(proposal.replacement.iter().cloned());
        records.push(magi_session_store::ExtensionRewriteRecord {
            plugin_id: proposal.plugin_id.clone(),
            contribution_id: proposal.contribution_id.clone(),
            hook: proposal.hook.clone(),
            from: proposal.from,
            to: proposal.to,
            original_digest: digest_of(&messages[proposal.from..proposal.to]),
            replacement_digest: digest_of(&proposal.replacement),
        });
        cursor = proposal.to;
    }
    view.extend_from_slice(&messages[cursor..]);
    Ok((view, records))
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

    fn chat(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: role.into(),
            content: Some(text.into()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }
    }

    fn proposal(from: usize, to: usize, text: &str) -> ExtensionRewriteProposal {
        ExtensionRewriteProposal {
            plugin_id: "acme.summary".into(),
            contribution_id: "summarize".into(),
            hook: "context.assemble".into(),
            from,
            to,
            replacement: vec![chat("assistant", text)],
        }
    }

    fn history() -> Vec<ChatMessage> {
        vec![
            chat("user", "u0"),
            chat("assistant", "a0"),
            chat("user", "u1"),
            chat("assistant", "a1"),
            chat("user", "u2"),
        ]
    }

    #[test]
    fn authorized_rewrites_replace_only_the_projection_and_record_digests_of_the_originals() {
        let original = history();
        let allow = |plugin: &str| plugin == "acme.summary";
        let (view, records) = apply_extension_rewrites(
            &original,
            0..original.len(),
            &allow,
            &[proposal(1, 4, "摘要")],
        )
        .unwrap();
        assert_eq!(
            view.iter()
                .map(|m| m.content.clone().unwrap())
                .collect::<Vec<_>>(),
            ["u0", "摘要", "u2"]
        );
        assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&history()).unwrap(),
            "输入序列（canonical 来源）不变"
        );
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].from, records[0].to), (1, 4));
        assert_eq!(
            records[0].original_digest,
            digest_of(&original[1..4]),
            "原文可由摘要回溯"
        );
        assert_eq!(
            records[0].replacement_digest,
            digest_of(&[chat("assistant", "摘要")])
        );
    }

    #[test]
    fn unauthorized_protected_and_malformed_proposals_are_rejected_without_partial_application() {
        let allow = |plugin: &str| plugin == "acme.summary";
        let original = history();
        let reject = |proposals: &[ExtensionRewriteProposal], rewritable_from| {
            apply_extension_rewrites(
                &original,
                rewritable_from..original.len(),
                &allow,
                proposals,
            )
            .unwrap_err()
        };
        // 未授权 contextRewrite：拒绝，不降级。
        let mut other = proposal(1, 2, "x");
        other.plugin_id = "acme.other".into();
        assert_eq!(
            reject(&[other], 0),
            ExtensionRewriteRejection::NotAuthorized("acme.other".into())
        );
        // 钩子不在允许集合内。
        let mut wrong_hook = proposal(1, 2, "x");
        wrong_hook.hook = "tool.result".into();
        assert!(matches!(
            reject(&[wrong_hook], 0),
            ExtensionRewriteRejection::HookNotAllowed(_)
        ));
        // 越界、空区间、落入不可改写前缀。
        assert!(matches!(
            reject(&[proposal(3, 9, "x")], 0),
            ExtensionRewriteRejection::InvalidRange { .. }
        ));
        assert!(matches!(
            reject(&[proposal(2, 2, "x")], 0),
            ExtensionRewriteRejection::InvalidRange { .. }
        ));
        assert!(matches!(
            reject(&[proposal(1, 3, "x")], 2),
            ExtensionRewriteRejection::InvalidRange { .. }
        ));
        // 重叠。
        assert_eq!(
            reject(&[proposal(0, 3, "x"), proposal(2, 4, "y")], 0),
            ExtensionRewriteRejection::Overlap
        );
        // 空替换与非纯文本替换。
        let mut empty = proposal(1, 2, "x");
        empty.replacement.clear();
        assert_eq!(
            reject(&[empty], 0),
            ExtensionRewriteRejection::InvalidReplacement
        );
        let mut tool_reply = proposal(1, 2, "x");
        tool_reply.replacement = vec![chat("tool", "结果")];
        assert_eq!(
            reject(&[tool_reply], 0),
            ExtensionRewriteRejection::InvalidReplacement
        );
        // 一个提案非法则整批拒绝：合法的那个也不应用。
        assert!(
            apply_extension_rewrites(
                &original,
                0..original.len(),
                &allow,
                &[proposal(0, 1, "ok"), proposal(9, 10, "bad")]
            )
            .is_err()
        );
    }

    #[test]
    fn system_segments_tool_pairs_and_provider_context_cannot_be_rewritten() {
        let allow = |_: &str| true;
        let mut protected = history();
        protected[1].context_origin = magi_bridge_client::ChatMessageOrigin::SystemInstruction;
        assert_eq!(
            apply_extension_rewrites(
                &protected,
                0..protected.len(),
                &allow,
                &[proposal(0, 3, "x")]
            )
            .unwrap_err(),
            ExtensionRewriteRejection::ProtectedMessage(1)
        );
        // 工具调用与其结果：任意一侧落入区间都拒绝，因此配对不会被切断或丢失。
        let mut paired = history();
        paired[1].tool_calls = vec![magi_bridge_client::ChatToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: magi_bridge_client::ChatToolFunction {
                name: "shell_exec".into(),
                arguments: "{}".into(),
            },
        }];
        paired[2] = {
            let mut result = chat("tool", "{}");
            result.tool_call_id = Some("c1".into());
            result
        };
        for range in [(1, 2), (2, 3), (0, 4)] {
            assert!(matches!(
                apply_extension_rewrites(
                    &paired,
                    0..paired.len(),
                    &allow,
                    &[proposal(range.0, range.1, "x")]
                ),
                Err(ExtensionRewriteRejection::ProtectedMessage(_))
            ));
        }
        // 区间避开配对时仍可改写。
        assert!(
            apply_extension_rewrites(&paired, 0..paired.len(), &allow, &[proposal(3, 5, "x")])
                .is_ok()
        );
        let mut signed = history();
        signed[1].provider_context = vec![magi_bridge_client::ModelProviderContext {
            provider: "openai".into(),
            kind: "response_output_item".into(),
            data: serde_json::json!({"type": "reasoning"}),
        }];
        assert!(matches!(
            apply_extension_rewrites(&signed, 0..signed.len(), &allow, &[proposal(1, 2, "x")]),
            Err(ExtensionRewriteRejection::ProtectedMessage(1))
        ));
    }

    #[test]
    fn the_rewritten_view_is_a_pure_function_so_live_and_rebuilt_views_are_identical() {
        let allow = |_: &str| true;
        let proposals = [proposal(3, 5, "尾部摘要"), proposal(0, 2, "头部摘要")];
        // 实时：提案乱序提交；重建：从持久化提案按另一顺序重放。结果必须逐字节一致。
        let live = apply_extension_rewrites(&history(), 0..5, &allow, &proposals).unwrap();
        let reversed: Vec<_> = proposals.iter().rev().cloned().collect();
        let rebuilt = apply_extension_rewrites(&history(), 0..5, &allow, &reversed).unwrap();
        assert_eq!(
            serde_json::to_string(&live.0).unwrap(),
            serde_json::to_string(&rebuilt.0).unwrap()
        );
        assert_eq!(live.1, rebuilt.1);
        assert_eq!(
            live.1.iter().map(|r| r.from).collect::<Vec<_>>(),
            [0, 3],
            "记录按区间排序"
        );
    }
}

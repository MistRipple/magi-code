//! 请求控制信息只活到一次恢复完成；不写入 transcript，也不参与历史哈希链。
use magi_bridge_client::{ChatMessage, ChatMessageOrigin};

#[derive(Default)]
pub(crate) struct RequestControls {
    recovery: Option<ChatMessage>,
}

impl RequestControls {
    /// 新恢复原因替换旧原因，避免机械重试累积相同的合成 user 消息。
    pub fn recover(&mut self, prompt: impl Into<String>) {
        self.recovery = Some(ChatMessage {
            input_request_id: None,
            context_origin: ChatMessageOrigin::RoundContext,
            role: "user".into(),
            content: Some(prompt.into()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        });
    }

    pub fn clear(&mut self) {
        self.recovery = None;
    }

    /// 每次发送重建视图。已确认的回复/工具事实可继续追加到 history，控制信息始终在其后。
    pub fn view(
        &self,
        history: &[ChatMessage],
        tail: impl IntoIterator<Item = ChatMessage>,
    ) -> Vec<ChatMessage> {
        let mut view = history.to_vec();
        view.extend(
            tail.into_iter()
                .chain(self.recovery.clone())
                .map(|mut message| {
                    message.context_origin = ChatMessageOrigin::RoundContext;
                    message
                }),
        );
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_lineage::PromptFrame;
    use magi_bridge_client::ModelInvocationRequest;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            input_request_id: None,
            context_origin: ChatMessageOrigin::Conversation,
            role: role.into(),
            content: Some(content.into()),
            images: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            provider_context: vec![],
        }
    }

    fn frame(messages: Vec<ChatMessage>) -> PromptFrame {
        PromptFrame::from_request(&ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: String::new(),
            messages: Some(messages),
            tools: None,
            tool_choice: None,
        })
    }

    #[test]
    fn retries_do_not_enter_history_and_confirmed_output_can_extend_the_prefix() {
        let mut history = vec![message("user", "完成任务")];
        let baseline = frame(history.clone());
        let mut controls = RequestControls::default();
        for reason in ["空响应，请继续", "空响应，请继续", "中断，请续写"] {
            controls.recover(reason);
            let view = controls.view(&history, []);
            assert_eq!(view.len(), history.len() + 1);
            assert_eq!(
                view.last().unwrap().context_origin,
                ChatMessageOrigin::RoundContext
            );
            let retry = frame(view);
            assert_eq!(
                baseline.append_only_history_hash,
                retry.append_only_history_hash
            );
            assert_eq!(baseline.history_message_count, retry.history_message_count);
            assert_eq!(baseline.history_chain_hash, retry.history_chain_hash);
        }
        // 已提交的片段追加在 canonical history；控制消息仍位于该事实之后。
        history.push(message("assistant", "已完成第一步"));
        let recovered = frame(controls.view(&history, []));
        let rebuilt = frame(history.clone());
        assert_eq!(
            recovered.append_only_history_hash,
            rebuilt.append_only_history_hash
        );
        assert_eq!(recovered.history_chain_hash, rebuilt.history_chain_hash);
        controls.clear();
        assert_eq!(controls.view(&history, []).len(), history.len());
        assert_eq!(
            frame(controls.view(&history, [])).volatile_tail_hash,
            rebuilt.volatile_tail_hash
        );
    }

    #[test]
    fn compaction_rebuild_and_live_tail_preserve_only_the_active_control() {
        let mut controls = RequestControls::default();
        controls.recover("仍须核验工具回执");
        let rebuilt = vec![message("user", "压缩后的历史")];
        let view = controls.view(&rebuilt, [message("developer", "当前计划")]);
        assert_eq!(view[0].context_origin, ChatMessageOrigin::Conversation);
        assert!(
            view[1..]
                .iter()
                .all(|m| m.context_origin == ChatMessageOrigin::RoundContext)
        );
        assert_eq!(
            view.last().unwrap().content.as_deref(),
            Some("仍须核验工具回执")
        );
        assert_eq!(
            frame(view).history_message_count,
            frame(rebuilt).history_message_count
        );
    }
}

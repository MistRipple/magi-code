//! 新会话标题精修：利用辅助模型对会话首条消息进行理解后生成标题。
//!
//! 设计目标
//! - 复用现有 `auxiliary` 配置：客户端构造统一走
//!   [`magi_conversation_runtime::task_execution_dispatcher::resolve_target_for_role`]
//!   （`RoleTarget::Auxiliary`），未配置辅助模型时静默跳过，绝不退化到业务模型，
//!   避免在标题这种低价值任务上消耗主模型配额。
//! - 单一写入点：标题改名通过 `session_store.rename_session`，沿用既有 timeline / lifecycle 通路。
//! - 安全栅栏：异步精修期间若用户已手动改名 / charter 写入了标题，会留下与 placeholder 不一致的
//!   当前值，此时直接放弃覆盖，避免与人工动作冲突。
//! - 失败静默：辅助模型缺失、调用失败、标题非法都仅记 debug 日志，不影响主流程。

use std::sync::Arc;

use magi_bridge_client::{ModelBridgeClient, ModelInvocationRequest};
use magi_conversation_runtime::session_turn_execution::BUSINESS_MODEL_PROVIDER;
use magi_core::{EventId, SessionId, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_session_store::SessionStore;
use serde_json::json;

use crate::state::ApiState;

pub(crate) const NEW_SESSION_PLACEHOLDER_TITLE: &str = "新会话";
/// 辅助模型返回内容若超过该字符数则视为越权输出（多半是直接把整段消息回吐），直接丢弃。
const TITLE_MAX_CHARS: usize = 40;

/// 新建会话时，把首条用户消息丢给辅助模型异步生成一个更易读的会话标题。
///
/// 辅助模型未配置或调用失败时静默跳过，placeholder 标题保留不变。
pub(crate) fn spawn_new_session_title_refinement(
    state: &ApiState,
    session_id: &SessionId,
    first_message: &str,
    placeholder_title: &str,
) {
    let state = state.clone();
    let session_id = session_id.clone();
    let first_message = first_message.to_string();
    let placeholder_title = placeholder_title.to_string();
    let thread_session_id = session_id.clone();
    let _ = std::thread::Builder::new()
        .name(format!("magi-session-title-{}", session_id))
        .spawn(move || {
            refine_new_session_title_and_publish(
                &state,
                &thread_session_id,
                &first_message,
                &placeholder_title,
            );
        })
        .map_err(|error| {
            tracing::warn!(
                session_id = %session_id,
                ?error,
                "辅助模型会话标题线程启动失败"
            );
        });
}

pub(crate) fn refine_new_session_title_and_publish(
    state: &ApiState,
    session_id: &SessionId,
    first_message: &str,
    placeholder_title: &str,
) -> bool {
    let Some(client) =
        magi_conversation_runtime::task_execution_dispatcher::resolve_target_for_role(
            Some(&state.settings_store),
            None,
            magi_conversation_runtime::task_execution_dispatcher::RoleTarget::Auxiliary,
            None,
        )
        .ok()
        .flatten()
    else {
        tracing::debug!(
            session_id = %session_id,
            "辅助模型未配置，跳过会话标题精修"
        );
        return false;
    };
    let refined_title = refine_new_session_title(
        client,
        state.session_store.clone(),
        session_id.clone(),
        first_message.to_string(),
        placeholder_title.to_string(),
    );
    let Some(title) = refined_title else {
        return false;
    };
    if let Err(error) = state.persist_session_durable_state() {
        tracing::warn!(
            session_id = %session_id,
            ?error,
            "辅助模型会话标题持久化失败"
        );
    }
    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| state.session_workspace_id(&session));
    let event_id = EventId::new(format!(
        "event-session-title-updated-{}-{}",
        session_id,
        UtcMillis::now().0
    ));
    let event = EventEnvelope::domain(
        event_id,
        "session.title.updated",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "title": title,
        }),
    )
    .with_context(EventContext {
        session_id: Some(session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    if let Err(error) = state.event_bus.publish(event) {
        tracing::warn!(
            session_id = %session_id,
            ?error,
            "辅助模型会话标题更新事件发布失败"
        );
        return false;
    }
    true
}

/// 同步执行一次会话标题精修。
///
/// 调用方负责在合适的位置 fire-and-forget 包装（参考 `submit_regular_session_turn` 中的接线）：
/// 该函数会发起一次阻塞式 LLM 调用，应放到独立线程，避免阻塞 HTTP 请求处理线程。
pub fn refine_new_session_title(
    client: Arc<dyn ModelBridgeClient>,
    session_store: Arc<SessionStore>,
    session_id: SessionId,
    first_message: String,
    placeholder_title: String,
) -> Option<String> {
    let trimmed = first_message.trim();
    let prompt = build_title_prompt(trimmed);
    let request = ModelInvocationRequest {
        provider: BUSINESS_MODEL_PROVIDER.to_string(),
        prompt,
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let response = match client.invoke(request) {
        Ok(resp) if resp.ok => resp,
        Ok(resp) => {
            tracing::debug!(
                session_id = %session_id,
                payload = %resp.payload,
                "辅助模型返回 ok=false，跳过会话标题精修"
            );
            return None;
        }
        Err(err) => {
            tracing::debug!(
                session_id = %session_id,
                error = %err,
                "辅助模型调用失败，跳过会话标题精修"
            );
            return None;
        }
    };
    let payload = response.parse_chat_payload();
    let Some(raw) = payload.content else {
        tracing::debug!(
            session_id = %session_id,
            thinking = ?payload.thinking,
            "辅助模型返回缺少 content，跳过会话标题精修"
        );
        return None;
    };
    let Some(title) = normalize_title(&raw) else {
        tracing::debug!(
            session_id = %session_id,
            raw = %raw,
            "辅助模型返回的标题不合规，跳过会话标题精修"
        );
        return None;
    };
    match session_store
        .session(&session_id)
        .map(|record| record.title)
    {
        Some(ref current) if current == &placeholder_title => {}
        Some(other) => {
            tracing::debug!(
                session_id = %session_id,
                current = %other,
                "会话标题已被改动，跳过辅助模型精修"
            );
            return None;
        }
        None => {
            tracing::debug!(
                session_id = %session_id,
                "会话已不存在，跳过辅助模型精修"
            );
            return None;
        }
    }
    match session_store.rename_session(&session_id, title.clone()) {
        Ok(_) => Some(title),
        Err(err) => {
            tracing::warn!(
                session_id = %session_id,
                ?err,
                "会话标题写回失败"
            );
            None
        }
    }
}

fn build_title_prompt(message: &str) -> String {
    format!(
        "请根据用户的首条消息为这个对话生成一个简短的会话标题。\n要求：\n\
- 中文 6-14 字，或英文 3-7 个词\n\
- 概括用户意图，不要复述原文\n\
- 不要加引号、句号、表情或前后缀\n\
- 直接输出标题文本本身\n\n\
用户首条消息：\n{message}"
    )
}

fn normalize_title(raw: &str) -> Option<String> {
    const TRIM_CHARS: &[char] = &[
        '"', '\'', '`', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '《', '》', '【', '】',
    ];
    let mut title = raw.trim().to_string();
    while let Some(c) = title.chars().next() {
        if TRIM_CHARS.contains(&c) {
            title.remove(0);
        } else {
            break;
        }
    }
    while let Some(c) = title.chars().last() {
        if TRIM_CHARS.contains(&c) {
            let len = c.len_utf8();
            let new_len = title.len() - len;
            title.truncate(new_len);
        } else {
            break;
        }
    }
    let title = title.trim().to_string();
    if title.is_empty() {
        return None;
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return None;
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use magi_bridge_client::{BridgeClientError, BridgeResponse, ModelStreamingDelta};
    use magi_session_store::SessionStore;

    struct StubAuxiliaryClient {
        payload: Mutex<Option<Result<BridgeResponse, BridgeClientError>>>,
    }

    impl StubAuxiliaryClient {
        fn ok_with_content(content: &str) -> Arc<Self> {
            Arc::new(Self {
                payload: Mutex::new(Some(Ok(BridgeResponse {
                    ok: true,
                    payload: serde_json::json!({ "content": content }).to_string(),
                }))),
            })
        }

        fn err() -> Arc<Self> {
            Arc::new(Self {
                payload: Mutex::new(Some(Err(BridgeClientError::CallFailed {
                    layer: magi_bridge_client::BridgeErrorLayer::Transport,
                    code: None,
                    message: "boom".to_string(),
                }))),
            })
        }
    }

    impl ModelBridgeClient for StubAuxiliaryClient {
        fn invoke(
            &self,
            _request: ModelInvocationRequest,
        ) -> Result<BridgeResponse, BridgeClientError> {
            self.payload
                .lock()
                .expect("stub payload lock poisoned")
                .take()
                .expect("stub payload already consumed")
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<BridgeResponse, BridgeClientError> {
            unreachable!("会话标题精修只走 invoke 同步路径")
        }
    }

    fn seed_session(store: &SessionStore, placeholder: &str) -> SessionId {
        let session_id = SessionId::new(format!("session-test-{placeholder}"));
        store
            .create_session_for_workspace(
                session_id.clone(),
                placeholder,
                Some("ws-test".to_string()),
            )
            .expect("create session");
        session_id
    }

    #[test]
    fn normalize_title_strips_quotes_and_whitespace() {
        assert_eq!(
            normalize_title("  “重构计划” ").as_deref(),
            Some("重构计划")
        );
        assert_eq!(normalize_title("\"Plan B\"").as_deref(), Some("Plan B"));
        assert_eq!(normalize_title("`title`").as_deref(), Some("title"));
    }

    #[test]
    fn normalize_title_rejects_empty_and_oversize() {
        assert!(normalize_title("   ").is_none());
        let too_long: String = std::iter::repeat('字').take(TITLE_MAX_CHARS + 1).collect();
        assert!(normalize_title(&too_long).is_none());
    }

    #[test]
    fn refine_new_session_title_rewrites_placeholder_on_success() {
        let store = Arc::new(SessionStore::new());
        let placeholder = "对话首条消息很长很长";
        let session_id = seed_session(&store, placeholder);
        let client = StubAuxiliaryClient::ok_with_content("“重构 Mission 模型 ”");

        let refined_title = refine_new_session_title(
            client,
            store.clone(),
            session_id.clone(),
            "我想把 mission 模型彻底重构一下，把生命周期阶段算出来".to_string(),
            placeholder.to_string(),
        );

        let record = store.session(&session_id).expect("session exists");
        assert_eq!(refined_title.as_deref(), Some("重构 Mission 模型"));
        assert_eq!(record.title, "重构 Mission 模型");
    }

    #[test]
    fn refine_new_session_title_skips_when_user_already_renamed() {
        let store = Arc::new(SessionStore::new());
        let placeholder = "原始消息内容";
        let session_id = seed_session(&store, placeholder);
        store
            .rename_session(&session_id, "用户手改的标题")
            .expect("rename");
        let client = StubAuxiliaryClient::ok_with_content("辅助模型生成标题");

        let refined_title = refine_new_session_title(
            client,
            store.clone(),
            session_id.clone(),
            "原始消息内容更长一些这里多写一点".to_string(),
            placeholder.to_string(),
        );

        let record = store.session(&session_id).expect("session exists");
        assert!(refined_title.is_none());
        assert_eq!(record.title, "用户手改的标题");
    }

    #[test]
    fn refine_new_session_title_skips_on_client_error() {
        let store = Arc::new(SessionStore::new());
        let placeholder = "占位标题内容长一些";
        let session_id = seed_session(&store, placeholder);
        let client = StubAuxiliaryClient::err();

        let refined_title = refine_new_session_title(
            client,
            store.clone(),
            session_id.clone(),
            "用户发送的首条消息内容比较长".to_string(),
            placeholder.to_string(),
        );

        let record = store.session(&session_id).expect("session exists");
        assert!(refined_title.is_none());
        assert_eq!(record.title, placeholder);
    }

    #[test]
    fn refine_new_session_title_uses_auxiliary_for_short_messages() {
        let store = Arc::new(SessionStore::new());
        let placeholder = "短消息";
        let session_id = seed_session(&store, placeholder);
        let client = StubAuxiliaryClient::ok_with_content("日常问候");

        let refined_title = refine_new_session_title(
            client,
            store.clone(),
            session_id.clone(),
            "你好".to_string(),
            placeholder.to_string(),
        );

        let record = store.session(&session_id).expect("session exists");
        assert_eq!(refined_title.as_deref(), Some("日常问候"));
        assert_eq!(record.title, "日常问候");
    }
}

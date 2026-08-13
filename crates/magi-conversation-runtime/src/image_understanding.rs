//! 主模型图片输入路由。
//!
//! 图片能力必须由配置显式声明。未声明为多模态的主模型不会收到原始图片，
//! 而是由独立 vision 模型生成带来源标识的文字上下文后再调用主模型。

use crate::{
    model_config::{NormalizedModelConfig, VISION_MODEL_SECTION, model_supports_images},
    usage_recording::{
        ModelUsageBinding, ModelUsageRecordInput, publish_model_usage_record,
        vision_model_usage_binding,
    },
};
use magi_bridge_client::{
    ChatMessage, ModelBridgeClient, ModelInvocationRequest, ModelResponse,
    model_invocation_error_is_cancelled,
};
use magi_core::{SessionId, WorkspaceId};
use magi_event_bus::InMemoryEventBus;
use magi_session_store::SessionStore;
use magi_settings_store::SettingsStore;
use magi_usage_authority::UsageCallStatus;
use std::sync::Arc;

pub const VISION_CONTEXT_MARKER: &str = "[Magi 图片理解上下文]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageUnderstandingProgress {
    Started { image_count: usize },
    Completed { image_count: usize },
    Failed { image_count: usize },
    Cancelled { image_count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageUnderstandingError {
    VisionModelNotConfigured,
    VisionModelFailed(String),
    Cancelled,
}

impl std::fmt::Display for ImageUnderstandingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VisionModelNotConfigured => f.write_str(
                "当前主模型不支持图片，尚未配置图片理解模型。请在设置中配置图片理解模型后重试。",
            ),
            Self::VisionModelFailed(detail) => write!(f, "图片理解模型调用失败：{detail}"),
            Self::Cancelled => f.write_str("图片处理已取消"),
        }
    }
}

/// 将消息物化为当前主模型可接受的视图。
///
/// 对显式支持图片的模型保留原始图片；对 text-only 模型，所有带图片的消息都
/// 通过 vision 模型转换为文字。已经包含本模块标记的历史消息不会重复调用 vision。
pub fn route_messages_for_model(
    messages: &mut [ChatMessage],
    settings_store: Option<&Arc<SettingsStore>>,
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    model: &str,
    is_cancelled: &dyn Fn() -> bool,
    on_progress: Option<&dyn Fn(ImageUnderstandingProgress)>,
) -> Result<(), ImageUnderstandingError> {
    let Some(settings_store) = settings_store else {
        if messages.iter().any(|message| !message.images.is_empty()) {
            return Err(ImageUnderstandingError::VisionModelNotConfigured);
        }
        return Ok(());
    };
    if model_supports_images(Some(settings_store.as_ref()), model) {
        return Ok(());
    }
    let needs_understanding = messages.iter().any(|message| {
        !message.images.is_empty()
            && !message
                .content
                .as_deref()
                .is_some_and(|content| content.contains(VISION_CONTEXT_MARKER))
    });
    if !needs_understanding {
        for message in messages {
            if !message.images.is_empty() {
                message.images.clear();
            }
        }
        return Ok(());
    }
    let image_count = messages
        .iter()
        .filter(|message| {
            !message.images.is_empty()
                && !message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(VISION_CONTEXT_MARKER))
        })
        .map(|message| message.images.len())
        .sum();
    notify_progress(
        on_progress,
        ImageUnderstandingProgress::Started { image_count },
    );
    if is_cancelled() {
        notify_progress(
            on_progress,
            ImageUnderstandingProgress::Cancelled { image_count },
        );
        return Err(ImageUnderstandingError::Cancelled);
    }
    let config = match NormalizedModelConfig::from_settings_value(
        &settings_store.get_section(VISION_MODEL_SECTION),
    ) {
        Ok(config) => config,
        Err(_) => {
            notify_progress(
                on_progress,
                ImageUnderstandingProgress::Failed { image_count },
            );
            return Err(ImageUnderstandingError::VisionModelNotConfigured);
        }
    };
    let Some(client) = config.to_http_vision_client() else {
        notify_progress(
            on_progress,
            ImageUnderstandingProgress::Failed { image_count },
        );
        return Err(ImageUnderstandingError::VisionModelNotConfigured);
    };

    route_messages_with_vision_client(
        messages,
        settings_store,
        event_bus,
        session_store,
        session_id,
        workspace_id,
        &client,
        is_cancelled,
        on_progress,
        image_count,
    )
}

#[allow(clippy::too_many_arguments)]
fn route_messages_with_vision_client(
    messages: &mut [ChatMessage],
    settings_store: &Arc<SettingsStore>,
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    client: &dyn ModelBridgeClient,
    is_cancelled: &dyn Fn() -> bool,
    on_progress: Option<&dyn Fn(ImageUnderstandingProgress)>,
    total_image_count: usize,
) -> Result<(), ImageUnderstandingError> {
    for (index, message) in messages.iter_mut().enumerate() {
        if message.images.is_empty() {
            continue;
        }
        if is_cancelled() {
            notify_progress(
                on_progress,
                ImageUnderstandingProgress::Cancelled {
                    image_count: total_image_count,
                },
            );
            return Err(ImageUnderstandingError::Cancelled);
        }
        if message
            .content
            .as_deref()
            .is_some_and(|content| content.contains(VISION_CONTEXT_MARKER))
        {
            message.images.clear();
            continue;
        }
        let image_count = message.images.len();
        let request = ModelInvocationRequest {
            provider: "vision".to_string(),
            prompt: "请按图片上传顺序分别描述下面的图片，使用“图片1：…\n图片2：…”格式。准确保留文字、数字、结构、颜色、表格和代码等可验证细节。不要猜测图片外的信息，只输出供另一个模型使用的事实描述。".to_string(),
            messages: Some(vec![ChatMessage {
                role: "user".to_string(),
                content: message.content.clone(),
                images: std::mem::take(&mut message.images),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            }]),
            tools: None,
            tool_choice: None,
        };
        let call_id = format!(
            "vision-{}-{}-{}",
            session_id,
            index,
            magi_core::UtcMillis::now().0
        );
        let binding = vision_model_usage_binding();
        let response = match client.invoke_with_cancellation(request, is_cancelled) {
            Ok(response) => response,
            Err(error) if model_invocation_error_is_cancelled(&error) || is_cancelled() => {
                publish_vision_usage(
                    event_bus,
                    session_store,
                    settings_store,
                    session_id,
                    workspace_id,
                    &binding,
                    call_id,
                    &ModelResponse::completed(""),
                    UsageCallStatus::Cancelled,
                    Some("vision_model_cancelled"),
                );
                notify_progress(
                    on_progress,
                    ImageUnderstandingProgress::Cancelled {
                        image_count: total_image_count,
                    },
                );
                return Err(ImageUnderstandingError::Cancelled);
            }
            Err(error) => {
                publish_vision_usage(
                    event_bus,
                    session_store,
                    settings_store,
                    session_id,
                    workspace_id,
                    &binding,
                    call_id,
                    &ModelResponse::completed(""),
                    UsageCallStatus::Failed,
                    Some("vision_model_invocation_failed"),
                );
                notify_progress(
                    on_progress,
                    ImageUnderstandingProgress::Failed {
                        image_count: total_image_count,
                    },
                );
                return Err(ImageUnderstandingError::VisionModelFailed(
                    error.to_string(),
                ));
            }
        };
        let Some(description) = response
            .content
            .as_deref()
            .map(str::trim)
            .filter(|content| !content.is_empty())
        else {
            publish_vision_usage(
                event_bus,
                session_store,
                settings_store,
                session_id,
                workspace_id,
                &binding,
                call_id,
                &response,
                UsageCallStatus::Failed,
                Some("vision_model_empty_response"),
            );
            notify_progress(
                on_progress,
                ImageUnderstandingProgress::Failed {
                    image_count: total_image_count,
                },
            );
            return Err(ImageUnderstandingError::VisionModelFailed(
                "图片理解模型返回空内容".to_string(),
            ));
        };
        publish_vision_usage(
            event_bus,
            session_store,
            settings_store,
            session_id,
            workspace_id,
            &binding,
            call_id,
            &response,
            UsageCallStatus::Success,
            None,
        );
        let original_text = message.content.take().unwrap_or_default();
        message.content = Some(if original_text.trim().is_empty() {
            format!(
                "{VISION_CONTEXT_MARKER}（原始消息第 {} 条，共 {image_count} 张图片，视觉模型按上传顺序描述）\n{description}",
                index + 1
            )
        } else {
            format!(
                "{original_text}\n\n{VISION_CONTEXT_MARKER}（原始消息第 {} 条，共 {image_count} 张图片，视觉模型按上传顺序描述）\n{description}",
                index + 1
            )
        });
    }
    notify_progress(
        on_progress,
        ImageUnderstandingProgress::Completed {
            image_count: total_image_count,
        },
    );
    Ok(())
}

fn notify_progress(
    on_progress: Option<&dyn Fn(ImageUnderstandingProgress)>,
    progress: ImageUnderstandingProgress,
) {
    if let Some(on_progress) = on_progress {
        on_progress(progress);
    }
}

fn publish_vision_usage(
    event_bus: &InMemoryEventBus,
    session_store: &SessionStore,
    settings_store: &Arc<SettingsStore>,
    session_id: &SessionId,
    workspace_id: &Option<WorkspaceId>,
    binding: &ModelUsageBinding,
    call_id: String,
    response: &ModelResponse,
    status: UsageCallStatus,
    error_code: Option<&str>,
) {
    publish_model_usage_record(
        event_bus,
        session_store,
        Some(settings_store),
        ModelUsageRecordInput {
            session_id,
            workspace_id,
            binding,
            call_id,
            usage: response.usage.as_ref(),
            status,
            assignment_id: None,
            error_code: error_code.map(ToOwned::to_owned),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_config::MODEL_CAPABILITIES_SECTION;
    use magi_bridge_client::{
        BridgeClientError, BridgeErrorLayer, ModelStreamingDelta, llm_types::ImageSource,
    };
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct RecordingVisionClient {
        response: Mutex<Option<Result<ModelResponse, BridgeClientError>>>,
        requests: Mutex<Vec<ModelInvocationRequest>>,
        calls: AtomicUsize,
    }

    impl RecordingVisionClient {
        fn success(content: &str) -> Self {
            Self {
                response: Mutex::new(Some(Ok(ModelResponse::completed(content)))),
                requests: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }

        fn failure(message: &str) -> Self {
            Self {
                response: Mutex::new(Some(Err(BridgeClientError::CallFailed {
                    layer: BridgeErrorLayer::RemoteBusiness,
                    code: None,
                    message: message.to_string(),
                }))),
                requests: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl ModelBridgeClient for RecordingVisionClient {
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .expect("vision requests mutex poisoned")
                .push(request);
            self.response
                .lock()
                .expect("vision response mutex poisoned")
                .take()
                .expect("test vision response must exist")
        }

        fn invoke_streaming(
            &self,
            _request: ModelInvocationRequest,
            _on_delta: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            panic!("图片理解必须使用非流式调用")
        }
    }

    fn settings() -> Arc<SettingsStore> {
        let store = Arc::new(SettingsStore::new());
        store
            .set_section(
                VISION_MODEL_SECTION,
                json!({
                    "baseUrl": "https://vision.example.test/v1",
                    "apiKey": "test-key",
                    "model": "vision-test",
                    "urlMode": "standard",
                    "apiProtocol": "openai_chat"
                }),
            )
            .expect("vision settings should save");
        store
    }

    fn image(data: &str) -> ImageSource {
        ImageSource {
            kind: "base64".to_string(),
            media_type: "image/png".to_string(),
            data: data.to_string(),
        }
    }

    fn user_message(content: &str, images: Vec<ImageSource>) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(content.to_string()),
            images,
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }
    }

    #[test]
    fn text_only_model_uses_vision_and_preserves_image_order() {
        let settings = settings();
        let client = RecordingVisionClient::success("图片1：第一张\n图片2：第二张");
        let event_bus = InMemoryEventBus::new(16);
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-vision-success");
        let mut messages = vec![user_message("比较图片", vec![image("AAA"), image("BBB")])];
        let progress = Mutex::new(Vec::new());

        route_messages_with_vision_client(
            &mut messages,
            &settings,
            &event_bus,
            &session_store,
            &session_id,
            &None,
            &client,
            &|| false,
            Some(&|event| progress.lock().unwrap().push(event)),
            2,
        )
        .expect("text-only model should receive image description");

        assert!(messages[0].images.is_empty());
        let content = messages[0].content.as_deref().unwrap();
        assert!(content.contains(VISION_CONTEXT_MARKER));
        assert!(content.contains("图片1：第一张\n图片2：第二张"));
        let requests = client.requests.lock().unwrap();
        let request_images = &requests[0].messages.as_ref().unwrap()[0].images;
        assert_eq!(request_images[0].data, "AAA");
        assert_eq!(request_images[1].data, "BBB");
        assert_eq!(
            *progress.lock().unwrap(),
            vec![ImageUnderstandingProgress::Completed { image_count: 2 }]
        );
    }

    #[test]
    fn multimodal_model_keeps_original_images_without_vision_call() {
        let settings = settings();
        settings
            .set_section(
                MODEL_CAPABILITIES_SECTION,
                json!({"main-model": "multimodal"}),
            )
            .unwrap();
        let mut messages = vec![user_message("看图", vec![image("AAA")])];

        route_messages_for_model(
            &mut messages,
            Some(&settings),
            &InMemoryEventBus::new(8),
            &SessionStore::new(),
            &SessionId::new("session-multimodal"),
            &None,
            "main-model",
            &|| false,
            None,
        )
        .expect("multimodal model should receive original image");

        assert_eq!(messages[0].images.len(), 1);
        assert!(
            !messages[0]
                .content
                .as_deref()
                .unwrap()
                .contains(VISION_CONTEXT_MARKER)
        );
    }

    #[test]
    fn existing_vision_context_is_not_processed_again() {
        let settings = settings();
        let client = RecordingVisionClient::success("不应调用");
        let event_bus = InMemoryEventBus::new(8);
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-vision-reuse");
        let mut messages = vec![user_message(
            &format!("{VISION_CONTEXT_MARKER}\n已识别内容"),
            vec![image("AAA")],
        )];

        route_messages_with_vision_client(
            &mut messages,
            &settings,
            &event_bus,
            &session_store,
            &session_id,
            &None,
            &client,
            &|| false,
            None,
            0,
        )
        .expect("existing vision context should be reused");

        assert!(messages[0].images.is_empty());
        assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn empty_vision_response_is_failed_usage_not_success() {
        let settings = settings();
        let client = RecordingVisionClient::success("   ");
        let event_bus = InMemoryEventBus::new(16);
        let mut messages = vec![user_message("看图", vec![image("AAA")])];
        let workspace_id = Some(WorkspaceId::new("workspace-vision-empty"));

        let result = route_messages_with_vision_client(
            &mut messages,
            &settings,
            &event_bus,
            &SessionStore::new(),
            &SessionId::new("session-vision-empty"),
            &workspace_id,
            &client,
            &|| false,
            None,
            1,
        );

        assert_eq!(
            result,
            Err(ImageUnderstandingError::VisionModelFailed(
                "图片理解模型返回空内容".to_string()
            ))
        );
        let usage_events = event_bus
            .snapshot()
            .recent_events
            .into_iter()
            .filter(|event| event.event_type == "model.usage.recorded")
            .collect::<Vec<_>>();
        assert_eq!(usage_events.len(), 1);
        assert_eq!(usage_events[0].payload["status"], json!("failed"));
        assert_eq!(
            usage_events[0].payload["errorCode"],
            json!("vision_model_empty_response")
        );
    }

    #[test]
    fn vision_failure_and_cancellation_are_explicit() {
        let settings = settings();
        let event_bus = InMemoryEventBus::new(16);
        let session_store = SessionStore::new();
        let session_id = SessionId::new("session-vision-terminal");
        let mut failed_messages = vec![user_message("看图", vec![image("AAA")])];
        let failed = route_messages_with_vision_client(
            &mut failed_messages,
            &settings,
            &event_bus,
            &session_store,
            &session_id,
            &None,
            &RecordingVisionClient::failure("upstream unavailable"),
            &|| false,
            None,
            1,
        );
        assert!(matches!(
            failed,
            Err(ImageUnderstandingError::VisionModelFailed(_))
        ));

        let mut cancelled_messages = vec![user_message("看图", vec![image("BBB")])];
        let cancelled = route_messages_with_vision_client(
            &mut cancelled_messages,
            &settings,
            &event_bus,
            &session_store,
            &session_id,
            &None,
            &RecordingVisionClient::success("不应调用"),
            &|| true,
            None,
            1,
        );
        assert_eq!(cancelled, Err(ImageUnderstandingError::Cancelled));
    }
}

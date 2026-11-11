//! 页面内模型业务复用原生连接及传输账本；不创建 Session、Turn 或第二个执行器。
use super::*;
use crate::auxiliary_usage::{AuxiliaryUsageFact, PluginModelOwner, valid_plugin_owner};
use base64::Engine as _;
use magi_bridge_client::{ChatMessage, ChatToolCall, ChatToolDefinition, ModelInvocationOptions};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

const OPERATION_EVENT: &str = "plugin.model.operation";

pub(super) struct ModelCall {
    cancelled: Arc<AtomicBool>,
    started: bool,
    created: std::time::Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    operation_id: String,
    selection: Value,
    messages: Vec<Message>,
    max_output_tokens: Option<u32>,
    #[serde(default)]
    tools: Vec<ChatToolDefinition>,
    temperature: Option<f64>,
    #[serde(default)]
    stop: Vec<String>,
    reasoning_effort: Option<magi_core::ReasoningEffort>,
    metadata: Option<Metadata>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    session_id: Option<String>,
    purpose: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    images: Vec<magi_bridge_client::llm_types::ImageSource>,
    #[serde(default)]
    tool_calls: Vec<ChatToolCall>,
    tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    provider_context: Vec<magi_bridge_client::ModelProviderContext>,
}

fn fingerprint(value: &impl serde::Serialize) -> String {
    magi_core::hex_lower(Sha256::digest(
        serde_json::to_vec(value).expect("model value serializes"),
    ))
}

struct RunningCall<'a> {
    calls: &'a Mutex<HashMap<String, ModelCall>>,
    key: String,
}
impl Drop for RunningCall<'_> {
    fn drop(&mut self) {
        self.calls
            .lock()
            .expect("plugin model calls")
            .remove(&self.key);
    }
}

impl LlmTaskDispatcher {
    pub(super) fn plugin_model_service(
        &self,
        plugin: &str,
        method: &str,
        mut params: Value,
        cancelled: Arc<AtomicBool>,
        progress: Option<magi_extension_host::ServiceProgress>,
    ) -> Result<Value, String> {
        let identity = params
            .as_object_mut()
            .ok_or("InvalidParams: 模型参数必须是对象")?
            .remove("_modelOwner")
            .ok_or("插件模型归属缺失")?;
        let mut owner: PluginModelOwner = serde_json::from_value(json!({
            "pluginId":identity["pluginId"],"digest":identity["digest"],"instanceId":identity["instanceId"],
            "operationId":"selection","displayName":identity.get("displayName")
        })).map_err(|e| e.to_string())?;
        if owner.plugin_id != plugin || !valid_plugin_owner(&owner) {
            return Err("插件模型归属无效".into());
        }
        if method == "model.cancel" {
            owner.operation_id = params
                .get("operationId")
                .and_then(Value::as_str)
                .ok_or("缺少 operationId")?
                .into();
            if !valid_plugin_owner(&owner) {
                return Err("operationId 无效".into());
            }
            let mut calls = self.plugin_model_calls.lock().map_err(|e| e.to_string())?;
            calls.retain(|_, call| call.started || call.created.elapsed().as_secs() < 120);
            if let Some(call) = calls.get(&owner.key()) {
                call.cancelled.store(true, Ordering::Release);
            } else if calls.len() < 64 {
                // 取消可先于 spawn_blocking 接纳到达；保留有界取消事实，不能漏发一次模型请求。
                calls.insert(
                    owner.key(),
                    ModelCall {
                        cancelled: Arc::new(AtomicBool::new(true)),
                        started: false,
                        created: std::time::Instant::now(),
                    },
                );
            }
            return Ok(json!({"cancelled":true}));
        }
        let resolve = || {
            let store = self
                .settings_store
                .as_ref()
                .ok_or("模型设置尚未就绪")?
                .execution_snapshot();
            let config = crate::model_config::resolve_orchestrator_model_config(&store, None)?;
            let usage = config.to_usage_llm_config().ok_or("默认模型未配置")?;
            // 内容身份也绑定凭据，返回的仅为摘要；不把密钥或端点交给原包。
            let mut selection = json!({"sourceId":config.source_id(),"model":config.require_model()?,"revision":fingerprint(&usage)});
            if let Some(image) = config.source_id().and_then(|source| {
                crate::main_model_sources::declared_image_input(
                    &store,
                    source,
                    config.require_model().ok()?,
                )
            }) {
                selection["inputModalities"] = if image {
                    json!(["text", "image"])
                } else {
                    json!(["text"])
                };
            }
            let mut capability = config.reasoning_capability(config.require_model()?);
            if !capability.supported_efforts.is_empty() {
                capability.default_effort = config.effective_reasoning_effort();
                selection["reasoning"] = json!(capability);
            }
            Ok::<_, String>((config, usage, selection))
        };
        if method == "model.selection" {
            return Ok(resolve()?.2);
        }
        let input: Input =
            serde_json::from_value(params).map_err(|e| format!("InvalidParams: {e}"))?;
        owner.operation_id = input.operation_id;
        if !valid_plugin_owner(&owner) {
            return Err("InvalidParams: operationId 无效".into());
        }
        let selection = input.selection;
        let limit = input.max_output_tokens.unwrap_or(4096);
        if !(1..=32768).contains(&limit)
            || input.messages.is_empty()
            || input.messages.len() > 256
            || input.messages.iter().any(|m| {
                !matches!(
                    m.role.as_str(),
                    "system" | "developer" | "user" | "assistant" | "tool"
                ) || m.content.is_empty()
                    && m.images.is_empty()
                    && m.tool_calls.is_empty()
                    && m.provider_context.is_empty()
                    || m.role != "assistant" && !m.provider_context.is_empty()
                    || m.role != "assistant" && !m.tool_calls.is_empty()
                    || (m.role == "tool")
                        != m.tool_call_id.as_ref().is_some_and(|id| !id.is_empty())
                    || !m.images.is_empty() && !matches!(m.role.as_str(), "user" | "tool")
            })
            || serde_json::to_vec(&input.messages)
                .map_err(|e| e.to_string())?
                .len()
                > 3 * 1_048_576
            || input.messages.iter().flat_map(|m| &m.images).any(|image| {
                image.kind != "base64"
                    || !matches!(
                        image.media_type.as_str(),
                        "image/jpeg" | "image/png" | "image/webp" | "image/gif"
                    )
                    || base64::engine::general_purpose::STANDARD
                        .decode(&image.data)
                        .is_err()
            })
            || input.metadata.as_ref().is_some_and(|m| {
                [m.session_id.as_ref(), m.purpose.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|s| s.len() > 256)
            })
            || input.tools.len() > 128
            || input.tools.iter().any(|t| {
                t.kind != "function"
                    || t.function.name.is_empty()
                    || !t.function.parameters.is_object()
            })
            || input
                .tools
                .iter()
                .map(|t| &t.function.name)
                .collect::<std::collections::HashSet<_>>()
                .len()
                != input.tools.len()
        {
            return Err("InvalidParams: 消息角色、工具关联或请求大小无效".into());
        }
        let mut calls = std::collections::HashSet::new();
        let mut answered = std::collections::HashSet::new();
        for message in &input.messages {
            for call in &message.tool_calls {
                if call.id.is_empty() || call.function.name.is_empty() || !calls.insert(&call.id) {
                    return Err("InvalidParams: 工具调用身份无效或重复".into());
                }
            }
            if let Some(id) = &message.tool_call_id
                && (!calls.contains(id) || !answered.insert(id))
            {
                return Err("InvalidParams: 工具结果缺少匹配调用或重复提交".into());
            }
        }
        let request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: BUSINESS_MODEL_PROVIDER.into(),
            prompt: String::new(),
            messages: Some(
                input
                    .messages
                    .into_iter()
                    .map(|message| {
                        serde_json::from_value::<ChatMessage>(
                            json!({"role":message.role,"content":message.content,"images":message.images,"tool_calls":message.tool_calls,"tool_call_id":message.tool_call_id,"provider_context":message.provider_context}),
                        )
                        .expect("validated model message")
                    })
                    .collect(),
            ),
            tools: (!input.tools.is_empty()).then_some(input.tools),
            tool_choice: None,
        };
        let request_id = fingerprint(
            &json!({"request":request,"selection":selection,"maxOutputTokens":limit,
                "temperature":input.temperature,"stop":input.stop,"reasoningEffort":input.reasoning_effort,"metadata":input.metadata}),
        );
        let key = owner.key();
        let local_cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut calls = self.plugin_model_calls.lock().map_err(|e| e.to_string())?;
            calls.retain(|_, call| call.started || call.created.elapsed().as_secs() < 120);
            if let Some(call) = calls.get(&key) {
                if !call.started {
                    calls.remove(&key);
                    return Err("Cancelled: 模型操作已取消，未发送请求".into());
                }
                return Err("Conflict: 同一模型操作仍在执行".into());
            }
            if calls.len() >= 64
                || calls.values().filter(|call| call.started).count() >= 8
                || calls
                    .iter()
                    .filter(|(key, call)| call.started && key.starts_with(&format!("{plugin}:")))
                    .count()
                    >= 4
            {
                return Err("Backpressure: 插件模型并发上限已达到".into());
            }
            calls.insert(
                key.clone(),
                ModelCall {
                    cancelled: local_cancelled.clone(),
                    started: true,
                    created: std::time::Instant::now(),
                },
            );
        }
        let _running = RunningCall {
            calls: &self.plugin_model_calls,
            key: key.clone(),
        };
        let previous = self.event_bus.with_audit_usage_ledger(|ledger| {
            ledger
                .audit_entries
                .iter()
                .rev()
                .find(|entry| entry.event_type == OPERATION_EVENT && entry.payload["key"] == key)
                .map(|entry| entry.payload.clone())
        });
        if let Some(previous) = previous {
            if previous["requestFingerprint"] != request_id {
                return Err("Conflict: 同一操作身份的模型请求发生变化".into());
            }
            if previous["stage"] == "cancelled" {
                return Err("Cancelled: 模型操作已取消".into());
            }
            if previous["stage"] == "failed" {
                return Err(previous
                    .pointer("/result/error")
                    .and_then(Value::as_str)
                    .unwrap_or("模型操作失败")
                    .into());
            }
            if previous["stage"] != "completed" {
                return Err(
                    "模型操作已有发送记录但结果未确认；请核对后显式发起新操作，不自动重放".into(),
                );
            }
            return previous
                .get("result")
                .filter(|value| !value.is_null())
                .cloned()
                .ok_or_else(|| "模型结果未持久化".into());
        }
        let (config, mut usage, current_selection) = resolve()?;
        if selection != current_selection {
            return Err("Conflict: 默认模型来源或配置已变化，请显式重试".into());
        }
        let journal = |stage: &str, result: Option<&Value>| -> Result<(), String> {
            self.event_bus.publish(EventEnvelope::audit(EventId::unique("plugin-model-operation"), OPERATION_EVENT,
                json!({"key":key,"owner":owner,"requestFingerprint":request_id,"selection":selection,"stage":stage,"result":result})));
            self.event_bus
                .refresh_audit_usage_ledger_persistence()
                .map_err(|e| e.to_string())
        };
        let client = config
            .to_http_model_client()
            .ok_or("默认模型连接不完整")?
            .with_generation_controls(input.temperature, input.stop, input.reasoning_effort)?;
        if let Some(effort) = input.reasoning_effort {
            usage.reasoning_effort = Some(effort);
        }
        let safe = magi_usage_authority::prepare_llm_config_for_persistence(usage);
        let context: magi_app_server_protocol::ModelUsageContext = serde_json::from_value(json!({"templateId":plugin,"engineId":selection["sourceId"].as_str().unwrap_or("orchestrator"),
            "bindingRevision":0,"role":"auxiliary","phase":"execution","modelConfig":safe})).map_err(|e| e.to_string())?;
        journal("started", None)?;
        let is_cancelled =
            || cancelled.load(Ordering::Acquire) || local_cancelled.load(Ordering::Acquire);
        let delivery_error = Mutex::new(None);
        let on_delta = |delta: &magi_bridge_client::ModelStreamingDelta| {
            if !is_cancelled()
                && let Some(progress) = &progress
                && let Err(error) =
                    progress(serde_json::to_value(delta).expect("model delta serializes"))
            {
                *delivery_error.lock().expect("model delivery") = Some(error.to_string());
                local_cancelled.store(true, Ordering::Release);
            }
        };
        let (response, _) = crate::usage_recording::invoke_observed_model_with_usage(
            &client,
            request,
            ModelInvocationOptions {
                on_delta: Some(&on_delta),
                on_retry: &|_| {},
                is_cancelled: &is_cancelled,
                max_output_tokens: Some(limit),
            },
            &self.event_bus,
            &|transport, attempt, call_status| {
                Some(AuxiliaryUsageFact {
                    schema_version: 2,
                    session_id: None,
                    plugin: Some(owner.clone()),
                    turn_id: None,
                    call_id: owner.operation_id.clone(),
                    attempt,
                    context: context.clone(),
                    transport,
                    call_status,
                })
            },
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let error = delivery_error
                    .lock()
                    .expect("model delivery")
                    .clone()
                    .unwrap_or_else(|| error.to_string());
                journal(
                    if is_cancelled() {
                        "cancelled"
                    } else {
                        "failed"
                    },
                    Some(&json!({"error":error})),
                )?;
                return Err(error);
            }
        };
        if is_cancelled() {
            journal("cancelled", None)?;
            return Err("Cancelled: 插件模型结果未交付".into());
        }
        let result = serde_json::to_value(response).map_err(|e| e.to_string())?;
        if serde_json::to_vec(&result)
            .map_err(|e| e.to_string())?
            .len()
            > 1_048_576
        {
            journal("failed", Some(&json!({"error":"模型结果超过 1 MiB"})))?;
            return Err("模型结果超过 1 MiB".into());
        }
        journal("completed", Some(&result))?;
        Ok(result)
    }
}

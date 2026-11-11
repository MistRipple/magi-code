use crate::llm_types::ImageSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeBindingKind {
    Model,
    Mcp,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeDispatchAction {
    ModelPrompt,
    McpToolCall,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeErrorLayer {
    Transport,
    Protocol,
    RemoteBusiness,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInvocationRequest {
    /// 稳定逻辑分支的网关账号亲和身份；不随模型、工具面或权限的 cache epoch 轮换。
    /// 仅由 daemon lineage authority 提供，不参与请求意图序列化。
    #[serde(skip)]
    pub cache_affinity_key: Option<String>,
    /// 由运行时从 branch/provider/epoch 派生的 Provider 缓存键，不使用 Turn、Step 或重试 ID。
    /// 它是传输投影；冻结后的身份由 ModelStep 的缓存摘要拥有，不参与请求意图序列化。
    #[serde(skip)]
    pub prompt_cache_key: Option<String>,
    pub provider: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ChatMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatToolDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ChatToolChoice>,
}

/// 可信调度器在实际 Task 执行边界提供的作用域，不能从模型参数或引擎配置反序列化。
#[derive(Clone, Debug)]
pub struct ModelExecutionScope {
    pub working_dir: Option<std::path::PathBuf>,
    pub policy: magi_core::TaskPolicy,
    pub requires_native_tools: bool,
    pub closed_skill_scope: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolChoice {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolChoiceFunction,
}

impl ChatToolChoice {
    pub fn force_function(name: impl Into<String>) -> Self {
        Self {
            kind: "function".to_string(),
            function: ChatToolChoiceFunction { name: name.into() },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolChoiceFunction {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    /// 可信输入装配端提供的原始 request 身份；只用于冻结消费集合，不发送给 Provider。
    #[serde(skip)]
    pub input_request_id: Option<String>,
    /// 由消息装配端指定的来源；用于本地上下文估算，以及适配器识别请求末尾的
    /// `RoundContext` 易变尾部（决定缓存断点位置），不会发送给模型。
    #[serde(skip)]
    pub context_origin: ChatMessageOrigin,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// assistant 响应携带的提供方私有上下文。
    ///
    /// 该上下文属于整条 assistant message，而不是某个工具调用。运行时只负责
    /// 持久化，协议适配器负责校验并按原协议回放。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatMessageOrigin {
    #[default]
    Conversation,
    SystemInstruction,
    /// 上下文装配器确认的静态规则前缀；缓存策略不得从消息正文中的字符串推断。
    StaticPolicy,
    ProjectContext,
    Skill,
    ContextReference,
    /// 只存在于本次请求视图末尾、每轮重建的易变上下文，不属于可被提供方缓存的历史。
    /// 适配器据此把缓存断点放在它之前；上下文统计仍按引用资料计入。
    RoundContext,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunctionDefinition,
    /// 工具所属的运行时域。该字段只在 Magi 的模型协议边界使用，不会透传给上游。
    ///
    /// 上游保留字或原生能力与本地工具冲突时，必须依据来源生成 wire name；不能
    /// 通过维护一份名称白名单猜测工具身份。
    #[serde(default)]
    pub origin: ChatToolOrigin,
}

/// IR 与 transport 共用同一规范化工具面；数组（含 required/enum）顺序保留，
/// 只规范化对象键和目录顺序。权限撤销或 schema 内容变化仍真实改变输出。
pub fn canonical_tool_surface(tools: &[ChatToolDefinition]) -> Vec<ChatToolDefinition> {
    let mut tools = tools.to_vec();
    for tool in &mut tools {
        tool.function.parameters = crate::llm_types::canonicalize_json(&tool.function.parameters);
    }
    tools.sort_by(|left, right| left.function.name.cmp(&right.function.name));
    tools
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatToolOrigin {
    /// Magi 内置工具与 Magi 自身的协调能力。
    Builtin,
    /// 外部 MCP server 暴露的工具。
    ExternalMcp,
    /// Skill 安装或激活后提供的自定义工具。
    Skill,
    /// 用户安装插件提供的工具。
    Plugin,
    /// 旧会话或第三方 bridge 生成的未分类工具；协议边界使用独立 legacy
    /// 命名空间，绝不把未知身份猜测为内置、MCP 或 Skill 工具。
    #[default]
    Unspecified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatToolFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionPayload {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(
        default,
        rename = "reasoning_content",
        alias = "thinking",
        alias = "reasoning",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

/// 提供方私有但必须跨轮回放的模型上下文。
///
/// `data` 保存已经由协议适配器校验过的完整 wire block；业务运行时只负责持久化，
/// 不解释其中字段。重新请求时由同一提供方适配器决定是否以及如何回放。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderContext {
    pub provider: String,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelResponseStatus {
    Completed,
    RequiresToolExecution,
    Incomplete,
}

/// 所有模型协议在 bridge 边界归一化后的唯一响应结构。
///
/// 会话、辅助模型和产品展示层只能消费该结构，不再解析提供方 JSON 字符串。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelResponse {
    pub status: ModelResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_context: Vec<ModelProviderContext>,
}

impl ModelResponse {
    pub fn completed(content: impl Into<String>) -> Self {
        Self {
            status: ModelResponseStatus::Completed,
            content: Some(content.into()),
            thinking: None,
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: Some("stop".to_string()),
            provider_context: Vec::new(),
        }
    }

    pub fn from_chat_payload(payload: ChatCompletionPayload) -> Self {
        let status = model_response_status(payload.finish_reason.as_deref(), &payload.tool_calls);
        Self {
            status,
            content: payload.content,
            thinking: payload.thinking,
            tool_calls: payload.tool_calls,
            usage: payload.usage,
            finish_reason: payload.finish_reason,
            provider_context: payload.provider_context,
        }
    }

    pub fn is_actionable(&self) -> bool {
        self.content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty())
            || !self.tool_calls.is_empty()
    }
}

fn model_response_status(
    finish_reason: Option<&str>,
    tool_calls: &[ChatToolCall],
) -> ModelResponseStatus {
    let normalized_finish_reason = finish_reason.map(|reason| reason.trim().to_ascii_lowercase());
    let finish_reason = normalized_finish_reason.as_deref();
    if !tool_calls.is_empty()
        || matches!(
            finish_reason,
            Some("tool_calls" | "tool_use" | "function_call")
        )
    {
        return ModelResponseStatus::RequiresToolExecution;
    }
    if matches!(
        finish_reason,
        Some(
            "length"
                | "max_tokens"
                | "max_output_tokens"
                | "content_filter"
                | "safety"
                | "blocked"
                | "recitation"
                | "error"
                | "cancelled"
                | "canceled"
        )
    ) {
        return ModelResponseStatus::Incomplete;
    }
    ModelResponseStatus::Completed
}

#[cfg(test)]
mod model_response_tests {
    use super::*;

    #[test]
    fn response_status_normalizes_cross_provider_finish_reasons() {
        for reason in [
            "length",
            "MAX_TOKENS",
            "max_output_tokens",
            "content_filter",
            "SAFETY",
            "RECITATION",
            "error",
            "cancelled",
        ] {
            assert_eq!(
                model_response_status(Some(reason), &[]),
                ModelResponseStatus::Incomplete,
                "finish_reason={reason}"
            );
        }
        assert_eq!(
            model_response_status(Some("TOOL_USE"), &[]),
            ModelResponseStatus::RequiresToolExecution
        );
        assert_eq!(
            model_response_status(Some("STOP"), &[]),
            ModelResponseStatus::Completed
        );
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelStreamingDelta {
    /// 这一帧**新增**的可见正文（增量，不是到目前为止的全文）；消费方自己累积。
    pub content: String,
    /// 这一帧新增的推理文字（增量）。
    pub thinking: String,
    /// 上游把已经给出的文字整段改写了（例如网页端重绘），无法用追加表达：此时 `content` 与
    /// `thinking` 都是改写后的**完整**文字，消费方丢弃已累积的内容、以它为准。普通追加帧为 false。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
    /// 上游只有工具调用增量、没有可见正文时仍必须把这一事实交给执行层。
    /// 该字段用于首个 raw tool-call chunk 的时序观测，不代表工具已经执行。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
}

impl ModelStreamingDelta {
    /// 积压时把后一帧并入前一帧：追加帧拼接，`replace` 帧取代之前的全部内容。
    pub fn merge(&mut self, next: ModelStreamingDelta) {
        let mut tool_calls = std::mem::take(&mut self.tool_calls);
        tool_calls.extend(next.tool_calls);
        if next.replace {
            self.content = next.content;
            self.thinking = next.thinking;
            self.replace = true;
        } else {
            self.content.push_str(&next.content);
            self.thinking.push_str(&next.thinking);
        }
        self.tool_calls = tool_calls;
    }

    /// 把这一帧并入消费方累积的正文与推理文字：追加帧接在末尾，`replace` 帧整体取代。
    pub fn accumulate_into(&self, content: &mut String, thinking: &mut String) {
        if self.replace {
            content.clear();
            thinking.clear();
        }
        content.push_str(&self.content);
        thinking.push_str(&self.thinking);
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelRetryRuntimePhase {
    Scheduled,
    AttemptStarted,
    Settled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRetryRuntimeEvent {
    pub phase: ModelRetryRuntimePhase,
    pub attempt: usize,
    pub max_attempts: usize,
    pub delay_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    pub server_name: String,
    pub tool_name: String,
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeBindingReference {
    pub binding_id: String,
    pub tool_name: String,
    pub bridge_kind: BridgeBindingKind,
    pub dispatch_action: BridgeDispatchAction,
    pub bridge_target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeBindingDispatchPlan {
    pub source_skill_ids: Vec<String>,
    pub bindings: Vec<BridgeBindingReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeDispatchInput {
    pub binding_id: String,
    pub payload: String,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeDispatchResult {
    pub binding_id: String,
    pub bridge_kind: BridgeBindingKind,
    pub dispatch_action: BridgeDispatchAction,
    pub response: BridgeDispatchResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "response")]
pub enum BridgeDispatchResponse {
    Model(ModelResponse),
    Mcp(BridgeResponse),
}

impl BridgeDispatchResponse {
    pub fn is_success(&self) -> bool {
        match self {
            Self::Model(_) => true,
            Self::Mcp(response) => response.ok,
        }
    }

    pub fn payload(&self) -> &str {
        match self {
            Self::Model(response) => response.content.as_deref().unwrap_or_default(),
            Self::Mcp(response) => &response.payload,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub ok: bool,
    pub payload: String,
}

#[derive(Debug, Error)]
pub enum BridgeClientError {
    #[error("executor_outcome_unknown: {source}；已有执行证据已保留，未自动重发")]
    ExecutorFailed {
        #[source]
        source: Box<BridgeClientError>,
        evidence: Box<crate::ExecutorFailureEvidence>,
    },
    #[error("桥接调用失败[{layer:?}]: {message}")]
    CallFailed {
        layer: BridgeErrorLayer,
        code: Option<i64>,
        message: String,
    },
    #[error("桥接调用失败[{layer:?}]: {message} (http_status={http_status})")]
    HttpStatusFailed {
        layer: BridgeErrorLayer,
        code: Option<i64>,
        http_status: u16,
        message: String,
        /// 服务商通过 Retry-After 要求的等待时间；重试必须遵守。
        retry_after: Option<std::time::Duration>,
    },
    #[error("无效的宿主桥接目标: binding={binding_id}, target={bridge_target}")]
    InvalidBindingTarget {
        binding_id: String,
        bridge_target: String,
    },
    #[error(
        "桥接绑定类型与派发动作不兼容: binding={binding_id}, kind={bridge_kind:?}, action={dispatch_action:?}"
    )]
    IncompatibleBindingAction {
        binding_id: String,
        bridge_kind: BridgeBindingKind,
        dispatch_action: BridgeDispatchAction,
    },
    #[error("缺少桥接 client: {bridge_kind:?}")]
    MissingClient { bridge_kind: BridgeBindingKind },
    #[error("缺少桥接绑定: {binding_id}")]
    MissingBinding { binding_id: String },
    #[error("缺少宿主工作目录: {binding_id}")]
    MissingWorkingDirectory { binding_id: String },
}

/// provider 已明确拒绝当前上下文大小时的结构化信息。
///
/// provider 差异只在 bridge 层归一化；上层运行时不再解析错误文本决定是否压缩。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextOverflowInfo {
    pub context_limit_tokens: Option<u64>,
    pub requested_tokens: Option<u64>,
}

impl BridgeClientError {
    pub fn executor_failure_evidence(&self) -> Option<&crate::ExecutorFailureEvidence> {
        match self {
            Self::ExecutorFailed { evidence, .. } => Some(evidence),
            _ => None,
        }
    }

    pub fn context_overflow(&self) -> Option<ContextOverflowInfo> {
        if let Self::ExecutorFailed { .. } = self {
            // 外部工具可能已经执行，不能因错误正文提到上下文超限而自动重发。
            return None;
        }
        let message = match self {
            Self::CallFailed { message, .. } => message.as_str(),
            Self::HttpStatusFailed { message, .. } => message.as_str(),
            _ => return None,
        };
        let normalized = message.to_ascii_lowercase().replace([',', '_'], " ");
        let overflow_marker = normalized.contains("context length")
            || normalized.contains("context window")
            || normalized.contains("prompt is too long")
            || normalized.contains("prompt too long")
            || normalized.contains("too many tokens")
            || normalized.contains("maximum context")
            || normalized.contains("token limit");
        if !overflow_marker {
            return None;
        }
        let numbers = normalized
            .split(|character: char| !character.is_ascii_digit())
            .filter_map(|part| part.parse::<u64>().ok())
            .collect::<Vec<_>>();
        let context_limit_tokens = numbers
            .iter()
            .copied()
            .find(|value| (1_024..=4_000_000).contains(value));
        let requested_tokens = numbers.iter().copied().rev().find(|value| {
            (1_024..=10_000_000).contains(value) && Some(*value) != context_limit_tokens
        });
        Some(ContextOverflowInfo {
            context_limit_tokens,
            requested_tokens,
        })
    }

    pub fn layer(&self) -> Option<BridgeErrorLayer> {
        match self {
            Self::ExecutorFailed { source, .. } => source.layer(),
            Self::CallFailed { layer, .. } => Some(*layer),
            Self::HttpStatusFailed { layer, .. } => Some(*layer),
            Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }

    pub fn code(&self) -> Option<i64> {
        match self {
            Self::ExecutorFailed { source, .. } => source.code(),
            Self::CallFailed { code, .. } => *code,
            Self::HttpStatusFailed { code, .. } => *code,
            Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }

    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::ExecutorFailed { source, .. } => source.http_status(),
            Self::HttpStatusFailed { http_status, .. } => Some(*http_status),
            Self::CallFailed { .. }
            | Self::InvalidBindingTarget { .. }
            | Self::IncompatibleBindingAction { .. }
            | Self::MissingClient { .. }
            | Self::MissingBinding { .. }
            | Self::MissingWorkingDirectory { .. } => None,
        }
    }
}

/// 实际 transport 的不可变缓存环境。只由可信客户端提供，不接受模型请求传入。
/// endpoint 不含 userinfo、query、fragment；运行时仅持久化其摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCacheIdentity {
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    pub reasoning_profile: String,
}

/// 一次真实 HTTP 尝试的证据。回调失败必须阻止后续发送，不写请求正文或连接凭据。
pub enum ModelTransportEvent {
    Started {
        request_fingerprint: String,
    },
    Updated {
        http_status: Option<u16>,
        usage: Option<magi_app_server_protocol::ModelTransportUsage>,
    },
    ResponseTiming(magi_app_server_protocol::ModelResponseTiming),
    Finished {
        outcome: magi_app_server_protocol::ModelTransportOutcome,
    },
}

/// 只持有本次调用的规范化传输回执；canonical 写入和用量投影复用同一个 reducer。
#[derive(Default)]
pub struct ModelTransportRecorder {
    pub attempts: Vec<magi_app_server_protocol::ModelTransportRecord>,
}

impl ModelTransportRecorder {
    pub fn apply(
        &mut self,
        event: &ModelTransportEvent,
    ) -> Result<&magi_app_server_protocol::ModelTransportRecord, BridgeClientError> {
        let invalid = || BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: Some(-32096),
            message: "transport observation order invalid".into(),
        };
        match event {
            ModelTransportEvent::Started {
                request_fingerprint,
            } => {
                if self
                    .attempts
                    .last()
                    .is_some_and(|record| record.outcome.is_none())
                {
                    return Err(invalid());
                }
                self.attempts
                    .push(magi_app_server_protocol::ModelTransportRecord {
                        usage_observed: Some(false),
                        response_timing: None,
                        observed_at: None,
                        request_fingerprint: request_fingerprint.clone(),
                        http_status: None,
                        usage: None,
                        outcome: None,
                    });
            }
            ModelTransportEvent::Updated { http_status, usage } => {
                let record = self
                    .attempts
                    .last_mut()
                    .filter(|record| record.outcome.is_none())
                    .ok_or_else(invalid)?;
                if let Some(status) = http_status {
                    record.http_status = Some(*status);
                }
                if let Some(usage) = usage {
                    record.usage = Some(usage.clone());
                    record.usage_observed = Some(true);
                }
            }
            ModelTransportEvent::ResponseTiming(timing) => {
                let record = self
                    .attempts
                    .last_mut()
                    .filter(|record| record.outcome.is_none() && record.response_timing.is_none())
                    .ok_or_else(invalid)?;
                record.response_timing = Some(timing.clone());
            }
            ModelTransportEvent::Finished { outcome } => {
                let record = self
                    .attempts
                    .last_mut()
                    .filter(|record| record.outcome.is_none())
                    .ok_or_else(invalid)?;
                record.outcome = Some(outcome.clone());
            }
        }
        let record = self.attempts.last_mut().ok_or_else(invalid)?;
        record.observed_at = Some(
            magi_core::UtcMillis::now()
                .0
                .max(record.observed_at.unwrap_or(0)),
        );
        Ok(record)
    }
}

pub type ModelTransportObserver<'a> =
    dyn Fn(&ModelTransportEvent) -> Result<(), BridgeClientError> + 'a;

pub struct ModelInvocationOptions<'a> {
    pub on_delta: Option<&'a dyn Fn(&ModelStreamingDelta)>,
    pub on_retry: &'a dyn Fn(&ModelRetryRuntimeEvent),
    pub is_cancelled: &'a dyn Fn() -> bool,
    pub max_output_tokens: Option<u32>,
}

pub trait ModelBridgeClient: Send + Sync {
    /// 返回与本次真实 Provider 请求共用的规范化 Prompt IR。
    ///
    /// 缓存摘要必须在同一个入口完成工具名称编码、协议前置归一化等操作；
    /// 调用方不能先对原始请求做一份摘要，再让 adapter 对另一份 IR 发包。
    /// 非 HTTP/未知执行器沿用通用编译器，保持“未观测”而不猜测其 wire 语义。
    fn cache_prompt_ir(&self, request: &ModelInvocationRequest) -> crate::prompt_ir::PromptIr {
        crate::prompt_ir::compile_prompt_ir(request, false, None, None)
    }

    /// 未实现 HTTP 观测的执行器保持未知，不用逻辑响应伪造 transport 证据。
    fn invoke_observed(
        &self,
        request: ModelInvocationRequest,
        options: ModelInvocationOptions<'_>,
        _observer: &ModelTransportObserver<'_>,
    ) -> Result<ModelResponse, BridgeClientError> {
        match (options.on_delta, options.max_output_tokens) {
            (Some(delta), Some(limit)) => self.invoke_streaming_with_output_limit(
                request,
                limit,
                delta,
                options.on_retry,
                options.is_cancelled,
            ),
            (Some(delta), None) => self.invoke_streaming_with_cancellation(
                request,
                delta,
                options.on_retry,
                options.is_cancelled,
            ),
            (None, Some(limit)) => self.invoke_with_retry_events_cancellation_and_output_limit(
                request,
                limit,
                options.on_retry,
                options.is_cancelled,
            ),
            (None, None) => self.invoke_with_retry_events_and_cancellation(
                request,
                options.on_retry,
                options.is_cancelled,
            ),
        }
    }
    /// 每个可信适配器必须显式声明实现的合同，包装器必须转发，不默认赋予原生能力。
    fn executor_capabilities(&self) -> crate::ExecutorCapabilities;
    /// 未声明的执行器保持未知，不能从请求的 provider 路由名猜测实际连接。
    fn cache_identity(&self) -> Option<ModelCacheIdentity> {
        None
    }
    /// 预留的外部执行器接口；当前没有具体外部 Agent 适配器。
    /// 将来接入时须在缓存 client 之外绑定本次 Task 的可信权限/目录。
    /// 普通模型没有本地执行面，保持原 client。返回的新 client 只用于本次执行。
    fn bind_execution_scope(
        &self,
        _scope: ModelExecutionScope,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
        Ok(None)
    }
    fn invoke(&self, request: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError>;

    fn invoke_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if is_cancelled() {
            return Err(model_invocation_cancelled_error());
        }
        self.invoke(request)
    }

    /// 非流式调用也公开 transport 重试边界，供运行时在再次发送前持久化 attempt。
    fn invoke_with_retry_events_and_cancellation(
        &self,
        request: ModelInvocationRequest,
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_cancellation(request, is_cancelled)
    }

    /// 执行带有供应商输出上限的可取消调用。
    ///
    /// 不支持该合同的适配器必须在发送前拒绝，不能静默忽略输出上限。
    fn invoke_with_cancellation_and_output_limit(
        &self,
        _request: ModelInvocationRequest,
        _max_output_tokens: u32,
        _is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        // 能力声明不能替代实现；只有覆盖此方法的适配器才能把上限传给供应商。
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32097),
            message: "executor_output_limit_unsupported: 当前执行器不支持供应商输出上限".into(),
        })
    }

    fn invoke_with_retry_events_cancellation_and_output_limit(
        &self,
        request: ModelInvocationRequest,
        max_output_tokens: u32,
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_with_cancellation_and_output_limit(request, max_output_tokens, is_cancelled)
    }

    /// 流式调用 LLM，`on_delta` 传入新增内容；上游重绘时使用 replace 全文帧。
    /// 实现方必须显式声明流式行为:真流式实现接收 SSE 增量,非流式实现必须返回错误而非静默降级。
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError>;

    fn invoke_streaming_with_retry_events(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke_streaming(request, on_delta)
    }

    fn invoke_streaming_with_cancellation(
        &self,
        request: ModelInvocationRequest,
        on_delta: &dyn Fn(&ModelStreamingDelta),
        on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if is_cancelled() {
            return Err(model_invocation_cancelled_error());
        }
        self.invoke_streaming_with_retry_events(request, on_delta, on_retry)
    }

    fn invoke_streaming_with_output_limit(
        &self,
        _request: ModelInvocationRequest,
        _max_output_tokens: u32,
        _on_delta: &dyn Fn(&ModelStreamingDelta),
        _on_retry: &dyn Fn(&ModelRetryRuntimeEvent),
        _is_cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32097),
            message: "executor_output_limit_unsupported: 当前模型执行器不支持供应商输出上限".into(),
        })
    }
}

pub fn model_invocation_cancelled_error() -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32800),
        message: "model invocation cancelled".to_string(),
    }
}

pub fn model_invocation_error_is_cancelled(error: &BridgeClientError) -> bool {
    error.code() == Some(-32800)
}

#[cfg(test)]
mod context_overflow_tests {
    use super::*;

    #[test]
    fn normalizes_provider_context_overflow_without_runtime_text_parsing() {
        let error = BridgeClientError::HttpStatusFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(400),
            http_status: 400,
            message: "maximum context length is 262144 tokens, requested 300000".to_string(),
            retry_after: None,
        };
        assert_eq!(
            error.context_overflow(),
            Some(ContextOverflowInfo {
                context_limit_tokens: Some(262_144),
                requested_tokens: Some(300_000),
            })
        );
    }

    #[test]
    fn unrelated_bad_request_is_not_context_overflow() {
        let error = BridgeClientError::HttpStatusFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(400),
            http_status: 400,
            message: "invalid temperature".to_string(),
            retry_after: None,
        };
        assert_eq!(error.context_overflow(), None);
    }
}

pub trait McpBridgeClient: Send + Sync {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError>;
}

use super::*;

pub(super) const OPENAI_BASE_URL_ENV: &str = "MAGI_OPENAI_COMPAT_BASE_URL";

pub(super) const OPENAI_API_KEY_ENV: &str = "MAGI_OPENAI_COMPAT_API_KEY";

pub(super) const OPENAI_MODEL_ENV: &str = "MAGI_OPENAI_COMPAT_MODEL";

pub(super) const OPENAI_STREAM_IDLE_TIMEOUT_MS_ENV: &str =
    "MAGI_OPENAI_COMPAT_STREAM_IDLE_TIMEOUT_MS";

pub(super) const MODEL_PROVIDER_MAX_IN_FLIGHT: usize = 16;

/// 暂态故障（5xx、限流、断连、网关抖动）的重试是整个模型调用链里**唯一**的一层：
/// 退避窗口约 1.5 分钟，足以覆盖网关重启、上游短暂过载；更上层不再重复重跑同一请求。
pub(super) const MODEL_PROVIDER_MAX_RETRIES: usize = 8;

pub(super) const MODEL_PROVIDER_EMPTY_STREAM_RETRIES: usize = 2;

/// 等待响应超时（整段 300s 无响应）本身已经很久，再重试多次只会把一轮拖成几十分钟。
pub(super) const MODEL_PROVIDER_TIMEOUT_RETRIES: usize = 2;

pub(super) const MODEL_PROVIDER_RETRY_DELAYS_MILLIS: [u64; MODEL_PROVIDER_MAX_RETRIES] =
    [500, 1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 30_000];

pub(super) const MODEL_PROVIDER_EMPTY_STREAM_RETRY_DELAYS_MILLIS: [u64;
    MODEL_PROVIDER_EMPTY_STREAM_RETRIES] = [1_000, 3_000];

pub(super) const MODEL_PROVIDER_MAX_RETRY_AFTER_DELAY: Duration = Duration::from_secs(60);

pub(super) const MODEL_PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) const MODEL_PROVIDER_MIN_STREAM_IDLE_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) const MODEL_PROVIDER_MAX_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(3_600);

pub(super) const MODEL_PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) const MODEL_CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) const MODEL_PROVIDER_TERMINAL_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

/// 同一上游连接对强制工具选择的真实协议能力。
///
/// 这不是模型型号白名单：能力由实际响应学习，并以协议、规范化端点和模型组成的
/// 连接指纹隔离。API Key 不参与指纹，也不会被记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForcedToolChoiceCapability {
    Unknown,
    /// 提供方接受指定函数的强制选择。
    Exact,
    /// 提供方拒绝指定函数，但接受“本轮必须调用某个工具”。
    /// 当前 required-tool round 只暴露一个工具，因此该语义仍严格等价于指定函数。
    Required,
    /// 提供方没有可用的强制工具选择语义，只能由模型自主选择。
    AutoOnly,
}

pub(super) static FORCED_TOOL_CHOICE_CAPABILITIES: OnceLock<
    Mutex<HashMap<String, ForcedToolChoiceCapability>>,
> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpModelBridgeProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCacheRouteKey {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCacheControl {
    None,
    AnthropicEphemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderContinuation {
    None,
    PreviousResponseId,
    Websocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderCacheContract {
    pub route_key: ProviderCacheRouteKey,
    pub cache_control: ProviderCacheControl,
    pub continuation: ProviderContinuation,
}

impl HttpModelBridgeProtocol {
    pub(super) fn label(self) -> &'static str {
        match self {
            HttpModelBridgeProtocol::ChatCompletions => "chat-completions",
            HttpModelBridgeProtocol::Responses => "responses",
            HttpModelBridgeProtocol::AnthropicMessages => "anthropic-messages",
        }
    }
}

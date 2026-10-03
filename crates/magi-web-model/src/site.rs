//! ChatGPT Web 站点常量、运行时 origin 与引擎身份命名空间。

use std::sync::OnceLock;

/// ChatGPT Web 站点的**产品默认** origin。
///
/// 唯一的运行时取值入口是 [`chatgpt_web_origin`]：产品路径下两者必然相等；
/// 自动化验收（`scripts/verify-web-model-dom.mjs`）通过 `MAGI_WEB_MODEL_ORIGIN`
/// 把站点指到本地假 ChatGPT 页面 fixture，此时只有运行时取值变化，常量不动。
pub const CHATGPT_WEB_ORIGIN: &str = "https://chatgpt.com";

/// 应用级 GPT Web 会话的主页地址（产品默认值）。
pub const CHATGPT_WEB_HOME_URL: &str = "https://chatgpt.com/";

/// 临时对话（Temporary Chat）入口（产品默认值）。
///
/// 对话实例与压缩用的一次性对话都从这里进入：它不进入用户的 ChatGPT 历史记录
/// 。
pub const CHATGPT_WEB_TEMPORARY_CHAT_URL: &str = "https://chatgpt.com/?temporary-chat=true";

/// 站点 origin 的自动化验收覆盖开关。
///
/// 只有站点适配与推理通道共用的 origin 需要被覆盖；它接受一个绝对的
/// `http://` / `https://` 地址（结尾斜杠会被规整掉），其余取值一律忽略，
/// 因此产品环境下不可能被误配置成相对路径或空 origin。
const WEB_MODEL_ORIGIN_ENV: &str = "MAGI_WEB_MODEL_ORIGIN";

/// 校验并规整覆盖值。非法取值（非绝对地址、空 origin）一律丢弃，
/// 保留产品默认，避免半个配置把站点指到未知位置。
fn normalize_origin_override(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim().trim_end_matches('/');
    let (scheme, rest) = trimmed
        .strip_prefix("https://")
        .map(|rest| ("https://", rest))
        .or_else(|| {
            trimmed
                .strip_prefix("http://")
                .map(|rest| ("http://", rest))
        })?;
    (!rest.is_empty()).then(|| format!("{scheme}{rest}"))
}

fn web_model_origin_override() -> Option<&'static str> {
    static OVERRIDE: OnceLock<Option<String>> = OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            normalize_origin_override(std::env::var(WEB_MODEL_ORIGIN_ENV).ok().as_deref())
        })
        .as_deref()
}

/// 当前生效的 ChatGPT Web 站点 origin（默认 `https://chatgpt.com`）。
///
/// 主页地址、临时对话入口与引擎身份判定都必须经过这里，不得各存一份 origin。
pub fn chatgpt_web_origin() -> &'static str {
    web_model_origin_override().unwrap_or(CHATGPT_WEB_ORIGIN)
}

/// 应用级 GPT Web 会话的主页地址。
pub fn chatgpt_web_home_url() -> String {
    format!("{}/", chatgpt_web_origin())
}

/// GPT Web 标签页顶部「快捷地址」可以去的 OpenAI 平台页面：固定白名单，不接受任意地址。
pub const OPENAI_PLATFORM_ORIGIN: &str = "https://platform.openai.com";

/// OpenAI 平台 Tunnels 管理页（创建 / 查看 Tunnel）。
pub fn openai_platform_tunnels_url() -> String {
    format!("{OPENAI_PLATFORM_ORIGIN}/settings/organization/tunnels")
}

/// OpenAI 平台 Runtime API keys 页（创建仅含 Tunnels Read + Use 的运行时密钥）。
pub fn openai_platform_api_keys_url() -> String {
    format!("{OPENAI_PLATFORM_ORIGIN}/settings/organization/api-keys")
}

/// 临时对话（Temporary Chat）入口。
pub fn chatgpt_web_temporary_chat_url() -> String {
    format!("{}/?temporary-chat=true", chatgpt_web_origin())
}

/// Web 引擎 id 的命名空间前缀。
///
/// 引擎身份固定为 `<命名空间>/<family>`（例如 `chatgpt-web/gpt-5`），避免与用户
/// 自填模型在身份与用量统计上串味。
pub const WEB_MODEL_ENGINE_ID_NAMESPACE: &str = "chatgpt-web";

/// GPT Web 引擎的默认 id。发现到具体模型族时会使用同一命名空间下的
/// `chatgpt-web/<family>`，这样多个已登录 Web 模型可以在 Magi 中并列展示，
/// 同时不会与本地/HTTP 引擎混淆。
pub const WEB_MODEL_ENGINE_ID: &str = "chatgpt-web/default";

/// 由站点模型族构造 Web 引擎 id。
pub fn web_model_engine_id(family: &str) -> String {
    let family = family.trim();
    if family.is_empty() || family.eq_ignore_ascii_case("default") {
        WEB_MODEL_ENGINE_ID.to_string()
    } else {
        let normalized = family
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        if normalized.is_empty() {
            WEB_MODEL_ENGINE_ID.to_string()
        } else {
            format!("{WEB_MODEL_ENGINE_ID_NAMESPACE}/{normalized}")
        }
    }
}

/// 判断引擎 id 是否属于 Web 引擎命名空间。
pub fn is_chatgpt_web_engine_id(engine_id: &str) -> bool {
    engine_id
        .strip_prefix(WEB_MODEL_ENGINE_ID_NAMESPACE)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_default_is_used_without_an_override() {
        assert_eq!(chatgpt_web_origin(), CHATGPT_WEB_ORIGIN);
        assert_eq!(chatgpt_web_home_url(), CHATGPT_WEB_HOME_URL);
        assert_eq!(
            chatgpt_web_temporary_chat_url(),
            CHATGPT_WEB_TEMPORARY_CHAT_URL
        );
        assert_eq!(normalize_origin_override(None), None);
    }

    #[test]
    fn only_absolute_origins_survive_normalization() {
        assert_eq!(
            normalize_origin_override(Some("  http://127.0.0.1:8099/  ")).as_deref(),
            Some("http://127.0.0.1:8099")
        );
        assert_eq!(
            normalize_origin_override(Some("https://example.test")).as_deref(),
            Some("https://example.test")
        );
        assert_eq!(normalize_origin_override(Some("")), None);
        assert_eq!(normalize_origin_override(Some("   ")), None);
        assert_eq!(normalize_origin_override(Some("https://")), None);
        assert_eq!(normalize_origin_override(Some("/local/fixture")), None);
        assert_eq!(normalize_origin_override(Some("file:///tmp/fixture")), None);
        assert_eq!(normalize_origin_override(Some("chatgpt.com")), None);
    }

    #[test]
    fn derived_urls_share_one_origin() {
        let origin = chatgpt_web_origin();
        assert!(chatgpt_web_home_url().starts_with(origin));
        assert!(chatgpt_web_temporary_chat_url().starts_with(origin));
        assert!(chatgpt_web_temporary_chat_url().contains("temporary-chat=true"));
    }
}

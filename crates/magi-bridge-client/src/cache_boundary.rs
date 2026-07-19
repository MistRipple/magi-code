//! Prompt 缓存边界标记。
//!
//! # 背景
//!
//! Anthropic Messages API 支持 `cache_control: {"type": "ephemeral"}` 标记，
//! 把 system prompt / messages 中的前缀显式声明为可缓存。命中缓存时输入 token
//! 计费按 1/10 计费，长 system prompt 的成本显著下降。
//!
//! 但 cache_control 必须打在 **content block** 上，而不是顶层 system 字符串。
//! 这意味着 system prompt 必须从单 string 改写成 content blocks 数组：
//!
//! ```json
//! "system": [
//!   {"type": "text", "text": "STATIC 段（角色 / 工具 / 防御）", "cache_control": {"type": "ephemeral"}},
//!   {"type": "text", "text": "DYNAMIC 段（SessionPlan / ExecutionChain / Mailbox）"}
//! ]
//! ```
//!
//! # 设计
//!
//! magi 在 [`conversation_loop`] 里把 17 段提示词拆成多条 `ChatMessage`，
//! 各段已经按 STATIC / SEMI-STATIC / DYNAMIC / APPEND-ONLY 分层注释。
//! 我们在 STATIC 段尾部 / DYNAMIC 段头部之间插入一条
//! [`PROMPT_CACHE_BOUNDARY`] 标记消息——这是一个 **跨 crate 协议字符串**：
//!
//! - 生产端：`magi-conversation-runtime::conversation_loop` 拼装时 push 标记
//! - 消费端：`AnthropicMessagesAdapter` 识别该标记并切分成 content blocks，
//!   静态前缀打 `cache_control: {type: ephemeral}`
//! - 兜底端：其他 protocol adapter（OpenAI Chat 等）透明剥离标记，
//!   确保不会泄漏到非 Anthropic 路径的最终 prompt
//!
//! 选择字符串标记而不是改 [`crate::llm_types::ChatMessage`] 增字段，是为了：
//!
//! 1. 不动消息结构的接口（最小充分修改）
//! 2. boundary 信息天然随 system_prompt join 流转，不需要额外通道
//! 3. 标记字符串足够唯一（含尖括号 + 模块名 + 全大写关键字），不会与
//!    业务 prompt 内容冲突
//!
//! # 复杂度边界
//!
//! 当前只做 **单一边界**（STATIC | NON-STATIC）。Anthropic 协议允许最多
//! 4 个 cache breakpoints，未来若有真实业务场景需要更细分层（SEMI-STATIC
//! 单独缓存等），再扩展第二、第三个 boundary。先做最小可工作版本。

/// STATIC 段与 DYNAMIC 段之间的边界标记。
///
/// 生产侧（conversation_loop）在 Tier A STATIC 段最后一条之后、
/// Tier B SEMI-STATIC 第一条之前 push 一条 content 为该常量的
/// system 消息，下游 adapter 据此切分 cache breakpoint。
///
/// 字符串本身设计要点：
/// - 含尖括号与冒号，业务 prompt 不会偶然产生
/// - 全大写 + `MAGI_` 前缀，跨 crate 抓 grep 立刻能定位
/// - 不含换行，避免被中间 join 步骤切碎
pub const PROMPT_CACHE_BOUNDARY: &str = "<<<MAGI_PROMPT_CACHE_BOUNDARY:STATIC_END>>>";

/// 在拼好的 system 文本中查找 [`PROMPT_CACHE_BOUNDARY`] 第一次出现的位置，
/// 返回 `(static_prefix, dynamic_suffix)` 两段，标记本身被丢弃。
///
/// 若文本中不含标记，返回 `None`，调用方应回退到「不分段，整段当 dynamic」
/// 的兼容行为。
///
/// 多次出现时只切第一处——多个 boundary 是未来扩展点，当前不支持。
pub fn split_at_cache_boundary(system_text: &str) -> Option<(&str, &str)> {
    let idx = system_text.find(PROMPT_CACHE_BOUNDARY)?;
    let static_part = &system_text[..idx];
    let dynamic_part = &system_text[idx + PROMPT_CACHE_BOUNDARY.len()..];
    Some((static_part, dynamic_part))
}

/// 透明剥离所有 [`PROMPT_CACHE_BOUNDARY`] 标记。
///
/// 用于不支持 cache_control 的 adapter（OpenAI Chat Completions 等），
/// 保证标记字符串不会泄漏到最终发送给模型的 prompt。
pub fn strip_cache_boundaries(system_text: &str) -> String {
    if !system_text.contains(PROMPT_CACHE_BOUNDARY) {
        return system_text.to_string();
    }
    system_text.replace(PROMPT_CACHE_BOUNDARY, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_constant_is_unique_enough() {
        // 标记字符串至少含尖括号与全大写关键字，业务 prompt 不会偶然命中。
        assert!(PROMPT_CACHE_BOUNDARY.starts_with("<<<"));
        assert!(PROMPT_CACHE_BOUNDARY.ends_with(">>>"));
        assert!(PROMPT_CACHE_BOUNDARY.contains("MAGI"));
        assert!(!PROMPT_CACHE_BOUNDARY.contains('\n'));
    }

    #[test]
    fn split_returns_two_parts_when_boundary_present() {
        let text = format!("STATIC{}DYNAMIC", PROMPT_CACHE_BOUNDARY);
        let (s, d) = split_at_cache_boundary(&text).expect("应当切分成功");
        assert_eq!(s, "STATIC");
        assert_eq!(d, "DYNAMIC");
    }

    #[test]
    fn split_returns_none_when_boundary_absent() {
        assert!(split_at_cache_boundary("no marker here").is_none());
    }

    #[test]
    fn split_only_splits_at_first_occurrence() {
        let text = format!("A{}B{}C", PROMPT_CACHE_BOUNDARY, PROMPT_CACHE_BOUNDARY);
        let (s, d) = split_at_cache_boundary(&text).expect("应当切分成功");
        assert_eq!(s, "A");
        // 第二个标记仍保留在 dynamic 段，下游 strip 会兜底清理
        assert_eq!(d, format!("B{}C", PROMPT_CACHE_BOUNDARY));
    }

    #[test]
    fn strip_removes_all_occurrences() {
        let text = format!("A{}B{}C", PROMPT_CACHE_BOUNDARY, PROMPT_CACHE_BOUNDARY);
        assert_eq!(strip_cache_boundaries(&text), "ABC");
    }

    #[test]
    fn strip_is_noop_when_absent() {
        assert_eq!(strip_cache_boundaries("plain text"), "plain text");
    }
}

//! 运行时临时上下文注入信道（`<system-reminder>` 风格包装）。
//!
//! 借鉴 Claude Code 的 `wrapInSystemReminder` 约定：把"非用户直接输入、
//! 也不是长期系统规则"的临时上下文——生命周期通知 / 计划状态变更 / 工具
//! 结果附注 / 子代理回传等——统一裹一层 `<system-reminder>...</system-reminder>`
//! 标签，让模型把这部分识别为"系统补充信息"，而不是用户/助手实际发言或
//! 长期不变的系统规则。
//!
//! 该 wrapper 是纯文本拼接，与具体 transport / 消息位置解耦——调用方可以
//! 把结果拼到 system prompt 尾部，也可以拼到下一条 user 消息开头，由
//! caller 按缓存策略决定（参见 Phase 3.2 缓存边界规划）。
//!
//! 设计要点：
//! - **幂等**：`wrap_in_system_reminder` 已经被包过的内容原样返回，避免
//!   多层 caller 叠 tag。
//! - **聚合**：`ReminderBuilder` 把同一轮多条 reminder 聚到同一个块里，
//!   避免一条消息里出现 N 个独立 `<system-reminder>` 标签碎片。
//! - **零依赖**：模块只用 std，不引入新 crate。

const OPEN_TAG: &str = "<system-reminder>";
const CLOSE_TAG: &str = "</system-reminder>";

/// 把单段提醒内容包成 `<system-reminder>...</system-reminder>` 块。
///
/// 已经被包过的内容（首尾正好是 open/close tag）原样返回——多层 caller
/// 嵌套调用时不会出现 `<system-reminder><system-reminder>...` 的双层标签。
/// 内容首尾的空白会被 trim，让 tag 与内容之间始终保持一个换行。
///
/// 空内容（trim 后为空）也会返回一个**带 tag 的空块**——这是有意的：调用
/// 方应当用 `ReminderBuilder` 控制"什么时候不该输出"，本函数只做纯包装。
pub fn wrap_in_system_reminder(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.starts_with(OPEN_TAG) && trimmed.ends_with(CLOSE_TAG) {
        return trimmed.to_string();
    }
    format!("{OPEN_TAG}\n{trimmed}\n{CLOSE_TAG}")
}

/// 多条提醒聚合器。
///
/// 每轮 prompt 拼装通常会有多个零散提醒（生命周期、计划状态、子代理回执
/// ……），把它们推进 builder，最后一次性 `build()` 拼到同一个
/// `<system-reminder>` 块里，避免一条消息里出现多个独立 tag 碎片。
///
/// 空白 / `None` 自动忽略；全空时 `build()` 返回 `None`，让调用方知道
/// "本轮没有需要注入的提醒" 而不是给出空 tag。
#[derive(Default, Debug, Clone)]
pub struct ReminderBuilder {
    sections: Vec<String>,
}

impl ReminderBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推一段提醒；trim 后为空字符串直接忽略。
    pub fn push<S: AsRef<str>>(&mut self, content: S) -> &mut Self {
        let trimmed = content.as_ref().trim();
        if !trimmed.is_empty() {
            self.sections.push(trimmed.to_string());
        }
        self
    }

    /// `Option<&str>` 便捷入口；`None` 或空白原样跳过。
    pub fn push_opt<S: AsRef<str>>(&mut self, content: Option<S>) -> &mut Self {
        if let Some(c) = content {
            self.push(c);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// 推过任何内容时返回完整 `<system-reminder>...</system-reminder>` 块；
    /// 完全没有内容则返回 `None`，避免外层拼出空 tag。
    /// 多条 section 之间用空行分隔，便于模型识别每条 reminder 的边界。
    pub fn build(self) -> Option<String> {
        if self.sections.is_empty() {
            return None;
        }
        let body = self.sections.join("\n\n");
        Some(format!("{OPEN_TAG}\n{body}\n{CLOSE_TAG}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_round_trips_basic_content() {
        let wrapped = wrap_in_system_reminder("Mission M-1 已恢复");
        assert_eq!(
            wrapped,
            "<system-reminder>\nMission M-1 已恢复\n</system-reminder>"
        );
    }

    #[test]
    fn wrap_trims_surrounding_whitespace() {
        let wrapped = wrap_in_system_reminder("  \n通知正文\n  ");
        assert!(wrapped.starts_with("<system-reminder>\n通知正文"));
        assert!(wrapped.ends_with("通知正文\n</system-reminder>"));
    }

    #[test]
    fn wrap_is_idempotent_when_already_wrapped() {
        let once = wrap_in_system_reminder("内容");
        let twice = wrap_in_system_reminder(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn builder_empty_returns_none() {
        let b = ReminderBuilder::new();
        assert!(b.is_empty());
        assert!(b.build().is_none());
    }

    #[test]
    fn builder_skips_blank_and_empty_sections() {
        let mut b = ReminderBuilder::new();
        b.push("").push("  ").push("\n\n");
        assert!(b.is_empty());
        assert!(b.build().is_none());
    }

    #[test]
    fn builder_joins_sections_with_blank_line() {
        let mut b = ReminderBuilder::new();
        b.push("Mission M-1 已恢复").push("子代理 T-3 已完成");
        let out = b.build().expect("有内容应该输出");
        assert_eq!(
            out,
            "<system-reminder>\nMission M-1 已恢复\n\n子代理 T-3 已完成\n</system-reminder>"
        );
    }

    #[test]
    fn builder_push_opt_handles_none() {
        let mut b = ReminderBuilder::new();
        b.push_opt::<&str>(None).push_opt(Some("有效通知"));
        assert_eq!(
            b.build().unwrap(),
            "<system-reminder>\n有效通知\n</system-reminder>"
        );
    }

    #[test]
    fn builder_push_opt_skips_blank_option() {
        let mut b = ReminderBuilder::new();
        b.push_opt(Some("  "));
        assert!(b.build().is_none());
    }
}

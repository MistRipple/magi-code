//! 有界的 Provider 流式写回缓冲。
//!
//! Provider 返回的是不断增长的内容快照。缓冲只保存当前快照和上一次 durable
//! 快照，按字符窗口或时间窗口触发一次 canonical 写回；终态前由 `flush` 强制
//! 交付最后一段内容。这里使用 Unicode scalar value 数量，和协议中的
//! `baseContentLength/contentLength` 保持同一长度单位。

use magi_core::UtcMillis;

const DEFAULT_FLUSH_INTERVAL_MS: u64 = 80;
const DEFAULT_FLUSH_CHAR_WINDOW: usize = 256;

#[derive(Clone, Debug)]
pub struct TurnStreamBuffer {
    flush_interval_ms: u64,
    flush_char_window: usize,
    durable_content: String,
    pending_content: Option<String>,
    last_flush_at: Option<UtcMillis>,
}

impl Default for TurnStreamBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_FLUSH_INTERVAL_MS, DEFAULT_FLUSH_CHAR_WINDOW)
    }
}

impl TurnStreamBuffer {
    pub fn new(flush_interval_ms: u64, flush_char_window: usize) -> Self {
        Self {
            flush_interval_ms,
            flush_char_window: flush_char_window.max(1),
            durable_content: String::new(),
            pending_content: None,
            last_flush_at: None,
        }
    }

    /// 接收一个累计内容快照。返回 `Some(snapshot)` 表示现在应写入 canonical。
    pub fn push(&mut self, content: &str) -> Option<String> {
        self.push_at(content, UtcMillis::now())
    }

    pub fn push_at(&mut self, content: &str, now: UtcMillis) -> Option<String> {
        if self
            .pending_content
            .as_deref()
            .is_some_and(|pending| pending == content)
            || (self.pending_content.is_none() && self.durable_content == content)
        {
            return None;
        }
        let reset = !content.starts_with(&self.durable_content);
        self.pending_content = Some(content.to_string());
        let pending_chars = content
            .chars()
            .count()
            .saturating_sub(self.durable_content.chars().count());
        let time_due = self
            .last_flush_at
            .is_some_and(|last| now.0.saturating_sub(last.0) >= self.flush_interval_ms);
        if self.last_flush_at.is_none()
            || reset
            || time_due
            || pending_chars >= self.flush_char_window
        {
            return self.flush_at(now);
        }
        None
    }

    /// 终态前必须调用，确保最后一个 Provider 快照已经进入 canonical。
    pub fn flush(&mut self) -> Option<String> {
        self.flush_at(UtcMillis::now())
    }

    pub fn flush_at(&mut self, now: UtcMillis) -> Option<String> {
        let content = self.pending_content.take()?;
        if content == self.durable_content {
            self.last_flush_at = Some(now);
            return None;
        }
        self.durable_content.clear();
        self.durable_content.push_str(&content);
        self.last_flush_at = Some(now);
        Some(content)
    }

    pub fn durable_content(&self) -> &str {
        &self.durable_content
    }

    pub fn pending_content(&self) -> Option<&str> {
        self.pending_content.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_snapshot_flushes_and_small_bursts_are_coalesced() {
        let mut buffer = TurnStreamBuffer::new(80, 4);
        assert_eq!(buffer.push_at("你", UtcMillis(1)), Some("你".to_string()));
        assert_eq!(buffer.push_at("你好", UtcMillis(2)), None);
        assert_eq!(buffer.push_at("你好世", UtcMillis(3)), None);
        assert_eq!(
            buffer.push_at("你好世界啊", UtcMillis(4)),
            Some("你好世界啊".to_string())
        );
        assert_eq!(buffer.durable_content(), "你好世界啊");
    }

    #[test]
    fn time_window_and_reset_flush_latest_snapshot() {
        let mut buffer = TurnStreamBuffer::new(10, 100);
        assert_eq!(
            buffer.push_at("old", UtcMillis(100)),
            Some("old".to_string())
        );
        assert_eq!(buffer.push_at("old!", UtcMillis(105)), None);
        assert_eq!(
            buffer.push_at("old!!", UtcMillis(110)),
            Some("old!!".to_string())
        );
        assert_eq!(
            buffer.push_at("new", UtcMillis(111)),
            Some("new".to_string())
        );
        assert_eq!(buffer.flush_at(UtcMillis(112)), None);
    }

    #[test]
    fn terminal_flush_does_not_drop_pending_content() {
        let mut buffer = TurnStreamBuffer::new(80, 256);
        assert_eq!(buffer.push_at("a", UtcMillis(1)), Some("a".to_string()));
        assert_eq!(buffer.push_at("ab", UtcMillis(2)), None);
        assert_eq!(buffer.flush_at(UtcMillis(3)), Some("ab".to_string()));
        assert_eq!(buffer.flush_at(UtcMillis(4)), None);
    }
}

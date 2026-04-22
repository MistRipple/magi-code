use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionStrategy {
    PreventiveTruncation,
    ImportanceBased,
    SummaryBased,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompressionConfig {
    pub max_tokens: usize,
    pub strategy: CompressionStrategy,
    pub preserve_recent_count: usize,
    pub preserve_system: bool,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            max_tokens: 20_000,
            strategy: CompressionStrategy::ImportanceBased,
            preserve_recent_count: 8,
            preserve_system: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextMessage {
    pub role: MessageRole,
    pub content: String,
    pub importance: f64,
    pub token_estimate: usize,
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompressionResult {
    pub messages: Vec<ContextMessage>,
    pub original_count: usize,
    pub retained_count: usize,
    pub original_tokens: usize,
    pub retained_tokens: usize,
    pub strategy_used: CompressionStrategy,
    pub compression_ratio: f64,
}

pub struct ContextAuxiliary {
    config: CompressionConfig,
}

impl ContextAuxiliary {
    pub fn new(config: CompressionConfig) -> Self {
        Self { config }
    }

    pub fn compress(&self, messages: Vec<ContextMessage>) -> CompressionResult {
        let original_count = messages.len();
        let original_tokens: usize = messages.iter().map(|m| m.token_estimate).sum();

        if original_tokens <= self.config.max_tokens {
            return CompressionResult {
                retained_count: original_count,
                retained_tokens: original_tokens,
                compression_ratio: 1.0,
                messages,
                original_count,
                original_tokens,
                strategy_used: self.config.strategy,
            };
        }

        let compressed = match self.config.strategy {
            CompressionStrategy::PreventiveTruncation => {
                self.preventive_truncation(messages)
            }
            CompressionStrategy::ImportanceBased => {
                self.importance_based(messages)
            }
            CompressionStrategy::SummaryBased => {
                self.summary_based(messages)
            }
        };

        let retained_count = compressed.len();
        let retained_tokens: usize = compressed.iter().map(|m| m.token_estimate).sum();

        CompressionResult {
            compression_ratio: if original_tokens > 0 {
                retained_tokens as f64 / original_tokens as f64
            } else {
                1.0
            },
            messages: compressed,
            original_count,
            retained_count,
            original_tokens,
            retained_tokens,
            strategy_used: self.config.strategy,
        }
    }

    fn preventive_truncation(&self, messages: Vec<ContextMessage>) -> Vec<ContextMessage> {
        let total: usize = messages.iter().map(|m| m.token_estimate).sum();
        if total <= self.config.max_tokens {
            return messages;
        }

        let mut result = Vec::new();
        let mut budget = self.config.max_tokens;
        let msg_count = messages.len();

        // 保护尾部消息
        let tail_start = msg_count.saturating_sub(self.config.preserve_recent_count);

        // 第一遍：加入钉住的和系统消息
        let mut included = vec![false; msg_count];
        for (i, msg) in messages.iter().enumerate() {
            if msg.pinned || (self.config.preserve_system && msg.role == MessageRole::System) {
                if msg.token_estimate <= budget {
                    included[i] = true;
                    budget = budget.saturating_sub(msg.token_estimate);
                }
            }
        }

        // 第二遍：加入尾部消息
        for i in tail_start..msg_count {
            if !included[i] && messages[i].token_estimate <= budget {
                included[i] = true;
                budget = budget.saturating_sub(messages[i].token_estimate);
            }
        }

        // 第三遍：填充剩余（从头部开始）
        for i in 0..tail_start {
            if !included[i] && messages[i].token_estimate <= budget {
                included[i] = true;
                budget = budget.saturating_sub(messages[i].token_estimate);
            }
        }

        for (i, msg) in messages.into_iter().enumerate() {
            if included[i] {
                result.push(msg);
            }
        }

        result
    }

    fn importance_based(&self, messages: Vec<ContextMessage>) -> Vec<ContextMessage> {
        let total: usize = messages.iter().map(|m| m.token_estimate).sum();
        if total <= self.config.max_tokens {
            return messages;
        }

        let msg_count = messages.len();
        let tail_start = msg_count.saturating_sub(self.config.preserve_recent_count);

        // 构建评分列表，用于排序
        let mut scored: Vec<(usize, f64, bool)> = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let protected = msg.pinned
                    || (self.config.preserve_system && msg.role == MessageRole::System)
                    || i >= tail_start;
                let score = if protected {
                    f64::MAX
                } else {
                    msg.importance
                };
                (i, score, protected)
            })
            .collect();

        // 按重要性降序排
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut budget = self.config.max_tokens;
        let mut keep = vec![false; msg_count];

        for (i, _, _) in &scored {
            let tok = messages[*i].token_estimate;
            if tok <= budget {
                keep[*i] = true;
                budget = budget.saturating_sub(tok);
            }
        }

        messages
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep[*i])
            .map(|(_, m)| m)
            .collect()
    }

    fn summary_based(&self, messages: Vec<ContextMessage>) -> Vec<ContextMessage> {
        // LLM 压缩需要外部调用，这里降级为基于重要性的策略
        self.importance_based(messages)
    }

    pub fn needs_compression(&self, messages: &[ContextMessage]) -> bool {
        let total: usize = messages.iter().map(|m| m.token_estimate).sum();
        total > self.config.max_tokens
    }

    pub fn estimate_tokens(text: &str) -> usize {
        text.len() / 4 + 1
    }
}

impl Default for ContextAuxiliary {
    fn default() -> Self {
        Self::new(CompressionConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: MessageRole, content: &str, importance: f64) -> ContextMessage {
        ContextMessage {
            role,
            content: content.to_string(),
            importance,
            token_estimate: content.len() / 4 + 1,
            pinned: false,
        }
    }

    fn make_pinned(content: &str) -> ContextMessage {
        ContextMessage {
            role: MessageRole::User,
            content: content.to_string(),
            importance: 1.0,
            token_estimate: content.len() / 4 + 1,
            pinned: true,
        }
    }

    #[test]
    fn no_compression_when_within_budget() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 1000,
            ..Default::default()
        });
        let msgs = vec![
            make_msg(MessageRole::User, "short message", 0.5),
            make_msg(MessageRole::Assistant, "reply", 0.5),
        ];
        let result = aux.compress(msgs);
        assert_eq!(result.original_count, 2);
        assert_eq!(result.retained_count, 2);
        assert!((result.compression_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preventive_truncation_preserves_recent() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 20,
            strategy: CompressionStrategy::PreventiveTruncation,
            preserve_recent_count: 2,
            preserve_system: false,
        });

        let msgs: Vec<ContextMessage> = (0..10)
            .map(|i| make_msg(MessageRole::User, &format!("消息 {i}"), 0.5))
            .collect();

        let result = aux.compress(msgs);
        assert!(result.retained_count < result.original_count);
        assert!(result.retained_tokens <= 20);
    }

    #[test]
    fn importance_based_keeps_high_importance() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 15,
            strategy: CompressionStrategy::ImportanceBased,
            preserve_recent_count: 0,
            preserve_system: false,
        });

        let msgs = vec![
            make_msg(MessageRole::User, "低重要性", 0.1),
            make_msg(MessageRole::User, "高重要性", 0.9),
            make_msg(MessageRole::User, "中重要性", 0.5),
        ];

        let result = aux.compress(msgs);
        assert!(result.retained_count <= result.original_count);
        let contents: Vec<&str> = result.messages.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"高重要性"));
    }

    #[test]
    fn pinned_messages_always_kept() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 10,
            strategy: CompressionStrategy::PreventiveTruncation,
            preserve_recent_count: 0,
            preserve_system: false,
        });

        let msgs = vec![
            make_pinned("钉住"),
            make_msg(MessageRole::User, "普通消息A", 0.1),
            make_msg(MessageRole::User, "普通消息B", 0.1),
        ];

        let result = aux.compress(msgs);
        let contents: Vec<&str> = result.messages.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"钉住"));
    }

    #[test]
    fn system_messages_preserved_when_configured() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 10,
            strategy: CompressionStrategy::PreventiveTruncation,
            preserve_recent_count: 0,
            preserve_system: true,
        });

        let msgs = vec![
            make_msg(MessageRole::System, "系统", 0.0),
            make_msg(MessageRole::User, "用户A", 0.1),
            make_msg(MessageRole::User, "用户B", 0.1),
        ];

        let result = aux.compress(msgs);
        let roles: Vec<MessageRole> = result.messages.iter().map(|m| m.role).collect();
        assert!(roles.contains(&MessageRole::System));
    }

    #[test]
    fn needs_compression_checks_threshold() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 10,
            ..Default::default()
        });

        let small = vec![make_msg(MessageRole::User, "hi", 0.5)];
        assert!(!aux.needs_compression(&small));

        let large: Vec<ContextMessage> = (0..100)
            .map(|i| make_msg(MessageRole::User, &format!("消息内容_{i}_填充长度"), 0.5))
            .collect();
        assert!(aux.needs_compression(&large));
    }

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(ContextAuxiliary::estimate_tokens(""), 1);
        assert_eq!(ContextAuxiliary::estimate_tokens("abcd"), 2);
        assert_eq!(ContextAuxiliary::estimate_tokens("12345678"), 3);
    }

    #[test]
    fn compression_result_serializes() {
        let result = CompressionResult {
            messages: vec![],
            original_count: 10,
            retained_count: 5,
            original_tokens: 1000,
            retained_tokens: 500,
            strategy_used: CompressionStrategy::ImportanceBased,
            compression_ratio: 0.5,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"importance_based\""));
        assert!(json.contains("0.5"));
    }

    #[test]
    fn config_serializes() {
        let config = CompressionConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"importance_based\""));
        assert!(json.contains("20000"));
    }

    #[test]
    fn summary_based_falls_back_to_importance() {
        let aux = ContextAuxiliary::new(CompressionConfig {
            max_tokens: 15,
            strategy: CompressionStrategy::SummaryBased,
            preserve_recent_count: 0,
            preserve_system: false,
        });

        let msgs = vec![
            make_msg(MessageRole::User, "低", 0.1),
            make_msg(MessageRole::User, "高", 0.9),
        ];

        let result = aux.compress(msgs);
        assert_eq!(result.strategy_used, CompressionStrategy::SummaryBased);
    }
}

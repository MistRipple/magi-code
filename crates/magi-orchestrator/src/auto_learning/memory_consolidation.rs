use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationConfig {
    pub min_new_entries: usize,
    pub interval_ms: u64,
    pub max_topic_tokens: usize,
    pub max_total_tokens: usize,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            min_new_entries: 5,
            interval_ms: 60 * 60 * 1000,
            max_topic_tokens: 800,
            max_total_tokens: 8000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationResult {
    pub processed_entries: usize,
    pub updated_topics: usize,
    pub forgotten_entries: usize,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct ConsolidationState {
    pub last_consolidation_at: u64,
    pub last_processed_count: usize,
    pub total_consolidations: usize,
}

#[derive(Clone, Debug)]
struct ParsedMemoryEntry {
    content: String,
    citations: Vec<String>,
    created_at: u64,
    confidence: f64,
}

#[derive(Clone, Debug)]
pub struct TopicBlock {
    topic: String,
    entries: Vec<ParsedMemoryEntry>,
}

const EXPIRY_THRESHOLD_MS: u64 = 45 * 24 * 60 * 60 * 1000;
const DEDUP_SIMILARITY_THRESHOLD: f64 = 0.85;

const TOPIC_KEYWORDS: &[(&[&str], &str)] = &[
    (&["架构", "设计", "模块", "接口", "服务"], "架构设计"),
    (&["代码", "实现", "函数", "类", "变量", "重构"], "代码实现"),
    (&["测试", "单测", "集成测试", "验证"], "测试验证"),
    (&["部署", "发布", "CI", "CD", "构建"], "部署构建"),
    (&["性能", "优化", "缓存", "并发"], "性能优化"),
    (&["错误", "修复", "Bug", "调试", "排查"], "问题排查"),
    (&["用户", "偏好", "反馈", "纠正"], "用户偏好"),
];

const TOPIC_ORDER: &[&str] = &[
    "架构设计",
    "代码实现",
    "测试验证",
    "部署构建",
    "性能优化",
    "问题排查",
    "用户偏好",
    "通用知识",
];

pub struct MemoryConsolidationService {
    config: ConsolidationConfig,
    state: ConsolidationState,
    pending_entries: Vec<RawMemoryInput>,
}

#[derive(Clone, Debug)]
pub struct RawMemoryInput {
    pub content: String,
    pub citations: Vec<String>,
    pub created_at: u64,
    pub confidence: f64,
}

impl MemoryConsolidationService {
    pub fn new(config: Option<ConsolidationConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            state: ConsolidationState::default(),
            pending_entries: Vec::new(),
        }
    }

    pub fn should_consolidate(&self) -> bool {
        if self.state.total_consolidations == 0 {
            return self.pending_entries.len() >= self.config.min_new_entries;
        }
        self.pending_entries.len() >= self.config.min_new_entries
    }

    pub fn add_entry(&mut self, entry: RawMemoryInput) {
        self.pending_entries.push(entry);
    }

    pub fn consolidate(&mut self) -> ConsolidationResult {
        let start = now_millis();

        let entries: Vec<ParsedMemoryEntry> = self
            .pending_entries
            .drain(..)
            .map(|e| ParsedMemoryEntry {
                content: e.content,
                citations: e.citations,
                created_at: e.created_at,
                confidence: e.confidence,
            })
            .collect();

        let total = entries.len();
        let (active, forgotten) = self.partition_expired(entries, start);
        let forgotten_count = forgotten.len();

        let mut topic_blocks = self.classify_into_topics(active);
        let mut updated_topics = 0;

        for block in &mut topic_blocks {
            let before = block.entries.len();
            self.deduplicate_topic(block);
            self.enforce_topic_token_limit(block);
            if !block.entries.is_empty() {
                updated_topics += 1;
            }
            let _ = before;
        }

        self.state.last_consolidation_at = start;
        self.state.last_processed_count = total;
        self.state.total_consolidations += 1;

        ConsolidationResult {
            processed_entries: total,
            updated_topics,
            forgotten_entries: forgotten_count,
            duration_ms: now_millis().saturating_sub(start),
        }
    }

    pub fn render_memory_md(&self, topic_blocks: &[TopicBlock]) -> String {
        let mut lines = vec!["# 项目知识手册".to_string(), String::new()];

        for &topic in TOPIC_ORDER {
            if let Some(block) = topic_blocks.iter().find(|b| b.topic == topic) {
                if block.entries.is_empty() {
                    continue;
                }
                lines.push(format!("## {topic}"));
                lines.push(String::new());
                for entry in &block.entries {
                    let citations = format_citations(&entry.citations);
                    lines.push(format!("- {}{citations}", entry.content));
                }
                lines.push(String::new());
            }
        }
        lines.join("\n")
    }

    pub fn state(&self) -> &ConsolidationState {
        &self.state
    }

    pub fn pending_count(&self) -> usize {
        self.pending_entries.len()
    }

    fn partition_expired(
        &self,
        entries: Vec<ParsedMemoryEntry>,
        now: u64,
    ) -> (Vec<ParsedMemoryEntry>, Vec<ParsedMemoryEntry>) {
        let mut active = Vec::new();
        let mut expired = Vec::new();
        for entry in entries {
            if now.saturating_sub(entry.created_at) > EXPIRY_THRESHOLD_MS {
                expired.push(entry);
            } else {
                active.push(entry);
            }
        }
        (active, expired)
    }

    fn classify_into_topics(&self, entries: Vec<ParsedMemoryEntry>) -> Vec<TopicBlock> {
        let mut topic_map: HashMap<String, Vec<ParsedMemoryEntry>> = HashMap::new();

        for entry in entries {
            let topic = classify_topic(&entry.content);
            topic_map.entry(topic.to_string()).or_default().push(entry);
        }

        topic_map
            .into_iter()
            .map(|(topic, entries)| TopicBlock { topic, entries })
            .collect()
    }

    fn deduplicate_topic(&self, block: &mut TopicBlock) {
        let mut unique: Vec<ParsedMemoryEntry> = Vec::new();
        for entry in block.entries.drain(..) {
            let normalized = normalize_text(&entry.content);
            let is_dup = unique.iter().any(|existing| {
                compute_similarity(&normalized, &normalize_text(&existing.content))
                    >= DEDUP_SIMILARITY_THRESHOLD
            });
            if !is_dup {
                unique.push(entry);
            }
        }
        block.entries = unique;
    }

    fn enforce_topic_token_limit(&self, block: &mut TopicBlock) {
        let mut total_tokens = 0usize;
        let mut keep = Vec::new();
        block.entries.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for entry in block.entries.drain(..) {
            let tokens = estimate_tokens(&entry.content);
            if total_tokens + tokens > self.config.max_topic_tokens {
                break;
            }
            total_tokens += tokens;
            keep.push(entry);
        }
        block.entries = keep;
    }
}

impl Default for MemoryConsolidationService {
    fn default() -> Self {
        Self::new(None)
    }
}

fn classify_topic(content: &str) -> &'static str {
    for (keywords, topic) in TOPIC_KEYWORDS {
        if keywords.iter().any(|kw| content.contains(kw)) {
            return topic;
        }
    }
    "通用知识"
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn compute_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let chars_a: std::collections::HashSet<char> = a.chars().collect();
    let chars_b: std::collections::HashSet<char> = b.chars().collect();
    let intersection = chars_a.intersection(&chars_b).count();
    let union = chars_a.len() + chars_b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

fn format_citations(citations: &[String]) -> String {
    let valid: Vec<&str> = citations
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    if valid.is_empty() {
        String::new()
    } else {
        format!(" [{}]", valid.join(", "))
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

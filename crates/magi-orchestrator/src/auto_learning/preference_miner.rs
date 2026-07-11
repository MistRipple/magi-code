use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceCategory {
    Style,
    Tool,
    Workflow,
    Constraint,
    Format,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinedPreference {
    pub pattern: String,
    pub category: PreferenceCategory,
    pub evidence: Vec<String>,
    pub confidence: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferenceMiningResult {
    pub preferences: Vec<MinedPreference>,
    pub has_new_high_signal: bool,
}

const HIGH_SIGNAL_THRESHOLD: f64 = 0.8;

pub struct PreferenceMiner;

impl PreferenceMiner {
    pub fn new() -> Self {
        Self
    }

    pub fn mine_from_conversation(
        &self,
        user_messages: &[&str],
        assistant_messages: &[&str],
    ) -> PreferenceMiningResult {
        let mut preferences = Vec::new();

        for (i, user_msg) in user_messages.iter().enumerate() {
            let prev_assistant = if i > 0 {
                assistant_messages.get(i - 1).copied().unwrap_or("")
            } else {
                ""
            };

            if let Some(p) = self.extract_correction(user_msg, prev_assistant) {
                preferences.push(p);
            }
            if let Some(p) = self.extract_mandatory(user_msg) {
                preferences.push(p);
            }
            if let Some(p) = self.extract_format(user_msg) {
                preferences.push(p);
            }
            if let Some(p) = self.extract_style(user_msg) {
                preferences.push(p);
            }
        }

        let repetition = self.detect_repetition_patterns(user_messages);
        preferences.extend(repetition);

        let deduplicated = self.deduplicate(preferences);
        let has_new_high_signal = deduplicated
            .iter()
            .any(|p| p.confidence >= HIGH_SIGNAL_THRESHOLD);

        PreferenceMiningResult {
            preferences: deduplicated,
            has_new_high_signal,
        }
    }

    fn extract_correction(&self, user_msg: &str, prev_assistant: &str) -> Option<MinedPreference> {
        let prefixes = ["不要", "别", "不要用", "不要写", "不能"];
        for prefix in &prefixes {
            if let Some(rest) = user_msg.strip_prefix(prefix) {
                let behavior = rest.trim();
                if behavior.is_empty() {
                    continue;
                }
                let mut evidence = vec![user_msg.to_string()];
                if !prev_assistant.is_empty() {
                    let truncated = if prev_assistant.len() > 120 {
                        format!("{}...", &prev_assistant[..120])
                    } else {
                        prev_assistant.to_string()
                    };
                    evidence.push(format!("[上下文] {truncated}"));
                }
                return Some(MinedPreference {
                    pattern: format!("不要{behavior}"),
                    category: PreferenceCategory::Constraint,
                    evidence,
                    confidence: 0.9,
                });
            }
        }

        for prefix in &prefixes {
            if let Some(pos) = user_msg.find(prefix) {
                let rest = &user_msg[pos + prefix.len()..];
                let behavior = rest.trim();
                if behavior.is_empty() {
                    continue;
                }
                let behavior = behavior
                    .split(['，', '。', ',', '.'])
                    .next()
                    .unwrap_or(behavior)
                    .trim();
                if behavior.is_empty() {
                    continue;
                }
                return Some(MinedPreference {
                    pattern: format!("不要{behavior}"),
                    category: PreferenceCategory::Constraint,
                    evidence: vec![user_msg.to_string()],
                    confidence: 0.7,
                });
            }
        }
        None
    }

    fn extract_mandatory(&self, user_msg: &str) -> Option<MinedPreference> {
        let prefixes = ["必须", "一定要", "务必"];
        for prefix in &prefixes {
            if let Some(pos) = user_msg.find(prefix) {
                let rest = &user_msg[pos + prefix.len()..];
                let behavior = rest
                    .split(['，', '。', ',', '.'])
                    .next()
                    .unwrap_or(rest)
                    .trim();
                if behavior.is_empty() {
                    continue;
                }
                return Some(MinedPreference {
                    pattern: format!("必须{behavior}"),
                    category: PreferenceCategory::Constraint,
                    evidence: vec![user_msg.to_string()],
                    confidence: 0.9,
                });
            }
        }
        None
    }

    fn extract_format(&self, user_msg: &str) -> Option<MinedPreference> {
        let patterns = [
            ("用中文", PreferenceCategory::Format, "使用中文输出"),
            ("用英文", PreferenceCategory::Format, "使用英文输出"),
            ("Markdown", PreferenceCategory::Format, "使用 Markdown 格式"),
            ("表格", PreferenceCategory::Format, "使用表格展示"),
            ("列表", PreferenceCategory::Format, "使用列表展示"),
            ("代码块", PreferenceCategory::Format, "使用代码块"),
        ];
        for (keyword, category, pattern) in &patterns {
            if user_msg.contains(keyword) {
                return Some(MinedPreference {
                    pattern: pattern.to_string(),
                    category: *category,
                    evidence: vec![user_msg.to_string()],
                    confidence: 0.7,
                });
            }
        }
        None
    }

    fn extract_style(&self, user_msg: &str) -> Option<MinedPreference> {
        let patterns = [
            ("直接", "倾向直接简洁的回答"),
            ("简单点", "倾向简洁的回答"),
            ("简洁", "倾向简洁的回答"),
            ("别废话", "不要冗余解释"),
            ("快速", "倾向快速执行"),
            ("一步到位", "倾向一步到位完成"),
        ];
        for (keyword, pattern) in &patterns {
            if user_msg.contains(keyword) {
                return Some(MinedPreference {
                    pattern: pattern.to_string(),
                    category: PreferenceCategory::Style,
                    evidence: vec![user_msg.to_string()],
                    confidence: 0.6,
                });
            }
        }
        None
    }

    fn detect_repetition_patterns(&self, user_messages: &[&str]) -> Vec<MinedPreference> {
        if user_messages.len() < 3 {
            return Vec::new();
        }

        let mut prefs = Vec::new();
        let keywords = ["继续", "下一步", "接着"];

        for keyword in &keywords {
            let count = user_messages
                .iter()
                .filter(|msg| msg.contains(keyword))
                .count();
            if count >= 3 {
                prefs.push(MinedPreference {
                    pattern: format!("用户习惯使用「{keyword}」推进工作"),
                    category: PreferenceCategory::Workflow,
                    evidence: user_messages
                        .iter()
                        .filter(|msg| msg.contains(keyword))
                        .take(3)
                        .map(|s| s.to_string())
                        .collect(),
                    confidence: 0.5 + (count as f64 * 0.1).min(0.4),
                });
            }
        }
        prefs
    }

    fn deduplicate(&self, prefs: Vec<MinedPreference>) -> Vec<MinedPreference> {
        let mut result: Vec<MinedPreference> = Vec::new();
        for pref in prefs {
            if let Some(existing) = result
                .iter_mut()
                .find(|p| p.pattern == pref.pattern && p.category == pref.category)
            {
                if pref.confidence > existing.confidence {
                    existing.confidence = pref.confidence;
                }
                for ev in &pref.evidence {
                    if !existing.evidence.contains(ev) {
                        existing.evidence.push(ev.clone());
                    }
                }
            } else {
                result.push(pref);
            }
        }
        result
    }
}

impl Default for PreferenceMiner {
    fn default() -> Self {
        Self::new()
    }
}

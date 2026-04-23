use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySignalType {
    UserPreference,
    ProceduralKnowledge,
    DecisionTrigger,
    EnvironmentEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryImportance {
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayer {
    ShortTerm,
    Working,
    LongTerm,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub source_key: String,
    pub content: String,
    pub signal_type: MemorySignalType,
    pub supported_by: Vec<String>,
    pub tags: Vec<String>,
    pub created_at: u64,
    pub last_accessed_at: u64,
    pub access_count: u32,
    pub importance: MemoryImportance,
    pub layer: MemoryLayer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferenceEntry {
    pub id: String,
    pub pattern: String,
    pub evidence: Vec<String>,
    pub category: PreferenceCategory,
    pub confidence: f64,
    pub supported_by: Vec<String>,
    pub created_at: u64,
    pub last_confirmed_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceCategory {
    Style,
    Tool,
    Workflow,
    Constraint,
    Format,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct LayeredMemoryStore {
    memories: HashMap<String, MemoryEntry>,
    preferences: HashMap<String, PreferenceEntry>,
    source_index: HashMap<String, Vec<String>>,
    max_short_term: usize,
    max_working: usize,
    max_long_term: usize,
}

impl LayeredMemoryStore {
    pub fn new(
        max_short_term: Option<usize>,
        max_working: Option<usize>,
        max_long_term: Option<usize>,
    ) -> Self {
        Self {
            memories: HashMap::new(),
            preferences: HashMap::new(),
            source_index: HashMap::new(),
            max_short_term: max_short_term.unwrap_or(100),
            max_working: max_working.unwrap_or(50),
            max_long_term: max_long_term.unwrap_or(200),
        }
    }

    pub fn add_memory(&mut self, entry: MemoryEntry) -> bool {
        if entry.id.is_empty() || entry.content.is_empty() {
            return false;
        }
        let id = entry.id.clone();
        let source_key = entry.source_key.clone();

        self.memories.insert(id.clone(), entry);
        self.source_index.entry(source_key).or_default().push(id);

        self.enforce_layer_limits();
        true
    }

    pub fn get_memory(&mut self, id: &str) -> Option<&MemoryEntry> {
        if let Some(entry) = self.memories.get_mut(id) {
            entry.last_accessed_at = now_millis();
            entry.access_count += 1;
        }
        self.memories.get(id)
    }

    pub fn query_by_signal_type(&self, signal_type: MemorySignalType) -> Vec<&MemoryEntry> {
        let mut results: Vec<&MemoryEntry> = self
            .memories
            .values()
            .filter(|e| e.signal_type == signal_type && !self.is_expired(e))
            .collect();
        results.sort_by(|a, b| b.importance.cmp(&a.importance));
        results
    }

    pub fn query_by_tags(&self, tags: &[String]) -> Vec<&MemoryEntry> {
        let mut results: Vec<&MemoryEntry> = self
            .memories
            .values()
            .filter(|e| !self.is_expired(e) && e.tags.iter().any(|t| tags.contains(t)))
            .collect();
        results.sort_by(|a, b| b.importance.cmp(&a.importance));
        results
    }

    pub fn query_by_layer(&self, layer: MemoryLayer) -> Vec<&MemoryEntry> {
        let mut results: Vec<&MemoryEntry> = self
            .memories
            .values()
            .filter(|e| e.layer == layer && !self.is_expired(e))
            .collect();
        results.sort_by(|a, b| b.importance.cmp(&a.importance));
        results
    }

    pub fn query_by_source(&self, source_key: &str) -> Vec<&MemoryEntry> {
        let Some(ids) = self.source_index.get(source_key) else {
            return vec![];
        };
        ids.iter()
            .filter_map(|id| self.memories.get(id))
            .filter(|e| !self.is_expired(e))
            .collect()
    }

    pub fn promote(&mut self, id: &str, target_layer: MemoryLayer) -> bool {
        let Some(entry) = self.memories.get_mut(id) else {
            return false;
        };
        if entry.layer >= target_layer {
            return false;
        }
        entry.layer = target_layer;
        entry.last_accessed_at = now_millis();
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let Some(entry) = self.memories.remove(id) else {
            return false;
        };
        if let Some(ids) = self.source_index.get_mut(&entry.source_key) {
            ids.retain(|i| i != id);
        }
        true
    }

    pub fn add_preference(&mut self, pref: PreferenceEntry) -> bool {
        if pref.id.is_empty() {
            return false;
        }
        self.preferences.insert(pref.id.clone(), pref);
        true
    }

    pub fn get_preferences_by_category(
        &self,
        category: PreferenceCategory,
    ) -> Vec<&PreferenceEntry> {
        let mut results: Vec<&PreferenceEntry> = self
            .preferences
            .values()
            .filter(|p| p.category == category)
            .collect();
        results.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results
    }

    pub fn all_preferences(&self) -> Vec<&PreferenceEntry> {
        self.preferences.values().collect()
    }

    pub fn cleanup_expired(&mut self) -> usize {
        let now = now_millis();
        let expired: Vec<String> = self
            .memories
            .iter()
            .filter(|(_, e)| {
                e.ttl_ms
                    .map_or(false, |ttl| now.saturating_sub(e.created_at) > ttl)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let count = expired.len();
        for id in expired {
            self.remove(&id);
        }
        count
    }

    pub fn len(&self) -> usize {
        self.memories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
    }

    pub fn preference_count(&self) -> usize {
        self.preferences.len()
    }

    fn is_expired(&self, entry: &MemoryEntry) -> bool {
        let Some(ttl) = entry.ttl_ms else {
            return false;
        };
        now_millis().saturating_sub(entry.created_at) > ttl
    }

    fn enforce_layer_limits(&mut self) {
        self.enforce_limit_for_layer(MemoryLayer::ShortTerm, self.max_short_term);
        self.enforce_limit_for_layer(MemoryLayer::Working, self.max_working);
        self.enforce_limit_for_layer(MemoryLayer::LongTerm, self.max_long_term);
    }

    fn enforce_limit_for_layer(&mut self, layer: MemoryLayer, limit: usize) {
        let mut layer_entries: Vec<(String, u64, MemoryImportance)> = self
            .memories
            .iter()
            .filter(|(_, e)| e.layer == layer)
            .map(|(id, e)| (id.clone(), e.last_accessed_at, e.importance))
            .collect();

        if layer_entries.len() <= limit {
            return;
        }

        layer_entries.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(&b.1)));

        let overflow = layer_entries.len() - limit;
        for (id, _, _) in layer_entries.into_iter().take(overflow) {
            self.remove(&id);
        }
    }
}

impl Default for LayeredMemoryStore {
    fn default() -> Self {
        Self::new(None, None, None)
    }
}

use magi_core::UtcMillis;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedContextEntryType {
    Decision,
    Contract,
    FileSummary,
    Risk,
    Constraint,
    Insight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportanceLevel {
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

impl ImportanceLevel {
    pub fn score(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReference {
    pub path: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedContextEntry {
    pub id: String,
    pub mission_id: String,
    pub source: String,
    pub entry_type: SharedContextEntryType,
    pub content: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_refs: Vec<FileReference>,
    pub importance: ImportanceLevel,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddAction {
    Added,
    Merged,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub action: AddAction,
    pub id: Option<String>,
    pub existing_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct QueryOptions {
    pub min_importance: Option<ImportanceLevel>,
    pub subscribed_tags: Option<Vec<String>>,
    pub exclude_sources: Option<Vec<String>>,
    pub max_tokens: Option<usize>,
}

const MAX_CONTENT_LENGTH: usize = 2000;
const SIMILARITY_THRESHOLD: f64 = 0.9;
const DEDUP_PREFIX_SAMPLE_SIZE: usize = 96;

pub struct MissionSharedContextPool {
    entries: HashMap<String, SharedContextEntry>,
    mission_type_index: HashMap<String, HashSet<String>>,
}

impl MissionSharedContextPool {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            mission_type_index: HashMap::new(),
        }
    }

    pub fn add(&mut self, entry: SharedContextEntry) -> AddResult {
        if entry.id.is_empty() || entry.mission_id.is_empty() || entry.content.is_empty() {
            return AddResult {
                action: AddAction::Merged,
                id: None,
                existing_id: None,
            };
        }

        let content = if entry.content.len() > MAX_CONTENT_LENGTH {
            entry.content[..MAX_CONTENT_LENGTH].to_string()
        } else {
            entry.content.clone()
        };

        if let Some(dup_id) = self.find_duplicate(&entry.mission_id, &entry.entry_type, &content) {
            if let Some(existing) = self.entries.get_mut(&dup_id) {
                if !existing.sources.contains(&entry.source) {
                    existing.sources.push(entry.source.clone());
                }
                existing.created_at = existing.created_at.max(entry.created_at);
            }
            return AddResult {
                action: AddAction::Merged,
                id: None,
                existing_id: Some(dup_id),
            };
        }

        let id = entry.id.clone();
        let mission_id = entry.mission_id.clone();
        let entry_type = entry.entry_type;
        let mut entry = entry;
        entry.content = content;

        self.entries.insert(id.clone(), entry);

        let index_key = format!("{}:{:?}", mission_id, entry_type);
        self.mission_type_index
            .entry(index_key)
            .or_default()
            .insert(id.clone());

        AddResult {
            action: AddAction::Added,
            id: Some(id),
            existing_id: None,
        }
    }

    pub fn get_by_mission(
        &self,
        mission_id: &str,
        options: &QueryOptions,
    ) -> Vec<&SharedContextEntry> {
        let now = UtcMillis::now().0;
        let mut results: Vec<&SharedContextEntry> = self
            .entries
            .values()
            .filter(|e| {
                if e.mission_id != mission_id {
                    return false;
                }
                if let Some(expires) = e.expires_at
                    && expires < now
                {
                    return false;
                }
                if let Some(min_imp) = options.min_importance
                    && e.importance < min_imp
                {
                    return false;
                }
                if let Some(ref tags) = options.subscribed_tags
                    && !tags.is_empty()
                    && !e.tags.iter().any(|t| tags.contains(t))
                {
                    return false;
                }
                if let Some(ref excludes) = options.exclude_sources
                    && excludes.contains(&e.source)
                {
                    return false;
                }
                true
            })
            .collect();

        results.sort_by(|a, b| b.importance.cmp(&a.importance));

        if let Some(max_tokens) = options.max_tokens {
            let mut total_tokens = 0usize;
            results.retain(|e| {
                let est = estimate_tokens(&e.content);
                if total_tokens + est > max_tokens {
                    return false;
                }
                total_tokens += est;
                true
            });
        }

        results
    }

    pub fn get_by_type(
        &self,
        mission_id: &str,
        entry_type: SharedContextEntryType,
    ) -> Vec<&SharedContextEntry> {
        let index_key = format!("{}:{:?}", mission_id, entry_type);
        let Some(ids) = self.mission_type_index.get(&index_key) else {
            return vec![];
        };
        let now = UtcMillis::now().0;
        let mut entries: Vec<&SharedContextEntry> = ids
            .iter()
            .filter_map(|id| self.entries.get(id))
            .filter(|e| e.expires_at.is_none_or(|exp| exp >= now))
            .collect();
        entries.sort_by(|a, b| b.importance.cmp(&a.importance));
        entries
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let Some(entry) = self.entries.remove(id) else {
            return false;
        };
        let index_key = format!("{}:{:?}", entry.mission_id, entry.entry_type);
        if let Some(set) = self.mission_type_index.get_mut(&index_key) {
            set.remove(id);
        }
        true
    }

    pub fn remove_by_mission(&mut self, mission_id: &str) -> usize {
        let ids: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.mission_id == mission_id)
            .map(|(id, _)| id.clone())
            .collect();
        let count = ids.len();
        for id in ids {
            self.remove(&id);
        }
        count
    }

    pub fn cleanup_expired(&mut self) -> usize {
        let now = UtcMillis::now().0;
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.expires_at.is_some_and(|exp| exp < now))
            .map(|(id, _)| id.clone())
            .collect();
        let count = expired.len();
        for id in expired {
            self.remove(&id);
        }
        count
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find_duplicate(
        &self,
        mission_id: &str,
        entry_type: &SharedContextEntryType,
        content: &str,
    ) -> Option<String> {
        let index_key = format!("{}:{:?}", mission_id, entry_type);
        let ids = self.mission_type_index.get(&index_key)?;

        let prefix = &content[..content.len().min(DEDUP_PREFIX_SAMPLE_SIZE)];

        for id in ids {
            let existing = self.entries.get(id)?;
            let existing_prefix =
                &existing.content[..existing.content.len().min(DEDUP_PREFIX_SAMPLE_SIZE)];
            if prefix != existing_prefix {
                continue;
            }

            let sim = content_similarity(content, &existing.content);
            if sim >= SIMILARITY_THRESHOLD {
                return Some(id.clone());
            }
        }
        None
    }
}

impl Default for MissionSharedContextPool {
    fn default() -> Self {
        Self::new()
    }
}

fn content_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let max_len = a_chars.len().max(b_chars.len());
    let min_len = a_chars.len().min(b_chars.len());

    let mut matches = 0;
    for i in 0..min_len {
        if a_chars[i] == b_chars[i] {
            matches += 1;
        }
    }
    matches as f64 / max_len as f64
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

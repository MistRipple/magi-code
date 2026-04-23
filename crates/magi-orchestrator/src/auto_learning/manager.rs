use super::memory_consolidation::{MemoryConsolidationService, RawMemoryInput};
use super::preference_miner::{PreferenceMiner, PreferenceMiningResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLearningCaptureInput {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub final_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_reason: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_summary: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLearningRawMemory {
    pub id: String,
    pub source_key: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub created_at: u64,
    pub final_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_reason: Option<String>,
    pub summary: String,
    pub decisions: Vec<String>,
    pub learnings: Vec<String>,
    pub warnings: Vec<String>,
    pub citations: Vec<String>,
}

pub struct AutoLearningManager {
    preference_miner: PreferenceMiner,
    consolidation_service: MemoryConsolidationService,
    raw_memories: Vec<AutoLearningRawMemory>,
}

impl AutoLearningManager {
    pub fn new() -> Self {
        Self {
            preference_miner: PreferenceMiner::new(),
            consolidation_service: MemoryConsolidationService::new(None),
            raw_memories: Vec::new(),
        }
    }

    pub fn capture(
        &mut self,
        input: &AutoLearningCaptureInput,
        user_messages: &[&str],
        assistant_messages: &[&str],
        summary: &str,
        decisions: Vec<String>,
        learnings: Vec<String>,
        warnings: Vec<String>,
    ) -> AutoLearningRawMemory {
        let now = now_millis();
        let source_key = format!(
            "session:{}:turn:{}",
            input.session_id,
            input.turn_id.as_deref().unwrap_or("unknown")
        );
        let id = format!("alm-{now}-{}", self.raw_memories.len());

        let raw = AutoLearningRawMemory {
            id: id.clone(),
            source_key: source_key.clone(),
            session_id: input.session_id.clone(),
            mission_id: input.mission_id.clone(),
            request_id: input.request_id.clone(),
            turn_id: input.turn_id.clone(),
            created_at: now,
            final_status: input.final_status.clone(),
            runtime_reason: input.runtime_reason.clone(),
            summary: summary.to_string(),
            decisions: decisions.clone(),
            learnings: learnings.clone(),
            warnings: warnings.clone(),
            citations: Vec::new(),
        };

        for learning in &learnings {
            self.consolidation_service.add_entry(RawMemoryInput {
                content: learning.clone(),
                citations: vec![source_key.clone()],
                created_at: now,
                confidence: if input.final_status == "completed" {
                    0.9
                } else {
                    0.5
                },
            });
        }
        for decision in &decisions {
            self.consolidation_service.add_entry(RawMemoryInput {
                content: decision.clone(),
                citations: vec![source_key.clone()],
                created_at: now,
                confidence: 0.8,
            });
        }
        for warning in &warnings {
            self.consolidation_service.add_entry(RawMemoryInput {
                content: warning.clone(),
                citations: vec![source_key.clone()],
                created_at: now,
                confidence: 0.7,
            });
        }

        self.raw_memories.push(raw.clone());

        let _preference_result = self
            .preference_miner
            .mine_from_conversation(user_messages, assistant_messages);

        raw
    }

    pub fn try_consolidate(&mut self) -> Option<super::memory_consolidation::ConsolidationResult> {
        if !self.consolidation_service.should_consolidate() {
            return None;
        }
        Some(self.consolidation_service.consolidate())
    }

    pub fn mine_preferences(
        &self,
        user_messages: &[&str],
        assistant_messages: &[&str],
    ) -> PreferenceMiningResult {
        self.preference_miner
            .mine_from_conversation(user_messages, assistant_messages)
    }

    pub fn raw_memory_count(&self) -> usize {
        self.raw_memories.len()
    }

    pub fn pending_consolidation_count(&self) -> usize {
        self.consolidation_service.pending_count()
    }
}

impl Default for AutoLearningManager {
    fn default() -> Self {
        Self::new()
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

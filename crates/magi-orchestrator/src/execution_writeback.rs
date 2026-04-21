use magi_core::{RecoveryResumeInput, SessionId, UtcMillis};
use magi_memory_store::{ExtractedMemory, MemoryExtractionApplyRequest, MemoryLayer, MemoryStore};

#[derive(Clone, Debug, Default)]
pub struct ExecutionWritebackPlans {
    plans: Vec<ExecutionWritebackPlan>,
}

#[derive(Clone, Copy, Debug)]
pub struct DispatchMemoryExtractionInput<'a> {
    pub accepted_at: UtcMillis,
    pub session_id: &'a SessionId,
    pub timeline_entry_id: &'a str,
    pub text: Option<&'a str>,
    pub skill_name: Option<&'a str>,
    pub deep_task: bool,
}

#[derive(Clone, Debug)]
enum ExecutionWritebackPlan {
    MemoryExtraction(MemoryExtractionApplyRequest),
}

impl ExecutionWritebackPlans {
    pub fn from_session_action_input(input: DispatchMemoryExtractionInput<'_>) -> Self {
        let Some(text) = trimmed_non_empty(input.text) else {
            return Self::default();
        };

        let mut content = text.to_string();
        if let Some(skill_name) = trimmed_non_empty(input.skill_name) {
            content.push_str("\nskill:");
            content.push_str(skill_name);
        }
        if input.deep_task {
            content.push_str("\nmode:deep_task");
        }

        Self {
            plans: vec![ExecutionWritebackPlan::MemoryExtraction(
                MemoryExtractionApplyRequest {
                    extraction_id: format!("extract-session-action-{}", input.accepted_at.0),
                    session_id: input.session_id.clone(),
                    source_ref: Some(format!("timeline://{}", input.timeline_entry_id)),
                    summary: "session.action shadow extraction".to_string(),
                    memories: vec![ExtractedMemory {
                        memory_id: format!("mem-session-action-{}", input.accepted_at.0),
                        layer: MemoryLayer::Durable,
                        content,
                        created_at: input.accepted_at,
                    }],
                    created_at: input.accepted_at,
                },
            )],
        }
    }

    pub fn from_optional_memory_extraction(
        request: Option<MemoryExtractionApplyRequest>,
    ) -> Self {
        let plans = request
            .into_iter()
            .map(ExecutionWritebackPlan::MemoryExtraction)
            .collect();
        Self { plans }
    }

    pub fn from_recovery_resume_input(input: &RecoveryResumeInput) -> Self {
        let Some(session_id) = input.ownership.session_id.clone() else {
            return Self::default();
        };
        let Some(diagnostic_summary) = input
            .diagnostic_summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Self::default();
        };

        Self {
            plans: vec![ExecutionWritebackPlan::MemoryExtraction(
                MemoryExtractionApplyRequest {
                    extraction_id: format!("extract-recovery-{}", input.recovery_id),
                    session_id,
                    source_ref: Some(format!(
                        "recovery://{}/snapshot/{}",
                        input.recovery_id, input.snapshot_id
                    )),
                    summary: "recovery.resume diagnostic extraction".to_string(),
                    memories: vec![ExtractedMemory {
                        memory_id: format!("mem-recovery-{}", input.recovery_id),
                        layer: MemoryLayer::Durable,
                        content: diagnostic_summary.to_string(),
                        created_at: input.updated_at,
                    }],
                    created_at: input.updated_at,
                },
            )],
        }
    }

    pub fn apply(self, memory_store: &MemoryStore) {
        for plan in self.plans {
            plan.apply(memory_store);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

impl ExecutionWritebackPlan {
    fn apply(self, memory_store: &MemoryStore) {
        match self {
            ExecutionWritebackPlan::MemoryExtraction(request) => {
                memory_store.apply_extraction(request);
            }
        }
    }
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{DispatchMemoryExtractionInput, ExecutionWritebackPlans};
    use magi_core::{ExecutionOwnership, RecoveryResumeInput, SessionId, UtcMillis, WorkspaceId};
    use magi_memory_store::{ExtractedMemory, MemoryExtractionApplyRequest, MemoryLayer, MemoryStore};

    #[test]
    fn empty_writeback_plan_is_noop() {
        let store = MemoryStore::new();

        ExecutionWritebackPlans::default().apply(&store);

        assert!(store
            .extraction_results_for_session(&SessionId::new("session-1"))
            .is_empty());
    }

    #[test]
    fn memory_extraction_writeback_plan_applies_closed_loop_record() {
        let store = MemoryStore::new();
        let plans = ExecutionWritebackPlans::from_optional_memory_extraction(Some(
            MemoryExtractionApplyRequest {
                extraction_id: "extract-1".to_string(),
                session_id: SessionId::new("session-1"),
                source_ref: Some("timeline://entry-1".to_string()),
                summary: "shadow extraction".to_string(),
                memories: vec![ExtractedMemory {
                    memory_id: "mem-1".to_string(),
                    layer: MemoryLayer::Durable,
                    content: "hello world".to_string(),
                    created_at: UtcMillis(42),
                }],
                created_at: UtcMillis(42),
            },
        ));

        plans.apply(&store);

        let verification = store
            .verify_extraction_linkage("extract-1")
            .expect("writeback plan should persist extraction linkage");
        assert!(verification.is_consistent);
    }

    #[test]
    fn session_action_input_builds_closed_loop_memory_extraction_plan() {
        let store = MemoryStore::new();
        let session_id = SessionId::new("session-1");

        ExecutionWritebackPlans::from_session_action_input(DispatchMemoryExtractionInput {
            accepted_at: UtcMillis(42),
            session_id: &session_id,
            timeline_entry_id: "timeline-1",
            text: Some("  hello world  "),
            skill_name: Some("  refactor  "),
            deep_task: true,
        })
        .apply(&store);

        let verification = store
            .verify_extraction_linkage("extract-session-action-42")
            .expect("session action writeback should persist extraction linkage");
        assert!(verification.is_consistent);
        let linkage = store
            .extraction_linkage("extract-session-action-42")
            .expect("session action extraction linkage should exist");
        assert_eq!(
            linkage.extraction.source_ref.as_deref(),
            Some("timeline://timeline-1")
        );
        assert_eq!(linkage.extraction.summary, "session.action shadow extraction");
        assert_eq!(linkage.produced_records[0].memory_id, "mem-session-action-42");
        assert_eq!(
            linkage.produced_records[0].content,
            "hello world\nskill:refactor\nmode:deep_task"
        );
    }

    #[test]
    fn session_action_input_skips_blank_text_even_with_skill_and_depth_metadata() {
        let store = MemoryStore::new();
        let session_id = SessionId::new("session-blank");

        ExecutionWritebackPlans::from_session_action_input(DispatchMemoryExtractionInput {
            accepted_at: UtcMillis(7),
            session_id: &session_id,
            timeline_entry_id: "timeline-blank",
            text: Some("   "),
            skill_name: Some("refactor"),
            deep_task: true,
        })
        .apply(&store);

        assert!(store.extraction_linkage("extract-session-action-7").is_none());
    }

    #[test]
    fn recovery_resume_input_builds_recovery_memory_extraction_plan() {
        let store = MemoryStore::new();
        let plans = ExecutionWritebackPlans::from_recovery_resume_input(&RecoveryResumeInput {
            recovery_id: "recovery-1".to_string(),
            snapshot_id: "snapshot-1".to_string(),
            ownership: ExecutionOwnership {
                session_id: Some(SessionId::new("session-1")),
                workspace_id: Some(WorkspaceId::new("workspace-1")),
                ..ExecutionOwnership::default()
            },
            diagnostic_summary: Some("resume parser after crash".to_string()),
            created_at: UtcMillis(7),
            updated_at: UtcMillis(9),
        });

        plans.apply(&store);

        let verification = store
            .verify_extraction_linkage("extract-recovery-recovery-1")
            .expect("recovery writeback plan should persist extraction linkage");
        assert!(verification.is_consistent);
        let linkage = store
            .extraction_linkage("extract-recovery-recovery-1")
            .expect("recovery extraction linkage should exist");
        assert_eq!(
            linkage.extraction.source_ref.as_deref(),
            Some("recovery://recovery-1/snapshot/snapshot-1")
        );
        assert_eq!(linkage.produced_records[0].content, "resume parser after crash");
    }

    #[test]
    fn recovery_resume_input_without_session_or_diagnostic_skips_writeback_plan() {
        let store = MemoryStore::new();
        ExecutionWritebackPlans::from_recovery_resume_input(&RecoveryResumeInput {
            recovery_id: "recovery-blank".to_string(),
            snapshot_id: "snapshot-blank".to_string(),
            ownership: ExecutionOwnership::default(),
            diagnostic_summary: Some("   ".to_string()),
            created_at: UtcMillis(1),
            updated_at: UtcMillis(2),
        })
        .apply(&store);

        assert!(store.extraction_linkage("extract-recovery-recovery-blank").is_none());
    }
}

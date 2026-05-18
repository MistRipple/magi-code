use super::*;

use magi_checkpoint::{
    Checkpoint, CheckpointKind, CheckpointLog, CheckpointStore, ConversationCheckpoint,
};
use magi_core::{MissionId, SessionId, UtcMillis, WorkspaceRootPath};
use magi_human_checkpoint::{
    HumanCheckpoint, HumanCheckpointLog, HumanCheckpointStatus, HumanCheckpointStore,
};
use magi_knowledge_graph::KnowledgeGraphStore;
use magi_mission_charter::{CharterState, MissionCharter, MissionCharterStore};
use magi_plan::{Plan, PlanStep, PlanStepStatus, PlanStore};
use magi_validation_runner::ValidationStore;
use tempfile::TempDir;

struct Harness {
    _tmp: TempDir,
    home: PathBuf,
    workspace: WorkspaceRootPath,
}

impl Harness {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tmp");
        let home = tmp.path().to_path_buf();
        Self {
            _tmp: tmp,
            home,
            workspace: WorkspaceRootPath::from("/Users/test/proj"),
        }
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn workspace(&self) -> &WorkspaceRootPath {
        &self.workspace
    }

    fn write_charter(&self, mission_id: &MissionId) {
        let store = MissionCharterStore::open_with_home(self.home(), self.workspace()).unwrap();
        let charter =
            MissionCharter::new(mission_id.clone(), "demo", "deliver test", UtcMillis::now());
        store.save(&charter).unwrap();
    }

    fn write_plan(&self, mission_id: &MissionId) {
        let store = PlanStore::open_with_home(self.home(), self.workspace()).unwrap();
        let mut plan = Plan::new(mission_id.clone(), UtcMillis::now());
        plan.steps.push(PlanStep {
            id: "s1".to_string(),
            content: "step 1".to_string(),
            status: PlanStepStatus::Pending,
            depends_on: vec![],
            notes: None,
        });
        store.save(&plan).unwrap();
    }

    fn write_checkpoint_log(&self, mission_id: &MissionId, checkpoints: Vec<Checkpoint>) {
        let store = CheckpointStore::open_with_home(self.home(), self.workspace()).unwrap();
        let now = UtcMillis::now();
        let log = CheckpointLog {
            mission_id: mission_id.clone(),
            checkpoints,
            created_at: now,
            updated_at: now,
        };
        store.save(&log).unwrap();
    }

    fn make_complete_checkpoint(&self, mission_id: &MissionId) -> Checkpoint {
        Checkpoint {
            sequence: 1,
            mission_id: mission_id.clone(),
            kind: CheckpointKind::ProcessRestart,
            created_at: UtcMillis::now(),
            label: None,
            plan_version: None,
            kg_fact_count: None,
            workspace_commit: Some("abc123".to_string()),
            open_conversations: vec![ConversationCheckpoint {
                session_id: SessionId::new("S-1"),
                recovery_ref: Some("recovery/abc".to_string()),
                execution_chain_ref: Some("chain/xyz".to_string()),
                turn_cursor: None,
                pending_mailbox: 0,
            }],
            notes: None,
        }
    }
}

#[test]
fn resume_mission_returns_aggregate_when_recovery_set_complete() {
    let h = Harness::new();
    let mid = MissionId::new("M-1");
    h.write_charter(&mid);
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();
    assert_eq!(agg.mission_id(), &mid);
    assert_eq!(agg.charter_head().mission_id, mid);
    assert_eq!(agg.plan_head().mission_id, mid);
    assert!(agg.head_checkpoint().recovery_set_status().is_ok());
}

#[test]
fn resume_mission_rejects_when_charter_missing() {
    let h = Harness::new();
    let mid = MissionId::new("M-2");
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let err = resume_mission(&mid, h.workspace(), h.home()).unwrap_err();
    assert!(matches!(
        err,
        MissionResumeError::CharterMissing { ref mission_id } if mission_id == &mid
    ));
}

#[test]
fn resume_mission_rejects_when_plan_missing() {
    let h = Harness::new();
    let mid = MissionId::new("M-3");
    h.write_charter(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let err = resume_mission(&mid, h.workspace(), h.home()).unwrap_err();
    assert!(matches!(
        err,
        MissionResumeError::PlanMissing { ref mission_id } if mission_id == &mid
    ));
}

#[test]
fn resume_mission_rejects_when_checkpoint_log_missing() {
    let h = Harness::new();
    let mid = MissionId::new("M-4");
    h.write_charter(&mid);
    h.write_plan(&mid);

    let err = resume_mission(&mid, h.workspace(), h.home()).unwrap_err();
    assert!(matches!(
        err,
        MissionResumeError::CheckpointLogMissing { ref mission_id } if mission_id == &mid
    ));
}

#[test]
fn resume_mission_rejects_when_latest_checkpoint_missing_workspace_commit() {
    let h = Harness::new();
    let mid = MissionId::new("M-5");
    h.write_charter(&mid);
    h.write_plan(&mid);
    let mut bad = h.make_complete_checkpoint(&mid);
    bad.workspace_commit = None;
    h.write_checkpoint_log(&mid, vec![bad]);

    let err = resume_mission(&mid, h.workspace(), h.home()).unwrap_err();
    assert!(matches!(
        err,
        MissionResumeError::LatestCheckpointIncomplete {
            ref mission_id,
            reason: MissingRecoverySetReason::WorkspaceCommitMissing,
        } if mission_id == &mid
    ));
}

#[test]
fn resume_mission_rejects_when_latest_checkpoint_conversation_missing_pointer() {
    let h = Harness::new();
    let mid = MissionId::new("M-6");
    h.write_charter(&mid);
    h.write_plan(&mid);
    let mut bad = h.make_complete_checkpoint(&mid);
    bad.open_conversations[0].recovery_ref = None;
    bad.open_conversations[0].execution_chain_ref = None;
    h.write_checkpoint_log(&mid, vec![bad]);

    let err = resume_mission(&mid, h.workspace(), h.home()).unwrap_err();
    match err {
        MissionResumeError::LatestCheckpointIncomplete {
            mission_id,
            reason: MissingRecoverySetReason::ConversationPointerMissing { session_id },
        } => {
            assert_eq!(mission_id, mid);
            assert_eq!(session_id, "S-1");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn aggregate_knowledge_returns_none_when_kg_absent() {
    let h = Harness::new();
    let mid = MissionId::new("M-7");
    h.write_charter(&mid);
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();
    // 验证 KG store 已 open 但 mission 没有 KG 记录
    assert!(KnowledgeGraphStore::open_with_home(h.home(), h.workspace()).is_ok());
    assert!(agg.knowledge().unwrap().is_none());
}

#[test]
fn aggregate_validation_returns_none_when_validation_absent() {
    let h = Harness::new();
    let mid = MissionId::new("M-8");
    h.write_charter(&mid);
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();
    assert!(ValidationStore::open_with_home(h.home(), h.workspace()).is_ok());
    assert!(agg.validation().unwrap().is_none());
}

#[test]
fn aggregate_human_checkpoint_returns_none_when_log_absent() {
    let h = Harness::new();
    let mid = MissionId::new("M-9");
    h.write_charter(&mid);
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);

    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();
    assert!(HumanCheckpointStore::open_with_home(h.home(), h.workspace()).is_ok());
    assert!(agg.human_checkpoint_log().unwrap().is_none());
}

#[test]
fn enumerate_resumable_missions_skips_empty_mission_dirs() {
    let h = Harness::new();
    let with_charter = MissionId::new("M-with");
    let empty = MissionId::new("M-empty");
    h.write_charter(&with_charter);
    h.write_plan(&with_charter);
    h.write_checkpoint_log(
        &with_charter,
        vec![h.make_complete_checkpoint(&with_charter)],
    );

    // 显式建空目录（没有 charter.md），模拟"已废弃的 mission"
    let empty_dir = magi_core::paths::mission_dir(h.home(), h.workspace(), &empty);
    std::fs::create_dir_all(&empty_dir).unwrap();

    let ids = enumerate_resumable_missions(h.workspace(), h.home()).unwrap();
    assert_eq!(ids, vec![with_charter]);
}

#[test]
fn enumerate_resumable_missions_lists_all_charter_bearing_missions_sorted() {
    let h = Harness::new();
    let ids = ["M-c", "M-a", "M-b"].map(MissionId::new);
    for id in &ids {
        h.write_charter(id);
        h.write_plan(id);
        h.write_checkpoint_log(id, vec![h.make_complete_checkpoint(id)]);
    }

    let listed = enumerate_resumable_missions(h.workspace(), h.home()).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>(),
        vec!["M-a", "M-b", "M-c"]
    );
}

#[test]
fn enumerate_resumable_missions_returns_empty_when_root_absent() {
    let h = Harness::new();
    // 不写任何 mission，missions root 目录不会被创建
    let ids = enumerate_resumable_missions(h.workspace(), h.home()).unwrap();
    assert!(ids.is_empty());
}

// ---------------------------------------------------------------------------
// lifecycle_phase 派生
// ---------------------------------------------------------------------------

impl Harness {
    fn write_charter_frozen(&self, mission_id: &MissionId) {
        let store = MissionCharterStore::open_with_home(self.home(), self.workspace()).unwrap();
        let mut charter =
            MissionCharter::new(mission_id.clone(), "demo", "deliver test", UtcMillis::now());
        charter.success_criteria = vec!["criterion".to_string()];
        charter.state = CharterState::Frozen;
        store.save(&charter).unwrap();
    }

    fn write_plan_with_steps(&self, mission_id: &MissionId, statuses: &[PlanStepStatus]) {
        let store = PlanStore::open_with_home(self.home(), self.workspace()).unwrap();
        let mut plan = Plan::new(mission_id.clone(), UtcMillis::now());
        for (idx, status) in statuses.iter().enumerate() {
            plan.steps.push(PlanStep {
                id: format!("s{}", idx + 1),
                content: format!("step {}", idx + 1),
                status: *status,
                depends_on: vec![],
                notes: None,
            });
        }
        store.save(&plan).unwrap();
    }

    fn write_empty_plan(&self, mission_id: &MissionId) {
        let store = PlanStore::open_with_home(self.home(), self.workspace()).unwrap();
        let plan = Plan::new(mission_id.clone(), UtcMillis::now());
        store.save(&plan).unwrap();
    }

    fn write_pending_human_checkpoint(&self, mission_id: &MissionId) {
        let store = HumanCheckpointStore::open_with_home(self.home(), self.workspace()).unwrap();
        let now = UtcMillis::now();
        let mut log = HumanCheckpointLog::new(mission_id.clone(), now);
        log.entries.push(HumanCheckpoint {
            sequence: 1,
            mission_id: mission_id.clone(),
            status: HumanCheckpointStatus::Pending,
            created_at: now,
            plan_step_id: "s1".to_string(),
            prompt_to_human: "need approval".to_string(),
            label: None,
            context: None,
            decision: None,
        });
        store.save(&log).unwrap();
    }
}

#[test]
fn lifecycle_phase_returns_charter_draft_when_charter_not_frozen() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-draft");
    h.write_charter(&mid); // 默认 Draft
    h.write_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::CharterDraft
    );
}

#[test]
fn lifecycle_phase_returns_awaiting_human_checkpoint_even_when_executing() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-hc");
    h.write_charter_frozen(&mid);
    // plan 已 InProgress——人审优先级更高
    h.write_plan_with_steps(&mid, &[PlanStepStatus::InProgress]);
    h.write_pending_human_checkpoint(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::AwaitingHumanCheckpoint
    );
}

#[test]
fn lifecycle_phase_returns_plan_ready_when_all_steps_pending() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-ready");
    h.write_charter_frozen(&mid);
    h.write_plan_with_steps(&mid, &[PlanStepStatus::Pending, PlanStepStatus::Pending]);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::PlanReady
    );
}

#[test]
fn lifecycle_phase_returns_plan_ready_when_steps_empty() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-empty");
    h.write_charter_frozen(&mid);
    h.write_empty_plan(&mid);
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::PlanReady
    );
}

#[test]
fn lifecycle_phase_returns_executing_when_any_step_in_progress() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-exec");
    h.write_charter_frozen(&mid);
    h.write_plan_with_steps(
        &mid,
        &[
            PlanStepStatus::Completed,
            PlanStepStatus::InProgress,
            PlanStepStatus::Pending,
        ],
    );
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::Executing
    );
}

#[test]
fn lifecycle_phase_returns_all_steps_completed_when_all_done_or_cancelled() {
    let h = Harness::new();
    let mid = MissionId::new("M-phase-done");
    h.write_charter_frozen(&mid);
    h.write_plan_with_steps(
        &mid,
        &[
            PlanStepStatus::Completed,
            PlanStepStatus::Cancelled,
            PlanStepStatus::Completed,
        ],
    );
    h.write_checkpoint_log(&mid, vec![h.make_complete_checkpoint(&mid)]);
    let agg = resume_mission(&mid, h.workspace(), h.home()).unwrap();

    assert_eq!(
        agg.lifecycle_phase().unwrap(),
        MissionLifecyclePhase::AllStepsCompleted
    );
}

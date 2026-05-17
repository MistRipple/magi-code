//! 集成测试：MissionAggregate 读端必须与 `append_checkpoint` 写端契约对称。
//!
//! 单元测试通过 `store.save()` 直接落盘，绕过了写端的 `recovery_set_status` 闸门。
//! 这条集成测试故意走真实写端入口 `append_checkpoint`：用它把恢复 kind 的
//! Checkpoint 落盘，然后用 `resume_mission` 读回来，确认：
//!
//! 1. 写端拒绝缺料（与单元测试不重复，这里只测拒绝路径返回的 reason）；
//! 2. 写端接受完整恢复集后，读端能恢复出 head Checkpoint；
//! 3. head Checkpoint 的关键 recovery 指针（workspace_commit / recovery_ref /
//!    execution_chain_ref）round-trip 无损。
//!
//! 这样就把"写端拒绝缺料"和"读端拒绝缺料"绑成同一条契约：写端落得下的，读端
//! 一定能 resume；写端拒绝的，读端永远不可能见到。**§1.4 写端 / 读端单源契约**
//! 的最小可执行证明。

use magi_checkpoint::{
    CheckpointCreateArgs, CheckpointError, CheckpointKind, CheckpointLog, CheckpointStore,
    ConversationCheckpoint, MissingRecoverySetReason, append_checkpoint,
};
use magi_core::{MissionId, SessionId, UtcMillis, WorkspaceRootPath};
use magi_mission::{MissionResumeError, resume_mission};
use magi_mission_charter::{MissionCharter, MissionCharterStore};
use magi_plan::{Plan, PlanStep, PlanStepStatus, PlanStore};
use tempfile::TempDir;

fn seed_charter_and_plan(home: &std::path::Path, workspace: &WorkspaceRootPath, mid: &MissionId) {
    let charter_store = MissionCharterStore::open_with_home(home, workspace).unwrap();
    charter_store
        .save(&MissionCharter::new(
            mid.clone(),
            "demo",
            "ship it",
            UtcMillis::now(),
        ))
        .unwrap();

    let plan_store = PlanStore::open_with_home(home, workspace).unwrap();
    let mut plan = Plan::new(mid.clone(), UtcMillis::now());
    plan.steps.push(PlanStep {
        id: "s1".to_string(),
        content: "step".to_string(),
        status: PlanStepStatus::Pending,
        depends_on: vec![],
        notes: None,
    });
    plan_store.save(&plan).unwrap();
}

fn complete_args() -> CheckpointCreateArgs {
    CheckpointCreateArgs {
        kind: CheckpointKind::ProcessRestart,
        label: Some("after restart".to_string()),
        plan_version: None,
        kg_fact_count: None,
        workspace_commit: Some("commit-abc".to_string()),
        open_conversations: vec![ConversationCheckpoint {
            session_id: SessionId::new("S-1"),
            recovery_ref: Some("recovery/rrr".to_string()),
            execution_chain_ref: Some("chain/ccc".to_string()),
            turn_cursor: Some(7),
            pending_mailbox: 0,
        }],
        notes: None,
    }
}

#[test]
fn append_checkpoint_then_resume_round_trip_recovery_pointers() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let workspace = WorkspaceRootPath::from("/Users/test/proj");
    let mid = MissionId::new("M-rt");

    seed_charter_and_plan(home, &workspace, &mid);

    // 走真实写端：append_checkpoint -> store.save
    let store = CheckpointStore::open_with_home(home, &workspace).unwrap();
    let mut log = CheckpointLog::new(mid.clone(), UtcMillis::now());
    let seq = append_checkpoint(&mut log, complete_args(), UtcMillis::now())
        .expect("write-side must accept complete recovery set");
    assert_eq!(seq, 1);
    store.save(&log).unwrap();

    // 走真实读端：resume_mission
    let agg = resume_mission(&mid, &workspace, home).expect("read-side must resume");
    let head = agg.head_checkpoint();
    assert_eq!(head.sequence, 1);
    assert_eq!(head.workspace_commit.as_deref(), Some("commit-abc"));
    assert_eq!(head.open_conversations.len(), 1);
    let conv = &head.open_conversations[0];
    assert_eq!(conv.session_id.as_str(), "S-1");
    assert_eq!(conv.recovery_ref.as_deref(), Some("recovery/rrr"));
    assert_eq!(conv.execution_chain_ref.as_deref(), Some("chain/ccc"));
    assert_eq!(conv.turn_cursor, Some(7));
    assert!(head.recovery_set_status().is_ok());
}

#[test]
fn append_checkpoint_rejects_missing_workspace_commit_symmetric_to_read_side() {
    let mut log = CheckpointLog::new(MissionId::new("M-w"), UtcMillis::now());
    let mut args = complete_args();
    args.workspace_commit = None;
    let err = append_checkpoint(&mut log, args, UtcMillis::now()).unwrap_err();
    match err {
        CheckpointError::IncompleteRecoverySet {
            reason: MissingRecoverySetReason::WorkspaceCommitMissing,
            ..
        } => {}
        other => panic!("unexpected: {other:?}"),
    }
    // Mirror：单元测试已覆盖读端在同样缺料下返回 LatestCheckpointIncomplete。
    // 写端拒绝 + 读端拒绝 = §1.4 闭环。
}

#[test]
fn append_checkpoint_rejects_missing_conversation_pointer_symmetric_to_read_side() {
    let mut log = CheckpointLog::new(MissionId::new("M-c"), UtcMillis::now());
    let mut args = complete_args();
    args.open_conversations[0].recovery_ref = None;
    args.open_conversations[0].execution_chain_ref = None;
    let err = append_checkpoint(&mut log, args, UtcMillis::now()).unwrap_err();
    match err {
        CheckpointError::IncompleteRecoverySet {
            reason: MissingRecoverySetReason::ConversationPointerMissing { session_id },
            ..
        } => {
            assert_eq!(session_id, "S-1");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn resume_mission_propagates_store_parse_error_via_store_kind() {
    // 故意写入非法 charter.md 文件，确认 resume_mission 通过 StoreError 透传，
    // 不会被吞为 *Missing。这是保证 diagnostic granularity 的回归测试。
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let workspace = WorkspaceRootPath::from("/Users/test/proj");
    let mid = MissionId::new("M-bad");

    let mission_dir = magi_core::paths::mission_dir(home, &workspace, &mid);
    std::fs::create_dir_all(&mission_dir).unwrap();
    std::fs::write(mission_dir.join("charter.md"), "not a valid charter").unwrap();

    let err = resume_mission(&mid, &workspace, home).unwrap_err();
    match err {
        MissionResumeError::StoreError { which, .. } => {
            assert_eq!(which.to_string(), "charter");
        }
        other => panic!("expected StoreError(Charter), got: {other:?}"),
    }
}

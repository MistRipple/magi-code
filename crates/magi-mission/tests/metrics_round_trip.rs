//! 集成测试:`MissionAggregate::metrics()` 读端必须与 `MissionMetricsStore::record_turn`
//! 写端契约对称。
//!
//! 单元测试已分别覆盖 store 写端累加(`record_turn_creates_and_accumulates`)与
//! aggregate 读端字段透传(`MissionAggregate::metrics` 仅是 store.load 转发)。
//! 本测试把两者串成一条:走真实 `record_turn` 写两轮,然后通过 `resume_mission`
//! 拿到 aggregate,再调 `metrics()` 读回,**证明读写打通**——防御 metrics crate
//! 写完没人调用、变成第二个 `CheckpointLog::latest()` 孤儿。
//!
//! 反孤儿担保:`magi-mission` 是 metrics 写端的唯一聚合读端,本测试保证 store →
//! aggregate 这条路径没有静默断裂。

use magi_checkpoint::{
    CheckpointCreateArgs, CheckpointKind, CheckpointLog, CheckpointStore, ConversationCheckpoint,
    append_checkpoint,
};
use magi_core::{MissionId, MissionLifecyclePhase, SessionId, UtcMillis, WorkspaceRootPath};
use magi_mission::resume_mission;
use magi_mission_charter::{MissionCharter, MissionCharterStore};
use magi_mission_metrics::{MissionMetricsStore, TurnUsage};
use magi_plan::{Plan, PlanStep, PlanStepStatus, PlanStore};
use tempfile::TempDir;

fn seed_recoverable_mission(
    home: &std::path::Path,
    workspace: &WorkspaceRootPath,
    mid: &MissionId,
) {
    let charter_store = MissionCharterStore::open_with_home(home, workspace).unwrap();
    charter_store
        .save(&MissionCharter::new(
            mid.clone(),
            "demo",
            "metrics round-trip mission",
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

    // 写一个完整恢复集 checkpoint,让 resume_mission 通过 head 校验。
    let store = CheckpointStore::open_with_home(home, workspace).unwrap();
    let mut log = CheckpointLog::new(mid.clone(), UtcMillis::now());
    append_checkpoint(
        &mut log,
        CheckpointCreateArgs {
            kind: CheckpointKind::ProcessRestart,
            label: Some("seed".to_string()),
            plan_version: None,
            kg_fact_count: None,
            workspace_commit: Some("commit-seed".to_string()),
            open_conversations: vec![ConversationCheckpoint {
                session_id: SessionId::new("S-metrics"),
                recovery_ref: Some("recovery/seed".to_string()),
                execution_chain_ref: Some("chain/seed".to_string()),
                turn_cursor: Some(0),
                pending_mailbox: 0,
            }],
            notes: None,
        },
        UtcMillis::now(),
    )
    .expect("seed checkpoint must accept complete recovery set");
    store.save(&log).unwrap();
}

#[test]
fn record_turn_twice_then_aggregate_metrics_returns_accumulated() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let workspace = WorkspaceRootPath::from("/Users/test/proj");
    let mid = MissionId::new("M-metrics");

    seed_recoverable_mission(home, &workspace, &mid);

    // 写端:走真实 `record_turn` 两轮。
    let store = MissionMetricsStore::open_with_home(home, &workspace).unwrap();
    store
        .record_turn(
            &mid,
            TurnUsage {
                prompt_tokens: 100,
                completion_tokens: 30,
                started_at: UtcMillis(1_000),
                finished_at: UtcMillis(1_500),
                phase: Some(MissionLifecyclePhase::Executing),
            },
        )
        .expect("first record_turn must accept");
    store
        .record_turn(
            &mid,
            TurnUsage {
                prompt_tokens: 60,
                completion_tokens: 40,
                started_at: UtcMillis(2_000),
                finished_at: UtcMillis(2_700),
                phase: Some(MissionLifecyclePhase::AllStepsCompleted),
            },
        )
        .expect("second record_turn must accept");

    // 读端:走真实 `resume_mission` → `aggregate.metrics()`。
    let aggregate = resume_mission(&mid, &workspace, home).expect("resume must succeed");
    let metrics = aggregate
        .metrics()
        .expect("metrics() must succeed")
        .expect("metrics file must exist after two turns");

    assert_eq!(metrics.mission_id, mid);
    assert_eq!(metrics.turn_count, 2);
    assert_eq!(metrics.total_prompt_tokens, 160);
    assert_eq!(metrics.total_completion_tokens, 70);
    assert_eq!(metrics.total_tokens, 230);
    assert_eq!(metrics.first_turn_started_at, Some(UtcMillis(1_000)));
    assert_eq!(metrics.last_turn_finished_at, Some(UtcMillis(2_700)));
    // 第一轮 500ms + 第二轮 700ms。
    assert_eq!(metrics.wall_clock_millis, 1_200);
    assert_eq!(
        metrics.last_lifecycle_phase,
        Some(MissionLifecyclePhase::AllStepsCompleted)
    );
}

#[test]
fn aggregate_metrics_returns_none_when_no_turn_recorded() {
    // 守护:从未调用 record_turn 时,aggregate.metrics() 应返回 Ok(None);
    // 防御性回归,防止后续误把"缺 metrics 文件"当成错误抛出。
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let workspace = WorkspaceRootPath::from("/Users/test/proj");
    let mid = MissionId::new("M-metrics-empty");

    seed_recoverable_mission(home, &workspace, &mid);

    let aggregate = resume_mission(&mid, &workspace, home).expect("resume must succeed");
    let metrics = aggregate.metrics().expect("metrics() must succeed");
    assert!(
        metrics.is_none(),
        "无任何 turn 记录时 metrics() 应返回 None,实际:{metrics:?}"
    );
}

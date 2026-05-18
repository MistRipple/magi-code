//! Task System v2 §1.4 Phase B：daemon bootstrap 阶段的 Mission 自动恢复入口。
//!
//! Phase A 已经在 `magi-mission` 落地读端聚合契约（`resume_mission` /
//! `enumerate_resumable_missions`），但写完后没有调用方——重蹈 `CheckpointLog::latest()`
//! 孤儿覆辙的风险。Phase B 把这条链路接进 daemon bootstrap：
//!
//! 1. 扫描每个 workspace 下 `<magi_home>/projects/<slug>/missions/` 的可恢复 mission；
//! 2. 对每个 mission 调用 `resume_mission` 触发 head Checkpoint 的恢复集校验；
//! 3. 把 `head_checkpoint.open_conversations` 里的 recovery_ref 真正回灌到对应
//!    SessionRuntimeSidecar，让下一次进入 conversation 能走 `apply_chain_recovery_if_needed`；
//! 4. 发布 `mission.resumed.from_recovery` 事件，使 UI / read model 能感知恢复进度。
//!
//! 容错原则（贴合 cn-engineering-standard 的"从源头修复，不绕过约束"）：
//! - workspace 没有 missions 目录：合法（fresh project）→ 静默跳过；
//! - mission 缺 charter/plan/checkpoint：`resume_mission` 已经拒绝，这里 warn + 跳过；
//! - sidecar 不存在：会话还没真正 bootstrap 出 runtime_sidecar，这种 mission 来自
//!   "数据持久化但 session 已被清理"的边缘场景 → warn + 跳过，**不**伪造 sidecar。
//!   下次该 session 真正被加载时仍能从 mission 数据恢复。
//!
//! 不在本期范围：
//! - 把 `apply_chain_recovery_if_needed` 同步搬到 bootstrap 阶段：那需要在 daemon 启动早期
//!   构造完整 conversation runtime，与现有"启动阶段轻量、运行期再恢复"的设计冲突。
//!   Phase B 只把 recovery_ref **写回**，让运行期下一次 conversation turn 自然消费。
//!
//! 简单 / 中等任务路径不依赖本模块（它们不创建 Mission），所以本模块对 §3.1/§3.2/§3.3
//! 验收完全透明；只服务于 §3.4 复杂 Mission 路径。

use std::path::Path;
use std::sync::Arc;

use magi_core::{EventId, MissionId, SessionId, WorkspaceRootPath};
use magi_event_bus::{EventContext, EventEnvelope, InMemoryEventBus};
use magi_mission::{enumerate_resumable_missions, resume_mission};
use magi_session_store::SessionStore;
use magi_workspace::WorkspaceStore;
use tracing::warn;

/// daemon bootstrap 阶段执行的 Mission 恢复扫描。
///
/// 调用语义：完全幂等 + 容错。任何单个 mission / conversation 失败都不会让整个
/// bootstrap 失败——其它 workspace 与 mission 的恢复必须继续。
pub(super) fn recover_missions_at_bootstrap(
    magi_home: &Path,
    session_store: &SessionStore,
    workspace_store: &WorkspaceStore,
    event_bus: &Arc<InMemoryEventBus>,
) {
    for workspace in workspace_store.workspaces() {
        let workspace_root = WorkspaceRootPath::from(workspace.root_path.as_str());
        let mission_ids = match enumerate_resumable_missions(&workspace_root, magi_home) {
            Ok(ids) => ids,
            Err(err) => {
                warn!(
                    workspace_id = %workspace.workspace_id,
                    error = %err,
                    "枚举可恢复 mission 失败，跳过该 workspace 的 mission 自动恢复",
                );
                continue;
            }
        };

        for mission_id in mission_ids {
            recover_single_mission(
                magi_home,
                &workspace_root,
                &workspace.workspace_id,
                &mission_id,
                session_store,
                event_bus,
            );
        }
    }
}

fn recover_single_mission(
    magi_home: &Path,
    workspace_root: &WorkspaceRootPath,
    workspace_id: &magi_core::WorkspaceId,
    mission_id: &MissionId,
    session_store: &SessionStore,
    event_bus: &Arc<InMemoryEventBus>,
) {
    let aggregate = match resume_mission(mission_id, workspace_root, magi_home) {
        Ok(agg) => agg,
        Err(err) => {
            // Charter/Plan/Checkpoint 缺失或 recovery set 不完整都走这里。
            // 这是写端契约保护下的预期失败路径：mission 数据不完整就不应该被恢复。
            warn!(
                workspace_id = %workspace_id,
                mission_id = %mission_id,
                error = %err,
                "mission 恢复集校验失败，跳过自动恢复",
            );
            return;
        }
    };

    let head = aggregate.head_checkpoint();
    if head.open_conversations.is_empty() {
        // 该 checkpoint 不带任何 conversation 指针（例如纯快照型 checkpoint）。
        // 写端契约允许这种情况（只要 workspace_commit 存在），这里也不需要发事件。
        return;
    }

    for conversation in &head.open_conversations {
        // 优先 execution_chain_ref，否则 recovery_ref——两者都是 Checkpoint 写端
        // 接受的恢复指针（writer 已经保证至少有一个）。
        let recovery_ref = conversation
            .recovery_ref
            .clone()
            .or_else(|| conversation.execution_chain_ref.clone());
        let Some(recovery_ref) = recovery_ref else {
            // 不应到达：写端 `IncompleteRecoverySet::ConversationPointerMissing` 已拒绝。
            warn!(
                workspace_id = %workspace_id,
                mission_id = %mission_id,
                session_id = %conversation.session_id,
                "open conversation 缺 recovery_ref 与 execution_chain_ref，写端契约异常",
            );
            continue;
        };

        write_back_recovery_ref(
            session_store,
            &conversation.session_id,
            recovery_ref.clone(),
            workspace_id,
            mission_id,
        );

        publish_mission_resumed_event(
            event_bus,
            workspace_id,
            mission_id,
            &conversation.session_id,
            &recovery_ref,
            conversation.execution_chain_ref.as_deref(),
            head.sequence,
            head.workspace_commit.as_deref(),
        );
    }
}

/// 把 recovery_ref 写回对应 session 的 runtime_sidecar。
///
/// `attach_recovery_ref` 要求 runtime_sidecar 已存在；不存在时返回 `NotFound`。
/// 在 bootstrap 场景下"sidecar 缺失"是合法状态——例如 mission 数据来自其它环境
/// 拷贝，或对应 session 已被显式清理。这种情况只 warn，不伪造 sidecar：
/// **不要为了避免错误而绕过约束**（cn-engineering-standard：禁止补丁式兼容）。
fn write_back_recovery_ref(
    session_store: &SessionStore,
    session_id: &SessionId,
    recovery_ref: String,
    workspace_id: &magi_core::WorkspaceId,
    mission_id: &MissionId,
) {
    if session_store.runtime_sidecar(session_id).is_none() {
        warn!(
            workspace_id = %workspace_id,
            mission_id = %mission_id,
            session_id = %session_id,
            "session runtime_sidecar 不存在，跳过 recovery_ref 回灌；session 下次真正加载时再恢复",
        );
        return;
    }

    if let Err(err) = session_store.attach_recovery_ref(session_id, Some(recovery_ref)) {
        warn!(
            workspace_id = %workspace_id,
            mission_id = %mission_id,
            session_id = %session_id,
            error = %err,
            "回灌 recovery_ref 失败",
        );
    }
}

fn publish_mission_resumed_event(
    event_bus: &Arc<InMemoryEventBus>,
    workspace_id: &magi_core::WorkspaceId,
    mission_id: &MissionId,
    session_id: &SessionId,
    recovery_id: &str,
    execution_chain_ref: Option<&str>,
    checkpoint_sequence: u32,
    workspace_commit: Option<&str>,
) {
    let event_id = EventId::new(format!(
        "mission-resumed-{mission_id}-{session_id}-{checkpoint_sequence}"
    ));
    let payload = serde_json::json!({
        "recovery_id": recovery_id,
        "execution_chain_ref": execution_chain_ref,
        "checkpoint_sequence": checkpoint_sequence,
        "workspace_commit": workspace_commit,
        "source": "daemon_bootstrap",
    });
    let envelope = EventEnvelope::domain(
        event_id,
        magi_event_bus::task_events::MISSION_RESUMED_FROM_RECOVERY,
        payload,
    )
    .with_context(EventContext {
        workspace_id: Some(workspace_id.clone()),
        session_id: Some(session_id.clone()),
        mission_id: Some(mission_id.clone()),
        assignment_id: None,
        task_id: None,
    });
    if let Err(err) = event_bus.publish(envelope) {
        warn!(
            workspace_id = %workspace_id,
            mission_id = %mission_id,
            session_id = %session_id,
            error = %err,
            "mission.resumed.from_recovery 事件发布失败",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use magi_checkpoint::{
        CheckpointCreateArgs, CheckpointKind, CheckpointLog, CheckpointStore,
        ConversationCheckpoint, append_checkpoint,
    };
    use magi_core::{AbsolutePath, ExecutionOwnership, SessionId, UtcMillis, WorkspaceId};
    use magi_mission_charter::{MissionCharter, MissionCharterStore};
    use magi_plan::{Plan, PlanStep, PlanStepStatus, PlanStore};
    use tempfile::TempDir;

    fn seed_mission(home: &Path, workspace: &WorkspaceRootPath, mid: &MissionId, session_id: &str) {
        let charter_store = MissionCharterStore::open_with_home(home, workspace).unwrap();
        charter_store
            .save(&MissionCharter::new(
                mid.clone(),
                "test mission",
                "ship the mission recovery fixture",
                UtcMillis::now(),
            ))
            .unwrap();

        let plan_store = PlanStore::open_with_home(home, workspace).unwrap();
        let mut plan = Plan::new(mid.clone(), UtcMillis::now());
        plan.steps.push(PlanStep {
            id: "s1".to_string(),
            content: "step1".to_string(),
            status: PlanStepStatus::Pending,
            depends_on: vec![],
            notes: None,
        });
        plan_store.save(&plan).unwrap();

        let cp_store = CheckpointStore::open_with_home(home, workspace).unwrap();
        let mut log = CheckpointLog::new(mid.clone(), UtcMillis::now());
        let args = CheckpointCreateArgs {
            kind: CheckpointKind::ProcessRestart,
            label: Some("bootstrap restart".to_string()),
            plan_version: None,
            kg_fact_count: None,
            workspace_commit: Some("commit-xyz".to_string()),
            open_conversations: vec![ConversationCheckpoint {
                session_id: SessionId::new(session_id),
                recovery_ref: Some("recovery/r1".to_string()),
                execution_chain_ref: Some("chain/c1".to_string()),
                turn_cursor: Some(3),
                pending_mailbox: 0,
            }],
            notes: None,
        };
        append_checkpoint(&mut log, args, UtcMillis::now()).unwrap();
        cp_store.save(&log).unwrap();
    }

    fn register_workspace(
        workspace_store: &WorkspaceStore,
        ws_id: &str,
        root: &str,
    ) -> WorkspaceId {
        let id = WorkspaceId::new(ws_id);
        workspace_store
            .register(id.clone(), AbsolutePath::new(root))
            .unwrap();
        id
    }

    fn seed_session_with_sidecar(session_store: &SessionStore, session_id: &str) {
        let sid = SessionId::new(session_id);
        session_store
            .create_session(sid.clone(), "test session")
            .unwrap();
        // bind_execution_ownership 会创建 runtime_sidecar
        session_store.bind_execution_ownership(
            sid,
            ExecutionOwnership {
                session_id: Some(SessionId::new(session_id)),
                ..ExecutionOwnership::default()
            },
        );
    }

    #[test]
    fn empty_home_publishes_no_events() {
        let tmp = TempDir::new().unwrap();
        let session_store = SessionStore::new();
        let workspace_store = WorkspaceStore::new();
        register_workspace(&workspace_store, "ws-1", "/Users/test/proj-empty");
        let event_bus = Arc::new(InMemoryEventBus::new(64));

        recover_missions_at_bootstrap(tmp.path(), &session_store, &workspace_store, &event_bus);

        let snapshot = event_bus.snapshot();
        let resumed: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.event_type == magi_event_bus::task_events::MISSION_RESUMED_FROM_RECOVERY)
            .collect();
        assert!(resumed.is_empty());
    }

    #[test]
    fn mission_with_existing_sidecar_publishes_event_and_attaches_recovery_ref() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let workspace_root_str = "/Users/test/proj-happy";
        let workspace_root = WorkspaceRootPath::from(workspace_root_str);
        let mid = MissionId::new("M-happy");
        let session_id_str = "S-happy";

        seed_mission(home, &workspace_root, &mid, session_id_str);

        let session_store = SessionStore::new();
        seed_session_with_sidecar(&session_store, session_id_str);

        let workspace_store = WorkspaceStore::new();
        let ws_id = register_workspace(&workspace_store, "ws-happy", workspace_root_str);
        let event_bus = Arc::new(InMemoryEventBus::new(64));

        recover_missions_at_bootstrap(home, &session_store, &workspace_store, &event_bus);

        // 1) recovery_ref 写回 sidecar
        let sid = SessionId::new(session_id_str);
        let sidecar = session_store.runtime_sidecar(&sid).expect("sidecar exists");
        assert_eq!(sidecar.recovery_id.as_deref(), Some("recovery/r1"));

        // 2) 发布 mission.resumed.from_recovery 事件，带上完整上下文
        let snapshot = event_bus.snapshot();
        let resumed: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.event_type == magi_event_bus::task_events::MISSION_RESUMED_FROM_RECOVERY)
            .collect();
        assert_eq!(resumed.len(), 1);
        let ev = resumed[0];
        assert_eq!(
            ev.workspace_id.as_ref().map(|w| w.as_str()),
            Some("ws-happy")
        );
        assert_eq!(ev.session_id.as_ref().map(|s| s.as_str()), Some("S-happy"));
        assert_eq!(ev.mission_id.as_ref().map(|m| m.as_str()), Some("M-happy"));
        assert_eq!(
            ev.payload.get("recovery_id").and_then(|v| v.as_str()),
            Some("recovery/r1")
        );
        assert_eq!(
            ev.payload
                .get("execution_chain_ref")
                .and_then(|v| v.as_str()),
            Some("chain/c1")
        );
        assert_eq!(
            ev.payload.get("workspace_commit").and_then(|v| v.as_str()),
            Some("commit-xyz")
        );
        let _ = ws_id;
    }

    #[test]
    fn mission_without_sidecar_publishes_event_but_does_not_fabricate_sidecar() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let workspace_root_str = "/Users/test/proj-orphan";
        let workspace_root = WorkspaceRootPath::from(workspace_root_str);
        let mid = MissionId::new("M-orphan");
        let session_id_str = "S-orphan";

        seed_mission(home, &workspace_root, &mid, session_id_str);

        // session_store 故意不创建 session：模拟 mission 数据存在但 session 已清理
        let session_store = SessionStore::new();

        let workspace_store = WorkspaceStore::new();
        register_workspace(&workspace_store, "ws-orphan", workspace_root_str);
        let event_bus = Arc::new(InMemoryEventBus::new(64));

        recover_missions_at_bootstrap(home, &session_store, &workspace_store, &event_bus);

        // sidecar 不存在时仍会发布事件（事件代表"我们感知到了恢复点"），
        // 但 recovery_ref 不会被回灌（不存在的东西无法 attach）。
        // 这里我们断言：没有 sidecar 被悄悄伪造出来。
        let sid = SessionId::new(session_id_str);
        assert!(session_store.runtime_sidecar(&sid).is_none());

        // 事件仍然发布：read model / UI 可以据此提醒用户"有可恢复 mission 但缺会话"。
        let snapshot = event_bus.snapshot();
        let resumed: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.event_type == magi_event_bus::task_events::MISSION_RESUMED_FROM_RECOVERY)
            .collect();
        assert_eq!(resumed.len(), 1);
    }

    #[test]
    fn workspace_with_no_missions_directory_is_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        let session_store = SessionStore::new();
        let workspace_store = WorkspaceStore::new();
        register_workspace(&workspace_store, "ws-fresh", "/Users/test/proj-fresh");
        let event_bus = Arc::new(InMemoryEventBus::new(64));

        recover_missions_at_bootstrap(tmp.path(), &session_store, &workspace_store, &event_bus);

        let snapshot = event_bus.snapshot();
        assert!(
            snapshot
                .recent_events
                .iter()
                .all(|e| e.event_type != magi_event_bus::task_events::MISSION_RESUMED_FROM_RECOVERY)
        );
    }
}

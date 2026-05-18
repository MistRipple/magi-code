//! Task System v2 — TaskRunner 调度循环归属 conversation-runtime。
//!
//! 本模块只维护 v2 的执行事实：pending/running/terminal 状态推进、
//! worker 匹配、租约、结果回收。

use crate::task_runner_bridge::{
    RunCycleOutcome, TaskDispatchGate, TaskDispatchGateDecision, TaskDispatcher, TaskOutcome,
    TaskResultReceiver,
};
use crate::{ConversationRegistry, MailboxAuthor, MailboxKind, RuntimeSignal};
use magi_agent_role::AgentRoleRegistry;
use magi_core::{SessionId, Task, TaskId, TaskStatus, UtcMillis};
use magi_event_bus::InMemoryEventBus;
use magi_orchestrator::{
    task_store::TaskStore,
    task_worker_catalog::{WorkerInfo, resolve_task_role},
};
use magi_session_store::SessionStore;
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const DEFAULT_LEASE_DURATION_MS: u64 = 60_000;

pub struct TaskRunner {
    store: Arc<TaskStore>,
    workers: Vec<WorkerInfo>,
    dispatcher: Arc<dyn TaskDispatcher>,
    result_receiver: Arc<dyn TaskResultReceiver>,
    dispatch_gate: Option<Arc<TaskDispatchGate>>,
    event_bus: Option<Arc<InMemoryEventBus>>,
    child_result_route: Option<ChildResultRoute>,
    checkpoint_signal: AtomicBool,
    agent_role_registry: AgentRoleRegistry,
}

#[derive(Clone)]
struct ChildResultRoute {
    session_id: SessionId,
    session_store: Arc<SessionStore>,
    conversation_registry: Arc<ConversationRegistry>,
    spawn_graph: Arc<Mutex<magi_spawn_graph::SpawnGraph>>,
}

impl TaskRunner {
    pub fn with_dispatcher(
        store: Arc<TaskStore>,
        workers: Vec<WorkerInfo>,
        dispatcher: Arc<dyn TaskDispatcher>,
        result_receiver: Arc<dyn TaskResultReceiver>,
    ) -> Self {
        Self {
            store,
            workers,
            dispatcher,
            result_receiver,
            dispatch_gate: None,
            event_bus: None,
            child_result_route: None,
            checkpoint_signal: AtomicBool::new(false),
            agent_role_registry: AgentRoleRegistry::load_default(),
        }
    }

    pub fn with_agent_role_registry(mut self, registry: AgentRoleRegistry) -> Self {
        self.agent_role_registry = registry;
        self
    }

    pub fn with_event_bus(mut self, event_bus: Arc<InMemoryEventBus>) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    pub fn with_dispatch_gate(mut self, gate: Arc<TaskDispatchGate>) -> Self {
        self.dispatch_gate = Some(gate);
        self
    }

    pub fn with_child_result_route(
        mut self,
        session_id: SessionId,
        session_store: Arc<SessionStore>,
        conversation_registry: Arc<ConversationRegistry>,
        spawn_graph: Arc<Mutex<magi_spawn_graph::SpawnGraph>>,
    ) -> Self {
        self.child_result_route = Some(ChildResultRoute {
            session_id,
            session_store,
            conversation_registry,
            spawn_graph,
        });
        self
    }

    pub fn take_checkpoint_signal(&self) -> bool {
        self.checkpoint_signal.swap(false, Ordering::Relaxed)
    }

    fn set_checkpoint_signal(&self) {
        self.checkpoint_signal.store(true, Ordering::Relaxed);
    }

    pub fn run_cycle(&self, root_task_id: &TaskId) -> RunCycleOutcome {
        if let Err(error) = self.apply_results() {
            return RunCycleOutcome::Error(error);
        }

        if let Err(error) = self.expire_stale_leases(root_task_id) {
            return RunCycleOutcome::Error(error);
        }

        let active_leases = self.store.collect_active_leases(root_task_id);
        for (task_id, lease_id) in &active_leases {
            self.store.heartbeat_lease(task_id, lease_id);
        }

        match self.terminal_state(root_task_id) {
            TerminalState::AllCompleted => return RunCycleOutcome::AllComplete,
            TerminalState::HasFailures(task_ids) => {
                return RunCycleOutcome::Error(format!("任务执行失败: {:?}", task_ids));
            }
            TerminalState::HasKilled(task_ids) => {
                return RunCycleOutcome::Error(format!("任务已终止: {:?}", task_ids));
            }
            TerminalState::NotTerminal => {}
        }

        let runnable = self.store.get_runnable_leaves(root_task_id);
        if runnable.is_empty() {
            if !active_leases.is_empty() {
                return RunCycleOutcome::Continue;
            }
            return RunCycleOutcome::Stalled(self.collect_non_terminal_task_ids(root_task_id));
        }

        let mut dispatched = 0usize;
        let mut unmatched = Vec::new();
        for task in runnable {
            if let Some(gate) = &self.dispatch_gate {
                match gate(&task) {
                    Ok(TaskDispatchGateDecision::Allow) => {}
                    Ok(TaskDispatchGateDecision::Blocked(reason)) => {
                        return RunCycleOutcome::Blocked {
                            task_ids: vec![task.task_id.clone()],
                            reason,
                        };
                    }
                    Err(error) => {
                        return RunCycleOutcome::Error(format!(
                            "任务 {} 派发检查失败: {error}",
                            task.task_id
                        ));
                    }
                }
            }
            let Some(worker) = self.match_worker(&task) else {
                unmatched.push(task.task_id.clone());
                continue;
            };
            let Some(lease) = self.store.grant_lease(
                &task.task_id,
                root_task_id,
                &worker.worker_id,
                &worker.role,
                DEFAULT_LEASE_DURATION_MS,
            ) else {
                continue;
            };
            if let Err(error) = self
                .store
                .update_status_checked(&task.task_id, TaskStatus::Running)
            {
                self.store.revoke_lease(&task.task_id, &lease.lease_id);
                return RunCycleOutcome::Error(format!(
                    "任务 {} 进入 running 失败: {error}",
                    task.task_id
                ));
            }
            if let Err(error) = self.dispatcher.dispatch(&task, &worker, &lease) {
                self.store.revoke_lease(&task.task_id, &lease.lease_id);
                let _ = self.store.update_status(&task.task_id, TaskStatus::Failed);
                self.set_checkpoint_signal();
                return RunCycleOutcome::Error(format!("任务 {} 派发失败: {error}", task.task_id));
            }
            dispatched += 1;
        }

        if dispatched > 0 {
            RunCycleOutcome::Continue
        } else {
            RunCycleOutcome::Stalled(unmatched)
        }
    }

    pub fn finalize_stalled_outcome(
        &self,
        _root_task_id: &TaskId,
        task_ids: &[TaskId],
    ) -> Result<(), String> {
        for task_id in task_ids {
            if let Some(task) = self.store.get_task(task_id) {
                if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                    if let Some(lease) = self.store.get_active_lease(task_id) {
                        self.store.revoke_lease(task_id, &lease.lease_id);
                    }
                    self.store
                        .update_status(task_id, TaskStatus::Failed)
                        .map_err(|error| format!("收口不可运行任务 {task_id} 失败: {error}"))?;
                }
            }
        }
        self.set_checkpoint_signal();
        Ok(())
    }

    pub fn kill_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| format!("任务不存在: {task_id}"))?;
        if matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        ) {
            return Ok(());
        }
        if let Some(lease) = self.store.get_active_lease(task_id) {
            self.store.revoke_lease(task_id, &lease.lease_id);
        }
        self.store
            .update_status(task_id, TaskStatus::Killed)
            .map_err(|error| format!("终止任务 {task_id} 失败: {error}"))?;
        self.set_checkpoint_signal();
        Ok(())
    }

    pub fn kill_tree(&self, root_task_id: &TaskId) -> Result<(), String> {
        for task_id in self.collect_subtree_ids(root_task_id) {
            self.kill_task(&task_id)?;
        }
        Ok(())
    }

    pub fn resume_task(&self, task_id: &TaskId) -> Result<(), String> {
        let task = self
            .store
            .get_task(task_id)
            .ok_or_else(|| format!("任务不存在: {task_id}"))?;
        match task.status {
            // 已经在 Pending：等待 dispatcher 派发，无需动作
            TaskStatus::Pending => Ok(()),
            // 用户显式继续：把 Failed 任务回退到 Pending，由 dispatcher 重新派发
            // 这条路径是 `/api/session/continue` 的 root-status==Failed 入口
            TaskStatus::Failed => self
                .store
                .update_status(task_id, TaskStatus::Pending)
                .map_err(|error| format!("将任务 {task_id} 回退到 Pending 失败: {error}")),
            // 终态：Completed / Killed 不可恢复；Running 仍在跑也不需要 resume
            other => Err(format!(
                "Task System v2 不支持从 {:?} 状态恢复任务 {}",
                other, task_id
            )),
        }
    }

    fn apply_results(&self) -> Result<(), String> {
        for result in self.result_receiver.poll_results() {
            self.store.complete_lease(&result.task_id, &result.lease_id);
            self.route_child_result_to_parent(&result);
            match result.outcome {
                TaskOutcome::Completed { output_refs } => {
                    self.store.set_output_refs(&result.task_id, output_refs);
                    self.store
                        .update_status(&result.task_id, TaskStatus::Completed)
                        .map_err(|error| {
                            format!("任务 {} 完成状态写入失败: {error}", result.task_id)
                        })?;
                }
                TaskOutcome::Failed { error } => {
                    self.store.set_output_refs(&result.task_id, vec![error]);
                    self.store
                        .update_status(&result.task_id, TaskStatus::Failed)
                        .map_err(|err| {
                            format!("任务 {} 失败状态写入失败: {err}", result.task_id)
                        })?;
                }
            }
            self.set_checkpoint_signal();
        }
        Ok(())
    }

    fn route_child_result_to_parent(&self, result: &crate::task_runner_bridge::TaskResult) {
        let Some(route) = self.child_result_route.as_ref() else {
            return;
        };
        let Some(task) = self.store.get_task(&result.task_id) else {
            return;
        };
        let parent_id = match route.spawn_graph.lock() {
            Ok(mut graph) => {
                let parent_id = graph.parent_of(&result.task_id).cloned();
                if parent_id.is_some() {
                    let _ = graph.mark_closed(&result.task_id, std::time::SystemTime::now());
                }
                parent_id
            }
            Err(err) => {
                tracing::warn!(?err, task_id = %result.task_id, "关闭 SpawnGraph 子任务边失败");
                None
            }
        };
        let Some(parent_id) = parent_id else {
            return;
        };
        let Some(parent_task) = self.store.get_task(&parent_id) else {
            return;
        };
        let (status, payload) = match &result.outcome {
            TaskOutcome::Completed { output_refs } => (
                "completed",
                serde_json::json!({
                    "task_id": result.task_id.to_string(),
                    "status": "completed",
                    "output_refs": output_refs,
                    "title": task.title,
                }),
            ),
            TaskOutcome::Failed { error } => (
                "failed",
                serde_json::json!({
                    "task_id": result.task_id.to_string(),
                    "status": "failed",
                    "error": error,
                    "title": task.title,
                }),
            ),
        };
        route
            .conversation_registry
            .conversation_for_task(&route.session_id, &parent_id)
            .lock()
            .expect("parent task Conversation mutex poisoned")
            .ingest_runtime_signal(RuntimeSignal {
                author: MailboxAuthor::Child(result.task_id.to_string()),
                kind: MailboxKind::AgentResult,
                trigger_turn: true,
                payload,
                enqueued_at: UtcMillis::now(),
            });
        route.session_store.append_thread_messages(
            &route
                .session_store
                .ensure_session_mission(&route.session_id, UtcMillis::now(), || {
                    parent_task.mission_id.clone()
                })
                .1,
            vec![magi_session_store::ThreadChatMessage {
                role: "system".to_string(),
                content: Some(format!(
                    "[mailbox]\nauthor=child:{}\nkind=agent_result\nstatus={}",
                    result.task_id, status
                )),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
            UtcMillis::now(),
        );
        if parent_task.status == TaskStatus::Completed {
            let _ = self.store.update_status(&parent_id, TaskStatus::Pending);
        }
    }

    fn expire_stale_leases(&self, root_task_id: &TaskId) -> Result<(), String> {
        for (task_id, lease_id) in self.store.collect_expired_leases(root_task_id) {
            self.store.revoke_lease(&task_id, &lease_id);
            if self.store.get_task(&task_id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
            }) {
                self.store
                    .update_status(&task_id, TaskStatus::Failed)
                    .map_err(|error| format!("任务 {task_id} 租约过期收口失败: {error}"))?;
                self.set_checkpoint_signal();
            }
        }
        Ok(())
    }

    fn match_worker(&self, task: &Task) -> Option<WorkerInfo> {
        let role = resolve_task_role(task, &self.agent_role_registry);
        self.workers
            .iter()
            .find(|worker| {
                worker.supported_kinds.contains(&task.kind)
                    && role.map(|role| worker.role == role).unwrap_or(true)
            })
            .cloned()
            .or_else(|| {
                self.workers
                    .iter()
                    .find(|worker| worker.supported_kinds.contains(&task.kind))
                    .cloned()
            })
    }

    fn terminal_state(&self, root_task_id: &TaskId) -> TerminalState {
        let task_ids = self.collect_subtree_ids(root_task_id);
        if task_ids.is_empty() {
            return TerminalState::NotTerminal;
        }
        let mut failed = Vec::new();
        let mut killed = Vec::new();
        let mut all_terminal = true;
        for task_id in task_ids {
            let Some(task) = self.store.get_task(&task_id) else {
                continue;
            };
            match task.status {
                TaskStatus::Completed => {}
                TaskStatus::Failed => failed.push(task_id),
                TaskStatus::Killed => killed.push(task_id),
                TaskStatus::Pending | TaskStatus::Running => all_terminal = false,
            }
        }
        if !failed.is_empty() && all_terminal {
            TerminalState::HasFailures(failed)
        } else if !killed.is_empty() && all_terminal {
            TerminalState::HasKilled(killed)
        } else if all_terminal {
            TerminalState::AllCompleted
        } else {
            TerminalState::NotTerminal
        }
    }

    fn collect_non_terminal_task_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        self.collect_subtree_ids(root_task_id)
            .into_iter()
            .filter(|task_id| {
                self.store.get_task(task_id).is_some_and(|task| {
                    matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                })
            })
            .collect()
    }

    fn collect_subtree_ids(&self, root_task_id: &TaskId) -> Vec<TaskId> {
        let mut ids = Vec::new();
        let mut stack = vec![root_task_id.clone()];
        let mut visited = HashSet::new();
        while let Some(task_id) = stack.pop() {
            if !visited.insert(task_id.clone()) {
                continue;
            }
            if self.store.get_task(&task_id).is_none() {
                continue;
            }
            for child in self.store.get_children(&task_id) {
                stack.push(child.task_id);
            }
            ids.push(task_id);
        }
        ids
    }
}

enum TerminalState {
    NotTerminal,
    AllCompleted,
    HasFailures(Vec<TaskId>),
    HasKilled(Vec<TaskId>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_runner_bridge::{EventBasedResultReceiver, TaskResult};
    use magi_core::{LeaseId, MissionId, TaskKind, TaskRuntimePayload};
    use magi_orchestrator::task_store::TaskLeaseState;
    use magi_session_store::SessionStore;

    struct RejectingDispatcher;

    impl TaskDispatcher for RejectingDispatcher {
        fn dispatch(
            &self,
            _task: &Task,
            _worker: &WorkerInfo,
            _lease: &magi_orchestrator::task_store::TaskLease,
        ) -> Result<(), String> {
            Err("test dispatcher should not run".to_string())
        }
    }

    fn test_task(task_id: &str, root_task_id: &str, parent: Option<TaskId>) -> Task {
        let now = UtcMillis(1_000);
        Task {
            task_id: TaskId::new(task_id),
            mission_id: MissionId::new("mission-child-result"),
            root_task_id: TaskId::new(root_task_id),
            parent_task_id: parent,
            kind: TaskKind::LocalAgent,
            title: task_id.to_string(),
            goal: format!("run {task_id}"),
            status: TaskStatus::Pending,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: None,
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn dispatch_gate_blocks_without_lease_or_status_change() {
        let store = Arc::new(TaskStore::new());
        let root = test_task("task-root-gated", "task-root-gated", None);
        store.insert_task(root.clone());
        let receiver = Arc::new(EventBasedResultReceiver::new());
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        )
        .with_dispatch_gate(Arc::new(|task: &Task| {
            Ok(TaskDispatchGateDecision::Blocked(format!(
                "blocked for {}",
                task.task_id
            )))
        }));

        let outcome = runner.run_cycle(&root.task_id);

        assert!(matches!(
            outcome,
            RunCycleOutcome::Blocked { ref task_ids, ref reason }
                if task_ids == &vec![root.task_id.clone()]
                    && reason.contains("blocked for task-root-gated")
        ));
        assert_eq!(
            store.get_task(&root.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert!(store.get_active_lease(&root.task_id).is_none());
    }

    #[test]
    fn child_result_closes_spawn_edge_and_wakes_parent_mailbox() {
        let store = Arc::new(TaskStore::new());
        let mut parent = test_task("task-parent-route", "task-parent-route", None);
        parent.status = TaskStatus::Completed;
        let mut child = test_task(
            "task-child-route",
            "task-parent-route",
            Some(parent.task_id.clone()),
        );
        child.status = TaskStatus::Running;
        store.insert_task(parent.clone());
        store.insert_task(child.clone());
        let lease_id = LeaseId::new("lease-child-route");
        store.insert_lease(magi_orchestrator::task_store::TaskLease {
            lease_id: lease_id.clone(),
            task_id: child.task_id.clone(),
            root_task_id: parent.task_id.clone(),
            worker_id: magi_core::WorkerId::new("worker-child-route"),
            role: "integration-dev".to_string(),
            granted_at: UtcMillis(1_000),
            expires_at: UtcMillis(60_000),
            heartbeat_at: UtcMillis(1_000),
            lease_status: TaskLeaseState::Active,
        });

        let receiver = Arc::new(EventBasedResultReceiver::new());
        receiver.push_result(TaskResult {
            task_id: child.task_id.clone(),
            lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec!["child done".to_string()],
            },
        });
        let session_store = Arc::new(SessionStore::new());
        let conversation_registry = Arc::new(ConversationRegistry::new());
        let mut graph = magi_spawn_graph::SpawnGraph::new();
        graph
            .add_edge(
                parent.task_id.clone(),
                child.task_id.clone(),
                child.kind,
                std::time::SystemTime::UNIX_EPOCH,
            )
            .unwrap();
        let spawn_graph = Arc::new(Mutex::new(graph));
        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&store),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        )
        .with_child_result_route(
            SessionId::new("session-child-result"),
            session_store,
            Arc::clone(&conversation_registry),
            Arc::clone(&spawn_graph),
        );

        runner.apply_results().unwrap();

        assert_eq!(
            store.get_task(&parent.task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(
            spawn_graph
                .lock()
                .unwrap()
                .edge_for(&child.task_id)
                .unwrap()
                .status,
            magi_spawn_graph::SpawnEdgeStatus::Closed
        );
        let pending = conversation_registry
            .conversation_for_task(&SessionId::new("session-child-result"), &parent.task_id)
            .lock()
            .unwrap()
            .drain_mailbox_items();
        assert_eq!(pending.len(), 1);
        match &pending[0] {
            crate::MailboxItem::Runtime(signal) => {
                assert_eq!(signal.kind, MailboxKind::AgentResult);
                assert_eq!(
                    signal.author,
                    MailboxAuthor::Child(child.task_id.to_string())
                );
                assert!(signal.trigger_turn);
                assert_eq!(signal.payload["status"].as_str(), Some("completed"));
            }
            crate::MailboxItem::User(_) => panic!("child result must be a runtime signal"),
        }
    }

    /// Task System v2 §3.3 验收：coordinator 通过 `agent_spawn` 派发的子任务，
    /// 子任务完成后回执必须：
    /// 1) 进入 parent 的 mailbox（`MailboxKind::AgentResult`）；
    /// 2) 让 parent task 从 Completed/Idle 回到 Pending，使下一轮 conversation 能聚合结果；
    /// 3) 关闭 SpawnGraph 上 parent→child 的 open edge。
    ///
    /// 已有的 `agent_spawn_registers_child_execution_plan_and_lane` 只覆盖了 spawn 的写端，
    /// 已有的 `child_result_closes_spawn_edge_and_wakes_parent_mailbox` 只覆盖了
    /// **手工置入** SpawnGraph 边的回收路径。本测试桥接两者，证明：
    /// **`agent_spawn` 写入的 SpawnGraph 边正是 TaskRunner 结果路由消费的那条边**——
    /// 二者共享单一 SpawnGraph instance 不是巧合而是契约。
    #[test]
    fn coordinator_agent_spawn_to_child_result_round_trip() {
        use crate::task_execution_registry::TaskExecutionRegistry;
        use crate::task_runner_bridge::TaskOutcome;
        use crate::tool_batch::execute_task_tool_call_batch;
        use magi_bridge_client::{ChatToolCall, ChatToolFunction};
        use magi_core::{
            ExecutionResultStatus, MissionId, TaskRuntimePayload, WorkerId, WorkspaceId,
        };
        use magi_event_bus::InMemoryEventBus;
        use magi_orchestrator::task_store::TaskLeaseState;
        use magi_session_store::{
            ActiveExecutionBranch, ActiveExecutionChain, ActiveExecutionDispatchContext,
            ActiveExecutionTurn,
        };
        use magi_tool_runtime::BuiltinToolName;

        let event_bus = InMemoryEventBus::new(16);
        let task_store_arc = Arc::new(TaskStore::new());
        let session_store_arc = Arc::new(SessionStore::new());
        let execution_registry = TaskExecutionRegistry::default();
        let conversation_registry_arc = Arc::new(ConversationRegistry::new());
        let spawn_graph_inner = Mutex::new(magi_spawn_graph::SpawnGraph::new());
        let todo_ledger = magi_todo_ledger::TodoLedger::new();
        let agent_role_registry = AgentRoleRegistry::load_default();

        let session_id = SessionId::new("session-coord-spawn-roundtrip");
        let workspace_id = Some(WorkspaceId::new("workspace-coord-spawn-roundtrip"));
        let mission_id = MissionId::new("mission-coord-spawn-roundtrip");

        let parent_task_id = TaskId::new("task-parent-coord");
        let mut parent = Task {
            task_id: parent_task_id.clone(),
            mission_id: mission_id.clone(),
            root_task_id: parent_task_id.clone(),
            parent_task_id: None,
            kind: magi_core::TaskKind::LocalAgent,
            title: "coordinator root".to_string(),
            goal: "coordinate sub-work".to_string(),
            status: TaskStatus::Running,
            dependency_ids: Vec::new(),
            required_children: Vec::new(),
            policy_snapshot: None,
            executor_binding: Some(serde_json::json!({ "target_role": "coordinator" })),
            knowledge_refs: Vec::new(),
            workspace_scope: None,
            write_scope: None,
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            evidence_refs: Vec::new(),
            retry_count: 0,
            runtime_payload: TaskRuntimePayload::default(),
            created_at: UtcMillis::now(),
            updated_at: UtcMillis::now(),
        };
        // 模拟「parent 在子任务回执前已经把当前 turn 跑完」，便于断言 mailbox 唤醒
        parent.status = TaskStatus::Completed;
        task_store_arc.insert_task(parent.clone());

        let now = UtcMillis::now();
        let _ = session_store_arc.ensure_session_mission(&session_id, now, || mission_id.clone());
        let parent_worker_id = WorkerId::new("worker-parent-coord");
        session_store_arc
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: mission_id.clone(),
                    root_task_id: parent_task_id.clone(),
                    execution_chain_ref: "chain-coord-spawn-roundtrip".to_string(),
                    workspace_id: workspace_id.clone(),
                    active_branch_task_ids: vec![parent_task_id.clone()],
                    active_worker_bindings: vec![parent_worker_id.clone()],
                    branches: vec![ActiveExecutionBranch {
                        task_id: parent_task_id.clone(),
                        worker_id: parent_worker_id.clone(),
                        stage: "execute".to_string(),
                        lease_id: None,
                        execution_intent_ref: None,
                        binding_lifecycle: None,
                        checkpoint_stage: Some("execute".to_string()),
                        next_step_index: Some(0),
                        checkpoint_at: Some(now),
                        resume_mode: Some("stage-restart".to_string()),
                        resume_token: None,
                        use_tools: true,
                        skill_name: None,
                        is_primary: true,
                    }],
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: now,
                        entry_id: "timeline-coord-spawn-roundtrip".to_string(),
                        trimmed_text: Some("spawn child via coordinator".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: "turn-coord-spawn-roundtrip".to_string(),
                        turn_seq: now.0,
                        accepted_at: now,
                        completed_at: None,
                        status: "running".to_string(),
                        user_message: Some("spawn child".to_string()),
                        items: Vec::new(),
                        worker_lanes: Vec::new(),
                    }),
                },
            )
            .expect("active chain should be accepted");

        // 1) coordinator 调用 agent_spawn 派发子任务
        let tool_call = ChatToolCall {
            id: "call-coord-spawn".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: BuiltinToolName::AgentSpawn.as_str().to_string(),
                arguments: serde_json::json!({
                    "role": "integration-dev",
                    "goal": "处理一个独立 review comment",
                    "context": "返回 CHILD_DONE 即可",
                    "task_kind": "action"
                })
                .to_string(),
            },
        };
        let spawn_results = execute_task_tool_call_batch(
            &event_bus,
            None,
            &agent_role_registry,
            None,
            &task_store_arc,
            &session_store_arc,
            &execution_registry,
            &conversation_registry_arc,
            &spawn_graph_inner,
            None,
            &todo_ledger,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &parent,
            &session_id,
            &workspace_id,
            None,
            Some(&parent_worker_id),
            &[tool_call],
        );
        assert_eq!(spawn_results.len(), 1);
        assert_eq!(spawn_results[0].1, ExecutionResultStatus::Succeeded);
        let spawn_payload: serde_json::Value =
            serde_json::from_str(&spawn_results[0].0).expect("agent_spawn result is json");
        let child_task_id = TaskId::new(
            spawn_payload["child_task_id"]
                .as_str()
                .expect("agent_spawn must return child_task_id"),
        );

        // 写端契约：spawn 必须在 SpawnGraph 上挂上 parent→child 的 open edge
        {
            let graph = spawn_graph_inner.lock().unwrap();
            let edge = graph
                .edge_for(&child_task_id)
                .expect("SpawnGraph must record parent→child edge after agent_spawn");
            assert_eq!(edge.parent, parent_task_id);
            assert_eq!(
                edge.status,
                magi_spawn_graph::SpawnEdgeStatus::Open,
                "刚 spawn 完的 edge 必须是 Open，等待子任务 result 关闭",
            );
        }

        // 2) 模拟子任务完成回收：构造 TaskResult，喂给 TaskRunner.apply_results
        let lease_id = magi_core::LeaseId::new("lease-child-coord");
        task_store_arc.insert_lease(magi_orchestrator::task_store::TaskLease {
            lease_id: lease_id.clone(),
            task_id: child_task_id.clone(),
            root_task_id: parent_task_id.clone(),
            worker_id: WorkerId::new("worker-child-coord"),
            role: "integration-dev".to_string(),
            granted_at: UtcMillis(now.0 + 10),
            expires_at: UtcMillis(now.0 + 60_000),
            heartbeat_at: UtcMillis(now.0 + 10),
            lease_status: TaskLeaseState::Active,
        });
        let receiver = Arc::new(EventBasedResultReceiver::new());
        receiver.push_result(TaskResult {
            task_id: child_task_id.clone(),
            lease_id,
            outcome: TaskOutcome::Completed {
                output_refs: vec!["CHILD_DONE".to_string()],
            },
        });

        // 共享同一个 SpawnGraph instance——这是 §3.3 桥接的关键不变量
        let mut transplanted_graph = magi_spawn_graph::SpawnGraph::new();
        {
            let src = spawn_graph_inner.lock().unwrap();
            for edge in src.all_edges() {
                transplanted_graph
                    .add_edge(
                        edge.parent.clone(),
                        edge.child.clone(),
                        edge.task_kind,
                        edge.created_at,
                    )
                    .unwrap();
            }
        }
        let shared_spawn_graph = Arc::new(Mutex::new(transplanted_graph));

        let runner = TaskRunner::with_dispatcher(
            Arc::clone(&task_store_arc),
            Vec::new(),
            Arc::new(RejectingDispatcher),
            receiver,
        )
        .with_child_result_route(
            session_id.clone(),
            Arc::clone(&session_store_arc),
            Arc::clone(&conversation_registry_arc),
            Arc::clone(&shared_spawn_graph),
        );
        runner
            .apply_results()
            .expect("apply_results 必须接受合法的子任务回执");

        // 3) 断言三条 §3.3 不变量
        assert_eq!(
            shared_spawn_graph
                .lock()
                .unwrap()
                .edge_for(&child_task_id)
                .expect("edge 仍可见，只是状态从 Open 翻为 Closed")
                .status,
            magi_spawn_graph::SpawnEdgeStatus::Closed,
            "§3.3：子任务回执必须关闭 SpawnGraph open edge",
        );
        assert_eq!(
            task_store_arc.get_task(&parent_task_id).unwrap().status,
            TaskStatus::Pending,
            "§3.3：parent 被 mailbox 中的 AgentResult 唤醒，须从 Completed 回到 Pending",
        );
        let pending = conversation_registry_arc
            .conversation_for_task(&session_id, &parent_task_id)
            .lock()
            .unwrap()
            .drain_mailbox_items();
        assert_eq!(
            pending.len(),
            1,
            "§3.3：parent 应当且仅有一条 child result mailbox 项",
        );
        match &pending[0] {
            crate::MailboxItem::Runtime(signal) => {
                assert_eq!(signal.kind, MailboxKind::AgentResult);
                assert_eq!(
                    signal.author,
                    MailboxAuthor::Child(child_task_id.to_string())
                );
                assert!(
                    signal.trigger_turn,
                    "AgentResult 必须 trigger_turn 以触发 parent 下一轮汇总",
                );
                assert_eq!(signal.payload["status"].as_str(), Some("completed"));
            }
            crate::MailboxItem::User(_) => {
                panic!("子任务回执必须以 Runtime signal 形式进入 mailbox")
            }
        }
    }
}

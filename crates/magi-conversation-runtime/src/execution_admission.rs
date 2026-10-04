use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use magi_core::{SessionId, TaskId, UtcMillis};
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

const MIN_AVAILABLE_MEMORY_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAdmissionLimits {
    pub max_active_tasks: usize,
    pub max_active_tasks_per_session: usize,
    pub max_active_tasks_per_role: usize,
    /// 仅在能够读取到系统可用内存时生效；低于阈值时暂停新任务准入，
    /// 不会中断已经运行的任务。
    pub min_available_memory_bytes: u64,
}

impl Default for ExecutionAdmissionLimits {
    fn default() -> Self {
        let available_parallelism = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4);
        let max_active_tasks = available_parallelism.clamp(2, 8);
        Self {
            max_active_tasks,
            max_active_tasks_per_session: max_active_tasks.min(4),
            max_active_tasks_per_role: 5,
            min_available_memory_bytes: MIN_AVAILABLE_MEMORY_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResourceSnapshot {
    /// 系统 CPU 使用率，单位为万分之一百分比（10_000 表示 100%）。
    pub system_cpu_usage_basis_points: u16,
    /// Magi 当前进程 CPU 使用率，单位为万分之一百分比。
    pub process_cpu_usage_basis_points: Option<u16>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub process_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAdmissionSnapshot {
    pub limits: ExecutionAdmissionLimits,
    pub available_parallelism: usize,
    pub resources: ExecutionResourceSnapshot,
    pub active_task_count: usize,
    pub queued_task_count: usize,
    pub active_task_ids: Vec<String>,
    pub queued: Vec<QueuedExecutionAdmission>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedExecutionAdmission {
    pub task_id: String,
    pub session_id: Option<String>,
    pub role: String,
    pub reason: String,
    pub queued_at: UtcMillis,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionAdmissionBlocked {
    pub reason: String,
}

#[derive(Clone)]
pub struct ExecutionAdmissionController {
    limits: ExecutionAdmissionLimits,
    available_parallelism: usize,
    resource_probe: Arc<Mutex<ExecutionResourceProbe>>,
    state: Arc<Mutex<ExecutionAdmissionState>>,
}

struct ExecutionAdmissionState {
    active: HashMap<TaskId, ActiveExecutionAdmission>,
    queued: HashMap<TaskId, QueuedExecutionAdmission>,
}

struct ActiveExecutionAdmission {
    session_id: Option<SessionId>,
    role: String,
    phase: AdmissionPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmissionPhase {
    /// 子代理注册时预占的名额，Runner 派发时转为 Running。
    Reserved,
    Running,
    /// 正在 agent_wait 等待子代理的协调者：不占用会话、全局和角色名额，
    /// 否则协调者会和自己派发的子代理抢同一批名额。
    Waiting,
}

impl AdmissionPhase {
    fn occupies_capacity(self) -> bool {
        !matches!(self, Self::Waiting)
    }
}

/// 协调者等待子代理期间的名额让渡；drop 时恢复占用。
pub struct AdmissionWaitGuard {
    controller: ExecutionAdmissionController,
    task_id: TaskId,
}

impl Drop for AdmissionWaitGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.controller.state.lock()
            && let Some(active) = state.active.get_mut(&self.task_id)
            && active.phase == AdmissionPhase::Waiting
        {
            active.phase = AdmissionPhase::Running;
        }
    }
}

struct ExecutionResourceProbe {
    system: System,
}

pub struct ExecutionAdmissionPermit {
    controller: Option<ExecutionAdmissionController>,
    task_id: TaskId,
}

impl ExecutionAdmissionController {
    pub fn new(limits: ExecutionAdmissionLimits) -> Self {
        let available_parallelism = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1);
        Self {
            limits,
            available_parallelism,
            resource_probe: Arc::new(Mutex::new(ExecutionResourceProbe {
                system: System::new(),
            })),
            state: Arc::new(Mutex::new(ExecutionAdmissionState {
                active: HashMap::new(),
                queued: HashMap::new(),
            })),
        }
    }

    pub fn acquire(
        &self,
        task_id: TaskId,
        session_id: Option<SessionId>,
        role: impl Into<String>,
    ) -> Result<ExecutionAdmissionPermit, ExecutionAdmissionBlocked> {
        let role = role.into();
        let resources = self.resource_snapshot();
        let mut state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        if let Some(active) = state.active.get_mut(&task_id) {
            if active.phase != AdmissionPhase::Reserved {
                return Err(ExecutionAdmissionBlocked {
                    reason: format!("任务 {task_id} 已占用执行槽位。"),
                });
            }
            // 注册时预占的名额直接转为执行许可。
            active.phase = AdmissionPhase::Running;
            return Ok(ExecutionAdmissionPermit {
                controller: Some(self.clone()),
                task_id,
            });
        }
        self.admit_locked(
            &mut state,
            task_id.clone(),
            session_id,
            role,
            &resources,
            AdmissionPhase::Running,
        )
        .map(|()| ExecutionAdmissionPermit {
            controller: Some(self.clone()),
            task_id,
        })
        .map_err(|reason| ExecutionAdmissionBlocked { reason })
    }

    pub fn limits(&self) -> &ExecutionAdmissionLimits {
        &self.limits
    }

    /// 子代理注册时预占名额。返回 `None` 表示已预占、会立即开始；
    /// 返回排队原因表示名额不足，任务进入排队，名额空出后由 Runner 准入。
    ///
    /// 预占与 `acquire` 共享同一份计数，同一批派发的多个代理不会都被报告为已开始。
    pub fn reserve(
        &self,
        task_id: TaskId,
        session_id: Option<SessionId>,
        role: &str,
    ) -> Option<String> {
        let resources = self.resource_snapshot();
        let mut state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        if state.active.contains_key(&task_id) {
            return None;
        }
        self.admit_locked(
            &mut state,
            task_id,
            session_id,
            role.to_string(),
            &resources,
            AdmissionPhase::Reserved,
        )
        .err()
    }

    /// 协调者开始等待子代理：让出名额，guard drop 时恢复。没有执行许可时返回 `None`。
    pub fn suspend_while_waiting(&self, task_id: &TaskId) -> Option<AdmissionWaitGuard> {
        let mut state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        let active = state.active.get_mut(task_id)?;
        if active.phase != AdmissionPhase::Running {
            return None;
        }
        active.phase = AdmissionPhase::Waiting;
        Some(AdmissionWaitGuard {
            controller: self.clone(),
            task_id: task_id.clone(),
        })
    }

    fn admit_locked(
        &self,
        state: &mut ExecutionAdmissionState,
        task_id: TaskId,
        session_id: Option<SessionId>,
        role: String,
        resources: &ExecutionResourceSnapshot,
        phase: AdmissionPhase,
    ) -> Result<(), String> {
        if let Some(reason) = self.block_reason(state, session_id.as_ref(), &role, resources) {
            state.queued.insert(
                task_id.clone(),
                QueuedExecutionAdmission {
                    task_id: task_id.to_string(),
                    session_id: session_id.as_ref().map(ToString::to_string),
                    role,
                    reason: reason.clone(),
                    queued_at: UtcMillis::now(),
                },
            );
            return Err(reason);
        }
        state.queued.remove(&task_id);
        state.active.insert(
            task_id,
            ActiveExecutionAdmission {
                session_id,
                role,
                phase,
            },
        );
        Ok(())
    }

    pub fn snapshot(&self) -> ExecutionAdmissionSnapshot {
        let resources = self.resource_snapshot();
        let state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        let mut active_task_ids = state
            .active
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        active_task_ids.sort();
        let mut queued = state.queued.values().cloned().collect::<Vec<_>>();
        queued.sort_by(|left, right| {
            left.queued_at
                .cmp(&right.queued_at)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        ExecutionAdmissionSnapshot {
            limits: self.limits.clone(),
            available_parallelism: self.available_parallelism,
            resources,
            active_task_count: state.active.len(),
            queued_task_count: queued.len(),
            active_task_ids,
            queued,
        }
    }

    /// 任务不再需要执行（被终止或已结束）：移除排队记录和尚未转为执行许可的预占。
    pub fn release_pending(&self, task_id: &TaskId) {
        let mut state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        state.queued.remove(task_id);
        if state
            .active
            .get(task_id)
            .is_some_and(|active| active.phase == AdmissionPhase::Reserved)
        {
            state.active.remove(task_id);
        }
    }

    pub fn release_pending_session(&self, session_id: &SessionId) {
        let mut state = self
            .state
            .lock()
            .expect("execution admission lock poisoned");
        state
            .queued
            .retain(|_, queued| queued.session_id.as_deref() != Some(session_id.as_str()));
        state.active.retain(|_, active| {
            active.phase != AdmissionPhase::Reserved
                || active.session_id.as_ref() != Some(session_id)
        });
    }

    fn release(&self, task_id: &TaskId) {
        self.state
            .lock()
            .expect("execution admission lock poisoned")
            .active
            .remove(task_id);
    }

    fn block_reason(
        &self,
        state: &ExecutionAdmissionState,
        session_id: Option<&SessionId>,
        role: &str,
        resources: &ExecutionResourceSnapshot,
    ) -> Option<String> {
        if let Some(available_memory_bytes) = resources.available_memory_bytes
            && available_memory_bytes < self.limits.min_available_memory_bytes
        {
            return Some(format!(
                "系统可用内存不足（{} MiB，最低需要 {} MiB），任务将在资源恢复后继续。",
                available_memory_bytes / (1024 * 1024),
                self.limits.min_available_memory_bytes / (1024 * 1024),
            ));
        }
        let occupying = || {
            state
                .active
                .values()
                .filter(|active| active.phase.occupies_capacity())
        };
        let active_total = occupying().count();
        if active_total >= self.limits.max_active_tasks {
            return Some(format!(
                "全局执行容量已满（{}/{}），任务将在有可用槽位后继续。",
                active_total, self.limits.max_active_tasks
            ));
        }
        if let Some(session_id) = session_id {
            let active_for_session = occupying()
                .filter(|active| active.session_id.as_ref() == Some(session_id))
                .count();
            if active_for_session >= self.limits.max_active_tasks_per_session {
                return Some(format!(
                    "当前会话执行容量已满（{active_for_session}/{}），任务将在有可用槽位后继续。",
                    self.limits.max_active_tasks_per_session
                ));
            }
        }
        let active_for_role = occupying().filter(|active| active.role == role).count();
        if active_for_role >= self.limits.max_active_tasks_per_role {
            return Some(format!(
                "角色 {role} 的全局执行容量已满（{active_for_role}/{}），任务将在有可用槽位后继续。",
                self.limits.max_active_tasks_per_role
            ));
        }
        None
    }

    fn resource_snapshot(&self) -> ExecutionResourceSnapshot {
        self.resource_probe
            .lock()
            .expect("execution resource probe lock poisoned")
            .snapshot()
    }
}

impl Default for ExecutionAdmissionController {
    fn default() -> Self {
        Self::new(ExecutionAdmissionLimits::default())
    }
}

impl ExecutionResourceProbe {
    fn snapshot(&mut self) -> ExecutionResourceSnapshot {
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();

        let process = get_current_pid().ok().and_then(|pid| {
            self.system
                .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            self.system.process(pid).map(|process| {
                (
                    percentage_to_basis_points(process.cpu_usage()),
                    process.memory(),
                )
            })
        });

        let total_memory_bytes = self.system.total_memory();
        let available_memory_bytes = self.system.available_memory();
        ExecutionResourceSnapshot {
            system_cpu_usage_basis_points: percentage_to_basis_points(
                self.system.global_cpu_usage(),
            ),
            process_cpu_usage_basis_points: process.map(|(cpu_usage, _)| cpu_usage),
            total_memory_bytes: (total_memory_bytes > 0).then_some(total_memory_bytes),
            available_memory_bytes: (available_memory_bytes > 0).then_some(available_memory_bytes),
            process_memory_bytes: process.map(|(_, memory)| memory),
        }
    }
}

fn percentage_to_basis_points(percentage: f32) -> u16 {
    (percentage.clamp(0.0, 100.0) * 100.0).round() as u16
}

impl Drop for ExecutionAdmissionPermit {
    fn drop(&mut self) {
        if let Some(controller) = self.controller.take() {
            controller.release(&self.task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> ExecutionAdmissionController {
        ExecutionAdmissionController::new(ExecutionAdmissionLimits {
            max_active_tasks: 2,
            max_active_tasks_per_session: 1,
            max_active_tasks_per_role: 1,
            min_available_memory_bytes: 0,
        })
    }

    #[test]
    fn enforces_global_session_and_role_limits_and_recovers_after_drop() {
        let controller = controller();
        let session_a = SessionId::new("session-a");
        let session_b = SessionId::new("session-b");
        let permit = controller
            .acquire(TaskId::new("task-a"), Some(session_a.clone()), "executor")
            .expect("first task should acquire capacity");

        let session_block = controller
            .acquire(TaskId::new("task-b"), Some(session_a), "reviewer")
            .err()
            .expect("same session must respect its capacity");
        assert!(session_block.reason.contains("当前会话执行容量已满"));

        let role_block = controller
            .acquire(TaskId::new("task-c"), Some(session_b.clone()), "executor")
            .err()
            .expect("same role must respect its global capacity");
        assert!(role_block.reason.contains("角色 executor"));

        drop(permit);
        let recovered = controller
            .acquire(TaskId::new("task-c"), Some(session_b), "executor")
            .expect("dropping the permit must release capacity");
        drop(recovered);
        assert_eq!(controller.snapshot().active_task_count, 0);
    }

    #[test]
    fn queued_entries_are_removed_when_the_session_is_closed() {
        let controller = controller();
        let session = SessionId::new("session-a");
        let _permit = controller
            .acquire(TaskId::new("task-a"), Some(session.clone()), "executor")
            .expect("first task should acquire capacity");
        let _ = controller.acquire(TaskId::new("task-b"), Some(session.clone()), "reviewer");
        assert_eq!(controller.snapshot().queued_task_count, 1);

        controller.release_pending_session(&session);
        assert_eq!(controller.snapshot().queued_task_count, 0);
    }

    #[test]
    fn repeated_acquire_cannot_create_a_second_permit_for_the_same_task() {
        let controller = controller();
        let task_id = TaskId::new("task-single-permit");
        let permit = controller
            .acquire(task_id.clone(), None, "executor")
            .expect("first acquire should succeed");

        let blocked = controller
            .acquire(task_id.clone(), None, "executor")
            .err()
            .expect("same task must not receive a second release-capable permit");
        assert!(blocked.reason.contains("已占用执行槽位"));
        assert_eq!(controller.snapshot().queued_task_count, 0);

        drop(permit);
        assert_eq!(controller.snapshot().active_task_count, 0);
    }

    #[test]
    fn reservations_count_against_capacity_and_become_the_dispatch_permit() {
        let controller = ExecutionAdmissionController::new(ExecutionAdmissionLimits {
            max_active_tasks: 8,
            max_active_tasks_per_session: 2,
            max_active_tasks_per_role: 8,
            min_available_memory_bytes: 0,
        });
        let session = SessionId::new("session-reserve");
        let coordinator = controller
            .acquire(
                TaskId::new("task-root"),
                Some(session.clone()),
                "coordinator",
            )
            .expect("coordinator should run");

        // 同一批派发：只有空余名额内的代理被报告为立即开始。
        assert_eq!(
            controller.reserve(
                TaskId::new("task-child-a"),
                Some(session.clone()),
                "executor"
            ),
            None
        );
        let queued = controller
            .reserve(
                TaskId::new("task-child-b"),
                Some(session.clone()),
                "executor",
            )
            .expect("second child must queue while the coordinator runs");
        assert!(queued.contains("当前会话执行容量已满"));

        // 协调者等待子代理时让出名额，排队的代理可以准入。
        let waiting = controller
            .suspend_while_waiting(&TaskId::new("task-root"))
            .expect("running coordinator can wait");
        let child_b = controller
            .acquire(
                TaskId::new("task-child-b"),
                Some(session.clone()),
                "executor",
            )
            .expect("waiting coordinator must not hold a slot");
        let child_a = controller
            .acquire(
                TaskId::new("task-child-a"),
                Some(session.clone()),
                "executor",
            )
            .expect("reservation turns into the dispatch permit");
        assert_eq!(controller.snapshot().active_task_count, 3);
        drop(waiting);

        drop(child_a);
        drop(child_b);
        drop(coordinator);
        assert_eq!(controller.snapshot().active_task_count, 0);
    }

    #[test]
    fn releasing_a_pending_task_frees_its_reservation_but_not_a_running_permit() {
        let controller = controller();
        let session = SessionId::new("session-release");
        assert_eq!(
            controller.reserve(
                TaskId::new("task-reserved"),
                Some(session.clone()),
                "executor"
            ),
            None
        );
        controller.release_pending(&TaskId::new("task-reserved"));
        assert_eq!(controller.snapshot().active_task_count, 0);

        let permit = controller
            .acquire(TaskId::new("task-running"), Some(session), "executor")
            .expect("should run");
        controller.release_pending(&TaskId::new("task-running"));
        assert_eq!(controller.snapshot().active_task_count, 1);
        drop(permit);
    }

    #[test]
    fn resource_snapshot_is_safe_to_collect_on_supported_and_unknown_hosts() {
        let snapshot = ExecutionAdmissionController::default().snapshot();
        assert!(snapshot.available_parallelism >= 1);
        assert!(snapshot.resources.system_cpu_usage_basis_points <= 10_000);
        assert!(
            snapshot
                .resources
                .process_cpu_usage_basis_points
                .is_none_or(|value| value <= 10_000)
        );
    }
}

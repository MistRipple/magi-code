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
        if state.active.contains_key(&task_id) {
            return Err(ExecutionAdmissionBlocked {
                reason: format!("任务 {task_id} 已占用执行槽位。"),
            });
        }

        let reason = self.block_reason(&state, session_id.as_ref(), &role, &resources);
        if let Some(reason) = reason {
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
            return Err(ExecutionAdmissionBlocked { reason });
        }

        state.queued.remove(&task_id);
        state.active.insert(
            task_id.clone(),
            ActiveExecutionAdmission { session_id, role },
        );
        Ok(ExecutionAdmissionPermit {
            controller: Some(self.clone()),
            task_id,
        })
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

    pub fn remove_queued_task(&self, task_id: &TaskId) {
        self.state
            .lock()
            .expect("execution admission lock poisoned")
            .queued
            .remove(task_id);
    }

    pub fn remove_queued_session(&self, session_id: &SessionId) {
        self.state
            .lock()
            .expect("execution admission lock poisoned")
            .queued
            .retain(|_, queued| queued.session_id.as_deref() != Some(session_id.as_str()));
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
        if state.active.len() >= self.limits.max_active_tasks {
            return Some(format!(
                "全局执行容量已满（{}/{}），任务将在有可用槽位后继续。",
                state.active.len(),
                self.limits.max_active_tasks
            ));
        }
        if let Some(session_id) = session_id {
            let active_for_session = state
                .active
                .values()
                .filter(|active| active.session_id.as_ref() == Some(session_id))
                .count();
            if active_for_session >= self.limits.max_active_tasks_per_session {
                return Some(format!(
                    "当前会话执行容量已满（{active_for_session}/{}），任务将在有可用槽位后继续。",
                    self.limits.max_active_tasks_per_session
                ));
            }
        }
        let active_for_role = state
            .active
            .values()
            .filter(|active| active.role == role)
            .count();
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

        controller.remove_queued_session(&session);
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

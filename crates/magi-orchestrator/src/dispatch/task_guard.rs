#[derive(Clone, Debug)]
pub struct TaskUpdateCallerContext {
    pub mission_id: String,
    pub lease_id: String,
    pub worker_id: String,
    pub current_task_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TaskUpdateRequest {
    pub task_id: String,
    pub status: Option<String>,
    pub content: Option<String>,
    pub force_reset: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct TaskView {
    pub id: String,
    pub lease_id: String,
    pub worker_id: String,
    pub status: String,
}

pub fn validate_worker_task_update(
    caller: Option<&TaskUpdateCallerContext>,
    target: Option<&TaskView>,
    update: &TaskUpdateRequest,
) -> Option<String> {
    let caller = match caller {
        Some(c) => c,
        None => return None,
    };

    let target = match target {
        Some(t) => t,
        None => return Some(format!("Task not found: {}", update.task_id)),
    };

    if target.lease_id != caller.lease_id {
        return Some("Worker 只能更新当前 Lease 内的 Task".to_string());
    }

    if target.worker_id != caller.worker_id {
        return Some("Worker 不能更新其他 Worker 的 Task".to_string());
    }

    if update.force_reset == Some(true) {
        return Some(
            "Worker 禁止通过 task_update 执行 force_reset；中断与重试由编排层或执行状态机负责"
                .to_string(),
        );
    }

    if update.content.is_some() {
        return Some(
            "Worker 禁止通过 task_update 改写 Task 内容；任务结构调整应通过 task_split 或编排层完成"
                .to_string(),
        );
    }

    if update.status.as_deref() == Some("draft") || update.status.as_deref() == Some("ready") {
        return Some(
            "Worker 禁止通过 task_update 将 Task 重置为 draft/ready；恢复与重试由执行状态机负责"
                .to_string(),
        );
    }

    if caller.current_task_id.as_deref() == Some(&target.id) && update.status.is_some() {
        return Some(
            "Worker 禁止通过 task_update 修改当前执行中的 Task 状态；当前 Task 的完成/失败由执行循环统一提交"
                .to_string(),
        );
    }

    if target.status == "running" && update.status.is_some() {
        return Some(
            "Worker 禁止通过 task_update 修改 running Task 状态；请交由编排层处理".to_string(),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_caller() -> TaskUpdateCallerContext {
        TaskUpdateCallerContext {
            mission_id: "m-1".to_string(),
            lease_id: "lease-1".to_string(),
            worker_id: "w-1".to_string(),
            current_task_id: Some("task-current".to_string()),
        }
    }

    fn make_target() -> TaskView {
        TaskView {
            id: "task-other".to_string(),
            lease_id: "lease-1".to_string(),
            worker_id: "w-1".to_string(),
            status: "ready".to_string(),
        }
    }

    #[test]
    fn valid_update_passes() {
        let caller = make_caller();
        let target = make_target();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        assert!(validate_worker_task_update(Some(&caller), Some(&target), &update).is_none());
    }

    #[test]
    fn no_caller_passes() {
        let target = make_target();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        assert!(validate_worker_task_update(None, Some(&target), &update).is_none());
    }

    #[test]
    fn missing_target_rejected() {
        let caller = make_caller();
        let update = TaskUpdateRequest {
            task_id: "missing".to_string(),
            status: None,
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), None, &update);
        assert!(result.unwrap().contains("not found"));
    }

    #[test]
    fn cross_lease_rejected() {
        let caller = make_caller();
        let mut target = make_target();
        target.lease_id = "lease-other".to_string();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("只能更新当前 Lease"));
    }

    #[test]
    fn cross_worker_rejected() {
        let caller = make_caller();
        let mut target = make_target();
        target.worker_id = "w-other".to_string();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("不能更新其他 Worker"));
    }

    #[test]
    fn force_reset_rejected() {
        let caller = make_caller();
        let target = make_target();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: None,
            content: None,
            force_reset: Some(true),
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("force_reset"));
    }

    #[test]
    fn content_change_rejected() {
        let caller = make_caller();
        let target = make_target();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: None,
            content: Some("new content".to_string()),
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("改写 Task 内容"));
    }

    #[test]
    fn draft_status_rejected() {
        let caller = make_caller();
        let target = make_target();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("draft".to_string()),
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("draft/ready"));
    }

    #[test]
    fn modify_current_task_rejected() {
        let caller = make_caller();
        let mut target = make_target();
        target.id = "task-current".to_string();
        let update = TaskUpdateRequest {
            task_id: "task-current".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("当前执行中的 Task"));
    }

    #[test]
    fn modify_running_task_rejected() {
        let caller = make_caller();
        let mut target = make_target();
        target.status = "running".to_string();
        let update = TaskUpdateRequest {
            task_id: "task-other".to_string(),
            status: Some("completed".to_string()),
            content: None,
            force_reset: None,
        };
        let result = validate_worker_task_update(Some(&caller), Some(&target), &update);
        assert!(result.unwrap().contains("running Task"));
    }
}

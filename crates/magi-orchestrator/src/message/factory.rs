use super::bus::{MessageContext, OrchestratorMessage, OrchestratorMessageKind};
use crate::dispatch::DispatchBatchSummary;
use magi_core::{MissionId, TaskId, UtcMillis, WorkerId};
use serde_json::Value;

pub struct MessageFactory;

impl MessageFactory {
    pub fn dispatch_started(
        mission_id: MissionId,
        batch_id: &str,
        task_count: usize,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::DispatchStarted,
            message: format!("派发批次 {} 启动，共 {} 个任务", batch_id, task_count),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({
                "batch_id": batch_id,
                "task_count": task_count,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn dispatch_completed(
        mission_id: MissionId,
        batch_id: &str,
        summary: &DispatchBatchSummary,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::DispatchCompleted,
            message: format!(
                "派发批次 {} 完成：{} 成功 / {} 失败 / {} 跳过",
                batch_id, summary.completed, summary.failed, summary.skipped
            ),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::to_value(summary).unwrap_or(Value::Null),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn dispatch_failed(
        mission_id: MissionId,
        batch_id: &str,
        reason: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::DispatchFailed,
            message: format!("派发批次 {} 失败：{}", batch_id, reason),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({
                "batch_id": batch_id,
                "reason": reason,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn dispatch_cancelled(
        mission_id: MissionId,
        batch_id: &str,
        reason: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::DispatchCancelled,
            message: format!("派发批次 {} 取消：{}", batch_id, reason),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({
                "batch_id": batch_id,
                "reason": reason,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn worker_started(
        mission_id: MissionId,
        task_id: TaskId,
        worker_id: WorkerId,
        task_title: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::WorkerStarted,
            message: format!("Worker {} 开始执行: {}", worker_id, task_title),
            context: MessageContext::for_worker(mission_id, task_id, worker_id.clone()),
            payload: serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_title": task_title,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn worker_completed(
        mission_id: MissionId,
        task_id: TaskId,
        worker_id: WorkerId,
        summary: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::WorkerCompleted,
            message: format!("Worker {} 完成: {}", worker_id, summary),
            context: MessageContext::for_worker(mission_id, task_id, worker_id.clone()),
            payload: serde_json::json!({
                "worker_id": worker_id.to_string(),
                "summary": summary,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn worker_failed(
        mission_id: MissionId,
        task_id: TaskId,
        worker_id: WorkerId,
        error: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::WorkerFailed,
            message: format!("Worker {} 失败: {}", worker_id, error),
            context: MessageContext::for_worker(mission_id, task_id, worker_id.clone()),
            payload: serde_json::json!({
                "worker_id": worker_id.to_string(),
                "error": error,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn mission_created(
        mission_id: MissionId,
        title: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::MissionCreated,
            message: format!("任务 {} 创建: {}", mission_id, title),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({ "title": title }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn mission_completed(
        mission_id: MissionId,
        summary: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::MissionCompleted,
            message: format!("任务 {} 完成: {}", mission_id, summary),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({ "summary": summary }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn mission_failed(
        mission_id: MissionId,
        reason: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::MissionFailed,
            message: format!("任务 {} 失败: {}", mission_id, reason),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({ "reason": reason }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn progress_update(
        mission_id: MissionId,
        progress_message: &str,
        percentage: Option<f64>,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::ProgressUpdate,
            message: progress_message.to_string(),
            context: MessageContext::for_mission(mission_id),
            payload: serde_json::json!({
                "message": progress_message,
                "percentage": percentage,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn error_report(
        context: MessageContext,
        error: &str,
        source: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::ErrorReport,
            message: format!("[{}] {}", source, error),
            context,
            payload: serde_json::json!({
                "error": error,
                "source": source,
            }),
            occurred_at: UtcMillis::now(),
        }
    }

    pub fn verification_result(
        mission_id: MissionId,
        task_id: TaskId,
        passed: bool,
        summary: &str,
    ) -> OrchestratorMessage {
        OrchestratorMessage {
            kind: OrchestratorMessageKind::VerificationResult,
            message: format!(
                "验证{}: {}",
                if passed { "通过" } else { "失败" },
                summary
            ),
            context: MessageContext::for_task(mission_id, task_id),
            payload: serde_json::json!({
                "passed": passed,
                "summary": summary,
            }),
            occurred_at: UtcMillis::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_creates_dispatch_started() {
        let msg = MessageFactory::dispatch_started(
            MissionId::new("m-1"),
            "batch-1",
            3,
        );
        assert_eq!(msg.kind, OrchestratorMessageKind::DispatchStarted);
        assert!(msg.message.contains("3"));
        assert!(msg.message.contains("batch-1"));
    }

    #[test]
    fn factory_creates_worker_completed() {
        let msg = MessageFactory::worker_completed(
            MissionId::new("m-1"),
            TaskId::new("t-1"),
            WorkerId::new("w-1"),
            "任务完成",
        );
        assert_eq!(msg.kind, OrchestratorMessageKind::WorkerCompleted);
        assert!(msg.message.contains("w-1"));
        assert!(msg.context.worker_id.is_some());
    }

    #[test]
    fn factory_creates_error_report() {
        let msg = MessageFactory::error_report(
            MessageContext::default(),
            "连接超时",
            "routing",
        );
        assert_eq!(msg.kind, OrchestratorMessageKind::ErrorReport);
        assert!(msg.message.contains("routing"));
    }

    #[test]
    fn factory_creates_verification_result() {
        let msg = MessageFactory::verification_result(
            MissionId::new("m-1"),
            TaskId::new("t-1"),
            true,
            "编译通过",
        );
        assert_eq!(msg.kind, OrchestratorMessageKind::VerificationResult);
        assert!(msg.message.contains("通过"));
    }
}

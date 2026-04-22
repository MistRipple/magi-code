use super::bus::{MessageContext, OrchestratorMessage, OrchestratorMessageBus, OrchestratorMessageKind};
use super::factory::MessageFactory;
use crate::dispatch::DispatchBatchSummary;
use crate::worker_pipeline::WorkerPipelineResult;
use magi_core::{MissionId, TaskId, WorkerId};

pub struct MessageHub {
    bus: OrchestratorMessageBus,
    recent_messages: Vec<OrchestratorMessage>,
    max_recent: usize,
}

impl MessageHub {
    pub fn new(bus: OrchestratorMessageBus) -> Self {
        Self {
            bus,
            recent_messages: Vec::new(),
            max_recent: 200,
        }
    }

    pub fn emit(&mut self, msg: OrchestratorMessage) -> u64 {
        let seq = self.bus.publish(msg.clone());
        self.recent_messages.push(msg);
        if self.recent_messages.len() > self.max_recent {
            self.recent_messages.remove(0);
        }
        seq
    }

    pub fn emit_dispatch_started(
        &mut self,
        mission_id: MissionId,
        batch_id: &str,
        task_count: usize,
    ) {
        let msg = MessageFactory::dispatch_started(mission_id, batch_id, task_count);
        self.emit(msg);
    }

    pub fn emit_dispatch_completed(
        &mut self,
        mission_id: MissionId,
        batch_id: &str,
        summary: &DispatchBatchSummary,
    ) {
        let msg = MessageFactory::dispatch_completed(mission_id, batch_id, summary);
        self.emit(msg);
    }

    pub fn emit_dispatch_cancelled(
        &mut self,
        mission_id: MissionId,
        batch_id: &str,
        reason: &str,
    ) {
        let msg = MessageFactory::dispatch_cancelled(mission_id, batch_id, reason);
        self.emit(msg);
    }

    pub fn emit_worker_lifecycle(
        &mut self,
        result: &WorkerPipelineResult,
        mission_id: MissionId,
    ) {
        let task_id = TaskId::new(result.task_id.clone());
        let worker_id = WorkerId::new(result.worker.clone());

        if result.success {
            let msg = MessageFactory::worker_completed(
                mission_id,
                task_id,
                worker_id,
                &result.summary,
            );
            self.emit(msg);
        } else {
            let error = result.errors.first().map(|s| s.as_str()).unwrap_or(&result.summary);
            let msg = MessageFactory::worker_failed(
                mission_id,
                task_id,
                worker_id,
                error,
            );
            self.emit(msg);
        }
    }

    pub fn emit_mission_created(&mut self, mission_id: MissionId, title: &str) {
        let msg = MessageFactory::mission_created(mission_id, title);
        self.emit(msg);
    }

    pub fn emit_mission_completed(&mut self, mission_id: MissionId, summary: &str) {
        let msg = MessageFactory::mission_completed(mission_id, summary);
        self.emit(msg);
    }

    pub fn emit_mission_failed(&mut self, mission_id: MissionId, reason: &str) {
        let msg = MessageFactory::mission_failed(mission_id, reason);
        self.emit(msg);
    }

    pub fn emit_progress(
        &mut self,
        mission_id: MissionId,
        message: &str,
        percentage: Option<f64>,
    ) {
        let msg = MessageFactory::progress_update(mission_id, message, percentage);
        self.emit(msg);
    }

    pub fn emit_error(&mut self, context: MessageContext, error: &str, source: &str) {
        let msg = MessageFactory::error_report(context, error, source);
        self.emit(msg);
    }

    pub fn recent_messages(&self) -> &[OrchestratorMessage] {
        &self.recent_messages
    }

    pub fn recent_messages_of_kind(
        &self,
        kind: OrchestratorMessageKind,
    ) -> Vec<&OrchestratorMessage> {
        self.recent_messages
            .iter()
            .filter(|m| m.kind == kind)
            .collect()
    }

    pub fn clear_recent(&mut self) {
        self.recent_messages.clear();
    }

    pub fn bus(&self) -> &OrchestratorMessageBus {
        &self.bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use magi_event_bus::InMemoryEventBus;

    fn make_hub() -> MessageHub {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let bus = OrchestratorMessageBus::new(event_bus);
        MessageHub::new(bus)
    }

    #[test]
    fn hub_emits_and_stores_messages() {
        let mut hub = make_hub();
        hub.emit_mission_created(MissionId::new("m-1"), "测试任务");
        hub.emit_progress(MissionId::new("m-1"), "进度 50%", Some(50.0));

        assert_eq!(hub.recent_messages().len(), 2);
    }

    #[test]
    fn hub_filters_by_kind() {
        let mut hub = make_hub();
        hub.emit_mission_created(MissionId::new("m-1"), "任务1");
        hub.emit_mission_created(MissionId::new("m-2"), "任务2");
        hub.emit_progress(MissionId::new("m-1"), "进度", None);

        let created = hub.recent_messages_of_kind(OrchestratorMessageKind::MissionCreated);
        assert_eq!(created.len(), 2);

        let progress = hub.recent_messages_of_kind(OrchestratorMessageKind::ProgressUpdate);
        assert_eq!(progress.len(), 1);
    }

    #[test]
    fn hub_respects_max_recent() {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let bus = OrchestratorMessageBus::new(event_bus);
        let mut hub = MessageHub::new(bus);

        for i in 0..210 {
            hub.emit_progress(MissionId::new("m-1"), &format!("步骤 {}", i), None);
        }

        assert_eq!(hub.recent_messages().len(), 200);
    }

    #[test]
    fn hub_clear_recent() {
        let mut hub = make_hub();
        hub.emit_mission_created(MissionId::new("m-1"), "任务");
        assert_eq!(hub.recent_messages().len(), 1);

        hub.clear_recent();
        assert!(hub.recent_messages().is_empty());
    }

    #[test]
    fn hub_emits_dispatch_lifecycle() {
        let mut hub = make_hub();
        let mid = MissionId::new("m-1");
        hub.emit_dispatch_started(mid.clone(), "batch-1", 3);

        let summary = DispatchBatchSummary {
            total: 3,
            completed: 2,
            failed: 1,
            skipped: 0,
            cancelled: 0,
            running: 0,
            pending: 0,
        };
        hub.emit_dispatch_completed(mid, "batch-1", &summary);

        assert_eq!(hub.recent_messages().len(), 2);
        assert_eq!(
            hub.recent_messages()[0].kind,
            OrchestratorMessageKind::DispatchStarted
        );
        assert_eq!(
            hub.recent_messages()[1].kind,
            OrchestratorMessageKind::DispatchCompleted
        );
    }

    #[test]
    fn hub_emits_worker_lifecycle_success() {
        let mut hub = make_hub();
        let result = WorkerPipelineResult {
            task_id: "task-1".to_string(),
            worker: "backend".to_string(),
            success: true,
            summary: "全部完成".to_string(),
            modified_files: vec![],
            errors: vec![],
            execution_result: None,
        };
        hub.emit_worker_lifecycle(&result, MissionId::new("m-1"));

        let msgs = hub.recent_messages_of_kind(OrchestratorMessageKind::WorkerCompleted);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn hub_emits_worker_lifecycle_failure() {
        let mut hub = make_hub();
        let result = WorkerPipelineResult {
            task_id: "task-1".to_string(),
            worker: "backend".to_string(),
            success: false,
            summary: "执行失败".to_string(),
            modified_files: vec![],
            errors: vec!["编译错误".to_string()],
            execution_result: None,
        };
        hub.emit_worker_lifecycle(&result, MissionId::new("m-1"));

        let msgs = hub.recent_messages_of_kind(OrchestratorMessageKind::WorkerFailed);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].message.contains("编译错误"));
    }
}

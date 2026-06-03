use crate::{EventCategory, EventContext, WorkerRecord, WorkerRuntime, WorkerStage};
use magi_core::{
    ExecutionResultStatus, TaskId, TaskResultKind, TerminationReason, ToolCallId, UtcMillis,
    VerificationStatus, WorkerId, WorkerLifecycleStatus,
};
use magi_skill_runtime::{SkillDispatchObservation, SkillDispatchRoute, SkillDispatchStatus};
use serde::{Deserialize, Serialize};

const SKILL_DISPATCH_NEEDS_APPROVAL_DETAIL: &str =
    "受限访问已拦截该 Skill 工具，请切换为完全访问权限后重试";
const SKILL_DISPATCH_REJECTED_DETAIL: &str = "Skill 工具调用被策略或配置阻断";
const SKILL_DISPATCH_FAILED_DETAIL: &str = "Skill 工具调用失败，请检查工具配置或外接服务状态";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutionReport {
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub stage: WorkerStage,
    pub summary: String,
    pub result_kind: Option<TaskResultKind>,
    pub termination_reason: Option<TerminationReason>,
    pub verification_status: VerificationStatus,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerExecutionFinalReport {
    pub summary: String,
    pub result_kind: Option<TaskResultKind>,
    pub termination_reason: Option<TerminationReason>,
    pub verification_status: VerificationStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerToolInvocation {
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub status: ExecutionResultStatus,
    pub observed_at: UtcMillis,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerSkillDispatchObservation {
    pub worker_id: WorkerId,
    pub task_id: TaskId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub route: Option<SkillDispatchRoute>,
    pub binding_id: Option<String>,
    pub status: SkillDispatchStatus,
    pub detail: String,
    pub observed_at: UtcMillis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillDispatchSummary {
    pub total_dispatches: usize,
    pub builtin_dispatches: usize,
    pub bridge_dispatches: usize,
    pub succeeded_dispatches: usize,
    pub rejected_dispatches: usize,
    pub failed_dispatches: usize,
}

impl WorkerRuntime {
    pub fn finish(&self, worker_id: &WorkerId, summary: impl Into<String>) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        let worker = self.transition(
            worker_id,
            Some(task_id.clone()),
            WorkerLifecycleStatus::Finished,
            WorkerStage::Finish,
        )?;
        self.append_report(
            worker_id.clone(),
            task_id,
            WorkerStage::Finish,
            summary.into(),
            Some(TaskResultKind::Success),
            Some(TerminationReason::Completed),
            VerificationStatus::Passed,
        );
        Some(worker)
    }

    pub fn fail(&self, worker_id: &WorkerId, summary: impl Into<String>) -> Option<WorkerRecord> {
        let task_id = self.current_task_id(worker_id)?;
        let worker = self.transition(
            worker_id,
            Some(task_id.clone()),
            WorkerLifecycleStatus::Failed,
            WorkerStage::Finish,
        )?;
        self.append_report(
            worker_id.clone(),
            task_id,
            WorkerStage::Finish,
            summary.into(),
            Some(TaskResultKind::Failure),
            Some(TerminationReason::Failed),
            VerificationStatus::Failed,
        );
        Some(worker)
    }

    pub fn record_review_note(
        &self,
        worker_id: &WorkerId,
        summary: impl Into<String>,
    ) -> Option<WorkerExecutionReport> {
        let task_id = self.current_task_id(worker_id)?;
        Some(self.append_report(
            worker_id.clone(),
            task_id,
            WorkerStage::Review,
            summary.into(),
            None,
            None,
            VerificationStatus::Pending,
        ))
    }

    pub fn record_verification(
        &self,
        worker_id: &WorkerId,
        verification_status: VerificationStatus,
        summary: impl Into<String>,
    ) -> Option<WorkerExecutionReport> {
        let task_id = self.current_task_id(worker_id)?;
        Some(self.append_report(
            worker_id.clone(),
            task_id,
            WorkerStage::Verify,
            summary.into(),
            None,
            None,
            verification_status,
        ))
    }

    pub fn record_repair_note(
        &self,
        worker_id: &WorkerId,
        summary: impl Into<String>,
    ) -> Option<WorkerExecutionReport> {
        let task_id = self.current_task_id(worker_id)?;
        Some(self.append_report(
            worker_id.clone(),
            task_id,
            WorkerStage::Repair,
            summary.into(),
            Some(TaskResultKind::Partial),
            Some(TerminationReason::Blocked),
            VerificationStatus::Pending,
        ))
    }

    pub fn reports(&self) -> Vec<WorkerExecutionReport> {
        self.reports
            .read()
            .expect("worker reports read lock poisoned")
            .clone()
    }

    pub fn observe_tool_invocation(
        &self,
        worker_id: &WorkerId,
        tool_call_id: ToolCallId,
        tool_name: impl Into<String>,
        status: ExecutionResultStatus,
    ) -> Option<WorkerToolInvocation> {
        let task_id = self.current_task_id(worker_id)?;
        let record = WorkerToolInvocation {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.into(),
            status,
            observed_at: UtcMillis::now(),
        };
        self.tool_invocations
            .write()
            .expect("worker tool invocation write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "worker.tool.observed",
            EventCategory::Audit,
            EventContext {
                task_id: Some(task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": task_id.to_string(),
                "tool_call_id": tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "status": format!("{:?}", record.status)
            }),
        );
        Some(record)
    }

    pub fn tool_invocations(&self) -> Vec<WorkerToolInvocation> {
        self.tool_invocations
            .read()
            .expect("worker tool invocation read lock poisoned")
            .clone()
    }

    pub fn observe_skill_dispatch(
        &self,
        worker_id: &WorkerId,
        observation: SkillDispatchObservation,
    ) -> Option<WorkerSkillDispatchObservation> {
        let task_id = self.current_task_id(worker_id)?;
        let record = public_worker_skill_dispatch_observation(WorkerSkillDispatchObservation {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            tool_call_id: observation.tool_call_id.clone(),
            tool_name: observation.tool_name.clone(),
            route: observation.route,
            binding_id: observation.binding_id.clone(),
            status: observation.status,
            detail: observation.detail.clone(),
            observed_at: UtcMillis::now(),
        });
        self.skill_dispatches
            .write()
            .expect("worker skill dispatch write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "worker.skill_dispatch.observed",
            EventCategory::Audit,
            EventContext {
                task_id: Some(task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": task_id.to_string(),
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "route": record.route.map(|route| format!("{:?}", route)),
                "binding_id": record.binding_id,
                "status": format!("{:?}", record.status),
                "detail": record.detail
            }),
        );
        Some(record)
    }

    pub(crate) fn append_report(
        &self,
        worker_id: WorkerId,
        task_id: TaskId,
        stage: WorkerStage,
        summary: String,
        result_kind: Option<TaskResultKind>,
        termination_reason: Option<TerminationReason>,
        verification_status: VerificationStatus,
    ) -> WorkerExecutionReport {
        let report = WorkerExecutionReport {
            worker_id: worker_id.clone(),
            task_id: task_id.clone(),
            stage,
            summary: summary.clone(),
            result_kind,
            termination_reason,
            verification_status,
            created_at: UtcMillis::now(),
        };
        self.reports
            .write()
            .expect("worker reports write lock poisoned")
            .push(report.clone());
        self.publish_with_category(
            "worker.reported",
            EventCategory::Audit,
            EventContext {
                task_id: Some(task_id.clone()),
                ..EventContext::default()
            },
            serde_json::json!({
                "worker_id": worker_id.to_string(),
                "task_id": task_id.to_string(),
                "stage": format!("{:?}", stage),
                "summary": summary,
                "status": report_status(&report),
                "result_kind": report.result_kind.map(|value| format!("{:?}", value)),
                "termination_reason": report.termination_reason.map(|value| format!("{:?}", value)),
                "verification_status": format!("{:?}", report.verification_status)
            }),
        );
        report
    }

    pub(crate) fn latest_report_for(
        &self,
        worker_id: &WorkerId,
        task_id: &TaskId,
        stage: WorkerStage,
    ) -> Option<WorkerExecutionReport> {
        self.reports
            .read()
            .expect("worker reports read lock poisoned")
            .iter()
            .rev()
            .find(|report| {
                &report.worker_id == worker_id
                    && &report.task_id == task_id
                    && report.stage == stage
            })
            .cloned()
    }
}

pub(crate) fn public_worker_skill_dispatch_observation(
    mut record: WorkerSkillDispatchObservation,
) -> WorkerSkillDispatchObservation {
    record.detail = public_skill_dispatch_detail(record.status, &record.detail);
    record
}

fn public_skill_dispatch_detail(status: SkillDispatchStatus, raw_detail: &str) -> String {
    match status {
        SkillDispatchStatus::Succeeded => raw_detail.to_string(),
        SkillDispatchStatus::NeedsApproval => SKILL_DISPATCH_NEEDS_APPROVAL_DETAIL.to_string(),
        SkillDispatchStatus::Rejected => SKILL_DISPATCH_REJECTED_DETAIL.to_string(),
        SkillDispatchStatus::Failed => SKILL_DISPATCH_FAILED_DETAIL.to_string(),
    }
}

pub(crate) fn derive_final_report(
    tool_invocations: &[WorkerToolInvocation],
    skill_dispatches: &[SkillDispatchObservation],
) -> WorkerExecutionFinalReport {
    let tool_failed = tool_invocations.iter().any(|record| {
        matches!(
            record.status,
            ExecutionResultStatus::Failed
                | ExecutionResultStatus::Rejected
                | ExecutionResultStatus::NeedsApproval
                | ExecutionResultStatus::Cancelled
        )
    });
    let skill_failed = skill_dispatches.iter().any(|record| {
        matches!(
            record.status,
            SkillDispatchStatus::Failed
                | SkillDispatchStatus::NeedsApproval
                | SkillDispatchStatus::Rejected
        )
    });

    if tool_failed || skill_failed {
        WorkerExecutionFinalReport {
            summary: "loopback execution completed with issues".to_string(),
            result_kind: Some(TaskResultKind::Failure),
            termination_reason: Some(TerminationReason::Failed),
            verification_status: VerificationStatus::Failed,
        }
    } else {
        WorkerExecutionFinalReport {
            summary: "loopback execution completed".to_string(),
            result_kind: Some(TaskResultKind::Success),
            termination_reason: Some(TerminationReason::Completed),
            verification_status: VerificationStatus::Passed,
        }
    }
}

fn report_status(report: &WorkerExecutionReport) -> &'static str {
    match report.termination_reason {
        Some(TerminationReason::Completed) => "finished",
        Some(TerminationReason::Failed) => "failed",
        Some(TerminationReason::Cancelled) => "cancelled",
        Some(TerminationReason::Blocked) => "blocked",
        None => match report.stage {
            WorkerStage::Execute => "running",
            WorkerStage::Review => "reviewing",
            WorkerStage::Verify => "verifying",
            WorkerStage::Repair => "repairing",
            WorkerStage::Finish => "finished",
        },
    }
}

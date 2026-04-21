use magi_core::{AssignmentId, MissionId, TaskId, WorkerId};
use serde::{Deserialize, Serialize};

use crate::{GovernanceAction, GovernanceDecision, ToolKind, WorkerControlKind};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceTarget {
    Tool {
        tool_name: String,
        tool_kind: ToolKind,
    },
    Sandbox {
        command: String,
        working_directory: String,
    },
    PathAccess {
        absolute_path: String,
    },
    WorkerControl {
        action: WorkerControlKind,
        worker_id: Option<WorkerId>,
        mission_id: Option<MissionId>,
        assignment_id: Option<AssignmentId>,
        task_id: Option<TaskId>,
    },
}

impl GovernanceTarget {
    fn label(&self) -> String {
        match self {
            Self::Tool { tool_name, .. } => format!("tool:{tool_name}"),
            Self::Sandbox { command, .. } => format!("sandbox:{command}"),
            Self::PathAccess { absolute_path } => format!("path:{absolute_path}"),
            Self::WorkerControl { action, .. } => {
                format!("worker_control:{}", worker_control_kind_label(action))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDecisionTrace {
    pub target: GovernanceTarget,
    pub decision: GovernanceDecision,
    pub action: GovernanceAction,
    pub summary: String,
}

impl GovernanceDecisionTrace {
    pub fn new(target: GovernanceTarget, decision: GovernanceDecision) -> Self {
        let action = decision.action();
        let summary = match &decision.reason {
            Some(reason) => format!(
                "{} -> {} ({reason})",
                target.label(),
                decision.outcome_label()
            ),
            None => format!("{} -> {}", target.label(), decision.outcome_label()),
        };

        Self {
            target,
            decision,
            action,
            summary,
        }
    }
}

fn worker_control_kind_label(kind: &WorkerControlKind) -> &'static str {
    match kind {
        WorkerControlKind::Execute => "execute",
        WorkerControlKind::Review => "review",
        WorkerControlKind::Verify => "verify",
        WorkerControlKind::Repair => "repair",
        WorkerControlKind::RepairRetry => "repair_retry",
        WorkerControlKind::Finish => "finish",
        WorkerControlKind::Fail => "fail",
    }
}

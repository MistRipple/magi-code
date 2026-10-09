//! 工作流核心的唯一类型化边界。
//!
//! 核心只能返回动作意图；daemon 负责预算、授权、持久化、执行和终态提交。插件不
//! 能通过这个合同直接写 TaskStore、审批记录或 canonical Turn。

use magi_plugin_runtime::RunCancellation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum WorkflowAction {
    ModelRequest {
        action_id: String,
        model: Option<String>,
        system: String,
        context_refs: Vec<String>,
        tools: Vec<String>,
    },
    ToolCall {
        action_id: String,
        tool: String,
        input: Value,
    },
    DispatchTask {
        action_id: String,
        role: String,
        instruction: String,
    },
    WaitUser {
        action_id: String,
        question: String,
    },
    SaveStage {
        action_id: String,
        stage: String,
        checkpoint: Value,
    },
    Complete {
        action_id: String,
        summary: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDecision {
    pub checkpoint_version: u32,
    pub stage: String,
    pub action: WorkflowAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowInput {
    pub run_id: String,
    pub attempt_id: String,
    pub stage: String,
    pub user_input: String,
    pub model_result: Option<Value>,
    pub tool_result: Option<Value>,
    pub context_summary: Value,
}

pub trait WorkflowCore: Send + Sync {
    fn decide<'a>(
        &'a self,
        input: WorkflowInput,
        cancellation: &'a RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowDecision, WorkflowError>> + Send + 'a>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("工作流动作无效：{message}")]
pub struct WorkflowError {
    pub message: String,
}

impl WorkflowDecision {
    pub fn validate(
        &self,
        expected_run_id: &str,
        input: &WorkflowInput,
    ) -> Result<(), WorkflowError> {
        if input.run_id != expected_run_id
            || input.attempt_id.is_empty()
            || input.stage.is_empty()
            || self.stage.is_empty()
            || self.checkpoint_version == 0
        {
            return Err(WorkflowError {
                message: "工作流身份、阶段或检查点版本无效".into(),
            });
        }
        let id = match &self.action {
            WorkflowAction::ModelRequest { action_id, .. }
            | WorkflowAction::ToolCall { action_id, .. }
            | WorkflowAction::DispatchTask { action_id, .. }
            | WorkflowAction::WaitUser { action_id, .. }
            | WorkflowAction::SaveStage { action_id, .. }
            | WorkflowAction::Complete { action_id, .. } => action_id,
        };
        if id.is_empty() || id.len() > 128 || id != &input.attempt_id {
            return Err(WorkflowError {
                message: "动作 ID 必须绑定当前 attempt".into(),
            });
        }
        Ok(())
    }
}

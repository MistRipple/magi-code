//! 子代理执行 Turn 的不可变来源；执行状态、Step 和工具仍在 canonical Turn 中。
use crate::CanonicalTurn;
use magi_core::{DomainError, DomainResult, MissionId, TaskId, ThreadId, WorkerId};
use serde::{Deserialize, Serialize};

pub(crate) const AGENT_EXECUTION_METADATA_KEY: &str = "agentExecution";
pub(crate) const AGENT_ACTIVE_SKILL_METADATA_KEY: &str = "agentActiveSkillId";

/// 可信宿主查询 TaskStore 的原转交根 Task（含 activation 与唯一转交记录）。
/// Ok(None) 仅表示未转交；来源缺失或查询不可用必须返回错误。
/// 查询不得回调 SessionStore 或执行写入；canonical 事务持有 Session 锁时会使用它。
pub type AgentRunTransferResolver =
    dyn Fn(&TaskId) -> DomainResult<Option<magi_core::Task>> + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTurnBinding {
    pub task_id: TaskId,
    pub root_task_id: TaskId,
    pub parent_task_id: TaskId,
    pub parent_turn_id: String,
    pub thread_id: ThreadId,
    pub mission_id: MissionId,
    pub worker_id: WorkerId,
    pub role_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_turn_id: Option<String>,
    /// 独立拥有方显式继续的幂等身份；原父任务与父 Turn 保持原始来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_operation_id: Option<String>,
}

impl AgentTurnBinding {
    pub fn turn_id(&self) -> String {
        if let Some(operation) = self.continuation_operation_id.as_deref() {
            format!(
                "turn-agent-continue-{}:{}:{}",
                self.task_id.as_str().len(),
                self.task_id,
                operation
            )
        } else if self.previous_turn_id.is_some() {
            Self::recovery_turn_id(&self.task_id, &self.parent_turn_id)
        } else {
            Self::turn_id_for_task(&self.task_id)
        }
    }

    fn turn_id_for_task(task_id: &TaskId) -> String {
        format!("turn-agent-{}", task_id.as_str())
    }

    fn recovery_turn_id(task_id: &TaskId, parent_turn_id: &str) -> String {
        // 长度前缀避免 Task/Turn 分隔符组合碰撞；来源仍由 canonical binding 校验。
        format!(
            "turn-agent-resume-{}:{}:{}",
            task_id.as_str().len(),
            task_id,
            parent_turn_id
        )
    }

    pub(crate) fn validate(&self, turn: &CanonicalTurn) -> DomainResult<()> {
        if self.task_id.as_str().trim().is_empty()
            || self.root_task_id.as_str().trim().is_empty()
            || self.parent_task_id.as_str().trim().is_empty()
            || self.thread_id.as_str().trim().is_empty()
            || self.mission_id.as_str().trim().is_empty()
            || self.worker_id.as_str().trim().is_empty()
            || self.role_id.trim().is_empty()
            || self.role_id == crate::ORCHESTRATOR_ROLE_ID
            || self.parent_turn_id.trim().is_empty()
            || self.task_id == self.root_task_id
            || self.task_id == self.parent_task_id
            || self.parent_turn_id == turn.turn_id
            || self
                .continuation_operation_id
                .as_ref()
                .is_some_and(|operation| {
                    !magi_core::agent_operation_id_is_valid(operation)
                        || self.previous_turn_id.is_none()
                })
            || self
                .previous_turn_id
                .as_ref()
                .is_some_and(|previous| previous.trim().is_empty() || previous == &turn.turn_id)
            || self.turn_id() != turn.turn_id
            || turn.items.iter().any(|item| {
                item.source_thread_id != self.thread_id
                    || item
                        .worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        != Some(&self.task_id)
            })
            || turn.model_steps.iter().any(|step| {
                step.task_id.as_ref() != Some(&self.task_id)
                    || step.thread_id != self.thread_id
                    || !step.input_request_ids.is_empty()
            })
        {
            return Err(DomainError::InvalidState {
                message: "Agent Turn 与 Task、父任务、Thread 或模型输入来源不匹配".into(),
            });
        }
        Ok(())
    }
}

impl CanonicalTurn {
    /// Agent 当前 Skill 的持久事实；主线 branch 和执行计划只提供初始值/投影。
    pub fn agent_active_skill_id(&self) -> DomainResult<Option<String>> {
        match self.metadata.get(AGENT_ACTIVE_SKILL_METADATA_KEY) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(id)) if !id.trim().is_empty() => Ok(Some(id.clone())),
            Some(_) => Err(DomainError::InvalidState {
                message: "Agent Turn 的 Skill 身份无效".into(),
            }),
        }
    }

    pub fn agent_turn_binding(&self) -> DomainResult<Option<AgentTurnBinding>> {
        let binding: Option<AgentTurnBinding> = self
            .metadata
            .get(AGENT_EXECUTION_METADATA_KEY)
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| DomainError::InvalidState {
                message: format!("Agent Turn 绑定无效：{error}"),
            })?;
        if let Some(binding) = &binding {
            binding.validate(self)?;
            self.agent_active_skill_id()?;
        }
        Ok(binding)
    }
}

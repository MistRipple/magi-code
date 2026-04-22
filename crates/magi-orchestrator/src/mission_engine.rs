use crate::dispatch::DispatchRoutingService;
use crate::message::{MessageHub, OrchestratorMessageBus};
use crate::orchestration_loop::{
    OrchestrationLoopConfig, OrchestrationLoopController, OrchestrationLoopResult,
    OrchestrationOutcome,
};
use crate::verification_runner::{VerificationResult, VerificationRunner};
use crate::{OrchestratedExecutionRuntime, OrchestratorControlPlane};
use magi_bridge_client::assignment_dispatch::{
    AssignmentDispatchPayload, AssignmentDispatchDecision, decide_assignment_dispatch,
};
use magi_core::{AssignmentId, MissionId, UtcMillis};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct MissionEngineConfig {
    pub session_id: String,
    pub workspace_root: Option<String>,
    pub enable_verification: bool,
    pub max_dispatch_waves: u32,
    pub allow_busy_fallback: bool,
}

impl Default for MissionEngineConfig {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            workspace_root: None,
            enable_verification: true,
            max_dispatch_waves: 10,
            allow_busy_fallback: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionPhase {
    Analyzing,
    Planning,
    Dispatching,
    Verifying,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionEngineResult {
    pub mission_id: String,
    pub phase: MissionPhase,
    pub dispatch_result: Option<OrchestrationLoopResult>,
    pub verification_result: Option<VerificationResult>,
    pub summary: String,
}

pub struct MissionEngine {
    config: MissionEngineConfig,
    message_hub: MessageHub,
}

impl MissionEngine {
    pub fn new(
        config: MissionEngineConfig,
        bus: OrchestratorMessageBus,
    ) -> Self {
        Self {
            config,
            message_hub: MessageHub::new(bus),
        }
    }

    pub fn execute_from_dispatch_payload(
        &mut self,
        payload: &AssignmentDispatchPayload,
        control_plane: &OrchestratorControlPlane,
        runtime: &OrchestratedExecutionRuntime,
        routing: DispatchRoutingService,
    ) -> MissionEngineResult {
        let mission_id = MissionId::new(format!("mission-{}", UtcMillis::now().0));
        let assignment_id = AssignmentId::new(format!("assignment-{}", UtcMillis::now().0));

        self.message_hub.emit_progress(
            mission_id.clone(),
            "开始分析任务",
            Some(0.0),
        );

        let loop_config = OrchestrationLoopConfig {
            session_id: self.config.session_id.clone(),
            allow_busy_fallback: self.config.allow_busy_fallback,
            max_dispatch_waves: self.config.max_dispatch_waves,
        };
        let controller = OrchestrationLoopController::new(loop_config);

        self.message_hub.emit_progress(
            mission_id.clone(),
            "派发任务执行中",
            Some(30.0),
        );

        let dispatch_result = controller.execute_assignment_dispatch(
            payload,
            &mission_id,
            &assignment_id,
            control_plane,
            runtime,
            routing,
            &mut self.message_hub,
        );

        let verification_result = if self.config.enable_verification
            && dispatch_result.outcome == OrchestrationOutcome::Completed
        {
            self.message_hub.emit_progress(
                mission_id.clone(),
                "验证执行结果",
                Some(80.0),
            );
            self.run_verification(&mission_id)
        } else {
            None
        };

        let phase = self.determine_final_phase(&dispatch_result, &verification_result);
        let summary = self.build_summary(&dispatch_result, &verification_result);

        self.message_hub.emit_progress(
            mission_id.clone(),
            &summary,
            Some(100.0),
        );

        MissionEngineResult {
            mission_id: mission_id.to_string(),
            phase,
            dispatch_result: Some(dispatch_result),
            verification_result,
            summary,
        }
    }

    pub fn execute_from_llm_text(
        &mut self,
        llm_text: &str,
        round: u32,
        request_id: Option<&str>,
        control_plane: &OrchestratorControlPlane,
        runtime: &OrchestratedExecutionRuntime,
        routing: DispatchRoutingService,
    ) -> Option<MissionEngineResult> {
        let decision = decide_assignment_dispatch(llm_text, request_id, round, false);

        match decision {
            AssignmentDispatchDecision::Dispatch { request, .. } => {
                Some(self.execute_from_dispatch_payload(
                    &request.payload,
                    control_plane,
                    runtime,
                    routing,
                ))
            }
            AssignmentDispatchDecision::BlockedTerminalHandoff { .. } => None,
            AssignmentDispatchDecision::None => None,
        }
    }

    fn run_verification(&mut self, mission_id: &MissionId) -> Option<VerificationResult> {
        let workspace_root = self.config.workspace_root.as_deref()?;
        let runner = VerificationRunner::new(workspace_root, None);
        let result = runner.run_verification("mission-verification", None);

        let passed = result.success;
        self.message_hub.emit(
            crate::message::MessageFactory::verification_result(
                mission_id.clone(),
                magi_core::TaskId::new("verification"),
                passed,
                &result.summary,
            ),
        );

        Some(result)
    }

    fn determine_final_phase(
        &self,
        dispatch: &OrchestrationLoopResult,
        verification: &Option<VerificationResult>,
    ) -> MissionPhase {
        match dispatch.outcome {
            OrchestrationOutcome::NoTasks => MissionPhase::Failed,
            OrchestrationOutcome::Failed => MissionPhase::Failed,
            OrchestrationOutcome::WaveLimitReached => MissionPhase::Failed,
            OrchestrationOutcome::Completed | OrchestrationOutcome::PartiallyCompleted => {
                if let Some(v) = verification {
                    if v.success {
                        MissionPhase::Completed
                    } else {
                        MissionPhase::Failed
                    }
                } else {
                    if dispatch.outcome == OrchestrationOutcome::Completed {
                        MissionPhase::Completed
                    } else {
                        MissionPhase::Completed
                    }
                }
            }
        }
    }

    fn build_summary(
        &self,
        dispatch: &OrchestrationLoopResult,
        verification: &Option<VerificationResult>,
    ) -> String {
        let dispatch_summary = format!(
            "{} 个任务完成 / {} 个失败（{} 轮派发）",
            dispatch.completed_tasks, dispatch.failed_tasks, dispatch.total_waves
        );

        if let Some(v) = verification {
            format!(
                "{}，验证{}：{}",
                dispatch_summary,
                if v.success { "通过" } else { "失败" },
                v.summary
            )
        } else {
            dispatch_summary
        }
    }

    pub fn message_hub(&self) -> &MessageHub {
        &self.message_hub
    }

    pub fn message_hub_mut(&mut self) -> &mut MessageHub {
        &mut self.message_hub
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::OrchestratorService;
    use magi_event_bus::InMemoryEventBus;

    fn make_engine() -> MissionEngine {
        let bus = OrchestratorMessageBus::new(Arc::new(InMemoryEventBus::new(64)));
        MissionEngine::new(MissionEngineConfig::default(), bus)
    }

    #[test]
    fn engine_config_defaults() {
        let config = MissionEngineConfig::default();
        assert!(config.enable_verification);
        assert_eq!(config.max_dispatch_waves, 10);
    }

    #[test]
    fn engine_constructs() {
        let engine = make_engine();
        assert!(engine.message_hub().recent_messages().is_empty());
    }

    #[test]
    fn phase_serializes() {
        let phase = MissionPhase::Dispatching;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"dispatching\"");
    }

    #[test]
    fn result_serializes() {
        let result = MissionEngineResult {
            mission_id: "m-1".to_string(),
            phase: MissionPhase::Completed,
            dispatch_result: None,
            verification_result: None,
            summary: "测试完成".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("completed"));
        assert!(json.contains("测试完成"));
    }

    #[test]
    fn execute_from_llm_text_returns_none_for_plain_text() {
        let mut engine = make_engine();
        let bus = Arc::new(InMemoryEventBus::new(64));
        let service = OrchestratorService::new(bus.clone());
        let cp = service.control_plane();

        let result = engine.execute_from_llm_text(
            "这是普通文本，没有 dispatch 内容",
            1,
            None,
            &cp,
            // runtime 不会被调用因为不会 dispatch
            &unsafe_empty_runtime(&service, bus.clone()),
            make_routing(),
        );
        assert!(result.is_none());
    }

    fn make_routing() -> DispatchRoutingService {
        DispatchRoutingService::new(
            vec!["backend".to_string()],
            std::collections::HashMap::new(),
            60_000,
        )
    }

    fn unsafe_empty_runtime(
        service: &OrchestratorService,
        bus: Arc<InMemoryEventBus>,
    ) -> OrchestratedExecutionRuntime {
        use magi_worker_runtime::WorkerRuntime;
        use magi_tool_runtime::ToolRegistry;
        use magi_skill_runtime::SkillDispatchRuntime;
        use magi_governance::GovernanceService;
        use magi_bridge_client::BridgeDispatchRuntime;
        let worker_runtime = WorkerRuntime::new_compare(bus.clone());
        let governance = Arc::new(GovernanceService::default());
        let tool_registry = ToolRegistry::new(governance, bus.clone());
        let bridge_runtime = BridgeDispatchRuntime::new();
        let skill_dispatch_runtime = SkillDispatchRuntime::new(tool_registry.clone(), bridge_runtime);
        service.execution_runtime(worker_runtime, tool_registry, skill_dispatch_runtime)
    }
}

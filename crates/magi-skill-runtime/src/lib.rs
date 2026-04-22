mod dispatch;
mod observation;
mod routing;
mod validation;

use magi_bridge_client::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeDispatchAction, BridgeClientError,
    BridgeDispatchResult, BridgeDispatchRuntime, BridgeErrorLayer,
};
use magi_tool_runtime::{
    ToolExecutionContext, ToolExecutionOutput, ToolExecutionPolicy, ToolRegistry,
};
use magi_core::{ApprovalRequirement, RiskLevel, ToolCallId};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub category: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillPromptInjection {
    pub skill_id: String,
    pub heading: String,
    pub body: String,
    pub priority: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomToolBinding {
    pub binding_id: String,
    pub tool_name: String,
    pub description: String,
    pub bridge_kind: BridgeBindingKind,
    pub dispatch_action: BridgeDispatchAction,
    pub bridge_target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub skill_id: String,
    pub title: String,
    pub instruction: String,
    pub metadata: SkillMetadata,
    pub allowed_tools: Vec<String>,
    pub custom_tool_bindings: Vec<CustomToolBinding>,
    pub prompt_priority: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSelection {
    pub skill_ids: Vec<String>,
    pub requested_tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillPolicyDecision {
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedSkillContext {
    pub skill_ids: Vec<String>,
    pub prompt_injections: Vec<SkillPromptInjection>,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub custom_tool_bindings: Vec<CustomToolBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillToolRuntimePlan {
    pub skill_ids: Vec<String>,
    pub tool_policy: ToolExecutionPolicy,
    pub routing: SkillToolRoutingSummary,
    pub prompt_injections: Vec<SkillPromptInjection>,
    pub custom_tool_bindings: Vec<CustomToolBinding>,
    pub bridge_dispatch_plan: BridgeBindingDispatchPlan,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillToolRoutingSummary {
    pub requested_builtin_tools: Vec<String>,
    pub requested_bridge_tool_names: Vec<String>,
    pub requested_bridge_binding_ids: Vec<String>,
    pub denied_requested_tools: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillDispatchRoute {
    Builtin,
    Bridge,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillDispatchStatus {
    Succeeded,
    Failed,
    Rejected,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillDispatchErrorKind {
    UnknownRequestedTool,
    AmbiguousBridgeBinding,
    MissingBridgeBinding,
    BridgeError,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillDispatchInput {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub binding_id: Option<String>,
    pub payload: String,
    pub approval_requirement: ApprovalRequirement,
    pub risk_level: RiskLevel,
    pub context: ToolExecutionContext,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SkillDispatchResult {
    Builtin {
        output: ToolExecutionOutput,
    },
    Bridge {
        output: BridgeDispatchResult,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillDispatchObservation {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub route: Option<SkillDispatchRoute>,
    pub binding_id: Option<String>,
    pub bridge_kind: Option<BridgeBindingKind>,
    pub dispatch_action: Option<BridgeDispatchAction>,
    pub status: SkillDispatchStatus,
    pub error_kind: Option<SkillDispatchErrorKind>,
    pub bridge_error_layer: Option<BridgeErrorLayer>,
    pub bridge_error_message: Option<String>,
    pub detail: String,
}

#[derive(Debug)]
pub struct SkillDispatchExecutionOutcome {
    pub observation: SkillDispatchObservation,
    pub result: Result<SkillDispatchResult, SkillDispatchError>,
}

#[derive(Debug)]
pub enum SkillDispatchError {
    UnknownRequestedTool { tool_name: String },
    AmbiguousBridgeBinding { tool_name: String, binding_ids: Vec<String> },
    MissingBridgeBinding { tool_name: String, binding_id: String },
    Bridge(BridgeClientError),
}

#[derive(Clone)]
pub struct SkillDispatchRuntime {
    tool_registry: ToolRegistry,
    bridge_runtime: BridgeDispatchRuntime,
}

#[derive(Clone, Debug, Default)]
pub struct SkillRegistry {
    skills: Arc<RwLock<HashMap<String, SkillDefinition>>>,
}

#[derive(Clone, Debug, Default)]
pub struct SkillRuntime {
    registry: SkillRegistry,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        self.skills.write().expect("skill registry write lock poisoned").clear();
    }

    pub fn register(&self, skill: SkillDefinition) {
        self.skills
            .write()
            .expect("skill registry write lock poisoned")
            .insert(skill.skill_id.clone(), validation::normalize_skill(skill));
    }

    pub fn get(&self, skill_id: &str) -> Option<SkillDefinition> {
        self.skills
            .read()
            .expect("skill registry read lock poisoned")
            .get(skill_id)
            .cloned()
    }

    pub fn list(&self) -> Vec<SkillDefinition> {
        let mut skills = self
            .skills
            .read()
            .expect("skill registry read lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
        skills
    }

    pub fn is_tool_allowed(&self, skill_id: &str, tool_name: &str) -> bool {
        self.get(skill_id).is_some_and(|skill| {
            skill.allowed_tools.iter().any(|tool| tool == tool_name)
                || skill
                    .custom_tool_bindings
                    .iter()
                    .any(|binding| binding.tool_name == tool_name)
        })
    }

    pub fn resolve_context(&self, selection: &SkillSelection) -> ResolvedSkillContext {
        let mut selected_skills = selection
            .skill_ids
            .iter()
            .filter_map(|skill_id| self.get(skill_id))
            .collect::<Vec<_>>();
        selected_skills.sort_by(|left, right| {
            left.prompt_priority
                .cmp(&right.prompt_priority)
                .then_with(|| left.skill_id.cmp(&right.skill_id))
        });

        let mut custom_tool_bindings = selected_skills
            .iter()
            .flat_map(|skill| skill.custom_tool_bindings.clone())
            .collect::<Vec<_>>();
        custom_tool_bindings.sort_by(|left, right| {
            left.binding_id
                .cmp(&right.binding_id)
                .then_with(|| left.tool_name.cmp(&right.tool_name))
        });
        custom_tool_bindings.dedup_by(|left, right| left.binding_id == right.binding_id);
        let routing =
            routing::classify_requested_tools(&selection.requested_tools, &custom_tool_bindings);
        let policy =
            validation::evaluate_policy(&selected_skills, &routing.requested_builtin_tools);

        let prompt_injections = selected_skills
            .into_iter()
            .map(|skill| SkillPromptInjection {
                skill_id: skill.skill_id.clone(),
                heading: skill.title,
                body: skill.instruction,
                priority: skill.prompt_priority,
            })
            .collect::<Vec<_>>();

        ResolvedSkillContext {
            skill_ids: selection.skill_ids.clone(),
            prompt_injections,
            allowed_tools: policy.allowed_tools,
            denied_tools: policy.denied_tools,
            custom_tool_bindings,
        }
    }

    pub fn build_tool_runtime_plan(&self, selection: &SkillSelection) -> SkillToolRuntimePlan {
        let resolved = self.resolve_context(selection);
        let mut routing = routing::classify_requested_tools(
            &selection.requested_tools,
            &resolved.custom_tool_bindings,
        );
        let allowed_builtin_tools = if selection.requested_tools.is_empty() {
            resolved.allowed_tools.clone()
        } else {
            routing
                .requested_builtin_tools
                .iter()
                .filter(|tool_name| {
                    resolved
                        .allowed_tools
                        .iter()
                        .any(|allowed_tool| allowed_tool == *tool_name)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let denied_builtin_tools = if selection.requested_tools.is_empty() {
            resolved.denied_tools.clone()
        } else {
            routing
                .requested_builtin_tools
                .iter()
                .filter(|tool_name| {
                    !resolved
                        .allowed_tools
                        .iter()
                        .any(|allowed_tool| allowed_tool == *tool_name)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        routing.denied_requested_tools = denied_builtin_tools.clone();
        SkillToolRuntimePlan {
            skill_ids: resolved.skill_ids.clone(),
            tool_policy: ToolExecutionPolicy {
                source_skill_ids: resolved.skill_ids.clone(),
                allowed_tool_names: allowed_builtin_tools,
                denied_tool_names: denied_builtin_tools,
            },
            routing: routing.clone(),
            prompt_injections: resolved.prompt_injections,
            bridge_dispatch_plan: routing::build_bridge_dispatch_plan(
                &resolved.skill_ids,
                &resolved.custom_tool_bindings,
                &routing,
            ),
            custom_tool_bindings: resolved.custom_tool_bindings,
        }
    }
}

impl SkillRuntime {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> SkillRegistry {
        self.registry.clone()
    }

    pub fn resolve(&self, selection: SkillSelection) -> ResolvedSkillContext {
        self.registry.resolve_context(&selection)
    }

    pub fn build_tool_runtime_plan(&self, selection: SkillSelection) -> SkillToolRuntimePlan {
        self.registry.build_tool_runtime_plan(&selection)
    }

    pub fn is_tool_allowed(&self, skill_ids: &[String], tool_name: &str) -> bool {
        self.resolve(SkillSelection {
            skill_ids: skill_ids.to_vec(),
            requested_tools: vec![tool_name.to_string()],
        })
        .allowed_tools
        .iter()
        .any(|tool| tool == tool_name)
    }
}

impl SkillDispatchRuntime {
    pub fn new(tool_registry: ToolRegistry, bridge_runtime: BridgeDispatchRuntime) -> Self {
        Self {
            tool_registry,
            bridge_runtime,
        }
    }

    pub fn dispatch(
        &self,
        plan: &SkillToolRuntimePlan,
        input: SkillDispatchInput,
    ) -> Result<SkillDispatchResult, SkillDispatchError> {
        dispatch::execute_dispatch(self, plan, input)
    }

    pub fn dispatch_observed(
        &self,
        plan: &SkillToolRuntimePlan,
        input: SkillDispatchInput,
    ) -> SkillDispatchExecutionOutcome {
        dispatch::dispatch_observed(self, plan, input)
    }
}

impl SkillDispatchError {
    pub(crate) fn kind(&self) -> SkillDispatchErrorKind {
        match self {
            Self::UnknownRequestedTool { .. } => SkillDispatchErrorKind::UnknownRequestedTool,
            Self::AmbiguousBridgeBinding { .. } => SkillDispatchErrorKind::AmbiguousBridgeBinding,
            Self::MissingBridgeBinding { .. } => SkillDispatchErrorKind::MissingBridgeBinding,
            Self::Bridge(_) => SkillDispatchErrorKind::BridgeError,
        }
    }

    pub(crate) fn bridge_error_layer(&self) -> Option<BridgeErrorLayer> {
        match self {
            Self::Bridge(error) => error.layer(),
            _ => None,
        }
    }

    pub(crate) fn bridge_error_message(&self) -> Option<String> {
        match self {
            Self::Bridge(error) => Some(error.to_string()),
            _ => None,
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self {
            Self::UnknownRequestedTool { tool_name } => {
                format!("unknown requested tool: {}", tool_name)
            }
            Self::AmbiguousBridgeBinding {
                tool_name,
                binding_ids,
            } => format!(
                "ambiguous bridge binding for {}: {}",
                tool_name,
                binding_ids.join(",")
            ),
            Self::MissingBridgeBinding {
                tool_name,
                binding_id,
            } => format!(
                "missing bridge binding for {}: {}",
                tool_name, binding_id
            ),
            Self::Bridge(error) => error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{ApprovalRequirement, RiskLevel, ToolCallId};
    use magi_governance::GovernanceService;
    use magi_tool_runtime::{BuiltinTool, BuiltinToolSpec, ToolExecutionContext, ToolRegistry};
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    struct EchoTool;

    impl BuiltinTool for EchoTool {
        fn name(&self) -> &'static str {
            "file_read"
        }

        fn execute(&self, input: &str) -> String {
            format!("echo:{input}")
        }

        fn spec(&self) -> BuiltinToolSpec {
            BuiltinToolSpec {
                name: self.name().to_string(),
                risk_level: RiskLevel::Low,
                approval_requirement: ApprovalRequirement::None,
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct TestModelClient;

    impl magi_bridge_client::ModelBridgeClient for TestModelClient {
        fn invoke(
            &self,
            request: magi_bridge_client::ModelInvocationRequest,
        ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError>
        {
            Ok(magi_bridge_client::BridgeResponse {
                ok: true,
                payload: format!("model:{}", request.prompt),
            })
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FailingModelClient;

    impl magi_bridge_client::ModelBridgeClient for FailingModelClient {
        fn invoke(
            &self,
            _request: magi_bridge_client::ModelInvocationRequest,
        ) -> Result<magi_bridge_client::BridgeResponse, magi_bridge_client::BridgeClientError>
        {
            Err(magi_bridge_client::BridgeClientError::CallFailed {
                layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
                code: None,
                message: "remote denied".to_string(),
            })
        }
    }

    #[test]
    fn builtin_requests_are_rejected_when_not_allowed_by_skill_policy() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-a".to_string(),
            title: "Skill A".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec!["tag".to_string()],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(tool_registry, BridgeDispatchRuntime::new());
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-a".to_string()],
            requested_tools: vec!["search_text".to_string()],
        });
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-1"),
                tool_name: "search_text".to_string(),
                binding_id: None,
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Rejected);
        assert!(matches!(
            outcome.result,
            Ok(SkillDispatchResult::Builtin { ref output })
                if output.status == magi_core::ExecutionResultStatus::Rejected
        ));
    }

    #[test]
    fn builtin_requests_succeed_when_allowed_by_skill_policy() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-a".to_string(),
            title: "Skill A".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec!["tag".to_string()],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(tool_registry, BridgeDispatchRuntime::new());
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-a".to_string()],
            requested_tools: vec!["file_read".to_string()],
        });
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-1b"),
                tool_name: "file_read".to_string(),
                binding_id: None,
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Succeeded);
        assert!(matches!(
            outcome.result,
            Ok(SkillDispatchResult::Builtin { ref output })
                if output.status == magi_core::ExecutionResultStatus::Succeeded
        ));
    }

    #[test]
    fn bridge_requests_succeed_with_valid_target() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-b".to_string(),
            title: "Skill B".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec!["tag".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-b".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-b".to_string()],
            requested_tools: vec!["model.prompt".to_string()],
        });
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-2b"),
                tool_name: "model.prompt".to_string(),
                binding_id: Some("binding-b".to_string()),
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Succeeded);
        assert!(matches!(
            outcome.result,
            Ok(SkillDispatchResult::Bridge { ref output })
                if output.response.ok && output.response.payload == "model:hello"
        ));
    }

    #[test]
    fn invalid_bridge_targets_are_rejected_not_failed() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-b".to_string(),
            title: "Skill B".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec!["tag".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-a".to_string(),
                tool_name: "host.exec".to_string(),
                description: "terminal".to_string(),
                bridge_kind: BridgeBindingKind::Host,
                dispatch_action: BridgeDispatchAction::HostTerminalExec,
                bridge_target: "linux".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-b".to_string()],
            requested_tools: vec!["host.exec".to_string()],
        });
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-2"),
                tool_name: "host.exec".to_string(),
                binding_id: Some("binding-a".to_string()),
                payload: "echo hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: Some("/tmp".to_string()),
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Rejected);
        assert_eq!(outcome.observation.error_kind, Some(SkillDispatchErrorKind::BridgeError));
        assert!(outcome.observation.bridge_error_layer.is_none());
        assert!(matches!(
            outcome.result,
            Err(SkillDispatchError::Bridge(
                magi_bridge_client::BridgeClientError::InvalidBindingTarget { .. }
            ))
        ));
    }

    #[test]
    fn bridge_transport_layers_are_preserved_in_observation() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-c".to_string(),
            title: "Skill C".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec!["tag".to_string()],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-c".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(FailingModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-c".to_string()],
            requested_tools: vec!["model.prompt".to_string()],
        });
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-3"),
                tool_name: "model.prompt".to_string(),
                binding_id: Some("binding-c".to_string()),
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Failed);
        assert_eq!(outcome.observation.error_kind, Some(SkillDispatchErrorKind::BridgeError));
        assert_eq!(
            outcome.observation.bridge_error_layer,
            Some(magi_bridge_client::BridgeErrorLayer::RemoteBusiness)
        );
        assert!(outcome
            .observation
            .bridge_error_message
            .as_deref()
            .is_some_and(|message| message.contains("remote denied")));
        assert!(matches!(
            outcome.result,
            Err(SkillDispatchError::Bridge(
                magi_bridge_client::BridgeClientError::CallFailed {
                    layer: magi_bridge_client::BridgeErrorLayer::RemoteBusiness,
                    ..
                }
            ))
        ));
    }

    // ── T-304: Skill Runtime 切换前验证补齐 ────────────────────────────────

    #[test]
    fn unknown_requested_tool_yields_rejected_observation() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-unknown".to_string(),
            title: "Skill Unknown".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(tool_registry, BridgeDispatchRuntime::new());
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-unknown".to_string()],
            requested_tools: vec!["file_read".to_string()],
        });

        // Dispatch a tool name that is NOT in the plan's routing
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-unknown"),
                tool_name: "nonexistent.tool".to_string(),
                binding_id: None,
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Rejected);
        assert_eq!(
            outcome.observation.error_kind,
            Some(SkillDispatchErrorKind::UnknownRequestedTool)
        );
        assert!(outcome.observation.route.is_none());
        assert!(matches!(
            outcome.result,
            Err(SkillDispatchError::UnknownRequestedTool { ref tool_name })
                if tool_name == "nonexistent.tool"
        ));
    }

    #[test]
    fn ambiguous_bridge_binding_yields_rejected_observation() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        // Two bindings with the same tool_name but different binding_ids
        skill_registry.register(SkillDefinition {
            skill_id: "skill-ambig".to_string(),
            title: "Skill Ambig".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![
                CustomToolBinding {
                    binding_id: "binding-1".to_string(),
                    tool_name: "model.prompt".to_string(),
                    description: "first".to_string(),
                    bridge_kind: BridgeBindingKind::Model,
                    dispatch_action: BridgeDispatchAction::ModelPrompt,
                    bridge_target: "openai".to_string(),
                },
                CustomToolBinding {
                    binding_id: "binding-2".to_string(),
                    tool_name: "model.prompt".to_string(),
                    description: "second".to_string(),
                    bridge_kind: BridgeBindingKind::Model,
                    dispatch_action: BridgeDispatchAction::ModelPrompt,
                    bridge_target: "anthropic".to_string(),
                },
            ],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-ambig".to_string()],
            requested_tools: vec!["model.prompt".to_string()],
        });

        // Dispatch without specifying binding_id — should be ambiguous
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-ambig"),
                tool_name: "model.prompt".to_string(),
                binding_id: None,
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Rejected);
        assert_eq!(
            outcome.observation.error_kind,
            Some(SkillDispatchErrorKind::AmbiguousBridgeBinding)
        );
        assert_eq!(
            outcome.observation.route,
            Some(SkillDispatchRoute::Bridge)
        );
        assert!(matches!(
            outcome.result,
            Err(SkillDispatchError::AmbiguousBridgeBinding { ref binding_ids, .. })
                if binding_ids.len() == 2
        ));
    }

    #[test]
    fn missing_bridge_binding_id_yields_rejected_observation() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-miss".to_string(),
            title: "Skill Missing Binding".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec![],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-exists".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-miss".to_string()],
            requested_tools: vec!["model.prompt".to_string()],
        });

        // Dispatch with a binding_id that doesn't exist in the plan
        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-missing"),
                tool_name: "model.prompt".to_string(),
                binding_id: Some("binding-nonexistent".to_string()),
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Rejected);
        assert_eq!(
            outcome.observation.error_kind,
            Some(SkillDispatchErrorKind::MissingBridgeBinding)
        );
        assert!(matches!(
            outcome.result,
            Err(SkillDispatchError::MissingBridgeBinding { ref binding_id, .. })
                if binding_id == "binding-nonexistent"
        ));
    }

    #[test]
    fn builtin_dispatch_emits_events_to_event_bus() {
        // Verify that builtin tool dispatch through SkillDispatchRuntime
        // propagates audit and usage events to the event bus
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-events".to_string(),
            title: "Events Skill".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(tool_registry, BridgeDispatchRuntime::new());
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-events".to_string()],
            requested_tools: vec!["file_read".to_string()],
        });

        let ctx = ToolExecutionContext {
            worker_id: Some(magi_core::WorkerId::new("wk-skill")),
            task_id: Some(magi_core::TaskId::new("td-skill")),
            session_id: Some(magi_core::SessionId::new("ss-skill")),
            workspace_id: Some(magi_core::WorkspaceId::new("ws-skill")),
        };

        let outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-events"),
                tool_name: "file_read".to_string(),
                binding_id: None,
                payload: "hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ctx,
                working_directory: None,
            },
        );

        assert_eq!(outcome.observation.status, SkillDispatchStatus::Succeeded);
        assert_eq!(outcome.observation.route, Some(SkillDispatchRoute::Builtin));

        // Verify events emitted to event_bus
        let snapshot = event_bus.snapshot();
        let audit_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| {
                e.category == magi_event_bus::EventCategory::Audit
                    && e.event_type == "tool.invoked"
            })
            .collect();
        assert_eq!(audit_events.len(), 1, "one audit event");
        assert_eq!(audit_events[0].payload["tool_name"], "file_read");
        assert_eq!(audit_events[0].payload["tool_call_id"], "call-events");
        assert_eq!(audit_events[0].payload["worker_id"], "wk-skill");

        let usage_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| {
                e.category == magi_event_bus::EventCategory::Usage
                    && e.event_type == "tool.usage.recorded"
            })
            .collect();
        assert_eq!(usage_events.len(), 1, "one usage event");
        assert_eq!(usage_events[0].payload["tool_name"], "file_read");
        assert_eq!(usage_events[0].payload["status"], "Succeeded");
    }

    #[test]
    fn mixed_builtin_and_bridge_plan_dispatches_correctly() {
        // A single plan with both builtin and bridge tools
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-mixed".to_string(),
            title: "Mixed Skill".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-mixed".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-mixed".to_string()],
            requested_tools: vec!["file_read".to_string(), "model.prompt".to_string()],
        });

        // Verify routing summary
        assert!(plan.routing.requested_builtin_tools.contains(&"file_read".to_string()));
        assert!(plan
            .routing
            .requested_bridge_tool_names
            .contains(&"model.prompt".to_string()));

        // Dispatch builtin
        let builtin_outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-mixed-builtin"),
                tool_name: "file_read".to_string(),
                binding_id: None,
                payload: "builtin-payload".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );
        assert_eq!(
            builtin_outcome.observation.status,
            SkillDispatchStatus::Succeeded
        );
        assert_eq!(
            builtin_outcome.observation.route,
            Some(SkillDispatchRoute::Builtin)
        );
        assert!(matches!(
            builtin_outcome.result,
            Ok(SkillDispatchResult::Builtin { .. })
        ));

        // Dispatch bridge
        let bridge_outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-mixed-bridge"),
                tool_name: "model.prompt".to_string(),
                binding_id: Some("binding-mixed".to_string()),
                payload: "bridge-payload".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );
        assert_eq!(
            bridge_outcome.observation.status,
            SkillDispatchStatus::Succeeded
        );
        assert_eq!(
            bridge_outcome.observation.route,
            Some(SkillDispatchRoute::Bridge)
        );
        assert!(matches!(
            bridge_outcome.result,
            Ok(SkillDispatchResult::Bridge { .. })
        ));

        // Unknown tool in same plan → rejected
        let unknown_outcome = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("call-mixed-unknown"),
                tool_name: "no.such.tool".to_string(),
                binding_id: None,
                payload: "".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );
        assert_eq!(
            unknown_outcome.observation.status,
            SkillDispatchStatus::Rejected
        );

        // Verify event bus: 1 builtin dispatch should produce audit+usage events
        let snapshot = event_bus.snapshot();
        let audit_count = snapshot
            .recent_events
            .iter()
            .filter(|e| {
                e.category == magi_event_bus::EventCategory::Audit
                    && e.event_type == "tool.invoked"
            })
            .count();
        assert_eq!(audit_count, 1, "only builtin dispatches emit tool.invoked");
    }

    #[test]
    fn skill_dispatch_observation_fields_are_fully_populated() {
        // Verify that all observation fields are correctly populated for each route
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_builtin(Arc::new(EchoTool));

        let skill_registry = SkillRegistry::new();
        skill_registry.register(SkillDefinition {
            skill_id: "skill-obs".to_string(),
            title: "Obs Skill".to_string(),
            instruction: "instruction".to_string(),
            metadata: SkillMetadata {
                category: "general".to_string(),
                tags: vec![],
            },
            allowed_tools: vec!["file_read".to_string()],
            custom_tool_bindings: vec![CustomToolBinding {
                binding_id: "binding-obs".to_string(),
                tool_name: "model.prompt".to_string(),
                description: "prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            }],
            prompt_priority: 10,
        });

        let runtime = SkillDispatchRuntime::new(
            tool_registry,
            BridgeDispatchRuntime::new().with_model_client(Arc::new(TestModelClient::default())),
        );
        let plan = skill_registry.build_tool_runtime_plan(&SkillSelection {
            skill_ids: vec!["skill-obs".to_string()],
            requested_tools: vec!["file_read".to_string(), "model.prompt".to_string()],
        });

        // Builtin observation
        let builtin = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("obs-builtin"),
                tool_name: "file_read".to_string(),
                binding_id: None,
                payload: "test".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );
        let obs = &builtin.observation;
        assert_eq!(obs.tool_call_id, ToolCallId::new("obs-builtin"));
        assert_eq!(obs.tool_name, "file_read");
        assert_eq!(obs.route, Some(SkillDispatchRoute::Builtin));
        assert!(obs.binding_id.is_none());
        assert!(obs.bridge_kind.is_none());
        assert!(obs.dispatch_action.is_none());
        assert!(obs.error_kind.is_none());
        assert!(obs.bridge_error_layer.is_none());
        assert!(obs.bridge_error_message.is_none());
        assert!(!obs.detail.is_empty());

        // Bridge observation
        let bridge = runtime.dispatch_observed(
            &plan,
            SkillDispatchInput {
                tool_call_id: ToolCallId::new("obs-bridge"),
                tool_name: "model.prompt".to_string(),
                binding_id: Some("binding-obs".to_string()),
                payload: "test".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
                context: ToolExecutionContext::default(),
                working_directory: None,
            },
        );
        let obs = &bridge.observation;
        assert_eq!(obs.tool_call_id, ToolCallId::new("obs-bridge"));
        assert_eq!(obs.tool_name, "model.prompt");
        assert_eq!(obs.route, Some(SkillDispatchRoute::Bridge));
        assert_eq!(obs.binding_id, Some("binding-obs".to_string()));
        assert_eq!(obs.bridge_kind, Some(BridgeBindingKind::Model));
        assert_eq!(obs.dispatch_action, Some(BridgeDispatchAction::ModelPrompt));
        assert!(obs.error_kind.is_none());
        assert!(!obs.detail.is_empty());
    }
}

use crate::types::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeClientError, BridgeDispatchAction,
    BridgeDispatchInput, BridgeDispatchResult, HostBridgeClient, HostBridgeCommand,
    HostBridgeRequest, HostKind, McpBridgeClient, McpToolCallRequest, ModelBridgeClient,
    ModelInvocationRequest,
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct BridgeDispatchRuntime {
    host_client: Option<Arc<dyn HostBridgeClient>>,
    model_client: Option<Arc<dyn ModelBridgeClient>>,
    mcp_client: Option<Arc<dyn McpBridgeClient>>,
}

impl BridgeDispatchRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_host_client(mut self, client: Arc<dyn HostBridgeClient>) -> Self {
        self.host_client = Some(client);
        self
    }

    pub fn with_model_client(mut self, client: Arc<dyn ModelBridgeClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    pub fn with_mcp_client(mut self, client: Arc<dyn McpBridgeClient>) -> Self {
        self.mcp_client = Some(client);
        self
    }

    pub fn dispatch(
        &self,
        plan: &BridgeBindingDispatchPlan,
        input: BridgeDispatchInput,
    ) -> Result<BridgeDispatchResult, BridgeClientError> {
        let binding = plan
            .bindings
            .iter()
            .find(|binding| binding.binding_id == input.binding_id)
            .ok_or_else(|| BridgeClientError::MissingBinding {
                binding_id: input.binding_id.clone(),
            })?;

        let response = match (binding.bridge_kind, binding.dispatch_action) {
            (BridgeBindingKind::Model, BridgeDispatchAction::ModelPrompt) => {
                let client =
                    self.model_client
                        .as_ref()
                        .ok_or(BridgeClientError::MissingClient {
                            bridge_kind: BridgeBindingKind::Model,
                        })?;
                client.invoke(ModelInvocationRequest {
                    provider: binding.bridge_target.clone(),
                    prompt: input.payload.clone(),
                    messages: None,
                    tools: None,
                })?
            }
            (BridgeBindingKind::Mcp, BridgeDispatchAction::McpToolCall) => {
                let client = self
                    .mcp_client
                    .as_ref()
                    .ok_or(BridgeClientError::MissingClient {
                        bridge_kind: BridgeBindingKind::Mcp,
                    })?;
                client.call_tool(McpToolCallRequest {
                    server_name: binding.bridge_target.clone(),
                    tool_name: binding.tool_name.clone(),
                    input: input.payload.clone(),
                })?
            }
            (BridgeBindingKind::Host, BridgeDispatchAction::HostTerminalExec) => {
                let host_kind = resolve_host_kind(&binding.bridge_target, &binding.binding_id)?;
                let working_directory = input.working_directory.clone().ok_or_else(|| {
                    BridgeClientError::MissingWorkingDirectory {
                        binding_id: binding.binding_id.clone(),
                    }
                })?;
                let client = self
                    .host_client
                    .as_ref()
                    .ok_or(BridgeClientError::MissingClient {
                        bridge_kind: BridgeBindingKind::Host,
                    })?;
                client.call(HostBridgeRequest {
                    host_kind,
                    command: HostBridgeCommand::TerminalExec {
                        command: input.payload.clone(),
                        working_directory,
                    },
                })?
            }
            (bridge_kind, dispatch_action) => {
                return Err(BridgeClientError::IncompatibleBindingAction {
                    binding_id: binding.binding_id.clone(),
                    bridge_kind,
                    dispatch_action,
                });
            }
        };

        Ok(BridgeDispatchResult {
            binding_id: binding.binding_id.clone(),
            bridge_kind: binding.bridge_kind,
            dispatch_action: binding.dispatch_action,
            response,
        })
    }
}

fn resolve_host_kind(bridge_target: &str, binding_id: &str) -> Result<HostKind, BridgeClientError> {
    match bridge_target.trim().to_ascii_lowercase().as_str() {
        "vscode" => Ok(HostKind::Vscode),
        "idea" => Ok(HostKind::Idea),
        _ => Err(BridgeClientError::InvalidBindingTarget {
            binding_id: binding_id.to_string(),
            bridge_target: bridge_target.to_string(),
        }),
    }
}

use crate::types::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeClientError, BridgeDispatchAction,
    BridgeDispatchInput, BridgeDispatchResult, McpBridgeClient, McpToolCallRequest,
    ModelBridgeClient, ModelInvocationRequest,
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct BridgeDispatchRuntime {
    model_client: Option<Arc<dyn ModelBridgeClient>>,
    mcp_client: Option<Arc<dyn McpBridgeClient>>,
}

impl BridgeDispatchRuntime {
    pub fn new() -> Self {
        Self::default()
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
                    tool_choice: None,
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

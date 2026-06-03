use crate::{
    SkillDispatchError, SkillDispatchInput, SkillDispatchObservation, SkillDispatchResult,
    SkillDispatchRoute, SkillDispatchStatus,
};
use magi_bridge_client::{BridgeBindingReference, BridgeClientError};
use magi_core::ExecutionResultStatus;

pub(crate) fn build_dispatch_observation(
    input: &SkillDispatchInput,
    binding: Option<&BridgeBindingReference>,
    route: Option<SkillDispatchRoute>,
    result: Result<&SkillDispatchResult, &SkillDispatchError>,
) -> SkillDispatchObservation {
    match result {
        Ok(SkillDispatchResult::Builtin { output }) => SkillDispatchObservation {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            route,
            binding_id: input.binding_id.clone(),
            bridge_kind: None,
            dispatch_action: None,
            status: map_builtin_dispatch_status(output.status),
            error_kind: None,
            bridge_error_layer: None,
            bridge_error_message: None,
            detail: output.payload.clone(),
        },
        Ok(SkillDispatchResult::Preflight { output }) => SkillDispatchObservation {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            route,
            binding_id: input
                .binding_id
                .clone()
                .or_else(|| binding.map(|binding| binding.binding_id.clone())),
            bridge_kind: binding.map(|binding| binding.bridge_kind),
            dispatch_action: binding.map(|binding| binding.dispatch_action),
            status: map_builtin_dispatch_status(output.status),
            error_kind: None,
            bridge_error_layer: None,
            bridge_error_message: None,
            detail: output.payload.clone(),
        },
        Ok(SkillDispatchResult::Bridge { output }) => SkillDispatchObservation {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            route,
            binding_id: Some(output.binding_id.clone()),
            bridge_kind: Some(output.bridge_kind),
            dispatch_action: Some(output.dispatch_action),
            status: if output.response.ok {
                SkillDispatchStatus::Succeeded
            } else {
                SkillDispatchStatus::Failed
            },
            error_kind: None,
            bridge_error_layer: None,
            bridge_error_message: None,
            detail: output.response.payload.clone(),
        },
        Err(error) => SkillDispatchObservation {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            route,
            binding_id: input
                .binding_id
                .clone()
                .or_else(|| binding.map(|binding| binding.binding_id.clone())),
            bridge_kind: binding.map(|binding| binding.bridge_kind),
            dispatch_action: binding.map(|binding| binding.dispatch_action),
            status: map_dispatch_error_status(error),
            error_kind: Some(error.kind()),
            bridge_error_layer: error.bridge_error_layer(),
            bridge_error_message: error.bridge_error_message(),
            detail: error.detail(),
        },
    }
}

fn map_builtin_dispatch_status(status: ExecutionResultStatus) -> SkillDispatchStatus {
    match status {
        ExecutionResultStatus::Succeeded => SkillDispatchStatus::Succeeded,
        ExecutionResultStatus::NeedsApproval => SkillDispatchStatus::NeedsApproval,
        ExecutionResultStatus::Rejected => SkillDispatchStatus::Rejected,
        ExecutionResultStatus::Failed | ExecutionResultStatus::Cancelled => {
            SkillDispatchStatus::Failed
        }
    }
}

fn map_dispatch_error_status(error: &SkillDispatchError) -> SkillDispatchStatus {
    match error {
        SkillDispatchError::UnknownRequestedTool { .. }
        | SkillDispatchError::AmbiguousBridgeBinding { .. }
        | SkillDispatchError::MissingBridgeBinding { .. } => SkillDispatchStatus::Rejected,
        SkillDispatchError::Bridge(error) => match error {
            BridgeClientError::InvalidBindingTarget { .. }
            | BridgeClientError::IncompatibleBindingAction { .. }
            | BridgeClientError::MissingClient { .. }
            | BridgeClientError::MissingBinding { .. }
            | BridgeClientError::MissingWorkingDirectory { .. } => SkillDispatchStatus::Rejected,
            BridgeClientError::CallFailed { .. } | BridgeClientError::HttpStatusFailed { .. } => {
                SkillDispatchStatus::Failed
            }
        },
    }
}

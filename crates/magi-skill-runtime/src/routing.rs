use crate::{
    CustomToolBinding, SkillDispatchError, SkillDispatchInput, SkillDispatchRoute,
    SkillToolRoutingSummary, SkillToolRuntimePlan,
};
use magi_bridge_client::{BridgeBindingDispatchPlan, BridgeBindingReference};

pub(crate) fn resolve_route(
    plan: &SkillToolRuntimePlan,
    input: &SkillDispatchInput,
) -> Result<SkillDispatchRoute, SkillDispatchError> {
    if plan
        .routing
        .requested_builtin_tools
        .iter()
        .any(|tool_name| tool_name == &input.tool_name)
    {
        return Ok(SkillDispatchRoute::Builtin);
    }

    if plan
        .routing
        .requested_bridge_tool_names
        .iter()
        .any(|tool_name| tool_name == &input.tool_name)
    {
        return Ok(SkillDispatchRoute::Bridge);
    }

    Err(SkillDispatchError::UnknownRequestedTool {
        tool_name: input.tool_name.clone(),
    })
}

pub(crate) fn resolve_bridge_binding_id(
    plan: &SkillToolRuntimePlan,
    input: &SkillDispatchInput,
) -> Result<String, SkillDispatchError> {
    if let Some(binding_id) = &input.binding_id {
        if plan
            .bridge_dispatch_plan
            .bindings
            .iter()
            .any(|binding| &binding.binding_id == binding_id)
        {
            return Ok(binding_id.clone());
        }
        return Err(SkillDispatchError::MissingBridgeBinding {
            tool_name: input.tool_name.clone(),
            binding_id: binding_id.clone(),
        });
    }

    let matching = plan
        .bridge_dispatch_plan
        .bindings
        .iter()
        .filter(|binding| binding.tool_name == input.tool_name)
        .map(|binding| binding.binding_id.clone())
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [binding_id] => Ok(binding_id.clone()),
        [] => Err(SkillDispatchError::UnknownRequestedTool {
            tool_name: input.tool_name.clone(),
        }),
        _ => Err(SkillDispatchError::AmbiguousBridgeBinding {
            tool_name: input.tool_name.clone(),
            binding_ids: matching,
        }),
    }
}

pub(crate) fn resolve_observation_binding<'a>(
    plan: &'a SkillToolRuntimePlan,
    input: &SkillDispatchInput,
) -> Option<&'a BridgeBindingReference> {
    if let Some(binding_id) = &input.binding_id {
        return plan
            .bridge_dispatch_plan
            .bindings
            .iter()
            .find(|binding| &binding.binding_id == binding_id);
    }

    let mut matching = plan
        .bridge_dispatch_plan
        .bindings
        .iter()
        .filter(|binding| binding.tool_name == input.tool_name);

    let binding = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(binding)
}

pub(crate) fn build_bridge_dispatch_plan(
    skill_ids: &[String],
    bindings: &[CustomToolBinding],
    routing: &SkillToolRoutingSummary,
) -> BridgeBindingDispatchPlan {
    let mut references = bindings
        .iter()
        .filter(|binding| {
            routing.requested_bridge_binding_ids.is_empty()
                || routing
                    .requested_bridge_binding_ids
                    .iter()
                    .any(|binding_id| binding_id == &binding.binding_id)
        })
        .map(|binding| BridgeBindingReference {
            binding_id: binding.binding_id.clone(),
            tool_name: binding.tool_name.clone(),
            bridge_kind: binding.bridge_kind,
            dispatch_action: binding.dispatch_action,
            bridge_target: binding.bridge_target.clone(),
        })
        .collect::<Vec<_>>();
    references.sort_by(|left, right| {
        left.binding_id
            .cmp(&right.binding_id)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
            .then_with(|| left.bridge_target.cmp(&right.bridge_target))
    });
    references.dedup_by(|left, right| left.binding_id == right.binding_id);

    let mut source_skill_ids = skill_ids.to_vec();
    source_skill_ids.sort();
    source_skill_ids.dedup();

    BridgeBindingDispatchPlan {
        source_skill_ids,
        bindings: references,
    }
}

pub(crate) fn classify_requested_tools(
    requested_tools: &[String],
    custom_tool_bindings: &[CustomToolBinding],
) -> SkillToolRoutingSummary {
    let mut requested_builtin_tools = Vec::new();
    let mut requested_bridge_tool_names = Vec::new();
    let mut requested_bridge_binding_ids = Vec::new();
    let mut denied_requested_tools = Vec::new();

    if requested_tools.is_empty() {
        requested_bridge_binding_ids = custom_tool_bindings
            .iter()
            .map(|binding| binding.binding_id.clone())
            .collect();
        requested_bridge_tool_names = custom_tool_bindings
            .iter()
            .map(|binding| binding.tool_name.clone())
            .collect();
    } else {
        for tool_name in requested_tools {
            let matched_bindings = custom_tool_bindings
                .iter()
                .filter(|binding| binding.tool_name == *tool_name)
                .collect::<Vec<_>>();
            if matched_bindings.is_empty() {
                requested_builtin_tools.push(tool_name.clone());
                continue;
            }
            requested_bridge_tool_names.push(tool_name.clone());
            for binding in matched_bindings {
                requested_bridge_binding_ids.push(binding.binding_id.clone());
            }
        }
    }

    requested_builtin_tools.sort();
    requested_builtin_tools.dedup();
    requested_bridge_tool_names.sort();
    requested_bridge_tool_names.dedup();
    requested_bridge_binding_ids.sort();
    requested_bridge_binding_ids.dedup();
    denied_requested_tools.sort();
    denied_requested_tools.dedup();

    SkillToolRoutingSummary {
        requested_builtin_tools,
        requested_bridge_tool_names,
        requested_bridge_binding_ids,
        denied_requested_tools,
    }
}

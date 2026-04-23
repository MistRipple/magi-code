use crate::{
    SkillDispatchError, SkillDispatchExecutionOutcome, SkillDispatchInput, SkillDispatchResult,
    SkillDispatchRoute, SkillDispatchRuntime, SkillToolRuntimePlan,
    observation::build_dispatch_observation,
    routing::{resolve_bridge_binding_id, resolve_observation_binding, resolve_route},
};
use magi_bridge_client::BridgeDispatchInput;
use magi_governance::ToolKind;
use magi_tool_runtime::ToolExecutionInput;

pub(crate) fn execute_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    match resolve_route(plan, &input)? {
        SkillDispatchRoute::Builtin => execute_builtin_dispatch(runtime, plan, input),
        SkillDispatchRoute::Bridge => execute_bridge_dispatch(runtime, plan, input),
    }
}

pub(crate) fn dispatch_observed(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> SkillDispatchExecutionOutcome {
    let route = resolve_route(plan, &input).ok();
    let result = execute_dispatch(runtime, plan, input.clone());
    let binding = resolve_observation_binding(plan, &input);
    let observation = build_dispatch_observation(&input, binding, route, result.as_ref());
    SkillDispatchExecutionOutcome {
        observation,
        result,
    }
}

fn execute_builtin_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    Ok(SkillDispatchResult::Builtin {
        output: runtime.tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: input.tool_call_id,
                tool_name: input.tool_name,
                tool_kind: ToolKind::Builtin,
                input: input.payload,
                approval_requirement: input.approval_requirement,
                risk_level: input.risk_level,
            },
            input.context,
            &plan.tool_policy,
        ),
    })
}

fn execute_bridge_dispatch(
    runtime: &SkillDispatchRuntime,
    plan: &SkillToolRuntimePlan,
    input: SkillDispatchInput,
) -> Result<SkillDispatchResult, SkillDispatchError> {
    let binding_id = resolve_bridge_binding_id(plan, &input)?;
    let output = runtime
        .bridge_runtime
        .dispatch(
            &plan.bridge_dispatch_plan,
            BridgeDispatchInput {
                binding_id,
                payload: input.payload,
                working_directory: input.working_directory,
            },
        )
        .map_err(SkillDispatchError::Bridge)?;
    Ok(SkillDispatchResult::Bridge { output })
}

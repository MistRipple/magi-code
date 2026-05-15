//! Task System v2 — M13：任务分解管线从旧版 API 派发层下沉到
//! conversation-runtime。
//!
//! 函数 `decompose_mission` 接收显式的 `ModelBridgeClient` 与 `workspace_root_path`，
//! 不再依赖 magi-api 的 `ApiState`；上层 `run_dispatch_submission` /
//! `replan_task_graph` 调用前自行拿 `state.model_bridge_client()` /
//! `state.workspace_root_path(...)` 并按值/引用传入。
//!
//! 这里同时承载 `task_plan_tool` 工具定义、`TASK_PLAN_TOOL_NAME` 常量与
//! 计划 JSON 的解析/规范化，所有这些都强依赖 `TaskGraphPlan`（已位于
//! `task_graph_builder` 模块）。

use std::path::Path;
use std::sync::Arc;

use magi_bridge_client::{
    ChatCompletionPayload, ChatToolChoice, ChatToolDefinition, ChatToolFunctionDefinition,
    LOOPBACK_MODEL_PROVIDER, ModelBridgeClient, ModelInvocationRequest,
};

use crate::task_graph_builder::{
    TASK_MAX_PHASES, TASK_MIN_PHASES, TaskGraphPlan, task_phase_count_is_valid,
};

pub const TASK_PLAN_TOOL_NAME: &str = "create_task_plan";

pub fn task_plan_tool() -> ChatToolDefinition {
    ChatToolDefinition {
        kind: "function".to_string(),
        function: ChatToolFunctionDefinition {
            name: TASK_PLAN_TOOL_NAME.to_string(),
            description: "创建严格结构化的任务图计划，供 Task Graph 构建器直接消费。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["phases"],
                "properties": {
                    "phases": {
                        "type": "array",
                        "minItems": TASK_MIN_PHASES,
                        "maxItems": TASK_MAX_PHASES,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["title", "workPackages"],
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "阶段标题。第一阶段必须是规划，最后一阶段必须是交付；中间可以有一个或多个按批次推进的执行阶段。"
                                },
                                "workPackages": {
                                    "type": "array",
                                    "minItems": 1,
                                    "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "required": ["title", "actions"],
                                        "properties": {
                                            "title": {
                                                "type": "string",
                                                "description": "工作包标题，表达一组可交付的相关动作。"
                                            },
                                            "actions": {
                                                "type": "array",
                                                "minItems": 1,
                                                "items": {
                                                    "type": "object",
                                                    "additionalProperties": false,
                                                    "required": ["title", "goal"],
                                                    "properties": {
                                                        "title": {
                                                            "type": "string",
                                                            "description": "动作标题，必须短小且可执行。不要在标题里写 [角色] 前缀；角色请用独立的 roleId 字段承载。"
                                                        },
                                                        "goal": {
                                                            "type": "string",
                                                            "description": "动作目标，必须说明完成标准或产出。"
                                                        },
                                                        "roleId": {
                                                            "type": ["string", "null"],
                                                            "description": "执行角色 ID，必须从预注册 role 集合中选取：architect / integration-dev / frontend-dev / backend-dev / reviewer / debugger / test-engineer / doc-writer / data-engineer / devops-engineer / security-analyst。不确定时可留空，后端会基于 goal 自动推断。"
                                                        },
                                                        "dependsOn": {
                                                            "type": "array",
                                                            "items": { "type": "string" },
                                                            "description": "同一阶段内已定义动作的标题。"
                                                        },
                                                        "writeScope": {
                                                            "type": ["string", "null"],
                                                            "description": "可选写入范围，例如 crates/magi-api 或 web/src。"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }),
        },
    }
}

pub fn decompose_mission(
    model_bridge_client: Option<&Arc<dyn ModelBridgeClient>>,
    workspace_root_path: Option<&Path>,
    prompt: Option<&str>,
) -> Option<TaskGraphPlan> {
    let prompt_text = prompt.filter(|s| !s.trim().is_empty())?;
    let client = model_bridge_client?;
    let workspace_context = workspace_root_path
        .map(|path| {
            format!(
                "\n当前工作区根目录：{}\n如果任务目标提到当前项目、当前仓库、本项目或 codebase，计划里的 action goal 必须要求读取这个工作区的真实目录、配置和关键源码，不要让 worker 等用户粘贴项目结构。",
                path.display()
            )
        })
        .unwrap_or_default();
    let request = ModelInvocationRequest {
        provider: LOOPBACK_MODEL_PROVIDER.to_string(),
        prompt: format!(
            "任务图规划器。\n\
             请只调用 {TASK_PLAN_TOOL_NAME} 工具输出结构化计划，不要返回自然语言正文。\n\
             计划必须包含 {TASK_MIN_PHASES} 到 {TASK_MAX_PHASES} 个 phase：第一 phase 是规划，最后 phase 是交付，中间 phase 是一个或多个按实际批次推进的执行阶段。\n\
             如果任务目标包含“第一批/第二批/下一批/继续创建任务/发现后继续推进/多段任务”等纵向编排要求，必须把每一批推进建模为独立执行 phase，不能把多批命令塞进同一个 action。\n\
             每个 phase 至少 1 个 workPackage，每个 workPackage 至少 1 个 action。\n\
             action 的 dependsOn 只能引用同一 phase 内已定义的较早 action 标题。\n\
             action goal 必须描述可验证产出或完成标准。\n\
             每个 action 应在 roleId 字段显式声明执行角色，从以下集合中选取：\
             architect / integration-dev / frontend-dev / backend-dev / reviewer / debugger / test-engineer / doc-writer / data-engineer / devops-engineer / security-analyst。\
             不要在 action title 里写 [角色] 或【角色】前缀，角色由结构化字段单独承载。\n\
             原始任务目标是唯一主事实，必须逐字保留其中的路径、工具名、命令、标记字符串和“必须/要求”条款；不得把它改写成历史任务、泛化检查或只读替代目标。\n\
             规划 phase 只输出目标、边界、执行计划和验收标准，不得调用工具，不得执行用户目标里的写入、删除、移动、补丁或其他有副作用操作。\n\
             中间执行 phase 是唯一可以执行用户目标和写操作的阶段；如果目标包含明确工具链路，对应批次的执行 action goal 必须按原始顺序列出这些工具和验收标记。\n\
             交付 phase 只能基于执行产出和验证证据总结，不得调用工具，不得重复写入、删除、移动、补丁或重新执行用户目标。\n\
             任务目标：\n<<<MAGI_TASK_GOAL>>>\n{}\n<<<END_MAGI_TASK_GOAL>>>{}",
            prompt_text, workspace_context
        ),
        messages: None,
        tools: Some(vec![task_plan_tool()]),
        tool_choice: Some(ChatToolChoice::force_function(TASK_PLAN_TOOL_NAME)),
    };
    let response = client.invoke(request).ok()?;
    if !response.ok {
        return None;
    }
    parse_decomposition_response(&response.payload, prompt_text)
}

pub fn parse_decomposition_response(
    response: &str,
    original_prompt: &str,
) -> Option<TaskGraphPlan> {
    let trimmed = response.trim();
    let normalized = trimmed
        .strip_prefix("loopback-model::")
        .unwrap_or(trimmed)
        .trim();

    if let Ok(payload) = serde_json::from_str::<ChatCompletionPayload>(normalized)
        && let Some(arguments) = payload
            .tool_calls
            .iter()
            .find(|call| call.function.name == TASK_PLAN_TOOL_NAME)
            .map(|call| call.function.arguments.as_str())
        && let Ok(plan_value) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(plan) = parse_decomposition_plan(plan_value, original_prompt)
    {
        return Some(plan);
    }

    let plan_value: serde_json::Value = serde_json::from_str(normalized).ok()?;
    parse_decomposition_plan(plan_value, original_prompt)
}

pub fn parse_decomposition_plan(
    plan_value: serde_json::Value,
    original_prompt: &str,
) -> Option<TaskGraphPlan> {
    let mut plan: TaskGraphPlan = serde_json::from_value(plan_value).ok()?;
    if !task_phase_count_is_valid(plan.phases.len()) {
        return None;
    }

    let last_phase_index = plan.phases.len().saturating_sub(1);
    for (phase_index, phase) in plan.phases.iter_mut().enumerate() {
        phase.title = normalize_plan_text(&phase.title, original_prompt)?;
        if phase.work_packages.is_empty() {
            return None;
        }
        for package in &mut phase.work_packages {
            package.title = normalize_plan_text(&package.title, original_prompt)?;
            if package.actions.is_empty() {
                return None;
            }
            for action in &mut package.actions {
                action.title = normalize_plan_text(&action.title, original_prompt)?;
                action.goal = normalize_plan_text(&action.goal, original_prompt)?;
                if phase_index == last_phase_index {
                    action.title = normalize_delivery_action_title(&action.title);
                }
                if action
                    .depends_on
                    .iter()
                    .any(|dependency| dependency.trim().is_empty())
                {
                    return None;
                }
                action.write_scope = action
                    .write_scope
                    .take()
                    .map(|scope| scope.trim().to_string())
                    .filter(|scope| !scope.is_empty());
            }
        }
    }
    Some(plan)
}

pub fn normalize_delivery_action_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed == "验证交付" || trimmed == "交付验证" {
        return "汇总结果".to_string();
    }
    trimmed.to_string()
}

pub fn normalize_plan_text(value: &str, original_prompt: &str) -> Option<String> {
    let text = value.trim();
    if text.is_empty() || text == original_prompt.trim() {
        return None;
    }
    Some(text.to_string())
}

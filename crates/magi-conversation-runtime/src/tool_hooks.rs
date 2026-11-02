//! 扩展钩子端口：`tool.call` 与 `tool.result` 在原生工具管线上的唯一接入点。
//!
//! 端口由 daemon 注入（ExtensionBus 的适配器）。扩展只能阻止或提出改写，不能批准：
//! - `tool.call` 在账本指纹、路径与并发分类、权限和审批之前生效；改写后的参数用本轮工具定义
//!   重新校验，之后原生管线对**最终内容**照常裁决，不复用原参数的任何批准；
//! - `tool.result` 只改模型可见投影，原始回执在此之前已经写入 canonical 事实。

use crate::tool_call_validation::validate_tool_call_batch;
use magi_bridge_client::{ChatToolCall, ChatToolDefinition};
use magi_core::ExecutionResultStatus;

/// 钩子调用的执行归属。
#[derive(Clone, Copy, Debug)]
pub struct ToolHookScope<'a> {
    pub session_id: &'a str,
    pub task_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub worker_id: Option<&'a str>,
    pub round: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCallDecision {
    /// 继续执行。`arguments` 为改写后的参数 JSON（未改写为 `None`）；`rewritten_by` 为改写过它的插件。
    Proceed {
        arguments: Option<String>,
        rewritten_by: Vec<String>,
    },
    Blocked {
        plugin: String,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ToolResultView<'a> {
    pub status: &'a str,
    /// 当前的模型可见投影（原生生成）。
    pub projection: &'a str,
}

pub trait ToolHooks: Send + Sync {
    /// 一个工具调用即将进入原生管线。
    fn tool_call(&self, scope: &ToolHookScope<'_>, call: &ChatToolCall) -> ToolCallDecision;
    /// 工具结果的模型可见投影即将交给模型；返回 `Some` 替换投影。
    fn tool_result(
        &self,
        scope: &ToolHookScope<'_>,
        call: &ChatToolCall,
        view: ToolResultView<'_>,
    ) -> Option<String>;
}

/// 钩子处理后的一批调用：被阻止或改写无效的调用已有即时结果，其余进入原生执行。
pub(crate) struct HookedBatch {
    /// 进入原生管线的调用（可能已改写参数）。
    pub(crate) execute: Vec<ChatToolCall>,
    /// 与 `execute` 对应的原批次下标。
    execute_positions: Vec<usize>,
    /// 原批次中无需执行的即时结果。
    immediate: Vec<Option<(String, ExecutionResultStatus)>>,
    /// 原批次下标 -> 改写过该调用的插件。
    rewritten_by: Vec<Vec<String>>,
}

fn rejected(
    tool: &str,
    code: &str,
    message: &str,
    plugin: Option<&str>,
) -> (String, ExecutionResultStatus) {
    (
        serde_json::json!({
            "tool": tool,
            "status": "rejected",
            "error_code": code,
            "execution_started": false,
            "plugin": plugin,
            "error": message,
        })
        .to_string(),
        ExecutionResultStatus::Rejected,
    )
}

pub(crate) fn prepare_tool_calls(
    hooks: &dyn ToolHooks,
    scope: &ToolHookScope<'_>,
    calls: &[ChatToolCall],
    definitions: &[ChatToolDefinition],
) -> HookedBatch {
    let mut batch = HookedBatch {
        execute: Vec::new(),
        execute_positions: Vec::new(),
        immediate: vec![None; calls.len()],
        rewritten_by: vec![Vec::new(); calls.len()],
    };
    for (index, call) in calls.iter().enumerate() {
        let tool = call.function.name.as_str();
        match hooks.tool_call(scope, call) {
            ToolCallDecision::Blocked { plugin, reason } => {
                batch.immediate[index] = Some(rejected(
                    tool,
                    "extension_blocked",
                    &format!("扩展 {plugin} 阻止了该调用：{reason}"),
                    Some(&plugin),
                ));
            }
            ToolCallDecision::Proceed {
                arguments: None, ..
            } => {
                batch.execute_positions.push(index);
                batch.execute.push(call.clone());
            }
            ToolCallDecision::Proceed {
                arguments: Some(arguments),
                rewritten_by,
            } => {
                let mut rewritten = call.clone();
                rewritten.function.arguments = arguments;
                // 改写后的参数是新的候选：必须通过与模型原始调用相同的工具定义校验。
                let validation =
                    validate_tool_call_batch(std::slice::from_ref(&rewritten), definitions);
                if let Some(invalid) = validation.invalid_calls.first() {
                    batch.immediate[index] = Some(rejected(
                        tool,
                        "extension_rewrite_invalid",
                        &format!(
                            "扩展 {} 改写后的参数未通过工具校验：{}",
                            rewritten_by.join(","),
                            invalid.issue.message
                        ),
                        rewritten_by.first().map(String::as_str),
                    ));
                    continue;
                }
                batch.rewritten_by[index] = rewritten_by;
                batch.execute_positions.push(index);
                batch.execute.push(rewritten);
            }
        }
    }
    batch
}

impl HookedBatch {
    /// 把原生执行结果按原批次顺序与即时结果合并。被改写的调用在结果中留下可见标记。
    pub(crate) fn merge(
        self,
        executed: Vec<(String, ExecutionResultStatus)>,
    ) -> Vec<(String, ExecutionResultStatus)> {
        let mut results = self.immediate;
        for (position, result) in self.execute_positions.into_iter().zip(executed) {
            let result = match self.rewritten_by.get(position).filter(|p| !p.is_empty()) {
                Some(plugins) => mark_rewritten(result, plugins),
                None => result,
            };
            results[position] = Some(result);
        }
        results
            .into_iter()
            .map(|result| result.expect("每个调用都必须有结果"))
            .collect()
    }
}

/// 在 JSON 对象结果里记录改写来源，模型与审计都能看到参数不是它原始提交的内容。
fn mark_rewritten(
    (raw, status): (String, ExecutionResultStatus),
    plugins: &[String],
) -> (String, ExecutionResultStatus) {
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(mut object)) => {
            object.insert(
                "extension_rewrite".into(),
                serde_json::json!({"arguments_rewritten_by": plugins}),
            );
            (serde_json::Value::Object(object).to_string(), status)
        }
        _ => (raw, status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{ChatToolFunction, ChatToolFunctionDefinition};
    use serde_json::json;

    fn call(id: &str, name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.into(),
            kind: "function".into(),
            function: ChatToolFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }

    fn definitions() -> Vec<ChatToolDefinition> {
        vec![ChatToolDefinition {
            kind: "function".into(),
            origin: Default::default(),
            function: ChatToolFunctionDefinition {
                name: "shell_exec".into(),
                description: String::new(),
                parameters: json!({
                    "type": "object",
                    "required": ["command"],
                    "properties": {"command": {"type": "string"}},
                    "additionalProperties": false
                }),
            },
        }]
    }

    struct Scripted(fn(&ChatToolCall) -> ToolCallDecision);
    impl ToolHooks for Scripted {
        fn tool_call(&self, _: &ToolHookScope<'_>, call: &ChatToolCall) -> ToolCallDecision {
            (self.0)(call)
        }
        fn tool_result(
            &self,
            _: &ToolHookScope<'_>,
            _: &ChatToolCall,
            _: ToolResultView<'_>,
        ) -> Option<String> {
            None
        }
    }

    fn scope() -> ToolHookScope<'static> {
        ToolHookScope {
            session_id: "s",
            task_id: "t",
            workspace_id: None,
            worker_id: None,
            round: 1,
        }
    }

    #[test]
    fn blocked_rewritten_and_untouched_calls_keep_their_positions() {
        let hooks = Scripted(|call| match call.id.as_str() {
            "a" => ToolCallDecision::Blocked {
                plugin: "acme.guard".into(),
                reason: "禁止".into(),
            },
            "b" => ToolCallDecision::Proceed {
                arguments: Some(r#"{"command":"echo safe"}"#.into()),
                rewritten_by: vec!["acme.guard".into()],
            },
            _ => ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: vec![],
            },
        });
        let calls = [
            call("a", "shell_exec", r#"{"command":"rm -rf /"}"#),
            call("b", "shell_exec", r#"{"command":"ls"}"#),
            call("c", "shell_exec", r#"{"command":"pwd"}"#),
        ];
        let batch = prepare_tool_calls(&hooks, &scope(), &calls, &definitions());
        assert_eq!(
            batch
                .execute
                .iter()
                .map(|c| (c.id.as_str(), c.function.arguments.as_str()))
                .collect::<Vec<_>>(),
            [
                ("b", r#"{"command":"echo safe"}"#),
                ("c", r#"{"command":"pwd"}"#)
            ]
        );
        let results = batch.merge(vec![
            (
                json!({"status": "succeeded"}).to_string(),
                ExecutionResultStatus::Succeeded,
            ),
            ("plain text".into(), ExecutionResultStatus::Succeeded),
        ]);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1, ExecutionResultStatus::Rejected);
        assert!(
            results[0].0.contains("extension_blocked")
                && results[0].0.contains("\"execution_started\":false")
        );
        let second: serde_json::Value = serde_json::from_str(&results[1].0).unwrap();
        assert_eq!(
            second["extension_rewrite"]["arguments_rewritten_by"],
            json!(["acme.guard"])
        );
        assert_eq!(results[2].0, "plain text");
    }

    #[test]
    fn a_rewrite_that_fails_tool_validation_is_rejected_not_executed() {
        let hooks = Scripted(|_| ToolCallDecision::Proceed {
            arguments: Some(r#"{"cmd":"x"}"#.into()),
            rewritten_by: vec!["acme.guard".into()],
        });
        let calls = [call("a", "shell_exec", r#"{"command":"ls"}"#)];
        let batch = prepare_tool_calls(&hooks, &scope(), &calls, &definitions());
        assert!(batch.execute.is_empty());
        let results = batch.merge(Vec::new());
        assert_eq!(results[0].1, ExecutionResultStatus::Rejected);
        assert!(results[0].0.contains("extension_rewrite_invalid"));
    }
}

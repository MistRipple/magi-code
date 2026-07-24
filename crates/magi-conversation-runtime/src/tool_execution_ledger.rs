//! 单个任务轮次内的工具执行账本。
//!
//! 模型可以重复提出相同的 function call，但模型请求不是工具执行授权。账本在
//! 调度边界统一约束实际执行次数：只读幂等调用复用同轮成功结果；用户明确要求
//! 某工具只调用一次时，后续调用返回结构化预算结果而不再触发外部副作用。

use std::collections::BTreeMap;

use magi_bridge_client::ChatToolCall;
use magi_core::ExecutionResultStatus;
use magi_tool_runtime::{BuiltinToolName, ToolRegistry};
use serde_json::Value;

use crate::canonical_tool_call_name;

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolExecutionLedger {
    successful_idempotent_calls: BTreeMap<ToolCallFingerprint, String>,
    executed_call_counts: BTreeMap<String, usize>,
    explicit_call_budgets: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ToolCallFingerprint {
    tool_name: String,
    canonical_arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallExecutionDecision {
    Execute {
        fingerprint: Option<ToolCallFingerprint>,
    },
    Reuse {
        result: String,
    },
    ReuseAfterExecution {
        source_index: usize,
        fingerprint: ToolCallFingerprint,
    },
    BudgetExhausted {
        result: String,
    },
}

impl ToolExecutionLedger {
    pub(crate) fn for_task_goal(goal: &str) -> Self {
        Self {
            explicit_call_budgets: explicit_single_call_budgets(goal),
            ..Self::default()
        }
    }

    /// 为一个模型响应生成执行决策。相同只读调用在同一响应内只允许一个
    /// 真实执行代表；跨响应则直接复用已经成功的结果。
    pub(crate) fn plan(
        &self,
        tool_calls: &[ChatToolCall],
        tool_registry: Option<&ToolRegistry>,
    ) -> Vec<ToolCallExecutionDecision> {
        let mut first_pending_by_fingerprint = BTreeMap::<ToolCallFingerprint, usize>::new();
        let mut planned_call_counts = self.executed_call_counts.clone();

        tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| {
                let canonical_name = canonical_tool_call_name(&tool_call.function.name);
                let fingerprint = idempotent_fingerprint(tool_call, &canonical_name, tool_registry);

                if let Some(fingerprint) = fingerprint.as_ref()
                    && let Some(result) = self.successful_idempotent_calls.get(fingerprint)
                {
                    return ToolCallExecutionDecision::Reuse {
                        result: reused_result(
                            &canonical_name,
                            result,
                            "duplicate_idempotent_call",
                            "本轮已复用相同只读工具的成功结果，未再次执行。",
                        ),
                    };
                }

                if let Some(limit) = self.explicit_call_budgets.get(&canonical_name)
                    && planned_call_counts
                        .get(&canonical_name)
                        .copied()
                        .unwrap_or_default()
                        >= *limit
                {
                    return ToolCallExecutionDecision::BudgetExhausted {
                        result: budget_exhausted_result(&canonical_name, *limit),
                    };
                }

                if let Some(fingerprint) = fingerprint {
                    if let Some(source_index) = first_pending_by_fingerprint.get(&fingerprint) {
                        return ToolCallExecutionDecision::ReuseAfterExecution {
                            source_index: *source_index,
                            fingerprint,
                        };
                    }
                    first_pending_by_fingerprint.insert(fingerprint.clone(), index);
                    *planned_call_counts.entry(canonical_name).or_default() += 1;
                    return ToolCallExecutionDecision::Execute {
                        fingerprint: Some(fingerprint),
                    };
                }

                *planned_call_counts.entry(canonical_name).or_default() += 1;
                ToolCallExecutionDecision::Execute { fingerprint: None }
            })
            .collect()
    }

    pub(crate) fn record_execution(
        &mut self,
        tool_call: &ChatToolCall,
        fingerprint: Option<&ToolCallFingerprint>,
        result: &(String, ExecutionResultStatus),
    ) {
        let canonical_name = canonical_tool_call_name(&tool_call.function.name);
        *self.executed_call_counts.entry(canonical_name).or_default() += 1;
        if matches!(result.1, ExecutionResultStatus::Succeeded)
            && let Some(fingerprint) = fingerprint
        {
            self.successful_idempotent_calls
                .insert(fingerprint.clone(), result.0.clone());
        }
    }

    pub(crate) fn reuse_after_execution(
        &self,
        tool_call: &ChatToolCall,
        fingerprint: &ToolCallFingerprint,
        source_result: &(String, ExecutionResultStatus),
    ) -> Option<(String, ExecutionResultStatus)> {
        if !matches!(source_result.1, ExecutionResultStatus::Succeeded) {
            return None;
        }
        let source = self.successful_idempotent_calls.get(fingerprint)?;
        Some((
            reused_result(
                &canonical_tool_call_name(&tool_call.function.name),
                source,
                "duplicate_idempotent_call",
                "本轮已复用相同只读工具的成功结果，未再次执行。",
            ),
            ExecutionResultStatus::Succeeded,
        ))
    }
}

fn idempotent_fingerprint(
    tool_call: &ChatToolCall,
    canonical_name: &str,
    tool_registry: Option<&ToolRegistry>,
) -> Option<ToolCallFingerprint> {
    if !is_idempotent_read_tool(canonical_name, tool_registry) {
        return None;
    }
    let arguments = serde_json::from_str::<Value>(&tool_call.function.arguments).ok()?;
    Some(ToolCallFingerprint {
        tool_name: canonical_name.to_string(),
        canonical_arguments: canonical_json(&arguments),
    })
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            let rendered = entries
                .into_iter()
                .map(|(key, value)| format!("{}:{}", serde_json::json!(key), canonical_json(value)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{rendered}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}

fn is_idempotent_read_tool(tool_name: &str, tool_registry: Option<&ToolRegistry>) -> bool {
    BuiltinToolName::from_name(tool_name).is_some_and(|tool| tool.is_idempotent_read_operation())
        || tool_registry.is_some_and(|registry| registry.is_idempotent_read_tool(tool_name))
}

fn explicit_single_call_budgets(goal: &str) -> BTreeMap<String, usize> {
    let normalized = goal.trim().to_ascii_lowercase();
    let mut budgets = BTreeMap::new();

    for tool in BuiltinToolName::ALL {
        let name = tool.as_str();
        let chinese = [
            format!("只调用一次 {name}"),
            format!("仅调用一次 {name}"),
            format!("只使用一次 {name}"),
            format!("仅使用一次 {name}"),
            format!("调用一次 {name}"),
            format!("使用一次 {name}"),
            format!("{name} 只调用一次"),
            format!("{name} 仅调用一次"),
        ];
        let english = [
            format!("only call {name} once"),
            format!("call {name} only once"),
            format!("call {name} exactly once"),
            format!("only use {name} once"),
            format!("use {name} only once"),
            format!("use {name} exactly once"),
        ];
        if chinese.iter().any(|pattern| goal.contains(pattern))
            || english.iter().any(|pattern| normalized.contains(pattern))
        {
            budgets.insert(name.to_string(), 1);
        }
    }
    budgets
}

fn reused_result(tool_name: &str, source_result: &str, reason: &str, message: &str) -> String {
    let source_result = serde_json::from_str::<Value>(source_result)
        .unwrap_or_else(|_| Value::String(source_result.to_string()));
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "reused",
        "reason": reason,
        "message": message,
        "source_result": source_result,
    })
    .to_string()
}

fn budget_exhausted_result(tool_name: &str, limit: usize) -> String {
    serde_json::json!({
        "tool": tool_name,
        "status": "succeeded",
        "execution": "skipped",
        "reason": "tool_call_budget_exhausted",
        "message": format!("用户已要求本轮 {tool_name} 最多调用 {limit} 次；预算已用尽，请基于已有结果继续回答。"),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;

    fn call(id: &str, name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }
    }

    #[test]
    fn reuses_successful_read_only_call_across_model_rounds() {
        let mut ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
        let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("first read call must execute");
        };
        let result = (
            r#"{"tool":"web_search","status":"succeeded","results":["Rust"]}"#.to_string(),
            ExecutionResultStatus::Succeeded,
        );
        ledger.record_execution(&first, fingerprint.as_ref(), &result);

        let repeat = call("call-2", "web_search", r#"{"query":"Rust"}"#);
        assert!(matches!(
            ledger.plan(&[repeat], None)[0],
            ToolCallExecutionDecision::Reuse { .. }
        ));
    }

    #[test]
    fn treats_object_key_order_as_the_same_idempotent_call() {
        let ledger = ToolExecutionLedger::for_task_goal("搜索 Rust");
        let calls = [
            call(
                "call-1",
                "web_search",
                r#"{"query":"Rust","locale":"zh-CN"}"#,
            ),
            call(
                "call-2",
                "web_search",
                r#"{"locale":"zh-CN","query":"Rust"}"#,
            ),
        ];
        let plan = ledger.plan(&calls, None);
        assert!(matches!(
            plan[1],
            ToolCallExecutionDecision::ReuseAfterExecution {
                source_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn honors_explicit_single_call_budget_for_different_arguments() {
        let mut ledger =
            ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
        let first = call("call-1", "web_search", r#"{"query":"Rust"}"#);
        let first_plan = ledger.plan(std::slice::from_ref(&first), None);
        let ToolCallExecutionDecision::Execute { fingerprint } = &first_plan[0] else {
            panic!("first call must execute");
        };
        ledger.record_execution(
            &first,
            fingerprint.as_ref(),
            &("ok".to_string(), ExecutionResultStatus::Succeeded),
        );

        let second = call("call-2", "web_search", r#"{"query":"Cargo"}"#);
        assert!(matches!(
            ledger.plan(&[second], None)[0],
            ToolCallExecutionDecision::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn applies_single_call_budget_within_one_model_response() {
        let ledger =
            ToolExecutionLedger::for_task_goal("请只调用一次 web_search，收到结果后回答。");
        let calls = [
            call("call-1", "web_search", r#"{"query":"Rust"}"#),
            call("call-2", "web_search", r#"{"query":"Cargo"}"#),
        ];
        let plan = ledger.plan(&calls, None);
        assert!(matches!(plan[0], ToolCallExecutionDecision::Execute { .. }));
        assert!(matches!(
            plan[1],
            ToolCallExecutionDecision::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn never_deduplicates_write_or_process_operations() {
        let ledger = ToolExecutionLedger::for_task_goal("执行写入");
        let calls = [
            call("call-1", "file_write", r#"{"path":"a","content":"x"}"#),
            call("call-2", "file_write", r#"{"path":"a","content":"x"}"#),
        ];
        assert!(
            ledger
                .plan(&calls, None)
                .iter()
                .all(|decision| matches!(decision, ToolCallExecutionDecision::Execute { .. }))
        );
    }
}

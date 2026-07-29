use magi_bridge_client::{ChatMessage, ChatToolCall, ChatToolDefinition};
use magi_core::public_runtime_excerpt;
use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) const TOOL_CALL_FAILURE_SCHEMA_VERSION: &str = "tool-call-failure.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolCallValidationIssue {
    pub(crate) code: String,
    pub(crate) reason_code: String,
    pub(crate) tool_name: String,
    pub(crate) message: String,
    pub(crate) missing_fields: Vec<String>,
    pub(crate) arguments_preview: String,
    expected_input_schema: Option<Value>,
}

impl ToolCallValidationIssue {
    pub(crate) fn model_feedback(&self) -> String {
        let instruction = if self.reason_code == "tool_not_available" {
            "该工具不在本轮可用工具列表中；请改用本轮已提供的工具，或直接返回文本结果。"
        } else {
            "请根据 expected_input_schema 重新生成完整参数；不要重复提交相同的无效调用。"
        };
        serde_json::json!({
            "schema_version": "tool-call-validation.v1",
            "status": "rejected",
            "error_code": self.code,
            "reason_code": self.reason_code,
            "stage": "tool_call_validation",
            "tool": self.tool_name,
            "message": self.message,
            "missing_fields": self.missing_fields,
            "received_arguments": self.arguments_preview,
            "expected_input_schema": self.expected_input_schema,
            "instruction": instruction,
        })
        .to_string()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InvalidToolCall {
    pub(crate) call: ChatToolCall,
    pub(crate) issue: ToolCallValidationIssue,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolCallValidationBatch {
    pub(crate) valid_calls: Vec<ChatToolCall>,
    pub(crate) invalid_calls: Vec<InvalidToolCall>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolCallValidationTracker {
    invalid_rounds: usize,
}

impl ToolCallValidationTracker {
    pub(crate) fn record_round(&mut self) -> usize {
        self.invalid_rounds = self.invalid_rounds.saturating_add(1);
        self.invalid_rounds
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolCallFailureDiagnostic {
    pub(crate) schema_version: &'static str,
    pub(crate) code: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) stage: &'static str,
    pub(crate) tool_name: String,
    pub(crate) reason_code: String,
    pub(crate) missing_fields: Vec<String>,
    pub(crate) arguments_preview: String,
    pub(crate) retry_attempts: usize,
}

impl ToolCallFailureDiagnostic {
    pub(crate) fn repeated(issue: &ToolCallValidationIssue, retry_attempts: usize) -> Self {
        let missing_fields = if issue.missing_fields.is_empty() {
            "无".to_string()
        } else {
            issue.missing_fields.join(", ")
        };
        let summary = if issue.reason_code == "tool_not_available" {
            format!(
                "模型连续调用本轮未提供的 {} 工具；工具未执行，本轮已停止。",
                issue.tool_name
            )
        } else {
            format!(
                "模型连续提交无效的 {} 工具参数；工具未执行，本轮已停止。",
                issue.tool_name
            )
        };
        Self {
            schema_version: TOOL_CALL_FAILURE_SCHEMA_VERSION,
            code: issue.code.clone(),
            summary,
            detail: format!(
                "工具：{}\n失败阶段：tool_call_validation\n直接原因：{}\n缺失字段：{}\n收到的参数：{}",
                issue.tool_name, issue.message, missing_fields, issue.arguments_preview
            ),
            stage: "tool_call_validation",
            tool_name: issue.tool_name.clone(),
            reason_code: issue.reason_code.clone(),
            missing_fields: issue.missing_fields.clone(),
            arguments_preview: issue.arguments_preview.clone(),
            retry_attempts,
        }
    }
}

pub(crate) fn validate_tool_call_batch(
    tool_calls: &[ChatToolCall],
    definitions: &[ChatToolDefinition],
) -> ToolCallValidationBatch {
    let mut batch = ToolCallValidationBatch::default();
    for call in tool_calls {
        match validate_tool_call(call, definitions) {
            Ok(()) => batch.valid_calls.push(call.clone()),
            Err(issue) => batch.invalid_calls.push(InvalidToolCall {
                call: call.clone(),
                issue: *issue,
            }),
        }
    }
    batch
}

pub(crate) fn invalid_tool_result_message(invalid: &InvalidToolCall) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(invalid.issue.model_feedback()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(invalid.call.id.clone()),
    }
}

fn validate_tool_call(
    call: &ChatToolCall,
    definitions: &[ChatToolDefinition],
) -> Result<(), Box<ToolCallValidationIssue>> {
    let definition = definitions
        .iter()
        .find(|definition| definition.function.name == call.function.name);
    if definition.is_none() {
        return Err(Box::new(validation_issue(
            call,
            "tool_not_available",
            format!("当前运行环境未提供工具 {}。", call.function.name),
            Vec::new(),
            None,
        )));
    }
    let expected_input_schema = definition.map(|definition| definition.function.parameters.clone());

    let raw_arguments = call.function.arguments.trim();
    if raw_arguments.is_empty() {
        return Err(Box::new(validation_issue(
            call,
            "tool_arguments_empty",
            "工具调用没有提供参数。".to_string(),
            required_fields_for_empty_call(&call.function.name, expected_input_schema.as_ref()),
            expected_input_schema,
        )));
    }

    let arguments = serde_json::from_str::<Value>(raw_arguments).map_err(|error| {
        Box::new(validation_issue(
            call,
            "tool_arguments_invalid_json",
            format!("工具参数不是有效 JSON：{error}"),
            Vec::new(),
            expected_input_schema.clone(),
        ))
    })?;
    let object = arguments.as_object().ok_or_else(|| {
        Box::new(validation_issue(
            call,
            "tool_arguments_not_object",
            "工具参数必须是 JSON 对象。".to_string(),
            Vec::new(),
            expected_input_schema.clone(),
        ))
    })?;

    let mut missing_fields = expected_input_schema
        .as_ref()
        .map(|schema| schema_missing_required_fields(object, schema))
        .unwrap_or_default();
    missing_fields.extend(shell_exec_missing_fields(&call.function.name, object));
    missing_fields.sort();
    missing_fields.dedup();
    if !missing_fields.is_empty() {
        return Err(Box::new(validation_issue(
            call,
            "tool_arguments_missing_required",
            format!("缺少必填参数：{}。", missing_fields.join(", ")),
            missing_fields,
            expected_input_schema,
        )));
    }

    Ok(())
}

fn required_fields_for_empty_call(tool_name: &str, schema: Option<&Value>) -> Vec<String> {
    let mut required = schema
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if tool_name == "shell_exec" && !required.iter().any(|field| field == "command") {
        required.push("command".to_string());
    }
    required
}

fn schema_missing_required_fields(object: &Map<String, Value>, schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|field| !field_has_required_value(object.get(*field)))
        .map(str::to_string)
        .collect()
}

fn shell_exec_missing_fields(tool_name: &str, object: &Map<String, Value>) -> Vec<String> {
    if tool_name != "shell_exec" {
        return Vec::new();
    }
    let action = object
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let has_command = field_has_required_value(object.get("command"));
    let has_terminal_id = object
        .get("terminal_id")
        .is_some_and(|value| value.is_number());

    match action.as_deref() {
        Some("read" | "write" | "kill") if !has_terminal_id => vec!["terminal_id".to_string()],
        Some("list") => Vec::new(),
        None if has_terminal_id && !has_command => Vec::new(),
        None | Some("run") if !has_command => vec!["command".to_string()],
        _ => Vec::new(),
    }
}

fn field_has_required_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(_) => true,
    }
}

fn validation_issue(
    call: &ChatToolCall,
    reason_code: &str,
    message: String,
    missing_fields: Vec<String>,
    expected_input_schema: Option<Value>,
) -> ToolCallValidationIssue {
    let arguments_preview = arguments_preview(&call.function.arguments);
    ToolCallValidationIssue {
        code: if reason_code == "tool_not_available" {
            reason_code.to_string()
        } else {
            "tool_arguments_invalid".to_string()
        },
        reason_code: reason_code.to_string(),
        tool_name: call.function.name.clone(),
        message,
        missing_fields,
        arguments_preview,
        expected_input_schema,
    }
}

fn arguments_preview(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    public_runtime_excerpt(trimmed, 512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::{ChatToolFunction, ChatToolFunctionDefinition, ChatToolOrigin};

    fn definition(name: &str, parameters: Value) -> ChatToolDefinition {
        ChatToolDefinition {
            kind: "function".to_string(),
            function: ChatToolFunctionDefinition {
                name: name.to_string(),
                description: "test".to_string(),
                parameters,
            },
            origin: ChatToolOrigin::Builtin,
        }
    }

    fn call(name: &str, arguments: &str) -> ChatToolCall {
        ChatToolCall {
            id: "call-1".to_string(),
            kind: "function".to_string(),
            function: ChatToolFunction {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }
    }

    #[test]
    fn empty_shell_call_is_rejected_before_execution() {
        let definitions = vec![definition(
            "shell_exec",
            serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": []
            }),
        )];

        let batch = validate_tool_call_batch(&[call("shell_exec", "")], &definitions);

        assert!(batch.valid_calls.is_empty());
        assert_eq!(batch.invalid_calls.len(), 1);
        assert_eq!(
            batch.invalid_calls[0].issue.reason_code,
            "tool_arguments_empty"
        );
        assert_eq!(batch.invalid_calls[0].issue.missing_fields, ["command"]);
    }

    #[test]
    fn shell_process_control_call_does_not_require_command() {
        let definitions = vec![definition(
            "shell_exec",
            serde_json::json!({"type": "object", "required": []}),
        )];

        let batch = validate_tool_call_batch(
            &[call("shell_exec", r#"{"action":"read","terminal_id":7}"#)],
            &definitions,
        );

        assert_eq!(batch.valid_calls.len(), 1);
        assert!(batch.invalid_calls.is_empty());
    }

    #[test]
    fn schema_required_field_rejects_blank_string() {
        let definitions = vec![definition(
            "file_read",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        )];

        let batch =
            validate_tool_call_batch(&[call("file_read", r#"{"path":"  "}"#)], &definitions);

        assert_eq!(batch.invalid_calls[0].issue.missing_fields, ["path"]);
    }

    #[test]
    fn validation_tracker_counts_invalid_model_rounds() {
        let mut tracker = ToolCallValidationTracker::default();

        assert_eq!(tracker.record_round(), 1);
        assert_eq!(tracker.record_round(), 2);
    }

    #[test]
    fn tool_call_is_rejected_when_current_round_exposes_no_tools() {
        let batch = validate_tool_call_batch(&[call("shell_exec", r#"{"command":"pwd"}"#)], &[]);

        assert!(batch.valid_calls.is_empty());
        assert_eq!(batch.invalid_calls.len(), 1);
        let issue = &batch.invalid_calls[0].issue;
        assert_eq!(issue.code, "tool_not_available");
        assert_eq!(issue.reason_code, "tool_not_available");
        assert!(issue.message.contains("未提供工具 shell_exec"));
        let model_feedback = issue.model_feedback();
        assert!(model_feedback.contains("改用本轮已提供的工具"));
        assert!(!model_feedback.contains("expected_input_schema 重新生成完整参数"));

        let diagnostic = ToolCallFailureDiagnostic::repeated(issue, 1);
        assert_eq!(diagnostic.code, "tool_not_available");
        assert!(diagnostic.summary.contains("本轮未提供"));
        assert!(!diagnostic.summary.contains("工具参数"));
    }
}

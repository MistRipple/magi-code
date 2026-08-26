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

    pub(crate) fn record_valid_round(&mut self) {
        self.invalid_rounds = 0;
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
    pub(crate) fn non_retryable(issue: &ToolCallValidationIssue) -> Self {
        Self {
            schema_version: TOOL_CALL_FAILURE_SCHEMA_VERSION,
            code: issue.code.clone(),
            summary: format!(
                "模型调用了本轮不可用的 {} 工具；工具未执行，已停止继续重试。",
                issue.tool_name
            ),
            detail: format!(
                "工具：{}\n失败阶段：tool_call_validation\n直接原因：{}\n本轮工具面未提供该工具，继续调用不会改变结果。",
                issue.tool_name, issue.message
            ),
            stage: "tool_call_validation",
            tool_name: issue.tool_name.clone(),
            reason_code: issue.reason_code.clone(),
            missing_fields: issue.missing_fields.clone(),
            arguments_preview: issue.arguments_preview.clone(),
            retry_attempts: 0,
        }
    }

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

pub(crate) fn non_retryable_tool_call_failure(
    issue: &ToolCallValidationIssue,
) -> Option<ToolCallFailureDiagnostic> {
    (issue.reason_code == "tool_not_available")
        .then(|| ToolCallFailureDiagnostic::non_retryable(issue))
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
        provider_context: Vec::new(),
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

    if let Some(schema) = expected_input_schema.as_ref()
        && let Err(message) = validate_json_schema_value(&arguments, schema)
    {
        return Err(Box::new(validation_issue(
            call,
            "tool_arguments_schema_mismatch",
            message,
            Vec::new(),
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
    let properties = schema.get("properties").and_then(Value::as_object);
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|field| {
            !field_has_required_value(
                object.get(*field),
                properties.and_then(|properties| properties.get(*field)),
            )
        })
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
    let has_command = field_has_required_value(object.get("command"), None);
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

fn field_has_required_value(value: Option<&Value>, schema: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::Null) => schema.is_some_and(schema_allows_null),
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(_) => true,
    }
}

fn schema_allows_null(schema: &Value) -> bool {
    let type_allows_null = match schema.get("type") {
        Some(Value::String(value)) => value == "null",
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some("null")),
        _ => false,
    };
    type_allows_null
        || ["anyOf", "oneOf"]
            .into_iter()
            .filter_map(|keyword| schema.get(keyword).and_then(Value::as_array))
            .flatten()
            .any(schema_allows_null)
}

fn validate_json_schema_value(value: &Value, schema: &Value) -> Result<(), String> {
    validate_json_schema_value_at(value, schema, "$".to_string())
}

/// 执行层之前只需要一组稳定、无外部依赖的 JSON Schema 子集校验。
///
/// 这里覆盖工具目录实际使用的类型、枚举、必填字段、数组/字符串长度以及
/// anyOf/oneOf/not 条件。Schema 仍然是协议边界的唯一来源，不能为某个工具
/// 另写一份参数判断，否则模型看到的契约和执行前契约会再次分叉。
fn validate_json_schema_value_at(
    value: &Value,
    schema: &Value,
    path: String,
) -> Result<(), String> {
    if let Some(type_value) = schema.get("type") {
        let type_matches = match type_value {
            Value::String(expected) => json_type_matches(value, expected),
            Value::Array(expected_types) => expected_types
                .iter()
                .filter_map(Value::as_str)
                .any(|expected| json_type_matches(value, expected)),
            _ => true,
        };
        if !type_matches {
            return Err(format!("{path} 的类型不符合 schema 要求：{}", type_value));
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(format!("{path} 的值不在允许的枚举范围内"));
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        return Err(format!("{path} 的值不符合固定参数约束"));
    }

    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64)
        && value
            .as_str()
            .is_some_and(|text| text.chars().count() < min_length as usize)
    {
        return Err(format!("{path} 的文本不能为空或长度不足 {min_length}"));
    }
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64)
        && value
            .as_str()
            .is_some_and(|text| text.chars().count() > max_length as usize)
    {
        return Err(format!("{path} 的文本长度不能超过 {max_length}"));
    }

    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
            && number < minimum
        {
            return Err(format!("{path} 必须大于或等于 {minimum}"));
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
            && number > maximum
        {
            return Err(format!("{path} 必须小于或等于 {maximum}"));
        }
        if let Some(exclusive_minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
            && number <= exclusive_minimum
        {
            return Err(format!("{path} 必须大于 {exclusive_minimum}"));
        }
        if let Some(exclusive_maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
            && number >= exclusive_maximum
        {
            return Err(format!("{path} 必须小于 {exclusive_maximum}"));
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} 至少需要 {min_items} 项"));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && array.len() > max_items as usize
        {
            return Err(format!("{path} 最多允许 {max_items} 项"));
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema_value_at(item, item_schema, format!("{path}[{index}]"))?;
            }
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    return Err(format!("{path} 缺少必填参数 {field}"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (field, field_schema) in properties {
                if let Some(field_value) = object.get(field) {
                    validate_json_schema_value_at(
                        field_value,
                        field_schema,
                        format!("{path}.{field}"),
                    )?;
                }
            }
        }
    }

    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array)
        && !any_of
            .iter()
            .any(|candidate| validate_json_schema_value_at(value, candidate, path.clone()).is_ok())
    {
        return Err(format!("{path} 不符合 anyOf 中的任何参数分支"));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = one_of
            .iter()
            .filter(|candidate| {
                validate_json_schema_value_at(value, candidate, path.clone()).is_ok()
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "{path} 必须且只能符合 oneOf 中的一个参数分支（当前符合 {matches} 个）"
            ));
        }
    }
    if let Some(not_schema) = schema.get("not")
        && validate_json_schema_value_at(value, not_schema, path.clone()).is_ok()
    {
        return Err(format!("{path} 命中了 schema 禁止的参数组合"));
    }

    Ok(())
}

fn json_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
        }
        _ => true,
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
    fn schema_required_field_accepts_explicit_null_when_property_is_nullable() {
        let definitions = vec![definition(
            "create_goal",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "objective": {"type": "string"},
                    "token_budget": {
                        "anyOf": [
                            {"type": "integer", "minimum": 16000},
                            {"type": "null"}
                        ]
                    }
                },
                "required": ["objective", "token_budget"]
            }),
        )];

        let batch = validate_tool_call_batch(
            &[call(
                "create_goal",
                r#"{"objective":"读取 Cargo.toml","token_budget":null}"#,
            )],
            &definitions,
        );

        assert_eq!(batch.valid_calls.len(), 1);
        assert!(batch.invalid_calls.is_empty());
    }

    #[test]
    fn validation_tracker_counts_invalid_model_rounds() {
        let mut tracker = ToolCallValidationTracker::default();

        assert_eq!(tracker.record_round(), 1);
        assert_eq!(tracker.record_round(), 2);
        tracker.record_valid_round();
        assert_eq!(tracker.record_round(), 1);
        assert_eq!(tracker.record_round(), 2);
    }

    #[test]
    fn conditional_schema_rejects_file_patch_mixed_input_before_execution() {
        let definition = definition(
            "file_patch",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "minLength": 1},
                    "old_string": {"type": "string", "minLength": 1},
                    "new_string": {"type": "string"},
                    "patches": {
                        "type": "array",
                        "minItems": 1,
                        "items": {"type": "object", "required": ["old_string", "new_string"]}
                    }
                },
                "required": ["path"],
                "oneOf": [
                    {"required": ["old_string", "new_string"], "not": {"required": ["patches"]}},
                    {"required": ["patches"], "not": {"anyOf": [{"required": ["old_string"]}, {"required": ["new_string"]}]}}
                ]
            }),
        );
        let batch = validate_tool_call_batch(
            &[call(
                "file_patch",
                r#"{"path":"src/lib.rs","old_string":"a","new_string":"b","patches":[{"old_string":"x","new_string":"y"}]}"#,
            )],
            &[definition],
        );

        assert!(batch.valid_calls.is_empty());
        assert_eq!(batch.invalid_calls.len(), 1);
        assert_eq!(
            batch.invalid_calls[0].issue.reason_code,
            "tool_arguments_schema_mismatch"
        );
    }

    #[test]
    fn conditional_schema_accepts_file_patch_single_and_batch_forms() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "minLength": 1},
                "old_string": {"type": "string", "minLength": 1},
                "new_string": {"type": "string"},
                "patches": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["old_string", "new_string"]}}
            },
            "required": ["path"],
            "oneOf": [
                {"required": ["old_string", "new_string"], "not": {"required": ["patches"]}},
                {"required": ["patches"], "not": {"anyOf": [{"required": ["old_string"]}, {"required": ["new_string"]}]}}
            ]
        });
        let definitions = [definition("file_patch", schema)];
        for arguments in [
            r#"{"path":"src/lib.rs","old_string":"a","new_string":""}"#,
            r#"{"path":"src/lib.rs","patches":[{"old_string":"a","new_string":"b"}]}"#,
        ] {
            let batch = validate_tool_call_batch(&[call("file_patch", arguments)], &definitions);
            assert!(batch.invalid_calls.is_empty(), "{arguments}");
        }
    }

    #[test]
    fn conditional_schema_rejects_navigation_cache_flag_on_url_action() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["url", "back", "forward", "reload"]},
                "url": {"type": "string", "minLength": 1},
                "ignore_cache": {"type": "boolean"}
            },
            "oneOf": [
                {"required": ["url"], "properties": {"action": {"enum": ["url"]}}, "not": {"required": ["ignore_cache"]}},
                {"required": ["action"], "properties": {"action": {"enum": ["reload"]}}}
            ]
        });
        let batch = validate_tool_call_batch(
            &[call(
                "browser_navigate",
                r#"{"url":"http://localhost:4173","ignore_cache":true}"#,
            )],
            &[definition("browser_navigate", schema)],
        );

        assert!(batch.valid_calls.is_empty());
        assert_eq!(batch.invalid_calls.len(), 1);
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

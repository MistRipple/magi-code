use crate::llm_types::{LlmContentBlock, LlmMessageContent, LlmMessageParams};

#[derive(Clone, Debug)]
pub struct ConformanceViolation {
    pub rule: String,
    pub message: String,
    pub severity: ViolationSeverity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViolationSeverity {
    Error,
    Warning,
}

pub struct ConformanceValidator;

impl ConformanceValidator {
    pub fn validate(params: &LlmMessageParams) -> Vec<ConformanceViolation> {
        let mut violations = Vec::new();

        if params.messages.is_empty() {
            violations.push(ConformanceViolation {
                rule: "non_empty_messages".to_string(),
                message: "messages array must not be empty".to_string(),
                severity: ViolationSeverity::Error,
            });
        }

        let mut prev_role: Option<&str> = None;
        for msg in &params.messages {
            if msg.role == "system" {
                if prev_role.is_some() && prev_role != Some("system") {
                    violations.push(ConformanceViolation {
                        rule: "system_message_position".to_string(),
                        message: "system messages should be at the beginning".to_string(),
                        severity: ViolationSeverity::Warning,
                    });
                }
            }
            prev_role = Some(&msg.role);
        }

        for (i, msg) in params.messages.iter().enumerate() {
            if msg.role == "assistant" {
                if let LlmMessageContent::Blocks(blocks) = &msg.content {
                    let has_tool_use = blocks
                        .iter()
                        .any(|b| matches!(b, LlmContentBlock::ToolUse { .. }));
                    if has_tool_use {
                        let next = params.messages.get(i + 1);
                        let has_result = next.map_or(false, |n| {
                            n.role == "user"
                                && matches!(&n.content, LlmMessageContent::Blocks(bs) if bs.iter().any(|b| matches!(b, LlmContentBlock::ToolResult { .. })))
                        });
                        if !has_result {
                            violations.push(ConformanceViolation {
                                rule: "tool_use_result_pairing".to_string(),
                                message: format!(
                                    "assistant message at index {} has tool_use without following tool_result",
                                    i
                                ),
                                severity: ViolationSeverity::Error,
                            });
                        }
                    }
                }
            }
        }

        if let Some(ref tools) = params.tools {
            for tool in tools {
                if tool.name.is_empty() {
                    violations.push(ConformanceViolation {
                        rule: "tool_name_required".to_string(),
                        message: "tool definition must have a name".to_string(),
                        severity: ViolationSeverity::Error,
                    });
                }
            }
        }

        violations
    }

    pub fn has_errors(violations: &[ConformanceViolation]) -> bool {
        violations
            .iter()
            .any(|v| v.severity == ViolationSeverity::Error)
    }
}

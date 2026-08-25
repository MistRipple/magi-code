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

        let mut seen_non_instruction = false;
        for msg in &params.messages {
            let is_instruction = matches!(msg.role.as_str(), "system" | "developer");
            if is_instruction && seen_non_instruction {
                violations.push(ConformanceViolation {
                    rule: "instruction_message_position".to_string(),
                    message: "system/developer messages should be in the stable prefix".to_string(),
                    severity: ViolationSeverity::Warning,
                });
            }
            seen_non_instruction |= !is_instruction;
        }

        for (i, msg) in params.messages.iter().enumerate() {
            if msg.role == "assistant"
                && let LlmMessageContent::Blocks(blocks) = &msg.content
            {
                let has_tool_use = blocks
                    .iter()
                    .any(|b| matches!(b, LlmContentBlock::ToolUse { .. }));
                if has_tool_use {
                    let next = params.messages.get(i + 1);
                    let has_result = next.is_some_and(|n| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{LlmMessage, LlmMessageContent, LlmMessageParams};

    fn params(messages: Vec<LlmMessage>) -> LlmMessageParams {
        LlmMessageParams {
            messages,
            max_tokens: None,
            temperature: None,
            tools: None,
            stream: None,
            system_prompt: None,
            tool_choice: None,
            reasoning_effort: None,
        }
    }

    fn message(role: &str, content: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: LlmMessageContent::Text(content.to_string()),
        }
    }

    #[test]
    fn developer_messages_are_valid_instruction_prefix() {
        let violations = ConformanceValidator::validate(&params(vec![
            message("system", "平台规则"),
            message("developer", "当前权限"),
            message("user", "本轮任务"),
            message("assistant", "开始处理"),
        ]));

        assert!(!ConformanceValidator::has_errors(&violations));
        assert!(
            !violations
                .iter()
                .any(|violation| violation.rule == "instruction_message_position")
        );
    }

    #[test]
    fn developer_after_transcript_is_reported_as_position_warning() {
        let violations = ConformanceValidator::validate(&params(vec![
            message("user", "本轮任务"),
            message("developer", "迟到的运行时规则"),
        ]));

        let warning = violations
            .iter()
            .find(|violation| violation.rule == "instruction_message_position")
            .expect("后置 developer 必须被发现");
        assert_eq!(warning.severity, ViolationSeverity::Warning);
        assert!(!ConformanceValidator::has_errors(&violations));
    }
}

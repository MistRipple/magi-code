use std::{sync::Arc, thread};

use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, LlmResponse, LlmUsage,
    ToolCall, ToolResult, parse_tool_arguments,
};
use crate::tool_concurrency::{
    ToolBatchKind, ToolConcurrencyInput, partition_tool_calls_with_inputs,
};
use crate::types::{BridgeClientError, ModelBridgeClient, ModelInvocationRequest};

#[derive(Clone, Debug)]
pub struct AdapterConfig {
    pub max_rounds: u32,
    pub max_tokens_per_round: Option<u32>,
    pub context_window: u32,
    pub buffer_tokens: u32,
    pub enable_summary_hijack_guard: bool,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            max_rounds: 50,
            max_tokens_per_round: Some(4096),
            context_window: 128_000,
            buffer_tokens: 13_000,
            enable_summary_hijack_guard: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoundResult {
    pub round: u32,
    pub response: LlmResponse,
    pub tool_calls: Vec<ToolCall>,
    pub had_tool_calls: bool,
}

pub trait ToolExecutor: Send + Sync {
    fn execute(&self, tool_call: &ToolCall) -> ToolResult;
}

pub(crate) fn execute_tool_calls(
    tool_calls: &[ToolCall],
    tool_executor: &dyn ToolExecutor,
) -> Vec<ToolResult> {
    let tool_inputs = tool_calls
        .iter()
        .map(|tool_call| ToolConcurrencyInput {
            tool_name: tool_call.name.as_str(),
            arguments: Some(&tool_call.arguments),
        })
        .collect::<Vec<_>>();
    let mut results = vec![None; tool_calls.len()];

    for batch in partition_tool_calls_with_inputs(&tool_inputs) {
        match batch.kind {
            ToolBatchKind::Serial => {
                for tool_index in batch.tool_indices {
                    results[tool_index] = Some(tool_executor.execute(&tool_calls[tool_index]));
                }
            }
            ToolBatchKind::Concurrent => {
                thread::scope(|scope| {
                    let handles = batch
                        .tool_indices
                        .iter()
                        .copied()
                        .map(|tool_index| {
                            let tool_call = &tool_calls[tool_index];
                            (
                                tool_index,
                                scope.spawn(move || tool_executor.execute(tool_call)),
                            )
                        })
                        .collect::<Vec<_>>();

                    for (tool_index, handle) in handles {
                        let result = handle.join().unwrap_or_else(|_| ToolResult {
                            tool_call_id: tool_calls[tool_index].id.clone(),
                            content: "工具执行线程异常".to_string(),
                            is_error: true,
                            standardized: None,
                            file_change: None,
                        });
                        results[tool_index] = Some(result);
                    }
                });
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(tool_index, result)| {
            result.unwrap_or_else(|| ToolResult {
                tool_call_id: tool_calls[tool_index].id.clone(),
                content: "工具执行结果缺失".to_string(),
                is_error: true,
                standardized: None,
                file_change: None,
            })
        })
        .collect()
}

pub struct BaseAdapter {
    config: AdapterConfig,
    model_client: Arc<dyn ModelBridgeClient>,
}

impl BaseAdapter {
    pub fn new(model_client: Arc<dyn ModelBridgeClient>, config: AdapterConfig) -> Self {
        Self {
            config,
            model_client,
        }
    }

    pub fn invoke_llm(&self, params: &LlmMessageParams) -> Result<LlmResponse, BridgeClientError> {
        let prompt = build_prompt_from_params(params);
        let request = ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt,
            messages: Some(
                params
                    .messages
                    .iter()
                    .map(|m| crate::types::ChatMessage {
                        role: m.role.clone(),
                        content: match &m.content {
                            LlmMessageContent::Text(t) => Some(t.clone()),
                            LlmMessageContent::Blocks(blocks) => {
                                let text: String = blocks
                                    .iter()
                                    .filter_map(|b| match b {
                                        LlmContentBlock::Text { text } => Some(text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if text.is_empty() { None } else { Some(text) }
                            }
                        },
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    })
                    .collect(),
            ),
            tools: params.tools.as_ref().map(|tools| {
                tools
                    .iter()
                    .map(|t| crate::types::ChatToolDefinition {
                        kind: "function".to_string(),
                        function: crate::types::ChatToolFunctionDefinition {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: serde_json::json!({
                                "type": t.input_schema.kind,
                                "properties": t.input_schema.properties,
                                "required": t.input_schema.required,
                            }),
                        },
                    })
                    .collect()
            }),
            tool_choice: None,
        };

        let bridge_response = self.model_client.invoke(request)?;
        let payload = bridge_response.parse_chat_payload();

        Ok(LlmResponse {
            content: payload.content.unwrap_or_default(),
            thinking: payload.thinking,
            tool_calls: payload
                .tool_calls
                .into_iter()
                .map(|tc| {
                    let raw_arguments = tc.function.arguments;
                    let (arguments, argument_parse_error) = parse_tool_arguments(&raw_arguments);
                    ToolCall {
                        id: tc.id,
                        name: tc.function.name,
                        arguments,
                        argument_parse_error,
                        raw_arguments: Some(raw_arguments),
                    }
                })
                .collect(),
            usage: LlmUsage::default(),
            stop_reason: payload.finish_reason.unwrap_or_else(|| "stop".to_string()),
        })
    }

    pub fn run_tool_loop(
        &self,
        initial_params: &LlmMessageParams,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(LlmResponse, Vec<RoundResult>), BridgeClientError> {
        let mut params = initial_params.clone();
        let mut rounds = Vec::new();
        let mut round_num = 0u32;

        loop {
            round_num += 1;
            if round_num > self.config.max_rounds {
                let last_response = rounds
                    .last()
                    .map(|r: &RoundResult| r.response.clone())
                    .unwrap_or(LlmResponse {
                        content: "max rounds exceeded".to_string(),
                        thinking: None,
                        tool_calls: Vec::new(),
                        usage: LlmUsage::default(),
                        stop_reason: "max_rounds".to_string(),
                    });
                return Ok((last_response, rounds));
            }

            let response = self.invoke_llm(&params)?;

            let has_tool_calls = !response.tool_calls.is_empty();
            let round_result = RoundResult {
                round: round_num,
                response: response.clone(),
                tool_calls: response.tool_calls.clone(),
                had_tool_calls: has_tool_calls,
            };
            rounds.push(round_result);

            if !has_tool_calls {
                return Ok((response, rounds));
            }

            let assistant_blocks: Vec<LlmContentBlock> = {
                let mut blocks = Vec::new();
                if !response.content.is_empty() {
                    blocks.push(LlmContentBlock::Text {
                        text: response.content.clone(),
                    });
                }
                for tc in &response.tool_calls {
                    blocks.push(LlmContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.arguments.clone(),
                    });
                }
                blocks
            };

            params.messages.push(LlmMessage {
                role: "assistant".to_string(),
                content: LlmMessageContent::Blocks(assistant_blocks),
            });

            let result_blocks: Vec<LlmContentBlock> =
                execute_tool_calls(&response.tool_calls, tool_executor)
                    .into_iter()
                    .map(|result| LlmContentBlock::ToolResult {
                        tool_use_id: result.tool_call_id,
                        content: result.content,
                        is_error: result.is_error,
                    })
                    .collect();

            params.messages.push(LlmMessage {
                role: "user".to_string(),
                content: LlmMessageContent::Blocks(result_blocks),
            });
        }
    }

    pub fn config(&self) -> &AdapterConfig {
        &self.config
    }
}

fn build_prompt_from_params(params: &LlmMessageParams) -> String {
    if let Some(msg) = params.messages.last() {
        match &msg.content {
            LlmMessageContent::Text(t) => t.clone(),
            LlmMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    LlmContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    } else {
        String::new()
    }
}

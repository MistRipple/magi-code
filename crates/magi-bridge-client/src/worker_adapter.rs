use std::sync::Arc;

use crate::base_adapter::{AdapterConfig, BaseAdapter, RoundResult, ToolExecutor};
use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, LlmResponse, LlmUsage,
};
use crate::micro_compaction as mc;
use crate::orchestrator_termination::OrchestratorTerminationReason;
use crate::types::{BridgeClientError, ModelBridgeClient};

#[derive(Clone, Debug)]
pub struct WorkerAdapterConfig {
    pub adapter: AdapterConfig,
    pub enable_micro_compaction: bool,
    pub enable_duplicate_guard: bool,
    pub micro_compaction_threshold_tokens: u64,
}

impl Default for WorkerAdapterConfig {
    fn default() -> Self {
        Self {
            adapter: AdapterConfig {
                max_rounds: 30,
                ..AdapterConfig::default()
            },
            enable_micro_compaction: true,
            enable_duplicate_guard: true,
            micro_compaction_threshold_tokens: 80_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkerAdapterResult {
    pub final_response: LlmResponse,
    pub total_rounds: u32,
    pub tool_call_count: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub termination_reason: OrchestratorTerminationReason,
    pub rounds: Vec<RoundResult>,
    pub micro_compaction_count: u32,
    pub duplicate_guard_blocks: u32,
}

pub struct WorkerAdapter {
    config: WorkerAdapterConfig,
    base: BaseAdapter,
}

impl WorkerAdapter {
    pub fn new(
        model_client: Arc<dyn ModelBridgeClient>,
        config: WorkerAdapterConfig,
    ) -> Self {
        let base = BaseAdapter::new(model_client, config.adapter.clone());
        Self { config, base }
    }

    pub fn run(
        &self,
        initial_params: &LlmMessageParams,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<WorkerAdapterResult, BridgeClientError> {
        let mut params = initial_params.clone();
        let mut total_rounds = 0u32;
        let mut tool_call_count = 0u32;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;
        let mut all_rounds = Vec::new();
        let mut micro_compaction_count = 0u32;

        loop {
            if total_rounds >= self.config.adapter.max_rounds {
                let final_response = all_rounds
                    .last()
                    .map(|r: &RoundResult| r.response.clone())
                    .unwrap_or_else(default_response);
                return Ok(WorkerAdapterResult {
                    final_response,
                    total_rounds,
                    tool_call_count,
                    total_input_tokens,
                    total_output_tokens,
                    termination_reason: OrchestratorTerminationReason::Stalled,
                    rounds: all_rounds,
                    micro_compaction_count,
                    duplicate_guard_blocks: 0,
                });
            }

            if self.config.enable_micro_compaction {
                let estimated =
                    crate::protocol::estimate_message_tokens(&params.messages);
                if estimated > self.config.micro_compaction_threshold_tokens {
                    apply_micro_compaction(&mut params.messages);
                    micro_compaction_count += 1;
                }
            }

            total_rounds += 1;
            let response = self.base.invoke_llm(&params)?;

            total_input_tokens += response.usage.input_tokens;
            total_output_tokens += response.usage.output_tokens;

            let has_tool_calls = !response.tool_calls.is_empty();
            tool_call_count += response.tool_calls.len() as u32;

            let round_result = RoundResult {
                round: total_rounds,
                response: response.clone(),
                tool_calls: response.tool_calls.clone(),
                had_tool_calls: has_tool_calls,
            };
            all_rounds.push(round_result);

            if !has_tool_calls {
                return Ok(WorkerAdapterResult {
                    final_response: response,
                    total_rounds,
                    tool_call_count,
                    total_input_tokens,
                    total_output_tokens,
                    termination_reason: OrchestratorTerminationReason::Completed,
                    rounds: all_rounds,
                    micro_compaction_count,
                    duplicate_guard_blocks: 0,
                });
            }

            let mut assistant_blocks = Vec::new();
            if !response.content.is_empty() {
                assistant_blocks.push(LlmContentBlock::Text {
                    text: response.content.clone(),
                });
            }
            for tc in &response.tool_calls {
                assistant_blocks.push(LlmContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }
            params.messages.push(LlmMessage {
                role: "assistant".to_string(),
                content: LlmMessageContent::Blocks(assistant_blocks),
            });

            let result_blocks: Vec<LlmContentBlock> = response
                .tool_calls
                .iter()
                .map(|tc| {
                    let result = tool_executor.execute(tc);
                    LlmContentBlock::ToolResult {
                        tool_use_id: result.tool_call_id,
                        content: result.content,
                        is_error: result.is_error,
                    }
                })
                .collect();

            params.messages.push(LlmMessage {
                role: "user".to_string(),
                content: LlmMessageContent::Blocks(result_blocks),
            });
        }
    }
}

fn apply_micro_compaction(messages: &mut Vec<LlmMessage>) {
    let mut mc_messages: Vec<mc::LlmMessage> = messages
        .iter()
        .map(|m| mc::LlmMessage {
            role: m.role.clone(),
            content: match &m.content {
                LlmMessageContent::Text(t) => mc::LlmContent::Text(t.clone()),
                LlmMessageContent::Blocks(blocks) => {
                    mc::LlmContent::Blocks(
                        blocks
                            .iter()
                            .map(|b| match b {
                                LlmContentBlock::Text { text } => {
                                    mc::ContentBlock::Text { text: text.clone() }
                                }
                                LlmContentBlock::Image { source } => mc::ContentBlock::Image {
                                    media_type: source.media_type.clone(),
                                    data: source.data.clone(),
                                },
                                LlmContentBlock::ToolUse { id, name, input } => {
                                    mc::ContentBlock::ToolUse {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: input.clone(),
                                    }
                                }
                                LlmContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => mc::ContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: content.clone(),
                                    is_error: *is_error,
                                    tool_name: None,
                                    status: None,
                                },
                            })
                            .collect(),
                    )
                }
            },
        })
        .collect();

    mc::compact_old_tool_results(&mut mc_messages, 4, 200, mc::MicroCompactionMode::Compact);

    *messages = mc_messages
        .into_iter()
        .map(|m| LlmMessage {
            role: m.role,
            content: match m.content {
                mc::LlmContent::Text(t) => LlmMessageContent::Text(t),
                mc::LlmContent::Blocks(blocks) => {
                    LlmMessageContent::Blocks(
                        blocks
                            .into_iter()
                            .map(|b| match b {
                                mc::ContentBlock::Text { text } => {
                                    LlmContentBlock::Text { text }
                                }
                                mc::ContentBlock::Image { media_type, data } => {
                                    LlmContentBlock::Image {
                                        source: crate::llm_types::ImageSource {
                                            kind: "base64".to_string(),
                                            media_type,
                                            data,
                                        },
                                    }
                                }
                                mc::ContentBlock::ToolUse { id, name, input } => {
                                    LlmContentBlock::ToolUse { id, name, input }
                                }
                                mc::ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                    ..
                                } => LlmContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                },
                            })
                            .collect(),
                    )
                }
            },
        })
        .collect();
}

fn default_response() -> LlmResponse {
    LlmResponse {
        content: String::new(),
        tool_calls: Vec::new(),
        usage: LlmUsage::default(),
        stop_reason: "none".to_string(),
    }
}

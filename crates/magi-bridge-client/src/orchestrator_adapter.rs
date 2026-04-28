use std::sync::Arc;

use crate::base_adapter::{
    AdapterConfig, BaseAdapter, RoundResult, ToolExecutor, execute_tool_calls,
};
use crate::conversation_compaction::{ConversationCompactionConfig, ConversationCompactor};
use crate::decision_engine::{OrchestratorDecisionPolicy, OrchestratorExecutionBudget};
use crate::execution_outcome::{ExecutionOutcomeStatus, extract_execution_outcome};
use crate::llm_types::{
    LlmContentBlock, LlmMessage, LlmMessageContent, LlmMessageParams, LlmResponse, LlmUsage,
    is_summary_hijack_text, sanitize_summary_hijack_messages,
};
use crate::orchestrator_termination::OrchestratorTerminationReason;
use crate::round_policy::build_summary_hijack_correction;
use crate::structured_dispatch::{StructuredDispatchResult, extract_structured_dispatch};
use crate::types::{BridgeClientError, ModelBridgeClient};

#[derive(Clone, Debug)]
pub struct OrchestratorAdapterConfig {
    pub adapter: AdapterConfig,
    pub compaction: ConversationCompactionConfig,
    pub budget: OrchestratorExecutionBudget,
    pub policy: OrchestratorDecisionPolicy,
    pub enable_progress_tracking: bool,
    pub enable_no_progress_detection: bool,
    pub stalled_round_threshold: u32,
    pub summary_hijack_max_rounds: u32,
}

impl Default for OrchestratorAdapterConfig {
    fn default() -> Self {
        Self {
            adapter: AdapterConfig {
                max_rounds: 100,
                ..AdapterConfig::default()
            },
            compaction: ConversationCompactionConfig::default(),
            budget: OrchestratorExecutionBudget {
                max_duration_ms: 300_000,
                max_token_usage: 1_000_000,
                max_error_rate: 0.5,
                max_rounds: 100,
            },
            policy: OrchestratorDecisionPolicy {
                stalled_window_size: 5,
                external_wait_sla_ms: 30_000,
                upstream_model_error_streak: 3,
                error_rate_min_samples: 5,
                budget_no_progress_streak_threshold: 5,
                budget_breach_streak_threshold: 3,
                external_wait_breach_streak_threshold: 3,
                budget_hard_limit_factor: 1.5,
                external_wait_hard_limit_factor: 2.0,
            },
            enable_progress_tracking: true,
            enable_no_progress_detection: true,
            stalled_round_threshold: 5,
            summary_hijack_max_rounds: 3,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OrchestratorProgress {
    pub total_rounds: u32,
    pub tool_call_count: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub stalled_rounds: u32,
    pub error_count: u32,
    pub error_streak: u32,
    pub consecutive_empty_rounds: u32,
    pub summary_hijack_rounds: u32,
}

#[derive(Clone, Debug)]
pub struct DecisionTraceEntry {
    pub round: u32,
    pub phase: DecisionPhase,
    pub action: DecisionAction,
    pub reason: Option<OrchestratorTerminationReason>,
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionPhase {
    NoTool,
    Tool,
    Handoff,
    Finalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionAction {
    Continue,
    ContinueWithPrompt,
    Terminate,
    Handoff,
    Fallback,
}

#[derive(Clone, Debug)]
pub struct OrchestratorAdapterResult {
    pub final_response: LlmResponse,
    pub progress: OrchestratorProgress,
    pub termination_reason: OrchestratorTerminationReason,
    pub rounds: Vec<RoundResult>,
    pub compaction_count: u32,
    pub decision_trace: Vec<DecisionTraceEntry>,
    pub execution_status: Option<ExecutionOutcomeStatus>,
    pub next_steps: Vec<String>,
    pub structured_dispatches: Vec<StructuredDispatchResult>,
}

pub struct OrchestratorAdapter {
    config: OrchestratorAdapterConfig,
    base: BaseAdapter,
    compactor: ConversationCompactor,
}

impl OrchestratorAdapter {
    pub fn new(
        model_client: Arc<dyn ModelBridgeClient>,
        config: OrchestratorAdapterConfig,
    ) -> Self {
        let base = BaseAdapter::new(model_client.clone(), config.adapter.clone());
        let compactor =
            ConversationCompactor::new(config.compaction.clone()).with_model_client(model_client);
        Self {
            config,
            base,
            compactor,
        }
    }

    pub fn run(
        &self,
        initial_params: &LlmMessageParams,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<OrchestratorAdapterResult, BridgeClientError> {
        let mut params = initial_params.clone();
        let mut progress = OrchestratorProgress::default();
        let mut all_rounds = Vec::new();
        let mut compaction_count = 0u32;
        let mut last_tool_call_count = 0u32;
        let mut decision_trace = Vec::new();
        let mut execution_status: Option<ExecutionOutcomeStatus> = None;
        let mut next_steps: Vec<String> = Vec::new();
        let mut structured_dispatches: Vec<StructuredDispatchResult> = Vec::new();
        let mut force_no_tools = false;

        loop {
            if progress.total_rounds >= self.config.adapter.max_rounds {
                decision_trace.push(DecisionTraceEntry {
                    round: progress.total_rounds,
                    phase: DecisionPhase::Finalize,
                    action: DecisionAction::Terminate,
                    reason: Some(OrchestratorTerminationReason::Stalled),
                    note: Some("达到最大轮次限制".to_string()),
                });
                return Ok(self.build_result(
                    &all_rounds,
                    progress,
                    OrchestratorTerminationReason::Stalled,
                    compaction_count,
                    decision_trace,
                    execution_status,
                    next_steps,
                    structured_dispatches,
                ));
            }

            if let Some(reason) = self.check_budget(&progress) {
                decision_trace.push(DecisionTraceEntry {
                    round: progress.total_rounds,
                    phase: DecisionPhase::Finalize,
                    action: DecisionAction::Terminate,
                    reason: Some(reason),
                    note: None,
                });
                return Ok(self.build_result(
                    &all_rounds,
                    progress,
                    reason,
                    compaction_count,
                    decision_trace,
                    execution_status,
                    next_steps,
                    structured_dispatches,
                ));
            }

            if self.compactor.should_compact(&params.messages) {
                let (compacted, _result) = self.compactor.compact(&params.messages);
                params.messages = compacted;
                compaction_count += 1;
            }

            if self.config.adapter.enable_summary_hijack_guard {
                params.messages = sanitize_summary_hijack_messages(&params.messages);
            }

            if force_no_tools {
                params.tools = None;
                force_no_tools = false;
            }

            progress.total_rounds += 1;
            let response = match self.base.invoke_llm(&params) {
                Ok(r) => {
                    progress.error_streak = 0;
                    r
                }
                Err(e) => {
                    progress.error_count += 1;
                    progress.error_streak += 1;
                    if progress.error_streak >= self.config.policy.upstream_model_error_streak {
                        decision_trace.push(DecisionTraceEntry {
                            round: progress.total_rounds,
                            phase: DecisionPhase::Finalize,
                            action: DecisionAction::Terminate,
                            reason: Some(OrchestratorTerminationReason::UpstreamModelError),
                            note: Some(format!("连续 {} 次 LLM 错误", progress.error_streak)),
                        });
                        return Ok(self.build_result(
                            &all_rounds,
                            progress,
                            OrchestratorTerminationReason::UpstreamModelError,
                            compaction_count,
                            decision_trace,
                            execution_status,
                            next_steps,
                            structured_dispatches,
                        ));
                    }
                    return Err(e);
                }
            };

            progress.total_input_tokens += response.usage.input_tokens;
            progress.total_output_tokens += response.usage.output_tokens;

            let has_tool_calls = !response.tool_calls.is_empty();
            let tool_count = response.tool_calls.len() as u32;
            progress.tool_call_count += tool_count;

            let round_result = RoundResult {
                round: progress.total_rounds,
                response: response.clone(),
                tool_calls: response.tool_calls.clone(),
                had_tool_calls: has_tool_calls,
            };
            all_rounds.push(round_result);

            // --- ExecutionOutcome 提取 ---
            if !response.content.is_empty() {
                let outcome_result = extract_execution_outcome(&response.content);
                if let Some(outcome) = &outcome_result.outcome {
                    if let Some(ref status) = outcome.status {
                        execution_status = Some(status.clone());
                    }
                    if !outcome.next_steps.is_empty() {
                        next_steps = outcome.next_steps.clone();
                    }
                }
            }

            // --- Structured Dispatch 检测 ---
            if let Some(dispatch) = extract_structured_dispatch(&response) {
                structured_dispatches.push(dispatch);
            }

            if !has_tool_calls {
                // --- Summary Hijack 检测 ---
                if self.config.adapter.enable_summary_hijack_guard
                    && is_summary_hijack_text(&response.content)
                {
                    progress.summary_hijack_rounds += 1;
                    if progress.summary_hijack_rounds <= self.config.summary_hijack_max_rounds {
                        let correction =
                            build_summary_hijack_correction(progress.summary_hijack_rounds);
                        force_no_tools = correction.force_no_tools_next_round;
                        progress.summary_hijack_rounds = correction.normalized_rounds;

                        decision_trace.push(DecisionTraceEntry {
                            round: progress.total_rounds,
                            phase: DecisionPhase::NoTool,
                            action: DecisionAction::ContinueWithPrompt,
                            reason: None,
                            note: Some(format!(
                                "summary hijack 检测 (第 {} 次)",
                                progress.summary_hijack_rounds
                            )),
                        });

                        inject_system_prompt(&mut params.messages, &correction.prompt);
                        continue;
                    }
                } else {
                    progress.summary_hijack_rounds = 0;
                }

                if response.content.trim().is_empty() {
                    progress.consecutive_empty_rounds += 1;
                } else {
                    progress.consecutive_empty_rounds = 0;
                }

                let termination_reason = match &execution_status {
                    Some(ExecutionOutcomeStatus::Completed) => {
                        OrchestratorTerminationReason::Completed
                    }
                    Some(ExecutionOutcomeStatus::Failed) => OrchestratorTerminationReason::Failed,
                    _ => OrchestratorTerminationReason::Completed,
                };

                decision_trace.push(DecisionTraceEntry {
                    round: progress.total_rounds,
                    phase: DecisionPhase::NoTool,
                    action: DecisionAction::Terminate,
                    reason: Some(termination_reason),
                    note: None,
                });

                return Ok(self.build_result(
                    &all_rounds,
                    progress,
                    termination_reason,
                    compaction_count,
                    decision_trace,
                    execution_status,
                    next_steps,
                    structured_dispatches,
                ));
            }

            // --- 工具调用轮：进展检测 ---
            if self.config.enable_no_progress_detection {
                if tool_count == last_tool_call_count && tool_count > 0 {
                    progress.stalled_rounds += 1;
                } else {
                    progress.stalled_rounds = 0;
                }
                last_tool_call_count = tool_count;

                if progress.stalled_rounds >= self.config.stalled_round_threshold {
                    decision_trace.push(DecisionTraceEntry {
                        round: progress.total_rounds,
                        phase: DecisionPhase::Tool,
                        action: DecisionAction::Terminate,
                        reason: Some(OrchestratorTerminationReason::Stalled),
                        note: Some(format!("连续 {} 轮无进展", progress.stalled_rounds)),
                    });
                    return Ok(self.build_result(
                        &all_rounds,
                        progress,
                        OrchestratorTerminationReason::Stalled,
                        compaction_count,
                        decision_trace,
                        execution_status,
                        next_steps,
                        structured_dispatches,
                    ));
                }
            }

            // --- 执行工具并追加结果 ---
            let tool_error_count =
                append_tool_round(&mut params.messages, &response, tool_executor);
            progress.error_count += tool_error_count;

            decision_trace.push(DecisionTraceEntry {
                round: progress.total_rounds,
                phase: DecisionPhase::Tool,
                action: DecisionAction::Continue,
                reason: None,
                note: if tool_error_count > 0 {
                    Some(format!("{} 个工具调用返回错误", tool_error_count))
                } else {
                    None
                },
            });
        }
    }

    fn check_budget(
        &self,
        progress: &OrchestratorProgress,
    ) -> Option<OrchestratorTerminationReason> {
        let budget = &self.config.budget;
        let total = progress.total_input_tokens + progress.total_output_tokens;
        if total >= budget.max_token_usage {
            return Some(OrchestratorTerminationReason::BudgetExceeded);
        }
        if progress.total_rounds >= self.config.policy.error_rate_min_samples {
            let error_rate = progress.error_count as f64 / progress.total_rounds as f64;
            if error_rate > budget.max_error_rate {
                return Some(OrchestratorTerminationReason::Failed);
            }
        }
        None
    }

    fn build_result(
        &self,
        rounds: &[RoundResult],
        progress: OrchestratorProgress,
        reason: OrchestratorTerminationReason,
        compaction_count: u32,
        decision_trace: Vec<DecisionTraceEntry>,
        execution_status: Option<ExecutionOutcomeStatus>,
        next_steps: Vec<String>,
        structured_dispatches: Vec<StructuredDispatchResult>,
    ) -> OrchestratorAdapterResult {
        OrchestratorAdapterResult {
            final_response: last_response_or_default(rounds),
            progress,
            termination_reason: reason,
            rounds: rounds.to_vec(),
            compaction_count,
            decision_trace,
            execution_status,
            next_steps,
            structured_dispatches,
        }
    }
}

fn inject_system_prompt(messages: &mut Vec<LlmMessage>, prompt: &str) {
    messages.push(LlmMessage {
        role: "user".to_string(),
        content: LlmMessageContent::Text(prompt.to_string()),
    });
}

fn append_tool_round(
    messages: &mut Vec<LlmMessage>,
    response: &LlmResponse,
    tool_executor: &dyn ToolExecutor,
) -> u32 {
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
    messages.push(LlmMessage {
        role: "assistant".to_string(),
        content: LlmMessageContent::Blocks(assistant_blocks),
    });

    let mut error_count = 0u32;
    let result_blocks: Vec<LlmContentBlock> =
        execute_tool_calls(&response.tool_calls, tool_executor)
            .into_iter()
            .map(|result| {
                if result.is_error {
                    error_count += 1;
                }
                LlmContentBlock::ToolResult {
                    tool_use_id: result.tool_call_id,
                    content: result.content,
                    is_error: result.is_error,
                }
            })
            .collect();

    messages.push(LlmMessage {
        role: "user".to_string(),
        content: LlmMessageContent::Blocks(result_blocks),
    });

    error_count
}

fn last_response_or_default(rounds: &[RoundResult]) -> LlmResponse {
    rounds
        .last()
        .map(|r| r.response.clone())
        .unwrap_or(LlmResponse {
            content: String::new(),
            thinking: None,
            tool_calls: Vec::new(),
            usage: LlmUsage::default(),
            stop_reason: "none".to_string(),
        })
}

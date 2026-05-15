//! Task System v2 - Conversation Runtime
//!
//! 提供 Mailbox 作为 user 信号进入任务系统的**单一通道**，
//! Conversation 绑定 SessionId、Mailbox 与当前 Turn 槽位，
//! Turn 状态机刻画"一轮 user → assistant"的推进契约。
//!
//! 已交付 slice：
//! - S1：Mailbox + Conversation 骨架（user 信号入栈姿势）
//! - S2：Turn 状态机 + 单 Conversation 不并发不变式（v2 拥有 Turn lifecycle，
//!   v1 `run_task_llm_loop` 暂作"一轮 IO 引擎"被 v2 调度）

#![recursion_limit = "256"]

mod builtin_tool_schema;
mod conversation;
mod driver;
pub mod execution_chain_recovery;
mod mailbox;
pub mod mission_decomposition;
pub mod model_config;
pub mod prompt_utils;
mod registry;
pub mod dispatch_submission;
pub mod session_thread;
pub mod task_execution_registry;
pub mod session_turn_execution;
pub mod session_turn_finalize;
pub mod session_writeback;
pub mod settings_store;
pub mod task_graph_builder;
pub mod task_graph_replan;
pub mod task_runner_v2;
mod skill_apply_tool;
mod stream;
pub mod task_helpers;
pub mod task_llm_loop;
pub mod tool_batch;
pub mod tool_result_utils;
mod turn;
pub mod usage_recording;

pub use builtin_tool_schema::{internal_builtin_tool_rejection_payload, public_builtin_tool_definitions};
pub use conversation::{AdvanceTurnError, BeginTurnError, Conversation, TurnAdvanceError};
pub use driver::{RoundOutcome, TurnDriver};
pub use mailbox::{MailboxItem, UserSignal};
pub use registry::ConversationRegistry;
pub use skill_apply_tool::{
    SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime, skill_apply_tool_definition,
};
pub use stream::{StreamEvent, StreamFanOut, SubscriptionId, ToolPhase};
pub use task_helpers::{
    BASE_TOOL_CALL_ROUNDS, MAX_TOOL_CALL_ROUNDS, TaskTurnVisibility,
    apply_task_final_visibility, apply_task_turn_visibility, apply_task_worker_detail_visibility,
    canonical_tool_call_name, collect_dependency_output_validation_facts,
    compact_validation_failure, deterministic_execution_tool_validation_content,
    deterministic_planning_content, deterministic_planning_validation_content,
    deterministic_task_final_content, extract_task_goal, forced_task_tool_choice_for_round,
    is_execution_tool_validation, is_planning_no_tool_action, is_planning_text_validation,
    is_tool_reference_boundary, public_builtin_tool_reference_aliases,
    record_completed_required_tools, required_tool_chain_is_complete,
    required_tool_chain_recovery_prompt, task_required_tool_chain, task_tool_failure_reason,
    task_turn_visibility, tool_call_round_limit, tool_reference_position,
    validation_result_rejects_delivery,
};
pub use tool_batch::execute_task_tool_call_batch;
pub use turn::{Turn, TurnState, TurnTransitionError};

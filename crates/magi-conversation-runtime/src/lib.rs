//! 任务系统 - Conversation Runtime
//!
//! SessionTurnCoordinator 接纳主线输入，TaskStore 管理任务与租约；
//! 原生主任务和子代理共用模型/工具循环及任务信号通道。

#![recursion_limit = "256"]

mod agent_activation;
mod agent_context_fork;
mod agent_receipt;
mod agent_spawn_preflight;
mod builtin_tool_schema;
mod context_authority;
pub mod context_reference;
pub mod conversation_loop;
mod discovery_progress;
pub mod dispatch_submission;
pub mod execution_admission;
pub mod execution_chain_recovery;
pub mod external_approval;
pub mod external_session_turn;
pub mod external_tool;
mod goal_tool_failure;
pub mod human_wait;
mod mailbox;
pub mod model_config;
pub mod model_context_window;
pub mod model_error;
mod model_step;
pub mod prompt_utils;
mod registry;
pub mod session_images;
pub mod session_thread;
pub mod session_turn_coordinator;
pub mod session_turn_execution;
pub mod session_turn_finalize;
pub mod session_writeback;
mod skill_apply_tool;
mod skill_custom_tool_surface;
pub mod task_completion_notifier;
pub mod task_execution_dispatcher;
mod task_execution_guard;
pub mod task_execution_registry;
pub mod task_helpers;
pub mod task_runner;
pub mod task_runner_bridge;
pub mod tool_approval;
pub mod tool_batch;
mod tool_call_validation;
mod tool_declared_paths;
mod tool_execution_ledger;
pub mod tool_result_utils;
mod tool_surface_state;
pub mod turn_contract;
pub mod turn_stream_buffer;
pub mod usage_recording;
pub mod user_question;
pub mod web_history_import;
pub mod workspace_execution;
mod workspace_rules;

pub use builtin_tool_schema::{
    internal_builtin_tool_rejection_payload, public_builtin_tool_definitions,
};
pub use human_wait::{HumanWait, HumanWaitGate, HumanWaitHook};
pub use mailbox::{MailboxAuthor, MailboxKind, RuntimeSignal, UserSignal};
pub use registry::{ConversationRegistry, TaskSignalBoundary, TaskSignalCommitError};
pub use session_turn_coordinator::{
    CoordinatorAdmission, CoordinatorCommandResult, CoordinatorError, CoordinatorTurnStatus,
    ExecutionProfile, SessionTurnCoordinator, SessionTurnInputBoundary,
    SessionTurnInputCommitError, SessionTurnInputError, TurnAdmission, TurnAttempt,
    request_fingerprint,
};
pub use session_turn_execution::{SessionTurnCommand, SessionTurnExecutionRequest};
pub use session_writeback::CanonicalTurnEventSink;
pub use skill_apply_tool::{
    SKILL_APPLY_TOOL_NAME, execute_skill_apply_from_runtime, skill_apply_tool_definition,
};
pub use skill_custom_tool_surface::{
    active_skill_tool_execution_policy, build_skill_custom_tool_definitions,
    execute_skill_custom_tool, extract_skill_custom_tool_payload, parse_skill_custom_tool_name,
    tool_execution_policy_scope,
};
pub use task_completion_notifier::{TaskCompletionNotification, TaskCompletionNotifier};
pub use task_execution_dispatcher::safety_gate_from_settings;
pub use task_helpers::{
    GoalModeLifecycleState, TaskTurnVisibility, apply_task_final_visibility,
    apply_task_turn_visibility, apply_task_worker_detail_visibility, canonical_tool_call_name,
    compact_validation_failure, forced_task_tool_choice_for_round, goal_mode_required_tool_chain,
    goal_mode_requires_terminalization, goal_mode_tool_batch_violation,
    record_completed_required_tools, required_tool_chain_is_complete,
    required_tool_chain_recovery_prompt, required_tool_definitions_for_round,
    strict_goal_mode_tool_definitions_for_round, task_required_tool_chain, task_turn_visibility,
    validation_result_rejects_delivery,
};
pub use tool_approval::{
    ApprovalRequestAgent, PendingToolApproval, TOOL_APPROVAL_TTL_MILLIS, ToolApprovalDecision,
    ToolApprovalRegistry, ToolApprovalRequestOutcome,
};
pub(crate) use tool_batch::execute_task_tool_call_batch;
pub use turn_contract::{TaskRunRecord, TurnCommand, TurnEventEnvelope, TurnRecord};
pub use user_question::{
    PendingUserQuestion, UserQuestion, UserQuestionAnswer, UserQuestionOption,
    UserQuestionRegistry, UserQuestionResponse,
};

#[cfg(test)]
pub(crate) fn test_plan_store(name: &str) -> magi_plan::PlanStore {
    let store = std::sync::Arc::new(magi_session_store::SessionStore::new());
    let session_id = magi_core::SessionId::new(name);
    store
        .create_session(session_id.clone(), name)
        .expect("test plan session should create");
    magi_plan::PlanStore::new(store, session_id)
}

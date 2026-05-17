//! Task System v2 — A 档（单代理 / codex 式一次对话一个 task）同步驱动入口。
//!
//! 所有 A 档"用户消息已 dispatch → 同步驱动到 done"路径都从本模块进入；
//! 中等任务的同步推进判定保留在调用方完成。本函数只负责同步推进。

use crate::{errors::ApiError, state::ApiState};
use magi_conversation_runtime::task_runner_bridge::RunCycleOutcome;
use magi_core::{TaskId, TaskStatus};

/// A 档同步驱动的产出。
#[derive(Debug)]
pub struct APathDriveResult {
    /// 是否真正进入过 run cycle（false 仅在循环 0 次直接退出时，本字段与既有行为一致）。
    pub runner_started: bool,
}

/// A 档（单代理）同步驱动入口：dispatch 已经把 task 派进 task_store，本函数把它推到终态。
///
/// 调用约定：
/// - `root_task_id`：mission 根 task id，用于 `RunnerManager::run_single_cycle` 推进；
/// - `action_task_id`：本次用户动作对应的 task id，用于"循环退出后状态判定"；
/// - `failure_title`：错误对外名称（中文短语，进 `ApiError::internal_assembly`）。
///
/// 行为：最多跑 32 轮 run cycle，遇 `AllComplete` 或 action task 已进 terminal 即收口。
pub fn drive_a_path(
    state: &ApiState,
    root_task_id: &TaskId,
    action_task_id: &TaskId,
    failure_title: &'static str,
) -> Result<APathDriveResult, ApiError> {
    // ExecutionChain 使用同步 for 循环，要求 dispatch 同步完成，否则结果来不及被收集。
    if let Some(dispatcher) = state.session_turn_dispatcher() {
        dispatcher.set_force_sync_dispatch(true);
    }

    let result = (|| {
        let manager = state
            .runner_manager()
            .ok_or_else(|| ApiError::internal_assembly(failure_title, "runner_manager 未配置"))?;
        let task_store = state
            .task_store()
            .ok_or_else(|| ApiError::internal_assembly(failure_title, "task_store 未配置"))?;

        let mut executed = false;
        for _ in 0..32 {
            executed = true;
            let outcome = manager
                .run_single_cycle(root_task_id.as_str())
                .map_err(|error| ApiError::internal_assembly(failure_title, error))?;
            match outcome {
                RunCycleOutcome::Continue => continue,
                RunCycleOutcome::AllComplete => break,
                RunCycleOutcome::Blocked { task_ids, reason } => {
                    return Err(ApiError::internal_assembly(
                        failure_title,
                        format!("task runner blocked: {:?}: {reason}", task_ids),
                    ));
                }
                RunCycleOutcome::Stalled(task_ids) => {
                    if task_store
                        .get_task(action_task_id)
                        .is_some_and(|task| task.status == TaskStatus::Failed)
                    {
                        break;
                    }
                    return Err(ApiError::internal_assembly(
                        failure_title,
                        format!("task runner stalled: {:?}", task_ids),
                    ));
                }
                RunCycleOutcome::Error(error) => {
                    return Err(ApiError::internal_assembly(failure_title, error));
                }
            }
        }

        let action_status = task_store
            .get_task(action_task_id)
            .ok_or_else(|| ApiError::internal_assembly(failure_title, "action task 不存在"))?
            .status;
        if action_status != TaskStatus::Completed && action_status != TaskStatus::Failed {
            return Err(ApiError::internal_assembly(
                failure_title,
                format!("同步任务未在窗口内完成: {:?}", action_status),
            ));
        }

        Ok(APathDriveResult {
            runner_started: executed,
        })
    })();

    if let Some(dispatcher) = state.session_turn_dispatcher() {
        dispatcher.set_force_sync_dispatch(false);
    }

    result
}

use magi_session_store::SessionRuntimeSidecar;

/// 会话目录只把明确属于执行生命周期的 Turn 状态视为“响应中”。
///
/// 这里使用活跃状态白名单，而不是终止状态黑名单：未知状态、暂停状态和
/// 停止状态都不能把侧栏状态灯错误地抬回运行中。
pub(crate) fn session_running_task_count(sidecar: Option<&SessionRuntimeSidecar>) -> usize {
    let Some(turn) = sidecar.and_then(current_turn) else {
        return 0;
    };
    if !current_turn_status_is_active(&turn.status) {
        return 0;
    }
    turn.items
        .iter()
        .filter(|item| {
            current_turn_item_status_is_active(&item.status)
                || item
                    .tool_status
                    .as_deref()
                    .is_some_and(current_turn_item_status_is_active)
        })
        .count()
        .max(1)
}

fn current_turn(
    sidecar: &SessionRuntimeSidecar,
) -> Option<&magi_session_store::ActiveExecutionTurn> {
    sidecar.current_turn.as_ref().or_else(|| {
        sidecar
            .active_execution_chain
            .as_ref()
            .and_then(|chain| chain.current_turn.as_ref())
    })
}

pub(crate) fn current_turn_status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "accepted"
            | "preparing"
            | "running"
            | "started"
            | "streaming"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

fn current_turn_item_status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "pending"
            | "queued"
            | "running"
            | "started"
            | "streaming"
            | "blocked"
            | "awaiting_approval"
            | "review_required"
            | "repairing"
            | "verifying"
    )
}

#[cfg(test)]
mod tests {
    use super::current_turn_status_is_active;

    #[test]
    fn only_explicit_execution_states_are_active() {
        for status in [
            "pending",
            "queued",
            "accepted",
            "preparing",
            "running",
            "started",
            "streaming",
            "awaiting_approval",
            "review_required",
            "repairing",
            "verifying",
        ] {
            assert!(
                current_turn_status_is_active(status),
                "{status} should be active"
            );
        }

        for status in [
            "completed",
            "complete",
            "succeeded",
            "success",
            "failed",
            "error",
            "blocked",
            "interrupted",
            "cancelled",
            "canceled",
            "killed",
            "superseded",
            "stopped",
            "terminated",
            "paused",
            "unknown",
        ] {
            assert!(
                !current_turn_status_is_active(status),
                "{status} should not be active"
            );
        }
    }
}

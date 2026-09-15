//! Session Turn 的单一生命周期协调器。
//!
//! Coordinator 只保存执行生命周期所需的轻量元数据（当前 turn、attempt 和 profile），
//! 不复制 canonical 内容。所有正文和状态事实仍由 SessionStore 持久化；Coordinator
//! 负责把命令串行化，并拒绝迟到的执行结果。

use magi_core::SessionId;
use magi_session_store::{CanonicalTurn, CanonicalTurnStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::turn_contract::TurnCommand;

/// Turn 接纳时确定的执行级别。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProfile {
    /// 只进行会话级模型交互，不创建任务、租约或 Runner。
    #[default]
    Conversation,
    /// 需要工程工具、Goal、计划或子代理的任务运行。
    Task,
}

impl ExecutionProfile {
    pub const fn is_task(self) -> bool {
        matches!(self, Self::Task)
    }
}

impl fmt::Display for ExecutionProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Conversation => "conversation",
            Self::Task => "task",
        })
    }
}

/// Coordinator 关心的生命周期状态。内容和详细任务状态不在这里复制。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorTurnStatus {
    #[default]
    Accepted,
    Preparing,
    Running,
    WaitingInput,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl CoordinatorTurnStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// 与 Turn 关联的执行代际。任何异步回调都必须携带它。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAttempt {
    pub turn_id: String,
    pub attempt_id: String,
    pub profile: ExecutionProfile,
}

/// 接纳命令需要的稳定身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAdmission {
    pub turn_id: String,
    pub request_id: String,
    pub request_fingerprint: String,
    pub profile: ExecutionProfile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatorAdmission {
    Accepted(TurnAttempt),
    /// 相同 requestId + fingerprint 的重试，返回原 Turn。
    Replay(TurnAttempt),
}

/// Coordinator 命令的结果。命令执行只改变内存中的生命周期所有权，
/// durable Turn 事实仍必须由调用方在命令成功后交给 TurnEventSink。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatorCommandResult {
    Admission(CoordinatorAdmission),
    Status(CoordinatorTurnStatus),
    Attempt(TurnAttempt),
    Finished(bool),
    Aborted(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatorError {
    ActiveTurn {
        active_turn_id: String,
    },
    RequestConflict {
        request_id: String,
    },
    TurnMismatch {
        expected: String,
        actual: String,
    },
    AttemptMismatch {
        expected: String,
        actual: String,
    },
    TerminalConflict {
        expected: CoordinatorTurnStatus,
        actual: CoordinatorTurnStatus,
    },
    InvalidTransition {
        from: CoordinatorTurnStatus,
        to: CoordinatorTurnStatus,
    },
    AlreadyTerminal,
    NoActiveTurn,
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveTurn { active_turn_id } => {
                write!(f, "session 已有活动 Turn {active_turn_id}")
            }
            Self::RequestConflict { request_id } => {
                write!(f, "requestId {request_id} 已绑定另一份 Turn")
            }
            Self::TurnMismatch { expected, actual } => {
                write!(f, "Turn 不匹配：期望 {expected}，实际 {actual}")
            }
            Self::AttemptMismatch { expected, actual } => {
                write!(f, "执行代际不匹配：期望 {expected}，实际 {actual}")
            }
            Self::TerminalConflict { expected, actual } => {
                write!(f, "Turn 终态冲突：已是 {expected:?}，不能改为 {actual:?}")
            }
            Self::InvalidTransition { from, to } => {
                write!(f, "Turn 状态迁移非法：{from:?} -> {to:?}")
            }
            Self::AlreadyTerminal => f.write_str("Turn 已经进入终态"),
            Self::NoActiveTurn => f.write_str("session 没有活动 Turn"),
        }
    }
}

impl std::error::Error for CoordinatorError {}

#[derive(Clone, Debug)]
struct ActiveTurn {
    admission: TurnAdmission,
    attempt_id: String,
    status: CoordinatorTurnStatus,
    /// canonical turnSeq；进程内刚接纳但尚未从 store 读取时为 0。
    turn_seq: u64,
}

#[derive(Clone, Debug)]
struct FinishedTurn {
    admission: TurnAdmission,
    attempt: TurnAttempt,
    status: CoordinatorTurnStatus,
    turn_seq: u64,
}

#[derive(Default)]
struct SessionState {
    active: Option<ActiveTurn>,
    recent: HashMap<String, FinishedTurn>,
}

fn status_transition_allowed(from: CoordinatorTurnStatus, to: CoordinatorTurnStatus) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (
            CoordinatorTurnStatus::Accepted,
            CoordinatorTurnStatus::Preparing
        ) | (
            CoordinatorTurnStatus::Accepted,
            CoordinatorTurnStatus::Blocked
        ) | (
            CoordinatorTurnStatus::Preparing,
            CoordinatorTurnStatus::Running
        ) | (
            CoordinatorTurnStatus::Preparing,
            CoordinatorTurnStatus::WaitingInput
        ) | (
            CoordinatorTurnStatus::Preparing,
            CoordinatorTurnStatus::Blocked
        ) | (
            CoordinatorTurnStatus::Running,
            CoordinatorTurnStatus::WaitingInput
        ) | (
            CoordinatorTurnStatus::Running,
            CoordinatorTurnStatus::Blocked
        ) | (
            CoordinatorTurnStatus::WaitingInput,
            CoordinatorTurnStatus::Running
        ) | (
            CoordinatorTurnStatus::WaitingInput,
            CoordinatorTurnStatus::Blocked
        ) | (
            CoordinatorTurnStatus::Blocked,
            CoordinatorTurnStatus::Preparing
        ) | (
            CoordinatorTurnStatus::Blocked,
            CoordinatorTurnStatus::WaitingInput
        ) | (
            CoordinatorTurnStatus::Blocked,
            CoordinatorTurnStatus::Running
        )
    )
}

#[derive(Default)]
struct CoordinatorState {
    sessions: HashMap<SessionId, SessionState>,
}

/// 进程内所有 session 的 Coordinator 注册表。
///
/// 每个 session 的命令在同一把状态锁内顺序执行；正文、事件和任务快照不在锁内写入，
/// 因而不会把磁盘 IO 带进生命周期临界区。`recent` 只保存已完成 requestId 的轻量
/// 身份，避免网络重试重新创建 Turn；正文仍从 SessionStore 读取。
#[derive(Clone, Default)]
pub struct SessionTurnCoordinator {
    state: Arc<Mutex<CoordinatorState>>,
    attempt_sequence: Arc<AtomicU64>,
}

impl SessionTurnCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行统一生命周期命令。
    ///
    /// 这里不写 SessionStore，也不发布事件；这样命令验证和 durable mutation
    /// 之间的边界始终显式，迟到的 provider/task 结果不能绕过 attempt 校验。
    pub fn execute_command(
        &self,
        session_id: &SessionId,
        command: TurnCommand,
    ) -> Result<CoordinatorCommandResult, CoordinatorError> {
        match command {
            TurnCommand::Start(admission) => self
                .accept(session_id, admission)
                .map(CoordinatorCommandResult::Admission),
            TurnCommand::SetStatus { turn_id, status } => {
                self.set_status(session_id, &turn_id, status)?;
                Ok(CoordinatorCommandResult::Status(status))
            }
            TurnCommand::Steer { attempt, .. } => {
                self.validate_attempt(session_id, &attempt)?;
                Ok(CoordinatorCommandResult::Attempt(attempt))
            }
            TurnCommand::Continue {
                previous,
                previous_status,
                next,
            } => {
                // Continue 必须先按 canonical 已提交的旧状态收口执行代际，
                // 才能把新的 admission 放入同一 session 槽位。
                match self.current_attempt(session_id, &previous.turn_id) {
                    Ok(current) => {
                        if current.attempt_id != previous.attempt_id {
                            return Err(CoordinatorError::AttemptMismatch {
                                expected: current.attempt_id,
                                actual: previous.attempt_id,
                            });
                        }
                        self.finish(session_id, &previous, previous_status)?;
                    }
                    Err(CoordinatorError::NoActiveTurn) => {
                        match self.terminal_status(session_id, &previous) {
                            Some(actual) if actual == previous_status => {}
                            Some(actual) => {
                                return Err(CoordinatorError::TerminalConflict {
                                    expected: actual,
                                    actual: previous_status,
                                });
                            }
                            None => return Err(CoordinatorError::NoActiveTurn),
                        }
                    }
                    Err(error) => return Err(error),
                }
                self.accept(session_id, next)
                    .map(CoordinatorCommandResult::Admission)
            }
            TurnCommand::Cancel { attempt } => self
                .finish(session_id, &attempt, CoordinatorTurnStatus::Cancelled)
                .map(CoordinatorCommandResult::Finished),
            TurnCommand::Recover {
                admission,
                attempt_id,
                status,
            } => {
                let turn_id = admission.turn_id.clone();
                let profile = admission.profile;
                self.restore_active(session_id, admission, attempt_id.clone(), status);
                Ok(CoordinatorCommandResult::Attempt(TurnAttempt {
                    turn_id,
                    attempt_id,
                    profile,
                }))
            }
            TurnCommand::Finish { attempt, status } => self
                .finish(session_id, &attempt, status)
                .map(CoordinatorCommandResult::Finished),
            TurnCommand::Abort { attempt } => self
                .abort(session_id, &attempt)
                .map(CoordinatorCommandResult::Aborted),
        }
    }

    fn validate_attempt(
        &self,
        session_id: &SessionId,
        attempt: &TurnAttempt,
    ) -> Result<(), CoordinatorError> {
        let current = self.current_attempt(session_id, &attempt.turn_id)?;
        if current.attempt_id != attempt.attempt_id {
            return Err(CoordinatorError::AttemptMismatch {
                expected: current.attempt_id,
                actual: attempt.attempt_id.clone(),
            });
        }
        if current.profile != attempt.profile {
            return Err(CoordinatorError::AttemptMismatch {
                expected: current.profile.to_string(),
                actual: attempt.profile.to_string(),
            });
        }
        Ok(())
    }

    pub fn accept(
        &self,
        session_id: &SessionId,
        admission: TurnAdmission,
    ) -> Result<CoordinatorAdmission, CoordinatorError> {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state.sessions.entry(session_id.clone()).or_default();
        if let Some(active) = session.active.as_ref() {
            if active.admission.request_id == admission.request_id {
                if active.admission.request_fingerprint == admission.request_fingerprint
                    && active.admission.profile == admission.profile
                {
                    return Ok(CoordinatorAdmission::Replay(TurnAttempt {
                        turn_id: active.admission.turn_id.clone(),
                        attempt_id: active.attempt_id.clone(),
                        profile: active.admission.profile,
                    }));
                }
                return Err(CoordinatorError::RequestConflict {
                    request_id: admission.request_id,
                });
            }
            return Err(CoordinatorError::ActiveTurn {
                active_turn_id: active.admission.turn_id.clone(),
            });
        }
        if let Some(existing) = session.recent.get(&admission.request_id) {
            if existing.admission.request_fingerprint == admission.request_fingerprint
                && existing.admission.profile == admission.profile
            {
                return Ok(CoordinatorAdmission::Replay(existing.attempt.clone()));
            }
            return Err(CoordinatorError::RequestConflict {
                request_id: admission.request_id,
            });
        }
        let attempt_id = format!(
            "attempt-{}-{}",
            admission.turn_id,
            self.attempt_sequence.fetch_add(1, Ordering::Relaxed) + 1
        );
        let attempt = TurnAttempt {
            turn_id: admission.turn_id.clone(),
            attempt_id: attempt_id.clone(),
            profile: admission.profile,
        };
        session.active = Some(ActiveTurn {
            admission,
            attempt_id,
            status: CoordinatorTurnStatus::Accepted,
            turn_seq: 0,
        });
        Ok(CoordinatorAdmission::Accepted(attempt))
    }

    pub fn set_status(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        status: CoordinatorTurnStatus,
    ) -> Result<(), CoordinatorError> {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let active = state
            .sessions
            .get_mut(session_id)
            .and_then(|session| session.active.as_mut())
            .ok_or(CoordinatorError::NoActiveTurn)?;
        if active.admission.turn_id != turn_id {
            return Err(CoordinatorError::TurnMismatch {
                expected: active.admission.turn_id.clone(),
                actual: turn_id.to_string(),
            });
        }
        if active.status.is_terminal() {
            if active.status == status {
                return Ok(());
            }
            return Err(CoordinatorError::TerminalConflict {
                expected: active.status,
                actual: status,
            });
        }
        if status.is_terminal() {
            return Err(CoordinatorError::AlreadyTerminal);
        }
        if !status_transition_allowed(active.status, status) {
            return Err(CoordinatorError::InvalidTransition {
                from: active.status,
                to: status,
            });
        }
        active.status = status;
        Ok(())
    }

    /// 只允许当前 attempt 提交结果，终态提交具有幂等性。
    /// 只允许当前 attempt 提交结果，终态提交具有幂等性。
    pub fn finish(
        &self,
        session_id: &SessionId,
        attempt: &TurnAttempt,
        status: CoordinatorTurnStatus,
    ) -> Result<bool, CoordinatorError> {
        if !status.is_terminal() {
            return Err(CoordinatorError::AlreadyTerminal);
        }
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or(CoordinatorError::NoActiveTurn)?;
        let Some(active) = session.active.take() else {
            // 终态提交可以在执行器、取消命令和恢复清理之间竞争。已记录为
            // finished 的同一 attempt 重复提交是幂等成功；不同终态必须显式冲突。
            if let Some(finished) = session
                .recent
                .values()
                .find(|finished| finished.attempt.attempt_id == attempt.attempt_id)
            {
                if finished.attempt.turn_id != attempt.turn_id {
                    return Err(CoordinatorError::TurnMismatch {
                        expected: finished.attempt.turn_id.clone(),
                        actual: attempt.turn_id.clone(),
                    });
                }
                if finished.attempt.profile != attempt.profile {
                    return Err(CoordinatorError::AttemptMismatch {
                        expected: finished.attempt.profile.to_string(),
                        actual: attempt.profile.to_string(),
                    });
                }
                if finished.status != status {
                    return Err(CoordinatorError::TerminalConflict {
                        expected: finished.status,
                        actual: status,
                    });
                }
                return Ok(false);
            }
            return Err(CoordinatorError::NoActiveTurn);
        };
        if active.admission.turn_id != attempt.turn_id {
            let expected = active.admission.turn_id.clone();
            session.active = Some(active);
            return Err(CoordinatorError::TurnMismatch {
                expected,
                actual: attempt.turn_id.clone(),
            });
        }
        if active.attempt_id != attempt.attempt_id {
            let expected = active.attempt_id.clone();
            session.active = Some(active);
            return Err(CoordinatorError::AttemptMismatch {
                expected,
                actual: attempt.attempt_id.clone(),
            });
        }
        if active.admission.profile != attempt.profile {
            let expected = active.admission.profile.to_string();
            session.active = Some(active);
            return Err(CoordinatorError::AttemptMismatch {
                expected,
                actual: attempt.profile.to_string(),
            });
        }
        if active.status.is_terminal() && active.status != status {
            let expected = active.status;
            session.active = Some(active);
            return Err(CoordinatorError::TerminalConflict {
                expected,
                actual: status,
            });
        }
        let finished_attempt = TurnAttempt {
            turn_id: attempt.turn_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            profile: active.admission.profile,
        };
        let finished = FinishedTurn {
            admission: active.admission.clone(),
            attempt: finished_attempt,
            status,
            turn_seq: active.turn_seq,
        };
        session
            .recent
            .insert(active.admission.request_id.clone(), finished);
        Ok(true)
    }

    /// 在 accepted durable 事务失败时撤销尚未写入 canonical 的 admission。
    /// 只允许相同 attempt 清理当前活动槽位，避免误删并发 Turn。
    pub fn abort(
        &self,
        session_id: &SessionId,
        attempt: &TurnAttempt,
    ) -> Result<bool, CoordinatorError> {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or(CoordinatorError::NoActiveTurn)?;
        let Some(active) = session.active.as_ref() else {
            return Ok(false);
        };
        if active.admission.turn_id != attempt.turn_id {
            return Err(CoordinatorError::TurnMismatch {
                expected: active.admission.turn_id.clone(),
                actual: attempt.turn_id.clone(),
            });
        }
        if active.attempt_id != attempt.attempt_id {
            return Err(CoordinatorError::AttemptMismatch {
                expected: active.attempt_id.clone(),
                actual: attempt.attempt_id.clone(),
            });
        }
        if active.status.is_terminal() {
            return Err(CoordinatorError::AlreadyTerminal);
        }
        session.active = None;
        Ok(true)
    }

    /// 返回当前或最近一次已完成 Turn 的终态，供取消/恢复路径做幂等判断。
    pub fn terminal_status(
        &self,
        session_id: &SessionId,
        attempt: &TurnAttempt,
    ) -> Option<CoordinatorTurnStatus> {
        let state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state.sessions.get(session_id)?;
        if let Some(active) = session.active.as_ref()
            && active.attempt_id == attempt.attempt_id
        {
            return active.status.is_terminal().then_some(active.status);
        }
        session
            .recent
            .values()
            .find(|finished| finished.attempt.attempt_id == attempt.attempt_id)
            .map(|finished| finished.status)
    }

    /// 从 canonical Turn 恢复生命周期身份。
    ///
    /// accepted 事实已经写入 canonical event 后，Coordinator 可以在 daemon 重启时
    /// 重新建立 requestId/fingerprint/profile/attempt 关联。正文不会被复制；终态 Turn
    /// 只进入 replay 索引，活动 Turn 才恢复为当前所有权。
    pub fn restore_canonical_turn(&self, session_id: &SessionId, turn: &CanonicalTurn) -> bool {
        fn metadata_string(turn: &CanonicalTurn, key: &str) -> Option<String> {
            turn.metadata
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    turn.items.iter().find_map(|item| {
                        item.metadata
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    })
                })
        }

        let request_id =
            metadata_string(turn, "requestId").or_else(|| metadata_string(turn, "request_id"));
        let Some(request_id) = request_id else {
            // 老 Turn 没有幂等身份时不能猜 requestId；它仍可由 SessionStore 展示，
            // 但不能被 Coordinator 当作可安全重放的请求。
            return false;
        };
        let request_fingerprint = metadata_string(turn, "requestFingerprint")
            .or_else(|| metadata_string(turn, "request_fingerprint"))
            .unwrap_or_else(|| format!("legacy-turn:{}", turn.turn_id));
        let profile = metadata_string(turn, "executionProfile")
            .or_else(|| metadata_string(turn, "execution_profile"))
            .map(|value| match value.as_str() {
                "task" => ExecutionProfile::Task,
                _ => ExecutionProfile::Conversation,
            })
            .unwrap_or_else(|| {
                let route = metadata_string(turn, "route");
                if route.as_deref().is_some_and(|route| route != "chat")
                    || turn.items.iter().any(|item| item.worker.is_some())
                {
                    ExecutionProfile::Task
                } else {
                    ExecutionProfile::Conversation
                }
            });
        let attempt_id = metadata_string(turn, "attemptId")
            .or_else(|| metadata_string(turn, "attempt_id"))
            .unwrap_or_else(|| format!("attempt-restored-{}", turn.turn_id));
        let admission = TurnAdmission {
            turn_id: turn.turn_id.clone(),
            request_id,
            request_fingerprint,
            profile,
        };
        let attempt = TurnAttempt {
            turn_id: turn.turn_id.clone(),
            attempt_id,
            profile,
        };
        let status = match turn.status {
            CanonicalTurnStatus::Pending => CoordinatorTurnStatus::Accepted,
            CanonicalTurnStatus::Running => CoordinatorTurnStatus::Running,
            CanonicalTurnStatus::Blocked => CoordinatorTurnStatus::Blocked,
            CanonicalTurnStatus::Completed => CoordinatorTurnStatus::Completed,
            CanonicalTurnStatus::Failed => CoordinatorTurnStatus::Failed,
            CanonicalTurnStatus::Interrupted | CanonicalTurnStatus::Cancelled => {
                CoordinatorTurnStatus::Cancelled
            }
            // Superseded 只表示旧 Turn 已被替换，也必须保留 request replay 身份，
            // 但不会重新占用 session 的活动槽位。
            CanonicalTurnStatus::Superseded => CoordinatorTurnStatus::Cancelled,
        };
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state.sessions.entry(session_id.clone()).or_default();
        if status.is_terminal() {
            // canonical terminal projection 可能在同一进程的执行回调之后才恢复；
            // 同一 Turn 的 active 槽位必须一并释放，否则重启/重放后新请求会被
            // 错误地拒绝为“已有活动 Turn”。更晚的不同 active Turn 保持不变。
            if session.active.as_ref().is_some_and(|active| {
                active.admission.turn_id == turn.turn_id && turn.turn_seq >= active.turn_seq
            }) {
                session.active = None;
            }
            if let Some(existing) = session.recent.get(&admission.request_id)
                && existing.turn_seq >= turn.turn_seq
            {
                return false;
            }
            session.recent.insert(
                admission.request_id.clone(),
                FinishedTurn {
                    admission,
                    attempt,
                    status,
                    turn_seq: turn.turn_seq,
                },
            );
            return true;
        }
        if let Some(active) = session.active.as_ref() {
            if active.admission.turn_id == turn.turn_id {
                return true;
            }
            // canonical turns 按 turnSeq 恢复时，较新的活动 Turn 优先；不要让旧
            // snapshot 覆盖已经恢复的当前所有权。
            if turn.turn_seq <= active.turn_seq {
                return false;
            }
        }
        // 如果已经先恢复了更晚的终态 Turn，则更早的非终态 Turn 只能是陈旧
        // projection。恢复它会重新占用 active 槽位并阻塞后续 request，因此直接跳过。
        if session
            .recent
            .values()
            .any(|finished| finished.turn_seq > turn.turn_seq)
        {
            return false;
        }
        session.active = Some(ActiveTurn {
            admission,
            attempt_id: attempt.attempt_id,
            status,
            turn_seq: turn.turn_seq,
        });
        true
    }

    /// 将已恢复的 attempt 作为当前活动 Turn 注册。保留旧入口供恢复代码使用。
    pub fn restore_active(
        &self,
        session_id: &SessionId,
        admission: TurnAdmission,
        attempt_id: String,
        status: CoordinatorTurnStatus,
    ) {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state.sessions.entry(session_id.clone()).or_default();
        session.active = Some(ActiveTurn {
            admission,
            attempt_id,
            status,
            turn_seq: 0,
        });
    }

    pub fn current_attempt(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<TurnAttempt, CoordinatorError> {
        let state = self.state.lock().expect("turn coordinator state poisoned");
        let active = state
            .sessions
            .get(session_id)
            .and_then(|session| session.active.as_ref())
            .ok_or(CoordinatorError::NoActiveTurn)?;
        if active.admission.turn_id != turn_id {
            return Err(CoordinatorError::TurnMismatch {
                expected: active.admission.turn_id.clone(),
                actual: turn_id.to_string(),
            });
        }
        Ok(TurnAttempt {
            turn_id: active.admission.turn_id.clone(),
            attempt_id: active.attempt_id.clone(),
            profile: active.admission.profile,
        })
    }

    pub fn current_status(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<CoordinatorTurnStatus, CoordinatorError> {
        let state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state
            .sessions
            .get(session_id)
            .ok_or(CoordinatorError::NoActiveTurn)?;
        if let Some(active) = session.active.as_ref() {
            if active.admission.turn_id != turn_id {
                return Err(CoordinatorError::TurnMismatch {
                    expected: active.admission.turn_id.clone(),
                    actual: turn_id.to_string(),
                });
            }
            return Ok(active.status);
        }
        session
            .recent
            .values()
            .find(|finished| finished.admission.turn_id == turn_id)
            .map(|finished| finished.status)
            .ok_or(CoordinatorError::NoActiveTurn)
    }

    pub fn active_profile(&self, session_id: &SessionId) -> Option<ExecutionProfile> {
        self.state
            .lock()
            .expect("turn coordinator state poisoned")
            .sessions
            .get(session_id)
            .and_then(|session| session.active.as_ref())
            .map(|active| active.admission.profile)
    }

    /// 从已持久化的 accepted Turn 恢复轻量所有权；不会恢复正文。
    pub fn restore(&self, session_id: &SessionId, admission: TurnAdmission, attempt_id: String) {
        self.restore_active(
            session_id,
            admission,
            attempt_id,
            CoordinatorTurnStatus::Accepted,
        );
    }

    pub fn clear_session(&self, session_id: &SessionId) {
        self.state
            .lock()
            .expect("turn coordinator state poisoned")
            .sessions
            .remove(session_id);
    }
}

/// 计算提交请求的稳定指纹，供 HTTP 和 App Server 共用。
pub fn request_fingerprint(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).expect("request fingerprint value must serialize");
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission(turn: &str, request: &str, fingerprint: &str) -> TurnAdmission {
        TurnAdmission {
            turn_id: turn.to_string(),
            request_id: request.to_string(),
            request_fingerprint: fingerprint.to_string(),
            profile: ExecutionProfile::Conversation,
        }
    }

    #[test]
    fn same_request_is_replay_and_different_fingerprint_is_conflict() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-coordinator");
        let first = coordinator
            .accept(&session, admission("turn-1", "request-1", "fp-1"))
            .unwrap();
        let attempt = match first {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("first request must be accepted"),
        };
        assert!(matches!(
            coordinator.accept(&session, admission("turn-1", "request-1", "fp-1")),
            Ok(CoordinatorAdmission::Replay(_))
        ));
        assert_eq!(
            coordinator
                .accept(&session, admission("turn-2", "request-1", "fp-2"))
                .unwrap_err(),
            CoordinatorError::RequestConflict {
                request_id: "request-1".to_string()
            }
        );
        assert_eq!(
            coordinator.finish(&session, &attempt, CoordinatorTurnStatus::Completed),
            Ok(true)
        );
        assert_eq!(
            coordinator.finish(&session, &attempt, CoordinatorTurnStatus::Completed),
            Ok(false)
        );
        assert!(matches!(
            coordinator.accept(&session, admission("turn-replayed", "request-1", "fp-1")),
            Ok(CoordinatorAdmission::Replay(replayed)) if replayed.turn_id == "turn-1"
        ));
    }

    #[test]
    fn execute_command_serializes_start_steer_cancel_and_replay() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-command-contract");
        let admission = TurnAdmission {
            turn_id: "turn-command-1".to_string(),
            request_id: "request-command-1".to_string(),
            request_fingerprint: "fp-command-1".to_string(),
            profile: ExecutionProfile::Conversation,
        };
        let attempt = match coordinator
            .execute_command(&session, TurnCommand::Start(admission.clone()))
            .expect("Start command should be accepted")
        {
            CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
            other => panic!("unexpected Start result: {other:?}"),
        };
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Steer {
                    attempt: attempt.clone(),
                    request_id: "steer-command-1".to_string(),
                },
            ),
            Ok(CoordinatorCommandResult::Attempt(_))
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Cancel {
                    attempt: attempt.clone(),
                },
            ),
            Ok(CoordinatorCommandResult::Finished(true))
        ));
        assert!(matches!(
            coordinator.execute_command(&session, TurnCommand::Start(admission)),
            Ok(CoordinatorCommandResult::Admission(
                CoordinatorAdmission::Replay(_)
            ))
        ));
    }

    #[test]
    fn continue_command_closes_previous_attempt_with_its_canonical_status() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-command-continue");
        let first = TurnAdmission {
            turn_id: "turn-command-old".to_string(),
            request_id: "request-command-old".to_string(),
            request_fingerprint: "fp-command-old".to_string(),
            profile: ExecutionProfile::Task,
        };
        let previous = match coordinator
            .execute_command(&session, TurnCommand::Start(first))
            .unwrap()
        {
            CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
            other => panic!("unexpected previous admission: {other:?}"),
        };
        let next = TurnAdmission {
            turn_id: "turn-command-next".to_string(),
            request_id: "request-command-next".to_string(),
            request_fingerprint: "fp-command-next".to_string(),
            profile: ExecutionProfile::Task,
        };
        let result = coordinator
            .execute_command(
                &session,
                TurnCommand::Continue {
                    previous: previous.clone(),
                    previous_status: CoordinatorTurnStatus::Failed,
                    next,
                },
            )
            .expect("Continue command should install the next attempt");
        assert!(matches!(
            result,
            CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(_))
        ));
        assert_eq!(
            coordinator.terminal_status(&session, &previous),
            Some(CoordinatorTurnStatus::Failed)
        );
    }

    #[test]
    fn one_session_cannot_start_two_different_turns() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-serial");
        coordinator
            .accept(&session, admission("turn-1", "request-1", "fp-1"))
            .unwrap();
        assert_eq!(
            coordinator
                .accept(&session, admission("turn-2", "request-2", "fp-2"))
                .unwrap_err(),
            CoordinatorError::ActiveTurn {
                active_turn_id: "turn-1".to_string()
            }
        );
    }

    #[test]
    fn late_attempt_is_rejected() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-attempt");
        let attempt = match coordinator
            .accept(&session, admission("turn-1", "request-1", "fp-1"))
            .unwrap()
        {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("first request must be accepted"),
        };
        let stale = TurnAttempt {
            attempt_id: "attempt-stale".to_string(),
            ..attempt.clone()
        };
        assert!(matches!(
            coordinator.finish(&session, &stale, CoordinatorTurnStatus::Completed),
            Err(CoordinatorError::AttemptMismatch { .. })
        ));
    }

    #[test]
    fn canonical_turn_restore_replays_finished_request() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-restore");
        let turn = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-restored".to_string(),
            turn_seq: 1,
            accepted_at: magi_core::UtcMillis(1),
            completed_at: Some(magi_core::UtcMillis(2)),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-restored",
                "requestFingerprint": "sha256:restored",
                "executionProfile": "conversation",
                "attemptId": "attempt-restored",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(coordinator.restore_canonical_turn(&session, &turn));
        let replay = coordinator
            .accept(
                &session,
                TurnAdmission {
                    turn_id: "new-client-turn".to_string(),
                    request_id: "request-restored".to_string(),
                    request_fingerprint: "sha256:restored".to_string(),
                    profile: ExecutionProfile::Conversation,
                },
            )
            .expect("restored request should replay");
        assert!(
            matches!(replay, CoordinatorAdmission::Replay(attempt) if attempt.turn_id == "turn-restored")
        );
    }

    #[test]
    fn canonical_turn_restore_keeps_active_profile_and_attempt() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-restore-active");
        let turn = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-active".to_string(),
            turn_seq: 1,
            accepted_at: magi_core::UtcMillis(1),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-active",
                "requestFingerprint": "sha256:active",
                "executionProfile": "task",
                "attemptId": "attempt-active",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(coordinator.restore_canonical_turn(&session, &turn));
        let attempt = coordinator
            .current_attempt(&session, "turn-active")
            .expect("active Turn should restore");
        assert_eq!(attempt.attempt_id, "attempt-active");
        assert_eq!(attempt.profile, ExecutionProfile::Task);
    }

    #[test]
    fn terminal_finish_rejects_late_different_status() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-terminal-conflict");
        let attempt = match coordinator
            .accept(
                &session,
                admission("turn-terminal", "request-terminal", "fp-terminal"),
            )
            .unwrap()
        {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("first request must be accepted"),
        };
        assert_eq!(
            coordinator.finish(&session, &attempt, CoordinatorTurnStatus::Cancelled),
            Ok(true)
        );
        assert_eq!(
            coordinator.finish(&session, &attempt, CoordinatorTurnStatus::Cancelled),
            Ok(false)
        );
        assert!(matches!(
            coordinator.finish(&session, &attempt, CoordinatorTurnStatus::Completed),
            Err(CoordinatorError::TerminalConflict {
                expected: CoordinatorTurnStatus::Cancelled,
                actual: CoordinatorTurnStatus::Completed
            })
        ));
    }

    #[test]
    fn restore_prefers_latest_active_turn() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-restore-order");
        let make_turn = |turn_id: &str, turn_seq: u64, request_id: &str| CanonicalTurn {
            session_id: session.clone(),
            turn_id: turn_id.to_string(),
            turn_seq,
            accepted_at: magi_core::UtcMillis(turn_seq),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": request_id,
                "requestFingerprint": format!("fp-{request_id}"),
                "executionProfile": "conversation",
                "attemptId": format!("attempt-{turn_id}"),
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(
            coordinator.restore_canonical_turn(&session, &make_turn("turn-old", 1, "request-old"))
        );
        assert!(
            coordinator.restore_canonical_turn(&session, &make_turn("turn-new", 2, "request-new"))
        );
        assert_eq!(
            coordinator
                .current_attempt(&session, "turn-new")
                .unwrap()
                .turn_id,
            "turn-new"
        );
        assert!(matches!(
            coordinator.current_attempt(&session, "turn-old"),
            Err(CoordinatorError::TurnMismatch { .. })
        ));
    }

    #[test]
    fn restore_does_not_resurrect_older_active_turn_after_newer_terminal_turn() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-restore-terminal-order");
        let terminal = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-terminal-new".to_string(),
            turn_seq: 20,
            accepted_at: magi_core::UtcMillis(20),
            completed_at: Some(magi_core::UtcMillis(21)),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-terminal-new",
                "requestFingerprint": "fp-terminal-new",
                "executionProfile": "conversation",
                "attemptId": "attempt-terminal-new",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        let stale_active = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-active-old".to_string(),
            turn_seq: 10,
            accepted_at: magi_core::UtcMillis(10),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-active-old",
                "requestFingerprint": "fp-active-old",
                "executionProfile": "conversation",
                "attemptId": "attempt-active-old",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(coordinator.restore_canonical_turn(&session, &terminal));
        assert!(!coordinator.restore_canonical_turn(&session, &stale_active));
        assert!(matches!(
            coordinator.current_attempt(&session, "turn-active-old"),
            Err(CoordinatorError::NoActiveTurn)
        ));
        assert!(matches!(
            coordinator.accept(
                &session,
                TurnAdmission {
                    turn_id: "turn-next".to_string(),
                    request_id: "request-next".to_string(),
                    request_fingerprint: "fp-next".to_string(),
                    profile: ExecutionProfile::Conversation,
                },
            ),
            Ok(CoordinatorAdmission::Accepted(_))
        ));
    }

    #[test]
    fn request_profile_conflict_is_rejected_and_transition_contract_is_enforced() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-profile-conflict");
        let attempt = match coordinator
            .accept(
                &session,
                admission("turn-profile", "request-profile", "fp-profile"),
            )
            .unwrap()
        {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("first admission must be accepted"),
        };
        assert_eq!(
            coordinator
                .accept(
                    &session,
                    TurnAdmission {
                        turn_id: "turn-other".to_string(),
                        request_id: "request-profile".to_string(),
                        request_fingerprint: "fp-profile".to_string(),
                        profile: ExecutionProfile::Task,
                    },
                )
                .unwrap_err(),
            CoordinatorError::RequestConflict {
                request_id: "request-profile".to_string()
            }
        );
        assert_eq!(
            coordinator.set_status(&session, &attempt.turn_id, CoordinatorTurnStatus::Running,),
            Err(CoordinatorError::InvalidTransition {
                from: CoordinatorTurnStatus::Accepted,
                to: CoordinatorTurnStatus::Running,
            })
        );
        coordinator
            .set_status(&session, &attempt.turn_id, CoordinatorTurnStatus::Preparing)
            .unwrap();
        coordinator
            .set_status(&session, &attempt.turn_id, CoordinatorTurnStatus::Blocked)
            .unwrap();
        coordinator
            .set_status(&session, &attempt.turn_id, CoordinatorTurnStatus::Preparing)
            .unwrap();
    }

    #[test]
    fn restoring_terminal_update_releases_same_turn_active_slot() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-restore-terminal-active");
        let admission = admission(
            "turn-terminal-active",
            "request-terminal-active",
            "fp-terminal-active",
        );
        let attempt = match coordinator.accept(&session, admission.clone()).unwrap() {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("first admission must be accepted"),
        };
        let active = CanonicalTurn {
            session_id: session.clone(),
            turn_id: admission.turn_id.clone(),
            turn_seq: 10,
            accepted_at: magi_core::UtcMillis(10),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": admission.request_id,
                "requestFingerprint": admission.request_fingerprint,
                "executionProfile": "conversation",
                "attemptId": attempt.attempt_id,
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        let mut terminal = active.clone();
        terminal.status = CanonicalTurnStatus::Completed;
        terminal.completed_at = Some(magi_core::UtcMillis(11));
        assert!(coordinator.restore_canonical_turn(&session, &active));
        // restore_canonical_turn 的同 turn active 已经存在时视为幂等；模拟状态被
        // 重新装载到 active 后再提交 terminal projection。
        coordinator.restore_active(
            &session,
            admission.clone(),
            attempt.attempt_id.clone(),
            CoordinatorTurnStatus::Running,
        );
        assert!(coordinator.restore_canonical_turn(&session, &terminal));
        assert!(matches!(
            coordinator.current_attempt(&session, &terminal.turn_id),
            Err(CoordinatorError::NoActiveTurn)
        ));
        assert!(matches!(
            coordinator.accept(
                &session,
                TurnAdmission {
                    turn_id: "turn-after-terminal".to_string(),
                    request_id: "request-after-terminal".to_string(),
                    request_fingerprint: "fp-after-terminal".to_string(),
                    profile: ExecutionProfile::Conversation,
                },
            ),
            Ok(CoordinatorAdmission::Accepted(_))
        ));
    }
}

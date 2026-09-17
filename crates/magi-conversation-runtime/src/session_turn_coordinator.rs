//! Session Turn 的单一生命周期协调器。
//!
//! Coordinator 只保存执行生命周期所需的轻量元数据（当前 turn、attempt 和 profile），
//! 不复制 canonical 内容。所有正文和状态事实仍由 SessionStore 持久化；Coordinator
//! 负责把命令串行化，并拒绝迟到的执行结果。

use magi_core::SessionId;
use magi_session_store::{CanonicalTurn, CanonicalTurnStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::ToolApprovalRegistry;
use crate::mailbox::UserSignal;
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
    Recovered { attempt: TurnAttempt, changed: bool },
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

#[derive(Debug, Default)]
struct SessionState {
    active: Option<ActiveTurn>,
    recent: HashMap<String, FinishedTurn>,
}

#[derive(Debug)]
struct SessionTurnInputState {
    turn_id: String,
    pending: VecDeque<UserSignal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTurnInputError {
    AlreadyActive {
        active_turn_id: String,
    },
    NoActiveTurn,
    TurnMismatch {
        active_turn_id: String,
        expected_turn_id: String,
    },
}

impl fmt::Display for SessionTurnInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive { active_turn_id } => {
                write!(f, "session already has active input turn {active_turn_id}")
            }
            Self::NoActiveTurn => f.write_str("session has no active input turn"),
            Self::TurnMismatch {
                active_turn_id,
                expected_turn_id,
            } => write!(
                f,
                "active input turn {active_turn_id} does not match expected turn {expected_turn_id}"
            ),
        }
    }
}

impl std::error::Error for SessionTurnInputError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTurnInputBoundary {
    Pending(Vec<UserSignal>),
    Closed,
}

#[derive(Debug)]
pub enum SessionTurnInputCommitError<E> {
    Input(SessionTurnInputError),
    Commit(E),
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

#[derive(Debug, Default)]
struct CoordinatorState {
    sessions: HashMap<SessionId, SessionState>,
}

/// 进程内所有 session 的 Coordinator 注册表。
///
/// 每个 session 的命令在同一把状态锁内顺序执行；正文、事件和任务快照不在锁内写入，
/// 因而不会把磁盘 IO 带进生命周期临界区。`recent` 只保存已完成 requestId 的轻量
/// 身份，避免网络重试重新创建 Turn；正文仍从 SessionStore 读取。
#[derive(Clone, Debug, Default)]
pub struct SessionTurnCoordinator {
    state: Arc<Mutex<CoordinatorState>>,
    attempt_sequence: Arc<AtomicU64>,
    /// 当前活跃 Turn 的 steer 输入由 Coordinator 持有，避免 ConversationRegistry
    /// 同时成为 Session Turn 的第二个生命周期所有者。
    session_turn_inputs: Arc<Mutex<HashMap<SessionId, SessionTurnInputState>>>,
    tool_approvals: ToolApprovalRegistry,
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
            TurnCommand::SetStatus { attempt, status } => {
                self.set_status(session_id, &attempt, status)?;
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
                turn_seq,
            } => {
                let turn_id = admission.turn_id.clone();
                let profile = admission.profile;
                let changed = self.apply_recover_command(
                    session_id,
                    admission,
                    attempt_id.clone(),
                    status,
                    turn_seq,
                );
                Ok(CoordinatorCommandResult::Recovered {
                    attempt: TurnAttempt {
                        turn_id,
                        attempt_id,
                        profile,
                    },
                    changed,
                })
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

    fn accept(
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

    fn set_status(
        &self,
        session_id: &SessionId,
        attempt: &TurnAttempt,
        status: CoordinatorTurnStatus,
    ) -> Result<(), CoordinatorError> {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let active = state
            .sessions
            .get_mut(session_id)
            .and_then(|session| session.active.as_mut())
            .ok_or(CoordinatorError::NoActiveTurn)?;
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
        if active.admission.profile != attempt.profile {
            return Err(CoordinatorError::AttemptMismatch {
                expected: active.admission.profile.to_string(),
                actual: attempt.profile.to_string(),
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
    fn finish(
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
    fn abort(
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

    /// 从 canonical Turn 构造 Recover command。
    ///
    /// canonical event 是恢复输入，Coordinator 的状态变更仍必须经过统一命令分发，
    /// 不允许恢复路径直接改写 Coordinator 内部状态。缺少完整幂等和 attempt 身份的
    /// 历史事实不能被猜测为可恢复的请求。
    pub fn recover_command_for_canonical_turn(
        session_id: &SessionId,
        turn: &CanonicalTurn,
    ) -> Option<TurnCommand> {
        if &turn.session_id != session_id {
            // 恢复命令的 session 作用域必须与 canonical 事实一致，避免调用方把
            // 一个 session 的 Turn 错装进另一个 Coordinator 槽位。
            return None;
        }

        let request_id = crate::turn_contract::canonical_turn_metadata_string(turn, "requestId")
            .or_else(|| crate::turn_contract::canonical_turn_metadata_string(turn, "request_id"));
        let Some(request_id) = request_id else {
            // 历史 Turn 没有幂等身份时仍可由 SessionStore 展示，但不能被 Coordinator
            // 当作可安全恢复的请求。
            return None;
        };
        let request_fingerprint =
            crate::turn_contract::canonical_turn_metadata_string(turn, "requestFingerprint")
                .or_else(|| {
                    crate::turn_contract::canonical_turn_metadata_string(
                        turn,
                        "request_fingerprint",
                    )
                })?;
        let profile = crate::turn_contract::canonical_execution_profile(turn)?;
        let attempt_id = crate::turn_contract::canonical_turn_metadata_string(turn, "attemptId")
            .or_else(|| crate::turn_contract::canonical_turn_metadata_string(turn, "attempt_id"))?;
        let admission = TurnAdmission {
            turn_id: turn.turn_id.clone(),
            request_id,
            request_fingerprint,
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
        Some(TurnCommand::Recover {
            admission,
            attempt_id,
            status,
            turn_seq: turn.turn_seq,
        })
    }

    /// 仅供 Coordinator 单元测试构造恢复状态；生产恢复必须通过 canonical Turn
    /// 生成 Recover command。
    #[cfg(test)]
    fn restore_active(
        &self,
        session_id: &SessionId,
        admission: TurnAdmission,
        attempt_id: String,
        status: CoordinatorTurnStatus,
    ) {
        self.execute_command(
            session_id,
            TurnCommand::Recover {
                admission,
                attempt_id,
                status,
                turn_seq: 0,
            },
        )
        .expect("Coordinator Recover command must restore active Turn");
    }

    fn apply_recover_command(
        &self,
        session_id: &SessionId,
        admission: TurnAdmission,
        attempt_id: String,
        status: CoordinatorTurnStatus,
        turn_seq: u64,
    ) -> bool {
        let mut state = self.state.lock().expect("turn coordinator state poisoned");
        let session = state.sessions.entry(session_id.clone()).or_default();
        if status.is_terminal() {
            let finished_attempt = TurnAttempt {
                turn_id: admission.turn_id.clone(),
                attempt_id: attempt_id.clone(),
                profile: admission.profile,
            };
            let active_turn_matches = session
                .active
                .as_ref()
                .is_some_and(|active| active.admission.turn_id == admission.turn_id);

            if let Some(active) = session.active.as_ref() {
                // Recover 终态也必须遵守 attempt 身份。否则同一 Turn 的迟到旧
                // attempt 会清掉当前活动 attempt，并把错误终态写进 replay 索引。
                if active_turn_matches
                    && (active.attempt_id != attempt_id
                        || active.admission != admission
                        || (active.turn_seq != 0 && active.turn_seq != turn_seq))
                {
                    return false;
                }
                if !active_turn_matches && active.admission.request_id == admission.request_id {
                    // requestId 是跨 Turn 的唯一幂等身份。另一个活动 Turn 已占用
                    // 该 request 时，终态恢复不能先写 replay 再影响当前槽位。
                    return false;
                }
            }
            if let Some(existing) = session.recent.get(&admission.request_id) {
                // requestId、Turn ID、profile 和 attempt 都是不可变身份；恢复不能
                // 用另一份 canonical 快照覆盖已经记录的 replay 事实。
                if existing.admission != admission
                    || existing.attempt != finished_attempt
                    || existing.status != status
                {
                    return false;
                }
                if existing.turn_seq != turn_seq {
                    // 进程内刚完成的 Turn 可能还没有从 canonical accepted 事实
                    // 回填序号，FinishedTurn 会暂存 turn_seq = 0。已知 canonical
                    // 序号只能把这个哨兵值补齐一次，不能反向覆盖已知序号，也不能
                    // 用另一个未知序号制造新的 replay 事实。
                    if existing.turn_seq != 0 || turn_seq == 0 {
                        return false;
                    }
                    let existing = session
                        .recent
                        .get_mut(&admission.request_id)
                        .expect("recent replay should still exist");
                    existing.turn_seq = turn_seq;
                    return true;
                }
                // 相同 request/attempt/status/序号的恢复是幂等空操作。
                return false;
            }
            if session.recent.values().any(|existing| {
                existing.attempt.turn_id == admission.turn_id
                    && (existing.admission != admission || existing.attempt != finished_attempt)
            }) {
                return false;
            }

            // 所有身份和序号检查完成后再释放 active。这样任何拒绝都不会破坏
            // 当前执行槽位，即使拒绝原因来自 recent replay 冲突。
            if session
                .active
                .as_ref()
                .is_some_and(|active| active.admission.turn_id == admission.turn_id)
            {
                session.active = None;
            }
            session.recent.insert(
                admission.request_id.clone(),
                FinishedTurn {
                    admission,
                    attempt: finished_attempt,
                    status,
                    turn_seq,
                },
            );
            return true;
        }
        // 终态 replay 已经是该 request 的最终事实，不能再恢复为活动 Turn。
        if session.recent.contains_key(&admission.request_id) {
            return false;
        }
        if let Some(active) = session.active.as_ref() {
            if active.admission.turn_id == admission.turn_id {
                if active.attempt_id != attempt_id || active.admission != admission {
                    return false;
                }
                if active.turn_seq != 0 && active.turn_seq != turn_seq {
                    return false;
                }
                if active.turn_seq == 0 && turn_seq != 0 {
                    session
                        .active
                        .as_mut()
                        .expect("active Turn should still exist")
                        .turn_seq = turn_seq;
                    return true;
                }
                return false;
            }
            if active.admission.request_id == admission.request_id {
                return false;
            }
            if turn_seq <= active.turn_seq {
                return false;
            }
        }
        if session
            .recent
            .values()
            .any(|finished| finished.attempt.turn_id == admission.turn_id)
        {
            return false;
        }
        if session
            .recent
            .values()
            .any(|finished| finished.turn_seq >= turn_seq)
        {
            return false;
        }
        session.active = Some(ActiveTurn {
            admission,
            attempt_id,
            status,
            turn_seq,
        });
        true
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

    pub fn tool_approvals(&self) -> &ToolApprovalRegistry {
        &self.tool_approvals
    }

    /// 注册当前 Turn 的 steer 输入边界。Conversation profile 和 Task profile
    /// 共用该边界，但 Task worker 的 mailbox 仍由 ConversationRegistry 按 task 持有。
    pub fn begin_session_turn_input(
        &self,
        session_id: SessionId,
        turn_id: String,
    ) -> Result<(), SessionTurnInputError> {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        if let Some(active) = guard.get(&session_id) {
            return Err(SessionTurnInputError::AlreadyActive {
                active_turn_id: active.turn_id.clone(),
            });
        }
        self.tool_approvals.begin_turn(&session_id);
        guard.insert(
            session_id,
            SessionTurnInputState {
                turn_id,
                pending: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub fn try_steer_session_turn(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
        signal: UserSignal,
    ) -> Result<(), SessionTurnInputError> {
        self.try_steer_session_turn_with(session_id, expected_turn_id, signal, || {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            SessionTurnInputCommitError::Input(error) => error,
            SessionTurnInputCommitError::Commit(never) => match never {},
        })
    }

    /// 在输入边界锁内先提交 canonical 用户项，再把 signal 放入 FIFO，保证 steer
    /// 不会在写回和接收之间被另一轮 Turn 插入。
    pub fn try_steer_session_turn_with<T, E, F>(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
        signal: UserSignal,
        commit: F,
    ) -> Result<T, SessionTurnInputCommitError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let active = guard
            .get_mut(session_id)
            .ok_or(SessionTurnInputCommitError::Input(
                SessionTurnInputError::NoActiveTurn,
            ))?;
        if active.turn_id != expected_turn_id {
            return Err(SessionTurnInputCommitError::Input(
                SessionTurnInputError::TurnMismatch {
                    active_turn_id: active.turn_id.clone(),
                    expected_turn_id: expected_turn_id.to_string(),
                },
            ));
        }
        let committed = commit().map_err(SessionTurnInputCommitError::Commit)?;
        active.pending.push_back(signal);
        Ok(committed)
    }

    pub fn drain_session_turn_steers(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Vec<UserSignal> {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let Some(active) = guard.get_mut(session_id) else {
            return Vec::new();
        };
        if active.turn_id != turn_id {
            return Vec::new();
        }
        active.pending.drain(..).collect()
    }

    pub fn take_session_turn_steers_or_close(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> SessionTurnInputBoundary {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let Some(active) = guard.get_mut(session_id) else {
            return SessionTurnInputBoundary::Closed;
        };
        if active.turn_id != turn_id {
            return SessionTurnInputBoundary::Closed;
        }
        if active.pending.is_empty() {
            guard.remove(session_id);
            drop(guard);
            self.tool_approvals.remove_turn(session_id, turn_id);
            SessionTurnInputBoundary::Closed
        } else {
            SessionTurnInputBoundary::Pending(active.pending.drain(..).collect())
        }
    }

    pub fn close_session_turn_input(&self, session_id: &SessionId, turn_id: &str) -> bool {
        let mut guard = self
            .session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned");
        let removed = if guard
            .get(session_id)
            .is_some_and(|active| active.turn_id == turn_id)
        {
            guard.remove(session_id);
            true
        } else {
            false
        };
        drop(guard);
        if removed {
            self.tool_approvals.remove_turn(session_id, turn_id);
        }
        removed
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

    pub fn clear_session(&self, session_id: &SessionId) {
        self.state
            .lock()
            .expect("turn coordinator state poisoned")
            .sessions
            .remove(session_id);
        self.session_turn_inputs
            .lock()
            .expect("session turn input mutex poisoned")
            .remove(session_id);
        self.tool_approvals.remove_session(session_id);
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
    fn recover_command_restores_active_attempt() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-command-recover");
        coordinator.restore_active(
            &session,
            TurnAdmission {
                turn_id: "turn-command-recover".to_string(),
                request_id: "request-command-recover".to_string(),
                request_fingerprint: "fp-command-recover".to_string(),
                profile: ExecutionProfile::Task,
            },
            "attempt-command-recover".to_string(),
            CoordinatorTurnStatus::Running,
        );
        let attempt = coordinator
            .current_attempt(&session, "turn-command-recover")
            .expect("Recover command should restore active attempt");
        assert_eq!(attempt.attempt_id, "attempt-command-recover");
        assert_eq!(attempt.profile, ExecutionProfile::Task);
    }

    #[test]
    fn recover_command_rejects_cross_session_canonical_turn() {
        let source_session = SessionId::new("session-recover-source");
        let target_session = SessionId::new("session-recover-target");
        let turn = CanonicalTurn {
            session_id: source_session,
            turn_id: "turn-cross-session".to_string(),
            turn_seq: 1,
            accepted_at: magi_core::UtcMillis(1),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-cross-session",
                "requestFingerprint": "fp-cross-session",
                "executionProfile": "conversation",
                "attemptId": "attempt-cross-session",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };

        assert!(
            SessionTurnCoordinator::recover_command_for_canonical_turn(&target_session, &turn)
                .is_none()
        );
    }

    #[test]
    fn recover_command_requires_complete_replay_identity() {
        let session = SessionId::new("session-recover-identity");
        let mut turn = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-missing-attempt".to_string(),
            turn_seq: 1,
            accepted_at: magi_core::UtcMillis(1),
            completed_at: Some(magi_core::UtcMillis(2)),
            status: CanonicalTurnStatus::Completed,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-missing-attempt",
                "requestFingerprint": "fp-missing-attempt",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none()
        );

        turn.metadata.remove("requestFingerprint");
        turn.metadata.insert(
            "attemptId".to_string(),
            serde_json::Value::String("attempt-missing-fingerprint".to_string()),
        );
        assert!(
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none()
        );
    }

    #[test]
    fn recover_command_rejects_unknown_execution_profile() {
        let session = SessionId::new("session-recover-unknown-profile");
        let turn = CanonicalTurn {
            session_id: session.clone(),
            turn_id: "turn-unknown-profile".to_string(),
            turn_seq: 1,
            accepted_at: magi_core::UtcMillis(1),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: Vec::new(),
            metadata: serde_json::json!({
                "requestId": "request-unknown-profile",
                "requestFingerprint": "fp-unknown-profile",
                "executionProfile": "future-profile",
                "attemptId": "attempt-unknown-profile",
            })
            .as_object()
            .cloned()
            .unwrap()
            .into_iter()
            .collect(),
        };
        assert!(
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none()
        );
    }

    #[test]
    fn recover_command_same_turn_is_not_idempotent_for_different_attempt() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-attempt-conflict");
        let admission = admission(
            "turn-recover-attempt-conflict",
            "request-recover-attempt-conflict",
            "fp-recover-attempt-conflict",
        );
        let first = coordinator
            .execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: "attempt-first".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 2,
                },
            )
            .expect("first recover should be accepted");
        assert!(matches!(
            first,
            CoordinatorCommandResult::Recovered { changed: true, .. }
        ));
        let second = coordinator
            .execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id: "attempt-stale".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 2,
                },
            )
            .expect("stale recover should be handled deterministically");
        assert!(matches!(
            second,
            CoordinatorCommandResult::Recovered { changed: false, .. }
        ));
        assert_eq!(
            coordinator
                .current_attempt(&session, "turn-recover-attempt-conflict")
                .expect("first recovered attempt should remain active")
                .attempt_id,
            "attempt-first"
        );
    }

    #[test]
    fn recover_terminal_stale_attempt_cannot_clear_active_turn() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-terminal-stale-attempt");
        let admission = admission(
            "turn-recover-terminal-stale-attempt",
            "request-recover-terminal-stale-attempt",
            "fp-recover-terminal-stale-attempt",
        );
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: "attempt-current".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 4,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id: "attempt-stale".to_string(),
                    status: CoordinatorTurnStatus::Failed,
                    turn_seq: 5,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
        assert_eq!(
            coordinator
                .current_attempt(&session, "turn-recover-terminal-stale-attempt")
                .expect("stale terminal recover must keep active attempt")
                .attempt_id,
            "attempt-current"
        );
        assert_eq!(
            coordinator.terminal_status(
                &session,
                &TurnAttempt {
                    turn_id: "turn-recover-terminal-stale-attempt".to_string(),
                    attempt_id: "attempt-stale".to_string(),
                    profile: ExecutionProfile::Conversation,
                },
            ),
            None
        );
    }

    #[test]
    fn recover_terminal_rejects_conflict_without_mutating_active_turn() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-terminal-conflict");
        let admission = admission(
            "turn-recover-terminal-conflict",
            "request-recover-terminal-conflict",
            "fp-recover-terminal-conflict",
        );
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: "attempt-current".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 7,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));

        let mut conflicting = admission.clone();
        conflicting.request_fingerprint = "fp-conflict".to_string();
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: conflicting,
                    attempt_id: "attempt-current".to_string(),
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 8,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
        assert_eq!(
            coordinator
                .current_attempt(&session, &admission.turn_id)
                .expect("rejected terminal recover must keep active Turn")
                .attempt_id,
            "attempt-current"
        );
    }

    #[test]
    fn recover_terminal_rejects_active_turn_seq_conflict_without_clearing_active() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-terminal-active-seq-conflict");
        let admission = admission(
            "turn-recover-terminal-active-seq-conflict",
            "request-recover-terminal-active-seq-conflict",
            "fp-recover-terminal-active-seq-conflict",
        );
        let attempt_id = "attempt-recover-terminal-active-seq-conflict".to_string();
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: attempt_id.clone(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 17,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id,
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 18,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
        let state = coordinator
            .state
            .lock()
            .expect("turn coordinator state poisoned");
        let active = state
            .sessions
            .get(&session)
            .and_then(|session| session.active.as_ref())
            .expect("sequence-conflicting terminal recover must keep active Turn");
        assert_eq!(active.turn_seq, 17);
        assert!(
            state
                .sessions
                .get(&session)
                .is_some_and(|session| session.recent.is_empty())
        );
    }

    #[test]
    fn recover_active_turn_seq_is_filled_once_and_then_immutable() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-turn-seq");
        let admission = admission(
            "turn-recover-turn-seq",
            "request-recover-turn-seq",
            "fp-recover-turn-seq",
        );
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: "attempt-recover-turn-seq".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 0,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: "attempt-recover-turn-seq".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 9,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id: "attempt-recover-turn-seq".to_string(),
                    status: CoordinatorTurnStatus::Running,
                    turn_seq: 10,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
    }

    #[test]
    fn recover_terminal_rejects_different_turn_seq_for_existing_replay() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-terminal-seq-conflict");
        let admission = admission(
            "turn-recover-terminal-seq-conflict",
            "request-recover-terminal-seq-conflict",
            "fp-recover-terminal-seq-conflict",
        );
        let attempt_id = "attempt-recover-terminal-seq-conflict".to_string();
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: attempt_id.clone(),
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 11,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id,
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 12,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
    }

    #[test]
    fn recover_terminal_fills_unknown_recent_turn_seq_once() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-recover-terminal-seq-fill");
        let admission = admission(
            "turn-recover-terminal-seq-fill",
            "request-recover-terminal-seq-fill",
            "fp-recover-terminal-seq-fill",
        );
        let attempt_id = "attempt-recover-terminal-seq-fill".to_string();
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: attempt_id.clone(),
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 0,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission: admission.clone(),
                    attempt_id: attempt_id.clone(),
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 13,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        assert_eq!(
            coordinator
                .state
                .lock()
                .expect("turn coordinator state poisoned")
                .sessions
                .get(&session)
                .and_then(|session| session.recent.get(&admission.request_id))
                .map(|finished| finished.turn_seq),
            Some(13)
        );
        assert!(matches!(
            coordinator.execute_command(
                &session,
                TurnCommand::Recover {
                    admission,
                    attempt_id,
                    status: CoordinatorTurnStatus::Completed,
                    turn_seq: 14,
                },
            ),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
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
        let command = SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn)
            .expect("canonical Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, command),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
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
        let command = SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn)
            .expect("canonical Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, command),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
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
        for turn in [
            make_turn("turn-old", 1, "request-old"),
            make_turn("turn-new", 2, "request-new"),
        ] {
            let command =
                SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn)
                    .expect("canonical Turn should produce Recover command");
            assert!(matches!(
                coordinator.execute_command(&session, command),
                Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
            ));
        }
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
        let terminal_command =
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &terminal)
                .expect("terminal Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, terminal_command),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        let stale_command =
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &stale_active)
                .expect("stale Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, stale_command),
            Ok(CoordinatorCommandResult::Recovered { changed: false, .. })
        ));
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
            coordinator.execute_command(
                &session,
                TurnCommand::SetStatus {
                    attempt: TurnAttempt {
                        turn_id: attempt.turn_id.clone(),
                        attempt_id: "attempt-stale".to_string(),
                        profile: attempt.profile,
                    },
                    status: CoordinatorTurnStatus::Preparing,
                },
            ),
            Err(CoordinatorError::AttemptMismatch {
                expected: attempt.attempt_id.clone(),
                actual: "attempt-stale".to_string(),
            })
        );
        assert_eq!(
            coordinator.execute_command(
                &session,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Running,
                },
            ),
            Err(CoordinatorError::InvalidTransition {
                from: CoordinatorTurnStatus::Accepted,
                to: CoordinatorTurnStatus::Running,
            })
        );
        coordinator
            .execute_command(
                &session,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Preparing,
                },
            )
            .unwrap();
        coordinator
            .execute_command(
                &session,
                TurnCommand::SetStatus {
                    attempt: attempt.clone(),
                    status: CoordinatorTurnStatus::Blocked,
                },
            )
            .unwrap();
        coordinator
            .execute_command(
                &session,
                TurnCommand::SetStatus {
                    attempt,
                    status: CoordinatorTurnStatus::Preparing,
                },
            )
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
        let active_command =
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &active)
                .expect("active Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, active_command),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
        // 同一 Turn 的 active 恢复已经存在时视为幂等；模拟状态被
        // 重新装载到 active 后再提交 terminal projection。
        coordinator.restore_active(
            &session,
            admission.clone(),
            attempt.attempt_id.clone(),
            CoordinatorTurnStatus::Running,
        );
        let terminal_command =
            SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &terminal)
                .expect("terminal Turn should produce Recover command");
        assert!(matches!(
            coordinator.execute_command(&session, terminal_command),
            Ok(CoordinatorCommandResult::Recovered { changed: true, .. })
        ));
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

    #[test]
    fn steer_input_is_cleared_with_coordinator_session_state() {
        let coordinator = SessionTurnCoordinator::new();
        let session = SessionId::new("session-coordinator-steer-input");
        coordinator
            .begin_session_turn_input(session.clone(), "turn-coordinator-steer".to_string())
            .expect("Coordinator should own the steer input boundary");
        coordinator
            .try_steer_session_turn(
                &session,
                "turn-coordinator-steer",
                UserSignal {
                    text: Some("收口".to_string()),
                    request_id: Some("request-coordinator-steer".to_string()),
                    user_message_id: None,
                    placeholder_message_id: None,
                    accepted_at: magi_core::UtcMillis(1),
                },
            )
            .expect("matching steer should be queued");
        coordinator.clear_session(&session);
        assert_eq!(
            coordinator.take_session_turn_steers_or_close(&session, "turn-coordinator-steer"),
            SessionTurnInputBoundary::Closed
        );
    }
}

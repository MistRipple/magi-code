#[cfg(test)]
use super::TimelineEntryInput;
use super::{SessionStore, unique_timeline_entry_id};
#[cfg(test)]
use crate::models::ActiveExecutionTurn;
use crate::models::{
    GoalBlockerState, GoalCompletionRecord, GoalContinuationPhase, GoalContinuationState,
    GoalResumeCheckpoint, GoalRevisionExpectation, GoalRuntimeFailureDisposition, GoalStatus,
    SessionGoal, SessionPlan, SessionStoreState, TimelineEntry, TimelineEntryKind,
};
use magi_core::{
    AccessProfile, DomainError, DomainResult, GoalId, GoalRejection, PlanItemStatus, PlanState,
    SessionId, ThreadId, UtcMillis,
};

const BLOCKED_TURN_THRESHOLD: u32 = 3;
/// 目标的自动续跑轮次连续因运行时错误失败多少次后才进入受阻。
const GOAL_RUNTIME_FAILURE_ATTEMPTS: u32 = 3;

const MAX_GOAL_OBJECTIVE_CHARS: usize = 4_000;

pub(super) fn activate_paused_plan(plan: &mut SessionPlan, now: UtcMillis) {
    if !plan
        .items
        .iter()
        .any(|item| item.status == PlanItemStatus::InProgress)
    {
        let next_index = plan
            .items
            .iter()
            .position(|item| item.status == PlanItemStatus::Blocked)
            .or_else(|| {
                plan.items
                    .iter()
                    .position(|item| item.status == PlanItemStatus::Pending)
            });
        if let Some(next_index) = next_index {
            plan.items[next_index].status = PlanItemStatus::InProgress;
        }
    }
    plan.state = if plan.items.iter().all(|item| {
        matches!(
            item.status,
            PlanItemStatus::Completed | PlanItemStatus::Canceled
        )
    }) {
        PlanState::Completed
    } else {
        PlanState::Active
    };
    plan.revision = plan.revision.saturating_add(1);
    plan.updated_at = now;
}
pub(super) fn pause_goal_and_bound_plan_in_state(
    state: &mut crate::models::SessionStoreState,
    goal_index: usize,
    now: UtcMillis,
) -> (SessionGoal, Option<SessionPlan>) {
    let goal_id = state.goals[goal_index].goal_id.clone();
    let updated_plan = state
        .plans
        .iter_mut()
        .find(|plan| {
            plan.session_id == state.goals[goal_index].session_id
                && plan.goal_id.as_ref() == Some(&goal_id)
        })
        .map(|plan| {
            if plan.state != PlanState::Paused {
                plan.state = PlanState::Paused;
                plan.revision = plan.revision.saturating_add(1);
                plan.updated_at = now;
            }
            plan.clone()
        });
    let goal = &mut state.goals[goal_index];
    if goal.status != GoalStatus::Paused {
        goal.status = GoalStatus::Paused;
        goal.control_revision = goal.control_revision.saturating_add(1);
    }
    goal.blocker = None;
    goal.continuation = GoalContinuationState::default();
    goal.updated_at = now;
    (goal.clone(), updated_plan)
}

fn normalize_objective(objective: impl Into<String>) -> DomainResult<String> {
    let objective = objective.into();
    let trimmed = objective.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation {
            message: "goal objective cannot be empty".to_string(),
        });
    }
    if trimmed.chars().count() > MAX_GOAL_OBJECTIVE_CHARS {
        return Err(DomainError::Validation {
            message: format!("goal objective cannot exceed {MAX_GOAL_OBJECTIVE_CHARS} characters"),
        });
    }
    Ok(trimmed.to_string())
}

fn new_goal_id(session_id: &SessionId, now: UtcMillis) -> DomainResult<GoalId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|error| DomainError::InvalidState {
        message: format!("failed to generate goal identity: {error}"),
    })?;
    let entropy = magi_core::hex_lower(entropy);
    Ok(GoalId::new(format!(
        "goal-{session_id}-{}-{entropy}",
        now.0
    )))
}

impl SessionStore {
    pub fn create_goal(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        turn_id: impl Into<String>,
        objective: impl Into<String>,
        access_profile: AccessProfile,
    ) -> DomainResult<SessionGoal> {
        let turn_id = normalize_non_empty(turn_id, "goal turn_id")?;
        let objective = normalize_objective(objective)?;
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !state
            .sessions
            .iter()
            .any(|session| session.session_id == session_id)
        {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let orchestrator_thread = state
            .thread_registry
            .iter()
            .find(|candidate| {
                candidate.session_id == session_id
                    && candidate.role_id == super::ORCHESTRATOR_ROLE_ID
            })
            .ok_or_else(|| DomainError::GoalRejected {
                reason: GoalRejection::NoOrchestratorThread,
                message: "session 尚未建立 orchestrator thread，不能创建 goal".to_string(),
            })?;
        if orchestrator_thread.thread_id != thread_id {
            return Err(DomainError::Validation {
                message: "goal 必须归属当前 session 的 orchestrator thread".to_string(),
            });
        }
        if state
            .goals
            .iter()
            .any(|goal| goal.session_id == session_id && goal.status.is_unfinished())
        {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::AlreadyUnfinished,
                message: "session already has an unfinished goal".to_string(),
            });
        }
        let goal = SessionGoal {
            goal_id: new_goal_id(&session_id, now)?,
            session_id: session_id.clone(),
            thread_id,
            created_by_turn_id: Some(turn_id),
            objective: objective.clone(),
            status: GoalStatus::Active,
            control_revision: 1,
            access_profile,
            tokens_used: 0,
            model_attempts_used: 0,
            time_used_seconds: 0,
            time_used_millis: 0,
            timing_started_at: None,
            timing_turn_id: None,
            blocker: None,
            continuation: GoalContinuationState::default(),
            completion: None,
            created_at: now,
            updated_at: now,
        };
        state
            .goals
            .retain(|existing| existing.session_id != session_id);
        state.plans.retain(|plan| plan.session_id != session_id);
        state.goals.push(goal.clone());
        let entry_id = unique_timeline_entry_id(
            &state.timeline,
            format!("timeline-goal-created-{}", goal.goal_id),
        );
        state.timeline.push(TimelineEntry {
            entry_id,
            session_id,
            kind: TimelineEntryKind::NotificationPublished,
            message: format!("目标已创建: {objective}"),
            occurred_at: now,
        });
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(state.goals.last().cloned().unwrap_or(goal))
    }

    pub fn set_active_goal_access_profile(
        &self,
        session_id: &SessionId,
        access_profile: AccessProfile,
    ) -> DomainResult<Option<SessionGoal>> {
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        if !state
            .sessions
            .iter()
            .any(|session| &session.session_id == session_id)
        {
            return Err(DomainError::NotFound { entity: "session" });
        }
        let Some(goal) = state
            .goals
            .iter_mut()
            .find(|goal| &goal.session_id == session_id && goal.status == GoalStatus::Active)
        else {
            return Ok(None);
        };
        if goal.access_profile != access_profile {
            goal.access_profile = access_profile;
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = now;
        }
        Ok(Some(goal.clone()))
    }

    pub fn current_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .find(|goal| &goal.session_id == session_id)
            .cloned()
    }

    pub fn current_unfinished_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .find(|goal| &goal.session_id == session_id && goal.status.is_unfinished())
            .cloned()
    }

    pub fn current_visible_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        self.current_goal(session_id)
    }

    pub fn active_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .find(|goal| &goal.session_id == session_id && goal.status == GoalStatus::Active)
            .cloned()
    }

    pub fn waiting_goal_session_ids(&self) -> Vec<SessionId> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .filter(|goal| {
                goal.status == GoalStatus::Active
                    && goal.continuation.phase == GoalContinuationPhase::Waiting
            })
            .map(|goal| goal.session_id.clone())
            .collect()
    }

    pub fn update_goal_objective_if_revision(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        objective: impl Into<String>,
        expected_revision: Option<u64>,
    ) -> DomainResult<SessionGoal> {
        let objective = normalize_objective(objective)?;
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal = state
            .goals
            .iter_mut()
            .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        validate_control_revision(goal, expected_revision)?;
        if !goal.status.is_unfinished() {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::Terminal,
                message: "terminal goal objective cannot be edited".to_string(),
            });
        }
        goal.objective = objective;
        goal.control_revision = goal.control_revision.saturating_add(1);
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn record_goal_turn_success(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        turn_id: &str,
    ) -> DomainResult<SessionGoal> {
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal = state
            .goals
            .iter_mut()
            .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        if !goal_owned_by_turn(goal, turn_id) {
            return Ok(goal.clone());
        }
        if goal.status == GoalStatus::Active {
            if goal
                .blocker
                .as_ref()
                .is_none_or(|blocker| blocker.last_observed_turn_id != turn_id)
            {
                goal.blocker = None;
            }
            goal.continuation = GoalContinuationState::default();
            goal.updated_at = now;
        }
        Ok(goal.clone())
    }

    /// 目标轮次因运行时错误失败（模型请求超时、网络抖动、执行环境暂时不可用等）。
    ///
    /// 一次偶发故障不应该让几小时的目标停下来等人：连续失败不到 [`GOAL_RUNTIME_FAILURE_ATTEMPTS`]
    /// 次时只记录并保持 active，由调用方退避后自动续跑；连续达到上限（中间没有成功轮次）才受阻。
    /// 配置类的确定性失败（例如模型未配置）重试没有意义，直接用 [`Self::stop_goal_for_runtime_failure`]。
    pub fn observe_goal_runtime_failure(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        turn_id: &str,
        reason: impl Into<String>,
    ) -> DomainResult<(SessionGoal, GoalRuntimeFailureDisposition)> {
        let reason = reason.into();
        let now = UtcMillis::now();
        {
            let mut state = self
                .state
                .write()
                .expect("session state write lock poisoned");
            let goal = state
                .goals
                .iter_mut()
                .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
                .ok_or(DomainError::NotFound { entity: "goal" })?;
            if !goal_owned_by_turn(goal, turn_id) || goal.status != GoalStatus::Active {
                return Ok((goal.clone(), GoalRuntimeFailureDisposition::Ignored));
            }
            let attempt = match goal.blocker.as_ref() {
                Some(previous) if previous.blocker_key == "runtime_error" => {
                    if previous.last_observed_turn_id == turn_id {
                        previous.consecutive_turns
                    } else {
                        previous.consecutive_turns.saturating_add(1)
                    }
                }
                _ => 1,
            };
            if attempt < GOAL_RUNTIME_FAILURE_ATTEMPTS {
                goal.blocker = Some(GoalBlockerState {
                    blocker_key: "runtime_error".to_string(),
                    reason: reason.clone(),
                    consecutive_turns: attempt,
                    last_observed_turn_id: turn_id.to_string(),
                });
                goal.continuation = GoalContinuationState {
                    phase: GoalContinuationPhase::Waiting,
                    turn_id: None,
                    reason: Some(reason),
                };
                goal.control_revision = goal.control_revision.saturating_add(1);
                goal.updated_at = now;
                let goal = goal.clone();
                super::sidecar::reconcile_goal_time_used(&mut state);
                return Ok((goal, GoalRuntimeFailureDisposition::Retrying { attempt }));
            }
        }
        let blocked =
            self.stop_goal_for_runtime_failure(session_id, goal_id, None, turn_id, reason)?;
        Ok((blocked, GoalRuntimeFailureDisposition::Blocked))
    }

    pub fn stop_goal_for_runtime_failure(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: Option<u64>,
        turn_id: &str,
        reason: impl Into<String>,
    ) -> DomainResult<SessionGoal> {
        let now = UtcMillis::now();
        let reason = reason.into();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        if let Some(expected_revision) = expected_revision {
            validate_control_revision(&state.goals[goal_index], Some(expected_revision))?;
        } else if !goal_owned_by_turn(&state.goals[goal_index], turn_id) {
            return Ok(state.goals[goal_index].clone());
        }
        if state.goals[goal_index].status != GoalStatus::Active {
            return Ok(state.goals[goal_index].clone());
        }
        if let Some(plan) = state
            .plans
            .iter_mut()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id))
        {
            plan.state = PlanState::Paused;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at = now;
        }
        {
            let goal = &mut state.goals[goal_index];
            goal.status = GoalStatus::Blocked;
            goal.blocker = Some(GoalBlockerState {
                blocker_key: "runtime_error".to_string(),
                reason: reason.clone(),
                consecutive_turns: BLOCKED_TURN_THRESHOLD,
                last_observed_turn_id: turn_id.to_string(),
            });
            goal.continuation = GoalContinuationState {
                phase: GoalContinuationPhase::Waiting,
                turn_id: None,
                reason: Some(reason),
            };
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = now;
        }
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(state.goals[goal_index].clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn observe_goal_blocker(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
        turn_id: &str,
        blocker_key: impl Into<String>,
        reason: impl Into<String>,
    ) -> DomainResult<SessionGoal> {
        let blocker_key = normalize_non_empty(blocker_key, "blocker_key")?;
        let reason = normalize_non_empty(reason, "blocker reason")?;
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        validate_control_revision(&state.goals[goal_index], Some(expected_revision))?;
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_plan_revision)?;
        if state.goals[goal_index].status != GoalStatus::Active {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::NotActive,
                message: "only an active goal can observe a blocker".to_string(),
            });
        }
        if !goal_owned_by_turn(&state.goals[goal_index], turn_id) {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::NotOwnedByTurn,
                message: "only the goal-owning turn can observe a blocker".to_string(),
            });
        }
        let consecutive_turns = match state.goals[goal_index].blocker.as_ref() {
            Some(previous) if previous.last_observed_turn_id == turn_id => {
                previous.consecutive_turns
            }
            Some(previous) if previous.blocker_key == blocker_key => {
                previous.consecutive_turns.saturating_add(1)
            }
            _ => 1,
        };
        if consecutive_turns >= BLOCKED_TURN_THRESHOLD
            && let Some(plan) = state.plans.iter_mut().find(|plan| {
                &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id)
            })
        {
            plan.state = PlanState::Paused;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at = now;
        }
        {
            let goal = &mut state.goals[goal_index];
            goal.blocker = Some(GoalBlockerState {
                blocker_key,
                reason: reason.clone(),
                consecutive_turns,
                last_observed_turn_id: turn_id.to_string(),
            });
            if consecutive_turns >= BLOCKED_TURN_THRESHOLD {
                goal.status = GoalStatus::Blocked;
                goal.continuation = GoalContinuationState {
                    phase: GoalContinuationPhase::Waiting,
                    turn_id: None,
                    reason: Some(reason),
                };
            }
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = now;
        }
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(state.goals[goal_index].clone())
    }

    pub fn set_goal_task_activity_resolver(
        &self,
        resolver: std::sync::Arc<super::GoalTaskActivityResolver>,
    ) {
        *self
            .goal_task_activity_resolver
            .write()
            .expect("goal task resolver lock poisoned") = Some(resolver);
    }

    pub fn complete_goal(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revisions: GoalRevisionExpectation,
        turn_id: impl Into<String>,
        summary: impl Into<String>,
        evidence_refs: Vec<String>,
    ) -> DomainResult<SessionGoal> {
        let turn_id = normalize_non_empty(turn_id, "completion turn_id")?;
        let summary = normalize_non_empty(summary, "completion summary")?;
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        validate_control_revision(
            &state.goals[goal_index],
            Some(expected_revisions.goal_control_revision),
        )?;
        if state.goals[goal_index].status != GoalStatus::Active {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::NotActive,
                message: "only an active goal can be completed".to_string(),
            });
        }
        if !goal_owned_by_turn(&state.goals[goal_index], &turn_id) {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::NotOwnedByTurn,
                message: "only the goal-owning turn can complete the goal".to_string(),
            });
        }
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_revisions.plan_revision)?;
        if let Some(plan) = bound_plan {
            if plan.items.iter().any(|item| {
                matches!(
                    item.status,
                    PlanItemStatus::Pending | PlanItemStatus::InProgress | PlanItemStatus::Blocked
                )
            }) {
                return Err(DomainError::GoalRejected {
                    reason: GoalRejection::PlanUnfinished,
                    message: "goal plan still contains unfinished or blocked work".to_string(),
                });
            }
            let has_active_tasks = if plan.task_bindings.is_empty() {
                false
            } else {
                let root = state
                    .execution_sidecar_store
                    .runtime_sidecars
                    .iter()
                    .find(|sidecar| &sidecar.session_id == session_id)
                    .and_then(|sidecar| sidecar.active_execution_chain.as_ref())
                    .map(|chain| &chain.root_task_id)
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "目标任务查询缺少执行根归属".into(),
                    })?;
                let resolver = self
                    .goal_task_activity_resolver
                    .read()
                    .expect("goal task resolver lock poisoned")
                    .clone()
                    .ok_or_else(|| DomainError::InvalidState {
                        message: "目标任务状态查询不可用".into(),
                    })?;
                resolver(
                    root,
                    session_id,
                    goal_id,
                    &plan.task_bindings.keys().cloned().collect::<Vec<_>>(),
                )?
            };
            if has_active_tasks {
                return Err(DomainError::GoalRejected {
                    reason: GoalRejection::PlanTasksActive,
                    message: "goal plan still has active bound tasks".to_string(),
                });
            }
        }
        let plan_revision = bound_plan.map(|plan| plan.revision);
        let evidence_refs = normalize_evidence_refs(evidence_refs);
        if bound_plan.is_some() && evidence_refs.is_empty() {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::EvidenceRequired,
                message: "completed goal with a bound plan requires evidence_refs".to_string(),
            });
        }
        {
            let goal = &mut state.goals[goal_index];
            goal.status = GoalStatus::Complete;
            goal.completion = Some(GoalCompletionRecord {
                turn_id,
                summary,
                plan_revision,
                evidence_refs,
                completed_at: now,
            });
            goal.continuation = GoalContinuationState::default();
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = now;
        }
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(state.goals[goal_index].clone())
    }

    pub fn pause_goal_with_plan(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
    ) -> DomainResult<(SessionGoal, Option<SessionPlan>)> {
        let (goal, plan, _) = self.transition_goal_with_plan(
            session_id,
            goal_id,
            GoalRevisionExpectation::new(expected_revision, expected_plan_revision),
            GoalStatus::Paused,
            None,
        )?;
        Ok((goal, plan))
    }

    pub fn pause_active_goal_for_diversion(
        &self,
        session_id: &SessionId,
    ) -> DomainResult<Option<(SessionGoal, Option<SessionPlan>)>> {
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let Some(goal_index) = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && goal.status == GoalStatus::Active)
        else {
            return Ok(None);
        };
        let result = pause_goal_and_bound_plan_in_state(&mut state, goal_index, now);
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(Some(result))
    }

    pub fn resume_goal_with_plan(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
        new_access_profile: Option<AccessProfile>,
    ) -> DomainResult<(SessionGoal, Option<SessionPlan>, GoalResumeCheckpoint)> {
        let (goal, plan, checkpoint) = self.transition_goal_with_plan(
            session_id,
            goal_id,
            GoalRevisionExpectation::new(expected_revision, expected_plan_revision),
            GoalStatus::Active,
            new_access_profile,
        )?;
        Ok((
            goal,
            plan,
            checkpoint.expect("resume transition must produce checkpoint"),
        ))
    }

    fn transition_goal_with_plan(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revisions: GoalRevisionExpectation,
        next_status: GoalStatus,
        new_access_profile: Option<AccessProfile>,
    ) -> DomainResult<(
        SessionGoal,
        Option<SessionPlan>,
        Option<GoalResumeCheckpoint>,
    )> {
        let _barrier = self
            .canonical_commit_barrier
            .read()
            .expect("canonical barrier poisoned");
        let lock = self.canonical_commit_lock_for(session_id);
        let _guard = lock.lock().expect("canonical session lock poisoned");
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        validate_control_revision(
            &state.goals[goal_index],
            Some(expected_revisions.goal_control_revision),
        )?;
        let current_status = state.goals[goal_index].status;
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_revisions.plan_revision)?;
        let allowed = match next_status {
            GoalStatus::Paused => current_status == GoalStatus::Active,
            GoalStatus::Active => {
                matches!(current_status, GoalStatus::Paused | GoalStatus::Blocked)
            }
            _ => false,
        };
        if !allowed {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::IllegalTransition,
                message: format!(
                    "illegal goal transition: {:?} -> {:?}",
                    current_status, next_status
                ),
            });
        }
        let goal_before = state.goals[goal_index].clone();
        let plan_before = bound_plan.cloned();
        let mut updated_plan = None;
        if let Some(plan) = state
            .plans
            .iter_mut()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id))
        {
            match next_status {
                GoalStatus::Paused => {
                    plan.state = PlanState::Paused;
                }
                GoalStatus::Active => {
                    if plan.state == PlanState::Paused {
                        activate_paused_plan(plan, now);
                    }
                }
                _ => unreachable!("goal-plan transition only supports pause/resume"),
            }
            if next_status == GoalStatus::Paused {
                plan.revision = plan.revision.saturating_add(1);
                plan.updated_at = now;
            }
            updated_plan = Some(plan.clone());
        }
        {
            let goal = &mut state.goals[goal_index];
            goal.status = next_status;

            if let Some(new_access_profile) = new_access_profile {
                goal.access_profile = new_access_profile;
            }
            goal.blocker = None;
            goal.continuation = if next_status == GoalStatus::Active {
                GoalContinuationState {
                    phase: GoalContinuationPhase::Waiting,
                    turn_id: None,
                    reason: Some("resume_requested".to_string()),
                }
            } else {
                GoalContinuationState::default()
            };
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.updated_at = now;
        }
        super::sidecar::reconcile_goal_time_used(&mut state);
        let checkpoint = (next_status == GoalStatus::Active).then(|| GoalResumeCheckpoint {
            goal_before,
            plan_before,
            applied_goal_revision: state.goals[goal_index].control_revision,
            applied_plan_revision: updated_plan.as_ref().map(|plan| plan.revision),
        });
        Ok((state.goals[goal_index].clone(), updated_plan, checkpoint))
    }

    pub fn rollback_goal_resume(&self, checkpoint: GoalResumeCheckpoint) -> DomainResult<()> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| goal.goal_id == checkpoint.goal_before.goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        let current_goal = &state.goals[goal_index];
        if current_goal.control_revision != checkpoint.applied_goal_revision
            || current_goal.status != GoalStatus::Active
            || current_goal.continuation.phase != GoalContinuationPhase::Waiting
        {
            return Err(DomainError::GoalRejected {
                reason: GoalRejection::ConcurrentModification,
                message: "恢复请求后的 Goal 已被并发修改，拒绝覆盖回滚".to_string(),
            });
        }
        match checkpoint.plan_before {
            Some(plan_before) => {
                let plan_index = state
                    .plans
                    .iter()
                    .position(|plan| plan.plan_id == plan_before.plan_id)
                    .ok_or(DomainError::NotFound { entity: "plan" })?;
                if Some(state.plans[plan_index].revision) != checkpoint.applied_plan_revision {
                    return Err(DomainError::GoalRejected {
                        reason: GoalRejection::ConcurrentModification,
                        message: "恢复请求后的 plan 已被并发修改，拒绝覆盖回滚".to_string(),
                    });
                }
                state.plans[plan_index] = plan_before;
            }
            None => {
                if state.plans.iter().any(|plan| {
                    plan.session_id == checkpoint.goal_before.session_id
                        && plan.goal_id.as_ref() == Some(&checkpoint.goal_before.goal_id)
                }) {
                    return Err(DomainError::GoalRejected {
                        reason: GoalRejection::ConcurrentModification,
                        message: "恢复请求后出现新的绑定 plan，拒绝覆盖回滚".to_string(),
                    });
                }
            }
        }
        state.goals[goal_index] = checkpoint.goal_before;
        reconcile_goal_token_usage(&mut state);
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(())
    }

    pub fn clear_goal_with_plan(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
    ) -> DomainResult<(SessionGoal, Option<SessionPlan>)> {
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal_index = state
            .goals
            .iter()
            .position(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        validate_control_revision(&state.goals[goal_index], Some(expected_revision))?;
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_plan_revision)?;
        let removed_goal = state.goals.remove(goal_index);
        let removed_plan = state
            .plans
            .iter()
            .position(|plan| {
                &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id)
            })
            .map(|index| state.plans.remove(index));
        Ok((removed_goal, removed_plan))
    }

    pub fn mark_goal_continuation_waiting(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        reason: impl Into<String>,
    ) -> DomainResult<SessionGoal> {
        let reason = normalize_non_empty(reason, "continuation wait reason")?;
        let now = UtcMillis::now();
        let mut state = self
            .state
            .write()
            .expect("session state write lock poisoned");
        let goal = state
            .goals
            .iter_mut()
            .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .ok_or(DomainError::NotFound { entity: "goal" })?;
        if goal.status != GoalStatus::Active {
            return Ok(goal.clone());
        }
        goal.continuation = GoalContinuationState {
            phase: GoalContinuationPhase::Waiting,
            turn_id: None,
            reason: Some(reason),
        };
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn goals_for_session(&self, session_id: &SessionId) -> Vec<SessionGoal> {
        self.state
            .read()
            .expect("session state read lock poisoned")
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id)
            .cloned()
            .collect()
    }
}

fn validate_control_revision(
    goal: &SessionGoal,
    expected_revision: Option<u64>,
) -> DomainResult<()> {
    if let Some(expected_revision) = expected_revision
        && goal.control_revision != expected_revision
    {
        return Err(DomainError::GoalRejected {
            reason: GoalRejection::ControlRevisionConflict,
            message: format!(
                "goal revision conflict: expected {}, current {}",
                expected_revision, goal.control_revision
            ),
        });
    }
    Ok(())
}

fn validate_bound_plan_revision(
    plan: Option<&SessionPlan>,
    expected_revision: Option<u64>,
) -> DomainResult<()> {
    match (plan, expected_revision) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(DomainError::GoalRejected {
            reason: GoalRejection::PlanMissing,
            message: "goal plan no longer exists".to_string(),
        }),
        (Some(plan), None) => Err(DomainError::GoalRejected {
            reason: GoalRejection::PlanRevisionRequired,
            message: format!(
                "goal plan revision is required; current revision is {}",
                plan.revision
            ),
        }),
        (Some(plan), Some(expected_revision)) if plan.revision != expected_revision => {
            Err(DomainError::GoalRejected {
                reason: GoalRejection::PlanRevisionConflict,
                message: format!(
                    "goal plan revision conflict: expected {}, current {}",
                    expected_revision, plan.revision
                ),
            })
        }
        (Some(_), Some(_)) => Ok(()),
    }
}

fn goal_owned_by_turn(goal: &SessionGoal, turn_id: &str) -> bool {
    goal.created_by_turn_id.as_deref() == Some(turn_id)
        || (goal.continuation.phase == GoalContinuationPhase::Running
            && goal.continuation.turn_id.as_deref() == Some(turn_id))
}

fn normalize_non_empty(value: impl Into<String>, field: &str) -> DomainResult<String> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation {
            message: format!("{field} cannot be empty"),
        });
    }
    Ok(trimmed.to_string())
}

fn normalize_evidence_refs(evidence_refs: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for evidence_ref in evidence_refs {
        let evidence_ref = evidence_ref.trim();
        if !evidence_ref.is_empty() && !normalized.iter().any(|item| item == evidence_ref) {
            normalized.push(evidence_ref.to_string());
        }
    }
    normalized
}

/// Step 的累计水位是 token 记账事实；Goal 只是读取投影。保留旧快照已有的基数，
/// 事件重放或重复重建只能前进到最大水位，不能再次累加同一响应。
pub(super) fn reconcile_goal_token_usage(state: &mut SessionStoreState) {
    for turn in &state.canonical_turns {
        apply_goal_token_watermarks(&mut state.goals, &turn.session_id, &turn.model_steps);
    }
    reconcile_goal_attempt_usage(state);
}

pub(super) fn apply_goal_token_watermarks(
    goals: &mut [SessionGoal],
    session_id: &SessionId,
    steps: &[crate::ModelStep],
) {
    for step in steps {
        if let Some((id, after)) = step.goal_id.as_ref().zip(step.goal_tokens_after)
            && let Some(goal) = goals
                .iter_mut()
                .find(|goal| &goal.session_id == session_id && &goal.goal_id == id)
            && after > goal.tokens_used
        {
            goal.tokens_used = after;
            goal.updated_at = goal
                .updated_at
                .max(step.settled_at.unwrap_or(step.started_at));
        }
    }
}

/// 只在恢复时全量重建；热路径按变化 Turn 维护请求次数统计。
pub(super) fn reconcile_goal_attempt_usage(state: &mut SessionStoreState) {
    for goal in &mut state.goals {
        goal.model_attempts_used = 0;
    }
    for turn in &state.canonical_turns {
        apply_goal_attempt_change(&mut state.goals, None, turn);
    }
}

pub(super) fn apply_goal_attempt_change(
    goals: &mut [SessionGoal],
    previous: Option<&crate::CanonicalTurn>,
    next: &crate::CanonicalTurn,
) {
    for goal in goals
        .iter_mut()
        .filter(|goal| goal.session_id == next.session_id)
    {
        let attempts = |turn: &crate::CanonicalTurn| {
            turn.model_steps
                .iter()
                .filter(|step| step.goal_id.as_ref() == Some(&goal.goal_id))
                .map(|step| step.attempts.len() as u64)
                .fold(0_u64, u64::saturating_add)
        };
        goal.model_attempts_used = goal
            .model_attempts_used
            .saturating_sub(previous.map_or(0, attempts))
            .saturating_add(attempts(next));
    }
}

#[cfg(test)]
mod tests;

use super::{SessionStore, unique_timeline_entry_id};
use crate::models::{
    GoalBlockerState, GoalCompletionRecord, GoalContinuationPhase, GoalContinuationState,
    GoalResumeCheckpoint, GoalStatus, SessionGoal, SessionPlan, SessionStoreState, TimelineEntry,
    TimelineEntryKind,
};
use magi_core::{
    AccessProfile, DomainError, DomainResult, GoalId, PlanItemStatus, PlanState, SessionId, TaskId,
    TaskStatus, ThreadId, UtcMillis,
};

const BLOCKED_TURN_THRESHOLD: u32 = 3;
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
    let entropy = entropy
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
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
        token_budget: Option<u64>,
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
            .ok_or_else(|| DomainError::InvalidState {
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
            return Err(DomainError::InvalidState {
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
            token_budget,
            tokens_used: 0,
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
            return Err(DomainError::InvalidState {
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
            return Err(DomainError::InvalidState {
                message: "only an active goal can observe a blocker".to_string(),
            });
        }
        if !goal_owned_by_turn(&state.goals[goal_index], turn_id) {
            return Err(DomainError::InvalidState {
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

    pub fn account_goal_token_usage(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        token_delta: u64,
    ) -> DomainResult<SessionGoal> {
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
        if !state.goals[goal_index].status.is_unfinished() {
            return Ok(state.goals[goal_index].clone());
        }
        let goal = &mut state.goals[goal_index];
        goal.tokens_used = goal.tokens_used.saturating_add(token_delta);
        let budget_exhausted = goal
            .token_budget
            .is_some_and(|token_budget| goal.tokens_used >= token_budget)
            && goal.status == GoalStatus::Active;
        if budget_exhausted {
            goal.status = GoalStatus::BudgetLimited;
            goal.control_revision = goal.control_revision.saturating_add(1);
            goal.continuation = GoalContinuationState {
                phase: GoalContinuationPhase::Waiting,
                turn_id: None,
                reason: Some("token_budget_exhausted".to_string()),
            };
        }
        goal.updated_at = now;
        if budget_exhausted
            && let Some(plan) = state.plans.iter_mut().find(|plan| {
                &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id)
            })
        {
            plan.state = PlanState::Paused;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at = now;
        }
        super::sidecar::reconcile_goal_time_used(&mut state);
        Ok(state.goals[goal_index].clone())
    }

    pub fn complete_goal(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
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
        validate_control_revision(&state.goals[goal_index], Some(expected_revision))?;
        if state.goals[goal_index].status != GoalStatus::Active {
            return Err(DomainError::InvalidState {
                message: "only an active goal can be completed".to_string(),
            });
        }
        if !goal_owned_by_turn(&state.goals[goal_index], &turn_id) {
            return Err(DomainError::InvalidState {
                message: "only the goal-owning turn can complete the goal".to_string(),
            });
        }
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_plan_revision)?;
        if let Some(plan) = bound_plan {
            if plan.items.iter().any(|item| {
                matches!(
                    item.status,
                    PlanItemStatus::Pending | PlanItemStatus::InProgress | PlanItemStatus::Blocked
                )
            }) {
                return Err(DomainError::InvalidState {
                    message: "goal plan still contains unfinished or blocked work".to_string(),
                });
            }
            if plan.task_statuses.iter().any(|(task_id, status)| {
                task_id.as_str() != turn_id
                    && matches!(status, TaskStatus::Pending | TaskStatus::Running)
                    && task_is_active_in_session_execution(&state, session_id, task_id)
            }) {
                return Err(DomainError::InvalidState {
                    message: "goal plan still has active bound tasks".to_string(),
                });
            }
        }
        let plan_revision = bound_plan.map(|plan| plan.revision);
        let evidence_refs = normalize_evidence_refs(evidence_refs);
        if bound_plan.is_some() && evidence_refs.is_empty() {
            return Err(DomainError::Validation {
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
            expected_revision,
            expected_plan_revision,
            GoalStatus::Paused,
            None,
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
        new_token_budget: Option<u64>,
        new_access_profile: Option<AccessProfile>,
    ) -> DomainResult<(SessionGoal, Option<SessionPlan>, GoalResumeCheckpoint)> {
        let (goal, plan, checkpoint) = self.transition_goal_with_plan(
            session_id,
            goal_id,
            expected_revision,
            expected_plan_revision,
            GoalStatus::Active,
            new_token_budget,
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
        expected_revision: u64,
        expected_plan_revision: Option<u64>,
        next_status: GoalStatus,
        new_token_budget: Option<u64>,
        new_access_profile: Option<AccessProfile>,
    ) -> DomainResult<(
        SessionGoal,
        Option<SessionPlan>,
        Option<GoalResumeCheckpoint>,
    )> {
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
        let current_status = state.goals[goal_index].status;
        let bound_plan = state
            .plans
            .iter()
            .find(|plan| &plan.session_id == session_id && plan.goal_id.as_ref() == Some(goal_id));
        validate_bound_plan_revision(bound_plan, expected_plan_revision)?;
        let allowed = match next_status {
            GoalStatus::Paused => current_status == GoalStatus::Active,
            GoalStatus::Active => {
                matches!(
                    current_status,
                    GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::UsageLimited
                ) || (current_status == GoalStatus::BudgetLimited
                    && new_token_budget
                        .is_some_and(|budget| budget > state.goals[goal_index].tokens_used))
            }
            _ => false,
        };
        if !allowed {
            return Err(DomainError::InvalidState {
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
            if let Some(new_token_budget) = new_token_budget {
                goal.token_budget = Some(new_token_budget);
            }
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
            return Err(DomainError::InvalidState {
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
                    return Err(DomainError::InvalidState {
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
                    return Err(DomainError::InvalidState {
                        message: "恢复请求后出现新的绑定 plan，拒绝覆盖回滚".to_string(),
                    });
                }
            }
        }
        state.goals[goal_index] = checkpoint.goal_before;
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
        return Err(DomainError::InvalidState {
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
        (None, Some(_)) => Err(DomainError::InvalidState {
            message: "goal plan no longer exists".to_string(),
        }),
        (Some(plan), None) => Err(DomainError::InvalidState {
            message: format!(
                "goal plan revision is required; current revision is {}",
                plan.revision
            ),
        }),
        (Some(plan), Some(expected_revision)) if plan.revision != expected_revision => {
            Err(DomainError::InvalidState {
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

fn task_is_active_in_session_execution(
    state: &SessionStoreState,
    session_id: &SessionId,
    task_id: &TaskId,
) -> bool {
    state
        .execution_sidecar_store
        .runtime_sidecars
        .iter()
        .find(|sidecar| &sidecar.session_id == session_id)
        .and_then(|sidecar| sidecar.active_execution_chain.as_ref())
        .is_some_and(|chain| {
            &chain.root_task_id == task_id
                || chain
                    .active_branch_task_ids
                    .iter()
                    .any(|active_task_id| active_task_id == task_id)
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, SessionExecutionSidecarStoreState,
    };
    use magi_core::MissionId;

    fn create_test_goal(
        store: &SessionStore,
        session_id: &SessionId,
        turn_id: &str,
        objective: &str,
        access_profile: AccessProfile,
        token_budget: Option<u64>,
    ) -> SessionGoal {
        let (_, thread_id) = store.ensure_session_mission(session_id, UtcMillis::now(), || {
            MissionId::new(format!("mission-{session_id}"))
        });
        store
            .create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                objective,
                access_profile,
                token_budget,
            )
            .expect("goal should be created")
    }

    fn set_test_continuation_owner(
        store: &SessionStore,
        session_id: &SessionId,
        goal_id: &GoalId,
        turn_id: &str,
    ) {
        let mut state = store
            .state
            .write()
            .expect("session state write lock should hold");
        let goal = state
            .goals
            .iter_mut()
            .find(|goal| &goal.session_id == session_id && &goal.goal_id == goal_id)
            .expect("goal should exist");
        goal.continuation = GoalContinuationState {
            phase: GoalContinuationPhase::Running,
            turn_id: Some(turn_id.to_string()),
            reason: None,
        };
    }

    #[test]
    fn active_goal_access_profile_change_survives_durable_restore() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-access-profile");
        store
            .create_session(session_id.clone(), "goal access profile")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-access-profile",
            "验证访问模式持久化",
            AccessProfile::ReadOnly,
            None,
        );

        let updated = store
            .set_active_goal_access_profile(&session_id, AccessProfile::FullAccess)
            .expect("active goal access profile should update")
            .expect("active goal should exist");
        assert_eq!(updated.goal_id, goal.goal_id);
        assert_eq!(updated.access_profile, AccessProfile::FullAccess);

        let restored = SessionStore::from_persisted_parts(
            store.durable_state(),
            SessionExecutionSidecarStoreState::default(),
        );
        let restored_goal = restored
            .active_goal(&session_id)
            .expect("active goal should survive durable restore");
        assert_eq!(restored_goal.goal_id, goal.goal_id);
        assert_eq!(restored_goal.access_profile, AccessProfile::FullAccess);
    }

    #[test]
    fn goal_creation_requires_registered_orchestrator_thread() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-thread-owner");
        store
            .create_session(session_id.clone(), "goal thread owner")
            .expect("session should be created");

        let missing_thread_error = store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-unregistered"),
                "turn-goal-thread-owner",
                "验证 thread 归属",
                AccessProfile::Restricted,
                None,
            )
            .expect_err("goal must not exist before orchestrator thread registration");
        assert!(matches!(
            missing_thread_error,
            DomainError::InvalidState { .. }
        ));

        store.ensure_session_mission(&session_id, UtcMillis::now(), || {
            MissionId::new("mission-goal-thread-owner")
        });
        let wrong_thread_error = store
            .create_goal(
                session_id,
                ThreadId::new("thread-worker"),
                "turn-goal-thread-owner",
                "验证 thread 归属",
                AccessProfile::Restricted,
                None,
            )
            .expect_err("goal must belong to the registered orchestrator thread");
        assert!(matches!(wrong_thread_error, DomainError::Validation { .. }));
    }

    #[test]
    fn goal_objective_length_is_bounded_for_create_and_update() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-objective-length");
        store
            .create_session(session_id.clone(), "goal objective length")
            .expect("session should be created");
        let (_, orchestrator_thread_id) =
            store.ensure_session_mission(&session_id, UtcMillis::now(), || {
                MissionId::new("mission-goal-objective-length")
            });
        let oversized = "目".repeat(MAX_GOAL_OBJECTIVE_CHARS + 1);
        assert!(
            store
                .create_goal(
                    session_id.clone(),
                    orchestrator_thread_id.clone(),
                    "turn-goal-objective-length",
                    &oversized,
                    AccessProfile::Restricted,
                    None,
                )
                .is_err()
        );
        let goal = store
            .create_goal(
                session_id.clone(),
                orchestrator_thread_id,
                "turn-goal-objective-length",
                "有效目标",
                AccessProfile::Restricted,
                None,
            )
            .expect("bounded goal should create");
        assert!(
            store
                .update_goal_objective_if_revision(
                    &session_id,
                    &goal.goal_id,
                    oversized,
                    Some(goal.control_revision),
                )
                .is_err()
        );
        assert_eq!(
            store
                .current_goal(&session_id)
                .expect("goal should remain")
                .objective,
            "有效目标"
        );
    }

    #[test]
    fn clearing_latest_goal_does_not_reveal_older_completed_goal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-history");
        store
            .create_session(session_id.clone(), "goal history")
            .expect("session should be created");
        let first = create_test_goal(
            &store,
            &session_id,
            "turn-goal-history-1",
            "第一个目标",
            AccessProfile::Restricted,
            None,
        );
        store
            .complete_goal(
                &session_id,
                &first.goal_id,
                first.control_revision,
                None,
                "turn-goal-history-1",
                "第一个目标已完成",
                Vec::new(),
            )
            .expect("first goal should complete");
        let second = create_test_goal(
            &store,
            &session_id,
            "turn-goal-history-2",
            "第二个目标",
            AccessProfile::Restricted,
            None,
        );
        store
            .clear_goal_with_plan(&session_id, &second.goal_id, second.control_revision, None)
            .expect("second goal should clear");

        assert!(store.current_visible_goal(&session_id).is_none());
        assert!(store.goals_for_session(&session_id).is_empty());
    }

    #[test]
    fn completed_goal_is_terminal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-terminal");
        store
            .create_session(session_id.clone(), "goal terminal")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-terminal",
            "终态目标",
            AccessProfile::Restricted,
            None,
        );
        let completed = store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                None,
                "turn-goal-terminal",
                "终态目标已完成",
                Vec::new(),
            )
            .expect("goal should complete");

        let error = store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                completed.control_revision,
                None,
                None,
                None,
            )
            .expect_err("completed goal must not reopen");
        assert!(matches!(error, DomainError::InvalidState { .. }));
    }

    #[test]
    fn blocker_requires_three_consecutive_goal_turns() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-failure-streak");
        store
            .create_session(session_id.clone(), "goal failure streak")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-failure-streak",
            "完成目标",
            AccessProfile::Restricted,
            None,
        );

        let mut current = goal;
        for expected in 1..=2 {
            let turn_id = format!("turn-{expected}");
            set_test_continuation_owner(&store, &session_id, &current.goal_id, &turn_id);
            let updated = store
                .observe_goal_blocker(
                    &session_id,
                    &current.goal_id,
                    current.control_revision,
                    None,
                    &turn_id,
                    "missing-input",
                    "等待用户输入",
                )
                .expect("blocker should be observed");
            assert_eq!(updated.status, GoalStatus::Active);
            assert_eq!(
                updated
                    .blocker
                    .as_ref()
                    .expect("blocker should exist")
                    .consecutive_turns,
                expected
            );
            current = updated;
        }
        set_test_continuation_owner(&store, &session_id, &current.goal_id, "turn-3");
        let blocked = store
            .observe_goal_blocker(
                &session_id,
                &current.goal_id,
                current.control_revision,
                None,
                "turn-3",
                "missing-input",
                "等待用户输入",
            )
            .expect("third observation should block");
        assert_eq!(blocked.status, GoalStatus::Blocked);
        assert_eq!(
            blocked
                .blocker
                .expect("blocker should exist")
                .consecutive_turns,
            3
        );
    }

    #[test]
    fn stale_turn_cannot_complete_or_block_goal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-stale-terminal-update");
        store
            .create_session(session_id.clone(), "stale terminal update")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-owner",
            "只允许归属 Turn 更新终态",
            AccessProfile::Restricted,
            None,
        );

        assert!(
            store
                .observe_goal_blocker(
                    &session_id,
                    &goal.goal_id,
                    goal.control_revision,
                    None,
                    "turn-stale",
                    "stale-blocker",
                    "旧 Turn 不应阻塞目标",
                )
                .is_err()
        );
        assert!(
            store
                .complete_goal(
                    &session_id,
                    &goal.goal_id,
                    goal.control_revision,
                    None,
                    "turn-stale",
                    "旧 Turn 不应完成目标",
                    Vec::new(),
                )
                .is_err()
        );
        let unchanged = store.current_goal(&session_id).expect("goal should remain");
        assert_eq!(unchanged.status, GoalStatus::Active);
        assert!(unchanged.blocker.is_none());
        assert!(unchanged.completion.is_none());

        set_test_continuation_owner(
            &store,
            &session_id,
            &goal.goal_id,
            "turn-current-continuation",
        );
        let completed = store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                None,
                "turn-current-continuation",
                "当前续跑 Turn 完成目标",
                Vec::new(),
            )
            .expect("current continuation should own terminal update");
        assert_eq!(completed.status, GoalStatus::Complete);
    }

    #[test]
    fn runtime_failure_blocks_immediately() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-runtime-failure");
        store
            .create_session(session_id.clone(), "goal runtime failure")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-runtime-error",
            "完成目标",
            AccessProfile::Restricted,
            None,
        );
        let blocked = store
            .stop_goal_for_runtime_failure(
                &session_id,
                &goal.goal_id,
                None,
                "turn-runtime-error",
                "provider failed",
            )
            .expect("runtime failure should stop goal");
        assert_eq!(blocked.status, GoalStatus::Blocked);
        assert_eq!(
            blocked
                .blocker
                .expect("runtime blocker should exist")
                .blocker_key,
            "runtime_error"
        );
    }

    #[test]
    fn stale_turn_cannot_charge_replacement_goal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-stale-accounting");
        store
            .create_session(session_id.clone(), "stale accounting")
            .expect("session should be created");
        let first = create_test_goal(
            &store,
            &session_id,
            "turn-stale-accounting-1",
            "第一个目标",
            AccessProfile::Restricted,
            None,
        );
        store
            .complete_goal(
                &session_id,
                &first.goal_id,
                first.control_revision,
                None,
                "turn-stale-accounting-1",
                "第一个目标已完成",
                Vec::new(),
            )
            .expect("first goal should complete");
        let second = create_test_goal(
            &store,
            &session_id,
            "turn-stale-accounting-2",
            "第二个目标",
            AccessProfile::Restricted,
            None,
        );

        assert!(
            store
                .account_goal_token_usage(&session_id, &first.goal_id, 100)
                .is_err()
        );
        let current = store
            .current_goal(&session_id)
            .expect("current goal should exist");
        assert_eq!(current.goal_id, second.goal_id);
        assert_eq!(current.tokens_used, 0);
        assert_eq!(current.time_used_seconds, 0);

        let after_stale_failure = store
            .stop_goal_for_runtime_failure(
                &session_id,
                &second.goal_id,
                None,
                "turn-stale-accounting-1",
                "旧 Turn 延迟失败",
            )
            .expect("stale failure should be ignored");
        assert_eq!(after_stale_failure.status, GoalStatus::Active);
        assert!(after_stale_failure.blocker.is_none());
    }

    #[test]
    fn goal_completion_requires_bound_plan_to_finish() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-completion-gate");
        store
            .create_session(session_id.clone(), "completion gate")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-completion-gate",
            "完成计划",
            AccessProfile::Restricted,
            None,
        );
        let plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-goal-completion-gate"),
            session_id: session_id.clone(),
            goal_id: Some(goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Active,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("finish"),
                "完成实现",
                PlanItemStatus::InProgress,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        store
            .upsert_plan(&session_id, plan.clone(), Some(0))
            .expect("plan should create");
        assert!(
            store
                .complete_goal(
                    &session_id,
                    &goal.goal_id,
                    goal.control_revision,
                    Some(1),
                    "turn-goal-completion-gate",
                    "尚未完成",
                    Vec::new(),
                )
                .is_err()
        );

        let mut completed_plan = plan;
        completed_plan.items[0].status = PlanItemStatus::Completed;
        completed_plan.state = PlanState::Completed;
        store
            .upsert_plan(&session_id, completed_plan, Some(1))
            .expect("plan should complete");
        let completed = store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                Some(2),
                "turn-goal-completion-gate",
                "计划和验证均已完成",
                vec!["test:goal-completion-gate".to_string()],
            )
            .expect("finished plan should allow completion");
        assert_eq!(completed.status, GoalStatus::Complete);
        assert_eq!(
            completed
                .completion
                .expect("completion record should exist")
                .plan_revision,
            Some(2)
        );
    }

    #[test]
    fn goal_completion_uses_active_execution_chain_as_task_authority() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-owning-turn-completion");
        let owning_turn_id = "turn-goal-owning-turn-completion";
        store
            .create_session(session_id.clone(), "owning turn completion")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            owning_turn_id,
            "完成当前 Goal Turn",
            AccessProfile::Restricted,
            None,
        );
        let own_task_id = magi_core::TaskId::new(owning_turn_id);
        let other_task_id = magi_core::TaskId::new("task-other-active-binding");
        let item_id = magi_core::PlanItemId::new("completed");
        let plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-goal-owning-turn-completion"),
            session_id: session_id.clone(),
            goal_id: Some(goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Completed,
            items: vec![magi_core::PlanItem::new(
                item_id.clone(),
                "完成目标",
                PlanItemStatus::Completed,
            )],
            task_bindings: std::collections::HashMap::from([
                (own_task_id.clone(), item_id.clone()),
                (other_task_id.clone(), item_id),
            ]),
            task_statuses: std::collections::HashMap::from([
                (own_task_id.clone(), TaskStatus::Running),
                (other_task_id.clone(), TaskStatus::Running),
            ]),
            updated_at: UtcMillis::now(),
        };
        let plan = store
            .upsert_plan(&session_id, plan.clone(), Some(0))
            .expect("completed plan should create");
        let active_chain = ActiveExecutionChain {
            session_id: session_id.clone(),
            mission_id: MissionId::new(format!("mission-{session_id}")),
            root_task_id: own_task_id,
            execution_chain_ref: "chain-goal-owning-turn-completion".to_string(),
            workspace_id: None,
            active_branch_task_ids: vec![other_task_id.clone()],
            active_worker_bindings: Vec::new(),
            branches: Vec::new(),
            recovery_ref: None,
            dispatch_context: ActiveExecutionDispatchContext {
                accepted_at: UtcMillis::now(),
                entry_id: "timeline-goal-owning-turn-completion".to_string(),
                trimmed_text: None,
                skill_name: None,
            },
            current_turn: None,
        };
        store
            .upsert_active_execution_chain(session_id.clone(), active_chain.clone())
            .expect("active execution chain should persist");

        assert!(
            store
                .complete_goal(
                    &session_id,
                    &goal.goal_id,
                    goal.control_revision,
                    Some(plan.revision),
                    owning_turn_id,
                    "仍有其他任务运行",
                    vec!["test:other-task-running".to_string()],
                )
                .is_err(),
            "an unrelated active task must still block Goal completion"
        );

        store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    active_branch_task_ids: Vec::new(),
                    ..active_chain
                },
            )
            .expect("stale task should leave the active execution chain");
        let completed = store
            .complete_goal(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                Some(plan.revision),
                owning_turn_id,
                "当前 Goal Turn 已完成计划和证据验证",
                vec!["test:owning-turn-completion".to_string()],
            )
            .expect("stale task projection must not block Goal completion");
        assert_eq!(completed.status, GoalStatus::Complete);
    }

    #[test]
    fn goal_terminal_updates_require_current_bound_plan_revision() {
        let store = SessionStore::new();
        let completion_session_id = SessionId::new("session-goal-completion-plan-cas");
        store
            .create_session(completion_session_id.clone(), "completion plan cas")
            .expect("session should be created");
        let completion_goal = create_test_goal(
            &store,
            &completion_session_id,
            "turn-goal-completion-plan-cas",
            "验证完成状态的计划版本",
            AccessProfile::Restricted,
            None,
        );
        let completion_plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-goal-completion-plan-cas"),
            session_id: completion_session_id.clone(),
            goal_id: Some(completion_goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Completed,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("completed"),
                "完成验证",
                PlanItemStatus::Completed,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        store
            .upsert_plan(&completion_session_id, completion_plan, Some(0))
            .expect("completion plan should create");

        for expected_plan_revision in [None, Some(0)] {
            assert!(
                store
                    .complete_goal(
                        &completion_session_id,
                        &completion_goal.goal_id,
                        completion_goal.control_revision,
                        expected_plan_revision,
                        "turn-goal-completion-plan-cas",
                        "旧计划快照不能完成目标",
                        vec!["test:goal-completion-plan-cas".to_string()],
                    )
                    .is_err()
            );
        }
        assert_eq!(
            store
                .current_goal(&completion_session_id)
                .expect("goal should remain active")
                .status,
            GoalStatus::Active
        );
        let completed = store
            .complete_goal(
                &completion_session_id,
                &completion_goal.goal_id,
                completion_goal.control_revision,
                Some(1),
                "turn-goal-completion-plan-cas",
                "当前计划快照允许完成目标",
                vec!["test:goal-completion-plan-cas".to_string()],
            )
            .expect("current plan revision should allow completion");
        assert_eq!(completed.status, GoalStatus::Complete);

        let blocker_session_id = SessionId::new("session-goal-blocker-plan-cas");
        store
            .create_session(blocker_session_id.clone(), "blocker plan cas")
            .expect("session should be created");
        let blocker_goal = create_test_goal(
            &store,
            &blocker_session_id,
            "turn-goal-blocker-plan-cas",
            "验证阻塞状态的计划版本",
            AccessProfile::Restricted,
            None,
        );
        let blocker_plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-goal-blocker-plan-cas"),
            session_id: blocker_session_id.clone(),
            goal_id: Some(blocker_goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Active,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("blocked"),
                "等待外部条件",
                PlanItemStatus::InProgress,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        store
            .upsert_plan(&blocker_session_id, blocker_plan.clone(), Some(0))
            .expect("blocker plan should create");
        let mut current = blocker_goal;
        for turn_id in ["turn-blocker-plan-cas-1", "turn-blocker-plan-cas-2"] {
            set_test_continuation_owner(&store, &blocker_session_id, &current.goal_id, turn_id);
            current = store
                .observe_goal_blocker(
                    &blocker_session_id,
                    &current.goal_id,
                    current.control_revision,
                    Some(1),
                    turn_id,
                    "external-state",
                    "等待外部状态变化",
                )
                .expect("current plan revision should observe blocker");
        }
        let mut changed_plan = blocker_plan;
        changed_plan.items[0].title = "外部条件仍未满足".to_string();
        let changed_plan = store
            .upsert_plan(&blocker_session_id, changed_plan, Some(1))
            .expect("concurrent plan update should succeed");
        set_test_continuation_owner(
            &store,
            &blocker_session_id,
            &current.goal_id,
            "turn-blocker-plan-cas-3",
        );
        assert!(
            store
                .observe_goal_blocker(
                    &blocker_session_id,
                    &current.goal_id,
                    current.control_revision,
                    Some(1),
                    "turn-blocker-plan-cas-3",
                    "external-state",
                    "等待外部状态变化",
                )
                .is_err()
        );
        let blocked = store
            .observe_goal_blocker(
                &blocker_session_id,
                &current.goal_id,
                current.control_revision,
                Some(changed_plan.revision),
                "turn-blocker-plan-cas-3",
                "external-state",
                "等待外部状态变化",
            )
            .expect("current plan revision should permit terminal blocker update");
        assert_eq!(blocked.status, GoalStatus::Blocked);
        let paused_plan = store
            .plan(&blocker_session_id)
            .expect("bound plan should remain");
        assert_eq!(paused_plan.state, PlanState::Paused);
        assert_eq!(paused_plan.revision, changed_plan.revision + 1);
    }

    #[test]
    fn budget_limited_goal_requires_increased_budget_to_resume() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-budget-resume");
        store
            .create_session(session_id.clone(), "budget resume")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-budget-resume",
            "完成预算恢复",
            AccessProfile::Restricted,
            Some(10),
        );
        store
            .upsert_plan(
                &session_id,
                SessionPlan {
                    plan_id: magi_core::PlanId::new("plan-goal-budget-resume"),
                    session_id: session_id.clone(),
                    goal_id: Some(goal.goal_id.clone()),
                    revision: 1,
                    language: "zh-CN".to_string(),
                    state: PlanState::Active,
                    items: vec![magi_core::PlanItem::new(
                        magi_core::PlanItemId::new("budget-step"),
                        "继续执行",
                        PlanItemStatus::InProgress,
                    )],
                    task_bindings: std::collections::HashMap::new(),
                    task_statuses: std::collections::HashMap::new(),
                    updated_at: UtcMillis::now(),
                },
                Some(0),
            )
            .expect("bound plan should create");
        let limited = store
            .account_goal_token_usage(&session_id, &goal.goal_id, 10)
            .expect("usage should be accounted");
        assert_eq!(limited.status, GoalStatus::BudgetLimited);
        let limited_plan = store.plan(&session_id).expect("bound plan should remain");
        assert_eq!(limited_plan.state, PlanState::Paused);
        assert_eq!(limited_plan.revision, 2);
        assert!(
            store
                .resume_goal_with_plan(
                    &session_id,
                    &goal.goal_id,
                    limited.control_revision,
                    Some(limited_plan.revision),
                    Some(10),
                    None,
                )
                .is_err()
        );
        let (resumed, _, _) = store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                limited.control_revision,
                Some(limited_plan.revision),
                Some(11),
                None,
            )
            .expect("larger budget should resume");
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(resumed.token_budget, Some(11));
    }

    #[test]
    fn usage_limited_goal_can_resume_after_system_limit_clears() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-usage-resume");
        store
            .create_session(session_id.clone(), "usage resume")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-usage-resume",
            "完成用量恢复",
            AccessProfile::Restricted,
            None,
        );
        {
            let mut state = store
                .state
                .write()
                .expect("session state write lock should hold");
            let stored = state
                .goals
                .iter_mut()
                .find(|stored| stored.goal_id == goal.goal_id)
                .expect("goal should exist");
            stored.status = GoalStatus::UsageLimited;
            stored.continuation = GoalContinuationState {
                phase: GoalContinuationPhase::Waiting,
                turn_id: None,
                reason: Some("system_usage_limit".to_string()),
            };
        }

        let limited = store.current_goal(&session_id).expect("goal should remain");
        let (resumed, _, _) = store
            .resume_goal_with_plan(
                &session_id,
                &limited.goal_id,
                limited.control_revision,
                None,
                None,
                None,
            )
            .expect("usage limited goal should resume");
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(resumed.continuation.phase, GoalContinuationPhase::Waiting);
    }

    #[test]
    fn goal_plan_controls_reject_stale_plan_revision_atomically() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-plan-control-revision");
        store
            .create_session(session_id.clone(), "goal plan control revision")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-plan-control-revision",
            "验证 Goal 与 Plan 控制版本",
            AccessProfile::Restricted,
            None,
        );
        store
            .upsert_plan(
                &session_id,
                SessionPlan {
                    plan_id: magi_core::PlanId::new("plan-goal-plan-control-revision"),
                    session_id: session_id.clone(),
                    goal_id: Some(goal.goal_id.clone()),
                    revision: 1,
                    language: "zh-CN".to_string(),
                    state: PlanState::Active,
                    items: vec![magi_core::PlanItem::new(
                        magi_core::PlanItemId::new("execute"),
                        "执行目标",
                        PlanItemStatus::InProgress,
                    )],
                    task_bindings: std::collections::HashMap::new(),
                    task_statuses: std::collections::HashMap::new(),
                    updated_at: UtcMillis::now(),
                },
                Some(0),
            )
            .expect("plan should create");

        assert!(
            store
                .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(0),)
                .is_err()
        );
        assert_eq!(
            store
                .current_goal(&session_id)
                .expect("goal should remain")
                .status,
            GoalStatus::Active
        );
        assert_eq!(
            store.plan(&session_id).expect("plan should remain").state,
            PlanState::Active
        );

        let (paused, paused_plan) = store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
            .expect("matching revisions should pause goal and plan");
        let paused_plan = paused_plan.expect("bound plan should pause");
        assert_eq!(paused.status, GoalStatus::Paused);
        assert_eq!(paused_plan.state, PlanState::Paused);

        assert!(
            store
                .resume_goal_with_plan(
                    &session_id,
                    &goal.goal_id,
                    paused.control_revision,
                    Some(1),
                    None,
                    None,
                )
                .is_err()
        );
        assert_eq!(
            store
                .current_goal(&session_id)
                .expect("goal should remain")
                .status,
            GoalStatus::Paused
        );
        assert!(
            store
                .clear_goal_with_plan(&session_id, &goal.goal_id, paused.control_revision, Some(1),)
                .is_err()
        );
        assert!(store.current_goal(&session_id).is_some());

        let (resumed, resumed_plan, _) = store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                paused.control_revision,
                Some(paused_plan.revision),
                None,
                None,
            )
            .expect("matching revisions should resume goal and plan");
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(
            resumed_plan.expect("bound plan should resume").state,
            PlanState::Active
        );
    }

    #[test]
    fn paused_goal_rejects_late_plan_progress_atomically() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-paused-goal-late-plan-progress");
        store
            .create_session(session_id.clone(), "paused goal late plan progress")
            .expect("session should be created");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-paused-goal-late-plan-progress",
            "验证暂停后的迟到计划更新",
            AccessProfile::Restricted,
            None,
        );
        let active_plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-paused-goal-late-plan-progress"),
            session_id: session_id.clone(),
            goal_id: Some(goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Active,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("execute"),
                "执行目标",
                PlanItemStatus::InProgress,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        let active_plan = store
            .upsert_plan_for_goal_progress(
                &session_id,
                active_plan,
                Some(0),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .expect("active goal should accept matching plan progress");
        let (paused_goal, paused_plan) = store
            .pause_goal_with_plan(
                &session_id,
                &goal.goal_id,
                goal.control_revision,
                Some(active_plan.revision),
            )
            .expect("goal and plan should pause atomically");
        let paused_plan = paused_plan.expect("bound plan should pause");

        let mut late_active_plan = paused_plan.clone();
        late_active_plan.state = PlanState::Active;

        assert!(
            store
                .upsert_plan_for_goal_progress(
                    &session_id,
                    late_active_plan.clone(),
                    Some(active_plan.revision),
                    Some(goal.goal_id.clone()),
                    Some(goal.control_revision),
                )
                .is_err(),
            "stale goal and plan revisions must be rejected"
        );
        assert!(
            store
                .upsert_plan_for_goal_progress(
                    &session_id,
                    late_active_plan.clone(),
                    Some(paused_plan.revision),
                    Some(goal.goal_id.clone()),
                    Some(goal.control_revision),
                )
                .is_err(),
            "current plan revision cannot bypass a stale goal revision"
        );
        assert!(
            store
                .upsert_plan_for_goal_progress(
                    &session_id,
                    late_active_plan.clone(),
                    Some(paused_plan.revision),
                    Some(goal.goal_id.clone()),
                    Some(paused_goal.control_revision),
                )
                .is_err(),
            "a current control revision cannot advance a paused goal"
        );
        assert!(
            store
                .upsert_plan(&session_id, late_active_plan, Some(paused_plan.revision))
                .is_err(),
            "internal writes cannot reactivate a non-active goal plan"
        );

        assert_eq!(
            store.current_goal(&session_id).expect("goal should remain"),
            paused_goal
        );
        assert_eq!(
            store.plan(&session_id).expect("plan should remain"),
            paused_plan
        );

        store
            .clear_goal_with_plan(
                &session_id,
                &paused_goal.goal_id,
                paused_goal.control_revision,
                Some(paused_plan.revision),
            )
            .expect("paused goal should clear with matching revisions");
        let replacement_goal = create_test_goal(
            &store,
            &session_id,
            "turn-replacement-goal",
            "替换后的新目标",
            AccessProfile::Restricted,
            None,
        );
        assert_eq!(replacement_goal.control_revision, goal.control_revision);
        let late_initial_plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-late-initial-after-goal-replacement"),
            session_id: session_id.clone(),
            goal_id: Some(goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Active,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("late-execute"),
                "迟到执行",
                PlanItemStatus::InProgress,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        assert!(
            store
                .upsert_plan_for_goal_progress(
                    &session_id,
                    late_initial_plan,
                    Some(0),
                    Some(goal.goal_id),
                    Some(goal.control_revision),
                )
                .is_err(),
            "matching revisions must not let an old Goal identity bind a replacement Goal"
        );
        assert!(store.plan(&session_id).is_none());
        assert_eq!(
            store
                .current_goal(&session_id)
                .expect("replacement goal should remain"),
            replacement_goal
        );
    }

    #[test]
    fn diversion_pauses_goal_and_bound_plan_atomically() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-diversion");
        store
            .create_session(session_id.clone(), "goal diversion")
            .expect("session should create");
        let goal = create_test_goal(
            &store,
            &session_id,
            "task-goal-diversion-owner",
            "验证任务切换原子暂停",
            AccessProfile::Restricted,
            None,
        );
        let plan = SessionPlan {
            plan_id: magi_core::PlanId::new("plan-goal-diversion"),
            session_id: session_id.clone(),
            goal_id: Some(goal.goal_id.clone()),
            revision: 1,
            language: "zh-CN".to_string(),
            state: PlanState::Active,
            items: vec![magi_core::PlanItem::new(
                magi_core::PlanItemId::new("execute"),
                "执行目标",
                PlanItemStatus::InProgress,
            )],
            task_bindings: std::collections::HashMap::new(),
            task_statuses: std::collections::HashMap::new(),
            updated_at: UtcMillis::now(),
        };
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                plan,
                Some(0),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .expect("plan should create");

        let (paused_goal, paused_plan) = store
            .pause_active_goal_for_diversion(&session_id)
            .expect("diversion should succeed")
            .expect("active goal should pause");

        assert_eq!(paused_goal.status, GoalStatus::Paused);
        assert_eq!(paused_goal.continuation, GoalContinuationState::default());
        assert_eq!(
            paused_plan.expect("bound plan should pause").state,
            PlanState::Paused
        );
        assert!(
            store
                .pause_active_goal_for_diversion(&session_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn execution_owner_and_resume_rollback_are_revision_safe() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-owner-rollback");
        store
            .create_session(session_id.clone(), "goal owner rollback")
            .expect("session should create");
        let goal = create_test_goal(
            &store,
            &session_id,
            "task-goal-owner",
            "验证执行归属和恢复回滚",
            AccessProfile::Restricted,
            None,
        );
        store
            .upsert_plan_for_goal_progress(
                &session_id,
                SessionPlan {
                    plan_id: magi_core::PlanId::new("plan-goal-owner"),
                    session_id: session_id.clone(),
                    goal_id: Some(goal.goal_id.clone()),
                    revision: 1,
                    language: "zh-CN".to_string(),
                    state: PlanState::Active,
                    items: vec![magi_core::PlanItem::new(
                        magi_core::PlanItemId::new("execute"),
                        "执行目标",
                        PlanItemStatus::InProgress,
                    )],
                    task_bindings: std::collections::HashMap::new(),
                    task_statuses: std::collections::HashMap::new(),
                    updated_at: UtcMillis::now(),
                },
                Some(0),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .expect("goal plan should create");
        assert_eq!(
            store
                .active_goal_for_execution_owner(&session_id, "task-goal-owner")
                .map(|owned| owned.goal_id),
            Some(goal.goal_id.clone())
        );
        assert!(
            store
                .active_plan_for_execution_owner(&session_id, "task-goal-owner")
                .is_some()
        );
        assert!(
            store
                .active_goal_for_execution_owner(&session_id, "task-unrelated")
                .is_none()
        );
        assert!(
            store
                .active_plan_for_execution_owner(&session_id, "task-unrelated")
                .is_none()
        );
        let paused = store
            .pause_goal_with_plan(&session_id, &goal.goal_id, goal.control_revision, Some(1))
            .expect("goal should pause")
            .0;
        let (resumed, resumed_plan, checkpoint) = store
            .resume_goal_with_plan(
                &session_id,
                &goal.goal_id,
                paused.control_revision,
                Some(2),
                None,
                Some(AccessProfile::FullAccess),
            )
            .expect("resume request should succeed");
        assert_eq!(resumed.status, GoalStatus::Active);
        assert_eq!(resumed.continuation.phase, GoalContinuationPhase::Waiting);
        assert_eq!(resumed.access_profile, AccessProfile::FullAccess);
        assert_eq!(
            resumed_plan.expect("plan should resume").state,
            PlanState::Active
        );
        assert_eq!(store.waiting_goal_session_ids(), vec![session_id.clone()]);
        assert!(
            store
                .active_goal_for_execution_owner(&session_id, "task-goal-owner")
                .is_none(),
            "waiting 阶段不能继续归属于旧 Goal Turn"
        );
        assert!(
            store
                .active_plan_for_execution_owner(&session_id, "task-goal-owner")
                .is_none(),
            "waiting 阶段没有执行拥有者"
        );

        store
            .rollback_goal_resume(checkpoint)
            .expect("unchanged resume request should roll back");
        assert_eq!(
            store.current_goal(&session_id).expect("goal should remain"),
            paused
        );
        assert!(store.waiting_goal_session_ids().is_empty());
    }

    #[test]
    fn current_goal_turn_interruption_pauses_goal_without_execution_chain() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-interrupt-without-chain");
        store
            .create_session(session_id.clone(), "goal interrupt without chain")
            .expect("session should create");
        let goal = create_test_goal(
            &store,
            &session_id,
            "turn-goal-interrupt-owner",
            "验证直接 Goal Turn 中断",
            AccessProfile::Restricted,
            None,
        );
        store
            .upsert_current_turn(
                session_id.clone(),
                crate::models::ActiveExecutionTurn {
                    turn_id: "turn-goal-interrupt-owner".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(1),
                    status: "running".to_string(),
                    completed_at: None,
                    user_message: Some("推进目标".to_string()),
                    items: Vec::new(),
                },
            )
            .expect("goal turn should persist");

        store
            .interrupt_current_turn_by_user(&session_id)
            .expect("interruption should succeed");
        let paused = store
            .current_goal(&session_id)
            .expect("owned goal should remain");
        assert_eq!(paused.goal_id, goal.goal_id);
        assert_eq!(paused.status, GoalStatus::Paused);
        assert_eq!(paused.continuation, GoalContinuationState::default());
    }
}

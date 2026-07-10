use super::{SessionStore, unique_timeline_entry_id};
use crate::models::{GoalStatus, SessionGoal, TimelineEntry, TimelineEntryKind};
use magi_core::{DomainError, DomainResult, GoalId, SessionId, ThreadId, UtcMillis};

fn normalize_objective(objective: impl Into<String>) -> DomainResult<String> {
    let objective = objective.into();
    let trimmed = objective.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation {
            message: "goal objective cannot be empty".to_string(),
        });
    }
    Ok(trimmed.to_string())
}

impl SessionStore {
    pub fn create_goal(
        &self,
        session_id: SessionId,
        thread_id: ThreadId,
        objective: impl Into<String>,
        token_budget: Option<u64>,
    ) -> DomainResult<SessionGoal> {
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
            goal_id: GoalId::new(format!("goal-{}-{}", session_id, now.0)),
            session_id: session_id.clone(),
            thread_id,
            objective: objective.clone(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            consecutive_failure_turns: 0,
            created_at: now,
            updated_at: now,
        };
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
        Ok(goal)
    }

    pub fn current_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id)
            .max_by(|left, right| {
                left.updated_at
                    .0
                    .cmp(&right.updated_at.0)
                    .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
            })
            .cloned()
    }

    pub fn current_unfinished_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id && goal.status.is_unfinished())
            .max_by(|left, right| {
                left.updated_at
                    .0
                    .cmp(&right.updated_at.0)
                    .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
            })
            .cloned()
    }

    pub fn current_visible_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id && goal.status != GoalStatus::Cleared)
            .max_by(|left, right| {
                left.updated_at
                    .0
                    .cmp(&right.updated_at.0)
                    .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
            })
            .cloned()
    }

    pub fn active_goal(&self, session_id: &SessionId) -> Option<SessionGoal> {
        let state = self.state.read().expect("session state read lock poisoned");
        state
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id && goal.status == GoalStatus::Active)
            .max_by(|left, right| {
                left.updated_at
                    .0
                    .cmp(&right.updated_at.0)
                    .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
            })
            .cloned()
    }

    pub fn update_goal_objective(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        objective: impl Into<String>,
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
        if !goal.status.is_unfinished() {
            return Err(DomainError::InvalidState {
                message: "terminal goal objective cannot be edited".to_string(),
            });
        }
        goal.objective = objective;
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn update_goal_status(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        status: GoalStatus,
    ) -> DomainResult<SessionGoal> {
        if !matches!(status, GoalStatus::Complete | GoalStatus::Blocked) {
            return Err(DomainError::Validation {
                message: "model-driven goal update only supports complete or blocked".to_string(),
            });
        }
        self.set_goal_status(session_id, goal_id, status)
    }

    pub fn set_goal_status(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        status: GoalStatus,
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
        if goal.status == GoalStatus::Cleared && goal.status != status {
            return Err(DomainError::InvalidState {
                message: "terminal goal status cannot transition".to_string(),
            });
        }
        goal.status = status;
        if status == GoalStatus::Active {
            goal.consecutive_failure_turns = 0;
        }
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn record_goal_turn_success(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
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
        if goal.status == GoalStatus::Active && goal.consecutive_failure_turns != 0 {
            goal.consecutive_failure_turns = 0;
            goal.updated_at = now;
        }
        Ok(goal.clone())
    }

    pub fn record_goal_turn_failure(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
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
        if goal.status != GoalStatus::Active {
            return Ok(goal.clone());
        }
        goal.consecutive_failure_turns = goal.consecutive_failure_turns.saturating_add(1);
        if goal.consecutive_failure_turns >= 3 {
            goal.status = GoalStatus::Blocked;
        }
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn account_goal_progress(
        &self,
        session_id: &SessionId,
        goal_id: &GoalId,
        token_delta: u64,
        elapsed_seconds_delta: u64,
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
        if !goal.status.is_unfinished() {
            return Ok(goal.clone());
        }
        goal.tokens_used = goal.tokens_used.saturating_add(token_delta);
        goal.time_used_seconds = goal.time_used_seconds.saturating_add(elapsed_seconds_delta);
        if goal
            .token_budget
            .is_some_and(|token_budget| goal.tokens_used >= token_budget)
        {
            goal.status = GoalStatus::BudgetLimited;
        }
        goal.updated_at = now;
        Ok(goal.clone())
    }

    pub fn goals_for_session(&self, session_id: &SessionId) -> Vec<SessionGoal> {
        let mut goals = self
            .state
            .read()
            .expect("session state read lock poisoned")
            .goals
            .iter()
            .filter(|goal| &goal.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        goals.sort_by(|left, right| {
            left.created_at
                .0
                .cmp(&right.created_at.0)
                .then_with(|| left.goal_id.as_str().cmp(right.goal_id.as_str()))
        });
        goals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_blocks_only_after_three_consecutive_failed_turns() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-failure-streak");
        store
            .create_session(session_id.clone(), "goal failure streak")
            .expect("session should be created");
        let goal = store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-failure-streak"),
                "完成目标",
                None,
            )
            .expect("goal should be created");

        for expected in 1..=2 {
            let current = store
                .record_goal_turn_failure(&session_id, &goal.goal_id)
                .expect("failure should be recorded");
            assert_eq!(current.status, GoalStatus::Active);
            assert_eq!(current.consecutive_failure_turns, expected);
        }

        store
            .record_goal_turn_success(&session_id, &goal.goal_id)
            .expect("success should reset streak");
        assert_eq!(
            store
                .active_goal(&session_id)
                .expect("goal should remain active")
                .consecutive_failure_turns,
            0
        );

        for _ in 0..3 {
            store
                .record_goal_turn_failure(&session_id, &goal.goal_id)
                .expect("failure should be recorded");
        }
        let blocked = store.current_goal(&session_id).expect("goal should exist");
        assert_eq!(blocked.status, GoalStatus::Blocked);
        assert_eq!(blocked.consecutive_failure_turns, 3);
    }
}

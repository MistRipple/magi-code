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

fn unique_goal_id(goals: &[SessionGoal], base: String) -> GoalId {
    if !goals.iter().any(|goal| goal.goal_id.as_str() == base) {
        return GoalId::new(base);
    }
    let mut suffix = 1_u64;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !goals.iter().any(|goal| goal.goal_id.as_str() == candidate) {
            return GoalId::new(candidate);
        }
        suffix = suffix.saturating_add(1);
    }
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
            goal_id: unique_goal_id(&state.goals, format!("goal-{}-{}", session_id, now.0)),
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
        self.current_goal(session_id)
            .filter(|goal| goal.status != GoalStatus::Cleared)
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
        let invalid_terminal_transition = match goal.status {
            GoalStatus::Cleared => status != GoalStatus::Cleared,
            GoalStatus::Complete => !matches!(status, GoalStatus::Complete | GoalStatus::Cleared),
            _ => false,
        };
        if invalid_terminal_transition {
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
    fn clearing_latest_goal_does_not_reveal_older_completed_goal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-history");
        store
            .create_session(session_id.clone(), "goal history")
            .expect("session should be created");
        let first = store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-history"),
                "第一个目标",
                None,
            )
            .expect("first goal should be created");
        store
            .set_goal_status(&session_id, &first.goal_id, GoalStatus::Complete)
            .expect("first goal should complete");
        let second = store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-history"),
                "第二个目标",
                None,
            )
            .expect("second goal should be created");
        store
            .set_goal_status(&session_id, &second.goal_id, GoalStatus::Cleared)
            .expect("second goal should clear");

        assert!(store.current_visible_goal(&session_id).is_none());
    }

    #[test]
    fn completed_goal_is_terminal() {
        let store = SessionStore::new();
        let session_id = SessionId::new("session-goal-terminal");
        store
            .create_session(session_id.clone(), "goal terminal")
            .expect("session should be created");
        let goal = store
            .create_goal(
                session_id.clone(),
                ThreadId::new("thread-goal-terminal"),
                "终态目标",
                None,
            )
            .expect("goal should be created");
        store
            .set_goal_status(&session_id, &goal.goal_id, GoalStatus::Complete)
            .expect("goal should complete");

        let error = store
            .set_goal_status(&session_id, &goal.goal_id, GoalStatus::Active)
            .expect_err("completed goal must not reopen");
        assert!(matches!(error, DomainError::InvalidState { .. }));
    }

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

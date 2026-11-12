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
fn finished_turn_replay_records_are_bounded_per_session() {
    let coordinator = SessionTurnCoordinator::new();
    let session = SessionId::new("session-recent-bounded");
    for index in 0..(MAX_RECENT_FINISHED_TURNS + 10) {
        let attempt = match coordinator
            .accept(
                &session,
                admission(
                    &format!("turn-{index}"),
                    &format!("request-{index}"),
                    &format!("fp-{index}"),
                ),
            )
            .expect("turn should be accepted")
        {
            CoordinatorAdmission::Accepted(attempt) => attempt,
            CoordinatorAdmission::Replay(_) => panic!("new request must be accepted"),
        };
        coordinator
            .finish(&session, &attempt, CoordinatorTurnStatus::Completed)
            .expect("turn should finish");
    }
    let recent_len = coordinator
        .state
        .lock()
        .expect("state")
        .sessions
        .get(&session)
        .map(|state| state.recent.len())
        .unwrap_or_default();
    assert_eq!(
        recent_len, MAX_RECENT_FINISHED_TURNS,
        "回放记录必须有界保留"
    );
    let last = MAX_RECENT_FINISHED_TURNS + 9;
    assert!(matches!(
        coordinator.accept(
            &session,
            admission(
                &format!("turn-replay-{last}"),
                &format!("request-{last}"),
                &format!("fp-{last}"),
            ),
        ),
        Ok(CoordinatorAdmission::Replay(_))
    ));
}

#[test]
fn task_turn_accepts_steer_only_while_input_boundary_is_open() {
    let coordinator = SessionTurnCoordinator::new();
    let session = SessionId::new("session-task-steer");
    let attempt = match coordinator
        .execute_command(
            &session,
            TurnCommand::Start(TurnAdmission {
                turn_id: "turn-task-1".to_string(),
                request_id: "request-task-1".to_string(),
                request_fingerprint: "fp-task-1".to_string(),
                profile: ExecutionProfile::Task,
            }),
        )
        .expect("Start command should be accepted")
    {
        CoordinatorCommandResult::Admission(CoordinatorAdmission::Accepted(attempt)) => attempt,
        other => panic!("unexpected Start result: {other:?}"),
    };

    assert!(!coordinator.active_turn_accepts_steer(&session));
    assert_eq!(
        coordinator
            .execute_command(
                &session,
                TurnCommand::Steer {
                    attempt: attempt.clone(),
                    request_id: "steer-task-1".to_string(),
                },
            )
            .unwrap_err(),
        CoordinatorError::SteerUnsupported {
            profile: ExecutionProfile::Task
        },
        "未开放输入边界时不得接纳引导"
    );
    coordinator
        .begin_session_turn_input(session.clone(), attempt.turn_id.clone())
        .unwrap();
    assert!(coordinator.active_turn_accepts_steer(&session));
    assert!(
        coordinator
            .execute_command(
                &session,
                TurnCommand::Steer {
                    attempt: attempt.clone(),
                    request_id: "steer-task-2".to_string(),
                }
            )
            .is_ok()
    );
    assert_eq!(
        coordinator.take_session_turn_steers_or_close(&session, &attempt.turn_id),
        SessionTurnInputBoundary::Closed
    );
    assert!(!coordinator.active_turn_accepts_steer(&session));
    assert!(matches!(
        coordinator.execute_command(
            &session,
            TurnCommand::Steer {
                attempt,
                request_id: "steer-task-late".to_string(),
            }
        ),
        Err(CoordinatorError::SteerUnsupported { .. })
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
    coordinator
        .begin_session_turn_input(session.clone(), attempt.turn_id.clone())
        .unwrap();
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
        model_steps: Vec::new(),
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
        model_steps: Vec::new(),
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
    assert!(SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none());

    turn.metadata.remove("requestFingerprint");
    turn.metadata.insert(
        "attemptId".to_string(),
        serde_json::Value::String("attempt-missing-fingerprint".to_string()),
    );
    assert!(SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none());
}

#[test]
fn recover_command_rejects_unknown_execution_profile() {
    let session = SessionId::new("session-recover-unknown-profile");
    let turn = CanonicalTurn {
        model_steps: Vec::new(),
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
    assert!(SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn).is_none());
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
        model_steps: Vec::new(),
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
        model_steps: Vec::new(),
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
        model_steps: Vec::new(),
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
        let command = SessionTurnCoordinator::recover_command_for_canonical_turn(&session, &turn)
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
        model_steps: Vec::new(),
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
        model_steps: Vec::new(),
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
        model_steps: Vec::new(),
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
                images: Vec::new(),
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

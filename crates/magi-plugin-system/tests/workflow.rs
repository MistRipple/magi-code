use magi_plugin_system::workflow::{WorkflowAction, WorkflowDecision, WorkflowInput};
use serde_json::json;

fn input() -> WorkflowInput {
    WorkflowInput {
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        stage: "plan".into(),
        user_input: "继续".into(),
        model_result: None,
        tool_result: None,
        context_summary: json!({"used": 12}),
        config: json!({}),
    }
}

#[test]
fn action_must_be_bound_to_current_attempt_and_decision_is_typed() {
    let input = input();
    let decision = WorkflowDecision {
        checkpoint_version: 1,
        stage: "execute".into(),
        action: WorkflowAction::ToolCall {
            action_id: "attempt-1".into(),
            tool: "files.read".into(),
            input: json!({"path":"README.md"}),
        },
    };
    decision.validate("run-1", &input).unwrap();
    let round_trip: WorkflowDecision =
        serde_json::from_value(serde_json::to_value(&decision).unwrap()).unwrap();
    assert_eq!(round_trip, decision);
}

#[test]
fn forged_run_or_action_id_is_rejected_without_fallback_core() {
    let input = input();
    let mut decision = WorkflowDecision {
        checkpoint_version: 1,
        stage: "execute".into(),
        action: WorkflowAction::Complete {
            action_id: "other-attempt".into(),
            summary: "done".into(),
        },
    };
    assert!(decision.validate("run-1", &input).is_err());
    decision.action = WorkflowAction::Complete {
        action_id: "attempt-1".into(),
        summary: "done".into(),
    };
    assert!(decision.validate("other-run", &input).is_err());
}

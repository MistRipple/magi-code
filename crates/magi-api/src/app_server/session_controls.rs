//! 多端控制适配器，只转换 wire DTO；交互和 Goal 复用现有应用服务。
use super::{api_error_to_protocol, error_response};
use crate::{errors::ApiError, scope_binding::resolve_existing_session_scope, state::ApiState};
use magi_app_server_protocol::{
    self as protocol, AppServerRequestMethod as Method, ErrorObject, RequestId, ServerResponse,
};
use magi_core::SessionId;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ErrorObject> {
    serde_json::from_value(value)
        .map_err(|error| ErrorObject::new(protocol::ERROR_INVALID_PARAMS, error.to_string()))
}
fn encode<T: DeserializeOwned + Serialize>(value: impl Serialize) -> Result<Value, ErrorObject> {
    let value = serde_json::to_value(value)
        .map_err(|error| ErrorObject::new(protocol::ERROR_INTERNAL, error.to_string()))?;
    let wire: T = serde_json::from_value(value).map_err(|error| {
        ErrorObject::new(
            protocol::ERROR_INTERNAL,
            format!("控制结果不符合协议: {error}"),
        )
    })?;
    serde_json::to_value(wire)
        .map_err(|error| ErrorObject::new(protocol::ERROR_INTERNAL, error.to_string()))
}
fn session_scope(
    state: &ApiState,
    session_id: &str,
    workspace_id: Option<&str>,
    workspace_path: Option<&str>,
) -> Result<SessionId, ErrorObject> {
    if session_id.trim().is_empty() {
        return Err(ErrorObject::new(
            protocol::ERROR_INVALID_PARAMS,
            "sessionId 不能为空",
        ));
    }
    let session = SessionId::new(session_id.trim());
    resolve_existing_session_scope(state, &session, workspace_id, workspace_path)
        .map_err(api_error_to_protocol)?;
    Ok(session)
}
fn application<T>(result: Result<T, ApiError>) -> Result<T, ErrorObject> {
    result.map_err(api_error_to_protocol)
}

pub(super) async fn dispatch(
    state: &ApiState,
    request_id: RequestId,
    method: Method,
    params: Value,
) -> ServerResponse {
    match handle(state, method, params).await {
        Ok(value) => protocol::response(request_id, value),
        Err(error) => error_response(request_id, error),
    }
}

async fn handle(state: &ApiState, method: Method, params: Value) -> Result<Value, ErrorObject> {
    use protocol::generated as wire;
    match method {
        Method::AgentContinue => {
            let params: wire::AgentContinueParams = decode(params)?;
            encode::<wire::AgentContinueResult>(application(
                crate::agent_service::continue_agent_run(state, params).await,
            )?)
        }
        Method::AgentTransfer => {
            let params: wire::AgentTransferParams = decode(params)?;
            encode::<wire::AgentTransferResult>(application(
                crate::agent_service::transfer_agent_run(state, params).await,
            )?)
        }
        Method::SessionQueueRead => {
            let params: wire::SessionInteractionScope = decode(params)?;
            encode::<wire::SessionQueueSnapshot>(application(
                crate::input_service::get_session_turn_queue(state.clone(), params).await,
            )?)
        }
        Method::SessionQueueUpdate => {
            let params: wire::SessionQueueUpdateParams = decode(params)?;
            encode::<wire::SessionQueueSnapshot>(application(
                crate::input_service::update_session_turn_queue_item(state.clone(), params).await,
            )?)
        }
        Method::SessionQueueRemove | Method::SessionQueueRetry => {
            let params: wire::SessionQueueItemParams = decode(params)?;
            require_queue_revision(params.expected_revision)?;
            let result = if method == Method::SessionQueueRemove {
                crate::input_service::remove_session_turn_queue_item(state.clone(), params).await
            } else {
                crate::input_service::retry_session_turn_queue_item(state.clone(), params).await
            };
            encode::<wire::SessionQueueSnapshot>(application(result)?)
        }
        Method::SessionQueueGuide => {
            let params: wire::SessionQueueGuideParams = decode(params)?;
            require_queue_revision(params.expected_revision)?;
            encode::<wire::SessionQueueSnapshot>(application(
                crate::input_service::guide_session_turn_queue_item(state.clone(), params).await,
            )?)
        }
        Method::SessionInteractions => {
            let params: wire::SessionInteractionScope = decode(params)?;
            let session = session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let pending_approvals = application(crate::interaction_service::pending_approvals(
                state, &session,
            ))?;
            let pending_questions = state
                .session_store
                .pending_user_questions(&session)
                .map_err(|error| {
                    api_error_to_protocol(ApiError::internal_assembly("读取提问失败", error))
                })?;
            encode::<wire::SessionInteractionsResult>(
                json!({"sessionId":session, "pendingApprovals":pending_approvals,"pendingQuestions":pending_questions}),
            )
        }
        Method::SessionApprovalResolve => {
            let params: wire::ResolveToolApprovalParams = decode(params)?;
            let session = session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let decision = decode(
                serde_json::to_value(&params.decision).expect("approval decision serializes"),
            )?;
            encode::<wire::ResolveToolApprovalResult>(application(
                crate::interaction_service::resolve_approval(
                    state,
                    &session,
                    params.approval_id.trim(),
                    decision,
                ),
            )?)
        }
        Method::SessionQuestionResolve => {
            let params: wire::ResolveUserQuestionParams = decode(params)?;
            let session = session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let response = decode(
                serde_json::to_value(params.response).expect("question response serializes"),
            )?;
            encode::<wire::ResolveUserQuestionResult>(application(
                crate::interaction_service::resolve_question(
                    state,
                    &session,
                    params.question_id.trim(),
                    response,
                ),
            )?)
        }
        Method::GoalRead => {
            let params: wire::GoalReadParams = decode(params)?;
            session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let query = decode(serde_json::to_value(params).expect("goal params serialize"))?;
            encode::<wire::GoalSnapshot>(application(crate::goal_service::snapshot_to_wire(
                application(crate::goal_service::get_current_goal(state, query).await)?,
            ))?)
        }
        Method::GoalUpdate => {
            let params: wire::GoalUpdateParams = decode(params)?;
            session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let query = decode(serde_json::to_value(params).expect("goal params serialize"))?;
            encode::<wire::GoalSnapshot>(application(crate::goal_service::snapshot_to_wire(
                application(crate::goal_service::update_current_goal(state, query).await)?,
            ))?)
        }
        Method::GoalPause | Method::GoalResume | Method::GoalClear => {
            let params: wire::GoalActionParams = decode(params)?;
            session_scope(
                state,
                &params.session_id,
                params.workspace_id.as_deref(),
                params.workspace_path.as_deref(),
            )?;
            let query = decode(serde_json::to_value(params).expect("goal params serialize"))?;
            let result = match method {
                Method::GoalPause => crate::goal_service::pause_current_goal(state, query).await,
                Method::GoalResume => crate::goal_service::resume_current_goal(state, query).await,
                Method::GoalClear => crate::goal_service::clear_current_goal(state, query).await,
                _ => unreachable!(),
            };
            encode::<wire::GoalSnapshot>(application(crate::goal_service::snapshot_to_wire(
                application(result)?,
            ))?)
        }
        _ => Err(ErrorObject::new(
            protocol::ERROR_METHOD_NOT_FOUND,
            "不是会话控制方法",
        )),
    }
}

fn require_queue_revision(revision: Option<u64>) -> Result<(), ErrorObject> {
    if revision.is_some_and(|revision| revision > 0) {
        Ok(())
    } else {
        Err(ErrorObject::new(
            protocol::ERROR_INVALID_PARAMS,
            "队列变更需要当前输入的 expectedRevision",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn state() -> ApiState {
        ApiState::new(
            "controls-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(magi_session_store::SessionStore::new()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        )
    }

    #[tokio::test]
    async fn explicit_scope_and_interaction_identity_are_required_without_mutation() {
        let state = state();
        let session = SessionId::new("controls-session");
        state
            .session_store
            .create_session(session.clone(), "controls")
            .unwrap();
        let original = serde_json::to_value(state.session_store.durable_state()).unwrap();
        for (method, params) in [
            (Method::SessionQueueRead, json!({"sessionId":""})),
            (
                Method::SessionQueueRead,
                json!({"sessionId":session,"workspaceId":"wrong-workspace"}),
            ),
            (
                Method::SessionQueueRemove,
                json!({"sessionId":session,"queueId":"missing"}),
            ),
            (
                Method::SessionQueueRetry,
                json!({"sessionId":session,"queueId":"missing","expectedRevision":0}),
            ),
            (
                Method::SessionQueueGuide,
                json!({"sessionId":session,"queueId":"missing","expectedRevision":1,"expectedTurnId":""}),
            ),
            (Method::SessionInteractions, json!({"sessionId":""})),
            (
                Method::SessionInteractions,
                json!({"sessionId":session,"workspaceId":"wrong-workspace"}),
            ),
            (
                Method::SessionApprovalResolve,
                json!({"sessionId":session,"approvalId":"missing","decision":"allow_once"}),
            ),
            (
                Method::SessionQuestionResolve,
                json!({"sessionId":session,"questionId":"missing","response":{"kind":"skipped"}}),
            ),
            (
                Method::SessionQuestionResolve,
                json!({"sessionId":session,"questionId":"missing","response":{"kind":"skipped","answers":[]}}),
            ),
            (
                Method::GoalRead,
                json!({"sessionId":"", "scope":"personal"}),
            ),
            (
                Method::GoalPause,
                json!({"sessionId":session,"scope":"personal","goalId":"missing","expectedRevision":1,"tokenBudget":100}),
            ),
        ] {
            assert!(handle(&state, method, params).await.is_err(), "{method:?}");
            assert_eq!(
                serde_json::to_value(state.session_store.durable_state()).unwrap(),
                original
            );
        }
        let pending = handle(
            &state,
            Method::SessionInteractions,
            json!({"sessionId":session}),
        )
        .await
        .unwrap();
        assert_eq!(pending["pendingApprovals"], json!([]));
        assert_eq!(pending["pendingQuestions"], json!([]));
        let goal = handle(
            &state,
            Method::GoalRead,
            json!({"sessionId":session,"scope":"personal"}),
        )
        .await
        .unwrap();
        assert!(goal["goal"].is_null());
        assert_eq!(goal["allowedActions"]["canPause"], false);
    }
}

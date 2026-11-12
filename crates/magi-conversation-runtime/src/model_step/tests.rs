use super::*;
use magi_core::{DomainError, DomainResult, Task};
use magi_session_store::{
    ActiveExecutionTurn, CanonicalTurnMutation, SessionAcceptanceRecord, SessionEventWriter,
    SessionInputMutation, TimelineEntryInput, TimelineEntryKind,
};
use std::sync::Arc;

fn store() -> (SessionStore, SessionId, ThreadId) {
    let store = SessionStore::new();
    let session = SessionId::new("session-journal-client");
    store
        .create_session(session.clone(), "journal client")
        .unwrap();
    store
        .accept_current_turn_with_timeline_entry(
            session.clone(),
            TimelineEntryInput::new(
                "timeline",
                TimelineEntryKind::UserMessage,
                "work",
                UtcMillis(1),
            ),
            ActiveExecutionTurn {
                turn_id: "turn-journal-client".into(),
                turn_seq: 1,
                accepted_at: UtcMillis(1),
                status: "running".into(),
                completed_at: None,
                user_message: Some("work".into()),
                items: vec![],
            },
        )
        .unwrap();
    (store, session, ThreadId::new("thread-journal-client"))
}
fn request(text: &str) -> ModelInvocationRequest {
    ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "test".into(),
        prompt: text.into(),
        messages: None,
        tools: None,
        tool_choice: None,
    }
}

fn http_fixture(
    responses: Vec<(&'static str, &'static str)>,
) -> (String, std::thread::JoinHandle<Vec<serde_json::Value>>) {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, body) in responses {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            bodies.push(serde_json::from_slice(&bytes).unwrap());
            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        bodies
    });
    (url, thread)
}

#[test]
fn model_step_notifies_observers_after_commit_without_prompt_content() {
    struct Echo;
    impl ModelBridgeClient for Echo {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>);
    impl crate::extension_hooks::ExtensionHooks for Recorder {
        fn tool_call(
            &self,
            _: &crate::extension_hooks::ToolHookScope<'_>,
            _: &magi_bridge_client::ChatToolCall,
        ) -> crate::extension_hooks::ToolCallDecision {
            unreachable!("模型步骤不会触发工具钩子")
        }
        fn tool_result(
            &self,
            _: &crate::extension_hooks::ToolHookScope<'_>,
            _: &magi_bridge_client::ChatToolCall,
            _: crate::extension_hooks::ToolResultView<'_>,
        ) -> Option<String> {
            unreachable!("模型步骤不会触发工具钩子")
        }
        fn observe(&self, hook: &'static str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((hook, payload));
        }
    }
    let (store, session, thread) = store();
    let recorder = Recorder::default();
    StepModelClient::new(
        &Echo,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "observed-call",
        0,
    )
    .with_extension_hooks(Some(&recorder))
    .invoke(request("机密提示词"))
    .unwrap();
    let events = recorder.0.lock().unwrap();
    assert_eq!(
        events.iter().map(|(hook, _)| *hook).collect::<Vec<_>>(),
        ["model.resolved", "model.request", "model.response"]
    );
    assert!(
        events
            .iter()
            .all(|(_, payload)| payload["stepId"] == "observed-call")
    );
    assert_eq!(events[2].1["ok"], true);
    assert!(
        events
            .iter()
            .all(|(_, payload)| !payload.to_string().contains("机密提示词")),
        "观察载荷不得包含提示词正文"
    );
}

#[test]
fn http_retry_records_actual_wire_status_and_usage_in_the_original_step() {
    let (url, server) = http_fixture(vec![
        (
            "500 Internal Server Error",
            r#"{"error":{"message":"temporarily overloaded"}}"#,
        ),
        (
            "200 OK",
            r#"{"choices":[{"message":{"content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":20}}}"#,
        ),
    ]);
    let client = magi_bridge_client::HttpModelBridgeClient::new(url, None, "test".into());
    let (store, session, thread) = store();
    StepModelClient::new(
        &client,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "http-call",
        0,
    )
    .invoke(request("work"))
    .unwrap();
    let bodies = server.join().unwrap();
    let step = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap()
        .model_steps
        .remove(0);
    assert_eq!(step.attempts.len(), 2);
    for (attempt, body) in step.attempts.iter().zip(bodies) {
        let actual = magi_core::hex_lower(Sha256::digest(body.to_string().as_bytes()));
        assert_eq!(
            attempt.transport.as_ref().unwrap().request_fingerprint,
            actual
        );
    }
    let first = step.attempts[0].transport.as_ref().unwrap();
    assert_eq!(first.http_status, Some(500));
    assert_eq!(
        first.outcome,
        Some(magi_session_store::ModelTransportOutcome::Failed)
    );
    assert!(first.usage.is_none());
    let last = step.attempts[1].transport.as_ref().unwrap();
    assert_eq!(last.http_status, Some(200));
    assert_eq!(last.usage.as_ref().unwrap().cache_read_tokens, Some(20));
    assert_eq!(
        last.outcome,
        Some(magi_session_store::ModelTransportOutcome::Succeeded)
    );
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        restored
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps[0],
        step
    );
}

#[test]
fn empty_sse_attempt_keeps_observed_usage_before_transport_retry() {
    let (url, server) = http_fixture(vec![
        (
            "200 OK",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
        ),
        (
            "200 OK",
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\ndata: [DONE]\n\n",
        ),
    ]);
    let client = magi_bridge_client::HttpModelBridgeClient::new(url, None, "test".into());
    let (store, session, thread) = store();
    StepModelClient::new(
        &client,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "sse-call",
        0,
    )
    .invoke_streaming(request("work"), &|_| {})
    .unwrap();
    assert_eq!(server.join().unwrap().len(), 2);
    let step = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap()
        .model_steps
        .remove(0);
    assert_eq!(step.attempts.len(), 2);
    let first = step.attempts[0].transport.as_ref().unwrap();
    assert_eq!(first.http_status, Some(200));
    assert_eq!(first.usage.as_ref().unwrap().input_tokens, 11);
    assert_eq!(first.usage.as_ref().unwrap().cache_read_tokens, None);
    assert_eq!(
        first.outcome,
        Some(magi_session_store::ModelTransportOutcome::Failed)
    );
    let last = step.attempts[1].transport.as_ref().unwrap();
    assert_eq!(last.usage.as_ref().unwrap().cache_read_tokens, Some(10));
    assert_eq!(first.request_fingerprint, last.request_fingerprint);
    assert_eq!(
        step.usage_tokens,
        Some(34),
        "Step 必须累计失败重试的实际用量"
    );
}

#[test]
fn transport_observation_failure_prevents_sending_the_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = magi_bridge_client::HttpModelBridgeClient::new(
        format!("http://{}", listener.local_addr().unwrap()),
        None,
        "test".into(),
    );
    let error = client
        .invoke_observed(
            request("work"),
            magi_bridge_client::ModelInvocationOptions {
                on_delta: None,
                on_retry: &|_| {},
                is_cancelled: &|| false,
                max_output_tokens: None,
            },
            &|_| Err(journal_error("injected durable write failure")),
        )
        .unwrap_err();
    assert!(error.to_string().contains("injected durable write failure"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn native_stream_failure_settles_observed_usage_without_delivering_a_response() {
    let (url, server) = http_fixture(vec![(
        "200 OK",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":1}}\n\n",
    )]);
    let client = magi_bridge_client::HttpModelBridgeClient::new(url, None, "test".into());
    let (store, session, thread) = store();
    assert!(
        StepModelClient::new(
            &client,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "partial-call",
            0,
        )
        .invoke_streaming(request("work"), &|_| {})
        .is_err()
    );
    assert_eq!(server.join().unwrap().len(), 1);
    let step = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap()
        .model_steps
        .remove(0);
    assert_eq!(step.status, ModelStepStatus::OutcomeUnknown);
    assert_eq!(step.usage_tokens, Some(12));
    assert!(step.response_fingerprint.is_none());
    step.validate_snapshot().unwrap();
}

#[test]
fn cache_affinity_uses_lineage_across_steps_and_isolates_worker_branch() {
    struct Capture(std::sync::Mutex<Vec<String>>);
    impl ModelBridgeClient for Capture {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.0
                .lock()
                .unwrap()
                .push(request.prompt_cache_key.expect("session key"));
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let (store, session, thread) = store();
    let capture = Capture(std::sync::Mutex::new(vec![]));
    for (revision, call) in ["first", "next-turn", "after-restart"].iter().enumerate() {
        let mut client =
            StepModelClient::new(&capture, &store, &session, None, None, &thread, call, 0);
        client.binding = Some(ModelStepBinding {
            usage_context: None,
            model: Some("same-model".into()),
            provider: Some("same-provider".into()),
            binding_revision: revision as u32,
            settings_revision: revision as u64,
        });
        client.invoke(request(call)).unwrap();
    }
    let worker_thread = ThreadId::new("worker-thread");
    let worker_task = TaskId::new("worker-task");
    StepModelClient::new(
        &capture,
        &store,
        &session,
        None,
        Some(&worker_task),
        &worker_thread,
        "worker",
        0,
    )
    .with_prompt_branch(crate::prompt_lineage::PromptBranchScope::Worker {
        task_id: worker_task.as_str().to_string(),
    })
    .invoke(request("work"))
    .unwrap();
    let other_session = SessionId::new("other-session");
    StepModelClient::new(
        &capture,
        &store,
        &other_session,
        None,
        None,
        &thread,
        "other",
        0,
    )
    .invoke(request("other"))
    .unwrap();
    let keys = capture.0.lock().unwrap();
    assert!(keys[..3].iter().all(|key| key == &keys[0]));
    assert_ne!(keys[3], keys[0]);
    assert_eq!(keys[0].len(), 64);
    assert_ne!(keys[0], keys[4]);
}

#[test]
fn compaction_generation_rotates_cache_epoch_but_history_append_does_not() {
    let make_request = |texts: &[&str]| ModelInvocationRequest {
        messages: Some(
            texts
                .iter()
                .map(|text| magi_bridge_client::ChatMessage {
                    input_request_id: None,
                    context_origin: Default::default(),
                    role: "user".into(),
                    content: Some((*text).into()),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                    provider_context: vec![],
                })
                .collect(),
        ),
        ..request("")
    };
    let before = PromptFrame::from_request(&make_request(&["first"]));
    let appended = PromptFrame::from_request(&make_request(&["first", "second"]));
    let scope = crate::prompt_lineage::ProviderCacheScope::default();
    assert_eq!(
        before.cache_epoch(&scope, 0, None),
        appended.cache_epoch(&scope, 0, None)
    );
    assert_ne!(
        before.cache_epoch(&scope, 0, None),
        appended.cache_epoch(&scope, 1, None)
    );
}

#[test]
fn history_rewrites_are_attributed_to_the_declared_projection() {
    use crate::prompt_lineage::CacheInvalidationReason;
    use magi_session_store::HistoryProjection;
    struct Echo;
    impl ModelBridgeClient for Echo {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let (store, session, thread) = store();
    let message = |role: &str, text: &str| magi_bridge_client::ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: role.into(),
        content: Some(text.into()),
        images: vec![],
        tool_calls: vec![],
        tool_call_id: None,
        provider_context: vec![],
    };
    let invoke = |first: &str, projection: HistoryProjection, call: &str| {
        let client = StepModelClient::new(
            &Echo,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            call,
            0,
        )
        .with_history_projection(projection);
        client
            .invoke(ModelInvocationRequest {
                messages: Some(vec![
                    message("user", first),
                    message("assistant", "reply"),
                    message("user", "next"),
                ]),
                ..request("")
            })
            .unwrap();
    };
    let none = HistoryProjection::default();
    invoke("a", none, "initial");
    invoke(
        "b",
        HistoryProjection {
            demoted_tool_results: 2,
            stale_file_facts: 0,
            ..Default::default()
        },
        "demoted",
    );
    invoke(
        "c",
        HistoryProjection {
            demoted_tool_results: 2,
            stale_file_facts: 1,
            ..Default::default()
        },
        "stale",
    );
    invoke(
        "d",
        HistoryProjection {
            demoted_tool_results: 2,
            stale_file_facts: 1,
            ..Default::default()
        },
        "undeclared",
    );
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    let reasons = turn
        .model_steps
        .iter()
        .map(|step| {
            step.prompt_frame
                .as_ref()
                .unwrap()
                .cache_binding
                .as_ref()
                .unwrap()
                .invalidation_reason
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![
            Some(CacheInvalidationReason::Initial),
            Some(CacheInvalidationReason::ToolHistoryDemoted),
            Some(CacheInvalidationReason::FileFactsRefreshed),
            Some(CacheInvalidationReason::HistoryRewritten),
        ]
    );
}

#[test]
fn extension_rewrites_are_attributed_added_changed_and_revoked_and_survive_restart() {
    use crate::prompt_lineage::CacheInvalidationReason;
    use magi_session_store::{ExtensionRewriteRecord, HistoryProjection};
    struct Echo;
    impl ModelBridgeClient for Echo {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let message = |role: &str, text: &str| magi_bridge_client::ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: role.into(),
        content: Some(text.into()),
        images: vec![],
        tool_calls: vec![],
        tool_call_id: None,
        provider_context: vec![],
    };
    let record = |digest: &str| ExtensionRewriteRecord {
        plugin_id: "acme.summary".into(),
        contribution_id: "summarize".into(),
        hook: "context.assemble".into(),
        from: 0,
        to: 1,
        original_digest: digest.into(),
        replacement_digest: format!("replacement-{digest}"),
    };
    let projection = |records: Vec<ExtensionRewriteRecord>| HistoryProjection {
        extension_rewrites: records,
        ..Default::default()
    };
    let (store, session, thread) = store();
    let invoke = |texts: &[&str], projection: HistoryProjection, call: &str| {
        StepModelClient::new(
            &Echo,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            call,
            0,
        )
        .with_history_projection(projection)
        .invoke(ModelInvocationRequest {
            messages: Some(
                texts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| message(if i % 2 == 0 { "user" } else { "assistant" }, t))
                    .collect(),
            ),
            ..request("")
        })
        .unwrap();
    };
    invoke(&["原文", "reply"], projection(vec![]), "initial");
    // 扩展改写生效：首条被摘要替换。
    invoke(
        &["摘要A", "reply"],
        projection(vec![record("a")]),
        "applied",
    );
    // 同一改写下追加新消息：继续追加，不是新的失效。
    invoke(
        &["摘要A", "reply", "next"],
        projection(vec![record("a")]),
        "appended",
    );
    // 改写内容变化（扩展换了摘要）。
    invoke(
        &["摘要B", "reply", "next"],
        projection(vec![record("b")]),
        "changed",
    );
    // 扩展被停用：改写撤销，历史回到原文，仍然是被声明的变化。
    invoke(&["原文", "reply", "next"], projection(vec![]), "revoked");
    // 没有任何声明的内容变化才是未声明的前缀破坏。
    invoke(
        &["悄悄改了", "reply", "next"],
        projection(vec![]),
        "undeclared",
    );
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    let reasons = turn
        .model_steps
        .iter()
        .map(|step| {
            step.prompt_frame
                .as_ref()
                .unwrap()
                .cache_binding
                .as_ref()
                .unwrap()
                .invalidation_reason
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![
            Some(CacheInvalidationReason::Initial),
            Some(CacheInvalidationReason::ExtensionRewrite),
            None,
            Some(CacheInvalidationReason::ExtensionRewrite),
            Some(CacheInvalidationReason::ExtensionRewrite),
            Some(CacheInvalidationReason::HistoryRewritten),
        ]
    );
    // 记录随 Step 事实持久化，重启后从同一事实读回逐项相同。
    let persisted = turn.model_steps[1]
        .prompt_frame
        .as_ref()
        .unwrap()
        .history_projection
        .clone();
    let restored: magi_session_store::ModelPromptFrame = serde_json::from_value(
        serde_json::to_value(turn.model_steps[1].prompt_frame.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(restored.history_projection, persisted);
    assert_eq!(
        restored.history_projection.unwrap().extension_rewrites,
        vec![record("a")]
    );
}

#[test]
fn recorded_lineage_detects_history_rewrites_and_survives_restart() {
    use crate::prompt_lineage::{CacheInvalidationReason, ProviderCacheScope};
    struct Capture(std::sync::Mutex<Vec<(String, String)>>);
    impl ModelBridgeClient for Capture {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }
        fn cache_identity(&self) -> Option<magi_bridge_client::ModelCacheIdentity> {
            Some(magi_bridge_client::ModelCacheIdentity {
                protocol: "openai_responses".into(),
                endpoint: "https://relay.example/v1/responses".into(),
                model: "actual-model".into(),
                reasoning_profile: "{}".into(),
            })
        }
        fn invoke(
            &self,
            request: ModelInvocationRequest,
        ) -> Result<ModelResponse, BridgeClientError> {
            self.0.lock().unwrap().push((
                request.prompt_cache_key.unwrap(),
                request.cache_affinity_key.unwrap(),
            ));
            Ok(ModelResponse::completed("ok"))
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let capture = Capture(std::sync::Mutex::new(Vec::new()));
    let (store, session, thread) = store();
    let make_request =
        |texts: &[&str]| ModelInvocationRequest {
            messages: Some(
                std::iter::once(magi_bridge_client::ChatMessage {
                    input_request_id: None,
                    context_origin: magi_bridge_client::ChatMessageOrigin::StaticPolicy,
                    role: "developer".into(),
                    content: Some("stable policy".into()),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                    provider_context: vec![],
                })
                .chain(texts.iter().enumerate().map(|(index, text)| {
                    magi_bridge_client::ChatMessage {
                        input_request_id: None,
                        context_origin: Default::default(),
                        role: if index == 0 { "user" } else { "assistant" }.into(),
                        content: Some((*text).into()),
                        images: vec![],
                        tool_calls: vec![],
                        tool_call_id: None,
                        provider_context: if index == 0 {
                            vec![]
                        } else {
                            vec![magi_bridge_client::ModelProviderContext {
                                provider: "openai".into(),
                                kind: "response_output_item".into(),
                                data: serde_json::json!({
                                    "type": "reasoning", "id": format!("rs_{index}"),
                                    "encrypted_content": format!("signature-{text}"), "summary": [],
                                }),
                            }]
                        },
                    }
                }))
                .collect(),
            ),
            ..request("")
        };
    let invoke = |store: &SessionStore, texts: &[&str], call: &str| {
        let mut client = StepModelClient::new(
            &capture,
            store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            call,
            0,
        );
        // 设置快照和路由别名不是真实 transport 身份。
        client.binding = Some(ModelStepBinding {
            usage_context: None,
            model: Some(call.into()),
            provider: Some(call.into()),
            binding_revision: call.len() as u32,
            settings_revision: call.len() as u64,
        });
        client.invoke(make_request(texts)).unwrap();
    };
    invoke(&store, &["first"], "first");
    invoke(&store, &["first", "second"], "appended");
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    invoke(&store, &["first", "second", "third"], "after-restart");
    invoke(&store, &["rewritten", "second", "third"], "rewrite");
    invoke(
        &store,
        &["rewritten", "second", "third", "fourth"],
        "append-after-rewrite",
    );
    invoke(
        &store,
        &["rewritten", "second", "third"],
        "truncate-to-rewrite-boundary",
    );
    invoke(
        &store,
        &["rewritten", "second", "third", "fourth"],
        "append-after-truncation",
    );
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    let steps = &turn.model_steps;
    let bindings = steps
        .iter()
        .map(|step| {
            step.prompt_frame
                .as_ref()
                .unwrap()
                .cache_binding
                .as_ref()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        bindings[0].invalidation_reason,
        Some(CacheInvalidationReason::Initial)
    );
    assert_eq!(bindings[1].invalidation_reason, None);
    assert_eq!(bindings[2].invalidation_reason, None);
    assert_eq!(
        bindings[3].invalidation_reason,
        Some(CacheInvalidationReason::HistoryRewritten)
    );
    assert_eq!(bindings[4].invalidation_reason, None);
    assert_eq!(
        bindings[5].invalidation_reason,
        Some(CacheInvalidationReason::HistoryRewritten)
    );
    assert_eq!(bindings[6].invalidation_reason, None);
    let captured = capture.0.lock().unwrap();
    let keys = captured
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    assert!(
        captured
            .iter()
            .all(|(_, affinity)| affinity == &bindings[0].lineage_id)
    );
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2]);
    assert_ne!(keys[2], keys[3]);
    assert_eq!(keys[3], keys[4]);
    assert_ne!(keys[4], keys[5]);
    assert_eq!(keys[5], keys[6]);
    assert_eq!(bindings[4].cache_key, keys[4]);

    // 对同一持久前驱分类真实环境变化；不把每一次调用都写成 initial。
    let frame =
        PromptFrame::from_request(&make_request(&["rewritten", "second", "third", "fourth"]));
    let scope = ProviderCacheScope::from_client(&capture).unwrap();
    let lineage = || {
        crate::prompt_lineage::PromptLineage::for_session(&session)
            .with_provider_scope(scope.clone())
    };
    let previous = &steps[4];
    for (scope, expected) in [
        (
            ProviderCacheScope {
                endpoint_fingerprint: Some("different".into()),
                ..scope.clone()
            },
            CacheInvalidationReason::ProviderChanged,
        ),
        (
            ProviderCacheScope {
                resolved_model: Some("different".into()),
                ..scope.clone()
            },
            CacheInvalidationReason::ModelChanged,
        ),
        (
            ProviderCacheScope {
                reasoning_profile: Some("different".into()),
                ..scope.clone()
            },
            CacheInvalidationReason::ReasoningChanged,
        ),
    ] {
        let (_, binding) = frame.bind_lineage(
            lineage().with_provider_scope(scope),
            0,
            None,
            Some(previous),
        );
        assert_eq!(binding.invalidation_reason, Some(expected));
        assert_ne!(binding.cache_key, bindings[4].cache_key);
    }
    let (_, binding) = frame.bind_lineage(lineage(), 1, None, Some(previous));
    assert_eq!(
        binding.invalidation_reason,
        Some(CacheInvalidationReason::Compaction)
    );
    assert_ne!(binding.cache_key, bindings[4].cache_key);

    for (mut changed, reason) in [
        (frame.clone(), CacheInvalidationReason::RendererChanged),
        (frame.clone(), CacheInvalidationReason::SkillChanged),
        (frame.clone(), CacheInvalidationReason::ToolSurfaceChanged),
        (frame.clone(), CacheInvalidationReason::StablePrefixChanged),
    ] {
        match reason {
            CacheInvalidationReason::RendererChanged => changed.ir_version += 1,
            CacheInvalidationReason::SkillChanged => {
                changed.skill_hash = "a".repeat(64);
                changed.stable_prefix_hash = "b".repeat(64);
            }
            CacheInvalidationReason::ToolSurfaceChanged => {
                changed.tool_surface_hash = "c".repeat(64)
            }
            CacheInvalidationReason::StablePrefixChanged => {
                changed.stable_prefix_hash = "d".repeat(64)
            }
            _ => unreachable!(),
        }
        let (_, binding) = changed.bind_lineage(lineage(), 0, None, Some(previous));
        assert_eq!(binding.invalidation_reason, Some(reason));
        assert_ne!(binding.cache_key, bindings[4].cache_key);
    }
    let mut volatile = frame.clone();
    volatile.volatile_tail_hash = "e".repeat(64);
    let (_, continued) = volatile.bind_lineage(lineage(), 0, None, Some(previous));
    assert_eq!(continued.invalidation_reason, None);
    assert_eq!(continued.cache_key, bindings[4].cache_key);

    // 首个 RoundContext 后的所有消息都属于本次发送视图，不能把恢复
    // 说明或其私有上下文当成旧历史的新增/改写。
    let mut tail_request = make_request(&["rewritten", "second", "third", "fourth"]);
    let mut tail = tail_request.messages.as_ref().unwrap()[2].clone();
    tail.context_origin = magi_bridge_client::ChatMessageOrigin::RoundContext;
    tail_request.messages.as_mut().unwrap().push(tail.clone());
    tail.context_origin = magi_bridge_client::ChatMessageOrigin::Conversation;
    tail.content = Some("transient recovery".into());
    tail_request.messages.as_mut().unwrap().push(tail);
    let tail_frame = PromptFrame::from_request(&tail_request);
    let (_, continued) = tail_frame.bind_lineage(lineage(), 0, None, Some(previous));
    assert_eq!(
        tail_frame.history_message_count,
        frame.history_message_count
    );
    assert_eq!(continued.invalidation_reason, None);
    assert_eq!(continued.cache_key, bindings[4].cache_key);

    // 历史之外的稳定私有上下文仍明确断代，不因上述修复被忽略。
    let mut prefix_request = make_request(&["rewritten", "second", "third", "fourth"]);
    prefix_request.messages.as_mut().unwrap()[0]
        .provider_context
        .push(magi_bridge_client::ModelProviderContext {
            provider: "openai".into(),
            kind: "response_output_item".into(),
            data: serde_json::json!({"type":"reasoning", "encrypted_content":"stable"}),
        });
    let (_, changed) =
        PromptFrame::from_request(&prefix_request).bind_lineage(lineage(), 0, None, Some(previous));
    assert_eq!(
        changed.invalidation_reason,
        Some(CacheInvalidationReason::ProviderContextChanged)
    );
    assert_ne!(changed.cache_key, bindings[4].cache_key);

    let mut execution = magi_bridge_client::ModelExecutionScope {
        working_dir: None,
        policy: crate::agent_spawn_preflight::agent_spawn_child_policy_snapshot(None),
        requires_native_tools: true,
        closed_skill_scope: false,
    };
    let (scoped, binding) = frame.bind_lineage(lineage(), 0, Some(&execution), Some(previous));
    assert_eq!(
        binding.invalidation_reason,
        Some(CacheInvalidationReason::PermissionChanged)
    );
    let mut scoped_previous = previous.clone();
    scoped_previous.cache_epoch = scoped.cache_epoch;
    scoped_previous.prompt_frame.as_mut().unwrap().cache_binding = Some(binding);
    execution.closed_skill_scope = true;
    let (_, binding) = frame.bind_lineage(lineage(), 0, Some(&execution), Some(&scoped_previous));
    assert_eq!(
        binding.invalidation_reason,
        Some(CacheInvalidationReason::SkillChanged)
    );

    let mut rewritten_provider_context = make_request(&["rewritten", "second", "third", "fourth"]);
    rewritten_provider_context.messages.as_mut().unwrap()[1]
        .provider_context
        .push(magi_bridge_client::ModelProviderContext {
            provider: "openai".into(),
            kind: "response_output_item".into(),
            data: serde_json::json!({"signature": "changed"}),
        });
    let (_, binding) = PromptFrame::from_request(&rewritten_provider_context).bind_lineage(
        lineage(),
        0,
        None,
        Some(previous),
    );
    assert_eq!(
        binding.invalidation_reason,
        Some(CacheInvalidationReason::HistoryRewritten)
    );

    // 生成 DTO 和领域快照必须读取同一份事实，并且不包含提示词正文或端点原文。
    let value = serde_json::to_value(previous).unwrap();
    serde_json::from_value::<magi_app_server_protocol::generated::ModelStep>(value.clone())
        .unwrap();
    let encoded = value.to_string();
    assert!(!encoded.contains("https://relay.example"));
    assert!(!encoded.contains("rewritten"));
    assert!(encoded.len() < 5000, "每 Step 的缓存元数据应保持常量大小");
}

#[test]
fn only_original_inputs_in_the_frozen_request_are_consumed_after_restart() {
    use magi_session_store::{SessionInput, SessionInputState};
    let (store, session, thread) = store();
    for id in ["first", "same-text-later"] {
        store
            .commit_input(
                SessionInput {
                    input_id: format!("input-{id}"),
                    session_id: session.clone(),
                    request_id: id.into(),
                    user_message_id: format!("user-{id}"),
                    request_fingerprint: id.into(),
                    accepted_at: UtcMillis(1),
                    revision: 1,
                    state: SessionInputState::Attached,
                    attached_turn_id: Some("turn-journal-client".into()),
                    consumption: None,
                    interruption: None,
                    payload: serde_json::json!({"text":"identical input"}),
                },
                None,
            )
            .unwrap();
    }
    let store = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    let inner = Client {
        calls: AtomicUsize::new(0),
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "frozen",
        0,
    );
    let mut req = request("identical input");
    req.messages = Some(vec![magi_bridge_client::ChatMessage {
        input_request_id: None,
        context_origin: Default::default(),
        role: "user".into(),
        content: Some("identical input".into()),
        images: vec![],
        tool_calls: vec![],
        tool_call_id: None,
        provider_context: vec![],
    }]);
    // 同文摘要没有原始身份，不能确认消费任意一条输入。
    client.invoke(req.clone()).unwrap();
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps[0]
            .input_request_ids
            .is_empty()
    );
    for id in ["first", "same-text-later"] {
        assert_eq!(
            store.input_for_request(id).unwrap().state,
            SessionInputState::Attached
        );
    }
    req.messages.as_mut().unwrap()[0].input_request_id = Some("first".into());
    client.invoke(req.clone()).unwrap();
    assert_eq!(
        store.input_for_request("first").unwrap().state,
        SessionInputState::Consumed
    );
    assert_eq!(
        store.input_for_request("same-text-later").unwrap().state,
        SessionInputState::Attached
    );
    assert_eq!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps[1]
            .input_request_ids,
        ["first"]
    );
    // 可信身份不会泄露到 Provider 的消息协议，也不会从外部 JSON 注入。
    let mut json = serde_json::to_value(&req).unwrap();
    assert!(json["messages"][0].get("input_request_id").is_none());
    json["messages"][0]["input_request_id"] = serde_json::json!("forged");
    json["messages"][0]["inputRequestId"] = serde_json::json!("forged");
    assert!(
        serde_json::from_value::<ModelInvocationRequest>(json)
            .unwrap()
            .messages
            .unwrap()[0]
            .input_request_id
            .is_none()
    );
    let compactor = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "summary",
        0,
    )
    .with_purpose(magi_session_store::ModelStepPurpose::ContextCompaction);
    req.messages.as_mut().unwrap()[0].input_request_id = Some("same-text-later".into());
    compactor.invoke(req).unwrap();
    assert_eq!(
        store.input_for_request("same-text-later").unwrap().state,
        SessionInputState::Attached
    );
}
struct Client {
    calls: AtomicUsize,
}

struct TextExecutor {
    calls: AtomicUsize,
    unexpected_tool: bool,
}

struct UnscopedExecutor;

impl ModelBridgeClient for UnscopedExecutor {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        let mut caps = magi_bridge_client::text_executor_capabilities();
        caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
        caps
    }

    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        panic!("未实现作用域绑定的执行器不得收到请求")
    }

    fn invoke_streaming(
        &self,
        _: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        panic!("未实现作用域绑定的执行器不得收到流式请求")
    }
}

#[test]
fn default_scope_binding_is_not_an_executor_authorization() {
    let (store, session, thread) = store();
    let client = StepModelClient::new(
        &UnscopedExecutor,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "unscoped",
        0,
    )
    .with_execution_scope(external_scope("/first"));
    for error in [
        client.invoke(request("work")).unwrap_err(),
        client
            .invoke_streaming(request("work"), &|_| {})
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("executor_scope_required"));
    }
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps
            .is_empty()
    );
}

#[derive(Clone)]
struct ScopedExecutor {
    bindings: Arc<std::sync::Mutex<Vec<magi_bridge_client::ModelExecutionScope>>>,
    calls: Arc<AtomicUsize>,
    bound: bool,
    reject_binding: bool,
}

impl ModelBridgeClient for ScopedExecutor {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        let mut caps = magi_bridge_client::text_executor_capabilities();
        caps.tools = magi_bridge_client::ExecutorToolMode::ExecutorOwned;
        caps
    }
    fn bind_execution_scope(
        &self,
        scope: magi_bridge_client::ModelExecutionScope,
    ) -> Result<Option<Arc<dyn ModelBridgeClient>>, BridgeClientError> {
        self.bindings.lock().unwrap().push(scope);
        if self.reject_binding {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(-32096),
                message: "executor_policy_unsupported: test policy rejected".into(),
            });
        }
        let mut client = self.clone();
        client.bound = true;
        Ok(Some(Arc::new(client)))
    }
    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        assert!(self.bound, "不能调用未绑定权限的缓存实例");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::completed("external result"))
    }
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

fn external_scope(root: &str) -> magi_bridge_client::ModelExecutionScope {
    magi_bridge_client::ModelExecutionScope {
        working_dir: Some(root.into()),
        policy: crate::agent_spawn_preflight::agent_spawn_child_policy_snapshot(None),
        requires_native_tools: false,
        closed_skill_scope: false,
    }
}

#[test]
fn external_executor_is_rebound_per_invocation_and_scope_changes_the_request_identity() {
    let (store, session, thread) = store();
    let inner = ScopedExecutor {
        bindings: Default::default(),
        calls: Default::default(),
        bound: false,
        reject_binding: false,
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "external",
        0,
    );
    assert!(
        client
            .invoke(request("work"))
            .unwrap_err()
            .to_string()
            .contains("executor_scope_required")
    );
    assert!(inner.bindings.lock().unwrap().is_empty());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    let client = client.with_execution_scope(external_scope("/first"));
    client.invoke(request("work")).unwrap();
    client.invoke_streaming(request("work"), &|_| {}).unwrap();
    let client = client.with_execution_scope(external_scope("/second"));
    client.invoke(request("work")).unwrap();
    let bindings = inner.bindings.lock().unwrap();
    assert_eq!(bindings.len(), 3);
    assert_eq!(bindings[0].working_dir, Some("/first".into()));
    assert_eq!(bindings[2].working_dir, Some("/second".into()));
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    assert_eq!(turn.model_steps.len(), 3);
    assert_eq!(
        turn.model_steps[0].request_fingerprint,
        turn.model_steps[1].request_fingerprint
    );
    assert_ne!(
        turn.model_steps[0].request_fingerprint,
        turn.model_steps[2].request_fingerprint
    );
    assert!(!inner.bound);
}

#[test]
fn external_executor_without_a_canonical_turn_is_rejected_before_send() {
    let (store, session, thread) = store();
    let inner = ScopedExecutor {
        bindings: Default::default(),
        calls: Default::default(),
        bound: false,
        reject_binding: false,
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        None,
        None,
        &thread,
        "untracked-external",
        0,
    )
    .with_execution_scope(external_scope("/workspace"));
    let error = client.invoke(request("inspect")).unwrap_err();
    assert!(error.to_string().contains("executor_step_required"));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps
            .is_empty()
    );
}

#[test]
fn binding_rejection_and_cancellation_do_not_create_step_or_invoke_executor() {
    let (store, session, thread) = store();
    let inner = ScopedExecutor {
        bindings: Default::default(),
        calls: Default::default(),
        bound: false,
        reject_binding: true,
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "denied-external",
        0,
    )
    .with_execution_scope(external_scope("/first"));
    assert!(
        client
            .invoke(request("work"))
            .unwrap_err()
            .to_string()
            .contains("policy_unsupported")
    );
    assert!(
        client
            .invoke_with_cancellation(request("work"), &|| true)
            .is_err()
    );
    assert_eq!(inner.bindings.lock().unwrap().len(), 1);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps
            .is_empty()
    );
}

impl ModelBridgeClient for TextExecutor {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::text_executor_capabilities()
    }
    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut response = ModelResponse::completed("done");
        if self.unexpected_tool {
            response.tool_calls.push(magi_bridge_client::ChatToolCall {
                id: "unexpected".into(),
                kind: "function".into(),
                function: magi_bridge_client::ChatToolFunction {
                    name: "shell_exec".into(),
                    arguments: r#"{"command":"publish"}"#.into(),
                },
            });
        }
        Ok(response)
    }
    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

#[test]
fn executor_contract_rejects_before_send_and_freezes_received_response_before_refusing_tools() {
    let (store, session, thread) = store();
    let inner = TextExecutor {
        calls: AtomicUsize::new(0),
        unexpected_tool: false,
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "caps",
        0,
    );
    let mut unsupported = request("unsupported");
    unsupported.tool_choice = Some(magi_bridge_client::ChatToolChoice::force_function(
        "shell_exec",
    ));
    assert!(
        client
            .invoke(unsupported)
            .unwrap_err()
            .to_string()
            .contains("executor_native_tools_unsupported")
    );
    assert!(
        client
            .invoke_with_cancellation_and_output_limit(request("limited"), 100, &|| false)
            .unwrap_err()
            .to_string()
            .contains("executor_output_limit_unsupported")
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps
            .is_empty()
    );
    client.invoke(request("supported")).unwrap();
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    assert_eq!(
        turn.model_steps[0].executor_capabilities,
        Some(inner.executor_capabilities())
    );
    let reloaded = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(
        reloaded
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps[0]
            .executor_capabilities,
        Some(inner.executor_capabilities())
    );
    let bad = TextExecutor {
        calls: AtomicUsize::new(0),
        unexpected_tool: true,
    };
    let client = StepModelClient::new(
        &bad,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "bad-caps",
        0,
    );
    assert!(
        client
            .invoke(request("bad response"))
            .unwrap_err()
            .to_string()
            .contains("executor_native_tools_unsupported")
    );
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    assert_eq!(
        turn.model_steps[1].status,
        magi_session_store::ModelStepStatus::ResponseReceived
    );
    assert!(turn.model_steps[1].response_fingerprint.is_some());
}

impl ModelBridgeClient for Client {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::completed("done"))
    }
    fn invoke_streaming(
        &self,
        _: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Transport,
            code: Some(-32005),
            message: "stream interrupted".into(),
        })
    }
}
struct RejectWrites;
impl SessionEventWriter for RejectWrites {
    fn append_session_transaction(
        &self,
        _: &SessionId,
        _: &[CanonicalTurnMutation],
        _: &[SessionInputMutation],
        _: Option<(&SessionAcceptanceRecord, Option<&Task>)>,
    ) -> DomainResult<()> {
        Err(DomainError::Persistence {
            message: "injected".into(),
        })
    }
}
#[test]
fn changed_recovery_request_has_its_own_identity_and_unknown_stream_is_preserved() {
    let (store, session, thread) = store();
    let inner = Client {
        calls: AtomicUsize::new(0),
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "call-0",
        0,
    );
    assert!(
        client
            .invoke_streaming(request("original"), &|_| {})
            .is_err()
    );
    client
        .invoke_with_cancellation(request("recover with partial output"), &|| false)
        .unwrap();
    let turn = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap();
    assert_eq!(turn.model_steps.len(), 2);
    assert_eq!(turn.model_steps[0].status, ModelStepStatus::OutcomeUnknown);
    assert_eq!(
        turn.model_steps[1].status,
        ModelStepStatus::ResponseReceived
    );
    assert_ne!(
        turn.model_steps[0].request_fingerprint,
        turn.model_steps[1].request_fingerprint
    );
    assert_ne!(turn.model_steps[0].step_id, turn.model_steps[1].step_id);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}
struct RetryClient<'a> {
    store: &'a SessionStore,
    reject_retry: bool,
}
impl ModelBridgeClient for RetryClient<'_> {
    fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
        magi_bridge_client::native_executor_capabilities()
    }

    fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
        unreachable!()
    }
    fn invoke_streaming(
        &self,
        _: ModelInvocationRequest,
        _: &dyn Fn(&ModelStreamingDelta),
    ) -> Result<ModelResponse, BridgeClientError> {
        unreachable!()
    }
    fn invoke_with_retry_events_and_cancellation(
        &self,
        _: ModelInvocationRequest,
        retry: &dyn Fn(&ModelRetryRuntimeEvent),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ModelResponse, BridgeClientError> {
        if self.reject_retry {
            self.store
                .install_session_event_writer(Arc::new(RejectWrites));
        }
        retry(&ModelRetryRuntimeEvent {
            phase: magi_bridge_client::ModelRetryRuntimePhase::AttemptStarted,
            attempt: 1,
            max_attempts: 2,
            delay_ms: None,
        });
        if cancelled() {
            return Err(magi_bridge_client::model_invocation_cancelled_error());
        }
        Ok(ModelResponse::completed("after retry"))
    }
}
#[test]
fn transport_retry_is_durable_and_failure_to_record_it_cancels_before_resend() {
    for reject_retry in [false, true] {
        let (store, session, thread) = store();
        let inner = RetryClient {
            store: &store,
            reject_retry,
        };
        let client = StepModelClient::new(
            &inner,
            &store,
            &session,
            Some("turn-journal-client"),
            None,
            &thread,
            "call-retry",
            0,
        );
        let result = client.invoke(request("retry"));
        assert_eq!(result.is_err(), reject_retry);
        let turn = store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap();
        let step = &turn.model_steps[0];
        if reject_retry {
            assert_eq!(step.attempts.len(), 1);
            assert_eq!(step.status, ModelStepStatus::Started);
        } else {
            assert_eq!(step.attempts.len(), 2);
            assert_eq!(step.attempts[0].status, ModelStepStatus::OutcomeUnknown);
            assert_eq!(step.attempts[1].status, ModelStepStatus::ResponseReceived);
        }
    }
}

#[test]
fn persistence_failure_prevents_provider_call_and_no_input_is_required_for_a_step() {
    let (store, session, thread) = store();
    let inner = Client {
        calls: AtomicUsize::new(0),
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "call-0",
        0,
    );
    store.install_session_event_writer(Arc::new(RejectWrites));
    assert!(client.invoke(request("original")).is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(
        store
            .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
            .unwrap()
            .model_steps
            .is_empty()
    );
}
#[test]
fn cancelled_turn_settles_late_usage_without_delivering_the_response() {
    let (store, session, thread) = store();
    let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
        magi_core::MissionId::new("mission-late-usage")
    });
    store
        .create_goal(
            session.clone(),
            main_thread,
            "turn-journal-client",
            "usage accounting",
            magi_core::AccessProfile::Restricted,
        )
        .unwrap();
    struct LateClient<'a> {
        store: &'a SessionStore,
        session: &'a SessionId,
    }
    impl ModelBridgeClient for LateClient<'_> {
        fn executor_capabilities(&self) -> magi_bridge_client::ExecutorCapabilities {
            magi_bridge_client::native_executor_capabilities()
        }

        fn invoke(&self, _: ModelInvocationRequest) -> Result<ModelResponse, BridgeClientError> {
            self.store
                .update_current_turn_status_for_turn_with_change(
                    self.session,
                    Some("turn-journal-client"),
                    "cancelled",
                )
                .unwrap();
            let mut response = ModelResponse::completed("late");
            response.usage = Some(serde_json::json!({"prompt_tokens":5,"completion_tokens":5008}));
            Ok(response)
        }
        fn invoke_streaming(
            &self,
            request: ModelInvocationRequest,
            _: &dyn Fn(&ModelStreamingDelta),
        ) -> Result<ModelResponse, BridgeClientError> {
            self.invoke(request)
        }
    }
    let inner = LateClient {
        store: &store,
        session: &session,
    };
    let client = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "late-call",
        0,
    );
    assert!(
        client.invoke(request("work")).is_err(),
        "迟到模型响应不能再次交付"
    );
    let goal = store.current_goal(&session).unwrap();
    assert_eq!(goal.tokens_used, 5013);
    let restored = SessionStore::from_persisted_parts(
        store.durable_state(),
        store.execution_sidecar_store_state(),
    )
    .unwrap();
    assert_eq!(restored.current_goal(&session).unwrap().tokens_used, 5013);
    let step = &restored
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap()
        .model_steps[0];
    assert_eq!(step.status, ModelStepStatus::ResponseReceived);
    assert_eq!(step.usage_tokens, Some(5013));
}

#[test]
fn user_pause_blocks_new_requests_before_turn_cancellation_finishes_and_resume_reopens_it() {
    let (store, session, thread) = store();
    let (_, main_thread) = store.ensure_session_mission(&session, UtcMillis::now(), || {
        magi_core::MissionId::new("mission-pause-before-cancel")
    });
    let goal = store
        .create_goal(
            session.clone(),
            main_thread,
            "turn-journal-client",
            "pause",
            magi_core::AccessProfile::Restricted,
        )
        .unwrap();
    // 先冻结一个已发送请求，暂停必须同时阻止它的 transport 重试。
    let now = UtcMillis::now();
    store
        .begin_model_step(
            &session,
            "turn-journal-client",
            ModelStep {
                step_id: "before-pause".into(),
                attempts: vec![ModelAttempt {
                    transport: None,
                    attempt_id: "before-pause:attempt:1".into(),
                    started_at: now,
                    settled_at: None,
                    status: ModelStepStatus::Started,
                }],
                task_id: None,
                goal_id: None,
                usage_tokens: None,
                goal_tokens_after: None,
                thread_id: thread.clone(),
                checkpoint_generation: 0,
                request_fingerprint: "a".repeat(64),
                provider: "test".into(),
                binding: None,
                executor_capabilities: None,
                purpose: None,
                message_count: 1,
                tool_count: 0,
                max_output_tokens: None,
                input_request_ids: vec![],
                started_at: now,
                settled_at: None,
                status: ModelStepStatus::Started,
                response_fingerprint: None,
                prompt_frame: None,
                cache_epoch: 0,
            },
        )
        .unwrap();
    let paused = store
        .pause_goal_with_plan(&session, &goal.goal_id, goal.control_revision, None)
        .unwrap()
        .0;
    let inner = Client {
        calls: AtomicUsize::new(0),
    };
    let error = StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "paused",
        0,
    )
    .invoke(request("do not send"))
    .unwrap_err();
    assert_eq!(
        crate::model_error::classify_model_invocation_error(&error.to_string()).code,
        "run_paused"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert!(
        store
            .start_model_step_retry(&session, "turn-journal-client", "before-pause")
            .is_err()
    );
    let steps = store
        .canonical_turn_for_session_turn_id(&session, "turn-journal-client")
        .unwrap()
        .model_steps;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].attempts.len(), 1);
    store
        .resume_goal_with_plan(
            &session,
            &paused.goal_id,
            paused.control_revision,
            None,
            None,
        )
        .unwrap();
    StepModelClient::new(
        &inner,
        &store,
        &session,
        Some("turn-journal-client"),
        None,
        &thread,
        "resumed",
        0,
    )
    .invoke(request("resume"))
    .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

use super::*;
use crate::ModelTransportRecorder;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn body_timing_uses_network_receipts_and_paired_final_usage() {
    use crate::protocol::streaming::SseEvent;
    let event = |data: &str| SseEvent {
        event_type: None,
        data: data.into(),
    };
    let mut timing = BodyStreamTiming::default();
    timing.observe(
        &event(r#"{"choices":[{"delta":{"reasoning_content":"thinking"}}]}"#),
        false,
        Duration::from_millis(50),
    );
    timing.observe(&event("{}"), true, Duration::from_millis(200));
    timing.observe(&event("{}"), true, Duration::from_millis(200));
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 100, true, false)
            .unwrap()
            .content_duration_ms,
        0,
        "multiple SSE events from one HTTP read must not manufacture a TPS interval"
    );
    timing.observe(&event("{}"), true, Duration::from_millis(1200));
    timing.observe(&event(r#"{"usage":{"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":60}}}"#), false, Duration::from_millis(1500));
    let result = timing
        .finish(ProviderFamily::OpenAiChat, 100, true, false)
        .unwrap();
    assert_eq!(result.first_content_ms, 200);
    assert_eq!(
        result.content_duration_ms, 1000,
        "usage-tail delay is not body generation time"
    );
    assert_eq!(result.text_output_tokens, Some(40));
    assert_eq!(
        result.response_duration_ms,
        Some(1500),
        "overall throughput includes the complete response, while body timing excludes the usage tail"
    );
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 101, true, false)
            .unwrap()
            .text_output_tokens,
        None
    );
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 100, true, true)
            .unwrap()
            .text_output_tokens,
        None
    );
    timing.observe(
        &event(r#"{"usage":{"completion_tokens":80}}"#),
        false,
        Duration::from_millis(1800),
    );
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 80, true, false)
            .unwrap()
            .response_duration_ms,
        Some(1800),
        "missing reasoning split does not discard observed response duration"
    );
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 80, false, false)
            .unwrap()
            .text_output_tokens,
        None,
        "final usage missing reasoning metadata cannot inherit an intermediate split"
    );
    timing.observe(&event(r#"{"usage":{"completion_tokens":20,"completion_tokens_details":{"reasoning_tokens":25}}}"#), false, Duration::from_millis(1900));
    assert_eq!(
        timing
            .finish(ProviderFamily::OpenAiChat, 20, true, false)
            .unwrap()
            .text_output_tokens,
        None
    );
}

#[test]
fn body_timing_excludes_redacted_thinking_audio_and_unknown_output_items() {
    use crate::protocol::streaming::SseEvent;
    for (family, non_text) in [
        (
            ProviderFamily::Anthropic,
            r#"{"content_block":{"type":"redacted_thinking","data":"encrypted"}}"#,
        ),
        (
            ProviderFamily::OpenAiResponses,
            r#"{"item":{"type":"image_generation_call"}}"#,
        ),
        (
            ProviderFamily::OpenAiChat,
            r#"{"choices":[{"index":0,"delta":{"audio":{"data":"bytes"}}}]}"#,
        ),
        (
            ProviderFamily::OpenAiChat,
            r#"{"choices":[{"index":1,"delta":{"content":"other choice"}}]}"#,
        ),
    ] {
        let mut timing = BodyStreamTiming::default();
        let event = |data: &str| SseEvent {
            event_type: None,
            data: data.into(),
        };
        timing.observe(&event(non_text), false, Duration::from_millis(10));
        timing.observe(&event("{}"), true, Duration::from_millis(200));
        timing.observe(&event("{}"), true, Duration::from_millis(1200));
        timing.observe(
            &event(
                r#"{"usage":{"output_tokens":100,"output_tokens_details":{"reasoning_tokens":0}}}"#,
            ),
            false,
            Duration::from_millis(1500),
        );
        assert_eq!(
            timing
                .finish(family, 100, false, false)
                .unwrap()
                .text_output_tokens,
            None
        );
    }
    let mut plain = BodyStreamTiming {
        first: Some(Duration::from_millis(200)),
        last: Some(Duration::from_millis(1200)),
        usage_output_tokens: Some(100),
        usage_observed: true,
        ..Default::default()
    };
    assert_eq!(
        plain
            .finish(ProviderFamily::Anthropic, 100, false, false)
            .unwrap()
            .text_output_tokens,
        Some(100)
    );
    assert_eq!(
        plain
            .finish(ProviderFamily::Anthropic, 100, true, false)
            .unwrap()
            .text_output_tokens,
        None
    );
    plain.usage_output_tokens = None;
    assert_eq!(
        plain
            .finish(ProviderFamily::Anthropic, 0, false, false)
            .unwrap()
            .text_output_tokens,
        None
    );
}

#[test]
fn timed_http_stream_receipt_matches_usage_authority_without_ui_estimates() {
    use magi_usage_authority::*;
    use std::io::BufRead;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (ack_tx, ack_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let mut stream = reader.into_inner();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        let mut send = |body: &str| {
            write!(stream, "{:x}\r\n{}\r\n", body.len(), body).unwrap();
            stream.flush().unwrap();
        };
        send(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
        );
        ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        send("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"A\"}}]}\n\n");
        ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        send("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"B\"}}]}\n\n");
        send(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"prompt_tokens_details\":{\"cached_tokens\":600},\"completion_tokens\":100,\"completion_tokens_details\":{\"reasoning_tokens\":80}}}\n\ndata: [DONE]\n\n",
        );
        stream.write_all(b"0\r\n\r\n").unwrap();
    });
    let client = HttpModelBridgeClient::new(url.clone(), None, "timing-test".into());
    let recorder = std::cell::RefCell::new(ModelTransportRecorder::default());
    let delta = |delta: &ModelStreamingDelta| {
        if !delta.thinking.is_empty() || delta.content == "A" {
            ack_tx.send(()).unwrap();
        }
    };
    let response = client
        .invoke_observed(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "test".into(),
                prompt: "test".into(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            ModelInvocationOptions {
                on_delta: Some(&delta),
                on_retry: &|_| {},
                is_cancelled: &|| false,
                max_output_tokens: None,
            },
            &|event| recorder.borrow_mut().apply(event).map(|_| ()),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.content.as_deref(), Some("AB"));
    let receipt = recorder.borrow().attempts[0].clone();
    let timing = receipt.response_timing.clone().unwrap();
    assert!(
        timing.first_content_ms >= 50,
        "thinking is not the first body token"
    );
    assert!(
        timing.content_duration_ms >= 80,
        "only distinct HTTP receipts establish body duration"
    );
    assert_eq!(timing.text_output_tokens, Some(20));
    let usage = receipt.usage.as_ref().unwrap();
    assert_eq!(receipt.usage_observed, Some(true));
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens
        ),
        (1000, 100, Some(600))
    );
    let mut identity =
        build_usage_call_identity("attempt-1", None, UsageSourceRole::Orchestrator, None);
    identity.transport = Some(UsageTransportIdentity {
        usage_reported: Some(true),
        response_timing: Some(timing.clone()),
        request_fingerprint: receipt.request_fingerprint,
        http_status: receipt.http_status,
    });
    let call = UsageCallRecordInput {
        workspace_id: Some("ws".into()),
        owner: magi_usage_authority::UsageOwner::session("session"),
        turn_id: None,
        dispatch_wave_id: None,
        assignment_id: None,
        event_id: Some("timed-receipt".into()),
        timestamp: Some(1),
        execution_binding: build_execution_binding_identity(
            "main",
            "native",
            1,
            UsageSourceRole::Orchestrator,
        ),
        model_config: LlmConfig {
            provider: "test".into(),
            model: "timing-test".into(),
            base_url: url,
            api_key: None,
            account_fingerprint: None,
            reasoning_effort: None,
        },
        call_identity: identity,
        usage: UsageTokenInput {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: None,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cache_read_included_in_input: usage.cache_read_included_in_input,
        },
        status: UsageCallStatus::Success,
        error_code: None,
    };
    let mut authority = UsageAuthority::new();
    authority.append_call_record(call.clone());
    authority.append_call_record(call);
    let stats = authority.get_session_snapshot("ws", "session").totals;
    assert_eq!(stats.llm_call_count, 1);
    assert_eq!(stats.token_observed_call_count, 1);
    assert_eq!(
        stats.cache_read_tokens * 100 / stats.cache_observed_input_tokens,
        60
    );
    assert_eq!(stats.performance.ttft_total_ms, timing.first_content_ms);
    assert_eq!(stats.performance.text_output_tokens, 20);
    assert_eq!(
        stats.performance.content_duration_ms,
        timing.content_duration_ms
    );
}

#[test]
fn non_streaming_body_failure_preserves_received_http_status() {
    use std::io::BufRead;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        reader.into_inner().write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{").unwrap();
    });
    let status = std::cell::Cell::new(None);
    let result = execute_cancellable_http_post(
        "test-body-failure".into(),
        url,
        json!({}),
        vec![],
        &|| false,
        &|event| {
            if let ModelTransportEvent::Updated {
                http_status: Some(value),
                ..
            } = event
            {
                status.set(Some(*value));
            }
            Ok(())
        },
    );
    server.join().unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("reading response body failed")
    );
    assert_eq!(status.get(), Some(503));
}

fn chat_tool_call(id: &str, path: &str) -> crate::types::ChatToolCall {
    crate::types::ChatToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::types::ChatToolFunction {
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": path}).to_string(),
        },
    }
}

#[test]
fn from_env_returns_none_when_base_url_not_set() {
    // Ensure the env var is not set in this process context.
    // from_env() reads MAGI_OPENAI_COMPAT_BASE_URL -- if it is not set,
    // it should return None.
    let saved = env::var(OPENAI_BASE_URL_ENV).ok();
    // SAFETY: test code runs serially via `cargo test`; no other thread
    // accesses this env var concurrently in this test binary.
    unsafe { env::remove_var(OPENAI_BASE_URL_ENV) };

    let result = HttpModelBridgeClient::from_env();
    assert!(
        result.is_none(),
        "from_env() should return None without BASE_URL"
    );

    // Restore if it was set.
    if let Some(value) = saved {
        // SAFETY: same as above.
        unsafe { env::set_var(OPENAI_BASE_URL_ENV, value) };
    }
}

#[test]
fn round_context_tail_is_excluded_from_the_anthropic_cached_history() {
    let message = |role: &str, text: &str, origin: ChatMessageOrigin| crate::types::ChatMessage {
        input_request_id: None,
        context_origin: origin,
        role: role.into(),
        content: Some(text.into()),
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        provider_context: Vec::new(),
    };
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://api.anthropic.com".into(),
        None,
        "test".into(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "test".into(),
        prompt: String::new(),
        messages: Some(vec![
            message("developer", "HEAD", ChatMessageOrigin::SystemInstruction),
            message("user", "goal", ChatMessageOrigin::Conversation),
            message("developer", "STEPS", ChatMessageOrigin::RoundContext),
            message("user", "CONTEXT", ChatMessageOrigin::RoundContext),
        ]),
        tools: None,
        tool_choice: None,
    };
    let body = client.build_http_request(&request, true).unwrap().body;
    assert!(!body["system"].to_string().contains("STEPS"));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert!(!messages[1].to_string().contains("cache_control"));
    assert!(!messages[2].to_string().contains("cache_control"));
}

#[test]
fn prompt_cache_keys_are_stable_across_streaming_and_obey_protocol_support() {
    let request = ModelInvocationRequest {
        cache_affinity_key: Some("stable-branch-key".into()),
        prompt_cache_key: Some("stable-session-key".into()),
        provider: "test".into(),
        prompt: "first".into(),
        messages: None,
        tools: None,
        tool_choice: None,
    };
    for (protocol, base_url, supported) in [
        (
            HttpModelBridgeProtocol::Responses,
            "https://relay.example/v1",
            true,
        ),
        (
            HttpModelBridgeProtocol::ChatCompletions,
            "https://api.openai.com/v1",
            true,
        ),
        (
            HttpModelBridgeProtocol::ChatCompletions,
            "https://api.openai.com.example/v1",
            false,
        ),
        (
            HttpModelBridgeProtocol::AnthropicMessages,
            "https://api.anthropic.com",
            false,
        ),
    ] {
        let client = HttpModelBridgeClient::new_with_protocol(
            base_url.into(),
            None,
            "test".into(),
            protocol,
            None,
        );
        assert_eq!(
            client.cache_contract().route_key == ProviderCacheRouteKey::Supported,
            supported
        );
        for stream in [true, false] {
            let mut next = request.clone();
            next.prompt = "second".into();
            let built = client.build_http_request(&request, stream).unwrap();
            // 会话头部与请求体支持情况无关：任何协议、任何端点都带同一个稳定摘要。
            assert!(
                built
                    .headers
                    .iter()
                    .any(|(name, value)| name == "X-Session-Id" && value == "stable-branch-key")
            );
            let first = built.body;
            let second = client
                .build_http_request_with_output_limit(&next, stream, Some(128))
                .unwrap()
                .body;
            assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
            assert_eq!(first.get("prompt_cache_key").is_some(), supported);
            if supported {
                assert_eq!(first["prompt_cache_key"], "stable-session-key");
            }
            next.prompt_cache_key = Some("next-epoch-key".into());
            let next_epoch = client.build_http_request(&next, stream).unwrap();
            assert!(
                next_epoch
                    .headers
                    .iter()
                    .any(|(name, value)| name == "X-Session-Id" && value == "stable-branch-key")
            );
            if supported {
                assert_eq!(next_epoch.body["prompt_cache_key"], "next-epoch-key");
            }
        }
    }
}

#[test]
fn cache_key_without_branch_affinity_does_not_invent_a_session_header() {
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://relay.example/v1".into(),
        None,
        "test".into(),
        HttpModelBridgeProtocol::Responses,
        None,
    );
    let mut request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "probe".into(),
        prompt: "ping".into(),
        messages: None,
        tools: None,
        tool_choice: None,
    };
    let built = client.build_http_request(&request, false).unwrap();
    assert!(built.headers.iter().all(|(name, _)| name != "X-Session-Id"));
    request.prompt_cache_key = Some("provider-epoch-only".into());
    let built = client.build_http_request(&request, false).unwrap();
    assert!(built.headers.iter().all(|(name, _)| name != "X-Session-Id"));
}

#[test]
fn tool_registry_order_does_not_change_wire_prefix_but_schema_changes_do() {
    let tool = |name: &str| crate::types::ChatToolDefinition {
        kind: "function".into(),
        origin: crate::types::ChatToolOrigin::Builtin,
        function: crate::types::ChatToolFunctionDefinition {
            name: name.into(),
            description: name.into(),
            parameters: json!({"type":"object"}),
        },
    };
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://relay.example".into(),
        None,
        "test".into(),
        HttpModelBridgeProtocol::Responses,
        None,
    );
    let mut request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "test".into(),
        prompt: "work".into(),
        messages: None,
        tools: Some(vec![tool("web_search"), tool("file_read")]),
        tool_choice: None,
    };
    let first = client.build_request_body(&request);
    request.tools.as_mut().unwrap().reverse();
    assert_eq!(first, client.build_request_body(&request));
    request.tools.as_mut().unwrap()[0].function.description = "changed policy".into();
    assert_ne!(first["tools"], client.build_request_body(&request)["tools"]);

    let schema_a = json!({
        "type": "object",
        "properties": {
            "z": {"description": "last", "type": "string"},
            "a": {"type": "integer", "description": "first"}
        },
        "required": ["a"]
    });
    let schema_b = json!({
        "required": ["a"],
        "properties": {
            "a": {"description": "first", "type": "integer"},
            "z": {"type": "string", "description": "last"}
        },
        "type": "object"
    });
    let schema_tool = |parameters| crate::types::ChatToolDefinition {
        kind: "function".into(),
        origin: crate::types::ChatToolOrigin::Builtin,
        function: crate::types::ChatToolFunctionDefinition {
            name: "schema_tool".into(),
            description: "schema_tool".into(),
            parameters,
        },
    };
    let mut schema_request = request;
    schema_request.tools = Some(vec![schema_tool(schema_a)]);
    let canonical_a = client.build_request_body(&schema_request);
    schema_request.tools = Some(vec![schema_tool(schema_b)]);
    assert_eq!(
        canonical_a["tools"],
        client.build_request_body(&schema_request)["tools"]
    );
}

#[test]
fn cache_identity_uses_actual_transport_and_excludes_credentials() {
    let make = |url: &str, key: &str, model: &str, protocol, effort| {
        HttpModelBridgeClient::new_with_protocol(
            url.into(),
            Some(key.into()),
            model.into(),
            protocol,
            effort,
        )
        .cache_identity()
        .unwrap()
    };
    let first = make(
        "https://relay.example/v1/",
        "first-secret",
        "gpt-5",
        HttpModelBridgeProtocol::Responses,
        Some(ReasoningEffort::Low),
    );
    let equivalent = make(
        "https://relay.example/v1/responses",
        "second-secret",
        "gpt-5",
        HttpModelBridgeProtocol::Responses,
        Some(ReasoningEffort::Low),
    );
    assert_eq!(first, equivalent);
    assert_ne!(
        first,
        make(
            "https://other.example/v1",
            "first-secret",
            "gpt-5",
            HttpModelBridgeProtocol::Responses,
            Some(ReasoningEffort::Low)
        )
    );
    assert_ne!(
        first,
        make(
            "https://relay.example/v1",
            "first-secret",
            "other-model",
            HttpModelBridgeProtocol::Responses,
            Some(ReasoningEffort::Low)
        )
    );
    assert_ne!(
        first,
        make(
            "https://relay.example/v1",
            "first-secret",
            "gpt-5",
            HttpModelBridgeProtocol::ChatCompletions,
            Some(ReasoningEffort::Low)
        )
    );
    assert_ne!(
        first,
        make(
            "https://relay.example/v1",
            "first-secret",
            "gpt-5",
            HttpModelBridgeProtocol::Responses,
            Some(ReasoningEffort::High)
        )
    );
    let credentials = make(
        "https://user:password@relay.example/v1/responses?token=secret#private",
        "api-secret",
        "gpt-5",
        HttpModelBridgeProtocol::Responses,
        None,
    );
    assert_eq!(credentials.endpoint, "https://relay.example/v1/responses");
    assert!(!format!("{credentials:?}").contains("secret"));
}

#[test]
fn cache_reasoning_identity_follows_rendered_parameters_only() {
    let make = |model: &str, protocol, effort| {
        HttpModelBridgeClient::new_with_protocol(
            "https://relay.example/v1".into(),
            None,
            model.into(),
            protocol,
            effort,
        )
    };
    for protocol in [
        HttpModelBridgeProtocol::Responses,
        HttpModelBridgeProtocol::ChatCompletions,
        HttpModelBridgeProtocol::AnthropicMessages,
    ] {
        let low = make("unknown-model", protocol, Some(ReasoningEffort::Low));
        let high = make("unknown-model", protocol, Some(ReasoningEffort::High));
        assert_eq!(
            low.cache_identity(),
            high.cache_identity(),
            "未发送的 effort 不影响缓存身份"
        );
    }
    for (model, protocol, field) in [
        ("gpt-5", HttpModelBridgeProtocol::Responses, "reasoning"),
        (
            "gpt-5",
            HttpModelBridgeProtocol::ChatCompletions,
            "reasoning_effort",
        ),
        (
            "claude-sonnet-4-20250514",
            HttpModelBridgeProtocol::AnthropicMessages,
            "thinking",
        ),
    ] {
        let low = make(model, protocol, Some(ReasoningEffort::Low));
        let high = make(model, protocol, Some(ReasoningEffort::High));
        assert_ne!(low.cache_identity(), high.cache_identity());
        let identity = low.cache_identity().unwrap();
        let profile: Value = serde_json::from_str(&identity.reasoning_profile).unwrap();
        let request = ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: "hello".into(),
            messages: None,
            tools: None,
            tool_choice: None,
        };
        assert_eq!(profile[field], low.build_request_body(&request)[field]);
    }
}

#[test]
fn stream_idle_timeout_uses_bounded_optional_runtime_override() {
    assert_eq!(
        provider_stream_idle_timeout_from_env(None),
        MODEL_PROVIDER_STREAM_IDLE_TIMEOUT
    );
    assert_eq!(
        provider_stream_idle_timeout_from_env(Some("250")),
        Duration::from_millis(250)
    );
    assert_eq!(
        provider_stream_idle_timeout_from_env(Some("1")),
        MODEL_PROVIDER_MIN_STREAM_IDLE_TIMEOUT
    );
    assert_eq!(
        provider_stream_idle_timeout_from_env(Some("999999999")),
        MODEL_PROVIDER_MAX_STREAM_IDLE_TIMEOUT
    );
    assert_eq!(
        provider_stream_idle_timeout_from_env(Some("not-a-duration")),
        MODEL_PROVIDER_STREAM_IDLE_TIMEOUT
    );
}

#[test]
fn build_request_body_creates_openai_compatible_payload() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "hello world".to_string(),
        messages: None,
        tools: None,
        tool_choice: None,
    });
    assert_eq!(body["model"], "gpt-4.1-mini");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hello world");
    assert_eq!(body["stream"], false);
}

#[test]
fn build_request_body_promotes_multimodal_tool_result_model_content() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let tool_result = serde_json::json!({
        "tool": "view_image",
        "status": "succeeded",
        "summary": "已读取图片",
        "model_content": [
            { "type": "text", "text": "已读取图片" },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "AAA"
                }
            }
        ]
    });

    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "ignored when messages exist".to_string(),
        messages: Some(vec![crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some(tool_result.to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_view_image".to_string()),
            provider_context: Vec::new(),
        }]),
        tools: None,
        tool_choice: None,
    });

    assert_eq!(body["messages"].as_array().expect("messages").len(), 2);
    assert_eq!(body["messages"][0]["role"], "tool");
    assert_eq!(body["messages"][0]["content"], "已读取图片");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(
        body["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAA"
    );
}

#[test]
fn request_history_coalesces_duplicate_tool_call_and_matching_result() {
    let duplicate_id = "call_duplicate";
    let normalized = normalize_chat_tool_call_history(&[
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![
                chat_tool_call(duplicate_id, "package.json"),
                chat_tool_call(duplicate_id, "package.json"),
                chat_tool_call("call_unique", "tsconfig.json"),
            ],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some("package contents".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(duplicate_id.to_string()),
            provider_context: Vec::new(),
        },
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some("reused package contents".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(duplicate_id.to_string()),
            provider_context: Vec::new(),
        },
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some("tsconfig contents".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_unique".to_string()),
            provider_context: Vec::new(),
        },
    ]);

    assert_eq!(normalized.len(), 3);
    assert_eq!(normalized[0].tool_calls.len(), 2);
    assert_eq!(normalized[1].tool_call_id.as_deref(), Some(duplicate_id));
    assert_eq!(normalized[2].tool_call_id.as_deref(), Some("call_unique"));
    assert_eq!(normalized[1].content.as_deref(), Some("package contents"));
}

#[test]
fn request_history_rewrites_conflicting_duplicate_ids_and_matching_results() {
    let duplicate_id = "call_duplicate";
    let normalized = normalize_chat_tool_call_history(&[
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![
                chat_tool_call(duplicate_id, "package.json"),
                chat_tool_call(duplicate_id, "tsconfig.json"),
            ],
            tool_call_id: None,
            provider_context: Vec::new(),
        },
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some("package contents".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(duplicate_id.to_string()),
            provider_context: Vec::new(),
        },
        crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "tool".to_string(),
            content: Some("tsconfig contents".to_string()),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(duplicate_id.to_string()),
            provider_context: Vec::new(),
        },
    ]);

    assert_eq!(normalized.len(), 3);
    assert_eq!(normalized[0].tool_calls.len(), 2);
    assert_eq!(normalized[0].tool_calls[0].id, duplicate_id);
    assert_ne!(normalized[0].tool_calls[1].id, duplicate_id);
    assert_eq!(normalized[1].tool_call_id.as_deref(), Some(duplicate_id));
    assert_eq!(
        normalized[2].tool_call_id,
        Some(normalized[0].tool_calls[1].id.clone())
    );
}

#[test]
fn build_request_body_sends_chat_message_images_as_multimodal_blocks() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "ignored when messages exist".to_string(),
        messages: Some(vec![crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "user".to_string(),
            content: Some("识别这张图片".to_string()),
            images: vec![crate::llm_types::ImageSource {
                kind: "base64".to_string(),
                media_type: "image/png".to_string(),
                data: "AAA".to_string(),
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
            provider_context: Vec::new(),
        }]),
        tools: None,
        tool_choice: None,
    });

    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "识别这张图片");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAA"
    );
}

#[test]
fn build_request_body_preserves_forced_tool_choice() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "classify".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "classify_session_turn".to_string(),
                description: "分类当前会话 turn".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "route": { "type": "string" }
                    },
                    "required": ["route"]
                }),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function(
            "classify_session_turn",
        )),
    });

    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(
        body["tool_choice"]["function"]["name"],
        "magi_builtin_classify_session_turn"
    );
}

#[test]
fn request_uses_wire_safe_names_for_every_tool_source() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "any-openai-compatible-model".to_string(),
    );
    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "搜索最新消息".to_string(),
        messages: Some(vec![crate::types::ChatMessage {
            input_request_id: None,
            context_origin: Default::default(),
            role: "assistant".to_string(),
            content: None,
            images: Vec::new(),
            tool_calls: vec![crate::types::ChatToolCall {
                id: "call-search-1".to_string(),
                kind: "function".to_string(),
                function: crate::types::ChatToolFunction {
                    name: "web_search".to_string(),
                    arguments: r#"{"query":"OpenAI"}"#.to_string(),
                },
            }],
            tool_call_id: None,
            provider_context: Vec::new(),
        }]),
        tools: Some(vec![
            crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "web_search".to_string(),
                    description: "搜索网页".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": { "query": { "type": "string" } },
                        "required": ["query"]
                    }),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            },
            crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "shell_exec".to_string(),
                    description: "执行命令".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": { "command": { "type": "string" } },
                        "required": ["command"]
                    }),
                },
                origin: crate::types::ChatToolOrigin::Builtin,
            },
            crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "mcp__repo__inspect".to_string(),
                    description: "检查仓库".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                },
                origin: crate::types::ChatToolOrigin::ExternalMcp,
            },
            crate::types::ChatToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ChatToolFunctionDefinition {
                    name: "skill__review__inspect".to_string(),
                    description: "执行审查技能".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                },
                origin: crate::types::ChatToolOrigin::Skill,
            },
        ]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
    });

    for tool in body["tools"].as_array().expect("tools must be an array") {
        assert!(
            tool["function"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("magi_"))
        );
    }
    assert_eq!(
        body["tools"][3]["function"]["name"],
        "magi_builtin_web_search"
    );
    assert_eq!(
        body["tools"][0]["function"]["name"],
        "magi_mcp_mcp__repo__inspect"
    );
    assert_eq!(
        body["tools"][2]["function"]["name"],
        "magi_skill_skill__review__inspect"
    );
    assert!(body["tools"][0].get("origin").is_none());
    assert!(body["tools"][2].get("origin").is_none());
    assert!(body["tools"][3].get("origin").is_none());
    assert_eq!(
        body["tool_choice"]["function"]["name"],
        body["tools"][3]["function"]["name"]
    );
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["name"],
        body["tools"][3]["function"]["name"]
    );
}

#[test]
fn cache_prompt_ir_uses_the_same_wire_tool_names_as_the_request() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        None,
        "cache-ir-model".to_string(),
    );
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "inspect".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "mcp__repo__inspect".to_string(),
                description: "inspect".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            },
            origin: crate::types::ChatToolOrigin::ExternalMcp,
        }]),
        tool_choice: None,
    };
    let body = client.build_request_body(&request);
    let ir = client.cache_prompt_ir(&request);
    assert_eq!(
        ir.tools.as_ref().unwrap()[0].name,
        body["tools"][0]["function"]["name"]
    );
}

#[test]
fn response_restores_canonical_tool_name_from_current_surface() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "any-openai-compatible-model".to_string(),
    );
    let invocation = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "search".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "web_search".to_string(),
                description: "search".to_string(),
                parameters: serde_json::json!({ "type": "object" }),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
    };
    let http_request = client
        .build_http_request(&invocation, false)
        .expect("request should build");
    let wire_name = http_request.tool_name_codec.encode_name("web_search");
    let canonical = client
        .parse_success_payload(
            &serde_json::json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "tool_calls": [{
                            "id": "call-search-1",
                            "type": "function",
                            "function": {
                                "name": wire_name,
                                "arguments": "{\"query\":\"OpenAI\"}"
                            }
                        }]
                    }
                }]
            })
            .to_string(),
            &http_request.tool_name_codec,
        )
        .expect("provider response should be bridgeable");
    assert_eq!(canonical.tool_calls[0].function.name, "web_search");
}

#[test]
fn compatible_response_normalizes_missing_tool_call_identity_and_reasoning_alias() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "non-gpt-compatible-model".to_string(),
    );
    let codec = client
        .build_http_request(
            &ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "test".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            false,
        )
        .expect("request should build")
        .tool_name_codec;
    let canonical = client
        .parse_success_payload(
            &serde_json::json!({
                "choices": [{
                    "stop_reason": "tool_calls",
                    "message": {
                        "reasoning": "需要查询",
                        "tool_calls": [{
                            "function": {
                                "name": "web_search",
                                "arguments": {"query": "Magi"}
                            }
                        }]
                    }
                }]
            })
            .to_string(),
            &codec,
        )
        .expect("兼容提供方省略工具 id 和 type 时仍应归一化");
    assert_eq!(canonical.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(canonical.thinking.as_deref(), Some("需要查询"));
    assert_eq!(canonical.tool_calls[0].kind, "function");
    assert!(canonical.tool_calls[0].id.starts_with("call_compat_0_"));
}

#[test]
fn streaming_tool_calls_restore_canonical_tool_name() {
    let tool_definition = crate::types::ChatToolDefinition {
        kind: "function".to_string(),
        function: crate::types::ChatToolFunctionDefinition {
            name: "web_search".to_string(),
            description: "搜索网页".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        },
        origin: crate::types::ChatToolOrigin::Builtin,
    };
    let invocation = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai-compatible".to_string(),
        prompt: "搜索 Magi".to_string(),
        messages: None,
        tools: Some(vec![tool_definition.clone()]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("web_search")),
    };
    let codec_client = HttpModelBridgeClient::new(
        "http://127.0.0.1:1".to_string(),
        Some("test-key".to_string()),
        "any-openai-compatible-model".to_string(),
    );
    let wire_name = codec_client
        .build_http_request(&invocation, true)
        .expect("request should build")
        .tool_name_codec
        .encode_name("web_search");
    let start_event = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "type": "function",
                    "function": {
                        "name": wire_name.clone(),
                        "arguments": {"query": "Magi"}
                    }
                }]
            },
            "stop_reason": null
        }]
    });
    let finish_event = serde_json::json!({
        "choices": [{
            "delta": {},
            "stop_reason": "tool_calls"
        }]
    });
    let server = spawn_mock_server_with_response_text(
        200,
        "text/event-stream",
        format!("data: {start_event}\n\ndata: {finish_event}\n\ndata: [DONE]\n\n"),
    );
    let client = HttpModelBridgeClient::new(
        server.address,
        Some("test-key".to_string()),
        "any-openai-compatible-model".to_string(),
    );

    let deltas = std::sync::Mutex::new(Vec::new());
    let response = client
        .invoke_streaming(invocation, &|delta| {
            deltas
                .lock()
                .expect("delta lock should hold")
                .push(delta.clone());
        })
        .expect("streaming response should be bridgeable");

    assert!(
        deltas
            .lock()
            .expect("delta lock should hold")
            .iter()
            .any(|delta| {
                delta
                    .tool_calls
                    .iter()
                    .any(|call| call.function.name == wire_name)
            }),
        "tool-call-only streaming must expose a raw tool-call delta"
    );
    assert_eq!(response.tool_calls[0].function.name, "web_search");
    assert_eq!(
        response.tool_calls[0].function.arguments,
        serde_json::json!({"query": "Magi"}).to_string(),
    );
    assert!(response.tool_calls[0].id.starts_with("call_compat_0_"));

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mock server should receive request");
    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("request body should be json");
    assert_eq!(body["tools"][0]["function"]["name"], wire_name);
    assert_eq!(body["tool_choice"]["function"]["name"], wire_name);
}

#[test]
fn forced_tool_choice_rejection_retries_with_required_choice() {
    let request = HttpModelRequest {
        url: "http://localhost/v1/chat/completions".to_string(),
        body: json!({
            "tools": [{"type": "function"}],
            "tool_choice": {
                "type": "function",
                "function": {"name": "shell_exec"}
            }
        }),
        headers: Vec::new(),
        tool_name_codec: ProviderToolNameCodec::for_params(&compile_prompt_ir(
            &ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "test".to_string(),
                prompt: "test".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            false,
            None,
            None,
        )),
    };
    let error_body = r#"{"error":{"message":"tool_choice does not support required or object in thinking mode"}}"#;

    assert!(is_forced_tool_choice_rejection(
        400,
        error_body,
        &request.body,
        ProviderFamily::OpenAiChat
    ));
    let (capability, fallback) =
        request_with_next_tool_choice_enforcement(&request, ProviderFamily::OpenAiChat)
            .expect("exact choice must have a required fallback");
    assert_eq!(capability, ForcedToolChoiceCapability::Required);
    assert_eq!(fallback.body["tool_choice"], "required");
    assert_eq!(request.body["tool_choice"]["type"], "function");
}

#[test]
fn opaque_gateway_bad_request_retries_forced_tool_choice() {
    let body = json!({
        "tools": [{"type": "function"}],
        "tool_choice": {
            "type": "function",
            "function": {"name": "shell_exec"}
        }
    });

    assert!(is_forced_tool_choice_rejection(
        400,
        r#"{"error":{"message":"Upstream request failed. Please try again later."}}"#,
        &body,
        ProviderFamily::OpenAiChat
    ));
    assert!(!is_forced_tool_choice_rejection(
        429,
        r#"{"error":{"message":"Upstream request failed. Please try again later."}}"#,
        &body,
        ProviderFamily::OpenAiChat
    ));
}

#[test]
fn required_choice_rejection_retries_with_automatic_choice() {
    let request = HttpModelRequest {
        url: "http://localhost/v1/chat/completions".to_string(),
        body: json!({
            "tools": [{"type": "function"}],
            "tool_choice": "required"
        }),
        headers: Vec::new(),
        tool_name_codec: ProviderToolNameCodec::for_params(&compile_prompt_ir(
            &ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "test".to_string(),
                prompt: "test".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            false,
            None,
            None,
        )),
    };

    let (capability, fallback) =
        request_with_next_tool_choice_enforcement(&request, ProviderFamily::OpenAiChat)
            .expect("required choice must have an automatic fallback");
    assert_eq!(capability, ForcedToolChoiceCapability::AutoOnly);
    assert_eq!(fallback.body["tool_choice"], "auto");
}

#[test]
fn unrelated_bad_request_is_not_retried_with_automatic_choice() {
    let body = json!({
        "tools": [{"type": "function"}],
        "tool_choice": {
            "type": "function",
            "function": {"name": "shell_exec"}
        }
    });

    assert!(!is_forced_tool_choice_rejection(
        400,
        r#"{"error":{"message":"context length exceeded"}}"#,
        &body,
        ProviderFamily::OpenAiChat
    ));
}

#[test]
fn build_anthropic_request_body_uses_messages_contract() {
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://api.anthropic.com".to_string(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );

    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "anthropic".to_string(),
        prompt: "ignored when messages exist".to_string(),
        messages: Some(vec![
            crate::types::ChatMessage {
                input_request_id: None,
                context_origin: Default::default(),
                role: "system".to_string(),
                content: Some("系统约束".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
            crate::types::ChatMessage {
                input_request_id: None,
                context_origin: Default::default(),
                role: "user".to_string(),
                content: Some("你好".to_string()),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                provider_context: Vec::new(),
            },
        ]),
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "shell_exec".to_string(),
                description: "运行 shell 命令".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string" }
                    },
                    "required": ["cmd"]
                }),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
    });

    assert_eq!(body["model"], "claude-sonnet-test");
    assert_eq!(body["system"], "系统约束");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "你好");
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(body["tools"][0]["name"], "magi_builtin_shell_exec");
    assert_eq!(body["tools"][0]["input_schema"]["required"][0], "cmd");
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], body["tools"][0]["name"]);
    assert_eq!(body["stream"], false);
}

#[test]
fn chat_completions_injects_reasoning_effort_when_configured() {
    let cases = [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Xhigh, "xhigh"),
    ];
    for (effort, expected) in cases {
        let client = HttpModelBridgeClient::new_with_protocol(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            // 选用命中能力表 OpenAI reasoning profile 的模型名（gpt-5*）；
            // 未命中条目的模型即便配置了 effort 也不会注入 reasoning_effort。
            "gpt-5-test".to_string(),
            HttpModelBridgeProtocol::ChatCompletions,
            Some(effort),
        );
        let body = client.build_request_body(&ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai".to_string(),
            prompt: "ping".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });
        assert_eq!(
            body["reasoning_effort"], expected,
            "effort {:?} should serialize to `{}`",
            effort, expected
        );
    }
}

#[test]
fn declared_alias_capabilities_drive_each_wire_protocol() {
    use crate::reasoning::{ReasoningCapability, ReasoningTransport};
    for protocol in [
        HttpModelBridgeProtocol::ChatCompletions,
        HttpModelBridgeProtocol::Responses,
        HttpModelBridgeProtocol::AnthropicMessages,
    ] {
        for effort in crate::reasoning::EFFORTS {
            let capability = ReasoningCapability {
                supported_efforts: crate::reasoning::EFFORTS.to_vec(),
                default_effort: Some(ReasoningEffort::High),
                transport: if protocol == HttpModelBridgeProtocol::AnthropicMessages {
                    ReasoningTransport::AnthropicEffort
                } else {
                    ReasoningTransport::Openai
                },
                source: "provider".into(),
            };
            let client = HttpModelBridgeClient::new_with_protocol(
                "https://fixture.invalid/v1".into(),
                None,
                "opaque-alias".into(),
                protocol,
                Some(*effort),
            )
            .with_reasoning_capability(capability);
            let request = ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "fixture".into(),
                prompt: "test".into(),
                messages: None,
                tools: None,
                tool_choice: None,
            };
            let body = client.build_http_request(&request, false).unwrap().body;
            assert_eq!(body["model"], "opaque-alias");
            let expected = crate::reasoning::effort_label(*effort);
            match protocol {
                HttpModelBridgeProtocol::ChatCompletions => {
                    assert_eq!(body["reasoning_effort"], expected)
                }
                HttpModelBridgeProtocol::Responses => {
                    assert_eq!(body["reasoning"]["effort"], expected)
                }
                HttpModelBridgeProtocol::AnthropicMessages if *effort == ReasoningEffort::None => {
                    assert!(body.get("thinking").is_none())
                }
                HttpModelBridgeProtocol::AnthropicMessages => {
                    assert_eq!(body["output_config"]["effort"], expected)
                }
            }
        }
    }
}

#[test]
fn undeclared_aliases_and_unspecified_defaults_omit_optional_reasoning_fields() {
    for protocol in [
        HttpModelBridgeProtocol::ChatCompletions,
        HttpModelBridgeProtocol::Responses,
        HttpModelBridgeProtocol::AnthropicMessages,
    ] {
        for (efforts, requested) in [
            (vec![], Some(ReasoningEffort::High)),
            (vec![ReasoningEffort::High], None),
        ] {
            let client = HttpModelBridgeClient::new_with_protocol(
                "https://fixture.invalid/v1".into(),
                None,
                "opaque-alias".into(),
                protocol,
                requested,
            )
            .with_reasoning_capability(crate::reasoning::ReasoningCapability {
                supported_efforts: efforts,
                default_effort: None,
                transport: if protocol == HttpModelBridgeProtocol::AnthropicMessages {
                    crate::reasoning::ReasoningTransport::AnthropicEffort
                } else {
                    crate::reasoning::ReasoningTransport::Openai
                },
                source: "provider".into(),
            });
            let body = client
                .build_http_request(
                    &ModelInvocationRequest {
                        cache_affinity_key: None,
                        prompt_cache_key: None,
                        provider: "fixture".into(),
                        prompt: "ping".into(),
                        messages: None,
                        tools: None,
                        tool_choice: None,
                    },
                    false,
                )
                .unwrap()
                .body;
            for key in ["reasoning", "reasoning_effort", "thinking", "output_config"] {
                assert!(body.get(key).is_none(), "{protocol:?}: {key}");
            }
        }
    }
}

#[test]
fn chat_completions_omits_reasoning_effort_when_unset() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-test".to_string(),
    );
    let body = client.build_request_body(&ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "ping".to_string(),
        messages: None,
        tools: None,
        tool_choice: None,
    });
    assert!(
        body.get("reasoning_effort").is_none(),
        "未配置推理级别时不得写入 reasoning_effort 字段"
    );
}

#[test]
fn compaction_output_limit_is_forwarded_to_provider_request() {
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "context-compaction".to_string(),
        prompt: "summarize".to_string(),
        messages: None,
        tools: None,
        tool_choice: None,
    };
    for stream in [false, true] {
        let chat = HttpModelBridgeClient::new_with_protocol(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-test".to_string(),
            HttpModelBridgeProtocol::ChatCompletions,
            None,
        );
        let chat_body = chat
            .build_http_request_with_output_limit(&request, stream, Some(1_024))
            .expect("chat request should build")
            .body;
        assert_eq!(chat_body["max_tokens"], 1_024);

        let responses = HttpModelBridgeClient::new_with_protocol(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "gpt-test".to_string(),
            HttpModelBridgeProtocol::Responses,
            None,
        );
        let responses_body = responses
            .build_http_request_with_output_limit(&request, stream, Some(1_024))
            .expect("responses request should build")
            .body;
        assert_eq!(responses_body["max_output_tokens"], 1_024);

        let anthropic = HttpModelBridgeClient::new_with_protocol(
            "https://api.example.com/v1".to_string(),
            Some("test-key".to_string()),
            "anthropic-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            None,
        );
        let anthropic_body = anthropic
            .build_http_request_with_output_limit(&request, stream, Some(1_024))
            .expect("anthropic request should build")
            .body;
        assert_eq!(anthropic_body["max_tokens"], 1_024);
    }
}

#[test]
fn anthropic_request_maps_reasoning_effort_to_budget_tokens() {
    let cases = [
        (ReasoningEffort::Low, 1024u32),
        (ReasoningEffort::Medium, 4096u32),
        (ReasoningEffort::High, 16384u32),
        (ReasoningEffort::Xhigh, 32768u32),
    ];
    for (effort, expected_budget) in cases {
        let client = HttpModelBridgeClient::new_with_protocol(
            "https://api.anthropic.com".to_string(),
            Some("sk-ant-test".to_string()),
            // 选用命中能力表 BudgetTokens profile 的模型名（claude-opus-4 前缀）；
            // 4.7 系列改走 effort 路径，legacy 4.x 才映射到 budget_tokens。
            "claude-opus-4-5-test".to_string(),
            HttpModelBridgeProtocol::AnthropicMessages,
            Some(effort),
        );
        let body = client.build_request_body(&ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "anthropic".to_string(),
            prompt: "ping".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        });
        assert_eq!(
            body["thinking"]["type"], "enabled",
            "effort {:?} 必须强制启用 thinking",
            effort
        );
        assert_eq!(
            body["thinking"]["budget_tokens"], expected_budget,
            "effort {:?} 应映射到 budget_tokens={}",
            effort, expected_budget
        );
        assert_eq!(
            body["temperature"], 1,
            "Anthropic 在 thinking 启用时 temperature 必须为 1"
        );
        let max_tokens = body["max_tokens"].as_u64().expect("max_tokens 必须为整数");
        assert!(
            max_tokens > expected_budget as u64,
            "max_tokens={} 必须严格大于 budget_tokens={}",
            max_tokens,
            expected_budget
        );
    }
}

#[test]
fn probe_request_inherits_reasoning_effort_for_chat_completions() {
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-5-test".to_string(),
        HttpModelBridgeProtocol::ChatCompletions,
        Some(ReasoningEffort::Xhigh),
    );
    let (url, body, headers) = client
        .build_probe_request()
        .expect("probe request should build");

    assert_eq!(url, "https://api.example.com/v1/chat/completions");
    assert_eq!(body["model"], "gpt-5-test");
    assert_eq!(body["messages"][0]["content"], "ping");
    assert_eq!(
        body["reasoning_effort"], "xhigh",
        "OpenAI 探针必须原样透传 reasoning_effort（Xhigh → xhigh，不再降级到 high）"
    );
    assert!(headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("Authorization") && value == "Bearer test-key"
    }));
}

#[test]
fn probe_request_inherits_thinking_budget_for_anthropic() {
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://api.anthropic.com".to_string(),
        Some("sk-ant-test".to_string()),
        // 命中 claude-opus-4 → BudgetTokens profile；4.7 系列改走 effort 路径。
        "claude-opus-4-5-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        Some(ReasoningEffort::Xhigh),
    );
    let (url, body, headers) = client
        .build_probe_request()
        .expect("probe request should build");

    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    assert_eq!(body["model"], "claude-opus-4-5-test");
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 32768);
    assert_eq!(body["temperature"], 1);
    let max_tokens = body["max_tokens"]
        .as_u64()
        .expect("Anthropic 探针 max_tokens 必须为整数");
    assert!(
        max_tokens > 32768,
        "max_tokens={max_tokens} 必须严格大于 budget_tokens=32768"
    );
    assert!(
        headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-api-key") && value == "sk-ant-test"
        })
    );
    assert!(headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("anthropic-version") && value == "2023-06-01"
    }));
}

#[test]
fn probe_request_omits_reasoning_effort_when_unset() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-test".to_string(),
    );
    let (_, body, _) = client
        .build_probe_request()
        .expect("probe request should build");
    assert!(
        body.get("reasoning_effort").is_none(),
        "未配置推理级别时探针不得写入 reasoning_effort"
    );
}

#[test]
fn adapted_response_preserves_cached_prompt_tokens() {
    let response = ModelResponse::from(AdaptedResponse {
        content: "hello".to_string(),
        thinking: None,
        tool_calls: Vec::new(),
        usage: crate::llm_types::LlmUsage {
            input_tokens: 10,
            output_tokens: 3,
            cache_read_tokens: Some(4),
            cache_write_tokens: None,
            cache_read_included_in_input: true,
        },
        stop_reason: "stop".to_string(),
        raw: None,
        provider_context: Vec::new(),
    });
    let value = response.usage.expect("usage should be present");

    assert_eq!(value["inputTokens"], 10);
    assert_eq!(value["cacheReadTokens"], 4);
}

#[test]
fn empty_prompt_returns_error() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-4".to_string(),
    );

    let result = client.invoke(ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "   ".to_string(),
        messages: None,
        tools: None,
        tool_choice: None,
    });

    let error = result.expect_err("empty prompt should fail");
    match error {
        BridgeClientError::CallFailed { code, .. } => {
            assert_eq!(code, Some(-32002));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn http_status_error_exposes_structured_status() {
    let error = provider_http_status_error(
        429,
        r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#,
        None,
    );

    assert_eq!(error.layer(), Some(BridgeErrorLayer::RemoteBusiness));
    assert_eq!(error.code(), Some(-32006));
    assert_eq!(error.http_status(), Some(429));
    assert!(error.to_string().contains("rate limited"));
    assert!(error.to_string().contains("http_status=429"));
}

#[test]
fn chat_completions_url_builds_correctly() {
    let client = HttpModelBridgeClient::new(
        "https://api.example.com/v1".to_string(),
        None,
        "gpt-4".to_string(),
    );
    assert_eq!(
        client.chat_completions_url().unwrap(),
        "https://api.example.com/v1/chat/completions"
    );

    let client2 = HttpModelBridgeClient::new(
        "https://api.example.com/v1/chat/completions".to_string(),
        None,
        "gpt-4".to_string(),
    );
    assert_eq!(
        client2.chat_completions_url().unwrap(),
        "https://api.example.com/v1/chat/completions"
    );

    let client3 = HttpModelBridgeClient::new("not-a-url".to_string(), None, "gpt-4".to_string());
    assert!(client3.chat_completions_url().is_err());

    let client4 = HttpModelBridgeClient::new(
        "http://example.com:8320/".to_string(),
        None,
        "gpt-4".to_string(),
    );
    assert_eq!(
        client4.chat_completions_url().unwrap(),
        "http://example.com:8320/v1/chat/completions"
    );
}

#[test]
fn standard_endpoints_follow_the_selected_protocol() {
    for (protocol, suffix) in [
        (HttpModelBridgeProtocol::ChatCompletions, "chat/completions"),
        (HttpModelBridgeProtocol::Responses, "responses"),
        (HttpModelBridgeProtocol::AnthropicMessages, "messages"),
    ] {
        for base in [
            "https://gateway.example.com",
            "https://gateway.example.com/v1/",
        ] {
            let client = HttpModelBridgeClient::new_with_protocol(
                base.to_string(),
                Some("test-key".to_string()),
                "test-model".to_string(),
                protocol,
                None,
            );
            let (url, _, _) = client.build_probe_request().unwrap();
            assert_eq!(url, format!("https://gateway.example.com/v1/{suffix}"));
        }
    }
}

#[test]
fn responses_protocol_builds_the_responses_endpoint() {
    let client = HttpModelBridgeClient::new_with_protocol(
        "https://api.openai.com/v1".to_string(),
        Some("test-key".to_string()),
        "gpt-5".to_string(),
        HttpModelBridgeProtocol::Responses,
        None,
    );
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai".to_string(),
        prompt: "读取文件".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "file_read".to_string(),
                description: "读取文件".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("file_read")),
    };
    let http_request = client
        .build_http_request(&request, false)
        .expect("Responses request");
    assert_eq!(http_request.url, "https://api.openai.com/v1/responses");
    let body = http_request.body;
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["tools"][0]["name"], "magi_builtin_file_read");
    assert_eq!(body["tool_choice"]["name"], "magi_builtin_file_read");
    assert_eq!(
        http_request.headers[0],
        ("Authorization".to_string(), "Bearer test-key".to_string())
    );
}

#[test]
fn responses_stream_without_event_headers_completes_through_http_client() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"content\":[]}}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Responses OK\"}\n\n",
                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Responses OK\"}]}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n",
            )
            .to_string(),
        );
    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-5.6-luna".to_string(),
        HttpModelBridgeProtocol::Responses,
        None,
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-responses".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("Responses data.type events should complete the HTTP stream");

    assert_eq!(response.content.as_deref(), Some("Responses OK"));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert_eq!(response.usage.as_ref().unwrap()["inputTokens"], 4);
    assert_eq!(response.usage.as_ref().unwrap()["outputTokens"], 2);
    assert_eq!(response.provider_context.len(), 1);

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mock server should receive the Responses request");
    assert_eq!(recorded.path, "/v1/responses");
}

#[test]
fn anthropic_messages_url_builds_correctly() {
    assert_eq!(
        build_anthropic_messages_url_for_test("https://api.anthropic.com").unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        build_anthropic_messages_url_for_test("https://api.anthropic.com/v1").unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        build_anthropic_messages_url_for_test("https://api.anthropic.com/v1/messages").unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        build_anthropic_messages_url_for_test("https://api.deepseek.com/anthropic").unwrap(),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
}

// -----------------------------------------------------------------------
// Integration test with a local mock HTTP server
// -----------------------------------------------------------------------

struct MockHttpServer {
    address: String,
    _handle: std::thread::JoinHandle<()>,
    request_receiver: mpsc::Receiver<RecordedRequest>,
}

struct RecordedRequest {
    method: String,
    path: String,
    body: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    anthropic_version: Option<String>,
}

struct MockHttpResponse {
    status: u16,
    content_type: String,
    response_text: String,
}

fn spawn_mock_server(status: u16, response_body: serde_json::Value) -> MockHttpServer {
    spawn_mock_server_with_response_text(status, "application/json", response_body.to_string())
}

fn spawn_mock_server_with_response_text(
    status: u16,
    content_type: &'static str,
    response_text: String,
) -> MockHttpServer {
    spawn_mock_server_sequence(vec![MockHttpResponse {
        status,
        content_type: content_type.to_string(),
        response_text,
    }])
}

fn spawn_mock_server_sequence(responses: Vec<MockHttpResponse>) -> MockHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
    let address = listener.local_addr().expect("address should exist");
    let (sender, receiver) = mpsc::channel();

    let handle = std::thread::spawn(move || {
        for response_config in responses {
            let (mut stream, _) = listener.accept().expect("mock server should accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout should set");

            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).expect("should read");
                assert!(read > 0, "should receive data");
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };

            let header_text =
                String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
            let mut lines = header_text.split("\r\n");
            let request_line = lines.next().expect("request line should exist").to_string();
            let mut authorization = None;
            let mut x_api_key = None;
            let mut anthropic_version = None;
            let mut content_length: usize = 0;
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                if let Some((name, value)) = line.split_once(':') {
                    let name_lower = name.trim().to_ascii_lowercase();
                    if name_lower == "content-length" {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                    if name_lower == "authorization" {
                        authorization = Some(value.trim().to_string());
                    }
                    if name_lower == "x-api-key" {
                        x_api_key = Some(value.trim().to_string());
                    }
                    if name_lower == "anthropic-version" {
                        anthropic_version = Some(value.trim().to_string());
                    }
                }
            }

            while buffer.len() < header_end + content_length {
                let read = stream.read(&mut chunk).expect("should read body");
                assert!(read > 0, "should receive body data");
                buffer.extend_from_slice(&chunk[..read]);
            }

            let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
                .expect("body should be utf-8");

            // Parse method and path from request line
            let parts: Vec<&str> = request_line.split_whitespace().collect();
            let method = parts.first().unwrap_or(&"").to_string();
            let path = parts.get(1).unwrap_or(&"").to_string();

            let _ = sender.send(RecordedRequest {
                method,
                path,
                body,
                authorization,
                x_api_key,
                anthropic_version,
            });

            let response = format!(
                "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_text}",
                status_reason(response_config.status),
                response_config.response_text.len(),
                status = response_config.status,
                content_type = response_config.content_type,
                response_text = response_config.response_text,
            );
            stream
                .write_all(response.as_bytes())
                .expect("should write response");
            stream.flush().expect("should flush");
        }
    });

    MockHttpServer {
        address: format!("http://{address}/v1"),
        _handle: handle,
        request_receiver: receiver,
    }
}

fn spawn_cancellation_server(streaming: bool) -> (String, mpsc::Receiver<()>, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
    let address = listener.local_addr().expect("address should exist");
    let (request_ready_tx, request_ready_rx) = mpsc::channel();
    let (disconnected_tx, disconnected_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("mock server should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout should set");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = match stream.read(&mut chunk) {
                Ok(0) => {
                    let _ = disconnected_tx.send(());
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => {
                    let _ = disconnected_tx.send(());
                    return;
                }
                Ok(read) => read,
            };
            request.extend_from_slice(&chunk[..read]);
            if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = match stream.read(&mut chunk) {
                Ok(0) => {
                    let _ = disconnected_tx.send(());
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => {
                    let _ = disconnected_tx.send(());
                    return;
                }
                Ok(read) => read,
            };
            request.extend_from_slice(&chunk[..read]);
        }
        if streaming {
            stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"started\"}}]}\n\n",
                    )
                    .expect("stream headers should write");
            stream.flush().expect("stream should flush");
        }
        let _ = request_ready_tx.send(());
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => {
                    let _ = disconnected_tx.send(());
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => {
                    let _ = disconnected_tx.send(());
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    (
        format!("http://{address}/v1"),
        request_ready_rx,
        disconnected_rx,
    )
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Test Response",
    }
}

#[test]
fn invoke_against_mock_server_returns_success() {
    let server = spawn_mock_server(
        200,
        serde_json::json!({
            "choices": [{
                "message": {
                    "content": "hello from direct HTTP client"
                }
            }]
        }),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let response = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai-compatible".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect("invoke should succeed against mock server");

    assert!(response.is_actionable());
    assert_eq!(
        response.content.as_deref(),
        Some("hello from direct HTTP client")
    );

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mock server should receive request");
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(
        recorded.authorization.as_deref(),
        Some("Bearer sk-test-key")
    );

    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("body should be json");
    assert_eq!(body["model"], "gpt-4.1-mini");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hello");
    assert_eq!(body["stream"], false);
}

#[test]
fn cancellable_non_streaming_request_releases_connection() {
    let (address, request_ready, disconnected) = spawn_cancellation_server(false);
    let client = HttpModelBridgeClient::new(
        address,
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_signal = cancelled.clone();
    let (cancellation_started_tx, cancellation_started_rx) = mpsc::channel();
    std::thread::spawn(move || {
        request_ready
            .recv_timeout(Duration::from_secs(5))
            .expect("request should reach provider before cancellation");
        let cancellation_started = std::time::Instant::now();
        cancellation_signal.store(true, Ordering::SeqCst);
        let _ = cancellation_started_tx.send(cancellation_started);
    });
    let error = client
        .invoke_with_cancellation(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|| cancelled.load(Ordering::SeqCst),
        )
        .expect_err("cancelled request should fail as cancelled");
    assert!(model_invocation_error_is_cancelled(&error));
    let cancellation_started = cancellation_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("cancellation should start after provider receives request");
    assert!(cancellation_started.elapsed() < Duration::from_secs(2));
    disconnected
        .recv_timeout(Duration::from_secs(5))
        .expect("cancelled request should close provider connection");
}

#[test]
fn cancellable_streaming_request_releases_connection() {
    let (address, request_ready, disconnected) = spawn_cancellation_server(true);
    let client = HttpModelBridgeClient::new(
        address,
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_signal = cancelled.clone();
    let (cancellation_started_tx, cancellation_started_rx) = mpsc::channel();
    std::thread::spawn(move || {
        request_ready
            .recv_timeout(Duration::from_secs(5))
            .expect("stream should reach provider before cancellation");
        let cancellation_started = std::time::Instant::now();
        cancellation_signal.store(true, Ordering::SeqCst);
        let _ = cancellation_started_tx.send(cancellation_started);
    });
    let error = client
        .invoke_streaming_with_cancellation(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
            &|_| {},
            &|| cancelled.load(Ordering::SeqCst),
        )
        .expect_err("cancelled stream should fail as cancelled");
    assert!(model_invocation_error_is_cancelled(&error));
    let cancellation_started = cancellation_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stream cancellation should start after provider receives request");
    assert!(cancellation_started.elapsed() < Duration::from_secs(2));
    disconnected
        .recv_timeout(Duration::from_secs(5))
        .expect("cancelled stream should close provider connection");
}

#[test]
fn invoke_2xx_error_body_is_reported_as_provider_error() {
    let server = spawn_mock_server_with_response_text(
        200,
        "application/json",
        serde_json::json!({
            "error": {
                "message": "claude executor: upstream returned empty stream response",
                "type": "server_error",
                "code": "internal_server_error",
            }
        })
        .to_string(),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let error = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai-compatible".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect_err("2xx provider error envelope must be surfaced directly");

    assert!(
        error
            .to_string()
            .contains("claude executor: upstream returned empty stream response")
    );
}

#[test]
fn streaming_preserves_latest_delta_when_one_http_read_contains_multiple_events() {
    let server = spawn_mock_server_with_response_text(
        200,
        "text/event-stream",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string(),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect("streaming invoke should succeed against mock server");

    assert!(response.is_actionable());
    let deltas = deltas.into_inner();
    assert_eq!(deltas.concat(), "Hello", "增量帧拼起来必须是完整内容");
    assert!(
        (1..=2).contains(&deltas.len()),
        "积压的增量允许合并成一帧，但不得丢内容"
    );

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mock server should receive request");
    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("body should be json");
    assert_eq!(body["stream"], true);
}

#[test]
fn streaming_missing_or_empty_usage_remains_unknown_in_response_and_receipt() {
    for tail in [
        "",
        "data: {\"choices\":[],\"usage\":{}}\n\n",
        "data: {\"choices\":[],\"usage\":null}\n\n",
    ] {
        let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"ok\"}}}}]}}\n\n{tail}data: [DONE]\n\n"
            ),
        );
        let client = HttpModelBridgeClient::new(server.address, None, "test".into());
        let recorder = std::cell::RefCell::new(ModelTransportRecorder::default());
        let response = client
            .invoke_observed(
                ModelInvocationRequest {
                    cache_affinity_key: None,
                    prompt_cache_key: None,
                    provider: "test".into(),
                    prompt: "test".into(),
                    messages: None,
                    tools: None,
                    tool_choice: None,
                },
                ModelInvocationOptions {
                    on_delta: Some(&|_| {}),
                    on_retry: &|_| {},
                    is_cancelled: &|| false,
                    max_output_tokens: None,
                },
                &|event| recorder.borrow_mut().apply(event).map(|_| ()),
            )
            .unwrap();
        assert!(
            response.usage.is_none(),
            "adapter defaults must not fabricate reported zero usage"
        );
        let receipt = &recorder.borrow().attempts[0];
        assert!(receipt.usage.is_none());
        assert_eq!(receipt.usage_observed, Some(false));
        assert!(
            receipt.response_timing.is_some(),
            "TTFT can still be measured without token usage"
        );
        assert_eq!(
            receipt.response_timing.as_ref().unwrap().text_output_tokens,
            None
        );
    }
}

#[test]
fn chat_final_usage_corrects_intermediate_counts_and_missing_cache_is_unknown() {
    let server = spawn_mock_server_with_response_text(200, "text/event-stream", concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":100,\"prompt_tokens_details\":{\"cached_tokens\":600},\"completion_tokens_details\":{\"reasoning_tokens\":60}}}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":800,\"completion_tokens\":50}}\n\n",
                "data: [DONE]\n\n",
            ).into());
    let client = HttpModelBridgeClient::new(server.address, None, "test".into());
    let recorder = std::cell::RefCell::new(ModelTransportRecorder::default());
    let response = client
        .invoke_observed(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "test".into(),
                prompt: "test".into(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            ModelInvocationOptions {
                on_delta: Some(&|_| {}),
                on_retry: &|_| {},
                is_cancelled: &|| false,
                max_output_tokens: None,
            },
            &|event| recorder.borrow_mut().apply(event).map(|_| ()),
        )
        .unwrap();
    let usage: crate::llm_types::LlmUsage =
        serde_json::from_value(response.usage.unwrap()).unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens
        ),
        (800, 50, None)
    );
    let receipt = &recorder.borrow().attempts[0];
    assert_eq!(receipt.usage.as_ref().unwrap().output_tokens, 50);
    assert_eq!(receipt.usage.as_ref().unwrap().cache_read_tokens, None);
    assert_eq!(
        receipt.response_timing.as_ref().unwrap().text_output_tokens,
        None
    );
}

#[test]
fn responses_terminal_usage_replaces_intermediate_snapshot_in_transport_receipt() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"content\":[]}}\n\n",
                "event: response.output_text.delta\ndata: {\"output_index\":0,\"delta\":\"ok\"}\n\n",
                "event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}}\n\n",
                "event: response.usage.updated\ndata: {\"type\":\"response.usage.updated\",\"usage\":{\"input_tokens\":10,\"output_tokens\":166}}\n\n",
                "event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":103,\"output_tokens_details\":{\"reasoning_tokens\":80}}}}\n\n",
            )
            .to_string(),
        );
    let client = HttpModelBridgeClient::new_with_protocol(
        server.address,
        None,
        "gpt-6-luna".to_string(),
        HttpModelBridgeProtocol::Responses,
        None,
    );
    let recorder = std::cell::RefCell::new(ModelTransportRecorder::default());
    let on_delta = |_delta: &ModelStreamingDelta| {};
    let on_retry = |_event: &ModelRetryRuntimeEvent| {};
    let cancelled = || false;
    let observer = |event: &ModelTransportEvent| recorder.borrow_mut().apply(event).map(|_| ());
    let response = client
        .invoke_observed(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-responses".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            ModelInvocationOptions {
                on_delta: Some(&on_delta),
                on_retry: &on_retry,
                is_cancelled: &cancelled,
                max_output_tokens: None,
            },
            &observer,
        )
        .expect("Responses stream should complete");
    let usage: crate::llm_types::LlmUsage =
        serde_json::from_value(response.usage.expect("response usage")).unwrap();
    assert_eq!(usage.output_tokens, 103);
    let timing = recorder.borrow().attempts[0]
        .response_timing
        .clone()
        .expect("body stream timing must reach the persisted receipt");
    assert_eq!(timing.text_output_tokens, Some(23));
    assert_eq!(
        timing.content_duration_ms, 0,
        "a single body chunk has no measurable generation interval"
    );
    assert_eq!(
        recorder.borrow().attempts[0]
            .usage
            .as_ref()
            .expect("transport usage")
            .output_tokens,
        103
    );
}

#[test]
fn streaming_coalesces_backlogged_increments_without_losing_content() {
    let mut response_text = String::new();
    for _ in 0..200 {
        response_text.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"字\"}}]}\n\n");
    }
    response_text.push_str(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    let server = spawn_mock_server_with_response_text(200, "text/event-stream", response_text);
    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let callback_count = std::cell::Cell::new(0usize);
    let latest_content = std::cell::RefCell::new(String::new());
    let started_at = Instant::now();

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| {
                callback_count.set(callback_count.get() + 1);
                latest_content.borrow_mut().push_str(&delta.content);
                std::thread::sleep(Duration::from_millis(10));
            },
        )
        .expect("backlogged stream should complete");

    assert!(response.is_actionable());
    assert_eq!(latest_content.borrow().chars().count(), 200);
    assert!(
        callback_count.get() < 20,
        "增量积压时应合并成少量帧，不得逐条回放"
    );
    assert!(
        started_at.elapsed() < Duration::from_millis(500),
        "业务写回速度不得把已完成的上游流拖成逐字播放"
    );
}

#[test]
fn streaming_negotiates_opaque_forced_tool_rejection_to_automatic_choice() {
    let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 400,
                content_type: "application/json".to_string(),
                response_text: r#"{"error":{"message":"Upstream request failed. Please try again later."}}"#.to_string(),
            },
            MockHttpResponse {
                status: 400,
                content_type: "application/json".to_string(),
                response_text: r#"{"error":{"message":"Upstream request failed. Please try again later."}}"#.to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"已恢复\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"已学习\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
        ]);
    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gateway-model-v2026".to_string(),
    );
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai-compatible".to_string(),
        prompt: "执行工具".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "shell_exec".to_string(),
                description: "执行 shell 命令".to_string(),
                parameters: json!({"type": "object"}),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
    };

    let response = client
        .invoke_streaming(request.clone(), &|_| {})
        .expect("opaque forced choice rejection should negotiate to automatic choice");
    assert!(response.is_actionable());
    let learned_response = client
        .invoke_streaming(request, &|_| {})
        .expect("learned automatic-only capability should keep working");
    assert!(learned_response.is_actionable());

    let first: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("first request should arrive")
            .body,
    )
    .expect("first request should be json");
    let second: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("fallback request should arrive")
            .body,
    )
    .expect("fallback request should be json");
    assert_eq!(first["tool_choice"]["type"], "function");
    assert_eq!(second["tool_choice"], "required");
    let third: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("automatic fallback request should arrive")
            .body,
    )
    .expect("automatic fallback request should be json");
    assert_eq!(third["tool_choice"], "auto");
    let fourth: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("learned request should arrive")
            .body,
    )
    .expect("learned request should be json");
    assert_eq!(fourth["tool_choice"], "auto");
}

#[test]
fn streaming_retries_forced_tool_choice_when_provider_rejects_after_delta() {
    let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"首轮半截\"}}]}\n\n",
                    "data: {\"error\":{\"message\":\"tool_choice is not supported in thinking mode\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"自动恢复\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
            },
        ]);
    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-5-turbo".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "openai-compatible".to_string(),
        prompt: "执行工具".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "shell_exec".to_string(),
                description: "执行 shell 命令".to_string(),
                parameters: json!({"type": "object"}),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
    };

    let response = client
        .invoke_streaming(request, &|delta| {
            deltas.borrow_mut().push(delta.content.clone())
        })
        .expect("stream rejection after a partial delta should recover");
    assert!(response.is_actionable());
    assert_eq!(deltas.into_inner(), vec!["自动恢复".to_string()]);

    let first: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("first request should arrive")
            .body,
    )
    .expect("first request should be json");
    let second: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("fallback request should arrive")
            .body,
    )
    .expect("fallback request should be json");
    assert_eq!(first["tool_choice"]["type"], "function");
    assert_eq!(second["tool_choice"], "required");
}

#[test]
fn anthropic_streaming_retries_forced_tool_choice_after_error_event() {
    let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"首轮半截\"}}\n\n",
                    "event: error\n",
                    "data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"tool_choice type tool is not supported\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"自动恢复\"}}\n\n",
                    "event: message_delta\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                    "event: message_stop\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                )
                .to_string(),
            },
        ]);
    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );
    let deltas = std::cell::RefCell::new(Vec::new());
    let request = ModelInvocationRequest {
        cache_affinity_key: None,
        prompt_cache_key: None,
        provider: "anthropic".to_string(),
        prompt: "执行工具".to_string(),
        messages: None,
        tools: Some(vec![crate::types::ChatToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ChatToolFunctionDefinition {
                name: "shell_exec".to_string(),
                description: "执行 shell 命令".to_string(),
                parameters: json!({"type": "object"}),
            },
            origin: crate::types::ChatToolOrigin::Builtin,
        }]),
        tool_choice: Some(crate::types::ChatToolChoice::force_function("shell_exec")),
    };

    let response = client
        .invoke_streaming(request, &|delta| {
            deltas.borrow_mut().push(delta.content.clone())
        })
        .expect("Anthropic stream rejection should recover");
    assert!(response.is_actionable());
    assert_eq!(deltas.into_inner(), vec!["自动恢复".to_string()]);

    let first: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("first request should arrive")
            .body,
    )
    .expect("first request should be json");
    let second: Value = serde_json::from_str(
        &server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("fallback request should arrive")
            .body,
    )
    .expect("fallback request should be json");
    assert_eq!(first["tool_choice"]["type"], "tool");
    assert_eq!(second["tool_choice"]["type"], "any");
}

#[test]
fn streaming_openai_partial_eof_without_terminal_event_is_error() {
    let server = spawn_mock_server_with_response_text(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"半截回复\"},\"finish_reason\":null}]}\n\n"
            .to_string(),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    let error = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect_err("缺少 [DONE]/finish_reason 的提前 EOF 不能被当成正常完成");

    assert!(
        error
            .to_string()
            .contains("provider stream interrupted: missing terminal SSE event")
    );
    assert_eq!(deltas.into_inner(), vec!["半截回复".to_string()]);
}

#[test]
fn streaming_openai_finish_reason_without_done_marker_is_complete() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )
            .to_string(),
        );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("finish_reason 已经证明 OpenAI 流完整");

    assert_eq!(response.content.as_deref(), Some("完整回复"));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
}

#[test]
fn streaming_openai_accepts_done_marker_without_trailing_blank_line() {
    let server = spawn_mock_server_with_response_text(
        200,
        "text/event-stream",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"}}]}\n\n",
            "data: [DONE]\n",
        )
        .to_string(),
    );
    let client = HttpModelBridgeClient::new(
        server.address,
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("EOF 前没有额外空行时仍应提交最终 SSE 事件");

    assert_eq!(response.content.as_deref(), Some("完整回复"));
}

#[test]
fn streaming_anthropic_partial_eof_without_message_stop_is_error() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"半截 Claude 回复\"}}\n\n",
                "event: content_block_stop\ndata: {\"index\":0}\n\n",
            )
            .to_string(),
        );

    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    let error = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "anthropic".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect_err("缺少 message_stop/stop_reason 的 Anthropic 提前 EOF 不能完成 turn");

    assert!(
        error
            .to_string()
            .contains("provider stream interrupted: missing terminal SSE event")
    );
    assert_eq!(deltas.into_inner(), vec!["半截 Claude 回复".to_string()]);
}

#[test]
fn streaming_anthropic_message_stop_marks_stream_complete() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"完整 Claude 回复\"}}\n\n",
                "event: content_block_stop\ndata: {\"index\":0}\n\n",
                "event: message_stop\ndata: {}\n\n",
            )
            .to_string(),
        );

    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "anthropic".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("message_stop 已经证明 Anthropic 流完整");

    assert_eq!(response.content.as_deref(), Some("完整 Claude 回复"));
    assert_eq!(response.finish_reason.as_deref(), Some("end_turn"));
}

#[test]
fn streaming_returns_when_done_marker_arrives_before_socket_close() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock stream server should bind");
    let address = listener.local_addr().expect("address should exist");
    let (request_sender, request_receiver) = mpsc::channel();
    let server_handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("mock stream server should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout should set");

        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("should read request");
            assert!(read > 0, "should receive request bytes");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_text =
            String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
        let content_length = header_text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("should read request body");
            assert!(read > 0, "should receive request body");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let _ = request_sender.send(());

        let response_head = concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: text/event-stream\r\n",
            "connection: keep-alive\r\n",
            "\r\n",
        );
        stream
            .write_all(response_head.as_bytes())
            .expect("should write response headers");
        stream
            .write_all(
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .as_bytes(),
            )
            .expect("should write stream body");
        stream.flush().expect("should flush stream body");
        std::thread::sleep(Duration::from_secs(2));
    });

    let client = HttpModelBridgeClient::new(
        format!("http://{address}/v1"),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let (result_sender, result_receiver) = mpsc::channel();
    let client_handle = std::thread::spawn(move || {
        let result = client.invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        );
        let _ = result_sender.send(result);
    });

    request_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("mock server should receive request");
    let response = result_receiver
        .recv_timeout(Duration::from_millis(500))
        .expect("[DONE] 后必须立即完成，不能等 socket close");
    assert!(
        response
            .expect("streaming invoke should succeed")
            .is_actionable()
    );

    client_handle
        .join()
        .expect("client streaming thread should not panic");
    server_handle
        .join()
        .expect("mock stream server should not panic");
}

#[test]
fn streaming_returns_after_finish_reason_drain_without_waiting_for_socket_close() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock stream server should bind");
    let address = listener.local_addr().expect("address should exist");
    let (request_sender, request_receiver) = mpsc::channel();
    let server_handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("mock stream server should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout should set");

        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("should read request");
            assert!(read > 0, "should receive request bytes");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_text =
            String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
        let content_length = header_text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("should read request body");
            assert!(read > 0, "should receive request body");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let _ = request_sender.send(());

        stream
                .write_all(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "content-type: text/event-stream\r\n",
                        "connection: keep-alive\r\n",
                        "\r\n",
                        "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                    .as_bytes(),
                )
                .expect("should write terminal stream events");
        stream.flush().expect("should flush terminal stream events");
        std::thread::sleep(Duration::from_millis(50));
        stream
                .write_all(
                    b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
                )
                .expect("should write delayed usage event");
        stream.flush().expect("should flush delayed usage event");
        std::thread::sleep(Duration::from_secs(2));
    });

    let client = HttpModelBridgeClient::new(
        format!("http://{address}/v1"),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let (result_sender, result_receiver) = mpsc::channel();
    let client_handle = std::thread::spawn(move || {
        let result = client.invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        );
        let _ = result_sender.send(result);
    });

    request_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("mock server should receive request");
    let response = result_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("finish_reason 后必须在排空窗口内完成，不能等待 socket close")
        .expect("streaming invoke should succeed");
    assert_eq!(response.content.as_deref(), Some("完整回复"));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    let usage = response.usage.expect("usage should exist");
    assert_eq!(usage["inputTokens"], 5);
    assert_eq!(usage["outputTokens"], 3);

    client_handle
        .join()
        .expect("client streaming thread should not panic");
    server_handle
        .join()
        .expect("mock stream server should not panic");
}

#[test]
fn streaming_error_preserves_prior_usage_without_confirming_success() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"prompt_tokens_details\":{\"cached_tokens\":88},\"completion_tokens\":3}}\n\n",
                "data: {\"error\":{\"message\":\"upstream stopped\",\"code\":\"UPSTREAM_STOPPED\"}}\n\n"
            ).into(),
        );
    let client = HttpModelBridgeClient::new(server.address, None, "test".into());
    let recorder = std::cell::RefCell::new(ModelTransportRecorder::default());
    let result = client.invoke_observed(
        ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "test".into(),
            prompt: "test".into(),
            messages: None,
            tools: None,
            tool_choice: None,
        },
        ModelInvocationOptions {
            on_delta: Some(&|_| {}),
            on_retry: &|_| panic!("business rejection must not retry"),
            is_cancelled: &|| false,
            max_output_tokens: None,
        },
        &|event| recorder.borrow_mut().apply(event).map(|_| ()),
    );
    assert!(result.unwrap_err().to_string().contains("upstream stopped"));
    let recorder = recorder.borrow();
    assert_eq!(recorder.attempts.len(), 1);
    let receipt = &recorder.attempts[0];
    assert_eq!(receipt.outcome, Some(ModelTransportOutcome::Failed));
    assert_eq!(receipt.usage_observed, Some(true));
    let usage = receipt.usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 3);
    assert_eq!(usage.cache_read_tokens, Some(88));
    assert!(
        receipt.response_timing.is_none(),
        "failure is not a performance sample"
    );
}

#[test]
fn streaming_json_error_is_reported_as_provider_error() {
    let server = spawn_mock_server_with_response_text(
        200,
        "application/json",
        serde_json::json!({
            "error": {
                "message": "empty_stream: upstream stream closed before first payload",
                "type": "server_error",
                "code": "internal_server_error",
            }
        })
        .to_string(),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let error = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect_err("JSON error body must not be collapsed into empty assistant response");

    assert!(matches!(
        error,
        BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            ..
        }
    ));
    assert!(
        error
            .to_string()
            .contains("empty_stream: upstream stream closed before first payload")
    );
}

#[test]
fn streaming_retries_terminal_empty_response_before_returning_failure() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
    ]);
    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect("terminal empty response should reconnect before failing");

    assert_eq!(response.content.as_deref(), Some("recovered"));
    assert_eq!(deltas.into_inner(), vec!["recovered".to_string()]);
    for _ in 0..2 {
        server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive the initial and retry requests");
    }
}

#[test]
fn streaming_non_sse_html_is_protocol_failure() {
    let server = spawn_mock_server_with_response_text(
        200,
        "text/html",
        "<!doctype html><html><body>gateway ui</body></html>".to_string(),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let error = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect_err("HTML response must not be treated as an empty model answer");

    assert!(
        error
            .to_string()
            .contains("provider response invalid: expected event stream")
    );
}

#[test]
fn invoke_retries_retryable_status_before_success() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 429,
            content_type: "application/json".to_string(),
            response_text: serde_json::json!({
                "error": {
                    "message": "rate limited",
                    "type": "rate_limit_error",
                    "code": "too_many_requests",
                }
            })
            .to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "application/json".to_string(),
            response_text: serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "retry recovered"
                    }
                }]
            })
            .to_string(),
        },
    ]);

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let events = std::cell::RefCell::new(Vec::new());
    let response = client
        .invoke_with_retry_events_and_cancellation(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|event| events.borrow_mut().push(event.phase),
            &|| false,
        )
        .expect("retryable rejection should recover on the next attempt");

    assert_eq!(response.content.as_deref(), Some("retry recovered"));
    assert_eq!(
        events.into_inner(),
        vec![
            ModelRetryRuntimePhase::Scheduled,
            ModelRetryRuntimePhase::AttemptStarted,
            ModelRetryRuntimePhase::Settled
        ]
    );

    for _ in 0..2 {
        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive every retry attempt");
        assert_eq!(recorded.path, "/v1/chat/completions");
    }
}

#[test]
fn model_provider_gate_caps_same_provider_at_internal_limit() {
    let gate = Arc::new(ModelProviderGate {
        slots: Mutex::new(HashMap::new()),
    });
    let permits = (0..MODEL_PROVIDER_MAX_IN_FLIGHT)
        .map(|_| gate.acquire("anthropic-messages|http://localhost:8317/v1|kiro"))
        .collect::<Vec<_>>();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let second_gate = Arc::clone(&gate);

    let handle = std::thread::spawn(move || {
        let _second_permit =
            second_gate.acquire("anthropic-messages|http://localhost:8317/v1|kiro");
        acquired_tx
            .send(())
            .expect("test receiver should still be alive");
    });

    assert!(
        acquired_rx
            .recv_timeout(Duration::from_millis(120))
            .is_err(),
        "同一 provider/model 达到并发上限后必须排队"
    );

    drop(permits);
    acquired_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("释放请求槽位后，下一个同端点请求应继续执行");
    handle.join().expect("waiting thread should not panic");
}

#[test]
fn model_provider_gate_allows_mainline_and_five_agents() {
    let gate = Arc::new(ModelProviderGate {
        slots: Mutex::new(HashMap::new()),
    });
    let existing_permits = (0..4)
        .map(|_| gate.acquire("openai-chat|http://localhost:8317/v1|gpt"))
        .collect::<Vec<_>>();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let handles = (0..2)
        .map(|_| {
            let gate = Arc::clone(&gate);
            let acquired_tx = acquired_tx.clone();
            std::thread::spawn(move || {
                let _permit = gate.acquire("openai-chat|http://localhost:8317/v1|gpt");
                acquired_tx
                    .send(())
                    .expect("test receiver should still be alive");
            })
        })
        .collect::<Vec<_>>();
    drop(acquired_tx);

    let acquired_concurrently =
        (0..2).all(|_| acquired_rx.recv_timeout(Duration::from_millis(250)).is_ok());
    drop(existing_permits);
    for handle in handles {
        handle.join().expect("waiting thread should not panic");
    }

    assert!(
        acquired_concurrently,
        "同一模型必须至少允许主线与五个子代理并发请求"
    );
}

#[test]
fn streaming_retries_before_first_delta_only() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 503,
            content_type: "application/json".to_string(),
            response_text: serde_json::json!({
                "error": {
                    "message": "provider warming up",
                    "type": "server_error",
                    "code": "unavailable",
                }
            })
            .to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
    ]);

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect("streaming retry should recover before any delta is emitted");

    assert!(response.is_actionable());
    assert_eq!(deltas.into_inner(), vec!["Hi".to_string()]);
    for _ in 0..2 {
        let recorded = server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive streaming retry attempts");
        assert_eq!(recorded.path, "/v1/chat/completions");
    }
}

#[test]
fn streaming_retries_incomplete_connection_before_first_delta() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: "data: {}\n\n".to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"reconnected\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
    ]);

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("首个可见增量前连接中断时应重连并继续");

    assert_eq!(response.content.as_deref(), Some("reconnected"));
    for _ in 0..2 {
        server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive reconnect attempts");
    }
}

#[test]
fn streaming_does_not_retry_after_visible_delta() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n"
                .to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"duplicate\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
    ]);
    let client = HttpModelBridgeClient::new(
        server.address,
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let deltas = std::cell::RefCell::new(Vec::new());

    client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|delta| deltas.borrow_mut().push(delta.content.clone()),
        )
        .expect_err("已经展示正文后不能从头重试并制造重复文本");

    assert_eq!(deltas.into_inner(), vec!["partial".to_string()]);
    server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("first request should be recorded");
    assert!(
        server
            .request_receiver
            .recv_timeout(Duration::from_millis(250))
            .is_err(),
        "可见增量后的失败不能发起第二次模型请求"
    );
}

#[test]
fn streaming_retries_transient_provider_error_event() {
    let server = spawn_mock_server_sequence(vec![
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: error\n",
                    "data: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"Our servers are currently overloaded. Please try again later.\"}}\n\n",
                )
                .to_string(),
            },
            MockHttpResponse {
                status: 200,
                content_type: "text/event-stream".to_string(),
                response_text: concat!(
                    "event: content_block_delta\n",
                    "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"recovered\"}}\n\n",
                    "event: message_stop\n",
                    "data: {}\n\n",
                )
                .to_string(),
            },
        ]);
    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );

    let response = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "anthropic".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect("overloaded SSE error 应在首个增量前重连");

    assert_eq!(response.content.as_deref(), Some("recovered"));
    for _ in 0..2 {
        server
            .request_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("mock server should receive reconnect attempts");
    }
}

#[test]
fn model_retry_policy_uses_short_exponential_backoff() {
    assert_eq!(MODEL_PROVIDER_MAX_RETRIES, 8);
    let delays = (1..=MODEL_PROVIDER_MAX_RETRIES)
        .map(|attempt| base_retry_delay(attempt).as_millis() as u64)
        .collect::<Vec<_>>();
    assert_eq!(
        delays,
        [500, 1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 30_000]
    );
    // 退避窗口约 1.5 分钟：能覆盖网关重启与上游短暂过载。
    assert!(delays.iter().sum::<u64>() >= 90_000);
}

#[test]
fn permanent_quota_exhaustion_is_not_retried_but_plain_rate_limits_are() {
    let quota = provider_http_status_error(
        429,
        r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota"}}"#,
        None,
    );
    assert!(!retryable_bridge_error(&quota), "额度耗尽等待不会恢复");
    let rate_limited = provider_http_status_error(
        429,
        r#"{"error":{"message":"Rate limit reached, retry in 2s","type":"rate_limit_error"}}"#,
        Some(Duration::from_secs(2)),
    );
    assert!(retryable_bridge_error(&rate_limited));
    assert!(!retryable_http_response(
        429,
        r#"{"error":{"type":"insufficient_quota"}}"#
    ));
    assert!(retryable_http_response(503, "{}"));
    assert!(!retryable_http_response(400, "{}"));
}

#[test]
fn transient_text_in_a_business_error_is_retried_but_other_business_errors_are_not() {
    let overloaded = BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        message: "provider rejected request: Our servers are currently Overloaded".to_string(),
    };
    assert!(retryable_bridge_error(&overloaded));
    let invalid = BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        message: "provider rejected request: invalid api key".to_string(),
    };
    assert!(!retryable_bridge_error(&invalid));
    let html = BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::RemoteBusiness,
        code: Some(-32006),
        message: "provider response invalid: expected event stream, body=<html>".to_string(),
    };
    assert!(
        !retryable_bridge_error(&html),
        "地址配错或登录页不是暂态故障"
    );
}

#[test]
fn timeouts_get_a_smaller_retry_budget_than_other_transient_failures() {
    let timeout = BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: "provider transport failed: operation timed out after 300 seconds".to_string(),
    };
    assert!(is_timeout_error(&timeout));
    const { assert!(MODEL_PROVIDER_TIMEOUT_RETRIES < MODEL_PROVIDER_MAX_RETRIES) };
    let reset = BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: "provider transport failed: connection reset by peer".to_string(),
    };
    assert!(!is_timeout_error(&reset));
}

#[test]
fn retry_delay_honors_retry_after_and_jitters_backoff() {
    assert_eq!(
        provider_retry_delay(1, Some(Duration::from_secs(7))),
        Duration::from_secs(7),
        "服务商给出 Retry-After 时必须遵守"
    );
    let delays = (0..64)
        .map(|_| provider_retry_delay(3, None))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        delays.len() > 1,
        "退避必须带随机抖动，避免多个子代理同步重试"
    );
    assert!(delays.iter().all(|delay| {
        (Duration::from_millis(1_500)..=Duration::from_millis(2_500)).contains(delay)
    }));
    let error = provider_http_status_error(429, "{}", Some(Duration::from_secs(3)));
    assert_eq!(
        provider_error_retry_after(&error),
        Some(Duration::from_secs(3))
    );
}

#[test]
fn streaming_retry_reports_runtime_lifecycle() {
    let server = spawn_mock_server_sequence(vec![
        MockHttpResponse {
            status: 503,
            content_type: "application/json".to_string(),
            response_text: serde_json::json!({
                "error": {
                    "message": "provider warming up",
                    "type": "server_error"
                }
            })
            .to_string(),
        },
        MockHttpResponse {
            status: 200,
            content_type: "text/event-stream".to_string(),
            response_text: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        },
    ]);
    let client = HttpModelBridgeClient::new(
        server.address,
        Some("sk-test-key".to_string()),
        "gpt-4.1-mini".to_string(),
    );
    let events = std::cell::RefCell::new(Vec::new());

    client
        .invoke_streaming_with_retry_events(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "openai-compatible".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
            &|event| events.borrow_mut().push(event.clone()),
        )
        .expect("retry should recover");

    let mut events = events.into_inner();
    // 退避带 ±25% 抖动：首轮 500ms 的基础等待落在 375..=625ms。
    let scheduled_delay = events[0].delay_ms.take().expect("scheduled retry delay");
    assert!((375..=625).contains(&scheduled_delay), "{scheduled_delay}");
    assert_eq!(
        events,
        vec![
            ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::Scheduled,
                attempt: 1,
                max_attempts: 8,
                delay_ms: None,
            },
            ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::AttemptStarted,
                attempt: 1,
                max_attempts: 8,
                delay_ms: None,
            },
            ModelRetryRuntimeEvent {
                phase: ModelRetryRuntimePhase::Settled,
                attempt: 1,
                max_attempts: 8,
                delay_ms: None,
            },
        ]
    );
}

#[test]
fn retry_policy_rejects_permanent_provider_failures() {
    assert!(!retryable_http_status(400));
    assert!(!retryable_http_status(401));
    assert!(!retryable_http_status(403));
    assert!(!retryable_http_status(404));
    assert!(retryable_http_status(408));
    assert!(retryable_http_status(429));
    assert!(retryable_http_status(500));
    assert!(retryable_http_status(529));
}

#[test]
fn retry_after_supports_seconds_http_dates_and_sixty_second_cap() {
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
    assert_eq!(parse_retry_after("12", now), Some(Duration::from_secs(12)));
    assert_eq!(parse_retry_after("120", now), Some(Duration::from_secs(60)));
    let http_date = httpdate::fmt_http_date(now + Duration::from_secs(30));
    assert_eq!(
        parse_retry_after(&http_date, now),
        Some(Duration::from_secs(30))
    );
    assert_eq!(parse_retry_after("invalid", now), None);
}

#[test]
fn streaming_surfaces_anthropic_error_event() {
    let server = spawn_mock_server_with_response_text(
            200,
            "text/event-stream",
            concat!(
                "event: error\n",
                "data: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}\n\n",
            )
            .to_string(),
        );
    let client = HttpModelBridgeClient::new_with_protocol(
        server.address,
        Some("bad-key".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );

    let error = client
        .invoke_streaming(
            ModelInvocationRequest {
                cache_affinity_key: None,
                prompt_cache_key: None,
                provider: "anthropic".to_string(),
                prompt: "hello".to_string(),
                messages: None,
                tools: None,
                tool_choice: None,
            },
            &|_| {},
        )
        .expect_err("SSE error event must fail the invocation");

    assert!(error.to_string().contains("invalid x-api-key"));
    assert_eq!(error.code(), Some(-32006));
}

#[test]
fn invoke_against_mock_server_returns_structured_payload_with_usage() {
    let server = spawn_mock_server(
        200,
        serde_json::json!({
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8,
            },
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": "structured reply"
                }
            }]
        }),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test".to_string()),
        "gpt-4".to_string(),
    );

    let response = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect("invoke should succeed");

    assert!(response.is_actionable());
    assert_eq!(response.content.as_deref(), Some("structured reply"));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    let usage = response.usage.expect("usage should exist");
    assert_eq!(usage["inputTokens"], 5);
    assert_eq!(usage["outputTokens"], 3);
}

#[test]
fn invoke_against_mock_server_uses_anthropic_messages_protocol_when_configured() {
    let server = spawn_mock_server(
        200,
        serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "anthropic reply"
            }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 9,
                "output_tokens": 5,
                "cache_read_input_tokens": 2
            }
        }),
    );

    let client = HttpModelBridgeClient::new_with_protocol(
        server.address.clone(),
        Some("sk-ant-test".to_string()),
        "claude-sonnet-test".to_string(),
        HttpModelBridgeProtocol::AnthropicMessages,
        None,
    );

    let response = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "anthropic".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect("invoke should succeed");

    assert!(response.is_actionable());
    assert_eq!(response.content.as_deref(), Some("anthropic reply"));
    assert_eq!(response.finish_reason.as_deref(), Some("end_turn"));
    let usage = response.usage.expect("usage should exist");
    assert_eq!(usage["inputTokens"], 9);
    assert_eq!(usage["outputTokens"], 5);
    assert_eq!(usage["cacheReadTokens"], 2);

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mock server should receive request");
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.authorization, None);
    assert_eq!(recorded.x_api_key.as_deref(), Some("sk-ant-test"));
    assert_eq!(recorded.anthropic_version.as_deref(), Some("2023-06-01"));

    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("body should be json");
    assert_eq!(body["model"], "claude-sonnet-test");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(body["stream"], false);
}

#[test]
fn invoke_against_mock_server_handles_rejection() {
    let server = spawn_mock_server(
        400,
        serde_json::json!({
            "error": {
                "message": "bad request",
                "type": "invalid_request_error",
                "code": "invalid_request",
            }
        }),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        Some("sk-test".to_string()),
        "gpt-4".to_string(),
    );

    let error = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect_err("rejected request should return error");

    match error {
        BridgeClientError::HttpStatusFailed {
            layer,
            code,
            http_status,
            message,
            ..
        } => {
            assert_eq!(layer, BridgeErrorLayer::RemoteBusiness);
            assert_eq!(code, Some(-32006));
            assert_eq!(http_status, 400);
            assert!(message.contains("bad request"), "message was: {message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn invoke_against_unreachable_server_returns_transport_error() {
    // Bind and immediately drop to get an unreachable port.
    let address = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind should succeed");
        let addr = listener.local_addr().expect("address should exist");
        drop(listener);
        addr
    };

    let client = HttpModelBridgeClient::new(
        format!("http://{address}/v1"),
        Some("sk-test".to_string()),
        "gpt-4".to_string(),
    );

    let error = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect_err("unreachable server should return error");

    match error {
        BridgeClientError::CallFailed { layer, .. } => {
            assert_eq!(layer, BridgeErrorLayer::Transport);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn invoke_without_api_key_omits_authorization_header() {
    let server = spawn_mock_server(
        200,
        serde_json::json!({
            "choices": [{
                "message": {
                    "content": "no auth needed"
                }
            }]
        }),
    );

    let client = HttpModelBridgeClient::new(
        server.address.clone(),
        None, // no API key
        "local-model".to_string(),
    );

    let response = client
        .invoke(ModelInvocationRequest {
            cache_affinity_key: None,
            prompt_cache_key: None,
            provider: "openai".to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
            tool_choice: None,
        })
        .expect("invoke should succeed without API key");

    assert!(response.is_actionable());
    assert_eq!(response.content.as_deref(), Some("no auth needed"));

    let recorded = server
        .request_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("should receive request");
    assert!(
        recorded.authorization.is_none(),
        "authorization header should be absent"
    );
}

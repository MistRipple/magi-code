use magi_bridge_client::{
    BridgeServerKind, BridgeTransport, BridgeTransportError, BridgeTransportRequest,
    JsonRpcBridgeServerProbeClient, JsonRpcModelBridgeClient, JsonRpcStdioTransport,
    ModelBridgeClient, ModelInvocationRequest, SHADOW_MODEL_PROVIDER,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

fn loopback_transport() -> JsonRpcStdioTransport {
    let mut path = std::env::current_exe().expect("current exe should exist");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("model_bridge_loopback");
    JsonRpcStdioTransport::new(path.to_string_lossy().to_string())
}

fn loopback_transport_with_env(envs: &[(&str, &str)]) -> JsonRpcStdioTransport {
    envs.iter()
        .fold(loopback_transport(), |transport, (key, value)| {
            transport.with_env(*key, *value)
        })
}

#[derive(Debug)]
struct RecordedHttpRequest {
    request_line: String,
    headers: BTreeMap<String, String>,
    body: String,
}

fn spawn_http_stub(
    status: u16,
    response_body: Value,
) -> (String, mpsc::Receiver<RecordedHttpRequest>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("http stub should bind");
    let address = listener.local_addr().expect("http stub addr should exist");
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("http stub should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout should set");
        let request = read_http_request(&mut stream);
        let body = response_body.to_string();
        let response = format!(
            "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            status_reason(status),
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("http stub should write");
        stream.flush().expect("http stub should flush");
        sender.send(request).expect("request should send to test");
    });

    (format!("http://{address}/v1"), receiver, handle)
}

fn read_http_request(stream: &mut TcpStream) -> RecordedHttpRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).expect("http request should read");
        assert!(read > 0, "http request should include headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_header_end(&buffer) {
            break index + 4;
        }
    };

    let header_text =
        String::from_utf8(buffer[..header_end].to_vec()).expect("headers should be utf-8");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("request line should exist").to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .expect("header should contain separator");
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .expect("content-length should exist")
        .parse::<usize>()
        .expect("content-length should parse");
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).expect("http body should read");
        assert!(read > 0, "http request should include body");
        buffer.extend_from_slice(&chunk[..read]);
    }

    let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
        .expect("request body should be utf-8");
    RecordedHttpRequest {
        request_line,
        headers,
        body,
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "Test Response",
    }
}

#[test]
fn model_client_round_trips_through_loopback_server() {
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport()));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: SHADOW_MODEL_PROVIDER.to_string(),
            prompt: "hello".to_string(),
            messages: None,
            tools: None,
        })
        .expect("loopback model invoke should succeed");

    assert!(response.ok);
    assert_eq!(response.payload, "shadow-model::hello");
}

#[test]
fn openai_compatible_provider_executes_real_http_smoke_path() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "choices": [{
                "message": {
                    "content": "hello from stub",
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("openai-compatible HTTP smoke invoke should succeed");

    assert!(response.ok);
    assert_eq!(response.payload, "hello from stub");

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer test-key")
    );

    let body: Value = serde_json::from_str(&request.body).expect("request body should be json");
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "say hi");
    assert_eq!(body["stream"], false);
}

#[test]
fn openai_compatible_provider_surfaces_structured_success_payload() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 5,
                "total_tokens": 17,
            },
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "hello from stub",
                    "tool_calls": [{
                        "id": "call_stub_1",
                        "type": "function",
                        "function": {
                            "name": "demo.lookup",
                            "arguments": "{\"topic\":\"bridge\"}",
                        }
                    }]
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("structured openai-compatible payload should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["content"], "hello from stub");
    assert_eq!(payload["finish_reason"], "tool_calls");
    assert_eq!(payload["usage"]["total_tokens"], 17);
    assert_eq!(payload["tool_calls"][0]["id"], "call_stub_1");
    assert_eq!(payload["tool_calls"][0]["function"]["name"], "demo.lookup");

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_accepts_tool_call_only_success_payload() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "usage": {
                "total_tokens": 7,
            },
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "tool_calls": [{
                        "id": "call_stub_lookup_1",
                        "type": "function",
                        "function": {
                            "name": "demo.lookup",
                            "arguments": "{\"topic\":\"bridge\"}",
                        }
                    }]
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("tool-call-only openai-compatible payload should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert!(payload.get("content").is_none());
    assert_eq!(payload["finish_reason"], "tool_calls");
    assert_eq!(payload["usage"]["total_tokens"], 7);
    assert_eq!(payload["tool_calls"][0]["id"], "call_stub_lookup_1");
    assert_eq!(payload["tool_calls"][0]["function"]["name"], "demo.lookup");
    assert_eq!(
        payload["tool_calls"][0]["function"]["arguments"],
        "{\"topic\":\"bridge\"}"
    );

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_surfaces_refusal_only_payload() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "usage": {
                "total_tokens": 11,
            },
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "refusal": "I can't help with that request."
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("refusal-only openai-compatible payload should succeed");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["content"], "I can't help with that request.");
    assert_eq!(payload["finish_reason"], "stop");
    assert_eq!(payload["usage"]["total_tokens"], 11);
    assert!(payload.get("tool_calls").is_none());

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_prefers_refusal_when_content_parts_are_empty() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "usage": {
                "total_tokens": 9,
            },
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": [{
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.test/mock.png",
                        }
                    }],
                    "refusal": "I can't comply with that request."
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("empty content parts should fall back to refusal");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    assert_eq!(payload["content"], "I can't comply with that request.");
    assert_eq!(payload["finish_reason"], "stop");
    assert_eq!(payload["usage"]["total_tokens"], 9);

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_tolerates_structured_tool_call_arguments() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "tool_calls": [{
                        "id": "call_stub_lookup_structured",
                        "type": "function",
                        "function": {
                            "name": "demo.lookup",
                            "arguments": {
                                "topic": "bridge",
                                "filters": ["stable", "provider"]
                            },
                        }
                    }]
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("structured tool arguments should survive round-trip");

    assert!(response.ok);
    let payload: Value = serde_json::from_str(&response.payload).expect("payload should be json");
    let arguments = payload["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("tool arguments should remain serialized as a string");
    assert_eq!(
        serde_json::from_str::<Value>(arguments).expect("tool arguments should stay valid json"),
        json!({
            "topic": "bridge",
            "filters": ["stable", "provider"]
        })
    );

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_flattens_content_parts_without_structured_metadata() {
    let (base_url, receiver, handle) = spawn_http_stub(
        200,
        json!({
            "choices": [{
                "message": {
                    "content": [{
                        "type": "text",
                        "text": "hello ",
                    }, {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.test/mock.png",
                        }
                    }, {
                        "type": "output_text",
                        "text": "from parts",
                    }]
                }
            }]
        }),
    );
    let client = JsonRpcModelBridgeClient::new(Arc::new(loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ])));

    let response = client
        .invoke(ModelInvocationRequest {
            provider: "openai-compatible".to_string(),
            prompt: "say hi".to_string(),
            messages: None,
            tools: None,
        })
        .expect("content parts without usage/finish_reason should still succeed");

    assert!(response.ok);
    assert_eq!(response.payload, "hello from parts");

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
}

#[test]
fn openai_compatible_provider_surfaces_upstream_http_errors() {
    let (base_url, receiver, handle) = spawn_http_stub(
        401,
        json!({
            "error": {
                "message": "bad api key",
                "type": "invalid_request_error",
                "code": "invalid_api_key",
            }
        }),
    );
    let transport = loopback_transport_with_env(&[
        ("MAGI_OPENAI_COMPAT_BASE_URL", base_url.as_str()),
        ("MAGI_OPENAI_COMPAT_API_KEY", "test-key"),
        ("MAGI_OPENAI_COMPAT_MODEL", "gpt-test"),
    ]);
    let error = transport
        .call(BridgeTransportRequest {
            method: "model.invoke".to_string(),
            params: json!({
                "provider": "openai-compatible",
                "prompt": "say hi"
            }),
        })
        .expect_err("upstream HTTP errors should remain remote business errors");

    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("http stub should receive request");
    handle.join().expect("http stub should join");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");

    match error {
        BridgeTransportError::RemoteBusiness {
            code,
            message,
            data,
        } => {
            assert_eq!(code, -32006);
            assert_eq!(message, "provider rejected request");
            let data = data.expect("error data should exist");
            assert_eq!(data["http_status"], 401);
            assert_eq!(data["upstream_message"], "bad api key");
            assert_eq!(data["upstream_type"], "invalid_request_error");
            assert_eq!(data["upstream_code"], "invalid_api_key");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn model_loopback_exposes_shared_handshake_and_health() {
    let probe = JsonRpcBridgeServerProbeClient::new(Arc::new(loopback_transport()));

    let handshake = probe.handshake().expect("model handshake should succeed");
    assert_eq!(handshake.server_kind, BridgeServerKind::Model);
    assert!(
        handshake
            .supported_methods
            .contains(&"model.invoke".to_string())
    );
    assert!(
        handshake
            .supported_methods
            .contains(&"bridge.handshake".to_string())
    );

    let health = probe.health().expect("model health should succeed");
    assert_eq!(health.server_kind, BridgeServerKind::Model);
    assert!(health.ok);

    let catalog = probe
        .describe_services()
        .expect("model service catalog should succeed");
    assert_eq!(catalog.server_kind, BridgeServerKind::Model);
    assert_eq!(catalog.services.len(), 2);
    assert!(
        catalog
            .services
            .iter()
            .any(|service| service.service_name == SHADOW_MODEL_PROVIDER)
    );
    assert!(
        catalog
            .services
            .iter()
            .any(|service| service.service_name == "openai-compatible")
    );
    assert!(catalog.services.iter().all(|service| {
        service
            .supported_operations
            .contains(&"invoke_prompt".to_string())
    }));
}

#[test]
fn unsupported_method_returns_protocol_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "model.not_supported".to_string(),
            params: json!({
                "provider": SHADOW_MODEL_PROVIDER,
                "prompt": "hello"
            }),
        })
        .expect_err("unsupported method should return protocol error");

    assert!(matches!(error, BridgeTransportError::Protocol { .. }));
}

#[test]
fn unknown_provider_returns_remote_business_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "model.invoke".to_string(),
            params: json!({
                "provider": "anthropic",
                "prompt": "hello"
            }),
        })
        .expect_err("unknown provider should return remote business error");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32001);
            assert_eq!(message, "unknown provider");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn empty_prompt_returns_remote_business_error() {
    let transport = loopback_transport();
    let error = transport
        .call(BridgeTransportRequest {
            method: "model.invoke".to_string(),
            params: json!({
                "provider": SHADOW_MODEL_PROVIDER,
                "prompt": "   "
            }),
        })
        .expect_err("empty prompt should return remote business error");

    match error {
        BridgeTransportError::RemoteBusiness { code, message, .. } => {
            assert_eq!(code, -32002);
            assert_eq!(message, "empty prompt");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn broken_subprocess_returns_transport_error() {
    let transport =
        JsonRpcStdioTransport::new("sh").with_args(vec!["-c".to_string(), "exit 2".to_string()]);

    let error = transport
        .call(BridgeTransportRequest {
            method: "model.invoke".to_string(),
            params: json!({
                "provider": SHADOW_MODEL_PROVIDER,
                "prompt": "hello"
            }),
        })
        .expect_err("non-zero exit should be transport error");

    assert!(matches!(error, BridgeTransportError::Transport { .. }));
}

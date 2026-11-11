use crate::{
    BridgeBindingDispatchPlan, BridgeBindingKind, BridgeBindingReference, BridgeClientError,
    BridgeDispatchAction, BridgeDispatchInput, BridgeDispatchRuntime, HttpImageGenerationClient,
    ImageGenerationRequest, McpBridgeClient, McpToolCallRequest, ModelBridgeClient,
    ModelInvocationRequest,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

#[derive(Default)]
struct RecordingMcpClient {
    calls: Mutex<Vec<McpToolCallRequest>>,
}

impl McpBridgeClient for RecordingMcpClient {
    fn call_tool(
        &self,
        request: McpToolCallRequest,
    ) -> Result<crate::BridgeResponse, BridgeClientError> {
        self.calls.lock().unwrap().push(request);
        Ok(crate::BridgeResponse {
            ok: true,
            payload: "dispatch".into(),
        })
    }
}

struct RecordingModelClient {
    response: crate::ModelResponse,
    calls: Mutex<Vec<ModelInvocationRequest>>,
}

impl RecordingModelClient {
    fn new(response: Value) -> Self {
        Self {
            response: serde_json::from_value(response).unwrap(),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl ModelBridgeClient for RecordingModelClient {
    fn executor_capabilities(&self) -> crate::ExecutorCapabilities {
        crate::text_executor_capabilities()
    }

    fn invoke(
        &self,
        request: ModelInvocationRequest,
    ) -> Result<crate::ModelResponse, BridgeClientError> {
        self.calls.lock().unwrap().push(request);
        Ok(self.response.clone())
    }

    fn invoke_streaming(
        &self,
        request: ModelInvocationRequest,
        _on_delta: &dyn Fn(&crate::ModelStreamingDelta),
    ) -> Result<crate::ModelResponse, BridgeClientError> {
        self.invoke(request)
    }
}

#[test]
fn incompatible_kind_action_is_rejected() {
    let runtime = BridgeDispatchRuntime::new();
    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec!["skill-a".to_string()],
        bindings: vec![BridgeBindingReference {
            binding_id: "binding-a".to_string(),
            tool_name: "model.prompt".to_string(),
            bridge_kind: BridgeBindingKind::Model,
            dispatch_action: BridgeDispatchAction::McpToolCall,
            bridge_target: "openai".to_string(),
        }],
    };

    let error = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "binding-a".to_string(),
                payload: "hi".to_string(),
                working_directory: None,
            },
        )
        .expect_err("incompatible binding/action should be rejected");

    match error {
        BridgeClientError::IncompatibleBindingAction {
            binding_id,
            bridge_kind,
            dispatch_action,
        } => {
            assert_eq!(binding_id, "binding-a");
            assert_eq!(bridge_kind, BridgeBindingKind::Model);
            assert_eq!(dispatch_action, BridgeDispatchAction::McpToolCall);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn dispatch_runtime_routes_model_and_mcp_bindings() {
    let mcp_client = Arc::new(RecordingMcpClient::default());

    let runtime = BridgeDispatchRuntime::new()
        .with_model_client(Arc::new(RecordingModelClient::new(
            serde_json::to_value(crate::ModelResponse::completed("dispatch")).unwrap(),
        )))
        .with_mcp_client(mcp_client.clone());

    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec!["skill-a".to_string()],
        bindings: vec![
            BridgeBindingReference {
                binding_id: "model-binding".to_string(),
                tool_name: "model.prompt".to_string(),
                bridge_kind: BridgeBindingKind::Model,
                dispatch_action: BridgeDispatchAction::ModelPrompt,
                bridge_target: "openai".to_string(),
            },
            BridgeBindingReference {
                binding_id: "mcp-binding".to_string(),
                tool_name: "mcp.call".to_string(),
                bridge_kind: BridgeBindingKind::Mcp,
                dispatch_action: BridgeDispatchAction::McpToolCall,
                bridge_target: "server-a".to_string(),
            },
        ],
    };

    let model = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "model-binding".to_string(),
                payload: "hello".to_string(),
                working_directory: None,
            },
        )
        .expect("model dispatch should succeed");
    assert_eq!(model.response.payload(), "dispatch");

    let mcp = runtime
        .dispatch(
            &plan,
            BridgeDispatchInput {
                binding_id: "mcp-binding".to_string(),
                payload: "{}".to_string(),
                working_directory: None,
            },
        )
        .expect("mcp dispatch should succeed");
    assert_eq!(mcp.response.payload(), "dispatch");
    let calls = mcp_client.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].server_name, "server-a");
    assert_eq!(calls[0].tool_name, "mcp.call");
    assert_eq!(calls[0].input, "{}");
}

#[test]
fn dispatch_runtime_enforces_declared_executor_response_contract() {
    let plan = BridgeBindingDispatchPlan {
        source_skill_ids: vec![],
        bindings: vec![BridgeBindingReference {
            binding_id: "model-binding".into(),
            tool_name: "model.prompt".into(),
            bridge_kind: BridgeBindingKind::Model,
            dispatch_action: BridgeDispatchAction::ModelPrompt,
            bridge_target: "loopback".into(),
        }],
    };
    for (response, code) in [
        (
            json!({"status":"requires_tool_execution", "content":"probe"}),
            "executor_native_tools_unsupported",
        ),
        (
            json!({"status":"incomplete", "content":"partial"}),
            "executor_incomplete_response",
        ),
        (
            serde_json::to_value(crate::ModelResponse::completed("")).unwrap(),
            "executor_incomplete_response",
        ),
    ] {
        let client = Arc::new(RecordingModelClient::new(response));
        let runtime = BridgeDispatchRuntime::new().with_model_client(client.clone());
        let error = runtime
            .dispatch(
                &plan,
                BridgeDispatchInput {
                    binding_id: "model-binding".into(),
                    payload: "hello".into(),
                    working_directory: None,
                },
            )
            .expect_err("dispatch must reject responses outside the executor contract");
        assert!(error.to_string().contains(code), "{error}");
        assert_eq!(
            client.calls.lock().unwrap().len(),
            1,
            "invalid responses must not be retried"
        );
    }
}

// ============================================================================
// Phase 4: Tool Concurrency Tests
// ============================================================================

#[test]
fn tool_concurrency_read_only_safe() {
    use crate::tool_concurrency::{ToolConcurrencyInput, is_concurrency_safe_call};
    for (names, expected) in [
        (
            &[
                "file_read",
                "view_image",
                "diff_preview",
                "search_text",
                "search_semantic",
                "knowledge_query",
                "knowledge_graph_query",
                "code_symbols",
                "tool_catalog",
                "web_search",
            ][..],
            true,
        ),
        (
            &[
                "file_view",
                "code_search_regex",
                "project_knowledge_query",
                "shell_exec",
                "process_launch",
                "file_edit",
            ][..],
            false,
        ),
    ] {
        for tool_name in names {
            assert_eq!(
                is_concurrency_safe_call(&ToolConcurrencyInput {
                    tool_name,
                    arguments: None,
                }),
                expected,
                "{tool_name}"
            );
        }
    }
    assert!(is_concurrency_safe_call(&ToolConcurrencyInput {
        tool_name: "shell_exec",
        arguments: Some(&json!({ "command": "printf probe", "access_mode": "read_only" })),
    }));
}

#[test]
fn tool_concurrency_partition_mixed() {
    let tools = [
        "file_read",
        "search_text",
        "file_write",
        "search_semantic",
        "shell_exec",
    ];
    let calls = tools.map(|tool_name| crate::tool_concurrency::ToolConcurrencyInput {
        tool_name,
        arguments: None,
    });
    let batches = crate::tool_concurrency::partition_tool_calls_with_inputs(&calls);
    assert_eq!(batches.len(), 4);
    assert!(matches!(
        batches[0].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[0].tool_indices, vec![0, 1]);
    assert!(matches!(
        batches[1].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[1].tool_indices, vec![2]);
    assert!(matches!(
        batches[2].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[2].tool_indices, vec![3]);
    assert!(matches!(
        batches[3].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[3].tool_indices, vec![4]);
}

#[test]
fn tool_concurrency_partitions_read_only_shells_concurrently() {
    let shell_a = json!({ "command": "printf a", "access_mode": "read_only" });
    let shell_b = json!({ "command": "printf b", "access_mode": "read_only" });
    let tools = [
        crate::tool_concurrency::ToolConcurrencyInput {
            tool_name: "shell_exec",
            arguments: Some(&shell_a),
        },
        crate::tool_concurrency::ToolConcurrencyInput {
            tool_name: "shell_exec",
            arguments: Some(&shell_b),
        },
    ];
    let batches = crate::tool_concurrency::partition_tool_calls_with_inputs(&tools);
    assert_eq!(batches.len(), 1);
    assert!(matches!(
        batches[0].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[0].tool_indices, vec![0, 1]);
}

#[test]
fn tool_concurrency_does_not_trust_read_only_shell_with_write_redirection() {
    let read_only_shell = json!({ "command": "printf a", "access_mode": "read_only" });
    let write_shell = json!({ "command": "printf b > out.txt", "access_mode": "read_only" });
    let tools = [
        crate::tool_concurrency::ToolConcurrencyInput {
            tool_name: "shell_exec",
            arguments: Some(&read_only_shell),
        },
        crate::tool_concurrency::ToolConcurrencyInput {
            tool_name: "shell_exec",
            arguments: Some(&write_shell),
        },
    ];
    let batches = crate::tool_concurrency::partition_tool_calls_with_inputs(&tools);

    assert_eq!(batches.len(), 2);
    assert!(matches!(
        batches[0].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[0].tool_indices, vec![0]);
    assert!(matches!(
        batches[1].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[1].tool_indices, vec![1]);
}

#[test]
fn tool_concurrency_keeps_agent_spawn_serial() {
    let tools = ["agent_spawn", "agent_spawn", "file_read"];
    let calls = tools.map(|tool_name| crate::tool_concurrency::ToolConcurrencyInput {
        tool_name,
        arguments: None,
    });
    let batches = crate::tool_concurrency::partition_tool_calls_with_inputs(&calls);

    assert_eq!(batches.len(), 3);
    assert!(matches!(
        batches[0].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[0].tool_indices, vec![0]);
    assert!(matches!(
        batches[1].kind,
        crate::tool_concurrency::ToolBatchKind::Serial
    ));
    assert_eq!(batches[1].tool_indices, vec![1]);
    assert!(matches!(
        batches[2].kind,
        crate::tool_concurrency::ToolBatchKind::Concurrent
    ));
    assert_eq!(batches[2].tool_indices, vec![2]);
}

// ============================================================================
// Phase 4: LLM Types Tests
// ============================================================================

#[test]
fn sanitize_tool_order_removes_orphan_tool_results() {
    use crate::llm_types::*;
    let messages = vec![
        LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Text("hello".into()),
        },
        LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "orphan".into(),
                content: "result".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        },
    ];

    let sanitized = sanitize_tool_order(&messages);
    assert_eq!(sanitized.len(), 1);
    assert_eq!(sanitized[0].role, "user");
}

#[test]
fn sanitize_tool_order_preserves_valid_pairs() {
    use crate::llm_types::*;
    let messages = vec![
        LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Text("hello".into()),
        },
        LlmMessage {
            origin: Default::default(),
            role: "assistant".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolUse {
                id: "t1".into(),
                name: "file_read".into(),
                input: serde_json::json!({}),
            }]),
        },
        LlmMessage {
            origin: Default::default(),
            role: "user".into(),
            content: LlmMessageContent::Blocks(vec![LlmContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "file content".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        },
    ];

    let sanitized = sanitize_tool_order(&messages);
    assert_eq!(sanitized.len(), 3);
}

#[test]
fn image_generation_client_builds_standard_openai_request_and_parses_base64_result() {
    let client = HttpImageGenerationClient::new(
        "http://127.0.0.1:8317/v1".to_string(),
        Some("sk-image-test".to_string()),
        "gpt-image-1".to_string(),
    );
    let request = ImageGenerationRequest {
        prompt: "一张极简的蓝色方块".to_string(),
        size: "1024x1024".to_string(),
        quality: Some("high".to_string()),
    };

    let built = client
        .build_request_for_test(&request)
        .expect("image request should build");
    assert_eq!(built.url, "http://127.0.0.1:8317/v1/images/generations");
    assert_eq!(built.body["model"], "gpt-image-1");
    assert_eq!(built.body["prompt"], "一张极简的蓝色方块");
    assert_eq!(built.body["size"], "1024x1024");
    assert_eq!(built.body["quality"], "high");
    assert_eq!(built.body["response_format"], "b64_json");
    assert!(
        built
            .headers
            .iter()
            .any(|(name, value)| { name == "Authorization" && value == "Bearer sk-image-test" })
    );

    let result = HttpImageGenerationClient::parse_response_for_test(
        &serde_json::json!({
            "created": 123,
            "usage": {
                "input_tokens": 18,
                "output_tokens": 32,
                "total_tokens": 50
            },
            "data": [{
                "b64_json": "iVBORw0KGgo=",
                "revised_prompt": "一个蓝色方块"
            }]
        })
        .to_string(),
    )
    .expect("base64 image response should parse");
    assert_eq!(result.bytes, b"\x89PNG\r\n\x1a\n");
    assert_eq!(result.media_type, "image/png");
    assert_eq!(result.revised_prompt.as_deref(), Some("一个蓝色方块"));
    assert_eq!(
        result
            .usage
            .as_ref()
            .and_then(|usage| usage["total_tokens"].as_u64()),
        Some(50)
    );
}

#[test]
fn image_generation_client_maps_grok_dimensions_to_xai_request_fields() {
    let client = HttpImageGenerationClient::new(
        "https://api.x.ai/v1".to_string(),
        Some("xai-image-test".to_string()),
        "grok-imagine-image-quality".to_string(),
    );
    let request = ImageGenerationRequest {
        prompt: "A cinematic mountain landscape".to_string(),
        size: "1536x1024".to_string(),
        quality: Some("high".to_string()),
    };

    let built = client
        .build_request_for_test(&request)
        .expect("xAI image request should build");
    assert_eq!(built.url, "https://api.x.ai/v1/images/generations");
    assert_eq!(built.body["model"], "grok-imagine-image-quality");
    assert_eq!(built.body["prompt"], "A cinematic mountain landscape");
    assert_eq!(built.body["n"], 1);
    assert_eq!(built.body["response_format"], "b64_json");
    assert_eq!(built.body["aspect_ratio"], "3:2");
    assert_eq!(built.body["resolution"], "2k");
    assert!(built.body.get("size").is_none());
    assert!(built.body.get("quality").is_none());
}

#[test]
fn image_generation_client_downloads_url_response_without_forwarding_api_key() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("image URL mock server should bind");
    let address = listener
        .local_addr()
        .expect("image URL mock server address should be available");
    let server = thread::spawn(move || {
        let (mut generation_stream, _) = listener
            .accept()
            .expect("image URL mock server should accept generation request");
        let mut generation_request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = generation_stream
                .read(&mut buffer)
                .expect("generation request should be readable");
            assert!(read > 0, "generation request ended before headers");
            generation_request.extend_from_slice(&buffer[..read]);
            if let Some(position) = generation_request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&generation_request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("generation request should include content length");
        while generation_request.len() < header_end + content_length {
            let read = generation_stream
                .read(&mut buffer)
                .expect("generation request body should be readable");
            assert!(read > 0, "generation request ended before body");
            generation_request.extend_from_slice(&buffer[..read]);
        }
        let response_body = serde_json::json!({
            "data": [{
                "url": format!("http://{address}/generated.png"),
                "revised_prompt": "a downloaded blue square"
            }]
        })
        .to_string();
        write!(
            generation_stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body,
        )
        .expect("generation response should be writable");
        drop(generation_stream);

        let (mut image_stream, _) = listener
            .accept()
            .expect("image URL mock server should accept image download");
        let mut image_request = Vec::new();
        loop {
            let read = image_stream
                .read(&mut buffer)
                .expect("image download request should be readable");
            assert!(read > 0, "image download request ended before headers");
            image_request.extend_from_slice(&buffer[..read]);
            if image_request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let image_headers = String::from_utf8_lossy(&image_request);
        assert!(image_headers.starts_with("GET /generated.png HTTP/1.1"));
        assert!(
            !image_headers
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        let image_bytes = b"\x89PNG\r\n\x1a\n";
        write!(
            image_stream,
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            image_bytes.len(),
        )
        .expect("image response headers should be writable");
        image_stream
            .write_all(image_bytes)
            .expect("image response body should be writable");
    });

    let client = HttpImageGenerationClient::new(
        format!("http://{address}/v1"),
        Some("secret-image-key".to_string()),
        "grok-imagine-image".to_string(),
    );
    let generated = client
        .generate(ImageGenerationRequest {
            prompt: "draw a blue square".to_string(),
            size: "1024x1024".to_string(),
            quality: None,
        })
        .expect("URL image response should be downloaded");
    server.join().expect("image URL mock server should finish");
    assert_eq!(generated.media_type, "image/png");
    assert_eq!(generated.bytes, b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        generated.revised_prompt.as_deref(),
        Some("a downloaded blue square")
    );
}

#[test]
fn image_generation_client_performs_real_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("image mock server should bind");
    let address = listener
        .local_addr()
        .expect("image mock server address should be available");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("image mock server should accept the request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream
                .read(&mut buffer)
                .expect("image mock request should be readable");
            assert!(read > 0, "image mock request ended before headers");
            request.extend_from_slice(&buffer[..read]);
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("image request should include content length");
        while request.len() < header_end + content_length {
            let read = stream
                .read(&mut buffer)
                .expect("image mock request body should be readable");
            assert!(read > 0, "image mock request ended before body");
            request.extend_from_slice(&buffer[..read]);
        }
        let body = String::from_utf8_lossy(&request[header_end..header_end + content_length]);
        assert!(body.contains("\"model\":\"gpt-image-test\""));
        assert!(body.contains("\"prompt\":\"draw a blue square\""));
        assert!(body.contains("\"response_format\":\"b64_json\""));

        let response_body = serde_json::json!({
            "data": [{
                "b64_json": "iVBORw0KGgo=",
                "revised_prompt": "a blue square"
            }]
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body,
        )
        .expect("image mock response should be writable");
    });

    let client = HttpImageGenerationClient::new(
        format!("http://{address}/v1"),
        Some("sk-image-test".to_string()),
        "gpt-image-test".to_string(),
    );
    let generated = client
        .generate(ImageGenerationRequest {
            prompt: "draw a blue square".to_string(),
            size: "1024x1024".to_string(),
            quality: None,
        })
        .expect("image generation client should call the HTTP endpoint");
    server.join().expect("image mock server should finish");
    assert_eq!(generated.media_type, "image/png");
    assert_eq!(generated.bytes, b"\x89PNG\r\n\x1a\n");
    assert_eq!(generated.revised_prompt.as_deref(), Some("a blue square"));
    assert!(generated.usage.is_none());
}

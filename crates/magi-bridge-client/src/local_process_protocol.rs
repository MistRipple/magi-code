use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Write};
use thiserror::Error;

pub const LOCAL_BRIDGE_PROTOCOL_VERSION: &str = "local-bridge-v1";
pub const LOCAL_BRIDGE_HANDSHAKE_METHOD: &str = "bridge.handshake";
pub const LOCAL_BRIDGE_HEALTH_METHOD: &str = "bridge.health";
pub const LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD: &str = "bridge.describe_services";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BridgeServerKind {
    Model,
    Host,
    Mcp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerHandshake {
    pub protocol_version: String,
    pub server_kind: BridgeServerKind,
    pub health_method: String,
    pub supported_methods: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerHealth {
    pub protocol_version: String,
    pub server_kind: BridgeServerKind,
    pub status: String,
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerServiceDescriptor {
    pub service_name: String,
    pub shim_kind: String,
    pub supported_operations: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_health: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_health_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_roots_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_manifest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_server_health: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_server_selection_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_route_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_route_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_targets: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_manifest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_manifest: Option<BridgeServerShellManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_profile: Option<BridgeServerShellProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_capability_profiles: Option<Vec<BridgeServerCommandCapabilityProfile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_descriptor: Option<BridgeServerSessionDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_context: Option<BridgeServerWorkspaceContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_resolution_boundary: Option<BridgeServerContextResolutionBoundary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerShellManifest {
    pub shell_id: String,
    pub minimum_version: String,
    pub capability_version: String,
    pub implementation_source: String,
    pub capability_profile: String,
    pub workspace_roots_source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerShellProfile {
    pub profile_id: String,
    pub shell_id: String,
    pub host_kind: String,
    pub shell_family: String,
    pub minimum_version: String,
    pub capability_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerCommandCapabilityProfile {
    pub command_name: String,
    pub capability_id: String,
    pub interaction_mode: String,
    pub side_effect_level: String,
    pub requires_session_context: bool,
    pub requires_workspace_context: bool,
    pub path_argument_policy: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerSessionDescriptor {
    pub session_id: String,
    pub session_scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerWorkspaceContext {
    pub workspace_id: String,
    pub workspace_scope: String,
    pub workspace_roots_source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerContextResolutionBoundary {
    pub request_binding: String,
    pub session_resolution_strategy: String,
    pub workspace_resolution_strategy: String,
    pub session_resolution_source: String,
    pub workspace_resolution_source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerServiceCatalog {
    pub protocol_version: String,
    pub server_kind: BridgeServerKind,
    pub services: Vec<BridgeServerServiceDescriptor>,
}

#[derive(Debug, Error)]
pub enum LocalProcessBridgeServerError {
    #[error("stdio 读写失败: {0}")]
    Io(#[from] io::Error),
    #[error("序列化失败: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequestEnvelope {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponseEnvelope {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcErrorEnvelope>,
}

#[derive(Clone, Debug, Serialize)]
struct JsonRpcErrorEnvelope {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct LocalProcessBridgeRequest {
    pub id: Value,
    pub params: Value,
}

#[derive(Clone, Debug)]
pub struct LocalProcessBridgeRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl LocalProcessBridgeRpcError {
    #[cfg(test)]
    pub fn code(&self) -> i64 {
        self.code
    }

    #[cfg(test)]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[cfg(test)]
    pub fn data(&self) -> Option<&Value> {
        self.data.as_ref()
    }

    pub fn invalid_params(reason: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: "invalid params".to_string(),
            data: Some(json!({
                "reason": reason.into(),
            })),
        }
    }

    pub fn remote_business(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
}

pub fn run_local_process_bridge_server<F>(
    server_kind: BridgeServerKind,
    business_method: &'static str,
    service_catalog: BridgeServerServiceCatalog,
    handler: F,
) -> Result<(), LocalProcessBridgeServerError>
where
    F: Fn(LocalProcessBridgeRequest) -> Result<Value, LocalProcessBridgeRpcError>,
{
    run_local_process_bridge_server_with_methods(
        server_kind,
        &[business_method],
        service_catalog,
        move |_, request| handler(request),
    )
}

pub fn run_local_process_bridge_server_with_methods<F>(
    server_kind: BridgeServerKind,
    business_methods: &[&'static str],
    service_catalog: BridgeServerServiceCatalog,
    handler: F,
) -> Result<(), LocalProcessBridgeServerError>
where
    F: Fn(&str, LocalProcessBridgeRequest) -> Result<Value, LocalProcessBridgeRpcError>,
{
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }

        let response = handle_request_line(
            &line,
            server_kind,
            business_methods,
            &service_catalog,
            &handler,
        )?;
        writer.write_all(response.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        return Ok(());
    }
}

fn handle_request_line<F>(
    raw: &str,
    server_kind: BridgeServerKind,
    business_methods: &[&'static str],
    service_catalog: &BridgeServerServiceCatalog,
    handler: &F,
) -> Result<String, LocalProcessBridgeServerError>
where
    F: Fn(&str, LocalProcessBridgeRequest) -> Result<Value, LocalProcessBridgeRpcError>,
{
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(_) => {
            return to_json_response(error_response(Value::Null, -32700, "parse error", None));
        }
    };

    let request_id = value.get("id").cloned().unwrap_or(Value::Null);
    let request: JsonRpcRequestEnvelope = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(_) => {
            return to_json_response(error_response(request_id, -32600, "invalid request", None));
        }
    };

    if request.jsonrpc.as_deref() != Some("2.0") || request.id.is_none() {
        return to_json_response(error_response(request_id, -32600, "invalid request", None));
    }

    let request_id = request.id.unwrap_or(Value::Null);
    let method = request.method.unwrap_or_default();
    let supported_methods = supported_methods(business_methods);

    let result = match method.as_str() {
        LOCAL_BRIDGE_HANDSHAKE_METHOD => Ok(serde_json::to_value(BridgeServerHandshake {
            protocol_version: LOCAL_BRIDGE_PROTOCOL_VERSION.to_string(),
            server_kind,
            health_method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
            supported_methods: supported_methods.clone(),
        })?),
        LOCAL_BRIDGE_HEALTH_METHOD => Ok(serde_json::to_value(BridgeServerHealth {
            protocol_version: LOCAL_BRIDGE_PROTOCOL_VERSION.to_string(),
            server_kind,
            status: "ok".to_string(),
            ok: true,
        })?),
        LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD => Ok(serde_json::to_value(service_catalog.clone())?),
        _ if business_methods
            .iter()
            .any(|candidate| *candidate == method) =>
        {
            handler(
                method.as_str(),
                LocalProcessBridgeRequest {
                    id: request_id.clone(),
                    params: request.params,
                },
            )
        }
        _ => Err(LocalProcessBridgeRpcError::remote_business(
            -32601,
            "method not found",
            Some(json!({
                "server_kind": server_kind,
                "supported_methods": supported_methods,
            })),
        )),
    };

    match result {
        Ok(result) => to_json_response(success_response(request_id, result)),
        Err(error) => to_json_response(error_response(
            request_id,
            error.code,
            error.message,
            error.data,
        )),
    }
}

fn success_response(id: Value, result: Value) -> JsonRpcResponseEnvelope {
    JsonRpcResponseEnvelope {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(
    id: Value,
    code: i64,
    message: impl Into<String>,
    data: Option<Value>,
) -> JsonRpcResponseEnvelope {
    JsonRpcResponseEnvelope {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcErrorEnvelope {
            code,
            message: message.into(),
            data,
        }),
    }
}

fn to_json_response(
    response: JsonRpcResponseEnvelope,
) -> Result<String, LocalProcessBridgeServerError> {
    Ok(serde_json::to_string(&response)?)
}

fn supported_methods(business_methods: &[&str]) -> Vec<String> {
    let mut methods = vec![
        LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
        LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
        LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
    ];
    methods.extend(business_methods.iter().map(|method| method.to_string()));
    methods
}

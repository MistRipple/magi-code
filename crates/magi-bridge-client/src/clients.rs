use crate::{
    local_process_protocol::{
        BridgeServerHandshake, BridgeServerHealth, BridgeServerServiceCatalog,
        LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD, LOCAL_BRIDGE_HANDSHAKE_METHOD,
        LOCAL_BRIDGE_HEALTH_METHOD,
    },
    types::{
        BridgeClientError, BridgeErrorLayer, BridgeResponse, BridgeTransportError,
        BridgeTransportRequest, HostBridgeClient, HostBridgeRequest, McpBridgeClient,
        McpManagerDescribeServerResponse, McpManagerListServersResponse,
        McpManagerServerHealthUpdateRequest, McpManagerServerOperationResponse,
        McpManagerServerRegistrationRequest, McpManagerServerSelectionRequest,
        McpToolCallRequest, ModelBridgeClient, ModelInvocationRequest,
        SharedBridgeTransport,
    },
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

#[derive(Clone)]
pub struct JsonRpcBridgeServerProbeClient {
    transport: SharedBridgeTransport,
}

#[derive(Clone)]
pub struct JsonRpcHostBridgeClient {
    transport: SharedBridgeTransport,
    method: String,
}

#[derive(Clone)]
pub struct JsonRpcModelBridgeClient {
    transport: SharedBridgeTransport,
    method: String,
}

#[derive(Clone)]
pub struct JsonRpcMcpBridgeClient {
    transport: SharedBridgeTransport,
    method: String,
}

#[derive(Clone)]
pub struct JsonRpcMcpManagerClient {
    transport: SharedBridgeTransport,
}

impl JsonRpcHostBridgeClient {
    pub fn new(transport: SharedBridgeTransport) -> Self {
        Self {
            transport,
            method: "host.call".to_string(),
        }
    }

    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }
}

impl JsonRpcModelBridgeClient {
    pub fn new(transport: SharedBridgeTransport) -> Self {
        Self {
            transport,
            method: "model.invoke".to_string(),
        }
    }

    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }
}

impl JsonRpcMcpBridgeClient {
    pub fn new(transport: SharedBridgeTransport) -> Self {
        Self {
            transport,
            method: "mcp.call_tool".to_string(),
        }
    }

    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }
}

impl JsonRpcMcpManagerClient {
    pub fn new(transport: SharedBridgeTransport) -> Self {
        Self { transport }
    }

    pub fn list_servers(&self) -> Result<McpManagerListServersResponse, BridgeClientError> {
        call_and_decode(&self.transport, "mcp.list_servers", Value::Null)
    }

    pub fn describe_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerDescribeServerResponse, BridgeClientError> {
        self.call_with_params("mcp.describe_server", request)
    }

    pub fn enable_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.enable_server", request)
    }

    pub fn disable_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.disable_server", request)
    }

    pub fn register_server(
        &self,
        request: McpManagerServerRegistrationRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.register_server", request)
    }

    pub fn start_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.start_server", request)
    }

    pub fn stop_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.stop_server", request)
    }

    pub fn deregister_server(
        &self,
        request: McpManagerServerSelectionRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.deregister_server", request)
    }

    pub fn update_health(
        &self,
        request: McpManagerServerHealthUpdateRequest,
    ) -> Result<McpManagerServerOperationResponse, BridgeClientError> {
        self.call_with_params("mcp.update_health", request)
    }

    fn call_with_params<TParams, TResponse>(
        &self,
        method: &str,
        request: TParams,
    ) -> Result<TResponse, BridgeClientError>
    where
        TParams: Serialize,
        TResponse: DeserializeOwned,
    {
        let params = serde_json::to_value(request)
            .map_err(|error| protocol_call_failed(format!("serialize {method} request failed: {error}")))?;
        call_and_decode(&self.transport, method, params)
    }
}

impl JsonRpcBridgeServerProbeClient {
    pub fn new(transport: SharedBridgeTransport) -> Self {
        Self { transport }
    }

    pub fn handshake(&self) -> Result<BridgeServerHandshake, BridgeClientError> {
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: LOCAL_BRIDGE_HANDSHAKE_METHOD.to_string(),
                params: Value::Null,
            })
            .map_err(transport_call_failed)?;
        serde_json::from_value(response.payload)
            .map_err(|error| protocol_call_failed(format!("decode bridge handshake failed: {error}")))
    }

    pub fn health(&self) -> Result<BridgeServerHealth, BridgeClientError> {
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: LOCAL_BRIDGE_HEALTH_METHOD.to_string(),
                params: Value::Null,
            })
            .map_err(transport_call_failed)?;
        serde_json::from_value(response.payload)
            .map_err(|error| protocol_call_failed(format!("decode bridge health failed: {error}")))
    }

    pub fn describe_services(&self) -> Result<BridgeServerServiceCatalog, BridgeClientError> {
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: LOCAL_BRIDGE_DESCRIBE_SERVICES_METHOD.to_string(),
                params: Value::Null,
            })
            .map_err(transport_call_failed)?;
        serde_json::from_value(response.payload).map_err(|error| {
            protocol_call_failed(format!("decode bridge service catalog failed: {error}"))
        })
    }
}

impl HostBridgeClient for JsonRpcHostBridgeClient {
    fn call(&self, request: HostBridgeRequest) -> Result<BridgeResponse, BridgeClientError> {
        let params = serde_json::to_value(request)
            .map_err(|error| protocol_call_failed(format!("serialize host request failed: {error}")))?;
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: self.method.clone(),
                params,
            })
            .map_err(transport_call_failed)?;
        decode_bridge_response(response.payload)
    }
}

impl ModelBridgeClient for JsonRpcModelBridgeClient {
    fn invoke(
        &self,
        request: ModelInvocationRequest,
    ) -> Result<BridgeResponse, BridgeClientError> {
        let params = serde_json::to_value(request)
            .map_err(|error| protocol_call_failed(format!("serialize model request failed: {error}")))?;
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: self.method.clone(),
                params,
            })
            .map_err(transport_call_failed)?;
        decode_bridge_response(response.payload)
    }
}

impl McpBridgeClient for JsonRpcMcpBridgeClient {
    fn call_tool(&self, request: McpToolCallRequest) -> Result<BridgeResponse, BridgeClientError> {
        let params = serde_json::to_value(request)
            .map_err(|error| protocol_call_failed(format!("serialize mcp request failed: {error}")))?;
        let response = self
            .transport
            .call(BridgeTransportRequest {
                method: self.method.clone(),
                params,
            })
            .map_err(transport_call_failed)?;
        decode_bridge_response(response.payload)
    }
}

fn decode_bridge_response(value: Value) -> Result<BridgeResponse, BridgeClientError> {
    serde_json::from_value::<BridgeResponse>(value)
        .map_err(|error| protocol_call_failed(format!("decode bridge response failed: {error}")))
}

fn call_and_decode<T: DeserializeOwned>(
    transport: &SharedBridgeTransport,
    method: &str,
    params: Value,
) -> Result<T, BridgeClientError> {
    let response = transport
        .call(BridgeTransportRequest {
            method: method.to_string(),
            params,
        })
        .map_err(transport_call_failed)?;
    serde_json::from_value(response.payload)
        .map_err(|error| protocol_call_failed(format!("decode {method} response failed: {error}")))
}

fn protocol_call_failed(message: String) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Protocol,
        code: None,
        message,
    }
}

fn transport_call_failed(error: BridgeTransportError) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: error.layer(),
        code: error.code(),
        message: error.to_string(),
    }
}

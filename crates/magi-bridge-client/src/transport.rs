use crate::types::{
    BridgeErrorLayer, BridgeTransport, BridgeTransportError, BridgeTransportRequest,
    BridgeTransportResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
};

#[derive(Clone, Debug)]
pub struct JsonRpcStdioTransportConfig {
    pub executable: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

impl JsonRpcStdioTransportConfig {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            env: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct JsonRpcStdioTransport {
    config: JsonRpcStdioTransportConfig,
}

impl JsonRpcStdioTransport {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            config: JsonRpcStdioTransportConfig::new(executable),
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.config.args = args;
        self
    }

    pub fn with_working_directory(mut self, working_directory: impl Into<PathBuf>) -> Self {
        self.config.working_directory = Some(working_directory.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env.insert(key.into(), value.into());
        self
    }
}

impl BridgeTransport for JsonRpcStdioTransport {
    fn call(
        &self,
        request: BridgeTransportRequest,
    ) -> Result<BridgeTransportResponse, BridgeTransportError> {
        let mut command = Command::new(&self.config.executable);
        command.args(&self.config.args);
        if let Some(working_directory) = &self.config.working_directory {
            command.current_dir(working_directory);
        }
        for (key, value) in &self.config.env {
            command.env(key, value);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| BridgeTransportError::Transport {
            message: format!("spawn {} failed: {error}", self.config.executable),
        })?;

        let mut stdin = child.stdin.take().ok_or_else(|| BridgeTransportError::Transport {
            message: format!("{} stdin unavailable", self.config.executable),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| BridgeTransportError::Transport {
            message: format!("{} stdout unavailable", self.config.executable),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| BridgeTransportError::Transport {
            message: format!("{} stderr unavailable", self.config.executable),
        })?;

        let stdout_handle = thread::spawn(move || read_stream(stdout));
        let stderr_handle = thread::spawn(move || read_stream(stderr));

        let json_rpc_request = JsonRpcRequestEnvelope {
            jsonrpc: "2.0",
            id: 1,
            method: request.method,
            params: request.params,
        };
        let json =
            serde_json::to_string(&json_rpc_request).map_err(|error| BridgeTransportError::Protocol {
                message: format!("serialize request failed: {error}"),
            })?;
        writeln!(stdin, "{json}").map_err(|error| BridgeTransportError::Transport {
            message: format!("write request failed: {error}"),
        })?;
        drop(stdin);

        let status = child.wait().map_err(|error| BridgeTransportError::Transport {
            message: format!("wait {} failed: {error}", self.config.executable),
        })?;

        let stdout_bytes = stdout_handle.join().map_err(|_| BridgeTransportError::Transport {
            message: "stdout reader thread panicked".to_string(),
        })??;
        let stderr_bytes = stderr_handle.join().map_err(|_| BridgeTransportError::Transport {
            message: "stderr reader thread panicked".to_string(),
        })??;

        if !status.success() {
            return Err(BridgeTransportError::Transport {
                message: format!(
                    "{} exited with status {:?}: {}",
                    self.config.executable,
                    status.code(),
                    String::from_utf8_lossy(&stderr_bytes).trim()
                ),
            });
        }

        let stdout_text = String::from_utf8(stdout_bytes).map_err(|error| {
            BridgeTransportError::Protocol {
                message: format!("stdout is not utf-8: {error}"),
            }
        })?;
        let envelope: JsonRpcResponseEnvelope = serde_json::from_str(stdout_text.trim()).map_err(
            |error| BridgeTransportError::Protocol {
                message: format!("parse response failed: {error}; body={stdout_text}"),
            },
        )?;

        if envelope.jsonrpc != "2.0" {
            return Err(BridgeTransportError::Protocol {
                message: format!("unsupported jsonrpc version: {}", envelope.jsonrpc),
            });
        }
        if envelope.id != Value::from(1) {
            return Err(BridgeTransportError::Protocol {
                message: format!("response id mismatch: expected=1 actual={}", envelope.id),
            });
        }

        if let Some(error) = envelope.error {
            return Err(match classify_json_rpc_error(error.code) {
                BridgeErrorLayer::Protocol => BridgeTransportError::Protocol {
                    message: format!(
                        "json-rpc protocol error [{code}]: {message}",
                        code = error.code,
                        message = error.message
                    ),
                },
                BridgeErrorLayer::RemoteBusiness => BridgeTransportError::RemoteBusiness {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                },
                BridgeErrorLayer::Transport => BridgeTransportError::Transport {
                    message: format!(
                        "unexpected transport-classified json-rpc error [{code}]: {message}",
                        code = error.code,
                        message = error.message
                    ),
                },
            });
        }

        let payload = envelope.result.ok_or_else(|| BridgeTransportError::Protocol {
            message: "missing result and error in response".to_string(),
        })?;

        Ok(BridgeTransportResponse { payload })
    }
}

fn read_stream<R: Read>(mut reader: R) -> Result<Vec<u8>, BridgeTransportError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| BridgeTransportError::Transport {
            message: format!("read stream failed: {error}"),
        })?;
    Ok(bytes)
}

fn classify_json_rpc_error(code: i64) -> BridgeErrorLayer {
    match code {
        -32700 | -32600 | -32601 | -32602 | -32603 => BridgeErrorLayer::Protocol,
        _ => BridgeErrorLayer::RemoteBusiness,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcRequestEnvelope {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcResponseEnvelope {
    jsonrpc: String,
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcErrorEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcErrorEnvelope {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

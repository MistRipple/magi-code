//! 协议消息、错误种类与钩子目录（见 `docs/extension-protocol-v1.md`）。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;
pub const MAX_REENTRY_DEPTH: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    Observe,
    Advise,
    Guard,
}

impl HookMode {
    /// 单次调用 deadline 的默认值与上限（设计文档 5.8，需压力测试校准）。
    pub fn default_deadline(self) -> Duration {
        Duration::from_secs(5)
    }

    pub fn max_deadline(self) -> Duration {
        match self {
            Self::Guard => Duration::from_secs(30),
            Self::Advise => Duration::from_secs(15),
            Self::Observe => Duration::from_secs(5),
        }
    }
}

/// 错误种类（稳定枚举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    ProtocolError,
    VersionMismatch,
    StaleGeneration,
    MethodNotFound,
    InvalidParams,
    RegistrationDenied,
    PermissionDenied,
    Blocked,
    Cancelled,
    DeadlineExceeded,
    ReentryDenied,
    Deadlock,
    Backpressure,
    HostGone,
    BlobExpired,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: ErrorData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    pub kind: ErrorKind,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub detail: Value,
}

impl RpcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
            data: ErrorData {
                kind,
                retryable: false,
                detail: Value::Null,
            },
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.data.kind
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.data.kind, self.message)
    }
}

impl std::error::Error for RpcError {}

/// 一条入站消息的分类。
#[derive(Debug)]
pub enum Message {
    Request {
        id: String,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: String,
        result: Result<Value, RpcError>,
    },
}

impl Message {
    pub fn parse(value: Value) -> Result<Self, RpcError> {
        let bad = |what: &str| RpcError::new(ErrorKind::ProtocolError, what.to_string());
        let object = value.as_object().ok_or_else(|| bad("消息必须是对象"))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(bad("jsonrpc 必须是 2.0"));
        }
        let id = match object.get("id") {
            None => None,
            Some(Value::String(id)) => Some(id.clone()),
            Some(_) => return Err(bad("id 必须是字符串")),
        };
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .ok_or_else(|| bad("method 必须是字符串"))?
                .to_string();
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            return Ok(match id {
                Some(id) => Message::Request { id, method, params },
                None => Message::Notification { method, params },
            });
        }
        let id = id.ok_or_else(|| bad("响应缺少 id"))?;
        if let Some(error) = object.get("error") {
            let error: RpcError =
                serde_json::from_value(error.clone()).map_err(|_| bad("error 不符合合同"))?;
            return Ok(Message::Response {
                id,
                result: Err(error),
            });
        }
        match object.get("result") {
            Some(result) => Ok(Message::Response {
                id,
                result: Ok(result.clone()),
            }),
            None => Err(bad("响应缺少 result 或 error")),
        }
    }
}

pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

pub fn response_ok(id: &str, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn response_err(id: &str, error: &RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// 钩子目录条目：允许的模式、是否允许 `replace` 及补丁上限（协议第 6 节）。
#[derive(Clone, Copy, Debug)]
pub struct HookSpec {
    pub name: &'static str,
    /// 协议允许的模式。
    pub modes: &'static [HookMode],
    /// 当前 daemon 实际接线的模式。声明了未接线模式的包在安装阶段被拒绝（不留装饰性声明，I5）。
    pub wired: &'static [HookMode],
    pub replace: bool,
    pub patch_limit: usize,
}

const NONE: &[HookMode] = &[];
const GUARD: &[HookMode] = &[HookMode::Guard];
const OBSERVE: &[HookMode] = &[HookMode::Observe];
const GUARD_ADVISE: &[HookMode] = &[HookMode::Guard, HookMode::Advise];
const ADVISE: &[HookMode] = &[HookMode::Advise];
const OBSERVE_ADVISE: &[HookMode] = &[HookMode::Observe, HookMode::Advise];
const KIB: usize = 1024;

pub const HOOKS: &[HookSpec] = &[
    HookSpec {
        name: "session.created",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "session.deleted",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "turn.accepted",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "turn.end",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "input",
        modes: GUARD_ADVISE,
        wired: GUARD,
        replace: true,
        patch_limit: 1024 * KIB,
    },
    HookSpec {
        name: "prompt.contribute",
        modes: ADVISE,
        wired: NONE,
        replace: true,
        patch_limit: 256 * KIB,
    },
    HookSpec {
        name: "context.assemble",
        modes: ADVISE,
        wired: NONE,
        replace: true,
        patch_limit: 64 * KIB,
    },
    HookSpec {
        name: "model.request",
        modes: OBSERVE_ADVISE,
        wired: OBSERVE,
        replace: true,
        patch_limit: 64 * KIB,
    },
    HookSpec {
        name: "model.response",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "tool.call",
        modes: GUARD_ADVISE,
        wired: GUARD_ADVISE,
        replace: true,
        patch_limit: 1024 * KIB,
    },
    HookSpec {
        name: "tool.result",
        modes: GUARD_ADVISE,
        wired: GUARD_ADVISE,
        replace: true,
        patch_limit: 1024 * KIB,
    },
    HookSpec {
        name: "compaction.before",
        modes: ADVISE,
        wired: NONE,
        replace: true,
        patch_limit: 64 * KIB,
    },
    HookSpec {
        name: "subagent.spawn",
        modes: GUARD_ADVISE,
        wired: GUARD,
        replace: true,
        patch_limit: 64 * KIB,
    },
    HookSpec {
        name: "model.resolved",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
    HookSpec {
        name: "usage.update",
        modes: OBSERVE,
        wired: OBSERVE,
        replace: false,
        patch_limit: 0,
    },
];

pub fn hook_spec(name: &str) -> Option<&'static HookSpec> {
    HOOKS.iter().find(|spec| spec.name == name)
}

/// 清单声明的钩子注册。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookRegistration {
    pub name: String,
    pub mode: HookMode,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
}

impl HookRegistration {
    /// 在 `validate` 之外要求该模式已被 daemon 接线；安装与激活用它，避免声明永远不会触发的钩子。
    pub fn validate_wired(&self) -> Result<Duration, RpcError> {
        let deadline = self.validate()?;
        let spec = hook_spec(&self.name).expect("validated hook exists");
        if spec.wired.contains(&self.mode) {
            Ok(deadline)
        } else {
            Err(RpcError::new(
                ErrorKind::RegistrationDenied,
                format!(
                    "钩子 {} 的 {:?} 模式尚未接入 daemon 调用链",
                    self.name, self.mode
                ),
            ))
        }
    }

    /// 校验目录与模式组合，返回生效的 deadline（受模式上限约束）。
    pub fn validate(&self) -> Result<Duration, RpcError> {
        let spec = hook_spec(&self.name).ok_or_else(|| {
            RpcError::new(
                ErrorKind::RegistrationDenied,
                format!("未知钩子 {}", self.name),
            )
        })?;
        if !spec.modes.contains(&self.mode) {
            return Err(RpcError::new(
                ErrorKind::RegistrationDenied,
                format!("钩子 {} 不允许 {:?} 模式", self.name, self.mode),
            ));
        }
        let requested = self
            .deadline_ms
            .map(Duration::from_millis)
            .unwrap_or_else(|| self.mode.default_deadline());
        Ok(requested.min(self.mode.max_deadline()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_classified_and_malformed_ones_rejected() {
        assert!(matches!(
            Message::parse(request("1", "m", json!({}))).unwrap(),
            Message::Request { .. }
        ));
        assert!(matches!(
            Message::parse(notification("m", json!({}))).unwrap(),
            Message::Notification { .. }
        ));
        assert!(matches!(
            Message::parse(response_ok("1", json!(1))).unwrap(),
            Message::Response { result: Ok(_), .. }
        ));
        let error = RpcError::new(ErrorKind::Blocked, "no");
        assert!(matches!(
            Message::parse(response_err("1", &error)).unwrap(),
            Message::Response { result: Err(_), .. }
        ));
        for bad in [
            json!([]),
            json!({"jsonrpc": "1.0", "method": "m"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "m"}),
            json!({"jsonrpc": "2.0", "id": "1"}),
        ] {
            assert!(Message::parse(bad).is_err());
        }
    }

    #[test]
    fn hook_registrations_follow_the_catalog_and_clamp_deadlines() {
        let reg = |name: &str, mode, deadline_ms| HookRegistration {
            name: name.into(),
            mode,
            deadline_ms,
        };
        assert_eq!(
            reg("tool.call", HookMode::Guard, None).validate().unwrap(),
            Duration::from_secs(5)
        );
        assert_eq!(
            reg("tool.call", HookMode::Guard, Some(120_000))
                .validate()
                .unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            reg("tool.call", HookMode::Advise, Some(120_000))
                .validate()
                .unwrap(),
            Duration::from_secs(15)
        );
        // 观察钩子不能是 guard；目录外的钩子不存在。
        assert!(reg("turn.end", HookMode::Guard, None).validate().is_err());
        assert!(
            reg("model.request", HookMode::Guard, None)
                .validate()
                .is_err()
        );
        assert!(reg("nope", HookMode::Observe, None).validate().is_err());
    }

    #[test]
    fn only_wired_hook_modes_are_accepted_for_production_registration() {
        let reg = |name: &str, mode| HookRegistration {
            name: name.into(),
            mode,
            deadline_ms: None,
        };
        for (name, mode) in [
            ("tool.call", HookMode::Guard),
            ("tool.call", HookMode::Advise),
            ("input", HookMode::Guard),
            ("subagent.spawn", HookMode::Guard),
            ("turn.end", HookMode::Observe),
            ("model.request", HookMode::Observe),
        ] {
            assert!(reg(name, mode).validate_wired().is_ok(), "{name} {mode:?}");
        }
        for (name, mode) in [
            ("input", HookMode::Advise),
            ("model.request", HookMode::Advise),
            ("prompt.contribute", HookMode::Advise),
            ("context.assemble", HookMode::Advise),
            ("compaction.before", HookMode::Advise),
            ("subagent.spawn", HookMode::Advise),
        ] {
            assert!(reg(name, mode).validate_wired().is_err(), "{name} {mode:?}");
        }
        // 目录允许但未接线的组合仍然通过协议级校验（测试与未来接线使用）。
        assert!(reg("model.request", HookMode::Advise).validate().is_ok());
    }
}

use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::fmt::Display;

/// 统一错误码枚举
///
/// 命名规则：`{领域}.{动作}.{原因}`
/// 所有对外错误必须经过此枚举映射，不允许 route handler 直接构造裸 HTTP 错误
#[derive(Clone, Debug)]
pub enum ApiError {
    /// 请求体 JSON 格式不合法或字段缺失
    InvalidInput(String),
    /// 指定的 session 不存在
    SessionNotFound(String),
    /// 指定的 recovery 不存在
    RecoveryNotFound(String),
    /// 通用资源不存在
    NotFound(String),
    /// 事件发布失败（event bus 内部异常）
    EventPublishFailed(String),
    /// 内部组装错误（bootstrap / projection / sidecar 合并等）
    InternalAssemblyError(String),
    /// 资源状态冲突（如 runner 已启动）
    Conflict(String),
}

/// 对外统一错误响应 DTO
///
/// 所有非 2xx 响应均使用此结构，前端仅需解析一种错误格式
#[derive(Clone, Debug, Serialize)]
pub struct ErrorResponseDto {
    pub error_code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ApiError {
    pub fn internal_assembly(context: &str, err: impl Display) -> Self {
        Self::InternalAssemblyError(format!("{}: {}", context, err))
    }

    pub fn event_publish_failed(context: &str, err: impl Display) -> Self {
        Self::EventPublishFailed(format!("{}: {}", context, err))
    }

    pub fn recovery_not_found(recovery_id: &str) -> Self {
        Self::RecoveryNotFound(format!("恢复入口不存在: {}", recovery_id))
    }

    pub fn session_not_found(session_id: &str) -> Self {
        Self::SessionNotFound(format!("会话不存在: {}", session_id))
    }

    pub fn not_found(context: &str, id: &str) -> Self {
        Self::NotFound(format!("{}: {}", context, id))
    }

    pub fn conflict(context: &str, id: &str) -> Self {
        Self::Conflict(format!("{}: {}", context, id))
    }

    fn error_code(&self) -> &'static str {
        match self {
            ApiError::InvalidInput(_) => "INPUT_INVALID",
            ApiError::SessionNotFound(_) => "SESSION_NOT_FOUND",
            ApiError::RecoveryNotFound(_) => "RECOVERY_NOT_FOUND",
            ApiError::NotFound(_) => "NOT_FOUND",
            ApiError::EventPublishFailed(_) => "EVENT_PUBLISH_FAILED",
            ApiError::InternalAssemblyError(_) => "INTERNAL_ASSEMBLY_ERROR",
            ApiError::Conflict(_) => "CONFLICT",
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            ApiError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::RecoveryNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::EventPublishFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::InternalAssemblyError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
        }
    }

    fn message(&self) -> &str {
        match self {
            ApiError::InvalidInput(message) => message,
            ApiError::SessionNotFound(message) => message,
            ApiError::RecoveryNotFound(message) => message,
            ApiError::NotFound(message) => message,
            ApiError::EventPublishFailed(message) => message,
            ApiError::InternalAssemblyError(message) => message,
            ApiError::Conflict(message) => message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorResponseDto {
            error_code: self.error_code().to_string(),
            message: self.message().to_string(),
            detail: None,
        };
        (status, Json(body)).into_response()
    }
}

/// 从 Axum 的 JsonRejection 自动转换为 ApiError
///
/// 覆盖 Axum 默认的 422 裸文本，统一为结构化错误响应
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::InvalidInput(rejection.body_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_mapping() {
        assert_eq!(
            ApiError::InvalidInput("bad".into()).error_code(),
            "INPUT_INVALID"
        );
        assert_eq!(
            ApiError::SessionNotFound("missing".into()).error_code(),
            "SESSION_NOT_FOUND"
        );
        assert_eq!(
            ApiError::RecoveryNotFound("missing".into()).error_code(),
            "RECOVERY_NOT_FOUND"
        );
        assert_eq!(
            ApiError::EventPublishFailed("bus down".into()).error_code(),
            "EVENT_PUBLISH_FAILED"
        );
        assert_eq!(
            ApiError::InternalAssemblyError("boom".into()).error_code(),
            "INTERNAL_ASSEMBLY_ERROR"
        );
    }

    #[test]
    fn status_code_mapping() {
        assert_eq!(
            ApiError::InvalidInput("bad".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::SessionNotFound("missing".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::RecoveryNotFound("missing".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::EventPublishFailed("fail".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::InternalAssemblyError("err".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn error_response_dto_serializes_without_detail_when_none() {
        let dto = ErrorResponseDto {
            error_code: "INPUT_INVALID".to_string(),
            message: "bad request".to_string(),
            detail: None,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("detail"));
    }

    #[test]
    fn error_response_dto_serializes_with_detail_when_present() {
        let dto = ErrorResponseDto {
            error_code: "INPUT_INVALID".to_string(),
            message: "bad request".to_string(),
            detail: Some("field 'text' is required".to_string()),
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("detail"));
        assert!(json.contains("field 'text' is required"));
    }

    #[test]
    fn helper_constructors_keep_error_variants_and_context() {
        let internal = ApiError::internal_assembly("创建会话失败", "boom");
        let publish = ApiError::event_publish_failed("事件发布失败", "down");
        let recovery = ApiError::recovery_not_found("recovery-1");

        match internal {
            ApiError::InternalAssemblyError(message) => {
                assert_eq!(message, "创建会话失败: boom");
            }
            other => panic!("unexpected internal variant: {:?}", other),
        }

        match publish {
            ApiError::EventPublishFailed(message) => {
                assert_eq!(message, "事件发布失败: down");
            }
            other => panic!("unexpected publish variant: {:?}", other),
        }

        match recovery {
            ApiError::RecoveryNotFound(message) => {
                assert_eq!(message, "恢复入口不存在: recovery-1");
            }
            other => panic!("unexpected recovery variant: {:?}", other),
        }
    }
}

use axum::{
    Json,
    body::to_bytes,
    extract::{Request, rejection::JsonRejection},
    http::{StatusCode, header::CONTENT_TYPE},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::fmt::Display;

/// 统一错误码枚举
///
/// 命名规则：`{领域}.{动作}.{原因}`
/// 所有对外错误必须经过此枚举映射，不允许 route handler 直接构造裸 HTTP 错误
#[derive(Clone, Debug)]
pub enum ApiError {
    /// 已进入业务处理后的输入校验失败，消息应当是可直接展示的产品文案
    InvalidInput(String),
    /// 请求体无法被框架解析，原始 parser 细节只进入日志
    InvalidRequestBody(String),
    /// 指定的 session 不存在
    SessionNotFound(String),
    /// 指定的 recovery 不存在
    RecoveryNotFound(String),
    /// 通用资源不存在
    NotFound(String),
    /// 模型或外部执行器调用失败
    ModelInvocationFailed(String),
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

    pub fn model_invocation_failed(context: &str, err: impl Display) -> Self {
        Self::ModelInvocationFailed(format!("{}: {}", context, err))
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

    pub fn invalid_request_body(err: impl Display) -> Self {
        Self::InvalidRequestBody(format!("请求体解析失败: {}", err))
    }

    fn error_code(&self) -> &'static str {
        match self {
            ApiError::InvalidInput(_) => "INPUT_INVALID",
            ApiError::InvalidRequestBody(_) => "REQUEST_BODY_INVALID",
            ApiError::SessionNotFound(_) => "SESSION_NOT_FOUND",
            ApiError::RecoveryNotFound(_) => "RECOVERY_NOT_FOUND",
            ApiError::NotFound(_) => "NOT_FOUND",
            ApiError::ModelInvocationFailed(_) => "MODEL_INVOCATION_FAILED",
            ApiError::InternalAssemblyError(_) => "INTERNAL_ASSEMBLY_ERROR",
            ApiError::Conflict(_) => "CONFLICT",
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            ApiError::InvalidRequestBody(_) => StatusCode::BAD_REQUEST,
            ApiError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::RecoveryNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::ModelInvocationFailed(_) => StatusCode::BAD_GATEWAY,
            ApiError::InternalAssemblyError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            ApiError::InvalidInput(message) => message,
            ApiError::InvalidRequestBody(message) => message,
            ApiError::SessionNotFound(message) => message,
            ApiError::RecoveryNotFound(message) => message,
            ApiError::NotFound(message) => message,
            ApiError::ModelInvocationFailed(message) => message,
            ApiError::InternalAssemblyError(message) => message,
            ApiError::Conflict(message) => message,
        }
    }

    fn public_message(&self) -> &str {
        match self {
            ApiError::InvalidRequestBody(_) => "请求内容格式不正确，请检查后重试",
            ApiError::ModelInvocationFailed(_) => "模型服务暂不可用，请检查模型配置或稍后重试",
            ApiError::InternalAssemblyError(_) => "服务状态暂不可用，请稍后重试",
            _ => self.message(),
        }
    }

    fn hides_private_message(&self) -> bool {
        matches!(
            self,
            ApiError::InvalidRequestBody(_)
                | ApiError::ModelInvocationFailed(_)
                | ApiError::InternalAssemblyError(_)
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_code = self.error_code();
        let private_message = self.message().to_string();
        let public_message = self.public_message().to_string();
        if self.hides_private_message() {
            tracing::warn!(
                error_code,
                error = %private_message,
                "api runtime detail hidden from response"
            );
        }
        let body = ErrorResponseDto {
            error_code: error_code.to_string(),
            message: public_message,
            detail: None,
        };
        (status, Json(body)).into_response()
    }
}

pub(crate) async fn normalize_framework_rejection_response(
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    if !is_framework_request_rejection_response(&response) {
        return response;
    }

    let status = response.status();
    let private_message = match to_bytes(response.into_body(), 16 * 1024).await {
        Ok(bytes) => {
            let body_text = String::from_utf8_lossy(&bytes).trim().to_string();
            if body_text.is_empty() {
                format!("framework request rejection: {status}")
            } else {
                body_text
            }
        }
        Err(error) => format!("framework request rejection body unavailable: {error}"),
    };
    tracing::warn!(
        status = %status,
        error = %private_message,
        "api framework rejection detail hidden from response"
    );
    ApiError::invalid_request_body(private_message).into_response()
}

fn is_framework_request_rejection_response(response: &Response) -> bool {
    let status = response.status();
    if !matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    !content_type.contains("application/json")
}

/// 从 Axum 的 JsonRejection 自动转换为 ApiError
///
/// 显式处理 JsonRejection 的 handler 也必须走同一套公共错误边界。
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::invalid_request_body(rejection.body_text())
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
            ApiError::invalid_request_body("line 1 column 1").error_code(),
            "REQUEST_BODY_INVALID"
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
            ApiError::ModelInvocationFailed("model down".into()).error_code(),
            "MODEL_INVOCATION_FAILED"
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
            ApiError::invalid_request_body("line 1 column 1").status_code(),
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
            ApiError::ModelInvocationFailed("fail".into()).status_code(),
            StatusCode::BAD_GATEWAY
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
    fn runtime_error_public_messages_hide_internal_detail() {
        let internal = ApiError::internal_assembly("创建会话失败", "task_store 未配置");
        let model = ApiError::model_invocation_failed("模型调用失败", "provider transport failed");
        let request_body = ApiError::invalid_request_body("expected value at line 1 column 1");

        assert_eq!(internal.message(), "创建会话失败: task_store 未配置");
        assert_eq!(internal.public_message(), "服务状态暂不可用，请稍后重试");
        assert_eq!(
            model.public_message(),
            "模型服务暂不可用，请检查模型配置或稍后重试"
        );
        assert_eq!(
            request_body.public_message(),
            "请求内容格式不正确，请检查后重试"
        );
        assert!(request_body.hides_private_message());
    }

    #[test]
    fn helper_constructors_keep_error_variants_and_context() {
        let internal = ApiError::internal_assembly("创建会话失败", "boom");
        let model = ApiError::model_invocation_failed("模型调用失败", "down");
        let recovery = ApiError::recovery_not_found("recovery-1");
        let request_body = ApiError::invalid_request_body("body parse failed");

        match internal {
            ApiError::InternalAssemblyError(message) => {
                assert_eq!(message, "创建会话失败: boom");
            }
            other => panic!("unexpected internal runtime_payload: {:?}", other),
        }

        match model {
            ApiError::ModelInvocationFailed(message) => {
                assert_eq!(message, "模型调用失败: down");
            }
            other => panic!("unexpected model runtime_payload: {:?}", other),
        }

        match recovery {
            ApiError::RecoveryNotFound(message) => {
                assert_eq!(message, "恢复入口不存在: recovery-1");
            }
            other => panic!("unexpected recovery runtime_payload: {:?}", other),
        }

        match request_body {
            ApiError::InvalidRequestBody(message) => {
                assert_eq!(message, "请求体解析失败: body parse failed");
            }
            other => panic!("unexpected request body runtime_payload: {:?}", other),
        }
    }

    #[tokio::test]
    async fn request_body_parse_error_response_hides_parser_detail() {
        let response =
            ApiError::invalid_request_body("expected value at line 1 column 1").into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value =
            serde_json::from_slice(&bytes).expect("error response json");

        assert_eq!(payload["error_code"], "REQUEST_BODY_INVALID");
        assert_eq!(payload["message"], "请求内容格式不正确，请检查后重试");
        assert!(payload.get("detail").is_none());
        assert!(
            !String::from_utf8_lossy(&bytes).contains("line 1 column 1"),
            "请求体解析器细节不能出现在响应体中"
        );
    }
}

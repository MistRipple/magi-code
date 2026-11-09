//! 浏览器原生资源加载共用插件路由执行入口；身份由 URL 绑定，不建立第二份资源缓存。
use super::*;
use axum::{
    body::Body,
    extract::RawQuery,
    http::{Method, header},
};
use magi_app_server_protocol::generated::{PluginRouteCallRequest, PluginRouteMethod};

pub(super) async fn resource(
    State(state): State<ApiState>,
    Path((id, digest, instance, path)): Path<(String, String, String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
) -> Result<Response, ApiError> {
    if path.len() > 4096 || query.as_ref().is_some_and(|value| value.len() > 8192) {
        return Err(ApiError::InvalidInput("插件资源地址超过大小上限".into()));
    }
    let executor = state
        .plugin_route_executor
        .clone()
        .ok_or_else(|| ApiError::Conflict("扩展宿主未运行".into()))?;
    let result = executor
        .call(
            id,
            PluginRouteCallRequest {
                method: if method == Method::HEAD {
                    PluginRouteMethod::HEAD
                } else {
                    PluginRouteMethod::GET
                },
                path: format!("/{path}"),
                query,
                headers: None,
                body: None,
                body_base64: None,
                expected_digest: Some(digest),
                expected_instance_id: Some(instance),
            },
        )
        .await?;
    let bytes = match (result.body, result.body_base64) {
        (body, Some(encoded)) if body.as_ref().is_none_or(Value::is_null) => {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ApiError::InvalidInput("插件资源字节编码无效".into()))?
        }
        (Some(Value::String(text)), None) => text.into_bytes(),
        (None | Some(Value::Null), None) => Vec::new(),
        (Some(value), None) => serde_json::to_vec(&value)
            .map_err(|error| ApiError::internal_assembly("插件资源编码失败", error))?,
        _ => return Err(ApiError::InvalidInput("插件资源载荷冲突".into())),
    };
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(ApiError::InvalidInput("插件资源超过 16 MiB".into()));
    }
    let mut response = Response::new(
        if method == Method::HEAD || matches!(result.status, 204 | 205 | 304) {
            Body::empty()
        } else {
            bytes.into()
        },
    );
    *response.status_mut() = StatusCode::from_u16(result.status)
        .map_err(|_| ApiError::InvalidInput("插件资源状态码无效".into()))?;
    let application_headers = result.headers.as_ref().and_then(Value::as_object);
    let content_type = result
        .content_type
        .as_deref()
        .or_else(|| application_headers?.get("content-type")?.as_str())
        .unwrap_or("application/octet-stream");
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .map_err(|_| ApiError::InvalidInput("插件资源类型无效".into()))?,
    );
    for name in [
        "etag",
        "last-modified",
        "content-disposition",
        "content-range",
        "accept-ranges",
    ] {
        if let Some(value) = application_headers
            .and_then(|values| values.get(name))
            .and_then(Value::as_str)
        {
            headers.insert(
                name,
                HeaderValue::from_str(value)
                    .map_err(|_| ApiError::InvalidInput("插件资源响应头无效".into()))?,
            );
        }
    }
    // opaque-origin 文档的原生 ESM 模块需要匿名 CORS；不允许凭据，不扩大其他 API 的来源规则。
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if content_type.starts_with("image/") {
            magi_media::IMAGE_CONTENT_SECURITY_POLICY
        } else {
            VIEW_CONTENT_SECURITY_POLICY
        }),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    struct Routes(Arc<Mutex<Vec<PluginRouteCallRequest>>>);
    impl crate::PluginRouteExecutor for Routes {
        fn call(
            &self,
            id: String,
            request: PluginRouteCallRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            magi_app_server_protocol::generated::PluginRouteCallResult,
                            ApiError,
                        >,
                    > + Send,
            >,
        > {
            assert_eq!(id, "acme.assets");
            let method = request.method.clone();
            self.0.lock().unwrap().push(request);
            Box::pin(async move {
                Ok(magi_app_server_protocol::generated::PluginRouteCallResult {
                    status: 200,
                    content_type: Some("text/javascript; charset=utf-8".into()),
                    body: (method != PluginRouteMethod::HEAD)
                        .then(|| Value::String("export const original = '完整';".into())),
                    body_base64: None,
                    headers: Some(
                        serde_json::json!({"etag":"original","set-cookie":"must-not-escape=1","content-security-policy":"default-src *"}),
                    ),
                })
            })
        }
    }

    #[tokio::test]
    async fn native_resources_delegate_once_preserve_bytes_and_enforce_their_own_response_policy() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = super::super::tests::bare_state(root.path())
            .with_plugin_route_executor(Arc::new(Routes(calls.clone())));
        let app = super::super::routes().with_state(state);
        for method in [Method::GET, Method::HEAD] {
            let response = app.clone().oneshot(axum::http::Request::builder()
                .method(method.clone()).uri("/plugins/acme.assets/assets/digest/instance/runtime/main.mjs?q=original")
                .body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/javascript; charset=utf-8"
            );
            assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()["etag"], "original");
            assert!(!response.headers().contains_key(header::SET_COOKIE));
            assert!(
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("connect-src 'none'")
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                bytes.as_ref(),
                if method == Method::HEAD {
                    b""
                } else {
                    "export const original = '完整';".as_bytes()
                }
            );
        }
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for call in calls.iter() {
            assert_eq!(call.path, "/runtime/main.mjs");
            assert_eq!(call.query.as_deref(), Some("q=original"));
            assert_eq!(call.expected_digest.as_deref(), Some("digest"));
            assert_eq!(call.expected_instance_id.as_deref(), Some("instance"));
            assert!(call.headers.is_none(), "不转发浏览器凭据或宿主请求头");
        }
    }
}

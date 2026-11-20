//! 浏览器原生资源加载共用插件路由执行入口；身份由 URL 绑定，不建立第二份资源缓存。
use super::*;
use axum::{
    body::Body,
    extract::RawQuery,
    http::{HeaderMap, Method, header},
};
use magi_app_server_protocol::generated::{PluginRouteCallRequest, PluginRouteMethod};

const ASSET_CONTENT_SECURITY_POLICY: &str = "sandbox allow-scripts allow-downloads; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; object-src 'none'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; connect-src 'none'";

pub(super) async fn document_resource(
    State(state): State<ApiState>,
    Path((id, digest, instance, path)): Path<(String, String, String, String)>,
    query: RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // A route URL based on the mounted document's full base URI inherits
    // its package and runtime identity, including for native script requests.
    if let Some(route) = path.strip_prefix("index.html/") {
        return resource(
            State(state),
            Path((id, digest, instance, route.to_owned())),
            query,
            method,
            headers,
        )
        .await;
    }
    super::resource_response(&state, &id, &path, Some(&digest))
}

pub(super) async fn resource(
    State(state): State<ApiState>,
    Path((id, digest, instance, path)): Path<(String, String, String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if path.len() > 4096 || query.as_ref().is_some_and(|value| value.len() > 8192) {
        return Err(ApiError::InvalidInput("插件资源地址超过大小上限".into()));
    }
    let result = dispatch_plugin_route(
        &state,
        id,
        PluginRouteCallRequest {
            method: if method == Method::HEAD {
                PluginRouteMethod::HEAD
            } else {
                PluginRouteMethod::GET
            },
            path: format!("/{path}"),
            query,
            headers: resource_request_headers(&headers)?,
            body: None,
            body_base64: None,
            expected_digest: Some(digest),
            expected_instance_id: Some(instance),
            session_id: None,
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
            ASSET_CONTENT_SECURITY_POLICY
        }),
    );
    Ok(response)
}

// 原生加载只传递资源协商信息。Cookie、Bearer、Origin、Host 和其他宿主头
// 不进入插件；页面显式业务头仍由原有 route.call 提交。
fn resource_request_headers(headers: &HeaderMap) -> Result<Option<Value>, ApiError> {
    let mut selected = serde_json::Map::new();
    let mut size = 0;
    for name in [
        "accept",
        "range",
        "if-range",
        "if-none-match",
        "if-modified-since",
    ] {
        let values = headers.get_all(name);
        if values.iter().next().is_none() {
            continue;
        }
        let value = values
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map_err(|_| ApiError::InvalidInput("插件资源请求头无效".into()))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        size += name.len() + value.len();
        if value.len() > 8192 || size > 16 * 1024 {
            return Err(ApiError::InvalidInput("插件资源请求头超过大小上限".into()));
        }
        selected.insert(name.into(), Value::String(value));
    }
    Ok((!selected.is_empty()).then_some(Value::Object(selected)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    struct Routes(Arc<Mutex<Vec<PluginRouteCallRequest>>>);
    impl crate::NativePluginPlatform for Routes {
        fn list_installed(
            &self,
            _: &magi_plugin_system::PluginManager,
        ) -> Result<Vec<magi_app_server_protocol::PluginInstalled>, ApiError> {
            unreachable!()
        }

        fn installed_upstream(
            &self,
            _: &magi_plugin_system::PluginManager,
        ) -> Result<crate::native_plugins::InstalledUpstreamPlugins, ApiError> {
            unreachable!()
        }

        fn enabled_manifests(
            &self,
            _: &magi_plugin_system::PluginManager,
        ) -> Result<Vec<magi_app_server_protocol::PluginManifest>, ApiError> {
            unreachable!()
        }

        fn enabled_commands(&self) -> Vec<Value> {
            unreachable!()
        }

        fn preview_upstream(
            &self,
            _: magi_app_server_protocol::generated::PluginUpstreamRequest,
        ) -> crate::NativePlatformFuture<
            '_,
            Result<magi_app_server_protocol::generated::PluginUpstreamPreview, ApiError>,
        > {
            unreachable!()
        }

        fn install_upstream(
            &self,
            _: magi_app_server_protocol::generated::PluginUpstreamRequest,
            _: Option<String>,
        ) -> crate::NativePlatformFuture<
            '_,
            Result<crate::native_plugins::PluginInstallOutcome, ApiError>,
        > {
            unreachable!()
        }

        fn configuration_error(&self) -> Option<String> {
            None
        }

        fn cancel_install(
            &self,
            _: String,
        ) -> crate::NativePlatformFuture<'_, Result<serde_json::Value, ApiError>> {
            unreachable!()
        }

        fn preview_archive(
            &self,
            _: String,
            _: Vec<u8>,
        ) -> crate::NativePlatformFuture<
            '_,
            Result<magi_app_server_protocol::generated::PluginCandidatePreview, ApiError>,
        > {
            unreachable!()
        }

        fn install_archive(
            &self,
            _: String,
            _: Vec<u8>,
            _: String,
        ) -> crate::NativePlatformFuture<'_, Result<(), ApiError>> {
            unreachable!()
        }

        fn set_enabled(
            &self,
            _: String,
            _: bool,
            _: Option<String>,
        ) -> crate::NativePlatformFuture<'_, Result<(), ApiError>> {
            unreachable!()
        }

        fn uninstall(&self, _: String) -> crate::NativePlatformFuture<'_, Result<(), ApiError>> {
            unreachable!()
        }

        fn serve_ui_asset(
            &self,
            _: &magi_plugin_system::PluginManager,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<(&'static str, Vec<u8>), ApiError> {
            unreachable!()
        }

        fn serve_icon(
            &self,
            _: &magi_plugin_system::PluginManager,
            _: &str,
        ) -> Result<Option<(&'static str, Vec<u8>)>, ApiError> {
            unreachable!()
        }

        fn report_client_slots(
            &self,
            _: &str,
            _: crate::PluginClientSlotsReport,
        ) -> Result<(), ApiError> {
            unreachable!()
        }

        fn restart_host(&self) -> crate::NativePlatformFuture<'_, Result<(), ApiError>> {
            unreachable!()
        }

        fn call_gateway(
            &self,
            id: String,
            request: PluginRouteCallRequest,
            _: Option<crate::native_plugins::PluginRouteSession>,
        ) -> crate::NativePlatformFuture<'_, Result<PluginRouteCallResult, ApiError>> {
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
            .with_native_plugin_platform(Arc::new(Routes(calls.clone())));
        let app = super::super::routes().with_state(state);
        for (method, uri) in [
            (
                Method::GET,
                "/plugins/acme.assets/assets/digest/instance/runtime/main.mjs?q=original",
            ),
            (
                Method::HEAD,
                "/plugins/acme.assets/assets/digest/instance/runtime/main.mjs?q=original",
            ),
            (
                Method::GET,
                "/plugins/acme.assets/ui-v/digest/instance/instance/index.html/runtime/main.mjs?q=original",
            ),
            (
                Method::HEAD,
                "/plugins/acme.assets/ui-v/digest/instance/instance/index.html/runtime/main.mjs?q=original",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .header("range", "bytes=0-31")
                        .header("if-range", "original")
                        .header("if-none-match", "original")
                        .header("if-modified-since", "Mon, 09 Nov 2026 00:00:00 GMT")
                        .header("accept", "text/javascript")
                        .header("cookie", "host-session=private")
                        .header("authorization", "Bearer host-private")
                        .header("origin", "https://host.example")
                        .header("x-host-private", "private")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
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
        assert_eq!(calls.len(), 4);
        for call in calls.iter() {
            assert_eq!(call.path, "/runtime/main.mjs");
            assert_eq!(call.query.as_deref(), Some("q=original"));
            assert_eq!(call.expected_digest.as_deref(), Some("digest"));
            assert_eq!(call.expected_instance_id.as_deref(), Some("instance"));
            assert_eq!(
                call.headers,
                Some(serde_json::json!({
                    "range": "bytes=0-31", "if-range": "original", "if-none-match": "original",
                    "if-modified-since": "Mon, 09 Nov 2026 00:00:00 GMT", "accept": "text/javascript",
                })),
                "只传递资源协商信息，不转发凭据或宿主请求头"
            );
        }
    }

    #[tokio::test]
    async fn oversized_resource_headers_are_rejected_before_plugin_business() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = super::super::tests::bare_state(root.path())
            .with_native_plugin_platform(Arc::new(Routes(calls.clone())));
        let app = super::super::routes().with_state(state);
        for headers in [
            vec![("range", "x".repeat(8193))],
            vec![("accept", "x".repeat(8192)), ("if-range", "x".repeat(8192))],
        ] {
            let mut request = axum::http::Request::builder()
                .uri("/plugins/acme.assets/assets/digest/instance/runtime/main.mjs");
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        assert!(calls.lock().unwrap().is_empty());
    }
}

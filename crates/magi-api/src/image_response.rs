//! 图片 HTTP 出口：图标、包内图片与文件预览共用，保持原始格式并隔离文档导航。
use axum::{
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

pub(crate) fn image_response(mime: &'static str, bytes: Vec<u8>) -> Response {
    let mut response = ([(header::CONTENT_TYPE, mime)], bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(magi_media::IMAGE_CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

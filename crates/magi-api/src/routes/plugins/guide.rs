use super::*;

const PLUGIN_AUTHORING_GUIDE: &str = include_str!("../../../../../docs/dsh-plugin-authoring.md");

pub(super) async fn guide() -> Response {
    let mut response = Response::new(axum::body::Body::from(PLUGIN_AUTHORING_GUIDE));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    response
}

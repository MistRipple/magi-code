use super::{
    config::{DaemonConfig, DaemonError},
    runtime::ShadowDaemonRuntime,
};
use axum::{Router, response::Redirect, routing::get};
use std::{env, path::PathBuf};
use tokio::net::TcpListener;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct Daemon {
    config: DaemonConfig,
}

impl Daemon {
    pub fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<(), DaemonError> {
        let runtime = ShadowDaemonRuntime::restore(&self.config)?;
        runtime.start_background_tasks();
        runtime.publish_started_event(&self.config.service_name);

        let listener = TcpListener::bind(self.config.socket_addr()?).await?;
        let app = build_application_router(runtime.router(self.config.service_name.clone()));
        info!(
            service_name = %self.config.service_name,
            host = %self.config.host,
            port = self.config.port,
            "Rust 影子后端已启动"
        );
        axum::serve(listener, app).await?;
        Ok(())
    }
}

fn build_application_router(api_router: Router) -> Router {
    let Some(web_dist_root) = resolve_web_dist_root() else {
        warn!("未找到 web/dist 构建产物，daemon 仅提供 API 路由");
        return api_router;
    };

    let web_html_path = web_dist_root.join("web.html");
    let assets_dir = web_dist_root.join("assets");

    if !web_html_path.is_file() {
        warn!(
            path = %web_html_path.display(),
            "web/dist 中缺少 web.html，daemon 仅提供 API 路由"
        );
        return api_router;
    }

    let mut app = Router::new()
        .route("/", get(|| async { Redirect::temporary("/web.html") }))
        .route_service("/web.html", ServeFile::new(web_html_path.clone()))
        .merge(api_router);

    if assets_dir.is_dir() {
        app = app.nest_service("/assets", ServeDir::new(assets_dir));
    } else {
        warn!(
            path = %assets_dir.display(),
            "web/dist 中缺少 assets 目录，静态资源可能无法完整加载"
        );
    }

    info!(
        web_dist_root = %web_dist_root.display(),
        "Rust daemon 已接入 web 构建产物，启用单端口入口"
    );
    app
}

fn resolve_web_dist_root() -> Option<PathBuf> {
    if let Ok(raw) = env::var("MAGI_WEB_DIST_ROOT") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let configured = PathBuf::from(trimmed);
            if configured.exists() {
                return Some(configured);
            }
        }
    }

    let repo_default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if repo_default.exists() {
        return Some(repo_default);
    }

    None
}

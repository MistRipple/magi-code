use super::{
    config::{DaemonConfig, DaemonError},
    runtime::DaemonRuntime,
};
use axum::{
    Router,
    response::{Html, Redirect},
    routing::get,
};
use std::{
    env,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

const WEB_DEV_ENABLED_ENV: &str = "MAGI_WEB_DEV";
const WEB_DEV_HOST_ENV: &str = "MAGI_WEB_DEV_HOST";
const WEB_DEV_PORT_ENV: &str = "MAGI_WEB_DEV_PORT";
const WEB_DEV_ROOT_ENV: &str = "MAGI_WEB_DEV_ROOT";
const DEFAULT_WEB_DEV_HOST: &str = "127.0.0.1";
const DEFAULT_WEB_DEV_PORT: u16 = 3000;
const WEB_DEV_READY_TIMEOUT: Duration = Duration::from_secs(30);
const WEB_DEV_READY_INTERVAL: Duration = Duration::from_millis(250);
const WEB_DEV_READY_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct Daemon {
    config: DaemonConfig,
}

impl Daemon {
    pub fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self) -> Result<(), DaemonError> {
        let runtime = DaemonRuntime::restore(&self.config)?;
        runtime.start_background_tasks();
        runtime.publish_started_event(&self.config.service_name);

        let frontend = resolve_frontend_mode(&self.config).await?;
        let listener = TcpListener::bind(self.config.socket_addr()?).await?;
        let app =
            build_application_router(runtime.router(self.config.service_name.clone()), &frontend);
        info!(
            service_name = %self.config.service_name,
            host = %self.config.host,
            port = self.config.port,
            "Rust 影子后端已启动"
        );
        let _frontend_guard = frontend;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

enum FrontendMode {
    Dev(WebDevServer),
    Static,
}

struct WebDevServer {
    origin: String,
    agent_origin: String,
    child: Option<Child>,
}

impl Drop for WebDevServer {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

async fn resolve_frontend_mode(config: &DaemonConfig) -> Result<FrontendMode, DaemonError> {
    if !env_flag_enabled(WEB_DEV_ENABLED_ENV) {
        return Ok(FrontendMode::Static);
    }

    let host =
        read_env_trimmed(WEB_DEV_HOST_ENV).unwrap_or_else(|| DEFAULT_WEB_DEV_HOST.to_string());
    let port = read_env_u16(WEB_DEV_PORT_ENV, DEFAULT_WEB_DEV_PORT)?;
    let origin = format!("http://{host}:{port}");
    let agent_origin = format!("http://{}:{}", browser_host(&config.host), config.port);
    let web_root = resolve_web_root();

    if is_web_dev_server_ready(&origin, &agent_origin, &web_root).await {
        info!(
            web_dev_origin = %origin,
            "已复用运行中的 Vite 前端热加载服务"
        );
        return Ok(FrontendMode::Dev(WebDevServer {
            origin,
            agent_origin,
            child: None,
        }));
    }

    if !web_root.join("package.json").is_file() {
        return Err(DaemonError::internal(format!(
            "启用 {WEB_DEV_ENABLED_ENV}=1 失败：未找到前端 package.json: {}",
            web_root.display()
        )));
    }

    info!(
        web_root = %web_root.display(),
        web_dev_origin = %origin,
        "正在启动 Vite 前端热加载服务"
    );
    let mut child = Command::new("npm")
        .arg("--prefix")
        .arg(&web_root)
        .arg("run")
        .arg("dev:daemon")
        .arg("--")
        .arg("--host")
        .arg(&host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--strictPort")
        .env("VITE_AGENT_BASE_URL", &agent_origin)
        .env("VITE_AGENT_PROXY_TARGET", &agent_origin)
        .env("MAGI_VITE_HOST", &host)
        .env("MAGI_VITE_PORT", port.to_string())
        .env("MAGI_VITE_OPEN", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| DaemonError::internal(format!("启动 Vite 前端热加载服务失败: {error}")))?;

    if let Err(error) =
        wait_for_spawned_web_dev_server(&origin, &agent_origin, &web_root, &mut child).await
    {
        let _ = child.start_kill();
        return Err(DaemonError::internal(error));
    }

    Ok(FrontendMode::Dev(WebDevServer {
        origin,
        agent_origin,
        child: Some(child),
    }))
}

fn build_application_router(api_router: Router, frontend: &FrontendMode) -> Router {
    if let FrontendMode::Dev(dev_server) = frontend {
        let dev_html = build_web_dev_html(&dev_server.origin, &dev_server.agent_origin);
        info!(
            web_dev_origin = %dev_server.origin,
            agent_origin = %dev_server.agent_origin,
            "Rust daemon 已接入 Vite 热加载前端，启用单端口开发入口"
        );
        return Router::new()
            .route("/", get(|| async { Redirect::temporary("/web.html") }))
            .route(
                "/web.html",
                get({
                    let dev_html = dev_html.clone();
                    move || {
                        let dev_html = dev_html.clone();
                        async move { Html(dev_html) }
                    }
                }),
            )
            .merge(api_router);
    }

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

fn build_web_dev_html(vite_origin: &str, agent_origin: &str) -> String {
    let vite_origin_json = serde_json::to_string(vite_origin).expect("origin should serialize");
    let agent_origin_json = serde_json::to_string(agent_origin).expect("origin should serialize");
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no" />
    <link rel="icon" href="data:," />
    <title>Magi Web</title>
    <script>
      window.__AGENT_BASE_URL__ = {agent_origin_json};
      window.__MAGI_WEB_DEV_ORIGIN__ = {vite_origin_json};
    </script>
    <script type="module" src="{vite_origin}/@vite/client"></script>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="{vite_origin}/src/main-web.ts"></script>
  </body>
</html>"#
    )
}

async fn is_web_dev_server_ready(origin: &str, agent_origin: &str, web_root: &PathBuf) -> bool {
    let origin = origin.trim_end_matches('/');
    let client = match reqwest::Client::builder()
        .timeout(WEB_DEV_READY_REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    let ready_payload = match client
        .get(format!("{origin}/__magi_vite_ready"))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>().await {
                Ok(payload) => payload,
                Err(_) => return false,
            }
        }
        _ => return false,
    };
    let expected_web_root = normalize_ready_path(web_root);
    let actual_web_root = ready_payload
        .get("webRoot")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .map(|path| normalize_ready_path(&path))
        .unwrap_or_default();
    if ready_payload.get("app").and_then(serde_json::Value::as_str) != Some("magi-web")
        || ready_payload
            .get("entry")
            .and_then(serde_json::Value::as_str)
            != Some("/src/main-web.ts")
        || ready_payload
            .get("agentOrigin")
            .and_then(serde_json::Value::as_str)
            != Some(agent_origin)
        || actual_web_root != expected_web_root
    {
        return false;
    }
    for path in ["/@vite/client", "/src/main-web.ts"] {
        match client.get(format!("{origin}{path}")).send().await {
            Ok(response) if response.status().is_success() => {}
            _ => return false,
        }
    }
    true
}

async fn wait_for_spawned_web_dev_server(
    origin: &str,
    agent_origin: &str,
    web_root: &PathBuf,
    child: &mut Child,
) -> Result<(), String> {
    let started_at = Instant::now();
    while started_at.elapsed() < WEB_DEV_READY_TIMEOUT {
        if is_web_dev_server_ready(origin, agent_origin, web_root).await {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "Vite 前端热加载服务启动后提前退出，状态: {status}；请确认 {origin} 未被其他进程占用，且该端口返回可用的 /web.html"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(format!("检查 Vite 前端热加载服务状态失败: {error}"));
            }
        }
        tokio::time::sleep(WEB_DEV_READY_INTERVAL).await;
    }
    Err(format!(
        "Vite 前端热加载服务未在 {} 秒内就绪: {origin}",
        WEB_DEV_READY_TIMEOUT.as_secs()
    ))
}

fn normalize_ready_path(path: &PathBuf) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.clone())
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string()
}

fn env_flag_enabled(name: &str) -> bool {
    read_env_trimmed(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn read_env_trimmed(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn read_env_u16(name: &str, default_value: u16) -> Result<u16, DaemonError> {
    let Some(raw) = read_env_trimmed(name) else {
        return Ok(default_value);
    };
    raw.parse::<u16>().map_err(|error| {
        DaemonError::internal(format!("{name} 必须是合法端口号，当前值 `{raw}`: {error}"))
    })
}

fn browser_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "::" => DEFAULT_WEB_DEV_HOST.to_string(),
        value => value.to_string(),
    }
}

fn resolve_web_root() -> PathBuf {
    if let Some(configured) = read_env_trimmed(WEB_DEV_ROOT_ENV) {
        return PathBuf::from(configured);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web")
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

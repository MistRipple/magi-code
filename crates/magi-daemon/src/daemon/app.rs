use super::{
    config::{DaemonConfig, DaemonError},
    runtime::DaemonRuntime,
};
use axum::{
    Router,
    body::Body,
    http::{StatusCode, Uri, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use std::{
    env,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

const WEB_DEV_ENABLED_ENV: &str = "MAGI_WEB_DEV";
const WEB_DEV_HOST_ENV: &str = "MAGI_WEB_DEV_HOST";
const WEB_DEV_PORT_ENV: &str = "MAGI_WEB_DEV_PORT";
const WEB_DEV_ROOT_ENV: &str = "MAGI_WEB_DEV_ROOT";
const DEFAULT_WEB_DEV_HOST: &str = "0.0.0.0";
const LOCAL_WEB_DEV_HOST: &str = "127.0.0.1";
const DEFAULT_WEB_DEV_PORT: u16 = 3000;
const WEB_DEV_READY_TIMEOUT: Duration = Duration::from_secs(30);
const WEB_DEV_READY_INTERVAL: Duration = Duration::from_millis(250);
const WEB_DEV_READY_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const WEB_DEV_PROXY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
        let frontend_entry_available = frontend.entry_available();
        let app =
            build_application_router(runtime.router(self.config.service_name.clone()), &frontend);
        info!(
            service_name = %self.config.service_name,
            host = %self.config.host,
            port = self.config.port,
            "Rust 影子后端已启动"
        );
        if self.config.open_browser && frontend_entry_available {
            let url = web_entry_url(&self.config);
            if let Err(error) = open_browser(&url) {
                warn!(error = %error, url = %url, "打开 Magi 界面失败");
            }
        }
        let _frontend_guard = frontend;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

enum FrontendMode {
    Dev(WebDevServer),
    Static(Option<StaticWebAssets>),
}

impl FrontendMode {
    fn entry_available(&self) -> bool {
        match self {
            FrontendMode::Dev(_) => true,
            FrontendMode::Static(assets) => assets.is_some(),
        }
    }
}

struct StaticWebAssets {
    web_html_path: PathBuf,
    assets_dir: PathBuf,
    web_dist_root: PathBuf,
}

#[derive(Clone)]
struct WebDevServer {
    inner: Arc<WebDevServerInner>,
}

struct WebDevServerInner {
    origin: String,
    agent_origin: String,
    web_root: PathBuf,
    host: String,
    port: u16,
    child: Mutex<Option<Child>>,
    recover_lock: AsyncMutex<()>,
}

impl WebDevServer {
    fn new(
        origin: String,
        agent_origin: String,
        web_root: PathBuf,
        host: String,
        port: u16,
        child: Option<Child>,
    ) -> Self {
        Self {
            inner: Arc::new(WebDevServerInner {
                origin,
                agent_origin,
                web_root,
                host,
                port,
                child: Mutex::new(child),
                recover_lock: AsyncMutex::new(()),
            }),
        }
    }

    fn origin(&self) -> &str {
        &self.inner.origin
    }

    fn agent_origin(&self) -> &str {
        &self.inner.agent_origin
    }

    async fn ensure_ready(&self) -> Result<(), String> {
        if is_web_dev_server_ready(
            &self.inner.origin,
            &self.inner.agent_origin,
            &self.inner.web_root,
        )
        .await
        {
            return Ok(());
        }

        let _recover_guard = self.inner.recover_lock.lock().await;
        if is_web_dev_server_ready(
            &self.inner.origin,
            &self.inner.agent_origin,
            &self.inner.web_root,
        )
        .await
        {
            return Ok(());
        }

        self.stop_owned_child().await?;
        let mut child = spawn_web_dev_server(
            &self.inner.host,
            self.inner.port,
            &self.inner.agent_origin,
            &self.inner.web_root,
        )?;
        if let Err(error) = wait_for_spawned_web_dev_server(
            &self.inner.origin,
            &self.inner.agent_origin,
            &self.inner.web_root,
            &mut child,
        )
        .await
        {
            let _ = child.start_kill();
            return Err(error);
        }
        self.store_owned_child(child)?;
        Ok(())
    }

    async fn stop_owned_child(&self) -> Result<(), String> {
        let mut existing = {
            let mut child = self
                .inner
                .child
                .lock()
                .map_err(|_| "Vite 前端热加载服务状态锁已损坏".to_string())?;
            child.take()
        };
        if let Some(child) = existing.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(
                        status = %status,
                        "Vite 前端热加载服务已退出，准备重新启动"
                    );
                }
                Ok(None) => {
                    warn!("Vite 前端热加载服务未通过健康检查，准备重新启动");
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
                Err(error) => {
                    warn!(
                        error = %error,
                        "检查 Vite 前端热加载服务状态失败，准备重新启动"
                    );
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
        }
        Ok(())
    }

    fn store_owned_child(&self, new_child: Child) -> Result<(), String> {
        let mut child = self
            .inner
            .child
            .lock()
            .map_err(|_| "Vite 前端热加载服务状态锁已损坏".to_string())?;
        *child = Some(new_child);
        Ok(())
    }
}

impl Drop for WebDevServerInner {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
        }
    }
}

async fn resolve_frontend_mode(config: &DaemonConfig) -> Result<FrontendMode, DaemonError> {
    if !env_flag_enabled(WEB_DEV_ENABLED_ENV) {
        return Ok(FrontendMode::Static(resolve_static_web_assets()));
    }

    let host =
        read_env_trimmed(WEB_DEV_HOST_ENV).unwrap_or_else(|| DEFAULT_WEB_DEV_HOST.to_string());
    let port = read_env_u16(WEB_DEV_PORT_ENV, DEFAULT_WEB_DEV_PORT)?;
    let origin = format!("http://{}:{port}", local_probe_host(&host));
    let agent_origin = format!("http://{}:{}", browser_host(&config.host), config.port);
    let web_root = resolve_web_root();

    if is_web_dev_server_ready(&origin, &agent_origin, &web_root).await {
        info!(
            web_dev_origin = %origin,
            "已复用运行中的 Vite 前端热加载服务"
        );
        return Ok(FrontendMode::Dev(WebDevServer::new(
            origin,
            agent_origin,
            web_root,
            host,
            port,
            None,
        )));
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
    let mut child = spawn_web_dev_server(&host, port, &agent_origin, &web_root)
        .map_err(DaemonError::internal)?;

    if let Err(error) =
        wait_for_spawned_web_dev_server(&origin, &agent_origin, &web_root, &mut child).await
    {
        let _ = child.start_kill();
        return Err(DaemonError::internal(error));
    }

    Ok(FrontendMode::Dev(WebDevServer::new(
        origin,
        agent_origin,
        web_root,
        host,
        port,
        Some(child),
    )))
}

fn build_application_router(api_router: Router, frontend: &FrontendMode) -> Router {
    if let FrontendMode::Dev(dev_server) = frontend {
        let dev_html = build_web_dev_html(dev_server.origin(), dev_server.agent_origin());
        info!(
            web_dev_origin = %dev_server.origin(),
            agent_origin = %dev_server.agent_origin(),
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
            .merge(api_router)
            .fallback(get({
                let dev_server = dev_server.clone();
                move |uri| {
                    let dev_server = dev_server.clone();
                    async move { proxy_vite_dev_asset(dev_server, uri).await }
                }
            }));
    }

    let FrontendMode::Static(static_assets) = frontend else {
        unreachable!("dev frontend handled above");
    };

    let Some(static_assets) = static_assets else {
        return api_router;
    };

    let mut app = Router::new()
        .route("/", get(|| async { Redirect::temporary("/web.html") }))
        .route_service(
            "/web.html",
            ServeFile::new(static_assets.web_html_path.clone()),
        )
        .merge(api_router);

    if static_assets.assets_dir.is_dir() {
        app = app.nest_service("/assets", ServeDir::new(static_assets.assets_dir.clone()));
    } else {
        warn!(
            path = %static_assets.assets_dir.display(),
            "web/dist 中缺少 assets 目录，静态资源可能无法完整加载"
        );
    }

    info!(
        web_dist_root = %static_assets.web_dist_root.display(),
        "Rust daemon 已接入 web 构建产物，启用单端口入口"
    );
    app
}

fn web_entry_url(config: &DaemonConfig) -> String {
    format!(
        "http://{}:{}/web.html",
        browser_host(&config.host),
        config.port
    )
}

fn open_browser(url: &str) -> std::io::Result<()> {
    let mut command = browser_open_command(url);
    command.spawn().map(|_| ())
}

fn browser_open_command(url: &str) -> StdCommand {
    #[cfg(target_os = "macos")]
    {
        let mut command = StdCommand::new("open");
        command.arg(url);
        command
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = StdCommand::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = StdCommand::new("xdg-open");
        command.arg(url);
        command
    }
}

fn build_web_dev_html(vite_origin: &str, agent_origin: &str) -> String {
    let agent_origin_json = serde_json::to_string(agent_origin).expect("origin should serialize");
    let vite_origin_json = serde_json::to_string(vite_origin).expect("origin should serialize");
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
    <script type="module" src="/@vite/client"></script>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main-web.ts"></script>
  </body>
</html>"#
    )
}

async fn proxy_vite_dev_asset(dev_server: WebDevServer, uri: Uri) -> Response {
    if !is_vite_dev_asset_path(uri.path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    match fetch_vite_dev_asset_response(&dev_server, path_and_query).await {
        Ok(response) => return response,
        Err(error) => {
            warn!(
                path = %path_and_query,
                error = %error,
                "Vite 前端热加载资源代理失败，尝试恢复"
            );
        }
    }

    if let Err(error) = dev_server.ensure_ready().await {
        warn!(
            path = %path_and_query,
            error = %error,
            "Vite 前端热加载服务恢复失败"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "前端热加载服务暂不可用，请稍后刷新重试",
        )
            .into_response();
    }

    match fetch_vite_dev_asset_response(&dev_server, path_and_query).await {
        Ok(response) => response,
        Err(error) => {
            warn!(
                path = %path_and_query,
                error = %error,
                "Vite 前端热加载资源代理重试失败"
            );
            (
                StatusCode::BAD_GATEWAY,
                "前端热加载资源暂不可用，请稍后刷新重试",
            )
                .into_response()
        }
    }
}

async fn fetch_vite_dev_asset_response(
    dev_server: &WebDevServer,
    path_and_query: &str,
) -> Result<Response, String> {
    let url = format!(
        "{}{}",
        dev_server.origin().trim_end_matches('/'),
        path_and_query
    );
    let client = reqwest::Client::builder()
        .timeout(WEB_DEV_PROXY_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("构造 Vite 资源代理客户端失败: {error}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("请求 Vite 资源失败: {error}"))?;
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(format!("读取 Vite 前端热加载资源失败: {error}"));
        }
    };
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(Body::from(bytes))
        .map_err(|error| format!("构造 Vite 资源响应失败: {error}"))
}

fn is_vite_dev_asset_path(path: &str) -> bool {
    path == "/favicon.ico"
        || path.starts_with("/@vite/")
        || path.starts_with("/@id/")
        || path.starts_with("/@fs/")
        || path.starts_with("/src/")
        || path.starts_with("/node_modules/")
        || path.starts_with("/assets/")
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

fn spawn_web_dev_server(
    host: &str,
    port: u16,
    agent_origin: &str,
    web_root: &PathBuf,
) -> Result<Child, String> {
    Command::new("npm")
        .arg("--prefix")
        .arg(web_root)
        .arg("run")
        .arg("dev:daemon")
        .arg("--")
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--strictPort")
        .env("VITE_AGENT_BASE_URL", agent_origin)
        .env("VITE_AGENT_PROXY_TARGET", agent_origin)
        .env("MAGI_VITE_HOST", host)
        .env("MAGI_VITE_PORT", port.to_string())
        .env("MAGI_VITE_OPEN", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("启动 Vite 前端热加载服务失败: {error}"))
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
        "" | "0.0.0.0" | "::" => LOCAL_WEB_DEV_HOST.to_string(),
        value => value.to_string(),
    }
}

fn local_probe_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "::" => LOCAL_WEB_DEV_HOST.to_string(),
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

    for candidate in packaged_web_dist_candidates() {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let repo_default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if repo_default.exists() {
        return Some(repo_default);
    }

    None
}

fn resolve_static_web_assets() -> Option<StaticWebAssets> {
    let Some(web_dist_root) = resolve_web_dist_root() else {
        warn!("未找到 web/dist 构建产物，daemon 仅提供 API 路由");
        return None;
    };

    let web_html_path = web_dist_root.join("web.html");
    let assets_dir = web_dist_root.join("assets");

    if !web_html_path.is_file() {
        warn!(
            path = %web_html_path.display(),
            "web/dist 中缺少 web.html，daemon 仅提供 API 路由"
        );
        return None;
    }

    Some(StaticWebAssets {
        web_html_path,
        assets_dir,
        web_dist_root,
    })
}

fn packaged_web_dist_candidates() -> Vec<PathBuf> {
    let Ok(executable_path) = env::current_exe() else {
        return Vec::new();
    };
    let Some(executable_dir) = executable_path.parent() else {
        return Vec::new();
    };
    packaged_web_dist_candidates_for_executable_dir(executable_dir)
}

fn packaged_web_dist_candidates_for_executable_dir(executable_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(executable_dir.join("resources/web/dist"));
    candidates.push(executable_dir.join("../resources/web/dist"));
    candidates.push(executable_dir.join("../Resources/web/dist"));

    if let Some(parent) = executable_dir.parent() {
        candidates.push(parent.join("resources/web/dist"));
        candidates.push(parent.join("Resources/web/dist"));
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_html_uses_daemon_origin_for_vite_modules() {
        let html = build_web_dev_html("http://127.0.0.1:3000", "http://127.0.0.1:38123");

        assert!(html.contains(r#"src="/@vite/client""#));
        assert!(html.contains(r#"src="/src/main-web.ts""#));
        assert!(!html.contains(r#"src="http://127.0.0.1:3000"#));
    }

    #[test]
    fn wildcard_hosts_use_loopback_for_local_process_probes() {
        assert_eq!(browser_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(browser_host("::"), "127.0.0.1");
        assert_eq!(local_probe_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(local_probe_host("::"), "127.0.0.1");
        assert_eq!(local_probe_host("192.168.1.2"), "192.168.1.2");
    }

    #[test]
    fn vite_dev_proxy_only_handles_frontend_asset_paths() {
        assert!(is_vite_dev_asset_path("/@vite/client"));
        assert!(is_vite_dev_asset_path("/src/main-web.ts"));
        assert!(is_vite_dev_asset_path("/node_modules/.vite/deps/chunk.js"));
        assert!(!is_vite_dev_asset_path("/api/lan-access"));
        assert!(!is_vite_dev_asset_path("/health"));
    }

    #[test]
    fn packaged_web_dist_candidates_cover_common_bundle_layouts() {
        let executable_dir = Path::new("/opt/magi/bin");
        let candidates = packaged_web_dist_candidates_for_executable_dir(executable_dir);

        assert!(candidates.contains(&PathBuf::from("/opt/magi/bin/resources/web/dist")));
        assert!(candidates.contains(&PathBuf::from("/opt/magi/bin/../resources/web/dist")));
        assert!(candidates.contains(&PathBuf::from("/opt/magi/bin/../Resources/web/dist")));
        assert!(candidates.contains(&PathBuf::from("/opt/magi/resources/web/dist")));
        assert!(candidates.contains(&PathBuf::from("/opt/magi/Resources/web/dist")));
    }
}

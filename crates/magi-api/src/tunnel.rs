//! Cloudflare Tunnel 管理器
//!
//! 职责：
//! 1. 检测/安装 cloudflared 二进制文件
//! 2. 启动/停止 Quick Tunnel（免费，无需账号）
//! 3. 生成一次性访问 token
//! 4. 解析隧道公网 URL

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const TUNNEL_ERROR_DEPENDENCY_UNAVAILABLE: &str = "tunnel_dependency_unavailable";
const TUNNEL_ERROR_START_FAILED: &str = "tunnel_start_failed";
const TUNNEL_ERROR_CONNECTION_LOST: &str = "tunnel_connection_lost";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteAccessBinding {
    workspace_id: Option<String>,
    workspace_path: Option<String>,
    session_id: Option<String>,
}

impl RemoteAccessBinding {
    pub fn new(
        workspace_id: Option<&str>,
        workspace_path: Option<&str>,
        session_id: Option<&str>,
    ) -> Self {
        Self {
            workspace_id: normalized_binding_value(workspace_id),
            workspace_path: normalized_binding_value(workspace_path),
            session_id: normalized_binding_value(session_id),
        }
    }

    pub fn web_access_url(&self, web_base_url: &str, tunnel_token: Option<&str>) -> String {
        let mut params = Vec::new();
        if let Some(token) = normalized_binding_value(tunnel_token) {
            params.push(("tunnel_token", token));
        }
        if let Some(workspace_id) = self.workspace_id.as_deref() {
            params.push(("workspaceId", workspace_id.to_string()));
        }
        if let Some(workspace_path) = self.workspace_path.as_deref() {
            params.push(("workspacePath", workspace_path.to_string()));
        }
        if let Some(session_id) = self.session_id.as_deref() {
            params.push(("sessionId", session_id.to_string()));
        }
        if params.is_empty() {
            return web_base_url.to_string();
        }
        let query = params
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencoding::encode(&value)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{web_base_url}?{query}")
    }
}

fn normalized_binding_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TunnelState {
    pub status: String,
    #[serde(rename = "publicUrl")]
    pub public_url: Option<String>,
    #[serde(rename = "accessUrl")]
    pub access_url: Option<String>,
    pub token: Option<String>,
    pub error: Option<String>,
}

impl Default for TunnelState {
    fn default() -> Self {
        Self {
            status: "stopped".into(),
            public_url: None,
            access_url: None,
            token: None,
            error: None,
        }
    }
}

struct TunnelInner {
    state: TunnelState,
    child: Option<Child>,
    local_port: u16,
    binding: RemoteAccessBinding,
}

#[derive(Clone)]
pub struct TunnelManager {
    inner: Arc<Mutex<TunnelInner>>,
}

impl TunnelManager {
    pub fn new(local_port: u16) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TunnelInner {
                state: TunnelState::default(),
                child: None,
                local_port,
                binding: RemoteAccessBinding::default(),
            })),
        }
    }

    pub async fn get_state(&self) -> TunnelState {
        self.inner.lock().await.state.clone()
    }

    pub async fn local_port(&self) -> u16 {
        self.inner.lock().await.local_port
    }

    pub async fn start(&self, binding: RemoteAccessBinding) -> TunnelState {
        let mut inner = self.inner.lock().await;
        if inner.state.status == "running" || inner.state.status == "starting" {
            inner.binding = binding;
            if let (Some(public_url), Some(token)) = (
                inner.state.public_url.as_deref(),
                inner.state.token.as_deref(),
            ) {
                inner.state.access_url = Some(
                    inner
                        .binding
                        .web_access_url(&format!("{public_url}/web.html"), Some(token)),
                );
            }
            return inner.state.clone();
        }
        inner.binding = binding;

        // 查找 cloudflared
        let bin_path = match resolve_cloudflared_path().await {
            Some(p) => p,
            None => {
                inner.state.status = "installing".into();
                match install_cloudflared().await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "cloudflared install failed");
                        inner.state.status = "error".into();
                        inner.state.error = Some(TUNNEL_ERROR_DEPENDENCY_UNAVAILABLE.to_string());
                        return inner.state.clone();
                    }
                }
            }
        };

        // 生成 token
        let token = generate_token();
        inner.state.token = Some(token.clone());
        inner.state.status = "starting".into();
        inner.state.error = None;

        let port = inner.local_port;

        // 启动子进程
        let result = Command::new(&bin_path)
            .args(["tunnel", "--url", &format!("http://127.0.0.1:{port}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        match result {
            Ok(mut child) => {
                let stderr = child.stderr.take();
                inner.child = Some(child);

                // 在后台任务中解析公网 URL，并持续监听 cloudflared 是否提前退出。
                let inner_clone = self.inner.clone();
                let token_clone = token.clone();
                tokio::spawn(async move {
                    let mut has_public_url = false;
                    if let Some(stderr) = stderr {
                        let reader = BufReader::new(stderr);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Some(url) = extract_tunnel_url(&line) {
                                let mut inner = inner_clone.lock().await;
                                if inner.state.status != "starting" {
                                    continue;
                                }
                                inner.state.public_url = Some(url.clone());
                                inner.state.status = "running".into();
                                inner.state.access_url = Some(inner.binding.web_access_url(
                                    &format!("{url}/web.html"),
                                    Some(&token_clone),
                                ));
                                has_public_url = true;
                            }
                        }
                    }

                    let mut inner = inner_clone.lock().await;
                    if inner.child.is_some()
                        && (inner.state.status == "starting" || inner.state.status == "running")
                    {
                        inner.child = None;
                        inner.state.status = "error".into();
                        inner.state.public_url = None;
                        inner.state.access_url = None;
                        inner.state.token = None;
                        inner.state.error = Some(if has_public_url {
                            TUNNEL_ERROR_CONNECTION_LOST.to_string()
                        } else {
                            TUNNEL_ERROR_START_FAILED.to_string()
                        });
                    }
                });

                inner.state.clone()
            }
            Err(e) => {
                tracing::warn!(error = %e, "cloudflared process spawn failed");
                inner.state.status = "error".into();
                inner.state.error = Some(TUNNEL_ERROR_START_FAILED.to_string());
                inner.state.token = None;
                inner.state.clone()
            }
        }
    }

    pub async fn stop(&self) -> TunnelState {
        let mut inner = self.inner.lock().await;
        if let Some(ref mut child) = inner.child {
            let _ = child.kill().await;
        }
        inner.child = None;
        inner.state = TunnelState::default();
        inner.binding = RemoteAccessBinding::default();
        inner.state.clone()
    }
}

fn magi_bin_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
        .join("bin")
}

fn cloudflared_bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

async fn resolve_cloudflared_path() -> Option<PathBuf> {
    // 1. ~/.magi/bin/
    let local = magi_bin_dir().join(cloudflared_bin_name());
    if local.exists() {
        return Some(local);
    }
    // 2. PATH
    let which = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    if let Ok(output) = tokio::process::Command::new(which)
        .arg("cloudflared")
        .output()
        .await
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() && Path::new(&path).exists() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

async fn install_cloudflared() -> Result<PathBuf, String> {
    let bin_dir = magi_bin_dir();
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let dest = bin_dir.join(cloudflared_bin_name());

    let url = resolve_download_url().ok_or("不支持的平台/架构")?;
    let is_tgz = url.ends_with(".tgz");

    // 使用 curl 下载
    let download_dest = if is_tgz {
        bin_dir.join("cloudflared.tgz")
    } else {
        dest.clone()
    };

    let status = tokio::process::Command::new("curl")
        .args(["-fsSL", "-o", download_dest.to_str().unwrap(), url])
        .status()
        .await
        .map_err(|e| format!("curl 执行失败: {e}"))?;

    if !status.success() {
        return Err("curl 下载失败".into());
    }

    if is_tgz {
        let status = tokio::process::Command::new("tar")
            .args([
                "-xzf",
                download_dest.to_str().unwrap(),
                "-C",
                bin_dir.to_str().unwrap(),
            ])
            .status()
            .await
            .map_err(|e| format!("tar 解压失败: {e}"))?;
        if !status.success() {
            return Err("tar 解压失败".into());
        }
        let _ = std::fs::remove_file(&download_dest);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }

    if dest.exists() {
        Ok(dest)
    } else {
        Err("安装完成但未找到 cloudflared".into())
    }
}

fn resolve_download_url() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64.tgz",
        ),
        ("macos", "x86_64") => Some(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz",
        ),
        ("linux", "aarch64") => Some(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64",
        ),
        ("linux", "x86_64") => Some(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64",
        ),
        ("windows", "x86_64") => Some(
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe",
        ),
        _ => None,
    }
}

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{:x}", ts, rand_u64())
}

fn rand_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::Instant::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    hasher.finish()
}

fn extract_tunnel_url(line: &str) -> Option<String> {
    // cloudflared 输出格式: ... https://xxx-xxx.trycloudflare.com ...
    let _re_pattern = "https://[a-zA-Z0-9-]+\\.trycloudflare\\.com";
    for word in line.split_whitespace() {
        if word.starts_with("https://") && word.contains(".trycloudflare.com") {
            // 去掉末尾的标点
            let clean = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '.');
            return Some(clean.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_state_error_uses_stable_marker() {
        let state = TunnelState {
            status: "error".to_string(),
            public_url: None,
            access_url: None,
            token: None,
            error: Some(TUNNEL_ERROR_START_FAILED.to_string()),
        };

        let value = serde_json::to_value(state).expect("tunnel state should serialize");
        assert_eq!(value["error"], serde_json::json!("tunnel_start_failed"));
        assert!(
            !value
                .to_string()
                .contains("cloudflared failed: permission denied")
        );
    }

    #[test]
    fn tunnel_error_markers_do_not_expose_process_details() {
        for marker in [
            TUNNEL_ERROR_DEPENDENCY_UNAVAILABLE,
            TUNNEL_ERROR_START_FAILED,
            TUNNEL_ERROR_CONNECTION_LOST,
        ] {
            assert!(!marker.contains(' '));
            assert!(!marker.contains("cloudflared"));
        }
    }

    #[test]
    fn remote_access_binding_preserves_workspace_session_scope() {
        let binding = RemoteAccessBinding::new(
            Some("workspace-a"),
            Some("/Users/xie/code/TEST"),
            Some("session-a"),
        );

        let url = binding.web_access_url("https://example.trycloudflare.com/web.html", Some("t k"));

        assert!(url.contains("tunnel_token=t%20k"));
        assert!(url.contains("workspaceId=workspace-a"));
        assert!(url.contains("workspacePath=%2FUsers%2Fxie%2Fcode%2FTEST"));
        assert!(url.contains("sessionId=session-a"));
    }
}

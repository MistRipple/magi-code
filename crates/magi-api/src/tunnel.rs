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
            })),
        }
    }

    pub async fn get_state(&self) -> TunnelState {
        self.inner.lock().await.state.clone()
    }

    pub async fn start(&self, workspace_id: Option<&str>) -> TunnelState {
        let mut inner = self.inner.lock().await;
        if inner.state.status == "running" || inner.state.status == "starting" {
            return inner.state.clone();
        }

        // 查找 cloudflared
        let bin_path = match resolve_cloudflared_path().await {
            Some(p) => p,
            None => {
                inner.state.status = "installing".into();
                match install_cloudflared().await {
                    Ok(p) => p,
                    Err(e) => {
                        inner.state.status = "error".into();
                        inner.state.error = Some(format!("安装 cloudflared 失败: {e}"));
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
        let ws_id = workspace_id.map(|s| s.to_string());

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

                // 在后台任务中解析公网 URL
                let inner_clone = self.inner.clone();
                let token_clone = token.clone();
                let ws_id_clone = ws_id.clone();
                tokio::spawn(async move {
                    if let Some(stderr) = stderr {
                        let reader = BufReader::new(stderr);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Some(url) = extract_tunnel_url(&line) {
                                let mut inner = inner_clone.lock().await;
                                inner.state.public_url = Some(url.clone());
                                inner.state.status = "running".into();
                                // 构造带 token 的访问 URL
                                let mut access = format!("{url}/web.html");
                                let mut params = vec![format!("tunnel_token={token_clone}")];
                                if let Some(ref ws) = ws_id_clone {
                                    params.push(format!("workspaceId={ws}"));
                                }
                                access = format!("{access}?{}", params.join("&"));
                                inner.state.access_url = Some(access);
                                break;
                            }
                        }
                    }
                });

                inner.state.clone()
            }
            Err(e) => {
                inner.state.status = "error".into();
                inner.state.error = Some(format!("启动隧道失败: {e}"));
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
    if cfg!(target_os = "windows") { "cloudflared.exe" } else { "cloudflared" }
}

async fn resolve_cloudflared_path() -> Option<PathBuf> {
    // 1. ~/.magi/bin/
    let local = magi_bin_dir().join(cloudflared_bin_name());
    if local.exists() {
        return Some(local);
    }
    // 2. PATH
    let which = if cfg!(target_os = "windows") { "where" } else { "which" };
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
            .args(["-xzf", download_dest.to_str().unwrap(), "-C", bin_dir.to_str().unwrap()])
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

    if dest.exists() { Ok(dest) } else { Err("安装完成但未找到 cloudflared".into()) }
}

fn resolve_download_url() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64.tgz"),
        ("macos", "x86_64") => Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz"),
        ("linux", "aarch64") => Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64"),
        ("linux", "x86_64") => Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64"),
        ("windows", "x86_64") => Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe"),
        _ => None,
    }
}

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
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

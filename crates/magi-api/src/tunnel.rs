//! Cloudflare Tunnel 管理器
//!
//! 职责：
//! 1. 检测/安装 cloudflared 二进制文件
//! 2. 启动/停止 Quick Tunnel（免费，无需账号）
//! 3. 生成一次性访问 token
//! 4. 解析隧道公网 URL

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const TUNNEL_ERROR_DEPENDENCY_UNAVAILABLE: &str = "tunnel_dependency_unavailable";
const TUNNEL_ERROR_START_FAILED: &str = "tunnel_start_failed";
const TUNNEL_ERROR_CONNECTION_LOST: &str = "tunnel_connection_lost";
const CLOUDFLARED_VERSION: &str = "2026.7.1";

#[derive(Clone, Copy)]
struct DownloadArtifact {
    url: &'static str,
    sha256: &'static str,
}

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

    pub async fn authorize_public_request(&self, candidate: Option<&str>) -> bool {
        let inner = self.inner.lock().await;
        let Some(expected) = inner.state.token.as_deref() else {
            return false;
        };
        candidate.is_some_and(|candidate| constant_time_eq(expected, candidate))
    }

    #[cfg(test)]
    pub(crate) fn new_with_token_for_test(local_port: u16, token: &str) -> Self {
        let state = TunnelState {
            token: Some(token.to_string()),
            ..TunnelState::default()
        };
        Self {
            inner: Arc::new(Mutex::new(TunnelInner {
                state,
                child: None,
                local_port,
                binding: RemoteAccessBinding::default(),
            })),
        }
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
        let token = match generate_token() {
            Ok(token) => token,
            Err(error) => {
                tracing::error!(error = %error, "secure tunnel token generation failed");
                inner.state.status = "error".into();
                inner.state.error = Some(TUNNEL_ERROR_START_FAILED.to_string());
                return inner.state.clone();
            }
        };
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

fn managed_cloudflared_bin_name() -> String {
    if cfg!(target_os = "windows") {
        format!("cloudflared-{CLOUDFLARED_VERSION}.exe")
    } else {
        format!("cloudflared-{CLOUDFLARED_VERSION}")
    }
}

async fn resolve_cloudflared_path() -> Option<PathBuf> {
    // 1. ~/.magi/bin/
    let local = magi_bin_dir().join(managed_cloudflared_bin_name());
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
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() && Path::new(&path).exists() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

async fn install_cloudflared() -> Result<PathBuf, String> {
    let bin_dir = magi_bin_dir();
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let dest = bin_dir.join(managed_cloudflared_bin_name());

    let artifact = resolve_download_artifact().ok_or("不支持的平台/架构")?;
    let is_tgz = artifact.url.ends_with(".tgz");

    // 使用 curl 下载
    let download_dest = if is_tgz {
        bin_dir.join("cloudflared.tgz")
    } else {
        dest.clone()
    };

    let status = tokio::process::Command::new("curl")
        .args(["-fsSL", "-o", download_dest.to_str().unwrap(), artifact.url])
        .status()
        .await
        .map_err(|e| format!("curl 执行失败: {e}"))?;

    if !status.success() {
        return Err("curl 下载失败".into());
    }

    if let Err(error) = verify_sha256(&download_dest, artifact.sha256) {
        let _ = std::fs::remove_file(&download_dest);
        return Err(error);
    }

    if is_tgz {
        let extract_dir = bin_dir.join(format!("cloudflared-{CLOUDFLARED_VERSION}-extract"));
        let _ = std::fs::remove_dir_all(&extract_dir);
        std::fs::create_dir_all(&extract_dir).map_err(|error| error.to_string())?;
        let status = tokio::process::Command::new("tar")
            .args([
                "-xzf",
                download_dest.to_str().unwrap(),
                "-C",
                extract_dir.to_str().unwrap(),
            ])
            .status()
            .await
            .map_err(|e| format!("tar 解压失败: {e}"))?;
        if !status.success() {
            return Err("tar 解压失败".into());
        }
        let extracted = extract_dir.join(cloudflared_bin_name());
        std::fs::rename(&extracted, &dest)
            .map_err(|error| format!("安装 cloudflared 失败: {error}"))?;
        let _ = std::fs::remove_dir_all(&extract_dir);
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

fn resolve_download_artifact() -> Option<DownloadArtifact> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(DownloadArtifact {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.7.1/cloudflared-darwin-arm64.tgz",
            sha256: "6d4b59383cdad387834d7ae5704fc512882b2d078074bf5770e02b186a0981ed",
        }),
        ("macos", "x86_64") => Some(DownloadArtifact {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.7.1/cloudflared-darwin-amd64.tgz",
            sha256: "05871d772745b0f8398c7be89113a0b178474936ff20638b3b07c0e7262f717e",
        }),
        ("linux", "aarch64") => Some(DownloadArtifact {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.7.1/cloudflared-linux-arm64",
            sha256: "18f2c9bfc7a67a971bd96f1a5a1935def3c1e52aa386626f1566f04e9b5478d6",
        }),
        ("linux", "x86_64") => Some(DownloadArtifact {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.7.1/cloudflared-linux-amd64",
            sha256: "79a0ade7fc854f62c1aaef48424d9d979e8c2fcd039189d24db82b84cd146be1",
        }),
        ("windows", "x86_64") => Some(DownloadArtifact {
            url: "https://github.com/cloudflare/cloudflared/releases/download/2026.7.1/cloudflared-windows-amd64.exe",
            sha256: "ccb0756de288d3c2c076d19764ca53e0849a10f2dd9c23f8656ac42bdeb45001",
        }),
        _ => None,
    }
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if constant_time_eq(&actual, expected) {
        Ok(())
    } else {
        Err("cloudflared 完整性校验失败".to_string())
    }
}

fn generate_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
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

    #[tokio::test]
    async fn public_tunnel_request_requires_current_token() {
        let manager = TunnelManager::new_with_token_for_test(38123, "secret-token");

        assert!(!manager.authorize_public_request(Some("wrong-token")).await);
        assert!(!manager.authorize_public_request(None).await);
        assert!(manager.authorize_public_request(Some("secret-token")).await);
    }

    #[test]
    fn generated_tokens_have_256_bits_of_hex_entropy() {
        let token = generate_token().expect("token generation should succeed");
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(
            token,
            generate_token().expect("second token should succeed")
        );
    }

    #[test]
    fn managed_binary_requires_matching_sha256() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let path = dir.path().join("cloudflared");
        std::fs::write(&path, b"verified binary").expect("fixture should write");

        assert!(
            verify_sha256(
                &path,
                "86fd6fb55a10988213329d914da3f5fbbc213ee143b46148ed21b60d9454e3dc",
            )
            .is_ok()
        );
        assert!(verify_sha256(&path, &"0".repeat(64)).is_err());
    }

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

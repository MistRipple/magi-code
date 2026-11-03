//! 本地传输：Unix 域套接字（0600）/ Windows 命名管道（仅本机客户端）。
//!
//! 每个宿主实例独占一个端点，只接受一个连接；连接建立后立即释放监听。
//! 访问控制只证明“连上来的是同一用户的进程”，身份仍由握手令牌确认。

use std::path::{Path, PathBuf};

/// 为实例生成端点。Unix 返回套接字路径；Windows 返回命名管道名。
/// 名称包含随机后缀，避免同用户其他进程预测并抢占。
pub fn new_endpoint(dir: &Path, tag: &str) -> std::io::Result<PathBuf> {
    let mut random = [0u8; 6];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("{tag}-{suffix}");
    #[cfg(unix)]
    {
        // macOS 的 sockaddr_un 路径上限约 104 字节；状态根可由用户指定为很深的
        // 路径，直接把它拼进 socket 名会导致宿主永远在端点错误中重启。长路径
        // 改用系统临时目录，并用状态目录摘要保持实例目录隔离，目录权限仍为 0700。
        let candidate = dir.join(format!("{name}.sock"));
        let (endpoint_dir, endpoint_name) = if candidate.as_os_str().len() >= 90 {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
            let short: String = digest
                .iter()
                .take(10)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            // macOS 的用户 TMPDIR 自身可能长达 70 字节；使用该平台标准的
            // 短临时根，socket 仍受私有目录权限与一次性令牌保护。
            let temporary_root = if cfg!(target_os = "macos") {
                PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            };
            (
                temporary_root.join(format!("magi-ext-{short}")),
                format!("{name}.sock"),
            )
        } else {
            (dir.to_path_buf(), format!("{name}.sock"))
        };
        std::fs::create_dir_all(&endpoint_dir)?;
        if !std::fs::symlink_metadata(&endpoint_dir)?
            .file_type()
            .is_dir()
        {
            return Err(std::io::Error::other("扩展端点目录不能是符号链接"));
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&endpoint_dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(endpoint_dir.join(endpoint_name))
    }
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(PathBuf::from(format!(r"\\.\pipe\magi-ext-{name}")))
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use tokio::net::{UnixListener, UnixStream};

    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
    }

    pub type Stream = UnixStream;

    pub fn bind(endpoint: &Path) -> std::io::Result<Listener> {
        let inner = UnixListener::bind(endpoint)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            inner,
            path: endpoint.to_path_buf(),
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> std::io::Result<Stream> {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    pub struct Listener {
        server: Option<NamedPipeServer>,
    }

    pub type Stream = NamedPipeServer;

    pub fn bind(endpoint: &Path) -> std::io::Result<Listener> {
        // 先创建首个实例：同名管道已存在则明确失败，不与其他进程共享信任边界。
        let server = ServerOptions::new()
            .reject_remote_clients(true)
            .first_pipe_instance(true)
            .create(endpoint.as_os_str())?;
        Ok(Listener {
            server: Some(server),
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> std::io::Result<Stream> {
            let server = self
                .server
                .take()
                .ok_or_else(|| std::io::Error::other("监听已被消费"))?;
            server.connect().await?;
            Ok(server)
        }
    }
}

pub use imp::{Listener, Stream, bind};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn long_state_roots_use_a_short_private_socket_path() {
        let root = tempfile::tempdir().unwrap();
        let long = root.path().join("x".repeat(120));
        let endpoint = new_endpoint(&long, "g1").unwrap();
        assert!(endpoint.as_os_str().len() < 104);
        let listener = bind(&endpoint).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&endpoint).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        assert!(!endpoint.exists());
        std::fs::remove_dir(endpoint.parent().unwrap()).unwrap();
    }
}

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
        std::fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(dir.join(format!("{name}.sock")))
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

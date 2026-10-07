//! `openai/tunnel-client` 的托管安装。
//!
//! 用户不需要自己下载或安装任何第三方工具：启用 GPT Web 工具通道时由 Magi 下载**固定版本**的
//! 官方发布包，先核对发布包的 SHA-256（固定在代码里），再解出可执行文件，并把解出后的文件摘要写进
//! 旁路 `.sha256`，之后每次启动都会按它复核。与 cloudflared 的按需安装是同一类做法。
//!
//! 安装位置固定在 `state_root/web-model/tunnel-client/v<版本>/`，不污染系统目录。

use std::path::{Path, PathBuf};

use magi_web_model::TUNNEL_CLIENT_VERSION;
use sha2::{Digest, Sha256};

const RELEASE_BASE: &str = "https://github.com/openai/tunnel-client/releases/download";

/// 固定的发布包：`(os, arch, SHA-256)`。摘要来自该版本发布的 `SHA256SUMS.txt`。
const PINNED: &[(&str, &str, &str)] = &[
    (
        "darwin",
        "amd64",
        "9dcae1e2fb121287e73271edb7b853dda52aa86b7bfca1df91bc275371261bdb",
    ),
    (
        "darwin",
        "arm64",
        "b2cae3aa9df45b4c2fe9b1d700ebacce39f9feb6a6b46b86e6499f9a51bf72ff",
    ),
    (
        "linux",
        "amd64",
        "8c836dc5d68d68b663d9a5c5b28ff9fa780d9f7a3fffb1c306880b8f32fab5f1",
    ),
    (
        "linux",
        "arm64",
        "c51bfd883fc22e3445494a03c0179875176564bde470661b308fd83af5d01abb",
    ),
    (
        "windows",
        "amd64",
        "3b53133a1e24d43f63088d843860cb1701a4c3ed6390de2e19f69089e43bddc1",
    ),
    (
        "windows",
        "arm64",
        "571e0d59ed9e86d1b105dc34f3267865f654de6968b01efd7c847f0af657d11d",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub url: String,
    pub sha256: &'static str,
}

fn platform() -> Option<(&'static str, &'static str)> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some((os, arch))
}

pub(crate) fn pinned_artifact_for(os: &str, arch: &str) -> Option<Artifact> {
    let (_, _, sha256) = PINNED.iter().find(|(o, a, _)| *o == os && *a == arch)?;
    Some(Artifact {
        url: format!(
            "{RELEASE_BASE}/v{TUNNEL_CLIENT_VERSION}/tunnel-client-v{TUNNEL_CLIENT_VERSION}-{os}-{arch}.zip"
        ),
        sha256,
    })
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    }
}

/// 托管安装的可执行文件位置。
pub(crate) fn managed_binary_path(state_root: &Path) -> PathBuf {
    state_root
        .join("web-model")
        .join("tunnel-client")
        .join(format!("v{TUNNEL_CLIENT_VERSION}"))
        .join(binary_name())
}

fn sidecar_of(binary: &Path) -> PathBuf {
    PathBuf::from(format!("{}.sha256", binary.to_string_lossy()))
}

/// 可执行文件与它的摘要旁路文件都在才算装好；摘要是否匹配由启动前的预检负责。
pub(crate) fn is_installed(binary: &Path) -> bool {
    binary.is_file() && sidecar_of(binary).is_file()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 下载、校验并安装固定版本。已安装则直接返回路径。
pub(crate) async fn install(state_root: &Path) -> Result<PathBuf, String> {
    let binary = managed_binary_path(state_root);
    if is_installed(&binary) {
        return Ok(binary);
    }
    let (os, arch) = platform().ok_or("当前平台没有可用的 tunnel-client 发布包")?;
    let artifact =
        pinned_artifact_for(os, arch).ok_or("当前平台没有可用的 tunnel-client 发布包")?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|error| format!("无法创建下载客户端：{error}"))?;
    let response = client
        .get(&artifact.url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("下载 tunnel-client 失败：{error}"))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("下载 tunnel-client 中断：{error}"))?;
    if !sha256_hex(&bytes).eq_ignore_ascii_case(artifact.sha256) {
        return Err("tunnel-client 发布包校验失败（SHA-256 不符），已放弃安装".to_string());
    }
    let target = binary.clone();
    tokio::task::spawn_blocking(move || extract_binary_from_zip(&bytes, &target))
        .await
        .map_err(|error| format!("解压 tunnel-client 失败：{error}"))??;
    Ok(binary)
}

/// 从发布包里取出 `tunnel-client` 可执行文件，写入目标路径并生成摘要旁路文件。
fn extract_binary_from_zip(zip_bytes: &[u8], target: &Path) -> Result<(), String> {
    use std::io::{Cursor, Read, Write};

    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|error| format!("发布包不是有效的 zip：{error}"))?;
    let wanted = binary_name();
    let mut contents = None;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取发布包失败：{error}"))?;
        let is_binary = entry
            .enclosed_name()
            .and_then(|name| name.file_name().map(|file| file == wanted))
            .unwrap_or(false);
        if entry.is_file() && is_binary {
            let mut buffer = Vec::new();
            entry
                .read_to_end(&mut buffer)
                .map_err(|error| format!("解压失败：{error}"))?;
            contents = Some(buffer);
            break;
        }
    }
    let contents = contents.ok_or_else(|| format!("发布包里没有 {wanted}"))?;
    let parent = target.parent().ok_or("安装路径无效")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("无法创建安装目录：{error}"))?;
    let partial = target.with_extension("part");
    {
        let mut file = std::fs::File::create(&partial)
            .map_err(|error| format!("无法写入 tunnel-client：{error}"))?;
        file.write_all(&contents)
            .map_err(|error| format!("无法写入 tunnel-client：{error}"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("无法设置可执行权限：{error}"))?;
    }
    std::fs::rename(&partial, target).map_err(|error| format!("无法完成安装：{error}"))?;
    std::fs::write(
        sidecar_of(target),
        format!("{}  {}\n", sha256_hex(&contents), binary_name()),
    )
    .map_err(|error| format!("无法写入校验文件：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pinned_platform_has_a_https_url_and_a_sha256() {
        for (os, arch, sha) in PINNED {
            let artifact = pinned_artifact_for(os, arch).expect("pinned");
            assert!(
                artifact
                    .url
                    .starts_with("https://github.com/openai/tunnel-client/")
            );
            assert!(
                artifact
                    .url
                    .contains(&format!("-v{TUNNEL_CLIENT_VERSION}-{os}-{arch}.zip"))
            );
            assert_eq!(sha.len(), 64);
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert!(pinned_artifact_for("plan9", "amd64").is_none());
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            for (name, bytes) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn extracting_writes_an_executable_and_a_matching_digest_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("v1").join(binary_name());
        let zip = zip_with(&[
            ("tunnel-client-v1/README.md", b"docs"),
            (
                &format!("tunnel-client-v1/{}", binary_name()),
                b"binary-bytes",
            ),
        ]);
        extract_binary_from_zip(&zip, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"binary-bytes");
        assert!(is_installed(&target));
        let sidecar = std::fs::read_to_string(sidecar_of(&target)).unwrap();
        assert!(sidecar.starts_with(&sha256_hex(b"binary-bytes")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
    }

    #[test]
    fn an_archive_without_the_binary_is_rejected_and_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(binary_name());
        let zip = zip_with(&[("README.md", b"docs")]);
        assert!(extract_binary_from_zip(&zip, &target).is_err());
        assert!(!is_installed(&target));
    }

    /// 手动验证：`MAGI_TEST_TUNNEL_ZIP=<官方发布包> cargo test -p magi-api -- --ignored real_release`。
    #[test]
    #[ignore = "needs the real tunnel-client release zip"]
    fn real_release_zip_extracts_into_a_runnable_binary() {
        let Some(zip_path) = std::env::var_os("MAGI_TEST_TUNNEL_ZIP") else {
            return;
        };
        let bytes = std::fs::read(zip_path).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = managed_binary_path(dir.path());
        extract_binary_from_zip(&bytes, &target).unwrap();
        assert!(is_installed(&target));
        let output = magi_process::std_command(&target)
            .arg("--version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains(TUNNEL_CLIENT_VERSION));
    }
}

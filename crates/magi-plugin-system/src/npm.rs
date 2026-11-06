//! npm 来源：上游 tarball 的完整性核对与安全解包。
//!
//! 只做纯函数式的字节处理：取包、下载与进程调用由 API／daemon 层负责。这里保证两件事：
//! tarball 的 SHA-512 与 registry 公布的 `dist.integrity` 完全一致；解包结果不含路径穿越、
//! 符号链接、硬链接、特殊文件，且数量与体积有界。解包不运行任何上游脚本。

use crate::{PluginError, invalid};
use base64::Engine as _;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha512};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

/// tarball（压缩态）上限。
pub const MAX_TARBALL_BYTES: usize = 64 * 1024 * 1024;
/// 解包后总量上限。
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// 校验 SSRI 形式的 `sha512-<base64>`，与 tarball 字节的摘要比较。
pub fn verify_integrity(tarball: &[u8], integrity: &str) -> Result<(), PluginError> {
    let expected = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| invalid("npm integrity 必须是 sha512-… 形式"))?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(expected)
        .map_err(|_| invalid("npm integrity 编码无效"))?;
    if expected.len() != 64 {
        return Err(invalid("npm integrity 长度无效"));
    }
    let actual = Sha512::digest(tarball);
    if actual.as_slice() != expected.as_slice() {
        return Err(invalid("上游 tarball 的摘要与 registry 公布的 integrity 不一致"));
    }
    Ok(())
}

/// 解包 npm tarball 到 `target`（必须是已存在的空目录）。tarball 内容在 `package/` 前缀下。
pub fn unpack_tarball(tarball: &[u8], target: &Path) -> Result<(), PluginError> {
    if tarball.is_empty() || tarball.len() > MAX_TARBALL_BYTES {
        return Err(invalid("上游 tarball 大小超限"));
    }
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));
    let mut count = 0usize;
    let mut total = 0u64;
    for entry in archive
        .entries()
        .map_err(|_| invalid("上游 tarball 不是有效的 gzip tar"))?
    {
        let mut entry = entry.map_err(|_| invalid("无法读取上游 tarball 条目"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(invalid("上游 tarball 条目过多"));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        // 只接受普通文件。符号链接、硬链接、设备与 FIFO 一律拒绝，而不是跳过。
        if !kind.is_file() {
            return Err(invalid("上游 tarball 含符号链接或特殊文件"));
        }
        let size = entry.header().size().unwrap_or(u64::MAX);
        if size > MAX_ENTRY_BYTES {
            return Err(invalid("上游 tarball 含超大文件"));
        }
        total = total.saturating_add(size);
        if total > MAX_UNPACKED_BYTES {
            return Err(invalid("上游 tarball 解包后体积超限"));
        }
        let raw = entry
            .path()
            .map_err(|_| invalid("上游 tarball 含无效路径"))?
            .into_owned();
        let relative = package_relative(&raw)?;
        let destination = target.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::with_capacity(size as usize);
        entry
            .by_ref()
            .take(size + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("读取上游 tarball 文件失败"))?;
        if bytes.len() as u64 != size {
            return Err(invalid("上游 tarball 文件大小与头部声明不符"));
        }
        // 不使用 tar 的 unpack：它会保留可执行位等权限，而这里只需要内容。
        std::fs::write(&destination, bytes)?;
    }
    if !target.join("package.json").is_file() {
        return Err(invalid("上游 tarball 缺少 package/package.json"));
    }
    Ok(())
}

/// 去掉 npm 的 `package/` 前缀并拒绝任何穿越：路径必须由普通分量组成。
fn package_relative(raw: &Path) -> Result<PathBuf, PluginError> {
    let mut components = raw.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "package" => {}
        _ => return Err(invalid("上游 tarball 内容必须位于 package/ 下")),
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => return Err(invalid("上游 tarball 含路径穿越")),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(invalid("上游 tarball 含空路径"));
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    fn build(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn integrity(bytes: &[u8]) -> String {
        format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
        )
    }

    #[test]
    fn integrity_must_match_exactly() {
        let tarball = build(&[("package/package.json", b"{}")]);
        assert!(verify_integrity(&tarball, &integrity(&tarball)).is_ok());
        let mut other = tarball.clone();
        other.push(0);
        assert!(verify_integrity(&other, &integrity(&tarball)).is_err());
        assert!(verify_integrity(&tarball, "sha1-AAAA").is_err());
        assert!(verify_integrity(&tarball, "sha512-AAAA").is_err());
    }

    #[test]
    fn unpacks_regular_files_without_package_prefix() {
        let tarball = build(&[
            ("package/package.json", b"{\"name\":\"x\"}"),
            ("package/lib/index.js", b"export {}"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        unpack_tarball(&tarball, dir.path()).unwrap();
        assert!(dir.path().join("package.json").is_file());
        assert_eq!(
            std::fs::read(dir.path().join("lib/index.js")).unwrap(),
            b"export {}"
        );
    }

    #[test]
    fn rejects_traversal_foreign_roots_and_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        // tar 构造器会拒绝 `..`；直接写头部字节模拟恶意包。
        let mut raw = tar::Header::new_gnu();
        raw.set_size(1);
        raw.set_mode(0o644);
        let name = b"package/../escape.txt";
        raw.as_old_mut().name[..name.len()].copy_from_slice(name);
        raw.set_cksum();
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        builder.append(&raw, &b"x"[..]).unwrap();
        let evil = builder.into_inner().unwrap().finish().unwrap();
        assert!(unpack_tarball(&evil, dir.path()).is_err());
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());

        let foreign = build(&[("other/package.json", b"{}")]);
        assert!(unpack_tarball(&foreign, dir.path()).is_err());

        let no_manifest = build(&[("package/lib/index.js", b"x")]);
        let fresh = tempfile::tempdir().unwrap();
        assert!(unpack_tarball(&no_manifest, fresh.path()).is_err());
    }

    #[test]
    fn rejects_links_and_special_entries() {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder
            .append_link(&mut header, "package/link", "/etc/passwd")
            .unwrap();
        let tarball = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(unpack_tarball(&tarball, dir.path()).is_err());
    }
}

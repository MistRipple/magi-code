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
        return Err(invalid(
            "上游 tarball 的摘要与 registry 公布的 integrity 不一致",
        ));
    }
    Ok(())
}

/// 解包 npm tarball 到 `target`（必须是已存在的空目录）。tarball 内容在 `package/` 前缀下。
pub fn unpack_tarball(tarball: &[u8], target: &Path) -> Result<(), PluginError> {
    unpack(tarball, target, Some("package".into()))
}

/// 仓库归档与 npm 使用同一解包边界；仓库根名由首条文件确定，全包只能有一个根。
pub fn unpack_repository_tarball(tarball: &[u8], target: &Path) -> Result<(), PluginError> {
    unpack(tarball, target, None)
}

/// Git archive 在首个全局 PAX header 中记录实际提交。身份来自这份已下载的归档，
/// 调用方据此取固定 SHA 的归档，不依赖第二种引用解析服务或公共 REST 查询配额。
pub fn repository_commit(tarball: &[u8]) -> Result<String, PluginError> {
    if tarball.is_empty() || tarball.len() > MAX_TARBALL_BYTES {
        return Err(invalid("上游 tarball 大小超限"));
    }
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));
    let mut entries = archive
        .entries()
        .map_err(|_| invalid("仓库归档不是有效的 gzip tar"))?;
    let mut entry = entries
        .next()
        .ok_or_else(|| invalid("仓库归档为空"))?
        .map_err(|_| invalid("仓库归档元数据无法读取"))?;
    if !entry.header().entry_type().is_pax_global_extensions()
        || entry.header().size().unwrap_or(u64::MAX) > 64 * 1024
    {
        return Err(invalid("仓库归档缺少有界的 Git 提交元数据"));
    }
    let metadata = entry
        .pax_extensions()
        .map_err(|_| invalid("仓库归档 PAX 元数据无效"))?
        .ok_or_else(|| invalid("仓库归档缺少 Git 提交元数据"))?;
    let mut commit = None;
    for item in metadata {
        let item = item.map_err(|_| invalid("仓库归档 PAX 记录无效"))?;
        if item.key_bytes() != b"comment" {
            continue;
        }
        let value = item.value().map_err(|_| invalid("仓库归档提交身份无效"))?;
        if commit.is_some() || value.len() != 40 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("仓库归档提交身份缺失、重复或无效"));
        }
        commit = Some(value.to_ascii_lowercase());
    }
    commit.ok_or_else(|| invalid("仓库归档缺少 Git 提交身份"))
}

fn unpack(tarball: &[u8], target: &Path, mut root: Option<String>) -> Result<(), PluginError> {
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
        // GitHub/git archive 带有全局 PAX comment（提交身份），它只是归档元数据，
        // 不生成文件，也不改变已校验的路径。tar crate 已处理本地 PAX 路径，
        // 实际文件仍逐项经过下方同一准入与配额；不把链接或设备当作元数据跳过。
        if kind.is_pax_global_extensions() {
            let size = entry.header().size().unwrap_or(u64::MAX);
            total = total.saturating_add(size);
            if size > 64 * 1024 || total > MAX_UNPACKED_BYTES {
                return Err(invalid("上游归档元数据超限"));
            }
            continue;
        }
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
        if root.is_none() {
            root = raw.components().next().and_then(|part| match part {
                Component::Normal(name) => name.to_str().map(str::to_owned),
                _ => None,
            });
        }
        let relative = archive_relative(&raw, root.as_deref().unwrap_or_default())?;
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
        // 不使用 tar 的 unpack：只为已分区的原生辅助文件保留执行意图，
        // 不恢复 setuid/setgid、任意写入权限或普通 JS 文件的执行位。
        std::fs::write(&destination, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable =
                relative.starts_with("native") && entry.header().mode().unwrap_or(0) & 0o111 != 0;
            std::fs::set_permissions(
                &destination,
                std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
            )?;
        }
    }
    if !target.join("package.json").is_file() {
        return Err(invalid("上游 tarball 缺少 package/package.json"));
    }
    Ok(())
}

/// 去掉归档的单一根前缀并拒绝任何穿越：路径必须由普通分量组成。
fn archive_relative(raw: &Path, root: &str) -> Result<PathBuf, PluginError> {
    let mut components = raw.components();
    match components.next() {
        Some(Component::Normal(first)) if first == root => {}
        _ => return Err(invalid("上游归档内容必须位于同一个根目录下")),
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
    fn repository_archives_share_bounds_and_refuse_multiple_roots() {
        let dir = tempfile::tempdir().unwrap();
        let archive = build(&[
            ("repo-commit/package.json", b"{}"),
            ("repo-commit/src/index.js", b"export {}"),
        ]);
        unpack_repository_tarball(&archive, dir.path()).unwrap();
        assert!(dir.path().join("src/index.js").is_file());
        let other = tempfile::tempdir().unwrap();
        let archive = build(&[
            ("first/package.json", b"{}"),
            ("second/overwrite.js", b"{}"),
        ]);
        assert!(unpack_repository_tarball(&archive, other.path()).is_err());
        assert!(!other.path().join("overwrite.js").exists());
        assert!(unpack_tarball(&archive, other.path()).is_err());
    }

    #[test]
    fn git_pax_commit_metadata_does_not_become_a_file_or_hide_special_entries() {
        let build_git = |link: bool| {
            let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
            let comment = b"52 comment=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(comment.len() as u64);
            header.set_cksum();
            archive
                .append_data(&mut header, "pax_global_header", &comment[..])
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "repo-sha/package.json", &b"{}"[..])
                .unwrap();
            if link {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_link_name("../../outside").unwrap();
                header.set_cksum();
                archive
                    .append_data(&mut header, "repo-sha/link", std::io::empty())
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            repository_commit(&build_git(false)).unwrap(),
            "a".repeat(40)
        );
        unpack_repository_tarball(&build_git(false), dir.path()).unwrap();
        assert!(dir.path().join("package.json").is_file());
        assert!(!dir.path().join("pax_global_header").exists());
        let dir = tempfile::tempdir().unwrap();
        assert!(unpack_repository_tarball(&build_git(true), dir.path()).is_err());
        assert!(!dir.path().join("link").exists());
    }

    #[test]
    fn repository_identity_rejects_missing_invalid_duplicate_and_unbounded_metadata() {
        let with_metadata = |records: &[(&str, &str)]| {
            let mut payload = Vec::new();
            for (key, value) in records {
                let content = format!("{key}={value}\n");
                let mut length = content.len() + 2;
                loop {
                    let actual = content.len() + length.to_string().len() + 1;
                    if actual == length {
                        break;
                    }
                    length = actual;
                }
                payload.extend_from_slice(format!("{length} {content}").as_bytes());
            }
            let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(payload.len() as u64);
            header.set_cksum();
            archive
                .append_data(&mut header, "pax_global_header", &payload[..])
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap()
        };
        let sha = "Ab".repeat(20);
        assert_eq!(
            repository_commit(&with_metadata(&[("comment", &sha)])).unwrap(),
            sha.to_ascii_lowercase()
        );
        for records in [
            vec![("other", "value")],
            vec![("comment", "not-a-sha")],
            vec![("comment", sha.as_str()), ("comment", sha.as_str())],
        ] {
            assert!(repository_commit(&with_metadata(&records)).is_err());
        }
        assert!(repository_commit(&with_metadata(&[("comment", &"a".repeat(65536))])).is_err());
        assert!(repository_commit(&build(&[("package/package.json", b"{}")])).is_err());
        assert!(repository_commit(b"invalid archive").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn native_execution_intent_survives_unpack_without_special_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (name, mode) in [
            ("package/package.json", 0o755),
            ("package/native/linux/helper", 0o6751),
            ("package/native/linux/data", 0o644),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(mode);
            header.set_cksum();
            builder.append_data(&mut header, name, &b"{}"[..]).unwrap();
        }
        let tarball = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unpack_tarball(&tarball, dir.path()).unwrap();
        for (name, mode) in [
            ("package.json", 0o644),
            ("native/linux/helper", 0o755),
            ("native/linux/data", 0o644),
        ] {
            assert_eq!(
                std::fs::metadata(dir.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                mode
            );
        }
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

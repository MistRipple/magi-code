//! Original npm package preparation; no SDK 2 manifest, code inspection or execution.

use super::{unpack_tarball, verify_integrity};
use crate::{PluginError, invalid};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Artifact identity owned by the installation transaction, independent of its runtime contributions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePackageIdentity {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub archive_sha256: String,
}

/// Prepared package in a private staging directory. The caller owns commit/cancel and cleanup.
/// Node's module resolver owns export conditions; this layer never substitutes require for import.
#[derive(Debug)]
pub struct NativePackage {
    identity: NativePackageIdentity,
    directory: PathBuf,
    manifest: Map<String, Value>,
    manifest_sha256: String,
}

impl NativePackage {
    pub fn prepare(
        tarball: &[u8],
        integrity: &str,
        expected_name: &str,
        expected_version: &str,
        staging: &Path,
    ) -> Result<Self, PluginError> {
        if !is_valid_package_name(expected_name)
            || semver::Version::parse(expected_version).is_err()
        {
            return Err(invalid("原包需要有效 npm 包名和精确版本"));
        }
        let pkg = Self::prepare_tarball(tarball, integrity, staging)?;
        if pkg.identity.name != expected_name || pkg.identity.version != expected_version {
            return Err(invalid("原包身份与选定的 npm 包名、版本不一致"));
        }
        Ok(pkg)
    }

    pub fn prepare_tarball(
        tarball: &[u8],
        integrity: &str,
        staging: &Path,
    ) -> Result<Self, PluginError> {
        verify_integrity(tarball, integrity)?;
        let metadata = fs::symlink_metadata(staging)?;
        if !metadata.file_type().is_dir() || fs::read_dir(staging)?.next().is_some() {
            return Err(invalid("原包准备目录必须是真实的空目录"));
        }
        unpack_tarball(tarball, staging)?;
        let file = staging.join("package.json");
        if fs::metadata(&file)?.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("原包 package.json 超过大小上限"));
        }
        let bytes = fs::read(&file)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("原包 package.json 无效：{error}")))?;
        let manifest = value
            .as_object()
            .ok_or_else(|| invalid("原包 package.json 必须是对象"))?
            .clone();
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| is_valid_package_name(name))
            .ok_or_else(|| invalid("原包 package.json 缺少有效的 npm 包名"))?
            .to_owned();
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .filter(|version| semver::Version::parse(version).is_ok())
            .ok_or_else(|| invalid("原包 package.json 缺少有效的语义版本"))?
            .to_owned();
        if let Some(kind) = manifest.get("type")
            && !matches!(kind.as_str(), Some("module" | "commonjs"))
        {
            return Err(invalid("原包 type 必须是 module 或 commonjs"));
        }
        for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
            if let Some(value) = manifest.get(field) {
                let entries = value
                    .as_object()
                    .ok_or_else(|| invalid(format!("原包 {field} 必须是对象")))?;
                if entries.iter().any(|(name, range)| {
                    !is_valid_package_name(name)
                        || !range.as_str().is_some_and(|value| !value.trim().is_empty())
                }) {
                    return Err(invalid(format!("原包 {field} 含无效包名或依赖声明")));
                }
            }
        }
        validate_entries(&manifest)?;
        Ok(Self {
            identity: NativePackageIdentity {
                name,
                version,
                integrity: integrity.into(),
                archive_sha256: magi_core::hex_lower(Sha256::digest(tarball)),
            },
            directory: fs::canonicalize(staging)?,
            manifest,
            manifest_sha256: magi_core::hex_lower(Sha256::digest(&bytes)),
        })
    }

    pub fn identity(&self) -> &NativePackageIdentity {
        &self.identity
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn manifest(&self) -> &Map<String, Value> {
        &self.manifest
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
}

/// Public npm package name, with an optional scope. Also suitable for registry URL paths.
pub fn is_valid_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let part = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part
                .bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || b"-_.~".contains(&ch))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, name)| part(scope) && part(name))
    } else {
        part(name)
    }
}

fn validate_entries(manifest: &Map<String, Value>) -> Result<(), PluginError> {
    if let Some(exports) = manifest.get("exports") {
        let (backend, client) = match exports {
            Value::Object(map) if map.keys().any(|key| key.starts_with('.')) => {
                if map.keys().any(|key| !key.starts_with('.')) {
                    return Err(invalid("原包 exports 不能混用条件和子路径键"));
                }
                (map.get("."), map.get("./client"))
            }
            value => (Some(value), None),
        };
        let backend = backend.ok_or_else(|| invalid("原包 exports 未开放后端入口 ."))?;
        let mut targets = Vec::new();
        collect_targets(backend, &mut targets, 0)?;
        if targets.is_empty() {
            return Err(invalid("原包没有可用的后端入口声明"));
        }
        if let Some(client) = client {
            collect_targets(client, &mut targets, 0)?;
        }
        for target in targets {
            if !target.starts_with("./") {
                return Err(invalid("原包 exports 目标必须位于包内"));
            }
            check_target(&target[2..])?;
        }
    } else {
        let main = match manifest.get("main") {
            Some(Value::String(value)) => value.strip_prefix("./").unwrap_or(value),
            Some(_) => return Err(invalid("原包 main 必须是字符串")),
            None => "index.js",
        };
        check_target(main)?;
    }
    Ok(())
}

/// Validate declared resources, without choosing or rewriting Node export conditions.
fn collect_targets<'a>(
    value: &'a Value,
    out: &mut Vec<&'a str>,
    depth: usize,
) -> Result<(), PluginError> {
    if depth > 16 {
        return Err(invalid("原包 exports 条件嵌套过深"));
    }
    match value {
        Value::Null => {}
        Value::String(target) => out.push(target),
        Value::Array(items) => {
            for item in items {
                collect_targets(item, out, depth + 1)?;
            }
        }
        Value::Object(conditions) => {
            // TypeScript declarations are not runtime entry resources. Other conditions
            // remain declarations here; Node decides which one applies to this host.
            for (condition, target) in conditions {
                if condition != "types" {
                    collect_targets(target, out, depth + 1)?;
                }
            }
        }
        _ => return Err(invalid("原包 exports 目标或条件结构无效")),
    }
    Ok(())
}

fn check_target(relative: &str) -> Result<(), PluginError> {
    if relative.is_empty()
        || relative.contains(['\\', '%', '*', '?', '#'])
        || Path::new(relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || relative
            .split('/')
            .any(|part| part == "node_modules" || part.is_empty())
    {
        return Err(invalid("原包入口必须是包内的普通文件路径"));
    }
    // Node checks the selected entry's existence; an unused platform condition
    // may legitimately point at a file absent from this package's platform build.
    Ok(())
}

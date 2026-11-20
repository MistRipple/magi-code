//! Build private, immutable candidate projects and materialize the candidate loading view.
use magi_api::{ApiError, NpmArchive};
use magi_core::fs_atomic::write_atomic;
use magi_extension_host::shared::PackageInspection;
use magi_plugin_system::{loading_runtime::LoadingPreparation, npm::NativePackageIdentity};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub struct PluginDependencyToolchain<'a> {
    pub node: &'a Path,
    pub npm_cli: &'a Path,
    pub sdk_project: &'a Path,
    pub registry: &'a str,
}

/// The caller owns an empty staging directory through cancellation and commit.
/// This prepares dependencies and verifies their binding, with no PluginManager writes.
/// An empty `archives` slice produces a valid empty candidate project (when the last bundle is uninstalled).
pub async fn prepare_plugin_dependencies(
    process_group: &ManagedProcessGroup,
    toolchain: &PluginDependencyToolchain<'_>,
    project: &Path,
    archives: &[NpmArchive],
) -> Result<Vec<(PackageInspection, NativePackageIdentity)>, ApiError> {
    let project = fs::canonicalize(project).map_err(io_error)?;
    if fs::read_dir(&project).map_err(io_error)?.next().is_some() {
        return Err(ApiError::InvalidInput(
            "插件依赖准备需要空的候选目录".into(),
        ));
    }
    if archives.is_empty() {
        fs::create_dir(project.join("node_modules")).map_err(io_error)?;
        let empty_manifest = serde_json::to_vec_pretty(&json!({
            "name": "magi-native-plugin-project",
            "private": true,
            "dependencies": {},
        }))
        .map_err(io_error)?;
        write_atomic(&project.join("package.json"), empty_manifest).map_err(io_error)?;
        return Ok(Vec::new());
    }
    fs::create_dir(project.join("archives")).map_err(io_error)?;
    fs::create_dir(project.join("originals")).map_err(io_error)?;
    let mut packages = Vec::with_capacity(archives.len());
    let mut originals = Vec::with_capacity(archives.len());
    for (index, archive) in archives.iter().enumerate() {
        let archive_path = format!("archives/{index}.tgz");
        let original_directory = format!("originals/{index}");
        fs::create_dir(project.join(&original_directory)).map_err(io_error)?;
        let extracted = archive.stage(&project.join(&original_directory))?;
        archive
            .write_archive(&project.join(&archive_path))
            .map_err(io_error)?;
        let identity = extracted.identity();
        originals.push(json!({
            "name": identity.name, "version": identity.version, "integrity": identity.integrity,
            "archive": archive_path, "manifest": format!("{original_directory}/package.json"),
            "manifestSha256": extracted.manifest_sha256(),
        }));
        packages.push((
            PackageInspection {
                project_directory: project.clone(),
                directory: project.join("node_modules").join(&identity.name),
                name: identity.name.clone(),
                version: identity.version.clone(),
                manifest_sha256: extracted.manifest_sha256().into(),
            },
            identity.clone(),
        ));
    }
    let input = project.join("preparation.json");
    fs::write(
        &input,
        serde_json::to_vec(&json!({
            "project": project, "sdkProject": toolchain.sdk_project, "npm": toolchain.npm_cli,
            "registry": toolchain.registry, "originals": originals,
        }))
        .map_err(io_error)?,
    )
    .map_err(io_error)?;
    let mut command = magi_process::tokio_command(toolchain.node);
    command
        .arg("--experimental-import-meta-resolve")
        .arg(
            toolchain
                .sdk_project
                .join("runtime/prepare-dependencies.mjs"),
        )
        .arg(&input)
        .env_remove("NODE_OPTIONS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = process_group.spawn_tokio(&mut command).map_err(io_error)?;
    let mut diagnostic = child.take_stderr().expect("piped stderr").take(8192);
    let result = async {
        let mut bytes = Vec::new();
        diagnostic.read_to_end(&mut bytes).await.map_err(io_error)?;
        drop(diagnostic);
        if !child.wait().await.map_err(io_error)?.success() {
            return Err(ApiError::InvalidInput(format!(
                "解析插件依赖失败：{}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(180), result)
        .await
        .map_err(|_| ApiError::Conflict("解析插件依赖超时，安装尚未提交".into()))??;
    fs::remove_file(input).map_err(io_error)?;
    fs::remove_dir_all(project.join("originals")).map_err(io_error)?;
    Ok(packages)
}

/// Read the verified original `.tgz` archives retained inside an accepted `LoadingPreparation`.
pub fn read_retained_archives(
    preparation: &LoadingPreparation,
) -> Result<BTreeMap<String, NpmArchive>, ApiError> {
    if preparation.packages.is_empty() {
        return Ok(BTreeMap::new());
    }
    let manifest_bytes =
        fs::read(preparation.project_directory.join("package.json")).map_err(io_error)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(io_error)?;
    let deps = manifest["dependencies"]
        .as_object()
        .ok_or_else(|| io_error("已接纳插件项目的 package.json 缺少 dependencies"))?;
    let mut archives = BTreeMap::new();
    for pkg in &preparation.packages {
        let spec = deps.get(&pkg.name).and_then(Value::as_str).ok_or_else(|| {
            io_error(format!(
                "已接纳插件项目的 dependencies 未记录原包 {}",
                pkg.name
            ))
        })?;
        let rel = spec
            .strip_prefix("file:./")
            .or_else(|| spec.strip_prefix("file:"))
            .ok_or_else(|| {
                io_error(format!(
                    "已接纳插件项目原包 {} 归档路径格式无效：{spec}",
                    pkg.name
                ))
            })?;
        if !rel.starts_with("archives/") || rel.contains("..") {
            return Err(io_error(format!(
                "已接纳插件项目原包 {} 归档路径越界：{rel}",
                pkg.name
            )));
        }
        let bytes = fs::read(preparation.project_directory.join(rel)).map_err(io_error)?;
        let archive = NpmArchive::from_tarball_bytes(bytes)?;
        if archive.name() != pkg.name || archive.version() != pkg.version {
            return Err(io_error(format!(
                "已接纳插件归档身份与记录不一致：期望 {}@{}，实际 {}@{}",
                pkg.name,
                pkg.version,
                archive.name(),
                archive.version()
            )));
        }
        archives.insert(pkg.name.clone(), archive);
    }
    Ok(archives)
}

/// Reuse the physical package directory of unchanged packages from `previous` via a symlink inside
/// `new_project/node_modules/<pkg>`, so `realpath` resolves to the exact same `entry` and unrelated
/// running plugins keep their live Cordis fibers and active gateway leases across bundle mutations.
pub fn preserve_unchanged_package_links(
    new_project: &Path,
    packages: &[PackageInspection],
    previous: &LoadingPreparation,
) -> Result<(), ApiError> {
    let node_modules = new_project.join("node_modules");
    for pkg in packages {
        let Some(prev_pkg) = previous.packages.iter().find(|old| {
            old.name == pkg.name
                && old.version == pkg.version
                && old.manifest_sha256 == pkg.manifest_sha256
        }) else {
            continue;
        };
        let Ok(canonical_old) = fs::canonicalize(&prev_pkg.directory) else {
            continue;
        };
        let target_slot = link_target_for_package(&node_modules, &pkg.name)?;
        if fs::canonicalize(&target_slot).ok().as_ref() == Some(&canonical_old) {
            continue;
        }
        if fs::symlink_metadata(&target_slot).is_ok() {
            remove_link_or_dir(&target_slot).map_err(io_error)?;
        }
        if let Some(parent) = target_slot.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        create_dir_link(&canonical_old, &target_slot).map_err(io_error)?;
    }
    Ok(())
}

/// Materialize `package.json` and `node_modules/<pkg>` links inside `source_root` so
/// DSH plugins inspecting the profile directory (`ctx.loader` -> `cordis.yml` sibling `package.json`
/// and `node_modules`) see the exact committed `LoadingPreparation` without mutating immutable package dirs.
pub fn materialize_candidate_project_view(
    source_root: &Path,
    preparation: &LoadingPreparation,
) -> Result<(), ApiError> {
    fs::create_dir_all(source_root).map_err(io_error)?;
    let mut dependencies = serde_json::Map::new();
    let mut expected_links: BTreeSet<String> = BTreeSet::new();
    let mut sorted_packages: Vec<&PackageInspection> = preparation.packages.iter().collect();
    sorted_packages.sort_by(|a, b| a.name.cmp(&b.name));
    for pkg in &sorted_packages {
        dependencies.insert(pkg.name.clone(), Value::String(pkg.version.clone()));
        expected_links.insert(pkg.name.clone());
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&json!({
        "name": "magi-dsh-loading-project",
        "private": true,
        "type": "module",
        "dependencies": dependencies,
    }))
    .map_err(io_error)?;
    manifest_bytes.push(b'\n');
    write_atomic(&source_root.join("package.json"), &manifest_bytes).map_err(io_error)?;

    let node_modules = source_root.join("node_modules");
    fs::create_dir_all(&node_modules).map_err(io_error)?;

    // Prune stale entries first so removed packages disappear immediately.
    for entry in fs::read_dir(&node_modules).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let entry_path = entry.path();
        if name_str.starts_with('@')
            && fs::symlink_metadata(&entry_path)
                .map_err(io_error)?
                .is_dir()
        {
            for scoped in fs::read_dir(&entry_path).map_err(io_error)? {
                let scoped = scoped.map_err(io_error)?;
                let scoped_name = scoped.file_name();
                let Some(sub) = scoped_name.to_str() else {
                    continue;
                };
                let full_name = format!("{name_str}/{sub}");
                if !expected_links.contains(&full_name) {
                    remove_link_or_dir(&scoped.path()).map_err(io_error)?;
                }
            }
            if fs::read_dir(&entry_path)
                .map_err(io_error)?
                .next()
                .is_none()
            {
                let _ = fs::remove_dir(&entry_path);
            }
        } else if !expected_links.contains(name_str) {
            remove_link_or_dir(&entry_path).map_err(io_error)?;
        }
    }

    for pkg in sorted_packages {
        let link_path = link_target_for_package(&node_modules, &pkg.name)?;
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let needs_relink = match fs::symlink_metadata(&link_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    fs::read_link(&link_path).ok().as_deref() != Some(pkg.directory.as_path())
                } else {
                    true
                }
            }
            Err(_) => true,
        };
        if needs_relink {
            if fs::symlink_metadata(&link_path).is_ok() {
                remove_link_or_dir(&link_path).map_err(io_error)?;
            }
            create_dir_link(&pkg.directory, &link_path).map_err(io_error)?;
        }
    }
    Ok(())
}

fn link_target_for_package(node_modules: &Path, name: &str) -> Result<PathBuf, ApiError> {
    if name.is_empty() || name.contains("..") || name.contains('\\') {
        return Err(ApiError::InvalidInput(format!("插件包名非法：{name}")));
    }
    let mut target = node_modules.to_path_buf();
    for segment in name.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ApiError::InvalidInput(format!("插件包名非法：{name}")));
        }
        target.push(segment);
    }
    Ok(target)
}

fn remove_link_or_dir(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
}

#[cfg(unix)]
fn create_dir_link(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
fn create_dir_link(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(original, link)
}

fn io_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal_assembly("准备插件安装项目失败", error)
}

//! Authoritative daemon coordinator for `ctx.pluginManager` and native bundle operations.
//! All package mutations stage immutable candidate projects and commit through `PluginManager`.

use crate::plugin_dependencies::{
    PluginDependencyToolchain, prepare_plugin_dependencies, preserve_unchanged_package_links,
    read_retained_archives,
};
use magi_api::{ApiError, NpmArchive, fetch_npm_packument, fetch_npm_release_archive};
use magi_core::fs_atomic::write_atomic;
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::{PackageInspection, SharedHost},
};
use magi_plugin_system::{
    LoadingSourceEdit, PluginManager,
    loading_runtime::{LoadingPreparation, LoadingRuntime},
    npm::is_valid_package_name,
};
use magi_process::ManagedProcessGroup;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

const MAX_SETTLED_INSTALLS: usize = 32;
const VERSION_LIST_LIMIT: usize = 20;

#[derive(Clone, Debug)]
pub struct PluginBundleManagementConfig {
    pub npm_cli: PathBuf,
    pub projects_dir: PathBuf,
    pub registry: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum CallerIdentity {
    #[serde(rename_all = "camelCase")]
    Host { host_generation: u64 },
    #[serde(rename_all = "camelCase")]
    Plugin {
        host_generation: u64,
        plugin_id: String,
        instance_id: String,
        activation_generation: u64,
        version: String,
        configuration_epoch: u64,
    },
}

impl CallerIdentity {
    fn host_generation(&self) -> u64 {
        match self {
            Self::Host { host_generation }
            | Self::Plugin {
                host_generation, ..
            } => *host_generation,
        }
    }

    fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::Host { .. } => None,
            Self::Plugin { plugin_id, .. } => Some(plugin_id.as_str()),
        }
    }

    fn validate(
        &self,
        expected_host_generation: u64,
        owner: &PluginManager,
    ) -> Result<(), RpcError> {
        if self.host_generation() != expected_host_generation {
            return Err(RpcError::new(
                ErrorKind::StaleGeneration,
                "插件管理请求的宿主代次已失效",
            ));
        }
        if let Self::Plugin {
            host_generation,
            plugin_id,
            instance_id,
            activation_generation,
            version,
            configuration_epoch,
        } = self
        {
            let valid_binding =
                owner
                    .loading_instance_binding(instance_id)
                    .is_some_and(|activation| {
                        activation.host_generation == *host_generation
                            && activation.activation_generation == *activation_generation
                            && activation.executable.plugin_id == *plugin_id
                            && activation.executable.version == *version
                    });
            if !valid_binding
                || !owner.loading_configuration_scope_current(instance_id, *configuration_epoch)
            {
                return Err(RpcError::new(
                    ErrorKind::PermissionDenied,
                    "插件管理请求的调用方运行作用域已失效",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallPhase {
    Installing,
    Applying,
}

struct ActiveInstallEntry {
    cancel_tx: watch::Sender<bool>,
    phase: InstallPhase,
    result_rx: watch::Receiver<Option<Value>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct InstallKey {
    plugin_caller: Option<CallerIdentity>,
    request_id: String,
}

impl InstallKey {
    fn new(caller: Option<&CallerIdentity>, request_id: &str) -> Self {
        Self {
            plugin_caller: caller
                .filter(|caller| caller.plugin_id().is_some())
                .cloned(),
            request_id: request_id.to_owned(),
        }
    }
}

#[derive(Default)]
struct InstallRegistry {
    active: HashMap<InstallKey, ActiveInstallEntry>,
    settled: VecDeque<(InstallKey, Value)>,
}

impl InstallRegistry {
    fn record_settled(&mut self, key: InstallKey, value: Value) {
        if let Some(pos) = self.settled.iter().position(|(id, _)| id == &key) {
            self.settled.remove(pos);
        }
        if self.settled.len() >= MAX_SETTLED_INSTALLS {
            self.settled.pop_front();
        }
        self.settled.push_back((key, value));
    }
}

#[derive(Clone, Debug)]
enum ParsedInstallSpec {
    Registry {
        name: String,
        exact_version: Option<String>,
    },
    TarballPath(PathBuf),
}

fn parse_install_spec(raw: &str) -> Result<ParsedInstallSpec, String> {
    let spec = raw.trim();
    if spec.is_empty() {
        return Err("安装规格不能为空".into());
    }
    if let Some(path_str) = spec.strip_prefix("file:") {
        let path = PathBuf::from(path_str);
        if path.is_absolute() && path_str.ends_with(".tgz") {
            return Ok(ParsedInstallSpec::TarballPath(path));
        }
        return Err(format!("仅支持绝对路径 .tgz 归档规格：{spec}"));
    }
    if Path::new(spec).is_absolute() && spec.ends_with(".tgz") {
        return Ok(ParsedInstallSpec::TarballPath(PathBuf::from(spec)));
    }
    let (name, version_part) = if let Some(rest) = spec.strip_prefix('@') {
        match rest.find('@') {
            Some(idx) => (&spec[..idx + 1], Some(&rest[idx + 1..])),
            None => (spec, None),
        }
    } else {
        match spec.find('@') {
            Some(idx) => (&spec[..idx], Some(&spec[idx + 1..])),
            None => (spec, None),
        }
    };
    if !is_valid_package_name(name) {
        return Err(format!("无效的 npm 包名：{name}"));
    }
    match version_part {
        None | Some("latest") => Ok(ParsedInstallSpec::Registry {
            name: name.to_owned(),
            exact_version: None,
        }),
        Some(ver) => {
            semver::Version::parse(ver)
                .map_err(|_| format!("仅支持精确语义版本或 latest：{ver}"))?;
            Ok(ParsedInstallSpec::Registry {
                name: name.to_owned(),
                exact_version: Some(ver.to_owned()),
            })
        }
    }
}

pub struct PluginManagementService {
    owner: Arc<Mutex<PluginManager>>,
    group: ManagedProcessGroup,
    node: PathBuf,
    sdk_project: PathBuf,
    config: PluginBundleManagementConfig,
    current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
    current_loading: Arc<RwLock<Option<Arc<LoadingRuntime>>>>,
    mutation_lock: tokio::sync::Mutex<()>,
    installs: Mutex<InstallRegistry>,
    project_sequence: AtomicU64,
}

impl PluginManagementService {
    pub(crate) fn new(
        owner: Arc<Mutex<PluginManager>>,
        group: ManagedProcessGroup,
        node: PathBuf,
        sdk_project: PathBuf,
        config: PluginBundleManagementConfig,
        current_host: Arc<RwLock<Option<Arc<SharedHost>>>>,
        current_loading: Arc<RwLock<Option<Arc<LoadingRuntime>>>>,
    ) -> Result<Arc<Self>, ApiError> {
        fs::create_dir_all(&config.projects_dir)
            .map_err(|e| ApiError::internal_assembly("创建插件候选项目目录失败", e))?;
        Ok(Arc::new(Self {
            owner,
            group,
            node,
            sdk_project,
            config,
            current_host,
            current_loading,
            mutation_lock: tokio::sync::Mutex::new(()),
            installs: Mutex::new(InstallRegistry::default()),
            project_sequence: AtomicU64::new(1),
        }))
    }

    pub fn registry(&self) -> &str {
        &self.config.registry
    }

    pub(crate) fn materialize_npmrc(&self, source_root: &Path) -> Result<(), ApiError> {
        fs::create_dir_all(source_root)
            .map_err(|e| ApiError::internal_assembly("创建插件来源目录失败", e))?;
        let content = format!("registry={}\n", self.config.registry.trim_end_matches('/'));
        write_atomic(&source_root.join(".npmrc"), content.as_bytes())
            .map_err(|e| ApiError::internal_assembly("写入加载目录 .npmrc 失败", e))
    }

    fn active_handles(&self) -> Result<(Arc<SharedHost>, Arc<LoadingRuntime>), ApiError> {
        let host = self
            .current_host
            .read()
            .expect("host lock")
            .clone()
            .ok_or_else(|| ApiError::Conflict("共享插件宿主当前未运行".into()))?;
        let loading = self
            .current_loading
            .read()
            .expect("loading lock")
            .clone()
            .ok_or_else(|| ApiError::Conflict("插件加载协调器当前未运行".into()))?;
        Ok((host, loading))
    }

    /// Native host bundle installation entry (Magi Settings / API).
    pub async fn install_bundle(
        &self,
        spec: &str,
        enabled: bool,
        request_id: Option<&str>,
        registry_override: Option<&str>,
    ) -> Result<Value, ApiError> {
        let (host, loading) = self.active_handles()?;
        let caller = CallerIdentity::Host {
            host_generation: host.generation(),
        };
        Ok(self
            .execute_install(
                &host,
                &loading,
                &caller,
                spec,
                InstallExecOptions {
                    enabled,
                    request_id: request_id.map(str::to_owned),
                    registry_override: registry_override.map(str::to_owned),
                    expected: None,
                    confirmed_digest: None,
                },
            )
            .await)
    }

    pub(crate) async fn install_confirmed_bundle(
        &self,
        spec: &str,
        expected_digest: String,
        request_id: Option<String>,
    ) -> Result<Value, ApiError> {
        let (host, loading) = self.active_handles()?;
        let caller = CallerIdentity::Host {
            host_generation: host.generation(),
        };
        Ok(self
            .execute_install(
                &host,
                &loading,
                &caller,
                spec,
                InstallExecOptions {
                    enabled: true,
                    request_id,
                    registry_override: None,
                    expected: None,
                    confirmed_digest: Some(expected_digest),
                },
            )
            .await)
    }

    /// Native single-step atomic bundle upgrade entry (replaces candidate version in one commit,
    /// including self-upgrades of management bundles without first leaving an uninstalled gap).
    pub async fn upgrade_bundle(
        &self,
        name: &str,
        version: Option<&str>,
        request_id: Option<&str>,
        registry_override: Option<&str>,
    ) -> Result<Value, ApiError> {
        let (host, loading) = self.active_handles()?;
        let caller = CallerIdentity::Host {
            host_generation: host.generation(),
        };
        Ok(self
            .execute_update(
                &host,
                &loading,
                &caller,
                name,
                version,
                request_id,
                registry_override,
            )
            .await)
    }

    /// Native bundle removal entry.
    pub async fn remove_bundle(&self, name: &str) -> Result<Value, ApiError> {
        let (host, loading) = self.active_handles()?;
        let caller = CallerIdentity::Host {
            host_generation: host.generation(),
        };
        Ok(self.execute_remove(&host, &loading, &caller, name).await)
    }

    /// Native bundle enable/disable entry.
    pub async fn set_bundle_enabled(&self, name: &str, enabled: bool) -> Result<Value, ApiError> {
        let (host, loading) = self.active_handles()?;
        let caller = CallerIdentity::Host {
            host_generation: host.generation(),
        };
        Ok(self
            .execute_set_bundle_enabled(&host, &loading, &caller, name, enabled)
            .await)
    }

    /// Cancel an in-flight installation and wait until its subprocess and staging directory are cleaned up.
    pub async fn cancel_install(&self, request_id: &str) -> Value {
        self.cancel_install_for(InstallKey::new(None, request_id))
            .await
    }

    async fn cancel_install_for(&self, key: InstallKey) -> Value {
        let result_rx = {
            let guard = self.installs.lock().expect("install registry lock");
            let Some(active) = guard.active.get(&key) else {
                if let Some((_, result)) = guard.settled.iter().find(|(id, _)| id == &key) {
                    return install_cancellation_status(result);
                }
                return json!({ "status": "not-running" });
            };
            if active.phase == InstallPhase::Applying {
                return json!({ "status": "too-late" });
            }
            let _ = active.cancel_tx.send(true);
            active.result_rx.clone()
        };
        install_cancellation_status(&await_install_result(result_rx).await)
    }

    async fn wait_for_install(&self, key: InstallKey) -> Value {
        let rx = {
            let guard = self.installs.lock().expect("install registry lock");
            if let Some(active) = guard.active.get(&key) {
                active.result_rx.clone()
            } else if let Some((_, settled)) = guard.settled.iter().find(|(id, _)| id == &key) {
                return json!({ "result": settled });
            } else {
                return json!({ "result": Value::Null });
            }
        };
        json!({ "result": await_install_result(rx).await })
    }

    pub async fn list_bundles(&self) -> Result<Value, ApiError> {
        let (host, _) = self.active_handles()?;
        self.inspect_bundles(&host)
            .await
            .map_err(|e| ApiError::Conflict(e.message))
    }

    fn validate_caller(
        &self,
        expected_generation: u64,
        caller: &CallerIdentity,
    ) -> Result<(), RpcError> {
        let guard = self
            .owner
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器锁已损坏"))?;
        caller.validate(expected_generation, &guard)
    }

    fn allocate_staging_dir(&self) -> Result<PathBuf, String> {
        let seq = self.project_sequence.fetch_add(1, Ordering::Relaxed);
        let mut rand_bytes = [0u8; 8];
        getrandom::fill(&mut rand_bytes).map_err(|e| format!("生成候选目录随机数失败：{e}"))?;
        let suffix: String = rand_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let dir = self
            .config
            .projects_dir
            .join(format!("project-{seq}-{suffix}"));
        fs::create_dir_all(&dir).map_err(|e| format!("创建候选安装目录失败：{e}"))?;
        Ok(dir)
    }

    async fn sync_sources_before_mutation(
        &self,
        loading: &LoadingRuntime,
    ) -> Result<(u64, LoadingPreparation, Vec<Value>), String> {
        let (initial_rev, initial_prep) = {
            let guard = self
                .owner
                .lock()
                .map_err(|_| "插件管理器锁已损坏".to_owned())?;
            let current = guard
                .loading_configuration()
                .ok_or_else(|| "加载协调器尚未接纳初始配置".to_owned())?;
            (current.revision, current.preparation.clone())
        };
        loading
            .reconcile_sources(initial_rev, &initial_prep)
            .await
            .map_err(|e| format!("同步当前配置文件失败：{e}"))?;
        let guard = self
            .owner
            .lock()
            .map_err(|_| "插件管理器锁已损坏".to_owned())?;
        let current = guard
            .loading_configuration()
            .ok_or_else(|| "加载协调器尚未接纳初始配置".to_owned())?;
        let sources = current
            .sources
            .iter()
            .map(|s| json!({ "name": s.name(), "content": s.content(), "sha256": s.sha256() }))
            .collect();
        Ok((current.revision, current.preparation.clone(), sources))
    }

    async fn inspect_bundles(&self, host: &SharedHost) -> Result<Value, RpcError> {
        let (sources, packages) = {
            let guard = self
                .owner
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器锁已损坏"))?;
            let Some(current) = guard.loading_configuration() else {
                return Ok(json!({ "bundles": [] }));
            };
            let sources: Vec<Value> = current
                .sources
                .iter()
                .map(|s| json!({ "name": s.name(), "content": s.content(), "sha256": s.sha256() }))
                .collect();
            (sources, current.preparation.packages.clone())
        };
        host.prepare_bundle_mutation(json!({
            "action": "inspectBundles",
            "sources": sources,
            "packages": packages,
        }))
        .await
    }

    fn inspect_plugins(&self) -> Result<Value, RpcError> {
        let guard = self
            .owner
            .lock()
            .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器锁已损坏"))?;
        let Some(current) = guard.loading_configuration() else {
            return Ok(json!({ "plugins": [] }));
        };
        let mut plugins = Vec::new();
        if let Some(root_entries) = current.graph["root"]["entries"].as_array() {
            for root_node in root_entries {
                if root_node["options"]["name"] == "cordis:include"
                    && root_node["options"]["id"] == "include"
                    && let Some(children) = root_node["children"].as_array()
                {
                    for child in children {
                        let Some(id) = child["options"]["id"].as_str() else {
                            continue;
                        };
                        let Some(module_name) = child["options"]["name"].as_str() else {
                            continue;
                        };
                        if module_name.starts_with("cordis:") {
                            continue;
                        }
                        let disabled = child["options"]["disabled"].as_bool().unwrap_or(false);
                        plugins.push(json!({
                            "entryId": format!("include:{id}"),
                            "moduleName": module_name,
                            "disabled": disabled,
                            "patchId": id,
                        }));
                    }
                }
            }
        }
        Ok(json!({ "plugins": plugins }))
    }

    fn read_bundle_detail(&self, name: &str, language: &str) -> Result<Value, RpcError> {
        let pkg_dir = {
            let guard = self
                .owner
                .lock()
                .map_err(|_| RpcError::new(ErrorKind::Internal, "插件管理器锁已损坏"))?;
            let Some(current) = guard.loading_configuration() else {
                return Ok(json!({ "error": "unknown-bundle" }));
            };
            let Some(pkg) = current.preparation.packages.iter().find(|p| p.name == name) else {
                return Ok(json!({ "error": "unknown-bundle" }));
            };
            pkg.directory.clone()
        };
        let mut candidates = vec![format!("README.{language}.md")];
        if let Some((prefix, _)) = language.split_once('-') {
            candidates.push(format!("README.{prefix}.md"));
        }
        candidates.push("README.md".into());
        for rel in candidates {
            let path = pkg_dir.join(&rel);
            if let Ok(raw) = fs::read_to_string(&path) {
                let markdown = strip_frontmatter(raw.trim_start_matches('\u{feff}'));
                return Ok(json!({
                    "detail": {
                        "status": "found",
                        "origin": "readme",
                        "path": rel,
                        "markdown": markdown,
                    }
                }));
            }
        }
        Ok(json!({ "detail": { "status": "none" } }))
    }

    async fn inspect_spec(&self, spec: &str, registry_override: Option<&str>) -> Value {
        let parsed = match parse_install_spec(spec) {
            Ok(parsed) => parsed,
            Err(reason) => {
                return json!({
                    "status": "refused",
                    "problem": "invalid-spec",
                    "reason": reason,
                });
            }
        };
        let installed_names: Vec<String> = {
            let Ok(guard) = self.owner.lock() else {
                return json!({ "status": "refused", "problem": "unknown", "reason": "插件管理器锁已损坏" });
            };
            guard
                .loading_configuration()
                .map(|c| {
                    c.preparation
                        .packages
                        .iter()
                        .map(|p| p.name.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        let effective_registry = registry_override.unwrap_or(&self.config.registry);
        match parsed {
            ParsedInstallSpec::TarballPath(path) => {
                let Ok(bytes) = fs::read(&path) else {
                    return json!({
                        "status": "refused",
                        "problem": "not-a-package",
                        "reason": "归档文件不存在或不可读",
                    });
                };
                match NpmArchive::from_tarball_bytes(bytes) {
                    Ok(archive) => {
                        if installed_names.iter().any(|n| n == archive.name()) {
                            return json!({
                                "status": "refused",
                                "problem": "already-installed",
                                "reason": format!("{} 已安装", archive.name()),
                            });
                        }
                        inspect_archive_bundle(&archive, effective_registry)
                    }
                    Err(err) => json!({
                        "status": "refused",
                        "problem": "not-a-package",
                        "reason": err.message(),
                    }),
                }
            }
            ParsedInstallSpec::Registry {
                name,
                exact_version,
            } => {
                if installed_names.iter().any(|n| n == &name) {
                    return json!({
                        "status": "refused",
                        "problem": "already-installed",
                        "reason": format!("{name} 已安装"),
                    });
                }
                match fetch_npm_release_archive(
                    Some(effective_registry),
                    &name,
                    exact_version.as_deref(),
                )
                .await
                {
                    Ok(archive) => inspect_archive_bundle(&archive, effective_registry),
                    Err(err) => json!({
                        "status": "refused",
                        "problem": classify_inspect_problem(&err),
                        "reason": err.message(),
                        "registries": [effective_registry],
                    }),
                }
            }
        }
    }

    async fn list_versions(&self, name: &str, registry_override: Option<&str>) -> Value {
        let installed_version = {
            let Ok(guard) = self.owner.lock() else {
                return json!({
                    "status": "refused",
                    "problem": "unknown",
                    "reason": "插件管理器锁已损坏",
                    "attempts": [],
                });
            };
            guard.loading_configuration().and_then(|c| {
                c.preparation
                    .packages
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| p.version.clone())
            })
        };
        let Some(installed_str) = installed_version else {
            return json!({
                "status": "refused",
                "problem": "not-updatable",
                "reason": format!("{name} 未在当前安装项目中"),
                "attempts": [],
            });
        };
        let Ok(installed_ver) = semver::Version::parse(&installed_str) else {
            return json!({
                "status": "refused",
                "problem": "not-updatable",
                "reason": format!("{name} 当前版本不是有效 semver"),
                "attempts": [],
            });
        };
        let effective_registry = registry_override.unwrap_or(&self.config.registry);
        let (answered_registry, packument) =
            match fetch_npm_packument(Some(effective_registry), name).await {
                Ok(res) => res,
                Err(err) => {
                    return json!({
                        "status": "refused",
                        "problem": "network",
                        "reason": err.message(),
                        "attempts": [{
                            "registry": effective_registry,
                            "problem": "network",
                            "reason": err.message(),
                        }],
                    });
                }
            };
        let dist_tags = packument["dist-tags"].as_object();
        let times = packument["time"].as_object();
        let mut newer: Vec<(semver::Version, Value)> = Vec::new();
        if let Some(versions_obj) = packument["versions"].as_object() {
            for (ver_str, manifest) in versions_obj {
                let Ok(ver) = semver::Version::parse(ver_str) else {
                    continue;
                };
                if ver <= installed_ver {
                    continue;
                }
                let tags: Vec<String> = dist_tags
                    .into_iter()
                    .flatten()
                    .filter(|(_, target)| target.as_str() == Some(ver_str))
                    .map(|(tag, _)| tag.clone())
                    .collect();
                let published_at = times.and_then(|t| t.get(ver_str)).and_then(Value::as_str);
                let is_bundle = manifest["dsh"]["bundle"]["patch"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty());
                let deprecated = manifest["deprecated"].as_str();
                let mut item = json!({
                    "version": ver_str,
                    "tags": tags,
                    "prerelease": !ver.pre.is_empty(),
                    "bundle": is_bundle,
                });
                if let Some(at) = published_at {
                    item["publishedAt"] = Value::String(at.to_owned());
                }
                if let Some(dep) = deprecated {
                    item["deprecated"] = Value::String(dep.to_owned());
                }
                newer.push((ver, item));
            }
        }
        newer.sort_by(|a, b| b.0.cmp(&a.0));
        let total_newer = newer.len();
        let listed: Vec<Value> = newer
            .into_iter()
            .take(VERSION_LIST_LIMIT)
            .map(|(_, item)| item)
            .collect();
        let omitted = total_newer.saturating_sub(listed.len());
        json!({
            "status": "listed",
            "installed": installed_str,
            "registry": answered_registry,
            "attempts": [{ "registry": answered_registry }],
            "versions": listed,
            "omitted": omitted,
        })
    }

    async fn execute_set_plugin_enabled(
        &self,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        id: &str,
        enabled: bool,
    ) -> Value {
        let _guard = self.mutation_lock.lock().await;
        if let Err(err) = self.validate_caller(loading.host_generation(), caller) {
            return failed_change("enable", id, Some(enabled), "operation-error", &err.message);
        }
        let (revision, _, _) = match self.sync_sources_before_mutation(loading).await {
            Ok(state) => state,
            Err(diag) => {
                return failed_change("enable", id, Some(enabled), "operation-error", &diag);
            }
        };
        let local_id = id.strip_prefix("include:").unwrap_or(id);
        let operation = json!({
            "kind": "update",
            "treeId": "include",
            "id": local_id,
            "options": { "disabled": !enabled },
        });
        match loading.edit_sources(revision, operation).await {
            Ok(_) => json!({
                "changed": true,
                "application": "applied",
                "stage": "enable",
                "target": id,
                "enabled": enabled,
            }),
            Err(err) => failed_change(
                "enable",
                id,
                Some(enabled),
                "operation-error",
                &err.to_string(),
            ),
        }
    }

    async fn execute_set_bundle_enabled(
        &self,
        host: &SharedHost,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        name: &str,
        enabled: bool,
    ) -> Value {
        let _guard = self.mutation_lock.lock().await;
        if let Err(err) = self.validate_caller(host.generation(), caller) {
            return failed_change(
                "enable",
                name,
                Some(enabled),
                "operation-error",
                &err.message,
            );
        }
        if !enabled && caller.plugin_id() == Some(name) {
            return failed_change(
                "enable",
                name,
                Some(false),
                "management-required",
                "运行中的插件不能通过 setBundleEnabled 停用自身",
            );
        }
        let (revision, current_prep, sources) =
            match self.sync_sources_before_mutation(loading).await {
                Ok(state) => state,
                Err(diag) => {
                    return failed_change("enable", name, Some(enabled), "operation-error", &diag);
                }
            };
        let Some(existing_pkg) = current_prep.packages.iter().find(|p| p.name == name) else {
            return failed_change(
                "enable",
                name,
                Some(enabled),
                "unknown-plugin",
                &format!("未安装插件包 {name}"),
            );
        };
        let compiled = match host
            .prepare_bundle_mutation(json!({
                "action": "setBundleEnabled",
                "sources": sources,
                "packages": current_prep.packages,
                "previousPackageDir": existing_pkg.directory,
                "targetName": name,
                "enabled": enabled,
            }))
            .await
        {
            Ok(v) => v,
            Err(err) => {
                return failed_change(
                    "enable",
                    name,
                    Some(enabled),
                    "operation-error",
                    &err.message,
                );
            }
        };
        if compiled["ok"] != true {
            return json!({
                "changed": false,
                "application": "failed",
                "stage": "enable",
                "target": name,
                "enabled": enabled,
                "error": compiled["error"],
            });
        }
        let edits: Vec<LoadingSourceEdit> = match serde_json::from_value(compiled["edits"].clone())
        {
            Ok(edits) => edits,
            Err(err) => {
                return failed_change(
                    "enable",
                    name,
                    Some(enabled),
                    "operation-error",
                    &err.to_string(),
                );
            }
        };
        if edits.is_empty() {
            return json!({
                "changed": false,
                "application": "applied",
                "stage": "enable",
                "target": name,
                "enabled": enabled,
            });
        }
        if let Err(err) = self.validate_caller(host.generation(), caller) {
            return failed_change(
                "enable",
                name,
                Some(enabled),
                "operation-error",
                &err.message,
            );
        }
        if let Err(err) = loading
            .submit_installation(revision, &current_prep, edits)
            .await
        {
            return failed_change(
                "enable",
                name,
                Some(enabled),
                "operation-error",
                &err.to_string(),
            );
        }
        if let Err(err) = loading.apply_current().await {
            return failed_change(
                "enable",
                name,
                Some(enabled),
                "operation-error",
                &err.to_string(),
            );
        }
        json!({
            "changed": true,
            "application": "applied",
            "stage": "enable",
            "target": name,
            "enabled": enabled,
        })
    }

    async fn execute_install(
        &self,
        host: &SharedHost,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        spec: &str,
        options: InstallExecOptions,
    ) -> Value {
        let key = options
            .request_id
            .as_deref()
            .map(|id| InstallKey::new(Some(caller), id));
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let (result_tx, result_rx) = watch::channel(None::<Value>);
        if let Some(key) = &key {
            let mut reg = self.installs.lock().expect("install registry lock");
            if reg.active.contains_key(key) || reg.settled.iter().any(|(id, _)| id == key) {
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    &format!("重复的 requestId：{}", key.request_id),
                );
            }
            reg.active.insert(
                key.clone(),
                ActiveInstallEntry {
                    cancel_tx,
                    phase: InstallPhase::Installing,
                    result_rx,
                },
            );
        }

        let outcome = self
            .execute_install_inner(host, loading, caller, spec, &options, &mut cancel_rx)
            .await;

        if let Some(key) = key {
            let mut reg = self.installs.lock().expect("install registry lock");
            let _ = result_tx.send(Some(outcome.clone()));
            reg.active.remove(&key);
            reg.record_settled(key, outcome.clone());
        }
        outcome
    }

    async fn execute_install_inner(
        &self,
        host: &SharedHost,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        spec: &str,
        options: &InstallExecOptions,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> Value {
        let _mutation = tokio::select! {
            guard = self.mutation_lock.lock() => guard,
            _ = wait_cancelled(cancel_rx) => {
                return cancelled_change(spec, options.enabled);
            }
        };
        if *cancel_rx.borrow() {
            return cancelled_change(spec, options.enabled);
        }
        if let Err(err) = self.validate_caller(host.generation(), caller) {
            return failed_change(
                "install",
                spec,
                Some(options.enabled),
                "operation-error",
                &err.message,
            );
        }

        let parsed = match parse_install_spec(spec) {
            Ok(parsed) => parsed,
            Err(diag) => {
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "invalid-spec",
                    &diag,
                );
            }
        };
        let effective_registry = options
            .registry_override
            .as_deref()
            .unwrap_or(&self.config.registry);

        let archive = tokio::select! {
            fetched = acquire_archive(&parsed, effective_registry) => match fetched {
                Ok(archive) => archive,
                Err((code, diag)) => {
                    return failed_change("install", spec, Some(options.enabled), code, &diag);
                }
            },
            _ = wait_cancelled(cancel_rx) => {
                return cancelled_change(spec, options.enabled);
            }
        };

        if let Some((exp_name, exp_version)) = &options.expected
            && (archive.name() != exp_name || archive.version() != exp_version)
        {
            return failed_change(
                "install",
                spec,
                Some(options.enabled),
                "unexpected-package",
                &format!(
                    "实际解析包 {}@{} 与期望 {}@{} 不符",
                    archive.name(),
                    archive.version(),
                    exp_name,
                    exp_version
                ),
            );
        }
        if options
            .confirmed_digest
            .as_deref()
            .is_some_and(|expected| expected != archive.sha256())
        {
            return failed_change(
                "install",
                spec,
                Some(options.enabled),
                "unexpected-package",
                "原包归档与确认的预览摘要不一致，请重新预览",
            );
        }

        let (_revision, current_prep, sources) =
            match self.sync_sources_before_mutation(loading).await {
                Ok(state) => state,
                Err(diag) => {
                    return failed_change(
                        "install",
                        spec,
                        Some(options.enabled),
                        "operation-error",
                        &diag,
                    );
                }
            };

        let mut retained = match read_retained_archives(&current_prep) {
            Ok(retained) => retained,
            Err(err) => {
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    err.message(),
                );
            }
        };
        let previous_pkg_dir = current_prep
            .packages
            .iter()
            .find(|p| p.name == archive.name())
            .map(|p| p.directory.clone());
        let is_upgrade = previous_pkg_dir.is_some();
        let pkg_name = archive.name().to_owned();
        let pkg_version = archive.version().to_owned();

        // Preflight bundle declaration and DSH runtime compatibility before spawning npm.
        if let Some(preflight_failure) = self
            .preflight_archive_bundle(
                host,
                &archive,
                is_upgrade,
                previous_pkg_dir.as_deref(),
                &sources,
                options.enabled,
            )
            .await
        {
            return json!({
                "changed": false,
                "application": "failed",
                "stage": "install",
                "target": spec,
                "enabled": options.enabled,
                "error": preflight_failure,
            });
        }

        retained.insert(pkg_name.clone(), archive);
        let archives_vec: Vec<NpmArchive> = retained.into_values().collect();

        let staging_dir = match self.allocate_staging_dir() {
            Ok(dir) => dir,
            Err(diag) => {
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    &diag,
                );
            }
        };
        let toolchain = PluginDependencyToolchain {
            node: &self.node,
            npm_cli: &self.config.npm_cli,
            sdk_project: &self.sdk_project,
            registry: effective_registry,
        };
        let prepared_packages = tokio::select! {
            res = prepare_plugin_dependencies(&self.group, &toolchain, &staging_dir, &archives_vec) => match res {
                Ok(pkgs) => pkgs,
                Err(err) => {
                    let _ = fs::remove_dir_all(&staging_dir);
                    return failed_change("install", spec, Some(options.enabled), "operation-error", err.message());
                }
            },
            _ = wait_cancelled(cancel_rx) => {
                let _ = fs::remove_dir_all(&staging_dir);
                return cancelled_change(spec, options.enabled);
            }
        };

        if let Some(req_id) = options.request_id.as_deref() {
            let mut reg = self.installs.lock().expect("install registry lock");
            if let Some(entry) = reg.active.get_mut(&InstallKey::new(Some(caller), req_id)) {
                if *cancel_rx.borrow() {
                    let _ = fs::remove_dir_all(&staging_dir);
                    return cancelled_change(spec, options.enabled);
                }
                entry.phase = InstallPhase::Applying;
            }
        }

        let inspections: Vec<PackageInspection> = prepared_packages
            .into_iter()
            .map(|(insp, _)| insp)
            .collect();
        let canonical_staging = match fs::canonicalize(&staging_dir) {
            Ok(p) => p,
            Err(err) => {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    &err.to_string(),
                );
            }
        };
        let mut committed = false;
        let mut last_submit_err = String::new();
        for _attempt in 0..3 {
            let (fresh_rev, fresh_prep, fresh_sources) =
                match self.sync_sources_before_mutation(loading).await {
                    Ok(state) => state,
                    Err(diag) => {
                        let _ = fs::remove_dir_all(&staging_dir);
                        return failed_change(
                            "install",
                            spec,
                            Some(options.enabled),
                            "operation-error",
                            &diag,
                        );
                    }
                };
            if let Err(err) =
                preserve_unchanged_package_links(&canonical_staging, &inspections, &fresh_prep)
            {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    err.message(),
                );
            }
            let fresh_prev_pkg_dir = fresh_prep
                .packages
                .iter()
                .find(|p| p.name == pkg_name)
                .map(|p| p.directory.clone());
            let fresh_is_upgrade = fresh_prev_pkg_dir.is_some();
            let compiled = match host
                .prepare_bundle_mutation(json!({
                    "action": if fresh_is_upgrade { "upgrade" } else { "install" },
                    "sources": fresh_sources,
                    "packages": inspections,
                    "previousPackageDir": fresh_prev_pkg_dir,
                    "targetName": pkg_name,
                    "enabled": options.enabled,
                    "preserveEnabled": options.confirmed_digest.is_some(),
                }))
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    let _ = fs::remove_dir_all(&staging_dir);
                    return failed_change(
                        "install",
                        spec,
                        Some(options.enabled),
                        "operation-error",
                        &err.message,
                    );
                }
            };
            if compiled["ok"] != true {
                let _ = fs::remove_dir_all(&staging_dir);
                return json!({
                    "changed": false,
                    "application": "failed",
                    "stage": "install",
                    "target": spec,
                    "enabled": options.enabled,
                    "error": compiled["error"],
                });
            }
            let edits: Vec<LoadingSourceEdit> =
                match serde_json::from_value(compiled["edits"].clone()) {
                    Ok(edits) => edits,
                    Err(err) => {
                        let _ = fs::remove_dir_all(&staging_dir);
                        return failed_change(
                            "install",
                            spec,
                            Some(options.enabled),
                            "operation-error",
                            &err.to_string(),
                        );
                    }
                };
            let candidate_prep = LoadingPreparation {
                project_directory: canonical_staging.clone(),
                base: fresh_prep.base.clone(),
                overlays: fresh_prep.overlays.clone(),
                packages: inspections.clone(),
            };
            if let Err(err) = self.validate_caller(host.generation(), caller) {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change(
                    "install",
                    spec,
                    Some(options.enabled),
                    "operation-error",
                    &err.message,
                );
            }
            match loading
                .submit_installation(fresh_rev, &candidate_prep, edits)
                .await
            {
                Ok(_) => {
                    committed = true;
                    break;
                }
                Err(err) => {
                    last_submit_err = err.to_string();
                }
            }
        }
        if !committed {
            return failed_change(
                "install",
                spec,
                Some(options.enabled),
                "operation-error",
                &last_submit_err,
            );
        }

        if caller.plugin_id() == Some(pkg_name.as_str()) {
            let loading_clone = loading.clone();
            tokio::spawn(async move {
                let _ = loading_clone.apply_current().await;
            });
        } else if let Err(err) = loading.apply_current().await {
            return failed_change(
                "install",
                spec,
                Some(options.enabled),
                "operation-error",
                &err.to_string(),
            );
        }

        json!({
            "changed": true,
            "application": "applied",
            "stage": "install",
            "target": spec,
            "enabled": options.enabled,
            "bundle": pkg_name,
            "version": pkg_version,
            "registries": [effective_registry],
            "packageResult": {
                "exitCode": 0,
                "output": format!("Installed {pkg_name}@{pkg_version}"),
                "truncated": false,
                "logPath": "",
            },
        })
    }

    async fn execute_remove(
        &self,
        host: &SharedHost,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        name: &str,
    ) -> Value {
        let _guard = self.mutation_lock.lock().await;
        if let Err(err) = self.validate_caller(host.generation(), caller) {
            return failed_change("remove", name, None, "operation-error", &err.message);
        }
        if caller.plugin_id() == Some(name) {
            return failed_change(
                "remove",
                name,
                None,
                "stop-profile",
                "运行中的插件不能通过 removeBundle 移除自身；请从 Magi 设置页执行原子更新或卸载",
            );
        }
        let (_revision, current_prep, _) = match self.sync_sources_before_mutation(loading).await {
            Ok(state) => state,
            Err(diag) => {
                return failed_change("remove", name, None, "operation-error", &diag);
            }
        };
        let Some(_old_pkg) = current_prep.packages.iter().find(|p| p.name == name) else {
            return failed_change(
                "remove",
                name,
                None,
                "unknown-plugin",
                &format!("未找到已安装的插件包 {name}"),
            );
        };
        let mut retained = match read_retained_archives(&current_prep) {
            Ok(retained) => retained,
            Err(err) => {
                return failed_change("remove", name, None, "operation-error", err.message());
            }
        };
        retained.remove(name);
        let archives_vec: Vec<NpmArchive> = retained.into_values().collect();
        let staging_dir = match self.allocate_staging_dir() {
            Ok(dir) => dir,
            Err(diag) => return failed_change("remove", name, None, "operation-error", &diag),
        };
        let toolchain = PluginDependencyToolchain {
            node: &self.node,
            npm_cli: &self.config.npm_cli,
            sdk_project: &self.sdk_project,
            registry: &self.config.registry,
        };
        let prepared =
            match prepare_plugin_dependencies(&self.group, &toolchain, &staging_dir, &archives_vec)
                .await
            {
                Ok(pkgs) => pkgs,
                Err(err) => {
                    let _ = fs::remove_dir_all(&staging_dir);
                    return failed_change("remove", name, None, "operation-error", err.message());
                }
            };
        let inspections: Vec<PackageInspection> =
            prepared.into_iter().map(|(insp, _)| insp).collect();
        let canonical_staging = match fs::canonicalize(&staging_dir) {
            Ok(p) => p,
            Err(err) => {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change("remove", name, None, "operation-error", &err.to_string());
            }
        };
        let mut committed = false;
        let mut last_submit_err = String::new();
        for _attempt in 0..3 {
            let (fresh_rev, fresh_prep, fresh_sources) =
                match self.sync_sources_before_mutation(loading).await {
                    Ok(state) => state,
                    Err(diag) => {
                        let _ = fs::remove_dir_all(&staging_dir);
                        return failed_change("remove", name, None, "operation-error", &diag);
                    }
                };
            let Some(fresh_old_pkg) = fresh_prep.packages.iter().find(|p| p.name == name) else {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change(
                    "remove",
                    name,
                    None,
                    "unknown-plugin",
                    &format!("未找到已安装的插件包 {name}"),
                );
            };
            if let Err(err) =
                preserve_unchanged_package_links(&canonical_staging, &inspections, &fresh_prep)
            {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change("remove", name, None, "operation-error", err.message());
            }
            let compiled = match host
                .prepare_bundle_mutation(json!({
                    "action": "remove",
                    "sources": fresh_sources,
                    "packages": inspections,
                    "previousPackageDir": fresh_old_pkg.directory,
                }))
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    let _ = fs::remove_dir_all(&staging_dir);
                    return failed_change("remove", name, None, "operation-error", &err.message);
                }
            };
            let edits: Vec<LoadingSourceEdit> =
                match serde_json::from_value(compiled["edits"].clone()) {
                    Ok(edits) => edits,
                    Err(err) => {
                        let _ = fs::remove_dir_all(&staging_dir);
                        return failed_change(
                            "remove",
                            name,
                            None,
                            "operation-error",
                            &err.to_string(),
                        );
                    }
                };
            let candidate_prep = LoadingPreparation {
                project_directory: canonical_staging.clone(),
                base: fresh_prep.base.clone(),
                overlays: fresh_prep.overlays.clone(),
                packages: inspections.clone(),
            };
            if let Err(err) = self.validate_caller(host.generation(), caller) {
                let _ = fs::remove_dir_all(&staging_dir);
                return failed_change("remove", name, None, "operation-error", &err.message);
            }
            match loading
                .submit_installation(fresh_rev, &candidate_prep, edits)
                .await
            {
                Ok(_) => {
                    committed = true;
                    break;
                }
                Err(err) => {
                    last_submit_err = err.to_string();
                }
            }
        }
        if !committed {
            return failed_change("remove", name, None, "operation-error", &last_submit_err);
        }
        if let Err(err) = loading.apply_current().await {
            return failed_change("remove", name, None, "operation-error", &err.to_string());
        }
        json!({
            "changed": true,
            "application": "applied",
            "stage": "remove",
            "target": name,
            "packageResult": {
                "exitCode": 0,
                "output": format!("Removed {name}"),
                "truncated": false,
                "logPath": "",
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_update(
        &self,
        host: &SharedHost,
        loading: &LoadingRuntime,
        caller: &CallerIdentity,
        name: &str,
        version: Option<&str>,
        request_id: Option<&str>,
        registry_override: Option<&str>,
    ) -> Value {
        let (from_version, was_enabled) = {
            let Ok(guard) = self.owner.lock() else {
                return json!({
                    "from": "",
                    "to": version.unwrap_or("latest"),
                    "step": "check",
                    "result": failed_change("install", name, None, "operation-error", "插件管理器锁已损坏"),
                });
            };
            let Some(current) = guard.loading_configuration() else {
                return json!({
                    "from": "",
                    "to": version.unwrap_or("latest"),
                    "step": "check",
                    "result": failed_change("install", name, None, "unknown-plugin", "尚无已接纳的加载配置"),
                });
            };
            let Some(pkg) = current.preparation.packages.iter().find(|p| p.name == name) else {
                return json!({
                    "from": "",
                    "to": version.unwrap_or("latest"),
                    "step": "check",
                    "result": failed_change("install", name, None, "unknown-plugin", &format!("未找到已安装的插件包 {name}")),
                });
            };
            let enabled = current
                .sources
                .iter()
                .find(|s| s.name() == "bundle.patch.yml")
                .is_none_or(|s| s.content().contains(&pkg.name));
            (pkg.version.clone(), enabled)
        };
        let target_spec = match version {
            Some(ver) if !ver.is_empty() => format!("{name}@{ver}"),
            _ => format!("{name}@latest"),
        };
        let result = self
            .execute_install(
                host,
                loading,
                caller,
                &target_spec,
                InstallExecOptions {
                    enabled: was_enabled,
                    request_id: request_id.map(str::to_owned),
                    registry_override: registry_override.map(str::to_owned),
                    expected: version
                        .filter(|v| !v.is_empty() && *v != "latest")
                        .map(|v| (name.to_owned(), v.to_owned())),
                    confirmed_digest: None,
                },
            )
            .await;
        let to_version = result["version"]
            .as_str()
            .or(version)
            .unwrap_or("latest")
            .to_owned();
        json!({
            "from": from_version,
            "to": to_version,
            "step": "install",
            "result": result,
        })
    }

    async fn preflight_archive_bundle(
        &self,
        host: &SharedHost,
        archive: &NpmArchive,
        is_upgrade: bool,
        previous_pkg_dir: Option<&Path>,
        sources: &[Value],
        enabled: bool,
    ) -> Option<Value> {
        let Ok(temp_dir) = self.allocate_staging_dir() else {
            return None;
        };
        let staged_res = archive.stage(&temp_dir);
        let outcome = match staged_res {
            Ok(_) => {
                let check = host
                    .prepare_bundle_mutation(json!({
                        "action": if is_upgrade { "upgrade" } else { "install" },
                        "sources": sources,
                        "packages": [{
                            "name": archive.name(),
                            "version": archive.version(),
                            "directory": temp_dir,
                        }],
                        "previousPackageDir": previous_pkg_dir,
                        "targetName": archive.name(),
                        "enabled": enabled,
                    }))
                    .await;
                match check {
                    Ok(compiled) if compiled["ok"] != true => Some(compiled["error"].clone()),
                    Ok(_) => None,
                    Err(err) => Some(json!({
                        "code": "operation-error",
                        "diagnostic": err.message,
                    })),
                }
            }
            Err(err) => Some(json!({
                "code": "invalid-spec",
                "diagnostic": err.message(),
            })),
        };
        let _ = fs::remove_dir_all(&temp_dir);
        outcome
    }
}

struct InstallExecOptions {
    enabled: bool,
    request_id: Option<String>,
    registry_override: Option<String>,
    expected: Option<(String, String)>,
    confirmed_digest: Option<String>,
}

async fn await_install_result(mut rx: watch::Receiver<Option<Value>>) -> Value {
    loop {
        if let Some(value) = rx.borrow_and_update().clone() {
            return value;
        }
        if rx.changed().await.is_err() {
            return Value::Null;
        }
    }
}

fn install_cancellation_status(outcome: &Value) -> Value {
    let status = if outcome["application"] == "cancelled" {
        "cancelled"
    } else {
        "too-late"
    };
    json!({ "status": status })
}

async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn acquire_archive(
    parsed: &ParsedInstallSpec,
    registry: &str,
) -> Result<NpmArchive, (&'static str, String)> {
    match parsed {
        ParsedInstallSpec::TarballPath(path) => {
            let bytes =
                fs::read(path).map_err(|e| ("invalid-spec", format!("读取本地归档失败：{e}")))?;
            NpmArchive::from_tarball_bytes(bytes)
                .map_err(|e| ("invalid-spec", e.message().to_owned()))
        }
        ParsedInstallSpec::Registry {
            name,
            exact_version,
        } => fetch_npm_release_archive(Some(registry), name, exact_version.as_deref())
            .await
            .map_err(|e| ("operation-error", e.message().to_owned())),
    }
}

fn inspect_archive_bundle(archive: &NpmArchive, registry: &str) -> Value {
    let Ok(staging) = fs::canonicalize(std::env::temp_dir()).and_then(|base| {
        let mut rand_bytes = [0u8; 8];
        let _ = getrandom::fill(&mut rand_bytes);
        let suffix: String = rand_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let dir = base.join(format!("magi-inspect-{suffix}"));
        fs::create_dir(&dir)?;
        Ok(dir)
    }) else {
        return json!({
            "status": "refused",
            "problem": "unknown",
            "reason": "创建临时检查目录失败",
        });
    };
    let outcome = (|| -> Result<Value, String> {
        let _ = archive
            .stage(&staging)
            .map_err(|e| e.message().to_owned())?;
        let manifest_bytes = fs::read(staging.join("package.json")).map_err(|e| e.to_string())?;
        let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
        let is_bundle = manifest["dsh"]["bundle"]["patch"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty());
        if !is_bundle {
            return Ok(json!({
                "status": "refused",
                "problem": "not-a-bundle",
                "reason": format!("{} 未声明 dsh.bundle", archive.name()),
                "registries": [registry],
            }));
        }
        let mut ok = json!({
            "status": "accepted",
            "kind": "registry",
            "name": archive.name(),
            "version": archive.version(),
            "bundle": true,
            "registry": registry,
        });
        if let Some(desc) = manifest["description"].as_str().filter(|s| !s.is_empty()) {
            ok["description"] = Value::String(desc.to_owned());
        }
        Ok(ok)
    })();
    let _ = fs::remove_dir_all(&staging);
    match outcome {
        Ok(value) => value,
        Err(reason) => json!({
            "status": "refused",
            "problem": "not-a-package",
            "reason": reason,
        }),
    }
}

fn classify_inspect_problem(err: &ApiError) -> &'static str {
    let msg = err.message();
    if msg.contains("404") || msg.contains("不存在") {
        "not-found"
    } else if matches!(err, ApiError::InvalidInput(_)) {
        "invalid-spec"
    } else {
        "network"
    }
}

fn strip_frontmatter(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---\n")
    {
        return rest[end + 5..].trim_start().to_owned();
    }
    if let Some(rest) = text.strip_prefix("---\r\n")
        && let Some(end) = rest.find("\r\n---\r\n")
    {
        return rest[end + 7..].trim_start().to_owned();
    }
    text.to_owned()
}

fn cancelled_change(target: &str, enabled: bool) -> Value {
    json!({
        "changed": false,
        "application": "cancelled",
        "stage": "install",
        "target": target,
        "enabled": enabled,
    })
}

fn failed_change(
    stage: &str,
    target: &str,
    enabled: Option<bool>,
    code: &str,
    diagnostic: &str,
) -> Value {
    let mut res = json!({
        "changed": false,
        "application": "failed",
        "stage": stage,
        "target": target,
        "error": {
            "code": code,
            "diagnostic": diagnostic,
        },
        "packageResult": {
            "exitCode": 1,
            "output": diagnostic,
            "truncated": false,
            "logPath": "",
        },
    });
    if let Some(en) = enabled {
        res["enabled"] = Value::Bool(en);
    }
    res
}

/// Generation-bound Incoming adapter handling `pluginManager.*` requests from the shared host.
pub(crate) struct PluginManagementIncoming {
    service: Arc<PluginManagementService>,
    host_generation: u64,
    host: Mutex<Option<std::sync::Weak<SharedHost>>>,
    loading: Mutex<Option<std::sync::Weak<LoadingRuntime>>>,
    delegate: Arc<dyn Incoming>,
}

impl PluginManagementIncoming {
    pub(crate) fn new(
        service: Arc<PluginManagementService>,
        host_generation: u64,
        delegate: Arc<dyn Incoming>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            host_generation,
            host: Mutex::new(None),
            loading: Mutex::new(None),
            delegate,
        })
    }

    pub(crate) fn bind(&self, host: &Arc<SharedHost>, loading: &Arc<LoadingRuntime>) {
        *self.host.lock().expect("management host lock") = Some(Arc::downgrade(host));
        *self.loading.lock().expect("management loading lock") = Some(Arc::downgrade(loading));
    }

    fn bound(&self) -> Result<(Arc<SharedHost>, Arc<LoadingRuntime>), RpcError> {
        let host = self
            .host
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(std::sync::Weak::upgrade))
            .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "插件宿主已关闭"))?;
        let loading = self
            .loading
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(std::sync::Weak::upgrade))
            .ok_or_else(|| RpcError::new(ErrorKind::HostGone, "插件加载协调器已关闭"))?;
        Ok((host, loading))
    }
}

fn parse_caller(params: &Value) -> Result<CallerIdentity, RpcError> {
    serde_json::from_value(params["caller"].clone())
        .map_err(|e| RpcError::new(ErrorKind::InvalidParams, format!("无效的调用人身份：{e}")))
}

impl Incoming for PluginManagementIncoming {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        if !method.starts_with("pluginManager.") {
            return self.delegate.request(method, params);
        }
        let service = self.service.clone();
        let host_generation = self.host_generation;
        let bound = self.bound();
        Box::pin(async move {
            let (host, loading) = bound?;
            let caller = parse_caller(&params)?;
            service.validate_caller(host_generation, &caller)?;

            match method.as_str() {
                "pluginManager.registries" => Ok(json!({
                    "registry": service.registry(),
                    "fallbackRegistries": [],
                    "resolved": service.registry(),
                })),
                "pluginManager.inspect" => {
                    let spec = params["spec"].as_str().unwrap_or_default();
                    let reg = params["options"]["registry"].as_str();
                    Ok(service.inspect_spec(spec, reg).await)
                }
                "pluginManager.listVersions" => {
                    let name = params["name"].as_str().unwrap_or_default();
                    let reg = params["options"]["registry"].as_str();
                    Ok(service.list_versions(name, reg).await)
                }
                "pluginManager.listPlugins" => service.inspect_plugins(),
                "pluginManager.listBundles" => service.inspect_bundles(&host).await,
                "pluginManager.readBundleDetail" => {
                    let name = params["name"].as_str().unwrap_or_default();
                    let language = params["language"].as_str().unwrap_or("en");
                    service.read_bundle_detail(name, language)
                }
                "pluginManager.setPluginEnabled" => {
                    let id = params["id"].as_str().unwrap_or_default();
                    let enabled = params["enabled"].as_bool().unwrap_or(true);
                    Ok(service
                        .execute_set_plugin_enabled(&loading, &caller, id, enabled)
                        .await)
                }
                "pluginManager.setBundleEnabled" => {
                    let name = params["name"].as_str().unwrap_or_default();
                    let enabled = params["enabled"].as_bool().unwrap_or(true);
                    Ok(service
                        .execute_set_bundle_enabled(&host, &loading, &caller, name, enabled)
                        .await)
                }
                "pluginManager.installBundle" => {
                    let spec = params["spec"].as_str().unwrap_or_default();
                    let opts = &params["options"];
                    let enabled = opts["enabled"].as_bool().unwrap_or(true);
                    let request_id = opts["requestId"].as_str().map(str::to_owned);
                    let registry_override = opts["registry"].as_str().map(str::to_owned);
                    let expected = opts["expected"].as_object().and_then(|obj| {
                        Some((
                            obj.get("name")?.as_str()?.to_owned(),
                            obj.get("version")?.as_str()?.to_owned(),
                        ))
                    });
                    Ok(service
                        .execute_install(
                            &host,
                            &loading,
                            &caller,
                            spec,
                            InstallExecOptions {
                                enabled,
                                request_id,
                                registry_override,
                                expected,
                                confirmed_digest: None,
                            },
                        )
                        .await)
                }
                "pluginManager.waitForInstall" => {
                    let req_id = params["requestId"].as_str().unwrap_or_default();
                    Ok(service
                        .wait_for_install(InstallKey::new(Some(&caller), req_id))
                        .await)
                }
                "pluginManager.cancelInstall" => {
                    let req_id = params["requestId"].as_str().unwrap_or_default();
                    Ok(service
                        .cancel_install_for(InstallKey::new(Some(&caller), req_id))
                        .await)
                }
                "pluginManager.removeBundle" => {
                    let name = params["name"].as_str().unwrap_or_default();
                    Ok(service.execute_remove(&host, &loading, &caller, name).await)
                }
                "pluginManager.updateBundle" => {
                    let name = params["name"].as_str().unwrap_or_default();
                    let version = params["version"].as_str();
                    let opts = &params["options"];
                    let request_id = opts["requestId"].as_str();
                    let registry_override = opts["registry"].as_str();
                    Ok(service
                        .execute_update(
                            &host,
                            &loading,
                            &caller,
                            name,
                            version,
                            request_id,
                            registry_override,
                        )
                        .await)
                }
                _ => Err(RpcError::new(
                    ErrorKind::MethodNotFound,
                    format!("Unknown pluginManager method: {method}"),
                )),
            }
        })
    }

    fn notification(&self, method: String, params: Value) {
        self.delegate.notification(method, params);
    }
}

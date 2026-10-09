use crate::{PluginError, PluginPackage, PluginResourceStore};
use magi_app_server_protocol::{PluginManifest, PluginPermission, PluginScopeKind};
use magi_core::fs_atomic::write_atomic;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const STATE_FILE: &str = "state.json";
const STATE_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginSource {
    Center { url: String },
    Address { url: String },
    Local { name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub source: PluginSource,
    pub package_path: String,
    pub enabled_scopes: BTreeSet<String>,
    pub grants: BTreeMap<String, Vec<PluginPermission>>,
    pub active_scopes: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManagerState {
    pub schema_version: u16,
    pub plugins: BTreeMap<String, InstalledPlugin>,
}

impl Default for PluginManagerState {
    fn default() -> Self {
        Self {
            schema_version: STATE_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePlugin {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub scope: String,
    pub package_path: PathBuf,
}

/// daemon 插件服务的唯一写入口。来源解析和下载在外层完成，进入此处的始终是已校验包。
pub struct PluginManager {
    root: PathBuf,
    state: PluginManagerState,
    resources: PluginResourceStore,
}

impl PluginManager {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let state: PluginManagerState = match fs::read(root.join(STATE_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| PluginError::CorruptState(error.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(error) => return Err(error.into()),
        };
        if state.schema_version != STATE_VERSION {
            return Err(PluginError::CorruptState("插件状态版本不受支持".into()));
        }
        for plugin in state.plugins.values() {
            validate_plugin_path(&root, plugin)?;
        }
        let resources =
            PluginResourceStore::open(root.join("resources.json")).map_err(PluginError::Storage)?;
        Ok(Self {
            root,
            state,
            resources,
        })
    }

    pub fn state(&self) -> &PluginManagerState {
        &self.state
    }

    pub fn resources(&self) -> &PluginResourceStore {
        &self.resources
    }
    pub fn resources_mut(&mut self) -> &mut PluginResourceStore {
        &mut self.resources
    }

    pub fn package(&self, id: &str) -> Result<PluginPackage, PluginError> {
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        let bytes = fs::read(self.root.join(&installed.package_path))?;
        let package = PluginPackage::from_archive(&bytes)?;
        if package.manifest().id != installed.id
            || package.manifest().version != installed.version
            || package.digest() != installed.digest
        {
            return Err(PluginError::CorruptState("已安装包摘要或身份不匹配".into()));
        }
        Ok(package)
    }

    pub fn install(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let manifest = package.manifest();
        if let Some(existing) = self.state.plugins.get(&manifest.id) {
            if existing.digest == package.digest() {
                return Ok(());
            }
            return Err(conflict("插件已有不同版本；必须先完成排空升级"));
        }
        let relative = format!("versions/{}/{}.zip", manifest.id, package.digest());
        let target = self.root.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&target, package.archive_bytes())?;
        let mut next = self.state.clone();
        next.plugins.insert(
            manifest.id.clone(),
            InstalledPlugin {
                id: manifest.id.clone(),
                version: manifest.version.clone(),
                digest: package.digest().to_string(),
                source,
                package_path: relative,
                enabled_scopes: BTreeSet::new(),
                grants: BTreeMap::new(),
                active_scopes: BTreeSet::new(),
            },
        );
        if let Err(error) = self.commit(next) {
            let _ = fs::remove_file(target);
            return Err(error);
        }
        Ok(())
    }

    /// 只允许停用且无实例租约的插件切换到新包；不保留双版本并发或自动回退。
    pub fn upgrade(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let id = package.manifest().id.as_str();
        let current = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装，不能升级"))?;
        if current.digest == package.digest() {
            return Ok(());
        }
        if !current.active_scopes.is_empty() || !current.enabled_scopes.is_empty() {
            return Err(conflict("插件仍有启用作用域或激活实例，不能切换"));
        }
        let relative = format!("versions/{}/{}.zip", id, package.digest());
        let target = self.root.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&target, package.archive_bytes())?;
        let replacement = InstalledPlugin {
            id: id.to_owned(),
            version: package.manifest().version.clone(),
            digest: package.digest().to_owned(),
            source,
            package_path: relative,
            enabled_scopes: BTreeSet::new(),
            grants: BTreeMap::new(),
            active_scopes: BTreeSet::new(),
        };
        let mut next = self.state.clone();
        let old = next
            .plugins
            .insert(id.to_owned(), replacement)
            .expect("current plugin checked");
        if let Err(error) = self.commit(next) {
            let _ = fs::remove_file(&target);
            return Err(error);
        }
        let _ = fs::remove_file(self.root.join(old.package_path));
        Ok(())
    }

    pub fn authorize(
        &mut self,
        id: &str,
        scope: &str,
        grants: Vec<PluginPermission>,
    ) -> Result<(), PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        for grant in &grants {
            if !permission_declared(&package.manifest().permissions, grant)
                || grant.scope != scope_kind(scope)
            {
                return Err(PluginError::NotAuthorized(
                    "授权超出清单声明或作用域".into(),
                ));
            }
        }
        let mut next = self.state.clone();
        let installed = next.plugins.get_mut(id).expect("package checked");
        installed.grants.insert(scope.to_owned(), grants);
        let complete = permissions_granted_for_scope(package.manifest(), installed, scope);
        if !complete {
            installed.active_scopes.remove(scope);
            if scope == "application" {
                // Workspace instances may share application-scoped credentials. Revoking
                // that grant therefore closes every dependent workspace admission too.
                installed
                    .active_scopes
                    .retain(|active_scope| active_scope == "application");
            }
        }
        self.commit(next)
    }

    pub fn enable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        let mut next = self.state.clone();
        next.plugins
            .get_mut(id)
            .expect("package checked")
            .enabled_scopes
            .insert(scope.to_owned());
        self.commit(next)
    }

    pub fn activate(&mut self, id: &str, scope: &str) -> Result<ActivePlugin, PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        let installed = self.state.plugins.get(id).expect("package checked");
        if !installed.enabled_scopes.contains(scope) {
            return Err(conflict("插件作用域未启用"));
        }
        if !permissions_granted_for_scope(package.manifest(), installed, scope) {
            return Err(PluginError::NotAuthorized(
                "插件声明的权限尚未全部授权".into(),
            ));
        }
        let mut next = self.state.clone();
        let active = {
            let installed = next.plugins.get_mut(id).expect("package checked");
            installed.active_scopes.insert(scope.to_owned());
            ActivePlugin {
                id: installed.id.clone(),
                version: installed.version.clone(),
                digest: installed.digest.clone(),
                scope: scope.to_owned(),
                package_path: self.root.join(&installed.package_path),
            }
        };
        self.commit(next)?;
        Ok(active)
    }

    pub fn deactivate(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        validate_scope_key(scope)?;
        let mut next = self.state.clone();
        let installed = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        installed.active_scopes.remove(scope);
        self.commit(next)
    }

    pub fn disable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        validate_scope_key(scope)?;
        let mut next = self.state.clone();
        let installed = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if installed.active_scopes.contains(scope) {
            return Err(conflict("插件仍处于激活状态"));
        }
        installed.enabled_scopes.remove(scope);
        self.commit(next)
    }

    pub fn active(&self, scope: &str) -> Vec<ActivePlugin> {
        self.state
            .plugins
            .values()
            .filter(|p| p.active_scopes.contains(scope))
            .map(|p| ActivePlugin {
                id: p.id.clone(),
                version: p.version.clone(),
                digest: p.digest.clone(),
                scope: scope.to_owned(),
                package_path: self.root.join(&p.package_path),
            })
            .collect()
    }

    /// 应用级实例与工作区准入分别记录；应用级激活不授予其他工作区权限。
    pub fn is_active_for_scope(&self, id: &str, scope: &str) -> Result<bool, PluginError> {
        validate_scope_key(scope)?;
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        Ok(installed.active_scopes.contains(scope) && installed.enabled_scopes.contains(scope))
    }

    pub fn manifests_for_scope(&self, scope: &str) -> Result<Vec<PluginManifest>, PluginError> {
        validate_scope_key(scope)?;
        self.state
            .plugins
            .keys()
            .filter_map(|id| match self.is_active_for_scope(id, scope) {
                Ok(true) => Some(self.package(id).map(|p| p.manifest().clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub fn projection(
        &self,
    ) -> Result<magi_app_server_protocol::generated::PluginList, PluginError> {
        use magi_app_server_protocol::generated::{PluginInstalled, PluginList, PluginScopeGrant};
        Ok(PluginList {
            plugins: self
                .state
                .plugins
                .values()
                .map(|installed| {
                    Ok(PluginInstalled {
                        manifest: self.package(&installed.id)?.manifest().clone(),
                        digest: installed.digest.clone(),
                        enabled_scopes: installed.enabled_scopes.iter().cloned().collect(),
                        active_scopes: installed.active_scopes.iter().cloned().collect(),
                        grants: installed
                            .grants
                            .iter()
                            .map(|(scope, permissions)| PluginScopeGrant {
                                scope: scope.clone(),
                                permissions: permissions.clone(),
                            })
                            .collect(),
                    })
                })
                .collect::<Result<_, PluginError>>()?,
        })
    }

    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if !installed.active_scopes.is_empty() || !installed.enabled_scopes.is_empty() {
            return Err(conflict("插件仍有启用作用域或激活实例"));
        }
        let package_path = self.root.join(&installed.package_path);
        let mut next = self.state.clone();
        next.plugins.remove(id);
        self.commit(next)?;
        let _ = fs::remove_file(package_path);
        Ok(())
    }

    fn commit(&mut self, next: PluginManagerState) -> Result<(), PluginError> {
        let bytes = serde_json::to_vec_pretty(&next)
            .map_err(|error| PluginError::CorruptState(error.to_string()))?;
        write_atomic(&self.root.join(STATE_FILE), bytes)?;
        self.state = next;
        Ok(())
    }
}

fn validate_plugin_path(root: &Path, plugin: &InstalledPlugin) -> Result<(), PluginError> {
    let path = Path::new(&plugin.package_path);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || !path.starts_with("versions")
        || !plugin.package_path.ends_with(".zip")
    {
        return Err(PluginError::CorruptState("安装包路径非法".into()));
    }
    if !root.join(path).is_file() {
        return Err(PluginError::CorruptState("安装包文件缺失".into()));
    }
    Ok(())
}
fn validate_scope_key(scope: &str) -> Result<(), PluginError> {
    let valid = scope == "application"
        || scope.strip_prefix("workspace:").is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        });
    if !valid {
        return Err(conflict("作用域标识无效"));
    }
    Ok(())
}
fn validate_scope(manifest: &PluginManifest, scope: &str) -> Result<(), PluginError> {
    validate_scope_key(scope)?;
    if scope == "application" && !manifest.application_instance {
        return Err(conflict("插件未声明应用级实例"));
    }
    Ok(())
}
fn scope_kind(scope: &str) -> PluginScopeKind {
    if scope == "application" {
        PluginScopeKind::Application
    } else {
        PluginScopeKind::Workspace
    }
}
fn permission_declared(declared: &[PluginPermission], grant: &PluginPermission) -> bool {
    declared.iter().any(|p| {
        p.kind == grant.kind
            && p.scope == grant.scope
            && grant.targets.iter().all(|t| p.targets.contains(t))
    })
}
fn permission_granted(permission: &PluginPermission, grants: &[PluginPermission]) -> bool {
    grants.iter().any(|g| {
        g.kind == permission.kind
            && g.scope == permission.scope
            && permission.targets.iter().all(|t| g.targets.contains(t))
    })
}

fn permissions_granted_for_scope(
    manifest: &PluginManifest,
    installed: &InstalledPlugin,
    scope: &str,
) -> bool {
    let scope_grants = installed
        .grants
        .get(scope)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let application_grants = installed
        .grants
        .get("application")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    manifest
        .permissions
        .iter()
        .all(|permission| match permission.scope {
            PluginScopeKind::Application => permission_granted(permission, application_grants),
            PluginScopeKind::Workspace if scope != "application" => {
                permission_granted(permission, scope_grants)
            }
            PluginScopeKind::Workspace => true,
        })
}
fn conflict(message: impl Into<String>) -> PluginError {
    PluginError::Conflict(message.into())
}

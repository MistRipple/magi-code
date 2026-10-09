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
        self.state.plugins.insert(
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
        if let Err(error) = self.persist() {
            self.state.plugins.remove(&manifest.id);
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
        let old = self
            .state
            .plugins
            .insert(id.to_owned(), replacement)
            .expect("current plugin checked");
        if let Err(error) = self.persist() {
            self.state.plugins.insert(id.to_owned(), old);
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
        self.state
            .plugins
            .get_mut(id)
            .expect("package checked")
            .grants
            .insert(scope.to_owned(), grants);
        self.persist()
    }

    pub fn enable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        self.state
            .plugins
            .get_mut(id)
            .expect("package checked")
            .enabled_scopes
            .insert(scope.to_owned());
        self.persist()
    }

    pub fn activate(&mut self, id: &str, scope: &str) -> Result<ActivePlugin, PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        let installed = self.state.plugins.get(id).expect("package checked");
        if !installed.enabled_scopes.contains(scope) {
            return Err(conflict("插件作用域未启用"));
        }
        let grants = installed.grants.get(scope).cloned().unwrap_or_default();
        if package.manifest().permissions.iter().any(|permission| {
            permission.scope == scope_kind(scope) && !permission_granted(permission, &grants)
        }) {
            return Err(PluginError::NotAuthorized(
                "插件声明的权限尚未全部授权".into(),
            ));
        }
        let active = {
            let installed = self.state.plugins.get_mut(id).expect("package checked");
            installed.active_scopes.insert(scope.to_owned());
            ActivePlugin {
                id: installed.id.clone(),
                version: installed.version.clone(),
                digest: installed.digest.clone(),
                scope: scope.to_owned(),
                package_path: self.root.join(&installed.package_path),
            }
        };
        self.persist()?;
        Ok(active)
    }

    pub fn deactivate(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        let installed = self
            .state
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        installed.active_scopes.remove(scope);
        self.persist()
    }

    pub fn disable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        let installed = self
            .state
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if installed.active_scopes.contains(scope) {
            return Err(conflict("插件仍处于激活状态"));
        }
        installed.enabled_scopes.remove(scope);
        self.persist()
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

    /// 判断插件是否可供一个作用域使用。应用级激活是所有工作区的共享准入，
    /// 工作区激活只对同一工作区生效；调用方必须先拿到当前作用域再查询。
    pub fn is_active_for_scope(&self, id: &str, scope: &str) -> Result<bool, PluginError> {
        let package = self.package(id)?;
        validate_scope(package.manifest(), scope)?;
        let installed = self.state.plugins.get(id).expect("package checked");
        Ok(installed.active_scopes.contains(scope)
            || (scope != "application" && installed.active_scopes.contains("application")))
    }

    /// 返回当前作用域能看到的清单；应用级插件向所有工作区投影，工作区插件只向
    /// 自己的工作区投影，避免 UI/API 把一个工作区的贡献泄漏到另一个作用域。
    pub fn manifests_for_scope(&self, scope: &str) -> Result<Vec<PluginManifest>, PluginError> {
        let mut manifests = Vec::new();
        for id in self.state.plugins.keys() {
            if self.is_active_for_scope(id, scope)? {
                manifests.push(self.package(id)?.manifest().clone());
            }
        }
        Ok(manifests)
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
        self.state.plugins.remove(id);
        self.persist()?;
        let _ = fs::remove_file(package_path);
        Ok(())
    }

    fn persist(&self) -> Result<(), PluginError> {
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| PluginError::CorruptState(error.to_string()))?;
        write_atomic(&self.root.join(STATE_FILE), bytes)?;
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
fn validate_scope(manifest: &PluginManifest, scope: &str) -> Result<(), PluginError> {
    if scope.is_empty() || (scope != "application" && !scope.starts_with("workspace:")) {
        return Err(conflict("作用域标识无效"));
    }
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
fn conflict(message: impl Into<String>) -> PluginError {
    PluginError::Conflict(message.into())
}

use crate::{
    ConvertedData, DataMigrationPlan, MigrationEntry, PluginError, PluginPackage, PluginResource,
    PluginResourceStore, invalid,
};
use magi_app_server_protocol::generated::{
    PluginCandidateOperation, PluginCandidatePreview, PluginDrainPhase, PluginDrainingScope,
    PluginInstalled as PluginInstalledProjection, PluginLifecycleLimits, PluginList,
    PluginScopeGrant, PluginSourceKind, PluginSourceProjection,
};
use magi_app_server_protocol::{
    PluginManifest, PluginPermission, PluginPermissionKind, PluginScopeKind,
};
use magi_core::{UtcMillis, fs_atomic::write_atomic};
use magi_plugin_runtime::RunCancellation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

mod drain;
mod upgrade;

pub use drain::{PluginDrainGuard, PluginRuntimeLease};
use upgrade::UpgradeOperation;

const STATE_FILE: &str = "state.json";
const STATE_VERSION: u16 = 1;
/// 设置沿用资源版本存储，使用清单资源 ID 不可能出现的保留键。
const SETTINGS_RESOURCE_ID: &str = "__settings";
const UPGRADE_OPERATION_FILE: &str = "upgrade-operation.json";
/// 第一方插件身份由 Magi 自己提供，外部包不能占用同一身份再获得原生宿主能力。
/// 新增第一方插件时必须同时加入这里，并继续通过统一的包安装入口落盘。
const BUILTIN_PLUGIN_IDS: &[&str] = &["openai.chatgpt-web", "magi.mcp-server"];
/// 停用与升级时等待在途运行自然结束的期限，到期后统一取消。
pub const PLUGIN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// 取消在途运行后等待其结算并释放租约的期限。
pub const PLUGIN_CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginSource {
    Center { url: String },
    Address { url: String },
    Local { name: String },
    Builtin { name: String },
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

/// 已通过目录、授权和参数校验、可以交给 Worker 执行的工具调用。
pub struct PreparedToolCall {
    pub plugin_id: String,
    pub package: Arc<PluginPackage>,
    pub tool: magi_app_server_protocol::PluginToolContribution,
    pub input: Value,
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
    /// 已校验包的内存副本，按插件 ID 缓存并以摘要核对；包文件不可变，
    /// 避免每次投影、准入或调用都重新读盘并解压 ZIP。
    packages: Mutex<BTreeMap<String, Arc<PluginPackage>>>,
    runtime_leases: BTreeMap<(String, String), BTreeMap<u64, RunCancellation>>,
    next_lease_id: u64,
    /// 正在关闭准入的 (插件, 作用域)，值为开始排空的时间。
    draining_scopes: BTreeMap<(String, String), UtcMillis>,
    /// 已提交但暂存数据尚未应用的升级操作：期间拒绝激活，避免新代码读到旧格式数据。
    pending_upgrade: Option<UpgradeOperation>,
}

/// 覆盖插件全部作用域的排空键，仅供升级使用；不是合法的作用域标识。
const PLUGIN_WIDE_DRAIN_SCOPE: &str = "*";

impl PluginSource {
    fn projection(&self) -> PluginSourceProjection {
        let (kind, location) = match self {
            Self::Center { url } => (PluginSourceKind::Center, url),
            Self::Address { url } => (PluginSourceKind::Address, url),
            Self::Local { name } => (PluginSourceKind::Local, name),
            Self::Builtin { name } => (PluginSourceKind::Builtin, name),
        };
        PluginSourceProjection {
            kind,
            location: location.clone(),
        }
    }
}

impl PluginManager {
    /// 工作流选择与工厂构造共用同一准入，版本从当前安装事实读取。
    pub fn prepare_workflow(
        &self,
        selected: &crate::ContributionRef,
    ) -> Result<Arc<PluginPackage>, PluginError> {
        let id = selected.plugin_id.as_str();
        self.ensure_scope_not_draining(id, crate::APPLICATION_SCOPE)?;
        if !self.is_active_for_scope(id, crate::APPLICATION_SCOPE)? {
            return Err(PluginError::NotAuthorized("插件工作流未激活".into()));
        }
        let package = self.package(id)?;
        if !package
            .manifest()
            .contributions
            .workflows
            .iter()
            .any(|workflow| workflow.id == selected.contribution_id)
        {
            return Err(PluginError::NotFound("插件工作流贡献不存在".into()));
        }
        Ok(package)
    }

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
        let mut missing = Vec::new();
        for plugin in state.plugins.values() {
            validate_plugin_path(plugin)?;
            match fs::metadata(root.join(&plugin.package_path)) {
                Ok(metadata) if metadata.is_file() => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(plugin.id.clone());
                }
                Ok(_) => return Err(PluginError::CorruptState("安装包不是普通文件".into())),
                Err(error) => return Err(error.into()),
            }
        }
        let resources =
            PluginResourceStore::open(root.join("resources.json")).map_err(PluginError::Storage)?;
        let mut manager = Self {
            root,
            state,
            resources,
            packages: Mutex::new(BTreeMap::new()),
            runtime_leases: BTreeMap::new(),
            next_lease_id: 0,
            draining_scopes: BTreeMap::new(),
            pending_upgrade: None,
        };
        // 先按原提交事实完成数据转换，不能把已提交升级误判成未提交并丢弃暂存数据。
        manager.recover_upgrade_operation()?;
        if !missing.is_empty() {
            // 旧版 Windows 曾误删仍被引用的 ZIP。隔离缺包记录，让其他功能正常启动；
            // 原始记录和资源保留，重新安装须显式启用和授权。
            let backup_path = manager.root.join(format!(
                "{STATE_FILE}.missing-packages-{}-{}",
                UtcMillis::now().0,
                std::process::id()
            ));
            write_atomic(&backup_path, fs::read(manager.root.join(STATE_FILE))?)?;
            let mut recovered = manager.state.clone();
            for id in &missing {
                let record = &manager.state.plugins[id];
                let bytes = serde_json::to_vec_pretty(record)
                    .map_err(|error| PluginError::CorruptState(error.to_string()))?;
                write_atomic(&missing_package_record_path(&manager.root, id), bytes)?;
                recovered.plugins.remove(id);
            }
            manager.commit(recovered)?;
            eprintln!(
                "插件安装包缺失，已隔离安装记录，请在插件中心重新安装：{}；原始记录备份：{}。插件资源数据已保留，重新安装后需重新启用和授权。",
                missing.join(", "),
                backup_path.display()
            );
        }
        cleanup_unreferenced_packages(&manager.root, &manager.state)?;
        Ok(manager)
    }

    pub fn state(&self) -> &PluginManagerState {
        &self.state
    }

    pub fn validate_settings(&self, id: &str, value: &Value) -> Result<(), PluginError> {
        let package = self.package(id)?;
        let validator = jsonschema::validator_for(&package.manifest().settings_schema)
            .map_err(|error| invalid(format!("插件设置 schema 无效：{error}")))?;
        if validator.is_valid(value) {
            Ok(())
        } else {
            Err(invalid("插件设置不符合清单声明的 schema"))
        }
    }

    pub fn permission_allowed(
        &self,
        id: &str,
        scope: &str,
        kind: PluginPermissionKind,
        target: &str,
    ) -> Result<bool, PluginError> {
        validate_scope_key(scope)?;
        let package = self.package(id)?;
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| PluginError::Conflict("插件未安装".into()))?;
        Ok(package.manifest().permissions.iter().any(|permission| {
            if permission.kind != kind || !permission.targets.iter().any(|item| item == target) {
                return false;
            }
            let grants = installed
                .grants
                .get(scope)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            permission_granted(permission, grants)
        }))
    }

    /// 设置读取的唯一入口：HTTP、视图消息桥和插件后台 SDK 都经过这里。
    pub fn read_settings(&self, id: &str, scope: &str) -> Result<PluginResource, PluginError> {
        self.ensure_storage_access(id, scope, SETTINGS_RESOURCE_ID)?;
        Ok(self.read_stored(id, scope, SETTINGS_RESOURCE_ID))
    }

    pub fn write_settings(
        &mut self,
        id: &str,
        scope: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_storage_access(id, scope, SETTINGS_RESOURCE_ID)?;
        self.validate_settings(id, &value)?;
        self.resources
            .write(id, scope, SETTINGS_RESOURCE_ID, expected_version, value)
            .map_err(PluginError::Resource)
    }

    /// 资源读取的唯一入口。资源必须由清单声明并取得存储授权；尚未写入的资源读到
    /// 版本 0，调用方据此携带预期版本完成首次提交。
    pub fn read_resource(
        &self,
        id: &str,
        scope: &str,
        resource_id: &str,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_declared_resource_access(id, scope, resource_id)?;
        Ok(self.read_stored(id, scope, resource_id))
    }

    /// 资源写入的唯一入口，UI 与插件后台共用；版本冲突明确返回，不覆盖较新的提交。
    pub fn write_resource(
        &mut self,
        id: &str,
        scope: &str,
        resource_id: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_declared_resource_access(id, scope, resource_id)?;
        self.resources
            .write(id, scope, resource_id, expected_version, value)
            .map_err(PluginError::Resource)
    }

    /// 订阅等长连接周期性复核：激活、声明和存储授权任一不再成立即为 `false`。
    pub fn resource_access_allowed(&self, id: &str, scope: &str, resource_id: &str) -> bool {
        self.ensure_declared_resource_access(id, scope, resource_id)
            .is_ok()
    }

    fn read_stored(&self, id: &str, scope: &str, resource_id: &str) -> PluginResource {
        self.resources
            .read(id, scope, resource_id)
            .unwrap_or(PluginResource {
                version: 0,
                value: Value::Null,
            })
    }

    fn ensure_declared_resource_access(
        &self,
        id: &str,
        scope: &str,
        resource_id: &str,
    ) -> Result<(), PluginError> {
        if !self.resource_declared(id, resource_id)? {
            return Err(PluginError::NotFound("插件资源未声明".into()));
        }
        self.ensure_storage_access(id, scope, resource_id)
    }

    /// 作用域已激活且声明的存储目标已获授权；隐藏界面入口不能代替这里的拒绝。
    fn ensure_storage_access(
        &self,
        id: &str,
        scope: &str,
        target: &str,
    ) -> Result<(), PluginError> {
        if !self.is_active_for_scope(id, scope)? {
            return Err(PluginError::NotAuthorized("插件未激活".into()));
        }
        if !self.permission_allowed(id, scope, PluginPermissionKind::Storage, target)? {
            return Err(PluginError::NotAuthorized("插件未获存储权限".into()));
        }
        Ok(())
    }

    /// 返回已安装包。包文件不可变，解析结果按摘要缓存；缓存未命中时读取并核对
    /// 身份与摘要，不一致视为状态损坏。
    pub fn package(&self, id: &str) -> Result<Arc<PluginPackage>, PluginError> {
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        let mut cache = self
            .packages
            .lock()
            .map_err(|_| PluginError::CorruptState("插件包缓存不可用".into()))?;
        if let Some(package) = cache.get(id)
            && package.digest() == installed.digest
        {
            return Ok(Arc::clone(package));
        }
        let bytes = fs::read(self.root.join(&installed.package_path))?;
        let package = PluginPackage::from_archive(&bytes)?;
        if package.manifest().id != installed.id
            || package.manifest().version != installed.version
            || package.digest() != installed.digest
        {
            return Err(PluginError::CorruptState("已安装包摘要或身份不匹配".into()));
        }
        let package = Arc::new(package);
        cache.insert(id.to_owned(), Arc::clone(&package));
        Ok(package)
    }

    /// Returns whether the installed manifest owns the resource identity.
    /// A permission grant alone must never create an implicit resource.
    pub fn resource_declared(&self, id: &str, resource_id: &str) -> Result<bool, PluginError> {
        Ok(self
            .package(id)?
            .manifest()
            .contributions
            .resources
            .iter()
            .any(|resource| resource.id == resource_id))
    }

    pub fn install(
        &mut self,
        package: &PluginPackage,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        let manifest = package.manifest();
        ensure_builtin_source(manifest.id.as_str(), &source)?;
        if let Some(existing) = self.state.plugins.get(&manifest.id) {
            if existing.digest == package.digest() {
                return Ok(());
            }
            return Err(conflict("插件已有不同版本；必须先完成排空升级"));
        }
        // 缺包恢复不能使新版本绕过已有数据的升级/迁移检查。先补回原包，再走升级。
        if self.resources.has_data(&manifest.id) {
            match fs::read(missing_package_record_path(&self.root, &manifest.id)) {
                Ok(bytes) => {
                    let previous: InstalledPlugin = serde_json::from_slice(&bytes)
                        .map_err(|error| PluginError::CorruptState(error.to_string()))?;
                    if previous.digest != package.digest() {
                        return Err(conflict(format!(
                            "插件原安装包缺失且保留了资源数据，请先重新安装原版本 {}，再执行升级",
                            previous.version
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let relative = self.write_version(package)?;
        let mut next = self.state.clone();
        next.plugins.insert(
            manifest.id.clone(),
            InstalledPlugin {
                id: manifest.id.clone(),
                version: manifest.version.clone(),
                digest: package.digest().to_string(),
                source,
                package_path: relative.clone(),
                enabled_scopes: BTreeSet::new(),
                grants: BTreeMap::new(),
                active_scopes: BTreeSet::new(),
            },
        );
        if let Err(error) = self.commit(next) {
            let _ = fs::remove_file(self.root.join(relative));
            return Err(error);
        }
        let _ = fs::remove_file(missing_package_record_path(&self.root, &manifest.id));
        Ok(())
    }

    /// 把不可变包写入版本目录，返回相对包路径。
    fn write_version(&self, package: &PluginPackage) -> Result<String, PluginError> {
        let relative = format!(
            "versions/{}/{}.zip",
            package.manifest().id,
            package.digest()
        );
        let target = self.root.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&target, package.archive_bytes())?;
        Ok(relative)
    }

    pub fn authorize(
        &mut self,
        id: &str,
        scope: &str,
        grants: Vec<PluginPermission>,
    ) -> Result<(), PluginError> {
        let package = self.package(id)?;
        validate_scope_key(scope)?;
        for grant in &grants {
            if !permission_declared(&package.manifest().permissions, grant) {
                return Err(PluginError::NotAuthorized("授权超出清单声明".into()));
            }
        }
        let mut next = self.state.clone();
        let installed = next.plugins.get_mut(id).expect("package checked");
        installed.grants.insert(scope.to_owned(), grants);
        let complete = permissions_granted_for_scope(package.manifest(), installed, scope);
        if !complete {
            installed.active_scopes.remove(scope);
        }
        self.commit(next)?;
        if !complete {
            // 撤权优先于排空租约：该范围内在途调用立即收到取消，不等待自然结束。
            self.cancel_runtime_leases_for_deactivation(id, scope);
        }
        Ok(())
    }

    pub fn enable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        self.package(id)?;
        validate_scope_key(scope)?;
        self.ensure_scope_not_draining(id, scope)?;
        let mut next = self.state.clone();
        next.plugins
            .get_mut(id)
            .expect("package checked")
            .enabled_scopes
            .insert(scope.to_owned());
        self.commit(next)
    }

    pub fn activate(&mut self, id: &str, scope: &str) -> Result<ActivePlugin, PluginError> {
        // 升级已提交但暂存数据尚未应用时先重试；仍失败就拒绝激活，避免新代码读到旧格式数据。
        self.apply_pending_upgrade()?;
        let package = self.package(id)?;
        validate_scope_key(scope)?;
        self.ensure_scope_not_draining(id, scope)?;
        ensure_host_supports_surfaces(package.manifest())?;
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
        self.package(id)?;
        validate_scope_key(scope)?;
        if self.runtime_lease_count(id, scope) > 0 {
            return Err(conflict("插件仍有在途运行，必须先完成或取消后再停用"));
        }
        let mut next = self.state.clone();
        let installed = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        installed.active_scopes.remove(scope);
        let result = self.commit(next);
        if result.is_ok() {
            self.release_drain(id, &[scope.to_owned()]);
        }
        result
    }

    pub fn disable(&mut self, id: &str, scope: &str) -> Result<(), PluginError> {
        self.package(id)?;
        validate_scope_key(scope)?;
        self.ensure_scope_not_draining(id, scope)?;
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
        let package = self.package(id)?;
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        Ok(installed.active_scopes.contains(scope)
            && installed.enabled_scopes.contains(scope)
            && permissions_granted_for_scope(package.manifest(), installed, scope))
    }

    /// 校验 Worker 调用仍绑定到激活时的包版本和作用域实例。
    ///
    /// Worker 运行期间可能跨过禁用、升级或重启边界；仅检查插件当前处于激活
    /// 状态会把迟到的旧调用错误地交给新包。所有能力桥都必须先通过这一个入口，
    /// 由当前安装事实核对不可变摘要和实例作用域。
    pub fn validate_runtime_identity(
        &self,
        id: &str,
        package_digest: &str,
        instance_scope: &str,
        scope: &str,
    ) -> Result<(), PluginError> {
        validate_scope_key(scope)?;
        if instance_scope != scope {
            return Err(PluginError::Conflict("插件实例作用域不匹配".into()));
        }
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if package_digest != installed.digest {
            return Err(PluginError::Conflict("插件执行包已过期".into()));
        }
        if !self.is_active_for_scope(id, scope)? {
            return Err(PluginError::Conflict("插件作用域未激活".into()));
        }
        Ok(())
    }

    /// 目标作用域内已激活插件的包。清单是贡献来源，这里是当前可用性的唯一投影。
    fn active_packages(&self, scope: &str) -> Result<Vec<Arc<PluginPackage>>, PluginError> {
        validate_scope_key(scope)?;
        let mut packages = Vec::new();
        for id in self.state.plugins.keys() {
            if self.is_active_for_scope(id, scope)? {
                packages.push(self.package(id)?);
            }
        }
        Ok(packages)
    }

    fn contributions_for_scope<T: Clone>(
        &self,
        scope: &str,
        select: impl Fn(&PluginManifest) -> &[T],
    ) -> Result<Vec<(PluginManifest, T)>, PluginError> {
        Ok(self
            .active_packages(scope)?
            .iter()
            .flat_map(|package| {
                let manifest = package.manifest();
                select(manifest)
                    .iter()
                    .map(move |item| (manifest.clone(), item.clone()))
            })
            .collect())
    }

    pub fn manifests_for_scope(&self, scope: &str) -> Result<Vec<PluginManifest>, PluginError> {
        Ok(self
            .active_packages(scope)?
            .iter()
            .map(|package| package.manifest().clone())
            .collect())
    }

    /// 模型可见且可执行的工具目录。只包含已获 `tools` 授权的贡献，目录与实际执行
    /// （[`PluginManager::prepare_tool_call`]）使用同一判定，不会向模型暴露必然被拒绝的工具。
    pub fn tool_contributions_for_scope(
        &self,
        scope: &str,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginToolContribution,
        )>,
        PluginError,
    > {
        let mut tools =
            self.contributions_for_scope(scope, |manifest| &manifest.contributions.tools)?;
        tools.retain(|(manifest, tool)| {
            self.permission_allowed(&manifest.id, scope, PluginPermissionKind::Tools, &tool.id)
                .unwrap_or(false)
        });
        Ok(tools)
    }

    /// 解析并校验一次模型发起的工具调用：工具必须在当前作用域的目录内，参数必须是
    /// 合法 JSON 且符合插件声明的 `inputSchema`。校验失败时不会启动 Worker。
    pub fn prepare_tool_call(
        &self,
        scope: &str,
        model_tool_name: &str,
        arguments: &str,
    ) -> Result<PreparedToolCall, PluginError> {
        let (manifest, tool) = self
            .tool_contributions_for_scope(scope)?
            .into_iter()
            .find(|(manifest, tool)| {
                crate::model_tool_name(&manifest.id, &tool.id) == model_tool_name
            })
            .ok_or_else(|| PluginError::NotFound("插件工具未激活或未授权".into()))?;
        // 部分模型对无参数工具发送空字符串，按空对象处理。
        let input: Value = if arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(arguments)
                .map_err(|_| PluginError::InvalidInput("工具参数不是合法 JSON".into()))?
        };
        let validator = jsonschema::validator_for(&tool.input_schema)
            .map_err(|error| invalid(format!("插件工具 schema 无效：{error}")))?;
        if !validator.is_valid(&input) {
            return Err(PluginError::InvalidInput(
                "工具参数不符合插件声明的 inputSchema".into(),
            ));
        }
        Ok(PreparedToolCall {
            package: self.package(&manifest.id)?,
            plugin_id: manifest.id,
            tool,
            input,
        })
    }

    pub fn command_contributions_for_scope(
        &self,
        scope: &str,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginNamedContribution,
        )>,
        PluginError,
    > {
        self.contributions_for_scope(scope, |manifest| &manifest.contributions.commands)
    }

    pub fn engine_contributions_for_scope(
        &self,
        scope: &str,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginNamedContribution,
        )>,
        PluginError,
    > {
        self.contributions_for_scope(scope, |manifest| &manifest.contributions.engines)
    }

    pub fn workflow_contributions_for_scope(
        &self,
        scope: &str,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginNamedContribution,
        )>,
        PluginError,
    > {
        self.contributions_for_scope(scope, |manifest| &manifest.contributions.workflows)
    }

    pub fn projection(&self) -> Result<PluginList, PluginError> {
        let now = UtcMillis::now();
        Ok(PluginList {
            plugins: self
                .state
                .plugins
                .values()
                .map(|installed| {
                    Ok(PluginInstalledProjection {
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
                        source: installed.source.projection(),
                        draining: self.draining_projection(&installed.id, now),
                    })
                })
                .collect::<Result<_, PluginError>>()?,
            lifecycle: PluginLifecycleLimits {
                drain_timeout_ms: PLUGIN_DRAIN_TIMEOUT.as_millis() as u64,
                cancel_settle_timeout_ms: PLUGIN_CANCEL_SETTLE_TIMEOUT.as_millis() as u64,
            },
        })
    }

    /// 排空阶段由开始时间与固定期限推出，不另存会与时间漂移的第二份状态。
    fn draining_projection(&self, id: &str, now: UtcMillis) -> Vec<PluginDrainingScope> {
        let drain = PLUGIN_DRAIN_TIMEOUT.as_millis() as u64;
        let settle = PLUGIN_CANCEL_SETTLE_TIMEOUT.as_millis() as u64;
        self.draining_scopes
            .iter()
            .filter(|((plugin_id, _), _)| plugin_id == id)
            .map(|((_, scope), started)| {
                let cancelling = now.0.saturating_sub(started.0) >= drain;
                PluginDrainingScope {
                    scope: scope.clone(),
                    phase: if cancelling {
                        PluginDrainPhase::Cancelling
                    } else {
                        PluginDrainPhase::Draining
                    },
                    started_at_ms: started.0,
                    deadline_ms: started.0 + drain + if cancelling { settle } else { 0 },
                }
            })
            .collect()
    }

    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        // 禁用/撤权只会发送取消信号；租约在调用真正结算前仍代表旧包正在被使用。
        // 先删除包会让迟到的 Worker 失去可核对的版本事实，并把取消误报成资源释放。
        if self.plugin_lease_count(id) > 0 {
            return Err(conflict("插件仍有在途运行，必须完成资源结算后再卸载"));
        }
        if !installed.active_scopes.is_empty() || !installed.enabled_scopes.is_empty() {
            return Err(conflict("插件仍有启用作用域或激活实例"));
        }
        let package_path = self.root.join(&installed.package_path);
        let staged_path = if package_path.exists() {
            Some(stage_package_for_removal(&package_path)?)
        } else {
            None
        };
        let mut next = self.state.clone();
        next.plugins.remove(id);
        if let Err(error) = self.commit(next) {
            if let Some(staged_path) = staged_path {
                let _ = fs::rename(staged_path, &package_path);
            }
            return Err(error);
        }
        cleanup_unreferenced_packages(&self.root, &self.state)?;
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

/// 宿主（BrowserAuthority 与 Main）当前只实现了一种原生 Browser surface 语义：应用级唯一实例、
/// 持久分区、保持挂载、关闭按钮只隐藏视图。其他组合在合同中合法，但宿主不能挂载，
/// 激活时明确拒绝，而不是接纳后静默按此语义运行。
fn ensure_host_supports_surfaces(manifest: &PluginManifest) -> Result<(), PluginError> {
    use magi_app_server_protocol::generated::{PluginSurfaceCloseAction, PluginSurfacePersistence};
    let unsupported = manifest
        .contributions
        .browser_surfaces
        .iter()
        .find(|surface| {
            surface.scope != PluginScopeKind::Application
                || surface.persistence != PluginSurfacePersistence::Persistent
                || !surface.keep_mounted
                || surface.close_action != PluginSurfaceCloseAction::Hide
        });
    match unsupported {
        Some(surface) => Err(conflict(format!(
            "宿主尚不支持浏览器 surface {} 声明的挂载策略",
            surface.id
        ))),
        None => Ok(()),
    }
}

fn ensure_builtin_source(id: &str, source: &PluginSource) -> Result<(), PluginError> {
    if BUILTIN_PLUGIN_IDS.contains(&id) && !matches!(source, PluginSource::Builtin { .. }) {
        return Err(conflict("第一方插件身份只能由 Magi 内置包提供"));
    }
    Ok(())
}

fn validate_plugin_path(plugin: &InstalledPlugin) -> Result<(), PluginError> {
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
    Ok(())
}

fn missing_package_record_path(root: &Path, id: &str) -> PathBuf {
    use sha2::Digest;
    let key = sha2::Sha256::digest(id.as_bytes());
    root.join(format!("missing-package-{key:x}.json"))
}

fn stage_package_for_removal(path: &Path) -> Result<PathBuf, PluginError> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PluginError::CorruptState("安装包文件名无效".into()))?;
    let staged = path.with_file_name(format!("{file_name}.removing"));
    fs::rename(path, &staged).map_err(PluginError::Storage)?;
    Ok(staged)
}

fn cleanup_unreferenced_packages(
    root: &Path,
    state: &PluginManagerState,
) -> Result<(), PluginError> {
    let versions = root.join("versions");
    let referenced = state
        .plugins
        .values()
        .map(|plugin| Path::new(&plugin.package_path))
        .collect::<BTreeSet<_>>();
    let entries = match fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for file in fs::read_dir(entry.path())? {
            let file = file?;
            let path = file.path();
            // 安装记录使用 `/`；Windows read_dir 返回 `\`。按宿主路径比较，
            // 不能把分隔符不同的当前版本误当成未引用包删除。
            let relative = path.strip_prefix(root).ok();
            if file.file_type()?.is_file()
                && relative.is_some_and(|value| !referenced.contains(value))
            {
                fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}
/// 插件全局生效：启用、授权和激活都只有应用级一份，作用域键固定为 `application`。
fn validate_scope_key(scope: &str) -> Result<(), PluginError> {
    if scope != crate::APPLICATION_SCOPE {
        return Err(conflict("插件全局生效，作用域只能是 application"));
    }
    Ok(())
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
    let grants = installed
        .grants
        .get(scope)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    manifest
        .permissions
        .iter()
        .all(|permission| permission_granted(permission, grants))
}
fn conflict(message: impl Into<String>) -> PluginError {
    PluginError::Conflict(message.into())
}

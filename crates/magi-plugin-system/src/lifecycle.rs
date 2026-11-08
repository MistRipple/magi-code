use crate::{PluginError, PluginPackage, PluginResource, PluginResourceStore, invalid};
use magi_app_server_protocol::generated::{
    PluginCandidateOperation, PluginCandidatePreview, PluginInstalled as PluginInstalledProjection,
    PluginList, PluginSourceKind, PluginSourceProjection,
};
use magi_app_server_protocol::{PluginManifest, PluginPermission, PluginPermissionKind};
use magi_core::{UtcMillis, fs_atomic::write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

mod compatibility;
mod drain;
mod image_attachments;
mod runtime_catalog;
mod upgrade;

pub use drain::PluginDrainGuard;

const STATE_FILE: &str = "state.json";
const STATE_VERSION: u16 = 2;
/// 设置沿用资源版本存储，使用清单资源 ID 不可能出现的保留键。
const SETTINGS_RESOURCE_ID: &str = "__settings";
/// Magi MCP 等原生功能不通过插件身份表达。这里仅保留旧版本内置记录的退役清理，
/// 防止历史占位包重新获得宿主能力（见 `retired_builtins`）。
const BUILTIN_PLUGIN_IDS: &[&str] = &[];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginSource {
    Center {
        url: String,
    },
    Address {
        url: String,
    },
    Local {
        name: String,
    },
    Builtin {
        name: String,
    },
    /// 上游生态（DSH、pi）的 npm 发布包，经翻译器在本机安装期翻译。版本是安装时固定的精确版本。
    Upstream {
        ecosystem: String,
        package: String,
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        github: Option<magi_app_server_protocol::generated::PluginGithubSource>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub source: PluginSource,
    pub package_path: String,
    pub enabled: bool,
    pub grants: Vec<PluginPermission>,
    pub active: bool,
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

/// 已通过目录、授权和参数校验、可以交给扩展宿主执行的工具调用。
#[derive(Clone, Debug)]
pub struct PreparedToolCall {
    pub plugin_id: String,
    /// 校验时的包摘要；执行方据此确认宿主仍是同一版本。
    pub digest: String,
    pub tool: magi_app_server_protocol::PluginToolContribution,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePlugin {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub package_path: PathBuf,
}

/// daemon 插件服务的唯一写入口。来源解析和下载在外层完成，进入此处的始终是已校验包。
pub struct PluginManager {
    root: PathBuf,
    runtime_catalogs: BTreeMap<String, runtime_catalog::RuntimeCatalog>,
    state: PluginManagerState,
    resources: PluginResourceStore,
    /// 已校验包的内存副本，按插件 ID 缓存并以摘要核对；包文件不可变，
    /// 避免每次投影、准入或调用都重新读盘并解压 ZIP。
    packages: Mutex<BTreeMap<String, Arc<PluginPackage>>>,
    /// 正在关闭准入的插件。
    draining: BTreeSet<String>,
    /// 数据重置提交未完成时拒绝该插件的新准入，避免恢复日志覆盖其后的编辑。
    pending_reset: Option<String>,
    /// 本次打开时退役的历史内置记录；原生功能自身拥有当前配置。
    retired_builtins: Vec<RetiredBuiltin>,
}

/// 被退役的内置插件：`was_active` 为退役前是否激活。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetiredBuiltin {
    pub id: String,
    pub was_active: bool,
}

impl PluginSource {
    fn projection(&self) -> PluginSourceProjection {
        let (kind, location) = match self {
            Self::Center { url } => (PluginSourceKind::Center, url.clone()),
            Self::Address { url } => (PluginSourceKind::Address, url.clone()),
            Self::Local { name } => (PluginSourceKind::Local, name.clone()),
            Self::Builtin { name } => (PluginSourceKind::Builtin, name.clone()),
            Self::Upstream {
                ecosystem,
                package,
                version,
                ..
            } => (
                PluginSourceKind::Upstream,
                format!("{ecosystem}:{package}@{version}"),
            ),
        };
        let github = match self {
            Self::Upstream { github, .. } => github.clone(),
            _ => None,
        };
        PluginSourceProjection {
            kind,
            location,
            github,
        }
    }
}

impl PluginManager {
    /// 本次打开时退役的内置插件记录。
    pub fn retired_builtins(&self) -> &[RetiredBuiltin] {
        &self.retired_builtins
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
            runtime_catalogs: BTreeMap::new(),
            state,
            resources,
            packages: Mutex::new(BTreeMap::new()),
            draining: BTreeSet::new(),
            pending_reset: None,
            retired_builtins: Vec::new(),
        };
        manager.recover_data_reset()?;
        manager.collect_image_attachment_files()?;
        // 内置包目录是宿主支持能力的唯一入口；停发的内置包不能在重启后恢复激活。
        // 先备份安装事实，再移除注册表记录。资源数据保留，不把退役包当作外部插件加载。
        let retired_records: Vec<_> = manager
            .state
            .plugins
            .values()
            .filter(|plugin| {
                matches!(plugin.source, PluginSource::Builtin { .. })
                    && !BUILTIN_PLUGIN_IDS.contains(&plugin.id.as_str())
            })
            .map(|plugin| RetiredBuiltin {
                id: plugin.id.clone(),
                was_active: plugin.active,
            })
            .collect();
        let retired: Vec<_> = retired_records.iter().map(|r| r.id.clone()).collect();
        if !retired.is_empty() {
            let backup = manager.root.join(format!(
                "{STATE_FILE}.retired-builtins-{}-{}",
                UtcMillis::now().0,
                std::process::id()
            ));
            write_atomic(
                &backup,
                serde_json::to_vec_pretty(&manager.state)
                    .map_err(|error| PluginError::CorruptState(error.to_string()))?,
            )?;
            let mut next = manager.state.clone();
            for id in &retired {
                next.plugins.remove(id);
            }
            manager.commit(next)?;
            missing.retain(|id| !retired.contains(id));
            manager.retired_builtins = retired_records;
        }

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

    /// daemon 的安装回执与原安装事实共用状态根，独立日志不拥有插件版本。
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state(&self) -> &PluginManagerState {
        &self.state
    }

    pub fn validate_settings(&self, id: &str, value: &Value) -> Result<(), PluginError> {
        let manifest = self.runtime_manifest(id, true)?;
        let validator = jsonschema::validator_for(&manifest.settings_schema)
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
        kind: PluginPermissionKind,
        target: &str,
    ) -> Result<bool, PluginError> {
        let package = self.package(id)?;
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| PluginError::Conflict("插件未安装".into()))?;
        Ok(package.manifest().permissions.iter().any(|permission| {
            if permission.kind != kind
                || !permission.targets.iter().any(|item| {
                    item == target
                        || (item == "*"
                            && matches!(
                                kind,
                                PluginPermissionKind::Tools | PluginPermissionKind::ModelSources
                            ))
                })
            {
                return false;
            }
            permission_granted(permission, &installed.grants)
        }))
    }

    /// 设置读取的唯一入口：HTTP、视图消息桥和插件后台 SDK 都经过这里。
    pub fn read_settings(&self, id: &str) -> Result<PluginResource, PluginError> {
        self.ensure_storage_access(id, SETTINGS_RESOURCE_ID)?;
        Ok(self.read_stored(id, SETTINGS_RESOURCE_ID))
    }

    pub fn write_settings(
        &mut self,
        id: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_storage_access(id, SETTINGS_RESOURCE_ID)?;
        self.validate_settings(id, &value)?;
        self.resources
            .write(id, SETTINGS_RESOURCE_ID, expected_version, value)
            .map_err(PluginError::Resource)
    }

    /// 资源读取的唯一入口。资源必须由清单声明并取得存储授权；尚未写入的资源读到
    /// 版本 0，调用方据此携带预期版本完成首次提交。
    pub fn read_resource(
        &self,
        id: &str,
        resource_id: &str,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_declared_resource_access(id, resource_id)?;
        Ok(self.read_stored(id, resource_id))
    }

    /// 资源写入的唯一入口，UI 与插件后台共用；版本冲突明确返回，不覆盖较新的提交。
    pub fn write_resource(
        &mut self,
        id: &str,
        resource_id: &str,
        expected_version: u64,
        value: Value,
    ) -> Result<PluginResource, PluginError> {
        self.ensure_declared_resource_access(id, resource_id)?;
        self.resources
            .write(id, resource_id, expected_version, value)
            .map_err(PluginError::Resource)
    }

    /// 订阅等长连接周期性复核：激活、声明和存储授权任一不再成立即为 `false`。
    pub fn resource_access_allowed(&self, id: &str, resource_id: &str) -> bool {
        self.ensure_declared_resource_access(id, resource_id)
            .is_ok()
    }

    fn read_stored(&self, id: &str, resource_id: &str) -> PluginResource {
        self.resources
            .read(id, resource_id)
            .unwrap_or(PluginResource {
                version: 0,
                value: Value::Null,
            })
    }

    fn ensure_declared_resource_access(
        &self,
        id: &str,
        resource_id: &str,
    ) -> Result<(), PluginError> {
        if !self.resource_declared(id, resource_id)? {
            return Err(PluginError::NotFound("插件资源未声明".into()));
        }
        self.ensure_storage_access(id, resource_id)
    }

    /// 插件已激活且声明的存储目标已获授权；隐藏界面入口不能代替这里的拒绝。
    fn ensure_storage_access(&self, id: &str, target: &str) -> Result<(), PluginError> {
        self.ensure_not_draining(id)?;
        if !self.is_active(id)? {
            return Err(PluginError::NotAuthorized("插件未激活".into()));
        }
        if !self.permission_allowed(id, PluginPermissionKind::Storage, target)? {
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
                enabled: false,
                grants: Vec::new(),
                active: false,
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
        grants: Vec<PluginPermission>,
    ) -> Result<(), PluginError> {
        let package = self.package(id)?;
        for grant in &grants {
            if !permission_declared(&package.manifest().permissions, grant) {
                return Err(PluginError::NotAuthorized("授权超出清单声明".into()));
            }
        }
        let mut next = self.state.clone();
        let installed = next.plugins.get_mut(id).expect("package checked");
        installed.grants = grants;
        let complete = permissions_granted(package.manifest(), installed);
        if !complete {
            installed.active = false;
        }
        self.commit(next)?;
        Ok(())
    }

    pub fn enable(&mut self, id: &str) -> Result<(), PluginError> {
        self.package(id)?;
        self.ensure_not_draining(id)?;
        let mut next = self.state.clone();
        next.plugins.get_mut(id).expect("package checked").enabled = true;
        self.commit(next)
    }

    pub fn activate(&mut self, id: &str) -> Result<ActivePlugin, PluginError> {
        let package = self.package(id)?;
        self.ensure_not_draining(id)?;
        ensure_host_supports_surfaces(package.manifest())?;
        let installed = self.state.plugins.get(id).expect("package checked");
        if !installed.enabled {
            return Err(conflict("插件未启用"));
        }
        if !permissions_granted(package.manifest(), installed) {
            return Err(PluginError::NotAuthorized(
                "插件声明的权限尚未全部授权".into(),
            ));
        }
        let mut next = self.state.clone();
        let active = {
            let installed = next.plugins.get_mut(id).expect("package checked");
            installed.active = true;
            ActivePlugin {
                id: installed.id.clone(),
                version: installed.version.clone(),
                digest: installed.digest.clone(),
                package_path: self.root.join(&installed.package_path),
            }
        };
        self.commit(next)?;
        Ok(active)
    }

    pub fn deactivate(&mut self, id: &str) -> Result<(), PluginError> {
        self.package(id)?;
        let mut next = self.state.clone();
        let installed = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        installed.active = false;
        let result = self.commit(next);
        if result.is_ok() {
            self.release_drain(id);
        }
        result
    }

    pub fn disable(&mut self, id: &str) -> Result<(), PluginError> {
        self.package(id)?;
        self.ensure_not_draining(id)?;
        let mut next = self.state.clone();
        let installed = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if installed.active {
            return Err(conflict("插件仍处于激活状态"));
        }
        installed.enabled = false;
        self.commit(next)
    }

    pub fn active(&self) -> Vec<ActivePlugin> {
        self.state
            .plugins
            .values()
            .filter(|p| p.active)
            .map(|p| ActivePlugin {
                id: p.id.clone(),
                version: p.version.clone(),
                digest: p.digest.clone(),
                package_path: self.root.join(&p.package_path),
            })
            .collect()
    }

    /// 应用级实例与工作区准入分别记录；应用级激活不授予其他工作区权限。
    pub fn is_active(&self, id: &str) -> Result<bool, PluginError> {
        let package = self.package(id)?;
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        Ok(self.pending_reset.as_deref() != Some(id)
            && !self.is_draining(id)
            && installed.active
            && installed.enabled
            && permissions_granted(package.manifest(), installed))
    }

    /// 全局已激活插件的包。清单是贡献来源，这里是当前可用性的唯一投影。
    fn active_packages(&self) -> Result<Vec<Arc<PluginPackage>>, PluginError> {
        let mut packages = Vec::new();
        for id in self.state.plugins.keys() {
            if self.is_active(id)? {
                packages.push(self.package(id)?);
            }
        }
        Ok(packages)
    }

    /// 插件是否有持久数据（资源或设置）。
    pub fn has_resource_data(&self, plugin_id: &str) -> bool {
        self.resources.has_data(plugin_id)
    }

    pub fn manifests(&self) -> Result<Vec<PluginManifest>, PluginError> {
        Ok(self
            .active_packages()?
            .iter()
            .map(|package| self.effective_manifest(&package.manifest().id))
            .collect::<Result<_, _>>()?)
    }

    /// 模型可见且可执行的工具目录：只含已获 `tools` 授权的贡献。目录与执行
    /// （[`PluginManager::prepare_tool_call`]）使用同一判定，不向模型暴露必然被拒绝的工具。
    pub fn tool_contributions(
        &self,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginToolContribution,
        )>,
        PluginError,
    > {
        let mut tools = Vec::new();
        for package in self.active_packages()? {
            let manifest = self.effective_manifest(&package.manifest().id)?;
            for tool in &manifest.contributions.tools {
                if self.permission_allowed(&manifest.id, PluginPermissionKind::Tools, &tool.id)? {
                    tools.push((manifest.clone(), tool.clone()));
                }
            }
        }
        Ok(tools)
    }

    /// 已激活插件声明的用户命令目录。
    pub fn command_contributions(
        &self,
    ) -> Result<
        Vec<(
            PluginManifest,
            magi_app_server_protocol::PluginNamedContribution,
        )>,
        PluginError,
    > {
        let mut commands = Vec::new();
        for package in self.active_packages()? {
            let manifest = self.effective_manifest(&package.manifest().id)?;
            for command in &manifest.contributions.commands {
                commands.push((manifest.clone(), command.clone()));
            }
        }
        Ok(commands)
    }

    /// 解析并校验一次模型发起的工具调用：工具必须在当前激活目录内，参数必须是
    /// 合法 JSON 且符合插件声明的 `inputSchema`。校验失败时不会启动任何扩展调用。
    pub fn prepare_tool_call(
        &self,
        model_tool_name: &str,
        arguments: &str,
    ) -> Result<PreparedToolCall, PluginError> {
        let (manifest, tool) = self
            .tool_contributions()?
            .into_iter()
            .find(|(manifest, tool)| {
                crate::model_tool_name(&manifest.id, &tool.id) == model_tool_name
            })
            .ok_or_else(|| PluginError::NotFound("插件工具未激活或未授权".into()))?;
        let digest = self.package(&manifest.id)?.digest().to_owned();
        // 部分模型对无参数工具发送空字符串，按空对象处理。
        let input: serde_json::Value = if arguments.trim().is_empty() {
            serde_json::Value::Object(Default::default())
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
            plugin_id: manifest.id,
            digest,
            tool,
            input,
        })
    }

    pub fn projection(&self) -> Result<PluginList, PluginError> {
        self.projection_with_upstream_metadata(false)
    }

    pub fn projection_with_upstream_metadata(
        &self,
        include_upstream: bool,
    ) -> Result<PluginList, PluginError> {
        Ok(PluginList {
            plugins: self
                .state
                .plugins
                .values()
                .map(|installed| {
                    let upstream_manifest = if include_upstream {
                        self.upstream_manifest(installed)?
                    } else {
                        None
                    };
                    let upstream_archive_path = if upstream_manifest.is_some() {
                        Some(
                            fs::canonicalize(self.root.join(&installed.package_path))?
                                .to_string_lossy()
                                .into_owned(),
                        )
                    } else {
                        None
                    };
                    Ok(PluginInstalledProjection {
                        manifest: self.effective_manifest(&installed.id)?,
                        digest: installed.digest.clone(),
                        enabled: installed.enabled,
                        active: installed.active,
                        grants: installed.grants.clone(),
                        source: installed.source.projection(),
                        upstream_manifest,
                        upstream_archive_path,
                        // 扩展宿主的运行状态属于 daemon 的 ExtensionRuntime，由 API 层合并，管理器不持有它。
                        runtime: None,
                    })
                })
                .collect::<Result<_, PluginError>>()?,
        })
    }

    // 原包元数据只从当前不可变安装包取得；不读插件可写目录或 registry 最新版。
    fn upstream_manifest(&self, installed: &InstalledPlugin) -> Result<Option<Value>, PluginError> {
        let PluginSource::Upstream {
            package: name,
            version,
            ..
        } = &installed.source
        else {
            return Ok(None);
        };
        let package = self.package(&installed.id)?;
        let Some(bytes) = package.files().get("extension/upstream/package.json") else {
            return Ok(None);
        };
        let manifest: Value = serde_json::from_slice(bytes)
            .map_err(|error| PluginError::CorruptState(format!("上游原包元数据无效：{error}")))?;
        if !manifest.is_object() || manifest["name"] != *name || manifest["version"] != *version {
            return Err(PluginError::CorruptState(
                "上游原包元数据与已安装来源不一致".into(),
            ));
        }
        Ok(Some(manifest))
    }

    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let installed = self
            .state
            .plugins
            .get(id)
            .ok_or_else(|| conflict("插件未安装"))?;
        if installed.active || installed.enabled {
            return Err(conflict("插件仍处于启用或激活状态"));
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
        self.invalidate_inactive_runtimes();
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
            surface.persistence != PluginSurfacePersistence::Persistent
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
    if let PluginSource::Builtin { name } = source
        && (!BUILTIN_PLUGIN_IDS.contains(&id) || name != id)
    {
        return Err(conflict("该内置插件不在当前宿主支持目录中"));
    }

    if BUILTIN_PLUGIN_IDS.contains(&id) && !matches!(source, PluginSource::Builtin { .. }) {
        return Err(conflict("历史内置插件身份不能由外部包占用"));
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
    root.join(format!(
        "missing-package-{}.json",
        magi_core::hex_lower(key)
    ))
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

fn permission_declared(declared: &[PluginPermission], grant: &PluginPermission) -> bool {
    declared
        .iter()
        .any(|p| p.kind == grant.kind && grant.targets.iter().all(|t| p.targets.contains(t)))
}
fn permission_granted(permission: &PluginPermission, grants: &[PluginPermission]) -> bool {
    grants.iter().any(|g| {
        g.kind == permission.kind && permission.targets.iter().all(|t| g.targets.contains(t))
    })
}

fn permissions_granted(manifest: &PluginManifest, installed: &InstalledPlugin) -> bool {
    let grants = &installed.grants;
    manifest
        .permissions
        .iter()
        .all(|permission| permission_granted(permission, grants))
}
fn conflict(message: impl Into<String>) -> PluginError {
    PluginError::Conflict(message.into())
}

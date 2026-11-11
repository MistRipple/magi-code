use crate::{PluginError, invalid};
use magi_app_server_protocol::{PluginManifest, PluginPermissionKind, PluginViewPlacement};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read},
    sync::OnceLock,
};

/// 与上游下载及准备阶段采用同一 64 MiB 容量，容纳原包资源及共享页面产物。
pub const MAX_PACKAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
/// 浏览器与 Node 的构建产物均可包含完整依赖；仍受归档与解压总量约束。
pub const MAX_SCRIPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_HOME_CARDS_PER_PLUGIN: usize = 2;
/// 工具说明是模型可见的使用指引，真实工具常需要数 KiB 的约束与示例；其余贡献的说明仍限 4096 字节。
pub const MAX_TOOL_DESCRIPTION_BYTES: usize = 32 * 1024;

/// 已校验的不可变包。安装与执行读取同一份内容，避免校验后再次读取外部源文件。
#[derive(Clone, Debug)]
pub struct PluginPackage {
    manifest: PluginManifest,
    digest: String,
    archive: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
    executable_files: HashSet<String>,
}

impl PluginPackage {
    pub fn from_archive(bytes: &[u8]) -> Result<Self, PluginError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKAGE_BYTES {
            return Err(invalid("发布包大小超限"));
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| invalid("发布包必须是 Magi ZIP 包"))?;
        if archive.len() > MAX_FILES {
            return Err(invalid("发布包文件数量超限"));
        }
        let mut files = BTreeMap::new();
        let mut executable_files = HashSet::new();
        let mut names = HashSet::new();
        let mut total = 0usize;
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|_| invalid("无法读取发布包条目"))?;
            let name = file
                .name()
                .map_err(|_| invalid("无法读取发布包文件路径"))?
                .trim_end_matches('/')
                .to_string();
            let native_asset = name.starts_with("extension/upstream/native/");
            if (!valid_path(&name) && !(native_asset && valid_native_path(&name)))
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err(invalid("发布包包含非法、重复或大小写冲突路径"));
            }
            let mode = file.unix_mode().unwrap_or(0);
            let kind = mode & 0o170000;
            if !matches!(kind, 0 | 0o100000 | 0o040000)
                || mode & 0o111 != 0 && !file.is_dir() && !native_asset
            {
                return Err(invalid("发布包不得包含符号链接、特殊文件或可执行权限文件"));
            }
            if file.is_dir() {
                continue;
            }
            if mode & 0o111 != 0 {
                executable_files.insert(name.clone());
            }
            let limit = file_limit(&name);
            if file.size() > limit as u64 {
                return Err(invalid(format!(
                    "发布包文件 {name} 为 {} 字节，超过 {limit} 字节上限",
                    file.size()
                )));
            }
            let mut content = Vec::new();
            file.by_ref()
                .take(limit as u64 + 1)
                .read_to_end(&mut content)?;
            total = total
                .checked_add(content.len())
                .ok_or_else(|| invalid("发布包大小超限"))?;
            if content.len() > limit || total > MAX_UNPACKED_BYTES {
                return Err(invalid("发布包展开大小超限"));
            }
            validate_file(&name, &content)?;
            files.insert(name, content);
        }
        let manifest_bytes = files
            .get("manifest.json")
            .ok_or_else(|| invalid("缺少 manifest.json"))?;
        reject_removed_sdk(manifest_bytes)?;
        let mut manifest: PluginManifest = serde_json::from_slice(manifest_bytes)
            .map_err(|_| invalid("manifest.json 不符合插件合同"))?;
        validate_manifest(&manifest)?;
        // 资源准入遵循公共 Node extension 合同，不按上游生态区别授权。
        // validate_manifest 要求 full-trust；下方核对入口和 extension 目录归属。
        annotate_settings_order(&mut manifest, manifest_bytes);
        // 声明 extension 的包必须携带入口；纯声明式包（技能、视图、资源、浏览器 surface）不携带任何可执行文件。
        if manifest.extension.is_some() != files.contains_key("extension/index.mjs") {
            return Err(invalid(
                "extension 声明与 extension/index.mjs 必须同时存在或同时缺失",
            ));
        }
        if manifest.extension.is_none() && files.keys().any(|name| name.starts_with("extension/")) {
            return Err(invalid("未声明 extension 的包不得携带 extension/ 目录"));
        }
        for view in &manifest.contributions.views {
            if !valid_path(&view.entry)
                || !view.entry.starts_with("ui/")
                || !view.entry.ends_with(".html")
                || !files.contains_key(&view.entry)
            {
                return Err(invalid("视图入口必须引用包内 ui/ 下的 HTML 文件"));
            }
        }
        if let Some(icon) = manifest.icon.as_deref() {
            let bytes = files
                .get(icon)
                .ok_or_else(|| invalid("图标必须引用包内 ui/ 下的图片文件"))?;
            validate_icon(icon, bytes)?;
        }
        Ok(Self {
            manifest,
            digest: magi_core::hex_lower(Sha256::digest(bytes)),
            archive: bytes.to_vec(),
            files,
            executable_files,
        })
    }

    /// 清单声明的图标：(内容类型, 字节)。SVG 保留矢量原文，与位图共用图片入口。
    pub fn icon(&self) -> Option<(&'static str, &[u8])> {
        let path = self.manifest.icon.as_deref()?;
        let bytes = self.files.get(path)?;
        Some((icon_content_type(path)?, bytes.as_slice()))
    }
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    /// 仅准入包中显式携带执行位的原生辅助文件；路径与普通文件类型已在解包时核对。
    pub fn extension_file_is_executable(&self, name: &str) -> bool {
        self.executable_files.contains(name)
    }
    pub(crate) fn archive_bytes(&self) -> &[u8] {
        &self.archive
    }
    /// 扩展代码树：`extension/` 下的全部文件（含入口），路径相对包根。仅声明了 `extension` 的包有。
    pub fn extension_files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.manifest.extension.iter().flat_map(|_| {
            self.files
                .iter()
                .filter(|(name, _)| name.starts_with("extension/"))
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        })
    }
    /// Node 扩展入口源码（`extension/index.mjs`）。仅声明了 `extension` 的包有。
    pub fn extension_entry(&self) -> Option<&[u8]> {
        self.manifest
            .extension
            .as_ref()
            .and_then(|_| self.files.get("extension/index.mjs").map(Vec::as_slice))
    }
}

/// SDK 1（QuickJS 后台）已移除：给出明确原因，而不是笼统的“不符合插件合同”。
fn reject_removed_sdk(manifest_bytes: &[u8]) -> Result<(), PluginError> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(manifest_bytes) else {
        return Ok(());
    };
    let legacy = value.get("sdkVersion").and_then(|v| v.as_u64()) == Some(1)
        || value.get("backend").is_some();
    if legacy {
        return Err(invalid(REMOVED_SDK_MESSAGE));
    }
    Ok(())
}

/// 设置表单按作者书写的顺序展示字段。JSON 对象在内存中按键排序，原始顺序会丢失，
/// 因此在解包时从原始字节恢复，并作为宿主注解 `x-property-order` 写进设置 schema。
/// 注解不参与校验（未知关键字被忽略），也不影响包摘要（摘要取自原始 ZIP）。
fn annotate_settings_order(manifest: &mut PluginManifest, manifest_bytes: &[u8]) {
    struct OrderedKeys(Vec<String>);
    impl<'de> serde::Deserialize<'de> for OrderedKeys {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = OrderedKeys;
                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut keys = Vec::new();
                    while let Some((key, _)) = map.next_entry::<String, serde::de::IgnoredAny>()? {
                        keys.push(key);
                    }
                    Ok(OrderedKeys(keys))
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    #[derive(serde::Deserialize)]
    struct Schema {
        properties: Option<OrderedKeys>,
    }
    #[derive(serde::Deserialize)]
    struct Root {
        #[serde(rename = "settingsSchema")]
        settings_schema: Option<Schema>,
    }
    let Some(keys) = serde_json::from_slice::<Root>(manifest_bytes)
        .ok()
        .and_then(|root| root.settings_schema)
        .and_then(|schema| schema.properties)
    else {
        return;
    };
    if let Some(schema) = manifest.settings_schema.as_object_mut() {
        schema.insert(
            "x-property-order".into(),
            serde_json::Value::Array(keys.0.into_iter().map(serde_json::Value::String).collect()),
        );
    }
}

/// Node 扩展声明的规则：钩子必须在目录内、模式合法且不重复；必须有“完全信任”授权项；
///
fn validate_extension(manifest: &PluginManifest) -> Result<(), PluginError> {
    use magi_app_server_protocol::generated::PluginHookMode;
    use magi_extension_host::protocol::{HookMode, HookRegistration};
    let trust_declared = manifest.permissions.iter().any(|permission| {
        permission.kind == PluginPermissionKind::Extension && permission.targets == ["full-trust"]
    });
    let Some(extension) = &manifest.extension else {
        if manifest.permissions.iter().any(|permission| {
            matches!(
                permission.kind,
                PluginPermissionKind::Extension
                    | PluginPermissionKind::ContextRewrite
                    | PluginPermissionKind::ModelSources
                    | PluginPermissionKind::PluginManagement
                    | PluginPermissionKind::SessionWrite
                    | PluginPermissionKind::Attachments
                    | PluginPermissionKind::WebProviders
            ) || (permission.kind == PluginPermissionKind::SessionRead
                && permission.targets.iter().any(|target| target != "render"))
        }) {
            return Err(invalid(
                "extension / contextRewrite / modelSources / pluginManagement / sessionRead / attachments 权限只能由声明了 extension 的包使用",
            ));
        }
        return Ok(());
    };
    if extension.runtime_contributions.is_empty()
        && extension.hooks.is_empty()
        && manifest.contributions.tools.is_empty()
        && manifest.contributions.commands.is_empty()
        && manifest.contributions.model_sources.is_empty()
        // 视图的后端路由由扩展宿主服务；只有界面和后端、没有其他贡献的包同样合法。
        && manifest.contributions.views.is_empty()
    {
        return Err(invalid(
            "声明 extension 的包必须至少声明一个钩子、工具、命令、模型数据源或视图",
        ));
    }
    // 改写已装配历史会让缓存前缀失效并改变模型看到的上下文，必须由用户单独授权。
    let rewrites_history = extension
        .hooks
        .iter()
        .any(|hook| magi_extension_host::EXTENSION_REWRITE_HOOKS.contains(&hook.name.as_str()));
    let rewrite_declared = manifest.permissions.iter().any(|permission| {
        permission.kind == PluginPermissionKind::ContextRewrite && permission.targets == ["history"]
    });
    if rewrites_history != rewrite_declared {
        return Err(invalid(
            "context.assemble / compaction.before 钩子必须且只能与权限 contextRewrite:[\"history\"] 一同声明",
        ));
    }
    if !trust_declared {
        return Err(invalid(
            "声明 extension 的包必须声明权限 extension:[\"full-trust\"]，用户据此确认扩展以其用户权限运行",
        ));
    }
    let mut seen = HashSet::new();
    for hook in &extension.hooks {
        let registration = HookRegistration {
            name: hook.name.clone(),
            mode: match hook.mode {
                PluginHookMode::Observe => HookMode::Observe,
                PluginHookMode::Advise => HookMode::Advise,
                PluginHookMode::Guard => HookMode::Guard,
            },
            deadline_ms: hook.deadline_ms.map(|ms| ms as u64),
        };
        registration
            .validate_wired()
            .map_err(|error| invalid(format!("扩展钩子声明无效：{}", error.message)))?;
        if !seen.insert(hook.name.clone()) {
            return Err(invalid(format!("扩展钩子 {} 重复声明", hook.name)));
        }
    }
    Ok(())
}

pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
    validate_manifest_with_limits(manifest, 128, 32 * 1024)
}

pub(crate) fn validate_runtime_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
    // 安装清单仍限 128 项；运行目录另受各类数量与 2 MiB 总预算约束。
    validate_manifest_with_limits(manifest, 128 + 1024 + 128 + 8, 256 * 1024)
}

fn validate_manifest_with_limits(
    manifest: &PluginManifest,
    max_contributions: usize,
    max_tool_schema_bytes: usize,
) -> Result<(), PluginError> {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    let validator = VALIDATOR.get_or_init(|| {
        let mut schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contracts/app-server/app-server.schema.json"
        ))
        .expect("repository schema must be valid JSON");
        schema["$ref"] = serde_json::json!("#/$defs/PluginManifest");
        jsonschema::validator_for(&schema).expect("repository plugin schema must compile")
    });
    let serialized = serde_json::to_value(manifest).map_err(|_| invalid("插件清单序列化失败"))?;
    if !validator.is_valid(&serialized) {
        return Err(invalid("插件清单违反版本、字段或大小合同"));
    }
    if manifest.sdk_version != 2 || manifest.data_schema_version < 1 {
        return Err(invalid("仅支持 SDK 2，数据格式版本必须为正整数"));
    }
    if !valid_plugin_id(&manifest.id) {
        return Err(invalid("插件 ID 必须包含有效的作者命名空间"));
    }
    if manifest.version.len() > 64
        || semver::Version::parse(&manifest.version).is_err()
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 128
        || manifest.description.len() > 4096
    {
        return Err(invalid("插件版本、名称或说明无效"));
    }
    validate_extension(manifest)?;
    if let Some(client) = &manifest.contributions.client {
        if !manifest
            .contributions
            .views
            .iter()
            .any(|view| view.entry == client.entry)
            || !manifest.permissions.iter().any(|permission| {
                permission.kind == PluginPermissionKind::Client
                    && permission.targets.as_slice() == ["persistent"]
            })
        {
            return Err(invalid(
                "常驻客户端须引用声明视图入口并申请 client:[persistent] 权限",
            ));
        }
    }
    for permission in &manifest.permissions {
        if permission.kind == PluginPermissionKind::WebProviders
            && (permission.targets.is_empty()
                || permission
                    .targets
                    .iter()
                    .any(|target| !matches!(target.as_str(), "search" | "fetch")))
        {
            return Err(invalid("webProviders 权限只允许 search 与 fetch"));
        }
        if permission.kind == PluginPermissionKind::Client
            && (permission.targets.as_slice() != ["persistent"]
                || manifest.contributions.client.is_none())
        {
            return Err(invalid(
                "client 权限必须为 persistent，且插件须声明常驻客户端",
            ));
        }
        if permission.kind == PluginPermissionKind::Attachments
            && permission.targets.as_slice() != ["images"]
        {
            return Err(invalid("attachments 权限当前只允许 images"));
        }
        if permission.kind == PluginPermissionKind::SessionRead
            && (permission.targets.is_empty()
                || permission
                    .targets
                    .iter()
                    .any(|target| !matches!(target.as_str(), "directory" | "render")))
        {
            return Err(invalid("sessionRead 权限只允许 directory 与 render"));
        }
        if permission.kind == PluginPermissionKind::SessionWrite
            && (permission.targets.is_empty()
                || permission
                    .targets
                    .iter()
                    .any(|target| !matches!(target.as_str(), "context" | "wake")))
        {
            return Err(invalid("sessionWrite 权限只允许 context 与 wake"));
        }
        if permission.kind == PluginPermissionKind::Composer
            && (permission.targets.is_empty()
                || permission
                    .targets
                    .iter()
                    .any(|target| !matches!(target.as_str(), "images" | "text"))
                || manifest.contributions.views.is_empty())
        {
            return Err(invalid(
                "composer 权限只允许 images 与 text，且插件须声明视图",
            ));
        }
    }
    let mut permissions = HashSet::new();
    if manifest.permissions.len() > 64 {
        return Err(invalid("权限声明超限"));
    }
    for permission in &manifest.permissions {
        if permission.targets.len() > 64
            || permission
                .targets
                .iter()
                .any(|t| t.trim().is_empty() || t.len() > 1024)
        {
            return Err(invalid("权限目标无效"));
        }
        let key = serde_json::to_string(permission).map_err(|_| invalid("权限声明无效"))?;
        if !permissions.insert(key) {
            return Err(invalid("权限声明重复"));
        }
    }
    // 通配授权只覆盖显式声明的运行期贡献种类，不扩展模型调用或存储权限。
    for permission in &manifest.permissions {
        if !permission.targets.iter().any(|target| target == "*") {
            continue;
        }
        use magi_app_server_protocol::generated::PluginRuntimeContributionKind as Kind;
        let kind = match permission.kind {
            PluginPermissionKind::Tools => Kind::Tools,
            PluginPermissionKind::ModelSources => Kind::ModelSources,
            _ => return Err(invalid("该权限不支持通配目标")),
        };
        if !manifest
            .extension
            .as_ref()
            .is_some_and(|extension| extension.runtime_contributions.contains(&kind))
        {
            return Err(invalid("通配授权必须对应显式运行期贡献种类"));
        }
    }
    let c = &manifest.contributions;
    let mut ids = HashSet::new();
    let mut check = |id: &str, title: &str, description: &str| -> Result<(), PluginError> {
        if !valid_id(id) || id.len() > 64 {
            return Err(invalid(format!(
                "贡献 ID `{id}` 无效：以小写字母开头，只含小写字母、数字和连字符，最长 64 位"
            )));
        }
        if title.trim().is_empty() || title.len() > 128 || description.len() > 4096 {
            return Err(invalid(format!("贡献 `{id}` 的名称或说明为空或超长")));
        }
        if !ids.insert(id.to_string()) {
            return Err(invalid(format!(
                "贡献 ID `{id}` 重复；所有类型的贡献共用一个命名空间"
            )));
        }
        Ok(())
    };
    for skill in &c.skills {
        check(&skill.id, &skill.title, &skill.description)?;
        if skill.instruction.trim().is_empty() || skill.instruction.len() > 65_536 {
            return Err(invalid("Skill 说明为空或超出上限"));
        }
    }
    // 工具由 Node 扩展提供；每个工具须有同名 `tools` 权限，用户授权后才对模型可见。
    if !c.tools.is_empty() && manifest.extension.is_none() {
        return Err(invalid("声明工具贡献的包必须同时声明 extension"));
    }
    for tool in &c.tools {
        check(&tool.id, &tool.title, "")?;
        if tool.description.len() > MAX_TOOL_DESCRIPTION_BYTES {
            return Err(invalid(format!(
                "工具 `{}` 的说明超过 {MAX_TOOL_DESCRIPTION_BYTES} 字节",
                tool.id
            )));
        }
        validate_object_schema(&tool.input_schema, max_tool_schema_bytes)?;
        if let Some(timeout) = tool.timeout_ms
            && !(1_000..=600_000).contains(&timeout)
        {
            return Err(invalid("工具 timeoutMs 必须在 1000 到 600000 毫秒之间"));
        }
        let permitted = manifest.permissions.iter().any(|permission| {
            permission.kind == PluginPermissionKind::Tools
                && permission
                    .targets
                    .iter()
                    .any(|target| target == &tool.id || target == "*")
        });
        if !permitted {
            return Err(invalid(format!(
                "工具 `{}` 必须有包含它的 tools 权限声明",
                tool.id
            )));
        }
    }
    // 命令由用户在输入框主动调用，执行在 Node 扩展里；授权由 `extension` 完全信任覆盖。
    if !c.commands.is_empty() && manifest.extension.is_none() {
        return Err(invalid("声明命令贡献的包必须同时声明 extension"));
    }
    for command in &c.commands {
        check(&command.id, &command.title, &command.description)?;
    }
    for view in &c.views {
        check(&view.id, &view.title, "")?;
        if view.placements.is_empty()
            || view.placements.len() > 5
            || view
                .placements
                .iter()
                .enumerate()
                .any(|(i, p)| view.placements[..i].contains(p))
        {
            return Err(invalid("视图展示位置无效或重复"));
        }
        // 主页入口只是进入主内容区页面的触发器；没有承载页面的入口无处可开。
        if view.placements.contains(&PluginViewPlacement::Home)
            && !view.placements.contains(&PluginViewPlacement::Main)
        {
            return Err(invalid("主页入口必须同时声明主内容区页面"));
        }
        if view.placements.iter().any(|placement| {
            matches!(
                placement,
                PluginViewPlacement::ComposerLeft | PluginViewPlacement::ChatAssistant
            )
        }) && !manifest.permissions.iter().any(|permission| {
            permission.kind == PluginPermissionKind::SessionRead
                && permission.targets.iter().any(|target| target == "render")
        }) {
            return Err(invalid("会话内视图必须声明 sessionRead:[render] 授权"));
        }
    }
    // 主页卡片是显式声明的嵌入视图；数量受限，避免一个插件占满主页。
    if c.views
        .iter()
        .filter(|view| view.placements.contains(&PluginViewPlacement::HomeCard))
        .count()
        > MAX_HOME_CARDS_PER_PLUGIN
    {
        return Err(invalid(format!(
            "每个插件最多声明 {MAX_HOME_CARDS_PER_PLUGIN} 张主页卡片"
        )));
    }
    for surface in &c.browser_surfaces {
        check(&surface.id, &surface.title, &surface.description)?;
        // 视图不可见时推理继续，要求 guest 保持挂载；否则隐藏就会中断关联运行。
        if surface.background_inference && !surface.keep_mounted {
            return Err(invalid("后台推理要求浏览器 surface 保持挂载"));
        }
        // 挂载、权限与释放由 BrowserAuthority 执行；贡献必须同时申请对应 surface 的 browser 权限。
        let authorized = manifest.permissions.iter().any(|permission| {
            permission.kind == PluginPermissionKind::Browser
                && permission
                    .targets
                    .iter()
                    .any(|target| target == &surface.id)
        });
        if !authorized {
            return Err(invalid(
                "浏览器 surface 必须声明对应 Browser surface 的 browser 权限",
            ));
        }
    }
    // 模型数据源由 Node 扩展在运行时提交，只出现在主对话目录；每项必须有同名 `modelSources` 授权，
    // 因为主对话内容会发送到插件提供的端点。
    if !c.model_sources.is_empty() && manifest.extension.is_none() {
        return Err(invalid("声明模型数据源贡献的包必须同时声明 extension"));
    }
    for source in &c.model_sources {
        check(&source.id, &source.title, &source.description)?;
        let authorized = manifest.permissions.iter().any(|permission| {
            permission.kind == PluginPermissionKind::ModelSources
                && permission
                    .targets
                    .iter()
                    .any(|target| target == &source.id || target == "*")
        });
        if !authorized {
            return Err(invalid(format!(
                "模型数据源 `{}` 必须有包含它的 modelSources 权限声明",
                source.id
            )));
        }
    }
    for item in &c.resources {
        check(&item.id, &item.title, &item.description)?;
    }
    // 只有声明钩子的扩展可不提供 UI/工具贡献；Magi MCP 服务属于内置功能，不经过此入口。
    if ids.len() > max_contributions || (ids.is_empty() && manifest.permissions.is_empty()) {
        return Err(invalid(format!(
            "插件需要至少一项贡献或权限声明，且贡献不超过 {max_contributions} 项"
        )));
    }
    validate_object_schema(&manifest.settings_schema, 32 * 1024)?;
    Ok(())
}

fn validate_object_schema(schema: &serde_json::Value, max_bytes: usize) -> Result<(), PluginError> {
    if !schema.is_object()
        || schema.get("type").and_then(|x| x.as_str()) != Some("object")
        || serde_json::to_vec(schema).map_or(true, |v| v.len() > max_bytes)
    {
        return Err(invalid("配置或工具输入必须声明有界的 object schema"));
    }
    reject_external_references(schema)?;
    jsonschema::validator_for(schema).map_err(|_| invalid("配置或工具输入 schema 无效"))?;
    Ok(())
}

fn reject_external_references(value: &serde_json::Value) -> Result<(), PluginError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && !child.as_str().is_some_and(|s| s.starts_with('#'))
                {
                    return Err(invalid("插件 schema 不得引用外部文件或网络资源"));
                }
                reject_external_references(child)?;
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                reject_external_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 安装与运行期贡献使用同一插件身份合同，不能借贡献 ID 的长度限制裁剪插件身份。
pub fn valid_plugin_id(id: &str) -> bool {
    id.len() <= 96 && id.split('.').count() >= 2 && id.split('.').all(valid_id)
}

fn valid_id(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment.ends_with('.')
                && segment.bytes().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, b'-' | b'_' | b'.' | b'@' | b'+' | b'~')
                })
                && !matches!(
                    segment
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

fn valid_native_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains('\\')
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment.ends_with('.')
                && segment.bytes().all(|c| c.is_ascii_graphic() || c == b' ')
        })
}

const REMOVED_SDK_MESSAGE: &str =
    "该插件使用已移除的 SDK 1（QuickJS 后台）；请改用声明 extension 的 SDK 2（Node 扩展）重新打包";

fn file_limit(name: &str) -> usize {
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if (name.starts_with("extension/") || name.starts_with("ui/"))
        && matches!(extension.as_str(), "js" | "mjs" | "cjs")
    {
        MAX_SCRIPT_BYTES
    } else {
        MAX_FILE_BYTES
    }
}

fn validate_file(name: &str, bytes: &[u8]) -> Result<(), PluginError> {
    if name == "plugin.mjs" {
        return Err(invalid(REMOVED_SDK_MESSAGE));
    }
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let extension = extension.as_str();
    let text = matches!(
        extension,
        "html" | "css" | "js" | "mjs" | "cjs" | "json" | "svg"
    );
    // 原生扩展代码仍为文本；受信原包资源的准入还需核对清单与 full-trust。
    let upstream_asset = name.starts_with("extension/upstream/");
    let extension_code =
        name.starts_with("extension/") && matches!(extension, "js" | "mjs" | "cjs" | "json");
    if name != "manifest.json"
        && name != "extension/index.mjs"
        && !extension_code
        && !upstream_asset
        && (!name.starts_with("ui/")
            || !matches!(
                extension,
                "html"
                    | "css"
                    | "js"
                    | "mjs"
                    | "json"
                    | "svg"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "avif"
                    | "bmp"
                    | "ico"
                    | "woff"
                    | "woff2"
            ))
    {
        return Err(invalid("发布包含未允许的文件类型或入口"));
    }
    if !upstream_asset && executable_payload(bytes) {
        return Err(invalid("发布包不得包含原生程序、字节码或 WASM 后台组件"));
    }
    if text && std::str::from_utf8(bytes).is_err() {
        return Err(invalid("插件文本资源必须使用 UTF-8"));
    }
    Ok(())
}

fn executable_payload(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"\0asm")
        || bytes.starts_with(b"\xfe\xed\xfa")
        || bytes.starts_with(b"\xcf\xfa\xed\xfe")
        || bytes.starts_with(b"\xca\xfe\xba\xbe")
}

/// 图标体积上限。图标只用于列表和标题展示，不需要大图。
pub const MAX_ICON_BYTES: usize = 256 * 1024;

pub fn icon_content_type(path: &str) -> Option<&'static str> {
    magi_media::image_content_type(path)
}

/// 图标只增加自身的体积约束，图片格式和内容准入与其他 GUI 图片共用。
pub fn validate_icon(path: &str, bytes: &[u8]) -> Result<(), PluginError> {
    if bytes.len() > MAX_ICON_BYTES {
        return Err(invalid("图标超过 256 KiB 上限"));
    }
    magi_media::validate_image(path, bytes).map_err(invalid)?;
    Ok(())
}

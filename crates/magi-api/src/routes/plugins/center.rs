//! 插件中心：目录读取、来源信任判定、中心安装与作者指南。
//!
//! 目录只是发现入口：安装仍走与本地包相同的校验、授权和生命周期（见 `super::install` 等）。

use super::*;

pub(super) const CENTER_SETTINGS_SECTION: &str = "pluginCenter";

/// Magi 官方插件中心：随 Magi 仓库发布在 `plugin-center/`，经 GitHub 提供。默认地址指向它，
/// 用户不需要任何配置就能浏览官方插件。开发和验收可用环境变量覆盖（空值表示不提供默认地址）。
pub(super) const OFFICIAL_PLUGIN_CENTER_URL: &str =
    "https://raw.githubusercontent.com/MistRipple/magi-code/main/plugin-center/catalog.json";
pub(super) const OFFICIAL_PLUGIN_CENTER_ENV: &str = "MAGI_PLUGIN_CENTER_DEFAULT_URL";

pub(super) fn official_center_url() -> String {
    match std::env::var(OFFICIAL_PLUGIN_CENTER_ENV) {
        Ok(value) => value.trim().to_string(),
        Err(_) => OFFICIAL_PLUGIN_CENTER_URL.to_string(),
    }
}

/// 目录地址是否就是官方插件中心（比较规范化后的地址）。
pub(super) fn is_official_center(url: &str) -> bool {
    let official = official_center_url();
    !official.is_empty() && validated_remote_url(&official).is_ok_and(|official| official == url)
}

/// 从未配置过（或保存时清空地址与公钥，即恢复默认）时使用官方地址。
pub(super) fn read_center_config(state: &ApiState) -> PluginCenterConfig {
    let stored = state.settings_store.get_section(CENTER_SETTINGS_SECTION);
    if stored.is_null() {
        return PluginCenterConfig {
            url: official_center_url(),
            trusted_keys: Vec::new(),
        };
    }
    serde_json::from_value(stored).unwrap_or(PluginCenterConfig {
        url: String::new(),
        trusted_keys: Vec::new(),
    })
}

/// 下载并解析目录，返回目录与“来源是否可信”。配置了受信公钥时验签失败不返回任何条目；
/// 没有配置公钥时接受普通目录：来自官方地址的目录可信（信任来自 Magi 仓库本身），
/// 其余按指定地址的信任级别对待。目录里的相对包地址按目录自身地址解析。
pub(super) async fn fetch_catalog(
    config: &PluginCenterConfig,
) -> Result<(PluginCatalog, bool), ApiError> {
    if config.url.trim().is_empty() {
        return Err(ApiError::InvalidInput("尚未配置插件中心地址".into()));
    }
    let url = validated_remote_url(&config.url)?;
    let bytes = download_bytes(&url, MAX_CATALOG_BYTES, "插件中心目录").await?;
    let trusted = config
        .trusted_keys
        .iter()
        .map(|key| TrustedKey {
            key_id: key.key_id.clone(),
            public_key: key.public_key.clone(),
        })
        .collect::<Vec<_>>();
    let parsed = PluginCatalog::parse(&bytes, &trusted)
        .map_err(|error| ApiError::Forbidden(error.to_string()))?;
    let mut catalog = parsed.catalog;
    let base =
        reqwest::Url::parse(&url).map_err(|_| ApiError::InvalidInput("插件中心地址无效".into()))?;
    for entry in &mut catalog.plugins {
        let resolved = base
            .join(&entry.url)
            .map_err(|_| ApiError::InvalidInput(format!("插件 {} 的包地址无效", entry.id)))?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return Err(ApiError::InvalidInput(format!(
                "插件 {} 的包地址只支持 HTTP(S)",
                entry.id
            )));
        }
        entry.url = resolved.to_string();
    }
    let official = config.trusted_keys.is_empty() && is_official_center(&url);
    Ok((catalog, parsed.signed || official))
}

pub(super) async fn center(
    State(state): State<ApiState>,
) -> Result<Json<PluginCenterProjection>, ApiError> {
    let config = read_center_config(&state);
    if config.url.trim().is_empty() {
        return Ok(Json(PluginCenterProjection {
            config,
            entries: Vec::new(),
            verified: false,
            error: None,
        }));
    }
    let (entries, verified, error) = match fetch_catalog(&config).await {
        Ok((catalog, verified)) => {
            let manager = locked(&state, "读取插件中心")?;
            (center_entries(&manager, &catalog), verified, None)
        }
        // 目录不可用不是服务故障：界面需要展示原因，并仍能管理已安装插件。
        Err(error) => (Vec::new(), false, Some(error.message().to_string())),
    };
    Ok(Json(PluginCenterProjection {
        config,
        entries,
        verified,
        error,
    }))
}

/// 每个插件只展示目录中的最新版本，并对照已安装版本给出可执行的动作状态。
pub(super) fn center_entries(
    manager: &PluginManager,
    catalog: &PluginCatalog,
) -> Vec<PluginCenterEntry> {
    let mut ids = catalog
        .plugins
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| catalog.latest(id))
        .map(|entry| {
            let installed = manager
                .state()
                .plugins
                .get(&entry.id)
                .map(|p| p.version.clone());
            let state = match installed.as_deref() {
                None => PluginCenterEntryState::Available,
                Some(version) if version == entry.version => PluginCenterEntryState::Installed,
                Some(_) => PluginCenterEntryState::Updatable,
            };
            PluginCenterEntry {
                id: entry.id.clone(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                version: entry.version.clone(),
                size: entry.size,
                state,
                installed_version: installed,
            }
        })
        .collect()
}

pub(super) async fn write_center_config(
    State(state): State<ApiState>,
    Json(config): Json<PluginCenterConfig>,
) -> Result<Json<PluginCenterConfig>, ApiError> {
    use base64::Engine as _;
    let url = config.url.trim();
    let url = if url.is_empty() {
        String::new()
    } else {
        validated_remote_url(url)?
    };
    let mut seen = std::collections::HashSet::new();
    let mut trusted_keys = Vec::new();
    for key in &config.trusted_keys {
        let key_id = key.key_id.trim().to_string();
        let public_key = key.public_key.trim().to_string();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&public_key)
            .map_err(|_| ApiError::InvalidInput("公钥必须是 Base64 编码".into()))?;
        if key_id.is_empty() || decoded.len() != 32 || !seen.insert(key_id.clone()) {
            return Err(ApiError::InvalidInput(
                "公钥必须是唯一标识下的 32 字节 Ed25519 公钥".into(),
            ));
        }
        trusted_keys.push(PluginCenterKey { key_id, public_key });
    }
    // 地址与公钥都留空表示“恢复默认”：清除已保存的配置，回到官方插件中心。
    if url.is_empty() && trusted_keys.is_empty() {
        state
            .settings_store
            .remove_section(CENTER_SETTINGS_SECTION)
            .map_err(|error| ApiError::internal_assembly("清除插件中心配置失败", error))?;
        return Ok(Json(read_center_config(&state)));
    }
    let config = PluginCenterConfig { url, trusted_keys };
    state
        .settings_store
        .set_section(
            CENTER_SETTINGS_SECTION,
            serde_json::to_value(&config)
                .map_err(|error| ApiError::internal_assembly("序列化插件中心配置失败", error))?,
        )
        .map_err(|error| ApiError::internal_assembly("保存插件中心配置失败", error))?;
    Ok(Json(config))
}

pub(super) async fn center_install(
    State(state): State<ApiState>,
    Json(request): Json<PluginCenterInstallRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let (catalog, verified) = fetch_catalog(&read_center_config(&state)).await?;
    let entry: &CatalogEntry = match request.version.as_deref().filter(|v| !v.trim().is_empty()) {
        Some(version) => catalog
            .plugins
            .iter()
            .find(|entry| entry.id == request.id && entry.version == version),
        None => catalog.latest(&request.id),
    }
    .ok_or_else(|| ApiError::NotFound("插件中心目录中没有该插件或版本".into()))?;
    let bytes = download_bytes(&entry.url, entry.size as usize, "插件包").await?;
    let package = PluginPackage::from_archive(&bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    // 下载内容必须就是目录签名所绑定的那份包。
    entry
        .check_package(&package)
        .map_err(|error| ApiError::Forbidden(error.to_string()))?;
    // 只有通过受信公钥验签、或来自官方插件中心地址的目录才算插件中心来源；其余目录里的包
    // 按指定地址对待，界面上与插件中心来源区分标识，不继承“已验证”的含义。
    let source = if verified {
        PluginSource::Center {
            url: entry.url.clone(),
        }
    } else {
        PluginSource::Address {
            url: entry.url.clone(),
        }
    };
    install_or_upgrade(&state, &package, source).await?;
    snapshot(&*locked(&state, "读取插件安装结果")?)
}

/// 作者指南随 daemon 发布，与仓库文档是同一份内容；插件中心在界面内直接展示，
/// 用户不需要知道仓库在哪里就能开始开发。
pub(super) const PLUGIN_AUTHORING_GUIDE: &str =
    include_str!("../../../../../docs/plugin-authoring.md");

pub(super) async fn guide() -> Response {
    let mut response = Response::new(axum::body::Body::from(PLUGIN_AUTHORING_GUIDE));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    response
}

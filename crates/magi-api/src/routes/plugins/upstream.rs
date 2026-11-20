//! Discover published packages and forward original package actions to the DSH platform.

use super::*;
use crate::native_plugins::InstalledUpstreamPlugins;
use magi_app_server_protocol::generated::{
    PluginEcosystem, PluginUpstreamEntry, PluginUpstreamEntryState, PluginUpstreamPreview,
    PluginUpstreamProjection, PluginUpstreamRequest,
};
use magi_plugin_system::npm;
use std::path::Path as FsPath;

mod artifacts;
pub use artifacts::{
    NpmArchive, fetch_npm_archive, fetch_npm_packument, fetch_npm_release_archive,
};
pub(super) mod icons;

/// registry 的开发与验收覆盖入口；默认使用 npm 官方 registry。
pub(super) const NPM_REGISTRY_ENV: &str = "MAGI_NPM_REGISTRY";
const DEFAULT_NPM_REGISTRY: &str = "https://registry.npmjs.org";
const MAX_REGISTRY_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_PAGE_SIZE: usize = 20;
/// 发布在这些作用域下的包属于上游官方发布者。这只说明“谁发布的”，不说明包适配 Magi 或经过 Magi 审核。
const OFFICIAL_SCOPES: &[&str] = &["@deepseek-ai/", "@earendil-works/"];

pub(super) fn registry() -> Result<String, ApiError> {
    let value = std::env::var(NPM_REGISTRY_ENV).unwrap_or_else(|_| DEFAULT_NPM_REGISTRY.into());
    Ok(validated_remote_url(value.trim())?
        .trim_end_matches('/')
        .to_string())
}

/// 上游生态的协议名与发现关键词。
struct EcosystemSpec {
    name: &'static str,
    search_keyword: &'static str,
}

fn ecosystem_spec(ecosystem: &PluginEcosystem) -> EcosystemSpec {
    match ecosystem {
        PluginEcosystem::Dsh => EcosystemSpec {
            name: "dsh",
            search_keyword: "dsh-plugin",
        },
        PluginEcosystem::Pi => EcosystemSpec {
            name: "pi",
            search_keyword: "pi-package",
        },
    }
}

pub(super) fn ecosystem_name(ecosystem: &PluginEcosystem) -> &'static str {
    ecosystem_spec(ecosystem).name
}

/// 本机能否安装该生态的包。返回不能的原因，界面原样展示。
fn blocked_reason(state: &ApiState, ecosystem: &PluginEcosystem) -> Option<String> {
    match ecosystem {
        PluginEcosystem::Dsh if state.native_plugin_platform.is_some() => None,
        PluginEcosystem::Dsh => Some("DSH 插件宿主尚未装配".into()),
        PluginEcosystem::Pi => Some("pi 插件宿主尚未提供，当前只能浏览".into()),
    }
}

/// npm 包名：可选作用域加名称，只含 npm 允许的小写字符。拒绝任何带路径、查询或空白的输入。
pub(super) fn validate_package_name(name: &str) -> Result<(), ApiError> {
    if npm::is_valid_package_name(name) {
        Ok(())
    } else {
        Err(ApiError::InvalidInput("npm 包名无效".into()))
    }
}

fn package_path(name: &str) -> String {
    // 作用域包的斜杠按 registry 的约定保持原样。
    name.to_string()
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    #[serde(default)]
    q: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    objects: Vec<SearchObject>,
}

#[derive(Deserialize)]
struct SearchObject {
    package: SearchPackage,
}

#[derive(Deserialize)]
struct SearchPackage {
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    publisher: Option<SearchPublisher>,
}

#[derive(Deserialize)]
struct SearchPublisher {
    username: String,
}

impl From<VersionDocument> for SearchPackage {
    fn from(document: VersionDocument) -> Self {
        Self {
            name: document.name,
            version: document.version,
            description: document.description,
            publisher: document.npm_user.map(|user| SearchPublisher {
                username: user.name,
            }),
        }
    }
}

/// 搜索上游生态的 npm 包（需要关键词）。结果只是发现线索：条目是否可安装、是否官方发布者都按事实标注。
pub(super) async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<PluginUpstreamProjection>, ApiError> {
    let registry = registry()?;
    let installed = installed_upstream(&state)?;
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    let text = query.q.trim();
    if !text.is_empty() {
        let exact_dsh = async {
            let name = text.to_ascii_lowercase();
            validate_package_name(&name).ok()?;
            let document = artifacts::registry_document(&registry, &name, None)
                .await
                .ok()?;
            document
                .dsh
                .get("bundle")
                .is_some_and(Value::is_object)
                .then_some(document)
        };
        let (dsh, pi, exact) = tokio::join!(
            search_ecosystem(&registry, &PluginEcosystem::Dsh, text),
            search_ecosystem(&registry, &PluginEcosystem::Pi, text),
            exact_dsh,
        );
        let exact_name = exact.as_ref().map(|document| document.name.clone());
        if let Some(document) = exact {
            let ecosystem = PluginEcosystem::Dsh;
            entries.push(entry_from_search(
                document.into(),
                &ecosystem,
                &blocked_reason(&state, &ecosystem),
                &installed,
            ));
        }
        for (ecosystem, result) in [(PluginEcosystem::Dsh, dsh), (PluginEcosystem::Pi, pi)] {
            match result {
                Ok(found) => {
                    let reason = blocked_reason(&state, &ecosystem);
                    entries.extend(
                        found
                            .into_iter()
                            .filter(|package| {
                                ecosystem != PluginEcosystem::Dsh
                                    || exact_name.as_deref() != Some(&package.name)
                            })
                            .map(|package| {
                                entry_from_search(package, &ecosystem, &reason, &installed)
                            }),
                    );
                }
                Err(error) => errors.push(format!(
                    "{}：{}",
                    ecosystem_name(&ecosystem),
                    error.message()
                )),
            }
        }
    }
    // 已安装的上游包即使不在本次搜索结果里，也要查它的最新版本，否则无法提示升级。
    for (eco, package) in installed.keys() {
        let ecosystem = match eco.as_str() {
            "dsh" => PluginEcosystem::Dsh,
            "pi" => PluginEcosystem::Pi,
            // 未知生态不是 DSH，不能借 DSH 的更新检查展示；该记录只可在已安装列表管理。
            _ => continue,
        };
        if entries
            .iter()
            .any(|entry| entry.package == *package && ecosystem_name(&entry.ecosystem) == eco)
        {
            continue;
        }
        let Ok(document) = artifacts::registry_document(&registry, package, None).await else {
            continue;
        };
        let reason = blocked_reason(&state, &ecosystem);
        entries.push(entry_from_search(
            document.into(),
            &ecosystem,
            &reason,
            &installed,
        ));
    }
    // 保留 registry 的相关度顺序；发布者身份与名称不能覆盖用户的搜索匹配度。
    Ok(Json(PluginUpstreamProjection {
        registry,
        entries,
        error: (!errors.is_empty()).then(|| errors.join("；")),
    }))
}

async fn search_ecosystem(
    registry: &str,
    ecosystem: &PluginEcosystem,
    text: &str,
) -> Result<Vec<SearchPackage>, ApiError> {
    let mut url = reqwest::Url::parse(&format!("{registry}/-/v1/search"))
        .map_err(|_| ApiError::InvalidInput("registry 地址无效".into()))?;
    let mut terms = format!("keywords:{}", ecosystem_spec(ecosystem).search_keyword);
    if !text.is_empty() {
        terms.push(' ');
        terms.push_str(text);
    }
    url.query_pairs_mut()
        .append_pair("text", &terms)
        .append_pair("size", &SEARCH_PAGE_SIZE.to_string());
    let bytes = download_bytes(
        url.as_str(),
        MAX_REGISTRY_DOCUMENT_BYTES,
        "上游插件搜索结果",
    )
    .await?;
    let parsed: SearchResponse = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidInput("registry 搜索结果格式无效".into()))?;
    Ok(parsed
        .objects
        .into_iter()
        .map(|object| object.package)
        .collect())
}

/// 已安装的上游来源包：(生态, 包名) → (插件 ID, 已安装版本)。
fn installed_upstream(state: &ApiState) -> Result<InstalledUpstreamPlugins, ApiError> {
    let manager = locked(state, "读取已安装的上游插件")?;
    native_platform(state)?.installed_upstream(&manager)
}

fn entry_from_search(
    package: SearchPackage,
    ecosystem: &PluginEcosystem,
    reason: &Option<String>,
    installed: &InstalledUpstreamPlugins,
) -> PluginUpstreamEntry {
    let key = (ecosystem_name(ecosystem).to_string(), package.name.clone());
    let current = installed.get(&key);
    PluginUpstreamEntry {
        ecosystem: ecosystem.clone(),
        official: OFFICIAL_SCOPES
            .iter()
            .any(|scope| package.name.starts_with(scope)),
        publisher: package
            .publisher
            .map(|publisher| publisher.username)
            .unwrap_or_default(),
        name: package.name,
        description: package.description,
        state: upstream_state(
            current.map(|(_, version)| version.as_str()),
            &package.version,
        ),
        installed_plugin_id: current.map(|(id, _)| id.clone()),
        installed_version: current.map(|(_, version)| version.clone()),
        version: package.version,
        installable: reason.is_none(),
        blocked_reason: reason.clone(),
        package: key.1,
    }
}

/// 只有 registry 版本在语义版本上更新时才是可升级；无法比较时按已安装处理，不制造升级提示。
pub(super) fn upstream_state(installed: Option<&str>, available: &str) -> PluginUpstreamEntryState {
    let Some(installed) = installed else {
        return PluginUpstreamEntryState::Available;
    };
    match (
        semver::Version::parse(installed),
        semver::Version::parse(available),
    ) {
        (Ok(installed), Ok(available)) if available > installed => {
            PluginUpstreamEntryState::Updatable
        }
        _ => PluginUpstreamEntryState::Installed,
    }
}

#[derive(Deserialize)]
pub(super) struct VersionDocument {
    pub(super) name: String,
    pub(super) version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    icon: Value,
    #[serde(default)]
    dsh: Value,
    dist: Dist,
    #[serde(default, rename = "_npmUser")]
    npm_user: Option<NpmUser>,
}

#[derive(Deserialize)]
struct Dist {
    tarball: String,
    integrity: Option<String>,
}

#[derive(Deserialize)]
struct NpmUser {
    name: String,
}

pub(super) async fn preview(
    State(state): State<ApiState>,
    Json(request): Json<PluginUpstreamRequest>,
) -> Result<Json<PluginUpstreamPreview>, ApiError> {
    native_platform(&state)?
        .preview_upstream(request)
        .await
        .map(Json)
}

pub(super) async fn install(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<PluginUpstreamRequest>,
) -> Result<Response, ApiError> {
    let outcome = native_platform(&state)?
        .install_upstream(request, install_request_id(&headers)?)
        .await?;
    if matches!(
        outcome,
        crate::native_plugins::PluginInstallOutcome::Cancelled
    ) {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    publish_plugin_registry_changed(&state);
    Ok(snapshot(&state, &*locked(&state, "读取插件安装结果")?)?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_reject_paths_queries_and_uppercase() {
        for ok in ["dsh-connect-workbuddy", "@deepseek-ai/dsh-llm", "a.b_c~d"] {
            assert!(validate_package_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "../x",
            "a/b",
            "@scope",
            "@scope/",
            "@/name",
            "Name",
            "a b",
            "a?x=1",
            ".hidden",
            "_private",
            "@a/b/c",
            &"x".repeat(215),
        ] {
            assert!(validate_package_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_semver_newer_registry_versions_are_updates() {
        use PluginUpstreamEntryState::*;
        assert_eq!(upstream_state(None, "1.0.0"), Available);
        assert_eq!(upstream_state(Some("1.0.0"), "1.0.0"), Installed);
        assert_eq!(upstream_state(Some("1.0.0"), "1.0.1"), Updatable);
        assert_eq!(upstream_state(Some("1.2.0"), "1.1.9"), Installed);
        assert_eq!(upstream_state(Some("1.0.0"), "not-semver"), Installed);
    }
}

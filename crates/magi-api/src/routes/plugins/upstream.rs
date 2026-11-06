//! 上游生态插件（DSH、pi）：搜索、预览与安装。
//!
//! 用户安装的是上游官方发布的 npm 包本身。这里只做三件事：向 npm registry 取包并核对
//! `dist.integrity`；调用随包的翻译器（`compat/cli.mjs prepare`）把它翻译成标准 Magi 包；
//! 把翻译结果交给与其他来源完全相同的预览、授权和生命周期。安装期不运行 npm、git 或上游脚本，
//! 上游代码只在翻译器的受限子进程里被执行一次以记录它声明了什么。
//!
//! 翻译是确定的：同一上游版本和同一兼容运行时得到同一摘要，所以预览确认的摘要可以在安装时
//! 重新翻译后核对，不需要在服务端缓存产物。

use super::*;
use magi_app_server_protocol::generated::{
    PluginEcosystem, PluginTranslationNote, PluginTranslationSummary, PluginUpstreamEntry,
    PluginUpstreamEntryState, PluginUpstreamPreview, PluginUpstreamProjection,
    PluginUpstreamRequest,
};
use magi_plugin_system::npm;
use std::path::Path as FsPath;

pub(super) mod icons;

/// registry 的开发与验收覆盖入口；默认使用 npm 官方 registry。
pub(super) const NPM_REGISTRY_ENV: &str = "MAGI_NPM_REGISTRY";
const DEFAULT_NPM_REGISTRY: &str = "https://registry.npmjs.org";
const MAX_REGISTRY_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_PAGE_SIZE: usize = 20;
const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(180);
/// 同一时间只翻译一个包：翻译会在子进程里执行上游代码，并发只会放大资源占用。
static TRANSLATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 发布在这些作用域下的包属于上游官方发布者。这只说明“谁发布的”，不说明包适配 Magi 或经过 Magi 审核。
const OFFICIAL_SCOPES: &[&str] = &["@deepseek-ai/", "@earendil-works/"];

fn registry() -> Result<String, ApiError> {
    let value = std::env::var(NPM_REGISTRY_ENV).unwrap_or_else(|_| DEFAULT_NPM_REGISTRY.into());
    Ok(validated_remote_url(value.trim())?
        .trim_end_matches('/')
        .to_string())
}

fn ecosystem_keyword(ecosystem: &PluginEcosystem) -> &'static str {
    match ecosystem {
        PluginEcosystem::Dsh => "dsh-plugin",
        PluginEcosystem::Pi => "pi-package",
    }
}

fn ecosystem_name(ecosystem: &PluginEcosystem) -> &'static str {
    match ecosystem {
        PluginEcosystem::Dsh => "dsh",
        PluginEcosystem::Pi => "pi",
    }
}

/// 本机能否安装该生态的包。返回不能的原因，界面原样展示。
fn blocked_reason(state: &ApiState, ecosystem: &PluginEcosystem) -> Option<String> {
    if !matches!(ecosystem, PluginEcosystem::Dsh) {
        return Some("pi 翻译器尚未提供，当前只能浏览".into());
    }
    if state.plugin_toolchain.is_none() {
        return Some("本机缺少随包 Node 或兼容运行时，不能翻译上游插件".into());
    }
    None
}

/// npm 包名：可选作用域加名称，只含 npm 允许的小写字符。拒绝任何带路径、查询或空白的输入。
pub(super) fn validate_package_name(name: &str) -> Result<(), ApiError> {
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && !part.starts_with('_')
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c))
    };
    let ok = name.len() <= 214
        && match name.strip_prefix('@') {
            Some(scoped) => scoped
                .split_once('/')
                .is_some_and(|(scope, rest)| valid(scope) && valid(rest)),
            None => valid(name),
        };
    if ok {
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

/// 搜索上游生态的 npm 包（需要关键词）。结果只是发现线索：条目是否可安装、是否官方发布者都按事实标注。
pub(super) async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<PluginUpstreamProjection>, ApiError> {
    let registry = registry()?;
    let installed = installed_upstream(&state)?;
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for ecosystem in [PluginEcosystem::Dsh, PluginEcosystem::Pi] {
        // 空查询时 registry 会返回按下载量排序的成千上万个包，不是有意义的浏览列表：
        // 只在用户输入关键词后搜索；已安装的上游包仍会在下面检查更新。
        if query.q.trim().is_empty() {
            break;
        }
        match search_ecosystem(&registry, &ecosystem, query.q.trim()).await {
            Ok(found) => {
                let reason = blocked_reason(&state, &ecosystem);
                for package in found {
                    entries.push(entry_from_search(package, &ecosystem, &reason, &installed));
                }
            }
            // 一个生态的搜索失败不应遮住另一个生态的结果。
            Err(error) => errors.push(format!(
                "{}：{}",
                ecosystem_name(&ecosystem),
                error.message()
            )),
        }
    }
    // 已安装的上游包即使不在本次搜索结果里，也要查它的最新版本，否则无法提示升级。
    for ((eco, package), (plugin_id, version)) in &installed {
        let ecosystem = if eco == "pi" {
            PluginEcosystem::Pi
        } else {
            PluginEcosystem::Dsh
        };
        if entries
            .iter()
            .any(|entry| entry.package == *package && ecosystem_name(&entry.ecosystem) == eco)
        {
            continue;
        }
        let url = format!("{registry}/{}/latest", package_path(package));
        let Ok(bytes) = download_bytes(&url, MAX_REGISTRY_DOCUMENT_BYTES, "上游插件元数据").await
        else {
            continue;
        };
        let Ok(document) = serde_json::from_slice::<VersionDocument>(&bytes) else {
            continue;
        };
        let reason = blocked_reason(&state, &ecosystem);
        entries.push(PluginUpstreamEntry {
            official: OFFICIAL_SCOPES
                .iter()
                .any(|scope| package.starts_with(scope)),
            publisher: document
                .npm_user
                .as_ref()
                .map(|user| user.name.clone())
                .unwrap_or_default(),
            name: package.clone(),
            description: document.description.clone(),
            state: upstream_state(Some(version), &document.version),
            installed_plugin_id: Some(plugin_id.clone()),
            installed_version: Some(version.clone()),
            version: document.version,
            installable: reason.is_none(),
            blocked_reason: reason,
            package: package.clone(),
            ecosystem,
        });
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
    let mut terms = format!("keywords:{}", ecosystem_keyword(ecosystem));
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
fn installed_upstream(
    state: &ApiState,
) -> Result<std::collections::HashMap<(String, String), (String, String)>, ApiError> {
    let manager = locked(state, "读取已安装的上游插件")?;
    Ok(manager
        .state()
        .plugins
        .values()
        .filter_map(|plugin| match &plugin.source {
            PluginSource::Upstream {
                ecosystem,
                package,
                version,
            } => Some((
                (ecosystem.clone(), package.clone()),
                (plugin.id.clone(), version.clone()),
            )),
            _ => None,
        })
        .collect())
}

fn entry_from_search(
    package: SearchPackage,
    ecosystem: &PluginEcosystem,
    reason: &Option<String>,
    installed: &std::collections::HashMap<(String, String), (String, String)>,
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
struct VersionDocument {
    name: String,
    version: String,
    #[serde(default)]
    description: String,
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

/// 一次翻译的结果：已校验的 Magi 包，加上给用户看的翻译摘要。
struct Translated {
    package: PluginPackage,
    document: VersionDocument,
    summary: PluginTranslationSummary,
}

pub(super) async fn preview(
    State(state): State<ApiState>,
    Json(request): Json<PluginUpstreamRequest>,
) -> Result<Json<PluginUpstreamPreview>, ApiError> {
    let translated = translate(&state, &request).await?;
    let source = upstream_source(&request.ecosystem, &translated.document);
    let candidate = {
        let manager = locked(&state, "预览上游插件")?;
        manager
            .preview(&translated.package, &source)
            .map_err(plugin_error)?
    };
    Ok(Json(PluginUpstreamPreview {
        candidate,
        official: OFFICIAL_SCOPES
            .iter()
            .any(|scope| translated.document.name.starts_with(scope)),
        publisher: translated
            .document
            .npm_user
            .as_ref()
            .map(|user| user.name.clone())
            .unwrap_or_default(),
        integrity: translated
            .document
            .dist
            .integrity
            .clone()
            .unwrap_or_default(),
        package: translated.document.name,
        version: translated.document.version,
        translation: translated.summary,
    }))
}

pub(super) async fn install(
    State(state): State<ApiState>,
    Json(request): Json<PluginUpstreamRequest>,
) -> Result<Json<magi_app_server_protocol::PluginList>, ApiError> {
    let expected = request
        .expected_digest
        .as_deref()
        .filter(|digest| !digest.trim().is_empty())
        .ok_or_else(|| ApiError::InvalidInput("安装前必须先预览并携带预览摘要".into()))?;
    if request
        .version
        .as_deref()
        .is_none_or(|version| version.trim().is_empty())
    {
        return Err(ApiError::InvalidInput(
            "安装必须指定预览时的精确版本".into(),
        ));
    }
    let translated = translate(&state, &request).await?;
    // 预览确认的是具体摘要：上游内容或兼容运行时在确认后变化时必须拒绝。
    if translated.package.digest() != expected {
        return Err(ApiError::Conflict(
            "翻译结果与预览时不一致，请重新预览后再确认".into(),
        ));
    }
    let source = upstream_source(&request.ecosystem, &translated.document);
    install_or_upgrade(&state, &translated.package, source).await?;
    snapshot(&state, &*locked(&state, "读取插件安装结果")?)
}

fn upstream_source(ecosystem: &PluginEcosystem, document: &VersionDocument) -> PluginSource {
    PluginSource::Upstream {
        ecosystem: ecosystem_name(ecosystem).into(),
        package: document.name.clone(),
        version: document.version.clone(),
    }
}

async fn translate(
    state: &ApiState,
    request: &PluginUpstreamRequest,
) -> Result<Translated, ApiError> {
    if let Some(reason) = blocked_reason(state, &request.ecosystem) {
        return Err(ApiError::InvalidInput(reason));
    }
    let toolchain = state
        .plugin_toolchain
        .clone()
        .ok_or_else(|| ApiError::InvalidInput("本机不能翻译上游插件".into()))?;
    validate_package_name(&request.package)?;
    let version = match request.version.as_deref().map(str::trim) {
        None | Some("") => "latest".to_string(),
        Some(version) => {
            semver::Version::parse(version)
                .map_err(|_| ApiError::InvalidInput("版本必须是精确的语义版本".into()))?;
            version.to_string()
        }
    };
    let registry = registry()?;
    let document_url = format!("{registry}/{}/{version}", package_path(&request.package));
    let bytes =
        download_bytes(&document_url, MAX_REGISTRY_DOCUMENT_BYTES, "上游插件元数据").await?;
    let document: VersionDocument = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidInput("registry 元数据格式无效".into()))?;
    if document.name != request.package {
        return Err(ApiError::InvalidInput(
            "registry 返回的包名与请求不一致".into(),
        ));
    }
    let integrity =
        document.dist.integrity.clone().ok_or_else(|| {
            ApiError::InvalidInput("该版本没有 sha512 完整性摘要，拒绝安装".into())
        })?;
    let tarball =
        download_bytes(&document.dist.tarball, npm::MAX_TARBALL_BYTES, "上游插件包").await?;
    npm::verify_integrity(&tarball, &integrity)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;

    let _serial = TRANSLATE_LOCK.lock().await;
    let work = tempfile::tempdir()
        .map_err(|error| ApiError::internal_assembly("创建翻译工作目录失败", error))?;
    let upstream_dir = work.path().join("upstream");
    std::fs::create_dir(&upstream_dir)
        .map_err(|error| ApiError::internal_assembly("创建翻译工作目录失败", error))?;
    {
        let target = upstream_dir.clone();
        tokio::task::spawn_blocking(move || npm::unpack_tarball(&tarball, &target))
            .await
            .map_err(|error| ApiError::internal_assembly("解包上游插件失败", error))?
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    }
    let extracted: Value = serde_json::from_slice(&std::fs::read(upstream_dir.join("package.json"))
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    if extracted["name"].as_str() != Some(document.name.as_str()) || extracted["version"].as_str() != Some(document.version.as_str()) {
        return Err(ApiError::InvalidInput("发布包的名称或版本与 registry 元数据不一致".into()));
    }
    let out_dir = work.path().join("package");
    let archive_path = work.path().join("package.zip");
    let stdout = run_translator(
        state,
        &toolchain,
        &request.ecosystem,
        &upstream_dir,
        &out_dir,
        &archive_path,
        &integrity,
    )
    .await?;
    let archive = std::fs::read(&archive_path)
        .map_err(|error| ApiError::internal_assembly("读取翻译结果失败", error))?;
    let package = PluginPackage::from_archive(&archive)
        .map_err(|error| ApiError::InvalidInput(format!("翻译结果不是有效的 Magi 包：{error}")))?;
    Ok(Translated {
        summary: summarize(&stdout),
        package,
        document,
    })
}

async fn run_translator(
    state: &ApiState,
    toolchain: &crate::state::PluginToolchain,
    ecosystem: &PluginEcosystem,
    upstream_dir: &FsPath,
    out_dir: &FsPath,
    archive: &FsPath,
    integrity: &str,
) -> Result<String, ApiError> {
    use tokio::io::AsyncReadExt;
    let mut command = magi_process::tokio_command(&toolchain.node);
    command
        .arg(toolchain.compat.join("cli.mjs"))
        .arg("prepare")
        .arg(ecosystem_name(ecosystem))
        .arg(upstream_dir)
        .arg("--out")
        .arg(out_dir)
        .arg("--integrity")
        .arg(integrity)
        .arg("--archive")
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child =
        magi_process::spawn_managed_tokio_in_group(state.managed_process_group(), &mut command)
            .map_err(|error| ApiError::internal_assembly("启动翻译器失败", error))?;
    let mut out = child.take_stdout();
    let mut err = child.take_stderr();
    let collect = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let read_out = async {
            if let Some(out) = out.as_mut() {
                let _ = out.take(8 * 1024 * 1024).read_to_end(&mut stdout).await;
            }
        };
        let read_err = async {
            if let Some(err) = err.as_mut() {
                let _ = err.take(64 * 1024).read_to_end(&mut stderr).await;
            }
        };
        tokio::join!(read_out, read_err);
        let status = child.wait().await;
        (status, stdout, stderr)
    };
    let (status, stdout, stderr) = match tokio::time::timeout(TRANSLATE_TIMEOUT, collect).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.terminate().await;
            return Err(ApiError::InvalidInput("翻译上游插件超时".into()));
        }
    };
    let status = status.map_err(|error| ApiError::internal_assembly("等待翻译器失败", error))?;
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr);
        let message = message.trim();
        let message: String = message.chars().take(2000).collect();
        return Err(ApiError::InvalidInput(if message.is_empty() {
            "翻译器失败，没有返回原因".into()
        } else {
            message
        }));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// 把翻译器的报告整理成给用户看的摘要。报告缺字段时按空处理：摘要只是展示，
/// 授权判断只依据清单与权限差异。
pub(super) fn summarize(stdout: &str) -> PluginTranslationSummary {
    let value: Value = serde_json::from_str(stdout).unwrap_or(Value::Null);
    let report = &value["report"];
    let names = |items: &Value, key: &str| -> Vec<String> {
        items
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item[key].as_str().map(str::to_string))
            .collect()
    };
    let strings = |items: &Value| -> Vec<String> {
        items
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    };
    PluginTranslationSummary {
        tools: names(&report["tools"], "id"),
        commands: names(&report["commands"], "id"),
        model_sources: strings(&report["modelSources"]),
        unmounted_slots: names(&report["ui"]["unmountedSlots"], "name"),
        degraded: report["static"]["degraded"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| PluginTranslationNote {
                name: item["name"].as_str().unwrap_or_default().to_string(),
                status: item["status"].as_str().unwrap_or_default().to_string(),
                note: item["note"].as_str().unwrap_or_default().to_string(),
            })
            .collect(),
        omitted_files: report["files"]["omitted"]
            .as_array()
            .map_or(0, |files| files.len() as i64),
    }
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

    #[test]
    fn summary_reads_report_and_tolerates_missing_fields() {
        let summary = summarize(
            r#"{"report":{"tools":[{"id":"t"}],"commands":[{"id":"c"}],"modelSources":["m"],
            "ui":{"unmountedSlots":[{"name":"main"}]},
            "static":{"degraded":[{"name":"x","status":"dropped","note":"n"}]},
            "files":{"omitted":["a","b"]}}}"#,
        );
        assert_eq!(summary.tools, ["t"]);
        assert_eq!(summary.commands, ["c"]);
        assert_eq!(summary.model_sources, ["m"]);
        assert_eq!(summary.unmounted_slots, ["main"]);
        assert_eq!(summary.degraded.len(), 1);
        assert_eq!(summary.omitted_files, 2);
        let empty = summarize("not json");
        assert!(empty.tools.is_empty() && empty.omitted_files == 0);
    }
}

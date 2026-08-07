use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use magi_api::{ApiState, BrowserRuntimeStatusSnapshot};
use magi_browser_runtime::{
    BrowserHostClient, BrowserHostCommand, BrowserHostCommandOutcome, BrowserHostCommandResult,
    BrowserHostControlMode, BrowserHostEvent, BrowserHostHandshake, BrowserHostIncomingEvent,
    BrowserProfileControlMode, BrowserRuntimeComponentAction, BrowserRuntimeComponentOperation,
    BrowserRuntimeComponentStatus, BrowserRuntimeControlReceiver, BrowserRuntimeManager,
    BrowserRuntimeManagerConfig, BrowserRuntimeReleaseChannel, BrowserRuntimeTarget,
    BrowserRuntimeUpdateLevel, BrowserSessionLifecycle, BrowserTabLifecycle, HostViewport,
    SignedBrowserRuntimeRelease, browser_runtime_control_channel,
};
use magi_core::{BrowserProfileId, EventId, UtcMillis};
use magi_event_bus::{EventContext, EventEnvelope};
use semver::Version;
use serde::Deserialize;
use sysinfo::{Pid, ProcessesToUpdate, Signal, System};
use tokio::io::{AsyncBufReadExt, BufReader};

const DEFAULT_BROWSER_PROFILE_ID: &str = "browser-profile-default";
const HOST_START_TIMEOUT: Duration = Duration::from_secs(30);
const HOST_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);
const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(65);
const STALE_PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_MAX_ATTEMPTS: usize = 2;
const RUNTIME_CONTROL_QUEUE_CAPACITY: usize = 8;
const RELEASE_FEED_MAX_BYTES: u64 = 1024 * 1024;
const MAX_SAFE_RUNTIME_EPOCH: u64 = (1 << 53) - 1;
const TRANSIENT_BROWSER_CACHE_PATHS: &[&str] = &[
    "Default/Cache",
    "Default/Code Cache",
    "Default/GPUCache",
    "Default/Service Worker/CacheStorage",
    "Default/Service Worker/ScriptCache",
    "Default/Shared Dictionary",
    "DawnCache",
    "DawnGraphiteCache",
    "GPUPersistentCache",
    "GrShaderCache",
    "GraphiteDawnCache",
    "ShaderCache",
];
static DOWNLOAD_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct BrowserHostProcessConfig {
    node_executable: PathBuf,
    host_entry: PathBuf,
    chromium_executable: PathBuf,
    profile_path: PathBuf,
    runtime_version: String,
    host_version: String,
    playwright_version: String,
    runtime_mode: &'static str,
}

struct ManagedBrowserRuntime {
    manager: Arc<BrowserRuntimeManager>,
    release_feed_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserRuntimeReleaseFeed {
    release: SignedBrowserRuntimeRelease,
    archive_url: String,
}

#[derive(Debug, Deserialize)]
struct HostStartupLine {
    status: String,
    port: Option<u16>,
    error: Option<String>,
}

pub(super) fn start_controller(state: &ApiState) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("当前线程没有 Tokio runtime，无法启动浏览器运行组件控制器");
        return;
    };
    let (control, receiver) = browser_runtime_control_channel(RUNTIME_CONTROL_QUEUE_CAPACITY);
    state.set_browser_runtime_control(control);
    let state = state.clone();
    handle.spawn(async move {
        run_browser_runtime_controller(state, receiver).await;
    });
}

#[cfg(debug_assertions)]
fn configured_dev_runtime(state: &ApiState) -> Option<BrowserHostProcessConfig> {
    // 只有 daemon 开发托管模式才允许注入外部 Browser Host。
    // 桌面 Debug 包也带有 debug_assertions，不能因为继承了同一终端的
    // Host 路径就被误判为开发态，否则设置页会永久关闭组件管理。
    if env::var("MAGI_WEB_DEV").ok().as_deref() != Some("1") {
        return None;
    }
    let host_entry = env::var_os("MAGI_BROWSER_DEV_HOST_ENTRY").map(PathBuf::from)?;
    let chromium_executable = match env::var_os("MAGI_BROWSER_DEV_CHROMIUM").map(PathBuf::from) {
        Some(path) => path,
        None => {
            tracing::warn!("已配置 MAGI_BROWSER_DEV_HOST_ENTRY，但缺少 MAGI_BROWSER_DEV_CHROMIUM");
            return None;
        }
    };
    let node_executable = env::var_os("MAGI_BROWSER_DEV_NODE")
        .map(PathBuf::from)
        .or_else(|| magi_process::resolve_executable("node"));
    let Some(node_executable) = node_executable else {
        tracing::warn!("开发态 Browser Host 缺少 Node 可执行文件");
        return None;
    };
    for (name, path) in [
        ("node", &node_executable),
        ("host", &host_entry),
        ("chromium", &chromium_executable),
    ] {
        if !path.is_file() {
            tracing::warn!(component = name, path = %path.display(), "开发态浏览器组件文件不存在");
            return None;
        }
    }
    let profile_path = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .profile(&BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID))
        .map(|profile| profile.data_path.clone());
    let Some(profile_path) = profile_path else {
        tracing::warn!("浏览器默认 Profile 尚未初始化");
        return None;
    };
    Some(BrowserHostProcessConfig {
        node_executable,
        host_entry,
        chromium_executable,
        profile_path,
        runtime_version: "dev-workspace".to_string(),
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        playwright_version: "1.58.2".to_string(),
        runtime_mode: "development",
    })
}

#[cfg(not(debug_assertions))]
fn configured_dev_runtime(_state: &ApiState) -> Option<BrowserHostProcessConfig> {
    None
}

fn configured_managed_runtime(state: &ApiState) -> Option<ManagedBrowserRuntime> {
    let key_hex = configured_release_key_hex()?;
    let trusted_release_key = parse_release_key(&key_hex)
        .map_err(|_| {
            tracing::warn!("Browser Runtime 发布公钥不是 32 字节十六进制值");
        })
        .ok()?;
    let root = state.browser_runtime_component_root()?;
    let channel = match env::var("MAGI_BROWSER_RUNTIME_CHANNEL")
        .unwrap_or_else(|_| "stable".to_string())
        .as_str()
    {
        "stable" => BrowserRuntimeReleaseChannel::Stable,
        "beta" => BrowserRuntimeReleaseChannel::Beta,
        "nightly" => BrowserRuntimeReleaseChannel::Nightly,
        value => {
            tracing::warn!(channel = value, "未知 Browser Runtime channel");
            return None;
        }
    };
    Some(ManagedBrowserRuntime {
        manager: Arc::new(BrowserRuntimeManager::new(
            BrowserRuntimeManagerConfig::production_defaults(
                root,
                BrowserRuntimeTarget {
                    os: env::consts::OS.to_string(),
                    arch: env::consts::ARCH.to_string(),
                },
                channel,
                Version::parse(env!("CARGO_PKG_VERSION")).ok()?,
                trusted_release_key,
            ),
        )),
        release_feed_url: configured_release_feed_url(),
    })
}

fn configured_release_key_hex() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(value) = env::var("MAGI_BROWSER_RUNTIME_RELEASE_KEY_HEX") {
        return Some(value);
    }
    option_env!("MAGI_BROWSER_RUNTIME_RELEASE_KEY_HEX").map(str::to_owned)
}

fn configured_release_feed_url() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(value) = env::var("MAGI_BROWSER_RUNTIME_RELEASE_FEED_URL") {
        return Some(value);
    }
    option_env!("MAGI_BROWSER_RUNTIME_RELEASE_FEED_URL").map(str::to_owned)
}

fn configured_component_runtime(
    state: &ApiState,
    manager: &BrowserRuntimeManager,
) -> Result<Option<BrowserHostProcessConfig>, String> {
    let release = match manager.inspect_active_release(UtcMillis::now()) {
        Ok(Some(release)) => release,
        Ok(None) => return Ok(None),
        Err(error) => return Err(format!("Browser Runtime Component 校验失败: {error}")),
    };
    let install_root = manager.runtime_path(&release.manifest.runtime_version);
    let entrypoints = magi_browser_runtime::BrowserRuntimeEntrypoints {
        node_executable: install_root.join(&release.manifest.node_executable_path),
        host_entry: install_root.join(&release.manifest.host_entry_path),
        chromium_executable: install_root.join(&release.manifest.chromium_executable_path),
        install_root,
    };
    for (name, path) in [
        ("node", &entrypoints.node_executable),
        ("host", &entrypoints.host_entry),
        ("chromium", &entrypoints.chromium_executable),
    ] {
        if !path.is_file() {
            tracing::warn!(component = name, path = %path.display(), "已激活 Browser Runtime 入口不存在");
            return Err(format!("已激活 Browser Runtime 入口不存在: {name}"));
        }
    }
    let profile_path = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .profile(&BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID))
        .map(|profile| profile.data_path.clone())
        .ok_or_else(|| "浏览器默认 Profile 尚未初始化".to_string())?;
    Ok(Some(BrowserHostProcessConfig {
        node_executable: entrypoints.node_executable,
        host_entry: entrypoints.host_entry,
        chromium_executable: entrypoints.chromium_executable,
        profile_path,
        runtime_version: release.manifest.runtime_version.to_string(),
        host_version: release.manifest.host_version.to_string(),
        playwright_version: release.manifest.playwright_version.to_string(),
        runtime_mode: "managed",
    }))
}

async fn run_browser_runtime_controller(
    state: ApiState,
    mut receiver: BrowserRuntimeControlReceiver,
) {
    let development = configured_dev_runtime(&state);
    let development_mode = development.is_some();
    let managed = configured_managed_runtime(&state);
    let management_available = development.is_none()
        && managed
            .as_ref()
            .and_then(|runtime| runtime.release_feed_url.as_ref())
            .is_some();
    let mut supervisor = None;

    if let Some(config) = development {
        supervisor = Some(tokio::spawn(supervise_browser_host(state.clone(), config)));
    } else if let Some(runtime) = managed.as_ref() {
        match configured_component_runtime(&state, runtime.manager.as_ref()) {
            Ok(Some(config)) => {
                let mut status = state.browser_runtime_status();
                status.runtime_mode = "managed".to_string();
                status.component_status = BrowserRuntimeComponentStatus::Installed;
                status.component_management_available = management_available;
                status.last_error_code = None;
                state.set_browser_runtime_status(status);
                supervisor = Some(tokio::spawn(supervise_browser_host(state.clone(), config)));
            }
            Ok(None) => set_component_state(
                &state,
                BrowserRuntimeComponentStatus::NotInstalled,
                "managed",
                "stopped",
                management_available,
                None,
            ),
            Err(error) => {
                tracing::warn!(%error);
                set_component_state(
                    &state,
                    BrowserRuntimeComponentStatus::Failed,
                    "managed",
                    "failed",
                    management_available,
                    Some("browser_runtime_invalid".to_string()),
                );
            }
        }
    } else {
        set_component_state(
            &state,
            BrowserRuntimeComponentStatus::NotInstalled,
            "unavailable",
            "stopped",
            false,
            None,
        );
    }

    while let Some(request) = receiver.recv().await {
        let result = if development_mode {
            Err("开发工作区运行时由 daemon 启动配置管理，不能从设置页修改".to_string())
        } else if let Some(runtime) = managed.as_ref() {
            handle_component_action(&state, runtime, &mut supervisor, request.action).await
        } else {
            Err("浏览器运行组件发布配置不可用".to_string())
        };
        if let Err(error) = result.as_ref() {
            tracing::warn!(action = ?request.action, %error, "浏览器运行组件操作失败");
            let mut status = state.browser_runtime_status();
            if status.component_management_available && status.last_error_code.is_none() {
                status.last_error_code = Some(
                    match request.action {
                        BrowserRuntimeComponentAction::CheckForUpdates => {
                            "browser_runtime_update_check_failed"
                        }
                        BrowserRuntimeComponentAction::Install => "browser_runtime_install_failed",
                        BrowserRuntimeComponentAction::Uninstall => {
                            "browser_runtime_uninstall_failed"
                        }
                    }
                    .to_string(),
                );
                state.set_browser_runtime_status(status);
            }
        }
        let _ = request.response.send(result);
    }

    stop_host_supervisor(&state, &mut supervisor, false).await;
}

async fn handle_component_action(
    state: &ApiState,
    runtime: &ManagedBrowserRuntime,
    supervisor: &mut Option<tokio::task::JoinHandle<()>>,
    action: BrowserRuntimeComponentAction,
) -> Result<BrowserRuntimeComponentOperation, String> {
    match action {
        BrowserRuntimeComponentAction::CheckForUpdates => {
            let feed_url = runtime
                .release_feed_url
                .as_deref()
                .ok_or_else(|| "浏览器运行组件发布源未配置".to_string())?;
            check_for_component_updates(state, runtime.manager.clone(), feed_url).await
        }
        BrowserRuntimeComponentAction::Install => {
            let feed_url = runtime
                .release_feed_url
                .as_deref()
                .ok_or_else(|| "浏览器运行组件发布源未配置".to_string())?;
            install_component(state, runtime.manager.clone(), feed_url, supervisor).await
        }
        BrowserRuntimeComponentAction::Uninstall => {
            uninstall_component(state, runtime.manager.clone(), supervisor).await
        }
    }
}

async fn check_for_component_updates(
    state: &ApiState,
    manager: Arc<BrowserRuntimeManager>,
    feed_url: &str,
) -> Result<BrowserRuntimeComponentOperation, String> {
    let feed = fetch_release_feed(feed_url).await?;
    let release = feed.release.clone();
    let assessment = tokio::task::spawn_blocking({
        let manager = manager.clone();
        move || manager.assess_release(&release, UtcMillis::now())
    })
    .await
    .map_err(|error| format!("浏览器更新检查任务异常退出: {error}"))?
    .map_err(|error| format!("浏览器更新清单校验失败: {error}"))?;
    let installed = tokio::task::spawn_blocking(move || manager.active())
        .await
        .map_err(|error| format!("读取浏览器安装状态任务异常退出: {error}"))?
        .map_err(|error| format!("读取浏览器安装状态失败: {error}"))?
        .is_some();
    let component_status = if !assessment.requires_install {
        BrowserRuntimeComponentStatus::Installed
    } else if !installed {
        BrowserRuntimeComponentStatus::NotInstalled
    } else if assessment.update_level == BrowserRuntimeUpdateLevel::RequiredSecurity {
        BrowserRuntimeComponentStatus::UpdateRequired
    } else {
        BrowserRuntimeComponentStatus::UpdateAvailable
    };
    let mut status = state.browser_runtime_status();
    status.component_status = component_status;
    status.runtime_mode = "managed".to_string();
    status.component_management_available = true;
    status.available_runtime_version = assessment
        .requires_install
        .then(|| assessment.runtime_version.to_string());
    status.update_level = assessment
        .requires_install
        .then_some(assessment.update_level);
    status.last_error_code = None;
    state.set_browser_runtime_status(status);
    publish_runtime_status(state, &state.browser_runtime_status().host_status, None);
    Ok(BrowserRuntimeComponentOperation {
        action: BrowserRuntimeComponentAction::CheckForUpdates,
        runtime_version: Some(assessment.runtime_version.to_string()),
        update_available: assessment.requires_install,
    })
}

async fn install_component(
    state: &ApiState,
    manager: Arc<BrowserRuntimeManager>,
    feed_url: &str,
    supervisor: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<BrowserRuntimeComponentOperation, String> {
    let feed = fetch_release_feed(feed_url).await?;
    let release = feed.release.clone();
    let assessment = tokio::task::spawn_blocking({
        let manager = manager.clone();
        let release = release.clone();
        move || manager.assess_release(&release, UtcMillis::now())
    })
    .await
    .map_err(|error| format!("浏览器安装评估任务异常退出: {error}"))?
    .map_err(|error| format!("浏览器安装清单校验失败: {error}"))?;
    if !assessment.requires_install {
        ensure_managed_host_ready(state, &manager, supervisor).await?;
        let mut status = state.browser_runtime_status();
        status.component_status = BrowserRuntimeComponentStatus::Installed;
        status.available_runtime_version = None;
        status.update_level = None;
        status.last_error_code = None;
        state.set_browser_runtime_status(status);
        return Ok(BrowserRuntimeComponentOperation {
            action: BrowserRuntimeComponentAction::Install,
            runtime_version: Some(assessment.runtime_version.to_string()),
            update_available: false,
        });
    }

    update_component_operation_status(
        state,
        BrowserRuntimeComponentStatus::Downloading,
        Some(assessment.runtime_version.to_string()),
        Some(assessment.update_level),
        None,
    );
    let archive_path = download_release_archive(
        &feed.archive_url,
        manager.root().to_path_buf(),
        release.archive_size_bytes,
    )
    .await
    .inspect_err(|_| {
        update_component_operation_status(
            state,
            BrowserRuntimeComponentStatus::Failed,
            Some(assessment.runtime_version.to_string()),
            Some(assessment.update_level),
            Some("browser_runtime_download_failed".to_string()),
        );
    })?;
    update_component_operation_status(
        state,
        BrowserRuntimeComponentStatus::Verifying,
        Some(assessment.runtime_version.to_string()),
        Some(assessment.update_level),
        None,
    );
    let install_result = tokio::task::spawn_blocking({
        let manager = manager.clone();
        let release = release.clone();
        let archive_path = archive_path.clone();
        move || {
            manager.install_archive(
                &release,
                &archive_path,
                UtcMillis::now(),
                &runtime_component_self_test,
            )
        }
    })
    .await
    .map_err(|error| format!("浏览器安装任务异常退出: {error}"))?;
    let _ = fs::remove_file(&archive_path);
    install_result.map_err(|error| {
        update_component_operation_status(
            state,
            BrowserRuntimeComponentStatus::Failed,
            Some(assessment.runtime_version.to_string()),
            Some(assessment.update_level),
            Some("browser_runtime_install_failed".to_string()),
        );
        format!("浏览器运行组件安装失败: {error}")
    })?;

    stop_host_supervisor(state, supervisor, true).await;
    ensure_managed_host_ready(state, &manager, supervisor).await?;
    Ok(BrowserRuntimeComponentOperation {
        action: BrowserRuntimeComponentAction::Install,
        runtime_version: Some(release.manifest.runtime_version.to_string()),
        update_available: false,
    })
}

async fn ensure_managed_host_ready(
    state: &ApiState,
    manager: &BrowserRuntimeManager,
    supervisor: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), String> {
    let current = state.browser_runtime_status();
    if current.host_status == "ready"
        && current.host_protocol_compatible
        && state.browser_host_client().is_some()
    {
        return Ok(());
    }

    if supervisor
        .as_ref()
        .is_some_and(|handle| handle.is_finished())
    {
        supervisor.take();
    }
    if supervisor.is_none() {
        let config = configured_component_runtime(state, manager)?
            .ok_or_else(|| "浏览器运行组件安装后没有激活版本".to_string())?;
        let mut status = state.browser_runtime_status();
        status.component_status = BrowserRuntimeComponentStatus::Installed;
        status.runtime_mode = "managed".to_string();
        status.host_status = "starting".to_string();
        status.host_protocol_compatible = false;
        status.runtime_version = Some(config.runtime_version.clone());
        status.host_version = Some(config.host_version.clone());
        status.playwright_version = Some(config.playwright_version.clone());
        status.available_runtime_version = None;
        status.update_level = None;
        status.component_management_available = true;
        status.last_error_code = None;
        state.set_browser_runtime_status(status);
        *supervisor = Some(tokio::spawn(supervise_browser_host(state.clone(), config)));
    }

    let deadline = Instant::now() + HOST_READY_WAIT_TIMEOUT;
    loop {
        let status = state.browser_runtime_status();
        if status.host_status == "ready"
            && status.host_protocol_compatible
            && state.browser_host_client().is_some()
        {
            return Ok(());
        }
        if status.host_status == "failed" {
            return Err(format!(
                "Browser Host 启动失败: {}",
                status
                    .last_error_code
                    .as_deref()
                    .unwrap_or("browser_host_start_failed")
            ));
        }
        if Instant::now() >= deadline {
            return Err("Browser Host 在 65 秒内未完成握手".to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn uninstall_component(
    state: &ApiState,
    manager: Arc<BrowserRuntimeManager>,
    supervisor: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<BrowserRuntimeComponentOperation, String> {
    stop_host_supervisor(state, supervisor, true).await;
    tokio::task::spawn_blocking(move || manager.uninstall())
        .await
        .map_err(|error| format!("浏览器卸载任务异常退出: {error}"))?
        .map_err(|error| format!("浏览器运行组件卸载失败: {error}"))?;
    set_component_state(
        state,
        BrowserRuntimeComponentStatus::NotInstalled,
        "managed",
        "stopped",
        true,
        None,
    );
    Ok(BrowserRuntimeComponentOperation {
        action: BrowserRuntimeComponentAction::Uninstall,
        runtime_version: None,
        update_available: false,
    })
}

fn update_component_operation_status(
    state: &ApiState,
    component_status: BrowserRuntimeComponentStatus,
    available_runtime_version: Option<String>,
    update_level: Option<BrowserRuntimeUpdateLevel>,
    error_code: Option<String>,
) {
    let mut status = state.browser_runtime_status();
    status.component_status = component_status;
    status.runtime_mode = "managed".to_string();
    status.available_runtime_version = available_runtime_version;
    status.update_level = update_level;
    status.component_management_available = true;
    status.last_error_code = error_code.clone();
    let host_status = status.host_status.clone();
    state.set_browser_runtime_status(status);
    publish_runtime_status(state, &host_status, error_code.as_deref());
}

fn set_component_state(
    state: &ApiState,
    component_status: BrowserRuntimeComponentStatus,
    runtime_mode: &str,
    host_status: &str,
    management_available: bool,
    error_code: Option<String>,
) {
    let mut status = state.browser_runtime_status();
    status.component_status = component_status;
    status.runtime_mode = runtime_mode.to_string();
    status.host_status = host_status.to_string();
    status.host_protocol_compatible = false;
    status.runtime_version = None;
    status.host_version = None;
    status.playwright_version = None;
    status.chromium_version = None;
    status.available_runtime_version = None;
    status.update_level = None;
    status.component_management_available = management_available;
    status.last_error_code = error_code.clone();
    state.set_browser_runtime_status(status);
    publish_runtime_status(state, host_status, error_code.as_deref());
}

async fn stop_host_supervisor(
    state: &ApiState,
    supervisor: &mut Option<tokio::task::JoinHandle<()>>,
    recover_sessions: bool,
) {
    let had_host = state.browser_host_client().is_some();
    if let Some(client) = state.browser_host_client() {
        state.set_browser_host_client(None);
        let shutdown = tokio::time::timeout(
            HOST_SHUTDOWN_TIMEOUT,
            client.request(BrowserHostCommand::Shutdown),
        )
        .await;
        if !matches!(
            shutdown,
            Ok(Ok(reply))
                if matches!(reply.response.outcome, BrowserHostCommandOutcome::Succeeded(_))
        ) {
            tracing::warn!("Browser Host 未在期限内确认优雅关闭，将终止受管进程树");
        }
        client.close().await;
    }
    if let Some(handle) = supervisor.take() {
        handle.abort();
        let _ = handle.await;
    }
    state.set_browser_host_client(None);
    if recover_sessions && had_host {
        begin_runtime_recovery(state);
    }
}

async fn fetch_release_feed(url: &str) -> Result<BrowserRuntimeReleaseFeed, String> {
    let url = validate_runtime_download_url(url)?;
    tokio::task::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("创建浏览器发布源客户端失败: {error}"))?;
        let mut response = client
            .get(url)
            .send()
            .map_err(|error| format!("读取浏览器发布源失败: {error}"))?
            .error_for_status()
            .map_err(|error| format!("浏览器发布源返回失败: {error}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > RELEASE_FEED_MAX_BYTES)
        {
            return Err("浏览器发布清单超过大小限制".to_string());
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(RELEASE_FEED_MAX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("读取浏览器发布清单失败: {error}"))?;
        if bytes.len() as u64 > RELEASE_FEED_MAX_BYTES {
            return Err("浏览器发布清单超过大小限制".to_string());
        }
        let feed: BrowserRuntimeReleaseFeed = serde_json::from_slice(&bytes)
            .map_err(|error| format!("浏览器发布清单格式无效: {error}"))?;
        validate_runtime_download_url(&feed.archive_url)?;
        Ok(feed)
    })
    .await
    .map_err(|error| format!("浏览器发布源任务异常退出: {error}"))?
}

async fn download_release_archive(
    url: &str,
    runtime_root: PathBuf,
    expected_size: u64,
) -> Result<PathBuf, String> {
    let url = validate_runtime_download_url(url)?;
    tokio::task::spawn_blocking(move || {
        let download_root = runtime_root.join(".downloads");
        fs::create_dir_all(&download_root)
            .map_err(|error| format!("创建浏览器下载目录失败: {error}"))?;
        let path = download_root.join(format!(
            "runtime-{}-{}.tar.zst",
            std::process::id(),
            DOWNLOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30 * 60))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("创建浏览器下载客户端失败: {error}"))?;
            let mut response = client
                .get(url)
                .send()
                .map_err(|error| format!("下载浏览器运行组件失败: {error}"))?
                .error_for_status()
                .map_err(|error| format!("浏览器运行组件下载源返回失败: {error}"))?;
            if response
                .content_length()
                .is_some_and(|length| length != expected_size)
            {
                return Err("浏览器运行组件下载大小与签名清单不一致".to_string());
            }
            let mut file = File::create(&path)
                .map_err(|error| format!("创建浏览器运行组件下载文件失败: {error}"))?;
            let copied = std::io::copy(
                &mut response.by_ref().take(expected_size.saturating_add(1)),
                &mut file,
            )
            .map_err(|error| format!("保存浏览器运行组件失败: {error}"))?;
            if copied != expected_size {
                return Err("浏览器运行组件下载大小与签名清单不一致".to_string());
            }
            file.flush()
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("同步浏览器运行组件下载文件失败: {error}"))?;
            Ok(path.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result
    })
    .await
    .map_err(|error| format!("浏览器下载任务异常退出: {error}"))?
}

fn validate_runtime_download_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value.trim())
        .map_err(|error| format!("浏览器运行组件下载地址无效: {error}"))?;
    if url.scheme() == "https" {
        return Ok(url);
    }
    #[cfg(debug_assertions)]
    if url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
    {
        return Ok(url);
    }
    Err("浏览器运行组件只允许从 HTTPS 发布源下载".to_string())
}

fn runtime_component_self_test(
    install_root: &std::path::Path,
    manifest: &magi_browser_runtime::BrowserRuntimeManifest,
) -> Result<(), String> {
    let profile_path = install_root.join(".self-test-profile");
    let config = BrowserHostProcessConfig {
        node_executable: install_root.join(&manifest.node_executable_path),
        host_entry: install_root.join(&manifest.host_entry_path),
        chromium_executable: install_root.join(&manifest.chromium_executable_path),
        profile_path: profile_path.clone(),
        runtime_version: manifest.runtime_version.to_string(),
        host_version: manifest.host_version.to_string(),
        playwright_version: manifest.playwright_version.to_string(),
        runtime_mode: "managed",
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("创建浏览器自检运行时失败: {error}"))?;
    let result = runtime.block_on(async {
        let (mut child, client, handshake) =
            start_host_attempt(&config, runtime_self_test_epoch()).await?;
        let validation = if handshake.host_version != manifest.host_version.to_string() {
            Err("Browser Host 版本与签名清单不一致".to_string())
        } else if handshake.playwright_version != manifest.playwright_version.to_string() {
            Err("Playwright 版本与签名清单不一致".to_string())
        } else if handshake.chromium_version != manifest.chromium_version {
            Err("Chromium 版本与签名清单不一致".to_string())
        } else {
            Ok(())
        };
        client.close().await;
        let _ = child.terminate().await;
        validation
    });
    let _ = fs::remove_dir_all(profile_path);
    result
}

fn runtime_self_test_epoch() -> u64 {
    UtcMillis::now().0.min(MAX_SAFE_RUNTIME_EPOCH)
}

fn parse_release_key(value: &str) -> Result<[u8; 32], ()> {
    let value = value.trim();
    if value.len() != 64 {
        return Err(());
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(key)
}

async fn supervise_browser_host(state: ApiState, config: BrowserHostProcessConfig) {
    let mut last_failure = "browser_host_start_failed".to_string();
    for attempt in 0..HOST_MAX_ATTEMPTS {
        set_runtime_status(&state, &config, "starting", false, None, None);
        let started = start_host_attempt(&config, attempt as u64).await;
        let (mut child, client, handshake) = match started {
            Ok(started) => started,
            Err(error) => {
                last_failure = error;
                tracing::warn!(attempt = attempt + 1, error = %last_failure, "Browser Host 启动失败");
                if attempt + 1 < HOST_MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                continue;
            }
        };
        if let Err(error) = synchronize_authority_control(&state, &client).await {
            last_failure = error;
            let _ = child.terminate().await;
            client.close().await;
            tracing::warn!(attempt = attempt + 1, error = %last_failure, "Browser Host 控制 fence 初始化失败");
            if attempt + 1 < HOST_MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            continue;
        }
        state.set_browser_host_client(Some(client.clone()));
        // `ready` 是会话级契约，不只是 Host 进程完成握手。
        // 现有权威会话在页面重建完成前仍处于恢复中，因此这段时间对 API
        // 调用方必须保持 Host 不可用。
        if let Err(error) = restore_browser_sessions(&state, &client).await {
            last_failure = format!("browser_session_restore_failed:{error}");
            tracing::warn!(error = %last_failure, "Browser Host 启动后恢复页面边界失败");
            let _ = child.start_terminate();
            let _ = child.wait().await;
            client.close().await;
            state.set_browser_host_client(None);
            begin_runtime_recovery(&state);
            set_runtime_status(
                &state,
                &config,
                "recovering",
                false,
                Some(last_failure.clone()),
                None,
            );
            publish_runtime_status(&state, "recovering", Some(&last_failure));
            if attempt + 1 < HOST_MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            continue;
        }
        set_runtime_status(&state, &config, "ready", true, None, Some(&handshake));
        publish_runtime_status(&state, "ready", None);

        let mut events = client.subscribe();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut last_heartbeat = Instant::now();
        let failure = loop {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if matches!(event.envelope.event, BrowserHostEvent::Heartbeat { .. }) {
                                last_heartbeat = Instant::now();
                            }
                            handle_host_event(&state, event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "Browser Host 事件消费者落后，状态投影将以 Authority 为准");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break "browser_host_event_stream_closed".to_string();
                        }
                    }
                }
                _ = tick.tick() => {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            break format!("browser_host_exited_{status}");
                        }
                        Ok(None) => {}
                        Err(error) => {
                            break format!("browser_host_wait_failed:{error}");
                        }
                    }
                    if last_heartbeat.elapsed() > HOST_HEARTBEAT_TIMEOUT {
                        let _ = child.start_terminate();
                        let _ = child.wait().await;
                        break "browser_host_heartbeat_timeout".to_string();
                    }
                }
            }
        };
        last_failure = failure;
        client.close().await;
        state.set_browser_host_client(None);
        begin_runtime_recovery(&state);
        set_runtime_status(
            &state,
            &config,
            "recovering",
            false,
            Some(last_failure.clone()),
            None,
        );
        publish_runtime_status(&state, "recovering", Some(&last_failure));
        if attempt + 1 < HOST_MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    mark_runtime_failed(&state);
    set_runtime_status(
        &state,
        &config,
        "failed",
        false,
        Some(last_failure.clone()),
        None,
    );
    publish_runtime_status(&state, "failed", Some(&last_failure));
}

async fn synchronize_authority_control(
    state: &ApiState,
    client: &BrowserHostClient,
) -> Result<(), String> {
    let control = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .profile_control_snapshot(&BrowserProfileId::new(DEFAULT_BROWSER_PROFILE_ID))
        .map_err(|error| format!("读取 BrowserAuthority 控制状态失败: {error}"))?;
    let reply = client
        .request(BrowserHostCommand::UpdateControl {
            fence: control.fence,
            mode: match control.mode {
                BrowserProfileControlMode::Agent => BrowserHostControlMode::Agent,
                BrowserProfileControlMode::User => BrowserHostControlMode::User,
            },
        })
        .await
        .map_err(|error| format!("同步 BrowserAuthority 控制状态失败: {error}"))?;
    if matches!(
        reply.response.outcome,
        BrowserHostCommandOutcome::Succeeded(_)
    ) {
        Ok(())
    } else {
        Err(format!(
            "Browser Host 拒绝 Authority 控制状态: {:?}",
            reply.response.outcome
        ))
    }
}

async fn start_host_attempt(
    config: &BrowserHostProcessConfig,
    runtime_epoch: u64,
) -> Result<
    (
        magi_process::AsyncManagedChild,
        BrowserHostClient,
        BrowserHostHandshake,
    ),
    String,
> {
    std::fs::create_dir_all(&config.profile_path)
        .map_err(|error| format!("创建浏览器 Profile 失败: {error}"))?;
    let recovered_processes = cleanup_stale_browser_processes(config).await?;
    let cleared_cache_entries = clear_transient_browser_cache(&config.profile_path)?;
    tracing::info!(
        profile_path = %config.profile_path.display(),
        cleared_cache_entries,
        recovered_processes,
        runtime_epoch,
        "Browser Host 启动前已清理临时缓存"
    );
    let token = random_auth_token()?;
    let mut command = magi_process::tokio_command(&config.node_executable);
    command
        .arg(&config.host_entry)
        .env("MAGI_BROWSER_PROFILE_PATH", &config.profile_path)
        .env(
            "MAGI_BROWSER_CHROMIUM_EXECUTABLE",
            &config.chromium_executable,
        )
        .env("MAGI_BROWSER_RUNTIME_VERSION", &config.runtime_version)
        .env("MAGI_BROWSER_HOST_VERSION", &config.host_version)
        .env(
            "MAGI_BROWSER_PLAYWRIGHT_VERSION",
            &config.playwright_version,
        )
        .env("MAGI_BROWSER_RUNTIME_EPOCH", runtime_epoch.to_string())
        .env("MAGI_BROWSER_DAEMON_PID", std::process::id().to_string())
        .env("MAGI_BROWSER_HOST_PORT", "0")
        .env("MAGI_BROWSER_HOST_TOKEN", &token)
        .env("MAGI_BROWSER_HEADLESS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = magi_process::spawn_managed_tokio(&mut command)
        .map_err(|error| format!("启动 Browser Host 进程失败: {error}"))?;
    if let Some(stderr) = child.take_stderr() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(
                    output = %magi_core::public_runtime_excerpt(&line, 2048),
                    "Browser Host stderr"
                );
            }
        });
    }
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "Browser Host stdout 未连接".to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    let startup_line = match tokio::time::timeout(HOST_START_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            let _ = child.terminate().await;
            return Err("Browser Host 在就绪前退出".to_string());
        }
        Ok(Err(error)) => {
            let _ = child.terminate().await;
            return Err(format!("读取 Browser Host 启动信息失败: {error}"));
        }
        Err(_) => {
            let _ = child.terminate().await;
            return Err("Browser Host 启动超时".to_string());
        }
    };
    let startup: HostStartupLine = serde_json::from_str(&startup_line)
        .map_err(|error| format!("Browser Host 启动协议无效: {error}"))?;
    if startup.status != "ready" {
        let _ = child.terminate().await;
        return Err(startup
            .error
            .unwrap_or_else(|| "Browser Host 未进入 ready".to_string()));
    }
    let port = startup
        .port
        .ok_or_else(|| "Browser Host 启动信息缺少端口".to_string())?;
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(
                output = %magi_core::public_runtime_excerpt(&line, 2048),
                "Browser Host stdout"
            );
        }
    });
    let url = format!("ws://127.0.0.1:{port}/control");
    let (client, handshake) =
        match BrowserHostClient::connect(&url, &token, HOST_HANDSHAKE_TIMEOUT).await {
            Ok(connected) => connected,
            Err(error) => {
                let _ = child.terminate().await;
                return Err(format!("连接 Browser Host 私有协议失败: {error}"));
            }
        };
    Ok((child, client, handshake))
}

async fn cleanup_stale_browser_processes(
    config: &BrowserHostProcessConfig,
) -> Result<usize, String> {
    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let candidates = matching_browser_processes(&system, config);
    if candidates.is_empty() {
        return Ok(0);
    }

    for pid in &candidates {
        if let Some(process) = system.process(*pid) {
            let _ = process
                .kill_with(Signal::Term)
                .unwrap_or_else(|| process.kill());
        }
    }
    if wait_for_processes_to_exit(&mut system, &candidates, STALE_PROCESS_EXIT_TIMEOUT).await {
        return Ok(candidates.len());
    }

    for pid in &candidates {
        if let Some(process) = system.process(*pid) {
            let _ = process.kill();
        }
    }
    if wait_for_processes_to_exit(&mut system, &candidates, Duration::from_secs(1)).await {
        Ok(candidates.len())
    } else {
        Err(format!(
            "Magi 浏览器 Profile 仍被残留进程占用: {}",
            config.profile_path.display()
        ))
    }
}

fn matching_browser_processes(system: &System, config: &BrowserHostProcessConfig) -> Vec<Pid> {
    let profile_environment = OsString::from(format!(
        "MAGI_BROWSER_PROFILE_PATH={}",
        config.profile_path.display()
    ));
    let user_data_argument =
        OsString::from(format!("--user-data-dir={}", config.profile_path.display()));
    let match_spec = BrowserProcessMatchSpec {
        node_executable: &config.node_executable,
        host_entry: &config.host_entry,
        chromium_executable: &config.chromium_executable,
        profile_environment: &profile_environment,
        user_data_argument: &user_data_argument,
    };
    system
        .processes()
        .values()
        .filter(|process| {
            process.pid().as_u32() != std::process::id()
                && browser_process_matches(
                    process.exe(),
                    process.cmd(),
                    process.environ(),
                    match_spec,
                )
        })
        .map(|process| process.pid())
        .collect()
}

#[derive(Clone, Copy)]
struct BrowserProcessMatchSpec<'a> {
    node_executable: &'a std::path::Path,
    host_entry: &'a std::path::Path,
    chromium_executable: &'a std::path::Path,
    profile_environment: &'a OsString,
    user_data_argument: &'a OsString,
}

fn browser_process_matches(
    executable: Option<&std::path::Path>,
    arguments: &[OsString],
    environment: &[OsString],
    spec: BrowserProcessMatchSpec<'_>,
) -> bool {
    let environment_matches = environment
        .iter()
        .any(|value| value == spec.profile_environment);
    let host_matches = environment_matches
        && executable.is_some_and(|path| same_executable(path, spec.node_executable))
        && arguments
            .iter()
            .any(|argument| same_path_argument(argument, spec.host_entry));
    let chromium_matches = executable
        .is_some_and(|path| same_executable(path, spec.chromium_executable))
        && arguments
            .iter()
            .any(|argument| argument == spec.user_data_argument);
    host_matches || chromium_matches
}

fn same_executable(actual: &std::path::Path, expected: &std::path::Path) -> bool {
    actual == expected
        || actual
            .canonicalize()
            .ok()
            .zip(expected.canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected)
}

fn same_path_argument(actual: &OsStr, expected: &std::path::Path) -> bool {
    std::path::Path::new(actual) == expected
        || std::path::Path::new(actual)
            .canonicalize()
            .ok()
            .zip(expected.canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected)
}

async fn wait_for_processes_to_exit(
    system: &mut System,
    processes: &[Pid],
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        system.refresh_processes(ProcessesToUpdate::Some(processes), true);
        if processes.iter().all(|pid| system.process(*pid).is_none()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn clear_transient_browser_cache(profile_path: &std::path::Path) -> Result<usize, String> {
    let mut removed = 0;
    for relative in TRANSIENT_BROWSER_CACHE_PATHS {
        let path = profile_path.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "读取浏览器临时缓存失败 {}: {error}",
                    path.display()
                ));
            }
        };
        let result = if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|error| format!("清理浏览器临时缓存失败 {}: {error}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

async fn restore_browser_sessions(
    state: &ApiState,
    client: &BrowserHostClient,
) -> Result<(), String> {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions
        .into_iter()
        .filter(|session| session.lifecycle == BrowserSessionLifecycle::Recovering)
        .collect::<Vec<_>>();
    for session in sessions {
        let tabs = {
            let authority = state
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            session
                .tab_ids
                .iter()
                .filter_map(|tab_id| authority.tab(tab_id).cloned())
                .collect::<Vec<_>>()
        };
        let mut failure = None;
        for tab in tabs {
            let reply = client
                .request(BrowserHostCommand::CreatePage {
                    tab_id: tab.tab_id.clone(),
                    initial_url: tab.url.clone(),
                    viewport: HostViewport {
                        width: tab.viewport.width,
                        height: tab.viewport.height,
                        surface_width: tab.viewport.width,
                        surface_height: tab.viewport.height,
                        device_scale_factor_millis: tab.viewport.device_scale_factor_millis,
                        device_type: tab.viewport.device_type,
                    },
                    navigation_revision: tab.navigation_revision,
                    snapshot_revision: tab.snapshot_revision,
                })
                .await;
            let page_state = match reply {
                Ok(reply) => match reply.response.outcome {
                    BrowserHostCommandOutcome::Succeeded(result) => match *result {
                        BrowserHostCommandResult::PageState(page_state) => page_state,
                        result => {
                            failure = Some(format!(
                                "恢复 Tab {} 失败: {:?}",
                                tab.tab_id,
                                BrowserHostCommandOutcome::Succeeded(Box::new(result))
                            ));
                            break;
                        }
                    },
                    outcome => {
                        failure = Some(format!("恢复 Tab {} 失败: {outcome:?}", tab.tab_id));
                        break;
                    }
                },
                Err(error) => {
                    failure = Some(format!("恢复 Tab {} 失败: {error}", tab.tab_id));
                    break;
                }
            };
            state
                .mutate_browser_authority(|authority| {
                    authority.transition_tab(
                        &tab.tab_id,
                        BrowserTabLifecycle::Ready,
                        UtcMillis::now(),
                    )?;
                    authority.apply_host_page_state(
                        &tab.tab_id,
                        page_state.navigation_revision,
                        page_state.url.clone(),
                        page_state.origin.clone(),
                        page_state.title.clone(),
                        UtcMillis::now(),
                    )
                })
                .map_err(|error| format!("恢复 Tab 权威状态失败: {error:?}"))?;
        }
        if let Some(error) = failure {
            mark_session_failed(state, &session.browser_session_id);
            tracing::warn!(browser_session_id = %session.browser_session_id, %error);
            continue;
        }
        state
            .mutate_browser_authority(|authority| {
                authority.transition_session(
                    &session.browser_session_id,
                    BrowserSessionLifecycle::Ready,
                    UtcMillis::now(),
                )
            })
            .map_err(|error| format!("恢复 Browser Session 权威状态失败: {error:?}"))?;
        state.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-browser-session-recovered-{}-{}",
                    session.browser_session_id,
                    UtcMillis::now().0
                )),
                "browser.session.recovered",
                serde_json::json!({
                    "browser_session_id": session.browser_session_id,
                    "runtime_epoch": session.runtime_epoch,
                }),
            )
            .with_context(EventContext {
                workspace_id: Some(session.workspace_id),
                session_id: Some(session.session_id),
                ..EventContext::default()
            }),
        );
    }
    Ok(())
}

fn handle_host_event(state: &ApiState, event: BrowserHostIncomingEvent) {
    match event.envelope.event {
        BrowserHostEvent::PageUpdated(page_state) => {
            let context = browser_tab_context(state, &page_state.tab_id);
            let updated = state.mutate_browser_authority(|authority| {
                authority.apply_host_page_state(
                    &page_state.tab_id,
                    page_state.navigation_revision,
                    page_state.url.clone(),
                    page_state.origin.clone(),
                    page_state.title.clone(),
                    UtcMillis::now(),
                )
            });
            if updated.is_ok() {
                publish_tab_event(
                    state,
                    "browser.tab.updated",
                    context,
                    serde_json::json!({
                        "tab_id": page_state.tab_id,
                        "url": page_state.url,
                        "title": page_state.title,
                        "navigation_revision": page_state.navigation_revision,
                    }),
                );
            }
        }
        BrowserHostEvent::PageSuspended { tab_id } => {
            publish_tab_event(
                state,
                "browser.tab.suspended",
                browser_tab_context(state, &tab_id),
                serde_json::json!({ "tab_id": tab_id }),
            );
        }
        BrowserHostEvent::PageCrashed { tab_id, diagnostic } => {
            let context = browser_tab_context(state, &tab_id);
            let _ = state.mutate_browser_authority(|authority| {
                authority.transition_tab(&tab_id, BrowserTabLifecycle::Crashed, UtcMillis::now())
            });
            publish_tab_event(
                state,
                "browser.tab.crashed",
                context,
                serde_json::json!({
                    "tab_id": tab_id,
                    "diagnostic": diagnostic.map(|value| magi_core::public_runtime_excerpt(&value, 1024)),
                }),
            );
        }
        BrowserHostEvent::ScreencastFrame(frame) => {
            let _ =
                state.record_browser_frame(&frame.tab_id, frame.frame_sequence, UtcMillis::now());
        }
        BrowserHostEvent::Dialog {
            tab_id,
            dialog_type,
            message,
        } => {
            publish_tab_event(
                state,
                "browser.dialog.dismissed",
                browser_tab_context(state, &tab_id),
                serde_json::json!({
                    "tab_id": tab_id,
                    "dialog_type": dialog_type,
                    "message": magi_core::public_runtime_excerpt(&message, 1024),
                }),
            );
        }
        BrowserHostEvent::Download {
            tab_id,
            suggested_filename,
            state: download_state,
            byte_length,
            error,
        } => {
            publish_tab_event(
                state,
                "browser.download.updated",
                browser_tab_context(state, &tab_id),
                serde_json::json!({
                    "tab_id": tab_id,
                    "suggested_filename": suggested_filename,
                    "state": download_state,
                    "byte_length": byte_length,
                    "error": error.map(|value| magi_core::public_runtime_excerpt(&value, 1024)),
                }),
            );
        }
        BrowserHostEvent::FileChooser { tab_id } => {
            publish_tab_event(
                state,
                "browser.file_chooser.cancelled",
                browser_tab_context(state, &tab_id),
                serde_json::json!({ "tab_id": tab_id }),
            );
        }
        BrowserHostEvent::PopupBlocked { tab_id } => {
            publish_tab_event(
                state,
                "browser.popup.blocked",
                browser_tab_context(state, &tab_id),
                serde_json::json!({ "tab_id": tab_id }),
            );
        }
        BrowserHostEvent::Ready(_)
        | BrowserHostEvent::Console { .. }
        | BrowserHostEvent::BinaryPayloadReady(_)
        | BrowserHostEvent::Heartbeat { .. } => {}
    }
}

fn browser_tab_context(
    state: &ApiState,
    tab_id: &magi_core::BrowserTabId,
) -> Option<(magi_core::WorkspaceId, magi_core::SessionId)> {
    let authority = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned");
    let tab = authority.tab(tab_id)?;
    let session = authority.session(&tab.browser_session_id)?;
    Some((session.workspace_id.clone(), session.session_id.clone()))
}

fn publish_tab_event(
    state: &ApiState,
    event_type: &str,
    context: Option<(magi_core::WorkspaceId, magi_core::SessionId)>,
    payload: serde_json::Value,
) {
    let Some((workspace_id, session_id)) = context else {
        return;
    };
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!(
                "event-{}-{}",
                event_type.replace('.', "-"),
                UtcMillis::now().0
            )),
            event_type,
            payload,
        )
        .with_context(EventContext {
            workspace_id: Some(workspace_id),
            session_id: Some(session_id),
            ..EventContext::default()
        }),
    );
}

fn begin_runtime_recovery(state: &ApiState) {
    interrupt_sessions_for_browser_runtime_failure(state);
    if let Err(error) = state.mutate_browser_authority(|authority| {
        authority.begin_runtime_recovery(UtcMillis::now());
        Ok(())
    }) {
        tracing::error!(?error, "Browser Host 失效后进入恢复状态失败");
    }
}

/// Browser Host 失效时，浏览器 Lease、Runner、Turn 和 Goal 必须在同一故障边界
/// 收敛。仅把 Browser Session 标成 recovering 会留下“模型仍在跑但浏览器已失效”
/// 的半状态，下一轮继续也会错误复用旧控制权。
fn interrupt_sessions_for_browser_runtime_failure(state: &ApiState) {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions
        .into_iter()
        .filter(|session| session.lifecycle.is_open())
        .collect::<Vec<_>>();
    for browser_session in sessions {
        state.cancel_execution_resources(
            Some(&browser_session.session_id),
            None,
            None,
            magi_browser_runtime::BrowserLeaseEndReason::RuntimeUnavailable,
        );
        let Some(current_turn) = state
            .session_store
            .runtime_sidecar(&browser_session.session_id)
            .and_then(|sidecar| sidecar.current_turn)
        else {
            continue;
        };
        if matches!(
            current_turn.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            continue;
        }
        let owned_goal = state
            .session_store
            .active_goal_for_execution_owner(&browser_session.session_id, &current_turn.turn_id)
            .is_some();
        if let Some(chain) = state
            .session_store
            .active_execution_chain(&browser_session.session_id)
            && let Some(manager) = state.runner_manager()
            && let Err(error) = manager.kill_tree(chain.root_task_id.as_str())
        {
            tracing::warn!(
                session_id = %browser_session.session_id,
                task_id = %chain.root_task_id,
                ?error,
                "Browser Host 失效后终止执行树失败"
            );
        }
        match state
            .session_store
            .interrupt_current_turn_by_daemon_restart(&browser_session.session_id)
        {
            Ok(Some(_)) => {
                state
                    .conversation_registry
                    .close_session_turn_input(&browser_session.session_id, &current_turn.turn_id);
                if owned_goal {
                    match state
                        .session_store
                        .pause_active_goal_for_diversion(&browser_session.session_id)
                    {
                        Ok(Some((_goal, Some(plan)))) => magi_plan::publish_plan_event(
                            &state.event_bus,
                            magi_plan::plan_event_type(&plan),
                            &plan,
                            Some(&browser_session.workspace_id),
                            None,
                            None,
                        ),
                        Ok(Some((_, None))) | Ok(None) => {}
                        Err(error) => tracing::warn!(
                            session_id = %browser_session.session_id,
                            ?error,
                            "Browser Host 失效后暂停 Goal 与计划失败"
                        ),
                    }
                }
                if let Err(error) = state
                    .persist_session_state_checkpoint("browser_runtime_failure_interrupted_session")
                {
                    tracing::warn!(
                        session_id = %browser_session.session_id,
                        ?error,
                        "Browser Host 失效后的 session 状态持久化失败"
                    );
                }
                state.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::new(format!(
                            "event-browser-runtime-turn-interrupted-{}-{}",
                            browser_session.session_id,
                            UtcMillis::now().0
                        )),
                        "session.turn.interrupted",
                        serde_json::json!({
                            "session_id": browser_session.session_id,
                            "workspace_id": browser_session.workspace_id,
                            "turn_id": current_turn.turn_id,
                            "interrupted": true,
                            "reason": "browser_runtime_unavailable",
                        }),
                    )
                    .with_context(EventContext {
                        workspace_id: Some(browser_session.workspace_id.clone()),
                        session_id: Some(browser_session.session_id.clone()),
                        ..EventContext::default()
                    }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    session_id = %browser_session.session_id,
                    ?error,
                    "Browser Host 失效后的 session Turn 收敛失败"
                );
            }
        }
    }
}

fn mark_runtime_failed(state: &ApiState) {
    let sessions = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .snapshot()
        .sessions;
    if let Err(error) = state.mutate_browser_authority(|authority| {
        for session in &sessions {
            if session.lifecycle == BrowserSessionLifecycle::Recovering {
                authority.transition_session(
                    &session.browser_session_id,
                    BrowserSessionLifecycle::Failed,
                    UtcMillis::now(),
                )?;
                for tab_id in &session.tab_ids {
                    let lifecycle = authority.tab(tab_id).map(|tab| tab.lifecycle);
                    if lifecycle == Some(BrowserTabLifecycle::Creating) {
                        authority.transition_tab(
                            tab_id,
                            BrowserTabLifecycle::Crashed,
                            UtcMillis::now(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }) {
        tracing::error!(?error, "Browser Host 恢复耗尽后标记失败状态失败");
    }
}

fn mark_session_failed(state: &ApiState, browser_session_id: &magi_core::BrowserSessionId) {
    let _ = state.mutate_browser_authority(|authority| {
        let session = authority
            .session(browser_session_id)
            .cloned()
            .ok_or_else(|| {
                magi_browser_runtime::BrowserAuthorityError::UnknownSession(
                    browser_session_id.clone(),
                )
            })?;
        for tab_id in &session.tab_ids {
            if authority.tab(tab_id).map(|tab| tab.lifecycle) == Some(BrowserTabLifecycle::Creating)
            {
                authority.transition_tab(tab_id, BrowserTabLifecycle::Crashed, UtcMillis::now())?;
            }
        }
        authority.transition_session(
            browser_session_id,
            BrowserSessionLifecycle::Failed,
            UtcMillis::now(),
        )?;
        Ok(())
    });
}

fn set_runtime_status(
    state: &ApiState,
    config: &BrowserHostProcessConfig,
    host_status: &str,
    protocol_compatible: bool,
    error_code: Option<String>,
    handshake: Option<&BrowserHostHandshake>,
) {
    let previous = state.browser_runtime_status();
    state.set_browser_runtime_status(BrowserRuntimeStatusSnapshot {
        revision: 0,
        in_app_browser_enabled: previous.in_app_browser_enabled,
        browser_use_enabled: previous.browser_use_enabled,
        component_status: BrowserRuntimeComponentStatus::Installed,
        runtime_mode: config.runtime_mode.to_string(),
        host_status: host_status.to_string(),
        host_protocol_compatible: protocol_compatible,
        runtime_version: Some(config.runtime_version.clone()),
        host_version: Some(
            handshake
                .map(|value| value.host_version.clone())
                .unwrap_or_else(|| config.host_version.clone()),
        ),
        playwright_version: Some(
            handshake
                .map(|value| value.playwright_version.clone())
                .unwrap_or_else(|| config.playwright_version.clone()),
        ),
        chromium_version: handshake
            .map(|value| value.chromium_version.clone())
            .or_else(|| {
                previous.chromium_version.filter(|_| {
                    previous.runtime_version.as_deref() == Some(&config.runtime_version)
                })
            }),
        available_runtime_version: previous.available_runtime_version,
        update_level: previous.update_level,
        component_management_available: if config.runtime_mode == "development" {
            false
        } else {
            previous.component_management_available
        },
        last_error_code: error_code,
    });
}

fn publish_runtime_status(state: &ApiState, host_status: &str, error_code: Option<&str>) {
    let runtime = state.browser_runtime_status();
    state.event_bus.publish(EventEnvelope::system(
        EventId::new(format!(
            "event-browser-runtime-status-{}-{}",
            host_status,
            UtcMillis::now().0
        )),
        "browser.runtime.status_changed",
        serde_json::json!({
            "host_status": host_status,
            "component_status": runtime.component_status,
            "host_protocol_compatible": runtime.host_protocol_compatible,
            "runtime_version": runtime.runtime_version,
            "error_code": error_code,
            "revision": runtime.revision,
        }),
    ));
}

fn random_auth_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("生成 Browser Host 认证令牌失败: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, ffi::OsString, path::PathBuf, sync::Arc};

    use magi_api::ApiState;
    use magi_core::{
        AccessProfile, BrowserLeaseId, BrowserProfileId, BrowserSessionId, BrowserTabId,
        ExecutionOwnership, MissionId, PlanId, PlanItem, PlanItemId, PlanItemStatus, PlanState,
        SessionId, TaskId, UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::{
        ActiveExecutionChain, ActiveExecutionDispatchContext, ActiveExecutionTurn, GoalStatus,
        SessionPlan, SessionStore,
    };
    use magi_workspace::WorkspaceStore;

    use super::{
        BrowserProcessMatchSpec, begin_runtime_recovery, browser_process_matches,
        clear_transient_browser_cache, runtime_self_test_epoch,
    };

    #[test]
    fn browser_host_self_test_epoch_is_safe_for_javascript() {
        assert!(runtime_self_test_epoch() < (1 << 53));
    }

    #[test]
    fn stale_browser_process_matching_is_profile_and_executable_specific() {
        let profile = PathBuf::from("/tmp/magi-browser-profile");
        let node = PathBuf::from("/usr/local/bin/node");
        let host = PathBuf::from("/tmp/browser-host/index.cjs");
        let chromium = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
        let profile_environment =
            OsString::from(format!("MAGI_BROWSER_PROFILE_PATH={}", profile.display()));
        let user_data_argument = OsString::from(format!("--user-data-dir={}", profile.display()));
        let host_argument = host.clone().into_os_string();
        let match_spec = BrowserProcessMatchSpec {
            node_executable: &node,
            host_entry: &host,
            chromium_executable: &chromium,
            profile_environment: &profile_environment,
            user_data_argument: &user_data_argument,
        };

        assert!(browser_process_matches(
            Some(&node),
            std::slice::from_ref(&host_argument),
            std::slice::from_ref(&profile_environment),
            match_spec,
        ));
        assert!(browser_process_matches(
            Some(&chromium),
            std::slice::from_ref(&user_data_argument),
            &[],
            match_spec,
        ));
        assert!(!browser_process_matches(
            Some(&chromium),
            &[OsString::from("--user-data-dir=/tmp/other-profile")],
            &[],
            match_spec,
        ));
        assert!(!browser_process_matches(
            Some(&node),
            std::slice::from_ref(&host_argument),
            &[OsString::from(
                "MAGI_BROWSER_PROFILE_PATH=/tmp/other-profile",
            )],
            match_spec,
        ));
    }

    #[test]
    fn browser_restart_clears_transient_cache_but_preserves_profile_state() {
        let profile = tempfile::tempdir().expect("browser profile fixture should create");
        for relative in [
            "Default/Cache/cache.bin",
            "Default/Code Cache/code.bin",
            "Default/GPUCache/gpu.bin",
            "Default/Service Worker/CacheStorage/cache.bin",
            "GrShaderCache/shader.bin",
        ] {
            let path = profile.path().join(relative);
            std::fs::create_dir_all(path.parent().expect("cache path should have parent"))
                .expect("cache directory should create");
            std::fs::write(path, b"cache").expect("cache fixture should write");
        }
        for relative in [
            "Default/Cookies",
            "Default/Local Storage/leveldb/state",
            "Default/IndexedDB/state",
            "Default/Preferences",
        ] {
            let path = profile.path().join(relative);
            std::fs::create_dir_all(path.parent().expect("profile path should have parent"))
                .expect("profile state directory should create");
            std::fs::write(path, b"persistent").expect("profile state fixture should write");
        }

        assert_eq!(
            clear_transient_browser_cache(profile.path())
                .expect("transient browser cache should clear"),
            5
        );
        for relative in [
            "Default/Cache",
            "Default/Code Cache",
            "Default/GPUCache",
            "Default/Service Worker/CacheStorage",
            "GrShaderCache",
        ] {
            assert!(!profile.path().join(relative).exists(), "{relative}");
        }
        for relative in [
            "Default/Cookies",
            "Default/Local Storage/leveldb/state",
            "Default/IndexedDB/state",
            "Default/Preferences",
        ] {
            assert_eq!(
                std::fs::read(profile.path().join(relative))
                    .expect("profile state should remain readable"),
                b"persistent",
                "{relative}"
            );
        }
    }

    #[test]
    fn browser_runtime_failure_atomically_interrupts_goal_plan_and_lease() {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let session_store = Arc::new(SessionStore::new());
        let state = ApiState::new(
            "browser-runtime-recovery-test",
            Arc::clone(&event_bus),
            Arc::clone(&session_store),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let workspace_id = WorkspaceId::new("workspace-browser-runtime-recovery");
        let session_id = SessionId::new("session-browser-runtime-recovery");
        let turn_id = "turn-browser-runtime-recovery";
        session_store
            .create_session_for_workspace(
                session_id.clone(),
                "Browser runtime recovery".to_string(),
                Some(workspace_id.to_string()),
            )
            .expect("session fixture should create");
        let (_, thread_id) =
            session_store.ensure_session_mission(&session_id, UtcMillis(1), || {
                MissionId::new("mission-browser-runtime-recovery")
            });
        let goal = session_store
            .create_goal(
                session_id.clone(),
                thread_id,
                turn_id,
                "验证浏览器运行组件故障收敛",
                AccessProfile::FullAccess,
                None,
            )
            .expect("goal fixture should create");
        session_store
            .upsert_plan_for_goal_progress(
                &session_id,
                SessionPlan {
                    plan_id: PlanId::new("plan-browser-runtime-recovery"),
                    session_id: session_id.clone(),
                    goal_id: Some(goal.goal_id.clone()),
                    revision: 1,
                    language: "zh-CN".to_string(),
                    state: PlanState::Active,
                    items: vec![PlanItem::new(
                        PlanItemId::new("browser-runtime-step"),
                        "推进浏览器任务",
                        PlanItemStatus::InProgress,
                    )],
                    task_bindings: HashMap::new(),
                    task_statuses: HashMap::new(),
                    updated_at: UtcMillis(2),
                },
                Some(0),
                Some(goal.goal_id.clone()),
                Some(goal.control_revision),
            )
            .expect("goal plan fixture should create");
        let root_task_id = TaskId::new(turn_id);
        session_store
            .upsert_active_execution_chain(
                session_id.clone(),
                ActiveExecutionChain {
                    session_id: session_id.clone(),
                    mission_id: MissionId::new("mission-browser-runtime-recovery"),
                    root_task_id: root_task_id.clone(),
                    execution_chain_ref: "chain-browser-runtime-recovery".to_string(),
                    workspace_id: Some(workspace_id.clone()),
                    active_branch_task_ids: Vec::new(),
                    active_worker_bindings: Vec::new(),
                    branches: Vec::new(),
                    recovery_ref: None,
                    dispatch_context: ActiveExecutionDispatchContext {
                        accepted_at: UtcMillis(3),
                        entry_id: "timeline-browser-runtime-recovery".to_string(),
                        trimmed_text: Some("推进浏览器任务".to_string()),
                        skill_name: None,
                    },
                    current_turn: Some(ActiveExecutionTurn {
                        turn_id: turn_id.to_string(),
                        turn_seq: 1,
                        accepted_at: UtcMillis(3),
                        status: "running".to_string(),
                        completed_at: None,
                        user_message: Some("推进浏览器任务".to_string()),
                        items: Vec::new(),
                    }),
                },
            )
            .expect("active execution chain fixture should create");

        let browser_session_id = BrowserSessionId::new("browser-session-runtime-recovery");
        let browser_profile_id = BrowserProfileId::new("browser-profile-runtime-recovery");
        let browser_tab_id = BrowserTabId::new("browser-tab-runtime-recovery");
        let browser_lease_id = BrowserLeaseId::new("browser-lease-runtime-recovery");
        state
            .mutate_browser_authority(|authority| {
                authority.register_profile(magi_browser_runtime::BrowserProfile {
                    profile_id: browser_profile_id.clone(),
                    kind: magi_browser_runtime::BrowserProfileKind::ManagedDefault,
                    data_path: tempfile::tempdir()
                        .expect("profile fixture should create")
                        .keep(),
                    created_at: UtcMillis(1),
                    updated_at: UtcMillis(1),
                })?;
                authority.create_session(magi_browser_runtime::CreateBrowserSession {
                    browser_session_id: browser_session_id.clone(),
                    workspace_id: workspace_id.clone(),
                    session_id: session_id.clone(),
                    profile_id: browser_profile_id.clone(),
                    now: UtcMillis(1),
                })?;
                authority.transition_session(
                    &browser_session_id,
                    magi_browser_runtime::BrowserSessionLifecycle::Ready,
                    UtcMillis(1),
                )?;
                authority.create_tab(magi_browser_runtime::CreateBrowserTab {
                    tab_id: browser_tab_id.clone(),
                    browser_session_id: browser_session_id.clone(),
                    url: "about:blank".to_string(),
                    viewport: magi_browser_runtime::BrowserViewport::default(),
                    now: UtcMillis(1),
                })?;
                authority.transition_tab(
                    &browser_tab_id,
                    magi_browser_runtime::BrowserTabLifecycle::Ready,
                    UtcMillis(1),
                )?;
                authority.acquire_lease(magi_browser_runtime::AcquireBrowserLease {
                    lease_id: browser_lease_id.clone(),
                    profile_id: browser_profile_id,
                    browser_session_id: browser_session_id.clone(),
                    owner: ExecutionOwnership {
                        workspace_id: Some(workspace_id.clone()),
                        session_id: Some(session_id.clone()),
                        task_id: Some(root_task_id),
                        ..ExecutionOwnership::default()
                    },
                    turn_id: turn_id.to_string(),
                    goal_binding: Some(magi_browser_runtime::GoalControlBinding {
                        goal_id: goal.goal_id.clone(),
                        control_revision: goal.control_revision,
                    }),
                    acquired_at: UtcMillis(4),
                    expires_at: UtcMillis(10_000),
                })?;
                Ok(())
            })
            .expect("browser authority fixture should create");

        begin_runtime_recovery(&state);

        assert_eq!(
            session_store
                .runtime_sidecar(&session_id)
                .and_then(|sidecar| sidecar.current_turn)
                .map(|turn| turn.status),
            Some("interrupted".to_string())
        );
        assert_eq!(
            session_store
                .current_goal(&session_id)
                .map(|goal| goal.status),
            Some(GoalStatus::Paused)
        );
        assert_eq!(
            session_store.plan(&session_id).map(|plan| plan.state),
            Some(PlanState::Paused)
        );
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock should hold");
        assert_eq!(
            authority
                .session(&browser_session_id)
                .map(|session| session.lifecycle),
            Some(magi_browser_runtime::BrowserSessionLifecycle::Recovering)
        );
        assert_eq!(
            authority
                .lease(&browser_lease_id)
                .map(|lease| lease.lifecycle),
            Some(magi_browser_runtime::BrowserLeaseLifecycle::Revoked)
        );
        assert_eq!(
            authority
                .lease(&browser_lease_id)
                .and_then(|lease| lease.end_reason),
            Some(magi_browser_runtime::BrowserLeaseEndReason::RuntimeUnavailable)
        );
        drop(authority);
        let events = event_bus.snapshot().recent_events;
        assert!(events.iter().any(|event| {
            event.event_type == "session.turn.interrupted"
                && event.payload["reason"] == "browser_runtime_unavailable"
        }));
        assert!(events.iter().any(|event| {
            event.event_type == "session.plan.paused"
                && event.payload["session_id"] == session_id.to_string()
        }));
    }
}

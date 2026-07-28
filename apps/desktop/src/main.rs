#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
    time::Duration,
};

use magi_daemon::{Daemon, DaemonConfig, DaemonHandle};
use magi_desktop::lifecycle::{DesktopAction, DesktopLifecycle, DesktopState};
use magi_desktop::runtime_recovery::{
    ExpectedPortOccupant, PortOccupant, diagnose_port, magi_health_available,
    terminate_port_occupants, wait_for_port_release,
};
use magi_runtime_state::RuntimeStateManager;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    ipc::Channel,
    menu::{Menu, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex as AsyncMutex;

const MAIN_WINDOW_LABEL: &str = "main";
const OPEN_MENU_ID: &str = "open-magi";
const QUIT_MENU_ID: &str = "quit-magi";
const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 38123;
const DEFAULT_SERVICE_NAME: &str = "magi-rust-backend";
const DESKTOP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DESKTOP_PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(4);
const DESKTOP_UPDATE_DIRECTORY: &str = "updates";
const STAGED_UPDATE_BYTES_FILE: &str = "pending-update.bin";
const STAGED_UPDATE_METADATA_FILE: &str = "pending-update.json";
const DESKTOP_ROUTE_QUERY_KEYS: [&str; 3] = ["workspaceId", "workspacePath", "sessionId"];

struct DesktopRuntime {
    lifecycle: Arc<DesktopLifecycle>,
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    state_root: PathBuf,
    web_dist_root: PathBuf,
    recovery: Arc<Mutex<DesktopRuntimeRecovery>>,
    restart_lock: AsyncMutex<()>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum DesktopRecoveryStatus {
    Starting,
    Ready,
    Restarting,
    PortOccupied,
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopRuntimeRecovery {
    status: DesktopRecoveryStatus,
    port: u16,
    technical_detail: Option<String>,
    occupants: Vec<PortOccupant>,
    can_restart: bool,
    requires_confirmation: bool,
    web_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestartDesktopRuntimeRequest {
    expected_occupants: Vec<ExpectedPortOccupant>,
    confirm_external_processes: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StagedDesktopUpdate {
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopUpdateInstallability {
    installable: bool,
    reason: Option<DesktopUpdateInstallabilityReason>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DesktopUpdateInstallabilityReason {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    DiskImage,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", content = "data")]
enum DesktopUpdateDownloadEvent {
    #[serde(rename = "Started")]
    Started {
        #[serde(rename = "contentLength")]
        content_length: Option<u64>,
    },
    #[serde(rename = "Progress")]
    Progress {
        #[serde(rename = "chunkLength")]
        chunk_length: usize,
    },
    #[serde(rename = "Finished")]
    Finished,
}

impl DesktopRuntime {
    fn new(state_root: PathBuf, web_dist_root: PathBuf) -> Self {
        Self {
            lifecycle: Arc::new(DesktopLifecycle::new()),
            daemon: Arc::new(Mutex::new(None)),
            state_root,
            web_dist_root,
            recovery: Arc::new(Mutex::new(DesktopRuntimeRecovery::starting(DEFAULT_PORT))),
            restart_lock: AsyncMutex::new(()),
        }
    }
}

impl DesktopRuntimeRecovery {
    fn starting(port: u16) -> Self {
        Self {
            status: DesktopRecoveryStatus::Starting,
            port,
            technical_detail: None,
            occupants: Vec::new(),
            can_restart: false,
            requires_confirmation: false,
            web_url: None,
        }
    }

    fn ready(port: u16, web_url: String) -> Self {
        Self {
            status: DesktopRecoveryStatus::Ready,
            port,
            technical_detail: None,
            occupants: Vec::new(),
            can_restart: false,
            requires_confirmation: false,
            web_url: Some(web_url),
        }
    }

    fn restarting(port: u16) -> Self {
        Self {
            status: DesktopRecoveryStatus::Restarting,
            port,
            technical_detail: None,
            occupants: Vec::new(),
            can_restart: false,
            requires_confirmation: false,
            web_url: None,
        }
    }

    fn failed(port: u16, detail: impl Into<String>) -> Self {
        Self {
            status: DesktopRecoveryStatus::Failed,
            port,
            technical_detail: Some(detail.into()),
            occupants: Vec::new(),
            can_restart: false,
            requires_confirmation: false,
            web_url: None,
        }
    }
}

fn default_state_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".magi")
}

fn desktop_update_installability(executable_path: &Path) -> DesktopUpdateInstallability {
    #[cfg(target_os = "macos")]
    if executable_path.starts_with("/Volumes") {
        return DesktopUpdateInstallability {
            installable: false,
            reason: Some(DesktopUpdateInstallabilityReason::DiskImage),
        };
    }

    #[cfg(not(target_os = "macos"))]
    let _ = executable_path;

    DesktopUpdateInstallability {
        installable: true,
        reason: None,
    }
}

fn desktop_update_installability_for_current_process() -> Result<DesktopUpdateInstallability, String>
{
    let executable_path =
        env::current_exe().map_err(|error| format!("读取桌面应用路径失败: {error}"))?;
    Ok(desktop_update_installability(&executable_path))
}

fn require_desktop_update_installability() -> Result<(), String> {
    let installability = desktop_update_installability_for_current_process()?;
    if installability.installable {
        return Ok(());
    }

    match installability.reason {
        Some(DesktopUpdateInstallabilityReason::DiskImage) => Err(
            "Magi 当前正在磁盘映像中运行。请先将 Magi 拖入“应用程序”文件夹，从应用程序重新打开后再安装更新"
                .to_string(),
        ),
        None => Err("当前 Magi 安装位置无法完成在线更新".to_string()),
    }
}

fn staged_update_paths(state_root: &Path) -> (PathBuf, PathBuf) {
    let directory = state_root.join(DESKTOP_UPDATE_DIRECTORY);
    (
        directory.join(STAGED_UPDATE_BYTES_FILE),
        directory.join(STAGED_UPDATE_METADATA_FILE),
    )
}

fn remove_staged_update(state_root: &Path) {
    let (bytes_path, metadata_path) = staged_update_paths(state_root);
    let _ = fs::remove_file(bytes_path);
    let _ = fs::remove_file(metadata_path);
}

fn read_staged_update(state_root: &Path) -> Result<Option<StagedDesktopUpdate>, String> {
    let (bytes_path, metadata_path) = staged_update_paths(state_root);
    let metadata = match fs::read(&metadata_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_staged_update(state_root);
            return Ok(None);
        }
        Err(error) => return Err(format!("读取更新元数据失败: {error}")),
    };

    if !bytes_path.is_file() {
        remove_staged_update(state_root);
        return Ok(None);
    }

    let update = match serde_json::from_slice::<StagedDesktopUpdate>(&metadata) {
        Ok(update) => update,
        Err(_) => {
            remove_staged_update(state_root);
            return Ok(None);
        }
    };
    if update.current_version != env!("CARGO_PKG_VERSION")
        || update.version == update.current_version
    {
        remove_staged_update(state_root);
        return Ok(None);
    }
    if fs::metadata(&bytes_path)
        .map(|metadata| metadata.len() == 0)
        .unwrap_or(true)
    {
        remove_staged_update(state_root);
        return Ok(None);
    }
    Ok(Some(update))
}

fn read_staged_update_for_version(
    state_root: &Path,
    expected_version: &str,
) -> Result<Option<StagedDesktopUpdate>, String> {
    let staged = read_staged_update(state_root)?;
    if staged
        .as_ref()
        .is_some_and(|update| update.version != expected_version)
    {
        remove_staged_update(state_root);
        return Ok(None);
    }
    Ok(staged)
}

fn write_staged_update(
    state_root: &Path,
    update: &StagedDesktopUpdate,
    bytes: &[u8],
) -> Result<(), String> {
    let directory = state_root.join(DESKTOP_UPDATE_DIRECTORY);
    fs::create_dir_all(&directory).map_err(|error| format!("创建更新目录失败: {error}"))?;
    let (bytes_path, metadata_path) = staged_update_paths(state_root);
    let bytes_temp_path = bytes_path.with_extension("bin.tmp");
    let metadata_temp_path = metadata_path.with_extension("json.tmp");
    remove_staged_update(state_root);
    let _ = fs::remove_file(&bytes_temp_path);
    let _ = fs::remove_file(&metadata_temp_path);

    fs::write(&bytes_temp_path, bytes).map_err(|error| format!("保存更新包失败: {error}"))?;
    let metadata_bytes =
        serde_json::to_vec(update).map_err(|error| format!("序列化更新元数据失败: {error}"))?;
    if let Err(error) = fs::write(&metadata_temp_path, metadata_bytes) {
        let _ = fs::remove_file(&bytes_temp_path);
        return Err(format!("保存更新元数据失败: {error}"));
    }
    if let Err(error) = fs::rename(&bytes_temp_path, &bytes_path) {
        let _ = fs::remove_file(&bytes_temp_path);
        let _ = fs::remove_file(&metadata_temp_path);
        return Err(format!("提交更新包失败: {error}"));
    }
    if let Err(error) = fs::rename(&metadata_temp_path, &metadata_path) {
        let _ = fs::remove_file(&metadata_temp_path);
        remove_staged_update(state_root);
        return Err(format!("提交更新元数据失败: {error}"));
    }
    Ok(())
}

fn resolve_web_dist_root(app: &tauri::App) -> tauri::Result<PathBuf> {
    if cfg!(debug_assertions) {
        return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/dist"));
    }
    Ok(app.path().resource_dir()?.join("web/dist"))
}

fn read_env(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn read_port() -> Result<u16, String> {
    read_env("MAGI_PORT")
        .map(|raw| {
            raw.parse::<u16>()
                .map_err(|error| format!("MAGI_PORT `{raw}` 非法: {error}"))
        })
        .unwrap_or(Ok(DEFAULT_PORT))
}

fn show_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn create_main_window(app: &AppHandle, url: WebviewUrl) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, url)
        .title("")
        .inner_size(1360.0, 860.0)
        .min_inner_size(960.0, 680.0)
        .center()
        .build()?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

fn parse_desktop_web_url(
    value: &str,
    current_url: Option<&tauri::Url>,
) -> Result<tauri::Url, String> {
    let mut url = value
        .parse::<tauri::Url>()
        .map_err(|error| format!("Magi Web 地址非法: {error}"))?;
    let route_binding = current_url
        .into_iter()
        .flat_map(|current| current.query_pairs())
        .filter(|(key, _)| DESKTOP_ROUTE_QUERY_KEYS.contains(&key.as_ref()))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    let mut query = url.query_pairs_mut();
    query.append_pair("desktopVersion", env!("CARGO_PKG_VERSION"));
    query.extend_pairs(route_binding);
    drop(query);
    Ok(url)
}

fn create_startup_error_window(app: &AppHandle) {
    if app.get_webview_window(MAIN_WINDOW_LABEL).is_some() {
        show_main_window(app);
        return;
    }
    if let Err(error) = create_main_window(app, WebviewUrl::App("index.html".into())) {
        eprintln!("创建 Magi 启动错误窗口失败: {error}");
    }
}

fn navigate_main_window(app: &AppHandle, web_url: &str) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let current_url = window
            .url()
            .map_err(|error| format!("读取当前工作台地址失败: {error}"))?;
        let url = parse_desktop_web_url(web_url, Some(&current_url))?;
        window
            .navigate(url)
            .map_err(|error| format!("打开 Magi 工作台失败: {error}"))?;
        show_main_window(app);
        return Ok(());
    }
    let url = parse_desktop_web_url(web_url, None)?;
    create_main_window(app, WebviewUrl::External(url))
        .map_err(|error| format!("创建 Magi 主窗口失败: {error}"))
}

fn set_desktop_recovery(runtime: &DesktopRuntime, recovery: DesktopRuntimeRecovery) {
    *runtime
        .recovery
        .lock()
        .expect("desktop recovery lock poisoned") = recovery;
}

fn recovery_from_port_diagnosis(
    port: u16,
    state_root: &Path,
    fallback_detail: Option<String>,
) -> DesktopRuntimeRecovery {
    if magi_health_available(port) {
        let mut recovery =
            DesktopRuntimeRecovery::ready(port, format!("http://127.0.0.1:{port}/web.html"));
        if let Ok(diagnosis) = diagnose_port(port, state_root) {
            recovery.can_restart = !diagnosis.occupants.is_empty();
            recovery.requires_confirmation =
                diagnosis.occupants.iter().any(|occupant| !occupant.is_magi);
            recovery.occupants = diagnosis.occupants;
        }
        return recovery;
    }
    match diagnose_port(port, state_root) {
        Ok(diagnosis) if diagnosis.listener_detected => {
            let requires_confirmation =
                diagnosis.occupants.iter().any(|occupant| !occupant.is_magi);
            DesktopRuntimeRecovery {
                status: DesktopRecoveryStatus::PortOccupied,
                port,
                technical_detail: fallback_detail,
                can_restart: !diagnosis.occupants.is_empty(),
                requires_confirmation,
                occupants: diagnosis.occupants,
                web_url: None,
            }
        }
        Ok(_) => DesktopRuntimeRecovery {
            status: DesktopRecoveryStatus::Unavailable,
            port,
            technical_detail: fallback_detail,
            occupants: Vec::new(),
            can_restart: true,
            requires_confirmation: false,
            web_url: None,
        },
        Err(error) => DesktopRuntimeRecovery::failed(
            port,
            fallback_detail
                .map(|detail| format!("{detail}\n{error}"))
                .unwrap_or(error),
        ),
    }
}

fn request_exit(app: AppHandle) {
    let runtime = app.state::<DesktopRuntime>();
    if runtime.lifecycle.request_exit() != DesktopAction::BeginExit {
        return;
    }

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.close();
    }

    let daemon = runtime.daemon.clone();
    let lifecycle = runtime.lifecycle.clone();
    let state_root = runtime.state_root.clone();
    tauri::async_runtime::spawn(async move {
        shutdown_desktop_runtime(daemon, lifecycle, state_root, "desktop exit").await;
        app.exit(0);
    });
}

async fn shutdown_desktop_runtime(
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    lifecycle: Arc<DesktopLifecycle>,
    state_root: PathBuf,
    reason: &'static str,
) {
    let handle = daemon.lock().expect("desktop daemon lock poisoned").take();
    if let Some(handle) = handle {
        if let Err(error) = handle.shutdown(reason) {
            eprintln!("请求 Magi daemon 优雅关闭失败: {error}");
        }
        match tokio::time::timeout(DESKTOP_SHUTDOWN_TIMEOUT, handle.wait()).await {
            Ok(Err(error)) => eprintln!("等待 Magi daemon 关闭失败: {error}"),
            Err(_) => eprintln!(
                "等待 Magi daemon 关闭超过 {} 秒，继续退出桌面进程",
                DESKTOP_SHUTDOWN_TIMEOUT.as_secs()
            ),
            Ok(Ok(())) => {}
        }
    }

    let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
    runtime_state.remove_runtime_state();
    runtime_state.remove_pid();
    lifecycle.mark_stopped();
}

fn force_shutdown_desktop_runtime(
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    lifecycle: Arc<DesktopLifecycle>,
    state_root: PathBuf,
) {
    // 更新安装前不能等待活动请求结束；先记录中断状态，再立即中止 daemon 服务任务。
    if let Some(mut handle) = daemon.lock().expect("desktop daemon lock poisoned").take()
        && let Err(error) = handle.force_shutdown("desktop update restart")
    {
        eprintln!("更新重启前记录 daemon 中断状态失败: {error}");
    }

    let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
    runtime_state.remove_runtime_state();
    runtime_state.remove_pid();
    lifecycle.mark_stopped();
}

#[tauri::command]
fn prepare_update_restart(app: AppHandle) -> Result<(), String> {
    require_desktop_update_installability()?;
    let (daemon, lifecycle, state_root) = {
        let runtime = app.state::<DesktopRuntime>();
        (
            runtime.daemon.clone(),
            runtime.lifecycle.clone(),
            runtime.state_root.clone(),
        )
    };
    match lifecycle.request_update_restart() {
        DesktopAction::BeginExit => {
            force_shutdown_desktop_runtime(daemon, lifecycle, state_root);
            Ok(())
        }
        DesktopAction::Ignore if lifecycle.state() == DesktopState::Stopped => Ok(()),
        _ => Err("Magi 当前无法进入更新重启状态，请稍后重试".to_string()),
    }
}

#[tauri::command]
async fn get_staged_desktop_update(
    app: AppHandle,
    expected_version: String,
) -> Result<Option<StagedDesktopUpdate>, String> {
    let state_root = app.state::<DesktopRuntime>().state_root.clone();
    read_staged_update_for_version(&state_root, &expected_version)
}

#[tauri::command]
fn get_desktop_update_installability() -> Result<DesktopUpdateInstallability, String> {
    desktop_update_installability_for_current_process()
}

#[tauri::command]
async fn stage_desktop_update(
    app: AppHandle,
    version: String,
    on_event: Channel<DesktopUpdateDownloadEvent>,
) -> Result<StagedDesktopUpdate, String> {
    require_desktop_update_installability()?;
    let state_root = app.state::<DesktopRuntime>().state_root.clone();
    let updater = app
        .updater()
        .map_err(|error| format!("创建更新器失败: {error}"))?;
    let update = updater
        .check()
        .await
        .map_err(|error| format!("检查更新失败: {error}"))?
        .ok_or_else(|| "远端更新已不可用，请重新检查更新".to_string())?;

    if update.version != version {
        return Err(format!(
            "更新版本已变化：请求 v{version}，当前可用版本为 v{}",
            update.version
        ));
    }

    let staged = StagedDesktopUpdate {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date: update.date.map(|date| date.to_string()),
        body: update.body.clone(),
    };
    let mut first_chunk = true;
    let bytes = update
        .download(
            |chunk_length, content_length| {
                if first_chunk {
                    first_chunk = false;
                    let _ = on_event.send(DesktopUpdateDownloadEvent::Started { content_length });
                }
                let _ = on_event.send(DesktopUpdateDownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DesktopUpdateDownloadEvent::Finished);
            },
        )
        .await
        .map_err(|error| format!("下载更新失败: {error}"))?;

    write_staged_update(&state_root, &staged, &bytes)?;
    Ok(staged)
}

#[tauri::command]
async fn install_staged_desktop_update(app: AppHandle) -> Result<(), String> {
    require_desktop_update_installability()?;
    let state_root = app.state::<DesktopRuntime>().state_root.clone();
    let staged = read_staged_update(&state_root)?
        .ok_or_else(|| "没有找到已下载的更新包，请重新下载".to_string())?;
    let (bytes_path, _) = staged_update_paths(&state_root);
    let bytes = fs::read(&bytes_path).map_err(|error| format!("读取已下载更新包失败: {error}"))?;

    let update = app
        .updater()
        .map_err(|error| format!("创建更新器失败: {error}"))?
        .check()
        .await
        .map_err(|error| format!("校验更新状态失败: {error}"))?
        .ok_or_else(|| "远端更新已不可用，无法安装已下载的更新".to_string())?;
    if update.version != staged.version {
        remove_staged_update(&state_root);
        return Err(format!(
            "已下载版本 v{} 已过期，当前可用版本为 v{}，已清理旧更新包，请重新下载",
            staged.version, update.version,
        ));
    }

    update
        .install(bytes)
        .map_err(|error| format!("安装更新失败: {error}"))?;
    remove_staged_update(&state_root);
    Ok(())
}

#[tauri::command]
async fn get_desktop_runtime_recovery(app: AppHandle) -> Result<DesktopRuntimeRecovery, String> {
    let runtime = app.state::<DesktopRuntime>();
    let current = runtime
        .recovery
        .lock()
        .expect("desktop recovery lock poisoned")
        .clone();
    if matches!(
        current.status,
        DesktopRecoveryStatus::Starting | DesktopRecoveryStatus::Restarting
    ) || (current.status == DesktopRecoveryStatus::Failed && !current.can_restart)
    {
        return Ok(current);
    }
    let port = read_port()?;
    let state_root = runtime.state_root.clone();
    let fallback_detail = current.technical_detail;
    let recovery = tauri::async_runtime::spawn_blocking(move || {
        recovery_from_port_diagnosis(port, &state_root, fallback_detail)
    })
    .await
    .map_err(|error| format!("等待桌面运行时诊断失败: {error}"))?;
    set_desktop_recovery(&runtime, recovery.clone());
    Ok(recovery)
}

async fn stop_current_daemon_for_recovery(runtime: &DesktopRuntime) -> Result<(), String> {
    let handle = runtime
        .daemon
        .lock()
        .expect("desktop daemon lock poisoned")
        .take();
    if let Some(mut handle) = handle {
        let shutdown_error = handle
            .shutdown("desktop runtime recovery")
            .err()
            .map(|error| error.to_string());
        match tokio::time::timeout(DESKTOP_SHUTDOWN_TIMEOUT, handle.wait_until_stopped()).await {
            Ok(Ok(())) => {
                if let Some(error) = shutdown_error {
                    eprintln!("daemon 停止请求曾返回错误，但服务已经退出: {error}");
                }
            }
            Ok(Err(error)) => {
                let detail = shutdown_error
                    .map(|shutdown| format!("{error}; 停止请求错误: {shutdown}"))
                    .unwrap_or_else(|| error.to_string());
                eprintln!("当前 Magi daemon 已异常退出，继续恢复运行环境: {detail}");
            }
            Err(_) => {
                let shutdown_detail = shutdown_error
                    .map(|error| format!("，停止请求错误: {error}"))
                    .unwrap_or_default();
                if let Err(error) =
                    handle.force_shutdown("desktop runtime recovery graceful shutdown timeout")
                {
                    eprintln!("强制中止 daemon 前记录运行中断失败: {error}");
                }
                eprintln!(
                    "当前 Magi daemon 在 {} 秒内未停止，已强制中止并继续恢复运行环境{shutdown_detail}",
                    DESKTOP_SHUTDOWN_TIMEOUT.as_secs()
                );
            }
        }
        let runtime_state = RuntimeStateManager::new(runtime.state_root.join("runtime"));
        runtime_state.remove_runtime_state();
        runtime_state.remove_pid();
    }
    Ok(())
}

async fn launch_desktop_daemon(
    app: &AppHandle,
    port: u16,
) -> Result<String, magi_daemon::DaemonError> {
    let runtime = app.state::<DesktopRuntime>();
    let host = read_env("MAGI_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let service_name =
        read_env("MAGI_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
    let config = DaemonConfig::new(host.clone(), port, service_name, &runtime.state_root)
        .with_web_dist_root(runtime.web_dist_root.clone())
        .with_open_browser(false);
    let handle = Daemon::new(config).start().await?;
    let web_url = handle.web_url().to_string();
    let bound_addr = handle.bound_addr();
    let runtime_state = RuntimeStateManager::new(runtime.state_root.join("runtime"));
    runtime_state.write_runtime_state(process::id(), Some(&host), bound_addr.port());
    runtime_state.write_pid(process::id());
    *runtime.daemon.lock().expect("desktop daemon lock poisoned") = Some(handle);
    set_desktop_recovery(
        &runtime,
        DesktopRuntimeRecovery::ready(bound_addr.port(), web_url.clone()),
    );
    Ok(web_url)
}

#[tauri::command]
async fn restart_desktop_runtime(
    app: AppHandle,
    request: RestartDesktopRuntimeRequest,
) -> Result<DesktopRuntimeRecovery, String> {
    let runtime = app.state::<DesktopRuntime>();
    let _restart_guard = runtime.restart_lock.lock().await;
    let port = read_port()?;
    set_desktop_recovery(&runtime, DesktopRuntimeRecovery::restarting(port));
    if let Err(error) = stop_current_daemon_for_recovery(&runtime).await {
        let recovery = recovery_from_port_diagnosis(port, &runtime.state_root, Some(error.clone()));
        set_desktop_recovery(&runtime, recovery);
        return Err(error);
    }

    let state_root = runtime.state_root.clone();
    let expected_occupants = request.expected_occupants;
    let confirm_external_processes = request.confirm_external_processes;
    let cleanup_result = tauri::async_runtime::spawn_blocking(move || {
        let diagnosis = diagnose_port(port, &state_root)?;
        if diagnosis.listener_detected {
            terminate_port_occupants(
                port,
                &state_root,
                &expected_occupants,
                confirm_external_processes,
            )?;
        }
        wait_for_port_release(port, &state_root, DESKTOP_PORT_RELEASE_TIMEOUT)
    })
    .await
    .map_err(|error| format!("等待端口清理任务失败: {error}"))?;

    if let Err(error) = cleanup_result {
        let recovery = recovery_from_port_diagnosis(port, &runtime.state_root, Some(error.clone()));
        set_desktop_recovery(&runtime, recovery);
        return Err(error);
    }

    let web_url = match launch_desktop_daemon(&app, port).await {
        Ok(web_url) => web_url,
        Err(error) => {
            let detail = format!("Magi daemon 重启失败: {error}");
            let recovery = match &error {
                magi_daemon::DaemonError::Io(source)
                    if source.kind() == io::ErrorKind::AddrInUse =>
                {
                    recovery_from_port_diagnosis(port, &runtime.state_root, Some(detail.clone()))
                }
                _ => DesktopRuntimeRecovery::failed(port, detail.clone()),
            };
            set_desktop_recovery(&runtime, recovery);
            return Err(detail);
        }
    };
    navigate_main_window(&app, &web_url)?;
    Ok(runtime
        .recovery
        .lock()
        .expect("desktop recovery lock poisoned")
        .clone())
}

fn install_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let open_item = MenuItemBuilder::with_id(OPEN_MENU_ID, "打开 Magi").build(app)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItemBuilder::with_id(QUIT_MENU_ID, "退出 Magi").build(app)?;
    let menu = Menu::with_items(app, &[&open_item, &separator, &quit_item])?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("Magi")
        .icon_as_template(true)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN_MENU_ID => {
                let runtime = app.state::<DesktopRuntime>();
                if runtime.lifecycle.request_show() == DesktopAction::ShowWindow {
                    show_main_window(app);
                }
            }
            QUIT_MENU_ID => request_exit(app.clone()),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

fn start_daemon(app: AppHandle, state_root: PathBuf, web_dist_root: PathBuf) {
    tauri::async_runtime::spawn(async move {
        if !web_dist_root.join("web.html").is_file() {
            let detail = format!("Magi 桌面包缺少内置 Web 入口: {}", web_dist_root.display());
            eprintln!("{detail}");
            let runtime = app.state::<DesktopRuntime>();
            set_desktop_recovery(
                &runtime,
                DesktopRuntimeRecovery::failed(DEFAULT_PORT, detail),
            );
            runtime.lifecycle.mark_ready();
            create_startup_error_window(&app);
            return;
        }

        let port = match read_port() {
            Ok(port) => port,
            Err(error) => {
                eprintln!("{error}");
                let runtime = app.state::<DesktopRuntime>();
                set_desktop_recovery(
                    &runtime,
                    DesktopRuntimeRecovery::failed(DEFAULT_PORT, error),
                );
                runtime.lifecycle.mark_ready();
                create_startup_error_window(&app);
                return;
            }
        };
        let web_url = match launch_desktop_daemon(&app, port).await {
            Ok(web_url) => web_url,
            Err(error) => {
                let detail = format!("Magi daemon 启动失败: {error}");
                eprintln!("{detail}");
                let runtime = app.state::<DesktopRuntime>();
                let recovery = match &error {
                    magi_daemon::DaemonError::Io(source)
                        if source.kind() == io::ErrorKind::AddrInUse =>
                    {
                        recovery_from_port_diagnosis(port, &state_root, Some(detail.clone()))
                    }
                    _ => DesktopRuntimeRecovery::failed(port, detail),
                };
                set_desktop_recovery(&runtime, recovery);
                runtime.lifecycle.mark_ready();
                create_startup_error_window(&app);
                return;
            }
        };

        let runtime = app.state::<DesktopRuntime>();
        if matches!(
            runtime.lifecycle.state(),
            DesktopState::ShuttingDown | DesktopState::Restarting | DesktopState::Stopped
        ) {
            drop(
                runtime
                    .daemon
                    .lock()
                    .expect("desktop daemon lock poisoned")
                    .take(),
            );
            let runtime_state = RuntimeStateManager::new(state_root.join("runtime"));
            runtime_state.remove_runtime_state();
            runtime_state.remove_pid();
            runtime.lifecycle.mark_stopped();
            app.exit(0);
            return;
        }
        runtime.lifecycle.mark_ready();
        if let Err(error) = navigate_main_window(&app, &web_url) {
            eprintln!("{error}");
            request_exit(app);
        }
    });
}

fn main() {
    magi_process::initialize_user_process_environment();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            prepare_update_restart,
            get_staged_desktop_update,
            get_desktop_update_installability,
            stage_desktop_update,
            install_staged_desktop_update,
            get_desktop_runtime_recovery,
            restart_desktop_runtime,
        ])
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let runtime = app.state::<DesktopRuntime>();
            if runtime.lifecycle.request_show() == DesktopAction::ShowWindow {
                show_main_window(app);
            }
        }))
        .setup(|app| {
            let state_root = read_env("MAGI_STATE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(default_state_root);
            let web_dist_root = resolve_web_dist_root(app)?;
            app.manage(DesktopRuntime::new(
                state_root.clone(),
                web_dist_root.clone(),
            ));
            install_tray(app)?;
            start_daemon(app.handle().clone(), state_root, web_dist_root);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != MAIN_WINDOW_LABEL {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let runtime = window.state::<DesktopRuntime>();
                if runtime.lifecycle.request_window_close() == DesktopAction::HideWindow {
                    api.prevent_close();
                    let _ = window.hide();
                    #[cfg(target_os = "macos")]
                    let _ = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("构建 Magi 桌面应用失败");

    app.run(|app, event| match event {
        RunEvent::ExitRequested { api, .. } => {
            let runtime = app.state::<DesktopRuntime>();
            if !matches!(
                runtime.lifecycle.state(),
                DesktopState::Restarting | DesktopState::Stopped
            ) {
                api.prevent_exit();
                request_exit(app.clone());
            }
        }
        RunEvent::Exit => {
            let runtime = app.state::<DesktopRuntime>();
            if matches!(
                runtime.lifecycle.state(),
                DesktopState::Restarting | DesktopState::Stopped
            ) {
                return;
            }

            runtime.lifecycle.request_exit();
            tauri::async_runtime::block_on(shutdown_desktop_runtime(
                runtime.daemon.clone(),
                runtime.lifecycle.clone(),
                runtime.state_root.clone(),
                "desktop process exit",
            ));
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_web_url_is_versioned_to_avoid_stale_entry_html() {
        let url = parse_desktop_web_url("http://127.0.0.1:38123/web.html", None)
            .expect("desktop web URL should parse");

        assert_eq!(
            url.as_str(),
            concat!(
                "http://127.0.0.1:38123/web.html?desktopVersion=",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn desktop_runtime_recovery_preserves_the_active_workspace_and_session() {
        let current_url = "http://127.0.0.1:38123/web.html?workspaceId=workspace-c&workspacePath=encoded-path&sessionId=session-n&agentBaseUrl=http%3A%2F%2Fexample.com"
            .parse::<tauri::Url>()
            .expect("current desktop URL should parse");
        let recovered =
            parse_desktop_web_url("http://127.0.0.1:38123/web.html", Some(&current_url))
                .expect("recovered desktop URL should parse");

        let query = recovered
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            query.get("workspaceId").map(|value| value.as_ref()),
            Some("workspace-c")
        );
        assert_eq!(
            query.get("workspacePath").map(|value| value.as_ref()),
            Some("encoded-path")
        );
        assert_eq!(
            query.get("sessionId").map(|value| value.as_ref()),
            Some("session-n")
        );
        assert!(!query.contains_key("agentBaseUrl"));
    }

    #[test]
    fn desktop_update_rejects_macos_disk_image_execution() {
        let installability =
            desktop_update_installability(Path::new("/Volumes/Magi/Magi.app/Contents/MacOS/Magi"));

        #[cfg(target_os = "macos")]
        {
            assert!(!installability.installable);
            assert!(matches!(
                installability.reason,
                Some(DesktopUpdateInstallabilityReason::DiskImage)
            ));
        }

        #[cfg(not(target_os = "macos"))]
        {
            assert!(installability.installable);
            assert!(installability.reason.is_none());
        }
    }

    #[test]
    fn desktop_update_accepts_application_execution() {
        let installability =
            desktop_update_installability(Path::new("/Applications/Magi.app/Contents/MacOS/Magi"));

        assert!(installability.installable);
        assert!(installability.reason.is_none());
    }

    #[test]
    fn stale_staged_update_is_removed_before_it_reaches_the_update_ui() {
        let state_root = env::temp_dir().join(format!(
            "magi-desktop-updater-{}-{}",
            process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        ));
        let staged = StagedDesktopUpdate {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            version: "3.0.25".to_string(),
            date: None,
            body: None,
        };
        write_staged_update(&state_root, &staged, b"stale update")
            .expect("staged update fixture should be written");

        let result = read_staged_update_for_version(&state_root, "3.0.26")
            .expect("staged update reconciliation should succeed");
        assert!(result.is_none());
        let (bytes_path, metadata_path) = staged_update_paths(&state_root);
        assert!(!bytes_path.exists());
        assert!(!metadata_path.exists());
        let _ = fs::remove_dir_all(state_root);
    }
}
